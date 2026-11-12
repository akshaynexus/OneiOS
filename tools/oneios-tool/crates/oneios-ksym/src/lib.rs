//! oneios-tool sym kernel: name the kernel addresses of an xnuhost panic (or addresses given by hand) in the
//! kernel collection that ran (`kernel.kc`) and the unprelinked kernel it was made from (`xnu.macho`): member and
//! segment, function + offset, file:line and the inlined frames.
//!
//! - `fileset.rs`: the collection's members and segments (`object`), each kernel segment's own move, the
//!   physical-aperture rule.
//! - `panic.rs`: the panic blocks of a log and each boot's slide.
//! - `dwarf.rs`: file, line and inlined frames of every instruction of the kernel (`addr2line`, `gimli`).
//! - `index.rs`: what the three found, kept under `out/ksym-index/` so that a later call reads none of the files.
//!
//! Library use (the log tool): `open(&Locate::from_env(), err)`, then `name_block` and `render_block`, or
//! `panics_report` for all of it.

pub mod dwarf;
pub mod fileset;
pub mod index;
pub mod panic;

pub use index::{Answer, FileId, Frame, Index};
pub use panic::{Block, Kind};

use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const AFTER_HELP: &str = "\
A word that is a file is read as an xnuhost session log (`-`: standard input): every `panic(cpu N caller 0x...)`
block is named, its caller, the pc and lr of the fault and every `frame 0x...` line. Any other word is an address
(hex, with or without 0x).

Addresses:
  in the collection   `caller` and `frame` lines: link-time addresses of kernel.kc. The member and segment that
                      hold the address are found; for com.apple.kernel the address in xnu.macho is the address
                      minus that segment's own move (the prelink places every segment on its own).
  of the running kernel   the fault's pc and lr. xnuhost runs the collection at RAM + (address - link base), so
                      the collection's address is the running address minus the slide. The slide is the log's
                      (`memory: N MiB at RAM, physical = virtual (slide S)`), or --slide.

The files: --kc and --kernel, else ONEIOS_KC and ONEIOS_KERNEL, else the newest out/kernel/*/kernel.kc and the
xnu.macho its prelink record (kernel.kc.build.json) names, else the newest out/kernel/current-hosted*/xnu.macho.
The first call for a pair of files builds an index under out/ksym-index/ (--index DIR or ONEIOS_KSYM_INDEX for
another place); later calls read only the index.
  index     as the first word: build the index now and describe it (--force: again)";

/// `oneios-tool sym kernel`'s command line.
#[derive(clap::Parser, Debug, Default)]
#[command(name = "oneios-tool sym kernel", about = "Name the kernel addresses of an xnuhost panic in kernel.kc and xnu.macho", after_help = AFTER_HELP)]
pub struct Cli {
    /// An xnuhost session log (`-`: standard input), addresses, or `index`
    #[arg(value_name = "LOG|ADDR")]
    pub words: Vec<String>,
    /// The kernel collection that ran (kernel.kc)
    #[arg(long, value_name = "PATH")]
    pub kc: Option<PathBuf>,
    /// The unprelinked kernel it was made from (xnu.macho, with symbols and a debug map or DWARF)
    #[arg(long, value_name = "PATH")]
    pub kernel: Option<PathBuf>,
    /// The kernel's dSYM (the bundle or the DWARF file in it), when it is not beside the kernel
    #[arg(long, value_name = "PATH")]
    pub dsym: Option<PathBuf>,
    /// Use the collection alone: members, segments and the symbols it keeps; no file:line
    #[arg(long = "no-kernel")]
    pub no_kernel: bool,
    /// The slide of running addresses (hex), for addresses given by hand or a log without its `memory:` line
    #[arg(long, value_name = "HEX")]
    pub slide: Option<String>,
    /// The folder the indexes are kept in (default: ONEIOS_KSYM_INDEX, else out/ksym-index)
    #[arg(long, value_name = "DIR")]
    pub index: Option<PathBuf>,
    /// text or json
    #[arg(short = 'f', long, value_name = "FORMAT", default_value = "text")]
    pub format: String,
    /// Whole source paths in the text output (JSON always has them)
    #[arg(long = "full-paths")]
    pub full_paths: bool,
    /// The addresses given are addresses of xnu.macho (the unprelinked kernel), not of the collection
    #[arg(long)]
    pub xnu: bool,
    /// The addresses given are return addresses, as a panic's frames are: say where the call is too
    #[arg(long = "return")]
    pub returns: bool,
    /// Do not add the call's place to return addresses
    #[arg(long = "no-call-sites")]
    pub no_call_sites: bool,
    /// Name the address of a kernel symbol (repeatable)
    #[arg(long, value_name = "NAME")]
    pub symbol: Vec<String>,
    /// Read more addresses from standard input
    #[arg(long)]
    pub stdin: bool,
    /// `index`: build it again
    #[arg(long)]
    pub force: bool,
    /// Hash the files again instead of trusting the size and time the last hash was taken at
    #[arg(long)]
    pub rehash: bool,
    /// Threads for the index build (default: the CPUs)
    #[arg(short = 'j', long, value_name = "N")]
    pub jobs: Option<usize>,
}

