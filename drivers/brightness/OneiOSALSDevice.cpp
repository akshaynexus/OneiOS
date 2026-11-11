/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS: the host's ambient light sensor as a HID device, for the original AppleUSBALSService
 * (com.apple.driver.AppleHIDALSService, in the kernel collection) to turn into the ambient light
 * events CoreBrightness reads. oneios_als_hid.h says why this shape and has the descriptor.
 *
 * It is a subclass of the kernel collection's own IOHIDDevice: the header "OriginalHidDeviceABI.h" is that
 * class's layout for the 24A446 iPhone18,2 collection (kernel/patches/oneios-hosted/hid). So it is built there, into
 * the hosted kernel's HID archive (build-oneios-hid.py), not by drivers/build.py: its class registers after the
 * collection's IOHIDFamily has loaded, and oneios_als_device_start() is called by oneios_hid_input_start
 * (OneiOSHidInput.cpp) once the touch screen and the buttons are published. Written on NativeHidInput's pattern,
 * with only the calls that file uses.
 *
 * Unknown until a session reaches backboardd: whether AppleUSBALSService takes a device that is not
 * on USB (its personality asks only for the IOHIDInterface, the usages and the vendor), and which
 * report fields it reads from Apple's displays.
 */
#include "OriginalHidDeviceABI.h"
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOWorkLoop.h>
#include <libkern/c++/OSString.h>

#include "oneios_bridge.h"
#include "oneios_als_hid.h"

class OneiOSALSDevice : public IOHIDDevice {
	OSDeclareDefaultStructors(OneiOSALSDevice)
	IOWorkLoop *fLoop = nullptr;
	volatile uint32_t fMillilux = 0;

	/* From the bridge's worker thread: the report is sent on this device's work loop. */
	static void reading(void *context, uint32_t millilux)
	{
		OneiOSALSDevice *self = static_cast<OneiOSALSDevice *>(context);
		if (self == nullptr || self->fLoop == nullptr) {
			return;
		}
		self->fMillilux = millilux;
		(void)self->fLoop->runAction(&OneiOSALSDevice::send, self);
	}
	static IOReturn send(OSObject *owner, void *, void *, void *, void *)
	{
		OneiOSALSDevice *self = OSDynamicCast(OneiOSALSDevice, owner);
		if (self == nullptr) {
			return kIOReturnSuccess;
		}
		uint8_t report[ONEIOS_ALS_REPORT_SIZE];
		oneios_als_report(self->fMillilux, report);
		IOBufferMemoryDescriptor *buffer = IOBufferMemoryDescriptor::withBytes(report, sizeof(report), kIODirectionNone);
		IOReturn result = buffer != nullptr ? self->handleReport(buffer, kIOHIDReportTypeInput, 0) : kIOReturnNoMemory;
		OSSafeReleaseNULL(buffer);
		return result;
	}
public:
	IOWorkLoop *getWorkLoop() const override { return fLoop; }
	OSString *newTransportString() const override { return OSString::withCString("Virtual"); }
	OSString *newProductString() const override { return OSString::withCString("OneiOS ambient light sensor"); }
	IOReturn newReportDescriptor(IOMemoryDescriptor **out) const override
	{
		if (out == nullptr) {
			return kIOReturnBadArgument;
		}
		*out = IOBufferMemoryDescriptor::withBytes(oneios_als_descriptor, sizeof(oneios_als_descriptor), kIODirectionNone);
		return *out != nullptr ? kIOReturnSuccess : kIOReturnNoMemory;
	}
	bool start(IOService *provider) override
	{
		fLoop = IOWorkLoop::workLoop();
		if (fLoop == nullptr) {
			return false;
		}
		/* What AppleUSBALSService's personality matches on the interface, beside the usages of the descriptor. */
		setProperty("VendorID", (unsigned long long)ONEIOS_ALS_VENDOR_ID, 32);
		setProperty("ProductID", (unsigned long long)ONEIOS_ALS_PRODUCT_ID, 32);
		setProperty("Built-In", true);
		if (!IOHIDDevice::start(provider)) {
			return false;
		}
		oneios_als_listen(&OneiOSALSDevice::reading, this);
		uint32_t now;
		if (oneios_als_read(&now)) {
			reading(this, now);
		}
		IOLog("ONEIOS ALS: on: HID ambient light sensor (Sensor/Ambient Light, millilux) fed by the host's readings\n");
		return true;
	}
	void stop(IOService *provider) override
	{
		oneios_als_listen(nullptr, nullptr);
		IOHIDDevice::stop(provider);
	}
	void free() override
	{
		OSSafeReleaseNULL(fLoop);
		IOHIDDevice::free();
	}
};
OSDefineMetaClassAndStructors(OneiOSALSDevice, IOHIDDevice)

extern "C" void
oneios_als_device_start(void)
{
	IOService *resources = IOService::getResourceService();
	OneiOSALSDevice *device = new OneiOSALSDevice;
	if (device == nullptr || resources == nullptr || !device->init() || !device->attach(resources)) {
		IOLog("ONEIOS ALS: off: the HID device could not be made\n");
		OSSafeReleaseNULL(device);
		return;
	}
	if (!device->start(resources)) {
		device->detach(resources);
		IOLog("ONEIOS ALS: off: the HID device did not start\n");
	} else {
		device->registerService();
	}
	device->release();
}
