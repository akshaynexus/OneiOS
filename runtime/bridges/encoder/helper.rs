// SPDX-License-Identifier: MIT
//! oneios-encoder: the video encoder helper process (bin/oneios-encoder in the runtime package; helper.c). MediaCodec
//! is a shared library, so it cannot run inside the static xnuhost; this helper does, exactly as oneios-camera and
//! oneios-audio do. It reads the device's messages ([`crate::link`]) on the inherited socket (fd given as argv[1],
//! default 3), runs the real encoder (backend_ndk on Android; the stand-in on a host, for the test, or on Android with
//! ONEIOS_ENCODER_STUB set), and streams the coded access units back. One session per virtio-media session; the
//! session's frames are drained after each feed and after a drain.
//!
//! Since 2026-11-23 (the phone's session of 2026-11-23 18:24: every open of the helper hung in a futex until the
//! device killed it at 2.5 s, `refused 96x96`):
//! - On Android it starts its binder threads before anything else (libbinder_ndk's pool, and libhidlbase's when that
//!   is loaded): a plain executable has none, and Codec2 calls its client back over binder (the camera helper's fault
//!   of 2026-11-17, `bridges/camera/README.md`: without them a callback waits for ever). The line `encoder: helper:
//!   binder threads: ...` says what started.
//! - It opens one HEVC encoder at once after its HELLO (the warm-up, on a thread of its own), so MediaCodec's first
//!   load is not in the first encode's 2.5 s; `encoder: helper: warm-up: ...` says how long it took, and which
//!   encoder and sizes the codec store gives.
//! - A watchdog thread names the MediaCodec call an open waits in, at 1 s and 2 s (`encoder: helper: an open of HEVC
//!   96x96 (NV12) has waited 1000 ms in AMediaCodec_configure`), and an open that came back after 1 s says how long it
//!   took. Nothing is written while opens are fast.
//!
//! Since 2026-11-24 it decodes too (README.md "The decoder"): right after its HELLO the device sends the media pool
//! (ENC_MSG_POOL: the RAM's memfd by SCM_RIGHTS), which the helper maps; a decoder session (DEC_MSG_OPEN) is MediaCodec
//! in asynchronous mode on Android ([`crate::decode_ndk`]; the stand-in on a host), which writes each picture straight
//! into a CAPTURE buffer the device gave it (DEC_MSG_CAPTURE) and says it (DEC_MSG_FRAME) from MediaCodec's own thread.
use crate::backend::{Backend, Polled, Session};
use crate::backend_stub::StubBackend;
use crate::decoder::{DecEvent, DecodeBackend, DecodeSink, Decoder, PoolMem, StubDecodeBackend};
use crate::link::*;
use crate::pod::Pod;
use crate::proto::{MEDIA_PIXFMT_HEVC, MEDIA_PIXFMT_NV12, MEDIA_PIXFMT_P010};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

const HELPER_SESSIONS: usize = 8;
const CODED_CAP: usize = 8 << 20;
/// The watchdog's lines: an open that has waited this long (ms), each.
const WATCH_MS: [u64; 2] = [1000, 2000];

fn stub_wanted() -> bool {
    !cfg!(target_os = "android") || std::env::var_os("ONEIOS_ENCODER_STUB").is_some()
}

fn backend() -> Box<dyn Backend> {
    #[cfg(target_os = "android")]
    if !stub_wanted() {
        return Box::new(crate::backend_ndk::NdkBackend);
    }
    Box::new(StubBackend) // the host build: no MediaCodec on the box
}

/// The decoder backend over the pool (2026-11-24): MediaCodec on Android, the stand-in on a host.
fn decode_backend(pool: PoolMem, link: &Link) -> Box<dyn DecodeBackend> {
    #[cfg(target_os = "android")]
    if !stub_wanted() {
        let link = Arc::clone(link);
        let log: crate::decode_ndk::DecodeLog = Arc::new(move |error, line| say(&link, error, line));
        return Box::new(crate::decode_ndk::NdkDecodeBackend::new(pool, log));
    }
    let _ = link;
    Box::new(StubDecodeBackend::new(pool))
}

