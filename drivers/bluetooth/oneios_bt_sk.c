/*
 * SPDX-License-Identifier: APSL-2.0
 *
 * OneiOS Bluetooth: the transport that iOS 27's bluetoothd opens. 24A446's bluetoothd does not read and write a
 * serial node and does not know the control com.apple.uart.bluetooth (oneios_bt_ctl.c, oneios_bt_dev.c: iOS 16's
 * transports). Its UART transport ("BM3UART" in its own lines) is:
 *   open /dev/btwake;
 *   socket(PF_SYSTEM, SOCK_STREAM, SYSPROTO_CONTROL), CTLIOCGINFO "com.apple.uart.sk.bluetooth", connect;
 *   setsockopt IOAOSSKYSETCHANNELSPEC {buffer size, TX slots, RX slots}, getsockopt IOAOSSKYGETCHANNELUUID;
 *   os_channel_create on that Skywalk nexus (a kernel pipe bound to its pid), and from then on the H4 bytes in
 *   the channel's packets, os_channel_sync both ways; the termios of the line by get and set socket options.
 * This file is that control and that kernel pipe, over the transport core (oneios_bt_ios.c): what bluetoothd
 * sends goes through the core's H4 framing and its answers on the controller's behalf to the host; what the
 * host's controller sends comes back in the receive ring.
 *
 * After UniXNU/QcaBluetooth 986d631 src/qbt_transport.c (APSL-2.0, by daimile2 and contributors, from Renoir XNU
 * Bluetooth's renoir_bluetooth_transport.inc), which carries H4 to the same bluetoothd on the same OS build: the
 * control's options and their numbers, the nexus provider, the two sync callbacks and the ring lifetime rules are
 * its. Changed: the UART owner is the transport core (no power requests, no firmware, no sysctls: the phone's HAL
 * does those when the core's client connects); received bytes are taken from the core's queue, not from a FIFO of
 * this file; the line's settings are stored as they are set.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/errno.h>
#include <kern/locks.h>
#include <sys/kern_control.h>
#include <sys/mbuf.h>
#include <sys/mcache.h>
#include <sys/proc.h>
#include <uuid/uuid.h>
#include <skywalk/os_skywalk.h>
#include "oneios_bt.h"

#define SK_CTL_NAME     "com.apple.uart.sk.bluetooth"
#define SK_FIFO         8192u
#define SK_OWNER        3u
/* the control's options (bluetoothd's numbers) */
#define SK_SET_SPEC     0x800c5414u     /* IOAOSSKYSETCHANNELSPEC: uint32 buffer size, TX slots, RX slots */
#define SK_GET_SPEC     0x400c5413u
#define SK_GET_UUID     0x40105412u     /* IOAOSSKYGETCHANNELUUID */
#define SK_SET_TERMIOS  0x80487414u     /* TIOCSETA with the 64-bit termios */
#define SK_GET_TERMIOS  0x40487413u     /* TIOCGETA */

struct sk_termios64 {
	uint64_t iflag, oflag, cflag, lflag;
	uint8_t cc[20];
	uint32_t pad;
	uint64_t ispeed, ospeed;
};

static LCK_GRP_DECLARE(sk_grp, "oneios_bt_sk");
static LCK_MTX_DECLARE(sk_lock, &sk_grp);
static struct {
	kern_ctl_ref ctl;
	bool connected, creating, closing;
	kern_nexus_t nexus;
	uint32_t unit, spec[3];
	struct sk_termios64 termios;
	nexus_controller_t ncd;
	uuid_t provider, instance;
	kern_pbufpool_t pool;
	kern_channel_ring_t rxring, txring;
	unsigned busy;                          /* ring notifications in flight: a disconnect waits for them */
	uint32_t tx_head, tx_len;
	uint8_t tx[SK_FIFO];                    /* bluetoothd's bytes until the pump thread hands them to the core */
	uint8_t chunk[1024];                    /* pump thread only */
	bool rx_kick;                           /* the core queued bytes: the receive ring is told */
	uint64_t n_tx, n_rx;
} sk = { .spec = { 2048, 16, 16 } };

/* the calling process's name, for a log line */
static const char *
sk_who(void)
{
	static char who[24];

	proc_selfname(who, sizeof(who));
	return who;
}

