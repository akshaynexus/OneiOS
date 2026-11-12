//! `prelink structors`: constructors and destructors by the vtable they store, factories by their init slot, VNOP
//! wrappers by their descriptor. A port of kernel/prelink/structors.py (its header has the method): the same
//! structors.json byte for byte.
//!
//! The step as its Python original described it (kernel/prelink/structors.py, retired 2026-11-12 after the
//! comparison in tools/oneios-tool/README.md "prelink"; the usage line is that script's):
//!
//! ```text
//! Pair the constructors and base destructors of kernel classes that kexts subclass, by the vtable they store.
//!
//! usage: structors.py --xnuprelink DIR --kernelcache KC --native NATIVE --map MAP --reviewed reviewed.json --out structors.json
//!
//! OSDefineMetaClassAndStructors emits, for a class X, X::X(const OSMetaClass *), (with default structors) X::X(), and
//! X::~X(), in that order, after the file's static initializer. In the original kernel each constructor is
//!     mov w8, #1; str w8, [x0, #8]; adrp/add x16, <X's vtable>; add x16, x16, #16; ...; pacda; str x16, [x0]
//! (the inlined OSObject constructor, then the vtable) possibly followed by member initializers, and a trivial base
//! destructor is `mov x8, #-1; str x8, [x0]; ret`. Kext subclasses call them directly, so each is a kext-facing
//! function, and none has a string or a named callee for the matcher to go by.
//!
//! For every vtable the pairing or the reviewed file names (__ZTV<X>), the unpaired original functions whose first
//! instructions store that vtable + 16 into [x0] after the retain count are, in address order, C2EPK11OSMetaClass and
//! C2Ev; the unpaired function that directly follows them and is the trivial destructor is D2Ev. A name is used only
//! when the native kernel defines it once. Factories (X::withFoo) are paired by the class they allocate and the init
//! slot they call; see the code. Output: pairs in align.py's format ('by': 'vtable').
//! ```

use crate::image::{hex, scan, u32_at, Image, RefKind};
use crate::kclasses::name_counts;
use crate::records;
use crate::vtables::named_vtables;
use anyhow::{bail, Context, Result};
use oneios_common::pyjson::{self, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(clap::Args, Debug)]
pub struct Args {
    #[arg(long)]
    xnuprelink: String,
    #[arg(long)]
    kernelcache: String,
    #[arg(long)]
    native: String,
    #[arg(long)]
    map: String,
    #[arg(long)]
    reviewed: String,
    #[arg(long)]
    out: String,
    /// kclasses output: more vtables by name
    #[arg(long)]
    classes: Option<String>,
}

/// Python's `s[a:b]` for ASCII text: clamped, never an error.
fn slice(s: &str, a: usize, b: usize) -> &str {
    let (a, b) = (a.min(s.len()), b.min(s.len()));
    if a >= b {
        ""
    } else {
        s.get(a..b).unwrap_or("")
    }
}

