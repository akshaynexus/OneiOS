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

  // A DHCP OFFER (server to client) and a DISCOVER; a frame that is UDP but not DHCP; one cut inside its options.
  static uint8_t d[14 + 20 + 8 + 240 + 16];
  memset(d, 0, sizeof d);
  memcpy(d, mac, 6); d[12] = 0x08; d[14] = 0x45; d[23] = 17;
  d[34] = 0; d[35] = 67; d[36] = 0; d[37] = 68;                              // UDP 67 -> 68
  uint8_t *bootp = d + 42;
  bootp[0] = 2; memcpy(bootp + 16, "\x0a\x00\x01\x37", 4);                   // yiaddr 10.0.1.55
  memcpy(bootp + 236, "\x63\x82\x53\x63", 4);
  memcpy(bootp + 240, "\x35\x01\x02\x36\x04\x0a\x00\x01\x01\xff", 10);       // OFFER, server 10.0.1.1
  br_wifi_dhcp_msg dm;
  CHECK(br_wifi_dhcp(d, sizeof d, &dm) && dm.type == 2 && dm.yiaddr[3] == 0x37 && dm.server[3] == 1 && dm.has_server);
  d[35] = 68; d[37] = 67; bootp[242] = 1;
  CHECK(br_wifi_dhcp(d, sizeof d, &dm) && dm.type == 1);
  d[37] = 53;
  CHECK(!br_wifi_dhcp(d, sizeof d, &dm));
  d[37] = 67;
  CHECK(!br_wifi_dhcp(d, 14 + 20 + 8 + 240 + 2, &dm) && !br_wifi_dhcp(d, 100, &dm));
}

// ---- DHCP: the REQUEST forms of RFC 2131 4.3.2 as clients send them ----
// An Ethernet frame with a DHCP message: the IPv4 header (checksummed), UDP, the BOOTP header with chaddr, the cookie
// and the options as iOS's and Android's clients put them (53 first, client id 61, the parameter list 55, 57, the
// requested address 50 and the server 54 where the state has them, 12 a host name, then the end).
typedef struct {
  const uint8_t *eth_dst, *eth_src, *client;
  const char *ip_src, *ip_dst, *ciaddr, *yiaddr, *requested, *server;   // dotted, NULL: none / 0.0.0.0
  uint8_t op, type;
  bool broadcast_flag;
} dhcp_spec;
static void ip4(const char *s, uint8_t out[4])
{
  unsigned a = 0, b = 0, c = 0, e = 0;
  if (s) sscanf(s, "%u.%u.%u.%u", &a, &b, &c, &e);
  out[0] = (uint8_t)a; out[1] = (uint8_t)b; out[2] = (uint8_t)c; out[3] = (uint8_t)e;
}
static size_t make_dhcp(uint8_t *f, const dhcp_spec *s)
{
  memset(f, 0, 600);
  memcpy(f, s->eth_dst, 6); memcpy(f + 6, s->eth_src, 6); f[12] = 0x08; f[13] = 0x00;
  uint8_t *ip = f + 14, *udp = ip + 20, *b = udp + 8, *o = b + 240;
  memcpy(b + 236, "\x63\x82\x53\x63", 4);
  b[0] = s->op; b[1] = 1; b[2] = 6; b[4] = 0x5a; b[5] = 0x1e; b[6] = 0x77; b[7] = 0x03;   // xid
  if (s->broadcast_flag) b[10] = 0x80;
  ip4(s->ciaddr, b + 12); ip4(s->yiaddr, b + 16); memcpy(b + 28, s->client, 6);
  *o++ = 53; *o++ = 1; *o++ = s->type;
  *o++ = 61; *o++ = 7; *o++ = 1; memcpy(o, s->client, 6); o += 6;
  if (s->op == 1) { static const uint8_t prl[] = { 55, 6, 1, 121, 3, 6, 15, 119 }; memcpy(o, prl, sizeof prl); o += sizeof prl; *o++ = 57; *o++ = 2; *o++ = 5; *o++ = 0xdc; }
  if (s->requested) { *o++ = 50; *o++ = 4; ip4(s->requested, o); o += 4; }
  if (s->server) { *o++ = 54; *o++ = 4; ip4(s->server, o); o += 4; }
  if (s->op == 1) { *o++ = 12; *o++ = 6; memcpy(o, "iPhone", 6); o += 6; }
  else { *o++ = 51; *o++ = 4; o[2] = 0x0e; o[3] = 0x10; o += 4; *o++ = 1; *o++ = 4; ip4("255.255.0.0", o); o += 4; }
  *o++ = 255;
  while ((size_t)(o - b) < 300) *o++ = 0;   // BOOTP's minimum of 300 bytes, as clients pad
  const size_t bootp = (size_t)(o - b), udp_len = 8 + bootp, ip_len = 20 + udp_len;
  ip[0] = 0x45; ip[2] = (uint8_t)(ip_len >> 8); ip[3] = (uint8_t)ip_len; ip[8] = 64; ip[9] = 17;
  ip4(s->ip_src, ip + 12); ip4(s->ip_dst, ip + 16);
  uint32_t sum = sum16(0, ip, 20);
  while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
  ip[10] = (uint8_t)(~sum >> 8); ip[11] = (uint8_t)~sum;
  udp[1] = s->op == 1 ? 68 : 67; udp[3] = s->op == 1 ? 67 : 68; udp[4] = (uint8_t)(udp_len >> 8); udp[5] = (uint8_t)udp_len;
  return 14 + ip_len;
}

