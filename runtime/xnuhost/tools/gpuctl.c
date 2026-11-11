// SPDX-License-Identifier: MIT
// gpuctl: tries the GPU path on the phone, without xnuhost or a guest. Run as root (su):
//   gpuctl nodes      the three device nodes, their SELinux labels' effect (can this process open them) and udmabuf's limits
//   gpuctl routes     KGSL alone, by raw ioctls (no drm2kgsl), each way memory could reach it, PASS or FAIL with the errno:
//                     a udmabuf of memfd pages imported, a /dev/dma_heap/system buffer imported, a /dev/dma_heap/qcom,system
//                     buffer imported, KGSL's own GPUOBJ_ALLOC mapped; each also bound into a small VBO and written by the
//                     CPU. Then the address room: a udmabuf imported beside a VBO of each size drm2kgsl would try
//   gpuctl probe      opens /dev/kgsl-3d0 through drm2kgsl: the chip, the GPU address range, the DRM capset a guest gets
//   gpuctl submit     probe, then what one guest context does: the context, its response buffer from the pool, GET_PARAM
//                     answered there, a buffer of "guest" pages imported through /dev/udmabuf and bound at a GPU address,
//                     a submit queue, one command buffer of CP_NOPs run by the GPU, and its fence retired
//   gpuctl large      probe, then buffers past udmabuf's limits, split the way xnuhost splits them: 96 MiB in one run
//                     (two udmabufs, by size) and 1100 separate pages (two, by count); commands at the end of each are
//                     run by the GPU, so the last udmabuf is bound where it belongs
//   gpuctl            all of them, in this order; `routes` only reports there (its failures are not the run's)
// It stands where virtio_gpu.c stands in xnuhost: the same renderer (src/gpu3d_kgsl.c), a memfd of its own as the guest's
// RAM. Each step prints what it found; the last line is "ok" or the step that failed and why. Nothing is left behind:
// the KGSL contexts and the memory go with the process.
#define _GNU_SOURCE
#include "gpu3d.h"
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <linux/dma-heap.h>
#include <linux/udmabuf.h>

#define RAM_PHYS  0x800000000ull       // any address: the "guest physical" base of the memfd
#define RAM_BYTES (256ull << 20)       // sparse: only the pages a step imports are ever allocated (about 110 MiB in `large`)
#define POOL_BYTES (8ull << 20)        // as xnuhost carves it (main.c GPU_POOL_BYTES)
#define SHMEM_BYTES 16384u

// The msm protocol a guest's Turnip speaks (virglrenderer src/drm/msm/msm_proto.h, drm-uapi/msm_drm.h), the few
// messages used here.
enum { CCMD_IOCTL_SIMPLE = 2, CCMD_GEM_NEW = 3, CCMD_GEM_SUBMIT = 7 };
enum { IOCTL_GET_PARAM = 0xc0186440u, IOCTL_SUBMITQUEUE_NEW = 0xc00c644au };     // DRM_IOWR(0x40 + nr, struct)
enum { PIPE_3D0 = 0x10, PARAM_GPU_ID = 1, PARAM_CHIP_ID = 3, PARAM_VA_START = 0x0e, PARAM_VA_SIZE = 0x0f };
enum { BO_WC = 0x00020000u, BO_GUEST_ALLOC = 0x80000000u, SUBMIT_CMD_BUF = 1, SUBMIT_BO_READ = 1, SUBMIT_FENCE_SN_IN = 0x02000000u };
typedef struct { uint32_t cmd, len, seqno, rsp_off; } ccmd_req;
typedef struct { uint32_t pipe, param; uint64_t value; uint32_t len, pad; } msm_param;
typedef struct { uint32_t flags, prio, id; } msm_queue;
typedef struct { uint32_t seqno, rsp_mem_offset, async_error, global_faults; } msm_shmem;

static int verbose;
static void log_line(int level, const char *line) { if (level <= 1 || verbose) printf("  %s%s\n", level == 0 ? "ERROR " : "", line); }

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t retired_cond = PTHREAD_COND_INITIALIZER;
static uint64_t retired_fence;
static void retired(xh_virtio *dev, uint32_t ctx, uint32_t ring, uint64_t fence)
{
  (void)dev; (void)ctx; (void)ring;
  pthread_mutex_lock(&lock);
  if (fence > retired_fence) retired_fence = fence;
  pthread_cond_broadcast(&retired_cond);
  pthread_mutex_unlock(&lock);
}

