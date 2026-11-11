// SPDX-License-Identifier: BSD-2-Clause-Patent
// OneiOS, hosted kernel: the touch screen and the buttons of the iOS 27 guest, and the light sensor's HID device.
// After UniXNU-droidvm's package/HidInputProbe.cpp (BSD-2-Clause-Patent), which publishes NativeHidInput's devices
// on the iPadOS 27 board; this is that start for a collection that runs slid, without the board's other devices
// (pen, trackpad, motion sensors) and without boot arguments to ask for it.
//
// What is published, under the collection's IOHIDResource:
//   NativeHidInput "OneiOS touch screen"  Digitizer / Touch Screen, ten fingers, Built-In, DisplayIntegrated:
//                                         the service backboardd takes touches from
//   NativeHidInput "OneiOS buttons"       Consumer Menu (Home), Power, Volume Increment and Decrement, Telephony
//                                         Ring Enable, and a boot keyboard
//   OneiOSALSDevice                       drivers/brightness: Sensor / Ambient Light, under IOResources
// The first two are fed by the staged DroidVMVirtioInput.c from xnuhost's virtio-input device (/arm-io/input-N).
//
// NativeHidInput and OneiOSALSDevice subclass the original IOHIDDevice of com.apple.iokit.IOHIDFamily
// (OriginalHidDeviceABI.h). Its functions are reached through the generated stubs (build-oneios-hid.py), its
// metaclass is an absolute symbol whose value is the link address; as OneiOSSoftwareAES.cpp does for
// IOAESAccelerator, the bound metaclass is compared with the live one after adding the slide, and each owned
// metaclass gets the live one as its superclass before the module load is finished.
//
// Boot arguments: oneios-hid=0 starts nothing; oneios-hid-log=0 keeps the once-a-second state line quiet
// (default 1: it is how a boot shows that backboardd opened the digitizer and that events arrive).
#include <IOKit/IOLib.h>
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <kern/thread.h>
#include <libkern/OSKextLib.h>
#include <libkern/c++/OSIterator.h>
#include <libkern/c++/OSKext.h>
#include <libkern/c++/OSMetaClass.h>
#include <libkern/c++/OSSerialize.h>
#include <mach/kmod.h>
#include <pexpert/pexpert.h>

extern kmod_info_t g_kernel_kmod_info;
extern vm_offset_t vm_kernel_slide;
extern "C" {
// The staged sources' late static initializers (their objects have no __mod_init_func).
void oneios_hid_static_init(void) __asm__("__GLOBAL__sub_I_NativeHidInput.cpp");
void oneios_als_static_init(void) __asm__("__GLOBAL__sub_I_OneiOSALSDevice.cpp");
extern char oneios_hid_native_meta[] __asm__("__ZN14NativeHidInput10gMetaClassE");
extern char oneios_hid_als_meta[] __asm__("__ZN15OneiOSALSDevice10gMetaClassE");
// The generated bindings: the parent metaclass's link address and instance size as plain numbers (no fixup).
extern const uint64_t oneios_hid_parent_meta_link;
extern const uint32_t oneios_hid_parent_size;

IOService *native_hid_input_allocate(void);
IOService *native_hid_touch_allocate(void);
void droidvm_input_start(void);
void oneios_als_device_start(void);
void oneios_hid_input_start(void);

// DroidVMVirtioInput.c's counters: devices found and started, events, frames, finger sets published.
extern volatile uint32_t droidvm_input_found, droidvm_input_started;
extern volatile uint64_t droidvm_input_events, droidvm_input_frames, droidvm_input_published;
extern volatile uint64_t oneios_hid_key_events, oneios_hid_button_reports;
// NativeHidInput's: the wrapper of the event driver's dispatch ([0] 3 = installed), contacts begun and at an edge.
extern volatile uint32_t native_touch_edge_install[3], native_touch_edge_stats[4];

// What the two sources share (kits/sheng/TouchProbe.cpp and DroidVMStubs.c on their own boards): the finger set
// the transport publishes for the digitizer, a seqlock (odd while written).
volatile uint32_t native_touch_seq, native_touch_count, native_touch_hid_reports;
volatile int native_touch_xs[12], native_touch_ys[12], native_touch_ids[12];

// 1 the family loaded, 2 the parent checked, 3 the classes registered, 4 IOHIDResource found, 5 devices published;
// 0x80 | n refused at step n; 0x40 no such class (the kernel is not in a kernel collection).
volatile uint32_t oneios_hid_stage;
// NativeHidInput's observations (HidInputProbe.cpp's numbering): [2] started, [3] the 1 s tick with an event driver,
// [4] a buttons report, [6] a touch report; the count and the last result of each.
volatile uint32_t native_hid_input_events[8];
volatile int native_hid_input_results[8] = {-1, -1, -1, -1, -1, -1, -1, -1};

void native_hid_input_observed(unsigned event, int result);
void native_hid_live_checkpoint(IOService *device);
bool native_hid_input_requested(void);
void droidvm_motion_register(void);
}

