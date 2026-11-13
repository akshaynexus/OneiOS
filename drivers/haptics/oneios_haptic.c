/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS haptics, the kernel's half below the "vibrator" IOService (OneiOSVibrator.cpp): requests go
 * to the host bridge (runtime/bridges/haptics), which plays them on the Galaxy S24 Ultra's actuator.
 *
 * Ported from iOSPlus10 (patches/laurel/xnu/2700-laurel-pmi632-haptics.patch, bsd/dev/laurel_haptic.c,
 * the Mi A3's PMI632 motor). Kept: the two iOS paths, the stream analysis of the Actuator samples,
 * the closed-loop self-check and its rule (never publish closed-loop without an Actuator route), the
 * log lines' shape. Replaced: the SPMI lane, the LDO, the motor thread with its overdrive, minimum
 * on-time, duty limit and watchdog. Those protected an ERM on a bare LDO; the S24 Ultra has an LRA on
 * a Cirrus CS40L26 whose own DSP does the braking and protection, and the host bridge stops the
 * actuator when the guest resets or xnuhost exits. New: amplitude and frequency (the Mi A3's motor
 * was on/off only).
 *
 * iOS paths (docs/hardware.md, Vibration and haptics):
 *   1. "vibrator" IOService: FigVibrator's legacy IOKit path (ringtone, alert and system-sound
 *      vibration). API oneios_haptic_vib(ms, intensity, hz): on for ms from now (0: stop).
 *   2. The "Actuator" audio device: the closed-loop path (Core Haptics, UIFeedbackGenerator,
 *      keyboard taps; with closed-loop on also ringtone and alert patterns, through VirtualAudio's
 *      VirtualAudioDevice_Actuator). Two ways in, both S16LE at 48 kHz:
 *      a. the Actuator IOAudio2Device of the audio archive (kernel/patches/oneios-hosted/audio,
 *         DroidVMAudio.cpp as staged): its engine hands every 5 ms period to
 *         oneios_haptic_actuator_play(), which oneios_actuator.h turns into few requests;
 *      b. /dev/oneios-haptic and /dev/redwood-haptic, for an Actuator HAL plug-in in the root
 *         (ios-topaz's RedwoodAudio): 2 ms blocks with a peak above OH_SILENT are "on"; each write's
 *         loud span is played at once with the span's peak as the intensity and its zero-crossing
 *         rate as the frequency.
 *      It needs IODeviceTree:/product/haptics closed-loop = 1, which is published only when one of
 *      the two exists (and VirtualAudio.plugin): with closed-loop on and no Actuator device iOS plays
 *      no vibration at all.
 *
 * Log: "ONEIOS HAPTIC: on: ..." or "ONEIOS HAPTIC: off: <why>"; "ONEIOS HAPTIC UI: on|off: <why>"; the
 * Actuator's user clients ("Actuator: user client N opened by audiomxd", formats asked, selectors refused,
 * mappings: the first 24 events); "ONEIOS HAPTIC: counters: ..." every 30 s while a figure moved, one line from the
 * Actuator's user clients through its samples to the bridge, so a log shows the layer where a tap stops (xnuhost's
 * "haptics: counters:" line is the host's side: what came over the bridge and what the device played).
 * Kill switches: oneios_haptic=0 (all), oneios_haptic_ui=0 (no closed-loop node, no Actuator device);
 * the audio archive's oneios-actuator=0 (no Actuator device); oneios_haptic_report=0 (no counter line).
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/conf.h>
#include <sys/errno.h>
#include <sys/uio.h>
#include <sys/proc.h>
#include <sys/vnode.h>
#include <sys/vnode_internal.h>
#include <miscfs/devfs/devfs.h>
#include <kern/clock.h>
#include <kern/locks.h>
#include <kern/thread.h>
#include <kern/sched_prim.h>
#include <pexpert/pexpert.h>
#include "oneios_bridge.h"
#include "oneios_actuator.h"

extern kern_return_t kernel_thread_start(thread_continue_t, void *, thread_t *);
extern kern_return_t thread_terminate(thread_t);
extern void proc_selfname(char *buf, int size);

/* the Actuator stream */
#define OH_RATE         48000u
#define OH_BLOCK        (OH_RATE / 1000u * 2u)  /* 96 frames = 2 ms */
#define OH_SILENT       256u            /* of 32768 */
#define OH_HOLD_MS      12u             /* a run that reaches the end of a write is held this long for the next write */
#define OH_LOG          24u

