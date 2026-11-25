// SPDX-License-Identifier: MIT
//! "droidvm,virtio-media": xnuhost's stateful V4L2 video encoder and (2026-11-24) decoder device (docs/hardware.md,
//! "Video encoder" and "Video decoder"). It is the host end of the V4L2 codecs UniXNU-droidvm's DroidVMVTHost.cpp
//! drives for iOS's paravirtualized VideoToolbox:
//! - an encoder session: OUTPUT is tightly packed NV12 (or P010) frames, CAPTURE the HEVC/H.264 bitstream (Annex-B);
//!   the encoding is a backend's ([`crate::backend`]): Android's MediaCodec on the phone, a stand-in on the PC.
//! - a decoder session (README.md "The decoder"): OUTPUT is the compressed stream (H.264, HEVC, VP9, AV1), CAPTURE the
//!   pictures, NV12 at the decoder's stride; the decoding is a [`DecodeBackend`]'s, which writes each picture straight
//!   into a queued CAPTURE buffer and tells the device ([`DecEvent`]).
//!
//! A session is an encoder or a decoder by its first S_FMT: OUTPUT in a compressed format makes a decoder, anything
//! else an encoder. The card says which the device offers: "oneios-encoder", or "oneios-encoder+decoder" with a
//! decoder backend and `decoder=auto` (the guest finds its decoder and its encoder by those words).
//!
//! - commandq (0): OPEN/CLOSE/IOCTL/MMAP/MUNMAP, answered on the vCPU thread ([`Device::handle`], the device lock
//!   held).
//! - eventq (1, manual): the guest posts writable event buffers; a worker thread fills one per event (DQBUF of a
//!   returned OUTPUT buffer, a coded or decoded CAPTURE buffer; EOS and SOURCE_CHANGE EVENTs).
//!
//! The worker sleeps until something is for it (2026-11-25): a command, a kick of the event queue (the guest gave event
//! buffers back), a decoder's result, or an encoder's coded unit or drain's end (the helper's receive thread wakes it
//! with each: [`Session::wakes`]). While an encoder session is open or an event waits for a guest buffer it wakes
//! [`MEDIA_IDLE_MS`] after its last look at most (the guard: a lost wake-up costs that much, never a stalled stream);
//! with neither, not at all (a decoder session and a device nobody uses cost no wake-up of their own). Only a backend
//! whose session cannot wake it (the NDK's MediaCodec in-process: encctl) is still asked every millisecond for
//! [`MEDIA_BUSY_MS`] after the guest's last command or its last unit, as every backend was until 2026-11-25. The
//! worker counts its wake-ups by why and says them every [`WAKES_REPORT_MS`] while it wakes ([`wakes_text`]).
//!
//! The sessions' MMAP'd buffers come from the shared media pool, first fit ([`Alloc`]; until 2026-11-24 each of two
//! sessions had a fixed half). A buffer that will not fit is refused at REQBUFS and VideoToolbox keeps its software
//! codec.
use crate::backend::{input_format, Backend, Coding, Session, Wakeup};
use crate::decoder::{CapLayout, DecEvent, DecodeBackend, DecodeSink, Decoder};
use crate::pod::Pod;
use crate::proto::*;
use crate::switches::TenBit;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::Duration;
use xh_core::bytes::le;
use xh_core::time::monotonic_ms;
use xh_core::virtio::{Device, Info, Viov, Virtio};
use xh_core::{loge, logi};

/// concurrent sessions, encoders and decoders (the pool's room decides how large each may be)
pub const MEDIA_SESSIONS: usize = 4;
const MEDIA_OUTPUT_BUFS: u32 = 4;
const MEDIA_CAPTURE_BUFS: u32 = 8;
const MEDIA_CODED_HEADROOM: u64 = 65536;
const MEDIA_EVENT_FIFO: usize = 64;
/// a backend that cannot wake the worker ([`Session::wakes`] false) is asked every millisecond this long after the
/// last command or unit
pub const MEDIA_BUSY_MS: u64 = 2000;
/// the worker's guard: while an encoder session is open or an event waits for a guest buffer it looks at least this
/// often (a lost wake-up costs this much at most)
pub const MEDIA_IDLE_MS: u64 = 50;
/// the worker's wake-ups line (`media: 30 s: ...`): at most this often, and only while it wakes
pub const WAKES_REPORT_MS: u64 = 30_000;
const EVENT_BODY: usize = MEDIA_EVENT_BYTES as usize - 8;
/// The coded pictures' line (`encoder: 60 s: ...`) and the decoded ones' (`decoder: 60 s: ...`): at most this often,
/// and only while pictures are coded (decoded) or opens refused.
pub const REPORT_MS: u64 = 60_000;
/// A decoder's compressed frames: each OUTPUT buffer this large at least and at most (the guest asks 2 MiB).
const DEC_OUTPUT_MIN: u32 = 1 << 20;
const DEC_OUTPUT_MAX: u32 = 4 << 20;
/// A decoder's pictures: the sizes it takes (ENUM_FRAMESIZES; c2.qti's decoders start at 96) and the most pixels
/// (4096x2304: two NV12 pictures and the OUTPUT buffers fit the 48 MiB pool).
pub const DEC_MIN_SIZE: u32 = 96;
pub const DEC_MAX_SIZE: u32 = 4096;
pub const DEC_MAX_PIXELS: u64 = 4096 * 2304;
/// The CAPTURE buffers a decoder asks beyond the guest's two (V4L2_CID_MIN_BUFFERS_FOR_CAPTURE): the decoder holds its
/// own pictures; these are only where they are handed over.
const DEC_MIN_CAPTURE: u32 = 2;

/// Counters for the host rig and encctl (`br_encoder_stats`; monotonic).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub sessions: u64,
    pub frames_in: u64,
    pub frames_out: u64,
    pub errors: u64,
    /// coded access units by the session's coding (8-bit, Main10, 10-bit as 8-bit; [`Coding`]'s order)
    pub coded: [u64; 3],
    /// backend opens refused
    pub refused: u64,
    /// the decoder (2026-11-24): its sessions and opens refused, the pictures decoded by codec ([`DECODER_CODECS`]'s
    /// order), the frames fed, the frames dropped (decoded into no picture, or fed and never out by the end of a drain
    /// or the session), and the decode time (the sum and count of the pictures timed, in us)
    pub dec_sessions: u64,
    pub dec_refused: u64,
    pub decoded: [u64; 4],
    pub dec_fed: u64,
    pub dec_dropped: u64,
    pub dec_us: u64,
    pub dec_timed: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Buf {
    /// into the pool (and the mmap handle)
    offset: u64,
    size: u64,
    /// on the device's side of the V4L2 queue
    queued: bool,
}

/// What a session is, by its first S_FMT.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Role {
    #[default]
    None,
    Encoder,
    Decoder,
}

/// A decoder session's results, from the decoder's thread to the worker.
type DecQueue = Arc<Mutex<VecDeque<DecEvent>>>;

#[derive(Default)]
struct MediaSession {
    used: bool,
    id: u32,
    role: Role,
    /// the encoder's codec (from CAPTURE) and input (NV12 or P010; 0 before S_FMT: NV12); the decoder's codec (from
    /// OUTPUT); the size from OUTPUT
    codec: u32,
    width: u32,
    height: u32,
    input: u32,
    output_on: bool,
    capture_on: bool,
    draining: bool,
    output: Vec<Buf>,
    capture: Vec<Buf>,
    enc: Option<Box<dyn Session>>,
    /// the encoder's backend wakes the worker with its units ([`Session::wakes`]): no 1 ms looks for it
    wakes: bool,
    /// a decoder: its OUTPUT buffers' size, the decoder, its results, the pictures' layout (from its first result),
    /// the frames fed and out (pictures or errors) since the last drain's end
    out_size: u32,
    dec: Option<Box<dyn Decoder>>,
    events: Option<DecQueue>,
    layout: Option<CapLayout>,
    fed: u64,
    out: u64,
}

impl MediaSession {
    fn buffers(&mut self, kind: u32) -> Option<&mut Vec<Buf>> {
        match kind {
            MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE => Some(&mut self.output),
            MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE => Some(&mut self.capture),
            _ => None,
        }
    }
}

