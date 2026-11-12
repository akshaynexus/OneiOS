// SPDX-License-Identifier: MIT
// virtio-gpu device (VirtIO 1.2, 5.7).
// 2D: the scanout the guest's display driver presents frames on (UniXNU-droidvm's DroidVMVirtioGpuDisplay.cpp under
// IOMobileFramebufferLegacy); the first frame says the guest is up.
// 3D (xh_virtio_gpu_create_3d): crosvm's DRM native context as UniXNU-droidvm's DroidVMVirtioGpu3D.cpp drives it --
// capset 6, CTX_CREATE, RESOURCE_CREATE_BLOB (HOST3D for a context's shmem, HOST3D_GUEST over guest pages),
// MAP_BLOB answered with MAP_INFO_POOL and the offset in /arm-io/virtio-gpu drm-host-pool, SUBMIT_3D with per-ring
// fences (a fenced command is returned when its fence retires). The renderer is behind gpu3d.h (drm2kgsl on KGSL).
#include "xnuhost.h"
#include "gpu3d.h"
#include <errno.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/eventfd.h>

enum {
  CMD_GET_DISPLAY_INFO = 0x100, CMD_RESOURCE_CREATE_2D, CMD_RESOURCE_UNREF, CMD_SET_SCANOUT, CMD_RESOURCE_FLUSH,
  CMD_TRANSFER_TO_HOST_2D, CMD_RESOURCE_ATTACH_BACKING, CMD_RESOURCE_DETACH_BACKING,
  CMD_GET_CAPSET_INFO = 0x108, CMD_GET_CAPSET = 0x109, CMD_RESOURCE_CREATE_BLOB = 0x10c, CMD_SET_SCANOUT_BLOB = 0x10d,
  CMD_CTX_CREATE = 0x200, CMD_CTX_DESTROY, CMD_CTX_ATTACH_RESOURCE, CMD_CTX_DETACH_RESOURCE,
  CMD_SUBMIT_3D = 0x207, CMD_RESOURCE_MAP_BLOB, CMD_RESOURCE_UNMAP_BLOB,
  RESP_OK_NODATA = 0x1100, RESP_OK_DISPLAY_INFO, RESP_OK_CAPSET_INFO, RESP_OK_CAPSET, RESP_OK_MAP_INFO = 0x1106,
  RESP_ERR_UNSPEC = 0x1200, RESP_ERR_OUT_OF_MEMORY, RESP_ERR_INVALID_SCANOUT_ID, RESP_ERR_INVALID_RESOURCE_ID,
  RESP_ERR_INVALID_CONTEXT_ID, RESP_ERR_INVALID_PARAMETER,
};
enum { FMT_BGRA = 1, FMT_BGRX = 2, FMT_ARGB = 3, FMT_XRGB = 4, FMT_RGBA = 67, FMT_XBGR = 68, FMT_ABGR = 121, FMT_RGBX = 134 };

typedef struct { uint32_t type, flags; uint64_t fence; uint32_t ctx; uint8_t ring, pad[3]; } gpu_hdr;
typedef struct { uint32_t x, y, w, h; } gpu_rect;
typedef xh_gpu_entry gpu_entry;
enum { FLAG_FENCE = 1, FLAG_RING_IDX = 2, MAP_INFO_POOL = 1u << 31 };
enum { F_VIRGL = 1u << 0, F_RESOURCE_BLOB = 1u << 3, F_CONTEXT_INIT = 1u << 4, F_CREATE_GUEST_HANDLE = 1u << 6 };

#define MAX_RES 64
#define MAX_BACKING 16384              // as MAX_BLOB_ENTRIES: a 2D surface of scattered 4 KiB pages (1440x3120 has 4388)
typedef struct {
  uint32_t id, format, width, height;
  uint8_t *pixels;                 // host copy, always B,G,R,X byte order
  gpu_entry *backing;
  unsigned nbacking;
} resource;

#define MAX_CTX 128
#define MAX_BLOB_ENTRIES 16384         // DroidVMVirtioGpu.h MaxEntries
#define BLOB_SLOTS (1u << 15)          // open addressing, linear probing: blob resource id -> blob
#define WORKER_IOV 256

// A blob resource. Guest blobs (GUEST: the display's surfaces; HOST3D_GUEST: a context's BOs) keep the guest's
// segments, so a blob can be a scanout; HOST3D blobs (a context's shmem) are the renderer's alone.
typedef struct {
  uint32_t id, ctx, mem;
  uint64_t size;
  gpu_entry *entries;
  unsigned nentries;
} blob;

// A fenced command whose chain goes back to the guest when its context's ring passes the fence.
typedef struct pending {
  struct pending *next;
  uint16_t head;
  uint32_t written, ctx, ring;
  uint64_t fence, gen;
} pending;

typedef struct {
  unsigned width, height;
  resource res[MAX_RES];
  uint32_t scanout_resource;
  bool first_frame;
  // 3D. The worker is the only thread that changes the state above and the tables below. `pl` guards the list of
  // pending fences and the worker's bookkeeping; `gen` (bumped at every device reset) changes under the device's lock.
  xh_gpu3d *b;
  xh_virtio *dev;
  uint32_t ctx[MAX_CTX];               // live context ids
  bool ctx_fast[MAX_CTX];              // that context's commands are the fast lane's (the render server's: "xnu:backboardd")
  blob *blobs;                         // BLOB_SLOTS of them; id 0 = free
  unsigned nblobs;
  struct { uint32_t width, height, format, stride, offset; } scan;      // the scanout when it is a blob
  pthread_mutex_t pl;
  pthread_cond_t idle;
  pending *fences;
  uint64_t gen, kicks, served;
  bool reset_requested;
  int wake;                            // eventfd: the control queue was kicked (written from the trap handler)
  // Two lanes. The queue's thread serves the render server's contexts and every command of no context (the display,
  // the 2D commands) at once, and hands every other context's commands, in their order, to the slow lane's thread.
  // An app that makes 100 blobs at its launch (1-30 ms each on the phone: udmabufs, KGSL's import and bind) then no
  // longer holds back the render server's next frame and the display's flush behind them. `cmd` makes the two take
  // turns at the state above and the renderer (a frame waits for one blob at most, not for all of them).
  pthread_mutex_t cmd;
  pthread_cond_t slow_cv;
  struct slow_job *slow_head, **slow_tail;
  uint64_t slow_queued, slow_done;
} gpu_state;

