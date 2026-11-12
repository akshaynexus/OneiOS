//! `prelink emit`: write kernel.kc, the kexts of the kernelcache rebound onto the hosted kernel. A port of
//! kernel/prelink/emit.py and of the parts of XnuPrelink (pin 9b950f2) it drives: fileset_emit.py
//! (`emit_images`, `rebase_collection`, the chain reader), fileset_segmented.py (`SegmentedEmit`) and
//! fileset_layout.py (layout.rs). The same kernel.kc and kernel.kc.prelink.json byte for byte, the same stdout.
//! emit.py's header and kernel/prelink/README.md have the why of each rule.

use crate::image::{cstr_at, hex, put32, put64, scan, signed, u16_at, u32_at, u64_at, unhex, Fix, Image, RefKind, LC_DYLD_CHAINED_FIXUPS, LC_FILESET_ENTRY, LC_SEGMENT_64, LC_SYMTAB, LC_UNIXTHREAD};
use crate::layout::{align, encode_uleb, relocate_native, relocate_original_stubs, Layout, PAGE};
use crate::matcher::{matcher_signature, reviewed_records, MATCHER_VERSION};
use crate::plistpy::Plist;
use crate::records::{text, truthy};
use anyhow::{anyhow, bail, Context, Result};
use indexmap::IndexMap;
use oneios_common::pyjson::{self, Value};
use oneios_common::sha256_hex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

const MH_DYLIB_IN_CACHE: u32 = 0x8000_0000;
const KERNEL: &str = "com.apple.kernel";
const STUB: &str = "_unixnu_fileset_unmapped";

// Complete Libm pools of the kernelcache's kexts, between RETAB and the next PACIBSP: (prefix, size, SHA-256).
const FILESET_MATH_POOLS: [(&str, usize, &str); 4] = [
    ("1f2003d500000000004a87c0f039fafe", 2156, "1187961410797e1dca751f916026de525922e55ef569770be507eaa817823a6d"),
    ("0000807f0000f0420000804cdb0f493f", 1260, "5396a940a73ec06075fe8d9fb0e36b79745424d60d2968bf6d9f2a4889b47b05"),
    ("00000000001018c00ee526157bcbdb3f", 2184, "747d8fd1b75aaa671ce2a5b32576235f4ac560f9497aa1b6dc8d7a5f8a736632"),
    ("0000c1c00000807f1f2003d5ff799f50", 1092, "08b778c55cf3169dbc5cd4729430069adfbe5a75a422fd5233ab9f2f60174fda"),
];
// Constant pools that are not bracketed by RETAB/NOP (the SHA-256 round constants).
const FILESET_DATA_POOLS: [(&str, usize, &str); 1] = [("982f8a42", 256, "74ef7306e7452d6859b6463ce496b8df30925f69e1b2969e1f3f34bbc9c6af04")];

#[derive(clap::Args, Debug)]
pub struct Args {
    #[arg(long)]
    xnuprelink: String,
    #[arg(long)]
    kernelcache: String,
    #[arg(long)]
    original_kernelcache: String,
    #[arg(long)]
    native: String,
    #[arg(long)]
    map: String,
    #[arg(long)]
    roots: String,
    #[arg(long)]
    reviewed: String,
    #[arg(long)]
    output: String,
    #[arg(long)]
    report: String,
    /// align's output: pairs inferred from link order
    #[arg(long)]
    aligned: Vec<String>,
    /// known-good pairs (pairs-*.json): over the matcher, the inferred pairs and the reviewed records
    #[arg(long)]
    pairs: Option<String>,
    /// the stub list (stubs-*.json): these originals stay unpaired
    #[arg(long)]
    stubs: Option<String>,
}

fn unhex_bytes(text: &str) -> Vec<u8> {
    (0..text.len() / 2).map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap()).collect()
}

/// Python's `blob.find(needle, from)`.
fn find(blob: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > blob.len() {
        return None;
    }
    memchr::memmem::find(&blob[from..], needle).map(|at| at + from)
}

fn inline_data_ranges(blob: &[u8]) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    for (prefix, size, digest) in FILESET_MATH_POOLS {
        let (needle, mut search) = (unhex_bytes(prefix), 0);
        while let Some(at) = find(blob, &needle, search) {
            if at >= 4 && at % 4 == 0 && blob[at - 4..at] == [0xff, 0x0f, 0x5f, 0xd6] && blob.get(at + size..at + size + 4) == Some(&[0x7f, 0x23, 0x03, 0xd5]) && sha256_hex(&blob[at..at + size]) == digest {
                result.push((at, at + size));
            }
            search = at + 4;
        }
    }
    for (prefix, size, digest) in FILESET_DATA_POOLS {
        let (needle, mut search) = (unhex_bytes(prefix), 0);
        while let Some(at) = find(blob, &needle, search) {
            if at % 4 == 0 && at + size <= blob.len() && sha256_hex(&blob[at..at + size]) == digest {
                result.push((at, at + size));
            }
            search = at + 4;
        }
    }
    result.sort_unstable();
    result
}

fn ranges(image: &Image) -> Vec<(u64, u64)> {
    image.mapped()
}

fn inside<T: PartialOrd + Copy>(items: &[(T, T)], address: T) -> bool {
    items.iter().any(|(b, e)| *b <= address && address < *e)
}

fn prelink_info(cache: &Image) -> Result<(crate::image::Segment, Plist)> {
    let segment = cache.segment("__PRELINK_INFO").ok_or_else(|| anyhow!("the kernelcache has no __PRELINK_INFO"))?.clone();
    let raw = &cache.data[segment.fileoff as usize..(segment.fileoff + segment.filesize) as usize];
    let raw = &raw[..raw.iter().position(|b| *b == 0).unwrap_or(raw.len())];
    Ok((segment, Plist::load(raw)?))
}

fn bundle(entry: &Plist) -> Result<&str> {
    entry.get("CFBundleIdentifier").and_then(Plist::as_str).ok_or_else(|| anyhow!("a prelink entry has no CFBundleIdentifier"))
}

fn dependency_closure(metadata: &Plist, roots: &[String]) -> Result<Option<HashSet<String>>> {
    if roots.is_empty() {
        return Ok(None);
    }
    let Some(Plist::Array(list)) = metadata.get("_PrelinkInfoDictionary") else { bail!("__PRELINK_INFO has no _PrelinkInfoDictionary") };
    let mut entries: HashMap<&str, &Plist> = HashMap::new();
    for e in list {
        entries.insert(bundle(e)?, e);
    }
    // A root written !bundle.id leaves that dependency out.
    let excluded: HashSet<&str> = roots.iter().filter_map(|r| r.strip_prefix('!')).collect();
    let (mut closure, mut pending): (HashSet<String>, Vec<String>) = (HashSet::new(), roots.iter().filter(|r| !r.starts_with('!')).cloned().collect());
    while let Some(identifier) = pending.pop() {
        if closure.contains(&identifier) || excluded.contains(identifier.as_str()) {
            continue;
        }
        let Some(entry) = entries.get(identifier.as_str()) else { bail!("Missing prelink dependency: {identifier}") };
        if let Some(libraries) = entry.get("OSBundleLibraries") {
            pending.extend(libraries.keys().into_iter().map(str::to_string));
        }
        closure.insert(identifier);
    }
    Ok(Some(closure))
}

const MASK64: i128 = (1 << 64) - 1;

fn signed64(value: u64) -> i128 {
    value as i64 as i128
}

