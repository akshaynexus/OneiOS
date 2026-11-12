// SPDX-License-Identifier: MIT
// The stub: the only Linux code inside an iOS task process. Freestanding, position independent,
// linked into a flat blob (stub.ld) that xnuhost's task factory copies to the stub region.
// It does three things:
//   - xs_entry: turns a forked child into an empty address space with nothing but this region,
//     installs the signal handlers and the seccomp filter, and becomes the service thread;
//   - the service thread: carries out xnuhost's orders (map, unmap, protect, new thread);
//   - xs_handler: every system call, fault and forced stop of an iOS thread lands here as a signal;
//     it hands the register state to xnuhost through the thread's trap page and waits to be resumed.
// The design is gVisor's systrap platform and User-Mode Linux's skas mode; the code is new.
#include "stub_abi.h"

// Every symbol is local to the blob: no GOT, so no absolute address that would need relocating.
#pragma GCC visibility push(hidden)

#define NR_futex 98
#define NR_exit 93
#define NR_exit_group 94
#define NR_sigaltstack 132
#define NR_rt_sigaction 134
#define NR_rt_sigreturn 139
#define NR_setresuid 147
#define NR_setresgid 149
#define NR_prctl 167
#define NR_getpid 172
#define NR_gettid 178
#define NR_munmap 215
#define NR_clone 220
#define NR_mmap 222
#define NR_mprotect 226
#define NR_seccomp 277

#define FUTEX_WAIT 0
#define FUTEX_WAKE 1
#define PROT_RW 3
#define MAP_SHARED 1
#define MAP_PRIVATE 2
#define MAP_FIXED 0x10
#define MAP_ANONYMOUS 0x20
#define SIGILL 4
#define SIGTRAP 5
#define SIGBUS 7
#define SIGFPE 8
#define SIGSEGV 11
#define SIGSYS 31
#define SA_SIGINFO 4u
#define SA_RESTORER 0x04000000u
#define SA_ONSTACK 0x08000000u
#define CLONE_THREAD_FLAGS 0x00250f00u     // VM | FS | FILES | SIGHAND | THREAD | SYSVSEM | CHILD_CLEARTID
#define PR_SET_NO_NEW_PRIVS 38
#define PR_PAC_SET_ENABLED_KEYS 60
#define FPSIMD_MAGIC 0x46508001u
#define ESR_MAGIC 0x45535201u
#define SVE_MAGIC 0x53564501u

extern char __xs_start[];                  // the blob's first byte = the region's base

static inline __attribute__((always_inline)) long sys6(long nr, long a, long b, long c, long d, long e, long f)
{
  register long x8 __asm__("x8") = nr;
  register long x0 __asm__("x0") = a;
  register long x1 __asm__("x1") = b;
  register long x2 __asm__("x2") = c;
  register long x3 __asm__("x3") = d;
  register long x4 __asm__("x4") = e;
  register long x5 __asm__("x5") = f;
  __asm__ volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5) : "memory", "cc");
  return x0;
}
#define sys(nr, ...) sys6_(nr, ##__VA_ARGS__, 0, 0, 0, 0, 0, 0)
#define sys6_(nr, a, b, c, d, e, f, ...) sys6(nr, (long)(a), (long)(b), (long)(c), (long)(d), (long)(e), (long)(f))

static void copy(void *to, const void *from, unsigned long n)
{
  volatile unsigned char *d = to;
  const volatile unsigned char *s = from;
  while (n--) *d++ = *s++;
}

static void wait_for(uint32_t *word)
{
  while (__atomic_load_n(word, __ATOMIC_ACQUIRE) == 0) sys(NR_futex, word, FUTEX_WAIT, 0, 0);
}

static void wake(uint32_t *word)
{
  __atomic_store_n(word, 1, __ATOMIC_RELEASE);
  sys(NR_futex, word, FUTEX_WAKE, 0x7fffffff);
}

static inline uint64_t counter(void) { uint64_t v; __asm__ volatile("isb\n mrs %0, cntvct_el0" : "=r"(v) : : "memory"); return v; }

