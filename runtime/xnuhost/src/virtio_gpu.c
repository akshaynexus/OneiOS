// SPDX-License-Identifier: MIT
// virtio-gpu device (VirtIO 1.2, 5.7), 2D commands only: the scanout the guest's display driver
// presents frames on (UniXNU-droidvm's DroidVMVirtioGpuDisplay.cpp under IOMobileFramebufferLegacy).
// No virgl, no native context: Metal goes another way (docs/booter.md, "GPU").
#include "xnuhost.h"
#include <stdlib.h>
#include <string.h>

enum {
  CMD_GET_DISPLAY_INFO = 0x100, CMD_RESOURCE_CREATE_2D, CMD_RESOURCE_UNREF, CMD_SET_SCANOUT, CMD_RESOURCE_FLUSH,
  CMD_TRANSFER_TO_HOST_2D, CMD_RESOURCE_ATTACH_BACKING, CMD_RESOURCE_DETACH_BACKING,
  RESP_OK_NODATA = 0x1100, RESP_OK_DISPLAY_INFO,
  RESP_ERR_UNSPEC = 0x1200, RESP_ERR_OUT_OF_MEMORY, RESP_ERR_INVALID_SCANOUT_ID, RESP_ERR_INVALID_RESOURCE_ID,
  RESP_ERR_INVALID_CONTEXT_ID, RESP_ERR_INVALID_PARAMETER,
};
enum { FMT_BGRA = 1, FMT_BGRX = 2, FMT_ARGB = 3, FMT_XRGB = 4, FMT_RGBA = 67, FMT_XBGR = 68, FMT_ABGR = 121, FMT_RGBX = 134 };

typedef struct { uint32_t type, flags; uint64_t fence; uint32_t ctx; uint8_t ring, pad[3]; } gpu_hdr;
typedef struct { uint32_t x, y, w, h; } gpu_rect;
typedef struct { uint64_t addr; uint32_t length, pad; } gpu_entry;

#define MAX_RES 64
#define MAX_BACKING 4096
typedef struct {
  uint32_t id, format, width, height;
  uint8_t *pixels;                 // host copy, always B,G,R,X byte order
  gpu_entry *backing;
  unsigned nbacking;
} resource;

typedef struct {
  unsigned width, height;
  resource res[MAX_RES];
  uint32_t scanout_resource;
  bool first_frame;
} gpu_state;

static resource *find(gpu_state *g, uint32_t id)
{
  for (unsigned i = 0; id && i < MAX_RES; i++) if (g->res[i].id == id) return &g->res[i];
  return NULL;
}

static void release(resource *r)
{
  free(r->pixels);
  free(r->backing);
  memset(r, 0, sizeof *r);
}

// Byte order of a format's pixel in guest memory -> B,G,R,X.
static void convert(uint8_t *to, const uint8_t *from, uint32_t format, size_t pixels)
{
  switch (format) {
  case FMT_BGRA: case FMT_BGRX: memcpy(to, from, pixels * 4); break;
  case FMT_RGBA: case FMT_RGBX:
    for (size_t i = 0; i < pixels; i++, to += 4, from += 4) { to[0] = from[2]; to[1] = from[1]; to[2] = from[0]; to[3] = from[3]; }
    break;
  case FMT_ARGB: case FMT_XRGB:
    for (size_t i = 0; i < pixels; i++, to += 4, from += 4) { to[0] = from[3]; to[1] = from[2]; to[2] = from[1]; to[3] = from[0]; }
    break;
  default:                         // ABGR, XBGR
    for (size_t i = 0; i < pixels; i++, to += 4, from += 4) { to[0] = from[1]; to[1] = from[2]; to[2] = from[3]; to[3] = from[0]; }
  }
}

// Copies `length` bytes at linear `offset` of a resource's backing into `out`.
static bool backing_read(const resource *r, uint64_t offset, uint8_t *out, uint64_t length)
{
  for (unsigned i = 0; i < r->nbacking && length; i++) {
    uint64_t size = r->backing[i].length;
    if (offset >= size) { offset -= size; continue; }
    uint64_t n = size - offset < length ? size - offset : length;
    const uint8_t *p = xh_virtio_translate(r->backing[i].addr + offset, n);
    if (!p) return false;
    memcpy(out, p, n);
    out += n; length -= n; offset = 0;
  }
  return length == 0;
}

static bool rect_ok(const gpu_rect *r, uint32_t w, uint32_t h)
{
  return r->w && r->h && r->x <= w && r->y <= h && r->w <= w - r->x && r->h <= h - r->y;
}

