// SPDX-License-Identifier: MIT
// What one trip of an iOS thread through the kernel costs, step by step (docs/booter.md 2.6a). Counters and
// histograms that each vCPU thread keeps for itself without locks (task.c and cpu.c add to them), merged and
// printed by the main thread: every perf-log seconds (default 30; 0 = never on its own), at the stop and on SIGQUIT.
//   perf: 30.0 s: 112340 task traps (3745/s): syscall 80000, fault 20000, interrupt 9000, other 3340; ...
//   perf:   up        n ... mean ... p50 ... p90 ... p99 ... max ... (microseconds)
//   perf: cpu0 E: 56% of a core; of its wall time 31% kernel, 40% an iOS thread's turn, 29% idle; 3700 traps/s
// A step is a histogram of nanoseconds with four buckets per power of two, so a quantile is good to 19%.
#define _GNU_SOURCE
#include "xnuhost.h"
#include <stdio.h>
#include <string.h>
#include <time.h>

static xh_perf shared_perf;               // for host calls outside a vCPU thread (none today)
static int perf_every = 30;
static uint64_t last_report;              // counter
static xh_perf last[XH_MAX_CPUS];         // the previous report's values, for the deltas
static uint64_t last_cpu_ns[XH_MAX_CPUS];
static uint64_t start_counter;

static unsigned bucket_of(uint64_t ns)
{
  if (ns < 4) return (unsigned)ns;
  unsigned top = 63u - (unsigned)__builtin_clzll(ns);        // >= 2
  unsigned b = top * 4 + (unsigned)((ns >> (top - 2)) & 3);  // 2 mantissa bits
  return b - 4 < XH_HIST_BUCKETS ? b - 4 : XH_HIST_BUCKETS - 1;
}

static uint64_t bucket_top(unsigned b)    // the largest ns that falls in bucket b
{
  if (b < 4) return b;
  unsigned top = (b + 4) / 4, m = (b + 4) % 4;
  return ((4ull + m + 1) << (top - 2)) - 1;
}

void xh_hist_add(xh_hist *h, uint64_t ns)
{
  h->n++;
  h->sum_ns += ns;
  if (ns > h->max_ns) h->max_ns = ns;
  h->b[bucket_of(ns)]++;
}

uint64_t xh_hist_quantile(const xh_hist *h, double q)
{
  if (!h->n) return 0;
  uint64_t want = (uint64_t)(q * (double)h->n), seen = 0;
  for (unsigned b = 0; b < XH_HIST_BUCKETS; b++) {
    seen += h->b[b];
    if (seen > want) { uint64_t top = bucket_top(b); return top < h->max_ns ? top : h->max_ns; }
  }
  return h->max_ns;
}

uint64_t xh_ticks_ns(uint64_t ticks)
{
  uint64_t hz = g_info.timebase_hz ? g_info.timebase_hz : xh_counter_hz();
  return ticks / hz * 1000000000ull + ticks % hz * 1000000000ull / hz;
}

xh_perf *xh_perf_self(void)
{
  xh_vcpu *v = xh_cpu_self();
  return v ? &v->perf : &shared_perf;
}

void xh_perf_init(const xh_config *c)
{
  perf_every = c->perf_log < 0 ? 30 : c->perf_log;
  start_counter = last_report = xh_counter();
}

static void hist_delta(xh_hist *out, const xh_hist *now, const xh_hist *then)
{
  out->n += now->n - (then ? then->n : 0);
  out->sum_ns += now->sum_ns - (then ? then->sum_ns : 0);
  if (now->max_ns > out->max_ns) out->max_ns = now->max_ns;      // the maximum is since the start
  for (unsigned b = 0; b < XH_HIST_BUCKETS; b++) out->b[b] += now->b[b] - (then ? then->b[b] : 0);
}

static void hist_line(const char *name, const xh_hist *h)
{
  if (!h->n) return;
  LOGI("perf:   %-18s n %8llu  mean %7.2f  p50 %7.2f  p90 %7.2f  p99 %8.2f  max %9.1f us", name, (unsigned long long)h->n,
       (double)h->sum_ns / (double)h->n / 1000.0, (double)xh_hist_quantile(h, 0.5) / 1000.0, (double)xh_hist_quantile(h, 0.9) / 1000.0,
       (double)xh_hist_quantile(h, 0.99) / 1000.0, (double)h->max_ns / 1000.0);
}

static uint64_t thread_cpu_ns(const xh_vcpu *v)
{
#if defined(__linux__)
  clockid_t id;
  struct timespec ts;
  if (!v->started || pthread_getcpuclockid(v->thread, &id) || clock_gettime(id, &ts)) return 0;
  return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
#else
  (void)v;
  return 0;
#endif
}

