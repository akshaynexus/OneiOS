// SPDX-License-Identifier: MIT
// CPU bridge: the host's cores from /sys/devices/system/cpu, the vCPU plan XNU's topology is made
// from (P = the big and prime cores, E = the little ones), and the placement of host threads.
// On the Galaxy S24 Ultra (SM8650): cpu0-1 Cortex-A520 (capacity-dmips-mhz 1024), cpu2-6 Cortex-A720
// (1792), cpu7 Cortex-X4 (1894); docs/hardware.md has the sources. Linux keeps clock control: the
// only frequency input from the guest is a utilisation clamp.
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include <errno.h>
#include <sched.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static void default_sink(int level, const char *line) { fprintf(stderr, "%s%s\n", level == 0 ? "bridges: error: " : "bridges: ", line); }
void (*br_log_sink)(int level, const char *line) = default_sink;

void br_log(int level, const char *fmt, ...)
{
  char line[512];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(line, sizeof line, fmt, ap);
  va_end(ap);
  if (br_log_sink) br_log_sink(level, line);
}

static bool read_text(const char *root, const char *rel, unsigned cpu, char *out, size_t size)
{
  char path[512];
  snprintf(path, sizeof path, rel, root, cpu);
  FILE *f = fopen(path, "r");
  if (!f) return false;
  size_t n = fread(out, 1, size - 1, f);
  fclose(f);
  out[n] = 0;
  while (n && (out[n - 1] == '\n' || out[n - 1] == ' ')) out[--n] = 0;
  return n > 0;
}

static bool read_number(const char *root, const char *rel, unsigned cpu, unsigned long long *out)
{
  char text[64];
  if (!read_text(root, rel, cpu, text, sizeof text)) return false;
  char *end;
  *out = strtoull(text, &end, 0);
  return end != text;
}

// "64K", "2048K", "12M" or bytes.
static uint32_t cache_bytes(const char *text)
{
  char *end;
  unsigned long long v = strtoull(text, &end, 10);
  if (*end == 'K') v <<= 10; else if (*end == 'M') v <<= 20;
  return v > 0xffffffffull ? 0 : (uint32_t)v;
}

static void read_caches(const char *root, unsigned cpu, br_host_cpu *c)
{
  for (unsigned i = 0; i < 6; i++) {
    char rel[128], level[16], type[32], size[32];
    snprintf(rel, sizeof rel, "%%s/devices/system/cpu/cpu%%u/cache/index%u/level", i);
    if (!read_text(root, rel, cpu, level, sizeof level)) continue;
    snprintf(rel, sizeof rel, "%%s/devices/system/cpu/cpu%%u/cache/index%u/type", i);
    if (!read_text(root, rel, cpu, type, sizeof type)) continue;
    snprintf(rel, sizeof rel, "%%s/devices/system/cpu/cpu%%u/cache/index%u/size", i);
    if (!read_text(root, rel, cpu, size, sizeof size)) continue;
    uint32_t bytes = cache_bytes(size);
    if (level[0] == '1' && type[0] == 'D') c->l1d_bytes = bytes;
    else if (level[0] == '1' && type[0] == 'I') c->l1i_bytes = bytes;
    else if (level[0] == '2') c->l2_bytes = bytes;
    else if (level[0] == '3') c->l3_bytes = bytes;
  }
}

static int compare_u32(const void *a, const void *b)
{
  uint32_t x = *(const uint32_t *)a, y = *(const uint32_t *)b;
  return x < y ? -1 : x > y;
}

