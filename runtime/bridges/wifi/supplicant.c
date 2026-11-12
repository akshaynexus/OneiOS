// SPDX-License-Identifier: MIT
// The vendor's wpa_supplicant as the Wi-Fi bridge's station (supplicant.h; docs/hardware.md section 3).
//
// What it does on the phone, as One UI's framework does: finds the service, takes the station interface of wlan0
// (adds it when the supplicant came up with none: it is a lazy service that ends when its client dies and starts on
// the next lookup), registers one callback object, and for a join adds one network, sets what the network needs and
// selects it. The supplicant does the 802.11 authentication, the 4-way handshake and the rekeys with the chip's
// driver; this file only says which network and reports how it went. iOS decides when to join and leave: when the
// link drops, the network is removed, so the supplicant does not come back on its own.
#define _GNU_SOURCE
#include "supplicant.h"
#include "bridges/bridges.h"
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

// ---- what a network asks for --------------------------------------------------------------------
static void suites(const uint8_t *p, size_t count, bool wpa1, br_wifi_security *out, bool akm)
{
  static const uint8_t rsn_oui[3] = { 0x00, 0x0f, 0xac }, wpa_oui[3] = { 0x00, 0x50, 0xf2 };
  for (size_t i = 0; i < count; i++, p += 4) {
    if (memcmp(p, wpa1 ? wpa_oui : rsn_oui, 3)) continue;
    const uint8_t t = p[3];
    if (akm) {
      if (t == 2 || t == 4) out->psk = true;                     // PSK, FT-PSK
      else if (t == 6) out->psk_sha256 = true;
      else if (!wpa1 && (t == 8 || t == 9 || t == 24 || t == 25)) out->sae = true;   // SAE, FT-SAE and their extended-key kinds
      else if (!wpa1 && t == 18) out->owe = true;
      else if (t == 1 || t == 3 || t == 5 || t == 11 || t == 12 || t == 13) out->eap = true;
    } else {
      if (t == 2) out->tkip = true;
      else if (t == 4) out->ccmp = true;
      else if (!wpa1 && (t == 8 || t == 9)) out->gcmp = true;
    }
  }
}

// version(2) group(4) pairwise count(2) + 4 each, AKM count(2) + 4 each, capabilities(2)
static void rsn_body(const uint8_t *p, size_t n, bool wpa1, br_wifi_security *out)
{
  if (n < 8) return;
  size_t at = 6;
  size_t count = (size_t)(p[at] | p[at + 1] << 8);
  at += 2;
  if (at + 4 * count > n) return;
  suites(p + at, count, wpa1, out, false);
  at += 4 * count;
  if (at + 2 > n) return;
  count = (size_t)(p[at] | p[at + 1] << 8);
  at += 2;
  if (at + 4 * count > n) return;
  suites(p + at, count, wpa1, out, true);
  at += 4 * count;
  if (wpa1 || at + 2 > n) return;
  const unsigned caps = (unsigned)(p[at] | p[at + 1] << 8);
  out->pmf_required = caps & 0x40;
  out->pmf_capable = caps & 0x80;
}

void br_wifi_security_parse(const uint8_t *ies, size_t length, br_wifi_security *out)
{
  memset(out, 0, sizeof *out);
  for (size_t i = 0; ies && i + 2 <= length;) {
    const uint8_t id = ies[i], len = ies[i + 1];
    if (i + 2u + len > length) break;
    const uint8_t *body = ies + i + 2;
    if (id == 48) { out->rsn = true; rsn_body(body, len, false, out); }
    else if (id == 221 && len >= 4 && body[0] == 0x00 && body[1] == 0x50 && body[2] == 0xf2 && body[3] == 1) {
      out->wpa = true;
      rsn_body(body + 4, len - 4u, true, out);
    }
    i += 2u + len;
  }
}

