// SPDX-License-Identifier: MIT
// thermalctl: the thermal bridge on the phone, without xnuhost (README.md, "On the phone"). Read only.
//   thermalctl                 the sensors found, one reading, the level and xnuhost's step with its CPUs
//   thermalctl watch [-i S] [-n N] [-q]
//                              a line every S seconds (10), N lines (90, 15 minutes): temperatures, level, CPU and GPU
//                              clocks, the battery's current, the phone's CPU use, xnuhost's and its session's
//   thermalctl top [-i S]      over S seconds (10): xnuhost's busiest threads, and the busiest processes of its session
//   ONEIOS_THERMAL_ROOT=DIR    a fixture instead of the phone's /sys (tests)
#include "thermal.h"
#include "bridges/bridges.h"

#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static const char *root(void) { const char *r = getenv("ONEIOS_THERMAL_ROOT"); return r ? r : ""; }

static long long read_ll(const char *fmt, const char *a, long long fallback)
{
  char path[300], text[64];
  snprintf(path, sizeof path, fmt, root(), a);
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return fallback;
  ssize_t n = read(fd, text, sizeof text - 1);
  close(fd);
  if (n <= 0) return fallback;
  text[n] = 0;
  char *end;
  long long v = strtoll(text, &end, 10);
  return end == text ? fallback : v;
}

static void deci(char *out, size_t size, int32_t v)
{
  if (v == BR_THERMAL_UNKNOWN) snprintf(out, size, "-");
  else snprintf(out, size, "%d.%d", v / 10, abs(v) % 10);
}

static void mask_text(char *out, size_t size, uint64_t m)
{
  size_t used = 0;
  out[0] = 0;
  for (unsigned i = 0; i < 64 && used + 4 < size; i++)
    if (m >> i & 1) used += (size_t)snprintf(out + used, size - used, "%s%u", used ? "," : "", i);
}

static int report(void)
{
  br_thermal_sensors s;
  if (br_thermal_find(root(), &s)) { printf("thermalctl: no sensor can be read\n"); return 1; }
  printf("zones: %u of interest\n", s.nzone);
  for (unsigned i = 0; i < s.nzone; i++) {
    long long v = read_ll("%s%s", s.zone[i].path + strlen(root()), -999999);
    printf("  %-14s %-4s %lld\n", s.zone[i].type, (const char *[]){ "cpu", "gpu", "ddr", "xo" }[s.zone[i].group], v);
  }
  br_thermal_raw raw;
  br_thermal_read(&s, &raw);
  br_thermal_limits l;
  br_thermal_limits_default(&l);
  br_thermal_state st;
  br_thermal_decide(&l, &raw, NULL, &st);
  char t[9][16];
  int32_t vals[9] = { raw.cpu, raw.gpu, raw.ddr, raw.soc, raw.xo, raw.ap, raw.battery, raw.lrp, raw.skin };
  for (unsigned i = 0; i < 9; i++) deci(t[i], sizeof t[i], vals[i]);
  printf("reading (C): cpu %s gpu %s ddr %s soc %s (%s) xo %s ap %s battery %s lrp %s; case = %s (%s)\n", t[0], t[1], t[2], t[3],
         raw.soc_zone[0] ? raw.soc_zone : "-", t[4], t[5], t[6], t[7], t[8], raw.lrp != BR_THERMAL_UNKNOWN ? "lrp" : "ap");
  printf("level: %s (%u); case rule %u, SoC rule %u, battery rule %u; xnuhost step %u\n", br_thermal_level_name(st.level), st.level, st.skin_level,
         st.soc_level, st.battery_level, st.cap);
  br_cpu_topology topo;
  char sys[200];
  snprintf(sys, sizeof sys, "%s/sys", root());
  if (br_cpu_read(sys, &topo) == 0)
    for (unsigned cap = 0; cap <= 3; cap++) {
      char e[64], p[64];
      mask_text(e, sizeof e, br_cpu_thermal_mask(&topo, 'E', cap));
      mask_text(p, sizeof p, br_cpu_thermal_mask(&topo, 'P', cap));
      printf("step %u: E vCPUs on cpu %s, P vCPUs on cpu %s\n", cap, e, p);
    }
  return 0;
}

