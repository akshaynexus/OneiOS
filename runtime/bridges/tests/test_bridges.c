// SPDX-License-Identifier: MIT
// Host tests of the bridges: no phone, no Android, no Apple files. The virtio devices run on
// xnuhost's real transport (runtime/xnuhost/src/virtio.c) against a guest made of plain memory.
#define _GNU_SOURCE
#include "bridges/devices.h"
#include "oneios_bridge_proto.h"
#include "../bt/hidl_parcel.h"
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

static int failures, checks;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

// ---- what xnuhost supplies around virtio.c -------------------------------------------------------
static unsigned irq_raised;
void xh_gic_set_spi(unsigned intid, bool level) { (void)intid; if (level) irq_raised++; }
int xh_mmio_add(const xh_mmio_region *region) { (void)region; return 0; }
void *xh_ram(uint64_t pa, uint64_t len) { (void)len; return (void *)(uintptr_t)pa; }      // guest "physical" = host pointer

// ---- a guest's side of one virtio-mmio device ----------------------------------------------------
#define QN 16u
typedef struct { uint64_t addr; uint32_t len; uint16_t flags, next; } vdesc;
typedef struct {
  vdesc desc[QN];
  struct { uint16_t flags, idx, ring[QN]; } avail;
  struct { uint16_t flags, idx; struct { uint32_t id, len; } ring[QN]; } used;
  uint16_t next_desc, seen_used;
} gqueue;
typedef struct { xh_virtio *dev; gqueue q[4]; } guest;

static void w32(guest *g, uint64_t off, uint32_t v) { xh_virtio_mmio_write(g->dev, off, 4, v); }
static uint32_t r32(guest *g, uint64_t off) { return (uint32_t)xh_virtio_mmio_read(g->dev, off, 4); }

static void guest_up(guest *g, xh_virtio *dev, unsigned nqueues, uint32_t low_features)
{
  memset(g, 0, sizeof *g);
  g->dev = dev;
  w32(g, 0x70, 0); w32(g, 0x70, 1); w32(g, 0x70, 3);
  w32(g, 0x24, 0); w32(g, 0x20, low_features); w32(g, 0x24, 1); w32(g, 0x20, 1);
  w32(g, 0x70, 11);
  CHECK(r32(g, 0x70) & 8);
  for (unsigned q = 0; q < nqueues; q++) {
    w32(g, 0x30, q); w32(g, 0x38, QN);
    uint64_t d = (uint64_t)(uintptr_t)g->q[q].desc, a = (uint64_t)(uintptr_t)&g->q[q].avail, u = (uint64_t)(uintptr_t)&g->q[q].used;
    w32(g, 0x80, (uint32_t)d); w32(g, 0x84, (uint32_t)(d >> 32));
    w32(g, 0x90, (uint32_t)a); w32(g, 0x94, (uint32_t)(a >> 32));
    w32(g, 0xa0, (uint32_t)u); w32(g, 0xa4, (uint32_t)(u >> 32));
    w32(g, 0x44, 1);
  }
  w32(g, 0x70, 15);
}

// Posts a chain of `n` buffers (writable[i] says which the device fills) and kicks the queue.
static uint16_t post(guest *g, unsigned q, void **bufs, const uint32_t *lens, const bool *writable, unsigned n)
{
  gqueue *gq = &g->q[q];
  uint16_t head = gq->next_desc;
  for (unsigned i = 0; i < n; i++) {
    uint16_t at = (uint16_t)((gq->next_desc + i) % QN);
    gq->desc[at] = (vdesc){ (uint64_t)(uintptr_t)bufs[i], lens[i], (uint16_t)((writable[i] ? 2 : 0) | (i + 1 < n ? 1 : 0)), (uint16_t)((at + 1) % QN) };
  }
  gq->next_desc = (uint16_t)((gq->next_desc + n) % QN);
  gq->avail.ring[gq->avail.idx % QN] = head;
  gq->avail.idx++;
  w32(g, 0x50, q);
  return head;
}
static uint16_t post1(guest *g, unsigned q, void *buf, uint32_t len, bool writable) { return post(g, q, &buf, &len, &writable, 1); }

// The length of the next used element of q, or -1 when the device has returned nothing new.
static int reap(guest *g, unsigned q)
{
  gqueue *gq = &g->q[q];
  if (gq->used.idx == gq->seen_used) return -1;
  return (int)gq->used.ring[gq->seen_used++ % QN].len;
}

static void wait_for(bool (*done)(void *), void *ctx)
{
  for (unsigned i = 0; i < 2000 && !done(ctx); i++) usleep(1000);
}

// ---- CPU -----------------------------------------------------------------------------------------
static void put_file(const char *root, const char *rel, const char *text)
{
  char path[600], cmd[700];
  snprintf(path, sizeof path, "%s/%s", root, rel);
  char *slash = strrchr(path, '/');
  *slash = 0;
  snprintf(cmd, sizeof cmd, "mkdir -p '%s'", path);
  if (system(cmd)) abort();
  *slash = '/';
  FILE *f = fopen(path, "w");
  if (!f) abort();
  fputs(text, f);
  fclose(f);
}

