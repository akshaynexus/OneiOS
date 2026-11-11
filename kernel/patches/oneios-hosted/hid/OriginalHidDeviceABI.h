// SPDX-License-Identifier: APSL-2.0
//
// OneiOS: the layout of the original IOHIDDevice of the iPhone18,2 iOS 27.0.1 (24A446) kernel collection
// (com.apple.iokit.IOHIDFamily, IOHIDFamily-2238.120.5), for drivers linked into the hosted kernel that subclass it:
// the touch screen and buttons (NativeHidInput, this folder) and the light sensor (drivers/brightness/
// OneiOSALSDevice.cpp). "v54" is the kernel collection's fixture name (kernel/prelink/*-24A446-v54.json).
//
// The declarations are those of Apple's public IOHIDDevice.h at the tag IOHIDFamily-2238.120.5 (the OS profile's pin,
// APSL 2.0; github.com/apple-oss-distributions/IOHIDFamily), reduced to what fixes the layout: the member variables
// (the instance size), every virtual function in declaration order (the vtable), and the types their signatures
// name (the pointer-authentication discriminator of a vtable slot is a hash of the mangled name of the function that
// introduced it, so a type spelled differently gives another discriminator). Private non-virtual functions are left
// out: nothing outside the family can call them.
//
// Evidence. The kernelcache has no symbols for a kext, so each fact is checked against what its code and data say
// (layout.py reads them into IOHIDDevice-24A446-v54.json; build-oneios-hid.py checks this header against that
// record at every build and refuses a mismatch):
//
//   instance size 0xd0     the class's static initializer passes 0xd0 to the OSMetaClass constructor
//                          (`mov w3, #0xd0` at 0xfffffe000a0bb29c). Here: sizeof(IOService) in this kernel (0x88)
//                          plus the members below (0x48), static_assert at the end.
//   207 vtable slots       the authenticated fixups from vtable + 16 (0xfffffe0008314590) on: 168 of IOService,
//                          39 of IOHIDDevice. The public header's 23 reserved slots are absent: a kernel for
//                          arm64 is built without vtable padding (APPLE_KEXT_VTABLE_PADDING 0), in this source too.
//   slot of each function  a class that overrides only newReportDescriptor is compiled against this header; every
//                          entry of its vtable must be signed with the discriminator the kernelcache's fixup of that
//                          slot has (all 207), and must name an IOHIDDevice function exactly where the kernelcache's
//                          slot points into IOHIDFamily's code (57 slots: 0, 1, 7, the 16 overrides of IOService's
//                          below, and 168..206 but 180).
//   slot 180 is abstract   its target is the kernel's __cxa_pure_virtual (the only slot from 168 on outside the
//                          family's code); slots 0 and 1, the destructors, are `bti c; brk #1`.
//   constructor            IOHIDDevice::IOHIDDevice(const OSMetaClass *) is the one function that stores
//                          vtable + 16: 0xfffffe000a0b3834 (`bti c; pacibsp; ...; bl <IOService's>`).
//   base destructor        0xfffffe000a0b386c, between it and getMetaClass: `bti c; b <IOService's>`.
//
// The slot numbers and discriminators, as the kernelcache has them (slot: function, discriminator):
//
//   IOService's slots that IOHIDDevice overrides
//     0, 1 destructors (0x3771, 0x56b7)   7 getMetaClass (0xfffc)        12 Dispatch (0x9209)
//     14 free (0xedc5)                    21 setProperty(OSSymbol, OSObject) (0x43aa)
//     23 init (0x4578)                    76 willTerminate (0xa51b)      77 didTerminate (0x4577)
//     81 configureReport (0xb049)         82 updateReport (0xc4b2)       86 start (0x3c68)
//     87 stop (0xb085)                    91 handleOpen (0x55eb)         92 handleClose (0xe933)
//     93 handleIsOpen (0xaec3)            101 matchPropertyTable (0xa27f)
//     132 message (0x7f14)                140 newUserClient (0x82cd)
//   IOHIDDevice's own
//     168 handleStart (0xfd21)            169 handleStop (0xa730)        170 publishProperties (0x2b56)
//     171 newTransportString (0x6be3)     172 newManufacturerString (0x86ed)
//     173 newProductString (0x4f55)       174 newVendorIDNumber (0x4897) 175 newProductIDNumber (0x01c8)
//     176 newVersionNumber (0x6408)       177 newSerialNumber (0xd333)   178 newPrimaryUsageNumber (0x3fbc)
//     179 newPrimaryUsagePageNumber (0x4e1a)                             180 newReportDescriptor (0x3f76), pure
//     181 handleReport (0xd548)           182 getReport/3 (0x9296)       183 setReport/3 (0xad83)
//     184 getMemoryWithCurrentElementValues (0x6d62)                     185 registerElement (0x6941)
//     186 startEventDelivery (0x0607)     187 stopEventDelivery (0x3df7) 188 checkEventDelivery (0xbc2d)
//     189 updateElementValues/2 (0x8d01)  190 postElementValues/2 (0x7733)
//     191 newSerialNumberString (0x1079)  192 newLocationIDNumber (0x54cf)
//     193 getReport/5 (0x78ae)            194 setReport/5 (0xda9e)       195 newVendorIDSourceNumber (0x6cc1)
//     196 newCountryCodeNumber (0xe70b)   197 handleReportWithTime (0xfc7f)
//     198 newReportIntervalNumber (0x2a3a)                               199 handleReportWithTimeAsync (0xc7e7)
//     200 newDeviceUsagePairs (0xe558)    201 createInterface (0x1bbd)   202 destroyInterface (0xde60)
//     203 conformsTo (0xb2ed)             204 completeReport (0x58a1)    205 updateElementValues/6 (0x15ed)
//     206 postElementValues/5 (0xb1e1)
//
// The discriminators are the same in the iPad17,3 24A446 collection (UniXNU-droidvm's hid-device-24A446.json): the
// two are builds of one source. The addresses are not.
//
// Not known from this header: what the members hold (only their sizes matter here; a subclass must not touch
// them), and the layout of IOHIDDevice::ExpansionData.
#ifndef ONEIOS_ORIGINAL_HID_DEVICE_ABI_H
#define ONEIOS_ORIGINAL_HID_DEVICE_ABI_H

