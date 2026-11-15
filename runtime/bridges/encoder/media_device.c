// SPDX-License-Identifier: MIT
// "droidvm,virtio-media": xnuhost's stateful V4L2 video encoder device (docs/hardware.md, "Video encoder"). It is the
// host end of the V4L2 codec UniXNU-droidvm's DroidVMVTHost.cpp drives for iOS's paravirtualized VideoToolbox
// encoder: OUTPUT is tightly packed NV12 frames, CAPTURE is the HEVC/H.264 bitstream (Annex-B). The real encoding is
// a backend (encoder.h): Android's MediaCodec on the phone, a stand-in on the PC.
//
//   commandq (0): OPEN/CLOSE/IOCTL/MMAP/MUNMAP, answered on the vCPU thread (handle(), the device lock held).
//   eventq   (1, manual): the guest posts writable event buffers; a worker thread fills one per event (DQBUF of a
//            returned OUTPUT buffer or a coded CAPTURE buffer, an EOS EVENT).
//
// The worker asks the backend for coded frames (a backend has no callback). It does so every millisecond only for
// MEDIA_BUSY_MS after the guest's last command or the backend's last frame, every MEDIA_IDLE_MS while a session is
// open and quiet, and not at all with no session and no event waiting: then it sleeps until a command or a kick
// wakes it (a device nobody uses costs no wake-ups; before 2026-11-15 it woke 50 times a second for ever).
//
// Each virtio-media session gets a fixed slice of the shared media pool for its MMAP'd buffers (MEDIA_SLOTS slots), so
// the allocator is a per-slot bump with no fragmentation. A session that will not fit is refused at REQBUFS and
// VideoToolbox keeps its software encoder.
#include "encoder.h"
#include "media_proto.h"
#include <errno.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

_Static_assert(sizeof(struct media_v4l2_buffer) == 88 + 64, "v4l2_buffer + v4l2_plane");
_Static_assert(sizeof(struct media_v4l2_format) == 208, "v4l2_format");

enum {
  MEDIA_SLOTS = 2,                         // concurrent sessions (chronod snapshots are near-serial)
  MEDIA_OUTPUT_BUFS = 4,
  MEDIA_CAPTURE_BUFS = 8,
  MEDIA_MAX_BUFS = 32,
  MEDIA_CODED_HEADROOM = 65536,
  MEDIA_EVENT_FIFO = 64,
  MEDIA_BUSY_MS = 2000,                    // the backend is asked every millisecond this long after the last activity
  MEDIA_IDLE_MS = 50,                      // then this often, while a session is open
};

struct buf {
  uint64_t offset;                         // into the pool (and mmap handle)
  uint64_t size;
  bool queued;                             // on the device's side of the V4L2 queue
};

struct media_session {
  bool used;
  uint32_t id;
  uint64_t slot_base, slot_size, bump;     // the session's pool slice
  uint32_t codec, width, height;           // codec from the CAPTURE format, size from OUTPUT
  bool output_fmt, capture_fmt;
  bool output_on, capture_on, draining, eos_sent;
  struct buf output[MEDIA_OUTPUT_BUFS];
  unsigned output_count;
  struct buf capture[MEDIA_MAX_BUFS];
  unsigned capture_count;
  br_enc_session *enc;
};

// A pending event, filled into a guest event buffer by the worker.
struct event {
  uint32_t kind, session;
  uint8_t body[MEDIA_EVENT_BYTES - 8];
  uint32_t body_len;
};

typedef struct {
  xh_virtio *dev;
  const br_encoder_backend *backend;
  uint8_t *pool_host;
  uint64_t pool_gpa, pool_size, slot_size;
  uint32_t next_session;
  struct media_session sessions[MEDIA_SLOTS];
  struct event fifo[MEDIA_EVENT_FIFO];
  unsigned fifo_head, fifo_tail;           // producer: handle()/worker; consumer: flush
  pthread_t worker;
  bool stop;
  uint64_t busy_until_ms;                  // the device lock held: the worker asks the backend every millisecond until then
  pthread_mutex_t wake_lock;               // the worker's sleep: `wake` under it
  pthread_cond_t wake_cond;
  bool wake;
  br_encoder_stats stats;
} media_state;

