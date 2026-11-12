/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS: the backlight service CoreBrightness (in backboardd) drives, on the Galaxy S24 Ultra's panel
 * through xnuhost's brightness bridge (runtime/bridges/brightness), and the host's ambient light
 * readings for the kernel. README.md here has the whole path and what is not proven.
 *
 * Ported from iOSPlus10's Mi A3 work (patches/laurel/xnu/2201-laurel-backlight-panel-power.patch and
 * xnu-12377/post/9005-laurel-backlight-calibration.patch: AppleARMBacklight in SweetFramebuffer.cpp;
 * docs/laurel_sprout/brightness.md, ios26-backboardd.md). The iOS side is kept: the properties, the
 * requests, the calibration. The DSI panel code is gone: a request is one message to the host, which
 * owns the curve, the fade and the panel.
 *
 * What CoreBrightness reads and writes [iOSPlus10: 20H392 and 23F84 disassembly; 24A446: the same
 * strings are in its CoreBrightness, no disassembly]:
 *   - it finds the service by the property backlight-control = true and IOObjectConformsTo(service,
 *     "AppleARMBacklight");
 *   - IODisplayParameters { brightness { min, max, value } }, max-nits, min-nits, brightness-nits;
 *   - backlight-calibration-parameters and backlight-marketing-table: without the first, 23F84's
 *     _DisplayCreateWithCapabilitiesAndID reads a table through NULL and backboardd dies;
 *   - it sets { brightness = 0..max (perceived), FadePeriod = seconds 16.16 } with
 *     IORegistryEntrySetCFProperties, or brightness-nits (16.16) alone.
 * What iOS 27's CoreBrightness reads besides [UniXNU-sheng package/AppleARMBacklight.cpp and docs/porting/brightness.md,
 * iPadOS 27.0.1 24A446 on hardware; the same CoreBrightness binary family]:
 *   - IODisplayParameters.BrightnessMilliNits { min, max, value } in millinits: its nits range (without it the
 *     capabilities of the phone's session of 2026-11-12 said MaxNits = 0, MinNits = 0);
 *   - LmaxProduct as 4 bytes of 16.16 nits (zero: every slider position is the maximum), LmidProduct and
 *     LminProduct as numbers in nits;
 *   - it sends brightness-nits as signed 16.16, zero or less when the display goes dark.
 * It sends to this service only when backboardd runs BLControl. With the device tree's vmm-present (kept for AMFI
 * and the rest) MobileGestalt says IsVirtualDevice and -[BrightnessSystemInternal init] picks mode 3, VMBLControl,
 * which serves a Virtualization.framework host over remote XPC and never touches this service. BacklightMode.inc
 * (built into the Metal plug-in, which runs in backboardd) gives backboardd mode 1. README.md has the evidence.
 *
 * The class name. The original AppleARMBacklight is a class of com.apple.driver.AppleARMPlatform, and
 * that kext is in OneiOS's kernel collection. A second class of that name in the kernel would make the
 * kext's start fail (OSMetaClass: duplicate class), and it carries AppleARMFunction and the rest. So
 * this class is OneiOSBacklight, and conformance is given another way: once the original metaclass is
 * registered, this class's superclass link is moved from IOService to it (the original is itself an
 * IOService subclass, so every cast that held still holds). No instance of the original exists and
 * none of its code runs. oneios_bl_conform=0 leaves the link alone.
 *
 * Boot arguments: oneios_bl=0 (no service), oneios_bl_conform=0.
 */
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <libkern/c++/OSCollectionIterator.h>
#include <libkern/c++/OSData.h>
#include <libkern/c++/OSDictionary.h>
#include <libkern/c++/OSMetaClass.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSString.h>
#include <libkern/c++/OSSymbol.h>
#include <libkern/libkern.h>
#include <kern/thread.h>
#include <pexpert/pexpert.h>

#include "oneios_bridge.h"

extern "C" void proc_selfname(char *buf, int size);

