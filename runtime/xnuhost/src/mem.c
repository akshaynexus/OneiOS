// SPDX-License-Identifier: MIT
// Guest RAM: one memfd. xnuhost maps all of it once (the kernel's window, at the address the
// kernel runs at); each iOS task maps the pages XNU's pmap gives it, at the addresses iOS expects,
// from the same memfd. Shared memory between tasks is therefore two mappings of one offset.
#define _GNU_SOURCE
#include "xnuhost.h"
#include <errno.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

xh_mem g_mem = { .fd = -1 };

// Where the window goes in xnuhost's address space. Below every address a 39-bit kernel hands out
// by itself (libraries and stacks sit near the top), above the iOS shared region's addresses so a
// kernel pointer never looks like a user one.
#define RAM_HINT 0x2000000000ull     // 128 GiB
#define IO_HINT  0x1f00000000ull

int xh_mem_create(uint64_t size)
{
  int fd = (int)syscall(SYS_memfd_create, "xnuhost-ram", 0u);     // inherited by the task factory: no CLOEXEC
  if (fd < 0) { LOGE("memfd_create: %s", strerror(errno)); return -1; }
  if (ftruncate(fd, (off_t)size)) { LOGE("RAM of %llu MiB: %s", (unsigned long long)(size >> 20), strerror(errno)); close(fd); return -1; }
  g_mem.fd = fd;
  g_mem.size = size;
  return 0;
}

int xh_mem_map(uint64_t phys, uint64_t low21)
{
  uint64_t want = RAM_HINT + (low21 & 0x1fffff);
  void *p = MAP_FAILED;
  for (int attempt = 0; attempt < 16 && p == MAP_FAILED; attempt++, want += 0x400000000ull) {
#ifdef MAP_FIXED_NOREPLACE
    p = mmap((void *)(uintptr_t)want, g_mem.size, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_SHARED | MAP_FIXED_NOREPLACE, g_mem.fd, 0);
#else
    p = mmap((void *)(uintptr_t)want, g_mem.size, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_SHARED, g_mem.fd, 0);
#endif
    if (p != MAP_FAILED && (uint64_t)(uintptr_t)p != want) { munmap(p, g_mem.size); p = MAP_FAILED; }
  }
  if (p == MAP_FAILED) { LOGE("cannot map %llu MiB of RAM executable: %s", (unsigned long long)(g_mem.size >> 20), strerror(errno)); return -1; }
  g_mem.host = p;
  g_mem.phys = phys;
  madvise(p, g_mem.size, MADV_DONTDUMP);
  return 0;
}

int xh_mem_io(uint64_t phys, uint64_t size)
{
  void *p = mmap((void *)(uintptr_t)IO_HINT, size, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  if (p == MAP_FAILED) { LOGE("cannot reserve the I/O aperture: %s", strerror(errno)); return -1; }
  g_mem.io_host = p;
  g_mem.io_phys = phys;
  g_mem.io_size = size;
  return 0;
}

void *xh_ram(uint64_t pa, uint64_t len)
{
  if (!g_mem.host || pa < g_mem.phys || pa - g_mem.phys > g_mem.size || len > g_mem.size - (pa - g_mem.phys)) return NULL;
  return g_mem.host + (pa - g_mem.phys);
}
