// SPDX-License-Identifier: MIT
// vCPUs: one host thread per CPU of the guest, running the kernel's code natively at EL0.
// The role of ArmCpu (https://github.com/UniXNU/ArmCpu 1604083) seen from the other side: where
// ArmCpu starts cores with PSCI, takes the timer on INTID 27 and sends SGIs, this file is the PSCI
// call's effect (a thread), the timer (a POSIX timer that signals the thread) and the SGI (a
// signal or a futex wake). Interrupt delivery is in docs/booter.md, "Interrupts".
#define _GNU_SOURCE
#include "xnuhost.h"
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

xh_vcpu g_vcpu[XH_MAX_CPUS];
unsigned g_ncpu;
struct xh_upcalls g_upcalls;
bool g_upcalls_set;
void (*xh_cpu_place)(int tid, unsigned vcpu);

// XH_HOST_TEST: the host unit tests (tests/test_xnuhost.c) take the stand-ins at the end, also on an arm64 Linux host.
#if defined(__aarch64__) && defined(__linux__) && !defined(XH_HOST_TEST)
#include <linux/futex.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <ucontext.h>

#define SIG_KICK  (SIGRTMIN + 1)      // another thread raised this CPU's interrupt line
#define SIG_TIMER (SIGRTMIN + 2)      // this CPU's timer is due
#define SIG_DUMP  (SIGRTMIN + 4)      // debugging: log where this CPU is (SIGQUIT to xnuhost)
#define SIGSTACK_BLOCK 0x100000ul     // per vCPU, aligned: [cpu pointer page][signal stack]
#define BOOT_STACK 0x40000ul
#define HOST_STACK 0x80000ul

uint64_t xh_sig_arena[2];             // [lo, hi) of the vCPU signal stacks, read by xh_sig_entry
static __thread xh_vcpu *self_vcpu;

void xh_sig_entry(int, siginfo_t *, void *);
void xh_upcall_tramp(void);
void xh_arch_enter(struct xh_cpu *, uint64_t entry, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t sp) __attribute__((noreturn));
void xh_cpu_signal(int signo, siginfo_t *info, void *context, struct xh_cpu *cpu);

uint64_t xh_counter(void) { uint64_t v; __asm__ volatile("mrs %0, cntvct_el0" : "=r"(v)); return v; }
uint64_t xh_counter_hz(void) { uint64_t v; __asm__("mrs %0, cntfrq_el0" : "=r"(v)); return v; }
xh_vcpu *xh_cpu_self(void) { return self_vcpu; }

static long futex(uint32_t *word, int op, uint32_t value, const struct timespec *timeout)
{
  return syscall(SYS_futex, word, op, value, timeout, NULL, 0);
}

// ---- the interrupt line -------------------------------------------------------------------------
static bool timer_due(const xh_vcpu *v)
{
  return __atomic_load_n(&v->timer_armed, __ATOMIC_ACQUIRE) && xh_counter() >= __atomic_load_n(&v->timer_deadline, __ATOMIC_ACQUIRE);
}

// For the GIC: the level of a CPU's virtual timer line (INTID 27).
bool xh_cpu_timer_due(unsigned index) { return index < g_ncpu && timer_due(&g_vcpu[index]); }

void xh_cpu_line(unsigned index, bool line)
{
  if (index >= g_ncpu) return;
  xh_vcpu *v = &g_vcpu[index];
  if (!line) {
    // Only the CPU itself lowers its line, and by recomputing: a store of 0 from another thread
    // could overwrite a 1 the timer signal just set.
    if (self_vcpu == v) {
      __atomic_store_n(&v->cpu.irq_pending, 0, __ATOMIC_RELEASE);
      if (xh_gic_pending(index)) __atomic_store_n(&v->cpu.irq_pending, 1, __ATOMIC_RELEASE);
    }
    return;
  }
  __atomic_store_n(&v->cpu.irq_pending, 1, __ATOMIC_RELEASE);
  if (self_vcpu == v || !v->started) return;
  if (__atomic_load_n(&v->idle, __ATOMIC_ACQUIRE)) futex(&v->cpu.irq_pending, FUTEX_WAKE_PRIVATE, 1, NULL);
  else syscall(SYS_tgkill, getpid(), v->tid, SIG_KICK);
}