static int nodes(void)
{
  static const char *const paths[] = { "/dev/kgsl-3d0", "/dev/udmabuf", "/dev/dma_heap/system" };
  static const char *const limits[] = { "/sys/module/udmabuf/parameters/list_limit", "/sys/module/udmabuf/parameters/size_limit_mb" };
  int bad = 0;
  for (unsigned i = 0; i < 3; i++) {
    int fd = open(paths[i], (i == 2 ? O_RDONLY : O_RDWR) | O_CLOEXEC);
    printf("%-22s %s\n", paths[i], fd >= 0 ? "opens" : strerror(errno));
    if (fd >= 0) close(fd); else bad = 1;
  }
  for (unsigned i = 0; i < 2; i++) {
    char text[32] = "";
    FILE *f = fopen(limits[i], "r");
    if (f) { if (!fgets(text, sizeof text, f)) text[0] = 0; fclose(f); }
    text[strcspn(text, "\n")] = 0;
    // One udmabuf: at most list_limit runs of adjacent pages and size_limit_mb MiB. A blob past either is several.
    printf("%-44s %s\n", limits[i], *text ? text : "(not readable)");
  }
  uint64_t bytes;
  unsigned runs;
  xh_gpu3d_udmabuf_limits(&bytes, &runs);
  printf("xnuhost splits a guest buffer into udmabufs of at most %llu MiB and %u runs of pages\n", (unsigned long long)(bytes >> 20), runs);
  return bad;
}

// ---- routes: KGSL by raw ioctls ---------------------------------------------------------------------------------------
// The structures and numbers of msm_kgsl.h used here (external/virglrenderer src/drm/drm2kgsl/msm_kgsl.h).
struct k_alloc { uint64_t size, flags, va_len, mmapsize; uint32_t id, metadata_len; uint64_t metadata; };
struct k_free { uint64_t flags, priv; uint32_t id, type, len; };
struct k_info { uint64_t gpuaddr, flags, size, va_len, va_addr; uint32_t id; };
struct k_import { uint64_t priv, priv_len, flags; uint32_t type, id; };
struct k_range { uint64_t child_offset, target_offset, length; uint32_t child_id, op; };
struct k_ranges { uint64_t ranges; uint32_t nents, size, id, flags; int32_t fence_id; uint32_t padding; };
#define K_ALLOC  _IOWR(0x09, 0x45, struct k_alloc)
#define K_FREE   _IOW(0x09, 0x46, struct k_free)
#define K_INFO   _IOWR(0x09, 0x47, struct k_info)
#define K_IMPORT _IOWR(0x09, 0x48, struct k_import)
#define K_BIND   _IOWR(0x09, 0x56, struct k_ranges)
#define K_VBO (1ull << 34)
#define K_VBO_NO_MAP_ZERO (1ull << 35)       // the reservation alone, without the zero page mapped over it: quick
#define K_IOCOHERENT (1ull << 31)
#define ROUTE_BYTES (1u << 20)

static int k_vbo(int kgsl, uint64_t size, uint64_t flags, uint32_t *id, uint64_t *gpuaddr)
{
  struct k_alloc a = { .size = size, .flags = K_VBO | flags };
  if (ioctl(kgsl, K_ALLOC, &a)) return -1;
  struct k_info info = { .id = a.id };
  if (ioctl(kgsl, K_INFO, &info)) return -1;
  *id = a.id; *gpuaddr = info.gpuaddr;
  return 0;
}

static void k_release(int kgsl, uint32_t id) { struct k_free f = { .id = id }; ioctl(kgsl, K_FREE, &f); }

static int k_import(int kgsl, int dmabuf, uint32_t *id)
{
  int fd = dmabuf;
  struct k_import req = { .priv = (uintptr_t)&fd, .priv_len = sizeof fd, .flags = K_IOCOHERENT, .type = 3 /* KGSL_USER_MEM_TYPE_DMABUF */ };
  if (ioctl(kgsl, K_IMPORT, &req)) return -1;
  *id = req.id;
  return 0;
}

static int a_udmabuf(uint64_t bytes)
{
  int memfd = (int)syscall(SYS_memfd_create, "gpuctl-route", 2u), fd = -1, error;
  if (memfd < 0) return -1;
  if (ftruncate(memfd, (off_t)bytes) == 0 && fcntl(memfd, 1033, 2) == 0) {
    int dev = open("/dev/udmabuf", O_RDWR | O_CLOEXEC);
    if (dev >= 0) {
      struct udmabuf_create c = { .memfd = (uint32_t)memfd, .flags = UDMABUF_FLAGS_CLOEXEC, .size = bytes };
      fd = ioctl(dev, UDMABUF_CREATE, &c);
      error = errno; close(dev); errno = error;
    }
  }
  error = errno; close(memfd); errno = error;
  return fd;
}

