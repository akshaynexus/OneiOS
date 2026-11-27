// SPDX-License-Identifier: MIT
//! Whether the kernel's gpuaddon sidecar (<kernel>.gpuaddon: oneios-tool's NGPUADD2, tools/oneios-tool/crates/
//! oneios-ios/src/metal.rs `pack`) carries the A56's Metal plug-in (K3): `addons/OneiOSRadeonMetal.bundle/` with its
//! executable `OneiOSRadeonMetal` and its `oneios-target`, the line userland/metal/build.sh's `finish` writes
//! ("MODEL BUILD", here `iPhone10,3 20F66`). Without the plug-in the guest has no Metal on this GPU, so xnuhost keeps
//! the 2D path and says why.
use xh_core::bytes::le;

/// The plug-in's name (its bundle, its executable).
pub const PLUGIN: &str = "OneiOSRadeonMetal";
/// The A56's OpenGL ES (docs/a56-gpu.md 4a): `addons/OneiOSRadeonGL.bundle/` with its executable and its oneios-target, as
/// userland/metal-amd/gl/build.sh `plugin` makes it. With the Metal plug-in it is what CoreAnimation's render server
/// composites with (the kernel's IOGLESBundleName, oneios-gpu-amd-gl=1).
pub const GL_PLUGIN: &str = "OneiOSRadeonGL";

/// What the sidecar holds of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plugin {
    /// its oneios-target line
    pub target: String,
    /// files under its bundle
    pub files: usize,
}

/// A file of the sidecar: its name, its packed bytes, its unpacked size.
type File<'a> = (&'a [u8], &'a [u8], u64);

/// The sidecar's files; Err when it is not NGPUADD2 or is cut.
fn files(blob: &[u8]) -> Result<Vec<File<'_>>, String> {
    let cut = || "the gpuaddon is cut short".to_owned();
    if blob.get(..8) != Some(b"NGPUADD2".as_slice()) {
        return Err("the gpuaddon is not NGPUADD2".into());
    }
    let count = le::try_u32(blob, 8).ok_or_else(cut)? as usize;
    let mut at = 24;
    let mut out = Vec::new();
    for _ in 0..count {
        let nlen = le::try_u32(blob, at).ok_or_else(cut)? as usize;
        let size = le::try_u64(blob, at + 8).ok_or_else(cut)?;
        let plen = le::try_u64(blob, at + 16).ok_or_else(cut)? as usize;
        at += 24;
        if nlen == 0 {
            return Err(cut());
        }
        let name = blob.get(at..at + nlen - 1).ok_or_else(cut)?;
        at += nlen.div_ceil(8) * 8;
        let packed = blob.get(at..at.checked_add(plen).ok_or_else(cut)?).ok_or_else(cut)?;
        at += plen.div_ceil(8) * 8;
        out.push((name, packed, size));
    }
    Ok(out)
}

/// Every plug-in bundle's `oneios-target` line in `addon` (`addons/<name>.bundle/oneios-target`, "MODEL BUILD"), as
/// (name, line): which iOS build each plug-in the sidecar carries was linked against (crate::addon::runtime_addon
/// refuses a runtime's sidecar whose plug-ins are for another build than the image set's).
pub fn bundle_targets(addon: &[u8]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for (name, packed, size) in files(addon)? {
        let Some(bundle) = name.strip_prefix(b"addons/").and_then(|r| r.strip_suffix(b".bundle/oneios-target")) else {
            continue;
        };
        if bundle.contains(&b'/') {
            continue;
        }
        let bundle = String::from_utf8_lossy(bundle).into_owned();
        let text = xh_core::inflate::zlib(packed, 256)
            .ok()
            .filter(|t| t.len() as u64 == size)
            .ok_or_else(|| format!("the {bundle} plug-in's oneios-target does not unpack"))?;
        out.push((bundle, String::from_utf8_lossy(&text).trim().to_owned()));
    }
    Ok(out)
}

/// The plug-in in `addon` (None: there is no sidecar), for an image set of build `build` (None: no image set):
/// Ok(what it holds), or Err(why not) in the words of the session's "gpu: 2D only" line.
pub fn plugin_in_addon(addon: Option<&[u8]>, build: Option<&str>) -> Result<Plugin, String> {
    bundle_in_addon(addon, build, PLUGIN, "the A56's Metal")
}

/// The A56's OpenGL ES bundle in `addon`, as [`plugin_in_addon`] says it of the Metal plug-in.
pub fn gl_in_addon(addon: Option<&[u8]>, build: Option<&str>) -> Result<Plugin, String> {
    bundle_in_addon(addon, build, GL_PLUGIN, "the A56's OpenGL ES")
}

fn bundle_in_addon(addon: Option<&[u8]>, build: Option<&str>, name: &str, what: &str) -> Result<Plugin, String> {
    let Some(blob) = addon else {
        return Err(format!("the kernel has no gpuaddon sidecar, so no {name} plug-in ({what})"));
    };
    let list = files(blob)?;
    let dir = format!("addons/{name}.bundle/");
    let exe = format!("{dir}{name}");
    let target = format!("{dir}oneios-target");
    let mine: Vec<_> = list.iter().filter(|(n, _, _)| n.starts_with(dir.as_bytes())).collect();
    if !mine.iter().any(|(n, _, _)| *n == exe.as_bytes()) {
        return Err(format!("the gpuaddon has no {name} plug-in ({what})"));
    }
    let Some((_, packed, size)) = mine.iter().find(|(n, _, _)| *n == target.as_bytes()) else {
        return Err(format!("the {name} plug-in in the gpuaddon has no oneios-target"));
    };
    let text = xh_core::inflate::zlib(packed, 256)
        .ok()
        .filter(|t| t.len() as u64 == *size)
        .ok_or_else(|| format!("the {name} plug-in's oneios-target does not unpack"))?;
    let line = String::from_utf8_lossy(&text).trim().to_owned();
    let named = line.split_whitespace().nth(1).unwrap_or("");
    if let Some(build) = build.filter(|b| *b != named) {
        return Err(format!("the {name} plug-in is for \"{line}\", the image set is build {build}"));
    }
    Ok(Plugin { target: line, files: mine.len() })
}

/// The boot argument the A56's OpenGL ES needs the kernel to read (docs/a56-gpu.md 4a): only a kernel that has it
/// names IOGLESBundleName OneiOSRadeonGL for the CPU-composition processes (SpringBoard among them). On an older A56
/// kernel (181d8bf2d60a and before) those processes would find no GL driver, and with the CoreImage fixup left out
/// SpringBoard's CoreImage would call glDisable through NULL (docs/gpu.md 5.2): so no GL, and the fixup kept.
pub const GL_BOOT_ARG: &[u8] = b"oneios-gpu-amd-gl";

/// Whether the kernel collection's bytes name [`GL_BOOT_ARG`] (its PE_parse_boot_argn string, in __cstring).
pub fn kernel_knows_gl(kc: &[u8]) -> bool {
    kc.windows(GL_BOOT_ARG.len()).any(|w| w == GL_BOOT_ARG)
}
