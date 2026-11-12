// SPDX-License-Identifier: MIT
// Unit tests of the Wi-Fi bridge's pure parts (no phone, no binder, no radio): the frame rules and the cut of
// coalesced frames, the station against a fake radio, the AIDL parcels byte for byte, what a network's information
// elements ask for, the supplicant's callbacks decoded. The binder path: tests/test_aidl_binder.c.
#define _GNU_SOURCE
#include "../wifi/supplicant.h"
#include "../wifi/wifi.h"
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int checks, failures;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

static const uint8_t mac[6] = { 0x02, 0x11, 0x22, 0x33, 0x44, 0x55 };

// ---- frames ----
typedef struct { uint8_t frame[8][1600]; size_t length[8]; unsigned count; } sink;
static void take(void *ctx, const uint8_t *frame, size_t length)
{
  sink *s = ctx;
  if (s->count < 8 && length <= 1600) { memcpy(s->frame[s->count], frame, length); s->length[s->count] = length; }
  s->count++;
}
static uint32_t sum16(uint32_t sum, const uint8_t *p, size_t n)
{
  for (size_t i = 0; i + 1 < n; i += 2) sum += (uint32_t)(p[i] << 8 | p[i + 1]);
  if (n & 1) sum += (uint32_t)p[n - 1] << 8;
  return sum;
}
static bool sum_ok(uint32_t sum) { while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16); return sum == 0xffff; }
// The IPv4 header and TCP checksums of a frame are right.
static bool tcp4_ok(const uint8_t *f, size_t n)
{
  const uint8_t *ip = f + 14, *tcp = ip + 20;
  size_t tcp_len = n - 34;
  if ((size_t)(ip[2] << 8 | ip[3]) != n - 14) return false;
  return sum_ok(sum16(0, ip, 20)) && sum_ok(sum16(sum16(0, ip + 12, 8) + 6 + (uint32_t)tcp_len, tcp, tcp_len));
}
static bool tcp6_ok(const uint8_t *f, size_t n)
{
  const uint8_t *ip = f + 14, *tcp = ip + 40;
  size_t tcp_len = n - 54;
  if ((size_t)(ip[4] << 8 | ip[5]) != tcp_len) return false;
  return sum_ok(sum16(sum16(0, ip + 8, 32) + 6 + (uint32_t)tcp_len, tcp, tcp_len));
}
static size_t make_tcp(uint8_t *f, bool v6, size_t payload, uint8_t flags)
{
  size_t l3 = v6 ? 40 : 20;
  memset(f, 0, 14 + l3 + 20);
  memcpy(f, mac, 6);
  memcpy(f + 6, "\x02\xaa\xbb\xcc\xdd\xee", 6);
  f[12] = v6 ? 0x86 : 0x08; f[13] = v6 ? 0xdd : 0x00;
  uint8_t *ip = f + 14;
  if (v6) { ip[0] = 0x60; ip[6] = 6; ip[7] = 64; ip[8] = 0xfe; ip[9] = 0x80; ip[23] = 1; ip[24] = 0xfe; ip[25] = 0x80; ip[39] = 2; }
  else { ip[0] = 0x45; ip[4] = 0x12; ip[5] = 0x34; ip[8] = 64; ip[9] = 6; memcpy(ip + 12, "\xc0\xa8\x01\x01\xc0\xa8\x01\x02", 8); }
  uint8_t *tcp = ip + l3;
  tcp[0] = 0x01; tcp[1] = 0xbb; tcp[2] = 0xc0; tcp[3] = 0x00; tcp[4] = 0x00; tcp[5] = 0x00; tcp[6] = 0xff; tcp[7] = 0x00;   // seq 0x0000ff00
  tcp[12] = 0x50; tcp[13] = flags; tcp[14] = 0xff; tcp[15] = 0xff;
  for (size_t i = 0; i < payload; i++) tcp[20 + i] = (uint8_t)(i * 7 + 1);
  return 14 + l3 + 20 + payload;
}

