// SPDX-License-Identifier: MIT
//! The device-side encoder backend that runs the real encoder in the oneios-encoder helper process
//! ([`crate::helper`]), because MediaCodec is a shared library and xnuhost is static (backend_helper.c). It spawns the
//! helper (at the first session), multiplexes the encoder sessions over a UNIX socket ([`crate::link`]), and buffers
//! the coded access units the helper returns for poll(). Static, libc only, like the rest of the bridges.
//!
//! Writes to the helper go under a lock of their own, not the state's: in the C one lock covered both, so a device
//! writing a frame while the helper wrote coded units back could stop the receive thread at the lock with the socket
//! full both ways. The two are never held together the other way round (the state's, then the writer's, only).
//!
//! A vCPU calls the backend (the device's commands are answered on the vCPU, [`crate::media_device`]), so nothing
//! here may wait for the helper without a limit: a vCPU in a host call takes no interrupt, and the guest's debugger
//! cross-call (a stackshot: spindump, ReportCrash) panics after debug-ack (5 s on the phone, hosted 0017) when one
//! vCPU does not answer. Two phone boots of v0.1.16 (2026-11-23, the C runtime) froze that way: the helper never
//! answered an OPEN (MediaCodec's open hung in it), the vCPU waited here for ever with the virtio-media device held,
//! xnuhost's main loop then waited for that device, and 80-90 s later a stackshot panicked ("Debugger synch pending on
//! cpu 1", then "on cpu 2"). The limits: the HELLO and each write [`HELPER_IO_MS`], an open's reply [`OPEN_MS`]
//! ([`HelperBackend::with_open_ms`]); a helper that misses an open's reply (or takes no message) is killed (it is stuck
//! in MediaCodec), and the next open starts a new one, [`HELPER_HANGS`] times at most a session. [`stop`] (xnuhost's
//! stop) kills the helper, also one that hangs and would never see its socket's end. Session ids are never reused, so
//! a late reply cannot be taken for a new open.
//!
//! The decoder (2026-11-24) is the same helper's: [`HelperBackend`] is a [`DecodeBackend`] too. Right after the HELLO
//! the device sends the media pool (ENC_MSG_POOL, the RAM's memfd by SCM_RIGHTS, [`HelperBackend::set_pool`]): the
//! helper writes the decoded pictures straight into the guest's CAPTURE buffers there, and says each one
//! (DEC_MSG_FRAME), which the receive thread hands to the session's [`DecodeSink`]; the frames go over the socket
//! (they are small).
use crate::backend::{Backend, Coding, Polled, Session, Wakeup};
use crate::decoder::{CapLayout, DecEvent, DecodeBackend, DecodeSink, Decoder};
use crate::link::*;
use crate::pod::Pod;
use onecamera::spawn::{reap_nowait, spawn_with_fd3};
use std::collections::VecDeque;
use std::io::Read;
use std::net::Shutdown;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, TryLockError, Weak};
use std::thread::JoinHandle;
use std::time::Duration;
use xh_core::thread::lock;
use xh_core::{loge, logi};

const HELPER_MAX_SESSIONS: usize = 8;
const HELPER_CODED_RING: usize = 32;
/// The HELLO's and each write's limit (ms; SO_RCVTIMEO for the HELLO only, SO_SNDTIMEO for every write).
pub const HELPER_IO_MS: u32 = 1000;
/// An open's reply's limit (ms; `br_encoder_helper_open_ms`): MediaCodec's open of an HEVC encoder takes tens of ms.
pub const OPEN_MS: u32 = 2500;
/// Helpers killed for a hang before no more are started this session.
pub const HELPER_HANGS: u32 = 2;

struct Coded {
    bytes: Vec<u8>,
    pts: i64,
    key: bool,
    eos: bool,
}

