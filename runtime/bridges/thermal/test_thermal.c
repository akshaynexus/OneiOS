// SPDX-License-Identifier: MIT
// Host tests of the thermal bridge: the limits parser, the rules and their hysteresis, the step's CPUs on the S24
// Ultra's topology, the sysfs reader on a fake tree, and the worker's two listeners.
#include "thermal.h"
#include "bridges/bridges.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static int checks, failed;
#define CHECK(cond) do { checks++; if (!(cond)) { failed++; fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

static char base[256];

static void put(const char *rel, const char *text)
{
  char path[512], dir[512];
  snprintf(path, sizeof path, "%s%s", base, rel);
  snprintf(dir, sizeof dir, "%s", path);
  for (char *p = dir + strlen(base) + 1; *p; p++)
    if (*p == '/') { *p = 0; mkdir(dir, 0755); *p = '/'; }
  FILE *f = fopen(path, "w");
  if (!f) { perror(path); exit(1); }
  fputs(text, f);
  fclose(f);
}

static void zone(unsigned n, const char *type, long milli)
{
  char rel[128], text[32];
  snprintf(rel, sizeof rel, "/sys/class/thermal/thermal_zone%u/type", n);
  snprintf(text, sizeof text, "%s\n", type);
  put(rel, text);
  snprintf(rel, sizeof rel, "/sys/class/thermal/thermal_zone%u/temp", n);
  snprintf(text, sizeof text, "%ld\n", milli);
  put(rel, text);
}

static br_thermal_raw raw_with(int32_t skin, int32_t soc, int32_t battery)
{
  br_thermal_raw r;
  memset(&r, 0, sizeof r);
  r.cpu = r.gpu = r.ddr = r.xo = r.ap = r.lrp = BR_THERMAL_UNKNOWN;
  r.skin = skin; r.soc = soc; r.battery = battery;
  return r;
}

static void test_parse(void)
{
  int32_t v[5] = { 0 };
  CHECK(br_thermal_limits_parse("39,42,44,46,50", v, 5) == 0 && v[0] == 390 && v[4] == 500);
  CHECK(br_thermal_limits_parse("38.5, 41,43,45,49\n", v, 5) == 0 && v[0] == 385 && v[1] == 410);
  CHECK(br_thermal_limits_parse("44,42,46,48,50", v, 5) == -1 && v[0] == 385);       // not ascending: unchanged
  CHECK(br_thermal_limits_parse("39,42", v, 5) == -1);
  CHECK(br_thermal_limits_parse("90,100,110", v, 2) == -1);
  CHECK(br_thermal_limits_parse("x", v, 2) == -1);
  int32_t w[2];
  CHECK(br_thermal_limits_parse("90,100", w, 2) == 0 && w[0] == 900 && w[1] == 1000);
}