static void test_frames(void)
{
  uint8_t f[64] = { 0 };
  memcpy(f, "\xff\xff\xff\xff\xff\xff", 6); memcpy(f + 6, mac, 6); f[12] = 0x08; f[13] = 0x06;
  CHECK(br_wifi_ethertype(f, 60) == 0x0806 && br_wifi_ethertype(f, 13) == 0);
  CHECK(br_wifi_to_radio(f, 60, mac));                                     // the guest's ARP request
  f[6] ^= 4;
  CHECK(!br_wifi_to_radio(f, 60, mac));                                    // another source address: a private address of iOS's
  f[6] ^= 4; f[12] = 0x88; f[13] = 0x8e;
  CHECK(!br_wifi_to_radio(f, 60, mac) && !br_wifi_to_guest(f, 60, mac));   // EAPOL is the supplicant's, both ways
  f[13] = 0xb5;
  CHECK(!br_wifi_to_radio(f, 60, mac) && !br_wifi_to_guest(f, 60, mac));   // the station's own messages never cross
  f[12] = 0x08; f[13] = 0x00;
  CHECK(br_wifi_to_guest(f, 60, mac));                                     // broadcast
  memcpy(f, "\x33\x33\x00\x00\x00\x01", 6);
  CHECK(br_wifi_to_guest(f, 60, mac));                                     // multicast (IPv6 all nodes)
  memcpy(f, mac, 6);
  CHECK(br_wifi_to_guest(f, 60, mac));
  f[5] ^= 1;
  CHECK(!br_wifi_to_guest(f, 60, mac) && !br_wifi_to_guest(f, 10, mac) && !br_wifi_to_radio(f, 1515, mac));

  static uint8_t big[14 + 40 + 20 + 4000];
  static sink s;
  // A frame of the link's size goes as it is.
  size_t n = make_tcp(big, false, 1000, 0x18);
  memset(&s, 0, sizeof s);
  CHECK(br_wifi_segment(big, n, 1448, false, take, &s) == 1 && s.count == 1 && s.length[0] == n && !memcmp(s.frame[0], big, n));
  // A coalesced IPv4 frame of 4000 bytes: three segments, lengths, sequence numbers, identifiers, flags, checksums.
  n = make_tcp(big, false, 4000, 0x19);                                    // FIN PSH ACK
  memset(&s, 0, sizeof s);
  CHECK(br_wifi_segment(big, n, 1448, true, take, &s) == 3 && s.count == 3);
  CHECK(s.length[0] == 54 + 1448 && s.length[1] == 54 + 1448 && s.length[2] == 54 + 1104);
  CHECK(tcp4_ok(s.frame[0], s.length[0]) && tcp4_ok(s.frame[1], s.length[1]) && tcp4_ok(s.frame[2], s.length[2]));
  CHECK(s.frame[1][38] == 0x00 && s.frame[1][39] == 0x01 && s.frame[1][40] == 0x04 && s.frame[1][41] == 0xa8);   // 0xff00 + 1448
  CHECK(s.frame[0][47] == 0x10 && s.frame[1][47] == 0x10 && s.frame[2][47] == 0x19);                             // FIN and PSH on the last
  CHECK(s.frame[0][19] == 0x34 && s.frame[1][19] == 0x35 && s.frame[2][19] == 0x36);
  CHECK(!memcmp(s.frame[2] + 54, big + 54 + 2896, 1104));
  // A larger MSS than the link holds is cut to the link.
  memset(&s, 0, sizeof s);
  CHECK(br_wifi_segment(big, n, 9000, true, take, &s) == 3 && s.length[0] == 1514);
  // IPv6.
  n = make_tcp(big, true, 3000, 0x10);
  memset(&s, 0, sizeof s);
  CHECK(br_wifi_segment(big, n, 1428, true, take, &s) == 3 && s.length[2] == 74 + 144 && tcp6_ok(s.frame[0], s.length[0]) && tcp6_ok(s.frame[2], s.length[2]));
  // Coalesced but of the link's size: one frame, its checksum made (the receive path left none).
  n = make_tcp(big, false, 900, 0x18);
  memset(&s, 0, sizeof s);
  CHECK(br_wifi_segment(big, n, 1448, true, take, &s) == 1 && s.length[0] == n && tcp4_ok(s.frame[0], n));
  // Too long and not TCP: dropped. Of the link's size and not TCP: as it is.
  big[14 + 9] = 17;
  memset(&s, 0, sizeof s);
  CHECK(br_wifi_segment(big, 3000, 1448, true, take, &s) == 0 && s.count == 0);
  CHECK(br_wifi_segment(big, 600, 1448, true, take, &s) == 1 && s.count == 1 && !memcmp(s.frame[0], big, 600));
}