static void test_dhcp(void)
{
  static const uint8_t bcast[6] = { 0xff, 0xff, 0xff, 0xff, 0xff, 0xff };
  static const uint8_t other[6] = { 0x3c, 0x22, 0xfb, 0x10, 0x20, 0x30 };    // another station on the network
  static const uint8_t router[6] = { 0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e };   // the access point's gateway
  uint8_t f[600];
  char line[160];
  br_wifi_dhcp_msg m;
  uint8_t a[4];

  // What the log said before 2026-11-13 for a REQUEST: its yiaddr and option 54. Only a SELECTING REQUEST has a
  // server; no REQUEST has a yiaddr. So "REQUEST of 0.0.0.0 from 0.0.0.0" is every INIT-REBOOT, RENEWING or
  // REBINDING REQUEST that came in from the radio (frames iOS sent were logged as "iOS sends ...").
  const dhcp_spec init_reboot = { bcast, other, other, "0.0.0.0", "255.255.255.255", NULL, NULL, "172.16.235.99", NULL, 1, 3, false };
  const dhcp_spec rebinding = { bcast, other, other, "172.16.235.80", "255.255.255.255", "172.16.235.80", NULL, NULL, NULL, 1, 3, false };
  const dhcp_spec renewing = { router, mac, mac, "172.16.235.71", "172.16.0.1", "172.16.235.71", NULL, NULL, NULL, 1, 3, false };
  const dhcp_spec selecting = { bcast, mac, mac, "0.0.0.0", "255.255.255.255", NULL, NULL, "172.16.235.71", "172.16.0.1", 1, 3, false };
  const dhcp_spec discover = { bcast, other, other, "0.0.0.0", "255.255.255.255", NULL, NULL, NULL, NULL, 1, 1, true };
  const dhcp_spec ack = { mac, router, mac, "172.16.0.1", "172.16.235.71", NULL, "172.16.235.71", NULL, "172.16.0.1", 2, 5, false };
  const dhcp_spec *forms[] = { &init_reboot, &rebinding, &renewing };
  for (unsigned i = 0; i < 3; i++) {
    const size_t n = make_dhcp(f, forms[i]);
    CHECK(br_wifi_dhcp(f, n, &m) && m.type == 3 && !(m.yiaddr[0] | m.yiaddr[1] | m.yiaddr[2] | m.yiaddr[3]) && !m.has_server);
  }

  // INIT-REBOOT of another station (a broadcast every station gets): its own line, counted apart from iOS's.
  size_t n = make_dhcp(f, &init_reboot);
  CHECK(br_wifi_dhcp(f, n, &m) && m.ip_broadcast && m.has_requested && !memcmp(m.client, other, 6));
  CHECK(!strcmp(br_wifi_dhcp_request_kind(&m), "INIT-REBOOT"));
  br_wifi_dhcp_address(&m, a);
  CHECK(a[0] == 172 && a[3] == 99);
  CHECK(br_wifi_dhcp_describe(&m, false, mac, line, sizeof line) == BR_WIFI_DHCP_OTHER &&
        !strcmp(line, "another station's REQUEST INIT-REBOOT for 172.16.235.99 (client 3c:22:fb:..)"));
  // REBINDING of another station: ciaddr, to the broadcast address.
  n = make_dhcp(f, &rebinding);
  CHECK(br_wifi_dhcp(f, n, &m) && !strcmp(br_wifi_dhcp_request_kind(&m), "REBINDING"));
  CHECK(br_wifi_dhcp_describe(&m, false, mac, line, sizeof line) == BR_WIFI_DHCP_OTHER &&
        !strcmp(line, "another station's REQUEST REBINDING for 172.16.235.80 (client 3c:22:fb:..)"));
  // iOS's own RENEWING REQUEST (unicast to the server) and SELECTING one, as the guest sends them.
  n = make_dhcp(f, &renewing);
  CHECK(br_wifi_dhcp(f, n, &m) && !m.ip_broadcast && !strcmp(br_wifi_dhcp_request_kind(&m), "RENEWING"));
  CHECK(br_wifi_dhcp_describe(&m, true, mac, line, sizeof line) == BR_WIFI_DHCP_IOS && !strcmp(line, "iOS sends REQUEST RENEWING for 172.16.235.71"));
  n = make_dhcp(f, &selecting);
  CHECK(br_wifi_dhcp(f, n, &m) && !strcmp(br_wifi_dhcp_request_kind(&m), "SELECTING"));
  CHECK(br_wifi_dhcp_describe(&m, true, mac, line, sizeof line) == BR_WIFI_DHCP_IOS && !strcmp(line, "iOS sends REQUEST SELECTING for 172.16.235.71"));
  // The same REQUEST back from the radio (an access point that reflects a station's broadcast to it): iOS's own.
  CHECK(br_wifi_dhcp_describe(&m, false, mac, line, sizeof line) == BR_WIFI_DHCP_IOS &&
        !strcmp(line, "iOS's own REQUEST SELECTING for 172.16.235.71 came back from the radio"));
  // iOS's INIT-REBOOT (a known network's lease asked again after a join) sent by the guest.
  dhcp_spec own_reboot = init_reboot;
  own_reboot.eth_src = mac; own_reboot.client = mac; own_reboot.requested = "172.16.235.71";
  n = make_dhcp(f, &own_reboot);
  CHECK(br_wifi_dhcp(f, n, &m) && br_wifi_dhcp_describe(&m, true, mac, line, sizeof line) == BR_WIFI_DHCP_IOS &&
        !strcmp(line, "iOS sends REQUEST INIT-REBOOT for 172.16.235.71"));
  // Another station's DISCOVER; the server's ACK to iOS.
  n = make_dhcp(f, &discover);
  CHECK(br_wifi_dhcp(f, n, &m) && m.type == 1 && br_wifi_dhcp_describe(&m, false, mac, line, sizeof line) == BR_WIFI_DHCP_OTHER &&
        !strcmp(line, "another station's DISCOVER for 0.0.0.0 (client 3c:22:fb:..)"));
  n = make_dhcp(f, &ack);
  CHECK(br_wifi_dhcp(f, n, &m) && m.type == 5 && !strcmp(br_wifi_dhcp_request_kind(&m), ""));
  CHECK(br_wifi_dhcp_describe(&m, false, mac, line, sizeof line) == BR_WIFI_DHCP_IOS && !strcmp(line, "ACK of 172.16.235.71 from 172.16.0.1"));
  // An answer to another station (a server that broadcasts its OFFER or ACK): another station's.
  dhcp_spec other_ack = ack;
  other_ack.eth_dst = bcast; other_ack.client = other; other_ack.yiaddr = "172.16.235.99";
  n = make_dhcp(f, &other_ack);
  CHECK(br_wifi_dhcp(f, n, &m) && br_wifi_dhcp_describe(&m, false, mac, line, sizeof line) == BR_WIFI_DHCP_OTHER &&
        !strcmp(line, "another station's ACK of 172.16.235.99 (client 3c:22:fb:..)"));
  // Cut inside the options: the type still read when option 53 came whole; cut in the header: not DHCP.
  n = make_dhcp(f, &init_reboot);
  CHECK(br_wifi_dhcp(f, 14 + 20 + 8 + 240 + 3, &m) && m.type == 3 && !m.has_requested);
  CHECK(!br_wifi_dhcp(f, 14 + 20 + 8 + 239, &m));
}

