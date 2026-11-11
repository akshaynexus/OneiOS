// SPDX-License-Identifier: MIT
// AArch64 instruction decoding for the two things xnuhost emulates in kernel context: integer
// loads and stores that hit a trapped device page, and system-register accesses that are
// undefined at EL0. Encodings: Arm ARM (DDI 0487), C4.1 "A64 instruction set encoding".
#include "xnuhost.h"

bool xh_decode_mem(uint32_t insn, xh_mem_insn *m)
{
  *m = (xh_mem_insn){ 0 };
  m->rt = insn & 31;
  m->rn = (insn >> 5) & 31;
  if (insn & (1u << 26)) return false;                        // SIMD&FP register
  unsigned size = insn >> 30, opc = (insn >> 22) & 3;

  if ((insn & 0x3b000000) == 0x39000000 ||                   // register, unsigned immediate
      (insn & 0x3b200000) == 0x38000000 ||                   // register, imm9 (unscaled, pre, post)
      (insn & 0x3b200c00) == 0x38200800) {                   // register, register offset
    bool imm9 = (insn & 0x3b200000) == 0x38000000;
    unsigned type = (insn >> 10) & 3;
    if (imm9 && type == 2) return false;                     // unprivileged (ldtr/sttr)
    if (size == 3 && opc == 2) return false;                 // prfm
    if (size == 3 && opc == 3) return false;
    m->size = 1u << size;
    m->load = opc != 0;
    m->sign_extend = opc >= 2;
    m->to64 = opc == 2 || (opc == 1 && size == 3);
    if (opc == 3 && size == 2) return false;
    if (imm9 && (type == 1 || type == 3)) {
      m->writeback = true;
      m->offset = (int64_t)((insn >> 12) & 0x1ff);
      if (m->offset & 0x100) m->offset -= 0x200;
    }
    return true;
  }
  if ((insn & 0x3a000000) == 0x28000000) {                   // pair: stp/ldp/ldpsw
    unsigned popc = insn >> 30, type = (insn >> 23) & 3;
    bool load = insn & (1u << 22);
    if (popc == 3 || (popc == 1 && !load)) return false;
    m->pair = true;
    m->rt2 = (insn >> 10) & 31;
    m->size = popc == 2 ? 8 : 4;
    m->load = load;
    m->sign_extend = popc == 1;
    m->to64 = popc != 0;
    if (type == 1 || type == 3) {
      m->writeback = true;
      int64_t imm7 = (insn >> 15) & 0x7f;
      if (imm7 & 0x40) imm7 -= 0x80;
      m->offset = imm7 * (int64_t)m->size;
    }
    return true;
  }
  return false;
}

bool xh_decode_sys(uint32_t insn, xh_sys_insn *s)
{
  if ((insn & 0xffd00000) != 0xd5100000) return false;      // mrs/msr with op0 = 2 or 3
  s->read = insn & (1u << 21);
  s->op0 = 2 + ((insn >> 19) & 1);
  s->op1 = (insn >> 16) & 7;
  s->crn = (insn >> 12) & 15;
  s->crm = (insn >> 8) & 15;
  s->op2 = (insn >> 5) & 7;
  s->rt = insn & 31;
  return true;
}

// iOS user code reads its thread-specific data base with `mrs xN, TPIDRRO_EL0` (0xd53bd060 | N).
// Linux leaves that register 0 and user space cannot write it, so the host keeps the base in
// TPIDR_EL0 (`mrs xN, TPIDR_EL0` is 0xd53bd040 | N) and rewrites the reads when the kernel maps a
// page executable. The risk is a data word in text that happens to equal the encoding.
//
// iOS itself uses TPIDR_EL0 for something else: XNU keeps the CPU number in its low 12 bits and
// _os_cpu_number() reads it to pick a per-CPU slot (libmalloc's magazines, libdispatch). With the TSD
// base there the slot index would be garbage, so those reads become `mov xN, #0`: every thread
// says CPU 0, which is always a valid slot (the number is only a hint: a thread can be moved right
// after reading it). A write of TPIDR_EL0 would destroy the TSD base and becomes a nop.
// Each word is classified by its original value, so one pass is right; a second pass over the same
// page is not (it would zero the TSD reads): the kernel fixes a page once, when it is first entered
// executable, and only pages of code (the patterns are also plain data words in a data page).
// A host with no seccomp (qemu-user) cannot trap a task's `svc`: Linux, or qemu, would run it as its
// own system call. There, text_fixup also turns iOS's `svc #0x80` into an undefined instruction
// (XH_SVC_MARK), and xh_thread_run reports that trap as the system call it stands for.
bool xh_text_fixup_svc;

uint32_t xh_text_fixup(uint32_t *text, uint64_t size)
{
  uint32_t count = 0;
  for (uint64_t i = 0; i < size / 4; i++) {
    uint32_t insn = text[i] & 0xffffffe0, rt = text[i] & 31;
    if (xh_text_fixup_svc && text[i] == 0xd4001001) { text[i] = XH_SVC_MARK; count++; continue; }
    if (insn == 0xd53bd060) text[i] = 0xd53bd040 | rt;          // mrs xN, TPIDRRO_EL0 -> mrs xN, TPIDR_EL0
    else if (insn == 0xd53bd040) text[i] = 0xd2800000 | rt;     // mrs xN, TPIDR_EL0   -> mov xN, #0
    else if (insn == 0xd51bd040) text[i] = 0xd503201f;          // msr TPIDR_EL0, xN   -> nop
    else continue;
    count++;
  }
  return count;
}