static void test_rules(void)
{
  br_thermal_limits l;
  br_thermal_limits_default(&l);
  br_thermal_state s, prev;
  br_thermal_raw r = raw_with(300, 500, 300);
  br_thermal_decide(&l, &r, NULL, &s);
  CHECK(s.level == BR_THERMAL_NOMINAL && s.cap == 0 && s.reason == 0);
  r = raw_with(395, 500, 300);
  br_thermal_decide(&l, &r, &s, &prev);
  CHECK(prev.level == BR_THERMAL_LIGHT && prev.reason == BR_THERMAL_BY_SKIN && prev.cap == 0);
  r = raw_with(445, 500, 300);
  br_thermal_decide(&l, &r, &prev, &s);
  CHECK(s.level == BR_THERMAL_HEAVY && s.cap == 1);
  // hysteresis: 43.0 C keeps HEAVY (on at 44, off below 42), 41.9 C drops to MODERATE (whose own threshold is 42, held to 40)
  r = raw_with(430, 500, 300);
  br_thermal_decide(&l, &r, &s, &prev);
  CHECK(prev.level == BR_THERMAL_HEAVY);
  r = raw_with(419, 500, 300);
  br_thermal_decide(&l, &r, &prev, &s);
  CHECK(s.level == BR_THERMAL_MODERATE && s.cap == 0);
  // rising again needs the full threshold: 43.9 C is not HEAVY from MODERATE
  r = raw_with(439, 500, 300);
  br_thermal_decide(&l, &r, &s, &prev);
  CHECK(prev.level == BR_THERMAL_MODERATE);
  // the SoC rule alone
  r = raw_with(350, 955, 300);
  br_thermal_decide(&l, &r, &prev, &s);
  CHECK(s.level == BR_THERMAL_HEAVY && s.reason == BR_THERMAL_BY_SOC && s.skin_level == BR_THERMAL_NOMINAL);
  r = raw_with(350, 1055, 300);
  br_thermal_decide(&l, &r, &s, &prev);
  CHECK(prev.level == BR_THERMAL_TRAPPING && prev.cap == 2);
  r = raw_with(350, 1010, 300);
  br_thermal_decide(&l, &r, &prev, &s);
  CHECK(s.level == BR_THERMAL_TRAPPING);                   // held to 100 C
  r = raw_with(350, 990, 300);
  br_thermal_decide(&l, &r, &s, &prev);
  CHECK(prev.level == BR_THERMAL_HEAVY);                   // below 100, above 90
  // two rules at one level: both named
  r = raw_with(445, 950, 300);
  br_thermal_decide(&l, &r, &prev, &s);
  CHECK(s.level == BR_THERMAL_HEAVY && s.reason == (BR_THERMAL_BY_SKIN | BR_THERMAL_BY_SOC));
  // the battery
  r = raw_with(350, 500, 505);
  br_thermal_decide(&l, &r, NULL, &s);
  CHECK(s.level == BR_THERMAL_TRAPPING && s.reason == BR_THERMAL_BY_BATTERY);
  // a missing reading keeps its rule's level
  r = raw_with(BR_THERMAL_UNKNOWN, BR_THERMAL_UNKNOWN, 300);
  br_thermal_decide(&l, &r, &prev, &s);
  CHECK(s.skin_level == prev.skin_level && s.soc_level == prev.soc_level);
  br_thermal_decide(&l, &r, NULL, &s);
  CHECK(s.level == BR_THERMAL_NOMINAL);
  // sleeping, and the steps
  r = raw_with(505, 500, 300);
  br_thermal_decide(&l, &r, NULL, &s);
  CHECK(s.level == BR_THERMAL_SLEEPING && s.cap == 3);
  CHECK(br_thermal_cap_for(BR_THERMAL_MODERATE) == 0 && br_thermal_cap_for(BR_THERMAL_HEAVY) == 1 && br_thermal_cap_for(BR_THERMAL_TRAPPING) == 2);
  CHECK(!strcmp(br_thermal_level_name(BR_THERMAL_TRAPPING), "trapping") && !strcmp(br_thermal_level_name(7), "?"));
  // what is worth a message
  br_thermal_state a = s, b = s;
  CHECK(!br_thermal_changed(&a, &b, 1000));
  CHECK(br_thermal_changed(&a, &b, BR_THERMAL_REPORT_MS));
  b.level = BR_THERMAL_HEAVY;
  CHECK(br_thermal_changed(&a, &b, 0));
}

// The S24 Ultra: cpu0-1 A520 (policy0, 2.27 GHz), cpu2-4 A720 (policy2, 3.15), cpu5-6 A720 (policy5, 2.96), cpu7 X4 (policy7, 3.40).
static void s24_topology(br_cpu_topology *t)
{
  memset(t, 0, sizeof *t);
  static const unsigned policy[8] = { 0, 0, 2, 2, 2, 5, 5, 7 }, cap[8] = { 325, 325, 870, 870, 870, 870, 870, 1024 };
  static const uint32_t khz[8] = { 2265600, 2265600, 3148800, 3148800, 3148800, 2956800, 2956800, 3398400 };
  t->ncpu = 8;
  for (unsigned i = 0; i < 8; i++) {
    t->cpu[i].present = true; t->cpu[i].policy = policy[i]; t->cpu[i].capacity = cap[i]; t->cpu[i].max_khz = khz[i];
    t->cpu[i].type = i < 2 ? 'E' : 'P';
  }
  t->n_e = 2; t->n_p = 6;
}

