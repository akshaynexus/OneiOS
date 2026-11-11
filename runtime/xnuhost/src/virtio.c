// SPDX-License-Identifier: MIT
// virtio-mmio transport, version 2 (VirtIO 1.2, section 4.2), split virtqueues: the device side of
// what UniXNU's NativeVirtioBlock (https://github.com/UniXNU/VirtioMmioBlock, 4e1013e) and
// UniXNU-droidvm's virtio drivers (package/DroidVMVirtio*.c) program. crosvm is that device side in
// a DroidVM; here it is this file, in the same process as the kernel.
#include "xnuhost.h"
#include <stdlib.h>
#include <string.h>

#define MAGIC 0x74726976u
#define F_VERSION_1 (1ull << 32)
#define DESC_NEXT 1u
#define DESC_WRITE 2u
#define DESC_INDIRECT 4u

typedef struct {
  uint32_t num, ready;
  uint64_t desc, avail, used;
  uint16_t last_avail;
} vqueue;

struct xh_virtio {
  pthread_mutex_t lock;
  const xh_virtio_ops *ops;
  void *opaque;
  const char *name;
  unsigned spi;
  uint32_t status, isr, features_sel, driver_sel, queue_sel, generation;
  uint64_t driver_features;
  vqueue q[XH_VIRTIO_QUEUES];
};

void *(*xh_virtio_translate)(uint64_t pa, uint64_t len) = xh_ram;

void *xh_virtio_opaque(xh_virtio *d) { return d->opaque; }
void xh_virtio_lock(xh_virtio *d) { pthread_mutex_lock(&d->lock); }
void xh_virtio_unlock(xh_virtio *d) { pthread_mutex_unlock(&d->lock); }

static void raise_irq(xh_virtio *d, uint32_t bits)
{
  d->isr |= bits;
  xh_gic_set_spi(d->spi, true);
}

static void reset(xh_virtio *d)
{
  d->status = d->isr = d->features_sel = d->driver_sel = d->queue_sel = 0;
  d->driver_features = 0;
  memset(d->q, 0, sizeof d->q);
  xh_gic_set_spi(d->spi, false);
  if (d->ops->reset) d->ops->reset(d);
}

// Reads the chain starting at `head` into iov. Returns the count of parts, 0 for a bad chain.
static unsigned chain(xh_virtio *d, vqueue *q, uint16_t head, xh_viov *iov, unsigned max)
{
  struct desc { uint64_t addr; uint32_t len; uint16_t flags, next; } *table, e;
  unsigned n = 0, seen = 0, count = q->num;
  bool indirect = false;
  (void)d;
  table = xh_virtio_translate(q->desc, 16ull * q->num);
  if (!table) return 0;
  uint16_t i = head;
  for (;;) {
    if (i >= count || ++seen > count) return 0;
    memcpy(&e, &table[i], sizeof e);
    if (e.flags & DESC_INDIRECT) {
      if (indirect || (e.len % 16) || e.len == 0) return 0;      // one level only
      indirect = true;
      count = e.len / 16;
      table = xh_virtio_translate(e.addr, e.len);
      if (!table) return 0;
      i = 0; seen = 0;
      continue;
    }
    if (n == max) return 0;
    iov[n].data = e.len ? xh_virtio_translate(e.addr, e.len) : NULL;
    if (e.len && !iov[n].data) return 0;
    iov[n].length = e.len;
    iov[n].writable = e.flags & DESC_WRITE;
    n++;
    if (!(e.flags & DESC_NEXT)) break;
    i = e.next;
  }
  return n;
}

static bool pop(xh_virtio *d, unsigned qi, uint16_t *head, xh_viov *iov, unsigned max, unsigned *n)
{
  vqueue *q = &d->q[qi];
  if (!q->ready || !q->num) return false;
  uint16_t *avail = xh_virtio_translate(q->avail, 4 + 2ull * q->num);
  if (!avail) return false;
  uint16_t idx = __atomic_load_n(&avail[1], __ATOMIC_ACQUIRE);
  if (idx == q->last_avail) return false;
  *head = avail[2 + q->last_avail % q->num];
  q->last_avail++;
  *n = chain(d, q, *head, iov, max);
  return true;
}

static void push(xh_virtio *d, unsigned qi, uint16_t head, uint32_t written)
{
  vqueue *q = &d->q[qi];
  uint16_t *used = xh_virtio_translate(q->used, 4 + 8ull * q->num);
  if (!used) return;
  uint32_t *ring = (uint32_t *)(used + 2);
  uint16_t idx = used[1];
  ring[2 * (idx % q->num)] = head;
  ring[2 * (idx % q->num) + 1] = written;
  __atomic_store_n(&used[1], (uint16_t)(idx + 1), __ATOMIC_RELEASE);
  raise_irq(d, 1);
}

unsigned xh_virtio_pop(xh_virtio *d, unsigned qi, uint16_t *head, xh_viov *iov, unsigned max)
{
  unsigned n = 0;
  pthread_mutex_lock(&d->lock);
  bool got = qi < XH_VIRTIO_QUEUES && (d->status & 4) && pop(d, qi, head, iov, max, &n);
  if (got && n == 0) push(d, qi, *head, 0);          // a bad chain is returned unused
  pthread_mutex_unlock(&d->lock);
  return got ? n : 0;
}

void xh_virtio_push(xh_virtio *d, unsigned qi, uint16_t head, uint32_t written)
{
  pthread_mutex_lock(&d->lock);
  push(d, qi, head, written);
  pthread_mutex_unlock(&d->lock);
}

