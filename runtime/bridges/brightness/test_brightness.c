// SPDX-License-Identifier: MIT
// Host unit tests of the brightness bridge: the curve, the fade, the light sensor's events and its
// conversion, and the session on a fake backlight device (a folder). No phone.
#define _GNU_SOURCE
#include "brightness.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int checks, failures;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

// The kernel's codes, written out so the test does not depend on the host's headers.
enum { T_SYN = 0, T_REL = 2, T_ABS = 3, C_REL_X = 0, C_REL_RX = 3, C_REL_RY = 4, C_REL_RZ = 5, C_ABS_Y = 1, C_ABS_Z = 2, C_ABS_RX = 3 };

static void test_curve(void)
{
  // Samsung's doubled ranges end the slider at the manual maximum; any other range is used whole.
  CHECK(br_bright_ceiling(510) == 255 && br_bright_ceiling(51000) == 25500);
  CHECK(br_bright_ceiling(255) == 255 && br_bright_ceiling(25500) == 25500 && br_bright_ceiling(4095) == 4095);

  // The ends: 0 is the floor (never 0), full is the ceiling.
  CHECK(br_bright_map(0, false, 1, 255) == 1 && br_bright_map(BR_BRIGHT_FULL, false, 1, 255) == 255);
  CHECK(br_bright_map(0, true, 1, 25500) == 1 && br_bright_map(BR_BRIGHT_FULL, true, 1, 25500) == 25500);
  CHECK(br_bright_map(BR_BRIGHT_FULL + 99, false, 1, 255) == 255);          // clamped
  // Half the slider is a quarter of the light; half of a linear level is half.
  CHECK(br_bright_map(BR_BRIGHT_FULL / 2, false, 0, 25500) == 6375);
  CHECK(br_bright_map(BR_BRIGHT_FULL / 2, true, 0, 25500) == 12750);
  CHECK(br_bright_map(BR_BRIGHT_FULL / 2, false, 1, 255) == 64 || br_bright_map(BR_BRIGHT_FULL / 2, false, 1, 255) == 65);
  // Never falling, and inside the range, over the whole scale and for both ranges.
  for (unsigned linear = 0; linear < 2; linear++) {
    uint32_t last = 0;
    bool ok = true;
    for (uint32_t level = 0; level <= BR_BRIGHT_FULL; level += 5) {
      uint32_t v = br_bright_map(level, linear, 1, 25500);
      if (v < last || v < 1 || v > 25500) ok = false;
      last = v;
    }
    CHECK(ok);
  }
  // A range with nothing in it does not divide by zero.
  CHECK(br_bright_map(1000, false, 7, 7) == 7 && br_bright_map(1000, true, 9, 7) == 7);

  // The fade: a line from one level to the other, both ways, and no fade is a jump.
  CHECK(br_bright_fade_at(0, 60000, 0, 300) == 0 && br_bright_fade_at(0, 60000, 150, 300) == 30000 && br_bright_fade_at(0, 60000, 300, 300) == 60000);
  CHECK(br_bright_fade_at(60000, 0, 100, 300) == 40000 && br_bright_fade_at(60000, 0, 999, 300) == 0);
  CHECK(br_bright_fade_at(123, 456, 0, 0) == 456);
}

static void test_als(void)
{
  br_als_frame f = { 0 };
  // One report as stk_sec_report sends it: infrared, UV, clear (each + 1), the gains, the sync.
  CHECK(!br_als_feed(&f, T_REL, C_REL_X, 41));
  CHECK(!br_als_feed(&f, T_REL, C_REL_RX, 11));
  CHECK(!br_als_feed(&f, T_REL, C_REL_RY, 351));
  CHECK(!br_als_feed(&f, T_ABS, C_ABS_Y, 17));
  CHECK(!br_als_feed(&f, T_ABS, C_ABS_Z, 17));
  CHECK(br_als_feed(&f, T_SYN, 0, 0));
  CHECK(f.clear == 350 && f.ir == 40 && f.uv == 10 && f.clear_gain == 16 && f.ir_gain == 16);
  CHECK(br_als_millilux(&f, 1000) == 350000 && br_als_millilux(&f, 250) == 87500);
  // A sync without a clear value (the flush event, or the flicker frequency alone) is not a reading.
  CHECK(!br_als_feed(&f, T_SYN, 0, 0));
  CHECK(!br_als_feed(&f, T_REL, C_REL_RZ, 100) && !br_als_feed(&f, T_SYN, 0, 0));
  // Darkness is sent as 1 (the driver adds 1 so the event is not dropped): 0 counts, 0 lux.
  CHECK(!br_als_feed(&f, T_REL, C_REL_RY, 1) && br_als_feed(&f, T_SYN, 0, 0) && f.clear == 0 && br_als_millilux(&f, 1000) == 0);
  // A value that could not come from the driver does not go negative; a huge one saturates.
  CHECK(!br_als_feed(&f, T_REL, C_REL_RY, -5) && br_als_feed(&f, T_SYN, 0, 0) && f.clear == 0);
  f.clear = 0xffffffffu;
  CHECK(br_als_millilux(&f, 1000) == 0xffffffffu);

  // What is sent on: not faster than 5 a second, a tenth of change (1 lux in the dark), a repeat at 2 s.
  CHECK(!br_als_changed(100000, 500000, 50));
  CHECK(br_als_changed(100000, 111000, 250) && !br_als_changed(100000, 109000, 250) && br_als_changed(100000, 89000, 250));
  CHECK(br_als_changed(0, 1000, 250) && !br_als_changed(0, 900, 250) && !br_als_changed(2000, 2500, 250));
  CHECK(br_als_changed(100000, 100000, 2000));
}

