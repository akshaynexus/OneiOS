// SPDX-License-Identifier: MIT
// Network bridge: the frames of the guest's en0 (virtio-net, UniXNU-droidvm's DroidVMVirtioNet.c and
// DroidVMVirtioEthernet.cpp in the kernel) to a tap device on One UI's kernel. One UI's Wi-Fi stays
// associated (wpa_supplicant and the vendor Wi-Fi HAL are init services and keep running in a
// session); the tap is routed and NATed through the phone's active network by net/oneios-net.sh.
//
// iOS configures en0 with DHCP. No DHCP server runs in a session (Android's is in the network stack
// app, which is gone with zygote), so the bridge answers the guest's DHCP itself: one lease, the
// tap's host address as router, the configured DNS servers.
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#ifdef __linux__
#include <linux/if.h>
#include <linux/if_tun.h>
#include <sys/ioctl.h>
#endif

#define FRAME_MAX 1514u

struct br_net {
  br_net_config config;
  int fd;
  br_net_rx_fn rx;
  void *ctx;
  pthread_t thread;
  bool running;
  int wake[2];
};

void br_net_config_defaults(br_net_config *c)
{
  memset(c, 0, sizeof *c);
  strcpy(c->ifname, "oneios0");
  c->host_ip = 0xc0a84d01;          // 192.168.77.1
  c->guest_ip = 0xc0a84d02;         // 192.168.77.2
  c->netmask = 0xffffff00;
  c->dns[0] = 0x08080808;           // replaced by oneios-net.sh's answer when the phone's DNS is known
  c->dns[1] = 0x01010101;
  c->lease_s = 86400;
  c->dhcp = true;
}

// ---- DHCP ---------------------------------------------------------------------------------------
static uint16_t be16(const uint8_t *p) { return (uint16_t)(p[0] << 8 | p[1]); }
static void put16(uint8_t *p, uint16_t v) { p[0] = (uint8_t)(v >> 8); p[1] = (uint8_t)v; }
static void put32(uint8_t *p, uint32_t v) { p[0] = (uint8_t)(v >> 24); p[1] = (uint8_t)(v >> 16); p[2] = (uint8_t)(v >> 8); p[3] = (uint8_t)v; }

static uint16_t checksum(const uint8_t *data, size_t length)
{
  uint32_t sum = 0;
  for (size_t i = 0; i + 1 < length; i += 2) sum += (uint32_t)(data[i] << 8 | data[i + 1]);
  if (length & 1) sum += (uint32_t)data[length - 1] << 8;
  while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
  return (uint16_t)~sum;
}

size_t br_dhcp_reply(const br_net_config *c, const uint8_t *frame, size_t length, uint8_t *out, size_t out_size)
{
  // Ethernet 14, IPv4 (IHL from the header), UDP 8, BOOTP 236, magic cookie 4, options.
  if (length < 14 + 20 + 8 + 240 || be16(frame + 12) != 0x0800) return 0;
  const uint8_t *ip = frame + 14;
  size_t ihl = (size_t)(ip[0] & 15) * 4;
  if ((ip[0] >> 4) != 4 || ihl < 20 || ip[9] != 17 || length < 14 + ihl + 8 + 240) return 0;
  const uint8_t *udp = ip + ihl;
  if (be16(udp) != 68 || be16(udp + 2) != 67) return 0;
  const uint8_t *bootp = udp + 8;
  size_t bootp_len = length - (size_t)(bootp - frame);
  if (bootp[0] != 1 || bootp[1] != 1 || bootp[2] != 6 || memcmp(bootp + 236, "\x63\x82\x53\x63", 4)) return 0;
  uint8_t kind = 0;
  for (size_t i = 240; i < bootp_len;) {
    uint8_t option = bootp[i];
    if (option == 255) break;
    if (option == 0) { i++; continue; }
    if (i + 2 > bootp_len || i + 2 + bootp[i + 1] > bootp_len) return 0;
    if (option == 53 && bootp[i + 1] == 1) kind = bootp[i + 2];
    i += 2u + bootp[i + 1];
  }
  uint8_t answer;
  if (kind == 1) answer = 2;                 // DISCOVER -> OFFER
  else if (kind == 3) answer = 5;            // REQUEST -> ACK
  else return 0;                             // RELEASE, INFORM, DECLINE: nothing to say

  uint8_t options[64], *o = options;
  *o++ = 53; *o++ = 1; *o++ = answer;
  *o++ = 54; *o++ = 4; put32(o, c->host_ip); o += 4;
  *o++ = 51; *o++ = 4; put32(o, c->lease_s); o += 4;
  *o++ = 1; *o++ = 4; put32(o, c->netmask); o += 4;
  *o++ = 3; *o++ = 4; put32(o, c->host_ip); o += 4;
  unsigned ndns = (c->dns[0] != 0) + (c->dns[1] != 0);
  if (ndns) {
    *o++ = 6; *o++ = (uint8_t)(4 * ndns);
    for (unsigned i = 0; i < 2; i++) if (c->dns[i]) { put32(o, c->dns[i]); o += 4; }
  }
  *o++ = 255;
  size_t options_len = (size_t)(o - options), bootp_out = 240 + options_len;
  if (bootp_out < 300) bootp_out = 300;      // BOOTP's minimum, which some clients insist on
  size_t total = 14 + 20 + 8 + bootp_out;
  if (total > out_size) return 0;
  memset(out, 0, total);
  // The client has no address yet: answer to broadcast, as a server does when the flag asks or always may.
  memset(out, 0xff, 6);
  uint8_t host_mac[6] = { 0x02, 0x4f, 0x69, 0x4f, 0x53, 0x01 };
  memcpy(out + 6, host_mac, 6);
  put16(out + 12, 0x0800);
  uint8_t *oip = out + 14, *oudp = oip + 20, *ob = oudp + 8;
  oip[0] = 0x45; put16(oip + 2, (uint16_t)(20 + 8 + bootp_out)); oip[8] = 64; oip[9] = 17;
  put32(oip + 12, c->host_ip); put32(oip + 16, 0xffffffff);
  put16(oip + 10, checksum(oip, 20));
  put16(oudp, 67); put16(oudp + 2, 68); put16(oudp + 4, (uint16_t)(8 + bootp_out));   // checksum 0: none (IPv4)
  ob[0] = 2; ob[1] = 1; ob[2] = 6;
  memcpy(ob + 4, bootp + 4, 4);              // xid
  memcpy(ob + 10, bootp + 10, 2);            // flags
  put32(ob + 16, c->guest_ip);               // yiaddr
  put32(ob + 20, c->host_ip);                // siaddr
  memcpy(ob + 28, bootp + 28, 16);           // chaddr
  memcpy(ob + 236, "\x63\x82\x53\x63", 4);
  memcpy(ob + 240, options, options_len);
  return total;
}

