// SPDX-License-Identifier: MIT
// The supplicant client (wifi/supplicant.c, wifi/aidl_binder.c) against a real binder driver: a private binderfs on
// the build host (`make test-binder` mounts one in a user namespace). A child process plays the service manager (the
// context manager) and android.hardware.wifi.supplicant.ISupplicant/default with one station interface and its
// networks, and calls back as wpa_supplicant's AIDL service does: the descriptor and the interface version of the
// callback asked for inside registerCallback (a call back into the client before its call is answered), the state
// changes one-way.
//
// Scenarios: a service that is not registered at first (a lazy service: the lookup is tried again); no station
// interface (added, and removed at the close); One UI's networks removed; a WPA2 join that completes, with what the
// network got; a wrong key (the access point ends the 4-way handshake); a network that is not there; an association
// rejected; a link that drops; a look without a callback (the tool beside One UI); a supplicant that dies.
// Both ends use aidl.h: what this proves is the kernel protocol (objects, handles, nested calls, one-way order,
// death). The parcels' bytes are checked in test_wifi.c; the real libbinder and the vendor's supplicant are
// `wifictl status` on the phone.
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include "../wifi/supplicant.h"
#include <errno.h>
#include <fcntl.h>
#include <linux/android/binder.h>
#include <linux/android/binderfs.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int failures, checks;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); fflush(stdout); } } while (0)
static void log_line(int level, const char *line) { printf("  [%s] %s\n", level == 0 ? "E" : level == 1 ? "I" : "D", line); fflush(stdout); }

// ================================ the fake service manager and supplicant (child) ================================
enum { OBJ_MANAGER = 0, OBJ_SUPPLICANT = 1, OBJ_IFACE = 2, OBJ_NETWORK = 0x100 };
static ab *fake;
static int fake_log = -1;            // what the client did, a line each, for the parent
static unsigned lookups;
static bool have_iface, have_callback;
static uint32_t callback;
static int next_network = 2;         // 0 and 1 are "One UI's"
static bool network_present[16] = { true, true };
static char ssid[33];
static int sfail;
#define SCHECK(cond) do { if (!(cond)) { sfail++; printf("FAKE FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); fflush(stdout); } } while (0)

