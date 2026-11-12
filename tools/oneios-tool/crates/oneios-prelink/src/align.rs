//! `prelink align`: pair original kernel functions the matcher left unpaired, by link order against a reference
//! kernel. A port of kernel/prelink/align.py (its header has the method): the same aligned.json byte for byte and
//! the same line on stdout.
//!
//! Byte identity rests on three things of CPython, kept exactly: the holdout sample (`random.Random(2).sample` of
//! a list made by iterating a set of addresses: `pyset.rs`), the scores (IEEE doubles, the same operations in the
//! same order) and `round(score, 2)` with `repr`.

use crate::image::{hex, scan, u32_at, unhex, Image, Symbols};
use crate::pyset::{PyRandom, PySet};
use crate::records::{self, Counter};
use anyhow::{anyhow, Context, Result};
use oneios_common::pyjson::{self, Value};
use oneios_common::sha256_hex;
use std::collections::{BTreeMap, HashMap, HashSet};

const WINDOW: usize = 60;
const GAP: f64 = -0.4;

#[derive(clap::Args, Debug)]
pub struct Args {
    #[arg(long)]
    xnuprelink: String,
    #[arg(long)]
    kernelcache: String,
    #[arg(long)]
    reference: String,
    #[arg(long)]
    native: String,
    #[arg(long)]
    map: String,
    #[arg(long)]
    out: String,
    #[arg(long, default_value_t = 2.0)]
    threshold: f64,
    #[arg(long, default_value_t = 2500)]
    holdout: usize,
    /// also pair the one function between two anchors by position
    #[arg(long)]
    position: bool,
    /// kernel whose function of the same name gives the features, when --reference is only the order
    #[arg(long)]
    features: Option<String>,
    /// pairs of an earlier run (another order) or a reviewed file: read as paired
    #[arg(long)]
    also: Vec<String>,
    /// print the alignment of the window around this original function and stop
    #[arg(long)]
    explain: Option<String>,
}

/// A function's features: (the counted keys sorted by key, its size, its count of calls out).
type Features = (Vec<(String, u32)>, u64, usize);

fn features(image: &Image, start: u64, end: u64, call_name: &dyn Fn(u64) -> Option<String>, data_name: &dyn Fn(u64) -> Option<String>) -> Features {
    let Some(blob) = image.read(start, end - start) else { return (Vec::new(), end - start, 0) };
    let scanned = scan(blob, start);
    let (mut found, mut count): (BTreeMap<String, u32>, usize) = (BTreeMap::new(), 0);
    for (_, target, _) in &scanned.calls {
        if start <= *target && *target < end {
            continue;
        }
        count += 1;
        if let Some(name) = call_name(*target).filter(|n| !n.is_empty()) {
            *found.entry(format!("c:{name}")).or_default() += 1;
        }
    }
    let mut offset = 0;
    while offset + 3 < blob.len() {
        let word = u32_at(blob, offset);
        offset += 4;
        if word & 0x1f80_0000 == 0x1280_0000 && (word >> 29) & 3 != 1 {
            // movn/movz/movk
            let value = (word >> 5) & 0xffff;
            if value > 15 {
                *found.entry(format!("i:{value:x}")).or_default() += 1;
            }
        } else if word & 0x7f80_0000 == 0x7100_0000 && ((word >> 10) & 0xfff) > 8 {
            // cmp/subs #imm
            *found.entry(format!("k:{:x}", (word >> 10) & 0xfff)).or_default() += 1;
        } else if word & 0x3b00_0000 == 0x3900_0000 && !matches!((word >> 5) & 31, 29 | 31) {
            // ldr/str [xN, #offset]
            let value = (((word >> 10) & 0xfff) as u64) << (word >> 30);
            if value >= 16 {
                *found.entry(format!("o:{value:x}")).or_default() += 1;
            }
        }
    }
    for r in &scanned.refs {
        match image.cstring(r.target).ok().flatten().filter(|t| t.len() > 3) {
            Some(text) => *found.entry(format!("s:{text}")).or_default() += 1,
            None => {
                if let Some(name) = data_name(r.target).filter(|n| !n.is_empty()) {
                    *found.entry(format!("d:{name}")).or_default() += 1;
                }
            }
        }
    }
    (found.into_iter().collect(), end - start, count)
}

fn first_name(symbols: &Symbols, address: u64) -> Option<String> {
    symbols.get(&address).and_then(|names| names.iter().min().cloned())
}

