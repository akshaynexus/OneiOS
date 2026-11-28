// SPDX-License-Identifier: MIT
//! A scripted event source for the virtio-input device: touches and key presses from a text file, for a host with no
//! touchscreen (the PC under qemu-aarch64) and for repeatable checks of the guest's input path.
//! Setting: input-script=FILE (--input-script FILE). It runs beside the evdev reader, or alone with --no-input.
//!
//! ```text
//!   # comment
//!   abs CODE MIN MAX [RES]   an axis of the device's configuration (what input.rs takes from EVIOCGABS on the
//!                            phone). Applied when the file is read, before the guest starts: the guest's driver
//!                            takes the device as a touch screen only if it has ABS_MT_POSITION_X/Y (0x35, 0x36).
//!   ready                    wait until the guest's driver runs the device (DRIVER_OK)
//!   wait MS
//!   ev TYPE CODE VALUE       one evdev event, as written
//!   syn                      ev 0 0 0: the end of a frame; events reach the guest at a frame's end
//!   tap X Y [MS]             a finger in slot 0 at X, Y (the axes' own units), lifted MS later (default 80)
//!   swipe X0 Y0 X1 Y1 [MS]   a finger in slot 0 moved from one point to the other in MS (default 300), 16 ms a step
//!   key CODE [MS]            a key pressed and released MS later (default 100): 116 power, 115 / 114 volume up / down
//!   repeat [N]               the steps after `ready` (or all, without one) again: forever, or N more times
//! ```
//!
//! The pen (pen.rs; a file with any pen- line has one, given to the guest as its own device when xnuhost runs with
//! pen=auto):
//!
//! ```text
//!   pen-abs CODE MIN MAX [RES]   an axis of the scripted pen's own device, as the phone's pen states it (ABS_X 0, ABS_Y 1,
//!                                ABS_PRESSURE 0x18, ABS_DISTANCE 0x19, ABS_TILT_X 0x1a, ABS_TILT_Y 0x1b; RES for tilt:
//!                                units per radian). Its events go through the pen's filter as the phone's do (pen=auto's
//!                                pen-rotate and pen-mirror do not apply: the script is in the panel's frame)
//!   pen-ev TYPE CODE VALUE       one event of the pen's device;  pen-syn: the end of its frame
//!   pen-hover X Y [MS] [DIST]    the pen in range above X, Y (BTN_TOOL_PEN; ABS_DISTANCE DIST), then MS ms (default 100)
//!   pen-stroke X0 Y0 X1 Y1 MS P0 P1 [TX TY]   the tip down at X0, Y0 with pressure P0, moved to X1, Y1 in MS ms (16 ms a
//!                                step) while the pressure goes to P1, at tilt TX, TY (device units); then the tip up, in range
//!   pen-button 0|1               the side button (BTN_STYLUS) up or down
//!   pen-out                      the pen leaves (BTN_TOOL_PEN 0)
//! ```
//!
//! Numbers are decimal or 0x hexadecimal (and, as strtol reads them, octal with a leading 0). A bad line stops
//! xnuhost's start with its number. The events go through input.rs's filter ([`input::Source::feed`]) as a touch
//! screen's do: only the codes the panel's would pass, each frame's end preceded by EV_MSC / MSC_TIMESTAMP with the time
//! it is sent, and one `input: touch:` line per gesture.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use xh_core::{loge, logi};

use crate::input::ev::*;
use crate::input::{self, monotonic_ns, Event, Failed};
use crate::pen;
use crate::virtio_input::{self, InputDev};

/// One step of a script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// An axis of the touch screen's device (or, `pen`, of the scripted pen's): code, min, max, resolution.
    Abs { pen: bool, code: u16, min: i32, max: i32, res: i32 },
    /// Wait until the guest's driver runs the device.
    Ready,
    /// Wait this many ms.
    Wait(i32),
    /// One event of the touch screen's device (or, `pen`, of the pen's).
    Event { pen: bool, kind: u16, code: u16, value: i32 },
    /// The steps after `ready` again: forever (None) or N more times.
    Repeat(Option<i32>),
}

impl Step {
    /// A pen- line's step.
    pub fn is_pen(&self) -> bool {
        matches!(self, Step::Abs { pen: true, .. } | Step::Event { pen: true, .. })
    }
}

/// Up to `max` numbers after the directive; None for something that is not a number (or more than `max`).
fn numbers(args: &str, max: usize) -> Option<Vec<i64>> {
    let v: Option<Vec<i64>> =
        args.split([' ', '\t']).filter(|w| !w.is_empty()).map(|w| xh_core::parse::i64_text(w, 0)).collect();
    v.filter(|v| v.len() <= max)
}

