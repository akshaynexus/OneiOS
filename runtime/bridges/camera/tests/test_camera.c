// SPDX-License-Identifier: MIT
// Host tests of the camera bridge: no phone, no Android, no Apple files. The frame ring, picture
// conversion, the PNG writer, camera metadata, the core with the test back end, the real helper
// process (built for the host, test back end) through xnuhost's end of it, and the virtio device on
// xnuhost's real transport (runtime/xnuhost/src/virtio.c) against a guest made of plain memory.
//
//   test_camera PATH-TO-HOST-oneios-camera
#define _GNU_SOURCE
#include "bridges/devices.h"
#include "bridges/camera.h"
#include "../camera.h"
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static int failures, checks;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

void cam_test_picture(uint8_t *y, uint8_t *cb, uint8_t *cr, int width, int height, uint64_t n);

// ---- what xnuhost supplies around virtio.c -------------------------------------------------------
void xh_gic_set_spi(unsigned intid, bool level) { (void)intid; (void)level; }
int xh_mmio_add(const xh_mmio_region *region) { (void)region; return 0; }
void *xh_ram(uint64_t pa, uint64_t len) { (void)len; return (void *)(uintptr_t)pa; }
// libonebridges' log (cpu/topology.c is not linked here).
void (*br_log_sink)(int level, const char *line);
void br_log(int level, const char *fmt, ...) { (void)level; (void)fmt; }

static void nap(unsigned ms) { usleep(ms * 1000); }

// ---- the ring ------------------------------------------------------------------------------------
static void put_frame(cam_ring_map *w, uint64_t sequence, uint32_t bytes)
{
  unsigned slot;
  uint8_t *at = cam_ring_begin(w, &slot);
  memset(at, (int)(sequence & 0xff), bytes);
  struct oc_frame f = { .magic = OC_FRAME_MAGIC, .sequence = sequence, .timestamp_ns = sequence * 1000, .width = 64, .height = 32, .stride = 64,
                        .format = OC_FMT_NV12, .bytes = bytes };
  cam_ring_commit(w, slot, &f);
}

static struct { cam_ring_map *ring; bool run; uint64_t written; } stress;
static void *stress_writer(void *arg)
{
  (void)arg;
  for (uint64_t n = 1; __atomic_load_n(&stress.run, __ATOMIC_ACQUIRE); n++) { put_frame(stress.ring, n, 60000); __atomic_store_n(&stress.written, n, __ATOMIC_RELEASE); }
  return NULL;
}

static void test_ring(void)
{
  cam_ring_map w, r;
  struct oc_frame f;
  static uint8_t buffer[70000];
  uint64_t last = 0;
  CHECK(cam_ring_create(&w, 1, 100) == -EINVAL);
  CHECK(cam_ring_create(&w, 2, 60000) == 0);
  CHECK(cam_ring_attach(&r, dup(w.fd)) == 0);
  CHECK(r.slots == 2 && r.slot_bytes == 60000);
  CHECK(cam_ring_read(&r, &last, &f, buffer, sizeof buffer) == -EAGAIN);
  CHECK(cam_ring_newest(&r) == 0);
  put_frame(&w, 1, 3000);
  CHECK(cam_ring_newest(&r) == 1);
  CHECK(cam_ring_read(&r, &last, &f, buffer, 100) == -EMSGSIZE && last == 0);
  CHECK(cam_ring_read(&r, &last, &f, buffer, sizeof buffer) == 0);
  CHECK(last == 1 && f.sequence == 1 && f.bytes == 3000 && f.timestamp_ns == 1000 && buffer[0] == 1 && buffer[2999] == 1);
  CHECK(cam_ring_read(&r, &last, &f, buffer, sizeof buffer) == -EAGAIN);
  // The reader takes the newest, not the next.
  for (uint64_t n = 2; n <= 9; n++) put_frame(&w, n, 3000);
  CHECK(cam_ring_read(&r, &last, &f, buffer, sizeof buffer) == 0 && f.sequence == 9 && buffer[100] == 9);
  // The writer is in another place: the newest stays readable. In the newest's own place: nothing whole to read.
  unsigned slot;
  put_frame(&w, 10, 3000);
  (void)cam_ring_begin(&w, &slot);
  CHECK(cam_ring_read(&r, &last, &f, buffer, sizeof buffer) == 0 && f.sequence == 10);
  struct oc_frame eleven = { .magic = OC_FRAME_MAGIC, .sequence = 11, .bytes = 10 };
  cam_ring_commit(&w, slot, &eleven);
  unsigned other;
  (void)cam_ring_begin(&w, &other);                          // the place frame 10 is in; latest names frame 11
  (void)cam_ring_begin(&w, &slot);                           // the place frame 11 is in
  CHECK(cam_ring_read(&r, &last, &f, buffer, sizeof buffer) == -EAGAIN && last == 10);
  struct oc_frame twelve = { .magic = OC_FRAME_MAGIC, .sequence = 12, .bytes = 10 }, thirteen = { .magic = OC_FRAME_MAGIC, .sequence = 13, .bytes = 10 };
  cam_ring_commit(&w, other, &twelve);
  cam_ring_commit(&w, slot, &thirteen);
  CHECK(cam_ring_read(&r, &last, &f, buffer, sizeof buffer) == 0 && last == 13);
  // A frame that claims more than a place holds is refused.
  struct oc_frame liar = { .magic = OC_FRAME_MAGIC, .sequence = 14, .bytes = 60001 };
  (void)cam_ring_begin(&w, &slot);
  cam_ring_commit(&w, slot, &liar);
  CHECK(cam_ring_read(&r, &last, &f, buffer, sizeof buffer) == -EIO);
  cam_ring_close(&r);
  cam_ring_close(&w);

  // A writer at full speed against a reader: every frame read is whole (one value) and matches its header.
  CHECK(cam_ring_create(&w, CAM_RING_SLOTS, 60000) == 0);
  CHECK(cam_ring_attach(&r, dup(w.fd)) == 0);
  stress.ring = &w; stress.run = true;
  pthread_t thread;
  pthread_create(&thread, NULL, stress_writer, NULL);
  unsigned good = 0, torn = 0;
  last = 0;
  for (unsigned i = 0; i < 20000 || good < 50; i++) {
    if (cam_ring_read(&r, &last, &f, buffer, sizeof buffer)) continue;
    bool whole = f.bytes == 60000;
    for (uint32_t k = 0; whole && k < f.bytes; k += 997) if (buffer[k] != (uint8_t)f.sequence) whole = false;
    if (whole && buffer[59999] == (uint8_t)f.sequence) good++; else torn++;
  }
  stress.run = false;
  pthread_join(thread, NULL);
  CHECK(good >= 50 && torn == 0);
  cam_ring_close(&r);
  cam_ring_close(&w);

  // What is not a ring is not attached.
  int fd = memfd_create("not-a-ring", 0);
  CHECK(ftruncate(fd, 100) == 0 && cam_ring_attach(&r, fd) == -EINVAL);
  CHECK(ftruncate(fd, 1 << 20) == 0 && cam_ring_attach(&r, fd) == -EINVAL);
  struct cam_ring bad = { .magic = CAM_RING_MAGIC, .version = CAM_RING_VERSION, .slots = 200, .slot_bytes = 1 << 20 };     // larger than the file
  CHECK(pwrite(fd, &bad, sizeof bad, 0) == sizeof bad && cam_ring_attach(&r, fd) == -EINVAL);
  close(fd);
}