// A netlink attribute (4-byte aligned) at the end of buf; returns where its payload starts, for a nested one.
static size_t nla(uint8_t *buf, size_t *len, uint16_t type, const void *data, uint16_t size)
{
  const size_t at = *len;
  buf[at] = (uint8_t)(4 + size); buf[at + 1] = (uint8_t)((4 + size) >> 8); buf[at + 2] = (uint8_t)type; buf[at + 3] = (uint8_t)(type >> 8);
  if (size) memcpy(buf + at + 4, data, size);
  *len = at + ((4u + size + 3u) & ~3u);
  return at;
}
// Closes a nested attribute opened at `at`: its length is what was added after it.
static void nla_end(uint8_t *buf, size_t len, size_t at) { buf[at] = (uint8_t)(len - at); buf[at + 1] = (uint8_t)((len - at) >> 8); }

static void test_bands(void)
{
  // NL80211_ATTR_WIPHY_BANDS as the kernel writes it: band 0 (2.4 GHz) with three frequencies, one disabled and one
  // passive; band 1 (5 GHz) with its bitrates first, then a DFS channel (passive, radar, indoor only, no 80 MHz) and
  // one with no 40 MHz above it.
  uint8_t b[512];
  size_t len = 0;
  uint32_t f;
  size_t band = nla(b, &len, 0, NULL, 0), freqs = nla(b, &len, 1, NULL, 0), e;
  e = nla(b, &len, 0, NULL, 0); f = 2412; nla(b, &len, 1, &f, 4); nla_end(b, len, e);
  e = nla(b, &len, 1, NULL, 0); f = 2467; nla(b, &len, 1, &f, 4); nla(b, &len, 2, NULL, 0); nla_end(b, len, e);
  e = nla(b, &len, 2, NULL, 0); f = 2472; nla(b, &len, 1, &f, 4); nla(b, &len, 3, NULL, 0); nla_end(b, len, e);
  nla_end(b, len, freqs); nla_end(b, len, band);
  band = nla(b, &len, 1, NULL, 0);
  const uint8_t rates[8] = { 0 };
  nla(b, &len, 2, rates, sizeof rates);
  freqs = nla(b, &len, 1, NULL, 0);
  e = nla(b, &len, 0, NULL, 0); f = 5260; nla(b, &len, 1, &f, 4); nla(b, &len, 3, NULL, 0); nla(b, &len, 5, NULL, 0); nla(b, &len, 14, NULL, 0);
  nla(b, &len, 11, NULL, 0); nla_end(b, len, e);
  e = nla(b, &len, 1, NULL, 0); f = 5745; nla(b, &len, 1, &f, 4); nla(b, &len, 10, NULL, 0); nla_end(b, len, e);
  nla_end(b, len, freqs); nla_end(b, len, band);
  struct ow_channel ch[8];
  memset(ch, 0, sizeof ch);
  unsigned n = br_wifi_nl_bands(b, len, ch, 0, 8);
  CHECK(n == 4);
  CHECK(ch[0].freq_mhz == 2412 && ch[0].flags == 0 && ch[1].freq_mhz == 2472 && ch[1].flags == OW_CH_PASSIVE);
  CHECK(ch[2].freq_mhz == 5260 && ch[2].flags == (OW_CH_PASSIVE | OW_CH_RADAR | OW_CH_INDOOR | OW_CH_NO_80MHZ));
  CHECK(ch[3].freq_mhz == 5745 && ch[3].flags == OW_CH_NO_HT40);
  // A second message of a split dump appends; a full list stops; a cut band is left out, the bands before it stay.
  CHECK(br_wifi_nl_bands(b, len, ch, 4, 6) == 6 && ch[5].freq_mhz == 2472);
  CHECK(br_wifi_nl_bands(b, len, ch, 0, 2) == 2);
  CHECK(br_wifi_nl_bands(b, len - 8, ch, 0, 8) == 2 && br_wifi_nl_bands(b, 3, ch, 0, 8) == 0);   // band 1 cut: band 0 stays
}

