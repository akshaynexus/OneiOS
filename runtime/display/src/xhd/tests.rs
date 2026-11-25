// SPDX-License-Identifier: MIT
//! Port of tests/test_display.c (all but its displayctl part: tests/displayctl.rs): the display.sock protocol against
//! a serving thread (what xnuhost does), the letterbox fit, the damage rectangles, the row copy, direct frames, the
//! snapshot, the layer's buffers. No phone, no Apple files.
use super::*;
use crate::pixels::{half_from_float, half_to_float, half_to_srgb8, srgb8_to_half};
use crate::sys::{self, Errno, Map};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};
use xh_core::bytes::le;

fn err(e: i32) -> Errno {
    Errno(e)
}

/// A folder of its own under the system's temporary folder, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!("oneios-display-{name}.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn test_fit() {
    let f = |sw, sh, dw, dh| {
        let r = fit(sw, sh, dw, dh);
        (r.w, r.h, r.x, r.y)
    };
    assert_eq!(f(1440, 3120, 1440, 3120), (1440, 3120, 0, 0)); // the guest at the panel's native mode: 1:1, no bars
    assert_eq!(f(1440, 3120, 1080, 2340), (1080, 2340, 0, 0)); // the panel left at FHD+: scaled, same shape
    assert_eq!(f(1320, 2868, 1440, 3120), (1436, 3120, 2, 0)); // a mismatch: the iPhone's own frame on the panel
    assert_eq!(f(1320, 2868, 1080, 2340), (1077, 2340, 1, 0)); // One UI at FHD+
    assert_eq!(f(720, 1560, 1440, 3120), (1440, 3120, 0, 0)); // same aspect, scaled up
    assert_eq!(f(1920, 1080, 1440, 3120), (1440, 810, 0, 1155)); // wide in tall: bars above and below
    assert_eq!(f(1320, 2868, 3120, 1440), (663, 1440, 1228, 0)); // tall in wide: bars left and right
    let thin = fit(1, 10000, 100, 100); // never thinner than a pixel
    assert!(thin.w == 1 && thin.h == 100);
    let big = fit(16384, 16384, 16384, 16384); // no overflow at the largest sizes
    assert!(big.w == 16384 && big.h == 16384);
    let none = fit(0, 10, 100, 100);
    assert!(none.w == 0 && none.h == 0);
    for sw in (1..400).step_by(7) {
        for sh in (1..400).step_by(11) {
            let r = fit(sw, sh, 144, 312);
            assert!(
                r.w >= 1 && r.h >= 1 && r.x + r.w <= 144 && r.y + r.h <= 312 && (r.w == 144 || r.h == 312),
                "fit inside the panel, touching two edges: {sw}x{sh} -> {r:?}"
            );
        }
    }
}

#[test]
fn test_rects() {
    let mut r = Rect::new(10, 20, 30, 40);
    assert!(r.clip(100, 100) && r == Rect::new(10, 20, 30, 40));
    r = Rect::new(90, 95, 30, 40);
    assert!(r.clip(100, 100) && r.w == 10 && r.h == 5);
    r = Rect::new(100, 0, 1, 1);
    assert!(!r.clip(100, 100) && r.w == 0);
    r = Rect::new(5, 5, u32::MAX, u32::MAX); // a width that would wrap
    assert!(r.clip(100, 100) && r.w == 95 && r.h == 95);
    r = Rect::new(0xffff_fff0, 0, 0x20, 1);
    assert!(!r.clip(100, 100));
    r = Rect::new(0, 0, 0, 5);
    assert!(!r.clip(100, 100));
    let mut u = Rect::EMPTY;
    u.union(&Rect::new(10, 10, 5, 5));
    assert_eq!(u, Rect::new(10, 10, 5, 5));
    u.union(&Rect::new(50, 2, 10, 10));
    assert_eq!(u, Rect::new(10, 2, 50, 13));
    u.union(&Rect::new(7, 7, 0, 0));
    assert_eq!(u, Rect::new(10, 2, 50, 13));
}

#[test]
fn test_hello_check() {
    let m = Message::new(Kind::Hello, 1320, 2868, 5280, 1);
    let size = 5280i64 * 2868;
    let check = |m: &Message, size: i64| check_hello(&m.to_bytes(), size).map(|_| ());
    assert_eq!(check(&m, size), Ok(()));
    assert_eq!(check_hello(&m.to_bytes(), size).unwrap(), Hello { width: 1320, height: 2868, stride: 5280, format: 1 });
    assert_eq!(check(&m, size + 4096), Ok(()));
    assert_eq!(check(&m, size - 1), Err(err(libc::EINVAL))); // a frame larger than its memfd
    assert_eq!(check_hello(&m.to_bytes()[..16], size).map(|_| ()), Err(err(libc::EPROTO)));
    let mut bad = m;
    bad.magic[3] = b'F';
    assert_eq!(check(&bad, size), Err(err(libc::EPROTO)));
    let with = |f: &dyn Fn(&mut Message)| {
        let mut b = m;
        f(&mut b);
        b
    };
    assert_eq!(check(&with(&|b| b.a = 0), size), Err(err(libc::EINVAL)));
    assert_eq!(check(&with(&|b| b.b = 20000), 1 << 40), Err(err(libc::EINVAL)));
    assert_eq!(check(&with(&|b| b.c = 5276), size), Err(err(libc::EINVAL))); // a stride shorter than a row
    assert_eq!(check(&with(&|b| b.c = 5281), size * 2), Err(err(libc::EINVAL)));
    assert_eq!(check(&with(&|b| b.d = 3), size), Err(err(libc::EINVAL))); // a format this helper does not know
    assert_eq!(check(&with(&|b| b.d = 0), size), Err(err(libc::EINVAL)));
    assert_eq!(check(&with(&|b| b.a = 0x4000_0001), size), Err(err(libc::EINVAL))); // width * 4 wraps to 4
                                                                                    // Half floats: 8 bytes a pixel, the stride a whole number of them.
    let half = Message::new(Kind::Hello, 1440, 3120, 11520, FORMAT_RGBA16F);
    assert_eq!(check(&half, 11520 * 3120), Ok(()));
    assert_eq!(check(&half, 11520 * 3120 - 1), Err(err(libc::EINVAL)));
    let mut b = half;
    b.c = 5760; // a B,G,R,X stride: a half-float row does not fit
    assert_eq!(check(&b, 11520 * 3120), Err(err(libc::EINVAL)));
    b.c = 11524; // not a whole number of pixels
    assert_eq!(check(&b, 11524 * 3120), Err(err(libc::EINVAL)));
}

/// The frame formats' buffers and dataspaces, the names of --dataspace, and the half-float conversions the copy uses.
#[test]
fn test_formats() {
    assert_eq!(buffer_for(FORMAT_BGRX), Some((BUFFER_BGRA_8888, DATASPACE_SRGB)));
    assert_eq!(buffer_for(FORMAT_RGBA16F), Some((BUFFER_RGBA_FP16, DATASPACE_SCRGB_LINEAR)));
    assert_eq!(buffer_for(7), None);
    assert_eq!(dataspace_parse("auto"), Some(-1));
    assert_eq!(dataspace_parse("p3"), Some(DATASPACE_DISPLAY_P3));
    assert_eq!(dataspace_parse("scrgb-linear"), Some(DATASPACE_SCRGB_LINEAR));
    assert_eq!(dataspace_parse("bt709"), None);
    assert_eq!(dataspace_name(DATASPACE_SRGB), "srgb");
    assert_eq!(dataspace_name(1234), "other");
    // Android's values, from its headers (data_space.h): standard << 16 | transfer << 22 | range << 27.
    assert_eq!(DATASPACE_SRGB, 1 << 16 | 2 << 22 | 1 << 27);
    assert_eq!(DATASPACE_DISPLAY_P3, 10 << 16 | 2 << 22 | 1 << 27);
    assert_eq!(DATASPACE_SCRGB, 1 << 16 | 2 << 22 | 3 << 27);
    assert_eq!(DATASPACE_SCRGB_LINEAR, 1 << 16 | 1 << 22 | 3 << 27);
    // Halves: exact values, rounding, the ends.
    assert!(half_from_float(1.0) == 0x3c00 && half_from_float(-2.0) == 0xc000 && half_from_float(0.5) == 0x3800);
    assert!(half_from_float(65504.0) == 0x7bff && half_from_float(1e6) == 0x7c00 && half_from_float(0.0) == 0);
    assert_eq!(half_from_float(5.960_464_5e-8), 0x0001); // the smallest subnormal
    assert!(half_from_float(f32::NAN) & 0x7fff > 0x7c00); // a NaN stays one
    assert!((0..0x7c00u16).all(|h| half_from_float(half_to_float(h)) == h)); // every finite positive half
    assert!(half_to_float(0x3c00) == 1.0 && half_to_float(0xbc00) == -1.0 && half_to_float(0x0400) == 6.103_515_6e-5);
    // sRGB: 8-bit -> linear -> 8-bit gives the byte back; out of range is clamped.
    let lin = srgb8_to_half();
    let enc = half_to_srgb8();
    assert!((0..256).all(|i| enc[lin[i] as usize] as usize == i));
    assert!(lin[0] == 0 && lin[255] == 0x3c00 && enc[0xbc00] == 0 && enc[0x4000] == 255);
    assert!(enc[0x7c00] == 255 && enc[0x7e00] == 0);
    assert_eq!(lin[128], half_from_float(0.215_860_5)); // sRGB's 128 is 21.6% of the light
}

