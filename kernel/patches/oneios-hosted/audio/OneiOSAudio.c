// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// The hosted kernel's speaker and microphone: oneios_audio_start, called once by oneios_drivers_start
// (drivers/bridge/OneiOSBridge.c; a weak default there for a kernel without this archive). It starts
// UniXNU-droidvm's virtio-snd transport (DroidVMVirtioSnd.c: xnuhost's /arm-io/virtio-snd, runtime/bridges/devices/
// virtio_snd.rs), which publishes the two IOAudio2Devices of DroidVMAudio.cpp, says what attached, and starts a
// report thread: every 30 s while the counters move, one line of each side's counters (what an underrun, a late
// engine period or a refused selector looks like from inside the guest), and while the speaker plays one line of its
// engine's timing (`speaker engine: ...`: how late it took its periods, how far ahead the HAL wrote: iOS's I/O buffer
// and safety offset as used), and one line of the engines' late periods by device and size (`late periods: ...`,
// fix-audio-late), and after iOS changed the volume where it settled (`speaker volume N of 128 (-X.Y dB applied ...)`,
// fix-audio-curve). oneios-audio-report=0 leaves it out.
//
// oneios-audio-selftest=SECONDS (1..10): 3 s after the start, a 1 kHz sine at -30 dBFS (quiet at any media volume)
// straight into the transport, before iOS's audio server runs: the guest kernel -> virtio-snd -> oneios-audio ->
// the phone's speaker path without iOS's audio stack. UniXNU-droidvm's droidvm-snd-selftest plays a full-scale
// sawtooth: not to be used with a speaker.
#include <stdbool.h>
#include <stdint.h>
#include <kern/thread.h>
#include <pexpert/pexpert.h>

extern void IOSleep(unsigned milliseconds);
extern int printf(const char *, ...);
extern void droidvm_snd_start(void);
extern uint32_t droidvm_snd_play(const int16_t *frames, uint32_t count);
extern void droidvm_snd_running(bool output, bool input);
extern uint64_t mach_absolute_time(void);
extern void nanoseconds_to_absolutetime(uint64_t nanoseconds, uint64_t *result);
extern void clock_delay_until(uint64_t deadline);
extern char droidvm_snd_status[64];
extern volatile uint32_t droidvm_snd_found, droidvm_snd_stage, droidvm_snd_streams;
extern volatile uint64_t droidvm_snd_tx_periods, droidvm_snd_tx_drops, droidvm_snd_rx_periods, droidvm_snd_rx_empty, droidvm_snd_errors;
extern volatile uint32_t droidvm_audio_stage, droidvm_audio_out_running, droidvm_audio_in_running, droidvm_audio_clients;
extern volatile uint64_t droidvm_audio_periods, droidvm_audio_late, droidvm_audio_played, droidvm_audio_captured;
extern volatile uint64_t droidvm_audio_last_selector, droidvm_audio_selectors, droidvm_audio_bad_selectors, droidvm_audio_maps;
extern volatile int droidvm_audio_family_status, droidvm_audio_hog_owner;
// The speaker engine's timing (DroidVMAudio.cpp as staged, fix-audio-latency).
extern uint32_t oneios_speaker_ring;
extern volatile uint64_t oneios_speaker_periods, oneios_speaker_late_sum, oneios_speaker_late_max, oneios_speaker_leads,
    oneios_speaker_lead_sum, oneios_speaker_lead_min, oneios_speaker_lead_max, oneios_speaker_lead_last, oneios_speaker_tight;
// The engines' lateness by device and size, and their realtime policy (DroidVMAudio.cpp as staged, fix-audio-late).
extern uint32_t oneios_audio_rt;
extern volatile uint32_t oneios_audio_rt_set, oneios_audio_rt_refused;
extern volatile uint64_t oneios_speaker_late_1ms, oneios_speaker_late_5ms, oneios_speaker_late_20ms, oneios_speaker_short,
    oneios_actuator_late;

// The speaker's volume as iOS last set it and the curve's applied dB (DroidVMAudio.cpp as staged, fix-audio-curve).
extern volatile uint32_t oneios_speaker_volume, oneios_speaker_gain, oneios_speaker_volume_sets, oneios_speaker_db10;
extern uint32_t oneios_audio_curve;