/// The symbols inside the image's code segments, sorted.
fn code_starts(image: &Image, symbols: &Symbols) -> Vec<u64> {
    let code: Vec<(u64, u64)> = image.segments.iter().filter(|s| s.name.contains("EXEC")).map(|s| (s.va, s.va + s.size)).collect();
    let mut starts: Vec<u64> = symbols.keys().copied().filter(|a| code.iter().any(|(b, e)| b <= a && a < e)).collect();
    starts.sort_unstable();
    starts
}

/// Python's `round(x, 2)`.
fn round2(x: f64) -> f64 {
    format!("{x:.2}").parse().unwrap()
}

struct Shape<'a> {
    image: Image<'a>,
    symbols: Symbols,
    starts: Vec<u64>,
    by_name: HashMap<String, Vec<usize>>,
}

struct Aligner<'a> {
    kernel: Image<'a>,
    starts: Vec<u64>,
    index: HashMap<u64, usize>,
    name_of: HashMap<u64, String>,
    data_of: HashMap<u64, String>,
    reference: Image<'a>,
    symbols: Symbols,
    ref_starts: Vec<u64>,
    ref_index: HashMap<u64, usize>,
    by_name: HashMap<String, Vec<u64>>,
    shape: Option<Shape<'a>>,
    cache_o: HashMap<usize, Features>,
    cache_r: HashMap<usize, Features>,
    threshold: f64,
    position: bool,
}

