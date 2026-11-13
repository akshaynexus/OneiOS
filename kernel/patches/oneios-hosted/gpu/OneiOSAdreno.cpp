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
//   droidvm-gpu-clients=...  the processes besides backboardd that get a Metal device: the grant of the user client
//                            (DroidVMGpu.cpp) and the plug-in's own list (oneios_gpu_client_list below)
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
#include <mach/task_info.h>
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

// The processes besides backboardd that get a Metal device: one list for both gates. The kernel's grant
// (DroidVMGpu.cpp grantedProcesses, staged by build-oneios-gpu.py to read it here) decides the open for the names it
// holds; the plug-in makes a device only in a process its NativeAdrenoMetal.clients names, and 0039 writes that file
// beside the bundle from oneios_gpu_plugin_client_list at every boot. Names as the grant and the plug-in take them:
// "a,b", "X*" (a prefix), "*X*" (a part), "@apps" (every process run from an app bundle), "*" (every process).
//   droidvm-gpu-clients=...         the list; empty: backboardd alone. Not given: kDefaultGpuClients ("*").
//   oneios-gpu-plugin-clients=...   the plug-in's list alone, for a test (the processes it names then ask, and the
//                                   grant or the sandbox decides each open). Not given: the list above.
// Every process (the owner's decision, 2026-11-12), as on an iPhone, where every process that asks has a Metal device and
// the iPad board of Droid-VM runs the same since 2026-11-03. Until then the default named WidgetRenderer_Default, *Poster*,
// iconservicesagent and SBRendererService: SpringBoard, the apps, the keyboard and WebKit drew on the CPU. The GPU's
// address room is cut per process for this (drm2kgsl patch 0004): a process the room has no place for gets no device.
static const char kDefaultGpuClients[] = "*";
extern "C" void oneios_gpu_client_list(char *out, size_t size)
{
    if (!out || size < 2) return;
    memset(out, 0, size);
    // The string forms of PE_parse_boot_argn copy up to the length given and terminate after it: one byte is kept.
    if (!PE_parse_boot_argn("droidvm-gpu-clients", out, int(size - 1))) strlcpy(out, kDefaultGpuClients, size);
}
extern "C" void oneios_gpu_plugin_client_list(char *out, size_t size)
{
    if (!out || size < 2) return;
    memset(out, 0, size);
    if (!PE_parse_boot_argn("oneios-gpu-plugin-clients", out, int(size - 1))) oneios_gpu_client_list(out, size);
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
//   oneios-gpu-sizes: backboardd <1 120, <2 30, <4 12, <8 4, <16 2, <32 6, <64 0, 64+ 3 MiB blobs   (one per client)
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
// NativeAdrenoPublish.cpp: the processes that read the accelerator's MetalPluginName (Metal's discovery, so each process
// that asked for a Metal device), the first twelve names with their counts.
struct NativeAdrenoAskerRecord { char name[20]; uint32_t reads; };
extern NativeAdrenoAskerRecord native_adreno_metal_askers[12];
// 0039 (IOUserClient.cpp): prints the plug-in's text rows that have been quiet this long.
void hosted_gpu_text_flush(uint64_t quiet_ns) __attribute__((weak));
}
// Which processes use the CPU, the same interval: the client that keeps a still screen redrawing (SpringBoard committing,
// or only backboardd running an animation of its own), and the heat. The busiest eight, as a percentage of one CPU:
//   oneios-cpu: 10 s: backboardd[53] 62%, SpringBoard[30] 35%, kernel_task[0] 20%, ... (all 140%)
// From each task's recorded user and system time (TASK_POWER_INFO); a process that started or ended inside the interval
// counts from where it was seen.
extern "C" {
typedef struct proc *proc_t;
typedef int (*proc_iterate_fn_t)(proc_t, void *);
void proc_iterate(unsigned int flags, proc_iterate_fn_t callout, void *arg, proc_iterate_fn_t filterfn, void *filterarg);
task_t proc_task(proc_t);
int proc_pid(proc_t);
kern_return_t task_info(task_t, task_flavor_t, task_info_t, mach_msg_type_number_t *);
void absolutetime_to_nanoseconds(uint64_t, uint64_t *);
}
namespace cpu {
struct Seen { int pid; uint32_t round; uint64_t mach; };
Seen seen[1024];
struct Busy { int pid; uint64_t mach; char name[24]; };
struct Walk { uint32_t round; Busy top[8]; uint64_t all; };
int visit(proc_t p, void *arg)
{
    Walk &w = *static_cast<Walk *>(arg);
    task_t task = proc_task(p);
    const int pid = proc_pid(p);
    if (!task || pid < 0) return 0;
    task_power_info_data_t info = {};
    mach_msg_type_number_t count = TASK_POWER_INFO_COUNT;
    if (task_info(task, TASK_POWER_INFO, reinterpret_cast<task_info_t>(&info), &count) != KERN_SUCCESS) return 0;
    const uint64_t now = info.total_user + info.total_system;
    Seen &e = seen[unsigned(pid) % 1024];
    const uint64_t used = e.pid == pid && e.round + 1 == w.round && now >= e.mach ? now - e.mach : 0;
    e.pid = pid; e.round = w.round; e.mach = now;
    w.all += used;
    unsigned at = 8;
    while (at > 0 && w.top[at - 1].mach < used) at--;
    if (at < 8 && used) {
        memmove(&w.top[at + 1], &w.top[at], (7 - at) * sizeof(Busy));
        w.top[at].pid = pid; w.top[at].mach = used;
        strlcpy(w.top[at].name, proc_best_name(p), sizeof w.top[at].name);
    }
    return 0;   // PROC_RETURNED
}
void report(uint32_t every, uint32_t round)
{
    // The span is the time since the last walk, measured: in a stall the report thread wakes late (the phone,
    // 2026-11-13 10:4x: the display's own 10 s line came after 30 s), and dividing by the nominal `every` gave a
    // 10 s line whose processes added up to 1,440% of a CPU on eight vCPUs. The line says the measured seconds.
    static uint64_t last;
    const uint64_t walked = mach_absolute_time();
    Walk w = {};
    w.round = round;
    proc_iterate(1 /* PROC_ALLPROCLIST */, visit, &w, nullptr, nullptr);
    uint64_t span = 0;
    absolutetime_to_nanoseconds(walked - last, &span);
    const bool first = round < 2 || !last;
    last = walked;
    if (first) return;   // the first walk only records where each process was
    if (span < 1000000000ull) span = uint64_t(every) * 1000000000ull;
    every = uint32_t((span + 500000000ull) / 1000000000ull);
    char text[220];
    size_t at = 0;
    for (unsigned i = 0; i < 8 && w.top[i].mach; i++) {
        uint64_t ns = 0;
        absolutetime_to_nanoseconds(w.top[i].mach, &ns);
        at += snprintf(text + at, sizeof text - at, "%s%.20s[%d] %llu%%", at ? ", " : "", w.top[i].name, w.top[i].pid, ns * 100 / span);
        if (at >= sizeof text - 1) break;
    }
    uint64_t all = 0;
    absolutetime_to_nanoseconds(w.all, &all);
    IOLog("oneios-cpu: %u s: %s (all %llu%%)\n", every, at ? text : "idle", all * 100 / span);
}
}  // namespace cpu

