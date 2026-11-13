//! `oneios-tool debug kernel-diff A.macho B.macho` (was tests/kernelgen/kernel-diff.py): where two builds of the
//! hosted kernel differ, byte by byte.
//!
//! Two builds of the same source are not the same file: XNU's version string carries the build's time, the linker
//! gives each link its own LC_UUID, the symbol table's N_OSO entries carry each object file's modification time,
//! and the build seals the kernel with an HMAC. This names the place of every differing byte (the Mach-O header
//! and load commands, a section, a symbol table entry and its type, the string table) and says whether all of
//! them are of those four kinds.
//! Exit status: 0 the two kernels differ only there (or not at all), 1 they differ elsewhere, 2 usage.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

/// A fixed-size name field: the bytes before the trailing NULs.
fn name(raw: &[u8]) -> String {
    let end = raw.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

/// The symbol table's place: (symoff, nsyms, stroff, strsize).
type Symtab = (usize, usize, usize, usize);

/// [(start, end, name)] of the header's parts and of every section, and the symbol table's place.
fn layout(data: &[u8]) -> Result<(Vec<(usize, usize, String)>, Symtab)> {
    if data.len() < 32 || u32_at(data, 0) != 0xfeed_facf {
        bail!("not a 64-bit Mach-O");
    }
    let ncmds = u32_at(data, 16);
    let (mut places, mut offset, mut symtab) = (Vec::new(), 32usize, None);
    for _ in 0..ncmds {
        let (cmd, size) = (u32_at(data, offset), u32_at(data, offset + 4) as usize);
        let kind = match cmd {
            0x1b => "LC_UUID".to_string(),
            0x19 => "LC_SEGMENT_64".to_string(),
            0x2 => "LC_SYMTAB".to_string(),
            other => format!("load command {other:#x}"),
        };
        places.push((offset, offset + size, format!("header: {kind}")));
        if cmd == 0x19 {
            let segment = name(&data[offset + 8..offset + 24]);
            for i in 0..u32_at(data, offset + 64) as usize {
                let at = offset + 72 + 80 * i;
                let (length, fileoff) = (u64_at(data, at + 40) as usize, u32_at(data, at + 48) as usize);
                if data[at + 64] != 1 {
                    // not zero-fill
                    places.push((fileoff, fileoff + length, format!("{segment},{}", name(&data[at..at + 16]))));
                }
            }
        } else if cmd == 0x2 {
            symtab = Some((u32_at(data, offset + 8) as usize, u32_at(data, offset + 12) as usize, u32_at(data, offset + 16) as usize, u32_at(data, offset + 20) as usize));
        }
        offset += size;
    }
    Ok((places, symtab.context("the kernel has no LC_SYMTAB")?))
}

/// Python's `repr()` of a string of printable ASCII.
fn repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for c in text.chars() {
        if c == '\\' || c == quote {
            out.push('\\');
        }
        out.push(c);
    }
    out.push(quote);
    out
}

fn printable(byte: u8) -> bool {
    (32..127).contains(&byte)
}

/// The run of printable bytes around `at`.
fn string_around(data: &[u8], at: usize) -> (usize, usize) {
    let (mut start, mut end) = (at, at);
    while start > 0 && printable(data[start - 1]) {
        start -= 1;
    }
    while end < data.len() && printable(data[end]) {
        end += 1;
    }
    (start, end)
}

/// The first 100 characters of a run (the other file's bytes there need not be ASCII).
fn shown(run: &[u8]) -> String {
    String::from_utf8_lossy(run).chars().take(100).collect()
}

/// What a differing symbol table entry is.
fn symbol_place(a: &[u8], symoff: usize, at: usize) -> String {
    let (index, field) = ((at - symoff) / 16, (at - symoff) % 16);
    let what = match a[symoff + index * 16 + 4] {
        0x66 => "N_OSO".to_string(),
        0x64 => "N_SO".to_string(),
        0x24 => "N_FUN".to_string(),
        0x2e => "N_BNSYM".to_string(),
        0x4e => "N_ENSYM".to_string(),
        kind => format!("type {kind:#x}"),
    };
    format!("symbol table: {what} entries, the {}", if field >= 8 { "value" } else { "name, type or section" })
}

/// The kinds of difference every two builds have: (what it is, is this place one).
const KINDS: [(&str, fn(&str) -> bool); 4] = [
    ("the build's time in the version string", |name| name.ends_with("(in a string)")),
    ("the link's UUID", |name| name == "header: LC_UUID"),
    ("the object files' modification times (N_OSO)", |name| name.starts_with("symbol table: N_OSO entries, the value")),
    ("the seal", |name| name.to_lowercase().contains("hmac")),
];

pub fn run(path_a: &str, path_b: &str) -> Result<i32> {
    let a = std::fs::read(path_a).with_context(|| path_a.to_string())?;
    let b = std::fs::read(path_b).with_context(|| path_b.to_string())?;
    if a.len() != b.len() {
        println!("sizes differ: {} and {} bytes", a.len(), b.len());
        return Ok(1);
    }
    let (places, (symoff, nsyms, stroff, strsize)) = layout(&a)?;
    let mut found: BTreeMap<String, u64> = BTreeMap::new();
    let mut strings: BTreeSet<(String, String, String)> = BTreeSet::new();
    for at in (0..a.len()).filter(|&at| a[at] != b[at]) {
        let place = if symoff <= at && at < symoff + nsyms * 16 {
            symbol_place(&a, symoff, at)
        } else if stroff <= at && at < stroff + strsize {
            "string table".to_string()
        } else {
            let mut place = places.iter().find(|(s, e, _)| *s <= at && at < *e).map_or("outside every section".to_string(), |p| p.2.clone());
            if !place.starts_with("header") {
                let (start, end) = string_around(&a, at);
                if end - start >= 16 {
                    strings.insert((place.clone(), shown(&a[start..end]), shown(&b[start..end])));
                    place += " (in a string)";
                }
            }
            place
        };
        *found.entry(place).or_default() += 1;
    }
    for (place, count) in &found {
        println!("{count:6} bytes  {place}");
    }
    for (place, one, two) in &strings {
        println!("  {place}: {}\n  {}  {}", repr(one), " ".repeat(place.chars().count()), repr(two));
    }
    let total: u64 = found.values().sum();
    // the time in the version string (__TEXT,__const has it twice), the link's UUID, the objects' times, the seal
    let other: Vec<&String> = found.keys().filter(|place| !KINDS.iter().any(|(_, test)| test(place))).collect();
    if !other.is_empty() {
        let list: Vec<String> = other.iter().map(|place| repr(place)).collect();
        println!("{total} bytes differ; NOT only the build's time, the UUID, the objects' times and the seal: [{}]", list.join(", "));
        return Ok(1);
    }
    if total == 0 {
        println!("the same file");
        return Ok(0);
    }
    let kinds: Vec<&str> = KINDS.iter().filter(|(_, test)| found.keys().any(|place| test(place))).map(|(kind, _)| *kind).collect();
    println!("{total} bytes differ: {}; nothing else", kinds.join(", "));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_is_pythons() {
        assert_eq!(repr("__TEXT,__const"), "'__TEXT,__const'");
        assert_eq!(repr("it's"), "\"it's\"");
        assert_eq!(repr("a\\b'\"c"), "'a\\\\b\\'\"c'");
    }
}
