// SPDX-License-Identifier: MIT
// The brightness bridge (brightness.h, README.md).
#define _GNU_SOURCE
#include "brightness.h"
#include "bridges/bridges.h"
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#ifdef __linux__
#include <linux/input.h>
#include <sys/ioctl.h>
#else
#define EV_SYN 0
#define EV_REL 2
#define EV_ABS 3
#define SYN_REPORT 0
#define REL_X 0
#define REL_RX 3
#define REL_RY 4
#define ABS_Y 1
#define ABS_Z 2
#define ABS_RX 3
#endif

#define STEP_MS 16u
#define ALS_STALE_MS 3000u                 // no report for this long: the sensor was turned off under us

// ---- the curve ----------------------------------------------------------------------------------

uint32_t br_bright_ceiling(uint32_t max_brightness)
{
  return max_brightness == 510 || max_brightness == 51000 ? max_brightness / 2 : max_brightness;
}

uint32_t br_bright_map(uint32_t level, bool linear, uint32_t floor, uint32_t ceiling)
{
  if (level > BR_BRIGHT_FULL) level = BR_BRIGHT_FULL;
  if (ceiling <= floor) return ceiling;
  uint64_t span = ceiling - floor, x = level;
  uint64_t part = linear ? (span * x + BR_BRIGHT_FULL / 2) / BR_BRIGHT_FULL
                         : (span * x * x + (uint64_t)BR_BRIGHT_FULL * BR_BRIGHT_FULL / 2) / ((uint64_t)BR_BRIGHT_FULL * BR_BRIGHT_FULL);
  return floor + (uint32_t)part;
}

uint32_t br_bright_fade_at(uint32_t from, uint32_t to, uint32_t elapsed_ms, uint32_t fade_ms)
{
  if (!fade_ms || elapsed_ms >= fade_ms) return to;
  int64_t delta = (int64_t)to - (int64_t)from;
  return (uint32_t)((int64_t)from + delta * (int64_t)elapsed_ms / (int64_t)fade_ms);
}

// The level whose squared map is nearest to a node value: where a first fade starts from.
static uint32_t level_of(uint32_t value, uint32_t floor, uint32_t ceiling)
{
  if (ceiling <= floor || value <= floor) return 0;
  if (value >= ceiling) return BR_BRIGHT_FULL;
  uint64_t square = (uint64_t)(value - floor) * BR_BRIGHT_FULL * BR_BRIGHT_FULL / (ceiling - floor), root = 0;
  for (uint64_t bit = 1ull << 16; bit; bit >>= 1)
    if ((root + bit) * (root + bit) <= square) root += bit;
  return (uint32_t)root;
}

// ---- the light sensor's events ------------------------------------------------------------------

static uint32_t minus_one(int32_t value) { return value > 0 ? (uint32_t)value - 1 : 0; }

bool br_als_feed(br_als_frame *f, uint16_t type, uint16_t code, int32_t value)
{
  if (type == EV_REL) {
    if (code == REL_RY) { f->clear = minus_one(value); f->have_clear = true; }
    else if (code == REL_X) f->ir = minus_one(value);
    else if (code == REL_RX) f->uv = minus_one(value);
  } else if (type == EV_ABS) {
    if (code == ABS_Y) f->clear_gain = minus_one(value);
    else if (code == ABS_Z) f->ir_gain = minus_one(value);
    else if (code == ABS_RX) f->uv_gain = minus_one(value);
  } else if (type == EV_SYN && code == SYN_REPORT) {
    bool whole = f->have_clear;
    f->have_clear = false;
    return whole;
  }
  return false;
}

uint32_t br_als_millilux(const br_als_frame *f, uint32_t millilux_per_count)
{
  uint64_t v = (uint64_t)f->clear * millilux_per_count;
  return v > UINT32_MAX ? UINT32_MAX : (uint32_t)v;
}

bool br_als_changed(uint32_t last, uint32_t now, uint32_t ms_since)
{
  if (ms_since >= 2000) return true;
  if (ms_since < 200) return false;
  uint32_t delta = last > now ? last - now : now - last;
  uint32_t tenth = last / 10;
  return delta >= (tenth > 1000 ? tenth : 1000);
}

// ---- the session --------------------------------------------------------------------------------

