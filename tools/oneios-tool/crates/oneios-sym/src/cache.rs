//! The dyld shared cache, read in place: the port of `scripts/debug/dsc.py`'s `Cache` class. Every method keeps the
//! Python's behaviour (which mapping an address falls in, which symbols an image has, how they sort), so the index
//! built from it and the direct path both answer as dsc.py does.

use anyhow::{anyhow, bail, Context, Result};
use oneios_common::Map;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// The name of the main cache file in the cache's folder.
pub const MAIN_NAME: &str = "dyld_shared_cache_arm64e";

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
    let path = dir.join(MAIN_NAME);
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
}

impl Cache {
    pub fn open(dir: &Path) -> Result<Cache> {
        let main_path = dir.join(MAIN_NAME);
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
        let sub_off = u32_at(&main, 0x188).ok_or_else(bad)? as usize;
        let sub_count = u32_at(&main, 0x18C).ok_or_else(bad)? as usize;
        let img_off = u32_at(&main, 0x1C0).ok_or_else(bad)? as usize;
        let img_count = u32_at(&main, 0x1C4).ok_or_else(bad)? as usize;

        let mut names = vec![MAIN_NAME.to_string()];
        for i in 0..sub_count {
            let entry = main.get(sub_off + 56 * i..sub_off + 56 * (i + 1)).ok_or_else(bad)?;
            let suffix = String::from_utf8_lossy(cstr_at(&entry[24..], 0)).into_owned();
            names.push(format!("{MAIN_NAME}{suffix}"));
        }
        let mut images = Vec::with_capacity(img_count);
        for i in 0..img_count {
            let at = img_off + 32 * i;
            let addr = u64_at(&main, at).ok_or_else(bad)?;
            let path_off = u32_at(&main, at + 24).ok_or_else(bad)? as usize;
            images.push((addr, String::from_utf8_lossy(cstr_at(&main, path_off)).into_owned()));
        }
        let mut files = vec![main];
        for name in &names[1..] {
            files.push(Map::open(dir.join(name))?);
        }
        let mut maps = Vec::new();
        for (index, file) in files.iter().enumerate() {
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
        let symbols_path = dir.join(format!("{MAIN_NAME}.symbols"));
        let symbols_file = if symbols_path.exists() { Some(Map::open(&symbols_path)?) } else { None };
        Ok(Cache { dir: dir.to_path_buf(), files, file_names: names, uuid, base, size, max_slide, maps, images, symbols_file })
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
            .ok_or_else(|| anyhow!("{}: no {MAIN_NAME}.symbols file", self.dir.display()))?;
        let cut = || anyhow!("{MAIN_NAME}.symbols: the local symbols are cut");
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
