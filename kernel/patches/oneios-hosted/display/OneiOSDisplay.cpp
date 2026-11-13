// SPDX-License-Identifier: BSD-2-Clause-Patent
// OneiOS, hosted kernel: the board's side of the display. The display itself is UniXNU's NativeMobileDisplay
// (NativeLegacyDisplay under the kernel collection's original IOMobileFramebufferLegacy), Droid-VM's board probe
// (LegacyDisplayProbe.cpp) and its virtio-gpu scanout client (DroidVMVirtioGpuDisplay.cpp), compiled by
// build-oneios-display.py. This file holds what the hosted guest adds around them:
//
//   - the two entries the kernel calls (bsd_init.c, CONFIG_XNU_NATIVE_LEGACY_DISPLAY): native_legacy_display_probe
//     after the graphics family is loaded, native_legacy_display_late_probe after bsd_autoconf. They run Droid-VM's
//     probe (compiled under other names) and say in the log how far it came.
//   - the parent class under xnuhost's slide. NativeLegacyDisplay's static initializer names its superclass by an
//     absolute symbol, whose value is the link address; the collection runs slid, so the superclass link gets the
//     live address before the module load ends (as kernel/patches/oneios-hosted/aes does for NativeAES), after the
//     live IOMobileFramebufferLegacy was compared with the bound one (address and instance size).
//   - the display's switches as defaults. Droid-VM passes them as boot arguments (offline-legacy-*, sheng-vsync);
//     here an absent one has the value the display needs, and boot argument oneios-display=0 turns all of it off.
//     NativeLegacyDisplay.cpp and the probe are compiled with PE_parse_boot_argn = oneios_display_boot_arg.
//   - the panel: the mode is the boot video's, which xnuhost makes the phone panel's native mode (1440x3120 on
//     the Galaxy S24 Ultra; --width/--height). xnuhost passes offline-legacy-unit-scale=N (its scale, 3 or 2: the
//     probe publishes or checks /chosen display-scale) and sheng-vsync=HZ (the panel's rate, 120: the vsync tick
//     and the refresh properties); the defaults below are for a kernel started without them.
//   - what Droid-VM's generated support file gives the display (the default surface, the surface's allocation
//     size) and the hooks of boards this guest is not (boot progress, the DPU window, Adreno's fences).
//
// The surface factory and allocation are Droid-VM's LegacyDisplaySupport27.cpp (build-droidvm-legacy-display.py,
// BSD-2-Clause-Patent).
#include <IOKit/IOLib.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <libkern/c++/OSBoolean.h>
#include <libkern/c++/OSCollectionIterator.h>
#include <libkern/c++/OSDictionary.h>
#include <libkern/c++/OSMetaClass.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSString.h>
#include <libkern/c++/OSSymbol.h>
#include <libkern/libkern.h>
#include <pexpert/pexpert.h>
#include <sys/proc.h>

extern vm_offset_t vm_kernel_slide;
// The generated bindings (build-oneios-display.py): the parent metaclass's link address and instance size as numbers.
extern "C" const uint64_t oneios_display_parent_meta_link;
extern "C" const uint32_t oneios_display_parent_size;
extern "C" char oneios_display_native_meta[] __asm__("__ZN19NativeLegacyDisplay10gMetaClassE");

