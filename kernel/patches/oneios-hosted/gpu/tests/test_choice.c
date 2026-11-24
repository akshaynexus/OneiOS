// SPDX-License-Identifier: MIT
// Host test of gpu/OneiOSGpuChoice.h (kernel/tests/host.sh): which accelerator the guest publishes for each host and
// boot argument, the CPU-composition list's matcher, the discovery keys and the A56 accelerator's open types.
// Above all: an MSM capset (or none read) without oneios-gpu-amd gives what every boot had before (the S24 Ultra), and
// an AMDGPU capset never gives the Adreno accelerator.
#include "../OneiOSGpuChoice.h"
#include <stdio.h>

static int failures;
#define CHECK(cond)                                                              \
    do {                                                                         \
        if (!(cond)) {                                                           \
            fprintf(stderr, "test_choice.c:%d: %s\n", __LINE__, #cond);          \
            ++failures;                                                          \
        }                                                                        \
    } while (0)

// The rule before the A56 port (oneios_adreno_wanted): state 3, or oneios-gpu-metal / droidvm-gpu-metal when given.
static unsigned before(unsigned state, int metal)
{
    const unsigned wanted = metal >= 0 ? metal != 0 : state == 3;
    return wanted ? ONEIOS_ACCEL_ADRENO : ONEIOS_ACCEL_NONE;
}

int main(void)
{
    unsigned why = 99;
    // Unchanged where the capset is MSM or not read, and oneios-gpu-amd is absent or 0: every state and metal value.
    for (unsigned state = 0; state <= 3; ++state)
        for (int metal = -1; metal <= 1; ++metal)
            for (int amd = -1; amd <= 0; ++amd) {
                CHECK(oneios_gpu_accelerator_choice(state, ONEIOS_DRM_CONTEXT_MSM, metal, amd, &why) == before(state, metal));
                CHECK(oneios_gpu_accelerator_choice(state, ONEIOS_DRM_CONTEXT_UNKNOWN, metal, amd, &why) == before(state, metal));
                // Not offering 3D, the capset is never read; were it, a non-MSM one changes nothing below state 3.
                if (state != 3) CHECK(oneios_gpu_accelerator_choice(state, ONEIOS_DRM_CONTEXT_AMDGPU, metal, amd, &why) == before(state, metal));
            }
    CHECK(oneios_gpu_accelerator_choice(3, ONEIOS_DRM_CONTEXT_MSM, -1, -1, &why) == ONEIOS_ACCEL_ADRENO && why == ONEIOS_WHY_3D);
    CHECK(oneios_gpu_accelerator_choice(2, ONEIOS_DRM_CONTEXT_UNKNOWN, 1, -1, &why) == ONEIOS_ACCEL_ADRENO && why == ONEIOS_WHY_METAL_ARG);
    CHECK(oneios_gpu_accelerator_choice(3, ONEIOS_DRM_CONTEXT_MSM, 0, -1, &why) == ONEIOS_ACCEL_NONE && why == ONEIOS_WHY_METAL_ARG);
    CHECK(oneios_gpu_accelerator_choice(2, ONEIOS_DRM_CONTEXT_UNKNOWN, -1, -1, &why) == ONEIOS_ACCEL_NONE && why == ONEIOS_WHY_NO_3D);

    // An AMDGPU capset (or any other that is not MSM) without oneios-gpu-amd=1: nothing, whatever oneios-gpu-metal says.
    for (int metal = -1; metal <= 1; ++metal)
        for (int amd = -1; amd <= 0; ++amd) {
            CHECK(oneios_gpu_accelerator_choice(3, ONEIOS_DRM_CONTEXT_AMDGPU, metal, amd, &why) == ONEIOS_ACCEL_NONE);
            CHECK(why == ONEIOS_WHY_NOT_MSM);
            CHECK(oneios_gpu_accelerator_choice(3, 3, metal, amd, &why) == ONEIOS_ACCEL_NONE);
        }

    // oneios-gpu-amd=1: the A56 accelerator on an AMDGPU capset, with 20F66's oneios-gpu-metal=0 too; never Adreno.
    for (int metal = -1; metal <= 1; ++metal) {
        CHECK(oneios_gpu_accelerator_choice(3, ONEIOS_DRM_CONTEXT_AMDGPU, metal, 1, &why) == ONEIOS_ACCEL_RADEON && why == ONEIOS_WHY_AMD);
        CHECK(oneios_gpu_accelerator_choice(3, ONEIOS_DRM_CONTEXT_MSM, metal, 1, &why) == ONEIOS_ACCEL_NONE && why == ONEIOS_WHY_AMD_NOT_AMDGPU);
        CHECK(oneios_gpu_accelerator_choice(3, ONEIOS_DRM_CONTEXT_UNKNOWN, metal, 1, &why) == ONEIOS_ACCEL_NONE);
        for (unsigned state = 0; state < 3; ++state)
            CHECK(oneios_gpu_accelerator_choice(state, ONEIOS_DRM_CONTEXT_AMDGPU, metal, 1, &why) == ONEIOS_ACCEL_NONE &&
                  why == ONEIOS_WHY_AMD_NOT_AMDGPU);
    }
    CHECK(oneios_gpu_accelerator_choice(3, ONEIOS_DRM_CONTEXT_AMDGPU, -1, 1, 0) == ONEIOS_ACCEL_RADEON);   // why may be null

    // The CPU-composition list.
    const char *list = ONEIOS_GPU_AMD_CPU_DEFAULT;
    CHECK(oneios_gpu_name_listed(list, "backboardd"));
    CHECK(oneios_gpu_name_listed(list, "SpringBoard"));
    CHECK(oneios_gpu_name_listed(list, "MobileSlideShow"));
    CHECK(oneios_gpu_name_listed(list, "SBRendererService"));
    CHECK(oneios_gpu_name_listed(list, "SBRendererServic"));    // the 16-byte command alias
    CHECK(oneios_gpu_name_listed(list, "CollectionsPost"));     // 15 characters
    CHECK(!oneios_gpu_name_listed(list, "SBRenderer"));         // a short start is another name
    CHECK(!oneios_gpu_name_listed(list, "backboard"));
    CHECK(!oneios_gpu_name_listed(list, "backboardd2"));
    CHECK(!oneios_gpu_name_listed(list, "MobileSafari"));
    CHECK(!oneios_gpu_name_listed(list, "iconservicesagent"));
    CHECK(!oneios_gpu_name_listed(list, ""));
    CHECK(!oneios_gpu_name_listed("", "backboardd"));          // oneios-gpu-amd-cpu= (empty): no process keeps the CPU
    CHECK(oneios_gpu_name_listed("*", "anything"));
    CHECK(oneios_gpu_name_listed("com.apple.WebKit*,x", "com.apple.WebKit.GPU"));
    CHECK(!oneios_gpu_name_listed("com.apple.WebKit*,x", "com.apple.Web"));
    CHECK(oneios_gpu_name_listed("a,,b", "b"));
    CHECK(!oneios_gpu_name_listed(0, "b") && !oneios_gpu_name_listed("b", 0));

    // The discovery keys.
    CHECK(oneios_gpu_discovery_key("MetalPluginName"));
    CHECK(oneios_gpu_discovery_key("MetalPluginClassName"));
    CHECK(oneios_gpu_discovery_key("IOGLESBundleName"));
    CHECK(oneios_gpu_discovery_key("IOGLESMetalBundleName"));
    CHECK(oneios_gpu_discovery_key("IOGLESDefaultUseMetal"));
    CHECK(!oneios_gpu_discovery_key("MetalPluginNam"));
    CHECK(!oneios_gpu_discovery_key("MetalPluginNameX"));
    CHECK(!oneios_gpu_discovery_key("IOClass"));
    CHECK(!oneios_gpu_discovery_key(0));

    // The A56 accelerator's open types.
    CHECK(oneios_gpu_radeon_open_allowed(0x4149));
    CHECK(oneios_gpu_radeon_open_allowed(0x41476800));   // a progress marker
    CHECK(oneios_gpu_radeon_open_allowed(0x414770cc));   // a text row
    CHECK(!oneios_gpu_radeon_open_allowed(0x4147));      // NativeAdreno's core
    CHECK(!oneios_gpu_radeon_open_allowed(0x41470000));
    CHECK(!oneios_gpu_radeon_open_allowed(0x4148));      // the display hand-off
    CHECK(!oneios_gpu_radeon_open_allowed(1));           // IOGPU's device client
    CHECK(!oneios_gpu_radeon_open_allowed(0));

    if (failures) {
        fprintf(stderr, "test_choice: %d failed\n", failures);
        return 1;
    }
    printf("test_choice: the accelerator's choice, the CPU list, the discovery keys and the open types\n");
    return 0;
}
