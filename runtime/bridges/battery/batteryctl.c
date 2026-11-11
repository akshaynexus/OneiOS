// SPDX-License-Identifier: MIT
// batteryctl: what the battery bridge reads on this phone. Run as root with One UI up; reads only.
//   batteryctl            the sample once, and what iOS would be told
//   batteryctl watch [S]  a line whenever the sample changes, for S seconds (default 60): plug and unplug
#define _GNU_SOURCE
#include "battery.h"
#include "bridges/bridges.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void print(const char *what, const br_battery_sample *s)
{
  printf("%s: %u%% present=%d plugged=%d charging=%d full=%d temp=%d (tenths C) voltage=%u mV current=%d mA full=%u mAh design=%u mAh cycles=%u minutes=%d\n",
         what, s->percent, s->present, s->plugged, s->charging, s->full, s->temp_deci_c, s->voltage_mv, s->current_ma, s->full_mah, s->design_mah, s->cycles, s->minutes);
  fflush(stdout);
}

static void changed(void *ctx, const br_battery_sample *s) { (void)ctx; print("change", s); }

int main(int argc, char **argv)
{
  bool watch = argc > 1 && !strcmp(argv[1], "watch");
  int seconds = watch && argc > 2 ? atoi(argv[2]) : 60;
  br_battery *b = br_battery_open(NULL);
  if (!b) { puts("batteryctl: failed: no battery node"); return 1; }
  br_battery_sample s;
  (void)br_battery_query(b, &s);
  print("sample", &s);
  if (watch) {
    br_battery_listen(b, changed, NULL);
    sleep(seconds > 0 ? (unsigned)seconds : 60u);
    br_battery_listen(b, NULL, NULL);
  }
  br_battery_close(b);
  puts("batteryctl: ok");
  return 0;
}
