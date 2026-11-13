//! `oneios-tool prelink stubs-check KERNEL.KC` (was kernel/prelink/stubs-check.py, retired 2026-11-13): every
//! `<name>` stub of the kernel (`bti c; b <name>.original`, the hosted archives' way into an original class) must
//! branch to the value of its N_ABS `<name>.original` symbol, inside the collection.
//! Prints the count of good stubs (`stubs: N good, M bad`, which `oneios-tool cycle` reads) and the first bad
//! ones; exit 1 when one is bad.
//! A collection emitted by XnuPrelink b74ed7c has all 507 bad (kernel/prelink/README.md, "On the phone").
//!
//! Its own byte reads, apart from image.rs: the check is of what the emitter (which uses image.rs) wrote.

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use crate::image::{u32_at, u64_at};

/// The load commands of the Mach-O header at `base`: (cmd, offset, size).
fn commands(d: &[u8], base: usize) -> Vec<(u32, usize, usize)> {
    let mut off = base + 32;
    (0..u32_at(d, base + 16))
        .map(|_| {
            let command = (u32_at(d, off), off, u32_at(d, off + 4) as usize);
            off += command.2;
            command
        })
        .collect()
}

/// The collection's segments (address, size, file offset) and the file offset of its com.apple.kernel member.
fn collection(d: &[u8]) -> (Vec<(u64, u64, u64)>, Option<usize>) {
    let (mut segments, mut kernel) = (Vec::new(), None);
    for (c, off, s) in commands(d, 0) {
        if c == 0x19 {
            segments.push((u64_at(d, off + 24), u64_at(d, off + 32), u64_at(d, off + 40)));
        }
        if c == 0x8000_0035 {
            let (fileoff, name_at) = (u64_at(d, off + 16) as usize, u32_at(d, off + 24) as usize);
            if d[off + name_at..off + s].split(|&b| b == 0).next() == Some(b"com.apple.kernel".as_slice()) {
                kernel = Some(fileoff);
            }
        }
    }
    (segments, kernel)
}

/// Every symbol of the kernel member: name -> value (of two with one name, the later).
fn symbols(d: &[u8], kernel: usize) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for (c, off, _) in commands(d, kernel) {
        if c == 2 {
            let (symoff, nsyms, stroff) = (u32_at(d, off + 8) as usize, u32_at(d, off + 12) as usize, u32_at(d, off + 16) as usize);
            for i in 0..nsyms {
                let at = stroff + u32_at(d, symoff + 16 * i) as usize;
                let end = at + d[at..].iter().position(|&b| b == 0).unwrap_or(d.len() - at);
                out.insert(String::from_utf8_lossy(&d[at..end]).into_owned(), u64_at(d, symoff + 16 * i + 8));
            }
        }
    }
    out
}

/// (the count of good stubs, a line per bad one).
pub fn check(d: &[u8]) -> Result<(usize, Vec<String>)> {
    let (segments, kernel) = collection(d);
    let symbols = symbols(d, kernel.context("stubs-check: the collection has no com.apple.kernel member")?);
    let word = |a: u64| segments.iter().find(|(vm, vs, _)| *vm <= a && a < vm + vs).map(|(vm, _, fo)| u32_at(d, (fo + a - vm) as usize));
    let (mut good, mut bad) = (0, Vec::new());
    for (name, &value) in &symbols {
        let Some(&want) = (if name.ends_with(".original") { None } else { symbols.get(&format!("{name}.original")) }) else { continue };
        // the instruction after `bti c`: b <target>
        let target = word(value.wrapping_add(4)).filter(|w| w & 0xfc00_0000 == 0x1400_0000).map(|w| {
            let imm = (w & 0x3ff_ffff) as i64;
            value.wrapping_add(4).wrapping_add(((if imm & 0x200_0000 != 0 { imm - 0x400_0000 } else { imm }) * 4) as u64)
        });
        match target {
            // the target must be mapped, and hold an instruction
            Some(target) if target == want && word(target).is_some_and(|w| w != 0) => good += 1,
            Some(target) if target != 0 => bad.push(format!("{name} at {value:#x}: branch to {target:#x} , .original {want:#x}")),
            _ => bad.push(format!("{name}: not a branch")),
        }
    }
    Ok((good, bad))
}

pub fn main(path: &str) -> Result<i32> {
    let data = std::fs::read(path).with_context(|| path.to_string())?;
    let (good, bad) = check(&data)?;
    println!("stubs: {good} good, {} bad", bad.len());
    for line in bad.iter().take(10) {
        println!("  {line}");
    }
    Ok(if bad.is_empty() { 0 } else { 1 })
}