/// The pool's buffers, first fit, 4 KiB aligned: (offset, size), by offset.
#[derive(Default)]
pub struct Alloc {
    size: u64,
    used: Vec<(u64, u64)>,
}

impl Alloc {
    pub fn new(size: u64) -> Alloc {
        Alloc { size, used: Vec::new() }
    }

    /// `size` bytes, None when no gap is large enough.
    pub fn take(&mut self, size: u64) -> Option<u64> {
        let size = size.checked_add(0xfff)? & !0xfff;
        let mut at = 0;
        for (i, &(offset, length)) in self.used.iter().enumerate() {
            if offset - at >= size {
                self.used.insert(i, (at, size));
                return Some(at);
            }
            at = offset + length;
        }
        (self.size.checked_sub(at)? >= size).then(|| {
            self.used.push((at, size));
            at
        })
    }

    pub fn give(&mut self, offset: u64) {
        self.used.retain(|&(o, _)| o != offset);
    }

    /// The bytes in use.
    pub fn in_use(&self) -> u64 {
        self.used.iter().map(|(_, l)| l).sum()
    }
}

/// A pending event, filled into a guest event buffer by the worker.
struct Event {
    kind: u32,
    session: u32,
    body: [u8; EVENT_BODY],
    body_len: usize,
}

/// The host mapping of the media pool: guest memory the MMAP'd buffers live in.
struct Pool {
    host: NonNull<u8>,
    size: u64,
}

// SAFETY: the pool is guest RAM mapped for the process's life; it is touched only with the device lock held, on a
// buffer the guest has handed over (a queued OUTPUT buffer, a queued CAPTURE buffer).
unsafe impl Send for Pool {}

impl Pool {
    /// `[offset, offset + size)` of the pool, if it is inside it.
    fn at(&self, offset: u64, size: u64) -> Option<*mut u8> {
        // SAFETY: inside the pool (checked).
        (offset.checked_add(size)? <= self.size).then(|| unsafe { self.host.as_ptr().add(offset as usize) })
    }
}

/// A 60 s window of counters: the counters when it began, when (0: none open), the largest decode time in it.
#[derive(Default)]
struct Window {
    from: Stats,
    since_ms: u64,
    max_us: u32,
}

struct State {
    sessions: [MediaSession; MEDIA_SESSIONS],
    alloc: Alloc,
    fifo: VecDeque<Event>,
    next_session: u32,
    /// the worker asks the backend every millisecond until then
    busy_until_ms: u64,
    stats: Stats,
    /// the 60 s lines ([`REPORT_MS`]): the encoder's and the decoder's
    report: Window,
    dec_report: Window,
}

/// The worker's wake-ups since the device was made (2026-11-25), by why: `woken` by something for it (a command, a
/// kick, a decoder's result, an encoder's unit: data), `timed_out` at its guard or a line's time with nothing for it,
/// `ticks` its 1 ms looks for a backend that cannot wake it ([`MEDIA_BUSY_MS`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wakes {
    pub woken: u64,
    pub timed_out: u64,
    pub ticks: u64,
}

impl Wakes {
    pub fn total(&self) -> u64 {
        self.woken + self.timed_out + self.ticks
    }
}

/// The worker's sleep: `wake` under its own lock (not the device's), and its wake-ups counted.
#[derive(Default)]
struct Wake {
    wake: Mutex<bool>,
    cond: Condvar,
    stop: AtomicBool,
    woken: AtomicU64,
    timed_out: AtomicU64,
    ticks: AtomicU64,
}

impl Wake {
    /// Something for the worker: a command, a kick of the guest's, a decoder's result, an encoder's coded unit (the
    /// device lock held or not). One that comes while the worker works is kept for its next sleep, which returns at
    /// once: none is lost.
    fn wake(&self) {
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cond.notify_one();
    }

    /// The worker's sleep: until it is woken, or `ms` at most (0: no limit); counted by which. Whether it was woken.
    fn sleep(&self, ms: u64) -> bool {
        let woken = self.wake.lock().unwrap_or_else(PoisonError::into_inner);
        let mut woken = if ms != 0 {
            let limit = Duration::from_millis(ms);
            self.cond.wait_timeout_while(woken, limit, |w| !*w).unwrap_or_else(PoisonError::into_inner).0
        } else {
            self.cond.wait_while(woken, |w| !*w).unwrap_or_else(PoisonError::into_inner)
        };
        let was = std::mem::replace(&mut *woken, false);
        drop(woken);
        let counter = if was { &self.woken } else { &self.timed_out };
        counter.fetch_add(1, Ordering::Relaxed);
        was
    }

    /// A 1 ms look for a backend that cannot wake the worker (a wake-up meanwhile is kept for the next sleep).
    fn tick(&self) {
        std::thread::sleep(Duration::from_millis(1));
        self.ticks.fetch_add(1, Ordering::Relaxed);
    }

    fn counts(&self) -> Wakes {
        Wakes {
            woken: self.woken.load(Ordering::Relaxed),
            timed_out: self.timed_out.load(Ordering::Relaxed),
            ticks: self.ticks.load(Ordering::Relaxed),
        }
    }
}

/// The wake-ups line (2026-11-25): the worker's wake-ups since `from` by why. With every backend waking it, "1 ms
/// looks" stays 0 and "by the timeout" is its guard's few (and this line's own); until 2026-11-25 an encoder session
/// cost about 2,000 1 ms looks after each command.
pub fn wakes_text(from: &Wakes, now: &Wakes) -> String {
    format!(
        "media: {} s: worker woken {}: by data (commands, kicks, results, coded units) {}, by the timeout {}; \
         1 ms looks {}",
        WAKES_REPORT_MS / 1000,
        now.total() - from.total(),
        now.woken - from.woken,
        now.timed_out - from.timed_out,
        now.ticks - from.ticks
    )
}

/// The wake-ups line's window: it opens at the first wake-up after the last line, and the line is said
/// [`WAKES_REPORT_MS`] later (a worker that sleeps says nothing).
#[derive(Default)]
struct WakesWindow {
    from: Wakes,
    since_ms: u64,
}

impl WakesWindow {
    /// The ms until the line is due (0: no window open), and the line when it is due.
    fn step(&mut self, now: Wakes, now_ms: u64) -> (u64, Option<String>) {
        if self.since_ms == 0 {
            if now == self.from {
                return (0, None);
            }
            self.since_ms = now_ms.max(1);
        }
        let gone = now_ms.saturating_sub(self.since_ms);
        if gone < WAKES_REPORT_MS {
            return (WAKES_REPORT_MS - gone, None);
        }
        let line = wakes_text(&self.from, &now);
        (self.from, self.since_ms) = (now, 0);
        (0, Some(line))
    }
}

/// The device model (`media_state`).
pub struct MediaDevice {
    backend: Arc<dyn Backend>,
    decoder: Option<Arc<dyn DecodeBackend>>,
    pool: Pool,
    state: RefCell<State>,
    wake: Arc<Wake>,
}

pub type MediaVirtio = Virtio<MediaDevice>;

impl State {
    fn new(pool_size: u64) -> State {
        State {
            sessions: Default::default(),
            alloc: Alloc::new(pool_size),
            fifo: VecDeque::with_capacity(MEDIA_EVENT_FIFO),
            next_session: 0,
            busy_until_ms: 0,
            stats: Stats::default(),
            report: Window::default(),
            dec_report: Window::default(),
        }
    }

    fn session(&mut self, id: u32) -> Option<&mut MediaSession> {
        self.sessions.iter_mut().find(|s| s.used && s.id == id)
    }

    fn push_event(&mut self, kind: u32, session: u32, body: &[u8]) {
        if self.fifo.len() >= MEDIA_EVENT_FIFO {
            self.stats.errors += 1;
            return;
        }
        let body_len = body.len().min(EVENT_BODY);
        let mut e = Event { kind, session, body: [0; EVENT_BODY], body_len };
        e.body[..body_len].copy_from_slice(&body[..body_len]);
        self.fifo.push_back(e);
    }

    /// A session ends (CLOSE, reset): its buffers back to the pool, its backend closed, the decoder's frames never out
    /// counted as dropped.
    fn end_session(&mut self, slot: usize) {
        let s = &mut self.sessions[slot];
        for b in s.output.iter().chain(s.capture.iter()) {
            self.alloc.give(b.offset);
        }
        if s.role == Role::Decoder {
            self.stats.dec_dropped += s.fed.saturating_sub(s.out);
        }
        *s = MediaSession::default(); // closes the encoder's or decoder's session
    }

