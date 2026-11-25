// SPDX-License-Identifier: MIT
//! The brightness bridge's session (brightness.c's second half): the panel's worker, the touch IC, the light sensor.

use super::{level_of, map, AlsFrame, FULL, TREBLE_NAMES};
use crate::hw::{self, Backend, Bridge};
use crate::{br_error, br_info};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use xh_core::thread::lock;
use xh_core::{evdev, parse};

const STEP: Duration = Duration::from_millis(16);
const TOUCH_MAX: usize = 4;
/// No report for this long: the sensor was turned off under us.
const ALS_STALE_MS: u64 = 3000;

/// For the tests: called for every write, in order: ("panel", value) and ("touch", "2,0" ...). None on the phone.
pub type TraceFn = Arc<dyn Fn(&str, &str) + Send + Sync>;
/// The light sensor's listener; runs on the sensor's thread.
pub type AlsFn = Arc<dyn Fn(&AlsSample) + Send + Sync>;

/// How the bridge opens. `Options::default()` is the phone's paths and no environment; [`Options::from_env`] is what
/// xnuhost opens with (C: `br_bright_open(NULL)`).
#[derive(Clone, Default)]
pub struct Options {
    /// None: /sys/class/backlight
    pub backlight_dir: Option<PathBuf>,
    /// None: /dev/input
    pub input_dir: Option<PathBuf>,
    /// None: /sys/class/input
    pub input_sysfs: Option<PathBuf>,
    /// None: als_rear
    pub als_name: Option<String>,
    /// None: /sys/class/sec (the touch IC's power state, README.md)
    pub touch_dir: Option<PathBuf>,
    /// 0: 50. The slider's start when One UI left the panel at 0 (ONEIOS_BRIGHT_START)
    pub start_percent: u32,
    /// The IC is not put to sleep at iOS's display off (ONEIOS_TSP_OFF=0)
    pub no_touch_off: bool,
    /// Also the S Pen's IC, <touch_dir>/sec_epen/enabled, as the touch IC (ONEIOS_PEN_IC=1: xnuhost sets it when it
    /// gives the guest the host's pen, pen=auto; docs/hardware.md 7f)
    pub pen_ic: bool,
    /// 0: ceiling(max_brightness) (ONEIOS_BRIGHT_MAX)
    pub ceiling: u32,
    /// 0: 1 (ONEIOS_BRIGHT_MIN)
    pub floor: u32,
    /// 0: 500, what iOS is told the top is (ONEIOS_BRIGHT_NITS)
    pub max_nits: u32,
    /// 0: 1000, uncalibrated (ONEIOS_ALS_SCALE)
    pub als_millilux_per_count: u32,
    pub no_als: bool,
    /// The light comes from elsewhere ([`Bright::als_feed`]: the sensor hub's front sensor, runtime/bridges/sensors), not
    /// from the evdev sensor: its description for the log. The evdev sensor is then not opened.
    pub als_external: Option<String>,
    /// Samsung, Treble; Unset: the process's hw-backend
    pub backend: Backend,
    pub trace: Option<TraceFn>,
}

/// C's strtoul(text, NULL, 10) of a variable that is set and not empty; else `fallback`.
fn env_u32(name: &str, fallback: u32) -> u32 {
    match std::env::var(name) {
        Ok(text) if !text.is_empty() => parse::strtoul(&text, 10).map_or(0, |(v, _)| v as u32),
        _ => fallback,
    }
}

impl Options {
    /// The phone's paths, with ONEIOS_BRIGHT_MAX, _MIN, _NITS, _START, ONEIOS_ALS_SCALE, ONEIOS_TSP_OFF, ONEIOS_PEN_IC.
    pub fn from_env() -> Options {
        Options {
            ceiling: env_u32("ONEIOS_BRIGHT_MAX", 0),
            floor: env_u32("ONEIOS_BRIGHT_MIN", 0),
            max_nits: env_u32("ONEIOS_BRIGHT_NITS", 0),
            als_millilux_per_count: env_u32("ONEIOS_ALS_SCALE", 0),
            start_percent: env_u32("ONEIOS_BRIGHT_START", 0),
            no_touch_off: env_u32("ONEIOS_TSP_OFF", 1) == 0,
            pen_ic: env_u32("ONEIOS_PEN_IC", 0) == 1,
            ..Options::default()
        }
    }
}

