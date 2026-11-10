/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS Bluetooth: the iOS H4 transport core. One client at a time: the kernel control
 * com.apple.uart.bluetooth (oneios_bt_ctl.c) or the device /dev/uart.bluetooth (oneios_bt_dev.c); a new
 * client empties the queue and resets the per-client answers.
 *
 * Ported from iOSPlus10 (2600-laurel-bluetooth-wcn3990.patch, bsd/dev/laurel_bt_ios.c, negroni 0332).
 * Kept: the client slot, the 64 KiB queue of whole H4 packets, the TX framing, the answers of
 * oneios_bt_h4.c (vendor commands never reach the controller), the log lines. Replaced: the UART.
 *   RX: the host bridge hands over whole HCI packets (drivers/bridge, channel OB_CH_HCI, the message
 *       type is the H4 indicator); they are queued with the indicator in front. No in-band sleep
 *       bytes: the phone's HAL keeps those to itself.
 *   TX: each framed packet goes to the host without its indicator, as the message type.
 *   Power follows the client (negroni 0335, which the Mi A3 port left out): the first client asks the
 *       host to open the controller (the HAL powers it and loads its firmware, a few seconds), the
 *       last one to close it. A send waits up to 60 s for the controller.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/errno.h>
#include <kern/locks.h>
#include <kern/thread.h>
#include <kern/sched_prim.h>
#include <kern/clock.h>
#include "oneios_bt.h"
#include "oneios_bridge.h"

extern kern_return_t kernel_thread_start(thread_continue_t, void *, thread_t *);
extern void thread_deallocate(thread_t);

#define RXQ_SIZE        (64u << 10)
#define WAIT_READY_S    60
#define PUMP_GUARD_MS   20u

static LCK_GRP_DECLARE(ios_grp, "oneios_bt");
static LCK_MTX_DECLARE(ios_lock, &ios_grp);     /* state, queue, owner */
static LCK_MTX_DECLARE(tx_lock, &ios_grp);      /* TX framing and packet order */

static struct {
	bool started, failed;
	volatile uint32_t host_state;           /* OB_HCI_STATE_* as the host last said */
	uint32_t owner, unit;
	uint8_t q[RXQ_SIZE];
	uint32_t qh, qt, qn;
	struct obt_shim shim;
	uint8_t rx[OBT_H4_MAX];                 /* bridge worker only */
	uint8_t tx[OBT_H4_MAX];                 /* tx_lock */
	uint32_t txn;
	uint32_t n_tx, n_rx, n_local, n_drop;
	uint8_t address[6];
	int pump_event;
} t;

bool
obt_ios_ready(void)
{
	return t.started && t.host_state == OB_HCI_STATE_READY;
}

/* ios_lock held */
static void
q_put(const uint8_t *p, uint32_t n)
{
	for (uint32_t i = 0; i < n; i++) {
		t.q[t.qh] = p[i];
		t.qh = (t.qh + 1u) % RXQ_SIZE;
	}
	t.qn += n;
}

uint32_t
obt_ios_take(uint8_t *b, uint32_t max)
{
	uint32_t n = 0;

	lck_mtx_lock(&ios_lock);
	while (n < max && t.qn) {
		b[n++] = t.q[t.qt];
		t.qt = (t.qt + 1u) % RXQ_SIZE;
		t.qn--;
	}
	lck_mtx_unlock(&ios_lock);
	return n;
}

uint32_t
obt_ios_queued(void)
{
	return t.qn;
}

/* one whole packet toward the client (dropped without a client or room) */
static void
deliver(const uint8_t *p, uint32_t n)
{
	uint32_t owner;

	lck_mtx_lock(&ios_lock);
	owner = t.owner;
	if (owner && RXQ_SIZE - t.qn >= n) {
		q_put(p, n);
	} else {
		t.n_drop++;
		owner = 0;
	}
	lck_mtx_unlock(&ios_lock);
	if (owner == 1) {
		obt_ctl_wakeup();
		thread_wakeup((event_t)&t.pump_event);
	} else if (owner == 2) {
		obt_dev_wakeup();
	}
}

/* From the host (the bridge's worker thread): an HCI packet or the controller's state. */
static void
from_host(void *context __unused, uint16_t type, const uint8_t *payload, uint32_t length)
{
	if (type == OB_HCI_STATE && length >= sizeof(struct ob_hci_state)) {
		struct ob_hci_state s;
		memcpy(&s, payload, sizeof(s));
		t.host_state = s.state;
		if (s.state == OB_HCI_STATE_READY) {
			if (obt_addr_ok(s.bd_addr)) {
				memcpy(t.address, s.bd_addr, 6);
				oneios_bt_adt_publish(s.bd_addr);
			}
			OBT_SAY("host: controller ready, address %02x:%02x:%02x:%02x:%02x:%02x%s\n", s.bd_addr[0], s.bd_addr[1],
			    s.bd_addr[2], s.bd_addr[3], s.bd_addr[4], s.bd_addr[5],
			    obt_addr_ok(s.bd_addr) ? "" : " (not a usable one: the device tree's stays)");
		} else {
			OBT_SAY("host: controller %s\n", s.state == OB_HCI_STATE_FAILED ? "failed (the HAL did not initialise it)" : "off");
		}
		return;
	}
	if (type < OB_HCI_ACL || type > OB_HCI_ISO || length == 0 || length + 1u > OBT_H4_MAX) {
		return;                         /* a controller sends ACL, SCO, events and ISO */
	}
	t.rx[0] = (uint8_t)type;
	memcpy(t.rx + 1, payload, length);
	if (obt_h4_frame_len(t.rx, length + 1u) != (int)(length + 1u)) {
		t.n_drop++;                     /* its own length field disagrees */
		return;
	}
	if (type == OB_HCI_EVENT) {
		(void)obt_fix_event(&t.shim, t.rx, length + 1u);
	}
	t.n_rx++;
	deliver(t.rx, length + 1u);
}