int br_wifi_plan_join(const struct ow_join *join, const br_wifi_security *seen, br_wifi_plan *plan)
{
  memset(plan, 0, sizeof *plan);
  plan->scan_ssid = !seen;
  const bool key = join->key_length != 0 && join->security != OW_SECURITY_OPEN;
  if (!key) {
    if (seen && seen->owe && !seen->psk && !seen->sae) {          // Enhanced Open
      plan->key_mgmt = KM_OWE; plan->proto = PROTO_RSN; plan->pairwise = PC_CCMP | PC_GCMP_256; plan->group = GC_CCMP | GC_GCMP_256;
      plan->set_pmf = plan->require_pmf = true;
      plan->kind = "enhanced open (OWE)";
    } else {
      plan->key_mgmt = KM_NONE;
      plan->kind = "open";
    }
    return 0;
  }
  plan->proto = PROTO_RSN;
  plan->pairwise = PC_CCMP | PC_GCMP_128 | PC_GCMP_256;
  plan->group = GC_CCMP | GC_GCMP_128 | GC_GCMP_256;
  const bool sae_only = seen && seen->sae && !seen->psk && !seen->psk_sha256;
  if (join->key_raw) {
    if (join->key_length != 32) return -EINVAL;
    if (sae_only || join->security == OW_SECURITY_SAE) return -EINVAL;   // WPA3 needs the password itself
    plan->psk_raw = true;
    plan->kind = "WPA2 (key)";
  } else if (sae_only || join->security == OW_SECURITY_SAE) {
    plan->key_mgmt = KM_SAE;
    plan->sae_password = true;
    plan->set_pmf = plan->require_pmf = true;
    plan->kind = "WPA3 (SAE)";
    return 0;
  } else {
    plan->psk_passphrase = true;
    plan->kind = seen && seen->sae ? "WPA2 (passphrase; the network also offers WPA3)" : "WPA2 (passphrase)";
  }
  plan->key_mgmt = KM_WPA_PSK | KM_WPA_PSK_SHA256;
  if (!seen || seen->wpa || seen->tkip) {                          // an old network, or one not seen: WPA and TKIP allowed too
    plan->proto |= PROTO_WPA;
    plan->pairwise |= PC_TKIP;
    plan->group |= GC_TKIP;
  }
  return 0;
}

// ---- the parcels --------------------------------------------------------------------------------
void sup_parcel_get_service(ab_parcel *p, const char *name) { ab_init(p); ab_put_token(p, SUP_MANAGER); ab_put_string(p, name); }
void sup_parcel_empty(ab_parcel *p, const char *descriptor) { ab_init(p); ab_put_token(p, descriptor); }
void sup_parcel_string(ab_parcel *p, const char *descriptor, const char *text) { sup_parcel_empty(p, descriptor); ab_put_string(p, text); }
void sup_parcel_i32(ab_parcel *p, const char *descriptor, int32_t value) { sup_parcel_empty(p, descriptor); ab_put_i32(p, value); }
void sup_parcel_bytes(ab_parcel *p, const char *descriptor, const uint8_t *bytes, size_t length) { sup_parcel_empty(p, descriptor); ab_put_bytes(p, bytes, length); }
void sup_parcel_callback(ab_parcel *p, uint64_t object) { sup_parcel_empty(p, SUP_STA_IFACE); ab_put_local(p, object); }

