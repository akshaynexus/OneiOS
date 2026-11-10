/*
 * SPDX-License-Identifier: MIT
 *
 * /dev/oneios-camera: the phone's cameras for iOS. What the capture plug-in (userland/camera) reads.
 *
 * The layout is the one of iOSPlus10's /dev/negronicam (userland/camera/negronicam.h there), which is
 * the one of HakiOS's /dev/hakios-camera (HakiOS userland/src/hakios.camera.h, MIT License, Copyright
 * (c) 2025-2026 the HakiOS authors): a 120-byte frame header with the magic "NCFRAME1", then the
 * frame, and a 24-byte command. So a reader written for those devices fits. What differs: the frames
 * are finished pictures (format 2, NV12: One UI's camera HAL has done the ISP's work), the fields of a
 * raw sensor are 0, and the commands select one of five cameras and a picture size.
 *
 *   open()   one client at a time (EBUSY). No camera is opened.
 *   read()   the first read starts the selected camera; each read waits (6 s for the first frame, 1 s
 *            after) for the newest frame not read yet and gives the header, then as many of the
 *            picture's bytes as fit. EAGAIN on a time-out, ENODEV when the host has no such camera,
 *            EIO or the host's errno when the camera stopped.
 *   write()  one struct oneios_camera_command. SENSOR, FORMAT: the camera restarts with the next read.
 *            EXPOSURE, KEEP, FOCUS, MODE: accepted and ignored (the HAL exposes and focuses).
 *   close()  stops the camera.
 */
#ifndef ONEIOS_CAMERA_ABI_H
#define ONEIOS_CAMERA_ABI_H

#include <stdint.h>

#define ONEIOS_CAMERA_DEVICE    "/dev/oneios-camera"
#define ONEIOS_CAMERA_MAGIC     "NCFRAME1"

enum { ONEIOS_CAMERA_RAW10 = 1, ONEIOS_CAMERA_NV12 = 2 };

struct oneios_camera_frame {
	char            magic[8];       /* ONEIOS_CAMERA_MAGIC */
	uint32_t        header_bytes;   /* this structure's size: the picture's bytes follow */
	uint32_t        width, height;  /* pixels */
	uint32_t        stride;         /* bytes from one row to the next, in both planes */
	uint32_t        format;         /* ONEIOS_CAMERA_NV12: Y rows, then interleaved CbCr rows of half height; full range, BT.601 */
	uint32_t        orientation;    /* (bayer) degrees the sensor is turned clockwise from the phone's upright position */
	uint32_t        facing;         /* (black) 0 front, 1 back */
	uint64_t        sequence;       /* rises by one a frame the camera made: a gap is frames nobody read */
	uint64_t        time;           /* when the frame reached the kernel, in mach_absolute_time()'s units */
	uint32_t        exposure_lines; /* 0 */
	uint32_t        line_ns;        /* 0 */
	uint32_t        gain;           /* 256 */
	uint32_t        gain_unity;     /* 256 */
	uint32_t        gain_max;       /* 256: no gain to set */
	uint32_t        fps;            /* (frame_lines) the rate the camera was asked for */
	uint32_t        host_time_lo;   /* (frame_lines_min) the sensor's own time for the frame, nanoseconds: low ... */
	uint32_t        host_time_hi;   /* (frame_lines_max) ... and high half. For the interval between frames */
	uint32_t        margin;         /* 0 */
	uint32_t        sensor;         /* which camera: ONEIOS_CAMERA_WIDE ... */
	uint32_t        focus;          /* ONEIOS_CAMERA_NO_FOCUS: the HAL focuses */
	uint32_t        mode;           /* 0 */
	uint32_t        focus_min, focus_max;   /* 0 */
	uint32_t        sensor_w, sensor_h;     /* 0: the whole sensor */
};
_Static_assert(sizeof(struct oneios_camera_frame) == 120, "the frame header is 120 bytes (NCFRAME1)");

#define ONEIOS_CAMERA_NO_FOCUS 0xffffffffu

/* The values of drivers/camera/oneios_camera_proto.h's OC_ROLE_*. 1 is the rear main camera, as in NCFRAME1. */
enum {
	ONEIOS_CAMERA_WIDE = 1,         /* S5KHP2, rear main */
	ONEIOS_CAMERA_FRONT = 2,        /* S5K3LU */
	ONEIOS_CAMERA_ULTRAWIDE = 3,    /* IMX564 */
	ONEIOS_CAMERA_TELE3 = 4,        /* IMX754, 3x */
	ONEIOS_CAMERA_TELE5 = 5,        /* IMX854, 5x */
};

enum {
	ONEIOS_CAMERA_EXPOSURE = 1,     /* ignored */
	ONEIOS_CAMERA_KEEP = 2,         /* ignored */
	ONEIOS_CAMERA_SENSOR = 3,       /* a: which camera (ONEIOS_CAMERA_WIDE ...); ENODEV for another value */
	ONEIOS_CAMERA_FOCUS = 4,        /* ignored */
	ONEIOS_CAMERA_MODE = 5,         /* ignored */
	ONEIOS_CAMERA_FORMAT = 6,       /* a: width, b: height, c: frames a second (0: 30); at most 1920 x 1080 */
};

struct oneios_camera_command {
	uint32_t        command;
	uint32_t        bytes;          /* of what follows this structure (0) */
	uint32_t        a, b, c, d;
};
_Static_assert(sizeof(struct oneios_camera_command) == 24, "the command is 24 bytes (NCFRAME1)");

#endif