typedef struct slow_job {
  struct slow_job *next;
  uint16_t head;
  unsigned n;
  uint64_t gen;
  xh_viov iov[];
} slow_job;

static resource *find(gpu_state *g, uint32_t id)
{
  for (unsigned i = 0; id && i < MAX_RES; i++) if (g->res[i].id == id) return &g->res[i];
  return NULL;
}

static void release(resource *r)
{
  free(r->pixels);
  free(r->backing);
  memset(r, 0, sizeof *r);
}

// Byte order of a format's pixel in guest memory -> B,G,R,X.
static void convert(uint8_t *to, const uint8_t *from, uint32_t format, size_t pixels)
{
  switch (format) {
  case FMT_BGRA: case FMT_BGRX: memcpy(to, from, pixels * 4); break;
  case FMT_RGBA: case FMT_RGBX:
    for (size_t i = 0; i < pixels; i++, to += 4, from += 4) { to[0] = from[2]; to[1] = from[1]; to[2] = from[0]; to[3] = from[3]; }
    break;
  case FMT_ARGB: case FMT_XRGB:
    for (size_t i = 0; i < pixels; i++, to += 4, from += 4) { to[0] = from[3]; to[1] = from[2]; to[2] = from[1]; to[3] = from[0]; }
    break;
  default:                         // ABGR, XBGR
    for (size_t i = 0; i < pixels; i++, to += 4, from += 4) { to[0] = from[1]; to[1] = from[2]; to[2] = from[3]; to[3] = from[0]; }
  }
}

// Copies `length` bytes at linear `offset` of a resource's backing into `out`.
static bool backing_read(const resource *r, uint64_t offset, uint8_t *out, uint64_t length)
{
  for (unsigned i = 0; i < r->nbacking && length; i++) {
    uint64_t size = r->backing[i].length;
    if (offset >= size) { offset -= size; continue; }
    uint64_t n = size - offset < length ? size - offset : length;
    const uint8_t *p = xh_virtio_translate(r->backing[i].addr + offset, n);
    if (!p) return false;
    memcpy(out, p, n);
    out += n; length -= n; offset = 0;
  }
  return length == 0;
}

// The guest's display driver also scans out the boot framebuffer itself (DroidVMVirtioGpuDisplay.cpp's `boot`
// resource: the kernel console until the compositor's first frame, and later whenever a blank or CPU-copied frame was
// drawn there). That picture is shown like any other, but it is the console's: the guest is ready (session/ready)
// at the first frame of a surface of its own.
static void frame_shown(gpu_state *g, const gpu_entry *entries, unsigned n)
{
  if (g->first_frame || (n && g_info.video[0] && entries[0].addr == g_info.video[0])) return;
  g->first_frame = true;
  xh_guest_ready();
}

static bool rect_ok(const gpu_rect *r, uint32_t w, uint32_t h)
{
  return r->w && r->h && r->x <= w && r->y <= h && r->w <= w - r->x && r->h <= h - r->y;
}

// ---- 3D --------------------------------------------------------------------------------------------------------------
static unsigned blob_hash(uint32_t id) { return (id * 2654435761u) >> 17 & (BLOB_SLOTS - 1); }

static blob *blob_find(gpu_state *g, uint32_t id)
{
  if (!id) return NULL;
  for (unsigned i = blob_hash(id); g->blobs[i].id; i = (i + 1) & (BLOB_SLOTS - 1)) if (g->blobs[i].id == id) return &g->blobs[i];
  return NULL;
}

static blob *blob_add(gpu_state *g, uint32_t id)
{
  if (g->nblobs >= BLOB_SLOTS / 4 * 3) return NULL;
  unsigned i = blob_hash(id);
  while (g->blobs[i].id) i = (i + 1) & (BLOB_SLOTS - 1);
  g->blobs[i] = (blob){ .id = id };
  g->nblobs++;
  return &g->blobs[i];
}

static void blob_del(gpu_state *g, blob *b)
{
  unsigned i = (unsigned)(b - g->blobs), j = i;
  free(b->entries);
  g->nblobs--;
  for (;;) {                                         // backward shift: the probe chains stay whole
    g->blobs[i].id = 0;
    for (;;) {
      j = (j + 1) & (BLOB_SLOTS - 1);
      if (!g->blobs[j].id) return;
      unsigned k = blob_hash(g->blobs[j].id);
      if (i <= j ? (i < k && k <= j) : (i < k || k <= j)) continue;
      break;
    }
    g->blobs[i] = g->blobs[j];
    i = j;
  }
}

static uint32_t *ctx_find(gpu_state *g, uint32_t id)
{
  for (unsigned i = 0; id && i < MAX_CTX; i++) if (g->ctx[i] == id) return &g->ctx[i];
  return NULL;
}


// The copy of each scanout flush, one log line per 10 s while frames come: `display: 10 s: flushes N, copy mean X us
// max Y us (Z MiB)`. The device's one thread does it, so the guest's next commands (a frame's submissions, its blobs)
// wait behind it.
static uint64_t flush_now_us(void)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (uint64_t)t.tv_sec * 1000000u + (uint64_t)t.tv_nsec / 1000u;
}

