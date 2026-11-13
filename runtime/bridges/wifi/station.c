// SPDX-License-Identifier: MIT
// The station the guest's Wi-Fi driver (kernel/patches/oneios-hosted/net/OneiOSWifi.cpp) talks to: its messages
// (drivers/net/oneios_wifi_proto.h) become calls on a radio back end, and what the radio reports becomes messages
// for the guest. No Linux interface here: the phone's back end is wlan.c, the tests use a fake radio.
#define _GNU_SOURCE
#include "wifi.h"
#include "bridges/bridges.h"
#include <errno.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>

#define SCAN_KEEP 64u
#define SENT_IES 1024u
_Static_assert(SENT_IES <= OW_MAX_IES, "the elements of a network fit a message");

struct br_wifi_station {
  const br_wifi_radio *radio;
  void *radio_ctx;
  uint8_t mac[6];
  char country[2];
  br_wifi_guest_fn to_guest;
  void *guest_ctx;
  pthread_mutex_t lock;          // everything below
  br_wifi_bss *scan;             // the last scan's networks (also the join's source of what a network offers)
  unsigned nscan;
  bool scanning, joining, joined;
  struct ow_join join;           // the network asked for (its key is cleared once the radio has it)
  br_wifi_bss current;
  uint32_t last_reason;
};

static void send(br_wifi_station *s, uint16_t type, const void *a, size_t alen, const void *b, size_t blen)
{
  uint8_t frame[BR_WIFI_FRAME];
  struct ow_header h = { OW_MAGIC, type, (uint16_t)(alen + blen) };
  if (14 + sizeof h + alen + blen > sizeof frame) return;
  memcpy(frame, s->mac, 6);                        // to the guest: its own address passes its receive filter
  memcpy(frame + 6, ow_address, 6);
  frame[12] = (uint8_t)(OW_ETHERTYPE >> 8); frame[13] = (uint8_t)OW_ETHERTYPE;
  memcpy(frame + 14, &h, sizeof h);
  if (alen) memcpy(frame + 14 + sizeof h, a, alen);
  if (blen) memcpy(frame + 14 + sizeof h + alen, b, blen);
  s->to_guest(s->guest_ctx, frame, 14 + sizeof h + alen + blen);
}

// The state message: the radio's view of the link, the joined network's record and elements.
static size_t state(br_wifi_station *s, struct ow_state *st, const uint8_t **ies)
{
  br_wifi_link link = { 0 };
  memset(st, 0, sizeof *st);
  st->version = OW_VERSION;
  st->radio = OW_RADIO_OK;
  memcpy(st->mac, s->mac, 6);
  memcpy(st->country, s->country, 2);
  int rc = s->radio->link ? s->radio->link(s->radio_ctx, &link) : -ENOSYS;
  pthread_mutex_lock(&s->lock);
  if (rc == 0 && link.up && s->joined) {
    st->link = OW_LINK_UP;
    st->rssi_dbm = link.rssi_dbm;
    st->noise_dbm = link.noise_dbm;
    st->tx_rate_mbps = link.tx_rate_mbps;
    if (link.freq_mhz) s->current.bss.freq_mhz = link.freq_mhz;
    s->current.bss.signal_dbm = (int16_t)link.rssi_dbm;
    if (link.bssid[0] | link.bssid[1] | link.bssid[2] | link.bssid[3] | link.bssid[4] | link.bssid[5]) memcpy(s->current.bss.bssid, link.bssid, 6);
    st->bss = s->current.bss;
    *ies = s->current.ies;
  } else {
    if (s->joined && rc == 0 && !link.up) { s->joined = false; s->last_reason = link.reason; }
    st->link = s->joining ? OW_LINK_AUTH : s->scanning ? OW_LINK_SCANNING : OW_LINK_IDLE;
    st->reason = s->last_reason;
    *ies = NULL;
  }
  pthread_mutex_unlock(&s->lock);
  return *ies ? st->bss.ie_length : 0;
}

size_t br_wifi_state_frame(br_wifi_station *s, uint8_t *out, size_t out_size)
{
  struct ow_state st;
  const uint8_t *ies;
  size_t n = state(s, &st, &ies);
  size_t total = 14 + sizeof(struct ow_header) + sizeof st + n;
  if (total > out_size) return 0;
  struct ow_header h = { OW_MAGIC, OW_STATE, (uint16_t)(sizeof st + n) };
  memcpy(out, s->mac, 6); memcpy(out + 6, ow_address, 6);
  out[12] = (uint8_t)(OW_ETHERTYPE >> 8); out[13] = (uint8_t)OW_ETHERTYPE;
  memcpy(out + 14, &h, sizeof h);
  memcpy(out + 14 + sizeof h, &st, sizeof st);
  if (n) memcpy(out + 14 + sizeof h + sizeof st, ies, n);
  return total;
}