static void test_groups_segments(void)   // the netlink groups, the cut of coalesced frames
{
  // A netlink multicast group as the kernel writes it (the id, then the name) and the other way round; another
  // name; a name that only begins with the wanted one; cut short.
  static const uint8_t scan_group[] = { 8, 0, 2, 0, 3, 0, 0, 0, 9, 0, 1, 0, 's', 'c', 'a', 'n', 0, 0, 0, 0 };
  static const uint8_t name_first[] = { 9, 0, 1, 0, 's', 'c', 'a', 'n', 0, 0, 0, 0, 8, 0, 2, 0, 7, 0, 0, 0 };
  static const uint8_t regulatory[] = { 8, 0, 2, 0, 4, 0, 0, 0, 15, 0, 1, 0, 'r', 'e', 'g', 'u', 'l', 'a', 't', 'o', 'r', 'y', 0, 0 };
  static const uint8_t scanner[] = { 8, 0, 2, 0, 5, 0, 0, 0, 12, 0, 1, 0, 's', 'c', 'a', 'n', 'n', 'e', 'r', 0 };
  uint32_t id = 0;
  CHECK(br_wifi_nl_group(scan_group, sizeof scan_group, "scan", &id) && id == 3);
  CHECK(br_wifi_nl_group(name_first, sizeof name_first, "scan", &id) && id == 7);
  CHECK(!br_wifi_nl_group(regulatory, sizeof regulatory, "scan", &id) && !br_wifi_nl_group(scanner, sizeof scanner, "scan", &id));
  CHECK(br_wifi_nl_group(regulatory, sizeof regulatory, "regulatory", &id) && id == 4);
  for (size_t cut = 0; cut < sizeof scan_group - 3; cut++) CHECK(!br_wifi_nl_group(scan_group, cut, "scan", &id));

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
static const br_wifi_radio fake = { fr_scan, fr_join, fr_leave, fr_link, NULL };

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
  br_wifi_station *sta = br_wifi_station_create(&fake, &r, mac, NULL, take, &s);
  CHECK(sta != NULL);
  struct ow_hello hello = { OW_VERSION };
  br_wifi_station_message(sta, f, guest_frame(f, OW_HELLO, &hello, sizeof hello));
  CHECK(s.count == 1 && host_message(&s, 0, &p, &n) == OW_STATE && n == sizeof st);
  memcpy(&st, p, sizeof st);
  CHECK(st.version == OW_VERSION && st.radio == OW_RADIO_OK && st.link == OW_LINK_IDLE && !memcmp(st.mac, mac, 6) && st.country[0] == '0' && st.country[1] == '0');

  // A scan: two networks, the second seen twice (the stronger one is the join's), elements cut to whole ones.
  memset(&s, 0, sizeof s);
  br_wifi_station_message(sta, f, guest_frame(f, OW_SCAN, NULL, 0));
  br_wifi_station_message(sta, f, guest_frame(f, OW_SCAN, NULL, 0));       // a second request while one runs: one scan
  CHECK(r.scans == 1 && s.count == 0);
  static uint8_t ies[1100];
  ies[0] = 0; ies[1] = 4; memcpy(ies + 2, "home", 4);                      // SSID
  ies[6] = 7; ies[7] = 20; ies[8] = 'I'; ies[9] = 'N';                     // Country (20 bytes; the RSN element of a real beacon comes after it)
  for (unsigned at = 28; at + 102 <= sizeof ies; at += 102) { ies[at] = 221; ies[at + 1] = 100; }   // vendor elements up to 1048
  struct ow_bss b = { .bssid = { 1, 2, 3, 4, 5, 6 }, .freq_mhz = 5180, .signal_dbm = -70, .ssid_length = 4, .ssid = "home" };
  br_wifi_scan_add(sta, &b, ies, 1048);
  b.bssid[5] = 7; b.signal_dbm = -50; b.freq_mhz = 2437;
  br_wifi_scan_add(sta, &b, ies, 28);
  struct ow_bss other = { .bssid = { 9, 9, 9, 9, 9, 9 }, .freq_mhz = 2412, .signal_dbm = -80, .ssid_length = 5, .ssid = "other" };
  br_wifi_scan_add(sta, &other, NULL, 0);
  br_wifi_scan_done(sta, 0);
  // Five frames: three networks, the state with the country the strongest network's Country element names, the end.
  CHECK(s.count == 5 && host_message(&s, 3, &p, &n) == OW_STATE);
  memcpy(&st, p, sizeof st);
  CHECK(st.country[0] == 'I' && st.country[1] == 'N');
  CHECK(host_message(&s, 0, &p, &n) == OW_BSS && n == sizeof b + 946);     // 28 + 9 * 102: the tenth does not fit 1024
  struct ow_bss got;
  memcpy(&got, p, sizeof got);
  CHECK(got.ie_length == 946 && got.freq_mhz == 5180 && !memcmp(p + sizeof got, ies, 946));
  CHECK(host_message(&s, 2, &p, &n) == OW_BSS && n == sizeof b);
  struct ow_scan_done done;
  CHECK(host_message(&s, 4, &p, &n) == OW_SCAN_DONE && n == sizeof done);
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
  // iOS's Wi-Fi switch: on changes nothing, off leaves the network.
  struct ow_power power = { 1 };
  br_wifi_station_message(sta, f, guest_frame(f, OW_POWER, &power, sizeof power));
  CHECK(r.leaves == 1 && s.count == 1);
  power.on = 0;
  br_wifi_station_message(sta, f, guest_frame(f, OW_POWER, &power, sizeof power));
  CHECK(r.leaves == 2 && s.count == 2 && host_message(&s, 1, &p, &n) == OW_STATE);
  // Malformed: a short frame, a wrong magic, a length past the frame, a join with no name.
  memset(&s, 0, sizeof s);
  br_wifi_station_message(sta, f, 10);
  n = guest_frame(f, OW_SCAN, NULL, 0); f[14] ^= 1;
  br_wifi_station_message(sta, f, n);
  n = guest_frame(f, OW_JOIN, &j, sizeof j);
  br_wifi_station_message(sta, f, n - 1);
  struct ow_join unnamed = { 0 };
  br_wifi_station_message(sta, f, guest_frame(f, OW_JOIN, &unnamed, sizeof unnamed));
  CHECK(s.count == 0 && r.joins == 3 && r.scans == 1 && r.leaves == 2);
  br_wifi_station_destroy(sta);
}

