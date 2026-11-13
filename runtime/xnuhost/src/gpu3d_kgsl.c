// SPDX-License-Identifier: MIT
// virtio-gpu 3D on KGSL: Droid-VM's drm2kgsl (external/virglrenderer src/drm/drm2kgsl, MIT), linked into xnuhost.
// drm2kgsl is virglrenderer's DRM native context for the msm protocol, served on /dev/kgsl-3d0. This file is what
// virglrenderer.c and crosvm's rutabaga are around it in a DroidVM, cut down to the one renderer: the context and
// resource tables, a blob made from a context object, the guest's pages handed over as a udmabuf, and the fences.
// virglrenderer.c itself is not built: it pulls in the GL renderer (epoxy, EGL) and libdrm, and Android has neither.
//
// The pool (/arm-io/virtio-gpu drm-host-pool) is drm2kgsl's "arena": it reads the memfd, the offset in it, the host
// address and the size from CROSVM_DRM2KGSL_ARENA_*, set here before the first context. A context's msm shmem is
// carved from it; every BO is the guest's own pages (MSM_BO_GUEST_ALLOC).
#include "gpu3d.h"
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/resource.h>

#include "virgl_context.h"
#include "virgl_resource.h"
#include "virgl_util.h"
#include "drm_hw.h"
#include "drm2kgsl/drm2kgsl_renderer.h"
#include "oneios_drm2kgsl.h"

typedef struct {
  xh_gpu3d base;
  struct virgl_renderer_capset_drm capset;
  int ram_fd;
  uint64_t ram_phys, ram_size;
  uint8_t *pool_host;
  uint64_t pool_size;
} kgsl;

static xh_gpu3d_log sink;

static void say(int level, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static void say(int level, const char *fmt, ...)
{
  char line[512];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(line, sizeof line, fmt, ap);
  va_end(ap);
  size_t n = strlen(line);
  while (n && line[n - 1] == '\n') line[--n] = 0;
  if (sink) sink(level, line);
}

// ---- what the virglrenderer sources expect of their surroundings --------------------------------------------------
// The library's log (drm_log, drm_err, drm_dbg), drm2kgsl's diagnostics (it writes them to Android's log, which a
// static binary has no library for) and mesa's two OS helpers (os_misc.c wants Android's liblog too).
static void virgl_line(enum virgl_log_level_flags level, const char *message, void *data)
{
  (void)data;
  say(level >= VIRGL_LOG_LEVEL_WARNING ? 0 : level == VIRGL_LOG_LEVEL_INFO ? 1 : 2, "drm2kgsl: %s", message);
}

int __android_log_vprint(int priority, const char *tag, const char *fmt, va_list ap);
int __android_log_vprint(int priority, const char *tag, const char *fmt, va_list ap)
{
  char line[512];
  (void)priority; (void)tag;
  vsnprintf(line, sizeof line, fmt, ap);
  say(2, "drm2kgsl: %s", line);
  return 0;
}

void os_log_message(const char *message);
void os_log_message(const char *message) { say(1, "drm2kgsl: %s", message); }
const char *os_get_option(const char *name);
const char *os_get_option(const char *name) { return getenv(name); }

// ---- the renderer -------------------------------------------------------------------------------------------------
static unsigned capset_count(xh_gpu3d *b) { (void)b; return 1; }

static bool capset_info(xh_gpu3d *b, unsigned index, uint32_t *id, uint32_t *version, uint32_t *size)
{
  (void)b;
  if (index) return false;
  *id = XH_GPU_CAPSET_DRM;
  *version = 0;
  *size = sizeof(struct virgl_renderer_capset_drm);
  return true;
}

static size_t capset(xh_gpu3d *b, uint32_t id, uint32_t version, void *out, size_t max)
{
  kgsl *k = b->opaque;
  (void)version;
  if (id != XH_GPU_CAPSET_DRM || max < sizeof k->capset) return 0;
  memcpy(out, &k->capset, sizeof k->capset);
  return sizeof k->capset;
}

static xh_gpu3d *the;      // virglrenderer's tables are the process's, and so is drm2kgsl's VBO

// ---- the host's share of a frame ------------------------------------------------------------------------------------
// The guest's GPU report times a render-server frame from its submission to the fence it sees signalled (9-11 ms on the
// phone with KGSL 7 % busy, 2026-11-12). This times the host's part of the same path, per 10 s, one log line:
//   gpu: 10 s host: backboardd fences 98, submit->retired mean 2100 us max 9000 us; submit calls mean 150 us max 900 us;
//        other fences 30, mean 1800 us
// "submit->retired" is from the fence's command reaching the renderer to drm2kgsl's retire callback (the GPU's work and
// KGSL's wake-up); "submit calls" is the time in drm2kgsl's submit (the KGSL ioctl and the command's checks). The rest of
// the guest's figure is the virtio round trip and the guest's own wake-ups.
#include <pthread.h>
#include <time.h>
enum { STAT_RINGS = 4, STAT_PENDING = 64, STAT_CTX = 128 };
typedef struct { uint64_t fence, at; } stat_pending;
typedef struct {
  uint32_t id;
  bool render;
  unsigned head[STAT_RINGS], count[STAT_RINGS];
  stat_pending pending[STAT_RINGS][STAT_PENDING];
} stat_ctx;
static struct {
  pthread_mutex_t lock;
  stat_ctx ctx[STAT_CTX];
  uint64_t since, render_n, render_sum, render_max, other_n, other_sum, submit_n, submit_sum, submit_max, blob_n, blob_sum, blob_max, holds, keepalives, udma_sum;
  uint64_t dropped, plain, unreadable;       // the import line's last figures (0007)
  bool imports_said;
} st = { .lock = PTHREAD_MUTEX_INITIALIZER };

static uint64_t now_us(void)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (uint64_t)t.tv_sec * 1000000u + (uint64_t)t.tv_nsec / 1000u;
}