extern "C" {
// Droid-VM's probe, compiled under these names.
void oneios_native_legacy_display_probe(void);
void oneios_native_legacy_display_late_probe(void);
extern volatile uint32_t native_legacy_stage, native_legacy_init_result, native_legacy_start_result, native_legacy_probe_match;
extern volatile uint32_t native_legacy_surface_found, native_legacy_dt_attached, native_legacy_object_size, native_legacy_parent_match;
extern volatile int native_legacy_meta_status, native_legacy_surface_status, native_legacy_unit_scale_status, native_legacy_pm_status;
extern volatile uint32_t native_legacy_chosen_scale, native_legacy_pm_stage;
extern volatile uint32_t native_legacy_pm_callbacks;
extern volatile uint64_t native_legacy_object;
extern volatile uint64_t native_legacy_gop_physical, native_legacy_gop_bytes;
extern volatile uint32_t native_legacy_gop_width, native_legacy_gop_height;
extern volatile uint32_t droidvm_virtio_display_state, droidvm_virtio_display_blob, droidvm_virtio_display_last_status;

// 0 not started; 1 the parent class is the bound one; 0x40 no such class (no kernel collection); 0x41 turned off;
// 0x81 the live class is not the bound one. For a debugger and the log.
volatile uint32_t oneios_display_stage;
// Framebuffer user clients opened, swaps accepted and refused, frames by kind (0 CPU copy, 1 blank, 2 virtio scanout).
volatile uint32_t oneios_display_clients, oneios_display_swaps[2], oneios_display_frames[3];

void native_legacy_display_probe(void);
void native_legacy_display_late_probe(void);
boolean_t oneios_display_boot_arg(const char *name, void *value, int size);
bool oneios_display_parent_bound(void);
void oneios_display_parent_slide(void);
void oneios_display_client_seen(IOReturn status, int pid, uint32_t type, IOUserClient *client);
void oneios_display_frame_seen(unsigned kind, uint64_t width, uint64_t height);
void oneios_display_swap_seen(bool accepted, unsigned reason, const void *surface);
bool oneios_display_wide(void);
}

static void oneios_display_power_on(void);
static void oneios_display_properties(IOService *display);

static bool displayEnabled()
{
    uint32_t enabled = 1;
    PE_parse_boot_argn("oneios-display", &enabled, sizeof enabled);
    return enabled != 0;
}

// The display's switches: a boot argument wins; an absent one has the value below while the display is on.
boolean_t oneios_display_boot_arg(const char *name, void *value, int size)
{
    if (PE_parse_boot_argn(name, value, size)) return TRUE;
    static const struct { const char *name; uint32_t value; } defaults[] = {
        {"offline-legacy-phase", 4},        // the original framebuffer class published as the system's display
        {"offline-legacy-gop", 1},          // startHardware takes the boot video as the panel
        {"offline-legacy-register", 1},     // registerService
        {"offline-legacy-pm", 1},           // the logical power state the user client's wait needs
        {"offline-legacy-validate", 1},     // swaps are validated (one full-frame BGRA or half-float surface)
        {"offline-legacy-transfer", 1},     // and shown
        {"offline-legacy-cpu-present", 1},  // a frame the virtio scanout did not take is copied and still retires
        {"offline-legacy-unit-scale", 3},   // /chosen display-scale: @3x unless xnuhost says 2
        {"sheng-vsync", 60},                // the refresh tick (work-loop timer) while a client asks for vsync
    };
    if (size != (int)sizeof(uint32_t) || !displayEnabled()) return FALSE;
    for (const auto &entry : defaults) {
        if (strcmp(name, entry.name)) continue;
        memcpy(value, &entry.value, sizeof entry.value);
        return TRUE;
    }
    return FALSE;
}

// OSMetaClass as laid out in libkern/c++/OSMetaClass.h: the vtable pointer, reserved, superClassLink.
struct MetaClassHead { void *vtable; void *reserved; const OSMetaClass *superClassLink; };

static const OSMetaClass *boundParent()
{
    return reinterpret_cast<const OSMetaClass *>(oneios_display_parent_meta_link + vm_kernel_slide);
}

// Before the probe registers NativeLegacyDisplay: is the collection's IOMobileFramebufferLegacy the class the
// bindings were made for?
bool oneios_display_parent_bound(void)
{
    if (!displayEnabled()) {
        oneios_display_stage = 0x41;
        return false;
    }
    const OSSymbol *name = OSSymbol::withCString("IOMobileFramebufferLegacy");
    const OSMetaClass *live = name ? OSMetaClass::getMetaClassWithName(name) : nullptr;
    OSSafeReleaseNULL(name);
    if (!live) {
        oneios_display_stage = 0x40;
        IOLog("oneios-display: no IOMobileFramebufferLegacy class (IOMobileGraphicsFamily is not in the kernel collection): no display\n");
        return false;
    }
    if (live != boundParent() || live->getClassSize() != oneios_display_parent_size) {
        oneios_display_stage = 0x81;
        IOLog("oneios-display: the live IOMobileFramebufferLegacy (%p, %u bytes) is not the bound one (%p, %u bytes): the bindings "
              "are for another kernel collection; no display\n", live, live->getClassSize(), boundParent(), oneios_display_parent_size);
        return false;
    }
    oneios_display_stage = 1;
    return true;
}

