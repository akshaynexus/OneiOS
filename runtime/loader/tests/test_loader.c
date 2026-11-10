// SPDX-License-Identifier: MIT
// Loader unit tests on generated fixtures (tests/fixture.h): no Apple files.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "fixture.h"
#include "xnuloader/adt.h"
#include "xnuloader/boot.h"
#include "xnuloader/machine.h"
#include "xnuloader/macho.h"

static int failures, checks;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

static xl_kernel K;   // large: static

static void test_macho(void)
{
  const char *err = "";
  fx_image im = fx_kernel(FX_BASE, 0, NULL, 0, 1);
  CHECK(xl_macho_parse(im.data, im.size, &K, &err) == 0);
  CHECK(K.nseg == 2 && K.base == FX_BASE && K.end == FX_BASE + 0x5000 && K.entry == FX_BASE + 0x1000);
  CHECK(!K.fileset && K.cpusubtype == 0 && !strcmp(K.seg[0].name, "__TEXT") && K.seg[1].filesize == 0);
  uint64_t v = 0;
  CHECK(xl_macho_symbol(im.data, im.size, 0, "_fixture_symbol", &v) == 0 && v == FX_BASE + 0x1008);
  CHECK(xl_macho_symbol(im.data, im.size, 0, "_absent", &v) == -1);
  CHECK(xl_macho_offset(&K, FX_BASE + 0x1000, 16) == 0x1000);
  CHECK(xl_macho_offset(&K, FX_BASE + 0x4000, 16) == -1);          // zero-fill has no file bytes

  // arm64e with capability bits in the subtype's top byte
  fx_w32(im.data + 8, 0x80000002u);
  CHECK(xl_macho_parse(im.data, im.size, &K, &err) == 0 && K.cpusubtype == 2);
  fx_w32(im.data + 8, 0);

  // DarwinBoot's rejected variants
  struct { size_t off; int wide; uint64_t value; } bad[] = {
    { 0, 0, 0xfeedface },                         // 32-bit magic
    { 4, 0, 0x01000007 },                         // x86_64
    { 8, 0, 3 },                                  // bad subtype
    { 12, 0, 6 },                                 // MH_DYLIB
    { 20, 0, (uint64_t)im.size },                 // commands past the file
    { 32 + 72 + 24, 1, FX_BASE + 0x1000 },        // __DATA overlaps __TEXT
    { 32 + 144 + 272, 1, FX_BASE + 0x4000 },      // entry in zero-fill, not executable
    { 32 + 144 + 272, 1, FX_BASE + 0x1002 },      // unaligned entry
    { 32 + 40, 1, 0x100000 },                     // segment file offset outside the file
    { 32 + 4, 0, 71 },                            // command length not a multiple of 8
  };
  for (size_t i = 0; i < sizeof bad / sizeof *bad; i++) {
    uint8_t saved[8];
    memcpy(saved, im.data + bad[i].off, 8);
    if (bad[i].wide) fx_w64(im.data + bad[i].off, bad[i].value); else fx_w32(im.data + bad[i].off, (uint32_t)bad[i].value);
    err = NULL;
    CHECK(xl_macho_parse(im.data, im.size, &K, &err) == -1 && err != NULL);
    memcpy(im.data + bad[i].off, saved, 8);
  }
  CHECK(xl_macho_parse(im.data, 16, &K, &err) == -1);
  CHECK(xl_macho_parse(im.data, im.size, &K, &err) == 0);
  free(im.data);
}

