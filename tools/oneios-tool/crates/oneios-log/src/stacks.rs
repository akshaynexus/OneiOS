//! `log stacks`: the user backtraces of a log (`oneios-ubt:` samples, `sheng-exc` / `sheng-ubt` exceptions), named
//! through the sym index, per thread over time.

use crate::parse::{boots, hex_token, Boot, Log, Rec, Scan, Src};
use oneios_sym::{Index, Slide};
use std::collections::BTreeMap;
use std::fmt::Write;

/// One thread of one sample: `oneios-ubt: PROC[pid] thread 0xT state 0xS: pc lr ret...`.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadRec {
    pub proc_name: String,
    pub pid: u32,
    pub tid: u64,
    pub state: u32,
    pub addrs: Vec<u64>,
    /// The line was cut: the stack may be longer than what is here.
    pub cut: bool,
    pub line: usize,
}

#[derive(Clone, Debug)]
pub struct Sample {
    pub time: Option<f64>,
    pub line: usize,
    /// The header's count and names, when the header line was there.
    pub header: Option<(u32, String)>,
    pub threads: Vec<ThreadRec>,
}

/// One exception: `sheng-exc: NAME[pid] exception type=.. code=.. subcode=..` and the lines after it.
#[derive(Clone, Debug)]
pub struct Exception {
    pub time: Option<f64>,
    pub line: usize,
    pub proc_name: String,
    pub pid: u32,
    pub what: String,
    pub pc: Option<u64>,
    pub lr: Option<u64>,
    pub far: Option<u64>,
    pub esr: Option<u64>,
    /// The `sheng-ubt` return addresses.
    pub rets: Vec<u64>,
}

#[derive(Default)]
pub struct BootStacks {
    pub samples: Vec<Sample>,
    pub exceptions: Vec<Exception>,
    pub unparsed: Vec<usize>,
    /// The start of the range of `NAME[pid] triggered unnest of range A->B of DYLD shared region` lines:
    /// start -> (lines, the first line).
    pub unnest: BTreeMap<u64, (usize, usize)>,
}

/// The kernel unnests the shared region's data at the first write to it: the range starts at the slid start of
/// the cache's data, rounded down to this (the size one level-2 table entry maps with 4 KiB pages).
pub const UNNEST_ALIGN: u64 = 0x20_0000;

/// The addresses of a record: `0x...` tokens up to the first token that is not one. A cut record's last token may
/// be cut too, so it is dropped unless a space follows it.
fn addresses(text: &str, cut: bool) -> Vec<u64> {
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        match hex_token(token) {
            Some(v) => out.push(v),
            None => break,
        }
    }
    if cut && !text.ends_with(' ') {
        out.pop();
    }
    out
}

/// `PROC[pid] thread 0xT state 0xS: addr...` (the text after `oneios-ubt: `).
pub fn parse_thread(text: &str, cut: bool, line: usize) -> Option<ThreadRec> {
    let at = text.find("] thread 0x")?;
    let open = text[..at].rfind('[')?;
    let pid = text[open + 1..at].parse().ok()?;
    let mut s = Scan::new(&text[at..]);
    let tid = s.hex("] thread ")?;
    let state = s.hex(" state ")? as u32;
    let rest = s.rest().strip_prefix(':')?;
    Some(ThreadRec { proc_name: text[..open].to_string(), pid, tid, state, addrs: addresses(rest, cut), cut, line })
}

fn name_pid(text: &str) -> Option<(String, u32, &str)> {
    let open = text.find('[')?;
    let close = open + text[open..].find(']')?;
    let pid = text[open + 1..close].parse().ok()?;
    Some((text[..open].to_string(), pid, &text[close + 1..]))
}

