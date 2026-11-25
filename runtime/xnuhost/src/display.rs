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
use std::sync::{Mutex, MutexGuard, OnceLock};
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

/// The DRM back end's double-buffered dumb buffers and mode.
struct Drm {
    fd: OwnedFd,
    crtc: u32,
    bufs: [DrmBuf; 2],
    front: usize,
    page_flip: bool,
    prev_dirty: (u32, u32, u32, u32),
    w: u32,
    h: u32,
    stride: u32,
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
            .map(|m| {
                let mut hz = m.vrefresh;
                let total = m.htotal as u64 * m.vtotal as u64;
                if hz == 0 && total != 0 {
                    hz = ((m.clock as u64 * 1000 + total / 2) / total) as u32;
                }
                PanelMode { width: m.hdisplay as u32, height: m.vdisplay as u32, hz }
            })
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
    for connector in connectors.iter().take(res.count_connectors.min(16) as usize) {
        let mut modes = [kms::ModeInfo::default(); 32];
        let mut conn_encoders = [0u32; 8];
        let mut conn = kms::Connector {
            connector_id: *connector,
            modes_ptr: modes.as_mut_ptr() as u64,
            count_modes: 32,
            encoders_ptr: conn_encoders.as_mut_ptr() as u64,
            count_encoders: 8,
            ..Default::default()
        };
        if !kms::call(raw, kms::GETCONNECTOR, &mut conn) || conn.connection != 1 || conn.count_modes == 0 {
            continue;
        }
        let n = conn.count_modes.min(32) as usize;
        let mode = modes[..n]
            .iter()
            .max_by_key(|m| (m.vrefresh, (m.kind & kms::MODE_TYPE_PREFERRED != 0) as u32))
            .copied()
            .unwrap_or(modes[0]);
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
        let mut bufs = Vec::with_capacity(2);
        let mut pitch = 0u32;
        for _ in 0..2 {
            let mut dumb = kms::CreateDumb {
                width: mode.hdisplay as u32,
                height: mode.vdisplay as u32,
                bpp: 32,
                ..Default::default()
            };
            if !kms::call(raw, kms::CREATE_DUMB, &mut dumb) {
                loge!("display: dumb buffer: {}", std::io::Error::last_os_error());
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
                loge!("display: framebuffer: {}", std::io::Error::last_os_error());
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
                return Err(());
            }
            // SAFETY: the mapping just made, dumb.size bytes.
            unsafe { std::ptr::write_bytes(p as *mut u8, 0, dumb.size as usize) };
            pitch = dumb.pitch;
            bufs.push(DrmBuf { fb: fb.fb_id, map: p as *mut u8 });
        }
        let Ok([b0, b1]) = <[DrmBuf; 2]>::try_from(bufs) else { return Err(()) };
        let mut set = kms::Crtc {
            crtc_id: crtc,
            fb_id: b0.fb,
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
            "display: KMS {}x{}@{} on connector {}, crtc {} (double-buffered)",
            mode.hdisplay,
            mode.vdisplay,
            mode.vrefresh,
            connector,
            crtc
        );
        return Ok(Drm {
            fd,
            crtc,
            bufs: [b0, b1],
            front: 0,
            page_flip: true,
            prev_dirty: (0, 0, mode.hdisplay as u32, mode.vdisplay as u32),
            w: mode.hdisplay as u32,
            h: mode.vdisplay as u32,
            stride: pitch,
        });
    }
    loge!("display: no connected KMS connector");
    Err(())
}

fn drm_flush(d: &Display, drm: &mut Drm, x: u32, y: u32, w: u32, h: u32) {
    // Copy the union of this frame's damage and the previous frame's damage into the back buffer so both
    // buffers stay in sync across page-flips.
    let (px, py, pw, ph) = drm.prev_dirty;
    let (cx, cy, cw, ch) = if drm.page_flip && pw > 0 && ph > 0 {
        let nx = x.min(px);
        let ny = y.min(py);
        let nx2 = (x + w).max(px + pw);
        let ny2 = (y + h).max(py + ph);
        (nx, ny, nx2 - nx, ny2 - ny)
    } else {
        (x, y, w, h)
    };
    let target = if drm.page_flip { 1 - drm.front } else { drm.front };
    let buf = &drm.bufs[target];
    let f = &d.frame;
    let ox = if drm.w > f.width { (drm.w - f.width) / 2 } else { 0 };
    let oy = if drm.h > f.height { (drm.h - f.height) / 2 } else { 0 };
    let mut row = cy;
    while row < cy + ch && row + oy < drm.h {
        let width = if cx + cw + ox <= drm.w {
            cw
        } else if cx + ox < drm.w {
            drm.w - cx - ox
        } else {
            0
        };
        // SAFETY: both rows lie inside their buffers (the clipping above); the frame is mapped for the process's life.
        unsafe {
            std::ptr::copy_nonoverlapping(
                f.as_ptr().add(row as usize * f.stride as usize + cx as usize * 4),
                buf.map.add((row + oy) as usize * drm.stride as usize + (cx + ox) as usize * 4),
                width as usize * 4,
            )
        };
        row += 1;
    }
    if drm.page_flip {
        let mut flip = kms::PageFlip { crtc_id: drm.crtc, fb_id: buf.fb, ..Default::default() };
        if kms::call(drm.fd.as_raw_fd(), kms::PAGE_FLIP, &mut flip) {
            drm.front = target;
            drm.prev_dirty = (x, y, w, h);
            return;
        }
        drm.page_flip = false;
    }
    let mut clip = kms::ClipRect {
        x1: (cx + ox) as u16,
        y1: (cy + oy) as u16,
        x2: (cx + ox + cw) as u16,
        y2: (cy + oy + ch) as u16,
    };
    let mut dirty =
        kms::FbDirty { fb_id: buf.fb, clips_ptr: &mut clip as *mut _ as u64, num_clips: 1, ..Default::default() };
    kms::call(drm.fd.as_raw_fd(), kms::DIRTYFB, &mut dirty); // command-mode panels need it; others ignore it
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
        (Some(PanelMode { width: d.w, height: d.h, hz: 0 }), false, true)
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
        drm: drm.map(Mutex::new),
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
    if let Some(drm) = d.drm.as_ref() {
        drm_flush(d, &mut drm.lock().unwrap_or_else(|e| e.into_inner()), x, y, w, h);
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
