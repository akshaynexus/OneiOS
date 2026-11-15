// SPDX-License-Identifier: MIT
// The stand-in encoder (encoder.h): for a host with no MediaCodec (the PC; xnuhost's encoder=sim, and encoder=auto with
// the bridges off) and for the host test. It compresses nothing, and what it writes is a real stream that any decoder
// decodes to the frame it was given:
//   HEVC    VPS, SPS, PPS and one IDR slice (Main profile, 16x16 coding tree blocks) whose every coding unit is I_PCM:
//           the frame's own samples, raw. The only arithmetic-coded bins are part_mode and the two terminating bins
//           (pcm_flag, end_of_slice_segment_flag), so the CABAC encoder below is the standard's, whole, in 40 lines.
//   H.264   SPS, PPS and one IDR slice (Constrained Baseline, CAVLC): I_PCM macroblocks when they fit the buffer, else
//           Intra 16x16 DC macroblocks with no residual (one byte each): a grey picture of the frame's size.
// The guest's coded buffer is width * height / 2 + 64 KiB (DroidVMVTHost.cpp's S_FMT), a third of the raw frame, so
// the samples are written at the largest PCM bit depths that fit (HEVC's pcm_sample_bit_depth_luma/chroma: 8+8, 4+4,
// 3+2, 2+2, 2+1 or 1+1 bits), with an ordered dither, and the decoder shifts them back up: the picture is the frame
// with fewer levels, not noise. The frame is padded to whole 16x16 blocks and cropped back by the conformance window.
#include "encoder.h"
#include <errno.h>
#include <stdlib.h>
#include <string.h>

enum { STUB_RING = 16, STUB_HEADROOM = 65536 };

struct frame {
  uint8_t *nv12;
  uint32_t len;
  int64_t pts_us;
};

struct br_enc_session {
  uint32_t codec, width, height;
  struct frame ring[STUB_RING];
  unsigned head, tail;
  bool draining, eos_done;
};

// ---- bits -----------------------------------------------------------------------------------------------------

struct bits { uint8_t *p; uint32_t cap, n; uint32_t acc; unsigned fill; bool overflow; };

static void put_byte(struct bits *b, uint8_t v) { if (b->n < b->cap) b->p[b->n++] = v; else b->overflow = true; }
static void put(struct bits *b, uint32_t value, unsigned count) {           // count <= 24
  b->acc = (b->acc << count) | (value & ((1u << count) - 1));
  b->fill += count;
  while (b->fill >= 8) { b->fill -= 8; put_byte(b, (uint8_t)(b->acc >> b->fill)); }
}
static void put_ue(struct bits *b, uint32_t v) {
  unsigned len = 0;
  for (uint32_t t = v + 1; t > 1; t >>= 1) len++;
  if (len > 12) { put(b, 0, len - 12); put(b, 0, 12); } else put(b, 0, len);
  put(b, 1, 1);
  if (len > 16) { put(b, (v + 1) >> 16, len - 16); put(b, v + 1, 16); } else put(b, v + 1, len);
}
static void put_se0(struct bits *b) { put(b, 1, 1); }                       // se(v) of 0
static void align_zero(struct bits *b) { if (b->fill) put(b, 0, 8 - b->fill); }
static void trailing(struct bits *b) { put(b, 1, 1); align_zero(b); }       // rbsp_trailing_bits

// One NAL unit, Annex B: the start code, the header, the payload with emulation prevention bytes. Returns the new
// length, or UINT32_MAX when it does not fit.
static uint32_t nal(uint8_t *out, uint32_t at, uint32_t cap, bool hevc, unsigned type, const uint8_t *rbsp, uint32_t n) {
  if (at + 6 > cap) return UINT32_MAX;
  out[at++] = 0; out[at++] = 0; out[at++] = 0; out[at++] = 1;
  if (hevc) { out[at++] = (uint8_t)(type << 1); out[at++] = 1; }           // nuh_layer_id 0, temporal_id_plus1 1
  else out[at++] = (uint8_t)(0x60 | type);                                  // nal_ref_idc 3
  unsigned zeros = 0;
  for (uint32_t i = 0; i < n; i++) {
    if (zeros >= 2 && rbsp[i] <= 3) { if (at >= cap) return UINT32_MAX; out[at++] = 3; zeros = 0; }
    if (at >= cap) return UINT32_MAX;
    out[at++] = rbsp[i];
    zeros = rbsp[i] ? 0 : zeros + 1;
  }
  return at;
}