struct Steps(Vec<Step>);

impl Steps {
    fn ev(&mut self, kind: u16, code: u16, value: i64) {
        self.0.push(Step::Event { pen: false, kind, code, value: value as i32 });
    }
    fn pen(&mut self, kind: u16, code: u16, value: i64) {
        self.0.push(Step::Event { pen: true, kind, code, value: value as i32 });
    }
    fn wait(&mut self, ms: i64) {
        self.0.push(Step::Wait(ms as i32));
    }
}

/// One line's steps; false: a bad line.
fn parse_line(directive: &str, v: &[i64], tracking: &mut i64, out: &mut Steps) -> bool {
    let c = v.len();
    let u16_ok = |x: i64| (0..=0xffff).contains(&x);
    match directive {
        "abs" | "pen-abs" if (c == 3 || c == 4) && (0..0x40).contains(&v[0]) && v[1] < v[2] => {
            out.0.push(Step::Abs {
                pen: directive == "pen-abs",
                code: v[0] as u16,
                min: v[1] as i32,
                max: v[2] as i32,
                res: if c == 4 { v[3] as i32 } else { 0 },
            });
        }
        "pen-ev" if c == 3 && u16_ok(v[0]) && u16_ok(v[1]) => out.pen(v[0] as u16, v[1] as u16, v[2]),
        "pen-syn" if c == 0 => out.pen(EV_SYN, 0, 0),
        "pen-hover" if (2..=4).contains(&c) && (c < 3 || v[2] >= 0) => {
            out.pen(EV_ABS, ABS_X, v[0]);
            out.pen(EV_ABS, ABS_Y, v[1]);
            if c == 4 {
                out.pen(EV_ABS, ABS_DISTANCE, v[3]);
            }
            out.pen(EV_KEY, BTN_TOOL_PEN, 1);
            out.pen(EV_SYN, 0, 0);
            out.wait(if c >= 3 { v[2] } else { 100 });
        }
        "pen-stroke" if (c == 7 || c == 9) && v[4] >= 0 => {
            let (x0, y0, x1, y1, ms, p0, p1) = (v[0], v[1], v[2], v[3], v[4], v[5], v[6]);
            let moves = if ms / 16 > 0 { ms / 16 } else { 1 };
            if c == 9 {
                out.pen(EV_ABS, ABS_TILT_X, v[7]);
                out.pen(EV_ABS, ABS_TILT_Y, v[8]);
            }
            out.pen(EV_KEY, BTN_TOOL_PEN, 1);
            out.pen(EV_ABS, ABS_X, x0);
            out.pen(EV_ABS, ABS_Y, y0);
            out.pen(EV_ABS, ABS_DISTANCE, 0);
            out.pen(EV_ABS, ABS_PRESSURE, p0);
            out.pen(EV_KEY, BTN_TOUCH, 1);
            out.pen(EV_SYN, 0, 0);
            for i in 1..=moves {
                out.wait(ms / moves);
                out.pen(EV_ABS, ABS_X, x0 + (x1 - x0) * i / moves);
                out.pen(EV_ABS, ABS_Y, y0 + (y1 - y0) * i / moves);
                out.pen(EV_ABS, ABS_PRESSURE, p0 + (p1 - p0) * i / moves);
                out.pen(EV_SYN, 0, 0);
            }
            out.pen(EV_ABS, ABS_PRESSURE, 0);
            out.pen(EV_KEY, BTN_TOUCH, 0);
            out.pen(EV_SYN, 0, 0);
        }
        "pen-button" if c == 1 && (v[0] == 0 || v[0] == 1) => {
            out.pen(EV_KEY, BTN_STYLUS, v[0]);
            out.pen(EV_SYN, 0, 0);
        }
        "pen-out" if c == 0 => {
            out.pen(EV_KEY, BTN_TOUCH, 0);
            out.pen(EV_KEY, BTN_TOOL_PEN, 0);
            out.pen(EV_SYN, 0, 0);
        }
        "ready" if c == 0 => out.0.push(Step::Ready),
        "wait" if c == 1 && v[0] >= 0 => out.wait(v[0]),
        "ev" if c == 3 && u16_ok(v[0]) && u16_ok(v[1]) => out.ev(v[0] as u16, v[1] as u16, v[2]),
        "syn" if c == 0 => out.ev(EV_SYN, 0, 0),
        "tap" | "swipe"
            if (directive == "tap" && (c == 2 || c == 3)) || (directive == "swipe" && (c == 4 || c == 5)) =>
        {
            let swipe = directive == "swipe";
            let (x0, y0) = (v[0], v[1]);
            let (x1, y1) = if swipe { (v[2], v[3]) } else { (x0, y0) };
            let ms = if swipe {
                if c == 5 {
                    v[4]
                } else {
                    300
                }
            } else if c == 3 {
                v[2]
            } else {
                80
            };
            if ms < 0 {
                return false;
            }
            let moves = if swipe { (ms / 16).max(1) } else { 0 };
            *tracking += 1;
            out.ev(EV_ABS, ABS_MT_SLOT, 0);
            out.ev(EV_ABS, ABS_MT_TRACKING_ID, *tracking);
            out.ev(EV_ABS, ABS_MT_POSITION_X, x0);
            out.ev(EV_ABS, ABS_MT_POSITION_Y, y0);
            out.ev(EV_KEY, BTN_TOUCH, 1);
            out.ev(EV_SYN, 0, 0);
            for i in 1..=moves {
                out.wait(ms / moves);
                out.ev(EV_ABS, ABS_MT_POSITION_X, x0 + (x1 - x0) * i / moves);
                out.ev(EV_ABS, ABS_MT_POSITION_Y, y0 + (y1 - y0) * i / moves);
                out.ev(EV_SYN, 0, 0);
            }
            if !swipe {
                out.wait(ms);
            }
            out.ev(EV_ABS, ABS_MT_TRACKING_ID, -1);
            out.ev(EV_KEY, BTN_TOUCH, 0);
            out.ev(EV_SYN, 0, 0);
        }
        "key" if (c == 1 || c == 2) && v[0] > 0 && v[0] <= 0xffff && (c == 1 || v[1] >= 0) => {
            out.ev(EV_KEY, v[0] as u16, 1);
            out.ev(EV_SYN, 0, 0);
            out.wait(if c == 2 { v[1] } else { 100 });
            out.ev(EV_KEY, v[0] as u16, 0);
            out.ev(EV_SYN, 0, 0);
        }
        "repeat" if c == 0 || (c == 1 && v[0] > 0) => out.0.push(Step::Repeat(v.first().map(|&n| n as i32))),
        _ => return false,
    }
    true
}

