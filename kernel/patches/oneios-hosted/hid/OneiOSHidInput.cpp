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
//   NativeHidInput "OneiOS pen"           (oneios-pen=1 and a pen device from xnuhost only) Digitizer / Stylus, report 4:
//                                         backboardd's pencil digitizer, the S Pen as an Apple Pencil (staged/hid_pen.rs)
//   OneiOSALSDevice                       drivers/brightness: Sensor / Ambient Light, under IOResources; with oneios-als-spu=1
//                                         an iPhone's SPU light sensor {0xff00, 4} (OneiOSALSEvents.cpp wraps its event driver)
//   AppleSPUHIDDevice x 1..4              (oneios-motion=MASK and a motion device from xnuhost only) the accelerometer {0xff00, 3},
//                                         gyroscope {0xff00, 9}, device orientation {0x20, 0x8a} and compass {0xff00, 0x0a}:
//                                         UniXNU-droidvm f9121cd's DroidVMMotionHid.inc (BSD-2-Clause-Patent) as staged by
//                                         staged/hid_motion.rs, with its SPU command channel (DroidVMSensorProcessor.cpp) and
//                                         kern.droidvm_motion (DroidVMMotion.c), as HidInputProbe.cpp publishes them on the iPad
// The first three are fed by the staged DroidVMVirtioInput.c from xnuhost's virtio-input devices (/arm-io/input-N: the
// touch screen and side keys on one, the pen on its own).
//
// NativeHidInput and OneiOSALSDevice subclass the original IOHIDDevice of com.apple.iokit.IOHIDFamily
// (OriginalHidDeviceABI.h). Its functions are reached through the generated stubs (build-oneios-hid.py), its
// metaclass is an absolute symbol whose value is the link address; as OneiOSSoftwareAES.cpp does for
// IOAESAccelerator, the bound metaclass is compared with the live one after adding the slide, and each owned
// metaclass gets the live one as its superclass before the module load is finished.
//
// Boot arguments: oneios-hid-driverkit=1 adds no personalities (oneios_hid_personalities); oneios-hid=0 starts nothing; oneios-hid-log=0 keeps the once-a-second state line quiet
// (default 1: it is how a boot shows that backboardd opened the digitizer and that events arrive);
// oneios-hid-queue-kib=N the event queue of the services' user clients (default 0: off, Apple's 16 KiB; 256 to try; setQueueSize);
// oneios-pen=1 publishes the pen digitizer when xnuhost has a pen device (default 0: off until proven on the phone; xnuhost's
// pen=auto adds oneios-pen=1 when it gives the guest one);
// oneios-motion=MASK (sensors-als, 2026-11-25; default 0: nothing) the motion devices when xnuhost gives a motion device: bit 0
// the accelerometer, 1 the gyroscope, 2 the device orientation, 3 the compass (xnuhost's sensors= adds 7, or 15 with a magnetometer);
// oneios-als-spu=1 (default 0) the light sensor as an iPhone's SPU light sensor (OneiOSALSDevice.cpp; xnuhost adds it when the
// light comes from the sensor hub)
#include <IOKit/IOCatalogue.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <kern/clock.h>
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
IOService *native_hid_pen_allocate(void);
bool droidvm_pen_present(void);
// The pen (staged/hid_pen.rs): the transport's pen frames; [0] HID reports sent, [1] stylus events completed as an Apple
// Pencil's, [2] stylus events of a layout the wrapper does not know; the last report's flags (1 in range, 2 tip, 4 barrel,
// 8 eraser), x, y (0..32767), pressure (0..4095), distance (0..255), tilt x and y (degrees).
extern volatile uint64_t droidvm_input_pen_reports;
extern volatile uint32_t native_pen_hid[3];
extern volatile int32_t oneios_pen_last[7];
void droidvm_input_start(void);
void oneios_als_device_start(void);
void oneios_hid_input_start(void);
// The motion sensors (staged DroidVMMotionHid.inc, DroidVMSensorProcessor.cpp, the transport): the class's late initializer and
// metaclass, the devices, the SPU channel's registration and counts, the transport's frames and the reports sent per kind.
void oneios_motion_static_init(void) __asm__("__GLOBAL__sub_I_DroidVMMotionHid.cpp");
extern char oneios_motion_meta[] __asm__("__ZN17AppleSPUHIDDevice10gMetaClassE");
IOService *droidvm_motion_hid_allocate(unsigned kind);
bool droidvm_motion_present(void);
int droidvm_sensor_processor_register(void);
extern volatile uint32_t droidvm_spu_opens, droidvm_spu_commands;
extern volatile uint64_t oneios_motion_frames, oneios_motion_reports[4];
// The light sensor's dispatch wrapper (OneiOSALSEvents.cpp) and the device's part after it (OneiOSALSDevice.cpp).
int oneios_als_spu_wrap(IORegistryEntry *driver);
void oneios_als_spu_wrapped(IORegistryEntry *driver);
extern volatile uint32_t oneios_als_spu_install[3];

