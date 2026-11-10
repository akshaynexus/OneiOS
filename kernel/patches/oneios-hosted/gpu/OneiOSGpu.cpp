// SPDX-License-Identifier: MIT
// OneiOS, hosted kernel: the start of the guest's 3D GPU driver.
//
// The driver itself is Droid-VM's, unchanged (UniXNU-droidvm package/, BSD-2-Clause-Patent), compiled into this
// archive by build-oneios-gpu.py:
//   DroidVMVirtioGpu.cpp     the virtio-gpu transport on /arm-io/virtio-gpu: one control queue, many commands in flight
//   DroidVMVirtioGpu3D.cpp   the DRM native context (capset 6), blobs over the guest's pages, fenced submissions,
//                            and droidvm_virtgpu_call: the DROIDVM_VIRTGPU_* interface (NativeAdreno
//                            DroidVMVirtgpuABI.h) that the accelerator's user client gives Turnip's vdrm
// The device is xnuhost's (runtime/xnuhost/src/virtio_gpu.c with drm2kgsl on /dev/kgsl-3d0), which answers as
// crosvm's does. Called once from oneios_board_late_start (0005-gpu-3d.patch), after the IOKit bootstrap.
//
// Boot arguments: vgpu-3d=0 leaves the 3D half alone (the display driver may still start the transport);
// vgpu-selftest=1 runs one context through the host at boot (capset, context, its response buffer from the pool,
// GET_PARAM answered there) and logs each stage.
//
// Then Metal's kernel side: oneios_adreno_start (OneiOSAdreno.cpp), NativeAdreno's accelerator and user client over
// droidvm_virtgpu_call. Compiled out with ONEIOS_GPU_ADRENO=0 (build-oneios-gpu.py, when its objects are not linked).
#include "DroidVMVirtioGpu.h"
#include <pexpert/pexpert.h>

extern "C" {
// 0 not started; 1 no device or no transport; 2 no 3D feature offered (the host has no KGSL: 2D only);
// 3 the 3D device is there. For a debugger and for the board's diagnostics.
volatile uint32_t oneios_gpu3d_state;
extern volatile uint64_t droidvm_virtgpu_selftest[14];
void oneios_gpu3d_start(void);
void oneios_adreno_start(void);
}

#ifndef ONEIOS_GPU_ADRENO
#define ONEIOS_GPU_ADRENO 1
#endif
static void adreno_start(void)
{
#if ONEIOS_GPU_ADRENO
    oneios_adreno_start();
#endif
}

void oneios_gpu3d_start(void)
{
    uint32_t enabled = 1;
    PE_parse_boot_argn("vgpu-3d", &enabled, sizeof enabled);
    if (!enabled) return;
    if (!DroidVMVirtioGpu::start()) {
        oneios_gpu3d_state = 1;
        IOLog("oneios-gpu: no virtio GPU (no /arm-io/virtio-gpu, or the device did not start)\n");
        adreno_start();
        return;
    }
    // VIRGL, RESOURCE_BLOB, CONTEXT_INIT and CREATE_GUEST_HANDLE: what xnuhost offers only with its renderer open.
    const uint64_t need = 1u << 0 | 1u << 3 | 1u << 4 | 1u << 6, have = DroidVMVirtioGpu::features();
    if ((have & need) != need) {
        oneios_gpu3d_state = 2;
        IOLog("oneios-gpu: the host offers no 3D (features %#llx): display only\n", have);
        adreno_start();
        return;
    }
    oneios_gpu3d_state = 3;
    IOLog("oneios-gpu: virtio GPU with the DRM native context (features %#llx)\n", have);
    DroidVMVirtioGpu::selftest3D();                  // only with vgpu-selftest=1
    if (droidvm_virtgpu_selftest[0])
        IOLog("oneios-gpu: selftest stage %llu status %#llx, gpu id %llu, context type %llu, pool offset %#llx\n", droidvm_virtgpu_selftest[0],
              droidvm_virtgpu_selftest[1], droidvm_virtgpu_selftest[2], droidvm_virtgpu_selftest[3], droidvm_virtgpu_selftest[4]);
    adreno_start();
}
