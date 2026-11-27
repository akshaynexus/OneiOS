// SPDX-License-Identifier: MIT
//! Where the guest's frames go. Three back ends (docs/booter.md, "Display and input"):
//! - shm: the frame lives in a memfd. A display helper connects to <session>/display.sock, receives the memfd and the
//!   geometry, and is told of every changed rectangle. In a OneOS takeover session One UI's surfaceflinger and
//!   composer keep running and hold the panel, so a root SurfaceControl layer (OneOS's boot-status helper pattern) is
//!   the only way to the glass; the helper owns that layer and copies from the memfd. This is the default. A helper
//!   that says it can ("direct", below) is given guest RAM read-only instead, and copies each frame of a blob scanout
//!   straight from the guest's pages into its layer's buffer: one copy a frame on the host, not two.
//! - drm: direct KMS scanout of a dumb buffer on /dev/dri/card0, for a session in which surfaceflinger and the
//!   composer are stopped (xnuhost is then the DRM master).
//! - none: the frame is kept in memory only (tests, headless bring-up).
//!
//! The wire format is the display helper's (oneios-display's xhd protocol, little-endian, one SOCK_SEQPACKET message
//! each):
//! ```text
//!   hello  { "XHD1", u32 width, u32 height, u32 stride, u32 format } + the memfd (SCM_RIGHTS); format 1: B,G,R,X 8-bit
//!          sRGB, 2: R,G,B,A half floats, linear extended sRGB (pixels.h, display-format=)
//!   flush  { "XHDF", u32 x, u32 y, u32 w, u32 h }
//! ```
//! and from the helper, once, straight after it connects, whatever xnuhost does with it:
//! ```text
//!   panel  { "XHDP", u32 width, u32 height, u32 hz, u32 flags }  the mode surfaceflinger runs the panel in;
//!                                                               flags bit 0: the helper copies frames directly
//! ```
//! The direct path, only to a helper that set that flag:
//! ```text
//!   memory { "XHDM", u32 RAM bytes low, high, u32 slots, u32 slot bytes } + the RAM memfd and the table memfd
//!   frame  { "XHDS", u32 slot, u32 seq low, u32 seq high, u32 sent } the frame described in that slot of the table;
//!          sent: the low 32 bits of CLOCK_MONOTONIC in us at the send (the helper logs its delay to the first read)
//!   done   { "XHDA", u32 seq low, u32 seq high, u32 flags, 0 }  (helper -> xnuhost) every frame up to seq is in the
//!          layer; flags bit 0: the last one could not be copied from guest RAM (xnuhost copies the next ones itself)
//! ```
//! A slot of the table: oneios-display's Slot, then nsegs Segs { u64 RAM memfd offset, u64 length } in the scanout's
//! order.
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use xh_core::bytes::le;

use oneios_display::sys::{self as dsys, Errno, Map};
use oneios_display::xhd::{
    Kind, Message, Seg, Slot, DONE_FAILED, OPTION_PRESENT_AHB, OPTION_SERIAL_POST, PANEL_DIRECT,
};
use xh_core::{loge, logi, pixels};

use crate::config::{self, Config, DisplayBackend, Mode, PanelMode};
use crate::gpu3d::Entry;
use crate::input::Failed;
use crate::virtio_gpu::Done;

// ---- the frame ---------------------------------------------------------------------------------------------------------
/// xnuhost's shadow of the visible frame: `width` x `height` pixels, rows of `stride` bytes, in `format`
/// (pixels::BGRX8, 4 bytes a pixel, or pixels::RGBA16F, 8).
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    pub bpp: u32,
    pixels: NonNull<u8>,
    len: usize,
}

// SAFETY: a frame is memory mapped (or allocated) for the process's life; who writes it is the callers' contract.
unsafe impl Send for Frame {}
// SAFETY: as above.
unsafe impl Sync for Frame {}

impl Frame {
    /// A frame over `len` bytes at `pixels`.
    ///
    /// # Safety
    /// The memory must stay mapped and writable for as long as the frame (and its copies) is used.
    pub unsafe fn new(pixels: NonNull<u8>, len: usize, width: u32, height: u32, stride: u32, format: u32) -> Frame {
        Frame { width, height, stride, format, bpp: pixels::bpp(format), pixels, len }
    }

    /// The pixels.
    pub fn as_ptr(&self) -> *mut u8 {
        self.pixels.as_ptr()
    }

    /// The pixels, to write.
    ///
    /// # Safety
    /// The caller is the frame's only writer while the slice lives (the GPU device's lanes, one at a time); the
    /// display helper only reads it.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn bytes_mut<'a>(&self) -> &'a mut [u8] {
        // SAFETY: the memory is mapped for the process's life (new's contract); one writer (this fn's contract).
        unsafe { std::slice::from_raw_parts_mut(self.pixels.as_ptr(), self.len) }
    }

    /// Whether the frame shows anything (pixels::lit).
    pub fn lit(&self) -> bool {
        // SAFETY: mapped for the process's life; a read racing the frame's writer only sees old or new pixels.
        let bytes = unsafe { std::slice::from_raw_parts(self.pixels.as_ptr(), self.len) };
        pixels::lit(bytes, self.stride as usize, self.width, self.height, self.format)
    }
}

// ---- direct frames --------------------------------------------------------------------------------------------------
// The frame's rows are the guest's own pages (the compositor's surface, which the guest scans out with
// SET_SCANOUT_BLOB and flushes whole): the helper maps guest RAM read-only and copies them into the layer's buffer. The
// guest's flush is answered when the helper says the frame is in the layer (virtio_gpu.rs holds the reply), so the
// guest does not draw into a surface the helper still reads; a helper that does not answer within DIRECT_TIMEOUT_MS is
// answered for, so that a stuck helper costs frames but never the guest.
const DIRECT_SLOTS: u32 = 4;
const DIRECT_SEGS: usize = 16384; // the guest display's MaxEntries (virtio_gpu.rs MAX_BACKING); a blob with more is copied
const DIRECT_TIMEOUT_MS: i64 = 100;
const DIRECT_SLOT_BYTES: usize = Slot::BYTES + DIRECT_SEGS * Seg::BYTES;
const OUTSTANDING: usize = 16;

#[derive(Clone)]
struct Outstanding {
    seq: u64,
    posted_us: i64,
    done: Done,
}

/// Frames taken off the list, answered once the lock is let go (at most OUTSTANDING: no allocation).
struct Taken {
    items: [Option<(u64, Done)>; OUTSTANDING],
    n: usize,
}

impl Taken {
    fn new() -> Taken {
        Taken { items: Default::default(), n: 0 }
    }

    fn answer(&mut self) {
        for item in self.items[..self.n].iter_mut() {
            if let Some((seq, done)) = item.take() {
                done(seq);
            }
        }
        self.n = 0;
    }
}

#[derive(Default)]
struct Stats {
    frames: u64,
    total_us: u64,
    max_us: u64,
    timeouts: u64,
    copies: u64,
    since_us: i64,
}

struct Table {
    fd: OwnedFd,
    map: Map,
}

/// What client_lock guards.
struct Client {
    fd: Option<OwnedFd>,
    gen: u64,
    direct: bool,
    memory: bool,
    ram_fd: RawFd,
    ram_phys: u64,
    ram_size: u64,
    table: Option<Table>,
    out: Vec<Outstanding>,
    stats: Stats,
}

/// One KMS dumb buffer.
struct DrmBuf {
    fb: u32,
    map: *mut u8,
}

/// How the DRM back end puts a written buffer on the panel. Each step down is taken once, when the one above it is
/// refused for good (never for EBUSY: a flip still in flight is waited for).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Present {
    /// PAGE_FLIP of a written buffer with DRM_MODE_PAGE_FLIP_EVENT; the flip thread (drm_events_main) reads the event
    /// and flips the buffer written meanwhile, if any.
    Flip,
    /// SETCRTC of the back buffer: a whole (blocking) commit a frame. A command-mode panel (the S24 Ultra's) is sent a
    /// frame only by a commit [I]: writes into the scanned buffer alone do not reach it.
    SetCrtc,
    /// One buffer, written in place, and DIRTYFB: drivers with neither of the above.
    Dirty,
}

/// A rectangle (x, y, w, h); w or h 0: none.
type Rect = (u32, u32, u32, u32);

/// The smallest rectangle holding both.
fn rect_union(a: Rect, b: Rect) -> Rect {
    if a.2 == 0 || a.3 == 0 {
        return b;
    }
    if b.2 == 0 || b.3 == 0 {
        return a;
    }
    let (x, y) = (a.0.min(b.0), a.1.min(b.1));
    let (x2, y2) = ((a.0 + a.2).max(b.0 + b.2), (a.1 + a.3).max(b.1 + b.3));
    (x, y, x2 - x, y2 - y)
}

/// Dumb buffers the DRM back end makes: one on the panel, one a flip in flight goes to, and one the next frame is
/// written into meanwhile. With two (the third refused) a frame waits for the flip in flight, as before 2026-11-28.
const DRM_BUFFERS: usize = 3;

