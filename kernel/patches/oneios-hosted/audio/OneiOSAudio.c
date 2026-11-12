// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// The hosted kernel's speaker and microphone: oneios_audio_start, called once by oneios_drivers_start
// (drivers/bridge/OneiOSBridge.c; a weak default there for a kernel without this archive). It starts
// UniXNU-droidvm's virtio-snd transport (DroidVMVirtioSnd.c: xnuhost's /arm-io/virtio-snd, runtime/bridges/devices/
// virtio_snd.c), which publishes the two IOAudio2Devices of DroidVMAudio.cpp, says what attached, and starts a
// report thread: every 30 s while the counters move, one line of each side's counters (what an underrun, a late
// engine period or a refused selector looks like from inside the guest). oneios-audio-report=0 leaves it out.
#include <stdbool.h>
#include <stdint.h>
#include <kern/thread.h>
#include <pexpert/pexpert.h>

extern void IOSleep(unsigned milliseconds);
extern int printf(const char *, ...);
extern void droidvm_snd_start(void);
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
}
