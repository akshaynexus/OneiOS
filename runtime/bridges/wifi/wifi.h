// SPDX-License-Identifier: MIT
// xnuhost's Wi-Fi bridge (docs/hardware.md section 3, README.md here): the guest's en0 on the phone's wlan0.
//   frames.c      which frames cross, and GRO frames cut back to the link's size
//   station.c     the station the guest's driver talks to (drivers/net/oneios_wifi_proto.h), over a radio back end
//   wlan.c        the back end on the phone: AF_PACKET on wlan0, nl80211 for scans and the link, wlan0's host state
//                 kept and given back
//   supplicant.c  joins: the vendor's wpa_supplicant through its AIDL service (aidl_parcel.c, aidl_binder.c)
#ifndef ONEIOS_BRIDGES_WIFI_H
#define ONEIOS_BRIDGES_WIFI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "oneios_wifi_proto.h"
#include "bridges/bridges.h"
#include "bridges/devices.h"

#define BR_WIFI_FRAME 1514u
#define BR_WIFI_CONTROL_TYPE OW_ETHERTYPE

// ---- frames.c ----
uint16_t br_wifi_ethertype(const uint8_t *frame, size_t length);
// A frame of the guest's that may go on the air: not EAPOL, not the bridge's own type, sent with the radio's address.
bool br_wifi_to_radio(const uint8_t *frame, size_t length, const uint8_t mac[6]);
// A frame of the radio's that the guest gets: to its address, broadcast or multicast; not EAPOL.
bool br_wifi_to_guest(const uint8_t *frame, size_t length, const uint8_t mac[6]);
// A frame for the guest: as it is when it fits the link and the receive path did not coalesce it; a coalesced TCP
// frame (IPv4 or IPv6) cut into segments of at most mss payload bytes with their checksums made again. fn gets each
// frame. Returns the frames given (0: the frame was too long and not TCP: dropped).
typedef void (*br_wifi_segment_fn)(void *ctx, const uint8_t *frame, size_t length);
size_t br_wifi_segment(const uint8_t *frame, size_t length, uint16_t mss, bool coalesced, br_wifi_segment_fn fn, void *ctx);

// ---- station.c ----
// What the radio back end reports about the link.
typedef struct {
  bool     up;
  int32_t  rssi_dbm, noise_dbm;
  uint32_t tx_rate_mbps, freq_mhz, reason;
  uint8_t  bssid[6];
} br_wifi_link;

typedef struct {
  struct ow_bss bss;
  uint8_t ies[OW_MAX_IES];
} br_wifi_bss;

// The radio, as the station uses it. Each call returns at once; scan and join answer later through
// br_wifi_scan_add/br_wifi_scan_done and br_wifi_join_done.
typedef struct {
  int  (*scan)(void *ctx);
  int  (*join)(void *ctx, const struct ow_join *join, const br_wifi_bss *bss);   // bss: the last scan's record, or NULL
  int  (*leave)(void *ctx);
  int  (*link)(void *ctx, br_wifi_link *out);    // 0 and *out, or -errno
} br_wifi_radio;

typedef struct br_wifi_station br_wifi_station;
// to_guest gets each frame for the guest (the station's messages, as frames of the control type).
typedef void (*br_wifi_guest_fn)(void *ctx, const uint8_t *frame, size_t length);
br_wifi_station *br_wifi_station_create(const br_wifi_radio *radio, void *radio_ctx, const uint8_t mac[6], const char country[2],
                                        br_wifi_guest_fn to_guest, void *guest_ctx);
void br_wifi_station_destroy(br_wifi_station *s);
// A control frame from the guest (br_wifi_ethertype == BR_WIFI_CONTROL_TYPE).
void br_wifi_station_message(br_wifi_station *s, const uint8_t *frame, size_t length);
void br_wifi_scan_add(br_wifi_station *s, const struct ow_bss *bss, const uint8_t *ies, size_t ie_length);
void br_wifi_scan_done(br_wifi_station *s, int error);
void br_wifi_join_done(br_wifi_station *s, uint32_t status, uint32_t code);
// The link changed (or a periodic look): the guest gets OW_STATE.
void br_wifi_link_changed(br_wifi_station *s);
// What the station sends for a state (for the tests and the tool).
size_t br_wifi_state_frame(br_wifi_station *s, uint8_t *out, size_t out_size);


// ---- wlan.c ----
// br_wlan_prepare, br_wlan_send, br_wlan_close and br_wlan_restore: bridges/devices.h (xnuhost uses them).
// One scan on the interface, for the tool: fn gets each network. The number of networks, or -errno.
typedef void (*br_wlan_bss_fn)(void *ctx, const struct ow_bss *bss, const uint8_t *ies, size_t ie_length);
int  br_wlan_scan_once(const char *ifname, unsigned wait_ms, br_wlan_bss_fn fn, void *ctx);

#endif
