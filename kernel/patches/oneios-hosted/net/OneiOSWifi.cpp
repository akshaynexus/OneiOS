// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// OneiOS: the guest's Wi-Fi, en0, on the phone's radio. docs/hardware.md section 3; README.md beside this file.
//
// One IOKit controller of the IEEE 802.11 kind over xnuhost's virtio-net device (/arm-io/net, UniXNU-droidvm's
// transport DroidVMVirtioNet.c): configd names its interface en0 and configures it (DHCP, IPv6, DNS: iOS's own), and
// wifid drives it through the Apple80211 requests of IO80211.framework (SIOCGA80211/SIOCSA80211 on the interface,
// which IONetworkInterface hands to performCommand) and the IEEE 802.11 kernel events it watches. The station below
// it is the phone's: each scan, join and leave is a message to xnuhost's Wi-Fi bridge (runtime/bridges/wifi), which
// drives the vendor's wpa_supplicant and nl80211 on wlan0; the frames go between en0 and wlan0 as they are, with
// wlan0's address. The messages travel in-band on the same queues as frames of their own type
// (drivers/net/oneios_wifi_proto.h).
//
// After UniXNU-droidvm f9121cd (BSD-2-Clause-Patent): package/DroidVMWifi.inc (the classes configd and wifid know a
// Wi-Fi interface by, the request layouts and answers, which follow UniXNU-sheng's WifiNet.cpp), with its station
// replaced by the host's and its controller given the transport; package/DroidVMVirtioNetIf.cpp (the start). No
// Ethernet controller: an iPhone's en0 is its Wi-Fi.
//
// The classes subclass the kernel collection's own IOEthernetController and IOEthernetInterface (Apple's
// IONetworkingFamily is a root of the collection, and 25 other roots link it, so it cannot be replaced): compiled
// against IONetworkingFamily-188's headers with __PRIVATE_SPI__, whose vtables are the collection's slot for slot
// (build-oneios-net.py checks every slot's discriminator against IONetworking-24A446-v54.json at each build). Calls
// into the family go through generated stubs (the virtual functions by their slots, the parents' constructors and
// destructors); nothing calls a non-virtual function of the family.
#include <IOKit/IOLib.h>
#include <IOKit/IOLocks.h>
#include <IOKit/IODeviceTreeSupport.h>
#include <IOKit/IOService.h>
#include <IOKit/network/IOEthernetController.h>
#include <IOKit/network/IOEthernetInterface.h>
#include <IOKit/network/IONetworkMedium.h>
#include <IOKit/network/IOOutputQueue.h>
#include <kern/clock.h>
#include <kern/thread_call.h>
#include <libkern/c++/OSMetaClass.h>
#include <mach/kmod.h>
#include <net/ethernet.h>
#include <net/if_media.h>
#include <net/kpi_interface.h>
#include <pexpert/pexpert.h>
#include <stdarg.h>
#include <sys/kern_event.h>
#include "oneios_wifi_proto.h"

extern kmod_info_t g_kernel_kmod_info;
extern vm_offset_t vm_kernel_slide;
// The generated bindings (build-oneios-net.py): the parents' metaclasses as linked and their instance sizes.
extern "C" const uint64_t oneios_net_controller_meta_link, oneios_net_interface_meta_link;
extern "C" const uint32_t oneios_net_controller_size, oneios_net_interface_size;
// OneiOSWifiLayout.cpp: IONetworkInterface::setInterfaceSubType's store, made with the family's own member layout.
extern "C" bool oneios_net_set_subtype(IONetworkInterface *interface, uint32_t subtype);
extern char oneios_net_controller_meta[] __asm__("__ZN17IO80211Controller10gMetaClassE");
extern char oneios_net_interface_meta[] __asm__("__ZN16IO80211Interface10gMetaClassE");
// sys/kern_event.h declares kev_msg_post without C linkage; the symbol is the C one
extern "C" errno_t oneios_kev_msg_post(struct kev_msg *event_msg) __asm__("_kev_msg_post");

extern "C" {
int droidvm_net_tx_frame(const void *frame, unsigned length);   // DroidVMVirtioNet.c
void droidvm_net_start(void);                                   // DroidVMVirtioNet.c
int droidvm_net_if_attach(const uint8_t mac[6]);                 // called by the transport's worker
void droidvm_net_if_input(const void *frame, unsigned length);   // the transport's worker, one frame at a time
void oneios_net_start(void);                                     // oneios_drivers_start (drivers/bridge/OneiOSBridge.c)
// For a debugger or a memory read: how far the start got, the requests, the frames.
volatile uint32_t oneios_wifi_stage, oneios_wifi_requests, oneios_wifi_refused, oneios_wifi_last_refused;
volatile uint64_t oneios_wifi_rx, oneios_wifi_tx, oneios_wifi_dropped;
}

#define WIFI_FRAME 1514
#define WIFI_SPEED (866ULL * 1000 * 1000)

