// SPDX-License-Identifier: MIT
//! display.sock's messages. One SOCK_SEQPACKET message, little-endian:
//!   hello  { "XHD1", width, height, stride, format } + the frame memfd (SCM_RIGHTS)
//!   flush  { "XHDF", x, y, w, h }
//! and from the helper, straight after connecting (xnuhost waits for it before it sizes the frame in a session):
//!   panel  { "XHDP", width, height, hz, flags }      flags bit 0 (PANEL_DIRECT): frames may come as "frame" below
//! Direct frames (runtime/xnuhost/src/display.rs): the helper copies the guest's scanout from guest RAM itself.
//!   memory { "XHDM", RAM bytes low, high, slots, slot bytes } + the RAM memfd and the table memfd (SCM_RIGHTS)
//!   frame  { "XHDS", slot, seq low, seq high, sent } described by that slot of the table ([`Slot`], then segments);
//!          sent: the low 32 bits of xnuhost's CLOCK_MONOTONIC in us when it sent this (0 from an xnuhost before
//!          2026-11-22)
//!   done   { "XHDA", seq low, seq high, flags, 0 }   (helper -> xnuhost) the frames up to seq are in the layer; flags
//!          bit 0 (DONE_FAILED): the last could not be copied, and xnuhost goes back to copying frames itself
//!   options { "XHDO", flags, 0, 0, 0 }  (xnuhost -> helper, after the hello; an older helper ignores it) bit 0
//!          (OPTION_SERIAL_POST, xnuhost's display-post=serial): the reader reads no further frame until the poster has
//!          posted the last (the one loop before 2026-11-24, docs/switches.md)
//!
//! The session's folder is writable by the guest, so nothing from the socket is trusted.
use super::direct::Direct;
use super::{Rect, MAX_SIDE};
use crate::pixels;
use crate::sys::{self, Errno, Map, Result};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

/// What a message is, by its magic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Hello,
    Flush,
    Panel,
    Memory,
    Frame,
    Done,
    Options,
}

impl Kind {
    pub const fn magic(self) -> [u8; 4] {
        match self {
            Kind::Hello => *b"XHD1",
            Kind::Flush => *b"XHDF",
            Kind::Panel => *b"XHDP",
            Kind::Memory => *b"XHDM",
            Kind::Frame => *b"XHDS",
            Kind::Done => *b"XHDA",
            Kind::Options => *b"XHDO",
        }
    }

    pub fn from_magic(magic: [u8; 4]) -> Option<Kind> {
        [Kind::Hello, Kind::Flush, Kind::Panel, Kind::Memory, Kind::Frame, Kind::Done, Kind::Options]
            .into_iter()
            .find(|k| k.magic() == magic)
    }
}

/// The panel report's flag: direct frames are welcome.
pub const PANEL_DIRECT: u32 = 1;
/// The done's flags: the last frame could not be copied (xnuhost copies the next ones itself).
pub const DONE_FAILED: u32 = 1;
/// The frame was what the layer showed: nothing was posted.
pub const DONE_UNCHANGED: u32 = 2;
/// The options' flags: each frame is posted before the next is read (display-post=serial).
pub const OPTION_SERIAL_POST: u32 = 1;

/// One message: a magic and four little-endian words (struct xhd_message, 20 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    pub magic: [u8; 4],
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub d: u32,
}

impl Message {
    pub const BYTES: usize = 20;

    pub const fn new(kind: Kind, a: u32, b: u32, c: u32, d: u32) -> Message {
        Message { magic: kind.magic(), a, b, c, d }
    }

    pub fn kind(&self) -> Option<Kind> {
        Kind::from_magic(self.magic)
    }

    pub fn is(&self, kind: Kind) -> bool {
        self.magic == kind.magic()
    }

