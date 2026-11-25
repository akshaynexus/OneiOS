// SPDX-License-Identifier: BSD-2-Clause-Patent
// OneiOS, hosted kernel (sensors-als, 2026-11-25): the light sensor's events as an iPhone's SPU ambient light sensor sends them.
//
// With oneios-als-spu=1 the light sensor's HID device (drivers/brightness/OneiOSALSDevice.cpp) is an Apple vendor {0xff00, 4}
// device on the SPU transport, and the collection's IOHIDEventDriver takes it (a personality of OneiOSHidInput.cpp). That driver
// makes no ambient light event of any report (IOHIDFamily-2238.120.5's IOHIDEventDriver.cpp has no such parser), so the device
// carries the lux in the three fields of an accelerometer (X the lux's low 15 bits, Y the rest, Z the millilux below one lux;
// drivers/brightness/oneios_als_hid.h), which the driver turns into an accelerometer event, and this file's wrapper of that one
// driver's dispatchEvent turns each such event into an ambient light event before it goes on: IOHIDEvent::setType(12) gives the
// event the ambient light sensor's data (initWithType frees the old data and allocates the new type's, IOHIDEvent.cpp), then the
// level (0xC0000), raw channel 0 (0xC0001) and the illuminance (0xC000B, the field Jesvi-iOS's inject_sensors.c gives
// CoreBrightness; ios-redfin's 0971 sets it with setIntegerValue in its kernel) are the lux. Every other event of the driver goes on
// unchanged.
//
// The wrapper is NativeHidInput's edge wrapper (UniXNU ShengHidInput 24f1f3f, NativeHidInput.cpp namespace edge, BSD-2-Clause-Patent)
// for a second driver: a copy of the driver's vtable with slot 198 (dispatchEvent) replaced, each slot checked against the table of
// IOHIDEventDriver's 220 discriminators that NativeHidInput carries (exported by staged/hid_motion.rs: oneios_hid_driver_slots)
// before the copy is used, and only for IOHIDEventDriver itself. IOHIDEvent's virtuals at the pin: getType 18, setType 19,
// setIntegerValue 24, getFixedValue 25 (NativeHidInput's numbering; setType follows getType in IOHIDEvent.h). Their discriminators
// are clang's of each slot's mangled name; the formula gives NativeHidInput's proven 0x8758, 0x02c5, 0x130e and 0xf637 for the
// others, and 0x856b for setType.
//
// Unknown until a phone boot [U]: whether CoreBrightness takes this service (its usage pair, Built-In, VendorID 0x5ac and
// ALSCurveInfo are what Jesvi-iOS found it checks on iOS 16.5) and reads the level or the illuminance on 24A446.
#include <IOKit/IOLib.h>
#include <IOKit/IOService.h>
#include <string.h>
#include <libkern/c++/OSArray.h>
#include <libkern/c++/OSMetaClass.h>
#if __has_feature(ptrauth_calls)
#include <ptrauth.h>
#endif

extern "C" {
// [0] 1 another class, 2 a slot that did not check out, 3 wrapped, 4 no slot table; [1] slots copied; [2] the first slot that did not
// check out. Events: [0] accelerometer events made ambient light events, [1] other events passed on; the last lux.
volatile uint32_t oneios_als_spu_install[3];
volatile uint64_t oneios_als_spu_events[2];
volatile uint32_t oneios_als_spu_last_lux;
const uint16_t *oneios_hid_driver_slots(unsigned *count) __attribute__((weak));
int oneios_als_spu_wrap(IORegistryEntry *driver);
}

namespace {
enum { kDispatch = 198, kGetType = 18, kSetType = 19, kSetInteger = 24, kGetFixed = 25 };
enum : uint32_t { kAccelerometer = 13, kAmbientLight = 12, kAccelX = 0xD0000, kAccelY = 0xD0001,
	              kLevel = 0xC0000, kChannel0 = 0xC0001, kIlluminance = 0xC000B };

#if __has_feature(ptrauth_calls)
enum : uint16_t { kDivDispatch = 0x1134, kDivGetType = 0x8758, kDivSetType = 0x856b, kDivSetInteger = 0x130e, kDivGetFixed = 0xf637,
	              kVptrDiscriminator = 0xcda1 };
static_assert(__builtin_ptrauth_string_discriminator("_ZN10IOHIDEvent7setTypeEj") == kDivSetType, "setType's discriminator");
static_assert(__builtin_ptrauth_string_discriminator("_ZN10IOHIDEvent7getTypeEv") == kDivGetType, "getType's discriminator");
static void (*originalDispatch)(IOService *, OSObject *, IOOptionBits);
static void **
vtableOf(void *object)
{
	return reinterpret_cast<void **>(ptrauth_auth_data(*reinterpret_cast<void **>(object), ptrauth_key_process_independent_data,
	    ptrauth_blend_discriminator(object, kVptrDiscriminator)));
}
template <typename R, typename... A>
static R
call(void *object, unsigned slot, uint16_t diversity, A... args)
{
	void **entry = vtableOf(object) + slot;
	void *function = ptrauth_auth_and_resign(*entry, ptrauth_key_function_pointer, ptrauth_blend_discriminator(entry, diversity),
	    ptrauth_key_function_pointer, 0);
	return reinterpret_cast<R (*)(void *, A...)>(function)(object, args...);
}
#else
enum : uint16_t { kDivDispatch = 0, kDivGetType = 0, kDivSetType = 0, kDivSetInteger = 0, kDivGetFixed = 0 };
static uintptr_t originalDispatch;
template <typename R, typename... A>
static R
call(void *object, unsigned slot, uint16_t, A... args)
{
	return reinterpret_cast<R (*)(void *, A...)>((*reinterpret_cast<uintptr_t **>(object))[slot])(object, args...);
}
#endif

// A field of the accelerometer event, as the device's whole number (unit exponent 0: the IOFixed is value << 16).
static uint32_t
whole(void *event, uint32_t field)
{
	int32_t fixed = call<int32_t, uint32_t, IOOptionBits>(event, kGetFixed, kDivGetFixed, field, 0);
	return fixed <= 0 ? 0 : (uint32_t)((fixed + 0x8000) >> 16);
}

static void
dispatch(IOService *self, OSObject *event, IOOptionBits options)
{
	if (event && call<uint32_t>(event, kGetType, kDivGetType) == kAccelerometer) {
		// The accelerometer event has no children (handleAccelReport makes it alone), so setType loses none.
		const uint32_t lux = (whole(event, kAccelY) << 15) | (whole(event, kAccelX) & 0x7fff);
		call<void, uint32_t>(event, kSetType, kDivSetType, kAmbientLight);
		call<void, uint32_t, int32_t, IOOptionBits>(event, kSetInteger, kDivSetInteger, kLevel, (int32_t)lux, 0);
		call<void, uint32_t, int32_t, IOOptionBits>(event, kSetInteger, kDivSetInteger, kChannel0, (int32_t)lux, 0);
		call<void, uint32_t, int32_t, IOOptionBits>(event, kSetInteger, kDivSetInteger, kIlluminance, (int32_t)lux, 0);
		oneios_als_spu_last_lux = lux;
		oneios_als_spu_events[0] = oneios_als_spu_events[0] + 1;
	} else {
		oneios_als_spu_events[1] = oneios_als_spu_events[1] + 1;
	}
#if __has_feature(ptrauth_calls)
	originalDispatch(self, event, options);
#else
	reinterpret_cast<void (*)(IOService *, OSObject *, IOOptionBits)>(originalDispatch)(self, event, options);
#endif
}
} // namespace

