// SPDX-License-Identifier: MIT
// Unit tests of xnuhost's portable parts on the build host: instruction decoding, device emulation,
// the interrupt controller, the virtio devices (driven as a guest driver would), the seccomp filter
// (run in a small BPF interpreter), configuration and the disk store. No Apple files.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include "xnuhost.h"
#include "xnuhost/free_report.h"
#include "stub_abi.h"
#include <sys/mman.h>
#include <sys/syscall.h>

static int failures, checks;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

// ---- what the rest of xnuhost provides ----------------------------------------------------------
static uint8_t guest[1 << 20];                      // "RAM" at physical 0x80000000
#define GUEST_PA 0x80000000ull
void *xh_ram(uint64_t pa, uint64_t len) { return pa >= GUEST_PA && pa - GUEST_PA <= sizeof guest && len <= sizeof guest - (pa - GUEST_PA) ? guest + (pa - GUEST_PA) : NULL; }
static uint32_t stop_reason = 99;
void xh_stop(uint32_t reason, uint32_t code, const char *why) { (void)code; (void)why; stop_reason = reason; }
bool xh_stopping(void) { return stop_reason != 99; }
static int ready_count;
void xh_guest_ready(void) { ready_count++; }
struct xh_info g_info;                       // main.c's: the device asks it where the boot framebuffer is
const struct xh_vector *xh_vector(void) { return NULL; }
static xh_display fake_display;
static unsigned flush_rect[4];
xh_display *xh_display_get(void) { return fake_display.pixels ? &fake_display : NULL; }
void xh_display_flush(unsigned x, unsigned y, unsigned w, unsigned h) { flush_rect[0] = x; flush_rect[1] = y; flush_rect[2] = w; flush_rect[3] = h; }
unsigned xh_task_filter(struct xs_filter_insn *f, uint64_t text_lo, uint64_t text_hi, int ram_fd, int trap_fd);

// ---- instructions -------------------------------------------------------------------------------
static void test_insn(void)
{
  xh_mem_insn m;
  CHECK(xh_decode_mem(0xb9400001, &m) && m.load && m.size == 4 && m.rt == 1 && m.rn == 0 && !m.sign_extend && !m.writeback);   // ldr w1, [x0]
  CHECK(xh_decode_mem(0xf9000462, &m) && !m.load && m.size == 8 && m.rt == 2 && m.rn == 3);                                    // str x2, [x3, #8]
  CHECK(xh_decode_mem(0x386668a4, &m) && m.load && m.size == 1 && m.rt == 4);                                                  // ldrb w4, [x5, x6]
  CHECK(xh_decode_mem(0xb8804507, &m) && m.load && m.size == 4 && m.sign_extend && m.to64 && m.writeback && m.offset == 4 && m.rn == 8);   // ldrsw x7, [x8], #4
  CHECK(xh_decode_mem(0x381ffc20, &m) && !m.load && m.size == 1 && m.writeback && m.offset == -1 && m.rn == 1);                // strb w0, [x1, #-1]!
  CHECK(xh_decode_mem(0xa9000801, &m) && !m.load && m.pair && m.size == 8 && m.rt == 1 && m.rt2 == 2);                         // stp x1, x2, [x0]
  CHECK(xh_decode_mem(0x29c11003, &m) && m.load && m.pair && m.size == 4 && m.rt2 == 4 && m.writeback && m.offset == 8);       // ldp w3, w4, [x0, #8]!
  CHECK(xh_decode_mem(0x79400020, &m) && m.load && m.size == 2);                                                               // ldrh w0, [x1]
  CHECK(!xh_decode_mem(0x3dc00000, &m));                                                                                       // ldr q0, [x0]
  CHECK(!xh_decode_mem(0xf9800000, &m));                                                                                       // prfm
  CHECK(!xh_decode_mem(0x8b010000, &m));                                                                                       // add
  xh_sys_insn s;
  CHECK(xh_decode_sys(0xd538d080, &s) && s.read && xh_sys_key(&s) == XH_SYSREG(3, 0, 13, 0, 4) && s.rt == 0);                  // mrs x0, tpidr_el1
  CHECK(xh_decode_sys(0xd51b4223, &s) && !s.read && xh_sys_key(&s) == XH_SYSREG(3, 3, 4, 2, 1) && s.rt == 3);                  // msr daif, x3
  CHECK(!xh_decode_sys(0xd503201f, &s));                                                                                       // nop
  // two TPIDRRO reads, a TPIDR read (iOS's CPU number), a TPIDRRO write (left alone), a TPIDR write
  uint32_t text[5] = { 0xd53bd060, 0xd53bd07e, 0xd53bd043, 0xd51bd060, 0xd51bd049 };
  CHECK(xh_text_fixup(text, sizeof text) == 4 && text[0] == 0xd53bd040 && text[1] == 0xd53bd05e && text[2] == 0xd2800003 &&
        text[3] == 0xd51bd060 && text[4] == 0xd503201f);
}

// ---- trapped device access ----------------------------------------------------------------------
static uint64_t reg_store[4], last_write_size;
static uint64_t fake_read(void *o, uint64_t off, unsigned size) { (void)o; (void)size; return reg_store[off / 8 & 3] >> (off % 8 * 8); }
static void fake_write(void *o, uint64_t off, unsigned size, uint64_t v) { (void)o; last_write_size = size; reg_store[off / 8 & 3] = v; }

static void test_mmio(void)
{
  CHECK(xh_mmio_add(&(xh_mmio_region){ .base = 0x9000000, .size = 0x1000, .name = "fake", .read = fake_read, .write = fake_write }) == 0);
  CHECK(xh_mmio_add(&(xh_mmio_region){ .base = 0x9000800, .size = 0x1000, .name = "overlap" }) == -1);
  CHECK(xh_mmio_covers(0x9000fff) && !xh_mmio_covers(0x9001000));
  xh_regs r = { .pc = 0x1000 };
  reg_store[0] = 0xfffffffff00dfeedull;
  CHECK(xh_mmio_emulate(&r, 0xb9400001, 0x9000000) && r.x[1] == 0xf00dfeed && r.pc == 0x1004);        // ldr w1: zero-extended
  r.x[8] = 0x5000;
  CHECK(xh_mmio_emulate(&r, 0xb8804507, 0x9000000) && r.x[7] == 0xfffffffff00dfeedull && r.x[8] == 0x5004);   // ldrsw, post-index
  r.x[2] = 0x1122334455667788ull;
  CHECK(xh_mmio_emulate(&r, 0xf9000462, 0x9000008) && reg_store[1] == 0x1122334455667788ull && last_write_size == 8);
  r.x[0] = 0xabcd;
  CHECK(xh_mmio_emulate(&r, 0x39000020, 0x9000010) && reg_store[2] == 0xcd && last_write_size == 1);          // strb w0, [x1]
  CHECK(xh_mmio_emulate(&r, 0xb900003f, 0x9000010) && reg_store[2] == 0);                                     // str wzr
  CHECK(!xh_mmio_emulate(&r, 0x3dc00000, 0x9000000));
  CHECK(xh_mmio_read(0x12345678, 4) == 0);                                                                     // nothing there
}

// ---- interrupt controller -----------------------------------------------------------------------
static bool line[XH_MAX_CPUS];
static void note_line(unsigned cpu, bool up) { line[cpu] = up; }

static void test_gic(void)
{
  xh_cpu_init(2);
  xh_gic_init(2, 0x8000000, 0x80a0000);
  xh_gic_notify = note_line;
  xh_gic_map();
  CHECK((xh_mmio_read(0x8000000 + 0xffe8, 4) >> 4 & 15) == 3);                 // GICD_PIDR2, as ArmCpu checks
  CHECK(xh_mmio_read(0x80a0000 + 0x20000 + 0x8, 8) >> 32 == 1);                // CPU 1's GICR_TYPER affinity
  CHECK(xh_mmio_read(0x80a0000 + 0x20000 + 0x8, 4) & 0x10);                    // and it is the last frame
  // the register path, as ArmCpu programs it: distributor on, SPI 40 enabled and routed to CPU 1
  xh_mmio_write(0x8000000, 4, 0x12);
  xh_mmio_write(0x8000000 + 0x100 + 4, 4, 1u << 8);
  xh_mmio_write(0x8000000 + 0x6000 + 8 * 40, 8, 1);
  xh_sys_insn igrpen = { .read = false, .op0 = 3, .op1 = 0, .crn = 12, .crm = 12, .op2 = 7 }, iar = { .read = true, .op0 = 3, .crn = 12, .crm = 12, .op2 = 0 };
  xh_sys_insn eoir = { .read = false, .op0 = 3, .crn = 12, .crm = 12, .op2 = 1 }, sgi1r = { .read = false, .op0 = 3, .crn = 12, .crm = 11, .op2 = 5 };
  uint64_t v = 1;
  CHECK(xh_gic_sysreg(1, &igrpen, &v));
  xh_gic_set_spi(40, true);
  CHECK(line[1] && !line[0] && xh_gic_pending(1));
  CHECK(xh_gic_sysreg(1, &iar, &v) && v == 40);
  CHECK(!line[1]);                                                             // active: not offered again
  CHECK(xh_gic_ack(1) == XH_INTID_NONE);
  v = 40;
  CHECK(xh_gic_sysreg(1, &eoir, &v) && line[1]);                               // level still high: pending again
  CHECK(xh_gic_ack(1) == 40);
  xh_gic_set_spi(40, false);
  xh_gic_eoi(1, 40);
  CHECK(!line[1] && xh_gic_ack(1) == XH_INTID_NONE);
  // SGIs: by register from CPU 1 to CPU 0, and by the host-call path
  xh_gic_enable(0, 3, true);
  v = 3ull << 24 | 1;
  CHECK(xh_gic_sysreg(1, &sgi1r, &v) && line[0] && xh_gic_ack(0) == 3);
  xh_gic_eoi(0, 3);
  CHECK(!line[0]);
  // ArmCpu's software IPI of core 1 (armcpu-spi-ipi=1): SPI 1009, routed, enabled, pended by a GICD_ISPENDR write
  xh_mmio_write(0x8000000 + 0x6000 + 8 * 1009, 8, 1);
  xh_mmio_write(0x8000000 + 0x100 + (1009 / 32) * 4, 4, 1u << (1009 % 32));
  CHECK(xh_mmio_read(0x8000000 + 0x80 + (1009 / 32) * 4, 4) & 1u << (1009 % 32));       // group 1 reads back
  xh_mmio_write(0x8000000 + 0x200 + (1009 / 32) * 4, 4, 1u << (1009 % 32));
  CHECK(xh_gic_pending(1) && !xh_gic_pending(0) && xh_gic_ack(1) == 1009);
  xh_gic_eoi(1, 1009);
  CHECK(xh_gic_ack(1) == XH_INTID_NONE);
  // the timer's line is a level read from the CPU layer
  xh_gic_enable(0, XH_INTID_VTIMER, true);
  xh_cpu_timer_set(&g_vcpu[0], 1);                                             // long past
  CHECK(xh_gic_pending(0) && xh_gic_ack(0) == XH_INTID_VTIMER);
  xh_cpu_timer_set(&g_vcpu[0], UINT64_MAX);
  xh_gic_eoi(0, XH_INTID_VTIMER);
  CHECK(!xh_gic_pending(0));
  // priority mask 0 holds everything back
  xh_sys_insn pmr = { .read = false, .op0 = 3, .crn = 4, .crm = 6, .op2 = 0 };
  v = 0;
  xh_gic_sysreg(0, &pmr, &v);
  xh_gic_send_sgi(0, 3);
  CHECK(!xh_gic_pending(0));
  v = 0xff;
  xh_gic_sysreg(0, &pmr, &v);
  CHECK(xh_gic_ack(0) == 3);
  xh_gic_eoi(0, 3);
}

// ---- system registers and PSCI ------------------------------------------------------------------
static void test_sysreg(void)
{
  xh_vcpu *v = &g_vcpu[0];
  xh_regs r = { .pc = 0x100 };
  r.x[5] = 0xdeadbeefcafe;
  CHECK(xh_sysreg_emulate(v, &r, 0xd518d085) && v->cpu.tpidr_el1 == 0xdeadbeefcafe && r.pc == 0x104);      // msr tpidr_el1, x5
  CHECK(xh_sysreg_emulate(v, &r, 0xd538d089) && r.x[9] == 0xdeadbeefcafe);                                 // mrs x9, tpidr_el1
  v->cpu.irq_masked = 0;
  CHECK(xh_sysreg_emulate(v, &r, 0xd50342df) && v->cpu.irq_masked == 1);                                   // msr daifset, #2
  CHECK(xh_sysreg_emulate(v, &r, 0xd53b4221) && (r.x[1] & 0x80));                                          // mrs x1, daif
  CHECK(xh_sysreg_emulate(v, &r, 0xd50342ff) && v->cpu.irq_masked == 0);                                   // msr daifclr, #2
  CHECK(xh_sysreg_emulate(v, &r, 0xd5384242) && r.x[2] == 4);                                              // mrs x2, CurrentEL
  r.x[3] = 0x30d00800;
  CHECK(xh_sysreg_emulate(v, &r, 0xd5181003) && xh_sysreg_emulate(v, &r, 0xd5381004) && r.x[4] == 0x30d00800);   // sctlr_el1 shadow
  CHECK(xh_sysreg_emulate(v, &r, 0xd508871f));                                                             // tlbi vmalle1
  CHECK(!xh_sysreg_emulate(v, &r, 0xd69f03e0));                                                            // eret: not emulated
  CHECK(!xh_sysreg_emulate(v, &r, 0x00000000));                                                            // udf
  // the timer registers arm the CPU's timer
  r.x[0] = 12345; r.x[1] = 1;
  CHECK(xh_sysreg_emulate(v, &r, 0xd51be340) && xh_sysreg_emulate(v, &r, 0xd51be321));                     // cntv_cval_el0, cntv_ctl_el0
  CHECK(v->timer_armed && v->timer_deadline == 12345);
  r.x[1] = 3;
  CHECK(xh_sysreg_emulate(v, &r, 0xd51be321) && !v->timer_armed);                                          // masked
  // PSCI
  r.x[0] = XH_PSCI_VERSION;
  CHECK(xh_sysreg_emulate(v, &r, 0xd4000002) && r.x[0] == 0x10000);
  r.x[0] = XH_PSCI_CPU_ON; r.x[1] = 1; r.x[2] = GUEST_PA + 0x1000; r.x[3] = 7;
  CHECK(xh_sysreg_emulate(v, &r, 0xd4000003) && r.x[0] == 0 && g_vcpu[1].started);
  r.x[0] = XH_PSCI_CPU_ON; r.x[1] = 1; r.x[2] = GUEST_PA + 0x1000;
  CHECK(xh_sysreg_emulate(v, &r, 0xd4000002) && (int64_t)r.x[0] == -4);                                    // ALREADY_ON
  r.x[0] = XH_PSCI_CPU_ON; r.x[1] = 1; r.x[2] = 0x10;
  CHECK(xh_sysreg_emulate(v, &r, 0xd4000002) && (int64_t)r.x[0] == -9);                                    // INVALID_ADDRESS
  r.x[0] = XH_PSCI_CPU_ON; r.x[1] = 5; r.x[2] = GUEST_PA;
  CHECK(xh_sysreg_emulate(v, &r, 0xd4000002) && (int64_t)r.x[0] == -2);
  r.x[0] = 0x84000099;
  CHECK(xh_sysreg_emulate(v, &r, 0xd4000002) && (int64_t)r.x[0] == -1);
}

// ---- a guest driver for the virtio devices ------------------------------------------------------
typedef struct { xh_virtio *dev; uint64_t queue_pa[2]; uint16_t avail[2], used_seen[2]; } driver;
struct vdesc { uint64_t addr; uint32_t len; uint16_t flags, next; };
#define R(d, off) xh_virtio_mmio_read((d)->dev, off, 4)
#define W(d, off, v) xh_virtio_mmio_write((d)->dev, off, 4, v)

