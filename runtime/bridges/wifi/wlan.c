// SPDX-License-Identifier: MIT
// The Wi-Fi bridge's back end on the phone (docs/hardware.md section 3): the guest's en0 on wlan0.
//   frames   AF_PACKET on wlan0 (all types, our own sends not read back, coalesced frames cut by frames.c)
//   scans    nl80211 (generic netlink) on wlan0, as wificond does: TRIGGER_SCAN, then GET_SCAN on NEW_SCAN_RESULTS
//   link     nl80211 GET_STATION and GET_INTERFACE while joined: signal, rate, channel
//   joins    the vendor's wpa_supplicant through its AIDL service (supplicant.c)
//   host     wlan0's state as One UI left it, kept in a file and given back (br_wlan_restore): GRO, IPv6 off, and
//            its IPv4 addresses removed for the session (one IP stack on the link, iOS's)
// Threads: frames (wlan0 to the guest), events (nl80211's scan group), control (the guest's station messages, the
// scans, the supplicant: everything that can wait). The guest's vCPU only sends a frame or queues a message.
#define _GNU_SOURCE
#include "wifi.h"
#include "supplicant.h"
#include "bridges/bridges.h"
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#ifdef __linux__
#include <arpa/inet.h>
#include <linux/ethtool.h>
#include <linux/genetlink.h>
#include <linux/if_packet.h>
#include <linux/netlink.h>
#include <linux/nl80211.h>
#include <linux/rtnetlink.h>
#include <linux/sockios.h>
#include <linux/virtio_net.h>
#include <net/if.h>
#include <sys/ioctl.h>
#include <sys/socket.h>

#ifndef PACKET_IGNORE_OUTGOING
#define PACKET_IGNORE_OUTGOING 23
#endif

enum { CT_GUEST = 1, CT_SCAN_READY, CT_SCAN_ABORTED, CT_SUPPLICANT, CT_QUIT };
struct ct_msg { uint8_t type; int event; uint32_t status, code; uint16_t length; uint8_t frame[256]; };
#define CT_QUEUE 32u
#define JOIN_TIMEOUT_MS 25000       // the guest's driver gives up at 30 s
#define SCAN_TIMEOUT_MS 8000        // the guest's at 10 s
#define LINK_POLL_MS 2000

struct br_wlan {
  char ifname[IFNAMSIZ];
  int ifindex;
  uint8_t mac[6];
  int packet;                       // AF_PACKET
  int genl;                         // nl80211 commands: the control thread's (and br_wlan_prepare's before it starts)
  int events;                       // nl80211 scan events: the events thread's
  uint16_t family;
  uint32_t seq;
  br_net_rx_fn rx;
  void *rx_ctx;
  br_wifi_station *station;
  br_supplicant *supplicant;        // the control thread's
  pthread_t frames_thread, events_thread, control_thread;
  bool threads;
  int wake[2];
  bool running;
  char state_path[512];
  uint64_t rx_frames, tx_frames, rx_dropped, segmented, tx_refused, control_dropped;
  unsigned dhcp_said;               // iOS's DHCP messages logged since the last join (the first of them)
  unsigned dhcp_others;             // other stations' DHCP messages seen since the last join (the first is logged)
  uint64_t joined_rx, joined_tx;    // the counters at the join, for the report 10 s after it
  bool report_due;
  struct timespec report_at;
  // The control thread: everything that is not a frame. The guest's vCPU, the nl80211 events and the supplicant's
  // callbacks only put a message here, so none of them waits on netlink, on binder or on the guest's queues.
  pthread_mutex_t lock;             // the queue
  pthread_cond_t more;
  struct ct_msg queue[CT_QUEUE];
  unsigned head, count;
  // the control thread's own
  bool scanning, joining, joined;
  uint32_t reason;                  // why the link went down
  struct timespec scan_deadline, join_deadline;
};

static void post(struct br_wlan *w, const struct ct_msg *m)
{
  pthread_mutex_lock(&w->lock);
  if (w->count == CT_QUEUE) w->control_dropped++;
  else {
    w->queue[(w->head + w->count) % CT_QUEUE] = *m;
    w->count++;
    pthread_cond_signal(&w->more);
  }
  pthread_mutex_unlock(&w->lock);
}

static struct timespec after_ms(unsigned ms)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  t.tv_sec += ms / 1000;
  t.tv_nsec += (long)(ms % 1000) * 1000000L;
  if (t.tv_nsec >= 1000000000L) { t.tv_sec++; t.tv_nsec -= 1000000000L; }
  return t;
}
static bool passed(const struct timespec *deadline)
{
  struct timespec now;
  clock_gettime(CLOCK_MONOTONIC, &now);
  return now.tv_sec > deadline->tv_sec || (now.tv_sec == deadline->tv_sec && now.tv_nsec >= deadline->tv_nsec);
}

// ---- netlink -----------------------------------------------------------------------------------
#define NLA_ALIGN4(n) (((n) + 3u) & ~3u)

static void put_attr(uint8_t *msg, size_t *len, uint16_t type, const void *data, uint16_t size)
{
  struct nlattr *a = (struct nlattr *)(msg + *len);
  a->nla_type = type;
  a->nla_len = (uint16_t)(NLA_HDRLEN + size);
  if (size) memcpy(msg + *len + NLA_HDRLEN, data, size);
  *len += NLA_ALIGN4(NLA_HDRLEN + size);
}