// Each blob's life (hang, 2026-11-13), from DroidVMVirtioGpu3D.cpp's staged hooks (staged/gpu.rs GPU3D): what kind of
// memory the live blobs of each client are, how old they are, and which closed ones still wait for the client's GPU
// work in flight. The phone's report (2026-11-12 23:45) showed backboardd at 926+148 MiB with 1,192 blobs under 1 MiB,
// about 120 more every 10 s (blobs +182 closed +17): the driver closes what it is asked to, so the holder is the client;
// these say which of its memory it is. Two lines per client with blobs, after its oneios-gpu-sizes line:
//   oneios-gpu-kinds: backboardd own task 900 (700 MiB), kernel 2 (1), mappable 650; wired IOSurface 80 (140 MiB),
//     malloc 3 (2), other 1 (1); made +182 gone +17
//   oneios-gpu-ages: backboardd <16K 600, <64K 300, <256K 200, <1M 92; age <10 s 120, <1 min 300, <10 min 500, older 72;
//     held closed 3 (1 MiB, oldest 2 s)
// "own task" blobs are this driver's pages in the client's task (Turnip's BOs: buffers, textures, command streams,
// descriptors, pooled blocks), "kernel" the same in the kernel's map (where the task's could not be had); "mappable" the
// own ones made with the blob flag USE_MAPPABLE (what the CPU writes: command streams, shared buffers, staging).
// "wired" blobs are the caller's own pages (CREATE_BLOB_USER) by the VM tag of the caller's mapping at that address:
// IOSurface (VM_MEMORY_IOSURFACE, a surface imported), malloc (VM_MEMORY_MALLOC*, a no-copy buffer) and other. A blob's
// age is from its creation; "held closed" are blobs closed while the client still had fenced work in flight, which the
// driver frees once that work has finished: a count that only grows, or an oldest age in minutes, is work that never
// finishes. Growth in "own task" small blobs that live long (the age columns) is memory the plug-in or Turnip keeps
// (Turnip's BO cache keeps at most 16 MiB of BOs up to 1 MiB and 96 MiB of larger ones: TU_DARWIN_BO_CACHE_*);
// growth in "wired IOSurface" is surfaces the render server keeps imported.
#include <mach/vm_region.h>
extern "C" kern_return_t mach_vm_region_recurse(vm_map_t, uint64_t *, uint64_t *, uint32_t *, int *, mach_msg_type_number_t *);

namespace blobs {
enum : uint32_t { Capacity = 32768, KindHostVisible = 1, KindWired = 2, KindTaskPages = 4, KindMappable = 1u << 4 };
enum : uint8_t { TagNone, TagIOSurface, TagMalloc, TagOther };
struct Entry { uint32_t id, born, closed; uint8_t slot, kind, tag, pad; uint64_t bytes; };
Entry *table;                    // open addressing by resource id, IOMalloc'd at the first blob
uint32_t used, untracked;
uint64_t made[64], gone[64];     // by client slot, since boot
IOSimpleLock *lock;
uint32_t secondsNow()
{
    uint64_t ns = 0;
    absolutetime_to_nanoseconds(mach_absolute_time(), &ns);
    return uint32_t(ns / 1000000000ull) + 1;   // never 0: 0 is "not closed"
}
uint32_t hash(uint32_t id) { return (id * 2654435761u) >> 17 & (Capacity - 1); }
Entry *find(uint32_t id)
{
    for (uint32_t i = hash(id), n = 0; table && n < Capacity; i = (i + 1) & (Capacity - 1), n++) {
        if (table[i].id == id) return &table[i];
        if (!table[i].id) return nullptr;
    }
    return nullptr;
}
// The VM tag of the caller's mapping at `address` (a CREATE_BLOB_USER's pages).
uint8_t tagAt(task_t task, uint64_t address)
{
    vm_map_t map = task ? get_task_map(task) : nullptr;
    if (!map) return TagNone;
    uint64_t at = address, size = 0;
    uint32_t depth = 64;
    vm_region_submap_info_data_64_t info = {};
    mach_msg_type_number_t count = VM_REGION_SUBMAP_INFO_COUNT_64;
    if (mach_vm_region_recurse(map, &at, &size, &depth, reinterpret_cast<int *>(&info), &count) != KERN_SUCCESS || at > address)
        return TagOther;
    if (info.user_tag == 88 /* VM_MEMORY_IOSURFACE */) return TagIOSurface;
    if (info.user_tag >= 1 && info.user_tag <= 13 && info.user_tag != 10) return TagMalloc;   // VM_MEMORY_MALLOC .. MALLOC_PROB_GUARD
    return TagOther;
}
}  // namespace blobs