static void
sk_termios_default(void)
{
	bzero(&sk.termios, sizeof(sk.termios));
	sk.termios.cflag = 0x300 | 0x800 | 0x8000 | 0x10000 | 0x20000;  /* 8N1, CREAD, CLOCAL, RTS and CTS flow control */
	sk.termios.ispeed = sk.termios.ospeed = OBT_LINE_SPEED;
}

/* Tells a ring, outside the lock (taken on entry, dropped on return); a disconnect drains these first. */
static void
sk_notify(kern_channel_ring_t ring)
{
	if (!ring) {
		lck_mtx_unlock(&sk_lock);
		return;
	}
	++sk.busy;
	lck_mtx_unlock(&sk_lock);
	(void)kern_channel_notify(ring, 0);
	lck_mtx_lock(&sk_lock);
	if (!--sk.busy) {
		wakeup(&sk.busy);
	}
	lck_mtx_unlock(&sk_lock);
}

/* sk_lock held */
static errno_t
sk_io_status(void)
{
	if (!sk.connected || sk.closing) {
		return ENOTCONN;
	}
	return 0;
}

/* The core queued bytes for this client (any thread; no lock of the core's is held). */
void
obt_sk_wakeup(void)
{
	sk.rx_kick = true;
}

/* The pump thread: bluetoothd's bytes to the core (which may wait for the host's controller), and the receive ring
 * told when the core has bytes. */
void
obt_sk_pump(void)
{
	for (;;) {
		uint32_t n, first;

		lck_mtx_lock(&sk_lock);
		n = MIN(sk.tx_len, (uint32_t)sizeof(sk.chunk));
		if (!n || !sk.connected || sk.closing) {
			lck_mtx_unlock(&sk_lock);
			break;
		}
		first = MIN(n, SK_FIFO - sk.tx_head);
		memcpy(sk.chunk, sk.tx + sk.tx_head, first);
		memcpy(sk.chunk + first, sk.tx, n - first);
		sk.tx_head = (sk.tx_head + n) % SK_FIFO;
		sk.tx_len -= n;
		sk.n_tx += n;
		sk_notify(sk.txring);                   /* room again: a writer that found the FIFO full goes on */
		(void)obt_ios_send(sk.chunk, n);        /* an error is the controller's: the core has said why */
	}
	if (sk.rx_kick && obt_ios_queued()) {
		lck_mtx_lock(&sk_lock);
		sk.rx_kick = false;
		if (sk.connected && !sk.closing) {
			sk_notify(sk.rxring);
		} else {
			lck_mtx_unlock(&sk_lock);
		}
	}
}

/* ---- the kernel pipe's provider ---- */
static errno_t
sk_pre_connect(kern_nexus_provider_t prov __unused, proc_t p __unused, kern_nexus_t nexus __unused,
    nexus_port_t port __unused, kern_channel_t channel __unused, void **ctx __unused)
{
	return 0;
}

static errno_t
sk_connected(kern_nexus_provider_t prov __unused, kern_nexus_t nexus, kern_channel_t channel __unused)
{
	bool valid;

	lck_mtx_lock(&sk_lock);
	valid = sk.connected && !sk.closing;
	if (valid) {
		sk.nexus = nexus;
	}
	lck_mtx_unlock(&sk_lock);
	OBT_SAY("sk: bluetoothd's channel %s\n", valid ? "is connected" : "came with no control client: refused");
	return valid ? 0 : ENXIO;
}

/* bluetoothd closes the control before it destroys its channel when it turns Bluetooth off, so this can come after
 * the control's disconnect is done: that channel no longer owns the state. */
static void
sk_pre_disconnect(kern_nexus_provider_t prov __unused, kern_nexus_t nexus, kern_channel_t channel __unused)
{
	lck_mtx_lock(&sk_lock);
	if (sk.nexus == nexus) {
		sk.closing = true;
	}
	while (sk.busy) {
		(void)msleep(&sk.busy, &sk_lock, PZERO, "obtsk", NULL);
	}
	lck_mtx_unlock(&sk_lock);
}

static void
sk_disconnected(kern_nexus_provider_t prov __unused, kern_nexus_t nexus __unused, kern_channel_t channel __unused)
{
}

static errno_t
sk_ring_init(kern_nexus_provider_t prov __unused, kern_nexus_t nexus __unused, kern_channel_t channel __unused,
    kern_channel_ring_t ring, boolean_t is_tx, void **ctx __unused)
{
	lck_mtx_lock(&sk_lock);
	if (is_tx) {
		sk.txring = ring;
	} else {
		sk.rxring = ring;
	}
	lck_mtx_unlock(&sk_lock);
	return 0;
}