#define OBL_VALUE_MAX        255u       /* IODisplayParameters brightness max, as on the Mi A3 */
#define OBL_FULL             65535u     /* the bridge's level scale */
#define OBL_TABLE_POINTS     17u        /* backlight-marketing-table, as on N104AP */
#define OBL_CONFORM_WAIT_S   600u       /* how long the original metaclass is waited for */

class OneiOSBacklight : public IOService {
	OSDeclareDefaultStructors(OneiOSBacklight);
public:
	bool start(IOService *provider) APPLE_KEXT_OVERRIDE;
	IOReturn setProperties(OSObject *properties) APPLE_KEXT_OVERRIDE;
	using IOService::newUserClient;
	IOReturn newUserClient(task_t owningTask, void *securityID, UInt32 type, OSDictionary *properties,
	    IOUserClient **handler) APPLE_KEXT_OVERRIDE;
	void hostAls(const struct ob_bright_als *als);
private:
	uint32_t fNits;                 /* the light at the top of the slider */
	uint32_t fMinNits16;            /* the lowest level's light, 16.16 */
	uint32_t fRequests;
	bool fHasAls;
	uint32_t nits16For(uint32_t value) const;
	void publish(uint32_t value, uint32_t nits16);
	bool calibration();
	void logRequest(OSDictionary *d, uint32_t level, uint32_t ms, bool linear);
};

OSDefineMetaClassAndStructors(OneiOSBacklight, IOService);

static OneiOSBacklight * volatile gOneiOSBacklight;
static volatile uint32_t obl_state_seen;
static struct ob_bright_state obl_state;
static volatile uint32_t obl_als_seen, obl_als_millilux;
static oneios_als_handler volatile obl_als_fn;
static void * volatile obl_als_ctx;

/* ---- from the host (the bridge's worker thread) -------------------------------------------------- */

static void
obl_from_host(void *context, uint16_t type, const uint8_t *payload, uint32_t length)
{
	(void)context;
	if (type == OB_BRIGHT_STATE && length >= sizeof(struct ob_bright_state)) {
		memcpy(&obl_state, payload, sizeof(obl_state));
		__atomic_store_n(&obl_state_seen, 1, __ATOMIC_RELEASE);
	} else if (type == OB_BRIGHT_ALS && length >= sizeof(struct ob_bright_als)) {
		struct ob_bright_als als;
		memcpy(&als, payload, sizeof(als));
		__atomic_store_n(&obl_als_millilux, als.millilux, __ATOMIC_RELAXED);
		__atomic_store_n(&obl_als_seen, 1, __ATOMIC_RELEASE);
		oneios_als_handler fn = obl_als_fn;
		if (fn != NULL) {
			fn(obl_als_ctx, als.millilux);
		}
		OneiOSBacklight *bl = gOneiOSBacklight;
		if (bl != NULL) {
			bl->hostAls(&als);
		}
	}
}

extern "C" bool
oneios_als_read(uint32_t *millilux)
{
	if (!__atomic_load_n(&obl_als_seen, __ATOMIC_ACQUIRE)) {
		return false;
	}
	*millilux = __atomic_load_n(&obl_als_millilux, __ATOMIC_RELAXED);
	return true;
}

extern "C" void
oneios_als_listen(oneios_als_handler fn, void *context)
{
	obl_als_fn = NULL;
	obl_als_ctx = context;
	obl_als_fn = fn;
}

/* ---- the service --------------------------------------------------------------------------------- */

/* The light of a perceived value: the host's curve (the slider squared), not under the lowest level. */
uint32_t
OneiOSBacklight::nits16For(uint32_t value) const
{
	uint64_t v = ((uint64_t)fNits << 16) * value * value / (OBL_VALUE_MAX * OBL_VALUE_MAX);
	return v < fMinNits16 ? fMinNits16 : (uint32_t)v;
}

