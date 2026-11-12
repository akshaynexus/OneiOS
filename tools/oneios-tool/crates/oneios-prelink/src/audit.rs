//! `prelink audit`: what every kext import of kernel.kc is bound to, checked against the known-good pairs and the
//! stub list. A port of kernel/prelink/audit.py (its header has the rules): the same report, draft and stdout,
//! and the same exit status (1 when a check fails).

use crate::image::{hex, scan, signed, u32_at, unhex, Image, Symbols};
use crate::pyset::PySet;
use crate::records::{repr_counts, text, truthy, Counter};
use anyhow::{bail, Context, Result};
use indexmap::IndexMap;
use oneios_common::pyjson::{self, Value};
use rayon::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet};

const BTI: [u32; 4] = [0xd503_245f, 0xd503_249f, 0xd503_24df, 0xd503_241f];
const RET: [u32; 3] = [0xd65f_03c0, 0xd65f_0bff, 0xd65f_0fff];
const STUB: &str = "_unixnu_fileset_unmapped";
const KERNEL: &str = "com.apple.kernel";

#[derive(clap::Args, Debug)]
pub struct Args {
    #[arg(long)]
    xnuprelink: String,
    #[arg(long)]
    kernelcache: String,
    #[arg(long)]
    collection: String,
    #[arg(long)]
    pairs: Option<String>,
    #[arg(long)]
    stubs: Option<String>,
    #[arg(long)]
    reviewed: Option<String>,
    #[arg(long)]
    against: Option<String>,
    #[arg(long)]
    report: Option<String>,
    #[arg(long)]
    draft: Option<String>,
}

/// The instructions of a function that only computes registers and returns (BTI left out), else None.
fn leaf(image: &Image, va: u64) -> Option<Vec<u32>> {
    let raw = image.read(va, 48)?;
    let mut body = Vec::new();
    for i in 0..12 {
        let word = u32_at(raw, i * 4);
        if BTI.contains(&word) {
            continue;
        }
        if RET.contains(&word) {
            body.push(0xd65f_03c0);
            return Some(body);
        }
        // data processing on registers only; not ADR/ADRP (an address, which differs between kernels)
        if !matches!((word >> 25) & 0xf, 0b1000 | 0b1001 | 0b0101 | 0b1101) || word & 0x1f00_0000 == 0x1000_0000 {
            return None;
        }
        body.push(word);
        if body.len() > 8 {
            return None;
        }
    }
    None
}

struct Original<'a> {
    raw: &'a [u8],
    sha256: String,
    top: Image<'a>,
    kernel: Image<'a>,
    /// site -> (target, owner), in fixup order
    sites: IndexMap<u64, (u64, String)>,
}

impl<'a> Original<'a> {
    fn new(raw: &'a [u8]) -> Result<Original<'a>> {
        let top = Image::new(raw, 0)?;
        let kernel = top.entry(KERNEL)?;
        let ranges = kernel.mapped();
        let inside = |a: u64| ranges.iter().any(|(b, e)| *b <= a && a < *e);
        let mut owners = Vec::new();
        for (name, (_, at)) in &top.entries {
            if name != KERNEL {
                for s in &Image::new(raw, *at as usize)?.segments {
                    if s.size != 0 && s.name != "__LINKEDIT" {
                        owners.push((s.va, s.va + s.size, name.clone()));
                    }
                }
            }
        }
        owners.sort();
        let mut sites = IndexMap::new();
        for (site, fix) in top.fixups()? {
            if inside(fix.target) && !inside(site) {
                let i = owners.partition_point(|o| o.0 <= site);
                if i > 0 && site < owners[i - 1].1 {
                    sites.insert(site, (fix.target, owners[i - 1].2.clone()));
                }
            }
        }
        Ok(Original { raw, sha256: oneios_common::sha256_hex(raw), top, kernel, sites })
    }
    fn is_function(&self, target: u64) -> bool {
        self.kernel.section(target).is_some_and(|s| s.segment == "__TEXT_EXEC")
    }
}

/// kernel.kc: each original import site's bound target, named.
struct Collection<'a> {
    sha256: String,
    kernel: Image<'a>,
    symbols: Symbols,
    by_name: HashMap<String, u64>,
    stub: Option<u64>,
    /// site -> ("stub" | "unpaired" | "other" | name, address)
    bound: IndexMap<u64, (String, u64)>,
}

