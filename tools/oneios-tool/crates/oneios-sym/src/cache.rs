//! The dyld shared cache, read in place: the port of `scripts/debug/dsc.py`'s `Cache` class. Every method keeps the
//! Python's behaviour (which mapping an address falls in, which symbols an image has, how they sort), so the index
//! built from it and the direct path both answer as dsc.py does.

use anyhow::{anyhow, bail, Context, Result};
use oneios_common::Map;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// The name of the main cache file in the cache's folder.
pub const MAIN_NAME: &str = "dyld_shared_cache_arm64e";
/// The main file's names: arm64e (iOS 27 of the iPhone18,2) first, then arm64 (iOS 16.5 of the iPhone X: the Galaxy A56's
/// image, docs/a56-gpu.md).
pub const MAIN_NAMES: [&str; 2] = [MAIN_NAME, "dyld_shared_cache_arm64"];

/// The main file's name in `dir`: the first of MAIN_NAMES that is there (MAIN_NAME when none is).
pub fn main_name(dir: &Path) -> &'static str {
    MAIN_NAMES.iter().copied().find(|n| dir.join(n).is_file()).unwrap_or(MAIN_NAME)
}

/// dsc.py's `is_call`: bl, blr, or an authenticated blr (blraaz, blrabz, blraa, blrab).
#[inline]
pub fn is_call(word: u32) -> bool {
    (word & 0xfc00_0000) == 0x9400_0000 || (word & 0xffff_fc1f) == 0xd63f_0000 || (word & 0xfeff_f800) == 0xd63f_0800
}