/// Where the files and the indexes are.
#[derive(Clone, Debug, Default)]
pub struct Locate {
    pub kc: Option<PathBuf>,
    pub kernel: Option<PathBuf>,
    pub dsym: Option<PathBuf>,
    pub no_kernel: bool,
    /// The folder that holds the indexes.
    pub index_parent: Option<PathBuf>,
    pub force: bool,
    pub rehash: bool,
    pub jobs: Option<usize>,
}

impl Locate {
    pub fn from_env() -> Locate {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        Locate { kc: var("ONEIOS_KC"), kernel: var("ONEIOS_KERNEL"), index_parent: var("ONEIOS_KSYM_INDEX"), ..Locate::default() }
    }

    /// The OneiOS checkouts to look in: the one above the tool or the current folder, and for a git worktree
    /// under `<checkout>/.claude/worktrees/` the checkout itself (its `out/` has the kernels).
    fn roots() -> Vec<PathBuf> {
        let is_root = |dir: &Path| dir.join("tools/oneios-tool/Cargo.toml").is_file();
        let exe = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
        let cwd = std::env::current_dir().ok();
        let mut out: Vec<PathBuf> = Vec::new();
        for start in [exe, cwd].into_iter().flatten() {
            for root in start.ancestors().filter(|d| is_root(d)) {
                if !out.iter().any(|r| r == root) {
                    out.push(root.to_path_buf());
                }
            }
        }
        out
    }

    fn parent(&self) -> Result<PathBuf> {
        if let Some(dir) = &self.index_parent {
            return Ok(dir.clone());
        }
        match Self::roots().first() {
            Some(root) => Ok(root.join("out/ksym-index")),
            None => bail!("no OneiOS repository above the tool or the current folder: give --index DIR (or ONEIOS_KSYM_INDEX) for the index"),
        }
    }
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// The newest file among `out/kernel/<folder>/<file>` of every root, for folders `keep` takes.
fn newest(file: &str, keep: impl Fn(&str) -> bool) -> Option<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for root in Locate::roots() {
        let Ok(entries) = std::fs::read_dir(root.join("out/kernel")) else { continue };
        for entry in entries.flatten() {
            let path = entry.path().join(file);
            if keep(&entry.file_name().to_string_lossy()) && path.is_file() {
                if let Some(time) = modified(&path) {
                    found.push((time, path));
                }
            }
        }
    }
    found.sort();
    found.pop().map(|f| f.1)
}

fn kernel_candidates() -> Vec<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for root in Locate::roots() {
        let Ok(entries) = std::fs::read_dir(root.join("out/kernel")) else { continue };
        for entry in entries.flatten() {
            let path = entry.path().join("xnu.macho");
            if entry.file_name().to_string_lossy().starts_with("current-hosted") && path.is_file() {
                if let Some(time) = modified(&path) {
                    found.push((time, path));
                }
            }
        }
    }
    found.sort();
    found.into_iter().rev().map(|f| f.1).collect()
}

/// A file's SHA-256, kept in `<index folder>/hashes.json` with the size and time it was taken at: the collection
/// is 93 MB, and hashing it on every call would take longer than the answer.
fn file_id(path: &Path, parent: &Path, rehash: bool) -> Result<FileId> {
    let meta = std::fs::metadata(path).with_context(|| format!("{}", path.display()))?;
    let full = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let key = full.display().to_string();
    let stamp = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as u64).unwrap_or(0);
    let cache_path = parent.join("hashes.json");
    let mut cache: BTreeMap<String, (u64, u64, String)> = std::fs::read_to_string(&cache_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if !rehash {
        if let Some((bytes, time, sha256)) = cache.get(&key) {
            if *bytes == meta.len() && *time == stamp && stamp != 0 {
                return Ok(FileId { path: key, sha256: sha256.clone(), bytes: meta.len() });
            }
        }
    }
    let map = oneios_common::Map::open(path)?;
    let sha256 = oneios_common::sha256_hex(&map);
    cache.insert(key.clone(), (meta.len(), stamp, sha256.clone()));
    if std::fs::create_dir_all(parent).is_ok() {
        let _ = oneios_common::atomic_write(&cache_path, serde_json::to_string_pretty(&cache)?.as_bytes());
    }
    Ok(FileId { path: key, sha256, bytes: meta.len() })
}

/// What the prelink wrote beside the collection (`kernel.kc.build.json`, kernel/prelink/README.md): the
/// collection's hash and the kernel it was made from.
struct Record {
    path: PathBuf,
    kc_sha256: String,
    kernel_path: String,
    kernel_sha256: String,
}

