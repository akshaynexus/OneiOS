// SPDX-License-Identifier: MIT
// The vendor's wpa_supplicant as the Wi-Fi bridge's station (docs/hardware.md section 3): its stable AIDL service
// android.hardware.wifi.supplicant.ISupplicant/default, version 2 (the frozen interface with the hash
// 5e5c6d9e5565999f73fa7f797f5b94616174745b), over /dev/binder with aidl.h. The bridge is its only client in a
// session, as One UI's framework is otherwise.
#ifndef ONEIOS_BRIDGES_SUPPLICANT_H
#define ONEIOS_BRIDGES_SUPPLICANT_H

#include "aidl.h"
#include "oneios_wifi_proto.h"

#define SUP_SERVICE      "android.hardware.wifi.supplicant.ISupplicant/default"
#define SUP_MANAGER      "android.os.IServiceManager"
#define SUP_ISUPPLICANT  "android.hardware.wifi.supplicant.ISupplicant"
#define SUP_STA_IFACE    "android.hardware.wifi.supplicant.ISupplicantStaIface"
#define SUP_STA_NETWORK  "android.hardware.wifi.supplicant.ISupplicantStaNetwork"
#define SUP_STA_CALLBACK "android.hardware.wifi.supplicant.ISupplicantStaIfaceCallback"
#define SUP_VERSION      2
#define SUP_HASH         "5e5c6d9e5565999f73fa7f797f5b94616174745b"

// Transaction codes: the place of each method in the frozen version 2 .aidl files (the first is 1).
enum { SM_GET_SERVICE = 1 };                                                     // IServiceManager.getService
enum { SUP_ADD_STA_INTERFACE = 2, SUP_GET_DEBUG_LEVEL = 3, SUP_GET_STA_INTERFACE = 5, SUP_REMOVE_INTERFACE = 10 };
enum { STA_ADD_NETWORK = 3, STA_DISCONNECT = 6, STA_GET_MAC_ADDRESS = 15, STA_GET_NAME = 16, STA_LIST_NETWORKS = 26, STA_REGISTER_CALLBACK = 29,
       STA_REMOVE_NETWORK = 35, STA_GET_SIGNAL_POLL_RESULTS = 62 };
enum { NET_GET_ID = 25, NET_SELECT = 45, NET_SET_GROUP_CIPHER = 72, NET_SET_KEY_MGMT = 75, NET_SET_PAIRWISE_CIPHER = 77, NET_SET_PROTO = 80,
       NET_SET_PSK = 81, NET_SET_PSK_PASSPHRASE = 82, NET_SET_REQUIRE_PMF = 83, NET_SET_SAE_H2E_MODE = 84, NET_SET_SAE_PASSWORD = 85,
       NET_SET_SCAN_SSID = 87, NET_SET_SSID = 88 };
enum { CB_ASSOCIATION_REJECTED = 2, CB_AUTHENTICATION_TIMEOUT = 3, CB_DISCONNECTED = 7, CB_NETWORK_NOT_FOUND = 21, CB_STATE_CHANGED = 24,
       CB_BSS_FREQUENCY_CHANGED = 33, CB_SUPPLICANT_STATE_CHANGED = 34 };
// KeyMgmtMask, ProtoMask, PairwiseCipherMask, GroupCipherMask
enum { KM_WPA_PSK = 1 << 1, KM_NONE = 1 << 2, KM_WPA_PSK_SHA256 = 1 << 8, KM_SAE = 1 << 10, KM_OWE = 1 << 22 };
enum { PROTO_WPA = 1 << 0, PROTO_RSN = 1 << 1 };
enum { PC_TKIP = 1 << 3, PC_CCMP = 1 << 4, PC_GCMP_128 = 1 << 6, PC_GCMP_256 = 1 << 8 };
enum { GC_TKIP = 1 << 3, GC_CCMP = 1 << 4, GC_GCMP_128 = 1 << 6, GC_GCMP_256 = 1 << 8 };
// StaIfaceCallbackState
enum { ST_DISCONNECTED = 0, ST_IFACE_DISABLED = 1, ST_INACTIVE = 2, ST_SCANNING = 3, ST_AUTHENTICATING = 4, ST_ASSOCIATING = 5, ST_ASSOCIATED = 6,
       ST_FOURWAY_HANDSHAKE = 7, ST_GROUP_HANDSHAKE = 8, ST_COMPLETED = 9 };