pub fn collect(recs: &[Rec]) -> BootStacks {
    let mut out = BootStacks::default();
    let mut last_time: Option<f64> = None;
    for r in recs {
        if r.src != Src::Guest {
            continue;
        }
        if let Some(text) = r.text.strip_prefix("oneios-ubt: ") {
            // A new sample: its header, or (the header lost) a gap in time.
            let gap = matches!((last_time, r.time), (Some(a), Some(b)) if b - a > 2.0);
            let mut s = Scan::new(text);
            let header = s.num("").filter(|_| s.rest().starts_with(" thread(s) of "));
            if let Some(count) = header {
                let names = s.rest().trim_start_matches(" thread(s) of ").trim().to_string();
                out.samples.push(Sample { time: r.time, line: r.line, header: Some((count as u32, names)), threads: Vec::new() });
            } else if let Some(thread) = parse_thread(text, r.tail_cut, r.line) {
                if out.samples.is_empty() || gap {
                    out.samples.push(Sample { time: r.time, line: r.line, header: None, threads: Vec::new() });
                }
                if let Some(sample) = out.samples.last_mut() {
                    sample.threads.push(thread);
                }
            } else {
                out.unparsed.push(r.line);
            }
            last_time = r.time.or(last_time);
        } else if let Some(at) = r.text.find(" triggered unnest of range 0x").filter(|_| r.text.contains("DYLD shared region")) {
            if let Some(start) = Scan::new(&r.text[at..]).hex(" triggered unnest of range ") {
                let e = out.unnest.entry(start).or_insert((0, r.line));
                e.0 += 1;
            }
        } else if let Some(text) = r.text.strip_prefix("sheng-ubt sheng-exc: ") {
            let Some(exc) = out.exceptions.last_mut() else { continue };
            let mut s = Scan::new(text);
            if text.starts_with('#') {
                match s.hex("ret=") {
                    Some(ret) => exc.rets.push(ret),
                    None => out.unparsed.push(r.line),
                }
            }
        } else if let Some(text) = r.text.strip_prefix("sheng-exc: ") {
            if let Some((name, pid, rest)) = name_pid(text).filter(|x| x.2.starts_with(" exception ")) {
                out.exceptions.push(Exception {
                    time: r.time,
                    line: r.line,
                    proc_name: name,
                    pid,
                    what: rest.trim().trim_start_matches("exception ").to_string(),
                    pc: None,
                    lr: None,
                    far: None,
                    esr: None,
                    rets: Vec::new(),
                });
            } else if text.starts_with("pc=") {
                if let Some(exc) = out.exceptions.last_mut() {
                    if exc.pc.is_none() {
                        let mut s = Scan::new(text);
                        exc.pc = s.hex("pc=");
                        exc.lr = s.hex("lr=");
                        exc.far = s.hex("far=");
                        exc.esr = s.hex("esr=");
                    }
                }
            }
        }
    }
    out
}

/// The scheduler state bits of a thread (osfmk/kern/thread.h).
pub fn state_text(state: u32) -> String {
    let names = [(0x01, "wait"), (0x02, "suspended"), (0x04, "run"), (0x08, "uninterruptible"), (0x10, "terminate"), (0x20, "terminate2"), (0x40, "wait-report"), (0x80, "idle")];
    let parts: Vec<&str> = names.iter().filter(|(bit, _)| state & bit != 0).map(|(_, n)| *n).collect();
    if parts.is_empty() {
        format!("{state:#x}")
    } else {
        format!("{state:#x} {}", parts.join("+"))
    }
}

/// Two stacks of one thread are the same when their addresses are equal; a cut one is the same as a stack it is
/// the start of.
fn same_stack(a: &ThreadRec, b: &ThreadRec) -> bool {
    match (a.cut, b.cut) {
        (false, false) => a.addrs == b.addrs,
        (true, false) => !a.addrs.is_empty() && b.addrs.starts_with(&a.addrs),
        (false, true) => !b.addrs.is_empty() && a.addrs.starts_with(&b.addrs),
        (true, true) => {
            let n = a.addrs.len().min(b.addrs.len());
            n > 0 && a.addrs[..n] == b.addrs[..n]
        }
    }
}

pub struct Options {
    pub proc_name: Option<String>,
    pub thread: Option<u64>,
    pub slide: Option<u64>,
    pub summary_only: bool,
}

