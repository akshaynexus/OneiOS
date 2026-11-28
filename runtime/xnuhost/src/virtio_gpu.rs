// SPDX-License-Identifier: MIT
//! The virtio-gpu device (VirtIO 1.2, 5.7).
//!
//! 2D: the scanout the guest's display driver presents frames on (UniXNU-droidvm's DroidVMVirtioGpuDisplay.cpp under
//! IOMobileFramebufferLegacy); the first frame says the guest is up.
//!
//! 3D ([`create_3d`]): crosvm's DRM native context as UniXNU-droidvm's DroidVMVirtioGpu3D.cpp drives it -- capset 6,
//! CTX_CREATE, RESOURCE_CREATE_BLOB (HOST3D for a context's shmem, HOST3D_GUEST over guest pages), MAP_BLOB answered
//! with MAP_INFO_POOL and the offset in /arm-io/virtio-gpu drm-host-pool, SUBMIT_3D with per-ring fences (a fenced
//! command is returned when its fence retires). The renderer is behind gpu3d.rs's [`Gpu3d`] (drm2kgsl on KGSL). Its
//! control queue is served by a thread of its own: a 3D command never runs in a vCPU's trap handler, and a command
//! fenced on a context's ring is returned to the guest only when its fence retires (what crosvm does, and what
//! UniXNU-droidvm's DroidVMVirtioGpu3D.cpp waits for). Guest blobs can be the scanout (SET_SCANOUT_BLOB); the 2D
//! commands stay.
use std::collections::{HashMap, VecDeque};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use xh_core::thread::lock;

use xh_core::bytes::le;
use xh_core::pixels::{self, BGRX8, LIT_LEVEL, LIT_STEP, RGBA16F, VIRTIO_GPU_FORMAT_RGBA16F};
use xh_core::virtio::{self, Viov, Virtio};
use xh_core::{logi, time};

use crate::display::Frame;
use crate::gpu3d::{self, Entry, Gpu3d};

const CMD_GET_DISPLAY_INFO: u32 = 0x100;
const CMD_RESOURCE_CREATE_2D: u32 = 0x101;
const CMD_RESOURCE_UNREF: u32 = 0x102;
const CMD_SET_SCANOUT: u32 = 0x103;
const CMD_RESOURCE_FLUSH: u32 = 0x104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x106;
const CMD_RESOURCE_DETACH_BACKING: u32 = 0x107;
const CMD_GET_CAPSET_INFO: u32 = 0x108;
const CMD_GET_CAPSET: u32 = 0x109;
const CMD_RESOURCE_CREATE_BLOB: u32 = 0x10c;
const CMD_SET_SCANOUT_BLOB: u32 = 0x10d;
const CMD_CTX_CREATE: u32 = 0x200;
const CMD_CTX_DESTROY: u32 = 0x201;
const CMD_CTX_ATTACH_RESOURCE: u32 = 0x202;
const CMD_CTX_DETACH_RESOURCE: u32 = 0x203;
const CMD_SUBMIT_3D: u32 = 0x207;
const CMD_RESOURCE_MAP_BLOB: u32 = 0x208;
const CMD_RESOURCE_UNMAP_BLOB: u32 = 0x209;
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
const RESP_OK_CAPSET_INFO: u32 = 0x1102;
const RESP_OK_CAPSET: u32 = 0x1103;
const RESP_OK_MAP_INFO: u32 = 0x1106;
const RESP_ERR_UNSPEC: u32 = 0x1200;
const RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;
const RESP_ERR_INVALID_SCANOUT_ID: u32 = 0x1202;
const RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;
const RESP_ERR_INVALID_CONTEXT_ID: u32 = 0x1204;
const RESP_ERR_INVALID_PARAMETER: u32 = 0x1205;

const FMT_BGRA: u32 = 1;
const FMT_BGRX: u32 = 2;
const FMT_ARGB: u32 = 3;
const FMT_XRGB: u32 = 4;
const FMT_RGBA: u32 = 67;
const FMT_XBGR: u32 = 68;
const FMT_ABGR: u32 = 121;
const FMT_RGBX: u32 = 134;
/// Half floats (R,G,B,A, linear extended sRGB): xnuhost's own code, include/xnuhost/pixels.h.
const FMT_RGBA16F: u32 = VIRTIO_GPU_FORMAT_RGBA16F;

const FLAG_FENCE: u32 = 1;
const FLAG_RING_IDX: u32 = 2;
const MAP_INFO_POOL: u32 = 1 << 31;
const F_VIRGL: u64 = 1 << 0;
const F_RESOURCE_BLOB: u64 = 1 << 3;
const F_CONTEXT_INIT: u64 = 1 << 4;
const F_CREATE_GUEST_HANDLE: u64 = 1 << 6;

const MAX_RES: usize = 64;
/// The guest display's MaxEntries (DroidVMVirtioGpu.h: 64 MiB in 4 KiB pages): a 2D surface of scattered 4 KiB pages
/// (1440x3120 has 4388)
const MAX_BACKING: u32 = 16384;
const MAX_CTX: usize = 128;
/// The guest's 3D blobs: DroidVMVirtioGpu3D.cpp MaxBlobEntries, (HugeRequest - sizeof(CreateBlob)) / sizeof(Entry) =
/// (1 MiB - 56) / 16, one entry a page of up to 256 MiB of scattered pages; the request is the 1 MiB `command` takes.
/// It was 16384 (the display's MaxEntries), and an 80 MiB blob of single pages (20480 entries: Unity2025Poster,
/// 2026-11-23 18:24) was refused. udmabufs() splits a blob into pieces of at most 1024 runs and 64 MiB.
const MAX_BLOB_ENTRIES: u32 = ((1 << 20) - 56) / 16;
/// gpu-blob-entries= (docs/switches.md): the most the device takes, MAX_BLOB_ENTRIES unless the config asks fewer (16384:
/// the size before 78c4b86).
static BLOB_ENTRIES: AtomicU32 = AtomicU32::new(MAX_BLOB_ENTRIES);

/// session.rs, once before the device starts.
pub fn set_blob_entries(n: u32) {
    BLOB_ENTRIES.store(n.min(MAX_BLOB_ENTRIES), Ordering::Relaxed);
}
/// The live blobs at most (C: an open-addressed table of 1 << 15 slots, three quarters full at most).
const MAX_BLOBS: usize = (1 << 15) / 4 * 3;
const WORKER_IOV: usize = 256;
const HDR: usize = 24;

fn format_known(f: u32) -> bool {
    matches!(f, FMT_BGRA | FMT_BGRX | FMT_ARGB | FMT_XRGB | FMT_RGBA | FMT_XBGR | FMT_ABGR | FMT_RGBX | FMT_RGBA16F)
}

/// Bytes a pixel of a scanout format takes in guest memory.
fn format_bpp(f: u32) -> u32 {
    if f == FMT_RGBA16F {
        8
    } else {
        4
    }
}

/// The command header (struct virtio_gpu_ctrl_hdr, 24 bytes).
#[derive(Clone, Copy, Debug, Default)]
struct Hdr {
    kind: u32,
    flags: u32,
    fence: u64,
    ctx: u32,
    ring: u8,
    pad: [u8; 3],
}

impl Hdr {
    fn decode(b: &[u8]) -> Hdr {
        Hdr {
            kind: le::u32(b, 0),
            flags: le::u32(b, 4),
            fence: le::u64(b, 8),
            ctx: le::u32(b, 16),
            ring: b[20],
            pad: [b[21], b[22], b[23]],
        }
    }