static uint64_t now_ms(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (uint64_t)ts.tv_sec * 1000 + (uint64_t)ts.tv_nsec / 1000000;
}

// Something for the worker: a command or a kick of the guest's (the device lock held or not).
static void wake_worker(media_state *m) {
  pthread_mutex_lock(&m->wake_lock);
  m->wake = true;
  pthread_cond_signal(&m->wake_cond);
  pthread_mutex_unlock(&m->wake_lock);
}

// The worker's sleep with nothing in flight: until it is woken, or `ms` at most (0: no limit).
static void worker_sleep(media_state *m, unsigned ms) {
  pthread_mutex_lock(&m->wake_lock);
  if (!m->wake) {
    if (ms) {
      struct timespec until;
      clock_gettime(CLOCK_MONOTONIC, &until);
      until.tv_nsec += (long)ms * 1000000L;
      if (until.tv_nsec >= 1000000000L) { until.tv_sec += until.tv_nsec / 1000000000L; until.tv_nsec %= 1000000000L; }
      pthread_cond_timedwait(&m->wake_cond, &m->wake_lock, &until);
    } else pthread_cond_wait(&m->wake_cond, &m->wake_lock);
  }
  m->wake = false;
  pthread_mutex_unlock(&m->wake_lock);
}

// ---- helpers --------------------------------------------------------------------------------------------------

static struct media_session *session_of(media_state *m, uint32_t id) {
  for (unsigned i = 0; i < MEDIA_SLOTS; i++)
    if (m->sessions[i].used && m->sessions[i].id == id) return &m->sessions[i];
  return NULL;
}

static uint8_t *pool_at(media_state *m, uint64_t offset, uint64_t size) {
  return offset + size <= m->pool_size ? m->pool_host + offset : NULL;
}

// A buffer out of the session's slot; UINT64_MAX when the slot is full.
static uint64_t slot_alloc(struct media_session *s, uint64_t size) {
  uint64_t aligned = (size + 0xfff) & ~(uint64_t)0xfff;
  if (s->bump + aligned > s->slot_size) return UINT64_MAX;
  uint64_t off = s->slot_base + s->bump;
  s->bump += aligned;
  return off;
}

static void push_event(media_state *m, uint32_t kind, uint32_t session, const void *body, uint32_t body_len) {
  if (m->fifo_head - m->fifo_tail >= MEDIA_EVENT_FIFO) { m->stats.errors++; return; }
  struct event *e = &m->fifo[m->fifo_head++ % MEDIA_EVENT_FIFO];
  e->kind = kind;
  e->session = session;
  e->body_len = body_len < sizeof e->body ? body_len : (uint32_t)sizeof e->body;
  memset(e->body, 0, sizeof e->body);
  if (body) memcpy(e->body, body, e->body_len);
}

// A DQBUF event: a struct v4l2_buffer (+ one plane) for the returned buffer.
static void push_dqbuf(media_state *m, struct media_session *s, uint32_t type, uint32_t index, uint32_t bytesused,
                       uint32_t flags, int64_t sec) {
  struct media_v4l2_buffer b;
  memset(&b, 0, sizeof b);
  b.index = index;
  b.type = type;
  b.memory = MEDIA_MEMORY_MMAP;
  b.flags = flags;
  b.sec = sec;
  b.length = 1;
  b.plane.bytesused = bytesused;
  b.plane.length = (uint32_t)(type == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE ? s->capture[index].size : s->output[index].size);
  push_event(m, MEDIA_EVT_DQBUF, s->id, &b, sizeof b);
}

// ---- the V4L2 ioctls ------------------------------------------------------------------------------------------