// ---- pictures ------------------------------------------------------------------------------------
static void test_convert(void)
{
  enum { W = 6, H = 4 };
  uint8_t y[H][8], cb[2][4], cr[2][4], out[W * H * 3 / 2], padded[8 * H * 3 / 2];
  for (int r = 0; r < H; r++) for (int x = 0; x < 8; x++) y[r][x] = (uint8_t)(x < W ? 10 * r + x : 0xee);
  for (int r = 0; r < 2; r++) for (int x = 0; x < 4; x++) { cb[r][x] = (uint8_t)(x < 3 ? 100 + 10 * r + x : 0xee); cr[r][x] = (uint8_t)(x < 3 ? 200 + 10 * r + x : 0xee); }
  // Planar (I420) with row padding.
  cam_yuv planar = { .y = &y[0][0], .cb = &cb[0][0], .cr = &cr[0][0], .y_stride = 8, .c_stride = 4, .c_step = 1, .width = W, .height = H };
  cam_yuv_to_nv12(&planar, out, W);
  bool ok = true;
  for (int r = 0; r < H; r++) for (int x = 0; x < W; x++) if (out[r * W + x] != 10 * r + x) ok = false;
  for (int r = 0; r < 2; r++) for (int x = 0; x < 3; x++) if (out[W * H + r * W + 2 * x] != 100 + 10 * r + x || out[W * H + r * W + 2 * x + 1] != 200 + 10 * r + x) ok = false;
  CHECK(ok);
  // Into a wider stride: the padding is not written.
  memset(padded, 0x55, sizeof padded);
  cam_yuv_to_nv12(&planar, padded, 8);
  CHECK(padded[5] == 5 && padded[6] == 0x55 && padded[8 * H + 4] == 102 && padded[8 * H + 5] == 202 && padded[8 * H + 6] == 0x55);
  // Interleaved CbCr (gralloc's NV12): the chroma plane's last byte does not exist in the buffer.
  uint8_t semi[2][8];
  for (int r = 0; r < 2; r++) for (int x = 0; x < 3; x++) { semi[r][2 * x] = (uint8_t)(100 + 10 * r + x); semi[r][2 * x + 1] = (uint8_t)(200 + 10 * r + x); }
  cam_yuv nv12 = { .y = &y[0][0], .cb = &semi[0][0], .cr = &semi[0][1], .y_stride = 8, .c_stride = 8, .c_step = 2, .width = W, .height = H };
  uint8_t again[sizeof out];
  cam_yuv_to_nv12(&nv12, again, W);
  CHECK(!memcmp(out, again, sizeof out));
  // Interleaved CrCb (NV21): swapped back.
  for (int r = 0; r < 2; r++) for (int x = 0; x < 3; x++) { semi[r][2 * x] = (uint8_t)(200 + 10 * r + x); semi[r][2 * x + 1] = (uint8_t)(100 + 10 * r + x); }
  cam_yuv nv21 = { .y = &y[0][0], .cb = &semi[0][1], .cr = &semi[0][0], .y_stride = 8, .c_stride = 8, .c_step = 2, .width = W, .height = H };
  memset(again, 0, sizeof again);
  cam_yuv_to_nv12(&nv21, again, W);
  CHECK(!memcmp(out, again, sizeof out));

  // Colours, BT.601 full range: grey, white, black, and the three primaries within 2 steps.
  static const struct { uint8_t y, cb, cr, r, g, b; } colours[] = {
    { 128, 128, 128, 128, 128, 128 }, { 255, 128, 128, 255, 255, 255 }, { 0, 128, 128, 0, 0, 0 },
    { 76, 85, 255, 254, 0, 0 }, { 150, 44, 21, 0, 255, 1 }, { 29, 255, 107, 0, 0, 254 },
  };
  for (unsigned i = 0; i < sizeof colours / sizeof colours[0]; i++) {
    uint8_t picture[2 * 2 * 3 / 2] = { colours[i].y, colours[i].y, colours[i].y, colours[i].y, colours[i].cb, colours[i].cr }, rgb[12], bgra[16];
    cam_nv12_to_rgb(picture, 2, 2, 2, rgb);
    cam_nv12_to_bgra(picture, 2, 2, 2, bgra, 8);
    CHECK(abs(rgb[9] - colours[i].r) <= 2 && abs(rgb[10] - colours[i].g) <= 2 && abs(rgb[11] - colours[i].b) <= 2);
    CHECK(bgra[12] == rgb[11] && bgra[13] == rgb[10] && bgra[14] == rgb[9] && bgra[15] == 255);
  }
}