// ---- the frame's samples at a PCM bit depth ----------------------------------------------------------------------

static const uint8_t bayer[4][4] = { { 0, 8, 2, 10 }, { 12, 4, 14, 6 }, { 3, 11, 1, 9 }, { 15, 7, 13, 5 } };

// A sample as `depth` bits: the top bits of the value after an ordered dither of what the shift drops.
static inline uint32_t pcm_code(uint8_t v, unsigned depth, unsigned x, unsigned y) {
  if (depth == 8) return v;
  unsigned shift = 8 - depth;
  uint32_t code = (v + ((bayer[y & 3][x & 3] << shift) >> 4)) >> shift;
  return code >> depth ? (1u << depth) - 1 : code;
}

struct picture { const uint8_t *nv12; uint32_t w, h, cw, ch, chroma_w, chroma_h; };

static inline uint8_t luma_at(const struct picture *p, uint32_t x, uint32_t y) {
  if (x >= p->w) x = p->w - 1;
  if (y >= p->h) y = p->h - 1;
  return p->nv12[(size_t)y * p->w + x];
}
static inline uint8_t chroma_at(const struct picture *p, unsigned plane, uint32_t x, uint32_t y) {
  if (x >= p->chroma_w) x = p->chroma_w - 1;
  if (y >= p->chroma_h) y = p->chroma_h - 1;
  return p->nv12[(size_t)p->w * p->h + (size_t)y * p->chroma_w * 2 + x * 2 + plane];
}
// The 16x16 block at (bx, by) in 16s: 256 luma, 64 Cb, 64 Cr, each sample `luma`/`chroma` bits.
static void pcm_block(struct bits *b, const struct picture *p, uint32_t bx, uint32_t by, unsigned luma, unsigned chroma) {
  for (unsigned y = 0; y < 16; y++)
    for (unsigned x = 0; x < 16; x++) put(b, pcm_code(luma_at(p, bx * 16 + x, by * 16 + y), luma, x, y), luma);
  for (unsigned plane = 0; plane < 2; plane++)
    for (unsigned y = 0; y < 8; y++)
      for (unsigned x = 0; x < 8; x++) put(b, pcm_code(chroma_at(p, plane, bx * 8 + x, by * 8 + y), chroma, x, y), chroma);
}

// ---- HEVC ---------------------------------------------------------------------------------------------------------

// The arithmetic encoder of the standard (H.265 9.3.4.x; the tables are 9-46 and 9-47, H.264's 9-44 and 9-45).
static const uint8_t range_lps[64][4] = {
  { 128, 176, 208, 240 }, { 128, 167, 197, 227 }, { 128, 158, 187, 216 }, { 123, 150, 178, 205 }, { 116, 142, 169, 195 },
  { 111, 135, 160, 185 }, { 105, 128, 152, 175 }, { 100, 122, 144, 166 }, { 95, 116, 137, 158 }, { 90, 110, 130, 150 },
  { 85, 104, 123, 142 }, { 81, 99, 117, 135 }, { 77, 94, 111, 128 }, { 73, 89, 105, 122 }, { 69, 85, 100, 116 },
  { 66, 80, 95, 110 }, { 62, 76, 90, 104 }, { 59, 72, 86, 99 }, { 56, 69, 81, 94 }, { 53, 65, 77, 89 },
  { 51, 62, 73, 85 }, { 48, 59, 69, 80 }, { 46, 56, 66, 76 }, { 43, 53, 63, 72 }, { 41, 50, 59, 69 },
  { 39, 48, 56, 65 }, { 37, 45, 54, 62 }, { 35, 43, 51, 59 }, { 33, 41, 48, 56 }, { 32, 39, 46, 53 },
  { 30, 37, 43, 50 }, { 29, 35, 41, 48 }, { 27, 33, 39, 45 }, { 26, 31, 37, 43 }, { 24, 30, 35, 41 },
  { 23, 28, 33, 39 }, { 22, 27, 32, 37 }, { 21, 26, 30, 35 }, { 20, 24, 29, 33 }, { 19, 23, 27, 31 },
  { 18, 22, 26, 30 }, { 17, 21, 25, 28 }, { 16, 20, 23, 27 }, { 15, 19, 22, 25 }, { 14, 18, 21, 24 },
  { 14, 17, 20, 23 }, { 13, 16, 19, 22 }, { 12, 15, 18, 21 }, { 12, 14, 17, 20 }, { 11, 14, 16, 19 },
  { 11, 13, 15, 18 }, { 10, 12, 15, 17 }, { 10, 12, 14, 16 }, { 9, 11, 13, 15 }, { 9, 11, 12, 14 },
  { 8, 10, 12, 14 }, { 8, 9, 11, 13 }, { 7, 9, 11, 12 }, { 7, 9, 10, 12 }, { 7, 8, 10, 11 },
  { 6, 8, 9, 11 }, { 6, 7, 9, 10 }, { 6, 7, 8, 9 }, { 2, 2, 2, 2 },
};
static const uint8_t next_lps[64] = {
  0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12, 13, 13, 15, 15, 16, 16, 18, 18, 19, 19, 21, 21, 22, 22, 23, 24,
  24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32, 33, 33, 33, 34, 34, 35, 35, 35, 36, 36, 36, 37, 37, 37, 38, 38, 63,
};