/// ENC_MSG_POOL, the device's first message after the HELLO (one recvmsg of its header, the memfd with it): the pool
/// mapped, or None (no pool, or it could not be mapped: the line says why).
fn receive_pool(socket: &File, link: &Link) -> Option<PoolMem> {
    let mut header = EncMsg::default();
    let got = xh_core::fd::recv_with_fds(socket.as_fd(), header.as_bytes_mut(), 1, libc::MSG_WAITALL).ok()?;
    if got.length != std::mem::size_of::<EncMsg>() || header.type_ != ENC_MSG_POOL {
        say(link, true, "decoder: the device sent no pool: nothing is decoded");
        return None;
    }
    let size = u64::from(header.a) | u64::from(header.b) << 32;
    let fd = got.fds.into_iter().next()?;
    if size == 0 {
        return None;
    }
    // The offset rounded down to a page (the pool lies 2 MiB aligned in the memfd today; nothing here relies on it).
    let page = 4096u64;
    let offset = header.pts as u64;
    let skip = offset % page;
    // SAFETY: a new shared mapping of the device's memfd, kept for the process's life (the decoders write into it).
    let at = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            (size + skip) as usize,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            (offset - skip) as libc::off_t,
        )
    };
    if at == libc::MAP_FAILED {
        say(
            link,
            true,
            &format!("decoder: the pool ({} MiB) could not be mapped: {}", size >> 20, std::io::Error::last_os_error()),
        );
        return None;
    }
    // SAFETY: the mapping above, size bytes past skip, never unmapped.
    unsafe { PoolMem::new(at.cast::<u8>().add(skip as usize), size) }
}

/// A decoder session's results onto the link (from whatever thread the decoder runs on).
fn decode_sink(link: &Link, id: u32) -> DecodeSink {
    let link = Arc::clone(link);
    Arc::new(move |e| match e {
        DecEvent::Format(l) => {
            send(&link, EncMsg { a: l.width, b: l.height, c: l.stride, ..EncMsg::new(DEC_MSG_FORMAT, id) }, &[]);
        }
        DecEvent::Frame { index, bytes, pts, error, decode_us } => {
            let m = EncMsg { a: index, b: bytes, c: u32::from(error), pts, ..EncMsg::new(DEC_MSG_FRAME, id) };
            send(&link, m, &decode_us.to_le_bytes());
        }
        DecEvent::Eos => send(&link, EncMsg::new(ENC_MSG_EOS, id), &[]),
    })
}

/// The link's writing end, shared by the main thread, the watchdog and the warm-up.
type Link = Arc<Mutex<File>>;

fn send(link: &Link, m: EncMsg, payload: &[u8]) {
    let mut file = link.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = m.send(&mut *file, payload);
}

/// A line for xnuhost's log (`encoder: helper: ...`; an error line when `error`).
fn say(link: &Link, error: bool, line: &str) {
    send(link, EncMsg { a: u32::from(error), ..EncMsg::new(ENC_MSG_LOG, 0) }, line.as_bytes());
}

fn send_coded(link: &Link, id: u32, p: &Polled, coded: &[u8]) {
    let flags = u32::from(p.key) | u32::from(p.eos) << 1;
    send(link, EncMsg { pts: p.pts_us, a: flags, ..EncMsg::new(ENC_MSG_CODED, id) }, &coded[..p.bytes]);
}

/// After a feed: every coded unit the session has ready (an error, or nothing more, ends it; so does an eos).
fn after_feed(link: &Link, id: u32, enc: &mut dyn Session, coded: &mut [u8]) {
    while let Ok(p) = enc.poll(coded) {
        if p.bytes == 0 && !p.eos {
            break;
        }
        if p.bytes > 0 {
            send_coded(link, id, &p, coded);
        }
        if p.eos {
            break;
        }
    }
}

