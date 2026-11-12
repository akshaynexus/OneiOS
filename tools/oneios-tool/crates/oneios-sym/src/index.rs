//! The persistent index of one dyld shared cache: what `sym` needs to answer without the cache.
//!
//! A folder `<index parent>/<cache UUID as hex>/` with four flat little-endian files, mapped and searched in place:
//!
//! `meta.bin`   a 128-byte header (below), then the arrays
//!              mappings  n x (addr u64, size u64, max_prot u32, init_prot u32)   the address ranges the cache maps
//!              ranges    n x (start u64, end u64, image u32, 0 u32)    every image's `__TEXT`, sorted
//!              images    n x (addr u64, path_off u32, path_len u32, sym_start u64, sym_count u64)
//!              paths     the images' paths, one after the other
//! `syms.bin`   n x (value u64, name_off u64): per image a run sorted by value, one entry per value (the name that
//!              dsc.py's `bisect_right` picks: the last of the sorted `(value, name)` tuples with that value). Only
//!              the entries an address in the image's `__TEXT` can get: the values below the image's last `__TEXT`
//!              end, and of those below its first `__TEXT` start only the highest.
//! `names.bin`  the names, each ended by a NUL, already decoded as Python's `decode(errors='replace')` (UTF-8).
//! `calls.bin`  "the word before this address is a call instruction", one bit per 4-byte word of every mapping,
//!              stored by the word's place in its 16 KiB page: row `o` (0..4096) holds one bit per page
//!              (`row_words` u64 each; bit `p` is the page `page_base + p`). A slide is a multiple of 16 KiB, so
//!              all the slide candidates of one address are neighbours in one row: a few KiB read per address.
//!
//! Header: magic "ONEIOSDX", version u32, 0 u32, uuid [16], base u64, size u64, max_slide u64, page_base u64,
//! pages u64, row_words u64, n_mappings u64, n_ranges u64, n_images u64, n_syms u64, names_len u64, paths_len u64.

use crate::cache::{is_call, u32_at, u64_at, Cache, TextRange};
use anyhow::{anyhow, bail, Context, Result};
use oneios_common::Map;
use rayon::prelude::*;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const MAGIC: &[u8; 8] = b"ONEIOSDX";
pub const VERSION: u32 = 2;
const HEADER: usize = 128;
/// dsc.py's slide step, and the page size the call bitmap is laid out by.
pub const SLIDE_STEP: u64 = 0x4000;
const ROWS: usize = (SLIDE_STEP / 4) as usize;

pub fn uuid_hex(uuid: &[u8; 16]) -> String {
    oneios_common::hex(uuid)
}

/// What a slide search found: dsc.py's `best` and `ties`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slide {
    pub slide: u64,
    /// Addresses that follow a call at this slide (-1 when the cache has no slide candidates).
    pub score: i64,
    /// Addresses inside the shared region plus the largest slide.
    pub inside: usize,
    /// Slides with this score; more than one means the answer is not reliable.
    pub ties: usize,
}

/// The name of an address: the image's file name, and the nearest symbol at or below it with the offset from it
/// (None when the image has no symbol at or below the address; dsc.py prints `?`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name<'a> {
    pub image: &'a str,
    pub symbol: Option<(&'a str, u64)>,
}

/// The sizes `sym index` prints.
pub struct BuildReport {
    pub dir: PathBuf,
    pub images: usize,
    pub symbols_read: usize,
    pub symbols_kept: usize,
    pub files: Vec<(&'static str, u64)>,
    pub seconds: f64,
    /// Seconds of the stages: the images' load commands; the call bitmap and the symbols (at the same time, on
    /// one thread pool); writing the files.
    pub stages: [f64; 3],
}

pub struct Index {
    pub dir: PathBuf,
    meta: Map,
    syms: Map,
    names: Map,
    calls: Map,
    pub uuid: [u8; 16],
    pub base: u64,
    pub size: u64,
    pub max_slide: u64,
    page_base: u64,
    pages: u64,
    row_words: usize,
    n_mappings: usize,
    n_ranges: usize,
    n_images: usize,
    n_syms: usize,
    ranges_at: usize,
    images_at: usize,
    paths_at: usize,
}

impl Index {
    /// True when `dir` holds an index of this format version.
    pub fn is_current(dir: &Path) -> bool {
        use std::io::Read;
        let mut head = [0u8; 12];
        match std::fs::File::open(dir.join("meta.bin")) {
            Ok(mut f) => f.read_exact(&mut head).is_ok() && &head[..8] == MAGIC && u32_at(&head, 8) == Some(VERSION),
            Err(_) => false,
        }
    }