fn record_of(kc: &Path) -> Option<Record> {
    let mut name = kc.file_name()?.to_os_string();
    name.push(".build.json");
    let path = kc.with_file_name(name);
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    let text = |v: &serde_json::Value| v.as_str().unwrap_or("").to_string();
    let kernel = &value["inputs"]["kernel"];
    Some(Record { path, kc_sha256: text(&value["sha256"]), kernel_path: text(&kernel["path"]), kernel_sha256: text(&kernel["sha256"]) })
}

/// The files in use and their index.
pub struct Session {
    pub index: Index,
    /// What was taken and from where, for the head of a report.
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
    /// The index was built by this call, in this many seconds.
    pub built: Option<f64>,
}

/// Find the collection and the kernel, say which, and open their index (building it once).
pub fn open(loc: &Locate, err: &mut dyn Write) -> Result<Session> {
    let parent = loc.parent()?;
    let mut warnings = Vec::new();
    let (kc_path, kc_how) = match &loc.kc {
        Some(path) => (path.clone(), "given"),
        None => (
            newest("kernel.kc", |_| true).ok_or_else(|| anyhow!("no kernel collection: give --kc PATH/kernel.kc (none under out/kernel/*/ of this checkout)"))?,
            "the newest under out/kernel",
        ),
    };
    let kc = file_id(&kc_path, &parent, loc.rehash)?;
    let record = record_of(&kc_path).filter(|r| {
        let ours = r.kc_sha256 == kc.sha256;
        if !ours {
            warnings.push(format!("{} is the record of another kernel.kc ({}): it says nothing about this one", r.path.display(), short(&r.kc_sha256)));
        }
        ours
    });

    let mut kernel_how = "given".to_string();
    let kernel_path: Option<PathBuf> = if loc.no_kernel {
        None
    } else if let Some(path) = &loc.kernel {
        Some(path.clone())
    } else {
        let mut pick = None;
        if let Some(record) = &record {
            let named = PathBuf::from(&record.kernel_path);
            let mut candidates = vec![named];
            candidates.extend(kernel_candidates());
            for candidate in candidates {
                if candidate.is_file() && file_id(&candidate, &parent, loc.rehash).is_ok_and(|id| id.sha256 == record.kernel_sha256) {
                    kernel_how = "the one the collection's prelink record names".into();
                    pick = Some(candidate);
                    break;
                }
            }
        }
        if pick.is_none() {
            pick = kernel_candidates().into_iter().next();
            kernel_how = "the newest under out/kernel/current-hosted*".into();
        }
        if pick.is_none() {
            warnings.push("no unprelinked kernel (give --kernel PATH/xnu.macho): only what the collection itself keeps is named, with no file:line".into());
        }
        pick
    };
    let kernel = kernel_path.as_ref().map(|path| file_id(path, &parent, loc.rehash)).transpose()?;

    let dir = index::dir_of(&parent, &kc.sha256, kernel.as_ref().map(|k| k.sha256.as_str()));
    let mut built = None;
    let mut wanted = loc.force || !Index::is_current(&dir, &kc.sha256, kernel.as_ref().map(|k| k.sha256.as_str()));
    if !wanted && loc.dsym.is_some() && kernel.is_some() {
        // A dSYM is named now and the index was built without one: its lines are to come from the dSYM.
        wanted = Index::open(&dir).map(|index| index.meta.dwarf.kind != "dsym").unwrap_or(true);
    }
    if wanted {
        let _ = writeln!(err, "oneios-tool sym kernel: no index of this pair in {}: building it (once)", parent.display());
        let seconds = build(&dir, &kc_path, &kc, kernel_path.as_deref().zip(kernel.as_ref()), loc)?;
        let _ = writeln!(err, "oneios-tool sym kernel: index built in {seconds:.1} s");
        built = Some(seconds);
    }
    let index = Index::open(&dir)?;
    let meta = &index.meta;

    let mut notes = Vec::new();
    notes.push(format!(
        "kc      {}  sha256 {}  ({kc_how}; {}, link base {:#x})",
        kc.path,
        kc.sha256,
        if meta.fileset { format!("{} members", meta.members.len()) } else { "not a collection: a plain Mach-O".to_string() },
        meta.link_base
    ));
    match &kernel {
        Some(kernel) => notes.push(format!("kernel  {}  sha256 {}  ({kernel_how})", kernel.path, kernel.sha256)),
        None => notes.push("kernel  none: the collection's own symbols only".to_string()),
    }
    if kernel.is_some() {
        notes.push(format!("dwarf   {}", meta.dwarf.describe()));
    }
    notes.push(format!(
        "index   {}  ({})",
        dir.display(),
        match built {
            Some(seconds) => format!("built now in {seconds:.1} s"),
            None => format!("built before in {:.1} s", meta.seconds),
        }
    ));

    // Do the two files belong together?
    if let Some(kernel) = &kernel {
        match &record {
            Some(record) if record.kernel_sha256 == kernel.sha256 => notes.push(format!("pair    {} names this kernel", record.path.display())),
            Some(record) => warnings.push(format!(
                "this kernel.kc was NOT prelinked from this kernel: {} names {} ({}), this one is {}. Names and lines may be wrong",
                record.path.display(),
                record.kernel_path,
                short(&record.kernel_sha256),
                short(&kernel.sha256)
            )),
            None => notes.push("pair    no prelink record (kernel.kc.build.json) beside the collection".to_string()),
        }
        if meta.kernel_member.is_none() {
            warnings.push(format!("the collection has no member {}", fileset::KERNEL_MEMBER));
        }
        if !meta.resized.is_empty() {
            warnings.push(format!("kernel segments of another size in the collection than in the kernel: {}. The two files do not belong together", meta.resized.join(", ")));
        }
        if meta.own_symbols != 0 {
            if meta.own_agree == meta.own_symbols {
                notes.push(format!("pair    {} symbols the collection keeps for the kernel are all where this kernel's symbols land", meta.own_symbols));
            } else {
                warnings.push(format!(
                    "only {} of the {} symbols the collection keeps for the kernel are where this kernel's symbols land: the two files do not belong together",
                    meta.own_agree, meta.own_symbols
                ));
            }
        }
        if meta.dwarf.kind == "debug-map" && (meta.dwarf.stale != 0 || meta.dwarf.missing != 0) {
            warnings.push(format!(
                "the kernel's object files were not all usable when the index was built ({}): frames in them have no file:line. With the files back (or --dsym), `oneios-tool sym kernel index --force`",
                meta.dwarf.example
            ));
        }
    }
    Ok(Session { index, notes, warnings, built })
}