// The driver below the light sensor's device gets the wrapped copy of its table. Returns the slots copied, or 0 (the reason in
// oneios_als_spu_install[0]). A driver that is already wrapped is left as it is (1 + its slot count).
extern "C" int
oneios_als_spu_wrap(IORegistryEntry *driver)
{
	if (!driver) {
		return 0;
	}
	const OSMetaClass *meta = driver->getMetaClass();
	const char *name = meta ? meta->getClassName() : nullptr;
	if (!name || strcmp(name, "IOHIDEventDriver")) {
		oneios_als_spu_install[0] = 1;
		return 0;
	}
#if __has_feature(ptrauth_calls)
	unsigned count = 0;
	const uint16_t *diversity = oneios_hid_driver_slots ? oneios_hid_driver_slots(&count) : nullptr;
	if (!diversity || count <= kDispatch || diversity[kDispatch] != kDivDispatch) {
		oneios_als_spu_install[0] = 4;
		return 0;
	}
	void **table = vtableOf(driver);
	void *wrapped = ptrauth_strip(reinterpret_cast<void *>(&dispatch), ptrauth_key_function_pointer);
	if (ptrauth_strip(table[kDispatch], ptrauth_key_function_pointer) == wrapped) {
		return (int)count; // this driver's table is the copy already
	}
	void **copy = static_cast<void **>(IOMalloc(count * sizeof(void *)));
	if (!copy) {
		return 0;
	}
	void *original = nullptr;
	oneios_als_spu_install[2] = count;
	for (unsigned i = 0; i < count; ++i) {
		void *entry = table[i];
		void *function = entry ? ptrauth_strip(entry, ptrauth_key_function_pointer) : nullptr;
		if (!entry || ptrauth_sign_unauthenticated(function, ptrauth_key_function_pointer,
		    ptrauth_blend_discriminator(&table[i], diversity[i])) != entry) {
			oneios_als_spu_install[0] = 2;
			oneios_als_spu_install[2] = i;
			IOFree(copy, count * sizeof(void *));
			return 0;
		}
		if (i == kDispatch) {
			original = function;
			function = wrapped;
		}
		copy[i] = ptrauth_sign_unauthenticated(function, ptrauth_key_function_pointer, ptrauth_blend_discriminator(&copy[i], diversity[i]));
	}
	originalDispatch = reinterpret_cast<void (*)(IOService *, OSObject *, IOOptionBits)>(
		ptrauth_sign_unauthenticated(original, ptrauth_key_function_pointer, 0));
	__asm__ volatile ("dmb ish" ::: "memory");
	*reinterpret_cast<void **>(driver) = ptrauth_sign_unauthenticated(copy, ptrauth_key_process_independent_data,
	    ptrauth_blend_discriminator(driver, kVptrDiscriminator));
	oneios_als_spu_install[0] = 3;
	oneios_als_spu_install[1] = count;
	return (int)count;
#else
	enum { kSlots = 256 };
	uintptr_t *table = *reinterpret_cast<uintptr_t **>(driver);
	if (table[kDispatch] == (uintptr_t)&dispatch) {
		return kSlots;
	}
	uintptr_t *copy = static_cast<uintptr_t *>(IOMalloc(kSlots * sizeof(uintptr_t)));
	if (!copy) {
		return 0;
	}
	memcpy(copy, table, kSlots * sizeof(uintptr_t));
	originalDispatch = copy[kDispatch];
	copy[kDispatch] = (uintptr_t)&dispatch;
	__asm__ volatile ("dmb ish" ::: "memory");
	*reinterpret_cast<uintptr_t **>(driver) = copy;
	oneios_als_spu_install[0] = 3;
	oneios_als_spu_install[1] = kSlots;
	return kSlots;
#endif
}