static void flush_note(uint64_t took, uint64_t bytes)
{
  static uint64_t since, n, sum, max, total;
  uint64_t now = flush_now_us();
  if (!since) since = now;
  n++; sum += took; total += bytes; if (took > max) max = took;
  if (now - since >= 10000000u) {
    LOGI("display: %llu s: flushes %llu, copy mean %llu us max %llu us (%llu MiB)", (unsigned long long)((now - since) / 1000000u),
         (unsigned long long)n, (unsigned long long)(sum / n), (unsigned long long)max, (unsigned long long)(total >> 20));
    since = now; n = sum = max = total = 0;
  }
}

// RESOURCE_FLUSH of a blob: when it is the scanout (SET_SCANOUT_BLOB), its rows go from the guest's pages to the display.
static uint32_t flush_blob(gpu_state *g, const blob *b, const gpu_rect *rect)
{
  xh_display *d = xh_display_get();
  if (b->id != g->scanout_resource || !g->scan.width || !d || !d->pixels) return RESP_OK_NODATA;
  if (!rect_ok(rect, g->scan.width, g->scan.height)) return RESP_ERR_INVALID_PARAMETER;
  uint32_t w = rect->x < d->width ? (rect->w < d->width - rect->x ? rect->w : d->width - rect->x) : 0;
  uint32_t hgt = rect->y < d->height ? (rect->h < d->height - rect->y ? rect->h : d->height - rect->y) : 0;
  // Rows straight from the guest's pages into the display's buffer, the blob's segments walked once for the whole
  // rectangle: rows lie at rising offsets. (Each row used to be read into a buffer of its own, the segment list searched
  // from its start for every row: 3120 rows of a 1440x3120 frame over about 1100 segments of 16 KiB pages.)
  uint32_t status = RESP_OK_NODATA;
  uint64_t t0 = flush_now_us();
  unsigned seg = 0;
  uint64_t seg_start = 0;                            // the blob offset at which entries[seg] begins
  for (uint32_t y = 0; y < hgt && w && status == RESP_OK_NODATA; y++) {
    uint64_t at = g->scan.offset + (uint64_t)(rect->y + y) * g->scan.stride + (uint64_t)rect->x * 4, left = (uint64_t)w * 4;
    uint8_t *to = d->pixels + (size_t)(rect->y + y) * d->stride + (size_t)rect->x * 4;
    if (at < seg_start) { seg = 0; seg_start = 0; }  // not expected: rows only rise
    while (left) {
      while (seg < b->nentries && at >= seg_start + b->entries[seg].length) seg_start += b->entries[seg++].length;
      if (seg >= b->nentries) { status = RESP_ERR_INVALID_PARAMETER; break; }
      uint64_t in = at - seg_start, n = b->entries[seg].length - in < left ? b->entries[seg].length - in : left;
      const uint8_t *from = xh_virtio_translate(b->entries[seg].addr + in, n);
      if (!from || (n & 3)) { status = RESP_ERR_INVALID_PARAMETER; break; }
      convert(to, from, g->scan.format, (size_t)(n / 4));
      to += n; at += n; left -= n;
    }
  }
  flush_note(flush_now_us() - t0, (uint64_t)w * hgt * 4);
  if (status == RESP_OK_NODATA && w && hgt) {
    xh_display_flush(rect->x, rect->y, w, hgt);
    frame_shown(g, b->entries, b->nentries);
  }
  return status;
}

static void push_pending(gpu_state *g, pending *p)
{
  xh_virtio_lock(g->dev);
  if (p->gen == g->gen) xh_virtio_push(g->dev, 0, p->head, p->written);       // a chain from before a reset is the old queue's
  xh_virtio_unlock(g->dev);
  free(p);
}

// Takes the pending fences that match off the list and returns their commands. ring < 0: every ring of the context.
static void retire(gpu_state *g, uint32_t ctx, int ring, uint64_t fence)
{
  pending *done = NULL, **tail = &done;
  pthread_mutex_lock(&g->pl);
  for (pending **link = &g->fences; *link;) {
    pending *p = *link;
    if (p->ctx == ctx && (ring < 0 || (p->ring == (uint32_t)ring && p->fence <= fence))) { *link = p->next; p->next = NULL; *tail = p; tail = &p->next; }
    else link = &p->next;
  }
  pthread_mutex_unlock(&g->pl);
  while (done) { pending *p = done; done = p->next; push_pending(g, p); }
}

static void gpu_retire(xh_virtio *dev, uint32_t ctx, uint32_t ring, uint64_t fence)
{
  retire(xh_virtio_opaque(dev), ctx, (int)ring, fence);
}