pub fn u32_at(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn u64_at(data: &[u8], off: usize) -> Option<u64> {
    let b = data.get(off..off.checked_add(8)?)?;
    Some(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

/// The bytes from `off` to the next NUL (to the end of the data when there is none).
pub fn cstr_at(data: &[u8], off: usize) -> &[u8] {
    let tail = data.get(off..).unwrap_or(&[]);
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    &tail[..end]
}

/// The UUID of a cache: 16 bytes at 0x58 of the main file's header. Reads only the header.
pub fn read_uuid(dir: &Path) -> Result<[u8; 16]> {
    use std::io::Read;
    let path = dir.join(main_name(dir));
    let mut head = [0u8; 0x68];
    let mut file = std::fs::File::open(&path).with_context(|| format!("{}", path.display()))?;
    file.read_exact(&mut head).with_context(|| format!("{}: the header", path.display()))?;
    if &head[..7] != b"dyld_v1" {
        bail!("{}: not a dyld shared cache", path.display());
    }
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&head[0x58..0x68]);
    Ok(uuid)
}

/// One mapping of a cache file, as dsc.py keeps it: (vmaddr, size, file, file offset).
#[derive(Clone, Copy, Debug)]
pub struct Mapping {
    pub addr: u64,
    pub size: u64,
    pub file: usize,
    pub foff: u64,
    /// The mapping's protections (1 read, 2 write, 4 execute): the most it may get, and what it starts with.
    pub max_prot: u32,
    pub init_prot: u32,
}

/// One `__TEXT` segment of an image: `[start, end)` and the image's number. Sorted as dsc.py's tuples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextRange {
    pub start: u64,
    pub end: u64,
    pub image: u32,
}

pub struct Cache {
    pub dir: PathBuf,
    /// MAIN_NAMES[0] or [1]: which the folder has.
    pub main_name: &'static str,
    /// File 0 is the main file; the subcaches follow in the header's order.
    pub files: Vec<Map>,
    pub file_names: Vec<String>,
    pub uuid: [u8; 16],
    pub base: u64,
    pub size: u64,
    pub max_slide: u64,
    /// Sorted by address (a stable sort, as dsc.py's).
    pub maps: Vec<Mapping>,
    /// (address, path) of every image.
    pub images: Vec<(u64, String)>,
    symbols_file: Option<Map>,
    /// The subcache files the header names that are not in the folder (a partial copy: `apfs-copy` of only the
    /// files a question needs, tools/oneios-tool/README.md "sym"). Empty for a whole cache.
    pub missing: Vec<String>,
}

/// The subcache file names the main file's header lists, in its order (the main file not included).
fn subcache_names(main: &[u8], main_name: &str, what: &Path) -> Result<Vec<String>> {
    let bad = || anyhow!("{}: the header is cut", what.display());
    let sub_off = u32_at(main, 0x188).ok_or_else(bad)? as usize;
    let sub_count = u32_at(main, 0x18C).ok_or_else(bad)? as usize;
    let mut names = Vec::with_capacity(sub_count);
    for i in 0..sub_count {
        let entry = main.get(sub_off + 56 * i..sub_off + 56 * (i + 1)).ok_or_else(bad)?;
        names.push(format!("{main_name}{}", String::from_utf8_lossy(cstr_at(&entry[24..], 0))));
    }
    Ok(names)
}

/// The index folder's name: the cache's UUID in hex; for a partial copy, `<uuid>.partial-<8 hex digits>` (FNV-1a of
/// the missing files' names), so that an index of a partial copy is never taken for the whole cache's, nor for a
/// copy with other files.
fn index_name_of(uuid: &[u8; 16], missing: &[String]) -> String {
    let name = oneios_common::hex(uuid);
    if missing.is_empty() {
        return name;
    }
    let mut h: u32 = 0x811c_9dc5;
    for b in missing.iter().flat_map(|m| m.bytes().chain([0])) {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    format!("{name}.partial-{h:08x}")
}

/// The index folder's name of the cache in `dir`, read from the main file's header and the folder's files only:
/// the UUID in hex, or `<uuid>.partial-XXXXXXXX` when subcache files are missing ([`Cache::missing`]).
pub fn index_name(dir: &Path) -> Result<String> {
    let uuid = read_uuid(dir)?;
    let main_name = main_name(dir);
    let path = dir.join(main_name);
    let main = Map::open(&path)?;
    let missing: Vec<String> = subcache_names(&main, main_name, &path)?.into_iter().filter(|n| !dir.join(n).exists()).collect();
    Ok(index_name_of(&uuid, &missing))
}

impl Cache {
    pub fn open(dir: &Path) -> Result<Cache> {
        let main_name = main_name(dir);
        let main_path = dir.join(main_name);
        let main = Map::open(&main_path)?;
        let bad = || anyhow!("{}: the header is cut", main_path.display());
        if main.get(..7) != Some(b"dyld_v1".as_slice()) {
            bail!("{}: not a dyld shared cache", main_path.display());
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(main.get(0x58..0x68).ok_or_else(bad)?);
        let base = u64_at(&main, 0xE0).ok_or_else(bad)?;
        let size = u64_at(&main, 0xE8).ok_or_else(bad)?;
        let max_slide = u64_at(&main, 0xF0).ok_or_else(bad)?;
        let img_off = u32_at(&main, 0x1C0).ok_or_else(bad)? as usize;
        let img_count = u32_at(&main, 0x1C4).ok_or_else(bad)? as usize;

        let mut names = vec![main_name.to_string()];
        names.extend(subcache_names(&main, main_name, &main_path)?);
        let mut images = Vec::with_capacity(img_count);
        for i in 0..img_count {
            let at = img_off + 32 * i;
            let addr = u64_at(&main, at).ok_or_else(bad)?;
            let path_off = u32_at(&main, at + 24).ok_or_else(bad)? as usize;
            images.push((addr, String::from_utf8_lossy(cstr_at(&main, path_off)).into_owned()));
        }
        let mut files = vec![main];
        let mut missing = Vec::new();
        for name in &names[1..] {
            let path = dir.join(name);
            if path.exists() {
                files.push(Map::open(path)?);
            } else {
                // A partial copy (only the subcaches a question needs): no mappings, its images are left out.
                missing.push(name.clone());
                files.push(Map::empty());
            }
        }
        let mut maps = Vec::new();
        for (index, file) in files.iter().enumerate() {
            if missing.contains(&names[index]) {
                continue;
            }
            let cut = || anyhow!("{}: the mappings are cut", names[index]);
            let off = u32_at(file, 0x10).ok_or_else(cut)? as usize;
            let count = u32_at(file, 0x14).ok_or_else(cut)? as usize;
            for i in 0..count {
                let at = off + 32 * i;
                maps.push(Mapping {
                    addr: u64_at(file, at).ok_or_else(cut)?,
                    size: u64_at(file, at + 8).ok_or_else(cut)?,
                    file: index,
                    foff: u64_at(file, at + 16).ok_or_else(cut)?,
                    max_prot: u32_at(file, at + 24).ok_or_else(cut)?,
                    init_prot: u32_at(file, at + 28).ok_or_else(cut)?,
                });
            }
        }
        maps.sort_by_key(|m| m.addr);
        let symbols_path = dir.join(format!("{main_name}.symbols"));
        let symbols_file = if symbols_path.exists() { Some(Map::open(&symbols_path)?) } else { None };
        Ok(Cache { dir: dir.to_path_buf(), main_name, files, file_names: names, uuid, base, size, max_slide, maps, images, symbols_file, missing })
    }

    /// The name of this cache's index folder ([`index_name`]).
    pub fn index_name(&self) -> String {
        index_name_of(&self.uuid, &self.missing)
    }

    /// dsc.py's `where`: the last mapping that starts at or below `va`, when `va` is inside it.
    pub fn where_(&self, va: u64) -> Option<(usize, u64)> {
        let i = self.maps.partition_point(|m| m.addr <= va);
        let m = self.maps.get(i.checked_sub(1)?)?;
        if va - m.addr < m.size {
            Some((m.file, m.foff.checked_add(va - m.addr)?))
        } else {
            None
        }
    }

    /// dsc.py's `read`: up to `n` bytes of the file that maps `va` (shorter at the file's end, like a Python slice).
    pub fn read(&self, va: u64, n: usize) -> Option<&[u8]> {
        let (file, off) = self.where_(va)?;
        let data: &[u8] = &self.files[file];
        let start = usize::try_from(off).ok()?.min(data.len());
        let end = start.saturating_add(n).min(data.len());
        Some(&data[start..end])
    }

    /// The 32-bit word at `va`: None when nothing maps it (or the file ends inside the word).
    pub fn word(&self, va: u64) -> Option<u32> {
        u32_at(self.read(va, 4)?, 0)
    }

    /// The address ranges `where_` answers for, each with its file and file offset: mapping i holds
    /// `[addr, min(addr + size, the next mapping's addr))`.
    pub fn effective_maps(&self) -> Vec<Mapping> {
        let mut out = Vec::new();
        for (i, m) in self.maps.iter().enumerate() {
            let mut end = m.addr.saturating_add(m.size);
            if let Some(next) = self.maps.get(i + 1) {
                end = end.min(next.addr);
            }
            if end > m.addr {
                out.push(Mapping { size: end - m.addr, ..*m });
            }
        }
        out
    }

    /// The load commands of the image at `addr`: (cmd, the command's bytes).
    pub fn commands(&self, addr: u64) -> Result<Vec<(u32, &[u8])>> {
        let cut = || anyhow!("the Mach-O header at {addr:#x} is not in the cache");
        let head = self.read(addr, 32).ok_or_else(cut)?;
        let ncmds = u32_at(head, 16).ok_or_else(cut)?;
        let mut at = addr + 32;
        let mut out = Vec::new();
        for _ in 0..ncmds {
            let cut = || anyhow!("a load command of the image at {addr:#x} is cut");
            let head = self.read(at, 8).ok_or_else(cut)?;
            let cmd = u32_at(head, 0).ok_or_else(cut)?;
            let size = u32_at(head, 4).ok_or_else(cut)?;
            if size < 8 {
                bail!("a load command of the image at {addr:#x} has size {size}");
            }
            out.push((cmd, self.read(at, size as usize).ok_or_else(cut)?));
            at += size as u64;
        }
        Ok(out)
    }

    /// The `__TEXT` segments of every image, sorted as dsc.py's `text_ranges` (it never reads the pickle here).
    pub fn text_ranges(&self) -> Result<Vec<TextRange>> {
        use rayon::prelude::*;
        let per_image: Vec<Result<Vec<TextRange>>> = (0..self.images.len())
            .into_par_iter()
            .map(|index| {
                let mut out = Vec::new();
                if !self.missing.is_empty() && self.read(self.images[index].0, 32).is_none_or(|h| h.len() < 32) {
                    return Ok(out); // a partial copy: this image's header is in a subcache that is not here
                }
                for (cmd, data) in self.commands(self.images[index].0)? {
                    if cmd == 0x19 && data.len() >= 40 && cstr_at(&data[8..24], 0) == b"__TEXT" {
                        let vmaddr = u64_at(data, 24).unwrap_or(0);
                        let vmsize = u64_at(data, 32).unwrap_or(0);
                        out.push(TextRange { start: vmaddr, end: vmaddr.wrapping_add(vmsize), image: index as u32 });
                    }
                }
                Ok(out)
            })
            .collect();
        let mut out = Vec::new();
        for (index, ranges) in per_image.into_iter().enumerate() {
            out.extend(ranges.with_context(|| format!("image {} ({})", index, self.images[index].1))?);
        }
        out.sort();
        Ok(out)
    }

    /// dsc.py's `symbols` before the sort: (value, the name's bytes) from the image's own symbol table (defined
    /// symbols in a section) and from the cache's local symbols (the `.symbols` file).
    pub fn raw_symbols(&self, index: usize) -> Result<Vec<(u64, &[u8])>> {
        let addr = self.images[index].0;
        let mut out = Vec::new();
        let mut linkedit: Option<usize> = None;
        for (cmd, data) in self.commands(addr)? {
            if cmd == 0x19 && data.len() >= 56 && cstr_at(&data[8..24], 0) == b"__LINKEDIT" {
                linkedit = self.where_(u64_at(data, 24).unwrap_or(0)).map(|w| w.0);
            }
            if cmd == 0x2 && data.len() >= 24 {
                let Some(file) = linkedit else { continue };
                let m: &[u8] = &self.files[file];
                let symoff = u32_at(data, 8).unwrap_or(0) as usize;
                let nsyms = u32_at(data, 12).unwrap_or(0) as usize;
                let stroff = u32_at(data, 16).unwrap_or(0) as usize;
                for i in 0..nsyms {
                    let at = symoff + 16 * i;
                    let Some(entry) = m.get(at..at + 16) else {
                        bail!("{}: the symbol table of image {index} runs past the file", self.file_names[file]);
                    };
                    let strx = u32_at(entry, 0).unwrap_or(0) as usize;
                    let kind = entry[4];
                    let value = u64_at(entry, 8).unwrap_or(0);
                    if value != 0 && (kind & 0x0e) == 0x0e {
                        out.push((value, cstr_at(m, stroff + strx)));
                    }
                }
            }
        }
        let sym: &[u8] = self
            .symbols_file
            .as_ref()
            .ok_or_else(|| anyhow!("{}: no {}.symbols file", self.dir.display(), self.main_name))?;
        let cut = || anyhow!("{}.symbols: the local symbols are cut", self.main_name);
        let local_off = usize::try_from(u64_at(sym, 0x48).ok_or_else(cut)?).map_err(|_| cut())?;
        let field = |i: usize| u32_at(sym, local_off + 4 * i).map(|v| v as usize).ok_or_else(cut);
        let (nlist_off, str_off, ent_off, ent_count) = (field(0)?, field(2)?, field(4)?, field(5)?);
        let wanted = addr.wrapping_sub(self.base);
        for i in 0..ent_count {
            let at = local_off + ent_off + 16 * i;
            if u64_at(sym, at).ok_or_else(cut)? != wanted {
                continue;
            }
            let start = u32_at(sym, at + 8).ok_or_else(cut)? as usize;
            let count = u32_at(sym, at + 12).ok_or_else(cut)? as usize;
            for k in start..start + count {
                let entry = local_off + nlist_off + 16 * k;
                let strx = u32_at(sym, entry).ok_or_else(cut)? as usize;
                let value = u64_at(sym, entry + 8).ok_or_else(cut)?;
                if value != 0 {
                    out.push((value, cstr_at(sym, local_off + str_off + strx)));
                }
            }
            break;
        }
        Ok(out)
    }

    /// dsc.py's `symbols`: (value, name) sorted by value, then by the name's text.
    pub fn symbols(&self, index: usize) -> Result<Vec<(u64, Cow<'_, str>)>> {
        let mut out: Vec<(u64, Cow<str>)> =
            self.raw_symbols(index)?.into_iter().map(|(v, n)| (v, String::from_utf8_lossy(n))).collect();
        out.sort();
        Ok(out)
    }
}

/// The target of a `b` or `bl` at `va`: (target, true for bl), None for any other instruction.
pub fn branch_target(word: u32, va: u64) -> Option<(u64, bool)> {
    let link = match word & 0xfc00_0000 {
        0x9400_0000 => true,
        0x1400_0000 => false,
        _ => return None,
    };
    let imm = ((word & 0x03ff_ffff) << 6) as i32 >> 6; // sign-extended imm26
    Some((va.wrapping_add_signed(i64::from(imm) * 4), link))
}

impl Cache {
    /// Every `b` and `bl` in an image's `__TEXT` whose target is one of `targets` (unslid): (site, target, bl?),
    /// sorted by site. `sym callers`.
    pub fn callers(&self, targets: &[u64]) -> Result<Vec<(u64, u64, bool)>> {
        use rayon::prelude::*;
        let want: std::collections::HashSet<u64> = targets.iter().copied().collect();
        let ranges = self.text_ranges()?;
        let mut out: Vec<(u64, u64, bool)> = ranges
            .par_iter()
            .flat_map_iter(|r| {
                let mut found = Vec::new();
                let mut va = r.start & !3;
                while va < r.end {
                    // Read the range one mapping piece at a time (a range never needs more than its file holds).
                    let Some(bytes) = self.read(va, (r.end - va).min(1 << 20) as usize) else { break };
                    if bytes.len() < 4 {
                        break;
                    }
                    for (k, w) in bytes.chunks_exact(4).enumerate() {
                        let site = va + 4 * k as u64;
                        let word = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                        if let Some((t, link)) = branch_target(word, site) {
                            if want.contains(&t) {
                                found.push((site, t, link));
                            }
                        }
                    }
                    va += (bytes.len() & !3) as u64;
                }
                found
            })
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }
}

/// adrp at `va`: (its register, the page it forms).
pub fn adrp_page(word: u32, va: u64) -> Option<(u32, u64)> {
    if word & 0x9f00_0000 != 0x9000_0000 {
        return None;
    }
    let lo = u64::from((word >> 29) & 3);
    let hi = u64::from((word >> 5) & 0x7_ffff);
    let imm = (((hi << 2 | lo) << 43) as i64) >> 43; // sign-extended 21 bits
    Some((word & 31, (va & !0xfff).wrapping_add_signed(imm << 12)))
}

/// What an instruction after an adrp makes of its page: an `add Xd, Xn, #imm` (64-bit, either shift) or a load or
/// store of a general register with an unsigned immediate (ldr, str, ldrb, ldrh, ...: the offset scaled by the size).
/// (the base register Xn, the address).
pub fn page_use(word: u32, page: u64) -> Option<(u32, u64)> {
    let rn = (word >> 5) & 31;
    let imm12 = u64::from((word >> 10) & 0xfff);
    if word & 0xff80_0000 == 0x9100_0000 {
        let shift = if word & (1 << 22) != 0 { 12 } else { 0 };
        return Some((rn, page.wrapping_add(imm12 << shift)));
    }
    if word & 0x3f00_0000 == 0x3900_0000 {
        return Some((rn, page.wrapping_add(imm12 << (word >> 30))));
    }
    None
}

/// How far after an adrp `refs` and `dis` look for the instruction that uses its page.
pub const PAGE_USE_WINDOW: usize = 8;

impl Cache {
    /// Every adrp + add/ldr/str pair in an image's `__TEXT` that forms one of `targets` (unslid): (the adrp, the
    /// instruction that uses its page, the target), sorted. The use is looked for in the next PAGE_USE_WINDOW
    /// instructions (the compilers keep the pair close). `sym refs`.
    pub fn refs(&self, targets: &[u64]) -> Result<Vec<(u64, u64, u64)>> {
        use rayon::prelude::*;
        let want: std::collections::HashSet<u64> = targets.iter().copied().collect();
        let pages: std::collections::HashSet<u64> = targets.iter().map(|t| t & !0xfff).collect();
        let ranges = self.text_ranges()?;
        let mut out: Vec<(u64, u64, u64)> = ranges
            .par_iter()
            .flat_map_iter(|r| {
                let mut found = Vec::new();
                let start = r.start & !3;
                let Some(bytes) = self.read(start, (r.end - start) as usize) else { return found };
                let words: Vec<u32> =
                    bytes.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
                for (k, &w) in words.iter().enumerate() {
                    let site = start + 4 * k as u64;
                    let Some((rd, page)) = adrp_page(w, site) else { continue };
                    if !pages.contains(&page) {
                        continue;
                    }
                    for (j, &u) in words.iter().enumerate().skip(k + 1).take(PAGE_USE_WINDOW) {
                        if let Some((rn, addr)) = page_use(u, page) {
                            if rn == rd && want.contains(&addr) {
                                found.push((site, start + 4 * j as u64, addr));
                            }
                        }
                    }
                }
                found
            })
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Every address of the cache where `text` stands as a whole C string (a NUL, or a mapping's start, before it, a
    /// NUL after it). `sym cstr`. Sorted.
    pub fn cstr(&self, text: &[u8]) -> Vec<u64> {
        use rayon::prelude::*;
        let mut needle = text.to_vec();
        needle.push(0);
        let n = needle.len();
        const CHUNK: usize = 64 << 20;
        let mut out: Vec<u64> = self
            .effective_maps()
            .par_iter()
            .flat_map_iter(|m| {
                let data: &[u8] = &self.files[m.file];
                let start = (m.foff as usize).min(data.len());
                let end = start.saturating_add(m.size as usize).min(data.len());
                let piece = &data[start..end];
                let mut found = Vec::new();
                let mut at = 0;
                while at < piece.len() {
                    let stop = (at + CHUNK + n).min(piece.len());
                    for (k, w) in piece[at..stop].windows(n).enumerate() {
                        let i = at + k;
                        if i >= at + CHUNK {
                            break;
                        }
                        if w[0] == needle[0] && w == needle.as_slice() && (i == 0 || piece[i - 1] == 0) {
                            found.push(m.addr + i as u64);
                        }
                    }
                    at += CHUNK;
                }
                found
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The C string at `va` when it is one: printable ASCII up to a NUL, 1 to 120 bytes.
    pub fn string_at(&self, va: u64) -> Option<String> {
        let bytes = self.read(va, 121)?;
        let len = bytes.iter().position(|&b| b == 0)?;
        let s = &bytes[..len];
        (len > 0 && s.iter().all(|&b| (0x20..0x7f).contains(&b))).then(|| String::from_utf8_lossy(s).into_owned())
    }
}

/// The address a pointer word of the cache holds: its low 36 bits (the rest are a chained fixup's or PAC's bits).
pub fn pointer_target(word: u64) -> u64 {
    word & 0x0000_000f_ffff_ffff
}

/// The address a pointer word holds in an arm64e cache whose pointers are offsets from the cache's base
/// (DYLD_CHAINED_PTR_ARM64E_SHARED_CACHE, iOS 27's: 34 bits of runtime offset; the rest are PAC's and the chain's).
/// None when the offset is past the cache.
pub fn pointer_offset_target(word: u64, base: u64, size: u64) -> Option<u64> {
    let offset = word & 0x0000_0003_ffff_ffff;
    (offset < size).then(|| base + offset)
}

impl Cache {
    /// Every 8-byte aligned word of the cache's mappings that points at one of `targets` (pointer_target, or
    /// pointer_offset_target: a word may hold either form): (where, target), sorted. `sym ptrs`: a
    /// selector's references (__objc_selrefs), a class's, a string's.
    pub fn ptrs(&self, targets: &[u64]) -> Vec<(u64, u64)> {
        use rayon::prelude::*;
        let want: std::collections::HashSet<u64> = targets.iter().copied().collect();
        let mut out: Vec<(u64, u64)> = self
            .effective_maps()
            .par_iter()
            .flat_map_iter(|m| {
                let data: &[u8] = &self.files[m.file];
                let start = (m.foff as usize).min(data.len());
                let end = start.saturating_add(m.size as usize).min(data.len());
                let skip = (8 - (m.addr % 8) as usize) % 8;
                let piece = &data[(start + skip).min(end)..end];
                piece
                    .chunks_exact(8)
                    .enumerate()
                    .filter_map(|(k, w)| {
                        let word = u64::from_le_bytes(w.try_into().unwrap());
                        let t = pointer_target(word);
                        let t = if want.contains(&t) {
                            Some(t)
                        } else {
                            pointer_offset_target(word, self.base, self.size).filter(|o| want.contains(o))
                        };
                        t.map(|t| (m.addr + skip as u64 + 8 * k as u64, t))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }
}
