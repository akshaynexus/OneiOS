// SPDX-License-Identifier: MIT
// The OneiOS camera device: one virtio-mmio device ("oneios,camera" in /arm-io) that carries the
// phone's camera frames from the host helper (runtime/bridges/camera, bin/oneios-camera) to the
// guest driver (drivers/camera). Freestanding C, included unchanged by the kernel side, by xnuhost
// and by the helper.
//
// Queue 0 (control): one chain per command: a readable struct oc_request, then a writable
// struct oc_reply. The host answers at once from what it knows; a camera that fails to start is
// reported in a frame's status.
// Queue 1 (frames): the guest posts writable buffers of OC_BUFFER_BYTES; the host fills one per
// frame: a struct oc_frame, then `bytes` of picture at offset OC_FRAME_DATA. A buffer with
// bytes == 0 and status != 0 says the camera stopped with that errno; one with bytes == 0 and
// status == 0 carries nothing (the guest posts it again).
// All values little-endian.
#ifndef ONEIOS_CAMERA_PROTO_H
#define ONEIOS_CAMERA_PROTO_H

#include <stdint.h>

#define OC_VIRTIO_DEVICE_ID 0x4f43u      // not a registered VirtIO id; the guest matches the node, not the id
#define OC_PROTO_VERSION    1u
#define OC_QUEUE_CONTROL    0u
#define OC_QUEUE_FRAME      1u

// The largest picture the transport carries (NV12: 12 bits a pixel).
#define OC_MAX_WIDTH        1920u
#define OC_MAX_HEIGHT       1080u
#define OC_FRAME_DATA       64u          // the picture's offset in a frame buffer
#define OC_FRAME_BYTES_MAX  (OC_MAX_WIDTH * OC_MAX_HEIGHT * 3u / 2u)
#define OC_BUFFER_BYTES     (OC_FRAME_DATA + OC_FRAME_BYTES_MAX)
#define OC_MAX_CAMERAS      8u

// Which camera. The S24 Ultra's five: S5KHP2 wide, S5K3LU front, IMX564 ultra-wide, IMX754 3x, IMX854 5x.
enum { OC_ROLE_NONE = 0, OC_ROLE_WIDE = 1, OC_ROLE_FRONT = 2, OC_ROLE_ULTRAWIDE = 3, OC_ROLE_TELE3 = 4,
       OC_ROLE_TELE5 = 5, OC_ROLE_OTHER = 6 };
enum { OC_FMT_NV12 = 1 };                // Y plane, then interleaved CbCr at half size; rows `stride` apart
enum { OC_FACING_FRONT = 0, OC_FACING_BACK = 1, OC_FACING_EXTERNAL = 2 };

enum { OC_CMD_INFO = 1, OC_CMD_START = 2, OC_CMD_STOP = 3 };
struct oc_request {
  uint32_t code;                         // OC_CMD_*
  uint32_t role;                         // START: OC_ROLE_*
  uint32_t width, height;                // START: the picture asked for (the nearest the camera has is used)
  uint32_t fps;                          // START: 0 = 30
  uint32_t reserved[3];
};

struct oc_camera {
  uint32_t role, facing;
  uint32_t orientation;                  // degrees the sensor is turned from the phone's upright position
  uint32_t max_width, max_height;        // the largest picture it gives within the transport's limit
  uint32_t max_fps;
  uint32_t focal_um;                     // focal length in micrometres
  uint32_t equiv35_mm;                   // 35 mm equivalent focal length, 0 = not known
};

struct oc_reply {
  uint32_t status;                       // 0, or an errno
  uint32_t count;                        // INFO: cameras that follow
  struct oc_camera cameras[OC_MAX_CAMERAS];
};

#define OC_FRAME_MAGIC 0x3146434fu       // "OCF1"
struct oc_frame {
  uint32_t magic;
  uint32_t status;                       // 0, or the errno the camera stopped with (then bytes is 0)
  uint64_t sequence;                     // rises by one a frame the camera made (a gap = frames the guest missed); never 0
  uint64_t timestamp_ns;                 // the sensor's time for the frame (host CLOCK_BOOTTIME or MONOTONIC)
  uint32_t width, height, stride;        // stride: bytes from one row to the next, both planes
  uint32_t format;                       // OC_FMT_NV12
  uint32_t bytes;                        // stride * height * 3 / 2
  uint32_t role;
  uint32_t orientation;
  uint32_t fps;                          // what the camera was asked for
  uint32_t reserved[2];
};
_Static_assert(sizeof(struct oc_request) == 32 && sizeof(struct oc_frame) == OC_FRAME_DATA &&
               sizeof(struct oc_reply) == 8 + 32 * OC_MAX_CAMERAS, "wire layout");

#endif