void sup_decode_callback(uint32_t code, ab_reader *r, sup_event *e)
{
  const uint8_t *bytes;
  size_t n, end;
  memset(e, 0, sizeof *e);
  switch (code) {
  case CB_STATE_CHANGED:                     // newState, bssid, id, ssid, filsHlpSent
    e->state = ab_get_i32(r);
    if (ab_get_bytes(r, &bytes, &n) && n == 6) memcpy(e->bssid, bytes, 6);
    e->id = ab_get_i32(r);
    break;
  case CB_SUPPLICANT_STATE_CHANGED:          // SupplicantStateChangeData: newState, id, ssid, bssid, keyMgmtMask, frequencyMhz, filsHlpSent
    if (!(end = ab_get_parcelable(r))) return;
    e->state = ab_get_i32(r);
    e->id = ab_get_i32(r);
    (void)ab_get_bytes(r, &bytes, &n);
    if (ab_get_bytes(r, &bytes, &n) && n == 6) memcpy(e->bssid, bytes, 6);
    (void)ab_get_i32(r);
    e->frequency = ab_get_i32(r);
    break;
  case CB_DISCONNECTED:                      // bssid, locallyGenerated, reasonCode
    if (ab_get_bytes(r, &bytes, &n) && n == 6) memcpy(e->bssid, bytes, 6);
    e->local = ab_get_i32(r) != 0;
    e->code = (uint32_t)ab_get_i32(r);
    break;
  case CB_ASSOCIATION_REJECTED:              // AssociationRejectionData: ssid, bssid, statusCode, timedOut, ...
    if (!(end = ab_get_parcelable(r))) return;
    (void)ab_get_bytes(r, &bytes, &n);
    if (ab_get_bytes(r, &bytes, &n) && n == 6) memcpy(e->bssid, bytes, 6);
    e->code = (uint32_t)ab_get_i32(r);
    e->timed_out = ab_get_i32(r) != 0;
    break;
  case CB_AUTHENTICATION_TIMEOUT:            // bssid
    if (ab_get_bytes(r, &bytes, &n) && n == 6) memcpy(e->bssid, bytes, 6);
    break;
  case CB_NETWORK_NOT_FOUND: break;          // ssid
  case CB_BSS_FREQUENCY_CHANGED: e->frequency = ab_get_i32(r); break;
  default: return;                           // not one the bridge uses
  }
  if (!r->bad) e->kind = (int)code;
}

// ---- the client ---------------------------------------------------------------------------------
#define CALLBACK_OBJECT 0x4f57434bull        // "OWCB": our one object
#define DEATH_COOKIE 0x4f575350ull

struct br_supplicant {
  ab *b;
  char ifname[16];
  br_supplicant_event_fn fn;
  void *ctx;
  uint32_t service, iface, network;          // handles
  bool have_service, have_iface, have_network, added_iface;
  int32_t network_id;
  pthread_mutex_t lock;                      // the join's state, set by calls and by the looper's callbacks
  bool dead, joining, joined, keyed;
  int reached;                               // the furthest state of this join
  unsigned not_found, shown;
  int32_t frequency;
};

static const char *why(int rc, int32_t specific)
{
  static _Thread_local char text[64];
  static const char *const codes[] = { "success", "unknown failure", "invalid arguments", "invalid interface", "unknown interface",
                                       "the interface exists", "the interface is disabled", "not disconnected", "invalid network",
                                       "unknown network", "not supported", "a request is running" };
  if (rc == -EREMOTEIO && specific >= 0 && specific < 12) snprintf(text, sizeof text, "the supplicant says: %s", codes[specific]);
  else if (rc == -EREMOTEIO) snprintf(text, sizeof text, "exception %d", specific);
  else if (rc == -ETIMEDOUT) snprintf(text, sizeof text, "no answer in 5 s");
  else if (rc == -EPIPE) snprintf(text, sizeof text, "the supplicant's process died");
  else snprintf(text, sizeof text, "%s", strerror(rc < 0 ? -rc : rc));
  return text;
}

struct taken { br_supplicant *s; uint32_t handle; bool have; };
static int take_handle(void *ctx, ab_reader *r)
{
  struct taken *t = ctx;
  if (!ab_get_binder(r, &t->handle, NULL)) return r->bad ? -EPROTO : -ENOENT;   // a null binder: not there (yet)
  int rc = ab_acquire(t->s->b, t->handle);         // ours, before the reply's buffer (which holds the call's) goes back
  if (!rc) t->have = true;
  return rc;
}