// Where a surface's pixels go (widgets, 2026-11-13). On the phone (2026-11-12 23:45) every widget was blank or white while
// WidgetRenderer_Default had a Metal device (its plug-in loaded, AGXAccelerator opened 4 times, 28+3 MiB of blobs), and
// nothing said where its drawing went: into IOSurfaces or not, done or not, read by the render server or not, and what
// the pages held. The staged driver's hooks (staged/gpu.rs GPU3D) feed this trail; the GPU report prints it while it
// changes, at most these lines each report (docs/gpu.md 5.1, "Where a surface's pixels go"):
//   oneios-gpu-sync: 10 s: tickets +40 ended +40 (closed +0, not kept +0); CPU locks +12 (read-only +10, no wait +0): waited +2,
//     gave up +0, mean 900 us, max ever 4000 us; reads that waited for writers +0, gave up +0, max ever 0 us
//   oneios-gpu-surfaces: WidgetRenderer_ submits +40; writes +24 done +24 (closed +0) on 6 surfaces: 5 since read by
//     another (backboardd), 1 by none, 0 read while written; reads +2: others' +0 (while written +0), with no GPU writer +2
//   oneios-gpu-pixels: WidgetRenderer_ 6 surfaces (5 MiB): zero 0, white 1, flat 0, drawn 5; changed 3; pages of another
//     process's surfaces 0 (-)
//   oneios-gpu-locks: 10 s: SpringBoard 12 (GPU-written 3, read-only 10), WidgetRenderer_Default 2 (GPU-written 2, ...)
// The surfaces lines are by IOSurface ID, from what each command buffer names as it commits (NativeMetal's surfacesync: the
// surfaces it renders into or samples, and when it completed). The pixels lines are by blob: each IOSurface the client
// imported for the GPU (CREATE_BLOB_USER over a mapping tagged VM_MEMORY_IOSURFACE), its guest pages read here, 8 pages
// spread over it and 16 pixels in each: all zero (never written, or cleared to transparent black), white (every byte
// 0xf0 or more), flat (one other value), drawn (anything else); "changed" since the last report. "pages of another
// process's surfaces": the import's first page is the first page of an IOSurface another client imported (one physical
// range seen by both), with the client named that most share with.
extern "C" {
extern volatile uint64_t droidvm_surface_stats[8], droidvm_surface_lock_options[4], droidvm_surface_read_waits[4];
void proc_selfname(char *, int);
uint64_t phystokv(uint64_t pa);                // arm_vm_init.c; the identity on the RAM window under xnuhost
}
namespace trail {
enum : uint32_t { Buckets = 1024, Ways = 4, NoSlot = 0xff, PageCapacity = 2048, Samples = 8, Words = 16, Lockers = 8 };
// An IOSurface the GPU used: its last use, the trail's sequence at its last GPU write done and at its last read by another
// client than its writer, the client that wrote it last and the other that read it last, the writes begun and not ended,
// and whether another client's read began while a write was in flight (bit 0).
struct Surface { uint32_t id, seen; uint64_t wroteSeq, readSeq; uint32_t wroteAt; uint8_t writer, reader, inflight, flags; };
struct Slot { uint64_t submits, writes, done, closedEnds, reads, readOthers, readDuring, readUnwritten; };
struct Locker { char name[16]; uint32_t locks, gpuWritten, readOnly; };
// An IOSurface import's pages: its blob, client, first page, the pages read for the report, and what they held last.
struct Pages { uint32_t id; uint8_t slot, peer, kept, look; uint64_t bytes, first, hash, pa[Samples]; };
Surface *surfaces;                 // Buckets x Ways, 128 KiB, at the first use
Slot slots[64];                    // by client slot, since boot (the report prints the differences)
Locker lockers[Lockers];           // since the last report
uint32_t lockersOther, replaced;
uint64_t sequence;
IOSimpleLock *slock;               // surfaces, slots, lockers
Pages *pages;                      // open addressing by blob id, 192 KiB, at the first import
uint32_t pagesUsed, pagesUntracked;
IOLock *plock;                     // pages: a lock that may be held while the report reads them (thread context only)
bool surfacesReady()
{
    if (!slock) {
        IOSimpleLock *made = IOSimpleLockAlloc();
        if (made && !OSCompareAndSwapPtr(nullptr, made, reinterpret_cast<void *volatile *>(&slock))) IOSimpleLockFree(made);
        if (!slock) return false;
    }
    if (!surfaces) {
        Surface *t = static_cast<Surface *>(IOMallocZero(sizeof(Surface) * Buckets * Ways));
        if (t && !OSCompareAndSwapPtr(nullptr, t, reinterpret_cast<void *volatile *>(&surfaces))) IOFree(t, sizeof(Surface) * Buckets * Ways);
    }
    return surfaces != nullptr;
}
bool pagesReady()
{
    if (!plock) {
        IOLock *made = IOLockAlloc();
        if (made && !OSCompareAndSwapPtr(nullptr, made, reinterpret_cast<void *volatile *>(&plock))) IOLockFree(made);
        if (!plock) return false;
    }
    if (!pages) {
        Pages *t = static_cast<Pages *>(IOMallocZero(sizeof(Pages) * PageCapacity));
        if (t && !OSCompareAndSwapPtr(nullptr, t, reinterpret_cast<void *volatile *>(&pages))) IOFree(t, sizeof(Pages) * PageCapacity);
    }
    return pages != nullptr;
}
// slock held. The surface's entry, or with `make` a new one in place of its bucket's least recently used.
Surface *surface(uint32_t id, uint32_t now, bool make)
{
    Surface *bucket = &surfaces[((id * 2654435761u) >> 22) * Ways], *pick = nullptr;
    for (uint32_t w = 0; w < Ways; w++) {
        if (bucket[w].id == id) return &bucket[w];
        if (!pick || (pick->id && (!bucket[w].id || bucket[w].seen < pick->seen))) pick = &bucket[w];
    }
    if (!make) return nullptr;
    if (pick->id) replaced++;
    *pick = {id, now, 0, 0, 0, uint8_t(NoSlot), uint8_t(NoSlot), 0, 0};
    return pick;
}
uint32_t pageHash(uint32_t id) { return (id * 2654435761u) >> 21 & (PageCapacity - 1); }
// plock held.
Pages *pagesOf(uint32_t id)
{
    for (uint32_t i = pageHash(id), n = 0; pages && n < PageCapacity; i = (i + 1) & (PageCapacity - 1), n++) {
        if (pages[i].id == id) return &pages[i];
        if (!pages[i].id) return nullptr;
    }
    return nullptr;
}
// plock held: the entry goes, the probe chains stay whole (backward shift).
void erase(Pages *e)
{
    uint32_t i = uint32_t(e - pages), j = i;
    for (;;) {
        pages[i].id = 0;
        for (;;) {
            j = (j + 1) & (PageCapacity - 1);
            if (!pages[j].id) { pagesUsed--; return; }
            const uint32_t k = pageHash(pages[j].id);
            if (i <= j ? (i < k && k <= j) : (i < k || k <= j)) continue;
            break;
        }
        pages[i] = pages[j];
        i = j;
    }
}
// From oneios_gpu_blob_made: whether the import is an IOSurface's (only those stay).
void kept(uint32_t id, bool iosurface)
{
    if (!plock || !pages) return;
    IOLockLock(plock);
    if (Pages *e = pagesOf(id)) {
        if (iosurface) e->kept = 1; else erase(e);
    }
    IOLockUnlock(plock);
}
// From oneios_gpu_blob_gone.
void gone(uint32_t id)
{
    if (!plock || !pages) return;
    IOLockLock(plock);
    if (Pages *e = pagesOf(id)) erase(e);
    IOLockUnlock(plock);
}
// What the words read from an import's pages are: 0 all zero, 1 white (every byte 0xf0 or more), 2 one other value, 3 drawn.
uint8_t classify(const uint32_t *w, uint32_t n)
{
    bool zero = true, white = true, flat = true;
    for (uint32_t i = 0; i < n; i++) {
        zero &= w[i] == 0;
        white &= (w[i] & 0xf0f0f0f0u) == 0xf0f0f0f0u;
        flat &= w[i] == w[0];
    }
    return zero ? 0 : white ? 1 : flat ? 2 : 3;
}
const char *slotName(uint32_t s) { return s < 64 && droidvm_virtgpu_client_memory[s].name[0] ? droidvm_virtgpu_client_memory[s].name : "-"; }
// The sync line: the tunnel's own IOSurface counters (DroidVMVirtioGpu3D.cpp surfaceAccess, surfaceTrap, awaitWriters).
void printSync(uint32_t every)
{
    static uint64_t was[8], wasOptions[4], wasReads[4];
    uint64_t st[8], op[4], rd[4];
    for (unsigned i = 0; i < 8; i++) st[i] = droidvm_surface_stats[i];
    for (unsigned i = 0; i < 4; i++) { op[i] = droidvm_surface_lock_options[i]; rd[i] = droidvm_surface_read_waits[i]; }
    if (!memcmp(st, was, sizeof st) && !memcmp(op, wasOptions, sizeof op) && !memcmp(rd, wasReads, sizeof rd)) return;
    const uint64_t locks = (op[0] + op[1] + op[2] + op[3]) - (wasOptions[0] + wasOptions[1] + wasOptions[2] + wasOptions[3]);
    const uint64_t waited = st[4] - was[4];
    IOLog("oneios-gpu-sync: %u s: tickets +%llu ended +%llu (closed +%llu, not kept +%llu); CPU locks +%llu (read-only +%llu, no wait +%llu): "
          "waited +%llu, gave up +%llu, mean %llu us, max ever %llu us; reads that waited for writers +%llu, gave up +%llu, max ever %llu us\n",
          every, st[0] - was[0], st[1] - was[1], st[2] - was[2], st[3] - was[3], locks, (op[1] + op[3]) - (wasOptions[1] + wasOptions[3]),
          (op[2] + op[3]) - (wasOptions[2] + wasOptions[3]), waited, st[5] - was[5], waited ? (st[6] - was[6]) / waited : 0, st[7],
          rd[0] - wasReads[0], rd[1] - wasReads[1], rd[3]);
    memcpy(was, st, sizeof was);
    memcpy(wasOptions, op, sizeof wasOptions);
    memcpy(wasReads, rd, sizeof wasReads);
}
// The surfaces lines (one per client that submitted or named a surface since the last report) and the CPU lockers.
void printSurfaces(uint32_t every)
{
    static Slot was[64];
    static uint32_t lastAt;
    if (!slock || !surfaces) return;
    const uint32_t now = blobs::secondsNow(), since = lastAt ? lastAt : (now > every ? now - every : 0);
    lastAt = now;
    struct Count { uint32_t written, reachedOther, notRead, during; uint8_t reader; };
    Count *count = static_cast<Count *>(IOMallocZero(sizeof(Count) * 64));
    if (!count) return;
    Slot nowSlots[64];
    Locker seen[Lockers];
    uint32_t other, lost;
    IOSimpleLockLock(slock);
    for (uint32_t i = 0; i < Buckets * Ways; i++) {
        const Surface &s = surfaces[i];
        if (!s.id || s.writer >= 64 || !s.wroteAt || s.wroteAt < since) continue;
        Count &c = count[s.writer];
        c.written++;
        if (s.readSeq > s.wroteSeq) { c.reachedOther++; c.reader = s.reader; } else c.notRead++;
        if (s.flags & 1) c.during++;
    }
    memcpy(nowSlots, slots, sizeof nowSlots);
    memcpy(seen, lockers, sizeof seen);
    other = lockersOther; lost = replaced;
    bzero(lockers, sizeof lockers);
    lockersOther = 0;
    IOSimpleLockUnlock(slock);
    for (uint32_t s = 0; s < 64; s++) {
        const Slot &n = nowSlots[s], &w = was[s];
        if (n.submits == w.submits && n.writes == w.writes && n.reads == w.reads && n.done == w.done) continue;
        const Count &c = count[s];
        IOLog("oneios-gpu-surfaces: %.16s submits +%llu; writes +%llu done +%llu (closed +%llu) on %u surfaces: %u since read by another (%.16s), "
              "%u by none, %u read while written; reads +%llu: others' +%llu (while written +%llu), with no GPU writer +%llu\n",
              slotName(s), n.submits - w.submits, n.writes - w.writes, n.done - w.done, n.closedEnds - w.closedEnds, c.written, c.reachedOther,
              c.reachedOther ? slotName(c.reader) : "-", c.notRead, c.during, n.reads - w.reads, n.readOthers - w.readOthers,
              n.readDuring - w.readDuring, n.readUnwritten - w.readUnwritten);
    }
    memcpy(was, nowSlots, sizeof was);
    IOFree(count, sizeof(Count) * 64);
    char text[200];
    size_t at = 0;
    for (const Locker &l : seen) {
        if (!l.name[0]) continue;
        char one[80];
        const int n = snprintf(one, sizeof one, "%.16s %u (GPU-written %u, read-only %u)", l.name, l.locks, l.gpuWritten, l.readOnly);
        if (at && at + 2 + size_t(n) >= sizeof text) { IOLog("oneios-gpu-locks: %u s: %s\n", every, text); at = 0; }
        at += snprintf(text + at, sizeof text - at, "%s%s", at ? ", " : "", one);
    }
    if (at || other) IOLog("oneios-gpu-locks: %u s: %s; other processes %u; surfaces forgotten for room %u\n", every, at ? text : "-", other, lost);
}
// The pixels lines: each client's IOSurface imports, what their pages hold. Printed for a client when its line changes.
void printPixels()
{
    static uint64_t printed[64];
    if (!plock || !pages) return;
    struct Count { uint32_t surfaces, cls[4], changed, shared; uint64_t bytes; uint16_t peers[64]; };
    Count *count = static_cast<Count *>(IOMallocZero(sizeof(Count) * 64));
    if (!count) return;
    uint32_t untracked;
    IOLockLock(plock);
    for (uint32_t i = 0; i < PageCapacity; i++) {
        Pages &e = pages[i];
        if (!e.id || !e.kept || e.slot >= 64) continue;
        uint32_t words[Samples * Words], n = 0;
        uint64_t hash = 1469598103934665603ull;
        for (uint32_t k = 0; k < Samples; k++) {
            const uint64_t pa = e.pa[k];
            if (!pa || pa < gPhysBase || pa - gPhysBase > gPhysSize - 4096) continue;
            const volatile uint32_t *page = reinterpret_cast<const volatile uint32_t *>(phystokv(pa & ~uint64_t(4095)));
            for (uint32_t j = 0; j < Words; j++) {
                words[n] = page[j * (4096 / 4 / Words) + (k & 3)];
                hash = (hash ^ words[n]) * 1099511628211ull;
                n++;
            }
        }
        Count &c = count[e.slot];
        c.surfaces++;
        c.bytes += e.bytes;
        c.cls[n ? classify(words, n) : 0]++;
        if (e.look && e.hash != hash) c.changed++;
        e.hash = hash;
        e.look = 1;
        if (e.peer < 64) { c.shared++; c.peers[e.peer]++; }
    }
    untracked = pagesUntracked;
    IOLockUnlock(plock);
    for (uint32_t s = 0; s < 64; s++) {
        const Count &c = count[s];
        uint32_t top = NoSlot;
        for (uint32_t p = 0; p < 64; p++) if (c.peers[p] && (top == NoSlot || c.peers[p] > c.peers[top])) top = p;
        uint64_t digest = c.surfaces;
        for (uint32_t k = 0; k < 4; k++) digest = digest * 1000003u + c.cls[k];
        digest = (digest * 1000003u + c.changed) * 1000003u + c.shared;
        if (digest == printed[s]) continue;
        printed[s] = digest;
        if (!c.surfaces && !droidvm_virtgpu_client_memory[s].name[0]) continue;
        IOLog("oneios-gpu-pixels: %.16s %u surfaces (%llu MiB): zero %u, white %u, flat %u, drawn %u; changed %u; pages of another process's "
              "surfaces %u (%.16s)%s\n", slotName(s), c.surfaces, c.bytes >> 20, c.cls[0], c.cls[1], c.cls[2], c.cls[3], c.changed, c.shared,
              top < 64 ? slotName(top) : "-", untracked ? "; table full: some imports not read" : "");
    }
    IOFree(count, sizeof(Count) * 64);
}
void print(uint32_t every)
{
    printSync(every);
    printSurfaces(every);
    printPixels();
}
}  // namespace trail
// A command buffer names an IOSurface it renders into (write) or samples, as it commits (DroidVMVirtioGpu3D.cpp surfaceAccess).
extern "C" void oneios_gpu_surface_use(uint32_t id, uint32_t write, uint32_t slot, uint64_t)
{
    using namespace trail;
    if (!id || slot >= 64 || !surfacesReady()) return;
    const uint32_t now = blobs::secondsNow();
    IOSimpleLockLock(slock);
    Surface *s = surface(id, now, true);
    Slot &me = slots[slot];
    s->seen = now;
    if (write) {
        me.writes++;
        s->writer = uint8_t(slot);
        if (s->inflight < 255) s->inflight++;
    } else {
        me.reads++;
        if (s->writer == NoSlot) {
            me.readUnwritten++;
        } else if (s->writer != slot) {
            me.readOthers++;
            s->reader = uint8_t(slot);
            s->readSeq = ++sequence;
            if (s->inflight) { me.readDuring++; s->flags |= 1; }
        }
    }
    IOSimpleLockUnlock(slock);
}
// The command buffer completed (ticket ended), or its connection closed (ticket 0): a write of the surface is done.
extern "C" void oneios_gpu_surface_done(uint32_t id, uint32_t write, uint32_t slot, uint64_t ticket)
{
    using namespace trail;
    if (!write || slot >= 64 || !slock || !surfaces) return;
    const uint32_t now = blobs::secondsNow();
    IOSimpleLockLock(slock);
    slots[slot].done++;
    if (!ticket) slots[slot].closedEnds++;
    if (Surface *s = surface(id, now, false)) {
        if (s->inflight) s->inflight--;
        s->wroteSeq = ++sequence;
        s->wroteAt = now;
    }
    IOSimpleLockUnlock(slock);
}
// A CPU lock of a surface (IOSurfaceRootUserClient's lock trap), in the locking process; only surfaces the GPU used.
extern "C" void oneios_gpu_surface_lock(uint32_t id, uint32_t options)
{
    using namespace trail;
    if (!slock || !surfaces) return;
    char name[16] = "";
    proc_selfname(name, sizeof name);
    const uint32_t now = blobs::secondsNow();
    IOSimpleLockLock(slock);
    if (const Surface *s = surface(id, now, false)) {
        Locker *l = nullptr;
        for (Locker &k : lockers) {
            if (!strncmp(k.name, name, sizeof k.name)) { l = &k; break; }
            if (!k.name[0] && !l) l = &k;
        }
        if (l && !l->name[0]) strlcpy(l->name, name, sizeof l->name);
        if (l) {
            l->locks++;
            if (s->writer != NoSlot) l->gpuWritten++;
            if (options & 1) l->readOnly++;
        } else {
            lockersOther++;
        }
    }
    IOSimpleLockUnlock(slock);
}
extern "C" void oneios_gpu_submit(uint32_t slot)
{
    if (slot < 64) __atomic_fetch_add(&trail::slots[slot].submits, 1, __ATOMIC_RELAXED);
}
// A test switch, off by default: oneios-gpu-read-wait=1 makes the render server's command buffers wait, as they name the
// surfaces they sample, for other processes' GPU work begun before them that writes those surfaces (the driver's
// awaitWriters, at most droidvm_surface_read_wait_ms: 100 ms), which a newer plug-in asks for itself and this pin never
// does. KGSL runs the render server's queue ahead of the apps', so without it a composition can sample a widget's surface
// before the widget's drawing into it has run. The waits are oneios-gpu-sync's "reads that waited for writers".
extern "C" volatile int droidvm_virtgpu_compositor_pid;
extern "C" bool oneios_gpu_reader_waits(int pid)
{
    static int on = -1;
    if (on < 0) {
        uint32_t value = 0;
        PE_parse_boot_argn("oneios-gpu-read-wait", &value, sizeof value);
        on = value != 0;
        IOLog("oneios-gpu-read-wait: %s\n", on ? "on: the render server's reads wait for other processes' writes" : "off");
    }
    return on && pid > 0 && pid == droidvm_virtgpu_compositor_pid;
}
// A blob of the caller's own pages (CREATE_BLOB_USER), its guest physical segments {address, length, padding}: the pages the
// report reads, and whether another client already imported the same first page.
extern "C" void oneios_gpu_blob_segments(uint32_t id, uint32_t slot, uint64_t bytes, const void *list, uint32_t count)
{
    using namespace trail;
    struct Segment { uint64_t address; uint32_t length, padding; };
    if (!id || slot >= 64 || !count || !bytes || !pagesReady()) return;
    const Segment *segment = static_cast<const Segment *>(list);
    Pages p = {};
    p.id = id; p.slot = uint8_t(slot); p.peer = uint8_t(NoSlot); p.bytes = bytes; p.first = segment[0].address;
    // Samples pages spread over the blob, its first and last among them, found in one walk of the segments.
    const uint64_t last = (bytes - 1) & ~uint64_t(4095);
    uint64_t at = 0;
    uint32_t i = 0;
    for (uint32_t k = 0; k < Samples; k++) {
        const uint64_t want = (last / 4096 * k / (Samples - 1)) * 4096;
        while (i < count && at + segment[i].length <= want) at += segment[i++].length;
        p.pa[k] = i < count ? segment[i].address + (want - at) : 0;
    }
    IOLockLock(plock);
    for (uint32_t j = 0; j < PageCapacity; j++) {
        if (pages[j].id && pages[j].kept && pages[j].first == p.first && pages[j].slot != slot) { p.peer = pages[j].slot; break; }
    }
    if (pagesUsed >= PageCapacity / 4 * 3) {
        pagesUntracked++;
    } else {
        uint32_t j = pageHash(id);
        while (pages[j].id && pages[j].id != id) j = (j + 1) & (PageCapacity - 1);
        if (!pages[j].id) pagesUsed++;
        pages[j] = p;
    }
    IOLockUnlock(plock);
}
extern "C" void oneios_gpu_blob_made(uint32_t id, uint32_t slot, uint64_t bytes, uint32_t kind, task_t task, uint64_t user)
{
    using namespace blobs;
    if (!id || slot >= 64) return;
    if (!lock) {
        IOSimpleLock *made_lock = IOSimpleLockAlloc();
        if (made_lock && !OSCompareAndSwapPtr(nullptr, made_lock, reinterpret_cast<void *volatile *>(&lock))) IOSimpleLockFree(made_lock);
        if (!lock) return;
    }
    if (!table) {
        Entry *t = static_cast<Entry *>(IOMallocZero(sizeof(Entry) * Capacity));   // 768 KiB, once
        if (t && !OSCompareAndSwapPtr(nullptr, t, reinterpret_cast<void *volatile *>(&table))) IOFree(t, sizeof(Entry) * Capacity);
        if (!table) return;
    }
    const uint8_t tag = (kind & KindWired) ? tagAt(task, user) : TagNone;
    const uint32_t now = secondsNow();
    IOSimpleLockLock(lock);
    made[slot]++;
    if (used >= Capacity / 4 * 3) {
        untracked++;
    } else {
        uint32_t i = hash(id);
        while (table[i].id && table[i].id != id) i = (i + 1) & (Capacity - 1);
        if (!table[i].id) used++;
        table[i] = {id, now, 0, uint8_t(slot), uint8_t(kind), tag, 0, bytes};
    }
    IOSimpleLockUnlock(lock);
    if (kind & KindWired) trail::kept(id, tag == TagIOSurface);     // the surface trail keeps the pages of IOSurface imports only
}
extern "C" void oneios_gpu_blob_closed(uint32_t id)
{
    using namespace blobs;
    if (!lock) return;
    const uint32_t now = secondsNow();
    IOSimpleLockLock(lock);
    if (Entry *e = find(id)) e->closed = now;
    IOSimpleLockUnlock(lock);
}
extern "C" void oneios_gpu_blob_gone(uint32_t id)
{
    using namespace blobs;
    trail::gone(id);          // before its pages go back (the driver unwires them after this hook)
    if (!lock) return;
    IOSimpleLockLock(lock);
    Entry *e = find(id);
    if (e) {
        if (e->slot < 64) gone[e->slot]++;
        // backward shift: the probe chains stay whole
        uint32_t i = uint32_t(e - table), j = i;
        for (;;) {
            table[i].id = 0;
            for (;;) {
                j = (j + 1) & (Capacity - 1);
                if (!table[j].id) goto done;
                const uint32_t k = hash(table[j].id);
                if (i <= j ? (i < k && k <= j) : (i < k || k <= j)) continue;
                break;
            }
            table[i] = table[j];
            i = j;
        }
    done:
        used--;
    }
    IOSimpleLockUnlock(lock);
}
namespace blobs {
struct Census {
    uint32_t task, kernel, mappable, iosurface, malloc, other, small[4], age[4], held;
    uint64_t taskBytes, kernelBytes, iosurfaceBytes, mallocBytes, otherBytes, heldBytes;
    uint32_t heldOldest;
};
// The two lines of each client slot with blobs. `was` keeps made/gone at the last report.
void print(uint64_t (&wasMade)[64], uint64_t (&wasGone)[64])
{
    if (!lock || !table) return;
    Census *all = static_cast<Census *>(IOMallocZero(sizeof(Census) * 64));
    if (!all) return;
    uint64_t madeNow[64], goneNow[64];
    const uint32_t now = secondsNow();
    IOSimpleLockLock(lock);
    for (uint32_t i = 0; i < Capacity; i++) {
        const Entry &e = table[i];
        if (!e.id || e.slot >= 64) continue;
        Census &c = all[e.slot];
        const uint64_t mib = e.bytes;
        if (e.kind & KindWired) {
            if (e.tag == TagIOSurface) { c.iosurface++; c.iosurfaceBytes += mib; }
            else if (e.tag == TagMalloc) { c.malloc++; c.mallocBytes += mib; }
            else { c.other++; c.otherBytes += mib; }
        } else if (!(e.kind & KindHostVisible)) {
            if (e.kind & KindTaskPages) { c.task++; c.taskBytes += mib; } else { c.kernel++; c.kernelBytes += mib; }
            if (e.kind & KindMappable) c.mappable++;
        }
        if (e.bytes < (1u << 20)) c.small[e.bytes < (16u << 10) ? 0 : e.bytes < (64u << 10) ? 1 : e.bytes < (256u << 10) ? 2 : 3]++;
        const uint32_t age = now - e.born;
        c.age[age < 10 ? 0 : age < 60 ? 1 : age < 600 ? 2 : 3]++;
        if (e.closed) {
            c.held++; c.heldBytes += e.bytes;
            if (now - e.closed > c.heldOldest) c.heldOldest = now - e.closed;
        }
    }
    memcpy(madeNow, made, sizeof madeNow);
    memcpy(goneNow, gone, sizeof goneNow);
    const uint32_t lost = untracked;
    IOSimpleLockUnlock(lock);
    for (unsigned s = 0; s < 64; s++) {
        const DroidVMClientMemory &m = droidvm_virtgpu_client_memory[s];
        const Census &c = all[s];
        const uint32_t live = c.task + c.kernel + c.iosurface + c.malloc + c.other;
        if (!m.name[0] || !(live || madeNow[s] != wasMade[s]) || (madeNow[s] == wasMade[s] && goneNow[s] == wasGone[s] && !c.held)) continue;
        IOLog("oneios-gpu-kinds: %.16s own task %u (%llu MiB), kernel %u (%llu), mappable %u; wired IOSurface %u (%llu MiB), malloc %u (%llu), "
              "other %u (%llu); made +%llu gone +%llu\n", m.name, c.task, c.taskBytes >> 20, c.kernel, c.kernelBytes >> 20, c.mappable,
              c.iosurface, c.iosurfaceBytes >> 20, c.malloc, c.mallocBytes >> 20, c.other, c.otherBytes >> 20, madeNow[s] - wasMade[s],
              goneNow[s] - wasGone[s]);
        IOLog("oneios-gpu-ages: %.16s <16K %u, <64K %u, <256K %u, <1M %u; age <10 s %u, <1 min %u, <10 min %u, older %u; held closed %u "
              "(%llu MiB, oldest %u s)%s\n", m.name, c.small[0], c.small[1], c.small[2], c.small[3], c.age[0], c.age[1], c.age[2], c.age[3],
              c.held, c.heldBytes >> 20, c.heldOldest, lost ? "; table full: some blobs not counted" : "");
    }
    memcpy(wasMade, madeNow, sizeof madeNow);
    memcpy(wasGone, goneNow, sizeof goneNow);
    IOFree(all, sizeof(Census) * 64);
}
}  // namespace blobs

