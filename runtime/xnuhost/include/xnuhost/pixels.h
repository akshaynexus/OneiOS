// SPDX-License-Identifier: MIT
// The guest frame's pixel formats between xnuhost and the display helper (display.sock's hello, docs/booter.md 2.8),
// and the conversions between them. Header-only: xnuhost (src/virtio_gpu.c) and the display helper (runtime/display)
// both include it, so the two sides cannot disagree on what a format holds.
//
//   XH_PIXELS_BGRX8    1  bytes B,G,R,X: 8 bits a channel, sRGB-encoded, sRGB primaries. iOS's 'BGRA' framebuffer:
//                         QuartzCore composites this display in sRGB (CA::WindowServer::IOMFBDisplay::
//                         compositing_color_space is CAGetColorSpace(36), kCGColorSpaceSRGB, in 24A446), so colours
//                         outside sRGB (Display P3's reds and greens) are clipped before the frame leaves the guest.
//   XH_PIXELS_RGBA16F  2  R,G,B,A IEEE 754 half floats, little-endian: linear light, sRGB primaries, extended range:
//                         components below 0 and above 1 carry colours outside sRGB and EDR highlights. iOS's 'RGhA'
//                         framebuffer (Android's RGBA_FP16 with the dataspace SCRGB_LINEAR is the same thing).
#ifndef XNUHOST_PIXELS_H
#define XNUHOST_PIXELS_H
#include <math.h>
#include <stdint.h>
#include <string.h>

#define XH_PIXELS_BGRX8   1u
#define XH_PIXELS_RGBA16F 2u

// The scanout format a guest gives half-float surfaces in (RESOURCE_CREATE_2D, SET_SCANOUT_BLOB): xnuhost's own, not
// virtio's (virtio-gpu has no half-float format; its codes are small numbers): IOSurface's FourCC 'RGhA'. The guest's
// display (kernel/patches/oneios-hosted/display) sends it only when xnuhost passed oneios-display-wide=1.
#define XH_VIRTIO_GPU_FORMAT_RGBA16F 0x52476841u

static inline unsigned xh_pixels_bpp(uint32_t format)
{
  return format == XH_PIXELS_BGRX8 ? 4u : format == XH_PIXELS_RGBA16F ? 8u : 0u;
}

static inline const char *xh_pixels_name(uint32_t format)
{
  return format == XH_PIXELS_BGRX8 ? "B,G,R,X 8-bit sRGB" : format == XH_PIXELS_RGBA16F ? "R,G,B,A half float, linear extended sRGB" : "unknown";
}

// IEEE 754 binary16 from binary32: round to nearest even, overflow to infinity, NaN kept a NaN.
static inline uint16_t xh_half_from_float(float value)
{
  uint32_t f;
  memcpy(&f, &value, sizeof f);
  uint32_t sign = (f >> 16) & 0x8000u, exp = (f >> 23) & 0xffu, mant = f & 0x7fffffu;
  if (exp == 0xffu) return (uint16_t)(sign | 0x7c00u | (mant ? 0x200u : 0u));
  int e = (int)exp - 127 + 15;
  if (e >= 31) return (uint16_t)(sign | 0x7c00u);
  if (e <= 0) {                                        // a subnormal half, or zero
    if (e < -10) return (uint16_t)sign;
    mant |= 0x800000u;
    unsigned shift = (unsigned)(14 - e);
    uint32_t half = mant >> shift, rest = mant & ((1u << shift) - 1), mid = 1u << (shift - 1);
    if (rest > mid || (rest == mid && (half & 1u))) half++;
    return (uint16_t)(sign | half);
  }
  uint32_t half = ((uint32_t)e << 10) | (mant >> 13), rest = mant & 0x1fffu;
  if (rest > 0x1000u || (rest == 0x1000u && (half & 1u))) half++;   // may carry into the exponent: still right
  return (uint16_t)(sign | half);
}

static inline float xh_half_to_float(uint16_t h)
{
  uint32_t sign = (uint32_t)(h & 0x8000u) << 16, exp = (h >> 10) & 0x1fu, mant = h & 0x3ffu, f;
  if (exp == 0x1fu) f = sign | 0x7f800000u | (mant << 13);
  else if (exp) f = sign | ((exp + 112u) << 23) | (mant << 13);
  else if (!mant) f = sign;
  else {                                               // subnormal: normalise
    exp = 113;
    while (!(mant & 0x400u)) { mant <<= 1; exp--; }
    f = sign | (exp << 23) | ((mant & 0x3ffu) << 13);
  }
  float value;
  memcpy(&value, &f, sizeof value);
  return value;
}

static inline float xh_srgb_decode(float v)
{
  return v <= 0.04045f ? v / 12.92f : powf((v + 0.055f) / 1.055f, 2.4f);
}

static inline float xh_srgb_encode(float v)
{
  return v <= 0.0031308f ? v * 12.92f : 1.055f * powf(v, 1.0f / 2.4f) - 0.055f;
}

// 8-bit sRGB code -> linear half. Filled once; a second filler writes the same values.
static inline const uint16_t *xh_srgb8_to_half(void)
{
  static uint16_t table[256];
  static int ready;
  if (!__atomic_load_n(&ready, __ATOMIC_ACQUIRE)) {
    for (unsigned i = 0; i < 256; i++) table[i] = xh_half_from_float(xh_srgb_decode((float)i / 255.0f));
    __atomic_store_n(&ready, 1, __ATOMIC_RELEASE);
  }
  return table;
}

// Linear half (all 65536 patterns) -> 8-bit sRGB code, clamped to sRGB's range: negative values and NaN give 0,
// 1 and above (and +infinity) 255. What a frame of extended range loses on an 8-bit buffer.
static inline const uint8_t *xh_half_to_srgb8(void)
{
  static uint8_t table[65536];
  static int ready;
  if (!__atomic_load_n(&ready, __ATOMIC_ACQUIRE)) {
    for (unsigned i = 0; i < 65536; i++) {
      float v = xh_half_to_float((uint16_t)i);
      if (!(v > 0.0f)) table[i] = 0;                   // negative, zero, NaN
      else if (v >= 1.0f) table[i] = 255;
      else table[i] = (uint8_t)(xh_srgb_encode(v) * 255.0f + 0.5f);
    }
    __atomic_store_n(&ready, 1, __ATOMIC_RELEASE);
  }
  return table;
}

// A row of B,G,R,X 8-bit pixels to R,G,B,A half floats (alpha 1).
static inline void xh_pixels_bgrx8_to_rgba16f(uint8_t *to, const uint8_t *from, size_t pixels)
{
  const uint16_t *t = xh_srgb8_to_half();
  for (size_t i = 0; i < pixels; i++, from += 4, to += 8) {
    uint16_t px[4] = { t[from[2]], t[from[1]], t[from[0]], 0x3c00u };
    memcpy(to, px, 8);
  }
}

// A row of R,G,B,A half floats to 8-bit pixels: rgb 0 gives B,G,R,X bytes, 1 gives R,G,B,X (X written 0xff).
static inline void xh_pixels_rgba16f_to_8(uint8_t *to, const uint8_t *from, size_t pixels, int rgb)
{
  const uint8_t *t = xh_half_to_srgb8();
  for (size_t i = 0; i < pixels; i++, from += 8, to += 4) {
    uint16_t px[4];
    memcpy(px, from, 8);
    uint8_t r = t[px[0]], g = t[px[1]], b = t[px[2]];
    to[0] = rgb ? r : b; to[1] = g; to[2] = rgb ? b : r; to[3] = 0xff;
  }
}
#endif
