// SPDX-License-Identifier: MIT
// The battery bridge: the phone's real battery for iOS. Reads Android's power_supply class under
// /sys/class/power_supply and hands a normalized sample to xnuhost's bridge device, which tells
// drivers/battery (iOS's IOPMPowerSource). Read only. README.md here has the nodes and the mapping.
#ifndef ONEIOS_BRIDGE_BATTERY_H
#define ONEIOS_BRIDGE_BATTERY_H

#include <stdbool.h>
#include <stdint.h>

#define BR_BATTERY_ABSENT INT64_MIN       // a node that was not there, or not a number
#define BR_BATTERY_POLL_MS 3000u          // how often the nodes are read while the guest listens
#define BR_BATTERY_TELEMETRY_MS 30000u    // temperature, voltage, current, time: at most this often
#define BR_BATTERY_UNKNOWN INT32_MIN      // br_battery_sample.temp_deci_c, .current_ma
#define BR_BATTERY_MINUTES_UNKNOWN (-1)
#define BR_BATTERY_DEFAULT_MAH 5000u      // the S24 Ultra's rated capacity, when no node gives one
#define BR_BATTERY_MAX_SUPPLIES 8u

// The nodes as read, each BR_BATTERY_ABSENT when missing.
typedef struct {
  char    status[24];                     // "Charging", "Discharging", "Full", "Not charging"; "" when missing
  int64_t capacity, present, temp, voltage, current, charge_full, charge_full_design, cycles;
  bool    plugged;                        // some supply of the list has online > 0
} br_battery_raw;

// Sticky between samples: the unit of current_now is learned from its size (battery.c).
typedef struct { bool current_ua, current_fixed; } br_battery_units;

typedef struct {
  uint32_t percent;                       // 0..100
  bool     present, plugged, charging, full;
  int32_t  temp_deci_c;                   // tenths of a degree; BR_BATTERY_UNKNOWN
  uint32_t voltage_mv;                    // 0 = unknown
  int32_t  current_ma;                    // into the battery > 0, out of it < 0; BR_BATTERY_UNKNOWN
  uint32_t full_mah, design_mah;          // never 0
  uint32_t cycles;                        // 0 = unknown
  int32_t  minutes;                       // to empty, or to full while charging; BR_BATTERY_MINUTES_UNKNOWN
} br_battery_sample;

// ---- pure ---------------------------------------------------------------------------------------
void br_battery_map(const br_battery_raw *raw, br_battery_units *units, uint32_t default_mah, br_battery_sample *out);
// Whether a new sample is worth a message: at once for the level and the charging state, the rest
// when ms_since_sent has reached BR_BATTERY_TELEMETRY_MS.
bool br_battery_changed(const br_battery_sample *sent, const br_battery_sample *now, uint32_t ms_since_sent);

// ---- sysfs --------------------------------------------------------------------------------------
// dir/battery/{capacity,status,...} and dir/<supply>/online for each of `supplies`, ended by NULL.
// 0, or -1 when there is no readable capacity.
int br_battery_read_raw(const char *dir, const char *const *supplies, br_battery_raw *raw);

// ---- the session --------------------------------------------------------------------------------
typedef struct {
  const char *dir;                        // NULL: /sys/class/power_supply
  const char *supplies;                   // comma list; NULL: usb,ac,wireless,pogo (ONEIOS_BATTERY_SUPPLIES)
  uint32_t poll_ms;                       // 0: BR_BATTERY_POLL_MS
  uint32_t default_mah;                   // 0: BR_BATTERY_DEFAULT_MAH (ONEIOS_BATTERY_MAH)
  char     current_unit;                  // 'u' or 'm' fixes current_now's unit; 0 learns it (ONEIOS_BATTERY_CURRENT_UNIT)
} br_battery_options;

typedef void (*br_battery_fn)(void *ctx, const br_battery_sample *sample);
typedef struct br_battery br_battery;

// NULL (with a log line) when there is no battery to read. options NULL: the phone's paths and the
// environment variables above.
br_battery *br_battery_open(const br_battery_options *options);
// Reads the nodes now. The result counts as sent: the worker reports changes from it. 0, or -1 with
// the last good sample in out.
int br_battery_query(br_battery *b, br_battery_sample *out);
// fn runs on the worker thread, without the bridge's locks, for each change. NULL stops the reads.
void br_battery_listen(br_battery *b, br_battery_fn fn, void *ctx);
void br_battery_close(br_battery *b);

#endif
