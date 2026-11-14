// SPDX-License-Identifier: MIT
// The guest's RAM, held fixed for the session (docs/booter.md 2.2, "Fixed RAM"; the owner's decision of 2026-11-13: iOS's
// RAM must not grow and shrink during a session). xnuhost takes the whole memfd once, at the start: a thread of its own
// at nice 19 prefaults it in steps (mlock, which also locks it, where Linux allows; else a read of each page), and nothing is given
// back until the process ends (ram_discard is refused; kernel 0067 does not ask). The exit frees all of it at once (the
// memfd's last mapping and descriptor go with the session's processes). Portable: the host tests run it on memfds of
// their own, with a failing mlock put in.
//
// Never: a realtime class, a lower OOM score, another cgroup, a write outside the process (docs/booter.md 2.9). Read only:
// /proc/meminfo, /proc/vmstat, /proc/self/{status,stat,cgroup}, the cgroup's memory files, KGSL's per-process files.
#define _GNU_SOURCE 1
#include "xnuhost.h"
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

xh_ramfix g_ramfix;
int (*xh_ramfix_mlock)(const void *, size_t) = mlock;
int (*xh_ramfix_munlock)(const void *, size_t) = munlock;

static uint64_t ms_now(void)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (uint64_t)t.tv_sec * 1000u + (uint64_t)t.tv_nsec / 1000000u;
}

// One step: locked (and so in the host's RAM) when the lock still holds, else read once a page through the mapping (a
// read of a hole through a shared mapping fills it with a zeroed page: shmem has no shared zero page). Neither changes a
// byte the guest wrote. Not fallocate: the pages it allocates stay "not up to date" until first touched, so mincore (the
// `memory:` line's "in the host's RAM") does not count them, and the guest's first touch still takes a fault.
// False when the prefault should stop here: the lock was asked for and failed. A page prefaulted and not locked is one
// iOS has not used yet, so it is the coldest page there is and the first the host swaps out (the PC, 2026-11-14: 841 MiB
// of a prefaulted, unlocked 4 GiB guest swapped out in 15 min, against 6 MiB faulted in as used); the rest is faulted
// in as iOS uses it.
static bool step(xh_ramfix *r, uint8_t *base, uint64_t at, uint64_t length)
{
  if (r->locking) {
    if (xh_ramfix_mlock(base + at, (size_t)length) == 0) { __atomic_store_n(&r->locked, at + length, __ATOMIC_RELAXED); return true; }
    // All or nothing: a RAM locked in part leaves the host short of what it could not lock and swaps the rest anyway.
    r->lock_error = errno ? errno : EINVAL;
    r->locking = false;
    uint64_t had = __atomic_load_n(&r->locked, __ATOMIC_RELAXED);
    if (had) (void)xh_ramfix_munlock(base, (size_t)had);
    __atomic_store_n(&r->locked, 0, __ATOMIC_RELAXED);
    return false;
  }
  long page = sysconf(_SC_PAGESIZE);
  for (uint64_t i = 0; i < length; i += (uint64_t)page) (void)*(volatile uint8_t *)(base + at + i);
  return true;
}

void xh_ramfix_prefault(xh_ramfix *r, uint8_t *base, uint64_t size)
{
  uint64_t chunk = r->step ? r->step : XH_RAMFIX_STEP, t0 = ms_now();
  r->size = size;
  r->locking = r->lock;
  r->lock_error = 0;
  for (uint64_t at = 0; at < size && !__atomic_load_n(&r->stop, __ATOMIC_RELAXED); at += chunk) {
    uint64_t length = size - at < chunk ? size - at : chunk;
    if (!step(r, base, at, length)) break;
    __atomic_store_n(&r->prefaulted, at + length, __ATOMIC_RELAXED);
  }
  r->ms = ms_now() - t0;
  __atomic_store_n(&r->done, true, __ATOMIC_RELEASE);
}

bool xh_ramfix_valve(xh_ramfix *r, uint8_t *base, uint64_t available)
{
  uint64_t locked = __atomic_load_n(&r->locked, __ATOMIC_RELAXED);
  if (!locked || r->unlocked || !r->unlock_below || available >= r->unlock_below) return false;
  (void)xh_ramfix_munlock(base, (size_t)locked);
  r->unlocked = true;
  r->unlock_available = available;
  __atomic_store_n(&r->locked, 0, __ATOMIC_RELAXED);
  return true;
}

