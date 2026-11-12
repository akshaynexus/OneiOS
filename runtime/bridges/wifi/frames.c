// SPDX-License-Identifier: MIT
// The Wi-Fi bridge's frame rules (docs/hardware.md section 3): which frames cross between the guest's en0 and the
// phone's wlan0, and how a frame that the receive path coalesced (GRO) is cut back into frames of the link's size.
// Pure functions: unit-tested on any host (tests/test_bridges.c).
#include "wifi.h"
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
