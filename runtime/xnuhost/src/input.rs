// SPDX-License-Identifier: MIT
//! Touch and side keys, read from /dev/input/event*. In the default session (session-mode=hosted) zygote and
//! system_server are gone and nothing is grabbed, so the switcher's Volume Up + Down readers keep receiving keys.
//! touch-exclusive=1 is different: only Samsung's native sec_touchscreen is EVIOCGRABbed, in two cases:
//!   - the bench+SHM preview (Android still runs underneath: no tap may reach it), where `read_keys` is false and no
//!     side-key device is opened at all;
//!   - session-mode=takeover (the opt-in Android takeover, docs/booter.md 2.9d: the touchscreen is iOS's alone), where
//!     the side keys are still read as in every session, and never grabbed.
//!
//! Side keys are NEVER grabbed. The grab ends with [`stop`] (before it waits for the reader: a reader blocked in a
//! guest device cannot keep the touchscreen from One UI), with the reader's end, and with the descriptor: an exit, a
//! SIGKILL, the exec of a restart in place (the descriptor is O_CLOEXEC; the kernel ends a grab with its file).
//!
//! Each touch frame goes to the guest with its scan time: an EV_MSC / MSC_TIMESTAMP event before the SYN_REPORT whose
//! value is the low 32 bits of the time the kernel stamped the frame with, in counter ticks (CNTVCT_EL0, the guest's
//! mach_absolute_time; 19.2 MHz on the S24 Ultra, so the value wraps every 223 s). The guest's transport uses it as the
//! HID report's time and for its latency counts (kernel/patches/oneios-hosted/hid/README.md, "Latency"). A kernel
//! without that code ignores it.
//!
//! SYN_DROPPED (the kernel's buffer for this reader overflowed: the input thread did not run for about 8 frames) is
//! handled as evdev's documentation says: the events up to the next SYN_REPORT are dropped and the device's state is
//! read again (EVIOCGMTSLOTS, EVIOCGKEY). The difference from what the guest was last sent goes to it as frames of their
//! own: a lift lost in the drop would otherwise leave a finger down in iOS until the next contact in that slot.
//!
//! While fingers are down the reader counts what it sees and logs one line when the last finger lifts (input: touch:
//! ...): the frames and their spacing (the touch IC's report rate), how long after the scan the reader got each frame,
//! and, for a touch on a still screen, how long until the guest showed its next frame ([`frame_shown`], called from
//! the display).
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicUsize, Ordering};
use std::sync::Mutex;

use xh_core::logi;

use ev::*;
/// Linux's event codes (linux/input-event-codes.h), the ones the input path uses (input.rs, pen.rs, input_script.rs,
/// virtio_input.rs): xh-core's.
pub use xh_core::evdev::codes as ev;

/// Where a filter's events go: the guest's virtio-input device ([`crate::virtio_input`]), or a test's capture.
pub trait EventSink {
    fn event(&self, kind: u16, code: u16, value: i32);
    /// Events still waiting for the guest's buffers.
    fn backlog(&self) -> u32 {
        0
    }
}

impl<T: EventSink + ?Sized> EventSink for &T {
    fn event(&self, kind: u16, code: u16, value: i32) {
        (**self).event(kind, code, value)
    }
    fn backlog(&self) -> u32 {
        (**self).backlog()
    }
}

impl<T: EventSink + ?Sized> EventSink for std::sync::Arc<T> {
    fn event(&self, kind: u16, code: u16, value: i32) {
        (**self).event(kind, code, value)
    }
    fn backlog(&self) -> u32 {
        (**self).backlog()
    }
}

/// The multitouch slots a reader keeps.
pub const SLOTS: usize = 16;
const SIDE_KEYS: [u16; 3] = [KEY_VOLUMEDOWN, KEY_VOLUMEUP, KEY_POWER];

/// An event as read, its time on CLOCK_MONOTONIC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub time_ns: i64,
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

impl Event {
    pub const fn new(time_ns: i64, kind: u16, code: u16, value: i32) -> Event {
        Event { time_ns, kind, code, value }
    }
}

