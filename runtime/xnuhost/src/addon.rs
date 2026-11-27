// SPDX-License-Identifier: MIT
//! The boot addon a runtime package carries: `kernel/gpuaddon` (the default iOS build's, 24A446) and
//! `kernel/<build>/gpuaddon` (another build's, 24B156), the `NGPUADD2` sidecar `oneios-tool ios addon` writes (the Metal
//! plug-in under `addons/`, the capture plug-in under `camera/`), each with the CDHashes its `.cdhashes` list names. The
//! kernel writes its files into the Data store and mounts them over the sealed system's folders before launchd starts
//! (hosted 0031, 0056, 0090).
//!
//! The image set's own sidecar (`<kernel>.gpuaddon`, staged by `oneios-tool phone addon`) is the one a phone chose, and
//! it wins (session/board.rs). The runtime's is the fallback, so a plug-in reaches the guest with the runtime alone — no
//! adb, no image rebuild — and its CDHashes join the trust cache this boot gives the kernel: an image built without the
//! plug-in has no owner module that names it (`overlay/userland/owner-trust.json`). Without it an image keeps the
//! plug-in its Data seed was made with, whatever runtime boots it: issue #86 (2026-11-27), an image made on v0.1.36 and
//! booted by v0.1.38 kept v0.1.36's metal2vulkan-rt plug-in and its refusal of the blur over `BGR10_XR` (issue #81,
//! fixed in v0.1.38's plug-in), so backboardd aborted at every boot.
//!
//! A plug-in is linked against one iOS build's frameworks (its bundle's `oneios-target`, "MODEL BUILD"): a sidecar
//! whose plug-ins name another build than the image set's is left out, and the image's own plug-in stays.

use xh_core::bytes::le;
use xh_core::{loge, logi};

/// A boot addon and the CDHashes its code needs in this boot's trust cache.
pub struct Addon {
    pub blob: Vec<u8>,
    /// The 20-byte CDHashes of the files the addon carries (the first word of each line of the list `ios addon` writes).
    pub cdhashes: Vec<[u8; 20]>,
}

/// The runtime's boot addon for an image set of iOS build `build` (None: no image set, e.g. a bench folder):
/// `kernel/<build>/gpuaddon` when the runtime has one for that build, else `kernel/gpuaddon`, each with its
/// `.cdhashes`. None when the runtime has none, when its plug-ins are for another build, or when the file there is not
/// a boot addon (left alone, said in the log).
pub fn runtime_addon(runtime: &str, build: Option<&str>) -> Option<Addon> {
    let root = runtime.trim_end_matches('/');
    let build = build.filter(|b| !b.is_empty() && b.bytes().all(|c| c.is_ascii_alphanumeric()));
    if let Some(build) = build {
        let path = format!("{root}/kernel/{build}/gpuaddon");
        if std::path::Path::new(&path).exists() {
            return addon_at(&path, Some(build));
        }
    }
    addon_at(&format!("{root}/kernel/gpuaddon"), build)
}

fn addon_at(path: &str, build: Option<&str>) -> Option<Addon> {
    let blob = crate::session::read_file(path, 64 << 20)?;
    // The loader checks the same magic and total: "NGPUADD2" and a total that is the file's length (ios addon writes
    // 24 + the body). A cut or padded file is refused here, where the log can name the runtime.
    if blob.len() < 24 || blob.get(..8) != Some(b"NGPUADD2".as_slice()) || le::u64(&blob, 16) != blob.len() as u64 {
        loge!("gpuaddon: {path}: not a boot addon (NGPUADD2, its total is not the file's length): left alone");
        return None;
    }
    // Without its list the plug-in would land in the Data store without being trusted. Refuse the runtime fallback
    // instead; a staged image sidecar still has its own image-set trust path.
    let cdhashes = match crate::session::read_file(&format!("{path}.cdhashes"), 1 << 16) {
        Some(list) => match parse_cdhashes(&list) {
            Ok(cdhashes) if !cdhashes.is_empty() => cdhashes,
            Ok(_) => {
                loge!("gpuaddon: {path}.cdhashes lists no CDHashes: runtime add-on refused");
                return None;
            }
            Err(why) => {
                loge!("gpuaddon: {path}.cdhashes: {why}: runtime add-on refused");
                return None;
            }
        },
        None => {
            loge!("gpuaddon: {path}.cdhashes is missing: runtime add-on refused");
            return None;
        }
    };
    // The plug-ins it carries are linked against one iOS build: on an image of another build they would load against
    // the wrong frameworks. Every bundle's oneios-target must name the image set's build.
    let targets = match crate::gpu3d_amdgpu::bundle_targets(&blob) {
        Ok(targets) => targets,
        Err(why) => {
            loge!("gpuaddon: {path}: {why}: runtime add-on refused");
            return None;
        }
    };
    if let Some(build) = build {
        if let Some((bundle, line)) = targets.iter().find(|(_, line)| line.split_whitespace().nth(1) != Some(build)) {
            logi!(
                "gpuaddon: {path}: its {bundle} plug-in is for \"{line}\", the image set is build {build}: the runtime's \
                 add-on left out (the image's own plug-in stays)"
            );
            return None;
        }
    }
    let named: Vec<&str> = targets.iter().map(|(_, line)| line.as_str()).collect();
    logi!(
        "gpuaddon: {path}: {} bytes, {} CDHashes trusted, plug-ins for {} (the runtime's; the image set has no \
         kernel.kc.gpuaddon): they replace the Data store's",
        blob.len(),
        cdhashes.len(),
        if named.is_empty() { "no build named".to_string() } else { named.join(", ") }
    );
    Some(Addon { blob, cdhashes })
}