static int a_heap(const char *path, uint64_t bytes)
{
  int heap = open(path, O_RDONLY | O_CLOEXEC);
  if (heap < 0) return -1;
  struct dma_heap_allocation_data a = { .len = bytes, .fd_flags = O_RDWR | O_CLOEXEC };
  int r = ioctl(heap, DMA_HEAP_IOCTL_ALLOC, &a), error = errno;
  close(heap);
  errno = error;
  return r ? -1 : (int)a.fd;
}

// One route: the memory, KGSL's object of it, its place in a VBO, the CPU's view of it. Returns whether the GPU side passed.
static bool route(int kgsl, uint32_t vbo, unsigned slot, const char *name, int kind, const char *heap)
{
  char line[320];
  int n = snprintf(line, sizeof line, "  %-28s", name), dmabuf = -1;
  uint32_t id = 0;
  bool ok = false;
  #define ADD(...) n += snprintf(line + n, sizeof line - (size_t)n, __VA_ARGS__)
  if (kind == 2) {
    struct k_alloc a = { .size = ROUTE_BYTES };
    if (ioctl(kgsl, K_ALLOC, &a)) { ADD("FAIL GPUOBJ_ALLOC: %s (%d)", strerror(errno), errno); goto out; }
    id = a.id;
    ADD("PASS alloc");
  } else {
    dmabuf = kind == 0 ? a_udmabuf(ROUTE_BYTES) : a_heap(heap, ROUTE_BYTES);
    if (dmabuf < 0) { ADD("FAIL %s: %s (%d)", kind == 0 ? "UDMABUF_CREATE" : "the heap's allocation", strerror(errno), errno); goto out; }
    if (k_import(kgsl, dmabuf, &id)) { ADD("FAIL GPUOBJ_IMPORT: %s (%d)", strerror(errno), errno); goto out; }
    ADD("PASS import");
  }
  struct k_info info = { .id = id };
  if (ioctl(kgsl, K_INFO, &info) == 0) ADD(" (id %u, GPU address 0x%llx, %llu KiB)", id, (unsigned long long)info.gpuaddr, (unsigned long long)(info.size >> 10));
  struct k_range range = { .target_offset = (uint64_t)slot * 2 * ROUTE_BYTES, .length = ROUTE_BYTES, .child_id = id, .op = 1 };
  struct k_ranges bind = { .ranges = (uintptr_t)&range, .nents = 1, .size = sizeof range, .id = vbo };
  if (!vbo) ADD(", no VBO to bind in");
  else if (ioctl(kgsl, K_BIND, &bind)) ADD(", FAIL bind in a VBO: %s (%d)", strerror(errno), errno);
  else { ADD(", PASS bind in a VBO"); ok = true; range.op = 2; ioctl(kgsl, K_BIND, &bind); }
  uint32_t *at = mmap(NULL, ROUTE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, kind == 2 ? kgsl : dmabuf, kind == 2 ? (off_t)id << 12 : 0);
  if (at == MAP_FAILED) ADD(", FAIL mmap: %s (%d)", strerror(errno), errno);
  else {
    at[0] = 0x6f6e6569; at[ROUTE_BYTES / 4 - 1] = 0x69656e6f;
    ADD(at[0] == 0x6f6e6569 && at[ROUTE_BYTES / 4 - 1] == 0x69656e6f ? ", PASS CPU mapping" : ", FAIL CPU mapping reads back wrong");
    munmap(at, ROUTE_BYTES);
  }
out:
  #undef ADD
  printf("%s\n", line);
  if (id) k_release(kgsl, id);
  if (dmabuf >= 0) close(dmabuf);
  return ok;
}

// In a child: KGSL keeps a process's objects and GPU addresses by process, and the steps after this want them untouched.
static int in_child(int (*run)(uint64_t), uint64_t arg)
{
  fflush(stdout);
  pid_t pid = fork();
  if (pid < 0) return -1;
  if (pid == 0) { int r = run(arg); fflush(stdout); _exit(r); }
  int status = 0;
  if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status)) { printf("  (the step's process ended abnormally, status 0x%x)\n", status); return -1; }
  return WEXITSTATUS(status);
}