/// A device's state as its ioctls give it: per slot the tracking id (-1 none) and position; the current slot; the side
/// keys held (bit 0 volume down, 1 volume up, 2 power).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub id: [i32; SLOTS],
    pub x: [i32; SLOTS],
    pub y: [i32; SLOTS],
    pub slot: u32,
    pub keys: u32,
}

impl Default for State {
    fn default() -> State {
        State { id: [-1; SLOTS], x: [0; SLOTS], y: [0; SLOTS], slot: 0, keys: 0 }
    }
}

/// What one gesture (first finger down to last finger up) looked like: logged when it ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timing {
    /// touch frames; contacts begun; frames the guest had no buffers for
    pub frames: u32,
    pub began: u32,
    pub held: u32,
    /// the guest showed no frame for 100 ms before the touch-down
    pub still: bool,
    pub first_scan_ns: i64,
    pub last_scan_ns: i64,
    pub gap_min_ns: i64,
    pub gap_max_ns: i64,
    pub read_sum_ns: i64,
    pub read_min_ns: i64,
    pub read_max_ns: i64,
}

/// A device's state read again after SYN_DROPPED (None: it could not be).
pub type ReadState<S> = fn(&Source<S>) -> Option<State>;

/// One reader's filter: a device's events to the guest.
pub struct Source<S> {
    pub sink: S,
    pub name: String,
    pub touch: bool,
    /// after SYN_DROPPED, until the next SYN_REPORT
    pub dropping: bool,
    pub fd: Option<std::os::fd::OwnedFd>,
    pub read_state: Option<ReadState<S>>,
    pub slot: u32,
    pub sent_keys: u32,
    pub began: u32,
    pub frame_began: u32,
    pub drops: u32,
    /// the tracking id the guest was last sent per slot
    pub sent_id: [i32; SLOTS],
    pub timing: Timing,
}

// ---- the touch-to-frame measurement (CLOCK_MONOTONIC ns) ------------------------------------------------------------
static LAST_SHOWN_NS: AtomicI64 = AtomicI64::new(0); // the guest's last frame, 0 = none yet
static DOWN_NS: AtomicI64 = AtomicI64::new(0); // a touch-down on a still screen that waits for its frame, 0 = none
static DOWN_FRAME_NS: AtomicI64 = AtomicI64::new(-1); // the wait of the last such touch-down, -1 = none measured
/// The screen counts as still when the guest showed no frame for this long before a touch-down.
const STILL_NS: i64 = 100_000_000;

/// CLOCK_MONOTONIC in ns.
pub fn monotonic_ns() -> i64 {
    xh_core::time::monotonic_ns() as i64
}

/// A start that failed: why is in the log already (as the C's -1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failed;

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("failed (the log says why)")
    }
}

impl std::error::Error for Failed {}

/// The display showed a guest frame: the end of the touch-to-frame measurement.
pub fn frame_shown() {
    let now = monotonic_ns();
    let down = DOWN_NS.load(Ordering::Acquire);
    LAST_SHOWN_NS.store(now, Ordering::Release);
    if down != 0 && now >= down && DOWN_NS.compare_exchange(down, 0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        DOWN_FRAME_NS.store(now - down, Ordering::Release);
    }
}

/// The guest's last frame shown (CLOCK_MONOTONIC ns, 0: none yet): the display's tests.
#[cfg(test)]
pub(crate) fn last_shown_ns() -> i64 {
    LAST_SHOWN_NS.load(Ordering::Acquire)
}

static POWER_KEY: AtomicUsize = AtomicUsize::new(0);

/// Called on the input thread at each press of the power key (KEY_POWER down) that is passed to the guest: the
/// brightness bridge wakes the touch IC (br_bright_key). None: nothing.
pub fn set_power_key(f: Option<fn()>) {
    POWER_KEY.store(f.map_or(0, |f| f as usize), Ordering::Release);
}

fn power_key_pressed() {
    let p = POWER_KEY.load(Ordering::Acquire);
    if p != 0 {
        // SAFETY: POWER_KEY only ever holds 0 or a `fn()` stored by set_power_key.
        let f: fn() = unsafe { std::mem::transmute::<usize, fn()>(p) };
        f();
    }
}

