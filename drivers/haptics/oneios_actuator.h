/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS haptics: what the "Actuator" audio device's samples ask of the motor. Shared by the kernel
 * (oneios_haptic.c, called once per engine period of the Actuator IOAudio2Device) and the host test
 * (test_actuator.c). No kernel or libc call: the state is the caller's, time is counted in the
 * periods it is given.
 *
 * iOS plays its haptics as audio on the Actuator device (Core Haptics, UIFeedbackGenerator, the
 * keyboard, and with closed-loop on also ringtone and alert patterns): 48 kHz samples whose envelope is
 * the force and whose carrier is the sharpness (145..300 Hz on an iPhone). The phone's LRA is driven by
 * sine effects (runtime/bridges/haptics): one request is "vibrate for N ms at intensity I and F Hz",
 * and each request is an effect upload over I2C. So the samples are turned into few requests:
 *   - a period is loud when a sample's magnitude is above OA_SILENT;
 *   - the first loud period asks for OA_TAP_MS (a UI tap is 3..20 ms of samples; a shorter buzz is
 *     not felt); while the run goes on, a new request is sent when the one in flight has less than
 *     OA_RENEW_MS left (OA_SEGMENT_MS each once the run is longer than a tap) or the level moved by
 *     more than OA_LEVEL_STEP of 1000: about 20 requests a second for a long vibration;
 *   - a long run (OA_LONG_MS or more) is stopped after OA_QUIET_MS of silence; a tap runs out;
 *   - intensity = the period's peak, 1..1000 of full scale; frequency = the run's zero crossings,
 *     once the run is 10 ms long (0 before: the host plays the actuator's resonance).
 */
#ifndef ONEIOS_ACTUATOR_H
#define ONEIOS_ACTUATOR_H

#include <stdbool.h>
#include <stdint.h>

#define OA_RATE        48000u
#define OA_SILENT      256u     /* of 32768 */
#define OA_TAP_MS      20u
#define OA_SEGMENT_MS  50u
#define OA_RENEW_MS    12u
#define OA_LONG_MS     30u
#define OA_QUIET_MS    10u
#define OA_LEVEL_STEP  100u

enum { OA_NONE = 0, OA_VIB = 1, OA_STOP = 2 };

struct oa_state {
	bool on;                /* a request is in flight */
	uint32_t now_us;        /* the periods given so far, in microseconds */
	uint32_t until_us;      /* when the request in flight ends */
	uint32_t level, hz;     /* what it asked */
	uint32_t run_us, run_crossings, quiet_us;
	int8_t sign;            /* the sign of the first channel's last sample above OA_SILENT (0: none yet) */
};

struct oa_request {
	int action;             /* OA_NONE, OA_VIB, OA_STOP */
	uint32_t ms, intensity, hz;
};

static inline struct oa_request
oa_period(struct oa_state *s, const int16_t *frames, uint32_t count, uint32_t channels)
{
	struct oa_request r = { OA_NONE, 0, 0, 0 };
	uint32_t peak = 0, crossings = 0;
	uint32_t period_us = (uint32_t)((uint64_t)count * 1000000u / OA_RATE);

	if (channels == 0) {
		channels = 1;
	}
	for (uint32_t i = 0; i < count; i++) {
		for (uint32_t c = 0; c < channels; c++) {
			int32_t v = frames[i * channels + c];
			uint32_t m = (uint32_t)(v < 0 ? -v : v);
			peak = m > peak ? m : peak;
		}
		int16_t x = frames[i * channels];
		uint32_t m = (uint32_t)(x < 0 ? -(int32_t)x : x);
		if (m > OA_SILENT) {            /* a crossing with hysteresis: the sign of loud samples only */
			int8_t sign = x < 0 ? -1 : 1;
			if (s->sign != 0 && sign != s->sign) {
				crossings++;
			}
			s->sign = sign;
		}
	}
	s->now_us += period_us;
	if (s->on && s->now_us >= s->until_us) {
		s->on = false;          /* the request ran out by itself */
	}
	if (peak > OA_SILENT) {
		s->run_us += period_us;
		s->run_crossings += crossings;
		s->quiet_us = 0;
		uint32_t intensity = peak * 1000u / 32768u;
		intensity = intensity == 0 ? 1 : intensity > 1000u ? 1000u : intensity;
		uint32_t hz = s->run_us >= 10000u ? (uint32_t)((uint64_t)s->run_crossings * 1000000u / (2u * s->run_us)) : 0u;
		uint32_t moved = intensity > s->level ? intensity - s->level : s->level - intensity;
		bool renew = s->on && s->until_us - s->now_us < OA_RENEW_MS * 1000u;
		if (!s->on || renew || moved > OA_LEVEL_STEP) {
			r.action = OA_VIB;
			r.ms = s->run_us > OA_TAP_MS * 1000u ? OA_SEGMENT_MS : OA_TAP_MS;
			r.intensity = intensity;
			r.hz = hz;
			s->on = true;
			s->until_us = s->now_us + r.ms * 1000u;
			s->level = intensity;
			s->hz = hz;
		}
	} else {
		s->quiet_us += period_us;
		if (s->quiet_us >= OA_QUIET_MS * 1000u) {
			if (s->on && s->run_us >= OA_LONG_MS * 1000u) {
				r.action = OA_STOP;
				s->on = false;
			}
			s->run_us = 0;
			s->run_crossings = 0;
		}
	}
	/* a session of hours: keep the clock away from the 32-bit wrap */
	if (!s->on && s->now_us > 0x40000000u) {
		s->now_us = 0;
		s->until_us = 0;
	}
	return r;
}

#endif
