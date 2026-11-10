// SPDX-License-Identifier: MIT
// Guest RAM: one memfd. xnuhost maps all of it once (the kernel's window, at the address the
// kernel runs at); each iOS task maps the pages XNU's pmap gives it, at the addresses iOS expects,
// from the same memfd. Shared memory between tasks is therefore two mappings of one offset.
#define _GNU_SOURCE
#include "xnuhost.h"
#include <errno.h>
#include <stdio.h>
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

// The window's physical address IS its address in xnuhost: one number names a byte of RAM for
// the kernel's pointers, its "physical" addresses (page tables it may still write, DMA addresses in
// virtqueues, boot_args.physBase) and the offsets tasks map (address - base). Code that runs before
// the kernel's MMU switch and computes physical addresses therefore still touches the right bytes.
int xh_mem_map(uint64_t low21)
{
  uint64_t want = RAM_HINT + (low21 & 0x1fffff);
  void *p = MAP_FAILED;
  for (int attempt = 0; attempt < 16 && p == MAP_FAILED; attempt++, want += 0x400000000ull) {
    p = mmap((void *)(uintptr_t)want, g_mem.size, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_SHARED | MAP_FIXED_NOREPLACE, g_mem.fd, 0);
    if (p != MAP_FAILED && (uint64_t)(uintptr_t)p != want) { munmap(p, g_mem.size); p = MAP_FAILED; }
  }
  if (p == MAP_FAILED) { LOGE("cannot map %llu MiB of RAM executable: %s", (unsigned long long)(g_mem.size >> 20), strerror(errno)); return -1; }
  g_mem.host = p;
  g_mem.phys = (uint64_t)(uintptr_t)p;
  madvise(p, g_mem.size, MADV_DONTDUMP);
  return 0;
}

// The board's device windows: reserved, inaccessible pages. At their physical addresses when those
// are free in this process (so a device's physical address is also its pointer), else elsewhere;
// the io_map host call and xh_info.io_virt say where.
int xh_mem_io(uint64_t phys, uint64_t size)
{
  void *p = mmap((void *)(uintptr_t)phys, size, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE | MAP_FIXED_NOREPLACE, -1, 0);
  if (p != MAP_FAILED && (uint64_t)(uintptr_t)p != phys) { munmap(p, size); p = MAP_FAILED; }
  if (p == MAP_FAILED) p = mmap((void *)(uintptr_t)IO_HINT, size, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  if (p == MAP_FAILED) { LOGE("cannot reserve the I/O aperture: %s", strerror(errno)); return -1; }
  g_mem.io_host = p;
  g_mem.io_phys = phys;
  g_mem.io_size = size;
  return 0;
}

// The kernel's own address space outside the window (hostcall.h, kernel_map): reserved, no access.
int xh_mem_kva(void)
{
  const uint64_t top = 0x3c00000000ull;
  uint64_t base = ((uint64_t)(uintptr_t)g_mem.host + g_mem.size + 0x3fffffffull) & ~0x3fffffffull;
  if (base >= top) { LOGE("no room for the kernel's address space below 0x%llx", (unsigned long long)top); return -1; }
  void *p = mmap((void *)(uintptr_t)base, top - base, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE | MAP_FIXED_NOREPLACE, -1, 0);
  if (p == MAP_FAILED || (uint64_t)(uintptr_t)p != base) {
    if (p != MAP_FAILED) munmap(p, top - base);
    LOGE("cannot reserve the kernel's address space at 0x%llx: %s", (unsigned long long)base, strerror(errno));
    return -1;
  }
  g_mem.kva = base;
  g_mem.kva_size = top - base;
  return 0;
}

int64_t xh_mem_kernel_map(uint64_t va, uint64_t size, uint32_t prot, uint64_t phys)
{
  uint64_t page = (uint64_t)sysconf(_SC_PAGESIZE) - 1;
  if (!size || ((va | size | phys) & page) || va < g_mem.kva || va - g_mem.kva > g_mem.kva_size || size > g_mem.kva_size - (va - g_mem.kva) ||
      !xh_ram(phys, size)) return -EINVAL;
  int p = (prot & XH_PROT_READ ? PROT_READ : 0) | (prot & XH_PROT_WRITE ? PROT_WRITE : 0) | (prot & XH_PROT_EXEC ? PROT_EXEC : 0);
  void *at = mmap((void *)(uintptr_t)va, size, p, MAP_SHARED | MAP_FIXED, g_mem.fd, (off_t)(phys - g_mem.phys));
  return at == MAP_FAILED ? -errno : 0;
}

int64_t xh_mem_kernel_unmap(uint64_t va, uint64_t size)
{
  uint64_t page = (uint64_t)sysconf(_SC_PAGESIZE) - 1;
  if (!size || ((va | size) & page) || va < g_mem.kva || va - g_mem.kva > g_mem.kva_size || size > g_mem.kva_size - (va - g_mem.kva)) return -EINVAL;
  void *at = mmap((void *)(uintptr_t)va, size, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE | MAP_FIXED, -1, 0);
  return at == MAP_FAILED ? -errno : 0;
}

// The kernel's heap and every task's map are page-granular mappings of one memfd: far more areas
// than Linux's default limit of 65530 per process. Root may raise it.
void xh_mem_map_limit(void)
{
  const long want = 1048576;
  long have = 0;
  FILE *f = fopen("/proc/sys/vm/max_map_count", "r");
  if (f) { if (fscanf(f, "%ld", &have) != 1) have = 0; fclose(f); }
  if (have < want && (f = fopen("/proc/sys/vm/max_map_count", "w"))) {
    fprintf(f, "%ld\n", want);
    if (fclose(f) == 0) { LOGI("memory: vm.max_map_count raised from %ld to %ld", have, want); return; }
  }
  if (have < want) LOGE("memory: vm.max_map_count is %ld and could not be raised to %ld (not root?): the kernel's mappings may run out", have, want);
  else LOGI("memory: vm.max_map_count is %ld", have);
}

void *xh_ram(uint64_t pa, uint64_t len)
{
  if (!g_mem.host || pa < g_mem.phys || pa - g_mem.phys > g_mem.size || len > g_mem.size - (pa - g_mem.phys)) return NULL;
  return g_mem.host + (pa - g_mem.phys);
}
