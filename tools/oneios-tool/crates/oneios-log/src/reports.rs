//! `log gpu`, `log cpu`, `log display`, `log perf`: tables over time from the periodic report lines.
//!
//! Every field is read by its label (`parse::Scan`), so a cut line gives the fields it still has and `-` for the
//! rest, and a format that gained a field at the end still reads. A line of a known kind whose first field is
//! not there counts as unparsed; the counts are in the notes.

use crate::parse::{Log, Rec, Scan, Src};
use crate::table::{Cell, Table};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct Notes {
    unparsed: BTreeMap<&'static str, (usize, usize)>,
    cut: BTreeMap<&'static str, (usize, usize)>,
}

impl Notes {
    fn unparsed(&mut self, kind: &'static str, line: usize) {
        let e = self.unparsed.entry(kind).or_insert((0, line));
        e.0 += 1;
    }

    fn seen(&mut self, kind: &'static str, r: &Rec) {
        if r.tail_cut {
            let e = self.cut.entry(kind).or_insert((0, r.line));
            e.0 += 1;
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (kind, (n, line)) in &self.unparsed {
            out.push(format!("unparsed: {n} `{kind}` line(s), the first at line {line}"));
        }
        for (kind, (n, line)) in &self.cut {
            out.push(format!("cut: {n} `{kind}` line(s) lost their end (another record was written into them), the first at line {line}; their missing fields are `-`"));
        }
        out
    }
}

fn head(r: &Rec) -> Vec<Cell> {
    vec![Cell::float(r.time, 3), Cell::Int(r.line as i64)]
}

fn guest<'a>(r: &'a Rec, tag: &str) -> Option<&'a str> {
    (r.src == Src::Guest).then(|| r.text.strip_prefix(tag)).flatten()
}

fn host<'a>(r: &'a Rec, tag: &str) -> Option<&'a str> {
    matches!(r.src, Src::Host(_)).then(|| r.text.strip_prefix(tag)).flatten()
}

/// `N s: ` or `N.N s: ` at the start: the period.
fn period(s: &mut Scan) -> Option<f64> {
    let p = s.num("")?;
    s.skip(" s").then_some(p)
}