namespace oneios_wifi {

#define SIOCSA80211 0x802869c8UL   // _IOW('i', 200, struct apple80211req)
#define SIOCGA80211 0xc02869c9UL   // _IOWR('i', 201, struct apple80211req)

struct apple80211req {
    char req_if_name[16];
    int32_t req_type;
    int32_t req_val;
    uint32_t req_len;
    user_addr_t req_data __attribute__((aligned(8)));
};

// request types (IO80211.framework's numbers)
enum {
    kSsid = 1, kAuthType = 2, kCipherKey = 3, kChannel = 4, kPowerSave = 5, kTxPower = 7, kRate = 8, kBssid = 9, kScanReq = 10,
    kScanResult = 11, kCardCaps = 12, kState = 13, kPhyMode = 14, kOpMode = 15, kRssi = 16, kNoise = 17, kPower = 19, kAssociate = 20,
    kAssociateResult = 21, kDisassociate = 22, kHostApMode = 25, kSupportedChannels = 27, kLocale = 28, kDeauth = 29, kDriverVersion = 43,
    kHardwareVersion = 44, kCountryCode = 51, kRadioInfo = 54, kScanReqMultiple = 86, kScanCacheClear = 90, kCurrentNetwork = 103,
    kRssiBounds = 106, kChannelsInfo = 207, kHwSupportedChannels = 254,
};
// events: class IEEE 802.11 (6), subclass 1, the data starting with the interface name
enum { kEvPowerChanged = 1, kEvSsidChanged = 2, kEvBssidChanged = 3, kEvLinkChanged = 4, kEvAssocDone = 9, kEvScanDone = 10 };
// ASSOCIATE_RESULT as the framework reads it: 0 while the join runs, 1 joined, anything else a failure
enum { kAssocRunning = 0, kAssocOk = 1, kAssocFailed = 2 };
enum { kCipherPmk = 6 };   // apple80211_cipher_type: the key is the 32-byte pairwise master key

struct channel { uint32_t version, channel, flags; };
struct channels_list { uint32_t version, count; channel list[401]; uint32_t unused; };
struct scan_result {
    uint32_t version; channel ch; uint16_t noise_known; int16_t noise, snr, rssi; uint16_t beacon_interval, capability; uint8_t bssid[6];
    uint8_t rate_count, unused0; uint32_t rates[15]; uint8_t ssid_length, ssid[32], unused1[3]; uint32_t age_ms; uint8_t flags, unused2;
    uint16_t elements_length; uint8_t elements[1024];
};
struct assoc_request {
    uint32_t version; uint16_t mode, auth_lower, auth_upper, unused0; uint32_t ssid_length; uint8_t ssid[32]; uint8_t bssid[6], unused1[2];
    uint32_t key_version, key_length, key_cipher; uint16_t key_flags, key_index; uint8_t key[64]; uint8_t rest[0x250 - 0x88];
};
struct link_event {
    uint8_t down, unused0[3]; int32_t rssi; uint16_t snr, noise; uint8_t unused1[4]; uint8_t involuntary, unused2[3]; uint32_t reason, subreason;
    uint8_t unused3[4];
};
static_assert(sizeof(apple80211req) == 0x28, "request layout");
static_assert(sizeof(scan_result) == 1164 && __builtin_offsetof(scan_result, ssid_length) == 0x60 &&
              __builtin_offsetof(scan_result, age_ms) == 0x84 && __builtin_offsetof(scan_result, elements) == 0x8c, "scan result layout");
static_assert(sizeof(assoc_request) == 592 && __builtin_offsetof(assoc_request, key) == 0x48, "association layout");
static_assert(sizeof(channels_list) == 4824, "channel list layout");
static_assert(sizeof(link_event) == 32, "link event layout");
static_assert(sizeof(ow_bss) == 56 && sizeof(ow_state) == 92 && sizeof(ow_join) == 108, "OneiOS Wi-Fi message layout");

// ---- the station: the host's radio ----
enum { kStaOff = OW_LINK_OFF, kStaIdle = OW_LINK_IDLE, kStaScanning = OW_LINK_SCANNING, kStaAuth = OW_LINK_AUTH, kStaAssoc = OW_LINK_ASSOC,
       kStaKeys = OW_LINK_KEYS, kStaUp = OW_LINK_UP };
enum { kMaxNetworks = 64, kScanTimeoutMs = 10000, kJoinTimeoutMs = 30000 };
struct Net { ow_bss bss; uint8_t ies[OW_MAX_IES]; };

static IOLock *gLock;                  // everything below
static Net *gScan, *gFill;             // the last finished scan, and the one the host is sending
static unsigned gScanCount, gFillCount, gScanCursor;
static bool gScanning;
static Net gCurrent;                   // the joined network (kStaAuth and above: the one being joined)
static uint32_t gState = kStaIdle;
static bool gPower = true, gRadio;     // gRadio: the host said it has the phone's radio
static int gAssocResult = kAssocFailed;
static int32_t gRssi = -127, gNoise = -95;
static uint32_t gRate;
static char gCountry[3] = "XX";
static uint8_t gMac[6];
static ifnet_t gIf;
static thread_call_t gScanTimeout, gJoinTimeout;
static void (*gLink)(bool up);         // the controller's link status
static uint32_t gLogged[256 / 32];

static void log(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static void log(const char *fmt, ...) {
    char line[200]; va_list ap; va_start(ap, fmt); vsnprintf(line, sizeof line, fmt, ap); va_end(ap);
    IOLog("ONEIOS WIFI: %s\n", line);
}

// A message to the host's bridge, as a frame of our own type on the transmit queue.
static void send(uint16_t type, const void *payload, uint16_t length) {
    uint8_t frame[WIFI_FRAME];
    if (length > OW_MAX_PAYLOAD) return;
    memcpy(frame, ow_address, 6);
    memcpy(frame + 6, gMac, 6);
    frame[12] = (uint8_t)(OW_ETHERTYPE >> 8); frame[13] = (uint8_t)OW_ETHERTYPE;
    ow_header h = { OW_MAGIC, type, length };
    memcpy(frame + 14, &h, sizeof h);
    if (length) memcpy(frame + 14 + sizeof h, payload, length);
    if (droidvm_net_tx_frame(frame, 14 + sizeof h + length)) log("message %u to the host not sent (transmit queue full or down)", type);
}

static void post_event(uint32_t code, const void *payload, size_t length) {
    uint8_t data[128]; if (length > sizeof data - 16) length = sizeof data - 16;
    memset(data, 0, 16);
    if (gIf) snprintf(reinterpret_cast<char *>(data), 16, "%s%u", ifnet_name(gIf), ifnet_unit(gIf));
    if (length) memcpy(data + 16, payload, length);
    struct kev_msg msg; memset(&msg, 0, sizeof msg);
    msg.vendor_code = KEV_VENDOR_APPLE; msg.kev_class = 6; msg.kev_subclass = 1; msg.event_code = code;
    msg.dv[0].data_length = static_cast<uint32_t>(16 + length); msg.dv[0].data_ptr = data;
    oneios_kev_msg_post(&msg);
}

static void link_changed(bool up, bool involuntary, uint32_t reason) {
    if (gLink) gLink(up);
    link_event ev; memset(&ev, 0, sizeof ev);
    ev.down = up ? 0 : 1; ev.rssi = gRssi; ev.noise = static_cast<uint16_t>(gNoise); ev.snr = static_cast<uint16_t>(gRssi - gNoise);
    ev.involuntary = involuntary ? 1 : 0; ev.reason = reason;
    post_event(kEvLinkChanged, &ev, sizeof ev);
}

// The joined network went away (a leave, the host's link down, power off): the events a driver sends.
static void leave_locked(bool involuntary, uint32_t reason, bool *notify) {
    const bool was_up = gState == kStaUp;
    if (gState >= kStaAuth && gState < kStaUp && gAssocResult == kAssocRunning) gAssocResult = kAssocFailed;
    gState = gPower ? kStaIdle : kStaOff;
    *notify = was_up;
    (void)involuntary; (void)reason;
}
static void left(bool involuntary, uint32_t reason) {
    post_event(kEvSsidChanged, nullptr, 0); post_event(kEvBssidChanged, nullptr, 0);
    link_changed(false, involuntary, reason);
}

static void scan_timeout(thread_call_param_t, thread_call_param_t) {
    IOLockLock(gLock);
    const bool was = gScanning;
    if (was) {   // the host never finished: what came is the result
        Net *t = gScan; gScan = gFill; gFill = t; gScanCount = gFillCount; gFillCount = 0; gScanCursor = 0; gScanning = false;
        if (gState == kStaScanning) gState = kStaIdle;
    }
    IOLockUnlock(gLock);
    if (was) { log("scan: no end from the host in %u ms", kScanTimeoutMs); post_event(kEvScanDone, nullptr, 0); }
}
static void join_timeout(thread_call_param_t, thread_call_param_t) {
    IOLockLock(gLock);
    const bool was = gAssocResult == kAssocRunning && gState >= kStaAuth && gState < kStaUp;
    if (was) { gAssocResult = kAssocFailed; gState = gPower ? kStaIdle : kStaOff; }
    IOLockUnlock(gLock);
    if (was) { log("join: no result from the host in %u ms", kJoinTimeoutMs); post_event(kEvAssocDone, nullptr, 0); }
}
static void arm(thread_call_t call, unsigned ms) {
    uint64_t at; clock_interval_to_deadline(ms, kMillisecondScale, &at); thread_call_enter_delayed(call, at);
}

static void scan() {
    IOLockLock(gLock);
    const bool start = gRadio && gPower && !gScanning;
    if (start) { gScanning = true; gFillCount = 0; if (gState == kStaIdle) gState = kStaScanning; }
    const bool radio = gRadio && gPower;
    IOLockUnlock(gLock);
    if (start) { send(OW_SCAN, nullptr, 0); arm(gScanTimeout, kScanTimeoutMs); }
    else if (!radio) post_event(kEvScanDone, nullptr, 0);   // nothing to scan with: an empty list at once
}

// A join request: the host joins with the vendor's supplicant and answers OW_JOIN_RESULT.
static int join(const assoc_request &a) {
    ow_join j; memset(&j, 0, sizeof j);
    if (a.ssid_length == 0 || a.ssid_length > 32 || a.key_length > 64) return EINVAL;
    j.ssid_length = static_cast<uint8_t>(a.ssid_length); memcpy(j.ssid, a.ssid, a.ssid_length);
    memcpy(j.bssid, a.bssid, 6);
    j.security = OW_SECURITY_AUTO;
    if (a.key_length == 32 && a.key_cipher == kCipherPmk) { j.key_raw = 1; j.key_length = 32; memcpy(j.key, a.key, 32); }
    else if (a.key_length >= 8 && a.key_length <= 63) { j.key_length = static_cast<uint8_t>(a.key_length); memcpy(j.key, a.key, a.key_length); }
    else if (a.key_length) { log("join: a key of %u bytes of kind %u is not one the host can use", a.key_length, a.key_cipher); return EINVAL; }
    else j.security = OW_SECURITY_OPEN;
    IOLockLock(gLock);
    if (!gRadio || !gPower) { IOLockUnlock(gLock); gAssocResult = kAssocFailed; post_event(kEvAssocDone, nullptr, 0); return ENXIO; }
    bool notify = false;
    if (gState == kStaUp) leave_locked(false, 0, &notify);
    memset(&gCurrent, 0, sizeof gCurrent);
    gCurrent.bss.ssid_length = j.ssid_length; memcpy(gCurrent.bss.ssid, j.ssid, j.ssid_length); memcpy(gCurrent.bss.bssid, j.bssid, 6);
    for (unsigned i = 0; i < gScanCount; i++) {   // what the last scan knew of it, until the host says
        if (gScan[i].bss.ssid_length == j.ssid_length && !memcmp(gScan[i].bss.ssid, j.ssid, j.ssid_length)) { gCurrent = gScan[i]; break; }
    }
    gState = kStaAuth; gAssocResult = kAssocRunning;
    IOLockUnlock(gLock);
    if (notify) left(false, 0);
    log("join: a network name of %u bytes, %s", j.ssid_length, j.key_raw ? "a PSK" : j.key_length ? "a passphrase" : "open");
    send(OW_JOIN, &j, sizeof j);
    memset(&j, 0, sizeof j);
    arm(gJoinTimeout, kJoinTimeoutMs);
    return 0;
}
static void leave() {
    bool notify = false;
    IOLockLock(gLock);
    if (gState >= kStaAuth) leave_locked(false, 0, &notify);
    IOLockUnlock(gLock);
    send(OW_LEAVE, nullptr, 0);
    if (notify) left(false, 0);
}

// ---- the host's messages (the transport's receive worker) ----
static void copy_bss(Net &to, const ow_bss &bss, const uint8_t *ies, size_t available) {
    to.bss = bss;
    size_t n = bss.ie_length;
    if (n > available) n = available;
    if (n > OW_MAX_IES) n = OW_MAX_IES;
    to.bss.ie_length = static_cast<uint16_t>(n);
    memcpy(to.ies, ies, n);
}

static void host_message(uint16_t type, const uint8_t *payload, size_t length) {
    switch (type) {
    case OW_STATE: {
        if (length < sizeof(ow_state)) return;
        ow_state s; memcpy(&s, payload, sizeof s);
        bool notify = false;
        IOLockLock(gLock);
        const bool first = !gRadio && s.radio == OW_RADIO_OK, lost = gRadio && s.radio != OW_RADIO_OK;
        gRadio = s.radio == OW_RADIO_OK;
        if (s.country[0] >= 'A' && s.country[0] <= 'Z' && s.country[1] >= 'A' && s.country[1] <= 'Z') { gCountry[0] = (char)s.country[0]; gCountry[1] = (char)s.country[1]; }
        if (s.link == OW_LINK_UP) {
            gRssi = s.rssi_dbm; gNoise = s.noise_dbm ? s.noise_dbm : -95; gRate = s.tx_rate_mbps;
            copy_bss(gCurrent, s.bss, payload + sizeof s, length - sizeof s);
        } else if (gState == kStaUp) leave_locked(true, s.reason, &notify);
        IOLockUnlock(gLock);
        if (first) log("the host has the phone's radio (%02x:%02x:%02x:..)", s.mac[0], s.mac[1], s.mac[2]);
        if (lost) log("the host has no radio any more");
        if (notify) { log("link down (reason %u)", s.reason); left(true, s.reason); }
        break;
    }
    case OW_BSS: {
        if (length < sizeof(ow_bss)) return;
        ow_bss bss; memcpy(&bss, payload, sizeof bss);
        IOLockLock(gLock);
        if (gScanning && gFillCount < kMaxNetworks) copy_bss(gFill[gFillCount++], bss, payload + sizeof bss, length - sizeof bss);
        IOLockUnlock(gLock);
        break;
    }
    case OW_SCAN_DONE: {
        ow_scan_done d = {}; memcpy(&d, payload, length < sizeof d ? length : sizeof d);
        IOLockLock(gLock);
        const bool was = gScanning;
        if (was) {
            Net *t = gScan; gScan = gFill; gFill = t; gScanCount = gFillCount; gFillCount = 0; gScanCursor = 0; gScanning = false;
            if (gState == kStaScanning) gState = kStaIdle;
        }
        const unsigned count = gScanCount;
        IOLockUnlock(gLock);
        if (was) { thread_call_cancel(gScanTimeout); post_event(kEvScanDone, nullptr, 0); }
        static unsigned shown;
        if (shown++ < 20) log("scan done: %u networks (host error %d)", count, d.error);
        break;
    }
    case OW_JOIN_RESULT: {
        ow_join_result r = {}; memcpy(&r, payload, length < sizeof r ? length : sizeof r);
        IOLockLock(gLock);
        const bool pending = gAssocResult == kAssocRunning && gState >= kStaAuth && gState < kStaUp;
        if (pending) {
            if (r.status == OW_JOIN_OK) { gState = kStaUp; gAssocResult = kAssocOk; }
            else { gState = gPower ? kStaIdle : kStaOff; gAssocResult = kAssocFailed; }
        }
        IOLockUnlock(gLock);
        if (!pending) break;
        thread_call_cancel(gJoinTimeout);
        log("join %s (status %u, 802.11 code %u)", r.status == OW_JOIN_OK ? "done: joined" : "failed", r.status, r.code);
        if (r.status == OW_JOIN_OK) {
            // The link first: configd starts DHCP when it comes up, wifid reads the result after ASSOC_DONE.
            link_changed(true, false, 0);
            post_event(kEvSsidChanged, nullptr, 0); post_event(kEvBssidChanged, nullptr, 0);
        }
        post_event(kEvAssocDone, nullptr, 0);
        break;
    }
    default: break;
    }
}

// ---- Apple80211 ----
static void fill_channel(channel &c, uint32_t mhz) {
    c.version = 1;
    if (mhz >= 5925) { c.channel = (mhz - 5950) / 5; c.flags = 0x2 | 0x10 | 0x2000; }   // 6 GHz
    else if (mhz >= 4900) { c.channel = (mhz - 5000) / 5; c.flags = 0x2 | 0x10; }
    else { c.channel = mhz == 2484 ? 14 : (mhz - 2407) / 5; c.flags = 0x2 | 0x8; }
}
// The rates of a BSS's Supported Rates and Extended Supported Rates elements, in 500 kb/s units without the basic bit.
static unsigned rates_of(const Net &n, uint32_t *out, unsigned max) {
    unsigned count = 0;
    for (unsigned i = 0; i + 2 <= n.bss.ie_length;) {
        const uint8_t id = n.ies[i], len = n.ies[i + 1];
        if (i + 2u + len > n.bss.ie_length) break;
        if (id == 1 || id == 50) for (unsigned k = 0; k < len && count < max; k++) out[count++] = n.ies[i + 2 + k] & 0x7f;
        i += 2u + len;
    }
    return count;
}
static void fill_result(scan_result &r, const Net &n) {
    memset(&r, 0, sizeof r); r.version = 1; fill_channel(r.ch, n.bss.freq_mhz); r.noise_known = 1; r.noise = -95; r.rssi = n.bss.signal_dbm;
    r.snr = static_cast<int16_t>(n.bss.signal_dbm + 95); r.beacon_interval = n.bss.beacon_interval; r.capability = n.bss.capability;
    memcpy(r.bssid, n.bss.bssid, 6); r.ssid_length = n.bss.ssid_length > 32 ? 32 : n.bss.ssid_length; memcpy(r.ssid, n.bss.ssid, 32);
    r.age_ms = n.bss.age_ms;
    uint32_t rates[15]; r.rate_count = static_cast<uint8_t>(rates_of(n, rates, 15));
    for (unsigned i = 0; i < r.rate_count; i++) r.rates[i] = rates[i] / 2;   // Mb/s
    const uint16_t ies = n.bss.ie_length > sizeof r.elements ? sizeof r.elements : n.bss.ie_length;
    r.elements_length = ies; memcpy(r.elements, n.ies, ies);
}
static int copy_out(const apple80211req &rq, const void *src, size_t size) {
    if (rq.req_len == 0 || !rq.req_data) return EINVAL;
    return copyout(src, rq.req_data, rq.req_len < size ? rq.req_len : size);
}
// A get that gives no buffer takes a single value in req_val; one with a buffer gets {version, value}.
static int answer_value(apple80211req &rq, uint32_t value) {
    if (rq.req_len == 0 || !rq.req_data) { rq.req_val = static_cast<int32_t>(value); return 0; }
    const uint32_t w[2] = { 1, value }; return copyout(w, rq.req_data, rq.req_len < sizeof w ? rq.req_len : sizeof w);
}
// The raw bytes, their length in req_len.
static int answer_bytes(apple80211req &rq, const void *src, uint32_t size) {
    if (!rq.req_data) return EINVAL;
    const uint32_t n = rq.req_len < size ? rq.req_len : size; rq.req_len = size; return copyout(src, rq.req_data, n);
}
static int copy_in(const apple80211req &rq, void *dst, size_t size) {
    if (!rq.req_data) return EINVAL;
    memset(dst, 0, size);
    return copyin(rq.req_data, dst, rq.req_len < size ? rq.req_len : size);
}
static int map_state(uint32_t s) {
    switch (s) { case kStaScanning: return 1; case kStaAuth: return 2; case kStaAssoc: case kStaKeys: return 3; case kStaUp: return 4; default: return 0; }
}

static int answer(bool set, apple80211req &rq) {
    const int type = rq.req_type;
    union Out { uint8_t raw[5120]; channels_list chans; scan_result res; assoc_request assoc; };   // not on the stack
    Out *outp = static_cast<Out *>(IOMallocData(sizeof(Out)));
    if (!outp) return ENOMEM;
    memset(outp, 0, sizeof(Out));
    struct Free { Out *p; ~Free() { memset(p, 0, sizeof(Out)); IOFreeData(p, sizeof(Out)); } } freeOut{outp};
    Out &out = *outp;
    uint32_t *w = reinterpret_cast<uint32_t *>(out.raw);
    if (set) {
        switch (type) {
        case kPower: {
            uint32_t in[6]; if (copy_in(rq, in, sizeof in)) return EFAULT;
            const bool on = in[2] != 0;
            bool notify = false;
            IOLockLock(gLock);
            gPower = on;
            if (!on) { if (gState >= kStaAuth) leave_locked(false, 0, &notify); gState = kStaOff; }
            else if (gState == kStaOff) gState = kStaIdle;
            IOLockUnlock(gLock);
            ow_power p = { on ? 1u : 0u }; send(OW_POWER, &p, sizeof p);
            if (notify) left(false, 0);
            log("power %u", on ? 1u : 0u);
            post_event(kEvPowerChanged, nullptr, 0); return 0; }
        case kScanReq: case kScanReqMultiple: scan(); return 0;
        case kAssociate: {
            if (copy_in(rq, &out.assoc, sizeof out.assoc)) return EFAULT;
            return join(out.assoc); }
        case kDisassociate: case kDeauth: log("leave"); leave(); return 0;
        // host access point mode: none (wifid sets it to 2 as it starts; accepted, it would offer a personal hotspot)
        case kHostApMode: return EOPNOTSUPP;
        // Every other set is accepted and has no effect, as in DroidVMWifi.inc: wifid tells the driver its state and
        // policies that way, and a refused APPLE80211_IOC_FILTER_MONITOR_EVENTS makes it give the device up.
        default: return 0;
        }
    }
    IOLockLock(gLock);
    const uint32_t state = gPower ? gState : kStaOff;
    const Net current = gCurrent;   // a copy: the answers below copy out without the lock
    const int32_t rssi = gRssi, noise = gNoise;
    const uint32_t rate = gRate;
    const int assoc = gAssocResult;
    const bool power = gPower;
    char country[3] = { gCountry[0], gCountry[1], 0 };
    bool have = false;
    if (type == kScanResult) {
        if (gScanCursor < gScanCount) { fill_result(out.res, gScan[gScanCursor++]); have = true; } else gScanCursor = 0;
    }
    IOLockUnlock(gLock);
    switch (type) {
    case kPower: w[0] = 1; w[1] = 1; w[2] = power ? 1 : 0; return copy_out(rq, w, 24);
    case kState: return answer_value(rq, static_cast<uint32_t>(map_state(state)));
    case kOpMode: return answer_value(rq, 1);   // station
    case kAuthType: w[0] = 1; w[1] = 1; w[2] = 0; return copy_out(rq, w, 12);   // {version, lower: open system, upper: none}
    case kPowerSave: return answer_value(rq, 0);
    case kCardCaps: {   // a bitmap, bit n in byte n / 8: AES, AES-CCM, power management, short slot and preamble, WPA2, WME, short guard interval
        static const uint8_t has[] = { 2, 3, 6, 9, 10, 14, 17, 19 };
        for (uint8_t b : has) out.raw[b / 8] |= static_cast<uint8_t>(1u << (b % 8));
        return answer_bytes(rq, out.raw, 12); }
    case kPhyMode: {
        uint32_t active = 0;
        if (state == kStaUp) active = current.bss.freq_mhz >= 4900 ? 0x2 : 0x8;   // 11a, 11g as the active PHY of the band
        w[0] = 1; w[1] = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x20 | 0x40; w[2] = active; return copy_out(rq, w, 12); }
    case kSsid: if (state < kStaAssoc) return ENOENT; return answer_bytes(rq, current.bss.ssid, current.bss.ssid_length);
    case kBssid: if (state < kStaAssoc) return ENOENT; return answer_bytes(rq, current.bss.bssid, 6);
    case kChannel: if (state < kStaAuth || !current.bss.freq_mhz) return ENOENT; w[0] = 1; fill_channel(*reinterpret_cast<channel *>(out.raw + 4), current.bss.freq_mhz); return copy_out(rq, w, 16);
    case kRssi: case kNoise: {   // {version, radios, unit (0: dBm), per chain[4], aggregate, extension per chain[4], its aggregate}
        if (state != kStaUp) return ENXIO;
        const uint32_t v = static_cast<uint32_t>(type == kRssi ? rssi : noise);
        w[0] = 1; w[1] = 1; w[2] = 0; for (int i = 3; i < 13; i++) w[i] = v; return copy_out(rq, w, 13 * 4); }
    case kRssiBounds: w[0] = 1; w[1] = static_cast<uint32_t>(-90); w[2] = static_cast<uint32_t>(-40); w[3] = static_cast<uint32_t>(-80); return copy_out(rq, w, 16);
    case kRate: if (state != kStaUp) return ENXIO; return answer_value(rq, rate ? rate : 54);
    case kTxPower: return answer_value(rq, 15);
    case kDriverVersion: case kHardwareVersion: {
        strlcpy(reinterpret_cast<char *>(out.raw), type == kDriverVersion ? "oneios-wifi 1" : "WCN785x (the phone's, through its vendor stack)", 256);
        return copy_out(rq, out.raw, 256); }
    case kCountryCode: return answer_bytes(rq, country, 3);   // the two letters and a NUL (the daemon asks with a 3-byte buffer)
    case kAssociateResult: return answer_value(rq, static_cast<uint32_t>(assoc));
    case kSupportedChannels: case kHwSupportedChannels: {
        out.chans.version = 1; unsigned n = 0;
        static const uint16_t known[] = { 2412, 2417, 2422, 2427, 2432, 2437, 2442, 2447, 2452, 2457, 2462, 2467, 2472,
                                          5180, 5200, 5220, 5240, 5260, 5280, 5300, 5320, 5500, 5520, 5540, 5560, 5580, 5600, 5620,
                                          5640, 5660, 5680, 5700, 5720, 5745, 5765, 5785, 5805, 5825 };
        for (uint16_t f : known) { fill_channel(out.chans.list[n], f); out.chans.list[n].flags |= 0x80; n++; }
        out.chans.count = n; return copy_out(rq, &out.chans, sizeof out.chans); }
    case kScanResult: if (!have) return EIO; return copy_out(rq, &out.res, sizeof out.res);   // EIO after the last record
    case kCurrentNetwork: if (state < kStaAssoc) return ENOENT; fill_result(out.res, current); out.res.rssi = static_cast<int16_t>(rssi); return copy_out(rq, &out.res, sizeof out.res);
    default: return EOPNOTSUPP;
    }
}

// Each request with its answer for the first few hundred, then a type's first one only.
static int request(bool set, apple80211req &rq) {
    const uint32_t length = rq.req_len; const int32_t value = rq.req_val, type = rq.req_type;
    const int error = answer(set, rq);
    oneios_wifi_requests = oneios_wifi_requests + 1;
    if (error == EOPNOTSUPP) { oneios_wifi_refused = oneios_wifi_refused + 1; oneios_wifi_last_refused = static_cast<uint32_t>(type); }
    static unsigned shown;
    const bool first = type >= 0 && type < 256 && !(gLogged[type / 32] & (1u << (type % 32)));
    if (type >= 0 && type < 256) gLogged[type / 32] |= 1u << (type % 32);
    if (shown < 150 || first || type == kScanReq || type == kAssociate || type == kDisassociate) {
        ++shown;
        log("wifid %s %d: len %u val %d -> %d (len %u val %d)", set ? "sets" : "gets", type, length, value, error, rq.req_len, rq.req_val);
    }
    return error;
}

static bool station_init() {
    gLock = IOLockAlloc();
    gScan = static_cast<Net *>(IOMallocZeroData(sizeof(Net) * kMaxNetworks));
    gFill = static_cast<Net *>(IOMallocZeroData(sizeof(Net) * kMaxNetworks));
    gScanTimeout = thread_call_allocate(scan_timeout, nullptr);
    gJoinTimeout = thread_call_allocate(join_timeout, nullptr);
    return gLock && gScan && gFill && gScanTimeout && gJoinTimeout;
}

}  // namespace oneios_wifi

// The classes configd and wifid know a Wi-Fi interface by; Apple's IO80211Family, which defines both, is not in
// this kernel or its collection.
class IO80211Interface : public IOEthernetInterface {
    OSDeclareDefaultStructors(IO80211Interface)
    typedef IOEthernetInterface super;
public:
    // wifid finds the interface an if_attached event names by IOPropertyMatch {IOInterfaceName: "en0"}, a property
    // IONetworkInterface does not publish: set as the interface is attached to DLIL.
    bool initIfnetParams(struct ifnet_init_params *params) override {
        if (!super::initIfnetParams(params)) return false;
        char name[16];   // IFNAMSIZ
        snprintf(name, sizeof name, "%s%u", params->name, params->unit);
        setProperty("IOInterfaceName", name);
        return true;
    }
    SInt32 performCommand(IONetworkController *controller, unsigned long command, void *argument0, void *argument1) override {
        if (!oneios_wifi::gIf) oneios_wifi::gIf = getIfnet();
        if (command == SIOCGA80211 || command == SIOCSA80211) {
            if (!argument1) return EINVAL;
            return oneios_wifi::request(command == SIOCSA80211, *static_cast<oneios_wifi::apple80211req *>(argument1));
        }
        return super::performCommand(controller, command, argument0, argument1);
    }
};
OSDefineMetaClassAndStructors(IO80211Interface, IOEthernetInterface)

class IO80211Controller : public IOEthernetController {
    OSDeclareAbstractStructors(IO80211Controller)
    typedef IOEthernetController super;
public:
    // IO80211.framework's interface list takes an interface whose SIOCGIFTYPE is IFT_ETHER of the Wi-Fi subfamily.
    IONetworkInterface *createInterface() override {
        IO80211Interface *interface = OSTypeAlloc(IO80211Interface);
        if (interface && !interface->init(this)) OSSafeReleaseNULL(interface);
        if (interface) {
            oneios_net_set_subtype(interface, IFNET_SUBFAMILY_WIFI);
            interface->setProperty("IO80211InterfaceRole", "Infrastructure");
        }
        return interface;
    }
};
OSDefineMetaClassAndAbstractStructors(IO80211Controller, IOEthernetController)

// The phone's Wi-Fi: one IEEE 802.11 medium, the link active only while the station is joined; frames to and from
// the transport while joined.
class OneiOSWifiController : public IO80211Controller {
    OSDeclareDefaultStructors(OneiOSWifiController)
    typedef IO80211Controller super;

