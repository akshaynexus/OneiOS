//! `log summary`: a session log's health on one screen. For a boot (the last one, or every one with `--all`):
//! the boot's stages and their times (docs/booter.md "Boot stages"), then one row per part of xnuhost and of the guest
//! (docs/booter.md "Health lines": which line each part writes): how many lines it wrote, its last line (its state),
//! the first line that names a problem; then the crashes and kills, the gestures, and the `xnuhost E` lines.
//! `--since T` (log seconds; `+T`: seconds after the boot's start) and `--last S` (the log's last S seconds) narrow
//! the rows to a window; the stages are always the whole boot's.

use crate::parse::{boots, Log, Rec, Src};
use std::fmt::Write;

/// The window of `--since` / `--last`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Window {
    /// `--since T`: log seconds; with `from_boot`, seconds after the boot's start.
    pub since: Option<f64>,
    pub from_boot: bool,
    /// `--last S`: the last S seconds of the boot.
    pub last: Option<f64>,
}

impl Window {
    /// `--since`'s text: `4900` (log seconds) or `+120` (after the boot's start).
    pub fn parse_since(text: &str) -> Option<Window> {
        let (from_boot, number) = match text.strip_prefix('+') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        number.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0).map(|v| Window { since: Some(v), from_boot, last: None })
    }

    /// The window's bounds in log seconds for a boot that starts at `start` and ends at `end`.
    fn bounds(&self, start: f64, end: f64) -> (f64, f64) {
        let mut from = f64::NEG_INFINITY;
        if let Some(s) = self.since {
            from = if self.from_boot { start + s } else { s };
        }
        if let Some(l) = self.last {
            from = from.max(end - l);
        }
        (from, f64::INFINITY)
    }

    fn is_set(&self) -> bool {
        self.since.is_some() || self.last.is_some()
    }
}

/// A part of the session and the records that are its lines.
pub struct Part {
    pub name: &'static str,
    host: &'static [&'static str],
    guest: &'static [&'static str],
    raw: &'static [&'static str],
}

/// The parts, in the order of the boot. A record belongs to the first part one of whose prefixes it starts with.
pub const PARTS: &[Part] = &[
    Part { name: "start", host: &["xnuhost:", "host:", "hw:", "placed:", "kernel:", "trust caches:", "text fixups:"], guest: &[], raw: &["oneios inner:"] },
    Part { name: "memory", host: &["memory:"], guest: &[], raw: &[] },
    Part { name: "disks", host: &["disk "], guest: &[], raw: &[] },
    Part { name: "virtio", host: &["virtio"], guest: &[], raw: &[] },
    Part { name: "tasks", host: &["tasks:", "thread "], guest: &[], raw: &[] },
    Part { name: "cpu", host: &["cpu"], guest: &[], raw: &[] },
    Part { name: "perf", host: &["perf:", "emulated:"], guest: &[], raw: &[] },
    Part { name: "thermal", host: &["thermal:"], guest: &[], raw: &[] },
    Part { name: "display", host: &["display:", "guest ready"], guest: &[], raw: &[] },
    Part { name: "gpu", host: &["gpu:", "drm2kgsl:"], guest: &[], raw: &[] },
    Part { name: "input", host: &["input:"], guest: &[], raw: &[] },
    Part { name: "brightness", host: &["brightness:"], guest: &[], raw: &[] },
    Part { name: "haptics", host: &["haptics:"], guest: &[], raw: &[] },
    Part { name: "battery", host: &["battery:"], guest: &[], raw: &[] },
    Part { name: "bluetooth", host: &["bluetooth:"], guest: &[], raw: &[] },
    Part { name: "wifi", host: &["wifi:"], guest: &[], raw: &[] },
    Part { name: "audio", host: &["audio:"], guest: &[], raw: &["oneios-audio:"] },
    Part { name: "camera", host: &["camera:"], guest: &[], raw: &["oneios-camera:"] },
    // the media device's: the encoder's lines, the decoder's (2026-11-24) and its worker's wake-ups (`media: 30 s`, 2026-11-25)
    Part { name: "encoder", host: &["encoder:", "decoder:", "media:"], guest: &["ONEIOS CODEC:"], raw: &["oneios-encoder:"] },
    Part { name: "usb", host: &["usb:"], guest: &[], raw: &[] },
    Part { name: "return", host: &["stopping:", "return:", "exit ", "the guest asked for a restart", "the guest restarted"], guest: &[], raw: &[] },
    Part { name: "ios launchd", host: &[], guest: &["com.apple.xpc.launchd|"], raw: &[] },
    Part { name: "ios memory", host: &[], guest: &["oneios-mem:", "memorystatus:"], raw: &[] },
    Part { name: "ios cpu", host: &[], guest: &["oneios-cpu", "oneios-top:", "oneios-perf:"], raw: &[] },
    Part { name: "ios gpu", host: &[], guest: &["oneios-gpu"], raw: &[] },
    Part { name: "ios display", host: &[], guest: &["oneios-display"], raw: &[] },
    Part { name: "ios hid", host: &[], guest: &["oneios-hid:"], raw: &[] },
    Part { name: "ios wifi", host: &[], guest: &["ONEIOS WIFI:"], raw: &[] },
    Part { name: "ios iokit", host: &[], guest: &["oneios-iobusy:", "oneios-iokit:", "oneios-keystore:"], raw: &[] },
];