// ---- the station against a fake radio ----
typedef struct { unsigned scans, joins, leaves; int join_rc; struct ow_join join; bool had_bss; br_wifi_bss bss; br_wifi_link link; } fake_radio;
static int fr_scan(void *ctx) { ((fake_radio *)ctx)->scans++; return 0; }
static int fr_join(void *ctx, const struct ow_join *j, const br_wifi_bss *bss)
{
  fake_radio *r = ctx;
  r->joins++; r->join = *j; r->had_bss = bss != NULL;
  if (bss) r->bss = *bss;
  return r->join_rc;
}
static int fr_leave(void *ctx) { ((fake_radio *)ctx)->leaves++; return 0; }
static int fr_link(void *ctx, br_wifi_link *out) { *out = ((fake_radio *)ctx)->link; return 0; }
static const br_wifi_radio fake = { fr_scan, fr_join, fr_leave, fr_link };

static size_t guest_frame(uint8_t *f, uint16_t type, const void *payload, uint16_t n)
{
  struct ow_header h = { OW_MAGIC, type, n };
  memcpy(f, ow_address, 6); memcpy(f + 6, mac, 6); f[12] = 0x88; f[13] = 0xb5;
  memcpy(f + 14, &h, sizeof h);
  if (n) memcpy(f + 22, payload, n);
  return 22u + n;
}
// The message of host frame i: its type; *payload its bytes.
static unsigned host_message(const sink *s, unsigned i, const uint8_t **payload, size_t *n)
{
  struct ow_header h;
  if (i >= s->count || s->length[i] < 22 || memcmp(s->frame[i], mac, 6) || memcmp(s->frame[i] + 6, ow_address, 6)) return 0;
  memcpy(&h, s->frame[i] + 14, sizeof h);
  if (h.magic != OW_MAGIC || 22u + h.length != s->length[i]) return 0;
  *payload = s->frame[i] + 22; *n = h.length;
  return h.type;
}

