// SPDX-License-Identifier: MIT
// "oneios,camera": the virtio-mmio device that hands the camera helper's frames to the guest driver
// (drivers/camera, protocol in oneios_camera_proto.h). Queue 0 takes the guest's commands and
// answers them at once; queue 1 holds the frame buffers the guest posted: each frame is copied once,
// from the helper's ring into the guest's buffer, on the bridge's thread.
#include "bridges/devices.h"
#include "bridges/camera.h"
#include <errno.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
  xh_virtio *dev;
  br_camera *camera;
  uint32_t role, fps;                     // of the stream the guest asked for
  bool started;
  uint64_t frames, dropped;               // handed to the guest; made while the guest had no buffer posted
} cam_state;

static uint32_t cam_config_read(xh_virtio *dev, uint32_t offset, unsigned size)
{
  (void)dev; (void)size;
  return offset == 0 ? OC_PROTO_VERSION : 0;
}

// Completes one posted frame buffer: with the newest frame, or (status != 0) with only a header.
static void deliver(cam_state *s, int status)
{
  xh_viov iov[4];
  uint16_t head;
  struct oc_frame peek;
  if (!status && br_camera_frame(s->camera, &peek, NULL, 0) == -EAGAIN) return;      // nothing new: keep the guest's buffer
  unsigned n = xh_virtio_pop(s->dev, OC_QUEUE_FRAME, &head, iov, 4);
  if (!n) { if (!status) s->dropped++; return; }
  struct oc_frame frame = { .magic = OC_FRAME_MAGIC, .status = (uint32_t)status, .role = s->role, .fps = s->fps };
  uint32_t written = 0;
  if (iov[0].writable && iov[0].length >= OC_FRAME_DATA) {
    if (!status) {
      int rc = br_camera_frame(s->camera, &frame, iov[0].data + OC_FRAME_DATA, iov[0].length - OC_FRAME_DATA);
      if (rc == -EAGAIN) {                                    // written again during the copy, four times over
        // The transport has no way to put a chain back: the guest gets it empty and posts it again.
        frame = (struct oc_frame){ .magic = OC_FRAME_MAGIC, .role = s->role, .fps = s->fps };
      } else if (rc) {
        frame = (struct oc_frame){ .magic = OC_FRAME_MAGIC, .status = (uint32_t)-rc, .role = s->role, .fps = s->fps };
      } else s->frames++;
    }
    memcpy(iov[0].data, &frame, sizeof frame);
    written = OC_FRAME_DATA + frame.bytes;
  }
  xh_virtio_push(s->dev, OC_QUEUE_FRAME, head, written);
}

static void on_event(void *ctx, int kind, int status)
{
  cam_state *s = ctx;
  xh_virtio_lock(s->dev);
  if (kind == BR_CAMERA_FRAME) { if (s->started) deliver(s, 0); }
  else if (status) {                                          // the stream ended by itself
    if (s->started) deliver(s, status);
    s->started = false;
  }
  xh_virtio_unlock(s->dev);
}

static int64_t cam_handle(xh_virtio *dev, unsigned queue, uint16_t head, xh_viov *iov, unsigned n)
{
  cam_state *s = xh_virtio_opaque(dev);
  (void)head;
  if (queue != OC_QUEUE_CONTROL) return 0;
  struct oc_request request;
  struct oc_reply reply;
  xh_viov *out = NULL;
  size_t size = 0;
  memset(&request, 0, sizeof request);
  memset(&reply, 0, sizeof reply);
  for (unsigned i = 0; i < n; i++) {
    if (iov[i].writable) { if (!out) out = &iov[i]; continue; }
    size_t part = iov[i].length < sizeof request - size ? iov[i].length : sizeof request - size;
    memcpy((uint8_t *)&request + size, iov[i].data, part);
    size += part;
  }
  if (!out || out->length < 8 || size < 4) return 0;
  uint32_t written = 8;
  switch (request.code) {
  case OC_CMD_INFO: {
    // The helper lists the cameras when it starts; the guest's first question may come before it has.
    int count = br_camera_list(s->camera, reply.cameras, (int)OC_MAX_CAMERAS, 0);
    while (count > 0 && 8 + (uint32_t)count * sizeof(struct oc_camera) > out->length) count--;
    reply.count = (uint32_t)count;
    written = 8 + (uint32_t)count * (uint32_t)sizeof(struct oc_camera);
    break;
  }
  case OC_CMD_START:
    if (size < sizeof request || request.width > OC_MAX_WIDTH || request.height > OC_MAX_HEIGHT) { reply.status = EINVAL; break; }
    s->role = request.role;
    s->fps = request.fps ? request.fps : 30;
    reply.status = (uint32_t)-br_camera_start(s->camera, request.role, request.width, request.height, s->fps);
    s->started = reply.status == 0;
    break;
  case OC_CMD_STOP:
    s->started = false;
    reply.status = (uint32_t)-br_camera_stop(s->camera);
    break;
  default:
    reply.status = ENOTSUP;
  }
  memcpy(out->data, &reply, written);
  return written;
}

static void cam_kick(xh_virtio *dev, unsigned queue) { (void)dev; (void)queue; }

static void cam_reset(xh_virtio *dev)
{
  cam_state *s = xh_virtio_opaque(dev);
  if (s->started) (void)br_camera_stop(s->camera);            // a guest that restarts leaves no camera open
  s->started = false;
}

static const xh_virtio_ops cam_ops = { .device_id = OC_VIRTIO_DEVICE_ID, .nqueues = 2, .handle = cam_handle, .config_read = cam_config_read,
                                       .reset = cam_reset, .manual = 1u << OC_QUEUE_FRAME, .kick = cam_kick };

xh_virtio *br_camera_create(uint64_t base, unsigned spi, br_camera *camera)
{
  if (!camera) return NULL;
  cam_state *s = calloc(1, sizeof *s);
  if (!s) return NULL;
  s->camera = camera;
  s->dev = xh_virtio_create(base, spi, &cam_ops, s, "camera");
  if (!s->dev) { free(s); return NULL; }
  br_camera_listen(camera, on_event, s);
  return s->dev;
}