#[derive(Default)]
struct Slot {
    used: bool,
    /// the session's id on the link (never 0, never reused)
    id: u32,
    open_ok: bool,
    open_done: bool,
    /// how the helper opened it (the OPEN_REPLY's b)
    coding: Coding,
    ring: VecDeque<Coded>,
    got_eos: bool,
    /// a decoder session's results go here (the receive thread calls it, the state's lock not held)
    sink: Option<DecodeSink>,
    /// an encoder session's: the device's worker, woken (the state's lock not held) at each coded unit and EOS and
    /// when the helper ends, so it waits for them instead of asking every millisecond (2026-11-25)
    wake: Option<Wakeup>,
}

/// The media pool as the helper maps it: the RAM's memfd (xnuhost's, open for the process's life), the pool's offset
/// in it and its size.
#[derive(Clone, Copy, Debug)]
pub struct PoolFd {
    pub fd: RawFd,
    pub offset: u64,
    pub size: u64,
}

#[derive(Default)]
struct State {
    started: bool,
    gone: bool,
    /// this helper was killed (it hung, or the stop): its receive thread sees the socket's end soon
    killed: bool,
    /// helpers killed for a hang this session
    hangs: u32,
    off_said: bool,
    next_id: u32,
    pid: libc::pid_t,
    /// a third end of the helper's socket, for the shutdown of a kill (the writer's may be held by a write)
    sock: Option<UnixStream>,
    receiver: Option<JoinHandle<()>>,
    slots: [Slot; HELPER_MAX_SESSIONS],
}

impl State {
    fn slot_of(&mut self, id: u32) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.used && s.id == id)
    }

    /// A helper that is up and was not killed.
    fn alive(&self) -> bool {
        self.started && !self.gone && !self.killed
    }
}

struct Shared {
    path: String,
    open_ms: u32,
    state: Mutex<State>,
    /// open replies and new coded buffers
    cond: Condvar,
    /// the device's end of the socket, once the helper said hello; writes under this lock
    writer: Mutex<Option<UnixStream>>,
    /// the helper's pid while it may run (0: none), for a stop that finds the state locked
    pid: AtomicI32,
    /// the media pool, sent to each helper after its HELLO (none: the helper decodes nothing)
    pool: Mutex<Option<PoolFd>>,
}

fn kill_now(pid: libc::pid_t) {
    if pid <= 0 {
        return; // 0 or -1 would be a process group, or every process
    }
    // SAFETY: a plain call on our own child.
    unsafe { libc::kill(pid, libc::SIGKILL) };
}

/// Reaps `pid` if it has ended, without waiting; whether it was.
fn reaped(pid: libc::pid_t) -> bool {
    // SAFETY: a plain call on our own child.
    pid > 0 && unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) } == pid
}

/// Kills `pid` and reaps it (a SIGKILLed child ends at once).
fn kill_and_wait(pid: libc::pid_t) {
    kill_now(pid);
    // SAFETY: a plain call on our own child.
    unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) };
}

fn errno(e: &std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(libc::EIO)
}