// fix-audio-curve: when iOS changed the volume since the last report, where it settled (the change lines of
// DroidVMAudio.cpp stop after the 16th, and a slider's drag is many changes): the step, the dB applied, the curve.
static void oneios_audio_volume_report(void) {
    static uint32_t sets;
    const uint32_t n = oneios_speaker_volume_sets;
    if (n == sets) return;
    const uint32_t db10 = oneios_speaker_db10;
    const uint32_t v = oneios_speaker_volume;
    printf("ONEIOS AUDIO: speaker volume %u of 128 (%s%s%u.%u dB applied, gain %u of 65536) after %u changes (%u since the last "
           "report); curve %s\n", v, v ? "" : "silent, ", db10 ? "-" : "", db10 / 10, db10 % 10, oneios_speaker_gain, n, n - sets,
           oneios_audio_curve ? "sheng -36 dB squared (oneios-audio-curve=1)" : "linear -64 dB (oneios-audio-curve=0)");
    sets = n;
}

// Since the last report, the speaker's engine: how late it took its periods, and how far ahead of each the HAL had
// written (its write end: iOS's I/O buffer and the safety offset as used, so the guest's part of the latency from
// iOS's mix); a lead within a period of the ring's end is "tight" (the ring too small for the I/O buffer iOS took).
static void oneios_audio_speaker_report(void) {
    static uint64_t periods, late_sum, leads, lead_sum, tight;
    const uint64_t p = oneios_speaker_periods - periods, l = oneios_speaker_leads - leads;
    if (!p) return;
    const uint64_t late = (oneios_speaker_late_sum - late_sum) / p, lead = l ? (oneios_speaker_lead_sum - lead_sum) / l : 0;
    if (l)
        printf("ONEIOS AUDIO: speaker engine: %llu periods, taken late by %llu us on average (max %llu); the HAL wrote %llu..%llu frames "
               "ahead (%llu on average, %llu ms: iOS's I/O buffer and safety offset), %llu periods tight (ring %u frames)\n",
               p, late, oneios_speaker_late_max, oneios_speaker_lead_min, oneios_speaker_lead_max, lead, lead / 48,
               oneios_speaker_tight - tight, oneios_speaker_ring);
    else
        printf("ONEIOS AUDIO: speaker engine: %llu periods, taken late by %llu us on average (max %llu); the HAL's write end not ahead "
               "(last %llu; ring %u frames)\n", p, late, oneios_speaker_late_max, oneios_speaker_lead_last, oneios_speaker_ring);
    periods = oneios_speaker_periods; late_sum = oneios_speaker_late_sum; leads = oneios_speaker_leads;
    lead_sum = oneios_speaker_lead_sum; tight = oneios_speaker_tight;
    oneios_speaker_late_max = 0; oneios_speaker_lead_max = 0; oneios_speaker_lead_min = ~0ULL;
}

static void oneios_audio_report(void *arg, wait_result_t result) {
    (void)arg; (void)result;
    uint64_t last = 0;
    uint32_t last_clients = 0;
    for (;;) {
        IOSleep(30000);
        oneios_audio_speaker_report();
        oneios_audio_volume_report();
        const uint64_t moved = droidvm_audio_periods + droidvm_audio_selectors + droidvm_snd_errors;
        if (moved == last && droidvm_audio_clients == last_clients) continue;
        last = moved;
        last_clients = droidvm_audio_clients;
        printf("ONEIOS AUDIO: speaker %s, mic %s, %u clients (hog %d), %llu selectors (%llu refused, last %llu), %llu maps; "
               "engine %llu periods, %llu late, %llu frames played, %llu captured; virtio-snd %llu tx periods, %llu dropped "
               "(host behind), %llu rx periods, %llu rx empty, %llu errors\n",
               droidvm_audio_out_running ? "running" : "idle", droidvm_audio_in_running ? "running" : "idle", droidvm_audio_clients,
               droidvm_audio_hog_owner, droidvm_audio_selectors, droidvm_audio_bad_selectors, droidvm_audio_last_selector,
               droidvm_audio_maps, droidvm_audio_periods, droidvm_audio_late, droidvm_audio_played, droidvm_audio_captured,
               droidvm_snd_tx_periods, droidvm_snd_tx_drops, droidvm_snd_rx_periods, droidvm_snd_rx_empty, droidvm_snd_errors);
        // fix-audio-late: since boot, the speaker's periods by how late its engine took them (the line above counts
        // only those over a period, the speaker's and the mic's), those the HAL had written only in part, the
        // Actuator's over a period (counted apart since 2026-11-24), and the engines' realtime policy (oneios-audio-rt).
        printf("ONEIOS AUDIO: late periods: speaker %llu over 1 ms, %llu over 5 ms, %llu over 20 ms of %llu, %llu written by the HAL "
               "only in part; Actuator %llu over 5 ms; engine threads realtime %u, refused %u (oneios-audio-rt=%u)\n",
               oneios_speaker_late_1ms, oneios_speaker_late_5ms, oneios_speaker_late_20ms, oneios_speaker_periods, oneios_speaker_short,
               oneios_actuator_late, oneios_audio_rt_set, oneios_audio_rt_refused, oneios_audio_rt);
    }
}