struct cabac { struct bits *out; uint32_t low, range, outstanding; bool first; uint8_t state, mps; };

static void cabac_start(struct cabac *c) { c->low = 0; c->range = 510; c->outstanding = 0; c->first = true; }
static void cabac_bit(struct cabac *c, unsigned b) {
  if (c->first) c->first = false; else put(c->out, b, 1);
  for (; c->outstanding; c->outstanding--) put(c->out, !b, 1);
}
static void cabac_renorm(struct cabac *c) {
  while (c->range < 256) {
    if (c->low < 256) cabac_bit(c, 0);
    else if (c->low >= 512) { c->low -= 512; cabac_bit(c, 1); }
    else { c->low -= 256; c->outstanding++; }
    c->range <<= 1;
    c->low <<= 1;
  }
}
static void cabac_decision(struct cabac *c, unsigned bin) {                 // the one context: part_mode's
  uint32_t lps = range_lps[c->state][(c->range >> 6) & 3];
  c->range -= lps;
  if (bin != c->mps) {
    c->low += c->range;
    c->range = lps;
    if (!c->state) c->mps = !c->mps;
    c->state = next_lps[c->state];
  } else if (c->state < 62) c->state++;
  cabac_renorm(c);
}
static void cabac_terminate(struct cabac *c, unsigned bin) {
  c->range -= 2;
  if (!bin) { cabac_renorm(c); return; }
  c->low += c->range;
  c->range = 2;                                                             // the flush
  cabac_renorm(c);
  cabac_bit(c, (c->low >> 9) & 1);
  put(c->out, ((c->low >> 7) & 3) | 1, 2);
}

static void hevc_profile_tier_level(struct bits *b, uint32_t samples) {
  put(b, 1, 8);                                                             // profile space 0, main tier, Main
  put(b, 0x6000, 16); put(b, 0, 16);                                        // compatible with Main and Main 10
  put(b, 0x9, 4);                                                           // progressive, frame only
  put(b, 0, 22); put(b, 0, 22);                                             // 43 reserved bits and inbld
  put(b, samples <= 552960 ? 93 : samples <= 2228224 ? 123 : samples <= 8912896 ? 153 : 183, 8);   // level 3.1, 4.1, 5.1, 6.1
}