// A session's backend is opened when both queues are streaming and the codec and size are known.
static void maybe_open_encoder(media_state *m, struct media_session *s) {
  if (s->enc || !s->output_on || !s->capture_on || !s->codec || !s->width || !s->height) return;
  s->enc = m->backend->open(s->codec, s->width, s->height);
  if (!s->enc) { m->stats.errors++; br_log(0, "encoder: backend %s refused %ux%u", m->backend->name, s->width, s->height); }
  else { m->stats.sessions++; br_log(1, "encoder: session %u: %s %ux%u on %s", s->id, s->codec == MEDIA_PIXFMT_HEVC ? "HEVC" : "H.264",
                                      s->width, s->height, m->backend->name); }
}

// IOCTL on a session: nr the _IOC_NR, `payload` the structure the guest sent (`len` valid bytes), modified in place
// for the bytes written back. Returns a Linux errno (0 on success).
static int do_ioctl(media_state *m, struct media_session *s, uint32_t nr, uint8_t *payload, uint32_t len) {
  switch (nr) {
  case MEDIA_VIDIOC_S_FMT: {
    if (len < sizeof(struct media_v4l2_format)) return EINVAL;
    struct media_v4l2_format f;
    memcpy(&f, payload, sizeof f);
    if (f.type == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE) {
      if (f.pixelformat != MEDIA_PIXFMT_NV12 || !f.width || !f.height) return EINVAL;
      s->width = f.width;
      s->height = f.height;
      s->output_fmt = true;
      f.num_planes = 1;
      f.planes[0].bytesperline = f.width;
      f.planes[0].sizeimage = f.width * f.height * 3 / 2;
    } else if (f.type == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE) {
      if (f.pixelformat != MEDIA_PIXFMT_HEVC && f.pixelformat != MEDIA_PIXFMT_H264) return EINVAL;
      s->codec = f.pixelformat;
      s->capture_fmt = true;
      f.num_planes = 1;
      if (!f.planes[0].sizeimage) f.planes[0].sizeimage = (f.width * f.height / 2) + MEDIA_CODED_HEADROOM;
    } else return EINVAL;
    memcpy(payload, &f, sizeof f);
    return 0;
  }
  case MEDIA_VIDIOC_REQBUFS: {
    if (len < 20) return EINVAL;
    uint32_t r[5];
    memcpy(r, payload, sizeof r);
    uint32_t want = r[0], type = r[1];
    struct buf *bufs;
    unsigned *count, cap;
    uint64_t size;
    if (type == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE) {
      bufs = s->output; count = &s->output_count; cap = MEDIA_OUTPUT_BUFS;
      size = (uint64_t)s->width * s->height * 3 / 2;
    } else if (type == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE) {
      bufs = s->capture; count = &s->capture_count; cap = MEDIA_CAPTURE_BUFS;
      size = (uint64_t)s->width * s->height / 2 + MEDIA_CODED_HEADROOM;
    } else return EINVAL;
    if (want > cap) want = cap;
    for (unsigned i = 0; i < want; i++) {
      uint64_t off = slot_alloc(s, size);
      if (off == UINT64_MAX) { m->stats.errors++; return ENOMEM; }    // the slot is too small: VT keeps software
      bufs[i] = (struct buf){ off, size, false };
    }
    *count = want;
    r[0] = want;
    memcpy(payload, r, sizeof r);
    return 0;
  }
  case MEDIA_VIDIOC_QUERYBUF: {
    if (len < sizeof(struct media_v4l2_buffer)) return EINVAL;
    struct media_v4l2_buffer b;
    memcpy(&b, payload, sizeof b);
    struct buf *bufs = b.type == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE ? s->output : s->capture;
    unsigned cnt = b.type == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE ? s->output_count : s->capture_count;
    if (b.index >= cnt) return EINVAL;
    b.length = 1;
    b.plane.length = (uint32_t)bufs[b.index].size;
    b.plane.m = bufs[b.index].offset;                              // the mmap handle (MMAP turns it into a pool gpa)
    memcpy(payload, &b, sizeof b);
    return 0;
  }
  case MEDIA_VIDIOC_QBUF: {
    if (len < sizeof(struct media_v4l2_buffer)) return EINVAL;
    struct media_v4l2_buffer b;
    memcpy(&b, payload, sizeof b);
    if (b.type == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE) {
      if (b.index >= s->output_count || !s->enc) return EINVAL;
      const uint8_t *nv12 = pool_at(m, s->output[b.index].offset, b.plane.bytesused);
      int rc = nv12 ? m->backend->feed(s->enc, nv12, b.plane.bytesused, b.sec * 1000000, 0) : -EINVAL;
      if (rc) { m->stats.errors++; return -rc; }
      m->stats.frames_in++;
      // MediaCodec copies the input; the OUTPUT buffer is free again at once.
      push_dqbuf(m, s, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, b.index, 0, 0, b.sec);
    } else if (b.type == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE) {
      if (b.index >= s->capture_count) return EINVAL;
      s->capture[b.index].queued = true;                          // free for the worker to fill
    } else return EINVAL;
    memcpy(payload, &b, sizeof b);
    return 0;
  }
  case MEDIA_VIDIOC_STREAMON:
  case MEDIA_VIDIOC_STREAMOFF: {
    if (len < 4) return EINVAL;
    uint32_t type;
    memcpy(&type, payload, 4);
    bool on = nr == MEDIA_VIDIOC_STREAMON;
    if (type == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE) s->output_on = on;
    else if (type == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE) s->capture_on = on;
    else return EINVAL;
    if (on) maybe_open_encoder(m, s);
    return 0;
  }
  case MEDIA_VIDIOC_SUBSCRIBE_EVENT:
    return 0;                                                      // EOS / SOURCE_CHANGE: accepted, delivered on the eventq
  case MEDIA_VIDIOC_ENCODER_CMD: {
    if (len < 4) return EINVAL;
    uint32_t cmd;
    memcpy(&cmd, payload, 4);
    if (cmd == MEDIA_ENC_CMD_STOP && s->enc && !s->draining) { s->draining = true; s->eos_sent = false; m->backend->drain(s->enc); }
    return 0;
  }
  case MEDIA_VIDIOC_ENUM_FRAMESIZES:
    return EINVAL;                                                 // encoder: let VideoToolbox decide the size
  case MEDIA_VIDIOC_ENUM_FMT: {
    // struct v4l2_fmtdesc: index, type, flags, description[32], pixelformat (at 44). The guest's transport lists
    // each queue's formats once at start (DroidVMVirtioMedia.c check(), up to 16 an answer of 0 gives it): OUTPUT
    // takes NV12, CAPTURE gives HEVC and H.264; past the list EINVAL ends its loop, as V4L2 says.
    if (len < 48) return EINVAL;
    uint32_t index, type, format = 0;
    memcpy(&index, payload, 4);
    memcpy(&type, payload + 4, 4);
    if (type == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE) format = index == 0 ? MEDIA_PIXFMT_NV12 : 0;
    else if (type == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE) format = index == 0 ? MEDIA_PIXFMT_HEVC : index == 1 ? MEDIA_PIXFMT_H264 : 0;
    if (!format) return EINVAL;
    memcpy(payload + 44, &format, 4);
    return 0;
  }
  case MEDIA_VIDIOC_G_FMT:
  case MEDIA_VIDIOC_G_CTRL:
    return 0;                                                      // not needed by the encoder path
  default:
    return ENOTTY;
  }
}