struct br_bright {
  br_bright_info info;
  uint32_t als_scale;
  // panel
  int fd;
  pthread_t panel_thread;
  pthread_mutex_t lock;
  pthread_cond_t wake;
  bool stop, pending, linear;
  uint32_t from, target, fade_ms;         // level scale
  uint32_t level;                         // where the fade is now
  uint64_t started_ms;
  uint32_t written;                       // the node's value written last
  // light sensor
  int als_fd;
  char als_enable[260];
  bool als_enabled_here;
  pthread_t als_thread;
  bool als_running;
  br_als_fn fn;
  void *fn_ctx;
};

static uint64_t now_ms(void)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (uint64_t)t.tv_sec * 1000u + (uint64_t)t.tv_nsec / 1000000u;
}

static int read_u32(const char *path, uint32_t *out)
{
  char text[32];
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return -1;
  ssize_t n = read(fd, text, sizeof text - 1);
  close(fd);
  if (n <= 0) return -1;
  text[n] = 0;
  char *end;
  unsigned long v = strtoul(text, &end, 10);
  if (end == text) return -1;
  *out = (uint32_t)v;
  return 0;
}

static int write_text(const char *path, const char *text)
{
  int fd = open(path, O_WRONLY | O_CLOEXEC);
  if (fd < 0) return -1;
  ssize_t n = write(fd, text, strlen(text));
  close(fd);
  return n < 0 ? -1 : 0;
}

static void panel_write(br_bright *b, uint32_t value)
{
  char text[16];
  int n = snprintf(text, sizeof text, "%u\n", value);
  if (value == __atomic_load_n(&b->written, __ATOMIC_RELAXED)) return;
  if (pwrite(b->fd, text, (size_t)n, 0) == n) __atomic_store_n(&b->written, value, __ATOMIC_RELAXED);
}

// Samsung's panel first ("panel"), then Qualcomm's name, then whatever is there.
static int panel_find(const char *dir, char *node, size_t size, uint32_t *max)
{
  static const char * const known[] = { "panel", "panel0-backlight" };
  char path[300];
  for (unsigned i = 0; i < 2; i++) {
    snprintf(path, sizeof path, "%s/%s/max_brightness", dir, known[i]);
    if (read_u32(path, max) == 0 && *max) { snprintf(node, size, "%.120s/%.60s/brightness", dir, known[i]); return 0; }
  }
  DIR *d = opendir(dir);
  if (!d) return -1;
  struct dirent *e;
  int found = -1;
  while (found && (e = readdir(d))) {
    if (e->d_name[0] == '.') continue;
    snprintf(path, sizeof path, "%s/%.100s/max_brightness", dir, e->d_name);
    if (read_u32(path, max) == 0 && *max) { snprintf(node, size, "%.120s/%.60s/brightness", dir, e->d_name); found = 0; }
  }
  closedir(d);
  return found;
}

static void *panel_worker(void *arg)
{
  br_bright *b = arg;
  pthread_mutex_lock(&b->lock);
  while (!b->stop) {
    if (!b->pending) { pthread_cond_wait(&b->wake, &b->lock); continue; }
    uint64_t elapsed = now_ms() - b->started_ms;
    b->level = br_bright_fade_at(b->from, b->target, elapsed > UINT32_MAX ? UINT32_MAX : (uint32_t)elapsed, b->fade_ms);
    if (b->level == b->target) b->pending = false;
    uint32_t value = br_bright_map(b->level, b->linear, b->info.floor, b->info.ceiling);
    bool more = b->pending;
    pthread_mutex_unlock(&b->lock);
    panel_write(b, value);                 // outside the lock: br_bright_set never waits for the panel
    if (more) usleep(STEP_MS * 1000u);
    pthread_mutex_lock(&b->lock);
  }
  pthread_mutex_unlock(&b->lock);
  return NULL;
}

#ifdef __linux__
static int als_find(const char *dir, const char *name, char *node, size_t size, char *event, size_t event_size)
{
  DIR *d = opendir(dir);
  if (!d) return -1;
  struct dirent *e;
  int fd = -1;
  while (fd < 0 && (e = readdir(d))) {
    if (strncmp(e->d_name, "event", 5)) continue;
    char path[190], got[80] = "";
    snprintf(path, sizeof path, "%.120s/%.60s", dir, e->d_name);
    int candidate = open(path, O_RDONLY | O_NONBLOCK | O_CLOEXEC);      // read only, and never grabbed
    if (candidate < 0) continue;
    if (ioctl(candidate, EVIOCGNAME(sizeof got), got) >= 0 && !strcmp(got, name)) {
      fd = candidate;
      snprintf(node, size, "%.190s", path);
      snprintf(event, event_size, "%.60s", e->d_name);
    } else close(candidate);
  }
  closedir(d);
  return fd;
}

