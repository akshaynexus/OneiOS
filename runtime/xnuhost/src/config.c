// SPDX-License-Identifier: MIT
// Command line, environment and the image set's optional xnuhost.conf.
#include "xnuhost.h"
#include <ctype.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

void xh_config_defaults(xh_config *c)
{
  memset(c, 0, sizeof *c);
  c->session_abi = 1;
  strcpy(c->mode, "takeover");
  // One vCPU per core of the host, up to the 8 the phone has (the bridges give them its cluster types).
  long cores = sysconf(_SC_NPROCESSORS_ONLN);
  c->ncpu = cores < 1 ? 1 : cores > XH_MAX_CPUS ? XH_MAX_CPUS : (unsigned)cores;
  c->ram_bytes = 0;                       // ram=auto
  c->ram_percent = -1;
  c->ram_host_floor = -1;
  c->host_mem_log = -1;
  c->ram_giveback = c->ram_prefault = c->ram_lock = -1;
  c->ram_session_reserve = c->ram_unlock_below = -1;
  strcpy(c->display, "shm");
  strcpy(c->gpu, "auto");
  c->input = true;
  c->touch_sleep = true;
  c->tasks = true;
  c->task_uid = -1;
  c->log_level = XH_LOG_INFO;
  c->bridges = -1;
  c->scale_fixup = -1;
  c->setway_skip = -1;
  c->perf_log = c->spin_host = c->spin_task = c->map_defer = c->cpu_pin = -1;
  strcpy(c->bluetooth, "auto");
  strcpy(c->wifi, "auto");
}

uint64_t xh_parse_size(const char *text, bool *ok)
{
  char *end;
  errno = 0;
  uint64_t v = strtoull(text, &end, 0);
  *ok = end != text && errno == 0;
  switch (*end) {
  case 'k': case 'K': v <<= 10; end++; break;
  case 'm': case 'M': v <<= 20; end++; break;
  case 'g': case 'G': v <<= 30; end++; break;
  }
  if (*end) *ok = false;
  return v;
}

static int copy(char *to, size_t size, const char *from)
{
  if (strlen(from) >= size) return -1;
  strcpy(to, from);
  return 0;
}

static int flag(const char *value, bool *out)
{
  if (!strcmp(value, "1") || !strcmp(value, "yes") || !strcmp(value, "true")) { *out = true; return 0; }
  if (!strcmp(value, "0") || !strcmp(value, "no") || !strcmp(value, "false")) { *out = false; return 0; }
  return -1;
}

// A whole decimal number in [lo, hi].
static bool number(const char *value, int lo, int hi, int *out)
{
  char *end;
  long v = strtol(value, &end, 10);
  if (end == value || *end || v < lo || v > hi) return false;
  *out = (int)v;
  return true;
}