/// What the helper's main thread waits in (its /proc wchan and state), for the line that says it hung.
fn helper_wait_state(pid: libc::pid_t) -> String {
    // At most 63 bytes, as the C's buffer took (a kernel symbol: ASCII).
    let mut wchan = xh_core::fs::read_line(format!("/proc/{pid}/wchan")).unwrap_or_default();
    let mut end = wchan.len().min(63);
    while !wchan.is_char_boundary(end) {
        end -= 1;
    }
    wchan.truncate(end);
    if wchan.is_empty() {
        wchan = "?".into();
    }
    let stat = xh_core::fs::read_line(format!("/proc/{pid}/stat")).unwrap_or_default();
    let state = stat
        .rfind(')')
        .and_then(|close| stat[close + 1..].strip_prefix(' '))
        .and_then(|rest| rest.chars().next())
        .unwrap_or('?');
    format!("state {state}, waiting in {wchan}")
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn send(&self, m: EncMsg, payload: &[u8]) -> Result<(), i32> {
        let mut writer = lock(&self.writer);
        match writer.as_mut() {
            Some(w) => m.send(w, payload).map_err(|_| libc::EIO),
            None => Err(libc::EPIPE),
        }
    }

    /// The helper hung: killed. Its receive thread sees the socket's end and marks it gone; the next open starts
    /// another.
    fn kill(&self, st: &mut State, why: &str) {
        let state = helper_wait_state(st.pid);
        st.hangs += 1;
        loge!(
            "encoder: the helper (pid {}) {why} ({state}): killed ({} of {HELPER_HANGS}); the next open starts a new one",
            st.pid,
            st.hangs
        );
        kill_now(st.pid);
        st.killed = true;
        if let Some(sock) = &st.sock {
            let _ = sock.shutdown(Shutdown::Both);
        }
    }

    /// ENC_MSG_POOL: the pool's memfd as SCM_RIGHTS on the header (one sendmsg of the 32 bytes), its offset and size.
    fn send_pool(&self, to: &UnixStream) -> Result<(), i32> {
        let pool = *lock(&self.pool);
        let (offset, size) = pool.map_or((0, 0), |p| (p.offset, p.size));
        let m = EncMsg { pts: offset as i64, a: size as u32, b: (size >> 32) as u32, ..EncMsg::new(ENC_MSG_POOL, 0) };
        // SAFETY: xnuhost's RAM memfd, open for the process's life (set_pool's promise).
        let fds: Vec<BorrowedFd<'_>> = pool.map(|p| unsafe { BorrowedFd::borrow_raw(p.fd) }).into_iter().collect();
        let header = m.as_bytes();
        match xh_core::fd::send_with_fds(to.as_fd(), &[std::io::IoSlice::new(header)], &fds, libc::MSG_NOSIGNAL) {
            Ok(n) if n == header.len() => Ok(()),
            Ok(_) => Err(libc::EIO),
            Err(e) => Err(errno(&e)),
        }
    }

    /// The receive loop: open replies, coded buffers and EOS from the helper; a decoder's results to its sink.
    fn receive(&self, mut from: UnixStream) {
        while let Ok(m) = EncMsg::receive(&mut from) {
            let mut payload = vec![0u8; m.len as usize];
            if from.read_exact(&mut payload).is_err() {
                break;
            }
            // The helper's own lines (its binder threads, its warm-up, an open that takes long): into xnuhost's log.
            if m.type_ == ENC_MSG_LOG {
                let line = String::from_utf8_lossy(&payload);
                if m.a != 0 {
                    loge!("encoder: helper: {line}");
                } else {
                    logi!("encoder: helper: {line}");
                }
                continue;
            }
            let mut st = self.lock();
            let mut result = None;
            let mut woken = None;
            if let Some(s) = st.slot_of(m.session) {
                match m.type_ {
                    ENC_MSG_OPEN_REPLY => {
                        (s.open_ok, s.open_done, s.coding) = (m.session != 0 && m.a != 0, true, Coding::from_u32(m.b));
                    }
                    ENC_MSG_CODED if !payload.is_empty() && s.ring.len() < HELPER_CODED_RING => {
                        s.ring.push_back(Coded {
                            bytes: payload,
                            pts: m.pts,
                            key: m.a & 1 != 0,
                            eos: (m.a >> 1) & 1 != 0,
                        });
                        woken = s.wake.clone(); // the unit is in the ring before the worker looks
                    }
                    // a decoder's results (2026-11-24): to its sink, below, the lock let go
                    ENC_MSG_EOS if s.sink.is_some() => result = s.sink.clone().map(|k| (k, DecEvent::Eos)),
                    ENC_MSG_EOS => {
                        s.got_eos = true;
                        woken = s.wake.clone();
                    }
                    DEC_MSG_FORMAT => {
                        let layout = CapLayout { width: m.a, height: m.b, stride: m.c };
                        result = s.sink.clone().map(|k| (k, DecEvent::Format(layout)));
                    }
                    DEC_MSG_FRAME => {
                        let decode_us = payload.get(..4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                        let frame =
                            DecEvent::Frame { index: m.a, bytes: m.b, pts: m.pts, error: m.c & 1 != 0, decode_us };
                        result = s.sink.clone().map(|k| (k, frame));
                    }
                    _ => {}
                }
            }
            self.cond.notify_all();
            drop(st);
            if let Some((sink, event)) = result {
                sink(event);
            }
            if let Some(wake) = woken {
                wake();
            }
        }
        // The last thing this thread does with the state: a start that finds `gone` may join it at once. The encoder
        // sessions' workers look once more (nothing comes from this helper any more: their polls say none).
        let wakes: Vec<Wakeup> = {
            let mut st = self.lock();
            st.gone = true;
            st.slots.iter().filter(|s| s.used).filter_map(|s| s.wake.clone()).collect()
        };
        self.cond.notify_all();
        for wake in wakes {
            wake();
        }
    }

    /// Spawns the helper and waits for its HELLO, or starts a new one after the last one ended (the state locked by
    /// the caller). An errno on failure.
    fn start(self: &Arc<Self>, st: &mut State) -> Result<(), i32> {
        if st.started && !st.gone {
            return if st.killed { Err(libc::EPIPE) } else { Ok(()) }; // a killed helper is not gone yet: this open fails
        }
        if st.started {
            // The last helper ended (it was killed, or it died): its receive thread has set `gone`, its last use of
            // the state, so the join under the lock returns at once.
            if let Some(t) = st.receiver.take() {
                let _ = t.join();
            }
            *lock(&self.writer) = None;
            st.sock = None;
            if st.pid > 0 {
                reap_nowait(st.pid);
            }
            st.pid = 0;
            self.pid.store(0, Ordering::Relaxed);
            (st.started, st.gone, st.killed) = (false, false, false);
            for s in &mut st.slots {
                *s = Slot::default();
            }
        }
        if st.hangs >= HELPER_HANGS {
            if !st.off_said {
                loge!(
                    "encoder: the helper hung {} times: no more helpers this session (VideoToolbox keeps its software encoder)",
                    st.hangs
                );
            }
            st.off_said = true;
            return Err(libc::EIO);
        }
        let (ours, theirs): (OwnedFd, OwnedFd) =
            xh_core::fd::socket_pair(libc::SOCK_STREAM | libc::SOCK_CLOEXEC).map_err(|e| errno(&e))?;
        let mut args = vec!["3"];
        args.extend(crate::switches::helper_args()); // encoder-10bit=, encoder-warmup= (docs/switches.md)
        let pid = spawn_with_fd3(&self.path, &args, theirs.as_fd());
        drop(theirs);
        let pid = pid?;
        let mut stream = UnixStream::from(ours);
        let io = Some(Duration::from_millis(u64::from(HELPER_IO_MS)));
        // The HELLO only: the receive thread waits without a limit. Every write: a helper that stops reading fails it.
        let timeouts = stream.set_read_timeout(io).and_then(|()| stream.set_write_timeout(io));
        let hello = timeouts.and_then(|()| EncMsg::receive(&mut stream));
        if !hello.is_ok_and(|h| h.type_ == ENC_MSG_HELLO && h.a == ENC_LINK_VERSION) {
            loge!("encoder: the helper {} (pid {pid}) sent no HELLO in {HELPER_IO_MS} ms: killed", self.path);
            kill_and_wait(pid);
            return Err(libc::EPROTO);
        }
        // The media pool, always (none: no descriptor, size 0): the helper maps it for its decoders (2026-11-24).
        if let Err(e) = self.send_pool(&stream) {
            loge!("encoder: the helper {} (pid {pid}) took no pool: {}: killed", self.path, xh_core::sys::strerror(e));
            kill_and_wait(pid);
            return Err(e);
        }
        let ends = stream.set_read_timeout(None).and_then(|()| Ok((stream.try_clone()?, stream.try_clone()?)));
        let (reader, sock) = match ends {
            Ok(ends) => ends,
            Err(e) => {
                kill_and_wait(pid);
                return Err(errno(&e));
            }
        };
        let shared = Arc::clone(self);
        let receiver = match xh_core::thread::spawn("encoder-link", move || shared.receive(reader)) {
            Ok(t) => t,
            Err(_) => {
                kill_and_wait(pid);
                return Err(libc::EAGAIN);
            }
        };
        *lock(&self.writer) = Some(stream);
        st.pid = pid;
        self.pid.store(pid, Ordering::Relaxed);
        st.sock = Some(sock);
        st.receiver = Some(receiver);
        st.started = true;
        logi!("encoder: helper {} (pid {pid})", self.path);
        Ok(())
    }

    /// xnuhost's stop: the helper goes with the session, also one that hangs in MediaCodec and would never see its
    /// socket's end. Never waits: the lock is only tried (a vCPU may be in a bounded wait), the kill is SIGKILL.
    fn stop(&self) {
        match self.state.try_lock() {
            Ok(mut st) => self.stop_locked(&mut st),
            Err(TryLockError::Poisoned(p)) => self.stop_locked(&mut p.into_inner()),
            Err(TryLockError::WouldBlock) => {
                // Busy: the kill alone; the receive thread sees the socket's end and marks the helper gone.
                let pid = self.pid.load(Ordering::Relaxed);
                if pid > 0 {
                    kill_now(pid);
                }
            }
        }
    }

    fn stop_locked(&self, st: &mut State) {
        if st.started && !st.gone && st.pid > 0 {
            kill_now(st.pid);
            if reaped(st.pid) {
                st.pid = 0; // never killed or reaped again: the number may be another process's now
                self.pid.store(0, Ordering::Relaxed);
            }
            st.killed = true;
        }
    }
}

/// Every helper backend of this process, for [`stop`] (xnuhost's stop has no handle on the backend it made).
fn all() -> &'static Mutex<Vec<Weak<Shared>>> {
    static ALL: OnceLock<Mutex<Vec<Weak<Shared>>>> = OnceLock::new();
    ALL.get_or_init(Mutex::default)
}