void
OneiOSBacklight::publish(uint32_t value, uint32_t nits16)
{
	OSDictionary *brightness = OSDictionary::withCapacity(3);
	OSDictionary *parameters = OSDictionary::withCapacity(1);
	OSNumber *min = OSNumber::withNumber(0ULL, 32), *max = OSNumber::withNumber((uint64_t)OBL_VALUE_MAX, 32);
	OSNumber *now = OSNumber::withNumber((uint64_t)value, 32);
	OSDictionary *milli = OSDictionary::withCapacity(3);
	OSNumber *milliMin = OSNumber::withNumber(((uint64_t)fMinNits16 * 1000) >> 16, 32);
	OSNumber *milliMax = OSNumber::withNumber((uint64_t)fNits * 1000, 32);
	OSNumber *milliNow = OSNumber::withNumber(((uint64_t)nits16 * 1000) >> 16, 32);
	if (brightness != NULL && parameters != NULL && min != NULL && max != NULL && now != NULL) {
		brightness->setObject("min", min);
		brightness->setObject("max", max);
		brightness->setObject("value", now);
		parameters->setObject("brightness", brightness);
		if (milli != NULL && milliMin != NULL && milliMax != NULL && milliNow != NULL) {
			milli->setObject("min", milliMin);
			milli->setObject("max", milliMax);
			milli->setObject("value", milliNow);
			parameters->setObject("BrightnessMilliNits", milli);       /* iOS 27: the nits range */
		}
		setProperty("IODisplayParameters", parameters);
	}
	setProperty("brightness-nits", (unsigned long long)nits16, 32);
	OSSafeReleaseNULL(min);
	OSSafeReleaseNULL(max);
	OSSafeReleaseNULL(now);
	OSSafeReleaseNULL(milliMin);
	OSSafeReleaseNULL(milliMax);
	OSSafeReleaseNULL(milliNow);
	OSSafeReleaseNULL(milli);
	OSSafeReleaseNULL(brightness);
	OSSafeReleaseNULL(parameters);
}

/*
 * iOSPlus10 9005: backlight-calibration-parameters and backlight-marketing-table, all 16.16. The table
 * is the slider 0..1 to nits in 17 points on the host's curve. No current values: an OLED panel has no
 * LED current, and CoreBrightness only prints these three. No digital-dimming-min-nits: it would turn
 * digital dimming on.
 */
bool
OneiOSBacklight::calibration()
{
	static const char * const zero[] = { "current-for-max-backlight", "current-for-mid-backlight", "hardware-max-current-limit" };
	uint32_t table[OBL_TABLE_POINTS];
	bool ok = false;

	for (uint32_t i = 0; i < OBL_TABLE_POINTS; i++) {
		uint64_t v = ((uint64_t)fNits << 16) * i * i / ((OBL_TABLE_POINTS - 1) * (OBL_TABLE_POINTS - 1));
		table[i] = v < fMinNits16 ? fMinNits16 : (uint32_t)v;
	}
	const struct { const char *key; uint32_t value; } q16[] = {
		{ "lMaxProduct", table[OBL_TABLE_POINTS - 1] }, { "lMidProduct", table[(OBL_TABLE_POINTS - 1) / 2] }, { "lMinProduct", table[0] },
		{ "lMaxPanel", table[OBL_TABLE_POINTS - 1] }, { "lMidPanel", table[(OBL_TABLE_POINTS - 1) / 2] }, { "lMinPanel", table[0] },
	};
	OSDictionary *d = OSDictionary::withCapacity(10);
	OSData *t = OSData::withBytes(table, sizeof(table));
	if (d != NULL && t != NULL) {
		ok = true;
		for (size_t i = 0; i < sizeof(q16) / sizeof(q16[0]); i++) {
			OSNumber *n = OSNumber::withNumber((uint64_t)q16[i].value, 32);
			ok = n != NULL && d->setObject(q16[i].key, n) && ok;
			OSSafeReleaseNULL(n);
		}
		for (size_t i = 0; i < sizeof(zero) / sizeof(zero[0]); i++) {
			OSNumber *n = OSNumber::withNumber(0ULL, 32);
			ok = n != NULL && d->setObject(zero[i], n) && ok;
			OSSafeReleaseNULL(n);
		}
	}
	if (ok) {
		ok = setProperty("backlight-marketing-table", t) && setProperty("backlight-calibration-parameters", d);
	}
	/* iOS 27's BrightnessControl reads the product luminances from their own properties: LmaxProduct as 4 bytes of 16.16
	 * (at zero every slider position means the maximum), LmidProduct and LminProduct as numbers in nits (as data they
	 * would be read as a float) [UniXNU-sheng]. LminProduct: the lowest level rounded up to 1 nit, as min-nits. */
	OSData *lmax = OSData::withBytes(&table[OBL_TABLE_POINTS - 1], sizeof(table[0]));
	OSNumber *lmid = OSNumber::withNumber((uint64_t)(table[(OBL_TABLE_POINTS - 1) / 2] >> 16), 32);
	OSNumber *lmin = OSNumber::withNumber((uint64_t)((fMinNits16 + 0xffff) >> 16), 32);
	ok = ok && lmax != NULL && lmid != NULL && lmin != NULL && setProperty("LmaxProduct", lmax) && setProperty("LmidProduct", lmid) &&
	    setProperty("LminProduct", lmin);
	OSSafeReleaseNULL(lmax);
	OSSafeReleaseNULL(lmid);
	OSSafeReleaseNULL(lmin);
	OSSafeReleaseNULL(d);
	OSSafeReleaseNULL(t);
	return ok;
}

