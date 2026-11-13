//! `oneios-tool ioslog` on Data stores whose newest checkpoint was cut off in the middle of a write: `show`,
//! `boots`, `checks` and `save` must give the log of the newest checkpoint that passes the checks, exactly as
//! they give it from that state's folder, and say on stderr which transaction they read and what they skipped.
//! The stores: tests/support/apfs_store.rs (two of mkapfs.py's grafted into one container, then damaged).
//!
//! The log files are stand-ins made here (header chunks only: no entries) unless a real archive is named:
//!
//!   ONEIOS_TEST_LOGARCHIVE=DIR.logarchive   the newer state holds the archive's files, the older one the same
//!                                           without the last files of Persist; the CSV of `show`, `boots` and
//!                                           `checks` are compared
//!   ONEIOS_TOOL=PATH                        the binary to run (default: this build's; the release build is much
//!                                           faster on a real archive)
//!   ONEIOS_TEST_FUSE=1                      also: apfs-fuse must read the good store as its newer state
//!                                           (`--apfs fuse`), which checks the store's maker
//!   ONEIOS_TEST_WORK=DIR                    where the stores are made (default: the temporary folder) and kept
//!
//! Needs python3 (mkapfs.py); skipped with a line on stderr where there is none.

#[path = "../../../tests/support/apfs_store.rs"]
mod store;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use store::{Layout, Store};

fn tool() -> PathBuf {
    std::env::var_os("ONEIOS_TOOL").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_oneios-tool")))
}

