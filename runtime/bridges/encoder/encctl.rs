// SPDX-License-Identifier: MIT
//! encctl: the phone test of the video encoder bridge, as camctl/bridgectl are (encctl.c). It runs the whole host
//! path -- the "droidvm,virtio-media" device ([`crate::media_device`]) on xh-core's real transport, with the real
//! MediaCodec backend ([`crate::backend_ndk`]; the stand-in on a host) -- with a guest made of plain memory in this
//! process ([`Guest`], which the host test drives too), encodes an NV12 test pattern through the phone's hardware
//! encoder as root with One UI up, and checks the bitstream. No xnuhost, no Apple code.
//!
//! ```text
//!   encctl hevc|h264|hevc10 [WIDTH HEIGHT [FILE]] default hevc 256x256; FILE: the coded stream, as it came (Annex B)
//!   encctl caps                                  the encoders' sizes and Main10, as Android 16's codec store says
//!   encctl decode FILE [WIDTH HEIGHT [OUT]]      the decoder (2026-11-24): an IVF (VP9, AV1) or Annex-B (H.264, HEVC)
//!                                                file through the device and MediaCodec ([`crate::encctl_decode`])
//! ```
//!
//! `hevc10` (2026-11-23) sends a P010 picture (10-bit, as the guest sends iOS's x420 and xf20 surfaces): the line
//! `encctl: encoder: session 1: HEVC WxH on mediacodec: ...` says whether it was coded as Main10 or as 8-bit, and the
//! last line names it again. `caps` prints, for the HEVC and H.264 encoders the device opens, their sizes
//! (lower-upper), alignment, the size an 84x84 tile is coded at, and for 64 to 512 square whether the size and (HEVC)
//! Main10 with P010 are taken. On Android encctl starts its binder threads first, as the helper does.
//!
//! A pass prints each access unit's size and the types of its NAL units (HEVC: 32 VPS, 33 SPS, 34 PPS, 19 or 20 the
//! IDR slice; H.264: 7 SPS, 8 PPS, 5 the IDR slice), and "ok". `error:` lines say which step failed. FILE is for a
//! decoder off the phone (`ffmpeg -i FILE -frames:v 1 out.png`): the picture is a grey gradient with diagonal bands.
use crate::backend::Backend;
use crate::decoder::{DecodeBackend, PoolMem};
use crate::media_device::{self, MediaVirtio, Stats};
use crate::pod::Pod;
use crate::proto::*;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};
use xh_core::bytes::le;
use xh_core::mmio::MmioDevice;
use xh_core::virtio::testing::{self, Driver, GuestRam, RING_BYTES};

const QN: u16 = 32;
/// The event buffers the guest posts at the start (once, as the C).
pub const EVENT_BUFS: usize = 8;
const POOL_BYTES: usize = 32 << 20;
// The command area: the command at 0, the ioctl's payload out at 0x100, the response at 0x400, the payload back at
// 0x500.
const CMD: usize = 0;
const OUT: usize = 0x100;
const RESP: usize = 0x400;
const IN: usize = 0x500;

/// Why an MMAP gave no buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmapError {
    /// The device said no (an errno).
    Status(u32),
    /// The place it gave is not inside the pool: (at, size).
    NotInPool(u64, u64),
}

/// C's `%#llx`: 0 is "0".
fn c_hex(v: u64) -> String {
    if v == 0 {
        "0".into()
    } else {
        format!("{v:#x}")
    }
}

impl std::fmt::Display for MmapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MmapError::Status(s) => write!(f, "MMAP: status {s}"),
            MmapError::NotInPool(at, size) => write!(f, "MMAP: {}+{} is not in the pool", c_hex(*at), c_hex(*size)),
        }
    }
}

/// The guest's media pool: a memfd mapped shared, as xnuhost's RAM is (the helper maps the same file to write the
/// decoder's pictures into it). Its guest physical address is its host address, as [`GuestRam`]'s.
struct PoolRam {
    fd: OwnedFd,
    ptr: *mut u8,
    len: usize,
}

// SAFETY: a mapping owned by the PoolRam; access goes through raw copies, as GuestRam's.
unsafe impl Send for PoolRam {}