/* Every request, who sent it and which keys (2026-11-12: the slider moved in iOS and no request came; until it is known
 * what iOS 27 sends, each one is logged; a slider drag is some tens of lines). */
void
OneiOSBacklight::logRequest(OSDictionary *d, uint32_t level, uint32_t ms, bool linear)
{
	uint32_t n = ++fRequests;
	char name[32], keys[160];
	size_t used = 0;
	keys[0] = 0;
	proc_selfname(name, sizeof(name));
	OSCollectionIterator *it = d != NULL ? OSCollectionIterator::withCollection(d) : NULL;
	if (it != NULL) {
		OSObject *o;
		while ((o = it->getNextObject()) != NULL && used + 2 < sizeof(keys)) {
			OSSymbol *key = OSDynamicCast(OSSymbol, o);
			if (key != NULL) {
				used += (size_t)scnprintf(keys + used, sizeof(keys) - used, "%s%s", used ? ", " : "", key->getCStringNoCopy());
			}
		}
		it->release();
	}
	if (level <= OBL_FULL) {
		IOLog("ONEIOS BACKLIGHT: request %u from %s {%s} -> level %u of 65535%s, fade %u ms\n", n, name, keys, level,
		    linear ? " (of the light)" : " (of the slider)", ms);
	} else {
		IOLog("ONEIOS BACKLIGHT: request %u from %s {%s}: no brightness in it, nothing sent\n", n, name, keys);
	}
}