static void als_enable(br_bright *b)
{
  if (b->als_enable[0] && write_text(b->als_enable, "1\n") == 0) b->als_enabled_here = true;
}

static void *als_worker(void *arg)
{
  br_bright *b = arg;
  br_als_frame frame = { 0 };
  uint32_t last_sent = 0;
  uint64_t sent_at = 0, heard_at = now_ms(), enabled_at = heard_at;
  bool sent = false;
  for (;;) {
    pthread_mutex_lock(&b->lock);
    bool stop = b->stop;
    br_als_fn fn = b->fn;
    void *ctx = b->fn_ctx;
    pthread_mutex_unlock(&b->lock);
    if (stop) break;
    struct pollfd p = { .fd = b->als_fd, .events = POLLIN };
    int ready = poll(&p, 1, 200);
    uint64_t now = now_ms();
    if (ready > 0 && (p.revents & POLLIN)) {
      struct input_event ev[32];
      ssize_t n = read(b->als_fd, ev, sizeof ev);
      for (ssize_t i = 0; n > 0 && i < n / (ssize_t)sizeof ev[0]; i++) {
        if (!br_als_feed(&frame, ev[i].type, ev[i].code, ev[i].value)) continue;
        heard_at = now;
        uint32_t millilux = br_als_millilux(&frame, b->als_scale);
        if (fn && (!sent || br_als_changed(last_sent, millilux, (uint32_t)(now - sent_at)))) {
          br_als_sample sample = { millilux, frame.clear, frame.ir, frame.uv };
          fn(ctx, &sample);
          last_sent = millilux; sent_at = now; sent = true;
        }
      }
    } else if (ready > 0 && (p.revents & (POLLERR | POLLHUP | POLLNVAL))) {
      br_log(0, "brightness: the light sensor's device went away");
      break;
    }
    // The camera HAL turns the same sensor on and off (its flicker detection): turn it on again.
    if (now - heard_at > ALS_STALE_MS && now - enabled_at > ALS_STALE_MS) { als_enable(b); enabled_at = now; }
  }
  return NULL;
}

static void als_open(br_bright *b, const br_bright_options *o)
{
  char event[64] = "";
  const char *dir = o->input_dir ? o->input_dir : "/dev/input", *name = o->als_name ? o->als_name : "als_rear";
  b->als_fd = als_find(dir, name, b->info.als_node, sizeof b->info.als_node, event, sizeof event);
  if (b->als_fd < 0) { br_log(1, "brightness: no light sensor: no input device named %s under %s (auto-brightness has no input)", name, dir); return; }
  uint32_t enabled = 0;
  char path[260];
  snprintf(path, sizeof path, "%s/%s/device/enable", o->input_sysfs ? o->input_sysfs : "/sys/class/input", event);
  if (access(path, W_OK) == 0) {
    snprintf(b->als_enable, sizeof b->als_enable, "%s", path);
    if (read_u32(path, &enabled) || !enabled) als_enable(b);
  }
  if (pthread_create(&b->als_thread, NULL, als_worker, b)) { close(b->als_fd); b->als_fd = -1; return; }
  b->als_running = true;
  b->info.als = true;
  br_log(1, "brightness: light sensor on: %s (%s, not grabbed), %u millilux per clear count%s", b->info.als_node, name, b->als_scale,
         b->als_enabled_here ? ", turned on here" : b->als_enable[0] ? ", was on" : ", no enable file");
}
#else
static void als_open(br_bright *b, const br_bright_options *o) { (void)b; (void)o; }
#endif

static uint32_t env_u32(const char *name, uint32_t fallback)
{
  const char *text = getenv(name);
  return text && *text ? (uint32_t)strtoul(text, NULL, 10) : fallback;
}