int xh_config_set(xh_config *c, const char *key, const char *value)
{
  bool ok = true;
  if (!strcmp(key, "session-abi")) c->session_abi = atoi(value);
  else if (!strcmp(key, "mode")) return copy(c->mode, sizeof c->mode, value);
  else if (!strcmp(key, "runtime")) return copy(c->runtime, sizeof c->runtime, value);
  else if (!strcmp(key, "image")) return copy(c->image, sizeof c->image, value);
  else if (!strcmp(key, "data")) return copy(c->data, sizeof c->data, value);
  else if (!strcmp(key, "session")) return copy(c->session, sizeof c->session, value);
  else if (!strcmp(key, "kernel")) return copy(c->kernel, sizeof c->kernel, value);
  else if (!strcmp(key, "identity")) return copy(c->identity, sizeof c->identity, value);
  else if (!strcmp(key, "boot-args")) return copy(c->boot_args, sizeof c->boot_args, value);
  else if (!strcmp(key, "display")) return copy(c->display, sizeof c->display, value);
  else if (!strcmp(key, "gpu")) { ok = !strcmp(value, "auto") || !strcmp(value, "3d") || !strcmp(value, "2d"); if (ok) return copy(c->gpu, sizeof c->gpu, value); }
  else if (!strcmp(key, "cpus")) { c->ncpu = (unsigned)atoi(value); ok = c->ncpu >= 1 && c->ncpu <= XH_MAX_CPUS; }
  else if (!strcmp(key, "ram")) {
    c->ram_bytes = strcmp(value, "auto") ? xh_parse_size(value, &ok) : 0;
    ok = ok && (c->ram_bytes || !strcmp(value, "auto"));
    c->ram_given = ok;
  }
  else if (!strcmp(key, "ram-percent")) { ok = number(value, 10, 100, &c->ram_percent); }
  else if (!strcmp(key, "ram-host-floor")) { uint64_t v = xh_parse_size(value, &ok); ok = ok && v < (1ull << 40); if (ok) c->ram_host_floor = (int64_t)v; }
  else if (!strcmp(key, "ram-meminfo")) return copy(c->ram_meminfo, sizeof c->ram_meminfo, value);
  else if (!strcmp(key, "host-mem-log")) { ok = number(value, 0, 86400, &c->host_mem_log); }
  else if (!strcmp(key, "ram-giveback")) { bool on; if (flag(value, &on)) return -1; c->ram_giveback = on; }
  else if (!strcmp(key, "ram-prefault")) { bool on; if (flag(value, &on)) return -1; c->ram_prefault = on; }
  else if (!strcmp(key, "ram-lock")) { bool on; if (flag(value, &on)) return -1; c->ram_lock = on; }
  else if (!strcmp(key, "ram-session-reserve")) { uint64_t v = xh_parse_size(value, &ok); ok = ok && v < (1ull << 40); if (ok) c->ram_session_reserve = (int64_t)v; }
  else if (!strcmp(key, "ram-unlock-below")) { uint64_t v = xh_parse_size(value, &ok); ok = ok && v < (1ull << 40); if (ok) c->ram_unlock_below = (int64_t)v; }
  else if (!strcmp(key, "width")) c->width = (unsigned)atoi(value);
  else if (!strcmp(key, "height")) c->height = (unsigned)atoi(value);
  else if (!strcmp(key, "hz")) { c->hz = (unsigned)atoi(value); ok = c->hz == 0 || (c->hz >= 24 && c->hz <= 240); }
  else if (!strcmp(key, "scale")) { c->scale = (unsigned)atoi(value); ok = c->scale == 2 || c->scale == 3; }
  else if (!strcmp(key, "scale-fixup")) { bool on; if (flag(value, &on)) return -1; c->scale_fixup = on; }
  else if (!strcmp(key, "text-fixup")) {
    // FILE,OFFSET,ORIGINAL,REPLACEMENT in hex, as UniXNU's [payload] text_fixups (the kernel checks them again)
    size_t n = strlen(value);
    unsigned commas = 0;
    for (size_t i = 0; i < n; i++) commas += value[i] == ',';
    if (commas != 3 || n >= 160 || c->text_fixups_length + n + 1 > sizeof c->text_fixups) return -1;
    memcpy(c->text_fixups + c->text_fixups_length, value, n + 1);
    c->text_fixups_length += (unsigned)n + 1;
  }
  else if (!strcmp(key, "input")) return flag(value, &c->input);
  else if (!strcmp(key, "input-script")) return copy(c->input_script, sizeof c->input_script, value);
  else if (!strcmp(key, "tasks")) return flag(value, &c->tasks);
  else if (!strcmp(key, "task-uid")) c->task_uid = atoi(value);
  else if (!strcmp(key, "touch-sleep")) return flag(value, &c->touch_sleep);
  else if (!strcmp(key, "bridges")) { bool on; if (flag(value, &on)) return -1; c->bridges = on; }
  else if (!strcmp(key, "clusters")) { ok = strlen(value) <= XH_MAX_CPUS && strspn(value, "EP") == strlen(value); if (ok) strcpy(c->clusters, value); }
  else if (!strcmp(key, "wifi")) { ok = !strcmp(value, "auto") || !strcmp(value, "off") || !strcmp(value, "tap") || !strcmp(value, "sim"); if (ok) strcpy(c->wifi, value); }
  else if (!strcmp(key, "bluetooth")) { ok = !strcmp(value, "auto") || !strcmp(value, "virtual") || !strcmp(value, "hidl") || !strcmp(value, "sim"); if (ok) strcpy(c->bluetooth, value); }
  else if (!strcmp(key, "legacy-boot-args")) return flag(value, &c->legacy_boot_args);
  else if (!strcmp(key, "setway-skip")) { bool on; if (flag(value, &on)) return -1; c->setway_skip = on; }
  else if (!strcmp(key, "perf-log")) { ok = number(value, 0, 86400, &c->perf_log); }
  else if (!strcmp(key, "spin-host")) { ok = number(value, 0, 1000, &c->spin_host); }
  else if (!strcmp(key, "spin-task")) { ok = number(value, 0, 1000, &c->spin_task); }
  else if (!strcmp(key, "cpu-pin")) { if (!strcmp(value, "2")) c->cpu_pin = 2; else { bool on; if (flag(value, &on)) return -1; c->cpu_pin = on; } }
  else if (!strcmp(key, "map-defer")) { bool on; if (flag(value, &on)) return -1; c->map_defer = on; }
  else if (!strcmp(key, "log-level")) c->log_level = atoi(value);
  else if (!strcmp(key, "disk") || !strcmp(key, "disk-rw") || !strcmp(key, "data-store")) {
    // disk=FILE (read-only), disk-rw=FILE[,SEED], data-store=FILE[,SEED]
    if (c->ndisk == XH_MAX_DISKS) return -1;
    xh_disk_config *d = &c->disk[c->ndisk];
    memset(d, 0, sizeof *d);
    const char *comma = strchr(value, ',');
    size_t n = comma ? (size_t)(comma - value) : strlen(value);
    if (n == 0 || n >= sizeof d->path) return -1;
    memcpy(d->path, value, n);
    if (comma && copy(d->seed, sizeof d->seed, comma + 1)) return -1;
    d->writable = strcmp(key, "disk") != 0;
    d->data_store = !strcmp(key, "data-store");
    if (comma && !d->writable) return -1;
    c->ndisk++;
  } else return -1;
  return ok ? 0 : -1;
}

