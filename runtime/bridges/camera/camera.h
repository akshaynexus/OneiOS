// SPDX-License-Identifier: MIT
// The camera helper's inside: the frame ring both processes map, the messages between xnuhost and
// the helper, picture conversion, camera metadata, and the back ends that open a camera.
// docs/hardware.md section 7; README.md here.
#ifndef ONEIOS_CAMERA_H
#define ONEIOS_CAMERA_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "oneios_camera_proto.h"

// ---- the frame ring (ring.c) --------------------------------------------------------------------
// A memfd made by the helper and mapped by xnuhost. One page of header, then `slots` places of one
// page (lock, struct oc_frame) and `slot_bytes` of picture. The writer fills the place after the
// newest one and then names it in `latest`; a reader copies the newest and checks the place's lock
// (odd while it is written, changed when it was written again) to see that the copy is whole.
#define CAM_RING_MAGIC   "OCRING1"
#define CAM_RING_VERSION 1u
#define CAM_RING_PAGE    4096u
#define CAM_RING_SLOTS   4u
struct cam_ring {
  char magic[8];
  uint32_t version, slots, slot_bytes, reserved;
  uint64_t latest;                        // atomic: sequence << 8 | slot of the newest whole frame; 0 = none
};
struct cam_slot {
  uint32_t lock, reserved;                // atomic
  struct oc_frame frame;
};
typedef struct {
  struct cam_ring *ring;
  size_t size;
  int fd;
  uint32_t slots, slot_bytes;             // copied at create/attach: the mapping's fields are never trusted again
  uint32_t next;                          // writer: the place to fill next
} cam_ring_map;

int  cam_ring_create(cam_ring_map *m, unsigned slots, uint32_t slot_bytes);     // 0 or -errno
int  cam_ring_attach(cam_ring_map *m, int fd);                                  // maps and checks; keeps fd
void cam_ring_close(cam_ring_map *m);
// Writer: the place to fill (its picture area), then the commit that publishes it.
uint8_t *cam_ring_begin(cam_ring_map *m, unsigned *slot);
void cam_ring_commit(cam_ring_map *m, unsigned slot, const struct oc_frame *frame);
// Reader: copies the newest frame after *last (updated) into dst. 0; -EAGAIN nothing new;
// -EMSGSIZE it does not fit in cap; -EIO the ring's contents make no sense.
int  cam_ring_read(const cam_ring_map *m, uint64_t *last, struct oc_frame *frame, uint8_t *dst, size_t cap);
uint64_t cam_ring_newest(const cam_ring_map *m);                // the newest frame's sequence, 0 = none

// ---- xnuhost <-> helper (SOCK_SEQPACKET) --------------------------------------------------------
enum {
  CAM_MSG_HELLO = 1,      // helper -> xnuhost, with the ring's descriptor (SCM_RIGHTS); a = protocol version
  CAM_MSG_CAMERAS = 2,    // helper -> xnuhost: a = count, then struct oc_camera[count]
  CAM_MSG_START = 3,      // xnuhost -> helper: a = role, b = width, c = height, d = fps
  CAM_MSG_STOP = 4,       // xnuhost -> helper
  CAM_MSG_STATE = 5,      // helper -> xnuhost: status = errno (0 = running), a = running, b = width, c = height, d = fps
  CAM_MSG_FRAME = 6,      // helper -> xnuhost: a frame is in the ring; seq = its sequence
};
struct cam_msg { uint32_t kind, status, a, b, c, d; uint64_t seq; };

// ---- pictures (convert.c) -----------------------------------------------------------------------
// A YUV 4:2:0 picture as Android gives it: three planes with their own strides. c_step is the
// distance between two samples of one chroma plane in a row (1 planar, 2 interleaved).
typedef struct {
  const uint8_t *y, *cb, *cr;
  int y_stride, c_stride, c_step;
  int width, height;
} cam_yuv;
void cam_yuv_to_nv12(const cam_yuv *src, uint8_t *dst, int dst_stride);
// BT.601 full range (Android's JFIF data space for camera YUV), as iOS's 420f.
void cam_nv12_to_rgb(const uint8_t *nv12, int width, int height, int stride, uint8_t *rgb);
void cam_nv12_to_bgra(const uint8_t *nv12, int width, int height, int stride, uint8_t *bgra, int bgra_stride);
int  cam_png_write(const char *path, const uint8_t *rgb, int width, int height);   // 0 or -errno
uint32_t cam_crc32(uint32_t crc, const uint8_t *data, size_t length);