/// A candidate evdev direct touchscreen, against a native Samsung touchscreen when present. An attached wearable can
/// advertise INPUT_PROP_DIRECT and multitouch axes too (e.g. HUAWEI Band 11 on the S26 Ultra): evdev event numbers are
/// not stable across reconnects, so the phone's own direct touchscreen is chosen when it is there.
pub fn accept_touch(name: Option<&str>, direct: bool, have_touch: bool, native_present: bool) -> bool {
    direct && !have_touch && (!native_present || name == Some("sec_touchscreen"))
}

/// The exclusive grab (a bench+SHM preview, session-mode=takeover) admits only the native sec_touchscreen: it never
/// falls back to a wearable or a secondary pointer.
pub fn exclusive_touch_allowed(name: Option<&str>, direct: bool, have_touch: bool) -> bool {
    direct && !have_touch && name == Some("sec_touchscreen")
}

/// The frame's scan time as the guest's counter: the counter now, less the time since the scan.
fn scan_ticks(scan_ns: i64, now_ns: i64, now_counter: u64, hz: u64) -> u32 {
    let ago = if now_ns > scan_ns { (now_ns - scan_ns) as u64 } else { 0 };
    let ticks = ago / 1_000_000_000 * hz + ago % 1_000_000_000 * hz / 1_000_000_000;
    now_counter.wrapping_sub(ticks) as u32
}

/// At most `max` bytes of `s`, cut at a character boundary (C's snprintf into a char array of max + 1).
pub(crate) fn clip(s: &str, max: usize) -> String {
    xh_core::text::truncate(s, max).to_owned()
}

/// ms with one decimal, as the C's "%lld.%lld".
fn ms1(ns: i64) -> String {
    format!("{}.{}", ns / 1_000_000, ns / 100_000 % 10)
}

fn plural(n: u32) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

impl<S: EventSink> Source<S> {
    /// A reader's filter for the guest device `sink` (`touch`: a touch screen, else a key device).
    pub fn new(sink: S, name: &str, touch: bool) -> Source<S> {
        Source {
            sink,
            name: clip(name, 23), // C's char name[24]
            touch,
            dropping: false,
            fd: None,
            read_state: None,
            slot: 0,
            sent_keys: 0,
            began: 0,
            frame_began: 0,
            drops: 0,
            sent_id: [-1; SLOTS],
            timing: Timing::default(),
        }
    }

    fn send(&mut self, kind: u16, code: u16, value: i32) {
        if kind == EV_ABS && code == ABS_MT_SLOT {
            self.slot = if (0..SLOTS as i32).contains(&value) { value as u32 } else { SLOTS as u32 };
        } else if kind == EV_ABS && code == ABS_MT_TRACKING_ID && (self.slot as usize) < SLOTS {
            let s = self.slot as usize;
            if value >= 0 && self.sent_id[s] < 0 {
                self.began += 1;
            }
            self.sent_id[s] = if value < 0 { -1 } else { value };
        } else if kind == EV_KEY {
            if let Some(k) = SIDE_KEYS.iter().position(|&c| c == code) {
                if value != 0 {
                    self.sent_keys |= 1 << k;
                } else {
                    self.sent_keys &= !(1 << k);
                }
            }
        }
        self.sink.event(kind, code, value);
    }

    fn contacts(&self) -> u32 {
        self.sent_id.iter().filter(|&&id| id >= 0).count() as u32
    }

