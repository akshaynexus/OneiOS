// SPDX-License-Identifier: MIT
//! The stand-in encoder (backend_stub.c): for a host with no MediaCodec (the PC; xnuhost's encoder=sim, and
//! encoder=auto with the bridges off) and for the host test. It compresses nothing, and what it writes is a real
//! stream that any decoder decodes to the frame it was given:
//! - HEVC: VPS, SPS, PPS and one IDR slice (Main profile, 16x16 coding tree blocks) whose every coding unit is I_PCM:
//!   the frame's own samples, raw. The only arithmetic-coded bins are part_mode and the two terminating bins
//!   (pcm_flag, end_of_slice_segment_flag), so the CABAC encoder below is the standard's, whole, in 40 lines.
//! - H.264: SPS, PPS and one IDR slice (Constrained Baseline, CAVLC): I_PCM macroblocks when they fit the buffer,
//!   else Intra 16x16 DC macroblocks with no residual (one byte each): a grey picture of the frame's size.
//!
//! The guest's coded buffer is width * height / 2 + 64 KiB (DroidVMVTHost.cpp's S_FMT), a third of the raw frame, so
//! the samples are written at the largest PCM bit depths that fit (HEVC's pcm_sample_bit_depth_luma/chroma: 8+8, 4+4,
//! 3+2, 2+2, 2+1 or 1+1 bits), with an ordered dither, and the decoder shifts them back up: the picture is the frame
//! with fewer levels, not noise. The frame is padded to whole 16x16 blocks and cropped back by the conformance window.
use crate::backend::{input_format, p010_to_nv12, Backend, Coding, Polled, Session};
use crate::proto::{MEDIA_PIXFMT_HEVC, MEDIA_PIXFMT_P010};
use std::collections::VecDeque;

const STUB_RING: usize = 16;
const STUB_HEADROOM: u64 = 65536;

// ---- bits --------------------------------------------------------------------------------------------------------
/// An RBSP being written: bits into `out` (at most `cap` bytes; more is an overflow, not a reallocation).
struct Bits<'a> {
    out: &'a mut [u8],
    n: usize,
    acc: u32,
    fill: u32,
    overflow: bool,
}

impl<'a> Bits<'a> {
    fn new(out: &'a mut [u8]) -> Bits<'a> {
        Bits { out, n: 0, acc: 0, fill: 0, overflow: false }
    }

    fn byte(&mut self, v: u8) {
        if self.n < self.out.len() {
            self.out[self.n] = v;
            self.n += 1;
        } else {
            self.overflow = true;
        }
    }

    /// The low `count` bits of `value` (count <= 24).
    fn put(&mut self, value: u32, count: u32) {
        let mask = if count >= 32 { u32::MAX } else { (1u32 << count) - 1 };
        self.acc = self.acc.wrapping_shl(count) | (value & mask);
        self.fill += count;
        while self.fill >= 8 {
            self.fill -= 8;
            self.byte((self.acc >> self.fill) as u8);
        }
    }

    /// ue(v)
    fn ue(&mut self, v: u32) {
        let t = v.wrapping_add(1);
        let len = if t == 0 { 0 } else { 31 - t.leading_zeros() }; // floor(log2(v + 1))
        if len > 12 {
            self.put(0, len - 12);
            self.put(0, 12);
        } else {
            self.put(0, len);
        }
        self.put(1, 1);
        if len > 16 {
            self.put(t >> 16, len - 16);
            self.put(t, 16);
        } else {
            self.put(t, len);
        }
    }

    /// se(v) of 0
    fn se0(&mut self) {
        self.put(1, 1);
    }

    fn align_zero(&mut self) {
        if self.fill != 0 {
            self.put(0, 8 - self.fill);
        }
    }

    /// rbsp_trailing_bits
    fn trailing(&mut self) {
        self.put(1, 1);
        self.align_zero();
    }

    fn bytes(&self) -> &[u8] {
        &self.out[..self.n]
    }
}

/// One NAL unit, Annex B: the start code, the header, the payload with emulation prevention bytes, appended at `at`.
/// The new length, or None when it does not fit.
fn nal(out: &mut [u8], mut at: usize, hevc: bool, kind: u8, rbsp: &[u8]) -> Option<usize> {
    let cap = out.len();
    if at + 6 > cap {
        return None;
    }
    out[at..at + 4].copy_from_slice(&[0, 0, 0, 1]);
    at += 4;
    if hevc {
        out[at] = kind << 1; // nuh_layer_id 0, temporal_id_plus1 1
        out[at + 1] = 1;
        at += 2;
    } else {
        out[at] = 0x60 | kind; // nal_ref_idc 3
        at += 1;
    }
    let mut zeros = 0;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            if at >= cap {
                return None;
            }
            out[at] = 3;
            at += 1;
            zeros = 0;
        }
        if at >= cap {
            return None;
        }
        out[at] = b;
        at += 1;
        zeros = if b != 0 { 0 } else { zeros + 1 };
    }
    Some(at)
}

