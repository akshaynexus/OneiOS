// SPDX-License-Identifier: MIT
// The Wi-Fi station's messages between the guest's Wi-Fi driver (kernel/patches/oneios-hosted/net/OneiOSWifi.cpp)
// and xnuhost's Wi-Fi bridge (runtime/bridges/wifi). They travel in-band on the virtio-net queues as Ethernet frames
// of their own type: the guest's frames of this type go to the bridge and never to the radio; the bridge's come
// into the driver and never reach iOS's network stack. One message per frame, at most OW_MAX_PAYLOAD bytes after
// the header. docs/hardware.md section 3.
//
// Frame to the host: dst ow_address (02:4f:57:49:46:49), src the guest's MAC. Frame to the guest: dst the guest's MAC
// (the transport's receive filter passes it), src ow_address. Type OW_ETHERTYPE, then struct ow_header and the
// payload. Little-endian, as both ends are.
#ifndef ONEIOS_WIFI_PROTO_H
#define ONEIOS_WIFI_PROTO_H

#include <stdint.h>

#define OW_ETHERTYPE    0x88b5u          // IEEE 802 local experimental 1: never on the air from here
#define OW_MAGIC        0x4f574946u      // "OWIF"
#define OW_VERSION      1u
#define OW_FRAME_MAX    1514u
#define OW_MAX_PAYLOAD  (OW_FRAME_MAX - 14u - 8u)
#define OW_MAX_IES      1200u            // information elements of one BSS kept (a beacon's are rarely more)
__attribute__((unused)) static const uint8_t ow_address[6] = { 0x02, 0x4f, 0x57, 0x49, 0x46, 0x49 };

struct ow_header {
  uint32_t magic;                        // OW_MAGIC
  uint16_t type;                         // OW_*
  uint16_t length;                       // payload bytes
};

enum {
  // guest -> host
  OW_HELLO = 1,                          // struct ow_hello: the host answers OW_STATE
  OW_SCAN = 2,                           // no payload: the host scans, sends OW_BSS for each network, then OW_SCAN_DONE
  OW_JOIN = 3,                           // struct ow_join: the host answers OW_JOIN_RESULT (once it knows)
  OW_LEAVE = 4,                          // no payload: the host disconnects; OW_STATE follows
  OW_POWER = 5,                          // struct ow_power
  // host -> guest
  OW_STATE = 16,                         // struct ow_state: at once for a change, every 2 s while joined
  OW_BSS = 17,                           // struct ow_bss + ies: one network of a scan
  OW_SCAN_DONE = 18,                     // struct ow_scan_done
  OW_JOIN_RESULT = 19,                   // struct ow_join_result
};

struct ow_hello { uint32_t version; };

enum { OW_RADIO_NONE = 0, OW_RADIO_OK = 1 };      // ow_state.radio
// ow_state.link: the station's state (as DroidVMWifi.inc's station numbers it)
enum { OW_LINK_OFF = 0, OW_LINK_IDLE = 1, OW_LINK_SCANNING = 2, OW_LINK_AUTH = 3, OW_LINK_ASSOC = 4, OW_LINK_KEYS = 5, OW_LINK_UP = 6 };

// A network as a scan saw it. ies[ie_length] follows the struct: the information elements as the beacon or probe
// response had them (the SSID element first is not promised).
struct ow_bss {
  uint8_t  bssid[6];
  uint16_t capability;                   // 802.11 capability information
  uint32_t freq_mhz;
  int16_t  signal_dbm;
  uint16_t beacon_interval;              // TU
  uint32_t age_ms;                       // since the radio last saw it
  uint16_t ie_length;
  uint8_t  ssid_length;
  uint8_t  ssid[32];
  uint8_t  pad;
};

struct ow_state {
  uint32_t version;                      // OW_VERSION
  uint32_t radio;                        // OW_RADIO_*
  uint32_t link;                         // OW_LINK_*
  int32_t  rssi_dbm;                     // joined: the signal; else 0
  int32_t  noise_dbm;
  uint32_t tx_rate_mbps;                 // joined: the last transmit rate
  uint32_t reason;                       // why the link went down (802.11 reason code), 0 if it did not
  uint8_t  mac[6];                       // the radio's address (wlan0's): the guest's en0 must send with it
  uint8_t  country[2];                   // the regulatory country, "00" when unknown
  struct ow_bss bss;                     // joined: the network (its ies follow the struct, ie_length of them)
};

enum { OW_SECURITY_OPEN = 0, OW_SECURITY_PSK = 1, OW_SECURITY_SAE = 2, OW_SECURITY_AUTO = 3 };
struct ow_join {
  uint8_t  ssid_length;
  uint8_t  ssid[32];
  uint8_t  bssid[6];                     // 00:00:00:00:00:00: any BSS of the network
  uint8_t  security;                     // OW_SECURITY_*; AUTO: the host picks from the network's RSN element
  uint8_t  key_length;                   // 0, 8..63 (a passphrase), or 32 with key_raw (a PSK)
  uint8_t  key_raw;                      // 1: key is the 32-byte PSK itself
  uint8_t  pad[2];
  uint8_t  key[64];
};

// ow_join_result.status: 0 joined; else why not
enum { OW_JOIN_OK = 0, OW_JOIN_FAILED = 1, OW_JOIN_NO_NETWORK = 2, OW_JOIN_AUTH = 3, OW_JOIN_TIMEOUT = 4, OW_JOIN_NO_RADIO = 5,
       OW_JOIN_REJECTED = 6 };
struct ow_join_result { uint32_t status; uint32_t code; };   // code: the 802.11 status or reason when there is one

struct ow_power { uint32_t on; };
struct ow_scan_done { uint32_t count; int32_t error; };

#endif
