// SPDX-License-Identifier: MIT
// Host test of oneios_als_hid.h: the descriptor is walked as a HID parser does, and must describe
// exactly the report oneios_als_report() makes.
#include "oneios_als_hid.h"
#include <stdio.h>

int main(void)
{
  unsigned page = 0, usage_first = 0, report_id = 0, size = 0, count = 0, input_bits = 0, depth = 0, inputs = 0;
  int exponent = 0, failures = 0;
  const uint8_t *d = oneios_als_descriptor;
  for (unsigned i = 0; i < sizeof oneios_als_descriptor;) {
    unsigned prefix = d[i], n = (prefix & 3) == 3 ? 4 : prefix & 3;
    uint32_t value = 0;
    if (i + 1 + n > sizeof oneios_als_descriptor) { printf("FAIL: an item runs past the end\n"); return 1; }
    for (unsigned k = 0; k < n; k++) value |= (uint32_t)d[i + 1 + k] << (8 * k);
    switch (prefix & 0xfc) {
    case 0x04: page = value; break;
    case 0x08: if (!usage_first) usage_first = page << 16 | value; break;
    case 0xa0: depth++; break;
    case 0xc0: depth--; break;
    case 0x84: report_id = value; break;
    case 0x54: exponent = value > 7 ? (int)value - 16 : (int)value; break;
    case 0x74: size = value; break;
    case 0x94: count = value; break;
    case 0x80: input_bits += size * count; inputs++; break;
    }
    i += 1 + n;
  }
#define CHECK(c) do { if (!(c)) { failures++; printf("FAIL: %s\n", #c); } } while (0)
  CHECK(usage_first == (0x20u << 16 | 0x41u));           // what AppleUSBALSService's personality matches
  CHECK(depth == 0 && inputs == 1 && report_id == ONEIOS_ALS_REPORT_ID && exponent == -3);
  CHECK(input_bits == 8 * (ONEIOS_ALS_REPORT_SIZE - 1));
  uint8_t report[ONEIOS_ALS_REPORT_SIZE];
  oneios_als_report(350000, report);
  CHECK(report[0] == 1 && report[1] == 0x30 && report[2] == 0x57 && report[3] == 0x05 && report[4] == 0);
  oneios_als_report(0xffffffffu, report);
  CHECK(report[4] == 0x7f && report[1] == 0xff);
  printf("drivers: light sensor HID descriptor %s\n", failures ? "FAILED" : "ok");
  return failures ? 1 : 0;
}
