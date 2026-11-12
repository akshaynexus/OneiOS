// SPDX-License-Identifier: MIT
// What the kernel does that is undefined at EL0, emulated from SIGILL: EL1 system registers, the
// virtual timer's control registers, PSTATE.DAIF, PSCI calls, WFI and TLB maintenance.
// A hosted kernel replaces the hot ones with host calls; unchanged code (Apple's kexts, ArmCpu,
// ArmDTPlatform) still works through this path, slowly.
#include "xnuhost.h"
#include <string.h>

static void timer_update(xh_vcpu *v)
{
  // CNTV_CTL_EL0: bit 0 ENABLE, bit 1 IMASK, bit 2 ISTATUS. The line (INTID 27) is a level: high
  // while the timer is enabled, unmasked and due; the GIC reads it from the CPU layer.
  bool on = (v->sys.cntv_ctl & 1) && !(v->sys.cntv_ctl & 2);
  xh_cpu_timer_set(v, on ? v->sys.cntv_cval : UINT64_MAX);
}

static uint64_t *shadow(xh_sysreg_state *s, unsigned key)
{
  for (unsigned i = 0; i < s->nshadow; i++)
    if (s->shadow_key[i] == key) return &s->shadow[i];
  if (s->nshadow == 64) return NULL;
  s->shadow_key[s->nshadow] = key;
  s->shadow[s->nshadow] = 0;
  return &s->shadow[s->nshadow++];
}

static void set_daif(xh_vcpu *v, uint64_t daif)
{
  v->sys.daif = daif & 0x3c0;
  v->cpu.irq_masked = (daif >> 7) & 1;
}

static int64_t psci(xh_vcpu *v, xh_regs *r)
{
  uint32_t fn = (uint32_t)r->x[0];
  switch (fn) {
  case XH_PSCI_VERSION: return 0x10000;                      // 1.0
  case 0x8400000a:                                           // PSCI_FEATURES
    return (uint32_t)r->x[1] == XH_PSCI_CPU_ON || (uint32_t)r->x[1] == XH_PSCI_SYSTEM_OFF ||
           (uint32_t)r->x[1] == XH_PSCI_SYSTEM_RESET || (uint32_t)r->x[1] == XH_PSCI_CPU_OFF ? 0 : -1;
  case XH_PSCI_CPU_ON: {
    unsigned index = (unsigned)r->x[1] & 0xff;
    if ((r->x[1] & 0xff00ffff00ull) || index >= g_ncpu) return -2;       // INVALID_PARAMETERS
    void *entry = xh_ram(r->x[2], 4);                        // a physical entry address
    if (!entry) return -9;                                   // INVALID_ADDRESS
    // Entered like vCPU 0: x0 = the context id, x1 = the vector, x2 = its struct xh_cpu.
    int rc = xh_cpu_start(index, (uint64_t)(uintptr_t)entry, r->x[3], (uint64_t)(uintptr_t)xh_vector(), (uint64_t)(uintptr_t)&g_vcpu[index].cpu);
    return rc == 0 ? 0 : rc == -2 ? -4 : -6;                 // ALREADY_ON, INTERNAL_FAILURE
  }
  case XH_PSCI_CPU_OFF:
    v->cpu.irq_masked = 1;
    for (;;) xh_cpu_idle(v);
  case XH_PSCI_SYSTEM_OFF: xh_stop(XH_HALT_OFF, 0, "PSCI SYSTEM_OFF"); for (;;) xh_cpu_idle(v);
  case XH_PSCI_SYSTEM_RESET: xh_stop(XH_HALT_RESTART, 0, "PSCI SYSTEM_RESET"); for (;;) xh_cpu_idle(v);
  }
  return -1;                                                 // NOT_SUPPORTED
}