static bool driver_init(driver *d, uint64_t features, unsigned queues, uint64_t memory)
{
  if (R(d, 0) != 0x74726976 || R(d, 4) != 2) return false;
  W(d, 0x70, 0); W(d, 0x70, 1); W(d, 0x70, 3);
  W(d, 0x24, 0); W(d, 0x20, (uint32_t)features); W(d, 0x24, 1); W(d, 0x20, 1 | (uint32_t)(features >> 32));
  W(d, 0x70, 11);
  if (!(R(d, 0x70) & 8)) return false;
  for (unsigned q = 0; q < queues; q++) {
    d->queue_pa[q] = memory + q * 0x4000;
    d->avail[q] = d->used_seen[q] = 0;
    memset(xh_ram(d->queue_pa[q], 0x4000), 0, 0x4000);
    W(d, 0x30, q);
    if (R(d, 0x34) < 8) return false;
    W(d, 0x38, 8);
    W(d, 0x80, (uint32_t)d->queue_pa[q]); W(d, 0x84, 0);
    W(d, 0x90, (uint32_t)d->queue_pa[q] + 0x1000); W(d, 0x94, 0);
    W(d, 0xa0, (uint32_t)d->queue_pa[q] + 0x2000); W(d, 0xa4, 0);
    W(d, 0x44, 1);
  }
  W(d, 0x70, 15);
  return (R(d, 0x70) & 15) == 15;
}

// Posts a chain of parts (address, length, device-writable) and kicks. Returns the used length, or -1.
static int64_t driver_send(driver *d, unsigned q, const uint64_t parts[][3], unsigned n, bool kick)
{
  struct vdesc *desc = xh_ram(d->queue_pa[q], 0x1000);
  uint16_t *avail = xh_ram(d->queue_pa[q] + 0x1000, 0x1000), *used = xh_ram(d->queue_pa[q] + 0x2000, 0x1000);
  // Single buffers take one descriptor each; longer chains alternate between the queue's halves.
  unsigned first = n == 1 ? d->avail[q] % 8u : (d->avail[q] % 2u) * 4;
  for (unsigned i = 0; i < n; i++)
    desc[first + i] = (struct vdesc){ parts[i][0], (uint32_t)parts[i][1], (uint16_t)((parts[i][2] ? 2 : 0) | (i + 1 < n ? 1 : 0)), (uint16_t)(first + i + 1) };
  avail[2 + d->avail[q] % 8] = (uint16_t)first;
  avail[1] = ++d->avail[q];
  if (!kick) return -1;
  W(d, 0x50, q);
  if (used[1] == d->used_seen[q]) return -1;
  uint32_t length = ((uint32_t *)(used + 2))[2 * (d->used_seen[q] % 8) + 1];
  d->used_seen[q]++;
  return length;
}

static void test_virtio_blk(const char *dir)
{
  char path[256];
  snprintf(path, sizeof path, "%s/ro.img", dir);
  FILE *f = fopen(path, "wb");
  for (int s = 0; s < 64; s++) { uint8_t sector[512]; memset(sector, s + 1, 512); fwrite(sector, 1, 512, f); }
  fclose(f);
  const char *error = NULL;
  xh_disk_config ro = { 0 };
  strcpy(ro.path, path);
  xh_blk *disk = xh_blk_open(&ro, &error);
  CHECK(disk && xh_blk_bytes(disk) == 64 * 512 && !xh_blk_writable(disk));
  driver d = { .dev = xh_virtio_blk_create(0, 40, disk) };
  CHECK(d.dev != NULL);
  CHECK(R(&d, 8) == 2);
  W(&d, 0x14, 0);
  CHECK(R(&d, 0x10) & (1u << 5));                                              // F_RO, which NativeVirtioBlock requires
  W(&d, 0x14, 1);
  CHECK(R(&d, 0x10) & 1);                                                      // VERSION_1
  CHECK(driver_init(&d, 1u << 5, 1, GUEST_PA + 0x10000));
  CHECK(R(&d, 0x100) == 64 && R(&d, 0x104) == 0);
  struct { uint32_t type, reserved; uint64_t sector; } *req = xh_ram(GUEST_PA + 0x20000, 16);
  uint8_t *data = xh_ram(GUEST_PA + 0x21000, 1024), *status = xh_ram(GUEST_PA + 0x22000, 1);
  *req = (__typeof__(*req)){ 0, 0, 5 };
  *status = 0xff;
  CHECK(driver_send(&d, 0, (const uint64_t[][3]){ { GUEST_PA + 0x20000, 16, 0 }, { GUEST_PA + 0x21000, 1024, 1 }, { GUEST_PA + 0x22000, 1, 1 } }, 3, true) == 1025);
  CHECK(*status == 0 && data[0] == 6 && data[511] == 6 && data[512] == 7);
  CHECK(R(&d, 0x60) == 1);
  W(&d, 0x64, 1);
  CHECK(R(&d, 0x60) == 0);
  *req = (__typeof__(*req)){ 1, 0, 5 };                                        // a write to a read-only disk
  CHECK(driver_send(&d, 0, (const uint64_t[][3]){ { GUEST_PA + 0x20000, 16, 0 }, { GUEST_PA + 0x21000, 512, 0 }, { GUEST_PA + 0x22000, 1, 1 } }, 3, true) == 1 && *status == 1);
  *req = (__typeof__(*req)){ 0, 0, 64 };                                       // past the end
  CHECK(driver_send(&d, 0, (const uint64_t[][3]){ { GUEST_PA + 0x20000, 16, 0 }, { GUEST_PA + 0x21000, 512, 1 }, { GUEST_PA + 0x22000, 1, 1 } }, 3, true) == 1 && *status == 1);
  *req = (__typeof__(*req)){ 0, 0, 0 };                                        // a part outside guest memory: the chain is returned unused
  CHECK(driver_send(&d, 0, (const uint64_t[][3]){ { GUEST_PA + 0x20000, 16, 0 }, { 0x10, 512, 1 }, { GUEST_PA + 0x22000, 1, 1 } }, 3, true) == 0);
  // a driver that does not accept VERSION_1 is refused
  W(&d, 0x70, 0); W(&d, 0x70, 3); W(&d, 0x24, 1); W(&d, 0x20, 0); W(&d, 0x70, 11);
  CHECK(!(R(&d, 0x70) & 8));

  // the writable Data store: created sparse on the first open, seeded from an image, not from a tar
  xh_disk_config rw = { .writable = true, .data_store = true };
  snprintf(rw.path, sizeof rw.path, "%s/data.img", dir);
  strcpy(rw.seed, path);
  xh_blk *store = xh_blk_open(&rw, &error);
  CHECK(store && xh_blk_writable(store) && xh_blk_bytes(store) >= 64 * 512);
  uint8_t sector[512];
  CHECK(store && xh_blk_read(store, 3 * 512, sector, 512) == 0 && sector[0] == 4);       // the seed's content
  memset(sector, 0xee, 512);
  CHECK(store && xh_blk_write(store, 1 << 20, sector, 512) == 0 && xh_blk_flush(store) == 0);
  struct stat st;
  CHECK(stat(rw.path, &st) == 0 && (uint64_t)st.st_blocks * 512 < (16u << 20));           // sparse
  xh_blk_close(store);
  store = xh_blk_open(&rw, &error);                                                       // a second start keeps the data
  CHECK(store && xh_blk_read(store, 1 << 20, sector, 512) == 0 && sector[0] == 0xee);
  driver w = { .dev = xh_virtio_blk_create(0, 41, store) };
  W(&w, 0x14, 0);
  CHECK(!(R(&w, 0x10) & (1u << 5)) && driver_init(&w, 0, 1, GUEST_PA + 0x10000));
  *req = (__typeof__(*req)){ 1, 0, 9 };
  memset(data, 0x77, 512);
  CHECK(driver_send(&w, 0, (const uint64_t[][3]){ { GUEST_PA + 0x20000, 16, 0 }, { GUEST_PA + 0x21000, 512, 0 }, { GUEST_PA + 0x22000, 1, 1 } }, 3, true) == 1 && *status == 0);
  *req = (__typeof__(*req)){ 4, 0, 0 };                                        // flush
  CHECK(driver_send(&w, 0, (const uint64_t[][3]){ { GUEST_PA + 0x20000, 16, 0 }, { GUEST_PA + 0x22000, 1, 1 } }, 2, true) == 1 && *status == 0);
  CHECK(xh_blk_read(store, 9 * 512, sector, 512) == 0 && sector[5] == 0x77);
  xh_blk_close(store);
  snprintf(path, sizeof path, "%s/seed.tar", dir);
  f = fopen(path, "wb");
  uint8_t tar[8192] = { 0 };
  memcpy(tar + 257, "ustar", 5);
  memset(tar + 4096, 0x42, 512);
  fwrite(tar, 1, sizeof tar, f);
  fclose(f);
  xh_disk_config fresh = { .writable = true };
  snprintf(fresh.path, sizeof fresh.path, "%s/fresh.img", dir);
  strcpy(fresh.seed, path);
  store = xh_blk_open(&fresh, &error);
  CHECK(store && xh_blk_read(store, 4096, sector, 512) == 0 && sector[0] == 0);           // a tar seed is not a disk image
  xh_blk_close(store);
}

static void test_virtio_gpu(void)
{
  fake_display = (xh_display){ .width = 8, .height = 4, .stride = 32, .pixels = calloc(4, 32) };
  driver d = { .dev = xh_virtio_gpu_create(0, 42, 8, 4) };
  CHECK(d.dev && R(&d, 8) == 16 && R(&d, 0x108) == 1 && driver_init(&d, 0, 2, GUEST_PA + 0x10000));
  uint32_t *cmd = xh_ram(GUEST_PA + 0x30000, 4096), *resp = xh_ram(GUEST_PA + 0x31000, 4096);
  uint32_t *pixels = xh_ram(GUEST_PA + 0x40000, 8 * 4 * 4);
#define SEND(words, resp_bytes) driver_send(&d, 0, (const uint64_t[][3]){ { GUEST_PA + 0x30000, (words) * 4, 0 }, { GUEST_PA + 0x31000, resp_bytes, 1 } }, 2, true)
  memset(cmd, 0, 4096);
  cmd[0] = 0x100;                                                              // GET_DISPLAY_INFO
  CHECK(SEND(6, 24 + 16 * 24) == 24 + 16 * 24 && resp[0] == 0x1101 && resp[8] == 8 && resp[9] == 4 && resp[10] == 1);
  cmd[0] = 0x101; cmd[6] = 1; cmd[7] = 67; cmd[8] = 8; cmd[9] = 4;             // RESOURCE_CREATE_2D id 1, R8G8B8A8, 8x4
  CHECK(SEND(10, 24) == 24 && resp[0] == 0x1100);
  CHECK(SEND(10, 24) == 24 && resp[0] == 0x1203);                              // the id is taken
  cmd[0] = 0x106; cmd[6] = 1; cmd[7] = 1; cmd[8] = (uint32_t)(GUEST_PA + 0x40000); cmd[9] = 0; cmd[10] = 8 * 4 * 4; cmd[11] = 0;   // ATTACH_BACKING
  CHECK(SEND(12, 24) == 24 && resp[0] == 0x1100);
  for (int i = 0; i < 32; i++) pixels[i] = 0xff000000u | (uint32_t)i << 16 | 0x80u << 8 | 0x11;      // bytes R=0x11, G=0x80, B=i, A=0xff
  cmd[0] = 0x105; cmd[6] = 0; cmd[7] = 0; cmd[8] = 8; cmd[9] = 4; cmd[10] = 0; cmd[11] = 0; cmd[12] = 1; cmd[13] = 0;              // TRANSFER_TO_HOST_2D
  CHECK(SEND(14, 24) == 24 && resp[0] == 0x1100);
  cmd[0] = 0x103; cmd[10] = 0; cmd[11] = 1;                                    // SET_SCANOUT rect, scanout 0, resource 1
  CHECK(SEND(12, 24) == 24 && resp[0] == 0x1100);
  cmd[11] = 9;
  CHECK(SEND(12, 24) == 24 && resp[0] == 0x1203);                              // no such resource
  cmd[10] = 3; cmd[11] = 1;
  CHECK(SEND(12, 24) == 24 && resp[0] == 0x1202);                              // no such scanout
  cmd[0] = 0x104; cmd[6] = 2; cmd[7] = 1; cmd[8] = 4; cmd[9] = 2; cmd[10] = 1; cmd[11] = 0;          // RESOURCE_FLUSH (2,1) 4x2
  cmd[1] = 1; cmd[2] = 0x55; cmd[3] = 0;                                       // with a fence
  CHECK(SEND(12, 24) == 24 && resp[0] == 0x1100 && resp[1] == 1 && resp[2] == 0x55);
  cmd[1] = 0;
  CHECK(flush_rect[0] == 2 && flush_rect[1] == 1 && flush_rect[2] == 4 && flush_rect[3] == 2 && ready_count == 1);
  const uint8_t *px = fake_display.pixels + 1 * 32 + 2 * 4;                    // pixel (2,1) = index 10: B,G,R,X
  CHECK(px[0] == 10 && px[1] == 0x80 && px[2] == 0x11);
  CHECK(fake_display.pixels[0] == 0 && fake_display.pixels[1 * 32 + 6 * 4] == 0);       // outside the flushed rectangle
  cmd[8] = 9;
  CHECK(SEND(12, 24) == 24 && resp[0] == 0x1205);                              // rectangle outside the resource
  cmd[0] = 0x999;
  CHECK(SEND(6, 24) == 24 && resp[0] == 0x1200);
  cmd[0] = 0x102; cmd[6] = 1;                                                  // RESOURCE_UNREF
  CHECK(SEND(8, 24) == 24 && resp[0] == 0x1100);
  {
    // A half-float 2D resource ('RGhA', 8 bytes a pixel in the backing) onto a half-float display: the rows as they are.
    uint8_t *eight = fake_display.pixels;
    fake_display = (xh_display){ .width = 8, .height = 4, .stride = 64, .format = XH_PIXELS_RGBA16F, .bpp = 8, .pixels = calloc(4, 64) };
    uint16_t *halves = xh_ram(GUEST_PA + 0x40000, 8 * 4 * 8);
    for (int i = 0; i < 8 * 4 * 4; i++) halves[i] = (uint16_t)(0x3400 + i);
    memset(cmd, 0, 4096);
    cmd[0] = 0x101; cmd[6] = 5; cmd[7] = XH_VIRTIO_GPU_FORMAT_RGBA16F; cmd[8] = 8; cmd[9] = 4;
    CHECK(SEND(10, 24) == 24 && resp[0] == 0x1100);
    cmd[0] = 0x106; cmd[6] = 5; cmd[7] = 1; cmd[8] = (uint32_t)(GUEST_PA + 0x40000); cmd[9] = (uint32_t)((GUEST_PA + 0x40000) >> 32); cmd[10] = 8 * 4 * 8; cmd[11] = 0;
    CHECK(SEND(12, 24) == 24 && resp[0] == 0x1100);
    memset(cmd, 0, 4096);
    cmd[0] = 0x105; cmd[8] = 8; cmd[9] = 4; cmd[12] = 5;                       // TRANSFER_TO_HOST_2D, all of it
    CHECK(SEND(14, 24) == 24 && resp[0] == 0x1100);
    memset(cmd, 0, 4096);
    cmd[0] = 0x103; cmd[8] = 8; cmd[9] = 4; cmd[11] = 5;                       // SET_SCANOUT
    CHECK(SEND(12, 24) == 24 && resp[0] == 0x1100);
    memset(cmd, 0, 4096);
    cmd[0] = 0x104; cmd[6] = 1; cmd[7] = 2; cmd[8] = 2; cmd[9] = 1; cmd[10] = 5;   // RESOURCE_FLUSH (1,2) 2x1
    CHECK(SEND(12, 24) == 24 && resp[0] == 0x1100);
    CHECK(!memcmp(fake_display.pixels + 2 * 64 + 1 * 8, halves + (2 * 8 + 1) * 4, 16) && fake_display.pixels[0] == 0);
    cmd[0] = 0x102; cmd[6] = 5;
    CHECK(SEND(8, 24) == 24 && resp[0] == 0x1100);
    free(fake_display.pixels);
    fake_display = (xh_display){ .width = 8, .height = 4, .stride = 32, .pixels = eight };
    memset(cmd, 0, 4096);
  }
  {
    // The boot framebuffer scanned out by the guest's display driver (the console) is shown but is not the guest's
    // first frame; the first surface of its own is.
    xh_virtio *boot_dev = xh_virtio_gpu_create(0x1000, 43, 8, 4);
    driver b = { .dev = boot_dev };
    uint64_t video = g_info.video[0];
    int before = ready_count;
    CHECK(boot_dev && driver_init(&b, 0, 2, GUEST_PA + 0x50000));
#define SENDB(words) driver_send(&b, 0, (const uint64_t[][3]){ { GUEST_PA + 0x30000, (words) * 4, 0 }, { GUEST_PA + 0x31000, 24, 1 } }, 2, true)
    for (uint32_t id = 1; id <= 2; id++) {
      uint64_t backing = GUEST_PA + 0x40000 + (id - 1) * 0x1000;
      if (id == 1) g_info.video[0] = backing;                                  // resource 1 is the boot framebuffer
      memset(cmd, 0, 4096);
      cmd[0] = 0x101; cmd[6] = id; cmd[7] = 2; cmd[8] = 8; cmd[9] = 4;         // RESOURCE_CREATE_2D, B8G8R8X8
      CHECK(SENDB(10) == 24 && resp[0] == 0x1100);
      cmd[0] = 0x106; cmd[6] = id; cmd[7] = 1; cmd[8] = (uint32_t)backing; cmd[9] = (uint32_t)(backing >> 32); cmd[10] = 8 * 4 * 4; cmd[11] = 0;
      CHECK(SENDB(12) == 24 && resp[0] == 0x1100);
      memset(cmd, 0, 4096);
      cmd[0] = 0x103; cmd[8] = 8; cmd[9] = 4; cmd[10] = 0; cmd[11] = id;       // SET_SCANOUT
      CHECK(SENDB(12) == 24 && resp[0] == 0x1100);
      cmd[0] = 0x104; cmd[10] = id; cmd[11] = 0;                               // RESOURCE_FLUSH
      flush_rect[2] = 0;
      CHECK(SENDB(12) == 24 && resp[0] == 0x1100 && flush_rect[2] == 8);
      CHECK(ready_count == before + (id == 2));
    }
#undef SENDB
    g_info.video[0] = video;
    memset(cmd, 0, 4096);
  }
  W(&d, 0x70, 0);                                                              // reset frees what is left
  free(fake_display.pixels);
  fake_display.pixels = NULL;
#undef SEND
}

