// SPDX-License-Identifier: MIT
// xnuhost host-call ABI, version 1: the whole contract between a hosted XNU ("LINUXHOST" machine
// configuration, kernel/) and xnuhost. This header is freestanding C and is meant to be included by
// the kernel's machine layer unchanged. docs/booter.md explains the model; the summary:
//
//  - The kernel is entered on vCPU 0 at its LC_UNIXTHREAD entry with x0 = boot_args (virtual),
//    x1 = struct xh_vector *, x2 = struct xh_cpu * of vCPU 0, sp = a 256 KiB boot stack.
//    The window of RAM is already mapped at boot_args.virtBase; there is no MMU to turn on.
//    The vector is also in the device tree: /chosen/xnuhost "vector".
//  - Every host call takes the calling vCPU's struct xh_cpu * first. The host switches to its own
//    thread register (TPIDR_EL0), x18 and stack for the call, so the kernel may keep anything in
//    TPIDR_EL0 and x18 while its own code runs. Host calls follow AAPCS64.
//  - Interrupts are a software flag. The kernel masks them by storing 1 to cpu->irq_masked and
//    unmasks by storing 0 and then, if cpu->irq_pending is not 0, calling its own interrupt path
//    (irq_ack until it returns XH_INTID_NONE). While unmasked and running kernel code, an interrupt
//    arrives as a call of upcalls->irq(cpu, frame) on the interrupted stack with irq_masked = 1;
//    returning from it resumes the interrupted code. The kernel may block or switch threads inside.
//  - A kernel that moves a thread to another vCPU inside an upcall (preemption in an interrupt) must
//    not return from that upcall: it ends it by calling frame_resume(the vCPU the thread is on now,
//    frame) itself, after correcting anything in the frame that names the old vCPU. Such a kernel
//    also makes every host call with irq_masked = 1, so that no interrupt is delivered on a host
//    call's return path (that path stores to the vCPU the call started on).
//  - A vCPU started by PSCI CPU_ON is entered like vCPU 0: x0 = the PSCI context id, x1 = the vector,
//    x2 = its struct xh_cpu *.
//  - iOS threads run in other Linux processes. thread_run() gives the vCPU to one until it traps.
#ifndef XNUHOST_HOSTCALL_H
#define XNUHOST_HOSTCALL_H

#include <stdint.h>

#define XH_ABI_VERSION 1u

// ---- per-vCPU block (host memory; the kernel reads and writes only the fields marked K) ----------
struct xh_cpu {
  uint32_t index;             // 0x00    vCPU number
  uint32_t irq_masked;        // 0x04 K  1 = interrupts masked (the kernel's DAIF.I)
  uint32_t irq_pending;       // 0x08    not 0 = the interrupt line is asserted (read by K)
  uint32_t in_host;           // 0x0c    host only
  uint64_t kernel_tpidr;      // 0x10    the kernel's TPIDR_EL0 while a host call runs
  uint64_t kernel_x18;        // 0x18    the kernel's x18 while a host call runs
  uint64_t host_tpidr;        // 0x20    host only
  uint64_t host_x18;          // 0x28    host only
  uint64_t host_sp;           // 0x30    host only
  uint64_t kernel_data;       // 0x38 K  free for the kernel (its cpu_data pointer)
  uint64_t tpidr_el1;         // 0x40 K  shadow of TPIDR_EL1 for trapped mrs/msr (current thread)
  uint64_t reserved[7];       // 0x48
};                            // 0x80; host-private data follows

// ---- saved register frames ---------------------------------------------------------------------
struct xh_vreg { uint64_t lo, hi; };

// Kernel context interrupted by an interrupt or exception; lives on the interrupted stack.
struct xh_frame {
  uint64_t x[31];
  uint64_t sp, pc, pstate;    // pstate: NZCV only is meaningful
  uint32_t fpsr, fpcr;
  struct xh_vreg v[32];
  uint32_t irq_masked;        // the mask state to return to
  uint32_t kind;              // XH_FRAME_*
  uint64_t far, esr;          // for XH_FRAME_EXCEPTION
};
#define XH_FRAME_IRQ        1u
#define XH_FRAME_EXCEPTION  2u

// User (iOS thread) register state.
struct xh_user_state {
  uint64_t x[31];
  uint64_t sp, pc, pstate;    // pstate: NZCV; the host forces EL0t
  uint64_t tsd;               // the thread's TSD base. iOS reads it from TPIDRRO_EL0; the host keeps
                              // it in TPIDR_EL0 and text_fixup() rewrites the reads (docs/booter.md)
  uint32_t fpsr, fpcr;
  struct xh_vreg v[32];
};