/// One light sensor reading as sent to the listener.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AlsSample {
    pub millilux: u32,
    pub clear: u32,
    pub ir: u32,
    pub uv: u32,
}

/// What the bridge found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info {
    pub panel: bool,
    pub als: bool,
    /// The brightness file.
    pub node: String,
    /// The event device.
    pub als_node: String,
    pub max_brightness: u32,
    pub floor: u32,
    pub ceiling: u32,
    /// One UI's value, read at open and written back at close.
    pub saved: u32,
    pub max_nits: u32,
    /// The node's value iOS starts from: saved, or the start level when saved is 0.
    pub start: u32,
    /// Touch ICs found (sec_touchscreen).
    pub touch_nodes: usize,
    /// Of those, the S Pen's IC (sec_epen; only with pen_ic).
    pub pen_nodes: usize,
    /// Samsung or Treble: the form in use.
    pub backend: Backend,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TouchRequest {
    #[default]
    None,
    Sleep,
    Wake,
}

/// The panel worker's state, under the lock.
struct State {
    stop: bool,
    pending: bool,
    linear: bool,
    /// The fade, on the level scale.
    from: u32,
    target: u32,
    fade_ms: u32,
    /// Where the fade is now.
    level: u32,
    started: Instant,
    /// What the worker does to the IC next.
    touch_request: TouchRequest,
    /// iOS's display power, as its framebuffer asked.
    display_on: bool,
    /// The level before the display went off: where it comes back on.
    lit_level: u32,
    lit_linear: bool,
}

/// The touch IC (Samsung's sec input class): its power state follows the display's.
#[derive(Default)]
struct Touch {
    /// Each IC's `enabled` file.
    nodes: Vec<PathBuf>,
    on: bool,
    failures: u32,
}

/// What the panel worker shares with the session.
struct Panel {
    file: Option<File>,
    floor: u32,
    ceiling: u32,
    /// The node's value written last.
    written: AtomicU32,
    state: Mutex<State>,
    wake: Condvar,
    touch: Mutex<Touch>,
    /// iOS's display off puts the IC to sleep.
    touch_sleep: bool,
    trace: Option<TraceFn>,
}

impl Panel {
    fn trace(&self, what: &str, value: &str) {
        if let Some(trace) = &self.trace {
            trace(what, value);
        }
    }

    fn write(&self, value: u32) {
        if value == self.written.load(Ordering::Relaxed) {
            return;
        }
        self.trace("panel", &value.to_string());
        let text = format!("{value}\n");
        if let Some(file) = &self.file {
            if file.write_at(text.as_bytes(), 0).is_ok_and(|n| n == text.len()) {
                self.written.store(value, Ordering::Relaxed);
            }
        }
    }

    /// What One UI's input HAL writes at a panel power change: "<display state>,<event>", state 2 on / 1 off, event 0
    /// early then 1 late. On: only after the backlight is not 0. A write that fails is logged (the first three) and
    /// nothing else changes.
    fn touch_power(&self, on: bool) {
        let mut touch = lock(&self.touch);
        let state = if on { 2 } else { 1 };
        for i in 0..touch.nodes.len() {
            for event in 0..2 {
                self.trace("touch", &format!("{state},{event}"));
                if let Err(e) = write_text(&touch.nodes[i], &format!("{state},{event}\n")) {
                    touch.failures += 1;
                    if touch.failures <= 3 {
                        br_error!(
                            "brightness: touch: {} <- {state},{event}: {}",
                            touch.nodes[i].display(),
                            xh_core::sys::error_text(&e)
                        );
                    }
                }
            }
        }
        touch.on = on;
    }

