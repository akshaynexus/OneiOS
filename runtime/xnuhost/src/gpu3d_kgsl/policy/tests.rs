// SPDX-License-Identifier: MIT
//! The GPU's per-process policy on any host, no KGSL (the port of tests/test_gpu_policy.c): the classes from names,
//! the guest's hint and whether the render server reads a context's surfaces, their KGSL priorities, the surface
//! table, and the level for frame streams of the kinds the phone draws (docs/gpu.md 2.2b). Then the same streams under
//! the rule before 2026-11-15 (gpu-level=hold: every render-server frame at the top, to 150 ms after it) and under the
//! deadline rule, side by side: the share of time the clock is held at the top and the frames over budget. Last, a
//! model GPU with a widget-like client beside other work, under #42's rules and now: how long each client's fences take
//! (fix-gpu-widgets).
use super::*;

fn cls(name: &str, hint: u8, qos: u8) -> Class {
    class_of(name.as_bytes(), hint, qos, false, None, None)
}

fn shown(name: &str, hint: u8, qos: u8) -> Class {
    class_of(name.as_bytes(), hint, qos, true, None, None)
}

const V: u8 = HINT_VALID;

#[test]
fn classes() {
    // The render server by name, whatever its role says.
    assert_eq!(cls("xnu:backboardd", 0, 0), Class::Render);
    assert_eq!(cls("xnu:backboardd", V | ROLE_BACKGROUND, QOS_BACKGROUND), Class::Render);
    assert_eq!(cls("backboardd", 0, 0), Class::Render);
    // The name list holds only daemons that never draw what the user sees (p_name: at most 32 characters).
    for n in ["photoanalysisd", "mediaanalysisd", "assetsd", "searchd", "analyticsd", "suggestd"] {
        assert_eq!(cls(&format!("xnu:{n}"), 0, 0), Class::Background, "{n}");
    }
    assert_eq!(cls("xnu:photoanalysisd", V | ROLE_FOREGROUND, QOS_USER_INTERACTIVE), Class::Background);
    assert_eq!(cls("xnu:assetsdx", 0, 0), Class::Default); // a name is the whole name
    assert_eq!(cls("xnu:SpringBoard", 0, 0), Class::Front);
    // What the user sees is no longer on it (until fix-gpu-widgets: WidgetRenderer*, *Widget*, *Poster*,
    // iconservices*, *Thumbnail*): such a process is classed by the guest's role, like any other.
    assert_eq!(cls("xnu:WidgetRenderer_Default", 0, 0), Class::Default);
    assert_eq!(cls("xnu:PosterBoard", V | ROLE_FOREGROUND, QOS_USER_INTERACTIVE), Class::Front);
    assert_eq!(cls("xnu:MercuryPosterExtension", 0, 0), Class::Default);
    assert_eq!(cls("xnu:PhotosReliveWidget", 0, 0), Class::Default);
    assert_eq!(cls("xnu:iconservicesagent", 0, 0), Class::Default);
    assert_eq!(cls("xnu:QuickLookThumbnailExtension", 0, 0), Class::Default);
    assert_eq!(cls("xnu:chronod", V | ROLE_DEFAULT, QOS_USER_INITIATED), Class::Default);
    // A widget renderer that RunningBoard calls non-UI, or whose thread runs at utility QoS, is background by its role...
    assert_eq!(cls("xnu:WidgetRenderer_Default", V | ROLE_NONUI, QOS_USER_INITIATED), Class::Background);
    assert_eq!(cls("xnu:WidgetRenderer_Default", V | ROLE_UNSPECIFIED, QOS_UTILITY), Class::Background);
    // ... until the render server reads one of its surfaces: then never background, whatever says so.
    assert_eq!(shown("xnu:WidgetRenderer_Default", V | ROLE_NONUI, QOS_USER_INITIATED), Class::Default);
    assert_eq!(shown("xnu:WidgetRenderer_Default", V | ROLE_UNSPECIFIED, QOS_UTILITY), Class::Default);
    assert_eq!(shown("xnu:Preferences", V | ROLE_FOREGROUND | HINT_DARWINBG, 0), Class::Default);
    assert_eq!(shown("xnu:photoanalysisd", 0, 0), Class::Default);
    assert_eq!(shown("xnu:MobileSafari", V | ROLE_FOREGROUND, QOS_USER_INTERACTIVE), Class::Front);
    assert_eq!(shown("xnu:backboardd", 0, 0), Class::Render);
    // No name rule: the guest's role, then the thread's QoS.
    assert_eq!(cls("xnu:MobileSafari", 0, 0), Class::Default); // an older guest
    assert_eq!(cls("xnu:MobileSafari", V | ROLE_FOREGROUND, QOS_USER_INTERACTIVE), Class::Front);
    assert_eq!(cls("xnu:MobileSafari", V | ROLE_BACKGROUND, QOS_USER_INTERACTIVE), Class::Background);
    assert_eq!(cls("xnu:Preferences", V | ROLE_FOREGROUND | HINT_DARWINBG, 0), Class::Background);
    assert_eq!(cls("xnu:mediaserverd", V | ROLE_UNSPECIFIED, QOS_UTILITY), Class::Background);
    assert_eq!(cls("xnu:mediaserverd", V | ROLE_UNSPECIFIED, QOS_USER_INITIATED), Class::Default);
    assert_eq!(cls("xnu:mediaserverd", V | ROLE_DEFAULT, QOS_UNSPECIFIED), Class::Default);
    assert_eq!(cls("xnu:Extension", V | ROLE_NONUI, QOS_USER_INITIATED), Class::Background);
    assert_eq!(cls("xnu:x", V | ROLE_GRAPHICS_SERVER, 0), Class::Front);
    // Lists of one's own replace the defaults (the C passed a length shorter than the name: the same bytes here).
    let own =
        |name: &str, shown: bool, bg: &str, front: &str| class_of(name.as_bytes(), 0, 0, shown, Some(bg), Some(front));
    assert_eq!(own("xnu:Foo", false, "Foo,Bar*", ""), Class::Background);
    assert_eq!(own("xnu:Barn", false, "Foo,Bar*", ""), Class::Background);
    assert_eq!(own("xnu:Barn", true, "Foo,Bar*", ""), Class::Default);
    assert_eq!(own("xnu:photoanalysisd", false, "Foo", ""), Class::Default);
    assert_eq!(own("xnu:SpringBoard", false, "", ""), Class::Default);
    assert!(
        name_listed(b"abc", "*b*")
            && name_listed(b"abc", "*c")
            && !name_listed(b"abc", "*d*")
            && !name_listed(b"ab", "abc*")
    );
    assert!(!name_listed(b"abc", "") && !name_listed(b"abc", ",,") && name_listed(b"abc", ",abc,"));
    // Priorities: the render server and the front app above the default; background with the default, never under it
    // (KGSL runs a lower ringbuffer only while every higher one is empty); never KGSL's first ringbuffer.
    assert!(kgsl_priority(Class::Render) == 4 && kgsl_priority(Class::Front) == 4);
    assert!(kgsl_priority(Class::Default) == 8 && kgsl_priority(Class::Background) == 8);
    for c in [Class::Default, Class::Render, Class::Front, Class::Background] {
        assert!((4..=11).contains(&kgsl_priority(c)));
        assert_eq!(kgsl_priority_for(c, false), 8);
    }
}