/// Move collection metadata to XNU's link address; chain targets stay relative. Returns the rebased bytes, the
/// slide and the dependencies whose personalities were dropped.
fn rebase_collection(cache: &Image, mut metadata: Plist, base: u64, closure: Option<&HashSet<String>>, early: &IndexMap<String, u64>, roots: &[String]) -> Result<(Vec<u8>, u64, Vec<String>)> {
    let delta = base.wrapping_sub(cache.base);
    let mut data = cache.data.to_vec();
    let add = |data: &mut [u8], at: usize| {
        let value = u64_at(data, at);
        put64(data, at, value.wrapping_add(delta));
    };
    let mut offsets: Vec<u64> = cache.entries.values().map(|e| e.1).collect();
    offsets.sort_unstable();
    offsets.dedup();
    for at in std::iter::once(0).chain(offsets) {
        let image = Image::new(cache.data, at as usize)?;
        for &(command, offset, _) in &image.commands {
            match command {
                LC_SEGMENT_64 => {
                    add(&mut data, offset + 24);
                    for index in 0..u32_at(&data, offset + 64) as usize {
                        add(&mut data, offset + 72 + 80 * index + 32);
                    }
                }
                LC_FILESET_ENTRY => add(&mut data, offset + 8),
                LC_UNIXTHREAD => add(&mut data, offset + 272),
                LC_SYMTAB => {
                    let (symoff, count) = (u32_at(&data, offset + 8) as usize, u32_at(&data, offset + 12) as usize);
                    for index in 0..count {
                        let symbol = symoff + 16 * index;
                        let kind = data[symbol + 4];
                        if kind & 0xe0 == 0 && kind & 0x0e == 0x0e {
                            add(&mut data, symbol + 8);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let segment = cache.segment("__PRELINK_INFO").unwrap().clone();
    let (mut retained, mut dropped_personalities) = (Vec::new(), Vec::new());
    let Some(Plist::Array(list)) = metadata.remove("_PrelinkInfoDictionary") else { bail!("__PRELINK_INFO has no _PrelinkInfoDictionary") };
    let int = |entry: &Plist, key: &str| (entry.get(key).and_then(Plist::as_int).unwrap_or(0) & MASK64) as u64;
    for mut entry in list {
        let identifier = bundle(&entry)?.to_string();
        if closure.is_some_and(|c| !c.contains(&identifier)) {
            continue;
        }
        let kmod = int(&entry, "_PrelinkKmodInfo");
        if kmod != 0 {
            // kmod_info.address is outside the fixup chains (pack(4), +156)
            let field = cache.offset_of(kmod.wrapping_add(156), 8)?;
            let value = u64_at(&data, field);
            if value != 0 {
                cache.offset_of(value, 1)?; // refuse a non-collection raw address
                put64(&mut data, field, value.wrapping_add(delta));
            }
        }
        for key in ["_PrelinkExecutableLoadAddr", "_PrelinkExecutableSourceAddr", "_PrelinkKmodInfo"] {
            let value = int(&entry, key);
            if !(value == 0 || value == u64::MAX || value == (1 << 63) - 1) {
                cache.offset_of(value, 1)?;
                entry.set(key, Plist::Int(signed64(value.wrapping_add(delta))));
            }
        }
        if !roots.is_empty() && !roots.contains(&identifier) && entry.remove("IOKitPersonalities").is_some_and(|p| p.truthy()) {
            // A dependency is loaded for its code; its drivers would match this board's device tree.
            dropped_personalities.push(identifier.clone());
        }
        for root in roots {
            if let Some(name) = root.strip_prefix('!') {
                if let Some(libraries) = entry.get_mut("OSBundleLibraries") {
                    libraries.remove(name);
                }
            }
        }
        entry.remove("_NativePrelinkEarlySecurityInit");
        if let Some(address) = early.get(&identifier) {
            // native-security-init.patch: OSKext calls this original registration routine after start.
            if entry.get("AppleSecurityExtension") != Some(&Plist::Bool(true)) {
                bail!("{identifier} is not a security extension");
            }
            entry.set("_NativePrelinkEarlySecurityInit", Plist::Int(signed64(address.wrapping_add(delta))));
        }
        retained.push(entry);
    }
    let have: HashSet<&str> = retained.iter().filter_map(|e| bundle(e).ok()).collect();
    let mut missing: Vec<&String> = early.keys().filter(|k| !have.contains(k.as_str())).collect();
    if !missing.is_empty() {
        missing.sort();
        bail!("Early security initializers for components outside the collection: {missing:?}");
    }
    // `metadata['_PrelinkInfoDictionary'] = retained` keeps the key's place in the dictionary.
    let (_, original) = prelink_info(cache)?;
    let mut rebuilt = Vec::new();
    for key in original.keys() {
        if key == "_PrelinkInfoDictionary" {
            rebuilt.push((key.to_string(), Plist::Array(std::mem::take(&mut retained))));
        } else if let Some(value) = metadata.remove(key) {
            rebuilt.push((key.to_string(), value));
        }
    }
    let mut xml = Plist::Dict(rebuilt).dumps()?;
    xml.push(0);
    let (begin, size) = (segment.fileoff as usize, segment.filesize as usize);
    if xml.len() > size {
        bail!("Rebased prelink metadata exceeds its segment");
    }
    data[begin..begin + xml.len()].copy_from_slice(&xml);
    data[begin + xml.len()..begin + size].fill(0);
    Ok((data, delta, dropped_personalities))
}

fn chain_word(pointer: &Fix, target_offset: u64, step: u64) -> Result<u64> {
    if target_offset >= 1 << 30 || step >= 1 << 12 {
        bail!("Chained pointer target or next step is out of range");
    }
    let mut value = (target_offset & 0x3fff_ffff) | ((step & 0xfff) << 51);
    if pointer.auth {
        value |= 1 << 63 | ((pointer.key as u64 & 3) << 49) | ((pointer.diversity as u64) << 32) | ((pointer.addr_div as u64) << 48);
    }
    Ok(value)
}

/// The ordered chain sites of every root segment that carries a table: per segment (in the image's order), per
/// page, (offset in page, value).
fn chain_pages(image: &Image) -> Result<Vec<(usize, Vec<(u64, Vec<(u64, u64)>)>)>> {
    let data = image.data;
    let (found, _) = image.command(LC_DYLD_CHAINED_FIXUPS).ok_or_else(|| anyhow!("the collection has no chained fixups"))?;
    let dataoff = u32_at(data, found + 8) as usize;
    if u32_at(data, dataoff + 16) != 0 {
        bail!("Chained fixup imports are not supported");
    }
    let table = dataoff + u32_at(data, dataoff + 4) as usize;
    let mut tables: HashMap<&str, (usize, usize)> = HashMap::new();
    for index in 0..u32_at(data, table) as usize {
        let relative = u32_at(data, table + 4 + 4 * index) as usize;
        if relative == 0 {
            continue;
        }
        let at = table + relative;
        let (page_size, fmt, segment_offset, pages) = (u16_at(data, at + 4) as u64, u16_at(data, at + 6), u64_at(data, at + 8), u16_at(data, at + 20) as usize);
        if fmt != 8 || page_size != PAGE {
            bail!("Unsupported chain table: format {fmt}, page {page_size}");
        }
        let segment = image.segments.iter().find(|s| s.va.wrapping_sub(image.base) == segment_offset).ok_or_else(|| anyhow!("Chain table outside known segments: {segment_offset:#x}"))?;
        tables.insert(&segment.name, (at, pages));
    }
    let mut out = Vec::new();
    for (index, segment) in image.segments.iter().enumerate() {
        let Some(&(at, count)) = tables.get(segment.name.as_str()) else { continue };
        let fileoff = image.segment(&segment.name).unwrap().fileoff;
        let mut pages = Vec::new();
        for page in 0..count {
            let start = u16_at(data, at + 22 + 2 * page);
            if start == 0xffff {
                continue;
            }
            if start & 0x8000 != 0 {
                bail!("Multi-start pages are not supported");
            }
            let (mut offset, mut sites) = (start as u64, Vec::new());
            loop {
                let value = u64_at(data, (fileoff + page as u64 * PAGE + offset) as usize);
                sites.push((offset, value));
                let step = (value >> 51) & 0xfff;
                if step == 0 {
                    break;
                }
                offset += step * 4;
                if offset >= PAGE {
                    bail!("Chain crosses a page boundary");
                }
            }
            pages.push((page as u64, sites));
        }
        out.push((index, pages));
    }
    Ok(out)
}

/// Python's Counter with values of any kind, as the report's "statistics".
#[derive(Default)]
struct Report(Vec<(String, Value)>);

impl Report {
    fn inc(&mut self, key: &str, n: i128) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some((_, Value::Int(v))) => *v += n,
            Some(_) => {}
            None => self.0.push((key.to_string(), Value::Int(n))),
        }
    }
    fn set(&mut self, key: &str, value: impl Into<Value>) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value.into(),
            None => self.0.push((key.to_string(), value.into())),
        }
    }
}

type Counts = BTreeMap<String, BTreeMap<u64, i128>>;

struct Emit<'a> {
    data: Vec<u8>,
    cache: &'a Image<'a>,
    native: &'a Image<'a>,
    pairs: HashMap<u64, u64>,
    closure: Option<&'a HashSet<String>>,
    old_ranges: Vec<(u64, u64)>,
    owner_ranges: Vec<(u64, u64, String)>,
    site_pairs: HashMap<u64, u64>,
    stub: Option<u64>,
    stubbed: Counts,
    missing: Counts,
    old_starts: HashSet<u64>,
    layout: Layout,
    fileoff: u64,
    table_cursor: u64,
    fixups: (u64, u64),
    starts: (u64, u64),
    native_pointers: BTreeMap<u64, Fix>,
    original_branches: Vec<Value>,
    report: Report,
}

impl Emit<'_> {
    fn owner(&self, address: u64) -> &str {
        let index = self.owner_ranges.partition_point(|r| r.0 <= address);
        if index > 0 && address < self.owner_ranges[index - 1].1 {
            return &self.owner_ranges[index - 1].2;
        }
        KERNEL
    }
    fn in_closure(&self, address: u64) -> bool {
        self.closure.is_none_or(|c| c.contains(self.owner(address)))
    }
    fn mapped(&self, address: u64) -> bool {
        inside(&self.old_ranges, address)
    }

    /// Where a reference to the old kernel's `target` goes: None when it stays unresolved.
    fn rebind(&mut self, target: u64, site: u64) -> Result<Option<u64>> {
        if !self.mapped(target) {
            return Ok(Some(target));
        }
        // A reviewed site binding wins over the target's pairing.
        let mut native = self.site_pairs.get(&site).or_else(|| self.pairs.get(&target)).copied();
        if native.is_none() && self.stub.is_some() && self.old_starts.contains(&target) {
            // A kernel function this kernel does not have: the stub reports the call site.
            let owner = self.owner(site).to_string();
            *self.stubbed.entry(owner).or_default().entry(target).or_default() += 1;
            native = self.stub;
        }
        match native {
            Some(native) => Ok(Some(self.layout.translate(native)?)),
            None => {
                let owner = self.owner(site).to_string();
                *self.missing.entry(owner).or_default().entry(target).or_default() += 1;
                Ok(None)
            }
        }
    }

    fn place_kernel(&mut self) -> Result<()> {
        self.report.set("segment_layout", self.layout.describe()?);
        let base = self.cache.base;
        self.fileoff = self.layout.placed("__TEXT")? - base;
        if (self.layout.size as usize) < self.data.len() {
            bail!("the laid-out collection is smaller than the kernelcache");
        }
        self.data.resize(self.layout.size as usize, 0);
        for segment in &self.native.segments {
            if segment.size == 0 {
                continue;
            }
            let at = (self.layout.placed(&segment.name)? - base) as usize;
            let (source, size) = (segment.fileoff as usize, segment.filesize as usize);
            let end = at + (segment.size as usize).max(size);
            if end > self.data.len() {
                bail!("the kernel's {} is placed past the end of the collection", segment.name);
            }
            self.data[at..at + segment.size as usize].fill(0);
            self.data[at..at + size].copy_from_slice(&self.native.data[source..source + size]);
        }
        let old_info = self.cache.segment("__PRELINK_INFO").unwrap();
        let new_info = &self.layout.roots["__PRELINK_INFO"];
        let (to, from, size) = (new_info.fileoff as usize, old_info.fileoff as usize, old_info.filesize as usize);
        if new_info.filesize != old_info.filesize {
            bail!("__PRELINK_INFO changed its size in the layout");
        }
        self.data[to..to + size].copy_from_slice(&self.cache.data[from..from + size]);
        self.table_cursor = self.layout.table_begin;
        Ok(())
    }

    fn append_table(&mut self, blob: &[u8]) -> Result<u64> {
        let at = align(self.table_cursor, 16);
        if at + blob.len() as u64 > self.layout.table_end {
            bail!("Derived linkedit reservation is insufficient");
        }
        self.data[at as usize..at as usize + blob.len()].copy_from_slice(blob);
        self.table_cursor = at + blob.len() as u64;
        Ok(at)
    }

    fn rewrite_native_chains(&mut self, native_fixups: &crate::image::Fixups) -> Result<()> {
        let (pointers, counts) = relocate_native(&mut self.data, &self.layout, self.native, native_fixups)?;
        self.native_pointers = pointers;
        self.original_branches = relocate_original_stubs(&mut self.data, &self.layout, self.native)?;
        self.report.set("native_original_branches_rebound", Value::Int(self.original_branches.len() as i128));
        self.report.set("native_split_relocations", counts.to_value());
        self.report.set("native_chain_sites", Value::Int(self.native_pointers.len() as i128));
        Ok(())
    }

    /// One starts table for the collection: the kept kext chains (read before any native bytes were placed in
    /// vacated pages) plus the kernel's own pointers.
    fn rebuild_linkedit(&mut self) -> Result<()> {
        let base = self.cache.base;
        let mut pointers = std::mem::take(&mut self.native_pointers);
        for (index, pages) in chain_pages(self.cache)? {
            let va = self.cache.segments[index].va;
            for (page, sites) in pages {
                for (offset, value) in sites {
                    let site = va + page * PAGE + offset;
                    if self.mapped(site) {
                        self.report.inc("old_kernel_sites_dropped", 1);
                        continue;
                    }
                    if !self.in_closure(site) {
                        self.report.inc("unselected_chain_sites_dropped", 1);
                        continue;
                    }
                    if (value >> 30) & 3 != 0 {
                        bail!("Only kernel cache level 0 is supported");
                    }
                    let mut pointer = Fix { target: base + (value & 0x3fff_ffff), auth: value >> 63 != 0, key: ((value >> 49) & 3) as u8, diversity: ((value >> 32) & 0xffff) as u16, addr_div: (value >> 48) & 1 != 0 };
                    if self.mapped(pointer.target) {
                        match self.rebind(pointer.target, site)? {
                            None => self.report.inc("closure_pointers_unmapped", 1),
                            Some(rebound) => {
                                pointer.target = rebound;
                                self.report.inc("kext_pointers_rebound", 1);
                            }
                        }
                    }
                    if pointers.insert(site, pointer).is_some() {
                        bail!("Native and kext fixup sites overlap");
                    }
                    self.report.inc("kext_chain_sites_kept", 1);
                }
            }
        }
        let order: Vec<crate::image::Segment> = self.layout.roots.values().cloned().collect();
        let mut blob = vec![0u8; 4 + 4 * order.len()];
        put32(&mut blob, 0, order.len() as u32);
        let mut grouped: IndexMap<usize, IndexMap<u64, Vec<(u64, Fix)>>> = IndexMap::new();
        for (site, pointer) in &pointers {
            let mut matches = order.iter().enumerate().filter(|(_, s)| s.va <= *site && site + 8 <= s.va + s.filesize);
            let index = match (matches.next(), matches.next()) {
                (Some((index, _)), None) if site % 4 == 0 => index,
                _ => bail!("Fixup is not within one root mapping: {site:#x}"),
            };
            let relative = site - order[index].va;
            grouped.entry(index).or_default().entry(relative / PAGE).or_default().push((relative % PAGE, *pointer));
        }
        for (index, pages) in &grouped {
            let count = pages.keys().max().unwrap() + 1;
            if count > 65535 {
                bail!("Too many chained-fixup pages");
            }
            let mut piece = Vec::with_capacity(22 + count as usize * 2);
            piece.extend_from_slice(&(22 + count as u32 * 2).to_le_bytes());
            piece.extend_from_slice(&(PAGE as u16).to_le_bytes());
            piece.extend_from_slice(&8u16.to_le_bytes());
            piece.extend_from_slice(&order[*index].va.wrapping_sub(base).to_le_bytes());
            piece.extend_from_slice(&0u32.to_le_bytes());
            piece.extend_from_slice(&(count as u16).to_le_bytes());
            piece.resize(22 + count as usize * 2, 0xff);
            for (page, sites) in pages {
                piece[22 + *page as usize * 2..24 + *page as usize * 2].copy_from_slice(&(sites[0].0 as u16).to_le_bytes());
                for (n, (offset, pointer)) in sites.iter().enumerate() {
                    let step = sites.get(n + 1).map_or(0, |next| (next.0 - offset) / 4);
                    let word = chain_word(pointer, pointer.target.wrapping_sub(base), step)?;
                    put64(&mut self.data, (order[*index].fileoff + page * PAGE + offset) as usize, word);
                }
            }
            let at = blob.len() as u32;
            put32(&mut blob, 4 + index * 4, at);
            blob.extend(piece);
        }
        let mut table: Vec<u8> = [0u32, 28, 28, 28, 0, 1, 0].iter().flat_map(|v| v.to_le_bytes()).collect();
        table.extend(blob);
        self.fixups = (self.append_table(&table)?, table.len() as u64);
        let mut addresses = self.native.function_starts()?.into_iter().map(|a| self.layout.translate(a)).collect::<Result<Vec<u64>>>()?;
        addresses.sort_unstable();
        let (mut previous, mut starts) = (self.layout.placed("__TEXT")?, Vec::new());
        for address in addresses {
            if address < previous {
                bail!("Negative ULEB");
            }
            encode_uleb(address - previous, &mut starts);
            previous = address;
        }
        starts.push(0);
        self.starts = (self.append_table(&starts)?, starts.len() as u64);
        Ok(())
    }

    fn patch_adrp_pair(&mut self, segment: &crate::image::Segment, adrp_offset: u64, use_offset: u64, rebound: u64, size: u32) -> Result<()> {
        let at = (segment.fileoff + adrp_offset) as usize;
        let word = u32_at(&self.data, at);
        let pc = segment.va + adrp_offset;
        let immediate = ((rebound & !4095) as i128 - (pc & !4095) as i128) >> 12;
        if immediate.abs() >= 1 << 20 {
            bail!("Adrp out of range at {pc:#x}");
        }
        let immediate = immediate as u32;
        put32(&mut self.data, at, (word & 0x9f00_001f) | (((immediate >> 2) & 0x7ffff) << 5) | ((immediate & 3) << 29));
        let at = (segment.fileoff + use_offset) as usize;
        let low = ((rebound & 0xfff) >> size) as u32;
        let word = u32_at(&self.data, at);
        put32(&mut self.data, at, (word & 0xffc0_03ff) | (low << 10));
        self.report.inc("adrp_pairs_rebound", 1);
        Ok(())
    }

    /// Rewrite selected executable entries using explicit ADRP provenance.
    fn rewrite_branches(&mut self) -> Result<()> {
        let cache = self.cache;
        for (name, (_, entry_at)) in &cache.entries {
            if name == KERNEL || self.closure.is_some_and(|c| !c.contains(name)) {
                continue;
            }
            for segment in &Image::new(cache.data, *entry_at as usize)?.segments {
                if segment.name != "__TEXT_EXEC" || segment.filesize == 0 {
                    continue;
                }
                let blob = &cache.data[segment.fileoff as usize..(segment.fileoff + segment.filesize) as usize];
                let pools = inline_data_ranges(blob);
                self.report.inc("inline_data_pools_preserved", pools.len() as i128);
                self.report.inc("inline_data_bytes_preserved", pools.iter().map(|(b, e)| (e - b) as i128).sum());
                let (mut begin, mut calls, mut refs) = (0, Vec::new(), Vec::new());
                for &(end, next_begin) in pools.iter().chain([&(blob.len(), blob.len())]) {
                    let found = scan(&blob[begin..end], segment.va + begin as u64);
                    calls.extend(found.calls);
                    refs.extend(found.refs);
                    begin = next_begin;
                }
                let file = |pc: u64| (segment.fileoff + pc - segment.va) as usize;
                for (pc, target, _) in calls {
                    if !self.mapped(target) {
                        continue;
                    }
                    let Some(rebound) = self.rebind(target, pc)? else {
                        self.report.inc("closure_branches_unmapped", 1);
                        continue;
                    };
                    let displacement = rebound as i128 - pc as i128;
                    if displacement % 4 != 0 || !(-(1i128 << 27)..1i128 << 27).contains(&displacement) {
                        bail!("Kext branch out of range at {pc:#x}");
                    }
                    let word = u32_at(&self.data, file(pc));
                    put32(&mut self.data, file(pc), (word & 0xfc00_0000) | ((displacement >> 2) as u32 & 0x3ff_ffff));
                    self.report.inc("branches_rebound", 1);
                }
                let mut groups: IndexMap<u64, Vec<(u64, u64, RefKind, u32)>> = IndexMap::new();
                for r in refs {
                    if let Some(producer) = r.producer {
                        groups.entry(producer).or_default().push((r.site, r.target, r.kind, r.scale));
                        continue;
                    }
                    if !self.mapped(r.target) {
                        continue;
                    }
                    let Some(rebound) = self.rebind(r.target, r.site)? else {
                        self.report.inc("closure_adr_unmapped", 1);
                        continue;
                    };
                    let immediate = rebound as i128 - r.site as i128;
                    if !(-(1i128 << 20)..1i128 << 20).contains(&immediate) {
                        bail!("Kext ADR out of range at {:#x}", r.site);
                    }
                    let immediate = immediate as u32;
                    let word = u32_at(&self.data, file(r.site));
                    put32(&mut self.data, file(r.site), (word & 0x9f00_001f) | (((immediate >> 2) & 0x7ffff) << 5) | ((immediate & 3) << 29));
                    self.report.inc("adr_rebound", 1);
                }
                for (producer, uses) in &groups {
                    let mut rewritten = Vec::new();
                    for &(pc, target, kind, scale) in uses {
                        let Some(rebound) = self.rebind(target, pc)? else {
                            self.report.inc("closure_adrp_unmapped", 1);
                            continue;
                        };
                        if self.mapped(target) {
                            if kind == RefKind::Add && scale != 0 {
                                bail!("Shifted ADD cannot be rewritten as a low page offset");
                            }
                            if rebound % (1 << scale) != 0 {
                                bail!("Relocated load/store target is not aligned");
                            }
                        }
                        rewritten.push((pc, rebound, scale, target));
                    }
                    if rewritten.len() != uses.len() || !rewritten.iter().any(|r| self.mapped(r.3)) {
                        continue;
                    }
                    if rewritten.iter().map(|r| r.1 & !4095).collect::<HashSet<u64>>().len() != 1 {
                        bail!("ADRP consumers require different pages at {producer:#x}");
                    }
                    for (pc, target, scale, _) in rewritten {
                        self.patch_adrp_pair(segment, producer - segment.va, pc - segment.va, target, scale)?;
                    }
                }
                // An ADRP into the old kernel without a proven consumer cannot be silently left pointing at
                // discarded code or data.
                let mut offset = 0;
                while offset + 3 < blob.len() {
                    let (word, pc) = (u32_at(blob, offset), segment.va + offset as u64);
                    let here = offset;
                    offset += 4;
                    if inside(&pools, here) || word & 0x9f00_0000 != 0x9000_0000 || groups.contains_key(&pc) {
                        continue;
                    }
                    let immediate = signed((((word >> 5) & 0x7ffff) << 2 | ((word >> 29) & 3)) as u64, 21);
                    let page = (pc & !4095).wrapping_add((immediate << 12) as u64);
                    if self.mapped(page) {
                        *self.missing.entry(name.clone()).or_default().entry(page).or_default() += 1;
                        self.report.inc("unproven_kernel_page_references", 1);
                    }
                }
            }
        }
        Ok(())
    }

    /// Reseal original components whose FIPS HMAC covers the __text that branch rebinding changed.
    fn seal_components(&mut self) -> Result<()> {
        let mut sealed = Value::dict();
        for (name, (_, entry_at)) in &self.cache.entries {
            if name == KERNEL || self.closure.is_some_and(|c| !c.contains(name)) {
                continue;
            }
            let sections = Image::new(self.cache.data, *entry_at as usize)?.sections;
            let seals: Vec<_> = sections.iter().filter(|s| s.name == "__fips_hmacs").collect();
            if seals.is_empty() {
                continue;
            }
            let code: Vec<_> = sections.iter().filter(|s| s.segment == "__TEXT_EXEC" && s.name == "__text").collect();
            if seals.len() != 1 || seals[0].size != 32 || code.len() != 1 {
                bail!("{name}: unexpected FIPS HMAC layout");
            }
            let (code_at, seal_at, size) = (self.cache.offset_of(code[0].va, code[0].size)?, self.cache.offset_of(seals[0].va, 32)?, code[0].size as usize);
            if hmac_zero(&self.cache.data[code_at..code_at + size]) != self.cache.data[seal_at..seal_at + 32] {
                bail!("{name}: original FIPS HMAC does not cover its __text");
            }
            let digest = hmac_zero(&self.data[code_at..code_at + size]);
            self.data[seal_at..seal_at + 32].copy_from_slice(&digest);
            sealed.set(name, oneios_common::hex(&digest));
        }
        self.report.set("component_hmacs", sealed);
        Ok(())
    }

    /// The kernel's own header inside the collection: every address and file offset moved by the layout.
    fn patch_entry_header(&mut self) -> Result<()> {
        let (native, base) = (self.native, self.cache.base);
        let u32_of = |v: u64| u32::try_from(v).map_err(|_| anyhow!("a file offset of the kernel does not fit 32 bits: {v:#x}"));
        let (mut rebuilt, mut count, mut section_index) = (Vec::new(), 0u32, 0u64);
        for &(command, offset, length) in &native.commands {
            let mut blob = native.data[offset..offset + length].to_vec();
            match command {
                LC_DYLD_CHAINED_FIXUPS | 0x1d => continue,
                LC_SEGMENT_64 => {
                    let name = String::from_utf8_lossy(&blob[8..24]).trim_end_matches('\0').to_string();
                    let filesize = self.layout.segment(&name)?.filesize;
                    let address = self.layout.placed(&name)?;
                    put64(&mut blob, 24, address);
                    put64(&mut blob, 40, if filesize != 0 { address - base } else { 0 });
                    if name == "__LINKEDIT" {
                        let size = self.layout.table_end - (address - base);
                        put64(&mut blob, 32, size);
                        put64(&mut blob, 48, size);
                    }
                    for index in 0..u32_at(&blob, 64) as usize {
                        section_index += 1;
                        let s = 72 + index * 80;
                        put64(&mut blob, s + 32, self.layout.section_address(section_index, 0, true)?);
                        for field in [48, 56] {
                            let value = u32_at(&blob, s + field);
                            if value != 0 {
                                put32(&mut blob, s + field, u32_of(self.layout.file(value as u64, 1)?)?);
                            }
                        }
                    }
                }
                LC_SYMTAB => {
                    let (symoff, nsyms, stroff, strsize) = (u32_at(&blob, 8) as u64, u32_at(&blob, 12) as u64, u32_at(&blob, 16) as u64, u32_at(&blob, 20) as u64);
                    let at = self.layout.file(symoff, nsyms * 16)?;
                    put32(&mut blob, 8, u32_of(at)?);
                    put32(&mut blob, 16, u32_of(self.layout.file(stroff, strsize)?)?);
                    for index in 0..nsyms {
                        let symbol = (at + index * 16) as usize;
                        let (kind, ordinal) = (self.data[symbol + 4], self.data[symbol + 5] as u64);
                        if kind & 0xe0 == 0 && kind & 0x0e == 0x0e {
                            let value = u64_at(&self.data, symbol + 8);
                            if !(1..=native.sections.len() as u64).contains(&ordinal) {
                                bail!("N_SECT symbol has invalid section ordinal");
                            }
                            let section = &native.sections[ordinal as usize - 1];
                            let moved = self.layout.section_address(ordinal, value as i128 - section.va as i128, true)?;
                            put64(&mut self.data, symbol + 8, moved);
                        }
                    }
                }
                0x26 => {
                    put32(&mut blob, 8, u32_of(self.starts.0)?);
                    put32(&mut blob, 12, u32_of(self.starts.1)?);
                }
                0x1e | 0x29 | 0x2b | 0x2e | 0x8000_0033 => {
                    let (value, size) = (u32_at(&blob, 8) as u64, u32_at(&blob, 12) as u64);
                    if size != 0 {
                        put32(&mut blob, 8, u32_of(self.layout.file(value, size)?)?);
                    }
                }
                0xb => {
                    for field in [32, 40, 48, 56, 64, 72] {
                        let value = u32_at(&blob, field) as u64;
                        if value != 0 {
                            put32(&mut blob, field, u32_of(self.layout.file(value, 1)?)?);
                        }
                    }
                }
                0x22 | 0x8000_0022 => {
                    for field in [8, 16, 24, 32, 40] {
                        let (value, size) = (u32_at(&blob, field) as u64, u32_at(&blob, field + 4) as u64);
                        if size != 0 {
                            put32(&mut blob, field, u32_of(self.layout.file(value, size)?)?);
                        }
                    }
                }
                LC_UNIXTHREAD => {
                    let pc = u64_at(&blob, 272);
                    put64(&mut blob, 272, self.layout.translate(pc)?);
                }
                _ => {}
            }
            count += 1;
            rebuilt.extend(blob);
        }
        let oldsize = u32_at(native.data, 20) as usize;
        if rebuilt.len() > oldsize {
            bail!("Native load commands exceed available header space");
        }
        let at = self.fileoff as usize;
        self.data[at + 32..at + 32 + rebuilt.len()].copy_from_slice(&rebuilt);
        self.data[at + 32 + rebuilt.len()..at + 32 + oldsize].fill(0);
        put32(&mut self.data, at + 16, count);
        put32(&mut self.data, at + 20, rebuilt.len() as u32);
        let flags = u32_at(&self.data, at + 24);
        put32(&mut self.data, at + 24, flags | MH_DYLIB_IN_CACHE);
        // corecrypto authenticates the relocated __text bytes in a fileset.
        let code = native.sections.iter().find(|s| s.segment == "__TEXT_EXEC" && s.name == "__text").ok_or_else(|| anyhow!("the kernel has no __TEXT_EXEC,__text"))?;
        let seals: Vec<_> = native.sections.iter().filter(|s| s.name == "__fips_hmacs").collect();
        if seals.len() != 1 || seals[0].size != 32 {
            bail!("Expected one 32-byte corecrypto HMAC field");
        }
        let code_at = (self.layout.translate(code.va)? - base) as usize;
        let digest = hmac_zero(&self.data[code_at..code_at + code.size as usize]);
        let seal_at = (self.layout.translate(seals[0].va)? - base) as usize;
        self.data[seal_at..seal_at + 32].copy_from_slice(&digest);
        self.report.set("corecrypto_hmac_sha256", oneios_common::hex(&digest));
        Ok(())
    }

    /// The collection's own header: the moved root segments, the kernel entry, the kept kexts.
    fn patch_headers(&mut self) -> Result<()> {
        let (cache, native) = (self.cache, self.native);
        let (thread, _) = native.command(LC_UNIXTHREAD).ok_or_else(|| anyhow!("the kernel has no LC_UNIXTHREAD"))?;
        let native_pc = u64_at(native.data, thread + 272);
        let (mut rebuilt, mut count) = (Vec::new(), 0u32);
        for &(command, offset, length) in &cache.commands {
            let mut blob = cache.data[offset..offset + length].to_vec();
            match command {
                LC_SEGMENT_64 => {
                    let name = String::from_utf8_lossy(&blob[8..24]).trim_end_matches('\0').to_string();
                    let Some(segment) = self.layout.roots.get(&name) else { continue };
                    let old_address = u64_at(&blob, 24);
                    for (i, v) in [segment.va, segment.size, segment.fileoff, segment.filesize].into_iter().enumerate() {
                        put64(&mut blob, 24 + 8 * i, v);
                    }
                    let delta = segment.va as i128 - old_address as i128;
                    for index in 0..u32_at(&blob, 64) as usize {
                        let s = 72 + index * 80;
                        let address = u64_at(&blob, s + 32);
                        put64(&mut blob, s + 32, (address as i128 + delta) as u64);
                        let fileoff = u32_at(&blob, s + 48);
                        if fileoff != 0 {
                            put32(&mut blob, s + 48, u32::try_from(fileoff as i128 + delta).map_err(|_| anyhow!("a section of {name} moved past 32 bits of file offset"))?);
                        }
                    }
                }
                LC_DYLD_CHAINED_FIXUPS => {
                    put32(&mut blob, 8, self.fixups.0 as u32);
                    put32(&mut blob, 12, self.fixups.1 as u32);
                }
                LC_FILESET_ENTRY => {
                    let name = cstr_at(&blob, u32_at(&blob, 24) as usize)?;
                    if name == KERNEL {
                        put64(&mut blob, 8, self.layout.placed("__TEXT")?);
                        put64(&mut blob, 16, self.fileoff);
                    } else if self.closure.is_some_and(|c| !c.contains(&name)) {
                        self.report.inc("unselected_entries_dropped", 1);
                        continue;
                    }
                }
                LC_UNIXTHREAD => put64(&mut blob, 272, self.layout.translate(native_pc)?),
                0x1d => continue,
                _ => {}
            }
            count += 1;
            rebuilt.extend(blob);
        }
        let original_names: HashSet<&str> = cache.segments.iter().map(|s| s.name.as_str()).collect();
        for (name, segment) in &self.layout.roots {
            if original_names.contains(name.as_str()) {
                continue;
            }
            let source = self.layout.segment(name)?;
            rebuilt.extend(LC_SEGMENT_64.to_le_bytes());
            rebuilt.extend(72u32.to_le_bytes());
            let mut field = [0u8; 16];
            field[..name.len().min(16)].copy_from_slice(&name.as_bytes()[..name.len().min(16)]);
            rebuilt.extend(field);
            for v in [segment.va, segment.size, segment.fileoff, segment.filesize] {
                rebuilt.extend(v.to_le_bytes());
            }
            rebuilt.extend_from_slice(&native.data[source.command_offset + 56..source.command_offset + 64]); // maxprot, initprot
            rebuilt.extend([0u8; 8]);
            count += 1;
        }
        let limit = self.layout.roots["__TEXT"].filesize as usize;
        if rebuilt.len() + 32 > limit {
            bail!("Root commands exceed header mapping");
        }
        self.data[32..32 + rebuilt.len()].copy_from_slice(&rebuilt);
        self.data[32 + rebuilt.len()..limit].fill(0);
        put32(&mut self.data, 16, count);
        put32(&mut self.data, 20, rebuilt.len() as u32);
        self.report.set("native_base", hex(self.layout.placed("__TEXT")?));
        self.report.set("native_fileoff", hex(self.fileoff));
        Ok(())
    }
}

