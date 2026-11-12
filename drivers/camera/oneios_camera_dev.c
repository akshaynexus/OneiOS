/*
 * SPDX-License-Identifier: MIT
 *
 * /dev/oneios-camera: the node the capture plug-in reads (oneios_camera_abi.h: the NCFRAME1 layout
 * of iOSPlus10's /dev/negronicam and HakiOS's /dev/hakios-camera, with finished NV12 pictures).
 * The frames come from the host through oneios_camera.c; this file is the BSD side: one client,
 * read() = the newest frame, write() = which camera and what size.
 *
 * read() copies the picture out of the transport's buffer (mmap of a character device gives ENODEV
 * in this XNU, kern_mman.c); the buffer goes back to the host after the copy, so a picture is never
 * torn. The plug-in puts it into an IOSurface (userland/camera).
 *
 * The node's work is also the session API of oneios_camera.h (oneios_camera_session_*): the IOKit
 * user client of AppleH9CamIn (OneiOSCameraService.cpp) uses it, because cameracaptured's sandbox
 * lets that process open AppleH9CamInUserClient and not, as far as is known, a /dev node. One
 * client at a time, whichever way it comes.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/errno.h>
#include <sys/conf.h>
#include <sys/uio.h>
#include <sys/fcntl.h>
#include <sys/proc.h>
#include <sys/vnode.h>                  /* IO_NDELAY */
#include <miscfs/devfs/devfs.h>
#include <kern/locks.h>
#include <kern/clock.h>
#include "oneios_camera.h"

#define OCD_FIRST_MS    6000            /* the camera opens: sensor power, the HAL's pipeline */
#define OCD_NEXT_MS     1000
#define OCD_TICK_MS     10              /* the reader looks this often even without the interrupt */

static LCK_GRP_DECLARE(ocd_grp, "oneios_camera");
static LCK_MTX_DECLARE(ocd_lock, &ocd_grp);

static struct {
	bool open;
	bool first;                     /* no frame since the start: the long wait applies */
	void *node;
	struct oca_state state;
	uint32_t ncameras;
	struct oc_camera cameras[OC_MAX_CAMERAS];
	uint64_t frames;
} d;

static uint32_t
ocd_facing(uint32_t role)
{
	for (uint32_t i = 0; i < d.ncameras; i++) {
		if (d.cameras[i].role == role) {
			return d.cameras[i].facing;
		}
	}
	return role == ONEIOS_CAMERA_FRONT ? OC_FACING_FRONT : OC_FACING_BACK;
}

/* The host's list: asked again while it is empty (the helper may list its cameras after the guest started). */
static void
ocd_list(void)
{
	struct oc_request ask = { .code = OC_CMD_INFO };
	struct oc_reply reply;

	if (d.ncameras == 0 && oneios_camera_command(&ask, &reply) == 0 && reply.status == 0 && reply.count <= OC_MAX_CAMERAS) {
		memcpy(d.cameras, reply.cameras, sizeof(d.cameras));
		d.ncameras = reply.count;
	}
}

static void
ocd_stop(void)
{
	struct oc_request stop = { .code = OC_CMD_STOP };
	struct oc_reply reply;

	if (d.state.started) {
		(void)oneios_camera_command(&stop, &reply);
		d.state.started = false;
	}
	d.state.restart = false;
}

static int
ocd_start(void)
{
	struct oc_request start;
	struct oc_reply reply;
	struct oc_frame stale;
	const uint8_t *picture;
	unsigned buffer;

	/* Frames of the stream before, if any, are not this one's. */
	while (oneios_camera_poll(&stale, &picture, &buffer) == 0) {
		oneios_camera_release(buffer);
	}
	ocd_list();
	oca_request(&d.state, &start);
	if (oneios_camera_command(&start, &reply)) {
		return EIO;
	}
	if (reply.status) {
		return (int)reply.status;
	}
	d.state.started = true;
	d.state.restart = false;
	d.first = true;
	return 0;
}

int
oneios_camera_session_open(void)
{
	int e = 0;

	lck_mtx_lock(&ocd_lock);
	if (d.open) {
		e = EBUSY;
	} else if (!oneios_camera_up()) {
		e = ENXIO;
	} else {
		d.open = true;
		oca_defaults(&d.state);
	}
	lck_mtx_unlock(&ocd_lock);
	return e;
}

void
oneios_camera_session_close(void)
{
	lck_mtx_lock(&ocd_lock);
	ocd_stop();                     /* the camera is closed on the host: no light, no power */
	d.open = false;
	lck_mtx_unlock(&ocd_lock);
}

