/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS camera, kernel side: the transport's API (oneios_camera.c, Mach universe) for the device
 * node (oneios_camera_dev.c, BSD universe), and the node's pure parts, which the host test runs
 * (tests/test_camera_dev.c).
 */
#ifndef ONEIOS_CAMERA_H
#define ONEIOS_CAMERA_H

#include <stdbool.h>
#include <stdint.h>
#include "oneios_camera_abi.h"
#include "oneios_camera_proto.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Brings up the "oneios,camera" device and makes /dev/oneios-camera. Called by oneios_drivers_start(). */
void oneios_camera_start(void);

/* ---- the transport (one caller at a time: the node holds its lock) ---- */
bool oneios_camera_up(void);
/* One command and its answer. 0, or EIO (no answer); the host's own verdict is reply->status. */
int  oneios_camera_command(const struct oc_request *request, struct oc_reply *reply);
/*
 * The newest frame the host has delivered: 0 with its header, its picture and the buffer's number
 * (give it back with oneios_camera_release); EAGAIN when there is none; else the errno the camera
 * stopped with. Older frames that arrived are given back to the host on the way.
 */
int  oneios_camera_poll(struct oc_frame *frame, const uint8_t **picture, unsigned *buffer);
void oneios_camera_release(unsigned buffer);
/* What a waiting reader sleeps on: the device's interrupt wakes it. */
extern int oneios_camera_event;
void oneios_camera_dev_attach(void);

/* ---- pure parts ---- */
struct oca_state {
	uint32_t sensor, width, height, fps;    /* what the next start asks for */
	bool started;                           /* a START was sent and no STOP since */
	bool restart;                           /* the selection changed: stop and start with the next read */
};

static inline void
oca_defaults(struct oca_state *s)
{
	s->sensor = ONEIOS_CAMERA_WIDE;
	s->width = 1280;
	s->height = 720;
	s->fps = 30;
	s->started = false;
	s->restart = false;
}

/* Applies a command from write(). 0, or an errno. */
static inline int
oca_command(struct oca_state *s, const struct oneios_camera_command *c)
{
	switch (c->command) {
	case ONEIOS_CAMERA_EXPOSURE:
	case ONEIOS_CAMERA_KEEP:
	case ONEIOS_CAMERA_FOCUS:
	case ONEIOS_CAMERA_MODE:
		return 0;
	case ONEIOS_CAMERA_SENSOR:
		if (c->a < ONEIOS_CAMERA_WIDE || c->a > ONEIOS_CAMERA_TELE5) {
			return 19;              /* ENODEV */
		}
		if (c->a != s->sensor) {
			s->sensor = c->a;
			s->restart = s->started;
		}
		return 0;
	case ONEIOS_CAMERA_FORMAT:
		if (c->a < 16 || c->b < 16 || (c->a & 1) || (c->b & 1) || c->a > OC_MAX_WIDTH || c->b > OC_MAX_HEIGHT || c->c > 240) {
			return 22;              /* EINVAL */
		}
		if (c->a != s->width || c->b != s->height || (c->c ? c->c : 30) != s->fps) {
			s->width = c->a;
			s->height = c->b;
			s->fps = c->c ? c->c : 30;
			s->restart = s->started;
		}
		return 0;
	default:
		return 22;
	}
}

static inline void
oca_request(const struct oca_state *s, struct oc_request *r)
{
	*r = (struct oc_request){ .code = OC_CMD_START, .role = s->sensor, .width = s->width, .height = s->height, .fps = s->fps };
}

/* Whether a frame from the host may be handed to a reader: its sizes agree and fit the buffer. */
static inline bool
oca_frame_ok(const struct oc_frame *f, uint32_t written)
{
	return f->magic == OC_FRAME_MAGIC && f->format == OC_FMT_NV12 && f->width >= 2 && f->height >= 2 &&
	       f->width <= OC_MAX_WIDTH && f->height <= OC_MAX_HEIGHT && f->stride >= f->width && f->stride <= OC_MAX_WIDTH &&
	       f->bytes == f->stride * f->height * 3 / 2 && f->bytes <= OC_FRAME_BYTES_MAX && written >= OC_FRAME_DATA &&
	       f->bytes <= written - OC_FRAME_DATA;
}

/* The header a reader gets for a frame. facing: of the camera, from the host's list. */
static inline void
oca_header(struct oneios_camera_frame *h, const struct oc_frame *f, uint32_t facing, uint64_t now)
{
	*h = (struct oneios_camera_frame){
		.magic = { 'N', 'C', 'F', 'R', 'A', 'M', 'E', '1' }, .header_bytes = sizeof(*h), .width = f->width, .height = f->height,
		.stride = f->stride, .format = ONEIOS_CAMERA_NV12, .orientation = f->orientation, .facing = facing, .sequence = f->sequence,
		.time = now, .gain = 256, .gain_unity = 256, .gain_max = 256, .fps = f->fps, .host_time_lo = (uint32_t)f->timestamp_ns,
		.host_time_hi = (uint32_t)(f->timestamp_ns >> 32), .sensor = f->role, .focus = ONEIOS_CAMERA_NO_FOCUS,
	};
}

#ifdef __cplusplus
}
#endif
#endif