    fn worker(&self) {
        let mut s = lock(&self.state);
        while !s.stop {
            if !s.pending && s.touch_request == TouchRequest::None {
                s = self.wake.wait(s).unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            let touch = std::mem::take(&mut s.touch_request);
            let step = s.pending;
            let (mut value, mut more) = (0, false);
            if step {
                let elapsed = u32::try_from(s.started.elapsed().as_millis()).unwrap_or(u32::MAX);
                s.level = super::fade_at(s.from, s.target, elapsed, s.fade_ms);
                if s.level == s.target {
                    s.pending = false;
                }
                value = map(s.level, s.linear, self.floor, self.ceiling);
                more = s.pending;
            }
            drop(s);
            // The touch IC follows iOS's display power (display()), never the backlight's level: asleep only when iOS
            // turns its display off, woken at every display on and at every power-key press (key()). Off: the IC
            // first, then the backlight. On: the backlight first.
            if touch == TouchRequest::Sleep && self.touch_sleep && lock(&self.touch).on {
                self.touch_power(false);
            }
            if step {
                self.write(value); // outside the lock: set() never waits for the panel
            }
            if touch == TouchRequest::Wake {
                self.touch_power(true);
            }
            if more {
                thread::sleep(STEP);
            }
            s = lock(&self.state);
        }
    }
}

/// The external light source ([`Bright::als_feed`]).
struct ExtAls {
    listener: Mutex<Option<AlsFn>>,
    sent: Mutex<Option<(u32, Instant)>>,
}

/// The light sensor, shared with its worker.
struct Als {
    file: File,
    /// Its `enable` file, when writable.
    enable: Option<PathBuf>,
    enabled_here: AtomicBool,
    scale: u32,
    stop: AtomicBool,
    listener: Mutex<Option<AlsFn>>,
}

impl Als {
    fn enable(&self) {
        if let Some(path) = &self.enable {
            if write_text(path, "1\n").is_ok() {
                self.enabled_here.store(true, Ordering::Relaxed);
            }
        }
    }

