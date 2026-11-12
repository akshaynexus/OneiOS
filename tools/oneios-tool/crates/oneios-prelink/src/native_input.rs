//! `prelink native-input IN OUT REPORT.json`: the hosted kernel as the emitter's native input, its weak-lookup
//! binds resolved to its own definitions. A port of kernel/prelink/native-input.py (its header has the why):
//! the same OUT and REPORT.json byte for byte, and the same line on stdout.

use crate::image::{cstr_at, hex, put32, put64, u16_at, u32_at, u64_at, LC_DYLD_CHAINED_FIXUPS, LC_SEGMENT_64, LC_SYMTAB};
use anyhow::{anyhow, bail, Context, Result};
use oneios_common::pyjson::{self, Value};
use std::collections::{HashMap, HashSet};

const PTR_ARM64E_KERNEL: u16 = 7;
/// -3 as the 8-bit ordinal of DYLD_CHAINED_IMPORT
const WEAK_LOOKUP: u32 = 0xfd;

pub fn run(src: &str, dst: &str, record: &str) -> Result<()> {
    let mut data = std::fs::read(src).with_context(|| src.to_string())?;
    let ncmds = u32_at(&data, 16);
    let (mut off, mut segs, mut fixups, mut symtab) = (32usize, Vec::new(), None, None);
    for _ in 0..ncmds {
        let (cmd, length) = (u32_at(&data, off), u32_at(&data, off + 4) as usize);
        if cmd == LC_SEGMENT_64 {
            // (va, vmsize, fileoff, filesize)
            segs.push((u64_at(&data, off + 24), u64_at(&data, off + 32), u64_at(&data, off + 40), u64_at(&data, off + 48)));
        } else if cmd == LC_DYLD_CHAINED_FIXUPS {
            fixups = Some(u32_at(&data, off + 8) as usize);
        } else if cmd == LC_SYMTAB {
            symtab = Some((u32_at(&data, off + 8) as usize, u32_at(&data, off + 12) as usize, u32_at(&data, off + 16) as usize));
        }
        off += length;
    }
    let base = segs.iter().find(|s| s.2 == 0 && s.3 != 0).ok_or_else(|| anyhow!("no segment maps the header"))?.0;
    let (symoff, nsyms, stroff) = symtab.ok_or_else(|| anyhow!("the kernel has no symbol table"))?;
    let mut defined: HashMap<String, u64> = HashMap::new();
    for i in 0..nsyms {
        let row = symoff + 16 * i;
        let kind = data[row + 4];
        if kind & 0xe0 == 0 && kind & 0x0e == 0x0e {
            defined.entry(cstr_at(&data, stroff + u32_at(&data, row) as usize)?).or_insert(u64_at(&data, row + 8));
        }
    }
    let dataoff = fixups.ok_or_else(|| anyhow!("the kernel has no chained fixups"))?;
    let (starts_offset, imports_offset, symbols_offset, count, imports_format) =
        (u32_at(&data, dataoff + 4) as usize, u32_at(&data, dataoff + 8) as usize, u32_at(&data, dataoff + 12) as usize, u32_at(&data, dataoff + 16) as usize, u32_at(&data, dataoff + 20));
    if imports_format != 1 {
        bail!("imports format {imports_format}, expected DYLD_CHAINED_IMPORT (1)");
    }
    let mut imports = Vec::new();
    for i in 0..count {
        let raw = u32_at(&data, dataoff + imports_offset + 4 * i);
        let (ordinal, name_at) = (raw & 0xff, (raw >> 9) as usize);
        let name = cstr_at(&data, dataoff + symbols_offset + name_at)?;
        if ordinal != WEAK_LOOKUP {
            bail!("import {name} has ordinal {ordinal}, not a weak lookup");
        }
        let Some(target) = defined.get(&name) else { bail!("weak import {name} has no definition in the kernel") };
        imports.push((name, *target));
    }
    let starts = dataoff + starts_offset;
    let seg_count = u32_at(&data, starts) as usize;
    let mut rewritten = Vec::new();
    let mut symbols = HashSet::new();
    for s in 0..seg_count {
        let info = u32_at(&data, starts + 4 + 4 * s) as usize;
        if info == 0 {
            continue;
        }
        let at = starts + info;
        let (page_size, pointer_format, segment_offset, page_count) = (u16_at(&data, at + 4) as u64, u16_at(&data, at + 6), u64_at(&data, at + 8), u16_at(&data, at + 20) as usize);
        if pointer_format != PTR_ARM64E_KERNEL {
            bail!("pointer format {pointer_format} in segment {s}");
        }
        for page in 0..page_count {
            let start = u16_at(&data, at + 22 + 2 * page);
            if start == 0xffff {
                continue;
            }
            let seg_va = base + segment_offset;
            // not an empty segment at the same address (__LAST)
            let seg = *segs.iter().find(|x| x.1 != 0 && x.0 <= seg_va && seg_va < x.0 + x.1).ok_or_else(|| anyhow!("a chain starts outside every segment"))?;
            let mut loc = (seg.2 + (seg_va - seg.0) + page as u64 * page_size + start as u64) as usize;
            loop {
                let mut raw = u64_at(&data, loc);
                if raw >> 62 & 1 != 0 {
                    let ordinal = (raw & 0xffff) as usize;
                    let (name, target) = imports.get(ordinal).ok_or_else(|| anyhow!("a bind names import {ordinal}, past the table"))?;
                    let site = seg.0 + loc as u64 - seg.2;
                    let mut row = Value::dict();
                    row.set("site", hex(site));
                    row.set("symbol", name);
                    if raw >> 63 != 0 {
                        let keep = raw & (0xffff << 32 | 1 << 48 | 3 << 49 | 0x7ff << 51 | 1 << 63);
                        raw = keep | (target - base);
                        row.set("target", hex(*target));
                        row.set("key", Value::Int((raw >> 49 & 3) as i128));
                        row.set("diversity", hex(raw >> 32 & 0xffff));
                        row.set("address_diversity", raw >> 48 & 1 != 0);
                    } else {
                        // a plain pointer: ordinal, 19-bit addend at bit 32 -> a 43-bit target offset
                        let mut addend = (raw >> 32 & 0x7ffff) as i64;
                        if addend >> 18 != 0 {
                            addend -= 1 << 19;
                        }
                        let to = target.wrapping_add(addend as u64);
                        raw = (raw & 0x7ff << 51) | (to.wrapping_sub(base) & ((1 << 43) - 1));
                        row.set("target", hex(to));
                        row.set("key", Value::Null);
                    }
                    symbols.insert(name.clone());
                    rewritten.push(row);
                    put64(&mut data, loc, raw);
                }
                let step = ((raw >> 51) & 0x7ff) as usize;
                if step == 0 {
                    break;
                }
                loc += 4 * step;
            }
        }
    }
    if symbols.len() != imports.len() {
        bail!("an import has no bind site");
    }
    put32(&mut data, dataoff + 16, 0); // imports_count
    std::fs::write(dst, &data).with_context(|| dst.to_string())?;
    let count = rewritten.len();
    let mut report = Value::dict();
    report.set("input", src);
    report.set("output", dst);
    report.set("weak_binds_resolved", Value::List(rewritten));
    std::fs::write(record, pyjson::dumps(&report, Some(1), false)).with_context(|| record.to_string())?;
    println!("native-input: {count} weak bind(s) resolved to the kernel's own definitions");
    Ok(())
}