/// The patched renderer's C uses oneios_gpu_policy.h's og_class_name and og_kgsl_priority itself (patch 0008): they
/// say what the Rust policy says.
#[test]
fn the_renderers_header_agrees() {
    if !drm2kgsl_sys::AVAILABLE {
        println!("the renderer's header: not checked (no drm2kgsl in this build)");
        return;
    }
    for c in [Class::Default, Class::Render, Class::Front, Class::Background] {
        assert_eq!(drm2kgsl_sys::policy_priority(c as u32), kgsl_priority(c));
        assert_eq!(drm2kgsl_sys::policy_class_name(c as u32), c.name());
    }
}

/// Whose surfaces another process reads: blobs over the same first guest page, in either order; one context's own
/// blobs are not another's; the table keeps the newest entries when it fills.
#[test]
fn pages() {
    let mut t = Pages::default();
    assert!(t.note(0x8_8000_4000, 10, 3).is_none()); // the widget draws into a surface
    assert!(t.note(0x8_8000_4000, 11, 3).is_none()); // its own second blob over it
    let o = t.note(0x8_8000_4000, 20, 1).unwrap(); // the render server imports it
    assert!(o.res == 11 && o.ctx == 3);
    assert!(t.note(0x9_0000_0000, 21, 1).is_none()); // the render server's own first
    let o = t.note(0x9_0000_0000, 30, 5).unwrap(); // then a client over the same pages
    assert!(o.res == 21 && o.ctx == 1);
    assert!(t.note(0, 40, 5).is_none() && t.note(0x1000, 0, 5).is_none());
    // Three times the table's size of blobs: the newest 512 are all still known.
    for i in 0..3 * PAGES as u64 {
        t.note(0x10_0000_0000 + i * 0x4000, 1000 + i as u32, 7);
    }
    let found = (3 * PAGES as u64 - 512..3 * PAGES as u64)
        .filter(|&i| t.note(0x10_0000_0000 + i * 0x4000, 90000 + i as u32, 8).is_some_and(|o| o.res == 1000 + i as u32))
        .count();
    assert_eq!(found, 512);
}

const MS: u64 = 1_000_000;
const US: u64 = 1000;

/// One frame: start, then done after `gpu_us`. Returns the action.
fn frame(p: &mut Policy, c: Class, at: u64, deadline_us: u32, gpu_us: u32) -> Action {
    let (a, f) = p.frame_start(c, at, deadline_us);
    p.frame_done(&f, gpu_us);
    a
}

