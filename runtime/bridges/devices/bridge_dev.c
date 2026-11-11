// SPDX-License-Identifier: MIT
// The "oneios,bridge" device: the host end of drivers/bridge/oneios_bridge_proto.h. Messages from the
// guest are dispatched to the haptic and Bluetooth bridges; messages for the guest wait in a backlog
// until it has posted a buffer.
#include "bridges/devices.h"
#include "oneios_bridge_proto.h"
#include <stdlib.h>
#include <string.h>

#define BACKLOG 256u

typedef struct { struct ob_header header; uint8_t *payload; } pending;
typedef struct {
  xh_virtio *dev;
  br_bridge_config config;
  br_hci *hci;
  pending backlog[BACKLOG];
  unsigned head, count;
  uint64_t dropped;
} bridge_state;

// Writes one message into a guest buffer chain. Returns false when the chain cannot hold it.
static bool fill(xh_viov *iov, unsigned n, const struct ob_header *header, const uint8_t *payload, uint32_t *written)
{
  uint8_t message[OB_MAX_MESSAGE];
  size_t total = sizeof *header + header->length, done = 0;
  memcpy(message, header, sizeof *header);
  if (header->length) memcpy(message + sizeof *header, payload, header->length);
  for (unsigned i = 0; i < n && done < total; i++) {
    if (!iov[i].writable) continue;
    size_t part = total - done < iov[i].length ? total - done : iov[i].length;
    memcpy(iov[i].data, message + done, part);
    done += part;
  }
  *written = (uint32_t)done;
  return done == total;
}

// Called with the device's lock held.
static void drain(bridge_state *s)
{
  while (s->count) {
    xh_viov iov[4];
    uint16_t head;
    unsigned n = xh_virtio_pop(s->dev, OB_QUEUE_RX, &head, iov, 4);
    if (!n) return;
    pending *m = &s->backlog[s->head];
    uint32_t written = 0;
    if (!fill(iov, n, &m->header, m->payload, &written)) written = 0;      // too small a buffer: the message is lost
    xh_virtio_push(s->dev, OB_QUEUE_RX, head, written);
    free(m->payload);
    s->head = (s->head + 1) % BACKLOG;
    s->count--;
  }
}

static void to_guest(bridge_state *s, uint16_t channel, uint16_t type, const void *payload, size_t length)
{
  if (length > OB_MAX_PAYLOAD) { s->dropped++; return; }
  xh_virtio_lock(s->dev);
  if (s->count == BACKLOG) {                         // the oldest is lost
    free(s->backlog[s->head].payload);
    s->head = (s->head + 1) % BACKLOG;
    s->count--;
    s->dropped++;
  }
  pending *m = &s->backlog[(s->head + s->count) % BACKLOG];
  m->header = (struct ob_header){ channel, type, (uint32_t)length };
  m->payload = length ? malloc(length) : NULL;
  if (length && !m->payload) { s->dropped++; xh_virtio_unlock(s->dev); return; }
  if (length) memcpy(m->payload, payload, length);
  s->count++;
  drain(s);
  xh_virtio_unlock(s->dev);
}

static void hci_rx(void *ctx, uint8_t type, const uint8_t *packet, size_t length) { to_guest(ctx, OB_CH_HCI, type, packet, length); }

static void hci_state(void *ctx, int state)
{
  bridge_state *s = ctx;
  struct ob_hci_state message = { .state = (uint32_t)state };
  if (state == OB_HCI_STATE_READY)
    (void)br_bt_address(s->config.bt_address_path ? s->config.bt_address_path : "/mnt/vendor/efs/bluetooth/bt_addr", message.bd_addr);
  to_guest(s, OB_CH_HCI, OB_HCI_STATE, &message, sizeof message);
}

static void bright_state(bridge_state *s)
{
  br_bright_info info;
  br_bright_info_get(s->config.brightness, &info);
  // The lowest level's light: the floor's share of the range, of max_nits (the node is close to linear in light).
  uint64_t min_millinits = info.ceiling ? (uint64_t)info.max_nits * 1000u * info.floor / info.ceiling : 0;
  struct ob_bright_state state = { (info.panel ? OB_BRIGHT_HAS_PANEL : 0) | (info.als ? OB_BRIGHT_HAS_ALS : 0), info.max_nits, (uint32_t)min_millinits,
                                   br_bright_level(s->config.brightness) };
  to_guest(s, OB_CH_BRIGHTNESS, OB_BRIGHT_STATE, &state, sizeof state);
}

static void bright_als(void *ctx, const br_als_sample *sample)
{
  struct ob_bright_als als = { sample->millilux, sample->clear, sample->ir, sample->uv };
  to_guest(ctx, OB_CH_BRIGHTNESS, OB_BRIGHT_ALS, &als, sizeof als);
}

static void battery_send(void *ctx, const br_battery_sample *b)
{
  struct ob_battery_state state = { (b->present ? OB_BATTERY_PRESENT : 0) | (b->plugged ? OB_BATTERY_PLUGGED : 0) |
                                        (b->charging ? OB_BATTERY_CHARGING : 0) | (b->full ? OB_BATTERY_FULL : 0),
                                    b->percent, b->temp_deci_c, b->voltage_mv, b->current_ma, b->full_mah, b->design_mah, b->cycles, b->minutes };
  to_guest(ctx, OB_CH_BATTERY, OB_BATTERY_STATE, &state, sizeof state);
}

