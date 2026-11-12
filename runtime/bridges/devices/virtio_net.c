// SPDX-License-Identifier: MIT
// virtio-net (VirtIO 1.2, 5.1) for the guest's en0, as UniXNU-droidvm's DroidVMVirtioNet.c drives it:
// VERSION_1 and MAC only, so every packet is one 12-byte virtio_net_hdr_v1 and one frame. Queue 0
// receives (device to guest), queue 1 transmits. The frames go to a br_net back end (the tap of net/net.c) or to
// another one with its own send (the phone's wlan0: wifi/wlan.c).
#include "bridges/devices.h"
#include <stdlib.h>
#include <string.h>

#define NET_F_MAC 5
#define NET_HDR 12u
#define FRAME_MAX 1514u

typedef struct {
  xh_virtio *dev;
  br_net *net;
  br_net_send_fn send;
  uint8_t mac[6];
  uint64_t rx_dropped, tx_frames, rx_frames;
} net_state;

static uint32_t net_config_read(xh_virtio *dev, uint32_t offset, unsigned size)
{
  net_state *s = xh_virtio_opaque(dev);
  uint8_t config[12] = { 0 };                 // mac[6], status (link up), max_virtqueue_pairs
  memcpy(config, s->mac, 6);
  config[6] = 1;
  uint32_t v = 0;
  if (offset < sizeof config && size <= sizeof config - offset) memcpy(&v, config + offset, size < 4 ? size : 4);
  return v;
}

static int64_t net_handle(xh_virtio *dev, unsigned queue, uint16_t head, xh_viov *iov, unsigned n)
{
  net_state *s = xh_virtio_opaque(dev);
  uint8_t packet[NET_HDR + FRAME_MAX];
  size_t size = 0;
  (void)head;
  if (queue != 1) return 0;
  for (unsigned i = 0; i < n; i++) {
    if (iov[i].writable) continue;
    if (iov[i].length > sizeof packet - size) return 0;          // larger than a frame: dropped
    memcpy(packet + size, iov[i].data, iov[i].length);
    size += iov[i].length;
  }
  if (size > NET_HDR && s->net) { s->tx_frames++; (void)s->send(s->net, packet + NET_HDR, size - NET_HDR); }
  return 0;
}

// A frame for the guest (the back end's thread, or br_net_send answering DHCP on a vCPU).
static void net_rx(void *ctx, const uint8_t *frame, size_t length)
{
  net_state *s = ctx;
  xh_viov iov[4];
  uint16_t head;
  if (length > FRAME_MAX) return;
  xh_virtio_lock(s->dev);
  unsigned n = xh_virtio_pop(s->dev, 0, &head, iov, 4);
  if (!n) { s->rx_dropped++; xh_virtio_unlock(s->dev); return; }     // the guest has no buffer: as a full NIC ring
  uint8_t packet[NET_HDR + FRAME_MAX] = { 0 };
  packet[10] = 1;                              // num_buffers = 1
  memcpy(packet + NET_HDR, frame, length);
  size_t total = NET_HDR + length, done = 0;
  for (unsigned i = 0; i < n && done < total; i++) {
    if (!iov[i].writable) continue;
    size_t part = total - done < iov[i].length ? total - done : iov[i].length;
    memcpy(iov[i].data, packet + done, part);
    done += part;
  }
  if (done == total) s->rx_frames++; else { s->rx_dropped++; done = 0; }
  xh_virtio_push(s->dev, 0, head, (uint32_t)done);
  xh_virtio_unlock(s->dev);
}

static void net_kick(xh_virtio *dev, unsigned queue) { (void)dev; (void)queue; }

static const xh_virtio_ops net_ops = { .device_id = 1, .features = 1ull << NET_F_MAC, .nqueues = 2, .handle = net_handle,
                                       .config_read = net_config_read, .manual = 1, .kick = net_kick };

xh_virtio *br_virtio_net_create_backend(uint64_t base, unsigned spi, const br_net_config *config, br_net_open_fn net_open, br_net_send_fn net_send)
{
  net_state *s = calloc(1, sizeof *s);
  if (!s) return NULL;
  memcpy(s->mac, config->guest_mac, 6);
  s->send = net_send ? net_send : br_net_send;
  s->dev = xh_virtio_create(base, spi, &net_ops, s, "virtio-net");
  if (!s->dev) { free(s); return NULL; }
  s->net = net_open ? net_open(config, net_rx, s) : NULL;            // without a back end the link is up and silent
  return s->dev;
}

xh_virtio *br_virtio_net_create(uint64_t base, unsigned spi, const br_net_config *config, br_net_open_fn net_open)
{
  return br_virtio_net_create_backend(base, spi, config, net_open, NULL);
}