static void test_masks(void)
{
  br_cpu_topology t;
  s24_topology(&t);
  CHECK(br_cpu_thermal_mask(&t, 'P', 0) == 0xfc && br_cpu_thermal_mask(&t, 'E', 0) == 0x03);
  CHECK(br_cpu_thermal_mask(&t, 'P', 1) == 0x7c);          // without the X4
  CHECK(br_cpu_thermal_mask(&t, 'P', 2) == 0x63);          // cpu5-6 (the slowest top clock) and the E cores
  CHECK(br_cpu_thermal_mask(&t, 'P', 3) == 0x03);
  CHECK(br_cpu_thermal_mask(&t, 'E', 3) == 0x03);
  br_cpu_plan base, plan;
  CHECK(br_cpu_plan_make(&t, 8, false, &base) == 0);
  plan = base;
  br_cpu_plan_cap(&plan, &base, &t, 1);
  CHECK(plan.v[0].affinity == 0x03 && plan.v[2].affinity == 0x7c && plan.v[7].affinity == 0x7c);
  // step 2: the P vCPUs on cpu5-6 and the E cores (they lost the E cores before 2026-11-13: 0x60), the E vCPUs on theirs
  br_cpu_plan_cap(&plan, &base, &t, 2);
  CHECK(plan.v[0].affinity == 0x03 && plan.v[1].affinity == 0x03 && plan.v[2].affinity == 0x63 && plan.v[7].affinity == 0x63);
  br_cpu_plan_cap(&plan, &base, &t, 3);
  CHECK(plan.v[0].affinity == 0x03 && plan.v[7].affinity == 0x03);
  // cpu-pin: one core each while the kind's vCPUs fit the mask, else the whole mask (never three to a core)
  br_cpu_plan_cap(&plan, &base, &t, 0);
  CHECK(plan.v[7].affinity == 0xfc);
  CHECK(br_cpu_pin_mask(&plan, &base, 0) == 0x01 && br_cpu_pin_mask(&plan, &base, 1) == 0x02);
  CHECK(br_cpu_pin_mask(&plan, &base, 2) == 0x04 && br_cpu_pin_mask(&plan, &base, 7) == 0x80);
  uint64_t seen = 0;
  for (unsigned i = 2; i < 8; i++) seen |= br_cpu_pin_mask(&plan, &base, i);
  CHECK(seen == 0xfc);                                      // six P vCPUs, six P cores, each its own
  br_cpu_plan_cap(&plan, &base, &t, 1);                       // five P cores for six P vCPUs
  CHECK(br_cpu_pin_mask(&plan, &base, 2) == 0x7c && br_cpu_pin_mask(&plan, &base, 0) == 0x01);
  br_cpu_plan_cap(&plan, &base, &t, 2);
  CHECK(br_cpu_pin_mask(&plan, &base, 7) == 0x63 && br_cpu_pin_mask(&plan, &base, 1) == 0x02);
  CHECK(br_cpu_pin_mask(&plan, &base, 8) == 0);
  br_cpu_plan_cap(&plan, &base, &t, 0);
  // pinned: each vCPU's own core when the step allows it, the step's CPUs when it does not
  CHECK(br_cpu_plan_make(&t, 8, true, &base) == 0);
  plan = base;
  br_cpu_plan_cap(&plan, &base, &t, 1);
  CHECK(base.v[2].affinity == 0x80 && plan.v[2].affinity == 0x7c);       // the X4's vCPU moves to the other P cores
  CHECK(plan.v[3].affinity == base.v[3].affinity);
  // one kind of core: the steps still leave CPUs
  br_cpu_topology same;
  memset(&same, 0, sizeof same);
  same.ncpu = 4;
  for (unsigned i = 0; i < 4; i++) { same.cpu[i].present = true; same.cpu[i].policy = 0; same.cpu[i].max_khz = 2000000; same.cpu[i].type = 'P'; }
  same.n_p = 4;
  CHECK(br_cpu_thermal_mask(&same, 'P', 1) == 0x0f && br_cpu_thermal_mask(&same, 'P', 3) == 0x0f);
}

static void test_sysfs(void)
{
  zone(0, "aoss-0", 30000);
  zone(1, "cpu-1-2-1", 61500);
  zone(2, "cpuss-0", 40000);
  zone(3, "gpuss-0", 55000);
  zone(4, "ddr", 45000);
  zone(5, "xo-therm", 33204);
  zone(6, "epm0", -273000);
  zone(7, "cpu-0-0-0", -273000);            // an off sensor is not the hottest
  put("/sys/class/sec/sec-ap-thermistor/temperature", "362\n");
  put("/sys/class/power_supply/battery/temp", "331\n");
  br_thermal_sensors s;
  CHECK(br_thermal_find(base, &s) == 0);
  CHECK(s.nzone == 6);
  br_thermal_raw r;
  br_thermal_read(&s, &r);
  CHECK(r.cpu == 615 && r.gpu == 550 && r.ddr == 450 && r.xo == 332 && r.soc == 615 && !strcmp(r.soc_zone, "cpu-1-2-1"));
  CHECK(r.ap == 362 && r.battery == 331 && r.lrp == BR_THERMAL_UNKNOWN && r.skin == 362);      // no lrp: the board thermistor
  put("/sys/class/power_supply/battery/lrp", "401\n");
  br_thermal_read(&s, &r);
  CHECK(r.lrp == 401 && r.skin == 401);
  zone(3, "gpuss-0", 71000);
  br_thermal_read(&s, &r);
  CHECK(r.soc == 710 && !strcmp(r.soc_zone, "gpuss-0"));
  // nothing at all
  char empty[300];
  snprintf(empty, sizeof empty, "%s/none", base);
  mkdir(empty, 0755);
  CHECK(br_thermal_find(empty, &s) == -1);
}