// After NativeLegacyDisplay's static initializer, before OSMetaClass::postModLoad: the superclass it passed is the
// absolute symbol's value, the link address.
void oneios_display_parent_slide(void)
{
    MetaClassHead *native = reinterpret_cast<MetaClassHead *>(oneios_display_native_meta);
    if (reinterpret_cast<uint64_t>(native->superClassLink) == oneios_display_parent_meta_link) native->superClassLink = boundParent();
}

void native_legacy_display_probe(void)
{
    oneios_native_legacy_display_probe();
    // Stage 3 with init 1: the object exists and waits for the late probe (IOSurface needs bsd_autoconf's IOBSD).
    IOLog("oneios-display: probe: stage %u, class registration %d, object %u bytes (parent %u), init %u\n", native_legacy_stage,
          native_legacy_meta_status, native_legacy_object_size, native_legacy_parent_match, native_legacy_init_result);
}

void native_legacy_display_late_probe(void)
{
    if (!displayEnabled()) return;
    oneios_native_legacy_display_late_probe();
    // Stage 10: IOMobileFramebufferLegacy started on the boot video and registered. 6-8: it did not (which step, below).
    IOLog("oneios-display: late probe: stage %u, IOSurface load %d root %u, tree node %u, probe %u, start %u, scale status %d (/chosen "
          "display-scale %u), power stage %u status %d\n", native_legacy_stage, native_legacy_surface_status, native_legacy_surface_found,
          native_legacy_dt_attached, native_legacy_probe_match, native_legacy_start_result, native_legacy_unit_scale_status,
          native_legacy_chosen_scale, native_legacy_pm_stage, native_legacy_pm_status);
    IOLog("oneios-display: panel %ux%u at %#llx (%llu bytes); virtio scanout state %u (2 = presenting), %s resources\n",
          native_legacy_gop_width, native_legacy_gop_height, native_legacy_gop_physical, native_legacy_gop_bytes,
          droidvm_virtio_display_state, droidvm_virtio_display_blob ? "blob" : "2D");
    IOLog("oneios-display: half-float frames: %s\n", oneios_display_wide()
              ? "to the virtio scanout as they are, wide colour and EDR kept (oneios-display-wide=1)"
              : "narrowed to 8-bit sRGB by the CPU (xnuhost's frame is 8-bit; display-format=rgba16f keeps them)");
    oneios_display_power_on();
    if (native_legacy_stage == 10)
        oneios_display_properties(reinterpret_cast<IOService *>(static_cast<uintptr_t>(native_legacy_object)));
}

// The display is on: said to the family the way the power manager says it.
//
// The family's swap_submit (24A446 v54, 0xfffffe000a18c63c) queues a swap only while the display object's word at
// +7180 is not 0; otherwise it releases the swap and answers success (0xfffffe000a18cfcc), so the compositor sees
// every frame accepted and none is shown. That word is the power state, written by one function only: the family's
// setPowerState (0xfffffe000a1915f4), which the power manager calls. The backend registers two power states and asks
// for state 1 at its start (NativeLegacyDisplay::startDisplay, "logical PM": the panel has no power switch here, it
// is lit by the host), and under this kernel the power manager never made that call: on the PC and on the phone
// `pm stage 2, callbacks 0`, +7180 = 0, backboardd submitting swap after swap into nothing. (User space cannot do
// it either: the user client's power request, selector 12, answers unsupported in this family unless a platform
// subclass says it manages power.) Why the power manager does not call is not known yet.
//
// So after the start the display is given a moment to be called, and if it was not, setPowerState(1) is called on
// it here: the virtual the power manager would call, with the same arguments; the backend's override passes it
// to the family. Boot argument oneios-display-power=0 leaves this out.
static void oneios_display_power_on(void)
{
    uint32_t wanted = 1;
    PE_parse_boot_argn("oneios-display-power", &wanted, sizeof wanted);
    IOService *display = reinterpret_cast<IOService *>(static_cast<uintptr_t>(native_legacy_object));
    if (!wanted || !display || native_legacy_stage != 10 || native_legacy_pm_stage < 2) return;
    for (unsigned waited = 0; waited < 20 && native_legacy_pm_callbacks == 0; waited++) IOSleep(100);
    const uint32_t called = native_legacy_pm_callbacks;
    uint32_t before = 0, after = 0;
    memcpy(&before, reinterpret_cast<const unsigned char *>(display) + 7180, sizeof before);
    IOReturn status = kIOReturnSuccess;
    if (!called) status = display->setPowerState(1, display);
    memcpy(&after, reinterpret_cast<const unsigned char *>(display) + 7180, sizeof after);
    IOLog("oneios-display: power: %s; the family's power word was %u and is %u%s\n",
          called ? "the power manager called setPowerState" : "the power manager did not call setPowerState in 2 s: called here",
          before, after, after ? "" : " (STILL OFF: swaps will be dropped)");
    (void)status;
}