static uint32_t oneios_hid_log = 1;

void
native_hid_input_observed(unsigned event, int result)
{
	if (event < 8) {
		native_hid_input_events[event] = native_hid_input_events[event] + 1;
		native_hid_input_results[event] = result;
	}
}

// NativeHidInput's own Home press (a timer's down and up, asked for by its board's test rig): never here. Home
// comes from the button sink, or from the edge swipe.
bool
native_hid_input_requested(void)
{
	return false;
}

// The transport registers a sysctl for motion sensors when it finds some: xnuhost has none.
void
droidvm_motion_register(void)
{
}

// The IOHIDEventDriver below a device, if one has attached.
static IORegistryEntry *
eventDriver(IORegistryEntry *parent, unsigned depth)
{
	if (!depth) {
		return nullptr;
	}
	OSIterator *it = parent->getChildIterator(gIOServicePlane);
	if (!it) {
		return nullptr;
	}
	IORegistryEntry *found = nullptr;
	while (OSObject *child = it->getNextObject()) {
		IORegistryEntry *entry = OSDynamicCast(IORegistryEntry, child);
		if (!entry) {
			continue;
		}
		found = entry->metaCast("IOHIDEventDriver") ? entry : eventDriver(entry, depth - 1);
		if (found) {
			break;
		}
	}
	it->release();
	return found;
}

// One record per device (two here): what was last said about it, so that each change is logged once.
struct DeviceState {
	IOService *device;
	IORegistryEntry *driver;
	bool published;
	unsigned clients;
	uint64_t reports;
	unsigned long long queued;
};
static DeviceState states[4];

static DeviceState *
stateOf(IOService *device)
{
	for (DeviceState &state : states) {
		if (state.device == device) {
			return &state;
		}
	}
	for (DeviceState &state : states) {
		if (!state.device) {
			state.device = device;
			return &state;
		}
	}
	return nullptr;
}

// The user clients below an event driver, each with the process that made it ("pid N, name").
static unsigned
describeClients(IORegistryEntry *driver, char *text, size_t size)
{
	unsigned count = 0;
	size_t used = 0;
	text[0] = 0;
	OSIterator *it = driver->getChildIterator(gIOServicePlane);
	if (!it) {
		return 0;
	}
	while (OSObject *child = it->getNextObject()) {
		IORegistryEntry *entry = OSDynamicCast(IORegistryEntry, child);
		if (!entry || !entry->metaCast("IOUserClient")) {
			continue;
		}
		OSObject *creator = entry->copyProperty("IOUserClientCreator");
		OSString *name = OSDynamicCast(OSString, creator);
		int n = snprintf(text + used, size - used, "%s%s (%s)", count ? ", " : "", entry->getMetaClass()->getClassName(),
		    name ? name->getCStringNoCopy() : "?");
		if (n > 0 && (size_t)n < size - used) {
			used += n;
		}
		OSSafeReleaseNULL(creator);
		count++;
	}
	it->release();
	return count;
}

// A number of a user client's "DebugState" (IOHIDEventServiceUserClient::serializeDebugState): the property is a
// serializer, read as its XML text (`<key>NAME</key><integer size="64">0x1a</integer>`). -1 when it is not there
// (the family leaves a count out while it is 0).
static long long
debugNumber(const char *text, const char *name)
{
	char key[64];
	snprintf(key, sizeof(key), "<key>%s</key>", name);
	const char *at = text ? strnstr(text, key, strlen(text)) : nullptr;
	at = at ? strchr(at + strlen(key), '>') : nullptr;
	return at ? (long long)strtouq(at + 1, nullptr, 0) : -1;
}

