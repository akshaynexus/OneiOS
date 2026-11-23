// SPDX-License-Identifier: MIT
//! The helper's run loop, portable (jni.rs gives it the layer's window; the host tests a stand-in layer): frames from
//! xnuhost's socket into the layer until the socket closes or a signal asks to stop.
//!
//! Two threads since 2026-11-24 (fix/frame-path). **The reader** (a thread of its own) takes xnuhost's messages, reads
//! each direct frame from guest RAM into the snapshot in one pass, and answers `done` at once; xnuhost then answers the
//! guest's flush, and the guest's display driver posts its next frame only after that answer
//! (DroidVMVirtioGpuDisplay.cpp, land()). **The poster** (the caller's thread, the one the Java half runs nRun on)
//! waits for a change, locks a buffer of the layer (ANativeWindow_lock: to the panel's vsync when the queue is full),
//! fills from the snapshot what that buffer lacks and posts it. Until then both were one loop: a frame that came while
//! the loop waited for a buffer or filled one was read only after the post, so the answer started late ("from 5.5 ms
//! after xnuhost sent the frame", buffer wait mean 3.6 ms; the phone's helper line of 2026-11-23 19:06), and the guest's
//! next frame waited for the panel's vsync on top of the read. Now the read starts when the frame comes, and the poster
//! posts the newest picture the snapshot holds: frames that come while it waits for a buffer are one post.
//!
//! The snapshot is under one lock: the reader holds it while it reads a frame into it, the poster while it fills a
//! buffer from it, so a post is always one whole frame as read (never half of the next). The reader waits for a fill at
//! most (about 1 ms on the phone), never for a buffer.
use super::{
    Buffers, Direct, Frame, Rect, Snapshot, BUFFER_BGRA_8888, BUFFER_RGBA_FP16, BUFFER_RGBX_8888, DATASPACE_SCRGB,
    DATASPACE_SCRGB_LINEAR, DATASPACE_SRGB, DONE_FAILED, DONE_UNCHANGED, FORMAT_BGRX, OPTION_SERIAL_POST,
};
use crate::sys::{self, Errno};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, MutexGuard};

/// A locked buffer of the layer: `stride` pixels a row, `format` the buffer's (AHARDWAREBUFFER_FORMAT_*).
pub struct Locked {
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: i32,
    pub bits: *mut u8,
}

/// What the run loop needs of the layer (jni.rs: the NDK's ANativeWindow on the layer's Surface). Only the poster's
/// thread (the caller's) uses it.
pub trait Layer {
    fn set_geometry(&self, width: u32, height: u32, format: i32);
    /// 0, or the NDK's error.
    fn set_dataspace(&self, dataspace: i32) -> i32;
    fn dataspace(&self) -> i32;
    /// Locks a buffer whole (the rectangle asked for: all of it). The error is the NDK's (negative) number.
    fn lock(&self, width: u32, height: u32) -> Result<Locked, i32>;
    fn unlock_and_post(&self);
}

/// How the run loop ended (nRun's answer to the Java half).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Closed = 0,
    Stopped = 1,
    LockFailed = -2,
    PollFailed = -3,
}

/// printf's %#x: 0 is "0", anything else 0x and its hex digits.
pub fn alt_hex(v: u32) -> String {
    if v == 0 {
        "0".into()
    } else {
        format!("{v:#x}")
    }
}

pub fn format_name(format: i32) -> &'static str {
    match format {
        BUFFER_BGRA_8888 => "BGRA_8888",
        BUFFER_RGBX_8888 => "RGBX_8888",
        BUFFER_RGBA_FP16 => "RGBA_FP16",
        _ => "other",
    }
}

/// Tags the layer's buffers with the dataspace: what the numbers in them mean, so that surfaceflinger turns them into
/// the panel's colours (One UI's colour mode decides how; nothing of it is changed here). Logs what the layer has.
pub fn set_dataspace(layer: &dyn Layer, dataspace: i32) {
    let error = layer.set_dataspace(dataspace);
    let now = layer.dataspace();
    if error != 0 {
        eprintln!(
            "oneios-display: the dataspace {} ({}) was refused ({error}); the buffers say {} ({})",
            super::dataspace_name(dataspace),
            alt_hex(dataspace as u32),
            super::dataspace_name(now),
            alt_hex(now as u32)
        );
    } else {
        eprintln!("oneios-display: buffers tagged {} (dataspace {})", super::dataspace_name(now), alt_hex(now as u32));
    }
}