static stat_ctx *stat_find(uint32_t id)      // with st.lock held
{
  for (unsigned i = 0; i < STAT_CTX; i++) if (st.ctx[i].id == id) return &st.ctx[i];
  return NULL;
}

static void stat_report(uint64_t now)         // with st.lock held
{
  if (!st.since) { st.since = now; return; }
  if (now - st.since < 10000000u) return;
  uint64_t holds = 0, keepalives = 0;
  drm2kgsl_renderer_hold_stats(&holds, &keepalives);
  if (st.render_n || st.other_n || st.blob_n)
    say(1, "gpu: %llu s host: backboardd fences %llu, submit->retired mean %llu us max %llu us; submit calls mean %llu us max %llu us; "
        "other fences %llu, mean %llu us; blobs made %llu, mean %llu us max %llu us (udmabufs %llu us of it); clock held +%llu times, keep-alives +%llu",
        (unsigned long long)((now - st.since) / 1000000u),
        (unsigned long long)st.render_n, (unsigned long long)(st.render_n ? st.render_sum / st.render_n : 0), (unsigned long long)st.render_max,
        (unsigned long long)(st.submit_n ? st.submit_sum / st.submit_n : 0), (unsigned long long)st.submit_max,
        (unsigned long long)st.other_n, (unsigned long long)(st.other_n ? st.other_sum / st.other_n : 0),
        (unsigned long long)st.blob_n, (unsigned long long)(st.blob_n ? st.blob_sum / st.blob_n : 0), (unsigned long long)st.blob_max,
        (unsigned long long)(st.blob_n ? st.udma_sum / st.blob_n : 0), (unsigned long long)(holds - st.holds), (unsigned long long)(keepalives - st.keepalives));
  st.holds = holds;
  st.keepalives = keepalives;
  // KGSL's coherence of the guest's imports (drm2kgsl patch 0007): a line the first time, then only when an import
  // lost its IO coherence, was imported without it, or could not be read back (docs/gpu.md 2.1, "0007").
  uint64_t imports, checked, kept, dropped, plain, unreadable;
  drm2kgsl_renderer_import_stats(&imports, &checked, &kept, &dropped, &plain, &unreadable);
  if (checked && (!st.imports_said || dropped != st.dropped || plain != st.plain || unreadable != st.unreadable)) {
    say(1, "gpu: KGSL imports %llu, read back %llu: IO-coherent %llu, coherence dropped by KGSL %llu, not coherent as asked %llu, "
        "not readable %llu", (unsigned long long)imports, (unsigned long long)checked, (unsigned long long)kept,
        (unsigned long long)dropped, (unsigned long long)plain, (unsigned long long)unreadable);
    st.imports_said = true;
    st.dropped = dropped; st.plain = plain; st.unreadable = unreadable;
  }
  st.since = now;
  st.render_n = st.render_sum = st.render_max = st.other_n = st.other_sum = st.submit_n = st.submit_sum = st.submit_max = 0;
  st.blob_n = st.blob_sum = st.blob_max = st.udma_sum = 0;
}

