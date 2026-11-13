//! The index of one collection and its unprelinked kernel: `out/ksym-index/<kernel.kc SHA-256>/<xnu.macho
//! SHA-256>/` (`kc-only` in place of the second when no kernel was given).
//!
//! - `meta.json`: the two files and their hashes, the collection's members and segments, each kernel segment's
//!   move, where the DWARF came from, counts. Written last: an index without it is not finished.
//! - `index.bin`: five tables, mapped and searched in place:
//!   - symbols: 16 bytes each, sorted by address: address in the collection, name, member
//!   - runs: 12 bytes each, sorted by address: address in the unprelinked kernel, chain (or none)
//!   - chains: 32-bit words: a count, then that many frames, the outermost function first
//!   - frames: 12 bytes each: function, file, line
//!   - strings: NUL-terminated
//!
//! The project's own format (as `oneios-sym`'s index is): it answers from two binary searches, without reading
//! the 93 MB collection, the kernel or any object file again. What it holds was read by `object` and `addr2line`.

use crate::dwarf::{FrameRec, Lines, Source, NONE};
use crate::fileset::{self, Image, Locator, Moved, Segment};
use anyhow::{bail, Context, Result};
use oneios_common::Map;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const VERSION: u32 = 1;
const MAGIC: &[u8; 8] = b"ONEKSYM\0";
const TABLES: usize = 5;
const HEADER: usize = 16 + TABLES * 16;

/// A file the index was built from.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileId {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MetaMember {
    pub name: String,
    pub vmaddr: u64,
    pub segments: Vec<Segment>,
    /// Symbols of this member in the index.
    pub symbols: u32,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub version: u32,
    pub kc: FileId,
    pub kernel: Option<FileId>,
    pub fileset: bool,
    pub link_base: u64,
    pub end: u64,
    /// The collection's own segments.
    pub segments: Vec<Segment>,
    pub members: Vec<MetaMember>,
    /// Index of the kernel in `members`.
    pub kernel_member: Option<usize>,
    /// The kernel's segments, each with its own move (empty without an unprelinked kernel).
    pub moves: Vec<Moved>,
    /// Kernel segments whose size in the collection is not the size in the unprelinked kernel.
    pub resized: Vec<String>,
    /// The collection keeps the kernel's symbols: how many, and how many of them the unprelinked kernel's symbol of
    /// the same name, moved with its segment, lands on. All of them when the two files belong together.
    pub own_symbols: u32,
    pub own_agree: u32,
    pub dwarf: Source,
    pub runs: u64,
    pub seconds: f64,
}

