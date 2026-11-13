// SPDX-License-Identifier: MIT
// The Wi-Fi bridge's frame rules (docs/hardware.md section 3): which frames cross between the guest's en0 and the
// phone's wlan0, and how a frame that the receive path coalesced (GRO) is cut back into frames of the link's size.
// Pure functions: unit-tested on any host (tests/test_bridges.c).
#include "wifi.h"
#include <stdio.h>
#include <string.h>

static uint16_t be16(const uint8_t *p) { return (uint16_t)(p[0] << 8 | p[1]); }
static void put16(uint8_t *p, uint16_t v) { p[0] = (uint8_t)(v >> 8); p[1] = (uint8_t)v; }
static void put32(uint8_t *p, uint32_t v) { p[0] = (uint8_t)(v >> 24); p[1] = (uint8_t)(v >> 16); p[2] = (uint8_t)(v >> 8); p[3] = (uint8_t)v; }
static uint32_t be32(const uint8_t *p) { return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3]; }

uint16_t br_wifi_ethertype(const uint8_t *frame, size_t length)
{
  return length >= 14 ? be16(frame + 12) : 0;
}

bool br_wifi_to_radio(const uint8_t *frame, size_t length, const uint8_t mac[6])
{
  if (length < 14 || length > BR_WIFI_FRAME) return false;
  const uint16_t type = br_wifi_ethertype(frame, length);
  if (type == 0x888e || type == BR_WIFI_CONTROL_TYPE) return false;   // EAPOL is the supplicant's; ours is the bridge's
  return memcmp(frame + 6, mac, 6) == 0;                              // a station sends with its own address only
}

bool br_wifi_to_guest(const uint8_t *frame, size_t length, const uint8_t mac[6])
{
  if (length < 14) return false;
  const uint16_t type = br_wifi_ethertype(frame, length);
  if (type == 0x888e || type == BR_WIFI_CONTROL_TYPE) return false;
  const bool group = frame[0] & 1;
  return group || memcmp(frame, mac, 6) == 0;
}

static uint16_t fold(uint32_t sum)
{
  while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
  return (uint16_t)~sum;
}

static uint32_t add(uint32_t sum, const uint8_t *data, size_t length)
{
  for (size_t i = 0; i + 1 < length; i += 2) sum += (uint32_t)(data[i] << 8 | data[i + 1]);
  if (length & 1) sum += (uint32_t)data[length - 1] << 8;
  return sum;
}

// The checksums of one IPv4 or IPv6 TCP segment: IPv4 header (when IPv4) and TCP over the pseudo-header.
static void tcp_checksums(uint8_t *l3, bool v6, size_t l3_len, size_t tcp_len)
{
  uint8_t *tcp = l3 + l3_len;
  uint32_t sum = 0;
  if (v6) {
    sum = add(sum, l3 + 8, 32);                                 // source and destination
  } else {
    put16(l3 + 10, 0);
    put16(l3 + 10, fold(add(0, l3, l3_len)));
    sum = add(sum, l3 + 12, 8);
  }
  sum += 6 + (uint32_t)tcp_len;                                   // protocol, TCP length
  put16(tcp + 16, 0);
  put16(tcp + 16, fold(add(sum, tcp, tcp_len)));
}