void br_wifi_link_changed(br_wifi_station *s)
{
  uint8_t frame[BR_WIFI_FRAME];
  size_t n = br_wifi_state_frame(s, frame, sizeof frame);
  if (n) s->to_guest(s->guest_ctx, frame, n);
}

br_wifi_station *br_wifi_station_create(const br_wifi_radio *radio, void *radio_ctx, const uint8_t mac[6], const char country[2],
                                        br_wifi_guest_fn to_guest, void *guest_ctx)
{
  br_wifi_station *s = calloc(1, sizeof *s);
  if (!s || !(s->scan = calloc(SCAN_KEEP, sizeof *s->scan))) { free(s); return NULL; }
  s->radio = radio;
  s->radio_ctx = radio_ctx;
  memcpy(s->mac, mac, 6);
  s->country[0] = country && country[0] ? country[0] : '0';
  s->country[1] = country && country[0] ? country[1] : '0';
  s->to_guest = to_guest;
  s->guest_ctx = guest_ctx;
  pthread_mutex_init(&s->lock, NULL);
  return s;
}

void br_wifi_station_destroy(br_wifi_station *s)
{
  if (!s) return;
  memset(&s->join, 0, sizeof s->join);
  pthread_mutex_destroy(&s->lock);
  free(s->scan);
  free(s);
}

void br_wifi_scan_add(br_wifi_station *s, const struct ow_bss *bss, const uint8_t *ies, size_t ie_length)
{
  // Whole elements only, as many as iOS's scan record holds (SENT_IES bytes).
  size_t whole = 0;
  while (ies && whole + 2 <= ie_length && whole + 2u + ies[whole + 1] <= ie_length && whole + 2u + ies[whole + 1] <= SENT_IES) whole += 2u + ies[whole + 1];
  ie_length = whole;
  struct ow_bss record = *bss;
  record.ie_length = (uint16_t)ie_length;
  pthread_mutex_lock(&s->lock);
  bool keep = s->nscan < SCAN_KEEP;
  if (keep) {
    br_wifi_bss *b = &s->scan[s->nscan++];
    b->bss = record;
    if (ie_length) memcpy(b->ies, ies, ie_length);
  }
  pthread_mutex_unlock(&s->lock);
  if (keep) send(s, OW_BSS, &record, sizeof record, ies, ie_length);
}

// The regulatory country: the Country element (7) of the strongest network that has one, as iOS's own drivers take it.
static bool scan_country(br_wifi_station *s, char out[2])
{
  int best = -32768;
  bool found = false;
  for (unsigned i = 0; i < s->nscan; i++) {
    const br_wifi_bss *b = &s->scan[i];
    if (b->bss.signal_dbm <= best) continue;
    for (size_t o = 0; o + 2 <= b->bss.ie_length && o + 2u + b->ies[o + 1] <= b->bss.ie_length; o += 2u + b->ies[o + 1])
      if (b->ies[o] == 7 && b->ies[o + 1] >= 2 && b->ies[o + 2] >= 'A' && b->ies[o + 2] <= 'Z' && b->ies[o + 3] >= 'A' && b->ies[o + 3] <= 'Z') {
        out[0] = (char)b->ies[o + 2]; out[1] = (char)b->ies[o + 3]; best = b->bss.signal_dbm; found = true;
        break;
      }
  }
  return found;
}

void br_wifi_scan_done(br_wifi_station *s, int error)
{
  char country[2];
  pthread_mutex_lock(&s->lock);
  struct ow_scan_done done = { s->nscan, error };
  s->scanning = false;
  bool changed = scan_country(s, country) && memcmp(country, s->country, 2);
  if (changed) memcpy(s->country, country, 2);
  pthread_mutex_unlock(&s->lock);
  if (changed) br_wifi_link_changed(s);              // the state carries the country
  send(s, OW_SCAN_DONE, &done, sizeof done, NULL, 0);
}

