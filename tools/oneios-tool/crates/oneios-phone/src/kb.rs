//! `oneios-tool app kb BUILD`: the pairing files of one iOS build (format 2, docs/releases.md "The pairing files'
//! format") in the fork's `assets/kb/` (24A446) or `assets/kb-<BUILD>/` (another build: 24B156), made from this
//! checkout's records: the OS profile `kernel/unixnu/profiles/os/ios-*-BUILD/profile.toml` as `profile.toml`,
//! `kernel/prelink/reviewed-BUILD-v54.json` as `reviewed.json`, `lead/reviewed-lead-BUILD-v54.json` as `lead.json`,
//! `user/reviewed-user-BUILD-v54.json` as `user.json`, `pairs-BUILD.json`, `stubs-BUILD.json`, XnuPrelink's reviewed
//! fixtures under `match/fixtures/` (the folder kernel/prelink/build.sh takes), and `kb.json` (the build, the device,
//! the profile's kernelcache SHA-256 and every file's). Until 2026-11-25 the folder was copied by hand; this writes the
//! same bytes (the fork's `assets/kb/` of `38dc4f1` again, byte for byte). `--check` writes nothing and fails when the
//! folder differs.

use anyhow::{anyhow, bail, Context, Result};
use oneios_common::json::{self, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The default build: its folder is `assets/kb/` (the one every APK since 0.5.46 has).
pub const DEFAULT_BUILD: &str = "24A446";

/// The folder of `build` in the fork, relative to the fork's checkout.
pub fn folder(build: &str) -> String {
    if build == DEFAULT_BUILD {
        "app-android/app/src/full/assets/kb".to_string()
    } else {
        format!("app-android/app/src/full/assets/kb-{build}")
    }
}

/// A `key = "value"` of a TOML file (the first one: the profile's keys used here are each there once).
fn toml_string(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then(|| v.split('#').next().unwrap_or("").trim().trim_matches('"').to_string())
    })
}

/// The files of `build`'s folder: (the name in the folder, the source in the checkout), sorted by name.
pub fn sources(repo: &Path, build: &str) -> Result<Vec<(String, PathBuf)>> {
    let os = repo.join("kernel/unixnu/profiles/os");
    let mut profiles: Vec<PathBuf> = fs::read_dir(&os)
        .with_context(|| os.display().to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("ios-") && n.ends_with(&format!("-{build}"))))
        .collect();
    profiles.sort();
    let [profile] = profiles.as_slice() else {
        bail!("kb: {} OS profiles for {build} in {} (one wanted)", profiles.len(), os.display());
    };
    let prelink = repo.join("kernel/prelink");
    let mut out = vec![
        ("profile.toml".to_string(), profile.join("profile.toml")),
        ("reviewed.json".to_string(), prelink.join(format!("reviewed-{build}-v54.json"))),
        ("lead.json".to_string(), prelink.join(format!("lead/reviewed-lead-{build}-v54.json"))),
        ("user.json".to_string(), prelink.join(format!("user/reviewed-user-{build}-v54.json"))),
        ("pairs.json".to_string(), prelink.join(format!("pairs-{build}.json"))),
        ("stubs.json".to_string(), prelink.join(format!("stubs-{build}.json"))),
    ];
    // kernel/prelink/build.sh's matcher folder: external/XnuPrelink, else the kernel base's submodule
    let own = repo.join("external/XnuPrelink/fixtures");
    let fixtures = if own.is_dir() { own } else { repo.join("external/Droid-VM-UniXNU/kits/XnuPrelink/fixtures") };
    let mut found: Vec<PathBuf> = fs::read_dir(&fixtures)
        .with_context(|| format!("{} (scripts/setup/get-references.sh XnuPrelink)", fixtures.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("fileset-") && n.ends_with("-symbols.json")))
        .collect();
    found.sort();
    if found.is_empty() {
        bail!("kb: no fileset-*-symbols.json in {}", fixtures.display());
    }
    for f in found {
        out.push((format!("match/fixtures/{}", f.file_name().unwrap().to_string_lossy()), f));
    }
    for (_, path) in &out {
        if !path.is_file() {
            bail!("kb: {} is missing: the records of build {build} (kernel/prelink/README.md, \"Another build\")", path.display());
        }
    }
    out.sort();
    Ok(out)
}