fn halves(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|h| h.to_le_bytes()).collect()
}

fn half_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i * 2], b[i * 2 + 1]])
}

/// The copy between frame formats and buffer layouts.
#[test]
fn test_copy_formats() {
    const W: usize = 3;
    const H: usize = 2;
    let values: [[f32; 3]; W] = [[1.0, 0.0, 0.0], [1.2249, -0.042, -0.0197], [2.0, 0.5, 0.215_860_5]];
    let mut half = Vec::new();
    for _ in 0..H {
        for v in &values {
            half.extend(halves(&[half_from_float(v[0]), half_from_float(v[1]), half_from_float(v[2]), 0x3c00]));
        }
    }
    let all = Rect::whole(W as u32, H as u32);
    let mut out16 = vec![0u8; W * H * 8];
    assert_eq!(copy(&mut out16, W * 8, BUFFER_RGBA_FP16, &half, W * 8, FORMAT_RGBA16F, all), Ok(()));
    assert_eq!(out16, half); // half floats into RGBA_FP16: as they are
    let mut out8 = vec![0u8; W * H * 4];
    let px = |o: &[u8], y: usize, x: usize| -> [u8; 4] { o[(y * W + x) * 4..][..4].try_into().unwrap() };
    assert_eq!(copy(&mut out8, W * 4, BUFFER_BGRA_8888, &half, W * 8, FORMAT_RGBA16F, all), Ok(()));
    // into 8-bit: encoded and clamped, so sRGB's red and P3's red (outside sRGB) become the same 255,0,0
    assert_eq!(px(&out8, 0, 0), [0, 0, 255, 255]);
    assert_eq!(px(&out8, 0, 1)[..3], [0, 0, 255]);
    assert_eq!(px(&out8, 1, 2)[..3], [128, 188, 255]); // 2.0 clamps; 0.5 is sRGB 188; 0.2159 is 128
    assert_eq!(copy(&mut out8, W * 4, BUFFER_RGBX_8888, &half, W * 8, FORMAT_RGBA16F, all), Ok(()));
    assert_eq!(px(&out8, 1, 2), [255, 188, 128, 255]);
    // 8-bit into RGBA_FP16: decoded to linear
    let bgrx = [0u8, 128, 255, 7, 255, 255, 255, 0];
    let mut got = vec![0u8; 16];
    let two = Rect::new(0, 0, 2, 1);
    assert_eq!(copy(&mut got, 16, BUFFER_RGBA_FP16, &bgrx, 8, FORMAT_BGRX, two), Ok(()));
    assert_eq!(
        [half_at(&got, 0), half_at(&got, 1), half_at(&got, 2), half_at(&got, 3)],
        [0x3c00, half_from_float(0.215_860_5), 0, 0x3c00]
    );
    assert_eq!([half_at(&got, 4), half_at(&got, 5), half_at(&got, 6)], [0x3c00; 3]);
    assert_eq!(copy(&mut got, 16, 99, &bgrx, 8, FORMAT_BGRX, two), Err(err(libc::EINVAL)));
    assert_eq!(copy(&mut got, 16, BUFFER_RGBA_FP16, &bgrx, 8, 9, two), Err(err(libc::EINVAL)));
}

#[test]
fn test_copy() {
    const SS: usize = 40; // strides wider than the rows (7 pixels, 5 rows)
    const DS: usize = 32;
    const H: usize = 5;
    let src: Vec<u8> = (0..SS * H).map(|i| (i * 7 + 1) as u8).collect();
    let mut dst = vec![0xeeu8; DS * H];
    let mut want = vec![0xeeu8; DS * H];
    let r = Rect::new(2, 1, 4, 3);
    assert_eq!(copy(&mut dst, DS, BUFFER_BGRA_8888, &src, SS, FORMAT_BGRX, r), Ok(()));
    for y in 1..4 {
        want[y * DS + 8..y * DS + 24].copy_from_slice(&src[y * SS + 8..y * SS + 24]);
    }
    assert_eq!(dst, want); // only the rectangle, bytes as they are
    assert_eq!(copy(&mut dst, DS, BUFFER_RGBX_8888, &src, SS, FORMAT_BGRX, r), Ok(()));
    for y in 1..4 {
        for x in 2..6 {
            let (s, d) = (&src[y * SS + x * 4..][..4], &dst[y * DS + x * 4..][..4]);
            assert_eq!(d, [s[2], s[1], s[0], 0xff]); // B and R swapped, alpha opaque
        }
    }
    assert!(dst[0] == 0xee && dst[DS * 4] == 0xee && dst[DS + 7] == 0xee && dst[DS + 24] == 0xee);
}

// ---- direct frames: guest RAM and the table from xnuhost, the rows copied from scattered segments -----------------

const RAM: usize = 0x10000;
const SLOTS: usize = 4;
const SLOT: usize = 64 + 8 * 16;
const W: u32 = 10;
const H: u32 = 100;
const STRIDE: u64 = 48;
const DS: usize = 44;

/// The test's xnuhost side: guest RAM and the table, mapped read-write.
struct Guest {
    ram: Map,
    table: Map,
}

impl Guest {
    /// The guest's byte b of row y of the scanout: 4800 bytes in two pages out of order (0x8000, then 0x2000).
    fn at(y: u32, b: u32) -> usize {
        let at = y as usize * STRIDE as usize + b as usize;
        if at < 0x1000 {
            0x8000 + at
        } else {
            0x2000 - 0x1000 + at
        }
    }

    fn flip(&mut self, y: u32, b: u32) {
        self.ram.bytes_mut()[Guest::at(y, b)] ^= 0xff;
    }

    fn get(&self, y: u32, b: u32) -> u8 {
        self.ram.bytes()[Guest::at(y, b)]
    }

    fn slot(&mut self, f: impl FnOnce(&mut Slot)) {
        let base = 2 * SLOT;
        let mut s = Slot::decode(&self.table.bytes()[base..]);
        f(&mut s);
        self.table.bytes_mut()[base..base + Slot::BYTES].copy_from_slice(&s.encode());
    }

    fn seg(&mut self, i: usize, seg: Seg) {
        let at = 2 * SLOT + Slot::BYTES + i * Seg::BYTES;
        self.table.bytes_mut()[at..at + Seg::BYTES].copy_from_slice(&seg.encode());
    }
}