// ---- virtio-gpu 3D: the device against a recording renderer, driven by generated command streams --------------------
#include "gpu3d.h"
#include <errno.h>
#include <sys/mman.h>
#include <sys/syscall.h>
static uint32_t rnd_seed;
static uint32_t rnd(void) { rnd_seed = rnd_seed * 1664525u + 1013904223u; return rnd_seed >> 8; }
static struct {
  unsigned contexts, blobs, submits, unrefs, attaches, fences;
  uint32_t live[8];                                  // context ids
  uint8_t last[256]; size_t last_size;               // the last SUBMIT_3D's bytes
  xh_gpu_entry entries[4]; unsigned nentries; uint64_t blob_id, blob_size; uint32_t blob_mem, blob_flags, blob_ctx;
  uint32_t host_blob[4]; unsigned nhost;             // HOST3D blobs: pool slot i
} fake;
static unsigned fake_capset_count(xh_gpu3d *b) { (void)b; return 1; }
static bool fake_capset_info(xh_gpu3d *b, unsigned i, uint32_t *id, uint32_t *version, uint32_t *size) { (void)b; if (i) return false; *id = 6; *version = 0; *size = 64; return true; }
static size_t fake_capset(xh_gpu3d *b, uint32_t id, uint32_t version, void *out, size_t max)
{
  (void)b; (void)version;
  if (id != 6 || max < 64) return 0;
  for (unsigned i = 0; i < 64; i++) ((uint8_t *)out)[i] = (uint8_t)(0xc0 + i);
  return 64;
}
static int fake_ctx_create(xh_gpu3d *b, uint32_t ctx, uint32_t capset, const char *name, size_t n)
{
  (void)b;
  if (capset != 6 || ctx == 99) return -ENOMEM;
  if ((n != 4 || memcmp(name, "test", 4)) && (n != 14 || memcmp(name, "xnu:backboardd", 14))) return -EINVAL;
  for (unsigned i = 0; i < 8; i++) if (!fake.live[i]) { fake.live[i] = ctx; fake.contexts++; return 0; }
  return -ENOMEM;
}
static void fake_ctx_destroy(xh_gpu3d *b, uint32_t ctx) { (void)b; for (unsigned i = 0; i < 8; i++) if (fake.live[i] == ctx) { fake.live[i] = 0; fake.contexts--; } }
static int fake_submit(xh_gpu3d *b, uint32_t ctx, const void *commands, size_t size)
{
  (void)b; (void)ctx;
  if (size >= 4 && !memcmp(commands, "\xad\xde\0\0", 4)) return -EINVAL;
  fake.submits++;
  fake.last_size = size;
  memcpy(fake.last, commands, size < sizeof fake.last ? size : sizeof fake.last);
  return 0;
}
// Ring 0 is retired inside the call (the host's own timeline); ring 9 is refused; the others wait for the test.
static int fake_submit_fence(xh_gpu3d *b, uint32_t ctx, uint32_t ring, uint64_t fence)
{
  fake.fences++;
  if (ring == 9) return -EINVAL;
  if (ring == 0) b->retire(b->dev, ctx, ring, fence);
  return 0;
}
static volatile int fake_blob_held;                  // the lanes' test: blob 0x5105 is made only once this is 0
static int fake_create_blob(xh_gpu3d *b, uint32_t ctx, uint32_t res, uint32_t mem, uint32_t flags, uint64_t blob_id, uint64_t size, const xh_gpu_entry *entries, unsigned n)
{
  (void)b;
  if (blob_id == 0xbad) return -EINVAL;
  while (blob_id == 0x5105 && fake_blob_held) usleep(1000);
  fake.blobs++;
  fake.blob_ctx = ctx; fake.blob_mem = mem; fake.blob_flags = flags; fake.blob_id = blob_id; fake.blob_size = size;
  fake.nentries = n;
  if (n) memcpy(fake.entries, entries, (n < 4 ? n : 4) * sizeof *entries);
  if (mem == XH_GPU_BLOB_HOST3D && fake.nhost < 4) fake.host_blob[fake.nhost++] = res;
  return 0;
}
static int fake_map_blob(xh_gpu3d *b, uint32_t res, uint64_t *offset, uint32_t *info)
{
  (void)b;
  for (unsigned i = 0; i < fake.nhost; i++) if (fake.host_blob[i] == res) { *offset = 0x200000 + i * 0x4000; *info = XH_GPU_MAP_CACHE_CACHED; return 0; }
  return -EINVAL;
}
static int fake_attach(xh_gpu3d *b, uint32_t ctx, uint32_t res, bool on) { (void)b; (void)ctx; (void)res; fake.attaches += on ? 1 : 100; return 0; }
static void fake_unref(xh_gpu3d *b, uint32_t res) { (void)b; (void)res; fake.unrefs++; fake.blobs--; }

// Posts one command and lets the device's thread finish. Returns how many commands the device has returned since the last call.
static unsigned gpu3d_send(driver *d, xh_virtio *dev, unsigned words, unsigned extra_bytes, unsigned resp_bytes)
{
  uint16_t *used = xh_ram(d->queue_pa[0] + 0x2000, 0x1000), seen = d->used_seen[0];
  if (extra_bytes) driver_send(d, 0, (const uint64_t[][3]){ { GUEST_PA + 0x30000, words * 4, 0 }, { GUEST_PA + 0x32000, extra_bytes, 0 }, { GUEST_PA + 0x31000, resp_bytes, 1 } }, 3, true);
  else driver_send(d, 0, (const uint64_t[][3]){ { GUEST_PA + 0x30000, words * 4, 0 }, { GUEST_PA + 0x31000, resp_bytes, 1 } }, 2, true);
  xh_virtio_gpu_idle(dev);
  unsigned n = (uint16_t)(used[1] - seen);           // driver_send takes an answer that came before it looked
  d->used_seen[0] = used[1];
  return n;
}

// The chunks of a blob over a memfd whose page N begins with the byte N: `want` chunks of pages[i] pages, the first
// page of each being first[i] and the following ones `step` pages apart. Closes and frees them.
static bool chunks_are(int count, xh_gpu_chunk *chunks, int want, const uint8_t *pages, const uint8_t *first, unsigned step)
{
  bool ok = count == want;
  for (int i = 0; ok && i < count; i++) {
    uint64_t size = (uint64_t)pages[i] * 0x1000;
    uint8_t *view = chunks[i].size == size && lseek(chunks[i].fd, 0, SEEK_END) == (off_t)size ? mmap(NULL, size, PROT_READ, MAP_SHARED, chunks[i].fd, 0) : MAP_FAILED;
    if (view == MAP_FAILED) { ok = false; break; }
    for (unsigned p = 0; p < pages[i]; p++) if (view[p * 0x1000] != (uint8_t)(first[i] + p * step)) ok = false;
    munmap(view, size);
  }
  if (count > 0) xh_gpu3d_chunks_free(chunks, (unsigned)count);
  return ok;
}