// ---- back ends ----------------------------------------------------------------------------------
static void *receiver(void *arg)
{
  br_net *n = arg;
  uint8_t frame[FRAME_MAX + 64];
  struct pollfd fds[2] = { { .fd = n->fd, .events = POLLIN }, { .fd = n->wake[0], .events = POLLIN } };
  while (__atomic_load_n(&n->running, __ATOMIC_ACQUIRE)) {
    if (poll(fds, 2, -1) < 0) { if (errno == EINTR) continue; break; }
    if (fds[1].revents) break;                                    // br_net_close
    if (!(fds[0].revents & POLLIN)) { if (fds[0].revents) break; continue; }
    ssize_t got = read(n->fd, frame, sizeof frame);
    if (got > 0) n->rx(n->ctx, frame, (size_t)got);
    else if (got == 0 || (errno != EINTR && errno != EAGAIN)) break;
  }
  return NULL;
}

br_net *br_net_open_fd(const br_net_config *c, int fd, br_net_rx_fn rx, void *ctx)
{
  br_net *n = calloc(1, sizeof *n);
  if (!n) return NULL;
  n->config = *c;
  n->fd = fd;
  n->rx = rx;
  n->ctx = ctx;
  n->running = true;
  if (pipe(n->wake)) { free(n); return NULL; }
  if (pthread_create(&n->thread, NULL, receiver, n)) { close(n->wake[0]); close(n->wake[1]); free(n); return NULL; }
  return n;
}

br_net *br_net_open_tap(const br_net_config *c, br_net_rx_fn rx, void *ctx)
{
#ifdef __linux__
  int fd = open("/dev/net/tun", O_RDWR | O_CLOEXEC);
  if (fd < 0) fd = open("/dev/tun", O_RDWR | O_CLOEXEC);          // Android's node
  if (fd < 0) { br_log(0, "net: off: /dev/tun: %s", strerror(errno)); return NULL; }
  struct ifreq request;
  memset(&request, 0, sizeof request);
  request.ifr_flags = IFF_TAP | IFF_NO_PI;
  strncpy(request.ifr_name, c->ifname, IFNAMSIZ - 1);
  if (ioctl(fd, TUNSETIFF, &request) < 0) { br_log(0, "net: off: TUNSETIFF %s: %s", c->ifname, strerror(errno)); close(fd); return NULL; }
  br_net *n = br_net_open_fd(c, fd, rx, ctx);
  if (!n) { close(fd); return NULL; }
  br_log(1, "net: on: tap %s (run net/oneios-net.sh up to route it)", request.ifr_name);
  return n;
#else
  (void)c; (void)rx; (void)ctx;
  return NULL;
#endif
}

int br_net_send(br_net *n, const uint8_t *frame, size_t length)
{
  if (!n || length < 14 || length > FRAME_MAX) return -EINVAL;
  if (n->config.dhcp) {
    uint8_t reply[600];
    size_t size = br_dhcp_reply(&n->config, frame, length, reply, sizeof reply);
    if (size) { n->rx(n->ctx, reply, size); return 0; }
  }
  return write(n->fd, frame, length) < 0 ? -errno : 0;
}

void br_net_close(br_net *n)
{
  if (!n) return;
  __atomic_store_n(&n->running, false, __ATOMIC_RELEASE);
  (void)!write(n->wake[1], "", 1);         // the thread sits in poll()
  pthread_join(n->thread, NULL);
  close(n->wake[0]); close(n->wake[1]);
  close(n->fd);
  free(n);
}