#[test]
fn test_direct() {
    let ram = memfd("test-ram", RAM).unwrap();
    let tab = memfd("test-table", SLOTS * SLOT).unwrap();
    let (x, helper) = sys::seqpacket_pair().unwrap();
    let mut g = Guest {
        ram: Map::new(ram.as_fd(), RAM, true).unwrap(),
        table: Map::new(tab.as_fd(), SLOTS * SLOT, true).unwrap(),
    };
    for (i, b) in g.ram.bytes_mut().iter_mut().enumerate() {
        *b = (i * 13 + (i >> 8)) as u8;
    }
    // The scanout: 4800 bytes in two pages out of order, so row 85 runs from one into the other.
    g.seg(0, Seg { offset: 0x8000, length: 0x1000 });
    g.seg(1, Seg { offset: 0x2000, length: 0x1000 });
    g.slot(|s| {
        *s = Slot {
            seq: 7,
            offset: 0,
            stride: STRIDE as u32,
            width: W,
            height: H,
            nsegs: 2,
            x: 1,
            y: 0,
            w: 8,
            h: H,
            format: 1,
        }
    });
    let mem = Message::new(Kind::Memory, RAM as u32, 0, SLOTS as u32, SLOT as u32);
    let mut frame = Message::new(Kind::Frame, 2, 7, 0, 0);
    mem.send(x.as_fd(), &[ram.as_fd(), tab.as_fd()], 0).unwrap();
    frame.send(x.as_fd(), &[], 0).unwrap();
    let mut d = Direct::new();
    let mut damage = Rect::EMPTY;
    assert!(drain_direct(helper.as_fd(), W, H, &mut damage, Some(&mut d)));
    assert!(d.is_mapped() && d.ram_size() == RAM as u64 && d.slots() == SLOTS as u32);
    assert_eq!(d.pending, Some(PendingFrame { slot: 2, seq: 7, sent_us: 0 }));
    let mut dst = vec![0xeeu8; DS * H as usize];
    let mut want = vec![0xeeu8; DS * H as usize];
    for y in 0..H {
        for b in 4..36 {
            want[y as usize * DS + b as usize] = g.get(y, b);
        }
    }
    let done = d.copy_direct(&mut dst, DS, W, H, false, None).unwrap();
    assert!(done.x == 1 && done.w == 8 && done.h == H);
    assert_eq!(dst, want); // every row, across the two pages, nothing else
    d.copy_direct(&mut dst, DS, W, H, true, None).unwrap();
    assert!(dst[85 * DS + 4] == want[85 * DS + 6] && dst[85 * DS + 6] == want[85 * DS + 4] && dst[85 * DS + 7] == 0xff); // swapped
    let done = d.copy_direct(&mut dst, DS, 4, 3, false, None).unwrap();
    assert!(done.w == 3 && done.h == 3); // clipped to the buffer

    // The snapshot: one read of the guest's pages a frame, each tile hashed as read and copied on only when it changed;
    // after the read the guest's pages may change and the snapshot keeps the frame as it was read.
    {
        let mut s = Snapshot::new(W, H).unwrap();
        assert!(!s.full && s.stride == W as usize * 4 && s.tile == 3);
        let (ch, e) = s.update(&d);
        assert!(e.is_ok() && s.full && ch == Rect::whole(W, H)); // the first: all
        let same = (0..H).all(|y| (0..W * 4).all(|x| s.pixels()[y as usize * s.stride + x as usize] == g.get(y, x)));
        assert!(same); // every row, across the two pages
        let (ch, e) = s.update(&d);
        assert!(e.is_ok() && ch.w == 0); // nothing changed: nothing copied
        g.flip(11, 4 * 4); // row 11, pixel 4: tile 1 (pixels 3-5)
        let (ch, e) = s.update(&d);
        assert!(e.is_ok() && ch == Rect::new(3, 11, 3, 1));
        assert_eq!(s.pixels()[11 * s.stride + 16], g.get(11, 16));
        g.flip(12, 3); // the X byte alone is no change
        let (ch, e) = s.update(&d);
        assert!(e.is_ok() && ch.w == 0);
        g.flip(12, 3);
        let kept = s.pixels()[90 * s.stride];
        g.flip(90, 0); // row 90 (in the second page), pixel 0
        assert_eq!(s.pixels()[90 * s.stride], kept); // the guest's write after the read: not in it
        let (ch, e) = s.update(&d);
        assert!(e.is_ok() && ch == Rect::new(0, 90, 3, 1) && s.pixels()[90 * s.stride] == kept ^ 0xff);
        // xnuhost's own pixels over a rectangle: its tiles lose their hashes, so the next frame takes them from the
        // guest again.
        let mem = vec![0x5au8; (W * 4 * H) as usize];
        let mut own = FrameRef { width: W, height: H, stride: W * 4, format: FORMAT_BGRX, pixels: &mem };
        assert_eq!(s.put(&own, Rect::new(4, 20, 2, 3)), Ok(()));
        assert_eq!(s.pixels()[21 * s.stride + 16], 0x5a);
        let (ch, e) = s.update(&d);
        assert!(e.is_ok() && ch == Rect::new(3, 20, 3, 3) && s.pixels()[21 * s.stride + 16] == g.get(21, 16));
        own.format = FORMAT_RGBA16F;
        assert_eq!(s.put(&own, Rect::whole(W, H)), Err(err(libc::EINVAL)));
        // A fault halfway (rows past the segments from row 85): the tiles read before it are in, the snapshot stays
        // whole.
        g.flip(5, 0);
        g.slot(|s| s.nsegs = 1);
        let (ch, e) = s.update(&d);
        assert!(
            e == Err(err(libc::EINVAL)) && s.full && ch.y == 5 && ch.h == 1 && s.pixels()[5 * s.stride] == g.get(5, 0)
        );
        g.slot(|s| {
            s.nsegs = 2;
            s.seq = 11
        });
        let (ch, e) = s.update(&d);
        assert!(e == Err(err(libc::ESTALE)) && ch.w == 0); // the slot was reused: nothing read
        g.slot(|s| s.seq = 7);
        g.flip(5, 0);
        g.flip(90, 0);
        g.flip(11, 16);
    }
    g.slot(|s| s.nsegs = 1);
    assert_eq!(d.copy_direct(&mut dst, DS, W, H, false, None), Err(err(libc::EINVAL))); // rows past the segments
    g.slot(|s| s.nsegs = 2);
    g.seg(1, Seg { offset: RAM as u64 - 0x800, length: 0x1000 });
    assert_eq!(d.copy_direct(&mut dst, DS, W, H, false, None), Err(err(libc::EINVAL))); // a segment past the RAM
    g.seg(1, Seg { offset: 0x2000, length: 0x1000 });
    g.slot(|s| s.seq = 11);
    assert_eq!(d.copy_direct(&mut dst, DS, W, H, false, None), Err(err(libc::ESTALE))); // the slot was reused
    frame.b = 5; // an older frame: the newer stays
    frame.send(x.as_fd(), &[], 0).unwrap();
    assert!(drain_direct(helper.as_fd(), W, H, &mut damage, Some(&mut d)) && d.pending.unwrap().seq == 7);
    assert_eq!(send_done(helper.as_fd(), 7, 0), Ok(()));
    let a = recv_message(x.as_fd());
    assert!(a.is(Kind::Done) && a.a == 7 && a.b == 0);
}

fn recv_message(fd: BorrowedFd<'_>) -> Message {
    let mut p = [0u8; Message::BYTES];
    let got = sys::recv_packet(fd, &mut p, 0).unwrap();
    assert_eq!(got.length, Message::BYTES);
    Message::parse(&p).unwrap()
}

// ---- the layer's buffers: Surface::lock's copy-back, and what the helper fills ------------------------------------

// A model of AOSP's Surface::lock (libs/gui/Surface.cpp) on an 8x8 picture, a region as a 64-bit mask of its pixels:
// copy back `mDirtyRegion - dirty` from the last posted buffer, then mDirtyRegion -= the handed-out buffer's last dirty
// region, that region = dirty, mDirtyRegion |= dirty. No buffer posted yet: nothing copied back, the dirty region whole.
const CB: u32 = 8;
const CB_MAX: usize = 5;

struct SurfaceModel {
    buf: [[u8; 64]; CB_MAX],
    slot_dirty: [u64; CB_MAX],
    dirty: u64,
    front: Option<usize>,
}

fn rect_mask(r: Rect) -> u64 {
    let mut m = 0u64;
    for y in r.y..(r.y + r.h).min(CB) {
        for x in r.x..(r.x + r.w).min(CB) {
            m |= 1 << (y * CB + x);
        }
    }
    m
}

fn mask_bounds(m: u64) -> Rect {
    let mut r = Rect::EMPTY;
    for i in (0..64).filter(|i| m >> i & 1 != 0) {
        r.union(&Rect::new(i % CB, i / CB, 1, 1));
    }
    r
}

impl SurfaceModel {
    fn lock(&mut self, b: usize, want: Rect) -> Rect {
        let mut dirty = rect_mask(want);
        match self.front {
            Some(front) => {
                let back = self.dirty & !dirty;
                if b != front {
                    for i in (0..64).filter(|i| back >> i & 1 != 0) {
                        self.buf[b][i] = self.buf[front][i];
                    }
                }
            }
            None => {
                dirty = !0;
                self.dirty = 0;
                self.slot_dirty = [0; CB_MAX];
            }
        }
        self.dirty &= !self.slot_dirty[b];
        self.slot_dirty[b] = dirty;
        self.dirty |= dirty;
        mask_bounds(dirty)
    }
}

