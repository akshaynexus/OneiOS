//! `prelink hosted KERNELCACHE OUT REPORT.json [--closure select.json] [--x18]`: Apple's kexts under the hosted
//! kernel: the thread register rewrite and the list of privileged instructions. A port of kernel/prelink/hosted.py
//! (its header has the why of each rewrite): the same OUT and REPORT.json, byte for byte, and the same line on
//! stdout.

use anyhow::{anyhow, Context, Result};
use oneios_common::pyjson::{self, Value};
use std::collections::{HashMap, HashSet};

const LC_SEGMENT_64: u32 = 0x19;
const LC_FILESET_ENTRY: u32 = 0x8000_0035;
type Register = (u32, u32, u32, u32, u32);
const TPIDR_EL1: Register = (3, 0, 13, 0, 4);
const MPIDR_EL1: Register = (3, 0, 0, 0, 5);
/// op0=3 op1=3 CRn=13 CRm=0 op2=2, bits 20..5.
const TPIDR_EL0_FIELD: u32 = 0x1bd040;
/// Readable or writable at EL0 under Linux: not counted.
const EL0: [Register; 16] = [
    (3, 3, 13, 0, 2), (3, 3, 13, 0, 3), (3, 3, 4, 2, 0), (3, 3, 4, 4, 0), (3, 3, 4, 4, 1),
    (3, 3, 0, 0, 1), (3, 3, 0, 0, 7), (3, 3, 14, 0, 0), (3, 3, 14, 0, 1), (3, 3, 14, 0, 2), (3, 3, 2, 4, 0),
    (3, 3, 2, 4, 1), (3, 3, 4, 2, 5), (3, 3, 4, 2, 6), (3, 3, 13, 0, 5), (3, 3, 13, 0, 7),
];

#[derive(clap::Args, Debug)]
pub struct Args {
    kernelcache: String,
    out: String,
    report: String,
    /// select.py record: report only the kexts of its closure
    #[arg(long)]
    closure: Option<String>,
    /// also list instructions that name x18 (slow)
    #[arg(long)]
    x18: bool,
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

/// A Python Counter: counts in the order the keys were first seen.
#[derive(Default)]
struct Counter {
    keys: Vec<String>,
    index: HashMap<String, usize>,
    counts: Vec<u64>,
}

impl Counter {
    fn add(&mut self, key: &str, n: u64) {
        match self.index.get(key) {
            Some(&i) => self.counts[i] += n,
            None => {
                self.index.insert(key.to_string(), self.keys.len());
                self.keys.push(key.to_string());
                self.counts.push(n);
            }
        }
    }
    fn items(&self) -> impl Iterator<Item = (&str, u64)> {
        self.keys.iter().map(String::as_str).zip(self.counts.iter().copied())
    }
}

/// The fileset's entries: (name, file offset of its header), in the order of the load commands.
fn entries(data: &[u8]) -> Vec<(String, usize)> {
    let count = u32_at(data, 16);
    let mut off = 32;
    let mut out = Vec::new();
    for _ in 0..count {
        let (cmd, length) = (u32_at(data, off), u32_at(data, off + 4) as usize);
        if cmd == LC_FILESET_ENTRY {
            let fileoff = u64_at(data, off + 16) as usize;
            let name_at = off + u32_at(data, off + 24) as usize;
            let end = name_at + data[name_at..].iter().position(|b| *b == 0).unwrap_or(0);
            out.push((String::from_utf8_lossy(&data[name_at..end]).into_owned(), fileoff));
        }
        off += length;
    }
    out
}

/// The executable segments with file content of the image at `at`: (name, va, file offset, file size).
fn exec_segments(data: &[u8], at: usize) -> Vec<(String, u64, usize, usize)> {
    let count = u32_at(data, at + 16);
    let mut off = at + 32;
    let mut out = Vec::new();
    for _ in 0..count {
        let (cmd, length) = (u32_at(data, off), u32_at(data, off + 4) as usize);
        if cmd == LC_SEGMENT_64 {
            let name = &data[off + 8..off + 24];
            let name = &name[..name.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1)];
            let va = u64_at(data, off + 24);
            let (fileoff, filesize) = (u64_at(data, off + 40) as usize, u64_at(data, off + 48) as usize);
            let initprot = u32_at(data, off + 60);
            if initprot & 4 != 0 && filesize != 0 {
                out.push((String::from_utf8_lossy(name).into_owned(), va, fileoff, filesize));
            }
        }
        off += length;
    }
    out
}

fn register(word: u32) -> Register {
    (2 + (word >> 19 & 1), word >> 16 & 7, word >> 12 & 15, word >> 8 & 15, word >> 5 & 7)
}

fn name_of(key: Register) -> String {
    if key == TPIDR_EL1 {
        "TPIDR_EL1".into()
    } else if key == (3, 3, 4, 2, 1) {
        "DAIF".into()
    } else {
        format!("S{}_{}_C{}_C{}_{}", key.0, key.1, key.2, key.3, key.4)
    }
}