int xh_config_file(xh_config *c, const char *path)
{
  FILE *f = fopen(path, "r");
  if (!f) return errno == ENOENT ? 0 : -1;
  char line[1400];
  int rc = 0, number = 0;
  while (fgets(line, sizeof line, f)) {
    number++;
    char *p = line;
    while (isspace((unsigned char)*p)) p++;
    size_t n = strlen(p);
    while (n && isspace((unsigned char)p[n - 1])) p[--n] = 0;
    if (!*p || *p == '#') continue;
    char *eq = strchr(p, '=');
    if (!eq) { rc = -1; break; }
    *eq = 0;
    if (xh_config_set(c, p, eq + 1)) { fprintf(stderr, "xnuhost: %s:%d: bad setting %s\n", path, number, p); rc = -1; break; }
  }
  fclose(f);
  return rc;
}

static void usage(void)
{
  fputs("usage: xnuhost --image DIR [--kernel FILE] [--data DIR] [--session DIR] [--runtime DIR]\n"
        "               [--session-abi 1] [--mode takeover|bench] [--cpus N] [--ram SIZE|auto]\n"
        "               [--boot-args TEXT] [--display shm|drm|none] [--gpu auto|3d|2d] [--width W --height H]\n"
        "               [--hz N] [--scale 2|3]\n"
        "               [--no-input] [--input-script FILE] [--no-tasks] [--config FILE] [--set KEY=VALUE] [-v]\n"
        "       xnuhost --selftest      checks of this host (trap layer, PAC, memory), no kernel\n"
        "docs/booter.md describes the image set's files, the session files and the exit codes.\n", stderr);
}