// ---- the made-up radio (wifi=sim) through its back-end calls ----
static void test_sim(void)
{
  static sink s;
  uint8_t f[256];
  const uint8_t *p = f;
  size_t n = 0;
  br_net_open_fn open_fn = br_wifi_sim_prepare(mac);
  br_net *net = open_fn ? open_fn(NULL, take, &s) : NULL;
  CHECK(net != NULL);
  if (!net) return;
  struct ow_hello hello = { OW_VERSION };
  CHECK(br_wifi_sim_send(net, f, guest_frame(f, OW_HELLO, &hello, sizeof hello)) == 0 && s.count == 2 && host_message(&s, 0, &p, &n) == OW_STATE);
  // Then the radio's channels: 13 of 2.4 GHz and 25 of 5 GHz, the DFS ones passive and radar.
  CHECK(host_message(&s, 1, &p, &n) == OW_CHANNELS && n == sizeof(struct ow_channels) + 38 * sizeof(struct ow_channel));
  if (n >= sizeof(struct ow_channels) + 38 * sizeof(struct ow_channel)) {
    struct ow_channels head; struct ow_channel first, dfs, last;
    memcpy(&head, p, sizeof head);
    memcpy(&first, p + sizeof head, sizeof first);
    memcpy(&dfs, p + sizeof head + 17 * sizeof dfs, sizeof dfs);              // the 18th: 5 GHz channel 52
    memcpy(&last, p + sizeof head + 37 * sizeof last, sizeof last);
    CHECK(head.count == 38 && first.freq_mhz == 2412 && first.flags == 0 && dfs.freq_mhz == 5260 && dfs.flags == (OW_CH_PASSIVE | OW_CH_RADAR) &&
          last.freq_mhz == 5825 && last.flags == 0);
  }
  memset(&s, 0, sizeof s);
  br_wifi_sim_send(net, f, guest_frame(f, OW_SCAN, NULL, 0));
  struct ow_bss b;
  CHECK(s.count == 3 && host_message(&s, 0, &p, &n) == OW_BSS && host_message(&s, 2, &p, &n) == OW_SCAN_DONE);
  CHECK(host_message(&s, 1, &p, &n) == OW_BSS);
  memcpy(&b, p, sizeof b);
  br_wifi_security sec;
  br_wifi_security_parse(p + sizeof b, b.ie_length, &sec);
  CHECK(b.ssid_length == 11 && !memcmp(b.ssid, "OneiOS WPA2", 11) && sec.rsn && sec.psk && sec.ccmp && (b.capability & 0x10));
  // The secured network: a wrong passphrase is a refused key, the right one joins; a data frame goes nowhere.
  struct ow_join j = { .ssid_length = 11, .ssid = "OneiOS WPA2", .security = OW_SECURITY_AUTO, .key_length = 8, .key = "12345678" };
  struct ow_join_result result;
  struct ow_state st;
  memset(&s, 0, sizeof s);
  br_wifi_sim_send(net, f, guest_frame(f, OW_JOIN, &j, sizeof j));
  CHECK(s.count == 1 && host_message(&s, 0, &p, &n) == OW_JOIN_RESULT);
  memcpy(&result, p, sizeof result);
  CHECK(result.status == OW_JOIN_AUTH && result.code == 15);
  memcpy(j.key, "password", 8);
  memset(&s, 0, sizeof s);
  br_wifi_sim_send(net, f, guest_frame(f, OW_JOIN, &j, sizeof j));
  CHECK(s.count == 2 && host_message(&s, 0, &p, &n) == OW_STATE);
  memcpy(&st, p, sizeof st);
  CHECK(st.link == OW_LINK_UP && st.bss.freq_mhz == 5180 && st.rssi_dbm == -55);
  CHECK(host_message(&s, 1, &p, &n) == OW_JOIN_RESULT);
  memcpy(&result, p, sizeof result);
  CHECK(result.status == OW_JOIN_OK);
  memset(&s, 0, sizeof s);
  memset(f, 0xff, 60);
  CHECK(br_wifi_sim_send(net, f, 60) == 0 && s.count == 0);
  struct ow_join open = { .ssid_length = 11, .ssid = "OneiOS open", .security = OW_SECURITY_OPEN };
  br_wifi_sim_send(net, f, guest_frame(f, OW_JOIN, &open, sizeof open));
  CHECK(s.count == 2 && host_message(&s, 1, &p, &n) == OW_JOIN_RESULT);
  memcpy(&result, p, sizeof result);
  CHECK(result.status == OW_JOIN_OK);
  memset(&s, 0, sizeof s);
  br_wifi_sim_send(net, f, guest_frame(f, OW_LEAVE, NULL, 0));
  CHECK(s.count == 1 && host_message(&s, 0, &p, &n) == OW_STATE);
  memcpy(&st, p, sizeof st);
  CHECK(st.link == OW_LINK_IDLE);
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
  test_dhcp();
  test_bands();
  test_groups_segments();
  test_station();
  test_sim();
  test_parcels();
  test_security();
  test_callbacks();
  printf("wifi: %d checks, %d failed\n", checks, failures);
  return failures ? 1 : 0;
}
