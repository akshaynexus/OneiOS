// SPDX-License-Identifier: MIT
// oneios-bootstat: says on the kernel console how far the guest's boot is, so the host (xnuhost's log) can
// tell "launchd is up" from "SpringBoard is up" without a display.
//
// It reads Darwin notify states, which need no entitlement:
//   com.apple.springboard.finishedstartup   non-zero once SpringBoard has started
//   com.apple.springboard.lockstate         non-zero while locked
//   com.apple.springboard.hasBlankedScreen  non-zero while the display is off
// (the three vphone-cli's screen.unlock reads: Research/Guest/screen_unlock.md), and whether a process named
// backboardd or SpringBoard exists (libproc).
//
// Each change is one line on stdout (oneios-init keeps it in log/bootstat.log) and on /dev/console:
//   ONEIOS-GUEST stage=<name> t=<seconds since start> [key=value...]
// stages, in order: bootstat-up, backboardd-up, springboard-process, springboard-up. It also writes the last
// stage to $ONEIOS_ROOT/state/boot. After springboard-up it keeps reporting lock and screen changes.
//
// Guest build: iOS SDK (notify.h; libproc's two calls, which the iPhoneOS SDK exports from libSystem but has no
// header for). Host build (tests, -DONEIOS_HOST_TEST, on Linux or macOS): states are read from files named by the
// notification under $ONEIOS_FAKE_NOTIFY, processes from $ONEIOS_FAKE_NOTIFY/proc.<name>.
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#if defined(__APPLE__) && !defined(ONEIOS_HOST_TEST)
#if __has_include(<libproc.h>)
#include <libproc.h>
#else
int proc_listallpids(void *buffer, int buffersize);
int proc_name(int pid, void *buffer, uint32_t buffersize);
#endif
#include <notify.h>

static int tokens[3] = {-1, -1, -1};
static uint64_t state_of(int index, const char *name) {
  uint64_t value = 0;
  if (tokens[index] < 0 && notify_register_check(name, &tokens[index]) != NOTIFY_STATUS_OK) { tokens[index] = -1; return 0; }
  if (notify_get_state(tokens[index], &value) != NOTIFY_STATUS_OK) return 0;
  return value;
}
static int running(const char *name) {
  static pid_t pids[4096];
  int bytes = proc_listallpids(pids, sizeof pids);
  for (int i = 0; i < bytes / (int)sizeof(pid_t); i++) {
    char found[64] = "";
    if (proc_name(pids[i], found, sizeof found) > 0 && !strcmp(found, name)) return 1;
  }
  return 0;
}
#else
static uint64_t fake(const char *name) {
  const char *dir = getenv("ONEIOS_FAKE_NOTIFY");
  char file[512];
  unsigned long long value = 0;
  if (!dir) return 0;
  snprintf(file, sizeof file, "%s/%s", dir, name);
  FILE *f = fopen(file, "r");
  if (!f) return 0;
  if (fscanf(f, "%llu", &value) != 1) value = 0;
  fclose(f);
  return value;
}
static uint64_t state_of(int index, const char *name) { (void)index; return fake(name); }
static int running(const char *name) {
  char key[80];
  snprintf(key, sizeof key, "proc.%s", name);
  return fake(key) != 0;
}
#endif

static time_t began;
static FILE *console;

static void report(const char *stage, const char *format, ...) {
  char extra[160] = "";
  if (format) {
    va_list ap;
    va_start(ap, format);
    vsnprintf(extra, sizeof extra, format, ap);
    va_end(ap);
  }
  char line[256];
  snprintf(line, sizeof line, "ONEIOS-GUEST stage=%s t=%ld%s%s", stage, (long)(time(NULL) - began), extra[0] ? " " : "", extra);
  printf("%s\n", line);
  fflush(stdout);
  if (console) { fprintf(console, "%s\n", line); fflush(console); }
}

static void keep(const char *stage) {
  const char *root = getenv("ONEIOS_ROOT");
  char file[512], tmp[520];
  snprintf(file, sizeof file, "%s/state/boot", root && *root ? root : "/private/var/oneios");
  snprintf(tmp, sizeof tmp, "%s.tmp", file);
  FILE *f = fopen(tmp, "w");
  if (!f) return;
  fprintf(f, "%s %ld\n", stage, (long)(time(NULL) - began));
  fclose(f);
  rename(tmp, file);
}

int main(int argc, char **argv) {
  // -n COUNT: stop after COUNT polls (tests); default runs for good.
  long polls = -1;
  int interval_ms = 500;
  for (int i = 1; i < argc; i++) {
    if (!strcmp(argv[i], "-n") && i + 1 < argc) polls = atol(argv[++i]);
    else if (!strcmp(argv[i], "-i") && i + 1 < argc) interval_ms = atoi(argv[++i]);
    else { fprintf(stderr, "usage: oneios-bootstat [-n polls] [-i milliseconds]\n"); return 2; }
  }
  if (interval_ms < 10) interval_ms = 10;
  began = time(NULL);
  const char *device = getenv("ONEIOS_CONSOLE");
  console = fopen(device ? device : "/dev/console", "w");  // absent or refused: stdout only
  report("bootstat-up", NULL);
  keep("bootstat-up");
  int backboard = 0, process = 0, up = 0;
  long long locked = -1, dark = -1;
  for (long n = 0; polls < 0 || n < polls; n++) {
    if (!backboard && running("backboardd")) { backboard = 1; report("backboardd-up", NULL); keep("backboardd-up"); }
    if (!process && running("SpringBoard")) { process = 1; report("springboard-process", NULL); keep("springboard-process"); }
    uint64_t started = state_of(0, "com.apple.springboard.finishedstartup");
    if (!up && process && started) { up = 1; report("springboard-up", NULL); keep("springboard-up"); }
    // The notify state outlives the process that set it, so a SpringBoard that died is seen by its name.
    if (process && !running("SpringBoard")) { up = 0; process = 0; locked = dark = -1; report("springboard-gone", NULL); keep("springboard-gone"); continue; }
    if (up) {
      // Until finishedstartup these two read "unlocked, lit" whatever will show (screen_unlock.md).
      long long l = state_of(1, "com.apple.springboard.lockstate") != 0, d = state_of(2, "com.apple.springboard.hasBlankedScreen") != 0;
      if (l != locked || d != dark) { locked = l; dark = d; report("screen", "locked=%lld dark=%lld", l, d); }
    }
    usleep((useconds_t)interval_ms * 1000);
  }
  return 0;
}