/// Which dumb buffer is where (2026-11-28). Until then each flush waited, on the GPU device's command lane and under
/// its lock, for the vblank of the flip before it (up to 8.3 ms at 120 Hz), then copied the frame into the back buffer:
/// the render server's next submissions, queued behind the flush on the same virtqueue, waited for both (session of
/// 2026-11-27, v0.1.40: "flush -> the next frame's first GPU submission" 16.8 ms at the median, 23-34 frames a second
/// on a 120 Hz panel). Now a frame is written into a buffer that is neither on the panel nor the target of the flip in
/// flight; it is flipped at once when no flip is in flight, else it waits as `ready` and the flip thread flips it when
/// the event comes. A frame written while one is already ready replaces it (that one is never shown: a mailbox).
///
/// `stale[b]`: what of buffer b is older than the frame (the union of the damage since b was last written), so a
/// frame copies its damage plus whatever the buffer missed while the others were written.
#[derive(Debug)]
struct Chain {
    n: usize,
    front: usize,
    /// the buffer a flip in flight shows (front itself: a commit of the driver's own, whose EBUSY gave no event)
    pending: Option<usize>,
    pending_since_us: i64,
    ready: Option<usize>,
    stale: [Rect; DRM_BUFFERS],
    /// ready frames written over before they were flipped, for the 10 s line
    replaced: u64,
}

impl Chain {
    fn new(n: usize, w: u32, h: u32) -> Chain {
        let n = n.clamp(1, DRM_BUFFERS);
        Chain {
            n,
            front: 0,
            pending: None,
            pending_since_us: 0,
            ready: None,
            stale: [(0, 0, w, h); DRM_BUFFERS],
            replaced: 0,
        }
    }

    /// A frame changed `r`: every buffer misses it until it is written.
    fn damage(&mut self, r: Rect) {
        for s in self.stale.iter_mut().take(self.n) {
            *s = rect_union(*s, r);
        }
    }

    /// The buffer to write the frame into and the rectangle to copy into it (then counted as written); None: every
    /// buffer is on the panel or the target of the flip in flight (two buffers), the flip's event frees one.
    fn begin(&mut self) -> Option<(usize, Rect)> {
        let free = |b: &usize| *b != self.front && Some(*b) != self.pending;
        let b = (0..self.n).filter(free).find(|b| Some(*b) != self.ready).or(self.ready.filter(free))?;
        if self.ready == Some(b) {
            self.ready = None;
            self.replaced += 1;
        }
        let r = std::mem::take(&mut self.stale[b]);
        Some((b, r))
    }

    /// The copy into b failed: it holds nothing of the frame.
    fn failed(&mut self, b: usize, full: Rect) {
        self.stale[b] = full;
    }

    /// Buffer b is written: Some(b) when it is to be flipped now (no flip in flight), None when it waits for the flip
    /// in flight (the flip thread flips it).
    fn written(&mut self, b: usize) -> Option<usize> {
        if self.pending.is_none() {
            return Some(b);
        }
        if self.ready.is_some_and(|r| r != b) {
            self.replaced += 1;
        }
        self.ready = Some(b);
        None
    }

    /// A PAGE_FLIP to b was queued.
    fn flipped(&mut self, b: usize, now_us: i64) {
        self.pending = Some(b);
        self.pending_since_us = now_us;
    }

    /// The flip to b was refused with EBUSY (a commit of the driver's own still in flight, which sends no event): b
    /// waits as ready, and the flip thread takes the commit as done after FLIP_WAIT_MS.
    fn busy(&mut self, b: usize, now_us: i64) {
        self.pending = Some(self.front);
        self.pending_since_us = now_us;
        self.ready = Some(b);
    }

    /// The flip in flight completed (its event, or FLIP_WAIT_MS passed): its buffer is on the panel. Returns the ready
    /// buffer to flip next.
    fn flip_done(&mut self) -> Option<usize> {
        if let Some(p) = self.pending.take() {
            self.front = p;
        }
        self.ready.take()
    }

    /// Whether the flip in flight has waited FLIP_WAIT_MS or more.
    fn overdue(&self, now_us: i64) -> bool {
        self.pending.is_some() && now_us - self.pending_since_us >= FLIP_WAIT_MS as i64 * 1000
    }
}

/// The DRM back end's dumb buffers and mode.
struct Drm {
    fd: OwnedFd,
    crtc: u32,
    connector: u32,
    mode: kms::ModeInfo,
    bufs: Vec<DrmBuf>,
    chain: Chain,
    present: Present,
    w: u32,
    h: u32,
    stride: u32,
    /// Frames put on the panel, flips that were not done within FLIP_WAIT_MS, frames flipped by the flip thread at
    /// the vblank after their copy, and the frames whose copy waited for a buffer (all for the 10 s line).
    shown: u64,
    late: u64,
    flipped_later: u64,
    waited: u64,
    last_note_us: i64,
}

impl Drm {
    /// Buffer b as a frame of the guest frame's size (`f`'s width and height) and B,G,R,X pixels, centred where the
    /// mode is larger; None when the frame does not fit the mode.
    fn view(&self, f: &Frame, b: usize) -> Option<Frame> {
        if f.width > self.w || f.height > self.h || b >= self.bufs.len() {
            return None;
        }
        let (ox, oy) = ((self.w - f.width) / 2, (self.h - f.height) / 2);
        let off = oy as usize * self.stride as usize + ox as usize * 4;
        let len = self.stride as usize * self.h as usize - off;
        // SAFETY: the dumb buffer is mapped for the process's life (stride * h bytes); off lies inside it.
        let p = NonNull::new(unsafe { self.bufs[b].map.add(off) })?;
        // SAFETY: as above; writers are serialized by Display::drm_write and write only a buffer the chain gave them.
        Some(unsafe { Frame::new(p, len, f.width, f.height, self.stride, pixels::BGRX8) })
    }

    /// The whole guest frame, as a rectangle.
    fn full(&self, f: &Frame) -> Rect {
        (0, 0, f.width.min(self.w), f.height.min(self.h))
    }
}

// SAFETY: the dumb buffer's mapping is used under the display's drm lock only.
unsafe impl Send for Drm {}

struct Display {
    backend: DisplayBackend,
    frame: Frame,
    shm: Option<OwnedFd>,
    _shm_map: Option<Map>,
    _heap: Option<Box<[u8]>>,
    listen: Option<OwnedFd>,
    socket_path: String,
    direct_enabled: bool,
    /// display-post=serial: the helper is told to post each frame before it reads the next (docs/switches.md)
    serial_post: bool,
    /// display-present=ahb: the helper posts into write-only buffers on a child of its layer (docs/switches.md)
    present_ahb: bool,
    drm: Option<Mutex<Drm>>,
    /// signalled by the flip thread when a flip completed (a frame with two buffers waits for it)
    drm_flipped: Condvar,
    /// the frames' writers, one at a time (the GPU device's lanes, the console)
    drm_write: Mutex<()>,
    /// wakes the flip thread to arm its FLIP_WAIT_MS deadline for a commit that sends no event (EBUSY)
    drm_wake: Option<OwnedFd>,
    /// the frame posted for the DRM copy thread (drm_post), and its wake-up
    drm_job: Mutex<Option<DrmJob>>,
    drm_job_posted: Condvar,
    client: Mutex<Client>,
}

static DISPLAY: OnceLock<Display> = OnceLock::new();

fn client(d: &Display) -> MutexGuard<'_, Client> {
    d.client.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_us() -> i64 {
    (xh_core::time::monotonic_ns() / 1000) as i64
}

/// The frame, once init made it.
pub fn get() -> Option<Frame> {
    DISPLAY.get().map(|d| d.frame)
}

impl Client {
    /// Takes the frames up to `seq` (all of them: u64::MAX; or those posted before `older_than_us`) off the list into
    /// `done`, for the caller to answer once the lock is let go.
    fn take_done(&mut self, seq: u64, older_than_us: i64, done: &mut Taken, timed_out: bool) {
        let now = now_us();
        let mut kept = 0;
        for i in 0..self.out.len() {
            let o = &self.out[i];
            let take = if older_than_us != 0 { o.posted_us < older_than_us } else { o.seq <= seq };
            if !take {
                self.out.swap(kept, i);
                kept += 1;
                continue;
            }
            if timed_out {
                self.stats.timeouts += 1;
            } else {
                let us = (now - o.posted_us) as u64;
                crate::frame_path::answered(us);
                self.stats.frames += 1;
                self.stats.total_us += us;
                self.stats.max_us = self.stats.max_us.max(us);
            }
            if done.n < OUTSTANDING {
                done.items[done.n] = Some((o.seq, o.done.clone()));
                done.n += 1;
            }
        }
        self.out.truncate(kept);
    }