static uint32_t be32(const uint8_t *p) { return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3]; }

static void test_png(const char *tmp)
{
  enum { W = 301, H = 217 };                                 // 196 kB of rows: more than one stored block
  char path[400];
  static uint8_t rgb[W * H * 3], file[W * H * 3 + 4096], raw[(W * 3 + 1) * H];
  for (unsigned i = 0; i < sizeof rgb; i++) rgb[i] = (uint8_t)(i * 7 + i / 251);
  snprintf(path, sizeof path, "%s/picture.png", tmp);
  CHECK(cam_png_write(path, rgb, W, H) == 0);
  FILE *f = fopen(path, "rb");
  size_t size = f ? fread(file, 1, sizeof file, f) : 0;
  if (f) fclose(f);
  CHECK(size > sizeof rgb && !memcmp(file, "\x89PNG\r\n\x1a\n", 8));
  // IHDR, one IDAT, IEND: lengths, types and CRCs.
  CHECK(be32(file + 8) == 13 && !memcmp(file + 12, "IHDR", 4) && be32(file + 16) == W && be32(file + 20) == H && file[24] == 8 && file[25] == 2);
  CHECK(be32(file + 29) == cam_crc32(0, file + 12, 17));
  uint32_t idat = be32(file + 33);
  CHECK(!memcmp(file + 37, "IDAT", 4) && 41 + (size_t)idat + 4 + 12 == size);
  CHECK(be32(file + 41 + idat) == cam_crc32(0, file + 37, idat + 4));
  CHECK(be32(file + size - 12) == 0 && !memcmp(file + size - 8, "IEND", 4) && be32(file + size - 4) == cam_crc32(0, file + size - 8, 4));
  // The zlib stream: stored blocks that add up to the filtered rows, and its Adler-32.
  const uint8_t *z = file + 41;
  size_t at = 2, have = 0;
  bool last = false, sane = z[0] == 0x78 && (z[0] * 256 + z[1]) % 31 == 0;
  while (sane && !last && at + 5 <= idat) {
    unsigned n = z[at + 1] | (unsigned)z[at + 2] << 8, inverse = z[at + 3] | (unsigned)z[at + 4] << 8;
    last = z[at] == 1;
    sane = z[at] <= 1 && (n ^ inverse) == 0xffff && have + n <= sizeof raw && at + 5 + n <= idat;
    if (sane) memcpy(raw + have, z + at + 5, n);
    have += n; at += 5 + n;
  }
  CHECK(sane && last && have == sizeof raw && at + 4 == idat);
  uint32_t a = 1, b = 0;
  bool rows = true;
  for (size_t i = 0; i < have; i++) { a = (a + raw[i]) % 65521; b = (b + a) % 65521; }
  CHECK(be32(z + at) == (b << 16 | a));
  for (int r = 0; r < H; r++) if (raw[(size_t)r * (W * 3 + 1)] != 0 || memcmp(raw + (size_t)r * (W * 3 + 1) + 1, rgb + (size_t)r * W * 3, W * 3)) rows = false;
  CHECK(rows);
  CHECK(cam_png_write("/nonexistent-dir/x.png", rgb, W, H) == -ENOENT);
  CHECK(cam_png_write(path, rgb, 0, H) == -EINVAL);
}

// ---- camera metadata -----------------------------------------------------------------------------
typedef struct { uint8_t buffer[2048]; uint32_t entries, data; } meta_builder;
static void meta_add(meta_builder *m, uint32_t tag, uint8_t type, uint32_t count, const void *values)
{
  static const uint8_t sizes[6] = { 1, 4, 4, 8, 8, 8 };
  uint32_t bytes = count * sizes[type];
  uint8_t *entry = m->buffer + 48 + m->entries * 16;
  memcpy(entry, &tag, 4); memcpy(entry + 4, &count, 4);
  entry[12] = type;
  if (bytes <= 4) memcpy(entry + 8, values, bytes);
  else { memcpy(entry + 8, &m->data, 4); memcpy(m->buffer + 48 + 16 * 16 + m->data, values, bytes); m->data += (bytes + 7) & ~7u; }
  m->entries++;
}
static size_t meta_finish(meta_builder *m)
{
  uint32_t header[12] = { 48 + 16 * 16 + m->data, 1, 0, m->entries, 16, 48, m->data, 1024, 48 + 16 * 16, 0, 0, 0 };
  memcpy(m->buffer, header, 48);
  return header[0];
}