// The 3D commands and the blob ones. 0: not one of them (command() goes on to the 2D ones).
static uint32_t command3d(gpu_state *g, const gpu_hdr *h, const uint8_t *body, size_t left, uint8_t *resp, size_t resp_max, size_t *resp_size)
{
  xh_gpu3d *b = g->b;
  uint32_t a[4];
  switch (h->type) {
  case CMD_GET_CAPSET_INFO: {
    if (left < 8 || resp_max < sizeof *h + 16) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 4);
    uint32_t info[4] = { 0 };
    if (b && a[0] < b->capset_count(b)) b->capset_info(b, a[0], &info[0], &info[1], &info[2]);
    memcpy(resp + sizeof *h, info, sizeof info);
    *resp_size = sizeof *h + 16;
    return RESP_OK_CAPSET_INFO;
  }
  case CMD_GET_CAPSET: {
    if (left < 8) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 8);
    size_t n = b ? b->capset(b, a[0], a[1], resp + sizeof *h, resp_max - sizeof *h) : 0;
    if (!n) return RESP_ERR_INVALID_PARAMETER;
    *resp_size = sizeof *h + n;
    return RESP_OK_CAPSET;
  }
  case CMD_CTX_CREATE: {
    if (left < 8) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 8);
    size_t name = left - 8 < 64 ? left - 8 : 64;
    if (a[0] < name) name = a[0];
    if (!b || !h->ctx || ctx_find(g, h->ctx)) return RESP_ERR_INVALID_CONTEXT_ID;
    uint32_t *slot = NULL;
    for (unsigned i = 0; i < MAX_CTX && !slot; i++) if (!g->ctx[i]) slot = &g->ctx[i];
    if (!slot) return RESP_ERR_OUT_OF_MEMORY;
    int r = b->ctx_create(b, h->ctx, a[1] & 0xff, (const char *)body + 8, name);
    if (r) return r == -ENOMEM ? RESP_ERR_OUT_OF_MEMORY : RESP_ERR_UNSPEC;
    *slot = h->ctx;
    g->ctx_fast[slot - g->ctx] = name >= 14 && !memcmp(body + 8, "xnu:backboardd", 14);
    return RESP_OK_NODATA;
  }
  case CMD_CTX_DESTROY: {
    uint32_t *slot = ctx_find(g, h->ctx);
    if (!slot) return RESP_ERR_INVALID_CONTEXT_ID;
    b->ctx_destroy(b, h->ctx);
    *slot = 0;
    retire(g, h->ctx, -1, 0);                        // its rings are gone: nothing would retire these
    return RESP_OK_NODATA;
  }
  case CMD_SUBMIT_3D: {
    if (left < 8) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 4);
    if (!ctx_find(g, h->ctx)) return RESP_ERR_INVALID_CONTEXT_ID;
    if (!a[0] || a[0] > left - 8) return RESP_ERR_INVALID_PARAMETER;
    return b->submit(b, h->ctx, body + 8, a[0]) ? RESP_ERR_UNSPEC : RESP_OK_NODATA;
  }
  case CMD_RESOURCE_CREATE_BLOB: {
    uint64_t wide[2];
    if (left < 32) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 16);                             // resource, blob_mem, blob_flags, nr_entries
    memcpy(wide, body + 16, 16);                     // blob_id, size
    if (!a[0] || find(g, a[0]) || blob_find(g, a[0])) return RESP_ERR_INVALID_RESOURCE_ID;
    bool guest = a[1] == XH_GPU_BLOB_GUEST || a[1] == XH_GPU_BLOB_HOST3D_GUEST, host = a[1] == XH_GPU_BLOB_HOST3D || a[1] == XH_GPU_BLOB_HOST3D_GUEST;
    if ((!guest && !host) || !wide[1] || a[3] > MAX_BLOB_ENTRIES || (guest ? !a[3] : a[3] != 0) || left - 32 < (size_t)a[3] * sizeof(gpu_entry)) return RESP_ERR_INVALID_PARAMETER;
    if (host && !ctx_find(g, h->ctx)) return RESP_ERR_INVALID_CONTEXT_ID;
    gpu_entry *entries = NULL;
    if (guest) {
      // The segments are guest RAM, whole host pages each (udmabuf takes nothing else), and cover the blob.
      uint64_t total = 0;
      if (!(entries = malloc(a[3] * sizeof *entries))) return RESP_ERR_OUT_OF_MEMORY;
      memcpy(entries, body + 32, a[3] * sizeof *entries);
      for (unsigned i = 0; i < a[3]; i++) {
        if (!entries[i].length || ((entries[i].addr | entries[i].length) & 0xfff) || !xh_virtio_translate(entries[i].addr, entries[i].length)) { free(entries); return RESP_ERR_INVALID_PARAMETER; }
        total += entries[i].length;
      }
      if (total < wide[1]) { free(entries); return RESP_ERR_INVALID_PARAMETER; }
    }
    blob *nb = blob_add(g, a[0]);
    if (!nb) { free(entries); return RESP_ERR_OUT_OF_MEMORY; }
    // The renderer makes every blob of a context, and a display blob it may be asked to attach to one later.
    int r = b && (host || (a[2] & XH_GPU_BLOB_FLAG_GUEST_HANDLE)) ? b->create_blob(b, host ? h->ctx : 0, a[0], a[1], a[2], wide[0], wide[1], entries, a[3]) : 0;
    if (r && host) { nb->entries = entries; blob_del(g, nb); return r == -ENOMEM ? RESP_ERR_OUT_OF_MEMORY : RESP_ERR_UNSPEC; }
    nb->ctx = host ? h->ctx : 0; nb->mem = a[1]; nb->size = wide[1]; nb->entries = entries; nb->nentries = a[3];
    return RESP_OK_NODATA;
  }
  case CMD_RESOURCE_MAP_BLOB: {
    if (left < 16 || resp_max < sizeof *h + 8) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 4);
    blob *bl = blob_find(g, a[0]);
    if (!bl) return RESP_ERR_INVALID_RESOURCE_ID;
    uint64_t offset = 0;
    uint32_t info[2] = { 0 };
    // Only what lies in the pool can be mapped: the guest maps pool base + offset from its own RAM.
    if (!b || bl->mem != XH_GPU_BLOB_HOST3D || b->map_blob(b, a[0], &offset, &info[0]) || offset > UINT32_MAX) return RESP_ERR_INVALID_PARAMETER;
    info[0] |= MAP_INFO_POOL;
    info[1] = (uint32_t)offset;
    memcpy(resp + sizeof *h, info, sizeof info);
    *resp_size = sizeof *h + 8;
    return RESP_OK_MAP_INFO;
  }
  case CMD_RESOURCE_UNMAP_BLOB:
    if (left < 4) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 4);
    return blob_find(g, a[0]) ? RESP_OK_NODATA : RESP_ERR_INVALID_RESOURCE_ID;
  case CMD_CTX_ATTACH_RESOURCE: case CMD_CTX_DETACH_RESOURCE: {
    if (left < 4) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 4);
    if (!ctx_find(g, h->ctx)) return RESP_ERR_INVALID_CONTEXT_ID;
    if (!blob_find(g, a[0])) return RESP_ERR_INVALID_RESOURCE_ID;
    return b->attach(b, h->ctx, a[0], h->type == CMD_CTX_ATTACH_RESOURCE) ? RESP_ERR_UNSPEC : RESP_OK_NODATA;
  }
  case CMD_RESOURCE_UNREF: {
    if (left < 4) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 4);
    blob *bl = blob_find(g, a[0]);
    if (!bl) return 0;                               // a 2D resource, or nothing
    if (g->scanout_resource == a[0]) { g->scanout_resource = 0; g->scan.width = 0; }
    if (b) b->unref(b, a[0]);
    blob_del(g, bl);
    return RESP_OK_NODATA;
  }
  case CMD_SET_SCANOUT_BLOB: {
    gpu_rect rect;
    uint32_t v[14];                                  // scanout, resource, width, height, format, padding, strides[4], offsets[4]
    if (left < 16 + sizeof v) return RESP_ERR_INVALID_PARAMETER;
    memcpy(&rect, body, 16);
    memcpy(v, body + 16, sizeof v);
    if (v[0] != 0) return RESP_ERR_INVALID_SCANOUT_ID;
    if (v[1] == 0) { g->scanout_resource = 0; g->scan.width = 0; return RESP_OK_NODATA; }
    blob *bl = blob_find(g, v[1]);
    if (!bl || !bl->entries) return RESP_ERR_INVALID_RESOURCE_ID;
    if (v[4] != FMT_BGRA && v[4] != FMT_BGRX && v[4] != FMT_ARGB && v[4] != FMT_XRGB && v[4] != FMT_RGBA && v[4] != FMT_XBGR &&
        v[4] != FMT_ABGR && v[4] != FMT_RGBX) return RESP_ERR_INVALID_PARAMETER;
    if (!v[2] || !v[3] || v[2] > 16384 || v[3] > 16384 || v[6] < v[2] * 4 || !rect_ok(&rect, v[2], v[3]) ||
        v[10] + (uint64_t)(v[3] - 1) * v[6] + (uint64_t)v[2] * 4 > bl->size) return RESP_ERR_INVALID_PARAMETER;
    g->scanout_resource = v[1];
    g->scan.width = v[2]; g->scan.height = v[3]; g->scan.format = v[4]; g->scan.stride = v[6]; g->scan.offset = v[10];
    return RESP_OK_NODATA;
  }
  }
  return 0;
}

