//! `ioslog checks`: how often the known problems show in a boot's log, with the first and last time and a few
//! example lines each.
//!
//! The patterns are the texts the project's notes quote from iOS's log (docs/agents.md, docs/newtodo.md,
//! docs/hardware.md). They were written without the phone's log at hand (2026-11-12): a count of 0 means "this
//! text is not in the log", not "the problem is absent". Add a row when a new text is found.

use crate::{open, pick_boot, text_line, time_text, From};
use anyhow::Result;
use macos_unifiedlogs::logarchive::visit_provider;

/// (name, what it means, the message pattern).
const CHECKS: &[(&str, &str, &str)] = &[
    ("touch-paths", "touch events dropped: more paths than backboardd's TouchEvents takes", r"MAX_PATH_COUNT"),
    ("sensor-mode", "UISensor mode changes (the digitizer switched off, pocket touches expected)", r"digitizerEnabled|pocketTouchesExpected"),
    ("backlight", "backlight control: the VM backlight path, the display's container, the lock reason", r"VMBLControl|Set VM Mode|No container for handle Display|BacklightLockReason"),
    ("crashes", "crash reports and the corpse limit", r"Too many corpses|report for corpse|exited due to SIG|crashed"),
    ("launch-constraints", "AMFI launch constraint violations", r"Launch Constraint Violation"),
    ("jetsam", "memory pressure kills", r"memorystatus: killing|JetsamEvent|jetsam"),
    ("watchdogs", "launchd's extension check-in timeout, 0x8BADF00D, failed graceful exits", r"Extension is hanging on launch|CHECK_IN_TIMEOUT|8BADF00D|Failed to terminate gracefully"),
    ("audio", "the HAL I/O thread's realtime policy, mixer timeouts, haptics", r"configure_thread_for_realtime|timed out after 3\.000s|IO Stopped Context|Haptics: unsupported"),
];

struct Hit {
    count: u64,
    first: f64,
    last: f64,
    examples: Vec<String>,
}

pub(crate) fn run(from: From, boot: &str, examples: usize) -> Result<()> {
    let source = open(&from)?;
    let boot = pick_boot(&source, boot)?;
    // one pass over the message for all checks
    let any = regex::RegexSet::new(CHECKS.iter().map(|(_, _, p)| *p))?;
    let mut hits: Vec<Hit> = CHECKS.iter().map(|_| Hit { count: 0, first: f64::MAX, last: f64::MIN, examples: Vec::new() }).collect();
    let (mut seen, mut first, mut last) = (0u64, f64::MAX, f64::MIN);
    visit_provider(&source, |entry| {
        if boot.is_some_and(|b| b != entry.boot_uuid) {
            return;
        }
        seen += 1;
        first = first.min(entry.time);
        last = last.max(entry.time);
        let message = entry.message();
        for i in any.matches(&message) {
            let hit = &mut hits[i];
            hit.count += 1;
            hit.first = hit.first.min(entry.time);
            hit.last = hit.last.max(entry.time);
            if hit.examples.len() < examples {
                hit.examples.push(text_line(&entry));
            }
        }
    })?;
    match boot {
        Some(b) => println!("{}: boot {}, {seen} entries", source.what, crate::uuid_text(b)),
        None => println!("{}: all boots, {seen} entries", source.what),
    }
    if seen > 0 {
        println!("from {} to {}", time_text(first), time_text(last));
    }
    for ((name, what, _), hit) in CHECKS.iter().zip(&hits) {
        if hit.count == 0 {
            println!("{name:<18} {:>8}  {what}", 0);
            continue;
        }
        println!("{name:<18} {:>8}  {what}; first {}, last {}", hit.count, time_text(hit.first), time_text(hit.last));
        for line in &hit.examples {
            println!("    {}", line.trim_end().replace('\n', "\n    "));
        }
    }
    Ok(())
}
