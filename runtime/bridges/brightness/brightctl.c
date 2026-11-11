// SPDX-License-Identifier: MIT
// brightctl: the brightness bridge on the phone, without xnuhost or a guest. As root, One UI running.
//   brightctl info                    the panel's node, its range, One UI's value, the light sensor
//   brightctl levels [--ms N] [L...]  sets each level (percent of iOS's slider, default 5 25 50 75 100
//                                     10) for N ms (default 2000), with a 300 ms fade, then restores
//   brightctl als [SECONDS]           live light sensor readings (default 15 s)
// Environment: ONEIOS_BRIGHT_MAX, ONEIOS_BRIGHT_MIN, ONEIOS_ALS_SCALE (brightness.h), and for a test
// off the phone ONEIOS_BRIGHT_DIR, ONEIOS_INPUT_DIR, ONEIOS_INPUT_SYSFS.
// It ends with "brightctl: ok" or the reason it failed. SIGINT and SIGTERM restore the panel too.
#define _GNU_SOURCE
#include "brightness.h"
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t stopped;
static void on_signal(int number) { (void)number; stopped = 1; }

static void wait_ms(unsigned ms)
{
  for (unsigned done = 0; done < ms && !stopped; done += 20) usleep(20000);
}

static br_bright *open_bridge(bool no_als)
{
  br_bright_options o = { 0 };
  const char *text;
  o.backlight_dir = getenv("ONEIOS_BRIGHT_DIR");
  o.input_dir = getenv("ONEIOS_INPUT_DIR");
  o.input_sysfs = getenv("ONEIOS_INPUT_SYSFS");
  if ((text = getenv("ONEIOS_BRIGHT_MAX"))) o.ceiling = (uint32_t)strtoul(text, NULL, 10);
  if ((text = getenv("ONEIOS_BRIGHT_MIN"))) o.floor = (uint32_t)strtoul(text, NULL, 10);
  if ((text = getenv("ONEIOS_ALS_SCALE"))) o.als_millilux_per_count = (uint32_t)strtoul(text, NULL, 10);
  o.no_als = no_als;
  o.touch_dir = getenv("ONEIOS_TSP_DIR") ? getenv("ONEIOS_TSP_DIR") : "/nonexistent";      // the phone test leaves the touch IC alone
  return br_bright_open(&o);
}

static void print_info(const br_bright_info *i)
{
  if (i->panel) printf("panel: %s  max_brightness %u, slider range %u..%u, One UI's value %u\n", i->node, i->max_brightness, i->floor, i->ceiling, i->saved);
  else printf("panel: no backlight device\n");
  if (i->als) printf("light sensor: %s\n", i->als_node);
  else printf("light sensor: none\n");
}

static void on_als(void *ctx, const br_als_sample *s)
{
  unsigned *count = ctx;
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  printf("%6ld.%03ld  clear %-8u ir %-8u uv %-8u -> %u.%03u lux\n", (long)(t.tv_sec % 100000), t.tv_nsec / 1000000, s->clear, s->ir, s->uv,
         s->millilux / 1000, s->millilux % 1000);
  fflush(stdout);
  (*count)++;
}

int main(int argc, char **argv)
{
  const char *command = argc > 1 ? argv[1] : "";
  setvbuf(stdout, NULL, _IOLBF, 0);
  signal(SIGINT, on_signal);
  signal(SIGTERM, on_signal);
  signal(SIGHUP, on_signal);

  if (!strcmp(command, "info")) {
    br_bright *b = open_bridge(false);
    if (!b) { printf("brightctl: failed: no backlight device and no light sensor\n"); return 1; }
    br_bright_info i;
    br_bright_info_get(b, &i);
    print_info(&i);
    br_bright_close(b);
    printf("brightctl: ok\n");
    return 0;
  }
  if (!strcmp(command, "levels")) {
    unsigned ms = 2000, percents[16], n = 0;
    for (int a = 2; a < argc; a++) {
      if (!strcmp(argv[a], "--ms") && a + 1 < argc) ms = (unsigned)atoi(argv[++a]);
      else if (n < 16) percents[n++] = (unsigned)atoi(argv[a]);
    }
    if (!n) { static const unsigned usual[] = { 5, 25, 50, 75, 100, 10 }; memcpy(percents, usual, sizeof usual); n = 6; }
    br_bright *b = open_bridge(true);
    br_bright_info i;
    if (b) br_bright_info_get(b, &i);
    if (!b || !i.panel) { printf("brightctl: failed: no writable backlight device\n"); if (b) br_bright_close(b); return 1; }
    print_info(&i);
    for (unsigned k = 0; k < n && !stopped; k++) {
      unsigned percent = percents[k] > 100 ? 100 : percents[k];
      uint32_t level = BR_BRIGHT_FULL * percent / 100;
      br_bright_set(b, level, false, 300);
      wait_ms(500);
      printf("slider %3u%% -> wanted %u, the node was written %u\n", percent, br_bright_map(level, false, i.floor, i.ceiling), br_bright_current(b));
      wait_ms(ms > 500 ? ms - 500 : 0);
    }
    br_bright_close(b);
    printf("restored %u\nbrightctl: %s\n", i.saved, stopped ? "stopped" : "ok");
    return 0;
  }
  if (!strcmp(command, "als")) {
    unsigned seconds = argc > 2 ? (unsigned)atoi(argv[2]) : 15, count = 0;
    br_bright *b = open_bridge(false);
    br_bright_info i;
    if (b) br_bright_info_get(b, &i);
    if (!b || !i.als) { printf("brightctl: failed: no light sensor (no input device named als_rear)\n"); if (b) br_bright_close(b); return 1; }
    print_info(&i);
    br_bright_listen(b, on_als, &count);
    wait_ms(seconds * 1000);
    br_bright_listen(b, NULL, NULL);
    br_bright_close(b);
    printf("%u readings in %u s\nbrightctl: %s\n", count, seconds, count ? "ok" : "failed: the sensor sent nothing (is its enable file writable?)");
    return count ? 0 : 1;
  }
  printf("usage: brightctl info | levels [--ms N] [PERCENT...] | als [SECONDS]\n");
  return 2;
}
