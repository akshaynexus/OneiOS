// SPDX-License-Identifier: MIT
//! Write-only present buffers (xnuhost's `display-present=ahb`, 2026-11-25). Ported from YumikoHayamato/OneiOS
//! cdb297bbf (runtime/display/src/jni.c, "the present path without the CPU caches' maintenance"; a fork of this
//! repository, CREDITS.md), rewritten here on the run loop's [`Layer`].
//!
//! Why. The window path's buffers are the CPU's cached ones: `ANativeWindow_lock` is Surface::lock, which asks for
//! SW_READ_OFTEN | SW_WRITE_OFTEN, and each lock and each post cleans and invalidates the CPU's caches over the whole
//! buffer, whatever was written. The fork measured it on the S25 (simpleperf, 1080x2340 at 60 frames a second):
//! `dcache_inval_poc` 36%, `dcache_clean_poc` 31%, `arch_sync_dma_for_cpu` and `_for_device` 9% each, the copy itself
//! 10%; with these buffers 114 frames a second at 17.1% of a core against 60 at 49.5%. Our frame is 1440x3120, 1.8
//! times theirs.
//!
//! How. Three AHardwareBuffers of the frame's size that the CPU only writes (`CPU_WRITE_RARELY | CPU_READ_NEVER |
//! GPU_SAMPLED_IMAGE | COMPOSER_OVERLAY`: not cached by the CPU, nothing to maintain), B,G,R,A (else R,G,B,X), on a
//! child of the layer's surface (`ASurfaceControl_createFromWindow`: the layer's matrix and position, which
//! OneiOSDisplay.java sets, place it too), posted with ASurfaceTransaction (`setBuffer` with the unlock's fence,
//! `setDamageRegion`; the first one shown, opaque, with the frame's dataspace and the panel's rate). The run loop's
//! [`super::Buffers`] fills each buffer with what it lacks, told apart by its slot ([`Locked::key`]). At most
//! [`IN_FLIGHT`] transactions are in flight, and a buffer is filled again only after the release fence the compositor
//! gave when it was replaced. The helper never reads the layer's buffers (it fills them from its snapshot), so buffers
//! the CPU cannot read lose nothing. 8-bit frames only: half floats keep the window path.
//!
//! The fallback. Whatever of it is missing (an option not asked, half floats, no such buffers, no child surface, an
//! allocation, a lock or a post refused), the helper says why once and posts through the layer's window, as before.
//! The way back to One UI: the child is hidden and taken off the layer when the run ends ([`Present`]'s drop), and
//! the Java half then removes the layer itself (which takes its children with it); every buffer goes with the process.
use super::run::{Layer, Locked};
use super::{Rect, BUFFER_BGRA_8888, BUFFER_RGBX_8888, FORMAT_BGRX, OPTION_PRESENT_AHB};
use crate::sys;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The buffers of the write-only path.
pub const SLOTS: usize = 3;
/// Transactions in flight at most (the next lock waits for a completion, up to [`IN_FLIGHT_WAIT`]).
pub const IN_FLIGHT: usize = 2;
pub const IN_FLIGHT_WAIT: Duration = Duration::from_secs(1);
/// The longest wait for a buffer's release fence.
pub const RELEASE_WAIT_MS: i32 = 200;
/// The environment's word (the helper's own, for a test by hand; it wins over xnuhost's option): `ahb` or `window`.
pub const ENV: &str = "ONEIOS_DISPLAY_PRESENT";

/// Called once when a post's transaction is complete, with the release fence of the buffer that post replaced (None:
/// none, or nothing was replaced).
pub type Done = Box<dyn FnOnce(Option<OwnedFd>) + Send>;