// The access unit of one frame at the PCM depths `luma` and `chroma`: its size, or UINT32_MAX when it does not fit.
static uint32_t hevc_frame(const struct picture *p, unsigned luma, unsigned chroma, uint8_t *out, uint32_t cap, uint8_t *work, uint32_t work_cap) {
  struct bits b = { .p = work, .cap = work_cap };
  uint32_t at = 0;
  // The video parameter set.
  put(&b, 0, 4); put(&b, 3, 2); put(&b, 0, 6); put(&b, 0, 3); put(&b, 1, 1); put(&b, 0xffff, 16);
  hevc_profile_tier_level(&b, p->cw * p->ch);
  put(&b, 1, 1); put_ue(&b, 0); put_ue(&b, 0); put_ue(&b, 0);              // one picture in the buffer, no reordering
  put(&b, 0, 6); put_ue(&b, 0); put(&b, 0, 1); put(&b, 0, 1);              // layer id 0, one layer set, no timing, no extension
  trailing(&b);
  if ((at = nal(out, at, cap, true, 32, work, b.n)) == UINT32_MAX) return at;
  // The sequence parameter set.
  b = (struct bits){ .p = work, .cap = work_cap };
  put(&b, 0, 4); put(&b, 0, 3); put(&b, 1, 1);
  hevc_profile_tier_level(&b, p->cw * p->ch);
  put_ue(&b, 0); put_ue(&b, 1);                                             // id 0, 4:2:0
  put_ue(&b, p->cw); put_ue(&b, p->ch);
  bool crop = p->cw != p->w || p->ch != p->h;
  put(&b, crop, 1);
  if (crop) { put_ue(&b, 0); put_ue(&b, (p->cw - p->w) / 2); put_ue(&b, 0); put_ue(&b, (p->ch - p->h) / 2); }
  put_ue(&b, 0); put_ue(&b, 0);                                             // 8 bits, luma and chroma
  put_ue(&b, 0);                                                            // picture order count: 4 bits
  put(&b, 1, 1); put_ue(&b, 0); put_ue(&b, 0); put_ue(&b, 0);
  put_ue(&b, 1); put_ue(&b, 0);                                             // coding blocks: 16x16 only
  put_ue(&b, 0); put_ue(&b, 2);                                             // transform blocks 4x4 to 16x16 (unused)
  put_ue(&b, 0); put_ue(&b, 0);
  put(&b, 0, 1); put(&b, 0, 1); put(&b, 0, 1);                              // no scaling lists, AMP, SAO
  put(&b, 1, 1);                                                            // PCM
  put(&b, luma - 1, 4); put(&b, chroma - 1, 4);
  put_ue(&b, 1); put_ue(&b, 0);                                             // PCM coding blocks: 16x16
  put(&b, 1, 1);                                                            // no loop filter on PCM samples
  put_ue(&b, 0);                                                            // no short-term reference picture sets
  put(&b, 0, 1); put(&b, 0, 1); put(&b, 0, 1);                              // no long-term pictures, temporal MVP, strong smoothing
  put(&b, 0, 1); put(&b, 0, 1);                                             // no VUI, no extension
  trailing(&b);
  if ((at = nal(out, at, cap, true, 33, work, b.n)) == UINT32_MAX) return at;
  // The picture parameter set.
  b = (struct bits){ .p = work, .cap = work_cap };
  put_ue(&b, 0); put_ue(&b, 0);
  put(&b, 0, 1); put(&b, 0, 1); put(&b, 0, 3); put(&b, 0, 1); put(&b, 0, 1);
  put_ue(&b, 0); put_ue(&b, 0); put_se0(&b);                                // init_qp 26
  put(&b, 0, 1); put(&b, 0, 1); put(&b, 0, 1);                              // no constrained intra, transform skip, cu_qp_delta
  put_se0(&b); put_se0(&b);
  put(&b, 0, 1); put(&b, 0, 1); put(&b, 0, 1); put(&b, 0, 1);               // no slice chroma offsets, weighted prediction, bypass
  put(&b, 0, 1); put(&b, 0, 1); put(&b, 0, 1);                              // no tiles, wavefront, filter across slices
  put(&b, 1, 1); put(&b, 0, 1); put(&b, 1, 1);                              // deblocking: control present, no override, off
  put(&b, 0, 1); put(&b, 0, 1); put_ue(&b, 0); put(&b, 0, 1); put(&b, 0, 1);
  trailing(&b);
  if ((at = nal(out, at, cap, true, 34, work, b.n)) == UINT32_MAX) return at;
  // The slice: IDR, intra, every coding tree unit one I_PCM coding unit.
  b = (struct bits){ .p = work, .cap = work_cap };
  put(&b, 1, 1); put(&b, 0, 1); put_ue(&b, 0); put_ue(&b, 2); put_se0(&b);  // first segment, PPS 0, I slice, QP 26
  trailing(&b);                                                             // byte_alignment(): the same bits
  // part_mode's context for an I slice at QP 26 (initValue 184): state 0, most probable symbol 1.
  struct cabac c = { .out = &b, .state = 0, .mps = 1 };
  uint32_t columns = p->cw / 16, rows = p->ch / 16;
  cabac_start(&c);
  for (uint32_t by = 0; by < rows; by++)
    for (uint32_t bx = 0; bx < columns; bx++) {
      cabac_decision(&c, 1);                                                // part_mode: 2Nx2N
      cabac_terminate(&c, 1);                                               // pcm_flag
      align_zero(&b);                                                       // pcm_alignment_zero_bit
      pcm_block(&b, p, bx, by, luma, chroma);
      cabac_start(&c);                                                      // the engine starts again after the samples
      cabac_terminate(&c, by == rows - 1 && bx == columns - 1);             // end_of_slice_segment_flag
    }
  align_zero(&b);                                                           // the flush's last bit was the stop bit
  if (b.overflow) return UINT32_MAX;
  return nal(out, at, cap, true, 19, work, b.n);                            // IDR_W_RADL
}

