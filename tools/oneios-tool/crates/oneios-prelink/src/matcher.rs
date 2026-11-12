//! `prelink match`: pair the functions and data of the kernelcache's own kernel with the hosted kernel's, a port
//! of XnuPrelink's fileset_match.py at pin 9b950f2 (matcher version 8). The same match.map.json byte for byte,
//! and the same lines on stdout.
//!
//! What byte identity needs, and where it is kept: Python's dicts iterate in insertion order (`IndexMap` here: the
//! pairs, the data pairs, the vote tables, the symbol table, the fixups), and one loop iterates a `set` of
//! integers whose order decides the order of new data pairs (`pair_metaclass_vtables`: `PySet`). Every other set
//! is only counted, intersected or reduced to its one member. Methods fileset_match.py defines but match_images
//! never calls (align_orders, validate_calls, validate_diversity, diversity_data) are not ported.
//!
//! The matcher signature is the SHA-256 XnuPrelink computes over its own matcher sources and reviewed fixtures;
//! this step reads those files from `--xnuprelink` so that the map says which matcher rules it follows, and the
//! emitter refuses a map made under other rules, as XnuPrelink's does.

use crate::image::{checked_shape, hex, scan, unhex, Fixups, Image, Symbols};
use crate::pyset::PySet;
use anyhow::{anyhow, bail, Context, Result};
use indexmap::IndexMap;
use oneios_common::pyjson::{self, Value};
use oneios_common::sha256_hex;
use rayon::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

const EXEC_SEGMENTS: [&str; 4] = ["__TEXT_EXEC", "__TEXT_BOOT_EXEC", "__KLD", "__LAST"];
pub const MATCHER_VERSION: i128 = 8;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// decompressed MH_FILESET kernelcache
    kernelcache: String,
    /// native arm64e kernel with symbols
    native: String,
    /// fileset entry to pair
    #[arg(long, default_value = "com.apple.kernel")]
    entry: String,
    /// write the pairing as JSON to this path
    #[arg(long)]
    out: Option<String>,
    /// call graph propagation rounds
    #[arg(long, default_value_t = 40)]
    max_rounds: usize,
    /// additional symbol-bearing kernel for unique code matches
    #[arg(long)]
    reference_kernel: Option<String>,
    /// the XnuPrelink checkout: its reviewed fixtures and the matcher signature
    #[arg(long)]
    xnuprelink: String,
}

/// XnuPrelink's matcher_signature(): SHA-256 over the matcher's sources and reviewed fixtures.
pub fn matcher_signature(xnuprelink: &Path) -> Result<String> {
    use sha2::Digest;
    let mut digest = sha2::Sha256::new();
    let mut paths = vec![xnuprelink.join("fileset_match.py"), xnuprelink.join("kc_image.py"), xnuprelink.join("fileset_shape.py")];
    paths.extend(fixtures(xnuprelink)?);
    for path in paths {
        digest.update(path.file_name().unwrap().as_encoded_bytes());
        digest.update([0u8]);
        digest.update(std::fs::read(&path).with_context(|| format!("{}", path.display()))?);
    }
    Ok(oneios_common::hex(&digest.finalize()))
}