IOReturn
OneiOSBacklight::setProperties(OSObject *properties)
{
	OSDictionary *d = OSDynamicCast(OSDictionary, properties);
	OSNumber *b = d != NULL ? OSDynamicCast(OSNumber, d->getObject("brightness")) : NULL;
	OSNumber *nits = d != NULL ? OSDynamicCast(OSNumber, d->getObject("brightness-nits")) : NULL;
	OSNumber *f = d != NULL ? OSDynamicCast(OSNumber, d->getObject("FadePeriod")) : NULL;
	struct ob_bright_set set = { 0, 0, 0 };

	/* FadePeriod: seconds, 16.16 */
	set.fade_ms = f != NULL ? (uint32_t)(((uint64_t)f->unsigned32BitValue() * 1000) >> 16) : 0;
	if (b != NULL) {
		int32_t v = (int32_t)b->unsigned32BitValue();
		uint32_t value = v < 0 ? 0 : (uint32_t)v > OBL_VALUE_MAX ? OBL_VALUE_MAX : (uint32_t)v;
		set.level = value * OBL_FULL / OBL_VALUE_MAX;
		logRequest(d, set.level, set.fade_ms, false);
		(void)oneios_bridge_send(OB_CH_BRIGHTNESS, OB_BRIGHT_SET, &set, sizeof(set));
		publish(value, nits16For(value));
		return kIOReturnSuccess;
	}
	if (nits != NULL) {
		/* brightness-nits alone: a fraction of the light. Signed 16.16 (iOS 27): zero or less is the display going dark,
		 * level 0 (the host keeps the panel at its floor and puts the touch IC to sleep). As unsigned, a negative value
		 * was the top of the range. */
		int32_t signed16 = (int32_t)nits->unsigned32BitValue();
		uint64_t n16 = signed16 < 0 ? 0 : (uint64_t)signed16, top = (uint64_t)fNits << 16;
		if (n16 > top) {
			n16 = top;
		}
		set.level = (uint32_t)(n16 * OBL_FULL / top);
		set.flags = OB_BRIGHT_LINEAR;
		logRequest(d, set.level, set.fade_ms, true);
		(void)oneios_bridge_send(OB_CH_BRIGHTNESS, OB_BRIGHT_SET, &set, sizeof(set));
		/* IODisplayParameters follow: the slider's value is the square root of the fraction (the host's curve). */
		uint64_t square = n16 * OBL_VALUE_MAX * OBL_VALUE_MAX / top, value = 0;
		while ((value + 1) * (value + 1) <= square) {
			value++;
		}
		publish((uint32_t)value, (uint32_t)n16);
		return kIOReturnSuccess;
	}
	logRequest(d, OBL_FULL + 1, 0, false);
	return kIOReturnSuccess;
}

/* CoreBrightness sets properties; nothing of ours opens a connection. A process that tries is logged (who, which type)
 * and gets what IOService gives (unsupported), so the log shows a client that looks for another way in. */
IOReturn
OneiOSBacklight::newUserClient(task_t owningTask, void *securityID, UInt32 type, OSDictionary *properties, IOUserClient **handler)
{
	char name[32];
	proc_selfname(name, sizeof(name));
	IOReturn r = IOService::newUserClient(owningTask, securityID, type, properties, handler);
	IOLog("ONEIOS BACKLIGHT: %s opened the service (type %u): 0x%x\n", name, (unsigned)type, r);
	return r;
}

void
OneiOSBacklight::hostAls(const struct ob_bright_als *als)
{
	/* For ioreg and for a session's first look; the HID service is OneiOSALSDevice.cpp. */
	setProperty("oneios-als-millilux", (unsigned long long)als->millilux, 32);
	setProperty("oneios-als-clear", (unsigned long long)als->clear, 32);
	setProperty("oneios-als-ir", (unsigned long long)als->ir, 32);
}