static int call(br_supplicant *s, uint32_t handle, uint32_t code, ab_parcel *p, const char *what)
{
  int32_t specific = 0;
  int rc = ab_call(s->b, handle, code, p, &specific, NULL, NULL);
  if (rc && what) br_log(0, "wifi: supplicant: %s: %s", what, why(rc, specific));
  return rc;
}

static void emit(br_supplicant *s, int event, uint32_t status, uint32_t code) { if (s->fn) s->fn(s->ctx, event, status, code); }

// A callback of the supplicant (the looper thread; one-way calls, in order).
static void callback(br_supplicant *s, const sup_event *e)
{
  int event = 0;
  uint32_t status = 0, code = 0;
  static const char *const states[] = { "disconnected", "interface disabled", "inactive", "scanning", "authenticating", "associating",
                                        "associated", "4-way handshake", "group handshake", "completed" };
  pthread_mutex_lock(&s->lock);
  const bool show = s->shown < 200;
  if (show) s->shown++;
  switch (e->kind) {
  case CB_STATE_CHANGED: case CB_SUPPLICANT_STATE_CHANGED:
    if (e->frequency > 0) s->frequency = e->frequency;
    if (e->state > s->reached) s->reached = e->state;
    if (e->state == ST_COMPLETED && s->joining) { s->joining = false; s->joined = true; event = BR_SUPPLICANT_JOINED; }
    if (show) br_log(1, "wifi: supplicant: %s%s", e->state >= 0 && e->state <= ST_COMPLETED ? states[e->state] : "state ?",
                     event ? ": joined" : "");
    break;
  case CB_BSS_FREQUENCY_CHANGED: s->frequency = e->frequency; break;
  case CB_DISCONNECTED:
    if (s->joined) { s->joined = false; event = BR_SUPPLICANT_DISCONNECTED; code = e->code; }
    else if (s->joining && s->keyed && s->reached >= ST_FOURWAY_HANDSHAKE && !e->local) {
      // The access point ended the key handshake: with a pre-shared key that is a wrong key.
      s->joining = false; event = BR_SUPPLICANT_JOIN_FAILED; status = OW_JOIN_AUTH; code = e->code;
    }
    if (show) br_log(1, "wifi: supplicant: disconnected (reason %u%s)%s", e->code, e->local ? ", by the phone" : "",
                     event == BR_SUPPLICANT_JOIN_FAILED ? ": the key was refused" : "");
    break;
  case CB_ASSOCIATION_REJECTED:
    if (s->joining && !e->timed_out) {
      s->joining = false; event = BR_SUPPLICANT_JOIN_FAILED; code = e->code;
      status = s->keyed && (e->code == 13 || e->code == 14 || e->code == 15) ? OW_JOIN_AUTH : OW_JOIN_REJECTED;
    }
    if (show) br_log(1, "wifi: supplicant: association rejected (status %u%s)", e->code, e->timed_out ? ", timed out: it tries again" : "");
    break;
  case CB_AUTHENTICATION_TIMEOUT:
    if (s->joining && s->keyed && s->reached >= ST_FOURWAY_HANDSHAKE) { s->joining = false; event = BR_SUPPLICANT_JOIN_FAILED; status = OW_JOIN_AUTH; code = 15; }
    if (show) br_log(1, "wifi: supplicant: authentication timed out");
    break;
  case CB_NETWORK_NOT_FOUND:
    if (s->joining && ++s->not_found >= 2) { s->joining = false; event = BR_SUPPLICANT_JOIN_FAILED; status = OW_JOIN_NO_NETWORK; }
    if (show) br_log(1, "wifi: supplicant: the network was not found in its scan");
    break;
  default: break;
  }
  pthread_mutex_unlock(&s->lock);
  if (event) emit(s, event, status, code);
}