static void test_virtio_gpu3d(void)
{
  static xh_gpu3d b = { .capset_count = fake_capset_count, .capset_info = fake_capset_info, .capset = fake_capset, .ctx_create = fake_ctx_create,
                        .ctx_destroy = fake_ctx_destroy, .submit = fake_submit, .submit_fence = fake_submit_fence, .create_blob = fake_create_blob,
                        .map_blob = fake_map_blob, .attach = fake_attach, .unref = fake_unref };
  fake_display = (xh_display){ .width = 8, .height = 4, .stride = 32, .pixels = calloc(4, 32) };
  ready_count = 0;
  driver d = { .dev = xh_virtio_gpu_create_3d(0, 43, 8, 4, &b) };
  xh_virtio *dev = d.dev;
  uint16_t *used = xh_ram(GUEST_PA + 0x10000 + 0x2000, 0x1000);
  CHECK(dev && b.retire && b.dev == dev);
  W(&d, 0x14, 0);
  CHECK(R(&d, 8) == 16 && (R(&d, 0x10) & 0x59) == 0x59 && R(&d, 0x108) == 1 && R(&d, 0x10c) == 1);       // VIRGL, BLOB, CONTEXT_INIT, GUEST_HANDLE; one capset
  CHECK(driver_init(&d, 0x59, 2, GUEST_PA + 0x10000));
  uint32_t *cmd = xh_ram(GUEST_PA + 0x30000, 4096), *resp = xh_ram(GUEST_PA + 0x31000, 4096), *extra = xh_ram(GUEST_PA + 0x32000, 4096);
#define G(words, resp_bytes) gpu3d_send(&d, dev, words, 0, resp_bytes)
#define GX(words, extra_bytes, resp_bytes) gpu3d_send(&d, dev, words, extra_bytes, resp_bytes)
#define CLEAR() (memset(cmd, 0, 4096), memset(resp, 0xee, 256))
  // capsets
  CLEAR(); cmd[0] = 0x108; cmd[6] = 0;
  CHECK(G(8, 40) == 1 && resp[0] == 0x1102 && resp[6] == 6 && resp[7] == 0 && resp[8] == 64);
  cmd[6] = 1;
  CHECK(G(8, 40) == 1 && resp[0] == 0x1102 && resp[6] == 0 && resp[8] == 0);                              // past the last: zeros
  CLEAR(); cmd[0] = 0x109; cmd[6] = 6;
  CHECK(G(8, 24 + 64) == 1 && resp[0] == 0x1103 && ((uint8_t *)resp)[24] == 0xc0 && ((uint8_t *)resp)[87] == 0xff);
  cmd[6] = 2;
  CHECK(G(8, 24 + 64) == 1 && resp[0] == 0x1205);                                                         // no such capset
  // contexts
  CLEAR(); cmd[0] = 0x200; cmd[4] = 5; cmd[6] = 4; cmd[7] = 6; memcpy(&cmd[8], "test", 4);
  CHECK(G(24, 24) == 1 && resp[0] == 0x1100 && fake.contexts == 1 && fake.live[0] == 5);
  CHECK(G(24, 24) == 1 && resp[0] == 0x1204 && fake.contexts == 1);                                       // the id is taken
  cmd[4] = 0;
  CHECK(G(24, 24) == 1 && resp[0] == 0x1204);
  cmd[4] = 99;
  CHECK(G(24, 24) == 1 && resp[0] == 0x1201 && fake.contexts == 1);                                       // the renderer has no memory
  cmd[4] = 6;
  CHECK(G(24, 24) == 1 && resp[0] == 0x1100 && fake.contexts == 2);
  // a context's shmem: a HOST3D blob, mapped from the pool
  CLEAR(); cmd[0] = 0x10c; cmd[4] = 5; cmd[6] = 10; cmd[7] = 2; cmd[8] = 1; cmd[9] = 0; cmd[10] = 0; cmd[12] = 16384;
  CHECK(G(14, 24) == 1 && resp[0] == 0x1100 && fake.blobs == 1 && fake.blob_ctx == 5 && fake.blob_mem == 2 && fake.blob_size == 16384 && fake.nentries == 0);
  CHECK(G(14, 24) == 1 && resp[0] == 0x1203 && fake.blobs == 1);                                          // the id is taken
  cmd[6] = 12; cmd[4] = 7;
  CHECK(G(14, 24) == 1 && resp[0] == 0x1204);                                                             // no such context
  cmd[4] = 5; cmd[9] = 1;
  CHECK(GX(14, 16, 24) == 1 && resp[0] == 0x1205);                                                        // a host blob has no guest pages
  CLEAR(); cmd[0] = 0x208; cmd[4] = 5; cmd[6] = 10;
  CHECK(G(10, 32) == 1 && resp[0] == 0x1106 && resp[6] == (0x80000000u | 1) && resp[7] == 0x200000);     // MAP_INFO_POOL | cached, pool offset
  cmd[6] = 77;
  CHECK(G(10, 32) == 1 && resp[0] == 0x1203);
  // a BO: the guest's pages (HOST3D_GUEST + CREATE_GUEST_HANDLE), two segments
  CLEAR(); cmd[0] = 0x10c; cmd[4] = 5; cmd[6] = 11; cmd[7] = 3; cmd[8] = 9; cmd[9] = 2; cmd[10] = 7; cmd[12] = 0x3000;
  uint64_t segs[4] = { GUEST_PA + 0x50000, 0x2000, GUEST_PA + 0x60000, 0x1000 };
  memcpy(extra, segs, sizeof segs);
  CHECK(GX(14, 32, 24) == 1 && resp[0] == 0x1100 && fake.blobs == 2 && fake.blob_mem == 3 && fake.blob_flags == 9 && fake.blob_id == 7 && fake.nentries == 2);
  CHECK(fake.entries[0].addr == GUEST_PA + 0x50000 && fake.entries[0].length == 0x2000 && fake.entries[1].addr == GUEST_PA + 0x60000 && fake.entries[1].length == 0x1000);
  CLEAR(); cmd[0] = 0x208; cmd[4] = 5; cmd[6] = 11;
  CHECK(G(10, 32) == 1 && resp[0] == 0x1205);                                                             // the guest's own pages are not in the pool
  CLEAR(); cmd[0] = 0x10c; cmd[4] = 5; cmd[6] = 13; cmd[7] = 3; cmd[8] = 9; cmd[9] = 2; cmd[10] = 8; cmd[12] = 0x3000;
  segs[1] = 0x1800; memcpy(extra, segs, sizeof segs);
  CHECK(GX(14, 32, 24) == 1 && resp[0] == 0x1205);                                                        // not whole pages
  segs[1] = 0x2000; segs[2] = 0x1000; memcpy(extra, segs, sizeof segs);
  CHECK(GX(14, 32, 24) == 1 && resp[0] == 0x1205);                                                        // not guest RAM
  segs[2] = GUEST_PA + 0x60000; cmd[12] = 0x4000; memcpy(extra, segs, sizeof segs);
  CHECK(GX(14, 32, 24) == 1 && resp[0] == 0x1205);                                                        // smaller than the blob
  cmd[12] = 0x3000;
  CHECK(GX(14, 16, 24) == 1 && resp[0] == 0x1205);                                                        // fewer segments than it says
  cmd[10] = 0xbad;
  CHECK(GX(14, 32, 24) == 1 && resp[0] == 0x1200 && fake.blobs == 2);                                     // the renderer refuses
  cmd[10] = 8;
  CHECK(GX(14, 32, 24) == 1 && resp[0] == 0x1100 && fake.blobs == 3);                                     // and the id is still free
  // attach, detach
  CLEAR(); cmd[0] = 0x202; cmd[4] = 6; cmd[6] = 11;
  CHECK(G(8, 24) == 1 && resp[0] == 0x1100 && fake.attaches == 1);
  cmd[0] = 0x203;
  CHECK(G(8, 24) == 1 && resp[0] == 0x1100 && fake.attaches == 101);
  cmd[6] = 55;
  CHECK(G(8, 24) == 1 && resp[0] == 0x1203);
  // SUBMIT_3D: the stream reaches the renderer whole, also when it is a part of its own in the chain
  CLEAR(); cmd[0] = 0x207; cmd[4] = 5; cmd[6] = 12; cmd[8] = 0x11111111; cmd[9] = 0x22222222; cmd[10] = 0x33333333;
  CHECK(G(11, 24) == 1 && resp[0] == 0x1100 && resp[1] == 0 && fake.submits == 1 && fake.last_size == 12 && !memcmp(fake.last, &cmd[8], 12));
  for (unsigned i = 0; i < 50; i++) extra[i] = i * 0x01010101u;
  cmd[6] = 200;
  CHECK(GX(8, 200, 24) == 1 && resp[0] == 0x1100 && fake.submits == 2 && fake.last_size == 200 && !memcmp(fake.last, extra, 200));
  cmd[6] = 204;
  CHECK(GX(8, 200, 24) == 1 && resp[0] == 0x1205 && fake.submits == 2);                                   // longer than the chain
  cmd[6] = 0;
  CHECK(G(8, 24) == 1 && resp[0] == 0x1205);
  cmd[6] = 8; cmd[8] = 0xdead;
  CHECK(G(10, 24) == 1 && resp[0] == 0x1200 && fake.submits == 2);                                        // the renderer refuses the stream
  cmd[4] = 8; cmd[8] = 1;
  CHECK(G(10, 24) == 1 && resp[0] == 0x1204);
  // fences. On the context's ring 1: the command stays with the device until the ring passes the fence.
  CLEAR(); cmd[0] = 0x207; cmd[1] = 3; cmd[2] = 1; cmd[4] = 5; cmd[5] = 1; cmd[6] = 4; cmd[8] = 0xaa;
  uint16_t before = used[1];
  CHECK(G(9, 24) == 0 && fake.submits == 3 && fake.fences == 1 && used[1] == before);
  CHECK(resp[0] == 0x1100 && resp[1] == 3 && resp[2] == 1 && resp[4] == 5 && (resp[5] & 0xff) == 1);      // the response is written, the chain is held
  cmd[2] = 2;
  CHECK(G(9, 24) == 0 && fake.submits == 4);
  cmd[2] = 3;
  CHECK(G(9, 24) == 0 && fake.submits == 5);
  cmd[1] = 0; cmd[2] = 0;
  CHECK(G(9, 24) == 1 && fake.submits == 6);                                                              // an unfenced one passes the held ones
  b.retire(dev, 6, 1, 3);                                                                                 // another context's ring: nothing
  b.retire(dev, 5, 2, 3);                                                                                 // another ring: nothing
  CHECK(used[1] == d.used_seen[0]);
  b.retire(dev, 5, 1, 2);                                                                                 // fences 1 and 2 (the renderer may name only the latest)
  CHECK((uint16_t)(used[1] - d.used_seen[0]) == 2);
  b.retire(dev, 5, 1, 3);
  CHECK((uint16_t)(used[1] - d.used_seen[0]) == 3 && R(&d, 0x60) == 1);
  d.used_seen[0] = used[1];
  cmd[1] = 3; cmd[2] = 9; cmd[5] = 0;
  CHECK(G(9, 24) == 1 && resp[2] == 9);                                                                   // ring 0: retired inside the call
  cmd[5] = 9;
  CHECK(G(9, 24) == 1);                                                                                   // a fence the renderer refuses is not held
  cmd[1] = 1; cmd[2] = 0x77; cmd[5] = 0;
  CHECK(G(9, 24) == 1 && resp[1] == 1 && resp[2] == 0x77 && fake.fences == 5);                            // the device's own ring: at once, the renderer not asked
  // a fenced command of a context that is destroyed comes back
  cmd[1] = 3; cmd[2] = 4; cmd[5] = 1;
  CHECK(G(9, 24) == 0);
  CLEAR(); cmd[0] = 0x201; cmd[4] = 5;
  CHECK(G(6, 24) == 2 && resp[0] == 0x1100 && fake.contexts == 1 && fake.live[0] == 0);
  CHECK(G(6, 24) == 1 && resp[0] == 0x1204);
  // the display's surface: a guest blob as the scanout, its rows copied at the flush
  CLEAR(); cmd[0] = 0x10c; cmd[6] = 20; cmd[7] = 1; cmd[8] = 8; cmd[9] = 1; cmd[12] = 0x1000;
  segs[0] = GUEST_PA + 0x40000; segs[1] = 0x1000; memcpy(extra, segs, 16);
  CHECK(GX(14, 16, 24) == 1 && resp[0] == 0x1100 && fake.blob_mem == 1 && fake.blob_ctx == 0);
  uint32_t *pixels = xh_ram(GUEST_PA + 0x40000, 0x1000);
  for (int i = 0; i < 4 * 10; i++) pixels[i] = 0x00102000u | (uint32_t)i;                                 // BGRX, stride 40: B = index
  CLEAR(); cmd[0] = 0x10d; cmd[8] = 8; cmd[9] = 4; cmd[10] = 0; cmd[11] = 20; cmd[12] = 8; cmd[13] = 4; cmd[14] = 2; cmd[16] = 40;
  CHECK(G(24, 24) == 1 && resp[0] == 0x1100);
  cmd[16] = 16;
  CHECK(G(24, 24) == 1 && resp[0] == 0x1205);                                                             // a stride shorter than a row
  cmd[16] = 40; cmd[13] = 200;
  CHECK(G(24, 24) == 1 && resp[0] == 0x1205);                                                             // more rows than the blob holds
  cmd[13] = 4; cmd[11] = 11;
  CHECK(G(24, 24) == 1 && resp[0] == 0x1100);                                                             // any guest blob can be one
  cmd[11] = 10;
  CHECK(G(24, 24) == 1 && resp[0] == 0x1203);                                                             // not the renderer's own
  cmd[11] = 20;
  CHECK(G(24, 24) == 1 && resp[0] == 0x1100);
  CLEAR(); cmd[0] = 0x104; cmd[1] = 1; cmd[2] = 5; cmd[6] = 1; cmd[7] = 2; cmd[8] = 3; cmd[9] = 2; cmd[10] = 20;                    // flush (1,2) 3x2, fenced
  CHECK(G(12, 24) == 1 && resp[0] == 0x1100 && resp[2] == 5 && ready_count == 1);
  CHECK(flush_rect[0] == 1 && flush_rect[1] == 2 && flush_rect[2] == 3 && flush_rect[3] == 2);
  const uint8_t *px = fake_display.pixels + 2 * 32 + 1 * 4;
  CHECK(px[0] == 21 && px[1] == 0x20 && px[2] == 0x10 && fake_display.pixels[3 * 32 + 3 * 4] == 33 && fake_display.pixels[0] == 0 && fake_display.pixels[2 * 32 + 4 * 4] == 0);
  {
    // Half floats (display-format=rgba16f, oneios-display-wide=1): a 'RGhA' scanout's rows go to a half-float display as
    // they are, and a B,G,R,X one is decoded to linear light; onto the 8-bit display half floats are encoded and clamped.
    uint8_t *eight = fake_display.pixels;
    fake_display = (xh_display){ .width = 8, .height = 4, .stride = 64, .format = XH_PIXELS_RGBA16F, .bpp = 8, .pixels = calloc(4, 64) };
    uint16_t *halves = xh_ram(GUEST_PA + 0x40000, 0x1000);
    for (int i = 0; i < 8 * 4 * 4; i++) halves[i] = (uint16_t)(0x3000 + i);
    halves[(1 * 8 + 2) * 4] = xh_half_from_float(1.2249f);             // pixel (2,1): Display P3's red, outside sRGB
    halves[(1 * 8 + 2) * 4 + 1] = xh_half_from_float(-0.042f);
    CLEAR(); cmd[0] = 0x10d; cmd[8] = 8; cmd[9] = 4; cmd[10] = 0; cmd[11] = 20; cmd[12] = 8; cmd[13] = 4; cmd[14] = XH_VIRTIO_GPU_FORMAT_RGBA16F; cmd[16] = 64;
    CHECK(G(24, 24) == 1 && resp[0] == 0x1100);
    cmd[16] = 60;
    CHECK(G(24, 24) == 1 && resp[0] == 0x1205);                                                           // not whole half-float pixels
    cmd[16] = 32;
    CHECK(G(24, 24) == 1 && resp[0] == 0x1205);                                                           // an 8-bit stride: a row does not fit
    cmd[16] = 64; cmd[14] = 99;
    CHECK(G(24, 24) == 1 && resp[0] == 0x1205);                                                           // an unknown format
    cmd[14] = XH_VIRTIO_GPU_FORMAT_RGBA16F;
    CHECK(G(24, 24) == 1 && resp[0] == 0x1100);
    CLEAR(); cmd[0] = 0x104; cmd[6] = 1; cmd[7] = 1; cmd[8] = 3; cmd[9] = 2; cmd[10] = 20;                 // flush (1,1) 3x2
    CHECK(G(12, 24) == 1 && resp[0] == 0x1100 && flush_rect[0] == 1 && flush_rect[2] == 3);
    const uint16_t *got = (const uint16_t *)(fake_display.pixels + 1 * 64 + 2 * 8);
    CHECK(got[0] == xh_half_from_float(1.2249f) && got[1] == xh_half_from_float(-0.042f) && got[2] == halves[(1 * 8 + 2) * 4 + 2]);
    CHECK(!memcmp(fake_display.pixels + 2 * 64 + 3 * 8, halves + (2 * 8 + 3) * 4, 8) && fake_display.pixels[0] == 0);
    // a B,G,R,X scanout onto the half-float display: decoded
    uint32_t *bgrx = xh_ram(GUEST_PA + 0x40000, 0x1000);
    for (int i = 0; i < 4 * 10; i++) bgrx[i] = 0x00ff8000u;                                               // B 0, G 128, R 255
    CLEAR(); cmd[0] = 0x10d; cmd[8] = 8; cmd[9] = 4; cmd[10] = 0; cmd[11] = 20; cmd[12] = 8; cmd[13] = 4; cmd[14] = 2; cmd[16] = 40;
    CHECK(G(24, 24) == 1 && resp[0] == 0x1100);
    CLEAR(); cmd[0] = 0x104; cmd[6] = 0; cmd[7] = 0; cmd[8] = 1; cmd[9] = 1; cmd[10] = 20;
    CHECK(G(12, 24) == 1 && resp[0] == 0x1100);
    got = (const uint16_t *)fake_display.pixels;
    CHECK(got[0] == 0x3c00 && got[1] == xh_half_from_float(0.21586050f) && got[2] == 0 && got[3] == 0x3c00);
    // a pixel split between two segments of a blob: rows at offset 4060 put pixel 4 across the page boundary
    CLEAR(); cmd[0] = 0x10c; cmd[6] = 30; cmd[7] = 1; cmd[8] = 8; cmd[9] = 2; cmd[12] = 0x2000;
    uint64_t two[4] = { GUEST_PA + 0x42000, 0x1000, GUEST_PA + 0x44000, 0x1000 };
    memcpy(extra, two, sizeof two);
    CHECK(GX(14, 32, 24) == 1 && resp[0] == 0x1100);
    uint16_t want[8][4];
    for (int p = 0; p < 8; p++) for (int c = 0; c < 4; c++) want[p][c] = (uint16_t)(0x3800 + p * 4 + c);
    for (int b = 0; b < 64; b++) {
      unsigned at = 4060 + b;
      uint8_t *dst = xh_ram(at < 4096 ? GUEST_PA + 0x42000 + at : GUEST_PA + 0x44000 + (at - 4096), 1);
      *dst = ((const uint8_t *)want)[b];
    }
    CLEAR(); cmd[0] = 0x10d; cmd[8] = 8; cmd[9] = 1; cmd[10] = 0; cmd[11] = 30; cmd[12] = 8; cmd[13] = 1; cmd[14] = XH_VIRTIO_GPU_FORMAT_RGBA16F; cmd[16] = 64; cmd[20] = 4060;
    CHECK(G(24, 24) == 1 && resp[0] == 0x1100);
    CLEAR(); cmd[0] = 0x104; cmd[6] = 0; cmd[7] = 0; cmd[8] = 8; cmd[9] = 1; cmd[10] = 30;
    CHECK(G(12, 24) == 1 && resp[0] == 0x1100);
    CHECK(!memcmp(fake_display.pixels, want, sizeof want));
    // half floats onto the 8-bit display: encoded to sRGB, clamped
    free(fake_display.pixels);
    fake_display = (xh_display){ .width = 8, .height = 4, .stride = 32, .format = XH_PIXELS_BGRX8, .bpp = 4, .pixels = eight };
    memset(eight, 0, 4 * 32);
    CHECK(G(12, 24) == 1 && resp[0] == 0x1100);                                                           // the same flush
    CHECK(eight[0] == 188 && eight[3] == 255);                                                            // 0.5 is sRGB 188; X opaque
    for (int p = 0; p < 8; p++) {
      const uint8_t *t = xh_half_to_srgb8();
      if (eight[p * 4] != t[want[p][2]] || eight[p * 4 + 1] != t[want[p][1]] || eight[p * 4 + 2] != t[want[p][0]]) { CHECK(!"half floats encoded to B,G,R,X"); break; }
    }
    CLEAR(); cmd[0] = 0x10d; cmd[8] = 8; cmd[9] = 4; cmd[10] = 0; cmd[11] = 20; cmd[12] = 8; cmd[13] = 4; cmd[14] = 2; cmd[16] = 40;
    CHECK(G(24, 24) == 1 && resp[0] == 0x1100);                                                           // back to the B,G,R,X scanout
    for (int i = 0; i < 4 * 10; i++) bgrx[i] = 0x00102000u | (uint32_t)i;
  }
  // the 2D commands stay
  CLEAR(); cmd[0] = 0x100;
  CHECK(G(6, 24 + 16 * 24) == 1 && resp[0] == 0x1101 && resp[8] == 8 && resp[9] == 4);
  cmd[0] = 0x101; cmd[6] = 20; cmd[7] = 67; cmd[8] = 8; cmd[9] = 4;
  CHECK(G(10, 24) == 1 && resp[0] == 0x1203);                                                             // a blob has the id
  cmd[6] = 1;
  CHECK(G(10, 24) == 1 && resp[0] == 0x1100);
  // unref
  CLEAR(); cmd[0] = 0x102; cmd[6] = 11;
  unsigned blobs = fake.blobs;
  CHECK(G(8, 24) == 1 && resp[0] == 0x1100 && fake.blobs == blobs - 1);
  CHECK(G(8, 24) == 1 && resp[0] == 0x1203);
  cmd[6] = 1;
  CHECK(G(8, 24) == 1 && resp[0] == 0x1100);                                                              // the 2D resource

  // A generated stream: 4000 commands of every type and of none, with lengths, ids and flags drawn at random. The
  // device answers each one in order with a response or an error, never touches memory it should not (the sanitizers
  // watch), and its contexts stay the renderer's.
  rnd_seed = getenv("XNUHOST_TEST_SEED") ? (uint32_t)strtoul(getenv("XNUHOST_TEST_SEED"), NULL, 0) : 0x1234567;
#define RND() rnd()
  static const uint32_t types[] = { 0x100, 0x101, 0x102, 0x103, 0x104, 0x105, 0x106, 0x107, 0x108, 0x109, 0x10a, 0x10c, 0x10d, 0x200, 0x201, 0x202, 0x203, 0x207, 0x208, 0x209, 0x300, 0x999 };
  unsigned answered = 0, errors = 0, held = 0;
  for (unsigned i = 0; i < 4000; i++) {
    for (unsigned w = 0; w < 40; w++) { uint32_t r = RND(); cmd[w] = r % 7 ? RND() % 24 : r << 8; }
    for (unsigned w = 0; w < 64; w++) { uint32_t r = RND(), q = RND(); extra[w] = r % 3 == 0 ? q : w % 4 == 0 ? (uint32_t)(GUEST_PA + (q % 512) * 0x1000) : w % 4 == 2 ? (q % 4) * 0x1000 : 0; }
    cmd[0] = types[RND() % (sizeof types / sizeof *types)];
    cmd[1] = RND() % 4; cmd[4] = RND() % 10; cmd[5] = RND() % 3;
    if (cmd[0] == 0x200 && RND() % 2) { cmd[6] = 4; cmd[7] = 6; memcpy(&cmd[8], "test", 4); }
    if (cmd[0] == 0x10c) { cmd[6] = 30 + RND() % 20; cmd[7] = 1 + RND() % 3; cmd[9] = cmd[7] == 2 ? 0 : 1 + RND() % 3; cmd[11] = 0; cmd[12] = (1 + RND() % 3) * 0x1000; cmd[13] = 0; }
    if (cmd[0] == 0x102 || cmd[0] == 0x208 || cmd[0] == 0x202) cmd[6] = 30 + RND() % 20;
    if (cmd[0] == 0x207) cmd[6] = RND() % 80;
    unsigned words = 6 + RND() % 30, more = RND() % 3 ? 0 : (RND() % 64) * 4, room = RND() % 9 ? 24 + RND() % 200 : RND() % 24;
    memset(resp, 0xee, 24);
    unsigned n = more ? GX(words, more, room) : G(words, room);
    bool fenced = (cmd[1] & 3) == 3 && cmd[5] != 0;
    if (!n && fenced) {                                                                                   // held on a context's ring: retire it
      held++;
      b.retire(dev, cmd[4], cmd[5], cmd[2] | (uint64_t)cmd[3] << 32);
      n = (uint16_t)(used[1] - d.used_seen[0]);
      d.used_seen[0] = used[1];
    }
    if (n != 1) fprintf(stderr, "i %u n %u type %x flags %x ctx %u ring %u words %u more %u room %u\n", i, n, cmd[0], cmd[1], cmd[4], cmd[5], words, more, room);
    CHECK(n == 1);
    if (room < 24) continue;                                                                              // no room for a response: returned unused
    answered++;
    CHECK((resp[0] >= 0x1100 && resp[0] <= 0x1106) || (resp[0] >= 0x1200 && resp[0] <= 0x1205));
    if (resp[0] >= 0x1200) errors++;
    if (cmd[1] & 1) CHECK((resp[1] & 1) && resp[2] == cmd[2]);
    unsigned live = 0;
    for (unsigned c = 0; c < 8; c++) live += fake.live[c] != 0;
    CHECK(live == fake.contexts && live <= 8);
  }
  CHECK(answered > 3000 && errors > 500 && errors < answered && held > 5);
  // a reset forgets the contexts and the blobs, and the device starts again
  CLEAR(); cmd[0] = 0x200; cmd[4] = 3; cmd[6] = 4; cmd[7] = 6; memcpy(&cmd[8], "test", 4);
  G(24, 24);
  cmd[0] = 0x207; cmd[1] = 3; cmd[2] = 50; cmd[5] = 1; cmd[6] = 4;
  G(9, 24);                                                                                               // may be held across the reset
  W(&d, 0x70, 0);
  xh_virtio_gpu_idle(dev);
  CHECK(fake.contexts == 0 && fake.blobs == 0);
  b.retire(dev, 3, 1, 50);                                                                                // late: its chain was the old queue's
  CHECK(driver_init(&d, 0x59, 2, GUEST_PA + 0x10000));
  CLEAR(); cmd[0] = 0x200; cmd[4] = 5; cmd[6] = 4; cmd[7] = 6; memcpy(&cmd[8], "test", 4);
  CHECK(G(24, 24) == 1 && resp[0] == 0x1100 && fake.contexts == 1);
  // a udmabuf over guest segments: refused without touching the device node when a segment is not the RAM's
  xh_gpu_entry outside = { .addr = 0x1000, .length = 0x1000 }, odd = { .addr = GUEST_PA + 0x800, .length = 0x1000 };
  errno = 0;
  CHECK(xh_gpu3d_udmabuf(-1, GUEST_PA, sizeof guest, &outside, 1) == -1 && errno == EINVAL);
  CHECK(xh_gpu3d_udmabuf(-1, GUEST_PA, sizeof guest, &odd, 1) == -1 && errno == EINVAL);
  CHECK(xh_gpu3d_udmabuf(-1, GUEST_PA, sizeof guest, &odd, 0) == -1);
  // and a real one where this host has /dev/udmabuf: three segments of a sealed memfd, two of them adjacent, read back
  // through the dma-buf in the blob's order; a memfd without the shrink seal is refused (mem.c seals the RAM for this)
  int node = open("/dev/udmabuf", O_RDWR | O_CLOEXEC);
  if (node >= 0) {
    close(node);
    int ram_fd = (int)syscall(SYS_memfd_create, "test-ram", 2u), bare = (int)syscall(SYS_memfd_create, "test-bare", 0u);
    CHECK(ram_fd >= 0 && bare >= 0 && ftruncate(ram_fd, 1 << 20) == 0 && ftruncate(bare, 1 << 20) == 0 && fcntl(ram_fd, 1033, 2) == 0);
    uint8_t *ram = mmap(NULL, 1 << 20, PROT_READ | PROT_WRITE, MAP_SHARED, ram_fd, 0);
    CHECK(ram != MAP_FAILED);
    for (unsigned i = 0; i < 256; i++) ram[i * 0x1000] = (uint8_t)i;
    xh_gpu_entry three[3] = { { GUEST_PA + 0x9000, 0x1000, 0 }, { GUEST_PA + 0x20000, 0x1000, 0 }, { GUEST_PA + 0x21000, 0x2000, 0 } };
    int buf = xh_gpu3d_udmabuf(ram_fd, GUEST_PA, 1 << 20, three, 3);
    CHECK(buf >= 0);
    uint8_t *view = buf >= 0 ? mmap(NULL, 0x4000, PROT_READ | PROT_WRITE, MAP_SHARED, buf, 0) : MAP_FAILED;
    CHECK(view != MAP_FAILED && lseek(buf, 0, SEEK_END) == 0x4000);
    if (view != MAP_FAILED) {
      CHECK(view[0] == 9 && view[0x1000] == 0x20 && view[0x2000] == 0x21 && view[0x3000] == 0x22);
      view[0x1001] = 0x5a;                                                                               // the same pages, not a copy
      CHECK(ram[0x20001] == 0x5a);
      munmap(view, 0x4000);
    }
    if (buf >= 0) close(buf);
    errno = 0;
    CHECK(xh_gpu3d_udmabuf(bare, GUEST_PA, 1 << 20, three, 3) == -1 && errno == EINVAL);
    // A blob past udmabuf's limits is several udmabufs in a row. With the limits set to 8 pages and 3 runs:
    xh_gpu3d_udmabuf_set_limits(0x8000, 3);
    xh_gpu_chunk *chunks = NULL;
    // 20 pages in one run: 8 + 8 + 4, by size
    xh_gpu_entry run = { GUEST_PA + 100 * 0x1000, 20 * 0x1000, 0 };
    static const uint8_t run_pages[] = { 8, 8, 4 }, run_first[] = { 100, 108, 116 };
    int count = xh_gpu3d_udmabufs(ram_fd, GUEST_PA, 1 << 20, &run, 1, &chunks);
    CHECK(chunks_are(count, chunks, 3, run_pages, run_first, 1));
    // 7 pages, every second one: 3 + 3 + 1, by runs
    xh_gpu_entry apart[7];
    for (unsigned i = 0; i < 7; i++) apart[i] = (xh_gpu_entry){ GUEST_PA + (10 + 2 * i) * 0x1000, 0x1000, 0 };
    static const uint8_t apart_pages[] = { 3, 3, 1 }, apart_first[] = { 10, 16, 22 };
    count = xh_gpu3d_udmabufs(ram_fd, GUEST_PA, 1 << 20, apart, 7, &chunks);
    CHECK(chunks_are(count, chunks, 3, apart_pages, apart_first, 2));
    // two adjacent segments joined (4 pages, one run), then 6 pages: the second run is cut where the first udmabuf is full
    xh_gpu_entry mixed[3] = { { GUEST_PA + 30 * 0x1000, 0x2000, 0 }, { GUEST_PA + 32 * 0x1000, 0x2000, 0 }, { GUEST_PA + 40 * 0x1000, 0x6000, 0 } };
    count = xh_gpu3d_udmabufs(ram_fd, GUEST_PA, 1 << 20, mixed, 3, &chunks);
    CHECK(count == 2 && chunks[0].size == 0x8000 && chunks[1].size == 0x2000 && lseek(chunks[0].fd, 0, SEEK_END) == 0x8000 && lseek(chunks[1].fd, 0, SEEK_END) == 0x2000);
    if (count == 2) {
      uint8_t *a = mmap(NULL, 0x8000, PROT_READ, MAP_SHARED, chunks[0].fd, 0), *b2 = mmap(NULL, 0x2000, PROT_READ, MAP_SHARED, chunks[1].fd, 0);
      CHECK(a != MAP_FAILED && b2 != MAP_FAILED && a[0] == 30 && a[0x3000] == 33 && a[0x4000] == 40 && a[0x7000] == 43 && b2[0] == 44 && b2[0x1000] == 45);
      munmap(a, 0x8000); munmap(b2, 0x2000);
    }
    if (count > 0) xh_gpu3d_chunks_free(chunks, (unsigned)count);
    // exactly the limit is one udmabuf; one page more is not one
    xh_gpu_entry full = { GUEST_PA + 50 * 0x1000, 0x8000, 0 }, over = { GUEST_PA + 50 * 0x1000, 0x9000, 0 };
    buf = xh_gpu3d_udmabuf(ram_fd, GUEST_PA, 1 << 20, &full, 1);
    CHECK(buf >= 0 && lseek(buf, 0, SEEK_END) == 0x8000);
    if (buf >= 0) close(buf);
    errno = 0;
    CHECK(xh_gpu3d_udmabuf(ram_fd, GUEST_PA, 1 << 20, &over, 1) == -1 && errno == E2BIG);
    // a bad segment anywhere in the list: nothing is made
    xh_gpu_entry bad[2] = { { GUEST_PA + 0x1000, 0x1000, 0 }, { GUEST_PA + (1 << 20), 0x1000, 0 } };
    errno = 0;
    CHECK(xh_gpu3d_udmabufs(ram_fd, GUEST_PA, 1 << 20, bad, 2, &chunks) == -1 && errno == EINVAL && !chunks);
    // The limits in use without a setting: the kernel's, never above 64 MiB and 1024 runs (the phone's). 72 MiB in one
    // run is 64 + 8 on any host; 1100 separate pages are 1024 + 76.
    uint64_t bytes = 0;
    unsigned runs = 0;
    xh_gpu3d_udmabuf_set_limits(0, 0);
    xh_gpu3d_udmabuf_limits(&bytes, &runs);
    CHECK(bytes && bytes <= (64u << 20) && runs && runs <= 1024);
    int big_fd = (int)syscall(SYS_memfd_create, "test-big", 2u);
    CHECK(big_fd >= 0 && ftruncate(big_fd, 96 << 20) == 0 && fcntl(big_fd, 1033, 2) == 0);
    if (bytes == (64u << 20) && runs == 1024) {
      xh_gpu_entry big = { GUEST_PA + (8 << 20), 72 << 20, 0 };
      count = xh_gpu3d_udmabufs(big_fd, GUEST_PA, 96 << 20, &big, 1, &chunks);
      CHECK(count == 2 && chunks[0].size == (64u << 20) && chunks[1].size == (8u << 20) && lseek(chunks[0].fd, 0, SEEK_END) == (64 << 20) && lseek(chunks[1].fd, 0, SEEK_END) == (8 << 20));
      if (count == 2) {                                                                                  // the second begins where the first ends
        uint8_t *a = mmap(NULL, 64 << 20, PROT_READ | PROT_WRITE, MAP_SHARED, chunks[0].fd, 0), *b2 = mmap(NULL, 8 << 20, PROT_READ | PROT_WRITE, MAP_SHARED, chunks[1].fd, 0);
        uint8_t *all = mmap(NULL, 96 << 20, PROT_READ | PROT_WRITE, MAP_SHARED, big_fd, 0);
        CHECK(a != MAP_FAILED && b2 != MAP_FAILED && all != MAP_FAILED);
        a[(64 << 20) - 1] = 0x11; b2[0] = 0x22; b2[(8 << 20) - 1] = 0x33;
        CHECK(all[(72 << 20) - 1] == 0x11 && all[72 << 20] == 0x22 && all[(80 << 20) - 1] == 0x33);
        munmap(a, 64 << 20); munmap(b2, 8 << 20); munmap(all, 96 << 20);
      }
      if (count > 0) xh_gpu3d_chunks_free(chunks, (unsigned)count);
      static xh_gpu_entry many[1100];
      for (unsigned i = 0; i < 1100; i++) many[i] = (xh_gpu_entry){ GUEST_PA + (uint64_t)i * 0x2000, 0x1000, 0 };
      count = xh_gpu3d_udmabufs(big_fd, GUEST_PA, 96 << 20, many, 1100, &chunks);
      CHECK(count == 2 && chunks[0].size == 1024 * 0x1000 && chunks[1].size == 76 * 0x1000);
      if (count > 0) xh_gpu3d_chunks_free(chunks, (unsigned)count);
      printf("virtio-gpu 3D: blobs past udmabuf's limits (64 MiB, 1024 runs) split and checked on this host\n");
    } else printf("virtio-gpu 3D: this host's udmabuf limits are %llu MiB and %u runs: the 64 MiB split is not checked\n", (unsigned long long)(bytes >> 20), runs);
    close(big_fd);
    munmap(ram, 1 << 20); close(ram_fd); close(bare);
    printf("virtio-gpu 3D: udmabuf checked on this host\n");
  } else printf("virtio-gpu 3D: no /dev/udmabuf here (%s): the real import is not checked\n", strerror(errno));
  const char *why = NULL;
  CHECK(!xh_gpu3d_kgsl_open(-1, GUEST_PA, sizeof guest, guest, GUEST_PA, 8 << 20, NULL, &why) && why && *why);   // no drm2kgsl in the host build
  // The two lanes: a blob an app's context is still making does not hold back the render server's submission behind it.
  {
    CLEAR(); cmd[0] = 0x200; cmd[4] = 40; cmd[6] = 4; cmd[7] = 6; memcpy(&cmd[8], "test", 4);
    CHECK(G(24, 24) == 1 && resp[0] == 0x1100);
    CLEAR(); cmd[0] = 0x200; cmd[4] = 41; cmd[6] = 14; cmd[7] = 6; memcpy(&cmd[8], "xnu:backboardd", 14);
    CHECK(G(24, 24) == 1 && resp[0] == 0x1100);
    uint32_t *ca = xh_ram(GUEST_PA + 0x33000, 4096), *ra = xh_ram(GUEST_PA + 0x34000, 4096), *cb = xh_ram(GUEST_PA + 0x35000, 4096), *rb = xh_ram(GUEST_PA + 0x36000, 4096);
    memset(ca, 0, 4096); memset(cb, 0, 4096); memset(ra, 0xee, 64); memset(rb, 0xee, 64);
    ca[0] = 0x10c; ca[4] = 40; ca[6] = 600; ca[7] = 2; ca[10] = 0x5105; ca[12] = 16384;              // the app's blob, held
    cb[0] = 0x207; cb[4] = 41; cb[6] = 8; memcpy(&cb[8], "frame!!!", 8);                            // the render server's SUBMIT_3D
    uint16_t *used0 = xh_ram(d.queue_pa[0] + 0x2000, 0x1000), seen = d.used_seen[0];
    fake_blob_held = 1;
    driver_send(&d, 0, (const uint64_t[][3]){ { GUEST_PA + 0x33000, 14 * 4, 0 }, { GUEST_PA + 0x34000, 24, 1 } }, 2, true);
    driver_send(&d, 0, (const uint64_t[][3]){ { GUEST_PA + 0x35000, 10 * 4, 0 }, { GUEST_PA + 0x36000, 24, 1 } }, 2, true);
    for (int i = 0; i < 2000 && rb[0] != 0x1100; i++) usleep(1000);
    CHECK(rb[0] == 0x1100 && ra[0] == 0xeeeeeeee);                                                  // the submit answered, the blob not yet
    fake_blob_held = 0;
    xh_virtio_gpu_idle(dev);
    CHECK(ra[0] == 0x1100 && (uint16_t)(used0[1] - seen) == 2);
    d.used_seen[0] = used0[1];
    CLEAR(); cmd[0] = 0x201; cmd[4] = 40;
    CHECK(G(6, 24) == 1 && resp[0] == 0x1100);
    cmd[4] = 41;
    CHECK(G(6, 24) == 1 && resp[0] == 0x1100);
  }
#undef G
#undef GX
#undef CLEAR
#undef RND
}

