// SPDX-License-Identifier: MIT
// The thermal bridge (thermal.h, README.md): reads the phone's sensors, decides a pressure level, and tells xnuhost
// (its own CPU step) and the guest (drivers/thermal). Read only; nothing outside the process is changed.
#include "thermal.h"
#include "bridges/bridges.h"

#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

// ---- pure ---------------------------------------------------------------------------------------
void br_thermal_limits_default(br_thermal_limits *l)
{
  // The case (Samsung's lrp sensor): its charger already limits current from 37-39 C with the screen on and acts
  // again at 46 C (the phone's device tree, battery,temp_table_LRP_* and battery,mix_v2_lrp_cond 460). Light from
  // 39, serious (HEAVY) from 44, critical (TRAPPING) from 46, SLEEPING from 50: a guess on that scale, not Samsung's
  // own policy, which is not readable (it lived in system_server).
  static const int32_t skin[5] = { 390, 420, 440, 460, 500 };
  memcpy(l->skin, skin, sizeof skin);
  l->skin_hyst = 20;
  // The SoC: the X4's first trip is 90 C (cpufreq-cpu7), the GPU's 95 C, the cores pause at 108-110 C (the phone's
  // thermal zones). HEAVY at 95 C, TRAPPING at 105 C: the kernel acts first, iOS is told. Not lower: a boot's first minute
  // reaches 94.6 C on the cores for some seconds (the phone, 2026-11-12), and a step then would only slow the boot.
  l->soc[0] = 950; l->soc[1] = 1050; l->soc_hyst = 50;
  // The cell: charging stops near 50 C on Samsung phones [inference]; HEAVY at 45 C, TRAPPING at 50 C.
  l->battery[0] = 450; l->battery[1] = 500; l->battery_hyst = 20;
}

int br_thermal_limits_parse(const char *text, int32_t *out, unsigned count)
{
  int32_t v[8];
  unsigned n = 0;
  if (!text || count > 8) return -1;
  const char *p = text;
  while (n < count) {
    char *end;
    double d = strtod(p, &end);
    if (end == p || d < -50 || d > 200) return -1;
    v[n++] = (int32_t)(d * 10 + (d < 0 ? -0.5 : 0.5));
    p = end;
    while (*p == ' ') p++;
    if (n < count && *p++ != ',') return -1;
  }
  while (*p == ' ' || *p == '\n') p++;
  if (*p) return -1;
  for (unsigned k = 1; k < count; k++) if (v[k] <= v[k - 1]) return -1;     // ascending, or the hysteresis has no meaning
  memcpy(out, v, count * sizeof v[0]);
  return 0;
}

// The level of one rule: the highest whose threshold the value reaches; a level at or below the previous one is
// kept down to `hyst` below its threshold. A missing reading keeps the previous level.
static unsigned rule(int32_t value, const int32_t *on, const unsigned *levels, unsigned n, unsigned prev, int32_t hyst)
{
  if (value == BR_THERMAL_UNKNOWN) return prev;
  unsigned out = BR_THERMAL_NOMINAL;
  for (unsigned k = 0; k < n; k++)
    if (value >= on[k] - (levels[k] <= prev ? hyst : 0)) out = levels[k];
  return out;
}

void br_thermal_decide(const br_thermal_limits *l, const br_thermal_raw *raw, const br_thermal_state *prev, br_thermal_state *out)
{
  static const unsigned skin_levels[5] = { BR_THERMAL_LIGHT, BR_THERMAL_MODERATE, BR_THERMAL_HEAVY, BR_THERMAL_TRAPPING, BR_THERMAL_SLEEPING };
  static const unsigned two_levels[2] = { BR_THERMAL_HEAVY, BR_THERMAL_TRAPPING };
  br_thermal_state s;
  memset(&s, 0, sizeof s);
  s.raw = *raw;
  s.skin_level = rule(raw->skin, l->skin, skin_levels, 5, prev ? prev->skin_level : 0, l->skin_hyst);
  s.soc_level = rule(raw->soc, l->soc, two_levels, 2, prev ? prev->soc_level : 0, l->soc_hyst);
  s.battery_level = rule(raw->battery, l->battery, two_levels, 2, prev ? prev->battery_level : 0, l->battery_hyst);
  s.level = s.skin_level;
  if (s.soc_level > s.level) s.level = s.soc_level;
  if (s.battery_level > s.level) s.level = s.battery_level;
  if (s.level) {
    if (s.skin_level == s.level) s.reason |= BR_THERMAL_BY_SKIN;
    if (s.soc_level == s.level) s.reason |= BR_THERMAL_BY_SOC;
    if (s.battery_level == s.level) s.reason |= BR_THERMAL_BY_BATTERY;
  }
  s.cap = br_thermal_cap_for(s.level);
  *out = s;
}

