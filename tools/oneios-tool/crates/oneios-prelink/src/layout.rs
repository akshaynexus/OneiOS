//! Segment placement and the linker's split-segment relocations for the collection: a port of XnuPrelink's
//! fileset_layout.py at pin 9b950f2, with the two changes kernel/prelink/emit.py makes to it at run time built in
//! (`__DATA_KERNEL`: the kernel's `__DATA` moves to the end of the collection when it does not fit in front of
//! the first kept kext's data).

use crate::image::{hex, put32, signed, u32_at, Fix, Fixups, Image, Section, Segment};
use crate::records::Counter;
use anyhow::{anyhow, bail, Result};
use indexmap::IndexMap;
use oneios_common::pyjson::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

pub const PAGE: u64 = 0x4000;

pub fn align(value: u64, alignment: u64) -> u64 {
    value.wrapping_add(alignment - 1) & alignment.wrapping_neg()
}

/// The linker's split-segment v2 records: (kind, source section, source offset, target section, target offset).
fn split_references(image: &Image) -> Result<Vec<(u64, u64, u64, u64, u64)>> {
    let (found, _) = image.command(0x1e).ok_or_else(|| anyhow!("Native image has no split-segment relocation information"))?;
    let (offset, size) = (u32_at(image.data, found + 8) as usize, u32_at(image.data, found + 12) as usize);
    let blob = image.data.get(offset..offset + size).filter(|b| !b.is_empty() && b[0] == 0x7f).ok_or_else(|| anyhow!("Expected complete split-segment v2 information"))?;
    let mut at = 1usize;
    let mut uleb = || -> Result<u64> {
        let mut result: u128 = 0;
        for shift in (0..70).step_by(7) {
            let byte = *blob.get(at).ok_or_else(|| anyhow!("Truncated split-segment ULEB"))?;
            at += 1;
            result |= ((byte & 127) as u128) << shift;
            if byte & 128 == 0 {
                return u64::try_from(result).map_err(|_| anyhow!("Overflowing split-segment ULEB"));
            }
        }
        bail!("Overflowing split-segment ULEB")
    };
    let mut out = Vec::new();
    for _ in 0..uleb()? {
        let (source, target, count) = (uleb()?, uleb()?, uleb()?);
        if source.max(target) > image.sections.len() as u64 {
            bail!("Split-segment section ordinal is out of bounds");
        }
        let mut target_offset = 0u64;
        for _ in 0..count {
            target_offset += uleb()?;
            for _ in 0..uleb()? {
                let (kind, count_sources) = (uleb()?, uleb()?);
                if !(1..=13).contains(&kind) {
                    bail!("Invalid split-segment kind {kind}");
                }
                let mut source_offset = 0u64;
                for _ in 0..count_sources {
                    source_offset += uleb()?;
                    out.push((kind, source, source_offset, target, target_offset));
                }
            }
        }
    }
    if blob[at..].iter().any(|b| *b != 0) {
        bail!("Unexpected bytes after split-segment records");
    }
    Ok(out)
}

