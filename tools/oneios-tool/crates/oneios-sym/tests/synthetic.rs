//! oneios-tool sym on a tiny dyld shared cache made here (no Apple bytes): a main file, one subcache and a
//! `.symbols` file with two images. The expected lines are what `scripts/debug/dsc.py` prints for such a cache
//! (worked out by hand from its code); the index and the direct path must agree on every address.

use oneios_sym::{cache::is_call, parse_hex, run_with, Cache, Index, Locate};
use std::path::{Path, PathBuf};

const BASE: u64 = 0x1_8000_0000;
const A: u64 = BASE + 0x1000; // image A: its header and __TEXT [A, A + 0x1000) in the main file
const B: u64 = BASE + 0x10000; // image B: __TEXT [B, B + 0x1000) in the subcache
const LINKEDIT: u64 = BASE + 0x12000; // file offset 0x3000 of the subcache
const SLIDE: u64 = 0x8000;
const NOP: u32 = 0xd503_201f;
const BL: u32 = 0x9400_0010;
const BLR: u32 = 0xd63f_0100;
const BLRAAZ: u32 = 0xd63f_087f;

fn put(buf: &mut Vec<u8>, at: usize, bytes: &[u8]) {
    if buf.len() < at + bytes.len() {
        buf.resize(at + bytes.len(), 0);
    }
    buf[at..at + bytes.len()].copy_from_slice(bytes);
}

fn put32(buf: &mut Vec<u8>, at: usize, v: u32) {
    put(buf, at, &v.to_le_bytes());
}

fn put64(buf: &mut Vec<u8>, at: usize, v: u64) {
    put(buf, at, &v.to_le_bytes());
}

/// A cache file's header: one mapping (addr, size, file offset) at 0x200.
fn header(buf: &mut Vec<u8>, uuid: u8, addr: u64, size: u64, foff: u64) {
    put(buf, 0, b"dyld_v1  arm64e\0");
    put32(buf, 0x10, 0x200);
    put32(buf, 0x14, 1);
    put(buf, 0x58, &[uuid; 16]);
    put64(buf, 0x200, addr);
    put64(buf, 0x208, size);
    put64(buf, 0x210, foff);
}

fn segment(name: &str, vmaddr: u64, vmsize: u64, fileoff: u64) -> Vec<u8> {
    let mut c = vec![0u8; 72];
    put32(&mut c, 0, 0x19);
    put32(&mut c, 4, 72);
    put(&mut c, 8, name.as_bytes());
    put64(&mut c, 24, vmaddr);
    put64(&mut c, 32, vmsize);
    put64(&mut c, 40, fileoff);
    put64(&mut c, 48, vmsize);
    c
}

fn symtab(symoff: u32, nsyms: u32, stroff: u32) -> Vec<u8> {
    let mut c = vec![0u8; 24];
    put32(&mut c, 0, 0x2);
    put32(&mut c, 4, 24);
    put32(&mut c, 8, symoff);
    put32(&mut c, 12, nsyms);
    put32(&mut c, 16, stroff);
    c
}

/// A Mach-O header with its load commands, at `at` of `buf`.
fn macho(buf: &mut Vec<u8>, at: usize, commands: &[Vec<u8>]) {
    put32(buf, at, 0xfeed_facf);
    put32(buf, at + 16, commands.len() as u32);
    let mut off = at + 32;
    for c in commands {
        put(buf, off, c);
        off += c.len();
    }
}

/// nlist_64 entries and their strings: (value, type, name).
fn symbols(buf: &mut Vec<u8>, symoff: usize, stroff: usize, list: &[(u64, u8, &[u8])]) {
    let mut strx = 1usize;
    for (i, (value, kind, name)) in list.iter().enumerate() {
        put32(buf, symoff + 16 * i, strx as u32);
        put(buf, symoff + 16 * i + 4, &[*kind]);
        put64(buf, symoff + 16 * i + 8, *value);
        put(buf, stroff + strx, name);
        put(buf, stroff + strx + name.len(), &[0]);
        strx += name.len() + 1;
    }
}

