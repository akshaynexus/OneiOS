//! `oneios-tool debug stub-kc OUT GOOD BAD [NOTE]`: a made-up kernel collection for the tests of
//! `prelink stubs-check` (tests/prelinkpy, tests/cycle): a fileset with one segment and a com.apple.kernel member
//! whose symbol table has GOOD + BAD stubs, each `bti c; b <target>` with an N_ABS `<name>.original` symbol. A
//! good stub's `.original` is its branch target (a `ret` in the segment); a bad one's is 0x1000 past it.
//! NOTE is appended after the file's last byte, so that two collections of one shape differ.
//! Nothing of Apple's: the names are `_stubNNNN`, the code is these three instructions.

use anyhow::{Context, Result};

const BASE: u64 = 0xffff_fff0_0700_4000;
const BTI_C: u32 = 0xd503_245f;
const RET: u32 = 0xd65f_03c0;

fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// A 64-bit Mach-O header for arm64e with `ncmds` commands of `size` bytes.
fn header(out: &mut Vec<u8>, filetype: u32, ncmds: u32, size: u32) {
    for value in [0xfeed_facf, 0x100_000c, 2, filetype, ncmds, size, 0, 0] {
        put32(out, value);
    }
}

pub fn build(good: u64, bad: u64, note: &str) -> Vec<u8> {
    let count = good + bad;
    // the layout: the fileset's header and two commands, the member's header and LC_SYMTAB, the code, the symbols
    let (top, member, code_off) = (32 + 72 + 56, 32 + 24, 4096usize);
    let code_size = (12 * count) as usize;
    let (symoff, nsyms) = (code_off + code_size, 2 * count);
    let stroff = symoff + 16 * nsyms as usize;
    let mut out = Vec::new();
    header(&mut out, 0xc, 2, 72 + 56);
    // LC_SEGMENT_64 __TEXT_EXEC: the code
    put32(&mut out, 0x19);
    put32(&mut out, 72);
    out.extend_from_slice(b"__TEXT_EXEC\0\0\0\0\0");
    for value in [BASE, code_size as u64, code_off as u64, code_size as u64] {
        put64(&mut out, value);
    }
    for value in [5, 5, 0, 0] {
        put32(&mut out, value);
    }
    // LC_FILESET_ENTRY com.apple.kernel
    put32(&mut out, 0x8000_0035);
    put32(&mut out, 56);
    put64(&mut out, BASE);
    put64(&mut out, top as u64);
    put32(&mut out, 32);
    put32(&mut out, 0);
    out.extend_from_slice(b"com.apple.kernel\0\0\0\0\0\0\0\0");
    assert_eq!(out.len(), top);
    header(&mut out, 2, 1, 24);
    for value in [2, 24, symoff as u32, nsyms as u32, stroff as u32, 0] {
        put32(&mut out, value);
    }
    assert_eq!(out.len(), top + member);
    out.resize(code_off, 0);
    // the stubs, then one `ret` per stub
    let target = |i: u64| BASE + 8 * count + 4 * i;
    for i in 0..count {
        put32(&mut out, BTI_C);
        let words = (target(i) - (BASE + 8 * i + 4)) / 4;
        put32(&mut out, 0x1400_0000 | words as u32);
    }
    (0..count).for_each(|_| put32(&mut out, RET));
    let mut strings = vec![0u8];
    for i in 0..count {
        for (suffix, kind, section, value) in [("", 0x0fu8, 1u8, BASE + 8 * i), (".original", 0x02, 0, target(i) + if i < good { 0 } else { 0x1000 })] {
            put32(&mut out, strings.len() as u32);
            out.extend_from_slice(&[kind, section, 0, 0]);
            put64(&mut out, value);
            strings.extend_from_slice(format!("_stub{i:04}{suffix}\0").as_bytes());
        }
    }
    out.extend_from_slice(&strings);
    out.extend_from_slice(note.as_bytes());
    out
}

pub fn run(out: &str, good: u64, bad: u64, note: &str) -> Result<i32> {
    std::fs::write(out, build(good, bad, note)).with_context(|| out.to_string())?;
    Ok(0)
}