static LCK_GRP_DECLARE(oh_grp, "oneios_haptic");
static LCK_MTX_DECLARE(oh_wlock, &oh_grp);      /* the stream state */

static bool oh_ready;                           /* the host serves the haptic channel */
static uint32_t oh_ui = 1;
static uint32_t oh_part, oh_part_peak;          /* the open 2 ms block */
static int16_t oh_last;                         /* the previous sample, for zero crossings */
static uint64_t oh_frames, oh_loud_blocks;
static uint32_t oh_said, oh_vibs;
static bool oh_node_made, oh_opened, oh_wrote;

/* The counter line's figures for the bridge (every layer has its own: see oh_report_thread). */
static volatile uint64_t oh_sent_vib, oh_sent_stop, oh_send_failed, oh_send_no_host;

int
oneios_haptic_vib(uint32_t ms, uint32_t intensity, uint32_t frequency_hz)
{
	if (!oh_ready) {
		oh_send_no_host++;
		return -1;
	}
	if (ms == 0) {
		int stop = oneios_bridge_send(OB_CH_HAPTIC, OB_HAPTIC_STOP, NULL, 0);
		if (stop == 0) {
			oh_sent_stop++;
		} else {
			oh_send_failed++;
		}
		return stop;
	}
	struct ob_haptic_vib vib = {
		.on_ms = ms,
		.intensity = (uint16_t)(intensity > 1000u ? 1000u : intensity),
		.frequency_hz = (uint16_t)(frequency_hz > 1000u ? 1000u : frequency_hz),
	};
	int rc = oneios_bridge_send(OB_CH_HAPTIC, OB_HAPTIC_VIB, &vib, sizeof(vib));
	if (rc == 0) {
		oh_sent_vib++;
	} else {
		oh_send_failed++;
	}
	if (rc == 0 && oh_vibs++ < OH_LOG) {
		printf("ONEIOS HAPTIC: play %u: %u ms, intensity %u of 1000, %u Hz%s\n", oh_vibs, ms, vib.intensity,
		    vib.frequency_hz, vib.frequency_hz ? "" : " (resonance)");
	}
	return rc;
}

/* ---- the Actuator IOAudio2Device (audio archive) --------------------------------------------------- */

static LCK_MTX_DECLARE(oh_alock, &oh_grp);      /* the Actuator's state */
static struct oa_state oh_act;
static uint32_t oh_act_decided;                 /* 0: not yet, 1: no Actuator device, 2: published */
static uint64_t oh_act_periods, oh_act_loud, oh_act_requests;
static uint32_t oh_act_starts, oh_act_said;
/* What the device's user clients did (oneios_haptic_actuator_event, from DroidVMAudio.cpp as staged) and what its
 * samples held: the counter line (oh_report_thread) shows where a tap stops. */
static volatile uint32_t oh_act_opens, oh_act_closes, oh_act_stops, oh_act_refused, oh_act_maps, oh_act_formats;
static volatile uint64_t oh_act_selectors, oh_act_sounding, oh_act_frames;
static volatile uint32_t oh_act_peak, oh_act_last_selector = UINT32_MAX;
static char oh_act_opener[32];
static uint32_t oh_act_event_said;

/* The audio archive asks before it publishes the device: only with the host's actuator and UI haptics on. */
bool
oneios_haptic_actuator_ready(void)
{
	return oh_ready && oh_ui != 0;
}

/* The audio archive's decision, once: whether an Actuator IOAudio2Device was published. */
void
oneios_haptic_actuator_published(bool yes)
{
	oh_act_decided = yes ? 2u : 1u;
	printf("ONEIOS HAPTIC: Actuator audio device %s\n", yes ? "published (UID \"Actuator\", mono 48 kHz S16)" :
	    "not published");
}

