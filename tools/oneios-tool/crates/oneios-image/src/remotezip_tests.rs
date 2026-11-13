//! remotezip.rs's tests, on a local stand-in archive (scripts/image/tests/test_image.py's RemoteZipTest: the
//! list and the fetch, a resumed fetch, a chunk that was marked done and is not).

use super::*;
use crate::fixtures::{noise, Tmp};
use std::sync::Mutex;

/// A ZIP of (name, data, deflated): local headers, the directory, the end record.
fn zip_of(members: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let (mut out, mut directory) = (Vec::new(), Vec::new());
    for (name, data, deflated) in members {
        let packed = if *deflated {
            let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        } else {
            data.to_vec()
        };
        // version, flags, method, time, date, crc, packed size, size, name length, extra length
        let mut fields = Vec::new();
        for (value, width) in [(20u32, 2), (0, 2), (if *deflated { 8 } else { 0 }, 2), (0, 2), (0x21, 2), (crc32fast::hash(data), 4), (packed.len() as u32, 4), (data.len() as u32, 4), (name.len() as u32, 2), (0, 2)] {
            fields.extend_from_slice(&value.to_le_bytes()[..width]);
        }
        let offset = out.len() as u32;
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&fields);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&packed);
        directory.extend_from_slice(b"PK\x01\x02\x14\x00");
        directory.extend_from_slice(&fields);
        directory.extend_from_slice(&[0u8; 10]); // comment length, disk, attributes
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let (start, count) = (out.len() as u32, members.len() as u16);
    out.extend_from_slice(&directory);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// The stand-in archive: a stored image and a deflated file in a folder; and the two members' bytes.
fn make(tmp: &Tmp) -> (String, Vec<u8>, Vec<u8>) {
    let (stored, packed) = (noise(20000, 1), b"trust cache ".repeat(700));
    let zip = tmp.write("a.zip", &zip_of(&[("big.dmg.aea", &stored, false), ("Firmware/x.trustcache", &packed, true)]));
    (zip, stored, packed)
}

#[test]
fn list_and_get() {
    let tmp = Tmp::new("remotezip");
    let (zip, stored, packed) = make(&tmp);
    let z = RemoteZip::open(&zip).unwrap();
    assert_eq!(z.infos().iter().map(|i| i.filename.as_str()).collect::<Vec<_>>(), ["big.dmg.aea", "Firmware/x.trustcache"]);
    let log = Mutex::new(Vec::new());
    let say = |line: String| log.lock().unwrap().push(line);
    assert_eq!(z.get("big.dmg.aea", &tmp.path("big"), 3, 4096, &say).unwrap(), (20000, crc32fast::hash(&stored)));
    z.get("x.trustcache", &tmp.path("x"), 2, 1000, &say).unwrap(); // found by its base name
    assert_eq!(std::fs::read(tmp.path("big")).unwrap(), stored);
    assert_eq!(std::fs::read(tmp.path("x")).unwrap(), packed);
    let mut left: Vec<String> = std::fs::read_dir(tmp.path("")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["a.zip", "big", "x"]); // no .part left
    z.get("big.dmg.aea", &tmp.path("big"), 8, CHUNK, &say).unwrap();
    assert_eq!(log.lock().unwrap().last().unwrap(), "big.dmg.aea: already complete");
    assert_eq!(z.head("big.dmg.aea", 16).unwrap(), &stored[..16]);
    assert_eq!(z.read("x.trustcache").unwrap(), packed);
    assert_eq!(z.find("missing").unwrap_err(), "missing: not in the archive");
    assert_eq!(z.head("x.trustcache", 4).unwrap_err(), "Firmware/x.trustcache: not stored, cannot read its first bytes alone");
}

#[test]
fn resume() {
    let tmp = Tmp::new("remotezip-resume");
    let (zip, stored, _) = make(&tmp);
    let z = RemoteZip::open(&zip).unwrap();
    let (crc, chunk) = (z.find("big.dmg.aea").unwrap().crc, 4096usize);
    // an interrupted run: chunks 0 and 2 done, the rest zero
    let mut part = vec![0u8; stored.len()];
    for k in [0, 2] {
        part[k * chunk..(k + 1) * chunk].copy_from_slice(&stored[k * chunk..(k + 1) * chunk]);
    }
    tmp.write("big.part", &part);
    tmp.write("big.part.done", format!("{crc:08x} {} {chunk}\n0\n2\n", stored.len()).as_bytes());
    let log = Mutex::new(Vec::new());
    z.get("big.dmg.aea", &tmp.path("big"), 1, chunk as u64, &|line| log.lock().unwrap().push(line)).unwrap();
    let fetched = log.lock().unwrap().iter().filter(|line| line.contains('%')).count();
    assert_eq!(fetched, stored.len().div_ceil(chunk) - 2);
    assert_eq!(std::fs::read(tmp.path("big")).unwrap(), stored);
}

#[test]
fn bad_chunk_is_caught() {
    let tmp = Tmp::new("remotezip-bad");
    let (zip, stored, _) = make(&tmp);
    let z = RemoteZip::open(&zip).unwrap();
    let crc = z.find("big.dmg.aea").unwrap().crc;
    // chunk 0 claimed done but holds zeros
    tmp.write("big.part", &vec![0u8; stored.len()]);
    tmp.write("big.part.done", format!("{crc:08x} {} 4096\n0\n", stored.len()).as_bytes());
    let error = z.get("big.dmg.aea", &tmp.path("big"), 8, 4096, &|_| ()).unwrap_err();
    assert!(error.contains("CRC-32") && error.ends_with("chunk list reset, run again"), "{error}");
    assert!(!std::path::Path::new(&tmp.path("big")).exists());
    assert!(!std::path::Path::new(&tmp.path("big.part.done")).exists());
}