    /// The RAM and the table go to a direct helper once the RAM is known (set_ram).
    fn send_memory(&mut self, fd: BorrowedFd<'_>, direct_enabled: bool) {
        let Some(table) = self.table.as_ref() else { return };
        if !self.direct || self.memory || self.ram_fd < 0 || !direct_enabled {
            return;
        }
        let m = Message::new(
            Kind::Memory,
            self.ram_size as u32,
            (self.ram_size >> 32) as u32,
            DIRECT_SLOTS,
            DIRECT_SLOT_BYTES as u32,
        );
        // SAFETY: the RAM memfd lives for the process's life (mem.rs).
        let ram = unsafe { BorrowedFd::borrow_raw(self.ram_fd) };
        if let Err(e) = m.send(fd, &[ram, table.fd.as_fd()], 0) {
            loge!("display: guest RAM could not be handed to the helper: {}", e);
            return;
        }
        self.memory = true;
        logi!(
            "display: the helper copies the guest's frames directly (guest RAM shared read-only, {} MiB)",
            self.ram_size >> 20
        );
    }

    /// The helper is gone (or replaced): its socket closed, a new generation.
    fn drop_helper(&mut self) {
        self.fd = None;
        self.gen += 1;
        self.direct = false;
        self.memory = false;
    }
}

/// Direct frames: guest RAM is the memfd `fd`, guest physical `phys` at offset 0, `size` bytes.
pub fn set_ram(fd: RawFd, phys: u64, size: u64) {
    let Some(d) = DISPLAY.get() else { return };
    if d.backend != DisplayBackend::Shm || !d.direct_enabled {
        return;
    }
    let bytes = DIRECT_SLOTS as usize * DIRECT_SLOT_BYTES;
    let table = dsys::memfd("xnuhost-frames", bytes)
        .and_then(|t| Map::new(t.as_fd(), bytes, true).map(|map| Table { fd: t, map }));
    let table = match table {
        Ok(t) => t,
        Err(e) => {
            loge!("display: no table for direct frames: {}", e);
            return;
        }
    };
    let mut c = client(d);
    (c.ram_fd, c.ram_phys, c.ram_size, c.table) = (fd, phys, size, Some(table));
    if let Some(fd) = c.fd.as_ref().map(|f| f.as_raw_fd()) {
        // SAFETY: the client's descriptor, held by `c`.
        c.send_memory(unsafe { BorrowedFd::borrow_raw(fd) }, d.direct_enabled);
    }
}

/// Whether a helper that copies frames from guest RAM itself is connected and has the RAM.
pub fn direct() -> bool {
    DISPLAY.get().is_some_and(|d| {
        if d.drm.is_some() {
            return true; // the DRM copy thread (drm_post)
        }
        let c = client(d);
        d.backend == DisplayBackend::Shm && c.fd.is_some() && c.direct && c.memory
    })
}

/// Hands the helper a frame: rows of `stride` bytes from byte `offset` of the guest segments, `width` x `height`, the
/// rectangle `rect` (x, y, w, h) changed, frame number `seq`. `done(seq)` is called when the helper has it in its layer
/// (or it was given up on). false: not posted (no direct helper, a segment not RAM): the caller copies it.
#[allow(clippy::too_many_arguments)]
pub fn post(
    entries: &[Entry],
    offset: u64,
    stride: u32,
    width: u32,
    height: u32,
    rect: [u32; 4],
    seq: u64,
    done: &Done,
) -> bool {
    let Some(d) = DISPLAY.get() else { return false };
    if d.drm.is_some() {
        let posted = drm_post(d, entries, offset, stride, (width, height), rect, seq, done);
        if posted {
            crate::input::frame_shown();
        }
        return posted;
    }
    let mut over = Taken::new();
    let posted = post_locked(d, &mut client(d), entries, offset, stride, width, height, rect, seq, done, &mut over);
    over.answer();
    // input.rs's touch-to-frame measurement, as flush() does for a copied frame. Without it a session with direct
    // frames (the default since the helper copies them) said "no guest frame after the touch-down" for every gesture
    // (out/session-logs/2026-11-22/kbd-debug: all 66 gestures, the helper showing 153 frames in 10 s).
    if posted {
        crate::input::frame_shown();
    }
    posted
}

#[allow(clippy::too_many_arguments)]
fn post_locked(
    d: &Display,
    c: &mut Client,
    entries: &[Entry],
    offset: u64,
    stride: u32,
    width: u32,
    height: u32,
    rect: [u32; 4],
    seq: u64,
    done: &Done,
    over: &mut Taken,
) -> bool {
    if d.backend != DisplayBackend::Shm || c.fd.is_none() || !c.direct || !c.memory {
        return false;
    }
    let (ram_phys, ram_size) = (c.ram_phys, c.ram_size);
    let Some(table) = c.table.as_mut() else { return false };
    let at = (seq % DIRECT_SLOTS as u64) as usize * DIRECT_SLOT_BYTES;
    let slot_bytes = &mut table.map.bytes_mut()[at..at + DIRECT_SLOT_BYTES];
    let (head, segs) = slot_bytes.split_at_mut(Slot::BYTES);
    let mut count = 0usize;
    let mut last: Option<Seg> = None;
    for e in entries {
        let (pa, len) = (e.addr, e.length as u64);
        if pa < ram_phys || len > ram_size || pa - ram_phys > ram_size - len {
            return false; // not RAM: the copy here instead
        }
        let at = pa - ram_phys;
        if let Some(l) = last.as_mut().filter(|l| l.offset + l.length == at) {
            l.length += len;
            segs[(count - 1) * Seg::BYTES..count * Seg::BYTES].copy_from_slice(&l.encode());
            continue;
        }
        if count == DIRECT_SEGS {
            return false;
        }
        let s = Seg { offset: at, length: len };
        segs[count * Seg::BYTES..(count + 1) * Seg::BYTES].copy_from_slice(&s.encode());
        last = Some(s);
        count += 1;
    }
    let [x, y, w, h] = rect;
    let slot = Slot { seq: 0, offset, stride, width, height, nsegs: count as u32, x, y, w, h, format: 1 };
    head.copy_from_slice(&slot.encode());
    // SAFETY: the slot's first 8 bytes, 8-aligned (the map is page-aligned and slots are 64-byte multiples apart), are
    // the seq the helper reads with acquire: written last, with release.
    unsafe { (*(head.as_mut_ptr() as *const AtomicU64)).store(seq, Ordering::Release) };
    if c.out.len() == OUTSTANDING {
        let oldest = c.out[0].seq;
        c.take_done(oldest, 0, over, true); // the oldest, unanswered
    }
    // The send time goes with the frame: the helper logs how long after it it began to read the guest's pages.
    let sent = now_us();
    let m = Message::new(Kind::Frame, (seq % DIRECT_SLOTS as u64) as u32, seq as u32, (seq >> 32) as u32, sent as u32);
    let fd = c.fd.as_ref().unwrap();
    if m.send(fd.as_fd(), &[], libc::MSG_DONTWAIT).is_err() {
        return false;
    }
    c.out.push(Outstanding { seq, posted_us: sent, done: done.clone() });
    true
}

/// One per helper: reads its messages (the late panel report that says it is direct, the frames it has shown),
/// answers for frames it took too long over, and when it goes, for all of them.
fn reader_main(fd: OwnedFd, gen: u64) {
    let d = DISPLAY.get().expect("the display");
    let mut done = Taken::new();
    loop {
        let (current, waiting) = {
            let c = client(d);
            (gen == c.gen, !c.out.is_empty())
        };
        if !current {
            break;
        }
        let ready = match dsys::poll(&[(fd.as_fd(), libc::POLLIN)], if waiting { 10 } else { 200 }, false) {
            Ok((n, _)) => n,
            Err(Errno(libc::EINTR)) => 0,
            Err(_) => break,
        };
        if ready > 0 {
            let mut packet = [0u8; Message::BYTES];
            let gone = match dsys::recv_packet(fd.as_fd(), &mut packet, libc::MSG_DONTWAIT) {
                Ok(r) if r.length == 0 => true,
                Ok(r) => {
                    let m = (r.length == Message::BYTES).then(|| Message::parse(&packet)).flatten();
                    if let Some(m) = m.filter(|m| m.is(Kind::Done)) {
                        let mut c = client(d);
                        c.take_done(Message::wide(m.a, m.b), 0, &mut done, false);
                        let off = m.c & DONE_FAILED != 0 && gen == c.gen && c.direct;
                        if off {
                            c.direct = false;
                        }
                        drop(c);
                        if off {
                            loge!("display: the helper could not copy a frame from guest RAM; xnuhost copies the frames again");
                        }
                    } else if let Some(_m) = m.filter(|m| m.is(Kind::Panel) && m.d & PANEL_DIRECT != 0) {
                        let mut c = client(d);
                        if gen == c.gen && !c.direct {
                            c.direct = true;
                            c.send_memory(fd.as_fd(), d.direct_enabled);
                        }
                    }
                    false
                }
                Err(Errno(e)) => e != libc::EAGAIN && e != libc::EINTR,
            };
            if gone {
                break;
            }
        }
        done.answer();
        let now = now_us();
        let (report, frames, total, max, timeouts, copies) = {
            let mut c = client(d);
            c.take_done(0, now - DIRECT_TIMEOUT_MS * 1000, &mut done, true);
            let report = now - c.stats.since_us >= 10_000_000;
            let s = &c.stats;
            let figures = (report, s.frames, s.total_us, s.max_us, s.timeouts, s.copies);
            if report {
                c.stats = Stats { since_us: now, ..Stats::default() };
            }
            figures
        };
        crate::frame_path::report(now as u64);
        done.answer();
        if report && (frames != 0 || timeouts != 0 || copies != 0) {
            logi!(
                "display: 10 s: {} frames copied by the helper from guest RAM (its answer: mean {:.1} ms, max {:.1} ms; since \
                 2026-11-23 the helper answers once the frame is in its snapshot, before its post), {} not answered \
                 within {} ms, {} flushes of xnuhost's own copy",
                frames,
                if frames != 0 { total as f64 / 1000.0 / frames as f64 } else { 0.0 },
                max as f64 / 1000.0,
                timeouts,
                DIRECT_TIMEOUT_MS,
                copies
            );
        }
    }
    // The helper is gone or another one took over: what it was shown is answered for. (A helper that took over has had
    // the frames already: helper_attach answered for them, and later ones are its own.)
    {
        let mut c = client(d);
        if gen == c.gen {
            c.take_done(u64::MAX, 0, &mut done, true);
            c.drop_helper();
        }
    }
    done.answer();
}

