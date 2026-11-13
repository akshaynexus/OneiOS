// SPDX-License-Identifier: MIT
// The renderer (src/gpu3d_kgsl.c with the patched drm2kgsl) on the build host against a stand-in for KGSL: the memory
// half only (objects, their GPU addresses, VBO binds), which is where the Galaxy S24 Ultra's kernel differs from what
// drm2kgsl assumed. `ioctl` and `open` are wrapped at link time: requests of KGSL's ioctl type go to the stand-in, the
// rest (the real /dev/udmabuf of this host) to the kernel. The stand-in keeps what Samsung's graphics-kernel keeps:
//   - GPU addresses for a 64-bit process's objects come from 0x4000000000..0x8000000000 (KGSL_IOMMU_VA_BASE64/END64),
//     first fit; a VBO takes its whole size there, and every imported or allocated object takes its own size there too
//     (kgsl_mem_entry_attach_to_process -> kgsl_mmu_get_gpuaddr), ENOMEM when nothing is left;
//   - a VBO may be as large as the range (gpumem_max_va_size), not larger (EINVAL);
//   - BIND_RANGES takes children of the same process, whole pages, inside the child and inside the VBO.
// Each scenario runs in a process of its own (the renderer is one per process). Skipped without /dev/udmabuf.
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "gpu3d.h"
#include "oneios_drm2kgsl.h"
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <linux/types.h>
#define __user
#include "drm2kgsl/msm_kgsl.h"

// ---- the stand-in -----------------------------------------------------------------------------------------------------
#define VA_BASE 0x4000000000ull
#define VA_END  0x8000000000ull
typedef struct { uint32_t id; uint64_t base, size, flags; bool vbo; } object;
static object objects[256];
static unsigned nobjects, next_id = 1, imports, frees;
// As KGSL on an MMU without IO coherence (kgsl_memdesc_init): an import keeps every flag it asked but that one.
static bool no_io_coherence;
typedef struct { uint32_t vbo, child, op; uint64_t target, length, child_offset; unsigned call; } bound;
static bound binds[256];
static unsigned nbinds, bind_calls;

static object *object_find(uint32_t id)
{
  for (unsigned i = 0; i < nobjects; i++) if (objects[i].id == id) return &objects[i];
  return NULL;
}

static int object_new(uint64_t size, bool vbo, uint32_t *id)
{
  uint64_t at = VA_BASE;
  for (bool moved = true; moved;) {                    // first fit
    moved = false;
    for (unsigned i = 0; i < nobjects; i++)
      if (at < objects[i].base + objects[i].size && objects[i].base < at + size) { at = objects[i].base + objects[i].size; moved = true; }
  }
  if (!size || at + size > VA_END || nobjects == 256) { errno = ENOMEM; return -1; }
  objects[nobjects++] = (object){ .id = *id = next_id++, .base = at, .size = size, .vbo = vbo };
  return 0;
}

