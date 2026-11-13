//! File, line and inlined frames of every instruction of the unprelinked kernel, read once for the index.
//!
//! All DWARF reading is `addr2line`'s (on `gimli`): `Loader::find_frames` per instruction address. The DWARF is
//! taken from where it is, in this order:
//!  1. a dSYM named with `--dsym`;
//!  2. the kernel file's own `__DWARF` sections;
//!  3. a `.dSYM` in the kernel's folder whose UUID is the kernel's;
//!  4. the kernel's debug map: its `N_OSO` stabs name the object files (or archive members) that hold the DWARF of
//!     each function, as the linker leaves it when no dsymutil ran (the Linux build). An object file whose time is
//!     not the one the stab recorded was rebuilt since the link: its functions get no lines, and the count is said.
//!
//! This file's own part is the walk: which addresses to ask for, in how many threads, and the runs of equal
//! answers. It asks for every 4-byte step of the kernel's code (2.5 million addresses), which is what makes the
//! index independent of the object files afterwards (the next kernel build replaces them).

use anyhow::{anyhow, bail, Context, Result};
use object::read::macho::{MachOFile64, Nlist as _};
use object::{Object, ObjectSection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One frame of an answer: a function and the place in it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FrameRec {
    /// The function's name as the DWARF has it (C++ names mangled). Empty: the DWARF has lines but no function.
    pub function: String,
    /// The source file's path. Empty: not known.
    pub file: String,
    /// 0: not known (the compiler's own code has line 0).
    pub line: u32,
}

/// No DWARF for the addresses from this run on.
pub const NONE: u32 = u32::MAX;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Source {
    /// `dsym`, `kernel`, `debug-map` or `none`.
    pub kind: String,
    /// The dSYM's DWARF file; empty otherwise.
    pub path: String,
    /// The debug map: object files named, read, rebuilt since the link, not there.
    pub objects: u32,
    pub read: u32,
    pub stale: u32,
    pub missing: u32,
    /// An example of a stale and of a missing object file.
    pub example: String,
    /// Addresses asked for, and those that got at least a line.
    pub probed: u64,
    pub answered: u64,
}

impl Source {
    pub fn describe(&self) -> String {
        match self.kind.as_str() {
            "dsym" => format!("the dSYM {}", self.path),
            "kernel" => "the kernel's own __DWARF sections".to_string(),
            "debug-map" => {
                let mut text = format!("the kernel's debug map: {} of {} object files read", self.read, self.objects);
                if self.stale != 0 {
                    text += &format!(", {} rebuilt since the link (no lines for their functions)", self.stale);
                }
                if self.missing != 0 {
                    text += &format!(", {} not there", self.missing);
                }
                text
            }
            _ => "none (no __DWARF sections, no dSYM beside the kernel, no debug map): function names only; with a dSYM, --dsym PATH".to_string(),
        }
    }
}

/// The walk's result. `runs` is sorted by address; a run's answer holds up to the next run's address.
#[derive(Default)]
pub struct Lines {
    /// (address in the unprelinked kernel, index into `chains` or NONE).
    pub runs: Vec<(u64, u32)>,
    /// Frames of `frames`, the outermost function first: the last one is where the address itself is, the ones
    /// before it are the functions it was inlined into.
    pub chains: Vec<Vec<u32>>,
    pub frames: Vec<FrameRec>,
    pub source: Source,
}

/// The DWARF file inside a dSYM bundle (or the file itself when `path` is one).
fn dsym_file(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    let dir = path.join("Contents/Resources/DWARF");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    files.sort();
    files.into_iter().next()
}

fn uuid_of(path: &Path) -> Option<[u8; 16]> {
    let map = oneios_common::Map::open(path).ok()?;
    object::File::parse(&*map).ok()?.mach_uuid().ok()?
}

/// A `.dSYM` in the kernel's folder with the kernel's UUID.
fn dsym_beside(kernel: &Path, uuid: [u8; 16]) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(kernel.parent()?)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "dSYM"))
        .filter_map(|p| dsym_file(&p))
        .filter(|p| uuid_of(p) == Some(uuid))
        .collect();
    found.sort();
    found.into_iter().next()
}

/// The object files of the debug map, in the order `object`'s `ObjectMap` numbers them, each with the time its
/// `N_OSO` stab recorded.
fn oso_times(file: &MachOFile64<'_, object::LittleEndian>) -> Vec<u64> {
    let table = file.macho_symbol_table();
    let strings = table.strings();
    let endian = file.endian();
    let mut out = Vec::new();
    for nlist in table.iter() {
        if nlist.n_type() == object::macho::N_OSO && nlist.name(endian, strings).is_ok_and(|name| !name.is_empty()) {
            out.push(nlist.n_value(endian));
        }
    }
    out
}