/// `oneios-tool ioslog ARGS`: (exit status, stdout, stderr).
fn ioslog(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(tool()).arg("ioslog").args(args).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Every file below a folder: path -> contents.
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(path.strip_prefix(root).unwrap().to_str().unwrap().to_string(), std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// A tracev3 file that is only its header chunk, with these start times (the decoder reads no entry from it).
fn header(wall: u64, continuous: u64) -> Vec<u8> {
    let mut data = vec![0u8; 16 + 208];
    data[0..4].copy_from_slice(&0x1000u32.to_le_bytes());
    data[4..8].copy_from_slice(&0x11u32.to_le_bytes());
    data[8..16].copy_from_slice(&208u64.to_le_bytes());
    data[24..32].copy_from_slice(&continuous.to_le_bytes());
    data[32..40].copy_from_slice(&wall.to_le_bytes());
    data
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (path, target) = (e.path(), to.join(e.file_name()));
        if path.is_dir() {
            copy_dir(&path, &target);
        } else if path.is_file() {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

/// The Data volume's folder of the older and of the newer state; true when the log is a real one.
fn states(dir: &Path) -> (PathBuf, PathBuf, bool) {
    let (older, newer) = (dir.join("older"), dir.join("newer"));
    let real = std::env::var_os("ONEIOS_TEST_LOGARCHIVE").map(PathBuf::from);
    match &real {
        Some(archive) => {
            // as the phone's Data volume has it: /private/var is the volume's root (tests/ioslog/compare.sh step 3)
            let mut persist: Vec<String> = Vec::new();
            for e in std::fs::read_dir(archive).unwrap_or_else(|e| panic!("ONEIOS_TEST_LOGARCHIVE {}: {e}", archive.display())).flatten() {
                let name = e.file_name().to_str().unwrap().to_string();
                let to = match name.as_str() {
                    "Persist" | "Special" | "Signpost" | "HighVolume" | "timesync" => format!("db/diagnostics/{name}"),
                    "Extra" | "Info.plist" | "logdata.LiveData.tracev3" => continue,
                    _ => format!("db/uuidtext/{name}"),
                };
                if name == "Persist" {
                    persist = std::fs::read_dir(e.path()).unwrap().flatten().map(|f| f.file_name().to_str().unwrap().to_string()).collect();
                }
                for root in [&older, &newer] {
                    if e.path().is_dir() {
                        copy_dir(&e.path(), &root.join(&to));
                    } else {
                        std::fs::create_dir_all(root.join(&to).parent().unwrap()).unwrap();
                        std::fs::copy(e.path(), root.join(&to)).unwrap();
                    }
                }
            }
            // the older state: before the last files of Persist were written
            persist.sort();
            assert!(persist.len() >= 2, "the archive's Persist folder has {} files", persist.len());
            for name in &persist[persist.len() - (persist.len() / 3).max(1)..] {
                std::fs::remove_file(older.join("db/diagnostics/Persist").join(name)).unwrap();
            }
        }
        None => {
            for (root, grown) in [(&older, false), (&newer, true)] {
                store::put(root, "db/diagnostics/Persist/0000000000000001.tracev3", &header(7, 1));
                store::put(root, "db/diagnostics/Special/0000000000000002.tracev3", &[header(5, 9), store::pattern(if grown { 150_000 } else { 60_000 }, 2)].concat());
                store::put(root, "db/diagnostics/timesync/0000000000000001.timesync", b"");
                store::put(root, "db/uuidtext/AB/CDEF0123456789ABCDEF0123456789", b"uuidtext");
                store::put(root, "db/uuidtext/dsc/0123456789ABCDEF0123456789ABCDEF", b"dsc");
                if grown {
                    store::put(root, "db/diagnostics/Persist/0000000000000003.tracev3", &header(9, 3));
                    store::put(root, "db/uuidtext/CD/EF0123456789ABCDEF0123456789AB", b"more");
                }
            }
        }
    }
    for root in [&older, &newer] {
        store::put(root, "root/log/other.txt", b"not a log\n");
        for i in 0..200 {
            store::put(root, &format!("mobile/Library/f{i:03}"), format!("file {i}").as_bytes());
        }
    }
    (older, newer, real.is_some())
}

/// What the tool gives from a source: `show -f csv`, `boots`, `checks` (status and stdout, the source's own name
/// taken out), and the files `save` writes.
#[derive(PartialEq)]
struct Log {
    csv: Vec<u8>,
    boots: (i32, String),
    checks: (i32, String),
    saved: BTreeMap<String, Vec<u8>>,
}

/// The log of `source` (`what`: how the tool names it in its output), and the stderr of the four commands.
fn log_of(source: &str, what: &str, more: &[&str], work: &Path) -> (Log, String) {
    let csv = work.join("out.csv");
    let saved = work.join("out.logarchive");
    let _ = std::fs::remove_file(&csv);
    let _ = std::fs::remove_dir_all(&saved);
    let mut said = String::new();
    let mut run = |args: &[&str]| {
        let (status, out, err) = ioslog(&[args, more].concat());
        said += &err;
        (status, out.replace(what, "SOURCE"))
    };
    let show = run(&["show", source, "-f", "csv", "-o", csv.to_str().unwrap()]);
    assert_eq!(show.0, 0, "ioslog show {source}: {said}");
    let boots = run(&["boots", source]);
    let checks = run(&["checks", source]);
    let save = run(&["save", source, saved.to_str().unwrap()]);
    assert_eq!(save.0, 0, "ioslog save {source}: {said}");
    (Log { csv: std::fs::read(&csv).unwrap(), boots, checks, saved: files(&saved) }, said)
}

#[test]
fn the_log_of_the_newest_checkpoint_that_passes() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    if !store::can_make(manifest) {
        return;
    }
    let keep = std::env::var_os("ONEIOS_TEST_WORK").map(PathBuf::from);
    let dir = keep.clone().unwrap_or_else(|| store::scratch("ioslog"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (older, newer, real) = states(&dir);
    let (good, old, new) = store::graft(&store::make(manifest, &dir, "older", &older), &store::make(manifest, &dir, "newer", &newer));

    // the reference: the same commands on the two states' folders
    let (want_old, _) = log_of(older.to_str().unwrap(), older.to_str().unwrap(), &[], &dir);
    let (want_new, _) = log_of(newer.to_str().unwrap(), newer.to_str().unwrap(), &[], &dir);
    assert!(want_old != want_new && want_old.saved != want_new.saved, "the two states hold the same log");
    assert_eq!(want_new.boots.0, 0, "ioslog boots of the newer state's folder");
    if real {
        assert!(want_old.csv != want_new.csv && want_old.csv.len() > 10_000, "the older state's CSV has {} bytes", want_old.csv.len());
        assert_eq!((want_old.boots.0, want_old.checks.0, want_new.checks.0), (0, 0, 0), "boots and checks of the folders");
        eprintln!("a real log: CSV of {} bytes (older state) and {} bytes (newer state)", want_old.csv.len(), want_new.csv.len());
    }

    type Damage = fn(&mut Store, &Layout, &Layout);
    // (case, the damage, the options, whether the older state is expected, what stderr must hold)
    let cases: [(&str, Damage, &[&str], bool, &[&str]); 9] = [
        ("good", |_, _, _| {}, &[], false, &["volume 2 (Data) is read at checkpoint xid 6 (superblock at block 4, volume superblock xid 5): the newest of 2; block 0 has xid 5;"]),
        ("good --block0", |_, _, _| {}, &["--block0"], true, &["volume 2 (Data) is read as block 0 has it (xid 5, volume superblock xid 5)"]),
        ("good --checkpoint 5", |_, _, _| {}, &["--checkpoint", "5"], true, &["is read at checkpoint xid 5 (superblock at block 2, volume superblock xid 5): the one asked for of 2;"]),
        ("a superblock checksum", |s, _, new| s.spoil_checksum(new.superblock), &[], true, &["is read at checkpoint xid 5 (superblock at block 2", "1 newer of 2 skipped:", "\n  xid 6 at block 4: refused the container superblock at block 4 (0x4)", "its checksum is wrong (stored 0x"]),
        (
            "b omap past the end",
            |s, _, new| {
                s.put64(new.superblock, 160, store::PATTERN_BLOCK);
                s.seal(new.superblock);
            },
            &[],
            true,
            &["is read at checkpoint xid 5", "1 newer of 2 skipped:", "\n  xid 6 at block 4: refused the container's object map at block 281470681808895 (0xffff0000ffff), named by block 4: the address is past the container's end ("],
        ),
        ("c leaf half zeros", |s, _, new| s.half_zeros(new.volumes[2].leaf(), 2048), &[], true, &["is read at checkpoint xid 5", "1 newer of 2 skipped:", "\n  xid 6 at block 4: volume 2, superblock xid 5 at block ", "refused the node of the file-system tree at block ", "its checksum is wrong"]),
        ("c omap node half pattern", |s, _, new| s.half_pattern(new.volumes[2].omap.nodes[0], 100), &[], true, &["is read at checkpoint xid 5", "refused the node of the volume's object map tree at block ", "half written, or not an object"]),
        (
            "c with a history in the container's object map",
            |s, old, new| {
                store::add_history(s, old, new, 2);
                s.half_zeros(new.volumes[2].root(), 2048);
            },
            &[],
            true,
            &["is read at checkpoint xid 6 (superblock at block 4, volume superblock xid 5); a newer state of the volume in this checkpoint was skipped:", "(an older state of the volume in the same checkpoint is read)"],
        ),
        ("a cut-off copy", |s, _, new| s.data.truncate(new.volumes[2].omap.block.min(new.omap.block) as usize * store::BLOCK), &[], true, &["is read at checkpoint xid 5", "the block is past the file's end (the file holds "]),
    ];
    for (n, (case, damage, options, is_older, says)) in cases.iter().enumerate() {
        let mut s = good.clone();
        damage(&mut s, &old, &new);
        let image = s.write(&dir.join(format!("case-{n}.img")));
        let (got, said) = log_of(&image, &format!("{image} (volume 2, Data)"), options, &dir);
        let want = if *is_older { &want_old } else { &want_new };
        assert!(got.saved == want.saved, "{case}: `save` wrote other files than the {} state has ({} and {})", if *is_older { "older" } else { "newer" }, got.saved.len(), want.saved.len());
        assert!(got.csv == want.csv, "{case}: the CSV of `show` differs ({} and {} bytes)", got.csv.len(), want.csv.len());
        assert_eq!(got.boots, want.boots, "{case}: boots\n{said}");
        assert_eq!(got.checks, want.checks, "{case}: checks\n{said}");
        for part in *says {
            // each of the four commands says it
            assert_eq!(said.matches(part).count(), 4, "{case}: {part:?} is not said by each command:\n{said}");
        }
        assert!(!said.contains("short read"), "{case}: {said}");
        if keep.is_none() {
            std::fs::remove_file(&image).unwrap();
        }
    }

    // (d) no checkpoint passes: one clear error, status 1, for every command
    let mut s = good.clone();
    s.spoil_checksum(new.superblock);
    s.half_zeros(old.omap.block, 40);
    let image = s.write(&dir.join("case-none.img"));
    for command in ["show", "boots", "checks"] {
        let (status, out, said) = ioslog(&[command, &image, "--apfs", "own"]);
        assert_eq!((status, out.as_str()), (1, ""), "{command}: {said}");
        assert!(said.starts_with(&format!("ioslog: {image}: none of the 3 container superblocks can be read:\n  xid 6 at block 4: refused the container superblock at block 4 (0x4)")), "{said}");
        assert!(said.contains("\n  xid 5 at block 2: refused the container's object map at block ") && said.contains("\n  xid 5 at block 0: refused the container's object map at block ") && !said.contains("short read"), "{said}");
    }
    // the Data volume passes at no checkpoint, the others do: the error names the volume
    let mut s = good.clone();
    s.half_zeros(new.volumes[2].leaf(), 2048);
    s.half_pattern(old.volumes[2].leaf(), 2048);
    let image = s.write(&dir.join("case-volume.img"));
    let (status, _, said) = ioslog(&["boots", &image, "--apfs", "own"]);
    assert_eq!(status, 1, "{said}");
    assert!(said.contains("no volume with a unified log:") && said.contains("volume 2 cannot be read at any of the 2 checkpoints:") && said.matches("refused the node of the file-system tree at block ").count() == 2, "{said}");
    assert!(said.contains("volume 0 (Preboot): no log folders") && !said.contains("short read"), "{said}");

    if std::env::var_os("ONEIOS_TEST_FUSE").is_some() {
        // apfs-fuse reads the newest checkpoint: the good store must be the newer state for it too
        let image = good.write(&dir.join("good.img"));
        let (got, said) = log_of(&image, &format!("{image} (volume 2, through apfs-fuse)"), &["--apfs", "fuse"], &dir);
        assert!(got.saved == want_new.saved && got.csv == want_new.csv, "apfs-fuse reads another log from the good store: {said}");
        eprintln!("apfs-fuse reads the good store as its newer state ({} files)", got.saved.len());
    }
    if keep.is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}
