/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS Bluetooth: the device tree's "bluetooth" nub for iOS bluetoothd, which finds a registry
 * entry named "bluetooth" and reads its properties.
 *   oneios_bt_adt_publish(addr): local-mac-address = the address the controller uses (6 bytes).
 *   oneios_bt_adt_hide(): Bluetooth off: the nub leaves the device-tree plane and is terminated, so
 *     bluetoothd sees no Bluetooth instead of a transport that fails.
 *
 * Ported as is from iOSPlus10 (2600-laurel-bluetooth-wcn3990.patch, iokit/Kernel/arm/LaurelBluetooth.cpp).
 * Changes: names; the node is looked for at /bluetooth (where xnuhost's tree has it) and at
 * /arm-io/bluetooth.
 */
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <IOKit/IORegistryEntry.h>
#include <IOKit/IODeviceTreeSupport.h>
#include <libkern/c++/OSData.h>

static const char *const kPaths[] = { "/bluetooth", "/arm-io/bluetooth" };

static IORegistryEntry *
find_nub(void)
{
	for (unsigned i = 0; i < 2; i++) {
		IORegistryEntry *e = IORegistryEntry::fromPath(kPaths[i], gIODTPlane);
		if (e != NULL) {
			return e;
		}
	}
	return NULL;
}

extern "C" void oneios_bt_adt_publish(const uint8_t addr[6]);
extern "C" void oneios_bt_adt_hide(void);

extern "C" void
oneios_bt_adt_publish(const uint8_t addr[6])
{
	IORegistryEntry *e = find_nub();

	if (e == NULL) {
		IOLog("ONEIOS BT: adt: no bluetooth node in the device tree (bluetoothd finds the transport by /dev only)\n");
		return;
	}
	OSData *d = OSData::withBytes(addr, 6);
	if (d != NULL) {
		e->setProperty("local-mac-address", d);
		d->release();
	}
	e->release();
}

extern "C" void
oneios_bt_adt_hide(void)
{
	IORegistryEntry *e = find_nub();

	if (e == NULL) {
		return;
	}
	IOService *s = OSDynamicCast(IOService, e);
	e->detachAll(gIODTPlane);
	if (s != NULL && !s->isInactive()) {
		s->terminate();
	}
	e->release();
}