size_t br_wifi_segment(const uint8_t *frame, size_t length, uint16_t mss, bool coalesced, br_wifi_segment_fn fn, void *ctx)
{
  const bool fits = length <= BR_WIFI_FRAME;
  if (fits && !coalesced) { fn(ctx, frame, length); return 1; }
#define AS_IT_IS do { if (!fits) return 0; fn(ctx, frame, length); return 1; } while (0)
  if (length < 14 + 20 || !mss) AS_IT_IS;
  const uint16_t type = br_wifi_ethertype(frame, length);
  const uint8_t *l3 = frame + 14;
  size_t l3_len;
  bool v6;
  if (type == 0x0800 && (l3[0] >> 4) == 4 && l3[9] == 6) { v6 = false; l3_len = (size_t)(l3[0] & 15) * 4; }
  else if (type == 0x86dd && length >= 14 + 40 && (l3[0] >> 4) == 6 && l3[6] == 6) { v6 = true; l3_len = 40; }
  else AS_IT_IS;                                                  // only TCP is coalesced
  if (l3_len < 20 || length < 14 + l3_len + 20) AS_IT_IS;
  const uint8_t *tcp = l3 + l3_len;
  const size_t tcp_hdr = (size_t)(tcp[12] >> 4) * 4;
  const size_t headers = 14 + l3_len + tcp_hdr;
  if (tcp_hdr < 20 || headers >= length || headers >= BR_WIFI_FRAME) AS_IT_IS;
#undef AS_IT_IS
  size_t room = BR_WIFI_FRAME - headers;
  if (mss > room) mss = (uint16_t)room;
  const uint8_t *payload = frame + headers;
  const size_t total = length - headers;
  const uint32_t seq = be32(tcp + 4);
  const uint16_t id = v6 ? 0 : be16(l3 + 4);
  const uint8_t flags = tcp[13];
  uint8_t out[BR_WIFI_FRAME];
  size_t count = 0;
  for (size_t done = 0; done < total; done += mss, count++) {
    const size_t part = total - done < mss ? total - done : mss;
    const bool last = done + part >= total;
    memcpy(out, frame, headers);
    memcpy(out + headers, payload + done, part);
    uint8_t *o3 = out + 14, *ot = o3 + l3_len;
    if (v6) put16(o3 + 4, (uint16_t)(tcp_hdr + part));
    else { put16(o3 + 2, (uint16_t)(l3_len + tcp_hdr + part)); put16(o3 + 4, (uint16_t)(id + count)); }
    put32(ot + 4, seq + (uint32_t)done);
    ot[13] = last ? flags : (uint8_t)(flags & ~0x09);              // FIN and PSH on the last segment only
    tcp_checksums(o3, v6, l3_len, tcp_hdr + part);
    fn(ctx, out, headers + part);
  }
  return count;
}

// A generic netlink multicast group's attributes: CTRL_ATTR_MCAST_GRP_NAME (1, a string) and CTRL_ATTR_MCAST_GRP_ID
// (2, 32 bits), each {length, type} then the value, padded to 4 bytes. Both are read before they are compared: the
// kernel writes the id first, and a reader that kept "the last name seen" took the id of the group after the one it
// wanted (the bridge then listened to nl80211's "regulatory" group for scan events and never got one).
bool br_wifi_nl_group(const uint8_t *attrs, size_t length, const char *name, uint32_t *id)
{
  const uint8_t *found_name = NULL;
  size_t name_size = 0;
  bool have_id = false;
  uint32_t value = 0;
  for (size_t at = 0; at + 4 <= length;) {
    const uint16_t len = (uint16_t)(attrs[at] | attrs[at + 1] << 8), type = (uint16_t)((attrs[at + 2] | attrs[at + 3] << 8) & 0x3fff);
    if (len < 4 || len > length - at) break;
    if (type == 1) { found_name = attrs + at + 4; name_size = len - 4u; }
    else if (type == 2 && len == 8) { memcpy(&value, attrs + at + 4, 4); have_id = true; }
    at += (len + 3u) & ~3u;
  }
  const size_t want = strlen(name);
  if (!have_id || !found_name || name_size < want || memcmp(found_name, name, want) || (name_size > want && found_name[want] != 0)) return false;
  *id = value;
  return true;
}