static void test_virtio_input(void)
{
  driver d = { .dev = xh_virtio_input_create(0, 43) };
  CHECK(d.dev && R(&d, 8) == 18);
  xh_virtio_input_set_abs(d.dev, 0x35, 0, 1439, 10);
  xh_virtio_input_set_abs(d.dev, 0x36, 0, 3119, 10);
  xh_virtio_mmio_write(d.dev, 0x100, 1, 0x12);                                 // select ABS_INFO
  xh_virtio_mmio_write(d.dev, 0x101, 1, 0x36);
  CHECK(xh_virtio_mmio_read(d.dev, 0x102, 1) == 20 && xh_virtio_mmio_read(d.dev, 0x108 + 4, 4) == 3119);
  xh_virtio_mmio_write(d.dev, 0x100, 1, 0x11);                                 // EV_BITS of EV_ABS
  xh_virtio_mmio_write(d.dev, 0x101, 1, 3);
  CHECK(xh_virtio_mmio_read(d.dev, 0x102, 1) == 7 && (xh_virtio_mmio_read(d.dev, 0x108 + 6, 1) & 0x60) == 0x60);
  xh_virtio_mmio_write(d.dev, 0x101, 1, 1);                                    // EV_KEY: volume down is key 114
  CHECK(xh_virtio_mmio_read(d.dev, 0x108 + 14, 1) & (1 << 2));
  xh_virtio_mmio_write(d.dev, 0x100, 1, 1);
  CHECK(xh_virtio_mmio_read(d.dev, 0x108, 4) == 0x69656e4f);                   // "Onei"
  CHECK(driver_init(&d, 0, 2, GUEST_PA + 0x10000));
  // events before the guest posts buffers are kept, and handed over at the report boundary
  xh_virtio_input_event(d.dev, 3, 0x35, 700);
  xh_virtio_input_event(d.dev, 3, 0x36, 1500);
  xh_virtio_input_event(d.dev, 0, 0, 0);
  uint16_t *used = xh_ram(d.queue_pa[0] + 0x2000, 16);
  CHECK(used[1] == 0);
  for (unsigned i = 0; i < 4; i++)
    driver_send(&d, 0, (const uint64_t[][3]){ { GUEST_PA + 0x50000 + i * 8, 8, 1 } }, 1, i == 3);
  CHECK(used[1] == 3);
  struct { uint16_t type, code; int32_t value; } *ev = xh_ram(GUEST_PA + 0x50000, 32);
  CHECK(ev[0].type == 3 && ev[0].code == 0x35 && ev[0].value == 700 && ev[1].value == 1500 && ev[2].type == 0);
  xh_virtio_input_event(d.dev, 1, 116, 1);                                     // the power key, no report yet
  CHECK(used[1] == 3);
  xh_virtio_input_event(d.dev, 0, 0, 0);
  CHECK(used[1] == 4 && ev[3].type == 1 && ev[3].code == 116);                 // only one buffer was left: the report waits
  driver_send(&d, 0, (const uint64_t[][3]){ { GUEST_PA + 0x50000, 8, 1 } }, 1, true);
  CHECK(used[1] == 5 && ev[0].type == 0);
}