static void put(const char *dir, const char *name, const char *text)
{
  char path[400];
  snprintf(path, sizeof path, "%s/%s", dir, name);
  FILE *f = fopen(path, "w");
  if (f) { fputs(text, f); fclose(f); }
}

static unsigned get(const char *dir, const char *name)
{
  char path[400];
  unsigned v = 0xffffffffu;
  snprintf(path, sizeof path, "%s/%s", dir, name);
  FILE *f = fopen(path, "r");
  if (f) { if (fscanf(f, "%u", &v) != 1) v = 0xffffffffu; fclose(f); }
  return v;
}

static void test_session(void)
{
  char tmp[] = "/tmp/oneios-bright-XXXXXX", panel[300], other[300], empty[300];
  CHECK(mkdtemp(tmp) != NULL);
  snprintf(other, sizeof other, "%s/aaa-led", tmp);
  snprintf(panel, sizeof panel, "%s/panel", tmp);
  snprintf(empty, sizeof empty, "%s/none", tmp);
  mkdir(other, 0755); mkdir(panel, 0755); mkdir(empty, 0755);
  put(other, "max_brightness", "100\n"); put(other, "brightness", "50\n");
  put(panel, "max_brightness", "51000\n"); put(panel, "brightness", "9000\n");

  br_bright_options o = { .backlight_dir = tmp, .no_als = true };
  br_bright *b = br_bright_open(&o);
  CHECK(b != NULL);
  if (b) {
    br_bright_info i;
    br_bright_info_get(b, &i);
    // Samsung's "panel" wins over another device, the slider ends at the manual maximum.
    CHECK(i.panel && !i.als && strstr(i.node, "/panel/brightness") && i.max_brightness == 51000 && i.ceiling == 25500 && i.floor == 1 && i.saved == 9000);
    CHECK(br_bright_current(b) == 9000);
    // Where One UI left the panel, on the slider's scale: the map of that level is the value again (to a step).
    uint32_t at = br_bright_map(br_bright_level(b), false, i.floor, i.ceiling);
    CHECK(at >= 8999 && at <= 9001);
    br_bright_set(b, BR_BRIGHT_FULL, false, 0);
    usleep(100000);
    CHECK(get(panel, "brightness") == 25500 && br_bright_current(b) == 25500 && br_bright_level(b) == BR_BRIGHT_FULL);
    br_bright_set(b, 0, false, 0);                                  // iOS's display off: the floor, not 0
    usleep(100000);
    CHECK(get(panel, "brightness") == 1);
    br_bright_set(b, BR_BRIGHT_FULL / 2, true, 0);
    usleep(100000);
    CHECK(get(panel, "brightness") == 12750);
    // A fade passes through values between the two and ends on the target.
    br_bright_set(b, BR_BRIGHT_FULL, true, 200);
    usleep(100000);
    unsigned mid = get(panel, "brightness");
    CHECK(mid > 12750 && mid < 25500);
    usleep(250000);
    CHECK(get(panel, "brightness") == 25500);
    // A fade cut short by the close still leaves One UI's value.
    br_bright_set(b, 0, false, 5000);
    usleep(50000);
    br_bright_close(b);
    CHECK(get(panel, "brightness") == 9000);
    CHECK(get(other, "brightness") == 50);                          // the other device was never touched
  }
  // The ceiling and the floor can be set; a ceiling past the device's range is not taken.
  o.ceiling = 30000; o.floor = 300;
  b = br_bright_open(&o);
  CHECK(b != NULL);
  if (b) {
    br_bright_info i;
    br_bright_info_get(b, &i);
    CHECK(i.ceiling == 30000 && i.floor == 300);
    br_bright_close(b);
  }
  o.ceiling = 99999; o.floor = 0;
  b = br_bright_open(&o);
  if (b) { br_bright_info i; br_bright_info_get(b, &i); CHECK(i.ceiling == 25500); br_bright_close(b); }
  // No device at all: no bridge.
  o.backlight_dir = empty; o.ceiling = 0;
  CHECK(br_bright_open(&o) == NULL);

  char command[400];
  snprintf(command, sizeof command, "rm -rf %s", tmp);
  CHECK(system(command) == 0);
}

int main(void)
{
  test_curve();
  test_als();
  test_session();
  printf("brightness: %d checks, %d failures\n", checks, failures);
  return failures ? 1 : 0;
}
