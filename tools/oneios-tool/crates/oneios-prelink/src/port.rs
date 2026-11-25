//! `oneios-tool prelink port`: carry the records that name addresses of one kernelcache (FROM, e.g. iOS 27.0.1
//! 24A446's) over to another build of the same device and XNU branch (TO, e.g. iOS 27.2.1 24B156's), and check
//! every carried address against TO's own bytes.
//!
//!   oneios-tool prelink port --from KC_A --to KC_B [--map MAP.json]
//!       [--record IN.json=OUT.json ...] [--report REPORT.json] [--strict]
//!
//! Why: kernel.kc's reviewed records (`pairs-BUILD.json`, `stubs-BUILD.json`, `reviewed-BUILD-v54.json`, the lead's
//! and fable-user's), and the board archives' layouts, name Apple's functions and data by their address in one
//! kernelcache. A point release of the same XNU branch (24A446 is xnu-13432.2.10, 24B156 xnu-13432.42.3) moves almost
//! every address but keeps almost every function: the same code, other PC-relative immediates.
//!
//! How a FROM function finds its TO function (in this order; an earlier pair is never replaced, and a TO function
//! is taken once):
//!
//!   shape   per fileset entry, the function's relocation-masked shape (`image::scan`: branch, ADRP, ADD and
//!           load/store immediates to other code or data taken out) and length are unique in FROM and in TO
//!   walk    two paired functions of the same shape are the same instructions: their calls (BL/B out of the
//!           function) go to paired functions, call by call, and their ADRP-based references to paired data
//!   slot    two paired data words that are both chained-fixup pointers (an import slot of `__auth_got`, a
//!           vtable entry) point at paired targets
//!   order   between two paired functions of one fileset entry, FROM and TO have as many unpaired functions, of
//!           lengths within a quarter of each other: they pair in link order (the weakest rule; counted apart)
//!
//! A data address pairs when the walks agree on it (every vote for one TO address). Each round of `walk`/`slot`
//! runs again over the pairs the last round made, until nothing is added; then `order`, then the walks again.
//!
//! An address inside a function (a call site, an instruction a record checks) carries over at the same offset when
//! the two functions have the same shape, else at the instruction the longest common subsequence of the two shapes
//! puts it on (`same: false`: such a record is marked for review).
//!
//! A data address no walk voted for carries over when it lies up to 64 bytes before one that did and the bytes
//! between are the same in FROM and TO (a vtable's symbol, 16 zero bytes before the slots its constructor stores);
//! such a record is marked for review.
//!
//! A data address still not carried that points to its own name (a pointer at +0..+24 to a C string of 4 to 128
//! printable bytes: a `vnodeop_desc`'s `vdesc_name`) goes to the one TO address whose pointer of the same kind at the
//! same offset points to the same string (none or several: not carried); marked for review.
//!
//! `--fields KEY` (with a record that has `"family_text": [low, high]`, the display record): every integer of the
//! record's object KEY is a struct field offset. Each FROM instruction of the paired functions in [low, high) that
//! reaches one of them (LDR/STR, LDUR/STUR, LDP/STP or ADD immediate off a register other than SP) votes for the
//! offset its TO instruction uses (the same instruction when the shapes are the same; else the one the longest common
//! subsequence of the two shapes, field offsets masked, puts it on). A field with votes for several offsets, or none,
//! fails, unless `--fields-loose` (the most voted; none: unchanged). The votes are printed and in the report.
//!
//! `--record IN=OUT` rewrites a JSON record: every string that is an address of FROM's mapped segments (and every
//! `linked` one, FROM's address plus the collection slide 0x1f000000000) becomes TO's, FROM's kernelcache SHA-256
//! becomes TO's, and every check object (`{"address", "size", "sha256"}`, or `[address, size, sha256]`) is read again
//! from TO (a whole-function check takes TO's function length). An element of a top-level list (one record) any of
//! whose addresses does not carry over is left out of OUT and listed in the report with the address; one carried
//! over only through `order` or a changed shape is kept and gets `"port": "review: ..."`. Nothing is guessed: an
//! address carries over by one of the rules above or not at all. `--strict` fails when a record is left out.
//!
//! The map (`--map`) holds addresses only (no Apple code). It is a work file of out/, not committed.

use std::collections::{BTreeMap, HashMap, VecDeque};

use anyhow::{anyhow, bail, Context, Result};
use oneios_common::json::{self, Value};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use crate::image::{hex, scan, unhex, Fixups, Image};

/// The kernel collection's slide: an original address plus this is the address in kernel.kc (classes.rs "linked").
const LINKED_SLIDE: u64 = 0x1f0_0000_0000;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// The kernelcache the records are of (decoded Mach-O)
    #[arg(long)]
    from: String,
    /// The kernelcache to carry them over to
    #[arg(long)]
    to: String,
    /// Write the function and data map here
    #[arg(long)]
    map: Option<String>,
    /// IN.json=OUT.json: a record to carry over (repeatable)
    #[arg(long = "record")]
    records: Vec<String>,
    /// What was carried, marked and left out, per record file
    #[arg(long)]
    report: Option<String>,
    /// Fail when a record is left out
    #[arg(long)]
    strict: bool,
    /// KEY: in a record with "family_text" [low, high], the object KEY of struct field offsets is read again from
    /// TO's code (repeatable; the display record's "display" and "request")
    #[arg(long = "fields")]
    fields: Vec<String>,
    /// With --fields: take the most voted offset when TO's code uses several
    #[arg(long = "fields-loose")]
    fields_loose: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum How {
    Shape,
    Walk,
    Slot,
    Order,
}

