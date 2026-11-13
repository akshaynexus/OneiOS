//! The APFS file reader (`apfs/files.rs`) on an image made by tests/ioslog/mkapfs.py from a folder made here:
//! listings, file contents (one extent, two extents, empty), a link's target, the choice of the volume, and the
//! newest checkpoint against block 0. No Apple file. mkapfs.py itself is checked against apfs-fuse by
//! tests/ioslog/compare.sh; this test needs python3 and is skipped (with a line on stderr) where there is none.

use oneios_image::apfs::{ApfsTree, DT_DIR, DT_LNK, DT_REG};
use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oneios-apfs-files-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Bytes that differ from block to block.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| ((i / 7) as u8) ^ (i as u8).wrapping_mul(31) ^ seed).collect()
}

#[test]
fn files_of_a_made_image() {
    let mkapfs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/ioslog/mkapfs.py");
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipped: no python3 for {}", mkapfs.display());
        return;
    }
    let dir = scratch("made");
    let src = dir.join("src");
    std::fs::create_dir_all(src.join("db/diagnostics/Persist")).unwrap();
    std::fs::create_dir_all(src.join("empty-dir")).unwrap();
    let small = pattern(5000, 1);
    let big = pattern(200_000, 2); // 49 blocks: two extents
    std::fs::write(src.join("db/diagnostics/Persist/0000000000000001.tracev3"), &small).unwrap();
    std::fs::write(src.join("db/big"), &big).unwrap();
    std::fs::write(src.join("db/none"), b"").unwrap();
    std::os::unix::fs::symlink("diagnostics/Persist", src.join("db/link")).unwrap();
    for i in 0..400 {
        // enough records for several leaf nodes and an index node
        std::fs::write(src.join(format!("empty-dir/f{i:03}")), format!("file {i}")).unwrap();
    }
    let other = dir.join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("x"), b"x").unwrap();
    let image = dir.join("t.img");
    let made = Command::new("python3")
        .arg("-I")
        .arg(&mkapfs)
        .arg(&image)
        .arg(format!("First={}", other.display()))
        .arg("Empty=")
        .arg(format!("Data={}", src.display()))
        .arg("--stale-block0")
        .output()
        .unwrap();
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    let image = image.to_str().unwrap();

    assert_eq!(ApfsTree::volume_count(image, true).unwrap(), 3);
    let tree = ApfsTree::open_volume(image, 2, true).unwrap();
    assert_eq!((tree.name.as_str(), tree.role.as_str()), ("Data", "none"));

    let mut top: Vec<(String, u16)> = tree.list(2).unwrap().into_iter().map(|e| (e.name, e.kind)).collect();
    top.sort();
    assert_eq!(top, [("db".to_string(), DT_DIR), ("empty-dir".to_string(), DT_DIR)]);
    let db = tree.find("db").unwrap().unwrap();
    let mut names: Vec<(String, u16)> = tree.list(db).unwrap().into_iter().map(|e| (e.name, e.kind)).collect();
    names.sort();
    assert_eq!(
        names,
        [("big".to_string(), DT_REG), ("diagnostics".to_string(), DT_DIR), ("link".to_string(), DT_LNK), ("none".to_string(), DT_REG)]
    );

    let read = |path: &str| tree.read_file(tree.find(path).unwrap().unwrap()).unwrap();
    assert_eq!(read("db/diagnostics/Persist/0000000000000001.tracev3"), small);
    assert_eq!(read("/db/big"), big);
    assert_eq!(read("db/none"), b"");
    assert_eq!(read("empty-dir/f399"), b"file 399");
    assert_eq!(tree.list(tree.find("empty-dir").unwrap().unwrap()).unwrap().len(), 400);
    let st = tree.stat(tree.find("db/big").unwrap().unwrap()).unwrap();
    assert_eq!((st.size, st.mode), (200_000, 0o100644));
    let link = tree.find("db/link").unwrap().unwrap();
    assert_eq!(tree.xattr(link, "com.apple.fs.symlink").unwrap().unwrap(), b"diagnostics/Persist\0");
    assert!(tree.find("db/nothing").unwrap().is_none());

    // block 0 is one transaction behind: the volumes are empty there
    let stale = ApfsTree::open_volume(image, 2, false).unwrap();
    assert!(stale.list(2).unwrap().is_empty());
    assert!(stale.find("db").unwrap().is_none());
    // the other volumes
    let first = ApfsTree::open_volume(image, 0, true).unwrap();
    assert_eq!(first.name, "First");
    assert_eq!(first.read_file(first.find("x").unwrap().unwrap()).unwrap(), b"x");
    assert!(ApfsTree::open_volume(image, 1, true).unwrap().list(2).unwrap().is_empty());
    assert!(ApfsTree::open_volume(image, 3, true).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
