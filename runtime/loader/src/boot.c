// SPDX-License-Identifier: BSD-2-Clause-Patent
// Ported from DarwinBoot (https://github.com/UniXNU/DarwinBoot, commit 8544957), Copyright (c) 2026
// DarwinBoot contributors. See include/xnuloader/boot.h for the list of changes.
#include "xnuloader/boot.h"
#include <string.h>

#define SIZE_4K   0x1000ull
#define SIZE_16K  0x4000ull
#define SIZE_2M   0x200000ull
#define ALIGN(v, a) (((v) + (a) - 1) & ~((uint64_t)(a) - 1))
#define FAIL(text) do { *error = (text); return -1; } while (0)

static uint32_t u32(const uint8_t *p) { uint32_t v; memcpy(&v, p, 4); return v; }
static uint64_t u64(const uint8_t *p) { uint64_t v; memcpy(&v, p, 8); return v; }

bool xl_trustcaches_valid(const uint8_t *data, size_t size)
{
  if (size < 8) return false;
  size_t count = u32(data);
  if (count == 0 || count > 128 || count > (size - 4) / 4) return false;
  for (size_t i = 0; i < count; i++) {
    size_t start = u32(data + 4 + i * 4);
    size_t end = i + 1 < count ? u32(data + 8 + i * 4) : size;
    if (start < 4 + count * 4 || (start & 3) || end > size || end < start || end - start < 24 || u32(data + start) != 2)
      return false;
    size_t entries = u32(data + start + 20);
    if (entries > (end - start - 24) / 24 || end - start != 24 + entries * 24) return false;
    for (size_t e = 0; e < entries; e++) {
      const uint8_t *hash = data + start + 24 + e * 24;
      if (hash[20] < 1 || hash[20] > 4 || (e != 0 && memcmp(hash - 24, hash, 20) >= 0)) return false;
    }
  }
  return true;
}

#define RAMDISK_MAX_LOGICAL (1ull << 40)

bool xl_ramdisk_layout(const uint8_t *data, size_t size, uint64_t *expanded, bool *packed)
{
  *expanded = size;
  *packed = false;
  if ((data == NULL) != (size == 0)) return false;
  if (size < 8 || memcmp(data, "DBRDISK1", 8)) return true;
  if (size < 32 || u32(data + 8) != 1 || u32(data + 12) != SIZE_4K) return false;
  uint64_t logical = u64(data + 16), count = u64(data + 24);
  if (logical == 0 || logical > RAMDISK_MAX_LOGICAL || (logical & (SIZE_4K - 1)) || count > 65536 ||
      count > (size - 32) / 16) return false;
  uint64_t cursor = 32 + count * 16, end = 0;
  for (uint64_t i = 0; i < count; i++) {
    uint64_t offset = u64(data + 32 + i * 16), length = u64(data + 40 + i * 16);
    if (!length || ((offset | length) & (SIZE_4K - 1)) || offset < end || offset > logical ||
        length > logical - offset || length > size - cursor) return false;
    end = offset + length;
    cursor += length;
  }
  if (cursor != size) return false;
  *expanded = logical;
  *packed = true;
  return true;
}

void xl_ramdisk_copy(uint8_t *destination, const uint8_t *data, size_t size, bool packed)
{
  if (!packed) { memcpy(destination, data, size); return; }
  uint64_t count = u64(data + 24), cursor = 32 + count * 16;
  for (uint64_t i = 0; i < count; i++) {
    uint64_t offset = u64(data + 32 + i * 16), length = u64(data + 40 + i * 16);
    memcpy(destination + offset, data + cursor, length);
    cursor += length;
  }
}

// The 16-byte (address, size) property of /chosen/memory-map for a sidecar, which must be present and
// zero when the sidecar is, and absent when it is not.
static int claim(xl_adt_node *map, const char *name, bool present, const xl_adt_prop **slot, const char **error)
{
  static const uint8_t zero[16];
  const xl_adt_prop *p = map ? xl_adt_get(map, name) : NULL;
  *slot = NULL;
  if (!present) {
    if (p) FAIL("the device tree names a memory-map sidecar that was not supplied");
    return 0;
  }
  if (!p || p->length != 16 || memcmp(p->value, zero, 16)) FAIL("a supplied sidecar has no empty memory-map property in the device tree");
  *slot = p;
  return 0;
}