impl How {
    fn name(self) -> &'static str {
        match self {
            How::Shape => "shape",
            How::Walk => "walk",
            How::Slot => "slot",
            How::Order => "order",
        }
    }
}

/// One function of one side: [start, end) in a fileset entry's __TEXT_EXEC, its masked shape and what it references.
struct Function {
    start: u64,
    end: u64,
    entry: usize,
    key: [u8; 32],
    shape: Vec<u32>,
    /// branch targets outside the function, in instruction order
    calls: Vec<u64>,
    /// ADRP-based reference targets, in instruction order
    refs: Vec<u64>,
}

struct Side<'a> {
    image: Image<'a>,
    fixups: Fixups,
    functions: Vec<Function>,
    /// start -> index in `functions`
    by_start: HashMap<u64, usize>,
    /// (entry, start) sorted, for "which function holds this address"
    sorted: Vec<(u64, usize)>,
    entries: Vec<String>,
    sha256: String,
}

impl<'a> Side<'a> {
    fn new(raw: &'a [u8], names: &[String]) -> Result<Side<'a>> {
        let image = Image::new(raw, 0)?;
        let fixups = image.fixups()?;
        let mut spans: Vec<(usize, u64, u64)> = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let entry = image.entry(name)?;
            // a function ends at the next start or at the end of its section; each import stub (16 bytes in
            // __auth_stubs, 12 in __stubs) is a function of its own, so that calls through stubs pair too
            let mut texts: Vec<(u64, u64)> = entry.sections.iter().filter(|s| s.segment == "__TEXT_EXEC" && s.size != 0).map(|s| (s.va, s.va + s.size)).collect();
            if texts.is_empty() {
                texts = entry.segments.iter().filter(|s| s.name == "__TEXT_EXEC" && s.filesize != 0).map(|s| (s.va, s.va + s.filesize)).collect();
            }
            let mut starts = entry.function_starts()?;
            for s in entry.sections.iter().filter(|s| s.segment == "__TEXT_EXEC") {
                let step = match s.name.as_str() {
                    "__auth_stubs" => 16,
                    "__stubs" => 12,
                    _ => continue,
                };
                starts.extend((0..s.size / step).map(|i| s.va + i * step));
            }
            starts.sort_unstable();
            starts.dedup();
            for (low, high) in texts {
                let inside: Vec<u64> = starts.iter().copied().filter(|&a| low <= a && a < high).collect();
                for (i, &start) in inside.iter().enumerate() {
                    let end = inside.get(i + 1).copied().unwrap_or(high);
                    if end > start {
                        spans.push((index, start, end));
                    }
                }
            }
        }
        let functions: Vec<Function> = spans
            .par_iter()
            .filter_map(|&(entry, start, end)| {
                let code = image.read(start, end - start)?;
                let scanned = scan(code, start);
                let mut hasher = Sha256::new();
                hasher.update((entry as u64).to_le_bytes());
                for word in &scanned.shape {
                    hasher.update(word.to_le_bytes());
                }
                let calls = scanned.calls.iter().filter(|c| c.1 < start || c.1 >= end).map(|c| c.1).collect();
                let refs = scanned.refs.iter().map(|r| r.target).collect();
                Some(Function { start, end, entry, key: hasher.finalize().into(), shape: scanned.shape, calls, refs })
            })
            .collect();
        let by_start = functions.iter().enumerate().map(|(i, f)| (f.start, i)).collect();
        let mut sorted: Vec<(u64, usize)> = functions.iter().enumerate().map(|(i, f)| (f.start, i)).collect();
        sorted.sort_unstable();
        Ok(Side { image, fixups, functions, by_start, sorted, entries: names.to_vec(), sha256: oneios_common::sha256_hex(raw) })
    }

    /// The function holding `address`.
    fn holding(&self, address: u64) -> Option<usize> {
        let at = self.sorted.partition_point(|&(start, _)| start <= address);
        let (_, index) = *self.sorted.get(at.checked_sub(1)?)?;
        (address < self.functions[index].end).then_some(index)
    }

    fn mapped(&self, address: u64) -> bool {
        self.image.segments.iter().any(|s| s.name != "__LINKEDIT" && s.size != 0 && s.va <= address && address < s.va + s.size)
    }
}

/// FROM index -> TO index for functions; FROM address -> TO address for data.
struct Map {
    function: HashMap<usize, (usize, How)>,
    taken: HashMap<usize, usize>,
    data_votes: HashMap<u64, BTreeMap<u64, u32>>,
    conflicts: u64,
}

impl Map {
    fn pair(&mut self, a: usize, b: usize, how: How, queue: &mut VecDeque<usize>) -> bool {
        match (self.function.get(&a), self.taken.get(&b)) {
            (None, None) => {
                self.function.insert(a, (b, how));
                self.taken.insert(b, a);
                queue.push_back(a);
                true
            }
            (Some(&(had, _)), _) if had == b => false,
            _ => {
                self.conflicts += 1;
                false
            }
        }
    }

