// SPDX-License-Identifier: MIT
// A stand-in for the hosted XNU: a freestanding AArch64 program that xnuhost loads and enters
// exactly as it will the kernel collection, and that exercises every part of the host-call ABI
// (xnuhost/hostcall.h) the way XNU's machine layer will. It prints one line per check and halts.
#include <stddef.h>
#include <stdint.h>
#include "xnuhost/hostcall.h"

#pragma GCC visibility push(hidden)
extern char __base[];

__asm__(".section .text.start,\"ax\"\n.globl _start\n_start:\n  b kernel_main\n");

struct boot_args {
  uint16_t revision, version; uint32_t pad0;
  uint64_t virt_base, phys_base, mem_size, top_of_kernel_data, video[6];
  uint32_t machine_type, pad1; uint64_t device_tree; uint32_t device_tree_length; char command_line[1024];
  uint32_t pad2; uint64_t boot_flags, mem_size_actual;
};

static const struct xh_vector *H;
static struct xh_cpu *C0;
static const struct boot_args *BA;
static int failures;
static uint64_t next_page;                      // physical bump allocator above topOfKernelData

static volatile uint32_t irq_count, last_intid, timer_fires, sgi_seen, rearm;
static volatile uint64_t secondary_seen, psci_seen;

static void put(const char *s) { size_t n = 0; while (s[n]) n++; H->console_write(C0, s, n); }
static void hex(uint64_t v)
{
  char b[19] = "0x";
  for (int i = 0; i < 16; i++) b[2 + i] = "0123456789abcdef"[(v >> (60 - 4 * i)) & 15];
  b[18] = 0;
  put(b);
}
static void check(int ok, const char *what)
{
  put(ok ? "PASS " : "FAIL ");
  put(what);
  put("\n");
  if (!ok) failures++;
}
static void *zero(void *p, size_t n) { volatile uint8_t *b = p; while (n--) *b++ = 0; return p; }
static int same(const void *a, const void *b, size_t n) { const uint8_t *x = a, *y = b; while (n--) if (*x++ != *y++) return 0; return 1; }

static uint64_t kva(uint64_t pa) { return pa - BA->phys_base + BA->virt_base; }
static uint64_t kpa(const void *va) { return (uint64_t)(uintptr_t)va - BA->virt_base + BA->phys_base; }
static uint64_t page_alloc(void) { uint64_t pa = next_page; next_page += 0x4000; zero((void *)(uintptr_t)kva(pa), 0x4000); return pa; }
static uint64_t now(void) { uint64_t v; __asm__ volatile("mrs %0, cntvct_el0" : "=r"(v)); return v; }

// ---- the device tree, as XNU reads it -----------------------------------------------------------
static const uint8_t *adt_prop(const uint8_t *node, const char *name, uint32_t *length, const uint8_t **end)
{
  uint32_t nprops = *(const uint32_t *)node, nchildren = *(const uint32_t *)(node + 4);
  const uint8_t *p = node + 8, *found = NULL;
  for (uint32_t i = 0; i < nprops; i++) {
    uint32_t len = *(const uint32_t *)(p + 32) & 0x7fffffff;
    size_t k = 0;
    while (name && name[k] && name[k] == (char)p[k]) k++;
    if (name && !name[k] && !p[k]) { found = p + 36; if (length) *length = len; }
    p += 36 + ((len + 3) & ~3u);
  }
  for (uint32_t i = 0; i < nchildren; i++) { const uint8_t *e; adt_prop(p, NULL, NULL, &e); p = e; }
  if (end) *end = p;
  return found;
}
static const uint8_t *adt_child(const uint8_t *node, const char *name)
{
  uint32_t nprops = *(const uint32_t *)node, nchildren = *(const uint32_t *)(node + 4);
  const uint8_t *p = node + 8;
  for (uint32_t i = 0; i < nprops; i++) p += 36 + (((*(const uint32_t *)(p + 32) & 0x7fffffff) + 3) & ~3u);
  for (uint32_t i = 0; i < nchildren; i++) {
    uint32_t len;
    const uint8_t *e, *n = adt_prop(p, "name", &len, &e);
    size_t k = 0;
    while (n && name[k] && name[k] == (char)n[k]) k++;
    if (n && !name[k] && !n[k]) return p;
    p = e;
  }
  return NULL;
}