// A DHCP message in an Ethernet frame (IPv4, UDP between ports 67 and 68, the BOOTP header and the magic cookie):
// its type (option 53), the client's hardware address (chaddr), ciaddr and yiaddr, the requested address (option 50)
// and the server's (option 54). For the log only: what the bridge carries is not changed.
bool br_wifi_dhcp(const uint8_t *frame, size_t length, br_wifi_dhcp_msg *m)
{
  if (length < 14 + 20 + 8 + 240 || br_wifi_ethertype(frame, length) != 0x0800) return false;
  const uint8_t *ip = frame + 14;
  const size_t ihl = (size_t)(ip[0] & 15) * 4;
  if ((ip[0] >> 4) != 4 || ihl < 20 || ip[9] != 17 || length < 14 + ihl + 8 + 240) return false;
  const uint8_t *udp = ip + ihl, *b = udp + 8;
  const uint16_t sport = be16(udp), dport = be16(udp + 2);
  if (!((sport == 68 && dport == 67) || (sport == 67 && dport == 68)) || be32(b + 236) != 0x63825363u) return false;
  memset(m, 0, sizeof *m);
  memcpy(m->ciaddr, b + 12, 4);
  memcpy(m->yiaddr, b + 16, 4);
  if (b[1] == 1 && b[2] == 6) memcpy(m->client, b + 28, 6);   // Ethernet's hardware type and length
  m->ip_broadcast = be32(ip + 16) == 0xffffffffu;
  const uint8_t *end = frame + length;
  for (const uint8_t *o = b + 240; o < end && *o != 255;) {
    if (*o == 0) { o++; continue; }
    if (o + 2 > end || o + 2 + o[1] > end) break;
    if (*o == 53 && o[1] == 1) m->type = o[2];
    if (*o == 50 && o[1] == 4) { memcpy(m->requested, o + 2, 4); m->has_requested = true; }
    if (*o == 54 && o[1] == 4) { memcpy(m->server, o + 2, 4); m->has_server = true; }
    o += 2u + o[1];
  }
  return m->type != 0;
}

static bool zero4(const uint8_t a[4]) { return !(a[0] | a[1] | a[2] | a[3]); }

const char *br_wifi_dhcp_request_kind(const br_wifi_dhcp_msg *m)
{
  if (m->type != 3) return "";
  if (m->has_server) return "SELECTING";
  if (zero4(m->ciaddr)) return "INIT-REBOOT";
  return m->ip_broadcast ? "REBINDING" : "RENEWING";
}

void br_wifi_dhcp_address(const br_wifi_dhcp_msg *m, uint8_t out[4])
{
  if (m->type == 3 || m->type == 4) memcpy(out, m->has_requested ? m->requested : m->ciaddr, 4);   // REQUEST, DECLINE
  else if (m->type == 7 || m->type == 8) memcpy(out, m->ciaddr, 4);                                 // RELEASE, INFORM
  else memcpy(out, m->yiaddr, 4);
}

bool br_wifi_dhcp_from_client(const br_wifi_dhcp_msg *m)
{
  return m->type == 1 || m->type == 3 || m->type == 4 || m->type == 7 || m->type == 8;
}

int br_wifi_dhcp_describe(const br_wifi_dhcp_msg *m, bool from_guest, const uint8_t mac[6], char *out, size_t size)
{
  static const char *const names[] = { "?", "DISCOVER", "OFFER", "REQUEST", "DECLINE", "ACK", "NAK", "RELEASE", "INFORM" };
  const char *name = m->type < 9 ? names[m->type] : "?", *kind = br_wifi_dhcp_request_kind(m), *sp = *kind ? " " : "";
  uint8_t a[4];
  br_wifi_dhcp_address(m, a);
  const bool client = br_wifi_dhcp_from_client(m);
  if (!from_guest && memcmp(m->client, mac, 6)) {
    snprintf(out, size, "another station's %s%s%s %s %u.%u.%u.%u (client %02x:%02x:%02x:..)", name, sp, kind, client ? "for" : "of",
             a[0], a[1], a[2], a[3], m->client[0], m->client[1], m->client[2]);
    return BR_WIFI_DHCP_OTHER;
  }
  if (from_guest)
    snprintf(out, size, "iOS sends %s%s%s %s %u.%u.%u.%u", name, sp, kind, client ? "for" : "of", a[0], a[1], a[2], a[3]);
  else if (client)   // iOS's own message, back from the radio (an access point's reflection)
    snprintf(out, size, "iOS's own %s%s%s for %u.%u.%u.%u came back from the radio", name, sp, kind, a[0], a[1], a[2], a[3]);
  else
    snprintf(out, size, "%s of %u.%u.%u.%u from %u.%u.%u.%u", name, a[0], a[1], a[2], a[3], m->server[0], m->server[1], m->server[2], m->server[3]);
  return BR_WIFI_DHCP_IOS;
}
