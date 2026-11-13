// SPDX-License-Identifier: MIT
// Bluetooth bridge: a stand-in controller (bluetooth=sim) for a boot with no phone, the PC's qemu-aarch64 boot
// above all. No radio and no HAL: every HCI command gets the answer a Bluetooth 5.4 controller gives (a Command
// Complete with plausible return parameters, or a Command Status for the commands that answer that way), vendor
// commands (OGF 0x3f) get "Unknown HCI Command" as a Qualcomm controller gives Broadcom's, and an LE scan that is
// turned on hears one made-up device (an advertising report named "OneiOS sim"), so that the whole path (bluetoothd,
// the guest's Skywalk pipe and H4 core, the bridge channel, this file) can be seen working in xnuhost's log.
// ACL, SCO and ISO data are dropped: nothing connects.
#include "bridges/bridges.h"
#include <errno.h>
#include <string.h>

typedef struct {
  br_hci *hci;
  uint8_t address[6];                   // display order
  uint64_t commands;
  unsigned said;                        // commands logged (the first SIM_SAY)
} sim_bt;

#define SIM_SAY 64u

static uint8_t sim_address[6] = { 0x02, 0x0e, 0x10, 0x05, 0x51, 0x4d };    // locally administered

void br_hci_sim_set_address(const uint8_t address[6]) { memcpy(sim_address, address, 6); }

// Return parameters after the status byte, by opcode: their length, and the bytes that are not zero.
static size_t sim_return(sim_bt *s, uint16_t op, const uint8_t *param, size_t plen, uint8_t *r)
{
  switch (op) {
  case 0x1001:                                                  // Read Local Version Information
    r[0] = 0x0d; r[1] = 0x00; r[2] = 0x00; r[3] = 0x0d; r[4] = 0x1d; r[5] = 0x00; r[6] = 0x00; r[7] = 0x00;   // 5.4, Qualcomm
    return 8;
  case 0x1002:                                                  // Read Local Supported Commands: all of them
    memset(r, 0xff, 64);
    return 64;
  case 0x1003:                                                  // Read Local Supported Features (BR/EDR and LE)
    memcpy(r, "\xff\xfe\x8f\xfe\xd8\x3f\x5b\x87", 8);
    return 8;
  case 0x1004:                                                  // Read Local Extended Features: page, max page, 8 bytes
    r[0] = plen ? param[0] : 0;
    r[1] = 2;
    if (r[0] == 0) memcpy(r + 2, "\xff\xfe\x8f\xfe\xd8\x3f\x5b\x87", 8);
    else if (r[0] == 1) r[2] = 0x0f;
    return 10;
  case 0x1005:                                                  // Read Buffer Size: ACL 1021 x 8, SCO 64 x 8
    r[0] = 0xfd; r[1] = 0x03; r[2] = 64; r[3] = 8; r[5] = 8;
    return 7;
  case 0x1009:                                                  // Read BD_ADDR (little-endian)
    for (unsigned i = 0; i < 6; i++) r[i] = s->address[5 - i];
    return 6;
  case 0x100b: return 2;                                        // Read Local Supported Codecs: none
  case 0x0c14: memcpy(r, "OneiOS sim", 10); return 248;         // Read Local Name
  case 0x0c23: return 3;                                        // Read Class of Device
  case 0x0c25: r[0] = 0x60; return 2;                           // Read Voice Setting
  case 0x0c38: r[0] = 1; return 1;                              // Read Number of Supported IAC
  case 0x0c39: r[0] = 1; r[1] = 0x33; r[2] = 0x8b; r[3] = 0x9e; return 4;   // Read Current IAC LAP: GIAC
  case 0x0c44: case 0x0c45: case 0x0c46: case 0x0c48: return 1; // inquiry scan type, inquiry mode, page scan type, AFH mode
  case 0x0c58: return 1;                                        // Read Inquiry Response Transmit Power Level
  case 0x0c6d: r[0] = 1; return 2;                              // Read LE Host Support
  case 0x0c79: return 1;                                        // Read Secure Connections Host Support
  case 0x1006: case 0x1007: case 0x0c0d: case 0x0c1b: case 0x0c1d: case 0x0c27: case 0x0c29: case 0x0c2d: return 2;
  case 0x1408: return 2;                                        // Read Encryption Key Size: handle
  case 0x2002: r[0] = 0xfb; r[2] = 8; return 3;                 // LE Read Buffer Size: 251 x 8
  case 0x2060: r[0] = 0xfb; r[2] = 8; return 6;                 // LE Read Buffer Size v2 (no ISO buffers)
  case 0x2003: memcpy(r, "\xff\x79\x00\x00\x00\x00\x00\x00", 8); return 8;   // LE features: through extended advertising
  case 0x2007: return 1;                                        // LE Read Advertising Physical Channel Tx Power
  case 0x200f: r[0] = 16; return 1;                             // LE Read Filter Accept List Size
  case 0x2017: return 16;                                       // LE Encrypt
  case 0x2018:                                                  // LE Rand
    for (unsigned i = 0; i < 8; i++) r[i] = (uint8_t)(0x5a ^ (s->commands * 31 + i * 17));
    return 8;
  case 0x201c: memset(r, 0xff, 5); r[5] = 0x03; return 8;       // LE Read Supported States
  case 0x2023: r[0] = 0xfb; r[2] = 0x48; r[3] = 0x08; return 4; // LE Read Suggested Default Data Length: 251, 2120
  case 0x202a: r[0] = 16; return 1;                             // LE Read Resolving List Size
  case 0x202b: case 0x202c: return 6;                           // LE Read Peer/Local Resolvable Address
  case 0x202f: r[0] = 0xfb; r[2] = 0x48; r[3] = 0x08; r[4] = 0xfb; r[6] = 0x48; r[7] = 0x08; return 8;     // LE Read Maximum Data Length
  case 0x2030: return 4;                                        // LE Read PHY: handle, 1M, 1M
  case 0x2036: return 1;                                        // LE Set Extended Advertising Parameters: Tx power
  case 0x203a: r[0] = 0xfb; r[1] = 0x00; return 2;              // LE Read Maximum Advertising Data Length: 251
  case 0x203b: r[0] = 6; return 1;                              // LE Read Number of Supported Advertising Sets
  case 0x204b: r[0] = 0xf0; r[1] = 0x14; return 2;              // LE Read Transmit Power: -16..20 dBm
  case 0x204c: return 2;                                        // LE Read RF Path Compensation
  }
  return 0;
}

