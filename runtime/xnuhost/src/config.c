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
  c->ncpu = 4;
  c->ram_bytes = 4ull << 30;
  strcpy(c->display, "shm");
  c->input = true;
  c->tasks = true;
  c->task_uid = -1;
  c->log_level = XH_LOG_INFO;
  c->bridges = -1;
  strcpy(c->bluetooth, "virtual");
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
  else if (!strcmp(key, "cpus")) { c->ncpu = (unsigned)atoi(value); ok = c->ncpu >= 1 && c->ncpu <= XH_MAX_CPUS; }
  else if (!strcmp(key, "ram")) c->ram_bytes = xh_parse_size(value, &ok);
  else if (!strcmp(key, "width")) c->width = (unsigned)atoi(value);
  else if (!strcmp(key, "height")) c->height = (unsigned)atoi(value);
  else if (!strcmp(key, "input")) return flag(value, &c->input);
  else if (!strcmp(key, "tasks")) return flag(value, &c->tasks);
  else if (!strcmp(key, "task-uid")) c->task_uid = atoi(value);
  else if (!strcmp(key, "bridges")) { bool on; if (flag(value, &on)) return -1; c->bridges = on; }
  else if (!strcmp(key, "clusters")) { ok = strlen(value) <= XH_MAX_CPUS && strspn(value, "EP") == strlen(value); if (ok) strcpy(c->clusters, value); }
  else if (!strcmp(key, "bluetooth")) { ok = !strcmp(value, "virtual") || !strcmp(value, "hidl"); if (ok) strcpy(c->bluetooth, value); }
  else if (!strcmp(key, "legacy-boot-args")) return flag(value, &c->legacy_boot_args);
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
        "               [--session-abi 1] [--mode takeover|bench] [--cpus N] [--ram SIZE]\n"
        "               [--boot-args TEXT] [--display shm|drm|none] [--width W --height H]\n"
        "               [--no-input] [--no-tasks] [--config FILE] [--set KEY=VALUE] [-v]\n"
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
  if (!*c->boot_args) strcpy(c->boot_args, from_image.boot_args);
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
  if (c->ram_bytes & 0x1fffff || c->ram_bytes < (256ull << 20)) { fprintf(stderr, "xnuhost: --ram must be a multiple of 2 MiB, at least 256 MiB\n"); return -1; }
  return 0;
}