    fn encode(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.kind.to_le_bytes());
        out[4..8].copy_from_slice(&self.flags.to_le_bytes());
        out[8..16].copy_from_slice(&self.fence.to_le_bytes());
        out[16..20].copy_from_slice(&self.ctx.to_le_bytes());
        out[20] = self.ring;
        out[21..24].copy_from_slice(&self.pad);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Rect {
    fn decode(b: &[u8]) -> Rect {
        Rect { x: le::u32(b, 0), y: le::u32(b, 4), w: le::u32(b, 8), h: le::u32(b, 12) }
    }

    fn ok(&self, w: u32, h: u32) -> bool {
        self.w != 0 && self.h != 0 && self.x <= w && self.y <= h && self.w <= w - self.x && self.h <= h - self.y
    }

    /// Clipped to a display of `w` x `h`.
    fn clip(&self, w: u32, h: u32) -> (u32, u32) {
        let cw = if self.x < w { self.w.min(w - self.x) } else { 0 };
        let ch = if self.y < h { self.h.min(h - self.y) } else { 0 };
        (cw, ch)
    }
}

/// Guest memory `[pa, pa + len)` as bytes, when it is all RAM.
///
/// The guest does not change a buffer it gave the device until the device answers (virtio's contract); a guest that
/// does anyway only gets wrong pixels: no Rust invariant hangs on a byte's value.
fn guest<'a>(pa: u64, len: u64) -> Option<&'a [u8]> {
    let p = xh_core::hooks::ram(pa, len)?;
    // SAFETY: the hook says the whole range is mapped guest RAM, mapped for the process's life (see above).
    Some(unsafe { std::slice::from_raw_parts(p.as_ptr(), len as usize) })
}

/// Copies `out.len()` bytes at linear `offset` of guest segments into `out`.
fn segments_read(entries: &[Entry], mut offset: u64, out: &mut [u8]) -> bool {
    let mut at = 0;
    for e in entries {
        if at == out.len() {
            break;
        }
        let size = e.length as u64;
        if offset >= size {
            offset -= size;
            continue;
        }
        let n = (size - offset).min((out.len() - at) as u64);
        let Some(p) = guest(e.addr + offset, n) else { return false };
        out[at..at + n as usize].copy_from_slice(p);
        at += n as usize;
        offset = 0;
    }
    at == out.len()
}

/// Byte order of a format's pixel in guest memory -> B,G,R,X.
fn convert(to: &mut [u8], from: &[u8], format: u32, pixels: usize) {
    let (to, from) = (&mut to[..pixels * 4], &from[..pixels * 4]);
    match format {
        FMT_BGRA | FMT_BGRX => to.copy_from_slice(from),
        FMT_RGBA | FMT_RGBX => {
            for (t, f) in to.chunks_exact_mut(4).zip(from.chunks_exact(4)) {
                t.copy_from_slice(&[f[2], f[1], f[0], f[3]]);
            }
        }
        FMT_ARGB | FMT_XRGB => {
            for (t, f) in to.chunks_exact_mut(4).zip(from.chunks_exact(4)) {
                t.copy_from_slice(&[f[3], f[2], f[1], f[0]]);
            }
        }
        _ => {
            // ABGR, XBGR
            for (t, f) in to.chunks_exact_mut(4).zip(from.chunks_exact(4)) {
                t.copy_from_slice(&[f[1], f[2], f[3], f[0]]);
            }
        }
    }
}

/// A row of a scanout format's pixels into the display's (`dformat`). Half floats into half floats are copied; into
/// 8 bits they are encoded to sRGB and clamped (what is outside sRGB and above white is lost, as in the guest's own CPU
/// conversion); 8-bit pixels into half floats are decoded to linear light.
fn convert_to(to: &mut [u8], dformat: u32, from: &[u8], format: u32, n: usize) {
    if format == FMT_RGBA16F {
        if dformat == RGBA16F {
            to[..n * 8].copy_from_slice(&from[..n * 8]);
        } else {
            pixels::rgba16f_to_8(&mut to[..n * 4], &from[..n * 8], false);
        }
        return;
    }
    if dformat != RGBA16F {
        convert(to, from, format, n);
        return;
    }
    if format == FMT_BGRA || format == FMT_BGRX {
        pixels::bgrx8_to_rgba16f(&mut to[..n * 8], &from[..n * 4]);
        return;
    }
    let mut row = [0u8; 4 * 256]; // another byte order: through B,G,R,X, 256 pixels at a time
    let mut done = 0;
    while done < n {
        let k = (n - done).min(256);
        convert(&mut row, &from[done * 4..], format, k);
        pixels::bgrx8_to_rgba16f(&mut to[done * 8..(done + k) * 8], &row[..k * 4]);
        done += k;
    }
}

// ---- what the device needs of the rest of xnuhost ---------------------------------------------------------------------
/// The display helper's answer to a posted frame: every frame up to this seq is in its layer (or was given up on).
pub type Done = Arc<dyn Fn(u64) + Send + Sync>;

/// What the device needs of the machine: the display's frame and helper (display.rs), the session's ready rule and
/// where the boot framebuffer is. The unit tests put a fake one here.
pub trait Board: Send + Sync {
    /// xnuhost's shadow of the visible frame (None: none yet).
    fn frame(&self) -> Option<Frame>;
    /// The rectangle changed (already copied into the frame).
    fn flush(&self, x: u32, y: u32, w: u32, h: u32);
    /// The DRM back end: `write(view, rect)` copies the frame's rectangle straight into the panel's next buffer (`view`:
    /// B,G,R,X 8-bit, the frame's size), which is then put on the panel (display.rs scanout_write). Some(whether the
    /// copy went); None: no such buffer, the caller copies into the frame and calls flush.
    fn scanout_write(&self, _damage: [u32; 4], _write: &mut dyn FnMut(&Frame, [u32; 4]) -> bool) -> Option<bool> {
        None
    }
    /// Whether a helper that copies frames from guest RAM itself is connected and has the RAM.
    fn direct(&self) -> bool;
    /// Hands the helper a frame (display.rs post); false: not posted, the caller copies it.
    #[allow(clippy::too_many_arguments)]
    fn post(
        &self,
        entries: &[Entry],
        offset: u64,
        stride: u32,
        width: u32,
        height: u32,
        rect: [u32; 4],
        seq: u64,
        done: &Done,
    ) -> bool;
    /// A frame of the guest's own was shown (the session's ready rule; `lit`: it has content). True once the guest is
    /// ready, after which it need not be called again.
    fn guest_frame(&self, lit: bool) -> bool;
    /// The guest's boot framebuffer (boot_args video), 0 when it has none.
    fn boot_framebuffer(&self) -> u64;
}

/// xnuhost's own: display.rs, the session, the host-call vector's boot arguments.
struct Machine;

impl Board for Machine {
    fn frame(&self) -> Option<Frame> {
        crate::display::get()
    }
    fn flush(&self, x: u32, y: u32, w: u32, h: u32) {
        crate::display::flush(x, y, w, h)
    }
    fn direct(&self) -> bool {
        crate::display::direct()
    }
    fn scanout_write(&self, damage: [u32; 4], write: &mut dyn FnMut(&Frame, [u32; 4]) -> bool) -> Option<bool> {
        let [x, y, w, h] = damage;
        crate::display::scanout_write((x, y, w, h), &mut |view, (x, y, w, h)| write(view, [x, y, w, h]))
    }
    fn post(
        &self,
        e: &[Entry],
        offset: u64,
        stride: u32,
        width: u32,
        height: u32,
        r: [u32; 4],
        seq: u64,
        done: &Done,
    ) -> bool {
        crate::display::post(e, offset, stride, width, height, r, seq, done)
    }
    fn guest_frame(&self, lit: bool) -> bool {
        crate::session::guest_frame(lit)
    }
    fn boot_framebuffer(&self) -> u64 {
        crate::hostcalls::info().video[0]
    }
}

// ---- the device's state -----------------------------------------------------------------------------------------------
struct Resource {
    id: u32,
    format: u32,
    width: u32,
    height: u32,
    /// host copy, in the display's pixels (dformat: B,G,R,X 8-bit or R,G,B,A half floats)
    pixels: Vec<u8>,
    dformat: u32,
    dbpp: u32,
    backing: Vec<Entry>,
}

/// A blob resource. Guest blobs (GUEST: the display's surfaces; HOST3D_GUEST: a context's BOs) keep the guest's
/// segments, so a blob can be a scanout; HOST3D blobs (a context's shmem) are the renderer's alone.
struct Blob {
    mem: u32,
    size: u64,
    entries: Vec<Entry>,
}

#[derive(Clone, Copy)]
struct Ctx {
    id: u32,
    /// that context's commands are the fast lane's (the render server's: "xnu:backboardd")
    fast: bool,
}

/// The scanout when it is a blob.
#[derive(Clone, Copy, Default)]
struct Scan {
    width: u32,
    height: u32,
    format: u32,
    stride: u32,
    offset: u32,
}

/// A flush of the blob scanout handed to the display helper: set by flush_blob, posted by work() once the command's
/// reply is held as the display's (ctx 0, which no context has), and answered when the helper has the frame.
#[derive(Clone, Copy)]
struct Post {
    blob: u32,
    rect: Rect,
    seq: u64,
}

/// The copy of each scanout flush, one log line per 10 s while frames come: `display: 10 s: flushes N, copy mean X us
/// max Y us (Z MiB)` (also display.rs's DRM copy thread's, the same line).
#[derive(Default)]
pub(crate) struct FlushNote {
    since: u64,
    n: u64,
    sum: u64,
    max: u64,
    total: u64,
}

impl FlushNote {
    /// One copy of `bytes` that took `took` us.
    pub(crate) fn note(&mut self, took: u64, bytes: u64) {
        let now = time::monotonic_ns() / 1000;
        if self.since == 0 {
            self.since = now;
        }
        self.n += 1;
        self.sum += took;
        self.total += bytes;
        self.max = self.max.max(took);
        if now - self.since >= 10_000_000 {
            logi!(
                "display: {} s: flushes {}, copy mean {} us max {} us ({} MiB)",
                (now - self.since) / 1_000_000,
                self.n,
                self.sum / self.n,
                self.max,
                self.total >> 20
            );
            *self = FlushNote { since: now, ..FlushNote::default() };
        }
    }
}

/// A B,G,R,X/A blob scanout's rectangle `r` (x, y, w, h) from the guest's pages (`entries`; rows of `stride` bytes
/// from byte `offset`; the scanout `width` x `height`) into `dst` (B,G,R,X 8-bit) at the same coordinates, clipped to
/// both. For display.rs's DRM copy thread. False: a segment is not guest RAM, or the blob is shorter than the rows.
pub(crate) fn blob_rows_bgrx(
    entries: &[Entry],
    offset: u64,
    stride: u32,
    (width, height): (u32, u32),
    r: [u32; 4],
    dst: &Frame,
) -> bool {
    let Ok(offset) = u32::try_from(offset) else { return false };
    let scan = Scan { width, height, format: FMT_BGRX, stride, offset };
    let rect = Rect { x: r[0], y: r[1], w: r[2], h: r[3] };
    let (w, h) = rect.clip(width.min(dst.width), height.min(dst.height));
    if w == 0 || h == 0 {
        return true;
    }
    Gpu::rows_into(&scan, entries, &Rect { x: rect.x, y: rect.y, w, h }, dst, BGRX8) == RESP_OK_NODATA
}

/// What the command lanes share (C: everything `cmd` guards).
struct Core {
    res: Vec<Resource>,
    scanout_resource: u32,
    first_frame: bool,
    /// 3D: the blobs (None: the 2D device)
    blobs: Option<HashMap<u32, Blob>>,
    ctx: Vec<Ctx>,
    // The blobs the host holds (2026-11-13): bytes of the live ones, made and unref'd since the start, and the last
    // line's figures (blob_note).
    blob_bytes: u64,
    blobs_made: u64,
    blobs_unref: u64,
    blob_note_us: u64,
    noted_made: u64,
    noted_unref: u64,
    scan: Scan,
    post: Option<Post>,
    display_seq: u64,
    flush_note: FlushNote,
    /// a command's bytes (whole words: drm2kgsl reads the stream as words in place) and its response, kept
    req: Vec<u32>,
    resp: Vec<u8>,
    row: Vec<u8>,
}

/// A fenced command whose chain goes back to the guest when its context's ring passes the fence.
#[derive(Clone, Copy)]
struct Pending {
    head: u16,
    written: u32,
    ctx: u32,
    ring: u32,
    fence: u64,
    gen: u64,
}

struct SlowJob {
    head: u16,
    gen: u64,
    iov: Vec<Viov>,
}

/// The pending fences and the lanes' bookkeeping (C: `pl`).
#[derive(Default)]
struct Lanes {
    fences: Vec<Pending>,
    kicks: u64,
    served: u64,
    reset_requested: bool,
    fast_waiting: u32,
    slow: VecDeque<SlowJob>,
    slow_queued: u64,
    slow_done: u64,
}

/// The virtio-gpu device.
pub struct Gpu {
    width: u32,
    height: u32,
    b: Option<Box<dyn Gpu3d>>,
    board: Arc<dyn Board>,
    core: Mutex<Core>,
    lanes: Mutex<Lanes>,
    idle: Condvar,
    slow_cv: Condvar,
    /// bumped at every device reset, under the device's lock
    gen: AtomicU64,
    /// eventfd: the control queue was kicked (written from the trap handler)
    wake: Option<OwnedFd>,
    dev: OnceLock<Weak<Virtio<Gpu>>>,
    done: OnceLock<Done>,
}

/// The board's virtio-gpu device.
pub type GpuDev = Arc<Virtio<Gpu>>;

static HALF_SAID: AtomicU32 = AtomicU32::new(0);
/// RESOURCE_CREATE_BLOBs refused as INVALID_PARAMETER (create_blob's line).
static BLOB_REFUSED: AtomicU32 = AtomicU32::new(0);

/// A view of `v`'s bytes.
fn words_as_bytes(v: &mut [u32]) -> &mut [u8] {
    // SAFETY: u32 and u8 have no invalid values; the byte view covers exactly the words' memory, borrowed mutably.
    unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr().cast(), v.len() * 4) }
}