bool xh_virtio_ready(xh_virtio *d)
{
  pthread_mutex_lock(&d->lock);
  bool ready = d->status & 4;
  pthread_mutex_unlock(&d->lock);
  return ready;
}

void xh_virtio_config_changed(xh_virtio *d)
{
  pthread_mutex_lock(&d->lock);
  d->generation++;
  raise_irq(d, 2);
  pthread_mutex_unlock(&d->lock);
}

static void notify(xh_virtio *d, unsigned qi)
{
  xh_viov iov[64];
  uint16_t head;
  unsigned n;
  if (qi >= d->ops->nqueues || !(d->status & 4)) return;
  if (d->ops->manual & (1u << qi)) { if (d->ops->kick) d->ops->kick(d, qi); return; }
  while (pop(d, qi, &head, iov, 64, &n)) {
    int64_t written = n ? d->ops->handle(d, qi, head, iov, n) : 0;
    if (written >= 0) push(d, qi, head, (uint32_t)written);
  }
}

uint64_t xh_virtio_mmio_read(xh_virtio *d, uint64_t off, unsigned size)
{
  uint64_t v = 0;
  pthread_mutex_lock(&d->lock);
  vqueue *q = &d->q[d->queue_sel < XH_VIRTIO_QUEUES ? d->queue_sel : 0];
  if (off >= 0x100) {
    v = d->ops->config_read ? d->ops->config_read(d, (uint32_t)(off - 0x100), size) : 0;
    pthread_mutex_unlock(&d->lock);
    return v;
  }
  switch (off) {
  case 0x000: v = MAGIC; break;
  case 0x004: v = 2; break;
  case 0x008: v = d->ops->device_id; break;
  case 0x00c: v = 0x4f694f53; break;                 // vendor: "SOiO"
  case 0x010: v = d->features_sel == 0 ? (uint32_t)d->ops->features :
                  d->features_sel == 1 ? (uint32_t)((d->ops->features | F_VERSION_1) >> 32) : 0; break;
  case 0x034: v = d->queue_sel < d->ops->nqueues ? XH_VIRTIO_QSIZE : 0; break;
  case 0x044: v = q->ready; break;
  case 0x060: v = d->isr; break;
  case 0x070: v = d->status; break;
  case 0x0fc: v = d->generation; break;
  }
  pthread_mutex_unlock(&d->lock);
  return v;
}

static void set_half(uint64_t *value, bool high, uint32_t half)
{
  *value = high ? (*value & 0xffffffffull) | (uint64_t)half << 32 : (*value & ~0xffffffffull) | half;
}

void xh_virtio_mmio_write(xh_virtio *d, uint64_t off, unsigned size, uint64_t value)
{
  uint32_t v = (uint32_t)value;
  pthread_mutex_lock(&d->lock);
  vqueue *q = &d->q[d->queue_sel < XH_VIRTIO_QUEUES ? d->queue_sel : 0];
  if (off >= 0x100) {
    if (d->ops->config_write) d->ops->config_write(d, (uint32_t)(off - 0x100), size, v);
    pthread_mutex_unlock(&d->lock);
    return;
  }
  switch (off) {
  case 0x014: d->features_sel = v; break;
  case 0x020: if (d->driver_sel < 2) set_half(&d->driver_features, d->driver_sel, v); break;
  case 0x024: d->driver_sel = v; break;
  case 0x030: d->queue_sel = v; break;
  case 0x038: if (v && v <= XH_VIRTIO_QSIZE && !(v & (v - 1))) q->num = v; break;
  case 0x044: q->ready = v & 1; if (!q->ready) q->last_avail = 0; break;
  case 0x050: notify(d, v); break;
  case 0x064: d->isr &= ~v; if (!d->isr) xh_gic_set_spi(d->spi, false); break;
  case 0x070:
    if (v == 0) reset(d);
    else {
      // FEATURES_OK is refused unless the driver accepted VERSION_1 and nothing the device lacks.
      if ((v & 8) && (!(d->driver_features & F_VERSION_1) || (d->driver_features & ~(d->ops->features | F_VERSION_1)))) v &= ~8u;
      d->status = v;
    }
    break;
  case 0x080: set_half(&q->desc, false, v); break;
  case 0x084: set_half(&q->desc, true, v); break;
  case 0x090: set_half(&q->avail, false, v); break;
  case 0x094: set_half(&q->avail, true, v); break;
  case 0x0a0: set_half(&q->used, false, v); break;
  case 0x0a4: set_half(&q->used, true, v); break;
  }
  pthread_mutex_unlock(&d->lock);
}

static uint64_t region_read(void *opaque, uint64_t off, unsigned size) { return xh_virtio_mmio_read(opaque, off, size); }
static void region_write(void *opaque, uint64_t off, unsigned size, uint64_t value) { xh_virtio_mmio_write(opaque, off, size, value); }

xh_virtio *xh_virtio_create(uint64_t base, unsigned spi, const xh_virtio_ops *ops, void *opaque, const char *name)
{
  xh_virtio *d = calloc(1, sizeof *d);
  if (!d) return NULL;
  pthread_mutexattr_t attr;
  pthread_mutexattr_init(&attr);
  pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE);     // a handler may complete chains itself
  pthread_mutex_init(&d->lock, &attr);
  d->ops = ops;
  d->opaque = opaque;
  d->name = name;
  d->spi = spi;
  if (base && xh_mmio_add(&(xh_mmio_region){ .base = base, .size = 0x1000, .name = name, .opaque = d, .read = region_read, .write = region_write })) {
    free(d);
    return NULL;
  }
  return d;
}