static int routes_child(uint64_t unused)
{
  (void)unused;
  int kgsl = open("/dev/kgsl-3d0", O_RDWR | O_CLOEXEC);
  if (kgsl < 0) { printf("  /dev/kgsl-3d0: %s\n", strerror(errno)); return 3; }
  uint32_t vbo = 0;
  uint64_t base = 0;
  if (k_vbo(kgsl, 4ull << 30, 0, &vbo, &base)) printf("  a 4 GiB VBO (KGSL_MEMFLAGS_VBO): %s (%d): nothing can be bound at a guest's address\n", strerror(errno), errno);
  else printf("  a 4 GiB VBO at GPU address 0x%llx to bind in\n", (unsigned long long)base);
  bool udmabuf = route(kgsl, vbo, 0, "udmabuf of memfd pages", 0, NULL);
  route(kgsl, vbo, 1, "/dev/dma_heap/system", 1, "/dev/dma_heap/system");
  route(kgsl, vbo, 2, "/dev/dma_heap/qcom,system", 1, "/dev/dma_heap/qcom,system");
  route(kgsl, vbo, 3, "KGSL GPUOBJ_ALLOC + mmap", 2, NULL);
  return udmabuf ? 0 : 1;
}

// A VBO of `size` where drm2kgsl reserves its own, then one import beside it. 0: imported; 1: the import failed; 2: no such VBO.
static int room_child(uint64_t size)
{
  int kgsl = open("/dev/kgsl-3d0", O_RDWR | O_CLOEXEC), dmabuf = a_udmabuf(65536);
  uint32_t vbo = 0, id = 0;
  uint64_t base = 0;
  if (kgsl < 0 || dmabuf < 0) { printf("  VBO of %4llu GiB: not tried: %s\n", (unsigned long long)(size >> 30), strerror(errno)); return 2; }
  if (k_vbo(kgsl, size, K_VBO_NO_MAP_ZERO, &vbo, &base)) { printf("  VBO of %4llu GiB: not granted: %s (%d)\n", (unsigned long long)(size >> 30), strerror(errno), errno); return 2; }
  if (k_import(kgsl, dmabuf, &id)) {
    printf("  VBO of %4llu GiB at 0x%llx: granted; a udmabuf import beside it FAIL: %s (%d)%s\n", (unsigned long long)(size >> 30), (unsigned long long)base,
           strerror(errno), errno, errno == ENOMEM ? ": the VBO leaves KGSL no GPU address for the object" : "");
    return 1;
  }
  struct k_info info = { .id = id };
  ioctl(kgsl, K_INFO, &info);
  printf("  VBO of %4llu GiB at 0x%llx: granted; a udmabuf import beside it PASS (its own GPU address 0x%llx)\n", (unsigned long long)(size >> 30),
         (unsigned long long)base, (unsigned long long)info.gpuaddr);
  return 0;
}

static int routes(bool alone)
{
  static const uint64_t ladder[] = { 1024ull << 30, 256ull << 30, 192ull << 30, 128ull << 30, 64ull << 30, 16ull << 30, 4ull << 30 };   // drm2kgsl's
  printf("routes: how memory reaches KGSL on this kernel (raw ioctls, %u KiB each)\n", ROUTE_BYTES >> 10);
  int memory = in_child(routes_child, 0);
  if (memory == 3) { printf("%s: no KGSL device\n", alone ? "FAILED at: routes" : "routes: FAIL"); return 1; }
  printf("address room: KGSL gives every object a GPU address of its own, from the range a VBO is reserved in\n");
  uint64_t fits = 0, filled = 0;
  for (unsigned i = 0; i < sizeof ladder / sizeof *ladder && !fits; i++) {
    int r = in_child(room_child, ladder[i]);
    if (r == 0) fits = ladder[i]; else if (r == 1 && !filled) filled = ladder[i];
  }
  if (fits && filled) printf("routes: a %llu GiB VBO fills KGSL's address range; drm2kgsl must reserve %llu GiB or less (this build of drm2kgsl checks for that room)\n",
                             (unsigned long long)(filled >> 30), (unsigned long long)(fits >> 30));
  else if (fits) printf("routes: the largest VBO still leaves room for imports (%llu GiB)\n", (unsigned long long)(fits >> 30));
  else printf("routes: no VBO size leaves room for an import\n");
  // "FAILED at:" is the run's verdict (the fork reads it): only when this step is the run.
  const char *verdict = alone ? "FAILED at: routes" : "routes: FAIL";
  if (memory != 0) { printf("%s: a udmabuf of memfd pages does not reach KGSL (see the lines above for the routes that do)\n", verdict); return 1; }
  if (!fits) { printf("%s: no VBO with room for imports\n", verdict); return 1; }
  return 0;
}