static void test_station(void)
{
  static sink s;
  fake_radio r = { 0 };
  uint8_t f[256];
  const uint8_t *p = f;
  size_t n = 0;
  struct ow_state st;
  br_wifi_station *sta = br_wifi_station_create(&fake, &r, mac, "IN", take, &s);
  CHECK(sta != NULL);
  struct ow_hello hello = { OW_VERSION };
  br_wifi_station_message(sta, f, guest_frame(f, OW_HELLO, &hello, sizeof hello));
  CHECK(s.count == 1 && host_message(&s, 0, &p, &n) == OW_STATE && n == sizeof st);
  memcpy(&st, p, sizeof st);
  CHECK(st.version == OW_VERSION && st.radio == OW_RADIO_OK && st.link == OW_LINK_IDLE && !memcmp(st.mac, mac, 6) && st.country[0] == 'I' && st.country[1] == 'N');

  // A scan: two networks, the second seen twice (the stronger one is the join's), elements cut to whole ones.
  memset(&s, 0, sizeof s);
  br_wifi_station_message(sta, f, guest_frame(f, OW_SCAN, NULL, 0));
  br_wifi_station_message(sta, f, guest_frame(f, OW_SCAN, NULL, 0));       // a second request while one runs: one scan
  CHECK(r.scans == 1 && s.count == 0);
  static uint8_t ies[1100];
  ies[0] = 0; ies[1] = 4; memcpy(ies + 2, "home", 4);                      // SSID
  ies[6] = 48; ies[7] = 20;                                                // RSN (20 bytes)
  for (unsigned at = 28; at + 102 <= sizeof ies; at += 102) { ies[at] = 221; ies[at + 1] = 100; }   // vendor elements up to 1048
  struct ow_bss b = { .bssid = { 1, 2, 3, 4, 5, 6 }, .freq_mhz = 5180, .signal_dbm = -70, .ssid_length = 4, .ssid = "home" };
  br_wifi_scan_add(sta, &b, ies, 1048);
  b.bssid[5] = 7; b.signal_dbm = -50; b.freq_mhz = 2437;
  br_wifi_scan_add(sta, &b, ies, 28);
  struct ow_bss other = { .bssid = { 9, 9, 9, 9, 9, 9 }, .freq_mhz = 2412, .signal_dbm = -80, .ssid_length = 5, .ssid = "other" };
  br_wifi_scan_add(sta, &other, NULL, 0);
  br_wifi_scan_done(sta, 0);
  CHECK(s.count == 4 && host_message(&s, 0, &p, &n) == OW_BSS && n == sizeof b + 946);     // 28 + 9 * 102: the tenth does not fit 1024
  struct ow_bss got;
  memcpy(&got, p, sizeof got);
  CHECK(got.ie_length == 946 && got.freq_mhz == 5180 && !memcmp(p + sizeof got, ies, 946));
  CHECK(host_message(&s, 2, &p, &n) == OW_BSS && n == sizeof b);
  struct ow_scan_done done;
  CHECK(host_message(&s, 3, &p, &n) == OW_SCAN_DONE && n == sizeof done);
  memcpy(&done, p, sizeof done);
  CHECK(done.count == 3 && done.error == 0);

  // A join: the radio gets the request and the stronger record; the result follows the network's state.
  memset(&s, 0, sizeof s);
  struct ow_join j = { .ssid_length = 4, .ssid = "home", .security = OW_SECURITY_AUTO, .key_length = 8, .key = "password" };
  br_wifi_station_message(sta, f, guest_frame(f, OW_JOIN, &j, sizeof j));
  CHECK(r.joins == 1 && r.had_bss && r.bss.bss.bssid[5] == 7 && r.join.key_length == 8 && !memcmp(r.join.key, "password", 8) && s.count == 0);
  r.link = (br_wifi_link){ .up = true, .rssi_dbm = -48, .noise_dbm = -95, .tx_rate_mbps = 433, .freq_mhz = 2437, .bssid = { 1, 2, 3, 4, 5, 7 } };
  br_wifi_join_done(sta, OW_JOIN_OK, 0);
  CHECK(s.count == 2 && host_message(&s, 0, &p, &n) == OW_STATE && n == sizeof st + 28);
  memcpy(&st, p, sizeof st);
  CHECK(st.link == OW_LINK_UP && st.rssi_dbm == -48 && st.tx_rate_mbps == 433 && st.bss.freq_mhz == 2437 && st.bss.ssid_length == 4 && st.bss.ie_length == 28);
  struct ow_join_result result;
  CHECK(host_message(&s, 1, &p, &n) == OW_JOIN_RESULT && n == sizeof result);
  memcpy(&result, p, sizeof result);
  CHECK(result.status == OW_JOIN_OK);
  br_wifi_join_done(sta, OW_JOIN_OK, 0);                                   // a second result: none is pending
  CHECK(s.count == 2);

  // The link drops: the state says idle and why.
  memset(&s, 0, sizeof s);
  r.link = (br_wifi_link){ .up = false, .reason = 4 };
  br_wifi_link_changed(sta);
  CHECK(s.count == 1 && host_message(&s, 0, &p, &n) == OW_STATE && n == sizeof st);
  memcpy(&st, p, sizeof st);
  CHECK(st.link == OW_LINK_IDLE && st.reason == 4);

  // A network no scan saw; the radio refuses: the result says so.
  memset(&s, 0, sizeof s);
  struct ow_join hidden = { .ssid_length = 6, .ssid = "hidden", .security = OW_SECURITY_OPEN };
  r.join_rc = -ENOENT;
  br_wifi_station_message(sta, f, guest_frame(f, OW_JOIN, &hidden, sizeof hidden));
  CHECK(r.joins == 2 && !r.had_bss && s.count == 1 && host_message(&s, 0, &p, &n) == OW_JOIN_RESULT);
  memcpy(&result, p, sizeof result);
  CHECK(result.status == OW_JOIN_NO_NETWORK);
  // A failed join reports its code.
  memset(&s, 0, sizeof s);
  r.join_rc = 0;
  br_wifi_station_message(sta, f, guest_frame(f, OW_JOIN, &j, sizeof j));
  br_wifi_join_done(sta, OW_JOIN_AUTH, 15);
  CHECK(s.count == 1 && host_message(&s, 0, &p, &n) == OW_JOIN_RESULT);
  memcpy(&result, p, sizeof result);
  CHECK(result.status == OW_JOIN_AUTH && result.code == 15);
  // Leave.
  memset(&s, 0, sizeof s);
  br_wifi_station_message(sta, f, guest_frame(f, OW_LEAVE, NULL, 0));
  CHECK(r.leaves == 1 && s.count == 1 && host_message(&s, 0, &p, &n) == OW_STATE);
  // Malformed: a short frame, a wrong magic, a length past the frame, a join with no name.
  memset(&s, 0, sizeof s);
  br_wifi_station_message(sta, f, 10);
  n = guest_frame(f, OW_SCAN, NULL, 0); f[14] ^= 1;
  br_wifi_station_message(sta, f, n);
  n = guest_frame(f, OW_JOIN, &j, sizeof j);
  br_wifi_station_message(sta, f, n - 1);
  struct ow_join unnamed = { 0 };
  br_wifi_station_message(sta, f, guest_frame(f, OW_JOIN, &unnamed, sizeof unnamed));
  CHECK(s.count == 0 && r.joins == 3 && r.scans == 1);
  br_wifi_station_destroy(sta);
}

