//! The Galaxy A56's GPU plug-ins into an iOS 16.5 image set (docs/a56-gpu.md 6): the staged runtime's
//! lib/guest/OneiOSRadeonMetal.bundle and OneiOSRadeonGL.bundle (runtime.json "metal_a56", runtime/package.sh 3c) as the
//! set's `kernel.kc.gpuaddon` (the boot addon xnuhost reads beside the kernel and hosted 0031 mounts), their CDHashes
//! in the set's `overlay/userland/owner-trust.json` (xnuhost's owner trust module), and that file's entry of
//! `manifest.json` with its new size and SHA-256.
//!
//! Two callers: `oneios-tool image a56-gpu-place --stage STAGE --out DIR` (the app's root script, at Update the
//! kernel's `finish kernel`: the three files in DIR, the script moves them in and rewrites image.sha256 and image.ids),
//! and Make iOS image (`build` here, after the manifest: owner-trust.json and manifest.json before the verify, the
//! sidecar after it, before its lists are made: [`place_listed`], [`write_sidecar`]). Only a set whose
//! manifest names the build the plug-ins are for (`iPhone10,3 20F66`); an iOS 27 set (24A446) never takes them, and
//! nothing is written for it.

use oneios_common::json::{self, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The sidecar's name in the set (xnuhost: `<kernel>.gpuaddon`, the kernel being the set's kernel.kc).
pub const ADDON: &str = "kernel.kc.gpuaddon";
pub const TRUST: &str = "overlay/userland/owner-trust.json";
const MANIFEST: &str = "manifest.json";

/// What the staged runtime carries for the A56: the Metal bundle, its OpenGL ES bundle (or none) and the image they
/// are for (`<device> <build>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugins {
    pub metal: PathBuf,
    pub gl: Option<PathBuf>,
    pub target: String,
}

/// The files of a placing: the addon, owner-trust.json and manifest.json as they become, and what they hold.
#[derive(Debug)]
pub struct Placed {
    pub addon: Vec<u8>,
    pub trust: String,
    pub manifest: String,
    /// the addon's CDHashes, and how many of them owner-trust.json did not list yet
    pub cdhashes: usize,
    pub new: usize,
}

/// The A56 plug-ins of a staged runtime (its folder: runtime.json beside lib/): Ok(None) when its runtime.json names
/// none (every runtime before 2026-11-25, or one packed without METAL_AMD_DIR); Err when it names them and they are
/// not there whole.
pub fn runtime_plugins(runtime: &Path) -> Result<Option<Plugins>, String> {
    let path = runtime.join("runtime.json");
    let doc = json::load(&path).map_err(|e| format!("{}: {e:#}", path.display()))?;
    let Some(entry) = doc.get("metal_a56").filter(|v| !v.is_null()) else { return Ok(None) };
    let below = |v: Option<&Value>| -> Result<Option<PathBuf>, String> {
        let Some(rel) = v.and_then(|b| b.get("bundle")).and_then(Value::as_str) else { return Ok(None) };
        if rel.starts_with('/') || rel.split('/').any(|p| p == "..") {
            return Err(format!("runtime.json metal_a56: {rel} is not below the runtime"));
        }
        let p = runtime.join(rel);
        if !p.is_dir() {
            return Err(format!("runtime.json names {rel}, which the staged runtime does not hold"));
        }
        Ok(Some(p))
    };
    let metal = below(entry.get("metal"))?.ok_or("runtime.json metal_a56 names no Metal bundle")?;
    let gl = below(entry.get("gl"))?;
    let target = entry.get("target").and_then(Value::as_str).ok_or("runtime.json metal_a56 names no target")?.to_string();
    Ok(Some(Plugins { metal, gl, target }))
}