void xh_cpu_timer_set(xh_vcpu *v, uint64_t deadline)
{
  struct itimerspec spec = { 0 };
  v->perf.timer_sets++;
  v->perf.timer_calls++;
  if (deadline == UINT64_MAX) {
    __atomic_store_n(&v->timer_armed, false, __ATOMIC_RELEASE);
    timer_settime(*(timer_t *)v->timer, 0, &spec, NULL);
    xh_cpu_line(v->cpu.index, false);
    return;
  }
  __atomic_store_n(&v->timer_deadline, deadline, __ATOMIC_RELEASE);
  __atomic_store_n(&v->timer_armed, true, __ATOMIC_RELEASE);
  uint64_t now = xh_counter();
  if (deadline <= now) {
    timer_settime(*(timer_t *)v->timer, 0, &spec, NULL);
    xh_cpu_line(v->cpu.index, xh_gic_pending(v->cpu.index));
    return;
  }
  // Counter ticks to nanoseconds without overflow for any realistic deadline.
  uint64_t hz = g_info.timebase_hz, ticks = deadline - now;
  uint64_t ns = ticks / hz * 1000000000ull + ticks % hz * 1000000000ull / hz;
  spec.it_value.tv_sec = (time_t)(ns / 1000000000ull);
  spec.it_value.tv_nsec = (long)(ns % 1000000000ull) + 1;
  if (spec.it_value.tv_nsec >= 1000000000l) { spec.it_value.tv_sec++; spec.it_value.tv_nsec -= 1000000000l; }
  timer_settime(*(timer_t *)v->timer, 0, &spec, NULL);
  xh_cpu_line(v->cpu.index, false);
}

void xh_cpu_idle(xh_vcpu *v)
{
  uint64_t began = xh_counter();
  __atomic_store_n(&v->idle, 1, __ATOMIC_RELEASE);
  while (!__atomic_load_n(&v->cpu.irq_pending, __ATOMIC_ACQUIRE)) {
    if (xh_stopping()) { pause(); continue; }
    // Sleep until woken, but no longer than to the timer's deadline: inside an emulated WFI the
    // timer's signal is blocked, so the deadline is watched here.
    struct timespec step = { 1, 0 };
    if (__atomic_load_n(&v->timer_armed, __ATOMIC_ACQUIRE)) {
      uint64_t now = xh_counter(), deadline = v->timer_deadline, hz = g_info.timebase_hz;
      if (deadline <= now) {
        if (xh_gic_pending(v->cpu.index)) { __atomic_store_n(&v->cpu.irq_pending, 1, __ATOMIC_RELEASE); break; }
        // due but not enabled or masked at the GIC: nothing to deliver; sleep the step
      } else if ((deadline - now) / hz == 0) {
        step.tv_sec = 0;
        step.tv_nsec = (long)((deadline - now) * 1000000000ull / hz) + 1000;
      }
    }
    futex(&v->cpu.irq_pending, FUTEX_WAIT_PRIVATE, 0, &step);
  }
  __atomic_store_n(&v->idle, 0, __ATOMIC_RELEASE);
  v->perf.ticks_idle += xh_counter() - began;
}

// ---- signals ------------------------------------------------------------------------------------
static struct fpsimd_context *fpsimd_of(ucontext_t *uc)
{
  struct _aarch64_ctx *head = (struct _aarch64_ctx *)uc->uc_mcontext.__reserved;
  for (size_t at = 0; at + sizeof *head <= sizeof uc->uc_mcontext.__reserved;) {
    head = (struct _aarch64_ctx *)((uint8_t *)uc->uc_mcontext.__reserved + at);
    if (head->magic == FPSIMD_MAGIC) return (struct fpsimd_context *)head;
    if (head->magic == 0 || head->size < sizeof *head) break;
    at += head->size;
  }
  return NULL;
}

