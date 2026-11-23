// SPDX-License-Identifier: MIT
//! One frame of the render server end to end, as the host sees it (fix/frame-path, 2026-11-24). A lasting line every
//! 10 s while the guest draws, its stages as p50/p95/max in milliseconds:
//!
//! ```text
//! frame: 10 s: 137 guest frames (13.7/s), 120 of them under 0.5 s apart; ms p50/p95/max: interval 8.4/33.0/120.0;
//!   flush -> the next frame's first GPU submission (the render server's CPU and its waits) 5.0/20.0/60.0; first
//!   submission -> flush 3.0/9.0/20.0, of it GPU (submission -> retired, 119 fences) 2.0/6.0/18.0 and retired -> the
//!   guest's flush 1.0/4.0/10.0; the helper's answer 6.0/12.0/40.0; touch -> next flush 20.0/60.0/90.0 (45 touches);
//!   3 flushes with no GPU work before them
//! ```
//!
//! The events, each from where it happens: a fenced submission of the render server's context and its retirement
//! (gpu3d_kgsl.rs), the guest's flush of its scanout (virtio_gpu.rs: what its display driver sends once the frame's
//! GPU work is done, DroidVMVirtioGpuDisplay.cpp), the helper's answer (display.rs) and each touch frame sent to the
//! guest (input.rs). A frame is the span between two flushes: "flush -> first submission" is what the render server
//! did before its GPU work (CoreAnimation's commit, layout, encoding, and its waits: for vsync, a Mach reply, a CPU), and
//! "first submission -> flush" its GPU work and the guest's way back to the swap. The interval is their sum. Only
//! intervals under BURST_US count in it (a frame after a pause is no frame rate). The helper's own line
//! (`oneios-display: 10 s: ...`, control/display.log) has the rest: its read, its buffer wait, its post.
//!
//! Cost: a lock and a few stores per event (about 2000 a second at most), no allocation, nothing when idle.
use crate::perf::Hist;
use std::sync::{Mutex, MutexGuard};
use xh_core::logi;

/// Two flushes further apart than this are a pause, not a frame interval.
pub const BURST_US: u64 = 500_000;
/// The report's period.
pub const PERIOD_US: u64 = 10_000_000;

#[derive(Default)]
struct State {
    since: u64,
    last_flush: u64,
    /// the render server's first fenced submission since the last flush (0: none)
    first_submit: u64,
    /// its latest retirement
    last_retire: u64,
    /// the first touch frame sent since the last flush (0: none)
    touch: u64,
    flushes: u64,
    burst: u64,
    no_gpu: u64,
    touches: u64,
    interval: Hist,
    before_gpu: Hist,
    to_flush: Hist,
    gpu: Hist,
    retired_to_flush: Hist,
    answer: Hist,
    touch_to_flush: Hist,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn state() -> MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn with(f: impl FnOnce(&mut State)) {
    let mut g = state();
    f(g.get_or_insert_with(State::default));
}

fn add(h: &mut Hist, us: u64) {
    h.add(us.saturating_mul(1000));
}

impl State {
    fn submit(&mut self, now: u64) {
        if self.first_submit == 0 {
            self.first_submit = now;
        }
    }

    fn retired(&mut self, now: u64, took: u64) {
        self.last_retire = now;
        add(&mut self.gpu, took);
    }

    fn touched(&mut self, scan: u64) {
        if self.touch == 0 {
            self.touch = scan.max(1);
        }
    }