// input.c's filter of one device: what reaches the guest, the scan time, SYN_DROPPED.
typedef struct { uint16_t type, code; int32_t value; } test_ev;
// The events the device wrote since the last call, in order; each buffer is posted again (8 in the queue).
static unsigned input_take(driver *d, test_ev *out, unsigned max)
{
  uint16_t *used = xh_ram(d->queue_pa[0] + 0x2000, 0x1000);
  struct vdesc *desc = xh_ram(d->queue_pa[0], 0x1000);
  unsigned n = 0;
  for (bool more = true; more;) {
    more = false;
    while (d->used_seen[0] != used[1] && n < max) {
      uint32_t id = ((uint32_t *)(used + 2))[2 * (d->used_seen[0] % 8)];
      uint64_t addr = desc[id].addr;
      memcpy(&out[n++], xh_ram(addr, 8), 8);
      d->used_seen[0]++;
      driver_send(d, 0, (const uint64_t[][3]){ { addr, 8, 1 } }, 1, false);
      more = true;
    }
    W(d, 0x50, 0);                                                              // the kick hands over what waited
  }
  return n;
}

static xh_input_state fake_state;
static int fake_read_state(xh_input_source *s, xh_input_state *state) { (void)s; *state = fake_state; return 0; }

static void test_input_filter(void)
{
  driver d = { .dev = xh_virtio_input_create(0, 45) };
  CHECK(d.dev && driver_init(&d, 0, 2, GUEST_PA + 0x10000));
  for (unsigned i = 0; i < 8; i++) driver_send(&d, 0, (const uint64_t[][3]){ { GUEST_PA + 0x50000 + i * 8, 8, 1 } }, 1, false);
  W(&d, 0x50, 0);
  test_ev got[32];
  const uint64_t hz = 19200000, counter = 1000000000;
  const int64_t now = 100000000000;
  xh_input_source s;
  xh_input_source_init(&s, d.dev, "touch", true);
  s.read_state = fake_read_state;
  // A touch-down, read 2 ms after its scan: the frame and then its scan time, 38400 ticks before the counter now.
  const xh_input_ev down[] = { { now - 2000000, 3, 0x2f, 0 }, { now - 2000000, 3, 0x39, 5 }, { now - 2000000, 3, 0x35, 700 },
                               { now - 2000000, 3, 0x36, 1500 }, { now - 2000000, 1, 0x14a, 1 }, { now - 2000000, 4, 5, 77 }, { now - 2000000, 0, 0, 0 } };
  xh_input_feed(&s, down, 7, now, counter, hz);
  unsigned n = input_take(&d, got, 32);
  CHECK(n == 7 && got[0].code == 0x2f && got[1].code == 0x39 && got[1].value == 5 && got[2].value == 700 && got[3].value == 1500 &&
        got[4].type == 1 && got[4].code == 0x14a && got[4].value == 1);                  // the device's own MSC is not passed on
  CHECK(got[5].type == 4 && got[5].code == 5 && (uint32_t)got[5].value == (uint32_t)(counter - 38400) && got[6].type == 0 && got[6].code == 0);
  CHECK(s.timing.frames == 1 && s.timing.began == 1 && s.sent_id[0] == 5 && s.timing.read_min_ns == 2000000);
  // A move 8 ms later: only what changed.
  const xh_input_ev move[] = { { now + 6000000, 3, 0x35, 710 }, { now + 6000000, 0, 0, 0 } };
  xh_input_feed(&s, move, 2, now + 6500000, counter, hz);
  n = input_take(&d, got, 32);
  CHECK(n == 3 && got[0].value == 710 && got[1].type == 4 && got[2].type == 0 && s.timing.frames == 2 && s.timing.gap_min_ns == 8000000);
  // Dropped events: the rest of that frame is ignored, the state is read again. Slot 0's finger lifted in the drop, slot 1
  // holds a new one: a frame of the lift (the gesture ends there), then a frame with the new contact.
  memset(&fake_state, 0, sizeof fake_state);
  for (unsigned i = 0; i < XH_INPUT_SLOTS; i++) fake_state.id[i] = -1;
  fake_state.id[1] = 9; fake_state.x[1] = 100; fake_state.y[1] = 200; fake_state.slot = 1;
  const xh_input_ev drop[] = { { now, 0, 3, 0 }, { now, 3, 0x35, 720 }, { now, 3, 0x39, -1 }, { now, 0, 0, 0 } };
  xh_input_feed(&s, drop, 4, now + 20000000, counter, hz);
  n = input_take(&d, got, 32);
  CHECK(n == 5 + 8 && got[0].code == 0x2f && got[0].value == 0 && got[1].code == 0x39 && got[1].value == -1 && got[2].code == 0x14a &&
        got[2].value == 0 && got[3].type == 4 && got[4].type == 0);
  CHECK(got[5].code == 0x2f && got[5].value == 1 && got[6].code == 0x39 && got[6].value == 9 && got[7].value == 100 && got[8].value == 200 &&
        got[9].code == 0x2f && got[9].value == 1 && got[10].code == 0x14a && got[10].value == 1 && got[11].type == 4 && got[12].type == 0);
  CHECK(s.drops == 1 && !s.dropping && s.sent_id[0] == -1 && s.sent_id[1] == 9 && s.timing.frames == 1 && s.timing.began == 1);
  // The lift: the gesture is logged and the counts start again; a frame with nothing down is no gesture.
  const xh_input_ev lift[] = { { now + 30000000, 3, 0x39, -1 }, { now + 30000000, 1, 0x14a, 0 }, { now + 30000000, 0, 0, 0 },
                               { now + 40000000, 1, 0x14a, 0 }, { now + 40000000, 0, 0, 0 } };
  xh_input_feed(&s, lift, 5, now + 41000000, counter, hz);
  n = input_take(&d, got, 32);
  CHECK(n == 4 + 3 && got[0].code == 0x39 && got[0].value == -1 && s.timing.frames == 0 && s.sent_id[1] == -1);
  // A touch-down on a still screen (no guest frame for 120 ms) is timed to the next frame shown.
  xh_input_frame_shown();
  usleep(120000);
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  const int64_t real = (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
  const xh_input_ev tap[] = { { real, 3, 0x39, 11 }, { real, 3, 0x35, 5 }, { real, 0, 0, 0 } };
  xh_input_feed(&s, tap, 3, real, counter, hz);
  CHECK(s.timing.still && s.timing.frames == 1);
  xh_input_frame_shown();
  const xh_input_ev up[] = { { real + 1, 3, 0x39, -1 }, { real + 1, 0, 0, 0 } };
  xh_input_feed(&s, up, 2, real + 2, counter, hz);
  CHECK(!s.timing.frames);
  input_take(&d, got, 32);
  // A key device: the side keys pass, other keys and touch codes do not; after a drop the keys are read again.
  xh_input_source k;
  xh_input_source_init(&k, d.dev, "keys", false);
  k.read_state = fake_read_state;
  const xh_input_ev press[] = { { now, 1, 116, 1 }, { now, 1, 30, 1 }, { now, 3, 0x35, 3 }, { now, 1, 172, 1 }, { now, 0, 0, 0 } };
  xh_input_feed(&k, press, 5, now, counter, hz);
  n = input_take(&d, got, 32);
  CHECK(n == 3 && got[0].code == 116 && got[0].value == 1 && got[1].code == 172 && got[2].type == 0 && k.sent_keys == 4);
  fake_state.keys = 2;                                                          // power up, volume up down: both changed
  const xh_input_ev kdrop[] = { { now, 0, 3, 0 }, { now, 1, 116, 0 }, { now, 0, 0, 0 } };
  xh_input_feed(&k, kdrop, 3, now, counter, hz);
  n = input_take(&d, got, 32);
  CHECK(n == 3 && got[0].code == 115 && got[0].value == 1 && got[1].code == 116 && got[1].value == 0 && got[2].type == 0 && k.sent_keys == 2);
}

// The scripted event source (input_script.c): the file's lines as steps.
static void test_input_script(void)
{
  xh_input_step *s = NULL;
  unsigned n = 0, bad = 0;
  CHECK(!xh_input_script_parse("# a panel\nabs 0x35 0 1439 10\n\nabs 0x36 0 3119\nready\nwait 250\ntap 700 1500 40  # a tap\nkey 116\nrepeat 2\n", &s, &n, &bad));
  // 2 axes, ready, wait, tap (6 events, wait, 3 events), key (2 events, wait, 2 events), repeat
  CHECK(n == 2 + 1 + 1 + 10 + 5 + 1 && s[0].kind == XH_STEP_ABS && s[0].code == 0x35 && s[0].max == 1439 && s[0].value == 10 && s[1].value == 0);
  CHECK(s[2].kind == XH_STEP_READY && s[3].kind == XH_STEP_WAIT && s[3].value == 250);
  CHECK(s[4].type == 3 && s[4].code == 0x2f && s[5].code == 0x39 && s[5].value == 1 && s[6].code == 0x35 && s[6].value == 700 &&
        s[7].code == 0x36 && s[7].value == 1500 && s[8].type == 1 && s[8].code == 0x14a && s[8].value == 1 && s[9].type == 0);
  CHECK(s[10].kind == XH_STEP_WAIT && s[10].value == 40 && s[11].code == 0x39 && s[11].value == -1 && s[12].value == 0 && s[13].type == 0);
  CHECK(s[14].type == 1 && s[14].code == 116 && s[14].value == 1 && s[16].kind == XH_STEP_WAIT && s[16].value == 100 && s[17].value == 0);
  CHECK(s[19].kind == XH_STEP_REPEAT && s[19].value == 2);
  free(s);
  CHECK(!xh_input_script_parse("swipe 100 3000 100 1000 160\nrepeat\n", &s, &n, &bad) && n == 6 + 10 * 4 + 3 + 1 && s[n - 1].value == -1 &&
        s[6].kind == XH_STEP_WAIT && s[6].value == 16 && s[8].code == 0x36 && s[8].value == 2800 && s[44].value == 1000);
  free(s);
  CHECK(xh_input_script_parse("wait 5\n\nev 3 0x35\n", &s, &n, &bad) == -1 && bad == 3);
  CHECK(xh_input_script_parse("abs 0x40 0 10\n", &s, &n, &bad) == -1 && bad == 1);
  CHECK(xh_input_script_parse("tap 1 2 x\n", &s, &n, &bad) == -1);
  driver d = { .dev = xh_virtio_input_create(0, 44) };
  CHECK(d.dev && !xh_virtio_ready(d.dev) && driver_init(&d, 0, 2, GUEST_PA + 0x10000) && xh_virtio_ready(d.dev));
}

// ---- the seccomp filter, run as the kernel would ------------------------------------------------
static uint32_t bpf_run(const struct xs_filter_insn *f, unsigned n, uint32_t nr, uint64_t ip, const uint64_t args[6], uint32_t arch)
{
  struct { uint32_t nr, arch; uint64_t ip, args[6]; } data = { nr, arch, ip, { args[0], args[1], args[2], args[3], args[4], args[5] } };
  uint32_t a = 0;
  for (unsigned pc = 0; pc < n; pc++) {
    switch (f[pc].code) {
    case 0x20: memcpy(&a, (uint8_t *)&data + f[pc].k, 4); break;
    case 0x15: pc += a == f[pc].k ? f[pc].jt : f[pc].jf; break;
    case 0x35: pc += a >= f[pc].k ? f[pc].jt : f[pc].jf; break;
    case 0x06: return f[pc].k;
    default: return 0xdead;
    }
  }
  return 0xdead;
}

static void test_filter(void)
{
  struct xs_filter_insn f[XS_FILTER_MAX];
  const uint64_t lo = 0x7e00000000ull, hi = lo + 3000, none[6] = { 0 };
  unsigned n = xh_task_filter(f, lo, hi, 5, 6);
  CHECK(n > 0 && n <= XS_FILTER_MAX);
  enum { TRAP = 0x00030000, ALLOW = 0x7fff0000, KILL = (int)0x80000000 };
  const uint32_t arm = 0xc00000b7;
  // iOS code: every system call traps, whatever Linux number x8 happens to hold
  CHECK(bpf_run(f, n, 98, 0x100004000ull, none, arm) == TRAP);
  CHECK(bpf_run(f, n, 222, 0x180001234ull, none, arm) == TRAP);
  CHECK(bpf_run(f, n, 93, lo - 4, none, arm) == TRAP);
  CHECK(bpf_run(f, n, 93, hi, none, arm) == TRAP);
  CHECK(bpf_run(f, n, 93, 0x7f00000010ull, none, arm) == TRAP);                 // same low word, other high word
  // the stub: its own calls pass
  CHECK(bpf_run(f, n, 98, lo + 0x100, none, arm) == (uint32_t)ALLOW);           // futex
  CHECK(bpf_run(f, n, 139, lo + 0x10, none, arm) == (uint32_t)ALLOW);           // rt_sigreturn
  CHECK(bpf_run(f, n, 220, hi - 4, none, arm) == (uint32_t)ALLOW);              // clone
  CHECK(bpf_run(f, n, 226, lo + 8, none, arm) == (uint32_t)ALLOW);              // mprotect
  // ... but nothing else from there (a jump into the stub's svc with a chosen x8)
  CHECK(bpf_run(f, n, 221, lo + 0x100, none, arm) == TRAP);                     // execve
  CHECK(bpf_run(f, n, 56, lo + 0x100, none, arm) == TRAP);                      // openat
  CHECK(bpf_run(f, n, 129, lo + 0x100, none, arm) == TRAP);                     // kill
  // mmap from the stub: only the two memfds or anonymous memory
  CHECK(bpf_run(f, n, 222, lo + 0x100, (const uint64_t[6]){ 0, 0, 0, 0, 5, 0 }, arm) == (uint32_t)ALLOW);
  CHECK(bpf_run(f, n, 222, lo + 0x100, (const uint64_t[6]){ 0, 0, 0, 0, 6, 0 }, arm) == (uint32_t)ALLOW);
  CHECK(bpf_run(f, n, 222, lo + 0x100, (const uint64_t[6]){ 0, 0, 0, 0, 0xffffffffffffffffull, 0 }, arm) == (uint32_t)ALLOW);
  CHECK(bpf_run(f, n, 222, lo + 0x100, (const uint64_t[6]){ 0, 0, 0, 0, 7, 0 }, arm) == TRAP);
  CHECK(bpf_run(f, n, 98, lo + 0x100, none, 0xc000003e) == (uint32_t)KILL);     // another architecture's call
  CHECK(xh_task_filter(f, 0xfffff000ull, 0x100001000ull, 5, 6) == 0);           // text across a 4 GiB line
}

// ---- configuration ------------------------------------------------------------------------------
static void test_config(const char *dir)
{
  xh_config c;
  bool ok;
  CHECK(xh_parse_size("4G", &ok) == 4ull << 30 && ok);
  CHECK(xh_parse_size("512m", &ok) == 512u << 20 && ok);
  CHECK(xh_parse_size("0x1000", &ok) == 4096 && ok);
  xh_parse_size("12q", &ok);
  CHECK(!ok);
  char image[256], path[300];
  snprintf(image, sizeof image, "%s/image", dir);
  mkdir(image, 0700);
  static const char *const files[] = { "kernel.kc", "os.img", "systemos.img", "data.seed" };
  for (unsigned i = 0; i < 4; i++) { snprintf(path, sizeof path, "%s/%s", image, files[i]); FILE *f = fopen(path, "w"); fputs("x", f); fclose(f); }
  // the OneOS fork's command line (oneos/interface.md, section 4)
  char *argv[] = { "xnuhost", "--session-abi", "1", "--mode", "takeover", "--runtime", "/data/runtime", "--image", image, "--data", (char *)dir, "--session", (char *)dir };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 13, argv) == 0 && xh_config_finish(&c) == 0);
  CHECK(strstr(c.kernel, "/image/kernel.kc") && c.ndisk == 4 && strstr(c.disk[3].path, "data.seed") && !c.disk[3].writable && !c.disk[0].writable && strstr(c.disk[1].path, "systemos.img"));
  CHECK(c.disk[2].data_store && c.disk[2].writable && strstr(c.disk[2].path, "/data.img") && strstr(c.disk[2].seed, "data.seed"));
  CHECK(c.ncpu >= 1 && c.ncpu <= XH_MAX_CPUS && !strcmp(c.display, "shm") && c.input && c.tasks);
  CHECK(c.bridges == 1 && !strcmp(c.bluetooth, "auto") && !strcmp(c.wifi, "auto"));                       // on in a session; the phone's HAL when it is there
  CHECK(c.touch_sleep && xh_config_set(&c, "touch-sleep", "0") == 0 && !c.touch_sleep && xh_config_set(&c, "touch-sleep", "maybe") != 0);
  c.touch_sleep = true;
  // the image set's own settings, and what the command line overrides
  snprintf(path, sizeof path, "%s/xnuhost.conf", image);
  FILE *f = fopen(path, "w");
  fputs("# settings of this image\nboot-args=-v debug=0x8\n", f);
  fclose(f);
  char *argv2[] = { "xnuhost", "--image", image, "--cpus", "6", "--ram", "6G", "--set", "display=none", "--no-input" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 10, argv2) == 0 && xh_config_finish(&c) == 0);
  CHECK(!strcmp(c.boot_args, "-v debug=0x8") && c.ncpu == 6 && c.ram_bytes == 6ull << 30 && !strcmp(c.display, "none") && !c.input && c.ndisk == 3);
  // ram=auto (docs/booter.md 1): min(90% of MemTotal, MemTotal - 1.5 GiB) in 256 MiB steps, at least 1 GiB, at most 64 GiB.
  // 11,746,000 kB stands for a 12 GB phone's MemTotal (about 11.2 GiB; the phone's own number is read at the start).
  const uint64_t G = 1ull << 30, M = 1ull << 20, phone = 11746000ull << 10;
  CHECK(xh_ram_auto(phone, 90, XH_RAM_AUTO_FLOOR) == 9728 * M);               // the floor decides: 9.5 GiB
  CHECK(xh_ram_auto(phone, 50, XH_RAM_AUTO_FLOOR) == 5632 * M);               // the share decides
  CHECK(xh_ram_auto(phone, 90, 4 * G) == 7168 * M);
  CHECK(xh_ram_auto(64 * G, 90, XH_RAM_AUTO_FLOOR) == 58880 * M && xh_ram_auto(128 * G, 90, XH_RAM_AUTO_FLOOR) == XH_RAM_MAX);
  CHECK(xh_ram_auto(2560 * M, 90, XH_RAM_AUTO_FLOOR) == G && xh_ram_auto(2 * G, 90, XH_RAM_AUTO_FLOOR) == 0 && xh_ram_auto(G, 100, 0) == G);
  CHECK(xh_ram_auto(XH_RAM_AUTO_FLOOR, 90, XH_RAM_AUTO_FLOOR) == 0 && xh_ram_auto(0, 90, 0) == 0);
  char meminfo[300], small[300], partial[300];
  snprintf(meminfo, sizeof meminfo, "%s/meminfo", dir);
  snprintf(small, sizeof small, "%s/meminfo-small", dir);
  snprintf(partial, sizeof partial, "%s/meminfo-partial", dir);
  f = fopen(meminfo, "w"); fputs("MemTotal:       11746000 kB\nMemFree:          401234 kB\nMemAvailable:    7340032 kB\nBuffers:            1234 kB\n", f); fclose(f);
  f = fopen(small, "w"); fputs("MemTotal:        2097152 kB\nMemAvailable:    1048576 kB\n", f); fclose(f);
  f = fopen(partial, "w"); fputs("MemTotal:        2097152 kB\nMemFree:          1048576 kB\n", f); fclose(f);
  uint64_t total = 0, available = 0;
  CHECK(xh_meminfo(meminfo, &total, &available) == 0 && total == phone && available == 7ull * G);
  CHECK(xh_meminfo(partial, &total, &available) == -1 && xh_meminfo("/nonexistent/meminfo", &total, &available) == -1);
  char set_meminfo[340], set_small[340], set_none[340];
  snprintf(set_meminfo, sizeof set_meminfo, "ram-meminfo=%s", meminfo);
  snprintf(set_small, sizeof set_small, "ram-meminfo=%s", small);
  snprintf(set_none, sizeof set_none, "ram-meminfo=%s/none", dir);
  char *ram1[] = { "xnuhost", "--image", image, "--set", set_meminfo };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, ram1) == 0 && xh_config_finish(&c) == 0 && c.ram_bytes == 9728 * M && c.ram_auto_total == phone &&
        c.ram_auto_available == 7ull * G && c.ram_percent == 90 && c.ram_host_floor == (int64_t)XH_RAM_AUTO_FLOOR && c.host_mem_log == 30);
  char *ram2[] = { "xnuhost", "--image", image, "--set", set_meminfo, "--set", "ram-percent=50", "--set", "host-mem-log=0" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 9, ram2) == 0 && xh_config_finish(&c) == 0 && c.ram_bytes == 5632 * M && c.host_mem_log == 0);
  char *ram3[] = { "xnuhost", "--image", image, "--set", set_meminfo, "--set", "ram-host-floor=4G" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 7, ram3) == 0 && xh_config_finish(&c) == 0 && c.ram_bytes == 7168 * M);
  char *ram4[] = { "xnuhost", "--image", image, "--set", set_small };          // a host too small for auto: say so, start nothing
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, ram4) == 0 && xh_config_finish(&c) == -1);
  char *ram5[] = { "xnuhost", "--image", image, "--set", set_none };           // no meminfo: the old 4 GiB
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, ram5) == 0 && xh_config_finish(&c) == 0 && c.ram_bytes == 4 * G && c.ram_auto_total == 0);
  char *ram6[] = { "xnuhost", "--image", image, "--set", set_small, "--ram", "1536M" };   // a size given: no auto
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 7, ram6) == 0 && xh_config_finish(&c) == 0 && c.ram_bytes == 1536 * M && c.ram_auto_total == 0);
  xh_config_defaults(&c);
  CHECK(xh_config_set(&c, "ram-percent", "5") != 0 && xh_config_set(&c, "ram-percent", "101") != 0 && xh_config_set(&c, "ram", "0") != 0 &&
        xh_config_set(&c, "ram", "lots") != 0 && xh_config_set(&c, "ram-host-floor", "x") != 0 && xh_config_set(&c, "host-mem-log", "-1") != 0);
  char *ram7[] = { "xnuhost", "--image", image, "--ram", "65G" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, ram7) == 0 && xh_config_finish(&c) == -1);
  // The image set's xnuhost.conf: its ram= and ram-percent= apply when the command line has none; --ram auto wins over it.
  f = fopen(path, "w");
  fprintf(f, "boot-args=-v debug=0x8\nram=3G\nram-percent=80\n%s\n", set_meminfo);
  fclose(f);
  char *ram8[] = { "xnuhost", "--image", image };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 3, ram8) == 0 && xh_config_finish(&c) == 0 && c.ram_bytes == 3 * G && c.ram_percent == 80);
  char *ram9[] = { "xnuhost", "--image", image, "--ram", "auto" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, ram9) == 0 && xh_config_finish(&c) == 0 && c.ram_bytes == 8960 * M && c.ram_auto_total == phone);   // 80% of 11.2 GiB
  f = fopen(path, "w");
  fputs("# settings of this image\nboot-args=-v debug=0x8\n", f);
  fclose(f);
  char *argv3[] = { "xnuhost", "--image", image, "--mode", "bench", "--bluetooth", "hidl" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 7, argv3) == 0 && xh_config_finish(&c) == 0 && c.bridges == 0 && !strcmp(c.bluetooth, "hidl"));
  char *argv4[] = { "xnuhost", "--image", image, "--set", "bluetooth=virtual" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, argv4) == 0 && xh_config_finish(&c) == 0 && !strcmp(c.bluetooth, "virtual"));
  char *argv4b[] = { "xnuhost", "--image", image, "--mode", "bench", "--set", "bluetooth=sim" };   // the PC's stand-in
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 7, argv4b) == 0 && xh_config_finish(&c) == 0 && c.bridges == 0 && !strcmp(c.bluetooth, "sim"));
  char *argv5[] = { "xnuhost", "--image", image, "--set", "wifi=off" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, argv5) == 0 && xh_config_finish(&c) == 0 && !strcmp(c.wifi, "off"));
  char *argv6[] = { "xnuhost", "--image", image, "--set", "wifi=sim" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, argv6) == 0 && xh_config_finish(&c) == 0 && !strcmp(c.wifi, "sim"));
  char *bad_wifi[] = { "xnuhost", "--image", image, "--set", "wifi=nat" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, bad_wifi) == -1);
  char *bad0[] = { "xnuhost", "--image", image, "--bluetooth", "usb" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, bad0) == -1);
  char *bad1[] = { "xnuhost", "--image", image, "--session-abi", "2" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, bad1) == 0 && xh_config_finish(&c) == -1);
  char *bad2[] = { "xnuhost", "--image", image, "--cpus", "9" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, bad2) == -1);
  // The display's mode: the panel's native (largest) mode, not the one One UI runs; overrides; the defaults.
  static const xh_panel_mode modes[] = { { 1080, 2340, 120 }, { 1440, 3120, 60 }, { 1440, 3120, 120 }, { 720, 1560, 120 }, { 1440, 3120, 24 }, { 0, 0, 120 } };
  xh_panel_mode native = { 0, 0, 0 }, mode;
  CHECK(xh_panel_native(modes, 6, &native) && native.width == 1440 && native.height == 3120 && native.hz == 120);
  CHECK(xh_panel_native(modes, 1, &native) && native.width == 1080 && native.height == 2340);       // an FHD+ panel is its own native
  CHECK(!xh_panel_native(modes + 5, 1, &native) && !xh_panel_native(modes, 0, &native));
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 7, argv3) == 0 && xh_config_finish(&c) == 0 && c.scale == 3 && c.hz == 0 && c.scale_fixup == 1);
  mode = xh_display_mode(&c, NULL);
  CHECK(mode.width == 1440 && mode.height == 3120 && mode.hz == 120);                                // nothing readable: the S24 Ultra's
  native = (xh_panel_mode){ 1260, 2800, 144 };
  mode = xh_display_mode(&c, &native);
  CHECK(mode.width == 1260 && mode.height == 2800 && mode.hz == 144);                                // another panel: its own
  native.hz = 0;
  CHECK(xh_display_mode(&c, &native).hz == 120);                                                     // a mode without a rate
  char *mode1[] = { "xnuhost", "--image", image, "--width", "1320", "--height", "2868", "--hz", "60", "--scale", "2", "--set", "scale-fixup=0" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 13, mode1) == 0 && xh_config_finish(&c) == 0 && c.scale == 2 && c.hz == 60 && c.scale_fixup == 0);
  native = (xh_panel_mode){ 1440, 3120, 120 };
  mode = xh_display_mode(&c, &native);
  CHECK(mode.width == 1320 && mode.height == 2868 && mode.hz == 60);
  // the connector of the panel: not the Virtual (writeback, 5120x2560) ones of the S24 Ultra, not DisplayPort
  CHECK(xh_panel_connector(16) && xh_panel_connector(14) && !xh_panel_connector(15) && !xh_panel_connector(10) && !xh_panel_connector(11));
  // the helper's report of the panel, and the mode it gives a guest with nothing configured
  struct { char magic[4]; uint32_t a, b, c, d; } report = { { 'X', 'H', 'D', 'P' }, 1440, 3120, 120, 0 };
  native = (xh_panel_mode){ 0, 0, 0 };
  CHECK(xh_panel_report(&report, sizeof report, &native) && native.width == 1440 && native.height == 3120 && native.hz == 120);
  CHECK(!xh_panel_report(&report, sizeof report - 1, &native));
  report.a = 0;
  CHECK(!xh_panel_report(&report, sizeof report, &native));
  report.a = 16385;
  CHECK(!xh_panel_report(&report, sizeof report, &native));
  report.a = 1440; report.magic[3] = 'Q';
  CHECK(!xh_panel_report(&report, sizeof report, &native));
  // the display driver's switches on the kernel's command line: added once, a given value stays
  char args[128] = "-v debug=0x8";
  CHECK(xh_display_boot_args(args, sizeof args, 3, 120, XH_PIXELS_BGRX8) == 0 && !strcmp(args, "-v debug=0x8 offline-legacy-unit-scale=3 sheng-vsync=120"));
  CHECK(xh_display_boot_args(args, sizeof args, 2, 60, XH_PIXELS_BGRX8) == 0 && !strcmp(args, "-v debug=0x8 offline-legacy-unit-scale=3 sheng-vsync=120"));
  strcpy(args, "sheng-vsync=90 not-sheng-vsync=1");
  CHECK(xh_display_boot_args(args, sizeof args, 2, 120, XH_PIXELS_BGRX8) == 0 && !strcmp(args, "sheng-vsync=90 not-sheng-vsync=1 offline-legacy-unit-scale=2"));
  args[0] = 0;
  CHECK(xh_display_boot_args(args, sizeof args, 2, 120, XH_PIXELS_BGRX8) == 0 && !strcmp(args, "offline-legacy-unit-scale=2 sheng-vsync=120"));
  CHECK(xh_display_boot_args(args, 44, 2, 120, XH_PIXELS_BGRX8) == 0);
  args[0] = 0;
  CHECK(xh_display_boot_args(args, 40, 2, 120, XH_PIXELS_BGRX8) == -1);                                               // no room
  // a half-float frame: the guest is told to hand its half-float surfaces over as they are; a given value stays
  args[0] = 0;
  CHECK(xh_display_boot_args(args, sizeof args, 3, 120, XH_PIXELS_RGBA16F) == 0 && !strcmp(args, "oneios-display-wide=1 offline-legacy-unit-scale=3 sheng-vsync=120"));
  strcpy(args, "oneios-display-wide=0");
  CHECK(xh_display_boot_args(args, sizeof args, 3, 120, XH_PIXELS_RGBA16F) == 0 && !strcmp(args, "oneios-display-wide=0 offline-legacy-unit-scale=3 sheng-vsync=120"));
  // display-format: the two names, nothing else; half floats need the shm back end
  {
    xh_config f;
    xh_config_defaults(&f);
    CHECK(xh_config_set(&f, "display-format", "rgba16f") == 0 && f.display_format == XH_PIXELS_RGBA16F);
    CHECK(xh_config_set(&f, "display-format", "bgrx") == 0 && f.display_format == XH_PIXELS_BGRX8);
    CHECK(xh_config_set(&f, "display-format", "rgb565") == -1);
  }
  // the scale in the image set (the userland hook's file), under the command line's
  char overlay[400];
  snprintf(overlay, sizeof overlay, "%s/overlay", image); mkdir(overlay, 0700);
  snprintf(overlay, sizeof overlay, "%s/overlay/userland", image); mkdir(overlay, 0700);
  snprintf(overlay, sizeof overlay, "%s/overlay/userland/xnuhost.conf", image);
  f = fopen(overlay, "w");
  fputs("scale=2\nwidth=1440\nheight=3120\nhz=120\n", f);
  fclose(f);
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 7, argv3) == 0 && xh_config_finish(&c) == 0 && c.scale == 2);
  CHECK(c.width == 1440 && c.height == 3120 && c.hz == 120);                                         // the image's mode, over any panel
  native = (xh_panel_mode){ 5120, 2560, 60 };
  mode = xh_display_mode(&c, &native);
  CHECK(mode.width == 1440 && mode.height == 3120 && mode.hz == 120);
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 13, mode1) == 0 && xh_config_finish(&c) == 0 && c.width == 1320 && c.hz == 60); // the command line's wins
  char *scale3[] = { "xnuhost", "--image", image, "--set", "scale=3", "--text-fixup", "file.01,10,aa,bb" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 7, scale3) == 0 && xh_config_finish(&c) == 0 && c.scale == 3 && c.text_fixups_length == 17);
  // the fixups of QuartzCore's preferred scale: for @3x on the build they were read in, after the configured ones
  unsigned length = xh_scale_fixups("24A446", 3, c.text_fixups, c.text_fixups_length, sizeof c.text_fixups);
  CHECK(length == 17 + 54 + 70 && !strcmp(c.text_fixups, "file.01,10,aa,bb") && !strcmp(c.text_fixups + 17, "dyld_shared_cache_arm64e.01,40b17a8,49008052,69008052"));
  CHECK(strstr(c.text_fixups + 17 + 54, ",40b17bc,280080520895889a,680080521f2003d5") && c.text_fixups[length - 1] == 0);
  CHECK(xh_scale_fixups("24A446", 2, c.text_fixups, 17, sizeof c.text_fixups) == 17);                // @2x is QuartzCore's own answer
  CHECK(xh_scale_fixups("24A447", 3, c.text_fixups, 17, sizeof c.text_fixups) == 17 && xh_scale_fixups(NULL, 3, c.text_fixups, 17, sizeof c.text_fixups) == 17);
  char *badscale[] = { "xnuhost", "--image", image, "--scale", "4" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, badscale) == -1);
  char *badhz[] = { "xnuhost", "--image", image, "--hz", "500" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, badhz) == -1);
  char *badfix[] = { "xnuhost", "--image", image, "--text-fixup", "file,10,aa" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, badfix) == -1);
  remove(overlay);
  char *bad3[] = { "xnuhost", "--image", (char *)dir };                         // no kernel there
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 3, bad3) == 0 && xh_config_finish(&c) == -1);
}