/// The script's steps, or the number of its first bad line.
pub fn parse(text: &str) -> Result<Vec<Step>, u32> {
    let mut out = Steps(Vec::new());
    let mut tracking = 0;
    for (n, row) in text.split('\n').enumerate() {
        let line = n as u32 + 1;
        let row = row.split('#').next().unwrap_or("").trim_matches(|c: char| c.is_ascii_whitespace() || c == '\x0b');
        if row.is_empty() {
            continue;
        }
        let (directive, args) = row.split_once([' ', '\t']).unwrap_or((row, ""));
        let ok = numbers(args, 9).is_some_and(|v| parse_line(directive, &v, &mut tracking, &mut out));
        if !ok {
            return Err(line);
        }
    }
    Ok(out.0)
}

struct Loaded {
    path: String,
    steps: Arc<Vec<Step>>,
}

struct Running {
    running: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// Reads and parses the file before the board is made: Ok(whether it has pen- lines, so it wants a pen device), also
/// for no path; Err: unreadable or a bad line (said).
pub fn load(path: &str) -> Result<bool, Failed> {
    if path.is_empty() {
        return Ok(false);
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            loge!("input script {}: {}", path, xh_core::sys::error_text(&e));
            return Err(Failed);
        }
    };
    if bytes.len() > 65535 {
        loge!("input script {}: {}", path, "longer than 64 KiB");
        return Err(Failed);
    }
    let text = String::from_utf8_lossy(&bytes);
    let steps = match parse(&text) {
        Ok(s) => s,
        Err(bad) => {
            loge!("input script {}: {}", path, "bad line");
            loge!("input script {}: line {}", path, bad);
            return Err(Failed);
        }
    };
    let has_pen = steps.iter().any(Step::is_pen);
    *LOADED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Loaded { path: path.to_owned(), steps: Arc::new(steps) });
    Ok(has_pen)
}

fn sleep_ms(mut ms: i64, running: &AtomicBool) {
    while ms > 0 && running.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(ms.min(50) as u64));
        ms -= 50;
    }
}