// DroidVMVirtioInput.c's counters: devices found and started, events, frames, finger sets published.
extern volatile uint32_t droidvm_input_found, droidvm_input_started;
extern volatile uint64_t droidvm_input_events, droidvm_input_frames, droidvm_input_published;
// Finger sets the transport sent again for a finger held still (its touch_still: every 8 ms without a frame).
extern volatile uint64_t droidvm_input_still;
// The touch frames' latency (OneiOSTouchTime.inc): [0] frames with a scan time, [1] without; microseconds after the
// scan: [2] [3] [4] sum, least, most when the worker had the frame, [5] [6] [7] when its HID report was handed over.
extern volatile uint64_t oneios_touch_latency[8];
// The transport's interrupt counts: [0] interrupts taken, [1] the worker's sleeps, [2] of those ended by the timer (its
// 250 ms guard, or the 4 ms look while fingers are down), [3] guard ends (no finger down) whose look found a frame: an
// interrupt that never came, [4] devices attached.
extern volatile uint64_t droidvm_input_irq_counts[5];
extern volatile uint64_t oneios_hid_key_events, oneios_hid_button_reports;
// NativeHidInput's: the wrapper of the event driver's dispatch ([0] 3 = installed), contacts begun and at an edge.
extern volatile uint32_t native_touch_edge_install[3], native_touch_edge_stats[4];
// Where the last contact began ([0] x, [1] y) and where the last one lifted ([2], [3]), as the event driver's
// events carry them: 16.16 shares of the screen, 0 at the left and at the top. Written by the staged edge wrapper.
volatile int32_t oneios_touch_last[4];

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
}

static uint32_t oneios_hid_log = 1;
// oneios-hid-buttons-builtin=1 (fix-power-key, 2026-11-14; off by default, not proven): the buttons' event service as an
// iPhone's built-in buttons (native_hid_live_checkpoint).
static uint32_t oneios_hid_buttons_builtin = 0;
// oneios-pen=1 (spen, 2026-11-15; off by default until a phone session shows it): the pen digitizer is published when the
// transport found xnuhost's pen device.
static uint32_t oneios_pen = 0;
// oneios-motion=MASK (sensors-als, 2026-11-25; default 0): the kinds of motion device published when xnuhost gives a motion device.
static uint32_t oneios_motion = 0;
// oneios-als-spu=1 (default 0): the light sensor's event driver is wrapped (OneiOSALSDevice.cpp reads the same switch).
static uint32_t oneios_als_spu = 0;
// The motion class registered (1), or why not: 2 the class name is taken, 3 the registration failed, 4 com.apple.driver.AppleSPU
// is in the collection (its load would register a second AppleSPUHIDDevice); 0 not asked for.
static uint32_t oneios_motion_registered = 0;

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

// The event queue of each user client of the service (backboardd's), in KiB (hang/keyboard, 2026-11-13). Apple's
// IOHIDEventServiceUserClient::start sizes its queue from the service's "QueueSize" property (kIOHIDEventServiceQueueSize,
// bytes), else 16 KiB, and drops an event that does not fit (DebugState's dropped count, our "N dropped"). 16 KiB holds a
// few dozen digitizer events: a backboardd that stalls for half a second while fingers move at 120 frames a second
// overflows it, and a dropped event can be a finger's lift, after which backboardd keeps that touch down for good (the
// phone, 2026-11-12 23:15: "5105 HID events queued ... 6 dropped"; in the hangs backboardd stalled for seconds). The
// iPhone's own touch path sets the property too (AppleSPUHIDDriver: "set %s kIOHIDEventServiceQueueSize to %d KB" in the
// kernelcache). Set on the event driver as soon as it attaches, before backboardd opens it (a client already open keeps its
// queue until it opens again). oneios-hid-queue-kib=N (default 0: Apple's 16 KiB, off until booted; 256 to try); hidq_size
// still wins over the property.
static uint32_t oneios_hid_queue_kib = 0;   // off until a boot has run it (hand-off, 2026-11-13)