static int genl_request(struct br_wlan *w, uint8_t cmd, uint16_t flags, const void *attrs, size_t attrs_len, uint16_t family)
{
  uint8_t msg[512];
  size_t len = NLMSG_HDRLEN + GENL_HDRLEN;
  if (len + attrs_len > sizeof msg) return -E2BIG;
  memset(msg, 0, len);
  struct nlmsghdr *n = (struct nlmsghdr *)msg;
  n->nlmsg_type = family;
  n->nlmsg_flags = (uint16_t)(NLM_F_REQUEST | flags);
  n->nlmsg_seq = ++w->seq;
  struct genlmsghdr *g = (struct genlmsghdr *)(msg + NLMSG_HDRLEN);
  g->cmd = cmd;
  g->version = 1;
  memcpy(msg + len, attrs, attrs_len);
  len += attrs_len;
  n->nlmsg_len = (uint32_t)len;
  return send(w->genl, msg, len, 0) < 0 ? -errno : 0;
}

// Each attribute of a generic netlink message's payload.
typedef void (*attr_fn)(void *ctx, uint16_t type, const uint8_t *data, uint16_t size);
static void each_attr(const uint8_t *p, size_t len, attr_fn fn, void *ctx)
{
  while (len >= NLA_HDRLEN) {
    const struct nlattr *a = (const struct nlattr *)p;
    if (a->nla_len < NLA_HDRLEN || a->nla_len > len) break;
    fn(ctx, a->nla_type & NLA_TYPE_MASK, p + NLA_HDRLEN, (uint16_t)(a->nla_len - NLA_HDRLEN));
    size_t step = NLA_ALIGN4(a->nla_len);
    if (step > len) break;
    p += step;
    len -= step;
  }
}

// Reads replies to the last request until its end (DONE or an ACK/error); msg_fn gets each message's attributes.
static int genl_replies(struct br_wlan *w, attr_fn fn, void *ctx)
{
  static _Thread_local uint8_t buffer[65536];
  for (;;) {
    struct pollfd p = { .fd = w->genl, .events = POLLIN };
    if (poll(&p, 1, 3000) <= 0) return -ETIMEDOUT;
    ssize_t got = recv(w->genl, buffer, sizeof buffer, 0);
    if (got < 0) { if (errno == EINTR) continue; return -errno; }
    for (struct nlmsghdr *n = (struct nlmsghdr *)buffer; NLMSG_OK(n, (size_t)got); n = NLMSG_NEXT(n, got)) {
      if (n->nlmsg_seq != w->seq) continue;
      if (n->nlmsg_type == NLMSG_DONE) return 0;
      if (n->nlmsg_type == NLMSG_ERROR) {
        const struct nlmsgerr *e = NLMSG_DATA(n);
        return e->error;
      }
      if (fn && n->nlmsg_len >= NLMSG_HDRLEN + GENL_HDRLEN)
        each_attr((const uint8_t *)n + NLMSG_HDRLEN + GENL_HDRLEN, n->nlmsg_len - NLMSG_HDRLEN - GENL_HDRLEN, fn, ctx);
      if (!(n->nlmsg_flags & NLM_F_MULTI)) return 0;
    }
  }
}

struct family_query { const char *group; uint16_t id; uint32_t group_id; };
// One multicast group of the family: its id and its name, in the order the kernel wrote them (the id first).
static void groups_attr(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  struct family_query *q = ctx;
  uint32_t id;
  (void)type;
  if (br_wifi_nl_group(data, size, q->group, &id)) q->group_id = id;
}
static void family_attr(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  struct family_query *q = ctx;
  if (type == CTRL_ATTR_FAMILY_ID && size == 2) memcpy(&q->id, data, 2);
  if (type == CTRL_ATTR_MCAST_GROUPS) each_attr(data, size, groups_attr, ctx);
}

static int nl80211_open(struct br_wlan *w)
{
  w->genl = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_GENERIC);
  w->events = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_GENERIC);
  if (w->genl < 0 || w->events < 0) return -errno;
  struct sockaddr_nl local = { .nl_family = AF_NETLINK };
  if (bind(w->genl, (struct sockaddr *)&local, sizeof local) || bind(w->events, (struct sockaddr *)&local, sizeof local)) return -errno;
  uint8_t attrs[64];
  size_t len = 0;
  put_attr(attrs, &len, CTRL_ATTR_FAMILY_NAME, "nl80211", 8);
  struct family_query q = { .group = "scan" };
  int rc = genl_request(w, CTRL_CMD_GETFAMILY, 0, attrs, len, GENL_ID_CTRL);
  if (!rc) rc = genl_replies(w, family_attr, &q);
  if (rc) return rc;
  if (!q.id || !q.group_id) return -ENOENT;
  w->family = q.id;
  return setsockopt(w->events, SOL_NETLINK, NETLINK_ADD_MEMBERSHIP, &q.group_id, sizeof q.group_id) ? -errno : 0;
}

// ---- scans --------------------------------------------------------------------------------------
struct bss_parse { struct ow_bss bss; const uint8_t *ies; size_t ie_len; bool have; };
struct scan_sink { br_wlan_bss_fn fn; void *ctx; unsigned count; };

