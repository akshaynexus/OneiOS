// SPDX-License-Identifier: MIT
// bridgectl: tries each host bridge on the phone, without xnuhost or a guest. Run as root (su):
//   bridgectl cpu [NVCPU]            the host's cores and the vCPU plan made from them
//   bridgectl haptic [MS [INTENSITY [HZ]]]   one vibration (default 200 ms, 1000, resonance)
//   bridgectl haptic-pattern         three taps and a ramp: what an iOS pattern feels like
//   bridgectl bt                     opens the Bluetooth HAL, sends HCI Reset and Read BD_ADDR, prints the events
//   bridgectl net                    opens the tap, answers the first DHCP request it sees, counts frames for 10 s
//   bridgectl audio HELPER           plays a 440 Hz tone for 2 s through the helper (path of oneios-audio)
// Each prints what it found and ends with "ok" or the reason it failed.
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int cpu(int argc, char **argv)
{
  br_cpu_topology t;
  br_cpu_plan plan;
  unsigned nvcpu = argc > 2 ? (unsigned)atoi(argv[2]) : 8;
  if (br_cpu_read("/sys", &t)) { printf("no CPUs under /sys/devices/system/cpu\n"); return 1; }
  printf("host: %u CPUs, %u P, %u E\n", t.ncpu, t.n_p, t.n_e);
  for (unsigned i = 0; i < t.ncpu; i++) {
    const br_host_cpu *c = &t.cpu[i];
    if (c->present) printf("  cpu%u: %c capacity %u part 0x%03x policy %u %u..%u kHz (%u steps) L1d %u L2 %u L3 %u\n", i, c->type, c->capacity, c->part, c->policy,
                           c->min_khz, c->max_khz, c->nfreq, c->l1d_bytes, c->l2_bytes, c->l3_bytes);
  }
  if (br_cpu_plan_make(&t, nvcpu, false, &plan)) { printf("no plan for %u vCPUs\n", nvcpu); return 1; }
  printf("plan: %u vCPUs, %u E then %u P\n", plan.nvcpu, plan.n_e, plan.n_p);
  for (unsigned i = 0; i < plan.nvcpu; i++)
    printf("  vcpu%u: %c cluster %u host mask 0x%llx capacity %u max %u kHz\n", i, plan.v[i].type, plan.v[i].cluster_id, (unsigned long long)plan.v[i].affinity,
           plan.v[i].capacity, plan.v[i].max_khz);
  int rc = br_cpu_place(0, &plan, plan.nvcpu - 1);
  printf("placing this thread as vcpu%u: %s\n", plan.nvcpu - 1, rc ? strerror(-rc) : "done");
  rc = br_cpu_request(0, &plan, plan.nvcpu - 1, plan.v[plan.nvcpu - 1].max_khz);
  printf("uclamp.min %u for the top frequency: %s\n", br_cpu_util_for(&plan, plan.nvcpu - 1, plan.v[plan.nvcpu - 1].max_khz), rc ? strerror(-rc) : "done");
  return 0;
}

static int haptic(int argc, char **argv, bool pattern)
{
  br_haptic *h = br_haptic_open(NULL);
  if (!h) return 1;
  if (pattern) {
    for (unsigned i = 0; i < 3; i++) { br_haptic_vib(h, 15, 1000, 0); usleep(150000); }
    for (unsigned level = 100; level <= 1000; level += 100) { br_haptic_vib(h, 200, level, 0); usleep(60000); }
    usleep(300000);
  } else {
    unsigned ms = argc > 2 ? (unsigned)atoi(argv[2]) : 200, intensity = argc > 3 ? (unsigned)atoi(argv[3]) : 1000, hz = argc > 4 ? (unsigned)atoi(argv[4]) : 0;
    br_haptic_vib(h, ms, intensity, hz);
    usleep((ms + 100) * 1000);
  }
  br_haptic_close(h);
  return 0;
}