void br_wifi_join_done(br_wifi_station *s, uint32_t status, uint32_t code)
{
  pthread_mutex_lock(&s->lock);
  bool was = s->joining;
  if (was) {                                           // a result nobody waits for changes nothing
    s->joining = false;
    s->joined = status == OW_JOIN_OK;
    if (!s->joined) s->last_reason = code;
  }
  pthread_mutex_unlock(&s->lock);
  if (!was) return;
  if (status == OW_JOIN_OK) br_wifi_link_changed(s);   // the network's record first: the driver copies it on the result
  struct ow_join_result r = { status, code };
  send(s, OW_JOIN_RESULT, &r, sizeof r, NULL, 0);
  br_log(status == OW_JOIN_OK ? 1 : 0, "wifi: join %s (status %u, code %u)", status == OW_JOIN_OK ? "done" : "failed", status, code);
}

void br_wifi_station_message(br_wifi_station *s, const uint8_t *frame, size_t length)
{
  struct ow_header h;
  if (length < 14 + sizeof h) return;
  memcpy(&h, frame + 14, sizeof h);
  if (h.magic != OW_MAGIC || 14 + sizeof h + h.length > length) return;
  const uint8_t *p = frame + 14 + sizeof h;
  switch (h.type) {
  case OW_HELLO:
    br_log(1, "wifi: the guest's driver is up; it gets the radio");
    br_wifi_link_changed(s);
    if (s->radio->channels) {   // the channels the phone may use: APPLE80211_IOC_CHANNELS_INFO and the channel lists
      struct { struct ow_channels head; struct ow_channel ch[OW_MAX_CHANNELS]; } list;
      memset(&list, 0, sizeof list);
      int n = s->radio->channels(s->radio_ctx, list.ch, OW_MAX_CHANNELS);
      if (n > 0) {
        list.head.count = (uint16_t)n;
        send(s, OW_CHANNELS, &list.head, sizeof list.head, list.ch, (size_t)n * sizeof list.ch[0]);
        br_log(1, "wifi: %d channels of the radio for the guest (%u MHz to %u MHz)", n, list.ch[0].freq_mhz, list.ch[n - 1].freq_mhz);
      } else br_log(0, "wifi: the radio's channel list: %s; the guest keeps its own", n == 0 ? "empty" : strerror(-n));
    }
    break;
  case OW_SCAN: {
    pthread_mutex_lock(&s->lock);
    bool start = !s->scanning;
    if (start) { s->scanning = true; s->nscan = 0; }
    pthread_mutex_unlock(&s->lock);
    if (start && s->radio->scan(s->radio_ctx) < 0) br_wifi_scan_done(s, -EIO);
    break;
  }
  case OW_JOIN: {
    if (h.length < sizeof(struct ow_join)) return;
    struct ow_join j;
    memcpy(&j, p, sizeof j);
    if (!j.ssid_length || j.ssid_length > 32) return;
    br_wifi_bss known, *found = NULL;
    pthread_mutex_lock(&s->lock);
    for (unsigned i = 0; i < s->nscan; i++)
      if (s->scan[i].bss.ssid_length == j.ssid_length && !memcmp(s->scan[i].bss.ssid, j.ssid, j.ssid_length) &&
          (!found || s->scan[i].bss.signal_dbm > found->bss.signal_dbm)) found = &s->scan[i];
    if (found) { known = *found; s->current = known; } else { memset(&s->current, 0, sizeof s->current); s->current.bss.ssid_length = j.ssid_length; memcpy(s->current.bss.ssid, j.ssid, j.ssid_length); }
    s->joining = true;
    s->joined = false;
    pthread_mutex_unlock(&s->lock);
    int rc = s->radio->join(s->radio_ctx, &j, found ? &known : NULL);
    memset(&j, 0, sizeof j);
    if (rc < 0) br_wifi_join_done(s, rc == -ENOENT ? OW_JOIN_NO_NETWORK : OW_JOIN_FAILED, 0);
    break;
  }
  case OW_POWER: {                          // iOS's Wi-Fi switch: off leaves the network; the radio itself stays on
    struct ow_power power = { 1 };
    if (h.length >= sizeof power) memcpy(&power, p, sizeof power);
    if (power.on) break;
  }
  // fall through
  case OW_LEAVE:
    pthread_mutex_lock(&s->lock);
    s->joining = s->joined = false;
    s->last_reason = 3;                     // deauthenticated: the station is leaving
    pthread_mutex_unlock(&s->lock);
    if (s->radio->leave) s->radio->leave(s->radio_ctx);
    br_wifi_link_changed(s);
    break;
  default:
    break;
  }
}
