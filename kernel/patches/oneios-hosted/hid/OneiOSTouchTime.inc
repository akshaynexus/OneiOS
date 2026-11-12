// SPDX-License-Identifier: MIT
// OneiOS: the scan time of each touch frame, appended to the staged DroidVMVirtioInput.c by build-oneios-hid.py.
//
// xnuhost (runtime/xnuhost/src/input.c) puts EV_MSC / MSC_TIMESTAMP before each touch frame's SYN_REPORT: the low 32
// bits of the time the host's kernel stamped the frame with, in ticks of the counter mach_absolute_time reads (the
// guest reads the host's CNTVCT_EL0 itself, so the two clocks are one). Here that time is rebuilt to 64 bits, against
// the counter now, and:
//   - oneios_touch_frame_time holds it while the frame is reported: NativeHidInput's touch report goes to the family
//     with handleReportWithTime(that time) instead of handleReport's "now", so every HID event backboardd gets carries
//     the time the finger was where the event says (what a digitizer's own timestamps give an iPhone). 0 between
//     frames (the held-finger resends, touch_still, are stamped "now").
//   - oneios_touch_latency counts, for the log line (OneiOSHidInput.cpp), how long after the scan the worker had the
//     frame and how long after the scan its HID report had been handed to the event driver (which queues the events
//     for backboardd inside that call).
// A scan time more than ONEIOS_TOUCH_SCAN_MAX_MS old, or in the future, is not used (a host without this code sends
// none; a guest clock with an offset would give one): the frame is stamped "now" and counted in [1].

#define ONEIOS_TOUCH_SCAN_MAX_MS 1000

// The scan time of the frame being reported (mach_absolute_time), 0 = none.
volatile uint64_t oneios_touch_frame_time;
// [0] frames with a usable scan time, [1] frames without one, then in microseconds: [2] the sum, [3] the least and
// [4] the most of the worker's delay (the frame's SYN_REPORT taken from the queue, after the scan), [5] [6] [7] the
// same of the report's (the frame's last HID report handed to the event driver). The logger reads and clears them.
volatile uint64_t oneios_touch_latency[8];

static uint32_t oneios_touch_scan_low;
static bool oneios_touch_scan_set;
static uint64_t oneios_touch_scan_full;

static void oneios_touch_scan(int32_t value) {
    oneios_touch_scan_low = (uint32_t)value;
    oneios_touch_scan_set = true;
}

static uint64_t oneios_touch_us(uint64_t ticks) {
    uint64_t ns;
    absolutetime_to_nanoseconds(ticks, &ns);
    return ns / 1000;
}

// A least of 0 is "none yet" (the logger clears it): a delay under 1 us counts as 1.
static void oneios_touch_count(unsigned at, uint64_t us) {
    if (!us) us = 1;
    oneios_touch_latency[at] = oneios_touch_latency[at] + us;
    if (!oneios_touch_latency[at + 1] || us < oneios_touch_latency[at + 1]) oneios_touch_latency[at + 1] = us;
    if (us > oneios_touch_latency[at + 2]) oneios_touch_latency[at + 2] = us;
}

// Before touch_frame(): the frame's scan time, if xnuhost sent one and it is plausible.
static void oneios_touch_frame_begin(void) {
    static uint64_t limit;
    if (!limit) nanoseconds_to_absolutetime((uint64_t)ONEIOS_TOUCH_SCAN_MAX_MS * NSEC_PER_MSEC, &limit);
    const uint64_t now = mach_absolute_time();
    oneios_touch_frame_time = 0;
    oneios_touch_scan_full = 0;
    if (oneios_touch_scan_set) {
        const uint64_t ago = (uint32_t)((uint32_t)now - oneios_touch_scan_low);   // modulo 2^32: the low bits' distance
        if (ago < limit && ago <= now) oneios_touch_scan_full = now - ago;
    }
    oneios_touch_scan_set = false;
    if (!oneios_touch_scan_full) {
        oneios_touch_latency[1] = oneios_touch_latency[1] + 1;
        return;
    }
    oneios_touch_latency[0] = oneios_touch_latency[0] + 1;
    oneios_touch_count(2, oneios_touch_us(now - oneios_touch_scan_full));
    oneios_touch_frame_time = oneios_touch_scan_full;
}

// After touch_frame(): its reports are with the event driver.
static void oneios_touch_frame_end(void) {
    oneios_touch_frame_time = 0;
    if (oneios_touch_scan_full) oneios_touch_count(5, oneios_touch_us(mach_absolute_time() - oneios_touch_scan_full));
}
