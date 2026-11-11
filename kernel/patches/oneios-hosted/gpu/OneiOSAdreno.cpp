// SPDX-License-Identifier: MIT
// OneiOS, hosted kernel: NativeAdreno's Metal face on the virtio GPU, bound to the iPhone18,2 kernel collection.
//
// What is linked (build-oneios-gpu.py): NativeAdreno's user client, IOKit surface, execbuf, memory and PM4 encoder
// with Droid-VM's edits, Droid-VM's package/DroidVMGpu.cpp (the hardware classes never attached, the publish, the
// accelerator's registration) and the staged AGXAccelerator (NativeAdrenoAccelerator.cpp), a subclass of the
// collection's IOAcceleratorES (com.apple.iokit.IOGPUFamily). Apple's AGXG18P is not in the collection, so the
// class name AGXAccelerator and the user client's name AGXDeviceUserClient are free.
//
// The parent is reached as the software AES reaches IOAESAccelerator (../aes/OneiOSSoftwareAES.cpp): its
// constructor and destructor through generated stubs `bti c; b <name>.original`, which XnuPrelink retargets, and
// its metaclass as an absolute symbol whose value is the link address. Under xnuhost the collection runs slid, so
//   - oneios_adreno_parent_bound: the live IOAcceleratorES must be the bound one (link address + slide, 136 bytes);
//   - oneios_adreno_parent_slide: AGXAccelerator's metaclass, constructed with the link address as its superclass,
//     gets the live one before OSMetaClass::postModLoad reads it.
// DroidVMGpu.cpp calls both from its publishMetal (the build stages those two lines and the start condition).
//
// The start: oneios_adreno_start, from oneios_gpu3d_start (OneiOSGpu.cpp), which the board's late start calls in
// bsd_init. Boot arguments:
//   oneios-gpu-metal=0|1     0: nothing is published. 1: published whatever the host offers. Not given: published
//                            when the host's virtio GPU offers 3D (oneios_gpu3d_state 3). Droid-VM's
//                            droidvm-gpu-metal is read the same way when oneios-gpu-metal is absent.
//   droidvm-gpu-clients=...  the processes besides backboardd granted the user client (DroidVMGpu.cpp)
//   oneios-adreno-selftest=1 after the publish: IOKit's match of IOAcceleratorES, an in-kernel open of the Metal
//                            plug-in's connection (type 0x4149) and one GETPARAM through its tunnel; one log line
//                            with each result.
#include <IOKit/IOLib.h>
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <libkern/c++/OSMetaClass.h>
#include <kern/thread.h>
#include <kern/task.h>
#include <pexpert/pexpert.h>
#include <IOKit/IODeviceTreeSupport.h>
#include <machine/machine_routines.h>
extern "C" uint64_t gPhysBase, gPhysSize;
#include "DroidVMVirtgpuABI.h"

extern vm_offset_t vm_kernel_slide;
extern "C" {
// The generated bindings (build-oneios-gpu.py): the parent metaclass's link address and instance size as numbers.
extern const uint64_t oneios_adreno_parent_meta_link;
extern const uint32_t oneios_adreno_parent_size;
extern char oneios_adreno_accelerator_meta[] __asm__("__ZN14AGXAccelerator10gMetaClassE");
extern volatile uint32_t oneios_gpu3d_state;                   // OneiOSGpu.cpp
extern volatile uint64_t droidvm_metal_publish[3];             // DroidVMGpu.cpp: publish status, accelerator stage, status
extern volatile uint32_t droidvm_gpu_chain;                    // DroidVMGpu.cpp: which class chains were adopted
void sheng_gpu_power_probe(void);                              // DroidVMGpu.cpp: the publish, on the caller's thread (staged)

// 0 not started; 1 not wanted; 2 the publish was started; 0x81 the live IOAcceleratorES is not the bound one.
volatile uint32_t oneios_adreno_stage;
// The self-test: [0] stage (1 waiting, 2 matched, 3 opened, 4 called, 5 closed; 0x80 | n failed at n), [1] the
// open's status, [2] the call's status, [3] its answer (the virtgpu ABI version).
volatile uint64_t oneios_adreno_selftest[4];
bool oneios_adreno_wanted(void);
bool oneios_adreno_parent_bound(void);
void oneios_adreno_parent_slide(void);
void oneios_adreno_start(void);
}

