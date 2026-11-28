// SPDX-License-Identifier: MIT
//! Port of main.c: the session (docs/booter.md, oneos/interface.md): the machine's assembly from the config, the main
//! loop and its timers, the signals, the stop and the return.
//!
//! What the other modules call: [`stop`], [`stopping`], [`mono_ms`] (any thread), [`guest_ready`] (the kernel's
//! ready host call) and [`guest_frame`] (virtio-gpu, a frame of the guest's own). [`main`] is the program.

mod board;
mod bridges;
mod host;
mod memory;

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use xh_core::text::truncate;
use xh_core::time::monotonic_ms;
use xh_core::{loge, logi};
use xnuhost_abi::{XH_HALT_OFF, XH_HALT_PANIC, XH_HALT_RESTART, XH_HALT_RETURN};

use crate::blk::{self, Blk};
use crate::config::{self, Config, READY_CONTENT_WAIT};
use crate::imageset::{DiskIdentity, ImageSet};
use crate::ready::{ReadyState, ReadyWhy};
use crate::ret::{ReturnDecision, ReturnState};
use crate::usb::{self, PropSource, UsbWatch};
use crate::{cpu, display, input, input_script, mem, perf, sysreg, task};

/// The exit codes (docs/booter.md, oneos/interface.md 5).
pub const EXIT_GUEST_OFF: i32 = 0;
pub const EXIT_SETUP: i32 = 2;
pub const EXIT_RESTART: i32 = 3;
pub const EXIT_PANIC: i32 = 4;

/// The finished config of the running session (set once, when the kernel is placed).
static CONFIG: OnceLock<Config> = OnceLock::new();

/// The session's config; None before the kernel was placed (the tests, a tool).
pub fn config() -> Option<&'static Config> {
    CONFIG.get()
}

// ---- the stop ------------------------------------------------------------------------------------------------------

/// Why the machine stops: the first request wins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StopRequest {
    pub reason: u32,
    pub code: u32,
    pub why: String,
}

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
static STOP: Mutex<Option<StopRequest>> = Mutex::new(None);

/// xh_stop: asks the machine to stop (any thread; returns). Only the first request is kept; it wakes the stop watch.
pub fn stop(reason: u32, code: u32, why: &str) {
    let mut s = STOP.lock().unwrap_or_else(|e| e.into_inner());
    if s.is_none() {
        // what C kept of it: 95 bytes
        *s = Some(StopRequest { reason, code, why: truncate(why, 95).to_owned() });
        STOP_REQUESTED.store(true, Ordering::Release);
        drop(s);
        watch::post();
    }
}

/// xh_stopping: a stop was asked.
pub fn stopping() -> bool {
    STOP_REQUESTED.load(Ordering::Acquire)
}

/// The stop that was asked, if any.
pub fn stop_request() -> Option<StopRequest> {
    STOP.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The stop path's change of the stop's reason (a restart in the return's window is the return), for [`exit_code`].
fn set_stop_reason(reason: u32) {
    if let Some(s) = STOP.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        s.reason = reason;
    }
}

/// The process's exit code for the stop asked (oneos/interface.md 5).
fn exit_code() -> i32 {
    let s = stop_request().unwrap_or_default();
    if s.reason == XH_HALT_RESTART {
        EXIT_RESTART
    } else if s.reason == XH_HALT_PANIC || s.code != 0 {
        EXIT_PANIC
    } else {
        EXIT_GUEST_OFF
    }
}

// ---- the stop watch -------------------------------------------------------------------------------------------------
// fix-debugger-crosscall-panic (main.c, 2026-11-23). Two phone boots of v0.1.16 froze for good after a guest panic: the
// panicking vCPU called halt (stop) and waited in pause(), but the main loop never saw the stop, because it was blocked
// in a device's lock (the virtio report, while a vCPU waited in the encoder's open with that device held). The session
// stayed up with six cores spinning until the user's chord. The stop watch is a thread that waits for the stop: when
// the main loop has not left its loop STOP_SEE_MS after it, the stop path runs from another thread (end_session, which
// only one thread runs); and when the stop path has not ended STOP_PATH_MS after the stop, the process ends with its
// exit code all the same. Step names what the main thread does, for these lines. The watch sleeps in a futex until
// the stop and then in a condvar: nothing on the trap path or in the main loop polls for it.

/// What the main thread does now, for the stop watch's lines (C's `main_step`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Step {
    Boot,
    SignalWait,
    PerfReport,
    DiskReport,
    MemoryReport,
    SwapWatch,
    RamValve,
    HugeReport,
    UsbWatch,
    VirtioReport,
    TasksReport,
    HapticsReport,
    ReadyRule,
    StopPath,
    StopReports,
    StopTasksBridges,
    StopSync,
}

impl Step {
    const ALL: [Step; 17] = [
        Step::Boot,
        Step::SignalWait,
        Step::PerfReport,
        Step::DiskReport,
        Step::MemoryReport,
        Step::SwapWatch,
        Step::RamValve,
        Step::HugeReport,
        Step::UsbWatch,
        Step::VirtioReport,
        Step::TasksReport,
        Step::HapticsReport,
        Step::ReadyRule,
        Step::StopPath,
        Step::StopReports,
        Step::StopTasksBridges,
        Step::StopSync,
    ];

    fn text(self) -> &'static str {
        match self {
            Step::Boot => "the boot",
            Step::SignalWait => "the wait for a signal",
            Step::PerfReport => "the perf report",
            Step::DiskReport => "the disk report",
            Step::MemoryReport => "the memory report",
            Step::SwapWatch => "the swap watch",
            Step::RamValve => "the RAM valve",
            Step::HugeReport => "the 2 MiB page report",
            Step::UsbWatch => "the USB watch",
            Step::VirtioReport => "the virtio report",
            Step::TasksReport => "the tasks report",
            Step::HapticsReport => "the haptics report",
            Step::ReadyRule => "the ready rule",
            Step::StopPath => "the stop path",
            Step::StopReports => "the stop path: the reports",
            Step::StopTasksBridges => "the stop path: the tasks and the bridges",
            Step::StopSync => "the stop path: sync",
        }
    }
}