static void test_fileset(void)
{
  const char *err = "";
  fx_image im = fx_collection(FX_BASE, 2, NULL, 0);
  CHECK(xl_macho_parse(im.data, im.size, &K, &err) == 0);
  if (err) printf("  fileset: %s\n", err);
  CHECK(K.fileset && K.nseg == 4 && K.nentries == 2 && K.kernel_header == 0x4000);
  CHECK(!strcmp(K.entries[0].name, "com.apple.kernel") && !strcmp(K.entries[1].name, "com.example.kext"));
  CHECK(K.entries[1].vmaddr == FX_BASE + 0xc000 && K.entry == FX_BASE + 0x5000 && K.base == FX_BASE);
  uint64_t v = 0;
  CHECK(xl_macho_symbol(im.data, im.size, K.kernel_header, "_fixture_symbol", &v) == 0 && v == FX_BASE + 0x5008);

  uint8_t *entry0 = im.data + 32 + 4 * 72 + 288;
  uint64_t saved = 0;
  memcpy(&saved, entry0 + 8, 8);
  fx_w64(entry0 + 8, FX_BASE + 0x8000);           // member header not mapped where the entry says
  CHECK(xl_macho_parse(im.data, im.size, &K, &err) == -1);
  fx_w64(entry0 + 8, saved);
  entry0[32 + 15] = 'x';                          // no com.apple.kernel
  CHECK(xl_macho_parse(im.data, im.size, &K, &err) == -1);
  entry0[32 + 15] = 'l';
  fx_w32(im.data + 0x4000 + 24, 0);               // the kernel member lost MH_DYLIB_IN_CACHE
  CHECK(xl_macho_parse(im.data, im.size, &K, &err) == -1);
  fx_w32(im.data + 0x4000 + 24, 0x80000000u);
  fx_w64(im.data + 0x4000 + 32 + 144 + 272, FX_BASE + 0x5004);    // member entry differs
  CHECK(xl_macho_parse(im.data, im.size, &K, &err) == -1);
  fx_w64(im.data + 0x4000 + 32 + 144 + 272, FX_BASE + 0x5000);
  CHECK(xl_macho_parse(im.data, im.size, &K, &err) == 0);
  free(im.data);
}

static void test_adt(void)
{
  xl_adt_node *root = xl_adt_new("device-tree");
  xl_adt_set_u32(root, "#address-cells", 2);
  xl_adt_node *map = xl_adt_path(root, "chosen/memory-map");
  uint8_t zero[16] = { 0 };
  xl_adt_set(map, "RAMDisk", zero, 16);
  xl_adt_set_str(xl_adt_path(root, "chosen"), "osenvironment", "main");
  xl_adt_set(xl_adt_path(root, "arm-io/dev"), "dma-coherent", "", 0);
  xl_adt_set(xl_adt_path(root, "arm-io/dev"), "odd", "abcde", 5);       // padded to 8 in the flat form
  CHECK(xl_adt_set(root, "a-property-name-that-is-longer-than-31", "", 0) == -1);
  CHECK(xl_adt_find(root, "chosen/memory-map") == map && xl_adt_find(root, "chosen/none") == NULL);

  size_t size = xl_adt_flat_size(root);
  uint8_t *flat = malloc(size);
  xl_adt_flatten(root, flat);
  // the layout XNU reads: root = 2 properties ("name" first), 2 children
  uint32_t head[2];
  memcpy(head, flat, 8);
  CHECK(head[0] == 2 && head[1] == 2 && !memcmp(flat + 8, "name", 5));
  uint32_t namelen;
  memcpy(&namelen, flat + 8 + 32, 4);
  CHECK(namelen == 12 && !memcmp(flat + 8 + 36, "device-tree", 12));
  CHECK(size % 4 == 0);

  const char *err = NULL;
  xl_adt_node *back = xl_adt_parse(flat, size, &err);
  CHECK(back != NULL);
  if (back) {
    uint8_t *again = malloc(size);
    CHECK(xl_adt_flat_size(back) == size);
    xl_adt_flatten(back, again);
    CHECK(!memcmp(flat, again, size));
    const xl_adt_prop *p = xl_adt_get(xl_adt_find(back, "arm-io/dev"), "odd");
    CHECK(p && p->length == 5 && !memcmp(p->value, "abcde", 5));
    CHECK(xl_adt_remove(back, "arm-io") == 0 && xl_adt_find(back, "arm-io") == NULL && xl_adt_remove(back, "arm-io") == -1);
    CHECK(xl_adt_unset(back, "#address-cells") == 0 && !xl_adt_get(back, "#address-cells"));
    free(again);
    xl_adt_free(back);
  }
  // malformed flat trees
  CHECK(xl_adt_parse(flat, size - 4, &err) == NULL && err);
  CHECK(xl_adt_parse(flat, 4, &err) == NULL);
  uint8_t *bad = malloc(size + 8);
  memcpy(bad, flat, size);
  memset(bad + size, 0, 8);
  CHECK(xl_adt_parse(bad, size + 8, &err) == NULL);                     // trailing bytes
  memcpy(bad, flat, size);
  uint32_t huge = 0x7ffffff0;
  memcpy(bad + 8 + 32, &huge, 4);
  CHECK(xl_adt_parse(bad, size, &err) == NULL);                         // property past the tree
  memcpy(bad, flat, size);
  huge = 0xffffff;
  memcpy(bad + 4, &huge, 4);
  CHECK(xl_adt_parse(bad, size, &err) == NULL);                         // child count past the tree
  free(bad);
  free(flat);
  xl_adt_free(root);
}

