// SPDX-License-Identifier: MIT
// Leaks of the GPU path: the renderer through many blob cycles, failed blobs and a reset, with every fd and KGSL object back
// and LeakSanitizer clean. The stand-in is tests/test_drm2kgsl.c's, copied: the renderer (src/gpu3d_kgsl.c with the patched
// drm2kgsl) on the build host against a stand-in for KGSL: the memory
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
typedef struct { uint32_t id; uint64_t base, size; bool vbo; } object;
static object objects[256];
static unsigned nobjects, next_id = 1, imports, frees;
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
    i->gpuaddr = o->base; i->size = o->size;
    return 0;
  }
  case IOCTL_KGSL_GPUOBJ_IMPORT: {
    struct kgsl_gpuobj_import *i = arg;
    struct kgsl_gpuobj_import_dma_buf *d = (void *)(uintptr_t)i->priv;
    off_t size = i->type == KGSL_USER_MEM_TYPE_DMABUF ? lseek(d->fd, 0, SEEK_END) : -1;
    if (size <= 0 || (i->flags & KGSL_MEMFLAGS_VBO)) { errno = EINVAL; return -1; }
    if (object_new((uint64_t)size, false, &i->id)) return -1;
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

// ---- leak scenarios: fds and KGSL objects back to where they were (mappings are printed: the allocator moves them) ----------------------------------------------------------------------------------
#include <dirent.h>
#include <sanitizer/lsan_interface.h>
static int count_fds(void) { int n = 0; DIR *d = opendir("/proc/self/fd"); struct dirent *e; while ((e = readdir(d))) n += e->d_name[0] != '.'; closedir(d); return n - 1; }
static int count_maps(void) { int n = 0; FILE *f = fopen("/proc/self/maps", "r"); int c; while ((c = fgetc(f)) != EOF) n += c == '\n'; fclose(f); return n; }
static unsigned live_objects(void) { unsigned n = 0; for (unsigned i = 0; i < nobjects; i++) n += !objects[i].vbo; return n; }
typedef struct { int fds, maps; unsigned objs; } snap;
static snap take(void) { return (snap){ count_fds(), count_maps(), live_objects() }; }
static bool same(const char *what, snap a) { snap b = take(); bool ok = a.fds == b.fds && a.objs == b.objs;
  printf("  %-60s fds %+d maps %+d kgsl objects %+d%s\n", what, b.fds - a.fds, b.maps - a.maps, (int)b.objs - (int)a.objs, ok ? "" : "   <-- NOT BACK"); return ok; }

static int set_iova(context *c, uint32_t res, uint64_t iova)
{
  struct { ccmd_req hdr; uint64_t iova; uint32_t res_id, pad; } __attribute__((packed, aligned(4))) set = { .hdr.cmd = 4, .iova = iova, .res_id = res };
  return ccmd(c, &set, sizeof set) || c->shmem->async_error ? -1 : 0;
}
static int upload(context *c, uint32_t res)
{
  struct { ccmd_req hdr; uint32_t res_id, pad, off, len; uint8_t payload[16]; } __attribute__((packed, aligned(4))) up = { .hdr.cmd = 8, .res_id = res, .off = 0x1000, .len = 16 };
  memset(up.payload, 0xab, 16);
  return ccmd(c, &up, sizeof up);
}

static int leaks(void)
{
  int fd0 = count_fds();
  int cycles = getenv("CYCLES") ? atoi(getenv("CYCLES")) : 30;
  CHECK(renderer());
  snap start = take();
  context c, d;
  CHECK(context_open(&c, 1, 1) && context_open(&d, 2, 2));
  snap base = take();
  xh_gpu_entry big = { .addr = RAM_PHYS + (8u << 20), .length = 72u << 20 };
  xh_gpu_entry small = { .addr = RAM_PHYS + (1u << 20), .length = 0x4000 };
  static xh_gpu_entry many[1100];
  for (unsigned i = 0; i < 1100; i++) many[i] = (xh_gpu_entry){ .addr = RAM_PHYS + (88u << 20) + (uint64_t)i * 0x2000, .length = 0x1000 };
  uint32_t blob = 10, res = 100;
  // 1. context blob -> attach to a second context -> SET_IOVA there -> upload in both -> detach -> unref; 30 times each
  for (int kind = 0; kind < 3; kind++) {
    for (int i = 0; i < cycles; i++) {
      blob++; res++;
      const xh_gpu_entry *e = kind == 0 ? &big : kind == 1 ? &small : many;
      unsigned ne = kind == 2 ? 1100 : 1;
      uint64_t bytes = kind == 0 ? 72ull << 20 : kind == 1 ? 0x4000 : 1100 * 0x1000;
      CHECK(guest_buffer(&c, res, blob, c.slice + (256u << 20), bytes, e, ne) == 0);
      CHECK(upload(&c, res) == 0);
      CHECK(gpu->attach(gpu, 2, res, true) == 0);
      CHECK(set_iova(&d, res, d.slice + (64u << 20)) == 0);
      CHECK(upload(&d, res) == 0);
      CHECK(ram[(kind == 0 ? (8u << 20) : kind == 1 ? (1u << 20) : (88u << 20) + 0x1000) + 0x1000] == 0xab);
      CHECK(gpu->attach(gpu, 2, res, false) == 0);
      gpu->unref(gpu, res);
    }
    CHECK(same(kind == 0 ? "30x 72 MiB ctx blob, attach/iova/upload/detach/unref" : kind == 1 ? "30x 16 KiB ctx blob, same" : "30x 1100-page ctx blob, same", base));
  }
  // 2. a display blob (ctx 0, GUEST), chunked and single: attach to both contexts, iova, upload, unref without a detach
  for (int kind = 0; kind < 2; kind++) {
    for (int i = 0; i < cycles; i++) {
      res++;
      CHECK(gpu->create_blob(gpu, 0, res, XH_GPU_BLOB_GUEST, XH_GPU_BLOB_FLAG_GUEST_HANDLE, 0, kind ? 0x4000 : 72ull << 20, kind ? &small : &big, 1) == 0);
      CHECK(gpu->attach(gpu, 1, res, true) == 0 && gpu->attach(gpu, 2, res, true) == 0);
      CHECK(set_iova(&c, res, c.slice + (512u << 20)) == 0 && set_iova(&d, res, d.slice + (512u << 20)) == 0);
      CHECK(upload(&c, res) == 0 && upload(&d, res) == 0);
      gpu->unref(gpu, res);
    }
    CHECK(same(kind ? "30x 16 KiB display blob in 2 contexts, unref" : "30x 72 MiB display blob in 2 contexts, unref", base));
  }
  // 3. a context destroyed with resources still attached (its own and others'), then the resources unref'ed
  for (int i = 0; i < cycles / 3; i++) {
    context e;
    CHECK(context_open(&e, 3, 3));
    uint32_t mine = ++res, theirs = ++res, disp = ++res, disp1 = ++res;
    CHECK(guest_buffer(&e, mine, ++blob, e.slice + (256u << 20), 72ull << 20, &big, 1) == 0 && upload(&e, mine) == 0);
    CHECK(guest_buffer(&c, theirs, ++blob, c.slice + (256u << 20), 72ull << 20, &big, 1) == 0);
    CHECK(gpu->create_blob(gpu, 0, disp, XH_GPU_BLOB_GUEST, XH_GPU_BLOB_FLAG_GUEST_HANDLE, 0, 72ull << 20, &big, 1) == 0);
    CHECK(gpu->create_blob(gpu, 0, disp1, XH_GPU_BLOB_GUEST, XH_GPU_BLOB_FLAG_GUEST_HANDLE, 0, 0x4000, &small, 1) == 0);
    CHECK(gpu->attach(gpu, 3, theirs, true) == 0 && gpu->attach(gpu, 3, disp, true) == 0 && gpu->attach(gpu, 3, disp1, true) == 0 && gpu->attach(gpu, 2, mine, true) == 0);
    CHECK(set_iova(&e, theirs, e.slice + (512u << 20)) == 0 && upload(&e, disp) == 0);
    gpu->ctx_destroy(gpu, 3);
    gpu->unref(gpu, mine); gpu->unref(gpu, theirs); gpu->unref(gpu, disp); gpu->unref(gpu, disp1); gpu->unref(gpu, 3);
  }
  CHECK(same("10x ctx destroyed with own+foreign+display resources attached", base));
  // 4. a failed get_blob: each kind measured on its own
  snap s = take();
  CHECK(guest_buffer(&c, ++res, ++blob, 0x1000, 72ull << 20, &big, 1) != 0);             // iova outside the slice: bind fails
  CHECK(same("chunked blob whose bind fails (iova outside the slice)", s));   // drm2kgsl patch 0003
  s = take();
  CHECK(guest_buffer(&c, ++res, ++blob, 0x1000, 0x4000, &small, 1) != 0);
  CHECK(same("single blob whose bind fails", s));   // drm2kgsl patch 0003
  s = take();
  CHECK(gpu->create_blob(gpu, 1, ++res, XH_GPU_BLOB_HOST3D_GUEST, XH_GPU_BLOB_FLAG_GUEST_HANDLE, 999, 0x4000, &small, 1) != 0);   // no GEM_NEW
  same("single blob with no GEM_NEW for its blob_id (fd parked)", s);
  s = take();
  CHECK(gpu->create_blob(gpu, 1, ++res, XH_GPU_BLOB_HOST3D_GUEST, XH_GPU_BLOB_FLAG_GUEST_HANDLE, 998, 72ull << 20, &big, 1) != 0);
  same("chunked blob with no GEM_NEW (chunks parked; replaces the parked single fd?)", s);
  // 5. "reset": every resource, then every context (virtio_gpu.c teardown's order)
  gpu->unref(gpu, 1); gpu->unref(gpu, 2);
  gpu->ctx_destroy(gpu, 1); gpu->ctx_destroy(gpu, 2);
  CHECK(same("after reset, against before the contexts (parked dma-bufs closed: patch 0003)", start));
  printf("  fds: %d at start, %d after the renderer opened, %d now\n", fd0, start.fds, count_fds());
  const int leaked = __lsan_do_recoverable_leak_check();
  printf("  LeakSanitizer: %s\n", leaked ? "LEAKS (see above)" : "no leaks");
  CHECK(!leaked);
  return 0;
}

int main(void)
{
  setvbuf(stdout, NULL, _IONBF, 0);
  int node = __real_open("/dev/udmabuf", O_RDWR | O_CLOEXEC);
  if (node < 0) { printf("no /dev/udmabuf\n"); return 0; }
  close(node);
  leaks();
  printf("leak scenarios: %u checks, %u failed\n", checks, failed);
  return failed ? 1 : 0;
}