static MAIN_STEP: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(Step::Boot as u8);

/// The main thread (or the stop path) is at `s` now: one relaxed store.
fn step(s: Step) {
    MAIN_STEP.store(s as u8, Ordering::Relaxed);
}

fn main_step() -> &'static str {
    Step::ALL.get(MAIN_STEP.load(Ordering::Relaxed) as usize).map_or("?", |s| s.text())
}

mod watch {
    use super::*;
    use std::sync::Condvar;
    use std::time::{Duration, Instant};

    pub(super) const STOP_SEE_MS: u64 = 3000;
    pub(super) const STOP_PATH_MS: u64 = 60_000;

    /// 1 once a stop was asked with the watch on: the futex word the watch sleeps on.
    static POSTED: AtomicU32 = AtomicU32::new(0);
    static ON: AtomicBool = AtomicBool::new(false);
    /// The main loop left: the stop path runs on the main thread.
    static LOOP_LEFT: Mutex<bool> = Mutex::new(false);
    static LEFT: Condvar = Condvar::new();

    fn futex(op: i32, value: u32) {
        // SAFETY: a futex wait or wake on a static word; a wait without a timeout returns on a wake or EINTR.
        unsafe {
            libc::syscall(libc::SYS_futex, POSTED.as_ptr(), op, value, std::ptr::null::<libc::timespec>());
        }
    }

    /// The first stop: wakes the watch (one futex wake; nothing before the watch is on).
    pub(super) fn post() {
        if ON.load(Ordering::Acquire) {
            POSTED.store(1, Ordering::Release);
            futex(libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG, 1);
        }
    }

    /// The main loop left its loop: the watch does not run the stop path.
    pub(super) fn loop_leave() {
        *xh_core::thread::lock(&LOOP_LEFT) = true;
        LEFT.notify_all();
    }