// What NativeAdrenoPublish.cpp calls on sheng and on a DroidVM board, and what stands behind it there: the
// display hand-off (NativeMobileDisplay's presenter: the display archive defines these two when it is linked, and
// its definitions win over these weak ones) and the board's log export. Neither exists here.
extern "C" __attribute__((weak)) IOReturn native_gop_handoff_call(uint32_t, task_t, const void *, size_t, void *, size_t)
{
    return kIOReturnUnsupported;
}
extern "C" __attribute__((weak)) IOMemoryDescriptor *native_gop_handoff_memory(void) {return nullptr;}
extern "C" __attribute__((weak)) int boot_log_export_put(uint32_t, uint32_t, uint32_t, const void *, uint32_t) {return -1;}

// OSMetaClass as laid out in libkern/c++/OSMetaClass.h: the vtable pointer, reserved, superClassLink.
struct MetaClassHead { void *vtable; void *reserved; const OSMetaClass *superClassLink; };

static const OSMetaClass *boundParent()
{
    return reinterpret_cast<const OSMetaClass *>(oneios_adreno_parent_meta_link + vm_kernel_slide);
}

bool oneios_adreno_wanted(void)
{
    uint32_t wanted = oneios_gpu3d_state == 3;
    if (!PE_parse_boot_argn("oneios-gpu-metal", &wanted, sizeof wanted)) PE_parse_boot_argn("droidvm-gpu-metal", &wanted, sizeof wanted);
    return wanted != 0;
}

bool oneios_adreno_parent_bound(void)
{
    const OSSymbol *name = OSSymbol::withCString("IOAcceleratorES");
    const OSMetaClass *live = name ? OSMetaClass::getMetaClassWithName(name) : nullptr;
    OSSafeReleaseNULL(name);
    if (live && live == boundParent() && live->getClassSize() == oneios_adreno_parent_size) return true;
    oneios_adreno_stage = 0x81;
    IOLog("oneios-adreno: the live IOAcceleratorES (%p, %u bytes) is not the bound one (%p, %u bytes): the bindings are for "
          "another kernel collection; no accelerator\n", live, live ? live->getClassSize() : 0, boundParent(), oneios_adreno_parent_size);
    return false;
}

void oneios_adreno_parent_slide(void)
{
    MetaClassHead *meta = reinterpret_cast<MetaClassHead *>(oneios_adreno_accelerator_meta);
    if (reinterpret_cast<uint64_t>(meta->superClassLink) == oneios_adreno_parent_meta_link) meta->superClassLink = boundParent();
    if (meta->superClassLink != boundParent())
        IOLog("oneios-adreno: AGXAccelerator's superclass is %p, expected %p\n", meta->superClassLink, boundParent());
}

