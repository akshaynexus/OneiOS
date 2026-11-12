// SPDX-License-Identifier: MIT
//
// OneiOS: backboardd's brightness control, the physical one. Appended to the Metal plug-in's NativeAdrenoMetalPlugin.mm by
// userland/metal/build.sh (stage): the plug-in is the OneiOS code that runs inside backboardd, loaded on backboardd's main
// thread when the window server makes its Metal device, before the same thread instantiates BrightnessSystem (every
// session of 2026-11-12 with the plug-in: "native-metal: ..." 20-90 ms before "instantiating BrightnessSystem").
//
// Why [read in 24A446's CoreBrightness, drivers/brightness/README.md has the addresses]: -[BrightnessSystemInternal init]
// asks _CBU_IsVirtualMachine (MobileGestalt IsVirtualDevice, which is kern.hv_vmm_present, which is the device tree's
// /defaults/vmm-present) and passes initBLControl: mode 3 on a virtual device, else 1 (2, test mode, on internal builds
// only). Mode 3 makes VMBLControl, which serves a Virtualization.framework host over remote XPC
// (com.apple.virtualization.corebrightness) and never sets a property on the backlight service: the slider did nothing,
// SpringBoard got "No container for handle Display(id: 1)", and the guest driver logged no request. vmm-present stays (AMFI,
// VirtualAudio, bluetoothd read it; hiding it from backboardd changes QuartzCore's display paths, and droidvm and sheng saw
// backboardd crash so), so only this one choice changes: mode 3 becomes 1, BLControl, as on an iPhone. The same fix as
// UniXNU's scripts/appsync/BacklightMode.m (iPadOS 27.0.1, the sheng board: the slider works there).
//
// What it logs (iOS's log, process backboardd): "OneiOS backlight: initBLControl: mode 3 -> 1 (BLControl, not VMBLControl)"
// when CoreBrightness asks, "OneiOS backlight: physical brightness control selected" at the swizzle, or why not. CoreBrightness
// itself then says "Set Default 1 BLControl" where it said "Set VM Mode 3 BLControl". oneios-bl-mode=0 in the kernel's boot
// arguments leaves CoreBrightness alone.
#include <objc/runtime.h>
#include <os/log.h>

static BOOL (*gOneiOSInitBLControl)(id, SEL, long long);

static BOOL oneiosInitBLControl(id self, SEL command, long long mode) {
    os_log(OS_LOG_DEFAULT, "OneiOS backlight: initBLControl: mode %lld -> %lld%s", mode, mode == 3 ? 1LL : mode,
           mode == 3 ? " (BLControl, not VMBLControl)" : "");
    return gOneiOSInitBLControl(self, command, mode == 3 ? 1 : mode);
}

__attribute__((constructor)) static void oneiosBacklightMode() {
    if (strcmp(getprogname(), "backboardd")) return;
    char args[1024] = {};
    size_t size = sizeof(args) - 1;
    if (!sysctlbyname("kern.bootargs", args, &size, nullptr, 0) && strstr(args, "oneios-bl-mode=0")) {
        os_log(OS_LOG_DEFAULT, "OneiOS backlight: oneios-bl-mode=0: CoreBrightness keeps its own mode");
        return;
    }
    @autoreleasepool {
        dlopen("/System/Library/PrivateFrameworks/CoreBrightness.framework/CoreBrightness", RTLD_NOW);
        Class cls = objc_getClass("BrightnessSystemInternal");
        Method method = cls ? class_getInstanceMethod(cls, sel_registerName("initBLControl:")) : nullptr;
        char type[16] = "";
        if (method) method_getArgumentType(method, 2, type, sizeof(type));
        if (!method || method_getNumberOfArguments(method) != 3 || !type[0] || !strchr("qQlL", type[0])) {
            os_log_error(OS_LOG_DEFAULT, "OneiOS backlight: no -[BrightnessSystemInternal initBLControl:] of the known shape (%s, type '%s'): "
                         "inactive, brightness stays with VMBLControl", cls ? "class found" : "no class", type);
            return;
        }
        gOneiOSInitBLControl = reinterpret_cast<BOOL (*)(id, SEL, long long)>(
            method_setImplementation(method, reinterpret_cast<IMP>(oneiosInitBLControl)));
        os_log(OS_LOG_DEFAULT, "OneiOS backlight: physical brightness control selected (initBLControl: mode 3 becomes 1)");
    }
}