static void
sk_ring_fini(kern_nexus_provider_t prov __unused, kern_nexus_t nexus __unused, kern_channel_ring_t ring)
{
	lck_mtx_lock(&sk_lock);
	if (sk.txring == ring) {
		sk.txring = NULL;
	}
	if (sk.rxring == ring) {
		sk.rxring = NULL;
	}
	lck_mtx_unlock(&sk_lock);
}

/* bluetoothd -> controller: whole slots into the FIFO; a slot that does not fit stays in the ring until the pump
 * thread has drained the FIFO and told the ring. */
static errno_t
sk_sync_tx(kern_nexus_provider_t prov __unused, kern_nexus_t nexus __unused, kern_channel_ring_t ring,
    uint32_t flags __unused)
{
	kern_channel_slot_t prev = NULL, slot;
	errno_t error;

	lck_mtx_lock(&sk_lock);
	error = sk_io_status();
	if (error) {
		lck_mtx_unlock(&sk_lock);
		return error;
	}
	for (slot = kern_channel_get_next_slot(ring, NULL, NULL); slot && !sk.closing;
	    slot = kern_channel_get_next_slot(ring, slot, NULL)) {
		kern_packet_t ph = kern_channel_slot_get_packet(ring, slot);

		if (ph) {
			kern_buflet_t buf = kern_packet_get_next_buflet(ph, NULL);
			uint32_t n = kern_packet_get_data_length(ph), at, first;
			const uint8_t *data;

			if (!buf || n > sk.spec[0] || kern_buflet_get_data_offset(buf) > kern_buflet_get_data_limit(buf) ||
			    n > kern_buflet_get_data_limit(buf) - kern_buflet_get_data_offset(buf)) {
				error = EPROTO;
				break;
			}
			if (n > SK_FIFO - sk.tx_len) {
				break;
			}
			data = (const uint8_t *)kern_buflet_get_data_address(buf) + kern_buflet_get_data_offset(buf);
			at = (sk.tx_head + sk.tx_len) % SK_FIFO;
			first = MIN(n, SK_FIFO - at);
			memcpy(sk.tx + at, data, first);
			memcpy(sk.tx, data + first, n - first);
			sk.tx_len += n;
		}
		prev = slot;
	}
	if (prev) {
		kern_channel_advance_slot(ring, prev);
		(void)kern_channel_reclaim(ring);
	}
	lck_mtx_unlock(&sk_lock);
	if (prev) {
		obt_ios_kick();                         /* the pump thread takes the FIFO now */
	}
	return error;
}

/* Controller -> bluetoothd: the core's queued bytes into freshly allocated packets of the receive ring. */
static errno_t
sk_sync_rx(kern_nexus_provider_t prov __unused, kern_nexus_t nexus, kern_channel_ring_t ring, uint32_t flags __unused)
{
	kern_channel_slot_t prev = NULL, slot;
	kern_pbufpool_t pool = NULL;
	errno_t error;

	if (kern_nexus_get_pbufpool(nexus, NULL, &pool) || !pool) {
		return 0;
	}
	lck_mtx_lock(&sk_lock);
	error = sk_io_status();
	if (error) {
		lck_mtx_unlock(&sk_lock);
		return error;
	}
	(void)kern_channel_reclaim(ring);
	for (slot = kern_channel_get_next_slot(ring, NULL, NULL); slot && obt_ios_queued() && !sk.closing;
	    slot = kern_channel_get_next_slot(ring, slot, NULL)) {
		kern_packet_t ph = 0;
		kern_buflet_t buf;
		uint32_t limit, n;

		if (kern_pbufpool_alloc_nosleep(pool, 1, &ph)) {
			break;
		}
		buf = kern_packet_get_next_buflet(ph, NULL);
		if (!buf || !kern_buflet_get_data_address(buf)) {
			kern_pbufpool_free(pool, ph);
			break;
		}
		limit = MIN(sk.spec[0], kern_buflet_get_data_limit(buf));
		n = obt_ios_take((uint8_t *)kern_buflet_get_data_address(buf), limit);
		kern_packet_clear_flow_uuid(ph);
		if (!n || kern_buflet_set_data_offset(buf, 0) || kern_buflet_set_data_length(buf, n) ||
		    kern_packet_finalize(ph) || kern_channel_slot_attach_packet(ring, slot, ph)) {
			kern_pbufpool_free(pool, ph);   /* bytes taken from the core are lost with it: counted there as received */
			break;
		}
		sk.n_rx += n;
		prev = slot;
	}
	if (prev) {
		kern_channel_advance_slot(ring, prev);
	}
	lck_mtx_unlock(&sk_lock);
	return 0;
}

