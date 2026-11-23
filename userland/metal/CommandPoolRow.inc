// SPDX-License-Identifier: MIT
//
// OneiOS: each Metal client's command pools and GPU memory on its kernel text line, lasting lines, and the idle trim of
// Turnip's cache of freed BOs. Appended to the Metal plug-in's NativeAdrenoMetalPlugin.mm by userland/metal/build.sh
// (stage), as DisplayWide.inc is; `oneios-tool ios metal-stage-edit` declares oneios_gpu_pool_row_start near the top of
// the file and calls it where a process gets its device (the render server's path and a client's).
//
// Why (2026-11-23, sessions s1824 and s2023): backboardd held 2,600-3,400 GPU blobs under 1 MiB, 1,400 of them under
// 16 KiB, growing by 200-600 in each 10 s an app opened, freed only at a memory pressure rise. Those are Turnip's command
// stream BOs that metal2vulkan's spare command pools keep (userland/metal/m2v-spare-command-pools.patch, docs/gpu.md
// "Who keeps a blob"). This line says how many command buffers the process's pools hold and how many the runtime let
// go, so the next boot shows whether the patch bounds them:
//   oneios-gpu-text: backboardd[64] CP 9/120 cut +2/80 idle +1/12 tuc 11M
// CP live pools / command buffers made from them; cut: pools released since the last row for holding past their recent
// use, with their command buffers; idle: pools the idle trim released, with theirs; tuc: Turnip's cache of freed BOs now.
// Every 60 s, only when the command buffers moved by 32 or more since the last row, or a pool went: the kernel prints at
// most 2000 text rows a boot (0039), and a quiet process sends none. Each row is about 60 marker calls, once a minute.
//
// The GPU memory row (2026-11-23, session mac-int): backboardd's 917 MiB and WebKit's GPU process's 475 MiB of own GPU
// blobs were mostly 5-18 blobs of 32 MiB and more each, and nothing said whether they were pooled blocks with little
// placed in them, dedicated images, or spares of images gone. metal2vulkan's census (userland/metal/m2v-idle-memory.patch,
// m2v_memory_census, as of its last idle trim, every 2 s) says it:
//   oneios-gpu-text: backboardd[64] GM p 7/448M u 210M e 2/128M d 11/400M s 3/150M bo -78M
// p: pooled blocks with a placement / their MiB; u: the MiB placed in them; e: kept empty pooled blocks / MiB; d: dedicated
// blocks / MiB (a placement past half a block, an image apart); s: spare dedicated image blocks / MiB (an image gone,
// kept 10 s for the next of its size); bo: MiB of Turnip's cached BOs the idle trim gave back since the last row. Every
// 60 s, only when a figure moved by 8 MiB or more (or the trim gave some back).
//
// The render pass row, every 5 minutes where render encoders ended (the same census, its last four words): on a tiler
// each render pass instance loads and stores its attachments' tiles, so a pass split into several costs a store and a
// load per split. metal2vulkan makes one Vulkan render pass per Metal render encoder and splits it only where Metal's
// order needs it (render.rs, "Instances"); this row says how often that happens on the phone:
//   oneios-gpu-text: backboardd[64] RP enc +36000 inst +36100 split +100 barrier +0
// enc: render encoders ended; inst: their render pass instances begun; split: the splits of their passes (inst - enc is
// about split, plus resolve-only instances); barrier: the splits at a memory barrier of the program's.
//
// The idle trim of Turnip's BO cache (userland/metal/turnip-idle-bo-cache.patch): every 60 s, the cached BOs no allocation
// took for 30 s go back to the host (tu_darwin_bo_cache_trim_idle). The cache went back only past its budgets (96 + 16
// MiB in the render server, 16 + 8 elsewhere) or at a memory pressure rise, so a still render server kept it full (tuc
// 78M at a still screen, mac-int). A busy process takes its BOs back within seconds, so the trim reaches only a still one.
//
// The switches (docs/switches.md): the boot argument oneios-pool-cut=0 (xnuhost.conf boot-args-add=oneios-pool-cut=0)
// sets METAL2VULKAN_POOL_USE_WINDOW_S=0 in every process the plug-in loads in, before any device is made, so the runtime
// keeps its pools as before; oneios-idle-spares=0 sets METAL2VULKAN_IDLE_SPARES=0 (the spares age only as before);
// oneios-bo-cache-idle=0: no trim of Turnip's cache. The rows stay either way.
#include <atomic>
#include <stdlib.h>
#include <string.h>
#include <sys/sysctl.h>
#include <thread>