static void bss_field(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  struct bss_parse *b = ctx;
  switch (type) {
  case NL80211_BSS_BSSID: if (size == 6) { memcpy(b->bss.bssid, data, 6); b->have = true; } break;
  case NL80211_BSS_FREQUENCY: if (size == 4) memcpy(&b->bss.freq_mhz, data, 4); break;
  case NL80211_BSS_BEACON_INTERVAL: if (size == 2) memcpy(&b->bss.beacon_interval, data, 2); break;
  case NL80211_BSS_CAPABILITY: if (size == 2) memcpy(&b->bss.capability, data, 2); break;
  case NL80211_BSS_SIGNAL_MBM: if (size == 4) { int32_t mbm; memcpy(&mbm, data, 4); b->bss.signal_dbm = (int16_t)(mbm / 100); } break;
  case NL80211_BSS_SEEN_MS_AGO: if (size == 4) memcpy(&b->bss.age_ms, data, 4); break;
  case NL80211_BSS_INFORMATION_ELEMENTS: b->ies = data; b->ie_len = size; break;
  default: break;
  }
}

static void scan_entry(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  struct scan_sink *sink = ctx;
  if (type != NL80211_ATTR_BSS) return;
  struct bss_parse b = { 0 };
  each_attr(data, size, bss_field, &b);
  if (!b.have) return;
  // The SSID element (id 0) gives the name.
  for (size_t i = 0; b.ies && i + 2 <= b.ie_len;) {
    uint8_t id = b.ies[i], len = b.ies[i + 1];
    if (i + 2u + len > b.ie_len) break;
    if (id == 0 && len <= 32) { b.bss.ssid_length = len; memcpy(b.bss.ssid, b.ies + i + 2, len); break; }
    i += 2u + len;
  }
  sink->count++;
  sink->fn(sink->ctx, &b.bss, b.ies, b.ies ? b.ie_len : 0);
}

static int dump_scan(struct br_wlan *w, br_wlan_bss_fn fn, void *ctx)
{
  uint8_t attrs[16];
  size_t len = 0;
  uint32_t index = (uint32_t)w->ifindex;
  struct scan_sink sink = { fn, ctx, 0 };
  put_attr(attrs, &len, NL80211_ATTR_IFINDEX, &index, 4);
  int rc = genl_request(w, NL80211_CMD_GET_SCAN, NLM_F_DUMP, attrs, len, w->family);
  if (!rc) rc = genl_replies(w, scan_entry, &sink);
  return rc ? rc : (int)sink.count;
}

// An active scan of every channel the driver scans by default: one wildcard SSID, as wificond asks.
static int trigger_scan(struct br_wlan *w)
{
  uint8_t attrs[32];
  size_t len = 0;
  uint32_t index = (uint32_t)w->ifindex;
  const struct nlattr wildcard = { .nla_len = NLA_HDRLEN, .nla_type = 1 };
  put_attr(attrs, &len, NL80211_ATTR_IFINDEX, &index, 4);
  put_attr(attrs, &len, NL80211_ATTR_SCAN_SSIDS, &wildcard, sizeof wildcard);
  int rc = genl_request(w, NL80211_CMD_TRIGGER_SCAN, NLM_F_ACK, attrs, len, w->family);
  if (!rc) rc = genl_replies(w, NULL, NULL);
  return rc;
}

static void station_bss(void *ctx, const struct ow_bss *bss, const uint8_t *ies, size_t ie_length)
{
  struct br_wlan *w = ctx;
  br_wifi_scan_add(w->station, bss, ies, ie_length);
}

static void finish_scan(struct br_wlan *w, int error)
{
  if (!w->scanning) return;
  w->scanning = false;
  int rc = error ? error : dump_scan(w, station_bss, w);
  br_wifi_scan_done(w->station, rc < 0 ? rc : 0);
}

static int radio_scan(void *ctx)
{
  struct br_wlan *w = ctx;
  int rc = trigger_scan(w);
  // Busy: a scan of the supplicant's (or the driver's) runs; its results come with NEW_SCAN_RESULTS all the same.
  if (rc == -EBUSY) rc = 0;
  if (rc) { br_log(0, "wifi: nl80211 TRIGGER_SCAN on %s: %s", w->ifname, strerror(-rc)); return rc; }
  w->scanning = true;
  w->scan_deadline = after_ms(SCAN_TIMEOUT_MS);
  return 0;
}

// The scan event group: NEW_SCAN_RESULTS (ours or anyone's) and SCAN_ABORTED, for our interface.
static void *events_loop(void *arg)
{
  struct br_wlan *w = arg;
  uint8_t buffer[8192];
  struct pollfd fds[2] = { { .fd = w->events, .events = POLLIN }, { .fd = w->wake[0], .events = POLLIN } };
  while (__atomic_load_n(&w->running, __ATOMIC_ACQUIRE)) {
    if (poll(fds, 2, -1) < 0) { if (errno == EINTR) continue; break; }
    if (fds[1].revents) break;
    ssize_t got = recv(w->events, buffer, sizeof buffer, MSG_DONTWAIT);
    if (got <= 0) continue;
    for (struct nlmsghdr *n = (struct nlmsghdr *)buffer; NLMSG_OK(n, (size_t)got); n = NLMSG_NEXT(n, got)) {
      if (n->nlmsg_len < NLMSG_HDRLEN + GENL_HDRLEN || n->nlmsg_type != w->family) continue;
      const struct genlmsghdr *g = (const struct genlmsghdr *)((const uint8_t *)n + NLMSG_HDRLEN);
      struct ct_msg m = { .type = g->cmd == NL80211_CMD_NEW_SCAN_RESULTS ? CT_SCAN_READY : g->cmd == NL80211_CMD_SCAN_ABORTED ? CT_SCAN_ABORTED : 0 };
      if (m.type) post(w, &m);
    }
  }
  return NULL;
}

