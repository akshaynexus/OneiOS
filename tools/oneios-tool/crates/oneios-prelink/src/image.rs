//! The arm64e kernel image reader every later step uses: a port of XnuPrelink's kc_image.py (`Image`, `scan`,
//! `signed`) and fileset_shape.py (`checked_shape`). Maps keep the order Python's dicts have (the order of the
//! load commands, the symbol table and the fixup chains), because later steps iterate them.
//!
//! No Mach-O library is used here on purpose: the steps need the exact walk order of the chains, the formats 7
//! and 8 of kernel collections, and segment file offsets that are absolute inside a fileset. `object` reads
//! none of the three (checked 2026-11-12: its chained-fixup support is the dyld user formats).

use anyhow::{anyhow, bail, Result};
use indexmap::IndexMap;

pub const LC_SEGMENT_64: u32 = 0x19;
pub const LC_SYMTAB: u32 = 0x2;
pub const LC_FUNCTION_STARTS: u32 = 0x26;
pub const LC_DYLD_CHAINED_FIXUPS: u32 = 0x8000_0034;
pub const LC_FILESET_ENTRY: u32 = 0x8000_0035;
pub const LC_UNIXTHREAD: u32 = 0x5;
const PTR_ARM64E_KERNEL: u16 = 7;
const PTR_64_KERNEL_CACHE: u16 = 8;