// Which instructions the kernel traps on, and how often (each is a signal round trip on the vCPU's thread): the
// table xh_sysreg_stats_log prints at the stop and on SIGQUIT. An idle guest that keeps its vCPUs busy shows here.
#define INSN_SLOTS 256u
static struct { uint32_t insn; uint64_t count, pc; } insn_seen[INSN_SLOTS];
static uint64_t insn_other;
static void insn_count(uint32_t insn, uint64_t pc)
{
  unsigned h = (insn * 2654435761u) >> 24;
  for (unsigned k = 0; k < 8; k++) {
    unsigned i = (h + k) % INSN_SLOTS;
    uint32_t key = __atomic_load_n(&insn_seen[i].insn, __ATOMIC_RELAXED);
    if (!key) {
      uint32_t zero = 0;
      if (!__atomic_compare_exchange_n(&insn_seen[i].insn, &zero, insn, false, __ATOMIC_RELAXED, __ATOMIC_RELAXED)) key = zero;
      else { __atomic_store_n(&insn_seen[i].pc, pc, __ATOMIC_RELAXED); key = insn; }
    }
    if (key == insn) { __atomic_fetch_add(&insn_seen[i].count, 1, __ATOMIC_RELAXED); return; }
  }
  __atomic_fetch_add(&insn_other, 1, __ATOMIC_RELAXED);
}

void xh_sysreg_stats_log(void)
{
  unsigned order[INSN_SLOTS], n = 0;
  for (unsigned i = 0; i < INSN_SLOTS; i++) if (insn_seen[i].count) order[n++] = i;
  for (unsigned a = 0; a < n; a++)
    for (unsigned b = a + 1; b < n; b++)
      if (insn_seen[order[b]].count > insn_seen[order[a]].count) { unsigned t = order[a]; order[a] = order[b]; order[b] = t; }
  for (unsigned k = 0; k < n && k < 12; k++)
    LOGI("emulated: insn 0x%08x %llu times (first at pc 0x%llx)", insn_seen[order[k]].insn, (unsigned long long)insn_seen[order[k]].count,
         (unsigned long long)insn_seen[order[k]].pc);
  if (insn_other) LOGI("emulated: %llu of other instructions", (unsigned long long)insn_other);
}

