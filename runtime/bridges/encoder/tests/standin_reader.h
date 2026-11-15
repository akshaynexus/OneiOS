// SPDX-License-Identifier: MIT
// The tests' reader of the stand-in encoder's streams (backend_stub.c): it splits an Annex-B access unit, takes the
// emulation prevention bytes out, and reads the I_PCM samples of the HEVC slice back into a picture, so a test can
// say that the frame it fed is the frame in the stream. It reads only what the stand-in writes (one slice, 16x16
// blocks, the PCM depths of its list); a real decoder's reading is tests/host.sh's ffmpeg comparison.
#ifndef ONEIOS_ENCODER_STANDIN_READER_H
#define ONEIOS_ENCODER_STANDIN_READER_H

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct { const uint8_t *nal; uint32_t size; unsigned type; } standin_nal;

// The NAL units of an access unit (4-byte start codes): how many (up to `max`), each with its header.
static inline unsigned standin_split(const uint8_t *au, uint32_t size, bool hevc, standin_nal *out, unsigned max) {
  unsigned n = 0;
  uint32_t i = 0;
  while (i + 4 < size && n < max) {
    if (au[i] || au[i + 1] || au[i + 2] || au[i + 3] != 1) { i++; continue; }
    uint32_t start = i + 4, end = start;
    while (end + 3 < size && (au[end] || au[end + 1] || au[end + 2] || au[end + 3] != 1)) end++;
    if (end + 3 >= size) end = size;
    out[n++] = (standin_nal){ au + start, end - start, hevc ? (au[start] >> 1) & 0x3f : au[start] & 0x1f };
    i = end;
  }
  return n;
}

// The payload of a NAL unit without its header and its emulation prevention bytes, in a buffer the caller frees.
static inline uint8_t *standin_rbsp(const standin_nal *nal, bool hevc, uint32_t *size) {
  unsigned header = hevc ? 2 : 1;
  uint8_t *out = malloc(nal->size ? nal->size : 1);
  uint32_t n = 0;
  unsigned zeros = 0;
  for (uint32_t i = header; out && i < nal->size; i++) {
    if (zeros >= 2 && nal->nal[i] == 3) { zeros = 0; continue; }
    out[n++] = nal->nal[i];
    zeros = nal->nal[i] ? 0 : zeros + 1;
  }
  *size = n;
  return out;
}

// The HEVC slice's samples as planar 4:2:0 (w x h luma, then Cb, then Cr), cropped from the 16x16 blocks, and the PCM
// depths found. `out` holds w * h + 2 * (w/2) * (h/2) bytes (w, h even). False when the slice is not the stand-in's.
static inline bool standin_hevc_picture(const uint8_t *rbsp, uint32_t size, uint32_t w, uint32_t h, uint8_t *out,
                                        unsigned *luma_depth, unsigned *chroma_depth) {
  static const uint8_t depths[][2] = { { 8, 8 }, { 4, 4 }, { 3, 2 }, { 2, 2 }, { 2, 1 }, { 1, 1 } };
  uint32_t columns = (w + 15) / 16, rows = (h + 15) / 16, blocks = columns * rows;
  // The slice header is one byte; each block: two arithmetic-coded bytes (the end of the block before, part_mode,
  // pcm_flag) and its samples; the last end_of_slice_segment_flag: two bytes more.
  if (size < 3 || rbsp[0] != 0xaf || (size - 3) % blocks) return false;
  uint32_t each = (size - 3) / blocks;
  unsigned dl = 0, dc = 0;
  for (unsigned i = 0; i < sizeof depths / sizeof depths[0]; i++)
    if (each == 2u + 32u * depths[i][0] + 16u * depths[i][1]) { dl = depths[i][0]; dc = depths[i][1]; }
  if (!dl) return false;
  *luma_depth = dl;
  *chroma_depth = dc;
  uint8_t *cb = out + (size_t)w * h, *cr = cb + (size_t)(w / 2) * (h / 2);
  for (uint32_t b = 0; b < blocks; b++) {
    const uint8_t *p = rbsp + 1 + (size_t)b * each + 2;                    // after the block's two arithmetic-coded bytes
    uint32_t bit = 0, bx = b % columns, by = b / columns;
    #define STANDIN_READ(depth, value) do { value = 0; for (unsigned k = 0; k < depth; k++, bit++) value = (value << 1) | ((p[bit >> 3] >> (7 - (bit & 7))) & 1); } while (0)
    for (unsigned y = 0; y < 16; y++)
      for (unsigned x = 0; x < 16; x++) {
        uint32_t v; STANDIN_READ(dl, v);
        if (bx * 16 + x < w && by * 16 + y < h) out[(size_t)(by * 16 + y) * w + bx * 16 + x] = (uint8_t)(v << (8 - dl));
      }
    for (unsigned plane = 0; plane < 2; plane++)
      for (unsigned y = 0; y < 8; y++)
        for (unsigned x = 0; x < 8; x++) {
          uint32_t v; STANDIN_READ(dc, v);
          if (bx * 8 + x < w / 2 && by * 8 + y < h / 2) (plane ? cr : cb)[(size_t)(by * 8 + y) * (w / 2) + bx * 8 + x] = (uint8_t)(v << (8 - dc));
        }
    #undef STANDIN_READ
  }
  return true;
}

// Whether the planar picture `got` is the NV12 frame `nv12` within the PCM depths' steps (the dither rounds up by
// less than a step, and the top code saturates).
static inline bool standin_same_picture(const uint8_t *got, const uint8_t *nv12, uint32_t w, uint32_t h, unsigned luma_depth, unsigned chroma_depth) {
  int luma_step = 1 << (8 - luma_depth), chroma_step = 1 << (8 - chroma_depth);
  for (uint32_t i = 0; i < w * h; i++) {
    int d = (int)got[i] - (int)nv12[i];
    if (d >= luma_step || d <= -luma_step) return false;
  }
  const uint8_t *cb = got + (size_t)w * h, *cr = cb + (size_t)(w / 2) * (h / 2), *uv = nv12 + (size_t)w * h;
  for (uint32_t i = 0; i < (w / 2) * (h / 2); i++) {
    int db = (int)cb[i] - (int)uv[2 * i], dr = (int)cr[i] - (int)uv[2 * i + 1];
    if (db >= chroma_step || db <= -chroma_step || dr >= chroma_step || dr <= -chroma_step) return false;
  }
  return true;
}

#endif