/// The 10 s statistics, both threads'.
#[derive(Default)]
struct Stats {
    posted: u64,
    direct: u64,
    unchanged: u64,
    reads: u64,
    delays: u64,
    lock_us: i64,
    copy_us: i64,
    copy_max: i64,
    rows_changed: i64,
    rows_filled: i64,
    read_us: i64,
    read_max: i64,
    delay_us: i64,
    delay_max: i64,
    /// from a read's answer to the post that showed it (the buffer wait, the fill), per post
    shown_us: i64,
    shown_max: i64,
    since: i64,
    cpu: i64,
}

impl Stats {
    fn new(since: i64, cpu: i64) -> Stats {
        Stats { since, cpu, ..Stats::default() }
    }

    /// Reported when a frame comes and 10 s have passed since the last report: no timer, so nothing at all when idle.
    fn report(&mut self, frame_height: u32, buffers_seen: usize, forgotten: u64) {
        let now = sys::now_us();
        if now - self.since < 10_000_000 {
            return;
        }
        let cpu = sys::cpu_us();
        let seen = self.posted + self.unchanged;
        let mean = |sum: i64, n: u64| if n > 0 { sum as f64 / 1000.0 / n as f64 } else { 0.0 };
        let delay = if self.delays > 0 {
            format!(
                "from {:.1} ms after xnuhost sent the frame (mean; max {:.1} ms)",
                self.delay_us as f64 / 1000.0 / self.delays as f64,
                self.delay_max as f64 / 1000.0
            )
        } else {
            "the send times not known (an xnuhost before 2026-11-22)".to_string()
        };
        let share =
            |rows: i64, n: u64| if n > 0 { rows as f64 * 100.0 / (n as f64 * frame_height as f64) } else { 0.0 };
        eprintln!(
            "oneios-display: {:.0} s: {seen} frames from xnuhost, {} unchanged (nothing posted), {} posted ({} from guest \
             RAM, {:.0}% of the rows changed, {:.0}% filled); guest pages read in one pass for mean {:.1} ms, max {:.1} \
             ms, {delay}; buffer wait mean {:.1} ms; copy mean {:.1} ms, max {:.1} ms; posted mean {:.1} ms, max {:.1} \
             ms after the read's answer; {} buffers seen ({} forgotten); this process {:.0}% of a core",
            (now - self.since) as f64 / 1e6,
            self.unchanged,
            self.posted,
            self.direct,
            share(self.rows_changed, self.reads),
            share(self.rows_filled, self.posted),
            mean(self.read_us, self.reads),
            self.read_max as f64 / 1000.0,
            mean(self.lock_us, self.posted),
            mean(self.copy_us, self.posted),
            self.copy_max as f64 / 1000.0,
            mean(self.shown_us, self.posted),
            self.shown_max as f64 / 1000.0,
            buffers_seen,
            forgotten,
            (cpu - self.cpu) as f64 * 100.0 / (now - self.since) as f64
        );
        *self = Stats::new(now, cpu);
    }
}

/// What the reader and the poster share, under one lock.
struct Shared {
    /// The picture of 8-bit frames (18 MB at 1440x3120, and its tiles' hashes); None: the memfd is the picture.
    snapshot: Option<Snapshot>,
    /// In the snapshot (or the memfd), not in the layer yet: the poster's work.
    changed: Rect,
    /// When the oldest change in `changed` was answered (0: none), for the stats.
    changed_at: i64,
    /// Set by either thread: the other one ends too.
    end: Option<End>,
    /// Posts made (the reader's hold looks at it: only the first post waits for `ready`).
    frames: u64,
    stats: Stats,
    buffers_seen: usize,
    forgotten: u64,
    /// The hold for session/ready ended: its seconds and the flushes that came meanwhile (the first post says them).
    held: Option<(f64, u64)>,
}

struct Pipe {
    shared: Mutex<Shared>,
    /// The poster waits on it for a change or the end.
    work: Condvar,
    /// The reader waits on it for a post (display-post=serial) or the end.
    posted: Condvar,
}