pub fn gpu(log: &Log) -> (Vec<Table>, Vec<String>) {
    let mut notes = Notes::default();
    let mut report = Table::new(
        "guest GPU report (oneios-gpu-report: per period; us)",
        &["time", "line", "s", "submits", "fenced", "frames", "mean_us", "max_ever_us", "<1ms", "<2ms", "<4ms", "<8ms", "<16ms", "<32ms", "longer", "fence_timeouts", "refusals"],
    );
    let mut memory = Table::new(
        "guest GPU memory (oneios-gpu-memory)",
        &["time", "line", "blobs+", "closed+", "deferred+", "waiting", "live", "own", "own_MiB", "wired", "wired_MiB", "contexts+", "clients"],
    );
    let mut clients = Table::new("guest GPU clients (oneios-gpu-clients: own+wired MiB)", &["time", "line", "client", "own_MiB", "wired_MiB"]);
    let mut hosts = Table::new(
        "host GPU (xnuhost gpu: N s host; us)",
        &["time", "line", "s", "bb_fences", "retired_mean", "retired_max", "submit_mean", "submit_max", "other_fences", "other_mean", "blobs", "blob_mean", "blob_max", "udmabuf", "clock_held+", "keepalives+"],
    );
    for r in &log.recs {
        if let Some(text) = guest(r, "oneios-gpu-report: ") {
            let mut s = Scan::new(text);
            let Some(p) = period(&mut s) else {
                notes.unparsed("oneios-gpu-report", r.line);
                continue;
            };
            notes.seen("oneios-gpu-report", r);
            let mut row = head(r);
            row.push(Cell::Int(p as i64));
            for label in ["submits ", "(fenced ", "frames ", "mean ", "max ever ", " ms ", "/", "/", "/", "/", "/", "longer ", "fence timeouts ", "refusals "] {
                row.push(Cell::int(s.num(label)));
            }
            report.push(row);
        } else if let Some(text) = guest(r, "oneios-gpu-memory: ") {
            let mut s = Scan::new(text);
            let Some(blobs) = s.num("blobs ") else {
                notes.unparsed("oneios-gpu-memory", r.line);
                continue;
            };
            notes.seen("oneios-gpu-memory", r);
            let mut row = head(r);
            row.push(Cell::Int(blobs as i64));
            for label in ["closed ", "(deferred ", "waiting ", "live ", "own ", "(", "wired ", "(", "contexts ", "clients "] {
                row.push(Cell::int(s.num(label)));
            }
            memory.push(row);
        } else if let Some(text) = guest(r, "oneios-gpu-clients: ") {
            notes.seen("oneios-gpu-clients", r);
            let mut any = false;
            for part in text.split(", ") {
                let mut words = part.split(' ');
                let (Some(name), Some(sizes)) = (words.next(), words.next()) else { continue };
                let Some((own, wired)) = sizes.split_once('+') else { continue };
                let (Ok(own), Ok(wired)) = (own.parse::<i64>(), wired.parse::<i64>()) else { continue };
                let mut row = head(r);
                row.extend([Cell::text(name), Cell::Int(own), Cell::Int(wired)]);
                clients.push(row);
                any = true;
            }
            if !any {
                notes.unparsed("oneios-gpu-clients", r.line);
            }
        } else if let Some(text) = host(r, "gpu: ") {
            let mut s = Scan::new(text);
            let Some(p) = period(&mut s) else { continue };
            if !s.skip(" host: ") {
                continue;
            }
            let mut row = head(r);
            row.push(Cell::Int(p as i64));
            for label in ["backboardd fences ", "submit->retired mean ", "max ", "submit calls mean ", "max ", "other fences ", "mean ", "blobs made ", "mean ", "max ", "(udmabufs ", "clock held ", "keep-alives "] {
                row.push(Cell::int(s.num(label)));
            }
            hosts.push(row);
        }
    }
    (vec![report, memory, clients, hosts], notes.lines())
}

/// `name[pid] N%` entries of an `oneios-cpu` line.
fn cpu_entries(text: &str) -> Vec<(String, i64, i64)> {
    let mut out = Vec::new();
    for part in text.split(", ") {
        let part = part.split(" (all").next().unwrap_or(part).trim();
        let Some(open) = part.rfind('[') else { continue };
        let Some(close) = part[open..].find(']') else { continue };
        let pid = part[open + 1..open + close].parse::<i64>();
        let pct = part[open + close + 1..].trim().trim_end_matches('%').parse::<i64>();
        if let (Ok(pid), Ok(pct)) = (pid, pct) {
            out.push((part[..open].to_string(), pid, pct));
        }
    }
    out
}

pub fn cpu(log: &Log) -> (Vec<Table>, Vec<String>) {
    let mut notes = Notes::default();
    let mut report = Table::new("guest CPU (oneios-cpu: percent of one CPU per period)", &["time", "line", "s", "all%", "top"]);
    let mut totals: BTreeMap<(String, i64), (i64, i64, i64)> = BTreeMap::new();
    let mut reports = 0i64;
    for r in &log.recs {
        let Some(text) = guest(r, "oneios-cpu: ") else { continue };
        let mut s = Scan::new(text);
        let Some(p) = period(&mut s) else {
            notes.unparsed("oneios-cpu", r.line);
            continue;
        };
        notes.seen("oneios-cpu", r);
        let entries = cpu_entries(s.rest().trim_start_matches(':').trim());
        let all = Scan::new(text).num("(all ");
        let top: Vec<String> = entries.iter().map(|(name, pid, pct)| format!("{name}[{pid}] {pct}%")).collect();
        let mut row = head(r);
        row.extend([Cell::Int(p as i64), Cell::int(all), Cell::text(top.join(", "))]);
        report.push(row);
        reports += 1;
        for (name, pid, pct) in entries {
            let e = totals.entry((name, pid)).or_insert((0, 0, 0));
            e.0 += 1;
            e.1 += pct;
            e.2 = e.2.max(pct);
        }
    }
    let mut summary = Table::new(
        "guest CPU by process (a process is listed in a report when it is among the top ones)",
        &["process", "pid", "reports", "sum%", "mean%_listed", "mean%_all_reports", "max%"],
    );
    let mut rows: Vec<_> = totals.into_iter().collect();
    rows.sort_by(|a, b| b.1 .1.cmp(&a.1 .1).then_with(|| a.0.cmp(&b.0)));
    for ((name, pid), (n, sum, max)) in rows {
        summary.push(vec![
            Cell::text(name),
            Cell::Int(pid),
            Cell::Int(n),
            Cell::Int(sum),
            Cell::Float(sum as f64 / n as f64, 1),
            Cell::Float(sum as f64 / reports.max(1) as f64, 1),
            Cell::Int(max),
        ]);
    }
    (vec![report, summary], notes.lines())
}