/// What predict returns: (the reference function's names sorted, score, shared, union, before, after, kind).
type Found = (Vec<String>, f64, u32, u32, String, String, &'static str);

impl Aligner<'_> {
    fn ref_address(&self, name: &str) -> Option<u64> {
        self.by_name.get(name).filter(|f| f.len() == 1).map(|f| f[0])
    }

    fn original(&mut self, i: usize) {
        if !self.cache_o.contains_key(&i) {
            let end = self.starts.get(i + 1).copied().unwrap_or(self.starts[i] + 64);
            let f = features(&self.kernel, self.starts[i], end, &|a| self.name_of.get(&a).cloned(), &|a| self.data_of.get(&a).cloned());
            self.cache_o.insert(i, f);
        }
    }

    fn reference(&mut self, j: usize) {
        if self.cache_r.contains_key(&j) {
            return;
        }
        let other = self.shape.as_ref().and_then(|shape| {
            self.symbols[&self.ref_starts[j]].iter().find_map(|name| shape.by_name.get(name).filter(|p| p.len() == 1).map(|p| p[0]))
        });
        let f = match (other, &self.shape) {
            (Some(at), Some(shape)) => {
                let end = shape.starts.get(at + 1).copied().unwrap_or(shape.starts[at] + 64);
                let first = |a: u64| first_name(&shape.symbols, a);
                features(&shape.image, shape.starts[at], end, &first, &first)
            }
            _ => {
                let end = self.ref_starts.get(j + 1).copied().unwrap_or(self.ref_starts[j] + 64);
                let first = |a: u64| first_name(&self.symbols, a);
                features(&self.reference, self.ref_starts[j], end, &first, &first)
            }
        };
        self.cache_r.insert(j, f);
    }

    fn similarity(&mut self, i: usize, j: usize, hidden: &HashSet<String>) -> (f64, u32, u32) {
        self.original(i);
        self.reference(j);
        let ((fo, so, co), (fr, sr, cr)) = (&self.cache_o[&i], &self.cache_r[&j]);
        let (mut shared, mut union, mut meta_only_here) = (0u32, 0u32, false);
        let (mut a, mut b) = (0, 0);
        let fo: Vec<&(String, u32)> = fo.iter().filter(|(k, _)| hidden.is_empty() || !hidden.contains(&k[2..])).collect();
        while a < fo.len() || b < fr.len() {
            let order = match (fo.get(a), fr.get(b)) {
                (Some(x), Some(y)) => x.0.cmp(&y.0),
                (Some(_), None) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Greater,
            };
            match order {
                std::cmp::Ordering::Equal => {
                    shared += fo[a].1.min(fr[b].1);
                    union += fo[a].1.max(fr[b].1);
                    a += 1;
                    b += 1;
                }
                std::cmp::Ordering::Less => {
                    union += fo[a].1;
                    // A function that names a class's metaclass is a member of that class.
                    meta_only_here |= fo[a].0.starts_with("d:") && fo[a].0.ends_with("10gMetaClassE");
                    a += 1;
                }
                std::cmp::Ordering::Greater => {
                    union += fr[b].1;
                    b += 1;
                }
            }
        }
        let mut score = (if union != 0 { 2.0 * shared as f64 / union as f64 } else { 0.0 }) + 1.0 - so.abs_diff(*sr) as f64 / (*so).max(*sr) as f64 + (if co == cr { 0.3 } else { 0.0 });
        if meta_only_here {
            score = score.min(self.threshold - 0.01);
        }
        (score, shared, union)
    }

    fn anchor(&self, at: usize, hidden: &HashSet<u64>) -> Option<usize> {
        let start = self.starts[at];
        if hidden.contains(&start) {
            return None;
        }
        self.name_of.get(&start).and_then(|name| self.ref_address(name)).map(|a| self.ref_index[&a])
    }

    fn predict(&mut self, target: u64, hidden: &HashSet<u64>, explain: bool) -> Option<Found> {
        let i = *self.index.get(&target)?;
        // The nearest anchors whose reference positions are in the same order and about as far apart.
        let (mut before, mut after) = (Vec::new(), Vec::new());
        let mut at = i;
        while at > 0 && i - (at - 1) <= WINDOW && before.len() < 5 {
            at -= 1;
            if let Some(r) = self.anchor(at, hidden) {
                before.push((at, r));
            }
        }
        let mut at = i + 1;
        while at < self.starts.len() && at - i <= WINDOW && after.len() < 5 {
            if let Some(r) = self.anchor(at, hidden) {
                after.push((at, r));
            }
            at += 1;
        }
        let mut windows: Vec<((usize, usize), (usize, usize))> = before.iter().flat_map(|a| after.iter().map(move |b| (*a, *b))).collect();
        windows.sort_by_key(|(a, b)| b.0 - a.0);
        let ((a, ra), (b, rb)) = windows.into_iter().find(|((a, ra), (b, rb))| b - a <= WINDOW && *rb > ra + 1 && rb - ra <= 3 * (b - a) + 8)?;
        let (left, right): (Vec<usize>, Vec<usize>) = ((a + 1..b).collect(), (ra + 1..rb).collect());
        let hidden_names: HashSet<String> = hidden.iter().filter_map(|h| self.name_of.get(h).cloned()).collect();
        let (n, k) = (left.len(), right.len());
        let mut cost = vec![vec![0.0f64; k + 1]; n + 1];
        let mut back = vec![vec![0u8; k + 1]; n + 1];
        for x in 1..=n {
            (cost[x][0], back[x][0]) = (x as f64 * GAP, 1);
        }
        for y in 1..=k {
            (cost[0][y], back[0][y]) = (y as f64 * GAP, 2);
        }
        let mut scores = vec![vec![(0.0f64, 0u32, 0u32); k + 1]; n + 1];
        for x in 1..=n {
            for y in 1..=k {
                scores[x][y] = self.similarity(left[x - 1], right[y - 1], &hidden_names);
                let choices = [cost[x - 1][y - 1] + scores[x][y].0 - 1.0, cost[x - 1][y] + GAP, cost[x][y - 1] + GAP];
                let mut best = 0;
                for c in 1..3 {
                    if choices[c] > choices[best] {
                        best = c;
                    }
                }
                (cost[x][y], back[x][y]) = (choices[best], best as u8);
            }
        }
        let (mut x, mut y) = (n, k);
        let name = |s: &Aligner, at: usize| s.name_of[&s.starts[at]].clone();
        if explain {
            let none = || "None".to_string();
            let mut rows: Vec<Vec<String>> = Vec::new();
            while x > 0 || y > 0 {
                let orig = |s: &mut Aligner, x: usize| {
                    s.original(left[x - 1]);
                    vec![hex(s.starts[left[x - 1]]), s.name_of.get(&s.starts[left[x - 1]]).cloned().unwrap_or_else(none), s.cache_o[&left[x - 1]].1.to_string()]
                };
                let refr = |s: &mut Aligner, y: usize| {
                    s.reference(right[y - 1]);
                    vec![first_name(&s.symbols, s.ref_starts[right[y - 1]]).unwrap_or_else(none), s.cache_r[&right[y - 1]].1.to_string()]
                };
                match back[x][y] {
                    0 => {
                        let mut row = orig(self, x);
                        row.extend(refr(self, y));
                        row.push(pyjson::float_repr(round2(scores[x][y].0)));
                        rows.push(row);
                        (x, y) = (x - 1, y - 1);
                    }
                    1 => {
                        let mut row = orig(self, x);
                        row.extend(["-".to_string(), String::new(), String::new()]);
                        rows.push(row);
                        x -= 1;
                    }
                    _ => {
                        let mut row = vec!["-".to_string(), String::new(), String::new()];
                        row.extend(refr(self, y));
                        row.push(String::new());
                        rows.push(row);
                        y -= 1;
                    }
                }
            }
            println!("between {} and {}: {n} original, {k} reference functions", name(self, a), name(self, b));
            for row in rows.iter().rev() {
                println!("  {}", row.join(" "));
            }
            return None;
        }
        // the anchors, then every pair at or over the threshold
        let (mut strong, mut mine) = (vec![(0, 0), (n + 1, k + 1)], None);
        while x > 0 || y > 0 {
            match back[x][y] {
                0 => {
                    if left[x - 1] == i {
                        mine = Some((x, y));
                    }
                    if scores[x][y].0 >= self.threshold {
                        strong.push((x, y));
                    }
                    (x, y) = (x - 1, y - 1);
                }
                1 => x -= 1,
                _ => y -= 1,
            }
        }
        let result = |s: &Aligner, y: usize, score: (f64, u32, u32), kind: &'static str| -> Found {
            let mut names = s.symbols[&s.ref_starts[right[y - 1]]].clone();
            names.sort();
            (names, score.0, score.1, score.2, name(s, a), name(s, b), kind)
        };
        if let Some((mx, my)) = mine {
            if scores[mx][my].0 >= self.threshold {
                return Some(result(self, my, scores[mx][my], "features"));
            }
        }
        // Too little in the function itself (a thunk, an accessor): its position counts only when it is the one
        // function between two anchors on both sides.
        let x = left.iter().position(|l| *l == i).unwrap() + 1;
        let below = *strong.iter().filter(|pair| pair.0 < x).max().unwrap();
        if self.position && n == 1 && k == 1 {
            let y = below.1 + x - below.0;
            return Some(result(self, y, scores[x][y], "position"));
        }
        mine.map(|(mx, my)| result(self, my, scores[mx][my], "weak"))
    }
}

