/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS Bluetooth: the kernel control that iOS bluetoothd opens: socket(PF_SYSTEM, SOCK_STREAM,
 * SYSPROTO_CONTROL), CTLIOCGINFO "com.apple.uart.bluetooth", connect; getsockopt TIOCGETA / setsockopt
 * TIOCSETA (termios, stored only: there is no line, the packets go to the host); then plain H4 bytes
 * both ways. Not privileged (bluetoothd and BlueTool run as mobile). Data toward the client is
 * enqueued only by the pump thread (obt_ctl_pump).
 *
 * Ported as is from iOSPlus10 (2600-laurel-bluetooth-wcn3990.patch, bsd/dev/laurel_bt_ctl.c, negroni
 * 0332). Changes: names; the speed reported is a constant.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/errno.h>
#include <sys/termios.h>
#include <sys/ttycom.h>
#include <sys/kern_control.h>
#include <sys/kpi_mbuf.h>
#include "oneios_bt.h"

#define CTL_NAME        "com.apple.uart.bluetooth"
#define CTL_CHUNK       2048u

static struct {
	kern_ctl_ref ref;
	volatile bool connected, full, kick;
	uint32_t unit;
	struct termios tio;
	uint8_t buf[CTL_CHUNK];                 /* RX thread only */
} c;

static errno_t
ctl_connect(kern_ctl_ref ref __unused, struct sockaddr_ctl *sac, void **unitinfo)
{
	errno_t e = obt_ios_claim(1, sac->sc_unit);

	if (e == 0) {
		c.unit = sac->sc_unit;
		c.full = false;
		c.connected = true;
	}
	*unitinfo = NULL;
	return e;
}

static errno_t
ctl_disconnect(kern_ctl_ref ref __unused, u_int32_t unit, void *unitinfo __unused)
{
	if (c.connected && c.unit == unit) {
		c.connected = false;
		obt_ios_release(1);
	}
	return 0;
}

static errno_t
ctl_send(kern_ctl_ref ref __unused, u_int32_t unit, void *unitinfo __unused, mbuf_t m, int flags __unused)
{
	errno_t e = 0;

	if (!c.connected || c.unit != unit) {
		mbuf_freem(m);
		return ENOTCONN;
	}
	for (mbuf_t n = m; n != NULL && !e; n = mbuf_next(n)) {
		if (mbuf_len(n) > 0) {
			e = obt_ios_send((const uint8_t *)mbuf_data(n), (uint32_t)mbuf_len(n));
		}
	}
	mbuf_freem(m);
	return e;
}

static errno_t
ctl_setopt(kern_ctl_ref ref __unused, u_int32_t unit __unused, void *unitinfo __unused, int opt, void *data,
    size_t len)
{
	if ((unsigned)opt == (unsigned)TIOCSETA || (unsigned)opt == (unsigned)TIOCSETAW ||
	    (unsigned)opt == (unsigned)TIOCSETAF) {
		if (data == NULL || len < sizeof(struct termios)) {
			return EINVAL;
		}
		memcpy(&c.tio, data, sizeof(struct termios));
	}
	return 0;
}

static errno_t
ctl_getopt(kern_ctl_ref ref __unused, u_int32_t unit __unused, void *unitinfo __unused, int opt, void *data,
    size_t *len)
{
	if ((unsigned)opt != (unsigned)TIOCGETA) {
		return ENOTSUP;
	}
	if (data != NULL) {
		if (*len < sizeof(struct termios)) {
			return EINVAL;
		}
		c.tio.c_ispeed = c.tio.c_ospeed = OBT_LINE_SPEED;
		memcpy(data, &c.tio, sizeof(struct termios));
	}
	*len = sizeof(struct termios);
	return 0;
}

static void
ctl_rcvd(kern_ctl_ref ref __unused, u_int32_t unit, void *unitinfo __unused, int flags __unused)
{
	if (c.connected && c.unit == unit) {
		c.full = false;
		c.kick = true;
	}
}

void
obt_ctl_wakeup(void)
{
	c.kick = true;
}

void
obt_ctl_pump(void)
{
	while (c.kick && c.connected && !c.full && obt_ios_queued()) {
		size_t space = 0;
		errno_t e = ctl_getenqueuespace(c.ref, c.unit, &space);
		uint32_t n;

		if (e == 0 && space == 0) {
			e = ENOBUFS;
		}
		if (e) {
			c.full = e == ENOBUFS;  /* ctl_rcvd clears it */
			return;
		}
		n = obt_ios_take(c.buf, (uint32_t)MIN(space, (size_t)CTL_CHUNK));
		if (n && (e = ctl_enqueuedata(c.ref, c.unit, c.buf, n, 0)) != 0) {
			OBT_SAY("ctl: enqueue error %d (%u bytes lost)\n", e, n);
			return;
		}
	}
	if (!obt_ios_queued()) {
		c.kick = false;
	}
}

void
obt_ctl_attach(void)
{
	struct kern_ctl_reg reg;
	errno_t e;

	bzero(&c.tio, sizeof(c.tio));
	c.tio.c_cflag = CS8 | CREAD | CLOCAL | CRTSCTS;
	bzero(&reg, sizeof(reg));
	strlcpy(reg.ctl_name, CTL_NAME, sizeof(reg.ctl_name));
	reg.ctl_flags = CTL_FLAG_REG_SOCK_STREAM | CTL_FLAG_REG_EXTENDED;       /* not CTL_FLAG_PRIVILEGED */
	reg.ctl_sendsize = 64u << 10;
	reg.ctl_recvsize = 128u << 10;
	reg.ctl_connect = ctl_connect;
	reg.ctl_disconnect = ctl_disconnect;
	reg.ctl_send = ctl_send;
	reg.ctl_setopt = ctl_setopt;
	reg.ctl_getopt = ctl_getopt;
	reg.ctl_rcvd = ctl_rcvd;
	if ((e = ctl_register(&reg, &c.ref)) != 0) {
		OBT_SAY("ctl: %s not registered (error %d)\n", CTL_NAME, e);
	}
}