/// After a drain: every coded unit, then the EOS message once the drain's end came, or after about 2 s of nothing
/// (the codec's flush is waited for that long at most).
fn after_drain(link: &Link, id: u32, enc: &mut dyn Session, coded: &mut [u8]) {
    let mut waited = 0;
    while waited < 1000 {
        let p = enc.poll(coded).unwrap_or_default();
        if p.bytes > 0 {
            send_coded(link, id, &p, coded);
            continue;
        }
        if p.eos {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
        waited += 1;
    }
    send(link, EncMsg::new(ENC_MSG_EOS, id), &[]);
}

/// The open the main thread is in: its number (one per open), whether it runs, what it opens.
#[derive(Default)]
struct Watch {
    state: Mutex<(u64, bool, String)>,
    cond: Condvar,
}

impl Watch {
    fn begin(&self, what: String) -> Instant {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        *st = (st.0 + 1, true, what);
        self.cond.notify_all();
        Instant::now()
    }

    fn end(&self) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1 = false;
        self.cond.notify_all();
    }

    /// The watchdog: for each open, a line at each of [`WATCH_MS`] it is still in, naming the MediaCodec call. It
    /// sleeps on the condition variable while no open runs (no wake-up then).
    fn run(&self, link: &Link) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            st = self.cond.wait_while(st, |s| !s.1).unwrap_or_else(PoisonError::into_inner);
            let (open, start) = (st.0, Instant::now());
            for limit in WATCH_MS {
                let left = Duration::from_millis(limit).saturating_sub(start.elapsed());
                st = self
                    .cond
                    .wait_timeout_while(st, left, |s| s.1 && s.0 == open)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
                if !(st.1 && st.0 == open) {
                    break;
                }
                let line = format!("an open of {} has waited {limit} ms in {}", st.2, open_step());
                drop(st);
                say(link, true, &line);
                st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            }
            st = self.cond.wait_while(st, |s| s.1 && s.0 == open).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

fn open_step() -> &'static str {
    if stub_wanted() {
        "the stand-in's open"
    } else {
        crate::backend_ndk::open_step()
    }
}

fn describe(codec: u32, input: u32, w: u32, h: u32) -> String {
    let name = if codec == MEDIA_PIXFMT_HEVC { "HEVC" } else { "H.264" };
    let samples = if input == MEDIA_PIXFMT_P010 { "P010" } else { "NV12" };
    format!("{name} {w}x{h} ({samples})")
}

/// This process's binder threads (Android), as the line says them.
#[cfg(target_os = "android")]
pub fn binder_threads() -> String {
    let binder = match onecamera::android::binder_threads() {
        Ok(()) => "libbinder_ndk's pool started".to_string(),
        Err(e) => format!("no libbinder_ndk pool ({})", std::io::Error::from_raw_os_error(e)),
    };
    // libhidlbase's: configureRpcThreadpool(4, true), then one thread joins it (the driver asks for more as needed).
    let sym = |name: &std::ffi::CStr| {
        // SAFETY: dlsym of a C string in the global namespace.
        let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        (!p.is_null()).then_some(p as usize)
    };
    let hidl = match (
        sym(c"_ZN7android8hardware22configureRpcThreadpoolEmb"),
        sym(c"_ZN7android8hardware17joinRpcThreadpoolEv"),
    ) {
        (Some(configure), Some(join)) => {
            // SAFETY: android::hardware::configureRpcThreadpool(size_t, bool) and joinRpcThreadpool(), by their C++
            // signatures (plain arguments, no return value).
            let configure: extern "C" fn(usize, bool) = unsafe { std::mem::transmute(configure) };
            configure(4, true);
            let joined = std::thread::Builder::new().name("hwbinder".into()).spawn(move || {
                // SAFETY: as above; it returns only when the pool ends.
                let join: extern "C" fn() = unsafe { std::mem::transmute(join) };
                join();
            });
            if joined.is_ok() {
                "libhidlbase's pool started"
            } else {
                "no thread for libhidlbase's pool"
            }
        }
        _ => "libhidlbase not loaded (no hwbinder pool)",
    };
    format!("binder threads: {binder}; {hidl}")
}