int xh_config_args(xh_config *c, int argc, char **argv)
{
  static const struct { const char *env, *key; } envs[] = {
    { "ONEIOS_SESSION_ABI", "session-abi" }, { "ONEIOS_MODE", "mode" }, { "ONEIOS_RUNTIME", "runtime" },
    { "ONEIOS_IMAGE", "image" }, { "ONEIOS_DATA", "data" }, { "ONEIOS_SESSION", "session" },
  };
  for (unsigned i = 0; i < sizeof envs / sizeof *envs; i++) {
    const char *v = getenv(envs[i].env);
    if (v && *v && xh_config_set(c, envs[i].key, v)) { fprintf(stderr, "xnuhost: bad %s\n", envs[i].env); return -1; }
  }
  for (int i = 1; i < argc; i++) {
    const char *a = argv[i];
    if (!strcmp(a, "-v")) { c->log_level = XH_LOG_DEBUG; continue; }
    if (!strcmp(a, "--selftest")) { c->selftest = true; continue; }
    if (!strcmp(a, "--no-input")) { c->input = false; continue; }
    if (!strcmp(a, "--no-tasks")) { c->tasks = false; continue; }
    if (!strcmp(a, "-h") || !strcmp(a, "--help")) { usage(); return -1; }
    if (strncmp(a, "--", 2) || i + 1 >= argc) { fprintf(stderr, "xnuhost: unknown or incomplete argument %s\n", a); usage(); return -1; }
    const char *v = argv[++i];
    if (!strcmp(a, "--config")) {
      if (xh_config_file(c, v)) { fprintf(stderr, "xnuhost: cannot use %s\n", v); return -1; }
    } else if (!strcmp(a, "--set")) {
      char pair[1200];
      const char *eq = strchr(v, '=');
      if (!eq || strlen(v) >= sizeof pair) { fprintf(stderr, "xnuhost: --set takes KEY=VALUE\n"); return -1; }
      strcpy(pair, v);
      pair[eq - v] = 0;
      if (xh_config_set(c, pair, eq + 1)) { fprintf(stderr, "xnuhost: bad setting %s\n", pair); return -1; }
    } else if (xh_config_set(c, a + 2, v)) {
      fprintf(stderr, "xnuhost: bad value for %s\n", a);
      return -1;
    }
  }
  return 0;
}

static bool exists(const char *dir, const char *name, char *out, size_t size)
{
  struct stat st;
  if (snprintf(out, size, "%s/%s", dir, name) >= (int)size) return false;
  return stat(out, &st) == 0 && S_ISREG(st.st_mode);
}