// ---- the frame's samples at a PCM bit depth ------------------------------------------------------------------------
const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// A sample as `depth` bits: the top bits of the value after an ordered dither of what the shift drops.
fn pcm_code(v: u8, depth: u32, x: u32, y: u32) -> u32 {
    if depth == 8 {
        return u32::from(v);
    }
    let shift = 8 - depth;
    let code = (u32::from(v) + ((u32::from(BAYER[(y & 3) as usize][(x & 3) as usize]) << shift) >> 4)) >> shift;
    if code >> depth != 0 {
        (1 << depth) - 1
    } else {
        code
    }
}

struct Picture<'a> {
    nv12: &'a [u8],
    w: u32,
    h: u32,
    /// padded to whole 16x16 blocks
    cw: u32,
    ch: u32,
    chroma_w: u32,
    chroma_h: u32,
}

impl Picture<'_> {
    fn luma(&self, x: u32, y: u32) -> u8 {
        let (x, y) = (x.min(self.w - 1), y.min(self.h - 1));
        self.nv12[(y * self.w + x) as usize]
    }

    fn chroma(&self, plane: u32, x: u32, y: u32) -> u8 {
        let (x, y) = (x.min(self.chroma_w - 1), y.min(self.chroma_h - 1));
        self.nv12[(self.w * self.h) as usize + (y * self.chroma_w * 2 + x * 2 + plane) as usize]
    }

    /// The 16x16 block at (bx, by) in 16s: 256 luma, 64 Cb, 64 Cr, each sample `luma`/`chroma` bits.
    fn pcm_block(&self, b: &mut Bits<'_>, bx: u32, by: u32, luma: u32, chroma: u32) {
        for y in 0..16 {
            for x in 0..16 {
                b.put(pcm_code(self.luma(bx * 16 + x, by * 16 + y), luma, x, y), luma);
            }
        }
        for plane in 0..2 {
            for y in 0..8 {
                for x in 0..8 {
                    b.put(pcm_code(self.chroma(plane, bx * 8 + x, by * 8 + y), chroma, x, y), chroma);
                }
            }
        }
    }
}

// ---- HEVC ----------------------------------------------------------------------------------------------------------
/// The arithmetic encoder of the standard (H.265 9.3.4.x; the tables are 9-46 and 9-47, H.264's 9-44 and 9-45).
const RANGE_LPS: [[u8; 4]; 64] = [
    [128, 176, 208, 240],
    [128, 167, 197, 227],
    [128, 158, 187, 216],
    [123, 150, 178, 205],
    [116, 142, 169, 195],
    [111, 135, 160, 185],
    [105, 128, 152, 175],
    [100, 122, 144, 166],
    [95, 116, 137, 158],
    [90, 110, 130, 150],
    [85, 104, 123, 142],
    [81, 99, 117, 135],
    [77, 94, 111, 128],
    [73, 89, 105, 122],
    [69, 85, 100, 116],
    [66, 80, 95, 110],
    [62, 76, 90, 104],
    [59, 72, 86, 99],
    [56, 69, 81, 94],
    [53, 65, 77, 89],
    [51, 62, 73, 85],
    [48, 59, 69, 80],
    [46, 56, 66, 76],
    [43, 53, 63, 72],
    [41, 50, 59, 69],
    [39, 48, 56, 65],
    [37, 45, 54, 62],
    [35, 43, 51, 59],
    [33, 41, 48, 56],
    [32, 39, 46, 53],
    [30, 37, 43, 50],
    [29, 35, 41, 48],
    [27, 33, 39, 45],
    [26, 31, 37, 43],
    [24, 30, 35, 41],
    [23, 28, 33, 39],
    [22, 27, 32, 37],
    [21, 26, 30, 35],
    [20, 24, 29, 33],
    [19, 23, 27, 31],
    [18, 22, 26, 30],
    [17, 21, 25, 28],
    [16, 20, 23, 27],
    [15, 19, 22, 25],
    [14, 18, 21, 24],
    [14, 17, 20, 23],
    [13, 16, 19, 22],
    [12, 15, 18, 21],
    [12, 14, 17, 20],
    [11, 14, 16, 19],
    [11, 13, 15, 18],
    [10, 12, 15, 17],
    [10, 12, 14, 16],
    [9, 11, 13, 15],
    [9, 11, 12, 14],
    [8, 10, 12, 14],
    [8, 9, 11, 13],
    [7, 9, 11, 12],
    [7, 9, 10, 12],
    [7, 8, 10, 11],
    [6, 8, 9, 11],
    [6, 7, 9, 10],
    [6, 7, 8, 9],
    [2, 2, 2, 2],
];
const NEXT_LPS: [u8; 64] = [
    0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12, 13, 13, 15, 15, 16, 16, 18, 18, 19, 19, 21, 21, 22, 22, 23, 24,
    24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32, 33, 33, 33, 34, 34, 35, 35, 35, 36, 36, 36, 37, 37, 37,
    38, 38, 63,
];