    /// The buffers of a queue made again (REQBUFS): the old ones back to the pool.
    fn give_queue(&mut self, slot: usize, kind: u32) {
        let s = &mut self.sessions[slot];
        if let Some(bufs) = s.buffers(kind) {
            for b in bufs.drain(..) {
                self.alloc.give(b.offset);
            }
        }
    }
}

/// The 60 s line (2026-11-23): the pictures coded since `from` by their path, the opens refused and the sessions.
pub fn report_text(from: &Stats, now: &Stats) -> String {
    let coded = |c: Coding| now.coded[c as usize] - from.coded[c as usize];
    format!(
        "encoder: {} s: coded 8-bit {}, 10-bit as Main10 {}, 10-bit as 8-bit {}; opens refused {}; sessions {}",
        REPORT_MS / 1000,
        coded(Coding::Eight),
        coded(Coding::Main10),
        coded(Coding::TenAsEight),
        now.refused - from.refused,
        now.sessions - from.sessions
    )
}

/// The decoder's 60 s line (2026-11-24): the pictures decoded since `from` by codec, the decode time (from a frame's
/// feed to its picture: average and largest), the frames dropped, the sessions and opens refused, and the backend.
pub fn dec_report_text(from: &Stats, now: &Stats, max_us: u32, backend: &str) -> String {
    let decoded = |i: usize| now.decoded[i] - from.decoded[i];
    let timed = now.dec_timed - from.dec_timed;
    let avg = if timed == 0 { 0.0 } else { (now.dec_us - from.dec_us) as f64 / timed as f64 / 1000.0 };
    format!(
        "decoder: {} s: decoded H.264 {}, HEVC {}, VP9 {}, AV1 {} of {} fed; decode time avg {avg:.1} ms, max {} ms; \
         dropped {}; sessions {}, opens refused {} ({backend})",
        REPORT_MS / 1000,
        decoded(0),
        decoded(1),
        decoded(2),
        decoded(3),
        now.dec_fed - from.dec_fed,
        max_us.div_ceil(1000),
        now.dec_dropped - from.dec_dropped,
        now.dec_sessions - from.dec_sessions,
        now.dec_refused - from.dec_refused
    )
}

/// The 60 s window: it opens at the first picture coded or open refused after the last line, and the line is said
/// [`REPORT_MS`] later (nothing while nothing is coded). The ms until it is due, 0 with no window open.
fn report(st: &mut State, now_ms: u64) -> u64 {
    let w = &mut st.report;
    if w.since_ms == 0 {
        if st.stats.coded == w.from.coded && st.stats.refused == w.from.refused {
            w.from = st.stats; // other counters moved: not a reason for a line
            return 0;
        }
        w.since_ms = now_ms.max(1);
    }
    let gone = now_ms.saturating_sub(w.since_ms);
    if gone < REPORT_MS {
        return REPORT_MS - gone;
    }
    logi!("{}", report_text(&w.from, &st.stats));
    (w.from, w.since_ms) = (st.stats, 0);
    0
}

/// The decoder's window, as [`report`]'s: it opens at a picture decoded, a frame dropped or an open refused.
fn dec_report(st: &mut State, now_ms: u64, backend: &str) -> u64 {
    let w = &mut st.dec_report;
    if w.since_ms == 0 {
        let moved = st.stats.decoded != w.from.decoded
            || st.stats.dec_dropped != w.from.dec_dropped
            || st.stats.dec_refused != w.from.dec_refused;
        if !moved {
            w.from = st.stats;
            w.max_us = 0;
            return 0;
        }
        w.since_ms = now_ms.max(1);
    }
    let gone = now_ms.saturating_sub(w.since_ms);
    if gone < REPORT_MS {
        return REPORT_MS - gone;
    }
    logi!("{}", dec_report_text(&w.from, &st.stats, w.max_us, backend));
    (w.from, w.since_ms, w.max_us) = (st.stats, 0, 0);
    0
}

/// A DQBUF event: a struct v4l2_buffer (+ one plane) for the returned buffer.
fn push_dqbuf(st: &mut State, slot: usize, kind: u32, index: usize, bytesused: u32, flags: u32, sec: i64) {
    let s = &st.sessions[slot];
    let size = if kind == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE { s.capture[index].size } else { s.output[index].size };
    let mut b = MediaV4l2Buffer {
        index: index as u32,
        type_: kind,
        memory: MEDIA_MEMORY_MMAP,
        flags,
        sec,
        length: 1,
        ..Default::default()
    };
    b.plane.bytesused = bytesused;
    b.plane.length = size as u32;
    let id = s.id;
    st.push_event(MEDIA_EVT_DQBUF, id, b.as_bytes());
}

/// An EVENT (struct v4l2_event: its type first; SOURCE_CHANGE's changes, V4L2_EVENT_SRC_CH_RESOLUTION, at 8).
fn push_v4l2_event(st: &mut State, session: u32, kind: u32) {
    let mut ev = [0u32; 16];
    ev[0] = kind;
    if kind == MEDIA_EVENT_SOURCE_CHANGE {
        ev[2] = 1;
    }
    st.push_event(MEDIA_EVT_EVENT, session, ev.as_bytes());
}

/// Whether `format` is one the decoder takes on OUTPUT (the backend there, `decoder=auto`, the codec in
/// `decoder-codecs=`).
fn decoder_format(decoder: bool, format: u32) -> bool {
    decoder && decoder_index(format).is_some_and(|i| crate::switches::decoder_codecs() & (1 << i) != 0)
}

impl MediaDevice {
    /// Whether this device decodes (a decoder backend and `decoder=auto`).
    fn decodes(&self) -> bool {
        self.decoder.is_some() && crate::switches::decoder()
    }

    /// A session's backend is opened when both queues are streaming and the codec and size are known.
    fn maybe_open_encoder(&self, st: &mut State, slot: usize) {
        let s = &mut st.sessions[slot];
        if s.role != Role::Encoder
            || s.enc.is_some()
            || !s.output_on
            || !s.capture_on
            || s.codec == 0
            || s.width == 0
            || s.height == 0
        {
            return;
        }
        let input = if s.input == 0 { MEDIA_PIXFMT_NV12 } else { s.input };
        s.enc = self.backend.open(s.codec, input, s.width, s.height);
        if let Some(enc) = s.enc.as_mut() {
            let wake = Arc::clone(&self.wake);
            let wakeup: Wakeup = Arc::new(move || wake.wake());
            s.wakes = enc.wakes(wakeup);
        }
        let (id, codec, w, h, wakes) = (s.id, s.codec, s.width, s.height, s.wakes);
        let name = if codec == MEDIA_PIXFMT_HEVC { "HEVC" } else { "H.264" };
        let p010 = if input == MEDIA_PIXFMT_P010 { " (P010 input)" } else { "" };
        match s.enc.as_ref().map(|e| e.coding()) {
            None => {
                st.stats.errors += 1;
                st.stats.refused += 1;
                loge!("encoder: backend {} refused {name} {w}x{h}{p010}", self.backend.name());
            }
            Some(coding) => {
                st.stats.sessions += 1;
                // which path codes it, once a session (2026-11-23)
                // and whether the worker waits for its units or asks every 1 ms (2026-11-25)
                let looks = if wakes { "" } else { "; asked every 1 ms (the backend cannot wake the worker)" };
                logi!("encoder: session {id}: {name} {w}x{h} on {}: {}{looks}", self.backend.name(), coding.text());
            }
        }
    }

