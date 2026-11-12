// SPDX-License-Identifier: MIT
// Bluetooth bridge, the part every back end shares: one HCI controller with packet checks and a
// state the guest's transport (drivers/bluetooth) follows. The back end for the phone is
// hci_hidl.c (the vendor's Bluetooth HAL, free once the Bluetooth app is gone with zygote).
#include "bridges/bridges.h"
#include <ctype.h>
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct br_hci {
  const br_hci_backend *backend;
  void *backend_ctx;
  br_hci_rx_fn rx;
  br_hci_state_fn state_fn;
  void *ctx;
  pthread_mutex_t lock;
  int state;
  bool open;
  uint64_t sent, received, dropped;
  uint64_t reports;               // advertising and inquiry reports: devices the controller heard
};

bool br_hci_packet_ok(uint8_t type, const uint8_t *p, size_t length)
{
  switch (type) {
  case 1: return length >= 3 && length == 3u + p[2];                                 // opcode, length
  case 2: return length >= 4 && length == 4u + (unsigned)(p[2] | p[3] << 8);         // handle, length
  case 3: return length >= 3 && length == 3u + p[2];
  case 4: return length >= 2 && length == 2u + p[1];                                 // code, length
  case 5: return length >= 4 && length == 4u + (unsigned)((p[2] | p[3] << 8) & 0x3fff);
  }
  return false;
}

br_hci *br_hci_create(const br_hci_backend *backend, void *backend_ctx, br_hci_rx_fn rx, br_hci_state_fn state, void *ctx)
{
  br_hci *h = calloc(1, sizeof *h);
  if (!h) return NULL;
  h->backend = backend; h->backend_ctx = backend_ctx; h->rx = rx; h->state_fn = state; h->ctx = ctx;
  pthread_mutex_init(&h->lock, NULL);
  return h;
}

int br_hci_open(br_hci *h)
{
  pthread_mutex_lock(&h->lock);
  bool was = h->open;
  h->open = true;
  pthread_mutex_unlock(&h->lock);
  if (was) return 0;
  int rc = h->backend->open(h->backend_ctx);
  if (rc) { pthread_mutex_lock(&h->lock); h->open = false; pthread_mutex_unlock(&h->lock); br_hci_set_state(h, 2); }
  return rc;
}

int br_hci_send(br_hci *h, uint8_t type, const uint8_t *packet, size_t length)
{
  if (type == 4 || !br_hci_packet_ok(type, packet, length)) { h->dropped++; return -EINVAL; }
  pthread_mutex_lock(&h->lock);
  bool ready = h->open && h->state == 1;
  pthread_mutex_unlock(&h->lock);
  if (!ready) { h->dropped++; return -ENOTCONN; }
  h->sent++;
  return h->backend->send(h->backend_ctx, type, packet, length);
}

void br_hci_close(br_hci *h)
{
  pthread_mutex_lock(&h->lock);
  bool was = h->open;
  h->open = false;
  pthread_mutex_unlock(&h->lock);
  if (was) h->backend->close(h->backend_ctx);
  br_hci_set_state(h, 0);
}

void br_hci_destroy(br_hci *h)
{
  if (!h) return;
  br_hci_close(h);
  free(h);
}

void br_hci_deliver(br_hci *h, uint8_t type, const uint8_t *packet, size_t length)
{
  if (!br_hci_packet_ok(type, packet, length)) { h->dropped++; return; }
  h->received++;
  // A device nearby, heard by the controller: an LE advertising report (LE Meta 0x3e, subevents 0x02 and 0x0d) or an
  // inquiry result (0x02, 0x22, 0x2f). One line for the first and for each power of two after: a scan that finds
  // devices can be read in the log.
  if (type == 4 && length >= 3 && ((packet[0] == 0x3e && (packet[2] == 0x02 || packet[2] == 0x0d)) || packet[0] == 0x02 || packet[0] == 0x22 ||
                                   packet[0] == 0x2f)) {
    uint64_t n = ++h->reports;
    if (!(n & (n - 1))) br_log(1, "bluetooth: the controller hears devices: %llu report(s) so far (%s)", (unsigned long long)n,
                               packet[0] == 0x3e ? "LE advertising" : "inquiry result");
  }
  if (h->rx) h->rx(h->ctx, type, packet, length);
}

void br_hci_set_state(br_hci *h, int state)
{
  pthread_mutex_lock(&h->lock);
  bool changed = h->state != state;
  h->state = state;
  pthread_mutex_unlock(&h->lock);
  if (changed && h->state_fn) h->state_fn(h->ctx, state);
}

int br_hci_state(br_hci *h)
{
  pthread_mutex_lock(&h->lock);
  int state = h->state;
  pthread_mutex_unlock(&h->lock);
  return state;
}

int br_bt_address(const char *path, uint8_t out[6])
{
  uint8_t raw[64];
  FILE *f = fopen(path, "rb");
  if (!f) return -1;
  size_t n = fread(raw, 1, sizeof raw - 1, f);
  fclose(f);
  raw[n] = 0;
  // Text: twelve hex digits with or without separators, ended by the file's end or a NUL (the S24 Ultra's
  // /mnt/vendor/efs/bluetooth/bt_addr is 18 bytes: "XX:XX:XX:XX:XX:XX" and a NUL). Else the first six bytes as they are.
  uint8_t hex[12];
  unsigned digits = 0;
  bool text = true;
  size_t end = n;
  if (n != 6) for (size_t i = 0; i < n; i++) if (!raw[i]) { end = i; break; }
  for (size_t i = 0; i < end && text; i++) {
    int c = raw[i];
    if (isxdigit(c)) { if (digits < 12) hex[digits] = (uint8_t)(isdigit(c) ? c - '0' : (tolower(c) - 'a' + 10)); digits++; }
    else if (c != ':' && c != '-' && c != '\n' && c != '\r' && c != ' ') text = false;
  }
  if (text && digits == 12) for (unsigned i = 0; i < 6; i++) out[i] = (uint8_t)(hex[2 * i] << 4 | hex[2 * i + 1]);
  else if (n == 6) memcpy(out, raw, 6);
  else return -1;
  static const uint8_t zero[6];
  return memcmp(out, zero, 6) ? 0 : -1;
}