// ---- the commandq (handle(), the device lock held) ------------------------------------------------------------

static uint32_t rd32(const uint8_t *p) { uint32_t v; memcpy(&v, p, 4); return v; }

static int64_t media_handle(xh_virtio *dev, unsigned queue, uint16_t head, xh_viov *iov, unsigned n) {
  (void)head;
  media_state *m = xh_virtio_opaque(dev);
  if (queue != MEDIA_QUEUE_COMMAND) return 0;
  xh_viov *reads[4], *writes[4];
  unsigned nr = 0, nw = 0;
  for (unsigned i = 0; i < n; i++) {
    if (iov[i].writable) { if (nw < 4) writes[nw++] = &iov[i]; }
    else { if (nr < 4) reads[nr++] = &iov[i]; }
  }
  if (nr < 1 || reads[0]->length < 8) return 0;
  const uint8_t *cmd = reads[0]->data;
  uint32_t op = rd32(cmd);
  m->busy_until_ms = now_ms() + MEDIA_BUSY_MS;                     // a command: the worker looks at once, and often for a while
  wake_worker(m);
  // CLOSE comes with no buffer for an answer (DroidVMVirtioMedia.c droidvm_media_close sends the command alone), so
  // it is done before the answer's buffer is asked for. Until 2026-11-15 a command without one was dropped: no
  // session was ever closed, and the device's MEDIA_SLOTS sessions were used up by the guest's start-up check and
  // the first encode.
  if (op == MEDIA_CMD_CLOSE) {
    struct media_session *s = reads[0]->length >= 12 ? session_of(m, rd32(cmd + 8)) : NULL;
    if (s) {
      if (s->enc) m->backend->close(s->enc);
      s->used = false;
    }
    if (nw < 1 || writes[0]->length < 8) return 0;
    memset(writes[0]->data, 0, 8);
    return 8;
  }
  if (nw < 1) return 0;
  uint8_t *resp = writes[0]->data;
  uint32_t resp_cap = writes[0]->length;
  if (resp_cap < 8) return 0;
  memset(resp, 0, resp_cap);
  uint32_t written = 8;

  switch (op) {
  case MEDIA_CMD_OPEN: {
    if (resp_cap < 16) { break; }
    struct media_session *slot = NULL;
    for (unsigned i = 0; i < MEDIA_SLOTS; i++) if (!m->sessions[i].used) { slot = &m->sessions[i]; break; }
    if (!slot) { uint32_t e = ENOMEM; memcpy(resp, &e, 4); written = 16; break; }
    memset(slot, 0, sizeof *slot);
    slot->used = true;
    slot->id = ++m->next_session;
    slot->slot_base = (uint64_t)(slot - m->sessions) * m->slot_size;
    slot->slot_size = m->slot_size;
    uint32_t zero = 0;
    memcpy(resp, &zero, 4);
    memcpy(resp + 8, &slot->id, 4);
    written = 16;
    break;
  }
  case MEDIA_CMD_IOCTL: {
    if (reads[0]->length < 16) break;
    struct media_session *s = session_of(m, rd32(cmd + 8));
    uint32_t ioc = rd32(cmd + 12);
    const uint8_t *out_payload = nr >= 2 ? reads[1]->data : NULL;
    uint32_t out_len = nr >= 2 ? reads[1]->length : 0;
    uint8_t *in_payload = nw >= 2 ? writes[1]->data : NULL;
    uint32_t in_len = nw >= 2 ? writes[1]->length : 0;
    uint8_t scratch[sizeof(struct media_v4l2_format)];
    uint32_t len = out_len > sizeof scratch ? (uint32_t)sizeof scratch : out_len;
    uint32_t back = in_len > sizeof scratch ? (uint32_t)sizeof scratch : in_len;
    memset(scratch, 0, sizeof scratch);
    if (out_payload) memcpy(scratch, out_payload, len);
    int status = s ? do_ioctl(m, s, ioc, scratch, len) : ENODEV;
    memcpy(resp, &status, 4);
    if (!status && in_payload) memcpy(in_payload, scratch, back);
    written = 8 + (in_payload ? back : 0);
    break;
  }
  case MEDIA_CMD_MMAP: {
    if (reads[0]->length < 20 || resp_cap < 24) break;
    struct media_session *s = session_of(m, rd32(cmd + 8));
    uint64_t offset = rd32(cmd + 16);
    uint32_t status = EINVAL;
    uint64_t at = 0, size = 0;
    if (s) {
      for (unsigned i = 0; i < s->output_count && !size; i++) if (s->output[i].offset == offset) { at = offset; size = s->output[i].size; }
      for (unsigned i = 0; i < s->capture_count && !size; i++) if (s->capture[i].offset == offset) { at = offset; size = s->capture[i].size; }
      if (size) status = 0;
    }
    // The answer is the buffer's place in the pool, not its address: the guest adds its media-host-pool base
    // (DroidVMVirtioMedia.c droidvm_media_mmap: `at > pool_size` is refused, then gpa = pool_base + at). Until
    // 2026-11-15 the pool's guest address was added here too, which the guest's bound refused for every buffer.
    memcpy(resp, &status, 4);
    memcpy(resp + 8, &at, 8);
    memcpy(resp + 16, &size, 8);
    written = 24;
    break;
  }
  case MEDIA_CMD_MUNMAP:
    written = 8;
    break;
  default: {
    uint32_t e = ENOTTY;
    memcpy(resp, &e, 4);
    break;
  }
  }
  return written;
}