impl Core {
    fn find(&self, id: u32) -> Option<usize> {
        if id == 0 {
            return None;
        }
        self.res.iter().position(|r| r.id == id)
    }

    fn blob(&self, id: u32) -> Option<&Blob> {
        self.blobs.as_ref()?.get(&id)
    }

    fn ctx_find(&self, id: u32) -> Option<usize> {
        if id == 0 {
            return None;
        }
        self.ctx.iter().position(|c| c.id == id)
    }

    fn blob_del(&mut self, id: u32) {
        if let Some(b) = self.blobs.as_mut().and_then(|t| t.remove(&id)) {
            self.blob_bytes -= b.size;
        }
    }

    /// The blobs the host holds, one line per 10 s while they change: `gpu: 10 s: host blobs N (M MiB), made +A unref'd
    /// +B, refused R` (R: the CREATE_BLOBs refused since the start; the first 32 each have a `virtio-gpu: blob ...
    /// refused` line naming the check). Against the guest's `oneios-gpu-memory` live count (and the sum of its own and
    /// wired MiB): the host holding more than the guest has live is blobs the guest released and the host kept; the same
    /// is the guest's own holding.
    fn blob_note(&mut self) {
        let now = time::monotonic_ns() / 1000;
        if self.blob_note_us == 0 {
            self.blob_note_us = now;
        }
        if now - self.blob_note_us < 10_000_000 {
            return;
        }
        if self.blobs_made != self.noted_made || self.blobs_unref != self.noted_unref {
            logi!(
                "gpu: {} s: host blobs {} ({} MiB), made +{} unref'd +{}, refused {}",
                (now - self.blob_note_us) / 1_000_000,
                self.blobs.as_ref().map_or(0, |t| t.len()),
                self.blob_bytes >> 20,
                self.blobs_made - self.noted_made,
                self.blobs_unref - self.noted_unref,
                BLOB_REFUSED.load(Ordering::Relaxed)
            );
        }
        self.blob_note_us = now;
        self.noted_made = self.blobs_made;
        self.noted_unref = self.blobs_unref;
    }

    /// The copy of each scanout flush, one log line per 10 s while frames come: `display: 10 s: flushes N, copy mean X
    /// us max Y us (Z MiB)`. The device's one thread does it, so the guest's next commands (a frame's submissions, its
    /// blobs) wait behind it.
    fn flush_note(&mut self, took: u64, bytes: u64) {
        self.flush_note.note(took, bytes);
    }
}

impl Gpu {
    fn dev(&self) -> Option<Arc<Virtio<Gpu>>> {
        self.dev.get()?.upgrade()
    }

    /// The display's pixels (display-format=): what a resource's host copy and a blob's flush are written in.
    fn display_format(&self) -> u32 {
        // 0: a display made before formats (B,G,R,X)
        self.board.frame().map_or(BGRX8, |d| if d.format != 0 { d.format } else { BGRX8 })
    }