// ---- watch --------------------------------------------------------------------------------------
static bool proc_ticks(int pid, unsigned long long *ticks)
{
  char path[64], text[1024];
  snprintf(path, sizeof path, "/proc/%d/stat", pid);
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return false;
  ssize_t n = read(fd, text, sizeof text - 1);
  close(fd);
  if (n <= 0) return false;
  text[n] = 0;
  char *p = strrchr(text, ')');
  if (!p) return false;
  unsigned long long utime = 0, stime = 0;
  // after ") ": state(3) ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt utime(14) stime(15)
  if (sscanf(p + 2, "%*c %*d %*d %*d %*d %*d %*u %*u %*u %*u %*u %llu %llu", &utime, &stime) != 2) return false;
  *ticks = utime + stime;
  return true;
}

static int find_xnuhost(void)
{
  DIR *d = opendir("/proc");
  if (!d) return 0;
  struct dirent *e;
  int found = 0;
  while ((e = readdir(d)) && !found) {
    int pid = atoi(e->d_name);
    if (pid <= 0) continue;
    char path[64], comm[32];
    snprintf(path, sizeof path, "/proc/%d/comm", pid);
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) continue;
    ssize_t n = read(fd, comm, sizeof comm - 1);
    close(fd);
    if (n > 0) { comm[n] = 0; if (!strcmp(comm, "xnuhost\n")) found = pid; }
  }
  closedir(d);
  return found;
}

// CPU ticks of every process in xnuhost's mount namespace: the whole session (iOS's processes are Linux processes there).
static unsigned long long session_ticks(int xnuhost)
{
  char path[64], ns[64], other[64];
  snprintf(path, sizeof path, "/proc/%d/ns/mnt", xnuhost);
  ssize_t n = readlink(path, ns, sizeof ns - 1);
  if (n <= 0) return 0;
  ns[n] = 0;
  DIR *d = opendir("/proc");
  if (!d) return 0;
  struct dirent *e;
  unsigned long long sum = 0;
  while ((e = readdir(d))) {
    int pid = atoi(e->d_name);
    if (pid <= 0) continue;
    snprintf(path, sizeof path, "/proc/%d/ns/mnt", pid);
    n = readlink(path, other, sizeof other - 1);
    if (n <= 0) continue;
    other[n] = 0;
    unsigned long long t;
    if (!strcmp(other, ns) && proc_ticks(pid, &t)) sum += t;
  }
  closedir(d);
  return sum;
}

static bool cpu_stat(unsigned long long *busy, unsigned long long *all)
{
  FILE *f = fopen("/proc/stat", "re");
  if (!f) return false;
  unsigned long long v[10] = { 0 };
  int n = fscanf(f, "cpu %llu %llu %llu %llu %llu %llu %llu %llu %llu %llu", &v[0], &v[1], &v[2], &v[3], &v[4], &v[5], &v[6], &v[7], &v[8], &v[9]);
  fclose(f);
  if (n < 5) return false;
  *all = 0;
  for (int i = 0; i < 8; i++) *all += v[i];      // guest and guest_nice are in user already
  *busy = *all - v[3] - v[4];
  return true;
}