/// xnuhost's stop (`br_encoder_helper_stop`): kills the helper of every helper backend of this process, also one
/// that hangs in MediaCodec and would never see its socket's end. Never waits for a helper or a vCPU.
pub fn stop() {
    let backends: Vec<Arc<Shared>> = lock(all()).iter().filter_map(Weak::upgrade).collect();
    for b in backends {
        b.stop();
    }
}

/// The backend ("mediacodec-helper", `br_encoder_backend_helper`): the helper at `path`.
pub struct HelperBackend {
    shared: Arc<Shared>,
}

impl HelperBackend {
    /// `path` is the helper binary (runtime/bin/oneios-encoder); it is started at the first session.
    pub fn new(path: &str) -> Arc<HelperBackend> {
        HelperBackend::with_open_ms(path, OPEN_MS)
    }

    /// The same, with an open's reply waited for at most `open_ms` (the tests' shorter limit).
    pub fn with_open_ms(path: &str, open_ms: u32) -> Arc<HelperBackend> {
        let shared = Arc::new(Shared {
            path: path.to_owned(),
            open_ms,
            state: Mutex::default(),
            cond: Condvar::new(),
            writer: Mutex::new(None),
            pid: AtomicI32::new(0),
            pool: Mutex::new(None),
        });
        let mut list = lock(all());
        list.retain(|w| w.strong_count() > 0);
        list.push(Arc::downgrade(&shared));
        Arc::new(HelperBackend { shared })
    }

