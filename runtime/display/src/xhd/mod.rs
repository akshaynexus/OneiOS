// SPDX-License-Identifier: MIT
//! Port of xhd.c and xhd.h: the display helper's portable core. xnuhost's display.sock protocol
//! (runtime/xnuhost/src/display.rs, docs/booter.md 2.8), the letterbox fit, and the row copy. No Android here, so the
//! host tests run it; xnuhost (its display device and test_display_mode) uses the same code for its end of the socket.
//!
//! - [`protocol`]: the messages, the helper's side (connect, panel, hello, drain, done) and xnuhost's (listen, hello,
//!   flush, memfd).
//! - [`direct`]: direct frames, the helper copying the guest's scanout from guest RAM itself.
//! - [`snapshot`]: the helper's own copy of the guest's frame and its tiles' hashes.
//! - [`buffers`]: what each of the layer's buffers lacks.
use crate::pixels;
use crate::sys::{Errno, Result};
use std::path::Path;

pub mod buffers;
pub mod direct;
pub mod present;
pub mod protocol;
pub mod run;
pub mod snapshot;

pub use buffers::{Buffers, BUFFERS};
pub use direct::{Direct, PendingFrame, Seg, Slot};
pub use protocol::*;
pub use snapshot::{hash_tile, Snapshot, TILES};

/// The hello's format: what the frame's pixels hold (xnuhost's pixels.h says it in full).
/// Bytes B,G,R,X, 8-bit sRGB: iOS's 'BGRA' framebuffer.
pub const FORMAT_BGRX: u32 = pixels::BGRX8;
/// R,G,B,A half floats, linear extended sRGB: iOS's 'RGhA'.
pub const FORMAT_RGBA16F: u32 = pixels::RGBA16F;
pub const MAX_SIDE: u32 = 16384;

/// The layer's buffer layouts the copy writes (Android's AHARDWAREBUFFER_FORMAT_* values).
pub const BUFFER_RGBX_8888: i32 = 2; // bytes R,G,B,X
pub const BUFFER_BGRA_8888: i32 = 5; // bytes B,G,R,A
pub const BUFFER_RGBA_FP16: i32 = 22; // R,G,B,A half floats

/// Android's dataspaces (ADataSpace) the helper tags the buffers with.
pub const DATASPACE_UNKNOWN: i32 = 0;
pub const DATASPACE_SRGB: i32 = 142671872; // STANDARD_BT709 | TRANSFER_SRGB | RANGE_FULL
pub const DATASPACE_DISPLAY_P3: i32 = 143261696; // STANDARD_DCI_P3 | TRANSFER_SRGB | RANGE_FULL
pub const DATASPACE_SCRGB: i32 = 411107328; // STANDARD_BT709 | TRANSFER_SRGB | RANGE_EXTENDED
pub const DATASPACE_SCRGB_LINEAR: i32 = 406913024; // STANDARD_BT709 | TRANSFER_LINEAR | RANGE_EXTENDED
/// --dataspace auto: the frame format's own.
pub const DATASPACE_AUTO: i32 = -1;

/// The buffer and dataspace for a frame format: B,G,R,A with sRGB for 8-bit frames (the colours iOS composited them
/// in), RGBA_FP16 with SCRGB_LINEAR for half-float ones (the same numbers, the same meaning). None for an unknown one.
pub fn buffer_for(format: u32) -> Option<(i32, i32)> {
    match format {
        FORMAT_BGRX => Some((BUFFER_BGRA_8888, DATASPACE_SRGB)),
        FORMAT_RGBA16F => Some((BUFFER_RGBA_FP16, DATASPACE_SCRGB_LINEAR)),
        _ => None,
    }
}

const DATASPACES: [(&str, i32); 5] = [
    ("unknown", DATASPACE_UNKNOWN),
    ("srgb", DATASPACE_SRGB),
    ("p3", DATASPACE_DISPLAY_P3),
    ("scrgb", DATASPACE_SCRGB),
    ("scrgb-linear", DATASPACE_SCRGB_LINEAR),
];

pub fn dataspace_name(dataspace: i32) -> &'static str {
    DATASPACES.iter().find(|(_, v)| *v == dataspace).map_or("other", |(n, _)| n)
}