/// Is the object file (or archive member) there, and is its time the recorded one? `Ok(true)`: usable.
/// A recorded time of 0 is not checked (a reproducible link).
fn object_state(path: &[u8], member: Option<&[u8]>, recorded: u64, archives: &mut HashMap<Vec<u8>, Option<HashMap<Vec<u8>, u64>>>) -> std::result::Result<bool, ()> {
    use std::os::unix::ffi::OsStrExt;
    let file = Path::new(std::ffi::OsStr::from_bytes(path));
    let Some(member) = member else {
        let meta = std::fs::metadata(file).map_err(|_| ())?;
        let seconds = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        return Ok(recorded == 0 || seconds == recorded);
    };
    let members = archives.entry(path.to_vec()).or_insert_with(|| {
        let map = oneios_common::Map::open(file).ok()?;
        let archive = object::read::archive::ArchiveFile::parse(&*map).ok()?;
        let mut dates = HashMap::new();
        for entry in archive.members().flatten() {
            dates.entry(entry.name().to_vec()).or_insert(entry.date().unwrap_or(0));
        }
        Some(dates)
    });
    let date = *members.as_ref().ok_or(())?.get(member).ok_or(())?;
    // An archive made with `ar D` has no times: nothing to compare.
    Ok(recorded == 0 || date == 0 || date == recorded)
}

struct Plan {
    /// The file `Loader` opens.
    target: PathBuf,
    /// The address ranges to ask for, sorted, not overlapping.
    ranges: Vec<(u64, u64)>,
    source: Source,
}

fn plan(kernel_path: &Path, data: &[u8], dsym: Option<&Path>) -> Result<Plan> {
    let file = MachOFile64::<object::LittleEndian>::parse(data).map_err(|e| anyhow!("{}: {e}", kernel_path.display()))?;
    let uuid = file.mach_uuid().ok().flatten();
    // The kernel's code: every section whose flags say instructions (`__TEXT_EXEC,__text`, `__KLD,__text`, ...;
    // `object`'s section kind knows `__TEXT,__text` only).
    let instructions = object::macho::S_ATTR_PURE_INSTRUCTIONS | object::macho::S_ATTR_SOME_INSTRUCTIONS;
    let mut code: Vec<(u64, u64)> = file
        .sections()
        .filter(|s| matches!(s.flags(), object::SectionFlags::MachO { flags } if flags & instructions != 0) && s.size() != 0)
        .map(|s| (s.address(), s.address() + s.size()))
        .collect();
    code.sort();
    let mut source = Source::default();

    let named = match dsym {
        Some(path) => {
            let found = dsym_file(path).ok_or_else(|| anyhow!("{}: no DWARF file in it", path.display()))?;
            let theirs = uuid_of(&found);
            if theirs.is_none() || theirs != uuid {
                bail!(
                    "{}: its UUID is {}, the kernel's is {}: not this kernel's dSYM",
                    found.display(),
                    theirs.map(|u| oneios_common::hex(&u)).unwrap_or_else(|| "none".into()),
                    uuid.map(|u| oneios_common::hex(&u)).unwrap_or_else(|| "none".into())
                );
            }
            Some(found)
        }
        None => None,
    };
    let own = file.section_by_name(".debug_info").is_some_and(|s| s.size() != 0);
    let beside = if named.is_none() && !own { uuid.and_then(|u| dsym_beside(kernel_path, u)) } else { None };
    if let Some(path) = named.or(beside) {
        source.kind = "dsym".into();
        source.path = path.display().to_string();
        return Ok(Plan { target: path, ranges: code, source });
    }
    if own {
        source.kind = "kernel".into();
        return Ok(Plan { target: kernel_path.to_path_buf(), ranges: code, source });
    }

    let map = file.object_map();
    if map.objects().is_empty() {
        source.kind = "none".into();
        return Ok(Plan { target: kernel_path.to_path_buf(), ranges: Vec::new(), source });
    }
    source.kind = "debug-map".into();
    source.objects = map.objects().len() as u32;
    let times = oso_times(&file);
    let mut archives = HashMap::new();
    let usable: Vec<bool> = map
        .objects()
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let name = || match o.member() {
                Some(m) => format!("{}({})", String::from_utf8_lossy(o.path()), String::from_utf8_lossy(m)),
                None => String::from_utf8_lossy(o.path()).into_owned(),
            };
            match object_state(o.path(), o.member(), times.get(i).copied().unwrap_or(0), &mut archives) {
                Ok(true) => {
                    source.read += 1;
                    true
                }
                Ok(false) => {
                    source.stale += 1;
                    if source.example.is_empty() {
                        source.example = format!("rebuilt: {}", name());
                    }
                    false
                }
                Err(()) => {
                    source.missing += 1;
                    if source.example.is_empty() {
                        source.example = format!("not there: {}", name());
                    }
                    false
                }
            }
        })
        .collect();
    // The functions of the usable object files (a stab pair N_FUN gives each its size).
    let mut ranges: Vec<(u64, u64)> =
        map.symbols().iter().filter(|s| s.size() != 0 && usable[s.object_index()]).map(|s| (s.address(), s.address() + s.size())).collect();
    ranges.sort();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    Ok(Plan { target: kernel_path.to_path_buf(), ranges: merged, source })
}