static void test_meta(void)
{
  meta_builder m;
  memset(&m, 0, sizeof m);
  uint8_t facing = 1, capabilities[3] = { 0, 11, 3 }, buffers = 0;
  int32_t orientation = 90, pixels[2] = { 16320, 12240 }, fps[8] = { 15, 30, 30, 30, 60, 60, 7, 24 }, target[2] = { 15, 30 };
  int32_t streams[] = { 0x21, 4000, 3000, 0, 0x23, 4000, 3000, 0, 0x23, 1920, 1080, 0, 0x23, 1280, 720, 0, 0x23, 1920, 1080, 1, 0x22, 640, 480, 0, 0x23, 640, 480, 0, 0x23, 1440, 1080, 0 };
  float focal = 6.3f, physical[2] = { 9.83f, 7.37f };
  meta_add(&m, CAM_TAG_LENS_FACING, CAM_META_BYTE, 1, &facing);
  meta_add(&m, CAM_TAG_SENSOR_ORIENTATION, CAM_META_INT32, 1, &orientation);
  meta_add(&m, CAM_TAG_FOCAL_LENGTHS, CAM_META_FLOAT, 1, &focal);
  meta_add(&m, CAM_TAG_PHYSICAL_SIZE, CAM_META_FLOAT, 2, physical);
  meta_add(&m, CAM_TAG_PIXEL_ARRAY_SIZE, CAM_META_INT32, 2, pixels);
  meta_add(&m, CAM_TAG_CAPABILITIES, CAM_META_BYTE, 3, capabilities);
  meta_add(&m, CAM_TAG_STREAM_CONFIGURATIONS, CAM_META_INT32, sizeof streams / 4, streams);
  meta_add(&m, CAM_TAG_AE_AVAILABLE_FPS_RANGES, CAM_META_INT32, 8, fps);
  meta_add(&m, CAM_TAG_AE_TARGET_FPS_RANGE, CAM_META_INT32, 2, target);
  meta_add(&m, CAM_TAG_BUFFER_MANAGEMENT, CAM_META_BYTE, 1, &buffers);
  size_t size = meta_finish(&m);

  cam_meta_entry e;
  CHECK(cam_meta_find(m.buffer, size, CAM_TAG_LENS_FACING, &e) == 0 && e.count == 1 && e.type == CAM_META_BYTE && e.data[0] == 1);
  CHECK(cam_meta_find(m.buffer, size, CAM_TAG_PIXEL_ARRAY_SIZE, &e) == 0 && e.count == 2 && !memcmp(e.data, pixels, 8));
  CHECK(cam_meta_find(m.buffer, size, 0x12345, &e) == -ENOENT);
  cam_info info;
  memset(&info, 0, sizeof info);
  cam_info_from_meta(&info, m.buffer, size);
  CHECK(info.facing == OC_FACING_BACK && info.orientation == 90 && info.logical == 1 && info.hal_buffers == 1 && info.pixel_w == 16320);
  CHECK(info.equiv35 > 21 && info.equiv35 < 24);
  CHECK(info.nsizes == 5 && info.sizes[0].w == 4000 && info.sizes[4].w == 1440 && info.nfps == 4 && info.fps[2].hi == 60);
  int w, h, lo, hi;
  CHECK(cam_pick_size(&info, 1280, 720, &w, &h) == 0 && w == 1280 && h == 720);
  CHECK(cam_pick_size(&info, 1600, 900, &w, &h) == 0 && w == 1280 && h == 720);       // the same shape, not larger
  CHECK(cam_pick_size(&info, 800, 800, &w, &h) == 0 && w == 1920 && h == 1080);       // no such shape: the largest that fits
  CHECK(cam_pick_size(&info, 4000, 3000, &w, &h) == 0 && w == 1440 && h == 1080);     // never above the transport's limit: the same shape below it
  CHECK(cam_pick_fps(&info, 30, &lo, &hi) == 0 && lo == 30 && hi == 30);
  CHECK(cam_pick_fps(&info, 60, &lo, &hi) == 0 && lo == 60 && hi == 60);
  CHECK(cam_pick_fps(&info, 25, &lo, &hi) == 0 && hi == 24);
  struct oc_camera wire;
  info.role = OC_ROLE_WIDE;
  cam_info_to_wire(&info, &wire);
  CHECK(wire.role == OC_ROLE_WIDE && wire.max_width == 1920 && wire.max_height == 1080 && wire.max_fps == 60 && wire.focal_um == 6300 && wire.orientation == 90);

  // The frame rate goes into a template in place.
  int32_t fixed[2] = { 30, 30 };
  CHECK(cam_meta_set_i32(m.buffer, size, CAM_TAG_AE_TARGET_FPS_RANGE, fixed, 2) == 0);
  CHECK(cam_meta_find(m.buffer, size, CAM_TAG_AE_TARGET_FPS_RANGE, &e) == 0 && !memcmp(e.data, fixed, 8));
  CHECK(cam_meta_set_i32(m.buffer, size, CAM_TAG_AE_TARGET_FPS_RANGE, fixed, 1) == -EINVAL);
  CHECK(cam_meta_set_i32(m.buffer, size, CAM_TAG_LENS_FACING, fixed, 1) == -EINVAL);
  CHECK(cam_meta_set_i32(m.buffer, size, 0x999, fixed, 2) == -ENOENT);

  // Broken buffers are refused, not read past: every truncation, and offsets and counts out of range.
  bool safe = true;
  for (size_t cut = 0; cut < size; cut += 3) {
    uint8_t *copy = malloc(cut ? cut : 1);
    memcpy(copy, m.buffer, cut);
    if (cam_meta_find(copy, cut, CAM_TAG_STREAM_CONFIGURATIONS, &e) != -EINVAL) safe = false;
    free(copy);
  }
  CHECK(safe);
  uint8_t broken[sizeof m.buffer];
  uint32_t huge = 0x7fffffff;
  memcpy(broken, m.buffer, size); memcpy(broken + 48 + 6 * 16 + 8, &huge, 4);         // the stream entry's offset
  CHECK(cam_meta_find(broken, size, CAM_TAG_STREAM_CONFIGURATIONS, &e) == -EINVAL);
  memcpy(broken, m.buffer, size); memcpy(broken + 48 + 6 * 16 + 4, &huge, 4);         // its count
  CHECK(cam_meta_find(broken, size, CAM_TAG_STREAM_CONFIGURATIONS, &e) == -EINVAL);
  memcpy(broken, m.buffer, size); memcpy(broken + 12, &huge, 4);                      // the entry count
  CHECK(cam_meta_find(broken, size, CAM_TAG_LENS_FACING, &e) == -EINVAL);
  memcpy(broken, m.buffer, size); broken[48 + 12] = 9;                                // a type that does not exist
  CHECK(cam_meta_find(broken, size, CAM_TAG_LENS_FACING, &e) == -EINVAL);

  // Roles: the S24 Ultra's lenses, a second wide one, a logical camera listed first, an external one.
  cam_info list[8];
  memset(list, 0, sizeof list);
  static const struct { int facing, logical; float focal, pw, ph; uint32_t role; } lenses[8] = {
    { OC_FACING_BACK, 1, 6.3f, 9.83f, 7.37f, OC_ROLE_OTHER }, { OC_FACING_BACK, 0, 6.3f, 9.83f, 7.37f, OC_ROLE_WIDE }, { OC_FACING_FRONT, 0, 3.3f, 4.5f, 3.4f, OC_ROLE_FRONT },
    { OC_FACING_BACK, 0, 2.2f, 5.9f, 4.4f, OC_ROLE_ULTRAWIDE }, { OC_FACING_BACK, 0, 7.9f, 4.1f, 3.1f, OC_ROLE_TELE3 }, { OC_FACING_BACK, 0, 18.6f, 5.8f, 4.35f, OC_ROLE_TELE5 },
    { OC_FACING_BACK, 0, 6.3f, 9.83f, 7.37f, OC_ROLE_OTHER }, { OC_FACING_EXTERNAL, 0, 4.0f, 5.0f, 4.0f, OC_ROLE_OTHER },
  };
  for (int i = 0; i < 8; i++) { list[i].facing = lenses[i].facing; list[i].logical = lenses[i].logical; list[i].focal_mm = lenses[i].focal; list[i].physical_w = lenses[i].pw; list[i].physical_h = lenses[i].ph; cam_info_finish(&list[i]); }
  cam_assign_roles(list, 8);
  bool roles = true;
  for (int i = 0; i < 8; i++) if (list[i].role != lenses[i].role) { roles = false; printf("  camera %d: role %s\n", i, cam_role_name(list[i].role)); }
  CHECK(roles);
  // Only a logical rear camera: it is the wide one.
  memset(list, 0, sizeof list);
  list[0].facing = OC_FACING_BACK; list[0].logical = 1;
  cam_assign_roles(list, 1);
  CHECK(list[0].role == OC_ROLE_WIDE);
  // The phone's list by cameraserver (2026-11-11): logical 0, front 1, ultra-wide 2, a second front 3; then
  // logical 0's physical cameras that are not listed themselves, in any order (a wide one too).
  memset(list, 0, sizeof list);
  static const struct { const char *id, *parent; int facing, logical; float focal, pw, ph; uint32_t role; } phone[7] = {
    { "0", "", OC_FACING_BACK, 1, 6.3f, 9.83f, 7.37f, OC_ROLE_WIDE }, { "1", "", OC_FACING_FRONT, 0, 3.3f, 4.5f, 3.4f, OC_ROLE_FRONT },
    { "2", "", OC_FACING_BACK, 0, 2.2f, 5.9f, 4.4f, OC_ROLE_ULTRAWIDE }, { "3", "", OC_FACING_FRONT, 0, 3.3f, 3.8f, 2.9f, OC_ROLE_OTHER },
    { "58", "0", OC_FACING_BACK, 0, 18.6f, 5.8f, 4.35f, OC_ROLE_TELE5 }, { "5", "0", OC_FACING_BACK, 0, 6.3f, 9.83f, 7.37f, OC_ROLE_OTHER },
    { "52", "0", OC_FACING_BACK, 0, 7.9f, 4.1f, 3.1f, OC_ROLE_TELE3 },
  };
  for (int i = 0; i < 7; i++) {
    snprintf(list[i].id, sizeof list[i].id, "%s", phone[i].id); snprintf(list[i].parent, sizeof list[i].parent, "%s", phone[i].parent);
    list[i].facing = phone[i].facing; list[i].logical = phone[i].logical; list[i].focal_mm = phone[i].focal; list[i].physical_w = phone[i].pw; list[i].physical_h = phone[i].ph;
    cam_info_finish(&list[i]);
  }
  cam_assign_roles(list, 7);
  roles = true;
  for (int i = 0; i < 7; i++) if (list[i].role != phone[i].role) { roles = false; printf("  phone camera %s: role %s\n", list[i].id, cam_role_name(list[i].role)); }
  CHECK(roles);                                     // the listed logical camera stays the wide one; the telephotos come from its physical cameras
  CHECK(cam_role_from_name("tele5") == OC_ROLE_TELE5 && cam_role_from_name("other") == OC_ROLE_NONE && !strcmp(cam_role_name(OC_ROLE_ULTRAWIDE), "ultrawide"));
}