struct Cabac {
    low: u32,
    range: u32,
    outstanding: u32,
    first: bool,
    state: u8,
    mps: u32,
}

impl Cabac {
    /// part_mode's context for an I slice at QP 26 (initValue 184): state 0, most probable symbol 1.
    fn new() -> Cabac {
        Cabac { low: 0, range: 510, outstanding: 0, first: true, state: 0, mps: 1 }
    }

    /// The engine starts again (after the samples of a PCM block); the context stays.
    fn start(&mut self) {
        (self.low, self.range, self.outstanding, self.first) = (0, 510, 0, true);
    }

    fn bit(&mut self, b: &mut Bits<'_>, bit: u32) {
        if self.first {
            self.first = false;
        } else {
            b.put(bit, 1);
        }
        while self.outstanding > 0 {
            b.put(u32::from(bit == 0), 1);
            self.outstanding -= 1;
        }
    }

    fn renorm(&mut self, b: &mut Bits<'_>) {
        while self.range < 256 {
            if self.low < 256 {
                self.bit(b, 0);
            } else if self.low >= 512 {
                self.low -= 512;
                self.bit(b, 1);
            } else {
                self.low -= 256;
                self.outstanding += 1;
            }
            self.range <<= 1;
            self.low <<= 1;
        }
    }

    /// The one context: part_mode's.
    fn decision(&mut self, b: &mut Bits<'_>, bin: u32) {
        let lps = u32::from(RANGE_LPS[self.state as usize][((self.range >> 6) & 3) as usize]);
        self.range -= lps;
        if bin != self.mps {
            self.low += self.range;
            self.range = lps;
            if self.state == 0 {
                self.mps = 1 - self.mps;
            }
            self.state = NEXT_LPS[self.state as usize];
        } else if self.state < 62 {
            self.state += 1;
        }
        self.renorm(b);
    }

    fn terminate(&mut self, b: &mut Bits<'_>, bin: bool) {
        self.range -= 2;
        if !bin {
            self.renorm(b);
            return;
        }
        self.low += self.range;
        self.range = 2; // the flush
        self.renorm(b);
        self.bit(b, (self.low >> 9) & 1);
        b.put(((self.low >> 7) & 3) | 1, 2);
    }
}

fn hevc_profile_tier_level(b: &mut Bits<'_>, samples: u32) {
    b.put(1, 8); // profile space 0, main tier, Main
    b.put(0x6000, 16); // compatible with Main and Main 10
    b.put(0, 16);
    b.put(0x9, 4); // progressive, frame only
    b.put(0, 22); // 43 reserved bits and inbld
    b.put(0, 22);
    // level 3.1, 4.1, 5.1, 6.1
    let level = match samples {
        0..=552_960 => 93,
        552_961..=2_228_224 => 123,
        2_228_225..=8_912_896 => 153,
        _ => 183,
    };
    b.put(level, 8);
}