// perf.c: the histograms of the trap path's steps, and the keys that tune the path.
static void test_perf(void)
{
  static xh_hist h;
  memset(&h, 0, sizeof h);
  CHECK(xh_hist_quantile(&h, 0.5) == 0);
  for (uint64_t ns = 1; ns <= 1000; ns++) xh_hist_add(&h, ns * 1000);       // 1 us .. 1 ms, evenly
  CHECK(h.n == 1000 && h.max_ns == 1000000 && h.sum_ns == 500500000ull);
  uint64_t p50 = xh_hist_quantile(&h, 0.5), p99 = xh_hist_quantile(&h, 0.99);
  CHECK(p50 >= 500000 && p50 <= 500000 * 5 / 4);                            // a bucket is a quarter of a power of two
  CHECK(p99 >= 990000 && p99 <= 1000000);                                    // never above the maximum
  for (uint64_t ns = 0; ns < 70000; ns += 7) {                               // every value lands at or below its bucket's top
    memset(&h, 0, sizeof h);
    xh_hist_add(&h, ns);
    if (xh_hist_quantile(&h, 0) != ns) { CHECK(xh_hist_quantile(&h, 0) == ns); break; }
  }
  memset(&h, 0, sizeof h);
  xh_hist_add(&h, ~0ull >> 1);                                               // past 2^32 ns: the last bucket
  CHECK(h.b[XH_HIST_BUCKETS - 1] == 1);
  xh_config c;
  xh_config_defaults(&c);
  CHECK(c.perf_log == -1 && c.spin_host == -1 && c.spin_task == -1 && c.map_defer == -1 && c.cpu_pin == -1);
  CHECK(xh_config_set(&c, "perf-log", "10") == 0 && c.perf_log == 10);
  CHECK(xh_config_set(&c, "perf-log", "ten") == -1 && xh_config_set(&c, "perf-log", "-1") == -1);
  CHECK(xh_config_set(&c, "spin-host", "20") == 0 && c.spin_host == 20 && xh_config_set(&c, "spin-task", "5000") == -1);
  CHECK(xh_config_set(&c, "map-defer", "0") == 0 && c.map_defer == 0);
  CHECK(xh_config_set(&c, "cpu-pin", "0") == 0 && c.cpu_pin == 0 && xh_config_set(&c, "cpu-pin", "2") == 0 && c.cpu_pin == 2);
  CHECK(xh_config_set(&c, "cpu-pin", "3") == -1);
}