/* The device's StartIO and StopIO (audiomxd's I/O on VirtualAudioDevice_Actuator). */
void
oneios_haptic_actuator_running(bool on)
{
	lck_mtx_lock(&oh_alock);
	if (!on && oh_act.on) {
		(void)oneios_haptic_vib(0, 0, 0);
	}
	uint32_t starts = on ? ++oh_act_starts : oh_act_starts;
	if (!on) {
		oh_act_stops++;
	}
	bzero(&oh_act, sizeof(oh_act));
	lck_mtx_unlock(&oh_alock);
	if (starts <= 3u || !on) {
		char who[32];
		proc_selfname(who, sizeof(who));
		if (on) {
			printf("ONEIOS HAPTIC: Actuator: I/O started by %s (start %u)\n", who, starts);
		} else if (oh_act_said < OH_LOG) {
			oh_act_said++;
			printf("ONEIOS HAPTIC: Actuator: I/O stopped: %llu periods, %llu loud, %llu requests to the motor so far\n",
			    oh_act_periods, oh_act_loud, oh_act_requests);
		}
	}
}

/* One engine period (240 frames, 5 ms) of the Actuator's samples; returns the frames taken. */
uint32_t
oneios_haptic_actuator_play(const int16_t *frames, uint32_t count, uint32_t channels)
{
	/* any sample at all, and the loudest: "samples reach the ring" even when they stay under OA_SILENT */
	uint32_t peak = 0;
	for (uint32_t i = 0; i < count * (channels ? channels : 1u); i++) {
		uint32_t m = frames[i] < 0 ? (uint32_t)(-(int32_t)frames[i]) : (uint32_t)frames[i];
		peak = m > peak ? m : peak;
	}
	lck_mtx_lock(&oh_alock);
	struct oa_request r = oa_period(&oh_act, frames, count, channels);
	oh_act_periods++;
	oh_act_frames += count;
	if (peak != 0) {
		oh_act_sounding++;
		oh_act_peak = peak > oh_act_peak ? peak : oh_act_peak;
	}
	if (oh_act.run_us > 0 && oh_act.quiet_us == 0) {
		oh_act_loud++;
	}
	if (r.action != OA_NONE) {
		oh_act_requests++;
	}
	lck_mtx_unlock(&oh_alock);
	if (r.action == OA_VIB) {
		(void)oneios_haptic_vib(r.ms, r.intensity, r.hz);
	} else if (r.action == OA_STOP) {
		(void)oneios_haptic_vib(0, 0, 0);
	}
	return count;
}

/* What a user client of the Actuator device did (DroidVMAudio.cpp as staged; oneios_bridge.h has the kinds). The
 * first OH_LOG events are logged with the caller's name; all of them are counted for the counter line. */
void
oneios_haptic_actuator_event(uint32_t kind, uint32_t a, uint32_t b)
{
	char who[32];
	proc_selfname(who, sizeof(who));
	switch (kind) {
	case OH_EV_OPEN:
		oh_act_opens++;
		strlcpy(oh_act_opener, who, sizeof(oh_act_opener));
		break;
	case OH_EV_CLOSE:
		oh_act_closes++;
		break;
	case OH_EV_SELECTOR:
		oh_act_selectors++;
		oh_act_last_selector = a;
		break;
	case OH_EV_REFUSED:
		oh_act_refused++;
		break;
	case OH_EV_FORMAT:
		oh_act_formats++;
		break;
	case OH_EV_MAP:
		oh_act_maps++;
		break;
	default:
		return;
	}
	if (kind == OH_EV_SELECTOR || oh_act_event_said >= OH_LOG) {
		return;                 /* a selector shows in the counter line; StartIO and StopIO have their own lines */
	}
	oh_act_event_said++;
	if (kind == OH_EV_OPEN) {
		printf("ONEIOS HAPTIC: Actuator: user client %u opened by %s\n", oh_act_opens, who);
	} else if (kind == OH_EV_CLOSE) {
		printf("ONEIOS HAPTIC: Actuator: user client closed by %s (%u of %u)\n", who, oh_act_closes, oh_act_opens);
	} else if (kind == OH_EV_REFUSED) {
		printf("ONEIOS HAPTIC: Actuator: selector %u refused (by %s)\n", a, who);
	} else if (kind == OH_EV_FORMAT) {
		printf("ONEIOS HAPTIC: Actuator: %s asks for %u channels at %u Hz (the device: %u at 48000 Hz: %s)\n", who, a & 0xffffu, b,
		    a >> 16, (a & 0xffffu) == a >> 16 && b == 48000u ? "the same" : "differs, refused; oneios-actuator-channels=2 if it asks for 2");
	} else {
		printf("ONEIOS HAPTIC: Actuator: %s maps memory type %#x\n", who, a);
	}
}

