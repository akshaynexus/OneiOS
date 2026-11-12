// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// The hosted kernel's speaker and microphone: oneios_audio_start, called once by oneios_drivers_start
// (drivers/bridge/OneiOSBridge.c; a weak default there for a kernel without this archive). It starts
// UniXNU-droidvm's virtio-snd transport (DroidVMVirtioSnd.c: xnuhost's /arm-io/virtio-snd, runtime/bridges/devices/
// virtio_snd.c), which publishes the two IOAudio2Devices of DroidVMAudio.cpp, says what attached, and starts a
// report thread: every 30 s while the counters move, one line of each side's counters (what an underrun, a late
// engine period or a refused selector looks like from inside the guest). oneios-audio-report=0 leaves it out.
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

static void oneios_audio_report(void *arg, wait_result_t result) {
    (void)arg; (void)result;
    uint64_t last = 0;
    uint32_t last_clients = 0;
    for (;;) {
        IOSleep(30000);
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