/// 300 frames of a UI in which rectangles change here and there (a widget, a page, a clock). The helper before
/// 2026-11-22 locked only the changed rectangle and filled it (`old`); now it locks the buffer whole and fills what
/// Buffers says the buffer lacks. order 0: the buffers in turn; 1: any buffer but the one shown, at random (a queue
/// that does not hand them out in turn). How many posts did not show the picture, and the pixels filled.
fn run_layer(nbuf: usize, old: bool, order: u32) -> (u32, u64) {
    let mut s = SurfaceModel { buf: [[0; 64]; CB_MAX], slot_dirty: [0; CB_MAX], dirty: 0, front: None };
    let mut picture = [0u8; 64];
    let mut q = Buffers::new(CB, CB);
    let mut seed: u32 = 12345;
    let mut next = || {
        seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
        seed
    };
    let (mut wrong, mut filled) = (0, 0u64);
    for k in 0..300u32 {
        // This frame's change: one rectangle (sometimes two), the frame's number written into it.
        let mut changed = Rect::EMPTY;
        for _ in 0..1 + (k % 5 == 0) as u32 {
            let r1 = next();
            let (x, y) = ((r1 >> 8) % CB, (r1 >> 12) % CB);
            let r2 = next();
            let (w, h) = (1 + (r2 >> 8) % 3, 1 + (r2 >> 12) % 3);
            let mut r = Rect::new(x, y, w, h);
            r.clip(CB, CB);
            for yy in r.y..r.y + r.h {
                for xx in r.x..r.x + r.w {
                    picture[(yy * CB + xx) as usize] = (k + 1) as u8;
                }
            }
            changed.union(&r);
        }
        if k == 0 {
            changed = Rect::whole(CB, CB);
        }
        let b = if order == 0 {
            k as usize % nbuf
        } else {
            let r = next() >> 8;
            match s.front {
                Some(front) if nbuf > 1 => {
                    let c = (r as usize) % (nbuf - 1);
                    if c >= front {
                        c + 1
                    } else {
                        c
                    }
                } // any buffer but the one shown
                _ => r as usize % nbuf,
            }
        };
        let bits = s.buf[b].as_ptr() as usize;
        let fill = if old {
            s.lock(b, changed)
        } else {
            s.lock(b, Rect::whole(CB, CB));
            q.fill(bits, &changed)
        };
        for yy in fill.y..fill.y + fill.h {
            for xx in fill.x..fill.x + fill.w {
                s.buf[b][(yy * CB + xx) as usize] = picture[(yy * CB + xx) as usize];
            }
        }
        filled += fill.w as u64 * fill.h as u64;
        s.front = Some(b); // posted
        if !old {
            q.posted(bits, &changed);
        }
        if s.buf[b] != picture {
            wrong += 1;
        }
    }
    (wrong, filled)
}

#[test]
fn test_buffers() {
    let whole = 300u64 * 64;
    // The fault as the phone had it: with two buffers Surface's copy-back is right, with three or more a post shows an
    // area as it was frames ago.
    assert_eq!(run_layer(2, true, 0).0, 0);
    assert!(run_layer(3, true, 0).0 > 0);
    assert!(run_layer(4, true, 0).0 > 0);
    // Now: right with any number of buffers in any order, and still far from filling every buffer whole.
    for n in 1..=CB_MAX {
        let (wrong, filled) = run_layer(n, false, 0);
        assert_eq!(wrong, 0);
        assert!(n > 3 || filled < whole * 3 / 4); // (on 8x8 the union of five changes is most of it)
        assert_eq!(run_layer(n, false, 1).0, 0);
    }
    // The bookkeeping itself.
    let mut q = Buffers::new(100, 50);
    let bits: Vec<usize> = (1..=BUFFERS + 1).map(|i| i * 64).collect();
    let (a, b) = (Rect::new(1, 2, 3, 4), Rect::new(50, 40, 10, 10));
    assert_eq!(q.fill(bits[0], &a), Rect::whole(100, 50)); // not seen yet: whole
    q.posted(bits[0], &a);
    assert_eq!(q.fill(bits[0], &b), b); // seen, nothing posted since: only this change
    q.posted(bits[1], &b); // another buffer posts b
    assert_eq!(q.fill(bits[0], &a), Rect::new(1, 2, 59, 48)); // it lacks b, and gets a
    q.posted(bits[0], &a);
    assert_eq!(q.fill(bits[1], &Rect::EMPTY), a); // the other one lacks a only
    for &bb in &bits[2..] {
        q.posted(bb, &a);
    }
    assert!(q.len() == BUFFERS && q.forgotten == 1); // nine buffers: the one posted longest ago forgotten
    assert_eq!(q.fill(bits[1], &a), Rect::whole(100, 50)); // and so filled whole
    assert_eq!(q.fill(0, &a), Rect::whole(100, 50));
    q.reset(100, 50);
    let r = q.fill(bits[0], &a);
    assert!(r.w == 100 && q.is_empty()); // new buffers (a format fallback): none known
}

#[test]
fn test_tile_hash() {
    const N: u32 = 360;
    let mut p: Vec<u8> = (0..N as usize * 4).map(|i| (i * 7 + 3) as u8).collect();
    let h = hash_tile(&p, N);
    assert!(h != 0 && h == hash_tile(&p, N) && h != hash_tile(&p, N - 1));
    for i in (3..p.len()).step_by(4) {
        p[i] ^= 0x5a; // the X bytes do not count
    }
    assert_eq!(hash_tile(&p, N), h);
    // Every pair of changes to the top bit of the R byte of two pixels (what a multiply alone would let cancel in 16
    // bits), every single bit of the first pixels, and a tile of all zeros against one of all X.
    for i in 0..64 {
        for j in i + 1..64 {
            p[i * 4 + 2] ^= 0x80;
            p[j * 4 + 2] ^= 0x80;
            assert_ne!(hash_tile(&p, N), h, "pixels {i} and {j}");
            p[i * 4 + 2] ^= 0x80;
            p[j * 4 + 2] ^= 0x80;
        }
    }
    for bit in 0..24 * 4 {
        let at = (bit / 24) * 4 + (bit % 24) / 8;
        p[at] ^= 1 << (bit % 8);
        assert_ne!(hash_tile(&p, N), h, "bit {bit}");
        p[at] ^= 1 << (bit % 8);
    }
    p.fill(0);
    let (zero, odd) = (hash_tile(&p, N), hash_tile(&p, 7));
    for (i, b) in p.iter_mut().enumerate() {
        *b = if i % 4 == 3 { 0xff } else { 0 };
    }
    assert!(zero == hash_tile(&p, N) && zero != odd && hash_tile(&p, 7) == odd);
    // 7: the odd pixel at the end
}

// ---- the protocol against a serving thread ------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Serve {
    Flushes,      // the hello and a round of flushes
    SmallMemfd,   // a frame larger than its memfd
    NoDescriptor, // a hello with no descriptor
}

fn serve(path: PathBuf, delay_ms: u64, width: u32, height: u32, mode: Serve) {
    sys::nap_ms(delay_ms); // the helper is already waiting for the socket
    let listener = listen(&path).unwrap();
    let fd = sys::accept(listener.as_fd()).unwrap();
    let fd = fd.as_fd();
    let bytes = (width * 4 * height) as usize;
    let memfd = memfd("test-frame", if mode == Serve::SmallMemfd { bytes / 2 } else { bytes }).unwrap();
    if mode != Serve::SmallMemfd {
        let mut map = Map::new(memfd.as_fd(), bytes, true).unwrap();
        for (i, b) in map.bytes_mut().iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
    }
    if mode == Serve::NoDescriptor {
        Message::new(Kind::Hello, width, height, width * 4, 1).send(fd, &[], 0).unwrap();
    } else {
        send_hello(fd, width, height, width * 4, FORMAT_BGRX, memfd.as_fd()).unwrap();
    }
    if mode == Serve::Flushes {
        send_flush(fd, 0, 0, width, height).unwrap(); // what xnuhost sends at a connection
        let mut ack = [0u8; 1];
        let _ = sys::recv_packet(fd, &mut ack, 0); // the test read the full flush
        send_flush(fd, 10, 10, 20, 20).unwrap();
        send_flush(fd, 100, 5, 20, 20).unwrap();
        send_flush(fd, width - 4, height - 4, 50, 50).unwrap(); // reaches past the frame: clipped
        send_flush(fd, width + 5, 0, 5, 5).unwrap(); // outside: dropped
        Message { magic: *b"NOPE", a: 0, b: 0, c: 9999, d: 9999 }.send(fd, &[], 0).unwrap(); // unknown: ignored
        sys::send_packet(fd, b"short", &[], 0).unwrap();
        let _ = sys::recv_packet(fd, &mut ack, 0);
    }
}