static int sup_incoming(void *ctx, uint64_t object, uint32_t code, uint32_t flags, ab_reader *r, ab_parcel *reply)
{
  br_supplicant *s = ctx;
  if (object != CALLBACK_OBJECT) return AB_UNKNOWN_TRANSACTION;
  if (ab_standard(code, SUP_STA_CALLBACK, SUP_VERSION, SUP_HASH, reply)) return 0;
  if (code > 0x00ffff00u) return AB_UNKNOWN_TRANSACTION;       // dump, shell command and the like
  if (!ab_get_token(r, SUP_STA_CALLBACK)) return AB_BAD_TYPE;
  sup_event e;
  sup_decode_callback(code, r, &e);
  if (e.kind) callback(s, &e);
  if (!(flags & AB_ONE_WAY)) ab_put_ok(reply);
  return 0;
}

static void sup_death(void *ctx, uint64_t cookie)
{
  br_supplicant *s = ctx;
  if (cookie != DEATH_COOKIE) return;
  pthread_mutex_lock(&s->lock);
  s->dead = true;
  s->joining = s->joined = false;
  pthread_mutex_unlock(&s->lock);
  br_log(0, "wifi: supplicant: its process died (init starts it again on the next lookup)");
  emit(s, BR_SUPPLICANT_DIED, 0, 0);
}

struct numbers { int32_t *out; unsigned max, count; };
static int take_ints(void *ctx, ab_reader *r)          // int[]
{
  struct numbers *n = ctx;
  int32_t count = ab_get_i32(r);
  if (r->bad || count < 0) return count == -1 ? 0 : -EPROTO;
  for (int32_t i = 0; i < count; i++) {
    int32_t v = ab_get_i32(r);
    if (r->bad) return -EPROTO;
    if (n->count < n->max) n->out[n->count++] = v;
  }
  return 0;
}
static int take_int(void *ctx, ab_reader *r) { *(int32_t *)ctx = ab_get_i32(r); return r->bad ? -EPROTO : 0; }
static int take_mac(void *ctx, ab_reader *r)
{
  const uint8_t *bytes;
  size_t n;
  if (!ab_get_bytes(r, &bytes, &n) || n != 6) return -EPROTO;
  memcpy(ctx, bytes, 6);
  return 0;
}
static int take_poll(void *ctx, ab_reader *r)          // SignalPollResult[]: linkId, currentRssiDbm, txBitrateMbps, rxBitrateMbps, frequencyMhz
{
  int32_t *out = ctx;
  if (ab_get_i32(r) < 1 || r->bad) return -ENOENT;
  if (!ab_get_parcelable(r)) return -EPROTO;
  for (int i = 0; i < 5; i++) out[i] = ab_get_i32(r);
  return r->bad ? -EPROTO : 0;
}

static void remove_networks(br_supplicant *s)
{
  ab_parcel p;
  int32_t ids[32];
  struct numbers n = { ids, 32, 0 };
  int32_t specific;
  sup_parcel_empty(&p, SUP_STA_IFACE);
  if (ab_call(s->b, s->iface, STA_LIST_NETWORKS, &p, &specific, take_ints, &n)) return;
  for (unsigned i = 0; i < n.count; i++) {
    sup_parcel_i32(&p, SUP_STA_IFACE, ids[i]);
    (void)call(s, s->iface, STA_REMOVE_NETWORK, &p, "removeNetwork");
  }
  if (n.count) br_log(1, "wifi: supplicant: %u network(s) of One UI's removed from it (the framework adds its own again when it starts)", n.count);
}