fn short(sha: &str) -> String {
    format!("{}...", sha.get(..12).unwrap_or(sha))
}

/// Read the files and write their index; the seconds it took.
fn build(dir: &Path, kc_path: &Path, kc: &FileId, kernel: Option<(&Path, &FileId)>, loc: &Locate) -> Result<f64> {
    let start = std::time::Instant::now();
    let kc_map = oneios_common::Map::open(kc_path)?;
    let kc_image = fileset::parse(&kc_map, true).with_context(|| format!("{}", kc_path.display()))?;
    let jobs = loc.jobs.unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)).clamp(1, 64);
    let mut kernel_image = None;
    let mut lines = dwarf::Lines::default();
    if let Some((path, _)) = kernel {
        let map = oneios_common::Map::open(path)?;
        let image = fileset::parse(&map, true).with_context(|| format!("{}", path.display()))?;
        if image.fileset {
            bail!("{}: a kernel collection, not the unprelinked kernel (xnu.macho)", path.display());
        }
        lines = dwarf::walk(path, &map, loc.dsym.as_deref(), jobs)?;
        kernel_image = Some(image);
    }
    let input = index::Input {
        kc: kc.clone(),
        kc_image: &kc_image,
        kernel: kernel.map(|(_, id)| id.clone()).zip(kernel_image.as_ref()),
        lines,
        seconds: 0.0,
    };
    let seconds = start.elapsed().as_secs_f64();
    index::build(dir, index::Input { seconds, ..input })?;
    Ok(start.elapsed().as_secs_f64())
}

/// One named address.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Named {
    /// `caller`, `pc`, `lr`, `frame`, `address`, or `symbol`.
    pub kind: String,
    /// The address as it was given.
    pub address: String,
    /// The log line it is on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// `collection` (a link-time address of kernel.kc), `running` (an address of the running kernel: the slide
    /// was taken off), `kernel` (an address of xnu.macho, given with --xnu), or `outside`.
    pub space: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kc_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_offset: Option<String>,
    /// The same place in xnu.macho (the kernel member only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_address: Option<String>,
    /// The function (C++ names demangled) and the offset in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The symbol as the symbol table has it, when `symbol` is not it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<String>,
    /// The source frames, the outermost function first; the last one is where the address is, the ones before it
    /// are what it was inlined into.
    pub frames: Vec<Frame>,
    /// For a return address whose call instruction (4 bytes before) is somewhere else: that place.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_site: Option<Box<Named>>,
    /// Why there is no name, in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How addresses are read.
#[derive(Clone, Copy, Debug, Default)]
pub struct Reading {
    /// The boot's slide, for addresses of the running kernel.
    pub slide: Option<u64>,
    /// Addresses are xnu.macho's.
    pub xnu: bool,
    /// Add the call's place to return addresses.
    pub call_sites: bool,
}

fn hex(v: u64) -> String {
    format!("{v:#x}")
}

/// The symbol as people read it: Mach-O's leading underscore off, C++ demangled.
fn display_symbol(raw: &str) -> String {
    dwarf::demangle(raw.strip_prefix('_').unwrap_or(raw))
}