static void
sk_destroy(nexus_controller_t ncd, const uuid_t provider, const uuid_t instance, kern_pbufpool_t pool)
{
	if (!uuid_is_null(instance)) {
		(void)kern_nexus_controller_free_provider_instance(ncd, instance);
	}
	if (pool) {
		kern_pbufpool_destroy(pool);
	}
	if (!uuid_is_null(provider)) {
		(void)kern_nexus_controller_deregister_provider(ncd, provider);
	}
	if (ncd) {
		kern_nexus_controller_destroy(ncd);
	}
}

/* One kernel pipe for the client's pid: bluetoothd attaches to its client port with os_channel_create. */
static errno_t
sk_create(const uint32_t spec[3], pid_t pid, nexus_controller_t *ncdp, uuid_t provider, uuid_t instance,
    kern_pbufpool_t *poolp)
{
	nexus_controller_t ncd = NULL;
	nexus_attr_t attr = NULL;
	kern_pbufpool_t pool = NULL;
	uuid_t domain;
	errno_t e;

	uuid_clear(provider);
	uuid_clear(instance);
	e = kern_nexus_controller_create(&ncd);
	if (!e) {
		e = kern_nexus_get_default_domain_provider(NEXUS_TYPE_KERNEL_PIPE, &domain);
	}
	if (!e) {
		e = kern_nexus_attr_create(&attr);
	}
	if (!e) {
		e = kern_nexus_attr_set(attr, NEXUS_ATTR_SLOT_BUF_SIZE, spec[0]);
	}
	if (!e) {
		e = kern_nexus_attr_set(attr, NEXUS_ATTR_TX_SLOTS, spec[1]);
	}
	if (!e) {
		e = kern_nexus_attr_set(attr, NEXUS_ATTR_RX_SLOTS, spec[2]);
	}
	if (!e) {
		struct kern_nexus_provider_init init = {
			.nxpi_version = KERN_NEXUS_DOMAIN_PROVIDER_CURRENT_VERSION, .nxpi_flags = NXPIF_VIRTUAL_DEVICE,
			.nxpi_pre_connect = sk_pre_connect, .nxpi_connected = sk_connected,
			.nxpi_pre_disconnect = sk_pre_disconnect, .nxpi_disconnected = sk_disconnected,
			.nxpi_ring_init = sk_ring_init, .nxpi_ring_fini = sk_ring_fini,
			.nxpi_sync_tx = sk_sync_tx, .nxpi_sync_rx = sk_sync_rx,
		};

		e = kern_nexus_controller_register_provider(ncd, domain, (const uint8_t *)SK_CTL_NAME, &init, sizeof(init), attr,
		    (uuid_t *)provider);
	}
	if (attr) {
		kern_nexus_attr_destroy(attr);
	}
	if (!e) {
		struct kern_pbufpool_init pp = {
			.kbi_version = KERN_PBUFPOOL_CURRENT_VERSION, .kbi_flags = KBIF_VIRTUAL_DEVICE | KBIF_QUANTUM,
			.kbi_packets = (spec[1] + spec[2]) * 2, .kbi_bufsize = spec[0], .kbi_max_frags = 1,
		};

		(void)snprintf((char *)pp.kbi_name, sizeof(pp.kbi_name), "com.apple.kpipe.uart.bluetooth");
		e = kern_pbufpool_create(&pp, &pool, NULL);
	}
	if (!e) {
		struct kern_nexus_init init = { .nxi_version = KERN_NEXUS_CURRENT_VERSION, .nxi_tx_pbufpool = pool };

		e = kern_nexus_controller_alloc_provider_instance(ncd, provider, NULL, NULL, (uuid_t *)instance, &init);
	}
	if (!e) {
		nexus_port_t port = NEXUS_PORT_KERNEL_PIPE_CLIENT;

		e = kern_nexus_controller_bind_provider_instance(ncd, instance, &port, pid, NULL, NULL, 0, NEXUS_BIND_PID);
	}
	if (e) {
		sk_destroy(ncd, provider, instance, pool);
		return e;
	}
	*ncdp = ncd;
	*poolp = pool;
	return 0;
}