    fn log_gesture(&mut self) {
        let t = self.timing;
        if t.frames == 0 {
            return;
        }
        let frame = DOWN_FRAME_NS.swap(-1, Ordering::AcqRel);
        DOWN_NS.store(0, Ordering::Release);
        let shown = if t.still && frame >= 0 {
            format!("; the guest's next frame {} ms after the touch-down", ms1(frame))
        } else if t.still {
            "; no guest frame after the touch-down".to_owned()
        } else {
            String::new()
        };
        let held = if t.held != 0 {
            format!("; {} frame{} waited for the guest's buffers", t.held, if t.held == 1 { "" } else { "s" })
        } else {
            String::new()
        };
        let span = t.last_scan_ns - t.first_scan_ns;
        logi!(
            "input: touch: {} frame{} in {} ms, {} contact{}, a frame every {}..{} ms; read {}/{}/{} us after the scan \
             (min/avg/max){}{}",
            t.frames,
            plural(t.frames),
            span / 1_000_000,
            t.began,
            plural(t.began),
            ms1(t.gap_min_ns),
            ms1(t.gap_max_ns),
            t.read_min_ns / 1000,
            t.read_sum_ns / t.frames as i64 / 1000,
            t.read_max_ns / 1000,
            held,
            shown
        );
        self.timing = Timing::default();
    }

    /// One touch frame was sent: its timing.
    fn count_frame(&mut self, scan_ns: i64, now_ns: i64) {
        if self.timing.frames == 0 && self.contacts() == 0 {
            self.frame_began = self.began; // no gesture: nothing was down nor is
            return;
        }
        crate::frame_path::touch(scan_ns.max(0) as u64 / 1000); // the frame line's touch -> next flush
        let read = if now_ns > scan_ns { now_ns - scan_ns } else { 0 };
        let t = &mut self.timing;
        if t.frames == 0 {
            t.first_scan_ns = scan_ns;
            t.read_min_ns = read;
            t.gap_min_ns = 0;
            // A touch-down on a still screen: the display's next frame is the guest's answer to it.
            let shown = LAST_SHOWN_NS.load(Ordering::Acquire);
            t.still = shown != 0 && scan_ns - shown >= STILL_NS;
            DOWN_FRAME_NS.store(-1, Ordering::Release);
            DOWN_NS.store(if t.still { scan_ns } else { 0 }, Ordering::Release);
        } else {
            let gap = scan_ns - t.last_scan_ns;
            if t.frames == 1 || gap < t.gap_min_ns {
                t.gap_min_ns = gap;
            }
            t.gap_max_ns = t.gap_max_ns.max(gap);
        }
        t.last_scan_ns = scan_ns;
        t.frames += 1;
        t.began += self.began - self.frame_began;
        self.frame_began = self.began;
        t.read_sum_ns += read;
        t.read_min_ns = t.read_min_ns.min(read);
        t.read_max_ns = t.read_max_ns.max(read);
        if self.sink.backlog() != 0 {
            self.timing.held += 1;
        }
        if self.contacts() == 0 {
            self.log_gesture();
        }
    }

    fn end_frame(&mut self, scan_ns: i64, now_ns: i64, now_counter: u64, hz: u64) {
        if self.touch {
            self.send(EV_MSC, MSC_TIMESTAMP, scan_ticks(scan_ns, now_ns, now_counter, hz) as i32);
        }
        self.send(EV_SYN, SYN_REPORT, 0);
        if self.touch {
            self.count_frame(scan_ns, now_ns);
        }
    }

    /// After SYN_DROPPED: what the device holds now, against what the guest was sent.
    fn resync(&mut self, now_ns: i64, now_counter: u64, hz: u64) {
        self.drops += 1;
        let Some(st) = self.read_state.and_then(|f| f(self)) else {
            logi!(
                "input: {}: events dropped by the kernel (the reader was too slow); the device's state could not be read again",
                self.name
            );
            return;
        };
        if self.touch {
            // Lifts first, in a frame of their own: a slot whose contact ended, or holds another contact now.
            let mut lifted = false;
            for i in 0..SLOTS {
                if self.sent_id[i] < 0 || st.id[i] == self.sent_id[i] {
                    continue;
                }
                self.send(EV_ABS, ABS_MT_SLOT, i as i32);
                self.send(EV_ABS, ABS_MT_TRACKING_ID, -1);
                lifted = true;
            }
            if lifted {
                let down = (self.contacts() > 0) as i32;
                self.send(EV_KEY, BTN_TOUCH, down);
                self.end_frame(now_ns, now_ns, now_counter, hz);
            }
            for i in 0..SLOTS {
                if st.id[i] < 0 {
                    continue;
                }
                self.send(EV_ABS, ABS_MT_SLOT, i as i32);
                if self.sent_id[i] != st.id[i] {
                    self.send(EV_ABS, ABS_MT_TRACKING_ID, st.id[i]);
                }
                self.send(EV_ABS, ABS_MT_POSITION_X, st.x[i]);
                self.send(EV_ABS, ABS_MT_POSITION_Y, st.y[i]);
            }
            self.send(EV_ABS, ABS_MT_SLOT, st.slot as i32); // the device's current slot: its next events may not name it
            let down = (self.contacts() > 0) as i32;
            self.send(EV_KEY, BTN_TOUCH, down);
        }
        for (k, &code) in SIDE_KEYS.iter().enumerate() {
            if (st.keys ^ self.sent_keys) & 1 << k != 0 {
                self.send(EV_KEY, code, ((st.keys >> k) & 1) as i32);
            }
        }
        self.end_frame(now_ns, now_ns, now_counter, hz);
        let n = self.contacts();
        logi!(
            "input: {}: events dropped by the kernel (the reader was too slow, {} time{}); the state was read again: {} \
             contact{} down",
            self.name,
            self.drops,
            plural(self.drops),
            n,
            plural(n)
        );
    }

