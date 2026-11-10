// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// The storage start of the hosted OneiOS kernel. After UniXNU-droidvm's package/DroidVMSourceStorage.cpp
// (BSD-2-Clause-Patent), which starts its iPadOS 27 board's disks this way.
//
// The kernel collection of 24A446 binds no class of the original IOStorageFamily. The public
// IOBlockStorageDevice, built from IOStorageFamily's source (build-oneios-hosted-storage.py), registers below
// the original class once the original family has loaded, so the original block storage driver, the partition
// schemes and APFS take the disks of OneiOSHostedDisk.cpp. bsd_init calls native_source_storage_start after
// the IOKit bootstrap (UniXNU-droidvm's source-storage-start.patch, in the board's series).
//
// Without the kernel collection (the standalone xnu.macho of kernel/build-hosted.sh) there is no IOStorageFamily
// and no APFS: the start says so and returns, and the kernel waits for a root device it will not get.
#include <IOKit/IOService.h>
#include <IOKit/IOCatalogue.h>
#include <IOKit/IOLib.h>
#include <IOKit/storage/IOBlockStorageDevice.h>
#include <libkern/OSKextLib.h>
#include <libkern/c++/OSUnserialize.h>
#include <mach/kmod.h>

extern kmod_info_t g_kernel_kmod_info;
// The static initializers of the source-built classes: their objects lose __mod_init_func when the archive is
// built, so the classes register here, between preModLoad and postModLoad, and not at kernel start before the
// original family exists.
extern "C" void oneios_storage_base_init(void) __asm__("__GLOBAL__sub_I_IOBlockStorageDevice.cpp");
extern "C" void oneios_storage_disk_init(void) __asm__("__GLOBAL__sub_I_OneiOSHostedDisk.cpp");
extern "C" void native_source_storage_start(void);
extern "C" const OSMetaClass *native_source_storage_parent(void);

extern "C" {
volatile int oneios_hosted_storage_status = -1;   // 0 started; 1 no IOStorageFamily; 2 no APFS
}

// The arm64e kernel's static link leaves no weak import undefined, so the hooks of UniXNU's boards that this one
// does not have get their defaults here (hidden and weak: a real definition wins). UniXNU's Data path asks its RAM
// block for a GPU addon handed over by the firmware: there is none (0), xnuhost passes the loader's sidecars another
// way. sheng's launchd job policy after the Data mount: nothing to do, this board's is native_board_launch_policy.
extern "C" __attribute__((weak, visibility("hidden"))) int native_ram_block_gpu_addon(const uint8_t **, uint64_t *)
{
  return 0;
}
extern "C" __attribute__((weak, visibility("hidden"))) int sheng_boot_watchdog_policy(void *, uint64_t)
{
  return 0;
}

static const OSMetaClass *storageParent;
static bool attempted;

// Only the source IOBlockStorageDevice's metaclass constructor asks, while its class registers: the original
// class is its parent.
extern "C" const OSMetaClass *native_source_storage_parent(void)
{
  if (!storageParent) panic("OneiOSHostedStorage: metaclass initialized before the original family");
  return storageParent;
}

static const char kPersonalities[] =
    "("
    "  {"
    "    'IOClass'         = OneiOSHostedDisk;"
    "    'IOProviderClass' = IOPlatformDevice;"
    "    'IOProbeScore'    = 1000:32;"
    "  }"
    ")";

extern "C" void native_source_storage_start(void)
{
  if (attempted) return;
  attempted = true;
  OSReturn status = OSKextLoadKextWithIdentifier("com.apple.iokit.IOStorageFamily");
  if (status != kOSReturnSuccess) {
    oneios_hosted_storage_status = 1;
    printf("OneiOSHostedStorage: no IOStorageFamily (%x): this kernel is not in a kernel collection, no disks\n", status);
    return;
  }
  const OSSymbol *name = OSSymbol::withCString("IOBlockStorageDevice");
  storageParent = name ? OSMetaClass::getMetaClassWithName(name) : nullptr;
  OSSafeReleaseNULL(name);
  if (!storageParent || storageParent->getClassSize() != sizeof(IOBlockStorageDevice))
    panic("OneiOSHostedStorage: original IOBlockStorageDevice is %u bytes, the source one %lu",
          storageParent ? storageParent->getClassSize() : 0, sizeof(IOBlockStorageDevice));
  status = OSKextLoadKextWithIdentifier("com.apple.filesystems.apfs");
  if (status != kOSReturnSuccess) {
    oneios_hosted_storage_status = 2;
    printf("OneiOSHostedStorage: APFS load failed (%x): no root\n", status);
    return;
  }
  void *handle = OSMetaClass::preModLoad(g_kernel_kmod_info.name);
  if (!handle) panic("OneiOSHostedStorage: cannot begin class registration");
  oneios_storage_base_init();
  oneios_storage_disk_init();
  status = OSMetaClass::postModLoad(handle);
  if (status != kOSReturnSuccess) panic("OneiOSHostedStorage: class registration failed %x", status);

  OSString *error = nullptr;
  OSArray *personalities = OSDynamicCast(OSArray, OSUnserialize(kPersonalities, &error));
  if (!personalities) panic("OneiOSHostedStorage: personalities: %s", error ? error->getCStringNoCopy() : "not an array");
  OSSafeReleaseNULL(error);
  // Matching starts at once on the platform devices already published (xnuhost's virtio-mmio windows).
  if (!gIOCatalogue->addDrivers(personalities, true)) panic("OneiOSHostedStorage: the disk driver was not added");
  personalities->release();
  oneios_hosted_storage_status = 0;
  printf("OneiOSHostedStorage: source IOBlockStorageDevice registered, xnuhost's disks matching\n");
}