    /// A decoder session's decoder, at its OUTPUT's STREAMON (the guest's dsta waits for it: a refusal is its
    /// error, and VideoToolbox decodes in software). A Linux errno.
    fn open_decoder(&self, st: &mut State, slot: usize) -> u32 {
        let Some(backend) = self.decoder.as_ref() else { return libc::EINVAL as u32 };
        let s = &mut st.sessions[slot];
        if s.dec.is_some() {
            return 0;
        }
        let queue: DecQueue = Arc::default();
        let (q, wake) = (Arc::clone(&queue), Arc::clone(&self.wake));
        let sink: DecodeSink = Arc::new(move |e| {
            q.lock().unwrap_or_else(PoisonError::into_inner).push_back(e);
            wake.wake();
        });
        let (id, codec, w, h) = (s.id, s.codec, s.width, s.height);
        s.dec = backend.open_decoder(codec, w, h, sink);
        if s.dec.is_none() {
            st.stats.dec_refused += 1;
            loge!(
                "decoder: backend {} refused {} {w}x{h}: VideoToolbox decodes it in software",
                backend.name(),
                codec_name(codec)
            );
            return libc::EIO as u32;
        }
        s.events = Some(queue);
        st.stats.dec_sessions += 1;
        logi!("decoder: session {id}: {} {w}x{h} on {}", codec_name(codec), backend.name());
        0
    }