/// One thread's share of the walk, with its own tables.
#[derive(Default)]
struct Part {
    runs: Vec<(u64, u32)>,
    chains: Vec<Vec<u32>>,
    frames: Vec<FrameRec>,
    probed: u64,
    answered: u64,
}

/// The instruction size of arm64: the step of the walk.
const STEP: u64 = 4;

fn probe(target: &Path, ranges: &[(u64, u64)]) -> Result<Part> {
    let loader = addr2line::Loader::new(target).map_err(|e| anyhow!("{}: {e}", target.display()))?;
    let mut part = Part::default();
    let mut frame_ids: HashMap<FrameRec, u32> = HashMap::new();
    let mut chain_ids: HashMap<Vec<u32>, u32> = HashMap::new();
    // The answer of the address before: most addresses repeat it.
    let mut last: u32 = NONE;
    let mut last_end = 0u64;
    let mut found: Vec<(String, &str, u32)> = Vec::new();
    for &(start, end) in ranges {
        if start != last_end && last != NONE {
            // A gap after the range before: its last answer ends there.
            part.runs.push((last_end, NONE));
            last = NONE;
        }
        let mut addr = start.next_multiple_of(STEP);
        while addr < end {
            part.probed += 1;
            found.clear();
            if let Ok(mut frames) = loader.find_frames(addr) {
                // addr2line gives the innermost frame first.
                while let Ok(Some(frame)) = frames.next() {
                    let function = match &frame.function {
                        Some(name) => name.raw_name().map(|n| n.into_owned()).unwrap_or_default(),
                        None => String::new(),
                    };
                    let (file, line) = match &frame.location {
                        Some(location) => (location.file.unwrap_or(""), location.line.unwrap_or(0)),
                        None => ("", 0),
                    };
                    found.push((function, file, line));
                }
            }
            let same = match last {
                NONE => found.is_empty(),
                id => {
                    let chain = &part.chains[id as usize];
                    chain.len() == found.len()
                        && chain.iter().zip(found.iter().rev()).all(|(&f, new)| {
                            let old = &part.frames[f as usize];
                            old.line == new.2 && old.function == new.0 && old.file == new.1
                        })
                }
            };
            if !found.is_empty() {
                part.answered += 1;
            }
            if !same {
                last = if found.is_empty() {
                    NONE
                } else {
                    let chain: Vec<u32> = found
                        .iter()
                        .rev()
                        .map(|(function, file, line)| {
                            let rec = FrameRec { function: function.clone(), file: file.to_string(), line: *line };
                            *frame_ids.entry(rec.clone()).or_insert_with(|| {
                                part.frames.push(rec);
                                part.frames.len() as u32 - 1
                            })
                        })
                        .collect();
                    *chain_ids.entry(chain.clone()).or_insert_with(|| {
                        part.chains.push(chain);
                        part.chains.len() as u32 - 1
                    })
                };
                part.runs.push((addr, last));
            }
            addr += STEP;
        }
        last_end = end;
    }
    if last != NONE {
        part.runs.push((last_end, NONE));
    }
    Ok(part)
}

/// `ranges` cut into `jobs` shares of about equal size, in address order.
fn shares(ranges: &[(u64, u64)], jobs: usize) -> Vec<Vec<(u64, u64)>> {
    let total: u64 = ranges.iter().map(|r| r.1 - r.0).sum();
    let each = (total / jobs.max(1) as u64).max(STEP).next_multiple_of(STEP);
    let mut out = vec![Vec::new()];
    let mut room = each;
    for &(mut start, end) in ranges {
        while start < end {
            if room == 0 && out.len() < jobs {
                out.push(Vec::new());
                room = each;
            }
            let take = if out.len() == jobs { end - start } else { room.min(end - start) };
            out.last_mut().unwrap().push((start, start + take));
            start += take;
            room = room.saturating_sub(take);
        }
    }
    out
}