static void
setQueueSize(IORegistryEntry *driver, const char *what)
{
	if (!oneios_hid_queue_kib) {
		return;
	}
	const uint32_t bytes = (oneios_hid_queue_kib > 4096 ? 4096 : oneios_hid_queue_kib) << 10;
	driver->setProperty("QueueSize", (unsigned long long)bytes, 32);
	char clients[256];
	const unsigned open = describeClients(driver, clients, sizeof(clients));
	IOLog("oneios-hid: %s: event queue %u KiB for its user clients (QueueSize)%s\n", what, bytes >> 10,
	    open ? "; already open: theirs stays 16 KiB until they open again" : "");
}

// The motion devices' line, every 60 s while the host sends frames: frames and reports since the last line, the SPU channel's
// opens and commands since the boot. From the touch screen's 1 s tick (no timer of its own).
static void
motionMinute(void)
{
	static uint64_t lastAt, lastFrames, lastReports[4];
	if (oneios_motion_registered != 1) {
		return;
	}
	uint64_t now = mach_absolute_time(), minute = 0;
	nanoseconds_to_absolutetime(60ULL * NSEC_PER_SEC, &minute);
	if (lastAt && now - lastAt < minute) {
		return;
	}
	const bool first = lastAt == 0;
	lastAt = now;
	const uint64_t frames = oneios_motion_frames;
	uint64_t reports[4];
	for (unsigned i = 0; i < 4; i++) {
		reports[i] = oneios_motion_reports[i];
	}
	if (!first && frames != lastFrames) {
		IOLog("oneios-hid: motion: 60 s: %llu frames from the host, reports: accelerometer %llu, gyroscope %llu, orientation %llu, "
		    "compass %llu; SPU channel opens %u, commands %u (since the boot)\n", frames - lastFrames, reports[0] - lastReports[0],
		    reports[1] - lastReports[1], reports[2] - lastReports[2], reports[3] - lastReports[3], droidvm_spu_opens, droidvm_spu_commands);
	}
	lastFrames = frames;
	for (unsigned i = 0; i < 4; i++) {
		lastReports[i] = reports[i];
	}
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
	// The pen device is Built-In too; only it has a hover range (staged/hid_pen.rs).
	const bool pen = device->getProperty("MaxHoverHeight") != nullptr;
	const bool touch = !pen && device->getProperty("Built-In") != nullptr;
	const char *what = pen ? "pen" : touch ? "touch screen" : "buttons";
	if (touch) {
		motionMinute();
	}
	IORegistryEntry *driver = eventDriver(device, 4);
	if (driver && driver != state->driver) {
		state->driver = driver;
		state->published = false;
		state->clients = 0;
		IOLog("oneios-hid: %s: event driver %s attached (registry id 0x%llx)\n", what, driver->getMetaClass()->getClassName(),
		    driver->getRegistryEntryID());
		setQueueSize(driver, what);
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
	// The pen (spen, 2026-11-15). 24A446's backboardd (BackBoardHIDTouchEventProcessor, -[BKHIDDirectTouchEventProcessor
	// matcher:servicesDidMatch:]) takes the service it matched as its "pencil digitizer" when the service isBuiltIn and its
	// primaryUsagePage / primaryUsage are 0x0d / 0x20 (it logs "*** acquired pencil digitizer"); the hover range and the
	// maximum pressure it hands UIKit come from the service's MaxHoverHeight and AccurateMaxDigitizerPressureValue
	// (droidvm's reading of iPadOS 27's backboardd; both strings are in 24A446's). Once, as for the touch screen.
	if (pen && !state->published) {
		state->published = true;
		OSObject *hover = device->getProperty("MaxHoverHeight"), *pressure = device->getProperty("AccurateMaxDigitizerPressureValue");
		IORegistryEntry *interface = driver->getParentEntry(gIOServicePlane);
		IORegistryEntry *const services[] = {driver, interface};
		for (IORegistryEntry *service : services) {
			if (!service) {
				continue;
			}
			service->setProperty("Built-In", kOSBooleanTrue);
			service->setProperty("DisplayIntegrated", kOSBooleanTrue);
			service->setProperty("PrimaryUsagePage", (unsigned long long)0x0d, 32);
			service->setProperty("PrimaryUsage", (unsigned long long)0x20, 32);
			if (hover) {
				service->setProperty("MaxHoverHeight", hover);
			}
			if (pressure) {
				service->setProperty("AccurateMaxDigitizerPressureValue", pressure);
			}
		}
		IOLog("oneios-hid: pen: Digitizer/Stylus, Built-In, DisplayIntegrated, MaxHoverHeight, AccurateMaxDigitizerPressureValue "
		    "published on the event service; edge wrapper %u\n", native_touch_edge_install[0]);
	}
	// The buttons (oneios-hid-buttons-builtin=1; fix-power-key, docs/hardware.md 5). backboardd lists them as
	// "eventSource: externalKeyboard; primaryUsagePage: 0x1; primaryUsage: 0x6; transport: Virtual" (the PC boot of
	// 2026-11-14) and logs each volume press with "source:externalKeyboard ... destinations:<none>": the boot keyboard
	// collection that holds report 2 makes the service a keyboard that is not built in. An iPhone's buttons are a
	// built-in Consumer Control service. Built-In and the Consumer Control pair on the event service and its interface,
	// as for the touch screen; the device's usage pair (1, 6), which the personality matches, stays.
	if (!touch && !pen && oneios_hid_buttons_builtin && !state->published) {
		state->published = true;
		driver->setProperty("Built-In", kOSBooleanTrue);
		driver->setProperty("PrimaryUsagePage", (unsigned long long)0x0c, 32);
		driver->setProperty("PrimaryUsage", (unsigned long long)1, 32);
		if (IORegistryEntry *interface = driver->getParentEntry(gIOServicePlane)) {
			interface->setProperty("Built-In", kOSBooleanTrue);
			interface->setProperty("PrimaryUsagePage", (unsigned long long)0x0c, 32);
			interface->setProperty("PrimaryUsage", (unsigned long long)1, 32);
		}
		IOLog("oneios-hid: buttons: Consumer Control, Built-In published on the event service (oneios-hid-buttons-builtin=1)\n");
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
	const uint64_t reports = pen ? native_hid_input_events[7] : touch ? native_hid_input_events[6] : native_hid_input_events[4];
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
		if (pen) {
			// What the pen sent last: where (tenths of a percent of the screen, from the left and from the top), how hard,
			// how far above the panel and how tilted; and how many of its stylus events became an Apple Pencil's.
			const int32_t flags = oneios_pen_last[0];
			const int x = (int)(((int64_t)oneios_pen_last[1] * 1000) >> 15), y = (int)(((int64_t)oneios_pen_last[2] * 1000) >> 15);
			IOLog("oneios-hid: pen: %llu pen frames, %llu HID reports (last result 0x%x), %u sent (%u stylus events given the "
			    "pencil's angles, pressure and height, %u of a layout not known); the last: %s%s%s%sx %d.%d%% y %d.%d%%, pressure "
			    "%d/4095, height %d/255, tilt x %d y %d degrees\n", droidvm_input_pen_reports, reports, native_hid_input_results[7],
			    native_pen_hid[0], native_pen_hid[1], native_pen_hid[2], flags & 1 ? "in range, " : "out of range, ",
			    flags & 2 ? "tip down, " : "", flags & 4 ? "barrel button, " : "", flags & 8 ? "eraser, " : "", x / 10, x % 10, y / 10,
			    y % 10, oneios_pen_last[3], oneios_pen_last[4], oneios_pen_last[5], oneios_pen_last[6]);
		} else if (touch) {
			// Tenths of a percent of the screen, from the left and from the top.
			int at[4];
			for (unsigned i = 0; i < 4; i++) {
				at[i] = (int)(((int64_t)oneios_touch_last[i] * 1000) >> 16);
			}
			IOLog("oneios-hid: touch screen: %llu virtio-input events, %llu frames, %llu finger sets, %llu HID reports "
			    "(last result 0x%x), %u contacts begun (%u at an edge); the last began at x %d.%d%% y %d.%d%%, "
			    "the last lift was at x %d.%d%% y %d.%d%%; %llu sets resent for a held finger\n", droidvm_input_events, droidvm_input_frames,
			    droidvm_input_published, reports, native_hid_input_results[6], native_touch_edge_stats[1],
			    native_touch_edge_stats[2], at[0] / 10, at[0] % 10, at[1] / 10, at[1] % 10, at[2] / 10, at[2] % 10,
			    at[3] / 10, at[3] % 10, droidvm_input_still);
			uint64_t l[8];
			for (unsigned i = 0; i < 8; i++) {
				l[i] = oneios_touch_latency[i];
				oneios_touch_latency[i] = 0;
			}
			if (l[0]) {
				// The interrupt counts since the last line. A worker that the interrupt does not wake sees each frame at its
				// next timed look (4 ms while fingers are down): interrupts then stay far below the frames, timer ends near
				// 250 a second, and a touch-down waits for the 250 ms guard (counted in "found a frame").
				static uint64_t irq[4];
				uint64_t now[4];
				for (unsigned i = 0; i < 4; i++) {
					now[i] = droidvm_input_irq_counts[i];
				}
				IOLog("oneios-hid: touch latency: %llu frames; the transport had each %llu/%llu/%llu us after its scan, its HID "
				    "report was with the event driver %llu/%llu/%llu us after it (min/avg/max); interrupts +%llu, sleeps +%llu, "
				    "timer ends +%llu (+%llu guard ends found a frame), %llu device(s) on the interrupt\n", l[0], l[3], l[2] / l[0], l[4], l[6],
				    l[5] / l[0], l[7], now[0] - irq[0], now[1] - irq[1], now[2] - irq[2], now[3] - irq[3], droidvm_input_irq_counts[4]);
				for (unsigned i = 0; i < 4; i++) {
					irq[i] = now[i];
				}
			}
			if (l[1]) {
				IOLog("oneios-hid: touch latency: %llu frames without a usable scan time (an xnuhost that sends none, or a guest "
				    "clock unlike the host's): their reports carry the time they were made\n", l[1]);
			}
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
	// oneios-als-spu=1: the event driver below the device gets the dispatch wrapper (OneiOSALSEvents.cpp) once it is there,
	// whatever oneios-hid-log says; a driver that cannot be wrapped is said once, and a later one is tried again.
	IORegistryEntry *tried = nullptr;
	bool wrapped = false;
	for (unsigned waited = 0; waited < 1800 && (oneios_hid_log || (oneios_als_spu && !wrapped)); waited += 5) {
		if (oneios_hid_log) {
			size_t used = 0;
			now[0] = 0;
			describeBelow(device, 4, now, sizeof(now), used);
			if (strcmp(now, last)) {
				strlcpy(last, now, sizeof(last));
				IOLog("oneios-hid: light sensor: OneiOSALSDevice%s\n", now[0] ? now : " (nothing attached)");
			}
		}
		IORegistryEntry *driver = oneios_als_spu && !wrapped ? eventDriver(device, 3) : nullptr;
		if (driver && driver != tried) {
			tried = driver;
			const int slots = oneios_als_spu_wrap(driver);
			if (slots > 0) {
				wrapped = true;
				oneios_als_spu_wrapped(driver);
				IOLog("ONEIOS ALS: IOHIDEventDriver's dispatch wrapped (%d slots): its lux events go on as ambient light events\n", slots);
			} else {
				const uint32_t why = oneios_als_spu_install[0];
				IOLog("ONEIOS ALS: the event driver's dispatch is not wrapped (%s%s): no ambient light events\n",
				    why == 1 ? "not IOHIDEventDriver: " : why == 2 ? "a slot of its table did not check out" : why == 4 ?
				    "NativeHidInput has no slot table" : "no memory", why == 1 ? driver->getMetaClass()->getClassName() : "");
			}
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

// Apple's kernel event driver for the two devices, at once. The collection's own personalities for an
// IOHIDInterface are deferred (IOMatchDefer) until the IOKit daemon has sent every plist, and the DriverKit one
// (AppleUserHIDEventDriver of com.apple.AppleUserHIDDrivers, any Consumer or Digitizer device) is then tried
// first: the kernel waits 120 s for that driver's process to check in, and on the S24 Ultra the process came up
// 12 s after that wait ended. The kernel's IOHIDEventDriver took the devices 233 s after they started; until then
// every report was accepted and went nowhere (the phone's session of 2026-11-11: the gestures of that time gave
// "0 contacts begun"). An iPhone's own touch screen and buttons are not DriverKit devices either: their event
// services are kernel drivers of the multitouch and button kexts.
//
// So the catalogue gets two personalities of its own, not deferred, with a probe score above Apple's (3000 at
// most): IOHIDEventDriver on the IOHIDInterface of a device with this transport and usage page. They are matched
// as the DriverKit personality is (DeviceUsagePairs, and Transport, by IOHIDDevice::matchPropertyTable); the light
// sensor (Virtual, Sensor page) is not among them and keeps AppleUSBALSService. The first driver that starts
// takes the match category: the DriverKit personality and Apple's deferred ones are not started for these two.
// oneios-hid-driverkit=1 leaves the catalogue alone (the old order).
static void
oneios_hid_personalities(void)
{
	// The usage pairs are the devices' application collections. The buttons' report 2 (Consumer: Menu, Power, Volume
	// Increment and Decrement; Telephony: Ring Enable) lies inside NativeHidInput's boot keyboard collection, so that
	// device's one pair is Generic Desktop / Keyboard (1, 6), never page 12. A page-12 personality did not match it; the
	// candidates were Apple's deferred "HID Keyboard Driver", then the DriverKit driver, and the side keys reached
	// backboardd only after the DriverKit driver's 120 s (phone, 2026-11-12: +256 s into the boot).
	// sensors-als, 2026-11-25: the motion devices (DroidVMMotionHid.inc: every descriptor ends with an empty {0xff0c, 3}
	// application collection, the pair of IOHIDFamily's "HID Sensor" personality) and the SPU light sensor {0xff00, 4}, both on
	// the SPU transport. A higher score does not keep Apple's personalities of the same pairs from being tried: probeCandidates
	// loads the kext of every candidate that passes matchPassive (IOCatalogue::isModuleLoaded) before it starts the best one.
	// v0.1.29 (2026-11-25) panicked so: com.apple.driver.AppleSPU's AppleSPUGestureDriver ({0xff0c, 3}, Transport SPU) matched
	// the motion devices, the kext loaded, and its AppleSPUHIDDevice was a duplicate of the one registered here. AppleSPU and the
	// kexts that link it are left out of the collection since (prelink roots, FORCE_OUT), and oneios_motion_register refuses
	// a collection that still has it.
	static const struct { const char *transport; uint32_t page, usage; } devices[] = {
		{"SPI", 0x0d, 0},       // the touch screen (NativeHidInput::newTransportString): Digitizer, any usage
		{"Virtual", 0x01, 0x06}, // the buttons: Generic Desktop / Keyboard
		{"SPU", 0xff0c, 0x03},   // the motion devices
		{"SPU", 0xff00, 0x04},   // the SPU light sensor (oneios-als-spu=1)
	};
	uint32_t driverkit = 0;
	PE_parse_boot_argn("oneios-hid-driverkit", &driverkit, sizeof(driverkit));
	OSArray *drivers = driverkit ? nullptr : OSArray::withCapacity(4);
	if (!drivers) {
		IOLog("oneios-hid: no personalities added: the collection's own order (DriverKit first) decides the event driver\n");
		return;
	}
	bool made = true;
	for (const auto &device : devices) {
		OSDictionary *personality = OSDictionary::withCapacity(8), *pair = OSDictionary::withCapacity(1);
		OSArray *pairs = OSArray::withCapacity(1);
		OSString *bundle = OSString::withCString("com.apple.iokit.IOHIDEventDriver"), *driver = OSString::withCString("IOHIDEventDriver");
		OSString *provider = OSString::withCString("IOHIDInterface"), *transport = OSString::withCString(device.transport);
		OSNumber *page = OSNumber::withNumber(device.page, 32), *score = OSNumber::withNumber(100000, 32);
		OSNumber *usage = device.usage ? OSNumber::withNumber(device.usage, 32) : nullptr;
		if (personality && pair && pairs && bundle && driver && provider && transport && page && score && (usage || !device.usage)) {
			pair->setObject("DeviceUsagePage", page);
			if (usage) {
				pair->setObject("DeviceUsage", usage);
			}
			pairs->setObject(pair);
			personality->setObject("CFBundleIdentifier", bundle);
			personality->setObject("IOClass", driver);
			personality->setObject("IOProviderClass", provider);
			personality->setObject("Transport", transport);
			personality->setObject("DeviceUsagePairs", pairs);
			personality->setObject("IOProbeScore", score);
			drivers->setObject(personality);
		} else {
			made = false;
		}
		OSSafeReleaseNULL(personality);
		OSSafeReleaseNULL(pair);
		OSSafeReleaseNULL(pairs);
		OSSafeReleaseNULL(bundle);
		OSSafeReleaseNULL(driver);
		OSSafeReleaseNULL(provider);
		OSSafeReleaseNULL(transport);
		OSSafeReleaseNULL(page);
		OSSafeReleaseNULL(usage);
		OSSafeReleaseNULL(score);
	}
	// No nub matching: no HID device of ours exists yet.
	const bool added = made && gIOCatalogue && gIOCatalogue->addDrivers(drivers, false);
	drivers->release();
	IOLog(added ? "oneios-hid: IOHIDEventDriver personalities added for the touch screen, the buttons, the motion devices and the "
	    "SPU light sensor: the kernel's event driver takes them when they start, the DriverKit one is not waited for\n"
	    : "oneios-hid: the IOHIDEventDriver personalities were not added: the DriverKit event driver is tried first "
	    "(the kernel's takes over after its 120 s)\n");
}

// The motion class (sensors-als, 2026-11-25): in a module load of its own, after the touch screen's, on this same thread (see
// below), so that a clash of its name can never take NativeHidInput's classes with it. AppleSPUHIDDevice and its channel's
// AppleSPUHIDDeviceUserClient are the names of the iPhone's own classes in com.apple.driver.AppleSPU: the platform sandbox lets
// SpringBoard and backboardd open only the services and user clients it names (DroidVM's reading). Then the SPU channel's class
// (DroidVMSensorProcessor.cpp's own module load); without it the orientation device is left out, as backboardd and SpringBoard
// crashed with locationd on the iPad without the channel.
// The name check below sees only kexts already loaded. A collection that has com.apple.driver.AppleSPU at all can load it later
// (a candidate's kext is loaded whatever its score, a dependency with its user) and panics then: v0.1.29, "class
// AppleSPUHIDDevice is a duplicate". So the classes register only when the collection has no such kext (prelink roots leaves it
// out since 2026-11-25, as UniXNU-droidvm's iPad kernelcache has none); one prelinked before is refused with its own line.
static void
oneios_motion_register(const OSMetaClass *bound)
{
	if (!oneios_motion) {
		return;
	}
	OSKext *spu = OSKext::lookupKextWithIdentifier("com.apple.driver.AppleSPU");
	const bool spuInCollection = spu != nullptr;
	OSSafeReleaseNULL(spu);
	if (spuInCollection) {
		oneios_motion_registered = 4;
		IOLog("oneios-hid: motion: com.apple.driver.AppleSPU is in this kernel collection (prelinked before 2026-11-25's roots): its "
		    "load would register a second AppleSPUHIDDevice and panic; no motion devices (prelink the collection again)\n");
		return;
	}
	static const char *const names[] = {"AppleSPUHIDDevice", "AppleSPUHIDDeviceUserClient"};
	for (const char *name : names) {
		const OSSymbol *symbol = OSSymbol::withCString(name);
		const bool taken = !symbol || OSMetaClass::getMetaClassWithName(symbol) != nullptr;
		OSSafeReleaseNULL(symbol);
		if (taken) {
			oneios_motion_registered = 2;
			IOLog("oneios-hid: motion: the class name %s is taken (com.apple.driver.AppleSPU loaded?): no motion devices\n", name);
			return;
		}
	}
	void *module = OSMetaClass::preModLoad(g_kernel_kmod_info.name);
	if (!module) {
		oneios_motion_registered = 3;
		IOLog("oneios-hid: motion: the kernel's module could not be opened for the motion class: no motion devices\n");
		return;
	}
	oneios_motion_static_init();
	MetaClassHead *head = reinterpret_cast<MetaClassHead *>(oneios_motion_meta);
	if (reinterpret_cast<uint64_t>(head->superClassLink) == oneios_hid_parent_meta_link) {
		head->superClassLink = bound;
	}
	const bool linked = head->superClassLink == bound;
	const OSReturn status = OSMetaClass::postModLoad(module); // always: it ends the class-loading lock preModLoad took
	if (!linked || status != kOSReturnSuccess) {
		oneios_motion_registered = 3;
		IOLog("oneios-hid: motion: AppleSPUHIDDevice did not register below IOHIDDevice (0x%x%s): no motion devices\n", status,
		    linked ? "" : ", its superclass not bound");
		return;
	}
	oneios_motion_registered = 1;
	const int channel = droidvm_sensor_processor_register();
	if (channel != 0 && (oneios_motion & 4)) {
		oneios_motion &= ~4u;
		IOLog("oneios-hid: motion: the SPU channel's class did not register (%d): the device orientation is left out\n", channel);
	}
	IOLog("oneios-hid: motion: AppleSPUHIDDevice registered below IOHIDDevice%s (oneios-motion=0x%x); the kernel's classes: "
	    "com.apple.driver.AppleSPU is not in the collection\n",
	    channel == 0 ? ", its SPU channel AppleSPUHIDDeviceUserClient too" : "", oneios_motion);
}

// The motion devices of the mask, after the transport found xnuhost's motion device (HidInputProbe.cpp's order).
static void
oneios_motion_publish(IOService *resource)
{
	if (!oneios_motion) {
		if (droidvm_motion_present()) {
			IOLog("oneios-hid: motion: xnuhost has a motion device; nothing published (oneios-motion=0)\n");
		}
		return;
	}
	if (!droidvm_motion_present()) {
		IOLog("oneios-hid: motion: oneios-motion=0x%x, but xnuhost gave no motion device (sensors=off, or no sensor hub): nothing "
		    "published\n", oneios_motion);
		return;
	}
	if (oneios_motion_registered != 1) {
		IOLog("oneios-hid: motion: nothing published: %s\n", oneios_motion_registered == 2 ? "the class name AppleSPUHIDDevice is taken"
		    : oneios_motion_registered == 4 ? "com.apple.driver.AppleSPU is in the collection" : "the class did not register");
		return;
	}
	static const char *const names[] = {"accelerometer", "gyroscope", "orientation", "compass"};
	char text[96] = "";
	size_t used = 0;
	for (unsigned kind = 0; kind < 4; ++kind) {
		if ((oneios_motion & (1u << kind)) && publish(droidvm_motion_hid_allocate(kind), resource, names[kind]) && used < sizeof(text)) {
			used += snprintf(text + used, sizeof(text) - used, "%s%s", used ? ", " : "", names[kind]);
		}
	}
	IOLog("oneios-hid: motion: AppleSPUHIDDevice %s published (oneios-motion=0x%x)\n", used ? text : "nothing", oneios_motion);
}

// The kexts and the classes. On the caller's thread (the board's late start, bsd_init's), not on a thread of its
// own: OSMetaClass::preModLoad takes the class-loading lock and keeps it until postModLoad, which looks the kext
// up under the kext lock; a kext that another thread loads meanwhile holds the kext lock and waits for the
// class-loading lock in its own preModLoad. native-adreno met that deadlock with its own registration; this one
// was not seen to hang, but it ran on its own thread beside bsd_init's kext loads. Everything that loads kexts or
// registers classes at the late start runs on that one thread, one after the other, and says so in the log.
static bool
oneios_hid_register(void)
{
	// The family and the event driver's kext, with its personalities: nothing matches a HID device before.
	OSReturn status = OSKextLoadKextWithIdentifier("com.apple.iokit.IOHIDFamily");
	const OSSymbol *parentName = OSSymbol::withCString("IOHIDDevice");
	const OSMetaClass *live = parentName ? OSMetaClass::getMetaClassWithName(parentName) : nullptr;
	OSSafeReleaseNULL(parentName);
	if (status != kOSReturnSuccess || !live) {
		oneios_hid_stage = 0x40;
		IOLog("oneios-hid: no IOHIDFamily (load status 0x%x; the kernel is not in a kernel collection): no touch, no buttons\n", status);
		return false;
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
		return false;
	}
	oneios_hid_stage = 2;

	void *module = OSMetaClass::preModLoad(g_kernel_kmod_info.name);
	if (!module) {
		oneios_hid_stage = 0x82;
		IOLog("oneios-hid: the kernel's module could not be opened for class registration\n");
		return false;
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
		return false;
	}
	oneios_hid_stage = 3;
	oneios_hid_personalities();
	oneios_motion_register(bound);
	IOLog("oneios-hid: IOHIDFamily and IOHIDEventDriver loaded, the device classes registered below IOHIDDevice; "
	    "the devices start when IOHIDResource is there\n");
	return true;
}

// The long part, on a thread of its own: the wait for IOHIDResource, then the devices.
static void
oneios_hid_thread(void *, wait_result_t)
{
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
	IOLog("oneios-hid: virtio-input: %u device(s) in the device tree, %u started%s\n", droidvm_input_found, droidvm_input_started,
	    droidvm_motion_present() ? "; motion sensors among them" : "");
	const bool buttons = publish(native_hid_input_allocate(), resource, "buttons");
	const bool touch = publish(native_hid_touch_allocate(), resource, "touch screen");
	// The pen: its own device, after the touch screen (its event driver takes the same personality: SPI, Digitizer page).
	if (droidvm_pen_present() && oneios_pen) {
		publish(native_hid_pen_allocate(), resource, "pen");
	} else if (droidvm_pen_present()) {
		IOLog("oneios-hid: pen: xnuhost has a pen device; not published (oneios-pen=0)\n");
	} else if (oneios_pen) {
		IOLog("oneios-hid: pen: oneios-pen=1, but xnuhost gave no pen device (pen=off, or no pen found on the host)\n");
	}
	oneios_motion_publish(resource);
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
	PE_parse_boot_argn("oneios-hid-buttons-builtin", &oneios_hid_buttons_builtin, sizeof(oneios_hid_buttons_builtin));
	PE_parse_boot_argn("oneios-hid-queue-kib", &oneios_hid_queue_kib, sizeof(oneios_hid_queue_kib));
	PE_parse_boot_argn("oneios-pen", &oneios_pen, sizeof(oneios_pen));
	PE_parse_boot_argn("oneios-motion", &oneios_motion, sizeof(oneios_motion));
	oneios_motion &= 0xf;
	PE_parse_boot_argn("oneios-als-spu", &oneios_als_spu, sizeof(oneios_als_spu));
	if (!enabled || oneios_hid_stage) {
		return;
	}
	if (!oneios_hid_register()) {
		return;
	}
	// Its own thread: the wait for IOHIDResource is long, and the board's late start is bsd_init's.
	thread_t thread;
	if (kernel_thread_start(oneios_hid_thread, nullptr, &thread) == KERN_SUCCESS) {
		thread_deallocate(thread);
	}
}
