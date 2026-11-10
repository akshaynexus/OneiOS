// SPDX-License-Identifier: MIT
// The brightness bridge: iOS's backlight level on the Galaxy S24 Ultra's panel, and the phone's
// ambient light sensor for iOS's auto-brightness. README.md here says why each route was chosen.
//
// Panel: the backlight class device of Samsung's panel driver (/sys/class/backlight/panel), written
// as root. It needs neither system_server nor surfaceflinger. Light sensor: the evdev device
// "als_rear" (STK6D2x), read without EVIOCGRAB.
#ifndef ONEIOS_BRIDGE_BRIGHTNESS_H
#define ONEIOS_BRIDGE_BRIGHTNESS_H

#include <stdbool.h>
#include <stdint.h>

#define BR_BRIGHT_FULL 65535u             // the level scale of the bridge channel (struct ob_bright_set)

// ---- the curve (pure) ---------------------------------------------------------------------------
// The slider's top on the panel's scale. Samsung's node goes past the manual range when it also
// carries the high-brightness levels (max_brightness 510 = 2 x 255, 51000 = 2 x 25500): those are
// left to One UI, and the slider ends at the manual maximum.
uint32_t br_bright_ceiling(uint32_t max_brightness);
// iOS's level (0..BR_BRIGHT_FULL) to the node's value in floor..ceiling. A perceived level (the
// slider's "brightness") is squared, as on the Mi A3 port; a linear one (a fraction of the nits) is
// taken as it is. Level 0 gives the floor, never 0: the panel is not turned off from here.
uint32_t br_bright_map(uint32_t level, bool linear, uint32_t floor, uint32_t ceiling);
// Where a fade from `from` to `to` is after elapsed_ms of fade_ms (both on the level scale).
uint32_t br_bright_fade_at(uint32_t from, uint32_t to, uint32_t elapsed_ms, uint32_t fade_ms);

// ---- the light sensor's events (pure) -----------------------------------------------------------
// One report of the STK6D2x driver (drivers/optics/stk6d2x_sec.c, stk_sec_report): the averages of
// the clear, infrared and UV channels, already divided by the gain, each sent as value + 1 on
// EV_REL (REL_RY clear, REL_X infrared, REL_RX UV), then the gains on EV_ABS and EV_SYN.
typedef struct {
  uint32_t clear, ir, uv;                 // counts (the event's value - 1)
  uint32_t clear_gain, ir_gain, uv_gain;  // as last reported (value - 1); 0 = not seen
  bool     have_clear;
} br_als_frame;
// Takes one input event. True when it ends a report that carried a clear value.
bool br_als_feed(br_als_frame *f, uint16_t type, uint16_t code, int32_t value);
// Clear counts to millilux. millilux_per_count is the calibration (README.md); saturates at 2^32-1.
uint32_t br_als_millilux(const br_als_frame *f, uint32_t millilux_per_count);
// Whether a new value is worth a message to the guest: a change of a tenth (or 1 lux), not more
// often than every 200 ms; a repeat every 2 s.
bool br_als_changed(uint32_t last_millilux, uint32_t now_millilux, uint32_t ms_since_last);

// ---- the session --------------------------------------------------------------------------------
typedef struct {
  const char *backlight_dir;              // NULL: /sys/class/backlight
  const char *input_dir;                  // NULL: /dev/input
  const char *input_sysfs;                // NULL: /sys/class/input
  const char *als_name;                   // NULL: als_rear
  uint32_t ceiling;                       // 0: br_bright_ceiling(max_brightness)   (ONEIOS_BRIGHT_MAX)
  uint32_t floor;                         // 0: 1                                   (ONEIOS_BRIGHT_MIN)
  uint32_t max_nits;                      // 0: 500, what iOS is told the top is    (ONEIOS_BRIGHT_NITS)
  uint32_t als_millilux_per_count;        // 0: 1000, uncalibrated                  (ONEIOS_ALS_SCALE)
  bool     no_als;
} br_bright_options;

typedef struct { uint32_t millilux, clear, ir, uv; } br_als_sample;
typedef void (*br_als_fn)(void *ctx, const br_als_sample *sample);

typedef struct {
  bool     panel, als;
  char     node[200];                     // the brightness file
  char     als_node[200];                 // the event device
  uint32_t max_brightness, floor, ceiling;
  uint32_t saved;                         // One UI's value, read at open and written back at close
  uint32_t max_nits;
} br_bright_info;

typedef struct br_bright br_bright;

// Opens the panel's node and the light sensor. options NULL: the phone's paths, with the environment
// variables above. NULL (with a log line) when neither is there. The light sensor is turned on
// through its `enable` file if it was off, and turned off again at close.
br_bright *br_bright_open(const br_bright_options *options);
void br_bright_info_get(const br_bright *b, br_bright_info *out);
// Never blocks: a worker writes the node, in steps of 16 ms when fade_ms is not 0.
void br_bright_set(br_bright *b, uint32_t level, bool linear, uint32_t fade_ms);
// The node's value written last (the saved one before the first set).
uint32_t br_bright_current(br_bright *b);
// Where the panel is now on the slider's scale (the squared curve undone), 0..BR_BRIGHT_FULL.
uint32_t br_bright_level(br_bright *b);
// fn runs on the sensor's thread. One listener; NULL removes it.
void br_bright_listen(br_bright *b, br_als_fn fn, void *ctx);
// Writes One UI's value back, turns the sensor off if this session turned it on. Within 300 ms.
void br_bright_close(br_bright *b);

#endif