/// Where the index of a pair of files is.
pub fn dir_of(parent: &Path, kc_sha: &str, kernel_sha: Option<&str>) -> PathBuf {
    parent.join(kc_sha).join(kernel_sha.unwrap_or("kc-only"))
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[derive(Default)]
struct Strings {
    blob: Vec<u8>,
    seen: HashMap<String, u32>,
}

impl Strings {
    fn add(&mut self, text: &str) -> u32 {
        if let Some(&at) = self.seen.get(text) {
            return at;
        }
        let at = self.blob.len() as u32;
        self.blob.extend_from_slice(text.as_bytes());
        self.blob.push(0);
        self.seen.insert(text.to_string(), at);
        at
    }
}

/// What `build` is given.
pub struct Input<'a> {
    pub kc: FileId,
    pub kc_image: &'a Image,
    /// The unprelinked kernel, parsed with its symbols.
    pub kernel: Option<(FileId, &'a Image)>,
    pub lines: Lines,
    pub seconds: f64,
}

/// Write the index of `input` into `dir`.
pub fn build(dir: &Path, input: Input) -> Result<Meta> {
    let image = input.kc_image;
    let kernel_member = if image.fileset { image.members.iter().position(|m| m.name == fileset::KERNEL_MEMBER) } else { Some(0) };
    let unprelinked = input.kernel.as_ref().and_then(|(_, k)| k.kernel());
    let moves = match (kernel_member, unprelinked) {
        (Some(k), Some(theirs)) => fileset::moves(&image.members[k], theirs),
        _ => Vec::new(),
    };
    // __LINKEDIT is the prelink's own (it writes the member's symbols and fixups anew): its size says nothing.
    let resized: Vec<String> =
        moves.iter().filter(|m| m.name != "__LINKEDIT" && m.kernel_vmsize != image_segment_size(image, kernel_member, &m.name)).map(|m| m.name.clone()).collect();

    // The symbols, in the collection's addresses: every member's own, but the kernel's from the unprelinked kernel
    // (each moved with its segment) when there is one.
    let mut strings = Strings::default();
    strings.add("");
    let mut symbols: Vec<(u64, u32, u32, bool)> = Vec::new();
    let mut counts = vec![0u32; image.members.len()];
    let (mut own_symbols, mut own_agree) = (0u32, 0u32);
    for (m, member) in image.members.iter().enumerate() {
        match (Some(m) == kernel_member, unprelinked) {
            (true, Some(theirs)) => {
                let mut moved: std::collections::HashSet<(&str, u64)> = std::collections::HashSet::new();
                for symbol in &theirs.symbols {
                    // The symbol moves with the segment of its section.
                    let segment = symbol.segment.and_then(|i| theirs.segments.get(i as usize));
                    let Some(mv) = segment.and_then(|segment| moves.iter().find(|mv| mv.name == segment.name)) else { continue };
                    let addr = mv.to_kc(symbol.addr);
                    moved.insert((symbol.name.as_str(), addr));
                    symbols.push((addr, strings.add(&symbol.name), m as u32, symbol.external));
                    counts[m] += 1;
                }
                for symbol in &member.symbols {
                    own_symbols += 1;
                    if moved.contains(&(symbol.name.as_str(), symbol.addr)) {
                        own_agree += 1;
                    }
                }
            }
            _ => {
                for symbol in &member.symbols {
                    symbols.push((symbol.addr, strings.add(&symbol.name), m as u32, symbol.external));
                    counts[m] += 1;
                }
            }
        }
    }
    // By address; of several names of one address the external ones last, in the table's order: `resolve` takes
    // the last (an end label such as LowResetVectorEnd comes before the function that starts there).
    symbols.sort_by_key(|s| (s.0, s.3));

    let mut tables: [Vec<u8>; TABLES] = Default::default();
    for &(addr, name, member, _) in &symbols {
        put_u64(&mut tables[0], addr);
        put_u32(&mut tables[0], name);
        put_u32(&mut tables[0], member);
    }
    let lines = input.lines;
    let frames: Vec<(u32, u32, u32)> = lines.frames.iter().map(|f: &FrameRec| (strings.add(&f.function), strings.add(&f.file), f.line)).collect();
    let mut chain_at = Vec::with_capacity(lines.chains.len());
    for chain in &lines.chains {
        chain_at.push((tables[2].len() / 4) as u32);
        put_u32(&mut tables[2], chain.len() as u32);
        for &frame in chain {
            put_u32(&mut tables[2], frame);
        }
    }
    for &(addr, chain) in &lines.runs {
        put_u64(&mut tables[1], addr);
        put_u32(&mut tables[1], if chain == NONE { NONE } else { chain_at[chain as usize] });
    }
    for (function, file, line) in frames {
        put_u32(&mut tables[3], function);
        put_u32(&mut tables[3], file);
        put_u32(&mut tables[3], line);
    }
    tables[4] = strings.blob;

    let mut out = Vec::with_capacity(HEADER + tables.iter().map(Vec::len).sum::<usize>());
    out.extend_from_slice(MAGIC);
    put_u32(&mut out, VERSION);
    put_u32(&mut out, TABLES as u32);
    let mut at = HEADER as u64;
    for table in &tables {
        put_u64(&mut out, at);
        put_u64(&mut out, table.len() as u64);
        at += table.len() as u64;
    }
    for table in &tables {
        out.extend_from_slice(table);
    }

    let meta = Meta {
        version: VERSION,
        kc: input.kc,
        kernel: input.kernel.map(|(id, _)| id),
        fileset: image.fileset,
        link_base: image.link_base(),
        end: image.end(),
        segments: image.segments.clone(),
        members: image
            .members
            .iter()
            .zip(&counts)
            .map(|(m, &symbols)| MetaMember { name: m.name.clone(), vmaddr: m.vmaddr, segments: m.segments.clone(), symbols })
            .collect(),
        kernel_member,
        moves,
        resized,
        own_symbols,
        own_agree,
        dwarf: lines.source,
        runs: lines.runs.len() as u64,
        seconds: input.seconds,
    };
    std::fs::create_dir_all(dir).with_context(|| format!("{}", dir.display()))?;
    let _ = std::fs::remove_file(dir.join("meta.json"));
    oneios_common::atomic_write(dir.join("index.bin"), &out)?;
    oneios_common::atomic_write(dir.join("meta.json"), serde_json::to_string_pretty(&meta)?.as_bytes())?;
    Ok(meta)
}

fn image_segment_size(image: &Image, member: Option<usize>, name: &str) -> u64 {
    member.and_then(|m| image.members[m].segments.iter().find(|s| s.name == name && s.vmsize != 0)).map(|s| s.vmsize).unwrap_or(0)
}

/// One source frame of an answer.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Frame {
    /// The function's name, C++ demangled.
    pub function: String,
    /// The name as the DWARF has it, when `function` is not it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_function: Option<String>,
    pub file: String,
    pub line: u32,
}

