// Written with `oneios-tool kernel gen codec` (2026-11-25): no Python original.
/// DroidVMVirtioMedia.c's lines that change (edit 7 of the codec archive, the transport's; edits 1 to 6 are
/// DroidVMVTHost.cpp's, staged/codec.rs and staged/codec_decode.rs).
///
/// 7. The event queue on the device's interrupt (fix/codec-irq, 2026-11-25). The pinned transport (f9121cd) sets
///    VIRTQ_AVAIL_F_NO_INTERRUPT on both queues and its thread looks at the event queue every millisecond while a
///    session streams (`IOSleep(fast ? 1 : 50)`): 1000 timer wakes of a vCPU a second while a video plays, 20 a second
///    for the whole boot, and an event (a decoded picture, a consumed input buffer) handed on up to a millisecond late.
///    xnuhost's device raises its SPI on every used-ring write (runtime/core/src/virtio.rs `put`), and the loader names it
///    in the window's node (`droidvm,intid`, runtime/loader/src/machine.rs); only the guest never took it.
///    Now, after Droid-VM/UniXNU-droidvm 96e8c1f and 2cd4c8e (BSD-2-Clause-Patent; lateautumn233, HuJK):
///    - The window's INTID is read from `droidvm,intid` (xnuhost's and DarwinBoot's name), `interrupts` or `gic-spi`, and
///      attached with ArmCpu's armcpu_spi_attach (a level SPI, as the bridge, camera and input drivers take theirs). The
///      handler acknowledges the device's interrupt status (0x60, 0x64) and wakes the event worker; it runs no mutex and
///      no callback.
///    - Only the event queue asks for interrupts. The command queue keeps VIRTQ_AVAIL_F_NO_INTERRUPT: xnuhost answers a
///      command inside the guest's notify (the used ring is written when `post` returns), so an interrupt per command would
///      be a trap and a dispatch for nothing; xnuhost honours the flag on this device (`Device::quiet_queues`). A command
///      not answered when posted still waits 1 ms at a time, counted.
///    - The event worker sleeps on the interrupt with a guard (OneiOSCodecWait.h: 25 ms while a session streams, 1000
///      otherwise), so a lost interrupt delays an event and never hangs a session. An interrupt between the wait's arming
///      and the sleep is kept in media_pending (96e8c1f's order).
///    - Events are copied and their buffers given back under the device's lock, then handed to DroidVMVTHost.cpp's sink
///      without it (96e8c1f): the sink takes the session table's and a session's locks, and a session may hold its own
///      across a command. Only the event worker delivers events now (a command no longer reaps them while it waits).
///    - `oneios-codec-irq=0` keeps the poll of f9121cd (1 ms streaming, 50 otherwise; both queues NO_INTERRUPT); a window
///      with no INTID or an SPI ArmCpu does not attach (16 slots) is polled the same way.
///    - Counters for OneiOSCodec.c's `transport:` line (oneios_media_counts[], OneiOSCodecWait.h's indices) and the mode
///      (oneios_media_polled[] NULL or why, oneios_media_spi[]).
pub const CODEC_TRANSPORT: &[(&str, &str)] = &[
    // The waits' headers and ArmCpu's attach.
    ("#include <kern/thread.h>\n#include <kern/locks.h>\n",
     "#include <kern/thread.h>\n#include <kern/sched_prim.h>\n#include <kern/clock.h>\n#include <kern/locks.h>\n"),
    ("extern void IOSleep(unsigned milliseconds);\nextern void delay(int usec);\n",
     r#"extern void IOSleep(unsigned milliseconds);
extern void delay(int usec);
// OneiOS (kernel gen codec, edit 7, 2026-11-25): the event queue on the device's interrupt (OneiOSCodecWait.h).
extern int armcpu_spi_attach(uint32_t intid, bool edge, void (*handler)(void *), void *context, uint32_t *readback, uint64_t *router);
#include "OneiOSCodecWait.h"
"#),
    // The device's interrupt.
    ("    volatile uint32_t fast;            // sessions streaming: the event queue is polled every millisecond, not every 50\n};\n",
     r#"    volatile uint32_t fast;            // sessions streaming: the event queue is polled every millisecond, not every 50
    uint32_t interrupt_id;             // OneiOS: the window's INTID (0: the device tree names none)
    bool irq;                          // OneiOS: the SPI is attached and the event queue raises it; else polled
};
"#),
    ("static lck_grp_t *media_group;\n",
     r#"static lck_grp_t *media_group;
// OneiOS (edit 7): an interrupt not yet seen by the event worker (kept across the arming of its wait, as 96e8c1f), the
// switch, and what OneiOSCodec.c's transport line says.
static bool media_pending;
static uint32_t media_irq_enabled = 1;
volatile uint64_t oneios_media_counts[ONEIOS_MEDIA_COUNTS];
const char *volatile oneios_media_polled[MEDIA_MAX];
volatile uint32_t oneios_media_spi[MEDIA_MAX];
"#),
    // The handler, and the events handed on without the device's lock.
    ("// The events the device has written back: counted (the layers above take them once they exist) and the buffers returned.
static void reap_events(struct media_device *d) {
    while (used(d, MEDIA_EVENTQ)[1] != d->used[MEDIA_EVENTQ]) {
        media_barrier();
        const uint32_t *ring = (const uint32_t *)(used(d, MEDIA_EVENTQ) + 2);
        const uint32_t id = ring[(d->used[MEDIA_EVENTQ] % MEDIA_QUEUE) * 2];
        ++d->used[MEDIA_EVENTQ];
        uint32_t event;
        memcpy(&event, media_events[index_of(d)][id % MEDIA_EVENTS], 4);
        if (event <= EVT_EVENT) droidvm_media_events[event]++;
        if (d->sink) d->sink(d->sink_context, media_events[index_of(d)][id % MEDIA_EVENTS], MEDIA_EVENT_SIZE);
        post(d, MEDIA_EVENTQ, (uint16_t)(id % MEDIA_EVENTS));
    }
}
",
     r#"// OneiOS (edit 7): the device's interrupt, in ArmCpu's primary interrupt context before the EOI. The line is a level: the
// device's interrupt status is acknowledged here, then the event worker is woken. No mutex, no callback.
static void media_interrupt(void *context) {
    struct media_device *d = context;
    const uint32_t status = media_read32(d, 0x60) & 3;
    if (status) media_write32(d, 0x64, status);
    __atomic_fetch_add(&oneios_media_counts[ONEIOS_MEDIA_INTERRUPTS], 1, __ATOMIC_RELAXED);
    __atomic_store_n(&media_pending, true, __ATOMIC_RELEASE);
    thread_wakeup((event_t)&media_pending);
}

// The events the device has written back: counted, copied and their buffers returned under the device's lock, then handed
// to the layer above without it (OneiOS edit 7, as 96e8c1f: the sink takes the session locks, and a session may hold its
// own across a command). Only the event worker calls it. Whether there was any.
static bool reap_events(struct media_device *d) {
    for (bool any = false;; any = true) {
        uint8_t copy[MEDIA_EVENT_SIZE];
        void (*sink)(void *, const uint8_t *, uint32_t);
        void *context;
        lck_mtx_lock(d->lock);
        if (used(d, MEDIA_EVENTQ)[1] == d->used[MEDIA_EVENTQ]) { lck_mtx_unlock(d->lock); return any; }
        media_barrier();
        const uint32_t *ring = (const uint32_t *)(used(d, MEDIA_EVENTQ) + 2);
        const uint32_t id = ring[(d->used[MEDIA_EVENTQ] % MEDIA_QUEUE) * 2];
        ++d->used[MEDIA_EVENTQ];
        memcpy(copy, media_events[index_of(d)][id % MEDIA_EVENTS], sizeof copy);
        uint32_t event;
        memcpy(&event, copy, 4);
        if (event <= EVT_EVENT) droidvm_media_events[event]++;
        sink = d->sink; context = d->sink_context;
        post(d, MEDIA_EVENTQ, (uint16_t)(id % MEDIA_EVENTS));
        lck_mtx_unlock(d->lock);
        if (sink) sink(context, copy, sizeof copy);
    }
}
"#),
    // A command no longer reaps events while it waits (the event worker does, without the lock it holds); counted.
    ("    for (unsigned waited = 0; used(d, MEDIA_COMMANDQ)[1] == d->used[MEDIA_COMMANDQ]; ++waited) {
        if (waited == 5000) { droidvm_media_timeouts++; return -1; }   // 5 s
        reap_events(d);
        IOSleep(1);
    }
",
     r#"    // OneiOS (edit 7): xnuhost answers inside the notify; a command it did not is counted and waits 1 ms at a time (the
    // command queue raises no interrupt: VIRTQ_AVAIL_F_NO_INTERRUPT). Events are the event worker's.
    if (used(d, MEDIA_COMMANDQ)[1] == d->used[MEDIA_COMMANDQ])
        __atomic_fetch_add(&oneios_media_counts[ONEIOS_MEDIA_COMMAND_WAITS], 1, __ATOMIC_RELAXED);
    for (unsigned waited = 0; used(d, MEDIA_COMMANDQ)[1] == d->used[MEDIA_COMMANDQ]; ++waited) {
        if (waited == 5000) { droidvm_media_timeouts++; return -1; }   // 5 s
        IOSleep(1);
    }
"#),
    // The event queue asks for the interrupt once it is attached; the command queue never.
    ("    media_barrier();\n    media_write32(d, 0x70, 15);         // DRIVER_OK\n",
     r#"    // OneiOS (edit 7): the window's SPI, unless oneios-codec-irq=0 or the tree names none; the event queue (1) then asks
    // for used-buffer interrupts. The command queue (0) keeps NO_INTERRUPT: xnuhost answers a command inside its notify.
    const unsigned device = index_of(d);
    int attach = -1;
    if (oneios_media_wants_irq(media_irq_enabled, d->interrupt_id))
        attach = armcpu_spi_attach(d->interrupt_id, false, media_interrupt, d, NULL, NULL);
    oneios_media_polled[device] = oneios_media_polled_why(media_irq_enabled, d->interrupt_id, attach);
    d->irq = oneios_media_polled[device] == NULL;
    oneios_media_spi[device] = d->irq ? d->interrupt_id : 0;
    if (d->irq) *(volatile uint16_t *)(media_rings[device][MEDIA_EVENTQ] + 1024) = 0;
    media_barrier();
    media_write32(d, 0x70, 15);         // DRIVER_OK
"#),
    // The event worker: on the interrupt with a guard, or polled as before.
    ("    // The event queues stay stocked, and their events go to the layer above that took the device (droidvm_media_set_sink).
    for (;;) {
        bool fast = false;
        for (unsigned device = 0; device < media_count; ++device) {
            if (!media[device].up) continue;
            lck_mtx_lock(media[device].lock);
            reap_events(&media[device]);
            lck_mtx_unlock(media[device].lock);
            fast |= media[device].fast != 0;
        }
        IOSleep(fast ? 1 : 50);
    }
",
     r#"    // The event queues stay stocked, and their events go to the layer above that took the device (droidvm_media_set_sink).
    // OneiOS (edit 7): the worker sleeps until the interrupt, at most the guard (25 ms while a session streams, 1000
    // otherwise); with a polled device among them it wakes every 1 ms while a session streams and every 50 otherwise, as
    // before. An interrupt that comes before the sleep is in media_pending: the wait is armed first, then looked at.
    bool polled = false;
    for (unsigned device = 0; device < media_count; ++device)
        polled |= media[device].up && !media[device].irq;
    for (;;) {
        bool fast = false;
        for (unsigned device = 0; device < media_count; ++device)
            fast |= media[device].up && media[device].fast != 0;
        assert_wait_timeout((event_t)&media_pending, THREAD_UNINT, oneios_media_wait_ms(polled, fast), NSEC_PER_MSEC);
        wait_result_t woke = THREAD_AWAKENED;
        if (__atomic_load_n(&media_pending, __ATOMIC_ACQUIRE)) clear_wait(current_thread(), THREAD_AWAKENED);
        else woke = thread_block(THREAD_CONTINUE_NULL);
        __atomic_store_n(&media_pending, false, __ATOMIC_RELEASE);
        bool any = false;
        for (unsigned device = 0; device < media_count; ++device)
            if (media[device].up) any |= reap_events(&media[device]);
        oneios_media_count_wake(oneios_media_counts, woke == THREAD_TIMED_OUT, any);
    }
"#),
    // The switch, and the window's INTID.
    ("    PE_parse_boot_argn(\"droidvm-media\", &enabled, sizeof(enabled));\n",
     "    PE_parse_boot_argn(\"droidvm-media\", &enabled, sizeof(enabled));\n    \
      PE_parse_boot_argn(\"oneios-codec-irq\", &media_irq_enabled, sizeof(media_irq_enabled));   // OneiOS (edit 7)\n"),
    ("        if (SecureDTGetProperty(node, \"reg\", &value, &size) != kSuccess || size != sizeof(reg)) continue;\n",
     r#"        // OneiOS (edit 7, as 2cd4c8e): the window's interrupt, by any of its names (xnuhost's tree and DarwinBoot's say
        // droidvm,intid; macOS-DroidVM's gic-spi); none: polled.
        if ((SecureDTGetProperty(node, "droidvm,intid", &value, &size) == kSuccess ||
             SecureDTGetProperty(node, "interrupts", &value, &size) == kSuccess ||
             SecureDTGetProperty(node, "gic-spi", &value, &size) == kSuccess) && size == sizeof d->interrupt_id)
            memcpy(&d->interrupt_id, value, size);
        if (SecureDTGetProperty(node, "reg", &value, &size) != kSuccess || size != sizeof(reg)) continue;
"#),
];