/// The part a record belongs to (an index into PARTS), None for the guest's other lines (Apple's own).
pub fn part_of(r: &Rec) -> Option<usize> {
    let t = r.text.as_str();
    PARTS.iter().position(|p| {
        let prefixes = match r.src {
            Src::Host(_) => p.host,
            Src::Guest => p.guest,
            Src::Raw => p.raw,
        };
        prefixes.iter().any(|x| t.starts_with(x))
    })
}

const PROBLEM_WORDS: &[&str] = &[
    "failed", "error", "refused", "timed out", "not answered", "none returned", "not taken", "dropped", "lost", "is gone",
    "did not", "could not", "stuck", "died on its own", "FAILED", "DROPPED", "panic", "no room",
];

/// Whether a line names a problem: one of PROBLEM_WORDS in a clause (split at `,;()`) that is not a zero count
/// (`0 failed`, `+0 dropped`, `0 of the guest's refused`, `failed 0`).
pub fn problem(text: &str) -> bool {
    text.split([',', ';', '(', ')']).any(|clause| {
        let c = clause.trim();
        let words: Vec<&str> = c.split_whitespace().collect();
        let zero = |w: Option<&&str>| matches!(w.map(|w| w.trim_end_matches([':', '.'])), Some("0") | Some("+0") | Some("0.0"));
        if zero(words.first()) || zero(words.last()) {
            return false;
        }
        PROBLEM_WORDS.iter().any(|w| match c.find(w) {
            // Only as words ("-no_slto_panic" is a boot argument), and not next to a zero ("refused 0 calls").
            Some(at) => {
                let before = c[..at].chars().next_back();
                let after = c[at + w.len()..].chars().next();
                let next = c[at + w.len()..].split_whitespace().next();
                let prev = c[..at].split_whitespace().next_back();
                !before.is_some_and(word_char) && !after.is_some_and(word_char) && !zero(next.as_ref()) && !zero(prev.as_ref())
            }
            None => false,
        })
    })
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max.saturating_sub(3)).collect();
        format!("{cut}...")
    }
}

/// One stage of a boot: its name and the first record that shows it.
pub struct Stage<'a> {
    pub name: &'static str,
    pub rec: Option<&'a Rec>,
}

fn host(r: &Rec, prefix: &str) -> bool {
    matches!(r.src, Src::Host(_)) && r.text.starts_with(prefix)
}

fn guest(r: &Rec, prefix: &str) -> bool {
    r.src == Src::Guest && r.text.starts_with(prefix)
}

/// The stages of a boot's records (docs/booter.md "Boot stages").
pub fn stages(recs: &[Rec]) -> Vec<Stage<'_>> {
    type Test = fn(&Rec) -> bool;
    let list: [(&'static str, Test); 14] = [
        ("xnuhost starts", |r| host(r, "xnuhost: session ABI")),
        ("RAM held and locked", |r| host(r, "memory: the guest's RAM is held:")),
        ("the kernel starts", |r| host(r, "cpu0: entering the kernel") || guest(r, "Darwin Kernel Version")),
        ("BSD root mounted", |r| guest(r, "BSD root: ")),
        ("launchd loaded", |r| guest(r, "load_init_program: attempting to load /sbin/launchd")),
        ("launchd's first line", |r| guest(r, "com.apple.xpc.launchd|")),
        ("the guest's drivers: first up", |r| host(r, "virtio ") && r.text.contains(": the guest's driver is up")),
        ("SpringBoard first seen", |r| r.src == Src::Guest && (r.text.contains("SpringBoard[") || r.text.contains("crashed: SpringBoard."))),
        ("backboardd first seen", |r| r.src == Src::Guest && r.text.contains("backboardd[")),
        ("backboardd opens the framebuffer", |r| guest(r, "oneios-display: framebuffer opened by backboardd")),
        ("the guest's first swap", |r| guest(r, "oneios-display: swap ")),
        ("the guest's first frame", |r| host(r, "display: the guest's first frame") || host(r, "guest ready: first frame")),
        ("guest ready", |r| host(r, "guest ready")),
        ("the first gesture", |r| host(r, "input: touch: ")),
    ];
    let mut out: Vec<Stage> = list.iter().map(|(name, _)| Stage { name, rec: None }).collect();
    for r in recs {
        for (i, (_, test)) in list.iter().enumerate() {
            if out[i].rec.is_none() && test(r) {
                out[i].rec = Some(r);
            }
        }
    }
    out
}

