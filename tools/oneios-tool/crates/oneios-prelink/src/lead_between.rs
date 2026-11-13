//! `oneios-tool prelink lead-between NAME_A NAME_B [--add [NOTE]]` (was kernel/prelink/lead/between.py, retired
//! 2026-11-13): the original kernel's functions between two paired ones, and the native kernel's between the same
//! two names, side by side by position (for pairing a block of small accessors by source order).
//! --add: when both sides have the same count, every known pair agrees with its position and each function has
//! the shape of its partner, the unknown ones are added to kernel/prelink/lead/lead-reviewed.json (NOTE ends the
//! evidence text). The files it reads: lead.rs.

use std::collections::HashSet;

use anyhow::{anyhow, Context, Result};
use indexmap::IndexMap;
use oneios_common::pyjson::Value;

use crate::image::Image;
use crate::lead::{function_record, list, original, stop, text, Lead};

#[derive(clap::Args, Debug)]
pub struct Args {
    name_a: String,
    name_b: String,
    /// Add the unknown functions of the block to the lead's reviewed records
    #[arg(long, num_args = 0..=1, default_missing_value = "")]
    add: Option<String>,
    /// This repository's checkout (default: the current directory)
    #[arg(long)]
    repo: Option<String>,
}

/// original address -> native name, in the order the sources give them: the matcher's map, the reviewed and the
/// lead's records (which replace a name), then what the emitter's report says it applied (where nothing is known).
fn names(lead: &Lead) -> Result<IndexMap<u64, String>> {
    let mut name_of = IndexMap::new();
    for (address, pair) in lead.map.get("functions").and_then(Value::as_dict).context("match.map.json: no functions")? {
        // (a pair without a name is no name: the script kept None there)
        if let Some(name) = pair.get("name").and_then(Value::as_str) {
            name_of.insert(crate::image::unhex(address)?, name.to_string());
        }
    }
    for f in lead.reviewed_functions()? {
        name_of.insert(original(f)?, text(f, "native_symbol")?.to_string());
    }
    for (_, group) in lead.report.get("reviewed_applied").and_then(Value::as_dict).unwrap_or(&[]) {
        for f in group.as_list().unwrap_or(&[]).iter().filter(|f| f.as_dict().is_some() && f.get("original").is_some()) {
            let name = ["native_symbol", "name"].iter().find_map(|k| f.get(k).and_then(Value::as_str).filter(|n| !n.is_empty()));
            if let Some(name) = name {
                name_of.entry(original(f)?).or_insert_with(|| name.to_string());
            }
        }
    }
    Ok(name_of)
}

/// The original functions the emitter put on the stub.
fn stubbed(report: &Value) -> Result<HashSet<u64>> {
    let mut stub = HashSet::new();
    for x in report.get("stubbed_functions").and_then(Value::as_list).unwrap_or(&[]) {
        let address = if x.as_dict().is_some() { text(x, "original")? } else { x.as_str().context("stubbed_functions: not an address")? };
        stub.insert(crate::image::unhex(address)?);
    }
    Ok(stub)
}

/// The first ten words of a function's first `size` bytes.
fn words(kernel: &Image, address: u64, size: u64) -> Result<String> {
    let bytes = kernel.read_of(address, size)?;
    Ok(bytes.chunks_exact(4).take(10).map(|w| format!("{:08x}", u32::from_le_bytes(w.try_into().unwrap()))).collect::<Vec<_>>().join(" "))
}

