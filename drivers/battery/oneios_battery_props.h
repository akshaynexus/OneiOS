/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS: what OneiOSBattery publishes for a host state, and when it tells powerd. Portable C and C++ with no kernel
 * call, unit-tested on the host (test_battery_props.c, drivers/tests/host.sh); OneiOSBattery.cpp is its one user.
 *
 * Why the order matters (iOS 27.0.1 24A446, read with ipsw in powerd and the shared cache's SpringBoard; README.md
 * "The charging chime"): powerd reads the IOPMPowerSource when it first matches it and publishes the internal
 * battery's power source with "Play Charging Chime" = ExternalConnected (or a battery case, or a wireless adapter).
 * SpringBoard's SBChargingController chimes for a power source ID whose "Play Charging Chime" it sees true and has
 * not marked yet (_powerSourceWantsToPlayChime, on every com.apple.system.powersources.timeremaining); at startup
 * (-[SpringBoard _finalizeStartupAfterScenesDidConnect:]) it marks every connected source without a sound when the
 * startup transition has a boot overlay or is dark. A power source whose ExternalConnected is false (or missing)
 * first and true later is a charger plugged in after startup: the chime. So the service carries the host's real
 * state in its registry properties before registerService() (before powerd can match it), the first publish sends
 * no kIOPMMessageBatteryStatusHasChanged, and later ones are sent only when a property changed.
 */
#ifndef ONEIOS_BATTERY_PROPS_H
#define ONEIOS_BATTERY_PROPS_H

#include <stdint.h>
#ifndef __cplusplus
#include <stdbool.h>
#endif
#include "oneios_bridge_proto.h"

#define OBA_WARN_PERCENT     10u
#define OBA_CRITICAL_PERCENT 2u
#define OBA_TIME_UNKNOWN     0xffffu     /* what AppleSmartBattery publishes while it has no estimate */

/* Every value OneiOSBattery publishes, by its IOPMPowerSource key. */
struct oba_props {
	bool     installed;                  /* BatteryInstalled */
	bool     external;                   /* ExternalConnected: powerd's "Power Source State" and "Play Charging Chime" */
	bool     charge_capable;             /* ExternalChargeCapable */
	bool     charging;                   /* IsCharging */
	bool     full;                       /* FullyCharged */
	bool     raw_external;               /* AppleRawExternalConnected: powerd's "Raw External Connected", "Show Charging UI" */
	bool     warn, critical;             /* AtWarnLevel, AtCriticalLevel */
	uint32_t current, max;               /* CurrentCapacity (percent), MaxCapacity (100) */
	uint32_t cycles;                     /* CycleCount */
	uint32_t time_remaining;             /* TimeRemaining (minutes), OBA_TIME_UNKNOWN */
	bool     has_voltage, has_amperage, has_temperature;
	uint32_t voltage;                    /* Voltage (mV) */
	int32_t  amperage;                   /* Amperage and InstantAmperage (mA, + into the battery) */
	uint32_t temperature;                /* Temperature (hundredths of a degree, as AppleSmartBattery) */
	uint32_t raw_current, raw_max;       /* AppleRawCurrentCapacity, AppleRawMaxCapacity (mAh) */
	uint32_t nominal, design;            /* NominalChargeCapacity, DesignCapacity (mAh) */
};

static inline void
oba_props_from_state(const struct ob_battery_state *s, struct oba_props *p)
{
	bool plugged = (s->flags & OB_BATTERY_PLUGGED) != 0;
	uint32_t percent = s->percent > 100 ? 100 : s->percent;

	p->installed = (s->flags & OB_BATTERY_PRESENT) != 0;
	p->external = plugged;
	p->charge_capable = plugged;
	p->charging = (s->flags & OB_BATTERY_CHARGING) != 0;
	p->full = (s->flags & OB_BATTERY_FULL) != 0;
	p->raw_external = plugged;
	p->warn = !plugged && percent <= OBA_WARN_PERCENT;
	p->critical = !plugged && percent <= OBA_CRITICAL_PERCENT;
	p->current = percent;
	p->max = 100;
	p->cycles = s->cycles;
	p->time_remaining = s->minutes < 0 ? OBA_TIME_UNKNOWN : (uint32_t)s->minutes;
	p->has_voltage = s->voltage_mv != 0;
	p->voltage = p->has_voltage ? s->voltage_mv : 0;
	p->has_amperage = s->current_ma != OB_BATTERY_UNKNOWN;
	p->amperage = p->has_amperage ? s->current_ma : 0;
	p->has_temperature = s->temp_deci_c != OB_BATTERY_UNKNOWN;
	p->temperature = p->has_temperature ? (uint32_t)(s->temp_deci_c * 10) : 0;
	p->raw_current = (uint32_t)((uint64_t)s->full_mah * percent / 100);
	p->raw_max = s->full_mah;
	p->nominal = s->full_mah;
	p->design = s->design_mah;
}

/* Whether powerd would read anything different. A value the service does not publish (has_* false) keeps its last
 * published one in the registry, as setPSProperty does, so it is no change. */
static inline bool
oba_props_differ(const struct oba_props *a, const struct oba_props *b)
{
	if (a->installed != b->installed || a->external != b->external || a->charge_capable != b->charge_capable ||
	    a->charging != b->charging || a->full != b->full || a->raw_external != b->raw_external || a->warn != b->warn ||
	    a->critical != b->critical || a->current != b->current || a->max != b->max || a->cycles != b->cycles ||
	    a->time_remaining != b->time_remaining || a->raw_current != b->raw_current || a->raw_max != b->raw_max ||
	    a->nominal != b->nominal || a->design != b->design) {
		return true;
	}
	return (b->has_voltage && (!a->has_voltage || a->voltage != b->voltage)) ||
	       (b->has_amperage && (!a->has_amperage || a->amperage != b->amperage)) ||
	       (b->has_temperature && (!a->has_temperature || a->temperature != b->temperature));
}

enum oba_publish {
	OBA_PUBLISH_NOTHING = 0,             /* the same values: no property set, no message */
	OBA_PUBLISH_INITIAL = 1,             /* the first: every property into the registry, no message (before registerService) */
	OBA_PUBLISH_CHANGE = 2,              /* a change: the properties, then kIOPMMessageBatteryStatusHasChanged */
};

/* The publisher's memory: what was last published. Zero it before the first call. */
struct oba_publisher {
	bool             published;
	struct oba_props last;
};

/* What to do with a host state. The first call is the initial publish; then a change only when a value differs.
 * *out gets the values to publish (the merged ones: an unknown reading keeps the last published value). */
static inline enum oba_publish
oba_publish(struct oba_publisher *pub, const struct ob_battery_state *s, struct oba_props *out)
{
	struct oba_props now;
	oba_props_from_state(s, &now);
	if (!pub->published) {
		pub->published = true;
		pub->last = now;
		*out = now;
		return OBA_PUBLISH_INITIAL;
	}
	if (!oba_props_differ(&pub->last, &now)) {
		*out = pub->last;
		return OBA_PUBLISH_NOTHING;
	}
	if (!now.has_voltage && pub->last.has_voltage) {
		now.has_voltage = true;
		now.voltage = pub->last.voltage;
	}
	if (!now.has_amperage && pub->last.has_amperage) {
		now.has_amperage = true;
		now.amperage = pub->last.amperage;
	}
	if (!now.has_temperature && pub->last.has_temperature) {
		now.has_temperature = true;
		now.temperature = pub->last.temperature;
	}
	pub->last = now;
	*out = now;
	return OBA_PUBLISH_CHANGE;
}

#endif