bool xh_sysreg_emulate(xh_vcpu *v, xh_regs *r, uint32_t insn)
{
  xh_sys_insn s;
  v->stat_sysreg++;
  insn_count(insn, r->pc);
  if (xh_log_level() >= XH_LOG_DEBUG) xh_log_signal("emulated instruction, pc", insn, r->pc);
  if (insn == 0xd4000002 || insn == 0xd4000003) {            // hvc #0, smc #0
    r->x[0] = (uint64_t)psci(v, r);
    r->pc += 4;
    return true;
  }
  if (insn == 0xd503207f || insn == 0xd503205f) {            // wfi, wfe
    r->pc += 4;
    if (insn == 0xd503207f && !v->cpu.irq_pending) xh_cpu_idle(v);
    return true;
  }
  if ((insn & 0xfff80000) == 0xd5080000) {                   // sys: tlbi, at, dc, ic at EL1: nothing to do
    r->pc += 4;
    return true;
  }
  if ((insn & 0xfff8f01f) == 0xd500401f) {                   // msr <pstatefield>, #imm
    unsigned op1 = (insn >> 16) & 7, op2 = (insn >> 5) & 7, imm = (insn >> 8) & 15;
    uint64_t daif = v->sys.daif | (uint64_t)v->cpu.irq_masked << 7;
    if (op1 == 3 && op2 == 6) set_daif(v, daif | (uint64_t)imm << 6);         // DAIFSet
    else if (op1 == 3 && op2 == 7) set_daif(v, daif & ~((uint64_t)imm << 6)); // DAIFClr
    r->pc += 4;                                              // SPSel, PAN, UAO, ...: no effect here
    return true;
  }
  if (!xh_decode_sys(insn, &s)) return false;
  uint64_t value = s.read ? 0 : xh_reg_get(r, s.rt);
  unsigned key = xh_sys_key(&s);
  switch (key) {
  case XH_SYSREG(3, 0, 13, 0, 4):                            // TPIDR_EL1: XNU's current thread
    if (s.read) value = v->cpu.tpidr_el1; else v->cpu.tpidr_el1 = value;
    break;
  case XH_SYSREG(3, 3, 4, 2, 1):                             // DAIF
    if (s.read) value = v->sys.daif | (uint64_t)v->cpu.irq_masked << 7; else set_daif(v, value);
    break;
  case XH_SYSREG(3, 0, 0, 0, 5):                             // MPIDR_EL1: Aff0 = the vCPU (the tree's "reg"), where
    value = 0x80000000ull | v->cpu.index;                    // the host traps it (Linux answers it itself: 0x80000000)
    break;
  case XH_SYSREG(3, 0, 4, 2, 2):                             // CurrentEL: EL1
    value = 4;
    break;
  case XH_SYSREG(3, 3, 14, 3, 1):                            // CNTV_CTL_EL0
    if (s.read) {
      bool due = (v->sys.cntv_ctl & 1) && xh_counter() >= v->sys.cntv_cval;
      value = (v->sys.cntv_ctl & 3) | (due ? 4 : 0);
    } else { v->sys.cntv_ctl = value & 3; timer_update(v); }
    break;
  case XH_SYSREG(3, 3, 14, 3, 2):                            // CNTV_CVAL_EL0
    if (s.read) value = v->sys.cntv_cval; else { v->sys.cntv_cval = value; timer_update(v); }
    break;
  case XH_SYSREG(3, 3, 14, 3, 0):                            // CNTV_TVAL_EL0
    if (s.read) value = (uint32_t)(v->sys.cntv_cval - xh_counter());
    else { v->sys.cntv_cval = xh_counter() + (uint64_t)(int64_t)(int32_t)value; timer_update(v); }
    break;
  case XH_SYSREG(3, 1, 0, 0, 1):                             // CLIDR_EL1: L1 separate instruction and data, L2 unified
    value = 3u | 4u << 3 | 2u << 24 | 2u << 27;              // (LoC and LoUU 2)
    break;
  case XH_SYSREG(3, 1, 0, 0, 0): {                           // CCSIDR_EL1 of the cache CSSELR_EL1 selects
    // The geometry XNU reads at each CPU's start (cpuid.c do_cacheid): the line size is the host's
    // (CTR_EL0.DminLine: 64 bytes on every core this runs on); sizes are those of a current big core
    // (64 KiB 4-way L1, 1 MiB 8-way L2). A shadow answered 0 here: 16-byte lines, and the kernel then
    // aligned its per-CPU page queues to 16 bytes ("pointer can't be packed" in vm_page_init_local_q).
    uint64_t ctr = 4u << 16, *selected = shadow(&v->sys, XH_SYSREG(3, 2, 0, 0, 0));
#if defined(__aarch64__)
    __asm__("mrs %0, ctr_el0" : "=r"(ctr));
#endif
    unsigned line = 4u << ((ctr >> 16) & 15), level = selected ? (unsigned)(*selected >> 1) & 7 : 0;
    unsigned ways = level == 0 ? 4 : 8, bytes = level == 0 ? 64u << 10 : 1u << 20, log2 = 0;
    while ((1u << log2) < line) log2++;
    value = (log2 - 4) | (ways - 1) << 3 | (uint64_t)(bytes / (line * ways) - 1) << 13;
    break;
  }
  default:
    if (xh_gic_sysreg(v->cpu.index, &s, &value)) break;
    // Any other EL1 or EL2 register: a per-CPU shadow that reads back what was written. This lets
    // unchanged start-up code run; it does not give the write its hardware meaning (page tables,
    // vectors and PAC keys are the hosted kernel's own business).
    // That includes EL0-class registers Linux refuses at EL0 (XNU's start clears TPIDRRO_EL0).
    {
      uint64_t *slot = shadow(&v->sys, key);
      if (!slot) return false;
      if (s.read) value = *slot; else *slot = value;
    }
  }
  if (s.read) xh_reg_set(r, s.rt, value);
  r->pc += 4;
  return true;
}