// ---- the core with the test back end -------------------------------------------------------------
static uint64_t core_notes;
static void core_note(void *ctx, uint64_t sequence) { (void)ctx; __atomic_store_n(&core_notes, sequence, __ATOMIC_RELEASE); }

// True when the picture is the test back end's for some frame number (its bar moves 8 pixels a frame).
static bool is_test_picture(const struct oc_frame *f, const uint8_t *nv12)
{
  static uint8_t y[OC_FRAME_BYTES_MAX];
  size_t luma = (size_t)f->width * f->height;
  int marker = -1;
  for (uint32_t x = 0; x < f->width; x++) if (nv12[x] == 255) { marker = (int)x; break; }
  if (marker < 0 || marker % 8 || f->bytes != luma * 3 / 2) return false;
  cam_test_picture(y, y + luma, y + luma + luma / 4, (int)f->width, (int)f->height, (uint64_t)marker / 8);
  if (memcmp(y, nv12, luma)) return false;
  for (size_t i = 0; i < luma / 4; i++) if (nv12[luma + 2 * i] != y[luma + i] || nv12[luma + 2 * i + 1] != y[luma + luma / 4 + i]) return false;
  return true;
}

static void test_core(void)
{
  static cam_core core;
  static uint8_t picture[OC_FRAME_BYTES_MAX];
  core.on_frame = core_note;
  CHECK(cam_core_init(&core, "nonsense") == -EINVAL);
  CHECK(cam_core_init(&core, "test") == 0 && core.ncameras == 5);
  CHECK(cam_core_find(&core, "wide") == &core.cameras[0] && cam_core_find(&core, "front") == &core.cameras[1] && cam_core_find(&core, "ultrawide") == &core.cameras[2]);
  CHECK(cam_core_find(&core, "tele3") == &core.cameras[3] && cam_core_find(&core, "tele5") == &core.cameras[4] && cam_core_find(&core, "id:2") == &core.cameras[2]);
  CHECK(cam_core_find(&core, "3") == &core.cameras[3] && !cam_core_find(&core, "9") && !cam_core_find(&core, "id:x") && !cam_core_find(&core, ""));
  cam_ring_map reader;
  CHECK(cam_ring_attach(&reader, dup(core.ring.fd)) == 0);
  CHECK(cam_core_start(&core, cam_core_find(&core, "wide"), 1280, 720, 60) == 0 && core.width == 1280 && core.height == 720 && core.fps == 60);
  struct oc_frame f, before = { 0 };
  uint64_t last = 0;
  unsigned got = 0, in_order = 0, pictures = 0;
  for (unsigned i = 0; i < 3000 && got < 12; i++) {
    if (cam_ring_read(&reader, &last, &f, picture, sizeof picture)) { nap(1); continue; }
    got++;
    if (f.sequence > before.sequence && f.timestamp_ns > before.timestamp_ns) in_order++;
    if (f.width == 1280 && f.height == 720 && f.stride == 1280 && f.format == OC_FMT_NV12 && f.role == OC_ROLE_WIDE && f.orientation == 90 && f.fps == 60 && is_test_picture(&f, picture)) pictures++;
    before = f;
  }
  CHECK(got == 12 && in_order == 12 && pictures == 12);
  // 60 frames a second by the frames' own times.
  CHECK(core.frames >= 12 && core.last_ns > core.first_ns);
  double fps = (double)(core.frames - 1) * 1e9 / (double)(core.last_ns - core.first_ns);
  CHECK(fps > 59 && fps < 61);
  cam_core_stop(&core);
  uint64_t stopped = core.sequence;
  nap(60);
  CHECK(core.sequence == stopped && __atomic_load_n(&core_notes, __ATOMIC_ACQUIRE) == stopped);
  // Another camera, a size it does not have: the nearest; the numbering goes on. What the ring holds
  // from before the start is passed over, as xnuhost's end does.
  last = cam_ring_newest(&reader);
  CHECK(cam_core_start(&core, cam_core_find(&core, "front"), 1000, 750, 30) == 0 && core.width == 640 && core.height == 480 && core.fps == 30);
  for (unsigned i = 0; i < 3000 && cam_ring_read(&reader, &last, &f, picture, sizeof picture); i++) nap(1);
  CHECK(f.sequence > stopped && f.width == 640 && f.height == 480 && f.role == OC_ROLE_FRONT && f.orientation == 270 && is_test_picture(&f, picture));
  cam_core_stop(&core);
  cam_core_stop(&core);                                      // twice is harmless
  cam_ring_close(&reader);
}