namespace {
void selftest(void *, wait_result_t)
{
    volatile uint64_t *st = oneios_adreno_selftest;
    st[0] = 1;
    // The publish has run by now (oneios_adreno_start): stage 5 is "registered", a status is a refusal.
    if (droidvm_metal_publish[1] != 5) {
        st[0] = 0x81;
        IOLog("oneios-adreno: selftest: not published (publish %#llx, accelerator stage %llu status %#llx)\n", droidvm_metal_publish[0],
              droidvm_metal_publish[1], droidvm_metal_publish[2]);
        return;
    }
    // What Metal does first: IOServiceMatching("IOAcceleratorES").
    OSDictionary *matching = IOService::serviceMatching("IOAcceleratorES");
    IOService *service = matching ? IOService::waitForMatchingService(matching, 10ull * NSEC_PER_SEC) : nullptr;
    OSSafeReleaseNULL(matching);
    if (!service) {
        st[0] = 0x82;
        IOLog("oneios-adreno: selftest: IOKit matched no IOAcceleratorES\n");
        return;
    }
    st[0] = 2;
    OSObject *plugin = service->copyProperty("MetalPluginName");
    OSString *pluginName = OSDynamicCast(OSString, plugin);
    IOLog("oneios-adreno: selftest: IOKit matched IOAcceleratorES: class %s (parent %s), %u bytes, MetalPluginName %s, registered %d, "
          "chains %u\n", service->getMetaClass()->getClassName(), service->getMetaClass()->getSuperClass()->getClassName(),
          service->getMetaClass()->getClassSize(), pluginName ? pluginName->getCStringNoCopy() : "(none)",
          (service->getState() & kIOServiceRegisteredState) != 0, droidvm_gpu_chain);
    OSSafeReleaseNULL(plugin);
    // The Metal plug-in's connection: open type 0x4149, then {'NTNL', 0x7200 + op} and the op's record.
    IOUserClient *client = nullptr;
    const IOReturn opened = service->newUserClient(kernel_task, nullptr, DROIDVM_VIRTGPU_OPEN_TYPE, nullptr, &client);
    st[1] = uint32_t(opened);
    if (opened || !client) {
        st[0] = 0x83;
        IOLog("oneios-adreno: selftest: the open with type %#x failed (%#x)\n", DROIDVM_VIRTGPU_OPEN_TYPE, opened);
        service->release();
        return;
    }
    st[0] = 3;
    struct {uint32_t magic, method; droidvm_virtgpu_getparam record;} request = {0x4e544e4c, DROIDVM_VIRTGPU_METHOD + DROIDVM_VIRTGPU_GETPARAM,
                                                                                {DROIDVM_VIRTGPU_PARAM_ABI_VERSION}};
    droidvm_virtgpu_getparam_reply reply = {};
    IOExternalMethodArguments arguments = {};
    arguments.version = kIOExternalMethodArgumentsCurrentVersion;
    arguments.structureInput = &request;
    arguments.structureInputSize = sizeof request;
    arguments.structureOutput = &reply;
    arguments.structureOutputSize = sizeof reply;
    const IOReturn called = client->externalMethod(0, &arguments, nullptr, nullptr, nullptr);
    st[2] = uint32_t(called);
    st[3] = reply.value;
    st[0] = 4;
    IOLog("oneios-adreno: selftest: user client %s opened with type %#x; GETPARAM(ABI_VERSION) through the tunnel: status %#x, value %llu "
          "(the 3D device is %s: gpu state %u)\n", client->getMetaClass()->getClassName(), DROIDVM_VIRTGPU_OPEN_TYPE, called, reply.value,
          oneios_gpu3d_state == 3 ? "there" : "not there", oneios_gpu3d_state);
    client->clientClose();
    client->release();
    service->release();
    st[0] = called ? 0x84 : 5;
}
}  // namespace

