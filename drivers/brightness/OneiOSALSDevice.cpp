/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS: the host's ambient light sensor as a HID device, in one of two shapes (oneios_als_hid.h has both
 * descriptors and says why):
 *   oneios-als-spu=0 (the default): a HID Sensor page device (0x20/0x41) for the original AppleUSBALSService
 *     (com.apple.driver.AppleHIDALSService, in the kernel collection) to turn into ambient light events. It attaches in
 *     a PC boot; on the phone no lux reached CoreBrightness (README.md).
 *   oneios-als-spu=1 (sensors-als, 2026-11-25; xnuhost adds it when the light comes from the sensor hub): an iPhone's
 *     sensor processor's ambient light sensor, the Apple vendor pair {0xff00, 4} on the SPU transport, Built-In, VendorID
 *     0x5ac, with ALSCurveInfo and Placement 0 (Jesvi-iOS a13e406 userland/src/inject_sensors.c, MIT: what CoreBrightness
 *     checks), under the collection's IOHIDEventDriver (OneiOSHidInput.cpp's personality), whose dispatch
 *     kernel/patches/oneios-hosted/hid/OneiOSALSEvents.cpp wraps to make each reading an ambient light event. The last
 *     reading is cached and sent again within 15 ms when CoreBrightness's ReportInterval goes from 0 to more (a wake)
 *     and 20 ms after the driver is wrapped, as ios-redfin's 1298 does (23019ab4, MIT), so CoreBrightness's 250 ms
 *     FirstALSSampleTimeout does not start it at a fixed level.
 *
 * It is a subclass of the kernel collection's own IOHIDDevice: the header "OriginalHidDeviceABI.h" is that
 * class's layout for the 24A446 iPhone18,2 collection (kernel/patches/oneios-hosted/hid). So it is built there, into
 * the hosted kernel's HID archive (oneios-tool kernel gen hid), not by drivers/build.py: its class registers after the
 * collection's IOHIDFamily has loaded, and oneios_als_device_start() is called by oneios_hid_input_start
 * (OneiOSHidInput.cpp) once the touch screen and the buttons are published. Written on NativeHidInput's pattern,
 * with only the calls that file uses.
 *
 * Unknown until a session reaches backboardd [U]: whether AppleUSBALSService takes a device that is not on USB and
 * which report fields it reads; whether CoreBrightness takes the SPU shape's service and reads its level or its
 * illuminance on 24A446.
 */
#include "OriginalHidDeviceABI.h"
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOTimerEventSource.h>
#include <IOKit/IOWorkLoop.h>
#include <libkern/c++/OSDictionary.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSString.h>
#include <pexpert/pexpert.h>

#include "oneios_bridge.h"
#include "oneios_als_hid.h"

extern "C" {
// OneiOSALSEvents.cpp's counts: [0] events made ambient light events, [1] other events of the driver.
extern volatile uint64_t oneios_als_spu_events[2];
void oneios_als_spu_wrapped(IORegistryEntry *driver);
}

/* Auto-brightness's lux -> nits curve, which CoreBrightness asks the ALS service for (Jesvi-iOS inject_sensors.c: without it
 * "Failed to obtain ALS Curve info"): E (lux) and L (nits) points; L2 is the top iOS is told the S24 Ultra's panel has
 * (ONEIOS_BRIGHT_NITS, 500). Integers: the kernel has no floating point, and CFNumberGetValue converts. */
static OSDictionary *
oneios_als_curve(void)
{
	static const char *const keys[] = {"E0a", "E0b", "E1", "E2", "L0a", "L0b", "L1", "L2"};
	static const uint32_t values[] = {0, 10, 150, 1500, 3, 30, 150, 500};
	OSDictionary *curve = OSDictionary::withCapacity(9), *prefs = OSDictionary::withCapacity(1);
	OSNumber *zero = OSNumber::withNumber(0ULL, 32);
	if (!curve || !prefs || !zero) {
		OSSafeReleaseNULL(curve);
		OSSafeReleaseNULL(prefs);
		OSSafeReleaseNULL(zero);
		return nullptr;
	}
	for (unsigned i = 0; i < sizeof(keys) / sizeof(keys[0]); i++) {
		OSNumber *n = OSNumber::withNumber((unsigned long long)values[i], 32);
		if (n) {
			curve->setObject(keys[i], n);
			n->release();
		}
	}
	prefs->setObject("thirdSlope", zero);
	curve->setObject("Prefs", prefs);
	prefs->release();
	zero->release();
	return curve;
}

