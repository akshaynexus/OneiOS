// SPDX-License-Identifier: MIT
// Picture conversion for the camera helper: Android's three-plane YUV 4:2:0 to NV12 (what the guest
// gets), NV12 to RGB and BGRA, and a PNG writer for camctl (stored blocks: no zlib on the phone side).
#include "camera.h"
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

void cam_yuv_to_nv12(const cam_yuv *src, uint8_t *dst, int dst_stride)
{
  int w = src->width, h = src->height, cw = (w + 1) / 2, ch = (h + 1) / 2;
  for (int row = 0; row < h; row++) memcpy(dst + (size_t)row * (size_t)dst_stride, src->y + (size_t)row * (size_t)src->y_stride, (size_t)w);
  uint8_t *out = dst + (size_t)dst_stride * (size_t)h;
  // Interleaved CbCr already (the usual gralloc layout): whole rows. The last sample of the last row
  // of such a plane may be one byte short of 2*cw, so that row is copied by samples.
  bool nv12 = src->c_step == 2 && src->cr == src->cb + 1;
  for (int row = 0; row < ch; row++) {
    const uint8_t *cb = src->cb + (size_t)row * (size_t)src->c_stride, *cr = src->cr + (size_t)row * (size_t)src->c_stride;
    uint8_t *line = out + (size_t)row * (size_t)dst_stride;
    if (nv12 && row + 1 < ch) { memcpy(line, cb, (size_t)cw * 2); continue; }
    for (int x = 0; x < cw; x++) {
      line[2 * x] = cb[(size_t)x * (size_t)src->c_step];
      line[2 * x + 1] = cr[(size_t)x * (size_t)src->c_step];
    }
  }
}

static inline uint8_t clamp8(int v) { return (uint8_t)(v < 0 ? 0 : v > 255 ? 255 : v); }

// BT.601 full range, 16.16 fixed point: R = Y + 1.402 Cr', G = Y - 0.344136 Cb' - 0.714136 Cr', B = Y + 1.772 Cb'.
static inline void pixel(int y, int cb, int cr, uint8_t *r, uint8_t *g, uint8_t *b)
{
  cb -= 128; cr -= 128;
  *r = clamp8(y + ((91881 * cr + 32768) >> 16));
  *g = clamp8(y - ((22554 * cb + 46802 * cr + 32768) >> 16));
  *b = clamp8(y + ((116130 * cb + 32768) >> 16));
}

void cam_nv12_to_rgb(const uint8_t *nv12, int width, int height, int stride, uint8_t *rgb)
{
  const uint8_t *chroma = nv12 + (size_t)stride * (size_t)height;
  for (int row = 0; row < height; row++) {
    const uint8_t *y = nv12 + (size_t)row * (size_t)stride, *c = chroma + (size_t)(row / 2) * (size_t)stride;
    uint8_t *out = rgb + (size_t)row * (size_t)width * 3;
    for (int x = 0; x < width; x++) pixel(y[x], c[x & ~1], c[(x & ~1) + 1], &out[3 * x], &out[3 * x + 1], &out[3 * x + 2]);
  }
}

void cam_nv12_to_bgra(const uint8_t *nv12, int width, int height, int stride, uint8_t *bgra, int bgra_stride)
{
  const uint8_t *chroma = nv12 + (size_t)stride * (size_t)height;
  for (int row = 0; row < height; row++) {
    const uint8_t *y = nv12 + (size_t)row * (size_t)stride, *c = chroma + (size_t)(row / 2) * (size_t)stride;
    uint8_t *out = bgra + (size_t)row * (size_t)bgra_stride;
    for (int x = 0; x < width; x++) {
      pixel(y[x], c[x & ~1], c[(x & ~1) + 1], &out[4 * x + 2], &out[4 * x + 1], &out[4 * x]);
      out[4 * x + 3] = 255;
    }
  }
}