    fn flush(&mut self, now: u64) {
        self.flushes += 1;
        if self.since == 0 {
            self.since = now;
        }
        if self.last_flush != 0 && now >= self.last_flush && now - self.last_flush < BURST_US {
            self.burst += 1;
            add(&mut self.interval, now - self.last_flush);
        }
        if self.first_submit != 0 && now >= self.first_submit {
            if self.last_flush != 0
                && self.first_submit >= self.last_flush
                && self.first_submit - self.last_flush < BURST_US
            {
                add(&mut self.before_gpu, self.first_submit - self.last_flush);
            }
            add(&mut self.to_flush, now - self.first_submit);
            if self.last_retire >= self.first_submit && now >= self.last_retire {
                add(&mut self.retired_to_flush, now - self.last_retire);
            }
        } else {
            self.no_gpu += 1;
        }
        if self.touch != 0 && now >= self.touch {
            self.touches += 1;
            add(&mut self.touch_to_flush, now - self.touch);
        }
        self.touch = 0;
        self.first_submit = 0;
        self.last_flush = now;
    }
}

/// A fenced submission of the render server's context (CLOCK_MONOTONIC us).
pub fn render_submit(now: u64) {
    with(|s| s.submit(now));
}

/// A fence of the render server retired, `took` us after its submission.
pub fn render_retired(now: u64, took: u64) {
    with(|s| s.retired(now, took));
}

/// A touch frame went to the guest (its scan time, us).
pub fn touch(scan: u64) {
    with(|s| s.touched(scan));
}

/// The guest flushed its scanout: a frame.
pub fn flush(now: u64) {
    with(|s| s.flush(now));
    report(now);
}

/// The helper answered a frame `took` us after xnuhost handed it over (the guest's flush waited that long).
pub fn answered(took: u64) {
    with(|s| add(&mut s.answer, took));
}

/// p50/p95/max in ms, or "-" with no sample.
fn three(h: &Hist) -> String {
    if h.n == 0 {
        return "-".into();
    }
    let ms = |ns: u64| ns as f64 / 1e6;
    format!("{:.1}/{:.1}/{:.1}", ms(h.quantile(0.5)), ms(h.quantile(0.95)), ms(h.max_ns))
}

/// The 10 s line, when 10 s have passed since the period's first flush (no timer: called at each flush and by the
/// display's reader). The period's figures are reset.
pub fn report(now: u64) {
    let line = state().as_mut().and_then(|s| s.take(now));
    if let Some(line) = line {
        logi!("{}", line);
    }
}

impl State {
    /// The line once the period is over (None before, or with no flush in it), and a new period that keeps what spans
    /// two (the last flush, a submission or touch not flushed yet, the last retirement).
    fn take(&mut self, now: u64) -> Option<String> {
        if self.since == 0 || now < self.since + PERIOD_US {
            return None;
        }
        let line = format_line(self, (now - self.since) as f64 / 1e6);
        let (last_flush, first_submit, last_retire, touch) =
            (self.last_flush, self.first_submit, self.last_retire, self.touch);
        *self = State { last_flush, first_submit, last_retire, touch, ..State::default() };
        line
    }
}

fn format_line(s: &State, secs: f64) -> Option<String> {
    if s.flushes == 0 {
        return None;
    }
    let touches = if s.touches != 0 {
        format!("; touch -> next flush {} ({} touches)", three(&s.touch_to_flush), s.touches)
    } else {
        String::new()
    };
    Some(format!(
        "frame: {secs:.0} s: {} guest frames ({:.1}/s), {} of them under 0.5 s apart; ms p50/p95/max: interval {}; flush \
         -> the next frame's first GPU submission (the render server's CPU and its waits) {}; first submission -> flush \
         {}, of it GPU (submission -> retired, {} fences) {} and retired -> the guest's flush {}; the helper's answer \
         {}{touches}; {} flushes with no GPU work before them",
        s.flushes,
        s.flushes as f64 / secs.max(0.001),
        s.burst,
        three(&s.interval),
        three(&s.before_gpu),
        three(&s.to_flush),
        s.gpu.n,
        three(&s.gpu),
        three(&s.retired_to_flush),
        three(&s.answer),
        s.no_gpu
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stages of a stream of frames: 8 ms apart, the render server's CPU 3 ms, its GPU work 4 ms, the guest's
    /// flush 1 ms after the retirement, a touch 7 ms before a flush; a pause of a second is no interval; a flush with no
    /// GPU work before it counts as such.
    #[test]
    fn stages() {
        let mut s = State::default();
        let mut t = 1_000_000u64;
        s.flush(t);
        for i in 0..20 {
            if i == 5 {
                s.touched(t + 1000);
                s.touched(t + 2000); // the first one counts
            }
            s.submit(t + 3000);
            s.submit(t + 4000); // a second submission of the same frame
            s.retired(t + 7000, 4000);
            t += 8000;
            s.flush(t);
        }
        t += 1_000_000; // a pause
        s.flush(t);
        let line = format_line(&s, 10.0).unwrap();
        assert_eq!(s.burst, 20);
        assert_eq!(s.no_gpu, 2); // the first flush and the one after the pause
        assert!(line.starts_with("frame: 10 s: 22 guest frames (2.2/s), 20 of them under 0.5 s apart"), "{line}");
        assert!(line.contains("interval 8.0/8.0/8.0"), "{line}");
        assert!(line.contains("(the render server's CPU and its waits) 3.0/3.0/3.0"), "{line}");
        assert!(line.contains("first submission -> flush 5.0/5.0/5.0"), "{line}");
        assert!(line.contains("GPU (submission -> retired, 20 fences) 4.0/4.0/4.0"), "{line}");
        assert!(line.contains("retired -> the guest's flush 1.0/1.0/1.0"), "{line}");
        assert!(line.contains("touch -> next flush 7.0/7.0/7.0 (1 touches)"), "{line}");
        assert!(line.contains("the helper's answer -"), "{line}");
    }

    /// A period is reported once 10 s have passed since its first flush, and the next starts empty but for what spans
    /// the two.
    #[test]
    fn period() {
        let mut s = State::default();
        let t0 = 5_000_000u64;
        s.flush(t0);
        s.submit(t0 + 2000);
        s.retired(t0 + 6000, 4000);
        s.flush(t0 + 8000);
        add(&mut s.answer, 5000);
        assert!(s.take(t0 + PERIOD_US - 1).is_none());
        s.submit(t0 + 9000);
        let line = s.take(t0 + PERIOD_US).unwrap();
        assert!(line.contains("the helper's answer 5.0/5.0/5.0"), "{line}");
        assert!(
            s.flushes == 0 && s.gpu.n == 0 && s.since == 0 && s.last_flush == t0 + 8000 && s.first_submit == t0 + 9000
        );
        assert!(s.take(t0 + 3 * PERIOD_US).is_none()); // nothing since: no line
        s.flush(t0 + 3 * PERIOD_US);
        assert!(s.before_gpu.n == 1 && s.interval.n == 0); // submitted 1 ms after a flush; the flush 30 s later: a pause
    }
}