    fn worker(&self) {
        let mut frame = AlsFrame::default();
        let start = Instant::now();
        let now_ms = || start.elapsed().as_millis() as u64;
        let (mut last_sent, mut sent_at, mut sent) = (0u32, 0u64, false);
        let (mut heard_at, mut enabled_at) = (0u64, 0u64);
        let mut events = [evdev::InputEvent::default(); 32];
        while !self.stop.load(Ordering::Relaxed) {
            let listener = lock(&self.listener).clone();
            let ready = poll_in(&self.file, 200);
            let now = now_ms();
            match ready {
                Ready::In => {
                    let n = evdev::read(self.file.as_raw_fd(), &mut events).unwrap_or(0);
                    for ev in &events[..n] {
                        if !frame.feed(ev.kind, ev.code, ev.value) {
                            continue;
                        }
                        heard_at = now;
                        let millilux = frame.millilux(self.scale);
                        if let Some(f) = &listener {
                            if !sent || super::als_changed(last_sent, millilux, (now - sent_at) as u32) {
                                f(&AlsSample { millilux, clear: frame.clear, ir: frame.ir, uv: frame.uv });
                                (last_sent, sent_at, sent) = (millilux, now, true);
                            }
                        }
                    }
                }
                Ready::Gone => {
                    br_error!("brightness: the light sensor's device went away");
                    break;
                }
                Ready::Timeout => {}
            }
            // The camera HAL turns the same sensor on and off (its flicker detection): turn it on again.
            if now - heard_at > ALS_STALE_MS && now - enabled_at > ALS_STALE_MS {
                self.enable();
                enabled_at = now;
            }
        }
    }
}

/// An open bridge. Dropping it (or [`Bright::close`]) writes One UI's value back (the start value when that was 0),
/// wakes the touch IC, and turns the sensor off if this session turned it on. Within 300 ms.
pub struct Bright {
    info: Info,
    panel: Arc<Panel>,
    panel_thread: Mutex<Option<JoinHandle<()>>>,
    als: Option<Arc<Als>>,
    als_thread: Mutex<Option<JoinHandle<()>>>,
    /// The external light source's listener and what was sent last (millilux, when)
    ext: Option<ExtAls>,
    /// [`Bright::shutdown`] ran (it runs once)
    shut: AtomicBool,
}

impl Bright {
    /// Opens the panel's node and the light sensor. None (with a log line) when neither is there. The light sensor is
    /// turned on through its `enable` file if it was off, and turned off again at close.
    pub fn open(o: &Options) -> Option<Bright> {
        let mut info = Info { max_nits: if o.max_nits != 0 { o.max_nits } else { 500 }, ..Info::default() };
        let als_scale = if o.als_millilux_per_count != 0 { o.als_millilux_per_count } else { 1000 };
        let dir = o.backlight_dir.clone().unwrap_or_else(|| "/sys/class/backlight".into());
        info.backend = hw::select(Bridge::Backlight, o.backend);
        let mut treble = info.backend == Backend::Treble;
        let mut found = if treble { panel_find_named(&dir) } else { panel_find(&dir) };
        if found.is_err() && treble {
            let what = format!(
                "no backlight class device panel0-backlight, panel1-backlight, backlight, lcd-backlight or panel with \
                 a max_brightness under {}",
                cut(&dir.display().to_string(), 200)
            );
            if !hw::missing(Bridge::Backlight, &what) {
                return None;
            }
            treble = false; // hw-backend-fallback=1: the samsung form, all of it
            info.backend = Backend::Samsung;
            found = panel_find(&dir);
        }
        let mut file = None;
        let mut panel_error = io::Error::from_raw_os_error(libc::ENOENT);
        match found {
            Ok((node, max)) => {
                info.node = node.display().to_string();
                info.max_brightness = max;
                match read_u32(&node).and_then(|saved| Ok((saved, OpenOptions::new().write(true).open(&node)?))) {
                    Ok((saved, f)) => {
                        info.saved = saved;
                        file = Some(f);
                    }
                    Err(e) => panel_error = e,
                }
            }
            Err(e) => panel_error = e,
        }
        if file.is_some() {
            // Samsung's doubled ranges end at the manual maximum; the treble backend takes the class device's range
            // whole.
            let top = if treble { info.max_brightness } else { super::ceiling(info.max_brightness) };
            info.ceiling = if o.ceiling != 0 && o.ceiling <= info.max_brightness { o.ceiling } else { top };
            info.floor = (if o.floor != 0 { o.floor } else { 1 }).min(info.ceiling);
            info.start = info.saved;
        }
        let panel = Arc::new(Panel {
            file,
            floor: info.floor,
            ceiling: info.ceiling,
            written: AtomicU32::new(info.saved),
            state: Mutex::new(State {
                stop: false,
                pending: false,
                linear: false,
                from: 0,
                target: 0,
                fade_ms: 0,
                level: 0,
                started: Instant::now(),
                touch_request: TouchRequest::None,
                display_on: true, // iOS draws from its start; its framebuffer says when it turns off
                lit_level: 0,
                lit_linear: false,
            }),
            wake: Condvar::new(),
            touch: Mutex::default(),
            touch_sleep: !o.no_touch_off,
            trace: o.trace.clone(),
        });
        let mut bright = Bright {
            info,
            panel,
            panel_thread: Mutex::new(None),
            als: None,
            als_thread: Mutex::new(None),
            ext: None,
            shut: AtomicBool::new(false),
        };
        let b = &mut bright;
        if b.panel.file.is_some() {
            if b.info.saved == 0 {
                // One UI left the panel off (the switch blanks it). iOS takes the level it finds as its own, so 0
                // would be a session at the dimmest setting: start from the middle of the slider, and light the panel
                // now.
                let percent = if o.start_percent != 0 && o.start_percent <= 100 { o.start_percent } else { 50 };
                b.info.start = map(FULL * percent / 100, false, b.info.floor, b.info.ceiling);
                b.panel.write(b.info.start);
            }
            let level = level_of(b.info.start, b.info.floor, b.info.ceiling);
            {
                let mut s = lock(&b.panel.state);
                s.level = level;
                s.target = level;
            }
            let panel = Arc::clone(&b.panel);
            match thread::Builder::new().name("brightness".into()).spawn(move || panel.worker()) {
                Ok(handle) => {
                    *lock(&b.panel_thread) = Some(handle);
                    b.info.panel = true;
                }
                Err(e) => panel_error = e,
            }
        }
        // The touch IC, before any request can reach the worker: One UI may have left it off, and nothing in a
        // session wakes it.
        let touch_dir = o.touch_dir.clone().unwrap_or_else(|| "/sys/class/sec".into());
        {
            let mut touch = lock(&b.panel.touch);
            if !treble {
                touch.nodes = touch_find(&touch_dir);
            }
            b.info.touch_nodes = touch.nodes.len();
            // The S Pen's IC beside them (pen_find): woken, slept and woken again with the touch IC, in the same
            // writes.
            if !treble && o.pen_ic && touch.nodes.len() < TOUCH_MAX {
                if let Some(pen) = pen_find(&touch_dir) {
                    touch.nodes.push(pen);
                    b.info.pen_nodes = 1;
                }
            }
        }
        let touch_dir_text = touch_dir.display().to_string();
        if treble {
            br_info!(
                "brightness: touch: treble: no vendor-neutral power node for the touch IC: left alone (the panel \
                 driver's own)"
            );
        } else if b.info.touch_nodes != 0 || b.info.pen_nodes != 0 {
            b.panel.touch_power(true);
            if b.info.touch_nodes != 0 {
                br_info!(
                    "brightness: touch: {} IC{} under {touch_dir_text} ({}) woken; {}",
                    b.info.touch_nodes,
                    if b.info.touch_nodes > 1 { "s" } else { "" },
                    lock(&b.panel.touch).nodes[0].display(),
                    if b.panel.touch_sleep {
                        "asleep while iOS's display is off (its framebuffer's power request), woken at display on and \
                         at a power-key press; a brightness of 0 does not sleep it"
                    } else {
                        "kept awake when iOS's display is off (ONEIOS_TSP_OFF=0)"
                    }
                );
            } else {
                br_info!(
                    "brightness: touch: no sec_touchscreen IC under {touch_dir_text}: its power state is left alone"
                );
            }
        } else {
            br_info!("brightness: touch: no sec_touchscreen IC under {touch_dir_text}: its power state is left alone");
        }
        if b.info.pen_nodes != 0 {
            br_info!(
                "brightness: pen: the S Pen's IC ({}) woken (2,0 then 2,1); it sleeps and wakes with the touch IC",
                lock(&b.panel.touch).nodes[b.info.touch_nodes].display()
            );
        } else if o.pen_ic && !treble {
            br_info!(
                "brightness: pen: no writable {touch_dir_text}/sec_epen/enabled of sec_e-pen: the pen IC's power state \
                 is left alone (it may send nothing)"
            );
        }
        if b.info.panel {
            br_info!(
                "brightness: panel on: {}, {}..{} of {} (One UI left {}{}), the slider squared",
                b.info.node,
                b.info.floor,
                b.info.ceiling,
                b.info.max_brightness,
                b.info.saved,
                if b.info.saved != 0 { "" } else { ": iOS starts from the start level" }
            );
        } else {
            br_error!(
                "brightness: panel off: no writable backlight device under {}: {}",
                dir.display(),
                xh_core::sys::error_text(&panel_error)
            );
        }

        const TREBLE_ALS: &str =
            "brightness: light sensor: treble: missing: the sensors HAL's light sensor (not read by this bridge yet);";
        if let (false, Some(what)) = (o.no_als, &o.als_external) {
            b.info.als = true;
            b.info.als_node = what.clone();
            b.ext = Some(ExtAls { listener: Mutex::new(None), sent: Mutex::new(None) });
            br_info!("brightness: light sensor on: {what} (the evdev sensor is not opened)");
        } else if !o.no_als && !treble {
            b.als_open(o, als_scale);
        } else if !o.no_als && hw::fallback_allowed() {
            br_info!("{TREBLE_ALS} the samsung lookup instead (hw-backend-fallback=1)");
            b.als_open(o, als_scale);
        } else if !o.no_als {
            br_info!("{TREBLE_ALS} off (hw-backend-fallback=0): auto-brightness has no input");
        }
        if !bright.info.panel && !bright.info.als {
            return None; // the drop restores what open changed
        }
        Some(bright)
    }