// ---- the helper process, through xnuhost's end ---------------------------------------------------
static struct { unsigned frames, states; int status; } events;
static void on_event(void *ctx, int kind, int status)
{
  (void)ctx;
  if (kind == BR_CAMERA_FRAME) __atomic_add_fetch(&events.frames, 1, __ATOMIC_SEQ_CST);
  else { __atomic_store_n(&events.status, status, __ATOMIC_SEQ_CST); __atomic_add_fetch(&events.states, 1, __ATOMIC_SEQ_CST); }
}
static bool wait_states(unsigned count)
{
  for (unsigned i = 0; i < 5000 && __atomic_load_n(&events.states, __ATOMIC_SEQ_CST) < count; i++) nap(1);
  return __atomic_load_n(&events.states, __ATOMIC_SEQ_CST) >= count;
}

static void test_helper(const char *helper)
{
  static uint8_t picture[OC_FRAME_BYTES_MAX];
  struct oc_camera list[OC_MAX_CAMERAS];
  struct oc_frame f;
  CHECK(br_camera_helper("/nonexistent/oneios-camera", "test") == NULL);
  br_camera *camera = br_camera_helper(helper, "test");
  CHECK(camera != NULL);
  if (!camera) return;
  br_camera_listen(camera, on_event, NULL);
  int count = br_camera_list(camera, list, (int)OC_MAX_CAMERAS, 5000);
  CHECK(count == 5 && list[0].role == OC_ROLE_WIDE && list[1].role == OC_ROLE_FRONT && list[1].facing == OC_FACING_FRONT && list[2].role == OC_ROLE_ULTRAWIDE);
  CHECK(list[3].role == OC_ROLE_TELE3 && list[4].role == OC_ROLE_TELE5 && list[0].max_width == 1920 && list[0].max_height == 1080 && list[0].max_fps == 60);
  CHECK(br_camera_frame(camera, &f, picture, sizeof picture) == -EAGAIN);
  // Start: the state event, then frames with their notes.
  CHECK(br_camera_start(camera, OC_ROLE_ULTRAWIDE, 640, 480, 60) == 0);
  CHECK(wait_states(1) && events.status == 0);
  unsigned got = 0, good = 0;
  uint64_t sequence = 0;
  for (unsigned i = 0; i < 4000 && got < 20; i++) {
    if (br_camera_frame(camera, &f, picture, sizeof picture)) { nap(1); continue; }
    got++;
    if (f.sequence > sequence && f.width == 640 && f.height == 480 && f.role == OC_ROLE_ULTRAWIDE && f.fps == 60 && is_test_picture(&f, picture)) good++;
    sequence = f.sequence;
  }
  CHECK(got == 20 && good == 20 && __atomic_load_n(&events.frames, __ATOMIC_SEQ_CST) >= 20);
  CHECK(br_camera_frame(camera, &f, picture, 10) != 0);      // too small a buffer, or nothing new: never a short copy
  // Stop: its state event, and no more frames.
  CHECK(br_camera_stop(camera) == 0 && wait_states(2) && events.status == 0);
  while (br_camera_frame(camera, &f, picture, sizeof picture) == 0) sequence = f.sequence;
  nap(80);
  CHECK(br_camera_frame(camera, &f, picture, sizeof picture) == -EAGAIN);
  // A camera that is not there: the error comes as a state event.
  CHECK(br_camera_start(camera, OC_ROLE_OTHER, 640, 480, 30) == 0 && wait_states(3) && events.status == ENODEV);
  // A start after a stop does not hand out the old run's last frame.
  CHECK(br_camera_start(camera, OC_ROLE_WIDE, 320, 240, 30) == 0 && wait_states(4) && events.status == 0);
  for (unsigned i = 0; i < 4000 && br_camera_frame(camera, &f, picture, sizeof picture); i++) nap(1);
  CHECK(f.sequence > sequence && f.width == 320 && f.role == OC_ROLE_WIDE);
  br_camera_close(camera);                                   // with the camera running: the helper stops it and exits

  // A helper that dies: the bridge says so and stays usable (no frames).
  camera = br_camera_helper(helper, "test");
  CHECK(camera != NULL);
  if (!camera) return;
  br_camera_listen(camera, on_event, NULL);
  CHECK(br_camera_list(camera, list, (int)OC_MAX_CAMERAS, 5000) == 5);
  unsigned states = __atomic_load_n(&events.states, __ATOMIC_SEQ_CST);
  CHECK(system("pkill -KILL -P $PPID -x oneios-camera") == 0);
  CHECK(wait_states(states + 1) && events.status == EPIPE);
  CHECK(br_camera_start(camera, OC_ROLE_WIDE, 320, 240, 30) == -EPIPE);
  CHECK(br_camera_frame(camera, &f, picture, sizeof picture) == -EAGAIN);
  br_camera_close(camera);
}

