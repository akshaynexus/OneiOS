/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS: the legacy iOS "vibrator" IOService and the closed-loop node, on the host actuator of
 * oneios_haptic.c (the Galaxy S24 Ultra's LRA, through xnuhost's haptics bridge).
 *
 * Ported as is from iOSPlus10 (patches/laurel/xnu/2700-laurel-pmi632-haptics.patch,
 * iokit/Kernel/arm/LaurelVibrator.cpp, itself a port of negroni 0301/0302). Changes: names; the
 * "intensity" of an on step is kept (0..1000) and sent to the host instead of being reduced to on/off
 * (the Mi A3's ERM had no amplitude); the service is made by oneios_vibrator_start() from the board's
 * late probe instead of a KernelConfigTables entry, so no kernel patch is needed.
 *
 * iOS [iOSPlus10 docs/laurel_sprout/haptics.md; negroni docs/haptics-plan.md section 4]: MediaExperience
 * FigVibrator, when VibeSynthEngineHasHardwareSupport() is false (closed-loop MobileGestalt answer
 * false), takes IOServiceGetMatchingService(IOServiceNameMatching("vibrator")) and
 * IORegistryEntrySetCFProperties:
 *   start:   { "hertz_millisecs" = ( {"intensity" = SInt32 16.16}, on_ms, false, off_ms, ... ), "repeat" = bool }
 *   stop:    a bare CFNumber 0
 * No user client. Here: pairs (state, ms); state = a dictionary with "intensity" > 0 or true = on,
 * false or intensity 0 = off. The steps run on our work loop timer; an on step asks
 * oneios_haptic_vib(ms + 10, intensity, 0) so back-to-back on steps do not gap.
 *
 * Repeat (iOSPlus10, 2026-11-01, the endless vibration on the Mi A3) [20H392 MediaExperience]:
 * FigVibrator's play sends {[on, off], repeat = true} when the total is longer than one period, and
 * the only end is its own one-shot timer at the total; for a pattern dictionary without TotalDuration
 * the total is 31 days, so the loop ends only when the iOS client calls FigVibratorStop. Two bounds:
 *   - a repeating pattern stops LV_REPEAT_CAP_MS (60 s) after its start request when no new request
 *     comes. Kill switch oneios_vib_cap=0 (then 5 min).
 *   - a power or volume key press (oneios_vibrator_key) stops a running repeat, as iOS silences a
 *     ring with these keys. One-shot patterns are not touched.
 * Each new request kind (its shape: classes, keys, element types) is logged once with the caller.
 *
 * oneios_haptic_publish_closed_loop(): IODeviceTree:/product/haptics {closed-loop 1,
 * supports-3rd-party-haptics 1}. Called by oneios_haptic.c only when the root has an Actuator HAL
 * plug-in and VirtualAudio.plugin: with closed-loop on, FigVibrator, Core Haptics and UIKit all play
 * on VirtualAudioDevice_Actuator, and without that device iOS plays no vibration at all.
 */
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOWorkLoop.h>
#include <IOKit/IOCommandGate.h>
#include <IOKit/IOTimerEventSource.h>
#include <IOKit/IORegistryEntry.h>
#include <IOKit/IODeviceTreeSupport.h>
#include <libkern/c++/OSArray.h>
#include <libkern/c++/OSBoolean.h>
#include <libkern/c++/OSCollectionIterator.h>
#include <libkern/c++/OSData.h>
#include <libkern/c++/OSDictionary.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSString.h>
#include <libkern/c++/OSSymbol.h>
#include <libkern/libkern.h>
#include <pexpert/pexpert.h>
#include <stdarg.h>

#include "oneios_bridge.h"

extern "C" void proc_selfname(char *buf, int size);

#define LV_MAX_STEPS        64u
#define LV_MAX_STEP_MS      10000u
#define LV_REPEAT_MAX_MS    (5u * 60u * 1000u)  /* with oneios_vib_cap=0 */
#define LV_REPEAT_CAP_MS    60000u              /* a repeat with no new request ends here */
#define LV_LOG              16u
#define LV_KINDS            16u                 /* request shapes logged once each */

class OneiOSVibrator : public IOService {
	OSDeclareDefaultStructors(OneiOSVibrator);
public:
	bool start(IOService *provider) APPLE_KEXT_OVERRIDE;
	IOReturn setProperties(OSObject *properties) APPLE_KEXT_OVERRIDE;
	void keyPress(int key);
private:
	struct Step { uint32_t ms; uint32_t intensity; bool on; };    /* intensity 0..1000 */
	IOWorkLoop *fLoop;
	IOCommandGate *fGate;
	IOTimerEventSource *fTimer;
	Step fSteps[LV_MAX_STEPS];
	uint32_t fCount, fNext, fElapsedMs, fCalls, fCapMs, fCycleMs;
	uint32_t fKind[LV_KINDS], fKinds;
	bool fRepeat, fRunning;
	static IOReturn gatedSet(OSObject *owner, void *arg0, void *, void *, void *);
	static IOReturn gatedKey(OSObject *owner, void *arg0, void *, void *, void *);
	static void timerFired(OSObject *owner, IOTimerEventSource *sender);
	IOReturn apply(OSObject *properties);
	void logKind(OSObject *properties);
	void onKey(int key);
	void runStep();
	void stopAll();
};

OSDefineMetaClassAndStructors(OneiOSVibrator, IOService);

static OneiOSVibrator * volatile gOneiOSVibrator;

bool
OneiOSVibrator::start(IOService *provider)
{
	if (oneios_haptic_start() != 0) {
		return false;                   /* oneios_haptic.c said why ("ONEIOS HAPTIC: off: ...") */
	}
	if (!IOService::start(provider)) {
		return false;
	}
	uint32_t cap = 1;
	(void)PE_parse_boot_argn("oneios_vib_cap", &cap, sizeof(cap));
	fCapMs = cap ? LV_REPEAT_CAP_MS : LV_REPEAT_MAX_MS;
	fLoop = IOWorkLoop::workLoop();
	fGate = fLoop ? IOCommandGate::commandGate(this, gatedSet) : NULL;
	fTimer = fLoop ? IOTimerEventSource::timerEventSource(this, timerFired) : NULL;
	if (!fGate || !fTimer || fLoop->addEventSource(fGate) != kIOReturnSuccess ||
	    fLoop->addEventSource(fTimer) != kIOReturnSuccess) {
		IOLog("ONEIOS VIBRATOR: off: no work loop (the motor works, iOS cannot reach it)\n");
		return false;
	}
	setName("vibrator");
	setProperty("OneiOSHaptics", "host actuator (Galaxy S24 Ultra LRA, Cirrus CS40L26) through xnuhost");
	registerService();
	gOneiOSVibrator = this;
	IOLog("ONEIOS VIBRATOR: on: published \"vibrator\" (FigVibrator hertz_millisecs; a repeat ends after %u s "
	    "without a new request%s, or at a power/volume key press)\n", fCapMs / 1000u,
	    cap ? "" : " (oneios_vib_cap=0)");
	return true;
}

IOReturn
OneiOSVibrator::setProperties(OSObject *properties)
{
	if (!fGate) {
		return kIOReturnNotReady;
	}
	return fGate->runAction(gatedSet, properties);
}

IOReturn
OneiOSVibrator::gatedSet(OSObject *owner, void *arg0, void *, void *, void *)
{
	return static_cast<OneiOSVibrator *>(owner)->apply(static_cast<OSObject *>(arg0));
}

/* ---- one log line per request shape ------------------------------------------------------------- */

struct LVText {
	char *p;
	size_t n, used;
	uint32_t hash;                  /* FNV-1a of the shape (types only, no values) */
};

static void lv_put(LVText &t, bool shape, const char *fmt, ...) __printflike(3, 4);

static void
lv_put(LVText &t, bool shape, const char *fmt, ...)
{
	char tmp[64];
	va_list ap;
	va_start(ap, fmt);
	int w = vsnprintf(tmp, sizeof(tmp), fmt, ap);
	va_end(ap);
	if (w < 0) {
		return;
	}
	size_t len = (size_t)w < sizeof(tmp) ? (size_t)w : sizeof(tmp) - 1;
	if (shape) {
		for (size_t i = 0; i < len; i++) {
			t.hash = (t.hash ^ (uint8_t)tmp[i]) * 16777619u;
		}
	}
	if (t.used + 1 < t.n) {
		size_t room = t.n - 1 - t.used;
		size_t c = len < room ? len : room;
		memcpy(t.p + t.used, tmp, c);
		t.used += c;
		t.p[t.used] = 0;
	}
}

/* shape text goes into the hash; values only into the line */
static void
lv_describe(LVText &t, OSObject *o, unsigned depth)
{
	if (o == NULL) {
		lv_put(t, true, "null");
	} else if (OSNumber *n = OSDynamicCast(OSNumber, o)) {
		lv_put(t, true, "N");
		lv_put(t, false, "%llu", n->unsigned64BitValue());
	} else if (o == kOSBooleanTrue || o == kOSBooleanFalse) {
		lv_put(t, true, "B");
		lv_put(t, false, "%s", o == kOSBooleanTrue ? ":true" : ":false");
	} else if (OSString *s = OSDynamicCast(OSString, o)) {
		lv_put(t, true, "S");
		lv_put(t, false, "\"%.24s\"", s->getCStringNoCopy());
	} else if (OSData *d = OSDynamicCast(OSData, o)) {
		lv_put(t, true, "D");
		lv_put(t, false, "%u", d->getLength());
	} else if (OSArray *a = OSDynamicCast(OSArray, o)) {
		unsigned c = a->getCount();
		lv_put(t, true, "[");
		lv_put(t, false, "%u:", c);
		for (unsigned i = 0; i < c && i < 4u && depth < 3u; i++) {
			if (i) {
				lv_put(t, true, ",");
			}
			lv_describe(t, a->getObject(i), depth + 1);
		}
		lv_put(t, false, "%s", c > 4u ? ",..." : "");
		lv_put(t, true, "]");
	} else if (OSDictionary *dict = OSDynamicCast(OSDictionary, o)) {
		lv_put(t, true, "{");
		OSCollectionIterator *it = depth < 3u ? OSCollectionIterator::withCollection(dict) : NULL;
		bool first = true;
		while (OSSymbol *k = it ? OSDynamicCast(OSSymbol, it->getNextObject()) : NULL) {
			lv_put(t, true, "%s%.24s=", first ? "" : ",", k->getCStringNoCopy());
			lv_describe(t, dict->getObject(k), depth + 1);
			first = false;
		}
		OSSafeReleaseNULL(it);
		lv_put(t, true, "}");
	} else {
		lv_put(t, true, "%s", o->getMetaClass()->getClassName());
	}
}

void
OneiOSVibrator::logKind(OSObject *properties)
{
	char line[200];
	LVText t = { line, sizeof(line), 0, 2166136261u };
	line[0] = 0;
	lv_describe(t, properties, 0);
	for (uint32_t i = 0; i < fKinds; i++) {
		if (fKind[i] == t.hash) {
			return;
		}
	}
	if (fKinds >= LV_KINDS) {
		return;
	}
	fKind[fKinds++] = t.hash;
	char who[32];
	proc_selfname(who, sizeof(who));
	IOLog("ONEIOS VIBRATOR: request kind %u from %s: %s\n", fKinds, who, line);
}

/* ---- requests ----------------------------------------------------------------------------------- */

void
OneiOSVibrator::stopAll()
{
	fTimer->cancelTimeout();
	fRunning = false;
	(void)oneios_haptic_vib(0, 0, 0);
}

IOReturn
OneiOSVibrator::apply(OSObject *properties)
{
	bool log = fCalls++ < LV_LOG;
	logKind(properties);
	if (OSDynamicCast(OSNumber, properties)) {
		if (log) {
			IOLog("ONEIOS VIBRATOR: stop%s\n", fRunning ? " (a pattern was running)" : "");
		}
		stopAll();
		return kIOReturnSuccess;
	}
	OSDictionary *d = OSDynamicCast(OSDictionary, properties);
	OSArray *a = d ? OSDynamicCast(OSArray, d->getObject("hertz_millisecs")) : NULL;
	if (!a) {
		if (log) {
			IOLog("ONEIOS VIBRATOR: unknown request (%s): ignored\n",
			    properties ? properties->getMetaClass()->getClassName() : "null");
		}
		return kIOReturnBadArgument;
	}
	stopAll();
	fCount = 0;
	uint32_t onMs = 0, cycle = 0;
	for (unsigned i = 0; i + 1 < a->getCount() && fCount < LV_MAX_STEPS; i += 2) {
		OSObject *state = a->getObject(i);
		OSNumber *ms = OSDynamicCast(OSNumber, a->getObject(i + 1));
		if (!ms) {
			continue;
		}
		bool on = false;
		uint32_t intensity = 1000;
		if (OSDictionary *sd = OSDynamicCast(OSDictionary, state)) {
			OSNumber *in = OSDynamicCast(OSNumber, sd->getObject("intensity"));
			int64_t v = in ? (int64_t)in->unsigned64BitValue() : 65536;
			if (in && in->numberOfBits() <= 32) {
				v = (int32_t)in->unsigned32BitValue();
			}
			on = v > 0;
			/* SInt32 16.16: 65536 = 1.0 */
			intensity = v >= 65536 ? 1000u : v > 0 ? (uint32_t)(v * 1000 / 65536) : 0u;
			intensity = on && intensity == 0 ? 1u : intensity;
		} else if (state == kOSBooleanTrue) {
			on = true;
		}
		uint64_t t = ms->unsigned64BitValue();
		fSteps[fCount].ms = t == 0 ? 1 : t > LV_MAX_STEP_MS ? LV_MAX_STEP_MS : (uint32_t)t;
		fSteps[fCount].on = on;
		fSteps[fCount].intensity = on ? intensity : 0;
		onMs += on ? fSteps[fCount].ms : 0;
		cycle += fSteps[fCount].ms;
		fCount++;
	}
	OSBoolean *rep = OSDynamicCast(OSBoolean, d->getObject("repeat"));
	fRepeat = rep == kOSBooleanTrue;
	fCycleMs = cycle;
	if (log) {
		IOLog("ONEIOS VIBRATOR: start: %u steps (first %u ms %s, %u ms on of %u ms), repeat %d%s\n", fCount,
		    fCount ? fSteps[0].ms : 0, fCount && fSteps[0].on ? "on" : "off", onMs, cycle, fRepeat,
		    fRepeat ? " (until iOS stops it, a key press, or the cap)" : "");
	}
	if (fCount == 0) {
		return kIOReturnBadArgument;
	}
	fNext = 0;
	fElapsedMs = 0;
	fRunning = true;
	runStep();
	return kIOReturnSuccess;
}

void
OneiOSVibrator::runStep()
{
	if (!fRunning) {
		return;
	}
	if (fRepeat && fElapsedMs >= fCapMs) {
		IOLog("ONEIOS VIBRATOR: repeat stopped: %u s and no new request (iOS sent no stop; %u cycles of %u ms)\n",
		    fElapsedMs / 1000u, fCycleMs ? fElapsedMs / fCycleMs : 0, fCycleMs);
		stopAll();
		return;
	}
	if (fNext >= fCount) {
		if (!fRepeat) {
			stopAll();
			return;
		}
		fNext = 0;
	}
	const Step &s = fSteps[fNext++];
	if (s.on) {
		(void)oneios_haptic_vib(s.ms + 10u, s.intensity, 0);
	}
	fElapsedMs += s.ms;
	fTimer->setTimeoutMS(s.ms);
}

void
OneiOSVibrator::timerFired(OSObject *owner, IOTimerEventSource *)
{
	static_cast<OneiOSVibrator *>(owner)->runStep();
}

/* ---- keys --------------------------------------------------------------------------------------- */

void
OneiOSVibrator::onKey(int k)
{
	if (!fRunning || !fRepeat) {
		return;                         /* only a loop is silenced; a one-shot ends by itself */
	}
	static const char *const names[3] = { "power", "volume up", "volume down" };
	IOLog("ONEIOS VIBRATOR: repeat stopped by the %s key after %u ms (as iOS silences a ring)\n",
	    k >= 0 && k < 3 ? names[k] : "?", fElapsedMs);
	stopAll();
}

IOReturn
OneiOSVibrator::gatedKey(OSObject *owner, void *arg0, void *, void *, void *)
{
	static_cast<OneiOSVibrator *>(owner)->onKey((int)(intptr_t)arg0);
	return kIOReturnSuccess;
}

void
OneiOSVibrator::keyPress(int k)
{
	if (fGate) {
		(void)fGate->runAction(gatedKey, (void *)(intptr_t)k);
	}
}

/* the HID input driver's thread (no locks held), on every key change: 0 power, 1 volume up, 2 volume down */
extern "C" void
oneios_vibrator_key(int key, int down)
{
	OneiOSVibrator *v = gOneiOSVibrator;
	if (v == NULL || !down) {
		return;
	}
	v->keyPress(key);
}

/* IODeviceTree:/product/haptics as in an iPhone's ADT; 0 = published (or there already). */
extern "C" int
oneios_haptic_publish_closed_loop(void)
{
	IORegistryEntry *product = IORegistryEntry::fromPath("/product", gIODTPlane);
	if (product == NULL) {
		IOLog("ONEIOS HAPTIC UI: no IODeviceTree:/product\n");
		return -1;
	}
	IORegistryEntry *old = product->childFromPath("haptics", gIODTPlane);
	if (old != NULL) {
		IOLog("ONEIOS HAPTIC UI: IODeviceTree:/product/haptics is in the ADT already: kept\n");
		old->release();
		product->release();
		return 0;
	}
	static const uint32_t one = 1;
	static const char name[] = "haptics";
	IORegistryEntry *node = new IORegistryEntry;
	OSData *n = OSData::withBytes(name, sizeof(name));
	OSData *cl = OSData::withBytes(&one, sizeof(one));
	OSData *third = OSData::withBytes(&one, sizeof(one));
	bool ok = node && n && cl && third && node->init(NULL);
	if (ok) {
		node->setName(name);
		ok = node->setProperty("name", n) && node->setProperty("closed-loop", cl) &&
		    node->setProperty("supports-3rd-party-haptics", third) && node->attachToParent(product, gIODTPlane);
	}
	OSSafeReleaseNULL(n);
	OSSafeReleaseNULL(cl);
	OSSafeReleaseNULL(third);
	OSSafeReleaseNULL(node);        /* the parent link keeps it */
	product->release();
	return ok ? 0 : -1;
}

/* The service, made by hand under IOResources (no personality in the kernel's catalogue): once, from
 * the board's late probe. */
extern "C" void
oneios_vibrator_start(void)
{
	static bool tried;
	if (tried) {
		return;
	}
	tried = true;
	IOService *resources = IOService::getResourceService();
	OneiOSVibrator *v = OSTypeAlloc(OneiOSVibrator);
	if (v == NULL || resources == NULL || !v->init()) {
		IOLog("ONEIOS VIBRATOR: off: the service could not be made\n");
		OSSafeReleaseNULL(v);
		return;
	}
	if (!v->attach(resources)) {
		IOLog("ONEIOS VIBRATOR: off: no attach to IOResources\n");
		v->release();
		return;
	}
	if (!v->start(resources)) {
		v->detach(resources);           /* start said why */
	}
	v->release();                           /* the registry keeps a started service */
}
