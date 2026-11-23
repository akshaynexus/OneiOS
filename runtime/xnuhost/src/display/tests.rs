// SPDX-License-Identifier: MIT
//! tests/test_display_mode.c: the real display module against a stand-in helper on the real socket: where the guest's
//! mode comes from; and direct frames, the helper being the display helper's own protocol code (oneios-display's xhd):
//! it says it is direct, gets guest RAM and the table, copies a frame from scattered guest pages and answers; one that
//! never answers is answered for. The display's state is once per process: each case runs in a process of its own
//! (the test binary runs itself again with XH_DISPLAY_CASE naming the case).
use std::os::fd::{AsFd, AsRawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::time::Instant;

use oneios_display::sys as dsys;
use oneios_display::xhd::{self, Direct, Rect, DONE_FAILED};

use super::*;

const CASE: &str = "XH_DISPLAY_CASE";

/// Runs `body` in a child process with `case` set (the test's name `name`); in that child, runs it.
fn in_child(name: &str, case: &str, body: impl FnOnce(&Path)) {
    if let Ok(c) = std::env::var(CASE) {
        if c == case {
            let dir = PathBuf::from(std::env::var("XH_DISPLAY_DIR").unwrap());
            let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&dir))).is_ok();
            use std::io::Write;
            let _ = std::io::stdout().flush();
            // SAFETY: the end of this child process (its threads, the accept thread among them, are not joined).
            unsafe { libc::_exit(if ok { 0 } else { 1 }) };
        }
        return;
    }
    let dir = std::env::temp_dir().join(format!("xnuhost-display-{}-{case}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let me = std::env::current_exe().unwrap();
    let status = std::process::Command::new(me)
        .args([format!("display::tests::{name}").as_str(), "--exact", "--nocapture", "--test-threads=1"])
        .env(CASE, case)
        .env("XH_DISPLAY_DIR", &dir)
        .status()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    println!("{case}: {}", if status.success() { "ok" } else { "FAILED" });
    assert!(status.success(), "{case}: {status}");
}

fn config(dir: &Path, mode: Mode, (w, h, hz): (u32, u32, u32), fallback: Option<&str>) -> Config {
    let mut c = Config::new();
    c.mode = mode;
    c.session = dir.to_string_lossy().into_owned();
    (c.scale, c.width, c.height, c.hz) = (3, w, h, hz);
    if let Some(f) = fallback {
        c.set("panel-fallback", f).unwrap();
    }
    c
}

/// The helper: reports `report` (w, h, hz), then takes the hello: its frame's width, height, stride, format and the
/// memfd's size.
fn helper(path: PathBuf, report: (u32, u32, u32)) -> Option<(u32, u32, u32, u32, i64)> {
    let fd = xhd::connect(&path, 3000, &xhd::no_stop).ok()?;
    xhd::send_panel(fd.as_fd(), report.0, report.1, report.2, 0).ok()?;
    dsys::poll(&[(fd.as_fd(), libc::POLLIN)], 15000, true).ok()?;
    let mut packet = [0u8; Message::BYTES];
    let got = dsys::recv_packet(fd.as_fd(), &mut packet, 0).ok()?;
    let m = Message::parse(&packet[..got.length]).filter(|m| m.is(Kind::Hello))?;
    let (size, _) = dsys::file_size(got.fds.first()?.as_fd()).ok()?;
    Some((m.a, m.b, m.c, m.d, size))
}

/// One case: the frame, the hello and the time as expected.
#[allow(clippy::too_many_arguments)]
fn mode_case(
    name: &str,
    case: &str,
    mode: Mode,
    pinned: (u32, u32, u32),
    report: (u32, u32, u32),
    fallback: Option<&str>,
    want: (u32, u32, u32),
    most_s: f64,
) {
    in_child(name, case, |dir| {
        let c = config(dir, mode, pinned, fallback);
        let path = dir.join("display.sock");
        let helper = std::thread::spawn(move || helper(path, report));
        let t0 = Instant::now();
        let got = init(&c).expect("display init");
        assert_eq!(got, want, "guest mode");
        assert!(t0.elapsed().as_secs_f64() <= most_s, "took {:?}", t0.elapsed());
        let (w, h, stride, format, size) = helper.join().unwrap().expect("the helper's hello");
        assert!(w == want.0 && h == want.1 && stride == want.0 * 4 && format == 1);
        // what the guest's scanout has to fit: 17.97 MB at 1440x3120
        assert_eq!(size, (want.0 * 4 * want.1) as i64);
        let d = get().unwrap();
        assert!(d.width == want.0 && d.height == want.1 && d.stride == want.0 * 4);
    });
}

#[test]
fn a_session_takes_the_helpers_report() {
    let name = "a_session_takes_the_helpers_report";
    mode_case(name, "report", Mode::Takeover, (0, 0, 0), (1440, 3120, 120), None, (1440, 3120, 120), 5.0);
}

#[test]
fn another_panels_report() {
    // not the default: only the report can give this
    let name = "another_panels_report";
    mode_case(name, "another", Mode::Takeover, (0, 0, 0), (1260, 2800, 90), None, (1260, 2800, 90), 5.0);
    // the base S25's PA1 panel
    mode_case(name, "s25", Mode::Takeover, (0, 0, 0), (1080, 2340, 120), None, (1080, 2340, 120), 5.0);
}

#[test]
fn a_configured_mode_wins() {
    let name = "a_configured_mode_wins";
    mode_case(name, "configured", Mode::Takeover, (1080, 2340, 60), (1440, 3120, 120), None, (1080, 2340, 60), 1.0);
}

#[test]
fn a_bench_run_assumes() {
    let name = "a_bench_run_assumes";
    // the S24 Ultra's, without waiting
    mode_case(name, "bench", Mode::Bench, (0, 0, 0), (1440, 3120, 120), None, (1440, 3120, 120), 1.0);
    // panel-fallback= is what a bench run assumes (the Galaxy S25's and A56's panel), without waiting
    let f = Some("1080x2340@120");
    mode_case(name, "fallback", Mode::Bench, (0, 0, 0), (1440, 3120, 120), f, (1080, 2340, 120), 1.0);
    // a helper's report still wins over panel-fallback=
    mode_case(name, "report-wins", Mode::Takeover, (0, 0, 0), (1440, 3120, 120), f, (1440, 3120, 120), 5.0);
}

// ---- direct frames -----------------------------------------------------------------------------------------------------
const RAM_PA: u64 = 0x8_0000_0000;
const RAM_BYTES: usize = 0x40000;
const DW: u32 = 16;
const DH: u32 = 40;
const DSTRIDE: u32 = 80;

/// The display's own call of input.rs's touch-to-frame end is counted by input.rs: LAST_SHOWN changes.
static ANSWERED: AtomicU32 = AtomicU32::new(0);
static ANSWERED_SEQ: AtomicU64 = AtomicU64::new(0);

/// The helper: direct, copies the frame from guest RAM, compares it with the guest's pages 3, 1, 2 (4 KiB each, in that
/// order), answers (answer 1: done, 2: done with DONE_FAILED, 0: never). (copied and right, its seq)
fn direct_helper(path: PathBuf, answer: u32, ram: &'static [u8]) -> Option<(bool, u64)> {
    let fd = xhd::connect(&path, 3000, &xhd::no_stop).ok()?;
    xhd::send_panel(fd.as_fd(), DW, DH, 120, xhd::PANEL_DIRECT).ok()?;
    let frame = xhd::recv_hello(fd.as_fd(), 3000).ok()?;
    let mut d = Direct::new();
    let mut damage = Rect::EMPTY;
    for _ in 0..300 {
        if d.pending.is_some() || !xhd::drain_direct(fd.as_fd(), frame.width, frame.height, &mut damage, Some(&mut d)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let p = d.pending?;
    let mut dst = vec![0u8; (DW * 4 * DH) as usize];
    let done = d.copy_direct(&mut dst, DW as usize * 4, DW, DH, false, None);
    let mut ok = done.is_ok_and(|r| r.w == DW && r.h == DH);
    for y in 0..DH as u64 {
        for x in 0..DW as u64 * 4 {
            let at = y * DSTRIDE as u64 + x;
            let page = match at / 4096 {
                0 => 3,
                1 => 1,
                _ => 2,
            };
            ok &= dst[(y * DW as u64 * 4 + x) as usize] == ram[(page * 4096 + at % 4096) as usize];
        }
    }
    if answer != 0 {
        let _ = xhd::send_done(fd.as_fd(), p.seq, if answer == 2 { DONE_FAILED } else { 0 });
    }
    std::thread::sleep(std::time::Duration::from_millis(400)); // still connected while xnuhost waits (or gives up)
    Some((ok, p.seq))
}

fn wait_for(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..300 {
        if f() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    f()
}

fn direct_case(case: &str, answer: u32) {
    in_child("direct_frames", case, |dir| {
        // pinned: the report comes later, to the reader
        let c = config(dir, Mode::Takeover, (DW, DH, 120), None);
        let ram_fd = dsys::memfd("test-ram", RAM_BYTES).unwrap();
        let mut map = Map::new(ram_fd.as_fd(), RAM_BYTES, true).unwrap();
        for (i, b) in map.bytes_mut().iter_mut().enumerate() {
            *b = (i * 7 + i / 4096) as u8;
        }
        let map: &'static Map = Box::leak(Box::new(map));
        init(&c).expect("display init");
        let path = dir.join("display.sock");
        let helper = std::thread::spawn(move || direct_helper(path, answer, map.bytes()));
        set_ram(ram_fd.as_raw_fd(), RAM_PA, RAM_BYTES as u64);
        assert!(wait_for(direct), "the helper is direct");
        let segs = [Entry::new(RAM_PA + 0x3000, 0x1000), Entry::new(RAM_PA + 0x1000, 0x2000)]; // pages 3, then 1 and 2
        let outside = [Entry::new(RAM_PA + RAM_BYTES as u64, 0x1000)];
        let done: Done = std::sync::Arc::new(|seq| {
            ANSWERED_SEQ.store(seq, Ordering::SeqCst);
            ANSWERED.fetch_add(1, Ordering::SeqCst);
        });
        assert!(!post(&outside, 0, DSTRIDE, DW, DH, [0, 0, DW, DH], 1, &done)); // not RAM: not posted
        assert!(post(&segs, 0, DSTRIDE, DW, DH, [0, 0, DW, DH], 2, &done));
        assert!(wait_for(|| ANSWERED.load(Ordering::SeqCst) != 0));
        let (ok, seq) = helper.join().unwrap().expect("the helper copied a frame");
        assert!(ANSWERED_SEQ.load(Ordering::SeqCst) == 2 && ok && seq == 2);
        if answer == 2 {
            // it said it could not copy: xnuhost copies again
            assert!(wait_for(|| !direct()), "no longer direct");
        }
    });
}

#[test]
fn direct_frames() {
    // the helper copies from guest RAM and answers; a helper that could not copy one turns them off; a helper that
    // never answers is answered for
    direct_case("answers", 1);
    direct_case("cannot-copy", 2);
    direct_case("never-answers", 0);
}

/// The posted frame (only it) ends a touch's wait for the guest's next frame, as a copied one does (flush).
#[test]
fn a_posted_frame_ends_the_touch_wait() {
    in_child("a_posted_frame_ends_the_touch_wait", "shown", |dir| {
        let c = config(dir, Mode::Takeover, (DW, DH, 120), None);
        let ram_fd = dsys::memfd("test-ram", RAM_BYTES).unwrap();
        let map: &'static Map = Box::leak(Box::new(Map::new(ram_fd.as_fd(), RAM_BYTES, true).unwrap()));
        init(&c).expect("display init");
        let path = dir.join("display.sock");
        let helper = std::thread::spawn(move || direct_helper(path, 1, map.bytes()));
        set_ram(ram_fd.as_raw_fd(), RAM_PA, RAM_BYTES as u64);
        assert!(wait_for(direct));
        let done: Done = std::sync::Arc::new(|_| {});
        let before = crate::input::last_shown_ns();
        assert!(!post(&[Entry::new(RAM_PA + RAM_BYTES as u64, 0x1000)], 0, DSTRIDE, DW, DH, [0, 0, DW, DH], 1, &done));
        assert_eq!(crate::input::last_shown_ns(), before); // not posted: no frame shown
        assert!(post(&[Entry::new(RAM_PA, 0x3000)], 0, DSTRIDE, DW, DH, [0, 0, DW, DH], 2, &done));
        assert_ne!(crate::input::last_shown_ns(), before);
        let _ = helper.join();
    });
}

/// display-post=serial (docs/switches.md): the helper is sent "options" with OPTION_SERIAL_POST right after the hello;
/// at the default (apart) nothing comes between the hello and the first flush.
#[test]
fn the_serial_post_option() {
    for (case, serial) in [("post-apart", false), ("post-serial", true)] {
        in_child("the_serial_post_option", case, |dir| {
            let mut c = config(dir, Mode::Takeover, (0, 0, 0), None);
            if serial {
                c.set("display-post", "serial").unwrap();
            }
            let path = dir.join("display.sock");
            let helper = std::thread::spawn(move || {
                let fd = xhd::connect(&path, 3000, &xhd::no_stop).ok()?;
                xhd::send_panel(fd.as_fd(), 1440, 3120, 120, 0).ok()?;
                let mut kinds = Vec::new();
                while kinds.len() < 3 && !kinds.contains(&Kind::Flush) {
                    let (n, _) = dsys::poll(&[(fd.as_fd(), libc::POLLIN)], 15000, true).ok()?;
                    if n == 0 {
                        break;
                    }
                    let mut packet = [0u8; Message::BYTES];
                    let got = dsys::recv_packet(fd.as_fd(), &mut packet, 0).ok()?;
                    let m = Message::parse(&packet[..got.length])?;
                    if m.is(Kind::Options) {
                        assert_eq!(m.a, OPTION_SERIAL_POST);
                    }
                    kinds.push(m.kind()?);
                }
                Some(kinds)
            });
            init(&c).expect("display init");
            let kinds = helper.join().unwrap().expect("the helper's messages");
            let want =
                if serial { vec![Kind::Hello, Kind::Options, Kind::Flush] } else { vec![Kind::Hello, Kind::Flush] };
            assert_eq!(kinds, want);
        });
    }
}