static uint32_t command(gpu_state *g, const uint8_t *req, size_t size, uint8_t *resp, size_t resp_max, size_t *resp_size)
{
  const gpu_hdr *h = (const gpu_hdr *)req;
  const uint8_t *body = req + sizeof *h;
  size_t left = size - sizeof *h;
  *resp_size = sizeof *h;
  if (g->blobs) {
    uint32_t status = command3d(g, h, body, left, resp, resp_max, resp_size);
    if (status) return status;
  }
  switch (h->type) {
  case CMD_GET_DISPLAY_INFO: {
    if (resp_max < sizeof *h + 16 * 24) return RESP_ERR_UNSPEC;
    memset(resp + sizeof *h, 0, 16 * 24);
    uint32_t mode[6] = { 0, 0, g->width, g->height, 1, 0 };
    memcpy(resp + sizeof *h, mode, sizeof mode);
    *resp_size = sizeof *h + 16 * 24;
    return RESP_OK_DISPLAY_INFO;
  }
  case CMD_RESOURCE_CREATE_2D: {
    uint32_t a[4];
    if (left < 16) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 16);
    if (!a[0] || find(g, a[0]) || (g->blobs && blob_find(g, a[0]))) return RESP_ERR_INVALID_RESOURCE_ID;
    if (!a[2] || !a[3] || a[2] > 16384 || a[3] > 16384) return RESP_ERR_INVALID_PARAMETER;
    if (a[1] != FMT_BGRA && a[1] != FMT_BGRX && a[1] != FMT_ARGB && a[1] != FMT_XRGB && a[1] != FMT_RGBA &&
        a[1] != FMT_XBGR && a[1] != FMT_ABGR && a[1] != FMT_RGBX) return RESP_ERR_INVALID_PARAMETER;
    resource *r = NULL;
    for (unsigned i = 0; i < MAX_RES && !r; i++) if (!g->res[i].id) r = &g->res[i];
    if (!r) return RESP_ERR_OUT_OF_MEMORY;
    r->pixels = calloc((size_t)a[2] * a[3], 4);
    if (!r->pixels) return RESP_ERR_OUT_OF_MEMORY;
    r->id = a[0]; r->format = a[1]; r->width = a[2]; r->height = a[3];
    return RESP_OK_NODATA;
  }
  case CMD_RESOURCE_UNREF: {
    uint32_t id;
    if (left < 4) return RESP_ERR_INVALID_PARAMETER;
    memcpy(&id, body, 4);
    resource *r = find(g, id);
    if (!r) return RESP_ERR_INVALID_RESOURCE_ID;
    if (g->scanout_resource == id) g->scanout_resource = 0;
    release(r);
    return RESP_OK_NODATA;
  }
  case CMD_RESOURCE_ATTACH_BACKING: {
    uint32_t a[2];
    if (left < 8) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 8);
    resource *r = find(g, a[0]);
    if (!r) return RESP_ERR_INVALID_RESOURCE_ID;
    if (!a[1] || a[1] > MAX_BACKING || left - 8 < (size_t)a[1] * sizeof(gpu_entry)) return RESP_ERR_INVALID_PARAMETER;
    gpu_entry *entries = malloc(a[1] * sizeof *entries);
    if (!entries) return RESP_ERR_OUT_OF_MEMORY;
    memcpy(entries, body + 8, a[1] * sizeof *entries);
    free(r->backing);
    r->backing = entries;
    r->nbacking = a[1];
    return RESP_OK_NODATA;
  }
  case CMD_RESOURCE_DETACH_BACKING: {
    uint32_t id;
    if (left < 4) return RESP_ERR_INVALID_PARAMETER;
    memcpy(&id, body, 4);
    resource *r = find(g, id);
    if (!r) return RESP_ERR_INVALID_RESOURCE_ID;
    free(r->backing);
    r->backing = NULL;
    r->nbacking = 0;
    return RESP_OK_NODATA;
  }
  case CMD_SET_SCANOUT: {
    gpu_rect rect;
    uint32_t a[2];
    if (left < 24) return RESP_ERR_INVALID_PARAMETER;
    memcpy(&rect, body, 16);
    memcpy(a, body + 16, 8);
    if (a[0] != 0) return RESP_ERR_INVALID_SCANOUT_ID;
    if (a[1] == 0) { g->scanout_resource = 0; return RESP_OK_NODATA; }      // scanout disabled
    resource *r = find(g, a[1]);
    if (!r) return RESP_ERR_INVALID_RESOURCE_ID;
    if (!rect_ok(&rect, r->width, r->height)) return RESP_ERR_INVALID_PARAMETER;
    g->scanout_resource = a[1];
    g->scan.width = 0;
    return RESP_OK_NODATA;
  }
  case CMD_TRANSFER_TO_HOST_2D: {
    gpu_rect rect;
    uint64_t offset;
    uint32_t id;
    if (left < 28) return RESP_ERR_INVALID_PARAMETER;
    memcpy(&rect, body, 16);
    memcpy(&offset, body + 16, 8);
    memcpy(&id, body + 24, 4);
    resource *r = find(g, id);
    if (!r) return RESP_ERR_INVALID_RESOURCE_ID;
    if (!rect_ok(&rect, r->width, r->height) || !r->backing) return RESP_ERR_INVALID_PARAMETER;
    uint8_t *row = malloc((size_t)rect.w * 4);
    if (!row) return RESP_ERR_OUT_OF_MEMORY;
    uint32_t status = RESP_OK_NODATA;
    for (uint32_t y = 0; y < rect.h; y++) {
      // `offset` is where the rectangle's first pixel is in the backing; rows follow at the resource's stride.
      if (!backing_read(r, offset + (uint64_t)y * r->width * 4, row, (uint64_t)rect.w * 4)) { status = RESP_ERR_INVALID_PARAMETER; break; }
      convert(r->pixels + ((size_t)(rect.y + y) * r->width + rect.x) * 4, row, r->format, rect.w);
    }
    free(row);
    return status;
  }
  case CMD_RESOURCE_FLUSH: {
    gpu_rect rect;
    uint32_t id;
    if (left < 20) return RESP_ERR_INVALID_PARAMETER;
    memcpy(&rect, body, 16);
    memcpy(&id, body + 16, 4);
    if (g->blobs && blob_find(g, id)) return flush_blob(g, blob_find(g, id), &rect);
    resource *r = find(g, id);
    if (!r) return RESP_ERR_INVALID_RESOURCE_ID;
    if (!rect_ok(&rect, r->width, r->height)) return RESP_ERR_INVALID_PARAMETER;
    xh_display *d = xh_display_get();
    if (id == g->scanout_resource && d && d->pixels) {
      uint32_t w = rect.x < d->width ? (rect.w < d->width - rect.x ? rect.w : d->width - rect.x) : 0;
      uint32_t hgt = rect.y < d->height ? (rect.h < d->height - rect.y ? rect.h : d->height - rect.y) : 0;
      for (uint32_t y = 0; y < hgt && w; y++)
        memcpy(d->pixels + (size_t)(rect.y + y) * d->stride + (size_t)rect.x * 4,
               r->pixels + ((size_t)(rect.y + y) * r->width + rect.x) * 4, (size_t)w * 4);
      if (w && hgt) xh_display_flush(rect.x, rect.y, w, hgt);
      frame_shown(g, r->backing, r->nbacking);
    }
    return RESP_OK_NODATA;
  }
  }
  return RESP_ERR_UNSPEC;
}

