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
#include "stub_abi.h"

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
  W(&d, 0x70, 0);                                                              // reset frees what is left
  free(fake_display.pixels);
  fake_display.pixels = NULL;
#undef SEND
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
  CHECK(c.ncpu == 4 && !strcmp(c.display, "shm") && c.input && c.tasks);
  CHECK(c.bridges == 1 && !strcmp(c.bluetooth, "virtual"));                    // on in a session
  // the image set's own settings, and what the command line overrides
  snprintf(path, sizeof path, "%s/xnuhost.conf", image);
  FILE *f = fopen(path, "w");
  fputs("# settings of this image\nboot-args=-v debug=0x8\n", f);
  fclose(f);
  char *argv2[] = { "xnuhost", "--image", image, "--cpus", "6", "--ram", "6G", "--set", "display=none", "--no-input" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 10, argv2) == 0 && xh_config_finish(&c) == 0);
  CHECK(!strcmp(c.boot_args, "-v debug=0x8") && c.ncpu == 6 && c.ram_bytes == 6ull << 30 && !strcmp(c.display, "none") && !c.input && c.ndisk == 3);
  char *argv3[] = { "xnuhost", "--image", image, "--mode", "bench", "--bluetooth", "hidl" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 7, argv3) == 0 && xh_config_finish(&c) == 0 && c.bridges == 0 && !strcmp(c.bluetooth, "hidl"));
  char *bad0[] = { "xnuhost", "--image", image, "--bluetooth", "usb" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, bad0) == -1);
  char *bad1[] = { "xnuhost", "--image", image, "--session-abi", "2" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, bad1) == 0 && xh_config_finish(&c) == -1);
  char *bad2[] = { "xnuhost", "--image", image, "--cpus", "9" };
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 5, bad2) == -1);
  char *bad3[] = { "xnuhost", "--image", (char *)dir };                         // no kernel there
  xh_config_defaults(&c);
  CHECK(xh_config_args(&c, 3, bad3) == 0 && xh_config_finish(&c) == -1);
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
  test_virtio_input();
  test_filter();
  freopen("/dev/null", "w", stderr);          // the refusals below print their reasons
  test_config(dir);
  char command[300];
  snprintf(command, sizeof command, "rm -rf %s", dir);
  if (system(command)) failures++;
  printf("xnuhost tests: %d checks, %d failed\n", checks, failures);
  return failures != 0;
}