#[test]
fn levels() {
    // A first frame (nothing known): the top. Easy frames at 120 Hz (0.3 ms at the top, 1.2 at the bottom): after 8 the
    // governor.
    let mut p = Policy::new(Mode::Deadline);
    assert_eq!(frame(&mut p, Class::Render, 1000 * MS, 0, 300), Action::Top);
    let mut t = 1000 * MS;
    let mut tops = 1;
    for _ in 0..7 {
        t += 8333 * US;
        tops += (frame(&mut p, Class::Render, t, 0, 300) == Action::Top) as u32;
    }
    assert!(tops == 8 && p.level == Level::Governor);
    t += 8333 * US;
    assert_eq!(frame(&mut p, Class::Render, t, 0, 1200), Action::Governor); // the hold stops at once
    t += 8333 * US;
    assert!(frame(&mut p, Class::Render, t, 0, 1200) == Action::None && p.level == Level::Governor);
    // A frame that nearly misses at the governor's clock (7 ms of 8.3, more than 80 %): the next ones at the top, and
    // longer.
    t += 8333 * US;
    assert!(frame(&mut p, Class::Render, t, 0, 7000) == Action::None && p.level == Level::Top && p.misses == 1);
    t += 8333 * US;
    assert_eq!(frame(&mut p, Class::Render, t, 0, 300), Action::Top);
    assert_eq!(p.down_need, 32);
    for _ in 0..31 {
        t += 8333 * US;
        frame(&mut p, Class::Render, t, 0, 300);
    }
    assert_eq!(p.level, Level::Governor);

    // Heavy frames at 120 Hz (3 ms at the top: 12 at the governor's here, over 6.7): the top, for as long as they come.
    // The way down guesses the governor at half the top's clock until it has measured it (6 ms: fits), so after 8
    // frames one frame teaches it (12 ms: late, the level up at once, the ratio x4 learned); then the top for good.
    let mut p = Policy::new(Mode::Deadline);
    t = 5000 * MS;
    for _ in 0..200 {
        t += 8333 * US;
        let (_, f) = p.frame_start(Class::Render, t, 0);
        p.frame_done(&f, if f.level == Level::Top { 3000 } else { 12000 });
    }
    assert!(p.level == Level::Top && p.frames_top == 199 && p.late == 1 && p.gov_x16 == 64, "{p:?}");
    // Frames the guess already rules out (3.5 ms at the top: 7 at half its clock, over 6.7) never leave the top.
    let mut p = Policy::new(Mode::Deadline);
    for _ in 0..200 {
        t += 8333 * US;
        frame(&mut p, Class::Render, t, 0, 3500);
    }
    assert!(p.level == Level::Top && p.frames_top == 200 && p.late == 0);
    // The same frames at 60 Hz fit at the bottom (12 ms of 16.7): the governor's after 8.
    let mut p = Policy::new(Mode::Deadline);
    for _ in 0..100 {
        t += 16667 * US;
        let gpu = if p.level == Level::Top { 3000 } else { 12000 };
        frame(&mut p, Class::Render, t, 0, gpu);
    }
    assert!(
        p.level == Level::Governor
            && p.frames_top < 12
            && p.misses == 0
            && p.interval_us > 16000
            && p.interval_us < 17000
    );

    // After a pause the governor is at its bottom: a frame known to be easy there stays with it (a clock tick), a heavy
    // one not.
    let mut p = Policy::new(Mode::Deadline);
    p.cold_us = 1000;
    t = 9000 * MS;
    assert!(frame(&mut p, Class::Render, t, 0, 1000) == Action::None && p.level == Level::Governor);
    t += 1000 * MS;
    assert_eq!(frame(&mut p, Class::Render, t, 0, 1000), Action::None);
    p.cold_us = 20000;
    t += 1000 * MS;
    assert_eq!(frame(&mut p, Class::Render, t, 0, 3000), Action::Top);
    // Unknown at the bottom but known at the top: x4 (KGSL's clock ratio) decides.
    let mut p = Policy::new(Mode::Deadline);
    p.top_us = 400; // 1.6 ms at the bottom: fits
    assert_eq!(frame(&mut p, Class::Render, 20000 * MS, 0, 1600), Action::None);
    let mut p = Policy::new(Mode::Deadline);
    p.top_us = 2500; // 10 ms at the bottom: misses
    assert_eq!(frame(&mut p, Class::Render, 20000 * MS, 0, 2500), Action::Top);
    let mut p = Policy::new(Mode::Deadline);
    p.ratio_x16 = 16 * 903 / 231; // KGSL's own list (903 and 231 MHz)
    p.top_us = 2000;
    assert!(p.ratio_x16 == 62 && p.at_bottom(2000) == 7750);
    assert_eq!(frame(&mut p, Class::Render, 20000 * MS, 0, 2000), Action::Top);

    // The guest's deadline (the work interval's, from the CPU agent's signal) is the budget when it comes.
    let mut p = Policy::new(Mode::Deadline);
    p.cold_us = 12000;
    assert_eq!(frame(&mut p, Class::Render, 30000 * MS, 30000, 12000), Action::None); // 12 ms fits 30
    assert_eq!(frame(&mut p, Class::Render, 31000 * MS, 8000, 12000), Action::Top); // not 8
    let (_, f) = p.frame_start(Class::Render, 32000 * MS, 200);
    assert_eq!(f.budget_us, 1000); // at least 1 ms
    let (_, f) = p.frame_start(Class::Render, 32000 * MS + 10 * MS, 0);
    assert_eq!(f.budget_us, 10000); // the cadence: 10 ms apart

    // Background contexts never count and never take the top; the lowest level for them once the screen is still
    // 500 ms; the first render-server frame after that gets the top (KGSL keeps the lowest until it expires otherwise).
    let mut p = Policy::new(Mode::Deadline);
    p.cold_us = 500;
    t = 40000 * MS;
    assert_eq!(frame(&mut p, Class::Render, t, 0, 500), Action::None);
    assert!(!p.background_min(t + 100 * MS));
    assert!(frame(&mut p, Class::Background, t + 100 * MS, 0, 50000) == Action::None && p.frames == 1 && p.misses == 0);
    assert!(p.background_min(t + 600 * MS));
    assert_eq!(frame(&mut p, Class::Render, t + 700 * MS, 0, 500), Action::Top);
    assert_eq!(frame(&mut p, Class::Render, t + 1500 * MS, 0, 500), Action::Governor); // no background at the lowest since
                                                                                       // Other contexts count within a second of the render server's last frame, and only then.
    let mut p = Policy::new(Mode::Deadline);
    assert!(frame(&mut p, Class::Default, 50000 * MS, 0, 30000) == Action::None && p.frames == 0);
    frame(&mut p, Class::Render, 50001 * MS, 0, 300);
    assert!(frame(&mut p, Class::Front, 50300 * MS, 0, 300) == Action::Governor && p.frames == 2); // easy: the hold goes
    assert!(frame(&mut p, Class::Front, 52000 * MS, 0, 300) == Action::None && p.frames == 2);
    // The other modes.
    let mut p = Policy::new(Mode::Hold);
    for i in 0..20 {
        assert_eq!(frame(&mut p, Class::Render, 60000 * MS + i * 8333 * US, 0, 100), Action::Top);
    }
    assert_eq!(frame(&mut p, Class::Background, 61000 * MS, 0, 100), Action::None);
    let mut p = Policy::new(Mode::Governor);
    for i in 0..20 {
        assert_eq!(frame(&mut p, Class::Render, 60000 * MS + i * 8333 * US, 0, 9000), Action::None);
    }
}

