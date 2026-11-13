//! `oneios-tool prelink port-fixture` (was kernel/prelink/port-fixture.py, retired 2026-11-13): carry XnuPrelink's
//! reviewed function identities of another kernelcache of the same XNU over to this one.
//!
//! usage: prelink port-fixture --xnuprelink DIR --kernelcache KC --fixture fileset-24A446-symbols.json --reviewed reviewed.json
//!
//! XnuPrelink's fixture for 24A446 is pinned to the iPad kernelcache (kernelcache.release.j820): each reviewed
//! function is recorded with its size and the SHA-256 of its bytes. A function of this kernelcache's kernel with
//! the same size and the same bytes is the same function (a position-dependent one never has the same bytes at
//! another address, so only functions without PC-relative references to moved targets carry over). Exactly one
//! such function must exist; then the record is written into REVIEWED's "functions" with its address here, marked
//! "ported", and replaces any earlier ported record. Hand-written records are kept. Nothing of the fixture's data
//! or sites carries over this way.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Context, Result};
use indexmap::IndexMap;
use oneios_common::pyjson::{self, Value};
use oneios_common::pyrepr::truthy;

use crate::image::Image;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Not read: the kernelcache reader is this crate's (image.rs, the port of XnuPrelink's kc_image.py)
    #[arg(long)]
    xnuprelink: String,
    #[arg(long)]
    kernelcache: String,
    #[arg(long)]
    fixture: String,
    #[arg(long)]
    reviewed: String,
}

/// A reviewed function's identity: (its size, the SHA-256 of its bytes).
type Key = (i128, String);

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value.get(key).ok_or_else(|| anyhow!("port-fixture: a record has no \"{key}\""))
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    field(value, key)?.as_str().ok_or_else(|| anyhow!("port-fixture: \"{key}\" is not a string"))
}

/// The fixture's functions by identity; of two with one identity the first counts.
fn wanted(fixture: &Value) -> Result<IndexMap<Key, &Value>> {
    let mut wanted = IndexMap::new();
    for record in field(fixture, "functions")?.as_list().context("port-fixture: the fixture's functions")? {
        let size = field(record, "original_size")?.as_int().context("port-fixture: original_size")?;
        wanted.entry((size, text(record, "original_sha256")?.to_string())).or_insert(record);
    }
    Ok(wanted)
}

/// Where each wanted identity is in this kernel: every function with that size and those bytes.
fn find(kernel: &Image, wanted: &IndexMap<Key, &Value>) -> Result<HashMap<Key, Vec<u64>>> {
    let mut starts = kernel.function_starts()?;
    starts.sort_unstable();
    let sizes: HashSet<i128> = wanted.keys().map(|k| k.0).collect();
    let mut found: HashMap<Key, Vec<u64>> = HashMap::new();
    for pair in starts.windows(2) {
        let size = pair[1] - pair[0];
        if !sizes.contains(&(size as i128)) {
            continue;
        }
        // (a function whose bytes the file does not hold is none of the fixture's)
        let Some(bytes) = kernel.read(pair[0], size) else { continue };
        let key = (size as i128, oneios_common::sha256_hex(bytes));
        if wanted.contains_key(&key) {
            found.entry(key).or_default().push(pair[0]);
        }
    }
    Ok(found)
}

pub fn run(args: Args) -> Result<()> {
    let raw = oneios_common::Map::open(&args.kernelcache)?;
    let mut reviewed = pyjson::load(&args.reviewed)?;
    if oneios_common::sha256_hex(&raw) != text(&reviewed, "kernelcache_sha256")? {
        bail!("port-fixture: the reviewed file is for another kernelcache");
    }
    let fixture = pyjson::load(&args.fixture)?;
    let kernel = Image::new(&raw, 0)?.entry(text(&reviewed, "entry")?)?;
    let wanted = wanted(&fixture)?;
    let mut found: Vec<(Key, Vec<u64>)> = find(&kernel, &wanted)?.into_iter().collect();
    found.sort_by(|a, b| a.1.cmp(&b.1));
    let kept: Vec<Value> = reviewed.get("functions").and_then(Value::as_list).unwrap_or(&[]).iter().filter(|r| !truthy(r.get("ported"))).cloned().collect();
    let by_hand: HashSet<&str> = kept.iter().map(|r| text(r, "original")).collect::<Result<_>>()?;
    let ipad = text(field(&fixture, "inputs")?, "kernelcache_sha256")?;
    let fixture_name = std::path::Path::new(&args.fixture).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (mut ported, mut ambiguous) = (Vec::new(), 0);
    for (key, addresses) in &found {
        if addresses.len() != 1 {
            ambiguous += 1;
            continue;
        }
        let (record, address) = (wanted[key], format!("{:#x}", addresses[0]));
        if by_hand.contains(address.as_str()) {
            continue;
        }
        let mut check = Value::dict();
        check.set("address", address.as_str());
        check.set("size", key.0);
        check.set("sha256", key.1.as_str());
        let mut entry = Value::dict();
        entry.set("original", address.as_str());
        entry.set("native_symbol", field(record, "native_symbol")?.clone());
        entry.set("ported", true);
        entry.set("optional", true);
        entry.set("check", check);
        entry.set(
            "evidence",
            format!(
                "Byte-identical ({} bytes) to {} of the iPad kernelcache ({}), reviewed in XnuPrelink's {fixture_name}: {}",
                key.0,
                text(record, "original")?,
                ipad.chars().take(12).collect::<String>(),
                text(record, "evidence")?
            ),
        );
        ported.push(entry);
    }
    let count = ported.len();
    reviewed.set("functions", kept.into_iter().chain(ported).collect::<Vec<Value>>());
    std::fs::write(&args.reviewed, pyjson::dumps(&reviewed, Some(1), false) + "\n").with_context(|| args.reviewed.clone())?;
    println!("port-fixture: {count} of {} reviewed functions carried over ({ambiguous} with more than one identical function here, left out)", wanted.len());
    Ok(())
}
