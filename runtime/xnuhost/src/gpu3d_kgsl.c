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

#include "virgl_context.h"
#include "virgl_resource.h"
#include "virgl_util.h"
#include "drm_hw.h"
#include "drm2kgsl/drm2kgsl_renderer.h"

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

static void fence_retired(struct virgl_context *ctx, uint32_t ring, uint64_t fence)
{
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
  return 0;
}

static void ctx_destroy(xh_gpu3d *b, uint32_t id) { (void)b; virgl_context_remove(id); }

static int submit(xh_gpu3d *b, uint32_t id, const void *commands, size_t size)
{
  (void)b;
  struct virgl_context *ctx = virgl_context_lookup(id);
  if (!ctx || (size & 3)) return -EINVAL;
  // drm_context reads the stream as 32-bit words in place: the device's copy is malloc'ed, so aligned.
  return ctx->submit_cmd(ctx, commands, size) ? -EINVAL : 0;
}

static int submit_fence(xh_gpu3d *b, uint32_t id, uint32_t ring, uint64_t fence)
{
  (void)b;
  struct virgl_context *ctx = virgl_context_lookup(id);
  if (!ctx) return -EINVAL;
  // Mergeable, as crosvm asks: a ring may retire only its latest fence, and the device returns every earlier one.
  return ctx->submit_fence(ctx, 1 /* VIRGL_RENDERER_FENCE_FLAG_MERGEABLE */, ring, fence);
}

static int create_blob(xh_gpu3d *b, uint32_t id, uint32_t res, uint32_t blob_mem, uint32_t flags, uint64_t blob_id,
                       uint64_t size, const xh_gpu_entry *entries, unsigned nentries)
{
  kgsl *k = b->opaque;
  struct virgl_context_blob blob = { 0 };
  if (!res || virgl_resource_lookup(res)) return -EINVAL;
  int guest_fd = -1;
  if (nentries) {
    guest_fd = xh_gpu3d_udmabuf(k->ram_fd, k->ram_phys, k->ram_size, entries, nentries);
    if (guest_fd < 0) {
      int error = errno;
      say(0, "gpu: udmabuf over %u segments of a %llu byte blob: %s", nentries, (unsigned long long)size, strerror(error));
      return -error;
    }
  }
  if (blob_mem == XH_GPU_BLOB_GUEST) {
    // The display's surface: known as a dma-buf, for a context that attaches it.
    struct virgl_resource *r = virgl_resource_create_from_fd(res, VIRGL_RESOURCE_FD_DMABUF, guest_fd, NULL, 0, NULL);
    if (!r) { close(guest_fd); return -ENOMEM; }
    r->map_info = VIRGL_RENDERER_MAP_CACHE_CACHED;
    r->map_size = size;
    return 0;
  }
  struct virgl_context *ctx = virgl_context_lookup(id);
  if (!ctx) { if (guest_fd >= 0) close(guest_fd); return -EINVAL; }
  // The pages of a guest-allocated BO are parked in the context for the get_blob that follows (it takes the fd).
  if (guest_fd >= 0 && (!ctx->set_guest_blob_fd || ctx->set_guest_blob_fd(ctx, blob_id, guest_fd))) { close(guest_fd); return -EINVAL; }
  int r = ctx->get_blob(ctx, res, blob_id, size, flags & ~(uint32_t)XH_GPU_BLOB_FLAG_GUEST_HANDLE, &blob);
  if (r) return r < 0 ? r : -r;
  if (blob.type != VIRGL_RESOURCE_FD_DMABUF && blob.type != VIRGL_RESOURCE_FD_SHM) return -EINVAL;
  struct virgl_resource *made = virgl_resource_create_from_fd(res, blob.type, blob.u.fd, NULL, 0, &blob.opaque_fd_metadata);
  if (!made) { close(blob.u.fd); return -ENOMEM; }
  made->map_info = blob.map_info;
  made->map_size = size;
  made->map_ptr = blob.map_ptr;
  made->fd_offset = blob.fd_offset;
  return 0;
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
  say(1, "gpu: drm2kgsl on %s: chip 0x%llx (gpu id %u), GPU addresses 0x%llx + %llu GiB, %u priorities; pool %llu MiB",
      KGSL_DEVICE_NODE, (unsigned long long)k->capset.u.msm.chip_id, k->capset.u.msm.gpu_id, (unsigned long long)k->capset.u.msm.va_start,
      (unsigned long long)(k->capset.u.msm.va_size >> 30), k->capset.u.msm.priorities, (unsigned long long)(pool_size >> 20));
  return the;
}