// A /sys/devices/system/cpu shaped like an SM8650's (made-up frequencies and capacities, the real core kinds):
// cpu0-1 A520, cpu2-6 A720 in two frequency domains, cpu7 X4.
static void make_sm8650(const char *root)
{
  static const struct { unsigned capacity, max, min, policy, midr; const char *table; } cpus[8] = {
    { 325, 2265600, 364800, 0, 0x410fd801, "364800 460800 556800 672000 787200 902400 1017600 1132800 1248000 1344000 1459200 1574400 1689600 1804800 1920000 2035200 2150400 2265600" },
    { 325, 2265600, 364800, 0, 0x410fd801, NULL },
    { 792, 3148800, 499200, 2, 0x411fd811, "499200 614400 729600 844800 960000 1075200 1190400 1286400 1401600 1497600 1612800 1708800 1824000 1920000 2035200 2131200 2188800 2246400 2323200 2380800 2438400 2515200 2572800 2630400 2707200 2764800 2841600 2899200 2956800 3014400 3072000 3148800" },
    { 792, 3148800, 499200, 2, 0x411fd811, NULL }, { 792, 3148800, 499200, 2, 0x411fd811, NULL },
    { 790, 2956800, 499200, 5, 0x411fd811, "499200 2956800" }, { 790, 2956800, 499200, 5, 0x411fd811, NULL },
    { 1024, 3398400, 480000, 7, 0x411fd821, "480000 3398400" },
  };
  for (unsigned i = 0; i < 8; i++) {
    char rel[128], text[512];
    snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/cpu_capacity", i); snprintf(text, sizeof text, "%u\n", cpus[i].capacity); put_file(root, rel, text);
    snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/cpufreq/cpuinfo_max_freq", i); snprintf(text, sizeof text, "%u\n", cpus[i].max); put_file(root, rel, text);
    snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/cpufreq/cpuinfo_min_freq", i); snprintf(text, sizeof text, "%u\n", cpus[i].min); put_file(root, rel, text);
    snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/cpufreq/related_cpus", i); snprintf(text, sizeof text, "%u %u\n", cpus[i].policy, i); put_file(root, rel, text);
    snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/regs/identification/midr_el1", i); snprintf(text, sizeof text, "0x%016x\n", cpus[i].midr); put_file(root, rel, text);
    if (cpus[i].table) { snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/cpufreq/scaling_available_frequencies", i); snprintf(text, sizeof text, "%s \n", cpus[i].table); put_file(root, rel, text); }
    snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/cache/index0/level", i); put_file(root, rel, "1\n");
    snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/cache/index0/type", i); put_file(root, rel, "Data\n");
    snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/cache/index0/size", i); put_file(root, rel, i < 2 ? "32K\n" : "64K\n");
    snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/cache/index2/level", i); put_file(root, rel, "2\n");
    snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/cache/index2/type", i); put_file(root, rel, "Unified\n");
    snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/cache/index2/size", i); put_file(root, rel, i == 7 ? "2048K\n" : i < 2 ? "256K\n" : "512K\n");
  }
}

static void test_cpu(const char *tmp)
{
  char root[400];
  snprintf(root, sizeof root, "%s/sys", tmp);
  make_sm8650(root);
  br_cpu_topology t;
  CHECK(br_cpu_read(root, &t) == 0);
  CHECK(t.ncpu == 8 && t.n_e == 2 && t.n_p == 6);
  CHECK(t.cpu[0].type == 'E' && t.cpu[1].type == 'E' && t.cpu[2].type == 'P' && t.cpu[7].type == 'P');
  CHECK(t.cpu[0].part == 0xd80 && t.cpu[2].part == 0xd81 && t.cpu[7].part == 0xd82);
  CHECK(t.cpu[0].nfreq == 18 && t.cpu[0].freq_khz[0] == 364800 && t.cpu[0].freq_khz[17] == 2265600);
  CHECK(t.cpu[1].nfreq == 2 && t.cpu[1].freq_khz[1] == 2265600);          // no table: the two ends
  CHECK(t.cpu[7].l2_bytes == 2048 * 1024 && t.cpu[0].l1d_bytes == 32 * 1024 && t.cpu[5].policy == 5);

  br_cpu_plan plan;
  CHECK(br_cpu_plan_make(&t, 8, false, &plan) == 0);
  CHECK(plan.n_e == 2 && plan.n_p == 6);
  CHECK(plan.v[0].type == 'E' && plan.v[1].type == 'E' && plan.v[2].type == 'P' && plan.v[7].type == 'P');
  CHECK(plan.v[0].affinity == 0x03 && plan.v[2].affinity == 0xfc);
  CHECK(plan.v[0].cluster_id == 0 && plan.v[7].cluster_id == 1);
  CHECK(plan.v[2].max_khz == 3398400 && plan.v[2].capacity == 1024);      // the P cluster's fastest core
  CHECK(br_cpu_plan_make(&t, 8, true, &plan) == 0);
  CHECK(plan.v[0].affinity == 0x01 && plan.v[1].affinity == 0x02 && plan.v[2].affinity == 0x80 && plan.v[3].affinity == 0x04);
  CHECK(plan.v[2].max_khz == 3398400 && plan.v[3].max_khz == 3148800);
  CHECK(br_cpu_plan_make(&t, 4, false, &plan) == 0 && plan.n_e == 1 && plan.n_p == 3 && plan.v[0].type == 'E' && plan.v[1].type == 'P');
  CHECK(br_cpu_plan_make(&t, 1, false, &plan) == 0 && plan.n_e == 0 && plan.v[0].type == 'P');
  CHECK(br_cpu_plan_make(&t, 9, false, &plan) == -1 && br_cpu_plan_make(&t, 0, false, &plan) == -1);

  CHECK(br_cpu_plan_make(&t, 8, false, &plan) == 0);
  CHECK(br_cpu_util_for(&plan, 2, 3398400) == 1024 && br_cpu_util_for(&plan, 2, 1699200) == 512 && br_cpu_util_for(&plan, 2, 0) == 0);
  CHECK(br_cpu_util_for(&plan, 0, 2265600) == 325);
  br_cpufreq *d = br_cpufreq_create(&plan, NULL);
  CHECK(br_cpufreq_read(d, 0x08, 4) == 18);                                // vCPU 0: the E table
  br_cpufreq_write(d, 0x0c, 4, 17);
  CHECK(br_cpufreq_read(d, 0x10, 4) == 2265600);
  CHECK(br_cpufreq_read(d, 2 * BR_CPUFREQ_WINDOW + 0x08, 4) == 2 && br_cpufreq_read(d, 2 * BR_CPUFREQ_WINDOW + 0x14, 4) == 1);
  br_cpufreq_write(d, 2 * BR_CPUFREQ_WINDOW + 0x04, 4, 3398400);
  CHECK(br_cpufreq_requested(d, 2) == 3398400 && br_cpufreq_read(d, 2 * BR_CPUFREQ_WINDOW, 4) == 3398400);
  br_cpufreq_write(d, 2 * BR_CPUFREQ_WINDOW + 0x04, 4, 480000);            // the lowest entry drops the request
  CHECK(br_cpufreq_requested(d, 2) == 0);
  CHECK(br_cpufreq_read(d, 9 * BR_CPUFREQ_WINDOW + 0x08, 4) == 0);
  br_cpufreq_destroy(d);

  // A host with one kind of core: all P.
  char flat[400];
  snprintf(flat, sizeof flat, "%s/flat", tmp);
  for (unsigned i = 0; i < 4; i++) { char rel[128]; snprintf(rel, sizeof rel, "devices/system/cpu/cpu%u/cpu_capacity", i); put_file(flat, rel, "1024\n"); }
  CHECK(br_cpu_read(flat, &t) == 0 && t.n_p == 4 && t.n_e == 0);
  CHECK(br_cpu_plan_make(&t, 4, false, &plan) == 0 && plan.n_e == 0 && plan.v[0].affinity == 0xf);
  CHECK(br_cpu_read("/nonexistent", &t) == -1);
}

// ---- haptics -------------------------------------------------------------------------------------
typedef struct { unsigned uploads, plays, stops, last_length, last_period, last_magnitude, gain; int next_id; } fake_motor;
static int fm_upload(void *ctx, int id, unsigned length_ms, unsigned period_ms, unsigned magnitude)
{
  fake_motor *m = ctx;
  m->uploads++; m->last_length = length_ms; m->last_period = period_ms; m->last_magnitude = magnitude;
  return id < 0 ? m->next_id++ : id;
}
static int fm_play(void *ctx, int id, bool on) { fake_motor *m = ctx; (void)id; if (on) m->plays++; else m->stops++; return 0; }
static int fm_gain(void *ctx, unsigned gain) { ((fake_motor *)ctx)->gain = gain; return 0; }
static const br_haptic_backend fake_motor_backend = { .upload = fm_upload, .play = fm_play, .gain = fm_gain };

static void test_haptic(void)
{
  unsigned period, magnitude;
  br_haptic_map(1000, 0, &period, &magnitude);
  CHECK(period == 6 && magnitude == 255);
  br_haptic_map(0, 0, &period, &magnitude);
  CHECK(magnitude == 0);
  br_haptic_map(1, 0, &period, &magnitude);
  CHECK(magnitude >= 40 && magnitude < 60);
  br_haptic_map(500, 230, &period, &magnitude);
  CHECK(period == 4 && magnitude > 100 && magnitude < 200);
  br_haptic_map(5000, 20, &period, &magnitude);
  CHECK(period == 10 && magnitude == 255);

  fake_motor motor = { .next_id = 3 };
  br_haptic *h = br_haptic_open_backend(&fake_motor_backend, &motor);
  CHECK(h && motor.gain == 10000);
  br_haptic_vib(h, 400, 1000, 0);
  br_haptic_flush(h);
  CHECK(motor.uploads == 1 && motor.plays == 1 && motor.last_length == 400 && motor.last_magnitude == 255);
  br_haptic_vib(h, 60000, 500, 0);                 // longer than one effect may be
  br_haptic_flush(h);
  CHECK(motor.uploads == 2 && motor.last_length == 10000);
  br_haptic_stop(h);
  br_haptic_flush(h);
  CHECK(motor.stops == 1);
  br_haptic_stop(h);                               // a stop with nothing playing does not touch the device
  br_haptic_flush(h);
  CHECK(motor.stops == 1);
  br_haptic_vib(h, 100, 0, 0);                     // intensity 0 is a stop
  br_haptic_flush(h);
  CHECK(motor.uploads == 2);
  br_haptic_vib(h, 100, 300, 0);
  br_haptic_flush(h);
  br_haptic_close(h);                              // close stops a running effect
  CHECK(motor.stops == 2);
}

// ---- network -------------------------------------------------------------------------------------
static size_t make_dhcp(uint8_t *frame, uint8_t kind, uint32_t xid)
{
  memset(frame, 0, 342);
  memset(frame, 0xff, 6);
  memcpy(frame + 6, "\x02\xd7\x01\x02\x03\x04", 6);
  frame[12] = 0x08;
  uint8_t *ip = frame + 14, *udp = ip + 20, *b = udp + 8;
  ip[0] = 0x45; ip[9] = 17; memset(ip + 16, 0xff, 4);
  udp[1] = 68; udp[3] = 67;
  b[0] = 1; b[1] = 1; b[2] = 6; memcpy(b + 4, &xid, 4);
  memcpy(b + 28, frame + 6, 6);
  memcpy(b + 236, "\x63\x82\x53\x63", 4);
  b[240] = 53; b[241] = 1; b[242] = kind; b[243] = 55; b[244] = 2; b[245] = 1; b[246] = 3; b[247] = 255;
  return 342;
}

static uint8_t dhcp_option(const uint8_t *reply, size_t length, uint8_t want, uint8_t *out)
{
  const uint8_t *b = reply + 42;
  for (size_t i = 240; i + 2 <= length - 42;) {
    if (b[i] == 255) break;
    if (b[i] == 0) { i++; continue; }
    if (b[i] == want) { memcpy(out, b + i + 2, b[i + 1]); return b[i + 1]; }
    i += 2u + b[i + 1];
  }
  return 0;
}

typedef struct { uint8_t frame[2000]; size_t length; unsigned count; } rx_record;
static void record_rx(void *ctx, const uint8_t *frame, size_t length) { rx_record *r = ctx; memcpy(r->frame, frame, length); r->length = length; __atomic_add_fetch(&r->count, 1, __ATOMIC_RELEASE); }
static bool rx_arrived(void *ctx) { return __atomic_load_n(&((rx_record *)ctx)->count, __ATOMIC_ACQUIRE) > 0; }

static void test_net(void)
{
  br_net_config c;
  br_net_config_defaults(&c);
  uint8_t frame[400], reply[600], value[16];
  size_t n = make_dhcp(frame, 1, 0x11223344);
  size_t size = br_dhcp_reply(&c, frame, n, reply, sizeof reply);
  CHECK(size >= 342);
  CHECK(reply[42] == 2 && !memcmp(reply + 42 + 4, "\x44\x33\x22\x11", 4));          // BOOTREPLY, the request's xid
  CHECK(!memcmp(reply + 42 + 16, "\xc0\xa8\x4d\x02", 4));                           // yiaddr 192.168.77.2
  CHECK(!memcmp(reply + 42 + 28, "\x02\xd7\x01\x02\x03\x04", 6));
  CHECK(dhcp_option(reply, size, 53, value) == 1 && value[0] == 2);                 // OFFER
  CHECK(dhcp_option(reply, size, 3, value) == 4 && !memcmp(value, "\xc0\xa8\x4d\x01", 4));
  CHECK(dhcp_option(reply, size, 6, value) == 8 && !memcmp(value, "\x08\x08\x08\x08\x01\x01\x01\x01", 8));
  CHECK(dhcp_option(reply, size, 1, value) == 4 && !memcmp(value, "\xff\xff\xff\x00", 4));
  // The IPv4 header checksum verifies (sums to 0xffff).
  uint32_t sum = 0;
  for (unsigned i = 0; i < 20; i += 2) sum += (uint32_t)(reply[14 + i] << 8 | reply[15 + i]);
  while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
  CHECK(sum == 0xffff);
  n = make_dhcp(frame, 3, 7);
  size = br_dhcp_reply(&c, frame, n, reply, sizeof reply);
  CHECK(size && dhcp_option(reply, size, 53, value) == 1 && value[0] == 5);         // REQUEST -> ACK
  n = make_dhcp(frame, 7, 7);
  CHECK(br_dhcp_reply(&c, frame, n, reply, sizeof reply) == 0);                     // RELEASE: no answer
  n = make_dhcp(frame, 1, 7);
  frame[14 + 20 + 3] = 53;                                                          // not the DHCP server port
  CHECK(br_dhcp_reply(&c, frame, n, reply, sizeof reply) == 0);
  CHECK(br_dhcp_reply(&c, frame, 60, reply, sizeof reply) == 0);
  n = make_dhcp(frame, 1, 7);
  frame[42 + 241] = 200;                                                            // an option running past the frame
  CHECK(br_dhcp_reply(&c, frame, n, reply, sizeof reply) == 0);

  // The fd back end: frames both ways, DHCP answered without reaching the wire.
  int pair[2];
  CHECK(socketpair(AF_UNIX, SOCK_DGRAM, 0, pair) == 0);
  rx_record record = { .count = 0 };
  br_net *net = br_net_open_fd(&c, pair[0], record_rx, &record);
  CHECK(net != NULL);
  uint8_t ping[64] = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 0x08, 0x06 };
  CHECK(br_net_send(net, ping, sizeof ping) == 0);
  CHECK(recv(pair[1], reply, sizeof reply, 0) == 64 && !memcmp(reply, ping, 64));
  CHECK(send(pair[1], ping, 60, 0) == 60);
  wait_for(rx_arrived, &record);
  CHECK(record.count == 1 && record.length == 60);
  n = make_dhcp(frame, 1, 9);
  CHECK(br_net_send(net, frame, n) == 0);
  CHECK(record.count == 2 && record.frame[42] == 2);                                // the offer came back to the guest
  CHECK(recv(pair[1], reply, sizeof reply, MSG_DONTWAIT) < 0);                      // and nothing went to the wire
  CHECK(br_net_send(net, ping, 10) == -EINVAL && br_net_send(net, ping, 2000) == -EINVAL);
  br_net_close(net);
  close(pair[1]);
}