// Runs the command of a chain and writes its response. Returns the response's length (0: a chain that is no command)
// and the command's header in `in`.
static int64_t serve(gpu_state *g, unsigned queue, xh_viov *iov, unsigned n, gpu_hdr *in, uint32_t *status)
{
  // Gather the readable parts (the command), find the writable ones (the response).
  size_t req_size = 0, resp_max = 0;
  for (unsigned i = 0; i < n; i++) { if (iov[i].writable) resp_max += iov[i].length; else req_size += iov[i].length; }
  if (req_size < sizeof(gpu_hdr) || req_size > (1u << 20) || resp_max < sizeof(gpu_hdr)) return 0;
  uint8_t *req = malloc(req_size), *resp = calloc(1, resp_max < 4096 ? 4096 : resp_max);
  if (!req || !resp) { free(req); free(resp); return 0; }
  size_t at = 0;
  for (unsigned i = 0; i < n; i++) if (!iov[i].writable) { memcpy(req + at, iov[i].data, iov[i].length); at += iov[i].length; }
  size_t resp_size = sizeof(gpu_hdr);
  gpu_hdr out = { 0 };
  memcpy(in, req, sizeof *in);
  out.type = queue == 0 ? command(g, req, req_size, resp, resp_max, &resp_size) : RESP_OK_NODATA;     // cursor queue: accepted, not drawn
  if (in->flags & FLAG_FENCE) { out.flags = in->flags & (FLAG_FENCE | FLAG_RING_IDX); out.fence = in->fence; out.ctx = in->ctx; out.ring = in->ring; }
  memcpy(resp, &out, sizeof out);
  if (resp_size > resp_max) resp_size = resp_max;
  at = 0;
  for (unsigned i = 0; i < n && at < resp_size; i++) {
    if (!iov[i].writable) continue;
    size_t len = resp_size - at < iov[i].length ? resp_size - at : iov[i].length;
    memcpy(iov[i].data, resp + at, len);
    at += len;
  }
  free(req);
  free(resp);
  *status = out.type;
  return (int64_t)resp_size;
}

static int64_t gpu_handle(xh_virtio *dev, unsigned queue, uint16_t head, xh_viov *iov, unsigned n)
{
  (void)head;
  gpu_hdr in;
  uint32_t status;
  return serve(xh_virtio_opaque(dev), queue, iov, n, &in, &status);
}