static int watch(int argc, char **argv)
{
  unsigned interval = 10, count = 90;
  bool quiet = false;
  for (int i = 2; i < argc; i++) {
    if (!strcmp(argv[i], "-i") && i + 1 < argc) interval = (unsigned)atoi(argv[++i]);
    else if (!strcmp(argv[i], "-n") && i + 1 < argc) count = (unsigned)atoi(argv[++i]);
    else if (!strcmp(argv[i], "-q")) quiet = true;
    else { fprintf(stderr, "usage: thermalctl watch [-i SECONDS] [-n LINES] [-q]\n"); return 2; }
  }
  if (!interval) interval = 1;
  br_thermal_sensors s;
  if (br_thermal_find(root(), &s)) { printf("thermalctl: no sensor can be read\n"); return 1; }
  br_thermal_limits l;
  br_thermal_limits_default(&l);
  br_thermal_state st, prev;
  bool have_prev = false;
  long hz = sysconf(_SC_CLK_TCK);
  if (hz <= 0) hz = 100;
  if (!quiet)
    printf("# time cpu gpu ddr xo ap battery lrp (C) level step | MHz cpu0 cpu2 cpu5 cpu7 (cpu7 max) gpu gpu%% | battery mA mV | CPU%% phone xnuhost session\n");
  unsigned long long busy0 = 0, all0 = 0, x0 = 0, s0 = 0;
  int xpid = find_xnuhost();
  cpu_stat(&busy0, &all0);
  if (xpid) { proc_ticks(xpid, &x0); s0 = session_ticks(xpid); }
  struct timespec t0, t1;
  clock_gettime(CLOCK_MONOTONIC, &t0);
  for (unsigned k = 0; k < count; k++) {
    sleep(interval);
    clock_gettime(CLOCK_MONOTONIC, &t1);
    double span = (double)(t1.tv_sec - t0.tv_sec) + (double)(t1.tv_nsec - t0.tv_nsec) / 1e9;   // the reads take time too
    t0 = t1;
    if (span <= 0) span = interval;
    br_thermal_raw raw;
    br_thermal_read(&s, &raw);
    br_thermal_decide(&l, &raw, have_prev ? &prev : NULL, &st);
    prev = st;
    have_prev = true;
    unsigned long long busy, all, x = 0, se = 0;
    double phone = -1, xh = -1, ses = -1;
    if (cpu_stat(&busy, &all) && all > all0) phone = 100.0 * 8 * (double)(busy - busy0) / (double)(all - all0);
    busy0 = busy; all0 = all;
    int now_pid = find_xnuhost();
    if (now_pid && now_pid == xpid && proc_ticks(xpid, &x)) {
      se = session_ticks(xpid);
      xh = 100.0 * (double)(x - x0) / (double)hz / span;
      ses = se >= s0 ? 100.0 * (double)(se - s0) / (double)hz / span : -1;
    } else if (now_pid) { proc_ticks(now_pid, &x); se = session_ticks(now_pid); }
    xpid = now_pid; x0 = x; s0 = se;
    char t[7][16];
    int32_t vals[7] = { raw.cpu, raw.gpu, raw.ddr, raw.xo, raw.ap, raw.battery, raw.lrp };
    for (unsigned i = 0; i < 7; i++) deci(t[i], sizeof t[i], vals[i]);
    time_t now = time(NULL);
    struct tm tm;
    localtime_r(&now, &tm);
    printf("%02d:%02d:%02d %s %s %s %s %s %s %s %s %u | %lld %lld %lld %lld (%lld) %lld %lld | %lld %lld | ", tm.tm_hour, tm.tm_min, tm.tm_sec,
           t[0], t[1], t[2], t[3], t[4], t[5], t[6], br_thermal_level_name(st.level), st.cap,
           read_ll("%s/sys/devices/system/cpu/cpufreq/policy%s/scaling_cur_freq", "0", 0) / 1000,
           read_ll("%s/sys/devices/system/cpu/cpufreq/policy%s/scaling_cur_freq", "2", 0) / 1000,
           read_ll("%s/sys/devices/system/cpu/cpufreq/policy%s/scaling_cur_freq", "5", 0) / 1000,
           read_ll("%s/sys/devices/system/cpu/cpufreq/policy%s/scaling_cur_freq", "7", 0) / 1000,
           read_ll("%s/sys/devices/system/cpu/cpufreq/policy%s/scaling_max_freq", "7", 0) / 1000,
           read_ll("%s/sys/class/kgsl/kgsl-3d0/%s", "gpuclk", 0) / 1000000, read_ll("%s/sys/class/kgsl/kgsl-3d0/%s", "gpu_busy_percentage", -1),
           read_ll("%s/sys/class/power_supply/battery/%s", "current_now", 0), read_ll("%s/sys/class/power_supply/battery/%s", "voltage_now", 0) / 1000);
    if (phone >= 0) printf("%.0f ", phone); else printf("- ");
    if (xh >= 0) printf("%.0f %.0f\n", xh, ses); else printf("- -\n");
    fflush(stdout);
  }
  return 0;
}

// ---- top ----------------------------------------------------------------------------------------
typedef struct { int id; char name[24]; unsigned long long ticks; } usage;