/// --dataspace's names: auto (DATASPACE_AUTO), unknown, srgb, p3, scrgb, scrgb-linear. None for a name it does not
/// know.
pub fn dataspace_parse(name: &str) -> Option<i32> {
    if name == "auto" {
        return Some(DATASPACE_AUTO);
    }
    DATASPACES.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

/// A rectangle of the frame; w == 0: empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const EMPTY: Rect = Rect { x: 0, y: 0, w: 0, h: 0 };

    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    /// The whole of a width x height frame.
    pub const fn whole(width: u32, height: u32) -> Rect {
        Rect { x: 0, y: 0, w: width, h: height }
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Clipped to width x height. false (and the rectangle emptied) when nothing is left.
    pub fn clip(&mut self, width: u32, height: u32) -> bool {
        if self.is_empty() || self.x >= width || self.y >= height {
            *self = Rect::EMPTY;
            return false;
        }
        self.w = self.w.min(width - self.x);
        self.h = self.h.min(height - self.y);
        true
    }

    /// The bounding rectangle of both (an empty one adds nothing).
    pub fn union(&mut self, r: &Rect) {
        if r.is_empty() {
            return;
        }
        if self.is_empty() {
            *self = *r;
            return;
        }
        let x0 = self.x.min(r.x);
        let y0 = self.y.min(r.y);
        let x1 = (self.x as u64 + self.w as u64).max(r.x as u64 + r.w as u64);
        let y1 = (self.y as u64 + self.h as u64).max(r.y as u64 + r.h as u64);
        *self = Rect { x: x0, y: y0, w: (x1 - x0 as u64) as u32, h: (y1 - y0 as u64) as u32 };
    }
}

/// The largest rectangle with the source's aspect inside dw x dh, centred (empty when a side is 0).
pub fn fit(sw: u32, sh: u32, dw: u32, dh: u32) -> Rect {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return Rect::EMPTY;
    }
    let (sw, sh, dw64, dh64) = (sw as u64, sh as u64, dw as u64, dh as u64);
    let (w, h) = if sw * dh64 <= sh * dw64 {
        (((sw * dh64 + sh / 2) / sh), dh64) // the source is the narrower: full height, bars left and right
    } else {
        (dw64, ((sh * dw64 + sw / 2) / sw)) // full width, bars above and below
    };
    let w = w.clamp(1, dw64) as u32;
    let h = h.clamp(1, dh64) as u32;
    Rect { x: (dw - w) / 2, y: (dh - h) / 2, w, h }
}

/// The bytes a pixel of a buffer layout takes (None for one it does not know).
fn buffer_bpp(buffer: i32) -> Option<usize> {
    match buffer {
        BUFFER_RGBA_FP16 => Some(8),
        BUFFER_BGRA_8888 | BUFFER_RGBX_8888 => Some(4),
        _ => None,
    }
}

/// Copy rectangle r of a frame in `format` (FORMAT_*) into a buffer of the same size laid out as `buffer`
/// (BUFFER_*; dst_stride in bytes). A frame into a buffer of its own layout is copied as it is (B,G,R,X into B,G,R,A,
/// half floats into RGBA_FP16); B,G,R,X into R,G,B,X swaps (alpha 0xff); half floats into an 8-bit buffer are encoded
/// to sRGB and clamped (what is outside sRGB is lost); 8-bit into RGBA_FP16 is decoded to linear.
/// EINVAL for a format or buffer it does not know.
pub fn copy(
    dst: &mut [u8],
    dst_stride: usize,
    buffer: i32,
    src: &[u8],
    src_stride: usize,
    format: u32,
    r: Rect,
) -> Result<()> {
    let (input, output) = match (pixels::bpp(format) as usize, buffer_bpp(buffer)) {
        (0, _) | (_, None) => return Err(Errno(libc::EINVAL)),
        (i, Some(o)) => (i, o),
    };
    let w = r.w as usize;
    for row in r.y as usize..r.y as usize + r.h as usize {
        let s = &src[row * src_stride + r.x as usize * input..][..w * input];
        let d = &mut dst[row * dst_stride + r.x as usize * output..][..w * output];
        match (format, buffer) {
            (FORMAT_RGBA16F, BUFFER_RGBA_FP16) | (FORMAT_BGRX, BUFFER_BGRA_8888) => d.copy_from_slice(s),
            (FORMAT_RGBA16F, _) => pixels::rgba16f_to_8(d, s, buffer == BUFFER_RGBX_8888),
            (_, BUFFER_RGBA_FP16) => pixels::bgrx8_to_rgba16f(d, s),
            _ => swap_bgrx(d, s),
        }
    }
    Ok(())
}

/// B,G,R,X pixels into R,G,B,X ones, alpha 0xff.
pub fn swap_bgrx(to: &mut [u8], from: &[u8]) {
    for (d, s) in to.chunks_exact_mut(4).zip(from.chunks_exact(4)) {
        d.copy_from_slice(&[s[2], s[1], s[0], 0xff]);
    }
}

/// The same in place: B,G,R,X pixels become R,G,B,X, alpha 0xff.
pub fn swap_bgrx_in_place(p: &mut [u8]) {
    for px in p.chunks_exact_mut(4) {
        px.swap(0, 2);
        px[3] = 0xff;
    }
}

/// The first post waits for the guest (2026-11-15): the layer is above the OneOS boot screen and opaque, so a posted
/// frame covers it, and until session/ready the frame is black (the empty memfd, then backboardd's first frames) or
/// the kernel console. true while `ready` (SESSION/ready, which xnuhost makes at the first frame of the guest's own
/// with content, or ready-content-wait seconds after its first) does not exist; false when it does, or with no session
/// (None or empty: --socket, displayctl). Any error but ENOENT counts as there, so a fault here never keeps the picture
/// away.
pub fn hold_first(ready: Option<&Path>) -> bool {
    match ready {
        Some(p) if !p.as_os_str().is_empty() => {
            matches!(std::fs::metadata(p), Err(e) if e.raw_os_error() == Some(libc::ENOENT))
        }
        _ => false,
    }
}

/// How often a held helper looks for `ready` between flushes.
pub const HOLD_POLL_MS: i32 = 250;

#[cfg(test)]
mod tests;