bool
OneiOSBacklight::start(IOService *provider)
{
	if (!oneios_bridge_ready() || !(oneios_bridge_channels() & (1u << OB_CH_BRIGHTNESS))) {
		IOLog("ONEIOS BACKLIGHT: off: the host serves no brightness channel (no bridge, or no backlight device and no light sensor on the phone)\n");
		return false;
	}
	oneios_bridge_listen(OB_CH_BRIGHTNESS, obl_from_host, NULL);
	(void)oneios_bridge_send(OB_CH_BRIGHTNESS, OB_BRIGHT_QUERY, NULL, 0);
	for (unsigned waited = 0; waited < 1000u && !__atomic_load_n(&obl_state_seen, __ATOMIC_ACQUIRE); waited += 10u) {
		IOSleep(10);
	}
	if (!__atomic_load_n(&obl_state_seen, __ATOMIC_ACQUIRE)) {
		IOLog("ONEIOS BACKLIGHT: off: the host did not answer the query\n");
		return false;
	}
	fHasAls = (obl_state.flags & OB_BRIGHT_HAS_ALS) != 0;
	if (!(obl_state.flags & OB_BRIGHT_HAS_PANEL) || obl_state.max_nits == 0) {
		IOLog("ONEIOS BACKLIGHT: off: the host has no panel backlight (light sensor: %s)\n", fHasAls ? "yes" : "no");
		return false;
	}
	if (!IOService::start(provider)) {
		return false;
	}
	fNits = obl_state.max_nits > 30000 ? 30000 : obl_state.max_nits;
	fMinNits16 = (uint32_t)(((uint64_t)obl_state.min_millinits << 16) / 1000);
	if (fMinNits16 == 0 || fMinNits16 >= fNits << 16) {
		fMinNits16 = 1u << 15;                      /* half a nit */
	}
	uint32_t value = (obl_state.level > OBL_FULL ? OBL_FULL : obl_state.level) * OBL_VALUE_MAX / OBL_FULL;
	setName("AppleARMBacklight");
	setProperty("backlight-control", true);
	/* backboardd's sandbox profile refuses FadePeriod on a backlight that is not Apple's (UniXNU-sheng int-276), and
	 * CoreBrightness sends it with every brightness: the grant of Droid-VM-UniXNU's zzwb-native-sandbox-grant-set-properties
	 * (in the arm64e series) lets backboardd alone set this service's properties. */
	setProperty("NativeSandboxGrantProcess", "backboardd");
	setProperty("max-nits", (unsigned long long)fNits, 32);
	setProperty("min-nits", (unsigned long long)((fMinNits16 + 0xffff) >> 16), 32);
	setProperty("OneiOSBacklight", "the Galaxy S24 Ultra's panel through xnuhost's brightness bridge");
	publish(value, nits16For(value));
	if (!calibration()) {
		IOLog("ONEIOS BACKLIGHT: off: no memory for the calibration properties (CoreBrightness would fault without them)\n");
		return false;
	}
	gOneiOSBacklight = this;
	registerService();
	IOLog("ONEIOS BACKLIGHT: on: the host's panel, %u nits at the top, %u/1000 at the bottom, now at %u of %u; brightness 0..%u "
	    "(the slider, squared by the host) and brightness-nits; calibration table of %u points, no digital dimming; light sensor: %s\n",
	    fNits, (uint32_t)(((uint64_t)fMinNits16 * 1000) >> 16), value, OBL_VALUE_MAX, OBL_VALUE_MAX, OBL_TABLE_POINTS,
	    fHasAls ? "host readings arrive (oneios-als-millilux)" : "none on the host");
	return true;
}

/* ---- conformance to "AppleARMBacklight" (see the header) ----------------------------------------- */

static bool
obl_conform(void)
{
	const OSSymbol *name = OSSymbol::withCString("AppleARMBacklight");
	const OSMetaClass *original = name != NULL ? OSMetaClass::getMetaClassWithName(name) : NULL;
	OSSafeReleaseNULL(name);
	if (original == NULL || original == OneiOSBacklight::metaClass) {
		return false;                               /* AppleARMPlatform has not started yet */
	}
	/* The original must itself come from IOService, or the link would cut this class off from it. */
	bool fromService = false;
	for (const OSMetaClass *m = original; m != NULL; m = m->getSuperClass()) {
		if (m == IOService::metaClass) {
			fromService = true;
			break;
		}
	}
	/* The superclass link is the one word of the metaclass that points at IOService's. Exactly one, or nothing is touched. */
	uintptr_t *words = (uintptr_t *)(uintptr_t)OneiOSBacklight::metaClass;
	const size_t count = sizeof(OSMetaClass) / sizeof(uintptr_t);
	size_t at = count, hits = 0;
	for (size_t i = 0; i < count; i++) {
		if (words[i] == (uintptr_t)IOService::metaClass) {
			at = i;
			hits++;
		}
	}
	if (!fromService || hits != 1 || OneiOSBacklight::metaClass->getSuperClass() != IOService::metaClass) {
		IOLog("ONEIOS BACKLIGHT: the service does not conform to AppleARMBacklight: the metaclass is not laid out as expected "
		    "(%u candidate words, original from IOService: %d); CoreBrightness may not take it\n", (unsigned)hits, fromService);
		return true;                                /* decided: do not try again */
	}
	words[at] = (uintptr_t)original;
	OneiOSBacklight *bl = gOneiOSBacklight;
	bool conforms = OneiOSBacklight::metaClass->getSuperClass() == original && bl != NULL && bl->metaCast("AppleARMBacklight") != NULL &&
	    OSDynamicCast(IOService, bl) != NULL;
	if (!conforms) {
		words[at] = (uintptr_t)IOService::metaClass;
	}
	IOLog("ONEIOS BACKLIGHT: %s (the original class of AppleARMPlatform is registered; oneios_bl_conform=0 turns this off)\n",
	    conforms ? "the service conforms to AppleARMBacklight now: its class is linked under the original's"
	    : "the service does not conform to AppleARMBacklight: the link did not take, undone");
	return true;
}