fn build_cache(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    // The main file: maps [BASE, BASE + 0x8000) from offset 0.
    let mut main = vec![0u8; 0x8000];
    header(&mut main, 0xa1, BASE, 0x8000, 0);
    put64(&mut main, 0xE0, BASE); // sharedRegionStart
    put64(&mut main, 0xE8, 0x14000); // sharedRegionSize
    put64(&mut main, 0xF0, 0x10000); // maxSlide: 4 candidates
    put32(&mut main, 0x188, 0x300); // subcaches
    put32(&mut main, 0x18C, 1);
    put(&mut main, 0x300 + 24, b".01\0");
    put32(&mut main, 0x1C0, 0x400); // images
    put32(&mut main, 0x1C4, 2);
    let paths: [(u64, &[u8], usize); 2] =
        [(A, b"/usr/lib/libalpha.dylib\0", 0x500), (B, b"/System/Library/Frameworks/Beta.framework/Beta\0", 0x540)];
    for (i, (addr, path, off)) in paths.iter().enumerate() {
        put64(&mut main, 0x400 + 32 * i, *addr);
        put32(&mut main, 0x400 + 32 * i + 24, *off as u32);
        put(&mut main, *off, path);
    }
    // Image A: header at A, then its text. Its symbol table is in the subcache's linkedit.
    for at in (0x1100..0x2000).step_by(4) {
        put32(&mut main, at, NOP);
    }
    macho(
        &mut main,
        0x1000,
        &[segment("__TEXT", A, 0x1000, 0x1000), segment("__LINKEDIT", LINKEDIT, 0x2000, 0x3000), symtab(0x3000, 8, 0x3400)],
    );
    put32(&mut main, 0x1104, BL);
    put32(&mut main, 0x1204, BL);
    put32(&mut main, 0x1404, BL);
    put32(&mut main, 0x1504, BLR);
    // An unaligned call: the bytes at A + 0x701 .. A + 0x705 read as a bl.
    put32(&mut main, 0x1700, 0);
    put32(&mut main, 0x1704, 0x0000_0094);
    // A second place, 0x4000 above the first call, that makes one address fit two slides.
    put32(&mut main, 0x5104, BL);
    std::fs::write(dir.join("dyld_shared_cache_arm64e"), &main).unwrap();

    // The subcache: maps [BASE + 0x10000, BASE + 0x14000) from offset 0x1000.
    let mut sub = vec![0u8; 0x5000];
    header(&mut sub, 0xb2, B, 0x4000, 0x1000);
    for at in (0x1100..0x2000).step_by(4) {
        put32(&mut sub, at, NOP);
    }
    macho(
        &mut sub,
        0x1000,
        &[segment("__TEXT", B, 0x1000, 0x1000), segment("__LINKEDIT", LINKEDIT, 0x2000, 0x3000), symtab(0x3800, 2, 0x3c00)],
    );
    put32(&mut sub, 0x1804, BLRAAZ);
    // Image A's own symbols: file offsets of the file that maps its __LINKEDIT (this one).
    symbols(
        &mut sub,
        0x3000,
        0x3400,
        &[
            (A + 0x100, 0x0f, b"_alpha_start"),
            (A + 0x200, 0x0f, b"_b_same"),
            (A + 0x200, 0x0f, b"_a_same"),
            (A + 0x300, 0x01, b"_an_import"), // not defined in a section: skipped
            (0, 0x0f, b"_zero"),              // value 0: skipped
            (A + 0x500, 0x0f, b"_x2"),
            (A + 0x500, 0x0f, b"_x10"),
            (A + 0x600, 0x0f, b"_bad\xff"),
        ],
    );
    symbols(&mut sub, 0x3800, 0x3c00, &[(B + 0x800, 0x0f, b"_beta_fn"), (B + 0x2000, 0x0f, b"_beta_data")]);
    std::fs::write(dir.join("dyld_shared_cache_arm64e.01"), &sub).unwrap();

    // The .symbols file: the local symbols of image A (dylib offset A - BASE) at 0x1000.
    let mut sym = vec![0u8; 0x2000];
    header(&mut sym, 0xc3, BASE + 0x20000, 0x1000, 0);
    put64(&mut sym, 0x48, 0x1000);
    let info = 0x1000;
    put32(&mut sym, info, 0x100); // nlist offset
    put32(&mut sym, info + 4, 3);
    put32(&mut sym, info + 8, 0x200); // strings offset
    put32(&mut sym, info + 12, 0x100);
    put32(&mut sym, info + 16, 0x40); // entries offset
    put32(&mut sym, info + 20, 2);
    put64(&mut sym, info + 0x40, 0x9999); // another dylib
    put32(&mut sym, info + 0x48, 0);
    put32(&mut sym, info + 0x4c, 1);
    put64(&mut sym, info + 0x50, A - BASE);
    put32(&mut sym, info + 0x58, 1);
    put32(&mut sym, info + 0x5c, 2);
    symbols(
        &mut sym,
        info + 0x100,
        info + 0x200,
        &[(A + 0x900, 0x0e, b"_not_mine"), (A + 0x400, 0x0e, b"_local_fn"), (A + 0x200, 0x0e, b"_z_local_same")],
    );
    std::fs::write(dir.join("dyld_shared_cache_arm64e.symbols"), &sym).unwrap();
}