// ---- free pages given back (discard.c, ram_discard; the kernel's free-page reporting, hosted 0058) ----------
static uint64_t memfd_held(int fd) { struct stat st; return fstat(fd, &st) == 0 ? (uint64_t)st.st_blocks * 512 : ~0ull; }

static void test_discard(void)
{
  // A RAM of 64 pages of 16 KiB at physical 0x40000000, as a memfd with xnuhost's shrink seal, mapped twice: the
  // window and "a task" (MAP_SHARED, as task_map maps it).
  const uint64_t page = 4096, gp = 16384, base = 0x40000000ull, size = 64 * gp;
  int fd = (int)syscall(SYS_memfd_create, "xnuhost-test-ram", 2u /* MFD_ALLOW_SEALING */);
  CHECK(fd >= 0 && ftruncate(fd, (off_t)size) == 0 && fcntl(fd, 1033 /* F_ADD_SEALS */, 2 /* F_SEAL_SHRINK */) == 0);
  uint8_t *window = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
  uint8_t *task = mmap(NULL, size, PROT_READ, MAP_SHARED, fd, 0);
  CHECK(window != MAP_FAILED && task != MAP_FAILED);
  if (fd < 0 || window == MAP_FAILED || task == MAP_FAILED) return;
  for (uint64_t i = 0; i < size / 8; i++) ((uint64_t *)window)[i] = i | 1;
  CHECK(memfd_held(fd) == size);
  memset(&g_discard, 0, sizeof g_discard);
  // Pages 8..15 given back: the memfd holds 8 pages less, both mappings read zero there and the rest is kept.
  CHECK(xh_ram_discard_fd(fd, base, base, size, base + 8 * gp, 8 * gp, page) == 0);
  CHECK(memfd_held(fd) == size - 8 * gp);
  bool zero = true, kept = true;
  for (uint64_t i = 0; i < size / 8; i++) {
    uint64_t at = i * 8;
    bool in = at >= 8 * gp && at < 16 * gp;
    if (in) zero = zero && ((uint64_t *)window)[i] == 0 && ((uint64_t *)task)[i] == 0;
    else kept = kept && ((uint64_t *)window)[i] == (i | 1) && ((uint64_t *)task)[i] == (i | 1);
  }
  CHECK(zero && kept);
  // A read of a hole through a shared mapping fills it (shmem has no shared zero page): the pages read above are held
  // again. Given back once more, then used again: a write makes one host page held, and the task's mapping sees it.
  CHECK(memfd_held(fd) == size);
  CHECK(xh_ram_discard_fd(fd, base, base, size, base + 8 * gp, 8 * gp, page) == 0 && memfd_held(fd) == size - 8 * gp);
  window[9 * gp] = 0x5a;
  CHECK(task[9 * gp] == 0x5a && memfd_held(fd) == size - 8 * gp + page);
  // A page that is a hole already: nothing more to give, the call succeeds.
  CHECK(xh_ram_discard_fd(fd, base, base, size, base + 10 * gp, gp, page) == 0 && memfd_held(fd) == size - 8 * gp + page);
  // Refused, nothing changed: unaligned, empty, outside the window, past its end, a window that starts below the memfd.
  CHECK(xh_ram_discard_fd(fd, base, base, size, base + 100, gp, page) == -EINVAL);
  CHECK(xh_ram_discard_fd(fd, base, base, size, base, 100, page) == -EINVAL);
  CHECK(xh_ram_discard_fd(fd, base, base, size, base, 0, page) == -EINVAL);
  CHECK(xh_ram_discard_fd(fd, base, base, size, base - gp, gp, page) == -EINVAL);
  CHECK(xh_ram_discard_fd(fd, base, base, size, base + 63 * gp, 2 * gp, page) == -EINVAL);
  CHECK(xh_ram_discard_fd(fd, base, base + gp, size - gp, base, gp, page) == -EINVAL);    // below the window's start
  CHECK(xh_ram_discard_fd(fd, base, base, size - gp, base + 63 * gp, gp, page) == -EINVAL); // above it (the framebuffer)
  CHECK(xh_ram_discard_fd(fd, base + gp, base, size, base + gp, gp, page) == -EINVAL);      // window below the memfd
  CHECK(((uint64_t *)window)[0] == 1 && ((uint64_t *)window)[63 * gp / 8] == ((63 * gp / 8) | 1));
  CHECK(g_discard.calls == 3 && g_discard.bytes == 17 * gp && g_discard.failed == 0 && g_discard.refused == 8);
  // fallocate's own failure is counted and returned (a closed descriptor).
  CHECK(xh_ram_discard_fd(1000, base, base, size, base, gp, page) < 0 && g_discard.failed == 1 && g_discard.calls == 4);
  munmap(window, size);
  munmap(task, size);
  close(fd);
}

static void test_free_report(void)
{
  // The batch and the hysteresis: a round starts above keep + hysteresis and runs down to keep.
  CHECK(xh_fr_batch(100, 100, 50, 64, false) == 0);
  CHECK(xh_fr_batch(150, 100, 50, 64, false) == 0);      // at keep + hysteresis: not yet
  CHECK(xh_fr_batch(151, 100, 50, 64, false) == 51);
  CHECK(xh_fr_batch(400, 100, 50, 64, false) == 64);     // at most a batch at once
  CHECK(xh_fr_batch(120, 100, 50, 64, true) == 20);      // in a round: on down to keep
  CHECK(xh_fr_batch(100, 100, 50, 64, true) == 0 && xh_fr_batch(99, 100, 50, 64, true) == 0);
  CHECK(xh_fr_batch(0, 0, 0, 64, false) == 0 && xh_fr_batch(1, 0, 0, 64, false) == 1);
  CHECK(xh_fr_batch(500, 100, 50, 0, false) == 0);
  CHECK(xh_fr_batch(0xffffffffu, 0xfffffff0u, 0x100u, 64, false) == 0);  // keep + hysteresis past 32 bits
  // A sequence of free counts: no round below the start, one round per rise, each down to keep exactly.
  uint32_t held = 0, given = 0, rounds = 0;
  bool active = false;
  const uint32_t frees[] = { 30, 30, 30, 200, 0, 10, 10, 300, 0, 0 };
  for (unsigned step = 0; step < sizeof frees / sizeof frees[0]; step++) {
    held += frees[step];
    uint32_t n;
    bool started = false;
    while ((n = xh_fr_batch(held, 100, 50, 64, active)) != 0) { if (!active) started = true; active = true; held -= n; given += n; }
    active = false;
    rounds += started;
    CHECK(held <= 150);
  }
  CHECK(rounds == 2 && held == 100 && given == 510);
  // Runs: sorted, consecutive pages merged, a duplicate given once.
  uint32_t pages[] = { 9, 3, 4, 5, 20, 8, 21, 5, 100 }, first[9], count[9];
  uint32_t runs = xh_fr_runs(pages, 9, first, count);
  CHECK(runs == 4 && first[0] == 3 && count[0] == 3 && first[1] == 8 && count[1] == 2 && first[2] == 20 && count[2] == 2 &&
        first[3] == 100 && count[3] == 1);
  uint32_t one[] = { 7 }, f1[1], c1[1];
  CHECK(xh_fr_runs(one, 1, f1, c1) == 1 && f1[0] == 7 && c1[0] == 1 && xh_fr_runs(one, 0, f1, c1) == 0);
  uint32_t many[256], fm[256], cm[256];
  for (uint32_t i = 0; i < 256; i++) many[i] = 1000 + (i * 37) % 256;    // every page of 1000..1255 once, shuffled
  CHECK(xh_fr_runs(many, 256, fm, cm) == 1 && fm[0] == 1000 && cm[0] == 256);
  CHECK(xh_fr_keep_per_queue(2048, 32) == 64 && xh_fr_keep_per_queue(2049, 32) == 65 && xh_fr_keep_per_queue(10, 0) == 10 &&
        xh_fr_keep_per_queue(1, 32) == 1);
  // The kernel's copy (kernel/patches/oneios-hosted/0058-free-page-report.patch adds osfmk/arm64/hosted/free_report.h)
  // is this header, line for line.
  FILE *patch = fopen("../../kernel/patches/oneios-hosted/0058-free-page-report.patch", "r"), *ours = fopen("include/xnuhost/free_report.h", "r");
  CHECK(patch && ours);
  if (patch && ours) {
    char line[512], mine[512];
    bool in = false, same = true;
    unsigned lines = 0;
    while (fgets(line, sizeof line, patch)) {
      if (!strncmp(line, "diff --git", 10)) { if (in) break; in = strstr(line, "b/osfmk/arm64/hosted/free_report.h\n") != NULL; continue; }
      if (!in || line[0] != '+' || !strncmp(line, "+++", 3)) continue;
      lines++;
      if (!fgets(mine, sizeof mine, ours) || strcmp(line + 1, mine)) { same = false; break; }
    }
    CHECK(same && lines > 20 && !fgets(mine, sizeof mine, ours));
  }
  if (patch) fclose(patch);
  if (ours) fclose(ours);
}

int main(void)
{
  char dir[] = "/tmp/xnuhost-test-XXXXXX";
  if (!mkdtemp(dir)) return 2;
  xh_log_open(NULL, XH_LOG_ERROR);
  xh_virtio_translate = xh_ram;
  test_insn();
  test_mmio();
  test_gic();
  test_sysreg();
  test_virtio_blk(dir);
  test_virtio_gpu();
  test_virtio_gpu3d();
  test_virtio_input();
  test_input_filter();
  test_input_script();
  test_filter();
  test_perf();
  test_discard();
  test_free_report();
  if (!freopen("/dev/null", "w", stderr)) failures++;   // the refusals below print their reasons
  test_config(dir);
  char command[300];
  snprintf(command, sizeof command, "rm -rf %s", dir);
  if (system(command)) failures++;
  printf("xnuhost tests: %d checks, %d failed\n", checks, failures);
  return failures != 0;
}