fn frame(out: &mut String, index: Option<&Index>, slide: Option<u64>, addr: u64, indent: &str) {
    let named = match (index, slide) {
        (Some(index), Some(slide)) => Some(addr.checked_sub(slide).and_then(|u| index.name(u))),
        _ => None,
    };
    let _ = match named {
        None => writeln!(out, "{indent}{addr:#x}"),
        // dsc.py's line, also for an address below the slide (a negative unslid address).
        Some(None) => match slide {
            Some(slide) if addr >= slide => writeln!(out, "{indent}{addr:#x}  (not in a cache image; unslid {:#x})", addr - slide),
            Some(slide) => writeln!(out, "{indent}{addr:#x}  (not in a cache image; unslid -{:#x})", slide - addr),
            None => writeln!(out, "{indent}{addr:#x}  (not in a cache image)"),
        },
        Some(Some(n)) => match n.symbol {
            Some((symbol, offset)) => writeln!(out, "{indent}{addr:#x}  {}  {symbol} + {offset:#x}", n.image),
            None => writeln!(out, "{indent}{addr:#x}  {}  ?", n.image),
        },
    };
}

/// The first named symbols of a stack, for a one-line summary.
fn top_symbols(index: Option<&Index>, slide: Option<u64>, addrs: &[u64], count: usize) -> String {
    let (Some(index), Some(slide)) = (index, slide) else {
        return addrs.iter().take(count).map(|a| format!("{a:#x}")).collect::<Vec<_>>().join(" <- ");
    };
    let mut names = Vec::new();
    for &a in addrs {
        if names.len() == count {
            break;
        }
        match a.checked_sub(slide).and_then(|u| index.name(u)) {
            Some(n) => names.push(n.symbol.map_or_else(|| format!("{}:?", n.image), |s| s.0.to_string())),
            None => names.push(format!("{a:#x}")),
        }
    }
    names.join(" <- ")
}

fn sample_label(i: usize, s: &Sample, line: usize) -> String {
    match s.time {
        Some(t) => format!("sample {} ({t:.3} s, line {line})", i + 1),
        None => format!("sample {} (line {line})", i + 1),
    }
}

/// The slide of a boot: from all the addresses of its backtraces. When several slides fit as well (few
/// addresses), the one that puts the start of the cache's data in the range the kernel unnested, when that is one
/// slide: the note says so.
pub fn boot_slide(index: &Index, stacks: &BootStacks) -> Option<(Slide, Option<String>)> {
    let mut addrs: Vec<u64> = stacks.samples.iter().flat_map(|s| s.threads.iter().flat_map(|t| t.addrs.iter().copied())).collect();
    for e in &stacks.exceptions {
        addrs.extend(e.lr);
        addrs.extend(e.rets.iter().copied());
    }
    addrs.sort_unstable();
    addrs.dedup();
    let (scores, _) = index.slide_scores(&addrs);
    let inside = addrs.iter().filter(|&&a| index.is_inside(a)).count();
    let mut found = oneios_sym::index::pick_slide(&scores, inside);
    if found.inside == 0 || found.score <= 0 {
        return None;
    }
    let mut note = None;
    let unnest = stacks.unnest.iter().max_by_key(|(_, (count, _))| *count).map(|(start, (_, line))| (*start, *line));
    if let (true, Some((start, line)), Some(data)) = (found.ties > 1, unnest, index.first_writable()) {
        let fits: Vec<u64> = scores
            .iter()
            .enumerate()
            .filter(|(_, &score)| score as i64 == found.score)
            .map(|(k, _)| k as u64 * oneios_sym::index::SLIDE_STEP)
            .filter(|slide| data + slide >= start && data + slide < start + UNNEST_ALIGN)
            .collect();
        if let [one] = fits[..] {
            found.slide = one;
            note = Some(format!("this one puts the cache's data in the unnest range {start:#x} of line {line}"));
        }
    }
    Some((found, note))
}