/// fixtures/fileset-*-symbols.json, sorted.
pub fn fixtures(xnuprelink: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut found = Vec::new();
    if let Ok(dir) = std::fs::read_dir(xnuprelink.join("fixtures")) {
        for entry in dir {
            let path = entry?.path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with("fileset-") && name.ends_with("-symbols.json") {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// The reviewed fixture records pinned to this kernelcache and entry.
pub fn reviewed_records(xnuprelink: &Path, kernelcache_sha256: &str, entry: &str) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for path in fixtures(xnuprelink)? {
        let record = pyjson::load(&path)?;
        let pinned = record.get("inputs").ok_or_else(|| anyhow!("{}: no inputs", path.display()))?;
        if pinned.get("kernelcache_sha256").and_then(Value::as_str) == Some(kernelcache_sha256) && pinned.get("entry").and_then(Value::as_str) == Some(entry) {
            out.push(record);
        }
    }
    Ok(out)
}

/// What two functions are compared by: the masked shape's digest and length, the resolved addresses and the calls
/// (each with its offset in the function).
pub struct Body {
    digest: [u8; 16],
    length: usize,
    refs: Vec<(u64, u64)>,
    calls: Vec<(u64, u64)>,
}

pub struct Func {
    end: u64,
    body: Body,
    strings: BTreeSet<String>,
}

fn md5_words(words: &[u32]) -> [u8; 16] {
    use md5::Digest;
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    md5::Md5::digest(&bytes).into()
}

/// Functions, references and pointer fixups of one kernel.
pub struct Side<'a> {
    image: Image<'a>,
    fixups: Arc<Fixups>,
    mapped: Vec<(u64, u64)>,
    exec_ranges: Vec<(u64, u64)>,
    starts: Vec<u64>,
    functions: IndexMap<u64, Func>,
    checked: HashMap<u64, Body>,
    callees: HashMap<u64, BTreeSet<u64>>,
    callers: HashMap<u64, BTreeSet<u64>>,
    symbol_names: Symbols,
    symbol_addrs: Vec<u64>,
    named_functions: HashMap<String, u64>,
    string_owners: IndexMap<String, BTreeSet<u64>>,
    diversity: IndexMap<u16, BTreeSet<u64>>,
    /// target -> the mapped sites that hold it as an IA-signed pointer, in fixup order
    uses: HashMap<u64, Vec<u64>>,
}

static EMPTY: BTreeSet<u64> = BTreeSet::new();

fn within(ranges: &[(u64, u64)], address: u64) -> bool {
    ranges.iter().any(|(b, e)| *b <= address && address < *e)
}

impl<'a> Side<'a> {
    pub fn new(image: Image<'a>, fixups: Arc<Fixups>, mapped: Vec<(u64, u64)>) -> Result<Side<'a>> {
        let execs: Vec<_> = image.segments.iter().filter(|s| EXEC_SEGMENTS.contains(&s.name.as_str()) && s.size != 0).collect();
        let exec_ranges: Vec<(u64, u64)> = execs.iter().map(|s| (s.va, s.va + s.size)).collect();
        let backed: Vec<(u64, u64)> = execs.iter().filter(|s| s.filesize != 0).map(|s| (s.va, s.va + s.filesize)).collect();
        let starts: Vec<u64> = image.function_starts()?.into_iter().filter(|a| within(&exec_ranges, *a)).collect::<BTreeSet<u64>>().into_iter().collect();
        let scanned: Vec<Option<(u64, Func, Body, Vec<u64>)>> = (0..starts.len())
            .into_par_iter()
            .map(|index| -> Result<Option<(u64, Func, Body, Vec<u64>)>> {
                let start = starts[index];
                let mut end = exec_ranges.iter().filter(|(b, e)| *b <= start && start < *e).map(|r| r.1).chain(starts.get(index + 1).copied()).min().unwrap();
                for (b, e) in &backed {
                    if *b <= start && start < *e {
                        end = end.min(*e);
                    }
                }
                if end < start + 4 {
                    return Ok(None);
                }
                let code = image.read_of(start, end - start)?;
                let found = scan(code, start);
                let mut strings = BTreeSet::new();
                for r in &found.refs {
                    if !within(&exec_ranges, r.target) {
                        if let Some(text) = image.cstring(r.target)? {
                            if text.len() >= 4 {
                                strings.insert(text.to_string());
                            }
                        }
                    }
                }
                let body = Body {
                    digest: md5_words(&found.shape),
                    length: found.shape.len(),
                    refs: found.refs.iter().map(|r| (r.site - start, r.target)).collect(),
                    calls: found.calls.iter().map(|c| (c.0 - start, c.1)).collect(),
                };
                let (shape, positions) = checked_shape(code, &found);
                let at = |site: u64| positions[((site - start) / 4) as usize].map(u64::from);
                let checked = Body {
                    digest: md5_words(&shape),
                    length: shape.len(),
                    refs: found.refs.iter().filter_map(|r| at(r.site).map(|p| (p, r.target))).collect(),
                    calls: found.calls.iter().filter_map(|c| at(c.0).map(|p| (p, c.1))).collect(),
                };
                let targets = found.calls.iter().map(|c| c.1).collect();
                Ok(Some((start, Func { end, body, strings }, checked, targets)))
            })
            .collect::<Result<Vec<_>>>()?;
        let (mut functions, mut checked) = (IndexMap::new(), HashMap::new());
        let (mut callees, mut callers): (HashMap<u64, BTreeSet<u64>>, HashMap<u64, BTreeSet<u64>>) = (HashMap::new(), HashMap::new());
        for (start, func, check, targets) in scanned.into_iter().flatten() {
            for target in targets {
                if within(&exec_ranges, target) {
                    let at = starts.partition_point(|s| *s <= target);
                    if at > 0 {
                        callees.entry(start).or_default().insert(starts[at - 1]);
                        callers.entry(starts[at - 1]).or_default().insert(start);
                    }
                }
            }
            functions.insert(start, func);
            checked.insert(start, check);
        }
        let symbol_names = image.symbols()?;
        let mut symbol_addrs: Vec<u64> = symbol_names.keys().copied().collect();
        symbol_addrs.sort_unstable();
        let mut names: HashMap<&str, (u64, bool)> = HashMap::new();
        for (address, symbols) in &symbol_names {
            if functions.contains_key(address) {
                for name in symbols {
                    names.entry(name).and_modify(|e| e.1 |= e.0 != *address).or_insert((*address, false));
                }
            }
        }
        let named_functions = names.into_iter().filter(|(_, (_, several))| !several).map(|(name, (address, _))| (name.to_string(), address)).collect();
        let mut string_owners: IndexMap<String, BTreeSet<u64>> = IndexMap::new();
        for (start, func) in &functions {
            for text in &func.strings {
                string_owners.entry(text.clone()).or_default().insert(*start);
            }
        }
        let mut diversity: IndexMap<u16, BTreeSet<u64>> = IndexMap::new();
        let mut uses: HashMap<u64, Vec<u64>> = HashMap::new();
        for (site, fix) in fixups.iter() {
            if fix.auth && fix.diversity != 0 && within(&mapped, fix.target) {
                diversity.entry(fix.diversity).or_default().insert(fix.target);
            }
            if fix.is_ia() && within(&mapped, *site) {
                uses.entry(fix.target).or_default().push(*site);
            }
        }
        Ok(Side { image, fixups, mapped, exec_ranges, starts, functions, checked, callees, callers, symbol_names, symbol_addrs, named_functions, string_owners, diversity, uses })
    }

    fn in_exec(&self, address: u64) -> bool {
        within(&self.exec_ranges, address)
    }
    fn in_mapped(&self, address: u64) -> bool {
        within(&self.mapped, address)
    }
    fn of(&self, address: u64) -> Result<&Func> {
        self.functions.get(&address).ok_or_else(|| anyhow!("{address:#x} is paired but is not a scanned function (Python's KeyError)"))
    }
    fn callees(&self, address: u64) -> &BTreeSet<u64> {
        self.callees.get(&address).unwrap_or(&EMPTY)
    }
    fn callers(&self, address: u64) -> &BTreeSet<u64> {
        self.callers.get(&address).unwrap_or(&EMPTY)
    }
    fn names(&self, address: u64) -> &[String] {
        self.symbol_names.get(&address).map_or(&[], Vec::as_slice)
    }
    fn uses(&self, address: u64) -> &[u64] {
        self.uses.get(&address).map_or(&[], Vec::as_slice)
    }

    fn name_at(&self, address: u64) -> (Option<&str>, u64) {
        let (names, addend) = match self.symbol_names.get(&address).filter(|n| !n.is_empty()) {
            Some(names) => (names, 0),
            None => {
                let index = self.symbol_addrs.partition_point(|a| *a <= address);
                if index == 0 {
                    return (None, 0);
                }
                let base = self.symbol_addrs[index - 1];
                if address - base > 0x10000 {
                    return (None, 0);
                }
                (&self.symbol_names[&base], address - base)
            }
        };
        (names.iter().map(String::as_str).min(), addend)
    }

    fn section_of(&self, address: u64) -> String {
        self.image.section(address).map_or("?".to_string(), |s| format!("{},{}", s.segment, s.name))
    }
    /// `section_of(address).split(',')[n]`; Python's IndexError where there is no section.
    fn section_part(&self, address: u64, last: bool) -> Result<String> {
        match self.image.section(address) {
            Some(s) => Ok(s.name.clone()),
            None if last => Ok("?".to_string()),
            None => bail!("{address:#x} is in no section (Python's IndexError)"),
        }
    }

    /// The 16 bytes at a table's start are zero; None where Python's read raises ValueError.
    fn zero16(&self, address: u64) -> Option<bool> {
        self.image.read(address, 16).map(|b| b.iter().all(|x| *x == 0))
    }
}

fn vtable_slots(side: &Side, base: u64) -> Vec<crate::image::Fix> {
    let mut result = Vec::new();
    for offset in (16..8192).step_by(8) {
        match side.fixups.get(&(base + offset)) {
            Some(pointer) if pointer.is_ia() && side.in_exec(pointer.target) => result.push(*pointer),
            _ => return result,
        }
    }
    Vec::new() // unterminated, so no proven table extent
}

type Known = IndexMap<u64, (u64, &'static str)>;

/// The finished pairing of one pass: what the reference steps read, and the JSON.
pub struct Pairing {
    /// original -> (native, how), sorted by original
    functions: Vec<(u64, u64, &'static str)>,
    /// original -> native, sorted by original
    data: Vec<(u64, u64)>,
    pub json: Value,
}

struct Matcher<'s, 'a> {
    original: &'s Side<'a>,
    native: &'s Side<'a>,
    pairs: IndexMap<u64, (u64, &'static str)>,
    reverse: HashMap<u64, u64>,
    conflicts: Vec<Value>,
}

const PURE_LEAF_SKIP: [u32; 4] = [0xd503_245f, 0xd503_241f, 0xd503_249f, 0xd503_24df];
const PURE_LEAF_RET: [u32; 3] = [0xd65f_03c0, 0xd65f_0bff, 0xd65f_0fff];

fn pure_leaf(side: &Side, start: u64) -> Result<Option<Vec<u32>>> {
    let Some(func) = side.functions.get(&start) else { return Ok(None) };
    let count = (func.end - start) / 4;
    let raw = side.image.read_of(start, count * 4)?;
    let words: Vec<u32> = raw.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).filter(|w| !PURE_LEAF_SKIP.contains(w)).collect();
    for (index, word) in words.iter().take(8).enumerate() {
        if PURE_LEAF_RET.contains(word) {
            return Ok(Some(words[..=index].to_vec()));
        }
        if !matches!((word >> 25) & 0xf, 0b1000 | 0b1001 | 0b0101 | 0b1101) {
            return Ok(None);
        }
    }
    Ok(None)
}

fn skip_name(name: &str) -> bool {
    [".cold.", "OUTLINED_FUNCTION", ".llvm."].iter().any(|part| name.contains(part))
}

impl<'s, 'a> Matcher<'s, 'a> {
    fn how(&self, orig: u64) -> Result<&'static str> {
        self.pairs.get(&orig).map(|p| p.1).ok_or_else(|| anyhow!("{orig:#x} has no pair (Python's KeyError)"))
    }
    fn paired(&self, orig: u64) -> Option<u64> {
        self.pairs.get(&orig).map(|p| p.0)
    }

    fn bind(&mut self, orig: u64, native: u64, how: &'static str) -> usize {
        if self.paired(orig) == Some(native) {
            return 0;
        }
        if self.native.names(native).iter().any(|name| name.contains(".cold.")) {
            if let (Some(left), Some(right)) = (self.original.functions.get(&orig), self.native.functions.get(&native)) {
                if left.body.digest != right.body.digest {
                    return 0;
                }
            }
        }
        if self.pairs.contains_key(&orig) || self.reverse.contains_key(&native) && !matches!(how, "vtable" | "reviewed" | "metaclass-getter") {
            let mut c = Value::dict();
            c.set("kind", "double-bind");
            c.set("orig", hex(orig));
            c.set("native", hex(native));
            c.set("via", how);
            self.conflicts.push(c);
            return 0;
        }
        self.pairs.insert(orig, (native, how));
        self.reverse.entry(native).or_insert(orig);
        1
    }

    fn pair_pure_leaves(&mut self, targets: &BTreeSet<u64>) -> Result<usize> {
        let mut pool: HashMap<Vec<u32>, VecDeque<u64>> = HashMap::new();
        let mut natives: Vec<u64> = self.native.functions.keys().copied().collect();
        natives.sort_unstable();
        for start in natives {
            if let Some(body) = pure_leaf(self.native, start)? {
                if !self.reverse.contains_key(&start) {
                    pool.entry(body).or_default().push_back(start);
                }
            }
        }
        let mut bound = 0;
        for &orig in targets {
            if self.pairs.contains_key(&orig) || !self.original.functions.contains_key(&orig) {
                continue;
            }
            if let Some(body) = pure_leaf(self.original, orig)? {
                if let Some(native) = pool.get_mut(&body).and_then(VecDeque::pop_front) {
                    bound += self.bind(orig, native, "pure-leaf");
                }
            }
        }
        Ok(bound)
    }

    /// The native function that has the one transferable name of the reference function `r`.
    fn transfer(&self, reference: &Side, r: u64) -> Option<u64> {
        let targets: BTreeSet<u64> = reference
            .names(r)
            .iter()
            .filter(|name| reference.named_functions.get(*name) == Some(&r) && !skip_name(name))
            .filter_map(|name| self.native.named_functions.get(name).copied())
            .collect();
        if targets.len() == 1 {
            targets.into_iter().next()
        } else {
            None
        }
    }

    fn anchor_reference(&mut self, reference: &Side, pairing: &Pairing) -> usize {
        let mut bound = 0;
        for &(orig, r, how) in &pairing.functions {
            if !matches!(how, "shape" | "pac-checked-shape" | "reference-shape" | "reviewed") {
                continue;
            }
            if let Some(target) = self.transfer(reference, r) {
                bound += self.bind(orig, target, "reference-symbol");
            }
        }
        bound
    }

    fn anchor_reference_order(&mut self, reference: &Side, pairing: &Pairing, rounds: usize) -> Result<usize> {
        let original = self.original;
        let mut pairs: IndexMap<u64, u64> = pairing.functions.iter().map(|f| (f.0, f.1)).collect();
        for _ in 0..rounds {
            let reverse: HashSet<u64> = pairs.values().copied().collect();
            let mut anchors: Vec<u64> = pairs.keys().copied().collect();
            anchors.sort_unstable();
            let mut added: IndexMap<u64, (u64, bool)> = IndexMap::new();
            for w in anchors.windows(2) {
                let (left, right) = (w[0], w[1]);
                if pairs[&right] <= pairs[&left] {
                    continue;
                }
                let (a, b) = (original.starts.partition_point(|s| *s <= left), original.starts.partition_point(|s| *s < right));
                let gap: Vec<u64> = original.starts[a..b.max(a)].iter().copied().filter(|o| !pairs.contains_key(o) && original.functions.contains_key(o)).collect();
                let (a, b) = (reference.starts.partition_point(|s| *s <= pairs[&left]), reference.starts.partition_point(|s| *s < pairs[&right]));
                let fill: Vec<u64> = reference.starts[a..b.max(a)].iter().copied().filter(|n| !reverse.contains(n) && reference.functions.contains_key(n)).collect();
                if gap.is_empty() || fill.is_empty() || gap.len() * fill.len() > 40000 {
                    continue;
                }
                let l0: Vec<usize> = gap.iter().map(|o| original.functions[o].body.length).collect();
                let l1: Vec<usize> = fill.iter().map(|n| reference.functions[n].body.length).collect();
                let close = |i: usize, j: usize| l0[i].abs_diff(l1[j]) <= 2.max(l0[i].max(l1[j]) / 10);
                if gap.len() == fill.len() {
                    for i in 0..gap.len() {
                        if close(i, i) {
                            added.insert(gap[i], (fill[i], true));
                        }
                    }
                    continue;
                }
                let mut table = vec![vec![0u32; fill.len() + 1]; gap.len() + 1];
                for i in (0..gap.len()).rev() {
                    for j in (0..fill.len()).rev() {
                        table[i][j] = if close(i, j) { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
                    }
                }
                let (mut i, mut j) = (0, 0);
                while i < gap.len() && j < fill.len() {
                    if close(i, j) && table[i][j] == table[i + 1][j + 1] + 1 {
                        added.insert(gap[i], (fill[j], false));
                        i += 1;
                        j += 1;
                    } else if table[i + 1][j] >= table[i][j + 1] {
                        i += 1;
                    } else {
                        j += 1;
                    }
                }
            }
            // Order alone is not proof: calls into paired functions must agree with the reference, and a function
            // without such calls needs a near-identical length.
            let mut checked: IndexMap<u64, u64> = IndexMap::new();
            for (&orig, &(r, equal)) in &added {
                let (mut agree, mut miss) = (0usize, 0usize);
                for t in original.callees(orig) {
                    if let Some(p) = pairs.get(t) {
                        if reference.callees(r).contains(p) {
                            agree += 1;
                        } else {
                            miss += 1;
                        }
                    }
                }
                let (left, right) = (&original.functions[&orig], &reference.functions[&r]);
                let exact = left.body.length.abs_diff(right.body.length) <= 2;
                let (t0, t1) = (&left.strings, &right.strings);
                if !t0.is_empty() && !t1.is_empty() && t0 != t1 {
                    continue;
                }
                if (agree != 0 && miss * 4 <= agree) || (equal && agree == 0 && miss == 0 && exact) || (!t0.is_empty() && t0 == t1 && miss == 0) {
                    checked.insert(orig, r);
                }
            }
            if checked.is_empty() {
                break;
            }
            for (k, v) in checked {
                pairs.insert(k, v);
            }
        }
        // A pair is kept when it sits in order between its neighbors.
        let mut anchors: Vec<u64> = pairs.keys().copied().collect();
        anchors.sort_unstable();
        let mut monotone = HashSet::new();
        for i in 1..anchors.len().saturating_sub(1) {
            if pairs[&anchors[i - 1]] < pairs[&anchors[i]] && pairs[&anchors[i]] < pairs[&anchors[i + 1]] {
                monotone.insert(anchors[i]);
            }
        }
        let mut bound = 0;
        for (&orig, &r) in &pairs {
            if self.pairs.contains_key(&orig) || !monotone.contains(&orig) {
                continue;
            }
            if let Some(target) = self.transfer(reference, r) {
                bound += self.bind(orig, target, "reference-order");
            }
        }
        Ok(bound)
    }

    fn reference_data(&self, reference: &Side, pairing: &Pairing, targets: &BTreeSet<u64>, known: &mut Known) -> Result<usize> {
        let pairs: HashMap<u64, u64> = pairing.data.iter().copied().collect();
        let anchors: Vec<u64> = pairing.data.iter().map(|d| d.0).collect();
        let mut natives: HashMap<&str, BTreeSet<u64>> = HashMap::new();
        for (address, names) in &self.native.symbol_names {
            if !self.native.in_exec(*address) {
                for name in names {
                    natives.entry(name).or_default().insert(*address);
                }
            }
        }
        let mut added = 0;
        for &target in targets {
            if known.contains_key(&target) || self.original.in_exec(target) {
                continue;
            }
            let r = match pairs.get(&target) {
                Some(r) => *r,
                None => {
                    let index = anchors.partition_point(|a| *a <= target);
                    if index == 0 || target - anchors[index - 1] > 0x10000 {
                        continue;
                    }
                    let low = anchors[index - 1];
                    if self.original.section_of(low) != self.original.section_of(target) {
                        continue;
                    }
                    let r = pairs[&low] + target - low;
                    if index < anchors.len() && anchors[index] - target < 0x10000 && pairs[&anchors[index]] as i128 - r as i128 != (anchors[index] - target) as i128 {
                        continue;
                    }
                    r
                }
            };
            if reference.section_part(r, false)? != self.original.section_part(target, false)? {
                continue;
            }
            let (Some(name), addend) = reference.name_at(r) else { continue };
            if !(addend == 0 || addend == 16) || (addend != 0 && !name.starts_with("__ZTV")) {
                continue;
            }
            if !reference.names(r - addend).is_empty() {
                if let Some(found) = natives.get(name).filter(|f| f.len() == 1) {
                    known.insert(target, (*found.iter().next().unwrap() + addend, "reference-data"));
                    added += 1;
                }
            }
        }
        Ok(added)
    }

    fn pair_data_slots(&self, targets: &BTreeSet<u64>, known: &mut Known) -> usize {
        let mut pointed: HashMap<u64, BTreeSet<u64>> = HashMap::new();
        for (site, fix) in self.native.fixups.iter() {
            if !self.native.in_exec(*site) && !self.native.names(*site).is_empty() {
                pointed.entry(fix.target).or_default().insert(*site);
            }
        }
        let objects: HashMap<u64, u64> = known.iter().map(|(a, (n, _))| (*a, *n)).collect();
        let mut added = 0;
        for &target in targets {
            if known.contains_key(&target) || self.original.in_exec(target) {
                continue;
            }
            let Some(fix) = self.original.fixups.get(&target) else { continue };
            let native_object = if self.original.in_exec(fix.target) { self.paired(fix.target) } else { objects.get(&fix.target).copied() };
            if let Some(slots) = native_object.and_then(|n| pointed.get(&n)) {
                if slots.len() == 1 {
                    known.insert(target, (*slots.iter().next().unwrap(), "slot"));
                    added += 1;
                }
            }
        }
        added
    }

    fn pair_sysctl_variables(&self, targets: &BTreeSet<u64>, known: &mut Known) -> usize {
        fn variables(side: &Side) -> IndexMap<Vec<u8>, BTreeSet<u64>> {
            let mut found: IndexMap<Vec<u8>, BTreeSet<u64>> = IndexMap::new();
            for (site, fix) in side.fixups.iter() {
                let Some(arg) = side.fixups.get(&site.wrapping_sub(0x10)) else { continue };
                if side.in_exec(arg.target) || !side.in_mapped(*site) {
                    continue;
                }
                let Some(raw) = side.image.read(fix.target, 64) else { continue };
                let Some(len) = raw.iter().position(|b| *b == 0) else { continue };
                let name = &raw[..len];
                if (3..64).contains(&len) && name.iter().all(|b| (48..123).contains(b) || *b == 95) {
                    found.entry(name.to_vec()).or_default().insert(arg.target);
                }
            }
            found
        }
        let (original, native) = (variables(self.original), variables(self.native));
        let mut added = 0;
        // SYSCTL_NODE anchors "__anchor__(_parent_name)" hang off their node's children list (oid_parent, +0).
        let mut symbols: HashMap<&str, u64> = HashMap::new();
        for (address, names) in &self.native.symbol_names {
            for name in names {
                symbols.insert(name, *address);
            }
        }
        for (site, fix) in self.original.fixups.iter() {
            let Some(parent) = self.original.fixups.get(&site.wrapping_sub(0x28)) else { continue };
            if !targets.contains(&parent.target) || known.contains_key(&parent.target) {
                continue;
            }
            let Some(raw) = self.original.image.read(fix.target, 80) else { continue };
            let name = &raw[..raw.iter().position(|b| *b == 0).unwrap_or(raw.len())];
            if name.starts_with(b"__anchor__(") && name.ends_with(b")") {
                let Ok(inner) = std::str::from_utf8(&name[11..name.len() - 1]) else { continue };
                if let Some(children) = symbols.get(format!("_sysctl_{inner}_children").as_str()) {
                    known.insert(parent.target, (*children, "sysctl"));
                    added += 1;
                }
            }
        }
        for (name, addresses) in &original {
            let Some(theirs) = native.get(name) else { continue };
            if addresses.len() != 1 || theirs.len() != 1 {
                continue;
            }
            let address = *addresses.iter().next().unwrap();
            if targets.contains(&address) && !known.contains_key(&address) {
                known.insert(address, (*theirs.iter().next().unwrap(), "sysctl"));
                added += 1;
            }
        }
        added
    }

    fn pair_data_fields(&self, targets: &BTreeSet<u64>, known: &mut Known) -> usize {
        let mut votes: IndexMap<u64, IndexMap<u64, usize>> = IndexMap::new();
        for (&orig, &(native, _)) in known.iter() {
            for offset in (0..0x100).step_by(8) {
                if let (Some(left), Some(right)) = (self.original.fixups.get(&(orig + offset)), self.native.fixups.get(&(native + offset))) {
                    if targets.contains(&left.target) && !known.contains_key(&left.target) && !self.original.in_exec(left.target) && !self.native.in_exec(right.target) {
                        *votes.entry(left.target).or_default().entry(right.target).or_default() += 1;
                    }
                }
            }
        }
        let mut added = 0;
        for (target, counter) in votes {
            let (mut best, mut count) = (0, 0);
            for (n, c) in &counter {
                if *c > count {
                    (best, count) = (*n, *c);
                }
            }
            if count >= 2 && count * 4 >= counter.values().sum::<usize>() * 3 {
                known.insert(target, (best, "fields"));
                added += 1;
            }
        }
        added
    }

    fn pair_data_by_users(&self, targets: &BTreeSet<u64>, known: &mut Known) -> Result<usize> {
        let mut users: HashMap<u64, Vec<u64>> = HashMap::new();
        for (orig, func) in &self.original.functions {
            if self.pairs.contains_key(orig) {
                for (_, address) in &func.body.refs {
                    let list = users.entry(*address).or_default();
                    if list.last() != Some(orig) && !list.contains(orig) {
                        list.push(*orig);
                    }
                }
            }
        }
        let mut added = 0;
        for &target in targets {
            let Some(mine) = users.get(&target) else { continue };
            if known.contains_key(&target) || self.original.in_exec(target) {
                continue;
            }
            let section = self.original.section_part(target, true)?;
            let mut candidates: Option<BTreeSet<u64>> = None;
            for orig in mine {
                let mut refs = BTreeSet::new();
                for (_, address) in &self.native.of(self.pairs[orig].0)?.body.refs {
                    if !self.native.in_exec(*address) && self.native.section_part(*address, true)? == section {
                        refs.insert(*address);
                    }
                }
                candidates = Some(match candidates {
                    None => refs,
                    Some(c) => c.intersection(&refs).copied().collect(),
                });
            }
            if let Some(c) = candidates {
                if c.len() == 1 && mine.len() >= 2 {
                    known.insert(target, (*c.iter().next().unwrap(), "users"));
                    added += 1;
                }
            }
        }
        Ok(added)
    }

    fn anchor_reviewed(&mut self, records: &[Value]) -> Result<usize> {
        let mut added = 0;
        for record in records {
            for entry in record.get("functions").and_then(Value::as_list).unwrap_or(&[]) {
                if !truthy(entry.get("evidence")) {
                    bail!("Reviewed function requires recorded evidence");
                }
                let text = entry.get("original").and_then(Value::as_str).unwrap_or("");
                let orig = unhex(text)?;
                let Some(func) = self.original.functions.get(&orig) else { bail!("Reviewed function is absent: {text}") };
                if Some(sha256_hex(self.original.image.read_of(orig, func.end - orig)?).as_str()) != entry.get("original_sha256").and_then(Value::as_str) {
                    bail!("Reviewed function bytes changed: {text}");
                }
                if let Some(native) = entry.get("native_symbol").and_then(Value::as_str).and_then(|n| self.native.named_functions.get(n)) {
                    added += self.bind(orig, *native, "reviewed");
                }
            }
        }
        Ok(added)
    }

    fn reviewed_data(&self, records: &[Value], known: &mut Known) -> Result<usize> {
        let mut names: HashMap<&str, BTreeSet<u64>> = HashMap::new();
        for (address, symbols) in &self.native.symbol_names {
            for name in symbols {
                names.entry(name).or_default().insert(*address);
            }
        }
        let mut added = 0;
        for record in records {
            for entry in record.get("data").and_then(Value::as_list).unwrap_or(&[]) {
                if !truthy(entry.get("evidence")) {
                    bail!("Reviewed data requires recorded evidence");
                }
                if let Some(found) = entry.get("native_symbol").and_then(Value::as_str).and_then(|n| names.get(n)).filter(|f| f.len() == 1) {
                    let addend = entry.get("addend").and_then(Value::as_int).unwrap_or(0);
                    let orig = unhex(entry.get("original").and_then(Value::as_str).unwrap_or(""))?;
                    known.insert(orig, ((*found.iter().next().unwrap() as i128 + addend) as u64, "reviewed"));
                    added += 1;
                }
            }
        }
        Ok(added)
    }

    /// Unique string references anchor the first pairs.
    fn anchor_strings(&mut self) -> usize {
        let mut votes: IndexMap<u64, IndexMap<u64, usize>> = IndexMap::new();
        for (text, owners) in &self.original.string_owners {
            let Some(targets) = self.native.string_owners.get(text) else { continue };
            if owners.len() != 1 || targets.len() != 1 {
                continue;
            }
            let (orig, mut target) = (*owners.iter().next().unwrap(), *targets.iter().next().unwrap());
            // LLVM may outline the panic path into foo.cold.N in only one build: a string there identifies foo.
            let (left, right) = (&self.original.functions[&orig], &self.native.functions[&target]);
            if left.body.digest != right.body.digest {
                let parents: BTreeSet<u64> = self
                    .native
                    .names(target)
                    .iter()
                    .filter(|name| name.contains(".cold."))
                    .filter_map(|name| self.native.named_functions.get(name.split(".cold.").next().unwrap()).copied())
                    .collect();
                if !parents.is_empty() {
                    if parents.len() != 1 {
                        continue;
                    }
                    let parent = *parents.iter().next().unwrap();
                    if !self.native.callees(parent).contains(&target) {
                        continue;
                    }
                    target = parent;
                }
            }
            *votes.entry(orig).or_default().entry(target).or_default() += 1;
        }
        let mut claimed: HashMap<u64, usize> = HashMap::new();
        for counter in votes.values() {
            for n in counter.keys() {
                *claimed.entry(*n).or_default() += 1;
            }
        }
        let mut bound = 0;
        for (orig, counter) in &votes {
            if counter.len() == 1 {
                let native = *counter.keys().next().unwrap();
                if claimed[&native] == 1 {
                    bound += self.bind(*orig, native, "string");
                }
            }
        }
        bound
    }

    /// A relocation-masked instruction sequence owned by exactly one function per side (`checked`: with the
    /// compiler's PAC checks taken out).
    fn anchor_shapes(&mut self, checked: bool) -> usize {
        let mut groups: IndexMap<([u8; 16], usize), (Vec<u64>, Vec<u64>)> = IndexMap::new();
        for (index, side) in [self.original, self.native].into_iter().enumerate() {
            for (address, func) in &side.functions {
                let body = if checked { &side.checked[address] } else { &func.body };
                let group = groups.entry((body.digest, body.length)).or_default();
                if index == 0 { &mut group.0 } else { &mut group.1 }.push(*address);
            }
        }
        let mut bound = 0;
        for (originals, natives) in groups.values() {
            if originals.len() == 1 && natives.len() == 1 {
                bound += self.bind(originals[0], natives[0], if checked { "pac-checked-shape" } else { "shape" });
            }
        }
        bound
    }

    fn comparable(&self, orig: u64, native: u64) -> Result<(&'s Body, &'s Body)> {
        let (left, right) = (&self.original.of(orig)?.body, &self.native.of(native)?.body);
        if left.digest != right.digest {
            return Ok((self.original.checked.get(&orig).unwrap_or(left), self.native.checked.get(&native).unwrap_or(right)));
        }
        Ok((left, right))
    }

    /// Match unmatched functions by their set of already matched neighbor names.
    fn propagate(&mut self) -> usize {
        let mut index: HashMap<(Vec<u64>, Vec<u64>), Vec<u64>> = HashMap::new();
        for native in self.native.functions.keys() {
            if !self.reverse.contains_key(native) {
                let out: Vec<u64> = self.native.callees(*native).iter().copied().filter(|n| self.reverse.contains_key(n)).collect();
                let incoming: Vec<u64> = self.native.callers(*native).iter().copied().filter(|n| self.reverse.contains_key(n)).collect();
                index.entry((out, incoming)).or_default().push(*native);
            }
        }
        let (mut proposals, mut claimed): (Vec<(u64, u64)>, HashMap<u64, usize>) = (Vec::new(), HashMap::new());
        for (orig, func) in &self.original.functions {
            if self.pairs.contains_key(orig) {
                continue;
            }
            let out: BTreeSet<u64> = self.original.callees(*orig).iter().filter_map(|t| self.paired(*t)).collect();
            let incoming: BTreeSet<u64> = self.original.callers(*orig).iter().filter_map(|t| self.paired(*t)).collect();
            if out.union(&incoming).count() < 2 {
                continue;
            }
            let Some(found) = index.get(&(out.into_iter().collect(), incoming.into_iter().collect())) else { continue };
            let candidates: Vec<u64> = if found.len() > 1 { found.iter().copied().filter(|n| self.native.functions[n].body.digest == func.body.digest).collect() } else { found.clone() };
            if candidates.len() == 1 {
                proposals.push((*orig, candidates[0]));
                *claimed.entry(candidates[0]).or_default() += 1;
            }
        }
        let mut bound = 0;
        for (orig, native) in proposals {
            if claimed[&native] == 1 {
                bound += self.bind(orig, native, "calls");
            }
        }
        bound
    }

    /// Identical functions reference their data at identical instruction offsets.
    fn pair_data(&self, known: &mut Known) -> Result<usize> {
        let mut votes: IndexMap<u64, IndexMap<u64, ()>> = IndexMap::new();
        for (&orig, &(native, _)) in &self.pairs {
            let (left, right) = self.comparable(orig, native)?;
            if left.digest != right.digest || left.refs.len() != right.refs.len() {
                continue;
            }
            for (&(offset, address), &(native_offset, native_address)) in left.refs.iter().zip(&right.refs) {
                if offset != native_offset {
                    break;
                }
                if !self.original.in_exec(address) && !self.native.in_exec(native_address) && self.original.in_mapped(address) && self.native.in_mapped(native_address) {
                    votes.entry(address).or_default().insert(native_address, ());
                }
            }
        }
        let mut split = 0;
        for (address, counter) in votes {
            if known.contains_key(&address) {
                continue;
            }
            if counter.len() == 1 {
                known.insert(address, (*counter.keys().next().unwrap(), "refs"));
            } else {
                split += 1;
            }
        }
        Ok(split)
    }

    /// Use corresponding call sites in already paired, equal-shaped functions.
    fn pair_calls(&mut self) -> Result<usize> {
        let mut votes: IndexMap<u64, BTreeSet<u64>> = IndexMap::new();
        for (&orig, &(native, _)) in &self.pairs {
            let (left, right) = self.comparable(orig, native)?;
            if left.digest != right.digest || left.length != right.length || left.calls.len() != right.calls.len() {
                continue;
            }
            for (&(offset, target), &(native_offset, native_target)) in left.calls.iter().zip(&right.calls) {
                if offset != native_offset {
                    break;
                }
                if self.original.functions.contains_key(&target) && self.native.functions.contains_key(&native_target) {
                    votes.entry(target).or_default().insert(native_target);
                }
            }
        }
        let mut bound = 0;
        for (orig, targets) in votes {
            if targets.len() == 1 {
                bound += self.bind(orig, *targets.iter().next().unwrap(), "call-sites");
            }
        }
        Ok(bound)
    }

    /// Two identified data objects jointly identify their unique function user.
    fn pair_data_users(&mut self, known: &Known) -> usize {
        let mut users: HashMap<u64, BTreeSet<u64>> = HashMap::new();
        for (native, func) in &self.native.functions {
            for (_, address) in &func.body.refs {
                users.entry(*address).or_default().insert(*native);
            }
        }
        let (mut proposals, mut claimed): (Vec<(u64, u64)>, HashMap<u64, usize>) = (Vec::new(), HashMap::new());
        for (orig, func) in &self.original.functions {
            if self.pairs.contains_key(orig) {
                continue;
            }
            let anchors: BTreeSet<u64> = func.body.refs.iter().filter_map(|(_, address)| known.get(address).map(|k| k.0)).collect();
            if anchors.len() < 2 {
                continue;
            }
            let mut candidates: Option<BTreeSet<u64>> = None;
            for address in &anchors {
                let set = users.get(address).unwrap_or(&EMPTY);
                candidates = Some(match candidates {
                    None => set.clone(),
                    Some(c) => c.intersection(set).copied().collect(),
                });
            }
            let mut candidates = candidates.unwrap();
            let calls: BTreeSet<u64> = self.original.callees(*orig).iter().filter(|t| *t != orig).filter_map(|t| self.paired(*t)).collect();
            if candidates.len() > 1 && !calls.is_empty() {
                candidates.retain(|n| calls.is_subset(self.native.callees(*n)));
            }
            if candidates.len() == 1 {
                let native = *candidates.iter().next().unwrap();
                proposals.push((*orig, native));
                *claimed.entry(native).or_default() += 1;
            }
        }
        let mut bound = 0;
        for (orig, native) in proposals {
            if claimed[&native] == 1 {
                bound += self.bind(orig, native, "data-users");
            }
        }
        bound
    }

    /// Disambiguate equal-shaped methods by every data operand and call site.
    fn pair_reference_shapes(&mut self, known: &Known) -> Result<usize> {
        let mut shapes: HashMap<([u8; 16], usize), Vec<u64>> = HashMap::new();
        for (n, func) in &self.native.functions {
            shapes.entry((func.body.digest, func.body.length)).or_default().push(*n);
        }
        let (mut proposals, mut claimed): (Vec<(u64, u64)>, HashMap<u64, usize>) = (Vec::new(), HashMap::new());
        for (&o, func) in &self.original.functions {
            let left = &func.body;
            if self.pairs.contains_key(&o) || !left.refs.iter().any(|(_, a)| known.contains_key(a)) {
                continue;
            }
            let mut candidates = Vec::new();
            'next: for &n in shapes.get(&(left.digest, left.length)).map_or(&[][..], Vec::as_slice) {
                let right = &self.native.functions[&n].body;
                if left.refs.len() != right.refs.len() || left.calls.len() != right.calls.len() {
                    continue;
                }
                for (&(site, address), &(other, target)) in left.refs.iter().zip(&right.refs) {
                    if site != other {
                        continue 'next;
                    }
                    match known.get(&address) {
                        Some(k) => {
                            if k.0 != target {
                                continue 'next;
                            }
                        }
                        None => match self.original.image.cstring(address)? {
                            Some(string) if Some(string) == self.native.image.cstring(target)? => {}
                            _ => continue 'next,
                        },
                    }
                }
                for (&(site, address), &(other, target)) in left.calls.iter().zip(&right.calls) {
                    if site != other {
                        continue 'next;
                    }
                    if o <= address && address < func.end {
                        if target.wrapping_sub(n) != address - o {
                            continue 'next;
                        }
                    } else if self.paired(address) != Some(target) {
                        continue 'next;
                    }
                }
                candidates.push(n);
            }
            if candidates.len() == 1 {
                proposals.push((o, candidates[0]));
                *claimed.entry(candidates[0]).or_default() += 1;
            }
        }
        let mut bound = 0;
        for (o, n) in proposals {
            if claimed[&n] == 1 {
                bound += self.bind(o, n, "reference-shape");
            }
        }
        Ok(bound)
    }

    /// Match slots only within an identified table with the same complete PAC layout.
    fn anchor_vtables(&mut self, known: &Known) -> Result<usize> {
        let mut votes: IndexMap<u64, BTreeSet<u64>> = IndexMap::new();
        let mut identified = HashSet::new();
        for (&orig, &(native, how)) in known {
            if !matches!(how, "refs" | "vtable-layout" | "vtable-user" | "metaclass-vtable") || !self.native.names(native).iter().any(|n| n.starts_with("__ZTV")) {
                continue;
            }
            let zero = |side: &Side, a: u64| side.zero16(a).ok_or_else(|| anyhow!("Unbacked address {a:#x}"));
            if !zero(self.original, orig)? || !zero(self.native, native)? {
                continue;
            }
            let (left, right) = (vtable_slots(self.original, orig), vtable_slots(self.native, native));
            if left.len() < 2 || left.len() != right.len() || left.iter().zip(&right).any(|(a, b)| a.meta() != b.meta()) {
                continue;
            }
            for (a, b) in left.iter().zip(&right) {
                votes.entry(a.target).or_default().insert(b.target);
                if matches!(how, "metaclass-vtable" | "vtable-user") {
                    identified.insert(a.target);
                }
            }
        }
        let mut added = 0;
        for (orig, targets) in votes {
            if targets.len() != 1 {
                continue;
            }
            let native = *targets.iter().next().unwrap();
            if identified.contains(&orig) && self.paired(orig).is_some_and(|p| p != native) {
                if self.how(orig)? == "reviewed" {
                    bail!("Class vtable contradicts reviewed identity at {orig:#x}");
                }
                let (old, via) = self.pairs.shift_remove(&orig).unwrap();
                let mut c = Value::dict();
                c.set("kind", "vtable-correction");
                c.set("orig", hex(orig));
                c.set("previous", hex(old));
                c.set("native", hex(native));
                c.set("via", via);
                self.conflicts.push(c);
                if self.reverse.get(&old) == Some(&orig) {
                    self.reverse.remove(&old);
                    if let Some((other, _)) = self.pairs.iter().find(|(_, (n, _))| *n == old) {
                        self.reverse.insert(old, *other);
                    }
                }
            }
            added += self.bind(orig, native, "vtable");
        }
        Ok(added)
    }

    /// Find a table using at least two distinct bound methods and all PAC slots.
    fn pair_vtables(&self, known: &mut Known) -> Result<usize> {
        let claimed: HashSet<u64> = known.values().map(|k| k.0).collect();
        let mut added = 0;
        for (&native, names) in &self.native.symbol_names {
            if claimed.contains(&native) || !names.iter().any(|n| n.starts_with("__ZTV")) {
                continue;
            }
            let right = vtable_slots(self.native, native);
            let mut anchors: Vec<(u64, u64)> = Vec::new();
            for (index, pointer) in right.iter().enumerate() {
                if let Some(orig) = self.reverse.get(&pointer.target) {
                    if !matches!(self.how(*orig)?, "identical-leaf" | "thunk") {
                        anchors.push((index as u64 * 8 + 16, *orig));
                    }
                }
            }
            if anchors.iter().map(|a| a.1).collect::<HashSet<u64>>().len() < 2 {
                continue;
            }
            anchors.sort_by_key(|row| self.original.uses(row.1).len());
            let mut candidates: Option<BTreeSet<u64>> = None;
            for (offset, orig) in &anchors {
                let locations: BTreeSet<u64> = self.original.uses(*orig).iter().map(|site| site.wrapping_sub(*offset)).collect();
                let next = match candidates {
                    None => locations,
                    Some(c) => c.intersection(&locations).copied().collect(),
                };
                let empty = next.is_empty();
                candidates = Some(next);
                if empty {
                    break;
                }
            }
            let mut qualified = Vec::new();
            for orig in candidates.unwrap_or_default() {
                if known.contains_key(&orig) || !self.original.in_mapped(orig) || self.original.zero16(orig) != Some(true) {
                    continue;
                }
                let left = vtable_slots(self.original, orig);
                if left.len() != right.len() || left.iter().zip(&right).any(|(a, b)| a.meta() != b.meta()) {
                    continue;
                }
                // (fileset_match.py's "identical-leaf" exception: no pair has that method in matcher version 8.)
                if left.iter().zip(&right).any(|(a, b)| self.paired(a.target).is_some_and(|t| t != b.target)) {
                    continue;
                }
                qualified.push(orig);
            }
            if qualified.len() == 1 {
                known.insert(qualified[0], (native, "vtable-layout"));
                added += 1;
            }
        }
        Ok(added)
    }

    /// An identified class method can identify its own complete vtable.
    fn pair_vtable_users(&self, known: &mut Known) -> usize {
        let mut votes: IndexMap<u64, BTreeSet<u64>> = IndexMap::new();
        for (&orig, &(native, how)) in &self.pairs {
            if !matches!(how, "string" | "vtable" | "reviewed" | "reference-shape") {
                continue;
            }
            let (Some(left), Some(right)) = (self.original.functions.get(&orig), self.native.functions.get(&native)) else { continue };
            let mut native_tables: IndexMap<u64, Vec<(bool, u8, u16, bool)>> = IndexMap::new();
            for (_, address) in &right.body.refs {
                let names: Vec<&String> = self.native.names(*address).iter().filter(|n| n.starts_with("__ZTV")).collect();
                if names.is_empty() || !names.iter().any(|name| self.native.names(native).iter().any(|method| method.contains(&name[5..]))) {
                    continue;
                }
                let slots = vtable_slots(self.native, *address);
                if slots.len() >= 2 {
                    native_tables.insert(*address, slots.iter().map(|p| p.meta()).collect());
                }
            }
            let mut original_tables: IndexMap<u64, Vec<(bool, u8, u16, bool)>> = IndexMap::new();
            for (_, address) in &left.body.refs {
                if !self.original.in_mapped(*address) || self.original.zero16(*address) != Some(true) {
                    continue;
                }
                let slots = vtable_slots(self.original, *address);
                if slots.len() >= 2 {
                    original_tables.insert(*address, slots.iter().map(|p| p.meta()).collect());
                }
            }
            for (address, signature) in &native_tables {
                let candidates: Vec<u64> = original_tables.iter().filter(|(_, s)| *s == signature).map(|(a, _)| *a).collect();
                if candidates.len() == 1 && native_tables.values().filter(|s| *s == signature).count() == 1 {
                    votes.entry(candidates[0]).or_default().insert(*address);
                }
            }
        }
        let mut added = 0;
        for (orig, targets) in votes {
            if !known.contains_key(&orig) && targets.len() == 1 {
                known.insert(orig, (*targets.iter().next().unwrap(), "vtable-user"));
                added += 1;
            }
        }
        added
    }

    /// The class-specific getMetaClass pointer identifies a complete table.
    fn pair_metaclass_vtables(&self, known: &mut Known) -> Result<usize> {
        let mut by_native: HashMap<u64, Vec<u64>> = HashMap::new();
        for (&orig, &(native, how)) in &self.pairs {
            if matches!(how, "reference-shape" | "shape" | "vtable" | "reviewed" | "metaclass-getter") {
                by_native.entry(native).or_default().push(orig);
            }
        }
        let mut added = 0;
        for (&native, names) in &self.native.symbol_names {
            let table_names: Vec<&str> = names.iter().filter(|n| n.starts_with("__ZTV")).map(|n| &n[5..]).collect();
            if table_names.is_empty() {
                continue;
            }
            let right = vtable_slots(self.native, native);
            for (index, pointer) in right.iter().enumerate() {
                let method = pointer.target;
                if !self.native.names(method).iter().any(|name| name.contains("12getMetaClassEv") && table_names.iter().any(|cls| name.contains(cls))) {
                    continue;
                }
                for &orig_method in by_native.get(&method).map_or(&[][..], Vec::as_slice) {
                    let (left_fn, right_fn) = (&self.original.of(orig_method)?.body, &self.native.of(method)?.body);
                    if left_fn.digest != right_fn.digest || left_fn.refs.len() != 1 || right_fn.refs.len() != 1 {
                        continue;
                    }
                    let (original_meta, native_meta) = (left_fn.refs[0].1, right_fn.refs[0].1);
                    if known.get(&original_meta).map(|k| k.0) != Some(native_meta) {
                        continue;
                    }
                    if !self.native.names(native_meta).iter().any(|name| name.contains("10gMetaClassE") && table_names.iter().any(|cls| name.contains(cls))) {
                        continue;
                    }
                    // Python iterates a set of sites here, and each site that qualifies appends a data pair: the
                    // set's own order.
                    for site in PySet::from_adds(self.original.uses(orig_method).iter().copied()).iter() {
                        let orig = site.wrapping_sub(16 + index as u64 * 8);
                        if known.contains_key(&orig) || self.original.zero16(orig) != Some(true) {
                            continue;
                        }
                        let left = vtable_slots(self.original, orig);
                        if left.len() == right.len() && left.iter().zip(&right).all(|(a, b)| a.meta() == b.meta()) {
                            known.insert(orig, (native, "metaclass-vtable"));
                            added += 1;
                        }
                    }
                }
            }
        }
        Ok(added)
    }

    /// Identify getMetaClass by its sole known metaobject and exact code shape.
    fn pair_metaclass_getters(&mut self, known: &Known) -> usize {
        let mut getters: HashMap<(u64, [u8; 16], usize), BTreeSet<u64>> = HashMap::new();
        for (n, func) in &self.native.functions {
            let body = &func.body;
            if !body.calls.is_empty() || body.refs.len() != 1 {
                continue;
            }
            let address = body.refs[0].1;
            if !self.native.names(*n).iter().any(|name| name.ends_with("12getMetaClassEv")) || !self.native.names(address).iter().any(|name| name.ends_with("10gMetaClassE")) {
                continue;
            }
            getters.entry((address, body.digest, body.length)).or_default().insert(*n);
        }
        let mut found = Vec::new();
        for (o, func) in &self.original.functions {
            let body = &func.body;
            if self.pairs.contains_key(o) || !body.calls.is_empty() || body.refs.len() != 1 {
                continue;
            }
            let Some(target) = known.get(&body.refs[0].1).map(|k| k.0) else { continue };
            if let Some(candidates) = getters.get(&(target, body.digest, body.length)).filter(|c| c.len() == 1) {
                found.push((*o, *candidates.iter().next().unwrap()));
            }
        }
        // (Python binds inside the loop; a bind changes nothing the loop reads but `pairs` for later functions,
        // and each function is visited once.)
        found.into_iter().map(|(o, n)| self.bind(o, n, "metaclass-getter")).sum()
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Str(s)) => !s.is_empty(),
        Some(Value::List(l)) => !l.is_empty(),
        Some(Value::Dict(d)) => !d.is_empty(),
        Some(Value::Int(n)) => *n != 0,
        Some(Value::Float(f)) => *f != 0.0,
        Some(Value::Bool(true)) => true,
    }
}

/// A Counter as Python's dict of it: keys in first-seen order.
fn counter<'x>(items: impl Iterator<Item = &'x str>) -> Value {
    let mut counts: IndexMap<&str, i128> = IndexMap::new();
    for item in items {
        *counts.entry(item).or_default() += 1;
    }
    Value::Dict(counts.into_iter().map(|(k, v)| (k.to_string(), Value::Int(v))).collect())
}

/// `{v} {k}` of a dict of counts, sorted by key and joined with ", ".
fn by_method(value: &Value) -> String {
    let mut items: Vec<(&str, i128)> = value.as_dict().unwrap().iter().map(|(k, v)| (k.as_str(), v.as_int().unwrap())).collect();
    items.sort();
    items.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", ")
}

/// Python's repr of a dict of counts.
pub fn repr_counts(value: &Value) -> String {
    let items: Vec<String> = value.as_dict().unwrap().iter().map(|(k, v)| format!("'{k}': {}", v.as_int().unwrap())).collect();
    format!("{{{}}}", items.join(", "))
}

pub struct Inputs<'a> {
    pub raw: &'a [u8],
    pub raw_sha256: &'a str,
    pub entry_name: &'a str,
    pub max_rounds: usize,
    pub xnuprelink: &'a Path,
    pub signature: &'a str,
}