pub fn encode_uleb(mut value: u64, out: &mut Vec<u8>) {
    while value >= 128 {
        out.push((value & 127) as u8 | 128);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Fit native segments around retained kexts, preserving their relative addresses.
pub struct Layout {
    pub base: u64,
    native_base: u64,
    native_segments: Vec<Segment>,
    native_sections: Vec<Section>,
    /// The collection's root segments, in the order the load commands are written.
    pub roots: IndexMap<String, Segment>,
    occupied: Vec<(u64, u64)>,
    pub retained_code: Vec<(u64, u64, String)>,
    pub placements: HashMap<String, u64>,
    cursor: u64,
    pub table_begin: u64,
    pub table_end: u64,
    pub size: u64,
    /// (size, room, address) when the kernel's __DATA went to the end of the collection (emit.py's `moved`).
    pub moved: Option<(u64, u64, u64)>,
}

impl Layout {
    pub fn new(cache: &Image, native: &Image, closure: Option<&HashSet<String>>, fixup_count: usize, function_starts: usize) -> Result<Layout> {
        let roots: IndexMap<String, Segment> = cache.segments.iter().map(|s| (s.name.clone(), s.clone())).collect();
        let (mut occupied, mut retained_code) = (Vec::new(), Vec::new());
        for (name, (_, offset)) in &cache.entries {
            if name == "com.apple.kernel" || closure.is_some_and(|c| !c.contains(name)) {
                continue;
            }
            for segment in &Image::new(cache.data, *offset as usize)?.segments {
                if segment.size != 0 && segment.name != "__LINKEDIT" {
                    occupied.push((segment.va, segment.va + segment.size));
                }
                if segment.name == "__TEXT_EXEC" && segment.filesize != 0 {
                    retained_code.push((segment.va, segment.va + segment.filesize, name.clone()));
                }
            }
        }
        let top = cache.segments.iter().map(|s| (s.va + s.size).wrapping_sub(cache.base)).max().unwrap_or(0);
        let mut l = Layout {
            base: cache.base,
            native_base: native.base,
            native_segments: native.segments.clone(),
            native_sections: native.sections.clone(),
            roots,
            occupied,
            retained_code,
            placements: HashMap::new(),
            cursor: align((cache.data.len() as u64).max(top), PAGE),
            table_begin: 0,
            table_end: 0,
            size: 0,
            moved: None,
        };
        let root = |l: &Layout, name: &str| l.roots.get(name).cloned().ok_or_else(|| anyhow!("the collection has no {name}"));
        let text = l.hole("__PRELINK_TEXT", l.segment("__TEXT")?.size)?;
        l.reserve("__TEXT", text);
        l.reserve("__DATA_CONST", root(&l, "__DATA_CONST")?.va);
        l.reserve("__LASTDATA_CONST", align(l.end("__DATA_CONST")?, PAGE));
        // All native code follows selected kext code; XNU derives the kext-code range from the start of native
        // __TEXT_EXEC.
        let code = root(&l, "__TEXT_EXEC")?;
        let end_kept = l.occupied.iter().filter(|(begin, _)| code.va <= *begin && *begin < code.va + code.size).map(|r| r.1).chain([code.va]).max().unwrap();
        l.reserve("__TEXT_EXEC", align(end_kept, PAGE));
        l.reserve("__KLD", align(l.end("__TEXT_EXEC")?, PAGE));
        l.reserve("__LAST", l.end("__KLD")?);
        let code_end = l.end("__KLD")?;
        // The old kernel boot segment and prelink XML are replaceable, provided no retained kext uses them.
        if code_end > root(&l, "__DATA")?.va {
            bail!("Native code would collide with collection data");
        }
        l.roots.shift_remove("__TEXT_BOOT_EXEC");
        let code = l.roots.get_mut("__TEXT_EXEC").unwrap();
        code.size = code_end - code.va;
        code.filesize = code.size;
        l.reserve("__DATA", root(&l, "__DATA")?.va);
        for name in ["__LINKINFO", "__LINKEDIT"] {
            let address = l.allocate(l.segment(name)?.size);
            l.reserve(name, address);
        }
        // Reserve enough root-linkedit space for new starts/fixups tables: derived from site counts.
        l.table_begin = l.cursor;
        l.allocate(align(fixup_count as u64 * 2 + function_starts as u64 * 10 + 0x10000, PAGE));
        l.table_end = l.cursor;
        let cursor = l.cursor;
        let link = l.roots.get_mut("__LINKEDIT").ok_or_else(|| anyhow!("the collection has no __LINKEDIT"))?;
        link.size = cursor - link.fileoff;
        link.filesize = link.size;
        let info_size = root(&l, "__PRELINK_INFO")?.size;
        let address = l.allocate(info_size);
        let base = l.base;
        let info = l.roots.get_mut("__PRELINK_INFO").unwrap();
        info.va = address;
        info.fileoff = address - base;
        for name in ["__KLDDATA", "__BOOTDATA"] {
            let mut segment = l.segment(name)?.clone();
            let address = l.allocate(segment.size);
            l.reserve(name, address);
            (segment.va, segment.fileoff, segment.filesize) = (address, address - base, segment.size);
            l.roots.insert(name.to_string(), segment);
        }
        // Zero-sized markers have distinct semantics even when their old addresses alias a real segment.
        for segment in native.segments.iter() {
            if l.placements.contains_key(&segment.name) {
                continue;
            }
            if segment.size != 0 {
                bail!("Unclassified native segment {}", segment.name);
            }
            let address = if segment.name == "__PRELINK_DATA" { l.end("__DATA")? } else { l.end("__KLD")? };
            l.reserve(&segment.name, address);
        }
        l.size = l.cursor;
        l.validate()?;
        Ok(l)
    }

    /// The native segment `name` (`__DATA_KERNEL` is the kernel's `__DATA`).
    pub fn segment(&self, name: &str) -> Result<&Segment> {
        let name = if name == "__DATA_KERNEL" { "__DATA" } else { name };
        self.native_segments.iter().find(|s| s.name == name).ok_or_else(|| anyhow!("the kernel has no segment {name}"))
    }

    fn allocate(&mut self, size: u64) -> u64 {
        let address = self.base + self.cursor;
        self.cursor = align(self.cursor + size, PAGE);
        address
    }

    fn reserve(&mut self, name: &str, address: u64) {
        self.placements.insert(name.to_string(), address);
    }

    pub fn placed(&self, name: &str) -> Result<u64> {
        self.placements.get(name).copied().ok_or_else(|| anyhow!("the segment {name} has no place in the collection"))
    }

    fn end(&self, name: &str) -> Result<u64> {
        Ok(self.placed(name)? + self.segment(name)?.size)
    }

    fn hole(&self, root_name: &str, size: u64) -> Result<u64> {
        let root = self.roots.get(root_name).ok_or_else(|| anyhow!("the collection has no {root_name}"))?;
        let (mut cursor, limit) = (root.va, root.va + root.size);
        let mut occupied = self.occupied.clone();
        occupied.sort_unstable();
        for (begin, end) in occupied {
            if end <= cursor || begin >= limit {
                continue;
            }
            if align(cursor, PAGE) + size <= begin {
                return Ok(align(cursor, PAGE));
            }
            cursor = cursor.max(end);
        }
        if align(cursor, PAGE) + size <= limit {
            return Ok(align(cursor, PAGE));
        }
        bail!("No room for {size:#x} bytes in {root_name}")
    }

    pub fn translate(&self, address: u64) -> Result<u64> {
        let mut matches = self.native_segments.iter().filter(|s| s.va <= address && address < s.va + s.size);
        match (matches.next(), matches.next()) {
            (Some(segment), None) => Ok(self.placed(&segment.name)?.wrapping_add(address - segment.va)),
            _ => bail!("Native address has no unique segment: {address:#x}"),
        }
    }

    /// The address of `offset` in the section with this ordinal (0: the header), before or after the move.
    pub fn section_address(&self, ordinal: u64, offset: i128, new: bool) -> Result<u64> {
        if ordinal == 0 {
            if offset > self.segment("__TEXT")?.size as i128 {
                bail!("Header-relative split-segment offset is out of range");
            }
            return Ok(((if new { self.placed("__TEXT")? } else { self.native_base }) as i128 + offset) as u64);
        }
        let section = self.native_sections.get(ordinal as usize - 1).ok_or_else(|| anyhow!("no section {ordinal} in the kernel"))?;
        if offset > section.size as i128 {
            bail!("Split-segment offset exceeds section {ordinal}");
        }
        let delta = if new { self.placed(&section.segment)? as i128 - self.segment(&section.segment)?.va as i128 } else { 0 };
        Ok((section.va as i128 + offset + delta) as u64)
    }

    /// The file offset in the collection of a file offset of the kernel.
    pub fn file(&self, old_offset: u64, length: u64) -> Result<u64> {
        let mut matches = self.native_segments.iter().filter(|s| s.filesize != 0 && s.fileoff <= old_offset && old_offset + length <= s.fileoff + s.filesize);
        match (matches.next(), matches.next()) {
            (Some(segment), None) => Ok(self.placed(&segment.name)? - self.base + old_offset - segment.fileoff),
            _ => bail!("Native file offset has no unique segment: {old_offset:#x}"),
        }
    }

    fn validate(&mut self) -> Result<()> {
        // emit.py: the kernel's __DATA to the end of the collection when it does not fit where Apple's was.
        let data = self.segment("__DATA")?.clone();
        let begin = self.placed("__DATA")?;
        if self.occupied.iter().any(|(b, e)| begin < *e && *b < begin + data.size) {
            let room = self.occupied.iter().filter(|(b, _)| *b >= begin).map(|r| r.0).min().ok_or_else(|| anyhow!("no kext data follows the kernel's"))? - begin;
            let address = self.allocate(data.size);
            self.reserve("__DATA", address);
            let marker = self.roots["__DATA"].va;
            self.reserve("__PRELINK_DATA", marker);
            let mut root = data.clone();
            (root.name, root.va, root.fileoff, root.filesize) = ("__DATA_KERNEL".to_string(), address, address - self.base, data.size);
            self.roots.insert("__DATA_KERNEL".to_string(), root);
            self.size = self.cursor;
            self.moved = Some((data.size, room, address));
        }
        let mut placed: Vec<(u64, u64)> = Vec::new();
        for segment in &self.native_segments {
            if segment.size == 0 {
                continue;
            }
            let (begin, end) = (self.placed(&segment.name)?, self.end(&segment.name)?);
            if begin % PAGE != 0 {
                bail!("Unaligned native segment");
            }
            if self.occupied.iter().chain(&placed).any(|(b, e)| begin < *e && *b < end) {
                bail!("Native {} overlaps a retained segment", segment.name);
            }
            if !self.roots.values().any(|s| s.va <= begin && end <= s.va + s.size) {
                bail!("Native {} is outside collection mappings", segment.name);
            }
            placed.push((begin, end));
        }
        let mut ordered: Vec<(u64, u64)> = self.roots.values().filter(|s| s.size != 0).map(|s| (s.va, s.va + s.size)).collect();
        ordered.sort_unstable();
        if ordered.windows(2).any(|w| w[0].1 > w[1].0) {
            bail!("Root mappings overlap");
        }
        let constants = &self.roots["__DATA_CONST"];
        let first = self.occupied.iter().filter(|(b, _)| constants.va <= *b && *b < constants.va + constants.size).map(|r| r.0).min().ok_or_else(|| anyhow!("no kept kext has constants"))?;
        if self.end("__LASTDATA_CONST")? > first {
            bail!("Native constants follow retained kext constants");
        }
        Ok(())
    }

    pub fn describe(&self) -> Result<Value> {
        let mut out = Value::dict();
        for s in &self.native_segments {
            let mut row = Value::dict();
            row.set("old", hex(s.va));
            row.set("new", hex(self.placed(&s.name)?));
            row.set("size", hex(s.size));
            out.set(&s.name, row);
        }
        Ok(out)
    }
}

/// Retarget BTI/B adapters to fixed kext addresses after moving native text (`<stub>.original` N_ABS symbols).
pub fn relocate_original_stubs(data: &mut [u8], layout: &Layout, native: &Image) -> Result<Vec<Value>> {
    let mut names: HashMap<String, HashSet<u64>> = HashMap::new();
    for (address, symbols) in native.symbols()? {
        for name in symbols {
            names.entry(name).or_default().insert(address);
        }
    }
    let mut result = Vec::new();
    for (name, target) in native.absolute_symbols()? {
        let Some(symbol) = name.strip_suffix(".original") else { continue };
        let stub = match names.get(symbol) {
            Some(stubs) if stubs.len() == 1 => *stubs.iter().next().unwrap(),
            _ => bail!("Absolute original target has no unique native stub: {name}"),
        };
        let raw = native.read_of(stub, 8)?;
        let (bti, branch) = (u32_at(raw, 0), u32_at(raw, 4));
        if bti != 0xd503_245f || branch & 0xfc00_0000 != 0x1400_0000 {
            bail!("Expected BTI/B original stub: {name}");
        }
        if (stub + 4).wrapping_add((signed((branch & 0x3ff_ffff) as u64, 26) * 4) as u64) != target {
            bail!("Original stub disagrees with absolute symbol: {name}");
        }
        let owners: Vec<&String> = layout.retained_code.iter().filter(|(begin, end, _)| *begin <= target && target < *end).map(|r| &r.2).collect();
        if owners.len() != 1 {
            bail!("Original target is not retained executable code: {name} -> {target:#x}; rebuild adapters without native-slide compensation");
        }
        let source = layout.translate(stub + 4)?;
        let displacement = target as i128 - source as i128;
        if displacement % 4 != 0 || !(-(1i128 << 27)..1i128 << 27).contains(&displacement) {
            bail!("Original branch out of range: {name}");
        }
        put32(data, (source - layout.base) as usize, 0x1400_0000 | ((displacement >> 2) as u32 & 0x3ff_ffff));
        let mut row = Value::dict();
        row.set("symbol", symbol);
        row.set("source", hex(source));
        row.set("target", hex(target));
        row.set("owner", owners[0]);
        result.push(row);
    }
    Ok(result)
}

/// Apply the linker's exact records to the kernel's copy in the collection; the pointers (site -> fixup, at their
/// new addresses) go to the root chain writer.
pub fn relocate_native(data: &mut [u8], layout: &Layout, native: &Image, fixups: &Fixups) -> Result<(BTreeMap<u64, Fix>, Counter)> {
    let base = layout.base;
    let (mut pointers, mut counts, mut written): (BTreeMap<u64, Fix>, Counter, HashSet<u64>) = (BTreeMap::new(), Counter::default(), HashSet::new());
    for (kind, si, so, ti, to) in split_references(native)? {
        let source = layout.section_address(si, so as i128, false)?;
        let target = layout.section_address(ti, to as i128, false)?;
        let new_source = layout.section_address(si, so as i128, true)?;
        let new_target = layout.section_address(ti, to as i128, true)?;
        let width: u64 = if matches!(kind, 2 | 4 | 13) { 8 } else { 4 };
        let at = (new_source - base) as usize;
        let raw = native.read_of(source, width)?;
        let original: u64 = if width == 8 { u64::from_le_bytes(raw.try_into().unwrap()) } else { u32_at(raw, 0) as u64 };
        if !written.insert(source) {
            bail!("Duplicate split-segment site {source:#x}");
        }
        if matches!(kind, 2 | 13) {
            match fixups.get(&source) {
                Some(fix) if fix.target == target => {
                    pointers.insert(new_source, Fix { target: new_target, ..*fix });
                }
                _ => bail!("Split-segment pointer disagrees with fixup at {source:#x}"),
            }
            counts.add("pointers", 1);
            continue;
        }
        let moved = new_target as i128 - target as i128;
        let value: u64 = match kind {
            3 | 4 => {
                let bits = width as u32 * 8;
                let value = signed(original, bits) as i128 + moved - (new_source as i128 - source as i128);
                if !(-(1i128 << (bits - 1))..1i128 << (bits - 1)).contains(&value) {
                    bail!("Relative value overflow at {source:#x}");
                }
                (value & ((1i128 << bits) - 1)) as u64
            }
            5 => {
                if original & 0x9f00_0000 != 0x9000_0000 {
                    bail!("Expected ADRP at {source:#x}");
                }
                let previous = (source & !4095).wrapping_add((signed(((original >> 5) & 0x7ffff) << 2 | ((original >> 29) & 3), 21) << 12) as u64);
                if previous != target & !4095 {
                    bail!("ADRP target disagrees with linker at {source:#x}");
                }
                let immediate = ((new_target & !4095) as i128 - (new_source & !4095) as i128) >> 12;
                if !(-(1i128 << 20)..1i128 << 20).contains(&immediate) {
                    bail!("Relocated ADRP out of range");
                }
                let immediate = immediate as u64;
                (original & 0x9f00_001f) | (((immediate >> 2) & 0x7ffff) << 5) | ((immediate & 3) << 29)
            }
            6 => {
                let scale = if original & 0xffc0_0000 == 0x9100_0000 {
                    0
                } else if original & 0x3b00_0000 == 0x3900_0000 {
                    if original & 0x0480_0000 == 0x0480_0000 { 4 } else { original >> 30 }
                } else {
                    bail!("Expected ADD or load/store at {source:#x}")
                };
                let low = new_target & 4095;
                if low % (1 << scale) != 0 || (((original >> 10) & 4095) << scale) & 4095 != target & 4095 {
                    bail!("Page offset disagrees with linker at {source:#x}");
                }
                // Page-aligned segment moves preserve the low bits.
                if moved.rem_euclid(PAGE as i128) != 0 {
                    bail!("Split-segment move must preserve page offsets");
                }
                original
            }
            7 => {
                if original & 0x7c00_0000 != 0x1400_0000 || source.wrapping_add((signed(original & 0x3ff_ffff, 26) * 4) as u64) != target {
                    bail!("Branch disagrees with linker at {source:#x}");
                }
                let displacement = new_target as i128 - new_source as i128;
                if displacement % 4 != 0 || !(-(1i128 << 27)..1i128 << 27).contains(&displacement) {
                    bail!("Relocated branch out of range");
                }
                (original & 0xfc00_0000) | ((displacement >> 2) as u64 & 0x3ff_ffff)
            }
            _ => bail!("Unsupported native split-segment kind {kind}"),
        };
        data[at..at + width as usize].copy_from_slice(&value.to_le_bytes()[..width as usize]);
        counts.add(&format!("kind_{kind}"), 1);
    }
    for (site, pointer) in fixups {
        let new_site = layout.translate(*site)?;
        if let std::collections::btree_map::Entry::Vacant(slot) = pointers.entry(new_site) {
            slot.insert(Fix { target: layout.translate(pointer.target)?, ..*pointer });
            counts.add("pointers_without_split_record", 1);
        }
    }
    Ok((pointers, counts))
}
