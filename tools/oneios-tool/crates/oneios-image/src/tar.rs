//! The headers Python's `tarfile` writes in `PAX_FORMAT` (`TarInfo.create_pax_header`), byte for byte: a ustar
//! header, with a pax extended header in front of it when a name is not ASCII or longer than 100 bytes, a number
//! does not fit its octal field, or the entry carries its own pax records (the seed's xattrs).

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

impl<'a> TarInfo<'a> {
    /// `TarInfo(name)`: a regular file, mode 0o644, everything else zero or empty.
    pub fn new(name: &'a str) -> TarInfo<'a> {
        TarInfo { name, mode: 0o644, uid: 0, gid: 0, size: 0, mtime: 0, kind: REG, linkname: "", pax: Vec::new() }
    }

    /// `TarInfo.tobuf(PAX_FORMAT)`.
    pub fn header(&self) -> Vec<u8> {
        let mut name = self.name.to_string();
        if self.kind == DIR && !name.ends_with('/') {
            name.push('/');
        }
        let mut pax = self.pax.clone();
        let has = |pax: &Vec<(String, String)>, key: &str| pax.iter().any(|(k, _)| k == key);
        // String fields that are too long or not ASCII. (uname and gname are always empty here.)
        for (value, hname) in [(name.as_str(), "path"), (self.linkname, "linkpath")] {
            if has(&pax, hname) {
                continue;
            }
            if !value.is_ascii() || value.chars().count() > 100 {
                pax.push((hname.to_string(), value.to_string()));
            }
        }
        // Number fields that do not fit their octal digits.
        let mut numbers = [("uid", 8u32, self.uid), ("gid", 8, self.gid), ("size", 12, self.size), ("mtime", 12, self.mtime)];
        for (key, digits, value) in numbers.iter_mut() {
            if !(0..8i128.pow(*digits - 1)).contains(value) {
                if !has(&pax, key) {
                    pax.push((key.to_string(), value.to_string()));
                }
                *value = 0;
            }
        }
        let mut out = Vec::with_capacity(3 * BLOCK);
        if !pax.is_empty() {
            let mut records = Vec::new();
            for (keyword, value) in &pax {
                // "LENGTH KEYWORD=VALUE\n", where LENGTH counts its own digits
                let l = keyword.len() + value.len() + 3;
                let mut p = 0usize;
                loop {
                    let n = l + p.to_string().len();
                    if n == p {
                        break;
                    }
                    p = n;
                }
                records.extend_from_slice(format!("{p} {keyword}={value}\n").as_bytes());
            }
            out.extend_from_slice(&ustar("././@PaxHeader", 0, 0, 0, records.len() as i128, 0, XHD, ""));
            out.extend_from_slice(&records);
            out.resize(out.len().next_multiple_of(BLOCK), 0);
        }
        out.extend_from_slice(&ustar(&name, self.mode & 0o7777, numbers[0].2, numbers[1].2, numbers[2].2, numbers[3].2, self.kind, self.linkname));
        out
    }
}

/// `stn(s, length, "ascii", "replace")`: every character that is not ASCII becomes `?`, the rest is cut or padded.
fn text(out: &mut Vec<u8>, s: &str, length: usize) {
    let start = out.len();
    out.extend(s.chars().take(length).map(|c| if c.is_ascii() { c as u8 } else { b'?' }));
    out.resize(start + length, 0);
}

/// `itn(n, digits)`: octal digits and a NUL.
fn number(out: &mut Vec<u8>, n: i128, digits: usize) {
    out.extend_from_slice(format!("{n:0width$o}\0", width = digits - 1).as_bytes());
}

/// `TarInfo._create_header(info, USTAR_FORMAT, "ascii", "replace")`.
#[allow(clippy::too_many_arguments)]
fn ustar(name: &str, mode: i128, uid: i128, gid: i128, size: i128, mtime: i128, kind: u8, linkname: &str) -> [u8; BLOCK] {
    let mut buf = Vec::with_capacity(BLOCK);
    text(&mut buf, name, 100);
    number(&mut buf, mode & 0o7777, 8);
    number(&mut buf, uid, 8);
    number(&mut buf, gid, 8);
    number(&mut buf, size, 12);
    number(&mut buf, mtime, 12);
    buf.extend_from_slice(b"        ");
    buf.push(kind);
    text(&mut buf, linkname, 100);
    buf.extend_from_slice(b"ustar\x0000");
    text(&mut buf, "", 32); // uname
    text(&mut buf, "", 32); // gname
    text(&mut buf, "", 8); // devmajor
    text(&mut buf, "", 8); // devminor
    text(&mut buf, "", 155); // prefix
    buf.resize(BLOCK, 0);
    let sum: u32 = buf.iter().map(|b| *b as u32).sum();
    buf[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    buf.try_into().expect("a 512-byte header")
}

/// The zero bytes after the last member: two blocks, then up to the next multiple of RECORD.
pub fn trailer(offset: u64) -> Vec<u8> {
    let end = offset + 2 * BLOCK as u64;
    vec![0u8; (2 * BLOCK as u64 + (RECORD - end % RECORD) % RECORD) as usize]
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
        assert_eq!(trailer(0).len(), 10240);
        assert_eq!(trailer(512).len(), 10240 - 512);
        assert_eq!(trailer(10240 - 1024).len(), 1024);
        assert_eq!(trailer(10240 - 512).len(), 512 + 10240);
        assert_eq!(trailer(10240).len(), 10240);
    }
}