    /// S_FMT: an encoder's (CAPTURE coded, OUTPUT NV12 or P010) or a decoder's (OUTPUT compressed; CAPTURE NV12).
    fn set_format(&self, s: &mut MediaSession, f: &mut MediaV4l2Format) -> u32 {
        let decoder = f.type_ == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE && decoder_format(self.decodes(), f.pixelformat);
        if decoder || s.role == Role::Decoder {
            if s.role == Role::Encoder || (f.type_ == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE && !decoder) {
                return libc::EINVAL as u32;
            }
            if f.type_ == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE {
                return if f.pixelformat == MEDIA_PIXFMT_NV12 { 0 } else { libc::EINVAL as u32 };
            }
            let (w, h) = (f.width, f.height);
            if w < DEC_MIN_SIZE / 2 || h < DEC_MIN_SIZE / 2 || w > DEC_MAX_SIZE || h > DEC_MAX_SIZE {
                return libc::EINVAL as u32;
            }
            if u64::from(w) * u64::from(h) > DEC_MAX_PIXELS || s.dec.is_some() {
                return libc::EINVAL as u32;
            }
            s.role = Role::Decoder;
            (s.codec, s.width, s.height) = (f.pixelformat, w, h);
            let asked = if f.planes[0].sizeimage == 0 { 2 << 20 } else { f.planes[0].sizeimage };
            s.out_size = asked.clamp(DEC_OUTPUT_MIN, DEC_OUTPUT_MAX);
            f.num_planes = 1;
            f.planes[0].sizeimage = s.out_size;
            f.planes[0].bytesperline = 0;
            return 0;
        }
        if f.type_ == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
            // NV12, or P010 (2026-11-23: iOS's 10-bit surfaces, two bytes a sample); encoder-10bit=nv12
            // (docs/switches.md): no P010, and the guest sends NV12 (the high bytes) then
            let refused = f.pixelformat == MEDIA_PIXFMT_P010 && crate::switches::ten_bit() == TenBit::Nv12;
            if !input_format(f.pixelformat) || refused || f.width == 0 || f.height == 0 {
                return libc::EINVAL as u32;
            }
            s.role = Role::Encoder;
            (s.width, s.height, s.input) = (f.width, f.height, f.pixelformat);
            let bytes = if f.pixelformat == MEDIA_PIXFMT_P010 { 2 } else { 1 };
            f.num_planes = 1;
            f.planes[0].bytesperline = f.width.wrapping_mul(bytes);
            f.planes[0].sizeimage = picture_bytes(f.pixelformat, f.width, f.height) as u32;
        } else if f.type_ == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE {
            if f.pixelformat != MEDIA_PIXFMT_HEVC && f.pixelformat != MEDIA_PIXFMT_H264 {
                return libc::EINVAL as u32;
            }
            s.role = Role::Encoder;
            s.codec = f.pixelformat;
            f.num_planes = 1;
            if f.planes[0].sizeimage == 0 {
                f.planes[0].sizeimage = f.width.wrapping_mul(f.height) / 2 + MEDIA_CODED_HEADROOM as u32;
            }
        } else {
            return libc::EINVAL as u32;
        }
        0
    }

    /// The formats of ENUM_FMT: OUTPUT takes NV12 and P010 (an encoder's) and, decoding, each codec of
    /// `decoder-codecs=`; CAPTURE gives HEVC and H.264 (an encoder's) and, decoding, NV12.
    fn formats(&self, kind: u32) -> Vec<u32> {
        let decodes = self.decodes();
        let mut f = Vec::new();
        if kind == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
            f.push(MEDIA_PIXFMT_NV12);
            if crate::switches::ten_bit() != TenBit::Nv12 {
                f.push(MEDIA_PIXFMT_P010);
            }
            f.extend(DECODER_CODECS.iter().filter(|c| decoder_format(decodes, **c)));
        } else if kind == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE {
            f.extend([MEDIA_PIXFMT_HEVC, MEDIA_PIXFMT_H264]);
            if decodes {
                f.push(MEDIA_PIXFMT_NV12);
            }
        }
        f
    }

    /// REQBUFS: `want` buffers of `kind` (0: none); the count made.
    fn request_buffers(&self, st: &mut State, slot: usize, kind: u32, want: u32) -> Result<u32, u32> {
        let s = &st.sessions[slot];
        let decoder = s.role == Role::Decoder;
        let (cap, size, least) = match (kind, decoder) {
            (MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, false) => {
                let input = if s.input == 0 { MEDIA_PIXFMT_NV12 } else { s.input };
                (MEDIA_OUTPUT_BUFS, picture_bytes(input, s.width, s.height), want.min(MEDIA_OUTPUT_BUFS))
            }
            (MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, false) => (
                MEDIA_CAPTURE_BUFS,
                u64::from(s.width) * u64::from(s.height) / 2 + MEDIA_CODED_HEADROOM,
                want.min(MEDIA_CAPTURE_BUFS),
            ),
            (MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, true) => (MEDIA_OUTPUT_BUFS, u64::from(s.out_size), 2),
            // the decoder's pictures: as many as fit, two at least
            (MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, true) => match s.layout {
                Some(l) => (MEDIA_CAPTURE_BUFS, l.bytes(), 2),
                None => return Err(libc::EINVAL as u32),
            },
            _ => return Err(libc::EINVAL as u32),
        };
        st.give_queue(slot, kind);
        if decoder && kind == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE {
            if let Some(d) = st.sessions[slot].dec.as_mut() {
                d.reset_capture();
            }
        }
        let want = want.min(cap);
        let mut bufs = Vec::with_capacity(want as usize);
        for _ in 0..want {
            match st.alloc.take(size) {
                Some(offset) => bufs.push(Buf { offset, size, queued: false }),
                None => break,
            }
        }
        if (bufs.len() as u32) < least.min(want) {
            for b in &bufs {
                st.alloc.give(b.offset);
            }
            st.stats.errors += 1;
            return Err(libc::ENOMEM as u32); // the pool is too full: VT keeps software
        }
        let made = bufs.len() as u32;
        *st.sessions[slot].buffers(kind).unwrap() = bufs;
        Ok(made)
    }

    /// IOCTL on a session: `nr` the _IOC_NR, `payload` the structure the guest sent (`len` valid bytes), changed in
    /// place for the bytes written back. A Linux errno (0 on success).
    fn ioctl(&self, st: &mut State, slot: usize, nr: u32, payload: &mut [u8], len: usize) -> u32 {
        let decoder = st.sessions[slot].role == Role::Decoder;
        match nr {
            MEDIA_VIDIOC_S_FMT => {
                if len < std::mem::size_of::<MediaV4l2Format>() {
                    return libc::EINVAL as u32;
                }
                let mut f = MediaV4l2Format::read_from(payload);
                let rc = self.set_format(&mut st.sessions[slot], &mut f);
                if rc == 0 {
                    f.write_to(payload);
                }
                rc
            }
            MEDIA_VIDIOC_G_FMT if decoder => {
                // the decoder's pictures (after its SOURCE_CHANGE): NV12 at the decoder's stride
                if len < std::mem::size_of::<MediaV4l2Format>() {
                    return libc::EINVAL as u32;
                }
                let s = &st.sessions[slot];
                let mut f = MediaV4l2Format::read_from(payload);
                match (f.type_, s.layout) {
                    (MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, Some(l)) => {
                        (f.width, f.height, f.pixelformat, f.num_planes) = (l.width, l.height, MEDIA_PIXFMT_NV12, 1);
                        f.planes[0].bytesperline = l.stride;
                        f.planes[0].sizeimage = l.bytes() as u32;
                    }
                    (MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, _) => {
                        (f.width, f.height, f.pixelformat, f.num_planes) = (s.width, s.height, s.codec, 1);
                        f.planes[0].sizeimage = s.out_size;
                    }
                    _ => return libc::EINVAL as u32,
                }
                f.write_to(payload);
                0
            }
            MEDIA_VIDIOC_G_CTRL if decoder => {
                // struct v4l2_control: id, value
                if len < 8 || le::u32(payload, 0) != MEDIA_CID_MIN_BUFFERS_FOR_CAPTURE {
                    return libc::EINVAL as u32;
                }
                payload[4..8].copy_from_slice(&DEC_MIN_CAPTURE.to_le_bytes());
                0
            }
            MEDIA_VIDIOC_REQBUFS => {
                if len < 20 {
                    return libc::EINVAL as u32;
                }
                let mut r = <[u32; 5]>::read_from(payload);
                match self.request_buffers(st, slot, r[1], r[0]) {
                    Ok(made) => {
                        r[0] = made;
                        r.write_to(payload);
                        0
                    }
                    Err(e) => e,
                }
            }
            MEDIA_VIDIOC_QUERYBUF => {
                if len < std::mem::size_of::<MediaV4l2Buffer>() {
                    return libc::EINVAL as u32;
                }
                let s = &st.sessions[slot];
                let mut b = MediaV4l2Buffer::read_from(payload);
                let bufs = if b.type_ == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE { &s.output } else { &s.capture };
                let Some(buf) = bufs.get(b.index as usize) else { return libc::EINVAL as u32 };
                b.length = 1;
                b.plane.length = buf.size as u32;
                b.plane.m = buf.offset; // the mmap handle (MMAP turns it into a place in the pool)
                b.write_to(payload);
                0
            }
            MEDIA_VIDIOC_QBUF => {
                if len < std::mem::size_of::<MediaV4l2Buffer>() {
                    return libc::EINVAL as u32;
                }
                let b = MediaV4l2Buffer::read_from(payload);
                let rc = if decoder { self.queue_decoder(st, slot, &b) } else { self.queue_encoder(st, slot, &b) };
                if rc == 0 {
                    b.write_to(payload);
                }
                rc
            }
            MEDIA_VIDIOC_STREAMON | MEDIA_VIDIOC_STREAMOFF => {
                if len < 4 {
                    return libc::EINVAL as u32;
                }
                let on = nr == MEDIA_VIDIOC_STREAMON;
                let kind = le::u32(payload, 0);
                let s = &mut st.sessions[slot];
                match kind {
                    MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE => s.output_on = on,
                    MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE => {
                        s.capture_on = on;
                        if !on && decoder {
                            // the pictures' buffers are the guest's again
                            s.capture.iter_mut().for_each(|b| b.queued = false);
                            if let Some(d) = s.dec.as_mut() {
                                d.reset_capture();
                            }
                        }
                    }
                    _ => return libc::EINVAL as u32,
                }
                if on && decoder && kind == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
                    let rc = self.open_decoder(st, slot);
                    if rc != 0 {
                        st.sessions[slot].output_on = false;
                    }
                    return rc;
                }
                if on {
                    self.maybe_open_encoder(st, slot);
                }
                0
            }
            MEDIA_VIDIOC_SUBSCRIBE_EVENT => 0, // EOS / SOURCE_CHANGE: accepted, delivered on the eventq
            MEDIA_VIDIOC_ENCODER_CMD => {
                if len < 4 {
                    return libc::EINVAL as u32;
                }
                let s = &mut st.sessions[slot];
                if le::u32(payload, 0) == MEDIA_ENC_CMD_STOP && !s.draining {
                    if let Some(enc) = s.enc.as_mut() {
                        s.draining = true;
                        let _ = enc.drain();
                    }
                }
                0
            }
            MEDIA_VIDIOC_DECODER_CMD if decoder => {
                // struct v4l2_decoder_cmd: cmd first. STOP: every frame out, then EOS; START: frames again after it.
                if len < 4 {
                    return libc::EINVAL as u32;
                }
                let s = &mut st.sessions[slot];
                let Some(dec) = s.dec.as_mut() else { return libc::EINVAL as u32 };
                let rc = match le::u32(payload, 0) {
                    MEDIA_DEC_CMD_STOP if !s.draining => {
                        s.draining = true;
                        dec.drain()
                    }
                    MEDIA_DEC_CMD_STOP => Ok(()),
                    MEDIA_DEC_CMD_START => {
                        s.draining = false;
                        dec.restart()
                    }
                    _ => return libc::EINVAL as u32,
                };
                rc.map_or_else(|e| e as u32, |()| 0)
            }
            MEDIA_VIDIOC_ENUM_FRAMESIZES => {
                // struct v4l2_frmsizeenum: index, pixel_format, type, then stepwise min/max/step of width and height.
                // A decoder's formats have sizes; the encoder lets VideoToolbox decide.
                if len < 36 || le::u32(payload, 0) != 0 || !decoder_format(self.decodes(), le::u32(payload, 4)) {
                    return libc::EINVAL as u32;
                }
                let sizes = [MEDIA_FRMSIZE_TYPE_STEPWISE, DEC_MIN_SIZE, DEC_MAX_SIZE, 2, DEC_MIN_SIZE, DEC_MAX_SIZE, 2];
                payload[8..36].copy_from_slice(sizes.as_bytes());
                0
            }
            MEDIA_VIDIOC_ENUM_FMT => {
                // struct v4l2_fmtdesc: index, type, flags, description[32], pixelformat (at 44). The guest's
                // transport lists each queue's formats once at start (DroidVMVirtioMedia.c check(), up to 16 an
                // answer of 0 gives it), and the decoder its OUTPUT once (staged DroidVMVTHost.cpp decoderCodecs);
                // past the list EINVAL ends their loops, as V4L2 says.
                if len < 48 {
                    return libc::EINVAL as u32;
                }
                let Some(&format) = self.formats(le::u32(payload, 4)).get(le::u32(payload, 0) as usize) else {
                    return libc::EINVAL as u32;
                };
                payload[44..48].copy_from_slice(&format.to_le_bytes());
                0
            }
            MEDIA_VIDIOC_G_FMT | MEDIA_VIDIOC_G_CTRL => 0, // not needed by the encoder path
            _ => libc::ENOTTY as u32,
        }
    }

    /// QBUF of an encoder: an OUTPUT frame to the backend (its buffer free again at once), a CAPTURE buffer to fill.
    fn queue_encoder(&self, st: &mut State, slot: usize, b: &MediaV4l2Buffer) -> u32 {
        let s = &mut st.sessions[slot];
        let index = b.index as usize;
        if b.type_ == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
            let (Some(buf), Some(enc)) = (s.output.get(index), s.enc.as_mut()) else {
                return libc::EINVAL as u32;
            };
            let used = u64::from(b.plane.bytesused);
            let rc = match self.pool.at(buf.offset, used) {
                Some(at) => {
                    // SAFETY: a queued OUTPUT buffer inside the pool: the guest does not touch it until DQBUF.
                    let nv12 = unsafe { std::slice::from_raw_parts(at, used as usize) };
                    enc.feed(nv12, b.sec * 1_000_000, false)
                }
                None => Err(libc::EINVAL),
            };
            if let Err(e) = rc {
                st.stats.errors += 1;
                return e as u32;
            }
            st.stats.frames_in += 1;
            // MediaCodec copies the input; the OUTPUT buffer is free again at once.
            push_dqbuf(st, slot, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, index, 0, 0, b.sec);
        } else if b.type_ == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE {
            let Some(buf) = s.capture.get_mut(index) else { return libc::EINVAL as u32 };
            buf.queued = true; // free for the worker to fill
        } else {
            return libc::EINVAL as u32;
        }
        0
    }

    /// QBUF of a decoder: an OUTPUT frame to the decoder (copied: its buffer free again at once), a CAPTURE buffer
    /// the decoder's to fill (it says when: [`DecEvent::Frame`]).
    fn queue_decoder(&self, st: &mut State, slot: usize, b: &MediaV4l2Buffer) -> u32 {
        let s = &mut st.sessions[slot];
        let index = b.index as usize;
        if b.type_ == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
            let (Some(buf), Some(dec)) = (s.output.get(index), s.dec.as_mut()) else {
                return libc::EINVAL as u32;
            };
            let used = u64::from(b.plane.bytesused).min(buf.size);
            let rc = match self.pool.at(buf.offset, used) {
                // SAFETY: a queued OUTPUT buffer inside the pool: the guest does not touch it until DQBUF.
                Some(at) => dec.feed(unsafe { std::slice::from_raw_parts(at, used as usize) }, b.sec),
                None => Err(libc::EINVAL),
            };
            if let Err(e) = rc {
                st.stats.errors += 1;
                return e as u32;
            }
            s.fed += 1;
            st.stats.dec_fed += 1;
            push_dqbuf(st, slot, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, index, 0, 0, b.sec);
        } else if b.type_ == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE {
            let Some(buf) = s.capture.get_mut(index) else { return libc::EINVAL as u32 };
            buf.queued = true;
            let (offset, size) = (buf.offset, buf.size);
            if let Some(dec) = s.dec.as_mut() {
                dec.capture(b.index, offset, size);
            }
        } else {
            return libc::EINVAL as u32;
        }
        0
    }

    /// Flushes the event FIFO into the guest's posted event buffers (the device lock held).
    fn flush_events(&self, dev: &MediaVirtio, st: &mut State) {
        while let Some(e) = st.fifo.front() {
            let mut iov = [Viov::EMPTY; 2];
            let Some((head, _)) = dev.pop(MEDIA_QUEUE_EVENT, &mut iov) else { break }; // no guest buffer: keep it
            let part = &iov[0];
            let mut written = 0;
            if part.writable() && part.len() >= 8 {
                part.write_at(0, &e.kind.to_le_bytes());
                part.write_at(4, &e.session.to_le_bytes());
                let body = e.body_len.min(part.len() as usize - 8);
                part.write_at(8, &e.body[..body]);
                written = 8 + body as u32;
            }
            dev.push(MEDIA_QUEUE_EVENT, head, written);
            st.fifo.pop_front();
        }
    }

    /// Pulls coded access units out of each streaming session's backend and turns them into CAPTURE DQBUF events.
    fn pump(&self, st: &mut State) {
        for slot in 0..MEDIA_SESSIONS {
            loop {
                let s = &mut st.sessions[slot];
                if !s.used || !s.capture_on || s.enc.is_none() {
                    break;
                }
                let Some(index) = s.capture.iter().position(|b| b.queued) else { break }; // none free; next poll
                let buf = s.capture[index];
                let Some(dst) = self.pool.at(buf.offset, buf.size) else { break };
                // SAFETY: a queued CAPTURE buffer inside the pool: the guest does not touch it until DQBUF.
                let dst = unsafe { std::slice::from_raw_parts_mut(dst, buf.size as usize) };
                let enc = s.enc.as_mut().unwrap();
                let coding = enc.coding();
                let polled = match enc.poll(dst) {
                    Ok(p) => p,
                    Err(_) => {
                        st.stats.errors += 1;
                        break;
                    }
                };
                if polled.bytes == 0 && !polled.eos {
                    break; // nothing ready yet
                }
                let flags = if polled.key { MEDIA_BUF_FLAG_KEYFRAME } else { 0 }
                    | if polled.eos { MEDIA_BUF_FLAG_LAST } else { 0 };
                s.capture[index].queued = false;
                let id = s.id;
                push_dqbuf(
                    st,
                    slot,
                    MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE,
                    index,
                    polled.bytes as u32,
                    flags,
                    polled.pts_us / 1_000_000,
                );
                st.busy_until_ms = monotonic_ms() + MEDIA_BUSY_MS; // the backend is giving: the next may follow at once
                if polled.bytes > 0 {
                    st.stats.frames_out += 1;
                    st.stats.coded[coding as usize] += 1;
                }
                if polled.eos {
                    push_v4l2_event(st, id, MEDIA_EVENT_EOS);
                    st.sessions[slot].draining = false;
                    break;
                }
            }
        }
    }

    /// Each decoder session's results into V4L2 events: its layout (SOURCE_CHANGE), its pictures (DQBUF of their
    /// CAPTURE buffers), a drain's end (EOS), counted.
    fn pump_decoders(&self, st: &mut State) {
        for slot in 0..MEDIA_SESSIONS {
            let Some(queue) = st.sessions[slot].events.clone() else { continue };
            let events: Vec<DecEvent> = queue.lock().unwrap_or_else(PoisonError::into_inner).drain(..).collect();
            for e in events {
                let s = &mut st.sessions[slot];
                let (id, codec) = (s.id, s.codec);
                match e {
                    DecEvent::Format(l) => {
                        if s.layout != Some(l) {
                            s.layout = Some(l);
                            logi!(
                                "decoder: session {id}: pictures {}x{} at a stride of {}",
                                l.width,
                                l.height,
                                l.stride
                            );
                            push_v4l2_event(st, id, MEDIA_EVENT_SOURCE_CHANGE);
                        }
                    }
                    DecEvent::Frame { index, bytes, pts, error, decode_us } => {
                        let i = index as usize;
                        if !s.capture.get(i).is_some_and(|b| b.queued) {
                            st.stats.errors += 1; // not a buffer the decoder had
                            continue;
                        }
                        s.capture[i].queued = false;
                        s.out += 1;
                        let flags = if error { MEDIA_BUF_FLAG_ERROR } else { 0 };
                        push_dqbuf(st, slot, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, i, bytes, flags, pts);
                        if error {
                            st.stats.dec_dropped += 1;
                        } else if let Some(c) = decoder_index(codec) {
                            st.stats.decoded[c] += 1;
                        }
                        if decode_us != 0 {
                            st.stats.dec_us += u64::from(decode_us);
                            st.stats.dec_timed += 1;
                            st.dec_report.max_us = st.dec_report.max_us.max(decode_us);
                        }
                    }
                    DecEvent::Eos => {
                        // what the drain did not bring out never will
                        st.stats.dec_dropped += s.fed.saturating_sub(s.out);
                        (s.fed, s.out, s.draining) = (0, 0, false);
                        push_v4l2_event(st, id, MEDIA_EVENT_EOS);
                    }
                }
            }
        }
    }

    /// One turn of the worker: pump, flush, and the 60 s lines when due. Whether an encoder session is open or an
    /// event waits (the worker then looks again [`MEDIA_IDLE_MS`] later at most: its guard), whether the busy time of
    /// a backend that cannot wake the worker runs (it is asked every 1 ms then), and the ms until a 60 s line is due
    /// (0: no window open).
    fn work(&self, dev: &MediaVirtio) -> (bool, bool, u64) {
        let mut st = self.state.borrow_mut();
        self.pump(&mut st);
        self.pump_decoders(&mut st);
        self.flush_events(dev, &mut st);
        let now = monotonic_ms();
        let backend = self.decoder.as_ref().map_or("", |d| d.name());
        let due = [report(&mut st, now), dec_report(&mut st, now, backend)];
        let report_in = due.iter().copied().filter(|d| *d != 0).min().unwrap_or(0);
        // Something may still come without a command: an encoder's backend may hold a frame, an event may wait for a
        // buffer (the guest's kick wakes the worker when it gives one). A decoder wakes the worker with each result,
        // an encoder's backend with each unit ([`Session::wakes`]); one that cannot is asked every 1 ms while busy.
        let open = !st.fifo.is_empty() || st.sessions.iter().any(|s| s.used && s.role != Role::Decoder);
        let polled = st.sessions.iter().any(|s| s.used && s.enc.is_some() && !s.wakes);
        (open, polled && now < st.busy_until_ms, report_in)
    }

    fn command(&self, reads: &[&Viov], writes: &[&Viov]) -> Option<u32> {
        let mut cmd = [0u8; 20];
        let cmd_len = reads[0].len() as usize;
        reads[0].read_at(0, &mut cmd);
        let op = le::u32(&cmd, 0);
        let mut st = self.state.borrow_mut();
        st.busy_until_ms = monotonic_ms() + MEDIA_BUSY_MS; // a command: the worker looks at once, and often for a while
        self.wake.wake();
        // CLOSE comes with no buffer for an answer (DroidVMVirtioMedia.c droidvm_media_close sends the command
        // alone), so it is done before the answer's buffer is asked for. Until 2026-11-15 a command without one was
        // dropped: no session was ever closed, and the device's sessions were used up by the guest's start-up check
        // and the first encode.
        if op == MEDIA_CMD_CLOSE {
            if cmd_len >= 12 {
                if let Some(slot) = st.sessions.iter().position(|s| s.used && s.id == le::u32(&cmd, 8)) {
                    st.end_session(slot); // closes the backend's session
                }
            }
            let resp = writes.first().filter(|w| w.len() >= 8)?;
            resp.fill(0, 0, 8);
            return Some(8);
        }
        let resp = *writes.first()?;
        let cap = resp.len() as usize;
        if cap < 8 {
            return None;
        }
        resp.fill(0, 0, cap);
        let status = |code: u32| {
            resp.write_at(0, &code.to_le_bytes());
        };
        let written = match op {
            MEDIA_CMD_OPEN => {
                if cap < 16 {
                    8
                } else if let Some(slot) = st.sessions.iter().position(|s| !s.used) {
                    st.next_session += 1;
                    let id = st.next_session;
                    st.sessions[slot] = MediaSession { used: true, id, ..Default::default() };
                    // the backend gets ready for an encoder (the helper starts at the guest's first session: its
                    // transport's check at boot), so the first encode does not wait for MediaCodec's first load
                    self.backend.prepare();
                    resp.write_at(8, &id.to_le_bytes());
                    16
                } else {
                    status(libc::ENOMEM as u32);
                    16
                }
            }
            MEDIA_CMD_IOCTL if cmd_len >= 16 => {
                let slot = st.sessions.iter().position(|s| s.used && s.id == le::u32(&cmd, 8));
                let nr = le::u32(&cmd, 12);
                let mut scratch = [0u8; std::mem::size_of::<MediaV4l2Format>()];
                let len = reads.get(1).map_or(0, |out| out.read_at(0, &mut scratch));
                let back = writes.get(1).map_or(0, |inp| (inp.len() as usize).min(scratch.len()));
                let code = match slot {
                    Some(slot) => self.ioctl(&mut st, slot, nr, &mut scratch, len),
                    None => libc::ENODEV as u32,
                };
                status(code);
                if let Some(inp) = writes.get(1) {
                    if code == 0 {
                        inp.write_at(0, &scratch[..back]);
                    }
                }
                8 + back as u32
            }
            MEDIA_CMD_MMAP if cmd_len >= 20 && cap >= 24 => {
                let offset = u64::from(le::u32(&cmd, 16));
                let found = st.session(le::u32(&cmd, 8)).and_then(|s| {
                    s.output.iter().chain(s.capture.iter()).find(|b| b.offset == offset).map(|b| (offset, b.size))
                });
                // The answer is the buffer's place in the pool, not its address: the guest adds its media-host-pool
                // base (DroidVMVirtioMedia.c droidvm_media_mmap: `at > pool_size` is refused, then gpa = pool_base
                // + at). Until 2026-11-15 the pool's guest address was added here too, which the guest's bound
                // refused for every buffer.
                let (at, size) = found.unwrap_or((0, 0));
                status(if size != 0 { 0 } else { libc::EINVAL as u32 });
                resp.write_at(8, &at.to_le_bytes());
                resp.write_at(16, &size.to_le_bytes());
                24
            }
            MEDIA_CMD_IOCTL | MEDIA_CMD_MMAP | MEDIA_CMD_MUNMAP => 8,
            _ => {
                status(libc::ENOTTY as u32);
                8
            }
        };
        Some(written)
    }

    /// The counters (`br_encoder_get_stats`).
    pub fn stats(&self) -> Stats {
        self.state.borrow().stats
    }

    /// The pool's bytes in use (the tests: every buffer given back).
    pub fn pool_in_use(&self) -> u64 {
        self.state.borrow().alloc.in_use()
    }
}

