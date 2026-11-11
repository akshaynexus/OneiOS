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

## How far it boots (qemu-aarch64 -cpu cortex-a710, 8 vCPUs, 4 GiB, `kernel.kc`, the real image set)

Seen on 2026-11-11 with series 0001-0015 and a collection from `kernel/prelink` (277 original kernel functions still
on the panic stub). In order, with the console in xnuhost's log:

1. `arm_init` to `kernel_bootstrap_thread`, corecrypto's POST (the sealed kernel), IOKit, 8 CPUs through PSCI.
2. The collection's kexts: AppleImage4 (with the software IOAESAccelerator of 0008), AMFI, Seatbelt, the trust caches,
   APFS; the root mount from xnuhost's verified disks; `bsd_init` complete; the SystemCryptex mount.
3. The Data volume's first start (0004): `/sbin/newfs_apfs` runs as pid 2. It is the first user process of the boot,
   so its dyld registers the signature of every shared cache file and `cs_native4k_prepare` reads and hashes all of
   them (5.8 GiB; 0014 does it on 8 threads: about 3 minutes here). The cache maps and slides (0013: 8 GiB region).
   Then `OneiOS Data: /dev/disk8s3 on /dev/disk4 (new)`, the mount, `seed verified: 6024 files (1216961847 bytes),
   14813 folders` (61 s). On a second start of the same store the volume is mounted as it is and launchd is the
   process that pays for the hashing.
4. `/sbin/launchd`: `hello, launchd`, every early boot task (fsck, mount-phase-1/2, data-protection, keybag,
   usermanagerd, MobileAssetEarlyBootTask, the user-space FIPS POST: all tests PASSED, tzinit, launchd_cache_loader,
   ...), `Early boot complete. Continuing system boot.`, then the daemons: about 125 processes in 5 minutes.
5. `SpringBoard` is started and exits with signal 5 each time (7 times in that run); `liveactivitiesd` traps (`brk`).

What that run showed is wrong or missing (none of it fixed yet unless a later line says so):

- `hosted: kernel fault inside the fault handler` ended the run during a daemon's copyout of the loaded-kext list.
  Fixed by 0015 (the count was per CPU; the handler sleeps).
- No IOMobileFramebuffer and no digitizer in the guest: only the 2D virtio-gpu console. backboardd cannot bring up
  a display, which is the likely reason SpringBoard does not stay (not proven: no user-space log was read yet).
- `MKB_INIT: FATAL KEYBAG ERROR: failed to load system bag` and launchd's `failed to find AppleKeyStore service`:
  UniXNU's AppleKeyStoreState stand-in is not built for this kernel. Fixed by 0022 (section below): the keybag
  boot task ends with `DONE MKB_INIT`.
- `Text page corruption detected for pid N` (3 processes), one `dyld` assertion (`!sMemoryManagerInitialized`), one
  `xpcproxy` fatal exception: user pages that fail their code-signature check or change under a process.
  **Fixed by 0025 and a change in xnuhost's task layer** (`0025-README.md`): two iOS processes could get one
  Linux process (task creation raced between vCPUs), xnuhost's waits ended early, and the host's text fixup made a
  page fail its hash the second time it was validated. With them: 14 minutes past "Early boot complete", 129
  processes, none of these lines.
- mount-phase-2: `failed to create overprovisioning volume` (ignored by the task); `DT_get_fstab_entries: failed to
  get filesystem entry at: /filesystems/fstab`.
- `OneiOS launch policy: no /private/var/oneios/bin/oneios-init in this seed: no service manager`.