int br_cpu_read(const char *root, br_cpu_topology *t)
{
  memset(t, 0, sizeof *t);
  unsigned low = ~0u, high = 0;
  for (unsigned i = 0; i < BR_MAX_CPUS; i++) {
    br_host_cpu *c = &t->cpu[i];
    unsigned long long v;
    char text[1024];
    bool have_capacity = read_number(root, "%s/devices/system/cpu/cpu%u/cpu_capacity", i, &v);
    if (have_capacity) c->capacity = (unsigned)v;
    bool have_freq = read_number(root, "%s/devices/system/cpu/cpu%u/cpufreq/cpuinfo_max_freq", i, &v);
    if (have_freq) c->max_khz = (uint32_t)v;
    if (!have_capacity && !have_freq && !read_text(root, "%s/devices/system/cpu/cpu%u/topology/core_id", i, text, sizeof text)) continue;
    c->present = true;
    t->ncpu = i + 1;
    if (read_number(root, "%s/devices/system/cpu/cpu%u/cpufreq/cpuinfo_min_freq", i, &v)) c->min_khz = (uint32_t)v;
    c->policy = i;
    if (read_text(root, "%s/devices/system/cpu/cpu%u/cpufreq/related_cpus", i, text, sizeof text)) c->policy = (unsigned)strtoul(text, NULL, 10);
    if (read_text(root, "%s/devices/system/cpu/cpu%u/cpufreq/scaling_available_frequencies", i, text, sizeof text)) {
      for (char *p = strtok(text, " \t\n"); p && c->nfreq < BR_MAX_FREQS; p = strtok(NULL, " \t\n")) {
        uint32_t khz = (uint32_t)strtoul(p, NULL, 10);
        if (khz) c->freq_khz[c->nfreq++] = khz;
      }
      qsort(c->freq_khz, c->nfreq, sizeof c->freq_khz[0], compare_u32);
    }
    if (!c->nfreq && c->max_khz) {                      // no table: the two ends
      if (c->min_khz && c->min_khz < c->max_khz) c->freq_khz[c->nfreq++] = c->min_khz;
      c->freq_khz[c->nfreq++] = c->max_khz;
    }
    if (read_number(root, "%s/devices/system/cpu/cpu%u/regs/identification/midr_el1", i, &v)) c->part = (unsigned)(v >> 4) & 0xfff;
    read_caches(root, i, c);
    // Without cpu_capacity (a kernel without the arch topology driver) the top frequency orders the cores.
    unsigned rank = c->capacity ? c->capacity : c->max_khz / 1000;
    if (rank < low) low = rank;
    if (rank > high) high = rank;
  }
  if (!t->ncpu) return -1;
  for (unsigned i = 0; i < t->ncpu; i++) {
    br_host_cpu *c = &t->cpu[i];
    if (!c->present) continue;
    unsigned rank = c->capacity ? c->capacity : c->max_khz / 1000;
    c->type = (low != high && rank == low) ? 'E' : 'P';
    if (c->type == 'E') t->n_e++; else t->n_p++;
  }
  return 0;
}

// Fills a vCPU from the host CPUs in `mask`: the fastest one gives the frequencies and caches.
static void vcpu_from(const br_cpu_topology *t, uint64_t mask, char type, br_vcpu *v)
{
  const br_host_cpu *best = NULL;
  for (unsigned i = 0; i < t->ncpu; i++) {
    const br_host_cpu *c = &t->cpu[i];
    if (!(mask >> i & 1)) continue;
    if (!best || c->capacity > best->capacity || (c->capacity == best->capacity && c->max_khz > best->max_khz)) best = c;
  }
  memset(v, 0, sizeof *v);
  v->type = type;
  v->cluster_id = type == 'P';
  v->affinity = mask;
  if (!best) return;
  v->capacity = best->capacity ? best->capacity : 1024;
  v->min_khz = best->min_khz;
  v->max_khz = best->max_khz;
  v->nfreq = best->nfreq;
  memcpy(v->freq_khz, best->freq_khz, sizeof v->freq_khz);
  v->l1d_bytes = best->l1d_bytes; v->l1i_bytes = best->l1i_bytes; v->l2_bytes = best->l2_bytes; v->l3_bytes = best->l3_bytes;
}