// The service, the interface, the callback. fn NULL: a look only (the tool beside One UI): nothing is added,
// registered or removed.
static int connect_service(br_supplicant *s, unsigned wait_ms)
{
  ab_parcel p;
  struct taken t = { .s = s };
  int32_t specific = 0;
  int rc = -ENOENT;
  for (unsigned waited = 0;; waited += 200) {
    sup_parcel_get_service(&p, SUP_SERVICE);
    rc = ab_call(s->b, 0, SM_GET_SERVICE, &p, &specific, take_handle, &t);
    if (rc != -ENOENT || waited >= wait_ms) break;
    usleep(200000);                                   // init is starting it
  }
  if (rc) {
    br_log(0, "wifi: supplicant: no %s from the service manager: %s%s", SUP_SERVICE, why(rc, specific),
           rc == -ENOENT ? " (not registered, or SELinux denies find to this domain)" : "");
    return rc;
  }
  s->service = t.handle;
  s->have_service = true;
  (void)ab_watch(s->b, s->service, DEATH_COOKIE);
  t = (struct taken){ .s = s };
  sup_parcel_string(&p, SUP_ISUPPLICANT, s->ifname);
  rc = ab_call(s->b, s->service, SUP_GET_STA_INTERFACE, &p, &specific, take_handle, &t);
  if (rc && s->fn) {
    // No station interface of that name: the supplicant started without its client (One UI's framework adds it).
    t = (struct taken){ .s = s };
    sup_parcel_string(&p, SUP_ISUPPLICANT, s->ifname);
    int32_t add_specific = 0;
    rc = ab_call(s->b, s->service, SUP_ADD_STA_INTERFACE, &p, &add_specific, take_handle, &t);
    if (!rc) { s->added_iface = true; br_log(1, "wifi: supplicant: it had no %s: added (removed again at the return)", s->ifname); }
    else specific = add_specific;
  }
  if (rc) { br_log(0, "wifi: supplicant: no station interface %s: %s", s->ifname, why(rc, specific)); return rc; }
  s->iface = t.handle;
  s->have_iface = true;
  if (!s->fn) return 0;
  sup_parcel_callback(&p, CALLBACK_OBJECT);
  rc = call(s, s->iface, STA_REGISTER_CALLBACK, &p, "registerCallback (can the supplicant call into this domain? look for avc denials "
                                                    "with scontext=...hal_wifi_supplicant in dmesg)");
  if (rc) return rc;
  sup_parcel_empty(&p, SUP_STA_IFACE);
  (void)call(s, s->iface, STA_DISCONNECT, &p, NULL);
  remove_networks(s);
  pthread_mutex_lock(&s->lock);
  s->dead = false;
  pthread_mutex_unlock(&s->lock);
  return 0;
}

static void drop_handles(br_supplicant *s)
{
  if (s->have_network) ab_release(s->b, s->network);
  if (s->have_iface) ab_release(s->b, s->iface);
  if (s->have_service) ab_release(s->b, s->service);
  s->have_network = s->have_iface = s->have_service = false;
}

br_supplicant *br_supplicant_open(const char *ifname, unsigned wait_ms, br_supplicant_event_fn fn, void *ctx)
{
  br_supplicant *s = calloc(1, sizeof *s);
  if (!s) return NULL;
  snprintf(s->ifname, sizeof s->ifname, "%s", ifname && *ifname ? ifname : "wlan0");
  s->fn = fn;
  s->ctx = ctx;
  s->dead = true;
  pthread_mutex_init(&s->lock, NULL);
  s->b = ab_open(NULL, sup_incoming, sup_death, s);
  if (!s->b) {
    br_log(0, "wifi: supplicant: the binder device: %s", strerror(errno));
    pthread_mutex_destroy(&s->lock);
    free(s);
    return NULL;
  }
  if (connect_service(s, wait_ms)) { drop_handles(s); ab_close(s->b); pthread_mutex_destroy(&s->lock); free(s); return NULL; }
  return s;
}

static bool is_dead(br_supplicant *s)
{
  pthread_mutex_lock(&s->lock);
  bool dead = s->dead;
  pthread_mutex_unlock(&s->lock);
  return dead;
}

static void forget_network(br_supplicant *s)
{
  ab_parcel p;
  if (!s->have_network) return;
  sup_parcel_i32(&p, SUP_STA_IFACE, s->network_id);
  (void)call(s, s->iface, STA_REMOVE_NETWORK, &p, NULL);
  ab_release(s->b, s->network);
  s->have_network = false;
}

