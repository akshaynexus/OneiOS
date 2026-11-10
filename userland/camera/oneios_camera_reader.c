/*
 * SPDX-License-Identifier: MIT
 *
 * The reader of /dev/oneios-camera. oneios_camera_reader.h.
 */
#include "oneios_camera_reader.h"
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define MAX_WIDTH  1920u
#define MAX_HEIGHT 1080u

int
oneios_camera_reader_open(oneios_camera_reader *r, const char *path)
{
	memset(r, 0, sizeof(*r));
	r->scratch_bytes = sizeof(struct oneios_camera_frame) + MAX_WIDTH * MAX_HEIGHT * 3 / 2;
	r->scratch = malloc(r->scratch_bytes);
	if (!r->scratch) {
		return -ENOMEM;
	}
	r->fd = open(path ? path : ONEIOS_CAMERA_DEVICE, O_RDWR | O_CLOEXEC);
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
		close(r->fd);           /* the kernel stops the camera */
		free(r->scratch);
	}
	memset(r, 0, sizeof(*r));
	r->fd = -1;
}

static int
command(oneios_camera_reader *r, uint32_t what, uint32_t a, uint32_t b, uint32_t c)
{
	struct oneios_camera_command cmd = { .command = what, .a = a, .b = b, .c = c };
	ssize_t n = write(r->fd, &cmd, sizeof(cmd));

	return n == (ssize_t)sizeof(cmd) ? 0 : n < 0 ? -errno : -EIO;
}

int
oneios_camera_reader_select(oneios_camera_reader *r, uint32_t sensor, uint32_t width, uint32_t height, uint32_t fps)
{
	int e = command(r, ONEIOS_CAMERA_SENSOR, sensor, 0, 0);

	return e ? e : command(r, ONEIOS_CAMERA_FORMAT, width, height, fps);
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