// What the event service's user clients say they queued for their process: the events Apple's event driver made of
// our reports and handed on, and those it dropped because the process's queue was full.
static void
queueCounts(IORegistryEntry *driver, unsigned long long *queued, unsigned long long *dropped)
{
	*queued = *dropped = 0;
	OSIterator *it = driver->getChildIterator(gIOServicePlane);
	if (!it) {
		return;
	}
	while (OSObject *child = it->getNextObject()) {
		IORegistryEntry *entry = OSDynamicCast(IORegistryEntry, child);
		if (!entry || !entry->metaCast("IOUserClient")) {
			continue;
		}
		OSObject *state = entry->copyProperty("DebugState");
		OSSerialize *text = state ? OSSerialize::withCapacity(2048) : nullptr;
		if (text && state->serialize(text)) {
			const long long count = debugNumber(text->text(), "EnqueueEventCount"), lost = debugNumber(text->text(), "DroppedEventCount");
			*queued += count > 0 ? count : 0;
			*dropped += lost > 0 ? lost : 0;
		}
		OSSafeReleaseNULL(text);
		OSSafeReleaseNULL(state);
	}
	it->release();
}

// Called by each NativeHidInput once a second, on its work loop (its tick).
//
// backboardd classifies a digitizer by the event service it opened, not by the HID device below it: it drops a
// digitizer event unless the sending service has PrimaryUsagePage 0x0D and PrimaryUsage 1, 4 or 0x20, and its
// digitizer filter is {DeviceUsagePage, DeviceUsage, Built-In} (HidInputProbe.cpp, decoded there from backboardd).
// So Built-In, DisplayIntegrated and the usage pair are set on the touch screen's event driver and on its
// interface once, when the driver has attached. Once only: every setProperty on a service with user clients is a
// registry notification.
void
native_hid_live_checkpoint(IOService *device)
{
	DeviceState *state = stateOf(device);
	if (!state) {
		return;
	}
	const bool touch = device->getProperty("Built-In") != nullptr;
	const char *what = touch ? "touch screen" : "buttons";
	IORegistryEntry *driver = eventDriver(device, 4);
	if (driver && driver != state->driver) {
		state->driver = driver;
		state->published = false;
		state->clients = 0;
		IOLog("oneios-hid: %s: event driver %s attached (registry id 0x%llx)\n", what, driver->getMetaClass()->getClassName(),
		    driver->getRegistryEntryID());
	}
	if (!driver) {
		return;
	}
	if (touch && !state->published) {
		state->published = true;
		driver->setProperty("Built-In", kOSBooleanTrue);
		driver->setProperty("DisplayIntegrated", kOSBooleanTrue);
		driver->setProperty("PrimaryUsagePage", (unsigned long long)0x0d, 32);
		driver->setProperty("PrimaryUsage", (unsigned long long)4, 32);
		if (IORegistryEntry *interface = driver->getParentEntry(gIOServicePlane)) {
			interface->setProperty("Built-In", kOSBooleanTrue);
			interface->setProperty("DisplayIntegrated", kOSBooleanTrue);
			interface->setProperty("PrimaryUsagePage", (unsigned long long)0x0d, 32);
			interface->setProperty("PrimaryUsage", (unsigned long long)4, 32);
		}
		IOLog("oneios-hid: touch screen: Digitizer/Touch Screen, Built-In, DisplayIntegrated published on the event service; "
		    "edge wrapper %u\n", native_touch_edge_install[0]);
	}
	if (!oneios_hid_log) {
		return;
	}
	char clients[256];
	const unsigned count = describeClients(driver, clients, sizeof(clients));
	if (count != state->clients) {
		state->clients = count;
		IOLog("oneios-hid: %s: %u user client(s) on the event service: %s\n", what, count, count ? clients : "none");
	}
	const uint64_t reports = touch ? native_hid_input_events[6] : native_hid_input_events[4];
	if (reports != state->reports) {
		state->reports = reports;
		unsigned long long queued = 0, dropped = 0;
		if (count) {
			queueCounts(driver, &queued, &dropped);
		}
		if (queued != state->queued) {
			state->queued = queued;
			IOLog("oneios-hid: %s: %llu HID events queued for the service's client(s), %llu dropped\n", what, queued, dropped);
		}
		if (touch) {
			IOLog("oneios-hid: touch screen: %llu virtio-input events, %llu frames, %llu finger sets, %llu HID reports "
			    "(last result 0x%x), %u contacts begun (%u at an edge)\n", droidvm_input_events, droidvm_input_frames,
			    droidvm_input_published, reports, native_hid_input_results[6], native_touch_edge_stats[1],
			    native_touch_edge_stats[2]);
		} else {
			IOLog("oneios-hid: buttons: %llu key events, %llu button states, %llu HID reports (last result 0x%x)\n",
			    oneios_hid_key_events, oneios_hid_button_reports, reports, native_hid_input_results[4]);
		}
	}
}

