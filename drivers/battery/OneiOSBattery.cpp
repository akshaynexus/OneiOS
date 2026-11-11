/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS: the phone's battery for iOS. xnuhost's battery bridge (runtime/bridges/battery) reads Android's
 * power_supply nodes and sends the state over the bridge channel; this class publishes it as an IOPMPowerSource,
 * the class powerd matches (IOServiceMatching("IOPMPowerSource") in 24A446's powerd) and reads CurrentCapacity,
 * MaxCapacity, IsCharging, ExternalConnected and the rest from. README.md here has the path and what is not proven.
 *
 * IOPMPowerSource is open XNU, so this subclasses it directly; the shape follows UniXNU-droidvm's DroidVMBattery.cpp
 * (which polls a goldfish device), with the host's messages in place of the device's registers. Boot argument:
 * oneios_battery=0 (no service).
 */
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOLocks.h>
#include <IOKit/pwr_mgt/IOPM.h>
#include <IOKit/pwr_mgt/IOPMPowerSource.h>
#include <libkern/c++/OSBoolean.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSSymbol.h>
#include <pexpert/pexpert.h>

#include "oneios_bridge.h"

#define OBA_WARN_PERCENT     10u
#define OBA_CRITICAL_PERCENT 2u
#define OBA_TIME_UNKNOWN     0xffff      /* what AppleSmartBattery publishes while it has no estimate */

class OneiOSBattery : public IOPMPowerSource {
	OSDeclareDefaultStructors(OneiOSBattery);
public:
	void apply(const struct ob_battery_state *state);
private:
	void setNumber(const OSSymbol *key, uint32_t value);
};

OSDefineMetaClassAndStructors(OneiOSBattery, IOPMPowerSource);

static const OSSymbol *gFullyCharged, *gRawExternal, *gRawCurrent, *gRawMax, *gDesign, *gNominal, *gTemperature, *gInstant;
static OneiOSBattery *gOneiOSBattery;
static IOLock *gBatteryLock;
static struct ob_battery_state gLatest;
static volatile uint32_t gLatestSeen;
static uint32_t gLoggedFlags = ~0u, gLoggedPercent = ~0u;

void
OneiOSBattery::setNumber(const OSSymbol *key, uint32_t value)
{
	OSNumber *n = OSNumber::withNumber((unsigned long long)value, 32);
	if (n != NULL) {
		setPSProperty(key, n);
		n->release();
	}
}

/* With gBatteryLock held. */
void
OneiOSBattery::apply(const struct ob_battery_state *s)
{
	bool present = (s->flags & OB_BATTERY_PRESENT) != 0, plugged = (s->flags & OB_BATTERY_PLUGGED) != 0;
	bool charging = (s->flags & OB_BATTERY_CHARGING) != 0, full = (s->flags & OB_BATTERY_FULL) != 0;
	uint32_t percent = s->percent > 100 ? 100 : s->percent;

	setBatteryInstalled(present);
	setExternalConnected(plugged);
	setExternalChargeCapable(plugged);
	setIsCharging(charging);
	setMaxCapacity(100);
	setCurrentCapacity(percent);
	setAtWarnLevel(!plugged && percent <= OBA_WARN_PERCENT);
	setAtCriticalLevel(!plugged && percent <= OBA_CRITICAL_PERCENT);
	setCycleCount(s->cycles);
	setTimeRemaining(s->minutes < 0 ? OBA_TIME_UNKNOWN : s->minutes);
	if (s->voltage_mv != 0) {
		setVoltage(s->voltage_mv);
	}
	if (s->current_ma != OB_BATTERY_UNKNOWN) {
		setAmperage(s->current_ma);
		setNumber(gInstant, (uint32_t)s->current_ma);
	}
	if (s->temp_deci_c != OB_BATTERY_UNKNOWN) {
		setNumber(gTemperature, (uint32_t)(s->temp_deci_c * 10));   /* hundredths of a degree, as AppleSmartBattery */
	}
	setPSProperty(gFullyCharged, full ? kOSBooleanTrue : kOSBooleanFalse);
	setPSProperty(gRawExternal, plugged ? kOSBooleanTrue : kOSBooleanFalse);
	setNumber(gRawCurrent, (uint32_t)((uint64_t)s->full_mah * percent / 100));
	setNumber(gRawMax, s->full_mah);
	setNumber(gNominal, s->full_mah);
	setNumber(gDesign, s->design_mah);
	updateStatus();

	if (s->flags != gLoggedFlags || percent != gLoggedPercent) {
		gLoggedFlags = s->flags;
		gLoggedPercent = percent;
		IOLog("ONEIOS BATTERY: %u%%, %s%s%s (%d/10 C, %u mV, %d mA)\n", percent, present ? (plugged ? "on power" : "on battery") : "no battery",
		    charging ? ", charging" : "", full ? ", full" : "", s->temp_deci_c == OB_BATTERY_UNKNOWN ? 0 : s->temp_deci_c, s->voltage_mv,
		    s->current_ma == OB_BATTERY_UNKNOWN ? 0 : s->current_ma);
	}
}

/* On the bridge's worker thread. */
static void
oba_from_host(void *context, uint16_t type, const uint8_t *payload, uint32_t length)
{
	(void)context;
	if (type != OB_BATTERY_STATE || length < sizeof(struct ob_battery_state)) {
		return;
	}
	struct ob_battery_state state;
	memcpy(&state, payload, sizeof(state));
	IOLockLock(gBatteryLock);
	gLatest = state;
	__atomic_store_n(&gLatestSeen, 1, __ATOMIC_RELEASE);
	if (gOneiOSBattery != NULL) {
		gOneiOSBattery->apply(&gLatest);
	}
	IOLockUnlock(gBatteryLock);
}

extern "C" void
oneios_battery_start(void)
{
	static bool tried;
	uint32_t on = 1;
	if (tried) {
		return;
	}
	tried = true;
	(void)PE_parse_boot_argn("oneios_battery", &on, sizeof(on));
	if (on == 0) {
		IOLog("ONEIOS BATTERY: off: oneios_battery=0\n");
		return;
	}
	if (!oneios_bridge_ready() || !(oneios_bridge_channels() & (1u << OB_CH_BATTERY))) {
		IOLog("ONEIOS BATTERY: off: the host serves no battery channel (no bridge, or no battery node on the phone)\n");
		return;
	}
	gBatteryLock = IOLockAlloc();
	gFullyCharged = OSSymbol::withCString(kIOPMFullyChargedKey);
	gRawExternal = OSSymbol::withCString("AppleRawExternalConnected");
	gRawCurrent = OSSymbol::withCString("AppleRawCurrentCapacity");
	gRawMax = OSSymbol::withCString("AppleRawMaxCapacity");
	gDesign = OSSymbol::withCString("DesignCapacity");
	gNominal = OSSymbol::withCString("NominalChargeCapacity");
	gTemperature = OSSymbol::withCString("Temperature");
	gInstant = OSSymbol::withCString("InstantAmperage");
	if (gBatteryLock == NULL || gFullyCharged == NULL || gRawExternal == NULL || gRawCurrent == NULL || gRawMax == NULL || gDesign == NULL ||
	    gNominal == NULL || gTemperature == NULL || gInstant == NULL) {
		IOLog("ONEIOS BATTERY: off: no memory\n");
		return;
	}
	oneios_bridge_listen(OB_CH_BATTERY, oba_from_host, NULL);
	(void)oneios_bridge_send(OB_CH_BATTERY, OB_BATTERY_QUERY, NULL, 0);
	for (unsigned waited = 0; waited < 1000u && !__atomic_load_n(&gLatestSeen, __ATOMIC_ACQUIRE); waited += 10u) {
		IOSleep(10);
	}
	if (!__atomic_load_n(&gLatestSeen, __ATOMIC_ACQUIRE)) {
		IOLog("ONEIOS BATTERY: off: the host did not answer the query\n");
		return;
	}

	IOService *resources = IOService::getResourceService();
	IOService *nub = resources != NULL ? OSTypeAlloc(IOService) : NULL;
	if (nub == NULL || !nub->init()) {
		OSSafeReleaseNULL(nub);
		IOLog("ONEIOS BATTERY: off: no nub\n");
		return;
	}
	nub->setName("oneios-battery");
	nub->setProperty("built-in", kOSBooleanTrue);
	if (!nub->attach(resources)) {
		nub->release();
		IOLog("ONEIOS BATTERY: off: no attach to IOResources\n");
		return;
	}
	nub->registerService();
	nub->release();

	OneiOSBattery *battery = OSTypeAlloc(OneiOSBattery);
	if (battery == NULL || !battery->init()) {
		OSSafeReleaseNULL(battery);
		IOLog("ONEIOS BATTERY: off: the service could not be made\n");
		return;
	}
	if (!battery->attach(nub) || !battery->start(nub)) {
		battery->release();
		IOLog("ONEIOS BATTERY: off: the service did not start\n");
		return;
	}
	IOLockLock(gBatteryLock);
	battery->apply(&gLatest);
	gOneiOSBattery = battery;
	uint32_t percent = gLatest.percent, flags = gLatest.flags;
	IOLockUnlock(gBatteryLock);
	battery->registerService();
	battery->release();                         /* the registry keeps it */
	IOLog("ONEIOS BATTERY: on: IOPMPowerSource published at %u%%, %s\n", percent, (flags & OB_BATTERY_PLUGGED) ? "on power" : "on battery");
}