// One period of 1 kHz at 48 kHz, amplitude 1000 (-30 dBFS).
static const int16_t oneios_sine[48] = {
    0, 131, 259, 383, 500, 609, 707, 793, 866, 924, 966, 991, 1000, 991, 966, 924, 866, 793, 707, 609, 500, 383, 259, 131,
    0, -131, -259, -383, -500, -609, -707, -793, -866, -924, -966, -991, -1000, -991, -966, -924, -866, -793, -707, -609,
    -500, -383, -259, -131};

static void oneios_audio_selftest(void *arg, wait_result_t result) {
    (void)result;
    const uint32_t seconds = (uint32_t)(uintptr_t)arg, periods = seconds * 200;
    static int16_t period[240 * 2];
    IOSleep(3000);
    droidvm_snd_running(true, false);
    uint32_t frame = 0, sent = 0, refused = 0;
    uint64_t step, deadline = mach_absolute_time();
    nanoseconds_to_absolutetime(5000000, &step);            // 240 frames at 48 kHz
    for (uint32_t p = 0; p < periods; ++p) {
        for (uint32_t i = 0; i < 240; ++i, ++frame) period[2 * i] = period[2 * i + 1] = oneios_sine[frame % 48];
        if (droidvm_snd_play(period, 240) == 240) sent++;
        else refused++;
        deadline += step;           // on the guest's clock, as DroidVMAudio's engine paces its periods
        clock_delay_until(deadline);
    }
    IOSleep(100);
    droidvm_snd_running(false, false);
    printf("ONEIOS AUDIO: self-test: %u of %u periods of 1 kHz at -30 dBFS sent (%u refused: no free slot)\n", sent, periods, refused);
}

void oneios_audio_start(void) {
    droidvm_snd_start();
    const bool up = droidvm_snd_stage == 3;
    printf("ONEIOS AUDIO: %s: virtio-snd %s (found %u, stage %u, %u streams); IOAudio2 devices: %s (stage %u, family %d)\n",
           up && droidvm_audio_stage == 4 ? "on" : "off", droidvm_snd_status[0] ? droidvm_snd_status : "not found",
           droidvm_snd_found, droidvm_snd_stage, droidvm_snd_streams,
           droidvm_audio_stage == 4 ? "Speaker and Mic published" : droidvm_audio_stage == 3 ? "none published" :
           droidvm_audio_stage ? "IOAudio2Family's classes not linked" : "not attached (droidvm-audio=0?)",
           droidvm_audio_stage, droidvm_audio_family_status);
    uint32_t report = 1;
    PE_parse_boot_argn("oneios-audio-report", &report, sizeof(report));
    thread_t thread;
    if (report && kernel_thread_start(oneios_audio_report, NULL, &thread) == KERN_SUCCESS) thread_deallocate(thread);
    uint32_t selftest = 0;
    if (up && PE_parse_boot_argn("oneios-audio-selftest", &selftest, sizeof(selftest)) && selftest) {
        if (selftest > 10) selftest = 10;
        if (kernel_thread_start(oneios_audio_selftest, (void *)(uintptr_t)selftest, &thread) == KERN_SUCCESS) thread_deallocate(thread);
    }
}