int
oneios_camera_session_read(oneios_camera_copy_fn copy, void *ctx, size_t room, bool nowait, size_t *given)
{
	struct oneios_camera_frame header;
	struct oc_frame frame;
	const uint8_t *picture = NULL;
	unsigned buffer = 0;
	int e = 0, waited = 0;

	*given = 0;
	if (room < sizeof(header)) {
		return EINVAL;
	}
	lck_mtx_lock(&ocd_lock);
	if (!d.open) {
		lck_mtx_unlock(&ocd_lock);
		return ENXIO;
	}
	if (d.state.restart) {
		ocd_stop();
	}
	if (!d.state.started) {
		e = ocd_start();
	}
	while (!e) {
		e = oneios_camera_poll(&frame, &picture, &buffer);
		if (e != EAGAIN) {
			break;                  /* a frame (0), or the camera stopped */
		}
		if (nowait) {
			break;
		}
		if (waited >= (d.first ? OCD_FIRST_MS : OCD_NEXT_MS)) {
			break;                  /* EAGAIN: the reader may try again */
		}
		struct timespec ts = { 0, OCD_TICK_MS * 1000 * 1000 };
		e = msleep(&oneios_camera_event, &ocd_lock, PZERO | PCATCH, "ocamrd", &ts);
		e = e == EWOULDBLOCK ? 0 : e;
		waited += OCD_TICK_MS;
		if (!d.open || !d.state.started) {
			e = e ? e : EIO;
		}
	}
	if (e && e != EAGAIN && e != EINTR && e != ERESTART) {
		d.state.started = false;        /* the host closed the camera: the next read starts it again */
	}
	if (!e) {
		size_t bytes = MIN((size_t)frame.bytes, room - sizeof(header));

		d.first = false;
		d.frames++;
		oca_header(&header, &frame, ocd_facing(frame.role), mach_absolute_time());
		e = copy(ctx, &header, sizeof(header), 0);
		if (!e && bytes > 0) {
			e = copy(ctx, picture, bytes, sizeof(header));
		}
		if (!e) {
			*given = sizeof(header) + bytes;
		}
		oneios_camera_release(buffer);
	}
	lck_mtx_unlock(&ocd_lock);
	return e;
}

int
oneios_camera_session_command(const struct oneios_camera_command *command)
{
	int e;

	lck_mtx_lock(&ocd_lock);
	e = oca_command(&d.state, command);
	if (!e && command->command == ONEIOS_CAMERA_SENSOR) {
		ocd_list();
		if (d.ncameras) {               /* known cameras: one the host does not have is refused now */
			bool have = false;

			for (uint32_t i = 0; i < d.ncameras; i++) {
				have = have || d.cameras[i].role == command->a;
			}
			e = have ? 0 : ENODEV;
		}
	}
	lck_mtx_unlock(&ocd_lock);
	return e;
}

uint32_t
oneios_camera_session_cameras(struct oc_camera *cameras, uint32_t max)
{
	uint32_t n;

	lck_mtx_lock(&ocd_lock);
	ocd_list();
	n = MIN(d.ncameras, max);
	memcpy(cameras, d.cameras, n * sizeof(cameras[0]));
	lck_mtx_unlock(&ocd_lock);
	return n;
}

static int
ocd_open(dev_t dev __unused, int flags __unused, int devtype __unused, struct proc *p __unused)
{
	return oneios_camera_session_open();
}

static int
ocd_close(dev_t dev __unused, int flags __unused, int devtype __unused, struct proc *p __unused)
{
	oneios_camera_session_close();
	return 0;
}

/* read() in order: `at` is where the uio already is. */
static int
ocd_uio_copy(void *ctx, const void *src, size_t bytes, size_t at __unused)
{
	return uiomove((const char *)src, (int)bytes, (struct uio *)ctx);
}

static int
ocd_read(dev_t dev __unused, struct uio *uio, int ioflag)
{
	size_t given;

	if (uio_resid(uio) < (user_ssize_t)sizeof(struct oneios_camera_frame)) {
		return EINVAL;
	}
	return oneios_camera_session_read(ocd_uio_copy, uio, (size_t)uio_resid(uio), (ioflag & IO_NDELAY) != 0, &given);
}

static int
ocd_write(dev_t dev __unused, struct uio *uio, int ioflag __unused)
{
	/* Zeroed: uiomove fills it, but Xcode 27.2's clang cannot see that (-Wuninitialized-const-pointer, -Werror). */
	struct oneios_camera_command command = { 0 };
	int e;

	if (uio_resid(uio) < (user_ssize_t)sizeof(command)) {
		return EINVAL;
	}
	if ((e = uiomove((char *)&command, (int)sizeof(command), uio)) != 0) {
		return e;
	}
	return oneios_camera_session_command(&command);
}

static const struct cdevsw ocd_cdevsw = {
	.d_open = ocd_open,
	.d_close = ocd_close,
	.d_read = ocd_read,
	.d_write = ocd_write,
	.d_ioctl = eno_ioctl,
	.d_stop = eno_stop,
	.d_reset = eno_reset,
	.d_ttys = NULL,
	.d_select = eno_select,
	.d_mmap = eno_mmap,
	.d_strategy = eno_strat,
	.d_type = 0,
};

void
oneios_camera_dev_attach(void)
{
	int major = cdevsw_add(-1, &ocd_cdevsw);

	if (major < 0) {
		printf("ONEIOS CAMERA: cdevsw_add failed: no /dev/oneios-camera\n");
		return;
	}
	/* root:mobile 0660, as iOSPlus10's node: the media server runs as mobile (uid, gid 501). */
	d.node = devfs_make_node(makedev(major, 0), DEVFS_CHAR, UID_ROOT, 501, 0660, "oneios-camera");
	if (!d.node) {
		printf("ONEIOS CAMERA: devfs_make_node failed: no /dev/oneios-camera\n");
	}
}