// ---- the link -----------------------------------------------------------------------------------
static void rate_field(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  br_wifi_link *l = ctx;
  if (type == NL80211_RATE_INFO_BITRATE32 && size == 4) { uint32_t r; memcpy(&r, data, 4); l->tx_rate_mbps = r / 10; }
  else if (type == NL80211_RATE_INFO_BITRATE && size == 2 && !l->tx_rate_mbps) { uint16_t r; memcpy(&r, data, 2); l->tx_rate_mbps = r / 10u; }
}
static void sta_field(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  br_wifi_link *l = ctx;
  if (type == NL80211_STA_INFO_SIGNAL && size >= 1) l->rssi_dbm = (int8_t)data[0];
  else if (type == NL80211_STA_INFO_TX_BITRATE) each_attr(data, size, rate_field, l);
}
static void station_attr(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  br_wifi_link *l = ctx;
  if (type == NL80211_ATTR_MAC && size == 6) memcpy(l->bssid, data, 6);
  else if (type == NL80211_ATTR_STA_INFO) each_attr(data, size, sta_field, l);
}
static void interface_attr(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  br_wifi_link *l = ctx;
  if (type == NL80211_ATTR_WIPHY_FREQ && size == 4) memcpy(&l->freq_mhz, data, 4);
}

// Up while the supplicant says the network is joined; the signal, the rate and the access point from nl80211's
// station entry (a station's only peer is its access point), the channel from the interface.
static int radio_link(void *ctx, br_wifi_link *out)
{
  struct br_wlan *w = ctx;
  memset(out, 0, sizeof *out);
  out->noise_dbm = -95;
  out->up = w->joined;
  out->reason = w->reason;
  if (!w->joined) return 0;
  uint8_t attrs[16];
  size_t len = 0;
  uint32_t index = (uint32_t)w->ifindex;
  put_attr(attrs, &len, NL80211_ATTR_IFINDEX, &index, 4);
  if (!genl_request(w, NL80211_CMD_GET_STATION, NLM_F_DUMP, attrs, len, w->family)) (void)genl_replies(w, station_attr, out);
  if (!genl_request(w, NL80211_CMD_GET_INTERFACE, 0, attrs, len, w->family)) (void)genl_replies(w, interface_attr, out);
  if (!out->freq_mhz && w->supplicant) out->freq_mhz = br_supplicant_frequency(w->supplicant);
  if (!out->rssi_dbm) out->rssi_dbm = -60;         // no station entry yet: a middling signal, not "none"
  return 0;
}

static int radio_join(void *ctx, const struct ow_join *j, const br_wifi_bss *bss)
{
  struct br_wlan *w = ctx;
  if (!w->supplicant) return -ENODEV;
  w->joined = false;
  int rc = br_supplicant_join(w->supplicant, j, bss ? bss->ies : NULL, bss ? bss->bss.ie_length : 0);
  w->joining = rc == 0;
  if (!rc) w->join_deadline = after_ms(JOIN_TIMEOUT_MS);
  return rc;
}

static int radio_leave(void *ctx)
{
  struct br_wlan *w = ctx;
  w->joining = w->joined = false;
  w->reason = 3;                                    // the station left
  return w->supplicant ? br_supplicant_leave(w->supplicant) : 0;
}

static const br_wifi_radio radio = { .scan = radio_scan, .join = radio_join, .leave = radio_leave, .link = radio_link };

// The supplicant's verdicts (its looper thread): to the control thread.
static void supplicant_event(void *ctx, int event, uint32_t status, uint32_t code)
{
  struct ct_msg m = { .type = CT_SUPPLICANT, .event = event, .status = status, .code = code };
  post(ctx, &m);
}

static void control_supplicant(struct br_wlan *w, const struct ct_msg *m)
{
  switch (m->event) {
  case BR_SUPPLICANT_JOINED:
    if (!w->joining) break;
    w->joining = false;
    w->joined = true;
    w->reason = 0;
    __atomic_store_n(&w->dhcp_said, 0, __ATOMIC_RELAXED);
    __atomic_store_n(&w->dhcp_others, 0, __ATOMIC_RELAXED);
    w->joined_rx = w->rx_frames;
    w->joined_tx = w->tx_frames;
    w->report_due = true;
    w->report_at = after_ms(10000);
    br_wifi_join_done(w->station, OW_JOIN_OK, 0);
    break;
  case BR_SUPPLICANT_JOIN_FAILED:
    if (!w->joining) break;
    w->joining = false;
    (void)br_supplicant_leave(w->supplicant);       // or it tries the network again by itself
    br_wifi_join_done(w->station, m->status, m->code);
    break;
  case BR_SUPPLICANT_DISCONNECTED: case BR_SUPPLICANT_DIED:
    // iOS decides whether to join again: the network goes out of the supplicant, which would come back on its own.
    if (m->event == BR_SUPPLICANT_DISCONNECTED) (void)br_supplicant_leave(w->supplicant);
    if (w->joining) { w->joining = false; br_wifi_join_done(w->station, OW_JOIN_FAILED, m->code); }
    if (w->joined) {
      w->joined = false;
      w->reason = m->code ? m->code : 1;
      br_log(1, "wifi: link down (reason %u)", w->reason);
      br_wifi_link_changed(w->station);
    }
    break;
  default: break;
  }
}