    /// This backend's part of [`stop`]: its helper killed (never waits).
    pub fn stop(&self) {
        self.shared.stop();
    }

    /// The media pool the helper's decoders write into (2026-11-24): `fd` the RAM's memfd, the pool `size` bytes at
    /// `offset` in it. Each helper started from now on gets it. The descriptor must stay open for the process's life
    /// (xnuhost's RAM memfd does).
    pub fn set_pool(&self, fd: RawFd, offset: u64, size: u64) {
        *lock(&self.shared.pool) = Some(PoolFd { fd, offset, size });
    }

    /// An OPEN or DEC_OPEN (`open`, its session filled in here) of a new session, and its reply (OPEN_MS at most; a
    /// helper that misses it is killed). The session's id and coding, None when refused.
    fn open_session(&self, mut open: EncMsg, payload: &[u8], sink: Option<DecodeSink>) -> Option<(u32, Coding)> {
        let shared = &self.shared;
        let mut st = shared.lock();
        shared.start(&mut st).ok()?;
        let index = st.slots.iter().position(|s| !s.used)?;
        st.next_id = st.next_id.wrapping_add(1).max(1); // 0 is no session
        let id = st.next_id;
        st.slots[index] = Slot { used: true, id, sink, ..Slot::default() };
        drop(st);
        open.session = id;
        let sent = shared.send(open, payload);
        let mut st = shared.lock();
        if sent.is_err() {
            if let Some(s) = st.slot_of(id) {
                *s = Slot::default();
            }
            if st.alive() {
                shared.kill(&mut st, "took no OPEN");
            }
            return None;
        }
        let limit = Duration::from_millis(u64::from(shared.open_ms));
        st = shared
            .cond
            .wait_timeout_while(st, limit, |st| st.slots[index].id == id && !st.slots[index].open_done && !st.gone)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
        let (done, ok, coding) =
            st.slot_of(id).map_or((false, false, Coding::Eight), |s| (s.open_done, s.open_done && s.open_ok, s.coding));
        if !done && st.alive() {
            shared.kill(&mut st, &format!("did not answer an open in {} ms", shared.open_ms));
        }
        if !ok {
            if let Some(s) = st.slot_of(id) {
                *s = Slot::default();
            }
            return None;
        }
        Some((id, coding))
    }
}