// ---- the virtio device against a guest of plain memory -------------------------------------------
#define QN 16u
typedef struct { uint64_t addr; uint32_t len; uint16_t flags, next; } vdesc;
typedef struct {
  vdesc desc[QN];
  struct { uint16_t flags, idx, ring[QN]; } avail;
  struct { uint16_t flags, idx; struct { uint32_t id, len; } ring[QN]; } used;
  uint16_t next_desc, seen_used;
} gqueue;
typedef struct { xh_virtio *dev; gqueue q[2]; } guest;
static void w32(guest *g, uint64_t off, uint32_t v) { xh_virtio_mmio_write(g->dev, off, 4, v); }
static uint32_t r32(guest *g, uint64_t off) { return (uint32_t)xh_virtio_mmio_read(g->dev, off, 4); }

static void guest_up(guest *g, xh_virtio *dev)
{
  memset(g, 0, sizeof *g);
  g->dev = dev;
  w32(g, 0x70, 0); w32(g, 0x70, 1); w32(g, 0x70, 3);
  w32(g, 0x24, 0); w32(g, 0x20, 0); w32(g, 0x24, 1); w32(g, 0x20, 1);
  w32(g, 0x70, 11);
  CHECK(r32(g, 0x70) & 8);
  for (unsigned q = 0; q < 2; q++) {
    w32(g, 0x30, q); w32(g, 0x38, QN);
    uint64_t d = (uint64_t)(uintptr_t)g->q[q].desc, a = (uint64_t)(uintptr_t)&g->q[q].avail, u = (uint64_t)(uintptr_t)&g->q[q].used;
    w32(g, 0x80, (uint32_t)d); w32(g, 0x84, (uint32_t)(d >> 32));
    w32(g, 0x90, (uint32_t)a); w32(g, 0x94, (uint32_t)(a >> 32));
    w32(g, 0xa0, (uint32_t)u); w32(g, 0xa4, (uint32_t)(u >> 32));
    w32(g, 0x44, 1);
  }
  w32(g, 0x70, 15);
}
static void post(guest *g, unsigned q, void **bufs, const uint32_t *lens, const bool *writable, unsigned n)
{
  gqueue *gq = &g->q[q];
  uint16_t head = gq->next_desc;
  for (unsigned i = 0; i < n; i++) {
    uint16_t at = (uint16_t)((gq->next_desc + i) % QN);
    gq->desc[at] = (vdesc){ (uint64_t)(uintptr_t)bufs[i], lens[i], (uint16_t)((writable[i] ? 2 : 0) | (i + 1 < n ? 1 : 0)), (uint16_t)((at + 1) % QN) };
  }
  gq->next_desc = (uint16_t)((gq->next_desc + n) % QN);
  gq->avail.ring[gq->avail.idx % QN] = head;
  __atomic_add_fetch(&gq->avail.idx, 1, __ATOMIC_SEQ_CST);
  w32(g, 0x50, q);
}
// The next used element of q: its length, or -1. *id is the chain's first descriptor.
static int reap(guest *g, unsigned q, uint32_t *id)
{
  gqueue *gq = &g->q[q];
  if (__atomic_load_n(&gq->used.idx, __ATOMIC_SEQ_CST) == gq->seen_used) return -1;
  if (id) *id = gq->used.ring[gq->seen_used % QN].id;
  return (int)gq->used.ring[gq->seen_used++ % QN].len;
}
static int command(guest *g, struct oc_request *request, struct oc_reply *reply)
{
  void *bufs[2] = { request, reply };
  uint32_t lens[2] = { sizeof *request, sizeof *reply };
  bool writable[2] = { false, true };
  memset(reply, 0xee, sizeof *reply);
  post(g, OC_QUEUE_CONTROL, bufs, lens, writable, 2);
  return reap(g, OC_QUEUE_CONTROL, NULL);
}
static void post_frame(guest *g, uint8_t *buffer, uint32_t length)
{
  void *bufs[1] = { buffer };
  bool writable[1] = { true };
  post(g, OC_QUEUE_FRAME, bufs, &length, writable, 1);
}
static int wait_frame(guest *g, uint32_t *id)
{
  for (unsigned i = 0; i < 5000; i++) { int n = reap(g, OC_QUEUE_FRAME, id); if (n >= 0) return n; nap(1); }
  return -1;
}