static uint32_t command(gpu_state *g, const uint8_t *req, size_t size, uint8_t *resp, size_t resp_max, size_t *resp_size)
{
  const gpu_hdr *h = (const gpu_hdr *)req;
  const uint8_t *body = req + sizeof *h;
  size_t left = size - sizeof *h;
  *resp_size = sizeof *h;
  switch (h->type) {
  case CMD_GET_DISPLAY_INFO: {
    if (resp_max < sizeof *h + 16 * 24) return RESP_ERR_UNSPEC;
    memset(resp + sizeof *h, 0, 16 * 24);
    uint32_t mode[6] = { 0, 0, g->width, g->height, 1, 0 };
    memcpy(resp + sizeof *h, mode, sizeof mode);
    *resp_size = sizeof *h + 16 * 24;
    return RESP_OK_DISPLAY_INFO;
  }
  case CMD_RESOURCE_CREATE_2D: {
    uint32_t a[4];
    if (left < 16) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 16);
    if (!a[0] || find(g, a[0])) return RESP_ERR_INVALID_RESOURCE_ID;
    if (!a[2] || !a[3] || a[2] > 16384 || a[3] > 16384) return RESP_ERR_INVALID_PARAMETER;
    if (a[1] != FMT_BGRA && a[1] != FMT_BGRX && a[1] != FMT_ARGB && a[1] != FMT_XRGB && a[1] != FMT_RGBA &&
        a[1] != FMT_XBGR && a[1] != FMT_ABGR && a[1] != FMT_RGBX) return RESP_ERR_INVALID_PARAMETER;
    resource *r = NULL;
    for (unsigned i = 0; i < MAX_RES && !r; i++) if (!g->res[i].id) r = &g->res[i];
    if (!r) return RESP_ERR_OUT_OF_MEMORY;
    r->pixels = calloc((size_t)a[2] * a[3], 4);
    if (!r->pixels) return RESP_ERR_OUT_OF_MEMORY;
    r->id = a[0]; r->format = a[1]; r->width = a[2]; r->height = a[3];
    return RESP_OK_NODATA;
  }
  case CMD_RESOURCE_UNREF: {
    uint32_t id;
    if (left < 4) return RESP_ERR_INVALID_PARAMETER;
    memcpy(&id, body, 4);
    resource *r = find(g, id);
    if (!r) return RESP_ERR_INVALID_RESOURCE_ID;
    if (g->scanout_resource == id) g->scanout_resource = 0;
    release(r);
    return RESP_OK_NODATA;
  }
  case CMD_RESOURCE_ATTACH_BACKING: {
    uint32_t a[2];
    if (left < 8) return RESP_ERR_INVALID_PARAMETER;
    memcpy(a, body, 8);
    resource *r = find(g, a[0]);
    if (!r) return RESP_ERR_INVALID_RESOURCE_ID;
    if (!a[1] || a[1] > MAX_BACKING || left - 8 < (size_t)a[1] * sizeof(gpu_entry)) return RESP_ERR_INVALID_PARAMETER;
    gpu_entry *entries = malloc(a[1] * sizeof *entries);
    if (!entries) return RESP_ERR_OUT_OF_MEMORY;
    memcpy(entries, body + 8, a[1] * sizeof *entries);
    free(r->backing);
    r->backing = entries;
    r->nbacking = a[1];
    return RESP_OK_NODATA;
  }
  case CMD_RESOURCE_DETACH_BACKING: {
    uint32_t id;
    if (left < 4) return RESP_ERR_INVALID_PARAMETER;
    memcpy(&id, body, 4);
    resource *r = find(g, id);
    if (!r) return RESP_ERR_INVALID_RESOURCE_ID;
    free(r->backing);
    r->backing = NULL;
    r->nbacking = 0;
    return RESP_OK_NODATA;
  }
  case CMD_SET_SCANOUT: {
    gpu_rect rect;
    uint32_t a[2];
    if (left < 24) return RESP_ERR_INVALID_PARAMETER;
    memcpy(&rect, body, 16);
    memcpy(a, body + 16, 8);
    if (a[0] != 0) return RESP_ERR_INVALID_SCANOUT_ID;
    if (a[1] == 0) { g->scanout_resource = 0; return RESP_OK_NODATA; }      // scanout disabled
    resource *r = find(g, a[1]);
    if (!r) return RESP_ERR_INVALID_RESOURCE_ID;
    if (!rect_ok(&rect, r->width, r->height)) return RESP_ERR_INVALID_PARAMETER;
    g->scanout_resource = a[1];
    return RESP_OK_NODATA;
  }
  case CMD_TRANSFER_TO_HOST_2D: {
    gpu_rect rect;
    uint64_t offset;
    uint32_t id;
    if (left < 28) return RESP_ERR_INVALID_PARAMETER;
    memcpy(&rect, body, 16);
    memcpy(&offset, body + 16, 8);
    memcpy(&id, body + 24, 4);
    resource *r = find(g, id);
    if (!r) return RESP_ERR_INVALID_RESOURCE_ID;
    if (!rect_ok(&rect, r->width, r->height) || !r->backing) return RESP_ERR_INVALID_PARAMETER;
    uint8_t *row = malloc((size_t)rect.w * 4);
    if (!row) return RESP_ERR_OUT_OF_MEMORY;
    uint32_t status = RESP_OK_NODATA;
    for (uint32_t y = 0; y < rect.h; y++) {
      // `offset` is where the rectangle's first pixel is in the backing; rows follow at the resource's stride.
      if (!backing_read(r, offset + (uint64_t)y * r->width * 4, row, (uint64_t)rect.w * 4)) { status = RESP_ERR_INVALID_PARAMETER; break; }
      convert(r->pixels + ((size_t)(rect.y + y) * r->width + rect.x) * 4, row, r->format, rect.w);
    }
    free(row);
    return status;
  }
  case CMD_RESOURCE_FLUSH: {
    gpu_rect rect;
    uint32_t id;
    if (left < 20) return RESP_ERR_INVALID_PARAMETER;
    memcpy(&rect, body, 16);
    memcpy(&id, body + 16, 4);
    resource *r = find(g, id);
    if (!r) return RESP_ERR_INVALID_RESOURCE_ID;
    if (!rect_ok(&rect, r->width, r->height)) return RESP_ERR_INVALID_PARAMETER;
    xh_display *d = xh_display_get();
    if (id == g->scanout_resource && d && d->pixels) {
      uint32_t w = rect.x < d->width ? (rect.w < d->width - rect.x ? rect.w : d->width - rect.x) : 0;
      uint32_t hgt = rect.y < d->height ? (rect.h < d->height - rect.y ? rect.h : d->height - rect.y) : 0;
      for (uint32_t y = 0; y < hgt && w; y++)
        memcpy(d->pixels + (size_t)(rect.y + y) * d->stride + (size_t)rect.x * 4,
               r->pixels + ((size_t)(rect.y + y) * r->width + rect.x) * 4, (size_t)w * 4);
      if (w && hgt) xh_display_flush(rect.x, rect.y, w, hgt);
      if (!g->first_frame) { g->first_frame = true; xh_guest_ready(); }
    }
    return RESP_OK_NODATA;
  }
  }
  return RESP_ERR_UNSPEC;
}