// ---- interrupts ---------------------------------------------------------------------------------
static void irq_upcall(struct xh_cpu *cpu, struct xh_frame *frame)
{
  (void)frame;
  for (;;) {
    uint32_t id = H->irq_ack(cpu);
    if (id == XH_INTID_NONE) break;
    irq_count++;
    last_intid = id;
    if (id == XH_INTID_VTIMER) {
      timer_fires++;
      if (rearm) { rearm--; H->timer_set(cpu, now() + H->info->timebase_hz / 1000); }
      else H->timer_set(cpu, UINT64_MAX);
    } else if (id < XH_SGI_COUNT) sgi_seen |= 1u << id;
    H->irq_eoi(cpu, id);
  }
}

static void secondary_entry(struct xh_cpu *cpu, uint64_t arg)
{
  secondary_seen = arg + cpu->index;
  H->ipi(cpu, 0, 5);
  for (;;) H->cpu_idle(cpu);
}

__attribute__((noreturn)) void psci_entry(uint64_t context);
void psci_entry(uint64_t context)
{
  psci_seen = context;
  for (;;) __asm__ volatile("wfi");
}

static uint64_t work(uint64_t n, double *f)
{
  uint64_t a = 1, b = 2, c = 3, d = 5;
  double x = 1.0;
  for (uint64_t i = 1; i <= n; i++) {
    a += i * i ^ (b >> 3); b = b * 31 + a; c ^= a + (d << 1); d += c >> 5;
    x = x * 1.0000001 + (double)(i & 7);
  }
  *f = x;
  return a ^ b ^ c ^ d;
}

// ---- virtio block, the registers NativeVirtioBlock programs ---------------------------------------
static int blk_read(volatile uint32_t *r, uint64_t queue_pa, uint64_t sector, uint8_t *data /* kernel VA */, uint8_t *status)
{
  struct desc { uint64_t addr; uint32_t len; uint16_t flags, next; } *desc = (void *)(uintptr_t)kva(queue_pa);
  uint16_t *avail = (uint16_t *)(uintptr_t)kva(queue_pa + 0x1000), *used = (uint16_t *)(uintptr_t)kva(queue_pa + 0x2000);
  struct { uint32_t type, reserved; uint64_t sector; } *req = (void *)(uintptr_t)kva(queue_pa + 0x3000);
  req->type = 0; req->reserved = 0; req->sector = sector;
  desc[0] = (struct desc){ kpa(req), 16, 1, 1 };
  desc[1] = (struct desc){ kpa(data), 512, 3, 2 };
  desc[2] = (struct desc){ kpa(status), 1, 2, 0 };
  uint16_t before = used[1];
  avail[2 + avail[1] % 8] = 0;
  avail[1]++;
  r[0x50 / 4] = 0;                               // QueueNotify
  return used[1] == (uint16_t)(before + 1);
}