impl Device for MediaDevice {
    fn handle(&self, _dev: &MediaVirtio, queue: u32, _head: u16, iov: &[Viov]) -> Option<u32> {
        if queue != MEDIA_QUEUE_COMMAND {
            return Some(0);
        }
        let reads: Vec<&Viov> = iov.iter().filter(|v| !v.writable()).take(4).collect();
        let writes: Vec<&Viov> = iov.iter().filter(|v| v.writable()).take(4).collect();
        if reads.first().is_none_or(|r| r.len() < 8) {
            return Some(0);
        }
        Some(self.command(&reads, &writes).unwrap_or(0))
    }

    fn config_read(&self, _dev: &MediaVirtio, offset: u32, _size: u32) -> u32 {
        match offset {
            0 => MEDIA_CAP_VIDEO_M2M_MPLANE | MEDIA_CAP_STREAMING, // device_caps
            4 => 1,                                                // device_type: 1 = encoder (host rig)
            8..40 => {
                // card: "oneios-encoder", or "oneios-encoder+decoder" when it decodes (the guest's decoderDevice and
                // encoderDevice look for the two words)
                let name: &[u8] = if self.decodes() { b"oneios-encoder+decoder" } else { b"oneios-encoder" };
                let mut card = [0u8; 36];
                card[..name.len()].copy_from_slice(name);
                let at = (offset - 8) as usize;
                le::u32(&card, at)
            }
            _ => 0,
        }
    }

