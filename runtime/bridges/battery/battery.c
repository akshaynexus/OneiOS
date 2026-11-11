// SPDX-License-Identifier: MIT
// The battery bridge (battery.h, README.md). Only reads: nothing is written to the phone, so a killed
// session leaves nothing behind. One worker thread, idle until the guest listens, SCHED_OTHER.
#define _GNU_SOURCE
#include "battery.h"
#include "bridges/bridges.h"
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <time.h>
#include <unistd.h>

// A phone never moves 20 A, so a larger number can only be microamps. Below it the number stays in the
// unit last learned: a phone that reports milliamps never crosses it, one that reports microamps does as
// soon as it draws 20 mA, and from then on the reading is sticky.
#define CURRENT_UA_MIN  20000
#define VOLTAGE_UV_MIN  100000          // 100 V
#define CHARGE_UAH_MIN  100000          // 100 Ah
#define TEMP_DECI_MIN   (-400)
#define TEMP_DECI_MAX   1500
#define CHARGE_MIN_MA   50              // below this a time to full means nothing
#define DISCHARGE_MIN_MA 25             // the same for a time to empty
#define MINUTES_MAX     (7 * 24 * 60)

// ---- pure ---------------------------------------------------------------------------------------

static int64_t abs64(int64_t v) { return v < 0 ? -v : v; }

static uint32_t mah_of(int64_t v)
{
  if (v == BR_BATTERY_ABSENT || v <= 0) return 0;
  v = v >= CHARGE_UAH_MIN ? v / 1000 : v;
  return v > UINT32_MAX ? 0 : (uint32_t)v;
}

void br_battery_map(const br_battery_raw *raw, br_battery_units *units, uint32_t default_mah, br_battery_sample *out)
{
  memset(out, 0, sizeof *out);
  int64_t percent = raw->capacity == BR_BATTERY_ABSENT ? 0 : raw->capacity;
  out->percent = percent < 0 ? 0 : percent > 100 ? 100 : (uint32_t)percent;
  out->present = raw->present == BR_BATTERY_ABSENT ? raw->capacity != BR_BATTERY_ABSENT : raw->present != 0;

  bool is_charging = !strcasecmp(raw->status, "Charging");
  bool is_full = !strcasecmp(raw->status, "Full");
  bool not_charging = !strcasecmp(raw->status, "Not charging");
  bool discharging = !strcasecmp(raw->status, "Discharging");
  out->plugged = raw->plugged || is_charging || is_full;
  out->full = is_full || (out->plugged && not_charging && out->percent >= 100);
  out->charging = is_charging;

  out->temp_deci_c = raw->temp == BR_BATTERY_ABSENT || raw->temp < TEMP_DECI_MIN || raw->temp > TEMP_DECI_MAX
                         ? BR_BATTERY_UNKNOWN : (int32_t)raw->temp;

  int64_t voltage = raw->voltage == BR_BATTERY_ABSENT || raw->voltage < 0 ? 0 : raw->voltage;
  voltage = voltage >= VOLTAGE_UV_MIN ? voltage / 1000 : voltage;
  out->voltage_mv = voltage > UINT32_MAX ? 0 : (uint32_t)voltage;

  out->current_ma = BR_BATTERY_UNKNOWN;
  if (raw->current != BR_BATTERY_ABSENT) {
    if (!units->current_fixed && abs64(raw->current) >= CURRENT_UA_MIN) units->current_ua = true;
    int64_t ma = units->current_ua ? raw->current / 1000 : raw->current;
    // The kernel's sign convention differs between drivers: the status says which way it flows.
    if (is_charging) ma = abs64(ma);
    else if (discharging) ma = -abs64(ma);
    if (ma > INT32_MIN && ma <= INT32_MAX) out->current_ma = (int32_t)ma;
  }

  out->full_mah = mah_of(raw->charge_full);
  out->design_mah = mah_of(raw->charge_full_design);
  uint32_t fallback = default_mah ? default_mah : BR_BATTERY_DEFAULT_MAH;
  if (!out->full_mah) out->full_mah = out->design_mah ? out->design_mah : fallback;
  if (!out->design_mah) out->design_mah = out->full_mah;
  out->cycles = raw->cycles == BR_BATTERY_ABSENT || raw->cycles < 0 || raw->cycles > UINT32_MAX ? 0 : (uint32_t)raw->cycles;

  out->minutes = BR_BATTERY_MINUTES_UNKNOWN;
  if (out->present && out->current_ma != BR_BATTERY_UNKNOWN) {
    uint64_t left = (uint64_t)out->full_mah * out->percent / 100, todo = out->full_mah - left, minutes = MINUTES_MAX + 1;
    if (is_charging && out->current_ma >= CHARGE_MIN_MA) minutes = todo * 60 / (uint64_t)out->current_ma;
    else if (discharging && out->current_ma <= -DISCHARGE_MIN_MA) minutes = left * 60 / (uint64_t)-(int64_t)out->current_ma;
    if (minutes <= MINUTES_MAX) out->minutes = (int32_t)minutes;
  }
}