static struct { int caps, reports; unsigned last_cap, last_level; } seen;
static void on_cap(void *ctx, const br_thermal_state *st) { (void)ctx; __atomic_fetch_add(&seen.caps, 1, __ATOMIC_RELAXED); seen.last_cap = st->cap; }
static void on_report(void *ctx, const br_thermal_state *st) { (void)ctx; __atomic_fetch_add(&seen.reports, 1, __ATOMIC_RELAXED); seen.last_level = st->level; }

static void wait_ms(unsigned ms) { struct timespec t = { ms / 1000, (long)(ms % 1000) * 1000000 }; nanosleep(&t, NULL); }

static void test_worker(void)
{
  put("/sys/class/power_supply/battery/lrp", "350\n");
  br_thermal_limits l;
  br_thermal_limits_default(&l);
  br_thermal_options o = { .root = base, .poll_ms = 20, .limits = &l };
  br_thermal *t = br_thermal_open(&o);
  CHECK(t != NULL);
  if (!t) return;
  br_thermal_state st;
  br_thermal_get(t, &st);
  CHECK(st.level == BR_THERMAL_NOMINAL && st.raw.skin == 350);
  br_thermal_on_cap(t, on_cap, NULL);
  br_thermal_on_report(t, on_report, NULL);
  wait_ms(150);
  CHECK(seen.caps == 0 && seen.reports == 0);            // nothing changed: no message before BR_THERMAL_REPORT_MS
  put("/sys/class/power_supply/battery/lrp", "447\n");
  wait_ms(200);
  CHECK(seen.caps == 1 && seen.last_cap == 1);
  CHECK(seen.reports == 1 && seen.last_level == BR_THERMAL_HEAVY);
  put("/sys/class/power_supply/battery/lrp", "462\n");
  wait_ms(200);
  CHECK(seen.caps == 2 && seen.last_cap == 2 && seen.last_level == BR_THERMAL_TRAPPING);
  put("/sys/class/power_supply/battery/lrp", "300\n");
  wait_ms(200);
  CHECK(seen.caps == 3 && seen.last_cap == 0 && seen.last_level == BR_THERMAL_NOMINAL);
  br_thermal_on_cap(t, NULL, NULL);
  br_thermal_on_report(t, NULL, NULL);
  put("/sys/class/power_supply/battery/lrp", "470\n");
  wait_ms(150);
  CHECK(seen.caps == 3 && seen.reports == 3);
  br_thermal_get(t, &st);
  CHECK(st.level == BR_THERMAL_TRAPPING);                 // still read with no listener: the next listener hears it
  br_thermal_on_cap(t, on_cap, NULL);
  wait_ms(150);
  CHECK(seen.caps == 4 && seen.last_cap == 2);
  br_thermal_close(t);
}

// The load line from fake /proc and battery files: two samples 30 s apart on an 8-CPU host.
static char last_line[512];
static void keep_line(int level, const char *line) { (void)level; if (!strncmp(line, "thermal: ", 9) && strstr(line, "host CPU")) snprintf(last_line, sizeof last_line, "%s", line); }

