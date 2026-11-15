// SPDX-License-Identifier: MIT
// Host test of the stand-in encoder (backend_stub.c) alone: for a list of frame sizes and buffer sizes it feeds a
// test picture, takes the access unit, and checks that it fits the guest's coded buffer (width * height / 2 + 64 KiB),
// that its NAL units are the ones a decoder needs in their order, that the first block's arithmetic-coded bytes are
// the ones worked out by hand from the standard, and that the samples in the HEVC slice are the frame's (within the
// PCM depth's step).
//
//   test_standin [DIR]    with DIR: also writes NAME.hevc / NAME.h264 (the access unit) and NAME.yuv (the picture a
//                         decoder must give: planar 4:2:0) for tests/host.sh, which decodes them with ffmpeg
#define _GNU_SOURCE
#include "encoder.h"
#include "standin_reader.h"
#include <stdio.h>

static int failures, checks;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

static void save(const char *dir, const char *name, const char *suffix, const uint8_t *data, uint32_t size) {
  if (!dir) return;
  char path[512];
  snprintf(path, sizeof path, "%s/%s.%s", dir, name, suffix);
  FILE *f = fopen(path, "wb");
  if (!f || fwrite(data, 1, size, f) != size) { failures++; printf("FAIL: cannot write %s\n", path); }
  if (f) fclose(f);
}

// A picture with every level in it, edges, a black and a white corner block, and coloured chroma.
static void pattern(uint8_t *nv12, uint32_t w, uint32_t h) {
  for (uint32_t y = 0; y < h; y++)
    for (uint32_t x = 0; x < w; x++)
      nv12[y * w + x] = x < 16 && y < 16 ? 0 : x >= w - 16 && y >= h - 16 ? 255 : (uint8_t)((x * 255 / (w - 1) + y * 255 / (h - 1)) / 2 + ((x ^ y) & 8 ? 6 : 0));
  uint8_t *uv = nv12 + w * h;
  for (uint32_t y = 0; y < h / 2; y++)
    for (uint32_t x = 0; x < w / 2; x++) {
      uv[(y * (w / 2) + x) * 2] = (uint8_t)(64 + x * 128 / (w / 2));
      uv[(y * (w / 2) + x) * 2 + 1] = (uint8_t)(192 - y * 128 / (h / 2));
    }
}