// ---- H.264 --------------------------------------------------------------------------------------------------------

static uint32_t h264_frame(const struct picture *p, bool pcm, uint8_t *out, uint32_t cap, uint8_t *work, uint32_t work_cap) {
  struct bits b = { .p = work, .cap = work_cap };
  uint32_t at = 0, columns = p->cw / 16, rows = p->ch / 16;
  put(&b, 66, 8); put(&b, 0xc0, 8);                                         // Constrained Baseline
  put(&b, columns * rows <= 3600 ? 31 : columns * rows <= 8192 ? 41 : 51, 8);   // level 3.1, 4.1, 5.1
  put_ue(&b, 0); put_ue(&b, 0); put_ue(&b, 2); put_ue(&b, 1); put(&b, 0, 1); // frame_num 4 bits, POC type 2, 1 reference
  put_ue(&b, columns - 1); put_ue(&b, rows - 1);
  put(&b, 1, 1); put(&b, 1, 1);                                             // frames only, direct 8x8 inference
  bool crop = p->cw != p->w || p->ch != p->h;
  put(&b, crop, 1);
  if (crop) { put_ue(&b, 0); put_ue(&b, (p->cw - p->w) / 2); put_ue(&b, 0); put_ue(&b, (p->ch - p->h) / 2); }
  put(&b, 0, 1);                                                            // no VUI
  trailing(&b);
  if ((at = nal(out, at, cap, false, 7, work, b.n)) == UINT32_MAX) return at;
  b = (struct bits){ .p = work, .cap = work_cap };
  put_ue(&b, 0); put_ue(&b, 0); put(&b, 0, 1); put(&b, 0, 1); put_ue(&b, 0); // CAVLC, one slice group
  put_ue(&b, 0); put_ue(&b, 0); put(&b, 0, 1); put(&b, 0, 2);
  put_se0(&b); put_se0(&b); put_se0(&b);                                    // QP 26
  put(&b, 1, 1); put(&b, 0, 1); put(&b, 0, 1);                              // deblocking control present
  trailing(&b);
  if ((at = nal(out, at, cap, false, 8, work, b.n)) == UINT32_MAX) return at;
  b = (struct bits){ .p = work, .cap = work_cap };
  put_ue(&b, 0); put_ue(&b, 7); put_ue(&b, 0); put(&b, 0, 4); put_ue(&b, 0); // first macroblock 0, I slice, frame 0, IDR 0
  put(&b, 0, 1); put(&b, 0, 1);                                             // marking: keep prior output, short term
  put_se0(&b); put_ue(&b, 1);                                               // QP 26, no deblocking
  for (uint32_t by = 0; by < rows; by++)
    for (uint32_t bx = 0; bx < columns; bx++) {
      if (!pcm) { put(&b, 0x27, 8); continue; }                             // Intra 16x16 DC, no residual: ue(3) 1 1 1
      put_ue(&b, 25);                                                       // I_PCM
      align_zero(&b);
      pcm_block(&b, p, bx, by, 8, 8);
    }
  trailing(&b);
  if (b.overflow) return UINT32_MAX;
  return nal(out, at, cap, false, 5, work, b.n);
}