/* ---- the kernel control ---- */
static errno_t
sk_ctl_connect(kern_ctl_ref ref __unused, struct sockaddr_ctl *sac, void **unitinfo)
{
	char who[24] = "?";
	int e;

	proc_selfname(who, sizeof(who));
	lck_mtx_lock(&sk_lock);
	if (sk.connected) {
		lck_mtx_unlock(&sk_lock);
		OBT_SAY("sk: %s[%d] connects to %s: busy\n", who, proc_selfpid(), SK_CTL_NAME);
		return EBUSY;
	}
	sk.connected = true;
	sk.closing = false;
	sk.unit = sac->sc_unit;
	sk.tx_head = sk.tx_len = 0;
	sk.n_tx = sk.n_rx = 0;
	sk_termios_default();
	lck_mtx_unlock(&sk_lock);
	*unitinfo = &sk;
	e = obt_ios_claim(SK_OWNER, sac->sc_unit);     /* the host opens the phone's controller now */
	if (e) {
		lck_mtx_lock(&sk_lock);
		sk.connected = false;
		lck_mtx_unlock(&sk_lock);
	}
	OBT_SAY("sk: %s[%d] connects to %s: %d\n", who, proc_selfpid(), SK_CTL_NAME, e);
	return e;
}

static errno_t
sk_ctl_disconnect(kern_ctl_ref ref __unused, u_int32_t unit, void *unitinfo __unused)
{
	nexus_controller_t ncd;
	kern_pbufpool_t pool;
	uuid_t provider, instance;

	lck_mtx_lock(&sk_lock);
	if (!sk.connected || sk.unit != unit) {
		lck_mtx_unlock(&sk_lock);
		return 0;
	}
	sk.closing = true;
	while (sk.creating) {
		(void)msleep(&sk.creating, &sk_lock, PZERO, "obtskclose", NULL);
	}
	while (sk.busy) {
		(void)msleep(&sk.busy, &sk_lock, PZERO, "obtskclose", NULL);
	}
	ncd = sk.ncd;
	pool = sk.pool;
	uuid_copy(provider, sk.provider);
	uuid_copy(instance, sk.instance);
	sk.ncd = NULL;
	sk.pool = NULL;
	uuid_clear(sk.provider);
	uuid_clear(sk.instance);
	lck_mtx_unlock(&sk_lock);
	if (ncd) {
		sk_destroy(ncd, provider, instance, pool);
	}
	lck_mtx_lock(&sk_lock);
	OBT_SAY("sk: the client is gone (%llu bytes from it, %llu to it)\n", sk.n_tx, sk.n_rx);
	sk.connected = sk.closing = false;
	sk.nexus = NULL;
	/* a channel that outlives this session gets no more notifications; its ring_fini finds nothing to clear */
	sk.rxring = sk.txring = NULL;
	sk.tx_head = sk.tx_len = 0;
	sk.spec[0] = 2048;
	sk.spec[1] = sk.spec[2] = 16;
	lck_mtx_unlock(&sk_lock);
	obt_ios_release(SK_OWNER);                      /* the host closes the controller */
	return 0;
}

static errno_t
sk_ctl_send(kern_ctl_ref ref __unused, u_int32_t unit __unused, void *unitinfo __unused, mbuf_t m, int flags __unused)
{
	mbuf_freem(m);                                  /* the bytes go by the channel, not by the socket */
	return EOPNOTSUPP;
}

