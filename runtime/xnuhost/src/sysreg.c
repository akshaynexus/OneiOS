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
    int rc = xh_cpu_start(index, (uint64_t)(uintptr_t)entry, r->x[3], 0, 0);
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

bool xh_sysreg_emulate(xh_vcpu *v, xh_regs *r, uint32_t insn)
{
  xh_sys_insn s;
  v->stat_sysreg++;
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
  default:
    if (xh_gic_sysreg(v->cpu.index, &s, &value)) break;
    // Any other EL1 or EL2 register: a per-CPU shadow that reads back what was written. This lets
    // unchanged start-up code run; it does not give the write its hardware meaning (page tables,
    // vectors and PAC keys are the hosted kernel's own business).
    if (s.op1 == 3 && s.op0 == 3) return false;              // an EL0 register Linux refused: not ours
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