int br_supplicant_leave(br_supplicant *s)
{
  ab_parcel p;
  pthread_mutex_lock(&s->lock);
  s->joining = s->joined = false;
  bool dead = s->dead;
  pthread_mutex_unlock(&s->lock);
  if (dead || !s->have_iface) return -ENODEV;
  sup_parcel_empty(&p, SUP_STA_IFACE);
  int rc = call(s, s->iface, STA_DISCONNECT, &p, NULL);
  forget_network(s);
  return rc;
}

int br_supplicant_join(br_supplicant *s, const struct ow_join *join, const uint8_t *ies, size_t ie_length)
{
  ab_parcel p;
  br_wifi_security seen;
  br_wifi_plan plan;
  int rc;
  if (is_dead(s)) {                                    // it died: init starts it again on this lookup
    drop_handles(s);
    s->added_iface = false;
    if ((rc = connect_service(s, 5000))) return rc;
  }
  if (ies) br_wifi_security_parse(ies, ie_length, &seen);
  if ((rc = br_wifi_plan_join(join, ies ? &seen : NULL, &plan))) {
    br_log(0, "wifi: join: the network is WPA3 only and iOS gave a WPA2 key, not the password: not joined");
    return rc;
  }
  (void)br_supplicant_leave(s);                        // one network at a time
  struct taken t = { .s = s };
  int32_t specific = 0;
  sup_parcel_empty(&p, SUP_STA_IFACE);
  rc = ab_call(s->b, s->iface, STA_ADD_NETWORK, &p, &specific, take_handle, &t);
  if (rc) { br_log(0, "wifi: supplicant: addNetwork: %s", why(rc, specific)); return rc; }
  s->network = t.handle;
  s->have_network = true;
  sup_parcel_empty(&p, SUP_STA_NETWORK);
  rc = ab_call(s->b, s->network, NET_GET_ID, &p, &specific, take_int, &s->network_id);
  const uint32_t n = s->network;
#define STEP(code, what) do { if (!rc) rc = call(s, n, code, &p, what); } while (0)
  if (!rc) { sup_parcel_bytes(&p, SUP_STA_NETWORK, join->ssid, join->ssid_length); STEP(NET_SET_SSID, "setSsid"); }
  if (!rc) { sup_parcel_i32(&p, SUP_STA_NETWORK, (int32_t)plan.key_mgmt); STEP(NET_SET_KEY_MGMT, "setKeyMgmt"); }
  if (!rc && plan.proto) { sup_parcel_i32(&p, SUP_STA_NETWORK, (int32_t)plan.proto); STEP(NET_SET_PROTO, "setProto"); }
  if (!rc && plan.pairwise) { sup_parcel_i32(&p, SUP_STA_NETWORK, (int32_t)plan.pairwise); STEP(NET_SET_PAIRWISE_CIPHER, "setPairwiseCipher"); }
  if (!rc && plan.group) { sup_parcel_i32(&p, SUP_STA_NETWORK, (int32_t)plan.group); STEP(NET_SET_GROUP_CIPHER, "setGroupCipher"); }
  if (!rc && plan.set_pmf) { sup_parcel_i32(&p, SUP_STA_NETWORK, plan.require_pmf); STEP(NET_SET_REQUIRE_PMF, "setRequirePmf"); }
  if (!rc && plan.scan_ssid) { sup_parcel_i32(&p, SUP_STA_NETWORK, 1); STEP(NET_SET_SCAN_SSID, "setScanSsid"); }
  if (!rc && (plan.psk_passphrase || plan.sae_password)) {
    char text[64];
    memcpy(text, join->key, join->key_length);
    text[join->key_length] = 0;
    sup_parcel_string(&p, SUP_STA_NETWORK, text);
    memset(text, 0, sizeof text);
    STEP(plan.sae_password ? NET_SET_SAE_PASSWORD : NET_SET_PSK_PASSPHRASE, plan.sae_password ? "setSaePassword" : "setPskPassphrase");
    ab_wipe(&p);
  }
  if (!rc && plan.sae_password) {                      // hash-to-element where the access point asks for it; an older supplicant has no such call
    sup_parcel_i32(&p, SUP_STA_NETWORK, 1 /* H2E_OPTIONAL */);
    (void)call(s, n, NET_SET_SAE_H2E_MODE, &p, NULL);
  }
  if (!rc && plan.psk_raw) {
    sup_parcel_bytes(&p, SUP_STA_NETWORK, join->key, 32);
    STEP(NET_SET_PSK, "setPsk");
    ab_wipe(&p);
  }
#undef STEP
  if (!rc) {
    pthread_mutex_lock(&s->lock);
    s->joining = true;
    s->joined = false;
    s->keyed = plan.key_mgmt != KM_NONE && plan.key_mgmt != KM_OWE;
    s->reached = 0;
    s->not_found = 0;
    pthread_mutex_unlock(&s->lock);
    sup_parcel_empty(&p, SUP_STA_NETWORK);
    rc = call(s, n, NET_SELECT, &p, "select");
  }
  if (rc) {
    pthread_mutex_lock(&s->lock);
    s->joining = false;
    pthread_mutex_unlock(&s->lock);
    forget_network(s);
    return rc;
  }
  br_log(1, "wifi: join: network %d in the supplicant, %s%s: selected", s->network_id, plan.kind, plan.scan_ssid ? ", not in the last scan" : "");
  return 0;
}

