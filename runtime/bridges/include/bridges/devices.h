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
// The same with a back end that is not net/net.c's: net_send takes the guest's frames (NULL: br_net_send) and gets
// what net_open returned. The phone's Wi-Fi: br_wlan_prepare's open and br_wlan_send (wifi/wifi.h).
typedef int (*br_net_send_fn)(br_net *n, const uint8_t *frame, size_t length);
xh_virtio *br_virtio_net_create_backend(uint64_t base, unsigned spi, const br_net_config *config, br_net_open_fn net_open, br_net_send_fn net_send);

// The phone's Wi-Fi as that back end (wifi/wlan.c; docs/hardware.md section 3): wlan0 is iOS's for the session.
// br_wlan_prepare takes the interface (state_dir/wifi.state keeps the two settings it changes: IPv6 and GRO) and
// gives its address, which the guest's en0 must use (config->guest_mac). NULL: no such interface or no access: iOS
// gets no Wi-Fi. Then br_virtio_net_create_backend(base, spi, config, the returned open, br_wlan_send).
br_net_open_fn br_wlan_prepare(const char *ifname, const char *state_dir, uint8_t mac_out[6]);
int  br_wlan_send(br_net *n, const uint8_t *frame, size_t length);
// The stop: the network left, the supplicant as it was found, wlan0's settings back.
void br_wlan_close(void);
// wlan0's settings back from a state file (`wifictl restore`, after a killed xnuhost). 0, -ENOENT (nothing to give
// back) or -errno.
int  br_wlan_restore(const char *state_path);
// A made-up radio for a boot with no phone (wifi/sim.c): two networks the guest can scan and join, no frames. mac:
// the guest's en0 address. Used like br_wlan_prepare, with br_wifi_sim_send.
br_net_open_fn br_wifi_sim_prepare(const uint8_t mac[6]);
int  br_wifi_sim_send(br_net *n, const uint8_t *frame, size_t length);

// "droidvm,virtio-snd": virtio-snd with one output stream (48 kHz S16 stereo) and one input stream
// (48 kHz S16 mono), 240-frame periods, as DroidVMVirtioSnd.c asks.
xh_virtio *br_virtio_snd_create(uint64_t base, unsigned spi, const br_audio_backend *backend, void *backend_ctx);

#endif
