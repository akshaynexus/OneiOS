// sensors-als, 2026-11-25: the motion sensors of xnuhost's third virtio-input device ("OneiOS motion sensors": the S24 Ultra's
// sensor hub through runtime/bridges/sensors) as the iPhone's sensor processor publishes them. After UniXNU-droidvm f9121cd
// (package/DroidVMMotionHid.inc, DroidVMVirtioInput.c, BSD-2-Clause-Patent), which does this for the iPadOS 27 board with crosvm's
// motion device; the compass kind's usage pair is ios-redfin 23019ab4's (xnu/patches/0971, MIT: Apple vendor page 0xff00, usage
// 0x0a), its fields IOHIDEventDriver's own (parseCompassElement: Sensor page magnetic flux X/Y/Z 0x0485-0x0487).
//
// The edits, each anchored on one line of the pinned source (a changed anchor stops the build):
//   DroidVMVirtioInput.c (after staged/hid_transport.rs): the motion device's ABS_HAT0X/HAT0Y/HAT1X (0x10-0x12, the magnetic
//     field, 16 per microtesla) are kept and published with the frame inside droidvm_motion's seqlock; each published frame
//     counts and wakes the motion devices (oneios_motion_kick, below).
//   DroidVMMotionHid.inc: a fourth kind, the compass {0xff00, 0x0a}; the devices tick when a frame arrives (oneios_motion_kick
//     re-arms each published device's timer at once), with a 1 s guard, instead of every 10 ms whatever happens: about 50
//     wake-ups a second per published kind while the host sends frames, 1 a second per kind when it sends none (the iPad's
//     source: 100 a second per kind, always); the reports per kind are counted for OneiOSHidInput.cpp's 60 s line.
//   NativeHidInput.cpp (after staged/hid.rs and hid_pen.rs): its table of IOHIDEventDriver's 220 slot discriminators is
//     exported for the light sensor's dispatch wrapper (kernel/patches/oneios-hosted/hid/OneiOSALSEvents.cpp), not copied.

/// DroidVMVirtioInput.c's lines that change for the magnetometer and the frame-driven ticks.
pub const MOTION_TRANSPORT: &[(&str, &str)] = &[
    ("static void motion_publish(const struct droidvm_input *d) {\n",
     concat!("// OneiOS (staged/hid_motion.rs): the magnetometer of xnuhost's motion device (ABS_HAT0X, HAT0Y, HAT1X: 16 per microtesla),\n",
             "// as last reported and as published with the frame (inside droidvm_motion's seqlock); frames published; the motion\n",
             "// devices' wake-up (DroidVMMotionHid.inc as staged).\n",
             "static int32_t oneios_motion_mag_raw[3];\n",
             "int32_t oneios_motion_mag[3];\n",
             "volatile uint64_t oneios_motion_frames;\n",
             "extern void oneios_motion_kick(void);\n",
             "static void motion_publish(const struct droidvm_input *d) {\n")),
    ("    droidvm_motion.orientation = motion_orientation(droidvm_motion.orientation, d->motion);\n",
     concat!("    droidvm_motion.orientation = motion_orientation(droidvm_motion.orientation, d->motion);\n",
             "    for (unsigned i = 0; i < 3; ++i) oneios_motion_mag[i] = oneios_motion_mag_raw[i];   // OneiOS\n")),
    ("    droidvm_motion.seq = droidvm_motion.seq + 1;\n}\n",
     concat!("    droidvm_motion.seq = droidvm_motion.seq + 1;\n",
             "    oneios_motion_frames = oneios_motion_frames + 1;   // OneiOS: counted, and the devices tick now\n",
             "    oneios_motion_kick();\n",
             "}\n")),
    ("    } else if (d->kind == KIND_MOTION && e->type == EV_ABS && e->code <= ABS_RZ) {\n        d->motion[e->code] = e->value;\n",
     concat!("    } else if (d->kind == KIND_MOTION && e->type == EV_ABS && e->code >= 0x10 && e->code <= 0x12) {\n",
             "        oneios_motion_mag_raw[e->code - 0x10] = e->value;   // OneiOS: the magnetometer\n",
             "    } else if (d->kind == KIND_MOTION && e->type == EV_ABS && e->code <= ABS_RZ) {\n",
             "        d->motion[e->code] = e->value;\n")),
];