impl PoolRam {
    fn new(len: usize) -> Option<PoolRam> {
        let fd = xh_core::fd::memfd(c"encctl-pool", libc::MFD_CLOEXEC).ok()?;
        // SAFETY: our new memfd, sized, then mapped shared for the PoolRam's life.
        unsafe {
            if libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) != 0 {
                return None;
            }
            let p = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            );
            (p != libc::MAP_FAILED).then(|| PoolRam { fd, ptr: p.cast(), len })
        }
    }

    fn pa(&self, offset: usize) -> u64 {
        self.ptr as u64 + offset as u64
    }

    fn read(&self, offset: usize, len: usize) -> Vec<u8> {
        assert!(offset + len <= self.len, "a read past the pool");
        // SAFETY: inside the mapping (checked).
        unsafe { std::slice::from_raw_parts(self.ptr.add(offset), len) }.to_vec()
    }

    fn write(&self, offset: usize, data: &[u8]) {
        assert!(offset + data.len() <= self.len, "a write past the pool");
        // SAFETY: inside the mapping (checked).
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.ptr.add(offset), data.len()) };
    }
}

impl Drop for PoolRam {
    fn drop(&mut self) {
        // SAFETY: our mapping, unmapped once.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

/// A guest of plain memory over xh-core's transport, doing what DroidVMVirtioMedia.c does.
pub struct Guest {
    pub dev: Arc<MediaVirtio>,
    driver: Driver,
    rings: GuestRam,
    pool: PoolRam,
    io: GuestRam,
    events: GuestRam,
}

impl Guest {
    /// The device at `base` over `backend`, with a 32 MiB pool, brought up, its event buffers posted.
    pub fn up(backend: Arc<dyn Backend>, base: u64) -> Result<Guest, &'static str> {
        Guest::up_with(backend, None, base)
    }

    /// The same, decoding too with `decoder`.
    pub fn up_with(
        backend: Arc<dyn Backend>,
        decoder: Option<Arc<dyn DecodeBackend>>,
        base: u64,
    ) -> Result<Guest, &'static str> {
        Guest::up_in(backend, |_, _| decoder, base)
    }

    /// The device decoding with the decoder `make` builds over the guest's pool: its host mapping, and its memfd (for
    /// the helper, [`crate::backend_helper::HelperBackend::set_pool`]: the pool is the whole file, at offset 0).
    pub fn up_decoding(
        backend: Arc<dyn Backend>,
        make: impl FnOnce(PoolMem, BorrowedFd<'_>) -> Arc<dyn DecodeBackend>,
        base: u64,
    ) -> Result<Guest, &'static str> {
        Guest::up_in(backend, |pool, fd| Some(make(pool, fd)), base)
    }

    fn up_in(
        backend: Arc<dyn Backend>,
        decoder: impl FnOnce(PoolMem, BorrowedFd<'_>) -> Option<Arc<dyn DecodeBackend>>,
        base: u64,
    ) -> Result<Guest, &'static str> {
        testing::install();
        let pool = PoolRam::new(POOL_BYTES).ok_or("no memfd for the pool")?;
        // SAFETY: the pool is this guest's memory, alive as long as the Guest (which holds the device's last Arc).
        let mem = unsafe { PoolMem::new(pool.ptr, POOL_BYTES as u64) }.ok_or("no pool")?;
        let decoder = decoder(mem, pool.fd.as_fd());
        // SAFETY: as above.
        let dev =
            unsafe { media_device::create(base, 40, Some(backend), decoder, pool.ptr, pool.pa(0), POOL_BYTES as u64) }
                .ok_or("the encoder device could not be made")?;
        let rings = GuestRam::new(2 * RING_BYTES as usize);
        let driver =
            Driver::up(dev.clone() as Arc<dyn MmioDevice>, 0, 2, QN, rings.pa(0)).ok_or("the guest's driver")?;
        let mut g =
            Guest { dev, driver, rings, pool, io: GuestRam::new(0x1000), events: GuestRam::new(EVENT_BUFS * 640) };
        for i in 0..EVENT_BUFS {
            let pa = g.events.pa(i * MEDIA_EVENT_BYTES as usize);
            g.driver.post(MEDIA_QUEUE_EVENT, &[(pa, MEDIA_EVENT_BYTES, true)]);
        }
        Ok(g)
    }