/// The lowest level's rules (lowest_for) and the top that ends a lowest level when other work comes (frame_start).
#[test]
fn lowest() {
    let mut p = Policy::new(Mode::Deadline);
    let t = 70000 * MS;
    frame(&mut p, Class::Render, t, 0, 500);
    assert!(!p.lowest_for(t + 100 * MS, 0, false, 0) && p.lowest_refused == 0); // the screen is not still
    assert!(!p.lowest_for(t + 600 * MS, 1, false, 0) && p.lowest_refused == 1); // another context's work is queued
    assert!(!p.lowest_for(t + 600 * MS, 0, true, 0) && p.lowest_refused == 2); // another process reads its surfaces
    assert!(!p.lowest_for(t + 600 * MS, 0, false, t + 540 * MS) && p.lowest_refused == 3); // its own fence waited 60 ms
    assert_eq!(p.last_bg_min_ns, 0);
    assert!(p.lowest_for(t + 600 * MS, 0, false, t + 590 * MS)); // 10 ms: still young
    assert!(p.lowest_for(t + 650 * MS, 0, false, 0) && p.last_bg_min_ns == t + 650 * MS);
    // A widget's drawing at that still screen (a context that counts no frame: the render server drew 1.7 s ago): the
    // top ends the lowest, once; then nothing until the next lowest.
    assert!(p.lowest_for(t + 1700 * MS, 0, false, 0));
    assert!(
        frame(&mut p, Class::Default, t + 1750 * MS, 0, 1500) == Action::Top && p.lowest_undone == 1 && p.frames == 1
    );
    assert!(frame(&mut p, Class::Default, t + 1760 * MS, 0, 1500) == Action::None && p.lowest_undone == 1);
    assert_eq!(frame(&mut p, Class::Background, t + 1770 * MS, 0, 1500), Action::None);
    assert!(p.lowest_for(t + 1800 * MS, 0, false, 0));
    // 300 ms on: expired
    assert!(frame(&mut p, Class::Default, t + 2100 * MS, 0, 1500) == Action::None && p.lowest_undone == 1);
    // Not in the other modes.
    assert!(!Policy::new(Mode::Hold).lowest_for(t, 0, false, 0));
    assert!(!Policy::new(Mode::Governor).lowest_for(t, 0, false, 0));
}

// ---- before and after: the same streams under gpu-level=hold (0005/0006) and =deadline ---------------------------
/// A stream: render-server frames at `hz` for `seconds` in bursts of `burst_ms` every `every_ms` (0: one burst), each
/// frame `top_us` of GPU time at the top level and `ratio` times that at the governor's (its bottom, as after a pause).
struct Stream {
    what: &'static str,
    hz: f64,
    seconds: f64,
    burst_ms: f64,
    every_ms: f64,
    top_us: u32,
    ratio: f64,
}

/// The share of time held at the top, the frames late, the frames.
fn run(s: &Stream, mode: Mode) -> (f64, u32, u32) {
    run_kgsl(s, mode, 64)
}

/// [`run`] with KGSL's top over lowest clock (x16) as the phone reads it (`ratio_x16`).
fn run_kgsl(s: &Stream, mode: Mode, ratio_x16: u32) -> (f64, u32, u32) {
    run_kgsl_down(s, mode, ratio_x16, false)
}

