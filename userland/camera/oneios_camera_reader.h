/*
 * SPDX-License-Identifier: MIT
 *
 * The reader of the kernel's camera for the capture plug-in: the user client of AppleH9CamIn when built with
 * ONEIOS_CAMERA_IOKIT (the iOS build: cameracaptured's sandbox lets it open that user client), else
 * /dev/oneios-camera (drivers/camera/oneios_camera_abi.h). Both give the same frame:
 * selects a camera and a picture size, and reads one frame straight into the two planes of the
 * caller's pixel buffer (an IOSurface's, with its own bytes per row). Plain C: the host test runs it
 * against a file that stands in for the node.
 */
#ifndef ONEIOS_CAMERA_READER_H
#define ONEIOS_CAMERA_READER_H

#include <stddef.h>
#include <stdint.h>
#include "oneios_camera_abi.h"

typedef struct {
	int fd;                         /* the node, or -1 */
	unsigned connect;               /* the user client (io_connect_t), or 0 */
	uint8_t *scratch;               /* one frame as the node gives it: header, then the picture */
	size_t scratch_bytes;
	uint64_t last_sequence;
	uint64_t frames, missed;        /* read; made by the camera and never read (gaps in the sequence) */
} oneios_camera_reader;

/*
 * Opens the camera: `path` a node or a file that stands in for one; NULL: AppleH9CamIn's user client (iOS build),
 * then ONEIOS_CAMERA_DEVICE. 0, or -errno (-EBUSY: another client has the camera).
 */
int  oneios_camera_reader_open(oneios_camera_reader *r, const char *path);
void oneios_camera_reader_close(oneios_camera_reader *r);
/* Which camera (ONEIOS_CAMERA_WIDE ...) and what picture; fps 0 = 30. The camera starts with the next read. */
int  oneios_camera_reader_select(oneios_camera_reader *r, uint32_t sensor, uint32_t width, uint32_t height, uint32_t fps);
/*
 * Reads the newest frame (waits as the node does). 0 with *header and the picture at *picture (in the
 * reader's own memory, good until the next call); -EAGAIN when no frame came in time; -errno.
 */
int  oneios_camera_reader_next(oneios_camera_reader *r, struct oneios_camera_frame *header, const uint8_t **picture);
/*
 * Copies a frame's picture into a two-plane 4:2:0 pixel buffer of the same size: luma rows to `y`
 * (y_stride bytes apart), CbCr rows to `cbcr`. 0, or -EINVAL when the sizes do not agree.
 */
int  oneios_camera_copy_planes(const struct oneios_camera_frame *header, const uint8_t *picture, uint8_t *y, size_t y_stride,
    uint8_t *cbcr, size_t cbcr_stride, uint32_t width, uint32_t height);
/*
 * The host's cameras (the kernel's list, struct oc_camera of drivers/camera/oneios_camera_proto.h): how many were
 * written to `out` (at most `max`); 0 when the reader is a node (the node has no such call) or the list is empty.
 */
struct oc_camera;
unsigned oneios_camera_reader_cameras(oneios_camera_reader *r, struct oc_camera *out, unsigned max);
/* 0 when the header describes a picture this reader can hand on. */
int  oneios_camera_header_check(const struct oneios_camera_frame *header, size_t got);

#endif
