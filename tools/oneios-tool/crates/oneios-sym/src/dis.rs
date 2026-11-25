//! `sym dis ADDR [COUNT]`: the instructions of the cache at ADDR, as llvm-mc reads them, each with its address and
//! word, and with the addresses an instruction reaches worked out: a branch's target, and the address an `adrp`
//! page plus a following `add` or load/store offset make (what blacktop's `ipsw dyld disass` printed for the Metal
//! plug-in's hooks, docs/gpu.md section 7). Names come from the index when it has them (code only: the index keeps
//! the symbols an image's __TEXT can get, so a data address has no name: a data address an adrp + add/load/store
//! makes is named by the C string there, or by the string or symbol a pointer word there points at).

use crate::cache::{branch_target, Cache};
use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;

/// The page an `adrp` gives (`pc`'s 4 KiB page plus its immediate) and its destination register.
pub fn adrp(word: u32, pc: u64) -> Option<(u32, u64)> {
    if word & 0x9f00_0000 != 0x9000_0000 {
        return None;
    }
    let immlo = (word >> 29) & 3;
    let immhi = (word >> 5) & 0x7ffff;
    let imm = (((immhi << 2) | immlo) << 11) as i32 >> 11; // sign-extended imm21, in pages
    Some((word & 31, (pc & !0xfff).wrapping_add_signed(i64::from(imm) << 12)))
}

/// What an instruction that adds an offset to its base register does with the sum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Then {
    /// `add Xd, Xn, #imm`: Xd holds the sum (a page plus an offset is still an address to follow).
    Holds(u32),
    /// A load into a general register: the register no longer holds a page.
    Loads(u32),
    /// A store, or a load into a SIMD register: no general register changes.
    Nothing,
}

/// What an instruction adds to the page in its base register: (base register, offset, what then) for
/// `add Xd, Xn, #imm` (64-bit, no shift or LSL #12) and for a load or store with an unsigned scaled offset.
pub fn page_offset(word: u32) -> Option<(u32, u64, Then)> {
    let (rd, rn) = (word & 31, (word >> 5) & 31);
    if word & 0xff80_0000 == 0x9100_0000 {
        let imm = u64::from((word >> 10) & 0xfff);
        return Some((rn, if word & (1 << 22) != 0 { imm << 12 } else { imm }, Then::Holds(rd)));
    }
    if word & 0x3b00_0000 == 0x3900_0000 {
        let size = word >> 30;
        let simd = word & (1 << 26) != 0;
        let opc = (word >> 22) & 3;
        let scale = if simd && size == 0 && opc & 2 != 0 { 4 } else { size };
        // opc 01, 10, 11 of a general register load into it
        let then = if !simd && opc != 0 { Then::Loads(rd) } else { Then::Nothing };
        return Some((rn, u64::from((word >> 10) & 0xfff) << scale, then));
    }
    None
}

/// The target of a conditional branch (b.cond, cbz/cbnz, tbz/tbnz) at `pc`.
pub fn cond_target(word: u32, pc: u64) -> Option<u64> {
    let imm19 = || (((word >> 5) & 0x7ffff) << 13) as i32 >> 13;
    let off = if word & 0xff00_0010 == 0x5400_0000 || word & 0x7e00_0000 == 0x3400_0000 {
        imm19()
    } else if word & 0x7e00_0000 == 0x3600_0000 {
        (((word >> 5) & 0x3fff) << 18) as i32 >> 18
    } else {
        return None;
    };
    Some(pc.wrapping_add_signed(i64::from(off) * 4))
}