impl Pipe {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Posts frames until the socket closes (Closed) or a signal asks to stop (Stopped). The picture the layer is to show
/// is the helper's snapshot for 8-bit frames (xhd::snapshot): a direct frame (xnuhost's "frame") is read into it in one
/// pass as soon as it comes and answered, and xnuhost's own pixels (the memfd's flushes) are put over it. Unchanged,
/// the frame is answered at once and nothing is locked or posted, so surfaceflinger has nothing to compose and the
/// panel's refresh rate can drop by itself. Changed, the poster locks the layer's buffer whole and fills what that
/// buffer lacks from the snapshot (xhd::buffers: Surface's own copy-back loses changes once the queue has three buffers
/// or more). Half-float frames have no snapshot: the memfd is the picture, and a direct frame is refused (xnuhost then
/// copies the frames itself).
///
/// Pacing: the next buffer is only handed out when surfaceflinger has taken the last one, so the lock blocks to the
/// panel's vsync, and every frame that came meanwhile is one post.
///
/// `ready`: SESSION/ready (--session), or None (--socket): the first post waits for it (xhd::hold_first). Until the
/// first post the layer has no buffer and the OneOS boot screen under it shows. `signal`: the read end of the signal
/// pipe (a byte in it: stop); `stopping`: the signal handler's flag.
#[allow(clippy::too_many_arguments)]
pub fn run(
    fd: BorrowedFd<'_>,
    f: &Frame,
    direct: &mut Direct,
    layer: &dyn Layer,
    format: i32,
    dataspace: i32,
    ready: Option<PathBuf>,
    signal: Option<BorrowedFd<'_>>,
    stopping: &(dyn Fn() -> bool + Sync),
) -> End {
    layer.set_geometry(f.width, f.height, format);
    set_dataspace(layer, dataspace);
    let snapshot = if f.format == FORMAT_BGRX { Snapshot::new(f.width, f.height).ok() } else { None };
    if f.format == FORMAT_BGRX && snapshot.is_none() {
        eprintln!(
            "oneios-display: no memory for the snapshot: direct frames are refused (xnuhost copies them), the memfd is \
             posted"
        );
    }
    let pipe = Pipe {
        shared: Mutex::new(Shared {
            snapshot,
            changed: Rect::EMPTY,
            changed_at: 0,
            end: None,
            frames: 0,
            stats: Stats::new(sys::now_us(), sys::cpu_us()),
            buffers_seen: 0,
            forgotten: 0,
            held: None,
        }),
        work: Condvar::new(),
        posted: Condvar::new(),
    };
    // The poster's way to wake the reader when it ends (the reader may sleep in poll for good).
    let (wake_read, wake_write) = match sys::pipe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("oneios-display: no pipe for the reader ({e})");
            return End::PollFailed;
        }
    };
    let end = std::thread::scope(|scope| {
        let reader = std::thread::Builder::new().name("oneios-display-read".into()).spawn_scoped(scope, || {
            read_loop(fd, f, direct, &pipe, ready.as_ref(), signal, wake_read.as_fd(), stopping)
        });
        let Ok(reader) = reader else {
            eprintln!("oneios-display: the reader thread could not start");
            return End::PollFailed;
        };
        let end = post_loop(f, layer, format, dataspace, &pipe);
        if end == End::LockFailed {
            let mut s = pipe.lock();
            if s.end.is_none() {
                s.end = Some(end);
            }
            pipe.posted.notify_all();
            drop(s);
            let byte = 1u8;
            // SAFETY: one byte from a live buffer into the pipe's write end, which this scope owns.
            let _ = unsafe { libc::write(wake_write.as_raw_fd(), &byte as *const u8 as *const libc::c_void, 1) };
        }
        let read_end = reader.join().unwrap_or(End::PollFailed);
        if end == End::LockFailed {
            end
        } else {
            read_end
        }
    });
    drop((wake_read, wake_write));
    let frames = pipe.lock().frames;
    let what = match end {
        End::Closed => "the socket closed",
        End::Stopped => "asked to stop",
        _ => "error",
    };
    eprintln!("oneios-display: {what} after {frames} frames");
    end
}

/// The direct frame is answered: done (flags), and no longer pending.
fn answer(fd: BorrowedFd<'_>, direct: &mut Direct, flags: u32) {
    if let Some(p) = direct.pending.take() {
        let _ = super::send_done(fd, p.seq, flags);
    }
}