int xh_config_finish(xh_config *c)
{
  char path[512];
  if (c->session_abi != 1) { fprintf(stderr, "xnuhost: session ABI %d is not supported (this build: 1)\n", c->session_abi); return -1; }
  if (strcmp(c->mode, "takeover") && strcmp(c->mode, "bench")) { fprintf(stderr, "xnuhost: mode %s is not supported\n", c->mode); return -1; }
  if (strcmp(c->display, "shm") && strcmp(c->display, "drm") && strcmp(c->display, "none")) { fprintf(stderr, "xnuhost: display %s is not supported\n", c->display); return -1; }
  // The host bridges change the phone's state (a tap, routing rules, the motor): on in a session.
  if (c->bridges < 0) c->bridges = !strcmp(c->mode, "takeover");
  if (c->selftest) return 0;
  if (!*c->image) { fprintf(stderr, "xnuhost: --image DIR is required\n"); return -1; }
  // The image set may carry settings of its own; the command line was applied first and a setting
  // there wins for single values (disks add up).
  xh_config from_image;
  xh_config_defaults(&from_image);
  if (snprintf(path, sizeof path, "%s/xnuhost.conf", c->image) < (int)sizeof path && xh_config_file(&from_image, path)) return -1;
  // The userland hook's settings of this image (the UI scale its Data seed was made for), under the same rule.
  if (snprintf(path, sizeof path, "%s/overlay/userland/xnuhost.conf", c->image) < (int)sizeof path && xh_config_file(&from_image, path)) return -1;
  if (!*c->boot_args) strcpy(c->boot_args, from_image.boot_args);
  if (!c->width) c->width = from_image.width;
  if (c->setway_skip < 0) c->setway_skip = from_image.setway_skip < 0 ? 1 : from_image.setway_skip;
  if (c->perf_log < 0) c->perf_log = from_image.perf_log;
  if (c->spin_host < 0) c->spin_host = from_image.spin_host;
  if (c->spin_task < 0) c->spin_task = from_image.spin_task;
  if (c->map_defer < 0) c->map_defer = from_image.map_defer;
  if (c->cpu_pin < 0) c->cpu_pin = from_image.cpu_pin < 0 ? 1 : from_image.cpu_pin;
  if (c->map_defer < 0) c->map_defer = 1;
  if (!c->height) c->height = from_image.height;
  if (!c->hz) c->hz = from_image.hz;
  if (!c->scale) c->scale = from_image.scale ? from_image.scale : 3;
  if (c->scale_fixup < 0) c->scale_fixup = from_image.scale_fixup < 0 ? 1 : from_image.scale_fixup;
  if (!c->text_fixups_length) { memcpy(c->text_fixups, from_image.text_fixups, sizeof c->text_fixups); c->text_fixups_length = from_image.text_fixups_length; }
  // Neither the command line nor the image set gave any: what the hosted kernel needs to start on this
  // machine (the session's command line has no boot arguments). From Droid-VM's ipados27/board.toml
  // kernel_args, the ones about the machine: SMP and ArmCpu's IPIs, no contiguous-hint mappings (the
  // collection's layout: arm_vm_init.c:1107 asserts without it), no interrupt-masked or preemption watchdogs
  // (a vCPU is a host thread), the source storage, the GPU's 3D context, the console in the log.
  if (!*c->boot_args)
    strcpy(c->boot_args, "debug=0x14e serial=3 -v kt_fixed_zones=120 -no_slto_panic ml-timeout-lock-panic-scale=40 tlto_us=0 "
           "sheng-smp=1 armcpu-self-ipi=1 armcpu-spi-ipi=1 use_contiguous_hint=0 interrupt-masked-debug-mode=0 "
           "sched_preemption_disable_debug_mode=0 wdt=-1 native-source-storage=1 vgpu-3d=1 droidvm-gpu-metal=1");
  if (c->ndisk == 0) { memcpy(c->disk, from_image.disk, sizeof c->disk); c->ndisk = from_image.ndisk; }
  if (!*c->kernel && !exists(c->image, "kernel.kc", c->kernel, sizeof c->kernel) &&
      !exists(c->image, "kernel.macho", c->kernel, sizeof c->kernel)) {
    fprintf(stderr, "xnuhost: no kernel.kc in %s (and no --kernel)\n", c->image);
    return -1;
  }
  if (!*c->identity && !exists(c->image, "identity.adt", c->identity, sizeof c->identity)) c->identity[0] = 0;
  if (c->ndisk == 0) {
    // The image set's volumes (scripts/image/build.sh): read-only APFS containers, in this order.
    static const char *const names[] = { "os.img", "systemos.img", "app.img" };
    for (unsigned i = 0; i < 3; i++)
      if (exists(c->image, names[i], path, sizeof path)) {
        xh_disk_config *d = &c->disk[c->ndisk++];
        memset(d, 0, sizeof *d);
        strcpy(d->path, path);
      }
    if (*c->data) {
      xh_disk_config *d = &c->disk[c->ndisk++];
      memset(d, 0, sizeof *d);
      if (snprintf(d->path, sizeof d->path, "%s/data.img", c->data) >= (int)sizeof d->path) return -1;
      d->writable = d->data_store = true;
      if (exists(c->image, "data.seed", path, sizeof path)) strcpy(d->seed, path);
    }
    // The Data seed is a tar (docs/image-format.md): Linux cannot write APFS, so the guest makes the
    // Data volume itself on the first start and unpacks the seed, which it reads as the last
    // read-only disk. (An APFS seed image is copied into the store instead: blk.c.)
    if (exists(c->image, "data.seed", path, sizeof path)) {
      xh_disk_config *d = &c->disk[c->ndisk++];
      memset(d, 0, sizeof *d);
      strcpy(d->path, path);
    }
  }
  // The guest's RAM: the command line's ram=, else the image set's, else auto.
  if (!c->ram_given && from_image.ram_given) c->ram_bytes = from_image.ram_bytes;
  if (c->ram_percent < 0) c->ram_percent = from_image.ram_percent < 0 ? XH_RAM_AUTO_PERCENT : from_image.ram_percent;
  if (c->ram_host_floor < 0) c->ram_host_floor = from_image.ram_host_floor < 0 ? (int64_t)XH_RAM_AUTO_FLOOR : from_image.ram_host_floor;
  if (c->host_mem_log < 0) c->host_mem_log = from_image.host_mem_log < 0 ? 30 : from_image.host_mem_log;
  if (!*c->ram_meminfo) strcpy(c->ram_meminfo, *from_image.ram_meminfo ? from_image.ram_meminfo : "/proc/meminfo");
  // The RAM held fixed for the session (ramfix.c) with ram-giveback=0; the default is still 0058's way (ram-giveback=1):
  // the fixed RAM has run in the host tests and the test kernel only, not in a PC boot of iOS or on the phone.
  if (c->ram_giveback < 0) c->ram_giveback = from_image.ram_giveback < 0 ? 1 : from_image.ram_giveback;
  if (c->ram_prefault < 0) c->ram_prefault = from_image.ram_prefault < 0 ? !c->ram_giveback : from_image.ram_prefault;
  if (c->ram_lock < 0) c->ram_lock = from_image.ram_lock < 0 ? !c->ram_giveback : from_image.ram_lock;
  if (c->ram_session_reserve < 0) c->ram_session_reserve = from_image.ram_session_reserve < 0 ? (int64_t)XH_RAM_SESSION_RESERVE : from_image.ram_session_reserve;
  if (c->ram_unlock_below < 0) c->ram_unlock_below = from_image.ram_unlock_below < 0 ? (int64_t)XH_RAM_UNLOCK_BELOW : from_image.ram_unlock_below;
  if (c->ram_giveback) c->ram_prefault = c->ram_lock = 0;        // a RAM that gives pages back is neither prefaulted nor locked
  if (!c->ram_prefault) c->ram_lock = 0;
  if (!c->ram_bytes) {
    uint64_t total = 0, available = 0;
    if (xh_meminfo(c->ram_meminfo, &total, &available)) {
      // Not Linux, or no /proc: the size of the time before ram=auto.
      fprintf(stderr, "xnuhost: ram=auto: no MemTotal in %s: the guest gets 4 GiB\n", c->ram_meminfo);
      c->ram_bytes = 4ull << 30;
    } else {
      // Held for the whole session: no more than the host has free at the start, less what it needs while iOS runs.
      uint64_t reserve = c->ram_giveback ? 0 : (uint64_t)c->ram_session_reserve;
      c->ram_bytes = xh_ram_fixed_auto(total, available, (unsigned)c->ram_percent, (uint64_t)c->ram_host_floor, reserve, &c->ram_auto_bound);
      if (!c->ram_bytes) {
        fprintf(stderr, "xnuhost: ram=auto: this host has %llu MiB, %llu MiB of it available; %d%% of it, all but %llu MiB, or the available "
                "less %llu MiB, is less than the guest's 1 GiB: give --ram SIZE\n", (unsigned long long)(total >> 20),
                (unsigned long long)(available >> 20), c->ram_percent, (unsigned long long)((uint64_t)c->ram_host_floor >> 20),
                (unsigned long long)(reserve >> 20));
        return -1;
      }
      c->ram_auto_total = total;
      c->ram_auto_available = available;
    }
  }
  if (c->ram_bytes & 0x1fffff || c->ram_bytes < (256ull << 20) || c->ram_bytes > XH_RAM_MAX) {
    fprintf(stderr, "xnuhost: --ram must be a multiple of 2 MiB, at least 256 MiB, at most %llu GiB\n", XH_RAM_MAX >> 30);
    return -1;
  }
  return 0;
}