// ---- the parcels ----
static void test_parcels(void)
{
  static ab_parcel p;
  sup_parcel_get_service(&p, "ab/c");
  static const uint8_t token[] = { 0, 0, 0, 0x80, 0xff, 0xff, 0xff, 0xff, 'T', 'S', 'Y', 'S', 26, 0, 0, 0, 'a', 0, 'n', 0, 'd', 0, 'r', 0 };
  CHECK(!p.overflow && p.nobjects == 0 && !memcmp(p.data, token, sizeof token));
  // "android.os.IServiceManager": 26 units and a terminator, 54 bytes padded to 56; then "ab/c": 4 units, 10 bytes padded to 12
  static const uint8_t name[] = { 4, 0, 0, 0, 'a', 0, 'b', 0, '/', 0, 'c', 0, 0, 0, 0, 0 };
  CHECK(p.size == 12 + 4 + 56 + 16 && !memcmp(p.data + 72, name, sizeof name));
  ab_reader r;
  char text[64];
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(ab_get_token(&r, SUP_MANAGER) && ab_get_string(&r, text, sizeof text) && !strcmp(text, "ab/c") && !r.bad && r.position == p.size);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(!ab_get_token(&r, SUP_ISUPPLICANT));

  // A callback object: the kernel's object, listed, then the stability word.
  sup_parcel_callback(&p, 0x1122334455667788ull);
  size_t at = 12 + 4 + (((strlen(SUP_STA_IFACE) + 1) * 2 + 3) & ~3u);
  static const uint8_t object[] = { 0x85, 0x2a, 0x62, 0x73, 0x13, 0x01, 0, 0, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
                                    0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x3f, 0, 0, 0 };
  CHECK(p.nobjects == 1 && p.offsets[0] == at && p.size == at + 28 && !memcmp(p.data + at, object, sizeof object));
  // As its receiver sees it: a handle. Not listed in the offsets: refused.
  uint8_t copy[256];
  memcpy(copy, p.data, p.size);
  uint32_t handle_type = AB_TYPE_HANDLE, handle = 7, got = 0;
  memcpy(copy + at, &handle_type, 4); memcpy(copy + at + 8, &handle, 4); memset(copy + at + 12, 0, 12);
  ab_reader_init(&r, copy, p.size, p.offsets, 8);
  CHECK(ab_get_token(&r, SUP_STA_IFACE) && ab_get_binder(&r, &got, NULL) && got == 7 && !r.bad);
  ab_reader_init(&r, copy, p.size, NULL, 0);
  CHECK(ab_get_token(&r, SUP_STA_IFACE) && !ab_get_binder(&r, &got, NULL) && r.bad);
  // A null binder: not listed, not an error.
  ab_init(&p);
  ab_put_ok(&p);
  ab_put_null_binder(&p);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(p.nobjects == 0 && ab_get_status(&r, NULL) == 0 && !ab_get_binder(&r, &got, NULL) && !r.bad && got == 0);

  // byte[] and int, a key wiped.
  sup_parcel_bytes(&p, SUP_STA_NETWORK, (const uint8_t *)"secret-key", 10);
  at = p.size - 16;
  CHECK(p.data[at] == 10 && !memcmp(p.data + at + 4, "secret-key\0\0", 12));
  ab_wipe(&p);
  CHECK(p.size == 0 && !memchr(p.data, 's', sizeof p.data));
  sup_parcel_i32(&p, SUP_STA_NETWORK, KM_WPA_PSK | KM_WPA_PSK_SHA256);
  CHECK(p.data[p.size - 4] == 0x02 && p.data[p.size - 3] == 0x01);

  // A reply's status: ok; a service's error; an exception with a stack trace header; a truncated one.
  ab_init(&p);
  ab_put_service_error(&p, 4);
  ab_put_i32(&p, 77);
  int32_t specific = 0;
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(ab_get_status(&r, &specific) == AB_EX_SERVICE_SPECIFIC && specific == 4 && ab_get_i32(&r) == 77 && !r.bad);
  ab_init(&p);
  ab_put_i32(&p, -3); ab_put_string(&p, "bad"); ab_put_i32(&p, 12); ab_put_i32(&p, 1); ab_put_i32(&p, 2); ab_put_i32(&p, 99);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(ab_get_status(&r, &specific) == -3 && specific == -3 && ab_get_i32(&r) == 99);
  ab_reader_init(&r, p.data, 6, NULL, 0);
  (void)ab_get_status(&r, &specific);
  CHECK(r.bad);
  // Strings: a null one, one cut to the buffer, one whose length is past the parcel.
  ab_init(&p);
  ab_put_string(&p, NULL); ab_put_string(&p, "abcdefgh"); ab_put_i32(&p, 1000);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(ab_get_string(&r, text, sizeof text) && !*text && ab_get_string(&r, text, 5) && !strcmp(text, "abcd") && !ab_get_string(&r, text, sizeof text) && r.bad);
  // The standard answers.
  CHECK(ab_standard(AB_INTERFACE, SUP_STA_CALLBACK, 2, SUP_HASH, &p) && p.data[0] == strlen(SUP_STA_CALLBACK));
  CHECK(ab_standard(AB_GET_VERSION, SUP_STA_CALLBACK, 2, SUP_HASH, &p) && p.size == 8 && p.data[4] == 2);
  CHECK(ab_standard(AB_GET_HASH, SUP_STA_CALLBACK, 2, SUP_HASH, &p) && p.data[4] == 40 && ab_standard(AB_PING, "", 0, "", &p) && p.size == 0);
  CHECK(!ab_standard(CB_DISCONNECTED, SUP_STA_CALLBACK, 2, SUP_HASH, &p));
  // An overflow is kept.
  ab_init(&p);
  static uint8_t much[5000];
  ab_put_bytes(&p, much, sizeof much);
  ab_put_i32(&p, 1);
  CHECK(p.overflow);
}