// The first user clients, swaps and frames go to the log: they are how a boot shows that backboardd found the
// framebuffer and that its frames reach xnuhost's display.
void oneios_display_client_seen(IOReturn status, int pid, uint32_t type, IOUserClient *client)
{
    const uint32_t n = oneios_display_clients;
    oneios_display_clients = n + 1;
    if (n >= 8) return;
    char name[32] = "?";
    proc_name(pid, name, sizeof name);
    IOLog("oneios-display: framebuffer opened by %s[%d], type %u: status %#x, client %s\n", name, pid, type, status,
          client ? client->getMetaClass()->getClassName() : "none");
}

// The surface formats the compositor swaps: the first swap of each format (up to 8 formats) is logged with what it is,
// so a boot says what iOS's render server renders into (QuartzCore's IOMFBDisplay::framebuffer_pixel_format: 'BGRA'
// 8-bit, 'RGhA' half float, 'w40a'/'b3a8' 10-bit extended range, '&'-spelled compressed forms) and whether this
// display took it. Only 'BGRA', '&BGA' and 'RGhA' are taken (NativeLegacyDisplay's nativeScanoutFormat).
extern "C" uint32_t native_legacy_surface_format(OSObject *), native_legacy_surface_planes(OSObject *);
extern "C" uint64_t native_legacy_surface_width(OSObject *), native_legacy_surface_height(OSObject *), native_legacy_surface_stride(OSObject *);
static void oneios_display_format_seen(bool accepted, unsigned reason, OSObject *surface)
{
    static uint32_t seen[8];
    static unsigned count;
    if (!surface || !surface->metaCast("IOSurface")) return;
    const uint32_t format = native_legacy_surface_format(surface);
    for (unsigned i = 0; i < count; i++) if (seen[i] == format) return;
    if (count >= 8) return;
    seen[count++] = format;
    char code[5] = {char(format >> 24), char(format >> 16), char(format >> 8), char(format), 0};
    for (char &c : code) if (c && (c < 32 || c > 126)) c = '?';
    const char *what = format == 0x42475241 || format == 0x26424741 ? "8-bit B,G,R,A, sRGB"
                     : format == 0x52476841 ? (oneios_display_wide() ? "half float, linear extended sRGB: to the scanout as it is (oneios-display-wide=1)"
                                                                     : "half float, linear extended sRGB: narrowed to 8-bit sRGB by the CPU")
                     : format == 0x77343061 || format == 0x26773461 || format == 0x62336138 || format == 0x26623338 ? "10-bit extended range (XR), wide colour"
                     : "other";
    IOLog("oneios-display: the compositor swaps format '%s' (%#x, %u planes, %llux%llu, stride %llu): %s; %s (reason %u)\n", code, format,
          native_legacy_surface_planes(surface), native_legacy_surface_width(surface), native_legacy_surface_height(surface),
          native_legacy_surface_stride(surface), what, accepted ? "taken" : "REFUSED", reason);
}

