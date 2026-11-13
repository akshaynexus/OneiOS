//! `oneios-tool prelink lead-imports [--add] [--kext ID]` (was kernel/prelink/lead/imports.py, retired
//! 2026-11-13): pair unknown kernel imports of the kexts by the order of each kext's import table.
//!
//! A kext's __auth_got (the slots its import stubs load) is in the alphabetical order of the imported symbols.
//! Every slot whose target is already paired gives a name; an unpaired target between two named slots must be a
//! symbol between those names. The bounds from every kext that imports the same target are intersected, and the
//! kernel's exported functions between them that no other original is paired with are the candidates.
//!
//! Prints: how well the known names obey the order (pairs that break it are suspects), and for each unpaired
//! target its bounds and candidates. --add writes the targets with exactly one candidate to
//! kernel/prelink/lead/lead-reviewed.json. The files it reads: lead.rs.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{Context, Result};
use oneios_common::pyjson::Value;

use crate::image::{u64_at, Image};
use crate::lead::{function_record, original, text, Lead};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Write the targets with exactly one candidate to the lead's reviewed records
    #[arg(long)]
    add: bool,
    /// Only this kext's import tables
    #[arg(long)]
    kext: Option<String>,
    /// This repository's checkout (default: the current directory)
    #[arg(long)]
    repo: Option<String>,
}

/// The name above every name: an unbounded upper end.
const TOP: &str = "\x7f";

/// What is known about the original kernel's addresses: the native name of each, and where the name came from.
#[derive(Default)]
struct Known {
    name_of: HashMap<u64, String>,
    /// The matcher's pairs without a name (strings and other data it paired by their references).
    unnamed: HashSet<u64>,
    how_of: HashMap<u64, String>,
}

impl Known {
    fn set(&mut self, address: u64, name: &str, how: String) {
        self.name_of.insert(address, name.to_string());
        self.unnamed.remove(&address);
        self.how_of.insert(address, how);
    }

    /// Records with "original" and "native_symbol" (a data record may have neither).
    fn set_records(&mut self, records: &[Value], how: &str, all: bool) -> Result<()> {
        for d in records.iter().filter(|d| all || (d.get("original").is_some() && d.get("native_symbol").is_some())) {
            self.set(original(d)?, text(d, "native_symbol")?, how.to_string());
        }
        Ok(())
    }

    /// In the scripts' order: the matcher's functions and data, what the emitter applied, the reviewed records.
    fn read(lead: &Lead) -> Result<Known> {
        let mut known = Known::default();
        for (kind, prefix) in [("functions", ""), ("data", "data:")] {
            for (address, pair) in lead.map.get(kind).and_then(Value::as_dict).with_context(|| format!("match.map.json: no {kind}"))? {
                let address = crate::image::unhex(address)?;
                match pair.get("name").and_then(Value::as_str) {
                    Some(name) => known.set(address, name, format!("{prefix}{}", text(pair, "how")?)),
                    None => {
                        known.name_of.remove(&address);
                        known.unnamed.insert(address);
                    }
                }
            }
        }
        let applied = lead.report.get("reviewed_applied").context("kernel.kc.prelink.json: no reviewed_applied")?;
        let group = |key: &str| applied.get(key).and_then(Value::as_list).unwrap_or(&[]);
        for key in ["aligned", "functions"] {
            known.set_records(group(key), &format!("applied:{key}"), true)?;
        }
        known.set_records(group("data_inferred"), "data", false)?;
        known.set_records(group("data"), "data", false)?;
        for f in lead.reviewed_functions()? {
            known.set(original(f)?, text(f, "native_symbol")?, "reviewed".to_string());
        }
        known.set_records(lead.reviewed.get("data").and_then(Value::as_list).unwrap_or(&[]), "reviewed-data", false)?;
        Ok(known)
    }
}

/// The native kernel's symbols as llvm-nm lists them.
struct Native {
    /// The exported symbols (what a kext can import), in strcmp order.
    exported: Vec<String>,
    /// Every symbol's address (the first of a name).
    at: HashMap<String, u64>,
    by_addr: HashMap<u64, Vec<String>>,
}

impl Native {
    fn read(lead: &Lead) -> Result<Native> {
        let keep = |rows: Vec<(u64, String, String)>| rows.into_iter().filter(|(_, _, name)| !name.ends_with(".original"));
        let exported: BTreeSet<String> = keep(lead.nm(&["-g", "--defined-only"])?).map(|(_, _, name)| name).collect();
        let (mut at, mut by_addr) = (HashMap::new(), HashMap::<u64, Vec<String>>::new());
        for (address, _, name) in keep(lead.nm(&["-n"])?) {
            if !at.contains_key(&name) {
                at.insert(name.clone(), address);
                by_addr.entry(address).or_default().push(name);
            }
        }
        Ok(Native { exported: exported.into_iter().collect(), at, by_addr })
    }