/// owner-trust.json's text with `add` in its "cdhashes" list (sorted, each once; every other key kept), and how many
/// were new: the merge `oneios-tool phone addon` makes (oneios-phone owner_trust_with).
pub fn trust_with(text: &str, add: &[String]) -> Result<(String, usize), String> {
    let mut trust = json::parse(text).map_err(|e| format!("owner-trust.json: {e}"))?;
    let list = trust.get("cdhashes").and_then(Value::as_list).ok_or("owner-trust.json has no \"cdhashes\" list")?;
    let mut all = list
        .iter()
        .map(|v| v.as_str().map(str::to_ascii_lowercase).ok_or("owner-trust.json: a CDHash that is not a string"))
        .collect::<Result<Vec<String>, _>>()?;
    let before = all.len();
    all.extend(add.iter().map(|s| s.to_ascii_lowercase()));
    all.sort();
    all.dedup();
    let new = all.len() - before.min(all.len());
    trust.set("cdhashes", Value::List(all.into_iter().map(Value::str).collect()));
    Ok((json::dumps(&trust, Some(2), false) + "\n", new))
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// The image a set is (`<device> <build>` of its manifest.json).
fn set_image(manifest: &Value) -> String {
    let field = |k: &str| manifest.get(k).and_then(Value::as_str).unwrap_or("");
    format!("{} {}", field("device"), field("build"))
}

/// The placing for one set (its folder: manifest.json, overlay/userland/owner-trust.json): Ok(Err(why)) when there is
/// nothing to place (no plug-ins, another image), Ok(Ok(placed)) with the three new files, Err on a fault.
pub fn for_set(set: &Path, plugins: Option<&Plugins>) -> Result<Result<Placed, String>, String> {
    let mpath = set.join(MANIFEST);
    let mut manifest = json::load(&mpath).map_err(|e| format!("{}: {e:#}", mpath.display()))?;
    let image = set_image(&manifest);
    let Some(p) = plugins else {
        return Ok(Err("the staged runtime carries no A56 GPU plug-ins (runtime.json metal_a56)".into()));
    };
    if image != p.target {
        return Ok(Err(format!("the image is {image}, the A56 GPU plug-ins are for {}", p.target)));
    }
    let (addon, cdhashes) = oneios_ios::metal::a56_addon(&p.metal, p.gl.as_deref()).map_err(|e| format!("the A56 addon: {e:#}"))?;
    let tpath = set.join(TRUST);
    let text = std::fs::read_to_string(&tpath).map_err(|e| format!("{}: {e}", tpath.display()))?;
    let (trust, new) = trust_with(&text, &cdhashes)?;
    let files = manifest.get_mut("files").ok_or("manifest.json has no \"files\" list")?;
    let Value::List(list) = files else { return Err("manifest.json \"files\" is not a list".into()) };
    let entry = list
        .iter_mut()
        .find(|f| f.get("path").and_then(Value::as_str) == Some(TRUST))
        .ok_or("manifest.json lists no overlay/userland/owner-trust.json: the addon's code would not be trusted")?;
    entry.set("size", trust.len() as u64);
    entry.set("sha256", sha256_hex(trust.as_bytes()));
    let manifest = json::dumps(&manifest, Some(2), false) + "\n";
    Ok(Ok(Placed { addon, trust, manifest, cdhashes: cdhashes.len(), new }))
}

fn put_file(path: &Path, data: &[u8]) -> Result<(), String> {
    let part = path.with_extension("part");
    std::fs::write(&part, data).and_then(|()| std::fs::rename(&part, path)).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write a placing's three files under `dir` (DIR/kernel.kc.gpuaddon, DIR/owner-trust.json, DIR/manifest.json for
/// the root script; the set's own paths for Make iOS image: `flat` false).
pub fn write(placed: &Placed, dir: &Path, flat: bool) -> Result<(), String> {
    write_sidecar(placed, dir)?;
    write_listed(placed, dir, flat)
}

/// The two files of a placing that manifest.json lists: owner-trust.json and manifest.json itself.
pub fn write_listed(placed: &Placed, dir: &Path, flat: bool) -> Result<(), String> {
    let trust_at = if flat { dir.join("owner-trust.json") } else { dir.join(TRUST) };
    put_file(&trust_at, placed.trust.as_bytes())?;
    put_file(&dir.join(MANIFEST), placed.manifest.as_bytes())
}

/// The sidecar of a placing, `kernel.kc.gpuaddon`, which manifest.json does not list (oneios-phone `SIDECARS`;
/// the lists image.sha256 and image.ids name it); its SHA-256.
pub fn write_sidecar(placed: &Placed, dir: &Path) -> Result<String, String> {
    put_file(&dir.join(ADDON), &placed.addon)?;
    Ok(sha256_hex(&placed.addon))
}

/// Make iOS image's placing, part one (before the set's verify): the owner-trust.json and manifest.json of the
/// placing written into the set. The sidecar goes in only after the verify ([`write_sidecar`]): `image verify`
/// refuses a file its manifest does not list, and kernel.kc.gpuaddon is not listed (issue #87: the A56's Make iOS
/// image stopped at "not in the manifest: kernel.kc.gpuaddon" when all three went in before the verify).
pub fn place_listed(set: &Path, plugins: Option<&Plugins>) -> Result<Result<Placed, String>, String> {
    let placed = match for_set(set, plugins)? {
        Ok(placed) => placed,
        Err(why) => return Ok(Err(why)),
    };
    write_listed(&placed, set, false)?;
    Ok(Ok(placed))
}

/// The line of a placing: the root script reads it as the tool's last line, word for word (the app's `a56_gpu_place`):
/// `@@A56GPU placed: kernel.kc.gpuaddon SHA BYTES; owner-trust.json SHA; manifest.json SHA`. Nothing else on it.
pub fn placed_line(p: &Placed) -> String {
    format!(
        "@@A56GPU placed: {ADDON} {} {}; owner-trust.json {}; manifest.json {}",
        sha256_hex(&p.addon),
        p.addon.len(),
        sha256_hex(p.trust.as_bytes()),
        sha256_hex(p.manifest.as_bytes())
    )
}

/// What the placing put in owner-trust.json: the line before [`placed_line`].
pub fn counts_line(p: &Placed) -> String {
    format!("a56gpu: the addon's {} CDHashes, {} of them new in owner-trust.json", p.cdhashes, p.new)
}

const USAGE: &str = "usage: oneios-tool image a56-gpu-place --stage STAGE --out DIR
  the Galaxy A56's GPU plug-ins of the staged runtime (STAGE/runtime, runtime.json metal_a56) for the staged image set
  (STAGE/image): DIR/kernel.kc.gpuaddon, DIR/owner-trust.json and DIR/manifest.json (owner-trust's entry updated), and
  the last line `@@A56GPU placed: ...`; `@@A56GPU none: WHY` (DIR left empty) for an image they are not for or a
  runtime without them; `@@A56GPU error: WHY` (exit 1). Reads only; writes only into DIR (an empty folder).";

/// `oneios-tool image a56-gpu-place --stage STAGE --out DIR` (the app's root script, docs/a56-gpu.md 6).
pub fn place_cmd(args: &[String]) -> i32 {
    let (mut stage, mut out) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--stage" => stage = it.next().cloned(),
            "--out" => out = it.next().cloned(),
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            _ => {
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    let (Some(stage), Some(out)) = (stage, out) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let (stage, out) = (Path::new(&stage), Path::new(&out));
    let run = || -> Result<String, String> {
        if !out.is_dir() || std::fs::read_dir(out).map_err(|e| format!("{}: {e}", out.display()))?.next().is_some() {
            return Err(format!("{}: not an empty folder", out.display()));
        }
        let plugins = runtime_plugins(&stage.join("runtime"))?;
        match for_set(&stage.join("image"), plugins.as_ref())? {
            Err(why) => Ok(format!("@@A56GPU none: {why}")),
            Ok(placed) => {
                write(&placed, out, true)?;
                println!("{}", counts_line(&placed));
                Ok(placed_line(&placed))
            }
        }
    };
    match run() {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(why) => {
            println!("@@A56GPU error: {why}");
            1
        }
    }
}

#[cfg(test)]
#[path = "a56gpu_tests.rs"]
mod tests;