    /// A command: `cmd` (read), `out` (read), a response of `resp_len` (written), a payload back of `back_len`
    /// (written). The used length (None: the device returned nothing), the response, the payload back.
    pub fn command(
        &mut self,
        cmd: &[u8],
        out: Option<&[u8]>,
        resp_len: usize,
        back_len: Option<usize>,
    ) -> (Option<u32>, Vec<u8>, Vec<u8>) {
        let io = &self.io;
        io.write(CMD, cmd);
        let mut parts = vec![(io.pa(CMD), cmd.len() as u32, false)];
        if let Some(out) = out {
            io.write(OUT, out);
            parts.push((io.pa(OUT), out.len() as u32, false));
        }
        io.fill(RESP, 0, resp_len);
        parts.push((io.pa(RESP), resp_len as u32, true));
        if let Some(n) = back_len {
            io.fill(IN, 0, n);
            parts.push((io.pa(IN), n as u32, true));
        }
        self.driver.post(MEDIA_QUEUE_COMMAND, &parts);
        let used = self.driver.reap(MEDIA_QUEUE_COMMAND).map(|(_, len)| len);
        (used, io.read(RESP, resp_len), io.read(IN, back_len.unwrap_or(0)))
    }

    /// A command's status, the used length.
    pub fn status(resp: &[u8]) -> u32 {
        u32::from_le_bytes(resp[..4].try_into().unwrap())
    }

    pub fn open(&mut self) -> Option<u32> {
        let (_, resp, _) = self.command([MEDIA_CMD_OPEN, 0].as_bytes(), None, 16, None);
        (Guest::status(&resp) == 0).then(|| le::u32(&resp, 8))
    }

    /// An ioctl with `payload` out and back (in place); its status.
    pub fn ioctl(&mut self, session: u32, nr: u32, payload: &mut [u8]) -> u32 {
        let (_, resp, back) =
            self.command([MEDIA_CMD_IOCTL, 0, session, nr].as_bytes(), Some(payload), 8, Some(payload.len()));
        let status = Guest::status(&resp);
        if status == 0 {
            payload.copy_from_slice(&back);
        }
        status
    }

    /// STREAMON (out only, as DroidVMVTHost does it): its status.
    pub fn stream_on(&mut self, session: u32, kind: u32) -> u32 {
        let (_, resp, _) = self.command(
            [MEDIA_CMD_IOCTL, 0, session, MEDIA_VIDIOC_STREAMON].as_bytes(),
            Some(&kind.to_le_bytes()),
            8,
            None,
        );
        Guest::status(&resp)
    }

    /// MMAP as the guest's transport does it (DroidVMVirtioMedia.c droidvm_media_mmap): the answer is the buffer's
    /// place in the pool and its size, refused when it is not inside the pool; the address is the pool's base plus
    /// it.
    pub fn mmap(&mut self, session: u32, handle: u64) -> Result<(u64, u64), MmapError> {
        let (_, resp, _) = self.command([MEDIA_CMD_MMAP, 0, session, 1, handle as u32].as_bytes(), None, 24, None);
        if Guest::status(&resp) != 0 {
            return Err(MmapError::Status(Guest::status(&resp)));
        }
        let at = le::u64(&resp, 8);
        let size = le::u64(&resp, 16);
        let pool = POOL_BYTES as u64;
        if at > pool || size > pool - at {
            return Err(MmapError::NotInPool(at, size));
        }
        Ok((self.pool.pa(at as usize), size))
    }

    /// CLOSE as the guest's transport sends it (droidvm_media_close): the command alone, no buffer for an answer.
    /// The used length.
    pub fn close(&mut self, session: u32) -> Option<u32> {
        self.io.write(CMD, [MEDIA_CMD_CLOSE, 0, session, 0].as_bytes());
        self.driver.post(MEDIA_QUEUE_COMMAND, &[(self.io.pa(CMD), 16, false)]);
        self.driver.reap(MEDIA_QUEUE_COMMAND).map(|(_, len)| len)
    }

    /// Guest memory at `pa` (a pool buffer).
    pub fn read(&self, pa: u64, len: usize) -> Vec<u8> {
        self.pool.read((pa - self.pool.pa(0)) as usize, len)
    }