static uint8_t *ram;
static xh_gpu3d *gpu;
static msm_shmem *shmem;
static uint32_t seqno;

// One request of the context; when `rsp` is given, its response (the guest says how long it may be, at rsp_off).
static int ccmd(uint32_t ctx, void *request, uint32_t size, void *rsp, uint32_t rsp_size)
{
  ccmd_req *hdr = request;
  uint8_t *rsp_mem = (uint8_t *)shmem + shmem->rsp_mem_offset;
  hdr->len = size;
  hdr->seqno = ++seqno;
  hdr->rsp_off = 0;
  if (rsp) memcpy(rsp_mem, &rsp_size, 4);
  int r = gpu->submit(gpu, ctx, request, size);
  if (r) return r;
  if (shmem->seqno != seqno) return -EIO;
  if (rsp) memcpy(rsp, rsp_mem, rsp_size);
  return 0;
}

static int ioctl_simple(uint32_t ctx, uint32_t nr, void *payload, uint32_t size)
{
  struct { ccmd_req hdr; uint32_t cmd; uint8_t payload[28]; } req = { .hdr.cmd = CCMD_IOCTL_SIMPLE, .cmd = nr };
  struct { uint32_t len; int32_t ret; uint8_t payload[24]; } rsp = { 0 };
  memcpy(req.payload, payload, size);
  int r = ccmd(ctx, &req, (uint32_t)(sizeof req.hdr + 4 + size + 3) & ~3u, &rsp, 8 + size);
  if (r) return r;
  memcpy(payload, rsp.payload, size);
  return rsp.ret;
}

static int get_param(uint32_t ctx, uint32_t param, uint64_t *value)
{
  msm_param p = { .pipe = PIPE_3D0, .param = param };
  int r = ioctl_simple(ctx, IOCTL_GET_PARAM, &p, sizeof p);
  *value = p.value;
  return r;
}

static uint32_t parity(uint32_t v) { return (0x9669u >> (0xf & (v ^ (v >> 4) ^ (v >> 8) ^ (v >> 12) ^ (v >> 16) ^ (v >> 20) ^ (v >> 24) ^ (v >> 28)))) & 1; }
static uint32_t pkt7(uint32_t opcode, uint32_t count) { return 7u << 28 | count | parity(count) << 15 | (opcode & 0x7f) << 16 | parity(opcode) << 23; }

#define STEP(what, ok, ...) do { if (!(ok)) { printf("FAILED at: " what ": " __VA_ARGS__); printf("\n"); return 1; } } while (0)

