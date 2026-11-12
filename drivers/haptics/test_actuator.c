// SPDX-License-Identifier: MIT
// Host test of oneios_actuator.h: the requests the Actuator device's samples make (drivers/tests/host.sh).
#include "oneios_actuator.h"
#include <math.h>
#include <stdio.h>
#include <string.h>

static int failures, checks;
#define CHECK(cond, ...) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: ", __FILE__, __LINE__); printf(__VA_ARGS__); printf("\n"); } } while (0)

#define PERIOD 240u   // the engine's period: 5 ms at 48 kHz

struct tally { unsigned vibs, stops, first_ms, last_intensity, last_hz; };

// Feeds `ms` of a sine of `hz` at `amplitude` (0: silence) in engine periods; counts the requests.
static void feed(struct oa_state *s, struct tally *t, unsigned ms, double hz, double amplitude, unsigned channels, double *phase)
{
	static int16_t buf[PERIOD * 2];
	for (unsigned done = 0; done < ms; done += 5) {
		for (unsigned i = 0; i < PERIOD; i++) {
			int16_t v = (int16_t)lrint(amplitude * sin(*phase));
			*phase += 2 * M_PI * hz / 48000.0;
			for (unsigned c = 0; c < channels; c++) buf[i * channels + c] = v;
		}
		struct oa_request r = oa_period(s, buf, PERIOD, channels);
		if (r.action == OA_VIB) {
			if (!t->vibs) t->first_ms = r.ms;
			t->vibs++;
			t->last_intensity = r.intensity;
			t->last_hz = r.hz;
		} else if (r.action == OA_STOP) {
			t->stops++;
		}
	}
}

int main(void)
{
	double phase = 0;

	// Silence asks for nothing.
	{
		struct oa_state s; memset(&s, 0, sizeof s);
		struct tally t = { 0 };
		feed(&s, &t, 2000, 0, 0, 1, &phase);
		CHECK(t.vibs == 0 && t.stops == 0, "silence: %u requests, %u stops", t.vibs, t.stops);
	}
	// A UI tap: 5 ms of a 230 Hz carrier at full scale -> one request of the tap length, no stop.
	{
		struct oa_state s; memset(&s, 0, sizeof s);
		struct tally t = { 0 };
		feed(&s, &t, 5, 230, 32000, 1, &phase);
		feed(&s, &t, 200, 0, 0, 1, &phase);
		CHECK(t.vibs == 1, "tap: %u requests", t.vibs);
		CHECK(t.first_ms == OA_TAP_MS, "tap: %u ms", t.first_ms);
		CHECK(t.stops == 0, "tap: %u stops (a tap runs out)", t.stops);
		CHECK(t.last_intensity > 900, "tap: intensity %u", t.last_intensity);
	}
	// A keyboard key: 10 ms at 0.10 of full scale (iOS's Keyboard-Key.wav) -> felt, intensity about 100.
	{
		struct oa_state s; memset(&s, 0, sizeof s);
		struct tally t = { 0 };
		feed(&s, &t, 10, 145, 3277, 1, &phase);
		feed(&s, &t, 100, 0, 0, 1, &phase);
		CHECK(t.vibs >= 1 && t.vibs <= 2, "key: %u requests", t.vibs);
		CHECK(t.last_intensity >= 80 && t.last_intensity <= 120, "key: intensity %u", t.last_intensity);
	}
	// A long vibration: 1 s of 160 Hz at half scale -> about 20 requests a second, the frequency found, one stop.
	{
		struct oa_state s; memset(&s, 0, sizeof s);
		struct tally t = { 0 };
		feed(&s, &t, 1000, 160, 16000, 1, &phase);
		unsigned during = t.vibs;
		feed(&s, &t, 100, 0, 0, 1, &phase);
		CHECK(during >= 15 && during <= 40, "long: %u requests in 1 s", during);
		CHECK(t.last_hz >= 150 && t.last_hz <= 170, "long: %u Hz", t.last_hz);
		CHECK(t.stops == 1, "long: %u stops", t.stops);
		CHECK(t.last_intensity >= 450 && t.last_intensity <= 520, "long: intensity %u", t.last_intensity);
	}
	// A level change of more than a step is sent at once; quiet below the threshold is silence.
	{
		struct oa_state s; memset(&s, 0, sizeof s);
		struct tally t = { 0 };
		feed(&s, &t, 100, 200, 6000, 1, &phase);
		unsigned before = t.vibs;
		feed(&s, &t, 5, 200, 30000, 1, &phase);
		CHECK(t.vibs == before + 1 && t.last_intensity > 850, "step: %u -> %u requests, intensity %u", before, t.vibs, t.last_intensity);
		struct oa_state q; memset(&q, 0, sizeof q);
		struct tally u = { 0 };
		feed(&q, &u, 500, 200, 200, 1, &phase);
		CHECK(u.vibs == 0, "below the threshold: %u requests", u.vibs);
	}
	// Two channels: the louder one counts; a pattern with 50 ms gaps restarts each pulse.
	{
		struct oa_state s; memset(&s, 0, sizeof s);
		struct tally t = { 0 };
		for (int i = 0; i < 4; i++) { feed(&s, &t, 100, 180, 20000, 2, &phase); feed(&s, &t, 50, 0, 0, 2, &phase); }
		CHECK(t.stops == 4, "pulses: %u stops", t.stops);
		CHECK(t.vibs >= 4 * 3, "pulses: %u requests", t.vibs);
	}
	// Hours of periods: the clock does not wrap into a stuck request.
	{
		struct oa_state s; memset(&s, 0, sizeof s);
		struct tally t = { 0 };
		feed(&s, &t, 1100000, 0, 0, 1, &phase);   // about 18 minutes of silence
		feed(&s, &t, 5, 230, 32000, 1, &phase);
		CHECK(t.vibs == 1, "after a long silence: %u requests", t.vibs);
	}
	printf("actuator: %d checks, %d failures\n", checks, failures);
	return failures ? 1 : 0;
}
