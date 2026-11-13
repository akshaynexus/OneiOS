// SPDX-License-Identifier: MIT
// A radio that is not there, for a boot with no phone (xnuhost wifi=sim, a PC or the Mac's container): the guest's
// Wi-Fi driver and wifid get a station that finds two made-up networks and joins them, so the guest's side (the
// Apple80211 requests, the scan records, a join's result and events) can be run and read in the log without wlan0, a
// supplicant or a network. No frame goes anywhere: a joined network has no DHCP server behind it.
//   "OneiOS open"   no security: joins
//   "OneiOS WPA2"   WPA2-PSK, CCMP: joins with the passphrase "password" (or any 32-byte key); another one is a
//                   refused key (802.11 status 15)
#include "wifi.h"
#include "bridges/bridges.h"
#include <stdlib.h>
#include <string.h>

struct br_wifi_sim {
  br_wifi_station *station;
  br_net_rx_fn rx;
  void *rx_ctx;
  uint8_t mac[6];
  bool joined;
  br_wifi_bss current;
};
static struct br_wifi_sim *the_sim;

static const uint8_t open_ies[] = { 0, 11, 'O', 'n', 'e', 'i', 'O', 'S', ' ', 'o', 'p', 'e', 'n', 1, 8, 0x82, 0x84, 0x8b, 0x96, 0x24, 0x30, 0x48, 0x6c };
static const uint8_t wpa2_ies[] = { 0, 11, 'O', 'n', 'e', 'i', 'O', 'S', ' ', 'W', 'P', 'A', '2', 1, 8, 0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c,
                                    48, 20, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 2, 0x0c, 0 };
static const struct ow_bss networks[2] = {
  { .bssid = { 0x02, 0x4f, 0x53, 0x00, 0x00, 0x01 }, .capability = 0x0401, .freq_mhz = 2437, .signal_dbm = -48, .beacon_interval = 100, .ssid_length = 11, .ssid = "OneiOS open" },
  { .bssid = { 0x02, 0x4f, 0x53, 0x00, 0x00, 0x02 }, .capability = 0x0411, .freq_mhz = 5180, .signal_dbm = -55, .beacon_interval = 100, .ssid_length = 11, .ssid = "OneiOS WPA2" },
};

static int sim_scan(void *ctx)
{
  struct br_wifi_sim *s = ctx;
  br_wifi_scan_add(s->station, &networks[0], open_ies, sizeof open_ies);
  br_wifi_scan_add(s->station, &networks[1], wpa2_ies, sizeof wpa2_ies);
  br_wifi_scan_done(s->station, 0);
  return 0;
}

static int sim_join(void *ctx, const struct ow_join *j, const br_wifi_bss *bss)
{
  struct br_wifi_sim *s = ctx;
  s->joined = false;
  if (!bss) { br_wifi_join_done(s->station, OW_JOIN_NO_NETWORK, 0); return 0; }
  const bool secured = bss->bss.capability & 0x10;
  const bool key = (j->key_raw && j->key_length == 32) || (j->key_length == 8 && !memcmp(j->key, "password", 8));
  br_log(1, "wifi: sim: join of a network name of %u bytes, %s, with %s", j->ssid_length, secured ? "WPA2" : "open",
         j->key_raw ? "a 32-byte key" : j->key_length ? "a passphrase" : "no key");
  if (secured && !key) { br_wifi_join_done(s->station, OW_JOIN_AUTH, 15); return 0; }
  s->joined = true;
  s->current = *bss;
  br_wifi_join_done(s->station, OW_JOIN_OK, 0);
  return 0;
}

static int sim_leave(void *ctx) { ((struct br_wifi_sim *)ctx)->joined = false; return 0; }

static int sim_link(void *ctx, br_wifi_link *out)
{
  struct br_wifi_sim *s = ctx;
  memset(out, 0, sizeof *out);
  out->up = s->joined;
  out->reason = 3;
  if (!s->joined) return 0;
  out->rssi_dbm = s->current.bss.signal_dbm;
  out->noise_dbm = -95;
  out->tx_rate_mbps = 433;
  out->freq_mhz = s->current.bss.freq_mhz;
  memcpy(out->bssid, s->current.bss.bssid, 6);
  return 0;
}

// The made-up radio's channels: 2.4 GHz 1 to 13, 5 GHz 36 to 165 with the DFS ones (52 to 144) passive and radar.
static int sim_channels(void *ctx, struct ow_channel *out, unsigned max)
{
  (void)ctx;
  unsigned n = 0;
  for (unsigned c = 1; c <= 13 && n < max; c++) out[n++] = (struct ow_channel){ .freq_mhz = (uint16_t)(2407 + 5 * c) };
  static const uint8_t five[] = { 36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144,
                                  149, 153, 157, 161, 165 };
  for (unsigned i = 0; i < sizeof five && n < max; i++)
    out[n++] = (struct ow_channel){ .freq_mhz = (uint16_t)(5000 + 5 * five[i]),
                                    .flags = (uint8_t)(five[i] >= 52 && five[i] <= 144 ? OW_CH_PASSIVE | OW_CH_RADAR : 0) };
  return (int)n;
}

static const br_wifi_radio sim_radio = { sim_scan, sim_join, sim_leave, sim_link, sim_channels };

static void sim_deliver(void *ctx, const uint8_t *frame, size_t length)
{
  struct br_wifi_sim *s = ctx;
  if (s->rx) s->rx(s->rx_ctx, frame, length);
}

static br_net *sim_open(const br_net_config *c, br_net_rx_fn rx, void *ctx)
{
  (void)c;
  if (!the_sim) return NULL;
  the_sim->rx = rx;
  the_sim->rx_ctx = ctx;
  return (br_net *)the_sim;
}

br_net_open_fn br_wifi_sim_prepare(const uint8_t mac[6])
{
  struct br_wifi_sim *s = calloc(1, sizeof *s);
  if (!s) return NULL;
  memcpy(s->mac, mac, 6);
  s->station = br_wifi_station_create(&sim_radio, s, mac, "XX", sim_deliver, s);
  if (!s->station) { free(s); return NULL; }
  the_sim = s;
  br_log(1, "wifi: sim: a made-up radio with two networks (wifi=sim): no frame goes anywhere");
  return sim_open;
}

int br_wifi_sim_send(br_net *n, const uint8_t *frame, size_t length)
{
  struct br_wifi_sim *s = (struct br_wifi_sim *)n;
  if (br_wifi_ethertype(frame, length) == BR_WIFI_CONTROL_TYPE) br_wifi_station_message(s->station, frame, length);
  return 0;
}