uint64_t xh_ram_auto(uint64_t total, unsigned percent, uint64_t floor)
{
  if (percent > 100) percent = 100;
  uint64_t share = total / 100 * percent + total % 100 * percent / 100, left = total > floor ? total - floor : 0;
  uint64_t ram = (share < left ? share : left) & ~((256ull << 20) - 1);
  if (ram > XH_RAM_MAX) ram = XH_RAM_MAX;
  return ram >= (1ull << 30) ? ram : 0;
}

uint64_t xh_ram_fixed_auto(uint64_t total, uint64_t available, unsigned percent, uint64_t floor, uint64_t reserve, unsigned *bound)
{
  uint64_t ram = xh_ram_auto(total, percent, floor);
  unsigned why = 0;
  if (ram) {
    unsigned p = percent > 100 ? 100 : percent;
    uint64_t share = total / 100 * p + total % 100 * p / 100, left = total > floor ? total - floor : 0;
    why = share <= left ? XH_RAM_BOUND_SHARE : XH_RAM_BOUND_FLOOR;
  }
  if (ram && reserve) {
    // Held from the start and never given back: no more than the host has free now, less what it needs while iOS runs.
    uint64_t fits = (available > reserve ? available - reserve : 0) & ~((256ull << 20) - 1);
    if (fits < ram) { ram = fits; why = XH_RAM_BOUND_AVAILABLE; }
    if (ram < (1ull << 30)) ram = 0;
  }
  if (bound) *bound = ram ? why : 0;
  return ram;
}