    pub fn open(dir: &Path) -> Result<Index> {
        let meta = Map::open(dir.join("meta.bin"))?;
        let cut = || anyhow!("{}: meta.bin is cut", dir.display());
        if meta.get(..8) != Some(MAGIC.as_slice()) {
            bail!("{}: not a oneios-tool sym index", dir.display());
        }
        let version = u32_at(&meta, 8).ok_or_else(cut)?;
        if version != VERSION {
            bail!("{}: index format {version}, this tool reads {VERSION}: run `oneios-tool sym index --force`", dir.display());
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(meta.get(16..32).ok_or_else(cut)?);
        let field = |i: usize| u64_at(&meta, 32 + 8 * i).ok_or_else(cut);
        let as_len = |v: u64| usize::try_from(v).map_err(|_| cut());
        let (base, size, max_slide, page_base, pages) = (field(0)?, field(1)?, field(2)?, field(3)?, field(4)?);
        let row_words = as_len(field(5)?)?;
        let n_mappings = as_len(field(6)?)?;
        let n_ranges = as_len(field(7)?)?;
        let n_images = as_len(field(8)?)?;
        let n_syms = as_len(field(9)?)?;
        let names_len = as_len(field(10)?)?;
        let paths_len = as_len(field(11)?)?;
        let ranges_at = HEADER + 24 * n_mappings;
        let images_at = ranges_at + 24 * n_ranges;
        let paths_at = images_at + 32 * n_images;
        if meta.len() < paths_at + paths_len {
            return Err(cut());
        }
        let syms = Map::open(dir.join("syms.bin"))?;
        let names = Map::open(dir.join("names.bin"))?;
        let calls = Map::open(dir.join("calls.bin"))?;
        if syms.len() < 16 * n_syms || names.len() < names_len || calls.len() < ROWS * row_words * 8 {
            bail!("{}: an index file is cut: run `oneios-tool sym index --force`", dir.display());
        }
        Ok(Index {
            dir: dir.to_path_buf(),
            meta,
            syms,
            names,
            calls,
            uuid,
            base,
            size,
            max_slide,
            page_base,
            pages,
            row_words,
            n_mappings,
            n_ranges,
            n_images,
            n_syms,
            ranges_at,
            images_at,
            paths_at,
        })
    }

    pub fn image_count(&self) -> usize {
        self.n_images
    }

    pub fn symbol_count(&self) -> usize {
        self.n_syms
    }

    pub fn mapping_count(&self) -> usize {
        self.n_mappings
    }

    /// Mapping `i`: (address, size, the most protection it may get, the protection it starts with).
    pub fn mapping(&self, i: usize) -> (u64, u64, u32, u32) {
        let at = HEADER + 24 * i;
        (self.meta_u64(at), self.meta_u64(at + 8), u32_at(&self.meta, at + 16).unwrap_or(0), u32_at(&self.meta, at + 20).unwrap_or(0))
    }

    /// The lowest address of the cache that a process may write to: the start of its data.
    pub fn first_writable(&self) -> Option<u64> {
        (0..self.n_mappings).map(|i| self.mapping(i)).filter(|m| m.2 & 2 != 0).map(|m| m.0).min()
    }

    fn meta_u64(&self, at: usize) -> u64 {
        u64_at(&self.meta, at).unwrap_or(0)
    }

    fn range(&self, i: usize) -> TextRange {
        let at = self.ranges_at + 24 * i;
        TextRange {
            start: self.meta_u64(at),
            end: self.meta_u64(at + 8),
            image: u32_at(&self.meta, at + 16).unwrap_or(0),
        }
    }

    /// The path of image `i` in the cache.
    pub fn image_path(&self, i: usize) -> &str {
        let at = self.images_at + 32 * i;
        let off = u32_at(&self.meta, at + 8).unwrap_or(0) as usize;
        let len = u32_at(&self.meta, at + 12).unwrap_or(0) as usize;
        self.meta
            .get(self.paths_at + off..self.paths_at + off + len)
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("")
    }

    fn sym_value(&self, i: usize) -> u64 {
        u64_at(&self.syms, 16 * i).unwrap_or(u64::MAX)
    }

    fn sym_name(&self, i: usize) -> &str {
        let off = u64_at(&self.syms, 16 * i + 8).unwrap_or(u64::MAX);
        let tail = usize::try_from(off).ok().and_then(|o| self.names.get(o..)).unwrap_or(&[]);
        let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
        std::str::from_utf8(&tail[..end]).unwrap_or("")
    }

    /// The image whose `__TEXT` holds the unslid address (dsc.py: `bisect_right` on the range starts).
    pub fn image_of(&self, unslid: u64) -> Option<usize> {
        let (mut lo, mut hi) = (0, self.n_ranges);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.range(mid).start <= unslid {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let r = self.range(lo.checked_sub(1)?);
        (r.start <= unslid && unslid < r.end).then_some(r.image as usize)
    }

    /// Name an unslid address: None when no image's `__TEXT` holds it.
    pub fn name(&self, unslid: u64) -> Option<Name<'_>> {
        let image = self.image_of(unslid)?;
        let at = self.images_at + 32 * image;
        let start = self.meta_u64(at + 16) as usize;
        let count = self.meta_u64(at + 24) as usize;
        let (mut lo, mut hi) = (start, start + count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.sym_value(mid) <= unslid {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let path = self.image_path(image);
        let base = path.rsplit('/').next().unwrap_or(path);
        let symbol = (lo > start).then(|| (self.sym_name(lo - 1), unslid - self.sym_value(lo - 1)));
        Some(Name { image: base, symbol })
    }

    /// dsc.py's `inside` test.
    pub fn is_inside(&self, addr: u64) -> bool {
        addr >= self.base && (addr as u128) < self.base as u128 + self.size as u128 + self.max_slide as u128
    }

    /// The number of slide candidates: `range(0, max_slide, 0x4000)`.
    pub fn candidates(&self) -> usize {
        self.max_slide.div_ceil(SLIDE_STEP) as usize
    }

    /// For every slide candidate (slide = index * 0x4000), how many of the addresses follow a call instruction.
    /// Addresses outside `is_inside` do not count. An address that is not a multiple of 4 cannot be answered
    /// from the bitmap: it adds nothing here and is returned in the second list (the command reads the cache
    /// for those).
    pub fn slide_scores(&self, addrs: &[u64]) -> (Vec<u32>, Vec<u64>) {
        let n = self.candidates();
        let mut scores = vec![0u32; n];
        let mut unaligned = Vec::new();
        if n == 0 || self.pages == 0 {
            return (scores, unaligned);
        }
        for &a in addrs {
            if !self.is_inside(a) || a < 4 {
                continue;
            }
            let va = a - 4;
            if va & 3 != 0 {
                unaligned.push(a);
                continue;
            }
            let row = ((va & (SLIDE_STEP - 1)) >> 2) as usize;
            // Candidate k reads page q - k: bit (q - k - page_base) of the row.
            let q = va / SLIDE_STEP;
            if q < self.page_base {
                continue;
            }
            let top = q - self.page_base; // the bit of candidate 0
            let low = top.saturating_sub(n as u64 - 1).min(self.pages); // the bit of the last candidate
            let high = top.min(self.pages.saturating_sub(1));
            if low > high {
                continue;
            }
            let words = &self.calls[row * self.row_words * 8..(row + 1) * self.row_words * 8];
            for w in (low / 64) as usize..=(high / 64) as usize {
                let mut bits = u64_at(words, 8 * w).unwrap_or(0);
                while bits != 0 {
                    let bit = w as u64 * 64 + bits.trailing_zeros() as u64;
                    bits &= bits - 1;
                    if bit >= low && bit <= high {
                        scores[(top - bit) as usize] += 1;
                    }
                }
            }
        }
        (scores, unaligned)
    }

    /// The slide of a backtrace: the multiple of 0x4000 below max_slide for which the most addresses sit right
    /// after a call instruction (the lowest of equals), as dsc.py finds it. Addresses that are not a multiple of 4
    /// never count here (dsc.py reads the unaligned word; return addresses are aligned).
    pub fn find_slide(&self, addrs: &[u64]) -> Slide {
        let (scores, _) = self.slide_scores(addrs);
        let inside = addrs.iter().filter(|&&a| self.is_inside(a)).count();
        pick_slide(&scores, inside)
    }
}

/// dsc.py's choice among the candidates' scores: the first of the highest, and how many share it.
pub fn pick_slide(scores: &[u32], inside: usize) -> Slide {
    let mut best: i64 = -1;
    let mut at = 0usize;
    let mut ties = 0usize;
    for (k, &s) in scores.iter().enumerate() {
        if s as i64 > best {
            best = s as i64;
            at = k;
            ties = 1;
        } else if s as i64 == best {
            ties += 1;
        }
    }
    Slide { slide: at as u64 * SLIDE_STEP, score: best, inside, ties }
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// The call bitmap of the whole cache: (page_base, pages, row_words, the words: row * row_words + column).
/// This pass reads every byte of every mapping (6 GiB) once: with `read` calls of 1 MiB from many threads, not
/// through the file maps (a page fault per 16 KiB). The symbols, read here and there, go through the maps.
fn build_calls(cache: &Cache) -> Result<(u64, u64, usize, Vec<AtomicU64>)> {
    use std::os::unix::fs::FileExt;
    let maps = cache.effective_maps();
    let lo = maps.iter().map(|m| m.addr).min().unwrap_or(0);
    let hi = maps.iter().map(|m| m.addr + m.size).max().unwrap_or(0);
    let page_base = lo / SLIDE_STEP;
    let pages = hi.div_ceil(SLIDE_STEP) - page_base;
    let row_words = pages.div_ceil(64) as usize;
    let words: Vec<AtomicU64> = (0..ROWS * row_words).map(|_| AtomicU64::new(0)).collect();
    // One piece per mapping and bitmap column (64 pages, 1 MiB of addresses).
    let column_span = 64 * SLIDE_STEP;
    let mut pieces = Vec::new();
    for m in &maps {
        let mut at = m.addr;
        let end = m.addr + m.size;
        while at < end {
            let column = (at / SLIDE_STEP - page_base) / 64;
            let stop = ((page_base * SLIDE_STEP) + (column + 1) * column_span).min(end);
            pieces.push((*m, at, stop, column as usize));
            at = stop;
        }
    }
    let files: Vec<std::fs::File> = cache
        .file_names
        .iter()
        .map(|name| {
            let path = cache.dir.join(name);
            std::fs::File::open(&path).with_context(|| format!("{}", path.display()))
        })
        .collect::<Result<_>>()?;
    let scan = |(buf, local): &mut (Vec<u8>, Vec<u64>), &(m, from, to, column): &(crate::cache::Mapping, u64, u64, usize)| -> Result<()> {
        // The piece's bytes and three more, for a last word that ends after it.
        let want = (to - from) as usize + 3;
        buf.resize(want, 0);
        let mut got = 0;
        while got < want {
            match files[m.file].read_at(&mut buf[got..], m.foff + (from - m.addr) + got as u64) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e).with_context(|| cache.file_names[m.file].clone()),
            }
        }
        let data = &buf[..got];
        local.fill(0);
        let mut any = false;
        let mut va = from.div_ceil(4) * 4;
        while va < to {
            if let Some(word) = u32_at(data, (va - from) as usize) {
                if is_call(word) {
                    let page = va / SLIDE_STEP - page_base;
                    local[((va & (SLIDE_STEP - 1)) >> 2) as usize] |= 1u64 << (page & 63);
                    any = true;
                }
            }
            va += 4;
        }
        if any {
            for (row, &bits) in local.iter().enumerate() {
                if bits != 0 {
                    words[row * row_words + column].fetch_or(bits, Ordering::Relaxed);
                }
            }
        }
        Ok(())
    };
    pieces.par_iter().try_for_each_init(|| (Vec::new(), vec![0u64; ROWS]), scan)?;
    Ok((page_base, pages, row_words, words))
}

struct ImageSyms {
    /// (value, offset in `names`).
    entries: Vec<(u64, u32)>,
    names: Vec<u8>,
    read: usize,
}

/// One image's entries: sorted by value, one name per value, cut to what its `__TEXT` addresses can get.
fn image_syms(cache: &Cache, index: usize, text: Option<(u64, u64)>) -> Result<ImageSyms> {
    let mut out = ImageSyms { entries: Vec::new(), names: Vec::new(), read: 0 };
    let Some((text_start, text_end)) = text else { return Ok(out) };
    let mut raw = cache.raw_symbols(index)?;
    out.read = raw.len();
    raw.retain(|&(value, _)| value < text_end);
    // dsc.py sorts (value, name) with the names decoded; equal values keep their last name.
    raw.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| String::from_utf8_lossy(a.1).as_bytes().cmp(String::from_utf8_lossy(b.1).as_bytes()))
    });
    let mut kept: Vec<(u64, &[u8])> = Vec::with_capacity(raw.len());
    for entry in raw {
        match kept.last_mut() {
            Some(last) if last.0 == entry.0 => *last = entry,
            _ => kept.push(entry),
        }
    }
    let below = kept.partition_point(|&(value, _)| value < text_start);
    for &(value, name) in &kept[below.saturating_sub(1)..] {
        let off = u32::try_from(out.names.len()).map_err(|_| anyhow!("image {index}: over 4 GiB of names"))?;
        out.entries.push((value, off));
        out.names.extend_from_slice(String::from_utf8_lossy(name).as_bytes());
        out.names.push(0);
    }
    Ok(out)
}