/// The warm-up (Android, a thread of its own): the codec store's HEVC encoder and one HEVC encoder opened and closed,
/// so the first encode does not wait for MediaCodec's first load; one line with what it found and how long it took.
#[cfg(target_os = "android")]
fn warm_up(link: Link) {
    use crate::backend_ndk::caps;
    crate::backend_ndk::ignore_open_step_on_thread();
    let start = Instant::now();
    let found = match caps(MEDIA_PIXFMT_HEVC) {
        Ok(c) => {
            let (w, h) = c.coded_size(2, 2);
            format!(
                "{}{}: widths {}-{}, heights {}-{}, alignment {}x{}; smallest {w}x{h}, Main10 (P010) there {}",
                c.name.to_string_lossy(),
                if c.vendor { "" } else { " (not a vendor encoder)" },
                c.widths.0,
                c.widths.1,
                c.heights.0,
                c.heights.1,
                c.align.0,
                c.align.1,
                if c.main10(w, h) { "yes" } else { "no" }
            )
        }
        Err(why) => format!("no capabilities: {why}"),
    };
    let listed = start.elapsed().as_millis();
    let (w, h) = caps(MEDIA_PIXFMT_HEVC).map_or((96, 96), |c| c.coded_size(96, 96));
    let opened = crate::backend_ndk::NdkBackend.open(MEDIA_PIXFMT_HEVC, MEDIA_PIXFMT_NV12, w, h).is_some();
    say(
        &link,
        !opened,
        &format!(
            "warm-up: {found} (in {listed} ms); an HEVC {w}x{h} encoder {} in {} ms",
            if opened { "opened and closed" } else { "REFUSED" },
            start.elapsed().as_millis()
        ),
    );
}