/* What CoreBrightness checks on the service (Jesvi-iOS): Built-In, Apple's VendorID, the curve; Placement 0 (redfin 1298). */
static void
oneios_als_spu_properties(IORegistryEntry *entry)
{
	entry->setProperty("Built-In", true);
	entry->setProperty("VendorID", (unsigned long long)ONEIOS_ALS_VENDOR_ID, 32);
	entry->setProperty("Placement", 0ULL, 32);
	OSDictionary *curve = oneios_als_curve();
	if (curve) {
		entry->setProperty("ALSCurveInfo", curve);
		curve->release();
	}
}

class OneiOSALSDevice : public IOHIDDevice {
	OSDeclareDefaultStructors(OneiOSALSDevice)
	IOWorkLoop *fLoop = nullptr;
	IOTimerEventSource *fRepost = nullptr, *fMinute = nullptr;
	volatile uint32_t fMillilux = 0;
	volatile bool fHaveReading = false;
	bool fSpu = false;
	uint32_t fInterval = 0;          /* CoreBrightness's ReportInterval (us), from the feature report */
	uint32_t fWakeInterval = 0;      /* the interval of the wake whose re-post is pending (0: an open, the driver wrapped) */
	uint64_t fReadings = 0, fReadingsShown = 0, fEventsShown = 0;