    /// eventq: the guest gave event buffers back; the worker fills them (an event may have waited for one).
    fn kick(&self, _dev: &MediaVirtio, _queue: u32) {
        self.wake.wake();
    }

    /// Both queues honour the guest's VIRTQ_AVAIL_F_NO_INTERRUPT (2026-11-25): the staged DroidVMVirtioMedia.c (edit 7)
    /// sets it on the command queue, whose answers it reads as soon as its notify returns (commands are answered inside
    /// it, [`Self::command`]), and clears it on the event queue, whose interrupt wakes its event worker. With
    /// oneios-codec-irq=0 it sets it on both and polls: the line then stays down, as nothing waits for it.
    fn quiet_queues(&self) -> u32 {
        1 << MEDIA_QUEUE_COMMAND | 1 << MEDIA_QUEUE_EVENT
    }

    fn reset(&self, _dev: &MediaVirtio) {
        let mut st = self.state.borrow_mut();
        for slot in 0..MEDIA_SESSIONS {
            if st.sessions[slot].used {
                st.end_session(slot);
            }
        }
        st.fifo.clear();
    }
}

impl Drop for MediaDevice {
    fn drop(&mut self) {
        // The worker holds the device weakly: it ends at its next wake-up.
        self.wake.stop.store(true, Ordering::Release);
        self.wake.wake();
    }
}

fn worker(dev: Weak<MediaVirtio>, wake: Arc<Wake>) {
    let mut line = WakesWindow::default();
    while !wake.stop.load(Ordering::Acquire) {
        let Some(dev) = dev.upgrade() else { break };
        let (open, busy, report_in) = {
            let guard = dev.lock();
            guard.work(&dev)
        };
        drop(dev);
        // the wake-ups line (2026-11-25): once every 30 s while the worker wakes, with no lock held
        let (line_in, said) = line.step(wake.counts(), monotonic_ms());
        if let Some(said) = said {
            logi!("{said}");
        }
        if busy {
            wake.tick(); // a backend that cannot wake the worker: asked again in 1 ms
        } else {
            // until a command, a kick, a decoder's result or an encoder's unit; with an encoder session open or an
            // event waiting, its guard at most; and no longer than a line's window (one wake-up for each)
            let guard = if open { MEDIA_IDLE_MS } else { 0 };
            let limit = [guard, report_in, line_in].into_iter().filter(|ms| *ms != 0).min().unwrap_or(0);
            wake.sleep(limit);
        }
    }
}