// stub_abi.h, the hand-off: store the word, then wake the other side only if it said it sleeps.
static void post(uint32_t *word, uint32_t *sleeping)
{
  __atomic_store_n(word, 1, __ATOMIC_RELEASE);
  __atomic_thread_fence(__ATOMIC_SEQ_CST);
  if (__atomic_load_n(sleeping, __ATOMIC_RELAXED)) sys(NR_futex, word, FUTEX_WAKE, 0x7fffffff);
}

// Spins up to `ticks` for *word to become non-zero, then sleeps for it. Returns 1 if it slept.
static uint32_t await(uint32_t *word, uint32_t *sleeping, uint64_t ticks)
{
  if (__atomic_load_n(word, __ATOMIC_ACQUIRE)) return 0;
  if (ticks) {
    uint64_t began = counter();
    while (!__atomic_load_n(word, __ATOMIC_ACQUIRE)) {
      if (counter() - began > ticks) break;
      __asm__ volatile("yield");
    }
    if (__atomic_load_n(word, __ATOMIC_ACQUIRE)) return 0;
  }
  __atomic_store_n(sleeping, 1, __ATOMIC_RELAXED);
  __atomic_thread_fence(__ATOMIC_SEQ_CST);
  while (__atomic_load_n(word, __ATOMIC_ACQUIRE) == 0) sys(NR_futex, word, FUTEX_WAIT, 0, 0);
  __atomic_store_n(sleeping, 0, __ATOMIC_RELAXED);
  return 1;
}

// ---- the signal frame, as the arm64 kernel lays it out ------------------------------------------
struct k_siginfo { int signo, err, code, pad; uint64_t addr; uint32_t syscall, arch; };
struct k_sigcontext { uint64_t fault_address, regs[31], sp, pc, pstate; uint8_t reserved[4096] __attribute__((aligned(16))); };
struct k_ucontext { uint64_t flags, link; uint64_t ss_sp; int ss_flags, pad; uint64_t ss_size; uint8_t sigmask[128]; struct k_sigcontext mc; };
struct k_record { uint32_t magic, size; };

static uint8_t *find_record(struct k_sigcontext *mc, uint32_t magic)
{
  for (unsigned long at = 0; at + 8 <= sizeof mc->reserved;) {
    struct k_record *r = (struct k_record *)(mc->reserved + at);
    if (r->magic == magic) return (uint8_t *)r;
    if (r->magic == 0 || r->size < 8) break;
    at += r->size;
  }
  return 0;
}

void xs_handler(int signo, struct k_siginfo *info, struct k_ucontext *uc);
void xs_restorer(void);
__asm__(".text\n.globl xs_restorer\n.type xs_restorer,%function\nxs_restorer:\n  mov x8, #139\n  svc #0\n");