	/* From the bridge's worker thread: the report is sent on this device's work loop. */
	static void reading(void *context, uint32_t millilux)
	{
		OneiOSALSDevice *self = static_cast<OneiOSALSDevice *>(context);
		if (self == nullptr || self->fLoop == nullptr) {
			return;
		}
		self->fMillilux = millilux;
		self->fHaveReading = true;
		self->fReadings = self->fReadings + 1;
		(void)self->fLoop->runAction(&OneiOSALSDevice::send, self);
	}
	static IOReturn send(OSObject *owner, void *, void *, void *, void *)
	{
		OneiOSALSDevice *self = OSDynamicCast(OneiOSALSDevice, owner);
		return self != nullptr ? self->sendNow() : kIOReturnSuccess;
	}
	IOReturn sendNow()
	{
		uint8_t report[ONEIOS_ALS_SPU_REPORT_SIZE > ONEIOS_ALS_REPORT_SIZE ? ONEIOS_ALS_SPU_REPORT_SIZE : ONEIOS_ALS_REPORT_SIZE];
		size_t size;
		if (fSpu) {
			oneios_als_spu_report(fMillilux, report);
			size = ONEIOS_ALS_SPU_REPORT_SIZE;
		} else {
			oneios_als_report(fMillilux, report);
			size = ONEIOS_ALS_REPORT_SIZE;
		}
		IOBufferMemoryDescriptor *buffer = IOBufferMemoryDescriptor::withBytes(report, size, kIODirectionNone);
		IOReturn result = buffer != nullptr ? handleReport(buffer, kIOHIDReportTypeInput, 0) : kIOReturnNoMemory;
		OSSafeReleaseNULL(buffer);
		return result;
	}
	/* The cached reading again (a wake, or the driver just wrapped): on the work loop. */
	static void repost(OSObject *owner, IOTimerEventSource *)
	{
		OneiOSALSDevice *self = OSDynamicCast(OneiOSALSDevice, owner);
		if (self == nullptr || !self->fHaveReading) {
			return;
		}
		self->sendNow();
		if (self->fWakeInterval) {
			IOLog("ONEIOS ALS: wake: the cached sample re-posted (ReportInterval 0 -> %u us, %u lux)\n", self->fWakeInterval,
			    self->fMillilux / 1000);
			self->fWakeInterval = 0;
		}
	}
	/* Every 60 s, while readings come: what arrived and what went on. */
	static void minute(OSObject *owner, IOTimerEventSource *source)
	{
		OneiOSALSDevice *self = OSDynamicCast(OneiOSALSDevice, owner);
		if (self == nullptr) {
			return;
		}
		source->setTimeoutMS(60000);
		const uint64_t readings = self->fReadings, events = oneios_als_spu_events[0];
		if (readings != self->fReadingsShown) {
			IOLog("ONEIOS ALS: 60 s: %llu readings from the host, %llu events dispatched, last %u lux, ReportInterval %u us\n",
			    readings - self->fReadingsShown, self->fSpu ? events - self->fEventsShown : 0ULL, self->fMillilux / 1000,
			    self->fInterval);
		}
		self->fReadingsShown = readings;
		self->fEventsShown = events;
	}
public:
	void setSpu(bool spu) { fSpu = spu; }
	bool spu() const { return fSpu; }
	/* The event driver was wrapped: its properties, and the cached reading within 20 ms (redfin 1298's open). */
	void wrapped(IORegistryEntry *driver)
	{
		oneios_als_spu_properties(driver);
		if (fRepost) {
			fRepost->setTimeoutMS(20);
		}
	}
	IOWorkLoop *getWorkLoop() const override { return fLoop; }
	OSString *newTransportString() const override { return OSString::withCString(fSpu ? "SPU" : "Virtual"); }
	OSString *newProductString() const override { return OSString::withCString("OneiOS ambient light sensor"); }
	IOReturn newReportDescriptor(IOMemoryDescriptor **out) const override
	{
		if (out == nullptr) {
			return kIOReturnBadArgument;
		}
		*out = fSpu ? IOBufferMemoryDescriptor::withBytes(oneios_als_spu_descriptor, sizeof(oneios_als_spu_descriptor), kIODirectionNone)
		    : IOBufferMemoryDescriptor::withBytes(oneios_als_descriptor, sizeof(oneios_als_descriptor), kIODirectionNone);
		return *out != nullptr ? kIOReturnSuccess : kIOReturnNoMemory;
	}
	/* The SPU shape's feature report: CoreBrightness's ReportInterval, through IOHIDEventDriver::setProperties. */
	IOReturn setReport(IOMemoryDescriptor *report, IOHIDReportType type, IOOptionBits options) override
	{
		if (!fSpu || type != kIOHIDReportTypeFeature || report == nullptr) {
			return IOHIDDevice::setReport(report, type, options);
		}
		uint8_t bytes[ONEIOS_ALS_SPU_FEATURE_SIZE] = {0};
		const uint32_t length = (uint32_t)report->readBytes(0, bytes, sizeof(bytes));
		const int64_t interval = oneios_als_spu_interval(bytes, length);
		if (interval < 0) {
			return kIOReturnBadArgument;
		}
		const uint32_t before = fInterval;
		fInterval = (uint32_t)interval;
		if (oneios_als_spu_is_wake(before, fInterval) && fRepost) {
			fWakeInterval = fInterval;
			fRepost->setTimeoutMS(10); /* within 15 ms: before CoreBrightness's 250 ms FirstALSSampleTimeout */
		}
		return kIOReturnSuccess;
	}
	IOReturn getReport(IOMemoryDescriptor *report, IOHIDReportType type, IOOptionBits options) override
	{
		if (!fSpu || type != kIOHIDReportTypeFeature || report == nullptr) {
			return IOHIDDevice::getReport(report, type, options);
		}
		const uint8_t bytes[ONEIOS_ALS_SPU_FEATURE_SIZE] = {ONEIOS_ALS_SPU_FEATURE_ID, (uint8_t)fInterval, (uint8_t)(fInterval >> 8),
			                                            (uint8_t)(fInterval >> 16), (uint8_t)(fInterval >> 24)};
		report->writeBytes(0, bytes, sizeof(bytes));
		return kIOReturnSuccess;
	}
	bool start(IOService *provider) override
	{
		fLoop = IOWorkLoop::workLoop();
		if (fLoop == nullptr) {
			return false;
		}
		/* What AppleUSBALSService's personality matches on the interface, beside the usages of the descriptor; and what
		 * CoreBrightness checks on an SPU light sensor. */
		setProperty("VendorID", (unsigned long long)ONEIOS_ALS_VENDOR_ID, 32);
		setProperty("ProductID", (unsigned long long)ONEIOS_ALS_PRODUCT_ID, 32);
		setProperty("Built-In", true);
		if (fSpu) {
			oneios_als_spu_properties(this);
		}
		if (!IOHIDDevice::start(provider)) {
			return false;
		}
		fRepost = IOTimerEventSource::timerEventSource(this, &OneiOSALSDevice::repost);
		fMinute = IOTimerEventSource::timerEventSource(this, &OneiOSALSDevice::minute);
		if (fRepost && fLoop->addEventSource(fRepost) != kIOReturnSuccess) {
			OSSafeReleaseNULL(fRepost);
		}
		if (fMinute && fLoop->addEventSource(fMinute) == kIOReturnSuccess) {
			fMinute->setTimeoutMS(60000);
		} else {
			OSSafeReleaseNULL(fMinute);
		}
		oneios_als_listen(&OneiOSALSDevice::reading, this);
		uint32_t now;
		if (oneios_als_read(&now)) {
			reading(this, now);
		}
		if (fSpu) {
			IOLog("ONEIOS ALS: SPU ambient light service (0xff00/4) published: Transport SPU, Built-In, VendorID 0x5ac, "
			    "ALSCurveInfo; its event driver's dispatch makes ambient light events (oneios-als-spu=1)\n");
		} else {
			IOLog("ONEIOS ALS: on: HID ambient light sensor (Sensor/Ambient Light, millilux) fed by the host's readings\n");
		}
		return true;
	}
	void stop(IOService *provider) override
	{
		oneios_als_listen(nullptr, nullptr);
		IOTimerEventSource **timers[2] = {&fRepost, &fMinute};
		for (IOTimerEventSource **timer : timers) {
			if (*timer) {
				(*timer)->cancelTimeout();
				fLoop->removeEventSource(*timer);
				OSSafeReleaseNULL(*timer);
			}
		}
		IOHIDDevice::stop(provider);
	}
	void free() override
	{
		OSSafeReleaseNULL(fLoop);
		IOHIDDevice::free();
	}
};
OSDefineMetaClassAndStructors(OneiOSALSDevice, IOHIDDevice)

/* The device of this boot (it lives for the kernel's life), for oneios_als_spu_wrapped. */
static OneiOSALSDevice *volatile gOneiOSALSDevice;

extern "C" void
oneios_als_spu_wrapped(IORegistryEntry *driver)
{
	OneiOSALSDevice *device = gOneiOSALSDevice;
	if (device != nullptr && driver != nullptr) {
		device->wrapped(driver);
	}
}

extern "C" void
oneios_als_device_start(void)
{
	uint32_t spu = 0;
	PE_parse_boot_argn("oneios-als-spu", &spu, sizeof(spu));
	IOService *resources = IOService::getResourceService();
	OneiOSALSDevice *device = new OneiOSALSDevice;
	if (device != nullptr) {
		device->setSpu(spu != 0);
	}
	if (device == nullptr || resources == nullptr || !device->init() || !device->attach(resources)) {
		IOLog("ONEIOS ALS: off: the HID device could not be made\n");
		OSSafeReleaseNULL(device);
		return;
	}
	if (!device->start(resources)) {
		device->detach(resources);
		IOLog("ONEIOS ALS: off: the HID device did not start\n");
	} else {
		gOneiOSALSDevice = device; /* the registry keeps it */
		device->registerService();
	}
	device->release();
}
