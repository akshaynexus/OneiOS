// SPDX-License-Identifier: MIT
// Host test of oneios_als_hid.h: each descriptor is walked as a HID parser does, and must describe exactly the reports
// oneios_als_report() and oneios_als_spu_report() make; the SPU shape's lux survives the trip through the event's fields.
#include "oneios_als_hid.h"
#include <stdio.h>

struct walk {
  unsigned usage_first, depth, inputs, features, input_bits, feature_bits, last_id, ids;
  int exponent;
  unsigned usages[16], nusages;
  int overrun;
};

static struct walk walk(const uint8_t *d, unsigned length)
{
  struct walk w = {0};
  unsigned page = 0, size = 0, count = 0;
  for (unsigned i = 0; i < length;) {
    unsigned prefix = d[i], n = (prefix & 3) == 3 ? 4 : prefix & 3;
    uint32_t value = 0;
    if (i + 1 + n > length) { w.overrun = 1; return w; }
    for (unsigned k = 0; k < n; k++) value |= (uint32_t)d[i + 1 + k] << (8 * k);
    switch (prefix & 0xfc) {
    case 0x04: page = value; break;
    case 0x08: {
      unsigned usage = page << 16 | value;
      if (!w.usage_first) w.usage_first = usage;
      if (w.nusages < 16) w.usages[w.nusages++] = usage;
      break;
    }
    case 0xa0: w.depth++; break;
    case 0xc0: w.depth--; break;
    case 0x84: w.last_id = value; w.ids++; break;
    case 0x54: w.exponent = value > 7 ? (int)value - 16 : (int)value; break;
    case 0x74: size = value; break;
    case 0x94: count = value; break;
    case 0x80: w.input_bits += size * count; w.inputs++; break;
    case 0xb0: w.feature_bits += size * count; w.features++; break;
    }
    i += 1 + n;
  }
  return w;
}

static int has(const struct walk *w, unsigned usage)
{
  for (unsigned i = 0; i < w->nusages; i++) if (w->usages[i] == usage) return 1;
  return 0;
}

int main(void)
{
  int failures = 0;
#define CHECK(c) do { if (!(c)) { failures++; printf("FAIL: %s\n", #c); } } while (0)
  // The HID Sensor page device of today (oneios-als-spu=0).
  struct walk w = walk(oneios_als_descriptor, sizeof oneios_als_descriptor);
  CHECK(!w.overrun);
  CHECK(w.usage_first == (0x20u << 16 | 0x41u));           // what AppleUSBALSService's personality matches
  CHECK(w.depth == 0 && w.inputs == 1 && w.last_id == ONEIOS_ALS_REPORT_ID && w.exponent == -3);
  CHECK(w.input_bits == 8 * (ONEIOS_ALS_REPORT_SIZE - 1));
  uint8_t report[ONEIOS_ALS_REPORT_SIZE];
  oneios_als_report(350000, report);
  CHECK(report[0] == 1 && report[1] == 0x30 && report[2] == 0x57 && report[3] == 0x05 && report[4] == 0);
  oneios_als_report(0xffffffffu, report);
  CHECK(report[4] == 0x7f && report[1] == 0xff);

  // The SPU shape (oneios-als-spu=1): {0xff00, 4} first, the accelerometer's three fields, the Report Interval feature.
  w = walk(oneios_als_spu_descriptor, sizeof oneios_als_spu_descriptor);
  CHECK(!w.overrun && w.depth == 0 && w.ids == 2);
  CHECK(w.usage_first == (ONEIOS_ALS_SPU_PAGE << 16 | ONEIOS_ALS_SPU_USAGE));
  CHECK(has(&w, 0x200073) && has(&w, 0x200453) && has(&w, 0x200454) && has(&w, 0x200455) && has(&w, 0x20030e));
  CHECK(w.exponent == 0 && w.inputs == 1 && w.features == 1);
  CHECK(w.input_bits == 8 * (ONEIOS_ALS_SPU_REPORT_SIZE - 1) && w.feature_bits == 8 * (ONEIOS_ALS_SPU_FEATURE_SIZE - 1));
  // No usage of the HID Sensor ALS (AppleUSBALSService's) nor of 0xff0c/3 (Apple's SPU gesture driver's pair).
  CHECK(!has(&w, 0x200041) && !has(&w, 0xff0c0003));
  uint8_t spu[ONEIOS_ALS_SPU_REPORT_SIZE];
  static const uint32_t millilux[] = {0, 999, 1000, 350000, 32767000, 32768000, 100000123, 120000000u, 0xffffffffu};
  for (unsigned i = 0; i < sizeof millilux / sizeof millilux[0]; i++) {
    oneios_als_spu_report(millilux[i], spu);
    unsigned x = spu[1] | spu[2] << 8, y = spu[3] | spu[4] << 8, z = spu[5] | spu[6] << 8;
    CHECK(spu[0] == ONEIOS_ALS_SPU_INPUT_ID && x <= 32767 && y <= 32767 && z <= 999);
    CHECK(oneios_als_spu_lux(x, y) == millilux[i] / 1000 && z == millilux[i] % 1000);
  }
  const uint8_t feature[] = {ONEIOS_ALS_SPU_FEATURE_ID, 0xa0, 0x86, 0x01, 0x00};   // 100000 us
  CHECK(oneios_als_spu_interval(feature, sizeof feature) == 100000);
  CHECK(oneios_als_spu_interval(feature, 4) == -1 && oneios_als_spu_interval(spu, sizeof spu) == -1);
  CHECK(oneios_als_spu_is_wake(0, 100000) && !oneios_als_spu_is_wake(100000, 50000) && !oneios_als_spu_is_wake(100000, 0));
  printf("drivers: light sensor HID descriptors (HID Sensor page and SPU) %s\n", failures ? "FAILED" : "ok");
  return failures ? 1 : 0;
}
