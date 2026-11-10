// SPDX-License-Identifier: MIT
// gpuctl: tries the GPU path on the phone, without xnuhost or a guest. Run as root (su):
//   gpuctl nodes      the three device nodes, their SELinux labels' effect (can this process open them) and udmabuf's limits
//   gpuctl probe      opens /dev/kgsl-3d0 through drm2kgsl: the chip, the GPU address range, the DRM capset a guest gets
//   gpuctl submit     probe, then what one guest context does: the context, its response buffer from the pool, GET_PARAM
//                     answered there, a buffer of "guest" pages imported through /dev/udmabuf and bound at a GPU address,
//                     a submit queue, one command buffer of CP_NOPs run by the GPU, and its fence retired
//   gpuctl            all three
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
#include <sys/mman.h>
#include <sys/syscall.h>

#define RAM_PHYS  0x800000000ull       // any address: the "guest physical" base of the memfd
#define RAM_BYTES (64ull << 20)
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
    // A guest blob is one udmabuf: at most list_limit runs of adjacent pages and size_limit_mb MiB.
    printf("%-44s %s\n", limits[i], *text ? text : "(not readable)");
  }
  return bad;
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

static int submit(void)
{
  const uint32_t ctx = 1, shmem_res = 1, bo_res = 2, bo_blob = 1, bo_bytes = 0x4000;
  uint64_t offset = 0, value = 0, va_start = 0, va_size = 0;
  uint32_t info = 0;
  int r;
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
  r = get_param(ctx, PARAM_VA_START, &va_start);
  if (!r) r = get_param(ctx, PARAM_VA_SIZE, &va_size);
  STEP("GET_PARAM(VA_START, VA_SIZE)", r == 0 && va_size >= bo_bytes, "%d", r);
  printf(", this context's GPU addresses 0x%llx + %llu GiB\n", (unsigned long long)va_start, (unsigned long long)(va_size >> 30));

  // A buffer object as a guest makes one: its own pages (here 16 KiB at 1 MiB of the "RAM"), announced with GEM_NEW at
  // a GPU address of the context's range, then given as a HOST3D_GUEST blob: udmabuf, KGSL import, bind at the address.
  const uint64_t bo_pa = RAM_PHYS + (1u << 20), iova = va_start + (16u << 20);
  uint32_t *words = (uint32_t *)(ram + (1u << 20));
  for (unsigned i = 0; i < 64; i += 2) { words[i] = pkt7(0x10 /* CP_NOP */, 1); words[i + 1] = 0x6f6e6569; }
  struct { ccmd_req hdr; uint64_t iova, size; uint32_t flags, blob_id; } __attribute__((packed, aligned(4))) gem =
    { .hdr.cmd = CCMD_GEM_NEW, .iova = iova, .size = bo_bytes, .flags = BO_WC | BO_GUEST_ALLOC, .blob_id = bo_blob };
  r = ccmd(ctx, &gem, sizeof gem, NULL, 0);
  STEP("GEM_NEW (guest-allocated)", r == 0 && shmem->async_error == 0, "%d, async errors %u", r, shmem->async_error);
  xh_gpu_entry pages = { .addr = bo_pa, .length = bo_bytes };
  r = gpu->create_blob(gpu, ctx, bo_res, XH_GPU_BLOB_HOST3D_GUEST, XH_GPU_BLOB_FLAG_GUEST_HANDLE, bo_blob, bo_bytes, &pages, 1);
  STEP("the buffer's pages (udmabuf -> KGSL import -> bind)", r == 0, "%s", strerror(-r));
  printf("buffer: 16 KiB of guest pages imported through /dev/udmabuf, bound at GPU address 0x%llx\n", (unsigned long long)iova);

  msm_queue queue = { 0 };
  r = ioctl_simple(ctx, IOCTL_SUBMITQUEUE_NEW, &queue, sizeof queue);
  STEP("SUBMITQUEUE_NEW (a KGSL draw context)", r == 0, "%d", r);
  struct {
    ccmd_req hdr;
    uint32_t flags, queue_id, nr_bos, nr_cmds, fence;
    struct { uint32_t flags, handle; uint64_t presumed; } bo;
    struct { uint32_t type, submit_idx, submit_offset, size, pad, nr_relocs; uint64_t relocs; } cmd;
  } __attribute__((packed, aligned(4))) job = {
    .hdr.cmd = CCMD_GEM_SUBMIT, .flags = PIPE_3D0 | SUBMIT_FENCE_SN_IN, .queue_id = queue.id, .nr_bos = 1, .nr_cmds = 1, .fence = 1,
    .bo = { .flags = SUBMIT_BO_READ, .handle = bo_res, .presumed = iova }, .cmd = { .type = SUBMIT_CMD_BUF, .size = 64 * 4 },
  };
  struct timespec start, end;
  clock_gettime(CLOCK_MONOTONIC, &start);
  r = ccmd(ctx, &job, sizeof job, NULL, 0);
  STEP("GEM_SUBMIT (32 CP_NOPs)", r == 0 && shmem->async_error == 0, "%d, async errors %u: KGSL refused the command", r, shmem->async_error);
  r = gpu->submit_fence(gpu, ctx, 1, 1);             // the queue's ring: priority 0 is ring 1
  STEP("the fence", r == 0, "%s", strerror(-r));
  struct timespec until;
  clock_gettime(CLOCK_REALTIME, &until);
  until.tv_sec += 5;
  pthread_mutex_lock(&lock);
  while (retired_fence < 1 && r == 0) r = pthread_cond_timedwait(&retired_cond, &lock, &until);
  pthread_mutex_unlock(&lock);
  clock_gettime(CLOCK_MONOTONIC, &end);
  STEP("the fence retiring", r == 0, "not within 5 s: the GPU did not run the command, or KGSL did not report it");
  r = get_param(ctx, 0x09 /* FAULTS */, &value);
  printf("submit: queue %u, 256 bytes of commands at 0x%llx run by the GPU, fence retired after %.2f ms; GPU faults %llu, async errors %u\n", queue.id,
         (unsigned long long)iova, (double)(end.tv_sec - start.tv_sec) * 1e3 + (double)(end.tv_nsec - start.tv_nsec) / 1e6,
         r ? 0ull : (unsigned long long)value, shmem->async_error);
  STEP("no fault", shmem->async_error == 0, "async errors %u", shmem->async_error);
  gpu->unref(gpu, bo_res);
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
  if (!all && strcmp(what, "nodes") && strcmp(what, "probe") && strcmp(what, "submit")) {
    fprintf(stderr, "usage: gpuctl [nodes|probe|submit] [-v]\n");
    return 2;
  }
  if (all || !strcmp(what, "nodes")) { int bad = nodes(); if (!all) { printf(bad ? "FAILED at: the device nodes\n" : "ok\n"); return bad; } }
  if (open_renderer()) return 1;
  if ((all || !strcmp(what, "submit")) && submit()) return 1;
  printf("ok\n");
  return 0;
}
