//! `oneios-tool debug kcmap` (was tests/ksym/kcmap.py): the test's own reader of a kernel collection, written
//! apart from oneios-tool's (crates/oneios-ksym, crates/oneios-prelink) so that tests/ksym/compare.sh does not
//! check the tool against itself.
//!
//!   debug kcmap members KC                    every fileset member: name, header address, segments, symbols kept
//!   debug kcmap forward KC KERNEL NAME...     per symbol of KERNEL (xnu.macho): its address there, the segment, and
//!                                             the address in KC's com.apple.kernel (the segment's own move added)
//!   debug kcmap own KC NAME...                the address of a symbol in com.apple.kernel's own symbol table in KC
//!                                             (a third source, when the collection keeps the kernel's symbols)
//!
//! Only byte reads over a memory map: no Mach-O library. Reads the files in place and writes nothing.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_FILESET_ENTRY: u32 = 0x8000_0035;
const MH_MAGIC_64: u32 = 0xFEED_FACF;
const MH_FILESET: u32 = 0xC;

/// A load command: (cmd, its offset, its size).
pub type Cmd = (u32, usize, usize);

/// A segment of a member.
#[derive(Clone, Debug)]
pub struct Seg {
    pub name: String,
    pub vmaddr: u64,
    pub vmsize: u64,
    pub fileoff: u64,
    pub filesize: u64,
}

impl Seg {
    pub fn holds(&self, addr: u64) -> bool {
        self.vmaddr <= addr && addr < self.vmaddr + self.vmsize
    }
}

pub fn open_map(path: &str) -> Result<memmap2::Mmap> {
    let file = std::fs::File::open(path).with_context(|| path.to_string())?;
    // Safety: the file is read in place and not changed while the helper runs.
    Ok(unsafe { memmap2::Mmap::map(&file) }.with_context(|| path.to_string())?)
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

/// The file type and the load commands of the Mach-O header at `at`.
pub fn commands(data: &[u8], at: usize) -> Result<(u32, Vec<Cmd>)> {
    if u32_at(data, at) != MH_MAGIC_64 {
        bail!("no 64-bit Mach-O header at offset {at:#x}");
    }
    let (filetype, ncmds) = (u32_at(data, at + 12), u32_at(data, at + 16));
    let mut off = at + 32;
    let mut out = Vec::new();
    for _ in 0..ncmds {
        let (cmd, size) = (u32_at(data, off), u32_at(data, off + 4) as usize);
        out.push((cmd, off, size));
        off += size;
    }
    Ok((filetype, out))
}

/// The bytes up to the first NUL, as latin-1.
fn cstr(raw: &[u8]) -> String {
    raw.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect()
}

pub fn segments(data: &[u8], cmds: &[Cmd]) -> Vec<Seg> {
    let mut out = Vec::new();
    for &(cmd, off, _) in cmds {
        if cmd == LC_SEGMENT_64 {
            out.push(Seg {
                name: cstr(&data[off + 8..off + 24]),
                vmaddr: u64_at(data, off + 24),
                vmsize: u64_at(data, off + 32),
                fileoff: u64_at(data, off + 40),
                filesize: u64_at(data, off + 48),
            });
        }
    }
    out
}

/// name -> address for the defined symbols (N_SECT, no stabs); empty without LC_SYMTAB. The first entry of a
/// name counts.
pub fn symbols(data: &[u8], cmds: &[Cmd]) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    for &(cmd, off, _) in cmds {
        if cmd != LC_SYMTAB {
            continue;
        }
        let (symoff, nsyms) = (u32_at(data, off + 8) as usize, u32_at(data, off + 12) as usize);
        let (stroff, strsize) = (u32_at(data, off + 16) as usize, u32_at(data, off + 20) as usize);
        for i in 0..nsyms {
            let entry = symoff + 16 * i;
            let (strx, ntype, value) = (u32_at(data, entry) as usize, data[entry + 4], u64_at(data, entry + 8));
            if ntype & 0xE0 != 0 || (ntype & 0x0E) != 0x0E {
                continue;
            }
            let name = cstr(&data[stroff + strx..stroff + strsize]);
            out.entry(name).or_insert(value);
        }
    }
    out
}