    // ---- the ready rule ---------------------------------------------------------------------------------------------
    /// xh_pixels_lit of a B,G,R,X (or B,G,R,A) blob scanout read where it lies, in the guest's pages: a frame the
    /// display helper copies itself (the direct path) never reaches the display's own buffer, so that buffer cannot say
    /// whether it is lit. Before this, every session with direct frames read the stale buffer as black and
    /// session/ready waited the whole ready-content-wait (out/session-logs/2026-11-22/kbd-debug: the first frame at
    /// 4768.157 s, ready at 4888.240 s, "no frame with content in 120 s" while the helper showed iOS's frames). The same
    /// grid and level as pixels::lit.
    fn blob_lit(scan: &Scan, entries: &[Entry]) -> bool {
        let (w, h) = (scan.width, scan.height);
        let x0 = if w >= LIT_STEP { LIT_STEP / 2 } else { w / 2 };
        let y0 = if h >= LIT_STEP { LIT_STEP / 2 } else { h / 2 };
        let (mut seg, mut seg_start) = (0usize, 0u64); // the blob offset at which entries[seg] begins
        for y in (y0..h).step_by(LIT_STEP as usize) {
            for x in (x0..w).step_by(LIT_STEP as usize) {
                // rising: the segments walked once
                let at = scan.offset as u64 + y as u64 * scan.stride as u64 + x as u64 * 4;
                while seg < entries.len() && at >= seg_start + entries[seg].length as u64 {
                    seg_start += entries[seg].length as u64;
                    seg += 1;
                }
                if seg >= entries.len() {
                    return false;
                }
                if at + 4 > seg_start + entries[seg].length as u64 {
                    continue; // a pixel split between segments
                }
                if let Some(p) = guest(entries[seg].addr + (at - seg_start), 4) {
                    if p[0] > LIT_LEVEL || p[1] > LIT_LEVEL || p[2] > LIT_LEVEL {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// The guest's display driver also scans out the boot framebuffer itself (DroidVMVirtioGpuDisplay.cpp's `boot`
    /// resource: the kernel console until the compositor's first frame, and later whenever a blank or CPU-copied frame
    /// was drawn there). That picture is shown like any other, but it is the console's. A frame of a surface of the
    /// guest's own goes to the session's ready rule (ready.rs) with whether the display's frame now shows anything:
    /// backboardd's first frames are black, and the guest is ready (session/ready) at the first that is not
    /// (2026-11-15). Called before the frame is posted to the display helper, so that session/ready is there when the
    /// helper sees the frame. `first_frame` stops the calls once the rule has said yes.
    fn frame_shown(&self, c: &mut Core, entries: &[Entry], direct: bool) {
        let boot = self.board.boot_framebuffer();
        if c.first_frame || (!entries.is_empty() && boot != 0 && entries[0].addr == boot) {
            return;
        }
        let lit = if direct { Gpu::blob_lit(&c.scan, entries) } else { self.board.frame().is_some_and(|d| d.lit()) };
        if self.board.guest_frame(lit) {
            c.first_frame = true;
        }
    }

    // ---- flushes ---------------------------------------------------------------------------------------------------
    /// The rows of a blob scanout's rectangle (clipped to the display) into xnuhost's frame, then the helper is told.
    /// B,G,R,X/A (the guest's display driver's format) goes straight from the guest's pages into the frame.
    fn flush_copy(&self, c: &mut Core, entries: &[Entry], rect: &Rect) -> u32 {
        let Some(d) = self.board.frame() else { return RESP_OK_NODATA };
        let same = c.scan.format == FMT_BGRX || c.scan.format == FMT_BGRA;
        // SAFETY: the device's lanes (one at a time, under `core`) are the frame's only writer.
        let px = unsafe { d.bytes_mut() };
        let mut row = std::mem::take(&mut c.row);
        row.resize(rect.w as usize * 4 + 4, 0);
        let mut status = RESP_OK_NODATA;
        for y in 0..rect.h {
            let to = (rect.y + y) as usize * d.stride as usize + rect.x as usize * 4;
            let n = rect.w as usize * 4;
            let at = c.scan.offset as u64 + (rect.y + y) as u64 * c.scan.stride as u64 + rect.x as u64 * 4;
            let into = if same { &mut px[to..to + n] } else { &mut row[..n] };
            if !segments_read(entries, at, into) {
                status = RESP_ERR_INVALID_PARAMETER;
                break;
            }
            if !same {
                convert(&mut px[to..to + n], &row, c.scan.format, rect.w as usize);
            }
        }
        c.row = row;
        if status == RESP_OK_NODATA && rect.w != 0 && rect.h != 0 {
            self.board.flush(rect.x, rect.y, rect.w, rect.h);
        }
        status
    }

    /// RESOURCE_FLUSH of a blob: when it is the scanout (SET_SCANOUT_BLOB), its rows go from the guest's pages to the
    /// display: handed to a helper that copies them itself (work() posts it), else copied here.
    fn flush_blob(&self, c: &mut Core, id: u32, rect: &Rect) -> u32 {
        let Some(d) = self.board.frame() else { return RESP_OK_NODATA };
        if id != c.scanout_resource || c.scan.width == 0 {
            return RESP_OK_NODATA;
        }
        if !rect.ok(c.scan.width, c.scan.height) {
            return RESP_ERR_INVALID_PARAMETER;
        }
        let (w, hgt) = rect.clip(d.width, d.height);
        if w == 0 || hgt == 0 {
            return RESP_OK_NODATA;
        }
        let entries = std::mem::take(&mut c.blobs.as_mut().unwrap().get_mut(&id).unwrap().entries);
        let status = self.flush_blob_rows(c, &d, &entries, id, rect, w, hgt);
        c.blobs.as_mut().unwrap().get_mut(&id).unwrap().entries = entries;
        status
    }

    #[allow(clippy::too_many_arguments)]
    fn flush_blob_rows(
        &self,
        c: &mut Core,
        d: &Frame,
        entries: &[Entry],
        id: u32,
        rect: &Rect,
        w: u32,
        hgt: u32,
    ) -> u32 {
        crate::frame_path::flush(time::monotonic_ns() / 1000);
        let clipped = Rect { x: rect.x, y: rect.y, w, h: hgt };
        if self.b.is_some() && (c.scan.format == FMT_BGRX || c.scan.format == FMT_BGRA) && self.board.direct() {
            c.display_seq += 1;
            c.post = Some(Post { blob: id, rect: clipped, seq: c.display_seq });
            self.frame_shown(c, entries, true);
            return RESP_OK_NODATA;
        }
        let t0 = time::monotonic_ns() / 1000;
        let gbpp = format_bpp(c.scan.format) as u64;
        // The DRM back end: the rows go straight into the panel's next buffer (one copy a frame, not two: until
        // 2026-11-28 into xnuhost's frame, then from it into the buffer, and the flush waited for the flip before it).
        // The rectangle copied is the damage plus what that buffer missed; clipped to the scanout.
        let scan = c.scan;
        let mut copied = 0u64;
        let mut status = RESP_OK_NODATA;
        let drm = self.board.scanout_write([rect.x, rect.y, w, hgt], &mut |view, [x, y, rw, rh]| {
            let r = Rect { x, y, w: rw, h: rh };
            let (rw, rh) = r.clip(scan.width.min(view.width), scan.height.min(view.height));
            if rw == 0 || rh == 0 {
                return true;
            }
            copied = rw as u64 * rh as u64 * gbpp;
            status = Gpu::rows_into(&scan, entries, &Rect { x, y, w: rw, h: rh }, view, BGRX8);
            status == RESP_OK_NODATA
        });
        if drm.is_some() {
            c.flush_note(time::monotonic_ns() / 1000 - t0, copied);
            if status == RESP_OK_NODATA {
                self.frame_shown(c, entries, true); // xnuhost's frame was not written: the guest's pages say
            }
            return status;
        }
        let status = Gpu::rows_into(&c.scan, entries, &clipped, d, self.display_format());
        c.flush_note(time::monotonic_ns() / 1000 - t0, w as u64 * hgt as u64 * gbpp);
        if status == RESP_OK_NODATA {
            self.frame_shown(c, entries, false); // first: session/ready before the helper is told of the frame
            self.board.flush(rect.x, rect.y, w, hgt);
        }
        status
    }

    /// The rows of a blob scanout's rectangle `rect` (inside the scanout and `d`) from the guest's pages into `d`, in
    /// `dformat`, the blob's segments walked once for the whole rectangle: rows lie at rising offsets. (Each row used to
    /// be read into a buffer of its own, the segment list searched from its start for every row: 3120 rows of a
    /// 1440x3120 frame over about 1100 segments of 16 KiB pages.) A half-float scanout is 8 bytes a pixel in the guest.
    fn rows_into(scan: &Scan, entries: &[Entry], rect: &Rect, d: &Frame, dformat: u32) -> u32 {
        let (w, hgt) = (rect.w, rect.h);
        let gbpp = format_bpp(scan.format) as u64;
        let dbpp = pixels::bpp(dformat) as usize;
        // SAFETY: the device's lanes (one at a time, under `core`) are the frame's only writer; a DRM buffer's view is
        // written only by the writer display.rs gave it to.
        let px = unsafe { d.bytes_mut() };
        // The same layout in the guest and in `d` (half floats into half floats, B,G,R,X/A into B,G,R,X): a row is its
        // bytes, copied segment by segment, a pixel split between two segments too (no carry, no conversion call per
        // segment; YMGPwcca/OneiOS 8be9ec18). Every other pair goes through convert_to.
        let raw = (scan.format == FMT_RGBA16F && dformat == RGBA16F)
            || ((scan.format == FMT_BGRX || scan.format == FMT_BGRA) && dformat == BGRX8);
        let (mut seg, mut seg_start) = (0usize, 0u64); // the blob offset at which entries[seg] begins
        for y in 0..hgt {
            let mut at = scan.offset as u64 + (rect.y + y) as u64 * scan.stride as u64 + rect.x as u64 * gbpp;
            let mut left = w as u64 * gbpp;
            let mut to = (rect.y + y) as usize * d.stride as usize + rect.x as usize * dbpp;
            let mut carry = [0u8; 8]; // a pixel split between two segments
            let mut have = 0usize;
            if at < seg_start {
                (seg, seg_start) = (0, 0); // not expected: rows only rise
            }
            while left > 0 {
                while seg < entries.len() && at >= seg_start + entries[seg].length as u64 {
                    seg_start += entries[seg].length as u64;
                    seg += 1;
                }
                if seg >= entries.len() {
                    return RESP_ERR_INVALID_PARAMETER;
                }
                let inside = at - seg_start;
                let n = (entries[seg].length as u64 - inside).min(left);
                let Some(from) = guest(entries[seg].addr + inside, n).filter(|_| n & 3 == 0) else {
                    return RESP_ERR_INVALID_PARAMETER;
                };
                if raw {
                    px[to..to + n as usize].copy_from_slice(from);
                    to += n as usize;
                    at += n;
                    left -= n;
                    continue;
                }
                let mut off = 0usize;
                if have != 0 {
                    off = (gbpp as usize - have).min(n as usize);
                    carry[have..have + off].copy_from_slice(&from[..off]);
                    have += off;
                    if have == gbpp as usize {
                        convert_to(&mut px[to..], dformat, &carry, scan.format, 1);
                        to += dbpp;
                        have = 0;
                    }
                }
                let whole = (n as usize - off) / gbpp as usize;
                let rest = n as usize - off - whole * gbpp as usize;
                convert_to(&mut px[to..], dformat, &from[off..], scan.format, whole);
                to += whole * dbpp;
                if rest != 0 {
                    let from_rest = off + whole * gbpp as usize;
                    carry[..rest].copy_from_slice(&from[from_rest..from_rest + rest]);
                    have = rest;
                }
                at += n;
                left -= n;
            }
        }
        RESP_OK_NODATA
    }

    // ---- fences ----------------------------------------------------------------------------------------------------
    fn push_pending(&self, p: &Pending) {
        let Some(dev) = self.dev() else { return };
        let _g = dev.lock();
        if p.gen == self.gen.load(Ordering::Acquire) {
            dev.push(0, p.head, p.written); // a chain from before a reset is the old queue's
        }
    }

    /// Takes the pending fences that match off the list and returns their commands. `ring` None: every ring of the
    /// context.
    fn retire(&self, ctx: u32, ring: Option<u32>, fence: u64) {
        let done: Vec<Pending> = {
            let mut l = lock(&self.lanes);
            let (done, kept): (Vec<Pending>, Vec<Pending>) =
                l.fences.iter().partition(|p| p.ctx == ctx && ring.is_none_or(|r| p.ring == r && p.fence <= fence));
            l.fences = kept;
            done
        };
        for p in &done {
            self.push_pending(p);
        }
    }

    fn hold(&self, p: Pending) {
        lock(&self.lanes).fences.push(p);
    }

    // ---- the commands ----------------------------------------------------------------------------------------------
    /// The 3D commands and the blob ones. None: not one of them (command() goes on to the 2D ones).
    fn command3d(&self, c: &mut Core, h: &Hdr, body: &[u8], resp: &mut [u8], resp_size: &mut usize) -> Option<u32> {
        let b = self.b.as_deref();
        let left = body.len();
        let resp_max = resp.len();
        let a = |i: usize| le::u32(body, i * 4);
        Some(match h.kind {
            CMD_GET_CAPSET_INFO => {
                if left < 8 || resp_max < HDR + 16 {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                let mut info = [0u32; 4];
                if let Some(b) = b.filter(|b| a(0) < b.capset_count()) {
                    if let Some((id, version, size)) = b.capset_info(a(0)) {
                        info[..3].copy_from_slice(&[id, version, size]);
                    }
                }
                for (i, v) in info.iter().enumerate() {
                    resp[HDR + i * 4..HDR + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                *resp_size = HDR + 16;
                RESP_OK_CAPSET_INFO
            }
            CMD_GET_CAPSET => {
                if left < 8 {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                let n = b.map_or(0, |b| b.capset(a(0), a(1), &mut resp[HDR..]));
                if n == 0 {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                *resp_size = HDR + n;
                RESP_OK_CAPSET
            }
            CMD_CTX_CREATE => {
                if left < 8 {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                let name_len = (left - 8).min(64).min(a(0) as usize);
                let Some(b) = b.filter(|_| h.ctx != 0 && c.ctx_find(h.ctx).is_none()) else {
                    return Some(RESP_ERR_INVALID_CONTEXT_ID);
                };
                if c.ctx.len() >= MAX_CTX {
                    return Some(RESP_ERR_OUT_OF_MEMORY);
                }
                let name = &body[8..8 + name_len];
                if let Err(e) = b.ctx_create(h.ctx, a(1) & 0xff, name) {
                    return Some(if e.0 == libc::ENOMEM { RESP_ERR_OUT_OF_MEMORY } else { RESP_ERR_UNSPEC });
                }
                c.ctx.push(Ctx { id: h.ctx, fast: name.starts_with(b"xnu:backboardd") });
                // context_init's bits 8-23: the creating task's role and its thread's QoS (OneiOSAdreno.cpp
                // oneios_gpu_hint).
                b.hint(h.ctx, [(a(1) >> 8) as u8, (a(1) >> 16) as u8, 0]);
                RESP_OK_NODATA
            }
            CMD_CTX_DESTROY => {
                let Some(i) = c.ctx_find(h.ctx) else { return Some(RESP_ERR_INVALID_CONTEXT_ID) };
                if let Some(b) = b {
                    b.ctx_destroy(h.ctx);
                }
                c.ctx.remove(i);
                self.retire(h.ctx, None, 0); // its rings are gone: nothing would retire these
                RESP_OK_NODATA
            }
            CMD_SUBMIT_3D => {
                if left < 8 {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                if c.ctx_find(h.ctx).is_none() {
                    return Some(RESP_ERR_INVALID_CONTEXT_ID);
                }
                let size = a(0) as usize;
                if size == 0 || size > left - 8 {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                let b = b?;
                b.hint(h.ctx, h.pad); // the submitting task's role, QoS and frame deadline
                if b.submit(h.ctx, &body[8..8 + size]).is_err() {
                    RESP_ERR_UNSPEC
                } else {
                    RESP_OK_NODATA
                }
            }
            CMD_RESOURCE_CREATE_BLOB => return Some(self.create_blob(c, h, body)),
            CMD_RESOURCE_MAP_BLOB => {
                if left < 16 || resp_max < HDR + 8 {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                let Some(bl) = c.blob(a(0)) else { return Some(RESP_ERR_INVALID_RESOURCE_ID) };
                // Only what lies in the pool can be mapped: the guest maps pool base + offset from its own RAM.
                let mapped = b.filter(|_| bl.mem == gpu3d::BLOB_HOST3D).and_then(|b| b.map_blob(a(0)).ok());
                let Some((offset, info)) = mapped.filter(|&(o, _)| o <= u32::MAX as u64) else {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                };
                resp[HDR..HDR + 4].copy_from_slice(&(info | MAP_INFO_POOL).to_le_bytes());
                resp[HDR + 4..HDR + 8].copy_from_slice(&(offset as u32).to_le_bytes());
                *resp_size = HDR + 8;
                RESP_OK_MAP_INFO
            }
            CMD_RESOURCE_UNMAP_BLOB => {
                if left < 4 {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                if c.blob(a(0)).is_some() {
                    RESP_OK_NODATA
                } else {
                    RESP_ERR_INVALID_RESOURCE_ID
                }
            }
            CMD_CTX_ATTACH_RESOURCE | CMD_CTX_DETACH_RESOURCE => {
                if left < 4 {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                if c.ctx_find(h.ctx).is_none() {
                    return Some(RESP_ERR_INVALID_CONTEXT_ID);
                }
                if c.blob(a(0)).is_none() {
                    return Some(RESP_ERR_INVALID_RESOURCE_ID);
                }
                match b?.attach(h.ctx, a(0), h.kind == CMD_CTX_ATTACH_RESOURCE) {
                    Ok(()) => RESP_OK_NODATA,
                    Err(_) => RESP_ERR_UNSPEC,
                }
            }
            CMD_RESOURCE_UNREF => {
                if left < 4 {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                let id = a(0);
                c.blob(id)?; // a 2D resource, or nothing
                if c.scanout_resource == id {
                    c.scanout_resource = 0;
                    c.scan.width = 0;
                }
                if let Some(b) = b {
                    b.unref(id);
                }
                c.blob_del(id);
                c.blobs_unref += 1;
                RESP_OK_NODATA
            }
            CMD_SET_SCANOUT_BLOB => {
                // scanout, resource, width, height, format, padding, strides[4], offsets[4]
                if left < 16 + 56 {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                let rect = Rect::decode(body);
                let v = |i: usize| le::u32(body, 16 + i * 4);
                if v(0) != 0 {
                    return Some(RESP_ERR_INVALID_SCANOUT_ID);
                }
                if v(1) == 0 {
                    c.scanout_resource = 0;
                    c.scan.width = 0;
                    return Some(RESP_OK_NODATA);
                }
                let Some(bl) = c.blob(v(1)).filter(|bl| !bl.entries.is_empty()) else {
                    return Some(RESP_ERR_INVALID_RESOURCE_ID);
                };
                let (w, hh, format, stride, offset) = (v(2), v(3), v(4), v(6), v(10));
                if !format_known(format) {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                let row = w as u64 * format_bpp(format) as u64;
                if w == 0
                    || hh == 0
                    || w > 16384
                    || hh > 16384
                    || (stride as u64) < row
                    || stride % format_bpp(format) != 0
                    || !rect.ok(w, hh)
                    || offset as u64 + (hh as u64 - 1) * stride as u64 + row > bl.size
                {
                    return Some(RESP_ERR_INVALID_PARAMETER);
                }
                if format == FMT_RGBA16F
                    && c.scan.format != FMT_RGBA16F
                    && HALF_SAID.fetch_add(1, Ordering::Relaxed) < 4
                {
                    logi!(
                        "display: the guest scans out half floats (R,G,B,A 16F, {}x{}, stride {}) onto a {} frame",
                        w,
                        hh,
                        stride,
                        pixels::name(self.display_format())
                    );
                }
                c.scanout_resource = v(1);
                c.scan = Scan { width: w, height: hh, format, stride, offset };
                RESP_OK_NODATA
            }
            _ => return None,
        })
    }

    fn create_blob(&self, c: &mut Core, h: &Hdr, body: &[u8]) -> u32 {
        let b = self.b.as_deref();
        let left = body.len();
        if left < 32 {
            return RESP_ERR_INVALID_PARAMETER;
        }
        let a = |i: usize| le::u32(body, i * 4);
        let (res, mem, flags, n) = (a(0), a(1), a(2), a(3)); // resource, blob_mem, blob_flags, nr_entries
        let (blob_id, size) = (le::u64(body, 16), le::u64(body, 24));
        if res == 0 || c.find(res).is_some() || c.blob(res).is_some() {
            return RESP_ERR_INVALID_RESOURCE_ID;
        }
        let guest_mem = mem == gpu3d::BLOB_GUEST || mem == gpu3d::BLOB_HOST3D_GUEST;
        let host = mem == gpu3d::BLOB_HOST3D || mem == gpu3d::BLOB_HOST3D_GUEST;
        // A refused blob is one line naming the check (the guest only logs `command 10c response 1205`; the phone's
        // 2026-11-23 13:51 session had two before the abort of drm2kgsl patch 0009, and nothing said why). Rare: the
        // first 32, then every 1024th.
        let refuse = |why: std::fmt::Arguments| {
            let k = BLOB_REFUSED.fetch_add(1, Ordering::Relaxed);
            if k < 32 || k % 1024 == 0 {
                logi!(
                    "virtio-gpu: blob {} of context {} refused ({} so far): mem {} flags {:#x}, {} bytes, {} entries: {}",
                    res,
                    h.ctx,
                    k + 1,
                    mem,
                    flags,
                    size,
                    n,
                    why
                );
            }
            RESP_ERR_INVALID_PARAMETER
        };
        if !guest_mem && !host {
            return refuse(format_args!("no such blob memory"));
        }
        let max = BLOB_ENTRIES.load(Ordering::Relaxed);
        if size == 0 || n > max || (if guest_mem { n == 0 } else { n != 0 }) {
            return refuse(format_args!("size or entry count (at most {max}; guest memory needs entries)"));
        }
        if left - 32 < n as usize * 16 {
            return refuse(format_args!("the request holds {} bytes of entries, not {}", left - 32, n as usize * 16));
        }
        if host && c.ctx_find(h.ctx).is_none() {
            return RESP_ERR_INVALID_CONTEXT_ID;
        }
        let mut entries = Vec::new();
        if guest_mem {
            // The segments are guest RAM, whole host pages each (udmabuf takes nothing else), and cover the blob.
            entries.reserve_exact(n as usize);
            let mut total = 0u64;
            for i in 0..n as usize {
                let e = Entry::decode(&body[32 + i * 16..]);
                if e.length == 0 || (e.addr | e.length as u64) & 0xfff != 0 {
                    return refuse(format_args!(
                        "entry {i} at {:#x}, {} bytes, is not whole 4 KiB pages",
                        e.addr, e.length
                    ));
                }
                if xh_core::hooks::ram(e.addr, e.length as u64).is_none() {
                    return refuse(format_args!("entry {i} at {:#x}, {} bytes, is not guest RAM", e.addr, e.length));
                }
                total += e.length as u64;
                entries.push(e);
            }
            if total < size {
                return refuse(format_args!("the entries hold {total} bytes"));
            }
        }
        if c.blobs.as_ref().is_some_and(|t| t.len() >= MAX_BLOBS) {
            return RESP_ERR_OUT_OF_MEMORY;
        }
        // The renderer makes every blob of a context, and a display blob it may be asked to attach to one later.
        let made = match b {
            Some(b) if host || flags & gpu3d::BLOB_FLAG_GUEST_HANDLE != 0 => {
                b.create_blob(if host { h.ctx } else { 0 }, res, mem, flags, blob_id, size, &entries)
            }
            _ => Ok(()),
        };
        if let (Err(e), true) = (made, host) {
            return if e.0 == libc::ENOMEM { RESP_ERR_OUT_OF_MEMORY } else { RESP_ERR_UNSPEC };
        }
        if let Some(t) = c.blobs.as_mut() {
            t.insert(res, Blob { mem, size, entries });
        }
        c.blob_bytes += size;
        c.blobs_made += 1;
        RESP_OK_NODATA
    }

    fn command(&self, c: &mut Core, req: &[u8], resp: &mut [u8], resp_size: &mut usize) -> u32 {
        let h = Hdr::decode(req);
        let body = &req[HDR..];
        let left = body.len();
        let resp_max = resp.len();
        *resp_size = HDR;
        if c.blobs.is_some() {
            c.blob_note();
            if let Some(status) = self.command3d(c, &h, body, resp, resp_size) {
                return status;
            }
        }
        let a = |i: usize| le::u32(body, i * 4);
        match h.kind {
            CMD_GET_DISPLAY_INFO => {
                if resp_max < HDR + 16 * 24 {
                    return RESP_ERR_UNSPEC;
                }
                resp[HDR..HDR + 16 * 24].fill(0);
                for (i, v) in [0, 0, self.width, self.height, 1, 0].iter().enumerate() {
                    resp[HDR + i * 4..HDR + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                *resp_size = HDR + 16 * 24;
                RESP_OK_DISPLAY_INFO
            }
            CMD_RESOURCE_CREATE_2D => {
                if left < 16 {
                    return RESP_ERR_INVALID_PARAMETER;
                }
                let (id, format, w, hh) = (a(0), a(1), a(2), a(3));
                if id == 0 || c.find(id).is_some() || c.blob(id).is_some() {
                    return RESP_ERR_INVALID_RESOURCE_ID;
                }
                if w == 0 || hh == 0 || w > 16384 || hh > 16384 || !format_known(format) {
                    return RESP_ERR_INVALID_PARAMETER;
                }
                if c.res.len() >= MAX_RES {
                    return RESP_ERR_OUT_OF_MEMORY;
                }
                let dformat = self.display_format();
                let dbpp = pixels::bpp(dformat);
                let mut pixels = Vec::new();
                if pixels.try_reserve_exact(w as usize * hh as usize * dbpp as usize).is_err() {
                    return RESP_ERR_OUT_OF_MEMORY;
                }
                pixels.resize(w as usize * hh as usize * dbpp as usize, 0);
                c.res.push(Resource { id, format, width: w, height: hh, pixels, dformat, dbpp, backing: Vec::new() });
                RESP_OK_NODATA
            }
            CMD_RESOURCE_UNREF => {
                if left < 4 {
                    return RESP_ERR_INVALID_PARAMETER;
                }
                let Some(i) = c.find(a(0)) else { return RESP_ERR_INVALID_RESOURCE_ID };
                if c.scanout_resource == a(0) {
                    c.scanout_resource = 0;
                }
                c.res.swap_remove(i);
                RESP_OK_NODATA
            }
            CMD_RESOURCE_ATTACH_BACKING => {
                if left < 8 {
                    return RESP_ERR_INVALID_PARAMETER;
                }
                let Some(i) = c.find(a(0)) else { return RESP_ERR_INVALID_RESOURCE_ID };
                let n = a(1);
                if n == 0 || n > MAX_BACKING || left - 8 < n as usize * 16 {
                    return RESP_ERR_INVALID_PARAMETER;
                }
                c.res[i].backing = (0..n as usize).map(|k| Entry::decode(&body[8 + k * 16..])).collect();
                RESP_OK_NODATA
            }
            CMD_RESOURCE_DETACH_BACKING => {
                if left < 4 {
                    return RESP_ERR_INVALID_PARAMETER;
                }
                let Some(i) = c.find(a(0)) else { return RESP_ERR_INVALID_RESOURCE_ID };
                c.res[i].backing = Vec::new();
                RESP_OK_NODATA
            }
            CMD_SET_SCANOUT => {
                if left < 24 {
                    return RESP_ERR_INVALID_PARAMETER;
                }
                let rect = Rect::decode(body);
                let (scanout, id) = (a(4), a(5));
                if scanout != 0 {
                    return RESP_ERR_INVALID_SCANOUT_ID;
                }
                if id == 0 {
                    c.scanout_resource = 0; // scanout disabled
                    return RESP_OK_NODATA;
                }
                let Some(i) = c.find(id) else { return RESP_ERR_INVALID_RESOURCE_ID };
                if !rect.ok(c.res[i].width, c.res[i].height) {
                    return RESP_ERR_INVALID_PARAMETER;
                }
                c.scanout_resource = id;
                c.scan.width = 0;
                RESP_OK_NODATA
            }
            CMD_TRANSFER_TO_HOST_2D => {
                if left < 28 {
                    return RESP_ERR_INVALID_PARAMETER;
                }
                let rect = Rect::decode(body);
                let (offset, id) = (le::u64(body, 16), a(6));
                let Some(i) = c.find(id) else { return RESP_ERR_INVALID_RESOURCE_ID };
                let mut row = std::mem::take(&mut c.row);
                let r = &mut c.res[i];
                if !rect.ok(r.width, r.height) || r.backing.is_empty() {
                    c.row = row;
                    return RESP_ERR_INVALID_PARAMETER;
                }
                let gbpp = format_bpp(r.format) as u64;
                row.resize(rect.w as usize * gbpp as usize, 0);
                let mut status = RESP_OK_NODATA;
                for y in 0..rect.h {
                    // `offset` is where the rectangle's first pixel is in the backing; rows follow at the resource's
                    // stride.
                    if !segments_read(&r.backing, offset + y as u64 * r.width as u64 * gbpp, &mut row) {
                        status = RESP_ERR_INVALID_PARAMETER;
                        break;
                    }
                    let to = ((rect.y + y) as usize * r.width as usize + rect.x as usize) * r.dbpp as usize;
                    convert_to(&mut r.pixels[to..], r.dformat, &row, r.format, rect.w as usize);
                }
                c.row = row;
                status
            }
            CMD_RESOURCE_FLUSH => {
                if left < 20 {
                    return RESP_ERR_INVALID_PARAMETER;
                }
                let rect = Rect::decode(body);
                let id = a(4);
                if c.blob(id).is_some() {
                    return self.flush_blob(c, id, &rect);
                }
                let Some(i) = c.find(id) else { return RESP_ERR_INVALID_RESOURCE_ID };
                if !rect.ok(c.res[i].width, c.res[i].height) {
                    return RESP_ERR_INVALID_PARAMETER;
                }
                let d = self.board.frame();
                if let Some(d) =
                    d.filter(|_| id == c.scanout_resource && c.res[i].dbpp == pixels::bpp(self.display_format()))
                {
                    let (w, hgt) = rect.clip(d.width, d.height);
                    let r = &c.res[i];
                    let bpp = r.dbpp as usize;
                    // SAFETY: the device's lanes (one at a time, under `core`) are the frame's only writer.
                    let px = unsafe { d.bytes_mut() };
                    for y in 0..hgt as usize {
                        let (ry, n) = (rect.y as usize + y, w as usize * bpp);
                        let to = ry * d.stride as usize + rect.x as usize * bpp;
                        let from = (ry * r.width as usize + rect.x as usize) * bpp;
                        px[to..to + n].copy_from_slice(&r.pixels[from..from + n]);
                    }
                    let backing = std::mem::take(&mut c.res[i].backing);
                    self.frame_shown(c, &backing, false); // first: session/ready before the helper is told of the frame
                    c.res[i].backing = backing;
                    if w != 0 && hgt != 0 {
                        self.board.flush(rect.x, rect.y, w, hgt);
                    }
                }
                RESP_OK_NODATA
            }
            _ => RESP_ERR_UNSPEC,
        }
    }

    /// Runs the command of a chain on queue 0 and writes its response. Returns the response's length (0: a chain that
    /// is no command), the command's header and the status. With `core` held (C: `cmd`).
    fn serve(&self, c: &mut Core, iov: &[Viov]) -> (u32, Hdr, u32) {
        // Gather the readable parts (the command), find the writable ones (the response).
        let (req_size, resp_max) = (virtio::readable_len(iov), virtio::writable_len(iov));
        if !(HDR..=1 << 20).contains(&req_size) || resp_max < HDR {
            return (0, Hdr::default(), 0);
        }
        let mut req = std::mem::take(&mut c.req);
        let mut resp = std::mem::take(&mut c.resp);
        req.clear();
        req.resize(req_size.div_ceil(4), 0);
        let req_bytes = &mut words_as_bytes(&mut req)[..req_size];
        virtio::read_chain(iov, 0, req_bytes);
        resp.clear();
        resp.resize(resp_max.max(4096), 0);
        let input = Hdr::decode(req_bytes);
        let mut resp_size = HDR;
        let status = self.command(c, req_bytes, &mut resp[..resp_max], &mut resp_size);
        let out = self.answer(&input, status);
        out.encode(&mut resp);
        let resp_size = resp_size.min(resp_max);
        virtio::write_chain(iov, 0, &resp[..resp_size]);
        c.req = req;
        c.resp = resp;
        (resp_size as u32, input, status)
    }

    /// The cursor queue's chain: accepted, not drawn.
    fn serve_cursor(&self, iov: &[Viov]) -> u32 {
        let req_size = virtio::readable_len(iov);
        if !(HDR..=1 << 20).contains(&req_size) || virtio::writable_len(iov) < HDR {
            return 0;
        }
        let mut h = [0u8; HDR];
        virtio::read_chain(iov, 0, &mut h);
        let mut resp = [0u8; HDR];
        self.answer(&Hdr::decode(&h), RESP_OK_NODATA).encode(&mut resp);
        virtio::write_chain(iov, 0, &resp);
        HDR as u32
    }

    fn answer(&self, input: &Hdr, status: u32) -> Hdr {
        let mut out = Hdr { kind: status, ..Hdr::default() };
        if input.flags & FLAG_FENCE != 0 {
            out.flags = input.flags & (FLAG_FENCE | FLAG_RING_IDX);
            out.fence = input.fence;
            out.ctx = input.ctx;
            out.ring = input.ring;
        }
        out
    }

    fn teardown(&self, c: &mut Core) {
        c.res.clear();
        c.scanout_resource = 0;
        c.scan.width = 0;
        let Some(blobs) = c.blobs.as_mut() else { return };
        if let Some(b) = self.b.as_deref() {
            for id in blobs.keys() {
                b.unref(*id);
            }
        }
        blobs.clear();
        c.blob_bytes = 0;
        for ctx in c.ctx.drain(..) {
            if let Some(b) = self.b.as_deref() {
                b.ctx_destroy(ctx.id);
            }
        }
        lock(&self.lanes).fences.clear(); // their chains were the old queue's
    }

    // ---- the 3D device's control queue: its own threads --------------------------------------------------------------
    fn wake(&self, reset: bool) {
        {
            let mut l = lock(&self.lanes);
            l.kicks += 1;
            if reset {
                l.reset_requested = true;
            }
        }
        if let Some(fd) = &self.wake {
            let one = 1u64.to_ne_bytes();
            // SAFETY: writes 8 bytes of ours to our eventfd.
            let _ = xh_core::fd::retry(|| unsafe { libc::write(fd.as_raw_fd(), one.as_ptr().cast(), 8) });
        }
    }

    fn slow_pending(&self) -> bool {
        let l = lock(&self.lanes);
        l.slow_done != l.slow_queued
    }

    /// Serves a chain and answers it (now, or when its fence retires). With `may_wait`, a command that names a
    /// resource the slow lane may still be making is not answered: None, and the caller runs it again later.
    fn work(&self, head: u16, iov: &[Viov], gen: u64, may_wait: bool) -> Option<u32> {
        let mut c = lock(&self.core);
        let (written, input, status) = self.serve(&mut c, iov);
        // A command of the fast lane that names a resource the slow lane has not made yet (an app's IOSurface the render
        // server attaches, the unref of an app's blob) waits for the slow lane's commands that came before it, and runs
        // again.
        if may_wait && status == RESP_ERR_INVALID_RESOURCE_ID && self.slow_pending() {
            return None;
        }
        // A command fenced on a context's ring is returned when the ring passes the fence; any other at once (a fence
        // on the device's own ring has nothing to wait for: the 2D commands are done when they return).
        let hold = written > 0
            && status < RESP_ERR_UNSPEC
            && input.flags & FLAG_FENCE != 0
            && input.flags & FLAG_RING_IDX != 0
            && c.ctx_find(input.ctx).is_some();
        let mut p = Pending { head, written, ctx: input.ctx, ring: input.ring as u32, fence: input.fence, gen };
        if let Some(post) = c.post.take() {
            // A frame for the helper: its flush is answered when the helper has it in the layer. Held before it is
            // posted, since the answer may come first. Not posted after all (the helper went): copied here, answered
            // now.
            (p.ctx, p.ring, p.fence) = (0, 0, post.seq);
            self.hold(p);
            let entries = std::mem::take(&mut c.blobs.as_mut().unwrap().get_mut(&post.blob).unwrap().entries);
            let r = post.rect;
            let done = self.done.get().expect("the device's done");
            let s = c.scan;
            let posted = self.board.post(
                &entries,
                s.offset as u64,
                s.stride,
                s.width,
                s.height,
                [r.x, r.y, r.w, r.h],
                post.seq,
                done,
            );
            if !posted {
                self.flush_copy(&mut c, &entries, &r);
            }
            c.blobs.as_mut().unwrap().get_mut(&post.blob).unwrap().entries = entries;
            if !posted {
                self.retire(0, Some(0), post.seq);
            }
            return Some(status);
        }
        if !hold {
            self.push_pending(&p);
            return Some(status);
        }
        self.hold(p);
        // May retire it before it returns. A refused fence is retired here.
        let b = self.b.as_deref().expect("a 3D device");
        if b.submit_fence(input.ctx, input.ring as u32, input.fence).is_err() {
            self.retire(input.ctx, Some(input.ring as u32), input.fence);
        }
        Some(status)
    }

    /// The fast lane's claim on `core`: from its first command to the ring's end, dropped while it waits for the slow
    /// lane. A mutex is not fair: the slow lane's thread, unlocking and locking it again on its own CPU between an
    /// app's blobs, mostly won it back, and a frame of the render server waited for several blobs, not one. And between
    /// two commands of the fast lane's thread (the lane choice of an app's next blob, then the render server's submit)
    /// the slow lane took the next blob's turn.
    fn fast_want(&self, on: bool) {
        let mut l = lock(&self.lanes);
        if on {
            l.fast_waiting += 1;
        } else {
            l.fast_waiting -= 1;
            if l.fast_waiting == 0 {
                self.slow_cv.notify_all();
            }
        }
    }

    /// Whether a command is the slow lane's: a context's, and not the render server's. Context creation stays in the
    /// fast lane (it decides the lane of what follows).
    fn slow_lane(&self, h: &Hdr) -> bool {
        if h.ctx == 0 || h.kind == CMD_CTX_CREATE {
            return false;
        }
        let c = lock(&self.core);
        c.ctx.iter().find(|x| x.id == h.ctx).is_some_and(|x| !x.fast)
    }
}

/// The header of a chain, read without taking it (None: shorter than a header).
fn peek_hdr(iov: &[Viov]) -> Option<Hdr> {
    let mut h = [0u8; HDR];
    (virtio::read_chain(iov, 0, &mut h) == HDR).then(|| Hdr::decode(&h))
}

fn slow_worker(dev: Arc<Virtio<Gpu>>) {
    let g = dev.device();
    loop {
        let job = {
            let mut l = lock(&g.lanes);
            loop {
                if let Some(j) = l.slow.pop_front() {
                    break j;
                }
                l = g.slow_cv.wait(l).unwrap_or_else(|e| e.into_inner());
            }
        };
        let current = {
            let _d = dev.lock();
            job.gen == g.gen.load(Ordering::Acquire) // a chain from before a reset is the old queue's: dropped
        };
        if current {
            {
                let mut l = lock(&g.lanes);
                while l.fast_waiting != 0 {
                    l = g.slow_cv.wait(l).unwrap_or_else(|e| e.into_inner()); // the fast lane first
                }
            }
            g.work(job.head, &job.iov, job.gen, false);
        }
        let mut l = lock(&g.lanes);
        l.slow_done += 1;
        g.slow_cv.notify_all();
        g.idle.notify_all();
    }
}

fn worker(dev: Arc<Virtio<Gpu>>) {
    let g = dev.device();
    let wake = g.wake.as_ref().expect("the 3D device's eventfd").as_raw_fd();
    let mut iov = vec![Viov::EMPTY; WORKER_IOV];
    loop {
        let mut count = [0u8; 8];
        // SAFETY: reads 8 bytes into ours from our eventfd.
        let r = unsafe { libc::read(wake, count.as_mut_ptr().cast(), 8) };
        if r < 0 && xh_core::sys::errno() != libc::EINTR {
            break;
        }
        let (upto, reset) = {
            let mut l = lock(&g.lanes);
            let reset = std::mem::take(&mut l.reset_requested);
            (l.kicks, reset)
        };
        g.fast_want(true);
        if reset {
            let mut c = lock(&g.core);
            g.teardown(&mut c);
        }
        loop {
            let (gen, popped) = {
                let _d = dev.lock();
                (g.gen.load(Ordering::Acquire), dev.pop(0, &mut iov))
            };
            let Some((head, n)) = popped else { break };
            let chain = &iov[..n];
            if peek_hdr(chain).is_some_and(|h| g.slow_lane(&h)) {
                let mut l = lock(&g.lanes);
                l.slow.push_back(SlowJob { head, gen, iov: chain.to_vec() });
                l.slow_queued += 1;
                g.slow_cv.notify_all();
                continue;
            }
            if g.work(head, chain, gen, true).is_none() {
                g.fast_want(false); // the slow lane's turn, up to this command's place
                {
                    let mut l = lock(&g.lanes);
                    // The slow lane's commands that came before this one: only this thread queues slow jobs (above), so
                    // the count read now is the one at this command's place. Read here, not before every fast command
                    // (a lock and unlock of `lanes` per command saved: YMGPwcca/OneiOS 8be9ec18).
                    let before = l.slow_queued;
                    while l.slow_done < before {
                        l = g.slow_cv.wait(l).unwrap_or_else(|e| e.into_inner());
                    }
                }
                g.fast_want(true);
                g.work(head, chain, gen, false);
            }
        }
        g.fast_want(false);
        let mut l = lock(&g.lanes);
        l.served = upto;
        g.idle.notify_all();
    }
}

impl virtio::Device for Gpu {
    fn handle(&self, _dev: &Virtio<Self>, queue: u32, _head: u16, iov: &[Viov]) -> Option<u32> {
        Some(if queue == 0 { self.serve(&mut lock(&self.core), iov).0 } else { self.serve_cursor(iov) })
    }

    fn config_read(&self, _dev: &Virtio<Self>, offset: u32, _size: u32) -> u32 {
        // events_read, events_clear, num_scanouts = 1, num_capsets
        match offset {
            8 => 1,
            12 => self.b.as_deref().map_or(0, |b| b.capset_count()),
            _ => 0,
        }
    }

    fn reset(&self, _dev: &Virtio<Self>) {
        if self.b.is_none() {
            let mut c = lock(&self.core);
            self.teardown(&mut c);
            return;
        }
        // With the device's lock held: the queues are already gone; the worker forgets the rest before its next
        // command.
        self.gen.fetch_add(1, Ordering::AcqRel);
        self.wake(true);
    }

    fn kick(&self, _dev: &Virtio<Self>, _queue: u32) {
        self.wake(false);
    }
}

/// The 2D virtio-gpu device at `base` (0: not on the MMIO bus), its display `width` x `height`.
pub fn create(base: u64, spi: u32, width: u32, height: u32) -> Option<GpuDev> {
    create_with(base, spi, width, height, None, Arc::new(Machine))
}

/// The virtio-gpu device with 3D (features VIRGL, RESOURCE_BLOB, CONTEXT_INIT, CREATE_GUEST_HANDLE; capsets from
/// `b`). With `b` None it is [`create`]'s 2D device.
pub fn create_3d(base: u64, spi: u32, width: u32, height: u32, b: Option<Box<dyn Gpu3d>>) -> Option<GpuDev> {
    create_with(base, spi, width, height, b, Arc::new(Machine))
}

/// [`create_3d`] on a given board (a test's).
pub fn create_with(
    base: u64,
    spi: u32,
    width: u32,
    height: u32,
    b: Option<Box<dyn Gpu3d>>,
    board: Arc<dyn Board>,
) -> Option<GpuDev> {
    create_in(virtio::Registry::global(), base, spi, width, height, b, board)
}

/// [`create_with`] on a registry of the test's own.
pub fn create_in(
    registry: &virtio::Registry,
    base: u64,
    spi: u32,
    width: u32,
    height: u32,
    b: Option<Box<dyn Gpu3d>>,
    board: Arc<dyn Board>,
) -> Option<GpuDev> {
    let three_d = b.is_some();
    let wake = if three_d {
        // SAFETY: a new eventfd of ours.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        // SAFETY: as above.
        Some(unsafe { OwnedFd::from_raw_fd(fd) })
    } else {
        None
    };
    let core = Core {
        res: Vec::new(),
        scanout_resource: 0,
        first_frame: false,
        blobs: three_d.then(HashMap::new),
        ctx: Vec::new(),
        blob_bytes: 0,
        blobs_made: 0,
        blobs_unref: 0,
        blob_note_us: 0,
        noted_made: 0,
        noted_unref: 0,
        scan: Scan::default(),
        post: None,
        display_seq: 0,
        flush_note: FlushNote::default(),
        req: Vec::new(),
        resp: Vec::new(),
        row: Vec::new(),
    };
    let gpu = Gpu {
        width,
        height,
        b,
        board,
        core: Mutex::new(core),
        lanes: Mutex::new(Lanes::default()),
        idle: Condvar::new(),
        slow_cv: Condvar::new(),
        gen: AtomicU64::new(0),
        wake,
        dev: OnceLock::new(),
        done: OnceLock::new(),
    };
    let info = if three_d {
        virtio::Info {
            device_id: 16,
            features: F_VIRGL | F_RESOURCE_BLOB | F_CONTEXT_INIT | F_CREATE_GUEST_HANDLE,
            nqueues: 2,
            manual: 1,
        }
    } else {
        virtio::Info { device_id: 16, features: 0, nqueues: 2, manual: 0 }
    };
    let dev = Virtio::create_in(registry, base, spi, info, gpu, "virtio-gpu").ok()?;
    let g = dev.device();
    let _ = g.dev.set(Arc::downgrade(&dev));
    let weak = Arc::downgrade(&dev);
    // The display helper has a posted frame in its layer (or it was given up on): its flush goes back to the guest.
    let _ = g.done.set(Arc::new(move |seq| {
        if let Some(d) = weak.upgrade() {
            d.device().retire(0, Some(0), seq)
        }
    }));
    if let Some(b) = g.b.as_deref() {
        let weak = Arc::downgrade(&dev);
        b.set_retire(Arc::new(move |ctx, ring, fence| {
            if let Some(d) = weak.upgrade() {
                d.device().retire(ctx, Some(ring), fence)
            }
        }));
        let (d1, d2) = (dev.clone(), dev.clone());
        std::thread::Builder::new().spawn(move || worker(d1)).ok()?;
        std::thread::Builder::new().spawn(move || slow_worker(d2)).ok()?;
    }
    Some(dev)
}

/// Test hook: waits until the 3D device's threads have nothing left to do.
pub fn idle(dev: &GpuDev) {
    let g = dev.device();
    if g.b.is_none() {
        return;
    }
    let mut l = lock(&g.lanes);
    while l.served != l.kicks || l.slow_done != l.slow_queued {
        l = g.idle.wait(l).unwrap_or_else(|e| e.into_inner());
    }
}

/// Test hook: whether the 3D device's fast lane (the render server's) has commands to serve (the slow lane waits).
pub fn fast_waiting(dev: &GpuDev) -> u32 {
    lock(&dev.device().lanes).fast_waiting
}

#[cfg(test)]
mod tests;