void xs_handler(int signo, struct k_siginfo *info, struct k_ucontext *uc)
{
  // The handler runs on the thread's own signal stack, which is inside its area.
  struct xs_trap_page *tp = (struct xs_trap_page *)((unsigned long)__builtin_frame_address(0) & ~(XS_AREA_SIZE - 1));
  struct xs_config *config = (struct xs_config *)(__xs_start + XS_CONFIG_OFF);
  struct k_sigcontext *mc = &uc->mc;
  struct xh_user_state *st = &tp->state;
  struct xh_trap *trap = &tp->trap;

  tp->t_enter = counter();
  copy(st->x, mc->regs, sizeof st->x);
  st->sp = mc->sp;
  st->pc = mc->pc;
  st->pstate = mc->pstate;
  __asm__ volatile("mrs %0, tpidr_el0" : "=r"(st->tsd));
  uint8_t *fp = find_record(mc, FPSIMD_MAGIC);
  if (fp) {
    copy(&st->fpsr, fp + 8, 4);
    copy(&st->fpcr, fp + 12, 4);
    copy(st->v, fp + 16, sizeof st->v);
  }
  uint8_t *esr = find_record(mc, ESR_MAGIC);
  trap->signo = (uint32_t)signo;
  trap->esr = 0;
  if (esr) copy(&trap->esr, esr + 8, 8);
  trap->far = 0;
  trap->svc = trap->access = trap->insn = trap->exit_status = 0;

  if (!tp->booted) {
    tp->booted = 1;
    trap->kind = XH_TRAP_BOOT;
  } else if (signo == SIGSYS) {
    trap->kind = XH_TRAP_SYSCALL;
    trap->far = st->pc - 4;
    trap->insn = *(volatile uint32_t *)(st->pc - 4);
    if ((trap->insn & 0xffe0001f) == 0xd4000001) trap->svc = (trap->insn >> 5) & 0xffff;
  } else if (signo == SIGSEGV || signo == SIGBUS) {
    trap->kind = XH_TRAP_FAULT;
    trap->far = info->addr;
    unsigned ec = (unsigned)(trap->esr >> 26) & 0x3f;
    if (ec == 0x20 || ec == 0x21) trap->access = XH_PROT_EXEC;
    else if (ec == 0x24 || ec == 0x25) trap->access = (trap->esr & 0x40) ? XH_PROT_WRITE : XH_PROT_READ;
    else if (info->addr == st->pc) trap->access = XH_PROT_EXEC;
  } else if (signo == SIGILL) {
    trap->kind = XH_TRAP_ILLEGAL;
    trap->far = st->pc;
    trap->insn = *(volatile uint32_t *)st->pc;
  } else if (signo == SIGTRAP) {
    trap->kind = XH_TRAP_BRK;
    trap->far = st->pc;
    // Linux's SIGTRAP carries no syndrome of its own: its esr_context is the task's last fault's (cpu.c says
    // why), which the kernel would report as that fault. The instruction gives the kernel the brk's immediate.
    trap->esr = 0;
    trap->insn = *(volatile uint32_t *)st->pc;
  } else if (signo == SIGFPE) {
    trap->kind = XH_TRAP_FPE;
    trap->far = st->pc;
  } else if (signo == (int)config->kick_signal) {
    trap->kind = XH_TRAP_INTERRUPT;
  } else {
    trap->kind = XH_TRAP_ILLEGAL;
  }

  __atomic_store_n(&tp->to_task, 0, __ATOMIC_RELEASE);
  if (trap->kind == XH_TRAP_BOOT) wake(&tp->to_host);      // thread_create waits for this one in a timed futex
  else post(&tp->to_host, &tp->host_sleeping);
  tp->slept = await(&tp->to_task, &tp->task_sleeping, tp->spin_ticks);
  tp->t_woken = counter();
  if (tp->command == XS_EXIT) sys(NR_exit, 0);

  // Maps, and unmaps given while none of the task's threads ran, that the kernel gave for this task since its last
  // order (task.c, map-defer): carried out here, in their order, before the thread runs again, while xnuhost holds the
  // task's order lock; then to_host is woken for them.
  uint32_t nops = tp->nops;
  if (nops) {
    struct xs_op *ops = (struct xs_op *)((uint8_t *)tp + XS_OPS_OFF);
    for (uint32_t i = 0; i < nops && i < XS_THREAD_OPS; i++) {
      long r = ops[i].op == XS_OP_MAP ? sys(NR_mmap, ops[i].va, ops[i].size, ops[i].prot & 7, MAP_SHARED | MAP_FIXED, config->ram_fd, ops[i].offset) :
               ops[i].op == XS_OP_UNMAP ? sys(NR_munmap, ops[i].va, ops[i].size) : -38;
      ops[i].result = r > 0 ? 0 : r;
    }
    tp->nops = 0;
    __atomic_store_n(&tp->ops_done, 1, __ATOMIC_RELEASE);
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
    if (__atomic_load_n(&tp->host_sleeping, __ATOMIC_RELAXED)) sys(NR_futex, &tp->to_host, FUTEX_WAKE, 0x7fffffff);
  }

  if (tp->pac_set) {
    tp->pac_set = 0;
    sys(NR_prctl, PR_PAC_SET_ENABLED_KEYS, 0xf, tp->pac_enabled & 0xf, 0, 0);
  }
  copy(mc->regs, st->x, sizeof st->x);
  mc->sp = st->sp;
  mc->pc = st->pc;
  mc->pstate = st->pstate & 0xf0000000ull;        // NZCV; EL0t, interrupts unmasked
  if (fp) {
    copy(fp + 8, &st->fpsr, 4);
    copy(fp + 12, &st->fpcr, 4);
    copy(fp + 16, st->v, sizeof st->v);
  }
  // A thread with live SVE state (none of iOS's code has any, but the host's may) is restored from
  // the SVE record instead: V[n] is the low 128 bits of Z[n]. Record: head, u16 vl, ..., then Z0..Z31.
  uint8_t *sve = find_record(mc, SVE_MAGIC);
  if (sve) {
    uint32_t size = ((struct k_record *)sve)->size, vl = *(uint16_t *)(sve + 8);
    if (vl >= 16 && size >= 16 + 32 * vl)
      for (unsigned n = 0; n < 32; n++) copy(sve + 16 + n * vl, &st->v[n], 16);
  }
  __asm__ volatile("msr tpidr_el0, %0" : : "r"(st->tsd));
}