    /// Every name of the address a name has.
    fn aliases<'a>(&'a self, name: &'a str) -> Vec<&'a str> {
        match self.at.get(name).and_then(|a| self.by_addr.get(a)) {
            Some(names) => names.iter().map(String::as_str).collect(),
            None => vec![name],
        }
    }
}

/// One kext's import slots of one section: each slot's target in the kernel, or None.
fn slots(raw: &[u8], kext: &Image, base: u64, kernel_ranges: &[(u64, u64)], va: u64, size: u64) -> Result<Vec<Option<u64>>> {
    (0..size / 8)
        .map(|i| {
            let target = base + (u64_at(raw, kext.offset_of(va + 8 * i, 8)?) & 0x3fff_ffff);
            Ok(Some(target).filter(|t| kernel_ranges.iter().any(|(a, b)| a <= t && t < b)))
        })
        .collect()
}

/// An unpaired target's bounds: the greatest name below it and the least above it over every kext, and the kexts.
struct Bounds {
    lower: String,
    upper: String,
    kexts: Vec<String>,
}

/// A pair of adjacent known names that are out of order: (kext, section, the two names, their origins and targets).
type Violation = (String, String, String, String, Option<String>, Option<String>, u64, u64);

/// The walk over every kext's import tables.
#[derive(Default)]
struct Walk {
    bounds: BTreeMap<u64, Bounds>,
    violations: Vec<Violation>,
    checked: u64,
}

impl Walk {
    fn section(&mut self, known: &Known, native: &Native, ident: &str, section: &str, slots: &[Option<u64>]) -> Result<()> {
        // (the script compared such a pair's missing name with a name, and stopped with a TypeError)
        if let Some(target) = slots.iter().flatten().find(|t| known.unnamed.contains(t)) {
            anyhow::bail!("{ident} {section}: the import slot's target {target:#x} is paired without a name");
        }
        let name_at = |i: usize| slots[i].and_then(|t| known.name_of.get(&t));
        let named: Vec<(usize, &String)> = (0..slots.len()).filter_map(|i| name_at(i).map(|n| (i, n))).collect();
        for pair in named.windows(2) {
            let ((i, a), (j, b)) = (pair[0], pair[1]);
            self.checked += 1;
            let in_order = native.aliases(a).iter().any(|x| native.aliases(b).iter().any(|y| x < y));
            if !in_order && slots[i] != slots[j] {
                let how = |k: usize| slots[k].and_then(|t| known.how_of.get(&t).cloned());
                self.violations.push((ident.to_string(), section.to_string(), a.clone(), b.clone(), how(i), how(j), slots[i].unwrap(), slots[j].unwrap()));
            }
        }
        for (i, target) in slots.iter().enumerate() {
            let Some(target) = target.filter(|t| !known.name_of.contains_key(t)) else { continue };
            let lower = (0..i).rev().find_map(name_at).map_or("", String::as_str);
            let upper = (i + 1..slots.len()).find_map(name_at).map_or(TOP, String::as_str);
            let entry = self.bounds.entry(target).or_insert_with(|| Bounds { lower: String::new(), upper: TOP.to_string(), kexts: Vec::new() });
            if lower > entry.lower.as_str() {
                entry.lower = lower.to_string();
            }
            if upper < entry.upper.as_str() {
                entry.upper = upper.to_string();
            }
            entry.kexts.push(ident.rsplit('.').next().unwrap_or(ident).to_string());
        }
        Ok(())
    }
}

/// The native kernel's place for an unpaired original function: between the native partners of the nearest
/// paired functions around it. (begin, end, the name below, the name above)
struct Window<'a> {
    known: &'a Known,
    native: &'a Native,
    starts: &'a [u64],
    paired_starts: Vec<u64>,
}

impl Window<'_> {
    fn around(&self, t: u64) -> Option<(u64, u64, &str, &str)> {
        const REACH: usize = 24;
        const SPAN: u64 = 0x8000;
        let i = self.paired_starts.partition_point(|s| *s < t);
        let index = |address: u64| self.starts.partition_point(|s| *s < address);
        let mut best: Option<(u64, u64, &str, &str)> = None;
        for &lo in self.paired_starts[i.saturating_sub(3)..i].iter().rev() {
            for &hi in &self.paired_starts[i..(i + 3).min(self.paired_starts.len())] {
                if index(hi) as i64 - index(lo) as i64 > REACH as i64 {
                    continue;
                }
                let (lo_name, hi_name) = (&self.known.name_of[&lo], &self.known.name_of[&hi]);
                let (a, b) = (self.native.at[lo_name], self.native.at[hi_name]);
                if a < b && b - a <= SPAN && best.is_none_or(|(x, y, ..)| b - a < y - x) {
                    best = Some((a, b, lo_name, hi_name));
                }
            }
        }
        best
    }
}