impl<'a> Collection<'a> {
    fn new(raw: &'a [u8], original: &Original) -> Result<Collection<'a>> {
        let top = Image::new(raw, 0)?;
        let kernel = top.entry(KERNEL)?;
        let symbols = kernel.symbols()?;
        let mut addresses: Vec<u64> = symbols.keys().copied().collect();
        addresses.sort_unstable();
        let by_name = kernel.names_by_definition()?;
        let stub = by_name.get(STUB).copied();
        let slide = top.base.wrapping_sub(original.top.base);
        let mapped = kernel.mapped();
        let fixups = top.fixups()?;
        let name = |value: u64| -> String {
            let i = addresses.partition_point(|a| *a <= value);
            if i == 0 {
                return hex(value);
            }
            let at = addresses[i - 1];
            let names = symbols[&at].iter().map(String::as_str).collect::<BTreeSet<&str>>().into_iter().collect::<Vec<_>>().join("|");
            if at == value {
                names
            } else {
                format!("{names}+{:#x}", value - at)
            }
        };
        let mut bound = IndexMap::new();
        for (site, (target, _)) in &original.sites {
            let Some(found) = fixups.get(&site.wrapping_add(slide)) else { continue }; // a kext this collection left out
            let value = found.target;
            let what = if Some(value) == stub {
                "stub".to_string()
            } else if value == target.wrapping_add(slide) {
                "unpaired".to_string()
            } else if mapped.iter().any(|(b, e)| *b <= value && value < *e) {
                name(value)
            } else {
                "other".to_string()
            };
            bound.insert(*site, (what, value));
        }
        Ok(Collection { sha256: oneios_common::sha256_hex(raw), kernel, symbols, by_name, stub, bound })
    }
    fn names(&self, value: u64) -> HashSet<&str> {
        self.symbols.get(&value).map(|n| n.iter().map(String::as_str).collect()).unwrap_or_default()
    }
}

#[derive(Default)]
struct Target {
    bound: BTreeSet<String>,
    address: Vec<u64>,
    kexts: BTreeSet<String>,
    sites: Vec<u64>,
}

impl Target {
    /// Python's `next(iter(entry['address']))`.
    fn first_address(&self) -> u64 {
        PySet::from_adds(self.address.iter().copied()).iter().next().unwrap()
    }
}

fn targets(original: &Original, collection: &Collection, skip_sites: &IndexMap<u64, String>) -> IndexMap<u64, Target> {
    let mut found: IndexMap<u64, Target> = IndexMap::new();
    for (site, (bound, value)) in &collection.bound {
        if skip_sites.contains_key(site) {
            continue;
        }
        let (target, owner) = &original.sites[site];
        let entry = found.entry(*target).or_default();
        entry.bound.insert(bound.clone());
        if !entry.address.contains(value) {
            entry.address.push(*value);
        }
        entry.kexts.insert(owner.clone());
        entry.sites.push(*site);
    }
    found
}

/// Per site: the kext code that calls through it (a branch to the stub that loads the slot, or a load of it).
fn callers(original: &Original, target_sites: &[u64]) -> Result<HashMap<u64, i128>> {
    let mut by_kext: IndexMap<&str, HashSet<u64>> = IndexMap::new();
    for site in target_sites {
        by_kext.entry(original.sites[site].1.as_str()).or_default().insert(*site);
    }
    let by_kext: Vec<(&str, HashSet<u64>)> = by_kext.into_iter().collect();
    let per_kext: Vec<Vec<(u64, i128)>> = by_kext
        .par_iter()
        .map(|(kext, sites)| -> Result<Vec<(u64, i128)>> {
            let image = original.top.entry(kext)?;
            let mut stubs: HashMap<u64, u64> = HashMap::new();
            for section in &image.sections {
                if section.name == "__auth_stubs" || section.name == "__stubs" {
                    for r in scan(image.read_of(section.va, section.size)?, section.va).refs {
                        if sites.contains(&r.target) {
                            stubs.insert(r.site - (r.site - section.va) % 16, r.target);
                        }
                    }
                }
            }
            let mut counts: HashMap<u64, i128> = HashMap::new();
            for section in &image.sections {
                if section.segment != "__TEXT_EXEC" || section.name != "__text" {
                    continue;
                }
                let code = image.read_of(section.va, section.size)?;
                for i in (0..code.len().saturating_sub(3)).step_by(4) {
                    let word = u32_at(code, i);
                    if word & 0x7c00_0000 == 0x1400_0000 {
                        let t = (section.va + i as u64).wrapping_add((signed((word & 0x3ff_ffff) as u64, 26) * 4) as u64);
                        if let Some(site) = stubs.get(&t) {
                            *counts.entry(*site).or_default() += 1;
                        }
                    }
                }
                for r in scan(code, section.va).refs {
                    if sites.contains(&r.target) {
                        *counts.entry(r.target).or_default() += 1;
                    }
                }
            }
            Ok(sites.iter().map(|site| (*site, counts.get(site).copied().unwrap_or(0))).collect())
        })
        .collect::<Result<_>>()?;
    Ok(per_kext.into_iter().flatten().collect())
}

fn repr_list(items: &[&str]) -> String {
    format!("[{}]", items.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", "))
}

