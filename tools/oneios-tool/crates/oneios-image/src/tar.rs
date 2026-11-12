//! The headers Python's `tarfile` writes in `PAX_FORMAT` (`TarInfo.create_pax_header`), byte for byte: a ustar
//! header, with a pax extended header in front of it when a name is not ASCII or longer than 100 bytes, a number
//! does not fit its octal field, or the entry carries its own pax records (the seed's xattrs).
//!
//! The 512-byte block is the `tar` crate's `Header` (`new_ustar`, `set_mode`, `set_uid`, `set_gid`, `set_size`,
//! `set_mtime`, `set_entry_type`: the same zero-padded octal digits and NUL that Python writes). Four things the
//! crate does differently from Python are done here instead, because `data.seed`'s SHA-256 is pinned in the
//! image's trust list (measured with tar 0.4.46 against Python 3's tarfile, 2026-11-12):
//! - the checksum field: the crate's `set_cksum` writes seven octal digits and a NUL (`0010715\0`), Python six
//!   digits, a NUL and a space (`010715\0 `), in every header;
//! - names: the crate's `set_path` moves the front of a long name into the ustar `prefix` field and refuses what
//!   does not fit; Python's pax format never uses `prefix`: the name field gets the first 100 characters with
//!   `?` for each one that is not ASCII, and the whole name goes into a `path` record;
//! - the extended header's own block: `Builder::append_pax_extensions` leaves its name empty and its mode, uid
//!   and gid fields all NUL; Python names it `././@PaxHeader` and writes `0000000\0` in the three fields. (The
//!   records' text, `LENGTH KEY=VALUE\n`, is the same in both.)
//! - the end: `Builder::finish` writes two zero blocks; Python fills up to the next multiple of 10240 bytes.
//! So `Builder` is not used; the seed's members are also written at their offsets in parallel (`seed.rs`), which
//! a `Builder` over one writer cannot do.

use ::tar::{EntryType, Header};

pub const BLOCK: usize = 512;
/// `tarfile.RECORDSIZE`: the archive's length is a multiple of this.
pub const RECORD: u64 = 10240;

pub const REG: u8 = b'0';
pub const SYM: u8 = b'2';
pub const DIR: u8 = b'5';
const XHD: u8 = b'x';

pub struct TarInfo<'a> {
    pub name: &'a str,
    pub mode: i128,
    pub uid: i128,
    pub gid: i128,
    pub size: i128,
    pub mtime: i128,
    pub kind: u8,
    pub linkname: &'a str,
    /// `TarInfo.pax_headers`, in the dict's order.
    pub pax: Vec<(String, String)>,
}