/// What the index knows about an address of the collection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answer {
    /// The member and the segment that hold the address.
    pub member: Option<(String, String)>,
    /// Offset of the address in that segment.
    pub segment_offset: u64,
    /// The address is in the kernel member.
    pub is_kernel: bool,
    /// The same place in the unprelinked kernel.
    pub kernel_addr: Option<u64>,
    /// The nearest symbol at or below the address in the same member and segment, as the symbol table has it, and
    /// the offset from it.
    pub symbol: Option<(String, u64)>,
    /// The source frames, the outermost function first; the last one is where the address is.
    pub frames: Vec<Frame>,
}

pub struct Index {
    pub dir: PathBuf,
    pub meta: Meta,
    map: Map,
    tables: [(usize, usize); TABLES],
    locator: Locator,
}

impl Index {
    /// Is there a finished index of these two files in `dir`?
    pub fn is_current(dir: &Path, kc_sha: &str, kernel_sha: Option<&str>) -> bool {
        let Ok(text) = std::fs::read_to_string(dir.join("meta.json")) else { return false };
        let Ok(meta) = serde_json::from_str::<Meta>(&text) else { return false };
        meta.version == VERSION && meta.kc.sha256 == kc_sha && meta.kernel.as_ref().map(|k| k.sha256.as_str()) == kernel_sha && dir.join("index.bin").is_file()
    }

    pub fn open(dir: &Path) -> Result<Index> {
        let text = std::fs::read_to_string(dir.join("meta.json")).with_context(|| format!("{}", dir.join("meta.json").display()))?;
        let meta: Meta = serde_json::from_str(&text).with_context(|| format!("{}", dir.join("meta.json").display()))?;
        if meta.version != VERSION {
            bail!("{}: index version {}, this tool reads {VERSION}", dir.display(), meta.version);
        }
        let map = Map::open(dir.join("index.bin"))?;
        if map.len() < HEADER || &map[..8] != MAGIC || u32::from_le_bytes(map[8..12].try_into().unwrap()) != VERSION {
            bail!("{}: not an index of this version", dir.join("index.bin").display());
        }
        let mut tables = [(0usize, 0usize); TABLES];
        for (i, table) in tables.iter_mut().enumerate() {
            let at = 16 + i * 16;
            let start = u64::from_le_bytes(map[at..at + 8].try_into().unwrap()) as usize;
            let len = u64::from_le_bytes(map[at + 8..at + 16].try_into().unwrap()) as usize;
            if start.checked_add(len).is_none_or(|end| end > map.len()) {
                bail!("{}: a table ends after the file", dir.join("index.bin").display());
            }
            *table = (start, len);
        }
        let locator = Locator::new(&meta.members.iter().map(|m| (m.name.clone(), m.vmaddr, m.segments.clone())).collect::<Vec<_>>());
        Ok(Index { dir: dir.to_path_buf(), meta, map, tables, locator })
    }