/// The NDK's half (jni.rs: AHardwareBuffer and ASurfaceTransaction on the layer's window; the host tests: a fake).
/// Only the poster's thread calls it; `done` may run on any thread.
pub trait Backend: Send {
    /// AHardwareBuffer_isSupported for a write-only buffer of this size and format.
    fn supported(&mut self, width: u32, height: u32, format: i32) -> bool;
    /// The child surface of the layer. Err: why not.
    fn open_child(&mut self) -> Result<(), String>;
    /// The buffer of `slot`; its stride in pixels. Err: the NDK's number.
    fn allocate(&mut self, slot: usize, width: u32, height: u32, format: i32) -> Result<u32, i32>;
    /// The buffer of `slot` mapped for writing. Err: the NDK's number.
    fn lock(&mut self, slot: usize) -> Result<*mut u8, i32>;
    /// Unmapped: the fence the writes are done at (None: done). Err: the NDK's number.
    fn unlock(&mut self, slot: usize) -> Result<Option<OwnedFd>, i32>;
    /// Posts `slot`'s buffer on the child with its acquire fence and the damage; `first`: Some(dataspace) for the first
    /// post (shown, opaque, the dataspace, the frame rate). `done` is called when the transaction completes. Err: the
    /// NDK's number (done is then never called).
    fn post(
        &mut self,
        slot: usize,
        acquire: Option<OwnedFd>,
        damage: Rect,
        first: Option<i32>,
        done: Done,
    ) -> Result<(), i32>;
    /// The end: the child hidden and taken off the layer, the buffers and the child let go.
    fn close(&mut self);
}

/// Which path: xnuhost's option (bit OPTION_PRESENT_AHB), unless the environment says (`ahb` or `window`).
pub fn wanted(options: u32, env: Option<&str>) -> bool {
    match env {
        Some("ahb") => true,
        Some("window") => false,
        _ => options & OPTION_PRESENT_AHB != 0,
    }
}

/// What the completions change, shared with them.
#[derive(Default)]
struct Flight {
    inflight: usize,
    release: [Option<OwnedFd>; SLOTS],
    /// Completions that came (the end's log).
    completed: u64,
}

#[derive(Default)]
struct Flights {
    state: Mutex<Flight>,
    changed: Condvar,
}