static void stat_files(unsigned long long user, unsigned long long system, unsigned long long idle, unsigned long long irq,
                       unsigned long long swpin, unsigned long long swpout, unsigned long long self)
{
  char text[1024];
  snprintf(text, sizeof text, "cpu  %llu 0 %llu %llu 0 %llu 0 0 0 0\n", user, system, idle, irq);
  size_t at = strlen(text);
  for (unsigned i = 0; i < 8; i++) at += (size_t)snprintf(text + at, sizeof text - at, "cpu%u 1 2 3 4 5 6 7 0 0 0\n", i);
  snprintf(text + at, sizeof text - at, "intr 1 2 3\nctxt 99\n");
  put("/proc/stat", text);
  snprintf(text, sizeof text, "nr_free_pages 10\npswpin %llu\npswpout %llu\npgfault 7\n", swpin, swpout);
  put("/proc/vmstat", text);
  // comm with a space and a parenthesis: the fields are counted after its last ')'
  snprintf(text, sizeof text, "4242 (xnu host) S 1 4242 4242 0 -1 4194560 100 0 0 0 %llu %llu 0 0 20 0 30 0 1 2 3\n", self / 2, self - self / 2);
  put("/proc/self/stat", text);
}

static void test_load(void)
{
  // 30 s at 100 ticks a second on 8 CPUs: 24000 ticks. sys 6780 (226%), user 5010 (167%), irq 360 (12%), the rest idle.
  stat_files(1000, 2000, 50000, 100, 500, 400, 700);
  put("/sys/class/power_supply/battery/current_now", "1600\n");
  put("/sys/class/power_supply/battery/status", "Charging\n");
  br_host_load a, b;
  br_host_load_read(base, &a);
  CHECK(a.have_cpu && a.ncpu == 8 && a.user == 1000 && a.system == 2000 && a.idle == 50000 && a.irq == 100);
  CHECK(a.have_swap && a.pswpin == 500 && a.pswpout == 400 && a.have_self && a.self_ticks == 700);
  CHECK(a.have_current && a.current == 1600 && !strcmp(a.status, "Charging"));
  stat_files(1000 + 5010, 2000 + 6780, 50000 + 11850, 100 + 360, 500 + 70350, 400 + 59400, 700 + 4560);
  br_host_load_read(base, &b);
  a.ms = 1000; b.ms = 31000;
  br_thermal_raw raw = raw_with(410, 780, 403);
  snprintf(raw.soc_zone, sizeof raw.soc_zone, "cpu-1-2-1");
  char line[400];
  br_host_load_line(&a, &b, &raw, 4096, line, sizeof line);
  // swap: 70350 pages of 4 KiB in 30 s = 9380 KiB/s, 59400 = 7920 KiB/s; xnuhost 4560 of 3000 ticks of one CPU = 152%
  CHECK(!strcmp(line, "30 s: host CPU 405% of 800 (sys 226%, user 167%, irq 12%), swap in 9380 out 7920 KiB/s, xnuhost 152%; "
                      "battery 40.3 C, Charging 1600 mA; case 41.0 C, SoC 78.0 C (cpu-1-2-1)"));
  if (strncmp(line, "30 s: host CPU 405%", 19)) fprintf(stderr, "  line: %s\n", line);
  // Missing files: each part says "-", nothing breaks; a short buffer stays terminated.
  br_host_load none;
  memset(&none, 0, sizeof none);
  none.ms = 31000;
  br_host_load_line(&a, &none, NULL, 4096, line, sizeof line);
  CHECK(!strcmp(line, "30 s: host CPU -, swap -, xnuhost -; battery -, -; case -, SoC - (-)"));
  CHECK(br_host_load_line(&a, &b, &raw, 4096, line, 12) == 11 && strlen(line) == 11);
  // The worker writes the line at its period.
  put("/sys/class/power_supply/battery/lrp", "350\n");
  br_log_sink = keep_line;
  br_thermal_options o = { .root = base, .poll_ms = 20, .load_ms = 50 };
  br_thermal *t = br_thermal_open(&o);
  CHECK(t != NULL);
  if (t) {
    wait_ms(250);
    br_thermal_close(t);
  }
  CHECK(strstr(last_line, "thermal: ") == last_line && strstr(last_line, "host CPU ") && strstr(last_line, "; case 35.0 C"));
  br_log_sink = NULL;
}

static void quiet(int level, const char *line) { (void)level; (void)line; }

int main(void)
{
  br_log_sink = quiet;
  snprintf(base, sizeof base, "/tmp/test_thermal.%d", (int)getpid());
  if (mkdir(base, 0755) && errno != EEXIST) { perror(base); return 1; }
  test_parse();
  test_rules();
  test_masks();
  test_sysfs();
  test_worker();
  test_load();
  char cmd[300];
  snprintf(cmd, sizeof cmd, "rm -rf %s", base);
  if (system(cmd)) fprintf(stderr, "could not remove %s\n", base);
  printf("thermal: %d checks, %d failed\n", checks, failed);
  return failed ? 1 : 0;
}