    /// Events read at `now_ns` (CLOCK_MONOTONIC) = `now_counter` (CNTVCT, `hz` ticks a second), to the guest.
    pub fn feed(&mut self, events: &[Event], now_ns: i64, now_counter: u64, hz: u64) {
        for v in events {
            if self.dropping {
                if v.kind == EV_SYN && v.code == SYN_REPORT {
                    self.dropping = false;
                    self.resync(now_ns, now_counter, hz);
                }
                continue;
            }
            match v.kind {
                EV_SYN => match v.code {
                    SYN_DROPPED => self.dropping = true,
                    SYN_REPORT => self.end_frame(v.time_ns, now_ns, now_counter, hz),
                    _ => {}
                },
                EV_KEY => {
                    // The side keys, and the two the guest also takes from a host that has them (OneiOSSideKeys.inc):
                    // Home, the ringer.
                    let side = matches!(v.code, KEY_VOLUMEDOWN | KEY_VOLUMEUP | KEY_POWER | KEY_MUTE | KEY_HOMEPAGE);
                    if side || (self.touch && v.code == BTN_TOUCH) {
                        self.send(EV_KEY, v.code, v.value);
                    }
                    if v.code == KEY_POWER && v.value == 1 {
                        power_key_pressed();
                    }
                }
                EV_ABS if self.touch => {
                    if matches!(
                        v.code,
                        ABS_MT_SLOT
                            | ABS_MT_TRACKING_ID
                            | ABS_MT_POSITION_X
                            | ABS_MT_POSITION_Y
                            | ABS_MT_PRESSURE
                            | ABS_MT_TOUCH_MAJOR
                    ) {
                        self.send(EV_ABS, v.code, v.value);
                    }
                }
                _ => {}
            }
        }
    }
}

/// evdev's ioctls and its input_event (input.rs and pen.rs): xh-core's.
pub(crate) use xh_core::evdev;

// ---- the reader ------------------------------------------------------------------------------------------------------
use crate::pen;
use crate::virtio_input::{self, InputDev};
use std::os::fd::AsRawFd;

/// What input.rs and input_script.rs get of the pen: the guest's pen device, the host's (its descriptor, or None for
/// the script's), the turn.
pub struct InputPen {
    pub dev: InputDev,
    pub device: pen::Device,
    pub rotate: u32,
    pub mirror: bool,
}

const MAX_DEVICES: usize = 8;

/// The reader's devices. Dropped: the exclusive touchscreen is released before any descriptor is closed.
struct Devices {
    sources: Vec<Source<InputDev>>,
    pen: Option<pen::Source<InputDev>>,
}

impl Drop for Devices {
    fn drop(&mut self) {
        release_exclusive();
    }
}

/// The grabbed touchscreen's descriptor (-1: none). Its owner is the reader's [`Devices`]; whichever of [`stop`] and
/// the devices' drop comes first releases the grab, once (the descriptor stays open until the drop, after both).
static EXCLUSIVE: AtomicI32 = AtomicI32::new(-1);

