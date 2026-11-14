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
static volatile uint64_t secondary_seen, psci_seen, psci_ok;

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

// vCPU 1's part of test_unmap_race: while armed it spins, and each time `go` moves it runs worker.thread once (from
// worker_st) and reports the trap; otherwise it idles (SGI 6 wakes it to look again).
static volatile struct { uint64_t thread; uint32_t armed, go, done, kind; uint64_t far; } worker;
static struct xh_user_state worker_st;

static void secondary_entry(struct xh_cpu *cpu, uint64_t arg)
{
  secondary_seen = arg + cpu->index;
  H->irq_enable(cpu, 6, 1);
  H->ipi(cpu, 0, 5);
  for (;;) {
    if (!worker.armed) { H->cpu_idle(cpu); if (cpu->irq_pending) irq_upcall(cpu, NULL); continue; }
    uint32_t go = __atomic_load_n(&worker.go, __ATOMIC_ACQUIRE);
    if (go == worker.done) { __asm__ volatile("yield"); continue; }
    struct xh_trap trap;
    int64_t rc;
    while ((rc = H->thread_run(cpu, worker.thread, &worker_st, &trap)) == 0 && trap.kind == XH_TRAP_INTERRUPT) irq_upcall(cpu, NULL);
    worker.kind = rc ? 0xffffffffu : trap.kind;
    worker.far = trap.far;
    __atomic_store_n(&worker.done, go, __ATOMIC_RELEASE);
  }
}