impl DecodeBackend for HelperBackend {
    fn name(&self) -> &'static str {
        "mediacodec-helper"
    }

    /// A decoder in the helper (2026-11-24): its results come back through the receive thread to `sink`.
    fn open_decoder(&self, codec: u32, width: u32, height: u32, sink: DecodeSink) -> Option<Box<dyn Decoder>> {
        if lock(&self.shared.pool).is_none() {
            return None; // the helper would have nowhere to put the pictures
        }
        let open = EncMsg { a: codec, b: width, c: height, ..EncMsg::new(DEC_MSG_OPEN, 0) };
        let (id, coding) = self.open_session(open, &[], Some(sink))?;
        Some(Box::new(HelperDecoder(HelperSession { shared: Arc::clone(&self.shared), id, coding })))
    }
}

/// A decoder session in the helper: its messages; its close is the session's (Drop).
struct HelperDecoder(HelperSession);

impl Decoder for HelperDecoder {
    fn feed(&mut self, data: &[u8], pts: i64) -> Result<(), i32> {
        let m = EncMsg { pts, ..EncMsg::new(DEC_MSG_FEED, self.0.id) };
        self.0.send(m, data, "did not take a frame to decode")
    }

    fn capture(&mut self, index: u32, offset: u64, size: u64) {
        let m = EncMsg { a: index, pts: offset as i64, b: size as u32, ..EncMsg::new(DEC_MSG_CAPTURE, self.0.id) };
        let _ = self.0.send(m, &[], "did not take a CAPTURE buffer");
    }

    fn reset_capture(&mut self) {
        let _ = self.0.send(EncMsg::new(DEC_MSG_RESET, self.0.id), &[], "did not take a reset");
    }

    fn drain(&mut self) -> Result<(), i32> {
        self.0.send(EncMsg::new(ENC_MSG_DRAIN, self.0.id), &[], "did not take a drain")
    }

    fn restart(&mut self) -> Result<(), i32> {
        self.0.send(EncMsg::new(DEC_MSG_START, self.0.id), &[], "did not take a start")
    }
}

struct HelperSession {
    shared: Arc<Shared>,
    id: u32,
    coding: Coding,
}