pub fn run(args: Args) -> Result<()> {
    let _ = &args.xnuprelink;
    let raw = oneios_common::Map::open(&args.kernelcache)?;
    let pairing = pyjson::load(&args.map)?;
    let reviewed = pyjson::load(&args.reviewed)?;
    let cache = Image::new(&raw, 0)?;
    let kernel = cache.entry("com.apple.kernel")?;
    let mut starts = kernel.function_starts()?;
    starts.sort_unstable();
    let mut paired: HashSet<u64> = records::originals(&pairing, "functions")?.into_iter().collect();
    paired.extend(records::pairs(&reviewed, "functions")?.into_iter().map(|p| p.0));
    let vtables = named_vtables(&pairing, args.classes.as_deref(), &reviewed)?;
    let native_raw = oneios_common::Map::open(&args.native)?;
    let native_image = Image::new(&native_raw, 0)?;
    let native = name_counts(&native_image)?;
    let exec_ranges: Vec<(u64, u64)> = kernel.segments.iter().filter(|s| s.name == "__TEXT_EXEC").map(|s| (s.va, s.va + s.size)).collect();
    let in_exec = |a: u64| exec_ranges.iter().any(|(b, e)| *b <= a && a < *e);
    let words_of = |blob: &[u8]| -> Vec<u32> { (0..blob.len() / 4).map(|i| u32_at(blob, i * 4)).collect() };
    let mut constructors: BTreeMap<u64, Vec<(u64, u64)>> = BTreeMap::new();
    for i in 0..starts.len().saturating_sub(1) {
        let start = starts[i];
        if !in_exec(start) {
            continue;
        }
        let size = (starts[i + 1] - start).min(48);
        let Some(blob) = kernel.read(start, size) else { continue };
        let words = words_of(blob);
        // mov w8, #1 ; str w8, [x0, #8] within the first three instructions
        if !words.iter().take(3).any(|w| *w == 0x5280_0028) || !words.iter().take(4).any(|w| *w == 0xb900_0808) {
            continue;
        }
        if let Some(r) = scan(blob, start).refs.iter().find(|r| r.kind == RefKind::Add && vtables.contains_key(&r.target)) {
            constructors.entry(r.target).or_default().push((start, starts[i + 1]));
        }
    }
    let mut pairs = Vec::new();
    let add = |pairs: &mut Vec<Value>, paired: &mut HashSet<u64>, address: u64, name: String, cls: &str, why: &str| {
        if paired.contains(&address) || native.get(&name) != Some(&1) {
            return;
        }
        let mut row = Value::dict();
        row.set("original", hex(address));
        row.set("native_symbol", name);
        row.set("score", 3.0);
        row.set("by", "vtable");
        row.set("shared_features", Value::Int(1));
        row.set("features", Value::Int(1));
        row.set("between", vec![cls.to_string(), why.to_string()]);
        pairs.push(row);
        paired.insert(address);
    };
    for (vtable, found) in constructors.iter_mut() {
        let name = &vtables[vtable];
        let cls = &name["__ZTV".len()..];
        found.sort_unstable();
        // The macro's pair comes first and adjacent; later functions that store the vtable are left to a review.
        let found: &[(u64, u64)] = if found.len() >= 2 && found[0].1 == found[1].0 {
            &found[..2]
        } else if found.len() == 1 {
            &found[..1]
        } else {
            continue;
        };
        add(&mut pairs, &mut paired, found[0].0, format!("__ZN{cls}C2EPK11OSMetaClass"), name, "first function that stores the vtable");
        if found.len() == 2 {
            add(&mut pairs, &mut paired, found[1].0, format!("__ZN{cls}C2Ev"), name, "second function that stores the vtable");
        }
        let after = found[found.len() - 1].1;
        let Some(trivial) = kernel.read(after, 16) else { continue };
        if words_of(trivial)[1..] == [0x9280_0008, 0xf900_0008, 0xd65f_03c0] {
            add(&mut pairs, &mut paired, after, format!("__ZN{cls}D2Ev"), name, "the trivial destructor that follows the constructors");
        }
    }
    let constructed = pairs_len(&pairs);
    // Factories: allocate from X::gMetaClass, then the object's virtual initWithFoo at some slot.
    let mut metaclasses: HashMap<u64, String> = records::named(&pairing, "data")?.into_iter().filter(|(_, n)| n.ends_with("10gMetaClassE")).collect();
    if let Some(path) = &args.classes {
        metaclasses.extend(records::pairs(&pyjson::load(path)?, "data")?.into_iter().filter(|(_, n)| n.ends_with("10gMetaClassE")));
    }
    let (native_symbols, native_fixups) = (native_image.symbols()?, native_image.fixups()?);
    let mut address_of: HashMap<&str, u64> = HashMap::new();
    for (address, names) in &native_symbols {
        for name in names {
            address_of.insert(name, *address);
        }
    }
    let mut candidates: BTreeMap<String, Vec<(u64, String, String)>> = BTreeMap::new();
    for i in 0..starts.len().saturating_sub(1) {
        let start = starts[i];
        if paired.contains(&start) || !in_exec(start) {
            continue;
        }
        let size = starts[i + 1] - start;
        if !(96..=240).contains(&size) {
            continue;
        }
        let blob = kernel.read_of(start, size)?;
        let named: Vec<&String> = scan(&blob[..64], start).refs.iter().filter(|r| r.kind == RefKind::Add).filter_map(|r| metaclasses.get(&r.target)).collect();
        if named.len() != 1 {
            continue;
        }
        let words = words_of(blob);
        let mut offsets = Vec::new();
        for (at, word) in words.iter().enumerate() {
            if word & 0xffe0_0fff == 0xf840_0e08 {
                offsets.push((word >> 12) & 0x1ff); // ldr x8, [x16, #imm]!
            } else if *word == 0x8b11_0210 && at != 0 && words[at - 1] & 0xffe0_001f == 0xd280_0011 {
                offsets.push((words[at - 1] >> 5) & 0xffff); // mov x17, #imm; add x16, x16, x17
            }
        }
        // alloc, init, and at most the release of a failed init (vtable +40): nothing else is called virtually
        if offsets.len() < 2 || offsets[0] != 104 || offsets[2..].iter().any(|o| *o != 40) || offsets.len() > 3 {
            continue;
        }
        let cls = slice(named[0], "__ZN".len(), named[0].len() - "10gMetaClassE".len());
        let Some(vtable) = address_of.get(format!("__ZTV{cls}").as_str()).filter(|v| **v != 0) else { continue };
        let Some(slot) = native_fixups.get(&(vtable + 16 + offsets[1] as u64)) else { continue };
        for init in native_symbols.get(&slot.target).map_or(&[][..], Vec::as_slice) {
            let prefix = format!("__ZN{cls}");
            let rest = slice(init, prefix.len(), init.len());
            let digits: String = slice(rest, 0, 3).chars().filter(char::is_ascii_digit).collect();
            if !init.starts_with(&prefix) || digits.is_empty() || !slice(rest, digits.len(), rest.len()).starts_with("initWith") {
                continue;
            }
            let length: usize = digits.parse()?;
            let method = slice(slice(rest, digits.len(), digits.len() + length), 4, usize::MAX); // WithFoo
            if method.is_empty() {
                bail!("{init}: no method name after \"init\" (Python's IndexError)");
            }
            let method = format!("{}{}", method[..1].to_lowercase(), &method[1..]);
            let factory = format!("{prefix}{}{method}{}", method.len(), slice(rest, digits.len() + length, rest.len()));
            candidates.entry(factory).or_default().push((start, named[0].clone(), format!("allocates, then the virtual {init} at vtable +{}", offsets[1])));
        }
    }
    for (factory, found) in &candidates {
        // a larger function that begins with an inlined factory looks the same: one only
        if found.len() == 1 {
            add(&mut pairs, &mut paired, found[0].0, factory.clone(), &found[0].1, &found[0].2);
        }
    }
    let factories = pairs_len(&pairs) - constructed;
    // VNOP wrappers: an unpaired function whose only named data is one operation descriptor, in kpi_vfs.c's range.
    let is_desc = |n: &String| n.starts_with("_vnop_") && n.ends_with("_desc");
    let mut descriptors: HashMap<u64, String> = records::named(&pairing, "data")?.into_iter().filter(|(_, n)| is_desc(n)).collect();
    descriptors.extend(records::pairs(&reviewed, "data")?.into_iter().filter(|(_, n)| is_desc(n)));
    let wrappers_paired: Vec<(u64, String)> = records::named(&pairing, "functions")?.into_iter().filter(|(_, n)| n.starts_with("_VNOP_")).collect();
    let mut wrapper_names: HashSet<String> = wrappers_paired.iter().map(|w| w.1.clone()).collect();
    wrapper_names.extend(records::pairs(&reviewed, "functions")?.into_iter().map(|p| p.1));
    let (low, high) = (wrappers_paired.iter().map(|w| w.0).min().unwrap_or(0), wrappers_paired.iter().map(|w| w.0).max().unwrap_or(0));
    let mut wrappers: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for i in 0..starts.len().saturating_sub(1) {
        let start = starts[i];
        if paired.contains(&start) || !(low.saturating_sub(0x1000) <= start && start <= high + 0x1000) {
            continue;
        }
        let size = starts[i + 1] - start;
        if !(40..=1200).contains(&size) {
            continue;
        }
        let named: BTreeSet<&String> = scan(kernel.read_of(start, size.min(160))?, start).refs.iter().filter_map(|r| descriptors.get(&r.target)).collect();
        if named.len() == 1 {
            wrappers.entry((*named.iter().next().unwrap()).clone()).or_default().push(start);
        }
    }
    for (descriptor, found) in &wrappers {
        let name = format!("_VNOP_{}", descriptor["_vnop_".len()..descriptor.len() - "_desc".len()].to_uppercase());
        if found.len() == 1 && !wrapper_names.contains(&name) {
            add(&mut pairs, &mut paired, found[0], name, descriptor, "the one unpaired function in kpi_vfs.c's range that passes this operation descriptor");
        }
    }
    let vnops = pairs_len(&pairs) - constructed - factories;
    let mut record = Value::dict();
    record.set("kernelcache_sha256", oneios_common::sha256_hex(&raw));
    record.set("vtables_named", Value::Int(vtables.len() as i128));
    record.set("factories", Value::Int(factories as i128));
    record.set("vnop_wrappers", Value::Int(vnops as i128));
    record.set("classes_with_constructors", Value::Int(constructors.len() as i128));
    record.set("pairs", Value::List(pairs));
    std::fs::write(&args.out, pyjson::dumps(&record, Some(1), false) + "\n").with_context(|| args.out.clone())?;
    println!(
        "structors: {constructed} constructors and destructors of {} classes paired by their vtable, {factories} factories by their init slot, {vnops} VNOP wrappers by their descriptor ({} vtables named)",
        constructors.len(),
        vtables.len()
    );
    Ok(())
}

fn pairs_len(pairs: &[Value]) -> usize {
    pairs.len()
}