__attribute__((noreturn)) void psci_entry(uint64_t context, const struct xh_vector *vector, struct xh_cpu *cpu);
void psci_entry(uint64_t context, const struct xh_vector *vector, struct xh_cpu *cpu)
{
  psci_ok = vector == H && cpu->index == 2;     // entered like vCPU 0
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

// The deferred unmaps (task.c, task.unmaps_out) with a loading thread: `ldr x0, [x1]; brk` at code_va + 32.
static int load_once(struct xh_cpu *cpu, uint64_t thread, uint64_t code_va, uint64_t va, struct xh_trap *trap, uint64_t *value)
{
  static struct xh_user_state ls;
  zero(&ls, sizeof ls);
  ls.pc = code_va + 32;
  ls.x[1] = va;
  int64_t rc;
  while ((rc = H->thread_run(cpu, thread, &ls, trap)) == 0 && trap->kind == XH_TRAP_INTERRUPT) irq_upcall(cpu, NULL);
  if (value) *value = ls.x[0];
  return rc == 0 ? (int)trap->kind : -1;
}

static void test_unmap_queue(uint64_t task, uint64_t thread, uint64_t code_va, uint64_t data_pa)
{
  struct xh_trap trap;
  uint64_t got = 0;
  const uint64_t x_va = 0x400000000ull, fill_va = 0x500000000ull;
  uint64_t other_pa = page_alloc();
  *(volatile uint64_t *)(uintptr_t)kva(data_pa) = 0x1111;
  *(volatile uint64_t *)(uintptr_t)kva(other_pa) = 0x2222;
  // In the queue's order: an unmap, then a map of the same address to another page.
  check(H->task_map(C0, task, x_va, 0x4000, XH_PROT_READ, data_pa) == 0 && load_once(C0, thread, code_va, x_va, &trap, &got) == XH_TRAP_BRK &&
        got == 0x1111, "deferred unmaps: a page mapped and loaded");
  check(H->task_unmap(C0, task, x_va, 0x4000) == 0 && H->task_map(C0, task, x_va, 0x4000, XH_PROT_READ, other_pa) == 0 &&
        load_once(C0, thread, code_va, x_va, &trap, &got) == XH_TRAP_BRK && got == 0x2222,
        "an unmap and a new map of the same address, queued: the thread sees the new page");
  // A full queue (256): the next unmap is ordered and takes the queue with it; the last one is queued again.
  int ok = 1;
  for (unsigned i = 0; i < 300; i++) ok &= H->task_unmap(C0, task, fill_va + (uint64_t)i * 0x4000, 0x4000) == 0;
  check(ok && H->task_unmap(C0, task, x_va, 0x4000) == 0 && load_once(C0, thread, code_va, x_va, &trap, NULL) == XH_TRAP_FAULT &&
        trap.far == x_va, "301 unmaps while the thread is stopped (more than the queue holds): the last is in force");
  // task_read after a queued unmap: the read must not see the old page (it fails; the kernel would fault it in).
  uint64_t back = 0;
  int64_t read = (H->task_map(C0, task, x_va, 0x4000, XH_PROT_READ, data_pa), H->task_read(C0, task, x_va, &back, 8));
  if (read == -38) { put("SKIP task_read after queued maps and unmaps: this host has no process_vm_readv\n"); H->task_unmap(C0, task, x_va, 0x4000); }
  else check(read == 8 && back == 0x1111 && H->task_unmap(C0, task, x_va, 0x4000) == 0 && H->task_read(C0, task, x_va, &back, 8) < 0,
             "task_read after a queued map reads the page, after a queued unmap it fails");
  // ram_discard (the kernel's free-page reporting, 0058) and a task's mapping of the page. One page in every
  // mapping: given back, the task reads zero; written again by the kernel, the task reads the new value.
  const uint64_t d_va = 0x600000000ull;
  uint64_t d_pa = page_alloc();
  *(volatile uint64_t *)(uintptr_t)kva(d_pa) = 0x3333;
  if (H->info->features & XH_FEATURE_RAM_FIXED) {
    put("SKIP ram_discard with a task's mapping: the RAM is held fixed (ram-giveback=0); check-arm64's first boot runs it\n");
    return;
  }
  check(H->task_map(C0, task, d_va, 0x4000, XH_PROT_READ, d_pa) == 0 && load_once(C0, thread, code_va, d_va, &trap, &got) == XH_TRAP_BRK &&
        got == 0x3333 && H->ram_discard(C0, d_pa, 0x4000) == 0 && load_once(C0, thread, code_va, d_va, &trap, &got) == XH_TRAP_BRK && got == 0 &&
        (*(volatile uint64_t *)(uintptr_t)kva(d_pa) = 0x4444, load_once(C0, thread, code_va, d_va, &trap, &got)) == XH_TRAP_BRK && got == 0x4444,
        "ram_discard of a page a task maps: the task reads zero, then what the kernel writes into the page again");
  // The page's unmap queued (the thread is stopped), the page given back and used again by the kernel before the
  // task runs: the task's next run carries the unmap out first and faults; it never sees the page's new data.
  check(H->task_unmap(C0, task, d_va, 0x4000) == 0 && H->ram_discard(C0, d_pa, 0x4000) == 0 &&
        (*(volatile uint64_t *)(uintptr_t)kva(d_pa) = 0x5555, load_once(C0, thread, code_va, d_va, &trap, NULL)) == XH_TRAP_FAULT && trap.far == d_va,
        "ram_discard of a page whose unmap is queued: the task faults at its next run, the page's new data is not seen");
}

// Two threads of one task on two vCPUs. Thread A is stopped with the page at x_va mapped in its process; the kernel
// queues 128 maps and then the unmap of x_va (no thread runs); then vCPU 1 releases thread B while vCPU 0 releases
// thread A (round 0) or gives the task an order (round 1), so one of them takes the queue. Whoever does not must wait
// until the unmap is done: both threads' loads of x_va fault, every time.
static void test_unmap_race(uint64_t task, uint64_t a, uint64_t code_va, uint64_t data_pa)
{
  uint64_t b = 0;
  if (H->thread_create(C0, task, &b)) { check(0, "unmap race: a second thread"); return; }
  const uint64_t x_va = 0x400000000ull, fill_va = 0x600000000ull;
  struct xh_trap trap;
  unsigned rounds = 200, bad_a = 0, bad_b = 0, setup = 0;
  worker.thread = b;
  worker.done = worker.go = 0;
  __atomic_store_n(&worker.armed, 1, __ATOMIC_RELEASE);
  H->ipi(C0, 1, 6);
  for (unsigned i = 0; i < rounds; i++) {
    if (H->task_map(C0, task, x_va, 0x4000, XH_PROT_READ, data_pa) || load_once(C0, a, code_va, x_va, &trap, NULL) != XH_TRAP_BRK) { setup++; continue; }
    for (unsigned j = 0; j < 128; j++) H->task_map(C0, task, fill_va + (uint64_t)j * 0x4000, 0x4000, XH_PROT_READ, data_pa);
    H->task_unmap(C0, task, x_va, 0x4000);
    zero(&worker_st, sizeof worker_st);
    worker_st.pc = code_va + 32;
    worker_st.x[1] = x_va;
    uint32_t go = worker.go + 1;
    __atomic_store_n(&worker.go, go, __ATOMIC_RELEASE);
    if (i & 1) H->task_protect(C0, task, code_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC);
    else if (load_once(C0, a, code_va, x_va, &trap, NULL) != XH_TRAP_FAULT || trap.far != x_va) bad_a++;
    uint64_t start = now();
    while (__atomic_load_n(&worker.done, __ATOMIC_ACQUIRE) != go && now() - start < H->info->timebase_hz * 20) {}
    if (worker.done != go || worker.kind != XH_TRAP_FAULT || worker.far != x_va) bad_b++;
    if (worker.done != go) break;
  }
  __atomic_store_n(&worker.armed, 0, __ATOMIC_RELEASE);
  if (bad_a || bad_b || setup) { put("  unmap race: rounds, bad A, bad B, setup: "); hex(rounds); put(" "); hex(bad_a); put(" "); hex(bad_b); put(" "); hex(setup); put("\n"); }
  check(!setup && !bad_a && !bad_b, "unmap race: 200 rounds, the thread that did not carry a queued unmap never sees the old page");
  check(H->thread_destroy(C0, b) == 0, "unmap race: the second thread destroyed");
}

// WebKit's JIT (MAP_JIT) at the host-call level: a page is writable through one mapping and executable through
// another of the same physical page (the "separated W^X heaps" WebKit uses on a host with no APRR/SPRR: real
// hardware here has none). The thread writes an instruction sequence through the read-write alias, does the i-cache
// maintenance an arm64 JIT must, and branches to the read-execute alias. The page is executed as it was written:
// the two aliases share the physical page (two task_map mmaps of one RAM offset), and a store through the writable
// one is seen by an instruction fetch through the executable one. 0067 (fault 10): the hosted layer carries no
// per-thread W^X (one Linux process cannot give two threads different permissions of one page), so WebKit must not
// take the per-thread path; this checks the aliasing path it does take.
// JSC's first JIT thunk (JSC::LLInt::createJSGateThunk -> LinkBuffer::finalizeCodeWithoutDisassemblyImpl ->
// sys_icache_invalidate): code written through the RW alias of a MAP_JIT page, then `dc cvau` + `dsb ish` +
// `ic ivau` + `dsb ish` + `isb` on the **RX** alias, then a branch to it. resident = 1: the RX alias is mapped first.
// resident = 0: it is not (as on the phone, where the hosted pmap maps a page into the task only when it faults), so
// the alias's first access is the dc cvau. On arm64 hardware that faults as a data abort with ISS.CM and WnR set: the
// stub must say XH_PROT_READ (the kernel then maps the RX page and the dc cvau is retried). Taken as a write, iOS
// failed it on the RX entry (phone 2026-11-14: every WebContent process, "byte write Permission fault"). qemu-user
// runs dc cvau / ic ivau as no-ops that never fault, so under qemu the first access is the branch (an exec fault).
static void test_jit_icache(int resident)
{
  const char *what = resident ? "jit icache (RX alias resident)" : "jit icache (RX alias never touched)";
  uint64_t task = 0, thread = 0;
  if (H->task_create(C0, &task) < 0) { put("SKIP "); put(what); put(": task_create\n"); return; }
  uint64_t code_pa = page_alloc(), jit_pa = page_alloc();
  uint32_t *c = (uint32_t *)(uintptr_t)kva(code_pa);
  c[0] = 0x52816c05; c[1] = 0x72ba5005; c[2] = 0xb9000025;      // mov w5,#0xb60; movk w5,#0xd280,lsl16; str w5,[x1]
  c[3] = 0x52800405; c[4] = 0x72ba8405; c[5] = 0xb9000425;      // mov w5,#0x20;  movk w5,#0xd420,lsl16; str w5,[x1,#4]
  c[6] = 0xd50b7b22; c[7] = 0xd5033b9f; c[8] = 0xd50b7522;      // dc cvau,x2; dsb ish; ic ivau,x2
  c[9] = 0xd5033b9f; c[10] = 0xd5033fdf; c[11] = 0xd61f0040;    // dsb ish; isb; br x2   (x2: "mov x0,#0x5b; brk #1")
  H->text_fixup(C0, c, 12 * 4);
  const uint64_t code_va = 0x110000000ull, exec_va = 0x190000000ull, write_va = 0x1d0000000ull;
  check(H->task_map(C0, task, write_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE | XH_PROT_EXEC, jit_pa) == 0 &&
        H->task_map(C0, task, code_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, code_pa) == 0 &&
        (!resident || H->task_map(C0, task, exec_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, jit_pa) == 0), what);
  if (H->thread_create(C0, task, &thread) != 0) { check(0, "jit icache: thread_create"); H->task_destroy(C0, task); return; }
  static struct xh_user_state st;
  struct xh_trap trap;
  zero(&st, sizeof st);
  st.pc = code_va;
  st.x[1] = write_va;
  st.x[2] = exec_va;
  int k = -1, faults = 0, cm_faults = 0, cm_as_write = 0, exec_faults = 0, other = 0;
  for (int round = 0; round < 8; round++) {
    int64_t r;
    while ((r = H->thread_run(C0, thread, &st, &trap)) == 0 && trap.kind == XH_TRAP_INTERRUPT) irq_upcall(C0, NULL);
    k = r == 0 ? (int)trap.kind : -1;
    if (k != XH_TRAP_FAULT) break;
    faults++;
    if ((trap.far & ~0x3fffull) != exec_va) { other++; break; }
    if (st.pc == code_va + 6 * 4 || st.pc == code_va + 8 * 4) {
      // the cache maintenance on the RX alias faulted (arm64 hardware): a read, with ISS.CM in Linux's syndrome
      cm_faults++;
      if (trap.access != XH_PROT_READ || (trap.esr != 0 && !(trap.esr & (1ull << 8)))) cm_as_write++;
      put("  "); put(what); put(": cache maintenance at pc "); hex(st.pc); put(" faulted: access ");
      hex(trap.access); put(" esr "); hex(trap.esr); put("\n");
    } else if (st.pc == exec_va) exec_faults++;
    else { other++; break; }
    // As the kernel does for a read or an execute fault on the RX entry: the page mapped read-execute, then retried.
    if (H->task_map(C0, task, exec_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, jit_pa) != 0) { other++; break; }
  }
  int ran = k == XH_TRAP_BRK && st.x[0] == 0x5b && st.pc == exec_va + 4;
  if (!ran || other) {
    put("  "); put(what); put(": kind "); hex((uint64_t)(uint32_t)k); put(" x0 "); hex(st.x[0]); put(" pc "); hex(st.pc);
    put(" far "); hex(trap.far); put(" faults "); hex((uint64_t)faults); put("\n");
  }
  check(ran && !other, resident ? "jit icache: dc cvau / ic ivau on the resident RX alias, then the code runs (x0 = 0x5b)"
                                : "jit icache: dc cvau / ic ivau as the RX alias's first access, then the code runs (x0 = 0x5b)");
  if (resident) check(faults == 0, "jit icache: no fault on the resident RX alias");
  else {
    check(!cm_as_write, "jit icache: a cache maintenance fault on the RX alias is a read (ISS.CM), not a write");
    if (cm_faults == 0) put("NOTE jit icache: the cache maintenance did not fault (qemu-user runs dc cvau / ic ivau as no-ops); "
                            "the RX alias's first fault was the branch\n");
    check(cm_faults + exec_faults >= 1, "jit icache: the never-touched RX alias faulted once and was mapped read-execute");
  }
  check(H->thread_destroy(C0, thread) == 0 && H->task_destroy(C0, task) == 0, "jit icache: thread_destroy, task_destroy");
}

static void test_jit(void)
{
  uint64_t task = 0, thread = 0;
  int64_t rc = H->task_create(C0, &task);
  if (rc < 0) { put("SKIP jit: task_create returned "); hex((uint64_t)rc); put("\n"); return; }
  // The writer stub (verified encodings): write "mov x0,#0x5a; brk #1" through x1 (the RW alias), clean and
  // invalidate the two code words for the exec alias in x2, then branch to it.
  uint64_t jcode_pa = page_alloc(), jit_pa = page_alloc();
  uint32_t *jc = (uint32_t *)(uintptr_t)kva(jcode_pa);
  jc[0] = 0x52816805; jc[1] = 0x72ba5005; jc[2] = 0xb9000025;      // mov w5,#0xb40; movk w5,#0xd280,lsl16; str w5,[x1]
  jc[3] = 0x52800405; jc[4] = 0x72ba8405; jc[5] = 0xb9000425;      // mov w5,#0x20;  movk w5,#0xd420,lsl16; str w5,[x1,#4]
  jc[6] = 0xd50b7b22; jc[7] = 0x91001046; jc[8] = 0xd50b7b26;      // dc cvau,x2; add x6,x2,#4; dc cvau,x6
  jc[9] = 0xd5033b9f; jc[10] = 0xd50b7522; jc[11] = 0xd50b7526;    // dsb ish; ic ivau,x2; ic ivau,x6
  jc[12] = 0xd5033b9f; jc[13] = 0xd5033fdf; jc[14] = 0xd61f0040;   // dsb ish; isb; br x2
  H->text_fixup(C0, jc, 15 * 4);
  const uint64_t jcode_va = 0x100000000ull, exec_va = 0x180000000ull, write_va = 0x1c0000000ull;
  // The JIT page in one task_map must be read, write and execute at once (MAP_JIT's W+X reservation: the host must
  // allow a W+X mapping of the RAM memfd).
  check(H->task_map(C0, task, write_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE | XH_PROT_EXEC, jit_pa) == 0,
        "jit: task_map of a W+X page (MAP_JIT)");
  check(H->task_map(C0, task, exec_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, jit_pa) == 0,
        "jit: the same physical page mapped read-execute at a second address (the W^X alias)");
  check(H->task_map(C0, task, jcode_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, jcode_pa) == 0, "jit: the writer stub mapped");
  if (H->thread_create(C0, task, &thread) != 0) { check(0, "jit: thread_create"); H->task_destroy(C0, task); return; }
  static struct xh_user_state st;
  struct xh_trap trap;
  zero(&st, sizeof st);
  st.pc = jcode_va;
  st.x[1] = write_va;
  st.x[2] = exec_va;
  int k = -1;
  int64_t r;
  while ((r = H->thread_run(C0, thread, &st, &trap)) == 0 && trap.kind == XH_TRAP_INTERRUPT) irq_upcall(C0, NULL);
  if (r == 0) k = (int)trap.kind;
  if (!(k == XH_TRAP_BRK && st.x[0] == 0x5a && st.pc == exec_va + 4)) {
    put("  jit: kind "); hex((uint64_t)(uint32_t)k); put(" x0 "); hex(st.x[0]); put(" pc "); hex(st.pc);
    put(" jit_pa[0] "); hex(*(volatile uint32_t *)(uintptr_t)kva(jit_pa)); put("\n");
  }
  check(k == XH_TRAP_BRK && st.x[0] == 0x5a && st.pc == exec_va + 4,
        "jit: code written through the RW alias runs through the RX alias (x0 = 0x5a at the brk)");
  // The kernel's own view of the page sees the thread's JIT store too (shared memory, as copyio would read it).
  check(*(volatile uint32_t *)(uintptr_t)kva(jit_pa) == 0xd2800b40,
        "jit: the thread's store through the RW alias is in the kernel's view of the page");
  check(H->thread_destroy(C0, thread) == 0, "jit: thread_destroy");
  check(H->task_unmap(C0, task, exec_va, 0x4000) == 0 && H->task_unmap(C0, task, write_va, 0x4000) == 0, "jit: unmap the aliases");
  check(H->task_destroy(C0, task) == 0, "jit: task_destroy");
  test_jit_icache(1);
  test_jit_icache(0);
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
  code[8] = 0xf9400020;      // ldr x0, [x1]      (test_unmap_race)
  code[9] = 0xd4200020;      // brk #1
  // As the kernel does with a page of signed text before it maps it: on a host with no seccomp the
  // svc becomes the undefined instruction thread_run reports as a system call.
  H->text_fixup(C0, code, 10 * 4);
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
  } else put("SKIP the svc trap: this host has no task layer\n");
  check(H->thread_run(C0, thread, &st, &trap) == 0 && trap.kind == XH_TRAP_FAULT && trap.far == data_va &&
        (trap.access == XH_PROT_WRITE || trap.access == 0) && st.pc == code_va + 16, "a store to an unmapped page arrives as a fault");
  if (!(st.tsd == 0x1234500 && st.v[3].lo == 0x1122334455667788ull)) { put("  tsd, v3: "); hex(st.tsd); put(" "); hex(st.v[3].lo); put("\n"); }
  check(st.tsd == 0x1234500 && st.v[3].lo == 0x1122334455667788ull, "the thread register and a vector register came back unchanged");
  check(H->task_map(C0, task, data_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE, data_pa) == 0, "the kernel maps the page (pmap_enter)");
  check(H->thread_run(C0, thread, &st, &trap) == 0 && trap.kind == XH_TRAP_BRK && st.pc == code_va + 20, "the store is retried, then brk traps");
  // the store's fault came just before on this task: Linux's SIGTRAP carries that fault's syndrome as its own
  // (stub.c), and the kernel would have reported the brk as a data abort
  if (trapped) check(trap.esr == 0 && trap.insn == 0xd4200020, "brk after a fault: no syndrome of that fault, and the brk instruction for the kernel");
  check(*(uint64_t *)(uintptr_t)kva(data_pa) == 7, "the task's store is visible in the kernel's view of the page (shared memory)");
  uint64_t value = 0xfeedfacecafebeefull, back = 0;
  int64_t wrote = H->task_write(C0, task, data_va + 8, &value, 8);
  if (wrote == -38) put("SKIP task_write and task_read: this host has no process_vm_writev\n");
  else check(wrote == 8 && H->task_read(C0, task, data_va + 8, &back, 8) == 8 && back == value &&
             ((uint64_t *)(uintptr_t)kva(data_pa))[1] == value, "task_write and task_read (copyout, copyin)");
  check(H->task_protect(C0, task, data_va, 0x4000, XH_PROT_READ) == 0, "task_protect");
  // task_map rides on the next release of a thread of the task (map-defer); an order given before that release
  // (here the protect above, below an unmap) must find it done and keep its place after it.
  st.pc = code_va + 16;                         // str x0, [x1]
  st.x[1] = data_va;
  check(H->thread_run(C0, thread, &st, &trap) == 0 && trap.kind == XH_TRAP_FAULT && trap.far == data_va && st.pc == code_va + 16,
        "a store to the page made read-only faults");
  const uint64_t other_va = 0x300000000ull;
  check(H->task_map(C0, task, other_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE, data_pa) == 0 && H->task_unmap(C0, task, other_va, 0x4000) == 0,
        "a map and an unmap of another page before the thread runs");
  st.x[1] = other_va;
  check(H->thread_run(C0, thread, &st, &trap) == 0 && trap.kind == XH_TRAP_FAULT && trap.far == other_va,
        "the unmap came after the map: the store faults");
  check(H->task_map(C0, task, other_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE, data_pa) == 0 &&
        H->thread_run(C0, thread, &st, &trap) == 0 && trap.kind == XH_TRAP_BRK && st.pc == code_va + 20 &&
        ((uint64_t *)(uintptr_t)kva(data_pa))[0] == st.x[0], "a map given before the run is in place when the thread runs");
  // task_unmap while no thread of the task runs waits in the queue (task.c): it must be in force when the thread runs.
  check(H->task_unmap(C0, task, other_va, 0x4000) == 0, "an unmap while the task's thread is stopped");
  st.pc = code_va + 16;
  st.x[1] = other_va;
  check(H->thread_run(C0, thread, &st, &trap) == 0 && trap.kind == XH_TRAP_FAULT && trap.far == other_va,
        "it is in force when the thread runs again: the store faults");
  if (trapped) test_unmap_queue(task, thread, code_va, data_pa);
  if (trapped) test_unmap_race(task, thread, code_va, data_pa);
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

// ---- benchmarks of the trap path (boot argument testkernel-bench; `make bench-arm64`) -------------
// Each loop times one kind of trip from the kernel's side, with the counter the iOS thread also reads (CNTVCT_EL0):
// the user code stamps the time just before its svc (x0) and just after its resume, so the two hand-offs are seen
// apart. Times go into a table of 4096 steps of one tick at 24 MHz or less (170 us at 24 MHz, 213 us at the phone's
// 19.2 MHz) and of hz / 24 MHz ticks on a faster counter (qemu-user's is 1 GHz: 41 ns steps, 168 us), so the table
// spans about the same time everywhere; later ones count in the last.
#define BT 4096
static void dec(uint64_t v)
{
  char b[21];
  int i = 20;
  b[i] = 0;
  do { b[--i] = (char)('0' + v % 10); v /= 10; } while (v);
  put(b + i);
}
typedef struct { uint32_t *t; uint64_t n, sum, step; } bhist;
static void bh_init(bhist *h)   // 4096 x 4 = one 16 KiB page
{
  h->t = (uint32_t *)(uintptr_t)kva(page_alloc());
  h->n = h->sum = 0;
  h->step = H->info->timebase_hz / 24000000u;
  if (!h->step) h->step = 1;
}
static void bh_add(bhist *h, uint64_t ticks)
{
  uint64_t i = ticks / h->step;
  h->t[i < BT ? i : BT - 1]++;
  h->n++;
  h->sum += ticks;
}
static uint64_t bh_q(const bhist *h, uint64_t per_mille)   // in ticks: the start of the step the quantile falls in
{
  uint64_t want = h->n * per_mille / 1000, seen = 0;
  for (uint64_t i = 0; i < BT; i++) { seen += h->t[i]; if (seen > want) return i * h->step; }
  return BT * h->step;
}
static void bh_put(const char *name, const bhist *h)
{
  uint64_t hz = H->info->timebase_hz;
  put("BENCH ");
  put(name);
  put(": n ");
  dec(h->n);
  if (!h->n) { put("\n"); return; }
  put(" mean ");
  dec(h->sum * 1000000000ull / h->n / hz);
  put(" ns, p50 ");
  dec(bh_q(h, 500) * 1000000000ull / hz);
  put(" p90 ");
  dec(bh_q(h, 900) * 1000000000ull / hz);
  put(" p99 ");
  dec(bh_q(h, 990) * 1000000000ull / hz);
  put(" ns\n");
}

static void bench(unsigned scale)
{
  uint64_t task = 0, thread = 0;
  if (H->task_create(C0, &task) < 0) { put("BENCH skipped: no task layer\n"); return; }
  uint64_t code_pa = page_alloc(), data_pa = page_alloc();
  uint32_t *code = (uint32_t *)(uintptr_t)kva(code_pa);
  // 0x00: the null system call               0x40: a store to an unmapped page, then a system call   0x80: a loop
  static const uint32_t syscall_loop[] = { 0xd53be040, 0xd4001001, 0x17fffffe };              // mrs x0, cntvct; svc #0x80; b 0
  static const uint32_t fault_loop[] = { 0xd53be040, 0xf9000041, 0xd53be043, 0xd4001001, 0x17fffffc };   // mrs x0; str x1, [x2]; mrs x3; svc; b 0x40
  for (unsigned i = 0; i < 3; i++) code[i] = syscall_loop[i];
  for (unsigned i = 0; i < 5; i++) code[16 + i] = fault_loop[i];
  code[32] = 0x14000000;                                                                        // b .
  H->text_fixup(C0, code, 33 * 4);
  const uint64_t code_va = 0x100000000ull, data_va = 0x200000000ull;
  H->task_map(C0, task, code_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, code_pa);
  if (H->thread_create(C0, task, &thread)) { put("BENCH skipped: no thread\n"); return; }
  static struct xh_user_state st;
  struct xh_trap trap;
  C0->irq_masked = 1;
  H->timer_set(C0, UINT64_MAX);
  irq_upcall(C0, NULL);

  // 1. The null system call: svc, trap, the kernel hands it straight back.
  bhist up, down, round;
  bh_init(&up); bh_init(&down); bh_init(&round);
  zero(&st, sizeof st);
  st.pc = code_va;
  uint64_t n = 20000ull * scale, before = 0, last = 0, bad = 0;
  for (uint64_t i = 0; i < n; i++) {
    before = now();
    if (H->thread_run(C0, thread, &st, &trap) || trap.kind != XH_TRAP_SYSCALL) { bad++; if (trap.kind == XH_TRAP_INTERRUPT) irq_upcall(C0, NULL); continue; }
    uint64_t t = now();
    if (i && st.x[0] >= before && st.x[0] <= t) { bh_add(&up, t - st.x[0]); bh_add(&down, st.x[0] - before); }
    if (last) bh_add(&round, t - last);
    last = t;
  }
  bh_put("null syscall, round trip (kernel to kernel)", &round);
  bh_put("  svc -> the kernel has the trap", &up);
  bh_put("  the kernel releases -> user code runs", &down);
  if (bad) { put("BENCH   (other traps: "); dec(bad); put(")\n"); }

  // 2. A page fault the kernel resolves by mapping the page, then the page is unmapped again (svc).
  bhist fault, map, unmap;
  bh_init(&fault); bh_init(&map); bh_init(&unmap);
  zero(&st, sizeof st);
  st.pc = code_va + 0x40;
  st.x[2] = data_va;
  n = 5000ull * scale;
  for (uint64_t i = 0; i < n; i++) {
    if (H->thread_run(C0, thread, &st, &trap) || trap.kind != XH_TRAP_FAULT) { bad++; if (trap.kind == XH_TRAP_INTERRUPT) irq_upcall(C0, NULL); continue; }
    uint64_t a = now();
    H->task_map(C0, task, data_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE, data_pa);
    bh_add(&map, now() - a);
    if (H->thread_run(C0, thread, &st, &trap) || trap.kind != XH_TRAP_SYSCALL) { bad++; continue; }
    bh_add(&fault, st.x[3] - st.x[0]);
    a = now();
    H->task_unmap(C0, task, data_va, 0x4000);
    bh_add(&unmap, now() - a);
  }
  bh_put("page fault filled by task_map, as user code sees it", &fault);
  bh_put("  task_map (host call)", &map);
  bh_put("  task_unmap (host call; queued while the thread is stopped)", &unmap);

  // 3. The interrupt of a running thread: the timer fires 50 us into a loop that never traps.
  bhist kick;
  bh_init(&kick);
  zero(&st, sizeof st);
  st.pc = code_va + 0x80;
  n = 2000ull * scale;
  for (uint64_t i = 0; i < n; i++) {
    uint64_t deadline = now() + H->info->timebase_hz / 20000;
    H->timer_set(C0, deadline);
    if (H->thread_run(C0, thread, &st, &trap) || trap.kind != XH_TRAP_INTERRUPT) { bad++; continue; }
    uint64_t t = now();
    if (t >= deadline) bh_add(&kick, t - deadline);
    irq_upcall(C0, NULL);
  }
  bh_put("timer interrupt of a running thread: deadline -> the kernel has the trap", &kick);
  H->timer_set(C0, UINT64_MAX);
  irq_upcall(C0, NULL);

  // 4. Free-page reporting (kernel 0058): ram_discard of one page and of 64 consecutive pages, and what a page costs
  // when it is used again, as XNU zero-fills a page it takes (16 KiB of 8-byte stores): resident, or given back (the
  // host's first touch of each 4 KiB part allocates and zeroes it).
  bhist give1, give64, fill_res, fill_given;
  bh_init(&give1); bh_init(&give64); bh_init(&fill_res); bh_init(&fill_given);
  uint64_t run = page_alloc();
  for (unsigned i = 1; i < 64; i++) page_alloc();
  n = 50ull * scale;
  for (uint64_t i = 0; i < n; i++) {
    for (unsigned p = 0; p < 64; p++) {
      volatile uint64_t *w = (volatile uint64_t *)(uintptr_t)kva(run + p * 0x4000ull);
      uint64_t a = now();
      for (unsigned k = 0; k < 0x4000 / 8; k++) w[k] = 0;
      bh_add(&fill_res, now() - a);
    }
    uint64_t a = now();
    if (H->ram_discard(C0, run, 64 * 0x4000ull)) bad++;
    bh_add(&give64, now() - a);
    for (unsigned p = 0; p < 64; p++) {
      volatile uint64_t *w = (volatile uint64_t *)(uintptr_t)kva(run + p * 0x4000ull);
      a = now();
      for (unsigned k = 0; k < 0x4000 / 8; k++) w[k] = 0;
      bh_add(&fill_given, now() - a);
    }
    for (unsigned p = 0; p < 64; p++) {
      a = now();
      if (H->ram_discard(C0, run + p * 0x4000ull, 0x4000)) bad++;
      bh_add(&give1, now() - a);
    }
    for (unsigned p = 0; p < 64; p++) {                                  // all of each page resident again
      volatile uint64_t *w = (volatile uint64_t *)(uintptr_t)kva(run + p * 0x4000ull);
      for (unsigned k = 0; k < 0x4000 / 8; k += 512) w[k] = 1;
    }
  }
  bh_put("ram_discard of one 16 KiB page (host call)", &give1);
  bh_put("ram_discard of 64 consecutive pages, 1 MiB (host call)", &give64);
  bh_put("zero-fill of a resident 16 KiB page", &fill_res);
  bh_put("zero-fill of a 16 KiB page given back (the host fills it again)", &fill_given);
  if (bad) { put("BENCH   (failed calls or other traps: "); dec(bad); put(")\n"); }
  H->thread_destroy(C0, thread);
  H->task_destroy(C0, task);
  put("BENCH done\n");
}

__attribute__((noreturn)) void kernel_main(const struct boot_args *args, const struct xh_vector *vector, struct xh_cpu *cpu);
void kernel_main(const struct boot_args *args, const struct xh_vector *vector, struct xh_cpu *cpu)
{
  H = vector; C0 = cpu; BA = args;
  put("TESTKERNEL: entered\n");
  // boot-args "testkernel-restart": ask for a restart at once, every time (xnuhost starts again in place,
  // then gives up: `make check-arm64` counts the starts).
  for (const char *c = args->command_line; *c; c++) {
    static const char want[] = "testkernel-restart";
    unsigned i = 0;
    while (want[i] && c[i] == want[i]) i++;
    if (!want[i]) { put("TESTKERNEL: restart\n"); H->halt(cpu, XH_HALT_RESTART, 0); }
  }
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
  // The RAM's size as XNU reads it in three places (ram=auto sizes all of them: docs/booter.md 1).
  const uint8_t *dram = chosen ? adt_prop(chosen, "dram-size", &len, NULL) : NULL;
  check(dram && len == 8 && *(const uint64_t *)dram == args->mem_size && vector->info->ram_size == args->mem_size && !(args->mem_size & 0x3fff),
        "the RAM's size agrees: boot_args.memSize, /chosen dram-size, xh_info.ram_size");
  put("TESTKERNEL: RAM ");
  dec(args->mem_size >> 20);
  put(" MiB\n");
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

  // 4b. A frame resumed unmasked while the line is up is interrupted again at once (an exception
  // return with a pending interrupt): the timer is made due masked, then the host is asked to
  // resume a frame of this very context, unmasked.
  timer_fires = 0;
  H->timer_set(cpu, now());                     // due now; masked, so only the line rises
  start = now();
  while (!cpu->irq_pending && now() - start < hz) {}
  static struct xh_frame rf;
  static volatile uint32_t resumed;
  zero(&rf, sizeof rf);
  resumed = 0;
  __asm__ volatile("adr x9, 1f\n str x9, [%0, #256]\n mov x9, sp\n str x9, [%0, #248]\n str x29, [%0, #232]\n str x19, [%0, #152]\n"
                   "str x20, [%0, #160]\n str x21, [%0, #168]\n str x22, [%0, #176]\n str x23, [%0, #184]\n str x24, [%0, #192]\n"
                   "str x25, [%0, #200]\n str x26, [%0, #208]\n str x27, [%0, #216]\n str x28, [%0, #224]\n str x18, [%0, #144]\n"
                   "mov x0, %1\n mov x1, %0\n blr %2\n 1:\n"
                   : : "r"(&rf), "r"(cpu), "r"(H->frame_resume)
                   : "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13", "x14", "x15", "x16", "x17", "x30", "cc", "memory");
  resumed = timer_fires;                          // the upcall ran before the first instruction after the resume
  cpu->irq_masked = 1;
  check(resumed == 1, "frame_resume of an unmasked frame with the line up takes the interrupt at once");

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
  check(psci_ok, "and gives it the vector in x1 and its xh_cpu in x2");

  // 6b. The kernel's own address space: a RAM page mapped at a kernel virtual address outside the window.
  const struct xh_info *info = H->info;
  check(vector->size >= offsetof(struct xh_vector, kernel_unmap) + sizeof vector->kernel_unmap && info->kva_size >= (1ull << 30) &&
        info->kva_base >= info->ram_virt + info->ram_size && info->kva_base + info->kva_size == 0x3c00000000ull, "the kernel address space reservation");
  uint64_t kpage = page_alloc(), kv = info->kva_base + 0x10000000;
  check(H->kernel_map(cpu, kv, 0x4000, XH_PROT_READ | XH_PROT_WRITE, kpage) == 0, "kernel_map");
  *(volatile uint64_t *)(uintptr_t)kv = 0x600dcafe;
  check(*(volatile uint64_t *)(uintptr_t)kva(kpage) == 0x600dcafe, "the mapping and the window show the same page");
  check(H->kernel_map(cpu, info->ram_virt, 0x4000, XH_PROT_READ, kpage) < 0 && H->kernel_map(cpu, kv, 0x4000, XH_PROT_READ, 0x1000) < 0,
        "kernel_map refuses addresses outside the reservation and outside RAM");
  check(H->kernel_unmap(cpu, kv, 0x4000) == 0 && H->kernel_map(cpu, kv + 0x4000, 0x4000, XH_PROT_READ, kpage) == 0 &&
        *(volatile uint64_t *)(uintptr_t)(kv + 0x4000) == 0x600dcafe, "kernel_unmap, and the page mapped again elsewhere");

  // 6c. A device page the kernel mapped itself: the fault's frame handed to mmio_emulate (what the
  // exception upcall does when its page-table entry points into the I/O aperture).
  static struct xh_frame mf;
  static uint32_t mcode[2];
  zero(&mf, sizeof mf);
  mcode[0] = 0xb9400023;                         // ldr w3, [x1]
  mcode[1] = 0xb8004422;                         // str w2, [x1], #4
  mf.pc = (uint64_t)(uintptr_t)mcode; mf.x[3] = ~0ull; mf.x[1] = 0x1000;
  check(vector->size >= offsetof(struct xh_vector, mmio_emulate) + sizeof vector->mmio_emulate &&
        H->mmio_emulate(cpu, &mf, 0x08000000 + 0xffe8) == 0 && (mf.x[3] >> 4 & 15) == 3 && mf.x[3] >> 32 == 0 &&
        mf.pc == (uint64_t)(uintptr_t)mcode + 4, "mmio_emulate: a load from GICD_PIDR2 on a frame, pc advanced");
  mf.x[2] = 1u << 9;                             // GICD_ISENABLER1: enable SPI 41
  check(H->mmio_emulate(cpu, &mf, 0x08000000 + 0x104) == 0 && mf.x[1] == 0x1004 && mf.pc == (uint64_t)(uintptr_t)mcode + 8 &&
        (H->mmio_read(cpu, 0x08000000 + 0x104, 4) & 1u << 9), "mmio_emulate: a store with writeback reaches the device");
  H->mmio_write(cpu, 0x08000000 + 0x184, 4, 1u << 9);
  mf.pc = (uint64_t)(uintptr_t)mcode;
  check(H->mmio_emulate(cpu, &mf, 0x1000) < 0 && mf.pc == (uint64_t)(uintptr_t)mcode, "mmio_emulate refuses an address outside the aperture");

  // 7. Devices and tasks.
  test_devices();
  uint32_t text[2] = { 0xd53bd063, 0xd53bd040 };                  // mrs x3, tpidrro_el0; mrs x0, tpidr_el0
  check(H->text_fixup(cpu, text, sizeof text) == 2 && text[0] == 0xd53bd043 && text[1] == 0xd2800000,
        "text_fixup: TPIDRRO_EL0 reads become TPIDR_EL0 reads, TPIDR_EL0 reads (the CPU number) become 0");
  uint64_t scratch = page_alloc();
  *(volatile uint64_t *)(uintptr_t)kva(scratch) = 5;
  if (H->info->features & XH_FEATURE_RAM_FIXED)
    // The RAM held for the session (xnuhost's default since 2026-11-13, kernel 0067): nothing goes back.
    check(H->ram_discard(cpu, scratch, 0x4000) == -1 && *(volatile uint64_t *)(uintptr_t)kva(scratch) == 5 &&
          H->ram_discard(cpu, scratch + 0x800, 0x4000) == -1 && *(volatile uint64_t *)(uintptr_t)kva(scratch) == 5,
          "ram_discard is refused while the RAM is held fixed (-EPERM), and the page keeps its data");
  else {
    check(H->ram_discard(cpu, scratch, 0x4000) == 0 && *(volatile uint64_t *)(uintptr_t)kva(scratch) == 0, "ram_discard gives pages back (they read as zero)");
    // A run of three pages and its neighbours: only the run reads zero; refused: unaligned, empty, outside the RAM.
    uint64_t run = page_alloc();
    page_alloc(); page_alloc(); page_alloc(); page_alloc();
    for (unsigned i = 0; i < 5; i++) *(volatile uint64_t *)(uintptr_t)kva(run + i * 0x4000ull) = 0x70 + i;
    int zeroed = H->ram_discard(cpu, run + 0x4000, 3 * 0x4000) == 0;
    for (unsigned i = 1; i < 4; i++) zeroed = zeroed && *(volatile uint64_t *)(uintptr_t)kva(run + i * 0x4000ull) == 0;
    check(zeroed && *(volatile uint64_t *)(uintptr_t)kva(run) == 0x70 && *(volatile uint64_t *)(uintptr_t)kva(run + 4 * 0x4000ull) == 0x74,
          "ram_discard of a run of pages: the run reads zero, the pages beside it keep their data");
    check(H->ram_discard(cpu, run + 0x800, 0x4000) == -22 && H->ram_discard(cpu, run, 0x800) == -22 && H->ram_discard(cpu, run, 0) == -22 &&
          H->ram_discard(cpu, H->info->ram_phys - 0x4000, 0x4000) == -22 &&
          H->ram_discard(cpu, H->info->ram_phys + H->info->ram_size + (1ull << 30), 0x4000) == -22 &&
          *(volatile uint64_t *)(uintptr_t)kva(run) == 0x70,
          "ram_discard refuses an unaligned range, an empty one and one outside the RAM (-EINVAL), and changes nothing");
  }
  check(H->wall_time(cpu) > 1700000000ull, "wall_time");
  test_tasks();
  test_jit();
  // boot-args "testkernel-bench" (`make bench-arm64`): the trap path's costs, after the checks.
  for (const char *c = args->command_line; *c; c++) {
    static const char want[] = "testkernel-bench";
    unsigned i = 0;
    while (want[i] && c[i] == want[i]) i++;
    if (!want[i]) { bench(c[i] == '=' && c[i + 1] >= '1' && c[i + 1] <= '9' ? (unsigned)(c[i + 1] - '0') : 1); break; }
  }

  put(failures ? "TESTKERNEL: FAILED\n" : "TESTKERNEL: all passed\n");
  H->halt(cpu, failures ? XH_HALT_PANIC : XH_HALT_OFF, (uint32_t)failures);
  for (;;) {}
}