impl Backend for HelperBackend {
    fn name(&self) -> &'static str {
        "mediacodec-helper"
    }

    fn open(&self, codec: u32, input: u32, width: u32, height: u32) -> Option<Box<dyn Session>> {
        let open = EncMsg { a: codec, b: width, c: height, ..EncMsg::new(ENC_MSG_OPEN, 0) };
        let (id, coding) = self.open_session(open, &input.to_le_bytes(), None)?;
        Some(Box::new(HelperSession { shared: Arc::clone(&self.shared), id, coding }))
    }

    /// The helper started at a guest's first virtio-media session (its transport's check at boot), not at the first
    /// encode: it starts its binder threads and opens one HEVC encoder at once (its warm-up line), so the first
    /// encode does not wait for MediaCodec's first load. A vCPU calls it: the HELLO's limit only.
    fn prepare(&self) {
        if !crate::switches::warmup() {
            return; // encoder-warmup=0: the helper starts at the first encode, as before b3da193
        }
        let shared = &self.shared;
        let mut st = shared.lock();
        if !st.alive() && st.hangs < HELPER_HANGS {
            let _ = shared.start(&mut st);
        }
    }
}

impl HelperSession {
    /// A session of the helper that runs now (a killed helper's sessions are cleared when the next one starts, and
    /// ids are never reused).
    fn live(&self) -> bool {
        self.shared.lock().slot_of(self.id).is_some()
    }

    /// A message of this session's; a write that fails on a helper still thought alive kills it (`why`).
    fn send(&self, m: EncMsg, payload: &[u8], why: &str) -> Result<(), i32> {
        if !self.live() {
            return Err(libc::EPIPE); // its helper ended: never to the next one
        }
        let rc = self.shared.send(m, payload).map_err(|_| libc::EIO);
        if rc.is_err() {
            let mut st = self.shared.lock();
            if st.alive() {
                self.shared.kill(&mut st, why);
            }
        }
        rc
    }
}

impl Session for HelperSession {
    fn coding(&self) -> Coding {
        self.coding
    }

    fn feed(&mut self, frame: &[u8], pts_us: i64, force_key: bool) -> Result<(), i32> {
        let m = EncMsg { pts: pts_us, a: u32::from(force_key), ..EncMsg::new(ENC_MSG_FEED, self.id) };
        self.send(m, frame, "did not take a frame").map_err(|_| libc::EIO)
    }

    fn drain(&mut self) -> Result<(), i32> {
        self.send(EncMsg::new(ENC_MSG_DRAIN, self.id), &[], "did not take a drain").map_err(|_| libc::EIO)
    }

    /// The receive thread wakes the worker with each coded unit and EOS of this session, and when the helper ends
    /// (2026-11-25). A session whose helper already ended gets nothing more: no wake-up is needed for it either.
    fn wakes(&mut self, wake: Wakeup) -> bool {
        if let Some(s) = self.shared.lock().slot_of(self.id) {
            s.wake = Some(wake);
        }
        true
    }

    fn poll(&mut self, buf: &mut [u8]) -> Result<Polled, i32> {
        let mut st = self.shared.lock();
        // A session of a helper that ended: nothing comes any more (an error here would count at every worker poll).
        let Some(s) = st.slot_of(self.id) else { return Ok(Polled::default()) };
        if let Some(c) = s.ring.pop_front() {
            // A unit larger than the guest's coded buffer is dropped and said once: left at the ring's head it came
            // back as an error at every poll of the device's worker, for as long as the session lived, and no later
            // frame got out.
            if c.bytes.len() > buf.len() {
                return Err(libc::ENOSPC);
            }
            buf[..c.bytes.len()].copy_from_slice(&c.bytes);
            return Ok(Polled { bytes: c.bytes.len(), key: c.key, pts_us: c.pts, eos: c.eos });
        }
        if s.got_eos {
            s.got_eos = false;
            return Ok(Polled { eos: true, ..Polled::default() });
        }
        Ok(Polled::default())
    }
}

impl Drop for HelperSession {
    fn drop(&mut self) {
        let alive = {
            let st = self.shared.lock();
            st.alive() && st.slots.iter().any(|s| s.used && s.id == self.id)
        };
        if alive {
            let _ = self.shared.send(EncMsg::new(ENC_MSG_CLOSE, self.id), &[]);
        }
        if let Some(s) = self.shared.lock().slot_of(self.id) {
            *s = Slot::default();
        }
    }
}