    fn als_open(&mut self, o: &Options, scale: u32) {
        let dir = o.input_dir.clone().unwrap_or_else(|| "/dev/input".into());
        let name = o.als_name.as_deref().unwrap_or("als_rear");
        let Some((file, node, event)) = als_find(&dir, name) else {
            br_info!(
                "brightness: no light sensor: no input device named {name} under {} (auto-brightness has no input)",
                dir.display()
            );
            return;
        };
        self.info.als_node = node.display().to_string();
        let path =
            o.input_sysfs.clone().unwrap_or_else(|| "/sys/class/input".into()).join(&event).join("device/enable");
        let enable = writable(&path).then_some(path);
        let als = Arc::new(Als {
            file,
            enable,
            enabled_here: AtomicBool::new(false),
            scale,
            stop: AtomicBool::new(false),
            listener: Mutex::new(None),
        });
        if let Some(path) = &als.enable {
            if read_u32(path).map_or(true, |on| on == 0) {
                als.enable();
            }
        }
        let worker = Arc::clone(&als);
        let Ok(handle) = thread::Builder::new().name("brightness-als".into()).spawn(move || worker.worker()) else {
            return;
        };
        *lock(&self.als_thread) = Some(handle);
        self.info.als = true;
        br_info!(
            "brightness: light sensor on: {} ({name}, not grabbed), {scale} millilux per clear count{}",
            self.info.als_node,
            if als.enabled_here.load(Ordering::Relaxed) {
                ", turned on here"
            } else if als.enable.is_some() {
                ", was on"
            } else {
                ", no enable file"
            }
        );
        self.als = Some(als);
    }