fn fill(named: &mut Named, index: &Index, kc_addr: u64) {
    let answer = index.resolve(kc_addr);
    named.kc_address = Some(hex(kc_addr));
    match &answer.member {
        Some((member, segment)) => {
            named.member = Some(member.clone());
            named.segment = Some(segment.clone());
            named.segment_offset = Some(hex(answer.segment_offset));
        }
        None => {
            named.note = Some(match index.top_segment(kc_addr) {
                Some(segment) => format!("in the collection's {segment}, in no member's segment"),
                None => "in no segment of the collection".to_string(),
            });
        }
    }
    named.kernel_address = answer.kernel_addr.map(hex);
    if let Some((raw, offset)) = &answer.symbol {
        let shown = display_symbol(raw);
        if &shown != raw {
            named.raw_symbol = Some(raw.clone());
        }
        named.symbol = Some(shown);
        named.offset = Some(hex(*offset));
    } else if answer.member.is_some() && !answer.is_kernel {
        named.note = Some("the collection keeps no symbols of this member: the offset is in its segment (kexts are at the kernelcache's addresses)".to_string());
    } else if answer.member.is_some() {
        named.note = Some("no symbol at or below this address in its segment".to_string());
    }
    named.frames = answer.frames;
}

/// Name one address.
pub fn name(index: &Index, kind: Kind, addr: u64, line: Option<usize>, reading: Reading) -> Named {
    let mut named = Named { kind: kind.label().to_string(), address: hex(addr), line, ..Named::default() };
    let meta = &index.meta;
    let kc_addr = if reading.xnu {
        named.space = "kernel".into();
        match index.to_kc(addr) {
            Some(kc_addr) => Some(kc_addr),
            None => {
                named.note = Some("not in a segment of the unprelinked kernel that the collection's kernel has".to_string());
                None
            }
        }
    } else if index.holds(addr) {
        named.space = "collection".into();
        Some(addr)
    } else {
        match reading.slide.map(|slide| fileset::unslide(addr, slide)).filter(|&unslid| index.holds(unslid)) {
            Some(unslid) => {
                named.space = "running".into();
                Some(unslid)
            }
            None => {
                named.space = "outside".into();
                let range = format!("not in the collection ({:#x} to {:#x})", meta.link_base, meta.end);
                named.note = Some(match reading.slide {
                    Some(slide) => format!("{range}; as an address of the running kernel (slide {slide:#x}) it is {:#x}: not in it either", fileset::unslide(addr, slide)),
                    None => format!("{range}; if it is an address of the running kernel, its slide is needed (the log's `memory:` line, or --slide)"),
                });
                None
            }
        }
    };
    let Some(kc_addr) = kc_addr else { return named };
    fill(&mut named, index, kc_addr);
    if kind.is_return() && reading.call_sites && named.member.is_some() {
        // A return address is the instruction after the call. The call's own line can be another one, and a
        // call that never returns can be the last instruction of its function.
        let mut call = Named { kind: "call".into(), address: hex(kc_addr.wrapping_sub(4)), space: "collection".into(), ..Named::default() };
        fill(&mut call, index, kc_addr.wrapping_sub(4));
        let same_function = call.symbol == named.symbol && call.member == named.member;
        if !same_function || call.frames != named.frames {
            named.call_site = Some(Box::new(call));
        }
    }
    named
}

/// A panic block, named.
#[derive(Clone, Debug, serde::Serialize)]
pub struct NamedBlock {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    pub line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u32>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram_base: Option<String>,
    /// The slide used for the running addresses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slide: Option<String>,
    pub warnings: Vec<String>,
    pub frames: Vec<Named>,
}

/// Name every address of a panic block. `slide`: one given by hand, used when the log has none.
pub fn name_block(index: &Index, block: &Block, log: Option<&str>, slide: Option<u64>, call_sites: bool) -> NamedBlock {
    let mut warnings = Vec::new();
    let meta = &index.meta;
    // The slide: the log's own line; it must be RAM - link base, and the link base must be this collection's.
    let computed = block.ram_base.zip(block.link_base).map(|(ram, base)| fileset::slide_of(ram, base));
    if let (Some(printed), Some(computed)) = (block.slide, computed) {
        if printed != computed {
            warnings.push(format!("the log's slide {printed:#x} is not RAM - link base = {computed:#x}: the printed one is used"));
        }
    }
    if let Some(base) = block.link_base {
        if base != meta.link_base {
            warnings.push(format!("the log's kernel has link base {base:#x}, this collection {:#x}: not the collection that ran", meta.link_base));
        }
    }
    let used = block.slide.or(computed).or(slide);
    if used.is_none() && block.entries.iter().any(|e| !index.holds(e.addr)) {
        warnings.push("the log has no `memory: ... (slide 0x...)` line before this panic: addresses of the running kernel are not named (give --slide)".to_string());
    }
    let reading = Reading { slide: used, xnu: false, call_sites };
    let mut frames: Vec<Named> = block.entries.iter().map(|e| name(index, e.kind, e.addr, Some(e.line), reading)).collect();
    // A frame line that is the fault's pc itself (a fault's first one) is not a return address: no call before it.
    let pcs: Vec<String> = frames.iter().filter(|f| f.kind == Kind::Pc.label()).filter_map(|f| f.kc_address.clone()).collect();
    for frame in frames.iter_mut().filter(|f| f.kind == Kind::Frame.label()) {
        if frame.kc_address.as_ref().is_some_and(|kc| pcs.contains(kc)) {
            frame.call_site = None;
        }
    }
    NamedBlock {
        log: log.map(str::to_string),
        line: block.line,
        cpu: block.cpu,
        text: block.text.clone(),
        link_base: block.link_base.map(hex),
        ram_base: block.ram_base.map(hex),
        slide: used.map(hex),
        warnings,
        frames,
    }
}