static void fence_retired(struct virgl_context *ctx, uint32_t ring, uint64_t fence)
{
  uint64_t now = now_us();
  pthread_mutex_lock(&st.lock);
  stat_ctx *c = stat_find(ctx->ctx_id);
  if (c && ring < STAT_RINGS) {
    // Mergeable fences: this one and every earlier one of the ring are done.
    while (c->count[ring]) {
      stat_pending *p = &c->pending[ring][c->head[ring]];
      if (p->fence > fence) break;
      uint64_t took = now - p->at;
      if (c->render) { st.render_n++; st.render_sum += took; if (took > st.render_max) st.render_max = took; }
      else { st.other_n++; st.other_sum += took; }
      c->head[ring] = (c->head[ring] + 1) % STAT_PENDING;
      c->count[ring]--;
    }
  }
  stat_report(now);
  pthread_mutex_unlock(&st.lock);
  if (the && the->retire) the->retire(the->dev, ctx->ctx_id, ring, fence);
}

static int ctx_create(xh_gpu3d *b, uint32_t id, uint32_t capset_id, const char *name, size_t name_length)
{
  (void)b;
  if (capset_id != XH_GPU_CAPSET_DRM) return -EINVAL;
  if (virgl_context_lookup(id)) return -EEXIST;
  int fd = open(KGSL_DEVICE_NODE, O_RDWR | O_CLOEXEC);
  if (fd < 0) return -errno;
  struct virgl_context *ctx = drm2kgsl_renderer_create(fd, name_length, name);        // owns fd, also when it fails
  if (!ctx) return -ENOMEM;
  ctx->ctx_id = id;
  ctx->capset_id = VIRGL_RENDERER_CAPSET_DRM;
  ctx->fence_retire = fence_retired;
  int r = virgl_context_add(ctx);
  if (r) { ctx->destroy(ctx); return -ENOMEM; }
  pthread_mutex_lock(&st.lock);
  stat_ctx *c = stat_find(0);
  if (c) {
    memset(c, 0, sizeof *c);
    c->id = id;
    c->render = name && name_length == 14 && !memcmp(name, "xnu:backboardd", 14);
  }
  pthread_mutex_unlock(&st.lock);
  return 0;
}

static void ctx_destroy(xh_gpu3d *b, uint32_t id)
{
  (void)b;
  virgl_context_remove(id);
  pthread_mutex_lock(&st.lock);
  stat_ctx *c = stat_find(id);
  if (c && id) memset(c, 0, sizeof *c);
  pthread_mutex_unlock(&st.lock);
}

static int submit(xh_gpu3d *b, uint32_t id, const void *commands, size_t size)
{
  (void)b;
  struct virgl_context *ctx = virgl_context_lookup(id);
  if (!ctx || (size & 3)) return -EINVAL;
  // drm_context reads the stream as 32-bit words in place: the device's copy is malloc'ed, so aligned.
  uint64_t start = now_us();
  int r = ctx->submit_cmd(ctx, commands, size) ? -EINVAL : 0;
  uint64_t took = now_us() - start;
  pthread_mutex_lock(&st.lock);
  st.submit_n++; st.submit_sum += took; if (took > st.submit_max) st.submit_max = took;
  pthread_mutex_unlock(&st.lock);
  return r;
}