/// The helper's main: its exit code.
pub fn main() -> i32 {
    let fd = std::env::args().nth(1).map_or(3, |a| a.trim().parse().unwrap_or(0));
    // encoder-10bit= and encoder-warmup= of xnuhost (docs/switches.md): its words after the fd
    let words: Vec<String> = std::env::args().skip(2).collect();
    crate::switches::from_helper_args(words.iter().map(String::as_str));
    // SAFETY: the socket xnuhost gave this process; it is ours for the process's life.
    let mut reader = unsafe { File::from_raw_fd(fd) };
    let Ok(writer) = reader.try_clone() else { return 1 };
    let link: Link = Arc::new(Mutex::new(writer));
    #[cfg(target_os = "android")]
    let threads = (!stub_wanted()).then(binder_threads);
    let backend = backend();
    let mut sessions: Vec<(u32, Box<dyn Session>)> = Vec::with_capacity(HELPER_SESSIONS);
    let mut coded = vec![0u8; CODED_CAP];
    let mut payload = Vec::new();
    let hello = EncMsg { a: ENC_LINK_VERSION, ..EncMsg::new(ENC_MSG_HELLO, 0) };
    if hello.send(&mut *link.lock().unwrap_or_else(PoisonError::into_inner), &[]).is_err() {
        return 1;
    }
    let decoders_backend = receive_pool(&reader, &link).map(|pool| decode_backend(pool, &link));
    let mut decoders: Vec<(u32, Box<dyn Decoder>)> = Vec::with_capacity(HELPER_SESSIONS);
    #[cfg(target_os = "android")]
    if let Some(threads) = threads {
        if crate::switches::warmup() {
            say(&link, false, &threads);
            let warm = Arc::clone(&link);
            let _ = std::thread::Builder::new().name("warm-up".into()).spawn(move || warm_up(warm));
        } else {
            // encoder-warmup=0 (docs/switches.md): MediaCodec loads at the first open, as before b3da193
            say(&link, false, &format!("{threads}; no warm-up (encoder-warmup=0)"));
        }
    }
    let watch = Arc::new(Watch::default());
    {
        let (watch, link) = (Arc::clone(&watch), Arc::clone(&link));
        let _ = std::thread::Builder::new().name("watchdog".into()).spawn(move || watch.run(&link));
    }
    while let Ok(m) = EncMsg::receive(&mut reader) {
        payload.resize(m.len as usize, 0);
        if reader.read_exact(&mut payload).is_err() {
            break;
        }
        let slot = sessions.iter().position(|(id, _)| *id == m.session);
        let decoder = decoders.iter().position(|(id, _)| *id == m.session);
        match (m.type_, slot) {
            (ENC_MSG_OPEN, _) => {
                // the input format: the payload's u32 (none: NV12)
                let input =
                    payload.get(..4).map_or(MEDIA_PIXFMT_NV12, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                let what = describe(m.a, input, m.b, m.c);
                let start = watch.begin(what.clone());
                let enc = (sessions.len() < HELPER_SESSIONS).then(|| backend.open(m.a, input, m.b, m.c)).flatten();
                watch.end();
                let took = start.elapsed().as_millis();
                if took >= u128::from(WATCH_MS[0]) {
                    let how = if enc.is_some() { "opened" } else { "refused" };
                    say(&link, false, &format!("an open of {what} came back after {took} ms ({how})"));
                }
                let (ok, coding) = enc.as_ref().map_or((false, 0), |e| (true, e.coding() as u32));
                if let Some(enc) = enc {
                    sessions.push((m.session, enc));
                }
                send(&link, EncMsg { a: u32::from(ok), b: coding, ..EncMsg::new(ENC_MSG_OPEN_REPLY, m.session) }, &[]);
            }
            (ENC_MSG_FEED, Some(i)) => {
                let enc = &mut sessions[i].1;
                let _ = enc.feed(&payload, m.pts, m.a & 1 != 0);
                after_feed(&link, m.session, enc.as_mut(), &mut coded); // whatever came out
            }
            (ENC_MSG_DRAIN, Some(i)) => {
                let enc = &mut sessions[i].1;
                let _ = enc.drain();
                after_drain(&link, m.session, enc.as_mut(), &mut coded);
            }
            (ENC_MSG_CLOSE, Some(i)) => drop(sessions.swap_remove(i)),
            // the decoder (2026-11-24)
            (DEC_MSG_OPEN, _) => {
                let what = format!("a decoder of {} {}x{}", crate::proto::codec_name(m.a), m.b, m.c);
                let start = watch.begin(what.clone());
                let dec = match (&decoders_backend, decoders.len() < HELPER_SESSIONS) {
                    (Some(b), true) => b.open_decoder(m.a, m.b, m.c, decode_sink(&link, m.session)),
                    _ => None,
                };
                watch.end();
                let took = start.elapsed().as_millis();
                if took >= u128::from(WATCH_MS[0]) {
                    let how = if dec.is_some() { "opened" } else { "refused" };
                    say(&link, false, &format!("an open of {what} came back after {took} ms ({how})"));
                }
                let ok = dec.is_some();
                if let Some(dec) = dec {
                    decoders.push((m.session, dec));
                }
                send(&link, EncMsg { a: u32::from(ok), ..EncMsg::new(ENC_MSG_OPEN_REPLY, m.session) }, &[]);
            }
            _ if decoder.is_some() => {
                let i = decoder.unwrap();
                let dec = &mut decoders[i].1;
                match m.type_ {
                    DEC_MSG_FEED => {
                        let _ = dec.feed(&payload, m.pts);
                    }
                    DEC_MSG_CAPTURE => dec.capture(m.a, m.pts as u64, u64::from(m.b)),
                    DEC_MSG_RESET => dec.reset_capture(),
                    ENC_MSG_DRAIN => {
                        let _ = dec.drain();
                    }
                    DEC_MSG_START => {
                        let _ = dec.restart();
                    }
                    ENC_MSG_CLOSE => drop(decoders.swap_remove(i)),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    0
}