bool br_battery_changed(const br_battery_sample *sent, const br_battery_sample *now, uint32_t ms_since_sent)
{
  if (sent->percent != now->percent || sent->present != now->present || sent->plugged != now->plugged ||
      sent->charging != now->charging || sent->full != now->full) return true;
  if (ms_since_sent < BR_BATTERY_TELEMETRY_MS) return false;
  return sent->temp_deci_c != now->temp_deci_c || sent->voltage_mv != now->voltage_mv || sent->current_ma != now->current_ma ||
         sent->full_mah != now->full_mah || sent->design_mah != now->design_mah || sent->cycles != now->cycles || sent->minutes != now->minutes;
}

// ---- sysfs --------------------------------------------------------------------------------------

static ssize_t read_text(const char *dir, const char *supply, const char *name, char *text, size_t size)
{
  char path[320];
  if (snprintf(path, sizeof path, "%s/%s/%s", dir, supply, name) >= (int)sizeof path) return -1;
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return -1;
  ssize_t n = read(fd, text, size - 1);
  close(fd);
  if (n < 0) return -1;
  while (n > 0 && (text[n - 1] == '\n' || text[n - 1] == ' ' || text[n - 1] == '\r' || text[n - 1] == '\t')) n--;
  text[n] = 0;
  return n;
}

static int64_t read_number(const char *dir, const char *supply, const char *name)
{
  char text[40], *end;
  if (read_text(dir, supply, name, text, sizeof text) <= 0) return BR_BATTERY_ABSENT;
  errno = 0;
  long long value = strtoll(text, &end, 0);
  return errno || end == text || *end ? BR_BATTERY_ABSENT : (int64_t)value;
}

int br_battery_read_raw(const char *dir, const char *const *supplies, br_battery_raw *raw)
{
  memset(raw, 0, sizeof *raw);
  raw->capacity = read_number(dir, "battery", "capacity");
  if (raw->capacity == BR_BATTERY_ABSENT) return -1;
  if (read_text(dir, "battery", "status", raw->status, sizeof raw->status) < 0) raw->status[0] = 0;
  raw->present = read_number(dir, "battery", "present");
  raw->temp = read_number(dir, "battery", "temp");
  raw->voltage = read_number(dir, "battery", "voltage_now");
  raw->current = read_number(dir, "battery", "current_now");
  raw->charge_full = read_number(dir, "battery", "charge_full");
  raw->charge_full_design = read_number(dir, "battery", "charge_full_design");
  raw->cycles = read_number(dir, "battery", "cycle_count");
  if (raw->cycles == BR_BATTERY_ABSENT) raw->cycles = read_number(dir, "battery", "battery_cycle");     // Samsung's name
  for (unsigned i = 0; supplies[i]; i++) {
    int64_t online = read_number(dir, supplies[i], "online");
    if (online != BR_BATTERY_ABSENT && online > 0) raw->plugged = true;
  }
  return 0;
}

// ---- the session --------------------------------------------------------------------------------

struct br_battery {
  char dir[200];
  char supply_text[120];
  const char *supplies[BR_BATTERY_MAX_SUPPLIES + 1];
  uint32_t poll_ms, default_mah;
  br_battery_units units;
  pthread_t thread;
  pthread_mutex_t lock;
  pthread_cond_t wake;
  bool quit, read_failed;
  br_battery_fn fn;
  void *ctx;
  br_battery_sample sent;
  uint64_t sent_ms;
};

static uint64_t now_ms(void)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (uint64_t)t.tv_sec * 1000u + (uint64_t)t.tv_nsec / 1000000u;
}

// With the lock held. Keeps the last good sample in *out when the nodes cannot be read.
static int sample_now(br_battery *b, br_battery_sample *out)
{
  br_battery_raw raw;
  if (br_battery_read_raw(b->dir, b->supplies, &raw)) {
    if (!b->read_failed) br_log(0, "battery: %s/battery/capacity cannot be read: iOS keeps the last level", b->dir);
    b->read_failed = true;
    *out = b->sent;
    return -1;
  }
  b->read_failed = false;
  br_battery_map(&raw, &b->units, b->default_mah, out);
  return 0;
}

