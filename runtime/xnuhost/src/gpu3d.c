// SPDX-License-Identifier: MIT
// virtio-gpu 3D: guest pages as dma-bufs. /dev/udmabuf makes one from ranges of a memfd; guest RAM is one memfd
// (mem.c), so a blob's guest segments become dma-bufs KGSL imports, with no copy and no pinning by address: one
// where the blob fits udmabuf's limits (size_limit_mb, list_limit), several in a row where it does not.
#include "gpu3d.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <linux/udmabuf.h>

#define MAX_BYTES (64ull << 20)       // the kernel's defaults; a host that allows more is not given more
#define MAX_RUNS  1024u
static uint64_t limit_bytes;
static unsigned limit_runs;

static long parameter(const char *name)
{
  char path[96], text[32] = "";
  snprintf(path, sizeof path, "/sys/module/udmabuf/parameters/%s", name);
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return -1;
  ssize_t got = read(fd, text, sizeof text - 1);
  close(fd);
  return got > 0 ? strtol(text, NULL, 10) : -1;
}

void xh_gpu3d_udmabuf_set_limits(uint64_t bytes, unsigned runs)
{
  limit_bytes = bytes & ~0xfffull;
  limit_runs = runs;
}

void xh_gpu3d_udmabuf_limits(uint64_t *bytes, unsigned *runs)
{
  if (!limit_bytes || !limit_runs) {
    // Not readable (another SELinux domain) or absurd: the defaults.
    long mb = parameter("size_limit_mb"), list = parameter("list_limit");
    limit_bytes = mb > 0 && (uint64_t)mb < (MAX_BYTES >> 20) ? (uint64_t)mb << 20 : MAX_BYTES;
    limit_runs = list > 0 && (unsigned long)list < MAX_RUNS ? (unsigned)list : MAX_RUNS;
  }
  if (bytes) *bytes = limit_bytes;
  if (runs) *runs = limit_runs;
}

void xh_gpu3d_chunks_free(xh_gpu_chunk *chunks, unsigned n)
{
  for (unsigned i = 0; chunks && i < n; i++) if (chunks[i].fd >= 0) close(chunks[i].fd);
  free(chunks);
}

int xh_gpu3d_udmabufs(int ram_fd, uint64_t ram_phys, uint64_t ram_size, const xh_gpu_entry *entries, unsigned n, xh_gpu_chunk **out)
{
  uint64_t max_bytes;
  unsigned max_runs;
  xh_gpu3d_udmabuf_limits(&max_bytes, &max_runs);
  *out = NULL;
  if (!n) { errno = EINVAL; return -1; }
  for (unsigned i = 0; i < n; i++) {
    uint64_t addr = entries[i].addr, length = entries[i].length;
    if (!length || ((addr | length) & 0xfff) || addr < ram_phys || addr - ram_phys > ram_size || length > ram_size - (addr - ram_phys)) { errno = EINVAL; return -1; }
  }
  struct udmabuf_create_list *list = calloc(1, sizeof *list + max_runs * sizeof(struct udmabuf_create_item));
  xh_gpu_chunk *chunks = NULL;
  unsigned nchunks = 0, room = 0, count = 0;
  uint64_t bytes = 0;
  int dev = -1, error = ENOMEM;
  if (!list) goto fail;
  if ((dev = open("/dev/udmabuf", O_RDWR | O_CLOEXEC)) < 0) { error = errno; goto fail; }
  for (unsigned i = 0; i <= n; i++) {
    uint64_t offset = i < n ? entries[i].addr - ram_phys : 0, left = i < n ? entries[i].length : 0;
    do {
      struct udmabuf_create_item *last = count ? &list->list[count - 1] : NULL;
      bool joins = last && left && last->offset + last->size == offset;
      // The chunk is done: the blob is, or it is full, or this piece would be one run too many.
      if (count && (i == n || bytes == max_bytes || (!joins && count == max_runs))) {
        if (nchunks == room) {
          xh_gpu_chunk *more = realloc(chunks, (room = room ? room * 2 : 4) * sizeof *chunks);
          if (!more) { error = ENOMEM; goto fail; }
          chunks = more;
        }
        list->flags = UDMABUF_FLAGS_CLOEXEC;
        list->count = count;
        int fd = ioctl(dev, UDMABUF_CREATE_LIST, list);
        if (fd < 0) { error = errno; goto fail; }
        chunks[nchunks++] = (xh_gpu_chunk){ .fd = fd, .size = bytes };
        bytes = 0; count = 0;
        continue;
      }
      if (!left) break;
      uint64_t take = left < max_bytes - bytes ? left : max_bytes - bytes;
      if (joins) last->size += take;
      else list->list[count++] = (struct udmabuf_create_item){ .memfd = (uint32_t)ram_fd, .offset = offset, .size = take };
      bytes += take; offset += take; left -= take;
    } while (left || i == n);
  }
  close(dev);
  free(list);
  *out = chunks;
  return (int)nchunks;
fail:
  if (dev >= 0) close(dev);
  free(list);
  xh_gpu3d_chunks_free(chunks, nchunks);
  errno = error;
  return -1;
}

int xh_gpu3d_udmabuf(int ram_fd, uint64_t ram_phys, uint64_t ram_size, const xh_gpu_entry *entries, unsigned n)
{
  xh_gpu_chunk *chunks;
  int count = xh_gpu3d_udmabufs(ram_fd, ram_phys, ram_size, entries, n, &chunks);
  if (count < 0) return -1;
  int fd = count == 1 ? chunks[0].fd : -1;
  if (count == 1) chunks[0].fd = -1;
  xh_gpu3d_chunks_free(chunks, (unsigned)count);
  if (fd < 0) errno = E2BIG;
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