/// How xnuhost ended as the fork's inner shell saw it (`oneios inner: DATE TIME xnuhost PID ended: exit N` or `killed
/// by signal S (NAME)`, the OneOS app after 0.5.80, docs/booter.md 2.9): the record and its words after "ended: ".
fn inner_end(recs: &[Rec]) -> Option<(&Rec, &str)> {
    recs.iter().rev().find_map(|r| match r.src {
        Src::Raw if r.text.starts_with("oneios inner:") && r.text.contains(" xnuhost ") => {
            r.text.split_once(" ended: ").map(|(_, how)| (r, how))
        }
        _ => None,
    })
}

/// The host's pressure at a boot's end (docs/booter.md 2.2, "The host's pressure"): xnuhost's last `memory: the host is
/// short of memory: ...` line when no `memory: the host's shortage is over` came after it; the record and its words.
fn pressure_at_end(recs: &[Rec]) -> Option<(&Rec, &str)> {
    const SHORT: &str = "memory: the host is short of memory: ";
    let r = recs.iter().rev().find(|r| host(r, SHORT) || host(r, "memory: the host's shortage is over"))?;
    r.text.strip_prefix(SHORT).map(|why| (r, why))
}

/// How the boot ended: the record and what it says, or None (the log stops while the session ran, or was cut).
fn ending(recs: &[Rec]) -> Option<(&Rec, String)> {
    let signal = |r: &Rec| r.src == Src::Raw && r.text.starts_with("xnuhost !!");
    let i = recs.iter().rposition(|r| {
        let t = r.text.as_str();
        match r.src {
            Src::Host(_) => t.starts_with("exit ") || t.starts_with("stopping: ") || t.starts_with("the guest asked for a restart"),
            Src::Guest => t.starts_with("panic(") || oneios_common::panic_head(t).is_some(),
            Src::Raw => signal(r),
        }
    })?;
    let r = &recs[i];
    match r.src {
        Src::Host(_) => Some((r, r.text.clone())),
        Src::Guest => Some((r, format!("PANIC: {}", short(&r.text, 90)))),
        // Signal context writes a head line and indented ones after it (pc, sp, frames): the head of the last group.
        Src::Raw => {
            let mut h = i;
            while h > 0 && signal(&recs[h - 1]) && recs[h].text.starts_with("xnuhost !!  ") {
                h -= 1;
            }
            Some((&recs[h], short(&recs[h].text, 90)))
        }
    }
}

#[derive(Default)]
struct PartStats<'a> {
    lines: usize,
    errors: usize,
    problems: usize,
    first_problem: Option<&'a Rec>,
    last: Option<&'a Rec>,
}

