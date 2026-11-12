//! Tests of `source.rs` on folders made here (no Apple file): the order of the log files, the string files' index,
//! the two layouts, and the saved archive.

use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oneios-ioslog-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A tracev3 file that is only its header chunk, with these start times.
fn header(wall: u64, continuous: u64) -> Vec<u8> {
    let mut data = vec![0u8; PREAMBLE_SIZE + RAW_HEADER_CHUNK_SIZE];
    data[0..4].copy_from_slice(&0x1000u32.to_le_bytes());
    data[4..8].copy_from_slice(&0x11u32.to_le_bytes());
    data[8..16].copy_from_slice(&(RAW_HEADER_CHUNK_SIZE as u64).to_le_bytes());
    data[24..32].copy_from_slice(&continuous.to_le_bytes());
    data[32..40].copy_from_slice(&wall.to_le_bytes());
    data
}

fn put(root: &Path, rel: &str, data: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

/// `Folder/name` of every tracev3 file, in the source's order.
fn names(source: &Source) -> Vec<String> {
    source
        .tracev3
        .iter()
        .map(|i| {
            let mut parts: Vec<&str> = i.label.rsplit('/').take(2).collect();
            parts.reverse();
            parts.join("/")
        })
        .collect()
}

#[test]
fn a_mounted_volume_and_its_saved_archive() {
    let dir = scratch("layout");
    let vol = dir.join("vol");
    // by the header's times: Special/2 (5), Persist/b (7, 1), Persist/a (7, 2); then the unreadable ones in
    // folder order (HighVolume, Persist, Signpost, Special), names sorted
    put(&vol, "db/diagnostics/Persist/a.tracev3", &header(7, 2));
    put(&vol, "db/diagnostics/Persist/b.tracev3", &header(7, 1));
    put(&vol, "db/diagnostics/Persist/short.tracev3", b"too short");
    put(&vol, "db/diagnostics/Persist/notes.txt", b"not a log");
    put(&vol, "db/diagnostics/Special/2.tracev3", &header(5, 9));
    put(&vol, "db/diagnostics/HighVolume/h.tracev3", b"");
    put(&vol, "db/diagnostics/timesync/0002.timesync", b"t2");
    put(&vol, "db/diagnostics/timesync/0001.timesync", b"t1");
    put(&vol, "db/diagnostics/timesync/readme", b"-");
    put(&vol, "db/uuidtext/AB/CDEF0123456789ABCDEF0123456789", b"uuidtext");
    put(&vol, "db/uuidtext/AB/short", b"no uuid");
    put(&vol, "db/uuidtext/dsc/0123456789ABCDEF0123456789ABCDEF", b"dsc");
    put(&vol, "db/uuidtext/zz/CDEF0123456789ABCDEF0123456789", b"not a hex folder");
    put(&vol, "mobile/x", b"other");

    let source = Source::open(vol.to_str().unwrap(), None, false).unwrap();
    assert!(!source.archive);
    assert_eq!(names(&source), ["Special/2.tracev3", "Persist/b.tracev3", "Persist/a.tracev3", "HighVolume/h.tracev3", "Persist/short.tracev3"]);
    assert!(source.tracev3[0].label.ends_with("/db/diagnostics/Special/2.tracev3"), "{}", source.tracev3[0].label);
    let timesync: Vec<&str> = source.timesync.iter().map(|i| i.label.rsplit('/').next().unwrap()).collect();
    assert_eq!(timesync, ["0001.timesync", "0002.timesync"]);
    assert_eq!(source.string_files(), (1, 1));
    let uuid = Uuid::parse_str("ABCDEF0123456789ABCDEF0123456789").unwrap();
    assert_eq!(source.read_uuidtext(&uuid).unwrap(), b"uuidtext");
    assert_eq!(source.read_dsc(&Uuid::parse_str("0123456789ABCDEF0123456789ABCDEF").unwrap()).unwrap(), b"dsc");
    assert_eq!(source.read_dsc(&uuid).unwrap_err().kind(), ErrorKind::NotFound);

    // the saved archive: the five folders and everything of uuidtext, nothing else
    let saved = dir.join("x.logarchive");
    let (files, _) = source.save(&saved).unwrap();
    assert_eq!(files, 13);
    assert!(saved.join("Persist/notes.txt").is_file() && saved.join("zz").is_dir() && saved.join("dsc").is_dir());
    assert!(!saved.join("mobile").exists() && !saved.join("db").exists());
    let again = Source::open(saved.to_str().unwrap(), None, false).unwrap();
    assert!(again.archive);
    assert_eq!(names(&again), names(&source));
    assert_eq!(again.string_files(), (1, 1));
    assert_eq!(again.tracev3[0].label, saved.join("Special/2.tracev3").to_str().unwrap());
    // a copy of an archive is the archive
    let twice = dir.join("y.logarchive");
    assert_eq!(again.save(&twice).unwrap().0, 13);

    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(Source::open(empty.to_str().unwrap(), None, false).is_err());
    assert!(Source::open(dir.join("nothing").to_str().unwrap(), None, false).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn extensions() {
    assert!(has_extension("a.tracev3", "tracev3") && has_extension("._a.tracev3", "tracev3"));
    assert!(!has_extension(".tracev3", "tracev3") && !has_extension("tracev3", "tracev3") && !has_extension("a.tracev3.x", "tracev3"));
}
