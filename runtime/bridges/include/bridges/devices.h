// SPDX-License-Identifier: MIT
// The bridges' virtio devices on xnuhost's virtio-mmio transport (runtime/xnuhost/src/virtio.c).
// Each create call takes the window's base and its SPI, as xh_virtio_blk_create does, and returns
// NULL when the device could not be made. docs/hardware.md and the board ("bridges -> booter") say
// which device-tree node goes with each.
#ifndef ONEIOS_BRIDGE_DEVICES_H
#define ONEIOS_BRIDGE_DEVICES_H

#include "bridges/bridges.h"
#include "../../battery/battery.h"
#include "../../brightness/brightness.h"
#include "../../thermal/thermal.h"
#include "xnuhost.h"

// "oneios,bridge": the message channel of drivers/bridge (haptics, Bluetooth HCI).
typedef struct {
  br_haptic *haptic;                      // NULL: the haptic channel is not served
  // Makes the HCI controller when the guest first opens it (br_hci_open_hidl on the phone). NULL:
  // the HCI channel is not served.
  br_hci *(*hci_factory)(br_hci_rx_fn rx, br_hci_state_fn state, void *ctx);
  const char *bt_address_path;            // default /mnt/vendor/efs/bluetooth/bt_addr
  br_bright *brightness;                  // NULL: the brightness channel is not served (br_bright_open)
  br_battery *battery;                    // NULL: the battery channel is not served (br_battery_open)
  br_thermal *thermal;                    // NULL: the thermal channel is not served (br_thermal_open)
} br_bridge_config;
xh_virtio *br_bridge_create(uint64_t base, unsigned spi, const br_bridge_config *config);

// "droidvm,net": virtio-net for en0 (VERSION_1 + MAC, 12-byte header, no offloads).
// `net_open` makes the back end with the device's receive function (br_net_open_tap on the phone).
typedef br_net *(*br_net_open_fn)(const br_net_config *c, br_net_rx_fn rx, void *ctx);
xh_virtio *br_virtio_net_create(uint64_t base, unsigned spi, const br_net_config *config, br_net_open_fn net_open);

// "droidvm,virtio-snd": virtio-snd with one output stream (48 kHz S16 stereo) and one input stream
// (48 kHz S16 mono), 240-frame periods, as DroidVMVirtioSnd.c asks.
xh_virtio *br_virtio_snd_create(uint64_t base, unsigned spi, const br_audio_backend *backend, void *backend_ctx);

#endif