/* Every OH_REPORT_S seconds while a figure moved: one line from the Actuator's user clients to the bridge, so a log
 * shows the layer where a tap or a vibration stops. The host's side (xnuhost's "haptics:" lines) counts what came
 * over the bridge and what the force-feedback device played. oneios_haptic_report=0 leaves it out. */
#define OH_REPORT_S 30u
extern volatile uint32_t oneios_vibrator_requests;
static void
oh_report_thread(__unused void *arg, __unused wait_result_t wr)
{
	uint64_t last = UINT64_MAX;
	for (;;) {
		delay_for_interval(OH_REPORT_S, NSEC_PER_SEC);
		uint64_t now = oh_act_opens + oh_act_closes + oh_act_selectors + oh_act_maps + oh_act_periods + oh_sent_vib + oh_sent_stop +
		    oh_send_failed + oh_send_no_host + oneios_vibrator_requests + oh_frames;
		if (now == last) {
			continue;
		}
		last = now;
		printf("ONEIOS HAPTIC: counters: Actuator %u opens (%u closed, last by %s), %llu selectors (%u StartIO, %u StopIO, "
		    "%u refused, %u formats asked, last %d), %u maps; %llu periods (%llu ms), %llu with samples, %llu loud, peak %u of 32768; "
		    "%llu requests to the motor | vibrator service %u requests | /dev/oneios-haptic %llu frames | bridge %llu vib + %llu stop sent, "
		    "%llu failed, %llu with no host actuator\n",
		    oh_act_opens, oh_act_closes, oh_act_opener[0] ? oh_act_opener : "-", oh_act_selectors, oh_act_starts, oh_act_stops,
		    oh_act_refused, oh_act_formats, (int)oh_act_last_selector, oh_act_maps, oh_act_periods, oh_act_frames / 48u,
		    oh_act_sounding, oh_act_loud, oh_act_peak, oh_act_requests, oneios_vibrator_requests, oh_frames, oh_sent_vib,
		    oh_sent_stop, oh_send_failed, oh_send_no_host);
	}
}

/* ---- /dev/oneios-haptic -------------------------------------------------------------------------- */

static int
oh_dev_write(__unused dev_t dev, struct uio *uio, __unused int ioflag)
{
	int16_t s[256];
	int error = 0;
	uint32_t blocks = 0, first_loud = UINT32_MAX, last_loud = 0, peak = 0, crossings = 0, loud = 0;

	lck_mtx_lock(&oh_wlock);
	if (!oh_wrote) {
		char who[32];
		oh_wrote = true;
		proc_selfname(who, sizeof(who));
		printf("ONEIOS HAPTIC: /dev/oneios-haptic: first write, by %s\n", who);
	}
	while (uio_resid(uio) >= 2) {
		user_ssize_t n = uio_resid(uio);
		n = n > (user_ssize_t)sizeof(s) ? (user_ssize_t)sizeof(s) : n;
		n &= ~(user_ssize_t)1;
		if ((error = uiomove((caddr_t)s, (int)n, uio)) != 0) {
			break;
		}
		for (unsigned i = 0; i < (unsigned)n / 2u; i++) {
			uint32_t v = s[i] < 0 ? (uint32_t)(-(int32_t)s[i]) : (uint32_t)s[i];
			oh_part_peak = v > oh_part_peak ? v : oh_part_peak;
			if ((s[i] < 0) != (oh_last < 0) && v > OH_SILENT) {
				crossings++;
			}
			oh_last = s[i];
			if (++oh_part < OH_BLOCK) {
				continue;
			}
			if (oh_part_peak > OH_SILENT) {
				oh_loud_blocks++;
				loud++;
				peak = oh_part_peak > peak ? oh_part_peak : peak;
				first_loud = first_loud == UINT32_MAX ? blocks : first_loud;
				last_loud = blocks;
			}
			blocks++;
			oh_part = 0;
			oh_part_peak = 0;
		}
		oh_frames += (uint64_t)n / 2u;
	}
	if (loud > 0) {
		/* The loud span of this write, played now. A span that reaches the write's last block continues
		 * in the next write: it is held a little longer so the two join without a gap. */
		uint32_t ms = (last_loud - first_loud + 1u) * 2u + (last_loud + 1u == blocks ? OH_HOLD_MS : 0u);
		uint32_t intensity = peak * 1000u / 32768u;
		/* two zero crossings per period; only a span long enough to hold a few periods says a frequency */
		uint32_t hz = loud >= 5u ? crossings * 1000u / (loud * 2u * 2u) : 0u;
		(void)oneios_haptic_vib(ms, intensity ? intensity : 1u, hz);
		if (oh_said < OH_LOG) {
			oh_said++;
			printf("ONEIOS HAPTIC: /dev/oneios-haptic: iOS plays (peak %u of 32768; %llu frames, %llu loud 2 ms blocks so far)%s\n",
			    peak, oh_frames, oh_loud_blocks, oh_ready ? "" : " (no host actuator: not felt)");
		}
	}
	lck_mtx_unlock(&oh_wlock);
	return error;
}