int br_cpu_plan_make(const br_cpu_topology *t, unsigned nvcpu, bool pin, br_cpu_plan *plan)
{
  memset(plan, 0, sizeof *plan);
  if (!nvcpu || nvcpu > BR_MAX_VCPUS || !t->ncpu || !t->n_p) return -1;
  uint64_t e_mask = 0, p_mask = 0;
  unsigned e_list[BR_MAX_CPUS], p_list[BR_MAX_CPUS], ne = 0, np = 0;
  for (unsigned i = 0; i < t->ncpu; i++) {
    if (!t->cpu[i].present) continue;
    if (t->cpu[i].type == 'E') { e_mask |= 1ull << i; e_list[ne++] = i; }
    else { p_mask |= 1ull << i; p_list[np++] = i; }
  }
  // The P list fastest first, so that with fewer vCPUs than cores the prime core is used.
  for (unsigned i = 0; i < np; i++)
    for (unsigned j = i + 1; j < np; j++)
      if (t->cpu[p_list[j]].capacity > t->cpu[p_list[i]].capacity ||
          (t->cpu[p_list[j]].capacity == t->cpu[p_list[i]].capacity && p_list[j] < p_list[i])) { unsigned x = p_list[i]; p_list[i] = p_list[j]; p_list[j] = x; }
  // How many E vCPUs: all the host's when every core gets a vCPU; otherwise one in four, at least
  // one (the boot CPU of an iPhone is an E core and XNU's AMP scheduler wants both kinds), none when
  // the host has no little cores or there is one vCPU.
  unsigned total = ne + np, n_e;
  if (!ne || nvcpu == 1) n_e = 0;
  else if (nvcpu >= total) n_e = ne;
  else { n_e = nvcpu / 4; if (!n_e) n_e = 1; if (n_e > ne) n_e = ne; }
  unsigned n_p = nvcpu - n_e;
  bool one_to_one = pin && n_e <= ne && n_p <= np;
  for (unsigned i = 0; i < n_e; i++) vcpu_from(t, one_to_one ? 1ull << e_list[i] : e_mask, 'E', &plan->v[i]);
  for (unsigned i = 0; i < n_p; i++) vcpu_from(t, one_to_one ? 1ull << p_list[i] : p_mask, 'P', &plan->v[n_e + i]);
  plan->nvcpu = nvcpu; plan->n_e = n_e; plan->n_p = n_p;
  return 0;
}

// The CPUs of the frequency domain with the highest (top = true) or lowest top clock among `mask`.
static uint64_t domain_by_clock(const br_cpu_topology *t, uint64_t mask, bool top)
{
  uint32_t best = 0;
  unsigned policy = ~0u;
  for (unsigned i = 0; i < t->ncpu; i++) {
    if (!(mask >> i & 1)) continue;
    uint32_t khz = t->cpu[i].max_khz;
    if (policy == ~0u || (top ? khz > best : khz < best)) { best = khz; policy = t->cpu[i].policy; }
  }
  uint64_t out = 0;
  for (unsigned i = 0; i < t->ncpu; i++) if (mask >> i & 1 && t->cpu[i].policy == policy) out |= 1ull << i;
  return out;
}

uint64_t br_cpu_thermal_mask(const br_cpu_topology *t, char type, unsigned cap)
{
  uint64_t e = 0, p = 0;
  for (unsigned i = 0; i < t->ncpu; i++) {
    if (!t->cpu[i].present) continue;
    if (t->cpu[i].type == 'E') e |= 1ull << i; else p |= 1ull << i;
  }
  if (type == 'E' && e) return e;                       // the E cores are the floor of every step
  uint64_t m = p;
  if (cap >= 1 && (p & ~domain_by_clock(t, p, true))) m = p & ~domain_by_clock(t, p, true);
  if (cap >= 2) m = domain_by_clock(t, m, false) | e;
  if (cap >= 3 && e) m = e;
  return m;
}

void br_cpu_plan_cap(br_cpu_plan *plan, const br_cpu_plan *base, const br_cpu_topology *t, unsigned cap)
{
  for (unsigned i = 0; i < base->nvcpu && i < BR_MAX_VCPUS; i++) {
    uint64_t want = base->v[i].affinity, allowed = br_cpu_thermal_mask(t, base->v[i].type, cap);
    uint64_t mask = want & allowed ? want & allowed : allowed ? allowed : want;
    __atomic_store_n(&plan->v[i].affinity, mask, __ATOMIC_RELEASE);
  }
}

int br_cpu_place(int tid, const br_cpu_plan *plan, unsigned index)
{
  if (index >= plan->nvcpu) return -EINVAL;
  cpu_set_t set;
  CPU_ZERO(&set);
  const uint64_t mask = __atomic_load_n(&plan->v[index].affinity, __ATOMIC_ACQUIRE);   // br_cpu_plan_cap may change it
  for (unsigned i = 0; i < 64 && i < CPU_SETSIZE; i++) if (mask >> i & 1) CPU_SET(i, &set);
  return sched_setaffinity(tid, sizeof set, &set) ? -errno : 0;
}

unsigned br_cpu_util_for(const br_cpu_plan *plan, unsigned index, uint32_t khz)
{
  if (index >= plan->nvcpu || !khz) return 0;
  const br_vcpu *v = &plan->v[index];
  if (!v->max_khz || khz >= v->max_khz) return v->capacity > 1024 ? 1024 : v->capacity;
  return (unsigned)((uint64_t)v->capacity * khz / v->max_khz);
}