// ---- Bluetooth -----------------------------------------------------------------------------------
typedef struct { unsigned opens, closes, sends; uint8_t last_type; uint8_t last[300]; size_t last_length; br_hci *hci; } fake_radio;
static int fr_open(void *ctx) { ((fake_radio *)ctx)->opens++; return 0; }
static int fr_send(void *ctx, uint8_t type, const uint8_t *packet, size_t length)
{
  fake_radio *r = ctx;
  r->sends++; r->last_type = type; r->last_length = length;
  memcpy(r->last, packet, length < sizeof r->last ? length : sizeof r->last);
  return 0;
}
static void fr_close(void *ctx) { ((fake_radio *)ctx)->closes++; }
static const br_hci_backend fake_radio_backend = { fr_open, fr_send, fr_close };

typedef struct { unsigned packets, states; int state; uint8_t type; size_t length; } hci_record;
static void hr_rx(void *ctx, uint8_t type, const uint8_t *packet, size_t length) { hci_record *r = ctx; (void)packet; r->packets++; r->type = type; r->length = length; }
static void hr_state(void *ctx, int state) { hci_record *r = ctx; r->states++; r->state = state; }

static void test_bt(const char *tmp)
{
  static const uint8_t reset[] = { 0x03, 0x0c, 0x00 }, complete[] = { 0x0e, 0x04, 0x01, 0x03, 0x0c, 0x00 };
  static const uint8_t acl[] = { 0x40, 0x20, 0x03, 0x00, 1, 2, 3 }, iso[] = { 0x01, 0x00, 0x02, 0x40, 9, 9 };
  CHECK(br_hci_packet_ok(1, reset, 3) && !br_hci_packet_ok(1, reset, 2) && !br_hci_packet_ok(1, complete, 6));
  CHECK(br_hci_packet_ok(4, complete, 6) && !br_hci_packet_ok(4, complete, 5));
  CHECK(br_hci_packet_ok(2, acl, 7) && !br_hci_packet_ok(2, acl, 8) && br_hci_packet_ok(5, iso, 6) && !br_hci_packet_ok(9, acl, 7));

  fake_radio radio = { 0 };
  hci_record record = { 0 };
  br_hci *h = br_hci_create(&fake_radio_backend, &radio, hr_rx, hr_state, &record);
  CHECK(br_hci_send(h, 1, reset, 3) == -ENOTCONN);                    // not open
  CHECK(br_hci_open(h) == 0 && radio.opens == 1 && br_hci_open(h) == 0 && radio.opens == 1);
  CHECK(br_hci_send(h, 1, reset, 3) == -ENOTCONN);                    // open, the HAL has not finished
  br_hci_set_state(h, 1);
  CHECK(record.states == 1 && record.state == 1);
  CHECK(br_hci_send(h, 1, reset, 3) == 0 && radio.sends == 1 && radio.last_type == 1);
  CHECK(br_hci_send(h, 4, complete, 6) == -EINVAL);                   // a guest does not send events
  CHECK(br_hci_send(h, 2, acl, 6) == -EINVAL && radio.sends == 1);
  br_hci_deliver(h, 4, complete, 6);
  br_hci_deliver(h, 4, complete, 3);                                   // malformed: dropped
  CHECK(record.packets == 1 && record.type == 4 && record.length == 6);
  br_hci_close(h);
  CHECK(radio.closes == 1 && record.state == 0);
  br_hci_destroy(h);

  // The address file, as text and as bytes.
  char path[400];
  uint8_t address[6];
  snprintf(path, sizeof path, "%s/bt_addr", tmp);
  FILE *f = fopen(path, "w"); fputs("A4:75:B9:01:02:03\n", f); fclose(f);
  CHECK(br_bt_address(path, address) == 0 && !memcmp(address, "\xa4\x75\xb9\x01\x02\x03", 6));
  f = fopen(path, "w"); fputs("a475b9010203", f); fclose(f);
  CHECK(br_bt_address(path, address) == 0 && address[5] == 3);
  f = fopen(path, "w"); fwrite("\x10\x20\x30\x40\x50\x60", 1, 6, f); fclose(f);
  CHECK(br_bt_address(path, address) == 0 && address[0] == 0x10);
  f = fopen(path, "w"); fputs("00:00:00:00:00:00", f); fclose(f);
  CHECK(br_bt_address(path, address) == -1 && br_bt_address("/nonexistent", address) == -1);

  // IServiceManager::get: the token, then two hidl_strings of two buffer objects each.
  static hb_parcel p, reply;
  bt_parcel_get_service(&p, BT_HCI_1_1, "default");
  CHECK(!p.overflow && p.nobjects == 4 && !strcmp((const char *)p.data, HIDL_MANAGER));
  size_t token = (strlen(HIDL_MANAGER) + 1 + 3) & ~(size_t)3;
  CHECK(p.offsets[0] == token && p.offsets[1] == token + 40 && p.size == token + 160);
  struct hb_buffer_object o0, o1, o3;
  memcpy(&o0, p.data + p.offsets[0], 40); memcpy(&o1, p.data + p.offsets[1], 40); memcpy(&o3, p.data + p.offsets[3], 40);
  CHECK(o0.type == HB_TYPE_PTR && o0.length == 16 && o0.flags == 0);
  CHECK(o1.flags == HB_BUFFER_HAS_PARENT && o1.parent == 0 && o1.parent_offset == 0 && o1.length == strlen(BT_HCI_1_1) + 1);
  CHECK(!strcmp((const char *)(uintptr_t)o1.buffer, BT_HCI_1_1) && o3.parent == 2 && o3.length == 8);
  CHECK(p.buffers_size == 16 + ((strlen(BT_HCI_1_1) + 1 + 7) & ~7ull) + 16 + 8);
  struct hb_hidl_ref ref;
  memcpy(&ref, (const void *)(uintptr_t)o0.buffer, 16);
  CHECK(ref.size == strlen(BT_HCI_1_1) && ref.pointer == o1.buffer);

  bt_parcel_initialize(&p, true, 0x1234);
  CHECK(p.nobjects == 1 && !strcmp((const char *)p.data, BT_HCI_1_1));
  struct hb_flat_object flat;
  memcpy(&flat, p.data + p.offsets[0], 24);
  CHECK(flat.type == HB_TYPE_BINDER && flat.binder_or_handle == 0x1234 && p.buffers_size == 0);

  // A send parcel is what the HAL's callback gets back in the other direction: read our own.
  bt_parcel_send(&p, BT_SEND_COMMAND, reset, 3);
  hb_reader r;
  hb_reader_init(&r, p.data, p.size, p.offsets, p.nobjects * 8);
  const uint8_t *bytes; size_t length;
  CHECK(!strcmp(hb_get_token(&r), BT_HCI_1_0) && hb_get_bytes(&r, &bytes, &length) && length == 3 && !memcmp(bytes, reset, 3) && !r.bad);

  // hciEventReceived as the kernel delivers it (the same layout, the callbacks' token).
  hb_parcel_init(&p); hb_put_token(&p, BT_CALLBACKS_1_0); hb_put_bytes(&p, complete, 6);
  hb_reader_init(&r, p.data, p.size, p.offsets, p.nobjects * 8);
  uint8_t type; int32_t status;
  CHECK(bt_callback_transact(BT_CB_EVENT, &r, &reply, &type, &bytes, &length, &status) == 0 && type == 4 && length == 6 && !memcmp(bytes, complete, 6));
  CHECK(reply.size == 4 && reply.nobjects == 0);                      // Status::ok()
  hb_reader_init(&r, p.data, p.size, p.offsets, p.nobjects * 8);
  CHECK(bt_callback_transact(BT_CB_ISO, &r, &reply, &type, &bytes, &length, &status) == HB_UNKNOWN_TRANSACTION);   // 1.1's method, 1.0's token
  hb_parcel_init(&p); hb_put_token(&p, BT_CALLBACKS_1_0); hb_put_i32(&p, 0);
  hb_reader_init(&r, p.data, p.size, p.offsets, 0);
  CHECK(bt_callback_transact(BT_CB_INIT_COMPLETE, &r, &reply, &type, &bytes, &length, &status) == 0 && type == 0 && status == 0);
  // A truncated event parcel is refused, not read.
  hb_parcel_init(&p); hb_put_token(&p, BT_CALLBACKS_1_0); hb_put_bytes(&p, complete, 6);
  hb_reader_init(&r, p.data, p.size - 40, p.offsets, p.nobjects * 8);
  CHECK(bt_callback_transact(BT_CB_EVENT, &r, &reply, &type, &bytes, &length, &status) == HB_UNKNOWN_TRANSACTION);
  // interfaceChain: a vector of three strings, each embedded in the array, in rising offsets.
  hb_parcel_init(&p); hb_put_token(&p, HIDL_BASE);
  hb_reader_init(&r, p.data, p.size, p.offsets, 0);
  CHECK(bt_callback_transact(HB_INTERFACE_CHAIN, &r, &reply, &type, &bytes, &length, &status) == 0 && reply.nobjects == 5 && !reply.overflow);
  struct hb_buffer_object c1, c4;
  memcpy(&c1, reply.data + reply.offsets[1], 40); memcpy(&c4, reply.data + reply.offsets[4], 40);
  CHECK(c1.length == 48 && c1.parent == 0 && c4.parent == 1 && c4.parent_offset == 32 && !strcmp((const char *)(uintptr_t)c4.buffer, HIDL_BASE));
  hb_reader_init(&r, p.data, p.size, p.offsets, 0);
  CHECK(bt_callback_transact(0x7777, &r, &reply, &type, &bytes, &length, &status) == HB_UNKNOWN_TRANSACTION);
  // A parcel too large for its buffer says so instead of writing past it.
  static uint8_t big[9000];
  hb_parcel_init(&p);
  for (unsigned i = 0; i < 20; i++) hb_put_bytes(&p, big, 10);
  CHECK(p.overflow);
}