    pub fn to_bytes(&self) -> [u8; Message::BYTES] {
        let mut out = [0u8; Message::BYTES];
        out[..4].copy_from_slice(&self.magic);
        for (i, v) in [self.a, self.b, self.c, self.d].into_iter().enumerate() {
            out[4 + i * 4..8 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// The message in a packet of exactly BYTES bytes; None for any other length.
    pub fn parse(packet: &[u8]) -> Option<Message> {
        let p: &[u8; Message::BYTES] = packet.try_into().ok()?;
        let word = |i: usize| u32::from_le_bytes(p[4 + i * 4..8 + i * 4].try_into().unwrap());
        Some(Message { magic: p[..4].try_into().unwrap(), a: word(0), b: word(1), c: word(2), d: word(3) })
    }

    /// The 64-bit number in two words, low first (a seq, the RAM's size).
    pub fn wide(low: u32, high: u32) -> u64 {
        low as u64 | (high as u64) << 32
    }

    /// Sent as one packet, with `fds` (SCM_RIGHTS) when there are any.
    pub fn send(&self, fd: BorrowedFd<'_>, fds: &[BorrowedFd<'_>], flags: libc::c_int) -> Result<()> {
        sys::send_packet(fd, &self.to_bytes(), fds, flags)
    }
}

/// A hello's frame: width x height pixels in `format`, rows of `stride` bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hello {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
}

/// The hello in `packet` when it describes a frame that fits in memfd_size bytes; EPROTO for a packet that is not a
/// hello, EINVAL for one whose frame is not usable.
pub fn check_hello(packet: &[u8], memfd_size: i64) -> Result<Hello> {
    let m = Message::parse(packet).filter(|m| m.is(Kind::Hello)).ok_or(Errno(libc::EPROTO))?;
    let (width, height, stride, format) = (m.a, m.b, m.c, m.d);
    let invalid = Err(Errno(libc::EINVAL));
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return invalid;
    }
    let bpp = pixels::bpp(format);
    if bpp == 0 {
        return invalid;
    }
    if stride < width * bpp || stride > MAX_SIDE * 8 || stride % bpp != 0 {
        return invalid;
    }
    if memfd_size < 0 || stride as u64 * height as u64 > memfd_size as u64 {
        return invalid;
    }
    Ok(Hello { width, height, stride, format })
}

/// xnuhost's frame: the hello and the memfd, mapped read-only.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    map: Map,
    _memfd: OwnedFd,
}

impl Frame {
    pub fn pixels(&self) -> &[u8] {
        self.map.bytes()
    }

    /// The memfd's map: stride * height bytes.
    pub fn bytes(&self) -> usize {
        self.map.len()
    }

    pub fn view(&self) -> FrameRef<'_> {
        FrameRef {
            width: self.width,
            height: self.height,
            stride: self.stride,
            format: self.format,
            pixels: self.pixels(),
        }
    }
}

/// A frame's pixels and layout (xnuhost's memfd, or a test's buffer).
#[derive(Clone, Copy)]
pub struct FrameRef<'a> {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    pub pixels: &'a [u8],
}

// ---- the helper's side -----------------------------------------------------------------------------------------

/// Connect to the socket at path, waiting up to wait_ms for it to appear; `stop()` true ends the wait. Refuses
/// anything that is not a socket (a link included).
pub fn connect(path: &Path, wait_ms: i32, stop: &dyn Fn() -> bool) -> Result<OwnedFd> {
    let end = sys::now_ms() + wait_ms.max(0) as i64;
    loop {
        let error = match std::fs::symlink_metadata(path) {
            Err(e) => Errno::from(e),
            Ok(st) => {
                use std::os::unix::fs::FileTypeExt;
                if !st.file_type().is_socket() {
                    return Err(Errno(libc::ENOTSOCK)); // a link or a file put there: not ours to follow
                }
                match sys::seqpacket_connect(path) {
                    Ok(fd) => return Ok(fd),
                    Err(e) => {
                        if ![libc::ECONNREFUSED, libc::ENOENT, libc::EAGAIN, libc::EINTR].contains(&e.0) {
                            return Err(e);
                        }
                        e
                    }
                }
            }
        };
        if stop() || sys::now_ms() >= end {
            return Err(error);
        }
        sys::nap_ms(100);
    }
}

/// Never stop a wait early (the tests, displayctl's stand-in).
pub fn no_stop() -> bool {
    false
}

/// Tell xnuhost the panel's mode (and PANEL_DIRECT in flags: direct frames are welcome).
pub fn send_panel(fd: BorrowedFd<'_>, width: u32, height: u32, hz: u32, flags: u32) -> Result<()> {
    Message::new(Kind::Panel, width, height, hz, flags).send(fd, &[], 0)
}

/// Every frame up to seq is in the layer (or, with DONE_FAILED, the last could not be copied).
pub fn send_done(fd: BorrowedFd<'_>, seq: u64, flags: u32) -> Result<()> {
    Message::new(Kind::Done, seq as u32, (seq >> 32) as u32, flags, 0).send(fd, &[], libc::MSG_DONTWAIT)
}