    /// The data address `a` pairs with: the one TO address every vote names.
    fn data(&self, a: u64) -> Option<u64> {
        let votes = self.data_votes.get(&a)?;
        (votes.len() == 1).then(|| *votes.keys().next().unwrap())
    }
}

fn walk(from: &Side, to: &Side, map: &mut Map, queue: &mut VecDeque<usize>) {
    while let Some(a) = queue.pop_front() {
        let (b, _) = map.function[&a];
        let (fa, fb) = (&from.functions[a], &to.functions[b]);
        if fa.key != fb.key || fa.calls.len() != fb.calls.len() || fa.refs.len() != fb.refs.len() {
            continue;
        }
        for (&ta, &tb) in fa.calls.iter().zip(&fb.calls) {
            if let (Some(&ia), Some(&ib)) = (from.by_start.get(&ta), to.by_start.get(&tb)) {
                map.pair(ia, ib, How::Walk, queue);
            }
        }
        for (&ta, &tb) in fa.refs.iter().zip(&fb.refs) {
            match (from.by_start.get(&ta), to.by_start.get(&tb)) {
                (Some(&ia), Some(&ib)) => {
                    map.pair(ia, ib, How::Walk, queue);
                }
                _ => {
                    *map.data_votes.entry(ta).or_default().entry(tb).or_insert(0) += 1;
                    // a pointer slot (an import of __auth_got, a vtable entry): its target pairs too
                    if let (Some(pa), Some(pb)) = (from.fixups.get(&ta), to.fixups.get(&tb)) {
                        match (from.by_start.get(&pa.target), to.by_start.get(&pb.target)) {
                            (Some(&ia), Some(&ib)) => {
                                map.pair(ia, ib, How::Slot, queue);
                            }
                            _ => *map.data_votes.entry(pa.target).or_default().entry(pb.target).or_insert(0) += 1,
                        }
                    }
                }
            }
        }
    }
}

/// Between paired neighbours of one entry: as many unpaired on both sides, lengths close: pair them in order.
fn order(from: &Side, to: &Side, map: &mut Map, queue: &mut VecDeque<usize>) -> usize {
    let mut added = 0;
    for entry in 0..from.entries.len() {
        let list = |side: &Side| -> Vec<usize> {
            let mut v: Vec<usize> = side.functions.iter().enumerate().filter(|(_, f)| f.entry == entry).map(|(i, _)| i).collect();
            v.sort_by_key(|&i| side.functions[i].start);
            v
        };
        let (la, lb) = (list(from), list(to));
        let pos_b: HashMap<usize, usize> = lb.iter().enumerate().map(|(p, &i)| (i, p)).collect();
        // anchors: paired functions in increasing order on both sides
        let mut anchors: Vec<(usize, usize)> = vec![];
        let mut last_b: isize = -1;
        for (pa, &ia) in la.iter().enumerate() {
            if let Some(&(ib, _)) = map.function.get(&ia) {
                if let Some(&pb) = pos_b.get(&ib) {
                    if pb as isize > last_b {
                        anchors.push((pa, pb));
                        last_b = pb as isize;
                    }
                }
            }
        }
        let mut bounds = vec![(-1isize, -1isize)];
        bounds.extend(anchors.iter().map(|&(a, b)| (a as isize, b as isize)));
        bounds.push((la.len() as isize, lb.len() as isize));
        for w in bounds.windows(2) {
            let ((a0, b0), (a1, b1)) = (w[0], w[1]);
            let gap_a: Vec<usize> = ((a0 + 1)..a1).map(|p| la[p as usize]).filter(|i| !map.function.contains_key(i)).collect();
            let gap_b: Vec<usize> = ((b0 + 1)..b1).map(|p| lb[p as usize]).filter(|i| !map.taken.contains_key(i)).collect();
            if gap_a.is_empty() || gap_a.len() != gap_b.len() {
                continue;
            }
            let close = gap_a.iter().zip(&gap_b).all(|(&ia, &ib)| {
                let (x, y) = (from.functions[ia].end - from.functions[ia].start, to.functions[ib].end - to.functions[ib].start);
                4 * x.abs_diff(y) <= x.max(y)
            });
            if !close {
                continue;
            }
            for (&ia, &ib) in gap_a.iter().zip(&gap_b) {
                if map.pair(ia, ib, How::Order, queue) {
                    added += 1;
                }
            }
        }
    }
    added
}

