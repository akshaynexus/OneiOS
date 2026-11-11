# 0002-return-to-user-copyio: user mode and copyio of the hosted kernel

Owner: fable-user. Date: 2026-11-10. A diff on top of `0001` + `0003` (it is last in `series`).
No change to `hostcall.h` and none to xnuhost.

**State, plainly.** The kernel builds from the committed series and boots as far as before
(`image4 interface not available`, no `kernel.kc` yet). copyio and the user-mode entry are checked by a
self-test inside the kernel (`hosted_selftest=1`), under `qemu-aarch64`. No real iOS task has run: the
standalone kernel stops before `bsd_init`, and qemu-user does not trap `svc`. The dispatch of a real
task's traps (`handle_svc`, exceptions, signals, ASTs, exec, thread exit) has not run anywhere.

## What it does

| Where | What |
|---|---|
| `locore.s` `arm64_thread_exception_return` | branches to `hosted_thread_exception_return` (so `thread_bootstrap_return`, `thread_exception_return`, every syscall return and continuation end there) |
| `hosted_asm.s` `hosted_thread_exception_return` | sp = the thread's `kstackptr` (an exception return leaves the kernel stack empty), then `hosted_user_loop` |
| `hosted_user.c` `hosted_user_loop` | `return_to_user`: io tier reset, preemption-count check, masked AST loop (`ast_taken_user`, the commpage PFZ rule after an interrupt), `recount_enter_user`/`leave_user`; then `thread_run`; then the trap as the EL0 vectors would hand it on: `sleh_synchronous(contextData, esr, far)` for svc / fault / undefined / brk / FP, `hosted_user_interrupt` (`sleh_irq` on the interrupt stack) for an interrupt |
| `hosted_user.c` twin | `hosted_twin_ensure`: one Linux thread per XNU thread and address space (`thread_create`, `thread_set_pac` from `ARM_MACHINE_THREAD_DISABLE_USER_JOP`). The thread holds a reference on the twin's pmap, so the task process outlives its twins; a thread whose map changed (exec) drops the twin and makes one in the new process. `machine_thread_destroy` -> `hosted_thread_twin_destroy` |
| `hosted_user.c` state | `hosted_state_to_user` / `hosted_state_from_user`: `arm_context_t` <-> `xh_user_state` (x0-x30, sp, pc, NZCV, fpsr/fpcr, v0-v31, `tsd` = `cthread_self`); the saved state's hash is checked before the run (`ml_check_signed_state`, what `AUTH_THREAD_STATE_IN_X0` is before an `eret`) and signed after it |
| `hosted_user.c` syndromes | `hosted_trap_esr`: svc -> `ESR_EC_SVC_64` + immediate; fault -> data/instruction abort from EL0 as a permission fault of the access (so `arm_fast_fault` runs before `vm_fault`), other fault codes as Linux gave them; brk -> `ESR_EC_BRK_AARCH64` (immediate read from the task's text when the stub sent none); undefined -> `ESR_EC_UNCATEGORIZED` or Linux's syndrome (FPAC) |
| `hosted_user.c` faults | a user fault first tries `hosted_user_fill` (the page table may already allow it: the host mapping is only a TLB); else `sleh_synchronous`, and the page is filled on the way back (`machine.hosted_refill`) instead of at a second trap |
| `XH_TRAP_EXITED` | the task process died outside the kernel: `psignal(SIGKILL)` to the task |
| `copyio.c` | `WRAP_COPYIO` selects `hosted_bcopyin`, `hosted_bcopyout`, `hosted_bcopyinstr`, `hosted_copyin_atomic32/64`, `hosted_copyin_atomic32_wait_if_equals`, `hosted_copyout_atomic32/64` (`hosted_copyio.c`). All of `copy_validate` stays. Covers `copyin`, `copyout`, `copyinstr`, the atomics, `copyin_word`, `copyinmap`/`copyoutmap`, `uiomove` |
| `hosted_copyio.c` | page-table walk + copy through the RAM window; where the table does not allow the access: `arm_fast_fault`, then `vm_fault` (as `handle_kernel_abort` does for a copyio fault), and the copy goes on. Not resolvable -> `EFAULT`. No kernel fault and no recovery entry is involved |
| bug 1, `hosted_pmap.h` `hosted_copy_user_pages` | the caller's buffer is touched with no lock held (1 KiB stack bounce buffer per piece; the stack is always host-mapped). Under the pmap lock and the TLB lock only the bounce buffer and the window are touched. With interrupts masked the pmap lock is only tried. `hosted_user_atomic` for the single-access words |
| bug 2, `copyio.c` `copy_ensure_address_space_spec` | no bit 55 under `XNU_HOSTED`: a kernel-to-kernel copy must name `VM_MIN_KERNEL_ADDRESS..VM_MAX_KERNEL_ADDRESS`, any other copy must not. `copy_validate_user_addr` refuses (`EFAULT`) a user range that reaches `VM_MIN_KERNEL_ADDRESS` or `xh_info.user_va_limit` |
| `bsd/kern/ubc_subr.c` `cs_text_fixup_page` | called `hosted_text_fixup` at validation. **Replaced by 0025** (`0025-README.md`): it also rewrote data pages and made every later validation of the page fail |
| `arm/thread.h` | `machine.hosted_thread`, `hosted_pmap`, `hosted_pac`, `hosted_refill` in place of `reserved4/12/14/6` |

## Tests (PC, `qemu-aarch64 -cpu cortex-a710`, boot arguments of `README.md` + `hosted_selftest=1`)

`hosted_selftest` (`hosted_user.c`, `STARTUP(EARLY_BOOT)`, off without the boot argument) makes an address
space of its own (pmap + map, 8 anonymous pages) and reports each check on the console.

- With the task layer (no `--no-tasks`): **27 passed, 0 failed**, also with `--cpus 4 sheng-smp=1`.
  copyio: copyout/copyin across four untouched pages (vm_fault from copyio), `copyinstr` at a page end,
  `ENAMETOOLONG`, the atomics, unaligned -> `EINVAL`, `wait_if_equals`, unmapped -> `EFAULT`, read-only page,
  a kernel address as a user pointer -> `EFAULT`, a range across `user_va_limit`, copies with preemption
  disabled, kernel-to-kernel copyin/copyout, a user address in a kernel-to-kernel copy -> `EFAULT`.
  User mode: 8 instructions on a twin through `hosted_user_enter`: instruction and write faults arrive and
  are resolved (`vm_fault`) and filled (`task_map`), registers and `d0` come back, `brk #0x4d2` with its
  immediate, an undefined instruction, the task's store read back by `copyin`. `svc`: SKIP (no seccomp
  under qemu-user).
- With `--no-tasks`: 21 passed, 0 failed (user mode skipped).
- Without the boot argument the boot is as before. `make -C runtime/xnuhost test`: passes, unchanged.

## Not tested, not done

1. `hosted_user_loop` itself and everything behind `sleh_synchronous` for a real task: syscalls, Mach
   traps, exceptions, signals, AST preemption of user code, exec (new twin), thread and task exit, the
   stack reset in `hosted_thread_exception_return`. Needs `kernel.kc` to reach launchd, and either the
   phone or an `svc` trap that works under qemu-user (work queue Q2).
2. `XH_TRAP_INTERRUPT` from a running twin with a kernel interrupt handler (the standalone kernel has none;
   the self-test acknowledges the line itself).
3. Hardware debug state (breakpoints, single step) is not given to the twin. SME state is not carried.
4. The task process lives until the last thread that ran in it is reaped (the pmap reference).
5. A copy to or from user-mapped device memory (a physical address outside the RAM window) is `EFAULT`.
6. For the booter: the stub fills `xh_trap.insn` for SIGILL only; for a brk the kernel reads the
   instruction itself (one `copyin`). Filling it for SIGTRAP would save that.