static void from_guest(bridge_state *s, const struct ob_header *h, const uint8_t *payload)
{
  switch (h->channel) {
  case OB_CH_CONTROL:
    if (h->type == OB_CTL_HELLO) {
      struct ob_ctl_hello hello = { OB_PROTO_VERSION, 1u << OB_CH_CONTROL | (s->config.haptic ? 1u << OB_CH_HAPTIC : 0) |
                                                       (s->config.hci_factory ? 1u << OB_CH_HCI : 0) |
                                                       (s->config.brightness ? 1u << OB_CH_BRIGHTNESS : 0) |
                                                       (s->config.battery ? 1u << OB_CH_BATTERY : 0) };
      to_guest(s, OB_CH_CONTROL, OB_CTL_HELLO, &hello, sizeof hello);
    }
    break;
  case OB_CH_HAPTIC:
    if (h->type == OB_HAPTIC_VIB && h->length >= sizeof(struct ob_haptic_vib)) {
      struct ob_haptic_vib vib;
      memcpy(&vib, payload, sizeof vib);
      br_haptic_vib(s->config.haptic, vib.on_ms, vib.intensity, vib.frequency_hz);
    } else if (h->type == OB_HAPTIC_STOP) br_haptic_stop(s->config.haptic);
    break;
  case OB_CH_HCI:
    if (!s->config.hci_factory) break;
    if (h->type == OB_HCI_OPEN) {
      if (!s->hci) s->hci = s->config.hci_factory(hci_rx, hci_state, s);
      // Failed after it was opened (the HAL died, or never finished initialising): a new client starts over.
      else if (br_hci_state(s->hci) == OB_HCI_STATE_FAILED) br_hci_close(s->hci);
      if (!s->hci || br_hci_open(s->hci)) hci_state(s, OB_HCI_STATE_FAILED);
      else if (br_hci_state(s->hci) == OB_HCI_STATE_READY) hci_state(s, OB_HCI_STATE_READY);   // already open: say so again
    } else if (h->type == OB_HCI_CLOSE) { if (s->hci) br_hci_close(s->hci); }
    else if (s->hci && h->type >= OB_HCI_COMMAND && h->type <= OB_HCI_ISO) (void)br_hci_send(s->hci, (uint8_t)h->type, payload, h->length);
    break;
  case OB_CH_BRIGHTNESS:
    if (!s->config.brightness) break;
    if (h->type == OB_BRIGHT_SET && h->length >= sizeof(struct ob_bright_set)) {
      struct ob_bright_set set;
      memcpy(&set, payload, sizeof set);
      br_bright_set(s->config.brightness, set.level, set.flags & OB_BRIGHT_LINEAR, set.fade_ms);
    } else if (h->type == OB_BRIGHT_QUERY) {
      bright_state(s);
      br_bright_listen(s->config.brightness, bright_als, s);      // light readings from now on
    }
    break;
  case OB_CH_BATTERY:
    if (s->config.battery && h->type == OB_BATTERY_QUERY) {
      br_battery_sample sample;
      (void)br_battery_query(s->config.battery, &sample);         // on a failed read: the last good sample
      battery_send(s, &sample);
      br_battery_listen(s->config.battery, battery_send, s);      // changes from now on
    }
    break;
  }
}

static int64_t bridge_handle(xh_virtio *dev, unsigned queue, uint16_t head, xh_viov *iov, unsigned n)
{
  bridge_state *s = xh_virtio_opaque(dev);
  uint8_t message[OB_MAX_MESSAGE];
  size_t size = 0;
  (void)head;
  if (queue != OB_QUEUE_TX) return 0;
  for (unsigned i = 0; i < n; i++) {
    if (iov[i].writable) continue;
    if (iov[i].length > sizeof message - size) return 0;        // longer than any message: ignored
    memcpy(message + size, iov[i].data, iov[i].length);
    size += iov[i].length;
  }
  struct ob_header header;
  if (size < sizeof header) return 0;
  memcpy(&header, message, sizeof header);
  if (header.length > size - sizeof header) return 0;
  from_guest(s, &header, message + sizeof header);
  return 0;
}

static void bridge_kick(xh_virtio *dev, unsigned queue) { (void)queue; drain(xh_virtio_opaque(dev)); }

static void bridge_reset(xh_virtio *dev)
{
  bridge_state *s = xh_virtio_opaque(dev);
  while (s->count) { free(s->backlog[s->head].payload); s->head = (s->head + 1) % BACKLOG; s->count--; }
  br_haptic_stop(s->config.haptic);                  // a guest that resets (or panics into a reset) leaves no motor running
  if (s->hci) br_hci_close(s->hci);
  if (s->config.battery) br_battery_listen(s->config.battery, NULL, NULL);  // no reads and no backlog for a guest that is gone
}

static uint32_t bridge_config_read(xh_virtio *dev, uint32_t offset, unsigned size)
{
  (void)dev; (void)size;
  return offset == 0 ? OB_PROTO_VERSION : 0;
}

static const xh_virtio_ops bridge_ops = { .device_id = OB_VIRTIO_DEVICE_ID, .nqueues = 2, .handle = bridge_handle, .config_read = bridge_config_read,
                                          .reset = bridge_reset, .manual = 1u << OB_QUEUE_RX, .kick = bridge_kick };

xh_virtio *br_bridge_create(uint64_t base, unsigned spi, const br_bridge_config *config)
{
  bridge_state *s = calloc(1, sizeof *s);
  if (!s) return NULL;
  s->config = *config;
  s->dev = xh_virtio_create(base, spi, &bridge_ops, s, "oneios-bridge");
  if (!s->dev) { free(s); return NULL; }
  return s->dev;
}
