// SPDX-License-Identifier: MIT
// C that runs in kernel context (the kernel's TPIDR_EL0 and x18, the kernel's stack): no thread-
// local storage, no stack protector, no libc beyond memset. Built with -fno-stack-protector.
#include "xnuhost.h"
#include <string.h>

// A host call is returning and an interrupt was noted while it ran. Deliver it as the kernel's
// interrupt upcall, called like a function: the frame says where the kernel "was" (here).
void xh_kctx_irq_poll(struct xh_cpu *cpu);
void xh_kctx_irq_poll(struct xh_cpu *cpu)
{
  if (!g_upcalls.irq) return;
  struct xh_frame frame;
  memset(&frame, 0, sizeof frame);
  frame.kind = XH_FRAME_IRQ;
  frame.pc = (uint64_t)(uintptr_t)__builtin_return_address(0);
  frame.sp = (uint64_t)(uintptr_t)__builtin_frame_address(0);
  cpu->irq_masked = 1;
  g_upcalls.irq(cpu, &frame);
  cpu->irq_masked = 0;
}