static uint32_t gpu_config(xh_virtio *dev, uint32_t offset, unsigned size)
{
  (void)size;
  gpu_state *g = xh_virtio_opaque(dev);
  // events_read, events_clear, num_scanouts = 1, num_capsets
  return offset == 8 ? 1 : offset == 12 && g->b ? g->b->capset_count(g->b) : 0;
}

static void teardown(gpu_state *g)
{
  for (unsigned i = 0; i < MAX_RES; i++) if (g->res[i].id) release(&g->res[i]);
  g->scanout_resource = 0;
  g->scan.width = 0;
  if (!g->blobs) return;
  for (unsigned i = 0; i < BLOB_SLOTS; i++) if (g->blobs[i].id) {
    if (g->b) g->b->unref(g->b, g->blobs[i].id);
    free(g->blobs[i].entries);
  }
  memset(g->blobs, 0, BLOB_SLOTS * sizeof *g->blobs);
  g->nblobs = 0;
  for (unsigned i = 0; i < MAX_CTX; i++) if (g->ctx[i]) { g->b->ctx_destroy(g->b, g->ctx[i]); g->ctx[i] = 0; }
  pthread_mutex_lock(&g->pl);
  pending *list = g->fences;
  g->fences = NULL;
  pthread_mutex_unlock(&g->pl);
  while (list) { pending *p = list; list = p->next; free(p); }                // their chains were the old queue's
}

static void gpu_reset(xh_virtio *dev) { teardown(xh_virtio_opaque(dev)); }

// ---- the 3D device's control queue: its own thread -------------------------------------------------------------------
static void wake(gpu_state *g, bool reset)
{
  pthread_mutex_lock(&g->pl);
  g->kicks++;
  if (reset) g->reset_requested = true;
  pthread_mutex_unlock(&g->pl);
  uint64_t one = 1;
  while (write(g->wake, &one, 8) < 0 && errno == EINTR) {}
}

static void gpu_kick(xh_virtio *dev, unsigned queue) { (void)queue; wake(xh_virtio_opaque(dev), false); }

// With the device's lock held (virtio.c): the queues are already gone; the worker forgets the rest before its next command.
static void gpu_reset_3d(xh_virtio *dev)
{
  gpu_state *g = xh_virtio_opaque(dev);
  g->gen++;
  wake(g, true);
}

// A command of the fast lane that names a resource the slow lane has not made yet (an app's IOSurface the render
// server attaches, the unref of an app's blob) waits for the slow lane's commands that came before it, and runs again.
static bool missing_resource(uint32_t status)
{
  return status == RESP_ERR_INVALID_RESOURCE_ID;
}

enum { RESP_PENDING_RETRY = 1 };                     // work(): not answered; run it again once the slow lane caught up

static bool slow_pending(gpu_state *g)
{
  pthread_mutex_lock(&g->pl);
  bool pending = g->slow_done != g->slow_queued;
  pthread_mutex_unlock(&g->pl);
  return pending;
}

// Serves a chain and answers it (now, or when its fence retires). With `may_wait`, a command that names a resource
// the slow lane may still be making is not answered: RESP_PENDING_RETRY, and the caller runs it again later.
static uint32_t work(gpu_state *g, uint16_t head, xh_viov *iov, unsigned n, uint64_t gen, bool may_wait)
{
  gpu_hdr in;
  uint32_t status = 0;
  int64_t written = serve(g, 0, iov, n, &in, &status);
  if (may_wait && missing_resource(status) && slow_pending(g)) return RESP_PENDING_RETRY;
  // A command fenced on a context's ring is returned when the ring passes the fence; any other at once (a fence on
  // the device's own ring has nothing to wait for: the 2D commands are done when they return).
  bool hold = written > 0 && status < RESP_ERR_UNSPEC && (in.flags & FLAG_FENCE) && (in.flags & FLAG_RING_IDX) && ctx_find(g, in.ctx);
  pending *p = calloc(1, sizeof *p);
  if (!p) return status;                             // the chain is lost; so is the guest's command
  *p = (pending){ .head = head, .written = (uint32_t)written, .ctx = in.ctx, .ring = in.ring, .fence = in.fence, .gen = gen };
  if (!hold) { push_pending(g, p); return status; }
  pthread_mutex_lock(&g->pl);
  p->next = g->fences;
  g->fences = p;
  pthread_mutex_unlock(&g->pl);
  // May retire it before it returns. A refused fence is retired here.
  if (g->b->submit_fence(g->b, in.ctx, in.ring, in.fence)) retire(g, in.ctx, (int)in.ring, in.fence);
  return status;
}

// The header of a chain, read without taking it (false: shorter than a header).
static bool peek_hdr(const xh_viov *iov, unsigned n, gpu_hdr *h)
{
  size_t at = 0;
  for (unsigned i = 0; i < n && at < sizeof *h; i++) {
    if (iov[i].writable) continue;
    size_t k = iov[i].length < sizeof *h - at ? iov[i].length : sizeof *h - at;
    memcpy((uint8_t *)h + at, iov[i].data, k);
    at += k;
  }
  return at == sizeof *h;
}

// Whether a command is the slow lane's: a context's, and not the render server's. Context creation stays here (it
// decides the lane of what follows).
static bool slow_lane(gpu_state *g, const gpu_hdr *h)
{
  if (!h->ctx || h->type == CMD_CTX_CREATE) return false;
  for (unsigned i = 0; i < MAX_CTX; i++) if (g->ctx[i] == h->ctx) return !g->ctx_fast[i];
  return false;
}