// ---- the eventq worker ----------------------------------------------------------------------------------------

// Flushes the event FIFO into the guest's posted event buffers. The device lock is held.
static void flush_events(media_state *m) {
  while (m->fifo_tail != m->fifo_head) {
    xh_viov iov[2];
    uint16_t head;
    unsigned got = xh_virtio_pop(m->dev, MEDIA_QUEUE_EVENT, &head, iov, 2);
    if (!got) break;                                               // no guest buffer: keep the event
    struct event *e = &m->fifo[m->fifo_tail % MEDIA_EVENT_FIFO];
    uint8_t *dst = iov[0].data;
    uint32_t cap = iov[0].length, w = 0;
    if (iov[0].writable && cap >= 8) {
      memcpy(dst, &e->kind, 4);
      memcpy(dst + 4, &e->session, 4);
      uint32_t body = e->body_len < cap - 8 ? e->body_len : cap - 8;
      memcpy(dst + 8, e->body, body);
      w = 8 + body;
    }
    xh_virtio_push(m->dev, MEDIA_QUEUE_EVENT, head, w);
    m->fifo_tail++;
  }
}

// Pulls coded access units out of each streaming session's backend and turns them into CAPTURE DQBUF events.
static void pump(media_state *m) {
  for (unsigned i = 0; i < MEDIA_SLOTS; i++) {
    struct media_session *s = &m->sessions[i];
    if (!s->used || !s->enc || !s->capture_on) continue;
    for (;;) {
      unsigned index = s->capture_count;
      for (unsigned b = 0; b < s->capture_count; b++) if (s->capture[b].queued) { index = b; break; }
      if (index == s->capture_count) break;                        // no free CAPTURE buffer; try next poll
      uint8_t *dst = pool_at(m, s->capture[index].offset, 0);
      if (!dst) break;
      int key = 0, eos = 0;
      int64_t pts = 0;
      int bytes = m->backend->poll(s->enc, dst, (uint32_t)s->capture[index].size, &key, &pts, &eos);
      if (bytes < 0) { m->stats.errors++; break; }
      if (bytes == 0 && !eos) break;                               // nothing ready yet
      uint32_t flags = (key ? MEDIA_BUF_FLAG_KEYFRAME : 0) | (eos ? MEDIA_BUF_FLAG_LAST : 0);
      s->capture[index].queued = false;
      push_dqbuf(m, s, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, index, (uint32_t)bytes, flags, pts / 1000000);
      m->busy_until_ms = now_ms() + MEDIA_BUSY_MS;                 // the backend is giving: the next may follow at once
      if (bytes > 0) m->stats.frames_out++;
      if (eos) {
        uint32_t ev[16] = { MEDIA_EVENT_EOS };
        push_event(m, MEDIA_EVT_EVENT, s->id, ev, sizeof ev);
        s->draining = false;
        s->eos_sent = true;
        break;
      }
    }
  }
}