/// run_kgsl with gpu-down=bottom (`bottom`, docs/switches.md) or governor.
fn run_kgsl_down(s: &Stream, mode: Mode, ratio_x16: u32, bottom: bool) -> (f64, u32, u32) {
    let mut p = Policy::new(mode);
    p.ratio_x16 = ratio_x16;
    p.down_at_bottom = bottom;
    let (hold_ns, period, end, start0) = (150 * MS, (1e9 / s.hz) as u64, (s.seconds * 1e9) as u64, 100_000 * MS);
    let (mut held, mut hold_until, mut late, mut frames) = (0u64, 0u64, 0, 0);
    let mut at = 0;
    while at < end {
        let skip = s.every_ms > 0.0 && (at % (s.every_ms * 1e6) as u64) as f64 >= s.burst_ms * 1e6;
        if !skip {
            let now = start0 + at;
            let (a, f) = p.frame_start(Class::Render, now, 0);
            if a == Action::Governor && hold_until > now {
                // the hold ends now: what was counted past it was not held
                held -= hold_until - now;
                hold_until = now;
            }
            if a == Action::Top {
                let from = hold_until.max(now);
                held += now + hold_ns - from;
                hold_until = now + hold_ns;
            }
            // The frame runs at the top while the clock is held (this frame's or an earlier one's), else at the
            // governor's bottom.
            let top = hold_until > now;
            let gpu = if top { s.top_us } else { (s.top_us as f64 * s.ratio) as u32 };
            if gpu as u64 > if period < 16667 * US { period / 1000 } else { 16667 } {
                late += 1;
            }
            frames += 1;
            let level = if top { Level::Top } else { Level::Governor };
            p.frame_done(&Frame { level, cold: f.cold, budget_us: f.budget_us }, gpu);
        }
        at += period;
    }
    if hold_until > start0 + end {
        held -= hold_until - (start0 + end);
    }
    (100.0 * held as f64 / end as f64, late, frames)
}

#[test]
fn before_after() {
    let streams = [
        Stream {
            what: "still lock screen redrawing (45 frames/s, 0.4 ms each)",
            hz: 45.0,
            seconds: 10.0,
            burst_ms: 0.0,
            every_ms: 0.0,
            top_us: 400,
            ratio: 4.0,
        },
        Stream {
            what: "clock tick (1 frame/s, 0.3 ms)",
            hz: 1.0,
            seconds: 10.0,
            burst_ms: 1.0,
            every_ms: 1000.0,
            top_us: 300,
            ratio: 4.0,
        },
        Stream {
            what: "typing (a key every 150 ms: 3 frames at 120 Hz, 1 ms)",
            hz: 120.0,
            seconds: 10.0,
            burst_ms: 25.0,
            every_ms: 150.0,
            top_us: 1000,
            ratio: 4.0,
        },
        Stream {
            what: "scrolling (120 frames/s, 3 ms each)",
            hz: 120.0,
            seconds: 10.0,
            burst_ms: 0.0,
            every_ms: 0.0,
            top_us: 3000,
            ratio: 4.0,
        },
        Stream {
            what: "a light animation (60 frames/s, 0.8 ms)",
            hz: 60.0,
            seconds: 10.0,
            burst_ms: 0.0,
            every_ms: 0.0,
            top_us: 800,
            ratio: 4.0,
        },
    ];
    println!("{:<58} {:>22} {:>22}", "stream (10 s)", "hold: top / late", "deadline: top / late");
    for s in &streams {
        let (h0, l0, f0) = run(s, Mode::Hold);
        let (h1, l1, f1) = run(s, Mode::Deadline);
        println!("{:<58} {:>10.1} % {:>4}/{:<5} {:>10.1} % {:>4}/{:<5}", s.what, h0, l0, f0, h1, l1, f1);
        assert!(h1 <= h0 + 0.01, "{}", s.what);
        // Heavy frames keep the top; nothing is later than before by more than the one frame that teaches the policy.
        if s.top_us as f64 * s.ratio > 8333.0 * 0.8 && s.hz >= 100.0 && s.every_ms == 0.0 {
            assert!(h1 > 95.0, "{}", s.what);
        }
        assert!(l1 <= l0 + 1 + f1 / 40, "{}", s.what);
    }
}

// ---- starvation: a widget-like client beside other work, under #42's rules and now --------------------------------
// A GPU in 50 us ticks, with KGSL's rules as this model takes them (assumptions, not measurements of the phone):
//  - ringbuffers by strict priority (KGSL priority / 4); one with work preempts a lower one at the next tick;
//  - within a ringbuffer a job runs to its end, and the contexts take turns, up to 5 jobs each (the dispatcher's burst);
//  - the clock: the top while a PWR_MAX is in force (to 150 ms after the frame that asked: the hold), else the lowest (a
//    job takes 4 times its top time) while a PWR_MIN is (to 80 ms after the background submission that set or renewed
//    it; a PWR_MAX ends it), else the governor's (2 times);
//  - a job's size is its GPU time at the top; each job is one fence.
// Under #42's rules the widget renderer is background by name (KGSL priority 12) and a background context's submission
// carries the lowest level whenever the screen is still (background_min); now its class comes from the policy (the
// render server reads its surfaces: `shown`) and the lowest from lowest_for.
const M_JOBS: usize = 64;
const M_TICK_NS: u64 = 50000;