/// The grab released, once: its line, or why the release failed (the descriptor's close ends it then).
fn release_exclusive() {
    let fd = EXCLUSIVE.swap(-1, Ordering::AcqRel);
    if fd < 0 {
        return;
    }
    if !evdev::grab(fd, false) {
        xh_core::loge!("input: touch-exclusive: explicit release failed: {}", std::io::Error::last_os_error());
    } else {
        logi!("input: touch-exclusive: sec_touchscreen released to Android");
    }
}

/// The line of the grab: the bench preview's, or the takeover's (the side keys read).
fn grabbed_line(read_keys: bool) -> String {
    format!(
        "input: touch-exclusive: sec_touchscreen grabbed{}; Android input hidden until release",
        if read_keys {
            " (session-mode=takeover; the side keys are read, never grabbed)"
        } else {
            " for the bench preview"
        }
    )
}

struct Reader {
    running: std::sync::Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}
static READER: Mutex<Option<Reader>> = Mutex::new(None);

/// Prefer Samsung's own direct touchscreen over a connected wearable's direct virtual display, regardless of which
/// eventN was allocated first. Read only.
fn native_touch_present() -> bool {
    (0..32).filter_map(evdev::open).any(|(_, fd)| {
        let fd = fd.as_raw_fd();
        if evdev::name(fd, 63).unwrap_or_default() != "sec_touchscreen" {
            return false;
        }
        let (mut abs, mut props) = ([0u8; 8], [0u8; 8]);
        evdev::bits(fd, EV_ABS, &mut abs)
            && evdev::props(fd, &mut props)
            && evdev::bit(&abs, ABS_MT_POSITION_X as usize)
            && evdev::bit(&abs, ABS_MT_POSITION_Y as usize)
            && evdev::bit(&props, ev::INPUT_PROP_DIRECT as usize)
    })
}

/// The device's state now, by ioctl (after SYN_DROPPED).
fn read_state(s: &Source<InputDev>) -> Option<State> {
    let fd = s.fd.as_ref()?.as_raw_fd();
    let mut st = State::default();
    let mut key = [0u8; ev::KEY_MAX / 8 + 1];
    if !evdev::keys(fd, &mut key) {
        return None;
    }
    for (k, &code) in SIDE_KEYS.iter().enumerate() {
        if evdev::bit(&key, code as usize) {
            st.keys |= 1 << k;
        }
    }
    if !s.touch {
        return Some(st);
    }
    st.id = evdev::mt_slots(fd, ABS_MT_TRACKING_ID)?;
    st.x = evdev::mt_slots(fd, ABS_MT_POSITION_X)?;
    st.y = evdev::mt_slots(fd, ABS_MT_POSITION_Y)?;
    let info = evdev::abs(fd, ABS_MT_SLOT)?;
    st.slot = if (0..SLOTS as i32).contains(&info.value) { info.value as u32 } else { 0 };
    let first = info.maximum.saturating_add(1);
    if first >= 0 {
        for id in st.id.iter_mut().skip(first as usize) {
            *id = -1; // past the device's slots: none
        }
    }
    Some(st)
}