static void test_devices(void)
{
  // The console's PL011 through the trapped aperture.
  volatile uint32_t *uart = (volatile uint32_t *)(uintptr_t)H->io_map(C0, 0x6000, 0x1000);
  check(uart == NULL, "io_map refuses an address outside the aperture");
  volatile uint8_t *gicd = (volatile uint8_t *)(uintptr_t)H->io_map(C0, 0x08000000, 0x10000);
  check(gicd != NULL && (*(volatile uint32_t *)(gicd + 0xffe8) >> 4 & 15) == 3, "GICD_PIDR2 by a trapped load says GICv3");
  volatile uint32_t *r = (volatile uint32_t *)(uintptr_t)H->io_map(C0, 0x0a000000, 0x1000);
  check(r != NULL && r[0] == 0x74726976 && r[1] == 2 && r[2] == 2, "virtio-mmio block device at the first window (trapped loads)");
  check(H->mmio_read(C0, 0x0a000000, 4) == 0x74726976, "the same register by the mmio_read host call");
  // Feature negotiation and queue setup as NativeVirtioBlock::configureQueue does them.
  r[0x70 / 4] = 0; r[0x70 / 4] = 1; r[0x70 / 4] = 3;
  r[0x14 / 4] = 0; uint32_t low = r[0x10 / 4];
  r[0x14 / 4] = 1; uint32_t high = r[0x10 / 4];
  check((low & (1u << 5)) && (high & 1), "the image is offered read-only (F_RO) and VERSION_1");
  r[0x24 / 4] = 0; r[0x20 / 4] = 1u << 5; r[0x24 / 4] = 1; r[0x20 / 4] = 1;
  r[0x70 / 4] = 11;
  check(r[0x70 / 4] & 8, "FEATURES_OK accepted");
  r[0x30 / 4] = 0;
  uint64_t q = page_alloc();
  check(r[0x44 / 4] == 0 && r[0x34 / 4] >= 8, "queue 0 is free and large enough");
  r[0x38 / 4] = 8;
  r[0x80 / 4] = (uint32_t)q; r[0x84 / 4] = (uint32_t)(q >> 32);
  r[0x90 / 4] = (uint32_t)(q + 0x1000); r[0x94 / 4] = (uint32_t)((q + 0x1000) >> 32);
  r[0xa0 / 4] = (uint32_t)(q + 0x2000); r[0xa4 / 4] = (uint32_t)((q + 0x2000) >> 32);
  r[0x44 / 4] = 1;
  uint64_t capacity = r[0x100 / 4] | (uint64_t)r[0x104 / 4] << 32;
  check(capacity == 2048, "capacity from the configuration space");
  r[0x70 / 4] = 15;
  uint8_t *data = (uint8_t *)(uintptr_t)kva(page_alloc()), *status = data + 0x1000;
  *status = 0xff;
  int done = blk_read(r, q, 77, data, status);
  uint32_t n;
  for (int i = 0; i < 4; i++) ((uint8_t *)&n)[i] = data[8 + i];
  check(done && *status == 0 && same(data, "ONEIOSTD", 8) && n == 77 && data[100] == (77 ^ 0x5a), "sector 77 read through the virtqueue");
  check(r[0x60 / 4] & 1, "the device raised its interrupt status");
  r[0x64 / 4] = 1;
}

