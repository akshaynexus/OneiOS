// SPDX-License-Identifier: MIT
// The OneiOS bridge channel: one virtio-mmio device ("oneios,bridge" in /arm-io) that carries small
// messages between the guest drivers in drivers/ and the host bridges in runtime/bridges/.
// Freestanding C, included unchanged by the kernel side and by xnuhost.
//
// Queue 0 (guest to host): one readable buffer per message. Queue 1 (host to guest): the guest posts
// writable buffers of OB_MAX_MESSAGE bytes; the host fills one per message. A message is this header
// and `length` payload bytes. All values little-endian.
#ifndef ONEIOS_BRIDGE_PROTO_H
#define ONEIOS_BRIDGE_PROTO_H

#include <stdint.h>

#define OB_VIRTIO_DEVICE_ID 0x4f42u      // not a registered VirtIO id; the guest matches the node, not the id
#define OB_PROTO_VERSION    1u
#define OB_MAX_MESSAGE      4096u
#define OB_MAX_PAYLOAD      (OB_MAX_MESSAGE - 8u)
#define OB_QUEUE_TX         0u           // guest to host
#define OB_QUEUE_RX         1u           // host to guest

struct ob_header {
  uint16_t channel;                      // OB_CH_*
  uint16_t type;                         // per channel
  uint32_t length;                       // payload bytes after the header
};

enum { OB_CH_CONTROL = 0, OB_CH_HAPTIC = 1, OB_CH_HCI = 2, OB_CH_CAMERA = 3, OB_CH_BRIGHTNESS = 4, OB_CH_BATTERY = 5, OB_CH_THERMAL = 6, OB_CH_COUNT = 7 };

// ---- control ----------------------------------------------------------------------------------
// guest -> host OB_CTL_HELLO { u32 version }; host -> guest OB_CTL_HELLO { u32 version, u32 channels }
// where bit n of channels says the host serves channel n.
// host -> guest OB_CTL_SHUTDOWN (no payload): the return asked for a graceful shutdown. The guest should begin its
// normal power-off (so iOS saves what a clean shutdown saves) and then halt; xnuhost waits up to return-graceful-ms
// (docs/booter.md "The return") and kills regardless. A guest that does nothing is simply killed at the limit, as
// before, so the way back is never at risk.
enum { OB_CTL_HELLO = 1, OB_CTL_SHUTDOWN = 2 };
struct ob_ctl_hello { uint32_t version, channels; };

// ---- haptics (guest -> host only) -------------------------------------------------------------
enum {
  OB_HAPTIC_VIB = 1,                     // struct ob_haptic_vib: vibrate from now; on_ms 0 stops
  OB_HAPTIC_STOP = 2,                    // no payload
};
struct ob_haptic_vib {
  uint32_t on_ms;                        // 0 = stop
  uint16_t intensity;                    // 0..1000 (iOS's 0.0..1.0)
  uint16_t frequency_hz;                 // 0 = the actuator's own (resonance)
};

// ---- Bluetooth HCI ----------------------------------------------------------------------------
// Types 1..5 are the H4 packet indicators and carry one whole HCI packet (without the indicator).
enum {
  OB_HCI_COMMAND = 1, OB_HCI_ACL = 2, OB_HCI_SCO = 3, OB_HCI_EVENT = 4, OB_HCI_ISO = 5,
  OB_HCI_OPEN = 16,                      // guest -> host: power the controller, no payload
  OB_HCI_CLOSE = 17,                     // guest -> host: release it
  OB_HCI_STATE = 18,                     // host -> guest: struct ob_hci_state
};
enum { OB_HCI_STATE_OFF = 0, OB_HCI_STATE_READY = 1, OB_HCI_STATE_FAILED = 2 };
struct ob_hci_state { uint32_t state; uint8_t bd_addr[6]; uint8_t reserved[2]; };