    pub fn write(&self, pa: u64, data: &[u8]) {
        self.pool.write((pa - self.pool.pa(0)) as usize, data)
    }

    /// The next event (`timeout` at most): kind, session, the event buffer (MEDIA_EVENT_BYTES).
    pub fn wait_event(&mut self, timeout: Duration) -> Option<(u32, u32, Vec<u8>)> {
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            if let Some((id, len)) = self.driver.reap(MEDIA_QUEUE_EVENT) {
                if len >= 8 {
                    let pa = self.driver.desc_addr(MEDIA_QUEUE_EVENT, id % u32::from(QN));
                    let ev = self.events.read((pa - self.events.pa(0)) as usize, MEDIA_EVENT_BYTES as usize);
                    // the buffer back to the device, as the guest's transport keeps its event queue stocked
                    self.driver.post(MEDIA_QUEUE_EVENT, &[(pa, MEDIA_EVENT_BYTES, true)]);
                    let word = |i: usize| le::u32(&ev, i);
                    return Some((word(0), word(4), ev));
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        None
    }

    pub fn stats(&self) -> Stats {
        media_device::stats(&self.dev)
    }

    /// Sets or clears VIRTQ_AVAIL_F_NO_INTERRUPT on `queue`'s avail ring, as the staged DroidVMVirtioMedia.c does (edit 7,
    /// 2026-11-25: the command queue always, the event queue unless it waits on the interrupt).
    pub fn set_no_interrupt(&self, queue: u32, on: bool) {
        self.rings.set_u16(queue as usize * RING_BYTES as usize + 0x1000, u16::from(on));
    }
}

/// A buffer of `kind` and `index` asked of the device (QUERYBUF).
pub fn querybuf(g: &mut Guest, session: u32, kind: u32, index: u32) -> MediaV4l2Buffer {
    let mut b = MediaV4l2Buffer { type_: kind, index, ..Default::default() };
    g.ioctl(session, MEDIA_VIDIOC_QUERYBUF, b.as_bytes_mut());
    b
}

/// The NAL unit types of an access unit, by its start codes (00 00 01, with or without a zero before it).
pub fn nal_types(au: &[u8], hevc: bool) -> Vec<u32> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 3 < au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            let h = au[i + 3];
            types.push(u32::from(if hevc { (h >> 1) & 0x3f } else { h & 0x1f }));
            i += 3;
        }
        i += 1;
    }
    types
}

fn backend() -> Arc<dyn Backend> {
    #[cfg(target_os = "android")]
    return Arc::new(crate::backend_ndk::NdkBackend);
    #[cfg(not(target_os = "android"))]
    Arc::new(crate::backend_stub::StubBackend) // no MediaCodec on a host: the stand-in
}

/// An MMAP that gave a place outside the pool is said (encctl.c's mmap_buf); one the device refused is not.
fn report_mmap<T>(r: Result<T, MmapError>) {
    if let Err(e @ MmapError::NotInPool(..)) = r {
        eprintln!("error: {e}");
    }
}

fn fail(text: &str) -> i32 {
    eprintln!("error: {text}");
    1
}

/// `encctl caps` (2026-11-23): what Android 16's codec store says of the HEVC and H.264 encoders the device opens. One
/// line an encoder, then one a square size; 1 when there is no store or no encoder.
fn caps() -> i32 {
    #[cfg(target_os = "android")]
    {
        let mut found = 0;
        for (codec, name) in [(MEDIA_PIXFMT_HEVC, "hevc"), (MEDIA_PIXFMT_H264, "h264")] {
            let c = match crate::backend_ndk::caps(codec) {
                Ok(c) => c,
                Err(why) => {
                    println!("encctl: caps {name}: {why}");
                    continue;
                }
            };
            found += 1;
            let (tw, th) = c.coded_size(84, 84);
            println!(
                "encctl: caps {name}: {}{}, widths {}-{}, heights {}-{}, alignment {}x{}; an 84x84 tile is coded {tw}x{th}",
                c.name.to_string_lossy(),
                if c.vendor { "" } else { " (not a vendor encoder)" },
                c.widths.0,
                c.widths.1,
                c.heights.0,
                c.heights.1,
                c.align.0,
                c.align.1
            );
            for side in [64, 84, 96, 128, 160, 192, 256, 512] {
                let main10 = if codec == MEDIA_PIXFMT_HEVC {
                    format!(", Main10 (P010) {}", if c.main10(side, side) { "yes" } else { "no" })
                } else {
                    String::new()
                };
                println!(
                    "encctl: caps {name} {side}x{side}: 8-bit {}{main10}",
                    if c.size(side, side) { "yes" } else { "no" }
                );
            }
        }
        if found == 0 {
            return 1;
        }
        0
    }
    #[cfg(not(target_os = "android"))]
    {
        println!("encctl: caps: no MediaCodec here (a host: the stand-in codes every size, 8-bit only)");
        1
    }
}

