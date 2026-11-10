// SPDX-License-Identifier: BSD-2-Clause-Patent
// boot_args, the boot sidecars and the placement of everything in the kernel's memory window.
// Ported from DarwinBoot (https://github.com/UniXNU/DarwinBoot, commit 8544957: BootArgs.h,
// PackedRamdisk.h, and Boot/ValidTrustCaches of DarwinBoot.c), Copyright (c) 2026 DarwinBoot
// contributors, BSD-2-Clause-Patent. The 608-byte command line variant is the layout iOSPlus10's shim
// loader (external/slop/shim/loader.c) writes for Darwin 22 and older.
// Changes: no UEFI; the window is a caller-supplied buffer; the device tree is edited as a tree
// (xnuloader/adt.h) instead of patched in its flat form; virt_base is a parameter, so a kernel can be
// handed the address at which the window is already mapped (xnuhost: XNU as a process, no MMU switch).
#ifndef XNULOADER_BOOT_H
#define XNULOADER_BOOT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "xnuloader/adt.h"
#include "xnuloader/macho.h"

// pexpert/pexpert/arm64/boot.h, Revision 2 Version 2; all fields little-endian.
#define XL_BOOT_LINE_LENGTH        1024   // Darwin 23 and later (xnu-10063 .. xnu-12377)
#define XL_BOOT_LINE_LENGTH_LEGACY 608    // Darwin 22 and older

typedef struct {
  uint16_t revision, version;
  uint32_t pad0;
  uint64_t virt_base, phys_base, mem_size, top_of_kernel_data;
  uint64_t video[6];                      // baseAddr, display, rowBytes, width, height, depth
  uint32_t machine_type, pad1;
  uint64_t device_tree;                   // a kernel virtual address
  uint32_t device_tree_length;
  char     command_line[XL_BOOT_LINE_LENGTH];
  uint32_t pad2;
  uint64_t boot_flags, mem_size_actual;
} xl_boot_args;

typedef struct {
  uint16_t revision, version;
  uint32_t pad0;
  uint64_t virt_base, phys_base, mem_size, top_of_kernel_data;
  uint64_t video[6];
  uint32_t machine_type, pad1;
  uint64_t device_tree;
  uint32_t device_tree_length;
  char     command_line[XL_BOOT_LINE_LENGTH_LEGACY];
  uint64_t boot_flags, mem_size_actual;
} xl_boot_args_legacy;

_Static_assert(offsetof(xl_boot_args, video) == 0x28, "video ABI");
_Static_assert(offsetof(xl_boot_args, device_tree) == 0x60, "DT ABI");
_Static_assert(offsetof(xl_boot_args, command_line) == 0x6c, "command line ABI");
_Static_assert(offsetof(xl_boot_args, mem_size_actual) == 0x478, "memory ABI");
_Static_assert(sizeof(xl_boot_args) == 0x480, "boot_args ABI");
_Static_assert(offsetof(xl_boot_args_legacy, command_line) == 108, "legacy command line ABI");
_Static_assert(sizeof(xl_boot_args_legacy) == 736, "legacy boot_args ABI");

typedef struct { const uint8_t *data; size_t size; } xl_blob;

// True for a well-formed trust cache sidecar: u32 count, u32 offsets[count], then version 2 modules
// (version, uuid[16], entry count, sorted 24-byte entries).
bool xl_trustcaches_valid(const uint8_t *data, size_t size);

// A "DBRDISK1" packed RAM disk keeps only its nonzero pages. Returns false for a malformed one.
// *expanded is the logical size (the input size for a plain disk); *packed says which it is.
bool xl_ramdisk_layout(const uint8_t *data, size_t size, uint64_t *expanded, bool *packed);
// Destination must be zeroed and hold *expanded bytes; the input must have passed xl_ramdisk_layout.
void xl_ramdisk_copy(uint8_t *destination, const uint8_t *data, size_t size, bool packed);

typedef struct {
  // The kernel image (whole file) and its parsed form.
  xl_blob          image;
  const xl_kernel *kernel;
  // The window: `window` bytes of zeroed, writable memory at host address `ram`, which the kernel
  // will see at physical address phys_base and at virtual address virt_base.
  // A firmware loader passes virt_base = kernel->base (the kernel maps itself there).
  // xnuhost passes the host address of the window, which must differ from kernel->base by a
  // multiple of 2 MiB (the slide the kernel finds when it rebases itself).
  uint8_t         *ram;
  uint64_t         window, phys_base, virt_base;
  // The device tree, edited in place: memory-map entries are filled in before it is flattened.
  xl_adt_node     *tree;
  const char      *command_line;
  uint64_t         video[6];
  uint64_t         boot_flags;
  bool             legacy_boot_args;      // 608-byte command line
  // Optional sidecars. A present sidecar needs its /chosen/memory-map property in the tree
  // (RAMDisk or PackedRAMDisk, TrustCache, GpuAddon, RootPolicies), 16 zero bytes; a tree that
  // names one whose sidecar is absent is refused, as DarwinBoot refuses it.
  xl_blob          ramdisk, trustcache, gpu_addon, root_policies;
} xl_boot_request;

typedef struct {
  uint64_t slide;                         // virt_base - kernel->base
  uint64_t entry_va, entry_pa;
  uint64_t args_pa, args_va;
  uint64_t tree_pa, tree_size;
  uint64_t ramdisk_pa, ramdisk_size;
  uint64_t trustcache_pa, gpu_addon_pa, root_policies_pa;
  uint64_t used;                          // bytes of the window in use: top_of_kernel_data - phys_base
} xl_boot_layout;

// Copies the kernel's segments, the device tree and the sidecars into the window and writes
// boot_args. Returns 0, or -1 with *error set. On failure the window's content is unspecified.
int xl_boot_place(const xl_boot_request *request, xl_boot_layout *layout, const char **error);

#endif