fn paired(bound: &str) -> bool {
    !matches!(bound, "stub" | "unpaired" | "other")
}

/// Returns whether a check failed (the exit status 1).
pub fn run(args: Args) -> Result<bool> {
    let _ = &args.xnuprelink;
    let raw = oneios_common::Map::open(&args.kernelcache)?;
    let original = Original::new(&raw)?;
    let collection_raw = oneios_common::Map::open(&args.collection)?;
    let collection = Collection::new(&collection_raw, &original)?;
    if collection.stub.is_none() {
        bail!("{} has no {STUB}", args.collection);
    }
    let mut failures: Vec<String> = Vec::new();
    let mut reviewed_sites: IndexMap<u64, String> = IndexMap::new();
    if let Some(path) = &args.reviewed {
        for record in pyjson::load(path)?.get("sites").and_then(Value::as_list).unwrap_or(&[]) {
            reviewed_sites.insert(unhex(text(record, "site")?)?, text(record, "native_symbol")?.to_string());
        }
    }
    for (site, symbol) in &reviewed_sites {
        if let Some((bound, value)) = collection.bound.get(site) {
            if !collection.names(*value).contains(symbol.as_str()) {
                failures.push(format!("site {site:#x}: reviewed {symbol}, bound to {bound}"));
            }
        }
    }
    let found = targets(&original, &collection, &reviewed_sites);
    let pairs = args.pairs.as_ref().map(pyjson::load).transpose()?;
    let stubs = args.stubs.as_ref().map(pyjson::load).transpose()?;
    if let Some(pairs) = &pairs {
        if truthy(Some(pairs)) && pairs.get("kernelcache_sha256").and_then(Value::as_str) != Some(&original.sha256) {
            bail!("the pairs list is for another kernelcache");
        }
    }
    let mut known: HashMap<u64, &Value> = HashMap::new();
    let mut stubbed: HashSet<u64> = HashSet::new();
    for kind in ["functions", "data"] {
        for entry in pairs.as_ref().and_then(|p| p.get(kind)).and_then(Value::as_list).unwrap_or(&[]) {
            known.insert(unhex(text(entry, "original")?)?, entry);
        }
        for entry in stubs.as_ref().and_then(|s| s.get(kind)).and_then(Value::as_list).unwrap_or(&[]) {
            stubbed.insert(unhex(text(entry, "original")?)?);
        }
    }
    let stub_sites: Vec<u64> = found.values().filter(|e| e.bound.contains("stub") || e.bound.contains("unpaired")).flat_map(|e| e.sites.iter().copied()).collect();
    let calls = callers(&original, &stub_sites)?;
    let mut counts = Counter::default();
    let (mut functions, mut data) = (Vec::new(), Vec::new());
    let mut sorted: Vec<u64> = found.keys().copied().collect();
    sorted.sort_unstable();
    for target in sorted {
        let entry = &found[&target];
        let kind = if original.is_function(target) { "functions" } else { "data" };
        let bound: Vec<&str> = entry.bound.iter().map(String::as_str).collect();
        let kexts: Vec<&str> = entry.kexts.iter().map(String::as_str).collect();
        let mut row = Value::dict();
        row.set("bound", if bound.len() > 1 { Value::from(bound.iter().map(|b| b.to_string()).collect::<Vec<_>>()) } else { Value::str(bound[0]) });
        row.set("kexts", kexts.iter().map(|k| k.to_string()).collect::<Vec<_>>());
        let place = format!(
            "{target:#x} ({}, {} kexts: {})",
            if kind == "functions" { "function" } else { "data" },
            kexts.len(),
            kexts.iter().take(4).map(|k| k.replace("com.apple.", "")).collect::<Vec<_>>().join(", ")
        );
        let mut check = || {
            if bound.len() > 1 {
                failures.push(format!("{place}: bound to several targets {}", repr_list(&bound)));
                return;
            }
            let bound = bound[0];
            if bound == "stub" || bound == "unpaired" {
                let callers: i128 = entry.sites.iter().map(|s| calls.get(s).copied().unwrap_or(0)).sum();
                row.set("callers", Value::Int(callers));
                counts.add(bound, 1);
                if let Some(want) = known.get(&target) {
                    let native = want.get("native").and_then(Value::as_str).unwrap_or("");
                    if truthy(want.get("absent")) && bound == "stub" && !collection.by_name.contains_key(native.split('+').next().unwrap()) {
                        counts.add("absent", 1); // this kernel has no such function (emit, "absent")
                        row.set("absent", want.get("absent").cloned().unwrap());
                        return;
                    }
                }
                if stubs.is_some() && !stubbed.contains(&target) {
                    failures.push(format!("{place}: {bound} without a reason in the stub list; {callers} call sites in the kexts"));
                }
                return;
            }
            if bound == "other" {
                failures.push(format!("{place}: bound outside our kernel"));
                return;
            }
            counts.add("paired", 1);
            if stubs.is_some() && stubbed.contains(&target) {
                failures.push(format!("{place}: the stub list has it, but it is bound to {bound}: review it into the pairs"));
                return;
            }
            if pairs.is_none() {
                return;
            }
            let Some(want) = known.get(&target) else {
                failures.push(format!("{place}: new pair {bound}, not in the known-good list"));
                return;
            };
            let address = entry.first_address();
            let native = want.get("native").and_then(Value::as_str).unwrap_or("");
            let (name, offset) = native.split_once('+').unwrap_or((native, ""));
            let offset = if offset.is_empty() { 0 } else { unhex(offset).unwrap_or(0) };
            let mut ok = collection.by_name.get(name).copied() == Some(address.wrapping_sub(offset));
            if !ok && truthy(want.get("leaf")) && kind == "functions" {
                let mine = leaf(&collection.kernel, address);
                ok = mine.is_some() && mine == leaf(&original.kernel, target);
            }
            if !ok {
                failures.push(format!("{place}: bound to {bound}, the known-good list says {native}"));
            }
        };
        check();
        if kind == "functions" { &mut functions } else { &mut data }.push((hex(target), row));
    }
    let mut report = Value::dict();
    report.set("collection_sha256", &collection.sha256);
    report.set("kernelcache_sha256", &original.sha256);
    report.set("functions", Value::Dict(functions));
    report.set("data", Value::Dict(data));
    report.set("counts", counts.to_value());
    if let Some(path) = &args.against {
        let other_raw = oneios_common::Map::open(path)?;
        let other = Collection::new(&other_raw, &original)?;
        let theirs = targets(&original, &other, &reviewed_sites);
        let mut diff: IndexMap<&str, Vec<Value>> = IndexMap::new();
        for (target, entry) in &found {
            let Some(there) = theirs.get(target) else { continue };
            let (mine, them) = (entry.bound.iter().next().unwrap().as_str(), there.bound.iter().next().unwrap().as_str());
            let key = if paired(them) && (mine == "stub" || mine == "unpaired") {
                "a"
            } else if (them == "stub" || them == "unpaired") && paired(mine) {
                "b"
            } else if paired(them) && paired(mine) {
                let (a, b) = (other.names(there.first_address()), collection.names(entry.first_address()));
                let same_body = original.is_function(*target) && leaf(&original.kernel, *target).is_some() && leaf(&other.kernel, there.first_address()) == leaf(&collection.kernel, entry.first_address());
                if a.is_disjoint(&b) && mine != them {
                    if same_body { "c-leaf" } else { "c" }
                } else {
                    continue;
                }
            } else {
                continue;
            };
            let mut row = Value::dict();
            row.set("original", hex(*target));
            row.set("there", them);
            row.set("here", mine);
            diff.entry(key).or_default().push(row);
        }
        let mut lens: Vec<(&str, i128)> = diff.iter().map(|(k, v)| (*k, v.len() as i128)).collect();
        lens.sort();
        println!("against {path} {}", repr_counts(lens.into_iter()));
        let mut against = Value::dict();
        against.set("collection_sha256", &other.sha256);
        for (k, v) in diff {
            against.set(k, Value::List(v));
        }
        report.set("against", against);
    }
    if let Some(path) = &args.draft {
        let mut draft = Value::dict();
        draft.set("kernelcache_sha256", &original.sha256);
        for kind in ["functions", "data"] {
            let mut rows = Vec::new();
            for (target, row) in report.get(kind).and_then(Value::as_dict).unwrap() {
                if let Some(bound) = row.get("bound").and_then(Value::as_str).filter(|b| paired(b)) {
                    let mut entry = Value::dict();
                    entry.set("original", target);
                    entry.set("native", bound.split('|').next().unwrap());
                    entry.set("why", "draft");
                    rows.push(entry);
                }
            }
            draft.set(kind, Value::List(rows));
        }
        std::fs::write(path, pyjson::dumps(&draft, Some(0), false) + "\n").with_context(|| path.clone())?;
    }
    report.set("failures", failures.clone());
    if let Some(path) = &args.report {
        std::fs::write(path, pyjson::dumps(&report, Some(1), false) + "\n").with_context(|| path.clone())?;
    }
    println!(
        "audit: {}: {} paired, {} functions on the stub, {} data unpaired; {} failures",
        &collection.sha256[..16],
        counts.get("paired"),
        counts.get("stub"),
        counts.get("unpaired"),
        failures.len()
    );
    for line in failures.iter().take(60) {
        println!("  FAIL {line}");
    }
    if failures.len() > 60 {
        println!("  ... {} more (the report lists them all)", failures.len() - 60);
    }
    Ok(!failures.is_empty())
}
