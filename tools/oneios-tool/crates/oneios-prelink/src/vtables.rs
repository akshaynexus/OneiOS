//! `prelink vtables`: the virtual methods of every vtable paired by name, slot by slot. A port of
//! kernel/prelink/vtables.py (its header has the method): the same vtables.json byte for byte.

use crate::image::{hex, Image};
use crate::records;
use anyhow::{Context, Result};
use oneios_common::pyjson::{self, Value};
use std::collections::{BTreeMap, HashMap};

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
    /// earlier pairs that stand (structors)
    #[arg(long)]
    also: Vec<String>,
    /// earlier inferred pairs (align) that a slot overrides
    #[arg(long)]
    weak: Vec<String>,
    /// kclasses output: more vtables by name
    #[arg(long)]
    classes: Option<String>,
}

/// The vtables named so far: the pairing's, then kclasses', then the reviewed records' (later ones win).
pub fn named_vtables(pairing: &Value, classes: Option<&str>, reviewed: &Value) -> Result<BTreeMap<u64, String>> {
    let mut vtables: BTreeMap<u64, String> = records::named(pairing, "data")?.into_iter().filter(|(_, n)| n.starts_with("__ZTV")).collect();
    if let Some(path) = classes {
        vtables.extend(records::pairs(&pyjson::load(path)?, "data")?.into_iter().filter(|(_, n)| n.starts_with("__ZTV")));
    }
    vtables.extend(records::pairs(reviewed, "data")?.into_iter().filter(|(_, n)| n.starts_with("__ZTV")));
    Ok(vtables)
}

pub fn run(args: Args) -> Result<()> {
    let _ = &args.xnuprelink;
    let raw = oneios_common::Map::open(&args.kernelcache)?;
    let pairing = pyjson::load(&args.map)?;
    let reviewed = pyjson::load(&args.reviewed)?;
    let cache = Image::new(&raw, 0)?;
    let kernel = cache.entry("com.apple.kernel")?;
    let code: Vec<(u64, u64)> = kernel.segments.iter().filter(|s| s.name.contains("EXEC")).map(|s| (s.va, s.va + s.size)).collect();
    let fixups = cache.fixups()?;
    let native_raw = oneios_common::Map::open(&args.native)?;
    let native = Image::new(&native_raw, 0)?;
    let symbols = native.symbols()?;
    let native_fixups = native.fixups()?;
    let (mut by_name, mut count): (HashMap<&str, u64>, HashMap<&str, u32>) = (HashMap::new(), HashMap::new());
    for (address, names) in &symbols {
        for name in names {
            by_name.insert(name, *address);
            *count.entry(name).or_default() += 1;
        }
    }
    let mut name_of: HashMap<u64, String> = records::named(&pairing, "functions")?.into_iter().collect();
    for path in &args.also {
        for (original, symbol) in records::pairs(&pyjson::load(path)?, "pairs")? {
            name_of.entry(original).or_insert(symbol);
        }
    }
    let mut weak: HashMap<u64, String> = HashMap::new();
    for path in &args.weak {
        for (original, symbol) in records::pairs(&pyjson::load(path)?, "pairs")? {
            if !name_of.contains_key(&original) {
                weak.insert(original, symbol);
            }
        }
    }
    name_of.extend(weak.iter().map(|(k, v)| (*k, v.clone())));
    for (original, symbol) in records::pairs(&reviewed, "functions")? {
        name_of.insert(original, symbol);
        weak.remove(&original);
    }
    let vtables = named_vtables(&pairing, args.classes.as_deref(), &reviewed)?;
    let (mut pairs, mut conflicts, mut used) = (Vec::new(), Vec::new(), 0);
    for (original, name) in &vtables {
        if count.get(name.as_str()) != Some(&1) {
            continue;
        }
        let ours = by_name[name.as_str()];
        let mut slots: Vec<(u64, u64, Vec<&String>)> = Vec::new();
        for slot in 0..4096u64 {
            let (Some(left), Some(right)) = (fixups.get(&(original + 16 + 8 * slot)), native_fixups.get(&(ours + 16 + 8 * slot))) else { break };
            if !code.iter().any(|(b, e)| *b <= left.target && left.target < *e) {
                break;
            }
            let mut names: Vec<&String> = symbols.get(&right.target).map(|n| n.iter().collect()).unwrap_or_default();
            names.sort();
            slots.push((slot, left.target, names));
        }
        let says = |s: &(u64, u64, Vec<&String>)| name_of.get(&s.1).map(|n| s.2.contains(&n));
        let agree = slots.iter().filter(|s| says(s) == Some(true)).count();
        let differ: Vec<&(u64, u64, Vec<&String>)> = slots.iter().filter(|s| !s.2.is_empty() && says(s) == Some(false)).collect();
        // an order-inferred pair is no evidence against a slot
        let firm = differ.iter().filter(|s| !weak.contains_key(&s.1)).count();
        let usable_vtable = agree >= 5 && firm * 10 <= agree;
        for (slot, target, names) in differ.iter().map(|s| (s.0, s.1, &s.2)) {
            let mut row = Value::dict();
            row.set("vtable", name);
            row.set("slot", Value::Int(slot as i128));
            row.set("original", hex(target));
            row.set("paired", &name_of[&target]);
            row.set("slot_says", names[0]);
            row.set("overridden", usable_vtable && weak.get(&target) == Some(&name_of[&target]));
            conflicts.push(row);
        }
        if !usable_vtable {
            continue;
        }
        used += 1;
        let total = slots.len();
        for (slot, target, names) in &slots {
            if let Some(current) = name_of.get(target) {
                if !(weak.contains_key(target) && !names.contains(&current)) {
                    continue;
                }
            }
            // No "name already used" test here: the original has a second copy of some vtables (IOService's)
            // whose entries are thunks to the same methods, so two original addresses rightly share one name.
            let Some(usable) = names.iter().find(|n| count.get(n.as_str()) == Some(&1) && !n.contains("pure_virtual")) else { continue };
            name_of.insert(*target, (*usable).clone());
            let mut row = Value::dict();
            row.set("original", hex(*target));
            row.set("native_symbol", *usable);
            row.set("score", 3.0);
            row.set("by", "slot");
            row.set("shared_features", Value::Int(agree as i128));
            row.set("features", Value::Int(total as i128));
            row.set("between", vec![name.clone(), format!("slot {slot}")]);
            pairs.push(row);
        }
    }
    let mut record = Value::dict();
    record.set("kernelcache_sha256", oneios_common::sha256_hex(&raw));
    record.set("vtables_named", Value::Int(vtables.len() as i128));
    record.set("vtables_used", Value::Int(used));
    let (count, disagree) = (pairs.len(), conflicts.len());
    record.set("pairs", Value::List(pairs));
    record.set("conflicts", Value::List(conflicts));
    std::fs::write(&args.out, pyjson::dumps(&record, Some(1), false) + "\n").with_context(|| args.out.clone())?;
    println!("vtables: {count} methods paired by slot in {used} of {} named vtables; {disagree} slots disagree with an earlier pair", vtables.len());
    Ok(())
}