static void fill(xl_adt_node *map, const xl_adt_prop *slot, uint64_t address, uint64_t size)
{
  uint64_t value[2] = { address, size };
  if (slot) xl_adt_set(map, slot->name, value, sizeof value);
}

int xl_boot_place(const xl_boot_request *r, xl_boot_layout *out, const char **error)
{
  const char *ignored;
  const xl_kernel *k = r->kernel;
  if (!error) error = &ignored;
  *error = NULL;
  memset(out, 0, sizeof *out);
  if (!k || !r->ram || !r->tree) FAIL("incomplete boot request");
  if ((r->window & (SIZE_2M - 1)) || r->window < 16 * SIZE_2M) FAIL("the window must be a multiple of 2 MiB, at least 32 MiB");
  if ((r->virt_base - k->base) & (SIZE_2M - 1)) FAIL("the kernel slide must be a multiple of 2 MiB");
  if ((r->phys_base ^ k->base) & (SIZE_2M - 1)) FAIL("the physical base must keep the kernel's offset inside its 2 MiB block");
  if (k->end - k->base > r->window - SIZE_2M) FAIL("the kernel does not fit the window");

  size_t line = r->legacy_boot_args ? XL_BOOT_LINE_LENGTH_LEGACY : XL_BOOT_LINE_LENGTH;
  const char *command = r->command_line ? r->command_line : "";
  size_t command_length = strlen(command);
  if (command_length >= line) FAIL("boot arguments too long");
  for (size_t i = 0; i < command_length; i++)
    if ((unsigned char)command[i] > 0x7f) FAIL("boot arguments are not ASCII");

  uint64_t ramdisk_expanded = 0;
  bool ramdisk_packed = false;
  if (r->ramdisk.data && !xl_ramdisk_layout(r->ramdisk.data, r->ramdisk.size, &ramdisk_expanded, &ramdisk_packed))
    FAIL("ramdisk layout rejected");

  xl_adt_node *map = xl_adt_find(r->tree, "chosen/memory-map");
  const xl_adt_prop *ramdisk_slot = NULL, *trust_slot, *addon_slot, *policy_slot;
  bool keep_packed = map && xl_adt_get(map, "PackedRAMDisk");
  if (keep_packed && map && xl_adt_get(map, "RAMDisk")) FAIL("the device tree names both RAMDisk and PackedRAMDisk");
  if (keep_packed && !ramdisk_packed) FAIL("the device tree asks for a packed ramdisk and the ramdisk is not packed");
  if (claim(map, keep_packed ? "PackedRAMDisk" : "RAMDisk", r->ramdisk.data != NULL, &ramdisk_slot, error) ||
      claim(map, "TrustCache", r->trustcache.data != NULL, &trust_slot, error) ||
      claim(map, "GpuAddon", r->gpu_addon.data != NULL, &addon_slot, error) ||
      claim(map, "RootPolicies", r->root_policies.data != NULL, &policy_slot, error)) return -1;
  if (r->trustcache.data && !xl_trustcaches_valid(r->trustcache.data, r->trustcache.size)) FAIL("trust cache sidecar rejected");
  if (r->gpu_addon.data && (r->gpu_addon.size < 24 || memcmp(r->gpu_addon.data, "NGPUADD2", 8) ||
                            u64(r->gpu_addon.data + 16) != r->gpu_addon.size)) FAIL("GPU addon sidecar rejected");
  if (r->root_policies.data) {
    const uint8_t *p = r->root_policies.data;
    if (r->root_policies.size < 32 || memcmp(p, "VROOTB01", 8) || u64(p + 8) != 1 || u64(p + 16) == 0 ||
        u64(p + 16) > 16 || u64(p + 24) != r->root_policies.size) FAIL("root policy sidecar rejected");
  }

  // Offsets inside the window, in DarwinBoot's order: kernel, boot_args, tree, ramdisk, trust
  // caches, GPU addon, root policies.
  uint64_t tree_size = xl_adt_flat_size(r->tree);
  uint64_t args = ALIGN(k->end - k->base, SIZE_16K);
  uint64_t tree = args + SIZE_4K;
  uint64_t ramdisk = ALIGN(tree + tree_size, SIZE_4K);
  uint64_t ramdisk_bytes = ALIGN(keep_packed ? r->ramdisk.size : ramdisk_expanded, SIZE_4K);
  if (ramdisk_bytes > r->window) FAIL("the ramdisk does not fit the window");
  uint64_t trust = ramdisk + ramdisk_bytes;
  uint64_t addon = ALIGN(trust + r->trustcache.size, SIZE_16K);
  uint64_t policy = ALIGN(addon + r->gpu_addon.size, SIZE_16K);
  uint64_t used = ALIGN(policy + r->root_policies.size, SIZE_16K);
  if (r->trustcache.size > r->window || r->gpu_addon.size > r->window || r->root_policies.size > r->window ||
      used > r->window - SIZE_2M) FAIL("the kernel and its sidecars do not fit the window");

  uint64_t pa = r->phys_base, va = r->virt_base;
  fill(map, ramdisk_slot, pa + ramdisk, keep_packed ? r->ramdisk.size : ramdisk_bytes);
  fill(map, trust_slot, pa + trust, r->trustcache.size);
  fill(map, addon_slot, pa + addon, r->gpu_addon.size);
  fill(map, policy_slot, pa + policy, r->root_policies.size);

  memset(r->ram, 0, used);
  for (unsigned i = 0; i < k->nseg; i++)
    memcpy(r->ram + (k->seg[i].vmaddr - k->base), r->image.data + k->seg[i].fileoff, k->seg[i].filesize);
  xl_adt_flatten(r->tree, r->ram + tree);
  if (r->ramdisk.data) {
    if (keep_packed) memcpy(r->ram + ramdisk, r->ramdisk.data, r->ramdisk.size);
    else xl_ramdisk_copy(r->ram + ramdisk, r->ramdisk.data, r->ramdisk.size, ramdisk_packed);
  }
  if (r->trustcache.data) memcpy(r->ram + trust, r->trustcache.data, r->trustcache.size);
  if (r->gpu_addon.data) memcpy(r->ram + addon, r->gpu_addon.data, r->gpu_addon.size);
  if (r->root_policies.data) memcpy(r->ram + policy, r->root_policies.data, r->root_policies.size);

  if (r->legacy_boot_args) {
    xl_boot_args_legacy a;
    memset(&a, 0, sizeof a);
    a.revision = 2; a.version = 2;
    a.virt_base = va; a.phys_base = pa; a.mem_size = r->window; a.top_of_kernel_data = pa + used;
    memcpy(a.video, r->video, sizeof a.video);
    a.device_tree = va + tree; a.device_tree_length = (uint32_t)tree_size;
    memcpy(a.command_line, command, command_length);
    a.boot_flags = r->boot_flags; a.mem_size_actual = r->window;
    memcpy(r->ram + args, &a, sizeof a);
  } else {
    xl_boot_args a;
    memset(&a, 0, sizeof a);
    a.revision = 2; a.version = 2;
    a.virt_base = va; a.phys_base = pa; a.mem_size = r->window; a.top_of_kernel_data = pa + used;
    memcpy(a.video, r->video, sizeof a.video);
    a.device_tree = va + tree; a.device_tree_length = (uint32_t)tree_size;
    memcpy(a.command_line, command, command_length);
    a.boot_flags = r->boot_flags; a.mem_size_actual = r->window;
    memcpy(r->ram + args, &a, sizeof a);
  }

  out->slide = va - k->base;
  out->entry_va = va + (k->entry - k->base);
  out->entry_pa = pa + (k->entry - k->base);
  out->args_pa = pa + args;
  out->args_va = va + args;
  out->tree_pa = pa + tree;
  out->tree_size = tree_size;
  out->ramdisk_pa = r->ramdisk.data ? pa + ramdisk : 0;
  out->ramdisk_size = r->ramdisk.data ? (keep_packed ? r->ramdisk.size : ramdisk_bytes) : 0;
  out->trustcache_pa = r->trustcache.data ? pa + trust : 0;
  out->gpu_addon_pa = r->gpu_addon.data ? pa + addon : 0;
  out->root_policies_pa = r->root_policies.data ? pa + policy : 0;
  out->used = used;
  return 0;
}