fn helper_attach(d: &Display, fd: OwnedFd, flags: u32) {
    let hello = Message::new(Kind::Hello, d.frame.width, d.frame.height, d.frame.stride, d.frame.format);
    let shm = d.shm.as_ref().expect("the shm frame");
    if hello.send(fd.as_fd(), &[shm.as_fd()], 0).is_err() {
        return;
    }
    // The helper's options, right after the hello (before any frame: its poster chooses its path at the first post).
    // display-post=serial: the old timing (a helper older than 2026-11-24 ignores the message and has it anyway);
    // display-present=ahb (the default): write-only buffers on a child of the layer (a helper older than 2026-11-25
    // ignores the bit and keeps the window). Nothing is sent when neither is asked.
    let options =
        (if d.serial_post { OPTION_SERIAL_POST } else { 0 }) | (if d.present_ahb { OPTION_PRESENT_AHB } else { 0 });
    if options != 0 {
        let _ = Message::new(Kind::Options, options, 0, 0, 0).send(fd.as_fd(), &[], 0);
    }
    let mut done = Taken::new();
    let direct = {
        let mut c = client(d);
        // the newest helper wins; the last one's frames: it will not answer for them now
        c.take_done(u64::MAX, 0, &mut done, true);
        let reader = fd.try_clone();
        c.fd = Some(fd);
        c.gen += 1;
        c.direct = d.direct_enabled && flags & PANEL_DIRECT != 0;
        c.memory = false;
        let raw = c.fd.as_ref().unwrap().as_raw_fd();
        // SAFETY: the client's descriptor, held by `c`.
        c.send_memory(unsafe { BorrowedFd::borrow_raw(raw) }, d.direct_enabled);
        let gen = c.gen;
        let started = reader.ok().and_then(|r| std::thread::Builder::new().spawn(move || reader_main(r, gen)).ok());
        if started.is_none() {
            c.direct = false; // nobody would read its answers
        }
        c.direct
    };
    done.answer();
    logi!("display: a helper connected{}", if direct { " (it copies frames directly)" } else { "" });
    flush(0, 0, d.frame.width, d.frame.height);
}

fn accept_main() {
    let d = DISPLAY.get().expect("the display");
    let Some(listen) = d.listen.as_ref() else { return };
    loop {
        match dsys::accept(listen.as_fd()) {
            Ok(fd) => helper_attach(d, fd, 0), // a later report says whether it is direct (reader_main)
            Err(Errno(libc::EINTR)) => continue,
            Err(_) => return,
        }
    }
}

fn wait_readable(fd: BorrowedFd<'_>, timeout_ms: i32) -> usize {
    dsys::poll(&[(fd, libc::POLLIN)], timeout_ms, true).map_or(0, |(n, _)| n)
}

// The mode surfaceflinger runs the panel in, from the helper the fork's switch starts beside xnuhost. Bounded: a
// session without a helper shows nothing anyway, and must still boot. A helper that connected but reports nothing keeps
// its connection and gets the hello all the same.
const HELPER_CONNECT_MS: i32 = 10000;
const HELPER_REPORT_MS: i32 = 8000; // under the 10 s a helper waits for the hello

/// The helper's report: the panel's mode and flags, and its connection (kept even without a report).
fn helper_report(listen: Option<&OwnedFd>, first: &mut Option<OwnedFd>) -> Option<(PanelMode, u32)> {
    let listen = listen?;
    if wait_readable(listen.as_fd(), HELPER_CONNECT_MS) == 0 {
        logi!("display: no helper connected within {} s", HELPER_CONNECT_MS / 1000);
        return None;
    }
    let fd = dsys::accept(listen.as_fd()).ok()?;
    let client = first.insert(fd);
    if wait_readable(client.as_fd(), HELPER_REPORT_MS) == 0 {
        logi!("display: the helper did not report the panel's mode");
        return None;
    }
    let mut packet = [0u8; Message::BYTES];
    let report = dsys::recv_packet(client.as_fd(), &mut packet, libc::MSG_DONTWAIT)
        .ok()
        .and_then(|r| config::panel_report(&packet[..r.length.min(Message::BYTES)]).map(|p| (p, r.length)));
    match report {
        Some((panel, _)) => Some((panel, le::u32(&packet, 16))),
        None => {
            logi!("display: the helper's report of the panel's mode was not usable");
            None
        }
    }
}

// ---- drm ---------------------------------------------------------------------------------------------------------------
mod kms {
    //! The few KMS ioctls of the DRM back end (drm/drm_mode.h's structures, by hand).
    use std::os::fd::RawFd;
    use xh_core::ioctl::{io, ioctl};

    const fn iowr(nr: u8, size: usize) -> u32 {
        xh_core::ioctl::iowr(b'd', nr, size)
    }
    pub const SET_MASTER: u32 = io(b'd', 0x1e);
    pub const DROP_MASTER: u32 = io(b'd', 0x1f);
    pub const GETRESOURCES: u32 = iowr(0xa0, std::mem::size_of::<CardRes>());
    pub const SETCRTC: u32 = iowr(0xa2, std::mem::size_of::<Crtc>());
    pub const GETENCODER: u32 = iowr(0xa6, std::mem::size_of::<Encoder>());
    pub const GETCONNECTOR: u32 = iowr(0xa7, std::mem::size_of::<Connector>());
    pub const ADDFB: u32 = iowr(0xae, std::mem::size_of::<FbCmd>());
    pub const PAGE_FLIP: u32 = iowr(0xb0, std::mem::size_of::<PageFlip>());
    pub const DIRTYFB: u32 = iowr(0xb1, std::mem::size_of::<FbDirty>());
    pub const CREATE_DUMB: u32 = iowr(0xb2, std::mem::size_of::<CreateDumb>());
    pub const MAP_DUMB: u32 = iowr(0xb3, std::mem::size_of::<MapDumb>());
    pub const MODE_TYPE_PREFERRED: u32 = 1 << 3;
    /// drm_mode.h's DRM_MODE_PAGE_FLIP_EVENT: the flip's completion is read from the descriptor.
    pub const PAGE_FLIP_EVENT: u32 = 0x01;
    /// drm.h's DRM_EVENT_FLIP_COMPLETE.
    pub const EVENT_FLIP_COMPLETE: u32 = 0x02;

    /// Whether the events read from the descriptor (`buf`: whole struct drm_event records, { u32 type, u32 length,
    /// ... }) hold a flip's completion.
    pub fn flip_completed(buf: &[u8]) -> bool {
        let mut at = 0usize;
        while at + 8 <= buf.len() {
            let kind = u32::from_ne_bytes(buf[at..at + 4].try_into().unwrap());
            let length = u32::from_ne_bytes(buf[at + 4..at + 8].try_into().unwrap()) as usize;
            if kind == EVENT_FLIP_COMPLETE {
                return true;
            }
            if length < 8 {
                break;
            }
            at += length;
        }
        false
    }

    /// Refresh rate of a DRM mode in Hz: `vrefresh`, else derived from `clock / (htotal * vtotal)`.
    pub fn mode_hz(m: &ModeInfo) -> u32 {
        if m.vrefresh != 0 {
            return m.vrefresh;
        }
        let total = m.htotal as u64 * m.vtotal as u64;
        (m.clock as u64 * 1000 + total / 2).checked_div(total).unwrap_or(0) as u32
    }