static int64_t gpu_handle(xh_virtio *dev, unsigned queue, uint16_t head, xh_viov *iov, unsigned n)
{
  (void)head;
  gpu_state *g = xh_virtio_opaque(dev);
  // Gather the readable parts (the command), find the writable ones (the response).
  size_t req_size = 0, resp_max = 0;
  for (unsigned i = 0; i < n; i++) { if (iov[i].writable) resp_max += iov[i].length; else req_size += iov[i].length; }
  if (req_size < sizeof(gpu_hdr) || req_size > (1u << 20) || resp_max < sizeof(gpu_hdr)) return 0;
  uint8_t *req = malloc(req_size), *resp = calloc(1, resp_max < 4096 ? 4096 : resp_max);
  if (!req || !resp) { free(req); free(resp); return 0; }
  size_t at = 0;
  for (unsigned i = 0; i < n; i++) if (!iov[i].writable) { memcpy(req + at, iov[i].data, iov[i].length); at += iov[i].length; }
  size_t resp_size = sizeof(gpu_hdr);
  gpu_hdr out = { 0 };
  const gpu_hdr *in = (const gpu_hdr *)req;
  out.type = queue == 0 ? command(g, req, req_size, resp, resp_max, &resp_size) : RESP_OK_NODATA;     // cursor queue: accepted, not drawn
  if (in->flags & 1) { out.flags = 1; out.fence = in->fence; out.ctx = in->ctx; out.ring = in->ring; }
  memcpy(resp, &out, sizeof out);
  if (resp_size > resp_max) resp_size = resp_max;
  at = 0;
  for (unsigned i = 0; i < n && at < resp_size; i++) {
    if (!iov[i].writable) continue;
    size_t len = resp_size - at < iov[i].length ? resp_size - at : iov[i].length;
    memcpy(iov[i].data, resp + at, len);
    at += len;
  }
  free(req);
  free(resp);
  return (int64_t)resp_size;
}

static uint32_t gpu_config(xh_virtio *dev, uint32_t offset, unsigned size)
{
  (void)dev; (void)size;
  return offset == 8 ? 1 : 0;        // events_read, events_clear, num_scanouts = 1, num_capsets = 0
}

static void gpu_reset(xh_virtio *dev)
{
  gpu_state *g = xh_virtio_opaque(dev);
  for (unsigned i = 0; i < MAX_RES; i++) if (g->res[i].id) release(&g->res[i]);
  g->scanout_resource = 0;
}

static const xh_virtio_ops gpu_ops = { .device_id = 16, .features = 0, .nqueues = 2, .handle = gpu_handle,
                                       .config_read = gpu_config, .reset = gpu_reset };

xh_virtio *xh_virtio_gpu_create(uint64_t base, unsigned spi, unsigned width, unsigned height)
{
  gpu_state *g = calloc(1, sizeof *g);
  if (!g) return NULL;
  g->width = width;
  g->height = height;
  return xh_virtio_create(base, spi, &gpu_ops, g, "virtio-gpu");
}