/// DroidVMMotionHid.inc's lines that change: the compass, the frame-driven ticks, the counts.
pub const MOTION_HID: &[(&str, &str)] = &[
    ("extern struct droidvm_motion droidvm_motion;\n",
     concat!("extern struct droidvm_motion droidvm_motion;\n",
             "// OneiOS (staged/hid_motion.rs): the magnetometer, 16 per microtesla, written with droidvm_motion's frame (the staged\n",
             "// transport); reports sent per kind, for OneiOSHidInput.cpp's 60 s line.\n",
             "extern int32_t oneios_motion_mag[3];\n",
             "volatile uint64_t oneios_motion_reports[4];\n")),
    ("enum { DROIDVM_MOTION_ACCELEROMETER, DROIDVM_MOTION_GYROSCOPE, DROIDVM_MOTION_ORIENTATION };\n",
     concat!("enum { DROIDVM_MOTION_ACCELEROMETER, DROIDVM_MOTION_GYROSCOPE, DROIDVM_MOTION_ORIENTATION, DROIDVM_MOTION_COMPASS };\n",
             "// OneiOS: each published device's timer, which oneios_motion_kick re-arms when the transport publishes a frame. The devices\n",
             "// live for the kernel's life (OneiOSHidInput.cpp never stops them); stop clears its entry before the timer goes.\n",
             "static IOTimerEventSource *volatile oneios_motion_timers[4];\n")),
    ("        source->setTimeoutMS(10);\n",
     concat!("        // OneiOS: a frame from the transport re-arms the timer at once (oneios_motion_kick); this is the guard only.\n",
             "        source->setTimeoutMS(1000);\n")),
    ("            raw[i] = self->kind == DROIDVM_MOTION_GYROSCOPE ? droidvm_motion.gyro[i] : droidvm_motion.accel[i];\n",
     concat!("            raw[i] = self->kind == DROIDVM_MOTION_COMPASS     ? oneios_motion_mag[i]   // OneiOS\n",
             "                   : self->kind == DROIDVM_MOTION_GYROSCOPE ? droidvm_motion.gyro[i] : droidvm_motion.accel[i];\n")),
    ("                report[i] = clamp(self->kind == DROIDVM_MOTION_GYROSCOPE ? (int64_t)raw[i] * 174533 / 160000\n",
     concat!("                // OneiOS: the compass in thousandths of a microtesla (16 per microtesla from the host).\n",
             "                report[i] = clamp(self->kind == DROIDVM_MOTION_COMPASS     ? (int64_t)raw[i] * 1000 / 16\n",
             "                                : self->kind == DROIDVM_MOTION_GYROSCOPE ? (int64_t)raw[i] * 174533 / 160000\n")),
    ("        if (buffer) self->handleReport(buffer, DROIDVM_MOTION_INPUT_REPORT, 0);\n",
     concat!("        if (buffer && self->kind < 4) oneios_motion_reports[self->kind] = oneios_motion_reports[self->kind] + 1;   // OneiOS\n",
             "        if (buffer) self->handleReport(buffer, DROIDVM_MOTION_INPUT_REPORT, 0);\n")),
    ("        return OSString::withCString(kind == DROIDVM_MOTION_GYROSCOPE     ? \"DroidVM gyroscope\"\n",
     concat!("        return OSString::withCString(kind == DROIDVM_MOTION_COMPASS       ? \"OneiOS compass\"\n",
             "                                     : kind == DROIDVM_MOTION_GYROSCOPE ? \"DroidVM gyroscope\"\n")),
    ("        static const unsigned char gyro[] = DROIDVM_MOTION_VECTOR(0x09, 0x76, 0x57);\n",
     concat!("        static const unsigned char gyro[] = DROIDVM_MOTION_VECTOR(0x09, 0x76, 0x57);\n",
             "        // OneiOS: the compass {0xff00, 0x0a} (ios-redfin 0971's pair), Sensor page Compass 3D, magnetic flux X/Y/Z.\n",
             "        static const unsigned char compass[] = DROIDVM_MOTION_VECTOR(0x0a, 0x83, 0x85);\n")),
    ("        *out = kind == DROIDVM_MOTION_GYROSCOPE ? IOBufferMemoryDescriptor::withBytes(gyro, sizeof(gyro), kIODirectionNone)\n",
     concat!("        *out = kind == DROIDVM_MOTION_COMPASS ? IOBufferMemoryDescriptor::withBytes(compass, sizeof(compass), kIODirectionNone)   // OneiOS\n",
             "             : kind == DROIDVM_MOTION_GYROSCOPE ? IOBufferMemoryDescriptor::withBytes(gyro, sizeof(gyro), kIODirectionNone)\n")),
    ("        timer->setTimeoutMS(10);\n        return true;\n",
     concat!("        if (kind < 4) oneios_motion_timers[kind] = timer;   // OneiOS: woken by the transport's frames\n",
             "        timer->setTimeoutMS(10);\n",
             "        return true;\n")),
    ("        if (timer) {\n            timer->cancelTimeout();\n",
     concat!("        if (timer) {\n",
             "            if (kind < 4) oneios_motion_timers[kind] = nullptr;   // OneiOS\n",
             "            __asm__ volatile(\"dmb ish\" ::: \"memory\");\n",
             "            timer->cancelTimeout();\n")),
];

