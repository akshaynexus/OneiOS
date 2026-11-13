//! `sym kernel` on files made here: a fileset-shaped Mach-O with two members whose segments were moved apart, the
//! "unprelinked kernel" it stands for, a prelink record, and a log. No Apple file, no DWARF (the real kernel's
//! DWARF is tests/ksym/compare.sh's part); the index's line tables are tested with frames given by hand.
//!
//! The addresses are the worked example's (the Mac build of 2026-11-12): `__TEXT` at 0xfffffff007184000 in the
//! collection and 0xfffffff007004000 in the kernel, `__TEXT_EXEC` at 0xfffffff00ab44000 and 0xfffffff0072d4000.

use oneios_ksym::dwarf::{FrameRec, Lines, Source, NONE};
use oneios_ksym::fileset::{self, Locator};
use oneios_ksym::index::{self, FileId, Index};
use oneios_ksym::{Cli, Locate};
use std::path::{Path, PathBuf};

const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_UUID: u32 = 0x1b;
const LC_FILESET_ENTRY: u32 = 0x8000_0035;
const MH_EXECUTE: u32 = 2;
const MH_FILESET: u32 = 0xc;

const KC_TEXT: u64 = 0xfffffff007184000;
const KC_EXEC: u64 = 0xfffffff00ab44000;
const KC_DATA: u64 = 0xfffffff00b6bc000;
const K_TEXT: u64 = 0xfffffff007004000;
const K_EXEC: u64 = 0xfffffff0072d4000;
const K_DATA: u64 = 0xfffffff007c94000;
const EXEC_SIZE: u64 = 0xa1_0000;
const KEXT_TEXT: u64 = 0xfffffff00711f140;
const KEXT_EXEC: u64 = 0xfffffff0084b1910;
const LINKEDIT: u64 = 0xfffffff00ba10000;
const SLIDE: u64 = 0x2ff9000000;