br_bright *br_bright_open(const br_bright_options *options)
{
  br_bright_options o = { 0 };
  if (options) o = *options;
  else {
    o.ceiling = env_u32("ONEIOS_BRIGHT_MAX", 0);
    o.floor = env_u32("ONEIOS_BRIGHT_MIN", 0);
    o.max_nits = env_u32("ONEIOS_BRIGHT_NITS", 0);
    o.als_millilux_per_count = env_u32("ONEIOS_ALS_SCALE", 0);
  }
  br_bright *b = calloc(1, sizeof *b);
  if (!b) return NULL;
  b->fd = b->als_fd = -1;
  b->als_scale = o.als_millilux_per_count ? o.als_millilux_per_count : 1000;
  b->info.max_nits = o.max_nits ? o.max_nits : 500;
  pthread_mutex_init(&b->lock, NULL);
  pthread_cond_init(&b->wake, NULL);

  const char *dir = o.backlight_dir ? o.backlight_dir : "/sys/class/backlight";
  if (panel_find(dir, b->info.node, sizeof b->info.node, &b->info.max_brightness) == 0 &&
      read_u32(b->info.node, &b->info.saved) == 0 && (b->fd = open(b->info.node, O_WRONLY | O_CLOEXEC)) >= 0) {
    b->info.ceiling = o.ceiling && o.ceiling <= b->info.max_brightness ? o.ceiling : br_bright_ceiling(b->info.max_brightness);
    b->info.floor = o.floor ? o.floor : 1;
    if (b->info.floor > b->info.ceiling) b->info.floor = b->info.ceiling;
    b->written = b->info.saved;
    b->level = b->target = level_of(b->info.saved, b->info.floor, b->info.ceiling);
    if (pthread_create(&b->panel_thread, NULL, panel_worker, b) == 0) b->info.panel = true;
    else { close(b->fd); b->fd = -1; }
  }
  if (b->info.panel)
    br_log(1, "brightness: panel on: %s, %u..%u of %u (One UI left %u), the slider squared", b->info.node, b->info.floor, b->info.ceiling,
           b->info.max_brightness, b->info.saved);
  else br_log(0, "brightness: panel off: no writable backlight device under %s: %s", dir, strerror(errno));

  if (!o.no_als) als_open(b, &o);
  if (!b->info.panel && !b->info.als) { br_bright_close(b); return NULL; }
  return b;
}

void br_bright_info_get(const br_bright *b, br_bright_info *out) { *out = b->info; }

void br_bright_set(br_bright *b, uint32_t level, bool linear, uint32_t fade_ms)
{
  if (!b || !b->info.panel) return;
  if (level > BR_BRIGHT_FULL) level = BR_BRIGHT_FULL;
  if (fade_ms > 10000) fade_ms = 10000;
  pthread_mutex_lock(&b->lock);
  b->from = b->level;
  b->target = level;
  b->linear = linear;
  b->fade_ms = fade_ms;
  b->started_ms = now_ms();
  b->pending = true;
  pthread_cond_signal(&b->wake);
  pthread_mutex_unlock(&b->lock);
}

uint32_t br_bright_current(br_bright *b) { return __atomic_load_n(&b->written, __ATOMIC_RELAXED); }

uint32_t br_bright_level(br_bright *b) { return level_of(br_bright_current(b), b->info.floor, b->info.ceiling); }

void br_bright_listen(br_bright *b, br_als_fn fn, void *ctx)
{
  if (!b) return;
  pthread_mutex_lock(&b->lock);
  b->fn = fn;
  b->fn_ctx = ctx;
  pthread_mutex_unlock(&b->lock);
}

void br_bright_close(br_bright *b)
{
  if (!b) return;
  pthread_mutex_lock(&b->lock);
  b->stop = true;
  pthread_cond_signal(&b->wake);
  pthread_mutex_unlock(&b->lock);
  if (b->info.panel) {
    pthread_join(b->panel_thread, NULL);
    panel_write(b, b->info.saved);         // One UI's own value; system_server sets it again when it starts
  }
  if (b->als_running) pthread_join(b->als_thread, NULL);
  if (b->als_enabled_here) (void)write_text(b->als_enable, "0\n");
  if (b->als_fd >= 0) close(b->als_fd);
  if (b->fd >= 0) close(b->fd);
  pthread_mutex_destroy(&b->lock);
  pthread_cond_destroy(&b->wake);
  free(b);
}
