/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS Bluetooth: the device nodes. bluetoothd checks access("/dev/cu.bluetooth") /
 * access("/dev/uart.bluetooth") before it starts the stack, and its H4BC transport opens /dev/btwake
 * (a failure is only logged). So: /dev/uart.bluetooth and /dev/cu.bluetooth (one char device: raw H4
 * bytes, select/poll, TIOCGETA/TIOCSETA/FIONREAD) and /dev/btwake (open and close do nothing).
 *
 * Ported as is from iOSPlus10 (2600-laurel-bluetooth-wcn3990.patch, bsd/dev/laurel_bt_dev.c, negroni
 * 0332). Changes: names; the speed reported is a constant.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/errno.h>
#include <sys/conf.h>
#include <sys/uio.h>
#include <sys/fcntl.h>
#include <sys/proc.h>
#include <sys/select.h>
#include <sys/termios.h>
#include <sys/ttycom.h>
#include <sys/filio.h>
#include <sys/vnode.h>                  /* IO_NDELAY */
#include <miscfs/devfs/devfs.h>
#include <kern/locks.h>
#include "oneios_bt.h"

#define MINOR_UART      0
#define MINOR_CU        1
#define MINOR_WAKE      2

static LCK_GRP_DECLARE(dev_grp, "oneios_bt_dev");
static LCK_MTX_DECLARE(dev_lock, &dev_grp);

static struct {
	bool open;
	void *node[3];
	struct selinfo rsel;
	struct termios tio;
} d;

void
obt_dev_wakeup(void)
{
	lck_mtx_lock(&dev_lock);
	wakeup(&d);
	selwakeup(&d.rsel);
	lck_mtx_unlock(&dev_lock);
}

static int
dev_open(dev_t dev, int flags __unused, int devtype __unused, struct proc *p __unused)
{
	int e;

	if (minor(dev) == MINOR_WAKE) {
		return 0;
	}
	if (minor(dev) > MINOR_CU) {
		return ENXIO;
	}
	if ((e = obt_ios_claim(2, 0)) == 0) {
		d.open = true;
	}
	return e;
}

static int
dev_close(dev_t dev, int flags __unused, int devtype __unused, struct proc *p __unused)
{
	if (minor(dev) <= MINOR_CU && d.open) {
		d.open = false;
		obt_ios_release(2);
		obt_dev_wakeup();               /* a blocked reader sees the closed client */
	}
	return 0;
}

static int
dev_read(dev_t dev, struct uio *uio, int ioflag)
{
	uint8_t b[1024];
	uint32_t n = 0;
	int e = 0;

	if (minor(dev) > MINOR_CU) {
		return ENODEV;
	}
	lck_mtx_lock(&dev_lock);
	while (!e && (n = obt_ios_take(b, (uint32_t)MIN((user_ssize_t)sizeof(b), uio_resid(uio)))) == 0) {
		if (!d.open) {
			e = EIO;
		} else if (ioflag & IO_NDELAY) {
			e = EWOULDBLOCK;
		} else {
			struct timespec ts = { 0, 50 * 1000 * 1000 };   /* bounded: no lost wakeup */
			e = msleep(&d, &dev_lock, PZERO | PCATCH, "lbtrd", &ts);
			e = e == EWOULDBLOCK ? 0 : e;
		}
	}
	lck_mtx_unlock(&dev_lock);
	return e ? e : uiomove((const char *)b, (int)n, uio);
}

static int
dev_write(dev_t dev, struct uio *uio, int ioflag __unused)
{
	uint8_t b[1024];
	int e = 0;

	if (minor(dev) > MINOR_CU) {
		return ENODEV;
	}
	while (uio_resid(uio) > 0 && !e) {
		int n = (int)MIN((user_ssize_t)sizeof(b), uio_resid(uio));

		if ((e = uiomove((char *)b, n, uio)) == 0) {
			e = obt_ios_send(b, (uint32_t)n);
		}
	}
	return e;
}

static int
dev_ioctl(dev_t dev, u_long cmd, caddr_t data, int flag __unused, struct proc *p __unused)
{
	if (minor(dev) > MINOR_CU) {
		return ENOTTY;
	}
	switch (cmd) {
	case TIOCGETA:
		d.tio.c_ispeed = d.tio.c_ospeed = OBT_LINE_SPEED;
		memcpy(data, &d.tio, sizeof(struct termios));
		return 0;
	case TIOCSETA:
	case TIOCSETAW:
	case TIOCSETAF:
		memcpy(&d.tio, data, sizeof(struct termios));   /* stored only: the line keeps its speed */
		return 0;
	case FIONREAD:
		*(int *)data = (int)obt_ios_queued();
		return 0;
	case FIONBIO:
	case FIOASYNC:
	case TIOCEXCL:
	case TIOCNXCL:
	case TIOCFLUSH:
		return 0;
	default:
		return ENOTTY;
	}
}

static int
dev_select(dev_t dev, int which, void *wql, struct proc *p)
{
	int ready = 0;

	if (minor(dev) > MINOR_CU) {
		return 0;
	}
	lck_mtx_lock(&dev_lock);
	if (which == FREAD) {
		ready = obt_ios_queued() > 0 || !d.open;
		if (!ready) {
			selrecord(p, &d.rsel, wql);
		}
	} else if (which == FWRITE) {
		ready = 1;
	}
	lck_mtx_unlock(&dev_lock);
	return ready;
}

static const struct cdevsw dev_cdevsw = {
	.d_open = dev_open,
	.d_close = dev_close,
	.d_read = dev_read,
	.d_write = dev_write,
	.d_ioctl = dev_ioctl,
	.d_stop = eno_stop,
	.d_reset = eno_reset,
	.d_ttys = NULL,
	.d_select = dev_select,
	.d_mmap = eno_mmap,
	.d_strategy = eno_strat,
	.d_type = 0,
};

void
obt_dev_attach(void)
{
	int major = cdevsw_add(-1, &dev_cdevsw);

	bzero(&d.tio, sizeof(d.tio));
	d.tio.c_cflag = CS8 | CREAD | CLOCAL | CRTSCTS;
	if (major < 0) {
		OBT_SAY("dev: cdevsw_add failed: no /dev nodes\n");
		return;
	}
	d.node[0] = devfs_make_node(makedev(major, MINOR_UART), DEVFS_CHAR, UID_ROOT, GID_WHEEL, 0666, "uart.bluetooth");
	d.node[1] = devfs_make_node(makedev(major, MINOR_CU), DEVFS_CHAR, UID_ROOT, GID_WHEEL, 0666, "cu.bluetooth");
	d.node[2] = devfs_make_node(makedev(major, MINOR_WAKE), DEVFS_CHAR, UID_ROOT, GID_WHEEL, 0666, "btwake");
	if (!d.node[0] || !d.node[1] || !d.node[2]) {
		OBT_SAY("dev: node failed (uart %d cu %d btwake %d)\n", d.node[0] != NULL, d.node[1] != NULL,
		    d.node[2] != NULL);
	}
}

/* Bluetooth off: bluetoothd's access() check then finds no device ("No bluetooth on this device") */
void
obt_dev_detach(void)
{
	for (unsigned i = 0; i < 3; i++) {
		if (d.node[i]) {
			devfs_remove(d.node[i]);
			d.node[i] = NULL;
		}
	}
}