bool xh_ram_lock_fits(uint64_t size, uint64_t available, uint64_t reserve)
{
  return available > reserve && size <= available - reserve;
}

int xh_meminfo(const char *path, uint64_t *total, uint64_t *available)
{
  FILE *f = fopen(path, "r");
  if (!f) return -1;
  char line[128];
  unsigned long long kb;
  int found = 0;
  while (fgets(line, sizeof line, f) && found != 3) {
    if (sscanf(line, "MemTotal: %llu kB", &kb) == 1) { *total = (uint64_t)kb << 10; found |= 1; }
    else if (sscanf(line, "MemAvailable: %llu kB", &kb) == 1) { *available = (uint64_t)kb << 10; found |= 2; }
  }
  fclose(f);
  return found == 3 && *total ? 0 : -1;
}

int xh_meminfo_value(const char *path, const char *key, uint64_t *bytes)
{
  FILE *f = fopen(path, "r");
  if (!f) return -1;
  char line[128];
  size_t n = strlen(key);
  int r = -1;
  while (fgets(line, sizeof line, f)) {
    unsigned long long kb;
    if (!strncmp(line, key, n) && line[n] == ':' && sscanf(line + n + 1, " %llu kB", &kb) == 1) { *bytes = (uint64_t)kb << 10; r = 0; break; }
  }
  fclose(f);
  return r;
}

// ---- the display's mode ---------------------------------------------------------------------------
bool xh_panel_native(const xh_panel_mode *modes, unsigned count, xh_panel_mode *native)
{
  bool found = false;
  for (unsigned i = 0; i < count; i++) {
    const xh_panel_mode *m = &modes[i];
    if (!m->width || !m->height || m->width > 16384 || m->height > 16384) continue;
    uint64_t area = (uint64_t)m->width * m->height, best = found ? (uint64_t)native->width * native->height : 0;
    if (!found || area > best || (area == best && m->width == native->width && m->hz > native->hz)) { *native = *m; found = true; }
  }
  return found;
}