static uint64_t esr_of(ucontext_t *uc)
{
  struct _aarch64_ctx *head;
  for (size_t at = 0; at + sizeof *head <= sizeof uc->uc_mcontext.__reserved;) {
    head = (struct _aarch64_ctx *)((uint8_t *)uc->uc_mcontext.__reserved + at);
    if (head->magic == ESR_MAGIC) return ((struct esr_context *)head)->esr;
    if (head->magic == 0 || head->size < sizeof *head) break;
    at += head->size;
  }
  return 0;
}

// Saves the interrupted kernel context in a frame on its own stack and makes the signal return
// into the kernel's upcall instead (xh_upcall_tramp), with interrupts masked.
static void divert(xh_vcpu *v, ucontext_t *uc, uint32_t kind, void *upcall, uint64_t far, uint64_t esr, const siginfo_t *info)
{
  mcontext_t *mc = &uc->uc_mcontext;
  struct xh_frame *frame = (struct xh_frame *)((mc->sp - 256 - sizeof *frame) & ~15ull);
  memcpy(frame->x, mc->regs, sizeof frame->x);
  frame->sp = mc->sp;
  frame->pc = mc->pc;
  frame->pstate = mc->pstate;
  struct fpsimd_context *fp = fpsimd_of(uc);
  if (fp) { frame->fpsr = fp->fpsr; frame->fpcr = fp->fpcr; memcpy(frame->v, fp->vregs, sizeof frame->v); }
  else { frame->fpsr = frame->fpcr = 0; memset(frame->v, 0, sizeof frame->v); }
  frame->irq_masked = v->cpu.irq_masked;
  frame->kind = kind;
  frame->far = far;
  frame->esr = esr;
  frame->signo = info ? (uint32_t)info->si_signo : 0;
  frame->sigcode = info ? (uint32_t)info->si_code : 0;
  if (kind == XH_FRAME_IRQ) v->acks_mark = __atomic_load_n(&v->stat_acks, __ATOMIC_RELAXED);
  v->cpu.irq_masked = 1;
  mc->regs[0] = (uint64_t)(uintptr_t)&v->cpu;
  mc->regs[1] = (uint64_t)(uintptr_t)frame;
  mc->regs[2] = (uint64_t)(uintptr_t)upcall;
  mc->sp = (uint64_t)(uintptr_t)frame;
  mc->pc = (uint64_t)(uintptr_t)xh_upcall_tramp;
  v->stat_irqs++;
}

static void maybe_interrupt(xh_vcpu *v, ucontext_t *uc)
{
  if (v->cpu.irq_pending && !v->cpu.irq_masked && !v->cpu.in_host && g_upcalls.irq)
    divert(v, uc, XH_FRAME_IRQ, g_upcalls.irq, 0, 0, NULL);
}

static __attribute__((noreturn)) void host_fault(int signo, siginfo_t *info, ucontext_t *uc, const char *what)
{
  xh_log_signal(what, (uint64_t)signo, (uint64_t)(uintptr_t)info->si_addr);
  xh_log_signal("  pc, sp", uc->uc_mcontext.pc, uc->uc_mcontext.sp);
  xh_log_signal("  x30, esr", uc->uc_mcontext.regs[30], esr_of(uc));
  signal(signo, SIG_DFL);
  raise(signo);
  _exit(5);
}