impl Flights {
    fn lock(&self) -> MutexGuard<'_, Flight> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

enum Mode {
    /// Not chosen yet (the first lock chooses, once xnuhost's options are read).
    Open,
    Window,
    Ahb,
}

struct Inner<B: Backend> {
    backend: B,
    mode: Mode,
    width: u32,
    height: u32,
    /// The guest's frame format (only FORMAT_BGRX goes the write-only way).
    frame_format: u32,
    /// The buffer format the run loop asked for (set_geometry), and the one the buffers have.
    format: i32,
    layout: i32,
    dataspace: i32,
    stride: [u32; SLOTS],
    shown: Option<usize>,
    locked: Option<usize>,
    posts: u64,
    /// The waits of the locks: for a transaction in flight, for a release fence (the end's log).
    flight_waits: u64,
    late: u64,
    fence_waits: u64,
}

/// The run loop's layer: the write-only buffers when asked for and to be had, else the window `W`.
pub struct Present<W: Layer, B: Backend> {
    window: W,
    inner: Mutex<Inner<B>>,
    flights: Arc<Flights>,
    env: Option<String>,
}

impl<W: Layer, B: Backend> Present<W, B> {
    /// `env`: the helper's ONEIOS_DISPLAY_PRESENT, if set. `frame_format`: the guest frame's.
    pub fn new(window: W, backend: B, frame_format: u32, env: Option<String>) -> Present<W, B> {
        Present {
            window,
            inner: Mutex::new(Inner {
                backend,
                mode: Mode::Open,
                width: 0,
                height: 0,
                frame_format,
                format: BUFFER_BGRA_8888,
                layout: BUFFER_BGRA_8888,
                dataspace: 0,
                stride: [0; SLOTS],
                shown: None,
                locked: None,
                posts: 0,
                flight_waits: 0,
                late: 0,
                fence_waits: 0,
            }),
            flights: Arc::new(Flights::default()),
            env,
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner<B>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the write-only buffers are in use.
    pub fn is_ahb(&self) -> bool {
        matches!(self.inner().mode, Mode::Ahb)
    }

    /// The buffers and the child, or why not (the window path then). Said once either way.
    fn open(&self, i: &mut Inner<B>, options: u32) {
        if !wanted(options, self.env.as_deref()) {
            i.mode = Mode::Window;
            eprintln!(
                "oneios-display: display-present=window: the frames go through the layer's window (its buffers locked \
                 and posted)"
            );
            return;
        }
        let refuse = |i: &mut Inner<B>, why: String| {
            i.backend.close();
            i.mode = Mode::Window;
            eprintln!("oneios-display: display-present=ahb: {why}: the window path");
        };
        if i.frame_format != FORMAT_BGRX || (i.format != BUFFER_BGRA_8888 && i.format != BUFFER_RGBX_8888) {
            return refuse(i, format!("the frame is not 8-bit (buffer format {})", i.format));
        }
        let (w, h) = (i.width, i.height);
        let layout = if i.backend.supported(w, h, i.format) {
            i.format
        } else if i.format != BUFFER_RGBX_8888 && i.backend.supported(w, h, BUFFER_RGBX_8888) {
            BUFFER_RGBX_8888
        } else {
            return refuse(i, format!("no write-only buffers of {w}x{h} here (AHardwareBuffer_isSupported)"));
        };
        if let Err(why) = i.backend.open_child() {
            return refuse(i, format!("no child surface of the layer ({why})"));
        }
        for slot in 0..SLOTS {
            match i.backend.allocate(slot, w, h, layout) {
                Ok(stride) if stride >= w => i.stride[slot] = stride,
                Ok(stride) => return refuse(i, format!("buffer {slot} has a stride of {stride} pixels for {w}")),
                Err(e) => {
                    return refuse(
                        i,
                        format!("buffer {slot} of {w}x{h} could not be made (AHardwareBuffer_allocate {e})"),
                    )
                }
            }
        }
        i.layout = layout;
        i.mode = Mode::Ahb;
        eprintln!(
            "oneios-display: display-present=ahb: {SLOTS} write-only buffers ({w}x{h} {}, CPU_WRITE_RARELY: no CPU cache to \
             maintain) on a child surface of the layer; at most {IN_FLIGHT} posts in flight",
            super::run::format_name(layout)
        );
    }

    /// The write-only path given up for the window's (said once).
    fn give_up(&self, i: &mut Inner<B>, why: String) {
        eprintln!("oneios-display: display-present=ahb: {why}: the window path from now on");
        self.end_ahb(i);
        i.mode = Mode::Window;
        // the window's buffers say the dataspace (it was kept for the child's first post)
        self.window.set_dataspace(i.dataspace);
    }

    /// Waits (a little) for what is in flight, then hides the child and lets everything go.
    fn end_ahb(&self, i: &mut Inner<B>) {
        let deadline = Instant::now() + IN_FLIGHT_WAIT;
        let mut f = self.flights.lock();
        while f.inflight > 0 {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            f = self.flights.changed.wait_timeout(f, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
        let (left, completed) = (f.inflight, f.completed);
        for fence in f.release.iter_mut() {
            *fence = None;
        }
        drop(f);
        i.backend.close();
        eprintln!(
            "oneios-display: display-present=ahb: {} posts, {completed} completed{}; locks that waited for a post in \
             flight {} ({} past {} ms), for a release fence {}",
            i.posts,
            if left > 0 { format!(", {left} still in flight at the end") } else { String::new() },
            i.flight_waits,
            i.late,
            IN_FLIGHT_WAIT.as_millis(),
            i.fence_waits
        );
    }

    fn lock_ahb(&self, i: &mut Inner<B>) -> Result<Locked, i32> {
        // At most IN_FLIGHT transactions: the compositor keeps up, or the next frame waits for it (up to a second).
        let deadline = Instant::now() + IN_FLIGHT_WAIT;
        let mut f = self.flights.lock();
        if f.inflight >= IN_FLIGHT {
            i.flight_waits += 1;
        }
        while f.inflight >= IN_FLIGHT {
            let now = Instant::now();
            if now >= deadline {
                i.late += 1;
                break;
            }
            f = self.flights.changed.wait_timeout(f, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
        let slot = i.shown.map_or(0, |s| (s + 1) % SLOTS);
        let fence = f.release[slot].take();
        drop(f);
        // The compositor is done with this buffer once its release fence signals.
        if let Some(fence) = fence {
            if !matches!(sys::poll(&[(fence.as_fd(), libc::POLLIN)], 0, false), Ok((1, _))) {
                i.fence_waits += 1;
                let _ = sys::poll(&[(fence.as_fd(), libc::POLLIN)], RELEASE_WAIT_MS, true);
            }
        }
        let bits = i.backend.lock(slot)?;
        if bits.is_null() {
            return Err(-libc::EFAULT);
        }
        i.locked = Some(slot);
        Ok(Locked {
            width: i.width as i32,
            height: i.height as i32,
            stride: i.stride[slot] as i32,
            format: i.layout,
            bits,
            key: slot + 1,
        })
    }

    fn post_ahb(&self, i: &mut Inner<B>, changed: &Rect) -> Result<(), i32> {
        let Some(slot) = i.locked.take() else { return Ok(()) };
        let acquire = i.backend.unlock(slot)?;
        let first = i.shown.is_none();
        let mut damage = if first { Rect::whole(i.width, i.height) } else { *changed };
        if !damage.clip(i.width, i.height) {
            damage = Rect::whole(i.width, i.height);
        }
        let replaced = i.shown;
        let flights = self.flights.clone();
        let done: Done = Box::new(move |fence| {
            let mut f = flights.lock();
            if let Some(r) = replaced {
                f.release[r] = fence;
            }
            f.inflight = f.inflight.saturating_sub(1);
            f.completed += 1;
            drop(f);
            flights.changed.notify_all();
        });
        self.flights.lock().inflight += 1;
        if let Err(e) = i.backend.post(slot, acquire, damage, first.then_some(i.dataspace), done) {
            let mut f = self.flights.lock();
            f.inflight = f.inflight.saturating_sub(1);
            return Err(e);
        }
        i.shown = Some(slot);
        i.posts += 1;
        Ok(())
    }
}

impl<W: Layer, B: Backend> Layer for Present<W, B> {
    fn set_geometry(&self, width: u32, height: u32, format: i32) {
        {
            let mut i = self.inner();
            i.width = width;
            i.height = height;
            i.format = format;
        }
        self.window.set_geometry(width, height, format)
    }

    fn set_dataspace(&self, dataspace: i32) -> i32 {
        let ahb = {
            let mut i = self.inner();
            i.dataspace = dataspace;
            matches!(i.mode, Mode::Ahb)
        };
        // The window's too while it may still be the path (the child's first post says it to the compositor).
        if ahb {
            0
        } else {
            self.window.set_dataspace(dataspace)
        }
    }

    fn dataspace(&self) -> i32 {
        let i = self.inner();
        if matches!(i.mode, Mode::Ahb) {
            return i.dataspace;
        }
        drop(i);
        self.window.dataspace()
    }

    fn choose(&self, options: u32) {
        let mut i = self.inner();
        if matches!(i.mode, Mode::Open) {
            self.open(&mut i, options);
        }
    }

    fn lock(&self, width: u32, height: u32) -> Result<Locked, i32> {
        let mut i = self.inner();
        if matches!(i.mode, Mode::Open) {
            self.open(&mut i, 0); // a run loop that never said its options: xnuhost's default is not known
        }
        if matches!(i.mode, Mode::Ahb) {
            match self.lock_ahb(&mut i) {
                Ok(b) => return Ok(b),
                Err(e) => self.give_up(&mut i, format!("a buffer could not be locked ({e})")),
            }
        }
        drop(i);
        self.window.lock(width, height)
    }

    fn unlock_and_post(&self, changed: &Rect) {
        let mut i = self.inner();
        if i.locked.is_some() {
            if let Err(e) = self.post_ahb(&mut i, changed) {
                // this frame is not shown; the next one goes through the window, filled whole (new buffers)
                self.give_up(&mut i, format!("a buffer could not be posted ({e})"));
            }
            return;
        }
        drop(i);
        self.window.unlock_and_post(changed)
    }
}

impl<W: Layer, B: Backend> Drop for Present<W, B> {
    fn drop(&mut self) {
        let mut i = self.inner();
        if matches!(i.mode, Mode::Ahb) {
            self.end_ahb(&mut i);
            i.mode = Mode::Window;
        }
    }
}