// ---- brightness (drivers/brightness, runtime/bridges/brightness) ------------------------------
enum {
  OB_BRIGHT_SET = 1,                     // guest -> host: struct ob_bright_set
  OB_BRIGHT_QUERY = 2,                   // guest -> host: no payload; the host answers OB_BRIGHT_STATE
  OB_BRIGHT_STATE = 3,                   // host -> guest: struct ob_bright_state
  OB_BRIGHT_ALS = 4,                     // host -> guest: struct ob_bright_als, when the light changes and every 2 s
  OB_BRIGHT_POWER = 5,                   // guest -> host: struct ob_bright_power, iOS's display on or off (its framebuffer's power request)
};
enum { OB_BRIGHT_LINEAR = 1u };          // ob_bright_set.flags: the level is a fraction of the light, not of the slider
enum { OB_BRIGHT_HAS_PANEL = 1u, OB_BRIGHT_HAS_ALS = 2u };      // ob_bright_state.flags
struct ob_bright_set {
  uint32_t level;                        // 0..65535 of the panel's range for iOS; 0 is the lowest level, not off
  uint32_t fade_ms;                      // the host walks to the level in this time; 0 = at once
  uint32_t flags;                        // OB_BRIGHT_LINEAR
};
struct ob_bright_state {
  uint32_t flags;                        // OB_BRIGHT_HAS_*
  uint32_t max_nits;                     // the light at level 65535
  uint32_t min_millinits;                // the light at level 0
  uint32_t level;                        // where the panel is now on the slider's scale, 0..65535
};
// The touch IC sleeps while the display is off and wakes when it is on; a brightness of 0 alone is a dim panel, not off.
enum { OB_POWER_FRAMEBUFFER = 1u };     // ob_bright_power.source: IOMobileFramebufferRequestPowerChange (selector 12)
struct ob_bright_power {
  uint32_t on;                           // 0 off, 1 on
  uint32_t source;                       // OB_POWER_*
};
struct ob_bright_als {
  uint32_t millilux;
  uint32_t clear, ir, uv;                // the sensor's channel counts
};

// ---- battery (drivers/battery, runtime/bridges/battery) ---------------------------------------
enum {
  OB_BATTERY_QUERY = 1,                  // guest -> host: no payload; the host answers OB_BATTERY_STATE, then reports changes
  OB_BATTERY_STATE = 2,                  // host -> guest: struct ob_battery_state
};
enum { OB_BATTERY_PRESENT = 1u, OB_BATTERY_PLUGGED = 2u, OB_BATTERY_CHARGING = 4u, OB_BATTERY_FULL = 8u };   // ob_battery_state.flags
#define OB_BATTERY_UNKNOWN INT32_MIN     // temp_deci_c, current_ma
struct ob_battery_state {
  uint32_t flags;                        // OB_BATTERY_*
  uint32_t percent;                      // 0..100
  int32_t  temp_deci_c;                  // tenths of a degree Celsius, or OB_BATTERY_UNKNOWN
  uint32_t voltage_mv;                   // 0 = unknown
  int32_t  current_ma;                   // into the battery > 0, out of it < 0, or OB_BATTERY_UNKNOWN
  uint32_t full_mah, design_mah;         // never 0
  uint32_t cycles;                       // 0 = unknown
  int32_t  minutes;                      // to empty, or to full while charging; -1 = unknown
};

// ---- thermal (drivers/thermal, runtime/bridges/thermal) ---------------------------------------
enum {
  OB_THERMAL_QUERY = 1,                  // guest -> host: no payload; the host answers OB_THERMAL_STATE, then reports changes
  OB_THERMAL_STATE = 2,                  // host -> guest: struct ob_thermal_state, at once for a new level, else every 30 s
};
#define OB_THERMAL_UNKNOWN INT32_MIN     // a reading the phone does not have
enum { OB_THERMAL_BY_CASE = 1u, OB_THERMAL_BY_SOC = 2u, OB_THERMAL_BY_BATTERY = 4u };      // ob_thermal_state.reason
struct ob_thermal_state {
  uint32_t level;                        // iOS's thermal pressure level (OSThermalNotification.h): 0 nominal, 10 light,
                                         // 20 moderate, 30 heavy, 40 trapping, 50 sleeping
  uint32_t reason;                       // OB_THERMAL_BY_* of the readings that set it
  uint32_t host_step;                    // what xnuhost does about it: 0 nothing, 1 vCPUs off the fastest cores,
                                         // 2 on the slowest P cores and the E cores, 3 on the E cores only
  int32_t  case_temp, soc, cpu, gpu, ddr, xo, board, battery;   // tenths of a degree C, or OB_THERMAL_UNKNOWN
};
#endif