fn reader_main(mut devices: Devices, running: std::sync::Arc<AtomicBool>) {
    let mut fds: Vec<libc::pollfd> = devices
        .sources
        .iter()
        .map(|s| s.fd.as_ref().map_or(-1, |f| f.as_raw_fd()))
        .chain(devices.pen.as_ref().map(|p| p.device.fd.as_ref().map_or(-1, |f| f.as_raw_fd())))
        .map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 })
        .collect();
    let pen_index = devices.pen.as_ref().map(|_| devices.sources.len());
    let mut raw = [evdev::InputEvent::default(); 64];
    let mut batch = [Event::default(); 64];
    let hz = crate::hostcalls::info().timebase_hz;
    while running.load(Ordering::Acquire) {
        // A pen frame held by the coalescing (pen.rs) is sent when its time comes, even if the pen sends nothing more.
        let mut timeout = 500;
        if let Some(due) = devices.pen.as_ref().map(|p| p.due()).filter(|&d| d != 0) {
            let left = due - monotonic_ns();
            timeout = if left <= 0 { 0 } else { ((left + 999_999) / 1_000_000) as i32 };
        }
        // SAFETY: polls our array of pollfds.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if let Some(p) = devices.pen.as_mut() {
            p.tick(monotonic_ns());
        }
        if ready <= 0 {
            continue;
        }
        for (i, p) in fds.iter().enumerate() {
            if p.revents & libc::POLLIN == 0 {
                continue;
            }
            let Some(count) = evdev::read(p.fd, &mut raw) else { continue };
            let now_ns = monotonic_ns();
            let now_counter = crate::cpu::counter();
            for (b, e) in batch.iter_mut().zip(&raw[..count]) {
                *b = Event::new(e.sec * 1_000_000_000 + e.usec * 1000, e.kind, e.code, e.value);
            }
            if Some(i) == pen_index {
                if let Some(pen) = devices.pen.as_mut() {
                    pen.feed(&batch[..count], now_ns);
                }
            } else {
                devices.sources[i].feed(&batch[..count], now_ns, now_counter, hz);
            }
        }
    }
    drop(devices); // the grab released first, then the descriptors
}

