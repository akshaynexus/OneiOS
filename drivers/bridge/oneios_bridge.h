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
void oneios_vibrator_start(void);
void oneios_vibrator_key(int key, int down);      // 0 power, 1 volume up, 2 volume down

// drivers/bluetooth
void oneios_bt_start(void);

#ifdef __cplusplus
}
#endif
#endif