static unsigned snapshot(const char *dir, int self_ns_pid, usage *out, unsigned max)
{
  char ns[64] = "", path[300], other[64];
  if (self_ns_pid) {
    snprintf(path, sizeof path, "/proc/%d/ns/mnt", self_ns_pid);
    ssize_t n = readlink(path, ns, sizeof ns - 1);
    if (n <= 0) return 0;
    ns[n] = 0;
  }
  DIR *d = opendir(dir);
  if (!d) return 0;
  struct dirent *e;
  unsigned count = 0;
  while ((e = readdir(d)) && count < max) {
    int id = atoi(e->d_name);
    if (id <= 0) continue;
    if (self_ns_pid) {
      snprintf(path, sizeof path, "%s/%d/ns/mnt", dir, id);
      ssize_t n = readlink(path, other, sizeof other - 1);
      if (n <= 0) continue;
      other[n] = 0;
      if (strcmp(other, ns)) continue;
    }
    char text[1024];
    snprintf(path, sizeof path, "%s/%d/stat", dir, id);
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) continue;
    ssize_t n = read(fd, text, sizeof text - 1);
    close(fd);
    if (n <= 0) continue;
    text[n] = 0;
    char *open_paren = strchr(text, '('), *close_paren = strrchr(text, ')');
    unsigned long long ut, st;
    if (!open_paren || !close_paren || sscanf(close_paren + 2, "%*c %*d %*d %*d %*d %*d %*u %*u %*u %*u %*u %llu %llu", &ut, &st) != 2) continue;
    usage *u = &out[count++];
    u->id = id;
    snprintf(u->name, sizeof u->name, "%.*s", (int)(close_paren - open_paren - 1 < 23 ? close_paren - open_paren - 1 : 23), open_paren + 1);
    u->ticks = ut + st;
  }
  closedir(d);
  return count;
}

static int by_ticks(const void *a, const void *b)
{
  const usage *x = a, *y = b;
  return x->ticks < y->ticks ? 1 : x->ticks > y->ticks ? -1 : 0;
}

// The busiest of `now` against `then`, by id, as a percentage of one CPU over `span` seconds.
static void busiest(const char *title, usage *then, unsigned nthen, usage *now, unsigned nnow, double span, long hz, unsigned shown)
{
  for (unsigned i = 0; i < nnow; i++) {
    unsigned long long before = now[i].ticks;          // new in the interval: all of it
    for (unsigned j = 0; j < nthen; j++) if (then[j].id == now[i].id) { before = then[j].ticks; break; }
    now[i].ticks = now[i].ticks >= before ? now[i].ticks - before : 0;
  }
  qsort(now, nnow, sizeof now[0], by_ticks);
  double all = 0;
  for (unsigned i = 0; i < nnow; i++) all += (double)now[i].ticks;
  printf("%s: %.0f%% in all\n", title, 100.0 * all / (double)hz / span);
  for (unsigned i = 0; i < nnow && i < shown && now[i].ticks; i++)
    printf("  %6d %-24s %5.1f%%\n", now[i].id, now[i].name, 100.0 * (double)now[i].ticks / (double)hz / span);
}

static int top(int argc, char **argv)
{
  unsigned interval = 10;
  for (int i = 2; i < argc; i++) {
    if (!strcmp(argv[i], "-i") && i + 1 < argc) interval = (unsigned)atoi(argv[++i]);
    else { fprintf(stderr, "usage: thermalctl top [-i SECONDS]\n"); return 2; }
  }
  int x = find_xnuhost();
  if (!x) { printf("thermalctl: no xnuhost running\n"); return 1; }
  static usage t0[512], t1[512], p0[2048], p1[2048];
  char tasks[64];
  snprintf(tasks, sizeof tasks, "/proc/%d/task", x);
  unsigned nt0 = snapshot(tasks, 0, t0, 512), np0 = snapshot("/proc", x, p0, 2048);
  struct timespec a, b;
  clock_gettime(CLOCK_MONOTONIC, &a);
  sleep(interval ? interval : 1);
  unsigned nt1 = snapshot(tasks, 0, t1, 512), np1 = snapshot("/proc", x, p1, 2048);
  clock_gettime(CLOCK_MONOTONIC, &b);
  double span = (double)(b.tv_sec - a.tv_sec) + (double)(b.tv_nsec - a.tv_nsec) / 1e9;
  long hz = sysconf(_SC_CLK_TCK);
  if (hz <= 0) hz = 100;
  char title[64];
  snprintf(title, sizeof title, "xnuhost %d, its threads (%.1f s)", x, span);
  busiest(title, t0, nt0, t1, nt1, span, hz, 16);
  busiest("the session's processes (xnuhost's mount namespace)", p0, np0, p1, np1, span, hz, 12);
  return 0;
}

int main(int argc, char **argv)
{
  if (argc == 1) return report();
  if (!strcmp(argv[1], "watch")) return watch(argc, argv);
  if (!strcmp(argv[1], "top")) return top(argc, argv);
  fprintf(stderr, "usage: thermalctl [watch [-i SECONDS] [-n LINES] [-q] | top [-i SECONDS]]\n");
  return 2;
}