bool xh_panel_connector(unsigned type)
{
  // DRM_MODE_CONNECTOR_LVDS, eDP, DSI, DPI. A phone's Virtual connectors (the writeback ones) report "connected"
  // with modes up to 5120x2560, and DisplayPort is whatever monitor is plugged in.
  return type == 7 || type == 14 || type == 16 || type == 17;
}

bool xh_panel_report(const void *message, size_t length, xh_panel_mode *panel)
{
  struct { char magic[4]; uint32_t width, height, hz, unused; } m;
  if (length != sizeof m) return false;
  memcpy(&m, message, sizeof m);
  if (memcmp(m.magic, "XHDP", 4) || !m.width || !m.height || m.width > 16384 || m.height > 16384) return false;
  *panel = (xh_panel_mode){ m.width, m.height, m.hz };
  return true;
}

xh_panel_mode xh_display_mode(const xh_config *c, const xh_panel_mode *panel)
{
  xh_panel_mode mode = { XH_PANEL_WIDTH, XH_PANEL_HEIGHT, XH_PANEL_HZ };
  if (panel && panel->width && panel->height) { mode = *panel; if (mode.hz < 24 || mode.hz > 240) mode.hz = XH_PANEL_HZ; }
  if (c->width) mode.width = c->width;
  if (c->height) mode.height = c->height;
  if (c->hz) mode.hz = c->hz;
  return mode;
}

static bool has_switch(const char *args, const char *name)
{
  size_t n = strlen(name);
  for (const char *p = args; (p = strstr(p, name)); p += n)
    if ((p == args || p[-1] == ' ') && p[n] == '=') return true;
  return false;
}

int xh_display_boot_args(char *args, size_t size, unsigned scale, unsigned hz)
{
  char add[80];
  if (!has_switch(args, "offline-legacy-unit-scale")) {
    snprintf(add, sizeof add, "%soffline-legacy-unit-scale=%u", *args ? " " : "", scale);
    if (strlen(args) + strlen(add) >= size) return -1;
    strcat(args, add);
  }
  if (!has_switch(args, "sheng-vsync")) {
    snprintf(add, sizeof add, "%ssheng-vsync=%u", *args ? " " : "", hz);
    if (strlen(args) + strlen(add) >= size) return -1;
    strcat(args, add);
  }
  return 0;
}

// -[CADisplayMode preferredScale] of QuartzCore, read in 24A446's cache (docs/userland.md, "The UI scale"): a
// mode with no preferred scale of its own is @2x in a virtual device (or what the mode's own field says, when
// more), else @2x when taller than 1080 pixels and @1x otherwise. Nothing there answers 3, so for a scale of 3
// both answers are changed: the virtual device's floor, and the height test's result. A scale of 2 is what the
// code gives unchanged. The kernel applies a fixup only where the original bytes match.
unsigned xh_scale_fixups(const char *build, unsigned scale, char *list, unsigned length, unsigned size)
{
  static const char *const fixups_24A446_3x[] = {
    "dyld_shared_cache_arm64e.01,40b17a8,49008052,69008052",                    // mov w9, #2 -> mov w9, #3
    "dyld_shared_cache_arm64e.01,40b17bc,280080520895889a,680080521f2003d5",    // mov w8, #1; cinc x8, x8, hi -> mov w8, #3; nop
  };
  if (scale != 3 || !build || strcmp(build, "24A446")) return length;
  for (unsigned i = 0; i < sizeof fixups_24A446_3x / sizeof *fixups_24A446_3x; i++) {
    unsigned n = (unsigned)strlen(fixups_24A446_3x[i]) + 1;
    if (length + n > size) break;
    memcpy(list + length, fixups_24A446_3x[i], n);
    length += n;
  }
  return length;
}