// struct sched_attr of sched_setattr(2), as far as the utilisation clamps (Linux 5.3).
struct br_sched_attr {
  uint32_t size, sched_policy;
  uint64_t sched_flags;
  int32_t  sched_nice;
  uint32_t sched_priority;
  uint64_t sched_runtime, sched_deadline, sched_period;
  uint32_t sched_util_min, sched_util_max;
};
#define BR_SCHED_FLAG_KEEP_ALL       0x18u       // KEEP_POLICY | KEEP_PARAMS
#define BR_SCHED_FLAG_UTIL_CLAMP_MIN 0x20u

int br_cpu_request(int tid, const br_cpu_plan *plan, unsigned index, uint32_t khz)
{
  if (index >= plan->nvcpu) return -EINVAL;
#if defined(__linux__) && defined(SYS_sched_setattr)
  struct br_sched_attr attr = { .size = sizeof attr, .sched_flags = BR_SCHED_FLAG_KEEP_ALL | BR_SCHED_FLAG_UTIL_CLAMP_MIN,
                                .sched_util_min = br_cpu_util_for(plan, index, khz) };
  return syscall(SYS_sched_setattr, tid, &attr, 0) ? -errno : 0;
#else
  (void)tid; (void)khz;
  return -ENOSYS;
#endif
}

// ---- the virtual cpufreq device -----------------------------------------------------------------
// Register layout of Linux's virtual-cpufreq driver, which UniXNU-droidvm's DroidVMCpufreq.c uses
// (https://github.com/Droid-VM/UniXNU-droidvm f9121cd, package/DroidVMCpufreq.c): per vCPU window
// 0x00 current kHz, 0x04 set kHz (write), 0x08 table length, 0x0c table select, 0x10 table read,
// 0x14 performance domain.
struct br_cpufreq {
  br_cpu_plan plan;
  int (*tid_of)(unsigned vcpu);
  uint32_t select[BR_MAX_VCPUS], requested[BR_MAX_VCPUS];
};

br_cpufreq *br_cpufreq_create(const br_cpu_plan *plan, int (*tid_of)(unsigned vcpu))
{
  br_cpufreq *d = calloc(1, sizeof *d);
  if (!d) return NULL;
  d->plan = *plan;
  d->tid_of = tid_of;
  return d;
}

void br_cpufreq_destroy(br_cpufreq *d) { free(d); }

uint64_t br_cpufreq_read(br_cpufreq *d, uint64_t offset, unsigned size)
{
  unsigned cpu = (unsigned)(offset / BR_CPUFREQ_WINDOW), reg = (unsigned)(offset % BR_CPUFREQ_WINDOW);
  (void)size;
  if (cpu >= d->plan.nvcpu) return 0;
  const br_vcpu *v = &d->plan.v[cpu];
  switch (reg) {
  case 0x00: return d->requested[cpu] ? d->requested[cpu] : v->max_khz;
  case 0x08: return v->nfreq;
  case 0x0c: return d->select[cpu];
  case 0x10: return d->select[cpu] < v->nfreq ? v->freq_khz[d->select[cpu]] : 0;
  case 0x14: return v->cluster_id;
  }
  return 0;
}

void br_cpufreq_write(br_cpufreq *d, uint64_t offset, unsigned size, uint64_t value)
{
  unsigned cpu = (unsigned)(offset / BR_CPUFREQ_WINDOW), reg = (unsigned)(offset % BR_CPUFREQ_WINDOW);
  (void)size;
  if (cpu >= d->plan.nvcpu) return;
  if (reg == 0x0c) d->select[cpu] = (uint32_t)value;
  else if (reg == 0x04) {
    uint32_t khz = (uint32_t)value;
    // The lowest table entry is "no request": the governor is left alone.
    if (khz <= d->plan.v[cpu].min_khz || (d->plan.v[cpu].nfreq && khz <= d->plan.v[cpu].freq_khz[0])) khz = 0;
    if (__atomic_exchange_n(&d->requested[cpu], khz, __ATOMIC_ACQ_REL) == khz) return;
    int rc = br_cpu_request(d->tid_of ? d->tid_of(cpu) : 0, &d->plan, cpu, khz);
    if (rc && rc != -ENOSYS) br_log(2, "cpufreq: vCPU %u: uclamp for %u kHz: %s", cpu, khz, strerror(-rc));
  }
}

uint32_t br_cpufreq_requested(const br_cpufreq *d, unsigned vcpu)
{
  return vcpu < d->plan.nvcpu ? __atomic_load_n(&d->requested[vcpu], __ATOMIC_ACQUIRE) : 0;
}