/// The "droidvm,virtio-media" device on xnuhost's transport (`br_encoder_create`). `pool_host` is the host mapping of
/// the media pool (guest physical `pool_gpa`, `pool_size` bytes) in which the MMAP'd V4L2 buffers live; the guest maps
/// the same pool through its device-tree media-host-pool property. With `decoder` (and `decoder=auto`) it decodes too.
/// None when the device could not be made.
///
/// # Safety
/// `pool_host` must point at `pool_size` bytes of guest RAM, mapped for as long as the device lives.
pub unsafe fn create(
    base: u64,
    spi: u32,
    backend: Option<Arc<dyn Backend>>,
    decoder: Option<Arc<dyn DecodeBackend>>,
    pool_host: *mut u8,
    pool_gpa: u64,
    pool_size: u64,
) -> Option<Arc<MediaVirtio>> {
    let (Some(backend), Some(host)) = (backend, NonNull::new(pool_host)) else {
        logi!("encoder: off: no media pool or backend");
        return None;
    };
    if pool_size < 2 << 20 {
        logi!("encoder: off: no media pool or backend");
        return None;
    }
    let name = backend.name();
    let wake = Arc::new(Wake::default());
    let device = MediaDevice {
        backend,
        decoder,
        pool: Pool { host, size: pool_size },
        state: RefCell::new(State::new(pool_size)),
        wake: Arc::clone(&wake),
    };
    let decodes = device.decodes();
    let decoder_name = device.decoder.as_ref().map(|d| d.name());
    let info =
        Info { device_id: MEDIA_VIRTIO_DEVICE_ID, features: 0, nqueues: MEDIA_NQUEUES, manual: 1 << MEDIA_QUEUE_EVENT };
    let dev = Virtio::create(base, spi, info, device, "virtio-media").ok()?;
    let weak = Arc::downgrade(&dev);
    if std::thread::Builder::new().name("encoder".into()).spawn(move || worker(weak, wake)).is_err() {
        loge!("encoder: no worker thread");
    }
    logi!(
        "encoder: on: {name} backend, media pool {} MiB at {pool_gpa:#x} ({MEDIA_SESSIONS} sessions)",
        pool_size >> 20
    );
    match (decoder_name, decodes) {
        (Some(d), true) => {
            let codecs: Vec<&str> =
                DECODER_CODECS.iter().filter(|c| decoder_format(true, **c)).map(|c| codec_name(*c)).collect();
            logi!("decoder: on: {d} backend, {} (decoder-codecs=)", codecs.join(", "));
        }
        (Some(_), false) => logi!("decoder: off (decoder=off): VideoToolbox decodes in software"),
        (None, _) => logi!("decoder: off: no decoder backend: VideoToolbox decodes in software"),
    }
    Some(dev)
}

/// The counters of a device (`br_encoder_get_stats`).
pub fn stats(dev: &MediaVirtio) -> Stats {
    dev.lock().stats()
}

/// The worker's wake-ups of a device since it was made, by why (the tests; the `media: 30 s` line says them).
pub fn wakes(dev: &MediaVirtio) -> Wakes {
    dev.lock().wake.counts()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wakes_line_comes_once_a_window_and_only_while_the_worker_wakes() {
        let mut w = WakesWindow::default();
        // a worker that never woke: no window, no line
        assert_eq!(w.step(Wakes::default(), 1000), (0, None));
        // the first wake-up opens it; the line 30 s later, with the counts since
        let a = Wakes { woken: 3, timed_out: 1, ticks: 0 };
        assert_eq!(w.step(a, 1000), (WAKES_REPORT_MS, None));
        let b = Wakes { woken: 903, timed_out: 21, ticks: 0 };
        assert_eq!(w.step(b, 1000 + WAKES_REPORT_MS - 1), (1, None));
        let (due, line) = w.step(b, 1000 + WAKES_REPORT_MS);
        assert_eq!(due, 0);
        assert_eq!(
            line.as_deref(),
            Some(
                "media: 30 s: worker woken 924: by data (commands, kicks, results, coded units) 903, by the timeout \
                 21; 1 ms looks 0"
            )
        );
        // nothing since: no window again; a 1 ms poller's looks show
        assert_eq!(w.step(b, 99_000), (0, None));
        let c = Wakes { ticks: 2000, ..b };
        assert_eq!(w.step(c, 100_000), (WAKES_REPORT_MS, None));
        let line = w.step(c, 130_000).1.expect("the line");
        assert!(line.contains("worker woken 2000: ") && line.ends_with("1 ms looks 2000"), "{line}");
    }

    #[test]
    fn a_wake_up_while_the_worker_works_is_kept_for_its_next_sleep() {
        let wake = Wake::default();
        assert!(!wake.sleep(1), "nothing for it: the timeout");
        wake.wake();
        assert!(wake.sleep(10_000), "woken before it slept: at once");
        wake.wake();
        wake.tick(); // a 1 ms look keeps it too
        assert!(wake.sleep(0), "no limit, but woken");
        assert_eq!(wake.counts(), Wakes { woken: 2, timed_out: 1, ticks: 1 });
    }

    #[test]
    fn the_60_s_line_comes_only_while_pictures_are_coded() {
        let mut st = State::new(1 << 20);
        // nothing coded: no window, whatever else moves
        st.stats.frames_in = 3;
        assert_eq!(report(&mut st, 1000), 0);
        // a picture coded: the window opens, the line is due 60 s later
        st.stats.coded[Coding::Main10 as usize] += 2;
        st.stats.coded[Coding::TenAsEight as usize] += 1;
        st.stats.sessions += 1;
        assert_eq!(report(&mut st, 5000), REPORT_MS);
        assert_eq!(report(&mut st, 5000 + REPORT_MS - 1), 1);
        st.stats.refused += 1;
        assert_eq!(
            report_text(&st.report.from, &st.stats),
            "encoder: 60 s: coded 8-bit 0, 10-bit as Main10 2, 10-bit as 8-bit 1; opens refused 1; sessions 1"
        );
        assert_eq!(report(&mut st, 5000 + REPORT_MS), 0); // said: the window is closed
        assert_eq!((st.report.from, st.report.since_ms), (st.stats, 0));
        // nothing more coded: no new window
        assert_eq!(report(&mut st, 200_000), 0);
        st.stats.coded[Coding::Eight as usize] += 1;
        assert_eq!(report(&mut st, 300_000), REPORT_MS);
        assert_eq!(
            report_text(&st.report.from, &st.stats),
            "encoder: 60 s: coded 8-bit 1, 10-bit as Main10 0, 10-bit as 8-bit 0; opens refused 0; sessions 0"
        );
    }

    #[test]
    fn the_decoders_60_s_line_comes_only_while_pictures_are_decoded() {
        let mut st = State::new(1 << 20);
        st.stats.dec_fed = 5; // fed, nothing out yet: no window
        assert_eq!(dec_report(&mut st, 1000, "x"), 0);
        st.stats.decoded[2] += 58;
        st.stats.decoded[0] += 2;
        st.stats.dec_fed += 57;
        st.stats.dec_dropped += 1;
        st.stats.dec_sessions += 1;
        (st.stats.dec_us, st.stats.dec_timed) = (60 * 4500, 60);
        assert_eq!(dec_report(&mut st, 2000, "x"), REPORT_MS);
        st.dec_report.max_us = 12_001;
        assert_eq!(
            dec_report_text(&st.dec_report.from, &st.stats, st.dec_report.max_us, "mediacodec-helper"),
            "decoder: 60 s: decoded H.264 2, HEVC 0, VP9 58, AV1 0 of 57 fed; decode time avg 4.5 ms, max 13 ms; \
             dropped 1; sessions 1, opens refused 0 (mediacodec-helper)"
        );
        assert_eq!(dec_report(&mut st, 2000 + REPORT_MS, "x"), 0);
        assert_eq!((st.dec_report.since_ms, st.dec_report.max_us), (0, 0));
        assert_eq!(dec_report(&mut st, 900_000, "x"), 0); // nothing new
    }

    #[test]
    fn the_pool_is_given_first_fit_and_taken_back() {
        let mut a = Alloc::new(64 << 10);
        let (x, y, z) = (a.take(4096).unwrap(), a.take(5000).unwrap(), a.take(4096).unwrap());
        assert_eq!((x, y, z), (0, 4096, 12288));
        assert_eq!(a.take(52 << 10), None); // 48 KiB left of 64
        a.give(y);
        assert_eq!(a.take(8192), Some(4096)); // the gap y left
        assert_eq!(a.take(52 << 10), None);
        assert_eq!(a.take(48 << 10), Some(16384));
        assert_eq!(a.in_use(), 64 << 10);
        for o in [x, 4096, z, 16384] {
            a.give(o);
        }
        assert_eq!(a.in_use(), 0);
        assert_eq!(a.take(u64::MAX), None);
    }
}