/// `ios addon`'s CDHash list (and `phone addon`'s reader of it): one 40-hex-digit CDHash per line, then the file's
/// name; blank lines and `#` comments are skipped.
fn parse_cdhashes(text: &[u8]) -> Result<Vec<[u8; 20]>, String> {
    let text = std::str::from_utf8(text).map_err(|_| "not a text list".to_string())?;
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let word = line.split_whitespace().next().unwrap_or("");
        if word.len() != 40 || !word.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{line:?} is not a CDHash line (40 hex digits, then the file's name)"));
        }
        let mut hash = [0u8; 20];
        for (i, byte) in hash.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&word[2 * i..2 * i + 2], 16)
                .map_err(|_| format!("{word:?} is not a CDHash (hex digits only)"))?;
        }
        out.push(hash);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("test-addon-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("kernel")).unwrap();
        dir
    }

    /// A stand-in for `ios addon`'s sidecar: the magic, an entry count and the total, no files (this reader checks the
    /// magic and the total; the kernel checks the entries).
    fn addon() -> Vec<u8> {
        let mut blob = Vec::new();
        blob.extend_from_slice(b"NGPUADD2");
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&24u64.to_le_bytes());
        blob
    }

    #[test]
    fn cdhash_list() {
        let list = b"# ios addon, 2026-11-23\n\
4baf7dbb8b7faf1312e87d5b81ea830b5578b3fd addons/NativeAdrenoMetal.bundle/NativeAdrenoMetal\r\n\
\n\
1936cc1bb5954173c0b267f6c1d25d2684dee656 addons/NativeAdrenoMetal.bundle/libvulkan_freedreno.dylib\n";
        let hashes = parse_cdhashes(list).unwrap();
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes[0][..4], [0x4b, 0xaf, 0x7d, 0xbb]);
        assert_eq!(hashes[1][19], 0x56);
        assert_eq!(parse_cdhashes(b"").unwrap().len(), 0);
        for bad in [
            &b"4baf7dbb8b7faf1312e87d5b81ea830b5578b3 addons/x\n"[..],
            b"zzaf7dbb8b7faf1312e87d5b81ea830b5578b3fd addons/x\n",
            b"\xff\xfe\n",
        ] {
            assert!(parse_cdhashes(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn the_runtime_addon() {
        // A runtime without one: nothing, quietly.
        let none = dir("none");
        assert!(runtime_addon(none.to_str().unwrap(), None).is_none());

        // The pair the packager places: the blob and its list.
        let rt = dir("pair");
        fs::write(rt.join("kernel/gpuaddon"), addon()).unwrap();
        fs::write(
            rt.join("kernel/gpuaddon.cdhashes"),
            "4baf7dbb8b7faf1312e87d5b81ea830b5578b3fd addons/NativeAdrenoMetal.bundle/NativeAdrenoMetal\n\
33247c78e0afb60610a6239991a859e342ac29f1 camera/MediaCapture/H9ISP.mediacapture\n",
        )
        .unwrap();
        let found = runtime_addon(rt.to_str().unwrap(), None).expect("the runtime's addon");
        assert_eq!(found.blob, addon());
        assert_eq!(found.cdhashes.len(), 2);
        // A trailing slash names the same folder.
        assert!(runtime_addon(&format!("{}/", rt.display()), None).is_some());

        // A blob without its trust list is not injected into the guest.
        fs::remove_file(rt.join("kernel/gpuaddon.cdhashes")).unwrap();
        assert!(runtime_addon(rt.to_str().unwrap(), None).is_none());
        // A list with a line that is not a CDHash: nothing is taken from it.
        fs::write(rt.join("kernel/gpuaddon.cdhashes"), "4baf addons/x\n").unwrap();
        assert!(runtime_addon(rt.to_str().unwrap(), None).is_none());

        // Not a boot addon at all: left alone.
        let bad = dir("bad");
        fs::write(bad.join("kernel/gpuaddon"), b"not an addon").unwrap();
        fs::write(bad.join("kernel/gpuaddon.cdhashes"), "").unwrap();
        assert!(runtime_addon(bad.to_str().unwrap(), None).is_none());
        // The magic, but a total that is not the file's length (cut short, or bytes after it): refused, as the loader
        // would refuse it.
        let cut = dir("cut");
        let mut blob = addon();
        blob.push(0);
        fs::write(cut.join("kernel/gpuaddon"), &blob).unwrap();
        fs::write(cut.join("kernel/gpuaddon.cdhashes"), "").unwrap();
        assert!(runtime_addon(cut.to_str().unwrap(), None).is_none());
        fs::write(cut.join("kernel/gpuaddon"), &addon()[..20]).unwrap();
        assert!(runtime_addon(cut.to_str().unwrap(), None).is_none());
        for d in [none, rt, bad, cut] {
            let _ = fs::remove_dir_all(d);
        }
    }

    /// An NGPUADD2 sidecar of these files, each a zlib stream of one stored block (oneios-tool's `pack` layout).
    fn sidecar(files: &[(&str, &[u8])]) -> Vec<u8> {
        let pad = |v: &mut Vec<u8>| v.resize(v.len().div_ceil(8) * 8, 0);
        let mut body = Vec::new();
        for (name, data) in files {
            let mut n = name.as_bytes().to_vec();
            n.push(0);
            let mut z = vec![0x78, 0x01, 0x01];
            z.extend((data.len() as u16).to_le_bytes());
            z.extend((!(data.len() as u16)).to_le_bytes());
            z.extend_from_slice(data);
            z.extend(xh_core::inflate::adler32(data).to_be_bytes());
            body.extend((n.len() as u32).to_le_bytes());
            body.extend(0o644u32.to_le_bytes());
            body.extend((data.len() as u64).to_le_bytes());
            body.extend((z.len() as u64).to_le_bytes());
            pad(&mut n);
            pad(&mut z);
            body.extend(n);
            body.extend(z);
        }
        let mut b = b"NGPUADD2".to_vec();
        b.extend((files.len() as u32).to_le_bytes());
        b.extend(0u32.to_le_bytes());
        b.extend(((24 + body.len()) as u64).to_le_bytes());
        b.extend(body);
        b
    }

    const LIST: &str = "4baf7dbb8b7faf1312e87d5b81ea830b5578b3fd addons/NativeAdrenoMetal.bundle/NativeAdrenoMetal\n";

    fn plugin(target: &[u8]) -> Vec<u8> {
        sidecar(&[
            ("addons/NativeAdrenoMetal.bundle/NativeAdrenoMetal", b"\xcf\xfa\xed\xfe"),
            ("addons/NativeAdrenoMetal.bundle/oneios-target", target),
            ("camera/MediaCapture/H9ISP.mediacapture", b"x"),
        ])
    }

    /// Issue #86: the runtime's plug-in reaches an image made by an older runtime, but only an image of the build the
    /// plug-in is linked against; another build's folder (kernel/<build>/) is taken first for that build.
    #[test]
    fn the_runtime_addon_is_for_the_image_sets_build() {
        let rt = dir("build");
        let default = plugin(b"iPhone18,2 24A446\n");
        fs::write(rt.join("kernel/gpuaddon"), &default).unwrap();
        fs::write(rt.join("kernel/gpuaddon.cdhashes"), LIST).unwrap();
        let root = rt.to_str().unwrap();
        // The default build's image, and no image set at all: the default's addon.
        assert_eq!(runtime_addon(root, Some("24A446")).expect("24A446").blob, default);
        assert_eq!(runtime_addon(root, None).expect("no image set").blob, default);
        // Another build's image (iOS 27.2.1, the A56's iOS 16.5) and no addon of its own: none, the image's plug-in stays.
        assert!(runtime_addon(root, Some("24B156")).is_none());
        assert!(runtime_addon(root, Some("20F66")).is_none());
        // With kernel/24B156/gpuaddon: that one, for that build only.
        let other = plugin(b"iPhone18,2 24B156\n");
        fs::create_dir_all(rt.join("kernel/24B156")).unwrap();
        fs::write(rt.join("kernel/24B156/gpuaddon"), &other).unwrap();
        fs::write(rt.join("kernel/24B156/gpuaddon.cdhashes"), LIST).unwrap();
        assert_eq!(runtime_addon(root, Some("24B156")).expect("24B156").blob, other);
        assert_eq!(runtime_addon(root, Some("24A446")).expect("24A446").blob, default);
        // A build's folder holding another build's plug-in (a packaging mistake): refused, not the default's either.
        fs::write(rt.join("kernel/24B156/gpuaddon"), &default).unwrap();
        assert!(runtime_addon(root, Some("24B156")).is_none());
        // A build name that is not one (a path) is not looked up as a folder.
        assert_eq!(runtime_addon(root, Some("../24B156")).map(|a| a.blob), Some(default.clone()));
        // A sidecar whose files are cut: refused.
        let mut cut = default.clone();
        cut.truncate(60);
        let total = cut.len() as u64;
        cut[16..24].copy_from_slice(&total.to_le_bytes());
        fs::write(rt.join("kernel/gpuaddon"), &cut).unwrap();
        assert!(runtime_addon(root, Some("24A446")).is_none());
        let _ = fs::remove_dir_all(rt);
    }
}