// ---- threads ------------------------------------------------------------------------------------
// A new thread starts here on its own signal stack: it registers that stack and stops with a
// breakpoint, which the handler reports as XH_TRAP_BOOT. It never comes back from the handler:
// xnuhost replaces its whole register state.
__attribute__((noreturn)) void xs_thread_start(struct xs_trap_page *tp);
void xs_thread_start(struct xs_trap_page *tp)
{
  struct { uint64_t sp; int flags, pad; uint64_t size; } stack = { (uint64_t)tp + XS_SIGSTACK_OFF, 0, 0, XS_SIGSTACK_SIZE };
  sys(NR_sigaltstack, &stack, 0);
  __atomic_store_n(&tp->tid, (uint32_t)sys(NR_gettid), __ATOMIC_RELEASE);
  __asm__ volatile("brk #0xf00");
  for (;;) sys(NR_exit, 1);
}

long xs_clone(unsigned long flags, unsigned long stack, uint32_t *tid, struct xs_trap_page *tp);
__asm__(".text\n.globl xs_clone\n.type xs_clone,%function\n"
        "xs_clone:\n"
        "  mov x9, x3\n"                  // the child's argument survives in a register
        "  mov x4, x2\n"                  // ctid (arm64 order: flags, stack, ptid, tls, ctid)
        "  mov x3, #0\n"
        "  mov x8, #220\n"
        "  svc #0\n"
        "  cbz x0, 1f\n"
        "  ret\n"
        "1:\n"
        "  mov x29, #0\n"
        "  mov x30, #0\n"
        "  mov x0, x9\n"
        "  b xs_thread_start\n");

