//! archive.rs's tests: a stand-in set packed and verified, and every way `verify` refuses an archive. Where
//! python3 and scripts/image/imagetool.py are there (the frozen original), each case is also run with it: the
//! archive's bytes and each message must be the same.

use super::*;
use crate::fixtures::{fake_set, Tmp};
use std::process::Command;

/// A stand-in set with its manifest.
fn set(tmp: &Tmp) -> String {
    let set = fake_set(tmp, true, true);
    let profile = profile::load_profile(&tmp.path("test.toml")).unwrap();
    manifest::make_manifest(&profile, &set, None, None, None).unwrap();
    set
}

/// imagetool.py's stdout, the last line of its stderr and its status; None where python3 or the script is not there.
fn python(args: &[&str]) -> Option<(String, String, i32)> {
    let tool = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../scripts/image/imagetool.py");
    if !tool.is_file() {
        return None;
    }
    let out = Command::new("python3").arg("-I").arg(tool).args(args).output().ok()?;
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim_end().to_string();
    Some((text(&out.stdout), text(&out.stderr).lines().last().unwrap_or("").to_string(), out.status.code().unwrap_or(-1)))
}

/// An archive of these files of the set, in this order (what `pack` writes for the manifest's own list).
fn archive_of(tmp: &Tmp, name: &str, setdir: &str, files: &[&str]) -> String {
    let out = tmp.path(name);
    let files: Vec<String> = files.iter().map(|f| f.to_string()).collect();
    let mut tar = Vec::new();
    write_set(setdir, &files, &mut tar).unwrap();
    let mut zstd = Command::new("zstd").args(["-q", "-c"]).stdin(Stdio::piped()).stdout(std::fs::File::create(&out).unwrap()).spawn().unwrap();
    zstd.stdin.take().unwrap().write_all(&tar).unwrap();
    assert!(zstd.wait().unwrap().success());
    out
}

/// `verify`'s refusal of an archive, which must be imagetool.py's too.
fn refusal(path: &str) -> String {
    let Err(Fail::Error(message)) = verify_archive(path) else { panic!("{path}: accepted, or another kind of failure") };
    if let Some((_, said, status)) = python(&["verify", path]) {
        assert_eq!((said.as_str(), status), (format!("imagetool: {message}").as_str(), 1), "imagetool.py on {path}");
    }
    message.replace(path, "ARCHIVE")
}

#[test]
fn a_packed_set_verifies_and_is_pythons_archive() {
    if need_zstd().is_err() {
        eprintln!("skipped: no zstd");
        return;
    }
    let tmp = Tmp::new("archive");
    let setdir = set(&tmp);
    let out = tmp.path("set.tar.zst");
    let digest = pack(&setdir, &out).unwrap();
    assert_eq!(std::fs::read_to_string(format!("{out}.sha256")).unwrap(), format!("{digest}  set.tar.zst\n"));
    assert!(!std::path::Path::new(&format!("{out}.part")).exists());
    let line = manifest::ok_line(&verify_archive(&out).unwrap());
    assert_eq!(line, manifest::ok_line(&manifest::verify_dir(&setdir, false).unwrap()));
    if let Some((said, _, status)) = python(&["pack", &setdir, &tmp.path("py.tar.zst")]) {
        assert_eq!(status, 0);
        assert_eq!(said, format!("{digest}  {}", tmp.path("py.tar.zst")), "the two archives' SHA-256");
        assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(tmp.path("py.tar.zst")).unwrap());
        assert_eq!(python(&["verify", &out]).unwrap(), (line, String::new(), 0));
    }
}

#[test]
fn verify_refuses_what_is_not_the_manifests_set() {
    if need_zstd().is_err() {
        eprintln!("skipped: no zstd");
        return;
    }
    let tmp = Tmp::new("archive-refused");
    let setdir = set(&tmp);
    let m = manifest::verify_dir(&setdir, true).unwrap();
    let listed: Vec<&str> = m.get("files").and_then(Value::as_list).unwrap().iter().map(|f| f.get("path").and_then(Value::as_str).unwrap()).collect();
    let whole: Vec<&str> = std::iter::once("manifest.json").chain(listed.iter().copied()).collect();

    // the sidecar's hash is another archive's
    let good = archive_of(&tmp, "good.tar.zst", &setdir, &whole);
    assert!(verify_archive(&good).is_ok());
    std::fs::write(format!("{good}.sha256"), format!("{}  good.tar.zst\n", "0".repeat(64))).unwrap();
    assert_eq!(refusal(&good), "ARCHIVE: SHA-256 differs from good.tar.zst.sha256");

    assert_eq!(refusal(&archive_of(&tmp, "first.tar.zst", &setdir, &whole[1..])), format!("ARCHIVE: the first file is {}, not manifest.json", whole[1]));
    let twice: Vec<&str> = whole.iter().copied().chain([whole[1]]).collect();
    assert_eq!(refusal(&archive_of(&tmp, "twice.tar.zst", &setdir, &twice)), format!("ARCHIVE: {} is not in the manifest, or is there twice", whole[1]));
    let mut missing: Vec<&str> = whole[whole.len() - 2..].to_vec();
    missing.sort_unstable();
    assert_eq!(refusal(&archive_of(&tmp, "missing.tar.zst", &setdir, &whole[..whole.len() - 2])), format!("ARCHIVE: missing {}", missing.join(", ")));

    // a file the manifest does not list; a file changed after the manifest was made
    std::fs::write(format!("{setdir}/extra.bin"), b"not listed").unwrap();
    let extra: Vec<&str> = whole.iter().copied().chain(["extra.bin"]).collect();
    assert_eq!(refusal(&archive_of(&tmp, "extra.tar.zst", &setdir, &extra)), "ARCHIVE: extra.bin is not in the manifest, or is there twice");
    std::fs::write(format!("{setdir}/trust/app.root_hash"), b"another root hash").unwrap();
    assert_eq!(refusal(&archive_of(&tmp, "changed.tar.zst", &setdir, &whole)), "ARCHIVE: trust/app.root_hash differs from the manifest");

    // no tar at all, and no archive at all
    assert_eq!(refusal(&archive_of(&tmp, "empty.tar.zst", &setdir, &[])), "ARCHIVE: no manifest.json");
    let not_zstd = tmp.write("text.tar.zst", b"not a zstd stream");
    assert_eq!(refusal(&not_zstd), "ARCHIVE: empty file");
}
