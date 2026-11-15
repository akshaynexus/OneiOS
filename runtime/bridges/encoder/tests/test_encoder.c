// SPDX-License-Identifier: MIT
// Host test of the video encoder bridge: no phone, no Android, no Apple files. It drives xnuhost's "droidvm,virtio-
// media" device (media_device.c) over xnuhost's real transport (runtime/xnuhost/src/virtio.c) with a guest made of
// plain memory, replaying exactly the V4L2 encoder sequence UniXNU-droidvm's DroidVMVTHost.cpp issues, and checks the
// message layout, the pool MMAP, and that the NV12 test pattern reaches the stand-in encoder and comes back as an
// HEVC Annex-B access unit (VPS, SPS, PPS, IDR) that holds the pattern's own samples (tests/standin_reader.h). Twice:
// through the oneios-encoder helper process (the phone's arrangement, with the stand-in in MediaCodec's place), and
// with the stand-in inside the device's process (xnuhost's encoder=sim and its PC boots).
//
//   test_encoder PATH-TO-HOST-oneios-encoder
#define _GNU_SOURCE
#include "encoder.h"
#include "media_proto.h"
#include "standin_reader.h"
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static int failures, checks;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

// ---- what xnuhost supplies around virtio.c -------------------------------------------------------
void xh_gic_set_spi(unsigned intid, bool level) { (void)intid; (void)level; }
int xh_mmio_add(const xh_mmio_region *region) { (void)region; return 0; }
void *xh_ram(uint64_t pa, uint64_t len) { (void)len; return (void *)(uintptr_t)pa; }
void (*br_log_sink)(int level, const char *line);
void br_log(int level, const char *fmt, ...) { (void)level; (void)fmt; }

static void nap(unsigned ms) { struct timespec ts = { 0, (long)ms * 1000000L }; nanosleep(&ts, NULL); }

// ---- the virtio device against a guest of plain memory -------------------------------------------
#define QN 32u
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