fn write_file(path: &Path, parts: &[&[u8]]) -> Result<u64> {
    let file = std::fs::File::create(path).with_context(|| format!("{}", path.display()))?;
    let mut out = std::io::BufWriter::with_capacity(1 << 20, file);
    let mut total = 0u64;
    for part in parts {
        out.write_all(part).with_context(|| format!("{}", path.display()))?;
        total += part.len() as u64;
    }
    out.flush().with_context(|| format!("{}", path.display()))?;
    Ok(total)
}

/// Build the index of `cache` as `parent/<uuid>/` (through a temporary folder, renamed into place) and note it as
/// the latest one in `parent/latest`.
pub fn build(cache: &Cache, parent: &Path) -> Result<BuildReport> {
    let started = std::time::Instant::now();
    let name = uuid_hex(&cache.uuid);
    let dir = parent.join(&name);
    let tmp = parent.join(format!("{name}.tmp{}", std::process::id()));
    std::fs::create_dir_all(&tmp).with_context(|| format!("{}", tmp.display()))?;
    let result = build_into(cache, &tmp);
    let mut report = match result {
        Ok(report) => report,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("{}", dir.display()))?;
    }
    std::fs::rename(&tmp, &dir).with_context(|| format!("{}", dir.display()))?;
    oneios_common::atomic_write(parent.join("latest"), format!("{name}\n").as_bytes())?;
    report.dir = dir;
    report.seconds = started.elapsed().as_secs_f64();
    Ok(report)
}