    IOEthernetAddress address;
    IONetworkInterface *interface;
    IONetworkMedium *medium;
    bool enabled;

    static OneiOSWifiController *gSelf;
    static void link(bool up) {
        if (gSelf) gSelf->setLinkStatus(up ? kIONetworkLinkValid | kIONetworkLinkActive : kIONetworkLinkValid, up ? gSelf->medium : nullptr,
                                        up ? WIFI_SPEED : 0);
    }

public:
    bool initWithAddress(const uint8_t mac[6]) {
        if (!super::init(nullptr)) return false;
        memcpy(address.bytes, mac, kIOEthernetAddressSize);
        memcpy(oneios_wifi::gMac, mac, 6);
        return true;
    }
    void free() override {
        OSSafeReleaseNULL(interface);
        OSSafeReleaseNULL(medium);
        super::free();
    }
    bool start(IOService *provider) override {
        if (!super::start(provider)) return false;
        // IONetworkMedium::medium and addMedium are not virtual (not bound): the object by its class name, its
        // virtual init, and the dictionary keyed by its name, as addMedium does.
        const OSSymbol *mediumClass = OSSymbol::withCString("IONetworkMedium");
        OSObject *object = mediumClass ? OSMetaClass::allocClassWithName(mediumClass) : nullptr;
        OSSafeReleaseNULL(mediumClass);
        medium = object && object->metaCast("IONetworkMedium") ? reinterpret_cast<IONetworkMedium *>(object) : nullptr;
        if (!medium) OSSafeReleaseNULL(object);
        if (medium && !medium->init(kIOMediumIEEE80211Auto, WIFI_SPEED, 0, 0, "IEEE 802.11 auto")) OSSafeReleaseNULL(medium);
        OSDictionary *media = OSDictionary::withCapacity(1);
        bool published = medium && media && medium->getName() && media->setObject(medium->getName(), medium) && publishMediumDictionary(media);
        OSSafeReleaseNULL(media);
        if (!published) return false;
        if (!attachInterface(&interface, true)) return false;
        gSelf = this;
        oneios_wifi::gLink = link;
        return true;
    }
    IOReturn getHardwareAddress(IOEthernetAddress *addressOut) override { *addressOut = address; return kIOReturnSuccess; }
    // The radio sends with wlan0's address only: iOS's private (random) addresses are refused.
    IOReturn setHardwareAddress(const IOEthernetAddress *addressIn) override { (void)addressIn; return kIOReturnUnsupported; }
    IOReturn enable(IONetworkInterface *client) override {
        (void)client;
        enabled = true;
        oneios_wifi::gIf = interface->getIfnet();
        IOLockLock(oneios_wifi::gLock);
        const bool up = oneios_wifi::gState == oneios_wifi::kStaUp;
        IOLockUnlock(oneios_wifi::gLock);
        link(up);
        return kIOReturnSuccess;
    }
    IOReturn disable(IONetworkInterface *client) override { (void)client; enabled = false; setLinkStatus(kIONetworkLinkValid, nullptr); return kIOReturnSuccess; }
    UInt32 outputPacket(mbuf_t packet, void *parameter) override {
        (void)parameter;
        uint8_t frame[WIFI_FRAME];
        size_t length = mbuf_pkthdr_len(packet);
        bool up = enabled && __atomic_load_n(&oneios_wifi::gState, __ATOMIC_RELAXED) == oneios_wifi::kStaUp;
        if (up && length >= ETHER_HDR_LEN && length <= sizeof frame && mbuf_copydata(packet, 0, length, frame) == 0 &&
            !(frame[12] == (uint8_t)(OW_ETHERTYPE >> 8) && frame[13] == (uint8_t)OW_ETHERTYPE) &&
            droidvm_net_tx_frame(frame, static_cast<unsigned>(length)) == 0) {
            oneios_wifi_tx = oneios_wifi_tx + 1;
        } else oneios_wifi_dropped = oneios_wifi_dropped + 1;
        freePacket(packet);
        return kIOReturnOutputSuccess;
    }
    IOReturn setPromiscuousMode(bool active) override { (void)active; return kIOReturnSuccess; }
    IOReturn setMulticastMode(bool active) override { (void)active; return kIOReturnSuccess; }
    IOReturn setMulticastList(IOEthernetAddress *addresses, UInt32 count) override { (void)addresses; (void)count; return kIOReturnSuccess; }
    // A frame from the transport (its one receive thread, so inputPacket calls are serialized).
    void input(const void *frame, unsigned length) {
        if (!interface || !enabled || length < ETHER_HDR_LEN || length > WIFI_FRAME) return;
        if (__atomic_load_n(&oneios_wifi::gState, __ATOMIC_RELAXED) != oneios_wifi::kStaUp) { oneios_wifi_dropped = oneios_wifi_dropped + 1; return; }
        mbuf_t packet = allocatePacket(length);
        if (!packet) return;
        if (mbuf_copyback(packet, 0, length, frame, MBUF_DONTWAIT) != 0) { freePacket(packet); return; }
        oneios_wifi_rx = oneios_wifi_rx + 1;
        interface->inputPacket(packet, length);
    }
};
OSDefineMetaClassAndStructors(OneiOSWifiController, IO80211Controller)
OneiOSWifiController *OneiOSWifiController::gSelf;

// ---- the start: the classes, then the transport; the controller once the device is up ----
extern "C" void oneios_net_static_init(void) __asm__("__GLOBAL__sub_I_OneiOSWifi.cpp");
struct MetaClassHead { void *vtable; void *reserved; const OSMetaClass *superClassLink; };
static OneiOSWifiController *gController;
static bool gRegistered;

// On the caller's thread (the board's late start), as hid/ does: the kext load and the class registration take the
// kext and class-loading locks and must not run beside another thread's kext loads.
static bool register_classes() {
    OSReturn status = OSKextLoadKextWithIdentifier("com.apple.iokit.IONetworkingFamily");
    const OSMetaClass *controller = nullptr, *interface = nullptr;
    if (const OSSymbol *name = OSSymbol::withCString("IOEthernetController")) { controller = OSMetaClass::getMetaClassWithName(name); name->release(); }
    if (const OSSymbol *name = OSSymbol::withCString("IOEthernetInterface")) { interface = OSMetaClass::getMetaClassWithName(name); name->release(); }
    if (status != kOSReturnSuccess || !controller || !interface) {
        IOLog("ONEIOS WIFI: off: no IONetworkingFamily (load status 0x%x; the kernel is not in a kernel collection)\n", status);
        return false;
    }
    oneios_wifi_stage = 1;
    const OSMetaClass *boundController = reinterpret_cast<const OSMetaClass *>(oneios_net_controller_meta_link + vm_kernel_slide);
    const OSMetaClass *boundInterface = reinterpret_cast<const OSMetaClass *>(oneios_net_interface_meta_link + vm_kernel_slide);
    if (controller != boundController || interface != boundInterface || controller->getClassSize() != oneios_net_controller_size ||
        interface->getClassSize() != oneios_net_interface_size) {
        IOLog("ONEIOS WIFI: off: the live IOEthernetController/IOEthernetInterface (%p %u, %p %u) are not the bound ones (%p %u, %p %u): "
              "the bindings are for another kernel collection\n", controller, controller->getClassSize(), interface, interface->getClassSize(),
              boundController, oneios_net_controller_size, boundInterface, oneios_net_interface_size);
        return false;
    }
    oneios_wifi_stage = 2;
    void *module = OSMetaClass::preModLoad(g_kernel_kmod_info.name);
    if (!module) { IOLog("ONEIOS WIFI: off: the kernel's module could not be opened for class registration\n"); return false; }
    oneios_net_static_init();
    // The superclass each static initializer passed is the absolute symbol's value: the link address.
    struct { char *meta; const OSMetaClass *bound; uint64_t link; } fix[] = {
        { oneios_net_controller_meta, boundController, oneios_net_controller_meta_link },
        { oneios_net_interface_meta, boundInterface, oneios_net_interface_meta_link },
    };
    bool linked = true;
    for (auto &f : fix) {
        MetaClassHead *head = reinterpret_cast<MetaClassHead *>(f.meta);
        if (reinterpret_cast<uint64_t>(head->superClassLink) == f.link) head->superClassLink = f.bound;
        linked = linked && head->superClassLink == f.bound;
    }
    if (!linked || OSMetaClass::postModLoad(module)) {
        IOLog("ONEIOS WIFI: off: the classes did not register below IOEthernetController and IOEthernetInterface\n");
        return false;
    }
    oneios_wifi_stage = 3;
    return true;
}

// On the transport's worker thread, once the device is up.
int droidvm_net_if_attach(const uint8_t mac[6]) {
    IOService *resources = IOService::getResourceService();
    if (!gRegistered || !resources) return -1;
    // A built-in nub: IONetworkInterface turns its "built-in" into IOBuiltin, so configd's InterfaceNamer names it en0.
    IOService *nub = OSTypeAlloc(IOService);
    if (!nub || !nub->init()) { OSSafeReleaseNULL(nub); return -1; }
    nub->setName("oneios-wlan");
    nub->setProperty("built-in", kOSBooleanTrue);
    if (!nub->attach(resources)) { nub->release(); return -1; }
    OneiOSWifiController *controller = OSTypeAlloc(OneiOSWifiController);
    if (!controller || !controller->initWithAddress(mac)) { OSSafeReleaseNULL(controller); return -1; }
    if (!controller->attach(nub)) { controller->release(); return -1; }
    if (!controller->start(nub)) { controller->detach(nub); controller->release(); IOLog("ONEIOS WIFI: off: the controller did not start\n"); return -1; }
    gController = controller;
    oneios_wifi_stage = 4;
    IOLog("ONEIOS WIFI: on: the phone's Wi-Fi, address %02x:%02x:%02x:%02x:%02x:%02x; asking the host for its radio\n",
          mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    ow_hello hello = { OW_VERSION };
    oneios_wifi::send(OW_HELLO, &hello, sizeof hello);
    return 0;
}

void droidvm_net_if_input(const void *frame, unsigned length) {
    const uint8_t *f = static_cast<const uint8_t *>(frame);
    if (length >= 14 + sizeof(ow_header) && f[12] == (uint8_t)(OW_ETHERTYPE >> 8) && f[13] == (uint8_t)OW_ETHERTYPE) {
        ow_header h; memcpy(&h, f + 14, sizeof h);
        if (h.magic == OW_MAGIC && 14 + sizeof h + h.length <= length) oneios_wifi::host_message(h.type, f + 14 + sizeof h, h.length);
        return;   // never iOS's: a frame of this type from the radio is dropped here
    }
    if (gController) gController->input(frame, length);
}

void oneios_net_start(void) {
    uint32_t enabled = 1;
    PE_parse_boot_argn("oneios-wifi", &enabled, sizeof enabled);
    if (!enabled) { IOLog("ONEIOS WIFI: off (oneios-wifi=0)\n"); return; }
    if (!oneios_wifi::station_init()) { IOLog("ONEIOS WIFI: off: no memory for the station\n"); return; }
    gRegistered = register_classes();
    if (!gRegistered) return;
    IOLog("ONEIOS WIFI: classes registered below the collection's IOEthernetController and IOEthernetInterface; starting /arm-io/net\n");
    droidvm_net_start();   // the transport: maps /arm-io/net, starts its worker, which calls droidvm_net_if_attach
}