    /// The mode the back end sets: the largest (the panel's native size, not a lower One UI resolution of the same
    /// panel), then the fastest, then the preferred one.
    pub fn pick_mode(modes: &[ModeInfo]) -> Option<ModeInfo> {
        modes
            .iter()
            .filter(|m| m.hdisplay != 0 && m.vdisplay != 0)
            .max_by_key(|m| {
                (m.hdisplay as u32 * m.vdisplay as u32, mode_hz(m), (m.kind & MODE_TYPE_PREFERRED != 0) as u32)
            })
            .copied()
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct CardRes {
        pub fb_id_ptr: u64,
        pub crtc_id_ptr: u64,
        pub connector_id_ptr: u64,
        pub encoder_id_ptr: u64,
        pub count_fbs: u32,
        pub count_crtcs: u32,
        pub count_connectors: u32,
        pub count_encoders: u32,
        pub min_width: u32,
        pub max_width: u32,
        pub min_height: u32,
        pub max_height: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct ModeInfo {
        pub clock: u32,
        pub hdisplay: u16,
        pub hsync_start: u16,
        pub hsync_end: u16,
        pub htotal: u16,
        pub hskew: u16,
        pub vdisplay: u16,
        pub vsync_start: u16,
        pub vsync_end: u16,
        pub vtotal: u16,
        pub vscan: u16,
        pub vrefresh: u32,
        pub flags: u32,
        pub kind: u32,
        pub name: [u8; 32],
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct Connector {
        pub encoders_ptr: u64,
        pub modes_ptr: u64,
        pub props_ptr: u64,
        pub prop_values_ptr: u64,
        pub count_modes: u32,
        pub count_props: u32,
        pub count_encoders: u32,
        pub encoder_id: u32,
        pub connector_id: u32,
        pub connector_type: u32,
        pub connector_type_id: u32,
        pub connection: u32,
        pub mm_width: u32,
        pub mm_height: u32,
        pub subpixel: u32,
        pub pad: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct Encoder {
        pub encoder_id: u32,
        pub encoder_type: u32,
        pub crtc_id: u32,
        pub possible_crtcs: u32,
        pub possible_clones: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct CreateDumb {
        pub height: u32,
        pub width: u32,
        pub bpp: u32,
        pub flags: u32,
        pub handle: u32,
        pub pitch: u32,
        pub size: u64,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct FbCmd {
        pub fb_id: u32,
        pub width: u32,
        pub height: u32,
        pub pitch: u32,
        pub bpp: u32,
        pub depth: u32,
        pub handle: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct MapDumb {
        pub handle: u32,
        pub pad: u32,
        pub offset: u64,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct Crtc {
        pub set_connectors_ptr: u64,
        pub count_connectors: u32,
        pub crtc_id: u32,
        pub fb_id: u32,
        pub x: u32,
        pub y: u32,
        pub gamma_size: u32,
        pub mode_valid: u32,
        pub mode: ModeInfo,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct ClipRect {
        pub x1: u16,
        pub y1: u16,
        pub x2: u16,
        pub y2: u16,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct PageFlip {
        pub crtc_id: u32,
        pub fb_id: u32,
        pub flags: u32,
        pub reserved: u32,
        pub user_data: u64,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct FbDirty {
        pub fb_id: u32,
        pub flags: u32,
        pub color: u32,
        pub num_clips: u32,
        pub clips_ptr: u64,
    }
    // drm_mode.h's sizes
    const _: () = assert!(std::mem::size_of::<CardRes>() == 64 && std::mem::size_of::<ModeInfo>() == 68);
    const _: () = assert!(std::mem::size_of::<Connector>() == 80 && std::mem::size_of::<Encoder>() == 20);
    const _: () = assert!(std::mem::size_of::<CreateDumb>() == 32 && std::mem::size_of::<FbCmd>() == 28);
    const _: () = assert!(std::mem::size_of::<MapDumb>() == 16 && std::mem::size_of::<Crtc>() == 104);
    const _: () = assert!(std::mem::size_of::<PageFlip>() == 24 && std::mem::size_of::<FbDirty>() == 24);

    /// The ioctl, true when it worked.
    pub fn call<T>(fd: RawFd, request: u32, arg: *mut T) -> bool {
        // SAFETY: each caller passes the structure its request encodes, and the arrays its pointers name.
        unsafe { ioctl(fd, request, arg) }.is_ok()
    }

    pub fn open() -> Option<std::os::fd::OwnedFd> {
        // SAFETY: a NUL-terminated path.
        let fd = unsafe { libc::open(c"/dev/dri/card0".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        // SAFETY: a new descriptor of ours.
        (fd >= 0).then(|| unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
    }
}

/// The panel's largest mode, without taking the display: the modes of the connected built-in panel's connector are
/// read (no DRM master is needed for that, and nothing is set). Only a fallback in a session: this list need not hold
/// the mode surfaceflinger runs (the helper's report does), and the Virtual connectors have to be skipped.
fn panel_native() -> Option<PanelMode> {
    let fd = kms::open()?;
    let raw = fd.as_raw_fd();
    let mut connectors = [0u32; 16];
    let mut res =
        kms::CardRes { connector_id_ptr: connectors.as_mut_ptr() as u64, count_connectors: 16, ..Default::default() };
    if !kms::call(raw, kms::GETRESOURCES, &mut res) {
        return None;
    }
    let mut modes = vec![kms::ModeInfo::default(); 256]; // the kernel copies none of them if they do not all fit
    for &id in connectors.iter().take(res.count_connectors.min(16) as usize) {
        let mut conn = kms::Connector {
            connector_id: id,
            modes_ptr: modes.as_mut_ptr() as u64,
            count_modes: 256,
            ..Default::default()
        };
        if !kms::call(raw, kms::GETCONNECTOR, &mut conn)
            || conn.connection != 1
            || !config::panel_connector(conn.connector_type)
            || conn.count_modes > 256
        {
            continue;
        }
        let list: Vec<PanelMode> = modes[..conn.count_modes as usize]
            .iter()
            .map(|m| PanelMode { width: m.hdisplay as u32, height: m.vdisplay as u32, hz: kms::mode_hz(m) })
            .collect();
        if let Some(n) = config::panel_native(&list) {
            return Some(n);
        }
    }
    None
}

fn drm_init() -> Result<Drm, ()> {
    let Some(fd) = kms::open() else {
        loge!("display: /dev/dri/card0: {}", std::io::Error::last_os_error());
        return Err(());
    };
    let raw = fd.as_raw_fd();
    if !kms::call::<u8>(raw, kms::SET_MASTER, std::ptr::null_mut()) {
        loge!(
            "display: not the DRM master (is surfaceflinger's composer still running?): {}",
            std::io::Error::last_os_error()
        );
        return Err(());
    }
    let (mut crtcs, mut connectors, mut encoders) = ([0u32; 16], [0u32; 16], [0u32; 16]);
    let mut res = kms::CardRes {
        crtc_id_ptr: crtcs.as_mut_ptr() as u64,
        count_crtcs: 16,
        connector_id_ptr: connectors.as_mut_ptr() as u64,
        count_connectors: 16,
        encoder_id_ptr: encoders.as_mut_ptr() as u64,
        count_encoders: 16,
        ..Default::default()
    };
    if !kms::call(raw, kms::GETRESOURCES, &mut res) {
        loge!("display: KMS resources: {}", std::io::Error::last_os_error());
        return Err(());
    }
    // The kernel copies none of a connector's modes when they do not all fit (as panel_native); 256 holds every list
    // seen. With 32, a panel of more modes came back as 32 zero modes and a 0x0 dumb buffer.
    let mut modes = vec![kms::ModeInfo::default(); 256];
    for connector in connectors.iter().take(res.count_connectors.min(16) as usize) {
        let mut conn_encoders = [0u32; 8];
        let mut conn = kms::Connector {
            connector_id: *connector,
            modes_ptr: modes.as_mut_ptr() as u64,
            count_modes: 256,
            encoders_ptr: conn_encoders.as_mut_ptr() as u64,
            count_encoders: 8,
            ..Default::default()
        };
        if !kms::call(raw, kms::GETCONNECTOR, &mut conn)
            || conn.connection != 1
            || !config::panel_connector(conn.connector_type)
            || conn.count_modes == 0
            || conn.count_modes > 256
        {
            continue;
        }
        let Some(mode) = kms::pick_mode(&modes[..conn.count_modes as usize]) else { continue };
        let mut enc = kms::Encoder {
            encoder_id: if conn.encoder_id != 0 { conn.encoder_id } else { conn_encoders[0] },
            ..Default::default()
        };
        if !kms::call(raw, kms::GETENCODER, &mut enc) {
            continue;
        }
        let mut crtc = enc.crtc_id;
        for (k, &c) in crtcs.iter().enumerate().take(res.count_crtcs.min(16) as usize) {
            if crtc == 0 && enc.possible_crtcs & (1 << k) != 0 {
                crtc = c;
            }
        }
        if crtc == 0 {
            continue;
        }
        let mut bufs = Vec::with_capacity(DRM_BUFFERS);
        let mut pitch = 0u32;
        for k in 0..DRM_BUFFERS {
            // Two are needed; a third the driver refuses leaves two (each frame then waits for the flip before it).
            let spare = k >= 2;
            let mut dumb = kms::CreateDumb {
                width: mode.hdisplay as u32,
                height: mode.vdisplay as u32,
                bpp: 32,
                ..Default::default()
            };
            if !kms::call(raw, kms::CREATE_DUMB, &mut dumb) {
                loge!("display: dumb buffer {}: {}", k, std::io::Error::last_os_error());
                if spare {
                    break;
                }
                return Err(());
            }
            let mut fb = kms::FbCmd {
                width: dumb.width,
                height: dumb.height,
                pitch: dumb.pitch,
                bpp: 32,
                depth: 24,
                handle: dumb.handle,
                ..Default::default()
            };
            let mut map = kms::MapDumb { handle: dumb.handle, ..Default::default() };
            if !kms::call(raw, kms::ADDFB, &mut fb) || !kms::call(raw, kms::MAP_DUMB, &mut map) {
                loge!("display: framebuffer {}: {}", k, std::io::Error::last_os_error());
                if spare {
                    break;
                }
                return Err(());
            }
            // SAFETY: maps the dumb buffer the kernel just made, at the offset it gave.
            let p = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    dumb.size as usize,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    raw,
                    map.offset as libc::off_t,
                )
            };
            if p == libc::MAP_FAILED {
                if spare {
                    break;
                }
                return Err(());
            }
            // SAFETY: the mapping just made, dumb.size bytes.
            unsafe { std::ptr::write_bytes(p as *mut u8, 0, dumb.size as usize) };
            pitch = dumb.pitch;
            bufs.push(DrmBuf { fb: fb.fb_id, map: p as *mut u8 });
        }
        let mut set = kms::Crtc {
            crtc_id: crtc,
            fb_id: bufs[0].fb,
            set_connectors_ptr: connector as *const u32 as u64,
            count_connectors: 1,
            mode,
            mode_valid: 1,
            ..Default::default()
        };
        if !kms::call(raw, kms::SETCRTC, &mut set) {
            loge!("display: mode set {}x{}: {}", mode.hdisplay, mode.vdisplay, std::io::Error::last_os_error());
            return Err(());
        }
        logi!(
            "display: KMS {}x{}@{} on connector {}, crtc {} ({} buffers: {})",
            mode.hdisplay,
            mode.vdisplay,
            mode.vrefresh,
            connector,
            crtc,
            bufs.len(),
            if bufs.len() > 2 {
                "a frame is copied while the flip before it waits for its vblank"
            } else {
                "a frame waits for the flip before it"
            }
        );
        let (w, h) = (mode.hdisplay as u32, mode.vdisplay as u32);
        return Ok(Drm {
            fd,
            crtc,
            connector: *connector,
            mode,
            chain: Chain::new(bufs.len(), w, h),
            bufs,
            present: Present::Flip,
            w,
            h,
            stride: pitch,
            shown: 0,
            late: 0,
            flipped_later: 0,
            waited: 0,
            last_note_us: now_us(),
        });
    }
    loge!("display: no connected KMS connector");
    Err(())
}

/// How long a flip may take before it is taken as done (two frames at 60 Hz; such a flip is counted as late).
const FLIP_WAIT_MS: i32 = 34;

fn lock_drm(m: &Mutex<Drm>) -> MutexGuard<'_, Drm> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Puts buffer `b` on the panel the way `drm.present` says; steps down (once, logged) when the driver refuses it for
/// good. With the drm lock held; never waits for a vblank (a flip's event is the flip thread's).
fn drm_show(d: &Display, drm: &mut Drm, b: usize) {
    let raw = drm.fd.as_raw_fd();
    let fb = drm.bufs[b].fb;
    if drm.present == Present::Flip {
        let mut flip =
            kms::PageFlip { crtc_id: drm.crtc, fb_id: fb, flags: kms::PAGE_FLIP_EVENT, ..Default::default() };
        if kms::call(raw, kms::PAGE_FLIP, &mut flip) {
            drm.chain.flipped(b, now_us());
            return;
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EBUSY) {
            // A commit in flight that sends this descriptor no event (the mode set's own): b waits as ready, and the
            // flip thread, woken to arm its deadline, takes the commit as done after FLIP_WAIT_MS and flips b.
            drm.chain.busy(b, now_us());
            if let Some(w) = d.drm_wake.as_ref() {
                let one = 1u64;
                // SAFETY: writes 8 bytes of ours to our eventfd.
                unsafe { libc::write(w.as_raw_fd(), (&one as *const u64).cast(), 8) };
            }
            return;
        }
        loge!("display: KMS page flip refused ({}): a mode set (SETCRTC) a frame from now on", e);
        drm.present = Present::SetCrtc;
    }
    if drm.present == Present::SetCrtc {
        let mut set = kms::Crtc {
            crtc_id: drm.crtc,
            fb_id: fb,
            set_connectors_ptr: &drm.connector as *const u32 as u64,
            count_connectors: 1,
            mode: drm.mode,
            mode_valid: 1,
            ..Default::default()
        };
        if kms::call(raw, kms::SETCRTC, &mut set) {
            drm.chain.front = b; // a whole commit: b is on the panel when it returns
            return;
        }
        loge!(
            "display: KMS mode set a frame refused ({}): one buffer written in place with DIRTYFB from now on",
            std::io::Error::last_os_error()
        );
        drm.present = Present::Dirty;
        // The panel scans the front buffer from now on, written in place: all of it is older than the frame.
        let (front, full) = (drm.chain.front, drm.full(&d.frame));
        drm.chain.failed(front, full);
    }
}

/// A flip in flight past FLIP_WAIT_MS (its event lost) is taken as done; the frame that waited for it is dropped (a
/// newer one is being written). With the drm lock held.
fn drm_take_overdue(drm: &mut Drm) {
    if drm.chain.overdue(now_us()) {
        drm.late += 1;
        if drm.chain.flip_done().is_some() {
            drm.chain.replaced += 1;
        }
    }
}

/// The rectangle clipped to the frame.
fn clip_to(f: &Frame, (x, y, w, h): Rect) -> Rect {
    let (x, y) = (x.min(f.width), y.min(f.height));
    (x, y, w.min(f.width - x), h.min(f.height - y))
}

/// Writes a frame into the panel's next buffer with `write(view, rect)` and puts it on the panel. `rect` is what of that
/// buffer is older than the frame (this frame's `damage` and whatever it missed while the other buffers were written),
/// in the frame's coordinates; `view` is the buffer as a frame of the guest frame's size. False: the copy failed.
fn drm_frame(d: &Display, m: &Mutex<Drm>, damage: Rect, write: &mut dyn FnMut(&Frame, Rect) -> bool) -> bool {
    let _writer = d.drm_write.lock().unwrap_or_else(|e| e.into_inner());
    let mut drm = lock_drm(m);
    drm.chain.damage(damage);
    if drm.present == Present::Dirty {
        let front = drm.chain.front;
        let r = clip_to(&d.frame, std::mem::take(&mut drm.chain.stale[front]));
        let Some(view) = drm.view(&d.frame, front) else { return false };
        let ok = write(&view, r);
        if ok {
            drm_dirty(d, &drm, front, r);
            drm_note(&mut drm);
        } else {
            let full = drm.full(&d.frame);
            drm.chain.failed(front, full);
        }
        return ok;
    }
    drm_take_overdue(&mut drm);
    let mut waited = false;
    let (b, r) = loop {
        if let Some(x) = drm.chain.begin() {
            break x;
        }
        // Two buffers: one on the panel, the other the target of the flip in flight; its event frees one.
        waited = true;
        let left = FLIP_WAIT_MS as i64 * 1000 - (now_us() - drm.chain.pending_since_us);
        if left <= 0 || drm.chain.pending.is_none() {
            drm.late += 1;
            drm.chain.flip_done();
            continue;
        }
        drm = d
            .drm_flipped
            .wait_timeout(drm, std::time::Duration::from_micros(left as u64))
            .unwrap_or_else(|e| e.into_inner())
            .0;
    };
    drm.waited += waited as u64;
    let Some(view) = drm.view(&d.frame, b) else { return false };
    drop(drm);
    // The copy runs without the lock: the flip thread flips the frame before this one meanwhile.
    let ok = write(&view, clip_to(&d.frame, r));
    let mut drm = lock_drm(m);
    if !ok {
        let full = drm.full(&d.frame);
        drm.chain.failed(b, full);
        return false;
    }
    drm_take_overdue(&mut drm);
    if let Some(b) = drm.chain.written(b) {
        drm_show(d, &mut drm, b);
    }
    drm_note(&mut drm);
    true
}

/// A frame shown; the 10 s line.
fn drm_note(drm: &mut Drm) {
    drm.shown += 1;
    let t = now_us();
    if t - drm.last_note_us >= 10_000_000 {
        logi!(
            "display: 10 s: KMS {} frames by {}, {} flipped by the flip thread at a later vblank, {} replaced before \
             their flip, {} copies waited for a buffer, {} flips later than {} ms",
            drm.shown,
            match drm.present {
                Present::Flip => "page flip",
                Present::SetCrtc => "mode set",
                Present::Dirty => "DIRTYFB",
            },
            drm.flipped_later,
            drm.chain.replaced,
            drm.waited,
            drm.late,
            FLIP_WAIT_MS
        );
        (drm.shown, drm.late, drm.flipped_later, drm.waited, drm.chain.replaced, drm.last_note_us) = (0, 0, 0, 0, 0, t);
    }
}

/// The flip thread: reads the flip events from the DRM descriptor (it alone reads them) and flips the frame written
/// while the flip before it was in flight. Sleeps in poll(2) on the descriptor (and on drm_wake, which arms the
/// FLIP_WAIT_MS deadline of a commit that sends no event); it never wakes on a timer while no flip is in flight.
fn drm_events_main() {
    let Some(d) = DISPLAY.get() else { return };
    let Some(m) = d.drm.as_ref() else { return };
    let fd = lock_drm(m).fd.as_raw_fd();
    let wake = d.drm_wake.as_ref().map_or(-1, |w| w.as_raw_fd());
    loop {
        let timeout = {
            let drm = lock_drm(m);
            if drm.chain.pending.is_some() && drm.present == Present::Flip {
                let left = FLIP_WAIT_MS as i64 * 1000 - (now_us() - drm.chain.pending_since_us);
                ((left + 999) / 1000).clamp(0, FLIP_WAIT_MS as i64) as i32
            } else {
                -1
            }
        };
        let mut p = [
            libc::pollfd { fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: wake, events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: our pollfds (a negative fd is ignored by poll).
        let n = unsafe { libc::poll(p.as_mut_ptr(), 2, timeout) };
        if n < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            loge!(
                "display: the flip thread's poll: {}; flips wait for the next frame from now on",
                std::io::Error::last_os_error()
            );
            return;
        }
        if p[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            loge!("display: the DRM descriptor reports an error ({:#x}): the flip thread ends", p[0].revents);
            return;
        }
        if p[1].revents & libc::POLLIN != 0 {
            let mut v = 0u64;
            // SAFETY: reads 8 bytes into ours from our eventfd.
            unsafe { libc::read(wake, (&mut v as *mut u64).cast(), 8) };
        }
        let mut done = false;
        if p[0].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 1024];
            // SAFETY: reads into the buffer, at most its length.
            let got = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            done = got > 0 && kms::flip_completed(&buf[..got as usize]);
        }
        let mut drm = lock_drm(m);
        if !done && drm.chain.overdue(now_us()) {
            drm.late += 1;
            done = true;
        }
        if done && drm.chain.pending.is_some() {
            if let Some(next) = drm.chain.flip_done() {
                drm.flipped_later += 1;
                drm_show(d, &mut drm, next);
            }
            d.drm_flipped.notify_all();
        }
    }
}

/// Rows of the frame's rectangle `r` from `src` (xnuhost's frame: B,G,R,X 8-bit or half floats) into `dst` (a
/// buffer's view, B,G,R,X 8-bit), at the same coordinates.
fn copy_frame(src: &Frame, dst: &Frame, (x, y, w, h): Rect) -> bool {
    let (x, y, w, h) = clip_to(dst, clip_to(src, (x, y, w, h)));
    // SAFETY: both frames are mapped for the process's life; the rows below lie inside them (clipped above).
    let (from, to) = unsafe {
        (std::slice::from_raw_parts(src.as_ptr(), src.len), std::slice::from_raw_parts_mut(dst.as_ptr(), dst.len))
    };
    for row in y..y + h {
        let s = row as usize * src.stride as usize + x as usize * src.bpp as usize;
        let t = row as usize * dst.stride as usize + x as usize * 4;
        let n = w as usize;
        if src.format == pixels::RGBA16F {
            pixels::rgba16f_to_8(&mut to[t..t + n * 4], &from[s..s + n * 8], false);
        } else {
            to[t..t + n * 4].copy_from_slice(&from[s..s + n * 4]);
        }
    }
    true
}

/// drm: a frame of the guest's, written by `write(view, rect)` straight into the panel's next buffer (B,G,R,X 8-bit,
/// `view` of the guest frame's size; `rect`: what to copy, `damage` and what that buffer missed) and put on the panel.
/// Some(whether the copy went); None: not the DRM back end, or the frame does not fit its mode, and the caller copies
/// into xnuhost's frame and calls flush. One copy a frame (until 2026-11-28 two: into xnuhost's frame, then into the
/// buffer).
pub fn scanout_write(damage: Rect, write: &mut dyn FnMut(&Frame, Rect) -> bool) -> Option<bool> {
    let d = DISPLAY.get()?;
    let m = d.drm.as_ref()?;
    lock_drm(m).view(&d.frame, 0)?;
    crate::input::frame_shown(); // input.rs's touch-to-frame measurement
    Some(drm_frame(d, m, damage, write))
}

/// A blob scanout's frame for the DRM copy thread: the guest's segments (rows of `stride` bytes from byte `offset`, the
/// scanout `size`), the changed rectangle, and the answer that releases the guest's flush.
struct DrmJob {
    entries: Vec<Entry>,
    offset: u64,
    stride: u32,
    size: (u32, u32),
    rect: Rect,
    seq: u64,
    done: Done,
}

/// drm: hands a B,G,R,X/A blob scanout's frame to the copy thread (2026-11-28), as a direct display helper is handed
/// one: the GPU device's command lane holds the flush's answer and goes on with the next commands (the render server's
/// submissions) while the thread copies the rows from the guest's pages into the panel's next buffer and answers
/// `done(seq)`. Until then the lane copied the frame itself, under the device's lock, and every command behind the
/// flush waited for it. A frame posted while the one before is not taken yet replaces it, its damage kept (the newer
/// seq answers both). False: a segment is not guest RAM, the caller copies the frame.
#[allow(clippy::too_many_arguments)]
fn drm_post(
    d: &Display,
    entries: &[Entry],
    offset: u64,
    stride: u32,
    size: (u32, u32),
    [x, y, w, h]: [u32; 4],
    seq: u64,
    done: &Done,
) -> bool {
    if entries.iter().any(|e| xh_core::hooks::ram(e.addr, e.length as u64).is_none()) {
        return false;
    }
    let mut job = d.drm_job.lock().unwrap_or_else(|e| e.into_inner());
    let rect = job.as_ref().map_or((x, y, w, h), |old| rect_union(old.rect, (x, y, w, h)));
    *job = Some(DrmJob { entries: entries.to_vec(), offset, stride, size, rect, seq, done: done.clone() });
    d.drm_job_posted.notify_one();
    true
}

/// The DRM copy thread: takes the posted frame (asleep on drm_job_posted while there is none), copies it into the
/// panel's next buffer and puts that on the panel (drm_frame), answers the guest's flush. The copy's 10 s line is the
/// GPU device's own (`display: 10 s: flushes N, copy mean X us max Y us (Z MiB)`).
fn drm_copy_main() {
    let Some(d) = DISPLAY.get() else { return };
    let Some(m) = d.drm.as_ref() else { return };
    let mut note = crate::virtio_gpu::FlushNote::default();
    loop {
        let job = {
            let mut q = d.drm_job.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(j) = q.take() {
                    break j;
                }
                q = d.drm_job_posted.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let t0 = now_us();
        let mut bytes = 0u64;
        drm_frame(d, m, job.rect, &mut |view, (x, y, w, h)| {
            bytes = w as u64 * h as u64 * 4;
            crate::virtio_gpu::blob_rows_bgrx(&job.entries, job.offset, job.stride, job.size, [x, y, w, h], view)
        });
        note.note((now_us() - t0) as u64, bytes);
        (job.done)(job.seq);
    }
}

/// DIRTYFB of the rectangle of buffer `target` (the frame's coordinates, centred as Drm::view places them).
fn drm_dirty(d: &Display, drm: &Drm, target: usize, (x, y, w, h): (u32, u32, u32, u32)) {
    let f = &d.frame;
    let ox = if drm.w > f.width { (drm.w - f.width) / 2 } else { 0 };
    let oy = if drm.h > f.height { (drm.h - f.height) / 2 } else { 0 };
    let mut clip = kms::ClipRect {
        x1: (x + ox).min(drm.w) as u16,
        y1: (y + oy).min(drm.h) as u16,
        x2: (x + ox + w).min(drm.w) as u16,
        y2: (y + oy + h).min(drm.h) as u16,
    };
    let mut dirty = kms::FbDirty {
        fb_id: drm.bufs[target].fb,
        clips_ptr: &mut clip as *mut _ as u64,
        num_clips: 1,
        ..Default::default()
    };
    kms::call(drm.fd.as_raw_fd(), kms::DIRTYFB, &mut dirty);
}

// ---- the start ---------------------------------------------------------------------------------------------------------
/// The socket first: the helper that reports the panel's mode connects to it before the frame has a size.
fn shm_listen(c: &Config) -> Result<(Option<OwnedFd>, String), ()> {
    if c.session.is_empty() {
        return Ok((None, String::new())); // no session folder: nobody to connect
    }
    let path = format!("{}/display.sock", c.session);
    if path.len() >= 108 {
        loge!("display: session path too long for a socket");
        return Err(());
    }
    match dsys::seqpacket_listen(std::path::Path::new(&path)) {
        Ok(fd) => Ok((Some(fd), path)),
        Err(e) => {
            loge!("display: {}: {}", path, e);
            Err(())
        }
    }
}

/// Opens the back end and sizes the frame: what is configured, else the mode the display helper reports
/// (surfaceflinger's, after its --takeover has asked for the native one), else the largest of the built-in panel's DRM
/// modes, else panel-fallback= (1440x3120 at 120 Hz, the Galaxy S24 Ultra's). With the KMS back end, the mode that was
/// set. Returns the guest's frame: width, height, refresh rate.
pub fn init(c: &Config) -> Result<(u32, u32, u32), Failed> {
    let backend = c.display;
    let drm = if backend == DisplayBackend::Drm { Some(drm_init().map_err(|_| Failed)?) } else { None };
    let (listen, socket_path) =
        if backend == DisplayBackend::Shm { shm_listen(c).map_err(|_| Failed)? } else { (None, String::new()) };
    // Only a session reads the host's panel: a bench run's host display is somebody's monitor.
    let session = c.mode == Mode::Takeover;
    let pinned = c.width != 0 && c.height != 0 && c.hz != 0;
    let mut first_client = None;
    let mut first_flags = 0;
    let (panel, reported, known) = if let Some(d) = drm.as_ref() {
        (Some(PanelMode { width: d.w, height: d.h, hz: kms::mode_hz(&d.mode) }), false, true)
    } else if session && !pinned {
        let report =
            if backend == DisplayBackend::Shm { helper_report(listen.as_ref(), &mut first_client) } else { None };
        match report {
            Some((p, flags)) => {
                first_flags = flags;
                (Some(p), true, true)
            }
            None => {
                let native = panel_native();
                (native, false, native.is_some())
            }
        }
    } else {
        (None, false, false)
    };
    let mode = c.display_mode(if known { panel.as_ref() } else { None });
    let format = c.display_format.pixels();
    let bpp = pixels::bpp(format);
    let (width, height) = (mode.width, mode.height);
    let stride = width * bpp;
    let bytes = stride as usize * height as usize;
    let (shm, shm_map, heap, pixels) = if backend == DisplayBackend::Shm {
        let made =
            dsys::memfd("xnuhost-frame", bytes).and_then(|fd| Map::new(fd.as_fd(), bytes, true).map(|m| (fd, m)));
        match made {
            Ok((fd, map)) => {
                let p = NonNull::new(map.as_ptr() as *mut u8).ok_or(Failed)?;
                (Some(fd), Some(map), None, p)
            }
            Err(e) => {
                loge!("display: shared frame: {}", e);
                return Err(Failed);
            }
        }
    } else {
        let mut heap = vec![0u8; bytes.max(1)].into_boxed_slice();
        let p = NonNull::new(heap.as_mut_ptr()).ok_or(Failed)?;
        (None, None, Some(heap), p)
    };
    // SAFETY: the memfd's map (or the heap buffer) is kept in the Display, which lives for the process's life.
    let frame = unsafe { Frame::new(pixels, bytes, width, height, stride, format) };
    let display = Display {
        backend,
        frame,
        shm,
        _shm_map: shm_map,
        _heap: heap,
        listen,
        socket_path,
        direct_enabled: c.display_direct,
        serial_post: c.display_post_serial,
        present_ahb: c.display_present_ahb,
        drm_wake: drm.as_ref().and_then(|_| {
            // SAFETY: a new eventfd of ours, owned from here on.
            let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
            // SAFETY: fd is open and ours alone.
            (fd >= 0).then(|| unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) })
        }),
        drm: drm.map(Mutex::new),
        drm_flipped: Condvar::new(),
        drm_write: Mutex::new(()),
        drm_job: Mutex::new(None),
        drm_job_posted: Condvar::new(),
        client: Mutex::new(Client {
            fd: None,
            gen: 0,
            direct: false,
            memory: false,
            ram_fd: -1,
            ram_phys: 0,
            ram_size: 0,
            table: None,
            out: Vec::with_capacity(OUTSTANDING),
            stats: Stats::default(),
        }),
    };
    if DISPLAY.set(display).is_err() {
        loge!("display: shared frame: {}", "already made");
        return Err(Failed);
    }
    let d = DISPLAY.get().unwrap();
    if d.drm.is_some() {
        for (name, main) in [("xh-drm-flip", drm_events_main as fn()), ("xh-drm-copy", drm_copy_main)] {
            if std::thread::Builder::new().name(name.into()).spawn(main).is_err() {
                loge!("display: thread {}: {}", name, std::io::Error::last_os_error());
                return Err(Failed);
            }
        }
    }
    if backend == DisplayBackend::Shm {
        if let Some(fd) = first_client.take() {
            helper_attach(d, fd, first_flags);
        }
        if d.listen.is_some() && std::thread::Builder::new().spawn(accept_main).is_err() {
            loge!("display: shared frame: {}", std::io::Error::last_os_error());
            return Err(Failed);
        }
    }
    if pinned && session {
        logi!("display: the mode is the configured one (width, height, hz); the panel's was not asked for");
    } else if reported {
        let p = panel.unwrap();
        logi!("display: the display helper reports the panel at {}x{}, {} Hz", p.width, p.height, p.hz);
    } else if known {
        let p = panel.unwrap();
        logi!("display: the built-in panel's largest DRM mode is {}x{} at {} Hz", p.width, p.height, p.hz);
    } else {
        let fallback = c.panel_fallback();
        logi!(
            "display: the panel's modes were not read ({}): {}x{} at {} Hz assumed ({})",
            if session {
                "no helper's report and no connected built-in panel in /dev/dri/card0"
            } else {
                "a bench run"
            },
            fallback.width,
            fallback.height,
            fallback.hz,
            if c.panel_fallback.is_some_and(|m| m.width != 0) {
                "panel-fallback="
            } else {
                "the S24 Ultra's; panel-fallback= sets another phone's"
            }
        );
    }
    let points = |n: u32| n.checked_div(c.scale).unwrap_or(0);
    logi!(
        "display: {}, guest frame {}x{} at {} Hz, @{}x ({}x{} points)",
        c.display.as_str(),
        width,
        height,
        mode.hz,
        c.scale,
        points(width),
        points(height)
    );
    logi!(
        "display: the frame's pixels: {}, {} bytes a pixel, stride {} (display-format={})",
        pixels::name(format),
        bpp,
        stride,
        if format == pixels::RGBA16F { "rgba16f" } else { "bgrx" }
    );
    Ok((width, height, mode.hz))
}

static DROPPED: AtomicU32 = AtomicU32::new(0);

/// The rectangle changed (already copied into the frame).
pub fn flush(x: u32, y: u32, w: u32, h: u32) {
    crate::input::frame_shown(); // input.rs's touch-to-frame measurement
    let Some(d) = DISPLAY.get() else { return };
    if let Some(m) = d.drm.as_ref() {
        let frame = d.frame;
        drm_frame(d, m, (x, y, w, h), &mut |view, r| copy_frame(&frame, view, r));
    }
    if d.backend != DisplayBackend::Shm {
        return;
    }
    let mut c = client(d);
    c.stats.copies += 1;
    let Some(fd) = c.fd.as_ref() else { return };
    // Only a helper that is gone loses its socket. A full queue (EAGAIN) or a passing shortage (ENOBUFS, ENOMEM) drops
    // this flush only: the next one names the damage again. On 2026-11-12 (15:35 session) the helper's socket was
    // closed after 7 frames with no line saying why, and the panel stayed dark while iOS drew.
    let m = Message::new(Kind::Flush, x, y, w, h);
    match m.send(fd.as_fd(), &[], libc::MSG_DONTWAIT) {
        Ok(()) => {}
        Err(Errno(e)) if e == libc::EAGAIN || e == libc::EINTR => {}
        Err(e @ Errno(libc::EPIPE | libc::ECONNRESET | libc::ENOTCONN)) => {
            logi!("display: the helper's socket is gone ({}): closed", e);
            c.drop_helper(); // its reader answers for its frames
        }
        Err(e) => {
            let n = DROPPED.fetch_add(1, Ordering::Relaxed);
            if n % 1000 == 0 {
                logi!("display: a flush to the helper failed ({}): dropped, the socket kept ({} so far)", e, n + 1);
            }
        }
    }
}

/// The socket removed, the DRM master given back.
pub fn close() {
    let Some(d) = DISPLAY.get() else { return };
    if let Some(listen) = d.listen.as_ref() {
        // SAFETY: shuts the listening socket down (the accept thread's accept returns), its descriptor kept.
        unsafe { libc::shutdown(listen.as_raw_fd(), libc::SHUT_RDWR) };
        let _ = std::fs::remove_file(&d.socket_path);
    }
    if let Some(drm) = d.drm.as_ref() {
        let drm = drm.lock().unwrap_or_else(|e| e.into_inner());
        kms::call::<u8>(drm.fd.as_raw_fd(), kms::DROP_MASTER, std::ptr::null_mut());
    }
}

#[cfg(test)]
mod tests;