static int open_renderer(void)
{
  int fd = (int)syscall(SYS_memfd_create, "gpuctl-ram", 2u /* MFD_ALLOW_SEALING */);
  STEP("the RAM memfd", fd >= 0 && ftruncate(fd, RAM_BYTES) == 0 && fcntl(fd, 1033 /* F_ADD_SEALS */, 2 /* F_SEAL_SHRINK */) == 0, "%s", strerror(errno));
  ram = mmap(NULL, RAM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
  STEP("mapping the RAM", ram != MAP_FAILED, "%s", strerror(errno));
  const char *why = "";
  uint64_t pool = RAM_BYTES - POOL_BYTES;
  gpu = xh_gpu3d_kgsl_open(fd, RAM_PHYS, RAM_BYTES, ram + pool, RAM_PHYS + pool, POOL_BYTES, log_line, &why);
  STEP("opening the renderer", gpu, "%s", why);
  gpu->retire = retired;
  uint32_t id = 0, version = 0, size = 0;
  struct {                                           // virgl_renderer_capset_drm, its msm part
    uint32_t wire_format, major, minor, patch, context_type, pad, has_cached_coherent, priorities;
    uint64_t va_start, va_size;
    uint32_t gpu_id, gmem_size;
    uint64_t gmem_base, chip_id;
  } caps;
  static uint8_t bytes[4096];
  STEP("the capset", gpu->capset_count(gpu) == 1 && gpu->capset_info(gpu, 0, &id, &version, &size) && id == XH_GPU_CAPSET_DRM &&
       size >= sizeof caps && gpu->capset(gpu, id, version, bytes, sizeof bytes) == size, "id %u size %u", id, size);
  memcpy(&caps, bytes, sizeof caps);
  printf("capset: DRM (6), %u bytes: wire format %u, msm %u.%u, context type %u, chip 0x%llx, gpu id %u, GPU addresses 0x%llx + %llu GiB, gmem %u KiB, priorities %u\n",
         size, caps.wire_format, caps.major, caps.minor, caps.context_type, (unsigned long long)caps.chip_id, caps.gpu_id,
         (unsigned long long)caps.va_start, (unsigned long long)(caps.va_size >> 30), caps.gmem_size >> 10, caps.priorities);
  STEP("the capset's content", caps.context_type == 1 && caps.va_size && caps.chip_id, "not an msm context or no GPU address range");
  return 0;
}

// A context as a guest opens one: its response buffer from the pool, GET_PARAM answered there, a submit queue.
static int context_open(uint32_t ctx, uint32_t shmem_res, uint64_t *va_start, uint32_t *queue_id)
{
  uint64_t offset = 0, value = 0, va_size = 0;
  uint32_t info = 0;
  int r;
  seqno = 0;
  pthread_mutex_lock(&lock);
  retired_fence = 0;
  pthread_mutex_unlock(&lock);
  r = gpu->ctx_create(gpu, ctx, XH_GPU_CAPSET_DRM, "gpuctl", 6);
  STEP("the context", r == 0, "%s (a KGSL context needs /dev/kgsl-3d0 and /dev/dma_heap/system)", strerror(-r));
  r = gpu->create_blob(gpu, ctx, shmem_res, XH_GPU_BLOB_HOST3D, XH_GPU_BLOB_FLAG_MAPPABLE, 0, SHMEM_BYTES, NULL, 0);
  STEP("the response buffer (HOST3D blob 0)", r == 0, "%s", strerror(-r));
  r = gpu->map_blob(gpu, shmem_res, &offset, &info);
  STEP("mapping it from the pool", r == 0 && offset + SHMEM_BYTES <= POOL_BYTES, "%s, offset 0x%llx", strerror(-r), (unsigned long long)offset);
  shmem = (msm_shmem *)(ram + RAM_BYTES - POOL_BYTES + offset);
  printf("context %u: response buffer at pool offset 0x%llx (cache mode %u), responses from byte %u\n", ctx, (unsigned long long)offset, info, shmem->rsp_mem_offset);
  STEP("the response buffer's header", shmem->rsp_mem_offset >= 8 && shmem->rsp_mem_offset < SHMEM_BYTES, "offset %u", shmem->rsp_mem_offset);

  r = get_param(ctx, PARAM_GPU_ID, &value);
  STEP("GET_PARAM(GPU_ID)", r == 0 && value, "%d, value %llu", r, (unsigned long long)value);
  printf("GET_PARAM: gpu id %llu", (unsigned long long)value);
  r = get_param(ctx, PARAM_CHIP_ID, &value);
  STEP("GET_PARAM(CHIP_ID)", r == 0, "%d", r);
  printf(", chip 0x%llx", (unsigned long long)value);
  r = get_param(ctx, PARAM_VA_START, va_start);
  if (!r) r = get_param(ctx, PARAM_VA_SIZE, &va_size);
  STEP("GET_PARAM(VA_START, VA_SIZE)", r == 0 && va_size >= (1ull << 30), "%d", r);
  printf(", this context's GPU addresses 0x%llx + %llu GiB\n", (unsigned long long)*va_start, (unsigned long long)(va_size >> 30));
  msm_queue queue = { 0 };
  r = ioctl_simple(ctx, IOCTL_SUBMITQUEUE_NEW, &queue, sizeof queue);
  STEP("SUBMITQUEUE_NEW (a KGSL draw context)", r == 0, "%d", r);
  *queue_id = queue.id;
  return 0;
}

// A buffer object as a guest makes one: its own pages, announced with GEM_NEW at a GPU address of the context's range,
// then given as a HOST3D_GUEST blob: udmabuf (one or several), KGSL import, bind at the address.
static int guest_buffer(uint32_t ctx, uint32_t res, uint32_t blob, uint64_t iova, uint64_t bytes, const xh_gpu_entry *pages, unsigned npages)
{
  struct { ccmd_req hdr; uint64_t iova, size; uint32_t flags, blob_id; } __attribute__((packed, aligned(4))) gem =
    { .hdr.cmd = CCMD_GEM_NEW, .iova = iova, .size = bytes, .flags = BO_WC | BO_GUEST_ALLOC, .blob_id = blob };
  int r = ccmd(ctx, &gem, sizeof gem, NULL, 0);
  STEP("GEM_NEW (guest-allocated)", r == 0 && shmem->async_error == 0, "%d, async errors %u", r, shmem->async_error);
  r = gpu->create_blob(gpu, ctx, res, XH_GPU_BLOB_HOST3D_GUEST, XH_GPU_BLOB_FLAG_GUEST_HANDLE, blob, bytes, pages, npages);
  STEP("the buffer's pages (udmabuf -> KGSL import -> bind)", r == 0, "%llu KiB in %u segments: %s", (unsigned long long)(bytes >> 10), npages, strerror(-r));
  return 0;
}

// 32 CP_NOPs at `offset` of the buffer `res` (bound at `iova`, its bytes at `words`), run by the GPU; its fence retired.
static int run_nops(uint32_t ctx, uint32_t queue, uint32_t res, uint64_t iova, uint32_t offset, uint32_t *words, uint32_t fence)
{
  for (unsigned i = 0; i < 64; i += 2) { words[i] = pkt7(0x10 /* CP_NOP */, 1); words[i + 1] = 0x6f6e6569; }
  struct {
    ccmd_req hdr;
    uint32_t flags, queue_id, nr_bos, nr_cmds, fence;
    struct { uint32_t flags, handle; uint64_t presumed; } bo;
    struct { uint32_t type, submit_idx, submit_offset, size, pad, nr_relocs; uint64_t relocs; } cmd;
  } __attribute__((packed, aligned(4))) job = {
    .hdr.cmd = CCMD_GEM_SUBMIT, .flags = PIPE_3D0 | SUBMIT_FENCE_SN_IN, .queue_id = queue, .nr_bos = 1, .nr_cmds = 1, .fence = fence,
    .bo = { .flags = SUBMIT_BO_READ, .handle = res, .presumed = iova }, .cmd = { .type = SUBMIT_CMD_BUF, .submit_offset = offset, .size = 64 * 4 },
  };
  struct timespec start, end;
  uint64_t value = 0, before = 0;
  bool counted = get_param(ctx, 0x09 /* FAULTS */, &before) == 0;
  clock_gettime(CLOCK_MONOTONIC, &start);
  int r = ccmd(ctx, &job, sizeof job, NULL, 0);
  STEP("GEM_SUBMIT (32 CP_NOPs)", r == 0 && shmem->async_error == 0, "%d, async errors %u: KGSL refused the command", r, shmem->async_error);
  r = gpu->submit_fence(gpu, ctx, 1, fence);         // the queue's ring: priority 0 is ring 1
  STEP("the fence", r == 0, "%s", strerror(-r));
  struct timespec until;
  clock_gettime(CLOCK_REALTIME, &until);
  until.tv_sec += 5;
  pthread_mutex_lock(&lock);
  while (retired_fence < fence && r == 0) r = pthread_cond_timedwait(&retired_cond, &lock, &until);
  pthread_mutex_unlock(&lock);
  clock_gettime(CLOCK_MONOTONIC, &end);
  STEP("the fence retiring", r == 0, "not within 5 s: the GPU did not run the command, or KGSL did not report it");
  r = get_param(ctx, 0x09 /* FAULTS */, &value);
  printf("submit: queue %u, 256 bytes of commands at 0x%llx run by the GPU, fence retired after %.2f ms; GPU faults %llu, async errors %u\n", queue,
         (unsigned long long)(iova + offset), (double)(end.tv_sec - start.tv_sec) * 1e3 + (double)(end.tv_nsec - start.tv_nsec) / 1e6,
         r ? 0ull : (unsigned long long)value, shmem->async_error);
  STEP("no fault", shmem->async_error == 0 && (r || !counted || value == before), "async errors %u, GPU faults %llu (%llu before): the GPU did not find the commands at that address",
       shmem->async_error, (unsigned long long)value, (unsigned long long)before);
  return 0;
}

static int submit(void)
{
  const uint32_t ctx = 1, shmem_res = 1, bo_res = 2, bo_bytes = 0x4000;
  uint64_t va_start = 0;
  uint32_t queue = 0;
  if (context_open(ctx, shmem_res, &va_start, &queue)) return 1;
  // 16 KiB at 1 MiB of the "RAM".
  const uint64_t iova = va_start + (16u << 20);
  xh_gpu_entry pages = { .addr = RAM_PHYS + (1u << 20), .length = bo_bytes };
  if (guest_buffer(ctx, bo_res, 1, iova, bo_bytes, &pages, 1)) return 1;
  printf("buffer: 16 KiB of guest pages imported through /dev/udmabuf, bound at GPU address 0x%llx\n", (unsigned long long)iova);
  if (run_nops(ctx, queue, bo_res, iova, 0, (uint32_t *)(ram + (1u << 20)), 1)) return 1;
  gpu->unref(gpu, bo_res);
  gpu->unref(gpu, shmem_res);
  gpu->ctx_destroy(gpu, ctx);
  return 0;
}

// Buffers one udmabuf cannot hold (64 MiB, 1024 runs of pages on this phone): several, bound in a row.
static int large(void)
{
  enum { PAGES = 1100 };
  const uint32_t ctx = 2, shmem_res = 11, big_res = 12, many_res = 13;
  const uint64_t big_bytes = 96ull << 20, big_at = 16ull << 20, many_at = 120ull << 20, many_bytes = (uint64_t)PAGES * 4096;
  uint64_t va_start = 0, limit = 0;
  uint32_t queue = 0;
  unsigned runs = 0;
  xh_gpu3d_udmabuf_limits(&limit, &runs);
  if (context_open(ctx, shmem_res, &va_start, &queue)) return 1;

  // 96 MiB in one run of pages: more than one udmabuf's bytes.
  const uint64_t big_iova = va_start + (256u << 20);
  xh_gpu_entry one = { .addr = RAM_PHYS + big_at, .length = (uint32_t)big_bytes };
  if (guest_buffer(ctx, big_res, 1, big_iova, big_bytes, &one, 1)) return 1;
  printf("large: %llu MiB in 1 run of pages as %llu udmabufs of up to %llu MiB, bound at GPU address 0x%llx\n", (unsigned long long)(big_bytes >> 20),
         (unsigned long long)((big_bytes + limit - 1) / limit), (unsigned long long)(limit >> 20), (unsigned long long)big_iova);
  // Commands in its first udmabuf, then in its last 64 KiB: the last udmabuf is bound after the others.
  if (run_nops(ctx, queue, big_res, big_iova, 0, (uint32_t *)(ram + big_at), 1)) return 1;
  if (run_nops(ctx, queue, big_res, big_iova, (uint32_t)(big_bytes - 65536), (uint32_t *)(ram + big_at + big_bytes - 65536), 2)) return 1;

  // 1100 pages, every second page of a range: more than one udmabuf's runs.
  static xh_gpu_entry pages[PAGES];
  for (unsigned i = 0; i < PAGES; i++) pages[i] = (xh_gpu_entry){ .addr = RAM_PHYS + many_at + (uint64_t)i * 8192, .length = 4096 };
  const uint64_t many_iova = va_start + (512u << 20);
  if (guest_buffer(ctx, many_res, 2, many_iova, many_bytes, pages, PAGES)) return 1;
  printf("large: %llu KiB in %u separate pages as %u udmabufs of up to %u runs, bound at GPU address 0x%llx\n", (unsigned long long)(many_bytes >> 10),
         (unsigned)PAGES, (PAGES + runs - 1) / runs, runs, (unsigned long long)many_iova);
  // Commands in its last page: page 1099 of the buffer, the 76th of its second udmabuf.
  if (run_nops(ctx, queue, many_res, many_iova, (PAGES - 1) * 4096, (uint32_t *)(ram + many_at + (uint64_t)(PAGES - 1) * 8192), 3)) return 1;
  gpu->unref(gpu, many_res);
  gpu->unref(gpu, big_res);
  gpu->unref(gpu, shmem_res);
  gpu->ctx_destroy(gpu, ctx);
  return 0;
}

int main(int argc, char **argv)
{
  const char *what = argc > 1 ? argv[1] : "all";
  setvbuf(stdout, NULL, _IOLBF, 0);
  verbose = argc > 2 && !strcmp(argv[2], "-v");
  if (!strcmp(what, "-v")) { verbose = 1; what = "all"; }
  bool all = !strcmp(what, "all");
  if (!all && strcmp(what, "nodes") && strcmp(what, "routes") && strcmp(what, "probe") && strcmp(what, "submit") && strcmp(what, "large")) {
    fprintf(stderr, "usage: gpuctl [nodes|routes|probe|submit|large] [-v]\n");
    return 2;
  }
  if (all || !strcmp(what, "nodes")) { int bad = nodes(); if (!all) { printf(bad ? "FAILED at: the device nodes\n" : "ok\n"); return bad; } }
  // Before the renderer: its threads and its KGSL state are not for a fork. Alone, its result is the run's; in a
  // whole run it only reports.
  if (all || !strcmp(what, "routes")) { int bad = routes(!all); if (!all) { if (!bad) printf("ok\n"); return bad; } }
  if (open_renderer()) return 1;
  if ((all || !strcmp(what, "submit")) && submit()) return 1;
  if ((all || !strcmp(what, "large")) && large()) return 1;
  printf("ok\n");
  return 0;
}
