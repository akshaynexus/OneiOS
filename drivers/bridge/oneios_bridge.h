// SPDX-License-Identifier: MIT
// Kernel-side API of the OneiOS bridge channel (OneiOSBridge.c): how a guest driver in drivers/
// reaches its host bridge in xnuhost. Plain C, usable from the Mach, BSD and IOKit universes.
#ifndef ONEIOS_BRIDGE_H
#define ONEIOS_BRIDGE_H

#include <stdbool.h>
#include <stdint.h>
#include "oneios_bridge_proto.h"

#ifdef __cplusplus
extern "C" {
#endif

// Starts every driver of drivers/ (the transport, then haptics and Bluetooth). Called once by the
// board's late probe, after IOKit is up (release-kernel wires it; drivers/README.md).
void oneios_drivers_start(void);

// Brings the "oneios,bridge" device up. Safe to call more than once.
void oneios_bridge_start(void);
// True once the device is up and the host has answered the hello.
bool oneios_bridge_ready(void);
// Bit n: the host serves channel n. 0 until ready.
uint32_t oneios_bridge_channels(void);
// Sends one message. Any thread context that may take a mutex. 0, or -1 (not up, too long, queue full).
int oneios_bridge_send(uint16_t channel, uint16_t type, const void *payload, uint32_t length);
// Messages from the host for `channel` are handed to fn on the bridge's worker thread.
typedef void (*oneios_bridge_handler)(void *context, uint16_t type, const uint8_t *payload, uint32_t length);
void oneios_bridge_listen(uint16_t channel, oneios_bridge_handler fn, void *context);

// drivers/haptics
int  oneios_haptic_start(void);
// Vibrate from now for ms (0 = stop) at intensity 0..1000 and frequency_hz (0 = resonance).
int  oneios_haptic_vib(uint32_t ms, uint32_t intensity, uint32_t frequency_hz);
void oneios_haptic_make_node(void);
int  oneios_haptic_publish_closed_loop(void);
// The Actuator IOAudio2Device of the audio archive (DroidVMAudio.cpp as staged): ready = may publish it;
// published = its decision, once; running = its StartIO/StopIO; play = one engine period, returns the frames taken.
bool oneios_haptic_actuator_ready(void);
void oneios_haptic_actuator_published(bool yes);
void oneios_haptic_actuator_running(bool on);
uint32_t oneios_haptic_actuator_play(const int16_t *frames, uint32_t count, uint32_t channels);
// What a user client of the Actuator device did, for its log lines and the counter line: OH_EV_OPEN, OH_EV_CLOSE,
// OH_EV_SELECTOR (a = the selector), OH_EV_REFUSED (a = the selector), OH_EV_FORMAT (a = the channels asked |
// the device's << 16, b = the rate asked in Hz), OH_EV_MAP (a = the memory type). The values are DroidVMAudio.cpp's
// as staged (tools/oneios-tool/crates/oneios-kernel/src/staged/audio.rs), which has no header of ours.
enum { OH_EV_OPEN = 0, OH_EV_CLOSE = 1, OH_EV_SELECTOR = 2, OH_EV_REFUSED = 3, OH_EV_FORMAT = 4, OH_EV_MAP = 5 };
void oneios_haptic_actuator_event(uint32_t kind, uint32_t a, uint32_t b);
void oneios_vibrator_start(void);
void oneios_vibrator_key(int key, int down);      // 0 power, 1 volume up, 2 volume down
// The stand-in for the always-on processor's button haptics service (OneiOSButtonHaptics.cpp; oneios-button-haptics=1).
void oneios_button_haptics_start(void);

// drivers/bluetooth
void oneios_bt_start(void);

// drivers/brightness
void oneios_backlight_start(void);

// drivers/battery
void oneios_battery_start(void);

// drivers/thermal
void oneios_thermal_start(void);

// drivers/camera (its own virtio device, "oneios,camera"; camera/oneios_camera.h)
void oneios_camera_start(void);
// The host's last ambient light reading. False until one has come.
bool oneios_als_read(uint32_t *millilux);
// fn gets every reading, on the bridge's worker thread. One listener; NULL removes it.
typedef void (*oneios_als_handler)(void *context, uint32_t millilux);
void oneios_als_listen(oneios_als_handler fn, void *context);

#ifdef __cplusplus
}
#endif
#endif