static void machine_default(xl_machine *m)
{
  static const xl_device_kind kinds[] = { XL_DEV_BLOCK, XL_DEV_BLOCK, XL_DEV_DATA_STORE, XL_DEV_INPUT, XL_DEV_GPU };
  memset(m, 0, sizeof *m);
  m->cpu_cluster[2] = 'E';
  xl_machine_board(m, 4, kinds, 5);
  m->timebase_hz = 19200000;
  m->dram_base = 0x800004000ull;
  m->dram_size = 0x40000000;
  m->trust_cache = true;
  m->model = "iPhone18,2";
  m->root_volume_uuid = "0a1b2c3d-0000-4000-8000-00000000abcd";
  m->host_vector = 0x2000100000ull;
  m->user_va_limit = 0x7e00000000ull;
  m->host_page_size = 4096;
  for (int i = 0; i < 256; i++) m->random_seed[i] = (uint8_t)i;
}

static void test_machine(void)
{
  xl_machine m;
  const char *err = NULL;
  machine_default(&m);
  xl_adt_node *t = xl_machine_tree(&m, &err);
  CHECK(t != NULL);
  if (!t) { printf("  machine: %s\n", err); return; }
  // what ArmDTPlatform::configure requires
  CHECK(xl_adt_find(t, "cpus") && xl_adt_find(t, "arm-io") && xl_adt_find(t, "defaults") && xl_adt_find(t, "options"));
  const xl_adt_node *chosen = xl_adt_find(t, "chosen");
  const xl_adt_prop *p = xl_adt_get(chosen, "nvram-bank-size");
  CHECK(p && p->length == 4);
  p = xl_adt_get(chosen, "nvram-proxy-data");
  CHECK(p && p->length == 8192 && p->value[0] == 0x70 && !memcmp(p->value + 4, "nvram", 5));
  CHECK(xl_adt_get(chosen, "random-seed")->length == 256);
  p = xl_adt_get(chosen, "root-matching");
  CHECK(p && strstr((const char *)p->value, "<string>0A1B2C3D-0000-4000-8000-00000000ABCD</string>"));
  // what ArmCpu reads
  const xl_adt_node *cpus = xl_adt_find(t, "cpus");
  CHECK(cpus->nchildren == 4);
  CHECK(!strcmp((const char *)xl_adt_get(cpus->children[0], "state")->value, "running"));
  CHECK(!strcmp((const char *)xl_adt_get(cpus->children[3], "state")->value, "waiting"));
  CHECK(!strcmp((const char *)xl_adt_get(cpus->children[2], "cluster-type")->value, "E"));
  uint32_t v32;
  memcpy(&v32, xl_adt_get(cpus->children[1], "timebase-frequency")->value, 4);
  CHECK(v32 == 19200000);
  const xl_adt_node *intc = xl_adt_find(t, "arm-io/intc");
  p = xl_adt_get(intc, "reg");
  uint64_t r[4];
  CHECK(p && p->length == 32);
  memcpy(r, p->value, 32);
  CHECK(r[0] == XL_BOARD_GIC_DIST - 0x1000 && r[1] >= 0x10000 && r[2] == XL_BOARD_GIC_REDIST - 0x1000 && r[3] == 4 * 0x20000);
  CHECK(!memcmp(xl_adt_get(intc, "compatible")->value, "arm,gic-v3", 11));
  CHECK(!strcmp((const char *)xl_adt_get(xl_adt_find(t, "psci"), "method")->value, "hvc"));
  // the devices, by the names and compatibles their drivers match
  p = xl_adt_get(xl_adt_find(t, "arm-io/virtio-mmio-1"), "compatible");
  CHECK(p && p->length == 12 && !memcmp(p->value, "virtio,mmio", 12));
  CHECK(xl_adt_find(t, "arm-io/data-store") && xl_adt_find(t, "arm-io/input-0") && xl_adt_find(t, "arm-io/virtio-gpu"));
  CHECK(!xl_adt_get(xl_adt_find(t, "arm-io/virtio-gpu"), "drm-host-pool"));                 // no 3D renderer: no pool
  p = xl_adt_get(xl_adt_find(t, "arm-io/data-store"), "reg");
  memcpy(r, p->value, 16);
  CHECK(r[0] == XL_BOARD_VIRTIO + 2 * XL_BOARD_VIRTIO_SIZE - 0x1000 && r[1] == XL_BOARD_VIRTIO_SIZE);
  uint32_t intid;
  memcpy(&intid, xl_adt_get(xl_adt_find(t, "arm-io/data-store"), "droidvm,intid")->value, 4);
  CHECK(intid == XL_BOARD_VIRTIO_SPI + 2);
  memcpy(&v32, xl_adt_get(xl_adt_find(t, "bluetooth"), "transport-encoding")->value, 4);
  CHECK(v32 == 10 && !xl_adt_get(xl_adt_find(t, "bluetooth"), "transport-speed") && !xl_adt_find(t, "arm-io/cpufreq"));
  // memory-map placeholders and the host node
  const xl_adt_node *map = xl_adt_find(t, "chosen/memory-map");
  CHECK(xl_adt_get(map, "TrustCache") && !xl_adt_get(map, "RAMDisk"));
  p = xl_adt_get(xl_adt_find(t, "chosen/xnuhost"), "vector");
  uint64_t v64;
  memcpy(&v64, p->value, 8);
  CHECK(v64 == 0x2000100000ull);
  // it flattens and parses back
  size_t size = xl_adt_flat_size(t);
  uint8_t *flat = malloc(size);
  xl_adt_flatten(t, flat);
  xl_adt_node *back = xl_adt_parse(flat, size, &err);
  CHECK(back != NULL);
  xl_adt_free(back);
  free(flat);
  xl_adt_free(t);

  // identity from a donor tree: model, target-type, board first in compatible, /product filtered
  xl_adt_node *donor = xl_adt_new("device-tree");
  xl_adt_set_str(donor, "model", "Fixture1,1");
  xl_adt_set_str(donor, "target-type", "F1");
  xl_adt_set(donor, "compatible", "F1AP\0Fixture1,1\0AppleARM", sizeof "F1AP\0Fixture1,1\0AppleARM");
  xl_adt_node *product = xl_adt_add(donor, "product");
  xl_adt_set_str(product, "product-name", "Fixture phone");
  xl_adt_set_u32(product, "has-exclaves", 1);
  xl_adt_set_str(product, "graphics-featureset-fallbacks", "MTL1,2:APPLE2:MTL2,2:GLES2,0");
  xl_adt_set_u32(xl_adt_add(product, "audio"), "acoustic-id", 77);
  m.identity = donor;
  t = xl_machine_tree(&m, &err);
  CHECK(t != NULL);
  if (t) {
    CHECK(!strcmp((const char *)xl_adt_get(t, "model")->value, "Fixture1,1"));
    p = xl_adt_get(t, "compatible");
    CHECK(p && !memcmp(p->value, "F1AP\0oneios,xnuhost", sizeof "F1AP\0oneios,xnuhost"));
    const xl_adt_node *pr = xl_adt_find(t, "product");
    CHECK(!xl_adt_get(pr, "has-exclaves") && xl_adt_get(pr, "product-name"));
    CHECK(!strcmp((const char *)xl_adt_get(pr, "graphics-featureset-fallbacks")->value, "MTL1,2:GLES2,0"));
    CHECK(xl_adt_get(xl_adt_find(t, "product/audio"), "acoustic-id"));
    xl_adt_free(t);
  }
  xl_adt_remove(donor, "product");
  CHECK(xl_machine_tree(&m, &err) == NULL && err);
  xl_adt_free(donor);

  // the bridged Bluetooth controller, the cpufreq windows and the bridge channel
  machine_default(&m);
  m.bt_transport = 3; m.bt_speed = 3000000; memcpy(m.bt_mac, "\x10\x20\x30\x40\x50\x60", 6);
  m.cpufreq_base = XL_BOARD_CPUFREQ; m.cpufreq_size = 4 * 0x1000;
  m.dev[4].kind = XL_DEV_BRIDGE;
  t = xl_machine_tree(&m, &err);
  CHECK(t != NULL);
  if (t) {
    const xl_adt_node *b = xl_adt_find(t, "bluetooth");
    memcpy(&v32, xl_adt_get(b, "transport-encoding")->value, 4);
    CHECK(v32 == 3 && !memcmp(xl_adt_get(b, "local-mac-address")->value, m.bt_mac, 6) && xl_adt_get(b, "transport-speed"));
    CHECK(!memcmp(xl_adt_get(xl_adt_find(t, "arm-io/bridge"), "compatible")->value, "oneios,bridge", 14));
    memcpy(r, xl_adt_get(xl_adt_find(t, "arm-io/cpufreq"), "reg")->value, 16);
    CHECK(r[0] == XL_BOARD_CPUFREQ - 0x1000 && r[1] == 0x4000);
    uint8_t wifi[6], eth[6];
    xl_machine_mac(&m, 1, wifi); xl_machine_mac(&m, 2, eth);
    CHECK(wifi[0] == 0x02 && memcmp(wifi, eth, 6) && !memcmp(xl_adt_get(xl_adt_find(t, "chosen"), "mac-address-wifi0")->value, wifi, 6));
    xl_adt_free(t);
  }

  // refused machines
  machine_default(&m); m.ncpu = 9;
  CHECK(xl_machine_tree(&m, &err) == NULL);
  machine_default(&m); m.timebase_hz = 0;
  CHECK(xl_machine_tree(&m, &err) == NULL);
  machine_default(&m); m.armio_base = 0;
  CHECK(xl_machine_tree(&m, &err) == NULL);
  machine_default(&m); m.dev[0].base = 0x50000000;
  CHECK(xl_machine_tree(&m, &err) == NULL);
  machine_default(&m); m.dev[1].kind = XL_DEV_GPU;
  CHECK(xl_machine_tree(&m, &err) == NULL);     // two GPUs
  // the 3D renderer's pool: base and size on the GPU's node, as UniXNU-droidvm's DroidVMVirtioGpu3D.cpp reads them
  machine_default(&m); m.gpu_pool_base = 0x8ff800000ull; m.gpu_pool_size = 8 << 20;
  xl_adt_node *gt = xl_machine_tree(&m, &err);
  const xl_adt_prop *pool = gt ? xl_adt_get(xl_adt_find(gt, "arm-io/virtio-gpu"), "drm-host-pool") : NULL;
  uint64_t pv[2] = { 0, 0 };
  if (pool && pool->length == 16) memcpy(pv, pool->value, 16);
  CHECK(pv[0] == 0x8ff800000ull && pv[1] == 8 << 20);
  xl_adt_free(gt);
}