    fn table(&self, i: usize) -> &[u8] {
        let (start, len) = self.tables[i];
        &self.map[start..start + len]
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    fn u64_at(bytes: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
    }

    fn string(&self, at: u32) -> &str {
        let blob = self.table(4);
        let Some(rest) = blob.get(at as usize..) else { return "" };
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        std::str::from_utf8(&rest[..end]).unwrap_or("")
    }

    pub fn symbol_count(&self) -> usize {
        self.tables[0].1 / 16
    }

    fn symbol(&self, i: usize) -> (u64, u32, u32) {
        let t = self.table(0);
        (Self::u64_at(t, i * 16), Self::u32_at(t, i * 16 + 8), Self::u32_at(t, i * 16 + 12))
    }

    /// The number of entries of a table whose address is at most `addr` (entries of `width` bytes, the address
    /// first).
    fn upto(table: &[u8], width: usize, addr: u64) -> usize {
        let (mut low, mut high) = (0, table.len() / width);
        while low < high {
            let mid = (low + high) / 2;
            if Self::u64_at(table, mid * width) <= addr {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    }

    /// The source frames of an address of the unprelinked kernel, the outermost function first.
    pub fn frames(&self, kernel_addr: u64) -> Vec<Frame> {
        let runs = self.table(1);
        let Some(i) = Self::upto(runs, 12, kernel_addr).checked_sub(1) else { return Vec::new() };
        let chain = Self::u32_at(runs, i * 12 + 8);
        if chain == NONE {
            return Vec::new();
        }
        let chains = self.table(2);
        let frames = self.table(3);
        let at = chain as usize * 4;
        if at + 4 > chains.len() {
            return Vec::new();
        }
        let count = Self::u32_at(chains, at) as usize;
        let mut out = Vec::with_capacity(count);
        for k in 0..count {
            let Some(word) = chains.get(at + 4 + k * 4..at + 8 + k * 4) else { break };
            let frame = u32::from_le_bytes(word.try_into().unwrap()) as usize * 12;
            if frame + 12 > frames.len() {
                break;
            }
            let raw = self.string(Self::u32_at(frames, frame));
            let function = crate::dwarf::demangle(raw);
            out.push(Frame {
                raw_function: (function != raw).then(|| raw.to_string()),
                function,
                file: self.string(Self::u32_at(frames, frame + 4)).to_string(),
                line: Self::u32_at(frames, frame + 8),
            });
        }
        out
    }

    /// Is the address inside the collection (one of the file's own segments)?
    pub fn holds(&self, addr: u64) -> bool {
        self.meta.segments.iter().any(|s| s.holds(addr)) || self.locator.find(addr).is_some()
    }

    /// The collection's own segment that holds an address.
    pub fn top_segment(&self, addr: u64) -> Option<&str> {
        self.meta.segments.iter().find(|s| s.holds(addr)).map(|s| s.name.as_str())
    }

    /// The kernel segment's move that covers an address of the collection.
    fn moved(&self, kc_addr: u64) -> Option<&Moved> {
        self.meta.moves.iter().find(|m| m.holds_kc(kc_addr))
    }

    /// The address in the collection of an address of the unprelinked kernel.
    pub fn to_kc(&self, kernel_addr: u64) -> Option<u64> {
        self.meta.moves.iter().find(|m| m.holds_kernel(kernel_addr)).map(|m| m.to_kc(kernel_addr))
    }

    /// Name an address of the collection.
    pub fn resolve(&self, addr: u64) -> Answer {
        let mut answer = Answer::default();
        let Some((m, s)) = self.locator.find(addr) else { return answer };
        let member = &self.meta.members[m];
        let segment = &member.segments[s];
        answer.member = Some((member.name.clone(), segment.name.clone()));
        answer.segment_offset = addr - segment.vmaddr;
        answer.is_kernel = Some(m) == self.meta.kernel_member;
        if answer.is_kernel {
            if let Some(mv) = self.moved(addr) {
                let kernel_addr = mv.to_kernel(addr);
                answer.kernel_addr = Some(kernel_addr);
                answer.frames = self.frames(kernel_addr);
            }
        }
        // The nearest symbol below, of this member, inside this segment. Other members' symbols may sit between.
        let mut i = Self::upto(self.table(0), 16, addr);
        while i > 0 {
            i -= 1;
            let (at, name, owner) = self.symbol(i);
            if at < segment.vmaddr {
                break;
            }
            if owner as usize != m {
                continue;
            }
            // Several names of one address: the one the DWARF gives the function, else the last (an external one;
            // the sort put them last).
            let mut best = name;
            if let Some(wanted) = answer.frames.first().map(|f| f.raw_function.as_deref().unwrap_or(&f.function)).filter(|w| !w.is_empty()) {
                let mut k = i + 1;
                while k > 0 {
                    k -= 1;
                    let (other, other_name, other_owner) = self.symbol(k);
                    if other != at {
                        break;
                    }
                    if other_owner as usize == m && self.string(other_name).strip_prefix('_') == Some(wanted) {
                        best = other_name;
                        break;
                    }
                }
            }
            answer.symbol = Some((self.string(best).to_string(), addr - at));
            break;
        }
        answer
    }

    /// The addresses in the collection of the symbols with this name (as the table has it, or without the
    /// table's leading underscore), each with its member.
    pub fn find_symbol(&self, name: &str) -> Vec<(u64, String, String)> {
        let mut out = Vec::new();
        for i in 0..self.symbol_count() {
            let (addr, at, member) = self.symbol(i);
            let raw = self.string(at);
            if raw == name || raw.strip_prefix('_') == Some(name) {
                out.push((addr, raw.to_string(), self.meta.members[member as usize].name.clone()));
            }
        }
        out
    }
}