void xh_cpu_signal(int signo, siginfo_t *info, void *context, struct xh_cpu *cpu)
{
  ucontext_t *uc = context;
  xh_vcpu *v = (xh_vcpu *)cpu;
  int saved_errno = errno;

  if (signo == SIG_DUMP) {
    if (!v) return;
    mcontext_t *m = &uc->uc_mcontext;
    xh_log_signal("cpu state: index, pc", v->cpu.index, m->pc);
    xh_log_signal("  x30, sp", m->regs[30], m->sp);
    xh_log_signal("  irq_masked, irq_pending", v->cpu.irq_masked, v->cpu.irq_pending);
    xh_log_signal("  in_host, kernel tpidr", v->cpu.in_host, v->cpu.kernel_tpidr);
    if (v->cpu.in_host) {
      // Inside a host call: xnuhost's own frame records (symbolize against build/android/xnuhost), which say
      // which call the vCPU is in (idle, thread_run, a task order, a device).
      uint64_t fp = m->regs[29];
      for (int i = 0; i < 12 && fp && !(fp & 7); i++) {
        unsigned char vec;
        if (mincore((void *)(uintptr_t)(fp & ~0xfffull), 0x1000, &vec)) break;
        uint64_t *rec = (uint64_t *)(uintptr_t)fp;
        xh_log_signal("  host frame: fp, lr", fp, rec[1]);
        if (rec[0] <= fp) break;
        fp = rec[0];
      }
    }
    if (!v->cpu.in_host) {
      // Kernel code: the argument and callee-saved registers too (a loop's counters and objects are in them).
      for (int i = 0; i < 30; i += 2) xh_log_signal(i < 10 ? "  x(even), x(odd) from x0" : "  x(even), x(odd)", m->regs[i], m->regs[i + 1]);
    }
    if (!v->cpu.in_host) {
      // The kernel's frame records, as far as they stay inside the RAM window or the kernel's mappings.
      uint64_t fp = m->regs[29];
      for (int i = 0; i < 24 && fp >= g_info.ram_virt && fp < 0x3c00000000ull && !(fp & 7); i++) {
        unsigned char vec;
        if (mincore((void *)(uintptr_t)(fp & ~0xfffull), 0x1000, &vec)) break;
        uint64_t *rec = (uint64_t *)(uintptr_t)fp;
        xh_log_signal("  frame: fp, lr", fp, rec[1]);
        if (rec[0] <= fp) break;
        fp = rec[0];
      }
    }
    errno = saved_errno;
    return;
  }
  if (signo == SIG_KICK || signo == SIG_TIMER) {
    if (!v) return;
    if (signo == SIG_TIMER) __atomic_store_n(&v->cpu.irq_pending, 1, __ATOMIC_RELEASE);
    maybe_interrupt(v, uc);
    errno = saved_errno;
    return;
  }
  // A synchronous trap. In host context (ours or a non-vCPU thread's) it is a bug of xnuhost.
  if (!v || v->cpu.in_host) host_fault(signo, info, uc, v ? "fault inside a host call: signal, address" : "fault on a host thread: signal, address");

  mcontext_t *mc = &uc->uc_mcontext;
  xh_regs regs;
  memcpy(regs.x, mc->regs, sizeof regs.x);
  regs.sp = mc->sp; regs.pc = mc->pc; regs.pstate = mc->pstate;
  bool handled = false;
  uint64_t far = (uint64_t)(uintptr_t)info->si_addr;
  if (signo == SIGSEGV || signo == SIGBUS) {
    uint64_t io = (uint64_t)(uintptr_t)g_mem.io_host;
    if (far >= io && far - io < g_mem.io_size && regs.pc != far) {
      v->stat_mmio++;
      handled = xh_mmio_emulate(&regs, *(uint32_t *)(uintptr_t)regs.pc, g_mem.io_phys + (far - io));
    }
  } else if (signo == SIGILL) {
    // in_host covers the emulation: PSCI CPU_ON and WFI call into the machine layer.
    v->cpu.in_host = 1;
    handled = xh_sysreg_emulate(v, &regs, *(uint32_t *)(uintptr_t)regs.pc);
    v->cpu.in_host = 0;
  }
  if (handled) {
    memcpy(mc->regs, regs.x, sizeof regs.x);
    mc->sp = regs.sp; mc->pc = regs.pc;
    maybe_interrupt(v, uc);
    errno = saved_errno;
    return;
  }
  if (g_upcalls.exception) {
    // A SIGTRAP's esr_context is never its own: Linux sends every SIGTRAP (brk, single step, watchpoint) through
    // send_user_sigtrap(), which leaves thread.fault_code alone, and fault_code is written only when a fault
    // signal is delivered. On a vCPU thread it is the last software-TLB fill's data abort, and the kernel took a
    // UBSan soft trap (brk #UBSAN_SOFT_TRAP_SIGNED_OF in tcp_input's compute_iaj_meat) for a data abort at pc:
    // "Unexpected fault in kernel physical aperture" (phone, 2026-11-12 23:15). Without a syndrome the kernel
    // reads the brk at pc (hosted_kernel_esr).
    divert(v, uc, XH_FRAME_EXCEPTION, g_upcalls.exception, far, signo == SIGTRAP ? 0 : esr_of(uc), info);
    errno = saved_errno;
    return;
  }
  xh_log_signal("kernel fault with no exception upcall: signal, address", (uint64_t)signo, far);
  xh_log_signal("  pc, sp", mc->pc, mc->sp);
  xh_log_signal("  x30, esr", mc->regs[30], esr_of(uc));
  xh_log_signal("  instruction, cpu", regs.pc >= (uint64_t)(uintptr_t)g_mem.host && regs.pc - (uint64_t)(uintptr_t)g_mem.host < g_mem.size ? *(uint32_t *)(uintptr_t)regs.pc : 0, v->cpu.index);
  xh_stop(XH_HALT_PANIC, 0, "kernel fault");
  for (;;) pause();
}