pub fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(data[at..at + 2].try_into().unwrap())
}
pub fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
pub fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}
pub fn put32(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
pub fn put64(data: &mut [u8], at: usize, value: u64) {
    data[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// The bytes up to the first NUL at or after `at`, as text (Python's `data[at:data.index(b'\0', at)].decode()`).
pub fn cstr_at(data: &[u8], at: usize) -> Result<String> {
    let end = data[at..].iter().position(|b| *b == 0).ok_or_else(|| anyhow!("no NUL after byte {at}"))?;
    Ok(std::str::from_utf8(&data[at..at + end]).map_err(|_| anyhow!("a name at byte {at} is not UTF-8"))?.to_string())
}

/// A 16-byte name field: trailing NULs stripped.
fn name16(raw: &[u8]) -> String {
    let mut end = raw.len();
    while end > 0 && raw[end - 1] == 0 {
        end -= 1;
    }
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

#[derive(Clone, Debug)]
pub struct Segment {
    pub name: String,
    pub va: u64,
    pub size: u64,
    pub fileoff: u64,
    pub filesize: u64,
    pub command_offset: usize,
}

#[derive(Clone, Debug)]
pub struct Section {
    pub segment: String,
    pub name: String,
    pub va: u64,
    pub size: u64,
}

/// One chained fixup: Python's (target, auth, key, diversity, address diversity). Without `auth` the last three
/// are None there and zero here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fix {
    pub target: u64,
    pub auth: bool,
    pub key: u8,
    pub diversity: u16,
    pub addr_div: bool,
}

impl Fix {
    /// Python's `pointer[1:]`.
    pub fn meta(&self) -> (bool, u8, u16, bool) {
        (self.auth, self.key, self.diversity, self.addr_div)
    }
    /// Python's `pointer[1:3] == (1, 0)`: an authenticated pointer with key IA.
    pub fn is_ia(&self) -> bool {
        self.auth && self.key == 0
    }
}

pub type Fixups = IndexMap<u64, Fix>;
pub type Symbols = IndexMap<u64, Vec<String>>;

/// One Mach-O header inside a file; segment file offsets are absolute in that file.
pub struct Image<'a> {
    pub data: &'a [u8],
    pub at: usize,
    pub kind: u32,
    pub segments: Vec<Segment>,
    pub sections: Vec<Section>,
    /// (command, offset, length)
    pub commands: Vec<(u32, usize, usize)>,
    /// name -> (va, file offset), in load-command order
    pub entries: IndexMap<String, (u64, u64)>,
    pub base: u64,
}

impl<'a> Image<'a> {
    pub fn new(data: &'a [u8], at: usize) -> Result<Image<'a>> {
        if data.len() < at + 32 || (u32_at(data, at), u32_at(data, at + 4)) != (0xfeed_facf, 0x100_000c) {
            bail!("Expected a 64-bit ARM Mach-O header");
        }
        let (kind, count) = (u32_at(data, at + 12), u32_at(data, at + 16));
        let (mut segments, mut sections, mut commands, mut entries) = (Vec::new(), Vec::new(), Vec::new(), IndexMap::new());
        let mut offset = at + 32;
        for _ in 0..count {
            let (command, length) = (u32_at(data, offset), u32_at(data, offset + 4) as usize);
            commands.push((command, offset, length));
            if command == LC_SEGMENT_64 {
                let name = name16(&data[offset + 8..offset + 24]);
                segments.push(Segment {
                    name,
                    va: u64_at(data, offset + 24),
                    size: u64_at(data, offset + 32),
                    fileoff: u64_at(data, offset + 40),
                    filesize: u64_at(data, offset + 48),
                    command_offset: offset,
                });
                let nsects = u32_at(data, offset + 64) as usize;
                for index in 0..nsects {
                    let s = offset + 72 + 80 * index;
                    sections.push(Section {
                        name: name16(&data[s..s + 16]),
                        segment: name16(&data[s + 16..s + 32]),
                        va: u64_at(data, s + 32),
                        size: u64_at(data, s + 40),
                    });
                }
            } else if command == LC_FILESET_ENTRY {
                let (va, fileoff, name_at) = (u64_at(data, offset + 8), u64_at(data, offset + 16), u32_at(data, offset + 24) as usize);
                entries.insert(cstr_at(data, offset + name_at)?, (va, fileoff));
            }
            offset += length;
        }
        let base = if at == 0 {
            segments.iter().find(|s| s.fileoff == 0 && s.filesize != 0).map(|s| s.va)
        } else {
            segments.first().map(|s| s.va)
        }
        .ok_or_else(|| anyhow!("the image has no segment that maps its header"))?;
        Ok(Image { data, at, kind, segments, sections, commands, entries, base })
    }

    /// The fileset entry `name` as an image.
    pub fn entry(&self, name: &str) -> Result<Image<'a>> {
        let (_, at) = self.entries.get(name).ok_or_else(|| anyhow!("No fileset entry '{name}'"))?;
        Image::new(self.data, *at as usize)
    }

    pub fn command(&self, kind: u32) -> Option<(usize, usize)> {
        self.commands.iter().find(|c| c.0 == kind).map(|c| (c.1, c.2))
    }

    pub fn segment(&self, name: &str) -> Option<&Segment> {
        self.segments.iter().find(|s| s.name == name)
    }

    /// (begin, end) of every mapped segment but __LINKEDIT.
    pub fn mapped(&self) -> Vec<(u64, u64)> {
        self.segments.iter().filter(|s| s.size != 0 && s.name != "__LINKEDIT").map(|s| (s.va, s.va.wrapping_add(s.size))).collect()
    }

    /// The file offset of `va`; None is Python's ValueError "Unbacked address".
    pub fn offset(&self, va: u64, length: u64) -> Option<usize> {
        for s in &self.segments {
            if s.va <= va && va as u128 + length as u128 <= s.va as u128 + s.filesize as u128 {
                return Some((s.fileoff + va - s.va) as usize);
            }
        }
        None
    }

    /// `offset` where Python lets the ValueError end the step.
    pub fn offset_of(&self, va: u64, length: u64) -> Result<usize> {
        self.offset(va, length).ok_or_else(|| anyhow!("Unbacked address {va:#x}"))
    }

    pub fn read(&self, va: u64, length: u64) -> Option<&'a [u8]> {
        let at = self.offset(va, length)?;
        self.data.get(at..at + length as usize)
    }

    pub fn read_of(&self, va: u64, length: u64) -> Result<&'a [u8]> {
        self.read(va, length).ok_or_else(|| anyhow!("Unbacked address {va:#x}"))
    }

    pub fn section(&self, va: u64) -> Option<&Section> {
        self.sections.iter().find(|s| s.va <= va && va < s.va.wrapping_add(s.size))
    }

    /// The printable C string at `va` in a string or constant section. Err is Python's ValueError (a section
    /// without file bytes), which some callers catch.
    pub fn cstring(&self, va: u64) -> Result<Option<&'a str>> {
        let Some(section) = self.section(va) else { return Ok(None) };
        if !(section.segment == "__TEXT" || section.segment == "__DATA_CONST")
            || !(section.name.starts_with("__cstring") || section.name.starts_with("__const") || section.name.starts_with("__os_log"))
        {
            return Ok(None);
        }
        let at = self.offset_of(va, 1)?;
        let limit = (at + 4096).min(self.data.len());
        let Some(len) = self.data[at..limit].iter().position(|b| *b == 0) else { return Ok(None) };
        if len == 0 {
            return Ok(None);
        }
        let raw = &self.data[at..at + len];
        if raw.iter().all(|b| (32..127).contains(b) || *b == 9 || *b == 10) {
            Ok(Some(std::str::from_utf8(raw).unwrap()))
        } else {
            Ok(None)
        }
    }

    /// The symbol table's rows: (name, type byte, section ordinal, value).
    pub fn symtab(&self) -> Result<Vec<(String, u8, u8, u64)>> {
        let Some((at, _)) = self.command(LC_SYMTAB) else { return Ok(Vec::new()) };
        let (symoff, count, stroff) = (u32_at(self.data, at + 8) as usize, u32_at(self.data, at + 12) as usize, u32_at(self.data, at + 16) as usize);
        let mut out = Vec::with_capacity(count);
        for index in 0..count {
            let row = symoff + 16 * index;
            let (strx, kind, sect, value) = (u32_at(self.data, row) as usize, self.data[row + 4], self.data[row + 5], u64_at(self.data, row + 8));
            if kind & 0xe0 != 0 || !matches!(kind & 0x0e, 0x0e | 0x02) {
                continue;
            }
            out.push((cstr_at(self.data, stroff + strx)?, kind, sect, value));
        }
        Ok(out)
    }

    /// Defined section symbols, address -> names, in symbol-table order.
    pub fn symbols(&self) -> Result<Symbols> {
        let mut result: Symbols = IndexMap::new();
        for (name, kind, _, value) in self.symtab()? {
            if kind & 0x0e == 0x0e {
                result.entry(value).or_default().push(name);
            }
        }
        Ok(result)
    }

    /// Named N_ABS values, sorted by name.
    pub fn absolute_symbols(&self) -> Result<std::collections::BTreeMap<String, u64>> {
        let mut result = std::collections::BTreeMap::new();
        for (name, kind, _, value) in self.symtab()? {
            if kind & 0x0e == 0x02 {
                if let Some(old) = result.get(&name) {
                    if *old != value {
                        bail!("Conflicting absolute symbol {name}");
                    }
                }
                result.insert(name, value);
            }
        }
        Ok(result)
    }

    /// name -> its external definition, or its one local one (emit.py and audit.py resolve names this way).
    pub fn names_by_definition(&self) -> Result<std::collections::HashMap<String, u64>> {
        let mut names = std::collections::HashMap::new();
        let mut local: IndexMap<String, std::collections::BTreeSet<u64>> = IndexMap::new();
        for (name, kind, _, value) in self.symtab()? {
            if kind & 0x0e != 0x0e {
                continue;
            }
            if kind & 1 != 0 {
                names.entry(name).or_insert(value);
            } else {
                local.entry(name).or_default().insert(value);
            }
        }
        for (name, values) in local {
            if !names.contains_key(&name) && values.len() == 1 {
                names.insert(name, *values.iter().next().unwrap());
            }
        }
        Ok(names)
    }

    pub fn function_starts(&self) -> Result<Vec<u64>> {
        let Some((found, _)) = self.command(LC_FUNCTION_STARTS) else { return Ok(Vec::new()) };
        let (dataoff, size) = (u32_at(self.data, found + 8) as usize, u32_at(self.data, found + 12) as usize);
        let text = self.segment("__TEXT").ok_or_else(|| anyhow!("the image has no __TEXT"))?.va;
        let (mut starts, mut address, mut at, end) = (Vec::new(), text, dataoff, dataoff + size);
        while at < end {
            let (mut value, mut shift) = (0u64, 0u32);
            loop {
                let byte = self.data[at];
                at += 1;
                if shift < 64 {
                    value |= ((byte & 0x7f) as u64) << shift;
                }
                shift += 7;
                if byte & 0x80 == 0 {
                    break;
                }
            }
            if value == 0 {
                break;
            }
            address = address.wrapping_add(value);
            starts.push(address);
        }
        Ok(starts)
    }

    /// Chained fixups: site va -> fixup, in the order the chains are walked.
    pub fn fixups(&self) -> Result<Fixups> {
        let mut result: Fixups = IndexMap::new();
        let Some((found, _)) = self.command(LC_DYLD_CHAINED_FIXUPS) else { return Ok(result) };
        let data = self.data;
        let dataoff = u32_at(data, found + 8) as usize;
        let (starts_offset, imports) = (u32_at(data, dataoff + 4) as usize, u32_at(data, dataoff + 16));
        if imports != 0 {
            bail!("Chained fixup imports are not supported");
        }
        let table = dataoff + starts_offset;
        let count = u32_at(data, table) as usize;
        for index in 0..count {
            let relative = u32_at(data, table + 4 + 4 * index) as usize;
            if relative == 0 {
                continue;
            }
            let at = table + relative;
            let (page_size, fmt, segment_offset, pages) = (u16_at(data, at + 4) as u64, u16_at(data, at + 6), u64_at(data, at + 8), u16_at(data, at + 20) as usize);
            if fmt != PTR_ARM64E_KERNEL && fmt != PTR_64_KERNEL_CACHE {
                bail!("Unsupported chained pointer format {fmt}");
            }
            for page in 0..pages {
                let start = u16_at(data, at + 22 + 2 * page);
                if start == 0xffff {
                    continue;
                }
                if start & 0x8000 != 0 {
                    bail!("Multi-start pages are not supported");
                }
                let mut site = self.base + segment_offset + page as u64 * page_size + start as u64;
                let mut file = self.offset_of(site, 8)?;
                loop {
                    let value = u64_at(data, file);
                    let auth = value >> 63 != 0;
                    let (target, step);
                    if fmt == PTR_64_KERNEL_CACHE {
                        if (value >> 30) & 3 != 0 {
                            bail!("Only cache level 0 is supported");
                        }
                        target = self.base + (value & 0x3fff_ffff);
                        step = (value >> 51) & 0xfff;
                    } else {
                        if (value >> 62) & 1 != 0 {
                            bail!("Chained binds are not supported");
                        }
                        let offset = if auth { value & 0xffff_ffff } else { (value & 0x7ff_ffff_ffff) | ((value >> 43) & 0xff) << 56 };
                        target = self.base.checked_add(offset).ok_or_else(|| anyhow!("a fixup target at {site:#x} is past 64 bits"))?;
                        step = (value >> 51) & 0x7ff;
                    }
                    let fix = if auth {
                        Fix { target, auth, key: ((value >> 49) & 3) as u8, diversity: ((value >> 32) & 0xffff) as u16, addr_div: (value >> 48) & 1 != 0 }
                    } else {
                        Fix { target, auth, key: 0, diversity: 0, addr_div: false }
                    };
                    result.insert(site, fix);
                    if step == 0 {
                        break;
                    }
                    site += step * 4;
                    file = self.offset_of(site, 8)?;
                }
            }
        }
        Ok(result)
    }
}