/// Appended to the staged DroidVMMotionHid.inc: the transport's wake-up of the published devices.
pub const MOTION_KICK: &str = r#"
// OneiOS (staged/hid_motion.rs): called by the transport's worker after each published frame. Each published device's timer fires
// at once and its tick sends the frame (IOTimerEventSource::setTimeout may be called from any thread).
extern "C" void oneios_motion_kick(void) {
    for (unsigned kind = 0; kind < 4; ++kind) {
        IOTimerEventSource *timer = oneios_motion_timers[kind];
        if (timer) timer->setTimeoutUS(1);
    }
}
"#;

/// The head of the staged translation unit around DroidVMMotionHid.inc: the iPad's build appends the .inc to NativeHidInput.cpp;
/// here it is a unit of its own, so its class registers in a module load of its own (OneiOSHidInput.cpp) and a clash of its
/// class name can never take the touch screen's classes with it.
pub const MOTION_PRELUDE: &str = r#"// SPDX-License-Identifier: BSD-2-Clause-Patent
// Generated by oneios-tool kernel gen hid (staged/hid_motion.rs): UniXNU-droidvm f9121cd package/DroidVMMotionHid.inc (BSD-2-Clause-
// Patent) in a translation unit of its own, with OneiOS's edits (the compass, frame-driven ticks, counts). Do not edit: change
// the edit list.
#include <IOKit/hid/IOHIDDevice.h>
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IOWorkLoop.h>
#include <IOKit/IOTimerEventSource.h>
#include <IOKit/IOLib.h>
#include <kern/clock.h>
#include <libkern/c++/OSString.h>
#include <libkern/c++/OSDictionary.h>
"#;

/// NativeHidInput.cpp's line after which its IOHIDEventDriver slot table is exported.
pub const HID_SLOTS: &[(&str, &str)] = &[
    ("static constexpr unsigned kDriverSlots=sizeof(kSlotDiversity)/sizeof(kSlotDiversity[0]);\n",
     concat!("static constexpr unsigned kDriverSlots=sizeof(kSlotDiversity)/sizeof(kSlotDiversity[0]);\n",
             "// OneiOS (staged/hid_motion.rs): the table for the light sensor's dispatch wrapper (OneiOSALSEvents.cpp), not a copy of it.\n",
             "extern \"C\" const uint16_t *oneios_hid_driver_slots(unsigned *count) {*count=kDriverSlots;return kSlotDiversity;}\n")),
];