static void guest_up(guest *g, xh_virtio *dev) {
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

static void post(guest *g, unsigned q, void **bufs, const uint32_t *lens, const bool *writable, unsigned n) {
  gqueue *gq = &g->q[q];
  uint16_t head = gq->next_desc;
  for (unsigned i = 0; i < n; i++) {
    uint16_t at = (uint16_t)((gq->next_desc + i) % QN);
    gq->desc[at] = (vdesc){ (uint64_t)(uintptr_t)bufs[i], lens[i],
                            (uint16_t)((writable[i] ? 2 : 0) | (i + 1 < n ? 1 : 0)), (uint16_t)((at + 1) % QN) };
  }
  gq->next_desc = (uint16_t)((gq->next_desc + n) % QN);
  gq->avail.ring[gq->avail.idx % QN] = head;
  __atomic_add_fetch(&gq->avail.idx, 1, __ATOMIC_SEQ_CST);
  w32(g, 0x50, q);
}

static int reap(guest *g, unsigned q, uint32_t *id) {
  gqueue *gq = &g->q[q];
  if (__atomic_load_n(&gq->used.idx, __ATOMIC_SEQ_CST) == gq->seen_used) return -1;
  if (id) *id = gq->used.ring[gq->seen_used % QN].id;
  return (int)gq->used.ring[gq->seen_used++ % QN].len;
}

// A command: cmd words (read), optional out (read), a response (write), optional in (write). Returns response status.
static uint32_t command(guest *g, const void *cmd, uint32_t cmd_len, void *out, uint32_t out_len,
                        uint8_t *resp, uint32_t resp_len, void *in, uint32_t in_len) {
  void *bufs[4]; uint32_t lens[4]; bool wr[4];
  unsigned n = 0;
  bufs[n] = (void *)cmd; lens[n] = cmd_len; wr[n] = false; n++;
  if (out) { bufs[n] = out; lens[n] = out_len; wr[n] = false; n++; }
  bufs[n] = resp; lens[n] = resp_len; wr[n] = true; n++;
  if (in) { bufs[n] = in; lens[n] = in_len; wr[n] = true; n++; }
  memset(resp, 0xee, resp_len);
  post(g, MEDIA_QUEUE_COMMAND, bufs, lens, wr, n);
  CHECK(reap(g, MEDIA_QUEUE_COMMAND, NULL) >= 0);
  uint32_t status; memcpy(&status, resp, 4);
  return status;
}

static uint32_t ioctl_cmd(guest *g, uint32_t session, uint32_t nr, void *payload, uint32_t len) {
  uint32_t cmd[4] = { MEDIA_CMD_IOCTL, 0, session, nr };
  uint8_t resp[8];
  return command(g, cmd, sizeof cmd, payload, len, resp, sizeof resp, payload, len);
}

// MMAP exactly as the guest's transport does it (DroidVMVirtioMedia.c droidvm_media_mmap): the answer is the buffer's
// place in the media pool and its size; one that is not inside the pool is refused; the address is the pool's base
// (the device tree's media-host-pool) plus it.
static uint64_t guest_pool_base, guest_pool_size;
static uint64_t mmap_buf(guest *g, uint32_t session, uint64_t handle) {
  uint32_t cmd[5] = { MEDIA_CMD_MMAP, 0, session, 1, (uint32_t)handle };
  uint8_t resp[24];
  uint32_t status = command(g, cmd, sizeof cmd, NULL, 0, resp, sizeof resp, NULL, 0);
  CHECK(status == 0);
  uint64_t at, size;
  memcpy(&at, resp + 8, 8);
  memcpy(&size, resp + 16, 8);
  CHECK(size != 0 && at <= guest_pool_size && size <= guest_pool_size - at);
  if (at > guest_pool_size || size > guest_pool_size - at) return 0;
  return guest_pool_base + at;
}

// CLOSE exactly as the guest's transport sends it (droidvm_media_close): the command alone, with no buffer for an
// answer; the device takes it and gives the descriptor back.
static void close_session(guest *g, uint32_t session) {
  uint32_t cmd[4] = { MEDIA_CMD_CLOSE, 0, session, 0 };
  void *buf = cmd; uint32_t len = sizeof cmd; bool writable = false;
  post(g, MEDIA_QUEUE_COMMAND, &buf, &len, &writable, 1);
  CHECK(reap(g, MEDIA_QUEUE_COMMAND, NULL) == 0);
}

// Posts EVENT_BUFS writable event buffers; the worker fills them.
#define EVENT_BUFS 8
static uint8_t event_store[EVENT_BUFS][MEDIA_EVENT_BYTES];
static void post_events(guest *g) {
  for (unsigned i = 0; i < EVENT_BUFS; i++) {
    void *b = event_store[i]; uint32_t l = MEDIA_EVENT_BYTES; bool w = true;
    post(g, MEDIA_QUEUE_EVENT, &b, &l, &w, 1);
  }
}

// Waits for one event; returns its first descriptor's buffer, or NULL on timeout.
static uint8_t *wait_event(guest *g, uint32_t *kind, uint32_t *session) {
  for (unsigned i = 0; i < 3000; i++) {
    uint32_t id;
    int len = reap(g, MEDIA_QUEUE_EVENT, &id);
    if (len >= 8) {
      uint8_t *buf = (uint8_t *)(uintptr_t)g->q[MEDIA_QUEUE_EVENT].desc[id % QN].addr;
      memcpy(kind, buf, 4);
      memcpy(session, buf + 4, 4);
      return buf;
    }
    nap(1);
  }
  return NULL;
}

// One session of one frame on a device over `backend`, as DroidVMVTHost.cpp runs it.
static void run(const br_encoder_backend *backend, uint64_t base) {
  static guest g;
  uint64_t pool_size = 32ull << 20;
  uint8_t *pool = malloc(pool_size);
  CHECK(pool != NULL);
  uint64_t pool_gpa = (uint64_t)(uintptr_t)pool;      // identity, as xh_ram here
  guest_pool_base = pool_gpa;
  guest_pool_size = pool_size;

  xh_virtio *dev = br_encoder_create(base, 40, backend, pool, pool_gpa, pool_size);
  CHECK(dev != NULL);
  if (!dev) return;

  CHECK(xh_virtio_mmio_read(dev, 0x08, 4) == MEDIA_VIRTIO_DEVICE_ID);
  CHECK(xh_virtio_mmio_read(dev, 0x100, 4) == (MEDIA_CAP_VIDEO_M2M_MPLANE | MEDIA_CAP_STREAMING));
  guest_up(&g, dev);
  post_events(&g);

  const uint32_t W = 256, H = 256;

  // OPEN
  uint32_t open_cmd[2] = { MEDIA_CMD_OPEN, 0 };
  uint8_t open_resp[16];
  CHECK(command(&g, open_cmd, sizeof open_cmd, NULL, 0, open_resp, sizeof open_resp, NULL, 0) == 0);
  uint32_t session; memcpy(&session, open_resp + 8, 4);
  CHECK(session != 0);

  // ENUM_FMT, as the guest's transport lists each queue once at start (DroidVMVirtioMedia.c check()): OUTPUT takes
  // NV12, CAPTURE gives HEVC and H.264, and the list ends with an error.
  struct { uint32_t index, type, flags; char description[32]; uint32_t pixelformat, mbus_code, reserved[3]; } fd;
  static const struct { uint32_t type, index, format; } formats[] = {
    { MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, 0, MEDIA_PIXFMT_NV12 }, { MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, 1, 0 },
    { MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, 0, MEDIA_PIXFMT_HEVC }, { MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, 1, MEDIA_PIXFMT_H264 },
    { MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, 2, 0 } };
  for (unsigned i = 0; i < sizeof formats / sizeof formats[0]; i++) {
    memset(&fd, 0, sizeof fd);
    fd.index = formats[i].index; fd.type = formats[i].type;
    uint32_t status = ioctl_cmd(&g, session, MEDIA_VIDIOC_ENUM_FMT, &fd, sizeof fd);
    CHECK(formats[i].format ? status == 0 && fd.pixelformat == formats[i].format : status != 0);
  }

  // S_FMT CAPTURE (HEVC) then OUTPUT (NV12), as startEncoder does.
  struct media_v4l2_format cap_fmt; memset(&cap_fmt, 0, sizeof cap_fmt);
  cap_fmt.type = MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
  cap_fmt.width = W; cap_fmt.height = H; cap_fmt.pixelformat = MEDIA_PIXFMT_HEVC; cap_fmt.num_planes = 1;
  CHECK(ioctl_cmd(&g, session, MEDIA_VIDIOC_S_FMT, &cap_fmt, sizeof cap_fmt) == 0);

  struct media_v4l2_format out_fmt; memset(&out_fmt, 0, sizeof out_fmt);
  out_fmt.type = MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
  out_fmt.width = W; out_fmt.height = H; out_fmt.pixelformat = MEDIA_PIXFMT_NV12; out_fmt.num_planes = 1;
  CHECK(ioctl_cmd(&g, session, MEDIA_VIDIOC_S_FMT, &out_fmt, sizeof out_fmt) == 0);
  CHECK(out_fmt.planes[0].sizeimage == W * H * 3 / 2);

  // REQBUFS + QUERYBUF + MMAP for OUTPUT.
  uint32_t req_out[5] = { 2, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, MEDIA_MEMORY_MMAP, 0, 0 };
  CHECK(ioctl_cmd(&g, session, MEDIA_VIDIOC_REQBUFS, req_out, sizeof req_out) == 0 && req_out[0] == 2);
  struct media_v4l2_buffer qb; memset(&qb, 0, sizeof qb);
  qb.type = MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE; qb.index = 0;
  CHECK(ioctl_cmd(&g, session, MEDIA_VIDIOC_QUERYBUF, &qb, sizeof qb) == 0);
  uint64_t out_gpa = mmap_buf(&g, session, qb.plane.m);
  uint8_t *out_buf = (uint8_t *)(uintptr_t)out_gpa;
  CHECK(out_buf >= pool && out_buf < pool + pool_size);

  // REQBUFS + QUERYBUF + MMAP + QBUF for CAPTURE.
  uint32_t req_cap[5] = { 4, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, MEDIA_MEMORY_MMAP, 0, 0 };
  CHECK(ioctl_cmd(&g, session, MEDIA_VIDIOC_REQBUFS, req_cap, sizeof req_cap) == 0 && req_cap[0] == 4);
  for (unsigned i = 0; i < 4; i++) {
    struct media_v4l2_buffer cb; memset(&cb, 0, sizeof cb);
    cb.type = MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE; cb.index = i;
    CHECK(ioctl_cmd(&g, session, MEDIA_VIDIOC_QUERYBUF, &cb, sizeof cb) == 0);
    mmap_buf(&g, session, cb.plane.m);
    CHECK(ioctl_cmd(&g, session, MEDIA_VIDIOC_QBUF, &cb, sizeof cb) == 0);
  }

  // STREAMON OUTPUT then CAPTURE: the backend opens.
  uint32_t t = MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
  CHECK(command(&g, (uint32_t[]){ MEDIA_CMD_IOCTL, 0, session, MEDIA_VIDIOC_STREAMON }, 16, &t, 4,
                (uint8_t[8]){ 0 }, 8, NULL, 0) == 0);
  t = MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
  CHECK(command(&g, (uint32_t[]){ MEDIA_CMD_IOCTL, 0, session, MEDIA_VIDIOC_STREAMON }, 16, &t, 4,
                (uint8_t[8]){ 0 }, 8, NULL, 0) == 0);

  // A NV12 test pattern into the OUTPUT buffer, then QBUF it.
  uint32_t frame_len = W * H * 3 / 2;
  for (uint32_t i = 0; i < frame_len; i++) out_buf[i] = (uint8_t)(i * 7 + 13);
  uint8_t *pattern = malloc(frame_len);               // the device may hand the OUTPUT buffer out again
  CHECK(pattern != NULL);
  if (pattern) memcpy(pattern, out_buf, frame_len);
  struct media_v4l2_buffer qbo; memset(&qbo, 0, sizeof qbo);
  qbo.type = MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE; qbo.index = 0; qbo.sec = 1; qbo.plane.bytesused = frame_len;
  CHECK(ioctl_cmd(&g, session, MEDIA_VIDIOC_QBUF, &qbo, sizeof qbo) == 0);

  // The events: a DQBUF of the returned OUTPUT buffer and a DQBUF of the coded CAPTURE buffer.
  bool saw_output = false, saw_capture = false;
  uint8_t *coded = NULL; uint32_t coded_len = 0, coded_index = 0;
  for (unsigned e = 0; e < 4 && !(saw_output && saw_capture); e++) {
    uint32_t kind, sess;
    uint8_t *ev = wait_event(&g, &kind, &sess);
    CHECK(ev != NULL);
    if (!ev) break;
    CHECK(sess == session && kind == MEDIA_EVT_DQBUF);
    struct media_v4l2_buffer b; memcpy(&b, ev + 8, sizeof b);
    if (b.type == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE) saw_output = true;
    else if (b.type == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE) {
      saw_capture = true;
      coded_index = b.index;
      coded_len = b.plane.bytesused;
      CHECK(b.flags & MEDIA_BUF_FLAG_KEYFRAME);
      // Re-MMAP the CAPTURE buffer to read the bitstream (same pool handle).
      struct media_v4l2_buffer cb; memset(&cb, 0, sizeof cb);
      cb.type = MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE; cb.index = coded_index;
      ioctl_cmd(&g, session, MEDIA_VIDIOC_QUERYBUF, &cb, sizeof cb);
      coded = (uint8_t *)(uintptr_t)mmap_buf(&g, session, cb.plane.m);
    }
  }
  CHECK(saw_output && saw_capture && coded && coded_len > 0);

  // The coded access unit: VPS (32), SPS (33), PPS (34), IDR (19), Annex-B, no larger than the coded buffer the guest
  // asks for; the slice's samples are the pattern's (at the PCM depth the stand-in wrote).
  CHECK(coded_len <= W * H / 2 + 65536);
  if (coded && coded_len > 40 && pattern) {
    standin_nal nals[8];
    unsigned count = standin_split(coded, coded_len, true, nals, 8), luma = 0, chroma = 0;
    CHECK(count == 4 && nals[0].type == 32 && nals[1].type == 33 && nals[2].type == 34 && nals[3].type == 19);
    uint32_t n = 0;
    uint8_t *rbsp = count == 4 ? standin_rbsp(&nals[3], true, &n) : NULL, *picture = malloc(frame_len);
    CHECK(rbsp && picture && standin_hevc_picture(rbsp, n, W, H, picture, &luma, &chroma));
    CHECK(rbsp && picture && luma && standin_same_picture(picture, pattern, W, H, luma, chroma));
    free(rbsp);
    free(picture);
  }
  free(pattern);

  // ENCODER_CMD STOP -> drain -> EOS.
  uint32_t enc_cmd[10] = { MEDIA_ENC_CMD_STOP };
  CHECK(ioctl_cmd(&g, session, MEDIA_VIDIOC_ENCODER_CMD, enc_cmd, sizeof enc_cmd) == 0);
  bool saw_eos = false;
  for (unsigned e = 0; e < 6 && !saw_eos; e++) {
    uint32_t kind, sess;
    uint8_t *ev = wait_event(&g, &kind, &sess);
    if (!ev) break;
    if (kind == MEDIA_EVT_EVENT) { uint32_t type; memcpy(&type, ev + 8, 4); if (type == MEDIA_EVENT_EOS) saw_eos = true; }
  }
  CHECK(saw_eos);

  br_encoder_stats st;
  br_encoder_get_stats(dev, &st);
  CHECK(st.sessions == 1 && st.frames_in == 1 && st.frames_out == 1 && st.errors == 0);

  // CLOSE, then more sessions than the device has slots, one after another: each close gives its slot back (the
  // guest's transport opens and closes one at start, VideoToolbox one for every picture).
  close_session(&g, session);
  for (unsigned i = 0; i < 5; i++) {
    CHECK(command(&g, open_cmd, sizeof open_cmd, NULL, 0, open_resp, sizeof open_resp, NULL, 0) == 0);
    uint32_t again; memcpy(&again, open_resp + 8, 4);
    CHECK(again != 0 && again != session);
    close_session(&g, again);
  }
}

int main(int argc, char **argv) {
  const char *helper = argc > 1 ? argv[1] : "build/host/oneios-encoder";
  run(br_encoder_backend_helper(helper), 0x0a000000);
  int after_helper = checks;
  run(br_encoder_backend_stub(), 0x0a001000);
  printf("test_encoder: %d/%d checks passed (%d through the helper process, %d with the stand-in in the device's process)\n",
         checks - failures, checks, after_helper, checks - after_helper);
  return failures ? 1 : 0;
}