#[test]
fn test_panel_report() {
    let (a, b) = sys::seqpacket_pair().unwrap();
    assert_eq!(send_panel(a.as_fd(), 1440, 3120, 120, PANEL_DIRECT), Ok(()));
    assert_eq!(recv_message(b.as_fd()), Message::new(Kind::Panel, 1440, 3120, 120, 1));
}

/// The first post waits for SESSION/ready (2026-11-15: the layer covers the OneOS boot screen, and the frames before
/// ready are black or the console): held while it is missing, not once it is there, never without a session.
#[test]
fn test_hold_first() {
    let dir = TempDir::new("hold");
    let ready = dir.0.join("ready");
    assert!(hold_first(Some(&ready)));
    assert!(!hold_first(None) && !hold_first(Some(Path::new("")))); // --socket: no hold
    std::fs::write(&ready, "ready\n").unwrap();
    assert!(!hold_first(Some(&ready))); // xnuhost said ready
    std::fs::remove_file(&ready).unwrap();
    std::fs::create_dir(&ready).unwrap();
    assert!(!hold_first(Some(&ready))); // whatever it is: there
}

#[test]
fn test_protocol() {
    let dir = TempDir::new("protocol");
    let path = dir.0.join("display.sock");
    let p = path.clone();
    let server = std::thread::spawn(move || serve(p, 300, 64, 48, Serve::Flushes));
    assert_eq!(connect(&path, 0, &no_stop).err(), Some(err(libc::ENOENT))); // not there yet, and no wait asked
    let fd = connect(&path, 5000, &no_stop).unwrap(); // waits for xnuhost to make it
    let frame = recv_hello(fd.as_fd(), 5000).unwrap();
    assert!(frame.width == 64 && frame.height == 48 && frame.stride == 256 && frame.format == FORMAT_BGRX);
    assert_eq!(frame.bytes(), 256 * 48);
    assert!(frame.pixels().iter().enumerate().all(|(i, &b)| b == (i % 251) as u8)); // the shared frame, mapped
    let mut damage = Rect::EMPTY;
    for _ in 0..200 {
        assert!(drain(fd.as_fd(), 64, 48, &mut damage));
        if damage.w != 0 {
            break;
        }
        sys::nap_ms(10);
    }
    assert_eq!(damage, Rect::whole(64, 48));
    damage = Rect::EMPTY;
    assert!(drain(fd.as_fd(), 64, 48, &mut damage) && damage.w == 0); // nothing pending, and it did not block
    sys::send_packet(fd.as_fd(), b"a", &[], 0).unwrap();
    sys::nap_ms(200);
    assert!(drain(fd.as_fd(), 64, 48, &mut damage));
    // 10,10 20x20 + 100,5 20x20 (clipped to 64 wide: dropped, x >= width) + the corner 60,44 4x4
    assert_eq!(damage, Rect::new(10, 10, 54, 38));
    sys::send_packet(fd.as_fd(), b"b", &[], 0).unwrap();
    server.join().unwrap();
    damage = Rect::EMPTY;
    assert!(!drain(fd.as_fd(), 64, 48, &mut damage)); // xnuhost closed: the session is over
    drop(frame);
    drop(fd);

    // Refused hellos: a frame larger than its memfd, and no memfd at all.
    for (mode, want) in [(Serve::SmallMemfd, libc::EINVAL), (Serve::NoDescriptor, libc::EPROTO)] {
        let p = path.clone();
        let server = std::thread::spawn(move || serve(p, 0, 64, 48, mode));
        let fd = connect(&path, 5000, &no_stop).unwrap();
        assert_eq!(recv_hello(fd.as_fd(), 5000).err(), Some(err(want)));
        drop(fd);
        server.join().unwrap();
    }

    // Not a socket: a link (even to a socket) and a plain file are refused; a stop ends the wait.
    let other = dir.0.join("real.sock");
    let link = dir.0.join("link.sock");
    let listener = listen(&other).unwrap();
    std::os::unix::fs::symlink(&other, &link).unwrap();
    assert_eq!(connect(&link, 0, &no_stop).err(), Some(err(libc::ENOTSOCK)));
    drop(listener);
    std::fs::remove_file(&link).unwrap();
    std::fs::remove_file(&other).unwrap();
    std::fs::write(&link, "").unwrap();
    assert_eq!(connect(&link, 1000, &no_stop).err(), Some(err(libc::ENOTSOCK)));
    std::fs::remove_file(&link).unwrap();
    assert_eq!(connect(&link, 60000, &|| true).err(), Some(err(libc::ENOENT))); // returns at once
                                                                                // A socket nobody listens on any more: refused, after the wait.
    drop(listen(&other).unwrap());
    assert_eq!(connect(&other, 200, &no_stop).err(), Some(err(libc::ECONNREFUSED)));
    // A hello that never comes.
    let (a, b) = sys::seqpacket_pair().unwrap();
    assert_eq!(recv_hello(a.as_fd(), 100).err(), Some(err(libc::ETIMEDOUT)));
    drop(b);
    assert_eq!(recv_hello(a.as_fd(), 100).err(), Some(err(libc::ECONNRESET)));
}

/// The table's layout is xnuhost's too (its display.rs writes this Slot; 64 bytes, as the C's struct direct_slot).
#[test]
fn test_slot_layout() {
    let s = Slot { seq: 1, offset: 2, stride: 3, width: 4, height: 5, nsegs: 6, x: 7, y: 8, w: 9, h: 10, format: 11 };
    let b = s.encode();
    assert_eq!(b.len(), 64);
    assert_eq!(le::u32(&b, 48), 11);
    assert_eq!(Slot::decode(&b), s);
    assert_eq!(Seg::decode(&Seg { offset: 5, length: 6 }.encode()), Seg { offset: 5, length: 6 });
    assert_eq!(Message::parse(&Message::new(Kind::Frame, 1, 2, 3, 4).to_bytes()).unwrap().kind(), Some(Kind::Frame));
}

// ---- the run loop (xhd::run): the reader answers while the poster waits for a buffer --------------------------------

/// A stand-in layer: `n` buffers handed out in turn, each lock blocking `lock_ms` (a queue that is full until the
/// panel's next vsync), every post kept as a copy. `fail_after`: locks after that many fail (the NDK's -22).
struct FakeLayer {
    n: usize,
    lock_ms: u64,
    fail_after: usize,
    width: u32,
    height: u32,
    state: std::sync::Mutex<FakeState>,
}

struct FakeState {
    buffers: Vec<Box<[u8]>>,
    locks: usize,
    locked: Option<usize>,
    posted: Vec<Vec<u8>>,
}

impl FakeLayer {
    fn new(n: usize, lock_ms: u64, fail_after: usize, width: u32, height: u32) -> FakeLayer {
        let bytes = (width * height * 4) as usize;
        let buffers = (0..n).map(|_| vec![0x77u8; bytes].into_boxed_slice()).collect();
        FakeLayer {
            n,
            lock_ms,
            fail_after,
            width,
            height,
            state: std::sync::Mutex::new(FakeState { buffers, locks: 0, locked: None, posted: Vec::new() }),
        }
    }

    fn posted(&self) -> Vec<Vec<u8>> {
        self.state.lock().unwrap().posted.clone()
    }
}

