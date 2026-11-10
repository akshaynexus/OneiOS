# kernel/patches/oneios-hosted: XNU as a process under xnuhost

Owner: hosted-xnu. The machine layer that lets xnu-12377 run with no EL1 and no MMU, against xnuhost's
host-call ABI (`runtime/xnuhost/include/xnuhost/hostcall.h`). Date: 2026-11-10.

**State, plainly.** The hosted kernel builds on Linux and boots under xnuhost in `qemu-aarch64` through
`arm_init`, the whole VM bootstrap and into the first kernel thread (`kernel_bootstrap_thread`). It stops in
corecrypto's start-up self-test. No user-mode code path has run. Nothing has run on the phone. Details below.

## Build and run

```sh
scripts/bg.sh kernel-hosted kernel/build-hosted.sh     # -> out/kernel/current-hosted/xnu.macho
```

`build-hosted.sh` does not touch the VM kernel's build: own output root (`out/kernel-hosted`), own copy of
`kernel/device` (as committed) with `series` of this directory added to the board's patch series, defconfig
`kernel/device/configs/oneios_hosted_defconfig` (`CONFIG_XNU_HOSTED=y`, which reaches C and assembly as
`-DXNU_HOSTED=1` through `makedefs/MakeInc.def`). Every change in the patch is under `#if XNU_HOSTED`, but the
patch has **not** been built with the flag off: it is not in the board's series, so `kernel/build-linux.sh` is
unchanged.

The kernel needs the `kernel_map` / `kernel_unmap` host calls and `xh_info.kva_base/kva_size`, which the booter
landed in `runtime/xnuhost` (commit `bbe3bcc`). `hostcall.h` in the patch is my copy from before that: the same
structures, older comments; replace it with the booter's when the patch is next regenerated. Run (STAGE from
`runtime/build.sh STAGE`; checked with the committed xnuhost and a kernel built from scratch by `build-hosted.sh`):

```sh
qemu-aarch64 -cpu cortex-a710 STAGE/bin/xnuhost --mode bench --image IMG --kernel out/kernel/current-hosted/xnu.macho \
  --data DATA --cpus 1 --ram 1G --display none --no-input --no-tasks --width 64 --height 64 \
  --boot-args "debug=0x14e serial=3 -v kt_fixed_zones=120 -no_slto_panic ml-timeout-lock-panic-scale=40 tlto_us=0 \
armcpu-self-ipi=1 armcpu-spi-ipi=1 use_contiguous_hint=0 interrupt-masked-debug-mode=0 sched_preemption_disable_debug_mode=0 wdt=-1"
```

