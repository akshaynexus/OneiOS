//! `prelink merge-reviewed --reviewed OWNER.json --native native.macho --out merged.json EXTRA.json...`: the other
//! agents' reviewed function records merged into the owner's for one build. A port of
//! kernel/prelink/merge-reviewed.py: the same merged file byte for byte and the same lines on stdout.
//!
//! One difference in how, not in what: the Python step asks `llvm-nm --defined-only` which names the kernel
//! defines; this one reads the kernel's symbol table itself (the same rows: every symbol that is not a debug
//! entry and is defined in a section or absolute), so the step needs no LLVM.
//!
//! The step as its Python original described it (kernel/prelink/merge-reviewed.py, retired 2026-11-12 after the
//! comparison in tools/oneios-tool/README.md "prelink"; the usage line is that script's):
//!
//! ```text
//! Merge the other agents' reviewed function records into the owner's reviewed file for one build.
//!
//! usage: merge-reviewed.py --reviewed OWNER.json --native native.macho --out merged.json EXTRA.json...
//!
//! kernel/prelink/reviewed-*.json is fable-prelink's; the lead and fable-user keep their own records beside it
//! (lead/reviewed-lead-*.json, user/reviewed-user-*.json: functions only, each with its evidence and byte check).
//! The owner's record wins for an original both have; a disagreement is printed and listed in the output under
//! `merge.disagreements`, so it gets looked at instead of silently decided. A record whose native symbol the kernel
//! does not define is left out and listed (`merge.no_symbol`). EXTRA files are taken in the order given.
//! ```

use crate::image::{hex, unhex, Image};
use anyhow::{anyhow, Context, Result};
use oneios_common::pyjson::{self, Value};
use std::collections::{HashMap, HashSet};

#[derive(clap::Args, Debug)]
pub struct Args {
    #[arg(long)]
    reviewed: String,
    #[arg(long)]
    native: String,
    #[arg(long)]
    out: String,
    extra: Vec<String>,
}

fn text<'v>(record: &'v Value, key: &str) -> Result<&'v str> {
    record.get(key).and_then(Value::as_str).ok_or_else(|| anyhow!("a reviewed record has no \"{key}\""))
}

pub fn run(args: Args) -> Result<()> {
    let mut merged = pyjson::load(&args.reviewed)?;
    let native = oneios_common::Map::open(&args.native)?;
    let defined: HashSet<String> = Image::new(&native, 0)?.symtab()?.into_iter().map(|row| row.0).collect();
    let mut have: HashMap<String, String> = HashMap::new();
    for record in merged.get("functions").and_then(Value::as_list).ok_or_else(|| anyhow!("{}: no functions", args.reviewed))? {
        have.insert(text(record, "original")?.to_string(), text(record, "native_symbol")?.to_string());
    }
    let (mut added, mut disagreements, mut missing, mut new) = (Value::dict(), Vec::new(), Vec::new(), Vec::new());
    for path in &args.extra {
        let source = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut count = 0;
        let extra = pyjson::load(path)?;
        for record in extra.get("functions").and_then(Value::as_list).unwrap_or(&[]) {
            let original = hex(unhex(text(record, "original")?)?);
            let symbol = text(record, "native_symbol")?;
            if let Some(kept) = have.get(&original) {
                if kept != symbol {
                    let mut row = Value::dict();
                    row.set("original", &original);
                    row.set("kept", kept);
                    row.set("other", symbol);
                    row.set("from", &source);
                    disagreements.push(row);
                }
                continue;
            }
            if !defined.contains(symbol) {
                let mut row = Value::dict();
                row.set("original", &original);
                row.set("native_symbol", symbol);
                row.set("from", &source);
                missing.push(row);
                continue;
            }
            let mut copy = record.clone();
            copy.set("original", &original);
            copy.set("source", &source);
            new.push(copy);
            have.insert(original, symbol.to_string());
            count += 1;
        }
        added.set(source, Value::Int(count));
    }
    let Some(Value::List(functions)) = merged.get_mut("functions") else { unreachable!() };
    functions.extend(new);
    let counts: Vec<(String, i128)> = added.as_dict().unwrap().iter().map(|(k, v)| (k.clone(), v.as_int().unwrap())).collect();
    println!(
        "merge-reviewed: {} record(s) added from {} file(s) ({}); {} disagreement(s), {} without a native symbol",
        counts.iter().map(|c| c.1).sum::<i128>(),
        args.extra.len(),
        counts.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join(", "),
        disagreements.len(),
        missing.len()
    );
    for item in &disagreements {
        println!("  disagree {}: kept {}, {} has {}", text(item, "original")?, text(item, "kept")?, text(item, "from")?, text(item, "other")?);
    }
    let mut merge = Value::dict();
    merge.set("added", added);
    merge.set("disagreements", Value::List(disagreements));
    merge.set("no_symbol", Value::List(missing));
    merged.set("merge", merge);
    std::fs::write(&args.out, pyjson::dumps(&merged, Some(1), false) + "\n").with_context(|| args.out.clone())?;
    Ok(())
}