    /// Waits on the condvar until `deadline` or until `done` says so; returns the flag.
    fn wait_until(deadline: Instant, done: impl Fn(bool) -> bool) -> bool {
        let mut left = xh_core::thread::lock(&LOOP_LEFT);
        loop {
            let now = Instant::now();
            if done(*left) || now >= deadline {
                return *left;
            }
            left = LEFT.wait_timeout(left, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    fn end_and_exit() -> ! {
        let code = end_session();
        // SAFETY: _exit ends the process at once, as main.c's stop path did (the main thread may be blocked).
        unsafe { libc::_exit(code) }
    }

    fn run() -> ! {
        while POSTED.load(Ordering::Acquire) == 0 {
            futex(libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG, 0);
        }
        let start = Instant::now();
        let left = wait_until(start + Duration::from_millis(STOP_SEE_MS), |left| left);
        if !left {
            loge!(
                "stopping: the main loop did not take the stop ({}) in {} ms: it waits in {}; the stop path runs from the stop watch",
                stop_request().unwrap_or_default().why,
                STOP_SEE_MS,
                main_step()
            );
            cpu::dump_all(); // where each vCPU is (the SIGQUIT lines)
            if xh_core::thread::spawn("xnuhost-end", || end_and_exit()).is_err() {
                end_and_exit();
            }
        }
        // Nothing notifies the condvar again: this is the stop path's limit.
        wait_until(start + Duration::from_millis(STOP_PATH_MS), |_| false);
        let code = exit_code();
        loge!(
            "stopping: the stop path did not end in {} s (it is at: {}): exit {} now",
            STOP_PATH_MS / 1000,
            main_step(),
            code
        );
        // SAFETY: _exit ends the process at once: the stop path is stuck.
        unsafe { libc::_exit(code) }
    }

    /// Started once, before the vCPUs, with every signal blocked (the stop signals are the main thread's).
    pub(super) fn start() {
        // SAFETY: a full set; this thread's mask changed around the spawn and put back after.
        let old = unsafe {
            let mut all: libc::sigset_t = std::mem::zeroed();
            let mut old: libc::sigset_t = std::mem::zeroed();
            libc::sigfillset(&mut all);
            libc::pthread_sigmask(libc::SIG_SETMASK, &all, &mut old);
            old
        };
        let made = xh_core::thread::spawn("xnuhost-stop", || run());
        // SAFETY: the mask saved above.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut()) };
        if let Err(e) = made {
            loge!("stopping: no stop watch: {}", xh_core::sys::error_text(&e));
            return;
        }
        ON.store(true, Ordering::Release);
        if stopping() {
            post(); // a stop asked before the watch was on
        }
    }
}

/// CLOCK_MONOTONIC in whole seconds (the C's tv_sec comparisons of the periodic lines).
fn mono_s() -> u64 {
    monotonic_ms() / 1000
}

// ---- the session files (oneos/interface.md 3) ----------------------------------------------------------------------

/// The session folder, "" for none: set at the start.
static SESSION_DIR: OnceLock<String> = OnceLock::new();
/// The versions line (imageset::versions_line), made once at the start and logged again in the stop summary.
static VERSIONS: OnceLock<String> = OnceLock::new();
/// ready-content-wait, set at the start.
static READY_WAIT: AtomicU32 = AtomicU32::new(READY_CONTENT_WAIT);

/// Writes `<session>/<name>` (0600, replaced whole); nothing without a session folder.
fn session_file(name: &str, content: &str) {
    let Some(dir) = SESSION_DIR.get().filter(|d| !d.is_empty()) else { return };
    let path = format!("{dir}/{name}");
    if path.len() >= 600 {
        return;
    }
    let file = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path);
    match file {
        Ok(mut f) => {
            let _ = f.write_all(content.as_bytes());
        }
        Err(e) => loge!("session: {}: {}", path, xh_core::sys::error_text(&e)),
    }
}

/// A whole file of at most `limit` bytes (None: unreadable, empty or larger). Not xh_core::fs::read_bytes, which cuts
/// a larger file at its limit: a kernel, a device tree or an identity file cut short must be refused.
pub(crate) fn read_file(path: &str, limit: usize) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len == 0 || len > limit as u64 {
        return None;
    }
    let mut data = Vec::with_capacity(len as usize);
    std::io::Read::read_to_end(&mut &f, &mut data).ok()?;
    (data.len() as u64 == len).then_some(data)
}

// ---- the guest is up -----------------------------------------------------------------------------------------------
// session/ready (the switch's dead-man timer stops; the OneOS boot screen hands over to the display helper) and the
// `guest ready: first frame` line, which the fork's boot stages and oneios-tool look for. When, is ready.rs's rule: the
// first frame of the guest's own with content, or ready-content-wait seconds after its first one. The device's thread
// (guest_frame), the main loop's tick and the ready host call all come through here.

static READY: Mutex<ReadyState> = Mutex::new(ReadyState { first_ms: 0, frames: 0, ready: false });
static GUEST_READY: AtomicBool = AtomicBool::new(false);
/// a frame of the guest's own has been shown: the console's copy stops
static GUEST_DRAWING: AtomicBool = AtomicBool::new(false);

fn ready_content_wait() -> u32 {
    READY_WAIT.load(Ordering::Relaxed)
}

/// Whether the guest is up (session/ready written).
pub fn is_guest_ready() -> bool {
    GUEST_READY.load(Ordering::Acquire)
}

/// Whether a frame of the guest's own was shown.
pub fn guest_drawing() -> bool {
    GUEST_DRAWING.load(Ordering::Acquire)
}

fn ready_now(rule: &ReadyState, why: ReadyWhy, now: u64) {
    if !why.now() || GUEST_READY.swap(true, Ordering::AcqRel) {
        return;
    }
    session_file("ready", "ready\n");
    let after = now.saturating_sub(rule.first_ms) as f64 / 1000.0;
    let frames = rule.frames;
    match why {
        ReadyWhy::Content if frames <= 1 => logi!("guest ready: first frame (it has content)"),
        ReadyWhy::Content => logi!(
            "guest ready: first frame with content, {:.1} s and {} frames after the guest's first (black) frame",
            after,
            frames - 1
        ),
        ReadyWhy::BlackTimeout => logi!(
            "guest ready: first frame, all black: no frame with content in {} s after the guest's first ({} frames); ready all the same (ready-content-wait)",
            ready_content_wait(),
            frames
        ),
        ReadyWhy::AnyFrame => logi!("guest ready: first frame (ready-content-wait=0: content not waited for)"),
        _ => logi!("guest ready: first frame (the kernel's ready call)"),
    }
}

/// xh_guest_ready: the kernel's ready host call.
pub fn guest_ready() {
    let mut rule = READY.lock().unwrap_or_else(|e| e.into_inner());
    let why = rule.on_call();
    ready_now(&rule, why, monotonic_ms());
}

/// xh_guest_frame: a frame of the guest's own was shown (virtio_gpu calls it before the frame goes to the display
/// helper, so that session/ready is there first); `lit`: pixels_lit of the display's frame. True once the guest is
/// ready, after which it need not be called again.
pub fn guest_frame(lit: bool) -> bool {
    let now = monotonic_ms();
    let wait = ready_content_wait();
    let mut rule = READY.lock().unwrap_or_else(|e| e.into_inner());
    let first = rule.frames == 0 && !rule.ready;
    let why = rule.on_frame(lit, wait, now);
    if first {
        GUEST_DRAWING.store(true, Ordering::Release);
        // The marker of the rule before 2026-11-15 (ready at this frame), kept for the timings.
        if !lit && why == ReadyWhy::Not {
            logi!(
                "display: the guest's first frame of its own: all black; session/ready waits for one with content (up to {} s)",
                wait
            );
        }
    }
    ready_now(&rule, why, now);
    rule.ready
}

/// The main loop's tick of the ready rule: session/ready for an iOS that only draws black.
fn ready_tick() {
    if is_guest_ready() {
        return;
    }
    let now = monotonic_ms();
    let mut rule = READY.lock().unwrap_or_else(|e| e.into_inner());
    let why = rule.on_tick(ready_content_wait(), now);
    ready_now(&rule, why, now);
}

// ---- the session ---------------------------------------------------------------------------------------------------

/// A setup step failed; its line is logged.
#[derive(Debug)]
pub struct BootFailed;

/// What the session holds from the boot to the stop.
#[derive(Default)]
struct Session {
    image_set: Option<ImageSet>,
    disk_identity: Vec<DiskIdentity>,
    seed_sha256: String,
    disks: Vec<Arc<Blk>>,
    /// the guest's display mode: width, height, refresh rate
    fb: (u32, u32, u32),
    boot_fb_pa: u64,
    /// the host's pen was given to the guest (its IC is woken with the touch IC)
    pen_ic: bool,
    /// the sensor hub (sensors=auto|ssc found it): streamed by the bridges, closed with them
    sensors: Option<std::sync::Arc<onebridges::sensors::Hub>>,
    memory: memory::MemoryWatch,
    bridges: bridges::Bridges,
    usb: UsbWatch,
    usb_source: Option<Box<dyn PropSource + Send>>,
    usb_last: Option<u64>,
    /// the console's last B,G,R,X frame, for a half-float display
    console_seen: Vec<u8>,
}

impl Session {
    /// Every usb-log seconds (3): the phone's USB gadget, logged at the start and whenever it changed (usb.rs). From
    /// the main thread, which is not a realtime one: three property serials and one named sysfs file a poll. Read only.
    fn usb_tick(&mut self, config: &Config, now: bool, when: &str) {
        if config.usb_log == 0 {
            return;
        }
        let t = mono_s();
        if !now && self.usb_last.is_some_and(|last| t - last < config.usb_log as u64) {
            return;
        }
        self.usb_last = Some(t);
        let src = self.usb_source.as_deref_mut().map(|s| s as &mut dyn PropSource);
        if self.usb.poll(src) || now {
            logi!("{}{}", self.usb.line, when);
        }
    }