pub fn run(args: Args) -> Result<()> {
    let _ = &args.xnuprelink; // Python imports kc_image from it; the reader is image.rs here
    let raw = oneios_common::Map::open(&args.kernelcache)?;
    let pairing = pyjson::load(&args.map)?;
    let cache = Image::new(&raw, 0)?;
    let kernel = cache.entry("com.apple.kernel")?;
    let kernel_ranges = kernel.mapped();
    let mut starts = kernel.function_starts()?;
    starts.sort_unstable();
    let index: HashMap<u64, usize> = starts.iter().enumerate().map(|(i, a)| (*a, i)).collect();
    let named = records::named(&pairing, "functions")?;
    let mut name_of: HashMap<u64, String> = named.iter().cloned().collect();
    let data_of: HashMap<u64, String> = records::named(&pairing, "data")?.into_iter().collect();
    // `matched = set(name_of)`: the set's own order decides the holdout sample.
    let matched = PySet::from_dict_keys(named.iter().map(|n| n.0));
    for path in &args.also {
        let record = pyjson::load(path)?; // align's own output, or a reviewed file
        if record.get("pairs").is_some() {
            for (original, symbol) in records::pairs(&record, "pairs")? {
                name_of.entry(original).or_insert(symbol);
            }
        } else {
            name_of.extend(records::pairs(&record, "functions")?);
        }
    }
    let reference_raw = oneios_common::Map::open(&args.reference)?;
    let reference = Image::new(&reference_raw, 0)?;
    let symbols = reference.symbols()?;
    let ref_starts = code_starts(&reference, &symbols);
    let ref_index: HashMap<u64, usize> = ref_starts.iter().enumerate().map(|(i, a)| (*a, i)).collect();
    let mut by_name: HashMap<String, Vec<u64>> = HashMap::new();
    for address in &ref_starts {
        for name in &symbols[address] {
            by_name.entry(name.clone()).or_default().push(*address);
        }
    }
    let native_raw = oneios_common::Map::open(&args.native)?;
    let mut native_names: HashMap<String, u32> = HashMap::new();
    for names in Image::new(&native_raw, 0)?.symbols()?.into_values() {
        for name in names {
            *native_names.entry(name).or_default() += 1;
        }
    }
    let shape_raw = args.features.as_ref().map(oneios_common::Map::open).transpose()?;
    let shape = match &shape_raw {
        Some(data) => {
            let image = Image::new(data, 0)?;
            let symbols = image.symbols()?;
            let starts = code_starts(&image, &symbols);
            let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
            for (position, address) in starts.iter().enumerate() {
                for name in &symbols[address] {
                    by_name.entry(name.clone()).or_default().push(position);
                }
            }
            Some(Shape { image, symbols, starts, by_name })
        }
        None => None,
    };
    let mut aligner = Aligner {
        kernel,
        starts,
        index,
        name_of,
        data_of,
        reference,
        symbols,
        ref_starts,
        ref_index,
        by_name,
        shape,
        cache_o: HashMap::new(),
        cache_r: HashMap::new(),
        threshold: args.threshold,
        position: args.position,
    };
    if let Some(explain) = &args.explain {
        let target = unhex(explain)?;
        if aligner.predict(target, &HashSet::new(), true).is_none() && !aligner.index.contains_key(&target) {
            println!("not a function start of the original kernel");
        }
        return Ok(());
    }
    let known: Vec<u64> = matched.iter().filter(|a| aligner.index.contains_key(a)).collect();
    // (kind, right) -> count
    let mut tally: HashMap<(&str, bool), i128> = HashMap::new();
    for address in PyRandom::new(2).sample(&known, args.holdout.min(known.len())) {
        if let Some(found) = aligner.predict(address, &HashSet::from([address]), false) {
            if found.6 != "weak" {
                *tally.entry((found.6, found.0.contains(&aligner.name_of[&address]))).or_default() += 1;
            }
        }
    }
    let mut taken: HashSet<String> = aligner.name_of.values().cloned().collect();
    let mut targets: Vec<u64> = cache
        .fixups()?
        .iter()
        .filter(|(site, fix)| aligner.index.contains_key(&fix.target) && !aligner.name_of.contains_key(&fix.target) && !kernel_ranges.iter().any(|(b, e)| b <= *site && *site < e))
        .map(|(_, fix)| fix.target)
        .collect();
    targets.sort_unstable();
    targets.dedup();
    let (mut pairs, mut rounds): (Vec<(u64, String, Value)>, i128) = (Vec::new(), 0);
    let mut refused;
    loop {
        // each new pair is an anchor and a named callee for the next round
        rounds += 1;
        let mut added = 0;
        refused = Counter::default();
        for &target in &targets {
            if aligner.name_of.contains_key(&target) {
                continue;
            }
            let Some((names, score, shared, union, before, after, kind)) = aligner.predict(target, &HashSet::new(), false) else {
                refused.add("no alignment", 1);
                continue;
            };
            if kind == "weak" {
                refused.add("below the threshold", 1);
                continue;
            }
            let Some(usable) = names.iter().find(|name| native_names.get(*name) == Some(&1) && !taken.contains(*name)) else {
                refused.add("name absent, ambiguous or already paired", 1);
                continue;
            };
            taken.insert(usable.clone());
            let mut row = Value::dict();
            row.set("original", hex(target));
            row.set("native_symbol", usable);
            row.set("score", round2(score));
            row.set("shared_features", Value::Int(shared as i128));
            row.set("features", Value::Int(union as i128));
            row.set("between", vec![before, after]);
            row.set("by", kind);
            row.set("round", Value::Int(rounds));
            pairs.push((target, usable.clone(), row));
            added += 1;
        }
        if added == 0 || rounds == 8 {
            break;
        }
        for (original, symbol, _) in &pairs {
            aligner.name_of.insert(*original, symbol.clone());
        }
        aligner.cache_o.clear();
    }
    let reference_name = std::path::Path::new(&args.reference).file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| anyhow!("--reference names no file"))?;
    let count = |kind: &'static str, right: bool| tally.get(&(kind, right)).copied().unwrap_or(0);
    let mut holdout = Value::dict();
    for kind in ["features", "position"] {
        let mut row = Value::dict();
        row.set("right", Value::Int(count(kind, true)));
        row.set("wrong", Value::Int(count(kind, false)));
        holdout.set(kind, row);
    }
    let mut record = Value::dict();
    record.set("kernelcache_sha256", sha256_hex(&raw));
    record.set("reference_sha256", sha256_hex(&reference_raw));
    record.set("threshold", args.threshold);
    record.set("holdout", holdout);
    record.set("order_of", &reference_name);
    record.set("rounds", Value::Int(rounds));
    record.set("unpaired_kext_targets", Value::Int(targets.len() as i128));
    record.set("refused", refused.to_value());
    let total = pairs.len();
    record.set("pairs", Value::List(pairs.into_iter().map(|p| p.2).collect()));
    std::fs::write(&args.out, pyjson::dumps(&record, Some(1), false) + "\n").with_context(|| args.out.clone())?;
    println!(
        "align: {total} of {} unpaired kext-facing functions paired by the order of {reference_name} in {rounds} round(s) (holdout: features {} wrong of {}, position {} wrong of {}); refused: {}",
        targets.len(),
        count("features", false),
        count("features", true) + count("features", false),
        count("position", false),
        count("position", true) + count("position", false),
        refused.repr()
    );
    Ok(())
}
