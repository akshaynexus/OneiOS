// SPDX-License-Identifier: MIT
// OneiOS, hosted kernel: which Metal accelerator the guest publishes, from what the host's virtio GPU offers and the
// boot arguments. Pure functions (no kernel type), so kernel/tests/host.sh builds and runs them on the build host
// (gpu/tests/test_choice.c); OneiOSAdreno.cpp calls them at the start.
//
// The host's DRM native context (capset 6, struct virgl_renderer_capset_drm) names its driver in context_type:
// 1 MSM (the Adreno: xnuhost's drm2kgsl on KGSL, the S24 Ultra), 2 AMDGPU (the Galaxy A56's Xclipse 540: xnuhost's
// amdgpu renderer). OneiOSGpu.cpp reads it at the start when the host offers 3D (0: not read).
//
//   ADRENO  NativeAdreno's AGXAccelerator, MetalPluginName NativeAdrenoMetal (Turnip over msm): what every boot had
//           before. Rule unchanged on an MSM capset (or one not read) and without oneios-gpu-amd=1: published when the
//           host offers 3D (gpu state 3); oneios-gpu-metal=0 never, =1 always (the PC with no 3D, a test).
//   RADEON  the same AGXAccelerator class and user client with MetalPluginName OneiOSRadeonMetal and
//           MetalPluginClassName OneiOSRadeonMetalDevice (the A56's Metal plug-in, docs/a56-gpu.md 4 and 5), serving
//           open type 0x4149 (the virtgpu methods) alone. Only with oneios-gpu-amd=1 (xnuhost adds it when, and only
//           when, it picks its amdgpu renderer: K5) and an AMDGPU capset. oneios-gpu-metal does not apply to it: that
//           word is against the Adreno accelerator (iOS 16.5's stock Metal opens it as an IOGPU device, type 1, which
//           NativeAdreno refuses; roottweak writes =0 into every 20F66 image's xnuhost.conf, which the A56 keeps).
//   NONE    nothing published.
// Never ADRENO on a capset whose context type is read and is not MSM (whatever oneios-gpu-metal says): Turnip asks
// capset 6 for an msm context and fails there, and on iOS 16.5 the published accelerator handed backboardd a Metal
// device that could not draw. Never ADRENO with oneios-gpu-amd=1 either: the host said its GPU is an AMD one.
#ifndef ONEIOS_GPU_CHOICE_H
#define ONEIOS_GPU_CHOICE_H

enum {
    ONEIOS_DRM_CONTEXT_UNKNOWN = 0,   // not read (no 3D, no DRM capset, or the query failed)
    ONEIOS_DRM_CONTEXT_MSM = 1,       // VIRTGPU_DRM_CONTEXT_MSM
    ONEIOS_DRM_CONTEXT_AMDGPU = 2,    // VIRTGPU_DRM_CONTEXT_AMDGPU
};
enum { ONEIOS_ACCEL_NONE = 0, ONEIOS_ACCEL_ADRENO = 1, ONEIOS_ACCEL_RADEON = 2 };
// Why (for the start's log line).
enum {
    ONEIOS_WHY_3D = 0,               // ADRENO: the host offers 3D (MSM capset or not read)
    ONEIOS_WHY_METAL_ARG,            // ADRENO or NONE: oneios-gpu-metal (or droidvm-gpu-metal) decided
    ONEIOS_WHY_NO_3D,                // NONE: the host offers no 3D and no oneios-gpu-metal=1
    ONEIOS_WHY_NOT_MSM,              // NONE: the capset's context type is not MSM and there is no oneios-gpu-amd=1
    ONEIOS_WHY_AMD,                  // RADEON: oneios-gpu-amd=1 and an AMDGPU capset
    ONEIOS_WHY_AMD_NOT_AMDGPU,       // NONE: oneios-gpu-amd=1 but no 3D or a capset that is not AMDGPU
};