// ---- what Linux says about locking here (read only), once at the start ----------------------------------------------
static bool read_line(const char *path, char *out, size_t size)
{
  FILE *f = fopen(path, "r");
  if (!f) return false;
  bool ok = fgets(out, (int)size, f) != NULL;
  fclose(f);
  if (ok) out[strcspn(out, "\n")] = 0;
  return ok;
}

// CapEff's bit 14 (CAP_IPC_LOCK: mlock past RLIMIT_MEMLOCK).
static int cap_ipc_lock(void)
{
  FILE *f = fopen("/proc/self/status", "r");
  char line[256];
  int have = -1;
  while (f && fgets(line, sizeof line, f))
    if (!strncmp(line, "CapEff:", 7)) { have = (int)(strtoull(line + 7, NULL, 16) >> 14 & 1); break; }
  if (f) fclose(f);
  return have;
}

void xh_ramfix_facts(char *out, size_t size)
{
  struct rlimit l = { 0, 0 };
  char limit[32], cg[256] = "?", path[320], max[32] = "?", swap[32] = "?", high[32] = "?";
  getrlimit(RLIMIT_MEMLOCK, &l);
  if (l.rlim_cur == RLIM_INFINITY) snprintf(limit, sizeof limit, "unlimited");
  else snprintf(limit, sizeof limit, "%llu KiB", (unsigned long long)(l.rlim_cur >> 10));
  // cgroup v2: "0::/path"; the memory controller's limits of that cgroup, if it has them
  char line[256];
  FILE *f = fopen("/proc/self/cgroup", "r");
  while (f && fgets(line, sizeof line, f))
    if (!strncmp(line, "0::", 3)) { line[strcspn(line, "\n")] = 0; snprintf(cg, sizeof cg, "%s", line + 3); break; }
  if (f) fclose(f);
  if (strcmp(cg, "?")) {
    snprintf(path, sizeof path, "/sys/fs/cgroup%s/memory.max", cg);       if (!read_line(path, max, sizeof max)) strcpy(max, "-");
    snprintf(path, sizeof path, "/sys/fs/cgroup%s/memory.swap.max", cg);  if (!read_line(path, swap, sizeof swap)) strcpy(swap, "-");
    snprintf(path, sizeof path, "/sys/fs/cgroup%s/memory.high", cg);      if (!read_line(path, high, sizeof high)) strcpy(high, "-");
  }
  int cap = cap_ipc_lock();
  snprintf(out, size, "RLIMIT_MEMLOCK %s, CAP_IPC_LOCK %s; cgroup %s (memory.max %s, memory.high %s, memory.swap.max %s)", limit,
           cap < 0 ? "?" : cap ? "yes" : "no", cg, max, high, swap);
}

// ---- the counters of the `memory:` lines ------------------------------------------------------------------------------
static uint64_t field_kib(const char *path, const char *key)
{
  FILE *f = fopen(path, "r");
  char line[256];
  size_t n = strlen(key);
  uint64_t v = 0;
  while (f && fgets(line, sizeof line, f))
    if (!strncmp(line, key, n) && (line[n] == ':' || line[n] == ' ')) { v = strtoull(line + n + 1, NULL, 10); break; }
  if (f) fclose(f);
  return v;
}

void xh_ramfix_sample(xh_ramfix_counters *c)
{
  memset(c, 0, sizeof *c);
  c->vm_locked = field_kib("/proc/self/status", "VmLck") << 10;
  c->mlocked = field_kib("/proc/meminfo", "Mlocked") << 10;
  c->swap_ins = field_kib("/proc/vmstat", "pswpin");
  c->swap_outs = field_kib("/proc/vmstat", "pswpout");
  // /proc/self/stat: majflt is the 12th field, after "(comm)" which may hold spaces
  char text[1024];
  FILE *f = fopen("/proc/self/stat", "r");
  size_t got = f ? fread(text, 1, sizeof text - 1, f) : 0;
  if (f) fclose(f);
  text[got] = 0;
  char *p = strrchr(text, ')');
  for (int field = 2; p && field < 12; field++) p = strchr(p + 1, ' ');
  if (p) c->major_faults = strtoull(p + 1, NULL, 10);
}