// ---- the virtio devices --------------------------------------------------------------------------
static fake_radio dev_radio;
static br_hci *radio_factory(br_hci_rx_fn rx, br_hci_state_fn state, void *ctx)
{
  dev_radio.hci = br_hci_create(&fake_radio_backend, &dev_radio, rx, state, ctx);
  return dev_radio.hci;
}

static void send_message(guest *g, uint16_t channel, uint16_t type, const void *payload, uint32_t length)
{
  static uint8_t message[OB_MAX_MESSAGE];
  struct ob_header header = { channel, type, length };
  memcpy(message, &header, sizeof header);
  if (length) memcpy(message + sizeof header, payload, length);
  post1(g, OB_QUEUE_TX, message, (uint32_t)sizeof header + length, false);
  CHECK(reap(g, OB_QUEUE_TX) == 0);
}

static void test_bridge_device(const char *tmp)
{
  char address_path[400];
  snprintf(address_path, sizeof address_path, "%s/bt_addr", tmp);
  FILE *f = fopen(address_path, "w"); fputs("A4:75:B9:0A:0B:0C\n", f); fclose(f);
  fake_motor motor = { 0 };
  br_haptic *haptic = br_haptic_open_backend(&fake_motor_backend, &motor);
  br_bridge_config config = { .haptic = haptic, .hci_factory = radio_factory, .bt_address_path = address_path };
  xh_virtio *dev = br_bridge_create(0, 40, &config);
  CHECK(dev != NULL);
  guest g;
  guest_up(&g, dev, 2, 0);
  CHECK(r32(&g, 0x008) == OB_VIRTIO_DEVICE_ID && r32(&g, 0x100) == OB_PROTO_VERSION);

  static uint8_t rx[4][OB_MAX_MESSAGE];
  for (unsigned i = 0; i < 2; i++) post1(&g, OB_QUEUE_RX, rx[i], OB_MAX_MESSAGE, true);
  CHECK(reap(&g, OB_QUEUE_RX) == -1);

  struct ob_ctl_hello hello = { OB_PROTO_VERSION, 0 };
  send_message(&g, OB_CH_CONTROL, OB_CTL_HELLO, &hello, sizeof hello);
  CHECK(reap(&g, OB_QUEUE_RX) == 16);
  struct ob_header header;
  memcpy(&header, rx[0], 8); memcpy(&hello, rx[0] + 8, 8);
  CHECK(header.channel == OB_CH_CONTROL && header.type == OB_CTL_HELLO && hello.channels == 7);

  struct ob_haptic_vib vib = { 250, 800, 0 };
  send_message(&g, OB_CH_HAPTIC, OB_HAPTIC_VIB, &vib, sizeof vib);
  br_haptic_flush(haptic);
  CHECK(motor.plays == 1 && motor.last_length == 250);
  send_message(&g, OB_CH_HAPTIC, OB_HAPTIC_STOP, NULL, 0);
  br_haptic_flush(haptic);
  CHECK(motor.stops == 1);

  // Bluetooth: open, the HAL becomes ready, a command goes out, its event comes back.
  static const uint8_t reset[] = { 0x03, 0x0c, 0x00 }, complete[] = { 0x0e, 0x04, 0x01, 0x03, 0x0c, 0x00 };
  send_message(&g, OB_CH_HCI, OB_HCI_COMMAND, reset, 3);              // before open: ignored
  CHECK(dev_radio.sends == 0);
  send_message(&g, OB_CH_HCI, OB_HCI_OPEN, NULL, 0);
  CHECK(dev_radio.opens == 1);
  br_hci_set_state(dev_radio.hci, OB_HCI_STATE_READY);
  CHECK(reap(&g, OB_QUEUE_RX) == 20);
  struct ob_hci_state state;
  memcpy(&header, rx[1], 8); memcpy(&state, rx[1] + 8, sizeof state);
  CHECK(header.channel == OB_CH_HCI && header.type == OB_HCI_STATE && state.state == OB_HCI_STATE_READY && !memcmp(state.bd_addr, "\xa4\x75\xb9\x0a\x0b\x0c", 6));
  send_message(&g, OB_CH_HCI, OB_HCI_COMMAND, reset, 3);
  CHECK(dev_radio.sends == 1 && dev_radio.last_type == 1 && dev_radio.last_length == 3);
  // The guest has no buffer posted: the event waits, and arrives when one is.
  br_hci_deliver(dev_radio.hci, OB_HCI_EVENT, complete, 6);
  CHECK(reap(&g, OB_QUEUE_RX) == -1);
  post1(&g, OB_QUEUE_RX, rx[2], OB_MAX_MESSAGE, true);
  CHECK(reap(&g, OB_QUEUE_RX) == 14);
  memcpy(&header, rx[2], 8);
  CHECK(header.type == OB_HCI_EVENT && header.length == 6 && !memcmp(rx[2] + 8, complete, 6));

  // A second open of a controller that is ready is answered with its state again (a new iOS client waits for it).
  post1(&g, OB_QUEUE_RX, rx[3], OB_MAX_MESSAGE, true);
  send_message(&g, OB_CH_HCI, OB_HCI_OPEN, NULL, 0);
  CHECK(dev_radio.opens == 1 && reap(&g, OB_QUEUE_RX) == 20);
  memcpy(&header, rx[3], 8); memcpy(&state, rx[3] + 8, sizeof state);
  CHECK(header.type == OB_HCI_STATE && state.state == OB_HCI_STATE_READY);

  // Malformed messages from the guest change nothing.
  uint8_t junk[4] = { 1, 2, 3, 4 };
  post1(&g, OB_QUEUE_TX, junk, 4, false);
  CHECK(reap(&g, OB_QUEUE_TX) == 0);
  struct ob_header lying = { OB_CH_HCI, OB_HCI_COMMAND, 4000 };
  post1(&g, OB_QUEUE_TX, &lying, sizeof lying, false);
  CHECK(reap(&g, OB_QUEUE_TX) == 0 && dev_radio.sends == 1);
  send_message(&g, 99, 1, NULL, 0);

  // A reset stops the motor and releases the controller.
  vib.on_ms = 5000;
  send_message(&g, OB_CH_HAPTIC, OB_HAPTIC_VIB, &vib, sizeof vib);
  br_haptic_flush(haptic);
  w32(&g, 0x70, 0);
  br_haptic_flush(haptic);
  CHECK(motor.stops == 2 && dev_radio.closes == 1);
  br_haptic_close(haptic);
}