static void test_device(const char *helper)
{
  static guest g;
  struct oc_request request = { .code = OC_CMD_INFO };
  struct oc_reply reply;
  struct oc_frame f;
  uint8_t *buffers[3], small[100];
  br_camera *camera = br_camera_helper(helper, "test");
  CHECK(camera != NULL && br_camera_create(0x0a000000, 40, NULL) == NULL);
  if (!camera) return;
  struct oc_camera list[OC_MAX_CAMERAS];
  CHECK(br_camera_list(camera, list, (int)OC_MAX_CAMERAS, 5000) == 5);
  xh_virtio *dev = br_camera_create(0x0a000000, 40, camera);
  CHECK(dev != NULL);
  if (!dev) return;
  CHECK(xh_virtio_mmio_read(dev, 0x08, 4) == OC_VIRTIO_DEVICE_ID && xh_virtio_mmio_read(dev, 0x100, 4) == OC_PROTO_VERSION);
  guest_up(&g, dev);
  for (unsigned i = 0; i < 3; i++) buffers[i] = malloc(OC_BUFFER_BYTES);

  CHECK(command(&g, &request, &reply) == 8 + 5 * (int)sizeof(struct oc_camera) && reply.status == 0 && reply.count == 5);
  CHECK(reply.cameras[1].role == OC_ROLE_FRONT && reply.cameras[4].role == OC_ROLE_TELE5 && reply.cameras[0].max_width == 1920);
  request = (struct oc_request){ .code = 99 };
  CHECK(command(&g, &request, &reply) == 8 && reply.status == ENOTSUP);
  request = (struct oc_request){ .code = OC_CMD_START, .role = OC_ROLE_WIDE, .width = 4000, .height = 3000 };
  CHECK(command(&g, &request, &reply) == 8 && reply.status == EINVAL);

  // Start with three buffers posted: frames arrive in them, in order, each the test picture.
  for (unsigned i = 0; i < 3; i++) post_frame(&g, buffers[i], OC_BUFFER_BYTES);
  request = (struct oc_request){ .code = OC_CMD_START, .role = OC_ROLE_TELE3, .width = 1280, .height = 720, .fps = 30 };
  CHECK(command(&g, &request, &reply) == 8 && reply.status == 0);
  unsigned good = 0;
  uint64_t sequence = 0;
  for (unsigned i = 0; i < 10; i++) {
    uint32_t id = 99;
    int n = wait_frame(&g, &id);
    if (n < (int)OC_FRAME_DATA) break;
    uint8_t *buffer = (uint8_t *)(uintptr_t)g.q[OC_QUEUE_FRAME].desc[id % QN].addr;
    memcpy(&f, buffer, sizeof f);
    if (f.magic == OC_FRAME_MAGIC && f.status == 0 && f.sequence > sequence && f.width == 1280 && f.height == 720 && f.stride == 1280 && f.format == OC_FMT_NV12 &&
        f.bytes == 1280 * 720 * 3 / 2 && n == (int)(OC_FRAME_DATA + f.bytes) && f.role == OC_ROLE_TELE3 && f.fps == 30 && f.timestamp_ns && is_test_picture(&f, buffer + OC_FRAME_DATA)) good++;
    sequence = f.sequence;
    post_frame(&g, buffer, OC_BUFFER_BYTES);                 // the guest gives the buffer back
  }
  CHECK(good == 10);
  // A buffer too small for a picture comes back with the error, not a cut picture.
  for (unsigned i = 0; i < 3; i++) (void)wait_frame(&g, NULL);
  memset(small, 0, sizeof small);
  post_frame(&g, small, sizeof small);
  CHECK(wait_frame(&g, NULL) == (int)OC_FRAME_DATA);
  memcpy(&f, small, sizeof f);
  CHECK(f.magic == OC_FRAME_MAGIC && f.status == EMSGSIZE && f.bytes == 0 && small[OC_FRAME_DATA] == 0);
  // Stop: buffers posted afterwards stay with the device.
  request = (struct oc_request){ .code = OC_CMD_STOP };
  CHECK(command(&g, &request, &reply) == 8 && reply.status == 0);
  nap(150);
  while (reap(&g, OC_QUEUE_FRAME, NULL) >= 0) {}
  post_frame(&g, buffers[0], OC_BUFFER_BYTES);
  nap(150);
  CHECK(reap(&g, OC_QUEUE_FRAME, NULL) == -1);
  // A camera that is not there: the posted buffer comes back with the errno.
  request = (struct oc_request){ .code = OC_CMD_START, .role = OC_ROLE_OTHER, .width = 640, .height = 480 };
  CHECK(command(&g, &request, &reply) == 8 && reply.status == 0);
  CHECK(wait_frame(&g, NULL) == (int)OC_FRAME_DATA);
  memcpy(&f, buffers[0], sizeof f);
  CHECK(f.status == ENODEV && f.bytes == 0);
  // A guest that resets the device with the camera on: the camera is stopped.
  request = (struct oc_request){ .code = OC_CMD_START, .role = OC_ROLE_FRONT, .width = 640, .height = 480, .fps = 60 };
  post_frame(&g, buffers[1], OC_BUFFER_BYTES);
  CHECK(command(&g, &request, &reply) == 8 && reply.status == 0 && wait_frame(&g, NULL) > (int)OC_FRAME_DATA);
  w32(&g, 0x70, 0);
  nap(150);
  struct oc_frame none;
  static uint8_t picture[OC_FRAME_BYTES_MAX];
  while (br_camera_frame(camera, &none, picture, sizeof picture) == 0) {}
  nap(100);
  CHECK(br_camera_frame(camera, &none, picture, sizeof picture) == -EAGAIN);
  for (unsigned i = 0; i < 3; i++) free(buffers[i]);
  // The device lives as long as xnuhost; the helper goes with its socket.
}

int main(int argc, char **argv)
{
  char tmp[] = "/tmp/oneios-camera-test-XXXXXX";
  if (argc < 2) { fprintf(stderr, "usage: test_camera PATH-TO-HOST-oneios-camera\n"); return 2; }
  if (!mkdtemp(tmp)) return 2;
  signal(SIGPIPE, SIG_IGN);
  test_ring();
  test_convert();
  test_png(tmp);
  test_meta();
  test_core();
  test_helper(argv[1]);
  test_device(argv[1]);
  char cmd[100];
  snprintf(cmd, sizeof cmd, "rm -rf '%s'", tmp);
  if (system(cmd)) failures++;
  printf("camera: %d checks, %d failed\n", checks, failures);
  return failures ? 1 : 0;
}
