// SPDX-License-Identifier: MIT
// virtio-input device (VirtIO 1.2, 5.8): one multitouch touchscreen with the phone's side keys.
// virtio-input carries Linux evdev events unchanged, which is what the phone's /dev/input gives
// (input.c), and what UniXNU-droidvm's DroidVMVirtioInput.c turns into IOHID digitizer reports.
#include "xnuhost.h"
#include <stdlib.h>
#include <string.h>

enum { CFG_ID_NAME = 1, CFG_ID_SERIAL = 2, CFG_ID_DEVIDS = 3, CFG_PROP_BITS = 0x10, CFG_EV_BITS = 0x11, CFG_ABS_INFO = 0x12 };
enum { EV_SYN_ = 0, EV_KEY_ = 1, EV_ABS_ = 3 };
#define ABS_COUNT_ 0x40
#define BACKLOG 1024

typedef struct { uint16_t type, code; int32_t value; } input_event;
typedef struct {
  uint8_t select, subsel;
  struct { int32_t min, max, fuzz, flat, res; bool present; } abs[ABS_COUNT_];
  input_event backlog[BACKLOG];              // events the guest had no buffer for
  unsigned head, count;
} input_state;

static const uint16_t keys[] = { 114 /* KEY_VOLUMEDOWN */, 115 /* KEY_VOLUMEUP */, 116 /* KEY_POWER */, 0x14a /* BTN_TOUCH */ };

// The configuration space answer for (select, subsel): its size and bytes.
static unsigned config_payload(input_state *s, uint8_t out[128])
{
  memset(out, 0, 128);
  switch (s->select) {
  case CFG_ID_NAME: memcpy(out, "OneiOS touchscreen", 18); return 18;
  case CFG_ID_SERIAL: memcpy(out, "xnuhost-0", 9); return 9;
  case CFG_ID_DEVIDS: { uint16_t ids[4] = { 0x06 /* BUS_VIRTUAL */, 0x4f69, 0x0001, 1 }; memcpy(out, ids, 8); return 8; }
  case CFG_PROP_BITS: out[0] = 1 << 1; return 1;                 // INPUT_PROP_DIRECT
  case CFG_EV_BITS:
    if (s->subsel == EV_KEY_) {
      unsigned top = 0;
      for (unsigned i = 0; i < sizeof keys / sizeof *keys; i++) { out[keys[i] / 8] |= (uint8_t)(1 << (keys[i] % 8)); if (keys[i] / 8u + 1 > top) top = keys[i] / 8u + 1; }
      return top;
    }
    if (s->subsel == EV_ABS_) {
      unsigned top = 0;
      for (unsigned a = 0; a < ABS_COUNT_; a++) if (s->abs[a].present) { out[a / 8] |= (uint8_t)(1 << (a % 8)); top = a / 8 + 1; }
      return top;
    }
    return 0;
  case CFG_ABS_INFO:
    if (s->subsel < ABS_COUNT_ && s->abs[s->subsel].present) { memcpy(out, &s->abs[s->subsel], 20); return 20; }
    return 0;
  }
  return 0;
}

static uint32_t input_config_read(xh_virtio *dev, uint32_t offset, unsigned size)
{
  input_state *s = xh_virtio_opaque(dev);
  uint8_t config[136] = { 0 };
  config[0] = s->select;
  config[1] = s->subsel;
  config[2] = (uint8_t)config_payload(s, config + 8);
  uint32_t v = 0;
  if (offset < sizeof config && size <= sizeof config - offset) memcpy(&v, config + offset, size < 4 ? size : 4);
  return v;
}

static void input_config_write(xh_virtio *dev, uint32_t offset, unsigned size, uint32_t value)
{
  input_state *s = xh_virtio_opaque(dev);
  (void)size;
  if (offset == 0) s->select = (uint8_t)value;
  else if (offset == 1) s->subsel = (uint8_t)value;
}

// Moves backlog events into buffers the guest has posted on the event queue.
static void drain(xh_virtio *dev, input_state *s)
{
  while (s->count) {
    xh_viov iov[4];
    uint16_t head;
    unsigned n = xh_virtio_pop(dev, 0, &head, iov, 4);
    if (!n) return;
    if (iov[0].writable && iov[0].length >= 8) {
      memcpy(iov[0].data, &s->backlog[s->head], 8);
      s->head = (s->head + 1) % BACKLOG;
      s->count--;
      xh_virtio_push(dev, 0, head, 8);
    } else xh_virtio_push(dev, 0, head, 0);
  }
}

static int64_t input_handle(xh_virtio *dev, unsigned queue, uint16_t head, xh_viov *iov, unsigned n)
{
  (void)dev; (void)queue; (void)head; (void)iov; (void)n;
  return 0;                        // queue 1 carries status (LEDs), which a touchscreen ignores
}

// The guest posted event buffers: hand over what was waiting for them.
static void input_kick(xh_virtio *dev, unsigned queue)
{
  input_state *s = xh_virtio_opaque(dev);
  (void)queue;                     // called with the device's lock held
  if (s->count && s->backlog[(s->head + s->count - 1) % BACKLOG].type == EV_SYN_) drain(dev, s);
}

static const xh_virtio_ops input_ops = { .device_id = 18, .features = 0, .nqueues = 2, .handle = input_handle,
                                         .config_read = input_config_read, .config_write = input_config_write,
                                         .manual = 1, .kick = input_kick };

xh_virtio *xh_virtio_input_create(uint64_t base, unsigned spi)
{
  input_state *s = calloc(1, sizeof *s);
  if (!s) return NULL;
  return xh_virtio_create(base, spi, &input_ops, s, "virtio-input");
}

void xh_virtio_input_set_abs(xh_virtio *dev, unsigned axis, int32_t min, int32_t max, int32_t res)
{
  input_state *s = xh_virtio_opaque(dev);
  if (axis >= ABS_COUNT_) return;
  s->abs[axis].min = min; s->abs[axis].max = max; s->abs[axis].res = res; s->abs[axis].present = true;
}

void xh_virtio_input_event(xh_virtio *dev, uint16_t type, uint16_t code, int32_t value)
{
  input_state *s = xh_virtio_opaque(dev);
  xh_virtio_lock(dev);
  if (s->count == BACKLOG) { s->head = (s->head + 1) % BACKLOG; s->count--; }      // oldest lost
  s->backlog[(s->head + s->count++) % BACKLOG] = (input_event){ type, code, value };
  // Events are handed over at a report boundary, so the guest never sees half a touch frame.
  if (type == EV_SYN_) drain(dev, s);
  xh_virtio_unlock(dev);
}
