// The guest gives free pages back to Linux: ram_discard (hostcall.h), used by the hosted kernel's free-page
// reporting (kernel patch 0058, docs/booter.md 2.2). Portable: the host tests run it on a memfd of their own.
#define _GNU_SOURCE 1   // fallocate (glibc)
#include "xnuhost.h"
#include <errno.h>
#include <fcntl.h>
#include <linux/falloc.h>
#include <time.h>

xh_discard_stats g_discard;

static uint64_t now_ns(void)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (uint64_t)t.tv_sec * 1000000000ull + (uint64_t)t.tv_nsec;
}

// [phys, phys + size) of the RAM memfd `fd`, whose first byte is physical address `fd_base`. Allowed only inside the
// kernel's window [lo, lo + len) (never the boot framebuffer or the GPU's pool above it) and page-aligned (`page`, the
// host's page size), or -EINVAL. A hole is punched in the memfd: Linux frees the pages and drops them from every
// mapping (xnuhost's window, every task that maps them); the next access reads zeros, a fresh page. The size of the memfd is kept (the GPU's udmabufs
// need its shrink seal; a punch is allowed under it).
int64_t xh_ram_discard_fd(int fd, uint64_t fd_base, uint64_t lo, uint64_t len, uint64_t phys, uint64_t size, uint64_t page)
{
  if (fd < 0 || page == 0 || ((phys | size) & (page - 1)) || size == 0 || lo < fd_base || phys < lo || phys - lo > len ||
      size > len - (phys - lo)) {
    __atomic_fetch_add(&g_discard.refused, 1, __ATOMIC_RELAXED);
    return -EINVAL;
  }
  uint64_t t0 = now_ns();
  int r = fallocate(fd, FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, (off_t)(phys - fd_base), (off_t)size) ? -errno : 0;
  __atomic_fetch_add(&g_discard.ns, now_ns() - t0, __ATOMIC_RELAXED);
  __atomic_fetch_add(&g_discard.calls, 1, __ATOMIC_RELAXED);
  if (r) __atomic_fetch_add(&g_discard.failed, 1, __ATOMIC_RELAXED);
  else __atomic_fetch_add(&g_discard.bytes, size, __ATOMIC_RELAXED);
  return r;
}
