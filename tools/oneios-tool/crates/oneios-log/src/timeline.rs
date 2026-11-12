//! `log timeline`: the milestones of every boot of a log with their time and line: the stages of
//! `oneos/interface.md` (xnuhost starts, the kernel, BSD root, launchd, backboardd, SpringBoard, guest ready), the
//! frames the display logged, the gestures with whether the guest drew after them, crashes, and the end.

use crate::parse::{boots, Log, Rec, Scan, Src};
use std::collections::BTreeMap;
use std::fmt::Write;

/// How long after a gesture's lift a swap still counts as its answer.
pub const AFTER_LIFT_S: f64 = 0.5;

fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}...")
    }
}

/// The swaps a boot's `oneios-display-report: W` records hold, by number, in the host's clock.
struct Swaps {
    /// number -> the guest clock's seconds.
    guest_t: BTreeMap<u64, f64>,
    /// The guest clock minus the log's clock, and how it was found.
    offset: Option<(f64, &'static str)>,
    /// (the log's time, swaps validated) of every display report.
    totals: Vec<(f64, u64)>,
}

impl Swaps {
    fn collect(recs: &[Rec]) -> Swaps {
        let mut guest_t = BTreeMap::new();
        let mut accepted: BTreeMap<u64, f64> = BTreeMap::new();
        let mut totals = Vec::new();
        let mut floor: Option<f64> = None;
        for r in recs {
            if r.src != Src::Guest {
                continue;
            }
            let Some(time) = r.time else { continue };
            if let Some(text) = r.text.strip_prefix("oneios-display-report: ") {
                let mut s = Scan::new(text);
                if let Some(rest) = text.strip_prefix('W') {
                    let mut s = Scan::new(rest);
                    if let (Some(n), Some(t)) = (s.num(""), s.num(" t=")) {
                        guest_t.entry(n as u64).or_insert(t);
                        // A record is printed after its swap: the offset is at least t - the print time.
                        floor = Some(floor.map_or(t - time, |f: f64| f.max(t - time)));
                    }
                } else if text.starts_with("clients ") {
                    if let Some(v) = s.num("swaps validated ") {
                        totals.push((time, v as u64));
                    }
                }
            } else if let Some(text) = r.text.strip_prefix("oneios-display: swap ") {
                let mut s = Scan::new(text);
                if let Some(n) = s.num("") {
                    accepted.entry(n as u64).or_insert(time);
                }
            }
        }
        // The exact offset: a swap whose own line (`oneios-display: swap N accepted`) is in the log.
        let exact = accepted.iter().find_map(|(n, time)| guest_t.get(n).map(|t| t - time));
        let offset = exact.map(|o| (o, "exact")).or(floor.map(|o| (o, "a lower bound")));
        Swaps { guest_t, offset, totals }
    }