static rx_record wire;
static br_net *wire_net;
static br_net_rx_fn guest_rx;
static void *guest_rx_ctx;
static int wire_pair[2];
static br_net *open_wire(const br_net_config *c, br_net_rx_fn rx, void *ctx)
{
  guest_rx = rx; guest_rx_ctx = ctx;
  if (socketpair(AF_UNIX, SOCK_DGRAM, 0, wire_pair)) return NULL;
  return wire_net = br_net_open_fd(c, wire_pair[0], rx, ctx);
}

static void test_net_device(void)
{
  br_net_config c;
  br_net_config_defaults(&c);
  memcpy(c.guest_mac, "\x02\xd7\x01\x02\x03\x04", 6);
  xh_virtio *dev = br_virtio_net_create(0, 41, &c, open_wire);
  CHECK(dev != NULL && wire_net != NULL);
  guest g;
  guest_up(&g, dev, 2, 1u << 5);
  CHECK(r32(&g, 0x008) == 1 && (r32(&g, 0x100) & 0xffffff) == 0x01d702);        // the MAC in the configuration space
  static uint8_t rxbuf[2][2048], tx[12 + 64];
  post1(&g, 0, rxbuf[0], 2048, true);
  post1(&g, 0, rxbuf[1], 2048, true);
  // Guest to wire: the 12-byte header is taken off.
  for (unsigned i = 0; i < 64; i++) tx[12 + i] = (uint8_t)i;
  post1(&g, 1, tx, sizeof tx, false);
  CHECK(reap(&g, 1) == 0);
  uint8_t seen[200];
  CHECK(recv(wire_pair[1], seen, sizeof seen, 0) == 64 && seen[13] == 13);
  // Wire to guest: a header with num_buffers = 1, then the frame.
  CHECK(send(wire_pair[1], seen, 60, 0) == 60);
  for (unsigned i = 0; i < 2000 && g.q[0].used.idx == 0; i++) usleep(1000);
  CHECK(reap(&g, 0) == 72 && rxbuf[0][10] == 1 && rxbuf[0][11] == 0 && rxbuf[0][12 + 13] == 13);
  // DHCP from the guest is answered into its receive queue.
  static uint8_t dhcp[12 + 342];
  make_dhcp(dhcp + 12, 1, 5);
  post1(&g, 1, dhcp, sizeof dhcp, false);
  CHECK(reap(&g, 1) == 0 && reap(&g, 0) >= 12 + 342 && rxbuf[1][12 + 42] == 2);
  // No buffer posted: the frame is dropped, nothing breaks.
  CHECK(send(wire_pair[1], seen, 60, 0) == 60);
  usleep(20000);
  CHECK(reap(&g, 0) == -1);
  (void)wire;
  br_net_close(wire_net);
  close(wire_pair[1]);
}

