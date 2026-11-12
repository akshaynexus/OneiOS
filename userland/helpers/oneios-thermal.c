// SPDX-License-Identifier: MIT
// oneios-thermal: gives iOS the phone's thermal pressure. The kernel's OneiOSThermal service (drivers/thermal) carries the
// level xnuhost's thermal bridge decided from the phone's own sensors (runtime/bridges/thermal); this helper puts it where
// iOS's frameworks read it: the notify state of "com.apple.system.thermalpressurelevel" (kOSThermalNotificationPressureLevelName,
// OSThermalNotification.h), on iOS's scale 0 nominal, 10 light, 20 moderate, 30 heavy, 40 trapping, 50 sleeping, then a post
// of that name. ProcessInfo.thermalState and the system's daemons follow that name [inference: what reads it was not traced].
//
// thermalmonitord also sets the name, from Apple's sensors, which do not exist here (it sees nominal). So the helper reads the
// state back at every poll and sets the host's level again when it differs, with one console line the first time.
//
//   oneios-thermal [-i SECONDS] [-n POLLS]     poll every SECONDS (5; 0 is 50 ms); POLLS 0 (the default) runs until killed
//
// Each change is a line on stdout (oneios-init keeps it in log/thermal.log) and on /dev/console:
//   ONEIOS-GUEST thermal level=<n> name=<name> step=<host CPU step> case=<C> soc=<C> [was=<n>]
// Runs as root (names under com.apple.system. are root's to set). Guest build: iOS SDK, IOKit and CoreFoundation.
// Host build (tests, -DONEIOS_HOST_TEST): the service is the file $ONEIOS_FAKE_THERMAL/service ("LEVEL STEP CASE SOC", or
// missing), the notify state the file $ONEIOS_FAKE_THERMAL/state (read and written).
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define PRESSURE_NAME "com.apple.system.thermalpressurelevel"

typedef struct { int have; unsigned level, step; int case_c100, soc_c100; } reading;

static const char *level_name(unsigned level)
{
  switch (level) {
  case 0: return "nominal";
  case 10: return "light";
  case 20: return "moderate";
  case 30: return "heavy";
  case 40: return "trapping";
  case 50: return "sleeping";
  }
  return "unknown";
}

#if defined(__APPLE__) && !defined(ONEIOS_HOST_TEST)
#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/IOKitLib.h>
#include <notify.h>

static int number(io_registry_entry_t e, CFStringRef key, CFDictionaryRef from, int *out)
{
  CFTypeRef v = from ? CFDictionaryGetValue(from, key) : IORegistryEntryCreateCFProperty(e, key, kCFAllocatorDefault, 0);
  int ok = v && CFGetTypeID(v) == CFNumberGetTypeID() && CFNumberGetValue((CFNumberRef)v, kCFNumberIntType, out);
  if (v && !from) CFRelease(v);
  return ok;
}

static reading read_service(void)
{
  reading r = { 0, 0, 0, INT32_MIN, INT32_MIN };
  static io_service_t service;
  if (!service) service = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("OneiOSThermal"));
  if (!service) return r;
  int level, step = 0;
  if (!number(service, CFSTR("ThermalPressureLevel"), NULL, &level)) { IOObjectRelease(service); service = 0; return r; }
  (void)number(service, CFSTR("HostStep"), NULL, &step);
  CFTypeRef temps = IORegistryEntryCreateCFProperty(service, CFSTR("Temperatures"), kCFAllocatorDefault, 0);
  if (temps && CFGetTypeID(temps) == CFDictionaryGetTypeID()) {
    (void)number(0, CFSTR("Case"), (CFDictionaryRef)temps, &r.case_c100);
    (void)number(0, CFSTR("SoC"), (CFDictionaryRef)temps, &r.soc_c100);
  }
  if (temps) CFRelease(temps);
  r.have = 1; r.level = (unsigned)level; r.step = (unsigned)step;
  return r;
}