/// fileset_match.match_images: the map bound to the exact two input images. `original` is the kernelcache's
/// side (the same for the reference pass and the real one).
pub fn match_images(inputs: &Inputs, original: &Side, native_data: &[u8], reference_data: Option<&[u8]>, pure_leaves: bool) -> Result<Pairing> {
    println!("original: {} functions, {} diversities", original.functions.len(), original.diversity.len());
    let native_image = Image::new(native_data, 0)?;
    let (native_fixups, native_mapped) = (Arc::new(native_image.fixups()?), native_image.mapped());
    let native = Side::new(native_image, native_fixups, native_mapped)?;
    println!("native:   {} functions, {} diversities", native.functions.len(), native.diversity.len());
    let mut matcher = Matcher { original, native: &native, pairs: IndexMap::new(), reverse: HashMap::new(), conflicts: Vec::new() };
    let native_sha256 = sha256_hex(native_data);
    let records = reviewed_records(inputs.xnuprelink, inputs.raw_sha256, inputs.entry_name)?;
    println!("reviewed anchors: {}", matcher.anchor_reviewed(&records)?);
    let mut reference = None;
    if let Some(reference_data) = reference_data {
        // Pure-leaf binds carry behaviour, not identity, so the naming reference must not use them.
        let pairing = match_images(inputs, original, reference_data, None, false)?;
        let image = Image::new(reference_data, 0)?;
        let (fixups, mapped) = (Arc::new(image.fixups()?), image.mapped());
        let side = Side::new(image, fixups, mapped)?;
        println!("reference symbol anchors: {}", matcher.anchor_reference(&side, &pairing));
        println!("reference order anchors: {}", matcher.anchor_reference_order(&side, &pairing, 4)?);
        reference = Some((side, pairing));
    }
    println!("string anchors: {}", matcher.anchor_strings());
    println!("PAC checked shape anchors: {}", matcher.anchor_shapes(true));
    let mut data: Known = IndexMap::new();
    let mut split = 0;
    let kernel = &original.mapped;
    let targets: BTreeSet<u64> = original.fixups.iter().filter(|(site, fix)| within(kernel, fix.target) && !within(kernel, **site)).map(|(_, fix)| fix.target).collect();
    println!("reviewed data: {}", matcher.reviewed_data(&records, &mut data)?);
    if let Some((side, pairing)) = &reference {
        println!("reference data names: {}", matcher.reference_data(side, pairing, &targets, &mut data)?);
    }
    for _ in 0..inputs.max_rounds {
        let mut bound = matcher.anchor_shapes(false) + matcher.propagate();
        split = matcher.pair_data(&mut data)?;
        bound += matcher.pair_metaclass_getters(&data);
        bound += matcher.pair_metaclass_vtables(&mut data)?;
        bound += matcher.pair_vtable_users(&mut data);
        bound += matcher.pair_vtables(&mut data)?;
        bound += matcher.anchor_vtables(&data)?;
        bound += matcher.pair_calls()?;
        bound += matcher.pair_data_users(&data);
        bound += matcher.pair_reference_shapes(&data)?;
        if bound == 0 {
            break;
        }
    }
    if pure_leaves {
        println!("pure leaves: {}", matcher.pair_pure_leaves(&targets)?);
    }
    let slots = matcher.pair_data_slots(&targets, &mut data);
    let users = matcher.pair_data_by_users(&targets, &mut data)?;
    let sysctl = matcher.pair_sysctl_variables(&targets, &mut data);
    let fields = matcher.pair_data_fields(&targets, &mut data);
    println!("data slots: {slots}, data by users: {users}, sysctl: {sysctl}, fields: {fields}");

    // report()
    let mut sorted_pairs: Vec<(u64, u64, &'static str)> = matcher.pairs.iter().map(|(o, (n, h))| (*o, *n, *h)).collect();
    sorted_pairs.sort_unstable();
    let mut functions = Vec::with_capacity(sorted_pairs.len());
    for &(orig, n, how) in &sorted_pairs {
        let (name, addend) = native.name_at(n);
        let mut row = Value::dict();
        row.set("native", hex(n));
        row.set("name", name.map(Value::str));
        row.set("addend", Value::Int(addend as i128));
        row.set("how", how);
        functions.push((hex(orig), row));
    }
    let mut sorted_data: Vec<(u64, u64, &'static str)> = data.iter().map(|(o, (n, h))| (*o, *n, *h)).collect();
    sorted_data.sort_unstable();
    let mut data_rows = Vec::with_capacity(sorted_data.len());
    for &(orig, n, how) in &sorted_data {
        let mut row = Value::dict();
        row.set("native", hex(n));
        row.set("name", native.name_at(n).0.map(Value::str));
        row.set("how", how);
        row.set("sections", vec![original.section_of(orig), native.section_of(n)]);
        data_rows.push((hex(orig), row));
    }
    let named = targets.iter().filter(|t| matcher.pairs.contains_key(*t) || data.get(*t).is_some_and(|d| native.name_at(d.0).0.is_some())).count();
    let mut stats = Value::dict();
    stats.set("functions_original", Value::Int(original.functions.len() as i128));
    stats.set("functions_native", Value::Int(native.functions.len() as i128));
    stats.set("functions_matched", Value::Int(matcher.pairs.len() as i128));
    stats.set("functions_by_method", counter(matcher.pairs.values().map(|p| p.1)));
    stats.set("data_matched", Value::Int(data.len() as i128));
    stats.set("data_by_method", counter(data.values().map(|d| d.1)));
    stats.set("conflicts", counter(matcher.conflicts.iter().map(|c| c.get("kind").and_then(Value::as_str).unwrap())));
    stats.set("kext_targets", Value::Int(targets.len() as i128));
    stats.set("kext_targets_named", Value::Int(named as i128));
    stats.set("data_votes_split", Value::Int(split as i128));
    println!("functions: {}/{} ({})", matcher.pairs.len(), original.functions.len(), by_method(stats.get("functions_by_method").unwrap()));
    let methods = by_method(stats.get("data_by_method").unwrap());
    println!("data: {} ({})", data.len(), if methods.is_empty() { "none" } else { &methods });
    println!("kext-facing targets named: {named}/{}", targets.len());
    let conflicts = stats.get("conflicts").unwrap();
    println!("conflicts: {}", if conflicts.as_dict().unwrap().is_empty() { "none".to_string() } else { repr_counts(conflicts) });
    let mut payload = Value::dict();
    payload.set("matcher_version", Value::Int(MATCHER_VERSION));
    payload.set("matcher_signature", inputs.signature);
    payload.set("functions", Value::Dict(functions));
    payload.set("data", Value::Dict(data_rows));
    payload.set("stats", stats);
    payload.set("conflicts", Value::List(matcher.conflicts.iter().take(100).cloned().collect()));
    let mut record = Value::dict();
    record.set("kernelcache_sha256", inputs.raw_sha256);
    record.set("native_sha256", native_sha256);
    record.set("entry", inputs.entry_name);
    payload.set("inputs", record);
    if let Some(reference_data) = reference_data {
        payload.set("reference_sha256", sha256_hex(reference_data));
    }
    Ok(Pairing { functions: sorted_pairs, data: sorted_data.iter().map(|d| (d.0, d.1)).collect(), json: payload })
}

pub fn run(args: Args) -> Result<()> {
    let raw = oneios_common::Map::open(&args.kernelcache)?;
    let native = oneios_common::Map::open(&args.native)?;
    let reference = args.reference_kernel.as_ref().map(oneios_common::Map::open).transpose()?;
    let xnuprelink = Path::new(&args.xnuprelink);
    let cache = Image::new(&raw, 0)?;
    let entry = cache.entry(&args.entry)?;
    let fixups = Arc::new(cache.fixups()?);
    let mapped = entry.mapped();
    let original = Side::new(entry, fixups, mapped)?;
    let (raw_sha256, signature) = (sha256_hex(&raw), matcher_signature(xnuprelink)?);
    let inputs = Inputs { raw: &raw, raw_sha256: &raw_sha256, entry_name: &args.entry, max_rounds: args.max_rounds, xnuprelink, signature: &signature };
    let pairing = match_images(&inputs, &original, &native, reference.as_deref(), true)?;
    if let Some(out) = &args.out {
        std::fs::write(out, pyjson::dumps(&pairing.json, Some(1), false)).with_context(|| out.clone())?;
        println!("wrote {out}");
    }
    Ok(())
}