`-cpu cortex-a710`, not qemu's default `max`: `max` has a 512-byte `DC ZVA` block and XNU's `bzero` assumes
the 64 bytes every Arm core (and the S24 Ultra's) has; with `max` the first large `bzero` wipes its own stack.

## How far it boots (qemu-aarch64, 1 vCPU, 1 GiB)

Before: the EL1 kernel faulted in `OSRuntimeSignStructorsInSegment` on the link-time address
`0xfffffff00719c028` (the booter's finding; the cause was not the MMU: nobody had slid the Mach-O load
commands or applied the image's chained fixups).

Now, in order, with the console in xnuhost's log:

1. `arm_init`: load commands slid, chained fixups applied and signed, `Darwin Kernel Version 25.5.0 ...`.
2. `arm_vm_init`, `pmap_bootstrap`, `vm_page_bootstrap: 251517 free pages`, the kmem ranges, `zone_init`,
   `kalloc_init`, `VM bootstrap done` (the kernel's own mappings go through `kernel_map`).
3. `kernel_bootstrap` loads the first thread (`machine_load_context`, `Call_continuation`):
   `kernel_bootstrap_thread`, `sched_startup`, the log and firehose set-up, `kdp_core`, serial debugging.
4. **Stops**: `panic: "FIPS Kernel POST Failed (-2074)!" @corecrypto_kext.c:342`, from
   `source_corecrypto14_bootstrap` in `kernel_bootstrap_thread` (a `STARTUP` entry). Cause not looked at yet.
   First things to check: an instruction corecrypto's self-test uses that qemu's cortex-a710 or EL0 lacks, and
   the integrity (HMAC of `__TEXT`) part of the POST against a kernel whose text was patched by `static_if`.

Not reached: IOKit start, `bsd_init`, root mount, any user process. 25 instructions were emulated by xnuhost's
SIGILL path in the whole run, no interrupt was taken (the timer is not armed that early).

## What the patch does (files, functions)

New, `osfmk/arm64/hosted/`:

| File | What |
|---|---|
| `hostcall.h` | xnuhost's ABI header plus the proposed `kernel_map`, `kernel_unmap`, `kva_base/kva_size` |
| `hosted.h` | the conventions: `x18` = `struct xh_cpu *` of the vCPU, `TPIDR_EL0` = current thread, the interrupt mask as single loads and stores through `x18`, `HOSTED_FN`/`hosted_hc` for calling the vector's unsigned addresses from arm64e code |
| `hosted_asm.s` | `hosted_start` (entry: x0 boot_args, x1 vector, x2 vCPU), `hosted_cpu_start`, `hosted_hc`, the upcall entries, `hosted_call_on_stack` |
| `hosted_machine.c` | `hosted_init` (facts, upcalls), console (`hosted_console_putc`), `hosted_panic`, `hosted_irq_upcall` / `hosted_irq_poll` / `hosted_interrupt` (what `fleh_irq` and `return_to_kernel` are), `hosted_exception_upcall` (kernel faults: software-TLB fill, else `sleh_synchronous` on a synthesized `arm_context_t`), `hosted_cpu_idle`, `hosted_io_map`, `hosted_text_fixup` |
| `hosted_pmap.h` | included at the end of `pmap.c`: the host mapping as the pmap's TLB. `hosted_walk`, `hosted_kernel_fill` / `hosted_kernel_enter` (kernel_map), `hosted_user_fill` (task_map), `hosted_tlb_flush_region` / `hosted_tlb_flush_pmap` (kernel_unmap, task_unmap, nested pmaps), `hosted_pmap_task` / `hosted_pmap_destroy` (task_create, task_destroy), `hosted_copy_user_pages` (copyio through the window), `hosted_mmu_kvtop` |

Changed (all `#if XNU_HOSTED`):

- `start.s` `_start` -> `hosted_start`; `arm_init.c`: `hosted_apply_chained_fixups`, load-command slide, no TCR,
  scheduling-hygiene timeouts off.
- `mach/arm/vm_param.h`: `VM_MIN_KERNEL_ADDRESS` `0x2000000000`, `VM_MAX_KERNEL_ADDRESS` `0x3bffffffff` (XNU's
  -128..-16 GiB layout modulo 2^38, so the T1SZ = 26 table indices are unchanged), pointer strip for low
  addresses. `arm_vm_init.c`: the window is image and physical aperture at once (`physmap_base = gVirtBase`),
  `dynamic_memory_begin = xh_info.kva_base`, no low-globals alias. `pmap.c`: kernel pmap bounds, flush entry
  points, eager kernel mapping in `pmap_enter_options_addr`, no TTBR switch; `pmap.h`: `hosted_task`,
  `hosted_seq`. `tlb.h`: no `tlbi`. `io_map.c`: `ml_io_map` through the `io_map` host call.
- Low kernel addresses: `pmap_data.h` (`PVH_POINTER_BITS`), `zalloc.c` (`zone_pva_t` page bit),
  `mach/vm_param.h` (`vm_unpack_pointer`), `smr_hash.h`.
- Interrupt mask and current thread: `cpu_data.h`, `asm.h` (`TPIDR_EL1` -> `TPIDR_EL0` in assembly),
  `machine_routines_common.c` (`ml_set_interrupts_enabled_with_debug`), `machine_routines.c`,
  `preemption_disable.c`, `pmap_internal.h`, `cpu.c` (idle through `cpu_idle`), `cswitch.s`
  (`set_thread_registers`, no EL1 PSTATE fields), `machine_routines_asm.s` (`mmu_kvtop`, `set_mmu_ttb`,
  `set_tcr`, `ml_pac_safe_interrupts_*`, `CHECK_THREAD_STATE_INTERRUPTS`).
- `pe_serial.c`: the host console; `debug.c`: `panic` -> xnuhost's log and `halt(XH_HALT_PANIC)`.
- `kalloc.c`: a kernel with no fixed or variable type views (stock clang, no kext) no longer asserts.

## Written and never run

`hosted_user_fill`, `hosted_pmap_task`, the user and nested-pmap flushes, `hosted_copy_user_pages`: compiled,
no caller yet. `hosted_irq_upcall`: no interrupt arrived before the stop. Secondary CPUs: `hosted_cpu_start`
parks them.

## Not written (what remains before launchd)

1. The corecrypto POST stop above, then whatever `kernel_bootstrap_thread` hits next (IOKit start, ArmCpu's
   GIC and timer through xnuhost's emulation, `bsd_init`).
2. Return to user: `hosted_thread_exception_return` is a panic. To write: the `thread_run` loop
   (`arm64_thread_exception_return`, `thread_bootstrap_return`, AST handling as `return_to_user`), the
   `xh_user_state` <-> `arm_saved_state64` conversion, trap -> `sleh_synchronous` / `sleh_irq`, the thread's
   Linux twin (`thread_create`/`thread_destroy`, keyed by `pmap->hosted_seq`), a fill after each resolved
   user fault.
3. copyio (`osfmk/arm64/copyio.c`, the `_bcopyin` family) on `hosted_copy_user_pages` + `arm_fast_fault` /
   `vm_fault`.
4. Device memory mapped by IOKit at kernel addresses (not through `ml_io_map`): a kernel fault whose page
   table entry points into the I/O aperture has to be emulated by the host (open request 1 below).
5. Secondary CPUs (`cpu_start` / PSCI entry -> `arm_init_cpu`), the `timer_set` host call instead of trapped
   `CNTV_*`, user-address caps from `xh_info.user_va_limit`, `text_fixup` in `cs_text_fixup_page`
   (`bsd/kern/ubc_subr.c`: `hosted_text_fixup` exists, the call is not wired).
6. A standalone kernel leaves its one weak bind (`native_source_storage_start`) NULL; inside `kernel.kc`
   XnuPrelink resolves it. `kernel.kc` itself (the prelink) is still absent.

## Requests to the booter (also on the board)

Landed (commit `bbe3bcc`): `kernel_map` / `kernel_unmap` with `xh_info.kva_base/kva_size` and a raised
`vm.max_map_count`; PSCI `CPU_ON` entry with x1 = vector, x2 = the vCPU's `struct xh_cpu *`; the rule that an upcall
which migrated its thread ends with `frame_resume` on the current vCPU, and that host calls are made masked.

Open (all four answered by xnuhost `9bb0dca` and `242ee8f`; see the 0003 section below):

1. `mmio_emulate(cpu, frame, phys)`: emulate the load or store at `frame->pc` against a device address, for
   device pages IOKit maps at kernel addresses (not through `ml_io_map`).
2. `TPIDR_EL0` in user mode: iOS reads the CPU number from its low 12 bits (`MACHDEP_TPIDR_CPUNUM_MASK`,
   `_os_cpu_number()`); with the TSD base there, per-CPU indexing in libmalloc and libdispatch goes out of
   range. Suggest `text_fixup` also rewrites user `mrs xN, TPIDR_EL0` to `mov xN, #0`.
3. `xh_frame` has no signal number: a `brk` and an undefined instruction both arrive with `esr` 0 (the kernel
   reads the instruction to tell them apart).
4. `frame_resume` of an unmasked frame while the interrupt line is up: re-raise it, or the interrupt waits for
   the next unmask.

## 0003-smp-timer-irq.patch (fable-smp, 2026-11-10)

On top of 0001, all under `#if XNU_HOSTED`. Work tree `HOSTED_OUT=smp`.

| Where | What |
|---|---|
| `hosted_pmap.h` `hosted_kernel_device`, `hosted_machine.c` `hosted_exception_upcall` | A kernel data abort whose page-table entry names an address inside `xh_info.io_phys/io_size` (IOKit's IOMemoryMap of a board device, as `ArmCpu::mapGic`) is handed to the `mmio_emulate` host call with the fault's frame; the frame is resumed past the instruction. Such a page is never given to `kernel_map`. An instruction the host cannot emulate stops the kernel with the registers, instead of faulting forever. |
| `start.s` `LowResetVectorBase`, `hosted_smp.c` `hosted_secondary_entry` | The reset vector page ArmCpu passes to PSCI `CPU_ON` branches to `hosted_cpu_start`. The new vCPU finds its CPU data by `xh_cpu.index` (the node's `reg`) and runs `arm_init_cpu` on that CPU's interrupt stack. |
| `kernel/hosted-fixup.py` (run by `build-hosted.sh`, then the seal again) | `mrs xN, MPIDR_EL1` -> `ldr wN, [x18]` (the vCPU index) in the linked kernel: 7 sites, XNU's and ArmCpu's. At EL0 Linux answers MPIDR_EL1 itself with one value for every core, and ArmCpu parks a secondary whose MPIDR is not its affinity. Done after the link because the text is under corecrypto's HMAC (a rewrite at boot fails the POST with -2074). **fable-prelink:** kexts of the collection that read MPIDR_EL1 need the same rewrite. |
| `machine_routines.c` `ml_set_decrementer`, `ml_get_decrementer`, `ml_get_timer_pending`, `_enable_virtual_timer`; `hosted_smp.c` `hosted_timer_*` | The decrementer is the `timer_set` host call (one call instead of a trapped `CNTV_TVAL_EL0`). It still arrives as INTID 27 through ArmCpu's handler, which calls `rtclock_intr`. |
| `hosted_machine.c` `hosted_kernel_esr` | Uses `xh_frame.signo` (SIGTRAP = `brk`) when the host gives it. |
| `hostcall.h` | xnuhost's header as of `242ee8f`. |

Boot arguments for more than one CPU: add `sheng-smp=1` (ArmCpu starts no secondary core without it). `armcpu-spi-ipi=1`
and plain SGIs both work in xnuhost's GIC. Bench runs without a host topology: `--set clusters=EEPPPPPP`.

**What ran, what did not.** The patched kernel builds (`HOSTED_OUT=smp kernel/build-hosted.sh`), passes corecrypto's
POST with the fixup and the second seal, and boots with `--cpus 4` to the same stop as 0001 alone: `image4 interface
not available` in `trust_cache_runtime_init`, which is before ArmCpu starts. So **`hosted_kernel_device`, the secondary
entry and the timer interrupt path have not run inside XNU**: they need `kernel.kc` (or the lead's local image4 skip).
The host side of each (`mmio_emulate`, PSCI `CPU_ON` entry, timer, SGIs, the re-raise) runs in xnuhost's test kernel.
