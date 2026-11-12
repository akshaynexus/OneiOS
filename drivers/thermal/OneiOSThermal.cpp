/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS: the phone's temperatures and thermal pressure for iOS. xnuhost's thermal bridge (runtime/bridges/thermal) reads
 * the phone's thermal zones, Samsung's board thermistors and the battery's sensors, decides a pressure level on iOS's
 * scale (OSThermalNotification.h: 0 nominal, 10 light, 20 moderate, 30 heavy, 40 trapping, 50 sleeping) and sends it
 * over the bridge channel with the readings. This service publishes them in the registry:
 *
 *   IOService:/IOResources/oneios-thermal/OneiOSThermal
 *     ThermalPressureLevel   the level (number)          ThermalPressureLevelName  "nominal" ... "sleeping"
 *     ThermalReason          "case", "SoC", "battery"    HostStep                  xnuhost's CPU step, 0..3
 *     Temperatures           { Case, SoC, CPU, GPU, DDR, XO, Board, Battery } in hundredths of a degree C (Apple's unit
 *                            for a temperature: AppleSmartBattery's Temperature), present only when the phone has it
 *
 * and sends kIOMessageServicePropertyChange to its clients at each new level. iOS's own thermalmonitord does not read
 * this service: it reads Apple's sensors (an SMC and the PMU's HID sensors, none of which exist here). The guest
 * helper userland/helpers/oneios-thermal sets the pressure level iOS's frameworks read
 * (notify "com.apple.system.thermalpressurelevel") from it. README.md here has the path and what is not proven.
 * Boot argument: oneios_thermal=0 (no service).
 */
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOLocks.h>
#include <IOKit/IOMessage.h>
#include <libkern/c++/OSBoolean.h>
#include <libkern/c++/OSDictionary.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSString.h>
#include <pexpert/pexpert.h>

#include "oneios_bridge.h"

class OneiOSThermal : public IOService {
	OSDeclareDefaultStructors(OneiOSThermal);
public:
	void apply(const struct ob_thermal_state *state, bool tell);
};

OSDefineMetaClassAndStructors(OneiOSThermal, IOService);

static OneiOSThermal *gOneiOSThermal;
static IOLock *gThermalLock;
static struct ob_thermal_state gLatest;
static volatile uint32_t gLatestSeen;
static uint32_t gLoggedLevel = ~0u, gLoggedStep = ~0u;

static const char *
level_name(uint32_t level)
{
	switch (level) {
	case 0: return "nominal";
	case 10: return "light";
	case 20: return "moderate";
	case 30: return "heavy";
	case 40: return "trapping";
	case 50: return "sleeping";
	}
	return "unknown";
}

static void
put_temp(OSDictionary *d, const char *key, int32_t deci)
{
	if (deci == OB_THERMAL_UNKNOWN) {
		return;
	}
	OSNumber *n = OSNumber::withNumber((unsigned long long)(int64_t)(deci * 10), 32);   /* hundredths of a degree */
	if (n != NULL) {
		d->setObject(key, n);
		n->release();
	}
}

static void
deci_text(char *out, size_t size, int32_t v)
{
	if (v == OB_THERMAL_UNKNOWN) {
		snprintf(out, size, "-");
	} else {
		snprintf(out, size, "%s%d.%d C", v < 0 ? "-" : "", (v < 0 ? -v : v) / 10, (v < 0 ? -v : v) % 10);
	}
}

/* With gThermalLock held. */
void
OneiOSThermal::apply(const struct ob_thermal_state *s, bool tell)
{
	OSDictionary *temps = OSDictionary::withCapacity(8);
	if (temps != NULL) {
		put_temp(temps, "Case", s->case_temp);
		put_temp(temps, "SoC", s->soc);
		put_temp(temps, "CPU", s->cpu);
		put_temp(temps, "GPU", s->gpu);
		put_temp(temps, "DDR", s->ddr);
		put_temp(temps, "XO", s->xo);
		put_temp(temps, "Board", s->board);
		put_temp(temps, "Battery", s->battery);
		setProperty("Temperatures", temps);
		temps->release();
	}
	char reason[32];
	snprintf(reason, sizeof(reason), "%s%s%s", (s->reason & OB_THERMAL_BY_CASE) ? "case " : "", (s->reason & OB_THERMAL_BY_SOC) ? "SoC " : "",
	    (s->reason & OB_THERMAL_BY_BATTERY) ? "battery " : "");
	size_t n = strlen(reason);
	if (n > 0) {
		reason[n - 1] = 0;
	}
	setProperty("ThermalPressureLevel", (unsigned long long)s->level, 32);
	setProperty("ThermalPressureLevelName", level_name(s->level));
	setProperty("ThermalReason", reason);
	setProperty("HostStep", (unsigned long long)s->host_step, 32);

	if (s->level != gLoggedLevel || s->host_step != gLoggedStep) {
		char c[16], soc[16], bat[16];
		deci_text(c, sizeof(c), s->case_temp);
		deci_text(soc, sizeof(soc), s->soc);
		deci_text(bat, sizeof(bat), s->battery);
		IOLog("ONEIOS THERMAL: %s (%u)%s%s: case %s, SoC %s, battery %s; the host's CPU step %u\n", level_name(s->level), s->level,
		    reason[0] ? " by " : "", reason, c, soc, bat, s->host_step);
		bool changed = gLoggedLevel != ~0u;
		gLoggedLevel = s->level;
		gLoggedStep = s->host_step;
		if (tell && changed) {
			messageClients(kIOMessageServicePropertyChange);
		}
	}
}