uint32_t br_supplicant_frequency(br_supplicant *s)
{
  pthread_mutex_lock(&s->lock);
  int32_t f = s->frequency;
  pthread_mutex_unlock(&s->lock);
  return f > 0 ? (uint32_t)f : 0;
}

int br_supplicant_status(br_supplicant *s, uint8_t mac[6], int32_t *networks, unsigned *nnetworks, int32_t poll[5])
{
  ab_parcel p;
  int32_t specific;
  struct numbers n = { networks, *nnetworks, 0 };
  sup_parcel_empty(&p, SUP_STA_IFACE);
  int rc = ab_call(s->b, s->iface, STA_GET_MAC_ADDRESS, &p, &specific, take_mac, mac);
  if (rc) { br_log(0, "wifi: supplicant: getMacAddress: %s", why(rc, specific)); return rc; }
  sup_parcel_empty(&p, SUP_STA_IFACE);
  rc = ab_call(s->b, s->iface, STA_LIST_NETWORKS, &p, &specific, take_ints, &n);
  if (rc) { br_log(0, "wifi: supplicant: listNetworks: %s", why(rc, specific)); return rc; }
  *nnetworks = n.count;
  memset(poll, 0, 5 * sizeof *poll);
  sup_parcel_empty(&p, SUP_STA_IFACE);
  (void)ab_call(s->b, s->iface, STA_GET_SIGNAL_POLL_RESULTS, &p, &specific, take_poll, poll);   // fails while not joined
  return 0;
}

void br_supplicant_close(br_supplicant *s)
{
  if (!s) return;
  if (s->fn && !is_dead(s)) {
    (void)br_supplicant_leave(s);
    if (s->added_iface) {                              // IfaceInfo: type (STA 0), name
      ab_parcel p;
      sup_parcel_empty(&p, SUP_ISUPPLICANT);
      ab_put_i32(&p, 1);
      size_t at = p.size;
      ab_put_i32(&p, 0);
      ab_put_i32(&p, 0);
      ab_put_string(&p, s->ifname);
      int32_t size = (int32_t)(p.size - at);
      memcpy(p.data + at, &size, 4);
      (void)call(s, s->service, SUP_REMOVE_INTERFACE, &p, "removeInterface");
    }
  }
  s->fn = NULL;
  drop_handles(s);
  ab_close(s->b);
  pthread_mutex_destroy(&s->lock);
  free(s);
}