// Inside the plug-in's own namespace (its Gpu, makeGpu, streamText and turnip::libraryPath are publish::'s); the
// extern "C" name is the one declared at the top of the file.
namespace publish {

// Whether the kernel's boot-args hold `word` as a whole word.
static bool oneiosBootWord(const char *args, const char *word) {
    const size_t length = strlen(word);
    for (const char *at = strstr(args, word); at; at = strstr(at + 1, word)) {
        const char end = at[length];
        if ((at == args || at[-1] == ' ') && (end == 0 || end == ' ')) return true;
    }
    return false;
}

static bool gOneiosBoCacheIdle = true;

__attribute__((constructor)) static void oneiosPoolCutSwitch() {
    char args[1024] = {};
    size_t size = sizeof(args) - 1;
    if (sysctlbyname("kern.bootargs", args, &size, nullptr, 0)) return;
    if (oneiosBootWord(args, "oneios-pool-cut=0")) setenv("METAL2VULKAN_POOL_USE_WINDOW_S", "0", 1);
    if (oneiosBootWord(args, "oneios-idle-spares=0")) setenv("METAL2VULKAN_IDLE_SPARES", "0", 1);
    if (oneiosBootWord(args, "oneios-bo-cache-idle=0")) gOneiosBoCacheIdle = false;
}

extern "C" size_t m2v_command_pool_census(uint64_t *out, size_t count) __attribute__((weak));
extern "C" size_t m2v_memory_census(uint64_t *out, size_t count) __attribute__((weak));

static unsigned long long oneiosMiB(uint64_t bytes) { return (unsigned long long)((bytes + (1 << 19)) >> 20); }

extern "C" void oneios_gpu_pool_row_start(unsigned port) {
    if (!&m2v_command_pool_census) return;
    static std::atomic<bool> started{false};
    if (started.exchange(true)) return;
    Gpu *gpu = makeGpu(port);
    if (!gpu) return;
    std::thread([gpu]{
        pthread_setname_np("pool-reporter");
        void (*cacheStats)(uint64_t *) = nullptr;
        uint64_t (*cacheTrim)(uint64_t) = nullptr;
        uint64_t shown[6] = {}, memoryShown[16] = {}, passShown[4] = {}, trimmed = 0;
        for (unsigned minute = 1;; ++minute) {
            sleep(60);
            if (!cacheStats) {
                char path[1024];
                if (turnip::libraryPath(path, sizeof path))
                    if (void *library = dlopen(path, RTLD_NOW | RTLD_NOLOAD)) {
                        cacheStats = (void (*)(uint64_t *))dlsym(library, "tu_darwin_bo_cache_stats");
                        cacheTrim = (uint64_t (*)(uint64_t))dlsym(library, "tu_darwin_bo_cache_trim_idle");
                    }
            }
            if (cacheTrim && gOneiosBoCacheIdle) trimmed += cacheTrim(30000);
            uint64_t now[6] = {};
            if (m2v_command_pool_census(now, 6) != 6) continue;
            const uint64_t moved = now[1] > shown[1] ? now[1] - shown[1] : shown[1] - now[1];
            if (moved >= 32 || now[2] != shown[2] || now[4] != shown[4]) {
                uint64_t cache[8] = {};
                if (cacheStats) cacheStats(cache);
                char row[64];
                snprintf(row, sizeof row, "CP %llu/%llu cut +%llu/%llu idle +%llu/%llu tuc %lluM", (unsigned long long)now[0],
                         (unsigned long long)now[1], (unsigned long long)(now[2] - shown[2]), (unsigned long long)(now[3] - shown[3]),
                         (unsigned long long)(now[4] - shown[4]), (unsigned long long)(now[5] - shown[5]),
                         (unsigned long long)(cache[5] >> 20));
                streamText(*gpu, row);
                memcpy(shown, now, sizeof shown);
            }
            uint64_t memory[16] = {};
            if (!&m2v_memory_census || m2v_memory_census(memory, 16) != 16) continue;
            // Every 5 minutes, where encoders ended: the passes since the last row (words 12-15).
            if (minute % 5 == 0 && memory[12] != passShown[0]) {
                char row[64];
                snprintf(row, sizeof row, "RP enc +%llu inst +%llu split +%llu barrier +%llu",
                         (unsigned long long)(memory[12] - passShown[0]), (unsigned long long)(memory[13] - passShown[1]),
                         (unsigned long long)(memory[14] - passShown[2]), (unsigned long long)(memory[15] - passShown[3]));
                streamText(*gpu, row);
                memcpy(passShown, memory + 12, sizeof passShown);
            }
            // The byte words: pooled, placed, empty, dedicated, spare.
            static const int bytes[] = {1, 2, 4, 6, 8};
            bool movedBytes = trimmed >= (1 << 20);
            for (int at : bytes) {
                const uint64_t a = memory[at], b = memoryShown[at];
                if ((a > b ? a - b : b - a) >= (8 << 20)) movedBytes = true;
            }
            if (!movedBytes) continue;
            char row[64];
            snprintf(row, sizeof row, "GM p %llu/%lluM u %lluM e %llu/%lluM d %llu/%lluM s %llu/%lluM bo -%lluM",
                     (unsigned long long)memory[0], oneiosMiB(memory[1]), oneiosMiB(memory[2]), (unsigned long long)memory[3],
                     oneiosMiB(memory[4]), (unsigned long long)memory[5], oneiosMiB(memory[6]), (unsigned long long)memory[7],
                     oneiosMiB(memory[8]), oneiosMiB(trimmed));
            streamText(*gpu, row);
            memcpy(memoryShown, memory, sizeof memoryShown);
            trimmed = 0;
        }
    }).detach();
}

}  // namespace publish