static int submit_fence(xh_gpu3d *b, uint32_t id, uint32_t ring, uint64_t fence)
{
  (void)b;
  struct virgl_context *ctx = virgl_context_lookup(id);
  if (!ctx) return -EINVAL;
  pthread_mutex_lock(&st.lock);
  stat_ctx *c = stat_find(id);
  if (c && ring < STAT_RINGS && c->count[ring] < STAT_PENDING) {
    c->pending[ring][(c->head[ring] + c->count[ring]) % STAT_PENDING] = (stat_pending){ .fence = fence, .at = now_us() };
    c->count[ring]++;
  }
  pthread_mutex_unlock(&st.lock);
  // Mergeable, as crosvm asks: a ring may retire only its latest fence, and the device returns every earlier one.
  return ctx->submit_fence(ctx, 1 /* VIRGL_RENDERER_FENCE_FLAG_MERGEABLE */, ring, fence);
}

// ---- blobs of more than one udmabuf ---------------------------------------------------------------------------------
// One udmabuf holds at most 64 MiB and 1024 runs of pages (gpu3d.c), so a larger or more scattered blob is several,
// "chunks". virglrenderer's resource has room for one fd; the chunks of such a resource are kept here, by resource id,
// for a second context that attaches it (drm2kgsl imports every chunk again). Only the device's thread comes here.
typedef struct { uint32_t res; unsigned n; xh_gpu_chunk *chunks; } chunked;
static chunked *chunked_list;
static unsigned chunked_count, chunked_room;

static chunked *chunked_find(uint32_t res)
{
  for (unsigned i = 0; i < chunked_count; i++) if (chunked_list[i].res == res) return &chunked_list[i];
  return NULL;
}

static bool chunked_add(uint32_t res, xh_gpu_chunk *chunks, unsigned n)
{
  if (chunked_count == chunked_room) {
    chunked *more = realloc(chunked_list, (chunked_room ? chunked_room * 2 : 16) * sizeof *more);
    if (!more) return false;
    chunked_list = more;
    chunked_room = chunked_room ? chunked_room * 2 : 16;
  }
  chunked_list[chunked_count++] = (chunked){ .res = res, .n = n, .chunks = chunks };
  return true;
}

static void chunked_remove(uint32_t res)
{
  chunked *c = chunked_find(res);
  if (!c) return;
  xh_gpu3d_chunks_free(c->chunks, c->n);
  *c = chunked_list[--chunked_count];
}