void oneios_display_swap_seen(bool accepted, unsigned reason, const void *surface)
{
    const uint32_t n = oneios_display_swaps[accepted ? 0 : 1];
    oneios_display_swaps[accepted ? 0 : 1] = n + 1;
    // Reasons (NativeLegacyDisplay::validateSwap): 0 a full-frame surface, 5 a blank frame; refused: 1 the display is
    // not ready, 2 not one surface on one layer, 3 not the whole panel, 4 the surface's format or memory.
    if (n < 4) IOLog("oneios-display: swap %u %s (reason %u)\n", n + 1, accepted ? "accepted" : "refused", reason);
    if (reason == 0 || reason == 4) oneios_display_format_seen(accepted, reason, const_cast<OSObject *>(static_cast<const OSObject *>(surface)));
}

// Half-float frames ('RGhA') to the virtio scanout as they are: xnuhost passes oneios-display-wide=1 when its frame
// keeps half floats (display-format=rgba16f; runtime/xnuhost/include/xnuhost/pixels.h). Without it they are narrowed
// to 8-bit sRGB by the CPU into the boot video, as before. LegacyDisplayProbe.cpp's offer asks (staged/display.rs).
bool oneios_display_wide(void)
{
    uint32_t wide = 0;
    PE_parse_boot_argn("oneios-display-wide", &wide, sizeof wide);
    return wide != 0;
}

// What the display tells iOS about the panel: the service's property names (and the values of the plain ones), once,
// after the late probe. QuartzCore reads these through IOMobileFramebufferCopyProperty when it makes its display
// (AppleDisplay's constructor, update_display_limits, update_framebuffer_locked: kernel/patches/oneios-hosted/display/
// README.md, "What iOS reads about the panel"); a key a real iPhone has and this list lacks is answered by its default.
static void oneios_display_properties(IOService *display)
{
    OSDictionary *properties = display ? display->dictionaryWithProperties() : nullptr;
    OSCollectionIterator *keys = properties ? OSCollectionIterator::withCollection(properties) : nullptr;
    if (!keys) { OSSafeReleaseNULL(properties); return; }
    char line[240];
    unsigned used = 0, total = 0;
    while (const OSSymbol *key = OSDynamicCast(OSSymbol, keys->getNextObject())) {
        OSObject *value = properties->getObject(key);
        char item[96];
        if (OSNumber *number = OSDynamicCast(OSNumber, value)) snprintf(item, sizeof item, "%s=%llu", key->getCStringNoCopy(), number->unsigned64BitValue());
        else if (OSBoolean *flag = OSDynamicCast(OSBoolean, value)) snprintf(item, sizeof item, "%s=%s", key->getCStringNoCopy(), flag->isTrue() ? "true" : "false");
        else if (OSString *text = OSDynamicCast(OSString, value)) snprintf(item, sizeof item, "%s=%s", key->getCStringNoCopy(), text->getCStringNoCopy());
        else snprintf(item, sizeof item, "%s=(%s)", key->getCStringNoCopy(), value ? value->getMetaClass()->getClassName() : "none");
        const unsigned length = (unsigned)strlen(item);
        if (used && used + length + 2 >= sizeof line) {
            IOLog("oneios-display: properties: %s\n", line);
            used = 0;
        }
        used += (unsigned)snprintf(line + used, sizeof line - used, "%s%s", used ? ", " : "", item);
        total++;
    }
    if (used) IOLog("oneios-display: properties: %s\n", line);
    IOLog("oneios-display: properties: %u on the display service (%s)\n", total, display->getMetaClass()->getClassName());
    keys->release();
    properties->release();
}

void oneios_display_frame_seen(unsigned kind, uint64_t width, uint64_t height)
{
    if (kind > 2) return;
    static const char *const kinds[3] = {"copied to the boot video by the CPU", "blank", "scanned out through virtio-gpu"};
    const uint32_t n = oneios_display_frames[kind];
    oneios_display_frames[kind] = n + 1;
    if (n < 3 || (n & (n - 1)) == 0) {
        if (kind == 2) IOLog("oneios-display: frame %u %s (%llux%llu)\n", n + 1, kinds[kind], width, height);
        else IOLog("oneios-display: frame %u %s\n", n + 1, kinds[kind]);
    }
}