pub fn display(log: &Log) -> (Vec<Table>, Vec<String>) {
    let mut notes = Notes::default();
    let mut report = Table::new(
        "guest display report (oneios-display-report: counts since the boot)",
        &["time", "line", "prepare", "clients", "swaps", "swaps+", "accepted", "refused", "transfers", "retirements", "kicks", "in_flight", "not_ready", "no_head", "latch_busy", "fence", "copy", "blank", "flips", "latches", "retires"],
    );
    let mut flushes = Table::new("host display (xnuhost display: N s; copy us)", &["time", "line", "s", "flushes", "copy_mean", "copy_max", "MiB"]);
    let mut swaps = Table::new(
        "guest swaps (oneios-display-report: W records; t is the guest's clock; each record once)",
        &["swap", "line", "t", "since_last_ms", "offer_ms", "done_ms"],
    );
    // One row per report: the block's lines carry the same time.
    let mut open: Option<(usize, f64)> = None; // (row, time)
    let mut last_swaps: Option<f64> = None;
    let mut seen_swaps: BTreeMap<i64, ()> = BTreeMap::new();
    for r in &log.recs {
        if r.src == Src::Guest && r.text.starts_with("Darwin Kernel Version") {
            last_swaps = None;
            seen_swaps.clear();
            open = None;
        }
        if let Some(text) = guest(r, "oneios-display-report: ") {
            let time = r.time.unwrap_or(0.0);
            let block_line = text.starts_with("prepare ") || text.starts_with("clients ") || text.starts_with("swapstat ");
            if block_line {
                notes.seen("oneios-display-report", r);
                if !matches!(open, Some((_, t)) if (t - time).abs() < 0.5) {
                    let mut row = head(r);
                    row.extend(std::iter::repeat_n(Cell::None, report.columns.len() - 2));
                    report.push(row);
                    open = Some((report.rows.len() - 1, time));
                }
            }
            let row = open.map(|(i, _)| i);
            let mut s = Scan::new(text);
            if text.starts_with("prepare ") {
                if let (Some(row), Some(calls)) = (row, s.num("calls=")) {
                    report.rows[row][2] = Cell::Int(calls as i64);
                } else {
                    notes.unparsed("oneios-display-report: prepare", r.line);
                }
            } else if text.starts_with("clients ") {
                let (Some(row), Some(clients)) = (row, s.num("clients ")) else {
                    notes.unparsed("oneios-display-report: clients", r.line);
                    continue;
                };
                let validated = s.num("swaps validated ");
                let cells = &mut report.rows[row];
                cells[3] = Cell::Int(clients as i64);
                cells[4] = Cell::int(validated);
                cells[5] = match (validated, last_swaps) {
                    (Some(now), Some(before)) => Cell::Int((now - before) as i64),
                    (Some(now), None) => Cell::Int(now as i64),
                    _ => Cell::None,
                };
                if validated.is_some() {
                    last_swaps = validated;
                }
                cells[6] = Cell::int(s.num("(accepted "));
                cells[7] = Cell::int(s.num("refused "));
                cells[8] = Cell::int(s.num("transfers "));
                cells[9] = Cell::int(s.num("retirements "));
            } else if text.starts_with("swapstat ") {
                let Some(row) = row else { continue };
                for (i, label) in ["kicks ", "in-flight ", "not-ready ", "no-head ", "latch-busy ", "fence ", "copy ", "blank ", "flips ", "latches ", "retires "].iter().enumerate() {
                    report.rows[row][10 + i] = Cell::int(s.num(label));
                }
            } else if let Some(rest) = text.strip_prefix('W') {
                let mut s = Scan::new(rest);
                let Some(n) = s.num("") else {
                    notes.unparsed("oneios-display-report: W", r.line);
                    continue;
                };
                if seen_swaps.insert(n as i64, ()).is_some() {
                    continue;
                }
                swaps.push(vec![
                    Cell::Int(n as i64),
                    Cell::Int(r.line as i64),
                    Cell::float(s.num(" t="), 3),
                    Cell::int(s.num("s +")),
                    Cell::int(s.num("offer=")),
                    Cell::int(s.num("done=")),
                ]);
            }
        } else if let Some(text) = host(r, "display: ") {
            let mut s = Scan::new(text);
            let Some(p) = period(&mut s) else { continue };
            let Some(n) = s.num(": flushes ") else { continue };
            let mut row = head(r);
            row.extend([Cell::Int(p as i64), Cell::Int(n as i64), Cell::int(s.num("copy mean ")), Cell::int(s.num("max ")), Cell::int(s.num("("))]);
            flushes.push(row);
        }
    }
    (vec![report, flushes, swaps], notes.lines())
}