/// The whole report.
pub fn report(log: &Log, window: Window, all: bool) -> String {
    let mut out = String::new();
    let all_boots = boots(log);
    let _ = writeln!(out, "{}: {} lines, {} boot(s)", log.path, log.lines, all_boots.len());
    // Which runtime, kernel, image and app made the log (xnuhost's `versions:` line, docs/booter.md 2.9b): the last one,
    // which a tail of the log has too (xnuhost logs it again at the stop); a log of several says how many differ.
    let versions: Vec<&Rec> = log.recs.iter().filter(|r| host(r, "versions: ")).collect();
    if let Some(last) = versions.last() {
        let mut texts: Vec<&str> = versions.iter().map(|r| r.text.as_str()).collect();
        texts.sort_unstable();
        texts.dedup();
        let others = if texts.len() > 1 { format!("; {} different ones in the log", texts.len()) } else { String::new() };
        let _ = writeln!(out, "{} (line {}{})", last.text, last.line, others);
    } else {
        let _ = writeln!(out, "versions: no line (an xnuhost before v0.1.19, or a part of the log without one)");
    }
    if all_boots.is_empty() {
        return out;
    }
    let shown: Vec<usize> = if all { (0..all_boots.len()).collect() } else { vec![all_boots.len() - 1] };
    for b in shown {
        let boot = &all_boots[b];
        let recs = &log.recs[boot.first..boot.end];
        let start = boot.start_time.unwrap_or(0.0);
        let end = recs.iter().rev().find_map(|r| r.time).unwrap_or(start);
        let (from, to) = window.bounds(start, end);
        let at = |t: Option<f64>| t.map_or("-".to_string(), |t| format!("+{:.1} s", t - start));
        let _ = writeln!(
            out,
            "\nboot {} of {} (lines {}-{}), {:.1} s of log{}",
            b + 1,
            all_boots.len(),
            boot.first_line,
            boot.last_line,
            end - start,
            if window.is_set() { format!("; rows from +{:.1} s ({:.3} in the log's clock)", (from - start).max(0.0), from.max(start)) } else { String::new() }
        );

        // ---- stages
        let _ = writeln!(out, "stages:");
        let stages = stages(recs);
        for s in &stages {
            match s.rec {
                Some(r) => {
                    let _ = writeln!(out, "  {:<34} {:>10}  line {:>6}", s.name, at(r.time), r.line);
                }
                None => {
                    let _ = writeln!(out, "  {:<34} {:>10}  (no line)", s.name, "-");
                }
            }
        }
        match ending(recs) {
            Some((r, what)) => {
                let _ = writeln!(out, "  {:<34} {:>10}  line {:>6}  {}", "the end", at(r.time), r.line, short(&what, 70));
            }
            None => {
                let _ = writeln!(out, "  {:<34} {:>10}  (no stop line: the session still ran, or xnuhost was killed)", "the end", at(Some(end)));
            }
        }
        if let Some((r, how)) = inner_end(recs) {
            let _ = writeln!(out, "  {:<34} {:>10}  line {:>6}  {}", "how xnuhost ended", at(r.time), r.line, short(how, 70));
        }
        if let Some((r, why)) = pressure_at_end(recs) {
            let _ = writeln!(out, "  {:<34} {:>10}  line {:>6}  {}", "host short of memory at the end", at(r.time), r.line, short(why, 70));
        }
        let stage_time = |name: &str| stages.iter().find(|s| s.name == name).and_then(|s| s.rec).and_then(|r| r.time);
        if let (Some(first), Some(ready)) = (stage_time("the guest's first frame"), stage_time("guest ready")) {
            if ready - first >= 10.0 {
                let _ = writeln!(out, "  NOTE: ready came {:.1} s after the guest's first frame (ready-content-wait: the frames read as black)", ready - first);
            }
        }

        // ---- parts
        let mut stats: Vec<PartStats> = (0..PARTS.len()).map(|_| PartStats::default()).collect();
        let mut errors = 0usize;
        let mut first_error: Option<&Rec> = None;
        let (mut panics, mut exceptions, mut traps, mut died, mut jetsam, mut held) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
        let (mut gestures, mut no_frame) = (0usize, 0usize);
        for r in recs {
            if r.time.is_some_and(|t| t < from || t > to) || (r.time.is_none() && window.is_set()) {
                continue;
            }
            let t = r.text.as_str();
            let is_error = matches!(r.src, Src::Host('E'));
            if is_error {
                errors += 1;
                first_error.get_or_insert(r);
            }
            match r.src {
                Src::Guest if t.starts_with("panic(") || oneios_common::panic_head(t).is_some() => panics += 1,
                Src::Guest if t.starts_with("sheng-exc: ") && t.contains("] exception ") => exceptions += 1,
                Src::Guest if t.starts_with("memorystatus: ") && (t.contains("SIGKILL") || t.contains("killing")) => jetsam += 1,
                Src::Host(_) if t.starts_with("tasks: pid ") && t.contains(" trap kind ") => traps += 1,
                Src::Host(_) if t.starts_with("tasks: pid ") && t.contains(" died on its own") => died += 1,
                Src::Host(_) if t.starts_with("virtio ") && t.contains(" none returned in ") => held += 1,
                Src::Host(_) if t.starts_with("input: touch: ") => {
                    gestures += 1;
                    if t.contains("no guest frame after the touch-down") {
                        no_frame += 1;
                    }
                }
                _ => {}
            }
            let Some(p) = part_of(r) else { continue };
            let s = &mut stats[p];
            s.lines += 1;
            s.last = Some(r);
            if is_error || problem(t) {
                s.problems += 1;
                if is_error {
                    s.errors += 1;
                }
                s.first_problem.get_or_insert(r);
            }
        }
        let _ = writeln!(out, "parts (lines, problems, the last line, the first problem):");
        for (p, s) in stats.iter().enumerate() {
            let name = PARTS[p].name;
            let Some(last) = s.last else {
                let _ = writeln!(out, "  {name:<12} {:>5}  -", 0);
                continue;
            };
            let problems = if s.problems == 0 { "-".to_string() } else if s.errors > 0 { format!("{} ({} E)", s.problems, s.errors) } else { s.problems.to_string() };
            let _ = writeln!(out, "  {name:<12} {:>5} {:>8}  {} {}", s.lines, problems, at(last.time), short(&last.text, 100));
            if let Some(f) = s.first_problem {
                let _ = writeln!(out, "  {:<12} {:>5} {:>8}  first: {} (line {})", "", "", at(f.time), short(&f.text, 90), f.line);
            }
        }
        let _ = writeln!(
            out,
            "crashes and kills: {panics} panic(s), {exceptions} exception(s) (sheng-exc), {traps} fatal trap line(s) of tasks, {died} task(s) died on their own, {jetsam} jetsam kill(s); {held} virtio queue(s) held chains"
        );
        if gestures > 0 {
            let _ = writeln!(out, "gestures: {gestures}, {no_frame} with no guest frame after the touch-down (xnuhost's word)");
        }
        match first_error {
            Some(r) => {
                let _ = writeln!(out, "`xnuhost E` lines: {errors}; the first at {} (line {}): {}", at(r.time), r.line, short(&r.text, 100));
            }
            None => {
                let _ = writeln!(out, "`xnuhost E` lines: 0");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{part_of, problem, PARTS};
    use crate::parse::{Rec, Src};

    #[test]
    fn the_media_devices_lines_are_the_encoders() {
        for text in [
            "encoder: session 3: HEVC 1920x1080 on mediacodec-helper: 8-bit",
            "decoder: session 4: VP9 1920x1080 on mediacodec-helper",
            "media: 30 s: worker woken 924: by data (commands, kicks, results, coded units) 903, by the timeout 21; 1 ms looks 0",
        ] {
            let r = Rec { line: 1, time: None, src: Src::Host('I'), text: text.into(), appended: false, head_cut: false, tail_cut: false };
            assert_eq!(part_of(&r).map(|p| PARTS[p].name), Some("encoder"), "{text}");
        }
    }

    #[test]
    fn problems_and_zero_counts() {
        for line in [
            "display: 10 s: 0 frames copied by the helper from guest RAM (posted to in the layer: mean 0.0 ms, max 0.0 ms), 0 not answered within 100 ms, 1 flushes of xnuhost's own copy",
            "memory: host MemAvailable 1569 MiB of 11084 MiB; given back 0 MiB of free pages in 0 calls (0 failed, 0 ms)",
            "ONEIOS WIFI: 60 s: scans asked 10 (started 5, reused 5, joined 0, refused 0), done 10 (40 networks), timed out 0; records 386",
            "wifi: closed (0 frames to the guest, 0 from it, 0 cut from coalesced frames, 0 dropped, 0 of the guest's refused)",
            "placed: boot-args \"debug=0x14e -no_slto_panic ml-timeout-lock-panic-scale=40\"",
            "oneios-gpu-kinds: MercuryPosterEx own task 19 (8 MiB); made +21 gone +3",
            "memory: the guest's RAM is held: 5632 MiB prefaulted in 3009 ms, locked (mlock: the host cannot swap it)",
            "memory: fixed RAM: held and locked; prefaulted 5632 of 5632 MiB, give-back refused 0 calls; since the last line: host swap-ins +171",
            "media: 30 s: worker woken 924: by data (commands, kicks, results, coded units) 903, by the timeout 21; 1 ms looks 0",
            "decoder: 60 s: decoded H.264 0, HEVC 0, VP9 1790, AV1 0 of 1791 fed; decode time avg 6.1 ms, max 19 ms; dropped 0; sessions 1, opens refused 0 (mediacodec-helper)",
        ] {
            assert!(!problem(line), "{line}");
        }
        for line in [
            "display: the helper's socket is gone (Connection reset by peer): closed",
            "input: sec_touchscreen: events dropped by the kernel (the reader was too slow, 2 times)",
            "virtio gpu#0: queue 0: 2 chain(s) in flight and none returned in 60 s (taken 9, returned 7 since the boot)",
            "tasks: pid 4242 died on its own (wait status 0x9: signal 9), not asked by the kernel",
            "disk data: a flush failed: I/O error",
            "audio: speaker latency not measured (I/O error): the kernel's default output latency",
            "input: OneiOS touchscreen: the guest takes events again (12 lost since the boot)",
        ] {
            assert!(problem(line), "{line}");
        }
    }
}