size_t xh_kgsl_memory(int pid, char *out, size_t size)
{
  char dir[96], path[400], value[64];
  snprintf(dir, sizeof dir, "/sys/class/kgsl/kgsl/proc/%d", pid);
  DIR *d = opendir(dir);
  size_t used = 0;
  if (!d || !size) { if (d) closedir(d); if (size) *out = 0; return 0; }
  *out = 0;
  struct dirent *e;
  unsigned shown = 0;
  while ((e = readdir(d)) && shown < 12) {
    if (e->d_name[0] == '.') continue;
    snprintf(path, sizeof path, "%s/%s", dir, e->d_name);
    char *end;
    if (!read_line(path, value, sizeof value)) continue;
    unsigned long long v = strtoull(value, &end, 10);
    if (end == value || (*end && *end != ' ')) continue;                 // one number each, as KGSL prints them
    int n = snprintf(out + used, size - used, "%s%s %llu", shown ? ", " : "", e->d_name, v);
    if (n < 0 || (size_t)n >= size - used) break;
    used += (size_t)n;
    shown++;
  }
  closedir(d);
  return used;
}

// ---- the thread ---------------------------------------------------------------------------------------------------------
static pthread_t thread;
static bool thread_on;
static uint8_t *ram_base;
static const char *meminfo_path = "/proc/meminfo";

static void say_done(xh_ramfix *r)
{
  char why[320];
  if (r->locked) snprintf(why, sizeof why, "locked (mlock: the host cannot swap it)");
  else if (!r->lock) snprintf(why, sizeof why, "not locked (%s)", r->lock_why ? r->lock_why : "ram-lock=0");
  else snprintf(why, sizeof why, "not locked (mlock: %s), so not prefaulted further: the rest is faulted in as iOS uses it (an unlocked, unused page is the first the host swaps)", strerror(r->lock_error));
  LOGI("memory: the guest's RAM is held: %llu MiB prefaulted in %llu ms, %s%s", (unsigned long long)(r->prefaulted >> 20),
       (unsigned long long)r->ms, why, r->locked || r->lock ? "" : ", read once a page (ram-lock=0)");
}

static void *ramfix_main(void *arg)
{
  xh_ramfix *r = arg;
  // Below every vCPU and task thread (nice 0): a prefault takes no CPU time the guest wants. SCHED_OTHER, never realtime.
  (void)setpriority(PRIO_PROCESS, (id_t)syscall(SYS_gettid), 19);
  xh_ramfix_prefault(r, ram_base, r->size);
  if (!__atomic_load_n(&r->stop, __ATOMIC_RELAXED)) say_done(r);
  // Then once a second: the host's MemAvailable against ram-unlock-below while the RAM is locked (the one way out of a
  // lock that leaves the host too little: the host may swap the guest again, which is today's way; the RAM stays held).
  while (!__atomic_load_n(&r->stop, __ATOMIC_RELAXED) && r->locked && r->unlock_below) {
    for (int i = 0; i < 10 && !__atomic_load_n(&r->stop, __ATOMIC_RELAXED); i++) {
      struct timespec s = { 0, 100 * 1000 * 1000 };
      nanosleep(&s, NULL);
    }
    uint64_t total = 0, available = 0;
    if (xh_meminfo(meminfo_path, &total, &available)) continue;
    if (xh_ramfix_valve(r, ram_base, available))
      LOGE("memory: host MemAvailable %llu MiB is under ram-unlock-below (%llu MiB): the guest's RAM is unlocked for the rest of "
           "the session; the host may swap it now (it stays held, nothing is given back)", (unsigned long long)(available >> 20),
           (unsigned long long)(r->unlock_below >> 20));
  }
  return NULL;
}

int xh_ramfix_start(xh_ramfix *r, uint8_t *base, uint64_t size, const char *meminfo)
{
  ram_base = base;
  r->size = size;
  if (meminfo && *meminfo) meminfo_path = meminfo;
  int rc = pthread_create(&thread, NULL, ramfix_main, r);
  if (rc) { LOGE("memory: no thread for the prefault: %s", strerror(rc)); return -1; }
  pthread_setname_np(thread, "xnuhost-ram");
  thread_on = true;
  return 0;
}

void xh_ramfix_stop(xh_ramfix *r)
{
  __atomic_store_n(&r->stop, true, __ATOMIC_RELAXED);
  if (thread_on) pthread_join(thread, NULL);
  thread_on = false;
}
