//! `prelink kclasses`: the original kernel's class objects (metaclass objects, the metaClass and superClass
//! pointers, vtables) named by the names their static initializers register. A port of
//! kernel/prelink/kclasses.py (its header has the method): the same kclasses.json byte for byte.

use crate::image::{hex, scan, u32_at, Image, RefKind};
use crate::records::{self, repr_counts};
use anyhow::{Context, Result};
use indexmap::IndexMap;
use oneios_common::pyjson::{self, Value};
use std::collections::{BTreeMap, HashMap};

const RET: u32 = 0xd65f_03c0;
const BTI_C: u32 = 0xd503_245f;

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
}

/// name -> how many symbols of the kernel have it.
pub fn name_counts(native: &Image) -> Result<HashMap<String, u32>> {
    let mut counts = HashMap::new();
    for names in native.symbols()?.into_values() {
        for name in names {
            *counts.entry(name).or_default() += 1;
        }
    }
    Ok(counts)
}

fn identifier(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty() && (b[0].is_ascii_alphabetic() || b[0] == b'_') && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

pub fn run(args: Args) -> Result<()> {
    let _ = &args.xnuprelink;
    let raw = oneios_common::Map::open(&args.kernelcache)?;
    let pairing = pyjson::load(&args.map)?;
    let reviewed = pyjson::load(&args.reviewed)?;
    let cache = Image::new(&raw, 0)?;
    let kernel = cache.entry("com.apple.kernel")?;
    let fixups = cache.fixups()?;
    let native_raw = oneios_common::Map::open(&args.native)?;
    let native = name_counts(&Image::new(&native_raw, 0)?)?;
    let mut known: HashMap<u64, String> = records::named(&pairing, "data")?.into_iter().collect();
    known.extend(records::pairs(&reviewed, "data")?);
    let word = |address: u64| -> Result<u32> { Ok(u32_at(&raw, cache.offset_of(address, 4)?)) };
    // metaclass object -> (name, parent metaclass object); function -> the metaclass object it returns
    let (mut classes, mut getters): (BTreeMap<u64, (String, Option<u64>)>, HashMap<u64, u64>) = (BTreeMap::new(), HashMap::new());
    for segment in &kernel.segments {
        if segment.name != "__TEXT_EXEC" || segment.filesize == 0 {
            continue;
        }
        let blob = &raw[segment.fileoff as usize..(segment.fileoff + segment.filesize) as usize];
        let refs = scan(blob, segment.va).refs;
        let by_site: HashMap<u64, (u64, RefKind)> = refs.iter().map(|r| (r.site, (r.target, r.kind))).collect();
        let add_to = |site: u64, register: u32| -> Result<Option<u64>> {
            match by_site.get(&site) {
                Some((target, RefKind::Add)) if word(site)? & 31 == register => Ok(Some(*target)),
                _ => Ok(None),
            }
        };
        for r in &refs {
            if r.kind != RefKind::Add {
                continue;
            }
            let (here, target) = (r.site, r.target);
            let register = word(here)? & 31;
            if register == 0 && word(here + 4)? == RET {
                let start = here - 4;
                getters.insert(if word(start - 4)? == BTI_C { start - 4 } else { start }, target);
            }
            if register != 1 {
                continue;
            }
            let Some(name) = kernel.cstring(target).ok().flatten().filter(|n| identifier(n)) else { continue };
            let (mut meta, mut parent) = (None, None);
            for back in [here - 8, here - 12, here - 16] {
                // Python tests `back in by_site` before it reads the word.
                if by_site.contains_key(&back) {
                    if let Some(found) = add_to(back, 0)? {
                        meta = Some(found);
                        break;
                    }
                }
            }
            for forward in (here + 4..here + 28).step_by(4) {
                if by_site.contains_key(&forward) {
                    if let Some(found) = add_to(forward, 2)? {
                        parent = Some(found);
                    }
                }
                if word(forward)? >> 26 == 0x25 {
                    break;
                }
            }
            if let Some(meta) = meta {
                classes.entry(meta).or_insert((name.to_string(), parent));
            }
        }
    }
    let data_const: Vec<(u64, u64)> = kernel.segments.iter().filter(|s| s.name == "__DATA_CONST").map(|s| (s.va, s.va + s.size)).collect();
    let inside = |a: u64| data_const.iter().any(|(b, e)| *b <= a && a < *e);
    let (mut pointing, mut slot7): (HashMap<u64, Vec<u64>>, HashMap<u64, Vec<u64>>) = (HashMap::new(), HashMap::new());
    for (site, fix) in &fixups {
        if classes.contains_key(&fix.target) && inside(*site) {
            pointing.entry(fix.target).or_default().push(*site);
        }
        if let Some(meta) = getters.get(&fix.target) {
            if inside(*site) {
                let base = site.wrapping_sub(16 + 7 * 8);
                if !fixups.contains_key(&base) && !fixups.contains_key(&(base + 8)) {
                    slot7.entry(*meta).or_default().push(base);
                }
            }
        }
    }
    let (mut pairs, mut conflicts) = (Vec::new(), Vec::new());
    let mut add = |address: u64, symbol: String, why: String| {
        if native.get(&symbol) != Some(&1) {
            return;
        }
        if let Some(paired) = known.get(&address) {
            if *paired != symbol {
                let mut row = Value::dict();
                row.set("original", hex(address));
                row.set("paired", paired);
                row.set("class_says", symbol);
                conflicts.push(row);
            }
            return;
        }
        known.insert(address, symbol.clone());
        let mut row = Value::dict();
        row.set("original", hex(address));
        row.set("native_symbol", symbol);
        row.set("by", "class");
        row.set("why", why);
        pairs.push(row);
    };
    for (meta, (name, parent)) in &classes {
        let prefix = format!("__ZN{}{name}", name.len());
        add(*meta, format!("{prefix}10gMetaClassE"), format!("the static initializer registers this object as \"{name}\""));
        let sites = pointing.get(meta).map_or(&[][..], Vec::as_slice);
        for site in sites {
            if let (Some(parent), Some(after)) = (parent, fixups.get(&(site + 8))) {
                if after.target == *parent {
                    add(*site, format!("{prefix}9metaClassE"), format!("points at {name}'s metaclass object, followed by a pointer to its parent's"));
                    add(site + 8, format!("{prefix}10superClassE"), format!("the pointer after {name}::metaClass, at its parent's metaclass object"));
                }
            }
        }
        for site in sites {
            // Any other constant pointer to the object has the value of X::metaClass: right for every read.
            add(*site, format!("{prefix}9metaClassE"), format!("a constant pointer to {name}'s metaclass object: the value of {name}::metaClass"));
        }
        if let Some(tables) = slot7.get(meta).filter(|t| t.len() == 1) {
            add(tables[0], format!("__ZTV{}", &prefix[4..]), format!("slot 7 is the function that returns {name}'s metaclass object"));
        }
    }
    let mut kinds: IndexMap<String, i128> = IndexMap::new();
    for pair in &pairs {
        let symbol = records::text(pair, "native_symbol")?;
        let key = if symbol.starts_with("__ZTV") {
            "vtable".to_string()
        } else {
            let head = symbol.rsplit_once('E').map_or(symbol, |p| p.0);
            head[head.len().saturating_sub(9)..].to_string()
        };
        *kinds.entry(key).or_default() += 1;
    }
    let mut record = Value::dict();
    record.set("kernelcache_sha256", oneios_common::sha256_hex(&raw));
    record.set("classes", Value::Int(classes.len() as i128));
    let (count, disagree) = (pairs.len(), conflicts.len());
    record.set("data", Value::List(pairs));
    record.set("conflicts", Value::List(conflicts));
    std::fs::write(&args.out, pyjson::dumps(&record, Some(1), false) + "\n").with_context(|| args.out.clone())?;
    println!(
        "kclasses: {} kernel classes; {count} class objects newly named {}; {disagree} disagree with an earlier pair",
        classes.len(),
        repr_counts(kinds.iter().map(|(k, v)| (k.as_str(), *v)))
    );
    Ok(())
}