/// The access unit of one frame at the PCM depths `luma` and `chroma` into `out`: its size, or None when it does not
/// fit. `work` holds each RBSP while it is written.
fn hevc_frame(p: &Picture<'_>, luma: u32, chroma: u32, out: &mut [u8], work: &mut [u8]) -> Option<usize> {
    let mut at = 0;
    // The video parameter set.
    let mut b = Bits::new(work);
    b.put(0, 4);
    b.put(3, 2);
    b.put(0, 6);
    b.put(0, 3);
    b.put(1, 1);
    b.put(0xffff, 16);
    hevc_profile_tier_level(&mut b, p.cw * p.ch);
    b.put(1, 1); // one picture in the buffer, no reordering
    b.ue(0);
    b.ue(0);
    b.ue(0);
    b.put(0, 6); // layer id 0, one layer set, no timing, no extension
    b.ue(0);
    b.put(0, 1);
    b.put(0, 1);
    b.trailing();
    at = nal(out, at, true, 32, b.bytes())?;
    // The sequence parameter set.
    let mut b = Bits::new(work);
    b.put(0, 4);
    b.put(0, 3);
    b.put(1, 1);
    hevc_profile_tier_level(&mut b, p.cw * p.ch);
    b.ue(0); // id 0, 4:2:0
    b.ue(1);
    b.ue(p.cw);
    b.ue(p.ch);
    let crop = p.cw != p.w || p.ch != p.h;
    b.put(u32::from(crop), 1);
    if crop {
        b.ue(0);
        b.ue((p.cw - p.w) / 2);
        b.ue(0);
        b.ue((p.ch - p.h) / 2);
    }
    b.ue(0); // 8 bits, luma and chroma
    b.ue(0);
    b.ue(0); // picture order count: 4 bits
    b.put(1, 1);
    b.ue(0);
    b.ue(0);
    b.ue(0);
    b.ue(1); // coding blocks: 16x16 only
    b.ue(0);
    b.ue(0); // transform blocks 4x4 to 16x16 (unused)
    b.ue(2);
    b.ue(0);
    b.ue(0);
    b.put(0, 1); // no scaling lists, AMP, SAO
    b.put(0, 1);
    b.put(0, 1);
    b.put(1, 1); // PCM
    b.put(luma - 1, 4);
    b.put(chroma - 1, 4);
    b.ue(1); // PCM coding blocks: 16x16
    b.ue(0);
    b.put(1, 1); // no loop filter on PCM samples
    b.ue(0); // no short-term reference picture sets
    b.put(0, 1); // no long-term pictures, temporal MVP, strong smoothing
    b.put(0, 1);
    b.put(0, 1);
    b.put(0, 1); // no VUI, no extension
    b.put(0, 1);
    b.trailing();
    at = nal(out, at, true, 33, b.bytes())?;
    // The picture parameter set.
    let mut b = Bits::new(work);
    b.ue(0);
    b.ue(0);
    b.put(0, 1);
    b.put(0, 1);
    b.put(0, 3);
    b.put(0, 1);
    b.put(0, 1);
    b.ue(0);
    b.ue(0);
    b.se0(); // init_qp 26
    b.put(0, 1); // no constrained intra, transform skip, cu_qp_delta
    b.put(0, 1);
    b.put(0, 1);
    b.se0();
    b.se0();
    b.put(0, 1); // no slice chroma offsets, weighted prediction, bypass
    b.put(0, 1);
    b.put(0, 1);
    b.put(0, 1);
    b.put(0, 1); // no tiles, wavefront, filter across slices
    b.put(0, 1);
    b.put(0, 1);
    b.put(1, 1); // deblocking: control present, no override, off
    b.put(0, 1);
    b.put(1, 1);
    b.put(0, 1);
    b.put(0, 1);
    b.ue(0);
    b.put(0, 1);
    b.put(0, 1);
    b.trailing();
    at = nal(out, at, true, 34, b.bytes())?;
    // The slice: IDR, intra, every coding tree unit one I_PCM coding unit.
    let mut b = Bits::new(work);
    b.put(1, 1); // first segment, PPS 0, I slice, QP 26
    b.put(0, 1);
    b.ue(0);
    b.ue(2);
    b.se0();
    b.trailing(); // byte_alignment(): the same bits
    let mut c = Cabac::new();
    let (columns, rows) = (p.cw / 16, p.ch / 16);
    c.start();
    for by in 0..rows {
        for bx in 0..columns {
            c.decision(&mut b, 1); // part_mode: 2Nx2N
            c.terminate(&mut b, true); // pcm_flag
            b.align_zero(); // pcm_alignment_zero_bit
            p.pcm_block(&mut b, bx, by, luma, chroma);
            c.start(); // the engine starts again after the samples
            c.terminate(&mut b, by == rows - 1 && bx == columns - 1); // end_of_slice_segment_flag
        }
    }
    b.align_zero(); // the flush's last bit was the stop bit
    if b.overflow {
        return None;
    }
    nal(out, at, true, 19, b.bytes()) // IDR_W_RADL
}