// ---- what a network asks for ----
static size_t rsn(uint8_t *out, const uint8_t *pairwise, unsigned npairwise, const uint8_t *akm, unsigned nakm, uint16_t caps)
{
  size_t n = 2;
  out[0] = 48;
  out[n++] = 1; out[n++] = 0;
  memcpy(out + n, "\x00\x0f\xac\x04", 4); n += 4;
  out[n++] = (uint8_t)npairwise; out[n++] = 0;
  for (unsigned i = 0; i < npairwise; i++) { memcpy(out + n, "\x00\x0f\xac", 3); out[n + 3] = pairwise[i]; n += 4; }
  out[n++] = (uint8_t)nakm; out[n++] = 0;
  for (unsigned i = 0; i < nakm; i++) { memcpy(out + n, "\x00\x0f\xac", 3); out[n + 3] = akm[i]; n += 4; }
  out[n++] = (uint8_t)caps; out[n++] = (uint8_t)(caps >> 8);
  out[1] = (uint8_t)(n - 2);
  return n;
}

static void test_security(void)
{
  uint8_t ies[128];
  br_wifi_security s;
  br_wifi_plan plan;
  size_t n = 6;
  memcpy(ies, "\x00\x04home", 6);
  struct ow_join pass = { .ssid_length = 4, .ssid = "home", .security = OW_SECURITY_AUTO, .key_length = 8, .key = "password" };
  struct ow_join raw = { .ssid_length = 4, .ssid = "home", .security = OW_SECURITY_AUTO, .key_length = 32, .key_raw = 1 };
  struct ow_join open = { .ssid_length = 4, .ssid = "home", .security = OW_SECURITY_OPEN };
  // WPA2: CCMP, PSK.
  n = 6 + rsn(ies + 6, (const uint8_t[]){ 4 }, 1, (const uint8_t[]){ 2 }, 1, 0x000c);
  br_wifi_security_parse(ies, n, &s);
  CHECK(s.rsn && s.psk && s.ccmp && !s.sae && !s.wpa && !s.tkip && !s.pmf_capable && !s.pmf_required);
  CHECK(br_wifi_plan_join(&pass, &s, &plan) == 0 && plan.key_mgmt == (KM_WPA_PSK | KM_WPA_PSK_SHA256) && plan.proto == PROTO_RSN && plan.psk_passphrase &&
        !plan.psk_raw && !plan.sae_password && !plan.set_pmf && !plan.scan_ssid && plan.pairwise == (PC_CCMP | PC_GCMP_128 | PC_GCMP_256));
  CHECK(br_wifi_plan_join(&raw, &s, &plan) == 0 && plan.psk_raw && !plan.psk_passphrase && plan.key_mgmt == (KM_WPA_PSK | KM_WPA_PSK_SHA256));
  // WPA3 only: SAE, management frame protection required.
  n = 6 + rsn(ies + 6, (const uint8_t[]){ 4 }, 1, (const uint8_t[]){ 8 }, 1, 0x00cc);
  br_wifi_security_parse(ies, n, &s);
  CHECK(s.sae && !s.psk && s.pmf_capable && s.pmf_required);
  CHECK(br_wifi_plan_join(&pass, &s, &plan) == 0 && plan.key_mgmt == KM_SAE && plan.sae_password && plan.set_pmf && plan.require_pmf && !plan.psk_passphrase);
  CHECK(br_wifi_plan_join(&raw, &s, &plan) == -EINVAL);                    // a WPA2 key cannot open it
  // WPA2/WPA3 transition: the passphrase as WPA2.
  n = 6 + rsn(ies + 6, (const uint8_t[]){ 4 }, 1, (const uint8_t[]){ 2, 8 }, 2, 0x008c);
  br_wifi_security_parse(ies, n, &s);
  CHECK(s.sae && s.psk && s.pmf_capable && !s.pmf_required);
  CHECK(br_wifi_plan_join(&pass, &s, &plan) == 0 && plan.psk_passphrase && plan.key_mgmt == (KM_WPA_PSK | KM_WPA_PSK_SHA256) && !plan.set_pmf);
  // WPA (version 1) with TKIP beside WPA2: both protocols and TKIP allowed.
  n = 6 + rsn(ies + 6, (const uint8_t[]){ 2, 4 }, 2, (const uint8_t[]){ 2 }, 1, 0);
  static const uint8_t wpa1[] = { 221, 22, 0x00, 0x50, 0xf2, 1, 1, 0, 0x00, 0x50, 0xf2, 2, 1, 0, 0x00, 0x50, 0xf2, 2, 1, 0, 0x00, 0x50, 0xf2, 2 };
  memcpy(ies + n, wpa1, sizeof wpa1);
  br_wifi_security_parse(ies, n + sizeof wpa1, &s);
  CHECK(s.rsn && s.wpa && s.tkip && s.ccmp && s.psk);
  CHECK(br_wifi_plan_join(&pass, &s, &plan) == 0 && plan.proto == (PROTO_RSN | PROTO_WPA) && (plan.pairwise & PC_TKIP) && (plan.group & GC_TKIP));
  // Open; enhanced open; no record of the network (hidden).
  br_wifi_security_parse(ies, 6, &s);
  CHECK(!s.rsn && !s.wpa && br_wifi_plan_join(&open, &s, &plan) == 0 && plan.key_mgmt == KM_NONE && !plan.proto && !plan.scan_ssid);
  n = 6 + rsn(ies + 6, (const uint8_t[]){ 4 }, 1, (const uint8_t[]){ 18 }, 1, 0x00cc);
  br_wifi_security_parse(ies, n, &s);
  CHECK(s.owe && br_wifi_plan_join(&open, &s, &plan) == 0 && plan.key_mgmt == KM_OWE && plan.require_pmf);
  CHECK(br_wifi_plan_join(&pass, NULL, &plan) == 0 && plan.scan_ssid && plan.psk_passphrase && (plan.proto & PROTO_WPA));
  CHECK(br_wifi_plan_join(&open, NULL, &plan) == 0 && plan.scan_ssid && plan.key_mgmt == KM_NONE);
  // Elements cut short or with lengths past the end: nothing read past them.
  n = 6 + rsn(ies + 6, (const uint8_t[]){ 4 }, 1, (const uint8_t[]){ 2 }, 1, 0x000c);
  for (size_t cut = 0; cut < n; cut++) br_wifi_security_parse(ies, cut, &s);
  ies[6 + 2 + 6] = 200;                                                    // a pairwise count past the element
  br_wifi_security_parse(ies, n, &s);
  CHECK(s.rsn && !s.psk && !s.ccmp);
}