// One frame through the backend. cap 0: the guest's coded buffer size.
static void one(const char *dir, uint32_t codec, uint32_t w, uint32_t h, uint32_t cap, bool black) {
  const br_encoder_backend *backend = br_encoder_backend_stub();
  bool hevc = codec == MEDIA_PIXFMT_HEVC;
  uint32_t frame = w * h * 3 / 2, budget = w * h / 2 + 65536;
  if (!cap) cap = budget;
  uint8_t *nv12 = malloc(frame), *au = malloc(cap), *planar = malloc(frame);
  char name[64];
  snprintf(name, sizeof name, "%s-%ux%u-%u%s", hevc ? "hevc" : "h264", w, h, cap, black ? "-black" : "");
  if (black) { memset(nv12, 0, w * h); memset(nv12 + w * h, 128, frame - w * h); } else pattern(nv12, w, h);
  br_enc_session *s = backend->open(codec, w, h);
  CHECK(s != NULL);
  if (!s) return;
  CHECK(backend->feed(s, nv12, frame, 123456, 1) == 0);
  int key = 0, eos = 0;
  int64_t pts = 0;
  int size = backend->poll(s, au, cap, &key, &pts, &eos);
  CHECK(size > 0 && (uint32_t)size <= cap && (uint32_t)size <= budget);
  CHECK(key == 1 && pts == 123456 && !eos);
  CHECK(backend->poll(s, au, cap, &key, &pts, &eos) == 0 && !eos);          // nothing more
  CHECK(backend->drain(s) == 0);
  CHECK(backend->poll(s, au + (size > 0 ? size : 0), 0, &key, &pts, &eos) == 0 && eos == 1);
  backend->close(s);
  if (size <= 0) { printf("FAIL %s: no access unit (%d)\n", name, size); free(nv12); free(au); free(planar); return; }

  standin_nal nals[8];
  unsigned count = standin_split(au, (uint32_t)size, hevc, nals, 8);
  unsigned luma = 8, chroma = 8;
  bool pcm = true;
  if (hevc) {
    CHECK(count == 4 && nals[0].type == 32 && nals[1].type == 33 && nals[2].type == 34 && nals[3].type == 19);
    // The profile, tier and level begin at the SPS's fourth byte (DroidVMVTHost.cpp copies them into the hvcC): Main.
    CHECK(count == 4 && nals[1].size > 15 && nals[1].nal[3] == 0x01 && nals[1].nal[4] == 0x60);
    uint32_t n = 0;
    uint8_t *rbsp = count == 4 ? standin_rbsp(&nals[3], true, &n) : NULL;
    CHECK(rbsp && standin_hevc_picture(rbsp, n, w, h, planar, &luma, &chroma));
    // The first block: part_mode 1 at state 0, then pcm_flag 1 and the flush, are the nine bits 100001101, then seven
    // alignment zeros (worked out by hand from 9.3.4.x: range 510 -> 270 -> 268, low 268, seven shifts).
    CHECK(rbsp && n > 3 && rbsp[1] == 0x86 && rbsp[2] == 0x80);
    free(rbsp);
    CHECK(standin_same_picture(planar, nv12, w, h, luma, chroma));
  } else {
    CHECK(count == 3 && nals[0].type == 7 && nals[1].type == 8 && nals[2].type == 5);
    CHECK(count == 3 && nals[0].nal[1] == 66);                              // Baseline
    pcm = (uint32_t)size > frame;                                           // raw samples, or the grey picture
    for (uint32_t i = 0; i < w * h; i++) planar[i] = pcm ? nv12[i] : 128;
    for (uint32_t i = 0; i < (w / 2) * (h / 2); i++) {
      planar[w * h + i] = pcm ? nv12[w * h + 2 * i] : 128;
      planar[w * h + (w / 2) * (h / 2) + i] = pcm ? nv12[w * h + 2 * i + 1] : 128;
    }
  }
  printf("%s: %d bytes of %u, %s\n", name, size, cap,
         hevc ? (luma == 8 ? "PCM 8+8 bits" : luma == 4 ? "PCM 4+4 bits" : luma == 3 ? "PCM 3+2 bits" : chroma == 2 ? "PCM 2+2 bits" : luma == 2 ? "PCM 2+1 bits" : "PCM 1+1 bits")
              : pcm ? "PCM" : "grey");
  save(dir, name, hevc ? "hevc" : "h264", au, (uint32_t)size);
  save(dir, name, "yuv", planar, frame);
  free(nv12); free(au); free(planar);
}

int main(int argc, char **argv) {
  const char *dir = argc > 1 ? argv[1] : NULL;
  static const uint32_t sizes[][2] = { { 64, 64 }, { 256, 256 }, { 364, 382 }, { 338, 158 }, { 640, 480 }, { 1440, 3120 } };
  for (unsigned i = 0; i < sizeof sizes / sizeof sizes[0]; i++) {
    one(dir, MEDIA_PIXFMT_HEVC, sizes[i][0], sizes[i][1], 0, false);
    one(dir, MEDIA_PIXFMT_H264, sizes[i][0], sizes[i][1], 0, false);
  }
  one(dir, MEDIA_PIXFMT_HEVC, 256, 256, 8u << 20, false);                   // a large buffer changes nothing: the guest's size rules
  one(dir, MEDIA_PIXFMT_HEVC, 640, 480, 0, true);                           // black: zero samples, the most emulation prevention bytes
  one(dir, MEDIA_PIXFMT_HEVC, 640, 480, 100000, false);                     // a small buffer: fewer levels
  one(dir, MEDIA_PIXFMT_H264, 64, 64, 0, true);
  // A buffer nothing fits in is an error, not an overrun.
  const br_encoder_backend *backend = br_encoder_backend_stub();
  br_enc_session *s = backend->open(MEDIA_PIXFMT_HEVC, 640, 480);
  uint8_t *nv12 = calloc(1, 640 * 480 * 3 / 2), small[64];
  int key, eos; int64_t pts;
  CHECK(s && nv12 && backend->feed(s, nv12, 640 * 480 * 3 / 2, 0, 1) == 0 && backend->poll(s, small, sizeof small, &key, &pts, &eos) < 0);
  CHECK(s && backend->feed(s, nv12, 100, 0, 1) < 0);                        // a short frame is refused
  if (s) backend->close(s);
  free(nv12);
  printf("test_standin: %d/%d checks passed\n", checks - failures, checks);
  return failures ? 1 : 0;
}