static void *worker_main(void *arg) {
  media_state *m = arg;
  for (;;) {
    xh_virtio_lock(m->dev);
    if (m->stop) { xh_virtio_unlock(m->dev); break; }
    pump(m);
    flush_events(m);
    // Something may still come without a command: a session's backend may hold a frame, an event may wait for a buffer.
    bool open = m->fifo_head != m->fifo_tail;
    for (unsigned i = 0; i < MEDIA_SLOTS; i++) open |= m->sessions[i].used;
    bool busy = open && now_ms() < m->busy_until_ms;
    xh_virtio_unlock(m->dev);
    if (busy) { struct timespec ts = { 0, 1000000L }; nanosleep(&ts, NULL); }
    else worker_sleep(m, open ? MEDIA_IDLE_MS : 0);
  }
  return NULL;
}

// ---- config space and lifecycle -------------------------------------------------------------------------------

static uint32_t media_config_read(xh_virtio *dev, uint32_t offset, unsigned size) {
  (void)dev; (void)size;
  if (offset == 0) return MEDIA_CAP_VIDEO_M2M_MPLANE | MEDIA_CAP_STREAMING;   // device_caps
  if (offset == 4) return 1;                                       // device_type: 1 = encoder (host rig)
  if (offset >= 8 && offset < 8 + 32) {                            // card: "oneios-encoder"
    static const char card[32] = "oneios-encoder";
    uint32_t v = 0;
    memcpy(&v, card + (offset - 8), 4);
    return v;
  }
  return 0;
}