// ---- the supplicant's callbacks ----
static void test_callbacks(void)
{
  static ab_parcel p;
  ab_reader r;
  sup_event e;
  static const uint8_t bssid[6] = { 1, 2, 3, 4, 5, 6 };
  // onStateChanged(newState, bssid, id, ssid, filsHlpSent)
  ab_init(&p);
  ab_put_token(&p, SUP_STA_CALLBACK);
  ab_put_i32(&p, ST_FOURWAY_HANDSHAKE); ab_put_bytes(&p, bssid, 6); ab_put_i32(&p, 3); ab_put_bytes(&p, (const uint8_t *)"home", 4); ab_put_i32(&p, 0);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(ab_get_token(&r, SUP_STA_CALLBACK));
  sup_decode_callback(CB_STATE_CHANGED, &r, &e);
  CHECK(e.kind == CB_STATE_CHANGED && e.state == ST_FOURWAY_HANDSHAKE && e.id == 3 && !memcmp(e.bssid, bssid, 6));
  // onSupplicantStateChanged(SupplicantStateChangeData)
  ab_init(&p);
  ab_put_i32(&p, 1);
  size_t at = p.size;
  ab_put_i32(&p, 0);
  ab_put_i32(&p, ST_COMPLETED); ab_put_i32(&p, 3); ab_put_bytes(&p, (const uint8_t *)"home", 4); ab_put_bytes(&p, bssid, 6);
  ab_put_i32(&p, KM_WPA_PSK); ab_put_i32(&p, 5180); ab_put_i32(&p, 0);
  int32_t size = (int32_t)(p.size - at);
  memcpy(p.data + at, &size, 4);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  sup_decode_callback(CB_SUPPLICANT_STATE_CHANGED, &r, &e);
  CHECK(e.kind == CB_SUPPLICANT_STATE_CHANGED && e.state == ST_COMPLETED && e.frequency == 5180 && e.id == 3 && !memcmp(e.bssid, bssid, 6));
  ab_reader_init(&r, p.data, p.size - 8, NULL, 0);                         // cut short: not used
  sup_decode_callback(CB_SUPPLICANT_STATE_CHANGED, &r, &e);
  CHECK(e.kind == 0);
  // onDisconnected(bssid, locallyGenerated, reasonCode)
  ab_init(&p);
  ab_put_bytes(&p, bssid, 6); ab_put_i32(&p, 1); ab_put_i32(&p, 3);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  sup_decode_callback(CB_DISCONNECTED, &r, &e);
  CHECK(e.kind == CB_DISCONNECTED && e.local && e.code == 3);
  // onAssociationRejected(AssociationRejectionData: ssid, bssid, statusCode, timedOut, ...)
  ab_init(&p);
  ab_put_i32(&p, 1);
  at = p.size;
  ab_put_i32(&p, 0);
  ab_put_bytes(&p, (const uint8_t *)"home", 4); ab_put_bytes(&p, bssid, 6); ab_put_i32(&p, 17); ab_put_i32(&p, 0);
  ab_put_i32(&p, 0); ab_put_i32(&p, 0); ab_put_i32(&p, 0);
  size = (int32_t)(p.size - at);
  memcpy(p.data + at, &size, 4);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  sup_decode_callback(CB_ASSOCIATION_REJECTED, &r, &e);
  CHECK(e.kind == CB_ASSOCIATION_REJECTED && e.code == 17 && !e.timed_out);
  // One the bridge does not use.
  sup_decode_callback(20, &r, &e);
  CHECK(e.kind == 0);
}

int main(void)
{
  test_frames();
  test_station();
  test_parcels();
  test_security();
  test_callbacks();
  printf("wifi: %d checks, %d failed\n", checks, failures);
  return failures ? 1 : 0;
}