static void *control_loop(void *arg)
{
  struct br_wlan *w = arg;
  // The supplicant first: a lazy service, started by this lookup when One UI's framework took it down with itself.
  w->supplicant = br_supplicant_open(w->ifname, 8000, supplicant_event, w);
  br_log(w->supplicant ? 1 : 0, "wifi: %s", w->supplicant ? "the vendor's supplicant is the bridge's: joins by its AIDL service"
                                                          : "no supplicant: iOS can scan but not join");
  struct timespec poll_at = after_ms(LINK_POLL_MS);
  for (;;) {
    struct ct_msg m = { 0 };
    pthread_mutex_lock(&w->lock);
    if (!w->count) {
      struct timespec until = after_ms(w->joining || w->scanning ? 250 : LINK_POLL_MS);
      pthread_cond_timedwait(&w->more, &w->lock, &until);
    }
    if (w->count) {
      m = w->queue[w->head];
      w->head = (w->head + 1) % CT_QUEUE;
      w->count--;
    }
    pthread_mutex_unlock(&w->lock);
    if (m.type == CT_QUIT) break;
    if (m.type == CT_GUEST) br_wifi_station_message(w->station, m.frame, m.length);
    else if (m.type == CT_SCAN_READY) finish_scan(w, 0);
    else if (m.type == CT_SCAN_ABORTED) finish_scan(w, -ECANCELED);
    else if (m.type == CT_SUPPLICANT) control_supplicant(w, &m);
    memset(&m, 0, sizeof m);                        // a join's message carried the key
    if (w->scanning && passed(&w->scan_deadline)) finish_scan(w, 0);        // no event: what the driver has
    if (w->joining && passed(&w->join_deadline)) {
      w->joining = false;
      br_log(0, "wifi: join: not joined in %u s: given up", JOIN_TIMEOUT_MS / 1000);
      (void)br_supplicant_leave(w->supplicant);
      br_wifi_join_done(w->station, OW_JOIN_TIMEOUT, 0);
    }
    if (passed(&poll_at)) { poll_at = after_ms(LINK_POLL_MS); if (w->joined) br_wifi_link_changed(w->station); }   // the signal
    if (w->report_due && passed(&w->report_at)) {   // did iOS's traffic start: its DHCP lines are above this one
      w->report_due = false;
      if (w->joined) br_log(1, "wifi: 10 s after the join: %llu frames from iOS, %llu to it; %u DHCP messages of other stations", (unsigned long long)(w->tx_frames - w->joined_tx),
                            (unsigned long long)(w->rx_frames - w->joined_rx), __atomic_load_n(&w->dhcp_others, __ATOMIC_RELAXED));
    }
  }
  if (w->supplicant) br_supplicant_close(w->supplicant);     // leaves the network, drops what it added
  w->supplicant = NULL;
  return NULL;
}

// ---- frames -------------------------------------------------------------------------------------
// iOS's address configuration as it crosses the bridge: each DHCP message of a join's first ones, one line.
// Only iOS's own exchange is logged and counted against that limit: a message whose client (chaddr) is another
// station's (its broadcast DISCOVER, INIT-REBOOT or REBINDING REQUEST reaches every station, iOS too) is counted, the
// first after a join named once. Before 2026-11-13 every one of them was logged as a server's answer with the
// yiaddr a REQUEST never has ("DHCP: REQUEST of 0.0.0.0 from 0.0.0.0", repeated on a busy hotel network).
static void say_dhcp(struct br_wlan *w, const uint8_t *frame, size_t length, bool from_guest)
{
  br_wifi_dhcp_msg m;
  char line[160];
  if (!br_wifi_dhcp(frame, length, &m)) return;
  if (br_wifi_dhcp_describe(&m, from_guest, w->mac, line, sizeof line) == BR_WIFI_DHCP_OTHER) {
    if (__atomic_fetch_add(&w->dhcp_others, 1, __ATOMIC_RELAXED) == 0)
      br_log(1, "wifi: DHCP: %s (passed to iOS as any broadcast; such messages are counted, not logged)", line);
    return;
  }
  if (__atomic_load_n(&w->dhcp_said, __ATOMIC_RELAXED) >= 16) return;
  __atomic_fetch_add(&w->dhcp_said, 1, __ATOMIC_RELAXED);
  br_log(1, "wifi: DHCP: %s", line);
}

static void deliver(void *ctx, const uint8_t *frame, size_t length)
{
  struct br_wlan *w = ctx;
  if (w->rx) w->rx(w->rx_ctx, frame, length);
}

