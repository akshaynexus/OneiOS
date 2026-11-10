// SPDX-License-Identifier: BSD-2-Clause-Patent
// The device tree of the machine xnuhost presents to XNU.
// Ported from UniXNU-droidvm (https://github.com/Droid-VM/UniXNU-droidvm, commit f9121cd,
// ipados27/scripts/make-droidvm-adt.py), BSD-2-Clause-Patent: the same nodes ArmDTPlatform and ArmCpu
// consume (/chosen with a CHRP NVRAM proxy, /cpus, /psci, /defaults, /options, /product, /arm-io with
// the GICv3 and the virtio-mmio windows). Changes: C instead of Python; the hardware values come from
// xnuhost's own board instead of a crosvm FDT; the identity is an iPhone's, not an iPad's; a
// /chosen/xnuhost node tells a hosted kernel where the host-call vector is.
#ifndef XNULOADER_MACHINE_H
#define XNULOADER_MACHINE_H

#include <stdbool.h>
#include <stdint.h>
#include "xnuloader/adt.h"

#define XL_MAX_CPUS    8        // ArmCpu starts at most eight cores
#define XL_MAX_DEVICES 16

typedef enum {
  XL_DEV_BLOCK,       // "virtio,mmio": read-only image, NativeVirtioBlock
  XL_DEV_DATA_STORE,  // "droidvm,data-store": the writable Data store disk
  XL_DEV_INPUT,       // "droidvm,virtio-input"
  XL_DEV_NET,         // "droidvm,net"
  XL_DEV_GPU,         // "droidvm,virtio-gpu"
  XL_DEV_SND,         // "droidvm,virtio-snd"
  XL_DEV_BRIDGE,      // "oneios,bridge": the message channel of runtime/bridges (haptics, Bluetooth HCI)
} xl_device_kind;

typedef struct { xl_device_kind kind; uint64_t base, size; } xl_device;

typedef struct {
  unsigned    ncpu;
  uint32_t    cpu_reg[XL_MAX_CPUS];       // MPIDR affinity of each CPU
  char        cpu_cluster[XL_MAX_CPUS];   // 'P' or 'E'
  uint32_t    timebase_hz;                // CNTFRQ_EL0 of the host
  uint64_t    dram_base, dram_size;       // the kernel's memory window (physical)
  uint64_t    armio_base, armio_size;     // must not start at 0 (pexpert takes it as the SoC base)
  uint64_t    gic_dist, gic_dist_size, gic_redist, gic_redist_size, gic_redist_stride;
  uint64_t    uart_base, uart_size;       // PL011, node "uart0"; size 0 for none
  const char *psci_method;                // "hvc" or "smc"
  xl_device   dev[XL_MAX_DEVICES];
  unsigned    ndev;
  uint64_t    cpufreq_base, cpufreq_size; // /arm-io/cpufreq: one 4 KiB window per CPU; size 0 for none
  // /bluetooth: transport-encoding 10 (the virtual transport, no controller) when bt_transport is 0;
  // else that encoding (3 = the phone's controller behind the bridge) with bt_speed and bt_mac.
  uint32_t    bt_transport, bt_speed;
  uint8_t     bt_mac[6];
  uint64_t    panic_base, panic_size;     // /pram; size 0 for none
  uint8_t     random_seed[256];           // xnu-12377 takes 4 x 64 seed bytes
  bool        have_chip_id;
  uint64_t    unique_chip_id;
  uint32_t    chip_id;
  const char *root_volume_uuid;           // /chosen root-matching by APFS volume UUID, or NULL
  // memory-map placeholders the loader fills (xl_boot_place)
  bool        ramdisk, packed_ramdisk, trust_cache, gpu_addon, root_policies;
  // Identity. `identity` is the device's own decoded device tree (the IPSW's DeviceTree payload), from
  // which model, target-type, the board name and /product are taken; NULL gives a generic machine
  // with model/product names from the strings below (which may be NULL).
  const xl_adt_node *identity;
  const char *model;                      // "iPhone18,2"
  const char *display_gamut;              // /product artwork-display-gamut, default "P3"
  const char *text_fixups;                // NUL-separated list for /chosen text-fixups, with its length
  uint32_t    text_fixups_length;
  // xnuhost: where a hosted kernel finds the host (0 = not hosted, node omitted)
  uint64_t    host_vector;                // virtual address of struct xh_vector
  uint64_t    host_io_base, host_io_size; // physical range of the trapped MMIO aperture
  uint64_t    user_va_limit;              // first address iOS tasks may not use
  uint32_t    host_page_size;
} xl_machine;

// The board xnuhost emulates: where its windows are (physical addresses inside /arm-io).
#define XL_BOARD_ARMIO_BASE     0x1000ull
#define XL_BOARD_ARMIO_SIZE     (0x40000000ull - XL_BOARD_ARMIO_BASE)
#define XL_BOARD_UART           0x00006000ull
#define XL_BOARD_UART_SIZE      0x1000ull
#define XL_BOARD_GIC_DIST       0x08000000ull
#define XL_BOARD_GIC_DIST_SIZE  0x10000ull
#define XL_BOARD_GIC_REDIST     0x080a0000ull
#define XL_BOARD_GIC_STRIDE     0x20000ull
#define XL_BOARD_CPUFREQ        0x09000000ull   // + cpu * 0x1000
#define XL_BOARD_VIRTIO         0x0a000000ull
#define XL_BOARD_VIRTIO_SIZE    0x1000ull   // one page per device, so each is its own trapped page
#define XL_BOARD_IO_BASE        XL_BOARD_GIC_DIST
#define XL_BOARD_IO_SIZE        0x03000000ull
#define XL_BOARD_VIRTIO_SPI     32u         // first SPI INTID of the virtio windows, one each
#define XL_BOARD_UART_SPI       31u         // not wired by default

// Fills the windows of *m for `ncpu` CPUs and the given devices from the XL_BOARD_* layout.
void xl_machine_board(xl_machine *m, unsigned ncpu, const xl_device_kind *kinds, unsigned ndev);

// The guest's stable addresses, from its unique chip ID: which = 0 Bluetooth, 1 Wi-Fi (en0), 2 Ethernet.
void xl_machine_mac(const xl_machine *m, unsigned which, uint8_t mac[6]);

// Builds the tree. NULL with *error set for a machine that cannot be described.
xl_adt_node *xl_machine_tree(const xl_machine *m, const char **error);

// A fresh volatile CHRP NVRAM bank for IODTNVRAM (8192 bytes written to `image`).
void xl_nvram_proxy(uint8_t image[8192]);

#endif