// ---- H.264 ---------------------------------------------------------------------------------------------------------
fn h264_frame(p: &Picture<'_>, pcm: bool, out: &mut [u8], work: &mut [u8]) -> Option<usize> {
    let (columns, rows) = (p.cw / 16, p.ch / 16);
    let mut b = Bits::new(work);
    b.put(66, 8); // Constrained Baseline
    b.put(0xc0, 8);
    let blocks = columns * rows;
    b.put(
        if blocks <= 3600 {
            31
        } else if blocks <= 8192 {
            41
        } else {
            51
        },
        8,
    ); // level 3.1, 4.1, 5.1
    b.ue(0); // frame_num 4 bits, POC type 2, 1 reference
    b.ue(0);
    b.ue(2);
    b.ue(1);
    b.put(0, 1);
    b.ue(columns - 1);
    b.ue(rows - 1);
    b.put(1, 1); // frames only, direct 8x8 inference
    b.put(1, 1);
    let crop = p.cw != p.w || p.ch != p.h;
    b.put(u32::from(crop), 1);
    if crop {
        b.ue(0);
        b.ue((p.cw - p.w) / 2);
        b.ue(0);
        b.ue((p.ch - p.h) / 2);
    }
    b.put(0, 1); // no VUI
    b.trailing();
    let mut at = nal(out, 0, false, 7, b.bytes())?;
    let mut b = Bits::new(work);
    b.ue(0); // CAVLC, one slice group
    b.ue(0);
    b.put(0, 1);
    b.put(0, 1);
    b.ue(0);
    b.ue(0);
    b.ue(0);
    b.put(0, 1);
    b.put(0, 2);
    b.se0(); // QP 26
    b.se0();
    b.se0();
    b.put(1, 1); // deblocking control present
    b.put(0, 1);
    b.put(0, 1);
    b.trailing();
    at = nal(out, at, false, 8, b.bytes())?;
    let mut b = Bits::new(work);
    b.ue(0); // first macroblock 0, I slice, frame 0, IDR 0
    b.ue(7);
    b.ue(0);
    b.put(0, 4);
    b.ue(0);
    b.put(0, 1); // marking: keep prior output, short term
    b.put(0, 1);
    b.se0(); // QP 26, no deblocking
    b.ue(1);
    for by in 0..rows {
        for bx in 0..columns {
            if !pcm {
                b.put(0x27, 8); // Intra 16x16 DC, no residual: ue(3) 1 1 1
                continue;
            }
            b.ue(25); // I_PCM
            b.align_zero();
            p.pcm_block(&mut b, bx, by, 8, 8);
        }
    }
    b.trailing();
    if b.overflow {
        return None;
    }
    nal(out, at, false, 5, b.bytes())
}

// ---- the backend ---------------------------------------------------------------------------------------------------
/// The PCM depths tried, best first (luma, chroma): bits a 16x16 block = 256 * luma + 128 * chroma.
pub const DEPTHS: [(u32, u32); 6] = [(8, 8), (4, 4), (3, 2), (2, 2), (2, 1), (1, 1)];

struct Frame {
    nv12: Vec<u8>,
    pts_us: i64,
}

/// A stand-in session (`br_enc_session` of backend_stub.c).
struct StubSession {
    codec: u32,
    /// MEDIA_PIXFMT_NV12, or MEDIA_PIXFMT_P010: coded 8-bit from the samples' high bytes (the stand-in has no Main10)
    input: u32,
    width: u32,
    height: u32,
    ring: VecDeque<Frame>,
    draining: bool,
    eos_done: bool,
    /// the RBSP being written: one buffer for the session's life, not one a frame
    work: Vec<u8>,
}

impl StubSession {
    /// One frame as an access unit of at most `out.len()` bytes (and at most what the guest's coded buffers hold).
    /// Its size, or ENOSPC.
    fn encode(&mut self, nv12: &[u8], out: &mut [u8]) -> Result<usize, i32> {
        let (w, h) = (self.width, self.height);
        let p = Picture {
            nv12,
            w,
            h,
            cw: (w + 15) & !15,
            ch: (h + 15) & !15,
            chroma_w: w.div_ceil(2),
            chroma_h: h.div_ceil(2),
        };
        let budget = u64::from(w) * u64::from(h) / 2 + STUB_HEADROOM;
        let cap = (out.len() as u64).min(budget) as usize;
        let out = &mut out[..cap];
        // a raw frame and two bytes a block
        let work_cap = (p.cw * p.ch * 3 / 2 + p.cw * p.ch / 64 + 4096) as usize;
        if self.work.len() < work_cap {
            self.work.resize(work_cap, 0);
        }
        let work = &mut self.work[..work_cap];
        let size = if self.codec == MEDIA_PIXFMT_HEVC {
            DEPTHS.iter().find_map(|&(luma, chroma)| hevc_frame(&p, luma, chroma, out, work))
        } else {
            h264_frame(&p, true, out, work).or_else(|| h264_frame(&p, false, out, work))
        };
        size.ok_or(libc::ENOSPC)
    }
}