static errno_t
sk_ctl_setopt(kern_ctl_ref ref __unused, u_int32_t unit __unused, void *unitinfo __unused, int opt, void *data, size_t len)
{
	if ((uint32_t)opt == SK_SET_SPEC) {
		const uint32_t *spec = data;
		uint32_t wanted[3];
		nexus_controller_t ncd = NULL;
		kern_pbufpool_t pool = NULL;
		uuid_t provider, instance;
		pid_t pid;
		errno_t e;

		if (len != 12 || !data) {
			return EINVAL;
		}
		memcpy(wanted, spec, sizeof(wanted));
		lck_mtx_lock(&sk_lock);
		if (!sk.connected || sk.closing) {
			lck_mtx_unlock(&sk_lock);
			return ENXIO;
		}
		if (sk.ncd) {
			lck_mtx_unlock(&sk_lock);
			return 0;
		}
		if (sk.creating) {
			lck_mtx_unlock(&sk_lock);
			return EBUSY;
		}
		if (!wanted[0] || (wanted[0] & (wanted[0] - 1)) || wanted[0] > 4096 || wanted[1] - 1 >= 512 || wanted[2] - 1 >= 512) {
			lck_mtx_unlock(&sk_lock);
			OBT_SAY("sk: a channel of %u-byte buffers and %u/%u slots is not one this pipe makes\n", wanted[0], wanted[1], wanted[2]);
			return EINVAL;
		}
		memcpy(sk.spec, wanted, sizeof(wanted));
		sk.creating = true;
		lck_mtx_unlock(&sk_lock);
		pid = proc_selfpid();
		e = sk_create(wanted, pid, &ncd, provider, instance, &pool);
		lck_mtx_lock(&sk_lock);
		sk.creating = false;
		if (!e) {
			sk.ncd = ncd;
			sk.pool = pool;
			uuid_copy(sk.provider, provider);
			uuid_copy(sk.instance, instance);
		}
		wakeup(&sk.creating);
		lck_mtx_unlock(&sk_lock);
		OBT_SAY("sk: channel of %u-byte buffers, %u/%u slots for pid %d: %d\n", wanted[0], wanted[1], wanted[2], pid, e);
		return e ? ENXIO : 0;
	}
	if ((uint32_t)opt == SK_SET_TERMIOS) {
		const struct sk_termios64 *t = data;

		if (len != sizeof(struct sk_termios64) || !data) {
			return EINVAL;
		}
		OBT_SAY("sk: line set: cflag 0x%llx, speed %llu/%llu (kept; there is no line)\n", t->cflag, t->ispeed, t->ospeed);
		lck_mtx_lock(&sk_lock);
		sk.termios = *t;
		lck_mtx_unlock(&sk_lock);
		return 0;
	}
	OBT_SAY("sk: socket option 0x%x (%zu bytes) is not one this control has\n", (uint32_t)opt, len);
	return ENOPROTOOPT;
}

static errno_t
sk_ctl_getopt(kern_ctl_ref ref __unused, u_int32_t unit __unused, void *unitinfo __unused, int opt, void *data, size_t *len)
{
	errno_t e = 0;

	lck_mtx_lock(&sk_lock);
	if ((uint32_t)opt == SK_GET_SPEC) {
		if (!data || *len < 12) {
			e = EINVAL;
		} else {
			memcpy(data, sk.spec, 12);
			*len = 12;
		}
	} else if ((uint32_t)opt == SK_GET_UUID) {
		if (!data || *len < sizeof(uuid_t)) {
			e = EINVAL;
		} else if (!sk.ncd) {
			e = ENXIO;
		} else {
			memcpy(data, sk.instance, sizeof(uuid_t));
			*len = sizeof(uuid_t);
		}
	} else if ((uint32_t)opt == SK_GET_TERMIOS) {
		if (!data || *len < sizeof(struct sk_termios64)) {
			e = EINVAL;
		} else {
			memcpy(data, &sk.termios, sizeof(struct sk_termios64));
			*len = sizeof(struct sk_termios64);
		}
	} else {
		e = ENOPROTOOPT;
	}
	lck_mtx_unlock(&sk_lock);
	if (e == ENOPROTOOPT) {
		OBT_SAY("sk: socket option 0x%x asked: not one this control has\n", (uint32_t)opt);
	} else if ((uint32_t)opt == SK_GET_UUID) {
		/* the step between the channel's specification and os_channel_create: where a stop after "channel of" is */
		OBT_SAY("sk: %s[%d] asks the channel's UUID: %d\n", sk_who(), proc_selfpid(), e);
	}
	return e;
}

void
obt_sk_attach(void)
{
	struct kern_ctl_reg reg = {
		.ctl_flags = CTL_FLAG_REG_SOCK_STREAM,  /* not privileged: bluetoothd runs as mobile */
		.ctl_connect = sk_ctl_connect, .ctl_disconnect = sk_ctl_disconnect, .ctl_send = sk_ctl_send,
		.ctl_setopt = sk_ctl_setopt, .ctl_getopt = sk_ctl_getopt,
	};
	errno_t e;

	sk_termios_default();
	strlcpy(reg.ctl_name, SK_CTL_NAME, sizeof(reg.ctl_name));
	if ((e = ctl_register(&reg, &sk.ctl)) != 0) {
		OBT_SAY("sk: %s not registered (error %d): bluetoothd has no transport\n", SK_CTL_NAME, e);
	}
}