    /// Until the guest's display driver presents a frame of its own, show what its kernel console draws (not until
    /// ready: between its first frame and ready the frames are the guest's, and the console must not be copied over
    /// them).
    fn console_tick(&mut self) {
        use xh_core::pixels::{bgrx8_to_rgba16f, BGRX8, RGBA16F};
        if is_guest_ready() || guest_drawing() {
            return;
        }
        let (w, h) = (self.fb.0 as usize, self.fb.1 as usize);
        let Some(d) = display::get() else { return };
        let Some(fb) = mem::ram(self.boot_fb_pa, (w * h * 4) as u64) else { return };
        // SAFETY: the boot framebuffer is guest RAM of w * h * 4 bytes (mem::ram); the kernel's console writes it while
        // this reads it, which can only tear the copy (the next tick copies it again).
        let fb = unsafe { std::slice::from_raw_parts(fb.as_ptr(), w * h * 4) };
        // SAFETY: until the guest shows a frame of its own (checked above) the console's copy is the frame's only
        // writer: the GPU device writes it only for the guest's frames.
        let pixels = unsafe { d.bytes_mut() };
        let stride = d.stride as usize;
        if d.format == BGRX8 {
            let n = (stride * d.height as usize).min(fb.len()).min(pixels.len());
            if pixels[..n] != fb[..n] {
                pixels[..n].copy_from_slice(&fb[..n]);
                display::flush(0, 0, d.width, d.height);
            }
        } else if d.format == RGBA16F && d.width as usize == w && d.height as usize == h {
            // A half-float frame (display-format=rgba16f): the console's B,G,R,X bytes are compared with the last ones
            // seen and decoded to linear light when they changed.
            if self.console_seen.len() != fb.len() {
                self.console_seen = vec![0; fb.len()];
            }
            if self.console_seen != fb {
                self.console_seen.copy_from_slice(fb);
                let len = pixels.len();
                for y in 0..h {
                    let row = &mut pixels[(y * stride).min(len)..(y * stride + w * 8).min(len)];
                    bgrx8_to_rgba16f(row, &self.console_seen[y * w * 4..(y + 1) * w * 4]);
                }
                display::flush(0, 0, d.width, d.height);
            }
        }
    }
}

/// A guest restart (PSCI SYSTEM_RESET, XH_HALT_RESTART: iOS restarting itself, or the kernel starting the Data volume
/// over for a new seed) is not the end of the session: for the OneOS fork xnuhost exiting means "back to One UI".
/// xnuhost starts itself again in place, same pid, cgroup and namespace: exec ends every vCPU thread, the tasks went
/// with task::shutdown, and every descriptor but the log is closed (the RAM memfd would otherwise stay alive).
/// XNUHOST_RESTARTS counts them: a guest that restarts 8 times in a row without ever getting ready is left, with exit
/// 3. Returns only when it could not start again.
fn guest_restart(argv: &[String]) {
    use std::os::unix::process::CommandExt;
    let counted = std::env::var("XNUHOST_RESTARTS").ok();
    let mut n = counted.map_or(0, |c| xh_core::parse::leading_i64(&c, 10).map_or(0, |(v, _)| v));
    if is_guest_ready() {
        n = 0;
    }
    if n >= 8 {
        loge!("the guest restarted {} times without getting ready: not starting it again", n);
        return;
    }
    std::env::set_var("XNUHOST_RESTARTS", (n + 1).to_string());
    logi!("the guest asked for a restart: starting again ({})", n + 1);
    let log = xh_core::log::fd();
    for fd in (3..4096).filter(|&fd| fd != log) {
        // SAFETY: closing descriptors before an exec of this program; nothing of ours uses them past this point.
        unsafe { libc::close(fd) };
    }
    // XNUHOST_RUNNER: the emulator xnuhost itself runs under ("qemu-aarch64 -cpu cortex-a710"), which a PC with no
    // binfmt entry needs in front of the program; unset on the phone.
    let runner = std::env::var("XNUHOST_RUNNER").unwrap_or_default();
    let words: Vec<&str> = runner.split(' ').filter(|w| !w.is_empty()).take(8).collect();
    if !words.is_empty() && words.len() + argv.len() < 63 {
        let _ = std::process::Command::new(words[0]).args(&words[1..]).args(argv).exec();
    }
    let e = std::process::Command::new("/proc/self/exe").arg0(&argv[0]).args(&argv[1..]).exec();
    loge!("restart: exec /proc/self/exe: {}", xh_core::sys::error_text(&e));
}

/// The return signals and SIGQUIT.
fn stop_signals() -> libc::sigset_t {
    // SAFETY: sigemptyset/sigaddset on a local set.
    unsafe {
        let mut s: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut s);
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
            libc::sigaddset(&mut s, sig);
        }
        s
    }
}