impl run::Layer for FakeLayer {
    fn set_geometry(&self, _: u32, _: u32, _: i32) {}
    fn set_dataspace(&self, _: i32) -> i32 {
        0
    }
    fn dataspace(&self) -> i32 {
        DATASPACE_SRGB
    }
    fn lock(&self, _: u32, _: u32) -> std::result::Result<run::Locked, i32> {
        sys::nap_ms(self.lock_ms);
        let mut s = self.state.lock().unwrap();
        if s.locks >= self.fail_after {
            return Err(-22);
        }
        let i = s.locks % self.n;
        s.locks += 1;
        s.locked = Some(i);
        let bits = s.buffers[i].as_mut_ptr();
        Ok(run::Locked {
            width: self.width as i32,
            height: self.height as i32,
            stride: self.width as i32,
            format: BUFFER_BGRA_8888,
            bits,
            key: bits as usize,
        })
    }
    fn unlock_and_post(&self, _: &Rect) {
        let mut s = self.state.lock().unwrap();
        let i = s.locked.take().unwrap();
        let copy = s.buffers[i].to_vec();
        s.posted.push(copy);
    }
}

/// Frame k's pixels: B,G,R from k and the byte's place, X 0.
fn run_pattern(k: u32, bytes: usize) -> Vec<u8> {
    (0..bytes).map(|i| if i % 4 == 3 { 0 } else { (k as usize * 37 + i * 7 + (i >> 9)) as u8 }).collect()
}

/// xnuhost's side for the run tests: the hello and its memfd, guest RAM, the table, the frames.
struct RunGuest {
    x: std::os::fd::OwnedFd,
    ram: Map,
    table: Map,
    bytes: usize,
}

const RW: u32 = 64;
const RH: u32 = 48;
const RSLOT: usize = 64 + 4 * 16;

fn run_guest() -> (RunGuest, std::os::fd::OwnedFd, Frame) {
    let bytes = (RW * RH * 4) as usize;
    let ram = memfd("run-ram", bytes).unwrap();
    let tab = memfd("run-table", 4 * RSLOT).unwrap();
    let shm = memfd("run-frame", bytes).unwrap();
    let (x, helper) = sys::seqpacket_pair().unwrap();
    send_hello(x.as_fd(), RW, RH, RW * 4, FORMAT_BGRX, shm.as_fd()).unwrap();
    let frame = recv_hello(helper.as_fd(), 1000).unwrap();
    Message::new(Kind::Memory, bytes as u32, 0, 4, RSLOT as u32)
        .send(x.as_fd(), &[ram.as_fd(), tab.as_fd()], 0)
        .unwrap();
    let g = RunGuest {
        x,
        ram: Map::new(ram.as_fd(), bytes, true).unwrap(),
        table: Map::new(tab.as_fd(), 4 * RSLOT, true).unwrap(),
        bytes,
    };
    (g, helper, frame)
}

impl RunGuest {
    /// Draws frame k into the guest's pages, writes its slot (seq last) and sends it; the time it was sent.
    fn send(&mut self, k: u32) -> i64 {
        self.ram.bytes_mut().copy_from_slice(&run_pattern(k, self.bytes));
        let base = (k as usize % 4) * RSLOT;
        let s = Slot {
            seq: k as u64,
            offset: 0,
            stride: RW * 4,
            width: RW,
            height: RH,
            nsegs: 1,
            x: 0,
            y: 0,
            w: RW,
            h: RH,
            format: 1,
        };
        self.table.bytes_mut()[base..base + Slot::BYTES].copy_from_slice(&s.encode());
        let seg = Seg { offset: 0, length: self.bytes as u64 };
        self.table.bytes_mut()[base + Slot::BYTES..base + Slot::BYTES + Seg::BYTES].copy_from_slice(&seg.encode());
        let sent = sys::now_us();
        Message::new(Kind::Frame, k % 4, k, 0, sent as u32).send(self.x.as_fd(), &[], 0).unwrap();
        sent
    }

    /// The helper's done for frame k: its flags, and how long after `sent` it came.
    fn done(&self, k: u32, sent: i64) -> (u32, i64) {
        let (n, _) = sys::poll(&[(self.x.as_fd(), libc::POLLIN)], 5000, true).unwrap();
        assert_eq!(n, 1, "no answer for frame {k} within 5 s");
        let m = recv_message(self.x.as_fd());
        assert!(m.is(Kind::Done) && m.a == k, "the answer is for frame {k}: {m:?}");
        (m.c, sys::now_us() - sent)
    }
}

/// The 2026-11-24 split (fix/frame-path): each frame is answered when it is in the snapshot, not after the poster's
/// buffer wait. With a layer whose every lock blocks 100 ms (the one loop of before answered a frame that came during
/// a lock only after it: up to 100 ms here, the phone's 5.5 ms late start), twenty frames sent one after another are
/// each answered within 50 ms; every post is one whole frame as read (never half of the next one, though the guest
/// draws the next into the same pages right after each answer), in order; the last post is the last frame; the socket's
/// close ends the run.
#[test]
fn test_run_answers_before_the_post() {
    let (mut g, helper, frame) = run_guest();
    let layer = std::sync::Arc::new(FakeLayer::new(3, 100, usize::MAX, RW, RH));
    let l = layer.clone();
    let poster = std::thread::spawn(move || {
        let mut d = Direct::new();
        let end =
            run::run(helper.as_fd(), &frame, &mut d, &*l, BUFFER_BGRA_8888, DATASPACE_SRGB, None, None, &|| false);
        (end, d)
    });
    let mut worst = 0;
    for k in 1..=20 {
        let sent = g.send(k);
        let (flags, took) = g.done(k, sent);
        assert_eq!(flags, 0, "frame {k} changed: answered as read");
        worst = worst.max(took);
    }
    assert!(worst < 50_000, "the slowest answer took {worst} us: it waited for the layer's buffer");
    // The last frame reaches the layer within two of its locks.
    let last = run_pattern(20, g.bytes);
    let mut shown = false;
    for _ in 0..50 {
        if layer.posted().last() == Some(&last) {
            shown = true;
            break;
        }
        sys::nap_ms(10);
    }
    assert!(shown, "the last frame was posted");
    drop(g); // xnuhost closed the socket
    let (end, _) = poster.join().unwrap();
    assert_eq!(end, run::End::Closed);
    let posted = layer.posted();
    assert!(!posted.is_empty() && posted.len() < 20, "frames that came during a lock are one post: {}", posted.len());
    let mut last_k = 0;
    for (i, p) in posted.iter().enumerate() {
        // the first post is the memfd (zeros) or a frame; each one is one whole frame, never older than the last
        let k = (0..=20).find(|&k| if k == 0 { p.iter().all(|&b| b == 0) } else { *p == run_pattern(k, p.len()) });
        let k = k.unwrap_or_else(|| panic!("post {i} is no whole frame (torn, or a stale buffer)"));
        assert!(k >= last_k, "post {i} shows frame {k} after frame {last_k}");
        last_k = k;
    }
}

/// The poster's failure ends the reader (it may sleep in poll for good), and a frame then is still answered; a byte in
/// the signal pipe ends both with Stopped.
#[test]
fn test_run_ends() {
    // no lock works: the first failure falls back to R,G,B,X (as before the split), the second is LockFailed; the
    // reader, asleep in poll, ends too
    let (g, helper, frame) = run_guest();
    let layer = FakeLayer::new(2, 0, 0, RW, RH);
    let started = sys::now_us();
    let mut d = Direct::new();
    let end = run::run(helper.as_fd(), &frame, &mut d, &layer, BUFFER_BGRA_8888, DATASPACE_SRGB, None, None, &|| false);
    assert_eq!(end, run::End::LockFailed);
    assert!(sys::now_us() - started < 3_000_000, "ended at once");
    assert!(layer.posted().is_empty());
    drop(g);

    // a stop through the signal pipe
    let (g, helper, frame) = run_guest();
    let layer = FakeLayer::new(2, 0, usize::MAX, RW, RH);
    let (r, w) = sys::pipe().unwrap();
    let end = std::thread::scope(|s| {
        let poster = s.spawn(|| {
            let mut d = Direct::new();
            run::run(
                helper.as_fd(),
                &frame,
                &mut d,
                &layer,
                BUFFER_BGRA_8888,
                DATASPACE_SRGB,
                None,
                Some(r.as_fd()),
                &|| false,
            )
        });
        sys::nap_ms(50);
        let byte = 1u8;
        // SAFETY: one byte from a live buffer into the pipe's write end.
        unsafe { libc::write(std::os::fd::AsRawFd::as_raw_fd(&w), &byte as *const u8 as *const libc::c_void, 1) };
        poster.join().unwrap()
    });
    assert_eq!(end, run::End::Stopped);
    drop(g);
}