/// Starts the reader: /dev/input/event* into the virtio-input device; with `pen` the pen's device too, read on the
/// same thread into its own guest device (pen.rs). `touch_exclusive` grabs only the native sec_touchscreen (a
/// bench+SHM preview, or session-mode=takeover); `read_keys` false (the bench preview only,
/// [`crate::config::Config::reads_side_keys`]) leaves every side-key device to Android. Err: touch-exclusive could not
/// take sec_touchscreen, or no thread.
pub fn start(
    dev: &InputDev,
    pen: Option<InputPen>,
    width: u32,
    height: u32,
    touch_exclusive: bool,
    read_keys: bool,
) -> Result<(), Failed> {
    let mut devices = Devices { sources: Vec::new(), pen: None };
    let mut have_touch = false;
    let native_touch = native_touch_present();
    for i in 0..32 {
        if devices.sources.len() >= MAX_DEVICES - 1 {
            break; // one place kept for the pen
        }
        let Some((path, fd)) = evdev::open(i) else { continue };
        let raw = fd.as_raw_fd();
        let (mut abs, mut key) = ([0u8; 8], [0u8; 96]);
        evdev::bits(raw, EV_ABS, &mut abs);
        evdev::bits(raw, EV_KEY, &mut key);
        let name = evdev::name(raw, 63).unwrap_or_default();
        let mut touch = evdev::bit(&abs, ABS_MT_POSITION_X as usize) && evdev::bit(&abs, ABS_MT_POSITION_Y as usize);
        // Keep physical side keys on Android during the interactive bench preview (read_keys false). Skipping all
        // key-only nodes also reserves the reader's limited slots for sec_touchscreen even if it was assigned a late
        // eventN number.
        let keys = read_keys && SIDE_KEYS.iter().any(|&k| evdev::bit(&key, k as usize));
        // One touchscreen: the multitouch device that is the screen itself (INPUT_PROP_DIRECT). The S24 Ultra also
        // has "sec_touchpad", a multitouch pointer device with the same axes: its events went into the same guest
        // device as the screen's (one slot state for two sources) and its ranges replaced the screen's. Not taken.
        if touch {
            let mut props = [0u8; 8];
            let direct = evdev::props(raw, &mut props) && evdev::bit(&props, ev::INPUT_PROP_DIRECT as usize);
            if !accept_touch(Some(&name), direct, have_touch, native_touch)
                || (touch_exclusive && !exclusive_touch_allowed(Some(&name), direct, have_touch))
            {
                let reason = if !direct {
                    "not the screen: no INPUT_PROP_DIRECT"
                } else if have_touch {
                    "a touchscreen is already taken"
                } else if touch_exclusive {
                    "exclusive preview only permits sec_touchscreen"
                } else {
                    "the native sec_touchscreen takes priority over a virtual direct touchscreen"
                };
                logi!("input: {} \"{}\": multitouch, not taken ({})", path, name, reason);
                touch = false;
            }
        }
        if !touch && !keys {
            continue;
        }
        // Event times on CLOCK_MONOTONIC, for this reader only (the clock is a property of the open file, not of
        // the device): the scan times the guest gets are taken from them.
        let monotonic = evdev::clock_monotonic(raw);
        let mut ranges = String::new();
        if touch {
            if touch_exclusive {
                if !evdev::grab(raw, true) {
                    xh_core::loge!(
                        "input: touch-exclusive: EVIOCGRAB {} \"{}\" refused: {}; guest will not boot",
                        path,
                        name,
                        std::io::Error::last_os_error()
                    );
                    return Err(Failed);
                }
                EXCLUSIVE.store(raw, Ordering::Release); // the descriptor goes into devices below and lives as long
                logi!("{}", grabbed_line(read_keys));
            }
            have_touch = true;
            // The guest gets the panel's own coordinate ranges and scales them to its frame.
            let axes = [
                ABS_MT_SLOT,
                ABS_MT_TRACKING_ID,
                ABS_MT_POSITION_X,
                ABS_MT_POSITION_Y,
                ABS_MT_PRESSURE,
                ABS_MT_TOUCH_MAJOR,
            ];
            let mut got = [evdev::AbsInfo::default(); 6];
            for (a, &axis) in axes.iter().enumerate() {
                if !evdev::bit(&abs, axis as usize) {
                    continue;
                }
                let Some(info) = evdev::abs(raw, axis) else { continue };
                virtio_input::set_abs(dev, axis as u32, info.minimum, info.maximum, info.resolution);
                got[a] = info;
            }
            let [slots, _, x, y, pressure, major] = got;
            ranges = format!(
                " (x {}..{}, y {}..{}, {} slots, major 0..{}, pressure 0..{}{})",
                x.minimum,
                x.maximum,
                y.minimum,
                y.maximum,
                slots.maximum + 1,
                major.maximum,
                pressure.maximum,
                if monotonic { "" } else { "; event times not on CLOCK_MONOTONIC" }
            );
        }
        logi!(
            "input: {} \"{}\"{}{}{}",
            path,
            name,
            if touch { " touch" } else { "" },
            ranges,
            if keys { " keys" } else { "" }
        );
        let mut s = Source::new(dev.clone(), if touch { "touch" } else { &name }, touch);
        s.fd = Some(fd);
        s.read_state = Some(read_state);
        devices.sources.push(s);
    }
    if touch_exclusive && !have_touch {
        xh_core::loge!("input: touch-exclusive: native sec_touchscreen not available; guest will not boot");
        return Err(Failed);
    }
    // The pen (pen.rs): found and opened before (session, pen::lookup), read here beside the others, its own filter.
    if let Some(p) = pen.filter(|p| p.device.fd.is_some()) {
        if devices.sources.len() < MAX_DEVICES {
            let mut s = pen::Source::new(p.dev, p.device, p.rotate, p.mirror);
            s.set_panel(width, height); // a hover that moves less than one of the guest's pixels is not sent
            s.read_state = Some(pen::read_state);
            devices.pen = Some(s);
        }
    }
    if devices.sources.is_empty() && devices.pen.is_none() {
        logi!("input: no touchscreen or key device under /dev/input");
        return Ok(());
    }
    let running = std::sync::Arc::new(AtomicBool::new(true));
    let r = running.clone();
    let thread = std::thread::Builder::new().spawn(move || reader_main(devices, r)).map_err(|_| Failed)?;
    *READER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Reader { running, thread });
    Ok(())
}

/// Stops the reader and closes its devices, the exclusive grab released first. Once: a second call finds no reader.
pub fn stop() {
    let reader = READER.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(r) = reader {
        // Before the join: a reader that does not come back (blocked in a guest device's lock, docs/booter.md 2.9c)
        // must not keep the touchscreen from One UI until the stop path's limit. It still holds the descriptor.
        release_exclusive();
        r.running.store(false, Ordering::Release);
        let _ = r.thread.join();
    }
}

#[cfg(test)]
mod tests;