/// `kb.json` for those files: the build, the device and kernelcache the profile pins, each file's SHA-256.
pub fn manifest(build: &str, files: &[(String, Vec<u8>)]) -> Result<String> {
    let profile = files.iter().find(|(n, _)| n == "profile.toml").ok_or_else(|| anyhow!("kb: no profile.toml"))?;
    let text = String::from_utf8_lossy(&profile.1);
    let get = |key: &str| toml_string(&text, key).ok_or_else(|| anyhow!("kb: the OS profile has no {key}"));
    if get("build")? != build {
        bail!("kb: the OS profile's build is {}, not {build}", get("build")?);
    }
    let mut sums = BTreeMap::new();
    for (name, bytes) in files {
        sums.insert(name.clone(), oneios_common::hex(&Sha256::digest(bytes)));
    }
    let mut listed = Value::dict();
    for (name, sum) in sums {
        listed.set(name.as_str(), sum);
    }
    let mut kb = Value::dict();
    kb.set("build", build);
    kb.set("device", get("product")?);
    kb.set("files", listed);
    kb.set("kb", 2i128);
    kb.set("kernelcache_sha256", get("kernelcache_sha256")?);
    kb.set("profile", "profile.toml");
    Ok(json::dumps(&kb, Some(1), false))
}

/// Write (or with `check`, compare) `build`'s folder in the fork. Prints one line per file that is (or would be)
/// written; with `check` an error names the files that differ.
pub fn run(repo: &Path, fork: &Path, build: &str, check: bool) -> Result<()> {
    let mut files = Vec::new();
    for (name, path) in sources(repo, build)? {
        files.push((name, fs::read(&path).with_context(|| path.display().to_string())?));
    }
    let kb_json = manifest(build, &files)?;
    files.push(("kb.json".to_string(), kb_json.into_bytes()));
    let dir = fork.join(folder(build));
    let mut differ = Vec::new();
    for (name, bytes) in &files {
        let at = dir.join(name);
        if fs::read(&at).ok().as_deref() != Some(bytes.as_slice()) {
            differ.push(name.clone());
            if !check {
                fs::create_dir_all(at.parent().unwrap()).with_context(|| at.display().to_string())?;
                fs::write(&at, bytes).with_context(|| at.display().to_string())?;
                println!("kb: wrote {} ({} bytes)", at.display(), bytes.len());
            }
        }
    }
    // a file of the folder that is not one of these (a record of another revision) is an error in both modes
    let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    let mut extra = Vec::new();
    let mut walk = vec![dir.clone()];
    while let Some(d) = walk.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk.push(p);
            } else if let Ok(rel) = p.strip_prefix(&dir) {
                let rel = rel.to_string_lossy().into_owned();
                if !names.contains(&rel.as_str()) {
                    extra.push(rel);
                }
            }
        }
    }
    if !extra.is_empty() {
        extra.sort();
        bail!("kb: {} holds files that are not build {build}'s pairing files: {}", dir.display(), extra.join(", "));
    }
    if check && !differ.is_empty() {
        bail!("kb: {} differs from this checkout's records of {build}: {} (oneios-tool app kb {build})", dir.display(), differ.join(", "));
    }
    println!("kb: {} is build {build}'s ({} files{})", dir.display(), files.len(), if differ.is_empty() { ", none changed" } else { "" });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_names_the_profiles_pins() {
        let profile = b"[ipsw]\nproduct = \"iPhone18,2\"\nbuild = \"24B156\"\n[kernel.prelink]\nkernelcache_sha256 = \"f6f3\" # pinned\n".to_vec();
        let files = vec![("pairs.json".to_string(), b"{}".to_vec()), ("profile.toml".to_string(), profile)];
        let text = manifest("24B156", &files).unwrap();
        assert!(text.starts_with("{\n \"build\": \"24B156\",\n \"device\": \"iPhone18,2\",\n \"files\": {\n  \"pairs.json\": \"44136fa3"), "{text}");
        assert!(text.ends_with("\"kernelcache_sha256\": \"f6f3\",\n \"profile\": \"profile.toml\"\n}"), "{text}");
        assert!(manifest("24A446", &files).unwrap_err().to_string().contains("build is 24B156"));
        assert_eq!(folder("24A446"), "app-android/app/src/full/assets/kb");
        assert_eq!(folder("24B156"), "app-android/app/src/full/assets/kb-24B156");
    }

    /// This checkout's records make the fork's kb folders, when the fork is beside the checkout (app/).
    #[test]
    fn every_build_has_its_records() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        for build in ["24A446", "24B156"] {
            let files = sources(&repo, build).unwrap();
            assert!(files.iter().any(|(n, _)| n == "match/fixtures/fileset-24A446-symbols.json"), "{build}");
            assert_eq!(files.len(), 9, "{build}: {files:?}");
        }
    }
}
