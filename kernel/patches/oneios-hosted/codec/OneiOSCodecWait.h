// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// OneiOS's wait policy of the staged virtio-media transport (DroidVMVirtioMedia.c with edit 7 of the codec archive,
// tools/oneios-tool/crates/oneios-kernel/src/staged/codec_transport.rs) and OneiOSCodec.c's "transport:" line, in plain C
// so that the host test (tests/test_wait.c, run by kernel/tests/host.sh) runs the code the kernel runs, on both paths:
//   oneios_media_polled_why  the interrupt or the poll, and why the poll: the switch (oneios-codec-irq, default 1), the
//                            window's INTID in the device tree, ArmCpu's attach of it
//   oneios_media_wait_ms     the event worker's longest sleep. Polled: 1 ms while a session streams, 50 otherwise (the
//                            pinned f9121cd's poll, unchanged). With the interrupt it is a guard only: 25 ms while a
//                            session streams, 1000 otherwise, so a lost interrupt delays an event and never hangs a session
//   oneios_media_count_wake  one wake of the event worker: by the interrupt or by the timer, and a timer wake that found
//                            events (polled: a poll that found work; with the interrupt: a late or lost interrupt)
//   oneios_media_line        the line: the counts since the last one
// The interrupt-driven event worker follows Droid-VM/UniXNU-droidvm 96e8c1f ("virtio-media: completion interrupts instead
// of polling", lateautumn233) and 2cd4c8e (either ADT name for the window's interrupt, polling where there is none, HuJK),
// both BSD-2-Clause-Patent. No Apple file, no file of another project.
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#ifndef KERNEL
#include <stdio.h>
#endif

enum {
    ONEIOS_MEDIA_POLL_FAST_MS = 1,     // polled, a session streaming (f9121cd's IOSleep(1))
    ONEIOS_MEDIA_POLL_MS = 50,         // polled, no session streaming (f9121cd's IOSleep(50))
    ONEIOS_MEDIA_GUARD_FAST_MS = 25,   // the interrupt, a session streaming: the guard
    ONEIOS_MEDIA_GUARD_MS = 1000,      // the interrupt, no session streaming: the guard
};

// The transport's counters (oneios_media_counts[] of the staged DroidVMVirtioMedia.c).
enum {
    ONEIOS_MEDIA_INTERRUPTS,           // the interrupt handler ran
    ONEIOS_MEDIA_WAKE_IRQ,             // the event worker woke by the interrupt
    ONEIOS_MEDIA_WAKE_TIMER,           // the event worker woke by its timer (a poll, or the guard)
    ONEIOS_MEDIA_TIMER_FOUND,          // of those, the wakes that found events
    ONEIOS_MEDIA_COMMAND_WAITS,        // commands not answered when posted (each such waits 1 ms at a time)
    ONEIOS_MEDIA_COUNTS
};

// Interrupts are SPIs: GIC INTIDs 32 to 1019.
static inline bool oneios_media_spi_valid(uint32_t intid) { return intid >= 32 && intid < 1020; }

// Whether the transport asks ArmCpu for the window's interrupt: the switch on and an SPI in the device tree.
static inline bool oneios_media_wants_irq(uint32_t enabled, uint32_t intid) {
    return enabled && oneios_media_spi_valid(intid);
}

// NULL when the device runs on its interrupt; else why it is polled. `attach` is armcpu_spi_attach's result (0: attached),
// asked only when oneios_media_wants_irq said so.
static inline const char *oneios_media_polled_why(uint32_t enabled, uint32_t intid, int attach) {
    if (!enabled) return "oneios-codec-irq=0";
    if (!oneios_media_spi_valid(intid)) return "no interrupt in the device tree";
    if (attach) return "ArmCpu did not attach the SPI";
    return NULL;
}

// The event worker's longest sleep, in milliseconds.
static inline uint32_t oneios_media_wait_ms(bool polled, bool streaming) {
    if (polled) return streaming ? ONEIOS_MEDIA_POLL_FAST_MS : ONEIOS_MEDIA_POLL_MS;
    return streaming ? ONEIOS_MEDIA_GUARD_FAST_MS : ONEIOS_MEDIA_GUARD_MS;
}

// One wake of the event worker: `timed_out` its timer (else the interrupt), `found` whether it found events.
static inline void oneios_media_count_wake(volatile uint64_t *counts, bool timed_out, bool found) {
    __atomic_fetch_add(&counts[timed_out ? ONEIOS_MEDIA_WAKE_TIMER : ONEIOS_MEDIA_WAKE_IRQ], 1, __ATOMIC_RELAXED);
    if (timed_out && found) __atomic_fetch_add(&counts[ONEIOS_MEDIA_TIMER_FOUND], 1, __ATOMIC_RELAXED);
}

// The line (after "ONEIOS CODEC: "): `why` NULL with the interrupt (`spi` its INTID), else why polled; the counts of the
// last `seconds` (now - last, each), `commands` the commands sent in them. snprintf's result.
static inline int oneios_media_line(char *out, size_t size, const char *why, uint32_t spi, uint32_t seconds,
                                    const uint64_t *now, const uint64_t *last, uint64_t commands) {
    uint64_t d[ONEIOS_MEDIA_COUNTS];
    for (int i = 0; i < ONEIOS_MEDIA_COUNTS; ++i) d[i] = now[i] - last[i];
    char mode[64];
    if (why) snprintf(mode, sizeof mode, "polled (%s)", why);
    else snprintf(mode, sizeof mode, "interrupt (SPI %u)", spi);
    return snprintf(out, size,
                    "transport: %s, %u s: interrupts %llu; event worker woken by the interrupt %llu, by the timer %llu "
                    "(with events %llu); commands %llu (waited %llu)",
                    mode, seconds, (unsigned long long)d[ONEIOS_MEDIA_INTERRUPTS], (unsigned long long)d[ONEIOS_MEDIA_WAKE_IRQ],
                    (unsigned long long)d[ONEIOS_MEDIA_WAKE_TIMER], (unsigned long long)d[ONEIOS_MEDIA_TIMER_FOUND],
                    (unsigned long long)commands, (unsigned long long)d[ONEIOS_MEDIA_COMMAND_WAITS]);
}