    pub fn info(&self) -> Info {
        self.info.clone()
    }

    /// Never blocks: a worker writes the node, in steps of 16 ms when fade_ms is not 0.
    pub fn set(&self, level: u32, linear: bool, fade_ms: u32) {
        if !self.info.panel {
            return;
        }
        let mut s = lock(&self.panel.state);
        s.from = s.level;
        s.target = level.min(FULL);
        s.linear = linear;
        s.fade_ms = fade_ms.min(10000);
        s.started = Instant::now();
        s.pending = true;
        self.panel.wake.notify_one();
    }

    /// iOS's display power (its framebuffer's power request, OB_BRIGHT_POWER). Off: the touch IC sleeps (unless
    /// ONEIOS_TSP_OFF=0), then the panel goes to its floor, the level kept. On (every time): the level from before the
    /// off when nothing has set another, then the IC woken. A level of 0 alone does neither: it is a dim panel, not an
    /// off one.
    pub fn display(&self, on: bool, why: Option<&str>) {
        {
            let mut s = lock(&self.panel.state);
            let was = s.display_on;
            s.display_on = on;
            if !on && was {
                // Off: the IC sleeps, then the backlight goes to the floor; the level is kept for the way back on.
                s.lit_level = s.target;
                s.lit_linear = s.linear;
                s.touch_request = TouchRequest::Sleep;
                if self.info.panel {
                    s.from = s.level;
                    s.target = 0;
                    s.fade_ms = 0;
                    s.started = Instant::now();
                    s.pending = true;
                }
            } else if on {
                // On (and every repeat of it): the level from before the off, then the IC woken, whatever it was
                // believed to be.
                if !was && self.info.panel && s.target == 0 && s.lit_level != 0 {
                    s.from = s.level;
                    s.target = s.lit_level;
                    s.linear = s.lit_linear;
                    s.fade_ms = 0;
                    s.started = Instant::now();
                    s.pending = true;
                }
                s.touch_request = TouchRequest::Wake;
            }
            self.panel.wake.notify_one();
        }
        br_info!(
            "brightness: iOS's display {} ({}){}",
            if on { "on" } else { "off" },
            why.unwrap_or("?"),
            if on {
                ": the touch IC woken"
            } else if self.panel.touch_sleep {
                ": the touch IC asleep, the panel at its floor"
            } else {
                ": the panel at its floor (touch-sleep=0)"
            }
        );
    }

    /// A press of the power key (xnuhost's evdev reader): the touch IC woken; iOS's display off that may follow sleeps
    /// it.
    pub fn key(&self) {
        if self.info.touch_nodes == 0 && self.info.pen_nodes == 0 {
            return;
        }
        // iOS turns the display on or off next; an off sleeps it again
        lock(&self.panel.state).touch_request = TouchRequest::Wake;
        self.panel.wake.notify_one();
    }

    /// The node's value written last (the saved one before the first set).
    pub fn current(&self) -> u32 {
        self.panel.written.load(Ordering::Relaxed)
    }

