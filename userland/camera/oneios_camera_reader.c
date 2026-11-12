/*
 * SPDX-License-Identifier: MIT
 *
 * The reader of the kernel's camera. oneios_camera_reader.h.
 */
#include "oneios_camera_reader.h"
#include "oneios_camera_proto.h"
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* The node opened when none is named: the kernel's, or a host test's stand-in (tests/host-mac.sh). */
#ifndef ONEIOS_CAMERA_NODE_PATH
#define ONEIOS_CAMERA_NODE_PATH ONEIOS_CAMERA_DEVICE
#endif

#define MAX_WIDTH  1920u
#define MAX_HEIGHT 1080u

#ifdef ONEIOS_CAMERA_IOKIT
/*
 * AppleH9CamIn's user client (drivers/camera/OneiOSCameraService.cpp): 0 INFO (out: count, reserved, the
 * cameras), 1 COMMAND (in: struct oneios_camera_command), 2 READ (in: scalar "do not wait"; out: the frame into the
 * structure buffer, scalar 0 the bytes given).
 */
#include <IOKit/IOKitLib.h>

struct uc_info {
	uint32_t count, reserved;
	struct oc_camera cameras[OC_MAX_CAMERAS];
};

static int
uc_errno(kern_return_t k)
{
	switch (k) {
	case KERN_SUCCESS: return 0;
	case kIOReturnTimeout: return EAGAIN;
	case kIOReturnBusy: case kIOReturnExclusiveAccess: return EBUSY;
	case kIOReturnNoDevice: return ENODEV;
	case kIOReturnNotAttached: return ENXIO;
	case kIOReturnBadArgument: return EINVAL;
	case kIOReturnAborted: return EINTR;
	case kIOReturnNotPermitted: case kIOReturnNotPrivileged: return EPERM;
	default: return EIO;
	}
}

static int
uc_open(oneios_camera_reader *r)
{
	io_service_t service = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("AppleH9CamIn"));
	io_connect_t connect = 0;
	kern_return_t k;

	if (!service) {
		return -ENOENT;
	}
	k = IOServiceOpen(service, mach_task_self(), 0, &connect);
	IOObjectRelease(service);
	if (k != KERN_SUCCESS) {
		return -(uc_errno(k) == EIO ? EPERM : uc_errno(k));
	}
	r->connect = connect;
	return 0;
}
#endif

int
oneios_camera_reader_open(oneios_camera_reader *r, const char *path)
{
	memset(r, 0, sizeof(*r));
	r->scratch_bytes = sizeof(struct oneios_camera_frame) + MAX_WIDTH * MAX_HEIGHT * 3 / 2;
	r->scratch = malloc(r->scratch_bytes);
	if (!r->scratch) {
		return -ENOMEM;
	}
	r->fd = -1;
#ifdef ONEIOS_CAMERA_IOKIT
	if (path == NULL && uc_open(r) == 0) {
		return 0;
	}
#endif
	r->fd = open(path ? path : ONEIOS_CAMERA_NODE_PATH, O_RDWR | O_CLOEXEC);
	if (r->fd < 0) {
		int e = errno;

		free(r->scratch);
		r->scratch = NULL;
		return -e;
	}
	return 0;
}

void
oneios_camera_reader_close(oneios_camera_reader *r)
{
	if (r->scratch) {
#ifdef ONEIOS_CAMERA_IOKIT
		if (r->connect) {
			IOServiceClose(r->connect);     /* the kernel stops the camera */
		}
#endif
		if (r->fd >= 0) {
			close(r->fd);           /* the kernel stops the camera */
		}
		free(r->scratch);
	}
	memset(r, 0, sizeof(*r));
	r->fd = -1;
}

static int
command(oneios_camera_reader *r, uint32_t what, uint32_t a, uint32_t b, uint32_t c)
{
	struct oneios_camera_command cmd = { .command = what, .a = a, .b = b, .c = c };
	ssize_t n;

#ifdef ONEIOS_CAMERA_IOKIT
	if (r->connect) {
		return -uc_errno(IOConnectCallStructMethod(r->connect, 1, &cmd, sizeof(cmd), NULL, NULL));
	}
#endif
	n = write(r->fd, &cmd, sizeof(cmd));

	return n == (ssize_t)sizeof(cmd) ? 0 : n < 0 ? -errno : -EIO;
}

