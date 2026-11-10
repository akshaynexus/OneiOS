// SPDX-License-Identifier: MIT
// virtio-gpu 3D: guest pages as a dma-buf. /dev/udmabuf makes one from ranges of a memfd; guest RAM is one memfd
// (mem.c), so a blob's guest segments become one dma-buf KGSL imports, with no copy and no pinning by address.
#include "gpu3d.h"
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <linux/udmabuf.h>

int xh_gpu3d_udmabuf(int ram_fd, uint64_t ram_phys, uint64_t ram_size, const xh_gpu_entry *entries, unsigned n)
{
  if (!n) { errno = EINVAL; return -1; }
  struct udmabuf_create_list *list = calloc(1, sizeof *list + n * sizeof(struct udmabuf_create_item));
  if (!list) { errno = ENOMEM; return -1; }
  unsigned count = 0;
  for (unsigned i = 0; i < n; i++) {
    uint64_t addr = entries[i].addr, length = entries[i].length;
    if (!length || ((addr | length) & 0xfff) || addr < ram_phys || addr - ram_phys > ram_size || length > ram_size - (addr - ram_phys)) {
      free(list);
      errno = EINVAL;
      return -1;
    }
    struct udmabuf_create_item *last = count ? &list->list[count - 1] : NULL;
    if (last && last->offset + last->size == addr - ram_phys) { last->size += length; continue; }      // the kernel caps the count (list_limit)
    list->list[count++] = (struct udmabuf_create_item){ .memfd = (uint32_t)ram_fd, .offset = addr - ram_phys, .size = length };
  }
  list->flags = UDMABUF_FLAGS_CLOEXEC;
  list->count = count;
  int dev = open("/dev/udmabuf", O_RDWR | O_CLOEXEC), fd = -1, error = errno;
  if (dev >= 0) {
    fd = ioctl(dev, UDMABUF_CREATE_LIST, list);
    error = errno;
    close(dev);
  }
  free(list);
  errno = error;
  return fd;
}

#ifndef XH_DRM2KGSL
xh_gpu3d *xh_gpu3d_kgsl_open(int ram_fd, uint64_t ram_phys, uint64_t ram_size, void *pool_host, uint64_t pool_pa,
                             uint64_t pool_size, xh_gpu3d_log log, const char **why)
{
  (void)ram_fd; (void)ram_phys; (void)ram_size; (void)pool_host; (void)pool_pa; (void)pool_size; (void)log;
  *why = "this build has no drm2kgsl (external/virglrenderer was absent: scripts/setup/get-references.sh virglrenderer)";
  return NULL;
}
#endif