// The services below an entry, as "Class(Class(...), Class)".
static void
describeBelow(IORegistryEntry *entry, unsigned depth, char *text, size_t size, size_t &used)
{
	OSIterator *it = depth ? entry->getChildIterator(gIOServicePlane) : nullptr;
	if (!it) {
		return;
	}
	bool first = true;
	while (OSObject *child = it->getNextObject()) {
		IORegistryEntry *below = OSDynamicCast(IORegistryEntry, child);
		if (!below || used + 80 >= size) {
			continue;
		}
		used += snprintf(text + used, size - used, "%s%s", first ? "(" : ", ", below->getMetaClass()->getClassName());
		first = false;
		describeBelow(below, depth - 1, text, size, used);
	}
	if (!first && used + 2 < size) {
		text[used++] = ')';
		text[used] = 0;
	}
	it->release();
}

// The light sensor's device has no tick of its own: for half an hour, say what is attached below it each time
// that changes (the interface, then whichever event service takes it: AppleUSBALSService is the one hoped for).
static void
watchLightSensor(void)
{
	OSDictionary *match = IOService::serviceMatching("OneiOSALSDevice");
	IOService *device = match ? IOService::waitForMatchingService(match, 5ULL * NSEC_PER_SEC) : nullptr;
	OSSafeReleaseNULL(match);
	if (!device) {
		return;
	}
	char last[512] = "", now[512];
	for (unsigned waited = 0; waited < 1800 && oneios_hid_log; waited += 5) {
		size_t used = 0;
		now[0] = 0;
		describeBelow(device, 4, now, sizeof(now), used);
		if (strcmp(now, last)) {
			strlcpy(last, now, sizeof(last));
			IOLog("oneios-hid: light sensor: OneiOSALSDevice%s\n", now[0] ? now : " (nothing attached)");
		}
		IOSleep(5000);
	}
	device->release();
}

// OSMetaClass as laid out in libkern/c++/OSMetaClass.h: the vtable pointer, reserved, superClassLink.
struct MetaClassHead { void *vtable; void *reserved; const OSMetaClass *superClassLink; };

static bool
publish(IOService *device, IOService *resource, const char *what)
{
	if (!device || !device->metaCast("IOHIDDevice") || !device->init() || !device->attach(resource)) {
		OSSafeReleaseNULL(device);
		IOLog("oneios-hid: %s: the device could not be made or attached\n", what);
		return false;
	}
	if (!device->start(resource)) {
		device->detach(resource);
		device->release();
		IOLog("oneios-hid: %s: the device did not start\n", what);
		return false;
	}
	// Kept for the life of the kernel: the registry holds the device, its timer and its sinks are live.
	IOLog("oneios-hid: %s: HID device started under %s\n", what, resource->getName());
	return true;
}