/// The fileset's members: (name, header address, file offset).
pub fn members(data: &[u8]) -> Result<Vec<(String, u64, u64)>> {
    let (filetype, cmds) = commands(data, 0)?;
    if filetype != MH_FILESET {
        return Ok(vec![("(not a fileset)".to_string(), 0, 0)]);
    }
    let mut out = Vec::new();
    for (cmd, off, _) in cmds {
        if cmd == LC_FILESET_ENTRY {
            let (vmaddr, fileoff, name_at) = (u64_at(data, off + 8), u64_at(data, off + 16), u32_at(data, off + 24) as usize);
            out.push((cstr(&data[off + name_at..]), vmaddr, fileoff));
        }
    }
    Ok(out)
}

/// The load commands of the collection's com.apple.kernel member.
pub fn kernel_member(data: &[u8]) -> Result<Vec<Cmd>> {
    for (name, _vmaddr, fileoff) in members(data)? {
        if name == "com.apple.kernel" {
            return Ok(commands(data, fileoff as usize)?.1);
        }
    }
    bail!("the collection has no com.apple.kernel member")
}

/// The segments of a member by name; of two with one name the later one counts.
pub fn by_name(segs: Vec<Seg>) -> HashMap<String, Seg> {
    segs.into_iter().map(|s| (s.name.clone(), s)).collect()
}

/// A symbol by its name, or by the name with the leading underscore.
fn lookup(table: &HashMap<String, u64>, name: &str) -> Option<u64> {
    table.get(name).or_else(|| table.get(&format!("_{name}"))).copied()
}

const USAGE: &str = "usage: oneios-tool debug kcmap members KC | forward KC KERNEL NAME... | own KC NAME...";

pub fn run(args: &[String]) -> Result<i32> {
    match args.first().map(String::as_str) {
        Some("members") if args.len() >= 2 => show_members(&args[1]),
        Some("forward") if args.len() >= 4 => forward(&args[1], &args[2], &args[3..]),
        Some("own") if args.len() >= 3 => own(&args[1], &args[2..]),
        _ => {
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

fn show_members(path: &str) -> Result<i32> {
    let data = open_map(path)?;
    for (name, vmaddr, fileoff) in members(&data)? {
        let (_, cmds) = commands(&data, fileoff as usize)?;
        println!("{name}  header {vmaddr:#x}  file {fileoff:#x}  symbols {}", symbols(&data, &cmds).len());
        for s in segments(&data, &cmds) {
            println!("    {:<18} {:#x} +{:#x}  file {:#x} +{:#x}", s.name, s.vmaddr, s.vmsize, s.fileoff, s.filesize);
        }
    }
    Ok(0)
}

fn forward(kc_path: &str, kernel_path: &str, names: &[String]) -> Result<i32> {
    let (kc, kernel) = (open_map(kc_path)?, open_map(kernel_path)?);
    let in_kc = by_name(segments(&kc, &kernel_member(&kc)?));
    let (_, cmds) = commands(&kernel, 0)?;
    let segs = segments(&kernel, &cmds);
    let table = symbols(&kernel, &cmds);
    let mut status = 0;
    for name in names {
        let Some(addr) = lookup(&table, name) else {
            println!("{name} - - - (no such symbol in the kernel)");
            status = 1;
            continue;
        };
        let seg = segs.iter().find(|s| s.holds(addr));
        match seg.and_then(|s| in_kc.get(&s.name).map(|k| (s, k))) {
            Some((s, k)) => println!("{name} {addr:#x} {} {:#x}", s.name, addr.wrapping_add(k.vmaddr).wrapping_sub(s.vmaddr)),
            None => {
                println!("{name} {addr:#x} {} - (the segment is not in the collection)", seg.map_or("-", |s| s.name.as_str()));
                status = 1;
            }
        }
    }
    Ok(status)
}

fn own(kc_path: &str, names: &[String]) -> Result<i32> {
    let kc = open_map(kc_path)?;
    let table = symbols(&kc, &kernel_member(&kc)?);
    for name in names {
        match lookup(&table, name) {
            Some(addr) => println!("{name} {addr:#x}"),
            None => println!("{name} -"),
        }
    }
    Ok(0)
}