typedef struct { unsigned starts[2], stops[2], frames; int16_t first; } fake_sound;
static int fs_start(void *ctx, bool output, unsigned rate, unsigned channels) { fake_sound *s = ctx; s->starts[output]++; return rate == 48000 && channels == (output ? 2u : 1u) ? 0 : -1; }
static int fs_stop(void *ctx, bool output) { ((fake_sound *)ctx)->stops[output]++; return 0; }
static int fs_write(void *ctx, const int16_t *pcm, unsigned frames) { fake_sound *s = ctx; if (!s->frames) s->first = pcm[0]; s->frames += frames; return (int)frames; }
static int fs_read(void *ctx, int16_t *pcm, unsigned frames) { (void)ctx; usleep(2000); for (unsigned i = 0; i < frames; i++) pcm[i] = (int16_t)(100 + i); return (int)frames; }
static const br_audio_backend fake_sound_backend = { fs_start, fs_stop, fs_write, fs_read, NULL };

static uint32_t snd_control(guest *g, void *request, uint32_t request_size, void *reply, uint32_t reply_size)
{
  void *bufs[2] = { request, reply };
  uint32_t lens[2] = { request_size, reply_size };
  bool writable[2] = { false, true };
  uint32_t status = 0;
  post(g, 0, bufs, lens, writable, 2);
  CHECK(reap(g, 0) >= 4);
  memcpy(&status, reply, 4);
  return status;
}