/// Instructions that name x18 or w18, by disassembly (llvm-mc), per segment.
fn x18_users(data: &[u8], segments: &[(String, u64, usize, usize)]) -> Result<Vec<Value>> {
    use std::io::Write;
    let mut found = Vec::new();
    for (_, va, fileoff, filesize) in segments {
        let step = 1usize << 20;
        for start in (0..*filesize).step_by(step) {
            let blob = &data[fileoff + start..fileoff + (*filesize).min(start + step)];
            let text: Vec<String> = blob.iter().map(|b| format!("0x{b:02x}")).collect();
            let mut child = std::process::Command::new("llvm-mc")
                .args(["--disassemble", "-triple=arm64e", "-mattr=+v8.5a,+pauth,+bti"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .context("llvm-mc")?;
            let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("llvm-mc: no stdin"))?;
            let input = text.join(" ");
            let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
            let out = child.wait_with_output().context("llvm-mc")?;
            let _ = writer.join();
            let out = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = out.lines().filter(|l| l.starts_with('\t') && !l.starts_with("\t.")).collect();
            let mut record = |address: u64, instruction: String| {
                let mut v = Value::dict();
                v.set("address", format!("{address:#x}"));
                v.set("instruction", instruction);
                found.push(v);
            };
            if lines.len() != blob.len() / 4 {
                record(va + start as u64, "undecoded range (data in code)".into());
                continue;
            }
            for (index, line) in lines.iter().enumerate() {
                let spaced = line.replace([',', '[', ']', '!'], " ");
                let mut tokens = spaced.split_whitespace();
                tokens.next();
                if tokens.any(|t| t == "x18" || t == "w18") {
                    record(va + (start + 4 * index) as u64, line.split_whitespace().collect::<Vec<_>>().join(" "));
                }
            }
        }
    }
    Ok(found)
}

fn rewrite(kext: &str, segment: &str, address: u64, old: u32, new: u32, instruction: String) -> Value {
    let mut v = Value::dict();
    v.set("kext", kext);
    v.set("segment", segment);
    v.set("address", format!("{address:#x}"));
    v.set("old", format!("{old:#010x}"));
    v.set("new", format!("{new:#010x}"));
    v.set("instruction", instruction);
    v
}

pub fn run(args: Args) -> Result<()> {
    let source = std::fs::read(&args.kernelcache).with_context(|| args.kernelcache.clone())?;
    let mut data = source.clone();
    let closure: Option<HashSet<String>> = match &args.closure {
        None => None,
        Some(path) => {
            let record = pyjson::load(path)?;
            let list = record.get("closure").and_then(Value::as_list).ok_or_else(|| anyhow!("{path}: no closure"))?;
            Some(list.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        }
    };
    let (mut rewrites, mut mpidr, mut pointers) = (Vec::new(), Vec::new(), Vec::new());
    // privileged[kext] in the order the kexts were first seen, each a Counter
    let mut privileged: Vec<(String, Counter)> = Vec::new();
    let mut x18: Vec<(String, Value)> = Vec::new();
    let mut scanned = 0usize;
    for (name, at) in entries(&source) {
        if name == "com.apple.kernel" {
            continue;
        }
        let in_scope = closure.as_ref().is_none_or(|c| c.contains(&name));
        let segments = exec_segments(&data, at);
        // where this kext's Counter goes: Python creates it at the first count
        let mut slot: Option<usize> = privileged.iter().position(|(n, _)| *n == name);
        let count = |privileged: &mut Vec<(String, Counter)>, slot: &mut Option<usize>, key: &str| {
            let i = *slot.get_or_insert_with(|| {
                privileged.push((name.clone(), Counter::default()));
                privileged.len() - 1
            });
            privileged[i].1.add(key, 1);
        };
        for (segment, va, fileoff, filesize) in &segments {
            scanned += filesize;
            let mut offset = *fileoff;
            // range(fileoff, fileoff + filesize - 3, 4)
            while offset + 3 < fileoff + filesize {
                let word = u32_at(&data, offset);
                let address = va + (offset - fileoff) as u64;
                if in_scope {
                    if word & 0xfffff0df == 0xd50340df {
                        // msr DAIFSet / DAIFClr, #imm
                        count(&mut privileged, &mut slot, if word & 0x20 != 0 { "msr DAIFClr" } else { "msr DAIFSet" });
                    } else if word & 0xfff80000 == 0xd5080000 && (word >> 16 & 7) != 3 {
                        // sys (tlbi, at, dc at EL1)
                        count(&mut privileged, &mut slot, &format!("sys #{}, C{}, C{}, #{}", word >> 16 & 7, word >> 12 & 15, word >> 8 & 15, word >> 5 & 7));
                    }
                }
                if word & 0xfffffc00 == 0xb2501c00 {
                    // orr xD, xN, #0x00ff000000000000 -> and xD, xN, #0xff00ffffffffffff
                    let new = 0x9248dc00 | (word & 0x3ff);
                    data[offset..offset + 4].copy_from_slice(&new.to_le_bytes());
                    let (d, n) = (word & 31, word >> 5 & 31);
                    pointers.push(rewrite(&name, segment, address, word, new, format!("orr x{d}, x{n}, #0xff000000000000 -> and x{d}, x{n}, #0xff00ffffffffffff")));
                    offset += 4;
                    continue;
                }
                if word & 0xffd00000 != 0xd5100000 {
                    offset += 4;
                    continue;
                }
                let key = register(word);
                let read = word >> 21 & 1 != 0;
                if key == TPIDR_EL1 {
                    let new = (word & 0xffe0001f) | TPIDR_EL0_FIELD;
                    data[offset..offset + 4].copy_from_slice(&new.to_le_bytes());
                    rewrites.push(rewrite(&name, segment, address, word, new, format!("{} x{}, TPIDR_EL1 -> TPIDR_EL0", if read { "mrs" } else { "msr" }, word & 31)));
                } else if key == MPIDR_EL1 && read {
                    // ldr wN, [x18]: the vCPU index (kernel/hosted-fixup.py)
                    let new = 0xb9400240 | (word & 31);
                    data[offset..offset + 4].copy_from_slice(&new.to_le_bytes());
                    mpidr.push(rewrite(&name, segment, address, word, new, format!("mrs x{}, MPIDR_EL1 -> ldr w{}, [x18]", word & 31, word & 31)));
                } else if !EL0.contains(&key) && in_scope {
                    count(&mut privileged, &mut slot, &format!("{}{}", if read { "mrs " } else { "msr " }, name_of(key)));
                }
                offset += 4;
            }
        }
        if args.x18 && in_scope {
            let users = x18_users(&data, &segments)?;
            if !users.is_empty() {
                x18.push((name.clone(), Value::List(users)));
            }
        }
    }
    let changed = data != source;
    if changed {
        std::fs::write(&args.out, &data).with_context(|| args.out.clone())?;
    }
    let mut totals = Counter::default();
    for (_, counts) in &privileged {
        for (key, n) in counts.items() {
            totals.add(key, n);
        }
    }
    let mut total_items: Vec<(&str, u64)> = totals.items().collect();
    total_items.sort_by_key(|(_, n)| std::cmp::Reverse(*n)); // stable, as Python's sorted
    let mut by_kext: Vec<&(String, Counter)> = privileged.iter().collect();
    by_kext.sort_by(|a, b| a.0.cmp(&b.0));
    let counts_value = |items: Vec<(&str, u64)>| Value::Dict(items.into_iter().map(|(k, n)| (k.to_string(), Value::from(n))).collect());

    let mut report = Value::dict();
    report.set("kernelcache_sha256", oneios_common::sha256_hex(&source));
    report.set("output_sha256", oneios_common::sha256_hex(&data));
    report.set("output_written", changed);
    report.set("kext_code_bytes_scanned", scanned);
    report.set("convention", "TPIDR_EL0 = current thread (TPIDR_EL1 on hardware), x18 = struct xh_cpu * of the vCPU");
    let (n_rewrites, n_mpidr, n_pointers) = (rewrites.len(), mpidr.len(), pointers.len());
    report.set("tpidr_el1_rewrites", Value::List(rewrites));
    report.set("mpidr_el1_rewrites", Value::List(mpidr));
    report.set("kernel_pointer_rewrites", Value::List(pointers));
    report.set("privileged_instructions_scope", if closure.is_some() { "closure" } else { "every kext entry" });
    let total: u64 = total_items.iter().map(|(_, n)| n).sum();
    report.set("privileged_instructions_total", counts_value(total_items));
    report.set(
        "privileged_instructions_by_kext",
        Value::Dict(
            by_kext
                .iter()
                .map(|(name, counts)| {
                    let mut items: Vec<(&str, u64)> = counts.items().collect();
                    items.sort_by(|a, b| a.0.cmp(b.0));
                    (name.clone(), counts_value(items))
                })
                .collect(),
        ),
    );
    report.set("x18_checked", args.x18);
    let x18_kexts = x18.len();
    report.set("x18_instructions_by_kext", Value::Dict(x18));
    std::fs::write(&args.report, pyjson::dumps(&report, Some(1), false)).with_context(|| args.report.clone())?;
    println!(
        "hosted: {n_rewrites} TPIDR_EL1, {n_mpidr} MPIDR_EL1 and {n_pointers} kernel pointer rewrite(s) in {scanned} bytes of kext code; \
         {total} other privileged system-register instruction(s) in {} kext(s){}",
        privileged.len(),
        if args.x18 { format!("; x18 named in {x18_kexts} kext(s)") } else { String::new() }
    );
    Ok(())
}