static int token = -1;
static int state_get(uint64_t *value)
{
  if (token < 0 && notify_register_check(PRESSURE_NAME, &token) != NOTIFY_STATUS_OK) { token = -1; return -1; }
  return notify_get_state(token, value) == NOTIFY_STATUS_OK ? 0 : -1;
}
static int state_set(uint64_t value)
{
  if (token < 0 && notify_register_check(PRESSURE_NAME, &token) != NOTIFY_STATUS_OK) { token = -1; return -1; }
  if (notify_set_state(token, value) != NOTIFY_STATUS_OK) return -1;
  return notify_post(PRESSURE_NAME) == NOTIFY_STATUS_OK ? 0 : -1;
}
#else
static const char *fake_dir(void) { const char *d = getenv("ONEIOS_FAKE_THERMAL"); return d ? d : "."; }
static reading read_service(void)
{
  reading r = { 0, 0, 0, INT32_MIN, INT32_MIN };
  char path[512];
  snprintf(path, sizeof path, "%s/service", fake_dir());
  FILE *f = fopen(path, "r");
  if (!f) return r;
  if (fscanf(f, "%u %u %d %d", &r.level, &r.step, &r.case_c100, &r.soc_c100) >= 2) r.have = 1;
  fclose(f);
  return r;
}
static int state_get(uint64_t *value)
{
  char path[512];
  unsigned long long v = 0;
  snprintf(path, sizeof path, "%s/state", fake_dir());
  FILE *f = fopen(path, "r");
  if (!f) { *value = 0; return 0; }
  int n = fscanf(f, "%llu", &v);
  fclose(f);
  *value = n == 1 ? v : 0;
  return 0;
}
static int state_set(uint64_t value)
{
  char path[512];
  snprintf(path, sizeof path, "%s/state", fake_dir());
  FILE *f = fopen(path, "w");
  if (!f) return -1;
  fprintf(f, "%llu\n", (unsigned long long)value);
  return fclose(f) ? -1 : 0;
}
#endif

static void say(const char *fmt, ...)
{
  char line[256];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(line, sizeof line, fmt, ap);
  va_end(ap);
  printf("%s\n", line);
  fflush(stdout);
#ifndef ONEIOS_HOST_TEST
  FILE *console = fopen("/dev/console", "w");
  if (console) { fprintf(console, "%s\n", line); fclose(console); }
#endif
}

static void temp_text(char *out, size_t size, int c100)
{
  if (c100 == INT32_MIN) snprintf(out, size, "-");
  else snprintf(out, size, "%d.%d", c100 / 100, (c100 < 0 ? -c100 : c100) % 100 / 10);
}

int main(int argc, char **argv)
{
  unsigned interval = 5, polls = 0;
  int opt;
  while ((opt = getopt(argc, argv, "i:n:")) != -1) {
    if (opt == 'i') interval = (unsigned)strtoul(optarg, NULL, 10);
    else if (opt == 'n') polls = (unsigned)strtoul(optarg, NULL, 10);
    else { fprintf(stderr, "usage: oneios-thermal [-i SECONDS] [-n POLLS]\n"); return 2; }
  }
  unsigned told = ~0u;            // the level last said
  int missing_said = 0, overwritten_said = 0;
  for (unsigned k = 0; !polls || k < polls; k++) {
    if (k && interval) sleep(interval);
    else if (k) usleep(50000);            // -i 0: 50 ms (the tests)
    reading r = read_service();
    if (!r.have) {
      if (!missing_said) say("ONEIOS-GUEST thermal no-service (no OneiOSThermal in the registry: an older kernel, or oneios_thermal=0)");
      missing_said = 1;
      continue;
    }
    uint64_t now = 0;
    int readable = state_get(&now) == 0;
    if (r.level != told) {
      char c[16], s[16];
      temp_text(c, sizeof c, r.case_c100);
      temp_text(s, sizeof s, r.soc_c100);
      if (state_set(r.level)) say("ONEIOS-GUEST thermal set-failed level=%u (notify_set_state %s)", r.level, PRESSURE_NAME);
      else if (told == ~0u) say("ONEIOS-GUEST thermal level=%u name=%s step=%u case=%s soc=%s", r.level, level_name(r.level), r.step, c, s);
      else say("ONEIOS-GUEST thermal level=%u name=%s step=%u case=%s soc=%s was=%u", r.level, level_name(r.level), r.step, c, s, told);
      told = r.level;
    } else if (readable && now != r.level) {
      // Someone else (thermalmonitord, from sensors that are not there) set another level: the host's stands.
      if (!overwritten_said) say("ONEIOS-GUEST thermal overwritten state=%llu: set back to %u (said once)", (unsigned long long)now, r.level);
      overwritten_said = 1;
      (void)state_set(r.level);
    }
  }
  return 0;
}