// The GPU addon handed over by the loader (xnuhost's <kernel>.gpuaddon sidecar, format NGPUADD2, built by
// userland/metal/build.sh): UniXNU's Data path (kern_exec.c native_gpu_addon_prepare) writes it into
// /private/var/ngpu before its addon mount, so a rebuilt plug-in does not need a new Data seed. This is
// RamBlock's native_ram_block_gpu_addon; storage/ has a weak answer of "none" for a kernel without this archive.
extern "C" int native_ram_block_gpu_addon(const uint8_t **data, uint64_t *bytes)
{
    if (!data || !bytes) return 0;
    *data = nullptr;
    *bytes = 0;
    IORegistryEntry *map = IORegistryEntry::fromPath("/chosen/memory-map", gIODTPlane);
    if (!map) return 0;
    uint64_t physical = 0, size = 0;
    if (OSData *range = OSDynamicCast(OSData, map->getProperty("GpuAddon"))) {
        if (range->getLength() == 16) {
            memcpy(&physical, range->getBytesNoCopy(), 8);
            memcpy(&size, static_cast<const uint8_t *>(range->getBytesNoCopy()) + 8, 8);
        }
    }
    map->release();
    if (!size || size > (uint64_t(64) << 20) || (physical & 4095) || physical < gPhysBase || size > gPhysSize ||
        physical - gPhysBase > gPhysSize - size) return 0;
    *data = reinterpret_cast<const uint8_t *>(ml_static_ptovirt(physical));
    if (!*data) return 0;
    *bytes = size;
    return 1;
}