static int create_blob_now(xh_gpu3d *b, uint32_t id, uint32_t res, uint32_t blob_mem, uint32_t flags, uint64_t blob_id,
                           uint64_t size, const xh_gpu_entry *entries, unsigned nentries)
{
  kgsl *k = b->opaque;
  struct virgl_context_blob blob = { 0 };
  if (!res || virgl_resource_lookup(res)) return -EINVAL;
  xh_gpu_chunk *chunks = NULL;         // the guest's pages; with more than one chunk the list stays ours (chunked_add)
  int nchunks = 0;
  if (nentries) {
    uint64_t u0 = now_us();
    nchunks = xh_gpu3d_udmabufs(k->ram_fd, k->ram_phys, k->ram_size, entries, nentries, &chunks);
    uint64_t u1 = now_us();
    pthread_mutex_lock(&st.lock);
    st.udma_sum += u1 - u0;
    pthread_mutex_unlock(&st.lock);
    if (nchunks < 0) {
      int error = errno;
      say(0, "gpu: udmabuf over %u segments of a %llu byte blob: %s", nentries, (unsigned long long)size, strerror(error));
      return -error;
    }
    if (nchunks > 1) say(2, "gpu: resource %u: %llu bytes in %u segments as %d udmabufs", res, (unsigned long long)size, nentries, nchunks);
  }
  unsigned n = (unsigned)nchunks;
  int first = n ? fcntl(chunks[0].fd, F_DUPFD_CLOEXEC, 0) : -1;       // the resource's own fd
  if (n && first < 0) { xh_gpu3d_chunks_free(chunks, n); return -EMFILE; }
  if (blob_mem == XH_GPU_BLOB_GUEST) {
    // The display's surface: known as a dma-buf (its first, when it is chunked), for a context that attaches it.
    struct virgl_resource *r = n ? virgl_resource_create_from_fd(res, VIRGL_RESOURCE_FD_DMABUF, first, NULL, 0, NULL) : NULL;
    if (!r || (n > 1 && !chunked_add(res, chunks, n))) {
      if (r) virgl_resource_remove(res); else if (first >= 0) close(first);
      xh_gpu3d_chunks_free(chunks, n);
      return -ENOMEM;
    }
    if (n == 1) xh_gpu3d_chunks_free(chunks, n);
    r->map_info = VIRGL_RENDERER_MAP_CACHE_CACHED;
    r->map_size = size;
    return 0;
  }
  if (first >= 0) close(first);          // a context's blob: get_blob gives the resource its fd
  struct virgl_context *ctx = virgl_context_lookup(id);
  if (!ctx) { xh_gpu3d_chunks_free(chunks, n); return -EINVAL; }
  // The pages of a guest-allocated BO are parked in the context for the get_blob that follows. It takes the fds it
  // is given: copies of them, so that the chunk list above stays whole.
  if (n) {
    int fds[n];
    uint64_t sizes[n];
    bool ok = true;
    for (unsigned i = 0; i < n; i++) { sizes[i] = chunks[i].size; if ((fds[i] = fcntl(chunks[i].fd, F_DUPFD_CLOEXEC, 0)) < 0) ok = false; }
    if (!ok) for (unsigned i = 0; i < n; i++) if (fds[i] >= 0) close(fds[i]);
    if (!ok || drm2kgsl_renderer_set_guest_blob_chunks(ctx, blob_id, fds, sizes, n)) { xh_gpu3d_chunks_free(chunks, n); return -EINVAL; }
  }
  int r = ctx->get_blob(ctx, res, blob_id, size, flags & ~(uint32_t)XH_GPU_BLOB_FLAG_GUEST_HANDLE, &blob);
  if (r) { xh_gpu3d_chunks_free(chunks, n); return r < 0 ? r : -r; }
  if (blob.type != VIRGL_RESOURCE_FD_DMABUF && blob.type != VIRGL_RESOURCE_FD_SHM) { xh_gpu3d_chunks_free(chunks, n); return -EINVAL; }
  struct virgl_resource *made = virgl_resource_create_from_fd(res, blob.type, blob.u.fd, NULL, 0, &blob.opaque_fd_metadata);
  if (!made || (n > 1 && !chunked_add(res, chunks, n))) {
    if (made) virgl_resource_remove(res); else close(blob.u.fd);
    xh_gpu3d_chunks_free(chunks, n);
    return -ENOMEM;
  }
  if (n == 1) xh_gpu3d_chunks_free(chunks, n);
  made->map_info = blob.map_info;
  made->map_size = size;
  made->map_ptr = blob.map_ptr;
  made->fd_offset = blob.fd_offset;
  return 0;
}

// The cost of a blob on the host (udmabufs of its pages, drm2kgsl's import and bind): one per guest BO, and a process
// drawing icons made 1335 under 1 MiB (phone, 2026-11-12). Counted in the 10 s host line.
static int create_blob(xh_gpu3d *b, uint32_t id, uint32_t res, uint32_t blob_mem, uint32_t flags, uint64_t blob_id,
                       uint64_t size, const xh_gpu_entry *entries, unsigned nentries)
{
  uint64_t start = now_us();
  int r = create_blob_now(b, id, res, blob_mem, flags, blob_id, size, entries, nentries);
  uint64_t took = now_us() - start;
  pthread_mutex_lock(&st.lock);
  st.blob_n++; st.blob_sum += took; if (took > st.blob_max) st.blob_max = took;
  stat_report(start + took);
  pthread_mutex_unlock(&st.lock);
  return r;
}