/// A C++ name demangled (`cpp_demangle`, through addr2line); any other name as it is. `name` is a DWARF linkage
/// name or a symbol without Mach-O's own leading underscore: `_ZN9IOService5startEPS_`, and for a block's function
/// `___ZN9IOService5startEPS__block_invoke.21` ("invocation function for block in ...", as LLVM writes it).
pub fn demangle(name: &str) -> String {
    let rest = name.trim_start_matches('_');
    if !name.starts_with('_') || !rest.starts_with('Z') {
        return name.to_string();
    }
    let mangled = format!("_{rest}");
    let cpp = |text: &str| addr2line::demangle(text, addr2line::gimli::DW_LANG_C_plus_plus);
    if let Some(at) = mangled.find("_block_invoke") {
        if let Some(inner) = cpp(&mangled[..at]) {
            return format!("invocation function for block in {inner}");
        }
    }
    cpp(&mangled).unwrap_or_else(|| name.to_string())
}

/// Read file, line and inlined frames of the whole kernel. `jobs` threads, each with its own `Loader`.
pub fn walk(kernel_path: &Path, data: &[u8], dsym: Option<&Path>, jobs: usize) -> Result<Lines> {
    let plan = plan(kernel_path, data, dsym)?;
    let mut lines = Lines { source: plan.source, ..Lines::default() };
    if plan.ranges.is_empty() {
        return Ok(lines);
    }
    let parts: Vec<Result<Part>> = std::thread::scope(|scope| {
        let handles: Vec<_> = shares(&plan.ranges, jobs.max(1))
            .into_iter()
            .map(|share| {
                let target = &plan.target;
                scope.spawn(move || probe(target, &share))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(anyhow!("a DWARF thread panicked")))).collect()
    });
    let mut frame_ids: HashMap<FrameRec, u32> = HashMap::new();
    let mut chain_ids: HashMap<Vec<u32>, u32> = HashMap::new();
    for part in parts {
        let part = part.context("reading the DWARF")?;
        lines.source.probed += part.probed;
        lines.source.answered += part.answered;
        let frames: Vec<u32> = part
            .frames
            .into_iter()
            .map(|rec| {
                *frame_ids.entry(rec.clone()).or_insert_with(|| {
                    lines.frames.push(rec);
                    lines.frames.len() as u32 - 1
                })
            })
            .collect();
        let chains: Vec<u32> = part
            .chains
            .into_iter()
            .map(|chain| {
                let chain: Vec<u32> = chain.into_iter().map(|f| frames[f as usize]).collect();
                *chain_ids.entry(chain.clone()).or_insert_with(|| {
                    lines.chains.push(chain);
                    lines.chains.len() as u32 - 1
                })
            })
            .collect();
        for (addr, chain) in part.runs {
            let chain = if chain == NONE { NONE } else { chains[chain as usize] };
            // Two shares meet inside a run: the second one's first answer may repeat the first one's last.
            match lines.runs.last() {
                Some(&(at, _)) if at == addr => {
                    lines.runs.pop();
                    if lines.runs.last().is_none_or(|l| l.1 != chain) {
                        lines.runs.push((addr, chain));
                    }
                }
                Some(&(_, before)) if before == chain => {}
                _ => lines.runs.push((addr, chain)),
            }
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_cover_the_ranges_in_order() {
        let ranges = [(0x1000, 0x1100), (0x2000, 0x2010), (0x3000, 0x3400)];
        for jobs in 1..=5 {
            let got = shares(&ranges, jobs);
            assert!(got.len() <= jobs);
            let flat: Vec<(u64, u64)> = got.iter().flatten().copied().collect();
            let bytes: u64 = flat.iter().map(|r| r.1 - r.0).sum();
            assert_eq!(bytes, 0x100 + 0x10 + 0x400);
            assert!(flat.windows(2).all(|w| w[0].1 <= w[1].0));
            assert!(flat.iter().all(|r| r.0 % STEP == 0));
        }
    }

    #[test]
    fn names() {
        assert_eq!(demangle("tcp_input"), "tcp_input");
        assert_eq!(demangle("_ZN9IOService5startEPS_"), "IOService::start(IOService*)");
        assert_eq!(demangle("___ZN9IOService5startEPS__block_invoke.21"), "invocation function for block in IOService::start(IOService*)");
        // C names that only look mangled stay.
        assert_eq!(demangle("_ZSTD_compress"), "_ZSTD_compress");
        assert_eq!(demangle("ZSTD_compress"), "ZSTD_compress");
        assert_eq!(demangle("__ubsan_handle_mul_overflow_minimal"), "__ubsan_handle_mul_overflow_minimal");
    }
}