#[derive(Clone, Copy, Default)]
struct MJob {
    submit_ns: u64,
    left_ns: u64,
    frame: Frame,
}

#[derive(Clone)]
struct MCtx {
    name: &'static str,
    hint: u8,
    qos: u8,
    /// the render server reads its surfaces; #42's name list had it
    shown: bool,
    old_background: bool,
    /// bursts: `jobs` of `size_us`, `apart_us` apart, every `every_ms` at `at_ms`
    every_ms: u64,
    at_ms: u64,
    jobs: u32,
    apart_us: u64,
    size_us: u64,
    /// instead: one job at a time, the next 1 ms after the last is done (a compute loop)
    looping: bool,
    /// instead (1 + a client's index): a burst 2 ms after that client's burst is done
    after: usize,
    // the run's state
    cls: Class,
    ring: usize,
    sent: u32,
    burst_ns: u64,
    next_ns: u64,
    last_done_ns: u64,
    q: [MJob; M_JOBS],
    head: usize,
    n: usize,
    fences: u64,
    sum_ns: u64,
    max_ns: u64,
    over_slice: u64,
    over_bound: u64,
}

impl MCtx {
    fn new(name: &'static str) -> MCtx {
        MCtx {
            name,
            hint: 0,
            qos: 0,
            shown: false,
            old_background: false,
            every_ms: 0,
            at_ms: 0,
            jobs: 0,
            apart_us: 0,
            size_us: 0,
            looping: false,
            after: 0,
            cls: Class::Default,
            ring: 0,
            sent: 0,
            burst_ns: 0,
            next_ns: 0,
            last_done_ns: 0,
            q: [MJob::default(); M_JOBS],
            head: 0,
            n: 0,
            fences: 0,
            sum_ns: 0,
            max_ns: 0,
            over_slice: 0,
            over_bound: 0,
        }
    }
}

struct Clock {
    t_max: u64,
    t_min: u64,
}

fn m_push(p: &mut Policy, all: &mut [MCtx], i: usize, now: u64, old: bool, clock: &mut Clock) {
    if all[i].n == M_JOBS {
        return;
    }
    let (undone, bg_min, told) = (p.lowest_undone, p.last_bg_min_ns, p.told);
    let (mut a, frame) = p.frame_start(all[i].cls, now, 0);
    let c = &mut all[i];
    let at = (c.head + c.n) % M_JOBS;
    c.n += 1;
    c.q[at] = MJob { submit_ns: now, left_ns: c.size_us * 1000, frame };
    if old && p.lowest_undone != undone {
        (p.lowest_undone, p.last_bg_min_ns, p.told) = (undone, bg_min, told);
        a = Action::None;
    }
    if a == Action::Top {
        clock.t_max = now + 150 * MS;
        clock.t_min = 0;
    }
    if a == Action::Governor && clock.t_max > now {
        clock.t_max = now;
    }
    if all[i].cls != Class::Background {
        return;
    }
    let others: usize = all.iter().filter(|c| c.cls != Class::Background).map(|c| c.n).sum();
    let c = &all[i];
    let oldest = (0..c.n - 1).map(|k| c.q[(c.head + k) % M_JOBS].submit_ns).min().unwrap_or(0);
    let low = if old { p.background_min(now) } else { p.lowest_for(now, others as u32, c.shown, oldest) };
    if low && clock.t_max <= now {
        clock.t_min = now + 80 * MS; // KGSL: a slower constraint never replaces a faster one
    }
}