static void test_tasks(void)
{
  uint64_t task = 0, thread = 0;
  int64_t rc = H->task_create(C0, &task);
  if (rc < 0) { put("SKIP tasks: task_create returned "); hex((uint64_t)rc); put("\n"); return; }
  int trapped = (H->info->features & XH_FEATURE_TASKS) != 0;
  uint64_t code_pa = page_alloc(), data_pa = page_alloc();
  uint32_t *code = (uint32_t *)(uintptr_t)kva(code_pa);
  code[0] = 0xd2800540;      // mov x0, #42
  code[1] = 0xd2800030;      // mov x16, #1
  code[2] = 0xd4001001;      // svc #0x80
  code[3] = 0xd2c00041;      // mov x1, #0x200000000
  code[4] = 0xf9000020;      // str x0, [x1]
  code[5] = 0xd4200020;      // brk #1
  code[6] = 0x14000000;      // b .
  const uint64_t code_va = 0x100000000ull, data_va = 0x200000000ull;
  check(H->task_map(C0, task, code_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, code_pa) == 0, "task_map of a code page at 0x100000000");
  check(H->task_map(C0, task, H->info->user_va_limit, 0x4000, XH_PROT_READ, code_pa) < 0, "task_map refuses the stub's region");
  check(H->thread_create(C0, task, &thread) == 0, "thread_create");
  static struct xh_user_state st;
  struct xh_trap trap;
  zero(&st, sizeof st);
  st.pc = trapped ? code_va : code_va + 12;
  st.x[0] = 7;
  st.tsd = 0x1234500;
  st.v[3].lo = 0x1122334455667788ull;
  if (trapped) {
    check(H->thread_run(C0, thread, &st, &trap) == 0 && trap.kind == XH_TRAP_SYSCALL && trap.svc == 0x80 && st.x[16] == 1 &&
          st.x[0] == 42 && st.pc == code_va + 12, "svc #0x80 arrives as a system-call trap with x16 and x0 intact");
    st.x[0] = 7;                                  // "the system call's return value"
  } else put("SKIP the svc trap: no seccomp filter on this host\n");
  check(H->thread_run(C0, thread, &st, &trap) == 0 && trap.kind == XH_TRAP_FAULT && trap.far == data_va &&
        (trap.access == XH_PROT_WRITE || trap.access == 0) && st.pc == code_va + 16, "a store to an unmapped page arrives as a fault");
  if (!(st.tsd == 0x1234500 && st.v[3].lo == 0x1122334455667788ull)) { put("  tsd, v3: "); hex(st.tsd); put(" "); hex(st.v[3].lo); put("\n"); }
  check(st.tsd == 0x1234500 && st.v[3].lo == 0x1122334455667788ull, "the thread register and a vector register came back unchanged");
  check(H->task_map(C0, task, data_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE, data_pa) == 0, "the kernel maps the page (pmap_enter)");
  check(H->thread_run(C0, thread, &st, &trap) == 0 && trap.kind == XH_TRAP_BRK && st.pc == code_va + 20, "the store is retried, then brk traps");
  check(*(uint64_t *)(uintptr_t)kva(data_pa) == 7, "the task's store is visible in the kernel's view of the page (shared memory)");
  uint64_t value = 0xfeedfacecafebeefull, back = 0;
  int64_t wrote = H->task_write(C0, task, data_va + 8, &value, 8);
  if (wrote == -38) put("SKIP task_write and task_read: this host has no process_vm_writev\n");
  else check(wrote == 8 && H->task_read(C0, task, data_va + 8, &back, 8) == 8 && back == value &&
             ((uint64_t *)(uintptr_t)kva(data_pa))[1] == value, "task_write and task_read (copyout, copyin)");
  check(H->task_protect(C0, task, data_va, 0x4000, XH_PROT_READ) == 0, "task_protect");
  // Preemption: a thread that never traps is stopped when the vCPU's timer fires.
  st.pc = code_va + 24;
  uint32_t fires = timer_fires;
  C0->irq_masked = 1;
  H->timer_set(C0, now() + H->info->timebase_hz / 50);
  check(H->thread_run(C0, thread, &st, &trap) == 0 && trap.kind == XH_TRAP_INTERRUPT && st.pc == code_va + 24, "a looping thread is preempted by the timer");
  irq_upcall(C0, NULL);
  check(timer_fires == fires + 1, "and the timer interrupt is then taken by the kernel");
  check(H->thread_destroy(C0, thread) == 0, "thread_destroy");
  check(H->task_unmap(C0, task, data_va, 0x4000) == 0, "task_unmap");
  check(H->task_destroy(C0, task) == 0, "task_destroy");
}

