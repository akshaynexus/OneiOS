// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// The hosted kernel's video codec: oneios_codec_start, called once by oneios_drivers_start
// (drivers/bridge/OneiOSBridge.c; a weak default there for a kernel without this archive). It brings up
// UniXNU-droidvm's virtio-media transport (DroidVMVirtioMedia.c: xnuhost's /arm-io/virtio-media-0, the host end
// runtime/bridges/encoder) and publishes iOS's paravirtualized VideoToolbox driver (DroidVMVTParavirt.cpp +
// DroidVMVTHost.cpp), so VideoToolbox encodes HEVC/H.264 through the phone's hardware encoder and CMPhoto writes
// HEIC (chronod's widget snapshots, screenshots, the photo library). See docs/hardware.md, "Video encoder".
//
// droidvm-media (default 1) brings up the transport; droidvm-vt-paravirt=1 (xnuhost adds it when encoder=auto or
// encoder=sim gave the guest a device) publishes AppleVideoToolboxParavirtualizationDriver. oneios-codec=0 leaves the
// whole archive out at boot.
//
// With a device, a report thread says what the three files count, every 30 s while a figure moves (nothing while
// nobody uses the codec): who opened the service, the messages, the encoder sessions and frames, the first failed
// step, the transport's commands and events, and the lines DroidVMVTHost.cpp keeps of each session (its trace ring:
// the size and codec of a session, the IOSurface lookups). oneios-codec-report=0 leaves it out.
#include <stdbool.h>
#include <stdint.h>
#include <kern/thread.h>
#include <pexpert/pexpert.h>

extern void IOSleep(unsigned milliseconds);
extern int printf(const char *, ...);
extern void droidvm_media_start(void);                 // DroidVMVirtioMedia.c
extern int droidvm_vt_paravirt_attach(void);           // DroidVMVTParavirt.cpp
extern volatile uint32_t droidvm_media_found, droidvm_media_up;
extern volatile uint64_t droidvm_media_commands, droidvm_media_timeouts, droidvm_media_events[3];
extern char droidvm_media_status[][192];
extern volatile uint32_t droidvm_vtpv_stage, droidvm_vtpv_opens, droidvm_vtpv_closes, droidvm_vtpv_messages, droidvm_vtpv_refused;
extern volatile uint32_t droidvm_vthost_sessions, droidvm_vthost_frames_in, droidvm_vthost_frames_out, droidvm_vthost_errors;
extern volatile uint32_t droidvm_vthost_error_stage, droidvm_vthost_replies, droidvm_vthost_unknown;
extern volatile int32_t droidvm_vthost_error_value;
extern volatile uint32_t droidvm_vtpv_sent, droidvm_vtpv_dropped;
extern char droidvm_vthost_trace[16][112];
extern volatile uint32_t droidvm_vthost_trace_count;
// DroidVMVTParavirt.cpp's record of a message from a process (its struct droidvm_vtpv_record at the pin f9121cd, 136
// bytes; the ring keeps the newest 32, indexed by seq).
struct oneios_codec_record {
    uint32_t seq, pid, length, surface_count;
    uint32_t type, flags;
    uint64_t request;
    uint8_t uuid[16];
    uint32_t surfaces[5];
    uint32_t reserved;
    uint8_t payload[64];
};
_Static_assert(sizeof(struct oneios_codec_record) == 136, "struct droidvm_vtpv_record");
extern struct oneios_codec_record droidvm_vtpv_log[32];
extern void proc_name(int pid, char *buf, int size);

static void oneios_codec_report(void *arg, wait_result_t result) {
    (void)arg; (void)result;
    IOSleep(3000);                                     // the transport's own start-up check of the device is done by now
    printf("ONEIOS CODEC: device 0: %.191s\n", droidvm_media_status[0]);
    uint64_t last = 0;
    uint32_t traced = 0, shown = 0;
    for (;;) {
        IOSleep(30000);
        const uint64_t moved = (uint64_t)droidvm_vtpv_opens + droidvm_vtpv_closes + droidvm_vtpv_messages + droidvm_vthost_errors +
                               droidvm_vthost_frames_out + droidvm_media_timeouts;
        if (moved == last) continue;
        last = moved;
        printf("ONEIOS CODEC: counters: service opened %u closed %u, messages %u (refused %u, unknown %u), to the guest %u "
               "(dropped %u), replies %u; sessions %u, frames in %u out %u, errors %u (last: step %u value %d); "
               "virtio-media commands %llu (timeouts %llu), events %llu errors %llu buffers %llu other\n",
               droidvm_vtpv_opens, droidvm_vtpv_closes, droidvm_vtpv_messages, droidvm_vtpv_refused, droidvm_vthost_unknown,
               droidvm_vtpv_sent, droidvm_vtpv_dropped, droidvm_vthost_replies,
               droidvm_vthost_sessions, droidvm_vthost_frames_in, droidvm_vthost_frames_out,
               droidvm_vthost_errors, droidvm_vthost_error_stage, droidvm_vthost_error_value,
               droidvm_media_commands, droidvm_media_timeouts, droidvm_media_events[0], droidvm_media_events[1], droidvm_media_events[2]);
        // The messages since the last report, the newest 32 at most: who sent what (encs: the encoder list; +enc, esta,
        // efrm, ecfr, einv, -enc: an encoder session; decs: the decoder list).
        const uint32_t messages = droidvm_vtpv_messages;
        if (messages - shown > 32) shown = messages - 32;
        for (; shown != messages; ++shown) {
            const struct oneios_codec_record *r = &droidvm_vtpv_log[shown % 32];
            char name[33] = "?";
            if (r->seq != shown) continue;             // the slot was taken again meanwhile
            proc_name((int)r->pid, name, sizeof name);
            printf("ONEIOS CODEC: message %u: %s[%u] '%c%c%c%c', %u bytes, %u surface(s)\n", r->seq, name, r->pid,
                   (char)(r->type >> 24), (char)(r->type >> 16), (char)(r->type >> 8), (char)r->type, r->length, r->surface_count);
        }
        // The session lines written since the last report (the ring keeps the newest 16).
        const uint32_t count = droidvm_vthost_trace_count;
        if (count - traced > 16) traced = count - 16;
        for (; traced != count; ++traced) printf("ONEIOS CODEC: session: %.111s\n", droidvm_vthost_trace[traced % 16]);
    }
}

void oneios_codec_start(void) {
    uint32_t enabled = 1, report = 1;
    PE_parse_boot_argn("oneios-codec", &enabled, sizeof enabled);
    if (!enabled) { printf("ONEIOS CODEC: off (oneios-codec=0)\n"); return; }

    droidvm_media_start();                             // the virtio-media encoder/decoder transport
    const int paravirt = droidvm_vt_paravirt_attach(); // the VideoToolbox paravirtualization classes + service

    printf("ONEIOS CODEC: %u virtio-media device(s), %u up; VideoToolbox paravirtualization %s (stage %u)\n",
           droidvm_media_found, droidvm_media_up,
           paravirt == 0 && droidvm_vtpv_stage == 2 ? "registered" : "not published", droidvm_vtpv_stage);
    PE_parse_boot_argn("oneios-codec-report", &report, sizeof report);
    thread_t thread;
    if (report && droidvm_media_up && kernel_thread_start(oneios_codec_report, NULL, &thread) == KERN_SUCCESS) thread_deallocate(thread);
}