static int fake_kgsl(unsigned long request, void *arg)
{
  switch (request) {
  case IOCTL_KGSL_DEVICE_GETPROPERTY: {
    struct kgsl_device_getproperty *p = arg;
    if (p->type != KGSL_PROP_DEVICE_INFO || p->sizebytes < sizeof(struct kgsl_devinfo)) { errno = EINVAL; return -1; }
    struct kgsl_devinfo info = { .device_id = 1, .chip_id = 0x43051400, .mmu_enabled = 1, .gpu_id = 6770, .gmem_sizebytes = 3072u << 10 };
    memcpy(p->value, &info, sizeof info);
    return 0;
  }
  case IOCTL_KGSL_GPUOBJ_ALLOC: {
    struct kgsl_gpuobj_alloc *a = arg;
    if ((a->flags & KGSL_MEMFLAGS_VBO) && a->size > VA_END - VA_BASE) { errno = EINVAL; return -1; }
    return object_new(a->size, a->flags & KGSL_MEMFLAGS_VBO, &a->id);
  }
  case IOCTL_KGSL_GPUOBJ_INFO: {
    struct kgsl_gpuobj_info *i = arg;
    object *o = object_find(i->id);
    if (!o) { errno = ENOENT; return -1; }
    i->gpuaddr = o->base; i->size = o->size; i->flags = o->flags;
    return 0;
  }
  case IOCTL_KGSL_GPUOBJ_IMPORT: {
    struct kgsl_gpuobj_import *i = arg;
    struct kgsl_gpuobj_import_dma_buf *d = (void *)(uintptr_t)i->priv;
    off_t size = i->type == KGSL_USER_MEM_TYPE_DMABUF ? lseek(d->fd, 0, SEEK_END) : -1;
    if (size <= 0 || (i->flags & KGSL_MEMFLAGS_VBO)) { errno = EINVAL; return -1; }
    if (object_new((uint64_t)size, false, &i->id)) return -1;
    object_find(i->id)->flags = no_io_coherence ? i->flags & ~(uint64_t)KGSL_MEMFLAGS_IOCOHERENT : i->flags;
    imports++;
    return 0;
  }
  case IOCTL_KGSL_GPUOBJ_FREE: {
    struct kgsl_gpuobj_free *f = arg;
    object *o = object_find(f->id);
    if (!o) { errno = ENOENT; return -1; }
    *o = objects[--nobjects];
    frees++;
    return 0;
  }
  case IOCTL_KGSL_GPUMEM_BIND_RANGES: {
    struct kgsl_gpumem_bind_ranges *r = arg;
    struct kgsl_gpumem_bind_range *ranges = (void *)(uintptr_t)r->ranges;
    object *vbo = object_find(r->id);
    if (!vbo || !vbo->vbo || r->ranges_size != sizeof *ranges) { errno = EINVAL; return -1; }
    for (unsigned i = 0; i < r->ranges_nents; i++) {
      object *child = ranges[i].child_id ? object_find(ranges[i].child_id) : NULL;
      bool bind = ranges[i].op == KGSL_GPUMEM_RANGE_OP_BIND;
      if (((ranges[i].target_offset | ranges[i].length | ranges[i].child_offset) & 0xfff) || !ranges[i].length ||
          ranges[i].target_offset + ranges[i].length > vbo->size || (bind && (!child || child->vbo || ranges[i].child_offset + ranges[i].length > child->size))) { errno = EINVAL; return -1; }
    }
    for (unsigned i = 0; i < r->ranges_nents && nbinds < 256; i++)
      binds[nbinds++] = (bound){ .vbo = r->id, .child = ranges[i].child_id, .op = ranges[i].op, .target = ranges[i].target_offset, .length = ranges[i].length,
                                 .child_offset = ranges[i].child_offset, .call = bind_calls };
    bind_calls++;
    return 0;
  }
  case IOCTL_KGSL_SETPROPERTY: return 0;
  default: errno = ENOTTY; return -1;
  }
}

int __real_ioctl(int fd, unsigned long request, ...);
int __wrap_ioctl(int fd, unsigned long request, ...);
int __wrap_ioctl(int fd, unsigned long request, ...)
{
  va_list ap;
  va_start(ap, request);
  void *arg = va_arg(ap, void *);
  va_end(ap);
  return _IOC_TYPE(request) == KGSL_IOC_TYPE ? fake_kgsl(request, arg) : __real_ioctl(fd, request, arg);
}

int __real_open(const char *path, int flags, ...);
int __wrap_open(const char *path, int flags, ...);
int __wrap_open(const char *path, int flags, ...)
{
  va_list ap;
  va_start(ap, flags);
  int mode = va_arg(ap, int);
  va_end(ap);
  if (!strcmp(path, "/dev/kgsl-3d0") || !strcmp(path, "/dev/dma_heap/system")) return __real_open("/dev/null", O_RDWR | O_CLOEXEC);
  return __real_open(path, flags, mode);
}

// ---- a guest context, as tools/gpuctl.c drives one --------------------------------------------------------------------
#define RAM_PHYS  0x800000000ull
#define RAM_BYTES (128ull << 20)
#define POOL_BYTES (8ull << 20)
typedef struct { uint32_t cmd, len, seqno, rsp_off; } ccmd_req;
typedef struct { uint32_t seqno, rsp_mem_offset, async_error, global_faults; } msm_shmem;
static unsigned checks, failed;
#define CHECK(x) do { checks++; if (!(x)) { failed++; printf("FAILED %s:%d: %s\n", __FILE__, __LINE__, #x); } } while (0)
static char last_error[512];
static void log_line(int level, const char *line) { if (level == 0) snprintf(last_error, sizeof last_error, "%s", line); if (getenv("TEST_VERBOSE")) printf("  %d %s\n", level, line); }

static uint8_t *ram;
static xh_gpu3d *gpu;
static uint64_t va_start, va_size;