#include <IOKit/IOService.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IOUserClient.h>
#include <IOKit/IOLocks.h>

#if !defined(__arm64e__) || !__has_feature(ptrauth_calls)
#error "OriginalHidDeviceABI.h describes the arm64e IOHIDDevice of the 24A446 iPhone18,2 kernel collection"
#endif

class IOHIDElementPrivate;
class IOHIDEventQueue;
class OSAction;
struct IOHIDReportHandler;

// IOHIDDeviceTypes.h (IOHIDFamily-2238.120.5), the types the virtual functions' signatures name.
enum IOHIDReportType {
	kIOHIDReportTypeInput = 0,
	kIOHIDReportTypeOutput,
	kIOHIDReportTypeFeature,
	kIOHIDReportTypeCount
};
typedef enum IOHIDReportType IOHIDReportType;
typedef uint32_t IOHIDElementCookie;
typedef void (*IOHIDCompletionAction)(void *target, void *parameter, IOReturn status, uint32_t bufferSizeRemaining);
typedef struct IOHIDCompletion {
	void *target;
	IOHIDCompletionAction action;
	void *parameter;
} IOHIDCompletion;

// What OSDeclareDefaultStructorsWithDispatch adds to the class: the generated header of the DriverKit interface
// (HIDDriverKit/IOHIDDevice.h, made by iig and not in the source tree) declares the kernel class's Dispatch
// override, its static _Dispatch and non-virtual remote-call helpers. Only Dispatch is in the vtable (slot 12).
#define IOHIDDevice_Methods \
public: \
	virtual kern_return_t Dispatch(const IORPC rpc) APPLE_KEXT_OVERRIDE; \
	static kern_return_t _Dispatch(IOHIDDevice *self, const IORPC rpc);
#define IOHIDDevice_KernelMethods

class IOHIDDevice : public IOService
{
	OSDeclareDefaultStructorsWithDispatch(IOHIDDevice)

private:
	// 0x48 bytes after IOService's 0x88 (IOHIDDevice.h's members, in its order).
	OSArray *                   _elementArray;
	UInt32                      _dataElementIndex;
	IORecursiveLock *           _elementLock;
	IOHIDReportHandler *        _reportHandlers;
	IOBufferMemoryDescriptor *  _elementValuesDescriptor;
	bool                        _readyForInputReports;
	UInt32                      _reportCount;
	UInt32                      _maxInputReportSize;
	UInt32                      _maxOutputReportSize;
	UInt32                      _maxFeatureReportSize;
	struct ExpansionData;
	ExpansionData *             _reserved;

	IOReturn configureReport(IOReportChannelList *channels, IOReportConfigureAction action, void *result, void *destination) APPLE_KEXT_OVERRIDE;
	IOReturn updateReport(IOReportChannelList *channels, IOReportUpdateAction action, void *result, void *destination) APPLE_KEXT_OVERRIDE;

protected:
	virtual void free() APPLE_KEXT_OVERRIDE;
	virtual bool handleOpen(IOService *client, IOOptionBits options, void *argument) APPLE_KEXT_OVERRIDE;
	virtual void handleClose(IOService *client, IOOptionBits options) APPLE_KEXT_OVERRIDE;
	virtual bool handleIsOpen(const IOService *client) const APPLE_KEXT_OVERRIDE;
	virtual bool handleStart(IOService *provider);
	virtual void handleStop(IOService *provider);
	virtual IOReturn newUserClient(task_t owningTask, void *security_id, UInt32 type, OSDictionary *properties,
	    IOUserClient **handler) APPLE_KEXT_OVERRIDE;
	virtual bool publishProperties(IOService *provider);

public:
	virtual bool init(OSDictionary *dictionary = 0) APPLE_KEXT_OVERRIDE;
	virtual bool start(IOService *provider) APPLE_KEXT_OVERRIDE;
	virtual void stop(IOService *provider) APPLE_KEXT_OVERRIDE;
	virtual bool willTerminate(IOService *provider, IOOptionBits options) APPLE_KEXT_OVERRIDE;
	virtual bool didTerminate(IOService *provider, IOOptionBits options, bool *defer) APPLE_KEXT_OVERRIDE;
	virtual bool matchPropertyTable(OSDictionary *table, SInt32 *score) APPLE_KEXT_OVERRIDE;
	virtual IOReturn message(UInt32 type, IOService *provider, void *argument = 0) APPLE_KEXT_OVERRIDE;
	using IOService::setProperty;
	virtual bool setProperty(const OSSymbol *aKey, OSObject *anObject) APPLE_KEXT_OVERRIDE;

