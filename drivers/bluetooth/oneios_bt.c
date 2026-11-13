/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS Bluetooth: start. The controller stays the phone's: xnuhost's Bluetooth bridge opens the
 * phone's Bluetooth HAL (which powers the WCN785x and loads its firmware) when iOS's bluetoothd
 * connects. Here: the kill switch, the check that the host serves HCI, the kernel control, the /dev
 * nodes after the root mount, and the one result line.
 *
 * Follows iOSPlus10's bsd/dev/laurel_bt.c (2600-laurel-bluetooth-wcn3990.patch) without its bring-up
 * steps (rails, UART, firmware, speed, address): none of them is this kernel's job here.
 * Off = /dev nodes absent, the device-tree nub hidden, connects refused (ENXIO). Kill switch oneios_bt=0.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/vnode.h>
#include <sys/vnode_internal.h>
#include <kern/clock.h>
#include <kern/thread.h>
#include <pexpert/pexpert.h>
#include "oneios_bt.h"
#include "oneios_bridge.h"

extern void IOSleep(unsigned milliseconds);
extern kern_return_t kernel_thread_start(thread_continue_t, void *, thread_t *);
extern kern_return_t thread_terminate(thread_t);

uint64_t
obt_us(void)
{
	uint64_t t, ns;

	clock_get_uptime(&t);
	absolutetime_to_nanoseconds(t, &ns);
	return ns / 1000u;
}

void
obt_sleep_ms(uint32_t ms)
{
	IOSleep(ms);
}

/* the result line, repeated when bluetoothd connects */
static char obt_result[160] = "start not done";

const char *
oneios_bt_result(void)
{
	return obt_result;
}

static void
off(const char *why)
{
	snprintf(obt_result, sizeof(obt_result), "off: %s", why);
	OBT_SAY("%s\n", obt_result);
	obt_ios_fail(why);
	oneios_bt_adt_hide();
}

/* After the root mount: the transport core, the two kernel controls and the /dev nodes (devfs is up then). bluetoothd
 * uses none of them before launchd starts it. Started at the board's probe, before the root mount, the PC's qemu boots
 * of 2026-11-13 never got their root device (3 of 3: APFS's start waited for IOBSD while IOResources stayed busy;
 * with oneios_bt=0 or no bridge, 2 of 2 booted). */
static void
node_thread(void *arg __unused, wait_result_t w __unused)
{
	unsigned waited = 0;

	while (rootvnode == NULLVP && waited < 600u * 1000u) {
		delay_for_interval(20, NSEC_PER_MSEC);
		waited += 20;
	}
	obt_ios_start();
	obt_ctl_attach();
	obt_sk_attach();
	obt_dev_attach();
	OBT_SAY("transport up %u ms after the start: control com.apple.uart.sk.bluetooth (bluetoothd 27), "
	    "control com.apple.uart.bluetooth, /dev/uart.bluetooth, /dev/cu.bluetooth, /dev/btwake\n", waited);
	(void)thread_terminate(current_thread());
}

void
oneios_bt_start(void)
{
	static bool tried;
	uint32_t on = 1;
	thread_t th;

	if (tried) {
		return;
	}
	tried = true;
	(void)PE_parse_boot_argn("oneios_bt", &on, sizeof(on));
	if (on == 0) {
		off("oneios_bt=0");
		return;
	}
	for (unsigned waited = 0; waited < 2000u && !oneios_bridge_ready(); waited += 10u) {
		delay_for_interval(10, NSEC_PER_MSEC);
	}
	if (!oneios_bridge_ready()) {
		off("no bridge device (or the host did not answer)");
		return;
	}
	if (!(oneios_bridge_channels() & (1u << OB_CH_HCI))) {
		off("the host serves no Bluetooth controller");
		return;
	}
	if (kernel_thread_start(node_thread, NULL, &th) != KERN_SUCCESS) {
		off("no thread to start the transport");
		return;
	}
	thread_deallocate(th);
	snprintf(obt_result, sizeof(obt_result), "on: the phone's controller through the host's Bluetooth HAL (opened when bluetoothd connects)");
	OBT_SAY("%s\n", obt_result);
}
