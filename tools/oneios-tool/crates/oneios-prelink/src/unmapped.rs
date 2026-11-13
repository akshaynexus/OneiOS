//! `oneios-tool prelink unmapped` (was kernel/prelink/unmapped.py, retired 2026-11-13): name the original kernel
//! function behind a "fileset: original component called an unmapped kernel function" panic.
//!
//! usage: prelink unmapped --xnuprelink DIR --kernelcache KC --report kernel.kc.prelink.json [--map MAP] [--lines N] ADDRESS...
//!
//! With --target ADDRESS is the original function itself. Otherwise ADDRESS is the panic's "from" address (a
//! return address: the call is the instruction before it) as xnuhost prints it (0x20...), as linked in kernel.kc
//! (0xfffffff0...) or as in the original kernelcache (0xfffffe00...). Prints the kext that made the call, the
//! original kernel function it called (through a stub or directly), the strings that function references and its
//! first instructions (llvm-mc), and the pairing the matcher had for it, if any. The answer goes into
//! reviewed-*.json as a "functions" record with its evidence.

use std::io::Write as _;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use oneios_common::pyjson::{self, Value};
use oneios_common::pyrepr::{repr_strs, repr_value, truthy};

use crate::image::{hex, scan, signed, u32_at, unhex, Fixups, Image};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Not read: the kernelcache reader is this crate's (image.rs, the port of XnuPrelink's kc_image.py)
    #[arg(long)]
    xnuprelink: String,
    #[arg(long)]
    kernelcache: String,
    #[arg(long)]
    report: String,
    #[arg(long)]
    map: Option<String>,
    #[arg(long, default_value_t = 24)]
    lines: u64,
    /// Where xnuhost placed the collection
    #[arg(long, default_value = "0x2000004000")]
    window: String,
    /// ADDRESS is an original kernel function, not a caller
    #[arg(long)]
    target: bool,
    #[arg(required = true)]
    address: Vec<String>,
}

/// The kernelcache and what the questions need of it.
struct Cache<'a> {
    raw: &'a [u8],
    cache: Image<'a>,
    kernel: Image<'a>,
    kernel_ranges: Vec<(u64, u64)>,
    fixups: Fixups,
    /// (begin, end, kext, segment) of every mapped segment of every entry.
    owners: Vec<(u64, u64, String, String)>,
    starts: Vec<u64>,
    /// match.map.json's "functions" (none without --map).
    functions: Value,
}

