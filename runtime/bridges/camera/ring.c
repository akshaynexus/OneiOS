// SPDX-License-Identifier: MIT
// The frame ring: shared memory between the camera helper (writer) and xnuhost (reader). camera.h.
#define _GNU_SOURCE
#include "camera.h"
#include <errno.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

static size_t slot_span(uint32_t slot_bytes) { return CAM_RING_PAGE + (((size_t)slot_bytes + CAM_RING_PAGE - 1) & ~(size_t)(CAM_RING_PAGE - 1)); }
static struct cam_slot *slot_at(const cam_ring_map *m, unsigned slot)
{
  return (struct cam_slot *)(void *)((uint8_t *)m->ring + CAM_RING_PAGE + slot * slot_span(m->slot_bytes));
}
static uint8_t *slot_data(const cam_ring_map *m, unsigned slot) { return (uint8_t *)slot_at(m, slot) + CAM_RING_PAGE; }

int cam_ring_create(cam_ring_map *m, unsigned slots, uint32_t slot_bytes)
{
  memset(m, 0, sizeof *m);
  m->fd = -1;
  if (slots < 2 || slots > 255 || !slot_bytes) return -EINVAL;
  size_t size = CAM_RING_PAGE + slots * slot_span(slot_bytes);
  int fd = memfd_create("oneios-camera", MFD_CLOEXEC);
  if (fd < 0) return -errno;
  if (ftruncate(fd, (off_t)size)) { int e = errno; close(fd); return -e; }
  void *at = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
  if (at == MAP_FAILED) { int e = errno; close(fd); return -e; }
  m->ring = at;
  m->size = size;
  m->fd = fd;
  m->slots = slots;
  m->slot_bytes = slot_bytes;
  memcpy(m->ring->magic, CAM_RING_MAGIC, 8);
  m->ring->version = CAM_RING_VERSION;
  m->ring->slots = slots;
  m->ring->slot_bytes = slot_bytes;
  return 0;
}

int cam_ring_attach(cam_ring_map *m, int fd)
{
  struct stat st;
  memset(m, 0, sizeof *m);
  m->fd = -1;
  if (fstat(fd, &st)) return -errno;
  if (st.st_size < (off_t)(2 * CAM_RING_PAGE)) return -EINVAL;
  size_t size = (size_t)st.st_size;
  // Read-only: the reader never writes, and a wrong helper cannot be handed a way into xnuhost.
  void *at = mmap(NULL, size, PROT_READ, MAP_SHARED, fd, 0);
  if (at == MAP_FAILED) return -errno;
  struct cam_ring header;
  memcpy(&header, at, sizeof header);
  if (memcmp(header.magic, CAM_RING_MAGIC, 8) || header.version != CAM_RING_VERSION || header.slots < 2 || header.slots > 255 ||
      !header.slot_bytes || header.slot_bytes > (1u << 28) || CAM_RING_PAGE + header.slots * slot_span(header.slot_bytes) > size) {
    munmap(at, size);
    return -EINVAL;
  }
  m->ring = at;
  m->size = size;
  m->fd = fd;
  m->slots = header.slots;
  m->slot_bytes = header.slot_bytes;
  return 0;
}

void cam_ring_close(cam_ring_map *m)
{
  if (m->ring) munmap(m->ring, m->size);
  if (m->fd >= 0) close(m->fd);
  memset(m, 0, sizeof *m);
  m->fd = -1;
}

uint8_t *cam_ring_begin(cam_ring_map *m, unsigned *slot)
{
  unsigned at = m->next;
  m->next = (at + 1) % m->slots;
  struct cam_slot *s = slot_at(m, at);
  __atomic_add_fetch(&s->lock, 1, __ATOMIC_SEQ_CST);          // odd: being written
  *slot = at;
  return slot_data(m, at);
}

void cam_ring_commit(cam_ring_map *m, unsigned slot, const struct oc_frame *frame)
{
  struct cam_slot *s = slot_at(m, slot);
  s->frame = *frame;
  __atomic_add_fetch(&s->lock, 1, __ATOMIC_SEQ_CST);          // even: whole
  __atomic_store_n(&m->ring->latest, frame->sequence << 8 | slot, __ATOMIC_SEQ_CST);
}

uint64_t cam_ring_newest(const cam_ring_map *m) { return __atomic_load_n(&m->ring->latest, __ATOMIC_SEQ_CST) >> 8; }

int cam_ring_read(const cam_ring_map *m, uint64_t *last, struct oc_frame *frame, uint8_t *dst, size_t cap)
{
  for (unsigned tries = 0; tries < 4; tries++) {
    uint64_t latest = __atomic_load_n(&m->ring->latest, __ATOMIC_SEQ_CST);
    uint64_t sequence = latest >> 8;
    unsigned slot = (unsigned)(latest & 0xff);
    if (!latest || sequence == *last) return -EAGAIN;
    if (slot >= m->slots) return -EIO;
    const struct cam_slot *s = slot_at(m, slot);
    uint32_t before = __atomic_load_n(&s->lock, __ATOMIC_SEQ_CST);
    if (before & 1) continue;                                 // the writer came round to it: take the next newest
    struct oc_frame header = s->frame;
    if (header.magic != OC_FRAME_MAGIC || header.bytes > m->slot_bytes) {
      if (__atomic_load_n(&s->lock, __ATOMIC_SEQ_CST) != before) continue;
      return -EIO;
    }
    if (header.bytes > cap) return -EMSGSIZE;
    memcpy(dst, slot_data(m, slot), header.bytes);
    if (__atomic_load_n(&s->lock, __ATOMIC_SEQ_CST) != before) continue;      // torn: written again during the copy
    *frame = header;
    *last = header.sequence;
    return 0;
  }
  return -EAGAIN;
}