/// (name, address, size, instructions)
type Seg = (&'static str, u64, u64, bool);
/// (name, address, section number from 1, external)
type Sym = (&'static str, u64, u8, bool);

struct Macho {
    filetype: u32,
    segs: Vec<Seg>,
    syms: Vec<Sym>,
    /// (id, header address, header offset in the file)
    entries: Vec<(&'static str, u64, u64)>,
    uuid: [u8; 16],
    /// Debug-map entries after the symbols: (n_type, name, section, value).
    stabs: Vec<(u8, String, u8, u64)>,
}

fn fixed(name: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..name.len()].copy_from_slice(name.as_bytes());
    out
}

impl Macho {
    /// The header and the load commands; the symbol table is at `symoff`, its strings right after it.
    fn header(&self, symoff: u32) -> Vec<u8> {
        let mut cmds = Vec::new();
        let mut ncmds = 0u32;
        let mut put = |cmd: u32, body: &[u8]| {
            cmds.extend_from_slice(&cmd.to_le_bytes());
            cmds.extend_from_slice(&(body.len() as u32 + 8).to_le_bytes());
            cmds.extend_from_slice(body);
            ncmds += 1;
        };
        for &(name, addr, size, code) in &self.segs {
            let mut body = Vec::new();
            body.extend_from_slice(&fixed(name));
            for v in [addr, size, 0, 0] {
                body.extend_from_slice(&v.to_le_bytes());
            }
            for v in [7u32, 5, 1, 0] {
                body.extend_from_slice(&v.to_le_bytes());
            }
            // One section over the whole segment.
            body.extend_from_slice(&fixed("__s"));
            body.extend_from_slice(&fixed(name));
            body.extend_from_slice(&addr.to_le_bytes());
            body.extend_from_slice(&size.to_le_bytes());
            for v in [0u32, 0, 0, 0, if code { 0x8000_0400 } else { 0 }, 0, 0, 0] {
                body.extend_from_slice(&v.to_le_bytes());
            }
            put(LC_SEGMENT_64, &body);
        }
        for &(id, vmaddr, fileoff) in &self.entries {
            let mut body = Vec::new();
            body.extend_from_slice(&vmaddr.to_le_bytes());
            body.extend_from_slice(&fileoff.to_le_bytes());
            body.extend_from_slice(&32u32.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(id.as_bytes());
            body.push(0);
            while body.len() % 8 != 0 {
                body.push(0);
            }
            put(LC_FILESET_ENTRY, &body);
        }
        put(LC_UUID, &self.uuid);
        if !self.syms.is_empty() {
            let (table, strings) = self.tables();
            let mut body = Vec::new();
            for v in [symoff, (self.syms.len() + self.stabs.len()) as u32, symoff + table.len() as u32, strings.len() as u32] {
                body.extend_from_slice(&v.to_le_bytes());
            }
            put(LC_SYMTAB, &body);
        }
        let mut out = Vec::new();
        for v in [0xfeed_facfu32, 0x0100_000c, 2, self.filetype, ncmds, cmds.len() as u32, 0, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&cmds);
        out
    }

    fn tables(&self) -> (Vec<u8>, Vec<u8>) {
        let mut table = Vec::new();
        let mut strings = vec![0u8];
        for &(name, addr, sect, external) in &self.syms {
            table.extend_from_slice(&(strings.len() as u32).to_le_bytes());
            table.push(0x0e | external as u8);
            table.push(sect);
            table.extend_from_slice(&0u16.to_le_bytes());
            table.extend_from_slice(&addr.to_le_bytes());
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
        }
        for (n_type, name, sect, value) in &self.stabs {
            table.extend_from_slice(&(if name.is_empty() { 0 } else { strings.len() as u32 }).to_le_bytes());
            table.push(*n_type);
            table.push(*sect);
            table.extend_from_slice(&0u16.to_le_bytes());
            table.extend_from_slice(&value.to_le_bytes());
            if !name.is_empty() {
                strings.extend_from_slice(name.as_bytes());
                strings.push(0);
            }
        }
        (table, strings)
    }
}

fn place(file: &mut Vec<u8>, at: usize, bytes: &[u8]) {
    if file.len() < at + bytes.len() {
        file.resize(at + bytes.len(), 0);
    }
    file[at..at + bytes.len()].copy_from_slice(bytes);
}

/// The unprelinked kernel: `xnu.macho`.
fn kernel_file() -> Vec<u8> {
    kernel_file_with(Vec::new())
}

/// The unprelinked kernel with a debug map: `stabs`.
fn kernel_file_with(stabs: Vec<(u8, String, u8, u64)>) -> Vec<u8> {
    let image = Macho {
        filetype: MH_EXECUTE,
        segs: vec![("__TEXT", K_TEXT, 0x1ac000, false), ("__TEXT_EXEC", K_EXEC, EXEC_SIZE, true), ("__LAST", K_EXEC + EXEC_SIZE, 0, false), ("__DATA", K_DATA, 0x4000, false)],
        syms: vec![
            ("_version", K_TEXT + 0x100, 1, true),
            ("_tcp_input", K_EXEC + 0x715000, 2, true),
            ("_compute_iaj", K_EXEC + 0x71c000, 2, false),
            ("_LowVectorEnd", K_EXEC + 0x1000, 2, true),
            ("_start_first", K_EXEC + 0x1000, 2, true),
            ("__ZN9IOService5startEPS_", K_EXEC + 0x2000, 2, true),
            ("___ubsan_handle_mul_overflow_minimal", K_EXEC + 0xa08000, 2, false),
            // The end of the code: its section is __TEXT_EXEC's, its address the next segment's first one.
            ("_etext_marker", K_EXEC + EXEC_SIZE, 2, true),
            ("_last_kernel_symbol", K_EXEC + EXEC_SIZE, 3, true),
            ("_kernel_data", K_DATA + 0x10, 4, true),
        ],
        entries: vec![],
        uuid: *b"kernel-uuid-0001",
        stabs,
    };
    let mut file = image.header(0x1000);
    let (table, strings) = image.tables();
    place(&mut file, 0x1000, &table);
    place(&mut file, 0x1000 + table.len(), &strings);
    file
}

/// The collection: the kernel with every segment moved by its own amount, and a kext. `keep`: the kernel member
/// keeps its symbols (at the collection's addresses); `kext_symbols`: the kext keeps one too.
fn collection_file(keep: bool, kext_symbols: bool) -> Vec<u8> {
    let kernel = Macho {
        filetype: MH_EXECUTE,
        segs: vec![
            ("__TEXT", KC_TEXT, 0x1ac000, false),
            ("__TEXT_EXEC", KC_EXEC, EXEC_SIZE, true),
            ("__LAST", KC_EXEC + EXEC_SIZE + 0x8000, 0, false),
            ("__DATA", KC_DATA, 0x4000, false),
            ("__LINKEDIT", LINKEDIT, 0x10000, false),
        ],
        syms: if keep {
            vec![
                ("_version", KC_TEXT + 0x100, 1, true),
                ("_tcp_input", KC_EXEC + 0x715000, 2, true),
                ("_etext_marker", KC_EXEC + EXEC_SIZE, 2, true),
                ("_last_kernel_symbol", KC_EXEC + EXEC_SIZE + 0x8000, 3, true),
                ("_kernel_data", KC_DATA + 0x10, 4, true),
            ]
        } else {
            vec![]
        },
        entries: vec![],
        uuid: *b"kernel-uuid-0001",
        stabs: vec![],
    };
    let kext = Macho {
        filetype: 0xb,
        segs: vec![("__TEXT", KEXT_TEXT, 0x4000, false), ("__TEXT_EXEC", KEXT_EXEC, 0x1000, true), ("__LINKEDIT", LINKEDIT, 0x10000, false)],
        syms: if kext_symbols { vec![("__ZN7Example5probeEv", KEXT_EXEC + 0x40, 2, true)] } else { vec![] },
        entries: vec![],
        uuid: *b"kext-uuid-000001",
        stabs: vec![],
    };
    let top = Macho {
        filetype: MH_FILESET,
        segs: vec![
            ("__TEXT", 0xfffffff007004000, 0x8000, false),
            ("__PRELINK_TEXT", 0xfffffff00700c000, 0xdc8000, false),
            ("__TEXT_EXEC", 0xfffffff008498000, 0x3100000, true),
            ("__DATA", KC_DATA, 0x308000, false),
            ("__LINKEDIT", 0xfffffff00b9c4000, 0xc54000, false),
        ],
        syms: vec![],
        entries: vec![("com.apple.kernel", KC_TEXT, 0x1000), ("com.example.kext", KEXT_TEXT, 0x2000)],
        uuid: *b"collection-uuid1",
        stabs: vec![],
    };
    let mut file = top.header(0);
    place(&mut file, 0x1000, &kernel.header(0x3000));
    place(&mut file, 0x2000, &kext.header(0x4000));
    for (image, at) in [(&kernel, 0x3000), (&kext, 0x4000)] {
        let (table, strings) = image.tables();
        place(&mut file, at, &table);
        place(&mut file, at + table.len(), &strings);
    }
    file
}

struct Files {
    dir: PathBuf,
    kc: PathBuf,
    kernel: PathBuf,
}

impl Files {
    fn new(name: &str, keep: bool, kext_symbols: bool) -> Files {
        let dir = std::env::temp_dir().join(format!("oneios-ksym-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("kc")).unwrap();
        std::fs::create_dir_all(dir.join("kernel")).unwrap();
        let files = Files { kc: dir.join("kc/kernel.kc"), kernel: dir.join("kernel/xnu.macho"), dir };
        std::fs::write(&files.kc, collection_file(keep, kext_symbols)).unwrap();
        std::fs::write(&files.kernel, kernel_file()).unwrap();
        files
    }

    fn locate(&self) -> Locate {
        Locate { index_parent: Some(self.dir.join("index")), ..Locate::default() }
    }

    fn cli(&self, words: &[&str]) -> Cli {
        Cli { words: words.iter().map(|w| w.to_string()).collect(), kc: Some(self.kc.clone()), kernel: Some(self.kernel.clone()), format: "text".into(), ..Cli::default() }
    }

    fn run(&self, cli: Cli) -> (String, String) {
        let mut err = Vec::new();
        let out = oneios_ksym::run_cli(cli, self.locate(), &mut std::io::empty(), &mut err).unwrap();
        (out, String::from_utf8(err).unwrap())
    }

    /// The prelink's record beside the collection, naming a kernel of this hash.
    fn record(&self, kernel_sha: &str) {
        let kc_sha = oneios_common::sha256_hex(&std::fs::read(&self.kc).unwrap());
        let text = format!(r#"{{"output": "kernel.kc", "sha256": "{kc_sha}", "inputs": {{"kernel": {{"path": "/elsewhere/xnu.macho", "sha256": "{kernel_sha}"}}}}}}"#);
        std::fs::write(self.dir.join("kc/kernel.kc.build.json"), text).unwrap();
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn sha(path: &Path) -> String {
    oneios_common::sha256_hex(&std::fs::read(path).unwrap())
}

#[test]
fn the_collection_is_read_member_by_member() {
    let image = fileset::parse(&collection_file(true, false), true).unwrap();
    assert!(image.fileset);
    assert_eq!(image.link_base(), 0xfffffff007004000);
    assert_eq!(image.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["com.apple.kernel", "com.example.kext"]);
    let kernel = image.kernel().unwrap();
    assert_eq!(kernel.vmaddr, KC_TEXT);
    assert_eq!(kernel.segments.iter().map(|s| (s.name.as_str(), s.vmaddr)).collect::<Vec<_>>()[..2], [("__TEXT", KC_TEXT), ("__TEXT_EXEC", KC_EXEC)]);
    assert_eq!(kernel.symbols.len(), 5);
    // A symbol's segment is its section's, not the one its address falls in.
    let marker = kernel.symbols.iter().find(|s| s.name == "_etext_marker").unwrap();
    assert_eq!(marker.segment, Some(1));
    assert_eq!(image.members[1].symbols.len(), 0);

    let locator = Locator::new(&image.members.iter().map(|m| (m.name.clone(), m.vmaddr, m.segments.clone())).collect::<Vec<_>>());
    assert_eq!(locator.find(KC_EXEC + 0x715388), Some((0, 1)));
    assert_eq!(locator.find(KC_TEXT), Some((0, 0)));
    assert_eq!(locator.find(KEXT_EXEC + 0xfff), Some((1, 1)));
    assert_eq!(locator.find(KEXT_EXEC + 0x1000), None);
    // Both members list the shared __LINKEDIT: the first has it.
    assert_eq!(locator.find(LINKEDIT + 8), Some((0, 4)));
    // A segment of no size holds nothing.
    assert_eq!(locator.find(KC_EXEC + EXEC_SIZE + 0x8000), None);

    // A plain Mach-O is one member.
    let plain = fileset::parse(&kernel_file(), true).unwrap();
    assert!(!plain.fileset);
    assert_eq!((plain.members.len(), plain.link_base()), (1, K_TEXT));
    assert!(fileset::parse(b"not a Mach-O file at all, but long enough to hold a header", true).is_err());
}

#[test]
fn every_segment_has_its_own_move() {
    let kc = fileset::parse(&collection_file(true, false), true).unwrap();
    let kernel = fileset::parse(&kernel_file(), true).unwrap();
    let moves = fileset::moves(kc.kernel().unwrap(), kernel.kernel().unwrap());
    let by = |name: &str| moves.iter().find(|m| m.name == name).unwrap();
    // The worked example: __TEXT moved by 0x180000, __TEXT_EXEC by 0x3870000.
    assert_eq!(by("__TEXT").kc_vmaddr - by("__TEXT").kernel_vmaddr, 0x180000);
    assert_eq!(by("__TEXT_EXEC").kc_vmaddr - by("__TEXT_EXEC").kernel_vmaddr, 0x3870000);
    assert_eq!(by("__TEXT_EXEC").to_kernel(0xfffffff00b54c094), 0xfffffff007cdc094);
    assert_eq!(by("__TEXT_EXEC").to_kc(0xfffffff007cdc094), 0xfffffff00b54c094);
    assert_eq!(by("__TEXT").to_kernel(KC_TEXT + 0x100), K_TEXT + 0x100);
    // __TEXT's move applied to a __TEXT_EXEC address would be 0x36f0000 off: the segments must not share one.
    assert_ne!(by("__TEXT").to_kernel(0xfffffff00b54c094), by("__TEXT_EXEC").to_kernel(0xfffffff00b54c094));
    assert!(by("__TEXT_EXEC").holds_kc(KC_EXEC + EXEC_SIZE - 4) && !by("__TEXT_EXEC").holds_kc(KC_EXEC + EXEC_SIZE));
    // __LINKEDIT is not in the kernel file here: no move for it. __LAST has no size and still a move.
    assert!(moves.iter().all(|m| m.name != "__LINKEDIT"));
    assert_eq!(by("__LAST").vmsize, 0);
}

#[test]
fn the_physical_aperture_rule() {
    // The log of 2026-11-12: link base 0xfffffff007004000, RAM at 0x2000004000, slide 0x2ff9000000.
    assert_eq!(fileset::slide_of(0x2000004000, 0xfffffff007004000), SLIDE);
    // pc 0x200454c094 is the frame 0xfffffff00b54c094; lr 0x2003ff1038 is 0xfffffff00aff1038.
    assert_eq!(fileset::unslide(0x200454c094, SLIDE), 0xfffffff00b54c094);
    assert_eq!(fileset::unslide(0x2003ff1038, SLIDE), 0xfffffff00aff1038);
    // The first byte of RAM is the link base.
    assert_eq!(fileset::unslide(0x2000004000, SLIDE), 0xfffffff007004000);
    // Another place for the RAM (xnuhost retries 16 GiB higher): another slide, the same answer.
    assert_eq!(fileset::unslide(0x2400004000 + 0x4548094, fileset::slide_of(0x2400004000, 0xfffffff007004000)), 0xfffffff00b54c094);
}

fn frame(function: &str, file: &str, line: u32) -> FrameRec {
    FrameRec { function: function.into(), file: file.into(), line }
}

#[test]
fn the_index_answers_with_lines_and_inlined_frames() {
    let files = Files::new("index", true, true);
    let kc_data = std::fs::read(&files.kc).unwrap();
    let kernel_data = std::fs::read(&files.kernel).unwrap();
    let kc_image = fileset::parse(&kc_data, true).unwrap();
    let kernel_image = fileset::parse(&kernel_data, true).unwrap();
    // Lines given by hand, at the unprelinked kernel's addresses: tcp_input, a call in it, and compute_iaj with
    // compute_iaj_meat inlined in it.
    let tcp = K_EXEC + 0x715000;
    let iaj = K_EXEC + 0x71c000;
    let lines = Lines {
        frames: vec![
            frame("tcp_input", "/src/bsd/netinet/tcp_input.c", 2789),
            frame("tcp_input", "/src/bsd/netinet/tcp_input.c", 3721),
            frame("tcp_input", "/src/bsd/netinet/tcp_input.c", 3723),
            frame("compute_iaj", "/src/bsd/netinet/tcp_input.c", 453),
            frame("compute_iaj_meat", "/src/bsd/netinet/tcp_input.c", 408),
            frame("_ZN9IOService5startEPS_", "/src/iokit/Kernel/IOService.cpp", 700),
        ],
        chains: vec![vec![0], vec![1], vec![2], vec![3, 4], vec![5]],
        runs: vec![(K_EXEC + 0x2000, 4), (K_EXEC + 0x2010, NONE), (tcp, 0), (tcp + 0x1538, 1), (tcp + 0x153c, 2), (tcp + 0x1550, NONE), (iaj, 3), (iaj + 0x2b8, NONE)],
        source: Source { kind: "debug-map".into(), objects: 2, read: 2, ..Source::default() },
    };
    let dir = files.dir.join("index/by-hand");
    let id = |path: &Path, data: &[u8]| FileId { path: path.display().to_string(), sha256: oneios_common::sha256_hex(data), bytes: data.len() as u64 };
    let meta = index::build(
        &dir,
        index::Input { kc: id(&files.kc, &kc_data), kc_image: &kc_image, kernel: Some((id(&files.kernel, &kernel_data), &kernel_image)), lines, seconds: 0.0 },
    )
    .unwrap();
    assert_eq!((meta.own_symbols, meta.own_agree), (5, 5));
    assert!(meta.resized.is_empty());
    assert!(Index::is_current(&dir, &meta.kc.sha256, Some(&sha(&files.kernel))));
    assert!(!Index::is_current(&dir, &meta.kc.sha256, Some("another kernel")));
    assert!(!Index::is_current(&dir, &meta.kc.sha256, None));

    let index = Index::open(&dir).unwrap();
    // A return address in tcp_input: the collection's address is the kernel's plus __TEXT_EXEC's own move.
    let answer = index.resolve(KC_EXEC + 0x715000 + 0x153c);
    assert_eq!(answer.member, Some(("com.apple.kernel".into(), "__TEXT_EXEC".into())));
    assert_eq!(answer.kernel_addr, Some(tcp + 0x153c));
    assert_eq!(answer.symbol, Some(("_tcp_input".into(), 0x153c)));
    assert_eq!(answer.frames.iter().map(|f| (f.function.as_str(), f.line)).collect::<Vec<_>>(), [("tcp_input", 3723)]);
    // The call before it is another line.
    assert_eq!(index.resolve(KC_EXEC + 0x715000 + 0x1538).frames[0].line, 3721);
    // Inlined: the outermost function first, the place itself last.
    let inlined = index.resolve(KC_EXEC + 0x71c000 + 0x60);
    assert_eq!(inlined.symbol, Some(("_compute_iaj".into(), 0x60)));
    assert_eq!(inlined.frames.iter().map(|f| (f.function.as_str(), f.line)).collect::<Vec<_>>(), [("compute_iaj", 453), ("compute_iaj_meat", 408)]);
    // After the last run: no lines, the symbol still.
    let after = index.resolve(KC_EXEC + 0x71c000 + 0x2b8);
    assert!(after.frames.is_empty() && after.symbol == Some(("_compute_iaj".into(), 0x2b8)));
    // Two names of one address: the last external one (an end label comes first); a C++ name demangled.
    assert_eq!(index.resolve(KC_EXEC + 0x1004).symbol, Some(("_start_first".into(), 4)));
    let cpp = index.resolve(KC_EXEC + 0x2008);
    assert_eq!(cpp.frames[0].function, "IOService::start(IOService*)");
    assert_eq!(cpp.frames[0].raw_function.as_deref(), Some("_ZN9IOService5startEPS_"));
    // __TEXT has its own move.
    let text = index.resolve(KC_TEXT + 0x104);
    assert_eq!((text.kernel_addr, text.symbol), (Some(K_TEXT + 0x104), Some(("_version".into(), 4))));
    // A symbol on its segment's end is that segment's: the next segment's first address does not get it.
    assert_eq!(index.find_symbol("etext_marker").len(), 1);
    assert_eq!(index.find_symbol("etext_marker")[0].0, KC_EXEC + EXEC_SIZE);
    assert_eq!(index.find_symbol("last_kernel_symbol")[0].0, KC_EXEC + EXEC_SIZE + 0x8000);
    assert_eq!(index.to_kc(tcp), Some(KC_EXEC + 0x715000));
    assert_eq!(index.to_kc(0x1000), None);
    // The kext: its own symbol where the collection keeps one, no kernel address, no lines.
    let kext = index.resolve(KEXT_EXEC + 0x48);
    assert_eq!(kext.member, Some(("com.example.kext".into(), "__TEXT_EXEC".into())));
    assert_eq!((kext.symbol, kext.kernel_addr, kext.frames.len()), (Some(("__ZN7Example5probeEv".into(), 8)), None, 0));
    // Below the kext's first symbol: no symbol, the offset in the segment.
    let low = index.resolve(KEXT_EXEC + 0x10);
    assert_eq!((low.symbol, low.segment_offset), (None, 0x10));
    // In the collection, in no member.
    assert_eq!(index.resolve(0xfffffff00700c000).member, None);
    assert_eq!(index.top_segment(0xfffffff00700c000), Some("__PRELINK_TEXT"));
    assert!(!index.holds(0x2000004000));
}

fn log_text() -> String {
    let pc = KC_EXEC + 0xa08000 + 4;
    let lr = KC_EXEC + 0x71c000 + 0x1ec;
    format!(
        "\
[    0.011] xnuhost I kernel: /data/local/tmp/oneios/stage/kernel.kc, collection arm64e, link base 0xfffffff007004000, 5 segments, 2 members
[    0.019] xnuhost I memory: 9728 MiB at 0x2000004000, physical = virtual (slide {SLIDE:#x}); I/O aperture 0x8000000
[ 4120.731] guest     panic(cpu 4 caller {caller:#x}): Unexpected fault in kernel physical aperture at pc 0x{run_pc:016x}, lr 0x{run_lr:016x} (saved state: 0x00000020178a3690)
[ 4120.731] guest     \t  x28: 0x0000000000000000 fp:  0x00000020178a3a10  lr:  0x{run_lr:016x}  sp:  0x00000020178a39f0
[ 4120.731] guest     \t  pc:  0x{run_pc:016x} cpsr: 0x60400204         esr: 0x00000000f2005519  far: 0x0000000000000000
[ 4120.731] guest       frame 0x{pc:016x}
[ 4120.731] guest       frame 0x{tcp:016x}
[ 4120.731] guest       frame 0x{kext:016x}
[ 4120.731] guest       frame 0x00000000deadbeef
[ 4120.740] xnuhost I stopping: the guest panicked
",
        caller = KC_EXEC + 0x1008,
        run_pc = pc.wrapping_add(SLIDE),
        run_lr = lr.wrapping_add(SLIDE),
        tcp = KC_EXEC + 0x715000 + 0x153c,
        kext = KEXT_EXEC + 0x20,
    )
}

#[test]
fn a_panic_block_is_named() {
    let files = Files::new("panic", true, false);
    files.record(&sha(&files.kernel));
    let log = files.dir.join("xnuhost.log");
    std::fs::write(&log, log_text()).unwrap();
    let (out, err) = files.run(files.cli(&[log.to_str().unwrap()]));
    assert!(err.contains("building it (once)") && err.contains("address(es) named"), "{err}");
    // The head: which files, their hashes, that they belong together.
    assert!(out.contains(&format!("sha256 {}", sha(&files.kc))), "{out}");
    assert!(out.contains(&format!("sha256 {}", sha(&files.kernel))), "{out}");
    assert!(out.contains("kernel.kc.build.json names this kernel"), "{out}");
    assert!(out.contains("5 symbols the collection keeps for the kernel are all where this kernel's symbols land"), "{out}");
    assert!(!out.contains("WARNING"), "{out}");
    assert!(out.contains("dwarf   none"), "{out}");
    assert!(out.contains("(cpu 4): Unexpected fault in kernel physical aperture"), "{out}");
    assert!(out.contains("running address = collection address + slide 0x2ff9000000 (RAM 0x2000004000 - link base 0xfffffff007004000)"), "{out}");
    let line = |start: &str| out.lines().find(|l| l.trim_start().starts_with(start)).unwrap_or_else(|| panic!("no line {start:?} in\n{out}")).to_string();
    // Two names of the caller's address: the function, not the end label before it.
    assert_eq!(line("caller"), format!("  caller {:#x}  com.apple.kernel __TEXT_EXEC  start_first+0x8", KC_EXEC + 0x1008));
    // The running pc is the first frame's address plus the slide: the same function, the same offset.
    let pc = KC_EXEC + 0xa08004;
    assert_eq!(
        line("pc "),
        format!("  pc     {:#x} (running; {pc:#x} in the collection)  com.apple.kernel __TEXT_EXEC  __ubsan_handle_mul_overflow_minimal+0x4", pc.wrapping_add(SLIDE))
    );
    assert_eq!(line(&format!("frame  {pc:#x}")), format!("  frame  {pc:#x}  com.apple.kernel __TEXT_EXEC  __ubsan_handle_mul_overflow_minimal+0x4"));
    assert!(line("lr ").ends_with("com.apple.kernel __TEXT_EXEC  compute_iaj+0x1ec"), "{out}");
    assert!(line(&format!("frame  {:#x}", KC_EXEC + 0x71653c)).ends_with("tcp_input+0x153c"), "{out}");
    // A kext without symbols: member, segment and the offset in it. An address outside: said, not dropped.
    assert!(line(&format!("frame  {:#x}", KEXT_EXEC + 0x20)).contains("com.example.kext __TEXT_EXEC  __TEXT_EXEC+0x20  [the collection keeps no symbols of this member"), "{out}");
    assert!(line("frame  0xdeadbeef").contains("not in the collection (0xfffffff007004000 to 0xfffffff00c618000); as an address of the running kernel (slide 0x2ff9000000) it is"), "{out}");
    assert_eq!(out.lines().filter(|l| l.starts_with("  frame ")).count(), 4);

    // The second call reads the index only: the same text but for the index line.
    let (again, err) = files.run(files.cli(&[log.to_str().unwrap()]));
    assert!(!err.contains("building"), "{err}");
    let strip = |text: &str| text.lines().filter(|l| !l.starts_with("index ")).collect::<Vec<_>>().join("\n");
    assert_eq!(strip(&out), strip(&again));

    // JSON: every field of every address.
    let (json, _) = files.run(Cli { format: "json".into(), ..files.cli(&[log.to_str().unwrap()]) });
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let frames = value["panics"][0]["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 7);
    assert_eq!(frames[1]["kind"], "pc");
    assert_eq!(frames[1]["space"], "running");
    assert_eq!(frames[1]["kc_address"], format!("{pc:#x}"));
    assert_eq!(frames[1]["kernel_address"], format!("{:#x}", K_EXEC + 0xa08004));
    assert_eq!(frames[1]["symbol"], "__ubsan_handle_mul_overflow_minimal");
    assert_eq!(frames[1]["raw_symbol"], "___ubsan_handle_mul_overflow_minimal");
    assert_eq!(frames[6]["space"], "outside");
    assert_eq!(value["panics"][0]["slide"], "0x2ff9000000");
    assert_eq!(value["kc"]["sha256"], sha(&files.kc));

    // What `oneios-tool log panic` and `log stacks` call: the same block, without the index line; nothing for a
    // log without a panic; the addresses unnamed when the collection is not there.
    let loc = Locate { kc: Some(files.kc.clone()), kernel: Some(files.kernel.clone()), ..files.locate() };
    let report = oneios_ksym::panics_report(&log_text(), "xnuhost.log", &loc, None, &mut Vec::new()).unwrap();
    assert!(report.contains("panic at xnuhost.log line 3 (cpu 4)") && report.contains("  lr     ") && !report.contains("\nindex "), "{report}");
    assert!(report.contains(&format!("  frame  {:#x}  com.apple.kernel __TEXT_EXEC  tcp_input+0x153c", KC_EXEC + 0x71653c)), "{report}");
    assert!(oneios_ksym::panics_report("[ 1.000] guest     no panic here\n", "x.log", &loc, None, &mut Vec::new()).is_none());
    let gone = Locate { kc: Some(files.dir.join("gone/kernel.kc")), ..files.locate() };
    let unnamed = oneios_ksym::panics_report(&log_text(), "xnuhost.log", &gone, None, &mut Vec::new()).unwrap();
    assert!(unnamed.starts_with("kernel addresses are not named: ") && unnamed.contains("\n  frame  0xdeadbeef\n"), "{unnamed}");

    // A log without the boot's `memory:` line: the collection's addresses are named, the running ones say why not.
    let cut: String = log_text().lines().skip(2).map(|l| format!("{l}\n")).collect();
    let cut_log = files.dir.join("cut.log");
    std::fs::write(&cut_log, cut).unwrap();
    let (out, _) = files.run(files.cli(&[cut_log.to_str().unwrap()]));
    assert!(out.contains("WARNING: the log has no `memory: ... (slide 0x...)` line before this panic"), "{out}");
    assert!(out.contains("its slide is needed") && out.contains("tcp_input+0x153c"), "{out}");
    // --slide gives it.
    let (out, _) = files.run(Cli { slide: Some("2ff9000000".into()), ..files.cli(&[cut_log.to_str().unwrap()]) });
    assert!(out.contains("(running; ") && !out.contains("WARNING"), "{out}");
}

#[test]
fn addresses_by_hand() {
    let files = Files::new("hand", true, true);
    let tcp = format!("{:#x}", KC_EXEC + 0x715010);
    // No slide: a running address cannot be named, and the line says what is missing.
    let running = format!("{:#x}", (KC_EXEC + 0x715010).wrapping_add(SLIDE));
    let (out, _) = files.run(files.cli(&[&tcp, &running]));
    assert!(out.contains(&format!("{tcp}  com.apple.kernel __TEXT_EXEC  tcp_input+0x10  [xnu.macho {:#x}]", K_EXEC + 0x715010)), "{out}");
    assert!(out.contains(&format!("{running}  not in the collection")) && out.contains("its slide is needed"), "{out}");
    // With --slide it is the same place.
    let (out, _) = files.run(Cli { slide: Some("0x2ff9000000".into()), ..files.cli(&[&running]) });
    assert!(out.contains(&format!("{running} (running; {tcp} in the collection)  com.apple.kernel __TEXT_EXEC  tcp_input+0x10  [xnu.macho {:#x}, running {running}]", K_EXEC + 0x715010)), "{out}");
    // --xnu: an address of the unprelinked kernel.
    let xnu = format!("{:#x}", K_EXEC + 0x715010);
    let (out, _) = files.run(Cli { xnu: true, ..files.cli(&[&xnu]) });
    assert!(out.contains(&format!("{xnu} (xnu.macho; {tcp} in the collection)  com.apple.kernel __TEXT_EXEC  tcp_input+0x10")), "{out}");
    // --symbol: where a function is in the collection; a C++ kext symbol demangled; a name that is not there.
    let (out, _) = files.run(Cli { symbol: vec!["tcp_input".into(), "_ZN7Example5probeEv".into(), "nothing".into()], ..files.cli(&[]) });
    assert!(out.contains(&format!("{:#x}  com.apple.kernel __TEXT_EXEC  tcp_input+0x0", KC_EXEC + 0x715000)), "{out}");
    assert!(out.contains(&format!("{:#x}  com.example.kext __TEXT_EXEC  Example::probe()+0x0", KEXT_EXEC + 0x40)), "{out}");
    assert!(out.contains("nothing  no symbol of this name in the index"), "{out}");
    // Standard input, and a word that is neither a file nor an address.
    let mut err = Vec::new();
    let out = oneios_ksym::run_cli(Cli { stdin: true, ..files.cli(&[]) }, files.locate(), &mut format!("{tcp}\n{tcp}\n").as_bytes(), &mut err).unwrap();
    assert_eq!(out.matches("tcp_input+0x10").count(), 2);
    assert!(oneios_ksym::run_cli(files.cli(&["nonsense"]), files.locate(), &mut std::io::empty(), &mut err).is_err());
    assert!(oneios_ksym::run_cli(files.cli(&[]), files.locate(), &mut std::io::empty(), &mut err).is_err());
    // `index`: the description, with each segment's move.
    let (out, _) = files.run(files.cli(&["index"]));
    assert!(out.contains("__TEXT_EXEC        0xfffffff00ab44000 in the collection, 0xfffffff0072d4000 in the kernel: moved by 0x3870000"), "{out}");
    assert!(out.contains("__TEXT             0xfffffff007184000 in the collection, 0xfffffff007004000 in the kernel: moved by 0x180000"), "{out}");
}

#[test]
fn a_wrong_pair_is_warned_about() {
    // The record names another kernel.
    let files = Files::new("record", true, false);
    files.record("0000000000000000000000000000000000000000000000000000000000000000");
    let (out, _) = files.run(files.cli(&[&format!("{:#x}", KC_EXEC + 0x715010)]));
    assert!(out.contains("WARNING: this kernel.kc was NOT prelinked from this kernel"), "{out}");
    assert!(out.contains("/elsewhere/xnu.macho (000000000000...)"), "{out}");

    // No record, and the collection's own kernel symbols are not where this kernel's land.
    let files = Files::new("symbols", true, false);
    let mut other = kernel_file();
    // Another build: tcp_input 0x40 further (its n_value in the symbol table).
    let at = other.windows(8).position(|w| w == (K_EXEC + 0x715000).to_le_bytes()).unwrap();
    other[at..at + 8].copy_from_slice(&(K_EXEC + 0x715040).to_le_bytes());
    std::fs::write(&files.kernel, other).unwrap();
    let (out, _) = files.run(files.cli(&[&format!("{:#x}", KC_EXEC + 0x715010)]));
    assert!(out.contains("no prelink record"), "{out}");
    assert!(out.contains("WARNING: only 4 of the 5 symbols the collection keeps for the kernel are where this kernel's symbols land"), "{out}");

    // The record is of another collection: it says nothing.
    let files = Files::new("stale", true, false);
    std::fs::write(files.dir.join("kc/kernel.kc.build.json"), r#"{"sha256": "1111", "inputs": {"kernel": {"path": "x", "sha256": "2222"}}}"#).unwrap();
    let (out, _) = files.run(files.cli(&[&format!("{:#x}", KC_EXEC + 0x715010)]));
    assert!(out.contains("is the record of another kernel.kc"), "{out}");
    assert!(!out.contains("NOT prelinked"), "{out}");
}

#[test]
fn the_collection_alone() {
    let files = Files::new("alone", true, false);
    let (out, _) = files.run(Cli { no_kernel: true, kernel: None, ..files.cli(&[&format!("{:#x}", KC_EXEC + 0x715010)]) });
    assert!(out.contains("kernel  none"), "{out}");
    // The kernel member's own symbol, at the collection's address; no address in xnu.macho, no lines.
    assert!(out.contains(&format!("{:#x}  com.apple.kernel __TEXT_EXEC  tcp_input+0x10\n", KC_EXEC + 0x715010)), "{out}");
    // A plain kernel in place of a collection (xnuhost loads those too): one member, no move.
    let (out, _) = files.run(Cli { kc: Some(files.kernel.clone()), ..files.cli(&[&format!("{:#x}", K_EXEC + 0x715010)]) });
    assert!(out.contains("not a collection: a plain Mach-O"), "{out}");
    assert!(out.contains(&format!("{:#x}  kernel __TEXT_EXEC  tcp_input+0x10", K_EXEC + 0x715010)), "{out}");
}

/// The kernel's debug map names object files. One that was rebuilt since the link (another time than the stab's)
/// or that is not there gives no lines, and the head says so: lines of another build would be wrong ones.
#[test]
fn object_files_that_changed_are_not_read() {
    const N_SO: u8 = 0x64;
    const N_OSO: u8 = 0x66;
    const N_FUN: u8 = 0x24;
    let files = Files::new("objects", true, false);
    let rebuilt = files.dir.join("tcp_input.o");
    let gone = files.dir.join("archive.a(compute.o)");
    let module = |source: &str, object: &Path, time: u64, function: &str, at: u64| {
        vec![
            (N_SO, source.to_string(), 0, 0),
            (N_OSO, object.display().to_string(), 0, time),
            (N_FUN, function.to_string(), 2, at),
            (N_FUN, String::new(), 0, 0x100),
            (N_SO, String::new(), 0, 0),
        ]
    };
    let mut stabs = module("tcp_input.c", &rebuilt, 1_700_000_000, "_tcp_input", K_EXEC + 0x715000);
    stabs.extend(module("compute.c", &gone, 1_700_000_001, "_compute_iaj", K_EXEC + 0x71c000));
    std::fs::write(&files.kernel, kernel_file_with(stabs)).unwrap();
    // The object file is there, written now: not the one the kernel was linked from.
    std::fs::write(&rebuilt, b"not the object file of the link").unwrap();
    let addr = format!("{:#x}", KC_EXEC + 0x715010);
    let (out, _) = files.run(files.cli(&[&addr]));
    assert!(out.contains("dwarf   the kernel's debug map: 0 of 2 object files read, 1 rebuilt since the link (no lines for their functions), 1 not there"), "{out}");
    assert!(out.contains("WARNING: the kernel's object files were not all usable when the index was built (rebuilt: "), "{out}");
    // The symbol table still names the function.
    assert!(out.contains(&format!("{addr}  com.apple.kernel __TEXT_EXEC  tcp_input+0x10")), "{out}");

    // With the stab's time the file counts as the link's (it holds no DWARF: still no lines, and no failure).
    let link_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    std::fs::File::options().write(true).open(&rebuilt).unwrap().set_modified(link_time).unwrap();
    let (out, err) = files.run(Cli { force: true, ..files.cli(&["index"]) });
    assert!(err.contains("building it"), "{err}");
    assert!(out.contains("dwarf   the kernel's debug map: 1 of 2 object files read, 1 not there"), "{out}");
    assert!(out.contains("WARNING: the kernel's object files were not all usable when the index was built (not there: "), "{out}");
}