static void *frames_loop(void *arg)
{
  struct br_wlan *w = arg;
  static uint8_t buffer[sizeof(struct virtio_net_hdr) + 65536];
  struct pollfd fds[2] = { { .fd = w->packet, .events = POLLIN }, { .fd = w->wake[0], .events = POLLIN } };
  while (__atomic_load_n(&w->running, __ATOMIC_ACQUIRE)) {
    if (poll(fds, 2, -1) < 0) { if (errno == EINTR) continue; break; }
    if (fds[1].revents) break;
    if (!(fds[0].revents & POLLIN)) continue;
    ssize_t got = recv(w->packet, buffer, sizeof buffer, MSG_DONTWAIT);
    if (got < (ssize_t)(sizeof(struct virtio_net_hdr) + 14)) continue;
    struct virtio_net_hdr h;
    memcpy(&h, buffer, sizeof h);
    const uint8_t *frame = buffer + sizeof h;
    size_t length = (size_t)got - sizeof h;
    if (!br_wifi_to_guest(frame, length, w->mac)) continue;
    say_dhcp(w, frame, length, false);
    // A frame the receive path coalesced (GRO was on, or came back on) has no checksum of its own any more.
    const bool coalesced = h.gso_type != VIRTIO_NET_HDR_GSO_NONE || (h.flags & VIRTIO_NET_HDR_F_NEEDS_CSUM);
    size_t n = br_wifi_segment(frame, length, h.gso_type != VIRTIO_NET_HDR_GSO_NONE ? h.gso_size : 1448, coalesced, deliver, w);
    if (!n) w->rx_dropped++;
    else { w->rx_frames += n; if (n > 1) w->segmented++; }
  }
  return NULL;
}

static int packet_open(struct br_wlan *w)
{
  w->packet = socket(AF_PACKET, SOCK_RAW | SOCK_CLOEXEC, htons(ETH_P_ALL));
  if (w->packet < 0) return -errno;
  int on = 1;
  if (setsockopt(w->packet, SOL_PACKET, PACKET_VNET_HDR, &on, sizeof on) ||
      setsockopt(w->packet, SOL_PACKET, PACKET_IGNORE_OUTGOING, &on, sizeof on)) return -errno;
  struct sockaddr_ll ll = { .sll_family = AF_PACKET, .sll_protocol = htons(ETH_P_ALL), .sll_ifindex = w->ifindex };
  if (bind(w->packet, (struct sockaddr *)&ll, sizeof ll)) return -errno;
  // Every multicast group: iOS's IPv6 neighbour discovery (its solicited-node group) and mDNS are groups Linux has
  // not joined on wlan0, and the driver passes up only the groups of the interface's list. The membership is the
  // socket's: it ends when the socket closes, also after a kill.
  struct packet_mreq all = { .mr_ifindex = w->ifindex, .mr_type = PACKET_MR_ALLMULTI };
  if (setsockopt(w->packet, SOL_PACKET, PACKET_ADD_MEMBERSHIP, &all, sizeof all))
    br_log(0, "wifi: %s: all-multicast not set (%s): IPv6 neighbour discovery and mDNS may not reach iOS", w->ifname, strerror(errno));
  return 0;
}

// ---- wlan0's host state -------------------------------------------------------------------------
static int read_int(const char *path, int fallback)
{
  FILE *f = fopen(path, "r");
  int v = fallback;
  if (f) { if (fscanf(f, "%d", &v) != 1) v = fallback; fclose(f); }
  return v;
}
static void write_int(const char *path, int value)
{
  FILE *f = fopen(path, "w");
  if (f) { fprintf(f, "%d\n", value); fclose(f); }
}

static int gro(const char *ifname, int set)      // set < 0: read
{
  int fd = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
  if (fd < 0) return -errno;
  struct ethtool_value v = { .cmd = set < 0 ? ETHTOOL_GGRO : ETHTOOL_SGRO, .data = set > 0 };
  struct ifreq r;
  memset(&r, 0, sizeof r);
  snprintf(r.ifr_name, sizeof r.ifr_name, "%s", ifname);
  r.ifr_data = (char *)&v;
  int rc = ioctl(fd, SIOCETHTOOL, &r) ? -errno : (int)v.data;
  close(fd);
  return rc;
}

static bool read_state(const char *path, char ifname[IFNAMSIZ], int *v6, int *g)
{
  FILE *f = fopen(path, "r");
  char line[128];
  if (!f) return false;
  ifname[0] = 0;
  while (fgets(line, sizeof line, f)) {
    sscanf(line, "ifname=%15s", ifname);
    sscanf(line, "disable_ipv6=%d", v6);
    sscanf(line, "gro=%d", g);
  }
  fclose(f);
  return *ifname && !strchr(ifname, '/');
}