/* The control client's data is enqueued by one thread only (ctl_enqueuedata order; ctl_rcvd asks for more). */
static void
pump_thread(void *arg __unused, wait_result_t w __unused)
{
	for (;;) {
		uint64_t deadline;

		obt_ctl_pump();
		clock_interval_to_deadline(PUMP_GUARD_MS, NSEC_PER_MSEC, &deadline);
		assert_wait_deadline((event_t)&t.pump_event, THREAD_UNINT, deadline);
		(void)thread_block(THREAD_CONTINUE_NULL);
	}
}

static int
wait_ready(void)
{
	for (unsigned i = 0; t.host_state == OB_HCI_STATE_OFF && !t.failed && i < WAIT_READY_S * 10; i++) {
		obt_sleep_ms(100);
	}
	return t.host_state == OB_HCI_STATE_READY ? 0 : EIO;
}

/* one whole H4 frame from userland, tx_lock held */
static int
tx_frame(uint8_t *p, uint32_t n)
{
	uint8_t ev[OBT_LOCAL_EVENT_MAX];
	unsigned len;

	if (p[0] == 1) {
		(void)obt_fix_command(p, n);
		if ((len = obt_local_answer(&t.shim, p, n, ev)) != 0) {
			t.n_local++;
			deliver(ev, len);
			return 0;
		}
	}
	t.n_tx++;
	return oneios_bridge_send(OB_CH_HCI, p[0], p + 1, n - 1u) == 0 ? 0 : EIO;
}

int
obt_ios_send(const uint8_t *b, uint32_t n)
{
	int e = wait_ready();

	lck_mtx_lock(&tx_lock);
	for (uint32_t i = 0; e == 0 && i < n; i++) {
		int fl;

		t.tx[t.txn++] = b[i];
		fl = obt_h4_frame_len(t.tx, t.txn);
		if (fl < 0) {
			t.txn = 0;              /* not an H4 frame start: drop the byte */
		} else if (fl > 0 && t.txn == (uint32_t)fl) {
			e = tx_frame(t.tx, t.txn);
			t.txn = 0;
		}
	}
	lck_mtx_unlock(&tx_lock);
	return e;
}

int
obt_ios_claim(uint32_t owner, uint32_t unit)
{
	int e = 0;

	lck_mtx_lock(&ios_lock);
	if (t.failed || !t.started) {
		e = ENXIO;                      /* Bluetooth off */
	} else if (t.owner) {
		e = EBUSY;
	} else {
		t.owner = owner;
		t.unit = unit;
		t.qh = t.qt = t.qn = 0;
		bzero(&t.shim, sizeof(t.shim));
	}
	lck_mtx_unlock(&ios_lock);
	if (e == 0) {
		lck_mtx_lock(&tx_lock);
		t.txn = 0;
		lck_mtx_unlock(&tx_lock);
		t.host_state = OB_HCI_STATE_OFF;
		(void)oneios_bridge_send(OB_CH_HCI, OB_HCI_OPEN, NULL, 0);      /* power follows the client */
		OBT_SAY("ios: client %s connected; %s\n", owner == 1 ? "control" : "device", oneios_bt_result());
	}
	return e;
}

void
obt_ios_release(uint32_t owner)
{
	bool was = false;

	lck_mtx_lock(&ios_lock);
	if (t.owner == owner) {
		t.owner = 0;
		t.qh = t.qt = t.qn = 0;
		was = true;
	}
	lck_mtx_unlock(&ios_lock);
	if (was) {
		(void)oneios_bridge_send(OB_CH_HCI, OB_HCI_CLOSE, NULL, 0);
	}
	OBT_SAY("ios: client gone (tx %u rx %u local %u dropped %u)\n", t.n_tx, t.n_rx, t.n_local, t.n_drop);
}

void
obt_ios_start(void)
{
	thread_t th;

	if (kernel_thread_start(pump_thread, NULL, &th) != KERN_SUCCESS) {
		obt_ios_fail("no pump thread");
		return;
	}
	thread_deallocate(th);
	oneios_bridge_listen(OB_CH_HCI, from_host, NULL);
	t.started = true;
}

void
obt_ios_fail(const char *why __unused)
{
	t.failed = true;
}