uint32_t cam_crc32(uint32_t crc, const uint8_t *data, size_t length)
{
  static uint32_t table[256];
  if (!table[1]) for (uint32_t i = 0; i < 256; i++) { uint32_t c = i; for (int k = 0; k < 8; k++) c = c & 1 ? 0xedb88320u ^ (c >> 1) : c >> 1; table[i] = c; }
  crc = ~crc;
  for (size_t i = 0; i < length; i++) crc = table[(crc ^ data[i]) & 0xff] ^ (crc >> 8);
  return ~crc;
}

static void be32(uint8_t *p, uint32_t v) { p[0] = (uint8_t)(v >> 24); p[1] = (uint8_t)(v >> 16); p[2] = (uint8_t)(v >> 8); p[3] = (uint8_t)v; }

static int chunk(FILE *f, const char *type, const uint8_t *data, uint32_t length)
{
  uint8_t head[8], tail[4];
  be32(head, length);
  memcpy(head + 4, type, 4);
  uint32_t crc = cam_crc32(0, head + 4, 4);
  if (length) crc = cam_crc32(crc, data, length);
  be32(tail, crc);
  return fwrite(head, 1, 8, f) == 8 && (!length || fwrite(data, 1, length, f) == length) && fwrite(tail, 1, 4, f) == 4 ? 0 : -EIO;
}

int cam_png_write(const char *path, const uint8_t *rgb, int width, int height)
{
  if (width <= 0 || height <= 0 || width > 16384 || height > 16384) return -EINVAL;
  size_t line = 1 + (size_t)width * 3, raw = line * (size_t)height;
  // zlib: 2 bytes, stored blocks of at most 65535 bytes (5 bytes each), Adler-32.
  size_t blocks = (raw + 65534) / 65535, size = 2 + raw + blocks * 5 + 4;
  uint8_t *z = malloc(size);
  if (!z) return -ENOMEM;
  size_t at = 0, done = 0;
  uint32_t a = 1, b = 0;
  z[at++] = 0x78; z[at++] = 0x01;
  // The filtered stream (filter 0 before each row) goes straight into stored blocks.
  size_t in_block = 0, block_left = 0;
  for (int row = 0; row < height; row++) {
    for (size_t i = 0; i < line; i++) {
      if (!block_left) {
        size_t n = raw - done < 65535 ? raw - done : 65535;
        z[at++] = done + n == raw;                     // BFINAL, BTYPE 00
        z[at++] = (uint8_t)n; z[at++] = (uint8_t)(n >> 8); z[at++] = (uint8_t)~n; z[at++] = (uint8_t)(~n >> 8);
        block_left = n; in_block = 0;
      }
      uint8_t value = i ? rgb[(size_t)row * (size_t)width * 3 + i - 1] : 0;
      z[at++] = value;
      a += value; if (a >= 65521) a -= 65521;
      b += a; if (b >= 65521) b -= 65521;
      block_left--; in_block++; done++;
    }
  }
  (void)in_block;
  be32(z + at, b << 16 | a); at += 4;
  FILE *f = fopen(path, "wb");
  if (!f) { int e = errno; free(z); return -e; }
  static const uint8_t signature[8] = { 0x89, 'P', 'N', 'G', '\r', '\n', 0x1a, '\n' };
  uint8_t ihdr[13];
  be32(ihdr, (uint32_t)width); be32(ihdr + 4, (uint32_t)height);
  ihdr[8] = 8; ihdr[9] = 2; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;     // 8 bits, RGB
  int rc = fwrite(signature, 1, 8, f) == 8 ? 0 : -EIO;
  if (!rc) rc = chunk(f, "IHDR", ihdr, 13);
  if (!rc) rc = chunk(f, "IDAT", z, (uint32_t)at);
  if (!rc) rc = chunk(f, "IEND", NULL, 0);
  if (fclose(f) && !rc) rc = -EIO;
  free(z);
  return rc;
}
