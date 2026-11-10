// SPDX-License-Identifier: MIT
// virtio-gpu 3D: what virtio_gpu.c asks of the renderer behind it (VirtIO 1.2 5.7 with crosvm's DRM native context).
// The real renderer is gpu3d_kgsl.c: Droid-VM's drm2kgsl (virglrenderer's msm protocol served on /dev/kgsl-3d0),
// linked into xnuhost (static; no helper process). The unit tests put a fake one here and drive the device with
// generated command streams.
#ifndef XNUHOST_GPU3D_H
#define XNUHOST_GPU3D_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct xh_virtio xh_virtio;

// A guest memory segment of a HOST3D_GUEST blob: guest physical address and length (virtio_gpu_mem_entry).
typedef struct { uint64_t addr; uint32_t length, pad; } xh_gpu_entry;

enum { XH_GPU_CAPSET_DRM = 6 };
enum { XH_GPU_BLOB_GUEST = 1, XH_GPU_BLOB_HOST3D = 2, XH_GPU_BLOB_HOST3D_GUEST = 3 };
enum { XH_GPU_BLOB_FLAG_MAPPABLE = 1, XH_GPU_BLOB_FLAG_GUEST_HANDLE = 8 };
enum { XH_GPU_MAP_CACHE_CACHED = 1, XH_GPU_MAP_CACHE_UNCACHED = 2, XH_GPU_MAP_CACHE_WC = 3 };

// The device calls this when a context's ring passes `fence` (and every earlier fence of that ring). Any thread.
typedef void (*xh_gpu3d_retire)(xh_virtio *dev, uint32_t ctx, uint32_t ring, uint64_t fence);

typedef struct xh_gpu3d xh_gpu3d;
struct xh_gpu3d {
  void *opaque;
  // Capsets: count, then each one's id, highest version and size; the bytes of one.
  unsigned (*capset_count)(xh_gpu3d *b);
  bool     (*capset_info)(xh_gpu3d *b, unsigned index, uint32_t *id, uint32_t *version, uint32_t *size);
  size_t   (*capset)(xh_gpu3d *b, uint32_t id, uint32_t version, void *out, size_t max);
  // Contexts. Returns 0 or a negative errno.
  int      (*ctx_create)(xh_gpu3d *b, uint32_t ctx, uint32_t capset, const char *name, size_t name_length);
  void     (*ctx_destroy)(xh_gpu3d *b, uint32_t ctx);
  int      (*submit)(xh_gpu3d *b, uint32_t ctx, const void *commands, size_t size);
  // A fence on `ring` of `ctx`: retired later through `retire`, or at once (from inside this call).
  int      (*submit_fence)(xh_gpu3d *b, uint32_t ctx, uint32_t ring, uint64_t fence);
  // Blobs. HOST3D_GUEST blobs (a context's BOs) come with the guest's pages; HOST3D ones (a context's shmem) are the
  // renderer's; GUEST ones (ctx 0: the display's surfaces, made with the GUEST_HANDLE flag) are only made known, so
  // that a context can attach them. All calls but `retire` come from the device's one thread.
  int      (*create_blob)(xh_gpu3d *b, uint32_t ctx, uint32_t res, uint32_t blob_mem, uint32_t flags, uint64_t blob_id,
                          uint64_t size, const xh_gpu_entry *entries, unsigned nentries);
  // Where a host blob is in the pool the guest knows (/arm-io/virtio-gpu drm-host-pool), and how to cache it.
  int      (*map_blob)(xh_gpu3d *b, uint32_t res, uint64_t *pool_offset, uint32_t *map_info);
  int      (*attach)(xh_gpu3d *b, uint32_t ctx, uint32_t res, bool attach);
  void     (*unref)(xh_gpu3d *b, uint32_t res);
  // Set by the device before the first context.
  xh_gpu3d_retire retire;
  xh_virtio *dev;
};

// The virtio-gpu device with 3D (features VIRGL, RESOURCE_BLOB, CONTEXT_INIT, CREATE_GUEST_HANDLE; capsets from `b`).
// Its control queue is served by a thread of its own: a 3D command never runs in a vCPU's trap handler, and a
// command fenced on a context's ring is returned to the guest only when its fence retires (what crosvm does, and
// what UniXNU-droidvm's DroidVMVirtioGpu3D.cpp waits for). Guest blobs can be the scanout (SET_SCANOUT_BLOB); the 2D
// commands stay. With b == NULL it is xh_virtio_gpu_create's 2D device.
xh_virtio *xh_virtio_gpu_create_3d(uint64_t base, unsigned spi, unsigned width, unsigned height, xh_gpu3d *b);
// Test hook: waits until the 3D device's thread has nothing left to do.
void xh_virtio_gpu_idle(xh_virtio *dev);

// gpu3d_kgsl.c: drm2kgsl on /dev/kgsl-3d0. Guest pages are imported through /dev/udmabuf from the RAM memfd
// (`ram_fd`, guest physical `ram_phys` at offset 0, `ram_size` bytes); host blobs (the contexts' shmem) are carved
// from the pool, a range of that memfd the kernel does not use: guest physical `pool_pa`, `pool_size` bytes,
// mapped at `pool_host` here. The memfd must carry F_SEAL_SHRINK (udmabuf refuses one without). NULL when there is
// no KGSL, no udmabuf, no dma heap or no drm2kgsl in this build; `why` says. One per process.
typedef void (*xh_gpu3d_log)(int level, const char *line);      // 0 error, 1 info, 2 debug
xh_gpu3d *xh_gpu3d_kgsl_open(int ram_fd, uint64_t ram_phys, uint64_t ram_size, void *pool_host, uint64_t pool_pa,
                             uint64_t pool_size, xh_gpu3d_log log, const char **why);
// gpu3d.c: a udmabuf over guest physical segments of the RAM memfd (adjacent segments are joined). -1 and errno on
// failure (EINVAL: a segment outside the RAM or not whole pages, or the memfd without F_SEAL_SHRINK).
int xh_gpu3d_udmabuf(int ram_fd, uint64_t ram_phys, uint64_t ram_size, const xh_gpu_entry *entries, unsigned n);
#endif