// ---- camera metadata (meta.c) -------------------------------------------------------------------
// Android's camera_metadata buffer (what the provider HAL returns as bytes), read without its library.
typedef struct { const uint8_t *data; uint32_t count; uint8_t type; } cam_meta_entry;
enum { CAM_META_BYTE = 0, CAM_META_INT32 = 1, CAM_META_FLOAT = 2, CAM_META_INT64 = 3, CAM_META_DOUBLE = 4, CAM_META_RATIONAL = 5 };
enum {
  CAM_TAG_AE_TARGET_FPS_RANGE = 0x10005, CAM_TAG_AE_AVAILABLE_FPS_RANGES = 0x10014,
  CAM_TAG_LENS_FACING = 0x80005, CAM_TAG_FOCAL_LENGTHS = 0x90002, CAM_TAG_CAPABILITIES = 0xc000c,
  CAM_TAG_STREAM_CONFIGURATIONS = 0xd000a, CAM_TAG_SENSOR_ORIENTATION = 0xe000e,
  CAM_TAG_PHYSICAL_SIZE = 0xf0005, CAM_TAG_PIXEL_ARRAY_SIZE = 0xf0006, CAM_TAG_BUFFER_MANAGEMENT = 0x150002,
};
int cam_meta_find(const uint8_t *buffer, size_t size, uint32_t tag, cam_meta_entry *entry);   // 0 or -ENOENT/-EINVAL
// Overwrites an int32 entry's values in place (same count). 0 or -errno.
int cam_meta_set_i32(uint8_t *buffer, size_t size, uint32_t tag, const int32_t *values, unsigned count);

#define CAM_MAX_SIZES 96
#define CAM_MAX_LIST  32
typedef struct {
  char id[32];                            // the camera's id for its back end
  char parent[32];                        // "" or, for a physical camera that is not listed by itself, the logical camera it is opened through
  uint32_t role;
  int facing, orientation, logical, hal_buffers;
  float focal_mm, physical_w, physical_h, equiv35;
  int pixel_w, pixel_h;
  int nsizes;
  struct { int w, h; } sizes[CAM_MAX_SIZES];    // YUV_420_888 outputs
  int nfps;
  struct { int lo, hi; } fps[32];
} cam_info;
void cam_info_from_meta(cam_info *info, const uint8_t *buffer, size_t size);
void cam_info_finish(cam_info *info);                        // equiv35 from the focal length and the sensor's size
// One camera per role, the others OC_ROLE_OTHER. A camera listed by itself comes before a logical one
// made of several, and that before a physical camera reached through its logical camera (`parent`).
void cam_assign_roles(cam_info *list, int count);
// The picture size to use for a request: the same size if the camera has it, else the largest with
// the same shape that is not larger, else the largest that fits the transport. 0 or -ENOENT.
int  cam_pick_size(const cam_info *info, int width, int height, int *out_w, int *out_h);
int  cam_pick_fps(const cam_info *info, int fps, int *lo, int *hi);   // the fixed range at fps, else the nearest
const char *cam_role_name(uint32_t role);
uint32_t cam_role_from_name(const char *name);
void cam_info_to_wire(const cam_info *info, struct oc_camera *out);

// ---- back ends ----------------------------------------------------------------------------------
typedef void (*cam_frame_fn)(void *ctx, const cam_yuv *picture, uint64_t timestamp_ns);
// The stream ended by itself. Called on a back end's thread: it must not call stop().
typedef void (*cam_error_fn)(void *ctx, int error);
typedef struct {
  const char *name;
  int  (*list)(cam_info *out, int max);                      // count or -errno
  int  (*start)(const cam_info *camera, int width, int height, int fps_lo, int fps_hi, cam_frame_fn frame, cam_error_fn error, void *ctx);
  void (*stop)(void);
} cam_backend;
extern const cam_backend cam_backend_test;                   // a moving test picture, no camera (host and phone)
#ifdef __ANDROID__
extern const cam_backend cam_backend_ndk;                    // Camera2 NDK through cameraserver
extern const cam_backend cam_backend_hal;                    // the provider HAL over binder, as cameraserver uses it
// Starts this process's binder threads once (4 at most): a plain executable has none, and both back
// ends are called back over binder (backend_ndk.c's header). 0 or -ENOSYS (no such platform symbol).
int cam_binder_threads(void);
// camctl --zoom: a zoom ratio put into the NDK request (0 = none). On a logical camera cameraserver
// then picks the lens; a second way to the telephotos when a physical stream is refused.
extern float cam_ndk_zoom;
#endif
bool cam_system_server_running(void);

// ---- the core: a back end writing into a ring (core.c) ------------------------------------------
typedef struct cam_core {
  const cam_backend *backend;
  cam_ring_map ring;
  cam_info cameras[CAM_MAX_LIST];
  int ncameras;
  // the running stream
  bool running;
  const cam_info *camera;
  int width, height, fps;
  uint64_t sequence;                                         // of the newest frame; goes on over a stop and start
  uint64_t frames;                                           // since the start
  uint64_t first_ns, last_ns;                                // sensor time of the first and the newest frame
  void (*on_frame)(void *ctx, uint64_t sequence);            // after each commit (a back end thread)
  void (*on_error)(void *ctx, int error);
  void *ctx;
} cam_core;
// backend: "auto" (NDK with system_server running, else the HAL), "ndk", "hal", "test".
int  cam_core_init(cam_core *core, const char *backend);     // makes the ring and lists the cameras
const cam_info *cam_core_find(const cam_core *core, const char *which);   // a role name, "id:<id>" or an index
int  cam_core_start(cam_core *core, const cam_info *camera, int width, int height, int fps);
void cam_core_stop(cam_core *core);
void cam_log(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
extern int cam_verbose;                                      // 0; 1 (-v) steps and the first callbacks; 2 (-vv) every callback and frame

#endif