static void
oneios_hid_thread(void *, wait_result_t)
{
	// The family and the event driver's kext, with its personalities: nothing matches a HID device before.
	OSReturn status = OSKextLoadKextWithIdentifier("com.apple.iokit.IOHIDFamily");
	const OSSymbol *parentName = OSSymbol::withCString("IOHIDDevice");
	const OSMetaClass *live = parentName ? OSMetaClass::getMetaClassWithName(parentName) : nullptr;
	OSSafeReleaseNULL(parentName);
	if (status != kOSReturnSuccess || !live) {
		oneios_hid_stage = 0x40;
		IOLog("oneios-hid: no IOHIDFamily (load status 0x%x; the kernel is not in a kernel collection): no touch, no buttons\n", status);
		return;
	}
	status = OSKext::loadKextWithIdentifier("com.apple.iokit.IOHIDEventDriver", true, false, kOSKextExcludeNone,
	    kOSKextExcludeNone, nullptr);
	if (status != kOSReturnSuccess) {
		IOLog("oneios-hid: com.apple.iokit.IOHIDEventDriver did not load (0x%x): devices will have no event service\n", status);
	}
	oneios_hid_stage = 1;
	const OSMetaClass *bound = reinterpret_cast<const OSMetaClass *>(oneios_hid_parent_meta_link + vm_kernel_slide);
	if (live != bound || live->getClassSize() != oneios_hid_parent_size) {
		oneios_hid_stage = 0x81;
		IOLog("oneios-hid: the live IOHIDDevice (%p, %u bytes) is not the bound one (%p, %u bytes): the bindings are for "
		    "another kernel collection; nothing started\n", live, live->getClassSize(), bound, oneios_hid_parent_size);
		return;
	}
	oneios_hid_stage = 2;

	void *module = OSMetaClass::preModLoad(g_kernel_kmod_info.name);
	if (!module) {
		oneios_hid_stage = 0x82;
		return;
	}
	oneios_hid_static_init();
	oneios_als_static_init();
	// The superclass each static initializer passed is the absolute symbol's value: the link address.
	bool linked = true;
	char *const metas[] = {oneios_hid_native_meta, oneios_hid_als_meta};
	for (char *meta : metas) {
		MetaClassHead *head = reinterpret_cast<MetaClassHead *>(meta);
		if (reinterpret_cast<uint64_t>(head->superClassLink) == oneios_hid_parent_meta_link) {
			head->superClassLink = bound;
		}
		linked = linked && head->superClassLink == bound;
	}
	if (!linked || OSMetaClass::postModLoad(module)) {
		oneios_hid_stage = 0x82;
		IOLog("oneios-hid: the classes did not register below IOHIDDevice (%p)\n", bound);
		return;
	}
	oneios_hid_stage = 3;

	OSDictionary *match = IOService::serviceMatching("IOHIDResource");
	IOService *resource = nullptr;
	// IOHIDResource matches on IOResources, whose matching ends when the root volume is found: minutes on a first start.
	for (unsigned waited = 0; match && !resource && waited < 1800; waited += 10) {
		resource = IOService::waitForMatchingService(match, 10ULL * NSEC_PER_SEC);
	}
	OSSafeReleaseNULL(match);
	if (!resource) {
		oneios_hid_stage = 0x83;
		IOLog("oneios-hid: no IOHIDResource service after 30 minutes: no touch, no buttons\n");
		return;
	}
	oneios_hid_stage = 4;
	// The transport first: it finds xnuhost's input devices before it returns, and the devices install their sinks
	// when they start.
	droidvm_input_start();
	IOLog("oneios-hid: virtio-input: %u device(s) in the device tree, %u started\n", droidvm_input_found, droidvm_input_started);
	const bool buttons = publish(native_hid_input_allocate(), resource, "buttons");
	const bool touch = publish(native_hid_touch_allocate(), resource, "touch screen");
	resource->release();
	oneios_als_device_start();
	oneios_hid_stage = buttons && touch ? 5 : 0x84;
	watchLightSensor();
}

void
oneios_hid_input_start(void)
{
	uint32_t enabled = 1;
	PE_parse_boot_argn("oneios-hid", &enabled, sizeof(enabled));
	PE_parse_boot_argn("oneios-hid-log", &oneios_hid_log, sizeof(oneios_hid_log));
	if (!enabled || oneios_hid_stage) {
		return;
	}
	// Its own thread: the wait for IOHIDResource is long, and the board's late start is bsd_init's.
	thread_t thread;
	if (kernel_thread_start(oneios_hid_thread, nullptr, &thread) == KERN_SUCCESS) {
		thread_deallocate(thread);
	}
}