- Later runs: `Debugger synch pending on cpu 0` at the first stackshot (fixed by 0017); once,
  `hosted: thread_create in task 85 failed (-110)` and a panic in `hosted_user.c:383` (xnuhost's wait counted
  interrupted futex calls, not time: fixed with 0025's commit).
- Why SpringBoard exits: `+[FBDisplayManager sharedInstance]` traps in FrontBoardServices (no main display), and
  chronod aborts in QuartzCore's `query_displays`: backboardd offers no display. The display stand-in is the board's
  Q4a.
- With the display (0020 and `display/`, its README): the framebuffer starts about ten seconds into the boot
  (`oneios-display: late probe: stage 10`), the console reaches `display.sock` through the guest's virtio scanout,
  and thermalmonitord, backboardd and MobileGestaltHelper open the framebuffer (`status 0`). No swap yet: in the
  one run that got there backboardd died 4.5 minutes later in `_pthread_wqthread_setup` (a new workqueue thread's
  first system call returned -1) and the kernel then stopped with `Console I/O from interrupt-disabled context`
  from a `printf` in `hosted_user_loop` (seen twice; the printf is fixed by 0023). The system call is 372,
  `thread_selfid`, which cannot fail in the kernel: the -1 is what the host returns for a `svc` it executes
  itself, so that thread's `svc` was not trapped, or it ran in another process's memory. Both ways are closed by
  0025 and its xnuhost change; in the first long run with them backboardd was still alive 7 minutes after it
  opened the framebuffer. Whether it then swaps is not known (no user-space log read).

- Touch and buttons (`hid/`, section below): the guest has a Digitizer / Touch Screen service and a buttons service;
  backboardd opens both and gets their events queued. No frame yet, so nothing can be seen to react.
- With series 0001-0025 (the display 0020, the key store 0022, 0023, the task layer's fixes with 0025) and a
  collection with 214 originals on the stub: a 1000 s run ends at its time limit with no panic; 123 services
  spawned, about 190 processes, `backboardd` and `SpringBoard` alive to the end, only `chronod` dead (abort in
  QuartzCore's `query_displays`, 40 s after backboardd opened the framebuffer). `display.sock` shows one flat colour,
  about two flushes a second; no swap from backboardd, no `session/ready`.

Not reached: a swap from backboardd, a frame from SpringBoard, `session/ready`. Nothing here has run on the phone.

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

## 0022-keystore.patch and keystore/ (q5-keystore, 2026-11-11)

The AppleKeyStore service in software: UniXNU's AppleKeyStoreState with OneiOS's class keys and key wrap. What it
is, what it answers and why it is **not a security boundary**: [keystore/README.md](keystore/README.md).

| Where | What |
|---|---|
| `keystore/OneiOSKeyStore.cpp`, `keystore/build-oneios-keystore.py` | the archive `liboneioskeystore.a` (the component's service and user client, the start, the class keys, wrap and unwrap), added to the link by `kernel/build-hosted.sh` |
| `osfmk/arm64/oneios_host.c` | `oneios_keystore_start()` (weak default) called from `oneios_board_late_start`, after the GPU, before the first user process |
| `bsd/vfs/kpi_vfs.c` `native_dataprotect_emulated` | UniXNU's `owner-dataprotect` (a file's protection class kept in the extended attribute `com.unixnu.dataprotect-class` on a volume without content protection) is on unless `owner-dataprotect=0` |

Work tree `HOSTED_OUT=keystore`. `com.apple.driver.AppleSEPKeyStore` stays out of the collection's roots.

**What ran** (PC, `qemu-aarch64 -cpu cortex-a710`, 8 vCPUs, 4 GiB, the real image set, a collection made from this
kernel with the committed prelink records of `a6c13fe` for boots 1 to 4, of the committed tree for boot 5; the first on a new Data store):

- At the board start: `oneios-keystore: AppleKeyStore started in software: system keybag loaded, no passcode, 31
  class keys, wrap and unwrap by AES key wrap. Not a security boundary.` The start's own test of the key wrap
  (wrap, unwrap, wrong class, changed byte; 32, 5 and 40 bytes) passes in the kernel; a failure would stop the start.
- launchd no longer logs `failed to find AppleKeyStore service` (0 times; once per boot before).
- launchd's keybag boot task, before: `MKB_INIT: FATAL KEYBAG ERROR: failed to load system bag`, `Could not save
  value:e00002e2`, `Rebooting...`, `REBOOTING INTO RECOVERY MODE.` Now: `oneios-keystore: first client: keybagd`,
  `selector 0 from keybagd: 0x0`, `selector 14 from keybagd: 0x0`, then `MKB_INIT: mount is with NO CPROTECT ...
  (/private/var/)`, `mount point (/private/var/) does not support Data Protection`, `****** DONE MKB_INIT ****`.
  The lines about `systembag.kb` not being there stay: `keybags/` is empty by design and the bag is the kernel's.
- `Early boot complete. Continuing system boot.` and the daemons, as before.

- **Boot 5** (the committed tree: series through 0017 plus 0022, a new collection; 8 minutes after "Early boot
  complete" and still running when stopped). Each selector's first call, as the service logs it:

  | Selector | From | Result |
  |---|---|---|
  | 0, 14 (`aks_get_system`) | keybagd | 0 |
  | 7 (lock state) | powerd | 0 |
  | 17 (device state) | backboardd | 0 |
  | 35 (extended device state) | powerd | 0 |
  | 10 (`aks_wrap_key`), 11 (`aks_unwrap_key`) | securityd | 0: the keychain's keys are wrapped and unwrapped |
  | 107 | keybagd | `0xe00002c7`, **not answered** (what it asks is not known; keybagd keeps running) |

  No selector failed other than 107. Not seen: a keychain item read back after a restart, and the
  protection-class attribute in use (PosterBoard's store): nothing was checked on the Data volume.

**What comes next.**

- `SpringBoard` still exits with status 5 each time it is started (12 times in boot 5): the display, not the key
  store (the lead's note: it dies in FBDisplayManager; `0020` and `display/` are that work).
- Selector 107 from keybagd, if something turns out to wait for it.
- Boots 1 to 3 (before the lead's 0016 and 0017, PC at load average 27 to 33) ended 50 to 100 s after "Early boot
  complete" in the task layer: twice `hosted: thread_create in task N failed (-110)` then `panic: hosted: no task
  process or thread for thread ... @hosted_user.c:383`, once seven "Text page corruption detected" lines, "Too many
  corpses being created", then `panic: Console I/O from interrupt-disabled context @serial_console.c:175`. A boot
  of the same collection with `oneios-keystore=0 owner-dataprotect=0` (boot 4) showed the old failure lines again
  and ran 7 minutes without a panic; boot 5, with the key store on, ran 8. Whether a working keybag task makes
  those stops likelier (more daemons get further at once) is not known: one boot each way.

## 0024-gpu-pool.patch and Metal's kernel side in gpu/ (native-adreno, 2026-11-11)

`0024` (`hosted_pmap.h`, `hosted_pa_in_gpu_pool`): the host RAM object's pages between the kernel's window and the
boot framebuffer (xnuhost's GPU pool, `/arm-io/virtio-gpu` `drm-host-pool`; at most 64 MiB) may be handed to the
host's `kernel_map` and `task_map`, as `0020` allows the boot framebuffer for kernel mappings. The virtio-gpu 3D
driver maps each context's response buffer from the pool into the client's task. Not run: xnuhost names a pool only
with a renderer, and the PC has none.

NativeAdreno (the `AGXAccelerator` under the collection's `IOAcceleratorES`, `AGXDeviceUserClient`) needs no patch:
it is in the GPU archive of `0005` and starts from `oneios_gpu3d_start` (`docs/gpu.md` 5.1, with the PC boot's
lines and what is not tested).

## hid/: touch, buttons and the light sensor's HID device (fable-hid, 2026-11-11)

No numbered patch: an archive (`hid/build-oneios-hid.py`, hooked into `kernel/build-hosted.sh`) whose start,
`oneios_hid_input_start`, is the last call of `oneios_drivers_start` (`drivers/bridge/OneiOSBridge.c`).
`hid/README.md` has the files, the evidence for the layout header, the buttons table and the log lines.

- `hid/OriginalHidDeviceABI.h`: the layout of the collection's original `IOHIDDevice` (24A446 iPhone18,2), checked
  at every build against what the kernelcache says (`hid/IOHIDDevice-24A446-v54.json`): 207 vtable slots with their
  pointer-authentication discriminators, 0xd0 bytes. Any driver that subclasses `IOHIDDevice` includes it.
- UniXNU's NativeHidInput as two devices (touch screen, buttons) fed by UniXNU-droidvm's virtio-input transport
  from xnuhost's `/arm-io/input-0`; `drivers/brightness/OneiOSALSDevice.cpp` built against the same header.
- It needs three pairs in the collection that were wrong or missing until 2026-11-11 (fable-prelink has them):
  `IOReportLegend::addReporterLegend(IOReporter *, ...)` (a PAC failure in `IOHIDDevice::start` without it) and
  `IOSharedDataQueue::getQueueSize` / `setQueueSize` (the panic stub when backboardd opens a service).

Seen under qemu-aarch64 (main at `3202197` plus this, 8 vCPUs, `--input-script runtime/xnuhost/tools/input-demo.script`):

1. 10 s into the boot: `droidvm input /arm-io/input-0: OneiOS touchscreen, touch`, both HID devices started under
   `IOHIDResource`, `ONEIOS ALS: on`, and `oneios-hid: light sensor: OneiOSALSDevice(IOHIDInterface(AppleUSBALSService))`:
   Apple's light sensor service attaches to our device.
2. The touch screen's and the buttons' interfaces wait (`matching deferred by IOHIDEventDriver`) until driverkitd
   is active, three to five minutes after launchd's early boot here. The collection then tries the DriverKit event
   driver first (`AppleUserHIDEventService`; `DK: ... waiting for server com.apple.driverkit.AppleUserHIDDrivers`),
   gives up after 120 s (`failed to launch server`: the driver extension's process does not start in this guest
   yet) and the kernel's `IOHIDEventDriver` takes both devices. Not a fault of this archive, but two minutes
   without touch; open.
3. `oneios-hid: touch screen: 1 user client(s) on the event service: IOHIDEventServiceUserClient (pid 84, backboardd)`
   and the same for the buttons: backboardd found the digitizer and opened it.
4. The script's events arrive as HID reports the family accepts (`last result 0x0`) and Apple's event driver
   queues its events for backboardd: `touch screen: 444 HID events queued for the service's client(s), 0 dropped`,
   `21 contacts begun (10 at an edge)`; `buttons: 66 HID events queued`.

Not seen: backboardd or SpringBoard acting on an event (no user-space log was read, and no frame is drawn yet).
Nothing ran on the phone.