    /// Where the panel is now on the slider's scale (the squared curve undone), 0..FULL.
    pub fn level(&self) -> u32 {
        level_of(self.current(), self.info.floor, self.info.ceiling)
    }

    /// The listener runs on the sensor's thread. One listener; None removes it.
    pub fn listen(&self, listener: Option<AlsFn>) {
        if let Some(ext) = &self.ext {
            *lock(&ext.listener) = listener.clone();
        }
        if let Some(als) = &self.als {
            *lock(&als.listener) = listener;
        }
    }

    /// A reading of the external light source (Options::als_external), on the caller's thread: to the listener when it
    /// is the first, changed by a tenth (or 1 lux), or 2 s after the last one sent. No time gate as the evdev sensor's:
    /// the hub reports on change, and a reading held back would stay the guest's last one.
    pub fn als_feed(&self, millilux: u32) {
        let Some(ext) = &self.ext else { return };
        let Some(f) = lock(&ext.listener).clone() else { return };
        let mut sent = lock(&ext.sent);
        let now = Instant::now();
        let send = match *sent {
            None => true,
            Some((last, at)) => {
                now.duration_since(at) >= Duration::from_secs(2) || last.abs_diff(millilux) >= (last / 10).max(1000)
            }
        };
        if send {
            *sent = Some((millilux, now));
            drop(sent);
            f(&AlsSample { millilux, clear: 0, ir: 0, uv: 0 });
        }
    }

    /// Writes One UI's value back (the start value when that was 0), wakes the touch IC, turns the sensor off if this
    /// session turned it on. Within 300 ms. (The same as dropping it.)
    pub fn close(self) {}

    /// What close does, through a shared reference and once (later calls and the drop do nothing more): for xnuhost's
    /// stop, which ends the process without dropping the bridge device that holds this (C: br_bright_close at the
    /// stop). The bridge takes no more requests after it.
    pub fn shutdown(&self) {
        if self.shut.swap(true, Ordering::AcqRel) {
            return;
        }
        lock(&self.panel.state).stop = true;
        self.panel.wake.notify_one();
        if let Some(als) = &self.als {
            als.stop.store(true, Ordering::Relaxed);
        }
        if let Some(handle) = lock(&self.panel_thread).take() {
            let _ = handle.join();
            // One UI's own value; system_server sets it again when it starts. If that was 0, the start value: the
            // return's screen needs a lit panel. Always written: the node may have been set outside the bridge during
            // the session (`oneios-tool phone backlight set N`, or by hand), so the bridge's own last write is not the
            // node's value.
            self.panel.written.store(u32::MAX, Ordering::Relaxed);
            self.panel.write(self.info.start);
        }
        if self.info.touch_nodes != 0 || self.info.pen_nodes != 0 {
            self.panel.touch_power(true); // after the backlight; the return finds the ICs awake
        }
        if let Some(handle) = lock(&self.als_thread).take() {
            let _ = handle.join();
        }
        if let Some(als) = &self.als {
            if als.enabled_here.load(Ordering::Relaxed) {
                if let Some(path) = &als.enable {
                    let _ = write_text(path, "0\n");
                }
            }
        }
    }
}

impl Drop for Bright {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---- the files ----------------------------------------------------------------------------------

/// The first 31 bytes of a file as a number (C's read_u32: strtoul, what follows the digits ignored).
fn read_u32(path: &Path) -> io::Result<u32> {
    let text = read_head(path, 31)?;
    parse::strtoul(&text, 10).map(|(v, _)| v as u32).ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))
}

