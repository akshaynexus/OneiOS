// SPDX-License-Identifier: MIT
// The seams between the hosted kernel's context and xnuhost's own (docs/booter.md, "Two contexts").
// Kernel context: XNU's stack, XNU's TPIDR_EL0 and x18. Host context: this thread's bionic TLS,
// x18 and host stack. Offsets are struct xh_cpu's (xnuhost/hostcall.h).
#if defined(__aarch64__)

#define CPU_MASKED   0x04
#define CPU_PENDING  0x08
#define CPU_IN_HOST  0x0c
#define CPU_K_TPIDR  0x10
#define CPU_K_X18    0x18
#define CPU_H_TPIDR  0x20
#define CPU_H_X18    0x28
#define CPU_H_SP     0x30

        .text
// ---- host calls: xh_hc_NAME(cpu, ...) -> xh_impl_NAME(cpu, ...) on the host stack -----------------
.macro HOSTCALL name
        .globl  xh_hc_\name
        .type   xh_hc_\name, %function
xh_hc_\name:
        stp     x29, x30, [sp, #-48]!
        mov     x29, sp
        stp     x19, x20, [sp, #16]
        str     x21, [sp, #32]
        mov     x19, x0
        mov     w9, #1
        str     w9, [x19, #CPU_IN_HOST]         // from here an interrupt is only noted
        mrs     x9, tpidr_el0
        str     x9, [x19, #CPU_K_TPIDR]
        str     x18, [x19, #CPU_K_X18]
        ldr     x9, [x19, #CPU_H_TPIDR]
        msr     tpidr_el0, x9
        ldr     x18, [x19, #CPU_H_X18]
        mov     x20, sp
        ldr     x9, [x19, #CPU_H_SP]
        mov     sp, x9
        bl      xh_impl_\name
        mov     sp, x20
        ldr     x9, [x19, #CPU_K_TPIDR]
        msr     tpidr_el0, x9
        ldr     x18, [x19, #CPU_K_X18]
        str     wzr, [x19, #CPU_IN_HOST]
        ldr     w9, [x19, #CPU_PENDING]         // an interrupt noted meanwhile, and not masked:
        cbz     w9, 1f                          // deliver it now, in kernel context
        ldr     w9, [x19, #CPU_MASKED]
        cbnz    w9, 1f
        mov     x21, x0
        mov     x0, x19
        bl      xh_kctx_irq_poll
        mov     x0, x21
1:      ldr     x21, [sp, #32]
        ldp     x19, x20, [sp, #16]
        ldp     x29, x30, [sp], #48
        ret
        .size   xh_hc_\name, . - xh_hc_\name
.endm

HOSTCALL console_write
HOSTCALL halt
HOSTCALL ready
HOSTCALL set_upcalls
HOSTCALL wall_time
HOSTCALL cpu_start
HOSTCALL cpu_idle
HOSTCALL irq_ack
HOSTCALL irq_eoi
HOSTCALL irq_enable
HOSTCALL ipi
HOSTCALL timer_set
HOSTCALL frame_resume
HOSTCALL io_map
HOSTCALL mmio_read
HOSTCALL mmio_write
HOSTCALL ram_discard
HOSTCALL text_fixup
HOSTCALL task_create
HOSTCALL task_destroy
HOSTCALL task_map
HOSTCALL task_unmap
HOSTCALL task_protect
HOSTCALL task_read
HOSTCALL task_write
HOSTCALL thread_create
HOSTCALL thread_destroy
HOSTCALL thread_run
HOSTCALL thread_set_pac
HOSTCALL kernel_map
HOSTCALL kernel_unmap
HOSTCALL mmio_emulate

// ---- signal entry: void xh_sig_entry(int, siginfo_t *, void *) ----------------------------------
// A vCPU thread takes signals on its own signal stack, a 1 MiB aligned block of the arena whose
// first word is its struct xh_cpu *. The interrupted code may have been the kernel, with its own
// TPIDR_EL0 and x18: put the host's back before any C runs, and the interrupted ones after.
        .globl  xh_sig_entry
        .type   xh_sig_entry, %function
xh_sig_entry:
        stp     x29, x30, [sp, #-48]!
        mov     x29, sp
        stp     x19, x20, [sp, #16]
        str     x21, [sp, #32]
        mov     x19, #0
        adrp    x9, xh_sig_arena
        add     x9, x9, :lo12:xh_sig_arena
        ldp     x10, x11, [x9]                  // arena [lo, hi)
        mov     x12, sp
        cmp     x12, x10
        b.lo    1f
        cmp     x12, x11
        b.hs    1f
        and     x12, x12, #0xfffffffffff00000
        ldr     x19, [x12]
        mrs     x20, tpidr_el0
        mov     x21, x18
        ldr     x9, [x19, #CPU_H_TPIDR]
        msr     tpidr_el0, x9
        ldr     x18, [x19, #CPU_H_X18]
1:      mov     x3, x19
        bl      xh_cpu_signal
        cbz     x19, 2f
        msr     tpidr_el0, x20
        mov     x18, x21
2:      ldr     x21, [sp, #32]
        ldp     x19, x20, [sp, #16]
        ldp     x29, x30, [sp], #48
        ret
        .size   xh_sig_entry, . - xh_sig_entry

// ---- where a diverted kernel context lands: x0 = cpu, x1 = frame, x2 = the kernel's upcall --------
// Kernel context, on the interrupted stack just below the frame.
        .globl  xh_upcall_tramp
        .type   xh_upcall_tramp, %function
xh_upcall_tramp:
        mov     x19, x0
        mov     x20, x1
        mov     x29, #0
        blr     x2
        mov     x0, x19
        mov     x1, x20
        b       xh_hc_frame_resume              // does not return
        .size   xh_upcall_tramp, . - xh_upcall_tramp

// ---- void xh_arch_enter(cpu, entry, a0, a1, a2, sp): leave the host for the kernel, for good ------
        .globl  xh_arch_enter
        .type   xh_arch_enter, %function
xh_arch_enter:
        mov     x9, x1
        mov     sp, x5
        mov     x10, x0
        mov     x0, x2
        mov     x1, x3
        mov     x2, x4
        msr     tpidr_el0, xzr
        mov     x29, #0
        mov     x30, #0
        str     wzr, [x10, #CPU_IN_HOST]
        br      x9
        .size   xh_arch_enter, . - xh_arch_enter

// ---- void xh_arch_sigreturn(cpu, sigframe, kernel_tpidr, kernel_x18_unused, masked) ---------------
// The last step of frame_resume: async signals are blocked; the frame's uc_sigmask unblocks them.
        .globl  xh_arch_sigreturn
        .type   xh_arch_sigreturn, %function
xh_arch_sigreturn:
        str     w4, [x0, #CPU_MASKED]
        msr     tpidr_el0, x2
        str     wzr, [x0, #CPU_IN_HOST]
        mov     sp, x1
        mov     x8, #139                        // rt_sigreturn
        svc     #0
        brk     #0x1
        .size   xh_arch_sigreturn, . - xh_arch_sigreturn

// ---- the task stub, a flat blob (stub/stub.c), copied into every task's stub region --------------
        .section .rodata
        .balign 16
        .globl  xh_stub_blob
        .globl  xh_stub_blob_end
xh_stub_blob:
        .incbin XH_STUB_BIN
xh_stub_blob_end:

        .section .note.GNU-stack, "", %progbits
#endif