pub fn run(args: Args) -> Result<()> {
    let mut lead = Lead::open(args.repo.as_deref())?;
    let raw = oneios_common::Map::open(&lead.kernelcache)?;
    let cache = Image::new(&raw, 0)?;
    let entry = text(&lead.reviewed, "entry")?.to_string();
    let kernel = cache.entry(&entry)?;
    let mut starts = kernel.function_starts()?;
    starts.sort_unstable();
    let start_set: HashSet<u64> = starts.iter().copied().collect();
    let kernel_ranges: Vec<(u64, u64)> = kernel.segments.iter().filter(|s| s.size != 0).map(|s| (s.va, s.va + s.size)).collect();
    let known = Known::read(&lead)?;
    let mut taken: HashSet<String> = known.name_of.values().cloned().collect();
    let native = Native::read(&lead)?;
    let mut paired_starts: Vec<u64> = known.name_of.iter().filter(|(a, name)| start_set.contains(a) && native.at.contains_key(*name)).map(|(a, _)| *a).collect();
    paired_starts.sort_unstable();
    let window = Window { known: &known, native: &native, starts: &starts, paired_starts };

    let mut walk = Walk::default();
    let mut idents: Vec<(&String, u64)> = cache.entries.iter().map(|(ident, (_, fileoff))| (ident, *fileoff)).collect();
    idents.sort();
    for (ident, fileoff) in idents {
        if *ident == entry || args.kext.as_ref().is_some_and(|only| only != ident) {
            continue;
        }
        let kext = Image::new(&raw, fileoff as usize)?;
        for sect in kext.sections.iter().filter(|s| (s.name == "__auth_got" || s.name == "__got") && s.size != 0) {
            walk.section(&known, &native, ident, &sect.name, &slots(&raw, &kext, cache.base, &kernel_ranges, sect.va, sect.size)?)?;
        }
    }

    println!("order of known names: {} adjacent pairs checked, {} out of order", walk.checked, walk.violations.len());
    let or_none = |how: &Option<String>| how.clone().unwrap_or_else(|| "None".to_string());
    for (ident, section, a, b, how_a, how_b, at_a, at_b) in &walk.violations {
        println!("  SUSPECT in {ident} {section}: {a} ({}, {at_a:#x}) before {b} ({}, {at_b:#x})", or_none(how_a), or_none(how_b));
    }
    let (mut unique, mut added) = (0, 0);
    for (&t, Bounds { lower, upper, kexts }) in &walk.bounds {
        let lo = native.exported.partition_point(|n| n <= lower);
        let hi = native.exported.partition_point(|n| n < upper).max(lo);
        let mut candidates: Vec<&String> = native.exported[lo..hi].iter().filter(|n| !taken.contains(*n)).collect();
        let is_function = start_set.contains(&t);
        let near = if is_function { window.around(t) } else { None };
        let narrowed: Vec<&String> = near.map_or(Vec::new(), |(a, b, ..)| candidates.iter().copied().filter(|n| native.at.get(*n).is_some_and(|at| a < *at && *at < b)).collect());
        let mut how = String::new();
        if candidates.len() != 1 && narrowed.len() == 1 {
            candidates = narrowed;
            let (_, _, below, above) = near.unwrap();
            how = format!(" It is also the only one of them that our kernel places between {below} and {above}, the paired functions on either side of this one in the original.");
        }
        unique += (candidates.len() == 1) as u64;
        if !args.add || candidates.len() <= 6 {
            let names: Vec<&str> = candidates.iter().map(|n| n.as_str()).collect();
            let shown = if candidates.len() <= 6 { names.join(", ") } else { format!("{} candidates", candidates.len()) };
            println!("{t:#x} {} {:3} kexts  ({lower} .. {upper}): {shown}", if is_function { "fn  " } else { "data" }, kexts.len());
        }
        if args.add && candidates.len() == 1 && is_function && !lower.is_empty() && upper != TOP {
            let candidate = candidates[0].clone();
            let size = starts[starts.partition_point(|s| *s < t) + 1] - t;
            let importers: Vec<&str> = kexts.iter().map(String::as_str).collect::<BTreeSet<_>>().into_iter().take(6).collect();
            let evidence = format!(
                "By the order of the kexts' import tables (__auth_got is in the alphabetical order of the imported symbols): in the {} kext(s) that import this function ({}) its slot lies between the slots of {lower} and {upper}, and {candidate} is the only exported kernel function between those names that no other original function is paired with.{how}",
                kexts.len(),
                importers.join(", ")
            );
            lead.add(function_record(t, &candidate, kernel.read_of(t, size)?, evidence));
            taken.insert(candidate);
            added += 1;
        }
    }
    let tail = if args.add { format!(", {added} added") } else { String::new() };
    println!("{} unpaired imported targets, {unique} with exactly one candidate{tail}", walk.bounds.len());
    if args.add {
        lead.save()?;
    }
    Ok(())
}