/// The reader: xnuhost's messages, the snapshot, the answers. Its end is the run's unless the poster failed.
#[allow(clippy::too_many_arguments)]
fn read_loop(
    fd: BorrowedFd<'_>,
    f: &Frame,
    direct: &mut Direct,
    pipe: &Pipe,
    ready: Option<&PathBuf>,
    signal: Option<BorrowedFd<'_>>,
    wake: BorrowedFd<'_>,
    stopping: &(dyn Fn() -> bool + Sync),
) -> End {
    let ready_name = ready.map_or(String::new(), |p| p.display().to_string());
    let whole = Rect::whole(f.width, f.height);
    let mut pending = whole; // xnuhost's own damage (the memfd's flushes), not in the snapshot yet
    let mut unposted = Rect::EMPTY; // read by a refused direct frame: goes with the next post
    let (mut refused, mut held_flushes, mut direct_frames) = (0u64, 0u64, 0u64);
    let (mut held, mut announced) = (false, false);
    let mut hold_start = 0i64;
    let end = loop {
        // Held: the guest's frame is not shown yet, the damage adds up, and `ready` is looked for at every flush and
        // every HOLD_POLL_MS between them (a guest that drew its frame with content and then nothing more).
        let first = pipe.lock().frames == 0;
        let hold = first && super::hold_first(ready.map(|p| p.as_path()));
        if hold && !held {
            held = true;
            hold_start = sys::now_us();
            eprintln!(
                "oneios-display: the first frame waits for {ready_name} (xnuhost's first frame of the guest's own with \
                 content); the boot screen stays"
            );
        }
        if !hold && held && first {
            // For the poster's "first frame posted" line, as before the split.
            let mut s = pipe.lock();
            if s.held.is_none() {
                s.held = Some(((sys::now_us() - hold_start) as f64 / 1e6, held_flushes));
            }
        }
        let timeout = if hold {
            super::HOLD_POLL_MS
        } else if pending.w != 0 || direct.pending.is_some() {
            0
        } else {
            -1
        };
        let mut watched = vec![(fd, libc::POLLIN), (wake, libc::POLLIN)];
        watched.extend(signal.map(|s| (s, libc::POLLIN)));
        let (ready_count, revents) = match sys::poll(&watched, timeout, false) {
            Ok((n, r)) => (n as i32, r),
            Err(Errno(libc::EINTR)) => (-1, vec![0; watched.len()]),
            Err(_) => break End::PollFailed,
        };
        if stopping() || (ready_count > 0 && revents.get(2).is_some_and(|&r| r != 0)) {
            break End::Stopped;
        }
        if let Some(e) = pipe.lock().end {
            break e; // the poster ended
        }
        if !super::drain_direct(fd, f.width, f.height, &mut pending, Some(direct)) {
            break End::Closed;
        }
        if hold {
            if ready_count > 0 {
                held_flushes += 1;
            }
            continue;
        }
        if pending.w == 0 && direct.pending.is_none() {
            continue;
        }
        let mut s = pipe.lock();
        let sh = &mut *s;
        // What this round changes against the snapshot: xnuhost's own damage, the direct frame's changed tiles, and
        // what an earlier refused read put into the snapshot.
        let mut changed = std::mem::take(&mut unposted);
        if pending.w != 0 {
            match sh.snapshot.as_mut() {
                Some(snap) => {
                    // Until the snapshot is whole the memfd is the whole picture. Put first: a direct frame of the same
                    // round is the guest's scanout, newer than xnuhost's copy, and the put tiles have no hash, so the
                    // read below takes them.
                    let put = if snap.full { pending } else { whole };
                    let _ = snap.put(&f.view(), put);
                    changed.union(&put);
                }
                None => changed.union(&pending),
            }
            pending = Rect::EMPTY;
        }
        let mut flags = None;
        if let Some(p) = direct.pending {
            // The guest's pages, read once and at once (the time since xnuhost sent the frame is logged: a late start
            // is as bad as a long read).
            let t0 = sys::now_us();
            if p.sent_us != 0 {
                let delay = (t0 as u32).wrapping_sub(p.sent_us);
                if delay < 10_000_000 {
                    sh.stats.delays += 1;
                    sh.stats.delay_us += delay as i64;
                    sh.stats.delay_max = sh.stats.delay_max.max(delay as i64);
                }
            }
            let (got, error) = match sh.snapshot.as_mut() {
                Some(snap) => snap.update(direct),
                None => (Rect::EMPTY, Err(Errno(libc::ENOMEM))),
            };
            let read = sys::now_us() - t0;
            sh.stats.reads += 1;
            sh.stats.read_us += read;
            sh.stats.read_max = sh.stats.read_max.max(read);
            match error {
                Err(e) => {
                    // Refused: xnuhost copies the frames itself from now on (they come as flushes of the memfd). What
                    // was read is in the snapshot and goes to the layer with the next post.
                    refused += 1;
                    if refused == 1 {
                        eprintln!("oneios-display: a direct frame could not be read from guest RAM ({e})");
                    }
                    flags = Some(DONE_FAILED);
                    unposted.union(&got);
                }
                Ok(()) => {
                    changed.union(&got);
                    sh.stats.rows_changed += got.h as i64;
                    // Changed (the guest's tiles or xnuhost's own pixels): posted next, answered now. Unchanged: the
                    // layer shows it already.
                    flags = Some(if changed.w != 0 { 0 } else { DONE_UNCHANGED });
                    if changed.w != 0 {
                        direct_frames += 1;
                        sh.stats.direct += 1;
                    } else {
                        sh.stats.unchanged += 1;
                    }
                }
            }
        }
        let posts_before = sh.frames;
        let notified = changed.w != 0;
        if notified {
            if sh.changed.w == 0 {
                sh.changed_at = sys::now_us();
            }
            sh.changed.union(&changed);
            pipe.work.notify_one();
        }
        sh.stats.report(f.height, sh.buffers_seen, sh.forgotten);
        drop(s);
        // Answered once the frame is in the snapshot, the helper's own copy, which the poster fills from: the guest
        // draws its next frame while this one waits for a buffer and is posted.
        if let Some(flags) = flags {
            answer(fd, direct, flags);
        }
        // display-post=serial (xnuhost's options): nothing more is read until this change is posted, the timing of the
        // one loop before 2026-11-24 (a frame that comes meanwhile waits for the buffer and the fill).
        if notified && direct.options & OPTION_SERIAL_POST != 0 {
            let mut s = pipe.lock();
            while s.frames == posts_before && s.end.is_none() {
                s = pipe.posted.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        }
        if direct_frames > 0 && !announced {
            announced = true;
            eprintln!(
                "oneios-display: frames come straight from guest RAM now, read once into the snapshot; each buffer gets \
                 what it lacks"
            );
        }
    };
    let mut s = pipe.lock();
    if s.end.is_none() {
        s.end = Some(end);
    }
    pipe.work.notify_one();
    drop(s);
    // A frame still pending (the poster failed, or a stop): answered, so xnuhost does not wait for it.
    answer(fd, direct, DONE_FAILED);
    end
}

/// The poster: a buffer of the layer for each change, filled from the snapshot (or the memfd), posted.
fn post_loop(f: &Frame, layer: &dyn Layer, mut format: i32, mut dataspace: i32, pipe: &Pipe) -> End {
    let whole = Rect::whole(f.width, f.height);
    let mut buffers = Buffers::new(f.width, f.height);
    let mut frames = 0u64;
    loop {
        {
            let mut s = pipe.lock();
            while s.changed.w == 0 && s.end.is_none() {
                s = pipe.work.wait(s).unwrap_or_else(|e| e.into_inner());
            }
            if let Some(e) = s.end {
                return e;
            }
        }
        // Locked whole: Surface then copies nothing back from the last buffer (xhd::buffers), and the poster fills
        // what this buffer lacks itself. No lock of ours is held while the layer hands out a buffer.
        let t0 = sys::now_us();
        let locked = layer.lock(f.width, f.height);
        let t1 = sys::now_us();
        let buffer = match locked {
            Ok(b) => b,
            Err(error) if frames == 0 && format != BUFFER_RGBX_8888 => {
                // No B,G,R,A buffers here: R,G,B,X and a swap. No RGBA_FP16 ones: B,G,R,A, the half floats encoded to
                // sRGB and clamped (what is outside sRGB is lost), tagged sRGB.
                let next = if format == BUFFER_RGBA_FP16 { BUFFER_BGRA_8888 } else { BUFFER_RGBX_8888 };
                eprintln!(
                    "oneios-display: no buffer in format {format} {} ({error}), using {}",
                    format_name(format),
                    format_name(next)
                );
                format = next;
                layer.set_geometry(f.width, f.height, format);
                if dataspace == DATASPACE_SCRGB_LINEAR || dataspace == DATASPACE_SCRGB {
                    dataspace = DATASPACE_SRGB;
                    set_dataspace(layer, dataspace);
                }
                buffers.reset(f.width, f.height); // new buffers
                continue; // the change stays for the next round
            }
            Err(error) => {
                eprintln!("oneios-display: the layer's buffer could not be locked ({error})");
                return End::LockFailed;
            }
        };
        let width = (buffer.width as u32).min(f.width);
        let height = (buffer.height as u32).min(f.height);
        let usable = buffer.width > 0 && buffer.height > 0 && buffer.stride >= buffer.width && !buffer.bits.is_null();
        // Only a buffer of the frame's own size is filled to the last pixel and so known afterwards.
        let exact = usable && buffer.width as u32 == f.width && buffer.height as u32 == f.height;
        let bits = buffer.bits as usize;
        // The buffer's own format decides the copy (RGBA_8888 and RGBX_8888 are both R,G,B bytes).
        let layout = if buffer.format == BUFFER_BGRA_8888 || buffer.format == BUFFER_RGBA_FP16 {
            buffer.format
        } else {
            BUFFER_RGBX_8888
        };
        let bytes = if layout == BUFFER_RGBA_FP16 { 8 } else { 4 };
        let mut s = pipe.lock();
        // The newest picture: every change since the last post, the ones that came while the buffer was awaited too.
        let changed = std::mem::take(&mut s.changed);
        let changed_at = std::mem::take(&mut s.changed_at);
        // What this buffer lacks and what this post changes (the first four buffers: whole, as a buffer not seen yet
        // is).
        let mut fill = if frames < 4 { whole } else { buffers.fill(bits, &changed) };
        let mut filled = 0;
        if usable && fill.clip(width, height) {
            let dst_stride = buffer.stride as usize * bytes;
            // SAFETY: the locked buffer is stride x height pixels of `bytes` each, ours until unlock_and_post.
            let dst = unsafe { std::slice::from_raw_parts_mut(buffer.bits, dst_stride * buffer.height as usize) };
            let _ = match s.snapshot.as_ref() {
                Some(snap) => super::copy(dst, dst_stride, layout, snap.pixels(), snap.stride, FORMAT_BGRX, fill),
                None => super::copy(dst, dst_stride, layout, f.pixels(), f.stride as usize, f.format, fill),
            };
            filled = fill.h as i64;
        }
        drop(s);
        let t2 = sys::now_us();
        layer.unlock_and_post();
        if exact {
            buffers.posted(bits, &changed);
        } else {
            buffers.reset(f.width, f.height); // none of them is known then
        }
        let t3 = sys::now_us();
        if frames == 0 {
            let now = layer.dataspace();
            let waited = match pipe.lock().held {
                Some((seconds, flushes)) => {
                    format!("; held {seconds:.1} s for session/ready, {flushes} wakes for flushes not shown")
                }
                None => String::new(),
            };
            eprintln!(
                "oneios-display: first frame posted ({}x{} buffer, stride {}, format {} {}, dataspace {} {}; the guest's \
                 frame: {}{waited})",
                buffer.width,
                buffer.height,
                buffer.stride,
                buffer.format,
                format_name(buffer.format),
                alt_hex(now as u32),
                super::dataspace_name(now),
                crate::pixels::name(f.format)
            );
        }
        frames += 1;
        let mut s = pipe.lock();
        s.frames = frames;
        pipe.posted.notify_all();
        s.buffers_seen = buffers.len();
        s.forgotten = buffers.forgotten;
        let st = &mut s.stats;
        st.posted += 1;
        st.rows_filled += filled;
        st.lock_us += t1 - t0;
        st.copy_us += t2 - t1;
        st.copy_max = st.copy_max.max(t2 - t1);
        if changed_at != 0 {
            st.shown_us += t3 - changed_at;
            st.shown_max = st.shown_max.max(t3 - changed_at);
        }
    }
}