static bool renderer(void)
{
  int fd = (int)syscall(SYS_memfd_create, "test-ram", 2u);
  if (fd < 0 || ftruncate(fd, RAM_BYTES) || fcntl(fd, 1033, 2)) return false;
  ram = mmap(NULL, RAM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
  const char *why = "";
  uint64_t pool = RAM_BYTES - POOL_BYTES;
  gpu = xh_gpu3d_kgsl_open(fd, RAM_PHYS, RAM_BYTES, ram + pool, RAM_PHYS + pool, POOL_BYTES, log_line, &why);
  if (!gpu) { printf("  the renderer: %s\n", why); return false; }
  struct { uint32_t wire_format, major, minor, patch, context_type, pad, has_cached_coherent, priorities; uint64_t start, size; } caps;
  static uint8_t bytes[4096];
  if (gpu->capset(gpu, XH_GPU_CAPSET_DRM, 0, bytes, sizeof bytes) < sizeof caps) return false;
  memcpy(&caps, bytes, sizeof caps);
  va_start = caps.start; va_size = caps.size;
  return true;
}

typedef struct { uint32_t id; msm_shmem *shmem; uint32_t seqno; uint64_t slice; } context;

static int ccmd(context *c, void *request, uint32_t size)
{
  ccmd_req *hdr = request;
  hdr->len = size; hdr->seqno = ++c->seqno; hdr->rsp_off = 0;
  int r = gpu->submit(gpu, c->id, request, size);
  return r ? r : c->shmem->seqno != c->seqno ? -EIO : 0;
}

static bool context_open(context *c, uint32_t id, uint32_t shmem_res)
{
  uint64_t offset = 0;
  uint32_t info = 0;
  *c = (context){ .id = id };
  if (gpu->ctx_create(gpu, id, XH_GPU_CAPSET_DRM, "test", 4)) return false;
  if (gpu->create_blob(gpu, id, shmem_res, XH_GPU_BLOB_HOST3D, XH_GPU_BLOB_FLAG_MAPPABLE, 0, 16384, NULL, 0) || gpu->map_blob(gpu, shmem_res, &offset, &info)) return false;
  c->shmem = (msm_shmem *)(ram + RAM_BYTES - POOL_BYTES + offset);
  // GET_PARAM(VA_START): this context's slice of the VBO
  struct { ccmd_req hdr; uint32_t cmd; struct { uint32_t pipe, param; uint64_t value; uint32_t len, pad; } __attribute__((packed)) p; } __attribute__((packed, aligned(4))) req = { .hdr.cmd = 2, .cmd = 0xc0186440u, .p = { .pipe = 0x10, .param = 0x0e } };
  uint32_t want = 8 + sizeof req.p;
  memcpy((uint8_t *)c->shmem + c->shmem->rsp_mem_offset, &want, 4);
  if (ccmd(c, &req, sizeof req)) return false;
  memcpy(&c->slice, (uint8_t *)c->shmem + c->shmem->rsp_mem_offset + 8 + 8, 8);
  return c->slice >= va_start && c->slice < va_start + va_size;
}

static int guest_buffer(context *c, uint32_t res, uint32_t blob, uint64_t iova, uint64_t bytes, const xh_gpu_entry *pages, unsigned n)
{
  struct { ccmd_req hdr; uint64_t iova, size; uint32_t flags, blob_id; } __attribute__((packed, aligned(4))) gem =
    { .hdr.cmd = 3, .iova = iova, .size = bytes, .flags = 0x00020000u | 0x80000000u, .blob_id = blob };
  int r = ccmd(c, &gem, sizeof gem);
  if (r || c->shmem->async_error) return r ? r : -EPROTO;
  return gpu->create_blob(gpu, c->id, res, XH_GPU_BLOB_HOST3D_GUEST, XH_GPU_BLOB_FLAG_GUEST_HANDLE, blob, bytes, pages, n);
}

// ---- the scenarios ----------------------------------------------------------------------------------------------------
// What the phone did with runtime 0.1.3: the VBO takes the whole range, and the first import finds no address.
static int whole_range(void)
{
  setenv("DRM2KGSL_VBO_HEADROOM_GB", "0", 1);          // as drm2kgsl was
  CHECK(renderer() && va_start == VA_BASE && va_size == (256ull << 30));
  context c;
  CHECK(context_open(&c, 1, 1));
  xh_gpu_entry pages = { .addr = RAM_PHYS + (1u << 20), .length = 0x4000 };
  int r = guest_buffer(&c, 2, 1, c.slice + (16u << 20), 0x4000, &pages, 1);
  CHECK(r == -ENOMEM && strstr(last_error, "GPUOBJ_IMPORT failed: Cannot allocate memory"));
  return 0;
}

static unsigned binds_of(uint32_t op) { unsigned n = 0; for (unsigned i = 0; i < nbinds; i++) n += binds[i].op == op; return n; }

static int with_room(void)
{
  CHECK(renderer() && va_start == VA_BASE && va_size == (192ull << 30));      // the next rung: 64 GiB of addresses left
  CHECK(nobjects == 1 && objects[0].vbo);                                      // the room check's reservation is gone
  context c, d;
  CHECK(context_open(&c, 1, 1));
  uint32_t vbo = objects[0].id;
  // a small buffer: one udmabuf, one object, one range
  xh_gpu_entry pages = { .addr = RAM_PHYS + (1u << 20), .length = 0x4000 };
  uint64_t iova = c.slice + (16u << 20);
  unsigned before = imports, base = nbinds;
  CHECK(guest_buffer(&c, 2, 1, iova, 0x4000, &pages, 1) == 0);
  CHECK(imports == before + 1 && nbinds == base + 1 && binds[base].vbo == vbo && binds[base].op == KGSL_GPUMEM_RANGE_OP_BIND &&
        binds[base].target == iova - va_start && binds[base].length == 0x4000 && binds[base].child_offset == 0);
  // 72 MiB in one run: two udmabufs (64 + 8 MiB), two objects, bound in a row by one call
  xh_gpu_entry big = { .addr = RAM_PHYS + (8u << 20), .length = 72u << 20 };
  uint64_t big_iova = c.slice + (256u << 20);
  before = imports; base = nbinds;
  unsigned calls = bind_calls;
  CHECK(guest_buffer(&c, 3, 2, big_iova, 72ull << 20, &big, 1) == 0);
  CHECK(imports == before + 2 && nbinds == base + 2 && bind_calls == calls + 1);
  CHECK(binds[base].target == big_iova - va_start && binds[base].length == (64u << 20) && binds[base + 1].target == big_iova - va_start + (64u << 20) &&
        binds[base + 1].length == (8u << 20) && binds[base].child != binds[base + 1].child && binds[base].child_offset == 0 && binds[base + 1].child_offset == 0);
  object *first = object_find(binds[base].child), *second = object_find(binds[base + 1].child);
  CHECK(first && second && first->size == (64u << 20) && second->size == (8u << 20));
  const uint32_t first_id = binds[base].child, second_id = binds[base + 1].child;
  // 1100 separate pages: two udmabufs (1024 + 76 pages)
  static xh_gpu_entry many[1100];
  for (unsigned i = 0; i < 1100; i++) many[i] = (xh_gpu_entry){ .addr = RAM_PHYS + (88u << 20) + (uint64_t)i * 0x2000, .length = 0x1000 };
  uint64_t many_iova = c.slice + (512u << 20);
  before = imports; base = nbinds;
  CHECK(guest_buffer(&c, 4, 3, many_iova, 1100 * 0x1000, many, 1100) == 0);
  CHECK(imports == before + 2 && nbinds == base + 2 && binds[base].length == 1024 * 0x1000 && binds[base + 1].target == many_iova - va_start + 1024 * 0x1000 &&
        binds[base + 1].length == 76 * 0x1000);
  // segments that hold more than the blob: only the blob's bytes are bound (the last chunk's range is cut)
  xh_gpu_entry more = { .addr = RAM_PHYS + (100u << 20), .length = 0x6000 };
  xh_gpu3d_udmabuf_set_limits(0x4000, 4);
  base = nbinds;
  CHECK(guest_buffer(&c, 5, 4, c.slice + (768u << 20), 0x5000, &more, 1) == 0);
  CHECK(nbinds == base + 2 && binds[base].length == 0x4000 && binds[base + 1].length == 0x1000);
  // segments that hold less than the blob: refused, and nothing stays imported
  unsigned live = nobjects;
  CHECK(guest_buffer(&c, 6, 5, c.slice + (800u << 20), 0x9000, &more, 1) != 0 && nobjects == live);
  xh_gpu3d_udmabuf_set_limits(0, 0);

  // a second context attaches the 72 MiB resource: it imports both chunks again and binds them at its own address
  CHECK(context_open(&d, 2, 11) && d.slice != c.slice);
  before = imports; base = nbinds;
  CHECK(gpu->attach(gpu, 2, 3, true) == 0 && imports == before + 2 && nbinds == base);
  CHECK(gpu->attach(gpu, 2, 3, true) == 0 && imports == before + 2);            // once
  struct { ccmd_req hdr; uint64_t iova; uint32_t res_id, pad; } __attribute__((packed, aligned(4))) set = { .hdr.cmd = 4, .iova = d.slice + (64u << 20), .res_id = 3 };
  CHECK(ccmd(&d, &set, sizeof set) == 0 && d.shmem->async_error == 0);
  CHECK(nbinds == base + 2 && binds[base].target == d.slice + (64u << 20) - va_start && binds[base].length == (64u << 20) &&
        binds[base + 1].target == binds[base].target + (64u << 20) && binds[base + 1].length == (8u << 20));
  // the small one too, by the resource's one fd as before
  before = imports;
  CHECK(gpu->attach(gpu, 2, 2, true) == 0 && imports == before + 1);

  // gone: every chunk of the creating context is unbound from its slice and freed, the second context's copies are freed
  live = nobjects; base = nbinds;
  unsigned freed = frees;
  gpu->unref(gpu, 3);
  CHECK(frees == freed + 4 && nobjects == live - 4 && binds_of(KGSL_GPUMEM_RANGE_OP_UNBIND) >= 2);
  unsigned unbound = 0;
  for (unsigned i = base; i < nbinds; i++) unbound += binds[i].op == KGSL_GPUMEM_RANGE_OP_UNBIND && (binds[i].child == first_id || binds[i].child == second_id);
  CHECK(unbound == 2);
  gpu->unref(gpu, 4); gpu->unref(gpu, 5); gpu->unref(gpu, 2);
  gpu->unref(gpu, 1); gpu->unref(gpu, 11);
  gpu->ctx_destroy(gpu, 1);
  gpu->ctx_destroy(gpu, 2);
  for (unsigned i = 0; i < nobjects; i++) if (!objects[i].vbo && getenv("TEST_VERBOSE")) printf("  left: id %u size 0x%llx\n", objects[i].id, (unsigned long long)objects[i].size);
  CHECK(nobjects == 1 && objects[0].vbo);                                      // only the VBO is left
  return 0;
}

// drm2kgsl patch 0007: each import's flags read back. A guest buffer asks IO coherence and keeps it; on a KGSL that drops
// the flag (an MMU without IO coherence) the loss is counted and said; an attach by a second context asks none.
typedef struct { uint64_t imports, checked, kept, dropped, plain, unreadable; } import_stats;
static import_stats imports_now(void)
{
  import_stats s;
  drm2kgsl_renderer_import_stats(&s.imports, &s.checked, &s.kept, &s.dropped, &s.plain, &s.unreadable);
  return s;
}

static int coherence(void)
{
  CHECK(renderer());
  context c, d;
  CHECK(context_open(&c, 1, 1));
  import_stats was = imports_now();
  xh_gpu_entry pages = { .addr = RAM_PHYS + (1u << 20), .length = 0x4000 };
  CHECK(guest_buffer(&c, 2, 1, c.slice + (16u << 20), 0x4000, &pages, 1) == 0);
  import_stats now = imports_now();
  CHECK(now.imports == was.imports + 1 && now.checked == was.checked + 1 && now.kept == was.kept + 1 && now.dropped == was.dropped &&
        now.unreadable == was.unreadable);
  no_io_coherence = true;
  was = now;
  xh_gpu_entry more = { .addr = RAM_PHYS + (2u << 20), .length = 0x4000 };
  CHECK(guest_buffer(&c, 3, 2, c.slice + (32u << 20), 0x4000, &more, 1) == 0);
  now = imports_now();
  CHECK(now.imports == was.imports + 1 && now.dropped == was.dropped + 1 && now.kept == was.kept);
  no_io_coherence = false;
  CHECK(context_open(&d, 2, 11));
  was = imports_now();
  CHECK(gpu->attach(gpu, 2, 2, true) == 0);
  now = imports_now();
  CHECK(now.imports == was.imports + 1 && now.plain == was.plain + 1);
  gpu->unref(gpu, 2); gpu->unref(gpu, 3); gpu->unref(gpu, 1); gpu->unref(gpu, 11);
  gpu->ctx_destroy(gpu, 1);
  gpu->ctx_destroy(gpu, 2);
  return 0;
}

static bool scenario(const char *name, int (*run)(void))
{
  fflush(stdout);
  pid_t pid = fork();
  if (pid == 0) { run(); printf("drm2kgsl on a stand-in KGSL: %s: %u checks, %u failed\n", name, checks, failed); fflush(stdout); _exit(failed ? 1 : 0); }
  int status = 0;
  return pid > 0 && waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

int main(void)
{
  int node = __real_open("/dev/udmabuf", O_RDWR | O_CLOEXEC);
  if (node < 0) { printf("drm2kgsl on a stand-in KGSL: no /dev/udmabuf here (%s): skipped\n", strerror(errno)); return 0; }
  close(node);
  bool ok = scenario("a VBO over the whole address range (the phone, runtime 0.1.3)", whole_range);
  ok = scenario("a VBO that leaves room; buffers past udmabuf's limits", with_room) && ok;
  ok = scenario("each import's IO coherence read back (patch 0007)", coherence) && ok;
  return ok ? 0 : 1;
}