/// Instruction index in FROM's function -> instruction index in TO's (the longest common subsequence of the shapes).
fn lcs(a: &[u32], b: &[u32]) -> Option<HashMap<usize, usize>> {
    let (n, m) = (a.len(), b.len());
    if n.saturating_mul(m) > 40_000_000 {
        return None;
    }
    let mut table = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * (m + 1) + j] = if a[i] == b[j] { table[(i + 1) * (m + 1) + j + 1] + 1 } else { table[(i + 1) * (m + 1) + j].max(table[i * (m + 1) + j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, HashMap::new());
    while i < n && j < m {
        if a[i] == b[j] {
            out.insert(i, j);
            i += 1;
            j += 1;
        } else if table[(i + 1) * (m + 1) + j] >= table[i * (m + 1) + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    Some(out)
}

/// Where a FROM address is in TO, and whether by a rule a reviewer need not look at again.
#[derive(Clone, Debug)]
struct Carried {
    to: u64,
    sure: bool,
    how: String,
}

struct Porter<'a> {
    from: Side<'a>,
    to: Side<'a>,
    map: Map,
}

impl<'a> Porter<'a> {
    fn carry(&self, a: u64) -> Option<Carried> {
        if let Some(&ia) = self.from.by_start.get(&a) {
            let &(ib, how) = self.map.function.get(&ia)?;
            let same = self.from.functions[ia].key == self.to.functions[ib].key;
            return Some(Carried { to: self.to.functions[ib].start, sure: how != How::Order, how: format!("{}{}", how.name(), if same { "" } else { ", shape changed" }) });
        }
        if let Some(ia) = self.from.holding(a) {
            let &(ib, how) = self.map.function.get(&ia)?;
            let (fa, fb) = (&self.from.functions[ia], &self.to.functions[ib]);
            let offset = a - fa.start;
            if fa.key == fb.key {
                return Some(Carried { to: fb.start + offset, sure: how != How::Order, how: format!("{} (+{offset:#x})", how.name()) });
            }
            if offset % 4 != 0 {
                return None;
            }
            let at = lcs(&fa.shape, &fb.shape)?.get(&(offset as usize / 4)).copied()?;
            return Some(Carried { to: fb.start + 4 * at as u64, sure: false, how: format!("{} (+{offset:#x}), shape changed: the instruction the two shapes have in common", how.name()) });
        }
        if let Some(b) = self.map.data(a) {
            return Some(Carried { to: b, sure: true, how: "data".into() });
        }
        // near: a data address no walk voted for (a vtable symbol, 16 bytes before the slots the constructor
        // stores) up to 64 bytes before a paired one, the words between the same on both sides: the same bytes, or
        // both chained-fixup pointers of the same kind to paired targets
        for d in (8..=64u64).step_by(8) {
            let Some(q) = self.map.data(a + d) else { continue };
            let b = q.checked_sub(d)?;
            let same = (0..d).step_by(8).all(|w| match (self.from.fixups.get(&(a + w)), self.to.fixups.get(&(b + w))) {
                (Some(x), Some(y)) => x.meta() == y.meta() && self.pairs(x.target, y.target),
                (None, None) => self.from.image.read(a + w, 8).is_some() && self.from.image.read(a + w, 8) == self.to.image.read(b + w, 8),
                _ => false,
            });
            if same {
                return Some(Carried { to: b, sure: false, how: format!("data, {d} bytes before the pair {} -> {}, the words between the same", hex(a + d), hex(q)) });
            }
            break; // the nearest pair only
        }
        self.named_by_content(a)
    }

    /// named: a data object that points to its own name (a `vnodeop_desc`'s `vdesc_name` at +8, "vnop_searchfs"): the
    /// one TO address whose pointer at the same offset, of the same kind, points to the same C string. How the
    /// 24A446 reviewers named these objects by hand (reviewed-24A446-v54.json `_vnop_searchfs_desc`); marked for review.
    fn named_by_content(&self, a: u64) -> Option<Carried> {
        for w in (0..32u64).step_by(8) {
            let Some(fix) = self.from.fixups.get(&(a + w)) else { continue };
            let Some(name) = cstr(&self.from.image, fix.target) else { continue };
            let mut found = self.to.fixups.iter().filter(|(_, y)| y.meta() == fix.meta() && cstr(&self.to.image, y.target) == Some(name)).map(|(&site, _)| site);
            let (Some(site), None) = (found.next(), found.next()) else { return None };
            let b = site.checked_sub(w)?;
            return Some(Carried { to: b, sure: false, how: format!("data, named by its content: the one pointer to \"{}\" at +{w}", String::from_utf8_lossy(name)) });
        }
        None
    }

    /// Whether FROM's `a` and TO's `b` are a pair (two functions' starts, or two data addresses).
    fn pairs(&self, a: u64, b: u64) -> bool {
        match (self.from.by_start.get(&a), self.to.by_start.get(&b)) {
            (Some(ia), Some(ib)) => self.map.function.get(ia).is_some_and(|&(j, _)| j == *ib),
            _ => self.map.data(a) == Some(b),
        }
    }
}

fn is_address(text: &str) -> Option<u64> {
    let t = text.strip_prefix("0x")?;
    // an original address (0xfffffe...) or a linked one (0xfffffff0...: original + LINKED_SLIDE). "fffff0" until
    // 2026-11-25 matched neither, so the layouts' "linked" addresses kept FROM's values.
    if t.len() != 16 || !t.starts_with("fffffe") && !t.starts_with("fffffff0") {
        return None;
    }
    u64::from_str_radix(t, 16).ok()
}

/// One record's walk: every address string rewritten; what could not be carried, and what is not sure.
#[derive(Default)]
struct Walked {
    missing: Vec<String>,
    review: Vec<String>,
    checks: u64,
}

impl<'a> Porter<'a> {
    fn rewrite(&self, value: &mut Value, out: &mut Walked) -> Result<()> {
        match value {
            Value::Str(text) => {
                if *text == self.from.sha256 {
                    *text = self.to.sha256.clone();
                    return Ok(());
                }
                let Some(address) = is_address(text) else { return Ok(()) };
                let (original, linked) = if !self.from.mapped(address) && self.from.mapped(address.wrapping_sub(LINKED_SLIDE)) {
                    (address - LINKED_SLIDE, true)
                } else {
                    (address, false)
                };
                if !self.from.mapped(original) {
                    return Ok(());
                }
                match self.carry(original) {
                    Some(c) => {
                        if !c.sure {
                            out.review.push(format!("{} -> {}: {}", hex(original), hex(c.to), c.how));
                        }
                        *text = hex(if linked { c.to + LINKED_SLIDE } else { c.to });
                    }
                    None => out.missing.push(hex(original)),
                }
            }
            Value::List(items) => {
                for item in items.iter_mut() {
                    self.rewrite(item, out)?;
                }
                self.recheck_list(items, out)?;
            }
            Value::Dict(_) => {
                if let Value::Dict(items) = value {
                    for (_, item) in items.iter_mut() {
                        self.rewrite(item, out)?;
                    }
                }
                self.recheck_dict(value, out)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// A check's size in TO: the TO function's length when the check covered the whole FROM function.
    fn check_size(&self, old_address: Option<u64>, new_address: u64, size: u64) -> u64 {
        if let (Some(a), Some(&ib)) = (old_address, self.to.by_start.get(&new_address)) {
            if let Some(&ia) = self.from.by_start.get(&a) {
                let f = &self.from.functions[ia];
                if f.end - f.start == size {
                    let g = &self.to.functions[ib];
                    return g.end - g.start;
                }
            }
        }
        size
    }

    fn digest(&self, address: u64, size: u64) -> Result<String> {
        let bytes = self.to.image.read(address, size).ok_or_else(|| anyhow!("{} +{size}: not in the TO kernelcache", hex(address)))?;
        Ok(oneios_common::sha256_hex(bytes))
    }

    fn recheck_dict(&self, value: &mut Value, out: &mut Walked) -> Result<()> {
        let (Some(address), Some(size), Some(_)) = (value.get("address").and_then(Value::as_str), value.get("size"), value.get("sha256")) else { return Ok(()) };
        let new = unhex(address)?;
        let size = match size {
            Value::Int(n) => *n as u64,
            Value::Str(s) => unhex(s)?,
            _ => return Ok(()),
        };
        let old = self.map.taken.get(&self.to.by_start.get(&new).copied().unwrap_or(usize::MAX)).map(|&ia| self.from.functions[ia].start);
        let size = self.check_size(old, new, size);
        value.set("size", size as i128);
        value.set("sha256", self.digest(new, size)?);
        out.checks += 1;
        Ok(())
    }

    fn recheck_list(&self, items: &mut [Value], out: &mut Walked) -> Result<()> {
        // [address, size, sha256] (the display record's "checks")
        if let [Value::Str(address), Value::Int(size), Value::Str(sum)] = items {
            if is_address(address).is_some() && sum.len() == 64 {
                let new = unhex(address)?;
                *sum = self.digest(new, *size as u64)?;
                out.checks += 1;
            }
        }
        Ok(())
    }
}


/// A struct field an instruction reaches: (the instruction with its registers and offset masked out, the byte
/// offset), for LDR/STR (unsigned offset), LDUR/STUR, LDP/STP (signed offset) and ADD (immediate, 64-bit) off a
/// register other than SP. Read from a shape word, whose ADRP-based offsets are already 0 (`image::scan`).
fn field_offset(word: u32) -> Option<(u32, u64)> {
    let rn = (word >> 5) & 31;
    if rn == 31 {
        return None;
    }
    if word & 0x3b00_0000 == 0x3900_0000 {
        let (size, vector, opc) = (word >> 30, (word >> 26) & 1, (word >> 22) & 3);
        let scale = if vector == 1 && opc >= 2 { 4 } else { size };
        return Some((word & 0xffc0_0000, (((word >> 10) & 0xfff) as u64) << scale));
    }
    if word & 0x3b20_0c00 == 0x3800_0000 {
        let offset = crate::image::signed(((word >> 12) & 0x1ff) as u64, 9);
        return (offset > 0).then_some((word & 0xffe0_0c00, offset as u64));
    }
    if word & 0x3b80_0000 == 0x2900_0000 {
        let (opc, vector) = (word >> 30, (word >> 26) & 1);
        let scale = if vector == 1 { 2 + opc } else { 2 + (opc >> 1) };
        let offset = crate::image::signed(((word >> 15) & 0x7f) as u64, 7) << scale;
        return (offset > 0).then_some((word & 0xffc0_0000, offset as u64));
    }
    if word & 0xff80_0000 == 0x9100_0000 {
        let shift = if word & 0x40_0000 != 0 { 12 } else { 0 };
        return Some((word & 0xffc0_0000, (((word >> 10) & 0xfff) as u64) << shift));
    }
    None
}

/// A shape with every field offset masked out, so that two builds' functions align across a moved field.
fn coarse(shape: &[u32]) -> Vec<u32> {
    shape.iter().map(|&w| match field_offset(w) {
        Some((class, _)) => class | 0x3e0 & w | 0x1f & w,
        None => w,
    }).collect()
}

/// One field's votes: TO offset -> (sure votes, unsure votes).
type Votes = BTreeMap<u64, (u64, u64)>;

impl<'a> Porter<'a> {
    /// The offsets the TO build's code uses where FROM's uses each field, in FROM's functions of [low, high).
    fn field_votes(&self, low: u64, high: u64, fields: &[(String, u64)]) -> BTreeMap<String, Votes> {
        let mut votes: BTreeMap<String, Votes> = fields.iter().map(|(n, _)| (n.clone(), Votes::new())).collect();
        let lo = self.from.sorted.partition_point(|&(start, _)| start < low);
        for &(start, ia) in &self.from.sorted[lo..] {
            if start >= high {
                break;
            }
            let Some(&(ib, how)) = self.map.function.get(&ia) else { continue };
            let (fa, fb) = (&self.from.functions[ia], &self.to.functions[ib]);
            let wanted: Vec<(usize, u32, &String)> = fa.shape.iter().enumerate().filter_map(|(i, &w)| {
                let (class, offset) = field_offset(w)?;
                fields.iter().find(|(_, v)| *v == offset).map(|(n, _)| (i, class, n))
            }).collect();
            if wanted.is_empty() {
                continue;
            }
            let same = fa.key == fb.key;
            let align: HashMap<usize, usize> = if same {
                (0..fa.shape.len()).map(|i| (i, i)).collect()
            } else {
                match lcs(&coarse(&fa.shape), &coarse(&fb.shape)) {
                    Some(a) => a,
                    None => continue,
                }
            };
            let sure = same && how != How::Order;
            for (i, class, name) in wanted {
                let Some(&j) = align.get(&i) else { continue };
                let Some((class_to, offset_to)) = fb.shape.get(j).and_then(|&w| field_offset(w)) else { continue };
                if class_to != class {
                    continue;
                }
                let slot = votes.get_mut(name).unwrap().entry(offset_to).or_default();
                if sure { slot.0 += 1 } else { slot.1 += 1 }
            }
        }
        votes
    }
}

/// `--fields KEY`: the record's KEY object of field offsets, read again from TO's code (see the header). Returns the
/// report lines; a field with no vote, or with votes for several offsets, is an error unless `loose`.
fn port_fields(porter: &Porter, record: &mut Value, keys: &[String], low: u64, high: u64, loose: bool) -> Result<Vec<Value>> {
    let mut lines = Vec::new();
    for key in keys {
        let Some(Value::Dict(items)) = record.get(key) else { bail!("--fields {key}: the record has no object \"{key}\"") };
        let fields: Vec<(String, u64)> = items.iter().filter_map(|(n, v)| v.as_int().map(|i| (n.clone(), i as u64))).collect();
        let votes = porter.field_votes(low, high, &fields);
        let mut new = Value::dict();
        for (name, old) in &fields {
            let tally = &votes[name];
            let shown = tally.iter().map(|(o, (s, u))| format!("{o}: {s} sure + {u}")).collect::<Vec<_>>().join(", ");
            let best = tally.iter().max_by_key(|(_, (s, u))| (*s, *u)).map(|(o, _)| *o);
            let line = format!("{key}.{name} {old} -> {} (votes {shown})", best.map_or("?".into(), |b| b.to_string()));
            println!("port: {line}");
            lines.push(Value::str(line));
            match best {
                Some(b) if tally.len() == 1 || loose => new.set(name.as_str(), b as i128),
                _ if loose => new.set(name.as_str(), *old as i128),
                None => bail!("--fields {key}.{name}: no instruction of FROM's [{}, {}) uses offset {old}", hex(low), hex(high)),
                _ => bail!("--fields {key}.{name}: TO's code uses several offsets where FROM's uses {old} ({shown}); --fields-loose takes the most voted"),
            }
        }
        record.set(key.as_str(), new);
    }
    Ok(lines)
}

fn port_record(porter: &Porter, input: &str, output: &str, fields: &[String], loose: bool) -> Result<Value> {
    let mut record = json::load(input)?;
    let mut summary = Value::dict();
    // the field offsets first, while "family_text" still names FROM's range
    let from_vtable = record.get("vtable").and_then(Value::as_str).and_then(|v| unhex(v).ok());
    let range = record.get("family_text").and_then(Value::as_list).and_then(|l| Some((unhex(l.first()?.as_str()?).ok()?, unhex(l.get(1)?.as_str()?).ok()?)));
    match (fields.is_empty(), range) {
        (true, _) => {}
        (false, Some((low, high))) => summary.set("fields", Value::List(port_fields(porter, &mut record, fields, low, high, loose)?)),
        (false, None) => {}
    }
    summary.set("in", input);
    summary.set("out", output);
    let (mut kept, mut left, mut marked, mut checks) = (0u64, Vec::new(), Vec::new(), 0u64);
    // each element of a top-level list is one record; anything else is walked whole
    if let Value::Dict(items) = &mut record {
        for (key, item) in items.iter_mut() {
            if let Value::List(list) = item {
                let mut carried = Vec::new();
                for (index, mut element) in std::mem::take(list).into_iter().enumerate() {
                    let mut walked = Walked::default();
                    porter.rewrite(&mut element, &mut walked)?;
                    checks += walked.checks;
                    if !walked.missing.is_empty() {
                        left.push(Value::str(format!("{key}[{index}]: {} not carried over", walked.missing.join(", "))));
                        continue;
                    }
                    if !walked.review.is_empty() {
                        let text = format!("review: {}", walked.review.join("; "));
                        if let Value::Dict(_) = element {
                            element.set("port", text.as_str());
                        }
                        marked.push(Value::str(format!("{key}[{index}]: {text}")));
                    }
                    kept += 1;
                    carried.push(element);
                }
                *list = carried;
            } else {
                let mut walked = Walked::default();
                porter.rewrite(item, &mut walked)?;
                checks += walked.checks;
                if !walked.missing.is_empty() {
                    bail!("{input}: \"{key}\" names {} which do not carry over (not a list of records: nothing to leave out)", walked.missing.join(", "));
                }
                for text in walked.review {
                    marked.push(Value::str(format!("{key}: review: {text}")));
                }
            }
        }
    }
    // a record with "vtable" and "slots" ([function, diversity] per slot, the display record's): every slot is read
    // again from the two vtables themselves (their chained-fixup pointers from vtable + 16 on, as `prelink classes`
    // reads them). A slot whose pointer is of another kind in TO (another diversity or key: another method) is an
    // error; a slot whose two functions are not a pair of the map is marked. A slot the record holds as FROM's vtable
    // does takes TO's function; one it holds otherwise (a pure slot under its callers' diversity) keeps the record's
    // diversity and its function as carried. Slots of another shape (the hid layout's objects) were walked above.
    let pairs = |slots: &Value| slots.as_list().is_some_and(|l| l.iter().all(|s| matches!(s.as_list(), Some([Value::Str(_), Value::Str(_)]))));
    if let (Some(from_vtable), Some(vtable), Some(Value::List(slots))) = (from_vtable, record.get("vtable").and_then(Value::as_str).map(str::to_string), record.get("slots").filter(|s| pairs(s)).cloned()) {
        let vtable = unhex(&vtable)?;
        let (mut moved, mut fixed) = (0u64, Vec::new());
        for (i, slot) in slots.iter().enumerate() {
            let at = |v: u64| v + 16 + 8 * i as u64;
            let (Some(fa), Some(fb)) = (porter.from.fixups.get(&at(from_vtable)), porter.to.fixups.get(&at(vtable))) else {
                bail!("{input}: slot {i}: no pointer in FROM's vtable {} or TO's {}", hex(from_vtable), hex(vtable));
            };
            if fa.meta() != fb.meta() {
                bail!("{input}: slot {i}: TO's pointer is {:?}, FROM's {:?} (another method)", fb.meta(), fa.meta());
            }
            if !porter.pairs(fa.target, fb.target) {
                marked.push(Value::str(format!("slots[{i}]: {} -> {}: not a pair of the map (taken from TO's vtable)", hex(fa.target), hex(fb.target))));
            }
            let ported = slot.as_list().and_then(|s| Some((s.first()?.as_str()?.to_string(), s.get(1)?.as_str()?.to_string())));
            let Some((target, diversity)) = ported else { bail!("{input}: slots[{i}] is not [function, diversity]") };
            let as_vtable = u64::from(fa.diversity) == unhex(&diversity)?;
            let mut row = Value::list();
            if as_vtable {
                if unhex(&target)? != fb.target {
                    moved += 1;
                }
                row.push(hex(fb.target));
            } else {
                row.push(target);
            }
            row.push(diversity);
            fixed.push(row);
        }
        if let Some(next) = porter.to.fixups.get(&(vtable + 16 + 8 * slots.len() as u64)) {
            if porter.to.by_start.contains_key(&next.target) && next.auth {
                marked.push(Value::str(format!("slots: TO's vtable {} has a pointer after slot {} (a slot more?)", hex(vtable), slots.len() - 1)));
            }
        }
        record.set("slots", Value::List(fixed));
        summary.set("slots_read_again", slots.len() as i128);
        summary.set("slots_not_as_carried", moved as i128);
        println!("port: {input}: {} vtable slots read again from the two vtables, {moved} not where the map put them", slots.len());
    }
    let mut ported = Value::dict();
    ported.set("from_kernelcache_sha256", porter.from.sha256.as_str());
    ported.set("by", "oneios-tool prelink port (kernel/prelink/README.md, \"Another build\")");
    ported.set("kept", kept as i128);
    ported.set("left_out", left.len() as i128);
    ported.set("review", marked.len() as i128);
    if let Value::Dict(_) = record {
        record.set("ported", ported);
    }
    std::fs::write(output, json::dumps(&record, Some(1), false) + "\n").with_context(|| output.to_string())?;
    summary.set("kept", kept as i128);
    summary.set("checks_read_again", checks as i128);
    summary.set("left_out", Value::List(left));
    summary.set("review", Value::List(marked));
    Ok(summary)
}

pub fn run(args: Args) -> Result<()> {
    let (raw_a, raw_b) = (oneios_common::Map::open(&args.from)?, oneios_common::Map::open(&args.to)?);
    let (ia, ib) = (Image::new(&raw_a, 0)?, Image::new(&raw_b, 0)?);
    let names: Vec<String> = ia.entries.keys().filter(|n| ib.entries.contains_key(*n)).cloned().collect();
    let only: Vec<&String> = ia.entries.keys().filter(|n| !ib.entries.contains_key(*n)).chain(ib.entries.keys().filter(|n| !ia.entries.contains_key(*n))).collect();
    let (from, to) = (Side::new(&raw_a, &names)?, Side::new(&raw_b, &names)?);
    let mut map = Map { function: HashMap::new(), taken: HashMap::new(), data_votes: HashMap::new(), conflicts: 0 };
    let mut queue = VecDeque::new();
    // shape: unique per entry on both sides
    let count = |side: &Side| {
        let mut c: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
        for (i, f) in side.functions.iter().enumerate() {
            c.entry(f.key).or_default().push(i);
        }
        c
    };
    let (ka, kb) = (count(&from), count(&to));
    let mut unique: Vec<(usize, usize)> = ka.iter().filter_map(|(k, v)| match (v.as_slice(), kb.get(k).map(Vec::as_slice)) {
        ([a], Some([b])) => Some((*a, *b)),
        _ => None,
    }).collect();
    unique.sort_unstable();
    for (a, b) in unique {
        map.pair(a, b, How::Shape, &mut queue);
    }
    walk(&from, &to, &mut map, &mut queue);
    loop {
        let added = order(&from, &to, &mut map, &mut queue);
        walk(&from, &to, &mut map, &mut queue);
        if added == 0 {
            break;
        }
    }
    let porter = Porter { from, to, map };
    let mut how_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut same = 0u64;
    for (&a, &(b, how)) in &porter.map.function {
        *how_counts.entry(how.name()).or_default() += 1;
        if porter.from.functions[a].key == porter.to.functions[b].key {
            same += 1;
        }
    }
    let data_paired = porter.map.data_votes.values().filter(|v| v.len() == 1).count();
    let data_split = porter.map.data_votes.len() - data_paired;
    println!(
        "port: {} -> {}: {} entries in both ({} in one only); functions {} / {}, paired {} ({}), {} of them the same shape; data paired {data_paired}, {data_split} with votes for several; {} conflicts",
        &porter.from.sha256[..12],
        &porter.to.sha256[..12],
        names.len(),
        only.len(),
        porter.from.functions.len(),
        porter.to.functions.len(),
        porter.map.function.len(),
        how_counts.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", "),
        same,
        porter.map.conflicts
    );
    if let Some(path) = &args.map {
        // keys are distinct (one per FROM start or data address): pushed, not `set` (a linear search per key: quadratic over 200,000)
        let mut functions: Vec<(String, Value)> = Vec::new();
        let mut pairs: Vec<(&usize, &(usize, How))> = porter.map.function.iter().collect();
        pairs.sort_by_key(|(a, _)| porter.from.functions[**a].start);
        for (&a, &(b, how)) in pairs {
            let (fa, fb) = (&porter.from.functions[a], &porter.to.functions[b]);
            let mut row = Value::dict();
            row.set("to", hex(fb.start));
            row.set("how", how.name());
            row.set("same", fa.key == fb.key);
            row.set("size", (fa.end - fa.start) as i128);
            row.set("size_to", (fb.end - fb.start) as i128);
            row.set("entry", porter.from.entries[fa.entry].as_str());
            functions.push((hex(fa.start), row));
        }
        let mut data: Vec<(String, Value)> = Vec::new();
        let mut keys: Vec<&u64> = porter.map.data_votes.keys().collect();
        keys.sort_unstable();
        for a in keys {
            if let Some(b) = porter.map.data(*a) {
                data.push((hex(*a), Value::str(hex(b))));
            }
        }
        let mut out = Value::dict();
        out.set("from_kernelcache_sha256", porter.from.sha256.as_str());
        out.set("to_kernelcache_sha256", porter.to.sha256.as_str());
        out.set("functions", Value::Dict(functions));
        out.set("data", Value::Dict(data));
        std::fs::write(path, json::dumps(&out, Some(1), false) + "\n").with_context(|| path.clone())?;
    }
    let mut report = Vec::new();
    let mut left_total = 0;
    for spec in &args.records {
        let (input, output) = spec.split_once('=').ok_or_else(|| anyhow!("--record IN=OUT, not {spec}"))?;
        let summary = port_record(&porter, input, output, &args.fields, args.fields_loose)?;
        let left = summary.get("left_out").and_then(Value::as_list).map_or(0, |l| l.len());
        let review = summary.get("review").and_then(Value::as_list).map_or(0, |l| l.len());
        println!("port: {input} -> {output}: {} kept ({review} to review), {left} left out, {} checks read again", summary.get("kept").and_then(Value::as_int).unwrap_or(0), summary.get("checks_read_again").and_then(Value::as_int).unwrap_or(0));
        left_total += left;
        report.push(summary);
    }
    if let Some(path) = &args.report {
        std::fs::write(path, json::dumps(&Value::List(report), Some(1), false) + "\n").with_context(|| path.clone())?;
    }
    if args.strict && left_total > 0 {
        bail!("port: {left_total} record(s) left out (--strict)");
    }
    Ok(())
}

/// The C string at `va` (4 to 128 printable bytes, then a NUL), without the NUL; None for anything else.
fn cstr<'a>(image: &Image<'a>, va: u64) -> Option<&'a [u8]> {
    let bytes = [129u64, 64, 32, 16, 8].iter().find_map(|&n| image.read(va, n))?;
    let end = bytes.iter().position(|&b| b == 0)?;
    let name = &bytes[..end];
    (name.len() >= 4 && name.iter().all(|b| b.is_ascii_graphic() || *b == b' ')).then_some(name)
}