pub fn perf(log: &Log) -> (Vec<Table>, Vec<String>) {
    let mut notes = Notes::default();
    let mut guest_t = Table::new(
        "guest perf (oneios-perf: the kernel's share of user traps per period; us)",
        &["time", "line", "s", "user_stays", "syscalls", "bsd", "bsd_us", "mach", "mach_us", "platform", "platform_us", "faults", "filled", "filled_us", "vm_fault", "vm_fault_us", "refilled", "interrupts", "twins+", "twins-"],
    );
    let mut pmap = Table::new(
        "guest pmap (oneios-perf: pmap; the TLB lock in us and percent of a vCPU)",
        &["time", "line", "user_maps", "user_unmaps", "kernel_maps", "kernel_unmaps", "tlb_taken", "tlb_waited", "wait_us", "wait%", "held_us", "held%", "copyio", "copyio_faults"],
    );
    let mut calls = Table::new("guest top calls (oneios-perf: bsd / mach traps: number, calls, mean us)", &["time", "line", "kind", "number", "calls", "mean_us"]);
    let mut host_t = Table::new(
        "host perf (xnuhost perf: per period)",
        &["time", "line", "s", "task_traps", "per_s", "syscall", "fault", "interrupt", "other", "sleeps_vcpu", "sleeps_ios", "handoffs", "wakes_a", "wakes_b", "map_by_release", "map_ordered", "timer_set", "to_host_timer"],
    );
    let mut lat = Table::new("host latencies (xnuhost perf rows; us)", &["time", "line", "what", "n", "mean", "p50", "p90", "p99", "max"]);
    let mut cpus = Table::new("host vCPUs (xnuhost perf: cpuN; percent)", &["time", "line", "cpu", "core%", "kernel%", "ios_thread%", "idle%", "traps_per_s"]);
    for r in &log.recs {
        if let Some(text) = guest(r, "oneios-perf: ") {
            let body = text.trim_start();
            let mut s = Scan::new(body);
            if let Some(rest) = body.strip_prefix("pmap: ") {
                let mut s = Scan::new(rest);
                let Some(maps) = s.num("user maps ") else {
                    notes.unparsed("oneios-perf: pmap", r.line);
                    continue;
                };
                notes.seen("oneios-perf: pmap", r);
                let mut row = head(r);
                row.push(Cell::Int(maps as i64));
                for label in ["unmaps ", "kernel maps ", "unmaps ", "TLB lock taken ", ", "] {
                    row.push(Cell::int(s.num(label)));
                }
                row.push(Cell::int(s.num("waited (")));
                row.push(Cell::float(s.num("us waiting, "), 1));
                row.push(Cell::int(s.num("held ")));
                row.push(Cell::float(s.num("us ("), 1));
                row.push(Cell::int(s.num("copyio ")));
                row.push(Cell::int(s.num("(")));
                pmap.push(row);
            } else if body.starts_with("bsd (") || body.starts_with("mach traps:") {
                notes.seen("oneios-perf: calls", r);
                let kind = if body.starts_with("bsd") { "bsd" } else { "mach" };
                let list = body.split_once(": ").map_or("", |x| x.1);
                let words: Vec<&str> = list.split_whitespace().collect();
                let mut any = false;
                for triple in words.chunks(3) {
                    let [number, count, mean] = triple else { continue };
                    let (Ok(number), Ok(count), Some(Ok(mean))) =
                        (number.parse::<i64>(), count.parse::<i64>(), mean.strip_prefix('x').map(|m| m.parse::<i64>()))
                    else {
                        continue;
                    };
                    let mut row = head(r);
                    row.extend([Cell::text(kind), Cell::Int(number), Cell::Int(count), Cell::Int(mean)]);
                    calls.push(row);
                    any = true;
                }
                if !any {
                    notes.unparsed("oneios-perf: calls", r.line);
                }
            } else if let Some(p) = period(&mut s) {
                let Some(stays) = s.num(": ") else {
                    notes.unparsed("oneios-perf", r.line);
                    continue;
                };
                notes.seen("oneios-perf", r);
                let mut row = head(r);
                row.extend([Cell::Int(p as i64), Cell::Int(stays as i64)]);
                for label in ["user stays; ", "(bsd ", ", ", "mach ", ", ", "platform ", ", ", "us); ", "faults, ", "(", "us), ", "(", "us), ", "refilled; ", "twins +", " -"] {
                    row.push(Cell::int(s.num(label)));
                }
                guest_t.push(row);
            }
        } else if let Some(text) = host(r, "perf: ") {
            let body = text.trim_start();
            let mut s = Scan::new(body);
            if let Some(rest) = body.strip_prefix("cpu") {
                let mut s = Scan::new(rest);
                let Some(n) = s.num("") else { continue };
                let mut row = head(r);
                row.push(Cell::Int(n as i64));
                for label in [": ", "of its wall time ", "kernel, ", "turn, ", "idle; "] {
                    row.push(Cell::int(s.num(label)));
                }
                cpus.push(row);
            } else if let Some(at) = body.find("  n ") {
                // `up (trap->vCPU)    n    46933  mean   12.52  p50 ...`
                let what = body[..at].trim();
                let mut s = Scan::new(&body[at..]);
                let mut row = head(r);
                row.push(Cell::text(what));
                let mut field = |label: &str, decimals: usize| {
                    s.skip(label);
                    let rest = s.rest().trim_start();
                    let mut inner = Scan::new(rest);
                    Cell::float(inner.num(""), decimals)
                };
                let n = field(" n ", 0);
                row.push(match n {
                    Cell::Float(v, _) => Cell::Int(v as i64),
                    other => other,
                });
                for (label, decimals) in [("mean", 2), ("p50", 2), ("p90", 2), ("p99", 2), ("max", 1)] {
                    row.push(field(label, decimals));
                }
                lat.push(row);
            } else if let Some(p) = period(&mut s) {
                let Some(traps) = s.num(": ") else {
                    notes.unparsed("xnuhost perf", r.line);
                    continue;
                };
                let mut row = head(r);
                row.extend([Cell::Float(p, 1), Cell::Int(traps as i64)]);
                for label in ["task traps (", "syscall ", "fault ", "interrupt ", "other ", "vCPU ", "iOS thread ", "(of ", "futex wakes ", "+ ", "task_map ", "release, ", "timer_set ", "("] {
                    row.push(Cell::int(s.num(label)));
                }
                host_t.push(row);
            }
        }
    }
    (vec![guest_t, pmap, calls, host_t, lat, cpus], notes.lines())
}