// ---- the backend --------------------------------------------------------------------------------------------------

// The PCM depths tried, best first (luma, chroma): bits a 16x16 block = 256 * luma + 128 * chroma.
static const uint8_t depths[][2] = { { 8, 8 }, { 4, 4 }, { 3, 2 }, { 2, 2 }, { 2, 1 }, { 1, 1 } };

// One frame as an access unit of at most `cap` bytes (and at most what the guest's coded buffers hold). Its size, or
// -errno.
static int stub_encode(const br_enc_session *s, const uint8_t *nv12, uint8_t *out, uint32_t cap) {
  struct picture p = { .nv12 = nv12, .w = s->width, .h = s->height, .cw = (s->width + 15) & ~15u, .ch = (s->height + 15) & ~15u,
                       .chroma_w = (s->width + 1) / 2, .chroma_h = (s->height + 1) / 2 };
  uint64_t budget = (uint64_t)s->width * s->height / 2 + STUB_HEADROOM;
  if (budget < cap) cap = (uint32_t)budget;
  uint32_t work_cap = p.cw * p.ch * 3 / 2 + p.cw * p.ch / 64 + 4096;       // a raw frame and two bytes a block
  uint8_t *work = malloc(work_cap);
  if (!work) return -ENOMEM;
  uint32_t size = UINT32_MAX;
  if (s->codec == MEDIA_PIXFMT_HEVC) {
    for (unsigned i = 0; i < sizeof depths / sizeof depths[0] && size == UINT32_MAX; i++)
      size = hevc_frame(&p, depths[i][0], depths[i][1], out, cap, work, work_cap);
  } else {
    size = h264_frame(&p, true, out, cap, work, work_cap);
    if (size == UINT32_MAX) size = h264_frame(&p, false, out, cap, work, work_cap);
  }
  free(work);
  return size == UINT32_MAX ? -ENOSPC : (int)size;
}

static br_enc_session *stub_open(uint32_t codec, uint32_t width, uint32_t height) {
  if (!width || !height || width > 16384 || height > 16384) return NULL;
  br_enc_session *s = calloc(1, sizeof *s);
  if (!s) return NULL;
  s->codec = codec;
  s->width = width;
  s->height = height;
  return s;
}

static int stub_feed(br_enc_session *s, const uint8_t *nv12, uint32_t len, int64_t pts_us, int force_key) {
  (void)force_key;
  if (s->head - s->tail >= STUB_RING) return -ENOSPC;
  uint32_t need = s->width * s->height + 2 * ((s->width + 1) / 2) * ((s->height + 1) / 2);
  if (len < need) return -EINVAL;
  uint8_t *copy = malloc(need);
  if (!copy) return -ENOMEM;
  memcpy(copy, nv12, need);
  s->ring[s->head % STUB_RING] = (struct frame){ copy, need, pts_us };
  s->head++;
  return 0;
}

// Every drain ends with its own end of stream (the guest starts the encoder again after one and may drain again).
static int stub_drain(br_enc_session *s) { s->draining = true; s->eos_done = false; return 0; }

static int stub_poll(br_enc_session *s, uint8_t *buf, uint32_t cap, int *key, int64_t *pts_us, int *eos) {
  *key = 0; *eos = 0; *pts_us = 0;
  if (s->tail != s->head) {
    struct frame *f = &s->ring[s->tail % STUB_RING];
    int size = stub_encode(s, f->nv12, buf, cap);
    *key = 1;                                                               // every frame is an IDR
    *pts_us = f->pts_us;
    free(f->nv12);
    f->nv12 = NULL;
    s->tail++;
    return size;
  }
  if (s->draining && !s->eos_done) { s->eos_done = true; *eos = 1; return 0; }
  return 0;
}

static void stub_close(br_enc_session *s) {
  while (s->tail != s->head) { free(s->ring[s->tail % STUB_RING].nv12); s->tail++; }
  free(s);
}

static const br_encoder_backend stub = {
  .name = "stand-in", .open = stub_open, .feed = stub_feed, .drain = stub_drain, .poll = stub_poll, .close = stub_close,
};

const br_encoder_backend *br_encoder_backend_stub(void) { return &stub; }