/// llvm-mc's text of each word (None for a word it does not decode).
fn texts(words: &[u32]) -> Result<Vec<Option<String>>> {
    use std::io::Write;
    let input: String = words.iter().map(|w| format!("0x{:02x} 0x{:02x} 0x{:02x} 0x{:02x}\n", w & 0xff, (w >> 8) & 0xff, (w >> 16) & 0xff, w >> 24)).collect();
    let mut child = std::process::Command::new(std::env::var("LLVM_MC").unwrap_or_else(|_| "llvm-mc".into()))
        .args(["--disassemble", "-triple=arm64e", "-mattr=+v8.5a,+pauth,+bti"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("llvm-mc (LLVM_MC=PATH names another)")?;
    child.stdin.take().context("llvm-mc's input")?.write_all(input.as_bytes())?;
    let o = child.wait_with_output()?;
    // A word llvm-mc cannot decode is said on stderr as "<stdin>:LINE:..." and has no line on stdout.
    let bad: BTreeSet<usize> = String::from_utf8_lossy(&o.stderr)
        .lines()
        .filter_map(|l| l.strip_prefix("<stdin>:")?.split(':').next()?.parse::<usize>().ok())
        .collect();
    let text = String::from_utf8_lossy(&o.stdout);
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('.'));
    let mut out = Vec::with_capacity(words.len());
    for i in 0..words.len() {
        out.push(if bad.contains(&(i + 1)) { None } else { lines.next().map(|l| l.replace('\t', " ")) });
    }
    Ok(out)
}

/// The lines of `sym dis`: `count` instructions from `start` (unslid), each address named by `name` where it can be;
/// an address an adrp + add/load/store makes that `name` does not know is named by `data` (a C string there, the
/// string or symbol a pointer there points at: fix/s24-fps-heat-camera's reading of selector and string uses).
pub fn lines(
    cache: &Cache,
    start: u64,
    count: u64,
    name: &dyn Fn(u64) -> Option<String>,
    data: &dyn Fn(u64) -> Option<String>,
) -> Result<Vec<String>> {
    if start & 3 != 0 {
        bail!("dis: {start:#x} is not a multiple of 4");
    }
    let mut words = Vec::new();
    for i in 0..count {
        match cache.word(start + 4 * i) {
            Some(w) => words.push(w),
            None if i == 0 => bail!("dis: {start:#x} is not in this copy of the cache"),
            None => break,
        }
    }
    let texts = texts(&words)?;
    let mut pages: [Option<u64>; 32] = [None; 32];
    let mut out = Vec::with_capacity(words.len());
    for (i, (&w, text)) in words.iter().zip(texts).enumerate() {
        let pc = start + 4 * i as u64;
        let mut note = None;
        let mut is_data = false;
        if let Some((rd, page)) = adrp(w, pc) {
            pages[rd as usize] = Some(page);
            note = Some(page);
        } else if let Some((rn, off, then)) = page_offset(w) {
            let sum = pages[rn as usize].map(|page| page + off);
            note = sum;
            is_data = true;
            match then {
                Then::Holds(rd) => pages[rd as usize] = sum,
                Then::Loads(rt) => pages[rt as usize] = None,
                Then::Nothing => {}
            }
        } else if let Some((t, _)) = branch_target(w, pc) {
            note = Some(t);
        } else if let Some(t) = cond_target(w, pc) {
            note = Some(t);
        }
        let mut line = format!("{pc:#x}  {w:08x}  {}", text.unwrap_or_else(|| "(not an instruction llvm-mc knows)".into()));
        if let Some(t) = note {
            line.push_str(&format!("    ; {t:#x}"));
            if let Some(n) = name(t).or_else(|| if is_data { data(t) } else { None }) {
                line.push_str(&format!(" {n}"));
            }
        }
        out.push(line);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_hooks_instructions() {
        // 24A446 QuartzCore -[CADisplayMode preferredScale] (0x1844b1758): adrp x8 at 0x1844b1774, ldr x8, [x8, #1920]
        assert_eq!(adrp(0xf0315b88, 0x1844b1778), Some((8, 0x1844b1000 + 1656172544)));
        assert_eq!(page_offset(0xf943c108), Some((8, 1920, Then::Loads(8))));
        // ldrb w8, [x8, #3545]; ldr s8, [x8, #0x104]
        assert_eq!(page_offset(0x39776508), Some((8, 3545, Then::Loads(8))));
        assert_eq!(page_offset(0xbd410508), Some((8, 0x104, Then::Nothing)));
        // add x0, x8, #0x680
        assert_eq!(page_offset(0x911a0100), Some((8, 0x680, Then::Holds(0))));
        // tbnz w8, #0, +0xc0 (the plug-in's branch word 0x37000608 at 0x184461928 -> 0x1844619e8)
        assert_eq!(cond_target(0x37000608, 0x184461928), Some(0x1844619e8));
        // cbnz w8, #84; b.ne #76
        assert_eq!(cond_target(0x350002a8, 0x1000), Some(0x1054));
        assert_eq!(cond_target(0x54000261, 0x1000), Some(0x104c));
    }
}
