/*
 * SPDX-License-Identifier: MIT
 *
 * The capture plug-in's shared parts (OneiOSCamera.c): its log and its off switch. The device: OneiOSCaptureDevice.c.
 */
#ifndef ONEIOS_CAMERA_PLUGIN_H
#define ONEIOS_CAMERA_PLUGIN_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "oneios_camera_abi.h"

/* One line: "ONEIOS CAMERA: ios: ..." to the unified log, the console (the kernel's log, xnuhost.log on the phone)
 * and /private/var/oneios/log/camera.log, each where the process may write. */
void oneios_camera_say(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
/* /private/var/oneios/camera/off exists: the plug-in makes no device. */
bool oneios_camera_off(void);

/* Scales a frame's NV12 picture into a two-plane 4:2:0 buffer of any size (nearest pixel; video_range: 420v's
 * 16-235/240 levels), or into a BGRA buffer. 0, or -EINVAL. (Exported for the host tests.) */
int oneios_camera_scale_nv12(const struct oneios_camera_frame *h, const uint8_t *picture, uint8_t *y, size_t y_stride, uint8_t *cbcr,
    size_t cbcr_stride, uint32_t width, uint32_t height, bool video_range);
int oneios_camera_scale_bgra(const struct oneios_camera_frame *h, const uint8_t *picture, uint8_t *bgra, size_t stride, uint32_t width,
    uint32_t height);

#endif