impl Session for StubSession {
    fn feed(&mut self, frame: &[u8], pts_us: i64, _force_key: bool) -> Result<(), i32> {
        if self.ring.len() >= STUB_RING {
            return Err(libc::ENOSPC);
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let need = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
        let nv12 = if self.input == MEDIA_PIXFMT_P010 {
            if frame.len() < 2 * need {
                return Err(libc::EINVAL);
            }
            let mut nv12 = vec![0u8; need];
            p010_to_nv12(&frame[..2 * need], &mut nv12);
            nv12
        } else {
            if frame.len() < need {
                return Err(libc::EINVAL);
            }
            frame[..need].to_vec()
        };
        self.ring.push_back(Frame { nv12, pts_us });
        Ok(())
    }

    fn coding(&self) -> Coding {
        if self.input == MEDIA_PIXFMT_P010 {
            Coding::TenAsEight
        } else {
            Coding::Eight
        }
    }

    /// Every drain ends with its own end of stream (the guest starts the encoder again after one and may drain
    /// again).
    fn drain(&mut self) -> Result<(), i32> {
        (self.draining, self.eos_done) = (true, false);
        Ok(())
    }

    /// Its units are coded in the poll, from what `feed` and `drain` (a guest's command, which wakes the worker)
    /// left: nothing comes between two commands, so the worker needs no wake-up of its own and no 1 ms look.
    fn wakes(&mut self, _wake: crate::backend::Wakeup) -> bool {
        true
    }

    fn poll(&mut self, buf: &mut [u8]) -> Result<Polled, i32> {
        if let Some(f) = self.ring.pop_front() {
            // every frame is an IDR
            let bytes = self.encode(&f.nv12, buf)?;
            return Ok(Polled { bytes, key: true, pts_us: f.pts_us, eos: false });
        }
        if self.draining && !self.eos_done {
            self.eos_done = true;
            return Ok(Polled { eos: true, ..Polled::default() });
        }
        Ok(Polled::default())
    }
}

/// The stand-in backend ("stand-in").
#[derive(Clone, Copy, Debug, Default)]
pub struct StubBackend;

impl Backend for StubBackend {
    fn name(&self) -> &'static str {
        "stand-in"
    }

    fn open(&self, codec: u32, input: u32, width: u32, height: u32) -> Option<Box<dyn Session>> {
        // The test of a helper whose open never comes back (tests/hung_helper.rs): MediaCodec's open hung so in the
        // phone's helper on 2026-11-23. Only the helper's environment sets it.
        if std::env::var_os("ONEIOS_ENCODER_STUB_HANG_OPEN").is_some() {
            loop {
                // SAFETY: pause takes no arguments; it returns only when a signal is caught.
                unsafe { libc::pause() };
            }
        }
        if width == 0 || height == 0 || width > 16384 || height > 16384 || !input_format(input) {
            return None;
        }
        Some(Box::new(StubSession {
            codec,
            input,
            width,
            height,
            ring: VecDeque::with_capacity(STUB_RING),
            draining: false,
            eos_done: false,
            work: Vec::new(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_golomb() {
        let mut out = [0u8; 8];
        let mut b = Bits::new(&mut out);
        b.ue(0); // 1
        b.ue(1); // 010
        b.ue(2); // 011
        b.ue(3); // 00100
        b.trailing(); // 1 then zeros: 1 010 011 0 | 0100 1 000
        assert_eq!(b.bytes(), &[0b1010_0110, 0b0100_1000]);
    }

    #[test]
    fn emulation_prevention() {
        let mut out = [0u8; 16];
        let n = nal(&mut out, 0, false, 5, &[0, 0, 1, 0, 0, 0]).unwrap();
        assert_eq!(&out[..n], &[0, 0, 0, 1, 0x65, 0, 0, 3, 1, 0, 0, 3, 0]);
        assert_eq!(nal(&mut out[..5], 0, true, 32, &[]), None);
    }
}