    /// The swaps between two times of the log's clock: (count, whether the records hold every swap of the span).
    fn between(&self, from: f64, to: f64) -> Option<(usize, bool)> {
        let (offset, _) = self.offset?;
        let host = |t: f64| t - offset;
        let inside: Vec<u64> = self.guest_t.iter().filter(|(_, &t)| host(t) >= from && host(t) <= to).map(|(&n, _)| n).collect();
        let before = self.guest_t.iter().filter(|(_, &t)| host(t) < from).map(|(&n, _)| n).max();
        let after = self.guest_t.iter().filter(|(_, &t)| host(t) > to).map(|(&n, _)| n).min();
        let last_known = self.guest_t.keys().next_back().copied().unwrap_or(0);
        let low = before.unwrap_or(1);
        let high = match after {
            Some(n) => n,
            // No later record: a later report's total says whether a swap came after the last record.
            None => match self.totals.iter().find(|(time, _)| *time >= to) {
                Some((_, total)) if *total <= last_known => last_known,
                _ => return Some((inside.len(), false)),
            },
        };
        let complete = (low..=high).all(|n| self.guest_t.contains_key(&n));
        Some((inside.len(), complete))
    }
}

pub fn report(log: &Log) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}: {} lines", log.path, log.lines);
    for (b, boot) in boots(log).iter().enumerate() {
        let recs = &log.recs[boot.first..boot.end];
        let swaps = Swaps::collect(recs);
        let start = boot.start_time;
        let _ = writeln!(out, "\nboot {} (lines {}-{})", b + 1, boot.first_line, boot.last_line);
        match swaps.offset {
            Some((offset, how)) => {
                let _ = writeln!(
                    out,
                    "  swaps: {} W record(s); the guest's clock is the log's + {offset:.3} s ({how}); a gesture's swaps are counted from its touch-down to {} ms after its lift",
                    swaps.guest_t.len(),
                    (AFTER_LIFT_S * 1000.0) as u32
                );
            }
            None => {
                let _ = writeln!(out, "  swaps: no W records (oneios-display-report is off or printed nothing): gestures have xnuhost's own word only");
            }
        }
        let _ = writeln!(out, "  {:>10}  {:>9}  {:>6}  what", "time", "+boot", "line");
        let mut row = |r: &Rec, what: String| {
            let time = r.time.map_or("-".to_string(), |t| format!("{t:.3}"));
            let since = match (r.time, start) {
                (Some(t), Some(s)) => format!("+{:.3}", t - s),
                _ => "-".to_string(),
            };
            let _ = writeln!(out, "  {time:>10}  {since:>9}  {:>6}  {what}", r.line);
        };
        let mut seen: BTreeMap<String, ()> = BTreeMap::new();
        let mut first = |key: &str| seen.insert(key.to_string(), ()).is_none();
        let mut errors = 0usize;
        let mut last_total: Option<u64> = None;
        for r in recs {
            let t = r.text.as_str();
            match r.src {
                Src::Host(level) => {
                    if level == 'E' {
                        errors += 1;
                    }
                    if t.starts_with("xnuhost: session ABI") {
                        row(r, format!("xnuhost starts: {}", t.trim_start_matches("xnuhost: ")));
                    } else if t.starts_with("cpu0: entering the kernel at") {
                        row(r, "cpu0 enters the kernel".to_string());
                    } else if t.starts_with("guest ready") {
                        row(r, format!("GUEST READY ({t})"));
                    } else if let Some(touch) = t.strip_prefix("input: touch: ") {
                        let mut s = Scan::new(touch);
                        let frames = s.num("");
                        let span_ms = s.num(" in ");
                        let own = if touch.contains("no guest frame after the touch-down") {
                            "xnuhost: NO guest frame from the touch-down to the lift".to_string()
                        } else if let Some(ms) = Scan::new(touch).num("the guest's next frame ") {
                            format!("xnuhost: the guest's next frame {ms} ms after the touch-down")
                        } else {
                            "xnuhost: not measured (the screen was not still at the touch-down)".to_string()
                        };
                        let counted = match (r.time, span_ms) {
                            (Some(end), Some(ms)) => match swaps.between(end - ms / 1000.0, end + AFTER_LIFT_S) {
                                Some((n, true)) => format!("; W records: {n} swap(s)"),
                                Some((n, false)) => format!("; W records: {n} swap(s) or more (they do not hold every swap of that time)"),
                                None => String::new(),
                            },
                            _ => String::new(),
                        };
                        let what = match (frames, span_ms) {
                            (Some(f), Some(ms)) => format!("{f} touch frames in {ms} ms"),
                            _ => short(touch, 60),
                        };
                        row(r, format!("gesture: {what}; {own}{counted}"));
                    } else if t.starts_with("stopping: ") || t.starts_with("exit ") || t.starts_with("the guest asked for a restart") || t.starts_with("the guest restarted") {
                        row(r, format!("END: {t}"));
                    } else if t.starts_with("tasks: pid ") && t.contains(" signal ") {
                        row(r, format!("host: a task trapped: {}", short(t.trim_start_matches("tasks: "), 110)));
                    }
                }
                Src::Raw => {
                    if t.starts_with("xnuhost !!") {
                        row(r, short(t, 140));
                    }
                }
                Src::Guest => {
                    if t.starts_with("Darwin Kernel Version") {
                        row(r, format!("kernel starts: {}", short(t, 110)));
                    } else if t.starts_with("BSD root: ") || t.starts_with("load_init_program: attempting to load") {
                        row(r, t.to_string());
                    } else if t.starts_with("com.apple.xpc.launchd|") {
                        if first("launchd") {
                            row(r, format!("launchd's first line: {}", short(t, 100)));
                        }
                    } else if t.starts_with("panic(") {
                        row(r, format!("PANIC: {}", short(t, 160)));
                    } else if let Some(rest) = t.strip_prefix("sheng-exc: ") {
                        if rest.contains("] exception ") {
                            row(r, format!("CRASH: {rest}"));
                        }
                    } else if t.starts_with("sheng-exit: ") || (t.starts_with("Process[") && t.contains("] crashed: ")) || t.contains(": killed] exiting with signal") {
                        row(r, format!("died: {}", short(t, 120)));
                    } else if let Some(rest) = t.strip_prefix("oneios-display: ") {
                        if rest.starts_with("swap ") || rest.starts_with("frame ") {
                            row(r, format!("display: {rest}"));
                        } else if let Some(by) = rest.strip_prefix("framebuffer opened by ") {
                            let name = by.split('[').next().unwrap_or(by);
                            if first(&format!("fb {name}")) {
                                row(r, format!("display: framebuffer opened by {}", short(by, 60)));
                            }
                        }
                    } else if let Some(rest) = t.strip_prefix("oneios-display-report: clients ") {
                        if let Some(total) = Scan::new(rest).num("swaps validated ") {
                            let total = total as u64;
                            let more = total.saturating_sub(last_total.unwrap_or(0));
                            if more > 0 {
                                row(r, format!("display report: {more} swap(s) since the report before ({total} since the boot)"));
                            }
                            last_total = Some(total);
                        }
                    } else if t.starts_with("oneios-iobusy: ") && t.contains(" services busy") {
                        // The report's last line; the busy services are the `busy N:` lines before it.
                        row(r, short(t, 140));
                    }
                    if t.contains("backboardd[") && first("backboardd") {
                        row(r, format!("backboardd first seen: {}", short(t, 90)));
                    }
                    if (t.contains("SpringBoard[") || t.contains("crashed: SpringBoard.")) && first("SpringBoard") {
                        row(r, format!("SpringBoard first seen: {}", short(t, 90)));
                    }
                }
            }
        }
        let _ = writeln!(out, "  ({errors} `xnuhost E` line(s) in this boot)");
    }
    out
}