__attribute__((noreturn)) void kernel_main(const struct boot_args *args, const struct xh_vector *vector, struct xh_cpu *cpu);
void kernel_main(const struct boot_args *args, const struct xh_vector *vector, struct xh_cpu *cpu)
{
  H = vector; C0 = cpu; BA = args;
  put("TESTKERNEL: entered\n");
  // 1. What the loader handed over.
  check(args->revision == 2 && args->version == 2, "boot_args revision and version");
  check(args->virt_base == (uint64_t)(uintptr_t)__base, "boot_args.virtBase is where the kernel runs (no MMU switch)");
  check(args->mem_size == args->mem_size_actual && args->top_of_kernel_data > args->phys_base, "memory fields");
  check(vector->abi == XH_ABI_VERSION && vector->size == sizeof *vector && vector->info->abi == XH_ABI_VERSION, "host-call vector ABI");
  check(vector->info->ram_virt == args->virt_base && vector->info->ram_phys == args->phys_base && vector->info->ncpu == 3, "xh_info agrees with boot_args");
  const uint8_t *tree = (const uint8_t *)(uintptr_t)args->device_tree;
  uint32_t len = 0;
  const uint8_t *name = adt_prop(tree, "name", &len, NULL);
  check(name && same(name, "device-tree", 12), "the device tree's root");
  const uint8_t *chosen = adt_child(tree, "chosen"), *host = chosen ? adt_child(chosen, "xnuhost") : NULL;
  const uint8_t *v = host ? adt_prop(host, "vector", &len, NULL) : NULL;
  check(v && *(const uint64_t *)v == (uint64_t)(uintptr_t)vector, "/chosen/xnuhost vector");
  const uint8_t *cpus = adt_child(tree, "cpus");
  check(cpus && adt_child(cpus, "cpu2") && !adt_child(cpus, "cpu3"), "/cpus has the three CPUs");
  const uint8_t *io = adt_child(tree, "arm-io");
  check(io && adt_child(io, "intc") && adt_child(io, "virtio-mmio-0") && adt_child(io, "data-store") && adt_child(io, "input-0") &&
        adt_child(io, "virtio-gpu") && adt_child(io, "uart0"), "/arm-io has the board's devices");
  next_page = (args->top_of_kernel_data + 0x3fff) & ~0x3fffull;

  // 2. Instructions that are undefined at EL0, emulated.
  uint64_t value = 0x1122334455667788ull, back = 0, el = 0;
  __asm__ volatile("msr tpidr_el1, %0" : : "r"(value));
  __asm__ volatile("mrs %0, tpidr_el1" : "=r"(back));
  __asm__ volatile("mrs %0, CurrentEL" : "=r"(el));
  check(back == value && cpu->tpidr_el1 == value, "TPIDR_EL1 write and read (XNU's current thread)");
  check(el == 4, "CurrentEL reads as EL1");
  check(cpu->irq_masked == 1, "the boot CPU starts with interrupts masked");
  __asm__ volatile("msr daifclr, #2");
  int unmasked = cpu->irq_masked == 0;
  __asm__ volatile("msr daifset, #2");
  check(unmasked && cpu->irq_masked == 1, "msr daifclr/daifset drive cpu->irq_masked");
  __asm__ volatile("msr vbar_el1, %0" : : "r"(value));
  __asm__ volatile("mrs %0, vbar_el1" : "=r"(back));
  check(back == value, "another EL1 register reads back what was written (shadow)");
  __asm__ volatile("tlbi vmalle1");
  check(1, "tlbi is accepted");

  // 3. Interrupts: the timer by host call, taken from idle.
  // Filled at run time: this program has no fixups, so a statically initialised function pointer
  // would hold its link address (XNU rebases its own chained fixups after entry).
  static volatile struct xh_upcalls upcalls;
  upcalls.irq = irq_upcall;
  upcalls.cpu_entry = secondary_entry;
  check(H->set_upcalls(cpu, (const struct xh_upcalls *)&upcalls) == 0, "set_upcalls");
  H->irq_enable(cpu, XH_INTID_VTIMER, 1);
  H->irq_enable(cpu, 5, 1);
  uint64_t start = now(), hz = H->info->timebase_hz;
  H->timer_set(cpu, start + hz / 100);
  while (!timer_fires && now() - start < hz * 5) { H->cpu_idle(cpu); if (cpu->irq_pending) irq_upcall(cpu, NULL); }
  uint64_t elapsed = now() - start;
  if (!(timer_fires == 1 && last_intid == XH_INTID_VTIMER && elapsed >= hz / 100)) { put("  fires, intid, elapsed, hz: "); hex(timer_fires); put(" "); hex(last_intid); put(" "); hex(elapsed); put(" "); hex(hz); put("\n"); }
  check(timer_fires == 1 && last_intid == XH_INTID_VTIMER && elapsed >= hz / 100, "the timer fires after its deadline and wakes cpu_idle");
  check(H->irq_ack(cpu) == XH_INTID_NONE && cpu->irq_pending == 0, "the line is low again after end-of-interrupt");

  // 4. Interrupts taken while kernel code runs unmasked: five timer ticks during a computation.
  double f0, f1;
  uint64_t expect = work(3000000, &f0);
  rearm = 4;
  timer_fires = 0;
  H->timer_set(cpu, now() + hz / 1000);
  cpu->irq_masked = 0;
  uint64_t got = work(3000000, &f1);
  start = now();
  while (timer_fires < 5 && now() - start < hz * 5) {}
  cpu->irq_masked = 1;
  check(timer_fires == 5, "five timer interrupts were delivered into running kernel code");
  check(got == expect && f0 == f1, "and the interrupted computation's integer and floating-point state survived");

  // 5. The timer through the registers ArmCpu writes.
  timer_fires = 0;
  uint64_t deadline = now() + hz / 200, ctl = 1;
  __asm__ volatile("msr cntv_cval_el0, %0" : : "r"(deadline));
  __asm__ volatile("msr cntv_ctl_el0, %0" : : "r"(ctl));
  start = now();
  while (!timer_fires && now() - start < hz * 5) { H->cpu_idle(cpu); if (cpu->irq_pending) irq_upcall(cpu, NULL); }
  check(timer_fires == 1, "CNTV_CVAL_EL0/CNTV_CTL_EL0 arm the same timer");
  uint64_t iar = 0;
  __asm__ volatile("mrs %0, S3_0_C12_C12_0" : "=r"(iar));        // ICC_IAR1_EL1
  check(iar == XH_INTID_NONE, "ICC_IAR1_EL1 reads no pending interrupt");

  // 6. Other CPUs: by host call, and by PSCI CPU_ON as ArmCpu calls it.
  check(H->cpu_start(cpu, 1, 0, 0x1000) == 0, "cpu_start of CPU 1");
  start = now();
  while (!(sgi_seen & 1u << 5) && now() - start < hz * 5) { H->cpu_idle(cpu); if (cpu->irq_pending) irq_upcall(cpu, NULL); }
  check(secondary_seen == 0x1001 && (sgi_seen & 1u << 5), "CPU 1 ran its entry and its SGI reached CPU 0");
  check(H->cpu_start(cpu, 1, 0, 0) < 0, "a CPU cannot be started twice");
  register uint64_t x0 __asm__("x0") = XH_PSCI_CPU_ON, x1 __asm__("x1") = 2, x2 __asm__("x2") = kpa(psci_entry), x3 __asm__("x3") = 0xabcdef;
  __asm__ volatile("hvc #0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3) : "memory");
  uint64_t psci_result = x0;
  start = now();
  while (!psci_seen && now() - start < hz * 5) {}
  check(psci_result == 0 && psci_seen == 0xabcdef, "PSCI CPU_ON (hvc) starts CPU 2 at a physical entry address");

  // 7. Devices and tasks.
  test_devices();
  uint32_t text[2] = { 0xd53bd063, 0xd53bd040 };                  // mrs x3, tpidrro_el0; mrs x0, tpidr_el0
  check(H->text_fixup(cpu, text, sizeof text) == 1 && text[0] == 0xd53bd043 && text[1] == 0xd53bd040, "text_fixup rewrites TPIDRRO_EL0 reads");
  uint64_t scratch = page_alloc();
  *(volatile uint64_t *)(uintptr_t)kva(scratch) = 5;
  check(H->ram_discard(cpu, scratch, 0x4000) == 0 && *(volatile uint64_t *)(uintptr_t)kva(scratch) == 0, "ram_discard gives pages back (they read as zero)");
  check(H->wall_time(cpu) > 1700000000ull, "wall_time");
  test_tasks();

  put(failures ? "TESTKERNEL: FAILED\n" : "TESTKERNEL: all passed\n");
  H->halt(cpu, failures ? XH_HALT_PANIC : XH_HALT_OFF, (uint32_t)failures);
  for (;;) {}
}
