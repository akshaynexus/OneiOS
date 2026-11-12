// SPDX-License-Identifier: MIT
// Host test of oneios_battery_props.h: the values OneiOSBattery publishes at registration, no change message for the
// host's first reading, and a real plug or unplug (or a level change) still told to powerd.
#include "oneios_battery_props.h"
#include <stdio.h>
#include <string.h>

static int failures, checks;
#define CHECK(c) do { checks++; if (!(c)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); } } while (0)

// The owner's S24 Ultra on USB (docs/hardware.md 7c, 2026-11-11 and the boot of 2026-11-12 23:45): Full at 100 %.
static struct ob_battery_state phone_on_usb(void)
{
  struct ob_battery_state s;
  memset(&s, 0, sizeof s);
  s.flags = OB_BATTERY_PRESENT | OB_BATTERY_PLUGGED | OB_BATTERY_FULL;
  s.percent = 100;
  s.temp_deci_c = 389;
  s.voltage_mv = 4294;
  s.current_ma = 74;
  s.full_mah = 5000;
  s.design_mah = 5000;
  s.cycles = 1103;
  s.minutes = -1;
  return s;
}

static void test_registration(void)
{
  struct oba_publisher pub;
  memset(&pub, 0, sizeof pub);
  struct ob_battery_state s = phone_on_usb();
  struct oba_props p;

  // The first reading is published whole, before registerService, and is not a change.
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_INITIAL);
  CHECK(p.installed && p.external && p.charge_capable && p.raw_external && !p.charging && p.full);
  CHECK(p.current == 100 && p.max == 100 && !p.warn && !p.critical && p.cycles == 1103);
  CHECK(p.time_remaining == OBA_TIME_UNKNOWN);
  CHECK(p.has_voltage && p.voltage == 4294 && p.has_amperage && p.amperage == 74 && p.has_temperature && p.temperature == 3890);
  CHECK(p.raw_current == 5000 && p.raw_max == 5000 && p.nominal == 5000 && p.design == 5000);

  // The same state again (the query's answer and the worker's next poll): nothing, so powerd hears nothing.
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_NOTHING);
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_NOTHING && p.external);

  // Unplugged: a change powerd must hear (ExternalConnected false: "Play Charging Chime" false, the mark cleared).
  s.flags = OB_BATTERY_PRESENT;
  s.current_ma = -310;
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_CHANGE);
  CHECK(!p.external && !p.charge_capable && !p.raw_external && !p.full && !p.charging && p.amperage == -310);
  // Plugged in again: a change, ExternalConnected true (the chime SpringBoard should play).
  s.flags = OB_BATTERY_PRESENT | OB_BATTERY_PLUGGED | OB_BATTERY_CHARGING;
  s.current_ma = 1800;
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_CHANGE);
  CHECK(p.external && p.charge_capable && p.raw_external && p.charging && !p.full);
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_NOTHING);

  // A level step and a telemetry step are changes; an unknown reading keeps the last published value.
  s.percent = 99;
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_CHANGE && p.current == 99 && p.raw_current == 4950);
  s.temp_deci_c = OB_BATTERY_UNKNOWN;
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_NOTHING && p.has_temperature && p.temperature == 3890);
  s.voltage_mv = 4300;
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_CHANGE && p.voltage == 4300 && p.has_temperature && p.temperature == 3890);
}

// A phone that boots on battery: ExternalConnected false from the start, and the first plug is a change.
static void test_on_battery(void)
{
  struct oba_publisher pub;
  memset(&pub, 0, sizeof pub);
  struct ob_battery_state s = phone_on_usb();
  struct oba_props p;
  s.flags = OB_BATTERY_PRESENT;
  s.percent = 8;
  s.current_ma = -400;
  s.minutes = 55;
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_INITIAL);
  CHECK(!p.external && !p.raw_external && p.warn && !p.critical && p.time_remaining == 55);
  s.percent = 2;
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_CHANGE && p.warn && p.critical);
  s.flags |= OB_BATTERY_PLUGGED | OB_BATTERY_CHARGING;
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_CHANGE && p.external && !p.warn && !p.critical);

  // Out-of-range numbers are clamped, never published as they came.
  memset(&pub, 0, sizeof pub);
  s.percent = 140;
  CHECK(oba_publish(&pub, &s, &p) == OBA_PUBLISH_INITIAL && p.current == 100 && p.raw_current == 5000);
}

int main(void)
{
  test_registration();
  test_on_battery();
  printf("battery props: %d checks, %d failed\n", checks, failures);
  return failures ? 1 : 0;
}