void xh_perf_log(bool since_start)
{
  static const char *const names[XP_COUNT] = { "up (trap->vCPU)", "down (vCPU->task)", "kernel: syscall", "kernel: fault",
                                               "kernel: interrupt", "kernel: other", "order: map", "order: unmap",
                                               "order: protect", "order: clone", "maps by release", "kick" };
  uint64_t now = xh_counter(), wall = now - (since_start ? start_counter : last_report);
  double seconds = (double)xh_ticks_ns(wall) / 1e9;
  if (seconds <= 0) return;
  static xh_perf sum;                     // main thread only
  memset(&sum, 0, sizeof sum);
  for (unsigned i = 0; i < g_ncpu; i++) {
    const xh_perf *p = &g_vcpu[i].perf, *then = since_start ? NULL : &last[i];
    for (unsigned k = 0; k < XP_COUNT; k++) hist_delta(&sum.h[k], &p->h[k], then ? &then->h[k] : NULL);
    for (unsigned k = 0; k < 16; k++) sum.traps[k] += p->traps[k] - (then ? then->traps[k] : 0);
#define D(f) sum.f += p->f - (then ? then->f : 0)
    D(host_sleeps); D(task_sleeps); D(host_wakes); D(task_wakes); D(maps_deferred); D(maps_ordered); D(timer_sets); D(timer_calls);
#undef D
  }
  uint64_t traps = 0;
  for (unsigned k = 0; k < 16; k++) traps += sum.traps[k];
  uint64_t other = traps - sum.traps[XH_TRAP_SYSCALL] - sum.traps[XH_TRAP_FAULT] - sum.traps[XH_TRAP_INTERRUPT];
  LOGI("perf: %.1f s%s: %llu task traps (%.0f/s): syscall %llu, fault %llu, interrupt %llu, other %llu; futex sleeps: vCPU %llu, "
       "iOS thread %llu (of %llu hand-offs each way); futex wakes %llu + %llu; task_map %llu carried by a release, %llu ordered; "
       "timer_set %llu (%llu to the host timer)", seconds, since_start ? " since the start" : "", (unsigned long long)traps, (double)traps / seconds,
       (unsigned long long)sum.traps[XH_TRAP_SYSCALL], (unsigned long long)sum.traps[XH_TRAP_FAULT], (unsigned long long)sum.traps[XH_TRAP_INTERRUPT],
       (unsigned long long)other, (unsigned long long)sum.host_sleeps, (unsigned long long)sum.task_sleeps, (unsigned long long)sum.h[XP_UP].n,
       (unsigned long long)sum.host_wakes, (unsigned long long)sum.task_wakes, (unsigned long long)sum.maps_deferred,
       (unsigned long long)sum.maps_ordered, (unsigned long long)sum.timer_sets, (unsigned long long)sum.timer_calls);
  for (unsigned k = 0; k < XP_COUNT; k++) hist_line(names[k], &sum.h[k]);
  for (unsigned i = 0; i < g_ncpu; i++) {
    xh_vcpu *v = &g_vcpu[i];
    const xh_perf *p = &v->perf, *then = since_start ? NULL : &last[i];
    uint64_t cpu = thread_cpu_ns(v), cpu_then = since_start ? 0 : last_cpu_ns[i];
    uint64_t user = p->ticks_user - (then ? then->ticks_user : 0), idle = p->ticks_idle - (then ? then->ticks_idle : 0), t = 0;
    for (unsigned k = 0; k < 16; k++) t += p->traps[k] - (then ? then->traps[k] : 0);
    double w = (double)wall, kern = w - (double)user - (double)idle;
    if (v->started && w > 0)
      LOGI("perf: cpu%u: %.0f%% of a core; of its wall time %.0f%% kernel, %.0f%% an iOS thread's turn, %.0f%% idle; %.0f traps/s", i,
           cpu >= cpu_then ? (double)(cpu - cpu_then) / 1e7 / seconds : 0.0, kern > 0 ? 100.0 * kern / w : 0.0,
           100.0 * (double)user / w, 100.0 * (double)idle / w, (double)t / seconds);
    if (!since_start) { last[i] = *p; last_cpu_ns[i] = cpu; }
  }
  if (!since_start) last_report = now;
}

void xh_perf_tick(void)
{
  if (perf_every <= 0) return;
  if (xh_ticks_ns(xh_counter() - last_report) >= (uint64_t)perf_every * 1000000000ull) xh_perf_log(false);
}