fn m_run(m: &mut [MCtx], old: bool, seconds: f64) -> u64 {
    let mut p = Policy::new(Mode::Deadline);
    let (start, end) = (200_000 * MS, 200_000 * MS + (seconds * 1e9) as u64);
    let mut clock = Clock { t_max: 0, t_min: 0 };
    let mut cur: [Option<usize>; 4] = [None; 4];
    let (mut burst, mut busy) = ([0u32; 4], [false; 4]);
    let mut lowest_ns = 0;
    for c in m.iter_mut() {
        c.cls = if old && c.old_background {
            Class::Background
        } else {
            class_of(c.name.as_bytes(), c.hint, c.qos, !old && c.shown, None, None)
        };
        let prio = if old {
            match c.cls {
                Class::Render | Class::Front => 4,
                Class::Background => 12,
                Class::Default => 8,
            }
        } else {
            kgsl_priority(c.cls)
        };
        c.ring = prio as usize / 4;
        (c.sent, c.head, c.n) = (0, 0, 0);
        if c.after != 0 {
            c.sent = c.jobs; // armed by its client
        }
        c.burst_ns = start + c.at_ms * MS;
        c.next_ns = c.burst_ns;
        c.last_done_ns = 0;
        (c.fences, c.sum_ns, c.max_ns, c.over_slice, c.over_bound) = (0, 0, 0, 0, 0);
    }
    let mut now = start;
    while now < end {
        // The clients' submissions.
        for i in 0..m.len() {
            if m[i].looping {
                if m[i].n == 0 && now >= m[i].next_ns {
                    m_push(&mut p, m, i, now, old, &mut clock);
                }
                continue;
            }
            if now >= m[i].burst_ns + m[i].sent as u64 * m[i].apart_us * 1000 && m[i].sent < m[i].jobs {
                m_push(&mut p, m, i, now, old, &mut clock);
                m[i].sent += 1;
            }
            if m[i].after != 0 {
                continue;
            }
            let c = &mut m[i];
            if c.sent == c.jobs && now >= c.burst_ns + c.every_ms * MS - M_TICK_NS {
                c.burst_ns += c.every_ms * MS;
                c.sent = 0;
            }
        }
        // The GPU: the highest ringbuffer with work; in it, the job begun, else the next context in turn.
        let speed4 = if clock.t_max > now {
            4
        } else if clock.t_min > now {
            1
        } else {
            2
        };
        if speed4 == 1 && now >= start + 1000 * MS {
            lowest_ns += M_TICK_NS;
        }
        let ring = (0..4).find(|&r| m.iter().any(|c| c.ring == r && c.n != 0));
        let Some(ring) = ring else {
            now += M_TICK_NS;
            continue;
        };
        if !busy[ring] && (cur[ring].is_none_or(|c| m[c].n == 0) || burst[ring] >= 5) {
            // The next context in turn with work (the same one again when it is the only one: its next burst).
            let from = cur[ring].map_or(0, |c| c + 1);
            if let Some(i) = (0..m.len()).map(|k| (from + k) % m.len()).find(|&i| m[i].ring == ring && m[i].n != 0) {
                cur[ring] = Some(i);
                burst[ring] = 0;
            }
        }
        let ci = cur[ring].unwrap();
        busy[ring] = true;
        let step = M_TICK_NS * speed4 / 4;
        let head = m[ci].head;
        if m[ci].q[head].left_ns > step {
            m[ci].q[head].left_ns -= step;
            now += M_TICK_NS;
            continue;
        }
        let j = m[ci].q[head];
        let done = now + M_TICK_NS;
        let took = done - j.submit_ns;
        let c = &mut m[ci];
        if j.submit_ns >= start + 1000 * MS {
            // the first second teaches the policy: not counted
            c.fences += 1;
            c.sum_ns += took;
            c.max_ns = c.max_ns.max(took);
            if took >= FENCE_SLOW_US * 1000 {
                c.over_slice += 1;
            }
            if took >= p.bg_bound_us as u64 * 1000 {
                c.over_bound += 1;
            }
        }
        let from = j.submit_ns.max(c.last_done_ns);
        p.frame_done(&j.frame, ((done - from) / 1000) as u32);
        c.last_done_ns = done;
        c.head = (c.head + 1) % M_JOBS;
        c.n -= 1;
        busy[ring] = false;
        burst[ring] += 1;
        if c.looping {
            c.next_ns = done + MS;
        }
        if !c.looping && c.n == 0 && c.sent == c.jobs {
            for o in m.iter_mut().filter(|o| o.after == ci + 1) {
                o.burst_ns = done + 2 * MS;
                o.sent = 0;
            }
        }
        now += M_TICK_NS;
    }
    lowest_ns
}

#[test]
fn starvation() {
    // A home screen with widgets, still otherwise, for 10 s after a first second that is not counted: once a second the
    // widget renderer redraws its 6 surfaces (one fence each), and 2 ms after the last is done the render server
    // composites 3 frames of it (60 Hz); photoanalysisd computes all the time. The widget renderer is a non-UI process
    // at utility QoS to the guest (the worst its hint can say), and the render server reads its surfaces. Three cases:
    // small widgets (1 ms of GPU a surface), heavy ones (5 ms), and heavy ones while SpringBoard also draws a frame at
    // each refresh.
    let cases: [(u64, usize, &str); 3] =
        [(1000, 3, "small widgets"), (5000, 3, "heavy widgets"), (5000, 4, "heavy widgets, SpringBoard draws too")];
    for (size_us, clients, what) in cases {
        let mut runs: Vec<Vec<MCtx>> = Vec::new();
        let mut low = [0u64; 2];
        for old in [true, false] {
            let mut render = MCtx::new("xnu:backboardd");
            (render.after, render.jobs, render.apart_us, render.size_us) = (2, 3, 16667, 1500);
            let mut widget = MCtx::new("xnu:WidgetRenderer_Default");
            (widget.hint, widget.qos, widget.shown, widget.old_background) = (V | ROLE_NONUI, QOS_UTILITY, true, true);
            (widget.every_ms, widget.at_ms, widget.jobs, widget.apart_us, widget.size_us) = (1000, 0, 6, 500, size_us);
            let mut photos = MCtx::new("xnu:photoanalysisd");
            (photos.looping, photos.size_us) = (true, 8000);
            let mut board = MCtx::new("xnu:SpringBoard");
            (board.every_ms, board.at_ms, board.jobs, board.apart_us, board.size_us) = (1000, 5, 1, 0, 2000);
            let mut m: Vec<MCtx> = vec![render, widget, photos, board];
            m.truncate(clients);
            low[old as usize] = m_run(&mut m, old, 11.0);
            runs.push(m);
        }
        let (old_run, now_run) = (&runs[0], &runs[1]);
        println!(
            "\n{:<40} {:<4} {:>7} {:>9} {:>9} {:>10} {:>10}",
            what, "", "fences", "mean ms", "max ms", ">= 50 ms", ">= 100 ms"
        );
        for i in 0..clients {
            for (tag, c) in [("#42", &old_run[i]), ("now", &now_run[i])] {
                let mean = if c.fences != 0 { c.sum_ns as f64 / c.fences as f64 / 1e6 } else { 0.0 };
                println!(
                    "  {:<38} {:<4} {:>7} {:>9.1} {:>9.1} {:>10} {:>10}",
                    &c.name[4..],
                    tag,
                    c.fences,
                    mean,
                    c.max_ns as f64 / 1e6,
                    c.over_bound,
                    c.over_slice
                );
            }
        }
        println!(
            "  {:<38} #42 {:.1} %, now {:.1} %",
            "time at KGSL's lowest level",
            100.0 * low[1] as f64 / 1e10,
            100.0 * low[0] as f64 / 1e10
        );
        // Now: no client's fence reaches one of Metal's 100 ms wait slices, the widget's stay inside bg_bound_us
        // (50 ms), and are faster than under #42's rules.
        for c in now_run.iter() {
            assert!(c.over_slice == 0 && c.fences > 0, "{what}: {}", c.name);
        }
        assert!(now_run[1].fences == 60 && now_run[1].over_bound == 0 && now_run[1].max_ns < 50 * MS, "{what}");
        assert!(now_run[1].max_ns < old_run[1].max_ns && now_run[1].sum_ns < old_run[1].sum_ns, "{what}");
    }
}