static void *worker(void *arg)
{
  br_battery *b = arg;
  pthread_mutex_lock(&b->lock);
  while (!b->quit) {
    if (!b->fn) { pthread_cond_wait(&b->wake, &b->lock); continue; }
    struct timespec until;
    clock_gettime(CLOCK_MONOTONIC, &until);
    until.tv_sec += b->poll_ms / 1000;
    until.tv_nsec += (long)(b->poll_ms % 1000) * 1000000;
    if (until.tv_nsec >= 1000000000) { until.tv_sec++; until.tv_nsec -= 1000000000; }
    pthread_cond_timedwait(&b->wake, &b->lock, &until);
    if (b->quit || !b->fn) continue;
    br_battery_sample sample;
    if (sample_now(b, &sample)) continue;
    uint64_t now = now_ms();
    if (!br_battery_changed(&b->sent, &sample, (uint32_t)(now - b->sent_ms > UINT32_MAX ? UINT32_MAX : now - b->sent_ms))) continue;
    b->sent = sample;
    b->sent_ms = now;
    br_battery_fn fn = b->fn;
    void *ctx = b->ctx;
    pthread_mutex_unlock(&b->lock);
    fn(ctx, &sample);                 // takes the bridge's lock, which br_battery_listen's caller may hold
    pthread_mutex_lock(&b->lock);
  }
  pthread_mutex_unlock(&b->lock);
  return NULL;
}

br_battery *br_battery_open(const br_battery_options *options)
{
  br_battery *b = calloc(1, sizeof *b);
  if (!b) return NULL;
  const char *env_supplies = options ? NULL : getenv("ONEIOS_BATTERY_SUPPLIES"), *env_mah = options ? NULL : getenv("ONEIOS_BATTERY_MAH");
  const char *env_unit = options ? NULL : getenv("ONEIOS_BATTERY_CURRENT_UNIT");
  const char *dir = options && options->dir ? options->dir : "/sys/class/power_supply";
  const char *supplies = options && options->supplies ? options->supplies : env_supplies ? env_supplies : "usb,ac,wireless,pogo";
  char unit = options ? options->current_unit : env_unit ? env_unit[0] : 0;
  snprintf(b->dir, sizeof b->dir, "%s", dir);
  snprintf(b->supply_text, sizeof b->supply_text, "%s", supplies);
  b->poll_ms = options && options->poll_ms ? options->poll_ms : BR_BATTERY_POLL_MS;
  b->default_mah = options && options->default_mah ? options->default_mah : env_mah ? (uint32_t)strtoul(env_mah, NULL, 0) : 0;
  b->units.current_fixed = unit == 'u' || unit == 'm';
  b->units.current_ua = unit == 'u';
  char *save = NULL;
  unsigned n = 0;
  for (char *name = strtok_r(b->supply_text, ",", &save); name && n < BR_BATTERY_MAX_SUPPLIES; name = strtok_r(NULL, ",", &save)) b->supplies[n++] = name;

  br_battery_raw raw;
  if (br_battery_read_raw(b->dir, b->supplies, &raw)) {
    br_log(1, "battery: off: no readable %s/battery/capacity", b->dir);
    free(b);
    return NULL;
  }
  br_battery_map(&raw, &b->units, b->default_mah, &b->sent);
  b->sent_ms = now_ms();
  pthread_mutex_init(&b->lock, NULL);
  pthread_condattr_t attr;
  pthread_condattr_init(&attr);
  pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
  pthread_cond_init(&b->wake, &attr);
  pthread_condattr_destroy(&attr);
  if (pthread_create(&b->thread, NULL, worker, b)) {
    br_log(0, "battery: off: no thread");
    pthread_cond_destroy(&b->wake);
    pthread_mutex_destroy(&b->lock);
    free(b);
    return NULL;
  }
  br_log(1, "battery: on: %s, status \"%s\", %u%%, %s; read every %u ms while iOS listens", b->dir, raw.status, b->sent.percent,
         b->sent.plugged ? "plugged in" : "on battery", b->poll_ms);
  return b;
}

int br_battery_query(br_battery *b, br_battery_sample *out)
{
  pthread_mutex_lock(&b->lock);
  int rc = sample_now(b, out);
  if (!rc) { b->sent = *out; b->sent_ms = now_ms(); }
  pthread_mutex_unlock(&b->lock);
  return rc;
}

void br_battery_listen(br_battery *b, br_battery_fn fn, void *ctx)
{
  pthread_mutex_lock(&b->lock);
  b->fn = fn;
  b->ctx = ctx;
  pthread_cond_broadcast(&b->wake);
  pthread_mutex_unlock(&b->lock);
}

void br_battery_close(br_battery *b)
{
  pthread_mutex_lock(&b->lock);
  b->quit = true;
  pthread_cond_broadcast(&b->wake);
  pthread_mutex_unlock(&b->lock);
  pthread_join(b->thread, NULL);
  pthread_cond_destroy(&b->wake);
  pthread_mutex_destroy(&b->lock);
  free(b);
}