fn place(frame: &Frame, full_paths: bool) -> String {
    if frame.file.is_empty() {
        return String::new();
    }
    let file = if full_paths { frame.file.as_str() } else { frame.file.rsplit('/').next().unwrap_or(&frame.file) };
    format!("{file}:{}", frame.line)
}

/// `function+off  (file:line)` and, on lines of their own, the inlined frames.
fn what(named: &Named, full_paths: bool, indent: &str, out: &mut String) {
    let function = match (&named.symbol, &named.offset, named.frames.first()) {
        (Some(symbol), Some(offset), _) => format!("{symbol}+{offset}"),
        (_, _, Some(frame)) if !frame.function.is_empty() => frame.function.clone(),
        _ => match (&named.segment, &named.segment_offset) {
            (Some(segment), Some(offset)) => format!("{segment}+{offset}"),
            _ => "?".to_string(),
        },
    };
    out.push_str(&function);
    if let Some(frame) = named.frames.first() {
        let at = place(frame, full_paths);
        if !at.is_empty() {
            out.push_str(&format!("  ({at})"));
        }
    }
    if let Some(note) = &named.note {
        out.push_str(&format!("  [{note}]"));
    }
    out.push('\n');
    for (depth, frame) in named.frames.iter().enumerate().skip(1) {
        let at = place(frame, full_paths);
        out.push_str(&format!(
            "{indent}{}inlined {}{}\n",
            "  ".repeat(depth),
            if frame.function.is_empty() { "?" } else { &frame.function },
            if at.is_empty() { String::new() } else { format!("  ({at})") }
        ));
    }
}

/// One address as text: `ADDR  member segment  function+off  (file:line)`, the inlined frames indented under it.
/// `label`: start with the kind (`frame`, `pc`, ...), as a panic block's lines do.
pub fn render_named(named: &Named, label: bool, full_paths: bool, out: &mut String) {
    let lead = if label { format!("  {:<7}", named.kind) } else { String::new() };
    let indent = " ".repeat(lead.len() + 4);
    out.push_str(&lead);
    out.push_str(&named.address);
    match (named.space.as_str(), &named.kc_address) {
        ("running", Some(kc)) => out.push_str(&format!(" (running; {kc} in the collection)")),
        ("kernel", Some(kc)) => out.push_str(&format!(" (xnu.macho; {kc} in the collection)")),
        _ => {}
    }
    let Some(member) = &named.member else {
        out.push_str(&format!("  {}\n", named.note.as_deref().unwrap_or("not named")));
        return;
    };
    out.push_str(&format!("  {member} {}  ", named.segment.as_deref().unwrap_or("?")));
    what(named, full_paths, &indent, out);
    if let Some(call) = &named.call_site {
        let mut text = String::new();
        what(call, full_paths, &format!("{indent}  "), &mut text);
        out.push_str(&format!("{indent}the call, at {}: {text}", call.address));
    }
}

/// A named panic block as text.
pub fn render_block(block: &NamedBlock, full_paths: bool, out: &mut String) {
    out.push_str(&format!(
        "panic at {}line {}{}: {}\n",
        block.log.as_ref().map(|l| format!("{l} ")).unwrap_or_default(),
        block.line,
        block.cpu.map(|c| format!(" (cpu {c})")).unwrap_or_default(),
        block.text
    ));
    if let (Some(slide), Some(ram), Some(base)) = (&block.slide, &block.ram_base, &block.link_base) {
        out.push_str(&format!("  running address = collection address + slide {slide} (RAM {ram} - link base {base})\n"));
    } else if let Some(slide) = &block.slide {
        out.push_str(&format!("  running address = collection address + slide {slide}\n"));
    }
    for warning in &block.warnings {
        out.push_str(&format!("  WARNING: {warning}\n"));
    }
    for named in &block.frames {
        render_named(named, true, full_paths, out);
    }
}