static void *slow_worker(void *arg)
{
  gpu_state *g = arg;
  pthread_mutex_lock(&g->pl);
  for (;;) {
    while (!g->slow_head) pthread_cond_wait(&g->slow_cv, &g->pl);
    slow_job *j = g->slow_head;
    g->slow_head = j->next;
    if (!g->slow_head) g->slow_tail = &g->slow_head;
    pthread_mutex_unlock(&g->pl);
    xh_virtio_lock(g->dev);
    bool current = j->gen == g->gen;                 // a chain from before a reset is the old queue's: dropped
    xh_virtio_unlock(g->dev);
    if (current) {
      pthread_mutex_lock(&g->cmd);
      work(g, j->head, j->iov, j->n, j->gen, false);
      pthread_mutex_unlock(&g->cmd);
    }
    free(j);
    pthread_mutex_lock(&g->pl);
    g->slow_done++;
    pthread_cond_broadcast(&g->slow_cv);
    pthread_cond_broadcast(&g->idle);
  }
  return NULL;
}

static void *worker(void *arg)
{
  gpu_state *g = arg;
  static _Thread_local xh_viov iov[WORKER_IOV];
  for (;;) {
    uint64_t count;
    if (read(g->wake, &count, 8) < 0 && errno != EINTR) break;
    pthread_mutex_lock(&g->pl);
    uint64_t upto = g->kicks;
    bool reset = g->reset_requested;
    g->reset_requested = false;
    pthread_mutex_unlock(&g->pl);
    if (reset) { pthread_mutex_lock(&g->cmd); teardown(g); pthread_mutex_unlock(&g->cmd); }
    for (;;) {
      uint16_t head;
      xh_virtio_lock(g->dev);
      uint64_t gen = g->gen;
      unsigned n = xh_virtio_pop(g->dev, 0, &head, iov, WORKER_IOV);
      xh_virtio_unlock(g->dev);
      if (!n) break;
      gpu_hdr h;
      pthread_mutex_lock(&g->cmd);
      bool slow = peek_hdr(iov, n, &h) && slow_lane(g, &h);
      pthread_mutex_unlock(&g->cmd);
      if (slow) {
        slow_job *j = malloc(sizeof *j + n * sizeof *iov);
        if (!j) continue;                            // the chain is lost; so is the guest's command
        *j = (slow_job){ .head = head, .n = n, .gen = gen };
        memcpy(j->iov, iov, n * sizeof *iov);
        pthread_mutex_lock(&g->pl);
        *g->slow_tail = j;
        g->slow_tail = &j->next;
        g->slow_queued++;
        pthread_cond_broadcast(&g->slow_cv);
        pthread_mutex_unlock(&g->pl);
        continue;
      }
      pthread_mutex_lock(&g->pl);
      uint64_t before = g->slow_queued;              // the slow lane's commands that came before this one
      pthread_mutex_unlock(&g->pl);
      pthread_mutex_lock(&g->cmd);
      uint32_t status = work(g, head, iov, n, gen, true);
      pthread_mutex_unlock(&g->cmd);
      if (status == RESP_PENDING_RETRY) {
        pthread_mutex_lock(&g->pl);
        while (g->slow_done < before) pthread_cond_wait(&g->slow_cv, &g->pl);
        pthread_mutex_unlock(&g->pl);
        pthread_mutex_lock(&g->cmd);
        work(g, head, iov, n, gen, false);
        pthread_mutex_unlock(&g->cmd);
      }
    }
    pthread_mutex_lock(&g->pl);
    g->served = upto;
    pthread_cond_broadcast(&g->idle);
    pthread_mutex_unlock(&g->pl);
  }
  return NULL;
}

void xh_virtio_gpu_idle(xh_virtio *dev)
{
  gpu_state *g = xh_virtio_opaque(dev);
  if (!g->blobs) return;
  pthread_mutex_lock(&g->pl);
  while (g->served != g->kicks || g->slow_done != g->slow_queued) pthread_cond_wait(&g->idle, &g->pl);
  pthread_mutex_unlock(&g->pl);
}

static const xh_virtio_ops gpu_ops = { .device_id = 16, .features = 0, .nqueues = 2, .handle = gpu_handle,
                                       .config_read = gpu_config, .reset = gpu_reset };
static const xh_virtio_ops gpu_3d_ops = { .device_id = 16, .features = F_VIRGL | F_RESOURCE_BLOB | F_CONTEXT_INIT | F_CREATE_GUEST_HANDLE, .nqueues = 2,
                                          .handle = gpu_handle, .config_read = gpu_config, .reset = gpu_reset_3d, .manual = 1, .kick = gpu_kick };

xh_virtio *xh_virtio_gpu_create(uint64_t base, unsigned spi, unsigned width, unsigned height)
{
  gpu_state *g = calloc(1, sizeof *g);
  if (!g) return NULL;
  g->width = width;
  g->height = height;
  return xh_virtio_create(base, spi, &gpu_ops, g, "virtio-gpu");
}

xh_virtio *xh_virtio_gpu_create_3d(uint64_t base, unsigned spi, unsigned width, unsigned height, xh_gpu3d *b)
{
  if (!b) return xh_virtio_gpu_create(base, spi, width, height);
  gpu_state *g = calloc(1, sizeof *g);
  pthread_t thread;
  if (!g) return NULL;
  g->width = width;
  g->height = height;
  g->b = b;
  g->blobs = calloc(BLOB_SLOTS, sizeof *g->blobs);
  g->wake = eventfd(0, EFD_CLOEXEC);
  pthread_mutex_init(&g->pl, NULL);
  pthread_cond_init(&g->idle, NULL);
  pthread_mutex_init(&g->cmd, NULL);
  pthread_cond_init(&g->slow_cv, NULL);
  g->slow_tail = &g->slow_head;
  if (!g->blobs || g->wake < 0 || !(g->dev = xh_virtio_create(base, spi, &gpu_3d_ops, g, "virtio-gpu"))) return NULL;
  b->retire = gpu_retire;
  b->dev = g->dev;
  if (pthread_create(&thread, NULL, worker, g)) return NULL;
  pthread_detach(thread);
  if (pthread_create(&thread, NULL, slow_worker, g)) return NULL;
  pthread_detach(thread);
  return g->dev;
}