/// Receive the hello and map the frame.
pub fn recv_hello(fd: BorrowedFd<'_>, timeout_ms: i32) -> Result<Frame> {
    let (ready, _) = sys::poll(&[(fd, libc::POLLIN)], timeout_ms, true)?;
    if ready == 0 {
        return Err(Errno(libc::ETIMEDOUT));
    }
    let mut packet = [0u8; Message::BYTES];
    let got = loop {
        match sys::recv_packet(fd, &mut packet, 0) {
            Err(Errno(libc::EINTR)) => continue,
            r => break r?,
        }
    };
    // Every descriptor that came is ours to close (dropped); the first is the frame.
    let mut fds = got.fds.into_iter();
    let memfd = fds.next();
    if got.length == 0 {
        return Err(Errno(libc::ECONNRESET));
    }
    let memfd = match memfd {
        Some(fd) if !got.truncated => fd,
        _ => return Err(Errno(libc::EPROTO)),
    };
    let (size, regular) = sys::file_size(memfd.as_fd())?;
    if !regular {
        return Err(Errno(libc::EINVAL));
    }
    let hello = check_hello(&packet[..got.length.min(Message::BYTES)], size)?;
    let map = Map::new(memfd.as_fd(), hello.stride as usize * hello.height as usize, false)?;
    Ok(Frame {
        width: hello.width,
        height: hello.height,
        stride: hello.stride,
        format: hello.format,
        map,
        _memfd: memfd,
    })
}

/// Read every pending flush without blocking, adding each (clipped to width x height) to `damage`. false: the socket
/// closed (or failed).
pub fn drain(fd: BorrowedFd<'_>, width: u32, height: u32, damage: &mut Rect) -> bool {
    drain_direct(fd, width, height, damage, None)
}

/// The same, and with `direct` (None: those messages are then ignored) the "memory" and "frame" messages too.
pub fn drain_direct(
    fd: BorrowedFd<'_>,
    width: u32,
    height: u32,
    damage: &mut Rect,
    mut direct: Option<&mut Direct>,
) -> bool {
    // A bound on one round: a guest that floods flushes must not keep the frame from being posted.
    for _ in 0..4096 {
        let mut packet = [0u8; Message::BYTES];
        let got = match sys::recv_packet(fd, &mut packet, libc::MSG_DONTWAIT) {
            Ok(got) => got,
            Err(Errno(libc::EINTR)) => continue,
            Err(Errno(e)) if e == libc::EAGAIN || e == libc::EWOULDBLOCK => return true,
            Err(e) => {
                eprintln!("oneios-display: reading xnuhost's socket failed: {e}");
                return false;
            }
        };
        let m = if got.length == Message::BYTES { Message::parse(&packet) } else { None };
        if let (Some(m), Some(d)) = (m.filter(|m| m.is(Kind::Memory)), direct.as_deref_mut()) {
            d.take_memory(&m, got.fds); // the kept ones it owns; the rest are closed here
        }
        if got.length == 0 {
            eprintln!("oneios-display: xnuhost closed the socket");
            return false;
        }
        let Some(m) = m else { continue };
        match m.kind() {
            Some(Kind::Frame) => {
                // The newest frame wins; the answer for it covers the ones before it.
                if let Some(d) = direct.as_deref_mut() {
                    d.offer(m.a, Message::wide(m.b, m.c), m.d);
                }
            }
            Some(Kind::Options) => {
                if let Some(d) = direct.as_deref_mut() {
                    d.options = m.a;
                }
            }
            Some(Kind::Flush) => {
                let mut r = Rect::new(m.a, m.b, m.c, m.d);
                if r.clip(width, height) {
                    damage.union(&r);
                }
            }
            _ => {} // not a flush: ignored
        }
    }
    true
}

// ---- the serving side (xnuhost's, displayctl's and the tests') --------------------------------------------------

/// A display.sock listening at path.
pub fn listen(path: &Path) -> Result<OwnedFd> {
    sys::seqpacket_listen(path)
}

pub fn send_hello(
    fd: BorrowedFd<'_>,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    memfd: BorrowedFd<'_>,
) -> Result<()> {
    Message::new(Kind::Hello, width, height, stride, format).send(fd, &[memfd], 0)
}

pub fn send_flush(fd: BorrowedFd<'_>, x: u32, y: u32, w: u32, h: u32) -> Result<()> {
    Message::new(Kind::Flush, x, y, w, h).send(fd, &[], 0)
}

/// A memfd of `bytes` for a frame (or guest RAM, a table).
pub fn memfd(name: &str, bytes: usize) -> Result<OwnedFd> {
    sys::memfd(name, bytes)
}