static int map_blob(xh_gpu3d *b, uint32_t res, uint64_t *pool_offset, uint32_t *map_info)
{
  kgsl *k = b->opaque;
  struct virgl_resource *r = virgl_resource_lookup(res);
  uint8_t *at = r ? r->map_ptr : NULL;
  if (!at || at < k->pool_host || at >= k->pool_host + k->pool_size || r->map_size > (uint64_t)(k->pool_host + k->pool_size - at)) return -EINVAL;
  *pool_offset = (uint64_t)(at - k->pool_host);
  *map_info = r->map_info;
  return 0;
}

static int attach(xh_gpu3d *b, uint32_t id, uint32_t res, bool on)
{
  (void)b;
  struct virgl_context *ctx = virgl_context_lookup(id);
  struct virgl_resource *r = virgl_resource_lookup(res);
  if (!ctx || !r) return -EINVAL;
  chunked *c = on ? chunked_find(res) : NULL;
  if (c) {
    // A resource of several udmabufs: the context imports each (nothing, when it made the resource itself).
    int fds[c->n];
    uint64_t sizes[c->n];
    for (unsigned i = 0; i < c->n; i++) { fds[i] = c->chunks[i].fd; sizes[i] = c->chunks[i].size; }
    int error = drm2kgsl_renderer_attach_chunks(ctx, res, fds, sizes, c->n);
    if (error) return error;
  }
  if (on) ctx->attach_resource(ctx, r); else ctx->detach_resource(ctx, r);
  return 0;
}

static bool detach_one(struct virgl_context *ctx, void *data)
{
  ctx->detach_resource(ctx, data);
  return true;
}

static void unref(xh_gpu3d *b, uint32_t res)
{
  (void)b;
  struct virgl_resource *r = virgl_resource_lookup(res);
  if (!r) return;
  struct virgl_context_foreach_args args = { .callback = detach_one, .data = r };
  virgl_context_foreach(&args);
  virgl_resource_remove(res);
  chunked_remove(res);
}