pub fn run(args: Args) -> Result<()> {
    let mut lead = Lead::open(args.repo.as_deref())?;
    let (a, b) = (&args.name_a, &args.name_b);
    let name_of = names(&lead)?;
    // (of two originals with one name, the later one of that order)
    let orig_of = |name: &str| name_of.iter().rev().find(|(_, n)| *n == name).map(|(o, _)| *o).ok_or_else(|| anyhow!("{name} is not the name of a paired original function"));
    let stub = stubbed(&lead.report)?;
    let raw = oneios_common::Map::open(&lead.kernelcache)?;
    let kernel = Image::new(&raw, 0)?.entry(text(&lead.reviewed, "entry")?)?;
    let mut starts = kernel.function_starts()?;
    starts.sort_unstable();
    let (oa, ob) = (orig_of(a)?, orig_of(b)?);
    let (i, j) = (starts.partition_point(|s| *s < oa), starts.partition_point(|s| *s < ob));
    let native: Vec<(u64, String)> = lead
        .nm(&["-n"])?
        .into_iter()
        .filter(|(_, kind, name)| "tTsS".contains(kind.as_str()) && !["ltmp", "l_", "L"].iter().any(|p| name.starts_with(p)) && !name.ends_with(".original"))
        .map(|(address, _, name)| (address, name))
        .collect();
    let place = |name: &str| native.iter().position(|(_, n)| n == name).ok_or_else(|| anyhow!("{name} is not a function symbol of the native kernel"));
    let (na, nb) = (place(a)?, place(b)?);
    println!("original: {} functions between; native: {}", j as i64 - i as i64 - 1, nb as i64 - na as i64 - 1);
    let orig_rows: Vec<(u64, u64)> = (i..=j).map(|k| (starts[k], starts[k + 1] - starts[k])).collect();
    let nat_rows: Vec<(&str, u64)> = (na..=nb).map(|k| (native[k].1.as_str(), native[k + 1].0 - native[k].0)).collect();
    for k in 0..orig_rows.len().max(nat_rows.len()) {
        let left = match orig_rows.get(k) {
            Some((address, size)) => {
                let name = name_of.get(address).map_or("?", String::as_str);
                format!("{address:#x} {size:5} {} {name:40.40}", if stub.contains(address) { "STUB" } else { "    " })
            }
            None => " ".repeat(70),
        };
        let right = nat_rows.get(k).map_or(String::new(), |(name, size)| format!("{size:5} {name}"));
        let code = match orig_rows.get(k) {
            Some((address, size)) if !name_of.contains_key(address) => words(&kernel, *address, (*size).min(40))?,
            _ => String::new(),
        };
        println!("{left} | {right} | {code}");
    }
    let Some(note) = &args.add else { return Ok(()) };
    if orig_rows.len() != nat_rows.len() {
        return stop("pairblock: the counts differ; nothing added".to_string());
    }
    let rows = || orig_rows.iter().zip(&nat_rows);
    // a locked update is 100 bytes or more in the original and 44 or more in ours; a short leaf is under both
    let bad: Vec<String> = rows().filter(|(o, n)| (o.1 >= 100) != (n.1 >= 44) && !name_of.contains_key(&o.0)).map(|(o, n)| format!("{:#x}/{} vs {}/{}", o.0, o.1, n.0, n.1)).collect();
    if !bad.is_empty() {
        return stop(format!("pairblock: shapes disagree: {}", bad.join(", ")));
    }
    let wrong: Vec<String> = rows()
        .filter_map(|(o, n)| name_of.get(&o.0).filter(|name| *name != n.0).map(|name| format!("{:#x} is {name}, position says {}", o.0, n.0)))
        .collect();
    if !wrong.is_empty() {
        return stop(format!("pairblock: existing pairs disagree with the position: {}", wrong.join(", ")));
    }
    let mut added = 0;
    for (o, n) in rows().filter(|(o, _)| !name_of.contains_key(&o.0)) {
        let evidence = format!(
            "By position in a block of accessors: between {a} ({oa:#x}) and {b} ({ob:#x}) the original kernel and ours have the same number of functions ({}), in the same order as the source file, every pair already known in the block agrees with its position, and each function has the shape of its partner (a locked update or a short leaf). {note}",
            orig_rows.len() - 2
        );
        lead.add(function_record(o.0, n.0, kernel.read_of(o.0, o.1)?, evidence));
        added += 1;
    }
    lead.save()?;
    println!("pairblock: {added} records added ({} in all)", list(&lead.lead, "functions")?.len());
    Ok(())
}
