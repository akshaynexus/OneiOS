// SPDX-License-Identifier: MIT
// virtio-blk device (VirtIO 1.2, 5.2) over blk.c. A read-only disk offers VIRTIO_BLK_F_RO, which
// NativeVirtioBlock requires of the verified images.
#include "xnuhost.h"
#include <string.h>

#define F_RO (1ull << 5)
#define F_BLK_SIZE (1ull << 6)
#define F_FLUSH (1ull << 9)
enum { T_IN = 0, T_OUT = 1, T_FLUSH = 4, T_GET_ID = 8 };
enum { S_OK = 0, S_IOERR = 1, S_UNSUPP = 2 };

static int64_t blk_handle(xh_virtio *dev, unsigned queue, uint16_t head, xh_viov *iov, unsigned n)
{
  (void)queue; (void)head;
  xh_blk *disk = xh_virtio_opaque(dev);
  struct { uint32_t type, reserved; uint64_t sector; } req;
  // header (readable, 16 bytes), data parts, status (writable, 1 byte, last)
  if (n < 2 || iov[0].writable || iov[0].length != sizeof req || !iov[n - 1].writable || iov[n - 1].length < 1) return 0;
  memcpy(&req, iov[0].data, sizeof req);
  uint8_t status = S_OK;
  uint64_t offset = req.sector * 512, written = 0;
  if (req.sector > UINT64_MAX / 512) status = S_IOERR;
  for (unsigned i = 1; i + 1 < n && status == S_OK; i++) {
    switch (req.type) {
    case T_IN:
      if (!iov[i].writable || xh_blk_read(disk, offset, iov[i].data, iov[i].length)) status = S_IOERR;
      else written += iov[i].length;
      break;
    case T_OUT:
      if (iov[i].writable || xh_blk_write(disk, offset, iov[i].data, iov[i].length)) status = S_IOERR;
      break;
    case T_GET_ID:
      if (iov[i].writable && iov[i].length >= 20) { memset(iov[i].data, 0, 20); memcpy(iov[i].data, "oneios-xnuhost", 14); written += 20; }
      break;
    default: status = S_UNSUPP;
    }
    offset += iov[i].length;
  }
  if (req.type == T_FLUSH) status = xh_blk_flush(disk) ? S_IOERR : S_OK;
  else if (req.type != T_IN && req.type != T_OUT && req.type != T_GET_ID) status = S_UNSUPP;
  iov[n - 1].data[0] = status;
  return (int64_t)written + 1;
}

static uint32_t blk_config(xh_virtio *dev, uint32_t offset, unsigned size)
{
  struct { uint64_t capacity; uint32_t size_max, seg_max; uint8_t geometry[4]; uint32_t blk_size; uint8_t rest[40]; } c;
  memset(&c, 0, sizeof c);
  c.capacity = xh_blk_bytes(xh_virtio_opaque(dev)) / 512;
  c.seg_max = 62;
  c.blk_size = 512;
  uint32_t v = 0;
  if (offset < sizeof c && size <= sizeof c - offset) memcpy(&v, (uint8_t *)&c + offset, size < 4 ? size : 4);
  return v;
}

static const xh_virtio_ops blk_ro = { .device_id = 2, .features = F_RO | F_BLK_SIZE | F_FLUSH, .nqueues = 1, .handle = blk_handle, .config_read = blk_config };
static const xh_virtio_ops blk_rw = { .device_id = 2, .features = F_BLK_SIZE | F_FLUSH, .nqueues = 1, .handle = blk_handle, .config_read = blk_config };

xh_virtio *xh_virtio_blk_create(uint64_t base, unsigned spi, xh_blk *disk)
{
  return xh_virtio_create(base, spi, xh_blk_writable(disk) ? &blk_rw : &blk_ro, disk, "virtio-blk");
}