/// The head of a report. `with_index`: with the line that says when the index was built (it differs between the
/// call that builds it and the next one).
fn head(session: &Session, with_index: bool, out: &mut String) {
    for note in session.notes.iter().filter(|n| with_index || !n.starts_with("index ")) {
        out.push_str(note);
        out.push('\n');
    }
    for warning in &session.warnings {
        out.push_str(&format!("WARNING: {warning}\n"));
    }
}

/// The kernel panics of a log, named, as text: for `oneios-tool log`. `None`: the log has no panic. Without a
/// collection the panics are listed with the reason they are not named.
pub fn panics_report(text: &str, log: &str, loc: &Locate, slide: Option<u64>, err: &mut dyn Write) -> Option<String> {
    let blocks = panic::blocks(text);
    if blocks.is_empty() {
        return None;
    }
    let mut out = String::new();
    match open(loc, err) {
        Ok(session) => {
            head(&session, false, &mut out);
            for block in &blocks {
                out.push('\n');
                render_block(&name_block(&session.index, block, Some(log), slide, true), false, &mut out);
            }
        }
        Err(e) => {
            out.push_str(&format!("kernel addresses are not named: {e:#}\n"));
            for block in &blocks {
                out.push_str(&format!("\npanic at {log} line {}: {}\n", block.line, block.text));
                for entry in &block.entries {
                    out.push_str(&format!("  {:<7}{:#x}\n", entry.kind.label(), entry.addr));
                }
            }
        }
    }
    Some(out)
}

#[derive(serde::Serialize)]
struct Report {
    kc: FileId,
    #[serde(skip_serializing_if = "Option::is_none")]
    kernel: Option<FileId>,
    dwarf: String,
    index: String,
    notes: Vec<String>,
    warnings: Vec<String>,
    panics: Vec<NamedBlock>,
    addresses: Vec<Named>,
}

fn parse_hex(text: &str) -> Option<u64> {
    let digits = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")).unwrap_or(text);
    if digits.is_empty() || digits.len() > 16 {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

fn describe(session: &Session, out: &mut String) {
    let index = &session.index;
    let meta = &index.meta;
    head(session, true, out);
    out.push_str(&format!(
        "  {} members, {} symbols ({} of the kernel); {} runs of equal file:line and inlined frames over {} addresses asked, {} with DWARF\n",
        meta.members.len(),
        index.symbol_count(),
        meta.kernel_member.map(|k| meta.members[k].symbols).unwrap_or(0),
        meta.runs,
        meta.dwarf.probed,
        meta.dwarf.answered
    ));
    for mv in meta.moves.iter().filter(|mv| mv.vmsize != 0) {
        out.push_str(&format!(
            "  {:<18} {:#x} in the collection, {:#x} in the kernel: moved by {:#x}  (size {:#x})\n",
            mv.name,
            mv.kc_vmaddr,
            mv.kernel_vmaddr,
            mv.kc_vmaddr.wrapping_sub(mv.kernel_vmaddr),
            mv.vmsize
        ));
    }
    for name in ["index.bin", "meta.json"] {
        let len = std::fs::metadata(index.dir.join(name)).map(|m| m.len()).unwrap_or(0);
        out.push_str(&format!("  {name:<10} {len:>12} bytes  ({:.1} MiB)\n", len as f64 / 1048576.0));
    }
}

/// Run `oneios-tool sym kernel ARGS...`; `args` are the arguments after `kernel`. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    let cli = match <Cli as clap::Parser>::try_parse_from(std::iter::once("oneios-tool sym kernel".to_string()).chain(args.iter().cloned())) {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { 1 } else { 0 };
        }
    };
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let status = match run_cli(cli, Locate::from_env(), &mut std::io::stdin().lock(), &mut std::io::stderr()) {
        Ok(text) => {
            // A closed pipe (`| head`) is not an error.
            let _ = out.write_all(text.as_bytes());
            0
        }
        Err(e) => {
            eprintln!("oneios-tool sym kernel: {e:#}");
            1
        }
    };
    let _ = out.flush();
    status
}

