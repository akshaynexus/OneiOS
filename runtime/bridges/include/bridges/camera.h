// SPDX-License-Identifier: MIT
// The camera bridge as xnuhost uses it (runtime/bridges/camera): the helper process that opens the
// phone's cameras, and the "oneios,camera" virtio device that hands its frames to the guest.
// docs/hardware.md section 7; runtime/bridges/camera/README.md.
#ifndef ONEIOS_BRIDGE_CAMERA_H
#define ONEIOS_BRIDGE_CAMERA_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "oneios_camera_proto.h"

typedef struct br_camera br_camera;
enum { BR_CAMERA_FRAME = 0, BR_CAMERA_STATE = 1 };
// Called on the bridge's own thread: a frame is ready (status 0), or the stream's state changed
// (status 0 = running or stopped as asked, else the errno it ended with; EPIPE: the helper is gone).
typedef void (*br_camera_event_fn)(void *ctx, int kind, int status);

// Starts the helper (bin/oneios-camera) with `backend` (NULL = auto). Returns at once: the helper's
// camera list arrives later. NULL (with a log line) when it cannot be started.
br_camera *br_camera_helper(const char *path, const char *backend);
void br_camera_listen(br_camera *camera, br_camera_event_fn fn, void *ctx);
// The cameras the helper found (one per role). Waits up to wait_ms for the helper's list.
int  br_camera_list(br_camera *camera, struct oc_camera *out, int max, int wait_ms);
// Ask the helper to open or close a camera. They do not wait: the result is a BR_CAMERA_STATE event.
int  br_camera_start(br_camera *camera, uint32_t role, uint32_t width, uint32_t height, uint32_t fps);
int  br_camera_stop(br_camera *camera);
// Copies the newest frame not read yet: its header to *frame, the picture to dst. 0, -EAGAIN (none),
// -EMSGSIZE (larger than cap), -EIO.
int  br_camera_frame(br_camera *camera, struct oc_frame *frame, uint8_t *dst, size_t cap);
// Closes the camera and ends the helper. Does not wait for the helper longer than it takes to close its socket.
void br_camera_close(br_camera *camera);

#ifdef ONEIOS_BRIDGE_DEVICES_H
// "oneios,camera": the device of drivers/camera/oneios_camera_proto.h. Device tree: /arm-io/camera,
// compatible "oneios,camera", one window and SPI, as the bridge channel.
xh_virtio *br_camera_create(uint64_t base, unsigned spi, br_camera *camera);
#endif

#endif