int
oneios_camera_reader_select(oneios_camera_reader *r, uint32_t sensor, uint32_t width, uint32_t height, uint32_t fps)
{
	int e = command(r, ONEIOS_CAMERA_SENSOR, sensor, 0, 0);

	return e ? e : command(r, ONEIOS_CAMERA_FORMAT, width, height, fps);
}

unsigned
oneios_camera_reader_cameras(oneios_camera_reader *r, struct oc_camera *out, unsigned max)
{
#ifdef ONEIOS_CAMERA_IOKIT
	if (r->connect) {
		struct uc_info info;
		size_t size = sizeof(info);
		unsigned n;

		memset(&info, 0, sizeof(info));
		if (IOConnectCallStructMethod(r->connect, 0, NULL, 0, &info, &size) != KERN_SUCCESS || size < 8) {
			return 0;
		}
		n = info.count < max ? info.count : max;
		n = n < OC_MAX_CAMERAS ? n : OC_MAX_CAMERAS;
		memcpy(out, info.cameras, n * sizeof(out[0]));
		return n;
	}
#endif
	(void)r;
	(void)out;
	(void)max;
	return 0;
}

int
oneios_camera_header_check(const struct oneios_camera_frame *h, size_t got)
{
	if (got < sizeof(*h) || memcmp(h->magic, ONEIOS_CAMERA_MAGIC, 8) != 0 || h->header_bytes != sizeof(*h)) {
		return -EPROTO;
	}
	if (h->format != ONEIOS_CAMERA_NV12 || h->width < 2 || h->height < 2 || (h->width & 1) || (h->height & 1) ||
	    h->width > MAX_WIDTH || h->height > MAX_HEIGHT || h->stride < h->width || h->stride > MAX_WIDTH) {
		return -EPROTO;
	}
	/* The last chroma row may end at the picture's width, not the stride. */
	if (got - sizeof(*h) < (size_t)h->stride * (h->height * 3 / 2 - 1) + h->width) {
		return -EPROTO;
	}
	return 0;
}

int
oneios_camera_reader_next(oneios_camera_reader *r, struct oneios_camera_frame *header, const uint8_t **picture)
{
	ssize_t n;
	int e;

#ifdef ONEIOS_CAMERA_IOKIT
	if (r->connect) {
		uint64_t nowait = 0, given = 0;
		uint32_t outputs = 1;
		size_t room = r->scratch_bytes;
		kern_return_t k;

		do {
			k = IOConnectCallMethod(r->connect, 2, &nowait, 1, NULL, 0, &given, &outputs, r->scratch, &room);
		} while (k == kIOReturnAborted);
		if (k != KERN_SUCCESS) {
			return -uc_errno(k);
		}
		n = (ssize_t)given;
	} else
#endif
	do {
		n = read(r->fd, r->scratch, r->scratch_bytes);
	} while (n < 0 && errno == EINTR);
	if (n < 0) {
		return -errno;
	}
	if (n == 0) {
		return -EAGAIN;
	}
	memcpy(header, r->scratch, (size_t)n < sizeof(*header) ? (size_t)n : sizeof(*header));
	if ((e = oneios_camera_header_check(header, (size_t)n)) != 0) {
		return e;
	}
	if (r->frames && header->sequence > r->last_sequence + 1) {
		r->missed += header->sequence - r->last_sequence - 1;
	}
	r->last_sequence = header->sequence;
	r->frames++;
	*picture = r->scratch + sizeof(*header);
	return 0;
}

int
oneios_camera_copy_planes(const struct oneios_camera_frame *h, const uint8_t *picture, uint8_t *y, size_t y_stride, uint8_t *cbcr,
    size_t cbcr_stride, uint32_t width, uint32_t height)
{
	if (h->width != width || h->height != height || y_stride < width || cbcr_stride < width) {
		return -EINVAL;
	}
	for (uint32_t row = 0; row < height; row++) {
		memcpy(y + row * y_stride, picture + (size_t)row * h->stride, width);
	}
	const uint8_t *chroma = picture + (size_t)h->stride * height;

	for (uint32_t row = 0; row < height / 2; row++) {
		memcpy(cbcr + row * cbcr_stride, chroma + (size_t)row * h->stride, width);
	}
	return 0;
}