namespace {
void report(void *, wait_result_t)
{
    uint32_t every = 10;
    PE_parse_boot_argn("oneios-gpu-report", &every, sizeof every);
    if (!every) return;
    uint64_t was[8] = {}, fences[3] = {}, hist[10] = {}, closed[2] = {}, live[4] = {}, askedBefore = 0;
    for (uint32_t round = 1;; round++) {
        IOSleep(every * 1000);
        cpu::report(every, round);
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
            // Each client's live blobs by size: metal2vulkan's pooled blocks are 64 MiB buffers ("64+"), windows and
            // textures the small sizes, so growth in "64+" is the runtime's pools and growth below is resources.
            for (unsigned i = 0; i < 64; i++) {
                const DroidVMClientMemory &c = droidvm_virtgpu_client_memory[i];
                if (!c.name[0] || !(c.pageBytes + c.userBytes)) continue;
                IOLog("oneios-gpu-sizes: %.16s <1 %u, <2 %u, <4 %u, <8 %u, <16 %u, <32 %u, <64 %u, 64+ %u MiB blobs\n", c.name,
                      c.sizes[0], c.sizes[1], c.sizes[2], c.sizes[3], c.sizes[4], c.sizes[5], c.sizes[6], c.sizes[7]);
            }
            // The kinds and ages of the blobs of each client that made or let go of one in the interval, or still has
            // closed ones held (blobs:: above).
            static uint64_t wasMade[64], wasGone[64];
            blobs::print(wasMade, wasGone);
        }
        // Where the clients' IOSurface drawing goes (trail:: above): only what changed since the last report.
        trail::print(every);
        // Who asked for a Metal device (read MetalPluginName), when that changed: with the clients line, a process that
        // asked and holds no GPU memory had its device refused or made none (its reason: the oneios-gpu-text lines).
        uint64_t asked = 0;
        for (const NativeAdrenoAskerRecord &a : native_adreno_metal_askers) asked = asked * 31 + a.reads + uint8_t(a.name[0]);
        if (asked != askedBefore) {
            askedBefore = asked;
            char text[200];
            size_t at = 0;
            for (const NativeAdrenoAskerRecord &a : native_adreno_metal_askers) {
                if (!a.name[0]) continue;
                char one[40];
                const int n = snprintf(one, sizeof one, "%.16s %u", a.name, a.reads);
                if (at && at + 2 + size_t(n) >= sizeof text) {
                    IOLog("oneios-gpu-askers: %s\n", text);
                    at = 0;
                }
                at += snprintf(text + at, sizeof text - at, "%s%s", at ? ", " : "", one);
            }
            if (at) IOLog("oneios-gpu-askers: %s\n", text);
        }
        if (hosted_gpu_text_flush) hosted_gpu_text_flush(1000000000ull);
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