xh_gpu3d *xh_gpu3d_kgsl_open(int ram_fd, uint64_t ram_phys, uint64_t ram_size, void *pool_host, uint64_t pool_pa,
                             uint64_t pool_size, xh_gpu3d_log log, const char **why)
{
  static char reason[160];
  static const char *const nodes[] = { KGSL_DEVICE_NODE, "/dev/udmabuf", "/dev/dma_heap/system" };
  *why = reason;
  if (the) { snprintf(reason, sizeof reason, "the renderer is already open"); return NULL; }
  sink = log;
  for (unsigned i = 0; i < 3; i++) {
    int fd = open(nodes[i], (i == 2 ? O_RDONLY : O_RDWR) | O_CLOEXEC);
    if (fd < 0) { snprintf(reason, sizeof reason, "%s: %s", nodes[i], strerror(errno)); return NULL; }
    close(fd);
  }
  // Every guest BO is one fd per udmabuf chunk here and one more in each context that imports it, and every Metal client
  // is a KGSL fd and a dma-heap fd: with many clients the soft limit of open files is what runs out first (Droid-VM's
  // crosvm ran out of fds under GFXBench and raised it the same way). Up to the hard limit, which needs no privilege.
  struct rlimit files;
  if (!getrlimit(RLIMIT_NOFILE, &files) && files.rlim_cur < files.rlim_max) {
    rlim_t was = files.rlim_cur;
    files.rlim_cur = files.rlim_max;
    if (!setrlimit(RLIMIT_NOFILE, &files)) say(1, "gpu: open files: soft limit %llu -> %llu", (unsigned long long)was, (unsigned long long)files.rlim_max);
  }
  uint64_t chunk_bytes;
  unsigned chunk_runs;
  xh_gpu3d_udmabuf_limits(&chunk_bytes, &chunk_runs);
  // One udmabuf of one page from the pool: the memfd is sealed against shrinking and the kernel takes it.
  xh_gpu_entry first = { .addr = pool_pa, .length = 4096 };
  int probe = xh_gpu3d_udmabuf(ram_fd, ram_phys, ram_size, &first, 1);
  if (probe < 0) { snprintf(reason, sizeof reason, "udmabuf over the RAM memfd: %s", strerror(errno)); return NULL; }
  close(probe);
  if (pool_pa < ram_phys || pool_size > ram_size || pool_pa - ram_phys > ram_size - pool_size || pool_size <= (2u << 20)) {
    snprintf(reason, sizeof reason, "the pool must lie in the RAM and be larger than 2 MiB");       // drm2kgsl never hands out its first 2 MiB
    return NULL;
  }
  char value[32];
  snprintf(value, sizeof value, "%d", ram_fd);                             setenv("CROSVM_DRM2KGSL_ARENA_FD", value, 1);
  snprintf(value, sizeof value, "%llu", (unsigned long long)(pool_pa - ram_phys)); setenv("CROSVM_DRM2KGSL_ARENA_FD_OFFSET", value, 1);
  snprintf(value, sizeof value, "%llu", (unsigned long long)(uintptr_t)pool_host); setenv("CROSVM_DRM2KGSL_ARENA_HOST_VA", value, 1);
  snprintf(value, sizeof value, "%llu", (unsigned long long)pool_size);    setenv("CROSVM_DRM2KGSL_ARENA_SIZE", value, 1);
  if (!getenv("CROSVM_DRM2KGSL_DIAG")) setenv("CROSVM_DRM2KGSL_DIAG", "0", 1);         // its periodic counters: on request only

  kgsl *k = calloc(1, sizeof *k);
  if (!k) { snprintf(reason, sizeof reason, "no memory"); return NULL; }
  virgl_log_set_handler(virgl_line, NULL, NULL);
  if (virgl_context_table_init() || virgl_resource_table_init(NULL)) { snprintf(reason, sizeof reason, "no memory for the renderer's tables"); free(k); return NULL; }
  int fd = open(KGSL_DEVICE_NODE, O_RDWR | O_CLOEXEC);
  k->capset.context_type = VIRTGPU_DRM_CONTEXT_MSM;
  int r = fd < 0 ? -errno : drm2kgsl_renderer_probe(fd, &k->capset);       // keeps its own descriptor for the VBO
  if (fd >= 0) close(fd);
  if (r) {
    snprintf(reason, sizeof reason, "KGSL refused the probe (%d): no device info, or no virtual buffer object (KGSL_MEMFLAGS_VBO)", r);
    free(k);
    return NULL;
  }
  k->ram_fd = ram_fd; k->ram_phys = ram_phys; k->ram_size = ram_size;
  k->pool_host = pool_host; k->pool_size = pool_size;
  k->base = (xh_gpu3d){ .opaque = k, .capset_count = capset_count, .capset_info = capset_info, .capset = capset, .ctx_create = ctx_create,
                        .ctx_destroy = ctx_destroy, .submit = submit, .submit_fence = submit_fence, .create_blob = create_blob,
                        .map_blob = map_blob, .attach = attach, .unref = unref };
  the = &k->base;
  say(1, "gpu: drm2kgsl on %s: chip 0x%llx (gpu id %u), GPU addresses 0x%llx + %llu GiB, %u priorities; pool %llu MiB; "
      "guest pages in udmabufs of up to %llu MiB and %u runs",
      KGSL_DEVICE_NODE, (unsigned long long)k->capset.u.msm.chip_id, k->capset.u.msm.gpu_id, (unsigned long long)k->capset.u.msm.va_start,
      (unsigned long long)(k->capset.u.msm.va_size >> 30), k->capset.u.msm.priorities, (unsigned long long)(pool_size >> 20),
      (unsigned long long)(chunk_bytes >> 20), chunk_runs);
  return the;
}
