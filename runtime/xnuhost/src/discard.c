// The guest gives free pages back to Linux: ram_discard (hostcall.h), used by the hosted kernel's free-page
// reporting (kernel patch 0058, docs/booter.md 2.2). Portable: the host tests run it on a memfd of their own.
#define _GNU_SOURCE 1   // fallocate (glibc)
#include "xnuhost.h"
#include <errno.h>
#include <fcntl.h>
#include <linux/falloc.h>
#include <sys/mman.h>
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

// While the RAM is held fixed (ram-giveback=0, XH_FEATURE_RAM_FIXED) nothing goes back: a hole punched in a RAM that is
// held for the session would only be faulted in again. A kernel from before 0067 still asks; its first -EPERM stops its
// reporting for the boot (0058: "free pages are no longer given back").
uint64_t g_discard_fixed_refused;

int64_t xh_ram_discard_host(bool fixed, int fd, uint64_t fd_base, uint64_t lo, uint64_t len, uint64_t phys, uint64_t size, uint64_t page)
{
  if (fixed) { __atomic_fetch_add(&g_discard_fixed_refused, 1, __ATOMIC_RELAXED); return -EPERM; }
  return xh_ram_discard_fd(fd, fd_base, lo, len, phys, size, page);
}

uint64_t xh_ram_resident(const void *base, uint64_t size, uint64_t page)
{
  static unsigned char vec[16384];            // one call per 16384 pages (64 MiB of 4 KiB pages); the main loop's only
  if (!base || !page || ((uintptr_t)base & (page - 1))) return 0;
  uint64_t resident = 0;
  for (uint64_t at = 0; at < size; at += (uint64_t)sizeof vec * page) {
    uint64_t length = size - at < (uint64_t)sizeof vec * page ? size - at : (uint64_t)sizeof vec * page;
    if (mincore((void *)((uintptr_t)base + at), (size_t)length, vec)) continue;
    uint64_t pages = (length + page - 1) / page;
    for (uint64_t i = 0; i < pages; i++) resident += vec[i] & 1;
  }
  return resident * page;
}