static void note(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static void note(const char *fmt, ...)
{
  char line[160];
  va_list ap;
  va_start(ap, fmt);
  int n = vsnprintf(line, sizeof line - 1, fmt, ap);
  va_end(ap);
  line[n++] = '\n';
  (void)!write(fake_log, line, (size_t)n);
}

static int check_descriptor(void *ctx, ab_reader *r)
{
  char text[96];
  (void)ctx;
  return ab_get_string(r, text, sizeof text) && !strcmp(text, SUP_STA_CALLBACK) ? 0 : -EPROTO;
}
static int check_version(void *ctx, ab_reader *r) { (void)ctx; return ab_get_i32(r) == SUP_VERSION && !r->bad ? 0 : -EPROTO; }

static void state(int32_t new_state, bool modern, int32_t frequency)
{
  static const uint8_t bssid[6] = { 1, 2, 3, 4, 5, 6 };
  ab_parcel p;
  ab_init(&p);
  ab_put_token(&p, SUP_STA_CALLBACK);
  if (modern) {                      // onSupplicantStateChanged(SupplicantStateChangeData)
    ab_put_i32(&p, 1);
    size_t at = p.size;
    ab_put_i32(&p, 0);
    ab_put_i32(&p, new_state); ab_put_i32(&p, next_network - 1); ab_put_bytes(&p, (const uint8_t *)ssid, strlen(ssid)); ab_put_bytes(&p, bssid, 6);
    ab_put_i32(&p, KM_WPA_PSK); ab_put_i32(&p, frequency); ab_put_i32(&p, 0);
    int32_t size = (int32_t)(p.size - at);
    memcpy(p.data + at, &size, 4);
    SCHECK(ab_oneway(fake, callback, CB_SUPPLICANT_STATE_CHANGED, &p) == 0);
  } else {                           // onStateChanged(newState, bssid, id, ssid, filsHlpSent)
    ab_put_i32(&p, new_state); ab_put_bytes(&p, bssid, 6); ab_put_i32(&p, next_network - 1); ab_put_bytes(&p, (const uint8_t *)ssid, strlen(ssid)); ab_put_i32(&p, 0);
    SCHECK(ab_oneway(fake, callback, CB_STATE_CHANGED, &p) == 0);
  }
}
static void disconnected(bool local, int32_t reason)
{
  static const uint8_t bssid[6] = { 1, 2, 3, 4, 5, 6 };
  ab_parcel p;
  ab_init(&p);
  ab_put_token(&p, SUP_STA_CALLBACK);
  ab_put_bytes(&p, bssid, 6); ab_put_i32(&p, local); ab_put_i32(&p, reason);
  SCHECK(ab_oneway(fake, callback, CB_DISCONNECTED, &p) == 0);
}

// What the network named ssid does once selected.
static void selected(void)
{
  ab_parcel p;
  if (!have_callback) return;
  disconnected(true, 3);             // as the real one: the old association ends by the phone's own doing
  state(ST_SCANNING, false, 0);
  if (!strcmp(ssid, "missing")) {
    for (int i = 0; i < 2; i++) {
      ab_init(&p);
      ab_put_token(&p, SUP_STA_CALLBACK);
      ab_put_bytes(&p, (const uint8_t *)ssid, strlen(ssid));
      SCHECK(ab_oneway(fake, callback, CB_NETWORK_NOT_FOUND, &p) == 0);
    }
    return;
  }
  state(ST_ASSOCIATING, false, 0);
  if (!strcmp(ssid, "reject")) {     // onAssociationRejected(AssociationRejectionData)
    ab_init(&p);
    ab_put_token(&p, SUP_STA_CALLBACK);
    ab_put_i32(&p, 1);
    size_t at = p.size;
    ab_put_i32(&p, 0);
    ab_put_bytes(&p, (const uint8_t *)ssid, strlen(ssid)); ab_put_bytes(&p, (const uint8_t *)"\1\2\3\4\5\6", 6); ab_put_i32(&p, 17); ab_put_i32(&p, 0);
    ab_put_i32(&p, 0); ab_put_i32(&p, 0); ab_put_i32(&p, 0);
    int32_t size = (int32_t)(p.size - at);
    memcpy(p.data + at, &size, 4);
    SCHECK(ab_oneway(fake, callback, CB_ASSOCIATION_REJECTED, &p) == 0);
    return;
  }
  state(ST_ASSOCIATED, false, 0);
  state(ST_FOURWAY_HANDSHAKE, false, 0);
  if (!strcmp(ssid, "wrongkey")) { disconnected(false, 15); state(ST_DISCONNECTED, false, 0); return; }
  state(ST_GROUP_HANDSHAKE, true, 5180);
  state(ST_COMPLETED, true, 5180);
  if (!strcmp(ssid, "drops")) { usleep(200000); disconnected(false, 4); state(ST_DISCONNECTED, true, 0); }
}

static int fake_incoming(void *ctx, uint64_t object, uint32_t code, uint32_t flags, ab_reader *r, ab_parcel *reply)
{
  char text[96];
  const uint8_t *bytes;
  size_t n;
  (void)ctx; (void)flags;
  ab_init(reply);
  if (object == OBJ_MANAGER) {
    if (code != SM_GET_SERVICE || !ab_get_token(r, SUP_MANAGER) || !ab_get_string(r, text, sizeof text)) return AB_UNKNOWN_TRANSACTION;
    ab_put_ok(reply);
    if (strcmp(text, SUP_SERVICE) || ++lookups <= 2) ab_put_null_binder(reply);      // a lazy service: there on the third lookup
    else ab_put_local(reply, OBJ_SUPPLICANT);
    return 0;
  }
  if (object == OBJ_SUPPLICANT) {
    if (!ab_get_token(r, SUP_ISUPPLICANT)) return AB_BAD_TYPE;
    switch (code) {
    case SUP_GET_STA_INTERFACE: case SUP_ADD_STA_INTERFACE:
      SCHECK(ab_get_string(r, text, sizeof text) && !strcmp(text, "wlan0"));
      if (code == SUP_GET_STA_INTERFACE && !have_iface) { ab_put_service_error(reply, 4); return 0; }   // FAILURE_IFACE_UNKNOWN
      if (code == SUP_ADD_STA_INTERFACE) { SCHECK(!have_iface); have_iface = true; note("addStaInterface %s", text); }
      ab_put_ok(reply);
      ab_put_local(reply, OBJ_IFACE);
      return 0;
    case SUP_REMOVE_INTERFACE: {     // IfaceInfo: type, name
      size_t end = ab_get_parcelable(r);
      SCHECK(end && ab_get_i32(r) == 0 && ab_get_string(r, text, sizeof text) && !strcmp(text, "wlan0") && r->position == end);
      have_iface = false;
      note("removeInterface %s", text);
      ab_put_ok(reply);
      return 0;
    }
    default: return AB_UNKNOWN_TRANSACTION;
    }
  }
  if (object == OBJ_IFACE) {
    if (!ab_get_token(r, SUP_STA_IFACE)) return AB_BAD_TYPE;
    switch (code) {
    case STA_REGISTER_CALLBACK: {
      ab_parcel q;
      SCHECK(ab_get_binder(r, &callback, NULL) && ab_acquire(fake, callback) == 0 && ab_watch(fake, callback, 1) == 0);
      // As libbinder_ndk's fromBinder and the supplicant do: the descriptor and the version, before the answer.
      ab_init(&q);
      SCHECK(ab_call(fake, callback, AB_INTERFACE, &q, NULL, check_descriptor, NULL) == 0);
      ab_init(&q);
      ab_put_token(&q, SUP_STA_CALLBACK);
      SCHECK(ab_call(fake, callback, AB_GET_VERSION, &q, NULL, check_version, NULL) == 0);
      ab_init(&q);
      SCHECK(ab_call(fake, callback, AB_PING, &q, NULL, NULL, NULL) == 0);
      ab_init(&q);
      ab_put_token(&q, SUP_STA_CALLBACK);
      SCHECK(ab_call(fake, callback, 0x5f444d50u /* dump */, &q, NULL, NULL, NULL) == -ENOSYS);
      have_callback = true;
      note("registerCallback");
      ab_put_ok(reply);
      return 0;
    }
    case STA_LIST_NETWORKS: {
      unsigned count = 0;
      for (unsigned i = 0; i < 16; i++) count += network_present[i];
      ab_put_ok(reply);
      ab_put_i32(reply, (int32_t)count);
      for (unsigned i = 0; i < 16; i++) if (network_present[i]) ab_put_i32(reply, (int32_t)i);
      return 0;
    }
    case STA_REMOVE_NETWORK: {
      int32_t id = ab_get_i32(r);
      if (id < 0 || id >= 16 || !network_present[id]) { ab_put_service_error(reply, 9); return 0; }
      network_present[id] = false;
      note("removeNetwork %d", id);
      ab_put_ok(reply);
      return 0;
    }
    case STA_ADD_NETWORK:
      SCHECK(next_network < 16);
      network_present[next_network] = true;
      ab_put_ok(reply);
      ab_put_local(reply, OBJ_NETWORK + (uint64_t)next_network);
      note("addNetwork %d", next_network++);
      return 0;
    case STA_DISCONNECT: note("disconnect"); ab_put_ok(reply); return 0;
    case STA_GET_MAC_ADDRESS: ab_put_ok(reply); ab_put_bytes(reply, (const uint8_t *)"\x02\x11\x22\x33\x44\x55", 6); return 0;
    case STA_GET_SIGNAL_POLL_RESULTS:
      ab_put_ok(reply);
      ab_put_i32(reply, 1); ab_put_i32(reply, 1); ab_put_i32(reply, 24);
      ab_put_i32(reply, 0); ab_put_i32(reply, -51); ab_put_i32(reply, 866); ab_put_i32(reply, 780); ab_put_i32(reply, 5180);
      return 0;
    default: return AB_UNKNOWN_TRANSACTION;
    }
  }
  if (object >= OBJ_NETWORK && object < OBJ_NETWORK + 16) {
    if (!ab_get_token(r, SUP_STA_NETWORK)) return AB_BAD_TYPE;
    ab_put_ok(reply);
    switch (code) {
    case NET_GET_ID: ab_put_i32(reply, (int32_t)(object - OBJ_NETWORK)); return 0;
    case NET_SET_SSID: SCHECK(ab_get_bytes(r, &bytes, &n) && n <= 32); memcpy(ssid, bytes, n); ssid[n] = 0; note("setSsid %s", ssid); return 0;
    case NET_SET_KEY_MGMT: note("setKeyMgmt %d", ab_get_i32(r)); return 0;
    case NET_SET_PROTO: note("setProto %d", ab_get_i32(r)); return 0;
    case NET_SET_PAIRWISE_CIPHER: note("setPairwiseCipher %d", ab_get_i32(r)); return 0;
    case NET_SET_GROUP_CIPHER: note("setGroupCipher %d", ab_get_i32(r)); return 0;
    case NET_SET_REQUIRE_PMF: note("setRequirePmf %d", ab_get_i32(r)); return 0;
    case NET_SET_SCAN_SSID: note("setScanSsid %d", ab_get_i32(r)); return 0;
    case NET_SET_PSK_PASSPHRASE: SCHECK(ab_get_string(r, text, sizeof text)); note("setPskPassphrase %s", text); return 0;
    case NET_SET_SAE_PASSWORD: SCHECK(ab_get_string(r, text, sizeof text)); note("setSaePassword %s", text); return 0;
    case NET_SET_SAE_H2E_MODE: ab_init(reply); return AB_UNKNOWN_TRANSACTION;          // an older supplicant: no such call
    case NET_SET_PSK: SCHECK(ab_get_bytes(r, &bytes, &n) && n == 32); note("setPsk %02x..%02x", bytes[0], bytes[31]); return 0;
    case NET_SELECT: note("select %d", (int)(object - OBJ_NETWORK)); selected(); return 0;
    default: ab_init(reply); return AB_UNKNOWN_TRANSACTION;
    }
  }
  return AB_UNKNOWN_TRANSACTION;
}

static void fake_death(void *ctx, uint64_t cookie) { (void)ctx; (void)cookie; note("the client died"); have_callback = false; }

static void run_fake(int log_fd, bool with_iface)
{
  fake_log = log_fd;
  have_iface = with_iface;
  fake = ab_open(NULL, fake_incoming, fake_death, NULL);
  if (!fake || ab_context_manager(fake)) { printf("fake: no binder or not the context manager: %s\n", strerror(errno)); _exit(91); }
  note("ready");
  for (;;) pause();
}

// ================================ the client (parent) ================================
static volatile int events[8];
static volatile uint32_t statuses[8], codes[8];
static volatile unsigned nevents;
static void on_event(void *ctx, int event, uint32_t status, uint32_t code)
{
  (void)ctx;
  unsigned i = nevents;
  if (i < 8) { events[i] = event; statuses[i] = status; codes[i] = code; nevents = i + 1; }
}
static bool wait_events(unsigned n)
{
  for (unsigned i = 0; i < 300 && nevents < n; i++) usleep(10000);
  return nevents >= n;
}

static pid_t start_fake(int *log_fd, bool with_iface)
{
  int fds[2];
  char line[16] = "";
  if (pipe(fds)) return -1;
  pid_t pid = fork();
  if (pid == 0) { close(fds[0]); run_fake(fds[1], with_iface); _exit(0); }
  close(fds[1]);
  *log_fd = fds[0];
  CHECK(read(fds[0], line, 6) == 6 && !memcmp(line, "ready\n", 6));
  return pid;
}

// Everything the fake noted so far (it is stopped first, so the pipe ends).
static char *stop_fake(pid_t pid, int log_fd)
{
  static char text[8192];
  size_t n = 0;
  kill(pid, SIGKILL);
  waitpid(pid, NULL, 0);
  for (ssize_t got; n < sizeof text - 1 && (got = read(log_fd, text + n, sizeof text - 1 - n)) > 0;) n += (size_t)got;
  text[n] = 0;
  close(log_fd);
  return text;
}

// A binderfs of a kernel built with no default devices has only binder-control: ask it for the device by name.
static void binderfs_add(const char *path)
{
  const char *slash = strrchr(path, '/');
  if (!slash || access(path, F_OK) == 0) return;
  char control[512];
  snprintf(control, sizeof control, "%.*s/binder-control", (int)(slash - path), path);
  int fd = open(control, O_RDWR | O_CLOEXEC);
  if (fd < 0) return;
  struct binderfs_device d = { 0 };
  snprintf(d.name, sizeof d.name, "%s", slash + 1);
  if (ioctl(fd, BINDER_CTL_ADD, &d)) printf("test_aidl_binder: %s: BINDER_CTL_ADD %s: %s\n", control, d.name, strerror(errno));
  close(fd);
}

int main(void)
{
  setvbuf(stdout, NULL, _IOLBF, 0);
  br_log_sink = log_line;
  const char *device = getenv("ONEIOS_BINDER");
  if (device) binderfs_add(device);
  if (!device || access(device, R_OK | W_OK)) { printf("test_aidl_binder: set ONEIOS_BINDER to a binderfs binder device (make test-binder does)\n"); return 77; }
  int log_fd;

  // 1. A lazy service with no station interface; joins of every outcome.
  pid_t pid = start_fake(&log_fd, false);
  CHECK(br_supplicant_open("wlan0", 0, on_event, NULL) == NULL);            // not registered yet, and no time to wait
  br_supplicant *s = br_supplicant_open("wlan0", 3000, on_event, NULL);    // the third lookup finds it
  CHECK(s != NULL);
  if (!s) { stop_fake(pid, log_fd); printf("aidl binder: %d checks, %d failed\n", checks, failures + 1); return 1; }
  uint8_t ies[64] = { 0, 4, 'h', 'o', 'm', 'e', 48, 20, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 2, 0x0c, 0 };
  struct ow_join j = { .ssid_length = 4, .ssid = "home", .security = OW_SECURITY_AUTO, .key_length = 10, .key = "passphrase" };
  CHECK(br_supplicant_join(s, &j, ies, 28) == 0);
  CHECK(wait_events(1) && events[0] == BR_SUPPLICANT_JOINED && br_supplicant_frequency(s) == 5180);
  uint8_t mac[6];
  int32_t ids[8], poll[5];
  unsigned n = 8;
  CHECK(br_supplicant_status(s, mac, ids, &n, poll) == 0 && n == 1 && ids[0] == 2 && mac[5] == 0x55 && poll[1] == -51 && poll[2] == 866 && poll[4] == 5180);
  // A wrong key: the access point ends the handshake.
  nevents = 0;
  struct ow_join wrong = { .ssid_length = 8, .ssid = "wrongkey", .security = OW_SECURITY_AUTO, .key_length = 8, .key = "12345678" };
  CHECK(br_supplicant_join(s, &wrong, ies, 28) == 0);
  CHECK(wait_events(1) && events[0] == BR_SUPPLICANT_JOIN_FAILED && statuses[0] == OW_JOIN_AUTH && codes[0] == 15);
  // A network that is not there (two scans without it); no record of it: a probe for its name.
  nevents = 0;
  struct ow_join missing = { .ssid_length = 7, .ssid = "missing", .security = OW_SECURITY_OPEN };
  CHECK(br_supplicant_join(s, &missing, NULL, 0) == 0);
  CHECK(wait_events(1) && events[0] == BR_SUPPLICANT_JOIN_FAILED && statuses[0] == OW_JOIN_NO_NETWORK);
  // An association rejected: the access point's status.
  nevents = 0;
  struct ow_join reject = { .ssid_length = 6, .ssid = "reject", .security = OW_SECURITY_AUTO, .key_length = 32, .key_raw = 1, .key = { 0xaa } };
  reject.key[31] = 0xbb;
  CHECK(br_supplicant_join(s, &reject, ies, 28) == 0);
  CHECK(wait_events(1) && events[0] == BR_SUPPLICANT_JOIN_FAILED && statuses[0] == OW_JOIN_REJECTED && codes[0] == 17);
  // A link that drops after the join.
  nevents = 0;
  struct ow_join drops = { .ssid_length = 5, .ssid = "drops", .security = OW_SECURITY_AUTO, .key_length = 8, .key = "12345678" };
  CHECK(br_supplicant_join(s, &drops, ies, 28) == 0);
  CHECK(wait_events(2) && events[0] == BR_SUPPLICANT_JOINED && events[1] == BR_SUPPLICANT_DISCONNECTED && codes[1] == 4);
  CHECK(br_supplicant_leave(s) == 0);
  // A WPA3-only network with a WPA2 key: refused before the supplicant is asked.
  uint8_t sae[64];
  memcpy(sae, ies, 28);
  sae[25] = 8;
  CHECK(br_supplicant_join(s, &reject, sae, 28) == -EINVAL);
  br_supplicant_close(s);
  usleep(100000);
  char *log = stop_fake(pid, log_fd);
  // What the supplicant saw, in order: the interface added, the callback, One UI's two networks removed, the first
  // join's network (WPA2: 0x102 = WPA_PSK | WPA_PSK_SHA256, RSN, CCMP and the GCMPs) ...
  static const char *const expected[] = {
    "addStaInterface wlan0\nregisterCallback\ndisconnect\nremoveNetwork 0\nremoveNetwork 1\n",
    "disconnect\naddNetwork 2\nsetSsid home\nsetKeyMgmt 258\nsetProto 2\nsetPairwiseCipher 336\nsetGroupCipher 336\nsetPskPassphrase passphrase\nselect 2\n",
    "disconnect\nremoveNetwork 2\naddNetwork 3\nsetSsid wrongkey\n",
    // the open network no scan saw: no ciphers, a probe for the name
    "addNetwork 4\nsetSsid missing\nsetKeyMgmt 4\nsetScanSsid 1\nselect 4\n",
    "setSsid reject\nsetKeyMgmt 258\nsetProto 2\nsetPairwiseCipher 336\nsetGroupCipher 336\nsetPsk aa..bb\nselect 5\n",
    "select 6\ndisconnect\nremoveNetwork 6\n",
    // the close: nothing joined, the interface it added removed
    "removeInterface wlan0\n",
  };
  const char *at = log;
  for (unsigned i = 0; i < sizeof expected / sizeof *expected; i++) {
    const char *found = at ? strstr(at, expected[i]) : NULL;
    CHECK(found != NULL);
    if (!found) { printf("  not found after the last match: %s  the fake's log:\n%s\n", expected[i], log); break; }
    at = found + strlen(expected[i]);
  }
  CHECK(!strstr(log, "FAKE FAIL"));

  // 2. A supplicant that has the interface (One UI running): a look registers, adds and removes nothing.
  pid = start_fake(&log_fd, true);
  s = br_supplicant_open("wlan0", 3000, NULL, NULL);
  CHECK(s != NULL);
  n = 8;
  if (s) {
    CHECK(br_supplicant_status(s, mac, ids, &n, poll) == 0 && n == 2 && ids[0] == 0 && ids[1] == 1);
    br_supplicant_close(s);
  }
  // The session's client on the same supplicant: the interface is there, so it is not removed at the close.
  s = br_supplicant_open("wlan0", 3000, on_event, NULL);
  CHECK(s != NULL);
  if (s) br_supplicant_close(s);
  usleep(100000);
  log = stop_fake(pid, log_fd);
  CHECK(!strcmp(log, "registerCallback\ndisconnect\nremoveNetwork 0\nremoveNetwork 1\ndisconnect\nthe client died\n"));
  if (strcmp(log, "registerCallback\ndisconnect\nremoveNetwork 0\nremoveNetwork 1\ndisconnect\nthe client died\n")) printf("  the fake's log:\n%s\n", log);

  // 3. A supplicant that dies while joined: the event, and calls fail without waiting.
  pid = start_fake(&log_fd, true);
  s = br_supplicant_open("wlan0", 3000, on_event, NULL);
  CHECK(s != NULL);
  nevents = 0;
  if (s) {
    CHECK(br_supplicant_join(s, &j, ies, 28) == 0 && wait_events(1) && events[0] == BR_SUPPLICANT_JOINED);
    (void)stop_fake(pid, log_fd);
    CHECK(wait_events(2) && events[1] == BR_SUPPLICANT_DIED);
    CHECK(br_supplicant_leave(s) == -ENODEV);
    CHECK(br_supplicant_join(s, &j, ies, 28) != 0);                        // no service manager any more
    br_supplicant_close(s);
  }
  printf("aidl binder: %d checks, %d failed\n", checks, failures);
  return failures ? 1 : 0;
}