// What One UI left, written before anything changes; br_wlan_restore (the stop, and `wifictl restore` after a
// killed xnuhost) reads it. Two settings only, and neither breaks One UI's Wi-Fi when it is never put back: IPv6
// off is how the framework itself leaves wlan0 between networks (it turns it on when it joins), GRO off only costs
// receive speed.
static void host_take(struct br_wlan *w)
{
  char p[160], old[IFNAMSIZ];
  snprintf(p, sizeof p, "/proc/sys/net/ipv6/conf/%s/disable_ipv6", w->ifname);
  int v6 = read_int(p, 0);
  int g = gro(w->ifname, -1);
  if (g < 0) g = 1;
  // A file that is there already is a start after a restart in place: its values are One UI's, the current ones ours.
  bool kept = *w->state_path && read_state(w->state_path, old, &v6, &g) && !strcmp(old, w->ifname);
  FILE *f = *w->state_path && !kept ? fopen(w->state_path, "w") : NULL;
  if (f) {
    fprintf(f, "ifname=%s\ndisable_ipv6=%d\ngro=%d\n", w->ifname, v6, g);
    fclose(f);
  }
  write_int(p, 1);
  (void)gro(w->ifname, 0);
  // One IP stack on the link: One UI's IPv4 addresses on wlan0 go (its framework adds them again after the return).
  char command[200];
  snprintf(command, sizeof command, "PATH=/system/bin:$PATH ip -4 addr flush dev %s >/dev/null 2>&1", w->ifname);
  (void)!system(command);
  br_log(1, "wifi: %s is iOS's for the session: IPv4 addresses removed, IPv6 off (was %s), GRO off (was %s); the old values are in %s",
         w->ifname, v6 ? "off" : "on", g ? "on" : "off", *w->state_path ? w->state_path : "(nowhere: no session folder)");
}

int br_wlan_restore(const char *state_path)
{
  char ifname[IFNAMSIZ];
  int v6 = 0, g = 1;
  if (!read_state(state_path, ifname, &v6, &g)) return errno == ENOENT ? -ENOENT : -EINVAL;
  char p[160];
  snprintf(p, sizeof p, "/proc/sys/net/ipv6/conf/%s/disable_ipv6", ifname);
  write_int(p, v6);
  (void)gro(ifname, g);
  unlink(state_path);
  br_log(1, "wifi: %s given back: IPv6 %s, GRO %s", ifname, v6 ? "off" : "on", g ? "on" : "off");
  return 0;
}

// ---- the back end -------------------------------------------------------------------------------
static struct br_wlan *the_wlan;

static struct br_wlan *wlan_new(const char *ifname)
{
  struct br_wlan *w = calloc(1, sizeof *w);
  if (!w) return NULL;
  snprintf(w->ifname, sizeof w->ifname, "%s", ifname && *ifname ? ifname : "wlan0");
  w->packet = w->genl = w->events = w->wake[0] = w->wake[1] = -1;
  w->ifindex = (int)if_nametoindex(w->ifname);
  char path[128];
  snprintf(path, sizeof path, "/sys/class/net/%s/address", w->ifname);
  FILE *f = fopen(path, "r");
  unsigned m[6];
  bool mac = f && fscanf(f, "%x:%x:%x:%x:%x:%x", &m[0], &m[1], &m[2], &m[3], &m[4], &m[5]) == 6;
  if (f) fclose(f);
  if (!w->ifindex || !mac) { br_log(0, "wifi: off: no %s (One UI's Wi-Fi was off at the switch?): iOS gets no Wi-Fi", w->ifname); free(w); return NULL; }
  for (int i = 0; i < 6; i++) w->mac[i] = (uint8_t)m[i];
  int rc = nl80211_open(w);
  if (rc < 0) {
    br_log(0, "wifi: off: nl80211: %s", strerror(-rc));
    if (w->genl >= 0) close(w->genl);
    if (w->events >= 0) close(w->events);
    free(w);
    return NULL;
  }
  return w;
}

static void wlan_free(struct br_wlan *w)
{
  for (int *fd = (int[]){ w->packet, w->genl, w->events, w->wake[0], w->wake[1], -2 }; *fd != -2; fd++) if (*fd >= 0) close(*fd);
  if (w->station) br_wifi_station_destroy(w->station);
  free(w);
}

static br_net *wlan_open(const br_net_config *c, br_net_rx_fn rx, void *ctx)
{
  (void)c;
  struct br_wlan *w = the_wlan;
  if (!w) return NULL;
  w->rx = rx;
  w->rx_ctx = ctx;
  w->running = true;
  if (pthread_create(&w->frames_thread, NULL, frames_loop, w) || pthread_create(&w->events_thread, NULL, events_loop, w) ||
      pthread_create(&w->control_thread, NULL, control_loop, w)) {
    br_log(0, "wifi: off: no threads");
    return NULL;                  // a thread may be running: w stays (the bridge lives as long as xnuhost)
  }
  w->threads = true;
  return (br_net *)w;
}

br_net_open_fn br_wlan_prepare(const char *ifname, const char *session_dir, uint8_t mac_out[6])
{
  if (the_wlan) return NULL;
  struct br_wlan *w = wlan_new(ifname);
  if (!w) return NULL;
  int rc;
  if ((rc = packet_open(w)) < 0) { br_log(0, "wifi: off: AF_PACKET on %s: %s", w->ifname, strerror(-rc)); wlan_free(w); return NULL; }
  pthread_condattr_t attr;
  pthread_condattr_init(&attr);
  pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
  pthread_mutex_init(&w->lock, NULL);
  pthread_cond_init(&w->more, &attr);
  w->station = br_wifi_station_create(&radio, w, w->mac, NULL, deliver, w);
  if (pipe2(w->wake, O_CLOEXEC) || !w->station) { wlan_free(w); return NULL; }
  if (session_dir && *session_dir) snprintf(w->state_path, sizeof w->state_path, "%s/wifi.state", session_dir);
  host_take(w);
  memcpy(mac_out, w->mac, 6);
  the_wlan = w;
  br_log(1, "wifi: on: %s (%02x:%02x:%02x:..) is the guest's en0: scans by nl80211, joins by the vendor's supplicant, frames by AF_PACKET",
         w->ifname, w->mac[0], w->mac[1], w->mac[2]);
  return wlan_open;
}