	virtual OSString *newTransportString() const;
	virtual OSString *newManufacturerString() const;
	virtual OSString *newProductString() const;
	virtual OSNumber *newVendorIDNumber() const;
	virtual OSNumber *newProductIDNumber() const;
	virtual OSNumber *newVersionNumber() const;
	virtual OSNumber *newSerialNumber() const;
	virtual OSNumber *newPrimaryUsageNumber() const;
	virtual OSNumber *newPrimaryUsagePageNumber() const;
	// The device's HID report descriptor. The one function a subclass must give.
	virtual IOReturn newReportDescriptor(IOMemoryDescriptor **descriptor) const = 0;
	// An input report from the device, on the device's work loop: the family parses it and hands the element values
	// to its clients (IOHIDInterface, and through it the event driver).
	virtual IOReturn handleReport(IOMemoryDescriptor *report, IOHIDReportType reportType = kIOHIDReportTypeInput,
	    IOOptionBits options = 0);
	virtual IOReturn getReport(IOMemoryDescriptor *report, IOHIDReportType reportType, IOOptionBits options);
	virtual IOReturn setReport(IOMemoryDescriptor *report, IOHIDReportType reportType, IOOptionBits options = 0);
	virtual IOMemoryDescriptor *getMemoryWithCurrentElementValues() const;
	virtual bool registerElement(IOHIDElementPrivate *element, IOHIDElementCookie *cookie);
	virtual IOReturn startEventDelivery(IOHIDEventQueue *queue, IOHIDElementCookie cookie, IOOptionBits options = 0);
	virtual IOReturn stopEventDelivery(IOHIDEventQueue *queue, IOHIDElementCookie cookie = 0);
	virtual IOReturn checkEventDelivery(IOHIDEventQueue *queue, IOHIDElementCookie cookie, bool *isActive);
	virtual IOReturn updateElementValues(IOHIDElementCookie *cookies, UInt32 cookieCount = 1);
	virtual IOReturn postElementValues(IOHIDElementCookie *cookies, UInt32 cookieCount = 1);
	virtual OSString *newSerialNumberString() const;
	virtual OSNumber *newLocationIDNumber() const;
	virtual IOReturn getReport(IOMemoryDescriptor *report, IOHIDReportType reportType, IOOptionBits options,
	    UInt32 completionTimeout, IOHIDCompletion *completion = 0);
	virtual IOReturn setReport(IOMemoryDescriptor *report, IOHIDReportType reportType, IOOptionBits options,
	    UInt32 completionTimeout, IOHIDCompletion *completion = 0);
	virtual OSNumber *newVendorIDSourceNumber() const;
	virtual OSNumber *newCountryCodeNumber() const;
	virtual IOReturn handleReportWithTime(AbsoluteTime timeStamp, IOMemoryDescriptor *report,
	    IOHIDReportType reportType = kIOHIDReportTypeInput, IOOptionBits options = 0);
	virtual OSNumber *newReportIntervalNumber() const;
	virtual IOReturn handleReportWithTimeAsync(AbsoluteTime timeStamp, IOMemoryDescriptor *report,
	    IOHIDReportType reportType, IOOptionBits options, UInt32 completionTimeout, IOHIDCompletion *completion);
	virtual OSArray *newDeviceUsagePairs();

protected:
	virtual bool createInterface(IOOptionBits options = 0);
	virtual void destroyInterface(IOOptionBits options = 0);
	virtual bool conformsTo(UInt32 usagePage, UInt32 usage);
	virtual void completeReport(OSAction *action, IOReturn status, uint32_t actualByteCount);
	virtual IOReturn updateElementValues(IOHIDElementCookie *cookies, UInt32 cookieCount, IOOptionBits options,
	    UInt32 completionTimeout, IOHIDCompletion *completion = 0, IOBufferMemoryDescriptor *elementData = 0);
	virtual IOReturn postElementValues(IOHIDElementCookie *cookies, UInt32 cookieCount, IOOptionBits options,
	    UInt32 completionTimeout, IOHIDCompletion *completion = 0);
};

static_assert(sizeof(IOService) == 0x88, "IOService of the 24A446 collection's kernel: 0x88 bytes");
static_assert(sizeof(IOHIDDevice) == 0xd0, "the original IOHIDDevice: 0xd0 bytes (its static initializer's size)");

#endif /* ONEIOS_ORIGINAL_HID_DEVICE_ABI_H */