// ---- what Droid-VM's LegacyDisplaySupport27.cpp gives the display
extern "C" IOMemoryDescriptor *native_legacy_surface_descriptor(OSObject *);
extern "C" OSObject *native_legacy_surface_create(OSObject *, void *, OSDictionary *);
extern "C" IOReturn native_legacy_surface_allocate(OSObject *);
extern "C" uint64_t native_legacy_surface_width(OSObject *), native_legacy_surface_height(OSObject *);
extern "C" uint64_t native_legacy_surface_stride(OSObject *);
extern "C" uint32_t native_legacy_surface_format(OSObject *), native_legacy_surface_planes(OSObject *);

// The framebuffer client's default surface (the family's slot 316 leaves it to the backend): BGRA, the panel's size.
extern "C" OSObject *native_legacy_default_surface_create(OSObject *root, uint32_t width, uint32_t height)
{
    if (!root || !width || !height || width > 16384 || height > 16384) return nullptr;
    const uint64_t stride = (uint64_t(width) * 4 + 127) & ~uint64_t(127);
    const uint64_t bytes = (stride * height + 4095) & ~uint64_t(4095);
    OSDictionary *properties = OSDictionary::withCapacity(6);
    if (!properties) return nullptr;
    const char *keys[] = {"IOSurfaceWidth", "IOSurfaceHeight", "IOSurfaceBytesPerRow",
                          "IOSurfaceBytesPerElement", "IOSurfacePixelFormat", "IOSurfaceAllocSize"};
    const uint64_t values[] = {width, height, stride, 4, 0x42475241, bytes};
    bool valid = true;
    for (unsigned i = 0; i < 6 && valid; ++i) {
        OSNumber *number = OSNumber::withNumber(values[i], 64);
        valid = number && properties->setObject(keys[i], number);
        if (number) number->release();
    }
    OSObject *surface = valid ? native_legacy_surface_create(root, nullptr, properties) : nullptr;
    properties->release();
    if (!surface) {
        IOLog("oneios-display: the default surface was not created\n");
        return nullptr;
    }
    IOReturn status = native_legacy_surface_allocate(surface);
    IOMemoryDescriptor *memory = native_legacy_surface_descriptor(surface);
    valid = status == kIOReturnSuccess && memory && memory->getLength() >= bytes &&
            native_legacy_surface_width(surface) == width && native_legacy_surface_height(surface) == height &&
            native_legacy_surface_stride(surface) == stride && native_legacy_surface_format(surface) == 0x42475241 &&
            native_legacy_surface_planes(surface) == 0;
    IOLog("oneios-display: default surface status %#x valid %u %ux%u stride %llu bytes %llu\n", status, valid, width, height, stride, bytes);
    if (!valid) {
        surface->release();
        return nullptr;
    }
    return surface;
}

// iOS 27's IOSurface has no allocation-size getter: bound the copy by the surface's memory descriptor.
extern "C" uint64_t native_legacy_surface_allocation(OSObject *surface)
{
    IOMemoryDescriptor *memory = native_legacy_surface_descriptor(surface);
    return memory ? memory->getLength() : 0;
}

// ---- hooks of other boards. Weak: an archive that has the real thing (the GPU's fences) wins.
extern "C" {
// No boot progress animation: the scanout is always free.
__attribute__((weak)) int boot_progress_prepare_scanout(void) { return 1; }
__attribute__((weak)) void boot_progress_cancel_scanout(void) {}
__attribute__((weak)) void boot_progress_stage(const char *) {}
__attribute__((weak)) int boot_progress_pulse_anchor(uintptr_t, uint64_t, unsigned, unsigned, unsigned) { return 0; }
// No display pipes to hand surfaces to (sheng's direct scanout), and no fence counters of an Adreno driver in this kernel:
// a swap is taken as written, and the virtio scanout waits for the virtio GPU's own submissions.
__attribute__((weak)) volatile uint32_t native_ufs_dpu_state = 0;
__attribute__((weak)) uint32_t native_ufs_dpu_map(unsigned, const uint64_t *, unsigned) { return 0; }
__attribute__((weak)) bool native_adreno_fences(uint32_t *, uint32_t *) { return false; }
// The probes Droid-VM's late probe starts beside the display (key store events, cpufreq, GPU power, touch, HID): not
// this archive's. Compiled into the probe under these names so that the real ones can be linked under their own.
void oneios_display_no_probe(void) {}
}