/// One pax record, "LENGTH KEYWORD=VALUE\n", where LENGTH counts its own digits.
fn record(out: &mut Vec<u8>, keyword: &str, value: &str) {
    let l = keyword.len() + value.len() + 3;
    let mut p = 0usize;
    loop {
        let n = l + p.to_string().len();
        if n == p {
            break;
        }
        p = n;
    }
    out.extend_from_slice(p.to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(keyword.as_bytes());
    out.push(b'=');
    out.extend_from_slice(value.as_bytes());
    out.push(b'\n');
}

impl<'a> TarInfo<'a> {
    /// `TarInfo(name)`: a regular file, mode 0o644, everything else zero or empty.
    pub fn new(name: &'a str) -> TarInfo<'a> {
        TarInfo { name, mode: 0o644, uid: 0, gid: 0, size: 0, mtime: 0, kind: REG, linkname: "", pax: Vec::new() }
    }

    /// `TarInfo.tobuf(PAX_FORMAT)`, as a new buffer.
    #[cfg(test)]
    pub fn header(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(3 * BLOCK);
        self.write_header(&mut out);
        out
    }

    /// `TarInfo.tobuf(PAX_FORMAT)`, appended to `out`: one to three blocks, more with long records.
    pub fn write_header(&self, out: &mut Vec<u8>) {
        let dir_name;
        let name = if self.kind == DIR && !self.name.ends_with('/') {
            dir_name = format!("{}/", self.name);
            dir_name.as_str()
        } else {
            self.name
        };
        let has = |key: &str| self.pax.iter().any(|(k, _)| k == key);
        // The extended header's block comes first, its records after it: keep its place and fill it in at the end.
        let start = out.len();
        out.resize(start + BLOCK, 0);
        for (keyword, value) in &self.pax {
            record(out, keyword, value);
        }
        // String fields that are too long or not ASCII. (uname and gname are always empty here.)
        for (value, hname) in [(name, "path"), (self.linkname, "linkpath")] {
            if !has(hname) && (!value.is_ascii() || value.chars().count() > 100) {
                record(out, hname, value);
            }
        }
        // Number fields that do not fit their octal digits.
        let mut numbers = [("uid", 8u32, self.uid), ("gid", 8, self.gid), ("size", 12, self.size), ("mtime", 12, self.mtime)];
        for (key, digits, value) in numbers.iter_mut() {
            if !(0..8i128.pow(*digits - 1)).contains(value) {
                if !has(key) {
                    record(out, key, &value.to_string());
                }
                *value = 0;
            }
        }
        let records = out.len() - start - BLOCK;
        if records == 0 {
            out.truncate(start);
        } else {
            let block = ustar("././@PaxHeader", 0, 0, 0, records as u64, 0, XHD, "");
            out[start..start + BLOCK].copy_from_slice(&block);
            out.resize(out.len().next_multiple_of(BLOCK), 0);
        }
        // every number is inside its field's range here, so the casts keep the value
        let block = ustar(name, (self.mode & 0o7777) as u32, numbers[0].2 as u64, numbers[1].2 as u64, numbers[2].2 as u64, numbers[3].2 as u64, self.kind, self.linkname);
        out.extend_from_slice(&block);
    }
}

/// `stn(s, length, "ascii", "replace")`: every character that is not ASCII becomes `?`, the rest is cut; the
/// field is NUL already.
fn text(field: &mut [u8], s: &str) {
    for (slot, c) in field.iter_mut().zip(s.chars()) {
        *slot = if c.is_ascii() { c as u8 } else { b'?' };
    }
}

/// `TarInfo._create_header(info, USTAR_FORMAT, "ascii", "replace")`: the crate's ustar header, field by field.
#[allow(clippy::too_many_arguments)]
fn ustar(name: &str, mode: u32, uid: u64, gid: u64, size: u64, mtime: u64, kind: u8, linkname: &str) -> [u8; BLOCK] {
    let mut h = Header::new_ustar();
    {
        let fields = h.as_ustar_mut().expect("new_ustar makes a ustar header");
        text(&mut fields.name, name);
        text(&mut fields.linkname, linkname);
    }
    h.set_mode(mode);
    h.set_uid(uid);
    h.set_gid(gid);
    h.set_size(size);
    h.set_mtime(mtime);
    h.set_entry_type(EntryType::new(kind));
    // Python's checksum field: six octal digits, a NUL, a space (the module comment has the crate's form).
    let bytes = h.as_mut_bytes();
    bytes[148..156].fill(b' ');
    let sum: u32 = bytes.iter().map(|b| *b as u32).sum();
    bytes[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    *bytes
}

/// The archive's length when its last member ends at `offset`: two zero blocks more, then up to the next
/// multiple of RECORD.
pub fn end(offset: u64) -> u64 {
    (offset + 2 * BLOCK as u64).next_multiple_of(RECORD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_header() {
        let mut info = TarInfo::new("mobile/Library");
        info.kind = DIR;
        info.mode = 0o40755; // only the permission bits are kept
        info.uid = 501;
        info.gid = 501;
        info.mtime = 1600000000;
        let h = info.header();
        assert_eq!(h.len(), 512);
        assert_eq!(&h[..15], b"mobile/Library/");
        assert_eq!(&h[100..108], b"0000755\0");
        assert_eq!(&h[108..116], b"0000765\0");
        assert_eq!(&h[116..124], b"0000765\0");
        assert_eq!(&h[124..136], b"00000000000\0");
        assert_eq!(&h[136..148], b"13727410000\0");
        assert_eq!(&h[148..156], b"010715\0 "); // as Python's tarfile writes this header
        assert_eq!(h[156], b'5');
        assert_eq!(&h[257..265], b"ustar\x0000");
        assert!(h[265..].iter().all(|b| *b == 0));
        // the checksum: six octal digits, a NUL, and the space that was there
        let mut blank = h.clone();
        blank[148..156].copy_from_slice(b"        ");
        let sum: u32 = blank.iter().map(|b| *b as u32).sum();
        assert_eq!(&h[148..156], format!("{sum:06o}\0 ").as_bytes());
    }

    #[test]
    fn pax_records() {
        let mut info = TarInfo::new("a.plist");
        info.pax = vec![("LIBARCHIVE.xattr.com.example.tag".into(), "AAEC".into())];
        let h = info.header();
        assert_eq!(h.len(), 3 * 512);
        assert_eq!(&h[..14], b"././@PaxHeader");
        assert_eq!(h[156], b'x');
        let record = b"41 LIBARCHIVE.xattr.com.example.tag=AAEC\n";
        assert_eq!(record.len(), 41);
        assert_eq!(&h[124..136], b"00000000051\0");
        assert_eq!(&h[512..512 + 41], record);
        assert!(h[512 + 41..1024].iter().all(|b| *b == 0));
        assert_eq!(&h[1024..1031], b"a.plist");

        // a length that gains a digit by counting itself: 98 bytes without the number, 101 with it
        let mut info = TarInfo::new("x");
        info.pax = vec![("k".into(), "v".repeat(94))];
        let h = info.header();
        assert_eq!(&h[512..518], b"101 k=");
    }

    #[test]
    fn long_and_unicode_names_and_big_numbers() {
        let long = "d/".repeat(60) + "file";
        let mut info = TarInfo::new(&long);
        info.size = 10_359_930_880; // more than eleven octal digits hold
        info.uid = 5_000_000;
        info.mtime = -5;
        let h = info.header();
        let records = format!("134 path={long}\n15 uid=5000000\n20 size=10359930880\n12 mtime=-5\n");
        assert_eq!(&h[512..512 + records.len()], records.as_bytes());
        let at = 512 + records.len().next_multiple_of(512);
        assert_eq!(&h[at..at + 100], &long.as_bytes()[..100]);
        assert_eq!(&h[at + 108..at + 116], b"0000000\0");
        assert_eq!(&h[at + 124..at + 136], b"00000000000\0");
        assert_eq!(&h[at + 136..at + 148], b"00000000000\0");

        let name = format!("caf{}", '\u{e9}');
        let target = format!("{}t", '\u{1f600}');
        let mut info = TarInfo::new(&name);
        info.kind = SYM;
        info.linkname = &target;
        let h = info.header();
        let records = format!("14 path={name}\n18 linkpath={target}\n");
        assert_eq!(&h[512..512 + records.len()], records.as_bytes());
        assert_eq!(&h[1024..1029], b"caf?\0");
        assert_eq!(&h[1024 + 157..1024 + 160], b"?t\0");
    }

    #[test]
    fn trailer_fills_the_record() {
        assert_eq!(end(0), 10240);
        assert_eq!(end(512), 10240);
        assert_eq!(end(10240 - 1024), 10240);
        assert_eq!(end(10240 - 512), 2 * 10240);
        assert_eq!(end(10240), 2 * 10240);
    }
}
