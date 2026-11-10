// SPDX-License-Identifier: BSD-2-Clause-Patent
// OneiOS, hosted kernel: UniXNU's SoftwareAES (NativeAES: AES-CBC in software through corecrypto) as the
// IOAESAccelerator of a guest with no AES hardware. After Droid-VM's package/DroidVMSoftwareAES.cpp
// (BSD-2-Clause-Patent), which does this for the iPad collection at slide 0.
//
// Why the boot needs it: AppleImage4's start (matched on IOResources) waits for a service of class
// IOAESAccelerator and never returns without one. That start runs inside IOResources' matching, so the matched
// notification of IOResources is never delivered, the IOMediaBSDClient of every disk stays in its wait for the
// "IOBSD" resource, no BSD disk node appears, APFS's container never starts and the root volume is never found
// (found by fable-prelink from the blocked threads' stacks).
//
// NativeAES subclasses the original IOAESAccelerator of com.apple.iokit.IOCryptoAcceleratorFamily in the kernel
// collection. Its parent's constructor, destructors and methods are reached through the stubs of the generated
// bindings (build-oneios-aes.py, from the class's addresses in this kernelcache): `bti c; b <name>.original`,
// which XnuPrelink retargets when it places the kernel (PC-relative: right at any slide). The parent's metaclass
// is data, an absolute symbol whose value is its link address: under xnuhost the collection runs slid, so
//   - the bound metaclass is compared with the live one after adding the slide, and
//   - NativeAES's own metaclass, constructed with that link address as its superclass, gets the slid one before
//     the module load is finished (nothing reads the link before OSMetaClass::postModLoad).
// A kernel that is not in a collection has no IOAESAccelerator class: nothing is started.
#include <IOKit/IOLib.h>
#include <IOKit/IODeviceTreeSupport.h>
#include <IOKit/IOService.h>
#include <libkern/c++/OSMetaClass.h>
#include <mach/kmod.h>
#include <pexpert/pexpert.h>

extern kmod_info_t g_kernel_kmod_info;
extern vm_offset_t vm_kernel_slide;
extern "C" void oneios_late_init_NativeAES(void) __asm__("__GLOBAL__sub_I_NativeAES.cpp");
// The generated bindings: the parent metaclass's link address and instance size as plain numbers (no fixup).
extern "C" const uint64_t oneios_aes_parent_meta_link;
extern "C" const uint32_t oneios_aes_parent_size;
extern "C" char oneios_aes_native_meta[] __asm__("__ZN9NativeAES10gMetaClassE");
extern "C" volatile uint32_t native_aes_kat_status;

// UniXNU's owner boot manifest (IOKitBSDInit.cpp): -1 not published yet, 0 published in /chosen, else why not.
extern "C" volatile int native_owner_boot_manifest_status __attribute__((weak_import));

extern "C" {
// 1 the parent checked, 2 NativeAES registered, 3 started; 0x80 | n refused at step n; 0x40 no such class.
volatile uint32_t oneios_software_aes_stage;
void oneios_software_aes_start(void);
}

// OSMetaClass as laid out in libkern/c++/OSMetaClass.h: the vtable pointer, reserved, superClassLink.
struct MetaClassHead { void *vtable; void *reserved; const OSMetaClass *superClassLink; };

void oneios_software_aes_start(void)
{
    if (oneios_software_aes_stage) return;
    uint32_t enabled = 1;
    PE_parse_boot_argn("oneios-software-aes", &enabled, sizeof enabled);
    if (!enabled) return;
    const OSSymbol *parentName = OSSymbol::withCString("IOAESAccelerator");
    const OSMetaClass *live = parentName ? OSMetaClass::getMetaClassWithName(parentName) : nullptr;
    OSSafeReleaseNULL(parentName);
    if (!live) {
        oneios_software_aes_stage = 0x40;
        IOLog("oneios-aes: no IOAESAccelerator class (the kernel is not in a kernel collection): no software AES\n");
        return;
    }
    const OSMetaClass *bound = reinterpret_cast<const OSMetaClass *>(oneios_aes_parent_meta_link + vm_kernel_slide);
    if (live != bound || live->getClassSize() != oneios_aes_parent_size) {
        oneios_software_aes_stage = 0x81;
        IOLog("oneios-aes: the live IOAESAccelerator (%p, %u bytes) is not the bound one (%p, %u bytes): the bindings are "
              "for another kernel collection; NativeAES not started\n", live, live->getClassSize(), bound, oneios_aes_parent_size);
        return;
    }
    oneios_software_aes_stage = 1;
    // AppleImage4 reads /chosen boot-manifest-hash as soon as its wait for this service ends, and panics without it
    // ("unable to initialize magazine"): say what is there, so that a boot which stops there shows why.
    {
        IORegistryEntry *chosen = IORegistryEntry::fromPath("/chosen", gIODTPlane);
        OSData *hash = chosen ? OSDynamicCast(OSData, chosen->getProperty("boot-manifest-hash")) : nullptr;
        IOLog("oneios-aes: /chosen boot-manifest-hash: %u bytes (owner boot manifest status %d)\n", hash ? hash->getLength() : 0,
              &native_owner_boot_manifest_status ? native_owner_boot_manifest_status : -2);
        OSSafeReleaseNULL(chosen);
    }

    void *module = OSMetaClass::preModLoad(g_kernel_kmod_info.name);
    if (!module) {
        oneios_software_aes_stage = 0x82;
        return;
    }
    oneios_late_init_NativeAES();
    // The superclass the static initializer passed is the absolute symbol's value: the link address.
    MetaClassHead *native = reinterpret_cast<MetaClassHead *>(oneios_aes_native_meta);
    if (reinterpret_cast<uint64_t>(native->superClassLink) == oneios_aes_parent_meta_link) native->superClassLink = bound;
    if (native->superClassLink != bound || OSMetaClass::postModLoad(module)) {
        oneios_software_aes_stage = 0x82;
        IOLog("oneios-aes: NativeAES's class did not register (superclass %p, expected %p)\n", native->superClassLink, bound);
        return;
    }
    oneios_software_aes_stage = 2;

    IOService *resources = IOService::getResourceService();
    const OSSymbol *name = OSSymbol::withCString("NativeAES");
    OSObject *object = name ? OSMetaClass::allocClassWithName(name) : nullptr;
    OSSafeReleaseNULL(name);
    IOService *aes = OSDynamicCast(IOService, object);
    if (!aes || !resources || !aes->init() || !aes->attach(resources)) {
        OSSafeReleaseNULL(object);
        oneios_software_aes_stage = 0x83;
        IOLog("oneios-aes: NativeAES could not be made or attached\n");
        return;
    }
    // NativeAES::start runs its known-answer test and registers the service, which ends AppleImage4's wait.
    if (!aes->start(resources)) {
        aes->detach(resources);
        aes->release();
        oneios_software_aes_stage = 0x84;
        IOLog("oneios-aes: NativeAES did not start (known-answer test %u)\n", native_aes_kat_status);
        return;
    }
    aes->release();
    oneios_software_aes_stage = 3;
    IOLog("oneios-aes: NativeAES started (IOAESAccelerator in software), known-answer test %u\n", native_aes_kat_status);
}