struct Fixture {
    dsc: PathBuf,
    index: PathBuf,
}

fn fixture(name: &str) -> Fixture {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("sym-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let f = Fixture { dsc: root.join("dsc"), index: root.join("index") };
    build_cache(&f.dsc);
    f
}

impl Fixture {
    fn loc(&self) -> Locate {
        Locate { dsc: Some(self.dsc.clone()), index_parent: Some(self.index.clone()) }
    }

    /// (status, stdout, stderr)
    fn run_in(&self, loc: Locate, args: &[&str], input: &str) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let status = run_with(&args, loc, &mut input.as_bytes(), &mut out, &mut err);
        (status, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        self.run_in(self.loc(), args, "")
    }

    /// The index's answer, checked against the direct path's.
    fn both(&self, args: &[&str]) -> String {
        let (status, out, _) = self.run(args);
        let mut direct = vec!["--no-index"];
        direct.extend_from_slice(args);
        let (direct_status, direct_out, _) = self.run(&direct);
        assert_eq!(out, direct_out, "the index and the direct path differ for {args:?}");
        assert_eq!((status, direct_status), (0, 0));
        out
    }
}

fn hex(v: u64) -> String {
    format!("{v:#x}")
}

#[test]
fn call_instructions() {
    for word in [BL, 0x97ff_ffff, BLR, 0xd63f_0000, BLRAAZ, 0xd73f_0a11] {
        assert!(is_call(word), "{word:#x}");
    }
    for word in [NOP, 0, 0x1400_0000 /* b */, 0xd61f_0100 /* br */, 0xd65f_03c0 /* ret */, 0x9000_0000] {
        assert!(!is_call(word), "{word:#x}");
    }
}

#[test]
fn hex_arguments_as_python() {
    assert_eq!(parse_hex("0x1f"), Some(0x1f));
    assert_eq!(parse_hex("1F"), Some(0x1f));
    assert_eq!(parse_hex(" 0X1f "), Some(0x1f));
    assert_eq!(parse_hex("-0x10"), Some(-16));
    assert_eq!(parse_hex("+10"), Some(16));
    assert_eq!(parse_hex("0x1_000"), Some(0x1000));
    assert_eq!(parse_hex("0x_1"), Some(1));
    for bad in ["", "0x", "x1", "1__0", "_1", "1_", "0x1g", "-", "12 34"] {
        assert_eq!(parse_hex(bad), None, "{bad:?}");
    }
}

#[test]
fn a_backtrace_finds_its_slide_and_names() {
    let f = fixture("backtrace");
    let returns = [A + 0x108, A + 0x208, A + 0x408, A + 0x508, B + 0x808];
    let mut args: Vec<String> = returns.iter().map(|a| hex(a + SLIDE)).collect();
    args.push(hex(A + 0x308 + SLIDE)); // not after a call; the import at A + 0x300 is not a symbol
    args.push(hex(A + 0x608 + SLIDE)); // a name that is not UTF-8
    args.push(hex(B + 0x100 + SLIDE)); // before the image's first symbol
    args.push(hex(A + 0x50 + SLIDE)); // image A, before its first symbol
    args.push("0x1000".to_string()); // outside the cache
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let replacement = char::from_u32(0xFFFD).unwrap();
    let expected = format!(
        "slide 0x8000 (5 of 9 addresses follow a call)\n\
         0x180009108  libalpha.dylib  _alpha_start + 0x8\n\
         0x180009208  libalpha.dylib  _z_local_same + 0x8\n\
         0x180009408  libalpha.dylib  _local_fn + 0x8\n\
         0x180009508  libalpha.dylib  _x2 + 0x8\n\
         0x180018808  Beta  _beta_fn + 0x8\n\
         0x180009308  libalpha.dylib  _z_local_same + 0x108\n\
         0x180009608  libalpha.dylib  _bad{replacement} + 0x8\n\
         0x180018100  Beta  ?\n\
         0x180009050  libalpha.dylib  ?\n\
         0x1000  (not in a cache image; unslid -0x7000)\n"
    );
    assert_eq!(f.both(&args), expected);

    // The same with the slide given: no slide line.
    let mut with_slide = vec!["--slide", "0x8000"];
    with_slide.extend_from_slice(&args);
    assert_eq!(f.both(&with_slide), expected.split_once('\n').unwrap().1);

    // The library's calls.
    let index = oneios_sym::open_index(&f.loc(), &mut Vec::new()).unwrap();
    let slid: Vec<u64> = returns.iter().map(|a| a + SLIDE).collect();
    let found = index.find_slide(&slid);
    assert_eq!((found.slide, found.score, found.inside, found.ties), (0x8000, 5, 5, 1));
    let name = index.name(A + 0x208).unwrap();
    assert_eq!((name.image, name.symbol), ("libalpha.dylib", Some(("_z_local_same", 8))));
    assert_eq!(index.name(B + 0x100).unwrap().symbol, None);
    assert!(index.name(B + 0x1000).is_none());
    assert!(index.name(BASE).is_none());
}

#[test]
fn few_addresses_tie() {
    let f = fixture("tie");
    // One address: the calls at A + 0x104 (slide 0x8000) and 0x4000 above it (slide 0x4000) both fit.
    let out = f.both(&[&hex(A + 0x108 + SLIDE)]);
    assert_eq!(
        out,
        "slide 0x4000 (1 of 1 addresses follow a call; 2 slides fit as well: NOT reliable, give a whole backtrace (8 or more return addresses) or --slide)\n\
         0x180009108  (not in a cache image; unslid 0x180005108)\n"
    );
    // No address inside: every candidate has 0.
    let out = f.both(&["0x10", "-0x20"]);
    assert_eq!(
        out,
        "slide 0x0 (0 of 0 addresses follow a call; 4 slides fit as well: NOT reliable, give a whole backtrace (8 or more return addresses) or --slide)\n\
         0x10  (not in a cache image; unslid 0x10)\n\
         -0x20  (not in a cache image; unslid -0x20)\n"
    );
}

#[test]
fn an_unaligned_address_reads_the_cache() {
    let f = fixture("unaligned");
    // The four bytes before A + 0x705 read as a bl: dsc.py counts it.
    let odd = hex(A + 0x705 + SLIDE);
    let out = f.both(&[&odd, &hex(A + 0x108 + SLIDE), &hex(A + 0x208 + SLIDE)]);
    assert!(out.starts_with("slide 0x8000 (3 of 3 addresses follow a call)\n"), "{out}");
    // Without the cache the index cannot count it, and says so.
    let no_cache = Locate { dsc: None, index_parent: Some(f.index.clone()) };
    let (status, out, err) = f.run_in(no_cache, &[&odd, &hex(A + 0x108 + SLIDE), &hex(A + 0x208 + SLIDE)], "");
    assert_eq!(status, 0);
    assert!(out.starts_with("slide 0x8000 (2 of 3 addresses follow a call)\n"), "{out}");
    assert!(err.contains("not multiples of 4"), "{err}");
}

#[test]
fn every_address_names_the_same_from_the_index_and_the_cache() {
    let f = fixture("every");
    // Every 4th byte of both mappings and some around them, unslid.
    let mut addrs: Vec<String> = Vec::new();
    for a in (BASE - 0x40..BASE + 0x8040).step_by(12).chain((B - 0x40..B + 0x4040).step_by(12)) {
        addrs.push(hex(a));
    }
    let mut args = vec!["--slide", "0"];
    args.extend(addrs.iter().map(String::as_str));
    let out = f.both(&args);
    assert_eq!(out.lines().count(), addrs.len());
    assert!(out.contains("libalpha.dylib  _local_fn + 0x"));
    // And the slide search over every word as a return address (most are not after a call).
    let slid: Vec<String> = (A..A + 0x1000).step_by(4).map(|a| hex(a + SLIDE)).collect();
    let args: Vec<&str> = slid.iter().map(String::as_str).collect();
    let out = f.both(&args);
    assert!(out.starts_with("slide 0x8000 (4 of 1024 addresses follow a call)\n"), "{}", out.lines().next().unwrap());
}

#[test]
fn the_index_is_built_once_and_found_again() {
    let f = fixture("index");
    let uuid = "a1".repeat(16);
    // The first use builds it and says so.
    let (status, out, err) = f.run(&["--slide", "0", &hex(A + 0x108)]);
    assert_eq!((status, out.as_str()), (0, "0x180001108  libalpha.dylib  _alpha_start + 0x8\n"));
    assert!(err.contains("building it (once)"), "{err}");
    assert!(Index::is_current(&f.index.join(&uuid)));
    // The second does not.
    let (_, _, err) = f.run(&["--slide", "0", &hex(A + 0x108)]);
    assert_eq!(err, "");
    // Without the cache: the index built last.
    let no_cache = Locate { dsc: None, index_parent: Some(f.index.clone()) };
    let (status, out, _) = f.run_in(no_cache.clone(), &["--slide", "0", "--stdin", &hex(A + 0x108)], "0x180001208 0x180001408\n0x10\n");
    assert_eq!(status, 0);
    assert_eq!(
        out,
        "0x180001108  libalpha.dylib  _alpha_start + 0x8\n\
         0x180001208  libalpha.dylib  _z_local_same + 0x8\n\
         0x180001408  libalpha.dylib  _local_fn + 0x8\n\
         0x10  (not in a cache image; unslid 0x10)\n"
    );
    // A cache folder that is not there (not mounted now): the index built last, with a note.
    let gone = Locate { dsc: Some(f.dsc.join("gone")), index_parent: Some(f.index.clone()) };
    let (status, out, err) = f.run_in(gone, &["--slide", "0", &hex(A + 0x108)], "");
    assert_eq!((status, out.as_str()), (0, "0x180001108  libalpha.dylib  _alpha_start + 0x8\n"));
    assert!(err.contains("cannot be read: using the index built last"), "{err}");
    // `index` reports the one that is there; --force builds it again.
    let (status, out, _) = f.run(&["index"]);
    assert!(status == 0 && out.contains("is there"), "{out}");
    let (status, out, _) = f.run(&["index", "--force"]);
    assert!(status == 0 && out.contains("built in") && out.contains("2 images"), "{out}");
    // What the index keeps of image A: one name per value, below the end of its __TEXT.
    let index = Index::open(&f.index.join(&uuid)).unwrap();
    assert_eq!(index.image_count(), 2);
    assert_eq!(index.symbol_count(), 5 + 1); // A: 0x100 0x200 0x400 0x500 0x600; B: 0x800 (0x2000 is past its __TEXT)
    // Errors: a bad address, no cache and no index.
    let (status, _, err) = f.run(&["0x12z"]);
    assert!(status == 1 && err.contains("invalid hex address"), "{err}");
    let nothing = Locate { dsc: None, index_parent: Some(f.index.join("none")) };
    let (status, _, err) = f.run_in(nothing, &["0x10"], "");
    assert!(status == 1 && err.contains("ONEIOS_DSC_DIR"), "{err}");
    // The cache as dsc.py's class reads it.
    let cache = Cache::open(&f.dsc).unwrap();
    assert_eq!((cache.base, cache.size, cache.max_slide, cache.images.len(), cache.maps.len()), (BASE, 0x14000, 0x10000, 2, 2));
    let syms = cache.symbols(0).unwrap();
    let names: Vec<&str> = syms.iter().filter(|s| s.0 == A + 0x200).map(|s| s.1.as_ref()).collect();
    assert_eq!(names, ["_a_same", "_b_same", "_z_local_same"]);
}
