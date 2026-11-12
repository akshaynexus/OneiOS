// SPDX-License-Identifier: MIT
// The thermal bridge: the phone's temperatures and a thermal pressure level for iOS, and the step by which
// xnuhost holds itself back. Reads Linux's thermal zones (/sys/class/thermal), Samsung's board thermistors
// (/sys/class/sec/sec-*-thermistor) and the battery's own sensors (/sys/class/power_supply/battery), read only.
// README.md here has the sensors, the levels and why.
//
// In a session One UI's system_server is gone, and with it Samsung's skin-temperature policy (SSRM). The kernel's
// own trips (the CPU and GPU junction at 90-110 C, LMh in hardware) keep running and cap every process, xnuhost's
// threads too. Nothing else acts on the case temperature: this bridge does, inside xnuhost only.
#ifndef ONEIOS_BRIDGE_THERMAL_H
#define ONEIOS_BRIDGE_THERMAL_H

#include <stdbool.h>
#include <stdint.h>

#define BR_THERMAL_UNKNOWN INT32_MIN      // a reading that is missing (all readings are tenths of a degree C)
#define BR_THERMAL_POLL_MS 5000u          // how often the sensors are read while someone listens
#define BR_THERMAL_REPORT_MS 30000u       // a sample is sent at least this often, and at once on a new level
#define BR_THERMAL_MAX_ZONES 96u

// iOS's thermal pressure levels (OSThermalNotification.h of the iPhoneOS SDK: kOSThermalPressureLevel*).
enum { BR_THERMAL_NOMINAL = 0, BR_THERMAL_LIGHT = 10, BR_THERMAL_MODERATE = 20, BR_THERMAL_HEAVY = 30, BR_THERMAL_TRAPPING = 40,
       BR_THERMAL_SLEEPING = 50 };
// Which rule set the level (br_thermal_state.reason).
enum { BR_THERMAL_BY_SKIN = 1u, BR_THERMAL_BY_SOC = 2u, BR_THERMAL_BY_BATTERY = 4u };

// One reading of every sensor, tenths of a degree C, BR_THERMAL_UNKNOWN when missing.
typedef struct {
  int32_t cpu, gpu, ddr, soc;             // the hottest CPU core or cluster zone, GPU zone, the DDR zone; soc = the hottest of the three
  char    soc_zone[24];                   // the zone that gave soc
  int32_t xo;                             // xo-therm: the crystal, near the SoC on the board
  int32_t ap;                             // sec-ap-thermistor: Samsung's board thermistor by the SoC
  int32_t battery;                        // power_supply/battery/temp
  int32_t lrp;                            // power_supply/battery/lrp: Samsung's case sensor, by which its charger limits current
  int32_t skin;                           // the case: lrp, or ap when there is no lrp
} br_thermal_raw;

// Thresholds, tenths of a degree C. skin[k] turns on LIGHT, MODERATE, HEAVY, TRAPPING, SLEEPING for k = 0..4;
// soc[k] and battery[k] turn on HEAVY and TRAPPING. A level stays until its reading is `*_hyst` below its threshold.
typedef struct {
  int32_t skin[5], skin_hyst;
  int32_t soc[2], soc_hyst;
  int32_t battery[2], battery_hyst;
} br_thermal_limits;

typedef struct {
  unsigned level;                         // BR_THERMAL_* (the highest of the three rules)
  unsigned reason;                        // BR_THERMAL_BY_* of the rules at that level
  unsigned skin_level, soc_level, battery_level;      // each rule's own level (its hysteresis follows it)
  unsigned cap;                           // xnuhost's step for the level: br_thermal_cap_for
  br_thermal_raw raw;
} br_thermal_state;

// ---- pure ---------------------------------------------------------------------------------------
void br_thermal_limits_default(br_thermal_limits *l);
// "390,420,440,460,500" into l->skin (and the same for soc, battery with two values). 0, or -1 (l unchanged).
int br_thermal_limits_parse(const char *text, int32_t *out, unsigned count);
// The next state from a reading and the previous state (NULL: nothing before).
void br_thermal_decide(const br_thermal_limits *l, const br_thermal_raw *raw, const br_thermal_state *prev, br_thermal_state *out);
// xnuhost's step for a level: 0 none; 1 (HEAVY) the fastest frequency domain left out; 2 (TRAPPING) only the slowest
// P domain and the E cores; 3 (SLEEPING) the E cores only. br_cpu_thermal_mask (bridges.h) turns a step into CPUs.
unsigned br_thermal_cap_for(unsigned level);
const char *br_thermal_level_name(unsigned level);
// Whether a new state is worth a message: a new level or cap at once, otherwise after BR_THERMAL_REPORT_MS.
bool br_thermal_changed(const br_thermal_state *sent, const br_thermal_state *now, uint32_t ms_since_sent);

// ---- sysfs --------------------------------------------------------------------------------------
typedef struct {
  char path[160];
  char type[24];
  uint8_t group;                          // 0 cpu, 1 gpu, 2 ddr, 3 xo
} br_thermal_zone;

typedef struct {
  char root[128];                         // "" on the phone; a fixture's directory in tests (prefixed to every path)
  br_thermal_zone zone[BR_THERMAL_MAX_ZONES];
  unsigned nzone;
} br_thermal_sensors;

// Finds the zones of interest under root/sys/class/thermal (cpu-*, cpuss-*, gpuss-*, ddr, xo-therm). 0, or -1 when no
// sensor of any kind can be read (no zone, no thermistor, no battery temperature).
int br_thermal_find(const char *root, br_thermal_sensors *s);
void br_thermal_read(const br_thermal_sensors *s, br_thermal_raw *raw);

// ---- the session --------------------------------------------------------------------------------
typedef struct {
  const char *root;                       // NULL: the phone's paths
  uint32_t poll_ms;                       // 0: BR_THERMAL_POLL_MS
  const br_thermal_limits *limits;        // NULL: the defaults and ONEIOS_THERMAL_SKIN, _SOC, _BATTERY
} br_thermal_options;

typedef void (*br_thermal_fn)(void *ctx, const br_thermal_state *state);
typedef struct br_thermal br_thermal;

// NULL (with a log line) when there is nothing to read. The worker runs from the start: the cap is the host's
// own protection and does not wait for the guest. options NULL: the phone and the environment.
br_thermal *br_thermal_open(const br_thermal_options *options);
// The last state (read at open, then by the worker).
void br_thermal_get(br_thermal *t, br_thermal_state *out);
// Two listeners: `cap` (xnuhost: runs on each new cap), `report` (the bridge device: on br_thermal_changed). fn runs on
// the worker thread without the bridge's locks. NULL stops that listener.
void br_thermal_on_cap(br_thermal *t, br_thermal_fn fn, void *ctx);
void br_thermal_on_report(br_thermal *t, br_thermal_fn fn, void *ctx);
void br_thermal_close(br_thermal *t);

#endif
