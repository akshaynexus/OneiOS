// SPDX-License-Identifier: MIT
//
// OneiOS: the switch of the Adreno shader fixes in the Metal translator. Appended to the Metal plug-in's
// NativeAdrenoMetalPlugin.mm by userland/metal/build.sh (stage), as DisplayWide.inc is: the plug-in is the OneiOS code
// that every process with a Metal device loads, before that device is made.
//
// What it switches (userland/metal/patches/metal2vulkan, applied to the bridge by `ios metal-stage-edit`):
// 0101 reads a constant table that a shader indexes at run time and that is larger than 4 KiB from the shared constant
// blob instead of a per-thread private copy (an Adreno backs private memory per fiber: MercuryPoster's 192 KiB of ramp
// tables a thread were refused); 0102 lowers pointer constants that are casts or constant GEPs of a blob global
// (`getelementptr (@table, 0, 0, 0)` handed to a helper). Both are the Adreno-generic fixes of hakios-arm64e-dada,
// rebased on the pin; a fork (orumanayurpanjayat-ux/OneiOS d157b43) turned them on in every build. 0110-shader-fix-switch.patch
// gates both on METAL2VULKAN_SHADER_FIX=1, read once per process, and hashes it into the device profile's fingerprint,
// so the shader cache never mixes translations made with and without it. Off, the translations and the cache's
// entries are those of a plug-in without the patches.
//
// Switch: on by default since 2026-11-25 (the owner's order: every new fix on); the boot argument
// oneios-m2v-shader-fix=0 (boot-args-add=oneios-m2v-shader-fix=0 in the image set's xnuhost.conf; docs/switches.md)
// turns it off. Every Metal client, backboardd included. The first boot with it on translates each shader once more
// (the fingerprint changed); the cache then holds the new entries.
//
// What it logs (iOS's log, each process that loads the plug-in), once:
// "OneiOS shader fix: on (oneios-m2v-shader-fix=1, the default): METAL2VULKAN_SHADER_FIX=1", or with the switch off
// "OneiOS shader fix: off (oneios-m2v-shader-fix=0)".
#include <os/log.h>
#include <stdlib.h>
#include <string.h>
#include <sys/sysctl.h>

__attribute__((constructor)) static void oneiosShaderFix() {
    char args[1024] = {};
    size_t size = sizeof(args) - 1;
    bool on = true;
    if (!sysctlbyname("kern.bootargs", args, &size, nullptr, 0)) {
        const char *word = strstr(args, "oneios-m2v-shader-fix=");
        if (word && (word == args || word[-1] == ' ')) {
            word += strlen("oneios-m2v-shader-fix=");
            on = !(word[0] == '0' && (!word[1] || word[1] == ' '));
        }
    }
    if (!on) {
        unsetenv("METAL2VULKAN_SHADER_FIX");
        os_log(OS_LOG_DEFAULT, "OneiOS shader fix: off (oneios-m2v-shader-fix=0)");
        return;
    }
    setenv("METAL2VULKAN_SHADER_FIX", "1", 1);
    os_log(OS_LOG_DEFAULT, "OneiOS shader fix: on (oneios-m2v-shader-fix=1, the default): METAL2VULKAN_SHADER_FIX=1");
}