unsigned br_thermal_cap_for(unsigned level)
{
  return level >= BR_THERMAL_SLEEPING ? 3 : level >= BR_THERMAL_TRAPPING ? 2 : level >= BR_THERMAL_HEAVY ? 1 : 0;
}

const char *br_thermal_level_name(unsigned level)
{
  switch (level) {
  case BR_THERMAL_NOMINAL: return "nominal";
  case BR_THERMAL_LIGHT: return "light";
  case BR_THERMAL_MODERATE: return "moderate";
  case BR_THERMAL_HEAVY: return "heavy";
  case BR_THERMAL_TRAPPING: return "trapping";
  case BR_THERMAL_SLEEPING: return "sleeping";
  }
  return "?";
}

bool br_thermal_changed(const br_thermal_state *sent, const br_thermal_state *now, uint32_t ms_since_sent)
{
  return sent->level != now->level || sent->cap != now->cap || ms_since_sent >= BR_THERMAL_REPORT_MS;
}

// ---- sysfs --------------------------------------------------------------------------------------
static bool read_line(const char *path, char *out, size_t size)
{
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return false;
  ssize_t n = read(fd, out, size - 1);
  close(fd);
  if (n <= 0) return false;
  out[n] = 0;
  while (n > 0 && (out[n - 1] == '\n' || out[n - 1] == ' ')) out[--n] = 0;
  return true;
}

static bool read_int(const char *path, long long *out)
{
  char text[32], *end;
  if (!read_line(path, text, sizeof text)) return false;
  errno = 0;
  long long v = strtoll(text, &end, 10);
  if (end == text || errno) return false;
  *out = v;
  return true;
}

// Tenths of a degree from a node in tenths; implausible values (a sensor that is off reads -273 C here) are missing.
static int32_t read_deci(const char *root, const char *rel)
{
  char path[256];
  long long v;
  snprintf(path, sizeof path, "%s%s", root, rel);
  if (!read_int(path, &v) || v < -400 || v > 2000) return BR_THERMAL_UNKNOWN;
  return (int32_t)v;
}

static int group_of(const char *type)
{
  if (!strncmp(type, "cpu-", 4) || !strncmp(type, "cpuss-", 6)) return 0;
  if (!strncmp(type, "gpuss-", 6)) return 1;
  if (!strcmp(type, "ddr")) return 2;
  if (!strcmp(type, "xo-therm")) return 3;
  return -1;
}

int br_thermal_find(const char *root, br_thermal_sensors *s)
{
  memset(s, 0, sizeof *s);
  snprintf(s->root, sizeof s->root, "%.*s", (int)sizeof s->root - 1, root ? root : "");
  char dir[200];
  snprintf(dir, sizeof dir, "%s/sys/class/thermal", s->root);
  DIR *d = opendir(dir);
  if (d) {
    struct dirent *e;
    while ((e = readdir(d)) && s->nzone < BR_THERMAL_MAX_ZONES) {
      if (strncmp(e->d_name, "thermal_zone", 12) || !isdigit((unsigned char)e->d_name[12])) continue;
      char path[400], type[64];
      snprintf(path, sizeof path, "%s/%.40s/type", dir, e->d_name);
      if (!read_line(path, type, sizeof type)) continue;
      int g = group_of(type);
      if (g < 0) continue;
      br_thermal_zone *z = &s->zone[s->nzone];
      if (snprintf(z->path, sizeof z->path, "%.110s/%.40s/temp", dir, e->d_name) >= (int)sizeof z->path) continue;
      snprintf(z->type, sizeof z->type, "%.*s", (int)sizeof z->type - 1, type);
      z->group = (uint8_t)g;
      s->nzone++;
    }
    closedir(d);
  }
  br_thermal_raw raw;
  br_thermal_read(s, &raw);
  bool any = raw.soc != BR_THERMAL_UNKNOWN || raw.ap != BR_THERMAL_UNKNOWN || raw.battery != BR_THERMAL_UNKNOWN || raw.lrp != BR_THERMAL_UNKNOWN;
  return any ? 0 : -1;
}

