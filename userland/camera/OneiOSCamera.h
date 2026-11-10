/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOSCamera's frame pump, for the FigCaptureStream that will use it (OneiOSCamera.c, README.md).
 */
#ifndef ONEIOS_CAMERA_PLUGIN_H
#define ONEIOS_CAMERA_PLUGIN_H

#include <CoreVideo/CoreVideo.h>
#include <stdint.h>
#include "oneios_camera_abi.h"

/*
 * Called on the pump's thread for each frame: an IOSurface-backed 420f pixel buffer (retain it to keep
 * it) and the frame's header (sequence, the kernel's arrival time in mach_absolute_time units, the
 * sensor's own time, the camera, how it is turned).
 */
typedef void (*oneios_camera_frame_fn)(void *ctx, CVPixelBufferRef picture, const struct oneios_camera_frame *header);

/* Opens /dev/oneios-camera and starts the camera (ONEIOS_CAMERA_WIDE ...). 0, or an errno. One pump at a time. */
int  oneios_camera_pump_start(uint32_t sensor, uint32_t width, uint32_t height, uint32_t fps, oneios_camera_frame_fn fn, void *ctx);
/* Stops the camera; returns when no callback runs any more. */
void oneios_camera_pump_stop(void);

#endif