static int
oh_dev_open(__unused dev_t dev, __unused int flags, __unused int type, __unused struct proc *p)
{
	if (!oh_opened) {
		char who[32];
		oh_opened = true;
		proc_selfname(who, sizeof(who));
		printf("ONEIOS HAPTIC: /dev/oneios-haptic opened by %s\n", who);
	}
	return 0;
}

static int
oh_dev_close(__unused dev_t dev, __unused int flags, __unused int type, __unused struct proc *p)
{
	return 0;
}

static const struct cdevsw oh_cdevsw = {
	.d_open = oh_dev_open,
	.d_close = oh_dev_close,
	.d_read = eno_rdwrt,
	.d_write = oh_dev_write,
	.d_ioctl = eno_ioctl,
	.d_stop = eno_stop,
	.d_reset = eno_reset,
	.d_ttys = NULL,
	.d_select = eno_select,
	.d_mmap = eno_mmap,
	.d_strategy = eno_strat,
	.d_reserved_1 = eno_getc,
	.d_reserved_2 = eno_putc,
	.d_type = 0,
};

/* The nodes, once. An audio plug-in opens its node a single time at its first haptic, so the node must
 * exist before the audio server starts (iOSPlus10 docs/laurel_sprout/haptics.md, "The node's timing"):
 * devfs must be up (after vfsinit); the caller that comes first makes them. */
void
oneios_haptic_make_node(void)
{
	lck_mtx_lock(&oh_wlock);
	if (!oh_node_made) {
		int major = cdevsw_add(-1, &oh_cdevsw);
		void *node = major < 0 ? NULL :
		    devfs_make_node(makedev(major, 0), DEVFS_CHAR, UID_ROOT, GID_WHEEL, 0666, "oneios-haptic");
		void *alias = major < 0 ? NULL :
		    devfs_make_node(makedev(major, 0), DEVFS_CHAR, UID_ROOT, GID_WHEEL, 0666, "redwood-haptic");
		oh_node_made = node != NULL;
		printf("ONEIOS HAPTIC: /dev/oneios-haptic %s%s (S16LE mono %u Hz, 2 ms blocks, >%u of 32768 = on)\n",
		    node ? "ready" : "not made (devfs)", alias ? ", also /dev/redwood-haptic" : "", OH_RATE, OH_SILENT);
	}
	lck_mtx_unlock(&oh_wlock);
}

static bool
oh_exists(const char *path)
{
	vnode_t vp = NULLVP;
	if (vnode_lookup(path, 0, &vp, vfs_context_kernel()) != 0) {
		return false;
	}
	vnode_put(vp);
	return true;
}

/* Where an Actuator HAL plug-in may be: the system volume (a root built with it) or the overlay. */
static const char *const oh_plugins[] = {
	"/System/Library/Audio/Plug-Ins/HAL/RedwoodAudio.driver",
	"/Library/Audio/Plug-Ins/HAL/OneiOSAudio.driver",
	"/private/var/oneios/Audio/HAL/OneiOSAudio.driver",
};
#define OH_VIRTUAL  "/Library/Audio/Plug-Ins/HAL/VirtualAudio.plugin"
#define OH_VIRTUAL2 "/System/Library/Audio/Plug-Ins/HAL/VirtualAudio.plugin"