static void test_sidecars(void)
{
  fx_image tc = fx_trustcache(3);
  CHECK(xl_trustcaches_valid(tc.data, tc.size));
  fx_w32(tc.data + 8, 3);
  CHECK(!xl_trustcaches_valid(tc.data, tc.size));     // version
  fx_w32(tc.data + 8, 2);
  fx_w32(tc.data + 28, 0xffffffff);
  CHECK(!xl_trustcaches_valid(tc.data, tc.size));     // count
  fx_w32(tc.data + 28, 3);
  fx_w32(tc.data + 4, 0xfffffff0);
  CHECK(!xl_trustcaches_valid(tc.data, tc.size));     // offset
  fx_w32(tc.data + 4, 8);
  memset(tc.data + 32 + 24, 0x05, 20);
  CHECK(!xl_trustcaches_valid(tc.data, tc.size));     // order
  CHECK(!xl_trustcaches_valid(tc.data, 4));
  free(tc.data);

  // packed ramdisk: 16 pages logical, two extents
  size_t size = 32 + 2 * 16 + 2 * 4096;
  uint8_t *rd = calloc(1, size);
  memcpy(rd, "DBRDISK1", 8);
  fx_w32(rd + 8, 1); fx_w32(rd + 12, 4096); fx_w64(rd + 16, 16 * 4096); fx_w64(rd + 24, 2);
  fx_w64(rd + 32, 4096); fx_w64(rd + 40, 4096); fx_w64(rd + 48, 8 * 4096); fx_w64(rd + 56, 4096);
  memset(rd + 64, 0xaa, 4096);
  memset(rd + 64 + 4096, 0xbb, 4096);
  uint64_t expanded = 0;
  bool packed = false;
  CHECK(xl_ramdisk_layout(rd, size, &expanded, &packed) && packed && expanded == 16 * 4096);
  uint8_t *out = calloc(1, expanded);
  xl_ramdisk_copy(out, rd, size, packed);
  CHECK(out[0] == 0 && out[4096] == 0xaa && out[8191] == 0xaa && out[8192] == 0 && out[8 * 4096] == 0xbb);
  free(out);
  fx_w64(rd + 48, 4096);                               // overlapping extents
  CHECK(!xl_ramdisk_layout(rd, size, &expanded, &packed));
  fx_w64(rd + 48, 8 * 4096);
  CHECK(!xl_ramdisk_layout(rd, size - 1, &expanded, &packed));
  CHECK(xl_ramdisk_layout((const uint8_t *)"plain disk", 10, &expanded, &packed) && !packed && expanded == 10);
  free(rd);
}