pub fn report(log: &Log, index: Option<&Index>, o: &Options) -> String {
    let mut out = String::new();
    let all = boots(log);
    for (b, boot) in all.iter().enumerate() {
        let stacks = collect(&log.recs[boot.first..boot.end]);
        boot_report(&mut out, b, boot, &stacks, index, o);
    }
    out
}

fn boot_report(out: &mut String, b: usize, boot: &Boot, stacks: &BootStacks, index: Option<&Index>, o: &Options) {
    let _ = write!(out, "boot {} (lines {}-{}): {} sample(s), {} exception(s)", b + 1, boot.first_line, boot.last_line, stacks.samples.len(), stacks.exceptions.len());
    let slide = match (o.slide, index) {
        (Some(slide), _) => {
            let _ = writeln!(out, "; slide {slide:#x} (given)");
            Some(slide)
        }
        (None, Some(index)) => match boot_slide(index, stacks) {
            Some((found, note)) => {
                let _ = write!(out, "; slide {:#x} ({} of {} distinct addresses follow a call", found.slide, found.score, found.inside);
                let _ = match (found.ties > 1, note) {
                    (false, _) => writeln!(out, ")"),
                    (true, Some(note)) => writeln!(out, "; {} slides fit as well: {note})", found.ties),
                    (true, None) => writeln!(out, "; {} slides fit as well: NOT reliable)", found.ties),
                };
                Some(found.slide)
            }
            None => {
                let _ = writeln!(out, "; no slide (no backtrace address in the cache)");
                None
            }
        },
        (None, None) => {
            let _ = writeln!(out, "; addresses not named");
            None
        }
    };
    let index = if slide.is_some() { index } else { None };
    let wanted = |name: &str, tid: Option<u64>| {
        o.proc_name.as_deref().is_none_or(|p| p == name) && (o.thread.is_none() || o.thread == tid)
    };

    // Per thread: its record in every sample.
    let mut threads: BTreeMap<(String, u32, u64), Vec<Option<&ThreadRec>>> = BTreeMap::new();
    for (i, sample) in stacks.samples.iter().enumerate() {
        for t in &sample.threads {
            if !wanted(&t.proc_name, Some(t.tid)) {
                continue;
            }
            let slot = threads.entry((t.proc_name.clone(), t.pid, t.tid)).or_insert_with(|| vec![None; stacks.samples.len()]);
            slot[i] = Some(t);
        }
    }
    for (i, sample) in stacks.samples.iter().enumerate() {
        let _ = write!(out, "  {}: {} thread(s) read", sample_label(i, sample, sample.line), sample.threads.len());
        let _ = match &sample.header {
            Some((count, names)) => writeln!(out, " of the {count} of {names}"),
            None => writeln!(out, " (its header line is not in the log)"),
        };
    }
    let mut stuck = Vec::new();
    let mut changed = Vec::new();
    let mut once = Vec::new();
    for ((name, pid, tid), slots) in &threads {
        let present: Vec<(usize, &ThreadRec)> = slots.iter().enumerate().filter_map(|(i, t)| t.map(|t| (i, t))).collect();
        // Runs of the same stack.
        let mut runs: Vec<(usize, usize, &ThreadRec)> = Vec::new(); // (first sample, last sample, the longest record)
        for &(i, t) in &present {
            match runs.last_mut() {
                Some(run) if same_stack(run.2, t) => {
                    run.1 = i;
                    if t.addrs.len() > run.2.addrs.len() {
                        run.2 = t;
                    }
                }
                _ => runs.push((i, i, t)),
            }
        }
        let label = format!("{name}[{pid}] thread {tid:#x}");
        let longest = runs.iter().map(|r| r.2).max_by_key(|t| t.addrs.len());
        let top = longest.map(|t| top_symbols(index, slide, &t.addrs, 4)).unwrap_or_default();
        let entry = (label.clone(), present.len(), runs.len(), top);
        if present.len() < 2 {
            once.push(entry);
        } else if runs.len() == 1 {
            stuck.push(entry);
        } else {
            changed.push(entry);
        }
        if o.summary_only {
            continue;
        }
        let _ = writeln!(out, "\n== {label}: in {} of {} sample(s), {} stack(s)", present.len(), stacks.samples.len(), runs.len());
        let mut at = 0; // index in `present`
        for &(first, last, shown) in &runs {
            let in_run: Vec<&(usize, &ThreadRec)> = present[at..].iter().take_while(|(i, _)| *i <= last).collect();
            at += in_run.len();
            let mut states: Vec<u32> = in_run.iter().map(|(_, t)| t.state).collect();
            states.dedup();
            let states: Vec<String> = states.iter().map(|&s| state_text(s)).collect();
            let missing = (last - first + 1) - in_run.len();
            let first_line = in_run.first().map_or(shown.line, |(_, t)| t.line);
            let _ = write!(out, "  {}", sample_label(first, &stacks.samples[first], first_line));
            if last > first {
                let _ = match last - first {
                    1 => write!(out, "; sample {}: same", last + 1),
                    _ => write!(out, "; samples {}-{}: same", first + 2, last + 1),
                };
                if missing > 0 {
                    let _ = write!(out, " ({missing} without this thread)");
                }
            }
            let _ = write!(out, "; state {}", states.join(", then "));
            if in_run.iter().any(|(_, t)| t.cut) {
                let cut = in_run.iter().filter(|(_, t)| t.cut).count();
                let _ = write!(out, "; {cut} of {} line(s) cut", in_run.len());
                if shown.cut {
                    let _ = write!(out, " (this stack may be longer)");
                }
            }
            let _ = writeln!(out, ":");
            for &addr in &shown.addrs {
                frame(out, index, slide, addr, "    ");
            }
        }
    }

    let shown: Vec<&Exception> = stacks.exceptions.iter().filter(|e| wanted(&e.proc_name, None) && o.thread.is_none()).collect();
    if !shown.is_empty() && !o.summary_only {
        let _ = writeln!(out, "\n== exceptions (sheng-exc)");
        for e in &shown {
            let time = e.time.map_or(String::new(), |t| format!("{t:.3} s, "));
            let _ = writeln!(out, "  {}[{}] ({time}line {}): {}", e.proc_name, e.pid, e.line, e.what);
            let field = |v: Option<u64>| v.map_or("-".to_string(), |v| format!("{v:#x}"));
            let _ = writeln!(out, "    pc {} lr {} far {} esr {}", field(e.pc), field(e.lr), field(e.far), field(e.esr));
            for addr in e.pc.into_iter().chain(e.lr).chain(e.rets.iter().copied()) {
                frame(out, index, slide, addr, "    ");
            }
        }
    }

    let _ = writeln!(out, "\nsummary of boot {}: {} sample(s), {} thread(s), {} exception(s)", b + 1, stacks.samples.len(), threads.len(), shown.len());
    let mut list = |title: &str, entries: &[(String, usize, usize, String)], with_runs: bool| {
        let _ = writeln!(out, "  {title}: {}", entries.len());
        for (label, present, runs, top) in entries {
            let runs = if with_runs { format!(", {runs} stacks") } else { String::new() };
            let _ = writeln!(out, "    {label}  ({present} samples{runs})  {top}");
        }
    };
    list("never changed (the same stack in every sample that has the thread, 2 or more)", &stuck, false);
    list("changed", &changed, true);
    list("in one sample only", &once, false);
    if !shown.is_empty() {
        let mut by_what: BTreeMap<(String, String), usize> = BTreeMap::new();
        for e in &shown {
            *by_what.entry((e.proc_name.clone(), e.what.clone())).or_default() += 1;
        }
        let _ = writeln!(out, "  exceptions:");
        for ((name, what), n) in by_what {
            let _ = writeln!(out, "    {name}: {n} x {what}");
        }
    }
    if !stacks.unparsed.is_empty() {
        let _ = writeln!(out, "  unparsed: {} backtrace line(s), the first at line {}", stacks.unparsed.len(), stacks.unparsed[0]);
    }
    let _ = writeln!(out);
}