fn build_into(cache: &Cache, dir: &Path) -> Result<BuildReport> {
    let clock = std::time::Instant::now();
    let ranges = cache.text_ranges()?;
    let ranges_s = clock.elapsed().as_secs_f64();
    let mut text: Vec<Option<(u64, u64)>> = vec![None; cache.images.len()];
    for r in &ranges {
        let slot = &mut text[r.image as usize];
        *slot = Some(match *slot {
            Some((lo, hi)) => (lo.min(r.start), hi.max(r.end)),
            None => (r.start, r.end),
        });
    }
    // The bitmap and the symbols at the same time: both are a parallel pass over the cache's files.
    let (calls, syms) = rayon::join(
        || {
            let clock = std::time::Instant::now();
            build_calls(cache).map(|calls| (calls, clock.elapsed().as_secs_f64()))
        },
        || -> Result<(Vec<ImageSyms>, f64)> {
            let clock = std::time::Instant::now();
            let syms: Result<Vec<ImageSyms>> = (0..cache.images.len())
                .into_par_iter()
                .map(|i| image_syms(cache, i, text[i]).with_context(|| format!("image {i} ({})", cache.images[i].1)))
                .collect();
            Ok((syms?, clock.elapsed().as_secs_f64()))
        },
    );
    let (syms, syms_s) = syms?;
    let (calls, calls_s) = calls?;
    let clock = std::time::Instant::now();
    let (page_base, pages, row_words, words) = calls;

    let effective = cache.effective_maps();
    let mut paths = Vec::new();
    let mut images = Vec::new();
    let mut sym_bytes = Vec::new();
    let mut names_len = 0u64;
    let mut n_syms = 0u64;
    for (i, (addr, path)) in cache.images.iter().enumerate() {
        put_u64(&mut images, *addr);
        put_u32(&mut images, u32::try_from(paths.len()).map_err(|_| anyhow!("the paths are over 4 GiB"))?);
        put_u32(&mut images, path.len() as u32);
        put_u64(&mut images, n_syms);
        put_u64(&mut images, syms[i].entries.len() as u64);
        paths.extend_from_slice(path.as_bytes());
        for &(value, off) in &syms[i].entries {
            put_u64(&mut sym_bytes, value);
            put_u64(&mut sym_bytes, names_len + off as u64);
        }
        n_syms += syms[i].entries.len() as u64;
        names_len += syms[i].names.len() as u64;
    }
    let mut meta = Vec::with_capacity(HEADER);
    meta.extend_from_slice(MAGIC);
    put_u32(&mut meta, VERSION);
    put_u32(&mut meta, 0);
    meta.extend_from_slice(&cache.uuid);
    for v in [
        cache.base,
        cache.size,
        cache.max_slide,
        page_base,
        pages,
        row_words as u64,
        effective.len() as u64,
        ranges.len() as u64,
        cache.images.len() as u64,
        n_syms,
        names_len,
        paths.len() as u64,
    ] {
        put_u64(&mut meta, v);
    }
    debug_assert_eq!(meta.len(), HEADER);
    for m in &effective {
        put_u64(&mut meta, m.addr);
        put_u64(&mut meta, m.size);
        put_u32(&mut meta, m.max_prot);
        put_u32(&mut meta, m.init_prot);
    }
    for r in &ranges {
        put_u64(&mut meta, r.start);
        put_u64(&mut meta, r.end);
        put_u32(&mut meta, r.image);
        put_u32(&mut meta, 0);
    }
    meta.extend_from_slice(&images);
    meta.extend_from_slice(&paths);

    let call_bytes: Vec<u8> = words.iter().flat_map(|w| w.load(Ordering::Relaxed).to_le_bytes()).collect();
    drop(words);
    let name_parts: Vec<&[u8]> = syms.iter().map(|s| s.names.as_slice()).collect();
    let files = vec![
        ("meta.bin", write_file(&dir.join("meta.bin"), &[&meta])?),
        ("syms.bin", write_file(&dir.join("syms.bin"), &[&sym_bytes])?),
        ("names.bin", write_file(&dir.join("names.bin"), &name_parts)?),
        ("calls.bin", write_file(&dir.join("calls.bin"), &[&call_bytes])?),
    ];
    Ok(BuildReport {
        dir: dir.to_path_buf(),
        images: cache.images.len(),
        symbols_read: syms.iter().map(|s| s.read).sum(),
        symbols_kept: n_syms as usize,
        files,
        seconds: 0.0,
        stages: [ranges_s, calls_s.max(syms_s), clock.elapsed().as_secs_f64()],
    })
}