static void test_snd_device(void)
{
  fake_sound sound = { 0 };
  xh_virtio *dev = br_virtio_snd_create(0, 42, &fake_sound_backend, &sound);
  guest g;
  guest_up(&g, dev, 4, 0);
  CHECK(r32(&g, 0x008) == 25 && r32(&g, 0x104) == 2);
  static uint8_t reply[4 + 2 * 32];
  uint32_t query[4] = { 0x0100, 0, 2, 32 };
  CHECK(snd_control(&g, query, sizeof query, reply, sizeof reply) == 0x8000);
  CHECK(reply[4 + 24] == 0 && reply[4 + 25] == 2 && reply[4 + 32 + 24] == 1 && reply[4 + 32 + 26] == 1);      // output stereo, input mono
  uint64_t formats;
  memcpy(&formats, reply + 4 + 8, 8);
  CHECK(formats == 1ull << 5);
  struct { uint32_t code, stream, buffer, period, features; uint8_t channels, format, rate, pad; } params = { 0x0101, 0, 15360, 960, 0, 2, 5, 7, 0 };
  CHECK(snd_control(&g, &params, sizeof params, reply, 4) == 0x8000);
  params.rate = 6;                                              // 44.1 kHz: not offered
  CHECK(snd_control(&g, &params, sizeof params, reply, 4) == 0x8002);
  params.rate = 7; params.stream = 1; params.channels = 1;
  CHECK(snd_control(&g, &params, sizeof params, reply, 4) == 0x8000);
  uint32_t command[2] = { 0x0102, 0 };
  CHECK(snd_control(&g, command, sizeof command, reply, 4) == 0x8000);
  command[0] = 0x0104;
  CHECK(snd_control(&g, command, sizeof command, reply, 4) == 0x8000 && sound.starts[1] == 1);
  command[1] = 7;
  CHECK(snd_control(&g, command, sizeof command, reply, 4) == 0x8001);

  // One output period.
  static uint32_t stream_id = 0, status[2];
  static int16_t period[480];
  for (unsigned i = 0; i < 480; i++) period[i] = (int16_t)(i + 1);
  void *bufs[3] = { &stream_id, period, status };
  uint32_t lens[3] = { 4, sizeof period, 8 };
  bool writable[3] = { false, false, true };
  post(&g, 2, bufs, lens, writable, 3);
  CHECK(reap(&g, 2) == 8 && status[0] == 0x8000 && sound.frames == 240 && sound.first == 1);

  // Capture: posted buffers come back holding a period each.
  static uint32_t in_stream = 1, in_status[2][2];
  static int16_t in_data[2][240];
  for (unsigned i = 0; i < 2; i++) {
    void *b[3] = { &in_stream, in_data[i], in_status[i] };
    uint32_t l[3] = { 4, 480, 8 };
    bool w[3] = { false, true, true };
    post(&g, 3, b, l, w, 3);
  }
  command[0] = 0x0104; command[1] = 1;
  CHECK(snd_control(&g, command, sizeof command, reply, 4) == 0x8000 && sound.starts[0] == 1);
  for (unsigned i = 0; i < 2000 && g.q[3].used.idx < 2; i++) usleep(1000);
  CHECK(reap(&g, 3) == 488 && in_data[0][0] == 100 && in_data[0][239] == 339 && in_status[0][0] == 0x8000);
  command[0] = 0x0105;
  CHECK(snd_control(&g, command, sizeof command, reply, 4) == 0x8000 && sound.stops[0] == 1);
  command[1] = 0;
  CHECK(snd_control(&g, command, sizeof command, reply, 4) == 0x8000 && sound.stops[1] == 1);
  // A period for a stopped stream is acknowledged and not played.
  post(&g, 2, bufs, lens, writable, 3);
  CHECK(reap(&g, 2) == 8 && sound.frames == 240);

  void *ctx;
  const br_audio_backend *null = br_audio_null(&ctx);
  CHECK(null->start(ctx, true, 48000, 2) == 0 && null->write(ctx, period, 240) == 240);
}

int main(void)
{
  char tmp[] = "/tmp/oneios-bridges-XXXXXX";
  if (!mkdtemp(tmp)) return 2;
  test_cpu(tmp);
  test_haptic();
  test_net();
  test_bt(tmp);
  test_bridge_device(tmp);
  test_net_device();
  test_snd_device();
  char cmd[300];
  snprintf(cmd, sizeof cmd, "rm -rf '%s'", tmp);
  (void)!system(cmd);
  printf("bridges: %d checks, %d failed\n", checks, failures);
  return failures ? 1 : 0;
}