// gpu_state: oneios_gpu3d_state (3: the 3D device is there). context_type: the capset's, 0 not read. metal: the value
// of oneios-gpu-metal (else droidvm-gpu-metal), -1 when neither is given. amd: the value of oneios-gpu-amd, -1 when
// not given (0 is the same as not given).
static inline unsigned oneios_gpu_accelerator_choice(unsigned gpu_state, unsigned context_type, int metal, int amd, unsigned *why)
{
    unsigned w;
    unsigned kind;
    if (amd > 0) {
        const int amdgpu = gpu_state == 3 && context_type == ONEIOS_DRM_CONTEXT_AMDGPU;
        kind = amdgpu ? ONEIOS_ACCEL_RADEON : ONEIOS_ACCEL_NONE;
        w = amdgpu ? ONEIOS_WHY_AMD : ONEIOS_WHY_AMD_NOT_AMDGPU;
    } else if (gpu_state == 3 && context_type != ONEIOS_DRM_CONTEXT_UNKNOWN && context_type != ONEIOS_DRM_CONTEXT_MSM) {
        kind = ONEIOS_ACCEL_NONE;
        w = ONEIOS_WHY_NOT_MSM;
    } else if (metal >= 0) {
        kind = metal ? ONEIOS_ACCEL_ADRENO : ONEIOS_ACCEL_NONE;
        w = ONEIOS_WHY_METAL_ARG;
    } else {
        kind = gpu_state == 3 ? ONEIOS_ACCEL_ADRENO : ONEIOS_ACCEL_NONE;
        w = gpu_state == 3 ? ONEIOS_WHY_3D : ONEIOS_WHY_NO_3D;
    }
    if (why) *why = w;
    return kind;
}

// Whether a process name is on a list of the RADEON accelerator's CPU-composition processes (oneios-gpu-amd-cpu):
// "a,b,c", an entry "X*" a prefix, "*" every process, "" none. `name` is proc_selfname's: the executable's name (up to
// 32 characters) or the kernel's 16-byte command alias, so an entry also matches a name of 15 characters or more that
// is the entry's start (SBRendererService as "SBRendererServic", CollectionsPoster as "CollectionsPost").
static inline int oneios_gpu_name_listed(const char *list, const char *name)
{
    if (!list || !name || !*name) return 0;
    unsigned named = 0;
    while (name[named]) ++named;
    for (const char *entry = list; *entry;) {
        unsigned length = 0;
        while (entry[length] && entry[length] != ',') ++length;
        if (length) {
            const int prefix = entry[length - 1] == '*';
            const unsigned want = prefix ? length - 1 : length;
            unsigned same = 0;
            while (same < want && same < named && entry[same] == name[same]) ++same;
            if (prefix && same == want) return 1;                         // "X*" and "*"
            if (!prefix && same == want && same == named) return 1;       // the whole name
            if (!prefix && named >= 15 && same == named && named < want) return 1;   // the command alias of a long name
        }
        entry += length;
        if (*entry == ',') ++entry;
    }
    return 0;
}

// The registry keys Metal's and OpenGL ES's discovery read off an IOAcceleratorES (Jesvi-iOS's
// redwood_graphics_policy.h names the same five): hidden from a CPU-composition process, that process finds no Metal
// plug-in and no GL driver and draws on the CPU, as with no accelerator at all.
static inline int oneios_gpu_discovery_key(const char *key)
{
    static const char *const keys[] = {"MetalPluginName", "MetalPluginClassName", "IOGLESBundleName", "IOGLESMetalBundleName",
                                       "IOGLESDefaultUseMetal"};
    if (!key) return 0;
    for (unsigned i = 0; i < sizeof keys / sizeof *keys; ++i) {
        const char *a = keys[i], *b = key;
        while (*a && *a == *b) ++a, ++b;
        if (!*a && !*b) return 1;
    }
    return 0;
}

// The RADEON accelerator's open types: 0x4149 (DROIDVM_VIRTGPU_OPEN_TYPE: the virtgpu methods, one DRM context a
// connection) and NativeAdreno's progress markers 0x4147xxxx (xxxx nonzero: no connection is made; the plug-in's text
// rows, oneios-gpu-text). Every other type is refused: 0x4147 (NativeAdreno's GPU core, KGSL-shaped), 0x4148 (the
// display hand-off), 1 (iOS's IOGPU device client), anything else.
static inline int oneios_gpu_radeon_open_allowed(unsigned type)
{
    return type == 0x4149u || ((type >> 16) == 0x4147u && (type & 0xffffu));
}

// The CPU-composition processes by default (oneios-gpu-amd-cpu not given): Jesvi-iOS's list of "core renderers" that
// keep their established path on its iOS 16.5 Metal (redwood_graphics_policy.h, a13e406), with the command aliases it
// names folded into the matcher above. backboardd heads it: CoreAnimation's render server on that Metal crashed at
// start there (its docs/devices/x1slte/display.md line 72) and was reverted; the rest had their own faults there
// (mediaserverd's stack overflow, PosterBoard's text, Photos).
#define ONEIOS_GPU_AMD_CPU_DEFAULT \
    "backboardd,SpringBoard,mediaserverd,PosterBoard,CollectionsPoster,WeatherPoster,SBRendererService,MobileSlideShow"

#endif