impl Cache<'_> {
    fn owner(&self, address: u64) -> String {
        self.owners.iter().find(|(b, e, ..)| *b <= address && address < *e).map_or("?".to_string(), |(_, _, n, seg)| format!("{n} {seg}"))
    }

    fn word(&self, address: u64) -> Result<u32> {
        Ok(u32_at(self.raw, self.cache.offset_of(address, 4)?))
    }

    /// `count` instructions at `address` as llvm-mc prints them, each with its address.
    fn disassemble(&self, address: u64, count: u64) -> Result<Vec<String>> {
        let at = self.cache.offset_of(address, 4 * count)?;
        let bytes: Vec<String> = self.raw[at..at + 4 * count as usize].iter().map(|b| format!("0x{b:02x}")).collect();
        let mut child = Command::new("llvm-mc")
            .args(["--disassemble", "-triple=arm64e", "-mattr=+v8.5a,+pauth,+bti"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("llvm-mc")?;
        child.stdin.take().unwrap().write_all(bytes.join(" ").as_bytes())?;
        let out = String::from_utf8_lossy(&child.wait_with_output()?.stdout).into_owned();
        // the instructions are the lines that start with a tab; the directives (.text) start with a tab and a dot
        let lines = out.lines().filter(|l| l.starts_with('\t') && !l.starts_with("\t."));
        Ok(lines.enumerate().map(|(i, l)| format!("  {:#x}:{}", address + 4 * i as u64, l.split_whitespace().collect::<Vec<_>>().join(" "))).collect())
    }

    /// adrp x17; add x17; ldr x16, [x17]; braa x16, x17 -> the slot's fixup target. (the target, the slot)
    fn through_stub(&self, target: u64) -> Result<(Option<u64>, Option<u64>)> {
        if self.kernel_ranges.iter().any(|(b, e)| *b <= target && target < *e) {
            return Ok((Some(target), None));
        }
        let at = self.cache.offset_of(target, 16)?;
        for r in scan(&self.raw[at..at + 16], target).refs {
            if let Some(fix) = self.fixups.get(&r.target) {
                return Ok((Some(fix.target), Some(r.target)));
            }
        }
        Ok((None, None))
    }

    /// The names of up to three paired functions next to `starts[index]`, going `step` (the original and the
    /// reference kernel are linked in the same order).
    fn near(&self, index: usize, target: u64, step: i64) -> Vec<String> {
        let (mut found, mut at) = (Vec::new(), index as i64 + step);
        while 0 <= at && (at as usize) < self.starts.len() && found.len() < 3 && (at - index as i64).abs() < 40 {
            let start = self.starts[at as usize];
            let name = self.functions.get(&hex(start)).and_then(|pair| pair.get("name"));
            if truthy(name) {
                let distance = (start as i64 - target as i64).div_euclid(4);
                found.push(format!("{} ({distance:+})", oneios_common::pyrepr::str_value(name.unwrap())));
            }
            at += step;
        }
        found
    }

    /// One ADDRESS of the command line.
    fn answer(&self, args: &Args, text: &str, slide: u64, link: u64) -> Result<()> {
        let mut address = unhex(text)?;
        if address < 1 << 40 {
            address = address.wrapping_sub(unhex(&args.window)?).wrapping_add(link);
        }
        if address >= link {
            address = address.wrapping_sub(slide);
        }
        let (target, slot) = if args.target {
            (address, None)
        } else {
            let call = address - 4;
            let instruction = self.word(call)?;
            println!("{text}: call at {call:#x} (original), in {}", self.owner(call));
            if instruction & 0x7c00_0000 != 0x1400_0000 {
                println!("  not a direct branch:");
                println!("{}", self.disassemble(call - 12, 5)?.join("\n"));
                return Ok(());
            }
            let branch = call.wrapping_add((signed((instruction & 0x3ff_ffff) as u64, 26) * 4) as u64);
            let (Some(target), slot) = self.through_stub(branch)? else {
                println!("  the branch target is not a stub with a bound slot");
                return Ok(());
            };
            (target, slot)
        };
        let end = self.starts.iter().copied().find(|s| *s > target).unwrap_or(target + 4 * args.lines);
        let through = slot.filter(|s| *s != 0).map_or(String::new(), |s| format!(", through the slot {s:#x}"));
        println!("  original kernel function {target:#x}, {} bytes{through}", end - target);
        println!("  matcher: {}", self.functions.get(&hex(target)).map_or("None".to_string(), repr_value));
        if let Ok(index) = self.starts.binary_search(&target) {
            println!("  paired neighbours before: {}", repr_strs(&self.near(index, target, -1)));
            println!("  paired neighbours after:  {}", repr_strs(&self.near(index, target, 1)));
        }
        let at = self.cache.offset_of(target, end - target)?;
        let mut strings: Vec<&str> = Vec::new();
        for r in scan(&self.raw[at..at + (end - target) as usize], target).refs {
            // (a reference into a section without file bytes is no string)
            if let Ok(Some(value)) = self.kernel.cstring(r.target) {
                if value.len() > 3 && !strings.contains(&value) {
                    strings.push(value);
                }
            }
        }
        println!("  strings: {}", repr_strs(&strings[..strings.len().min(8)]));
        println!("{}", self.disassemble(target, args.lines.min((end - target) / 4))?.join("\n"));
        Ok(())
    }
}

pub fn run(args: Args) -> Result<()> {
    let raw = oneios_common::Map::open(&args.kernelcache)?;
    let report = pyjson::load(&args.report)?;
    let number = |key: &str| report.get(key).and_then(Value::as_str).ok_or_else(|| anyhow!("{}: no {key}", args.report)).and_then(unhex);
    let (slide, link) = (number("collection_slide")?, number("native_link_address")?);
    let functions = match &args.map {
        Some(map) => pyjson::load(map)?.get("functions").cloned().ok_or_else(|| anyhow!("{map}: no functions"))?,
        None => Value::dict(),
    };
    let cache = Image::new(&raw, 0)?;
    let kernel = cache.entry("com.apple.kernel")?;
    let mut owners = Vec::new();
    for (name, (_, at)) in &cache.entries {
        for s in Image::new(&raw, *at as usize)?.segments.iter().filter(|s| s.size != 0 && s.name != "__LINKEDIT") {
            owners.push((s.va, s.va + s.size, name.clone(), s.name.clone()));
        }
    }
    let mut starts = kernel.function_starts()?;
    starts.sort_unstable();
    let reader = Cache { raw: &raw, kernel_ranges: kernel.mapped(), fixups: cache.fixups()?, cache, kernel, owners, starts, functions };
    for text in &args.address {
        reader.answer(&args, text, slide, link)?;
    }
    Ok(())
}