// Why a thread stopped running user code.
struct xh_trap {
  uint32_t kind;              // XH_TRAP_*
  uint32_t signo;             // the Linux signal that reported it (diagnostic)
  uint64_t far;               // fault address (FAULT), instruction address (ILLEGAL, BRK)
  uint64_t esr;               // ESR_EL1 as Linux reported it, 0 if it did not
  uint32_t svc;               // SYSCALL: the svc immediate (0x80 for XNU's)
  uint32_t access;            // FAULT: XH_PROT_* bit of the access that faulted, 0 if unknown
  uint32_t insn;              // the instruction at the trap, when readable
  uint32_t exit_status;       // EXITED: the Linux wait status of the task process
};
#define XH_TRAP_SYSCALL     1u   // svc: number in x16, as XNU's handle_svc expects
#define XH_TRAP_FAULT       2u   // data or instruction abort from user mode
#define XH_TRAP_ILLEGAL     3u   // undefined instruction
#define XH_TRAP_BRK         4u   // brk / breakpoint / single step
#define XH_TRAP_FPE         5u
#define XH_TRAP_INTERRUPT   6u   // an interrupt is pending for this vCPU; state is the interrupted user state
#define XH_TRAP_EXITED      7u   // the task's Linux process is gone
#define XH_TRAP_BOOT        8u   // a new thread's first stop, before any user instruction

#define XH_PROT_READ   1u
#define XH_PROT_WRITE  2u
#define XH_PROT_EXEC   4u

// ---- interrupts: the board's GICv3 numbering ----------------------------------------------------
#define XH_INTID_NONE   1023u
#define XH_INTID_VTIMER 27u      // per-CPU virtual timer (PPI), as ArmCpu takes it
#define XH_SGI_COUNT    16u

// ---- halt reasons -------------------------------------------------------------------------------
#define XH_HALT_OFF      0u      // guest shut down: xnuhost exits 0
#define XH_HALT_RESTART  1u      // guest asked for a reboot: exit 3
#define XH_HALT_PANIC    2u      // kernel panic: exit 4
#define XH_HALT_RETURN   3u      // guest asks to return to One UI: session/return.request, exit 0

// ---- static facts about the host ----------------------------------------------------------------
struct xh_info {
  uint32_t abi, ncpu;
  uint32_t host_page_size;     // 4096 on the S24 Ultra; never larger than 16384
  uint32_t user_va_bits;       // of the host's user address space
  uint64_t ram_phys, ram_virt, ram_size;      // the kernel's window: physical, where it is mapped, bytes
  uint64_t io_phys, io_virt, io_size;         // trapped MMIO aperture (the board's devices)
  uint64_t user_va_min, user_va_limit;        // iOS tasks may use [min, limit)
  uint64_t timebase_hz;        // CNTFRQ_EL0; CNTVCT_EL0 is readable in the kernel and in tasks
  uint64_t features;           // XH_FEATURE_*
  uint64_t video[6];           // boot_args video, repeated
  // Appended after the first ABI 1 layout; present when vector->size covers kernel_unmap.
  // A reservation (no access) in xnuhost's own address space for the kernel's mappings outside the
  // RAM window (kernel_map: zones, kalloc, stacks, IOKit buffers): from the end of the RAM rounded
  // up to 1 GiB, to 0x3c00000000. kva_size is 0 if it could not be reserved.
  uint64_t kva_base, kva_size;
};
#define XH_FEATURE_PAC          (1ull << 0)   // paca/pacg: arm64e runs; one key set for kernel and tasks
#define XH_FEATURE_TASKS        (1ull << 1)   // the task trap layer works on this host (seccomp, memfd)
#define XH_FEATURE_PAC_KEYS_OFF (1ull << 2)   // PR_PAC_SET_ENABLED_KEYS: plain arm64 tasks can run

// ---- calls from the host into the kernel --------------------------------------------------------
struct xh_upcalls {
  // An interrupt while kernel code ran unmasked. Returning resumes `frame`.
  void (*irq)(struct xh_cpu *cpu, struct xh_frame *frame);
  // A fault in kernel code that is not a device access (frame->kind = XH_FRAME_EXCEPTION, far, esr).
  // The kernel may change frame->pc (copyio recovery) and return, or panic. NULL: xnuhost stops.
  void (*exception)(struct xh_cpu *cpu, struct xh_frame *frame);
  // A secondary vCPU's first call, on its boot stack, instead of an entry address (cpu_start with
  // entry 0). Does not return.
  void (*cpu_entry)(struct xh_cpu *cpu, uint64_t arg);
};

// ---- the vector ---------------------------------------------------------------------------------
// Return values: 0 or a positive count on success, a negative errno otherwise.
struct xh_vector {
  uint32_t abi, size;
  const struct xh_info *info;

  // console, life cycle
  void     (*console_write)(struct xh_cpu *, const char *text, uint64_t length);
  void     (*halt)(struct xh_cpu *, uint32_t reason, uint32_t code);           // does not return
  void     (*ready)(struct xh_cpu *);                 // the guest's first frame is on screen
  int64_t  (*set_upcalls)(struct xh_cpu *, const struct xh_upcalls *);
  uint64_t (*wall_time)(struct xh_cpu *);             // seconds since 1970 (armdt_platform_clock_register)