// The tunnel's own counters (DroidVMVirtioGpu3D.cpp), printed as they change: whether the processes that hold a Metal
// device give the GPU work, how much, and what GPU memory they hold. Every oneios-gpu-report seconds (default 10, 0: off)
// while any changed, three lines, each short enough for the kernel's log (IOLog cuts a line at 256 bytes, which cut the
// one long line this was before 2026-11-11 just before its memory figures):
//   oneios-gpu-report: 10 s: submits +612 (fenced +600); frames +598 mean 3100 us max ever 41000 us;
//     <1/<2/<4/<8/<16/<32 ms +0/+12/+400/+150/+30/+6 longer +0; fence timeouts +0; refusals +0 (op 0 0x0)
//   oneios-gpu-memory: blobs +14 closed +9 (deferred +2, waiting 3); live 210: own 150 (64 MiB), wired 60 (32 MiB);
//     contexts +0 clients 3
//   oneios-gpu-clients: backboardd 40+20 MiB, iconservicesagent 16+0 MiB
// "frames" are backboardd's fenced submissions, one or more per frame CoreAnimation draws on the GPU, with the time from
// submission to the GPU being done. "own" blobs are pages this driver allocated (Turnip's buffers, textures): GPU memory
// that exists because of the client. "wired" blobs are the caller's own pages wired for the GPU (CREATE_BLOB_USER: an
// IOSurface or a no-copy buffer imported, often another process's window): those pages are the surface's and are counted
// again wherever the surface is. In the clients line the two are own+wired per process. A leak shows as "own" or the
// live count growing while blobs closed lag behind blobs made, through screens that come back to the same state.
extern "C" {
extern volatile uint64_t droidvm_virtgpu_stats[8], droidvm_virtgpu_rs_fences[3], droidvm_virtgpu_rs_fence_hist[10];
extern volatile uint64_t droidvm_virtgpu_live[4], droidvm_virtgpu_last_refusal[4], droidvm_virtgpu_closes[4];
struct DroidVMClientMemory { char name[16]; uint64_t pageBytes, userBytes; uint32_t sizes[8]; };
extern DroidVMClientMemory droidvm_virtgpu_client_memory[64];
}
namespace {
void report(void *, wait_result_t)
{
    uint32_t every = 10;
    PE_parse_boot_argn("oneios-gpu-report", &every, sizeof every);
    if (!every) return;
    uint64_t was[8] = {}, fences[3] = {}, hist[10] = {}, closed[2] = {}, live[4] = {};
    for (;;) {
        IOSleep(every * 1000);
        uint64_t now[8], rs[3], h[10], cl[3], lv[4];
        for (unsigned i = 0; i < 8; i++) now[i] = droidvm_virtgpu_stats[i];
        for (unsigned i = 0; i < 3; i++) rs[i] = droidvm_virtgpu_rs_fences[i];
        for (unsigned i = 0; i < 10; i++) h[i] = droidvm_virtgpu_rs_fence_hist[i];
        for (unsigned i = 0; i < 3; i++) cl[i] = droidvm_virtgpu_closes[i];
        for (unsigned i = 0; i < 4; i++) lv[i] = droidvm_virtgpu_live[i];
        bool work = rs[0] != fences[0], memory = memcmp(lv, live, sizeof lv) != 0 || cl[0] != closed[0];
        for (unsigned i = 1; i < 8; i++) work |= now[i] != was[i];
        if (work) {
            const uint64_t frames = rs[0] - fences[0];
            IOLog("oneios-gpu-report: %u s: submits +%llu (fenced +%llu); frames +%llu mean %llu us max ever %llu us; "
                  "<1/<2/<4/<8/<16/<32 ms +%llu/+%llu/+%llu/+%llu/+%llu/+%llu longer +%llu; fence timeouts +%llu; refusals +%llu (op %llu %#llx)\n",
                  every, now[4] - was[4], now[5] - was[5], frames, frames ? (rs[1] - fences[1]) / frames : 0, rs[2],
                  h[0] - hist[0], h[1] - hist[1], h[2] - hist[2], h[3] - hist[3], h[4] - hist[4], h[5] - hist[5],
                  (h[6] + h[7] + h[8] + h[9]) - (hist[6] + hist[7] + hist[8] + hist[9]), now[6] - was[6], now[7] - was[7],
                  droidvm_virtgpu_last_refusal[0], droidvm_virtgpu_last_refusal[1]);
        }
        if (work || memory) {
            IOLog("oneios-gpu-memory: blobs +%llu closed +%llu (deferred +%llu, waiting %llu); live %llu: own %llu (%llu MiB), "
                  "wired %llu (%llu MiB); contexts +%llu clients %llu\n",
                  now[2] - was[2], cl[0] - closed[0], cl[1] - closed[1], cl[2], lv[2] + lv[3], lv[2], lv[0] >> 20, lv[3], lv[1] >> 20,
                  now[1] - was[1], now[0]);
        }
        if (memory) {
            // The clients holding GPU memory, own+wired MiB, in lines of at most ~200 bytes.
            char text[200];
            size_t at = 0;
            for (unsigned i = 0; i < 64; i++) {
                const DroidVMClientMemory &c = droidvm_virtgpu_client_memory[i];
                if (!c.name[0] || !(c.pageBytes + c.userBytes)) continue;
                char one[64];
                const int n = snprintf(one, sizeof one, "%.16s %llu+%llu MiB", c.name, c.pageBytes >> 20, c.userBytes >> 20);
                if (at && at + 2 + size_t(n) >= sizeof text) {
                    IOLog("oneios-gpu-clients: %s\n", text);
                    at = 0;
                }
                at += snprintf(text + at, sizeof text - at, "%s%s", at ? ", " : "", one);
            }
            IOLog("oneios-gpu-clients: %s\n", at ? text : "none");
        }
        memcpy(was, now, sizeof was);
        memcpy(fences, rs, sizeof fences);
        memcpy(hist, h, sizeof hist);
        memcpy(closed, cl, sizeof closed);
        memcpy(live, lv, sizeof live);
    }
}
}  // namespace

void oneios_adreno_start(void)
{
    if (oneios_adreno_stage) return;
    if (!oneios_adreno_wanted()) {
        oneios_adreno_stage = 1;
        IOLog("oneios-adreno: no Metal accelerator (gpu state %u; oneios-gpu-metal=1 publishes it anyway)\n", oneios_gpu3d_state);
        return;
    }
    oneios_adreno_stage = 2;
    sheng_gpu_power_probe();
    uint32_t test = 0;
    PE_parse_boot_argn("oneios-adreno-selftest", &test, sizeof test);
    thread_t thread = nullptr;
    if (test && kernel_thread_start(selftest, nullptr, &thread) == KERN_SUCCESS) thread_deallocate(thread);
    thread_t reporter = nullptr;
    if (kernel_thread_start(report, nullptr, &reporter) == KERN_SUCCESS) thread_deallocate(reporter);
}