fn script_main(
    steps: Arc<Vec<Step>>,
    target: InputDev,
    mut source: input::Source<InputDev>,
    mut pen: Option<pen::Source<InputDev>>,
    running: Arc<AtomicBool>,
) {
    let hz = crate::cpu::guest_timebase_hz(crate::hostcalls::info().timebase_hz);
    let mut start = 0;
    let mut left = 0; // repeats still to run; 0 before the first `repeat` is met
    let mut counting = false;
    let mut i = 0;
    while i < steps.len() && running.load(Ordering::Acquire) {
        match steps[i] {
            Step::Abs { .. } => {} // applied when the file was read
            Step::Ready => {
                while running.load(Ordering::Acquire) && !target.ready() {
                    sleep_ms(200, &running);
                }
                if running.load(Ordering::Acquire) {
                    logi!("input script: the guest's driver runs the input device");
                }
                start = i + 1;
            }
            Step::Wait(ms) => sleep_ms(ms as i64, &running),
            Step::Event { pen: is_pen, kind, code, value } => {
                // Through input.rs's filter, as the panel's events go: a frame's end gets its time (EV_MSC /
                // MSC_TIMESTAMP, the time it is sent), and the gesture is counted and logged (input: touch: ...) with
                // its touch-to-frame time.
                let now_ns = monotonic_ns();
                let ev = [Event::new(now_ns, kind, code, value)];
                if is_pen {
                    if let Some(p) = pen.as_mut() {
                        p.feed(&ev, now_ns); // the pen's filter (pen.rs), as the phone's pen
                    }
                } else {
                    source.feed(&ev, now_ns, crate::cpu::guest_counter(), hz);
                }
            }
            Step::Repeat(n) => match n {
                None => {
                    i = start;
                    continue;
                }
                Some(n) => {
                    if !counting {
                        counting = true;
                        left = n;
                    }
                    if left > 0 {
                        left -= 1;
                        i = start;
                        continue;
                    }
                    counting = false;
                }
            },
        }
        i += 1;
    }
    if running.load(Ordering::Acquire) {
        logi!("input script: done");
    }
}

/// Starts the loaded script on the touch screen's device and the pen's (None: pen lines left out).
pub fn start(dev: &InputDev, pen_dev: Option<&InputDev>) -> Result<(), Failed> {
    let (path, steps) = match LOADED.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(l) if !l.steps.is_empty() => (l.path.clone(), l.steps.clone()),
        _ => return Ok(()),
    };
    let mut scripted = pen::Device { name: "the script's pen".into(), stylus: true, ..pen::Device::default() };
    const PEN_CODES: [u16; pen::AXES] = [ABS_X, ABS_Y, ABS_PRESSURE, ABS_DISTANCE, ABS_TILT_X, ABS_TILT_Y];
    let (mut events, mut pen_events) = (0u32, 0u32);
    for s in steps.iter() {
        match *s {
            Step::Abs { pen: true, code, min, max, res } => {
                if let Some(a) = PEN_CODES.iter().position(|&c| c == code) {
                    scripted.axis[a] = pen::Axis::new(min, max, res);
                }
            }
            Step::Abs { pen: false, code, min, max, res } => virtio_input::set_abs(dev, code as u32, min, max, res),
            Step::Event { pen, .. } => {
                events += 1;
                pen_events += pen as u32;
            }
            _ => {}
        }
    }
    let source = input::Source::new(dev.clone(), "script", true);
    let pen = match pen_dev {
        Some(p) => {
            logi!("input script: a pen ({}) on the guest's pen device", pen::describe(&scripted));
            Some(pen::Source::new(p.clone(), scripted, 0, false))
        }
        None => {
            if pen_events != 0 {
                logi!("input script: {} pen events, no pen device (pen=off): left out", pen_events);
            }
            None
        }
    };
    let running = Arc::new(AtomicBool::new(true));
    let (r, target, n) = (running.clone(), dev.clone(), steps.len());
    let thread =
        std::thread::Builder::new().spawn(move || script_main(steps, target, source, pen, r)).map_err(|_| Failed)?;
    *RUNNING.lock().unwrap_or_else(|e| e.into_inner()) = Some(Running { running, thread });
    logi!("input script: {}, {} steps ({} events, {} of them the pen's)", path, n, events, pen_events);
    Ok(())
}

/// Stops the script.
pub fn stop() {
    let r = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(r) = r {
        r.running.store(false, Ordering::Release);
        let _ = r.thread.join();
    }
}

#[cfg(test)]
mod tests;