static volatile int bt_state = -1;
static volatile unsigned bt_events;
static void bt_rx(void *ctx, uint8_t type, const uint8_t *packet, size_t length)
{
  (void)ctx;
  printf("  <- type %u, %zu bytes:", type, length);
  for (size_t i = 0; i < length && i < 24; i++) printf(" %02x", packet[i]);
  printf("\n");
  bt_events++;
}
static void bt_changed(void *ctx, int state) { (void)ctx; bt_state = state; printf("  state %d\n", state); }

static int bt(void)
{
  static const uint8_t reset[] = { 0x03, 0x0c, 0x00 }, read_address[] = { 0x09, 0x10, 0x00 };
  uint8_t address[6];
  if (!br_bt_address("/mnt/vendor/efs/bluetooth/bt_addr", address)) printf("address file: %02x:%02x:%02x:%02x:%02x:%02x\n", address[0], address[1], address[2], address[3], address[4], address[5]);
  br_hci *h = br_hci_open_hidl(bt_rx, bt_changed, NULL);
  if (!h || br_hci_open(h)) return 1;
  for (unsigned i = 0; i < 200 && bt_state < 1; i++) usleep(100000);      // the firmware download takes seconds
  if (bt_state != 1) { printf("the HAL did not become ready (state %d)\n", bt_state); return 1; }
  int rc = br_hci_send(h, 1, reset, sizeof reset);
  usleep(500000);
  if (!rc) rc = br_hci_send(h, 1, read_address, sizeof read_address);
  usleep(500000);
  br_hci_close(h);
  usleep(200000);
  if (rc || bt_events < 2) { printf("send %d, %u events (2 expected)\n", rc, bt_events); return 1; }
  return 0;
}

static volatile unsigned net_frames;
static void net_rx(void *ctx, const uint8_t *frame, size_t length) { (void)ctx; (void)frame; (void)length; net_frames++; }

static int net(void)
{
  br_net_config c;
  br_net_config_defaults(&c);
  br_net *n = br_net_open_tap(&c, net_rx, NULL);
  if (!n) return 1;
  printf("tap %s is open; run `oneios-net.sh up` in another shell, then `ping -I %s 192.168.77.2` shows ARP requests here\n", c.ifname, c.ifname);
  sleep(10);
  printf("%u frames from the tap in 10 s\n", net_frames);
  br_net_close(n);
  return 0;
}

static int audio(const char *path)
{
  void *ctx;
  const br_audio_backend *b = br_audio_helper(path, &ctx);
  if (!b || b->start(ctx, true, 48000, 2)) return 1;
  int16_t period[480];
  double phase = 0;
  for (unsigned p = 0; p < 400; p++) {                          // 2 s of 5 ms periods
    for (unsigned i = 0; i < 240; i++) { period[2 * i] = period[2 * i + 1] = (int16_t)(8000 * sin(phase)); phase += 2 * M_PI * 440 / 48000; }
    b->write(ctx, period, 240);
    usleep(5000);
  }
  b->stop(ctx, true);
  usleep(100000);
  b->close(ctx);
  return 0;
}

int main(int argc, char **argv)
{
  int rc = 2;
  setvbuf(stdout, NULL, _IOLBF, 0);
  if (argc < 2) { fprintf(stderr, "usage: bridgectl cpu|haptic|haptic-pattern|bt|net|audio ... (see tools/bridgectl.c)\n"); return 2; }
  if (!strcmp(argv[1], "cpu")) rc = cpu(argc, argv);
  else if (!strcmp(argv[1], "haptic")) rc = haptic(argc, argv, false);
  else if (!strcmp(argv[1], "haptic-pattern")) rc = haptic(argc, argv, true);
  else if (!strcmp(argv[1], "bt")) rc = bt();
  else if (!strcmp(argv[1], "net")) rc = net();
  else if (!strcmp(argv[1], "audio") && argc > 2) rc = audio(argv[2]);
  printf("%s\n", rc == 0 ? "ok" : rc == 2 ? "unknown command" : "failed");
  return rc;
}