/// The switches of docs/switches.md that act outside the config: set once, before the devices and the bridges start.
fn set_switches(config: &Config) {
    use crate::config::{Encoder10, GpuDown, WifiJoinedScan};
    use onebridges::switches::{JoinedScan, Wifi};
    onebridges::switches::set_wifi(Wifi {
        ring_wait: config.wifi_ring_wait,
        send_wait: config.wifi_send_wait,
        buffers: config.wifi_buffers,
        joined_scan: match config.wifi_joined_scan {
            WifiJoinedScan::Cache => JoinedScan::Cache,
            WifiJoinedScan::Low => JoinedScan::Low,
            WifiJoinedScan::Plain => JoinedScan::Plain,
        },
    });
    let ten = match config.encoder_10bit {
        Encoder10::Main10 => oneencoder::switches::TenBit::Main10,
        Encoder10::EightBit => oneencoder::switches::TenBit::EightBit,
        Encoder10::Nv12 => oneencoder::switches::TenBit::Nv12,
    };
    oneencoder::switches::set(ten, config.encoder_warmup);
    oneencoder::switches::set_decoder(config.decoder == crate::config::Decoder::Auto, config.decoder_codecs);
    crate::virtio_gpu::set_blob_entries(config.gpu_blob_entries);
    crate::cpu::UCLAMP_ALL.store(config.uclamp_rule == crate::config::UclampRule::All, Ordering::Relaxed);
    crate::cpu::TIMER_COALESCE.store(config.timer_coalesce, Ordering::Relaxed);
    crate::gpu3d_kgsl::DOWN_AT_BOTTOM.store(config.gpu_down == GpuDown::Bottom, Ordering::Relaxed);
    crate::gpu3d_kgsl::UI_RB.store(config.gpu_kgsl_ui_rb, Ordering::Relaxed);
    crate::gpu3d_kgsl::OFF_LEVEL.store(config.gpu_off_level, Ordering::Relaxed);
}

fn hostcalls_vector() -> u64 {
    crate::hostcalls::vector().map_or(0, |v| v as *const _ as u64)
}

fn ro_bind(paths: &[String]) -> i32 {
    if paths.is_empty() {
        eprintln!("xnuhost: --ro-bind requires at least one path");
        return 2;
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        for p in paths {
            let Ok(cpath) = std::ffi::CString::new(p.as_str()) else {
                eprintln!("xnuhost: --ro-bind: NUL in path '{p}'");
                return 2;
            };
            // SAFETY: cpath is a valid NUL-terminated C string; fstype and data are NULL for MS_BIND | MS_REMOUNT | MS_RDONLY.
            let rc = unsafe {
                libc::mount(
                    cpath.as_ptr(),
                    cpath.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                    std::ptr::null(),
                )
            };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                eprintln!("xnuhost: --ro-bind {p}: {err}");
                return 1;
            }
        }
        0
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = paths;
        eprintln!("xnuhost: --ro-bind: supported only on Linux/Android");
        2
    }
}