/// The S24 Ultra's KGSL lists 80-1000 MHz (x12.50). With the way down judged at that bottom, a frame over 0.53 ms at the
/// top never "fitted", so the level never came down: the session of 2026-11-23 20:23 logged `frames 548, at the top
/// 548; up +0 down +0` while its frames took 2.3 ms at the top and 1.8 ms at the governor's. Now the way down uses the
/// governor's clock in a burst (half the top's until measured, then what its frames took), and a frame that nearly
/// misses there sends the level up again for longer.
#[test]
fn down_with_a_wide_clock_range() {
    let wide = 16 * 1000 / 80; // 200: x12.50
                               // The phone's case: 2.3 ms frames at 120 Hz, the governor's clock in a burst costing them 1.3 times (its own
                               // choice for 28 % busy, not the 80 MHz bottom).
    let s = Stream {
        what: "steady redraw, 2.3 ms frames, governor x1.3",
        hz: 120.0,
        seconds: 10.0,
        burst_ms: 0.0,
        every_ms: 0.0,
        top_us: 2300,
        ratio: 1.3,
    };
    let (h_hold, l_hold, _) = run_kgsl(&s, Mode::Hold, wide);
    let (h, l, f) = run_kgsl(&s, Mode::Deadline, wide);
    println!("{}: hold {h_hold:.1} % ({l_hold} late), deadline {h:.1} % ({l} late of {f})", s.what);
    assert!(h < 5.0, "the level comes down and stays down: {h:.1} % at the top");
    assert!(l <= l_hold, "no frame later than at the top");
    // gpu-down=bottom (docs/switches.md): the rule before 3b36ff0, judged at KGSL's bottom: the level never comes down.
    let (h_old, _, _) = run_kgsl_down(&s, Mode::Deadline, wide, true);
    assert!(h_old > 95.0, "gpu-down=bottom keeps the old rule: {h_old:.1} % at the top");
    // A frame the governor's clock really cannot carry (it is at x12.5, as after a pause): the level goes back up at
    // the first near miss, learns the ratio and stays at the top; a frame late, not more.
    let s = Stream { what: "heavy 2.3 ms frames, governor at its bottom", ratio: 12.5, ..s };
    let (h, l, f) = run_kgsl(&s, Mode::Deadline, wide);
    println!("{}: deadline {h:.1} % ({l} late of {f})", s.what);
    assert!(h > 95.0, "{h:.1} % at the top");
    assert!(l <= 1, "{l} late");
    // The ratio it learns is the governor's, bounded by KGSL's.
    let mut p = Policy::new(Mode::Deadline);
    p.ratio_x16 = wide;
    p.top_us = 2000;
    p.frame_done(&Frame { level: Level::Governor, cold: false, budget_us: 8333 }, 3000);
    assert_eq!(p.gov_x16, 24); // x1.5
    assert_eq!(p.at_governor(2000), 3000);
    p.frame_done(&Frame { level: Level::Governor, cold: false, budget_us: 8333 }, 900_000);
    assert!(p.gov_x16 <= wide);
    // Unmeasured: half the top's clock, never more than KGSL's own ratio.
    let mut p = Policy::new(Mode::Deadline);
    p.ratio_x16 = wide;
    assert_eq!(p.at_governor(2000), 4000);
    p.ratio_x16 = 24;
    assert_eq!(p.at_governor(2000), 3000);
    // A high gov_x16 spike (e.g. 7.87x during boot) decays across fast frames at TOP so the governor is retried.
    let mut p = Policy::new(Mode::Deadline);
    p.ratio_x16 = wide;
    p.gov_x16 = 126;
    p.level = Level::Top;
    for _ in 0..120 {
        p.frame_done(&Frame { level: Level::Top, cold: false, budget_us: 8333 }, 2000);
    }
    assert_eq!(p.gov_x16, DOWN_GUESS_X16);
    assert!(p.downs > 0, "gov_x16 decayed and allowed stepping down from TOP");
}