// ---- frame_resume: restore a full kernel context --------------------------------------------------
void xh_arch_sigreturn(struct xh_cpu *, void *sigframe, uint64_t tpidr, uint64_t unused, uint32_t masked) __attribute__((noreturn));
static sigset_t vcpu_mask;            // the signal mask vCPU threads run with

void xh_cpu_frame_resume(xh_vcpu *v, const struct xh_frame *frame)
{
  // Linux's own signal return restores every register at once; build the frame it expects
  // (arch/arm64/kernel/signal.c: struct rt_sigframe = siginfo, ucontext) on the host stack.
  struct { siginfo_t info; ucontext_t uc; } __attribute__((aligned(16))) sf;
  memset(&sf.info, 0, sizeof sf.info);
  memset(&sf.uc, 0, offsetof(ucontext_t, uc_mcontext));
  sf.uc.uc_stack.ss_sp = v->sig_stack + 0x10000;
  sf.uc.uc_stack.ss_size = SIGSTACK_BLOCK - 0x10000;
  sf.uc.uc_stack.ss_flags = 0;
  memcpy(&sf.uc.uc_sigmask, &vcpu_mask, sizeof vcpu_mask);
  mcontext_t *mc = &sf.uc.uc_mcontext;
  mc->fault_address = 0;
  memcpy(mc->regs, frame->x, sizeof frame->x);
  mc->sp = frame->sp;
  mc->pc = frame->pc;
  mc->pstate = frame->pstate & 0xf0000000ull;
  struct fpsimd_context *fp = (struct fpsimd_context *)mc->__reserved;
  fp->head.magic = FPSIMD_MAGIC;
  fp->head.size = sizeof *fp;
  fp->fpsr = frame->fpsr;
  fp->fpcr = frame->fpcr;
  memcpy(fp->vregs, frame->v, sizeof frame->v);
  struct _aarch64_ctx *end = (struct _aarch64_ctx *)(fp + 1);
  end->magic = 0;
  end->size = 0;
  // From here to the signal return nothing may divert this context again.
  sigset_t all;
  sigfillset(&all);
  pthread_sigmask(SIG_SETMASK, &all, NULL);
  // An exception return with the line up takes the interrupt at once. Here: a kick to ourselves,
  // which stays pending until the signal return opens the mask, and then diverts the resumed
  // context into the interrupt upcall. Not after an interrupt upcall that acknowledged nothing (a
  // kernel with no handler for this CPU yet): that would only come straight back.
  if (!frame->irq_masked && __atomic_load_n(&v->cpu.irq_pending, __ATOMIC_ACQUIRE) &&
      (frame->kind != XH_FRAME_IRQ || __atomic_load_n(&v->stat_acks, __ATOMIC_RELAXED) != v->acks_mark)) {
    v->stat_reraised++;
    syscall(SYS_tgkill, getpid(), v->tid, SIG_KICK);
  }
  xh_arch_sigreturn(&v->cpu, &sf, v->cpu.kernel_tpidr, 0, frame->irq_masked);
}