/// The command's whole output (the tests call this).
pub fn run_cli(cli: Cli, mut loc: Locate, input: &mut dyn Read, err: &mut dyn Write) -> Result<String> {
    let started = std::time::Instant::now();
    if cli.kc.is_some() {
        loc.kc = cli.kc.clone();
    }
    if cli.kernel.is_some() {
        loc.kernel = cli.kernel.clone();
    }
    if cli.index.is_some() {
        loc.index_parent = cli.index.clone();
    }
    loc.dsym = cli.dsym.clone();
    loc.no_kernel = cli.no_kernel;
    loc.rehash = cli.rehash;
    loc.jobs = cli.jobs;
    let json = match cli.format.as_str() {
        "text" => false,
        "json" => true,
        other => bail!("-f {other}: text or json"),
    };
    let slide = cli.slide.as_deref().map(|s| parse_hex(s).ok_or_else(|| anyhow!("--slide {s}: not a hex number"))).transpose()?;

    let build_only = cli.words.first().is_some_and(|w| w == "index");
    loc.force = cli.force && build_only;
    // The words: logs and addresses.
    let mut logs: Vec<(String, String)> = Vec::new();
    let mut addrs: Vec<u64> = Vec::new();
    for word in cli.words.iter().skip(build_only as usize) {
        if word == "-" {
            let mut text = String::new();
            input.read_to_string(&mut text).context("standard input")?;
            logs.push(("standard input".to_string(), text));
        } else if !word.starts_with("0x") && Path::new(word).is_file() {
            let bytes = std::fs::read(word).with_context(|| word.to_string())?;
            logs.push((word.clone(), String::from_utf8_lossy(&bytes).into_owned()));
        } else {
            addrs.push(parse_hex(word).ok_or_else(|| anyhow!("{word}: neither a file nor a hex address"))?);
        }
    }
    if cli.stdin {
        let mut text = String::new();
        input.read_to_string(&mut text).context("standard input")?;
        for word in text.split_whitespace() {
            addrs.push(parse_hex(word).ok_or_else(|| anyhow!("{word}: not a hex address"))?);
        }
    }

    let session = open(&loc, err)?;
    let index = &session.index;
    if build_only {
        let mut out = String::new();
        describe(&session, &mut out);
        return Ok(out);
    }
    if logs.is_empty() && addrs.is_empty() && cli.symbol.is_empty() {
        bail!("nothing to name: give an xnuhost log, addresses, or --symbol NAME (see --help)");
    }

    let call_sites = !cli.no_call_sites;
    let mut panics = Vec::new();
    let mut warnings = session.warnings.clone();
    for (name, text) in &logs {
        let blocks = panic::blocks(text);
        if blocks.is_empty() {
            warnings.push(format!("{name}: no `panic(cpu N caller 0x...)` line"));
        }
        for block in &blocks {
            panics.push(name_block(index, block, Some(name), slide, call_sites));
        }
    }
    // Addresses given by hand take the slide given by hand, else the one of the log's last boot.
    let hand_slide = slide.or_else(|| logs.iter().rev().find_map(|(_, text)| panic::last_slide(text)));
    let reading = Reading { slide: hand_slide, xnu: cli.xnu, call_sites };
    let kind = if cli.returns { Kind::Frame } else { Kind::Given };
    let mut addresses: Vec<Named> = addrs
        .iter()
        .map(|&addr| {
            let mut named = name(index, kind, addr, None, reading);
            named.kind = Kind::Given.label().to_string();
            named
        })
        .collect();
    for wanted in &cli.symbol {
        let found = index.find_symbol(wanted);
        if found.is_empty() {
            addresses.push(Named {
                kind: "symbol".into(),
                address: wanted.clone(),
                space: "outside".into(),
                note: Some("no symbol of this name in the index".into()),
                ..Named::default()
            });
        }
        for (addr, _, _) in found {
            let mut named = name(index, Kind::Given, addr, None, Reading { slide: hand_slide, xnu: false, call_sites: false });
            named.kind = "symbol".into();
            addresses.push(named);
        }
    }

    let count = panics.iter().map(|p| p.frames.len()).sum::<usize>() + addresses.len();
    let out = if json {
        let report = Report {
            kc: index.meta.kc.clone(),
            kernel: index.meta.kernel.clone(),
            dwarf: index.meta.dwarf.describe(),
            index: index.dir.display().to_string(),
            notes: session.notes.clone(),
            warnings,
            panics,
            addresses,
        };
        serde_json::to_string_pretty(&report)? + "\n"
    } else {
        let mut out = String::new();
        for note in &session.notes {
            out.push_str(note);
            out.push('\n');
        }
        for warning in &warnings {
            out.push_str(&format!("WARNING: {warning}\n"));
        }
        for block in &panics {
            out.push('\n');
            render_block(block, cli.full_paths, &mut out);
        }
        if !addresses.is_empty() {
            out.push('\n');
        }
        for named in &addresses {
            let mut line = String::new();
            render_named(named, false, cli.full_paths, &mut line);
            // Addresses given by hand: say where the place is in the other files too.
            if let (Some(kernel), Some(first)) = (&named.kernel_address, line.find('\n')) {
                let running = hand_slide.zip(named.kc_address.as_deref().and_then(parse_hex)).map(|(s, kc)| format!(", running {:#x}", kc.wrapping_add(s))).unwrap_or_default();
                line.insert_str(first, &format!("  [xnu.macho {kernel}{running}]"));
            }
            out.push_str(&line);
        }
        out
    };
    let _ = writeln!(
        err,
        "oneios-tool sym kernel: {count} address(es) named in {:.0} ms{}",
        started.elapsed().as_secs_f64() * 1000.0,
        session.built.map(|s| format!(" (of which the index build {s:.1} s)")).unwrap_or_default()
    );
    Ok(out)
}