  // CPUs, interrupts, timer
  int64_t  (*cpu_start)(struct xh_cpu *, uint32_t index, uint64_t entry, uint64_t arg);
  void     (*cpu_idle)(struct xh_cpu *);              // returns when irq_pending is set
  uint32_t (*irq_ack)(struct xh_cpu *);               // highest pending INTID, now active; XH_INTID_NONE
  void     (*irq_eoi)(struct xh_cpu *, uint32_t intid);
  void     (*irq_enable)(struct xh_cpu *, uint32_t intid, uint32_t enable);
  void     (*ipi)(struct xh_cpu *, uint32_t target_cpu, uint32_t sgi);
  void     (*timer_set)(struct xh_cpu *, uint64_t deadline);   // CNTVCT_EL0 value; UINT64_MAX cancels
  void     (*frame_resume)(struct xh_cpu *, const struct xh_frame *);          // does not return

  // devices
  uint64_t (*io_map)(struct xh_cpu *, uint64_t phys, uint64_t size);           // kernel VA, 0 if outside
  uint64_t (*mmio_read)(struct xh_cpu *, uint64_t phys, uint32_t size);
  void     (*mmio_write)(struct xh_cpu *, uint64_t phys, uint32_t size, uint64_t value);

  // memory
  int64_t  (*ram_discard)(struct xh_cpu *, uint64_t phys, uint64_t size);      // give free pages back to Linux
  uint32_t (*text_fixup)(struct xh_cpu *, void *text, uint64_t size);          // rewritten instruction count

  // iOS tasks (Linux processes) and threads
  int64_t  (*task_create)(struct xh_cpu *, uint64_t *task);
  int64_t  (*task_destroy)(struct xh_cpu *, uint64_t task);
  int64_t  (*task_map)(struct xh_cpu *, uint64_t task, uint64_t va, uint64_t size, uint32_t prot, uint64_t phys);
  int64_t  (*task_unmap)(struct xh_cpu *, uint64_t task, uint64_t va, uint64_t size);
  int64_t  (*task_protect)(struct xh_cpu *, uint64_t task, uint64_t va, uint64_t size, uint32_t prot);
  int64_t  (*task_read)(struct xh_cpu *, uint64_t task, uint64_t va, void *buffer, uint64_t size);
  int64_t  (*task_write)(struct xh_cpu *, uint64_t task, uint64_t va, const void *buffer, uint64_t size);
  int64_t  (*thread_create)(struct xh_cpu *, uint64_t task, uint64_t *thread);
  int64_t  (*thread_destroy)(struct xh_cpu *, uint64_t thread);
  // Runs the thread from *state until it traps; *state is then the trapped state. Blocks the vCPU.
  int64_t  (*thread_run)(struct xh_cpu *, uint64_t thread, struct xh_user_state *state, struct xh_trap *trap);
  // Pointer-authentication keys enabled for the thread from its next run (plain arm64 binaries: 0).
  int64_t  (*thread_set_pac)(struct xh_cpu *, uint64_t thread, uint32_t enabled);

  // Appended (check vector->size). The kernel's own address space: RAM pages at kernel virtual
  // addresses inside [info->kva_base, +kva_size). The host mapping plays the TLB of the kernel's
  // page tables: kernel_map on a fault the page table allows, kernel_unmap on an invalidation.
  // va, size and phys are host-page aligned. After kernel_unmap the range faults again
  // (upcalls->exception). Both may be called with interrupts masked.
  int64_t  (*kernel_map)(struct xh_cpu *, uint64_t va, uint64_t size, uint32_t prot, uint64_t phys);
  int64_t  (*kernel_unmap)(struct xh_cpu *, uint64_t va, uint64_t size);

  // Appended (check vector->size). A device page mapped at a kernel address by the kernel's own page
  // tables (IOKit's IOMemoryMap, not io_map) has no host mapping: the access faults into
  // upcalls->exception. When the page-table entry's physical address is inside [io_phys, +io_size),
  // the kernel calls this instead of kernel_map: the host emulates the integer load or store at
  // frame->pc against device address `phys` (page address plus the offset of frame->far in its
  // page) on the frame's registers and advances frame->pc. 0 = done; -EINVAL: phys is not in the
  // aperture or pc is not kernel text; -ENOTSUP: not a load or store the host emulates (SIMD,
  // exclusive, atomics): the frame is unchanged. May be called with interrupts masked.
  int64_t  (*mmio_emulate)(struct xh_cpu *, struct xh_frame *frame, uint64_t phys);
};
#define XH_PAC_ALL 0xfu          // APIA, APIB, APDA, APDB

// PSCI function numbers the host answers when the kernel issues `hvc #0` (or `smc #0`) directly,
// as ArmCpu and ArmDTPlatform do: CPU_ON starts a vCPU at a physical entry address.
#define XH_PSCI_VERSION       0x84000000u
#define XH_PSCI_CPU_OFF       0x84000002u
#define XH_PSCI_CPU_ON        0xc4000003u
#define XH_PSCI_SYSTEM_OFF    0x84000008u
#define XH_PSCI_SYSTEM_RESET  0x84000009u

#endif
