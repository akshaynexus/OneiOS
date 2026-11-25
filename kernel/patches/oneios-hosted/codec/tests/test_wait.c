// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// The host test of OneiOSCodecWait.h, the code the staged DroidVMVirtioMedia.c (edit 7) and OneiOSCodec.c run in the
// kernel (kernel/tests/host.sh builds and runs it), on both paths:
//   mode      oneios_media_wants_irq and oneios_media_polled_why: the switch off, no INTID, an INTID that is no SPI, an
//             attach ArmCpu refused, the interrupt
//   wait      oneios_media_wait_ms: polled, the pin's 1 ms streaming and 50 ms otherwise; on the interrupt only the guard
//   wake      oneios_media_count_wake: a stream of wakes on the interrupt (a lost interrupt found by the guard) and polled
//   line      oneios_media_line: the text of both modes, the counts as differences, a short buffer
// No Apple file, no file of another project.
#include "../OneiOSCodecWait.h"
#include <stdio.h>
#include <string.h>

static int checks, failures;
#define CHECK(cond, ...) do { ++checks; if (!(cond)) { ++failures; fprintf(stderr, "FAIL %s:%d: ", __FILE__, __LINE__); \
                              fprintf(stderr, __VA_ARGS__); fputc('\n', stderr); } } while (0)

static void mode(void) {
    // xnuhost's loader gives each device droidvm,intid 32 + its index (runtime/loader/src/machine.rs BOARD_VIRTIO_SPI)
    CHECK(oneios_media_wants_irq(1, 40), "an SPI with the switch on");
    CHECK(!oneios_media_wants_irq(0, 40), "the switch off");
    CHECK(!oneios_media_wants_irq(1, 0), "no INTID in the tree");
    CHECK(!oneios_media_wants_irq(1, 27) && !oneios_media_wants_irq(1, 1020), "a PPI or a special INTID is no SPI");
    CHECK(oneios_media_polled_why(1, 40, 0) == NULL, "attached: on the interrupt");
    const char *why = oneios_media_polled_why(0, 40, -1);
    CHECK(why && !strcmp(why, "oneios-codec-irq=0"), "the switch: %s", why ? why : "(null)");
    why = oneios_media_polled_why(1, 0, -1);
    CHECK(why && !strcmp(why, "no interrupt in the device tree"), "no INTID: %s", why ? why : "(null)");
    why = oneios_media_polled_why(1, 40, 0x2c5);   // kIOReturnExclusiveAccess-like refusal, or ArmCpu's 16 slots full
    CHECK(why && !strcmp(why, "ArmCpu did not attach the SPI"), "refused: %s", why ? why : "(null)");
}

static void wait(void) {
    CHECK(oneios_media_wait_ms(true, true) == 1, "polled, streaming: the pin's 1 ms");
    CHECK(oneios_media_wait_ms(true, false) == 50, "polled, quiet: the pin's 50 ms");
    CHECK(oneios_media_wait_ms(false, true) == 25, "interrupt, streaming: the 25 ms guard");
    CHECK(oneios_media_wait_ms(false, false) == 1000, "interrupt, quiet: the 1 s guard");
    // the guard is what bounds a lost interrupt: never unbounded, and longer than the poll it replaces
    for (int p = 0; p < 2; ++p) for (int s = 0; s < 2; ++s) {
        const uint32_t ms = oneios_media_wait_ms(p, s);
        CHECK(ms > 0 && ms <= 1000, "bounded: %u", ms);
    }
    CHECK(oneios_media_wait_ms(false, true) > oneios_media_wait_ms(true, true), "fewer wakes than the poll");
}

static void wake(void) {
    volatile uint64_t c[ONEIOS_MEDIA_COUNTS] = {0};
    // on the interrupt, one second of a 30 fps video: about 60 events (a decoded picture and a consumed input each
    // frame), some coalesced into one interrupt; one interrupt lost and found by the guard
    for (int i = 0; i < 50; ++i) oneios_media_count_wake(c, false, true);
    oneios_media_count_wake(c, false, false);   // an interrupt whose events the previous pass took already
    oneios_media_count_wake(c, true, true);     // the guard found an event: a late or lost interrupt
    oneios_media_count_wake(c, true, false);    // the guard found nothing
    CHECK(c[ONEIOS_MEDIA_WAKE_IRQ] == 51, "by the interrupt: %llu", (unsigned long long)c[ONEIOS_MEDIA_WAKE_IRQ]);
    CHECK(c[ONEIOS_MEDIA_WAKE_TIMER] == 2, "by the timer: %llu", (unsigned long long)c[ONEIOS_MEDIA_WAKE_TIMER]);
    CHECK(c[ONEIOS_MEDIA_TIMER_FOUND] == 1, "timer with events: %llu", (unsigned long long)c[ONEIOS_MEDIA_TIMER_FOUND]);
    CHECK(c[ONEIOS_MEDIA_INTERRUPTS] == 0 && c[ONEIOS_MEDIA_COMMAND_WAITS] == 0, "the handler's and commands' are their own");
    // polled: every wake is the timer's (a second streaming: 1000), the ones with events counted
    volatile uint64_t p[ONEIOS_MEDIA_COUNTS] = {0};
    for (int i = 0; i < 1000; ++i) oneios_media_count_wake(p, true, i % 17 == 0);
    CHECK(p[ONEIOS_MEDIA_WAKE_TIMER] == 1000 && p[ONEIOS_MEDIA_WAKE_IRQ] == 0, "polled: %llu by the timer",
          (unsigned long long)p[ONEIOS_MEDIA_WAKE_TIMER]);
    CHECK(p[ONEIOS_MEDIA_TIMER_FOUND] == 59, "polled with events: %llu", (unsigned long long)p[ONEIOS_MEDIA_TIMER_FOUND]);
}

static void line(void) {
    char out[256];
    const uint64_t last[ONEIOS_MEDIA_COUNTS] = {10, 9, 1, 0, 0};
    const uint64_t now[ONEIOS_MEDIA_COUNTS] = {1810, 1790, 41, 2, 0};
    int n = oneios_media_line(out, sizeof out, NULL, 40, 30, now, last, 3600);
    const char *irq = "transport: interrupt (SPI 40), 30 s: interrupts 1800; event worker woken by the interrupt 1781, by the "
                      "timer 40 (with events 2); commands 3600 (waited 0)";
    CHECK(!strcmp(out, irq) && n == (int)strlen(irq), "interrupt line: %s", out);
    const uint64_t zero[ONEIOS_MEDIA_COUNTS] = {0};
    const uint64_t polled[ONEIOS_MEDIA_COUNTS] = {0, 0, 30000, 1800, 1};
    oneios_media_line(out, sizeof out, "oneios-codec-irq=0", 0, 30, polled, zero, 3600);
    const char *poll = "transport: polled (oneios-codec-irq=0), 30 s: interrupts 0; event worker woken by the interrupt 0, by "
                       "the timer 30000 (with events 1800); commands 3600 (waited 1)";
    CHECK(!strcmp(out, poll), "polled line: %s", out);
    // a short buffer is cut, terminated, and the length it wanted is returned
    char small[16];
    n = oneios_media_line(small, sizeof small, NULL, 40, 30, now, last, 0);
    CHECK(strlen(small) == 15 && n > 15, "cut: '%s' %d", small, n);
}

int main(void) {
    mode();
    wait();
    wake();
    line();
    printf("test_wait: %d checks, %d failures\n", checks, failures);
    return failures != 0;
}
