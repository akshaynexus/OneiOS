// SPDX-License-Identifier: MIT
//
// OneiOS: backboardd's framebuffer in half floats when the display path keeps them. Appended to the Metal plug-in's
// NativeAdrenoMetalPlugin.mm by userland/metal/build.sh (stage), as drivers/brightness/BacklightMode.inc is: the plug-in
// is the OneiOS code that runs inside backboardd (the render server).
//
// Why [read in 24A446's QuartzCore, kernel/patches/oneios-hosted/display/README.md "What iOS renders into"]:
// CA::WindowServer::IOMFBDisplay::framebuffer_pixel_format (0x184461838) gives 'RGhA' (half floats, linear extended
// sRGB: wide colour and EDR) when CA_FORCE_FP16_FRAMEBUFFER is set, else, on this display, 'BGRA' (8-bit sRGB: P3's
// colours clipped) or a 10-bit extended-range format the display does not take. The plug-in holds the EDR headroom at 1
// and the FP16 threshold at 1e30 so that it is 'BGRA' (upstream: the CPU's half-float conversion was slow and grey).
// With xnuhost's display-format=rgba16f the kernel has oneios-display-wide=1 and hands 'RGhA' frames to the scanout
// as they are (no conversion in the guest; the display helper posts them as RGBA_FP16 with the dataspace SCRGB_LINEAR):
// then the render server is asked for half floats. Without that boot argument nothing changes.
//
// The option is QuartzCore's bool debug option CA_FORCE_FP16_FRAMEBUFFER (init_debug: _ca_debug_options + 128, read by
// framebuffer_pixel_format at 0x1844618c4): set in the environment for a read after this, and written for one before it
// (the address is the hooks file's, userland/metal/hooks/iPhone18,2-24A446.json: 0x1e702516c, slid by CARenderServerStart's).
//
// What it logs (iOS's log, process backboardd): "OneiOS display: half-float framebuffer asked (...)", or nothing without
// the boot argument. The kernel then logs "oneios-display: the compositor swaps format 'RGhA' ...: to the scanout as it is".
#include <dlfcn.h>
#include <mach/mach.h>
#include <os/log.h>
#include <ptrauth.h>
#include <stdlib.h>
#include <string.h>
#include <sys/sysctl.h>
#include <unistd.h>

__attribute__((constructor)) static void oneiosDisplayWide() {
    if (strcmp(getprogname(), "backboardd")) return;
    char args[1024] = {};
    size_t size = sizeof(args) - 1;
    if (sysctlbyname("kern.bootargs", args, &size, nullptr, 0) || !strstr(args, "oneios-display-wide=1")) return;
    setenv("CA_FORCE_FP16_FRAMEBUFFER", "1", 1);
    bool stored = false;
#if defined(__arm64e__)
    if (void *start = dlsym(RTLD_DEFAULT, "CARenderServerStart")) {
        const uintptr_t slide = (uintptr_t)ptrauth_strip(start, ptrauth_key_function_pointer) - 0x1846dce88;
        const uint8_t on = 1;
        stored = vm_write(mach_task_self(), 0x1e702516c + 128 + slide, (vm_offset_t)&on, sizeof on) == KERN_SUCCESS;
    }
#endif
    os_log(OS_LOG_DEFAULT, "OneiOS display: half-float framebuffer asked (CA_FORCE_FP16_FRAMEBUFFER=1%s; oneios-display-wide=1)",
           stored ? ", the stored option written too" : ", the stored option not written");
}