static void
obl_conform_thread(void *arg, wait_result_t result)
{
	(void)arg;
	(void)result;
	/* Said at the start and every 60 s: on the phone (2026-11-12) neither outcome line came in 18 minutes, so a look that
	 * never returns (the class lock) shows as a start line with nothing after it. */
	IOLog("ONEIOS BACKLIGHT: waiting for AppleARMBacklight's class (AppleARMPlatform) to make the service conform, %u s at most\n",
	    OBL_CONFORM_WAIT_S);
	for (unsigned waited = 0; waited < OBL_CONFORM_WAIT_S * 2; waited++) {
		if (obl_conform()) {
			return;
		}
		if (waited && waited % 120 == 0) {
			IOLog("ONEIOS BACKLIGHT: still no AppleARMBacklight class after %u s\n", waited / 2);
		}
		IOSleep(500);
	}
	IOLog("ONEIOS BACKLIGHT: the service does not conform to AppleARMBacklight: no class of that name was registered in %u s "
	    "(AppleARMPlatform did not start); CoreBrightness may not take it\n", OBL_CONFORM_WAIT_S);
}

extern "C" void
oneios_backlight_start(void)
{
	static bool tried;
	uint32_t on = 1, conform = 1;
	if (tried) {
		return;
	}
	tried = true;
	(void)PE_parse_boot_argn("oneios_bl", &on, sizeof(on));
	(void)PE_parse_boot_argn("oneios_bl_conform", &conform, sizeof(conform));
	if (on == 0) {
		IOLog("ONEIOS BACKLIGHT: off: oneios_bl=0\n");
		return;
	}
	IOService *resources = IOService::getResourceService();
	OneiOSBacklight *bl = OSTypeAlloc(OneiOSBacklight);
	if (bl == NULL || resources == NULL || !bl->init()) {
		IOLog("ONEIOS BACKLIGHT: off: the service could not be made\n");
		OSSafeReleaseNULL(bl);
		return;
	}
	if (!bl->attach(resources)) {
		IOLog("ONEIOS BACKLIGHT: off: no attach to IOResources\n");
		bl->release();
		return;
	}
	if (!bl->start(resources)) {
		bl->detach(resources);                      /* start said why */
		bl->release();
		return;
	}
	bl->release();                                  /* the registry keeps a started service */
	if (conform == 0) {
		IOLog("ONEIOS BACKLIGHT: oneios_bl_conform=0: the service is not made to conform to AppleARMBacklight\n");
		return;
	}
	thread_t thread;
	if (kernel_thread_start(obl_conform_thread, NULL, &thread) == KERN_SUCCESS) {
		thread_deallocate(thread);
	} else {
		IOLog("ONEIOS BACKLIGHT: the service does not conform to AppleARMBacklight: its thread could not be started\n");
	}
}