/* after the root mount: the node, then the closed-loop self-check */
static void
oh_root_thread(__unused void *arg, __unused wait_result_t wr)
{
	unsigned waited = 0;
	while (rootvnode == NULLVP && waited < 600u * 1000u) {
		delay_for_interval(20, NSEC_PER_MSEC);
		waited += 20;
	}
	oneios_haptic_make_node();
	/* The audio archive decides about its Actuator device at the drivers' start, normally before the
	 * root is mounted; without the archive (a kernel built without it) no decision comes. */
	for (unsigned w = 0; oh_act_decided == 0 && w < 5000u; w += 20u) {
		delay_for_interval(20, NSEC_PER_MSEC);
	}
	const char *plugin = oh_act_decided == 2u ? "the Actuator IOAudio2Device" : NULL;
	for (unsigned i = 0; plugin == NULL && rootvnode != NULLVP && i < sizeof(oh_plugins) / sizeof(oh_plugins[0]); i++) {
		if (oh_exists(oh_plugins[i])) {
			plugin = oh_plugins[i];
			break;
		}
	}
	if (rootvnode == NULLVP) {
		printf("ONEIOS HAPTIC UI: off: no root file system after %u s\n", waited / 1000u);
	} else if (oh_ui == 0) {
		printf("ONEIOS HAPTIC UI: off: oneios_haptic_ui=0\n");
	} else if (!oh_node_made) {
		printf("ONEIOS HAPTIC UI: off: no /dev/oneios-haptic\n");
	} else if (plugin == NULL) {
		printf("ONEIOS HAPTIC UI: off: no Actuator audio device (%s) and no Actuator HAL plug-in in the root (%s, ...): "
		    "iOS vibrates through the \"vibrator\" service (ringtones, alerts; no UI/keyboard taps)\n",
		    oh_act_decided == 1u ? "not published" : "no decision from the audio archive in 5 s", oh_plugins[0]);
	} else if (!oh_exists(OH_VIRTUAL) && !oh_exists(OH_VIRTUAL2)) {
		printf("ONEIOS HAPTIC UI: off: no VirtualAudio.plugin: no Actuator route; \"vibrator\" service only\n");
	} else if (oneios_haptic_publish_closed_loop() != 0) {
		printf("ONEIOS HAPTIC UI: off: IODeviceTree:/product/haptics not published\n");
	} else {
		printf("ONEIOS HAPTIC UI: on: /product/haptics closed-loop = 1 (Core Haptics, UI and keyboard taps, "
		    "ringtones -> VirtualAudioDevice_Actuator -> %s -> the host's actuator)\n", plugin);
	}
	(void)thread_terminate(current_thread());
}

/* ---- start (the board's late probe, through oneios_drivers_start) --------------------------------- */

int
oneios_haptic_start(void)
{
	static bool tried;
	uint32_t on = 1;
	thread_t th;

	if (tried) {
		return oh_ready ? 0 : -1;
	}
	tried = true;
	(void)PE_parse_boot_argn("oneios_haptic", &on, sizeof(on));
	(void)PE_parse_boot_argn("oneios_haptic_ui", &oh_ui, sizeof(oh_ui));
	if (on == 0) {
		printf("ONEIOS HAPTIC: off: oneios_haptic=0\n");
		return -1;
	}
	/* the counter line, also without a host actuator (then the vibrator's requests and "no host actuator" move) */
	uint32_t report = 1;
	(void)PE_parse_boot_argn("oneios_haptic_report", &report, sizeof(report));
	if (report != 0 && kernel_thread_start(oh_report_thread, NULL, &th) == KERN_SUCCESS) {
		thread_deallocate(th);
	}
	/* The host's hello says whether it has an actuator; it comes within a few milliseconds of the start. */
	for (unsigned waited = 0; waited < 2000u && !oneios_bridge_ready(); waited += 10u) {
		delay_for_interval(10, NSEC_PER_MSEC);
	}
	if (!oneios_bridge_ready()) {
		printf("ONEIOS HAPTIC: off: no bridge device (or the host did not answer)\n");
		return -1;
	}
	if (!(oneios_bridge_channels() & (1u << OB_CH_HAPTIC))) {
		printf("ONEIOS HAPTIC: off: the host has no actuator (no force-feedback device opened)\n");
		return -1;
	}
	oh_ready = true;
	printf("ONEIOS HAPTIC: on: host actuator over the bridge channel (amplitude 0..1000, frequency in Hz)\n");
	if (kernel_thread_start(oh_root_thread, NULL, &th) == KERN_SUCCESS) {
		thread_deallocate(th);
	} else {
		printf("ONEIOS HAPTIC UI: off: no root-wait thread (no /dev/oneios-haptic)\n");
	}
	return 0;
}
