// SPDX-License-Identifier: MIT
// OneiOS, hosted kernel: the start of the guest's 3D GPU driver.
//
// The driver itself is Droid-VM's, unchanged (UniXNU-droidvm package/, BSD-2-Clause-Patent), compiled into this
// archive by build-oneios-gpu.py:
//   DroidVMVirtioGpu.cpp     the virtio-gpu transport on /arm-io/virtio-gpu: one control queue, many commands in flight
//   DroidVMVirtioGpu3D.cpp   the DRM native context (capset 6), blobs over the guest's pages, fenced submissions,
//                            and droidvm_virtgpu_call: the DROIDVM_VIRTGPU_* interface (NativeAdreno
//                            DroidVMVirtgpuABI.h) that the accelerator's user client gives Turnip's vdrm
// The device is xnuhost's (runtime/xnuhost/src/virtio_gpu.rs with drm2kgsl on /dev/kgsl-3d0), which answers as
// crosvm's does. Called once from oneios_board_late_start (0005-gpu-3d.patch), after the IOKit bootstrap.
//
// Boot arguments: vgpu-3d=0 leaves the 3D half alone (the display driver may still start the transport);
// vgpu-selftest=1 runs one context through the host at boot (capset, context, its response buffer from the pool,
// GET_PARAM answered there) and logs each stage.
//
// Then Metal's kernel side: oneios_adreno_start (OneiOSAdreno.cpp), NativeAdreno's accelerator and user client over
// droidvm_virtgpu_call. Compiled out with ONEIOS_GPU_ADRENO=0 (build-oneios-gpu.py, when its objects are not linked).
//
// With the 3D device there, the DRM capset's context type is read first (a56-gpu, 2026-11-24): 1 MSM (drm2kgsl, the
// Adreno), 2 AMDGPU (xnuhost's amdgpu renderer on the Galaxy A56's Xclipse). OneiOSAdreno.cpp publishes the Adreno
// accelerator on MSM only, and the A56's on AMDGPU with oneios-gpu-amd=1 (gpu/OneiOSGpuChoice.h). Two read-only
// queries the host answers as it does for every Metal process's vdrm; a line only when the type is not MSM.
#include "DroidVMVirtioGpu.h"
#include <pexpert/pexpert.h>

extern "C" {
// 0 not started; 1 no device or no transport; 2 no 3D feature offered (the host has no KGSL: 2D only);
// 3 the 3D device is there. For a debugger and for the board's diagnostics.
volatile uint32_t oneios_gpu3d_state;
// The DRM capset's (capset 6) context_type, read at the start when the state is 3: 1 MSM, 2 AMDGPU; 0 not read.
volatile uint32_t oneios_gpu3d_context_type;
extern volatile uint64_t droidvm_virtgpu_selftest[14];
void oneios_gpu3d_start(void);
void oneios_adreno_start(void);
}

namespace {
// struct virgl_renderer_capset_drm's context_type: its fifth word (wire_format, version_major, _minor, _patch first).
// 0 when the host lists no capset 6 or does not answer; the queries are GET_CAPSET_INFO by index and GET_CAPSET, as
// DroidVMVirtioGpu3D.cpp's own (it keeps its answers to itself).
uint32_t drmContextType(uint32_t *bytesOut)
{
    using namespace DroidVMVirtioGpu;
    enum : uint32_t { GetCapsetInfo = 0x108, GetCapset = 0x109, CapsetInfoOk = 0x1102, CapsetOk = 0x1103, CapsetDrm = 6 };
    struct InfoAsk { Header h; uint32_t index, padding; };
    struct Info { Header h; uint32_t id, maxVersion, maxSize, padding; };
    struct CapsetAsk { Header h; uint32_t id, version; };
    uint32_t bytes = 0;
    for (uint32_t index = 0; index < 16 && !bytes; ++index) {
        InfoAsk ask = {{GetCapsetInfo, 0, 0, 0, 0, {}}, index, 0};
        Info info = {};
        if (command(&ask, sizeof ask, nullptr, 0, &info, sizeof info) || info.h.type != CapsetInfoOk) break;
        if (info.id == CapsetDrm) bytes = info.maxSize;
    }
    *bytesOut = bytes;
    static uint8_t answer[4096];   // once, at the start; the device answers with the whole capset
    if (bytes < 20 || bytes > sizeof answer - sizeof(Header)) return 0;
    CapsetAsk ask = {{GetCapset, 0, 0, 0, 0, {}}, CapsetDrm, 0};
    if (command(&ask, sizeof ask, nullptr, 0, answer, uint32_t(sizeof(Header) + bytes)) ||
        reinterpret_cast<const Header *>(answer)->type != CapsetOk) return 0;
    uint32_t type;
    memcpy(&type, answer + sizeof(Header) + 16, sizeof type);
    return type;
}
}  // namespace

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
    uint32_t capsetBytes = 0;
    oneios_gpu3d_context_type = drmContextType(&capsetBytes);
    if (oneios_gpu3d_context_type != 1)
        IOLog("oneios-gpu: DRM capset (%u bytes) context type %u (%s): %s\n", capsetBytes, oneios_gpu3d_context_type,
              oneios_gpu3d_context_type == 2 ? "AMDGPU" : oneios_gpu3d_context_type ? "not MSM" : "not read",
              oneios_gpu3d_context_type ? "no Adreno accelerator; the A56's with oneios-gpu-amd=1 on AMDGPU"
                                        : "the accelerator's rule as before");
    DroidVMVirtioGpu::selftest3D();                  // only with vgpu-selftest=1
    if (droidvm_virtgpu_selftest[0])
        IOLog("oneios-gpu: selftest stage %llu status %#llx, gpu id %llu, context type %llu, pool offset %#llx\n", droidvm_virtgpu_selftest[0],
              droidvm_virtgpu_selftest[1], droidvm_virtgpu_selftest[2], droidvm_virtgpu_selftest[3], droidvm_virtgpu_selftest[4]);
    adreno_start();
}