void br_thermal_read(const br_thermal_sensors *s, br_thermal_raw *raw)
{
  int32_t best[4] = { BR_THERMAL_UNKNOWN, BR_THERMAL_UNKNOWN, BR_THERMAL_UNKNOWN, BR_THERMAL_UNKNOWN };
  const char *name[4] = { "", "", "", "" };
  for (unsigned i = 0; i < s->nzone; i++) {
    long long v;
    // millidegrees; a zone that is off reads -273000, a broken one huge numbers
    if (!read_int(s->zone[i].path, &v) || v < -40000 || v > 200000) continue;
    int32_t deci = (int32_t)(v / 100);
    unsigned g = s->zone[i].group;
    if (best[g] == BR_THERMAL_UNKNOWN || deci > best[g]) { best[g] = deci; name[g] = s->zone[i].type; }
  }
  memset(raw, 0, sizeof *raw);
  raw->cpu = best[0]; raw->gpu = best[1]; raw->ddr = best[2]; raw->xo = best[3];
  raw->soc = BR_THERMAL_UNKNOWN;
  for (unsigned g = 0; g < 3; g++)
    if (best[g] != BR_THERMAL_UNKNOWN && (raw->soc == BR_THERMAL_UNKNOWN || best[g] > raw->soc)) {
      raw->soc = best[g];
      snprintf(raw->soc_zone, sizeof raw->soc_zone, "%.*s", (int)sizeof raw->soc_zone - 1, name[g]);
    }
  raw->ap = read_deci(s->root, "/sys/class/sec/sec-ap-thermistor/temperature");
  raw->battery = read_deci(s->root, "/sys/class/power_supply/battery/temp");
  raw->lrp = read_deci(s->root, "/sys/class/power_supply/battery/lrp");
  raw->skin = raw->lrp != BR_THERMAL_UNKNOWN ? raw->lrp : raw->ap;
}

// ---- the session --------------------------------------------------------------------------------
struct br_thermal {
  br_thermal_sensors sensors;
  br_thermal_limits limits;
  uint32_t poll_ms;
  bool cap_off;
  pthread_t thread;
  pthread_mutex_t lock;
  pthread_cond_t wake;
  bool quit;
  br_thermal_fn cap_fn, report_fn;
  void *cap_ctx, *report_ctx;
  br_thermal_state now, sent;
  uint64_t sent_ms;
  unsigned cap_told;
};

static uint64_t now_ms(void)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (uint64_t)t.tv_sec * 1000u + (uint64_t)t.tv_nsec / 1000000u;
}

static void deci_text(char *out, size_t size, int32_t v)
{
  if (v == BR_THERMAL_UNKNOWN) snprintf(out, size, "-");
  else snprintf(out, size, "%s%d.%d C", v < 0 ? "-" : "", abs(v) / 10, abs(v) % 10);
}

static void say_level(const br_thermal_state *s, const br_thermal_state *was)
{
  char skin[16], soc[16], battery[16];
  deci_text(skin, sizeof skin, s->raw.skin);
  deci_text(soc, sizeof soc, s->raw.soc);
  deci_text(battery, sizeof battery, s->raw.battery);
  br_log(1, "thermal: %s (was %s; by%s%s%s): case %s, SoC %s (%s), battery %s", br_thermal_level_name(s->level), br_thermal_level_name(was->level),
         s->reason & BR_THERMAL_BY_SKIN ? " case" : "", s->reason & BR_THERMAL_BY_SOC ? " SoC" : "", s->reason & BR_THERMAL_BY_BATTERY ? " battery" : "",
         skin, soc, s->raw.soc_zone[0] ? s->raw.soc_zone : "-", battery);
}

// With the lock held.
static void step(br_thermal *t)
{
  br_thermal_raw raw;
  br_thermal_read(&t->sensors, &raw);
  br_thermal_state next;
  br_thermal_decide(&t->limits, &raw, &t->now, &next);
  if (t->cap_off) next.cap = 0;
  if (next.level != t->now.level) say_level(&next, &t->now);
  t->now = next;
}

static void *worker(void *arg)
{
  br_thermal *t = arg;
  pthread_mutex_lock(&t->lock);
  while (!t->quit) {
    struct timespec until;
    clock_gettime(CLOCK_MONOTONIC, &until);
    until.tv_sec += t->poll_ms / 1000;
    until.tv_nsec += (long)(t->poll_ms % 1000) * 1000000;
    if (until.tv_nsec >= 1000000000) { until.tv_sec++; until.tv_nsec -= 1000000000; }
    pthread_cond_timedwait(&t->wake, &t->lock, &until);
    if (t->quit) break;
    step(t);
    br_thermal_state state = t->now;
    br_thermal_fn cap_fn = t->cap_told != state.cap ? t->cap_fn : NULL, report_fn = NULL;
    void *cap_ctx = t->cap_ctx, *report_ctx = t->report_ctx;
    if (cap_fn) t->cap_told = state.cap;
    uint64_t now = now_ms();
    if (t->report_fn && br_thermal_changed(&t->sent, &state, (uint32_t)(now - t->sent_ms > UINT32_MAX ? UINT32_MAX : now - t->sent_ms))) {
      report_fn = t->report_fn;
      t->sent = state;
      t->sent_ms = now;
    }
    if (!cap_fn && !report_fn) continue;
    pthread_mutex_unlock(&t->lock);
    if (cap_fn) cap_fn(cap_ctx, &state);
    if (report_fn) report_fn(report_ctx, &state);       // takes the bridge's lock
    pthread_mutex_lock(&t->lock);
  }
  pthread_mutex_unlock(&t->lock);
  return NULL;
}