// The commands a controller answers with a Command Status (their result comes later as its own event).
static bool sim_status_only(uint16_t op)
{
  switch (op) {
  case 0x0401: case 0x0405: case 0x0406: case 0x0409: case 0x040b: case 0x0419: case 0x041b: case 0x041c: case 0x041d:
  case 0x200d: case 0x2013: case 0x2016: case 0x2019: case 0x2025: case 0x2026: case 0x2032: case 0x2043: case 0x205e:
    return true;
  }
  return false;
}

static void sim_event(sim_bt *s, const uint8_t *e, size_t n) { br_hci_deliver(s->hci, 4, e, n); }

// An LE scan turned on: one report of a made-up device (legacy or extended, as the scan asked).
static void sim_heard(sim_bt *s, bool extended)
{
  static const uint8_t name[] = "OneiOS sim";
  uint8_t data[2 + sizeof name - 1 + 3];
  size_t dn = 0;
  data[dn++] = 2; data[dn++] = 0x01; data[dn++] = 0x06;                    // flags: LE general discoverable
  data[dn++] = (uint8_t)sizeof name; data[dn++] = 0x09;                    // complete local name
  memcpy(data + dn, name, sizeof name - 1); dn += sizeof name - 1;
  uint8_t e[64];
  size_t n = 0;
  e[n++] = 0x3e;
  n++;                                                                       // length, below
  if (!extended) {
    e[n++] = 0x02; e[n++] = 1; e[n++] = 0x00; e[n++] = 0x01;               // advertising report, 1, ADV_IND, random
    memcpy(e + n, "\x11\x22\x33\x44\x55\xc6", 6); n += 6;
    e[n++] = (uint8_t)dn; memcpy(e + n, data, dn); n += dn;
    e[n++] = 0xc4;                                                           // RSSI -60
  } else {
    e[n++] = 0x0d; e[n++] = 1; e[n++] = 0x13; e[n++] = 0x00; e[n++] = 0x01; // extended report, 1, legacy ADV_IND, random
    memcpy(e + n, "\x11\x22\x33\x44\x55\xc6", 6); n += 6;
    e[n++] = 1; e[n++] = 0; e[n++] = 0xff; e[n++] = 0x7f; e[n++] = 0xc4;     // 1M, no secondary, no SID, no Tx power, RSSI
    e[n++] = 0; e[n++] = 0; e[n++] = 0; memset(e + n, 0, 6); n += 6;         // interval, direct address type and address
    e[n++] = (uint8_t)dn; memcpy(e + n, data, dn); n += dn;
  }
  e[1] = (uint8_t)(n - 2);
  sim_event(s, e, n);
}