/// encctl's main: its exit code.
pub fn main() -> i32 {
    // The bridges' log goes to stderr here, as encctl.c's br_log.
    xh_core::log::set_sink(xh_core::log::Sink::Custom(Arc::new(|level, line: &str| {
        use xh_core::log::Level;
        let tag = match level {
            Level::Error => "error:",
            Level::Info => "encctl:",
            Level::Debug => "debug:",
        };
        eprintln!("{tag} {line}");
    })));
    let args: Vec<String> = std::env::args().collect();
    let codec_name = args.get(1).map_or("hevc", String::as_str);
    if codec_name == "caps" {
        return caps();
    }
    if codec_name == "decode" {
        // the decoder (2026-11-24); on Android its binder threads first, as the helper's
        #[cfg(target_os = "android")]
        eprintln!("encctl: {}", crate::helper::binder_threads());
        return crate::encctl_decode::decode(&args[2..]);
    }
    #[cfg(target_os = "android")]
    eprintln!("encctl: {}", crate::helper::binder_threads()); // as the helper: Codec2 calls its client back over binder
                                                              // hevc10: a P010 picture (2026-11-23)
    let ten = codec_name == "hevc10";
    let input = if ten { MEDIA_PIXFMT_P010 } else { MEDIA_PIXFMT_NV12 };
    let num = |i: usize| onecamera::util::atoi(&args[i]) as u32;
    let (w, h) = if args.len() > 3 { (num(2), num(3)) } else { (256, 256) };
    let codec = if codec_name == "h264" { MEDIA_PIXFMT_H264 } else { MEDIA_PIXFMT_HEVC };
    let hevc = codec == MEDIA_PIXFMT_HEVC;
    let mut stream = match args.get(4) {
        Some(path) => match std::fs::File::create(path) {
            Ok(f) => Some(f),
            Err(_) => return fail(&format!("cannot write {path}")),
        },
        None => None,
    };
    let mut g = match Guest::up(backend(), 0x0a00_0000) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let Some(session) = g.open() else { return fail("OPEN") };
    let mut f = MediaV4l2Format {
        type_: MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE,
        width: w,
        height: h,
        pixelformat: codec,
        num_planes: 1,
        ..Default::default()
    };
    if g.ioctl(session, MEDIA_VIDIOC_S_FMT, f.as_bytes_mut()) != 0 {
        return fail("S_FMT capture");
    }
    let mut f = MediaV4l2Format {
        type_: MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
        width: w,
        height: h,
        pixelformat: input,
        num_planes: 1,
        ..Default::default()
    };
    if g.ioctl(session, MEDIA_VIDIOC_S_FMT, f.as_bytes_mut()) != 0 {
        return fail("S_FMT output");
    }
    let mut ro = [2, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, MEDIA_MEMORY_MMAP, 0, 0];
    g.ioctl(session, MEDIA_VIDIOC_REQBUFS, ro.as_bytes_mut());
    let qb = querybuf(&mut g, session, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, 0);
    let out_buf = g.mmap(session, qb.plane.m);
    let mut rc = [4, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, MEDIA_MEMORY_MMAP, 0, 0];
    g.ioctl(session, MEDIA_VIDIOC_REQBUFS, rc.as_bytes_mut());
    for i in 0..4 {
        let mut cb = querybuf(&mut g, session, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, i);
        report_mmap(g.mmap(session, cb.plane.m));
        g.ioctl(session, MEDIA_VIDIOC_QBUF, cb.as_bytes_mut());
    }
    g.stream_on(session, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE);
    g.stream_on(session, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE);
    let out_buf = match out_buf {
        Ok((pa, _)) => pa,
        Err(e) => {
            report_mmap(Err::<(), _>(e));
            return fail("no output buffer");
        }
    };
    let frame = picture_bytes(input, w, h) as usize;
    let mut picture = vec![128u8; (w * h * 3 / 2) as usize]; // grey chroma: a visible gradient
    for y in 0..h {
        for x in 0..w {
            picture[(y * w + x) as usize] = ((x ^ y) + y) as u8;
        }
    }
    if ten {
        // the same picture in P010: each 8-bit value v as the 10-bit 4v, in the high 10 bits (v << 8)
        picture = picture.iter().flat_map(|&v| (u16::from(v) << 8).to_le_bytes()).collect();
    }
    g.write(out_buf, &picture);
    let mut qo = MediaV4l2Buffer { type_: MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, index: 0, sec: 1, ..Default::default() };
    qo.plane.bytesused = frame as u32;
    g.ioctl(session, MEDIA_VIDIOC_QBUF, qo.as_bytes_mut());
    let mut enc = [0u32; 10];
    enc[0] = MEDIA_ENC_CMD_STOP;
    g.ioctl(session, MEDIA_VIDIOC_ENCODER_CMD, enc.as_bytes_mut());

    let (mut total, mut aus, mut key, mut eos) = (0u32, 0u32, false, false);
    for _ in 0..16 {
        if eos {
            break;
        }
        let Some((kind, _, ev)) = g.wait_event(Duration::from_secs(5)) else { break };
        if kind == MEDIA_EVT_EVENT {
            eos = true;
            continue;
        }
        if kind != MEDIA_EVT_DQBUF {
            continue;
        }
        let b = MediaV4l2Buffer::read_from(&ev[8..]);
        if b.type_ != MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE || b.plane.bytesused == 0 {
            continue;
        }
        total += b.plane.bytesused;
        aus += 1;
        key |= b.flags & MEDIA_BUF_FLAG_KEYFRAME != 0;
        let cb = querybuf(&mut g, session, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, b.index);
        match g.mmap(session, cb.plane.m) {
            Ok((pa, _)) if b.plane.bytesused >= 6 => {
                let au = g.read(pa, b.plane.bytesused as usize);
                let types: Vec<String> = nal_types(&au, hevc).iter().map(u32::to_string).collect();
                let mut list = String::new();
                for t in &types {
                    if list.len() + 8 >= 160 {
                        break; // C's char types[160]
                    }
                    list += &format!("{}{t}", if list.is_empty() { "" } else { ", " });
                }
                println!(
                    "encctl: AU {} bytes of a {}-byte buffer, NAL types {list}{}",
                    b.plane.bytesused,
                    b.plane.length,
                    if b.flags & MEDIA_BUF_FLAG_KEYFRAME != 0 { " (keyframe)" } else { "" }
                );
                if let Some(s) = stream.as_mut() {
                    if s.write_all(&au).is_err() {
                        return fail(&format!("cannot write {}", args[4]));
                    }
                }
            }
            Ok(_) => {}
            Err(e) => report_mmap(Err::<(), _>(e)),
        }
        eos |= b.flags & MEDIA_BUF_FLAG_LAST != 0;
    }
    drop(stream); // written as it came (a File has no buffer to flush)
                  // CLOSE as the guest's transport sends it: the command alone, no buffer for an answer.
    g.close(session);
    let st = g.stats();
    if aus != 0 && key && total != 0 && st.errors == 0 {
        let paths = ["8-bit", "Main10", "10-bit as 8-bit"];
        let how: Vec<&str> = st.coded.iter().zip(paths).filter(|(n, _)| **n != 0).map(|(_, p)| p).collect();
        println!(
            "encctl: {codec_name} {w}x{h}: {aus} access unit(s), {total} bytes, keyframe present, coded {}; ok",
            how.join(", ")
        );
        return 0;
    }
    fail(&format!(
        "{codec_name} {w}x{h}: {aus} access units, {total} bytes, key {}, {} errors",
        u8::from(key),
        st.errors
    ))
}