/// display-post=serial (xnuhost's options message): the reader reads no frame until the last change is posted, the
/// one loop's timing of before. With 100 ms locks, a frame sent right after the answer to the one before waits for that
/// post: its answer takes most of a lock.
#[test]
fn test_run_serial() {
    let (mut g, helper, frame) = run_guest();
    Message::new(Kind::Options, OPTION_SERIAL_POST, 0, 0, 0).send(g.x.as_fd(), &[], 0).unwrap();
    let layer = FakeLayer::new(3, 100, usize::MAX, RW, RH);
    let end = std::thread::scope(|s| {
        let poster = s.spawn(|| {
            let mut d = Direct::new();
            let end =
                run::run(helper.as_fd(), &frame, &mut d, &layer, BUFFER_BGRA_8888, DATASPACE_SRGB, None, None, &|| {
                    false
                });
            assert_eq!(d.options, OPTION_SERIAL_POST);
            end
        });
        let mut slowest = 0;
        for k in 1..=3 {
            let sent = g.send(k);
            slowest = slowest.max(g.done(k, sent).1);
        }
        assert!(slowest >= 50_000, "a frame waited for the post before it: slowest answer {slowest} us");
        drop(g);
        poster.join().unwrap()
    });
    assert_eq!(end, run::End::Closed);
}

// ---- display-present=ahb (xhd::present) ---------------------------------------------------------------------------

impl<L: run::Layer + ?Sized> run::Layer for &L {
    fn set_geometry(&self, w: u32, h: u32, f: i32) {
        (**self).set_geometry(w, h, f)
    }
    fn set_dataspace(&self, d: i32) -> i32 {
        (**self).set_dataspace(d)
    }
    fn dataspace(&self) -> i32 {
        (**self).dataspace()
    }
    fn lock(&self, w: u32, h: u32) -> std::result::Result<run::Locked, i32> {
        (**self).lock(w, h)
    }
    fn unlock_and_post(&self, changed: &Rect) {
        (**self).unlock_and_post(changed)
    }
    fn choose(&self, options: u32) {
        (**self).choose(options)
    }
}

/// A post the fake compositor took: its slot, damage, first-post dataspace, the buffer's bytes, its completion.
struct FakePost {
    slot: usize,
    damage: Rect,
    first: Option<i32>,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct FakeAhbState {
    /// formats isSupported says yes to
    supported: Vec<i32>,
    no_child: bool,
    alloc_fails_at: Option<usize>,
    /// the lock (counted from 0) that fails, and the post
    lock_fails_at: Option<usize>,
    post_fails_at: Option<usize>,
    /// completions are given back by the test (else at once, with a signalled release fence)
    hold: bool,
    buffers: Vec<Vec<u8>>,
    locks: usize,
    posts: Vec<FakePost>,
    held: Vec<present::Done>,
    opened: bool,
    closed: bool,
}

#[derive(Clone, Default)]
struct FakeAhb(std::sync::Arc<std::sync::Mutex<FakeAhbState>>);

impl FakeAhb {
    fn working() -> FakeAhb {
        let f = FakeAhb::default();
        f.st().supported = vec![BUFFER_BGRA_8888, BUFFER_RGBX_8888];
        f
    }
    fn st(&self) -> std::sync::MutexGuard<'_, FakeAhbState> {
        self.0.lock().unwrap()
    }
    /// A fence that has signalled (a pipe with a byte in it), or one that signals when its writer is written.
    fn fence(signalled: bool) -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
        let (r, w) = sys::pipe().unwrap();
        if signalled {
            let byte = 1u8;
            // SAFETY: one byte from a live buffer into the pipe's write end.
            unsafe { libc::write(std::os::fd::AsRawFd::as_raw_fd(&w), &byte as *const u8 as *const libc::c_void, 1) };
        }
        (r, w)
    }
}

impl present::Backend for FakeAhb {
    fn supported(&mut self, _: u32, _: u32, format: i32) -> bool {
        self.st().supported.contains(&format)
    }
    fn open_child(&mut self) -> std::result::Result<(), String> {
        let mut s = self.st();
        if s.no_child {
            return Err("none".into());
        }
        s.opened = true;
        Ok(())
    }
    fn allocate(&mut self, slot: usize, w: u32, h: u32, _: i32) -> std::result::Result<u32, i32> {
        let mut s = self.st();
        if s.alloc_fails_at == Some(slot) {
            return Err(-12);
        }
        s.buffers.push(vec![0x55; (w * h * 4) as usize]);
        Ok(w)
    }
    fn lock(&mut self, slot: usize) -> std::result::Result<*mut u8, i32> {
        let mut s = self.st();
        let n = s.locks;
        s.locks += 1;
        if s.lock_fails_at == Some(n) {
            return Err(-22);
        }
        Ok(s.buffers[slot].as_mut_ptr())
    }
    fn unlock(&mut self, _: usize) -> std::result::Result<Option<std::os::fd::OwnedFd>, i32> {
        Ok(None)
    }
    fn post(
        &mut self,
        slot: usize,
        _: Option<std::os::fd::OwnedFd>,
        damage: Rect,
        first: Option<i32>,
        done: present::Done,
    ) -> std::result::Result<(), i32> {
        let mut s = self.st();
        if s.post_fails_at == Some(s.posts.len()) {
            return Err(-32);
        }
        let bytes = s.buffers[slot].clone();
        s.posts.push(FakePost { slot, damage, first, bytes });
        if s.hold {
            s.held.push(done);
        } else {
            drop(s);
            done(Some(FakeAhb::fence(true).0));
        }
        Ok(())
    }
    fn close(&mut self) {
        self.st().closed = true;
    }
}

const PW: u32 = 16;
const PH: u32 = 8;

/// One frame through the layer as post_loop does it: choose (first), lock, a byte written, post.
fn present_frame(layer: &dyn run::Layer, options: u32, changed: Rect, byte: u8) -> run::Locked {
    layer.choose(options);
    let b = layer.lock(PW, PH).unwrap();
    // SAFETY: the locked buffer is stride x height pixels of 4 bytes.
    unsafe { *b.bits = byte };
    layer.unlock_and_post(&changed);
    b
}

#[test]
fn test_present_choice() {
    use present::wanted;
    assert!(wanted(OPTION_PRESENT_AHB, None));
    assert!(wanted(OPTION_PRESENT_AHB | OPTION_SERIAL_POST, None));
    assert!(!wanted(0, None) && !wanted(OPTION_SERIAL_POST, None));
    // the helper's own environment wins either way; another word is no word
    assert!(wanted(0, Some("ahb")) && !wanted(OPTION_PRESENT_AHB, Some("window")));
    assert!(wanted(OPTION_PRESENT_AHB, Some("other")) && !wanted(0, Some("")));
}

/// Every reason not to take the write-only path ends on the window, which then gets every frame; whatever was opened
/// is closed.
#[test]
fn test_present_fallbacks() {
    type Setup = fn(&mut FakeAhbState);
    let cases: [(&str, u32, u32, Setup, bool); 6] = [
        ("not asked", 0, FORMAT_BGRX, |_| {}, false),
        ("half floats", OPTION_PRESENT_AHB, FORMAT_RGBA16F, |_| {}, false),
        ("no such buffers", OPTION_PRESENT_AHB, FORMAT_BGRX, |s| s.supported.clear(), false),
        ("no child", OPTION_PRESENT_AHB, FORMAT_BGRX, |s| s.no_child = true, false),
        ("the second buffer refused", OPTION_PRESENT_AHB, FORMAT_BGRX, |s| s.alloc_fails_at = Some(1), true),
        ("the environment says window", OPTION_PRESENT_AHB, FORMAT_BGRX, |_| {}, false),
    ];
    for (what, options, frame_format, setup, opened) in cases {
        let window = FakeLayer::new(3, 0, usize::MAX, PW, PH);
        let ahb = FakeAhb::working();
        setup(&mut ahb.st());
        let env = (what == "the environment says window").then(|| "window".to_string());
        let p = present::Present::new(&window, ahb.clone(), frame_format, env);
        let format = if frame_format == FORMAT_BGRX { BUFFER_BGRA_8888 } else { BUFFER_RGBA_FP16 };
        run::Layer::set_geometry(&p, PW, PH, format);
        for k in 0..4 {
            present_frame(&p, options, Rect::new(0, 0, PW, 1), k);
        }
        assert!(!p.is_ahb(), "{what}: the window path");
        assert_eq!(window.posted().len(), 4, "{what}: every frame through the window");
        assert!(ahb.st().posts.is_empty(), "{what}: nothing on the child");
        assert_eq!(ahb.st().opened, opened, "{what}");
        drop(p);
        // a child that was made is hidden and let go (the refusal closes; a refusal before it closes nothing open)
        assert!(!ahb.st().opened || ahb.st().closed, "{what}: closed");
    }
    // R,G,B,X when B,G,R,A buffers are not to be had
    let window = FakeLayer::new(3, 0, usize::MAX, PW, PH);
    let ahb = FakeAhb::working();
    ahb.st().supported = vec![BUFFER_RGBX_8888];
    let p = present::Present::new(&window, ahb.clone(), FORMAT_BGRX, None);
    run::Layer::set_geometry(&p, PW, PH, BUFFER_BGRA_8888);
    let b = present_frame(&p, OPTION_PRESENT_AHB, Rect::new(0, 0, PW, 1), 1);
    assert!(p.is_ahb() && b.format == BUFFER_RGBX_8888 && window.posted().is_empty());
}