static int sim_open(void *ctx)
{
  sim_bt *s = ctx;
  memcpy(s->address, sim_address, 6);
  s->said = 0;
  br_log(1, "bluetooth: sim: the stand-in controller is on, address %02x:%02x:%02x:%02x:%02x:%02x", s->address[0], s->address[1],
         s->address[2], s->address[3], s->address[4], s->address[5]);
  br_hci_set_state(s->hci, 1);
  return 0;
}

static int sim_send(void *ctx, uint8_t type, const uint8_t *p, size_t length)
{
  sim_bt *s = ctx;
  if (type != 1) return 0;                                                  // data: nothing is connected
  uint16_t op = (uint16_t)(p[0] | p[1] << 8);
  const uint8_t *param = p + 3;
  size_t plen = length - 3;
  s->commands++;
  uint8_t e[3 + 3 + 1 + 255] = { 0 };
  uint8_t status = (op >> 10) == 0x3f ? 0x01 : 0x00;                        // vendor commands: Unknown HCI Command
  if (s->said < SIM_SAY) {
    s->said++;
    br_log(2, "bluetooth: sim: command 0x%04x, %zu parameter bytes", op, plen);
  }
  if (status == 0 && sim_status_only(op)) {
    e[0] = 0x0f; e[1] = 4; e[2] = 0; e[3] = 1; e[4] = p[0]; e[5] = p[1];
    sim_event(s, e, 6);
    if (op == 0x0401) { uint8_t done[3] = { 0x01, 1, 0 }; sim_event(s, done, 3); }    // Inquiry Complete: nobody there
    return 0;
  }
  size_t rn = status ? 0 : sim_return(s, op, param, plen, e + 6);
  e[0] = 0x0e; e[1] = (uint8_t)(4 + rn); e[2] = 1; e[3] = p[0]; e[4] = p[1]; e[5] = status;
  sim_event(s, e, 6 + rn);
  if ((op == 0x200c || op == 0x2042) && plen >= 1 && param[0] == 1) sim_heard(s, op == 0x2042);
  return 0;
}

static void sim_close(void *ctx)
{
  sim_bt *s = ctx;
  br_log(1, "bluetooth: sim: the stand-in controller is off after %llu commands", (unsigned long long)s->commands);
}

static const br_hci_backend sim_backend = { .open = sim_open, .send = sim_send, .close = sim_close };

// One stand-in per process (xnuhost makes one controller); a later call starts it over.
static sim_bt the_sim;

br_hci *br_hci_open_sim(br_hci_rx_fn rx, br_hci_state_fn state, void *ctx)
{
  sim_bt *s = &the_sim;
  memset(s, 0, sizeof *s);
  s->hci = br_hci_create(&sim_backend, s, rx, state, ctx);
  return s->hci;
}