static long new_thread(struct xs_config *config, uint64_t index, uint64_t offset)
{
  if (index >= XS_MAX_THREADS) return -22;
  unsigned long area = config->base + XS_THREADS_OFF + index * XS_AREA_SIZE;
  long r = sys(NR_mmap, area, XS_SLOT_SIZE, PROT_RW, MAP_SHARED | MAP_FIXED, config->trap_fd, offset);
  if (r < 0) return r;
  r = sys(NR_mmap, area + XS_SIGSTACK_OFF, XS_SIGSTACK_SIZE, PROT_RW, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
  if (r < 0) return r;
  struct xs_trap_page *tp = (struct xs_trap_page *)area;
  r = xs_clone(CLONE_THREAD_FLAGS, area + XS_SIGSTACK_OFF + XS_SIGSTACK_SIZE, &tp->tid, tp);
  return r < 0 ? r : 0;
}

// ---- the service thread -------------------------------------------------------------------------
static __attribute__((noreturn)) void service(struct xs_config *config, struct xs_service_page *sp)
{
  for (;;) {
    wait_for(&sp->request);
    for (uint32_t i = 0; i < sp->count && i < XS_OPS_MAX; i++) {
      struct xs_op *op = &sp->ops[i];
      if (op->op == XS_OP_MAP)
        op->result = sys(NR_mmap, op->va, op->size, op->prot & 7, MAP_SHARED | MAP_FIXED, config->ram_fd, op->offset);
      else if (op->op == XS_OP_UNMAP) op->result = sys(NR_munmap, op->va, op->size);
      else if (op->op == XS_OP_PROTECT) op->result = sys(NR_mprotect, op->va, op->size, op->prot & 7);
      else if (op->op == XS_OP_CLONE) op->result = new_thread(config, op->va, op->offset);
      else if (op->op == XS_OP_EXIT) sys(NR_exit_group, 0);
      else op->result = -38;
      if (op->op == XS_OP_MAP && op->result > 0) op->result = 0;
    }
    __atomic_store_n(&sp->request, 0, __ATOMIC_RELEASE);
    wake(&sp->done);
  }
}

__attribute__((noreturn)) void xs_main(void);
void xs_main(void)
{
  struct xs_config *config = (struct xs_config *)(__xs_start + XS_CONFIG_OFF);
  unsigned long base = config->base, end = base + XS_REGION_SIZE;

  // Everything the child inherited from the factory goes: the task's address space is XNU's to fill.
  sys(NR_munmap, 0, base);
  static const unsigned char top_bits[] = { 52, 48, 47, 42, 39, 36 };
  for (unsigned i = 0; i < sizeof top_bits; i++) {
    unsigned long top = 1ul << top_bits[i];
    if (top > end && sys(NR_munmap, end, top - end) == 0) break;
  }

  struct xs_service_page *sp = (struct xs_service_page *)(base + XS_SERVICE_OFF);
  long r = sys(NR_mmap, sp, XS_SLOT_SIZE, PROT_RW, MAP_SHARED | MAP_FIXED, config->trap_fd, (uint64_t)config->service_slot * XS_SLOT_SIZE);
  if (r < 0) sys(NR_exit_group, 70);

  struct { void (*handler)(int, struct k_siginfo *, struct k_ucontext *); unsigned long flags; void (*restorer)(void); uint64_t mask; } action;
  action.handler = xs_handler;
  action.flags = SA_SIGINFO | SA_ONSTACK | SA_RESTORER;
  action.restorer = xs_restorer;
  action.mask = ~0ull;                              // no nesting: a second signal waits for sigreturn
  const int signals[7] = { SIGSYS, SIGSEGV, SIGBUS, SIGILL, SIGTRAP, SIGFPE, (int)config->kick_signal };
  for (unsigned i = 0; i < 7 && r >= 0; i++) r = sys(NR_rt_sigaction, signals[i], &action, 0, 8);
  if (r >= 0 && config->uid >= 0) {
    r = sys(NR_setresgid, config->uid, config->uid, config->uid);
    if (r >= 0) r = sys(NR_setresuid, config->uid, config->uid, config->uid);
  }
  if (r >= 0 && config->filter_length) {
    struct { uint16_t length; struct xs_filter_insn *filter; } program = { (uint16_t)config->filter_length, config->filter };
    r = sys(NR_prctl, PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    if (r >= 0) r = sys(NR_seccomp, 1 /* SECCOMP_SET_MODE_FILTER */, 0, &program);
  }
  sp->pid = (int32_t)sys(NR_getpid);
  sp->setup_error = r < 0 ? (int32_t)r : 0;
  wake(&sp->ready);
  if (r < 0) sys(NR_exit_group, 71);
  service(config, sp);
}

// The blob's first instruction: move to the service thread's stack and go.
__asm__(".section .text.entry,\"ax\"\n.globl xs_entry\n.type xs_entry,%function\n"
        "xs_entry:\n"
        "  adr x9, xs_entry\n"            // = the region's base
        "  mov x10, #0x30000\n"           // XS_BOOTSTACK_OFF + XS_BOOTSTACK_SIZE
        "  add sp, x9, x10\n"
        "  mov x29, #0\n"
        "  mov x30, #0\n"
        "  b xs_main\n");