// eventq: the guest gave event buffers back; the worker fills them (an event may have waited for one).
static void media_kick(xh_virtio *dev, unsigned queue) { (void)queue; wake_worker(xh_virtio_opaque(dev)); }

static void media_reset(xh_virtio *dev) {
  media_state *m = xh_virtio_opaque(dev);
  for (unsigned i = 0; i < MEDIA_SLOTS; i++)
    if (m->sessions[i].used) { if (m->sessions[i].enc) m->backend->close(m->sessions[i].enc); m->sessions[i].used = false; }
  m->fifo_head = m->fifo_tail = 0;
}

static const xh_virtio_ops media_ops = {
  .device_id = MEDIA_VIRTIO_DEVICE_ID, .nqueues = MEDIA_NQUEUES, .handle = media_handle,
  .config_read = media_config_read, .reset = media_reset, .manual = 1u << MEDIA_QUEUE_EVENT, .kick = media_kick,
};

xh_virtio *br_encoder_create(uint64_t base, unsigned spi, const br_encoder_backend *backend,
                             void *pool_host, uint64_t pool_gpa, uint64_t pool_size) {
  if (!backend || !pool_host || pool_size < MEDIA_SLOTS * (1u << 20)) {
    br_log(1, "encoder: off: no media pool or backend");
    return NULL;
  }
  media_state *m = calloc(1, sizeof *m);
  if (!m) return NULL;
  m->backend = backend;
  m->pool_host = pool_host;
  m->pool_gpa = pool_gpa;
  m->pool_size = pool_size;
  m->slot_size = pool_size / MEDIA_SLOTS;
  pthread_condattr_t attr;
  pthread_condattr_init(&attr);
  pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
  pthread_mutex_init(&m->wake_lock, NULL);
  pthread_cond_init(&m->wake_cond, &attr);
  pthread_condattr_destroy(&attr);
  m->dev = xh_virtio_create(base, spi, &media_ops, m, "virtio-media");
  if (!m->dev) { free(m); return NULL; }
  if (pthread_create(&m->worker, NULL, worker_main, m)) { br_log(0, "encoder: no worker thread"); }
  br_log(1, "encoder: on: %s backend, media pool %llu MiB at %#llx (%u session slots)",
         backend->name, (unsigned long long)(pool_size >> 20), (unsigned long long)pool_gpa, MEDIA_SLOTS);
  return m->dev;
}

void br_encoder_get_stats(xh_virtio *dev, br_encoder_stats *out) {
  media_state *m = xh_virtio_opaque(dev);
  xh_virtio_lock(dev);
  *out = m->stats;
  xh_virtio_unlock(dev);
}