/// The program (main.c's main): the exit code.
pub fn main() -> i32 {
    let argv: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    // --prop-wait: the OneOS stop loop's wait on Android's properties (propwait.rs), before anything of a session.
    if argv.get(1).map(String::as_str) == Some("--prop-wait") {
        return crate::propwait::main(&argv[2..]);
    }
    // --ro-bind PATH...: remount existing bind mounts read-only via mount(2) directly (BusyBox 1.36 on KernelSU/S25/S26
    // fails to match single-file bind mounts in /proc/mounts; burugou05-wq/OneiOS e85405f1).
    if argv.get(1).map(String::as_str) == Some("--ro-bind") {
        return ro_bind(&argv[2..]);
    }
    #[cfg(all(target_os = "android", target_arch = "aarch64"))]
    {
        // Before any thread exists: bionic gives each new thread of a program with target SDK >= 31 its own APIA key.
        // The kernel's vCPUs and the tasks need one key set (host.rs, shared_by_threads).
        extern "C" {
            fn android_set_application_target_sdk_version(target: u32);
        }
        // SAFETY: bionic's call, made while the process has one thread.
        unsafe { android_set_application_target_sdk_version(30) };
    }
    let mut config = Config::new();
    if let Err(e) = config.args(&argv) {
        e.print();
        return EXIT_SETUP;
    }
    // --host-info: the host table's answer for this phone, one line on stdout, nothing else (the OneOS switch's preflight
    // asks it before it stops anything: oneos/interface.md 4a).
    if config.host_info {
        return match config.host_info_line() {
            Ok(line) => {
                println!("{line}");
                0
            }
            Err(e) => {
                e.print();
                EXIT_SETUP
            }
        };
    }
    if let Err(e) = config.finish_with(&crate::ramfix::can_lock) {
        e.print();
        return EXIT_SETUP;
    }
    xh_core::log::open(None, config.log_level);
    let _ = SESSION_DIR.set(config.session.clone());
    READY_WAIT.store(config.ready_content_wait, Ordering::Relaxed);
    logi!("xnuhost: session ABI {}, mode {}, pid {}", config.session_abi, config.mode, std::process::id());
    // Which runtime, kernel, image and app made this log (docs/booter.md 2.9b); again in the stop summary.
    let app = std::env::var(crate::imageset::APP_VERSION_ENV).ok();
    logi!(
        "{}",
        VERSIONS.get_or_init(|| crate::imageset::versions_line(&config.runtime, &config.image, app.as_deref()))
    );
    // The switches (docs/switches.md): one line with each one's value, and each set where it acts, once, before any
    // device or bridge starts.
    logi!("{}", config.switches_line());
    set_switches(&config);
    // virtio's health lines (docs/booter.md 2.9b)
    xh_core::virtio::set_log(true);
    // who holds a device, in its "held by" line
    xh_core::hooks::install_cpu(|| cpu::current().map(|v| v.index() as u32));
    // The host phone (onebridges::hw, config.rs): what was found, from which property, and each default it changed.
    // Its environment defaults (the S26 Ultra's ONEIOS_BRIGHT_MAX) are set here, while this process has one thread; a
    // variable the session already set wins.
    if !config.host_line.is_empty() {
        logi!("{}", config.host_line);
    }
    // Only in the opt-in mode: the default session's log has no such line (docs/booter.md 2.9d).
    if let Some(line) = config.takeover_line() {
        logi!("{}", line);
    }
    if let Some(h) = config.host {
        onebridges::hw::host_env_apply(h.0);
    }
    host::host_facts();
    let mut s = Session { usb: UsbWatch::new(""), usb_source: usb::bionic(), ..Session::default() };
    if config.selftest {
        return host::selftest(&config);
    }
    memory::ram_choice_log(&config);
    s.usb_tick(&config, true, "");

    // iOS 27's CoreAudio (audiomxd / mediaplaybackd) and libswift_Concurrency assume Apple's 24 MHz timebase;
    // on a 19.2 MHz Qualcomm counter, audio clock ran at 0.8000x (ratio -0.20) with thousands of HALS_IOThread
    // deadline errors. Pass droidvm-timebase-hz=24000000 and arm the stub's SIGSYS fast-path before task::init.
    let host_hz = cpu::counter_hz();
    if config::timebase_boot_arg(&mut config.boot_args, config::BOOT_ARGS_SIZE, host_hz).is_err() {
        loge!("no room in the boot arguments for droidvm-timebase-hz");
        return EXIT_SETUP;
    }
    let (tb_num, tb_den, guest_hz) = config::timebase_scale(&config.boot_args, host_hz);
    cpu::set_timebase_scale(tb_num, tb_den);
    if tb_num != tb_den {
        logi!(
            "timebase: {} Hz host -> {} Hz guest ({}/{}; droidvm-timebase-hz={}, stub fast-path on)",
            host_hz,
            guest_hz,
            tb_num,
            tb_den,
            guest_hz
        );
    }

    // Order matters: the RAM memfd and the task factory come first, while this process is still single-threaded and
    // has nothing of the guest mapped.
    mem::map_limit();
    if mem::create(config.ram_bytes).is_err() {
        return EXIT_SETUP;
    }
    if config.tasks && task::init(&config).is_err() {
        return EXIT_SETUP;
    }

    // Signals: the main thread waits for the stop signals; every thread started from here on inherits them blocked.
    // vCPU threads open the ones they take themselves.
    let mut blocked = stop_signals();
    // SAFETY: the real-time signals the vCPUs use, added to a local set; this thread's mask changed; SIGPIPE ignored (a
    // helper's socket that closed is an error return, not a death).
    unsafe {
        for sig in libc::SIGRTMIN()..=libc::SIGRTMIN() + 3 {
            libc::sigaddset(&mut blocked, sig);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, std::ptr::null_mut());
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    watch::start();
    if !config.session.is_empty() {
        session_file("xnuhost.pid", &format!("{}\n", std::process::id()));
    }
    let Ok(fb) = display::init(&config) else { return EXIT_SETUP };
    s.fb = fb;
    // The display driver's scale, refresh rate and fixed-rate flag go on the kernel's command line (a value given
    // there stays).
    if config::display_boot_args(
        &mut config.boot_args,
        config::BOOT_ARGS_SIZE,
        config.scale,
        s.fb.2,
        config.display_format,
    )
    .is_err()
    {
        loge!("no room in the boot arguments for the display's switches");
        return EXIT_SETUP;
    }
    // 24A446's bluetoothd with the virtual machine hidden takes its chip path and dies (SIGSEGV at 0x350) in a loop
    // that launchd then throttles for 1200 s (phone and PC, 2026-11-14; docs/hardware.md 4): said, not changed. [I] Its
    // chip factory finds no chip for transport-encoding 1 outside a VM (only transport 9 or a product of its table gives
    // one), and 0x350 is a field of the chip object it then reads through a null pointer.
    if config::boot_args_hides_vmm_from(&config.boot_args, "bluetoothd") {
        loge!("boot arguments: owner-hide-vmm names bluetoothd: it crashes in a loop that way (docs/hardware.md 4); take it out");
    }
    // bt-transport-override=N replaces the transport-encoding bluetoothd reads. Since 2026-11-23 the guest's Bluetooth
    // driver gives it 10 (in a VM its power init passes only with 10) and the kernel adds BT_HCI_TRANSPORT=H4:POSIX to
    // its environment (hosted 0087), which keeps its HCI on our transport: so 10 changes nothing, any other N makes its
    // power init fail (PowerState 3, Bluetooth off). Without the kernel's variable (oneios-bt-hci=0, or a kernel before
    // 0087) 10 is BTVC, a simulated controller in bluetoothd's own process (main 3233fed, 2026-11-15: exit(1) every 56 s).
    match config::boot_args_bt_transport(&config.boot_args) {
        Some(10) => logi!("boot arguments: bt-transport-override=10 is what the guest's driver gives bluetoothd already; with oneios-bt-hci=0 (no BT_HCI_TRANSPORT) it is bluetoothd's own simulated controller (docs/hardware.md 4)"),
        Some(n) => loge!("boot arguments: bt-transport-override={}: bluetoothd's power init passes only with 10 in a VM, so Bluetooth stays off (docs/hardware.md 4); take it out", n),
        None => {}
    }
    perf::init(config.perf_log);
    let placed = match s.boot(&mut config) {
        Ok(p) => p,
        Err(BootFailed) => {
            task::shutdown();
            input::stop(); // the touchscreen's grab (touch-exclusive), released here too, not only by the exit
            return EXIT_SETUP;
        }
    };
    let config: &'static Config = CONFIG.get_or_init(|| config);
    let block = cpu::vcpu(0).cpu.as_ptr() as u64;
    if cpu::start(0, placed.entry_va, placed.args_va, hostcalls_vector(), block).is_err() {
        task::shutdown();
        input::stop();
        return EXIT_SETUP;
    }
    let _ = ARGV.set(argv);
    *xh_core::thread::lock(&BRIDGES) = Some(std::mem::take(&mut s.bridges));
    *xh_core::thread::lock(&SESSION) = Some(s);
    run(config)
}

/// The session from the boot's end to the stop: the main loop's, and the stop path's from whichever thread runs it.
/// The main loop holds it only around its own ticks, so a stop path on the stop watch finds it free unless the main
/// thread is blocked in one of those.
static SESSION: Mutex<Option<Session>> = Mutex::new(None);
/// The bridges, apart from the rest of the session: the stop path closes them (the motor, the backlight, the network
/// as One UI had them) even while the main thread is blocked in a memory or USB report or the console's copy.
static BRIDGES: Mutex<Option<bridges::Bridges>> = Mutex::new(None);
/// The return's graceful wait (the main loop's; the stop path reads it).
static RET: Mutex<ReturnState> = Mutex::new(ReturnState { active: false, started_ms: 0, deadline_ms: 0 });
/// The command line, for a guest restart's exec.
static ARGV: OnceLock<Vec<String>> = OnceLock::new();
/// The stop path ran (end_session runs once).
static ENDING: AtomicBool = AtomicBool::new(false);

/// The main loop, then the stop path: the exit code.
fn run(config: &'static Config) -> i32 {
    let waited = stop_signals();
    // The return's graceful shutdown (docs/booter.md "The return"): the first stop signal, when iOS is up and
    // return-graceful-ms > 0, asks iOS to power off and keeps the vCPUs running up to an absolute deadline; then the
    // stop below runs as always. A panic (it sets stop directly, not through a signal), a second signal, the guest
    // halting itself, or the deadline all leave the wait at once. The way back never depends on any of this: the
    // deadline is ours, and the fork's SIGKILL is the backstop under everything.
    while !stopping() {
        let tick = libc::timespec { tv_sec: 0, tv_nsec: 100_000_000 };
        step(Step::SignalWait);
        // SAFETY: waits up to 100 ms for one of the blocked stop signals.
        let signo = unsafe { libc::sigtimedwait(&waited, std::ptr::null_mut(), &tick) };
        if signo == libc::SIGQUIT {
            // debugging: where each vCPU is
            cpu::dump_all();
            task::dump();
            sysreg::stats_log();
            perf::log(true);
            continue;
        }
        // RET is held only for its own update (one statement), never across a bridge's request: the stop path reads it.
        if signo > 0 {
            let decision =
                xh_core::thread::lock(&RET).on_signal(config.return_graceful_ms, is_guest_ready(), monotonic_ms());
            match decision {
                ReturnDecision::Graceful => {
                    let asked = bridges::request_shutdown();
                    logi!(
                        "return: graceful shutdown asked{}; iOS has up to {} ms",
                        if asked { "" } else { " (no bridge; nothing to ask, will time out)" },
                        config.return_graceful_ms
                    );
                }
                ReturnDecision::StopSecond => {
                    logi!("return: second request while shutting down; stopping now");
                    stop(XH_HALT_OFF, 0, "second return request");
                }
                ReturnDecision::StopNow => stop(XH_HALT_OFF, 0, &format!("signal {signo}")),
            }
        }
        let ret = *xh_core::thread::lock(&RET);
        if ret.expired(monotonic_ms()) {
            logi!("return: graceful shutdown timed out after {} ms; stopping", ret.elapsed_ms(monotonic_ms()));
            stop(XH_HALT_OFF, 0, "graceful shutdown timed out");
        }
        step(Step::PerfReport);
        perf::tick(); // "perf:" lines every perf-log seconds
        step(Step::DiskReport);
        blk::tick(); // "disk NAME: 30 s:" lines (blk.rs)
        {
            let mut guard = xh_core::thread::lock(&SESSION);
            let Some(s) = guard.as_mut() else { break };
            step(Step::MemoryReport);
            s.memory.host_memory_tick(config, false); // "memory: host MemAvailable" every host-mem-log seconds
            step(Step::SwapWatch);
            s.memory.swap_watch(config); // "memory: the host swaps iOS's RAM" when it does (nothing while all is locked)
            s.memory.pressure_watch(config); // "memory: the host is short of memory" once per event (every 5 s a look)
            step(Step::RamValve);
            s.memory.ram_valve_tick(); // the lock's valve, once a second while locked
            step(Step::HugeReport);
            s.memory.huge_tick(config); // "memory: 2 MiB pages (the prefault done): ..." once
            step(Step::UsbWatch);
            s.usb_tick(config, false, ""); // "usb: state ..." when the USB gadget changed
        }
        step(Step::VirtioReport);
        xh_core::virtio::tick(); // "virtio: 60 s: ..." and a queue that holds chains (xh_core::virtio)
        step(Step::TasksReport);
        task::tick(); // "tasks: 60 s: ..." made, ended, died on their own (task.rs)
        step(Step::HapticsReport);
        if let Some(b) = xh_core::thread::lock(&BRIDGES).as_ref() {
            b.haptic_tick(); // "haptics: 60 s: ..." when iOS used the motor
        }
        step(Step::ReadyRule);
        ready_tick(); // session/ready for an iOS that only draws black (ready-content-wait)
        if let Some(s) = xh_core::thread::lock(&SESSION).as_mut() {
            s.console_tick();
        }
    }
    step(Step::StopPath);
    watch::loop_leave();
    end_session()
}

/// The stop path, once: from the main thread when its loop sees the stop, or from the stop watch when the loop does
/// not (main.c's end_session). The thread that comes second waits; the first ends the process (the main thread by
/// returning the exit code, the watch's thread with _exit).
fn end_session() -> i32 {
    if ENDING.swap(true, Ordering::AcqRel) {
        loop {
            // SAFETY: waits for a signal; the other thread's stop path ends the process.
            unsafe { libc::pause() };
        }
    }
    let ret = *xh_core::thread::lock(&RET);
    let mut request = stop_request().unwrap_or_default();
    // A return was asked: a guest-initiated restart in the window is iOS finishing its shutdown, not a reboot to re-run
    // in place. Take it as the return (exit 0), so the fork's watcher goes back to One UI instead of xnuhost re-exec'ing.
    if ret.active && request.reason == XH_HALT_RESTART {
        logi!("return: the guest restarted during the graceful shutdown; taking it as the return");
        request.reason = XH_HALT_OFF;
        set_stop_reason(XH_HALT_OFF);
    }
    // The guest shut itself down inside the window (its kernel's PSCI SYSTEM_OFF or halt set the stop): say how long it
    // took. A timeout or a second request logged its own line above; a panic took the panic path (reason PANIC).
    if ret.active
        && request.reason != XH_HALT_PANIC
        && request.why != "graceful shutdown timed out"
        && request.why != "second return request"
    {
        logi!("return: iOS shut down in {} ms", ret.elapsed_ms(monotonic_ms()));
    }
    logi!("stopping: {} (reason {}, code {})", request.why, request.reason, request.code);
    if let Some(v) = VERSIONS.get() {
        logi!("{}", v); // a tail of the log has it too
    }
    step(Step::StopReports);
    let config = config();
    // The session's own lines at the stop; never waited for: a main thread blocked in one of its ticks holds it (the
    // stop watch's line said where), and the rest of the stop path must go on.
    match (SESSION.try_lock(), config) {
        (Ok(mut guard), Some(config)) => {
            if let Some(s) = guard.as_mut() {
                s.memory.host_memory_tick(config, true);
                if mem::state().huge {
                    s.memory.huge_report(" at the stop");
                }
                s.usb_tick(config, true, " (at the stop)");
            }
        }
        (Err(std::sync::TryLockError::WouldBlock), _) => {
            loge!(
                "stopping: the main thread holds the session ({}): its memory and USB lines at the stop are left out",
                main_step()
            )
        }
        _ => {}
    }
    for (i, v) in cpu::vcpus().iter().enumerate().take(cpu::ncpu()) {
        logi!(
            "cpu{}: {}, {} interrupts ({} timer, {} IPI acknowledged, {} re-raised), {} device accesses, {} emulated instructions, {} task traps",
            i,
            if v.started.load(Ordering::Relaxed) { "started" } else { "never started" },
            v.stat.irqs.get(),
            v.stat.timer.get(),
            v.stat.sgi.get(),
            v.stat.reraised.get(),
            v.stat.mmio.get(),
            v.stat.sysreg.get(),
            v.stat.traps.get()
        );
    }
    sysreg::stats_log(); // which trapped instructions kept the vCPUs busy
    perf::log(true); // what the trap path cost, step by step (perf.rs)
    blk::report_all(); // what each disk did, its flushes and their time (blk.rs)
                       // Debugging aid: XNUHOST_DUMP=FILE writes the first 64 MiB of the kernel's window at the stop (XNUHOST_DUMP_MB=N for
                       // more: a kernel collection's data is past 64 MiB).
    if let Ok(path) = std::env::var("XNUHOST_DUMP") {
        let mb = std::env::var("XNUHOST_DUMP_MB").ok().and_then(|t| xh_core::parse::leading_u64(&t, 0).map(|(v, _)| v));
        let m = mem::state();
        if !m.host.is_null() {
            let n = m.size.min(mb.unwrap_or(64) << 20) as usize;
            // SAFETY: the window is mapped for the process's life; the vCPUs are stopped or stopping, a torn byte in a
            // debugging dump is acceptable.
            let bytes = unsafe { std::slice::from_raw_parts(m.host, n) };
            let _ = std::fs::write(path, bytes);
        }
    }
    step(Step::StopTasksBridges);
    oneencoder::backend_helper::stop(); // the encoder helper, also one that hangs in MediaCodec
    task::shutdown();
    if let Some(mut b) = xh_core::thread::lock(&BRIDGES).take() {
        b.close();
    }
    input_script::stop();
    input::stop();
    display::close();
    step(Step::StopSync);
    // SAFETY: sync has no preconditions: the Data store's pages.
    unsafe { libc::sync() };
    // The RAM goes back to the host at once with this process (its last mapping and descriptor: the tasks and the
    // factory are gone already); a kill does the same. Nothing to undo: mlock ends with the mapping.
    if let Some(s) = xh_core::thread::lock(&SESSION).as_ref() {
        s.memory.stop();
    }
    if request.reason == XH_HALT_RETURN {
        session_file("return.request", "the guest asked to return\n");
    }
    if request.reason == XH_HALT_RESTART {
        guest_restart(ARGV.get().map_or(&[][..], |a| a.as_slice())); // returns only if it could not
    }
    let code = exit_code();
    logi!("exit {}", code);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_keeps_the_first() {
        let long = "x".repeat(200);
        stop(XH_HALT_PANIC, 7, &long);
        stop(XH_HALT_OFF, 0, "later");
        let r = stop_request().unwrap();
        assert!(stopping() && r.reason == XH_HALT_PANIC && r.code == 7 && r.why.len() == 95);
    }

    #[test]
    fn the_main_steps_texts() {
        // MAIN_STEP holds a Step as u8: ALL must list each at its own index, with main.c's texts.
        for (i, s) in Step::ALL.iter().enumerate() {
            assert_eq!(*s as usize, i);
        }
        assert_eq!(main_step(), "the boot");
        step(Step::VirtioReport);
        assert_eq!(main_step(), "the virtio report");
        step(Step::StopTasksBridges);
        assert_eq!(main_step(), "the stop path: the tasks and the bridges");
        assert_eq!(watch::STOP_SEE_MS, 3000);
        assert_eq!(watch::STOP_PATH_MS, 60_000);
    }
}