// ---- what a network asks for (pure: tests/test_bridges.c) ----
// What a BSS's information elements offer.
typedef struct {
  bool rsn, wpa;                 // an RSN element; a WPA (version 1) element
  bool psk, psk_sha256, sae, owe, eap;
  bool tkip, ccmp, gcmp;         // pairwise ciphers
  bool pmf_capable, pmf_required;
} br_wifi_security;
void br_wifi_security_parse(const uint8_t *ies, size_t length, br_wifi_security *out);

// The supplicant network of a join: the calls of ISupplicantStaNetwork to make, decided from what iOS gave (the key)
// and what the last scan saw of the network (NULL: not seen).
typedef struct {
  uint32_t key_mgmt, proto, pairwise, group;
  bool psk_passphrase, psk_raw, sae_password;      // which secret call takes the key
  bool require_pmf, set_pmf;                       // set_pmf false: the supplicant's own default
  bool scan_ssid;                                  // probe for the name: the network was in no scan
  const char *kind;                                // for the log: "open", "WPA2 (passphrase)", ...
} br_wifi_plan;
// 0, or -EINVAL when the key cannot open the network (a raw WPA2 key for a WPA3-only network).
int br_wifi_plan_join(const struct ow_join *join, const br_wifi_security *seen, br_wifi_plan *plan);

// The request parcels (tests check them byte for byte).
void sup_parcel_get_service(ab_parcel *p, const char *name);
void sup_parcel_string(ab_parcel *p, const char *descriptor, const char *text);
void sup_parcel_i32(ab_parcel *p, const char *descriptor, int32_t value);
void sup_parcel_bytes(ab_parcel *p, const char *descriptor, const uint8_t *bytes, size_t length);
void sup_parcel_empty(ab_parcel *p, const char *descriptor);
void sup_parcel_callback(ab_parcel *p, uint64_t object);

// A callback of ISupplicantStaIfaceCallback, decoded.
typedef struct {
  int      kind;                 // a CB_* code, or 0: not one the bridge uses
  int32_t  state, id, frequency; // state changes
  uint32_t code;                 // disconnect: the reason; rejection: the status
  bool     local, timed_out;
  uint8_t  bssid[6];
} sup_event;
void sup_decode_callback(uint32_t code, ab_reader *r, sup_event *out);

// ---- the client ----
enum { BR_SUPPLICANT_JOINED = 1, BR_SUPPLICANT_JOIN_FAILED = 2, BR_SUPPLICANT_DISCONNECTED = 3, BR_SUPPLICANT_DIED = 4 };
// status: an OW_JOIN_* for JOIN_FAILED; code: the 802.11 status or reason. Called on the binder's looper thread:
// the receiver must not call back into the supplicant from it.
typedef void (*br_supplicant_event_fn)(void *ctx, int event, uint32_t status, uint32_t code);
typedef struct br_supplicant br_supplicant;

// Finds the service (started by init on the first lookup: up to wait_ms), takes the station interface (adds it when
// the supplicant has none of that name), registers the callback and removes the networks left in it. NULL when any
// step fails; the reason is logged.
br_supplicant *br_supplicant_open(const char *ifname, unsigned wait_ms, br_supplicant_event_fn fn, void *ctx);
// One network: removes the last one, adds this, selects it. The result comes as an event. 0 or -errno.
int  br_supplicant_join(br_supplicant *s, const struct ow_join *join, const uint8_t *ies, size_t ie_length);
// Disconnects and removes the network.
int  br_supplicant_leave(br_supplicant *s);
uint32_t br_supplicant_frequency(br_supplicant *s);
// Reads the interface's address, its networks' ids and the signal poll; for the tool. 0 or -errno.
int  br_supplicant_status(br_supplicant *s, uint8_t mac[6], int32_t *networks, unsigned *nnetworks, int32_t poll[5]);
// Leaves the network, removes the interface if open added it, and closes the binder (the supplicant drops a dead
// client's callback).
void br_supplicant_close(br_supplicant *s);

#endif