/* On the bridge's worker thread. */
static void
oth_from_host(void *context, uint16_t type, const uint8_t *payload, uint32_t length)
{
	(void)context;
	if (type != OB_THERMAL_STATE || length < sizeof(struct ob_thermal_state)) {
		return;
	}
	struct ob_thermal_state state;
	memcpy(&state, payload, sizeof(state));
	IOLockLock(gThermalLock);
	gLatest = state;
	__atomic_store_n(&gLatestSeen, 1, __ATOMIC_RELEASE);
	OneiOSThermal *service = gOneiOSThermal;
	if (service != NULL) {
		service->apply(&gLatest, true);
	}
	IOLockUnlock(gThermalLock);
}

extern "C" void
oneios_thermal_start(void)
{
	static bool tried;
	uint32_t on = 1;
	if (tried) {
		return;
	}
	tried = true;
	(void)PE_parse_boot_argn("oneios_thermal", &on, sizeof(on));
	if (on == 0) {
		IOLog("ONEIOS THERMAL: off: oneios_thermal=0\n");
		return;
	}
	if (!oneios_bridge_ready() || !(oneios_bridge_channels() & (1u << OB_CH_THERMAL))) {
		IOLog("ONEIOS THERMAL: off: the host serves no thermal channel (no bridge, or no sensor readable on the phone)\n");
		return;
	}
	gThermalLock = IOLockAlloc();
	if (gThermalLock == NULL) {
		IOLog("ONEIOS THERMAL: off: no memory\n");
		return;
	}
	oneios_bridge_listen(OB_CH_THERMAL, oth_from_host, NULL);
	(void)oneios_bridge_send(OB_CH_THERMAL, OB_THERMAL_QUERY, NULL, 0);
	for (unsigned waited = 0; waited < 1000u && !__atomic_load_n(&gLatestSeen, __ATOMIC_ACQUIRE); waited += 10u) {
		IOSleep(10);
	}
	if (!__atomic_load_n(&gLatestSeen, __ATOMIC_ACQUIRE)) {
		IOLog("ONEIOS THERMAL: off: the host did not answer the query\n");
		return;
	}

	IOService *resources = IOService::getResourceService();
	IOService *nub = resources != NULL ? OSTypeAlloc(IOService) : NULL;
	if (nub == NULL || !nub->init()) {
		OSSafeReleaseNULL(nub);
		IOLog("ONEIOS THERMAL: off: no nub\n");
		return;
	}
	nub->setName("oneios-thermal");
	if (!nub->attach(resources)) {
		nub->release();
		IOLog("ONEIOS THERMAL: off: no attach to IOResources\n");
		return;
	}
	nub->registerService();
	nub->release();

	OneiOSThermal *service = OSTypeAlloc(OneiOSThermal);
	if (service == NULL || !service->init()) {
		OSSafeReleaseNULL(service);
		IOLog("ONEIOS THERMAL: off: the service could not be made\n");
		return;
	}
	if (!service->attach(nub) || !service->start(nub)) {
		service->release();
		IOLog("ONEIOS THERMAL: off: the service did not start\n");
		return;
	}
	IOLockLock(gThermalLock);
	service->apply(&gLatest, false);
	gOneiOSThermal = service;
	IOLockUnlock(gThermalLock);
	service->registerService();
	service->release();                         /* the registry keeps it */
	IOLog("ONEIOS THERMAL: on: OneiOSThermal published\n");
}