pub fn signed(value: u64, bits: u32) -> i64 {
    if value & (1 << (bits - 1)) != 0 {
        (value as i64).wrapping_sub(1i64 << bits)
    } else {
        value as i64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Adr,
    Add,
    Ldst,
}

/// One resolved address: Python's (here, target, kind[, producer, scale]).
#[derive(Clone, Copy, Debug)]
pub struct Ref {
    pub site: u64,
    pub target: u64,
    pub kind: RefKind,
    /// The ADRP that made the page (None for ADR).
    pub producer: Option<u64>,
    pub scale: u32,
}

pub struct Scan {
    /// (site, target, is a BL)
    pub calls: Vec<(u64, u64, bool)>,
    pub refs: Vec<Ref>,
    pub shape: Vec<u32>,
}

/// Branch targets, resolved adrp-based addresses and a relocation-masked shape of one piece of code.
pub fn scan(code: &[u8], pc: u64) -> Scan {
    let (mut calls, mut refs, mut shape) = (Vec::new(), Vec::new(), Vec::with_capacity(code.len() / 4));
    let mut pages: [Option<(u64, u64)>; 32] = [None; 32];
    let len = code.len() as u64;
    let mut offset = 0usize;
    while offset + 3 < code.len() {
        let word = u32_at(code, offset);
        let here = pc.wrapping_add(offset as u64);
        offset += 4;
        let rd = (word & 31) as usize;
        if word & 0x7c00_0000 == 0x1400_0000 {
            let target = here.wrapping_add((signed((word & 0x3ff_ffff) as u64, 26) * 4) as u64);
            calls.push((here, target, word >> 31 != 0));
            shape.push(if pc <= target && target < pc.wrapping_add(len) { word } else { word & 0xfc00_0000 });
            pages = [None; 32];
            continue;
        }
        if word & 0x9f00_0000 == 0x9000_0000 {
            let immediate = signed((((word >> 5) & 0x7ffff) << 2 | ((word >> 29) & 3)) as u64, 21);
            pages[rd] = Some(((here & !4095).wrapping_add((immediate << 12) as u64), here));
            shape.push(word & 0x9f00_001f);
            continue;
        }
        if word & 0x9f00_0000 == 0x1000_0000 {
            let immediate = signed((((word >> 5) & 0x7ffff) << 2 | ((word >> 29) & 3)) as u64, 21);
            refs.push(Ref { site: here, target: here.wrapping_add(immediate as u64), kind: RefKind::Adr, producer: None, scale: 0 });
            shape.push(word & 0x9f00_001f);
            pages[rd] = None;
            continue;
        }
        let base = ((word >> 5) & 31) as usize;
        if word & 0xff80_0000 == 0x9100_0000 {
            if let Some((page, producer)) = pages[base] {
                let shift = if word & 0x40_0000 != 0 { 12 } else { 0 };
                refs.push(Ref { site: here, target: page.wrapping_add((((word >> 10) & 0xfff) as u64) << shift), kind: RefKind::Add, producer: Some(producer), scale: shift });
                shape.push(word & 0xffc0_03ff);
                pages[rd] = None;
                continue;
            }
        }
        if word & 0x3b00_0000 == 0x3900_0000 {
            if let Some((page, producer)) = pages[base] {
                let mut size = word >> 30;
                if word & 0x0480_0000 == 0x0480_0000 {
                    size = 4;
                }
                refs.push(Ref { site: here, target: page.wrapping_add((((word >> 10) & 0xfff) as u64) << size), kind: RefKind::Ldst, producer: Some(producer), scale: size });
                shape.push(word & 0xffc0_03ff);
                // A load can replace the register that held the page; dropping a store's source is conservative.
                pages[rd] = None;
                continue;
            }
        }
        shape.push(word);
        // Only the recognized straight-line operations keep page provenance.
        pages = [None; 32];
    }
    Scan { calls, refs, shape }
}

/// fileset_shape.checked_shape: the shape with the compiler's arm64e authentication checks taken out, and the
/// position of every kept instruction (byte offset -> byte offset in the shortened code; None when removed).
pub fn checked_shape(code: &[u8], scanned: &Scan) -> (Vec<u32>, Vec<Option<u32>>) {
    let words: Vec<u32> = (0..code.len() / 4).map(|i| u32_at(code, i * 4)).collect();
    let n = words.len();
    let mut removed = vec![false; n];
    for i in 0..n {
        if i + 5 <= n {
            let block = &words[i..i + 5];
            if (block == [0xaa10_03f1, 0xdac1_47f1, 0xeb11_021f, 0x5400_0040, 0xd438_8e40] || block == [0xaa10_03f1, 0xdac1_43f1, 0xeb11_021f, 0x5400_0040, 0xd438_8e00])
                && i > 0
                && (words[i - 1] == 0xdac1_1230 || words[i - 1] == 0xdac1_1a30)
            {
                removed[i..i + 5].fill(true);
            }
        }
        if i + 3 < n && words[i..i + 3] == [0xca1e_07d0, 0xb6f0_0050, 0xd438_8e20] && words[i + 3] & 0xfc00_0000 == 0x1400_0000 {
            removed[i..i + 3].fill(true);
        }
    }
    let identity = || (scanned.shape.clone(), (0..n as u32).map(|j| Some(j * 4)).collect::<Vec<_>>());
    let mut positions: Vec<Option<u32>> = vec![None; n];
    let mut kept = 0u32;
    for i in 0..n {
        if !removed[i] {
            positions[i] = Some(kept * 4);
            kept += 1;
        }
    }
    let mut normalized = Vec::with_capacity(kept as usize);
    for (i, &word) in words.iter().enumerate() {
        if removed[i] {
            continue;
        }
        let (bits, shift) = if word & 0x7c00_0000 == 0x1400_0000 {
            (26, 0)
        } else if word & 0xff00_0010 == 0x5400_0000 || word & 0x7e00_0000 == 0x3400_0000 {
            (19, 5)
        } else if word & 0x7e00_0000 == 0x3600_0000 {
            (14, 5)
        } else {
            (0, 0)
        };
        let mut value = scanned.shape[i];
        if bits != 0 {
            let mask = (1u32 << bits) - 1;
            let target = i as i64 * 4 + signed(((word >> shift) & mask) as u64, bits) * 4;
            if 0 <= target && target < code.len() as i64 {
                // Python looks the byte offset up in `positions`: an offset that is not a multiple of 4 cannot
                // occur (targets are instruction offsets); a removed target means another edge enters a check.
                let Some(to) = positions.get(target as usize / 4).copied().flatten() else { return identity() };
                let immediate = (to as i64 - positions[i].unwrap() as i64) / 4;
                value = (word & !(mask << shift)) | (((immediate as u32) & mask) << shift);
            }
        }
        normalized.push(value);
    }
    (normalized, positions)
}

/// Python's `hex(n)` / `f'{n:#x}'`.
pub fn hex(n: u64) -> String {
    format!("{n:#x}")
}

/// Python's `int(text, 16)` for the addresses the JSON files carry ("0x..." or bare hex).
pub fn unhex(text: &str) -> Result<u64> {
    let t = text.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u64::from_str_radix(t, 16).map_err(|_| anyhow!("not a hex address: {text}"))
}