// ---- threads ------------------------------------------------------------------------------------
static void *vcpu_main(void *arg)
{
  xh_vcpu *v = arg;
  self_vcpu = v;
  v->tid = (int)syscall(SYS_gettid);
  uint64_t tpidr, x18;
  __asm__("mrs %0, tpidr_el0" : "=r"(tpidr));
  __asm__("mov %0, x18" : "=r"(x18));
  v->cpu.host_tpidr = tpidr;
  v->cpu.host_x18 = x18;
  v->cpu.host_sp = (uint64_t)(uintptr_t)(v->host_stack + HOST_STACK);
  v->cpu.in_host = 1;
  v->cpu.irq_masked = 1;              // as a CPU leaves reset: DAIF masked
  *(struct xh_cpu **)v->sig_stack = &v->cpu;
  stack_t ss = { .ss_sp = v->sig_stack + 0x10000, .ss_size = SIGSTACK_BLOCK - 0x10000 };
  sigaltstack(&ss, NULL);
  static timer_t timers[XH_MAX_CPUS];
  struct sigevent ev = { .sigev_notify = SIGEV_THREAD_ID, .sigev_signo = SIG_TIMER };
  ev.sigev_notify_thread_id = v->tid;
  if (timer_create(CLOCK_MONOTONIC, &ev, &timers[v->cpu.index])) { LOGE("cpu%u: timer_create: %s", v->cpu.index, strerror(errno)); xh_stop(XH_HALT_PANIC, 0, "no timer"); return NULL; }
  v->timer = &timers[v->cpu.index];
  char name[16];
  snprintf(name, sizeof name, "xnu-cpu%u", v->cpu.index);
  pthread_setname_np(pthread_self(), name);
  if (xh_cpu_place) xh_cpu_place(0, v->cpu.index);
  pthread_sigmask(SIG_SETMASK, &vcpu_mask, NULL);
  uint64_t entry = v->entry, a0 = v->arg0, a1 = v->arg1, a2 = v->arg2;
  if (!entry) { entry = (uint64_t)(uintptr_t)g_upcalls.cpu_entry; a1 = a0; a0 = (uint64_t)(uintptr_t)&v->cpu; }
  LOGI("cpu%u: entering the kernel at 0x%llx", v->cpu.index, (unsigned long long)entry);
  v->started = true;
  xh_arch_enter(&v->cpu, entry, a0, a1, a2, (uint64_t)(uintptr_t)(v->boot_stack + BOOT_STACK));
}