static void limits_from_env(br_thermal_limits *l)
{
  static const struct { const char *name; size_t offset; unsigned count; } env[] = {
    { "ONEIOS_THERMAL_SKIN", offsetof(br_thermal_limits, skin), 5 },
    { "ONEIOS_THERMAL_SOC", offsetof(br_thermal_limits, soc), 2 },
    { "ONEIOS_THERMAL_BATTERY", offsetof(br_thermal_limits, battery), 2 },
  };
  for (unsigned i = 0; i < sizeof env / sizeof env[0]; i++) {
    const char *text = getenv(env[i].name);
    if (!text) continue;
    if (br_thermal_limits_parse(text, (int32_t *)((char *)l + env[i].offset), env[i].count))
      br_log(0, "thermal: %s=\"%s\" is not %u ascending degrees (\"39,42,44,46,50\"): the defaults stay", env[i].name, text, env[i].count);
  }
}

br_thermal *br_thermal_open(const br_thermal_options *options)
{
  br_thermal *t = calloc(1, sizeof *t);
  if (!t) return NULL;
  if (br_thermal_find(options ? options->root : NULL, &t->sensors)) {
    br_log(1, "thermal: off: no thermal zone, board thermistor or battery temperature can be read");
    free(t);
    return NULL;
  }
  if (options && options->limits) t->limits = *options->limits;
  else { br_thermal_limits_default(&t->limits); if (!options) limits_from_env(&t->limits); }
  const char *cap = options ? NULL : getenv("ONEIOS_THERMAL_CAP");
  t->cap_off = cap && !strcmp(cap, "0");
  t->poll_ms = options && options->poll_ms ? options->poll_ms : BR_THERMAL_POLL_MS;
  br_thermal_raw raw;
  br_thermal_read(&t->sensors, &raw);
  br_thermal_decide(&t->limits, &raw, NULL, &t->now);
  if (t->cap_off) t->now.cap = 0;
  t->sent = t->now;
  t->sent_ms = now_ms();
  pthread_mutex_init(&t->lock, NULL);
  pthread_condattr_t attr;
  pthread_condattr_init(&attr);
  pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
  pthread_cond_init(&t->wake, &attr);
  pthread_condattr_destroy(&attr);
  if (pthread_create(&t->thread, NULL, worker, t)) {
    br_log(0, "thermal: off: no thread");
    pthread_cond_destroy(&t->wake);
    pthread_mutex_destroy(&t->lock);
    free(t);
    return NULL;
  }
  char skin[16], soc[16], battery[16];
  deci_text(skin, sizeof skin, raw.skin);
  deci_text(soc, sizeof soc, raw.soc);
  deci_text(battery, sizeof battery, raw.battery);
  br_log(1, "thermal: on: %u zones; case %s (%s), SoC %s (%s), battery %s; %s; read every %u ms%s", t->sensors.nzone, skin,
         raw.lrp != BR_THERMAL_UNKNOWN ? "lrp" : "ap thermistor", soc, raw.soc_zone[0] ? raw.soc_zone : "-", battery,
         br_thermal_level_name(t->now.level), t->poll_ms, t->cap_off ? "; ONEIOS_THERMAL_CAP=0: xnuhost does not hold itself back" : "");
  return t;
}

void br_thermal_get(br_thermal *t, br_thermal_state *out)
{
  pthread_mutex_lock(&t->lock);
  *out = t->now;
  pthread_mutex_unlock(&t->lock);
}

void br_thermal_on_cap(br_thermal *t, br_thermal_fn fn, void *ctx)
{
  pthread_mutex_lock(&t->lock);
  t->cap_fn = fn;
  t->cap_ctx = ctx;
  t->cap_told = 0;                          // a new listener hears the current cap at the next read when it is not 0
  pthread_cond_broadcast(&t->wake);
  pthread_mutex_unlock(&t->lock);
}

void br_thermal_on_report(br_thermal *t, br_thermal_fn fn, void *ctx)
{
  pthread_mutex_lock(&t->lock);
  t->report_fn = fn;
  t->report_ctx = ctx;
  t->sent = t->now;                         // the caller sends the current state itself (br_thermal_get)
  t->sent_ms = now_ms();
  pthread_mutex_unlock(&t->lock);
}

void br_thermal_close(br_thermal *t)
{
  pthread_mutex_lock(&t->lock);
  t->quit = true;
  pthread_cond_broadcast(&t->wake);
  pthread_mutex_unlock(&t->lock);
  pthread_join(t->thread, NULL);
  pthread_cond_destroy(&t->wake);
  pthread_mutex_destroy(&t->lock);
  free(t);
}