int br_wlan_send(br_net *n, const uint8_t *frame, size_t length)
{
  struct br_wlan *w = (struct br_wlan *)n;
  if (br_wifi_ethertype(frame, length) == BR_WIFI_CONTROL_TYPE) {
    struct ct_msg m = { .type = CT_GUEST, .length = (uint16_t)length };
    if (length > sizeof m.frame) return -EMSGSIZE;
    memcpy(m.frame, frame, length);
    post(w, &m);
    memset(&m, 0, sizeof m);
    return 0;
  }
  if (!br_wifi_to_radio(frame, length, w->mac)) { w->tx_refused++; return -EINVAL; }
  say_dhcp(w, frame, length, true);
  uint8_t packet[sizeof(struct virtio_net_hdr) + BR_WIFI_FRAME];
  memset(packet, 0, sizeof(struct virtio_net_hdr));
  memcpy(packet + sizeof(struct virtio_net_hdr), frame, length);
  ssize_t sent = send(w->packet, packet, sizeof(struct virtio_net_hdr) + length, MSG_DONTWAIT);
  if (sent < 0) return -errno;
  w->tx_frames++;
  return 0;
}

void br_wlan_close(void)
{
  struct br_wlan *w = the_wlan;
  if (!w) return;
  the_wlan = NULL;
  if (w->threads) {
    __atomic_store_n(&w->running, false, __ATOMIC_RELEASE);
    (void)!write(w->wake[1], "", 1);
    struct ct_msg quit = { .type = CT_QUIT };
    pthread_mutex_lock(&w->lock);
    w->count = 0;                                 // the stop goes first
    pthread_mutex_unlock(&w->lock);
    post(w, &quit);
    pthread_join(w->control_thread, NULL);        // the supplicant: the network left, what was added removed
    pthread_join(w->frames_thread, NULL);
    pthread_join(w->events_thread, NULL);
  }
  if (*w->state_path) br_wlan_restore(w->state_path);
  br_log(1, "wifi: closed (%llu frames to the guest, %llu from it, %llu cut from coalesced frames, %llu dropped, %llu of the guest's refused)",
         (unsigned long long)w->rx_frames, (unsigned long long)w->tx_frames, (unsigned long long)w->segmented, (unsigned long long)w->rx_dropped,
         (unsigned long long)w->tx_refused);
  // w stays: a vCPU may still be inside br_wlan_send (the process ends next)
}

int br_wlan_scan_once(const char *ifname, unsigned wait_ms, br_wlan_bss_fn fn, void *ctx)
{
  struct br_wlan *w = wlan_new(ifname);
  if (!w) return -ENODEV;
  int rc = trigger_scan(w);
  if (rc && rc != -EBUSY) br_log(0, "wifi: nl80211 TRIGGER_SCAN on %s: %s (the networks the driver has are listed)", w->ifname, strerror(-rc));
  else {
    uint8_t buffer[8192];
    struct timespec until = after_ms(wait_ms);
    for (bool done = false; !done && !passed(&until);) {
      struct pollfd p = { .fd = w->events, .events = POLLIN };
      if (poll(&p, 1, 250) <= 0) continue;
      ssize_t got = recv(w->events, buffer, sizeof buffer, MSG_DONTWAIT);
      for (struct nlmsghdr *n = (struct nlmsghdr *)buffer; got > 0 && NLMSG_OK(n, (size_t)got); n = NLMSG_NEXT(n, got)) {
        if (n->nlmsg_len < NLMSG_HDRLEN + GENL_HDRLEN || n->nlmsg_type != w->family) continue;
        const struct genlmsghdr *g = (const struct genlmsghdr *)((const uint8_t *)n + NLMSG_HDRLEN);
        if (g->cmd == NL80211_CMD_NEW_SCAN_RESULTS || g->cmd == NL80211_CMD_SCAN_ABORTED) done = true;
      }
    }
  }
  rc = dump_scan(w, fn, ctx);
  wlan_free(w);
  return rc;
}
#else
br_net_open_fn br_wlan_prepare(const char *ifname, const char *session_dir, uint8_t mac_out[6]) { (void)ifname; (void)session_dir; (void)mac_out; return NULL; }
int br_wlan_send(br_net *n, const uint8_t *frame, size_t length) { (void)n; (void)frame; (void)length; return -ENOSYS; }
void br_wlan_close(void) {}
int br_wlan_restore(const char *state_path) { (void)state_path; return -ENOSYS; }
int br_wlan_scan_once(const char *ifname, unsigned wait_ms, br_wlan_bss_fn fn, void *ctx) { (void)ifname; (void)wait_ms; (void)fn; (void)ctx; return -ENOSYS; }
#endif