/// Up to `max` bytes of a file, one read; empty is an error.
fn read_head(path: &Path, max: usize) -> io::Result<String> {
    let buf = xh_core::fs::read_bytes(path, max)?;
    if buf.is_empty() {
        return Err(io::Error::from_raw_os_error(libc::ENODATA));
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// One write of `text` to an existing file.
fn write_text(path: &Path, text: &str) -> io::Result<()> {
    OpenOptions::new().write(true).open(path)?.write(text.as_bytes()).map(|_| ())
}

/// access(path, W_OK).
fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: c is a NUL-terminated path that lives across the call.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// `text` cut to `max` bytes, as C's "%.Ns".
fn cut(text: &str, max: usize) -> &str {
    crate::log::truncate(text, max)
}

fn first_line(text: &str) -> &str {
    text.split(['\r', '\n']).next().unwrap_or("")
}

/// DIR/NAME/brightness when DIR/NAME/max_brightness reads a number other than 0.
fn panel_at(dir: &Path, name: &str) -> Option<(PathBuf, u32)> {
    let max = read_u32(&dir.join(name).join("max_brightness")).ok()?;
    (max != 0).then(|| (dir.join(name).join("brightness"), max))
}

/// Samsung's panel first ("panel"), then Qualcomm's name, then whatever is there.
fn panel_find(dir: &Path) -> io::Result<(PathBuf, u32)> {
    if let Some(found) = ["panel", "panel0-backlight"].iter().find_map(|name| panel_at(dir, name)) {
        return Ok(found);
    }
    fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .find_map(|name| panel_at(dir, &name))
        .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
}

/// The treble backend's lookup: the standard class device names only, each a named file; no folder is listed.
fn panel_find_named(dir: &Path) -> io::Result<(PathBuf, u32)> {
    TREBLE_NAMES.iter().find_map(|name| panel_at(dir, name)).ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
}

/// The touch ICs: every <dir>/tsp*/enabled whose input/name is sec_touchscreen. Not the pen (sec_e-pen), not the
/// touchpad, whatever their folders are called.
fn touch_find(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.file_name().as_encoded_bytes().starts_with(b"tsp"))
        .filter(|e| read_head(&e.path().join("input/name"), 39).is_ok_and(|n| first_line(&n) == "sec_touchscreen"))
        .map(|e| e.path().join("enabled"))
        .filter(|p| writable(p))
        .take(TOUCH_MAX)
        .collect()
}

/// The S Pen's IC (spen, 2026-11-22): <dir>/sec_epen/enabled, the sec input class's power node of Samsung's Wacom
/// driver (drivers/input/sec_input/wacom: "sec,enable_sysinput_enabled" in the S24 Ultra's device tree, so `enabled`
/// takes the same "<display state>,<event>" as the touch IC's: 2,1 runs wacom_enable, 1,0 wacom_disable, which leaves
/// the IC in its garage/AOP survey mode, where it sends no coordinates). One UI's input HAL writes it at every panel
/// power change, as it does the touch IC's, so after the switch's blank it is asleep like the touch IC was (README.md,
/// "The touch IC follows the display"). Taken only with pen_ic (xnuhost gives the guest the host's pen). The folder is
/// the driver's class device name; when it has an input/ link, that must be the pen's ("sec_e-pen"). Appended after
/// the touch ICs.
fn pen_find(dir: &Path) -> Option<PathBuf> {
    let pen = dir.join("sec_epen");
    if File::open(pen.join("input/name")).is_ok() {
        let name = read_head(&pen.join("input/name"), 39).ok()?;
        if first_line(&name) != "sec_e-pen" {
            return None;
        }
    }
    let path = pen.join("enabled");
    (fs::metadata(&path).is_ok_and(|m| m.is_file()) && writable(&path)).then_some(path)
}

// ---- evdev --------------------------------------------------------------------------------------

/// The device's name (EVIOCGNAME), at most 79 bytes.
fn evdev_name(file: &File) -> Option<String> {
    evdev::name(file.as_raw_fd(), 80)
}

/// The first <dir>/event* whose name is `name`: opened read only, non-blocking, and never grabbed.
fn als_find(dir: &Path, name: &str) -> Option<(File, PathBuf, String)> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let event = e.file_name().into_string().ok()?;
        if !event.starts_with("event") {
            return None;
        }
        let path = e.path();
        let file = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path).ok()?;
        (evdev_name(&file)? == name).then_some((file, path, event))
    })
}

enum Ready {
    In,
    Gone,
    Timeout,
}

fn poll_in(file: &File, timeout_ms: i32) -> Ready {
    let mut p = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: one pollfd that lives across the call.
    let ready = unsafe { libc::poll(&mut p, 1, timeout_ms) };
    if ready > 0 && p.revents & libc::POLLIN != 0 {
        Ready::In
    } else if ready > 0 && p.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        Ready::Gone
    } else {
        Ready::Timeout
    }
}