/// HMAC-SHA-256 with the one-byte key 0, as corecrypto's FIPS self-test keys it.
fn hmac_zero(data: &[u8]) -> [u8; 32] {
    use hmac::Mac;
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&[0]).unwrap();
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// One side of the pairing as emit.py edits it: original (the key's own text) -> (native address, name).
type Side = IndexMap<String, (u64, Option<String>)>;

fn load_side(payload: &Value, kind: &str) -> Result<Side> {
    let mut out = IndexMap::new();
    for (key, value) in payload.get(kind).and_then(Value::as_dict).ok_or_else(|| anyhow!("the pairing has no \"{kind}\""))? {
        out.insert(key.clone(), (unhex(text(value, "native")?)?, value.get("name").and_then(Value::as_str).map(str::to_string)));
    }
    Ok(out)
}

fn row(items: &[(&str, Value)]) -> Value {
    Value::Dict(items.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

fn counts_value(counts: &Counts) -> Value {
    Value::Dict(counts.iter().map(|(owner, targets)| (owner.clone(), Value::Dict(targets.iter().map(|(t, c)| (hex(*t), Value::Int(*c))).collect()))).collect())
}

/// Python's `str(Path(p).resolve())`: the folder made real, the last name kept (the file may not exist yet).
fn resolved(path: &str) -> Result<String> {
    let p = Path::new(path);
    if let Ok(real) = std::fs::canonicalize(p) {
        return Ok(real.to_string_lossy().into_owned());
    }
    let parent = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = p.file_name().ok_or_else(|| anyhow!("{path} names no file"))?;
    Ok(std::fs::canonicalize(parent).with_context(|| format!("{}", parent.display()))?.join(name).to_string_lossy().into_owned())
}

/// A file written through a temporary name in its folder, readable by its owner only (Python's
/// NamedTemporaryFile makes it so, and the collection is Apple's code).
fn private_write(path: &str, contents: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let tmp = format!("{path}.tmp{}", std::process::id());
    std::fs::write(&tmp, contents).with_context(|| path.to_string())?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).with_context(|| path.to_string())?;
    std::fs::rename(&tmp, path).with_context(|| path.to_string())
}

pub fn run(args: Args) -> Result<()> {
    let xnuprelink = Path::new(&args.xnuprelink);
    let raw = oneios_common::Map::open(&args.kernelcache)?;
    let original_raw = oneios_common::Map::open(&args.original_kernelcache)?;
    let native_data = oneios_common::Map::open(&args.native)?;
    let payload = pyjson::load(&args.map)?;
    let reviewed = pyjson::load(&args.reviewed)?;
    let roots_value = pyjson::load(&args.roots)?.get("roots").cloned().ok_or_else(|| anyhow!("{}: no roots", args.roots))?;
    let roots: Vec<String> = roots_value.as_list().unwrap_or(&[]).iter().filter_map(|r| r.as_str().map(str::to_string)).collect();
    let reviewed_sha = text(&reviewed, "kernelcache_sha256")?;
    if sha256_hex(&original_raw) != reviewed_sha {
        bail!("the reviewed records are for another kernelcache");
    }
    let original = Image::new(&original_raw, 0)?;
    let check = |record: &Value| -> Result<()> {
        if !truthy(record.get("evidence")) {
            bail!("a reviewed record needs its evidence: {}", pyjson::dumps(record, None, false));
        }
        if let Some(pin) = record.get("check").filter(|p| truthy(Some(p))) {
            let (address, size) = (text(pin, "address")?, pin.get("size").and_then(Value::as_u64).ok_or_else(|| anyhow!("a check has no size"))?);
            if sha256_hex(original.read_of(unhex(address)?, size)?) != text(pin, "sha256")? {
                bail!("the original bytes at {address} are not the reviewed ones");
            }
        }
        Ok(())
    };
    // A name -> its external definition; a local one only when it is the one definition of that name.
    let native = Image::new(&native_data, 0)?;
    let names = native.names_by_definition()?;
    let (mut functions, mut data) = (load_side(&payload, "functions")?, load_side(&payload, "data")?);
    let list = |record: &Value, key: &str| -> Vec<Value> { record.get(key).and_then(Value::as_list).map(<[Value]>::to_vec).unwrap_or_default() };
    let was = |previous: &Option<(u64, Option<String>)>| -> Value { previous.as_ref().and_then(|p| p.1.clone()).into() };
    let mut applied: IndexMap<&str, Vec<Value>> = ["functions", "functions_absent", "data", "data_inferred", "sites", "aligned", "known_good", "known_good_replaced", "known_good_absent", "stub_list"].into_iter().map(|k| (k, Vec::new())).collect();
    for path in &args.aligned {
        let record = pyjson::load(path)?;
        for pair in list(&record, "data") {
            // kclasses: class objects by name
            let (orig, symbol) = (text(&pair, "original")?, text(&pair, "native_symbol")?);
            let Some(address) = names.get(symbol).filter(|_| !data.contains_key(orig)) else { continue };
            data.insert(orig.to_string(), (*address, Some(symbol.to_string())));
            applied["data_inferred"].push(row(&[("original", orig.into()), ("native_symbol", symbol.into())]));
        }
        for pair in list(&record, "pairs") {
            let (orig, symbol) = (text(&pair, "original")?, text(&pair, "native_symbol")?);
            let Some(address) = names.get(symbol).filter(|_| !functions.contains_key(orig)) else { continue };
            functions.insert(orig.to_string(), (*address, Some(symbol.to_string())));
            applied["aligned"].push(row(&[("original", orig.into()), ("native_symbol", symbol.into()), ("score", pair.get("score").cloned().ok_or_else(|| anyhow!("{path}: a pair has no score"))?)]));
        }
    }
    for record in list(&reviewed, "functions") {
        check(&record)?;
        let (orig, symbol) = (text(&record, "original")?, text(&record, "native_symbol")?);
        let Some(address) = names.get(symbol) else {
            if truthy(record.get("optional")) {
                // a carried-over identity of a function this configuration lacks
                applied["functions_absent"].push(symbol.into());
                continue;
            }
            bail!("{symbol} is not in this kernel");
        };
        let previous = functions.insert(orig.to_string(), (*address, Some(symbol.to_string())));
        applied["functions"].push(row(&[("original", orig.into()), ("native_symbol", symbol.into()), ("replaced", was(&previous))]));
    }
    for record in list(&reviewed, "data") {
        check(&record)?;
        let (orig, symbol) = (text(&record, "original")?, text(&record, "native_symbol")?);
        let Some(address) = names.get(symbol) else { bail!("{symbol} is not in this kernel") };
        data.insert(orig.to_string(), (*address, Some(symbol.to_string())));
        applied["data"].push(row(&[("original", orig.into()), ("native_symbol", symbol.into())]));
    }
    // The known-good pairs over everything above, and the stub list unpaired (README "Known-good pairs").
    let mut reviewed_names: HashMap<String, String> = HashMap::new();
    for kind in ["functions", "data"] {
        for r in list(&reviewed, kind) {
            reviewed_names.insert(text(&r, "original")?.to_string(), text(&r, "native_symbol")?.to_string());
        }
    }
    if let Some(path) = &args.pairs {
        let known = pyjson::load(path)?;
        if known.get("kernelcache_sha256").and_then(Value::as_str) != Some(reviewed_sha) {
            bail!("the known-good pairs are for another kernelcache");
        }
        for kind in ["functions", "data"] {
            let side = if kind == "functions" { &mut functions } else { &mut data };
            for entry in list(&known, kind) {
                let (orig, full) = (text(&entry, "original")?, text(&entry, "native")?);
                let (name, offset) = full.split_once('+').unwrap_or((full, ""));
                let Some(found) = names.get(name) else {
                    if truthy(entry.get("absent")) {
                        let previous = side.shift_remove(orig);
                        applied["known_good_absent"].push(row(&[("original", orig.into()), ("native", full.into()), ("was", was(&previous))]));
                        continue;
                    }
                    bail!("{name} (known-good pair of {orig}) is not in this kernel");
                };
                let address = found + if offset.is_empty() { 0 } else { unhex(offset)? };
                let previous = side.get(orig).cloned();
                if let Some(reviewed_name) = reviewed_names.get(orig) {
                    if names.get(reviewed_name) != Some(found) {
                        println!("emit: {orig}: reviewed {reviewed_name}, known-good {full}");
                    }
                }
                if previous.as_ref().is_some_and(|p| p.0 == address) {
                    continue;
                }
                side.insert(orig.to_string(), (address, Some(if kind == "functions" { name } else { full }.to_string())));
                applied[if previous.is_some() { "known_good_replaced" } else { "known_good" }].push(row(&[("original", orig.into()), ("native", full.into()), ("was", was(&previous))]));
            }
        }
    }
    if let Some(path) = &args.stubs {
        let stubs = pyjson::load(path)?;
        for kind in ["functions", "data"] {
            let side = if kind == "functions" { &mut functions } else { &mut data };
            for entry in list(&stubs, kind) {
                let orig = text(&entry, "original")?;
                if let Some(previous) = side.shift_remove(orig) {
                    applied["stub_list"].push(row(&[("original", orig.into()), ("was", previous.1.into())]));
                }
            }
        }
    }
    let mut early: IndexMap<String, u64> = IndexMap::new();
    if let Some(record) = reviewed.get("early_security_init").filter(|r| truthy(Some(r))) {
        check(record)?;
        early.insert(text(record, "bundle")?.to_string(), unhex(text(record, "original")?)?);
    }

    // fileset_emit.emit_images
    let (raw_sha, native_sha) = (sha256_hex(&raw), sha256_hex(&native_data));
    let inputs = payload.get("inputs");
    let input = |key: &str| inputs.and_then(|i| i.get(key)).and_then(Value::as_str);
    if inputs.and_then(Value::as_dict).map(<[_]>::len) != Some(3) || input("kernelcache_sha256") != Some(&raw_sha) || input("native_sha256") != Some(&native_sha) || input("entry") != Some(KERNEL) {
        bail!("Pairing inputs do not match these images; rerun the match step");
    }
    let signature = matcher_signature(xnuprelink)?;
    if payload.get("matcher_version").and_then(Value::as_int) != Some(MATCHER_VERSION) || payload.get("matcher_signature").and_then(Value::as_str) != Some(&signature) {
        bail!("Pairing predates the current matching checks; rerun the match step");
    }
    let cache0 = Image::new(&raw, 0)?;
    if cache0.kind != 12 || native.kind != 2 {
        bail!("Expected an MH_FILESET collection and an MH_EXECUTE native kernel");
    }
    let mut pairs: HashMap<u64, u64> = HashMap::new();
    for side in [&functions, &data] {
        for (key, (address, _)) in side {
            pairs.insert(unhex(key)?, *address);
        }
    }
    let (_, metadata) = prelink_info(&cache0)?;
    let closure = dependency_closure(&metadata, &roots)?;
    for (identifier, address) in &early {
        let entry = cache0.entry(identifier)?;
        if !entry.segments.iter().any(|s| s.name == "__TEXT_EXEC" && s.va <= *address && *address < s.va + s.size) {
            bail!("Early security initializer {address:#x} is outside {identifier} code");
        }
    }
    let (rebased, collection_slide, dropped_personalities) = rebase_collection(&cache0, metadata, native.base, closure.as_ref(), &early, &roots)?;
    let cache = Image::new(&rebased, 0)?;
    let pairs: HashMap<u64, u64> = pairs.into_iter().map(|(address, target)| (address.wrapping_add(collection_slide), target)).collect();
    // Reviewed per-site bindings: XnuPrelink's fixtures for this kernelcache, then OneiOS's reviewed "sites".
    let mut site_pairs: HashMap<u64, u64> = HashMap::new();
    let native_symbols = native.symbols()?;
    let mut last_names: HashMap<&str, u64> = HashMap::new();
    for (address, symbols) in &native_symbols {
        for name in symbols {
            last_names.insert(name, *address);
        }
    }
    for record in reviewed_records(xnuprelink, &raw_sha, KERNEL)? {
        for entry in list(&record, "sites") {
            if !truthy(entry.get("evidence")) {
                bail!("Reviewed site requires recorded evidence");
            }
            let symbol = text(&entry, "native_symbol")?;
            let Some(address) = last_names.get(symbol) else { bail!("Reviewed site names a missing native symbol: {symbol}") };
            site_pairs.insert(unhex(text(&entry, "site")?)?.wrapping_add(collection_slide), *address);
        }
    }
    for record in list(&reviewed, "sites") {
        check(&record)?;
        let (site, symbol) = (text(&record, "site")?, text(&record, "native_symbol")?);
        let Some(address) = names.get(symbol) else { bail!("{symbol} is not in this kernel") };
        site_pairs.insert(unhex(site)?.wrapping_add(collection_slide), *address);
        applied["sites"].push(row(&[("site", site.into()), ("native_symbol", symbol.into())]));
    }
    let stubs: Vec<u64> = native_symbols.iter().filter(|(_, names)| names.iter().any(|n| n == STUB)).map(|(a, _)| *a).collect();
    if stubs.len() != 1 {
        bail!("Native kernel has no unique {STUB}");
    }
    let old = cache.entry(KERNEL)?;
    let mut owner_ranges = Vec::new();
    for (name, (_, entry_at)) in &cache.entries {
        for (begin, end) in ranges(&Image::new(&rebased, *entry_at as usize)?) {
            owner_ranges.push((begin, end, name.clone()));
        }
    }
    owner_ranges.sort();
    let native_fixups = native.fixups()?;
    let layout = Layout::new(&cache, &native, closure.as_ref(), cache.fixups()?.len() + native_fixups.len(), native.function_starts()?.len())?;
    let moved = layout.moved;
    let mut emit = Emit {
        data: rebased.clone(),
        cache: &cache,
        native: &native,
        pairs,
        closure: closure.as_ref(),
        old_ranges: ranges(&old),
        owner_ranges,
        site_pairs,
        stub: Some(stubs[0]),
        stubbed: BTreeMap::new(),
        missing: BTreeMap::new(),
        old_starts: old.function_starts()?.into_iter().collect(),
        layout,
        fileoff: 0,
        table_cursor: 0,
        fixups: (0, 0),
        starts: (0, 0),
        native_pointers: BTreeMap::new(),
        original_branches: Vec::new(),
        report: Report::default(),
    };
    emit.place_kernel()?;
    emit.rewrite_native_chains(&native_fixups)?;
    emit.rebuild_linkedit()?;
    emit.rewrite_branches()?;
    emit.seal_components()?;
    emit.patch_entry_header()?;
    emit.patch_headers()?;
    let Emit { data: mut image, report: statistics, missing, stubbed, original_branches, .. } = emit;
    // The collection's header keeps Apple's CPU subtype; its kernel is now ours, and the loader wants the two to
    // agree (XNU does not read the collection's subtype): the header takes the native kernel's.
    let (collection_subtype, native_subtype) = (u32_at(&image, 8), u32_at(&native_data, 8));
    put32(&mut image, 8, native_subtype);

    let mut report = Value::dict();
    report.set("format", "MH_FILESET");
    report.set("inputs", row(&[("kernelcache_sha256", raw_sha.as_str().into()), ("native_sha256", native_sha.as_str().into()), ("entry", KERNEL.into())]));
    report.set("matcher_version", Value::Int(MATCHER_VERSION));
    report.set("matcher_signature", &signature);
    report.set("reference_sha256", payload.get("reference_sha256").cloned().unwrap_or(Value::Null));
    report.set("closure", closure.as_ref().map(|c| {
        let mut sorted: Vec<String> = c.iter().cloned().collect();
        sorted.sort();
        sorted
    }));
    report.set("collection_base", hex(cache.base));
    report.set("collection_slide", hex(collection_slide));
    report.set("native_link_address", hex(native.base));
    report.set("statistics", Value::Dict(statistics.0));
    report.set("native_original_branches", Value::List(original_branches));
    report.set("missing", counts_value(&missing));
    report.set("stubbed", counts_value(&stubbed));
    report.set("stub_symbol", STUB);
    report.set("personalities_dropped", dropped_personalities);
    report.set("early_security_init", Value::Dict(early.iter().map(|(k, v)| (k.clone(), Value::Str(hex(*v)))).collect()));
    report.set("missing_address_space", "output collection");
    report.set("mapping_complete", missing.is_empty());
    report.set("runtime_qualified", false);
    report.set("loadable", false);
    report.set("candidate_bytes", Value::Int(image.len() as i128));
    report.set("candidate_sha256", sha256_hex(&image));
    report.set("collection_cpusubtype", row(&[("original", hex(collection_subtype as u64).into()), ("written", hex(native_subtype as u64).into())]));
    report.set("kernelcache", resolved(&args.kernelcache)?);
    report.set("native", resolved(&args.native)?);
    report.set("map", resolved(&args.map)?);
    report.set("roots", roots_value);
    report.set("reviewed", resolved(&args.reviewed)?);
    report.set("reviewed_applied", Value::Dict(applied.into_iter().map(|(k, v)| (k.to_string(), Value::List(v))).collect()));
    report.set("kernel_data_moved", moved.map(|(size, room, address)| row(&[("size", hex(size).into()), ("room", hex(room).into()), ("address", hex(address).into())])));
    report.set("original_kernelcache_sha256", reviewed_sha);
    report.set("stubbed_functions", Value::Int(stubbed.values().flat_map(|t| t.keys()).collect::<HashSet<_>>().len() as i128));
    report.set("missing_data", Value::Int(missing.values().flat_map(|t| t.keys()).collect::<HashSet<_>>().len() as i128));
    // fileset_emit.write_candidate(allow_missing=True)
    report.set("output", resolved(&args.output)?);
    private_write(&args.output, &image)?;
    report.set("candidate_written", true);
    report.set("allow_missing", true);
    private_write(&args.report, (pyjson::dumps(&report, Some(2), false) + "\n").as_bytes())?;
    println!("wrote {} ({} bytes)", args.output, image.len());
    let total: i128 = missing.values().flat_map(|t| t.values()).sum();
    let stubbed: usize = stubbed.values().map(BTreeMap::len).sum();
    println!("unresolved closure references: {total}; stubbed functions: {stubbed}; runtime qualification: pending");
    Ok(())
}