int xh_cpu_init(unsigned ncpu)
{
  if (ncpu == 0 || ncpu > XH_MAX_CPUS) return -1;
  // One arena for all signal stacks, 1 MiB aligned, so xh_sig_entry finds the CPU from sp.
  size_t arena = SIGSTACK_BLOCK * (ncpu + 1);
  uint8_t *raw = mmap(NULL, arena, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  uint8_t *stacks = mmap(NULL, (BOOT_STACK + HOST_STACK) * ncpu, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (raw == MAP_FAILED || stacks == MAP_FAILED) return -1;
  uint8_t *aligned = (uint8_t *)(((uintptr_t)raw + SIGSTACK_BLOCK - 1) & ~(SIGSTACK_BLOCK - 1));
  xh_sig_arena[0] = (uint64_t)(uintptr_t)aligned;
  xh_sig_arena[1] = (uint64_t)(uintptr_t)aligned + SIGSTACK_BLOCK * ncpu;
  for (unsigned i = 0; i < ncpu; i++) {
    memset(&g_vcpu[i], 0, sizeof g_vcpu[i]);
    g_vcpu[i].cpu.index = i;
    g_vcpu[i].sig_stack = aligned + SIGSTACK_BLOCK * i;
    g_vcpu[i].boot_stack = stacks + (BOOT_STACK + HOST_STACK) * i;
    g_vcpu[i].host_stack = g_vcpu[i].boot_stack + BOOT_STACK;
  }
  g_ncpu = ncpu;

  // vCPU threads take the two interrupt signals and the synchronous traps; everything else
  // (SIGTERM, SIGINT, ...) is the main thread's.
  sigfillset(&vcpu_mask);
  static const int open_signals[] = { SIGSEGV, SIGBUS, SIGILL, SIGTRAP, SIGFPE, SIGSYS, SIGABRT };
  for (unsigned i = 0; i < sizeof open_signals / sizeof *open_signals; i++) sigdelset(&vcpu_mask, open_signals[i]);
  sigdelset(&vcpu_mask, SIG_KICK);
  sigdelset(&vcpu_mask, SIG_TIMER);
  sigdelset(&vcpu_mask, SIG_DUMP);
  struct sigaction sa;
  memset(&sa, 0, sizeof sa);
  sa.sa_sigaction = xh_sig_entry;
  sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
  sigfillset(&sa.sa_mask);
  static const int handled[] = { SIGSEGV, SIGBUS, SIGILL, SIGTRAP, SIGFPE };
  for (unsigned i = 0; i < sizeof handled / sizeof *handled; i++) sigaction(handled[i], &sa, NULL);
  sigaction(SIG_KICK, &sa, NULL);
  sigaction(SIG_TIMER, &sa, NULL);
  sigaction(SIG_DUMP, &sa, NULL);
  return 0;
}

void xh_cpu_dump_all(void)
{
  for (unsigned i = 0; i < g_ncpu; i++) if (g_vcpu[i].started) syscall(SYS_tgkill, getpid(), g_vcpu[i].tid, SIG_DUMP);
}

int xh_cpu_start(unsigned index, uint64_t entry, uint64_t a0, uint64_t a1, uint64_t a2)
{
  static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
  if (index >= g_ncpu) return -1;
  xh_vcpu *v = &g_vcpu[index];
  pthread_mutex_lock(&lock);
  if (v->thread) { pthread_mutex_unlock(&lock); return -2; }      // already on
  if (!entry && !g_upcalls.cpu_entry) { pthread_mutex_unlock(&lock); return -1; }
  v->entry = entry; v->arg0 = a0; v->arg1 = a1; v->arg2 = a2;
  // The new thread starts with every signal blocked and opens its own mask.
  sigset_t all, old;
  sigfillset(&all);
  pthread_sigmask(SIG_SETMASK, &all, &old);
  int rc = pthread_create(&v->thread, NULL, vcpu_main, v);
  pthread_sigmask(SIG_SETMASK, &old, NULL);
  pthread_mutex_unlock(&lock);
  return rc ? -1 : 0;
}

#else   // hosts that cannot run the kernel: unit-test builds

uint64_t xh_counter(void) { struct timespec ts; clock_gettime(CLOCK_MONOTONIC, &ts); return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec; }
uint64_t xh_counter_hz(void) { return 1000000000ull; }
xh_vcpu *xh_cpu_self(void) { return NULL; }
bool xh_cpu_timer_due(unsigned index) { return index < g_ncpu && g_vcpu[index].timer_armed && xh_counter() >= g_vcpu[index].timer_deadline; }
void xh_cpu_line(unsigned index, bool line) { if (index < g_ncpu) g_vcpu[index].cpu.irq_pending = line; }
void xh_cpu_timer_set(xh_vcpu *v, uint64_t deadline) { v->timer_armed = deadline != UINT64_MAX; v->timer_deadline = deadline; }
void xh_cpu_idle(xh_vcpu *v) { (void)v; }
int xh_cpu_init(unsigned ncpu) { g_ncpu = ncpu; for (unsigned i = 0; i < ncpu; i++) g_vcpu[i].cpu.index = i; return 0; }
void xh_cpu_dump_all(void) {}
int xh_cpu_start(unsigned index, uint64_t entry, uint64_t a0, uint64_t a1, uint64_t a2) { (void)entry; (void)a0; (void)a1; (void)a2; if (index >= g_ncpu) return -1; if (g_vcpu[index].started) return -2; g_vcpu[index].started = true; return 0; }

#endif