static void test_place(void)
{
  const char *err = NULL;
  static const uint8_t code[] = { 0x1f, 0x20, 0x03, 0xd5, 0xc0, 0x03, 0x5f, 0xd6 };    // nop; ret
  fx_image im = fx_kernel(FX_BASE, 0, code, sizeof code, 0);
  CHECK(xl_macho_parse(im.data, im.size, &K, &err) == 0);
  xl_machine m;
  machine_default(&m);
  m.ramdisk = true;
  const uint64_t window = 64ull << 20;
  uint8_t *ram = calloc(1, window);
  fx_image tc = fx_trustcache(2);
  uint8_t disk[4097];
  memset(disk, 0x5a, sizeof disk);

  for (int hosted = 0; hosted < 2; hosted++) {
    for (int legacy = 0; legacy < 2; legacy++) {
      xl_adt_node *tree = xl_machine_tree(&m, &err);
      xl_boot_request r = { .image = { im.data, im.size }, .kernel = &K, .ram = ram, .window = window,
                            .phys_base = 0x800004000ull,
                            .virt_base = hosted ? 0x2000004000ull : FX_BASE,
                            .tree = tree, .command_line = "debug=0x8 -v", .legacy_boot_args = legacy,
                            .video = { 0x900000000ull, 1, 4 * 1080, 1080, 2340, 32 },
                            .ramdisk = { disk, sizeof disk }, .trustcache = { tc.data, tc.size } };
      xl_boot_layout l;
      CHECK(xl_boot_place(&r, &l, &err) == 0);
      if (err) { printf("  place: %s\n", err); xl_adt_free(tree); continue; }
      CHECK(l.slide == r.virt_base - FX_BASE && l.entry_va == r.virt_base + 0x1000 && l.entry_pa == r.phys_base + 0x1000);
      CHECK(!memcmp(ram + 0x1000, code, sizeof code));                         // __TEXT copied
      CHECK(l.args_pa == r.phys_base + 0x8000 && l.tree_pa == l.args_pa + 0x1000);    // 16 KiB after the kernel's end
      const uint8_t *args = ram + (l.args_pa - r.phys_base);
      uint64_t f[4];
      memcpy(f, args + 8, 32);
      CHECK(args[0] == 2 && args[2] == 2 && f[0] == r.virt_base && f[1] == r.phys_base && f[2] == window &&
            f[3] == r.phys_base + l.used);
      uint64_t dt, video0, actual;
      uint32_t dtlen;
      memcpy(&video0, args + 0x28, 8);
      memcpy(&dt, args + 0x60, 8);
      memcpy(&dtlen, args + 0x68, 4);
      memcpy(&actual, args + (legacy ? 728 : 0x478), 8);
      CHECK(video0 == 0x900000000ull && dt == r.virt_base + (l.tree_pa - r.phys_base) && dtlen == l.tree_size && actual == window);
      CHECK(!strcmp((const char *)args + 0x6c, "debug=0x8 -v"));
      // the tree in the window parses and its memory-map names the copies
      xl_adt_node *placed = xl_adt_parse(ram + (l.tree_pa - r.phys_base), l.tree_size, &err);
      CHECK(placed != NULL);
      if (placed) {
        uint64_t pair[2];
        memcpy(pair, xl_adt_get(xl_adt_find(placed, "chosen/memory-map"), "RAMDisk")->value, 16);
        CHECK(pair[0] == l.ramdisk_pa && pair[1] == 0x2000 && ram[pair[0] - r.phys_base + 4096] == 0x5a &&
              ram[pair[0] - r.phys_base + 4097] == 0);
        memcpy(pair, xl_adt_get(xl_adt_find(placed, "chosen/memory-map"), "TrustCache")->value, 16);
        CHECK(pair[0] == l.trustcache_pa && pair[1] == tc.size && !memcmp(ram + (pair[0] - r.phys_base), tc.data, tc.size));
        xl_adt_free(placed);
      }
      CHECK(l.used % 0x4000 == 0 && l.used <= window - 0x200000);
      xl_adt_free(tree);
    }
  }

  // refusals
  xl_adt_node *tree = xl_machine_tree(&m, &err);
  xl_boot_request r = { .image = { im.data, im.size }, .kernel = &K, .ram = ram, .window = window,
                        .phys_base = 0x800004000ull, .virt_base = FX_BASE, .tree = tree,
                        .ramdisk = { disk, sizeof disk }, .trustcache = { tc.data, tc.size } };
  xl_boot_layout l;
  xl_boot_request b = r;
  b.trustcache = (xl_blob){ 0 };
  CHECK(xl_boot_place(&b, &l, &err) == -1);          // tree names a trust cache that is not supplied
  b = r; b.virt_base = FX_BASE + 0x4000;
  CHECK(xl_boot_place(&b, &l, &err) == -1);          // slide not a multiple of 2 MiB
  b = r; b.phys_base = 0x800000000ull;
  CHECK(xl_boot_place(&b, &l, &err) == -1);          // physical base loses the 2 MiB offset
  b = r; b.window = 16ull << 20;
  CHECK(xl_boot_place(&b, &l, &err) == -1);          // window too small
  b = r; b.command_line = "caf\xc3\xa9";
  CHECK(xl_boot_place(&b, &l, &err) == -1);          // not ASCII
  char longline[1100];
  memset(longline, 'a', sizeof longline - 1);
  longline[sizeof longline - 1] = 0;
  b = r; b.command_line = longline;
  CHECK(xl_boot_place(&b, &l, &err) == -1);
  tc.data[8] = 9;
  CHECK(xl_boot_place(&r, &l, &err) == -1);          // bad trust cache
  tc.data[8] = 2;
  uint8_t stale[16] = { 1 };
  xl_adt_set(xl_adt_find(tree, "chosen/memory-map"), "RAMDisk", stale, 16);
  CHECK(xl_boot_place(&r, &l, &err) == -1);          // stale memory-map entry
  xl_adt_free(tree);
  free(tc.data);
  free(ram);
  free(im.data);
}

int main(void)
{
  test_macho();
  test_fileset();
  test_adt();
  test_machine();
  test_sidecars();
  test_place();
  printf("loader tests: %d checks, %d failed\n", checks, failures);
  return failures != 0;
}
