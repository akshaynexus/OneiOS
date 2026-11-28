// OneiOS: the switches of the bridge on metal2vulkan-rt (userland/metal/rt, docs/switches.md), read once as the plug-in
// loads, before any device is made. Appended to the staged NativeAdrenoMetalPlugin.mm by userland/metal/build.sh `stage`
// with METAL_RUNTIME=rt only (the 624bab3 build has none of these).
//   boot-args-add=oneios-dont-care=1      Metal's DontCare stores as DONT_CARE (rt 0005 on, an A/B of the speed only: it
//                                         loses the draws before a split pass's split; the bridge reads
//                                         METAL2VULKAN_DONT_CARE_STORES=1). Without it they store, the default.
//   boot-args-add=oneios-queue-priority=0 no global queue priority (rt 0006 off; the plug-in's variable is
//                                         METAL2VULKAN_QUEUE_PRIORITY=default, which the bridge takes as none)
//   boot-args-add=oneios-gmem=1           keep Turnip GMEM on for 8-bit BGRA displays (by default on 8-bit BGRA
//                                         displays — when oneios-display-wide=1 is not set — TU_DEBUG=sysmem,nocb is
//                                         set before deviceForPort so Turnip renders in system memory, avoiding GMEM
//                                         tile/feedback-loop corruption on Adreno 750; sheng-tu-debug= still overrides).
#include <stdlib.h>
#include <string.h>
#include <sys/sysctl.h>

namespace oneios_rt {

// Whether the kernel's boot-args hold `word` as a whole word.
static bool bootWord(const char *args, const char *word) {
    const size_t length = strlen(word);
    for (const char *at = strstr(args, word); at; at = strstr(at + 1, word)) {
        const char end = at[length];
        if ((at == args || at[-1] == ' ') && (end == 0 || end == ' ')) return true;
    }
    return false;
}

__attribute__((constructor)) static void rtSwitches() {
    char args[1024] = {};
    size_t size = sizeof(args) - 1;
    if (sysctlbyname("kern.bootargs", args, &size, nullptr, 0)) return;
    if (bootWord(args, "oneios-dont-care=1")) setenv("METAL2VULKAN_DONT_CARE_STORES", "1", 1);
    // The plug-in sets "high" later in the render server without overwriting: "default" set here first stays.
    if (bootWord(args, "oneios-queue-priority=0")) setenv("METAL2VULKAN_QUEUE_PRIORITY", "default", 1);
    // On 8-bit BGRA displays (the default without oneios-display-wide=1), Turnip's GMEM path has known tile
    // swap/feedback-loop corruption on Adreno 750 and was measured slightly slower than sysmem (NativeAdrenoMetalPlugin.mm).
    // Set sysmem,nocb without overwriting so sheng-tu-debug= and /private/var/ngpu/tu_debug still win.
    if (!bootWord(args, "oneios-display-wide=1") && !bootWord(args, "oneios-gmem=1") && !strstr(args, "sheng-tu-debug="))
        setenv("TU_DEBUG", "sysmem,nocb", 0);
}

}  // namespace oneios_rt