/// The ring: three buffers in turn, told apart by their slot; the first post is whole and says the dataspace, the
/// others carry their change as damage; the window is never locked; the end hides the child.
#[test]
fn test_present_ring() {
    let window = FakeLayer::new(3, 0, usize::MAX, PW, PH);
    let ahb = FakeAhb::working();
    let p = present::Present::new(&window, ahb.clone(), FORMAT_BGRX, None);
    run::Layer::set_geometry(&p, PW, PH, BUFFER_BGRA_8888);
    assert_eq!(run::Layer::set_dataspace(&p, DATASPACE_SRGB), 0);
    let mut keys = Vec::new();
    for k in 0..5u8 {
        let b = present_frame(&p, OPTION_PRESENT_AHB, Rect::new(0, k as u32, PW, 1), k);
        assert_eq!((b.width, b.height, b.stride, b.format), (PW as i32, PH as i32, PW as i32, BUFFER_BGRA_8888));
        keys.push(b.key);
    }
    assert!(p.is_ahb());
    assert_eq!(keys, [1, 2, 3, 1, 2]);
    assert_eq!(run::Layer::dataspace(&p), DATASPACE_SRGB);
    {
        let s = ahb.st();
        assert_eq!(s.posts.iter().map(|q| q.slot).collect::<Vec<_>>(), [0, 1, 2, 0, 1]);
        assert_eq!(s.posts[0].first, Some(DATASPACE_SRGB));
        assert_eq!(s.posts[0].damage, Rect::whole(PW, PH));
        assert!(s.posts[1..].iter().all(|q| q.first.is_none()));
        assert_eq!(s.posts[3].damage, Rect::new(0, 3, PW, 1));
        assert_eq!(s.posts[4].bytes[0], 4, "what was written is what was posted");
    }
    assert!(window.posted().is_empty());
    drop(p);
    assert!(ahb.st().closed);
}

/// At most two posts in flight: the third lock waits for a completion; a buffer is written again only after the
/// release fence of its replacement's post signalled.
#[test]
fn test_present_in_flight_and_release() {
    let window = FakeLayer::new(3, 0, usize::MAX, PW, PH);
    let ahb = FakeAhb::working();
    ahb.st().hold = true;
    let p = present::Present::new(&window, ahb.clone(), FORMAT_BGRX, None);
    run::Layer::set_geometry(&p, PW, PH, BUFFER_BGRA_8888);
    present_frame(&p, OPTION_PRESENT_AHB, Rect::whole(PW, PH), 0); // slot 0, in flight
    present_frame(&p, OPTION_PRESENT_AHB, Rect::whole(PW, PH), 1); // slot 1, in flight
    let (fence_r, fence_w) = FakeAhb::fence(false);
    let a = ahb.clone();
    let t0 = sys::now_us();
    let (lock_us, release_us) = std::thread::scope(|s| {
        s.spawn(move || {
            sys::nap_ms(150);
            // the first post completes (it replaced nothing), then the second, which gives slot 0's release fence
            let mut held = std::mem::take(&mut a.st().held);
            let second = held.pop().unwrap();
            let first = held.pop().unwrap();
            first(None);
            second(Some(fence_r));
        });
        let b = run::Layer::lock(&p, PW, PH).unwrap(); // the third: slot 2, after a completion
        let lock_us = sys::now_us() - t0;
        assert_eq!(b.key, 3);
        run::Layer::unlock_and_post(&p, &Rect::whole(PW, PH));
        // the fourth is slot 0 again: its release fence has not signalled, so the lock waits for it (or 200 ms)
        let writer = std::thread::spawn(move || {
            sys::nap_ms(100);
            let byte = 1u8;
            // SAFETY: one byte from a live buffer into the pipe's write end.
            unsafe {
                libc::write(std::os::fd::AsRawFd::as_raw_fd(&fence_w), &byte as *const u8 as *const libc::c_void, 1)
            };
            fence_w
        });
        // a completion for the third, so the fourth does not wait for one in flight
        sys::nap_ms(20);
        let third = ahb.st().held.pop().unwrap();
        third(None);
        let t1 = sys::now_us();
        let b = run::Layer::lock(&p, PW, PH).unwrap();
        let release_us = sys::now_us() - t1;
        assert_eq!(b.key, 1);
        run::Layer::unlock_and_post(&p, &Rect::whole(PW, PH));
        drop(writer.join().unwrap());
        (lock_us, release_us)
    });
    assert!(lock_us >= 120_000, "the third lock waited for a completion: {lock_us} us");
    assert!((50_000..190_000).contains(&release_us), "the fourth waited for slot 0's release fence: {release_us} us");
    // the rest complete: the end does not wait for them
    for done in std::mem::take(&mut ahb.st().held) {
        done(None);
    }
    drop(p);
    assert!(ahb.st().closed && window.posted().is_empty());
}

/// A lock or a post refused later on: the window from then on, with the dataspace the child had.
#[test]
fn test_present_gives_up() {
    for lock in [true, false] {
        let window = FakeLayer::new(3, 0, usize::MAX, PW, PH);
        let ahb = FakeAhb::working();
        if lock {
            ahb.st().lock_fails_at = Some(2);
        } else {
            ahb.st().post_fails_at = Some(2);
        }
        let p = present::Present::new(&window, ahb.clone(), FORMAT_BGRX, None);
        run::Layer::set_geometry(&p, PW, PH, BUFFER_BGRA_8888);
        run::Layer::set_dataspace(&p, DATASPACE_SRGB);
        for k in 0..5 {
            present_frame(&p, OPTION_PRESENT_AHB, Rect::whole(PW, PH), k);
        }
        assert!(!p.is_ahb());
        assert!(ahb.st().closed);
        assert_eq!(ahb.st().posts.len(), 2);
        // the refused lock's frame went through the window; a refused post's frame is lost, the next ones go there
        assert_eq!(window.posted().len(), if lock { 3 } else { 2 }, "lock fails: {lock}");
    }
}

/// The whole helper: xnuhost's options say ahb, and every frame reaches the child's buffers (the last one shown is
/// the last frame), none the window's.
#[test]
fn test_present_run() {
    let (mut g, helper, frame) = run_guest();
    Message::new(Kind::Options, OPTION_PRESENT_AHB, 0, 0, 0).send(g.x.as_fd(), &[], 0).unwrap();
    let window = FakeLayer::new(3, 0, usize::MAX, RW, RH);
    let ahb = FakeAhb::working();
    let p = present::Present::new(&window, ahb.clone(), FORMAT_BGRX, None);
    let end = std::thread::scope(|s| {
        let poster = s.spawn(|| {
            let mut d = Direct::new();
            run::run(helper.as_fd(), &frame, &mut d, &p, BUFFER_BGRA_8888, DATASPACE_SRGB, None, None, &|| false)
        });
        for k in 1..=6 {
            let sent = g.send(k);
            g.done(k, sent);
        }
        let last = run_pattern(6, g.bytes);
        let mut shown = false;
        for _ in 0..100 {
            if ahb.st().posts.last().is_some_and(|q| q.bytes == last) {
                shown = true;
                break;
            }
            sys::nap_ms(10);
        }
        assert!(shown, "the last frame is on the child");
        drop(g);
        poster.join().unwrap()
    });
    assert_eq!(end, run::End::Closed);
    assert!(p.is_ahb() && window.posted().is_empty());
    drop(p);
    assert!(ahb.st().closed);
}
