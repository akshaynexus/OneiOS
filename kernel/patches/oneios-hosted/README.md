# kernel/patches/oneios-hosted: XNU as a process under xnuhost

Owner: hosted-xnu. The machine layer that lets xnu-12377 run with no EL1 and no MMU, against xnuhost's
host-call ABI (`runtime/xnuhost/include/xnuhost/hostcall.h`). Date: 2026-11-10.

**State, plainly.** The hosted kernel builds on Linux and boots under xnuhost in `qemu-aarch64` through
`arm_init`, the whole VM bootstrap and into the first kernel thread (`kernel_bootstrap_thread`). It stops in
corecrypto's start-up self-test. No user-mode code path has run. Nothing has run on the phone. Details below.

The archives of this layer (the disks, software AES, the key store, the Wi-Fi, HID, audio, the GPU, the display) are
each built by `oneios-tool kernel gen NAME` (`tools/oneios-tool/crates/oneios-kernel`, one module per archive), which
`kernel/build-hosted.sh` adds to the board's archives. Until 2026-11-13 these were Python scripts in the archives'
folders (`NAME/build-oneios-NAME.py`); where a comment in a source or a patch here still names one, it means the
module `build_oneios_NAME.rs` (the scripts' outputs are frozen in `tools/oneios-tool/tests/kernelgen/golden`).

## Build and run

```sh
scripts/bg.sh kernel-hosted env HOSTED_OUT=lead kernel/build-hosted.sh     # -> out/kernel/current-hosted/xnu.macho
```

Linux (stock clang and xnu-linux-toolchain) or macOS (Xcode's clang and iphoneos SDK; `ALLOW_XCODE=1` for an Xcode
other than the pin): [kernel/README.md](../../README.md) "Build", [docs/building.md](../../../docs/building.md) 2.

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
   (Since 2026-11-24 it runs as `newfs_apfs -e -x -v Data`, and again without `-x` when it exits 64, usage: iOS 16.5's
   `newfs_apfs` (20F66, the Galaxy A56, issue 73) has no `-x`, and the first A56 boot of our image panicked with
   `newfs_apfs failed: wait status 0x4000`, then `offline scratch Data mount failed: 5`.)
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

- **The first phone boot (S24 Ultra, 2026-11-11)** got through the kernel, the root mount, the cryptexes, the
  shared cache's hashing (5791 MiB in 23 s), 8 CPUs, the GPU, the HID devices, and stopped at the first user
  process: `newfs_apfs failed: wait status 0xb`, `panic: offline scratch Data mount failed: 5`. With
  `sheng-exc-name=newfs_apfs` in `IMAGE/xnuhost.conf`: a permission fault at `0x81f8d4ed30` in libobjc's
  `realizeClassWithoutSwift` (from `map_images`), which takes a class's signed isa with a constant mask (`and x0,
  x8, #0x7fffffffff8`). The phone's Linux has 39-bit user addresses, so the CPU's authentication code starts at
  bit 39 and four bits of it survive that mask; under qemu-user (48 bits) and on an iPhone (47) none does. **0027**:
  on a host narrower than 47 bits iOS user space runs without pointer authentication (`arm_user_jop_disabled()`
  is true: the kernel signs nothing for user space; every task thread runs with its keys off through
  `PR_PAC_SET_ENABLED_KEYS`). Built and booted on the PC, where it changes nothing (47 bits); **not yet run on the
  phone**, and the PC cannot run the keys-off mode (qemu-user has no such prctl).

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

## iOS 16.5 userland on the Galaxy A56 (port-a56, 2026-11-21; docs/a56.md)

Not built or run here — host-tested only (the series resolves, the staged and golden tests pass). The fork
(`Musicalista/OneiOS-A56`) booted 20F66 to the lock screen on the A56 with these, on this XNU pin; 27 on the
S24 Ultra takes none of the new paths by default (since the 0068 fix of 2026-11-21; before it, 0068 broke every iOS 27
boot: below).

- `0068-hosted-ios16-registered-ports.patch` (`hosted_task_ios16.defs`, `osfmk/mach/Makefile`, `osfmk/conf/files`,
  `ipc_kobject.c`, `ipc_tt.c`): the 16.5 Mach user ABI carries the registered ports in one OOL descriptor; compat
  shims translate to the 3-port kernel RPCs. **Opt-in since 2026-11-21: `oneios-legacy-registered-ports=1`**
  (roottweak writes it for a 20F66 image). As ported it changed `task.defs`' kernel server form, and the 16.5
  routines are the same mig ids (3403, 3404) as 27's `_kernelrpc_mach_ports_register3`/`lookup3`: iOS 27's launchd
  got an OOL reply, `mach_ports_lookup() failed: -300` (MIG_TYPE_ERROR), and the boot ended with the panic
  `initproc exited` (PC boot, 2026-11-21, 184 s in; every kernel built from `main` after the A56 merge `b2fa083`).
  Now `task.defs` is stock, the 16.5 form is its own MIG file (`hosted_task_ios16_server.c`), and `mig_init` swaps
  its two stubs into the dispatch table only with the boot argument (it logs `hosted: mig ids 3403-3404 answer in
  the iOS 16 registered-ports form`).
- `0077-hosted-ios16-bootstrap-replies.patch` (`ipc_policy.c`): `oneios-legacy-bootstrap-replies=1` accepts
  ordinary MIG reply ports at the bootstrap port; default 0 keeps the 27 defense.
- `0072-iomfb-ios16-swap-record.patch` (`IOUserClient.cpp`): rewrites 20F66's 0x46c swap record, 1-scalar
  frame info, 0xa68 surface infos and 0x18 limits into 24A446's shapes and back. Default on but
  size-triggered, so inert for 27 clients. `oneios-iomfb-swap16=0` turns it off.
- `0073-mac-exec-label-report.patch` (`mac_vfs.c`): logs which MAC policy refuses `mac_cred_label_update_execve`
  (at most 200 lines); `oneios-mac-exec-skip=NAME,...` ignores a Sandbox refusal for named executables.
- `0074-hosted-shared-cache-volume.patch` (`vm_unix.c`): the shared-cache volume check passes when the SystemOS
  union sits above the verified OS mount.
- `0004` gains `oneios_hosted_amdgpu_probe` (`oneios-amdgpu-probe=1` only); `0029` logs scalar/struct sizes.
  `ONEIOS_CRYPTO=original` (`kernel/build-hosted.sh`, `optional/original-corecrypto-only.patch`,
  `kernel/patches/unixnu/0003`) links no source corecrypto; default `source`. The owner's licence call stands
  (kernel/README.md "Releases and licences").

## 0034-syscall-failures.patch and 0035-staged-apps-on.patch (lead, 2026-11-11)

Found on the phone: SpringBoard reached the Home Screen with an empty Dock and no icon, the App Library was empty,
and on unlock SpringBoard died in an assertion of FrontBoardServices, `Invalid condition not satisfying:
processIdentity` (`+[FBSSceneClientIdentity identityForProcessIdentity:]` with nil, called from SpringBoard's scene
request for Spotlight while the lock screen's views appeared: named with `scripts/debug/dsc.py` (now `oneios-tool sym`) from the `sheng-exc`
backtrace).

- **0035.** The removable system apps (Camera, Photos, Safari, Messages, Weather, ...: 52 bundles) are not in the
  sealed `/Applications`. They ship in the OS volume's `/private/var/staged_system_apps`, and a restore's data
  migration installs them. The Data seed marks that migration done and carries no copy; UniXNU's kernel shows the
  folder over `/Applications` as a read-only union (`native_staged_apps_union`, bindfs, before Data covers
  `/private/var`), but only with `owner-staged-apps=1`, which its boards pass and xnuhost does not. Under
  `XNU_HOSTED` it is now on unless `owner-staged-apps=0`, and says how far it got:
  `oneios-apps: /private/var/staged_system_apps shown over /Applications: stage 5 of 5, status 0`.
  The LaunchServices database is already dropped at every boot (`native_ls_store_drop`, since 0031 made the addon
  path run), so lsd registers what the view shows.
  **Not shown by this:** why Settings, Phone and the other apps of the sealed `/Applications` were missing too
  (UniXNU's note for the same state is "the Home Screen showed Settings alone"), and whether the assertion goes
  with the apps there. Built; not run.
- **0034**, a debugging aid, off without its argument: `oneios-sysfail=NAME[,NAME...]` (`*`: every process) prints
  each file system call of those processes that fails, with the path:
  `oneios-sysfail: installd[99] mkdir(/private/var/installd/Library) -> errno 13`. `ENOENT` is left out unless
  `oneios-sysfail-enoent=1`; at most `oneios-sysfail-max` lines (800). The guest's user-space log is not on the
  console; this is what shows a daemon that cannot make or open what it needs.

## 0036-icon-store-generation.patch and 0037-lsd-first-boot.patch (lead, 2026-11-11)

- **0036.** iconservicesagent keeps the icons it drew in
  `.../systemgroup.com.apple.lsd.iconscache/Library/Caches/com.apple.IconsCache/<UUID>/*.isdata`, keyed by build and
  language only: an icon drawn while its GPU open was refused (before 0033) or while no app was registered (before
  0035) stays blank. After iOSPlus10's `userland/metal/plugin/icon_store_rule.h`: a stamp `.oneios-generation-N` in
  that folder; when it is not `ONEIOS_ICON_GENERATION` (1) or `oneios-icons-generation=N`, every `.isdata` goes
  before launchd and the stamp is written. Raise the number only when what draws icons changes: redrawing some 500
  icons costs minutes. A store that does not exist yet is left alone. Console: `oneios-icons: the drawn-icon store
  had another generation: N icons removed, now generation 1`, then `... has generation 1: kept` on later boots.
- **0037.** lsd drops the apps of every volume but the system's own the first time it starts after a boot
  (`_LSServerCleanEphemeralMobileContainersForFirstBoot`), unless the POSIX semaphore `com.apple.lsd.firstboot`
  exists. The staged-apps view of 0035 is a volume of its own, so from the second boot of a Data store on its apps
  would be gone. The semaphore is made before launchd (Droid-VM's `DroidVMLaunchPolicy.c`,
  `mark_lsd_first_boot_done`), only while the view is on: `oneios-apps: lsd's first-boot semaphore made (0)`.

Both built; not run.

## 0040-0043: the CPU path's kernel side (perf, 2026-11-12)

- **0040, the idle cache flush off.** `cpu_idle` calls `platform_cache_idle_enter` (this board is not
  `APPLE_ARM64_ARCH_FAMILY`), which flushes L1 by set and way (`FlushPoU_Dcache`). Under xnuhost each `dc cisw` and each
  CLIDR, CSSELR and CCSIDR access is an undefined instruction at EL0 that xnuhost emulates with a signal: 6 traps per
  idle entry with `setway-skip` (1024 without), about 600 idle entries a second on an idle lock screen
  (`docs/hardware.md` 7d). A vCPU is a Linux thread; it never powers a core down, and Linux keeps the caches coherent,
  so the flush had no effect. Now the function is empty under `XNU_HOSTED` (in the built kernel: `bti c; ret`). In the
  phone log, the stop's `emulated: insn` lines should no longer list `0xd5087e42` (`dc cisw`) or the CLIDR/CSSELR/CCSIDR
  reads among the most frequent.
- **0041, `oneios-perf=SECONDS`** (default 30, 0 = off). A kernel thread prints, for each period: user stays, system
  calls by kind (BSD, Mach, platform) with the vCPU time each took (from the trap to the vCPU's next user entry or
  idle: the work up to the return or the block, not the wait), faults filled from the page tables (with the fill's
  time) and faults sent to `vm_fault` (with their time), refills, interrupts of user code, twins made and dropped; the
  pmap's user and kernel maps and unmaps; the TLB lock (`hosted_pmap.h`): times taken, times it was held by another
  CPU, the time spent waiting and held (and their share of one vCPU); copyio copies and faults; the 12 BSD system
  calls and 8 Mach traps with the most calls (`NUMBER CALLS xMEAN_US`; `bsd/kern/syscalls.master` and
  `osfmk/kern/syscall_sw.c` name them; Mach traps are negative). With xnuhost's `perf:` lines (`docs/booter.md`
  2.6a) this says which calls the kernel's share goes to, and whether the one TLB lock is a bottleneck.
- **0042, `oneios-iobusy`** (on unless `oneios-iobusy=0`): at 30, 60, 120, 240 and 480 s after bsd_init (the first
  phone boot printed all five at once: the guest's counter is the host's, which runs since the phone booted), the services
  that keep IOKit busy and have no busy child (`oneios-iobusy: 60 s: busy 1: NAME (class CLASS, id 0x...), inactive 0`)
  and the count. configd's InterfaceNamer waits up to 4 minutes for IOKit to go quiet before it names en0 (261 s
  after the Wi-Fi driver published it in prelink's session 2); these lines name what it waits for. **Read on the PC
  (2026-11-13):** the one busy service is the service root, `oneios,xnuhost (class IOPlatformExpertDevice)`, held by
  `StartIOKitMatching` until driverkitd sends its dexts' personalities (`KextLog: driverkitd is active`). InterfaceNamer
  waits for that only for an interface it does not know; the Wi-Fi nub now reserves en0's unit
  (`network-interface-unit-en`, `net/README.md` "en0's name"), so en0 no longer waits for it.
- **0043, the LaunchServices database is kept when it holds the apps.** The base's `native_ls_store_drop` deleted
  LaunchServices' database at every boot, on purpose (its comment: the staged apps are registered only by a rebuild,
  and a database saved before they were registered kept them out). So lsd registered every app again at every boot:
  6 to 11 minutes on the phone. In the diagnostic boot of 11:53 (kernel.kc 8eff915f) lsd ran from +82 s to +456 s;
  chronod started its own work at +466 s, and SpringBoard's main thread had waited in `kevent_id` under ChronoServices'
  `-[CHSWidgetDescriptorProvider initWithConnection:...]` for a queue whose thread was in a synchronous XPC to chronod
  (`-[CHSChronoServicesConnection _subscribeToExtensions]`), so no touch was handled until then. 0037's semaphore does
  not change that. Now (`oneios-ls-keep=2`, the default) a container's database is kept when its largest file names
  every `NAME.app` entry of `/Applications` (the system's and the staged view's) in its strings (a name ends at
  `.app` and starts after a `/` or a NUL); one that lacks any of them is dropped as before. `oneios-ls-keep=1` keeps
  every one, `0` drops every one. One line per container: `oneios-ls: CONTAINER/FILE: N bytes, names K of M apps of
  /Applications: kept` (or `dropped (...)`). If K stays 0 on a database that does hold the apps, the database does
  not store the names as plain text, and only `oneios-ls-keep=1` keeps it. The scan was tested on the Mac against
  sample bytes (a name across two 1 MiB chunks, `Notes.apps` against `Notes.app`); not on a real database.
- **0044, `oneios-enable-jobs`** (default `com.apple.analyticsd`; `-` for none). Before launchd, each listed job's
  `<true/>` in the Data store's `/private/var/db/com.apple.xpc.launchd/disabled.plist` becomes `<false/>` (enabled),
  and the file is written again (root's, 0644). The image's daemon policy disabled analyticsd (vphone's base27); then
  CoreAnalytics' clients retry each synchronous send with a new connection: in the same boot backboardd spun at 217%
  after a brightness change (`CoreAnalytics::Client::retryXpcMessageHelper_sync` and
  `__kernelrpc_mach_port_construct_trap`) and the brightness never reached the panel. `userland/config/daemons.toml`
  now keeps analyticsd, so a new seed has no such entry; 0044 fixes a Data store made from an older seed without a new
  one. One line per label: `oneios-jobs: com.apple.analyticsd: enabled in disabled.plist (it was disabled)` or `not
  disabled there`. The edit was tested on the Mac on a plistlib file.

Built on the Mac (`HOSTED_OUT=perf`, series through 0044, DEVELOPMENT and the RELEASE reference).

**First phone run** (combined boot of 2026-11-12 12:30, kernel.kc 1e297354, runtime 0.1.10-dev, 30 s windows):
- 0040: done (no idle flush). 0043: `names 52 of 52 apps of /Applications: kept`, lsd no longer rebuilds. 0044:
  `com.apple.analyticsd: enabled in disabled.plist`; analyticsd runs (15-30 % CPU), but backboardd's CoreAnalytics queue
  still loops in `retryXpcMessageHelper_sync` (the thread is runnable in every sample: each send fails at once and is
  sent again). Not solved.
- 0041: 150,000-240,000 system calls and 180,000-520,000 faults per 30 s. The top BSD calls by count: 75 `madvise` (35,000-45,000
  per 30 s, about 120 us each: the most kernel time of any call), 375 `kevent_id`, 368 `workq_kernreturn`, 478
  `bsdthread_ctl`, 153 `pread`, 305 `psynch_cvwait`, 220 `getattrlist` (250-350 us), 381 `__mac_syscall`; the top Mach
  traps: -47 `mach_msg2_trap` (about 90 us), -18 `mach_port_deallocate`, -14 `mach_vm_protect`, -12
  `mach_vm_deallocate` (100-130 us). malloc's `madvise` frees pages, which the pmap flushes one by one: a likely source
  of the many unmap orders below. Faults split about evenly between fills from the tables and
  `vm_fault`. The fills' own time was 77 ms per 30 s in the first window, then 44-572 s (summed over vCPUs and with the
  time a filling thread was switched out) once the TLB lock got busy: the one global lock was taken 0.3-1.9 million
  times per 30 s, held 38 % of a vCPU in the first window, and waited for 14 % of a vCPU in the first window and up to
  41-47 s per 30 s (one and a half vCPUs spinning) later. User unmaps rose to 140,000-500,000 per 30 s (more than the
  maps): a flush of the shared region (a nested pmap) sends an unmap order to every task, each a round trip of about
  30 us made with the global lock held. The next step: a lock per pmap, and a flush of a nested range sent only to the
  tasks that mapped in it.
- 0042: no result (the timing fault above; fixed).

## 0045-0046: the TLB lock per task, the shared-region fan-out, fault-around (perf, 2026-11-12)

- **0045, a TLB lock per user pmap.** The one global word (`hosted_tlb_lock`) ordered every task's fills, flushes and
  copies against every other's, and was held across task orders: on the first combined boot it was waited for up to
  41-47 s per 30 s. What the lock orders is per address space (a fill of P reads P's entry and maps in P's task; a
  flush of P unmaps in P's task; a copy reads P's entry and touches the page), so each user pmap now has its own word
  (`pmap->hosted_tlb_word`), taken the same way. The kernel pmap keeps the global word. A kernel fault inside a user
  copy no longer meets a held lock (a different word).
- **0045, the shared-region fan-out.** A flush of a nested pmap (the shared region) sent an unmap order to every task
  that nests it. Each user pmap now records the 32 MiB blocks of its nested region it filled pages in
  (`pmap->hosted_nested_seen`, 512 bits; the last means "beyond the 16 GiB covered"); the bit is set under the task's
  word before the map, and the fan-out reads it under the same word after the entry changed, so a task without the bit
  has no host mapping in that range and is skipped. A full flush of a task clears its bits. The commpage still goes to
  every task. pmaps_lock is still held across the remaining orders (it keeps the list; a pmap freed under the loop is the
  alternative).
- **madvise (item 3 of the plan): not batched.** arm's pmap strips `PMAP_OPTIONS_NOFLUSH` and makes each page
  operation complete its own flush (`sync_tlb_flush` before it returns; `pmap_flush` is empty), so unmap orders cannot be
  joined across pages without changing that contract. 0045's report line says where the unmaps come from instead:
  `oneios-perf:   user TLB words taken T, W waited (U us waiting); unmaps: A for a task's own flushes, F from N
  shared-region or commpage flushes, which skipped S tasks that never filled there; fault-around maps M`.
- **0046, `oneios-fault-around=N`** (pages, a power of two up to 16; off by default). After a fill, the other pages of
  the N-page aligned window whose entries already allow access (valid, reference bit set, in the RAM window) are mapped
  too, with what each entry allows, under the same lock and rule as the fill. A TLB may hold any entry the tables allow;
  a later change is flushed and unmapped as before. On the first phone boot half the faults were fills; each costs a
  trap and a release (about 40 us), a fault-around map one deferred mmap in the task (about 40 us). Worth it only when
  most neighbours get used: `filled from the tables` against `fault-around maps` in oneios-perf says.
- Built on the Mac (`HOSTED_OUT=perf`, series through 0046, DEVELOPMENT and the RELEASE reference). Not run: the hosted
  kernel cannot run in the Mac's container (no pointer authentication), so these are measured on the phone only.
- **Phone, round 2** (boot 14:12, kernel.kc 29bef797, `oneios-fault-around=8`): the user words were taken 0.7-1.1 million
  times per 30 s and waited for 1.7-6.7 s (the global word: 41-47 s before); unmaps: 234,435 for tasks' own flushes and
  4,400 from 49 shared-region flushes, which skipped 2,432 tasks. But a fill from the tables took about 2.1 ms (26,576
  fills, 56 s per 30 s) against 77 us for a vm_fault: fault-around made 500,000-930,000 maps per 30 s, three per
  fault, mostly the same windows again (each vm_fault's refill did its window again), and each fill's maps waited for
  the task's queue while another thread of the task carried out earlier maps (100-130 us a release). Fixed by 0050 and
  xnuhost's queue lock (below).

## 0050-0051: fault-around once per window, kernel_task's CPU (perf, 2026-11-12)

- **0050.** A window is faulted around once until the task's next flush of it (16 slots per pmap,
  `pmap->hosted_around`, cleared by `hosted_task_unmap_locked` for the windows a flush covers). A page of a done window
  that becomes valid later gets its own fault and fill of that page only. The report adds `windows done already W`.
- **xnuhost (same commit):** the map queue has its own lock (`task.qlock`); a `task_map` adds to it without waiting for a
  release whose thread is carrying out earlier maps (the task's order lock), which is what made the fills wait with the
  task's TLB word held. The order is unchanged: an order or a release takes both locks to take the queue.
- **0051.** oneios-perf adds `kernel_task: N threads, T us of CPU (P% of a vCPU); most: NAME T ... #ID@0xCONT T ... us`:
  the kernel's own threads (pager, compressor, IOKit work loops, thread calls, idle threads with the interrupts they
  take), by name or by their continuation (unslid; name it against the kernel). A stall showed kernel_task at 127 %. The
  kernel's work for a user trap is charged to the user thread, so it is not in this line.
- Expected per fill without fault-around: a walk and one queue add (a host call of about 1 us), 3-5 us (round 1's first
  window, before the global lock got busy: 77 ms for 27,605 fills, 2.8 us each); the page's mmap (about 30 us) is paid
  at the next release, as before. With `oneios-fault-around=8` and 0050: up to 8 queue adds, about 10 us, and at most
  7 extra maps per window per flush.
- Built on the Mac (series through 0051, with the other agents' 0047-0049, DEVELOPMENT and the RELEASE
  reference); not run.

## 0039-gpu-client-list-and-texts.patch (gpu, 2026-11-12)

- **One list for both gates of a Metal device.** A process other than backboardd gets a device only when the
  accelerator's grant (or its sandbox) lets it open the user client and the plug-in's `NativeAdrenoMetal.clients`
  (beside its bundle) names it. The grant's list was the kernel's (`droidvm-gpu-clients`, default in the GPU archive),
  the plug-in's the Data seed's (roottweak), so another list needed a new seed, and a new seed starts the Data store
  over. Now `native_gpu_addon_prepare` (`bsd/kern/kern_exec.c`, before the addon mount) writes the file into the mount's
  source folder at every boot from `oneios_gpu_plugin_client_list` (`gpu/OneiOSAdreno.cpp`): `oneios-gpu-plugin-clients`
  when given, else the grant's list. One line: `oneios-gpu-list: <path> "<list>": written (0)` (or `unchanged (-1)`,
  or `not written (errno)`). A kernel without the GPU archive's Metal side writes nothing.
- **The plug-in's text rows, one line a row.** The plug-in streams text as open types (`0x41477fff`/`0x41477ffe` start a
  row, `0x414770cc` appends a character): a client's reason for having no device, the render server's phase results.
  0031 logged each as an `oneios-gpu-open` line, one character a line, and the render server's rows used most of the
  boot's 400 lines. `hosted_gpu_open_note` (`iokit/Kernel/IOUserClient.cpp`) now keeps a row per process (8 at once) and
  prints it whole: `oneios-gpu-text: iconservicesagent[95] no-device device: Vulkan(VkResultCode(-3))`, at the next
  row or other marker of that process, at 63 characters, or from the GPU report thread once the row is quiet for a
  second. Telemetry types `0x41478000` and up are not logged. Rows do not count against the 400 lines (at most 2000).
- Built on the Mac with the series through 0039 and linked into `kernel.kc`; not yet run.

## 0038-proc-pid-declared.patch (mac-kernel, 2026-11-12)

`hosted_machine.c`'s thread dump (0009, 0012) calls `proc_pid()`, which `osfmk/vm/vm_protos.h` declares only
`#if DEVELOPMENT || DEBUG`. The DEVELOPMENT kernel compiled; the RELEASE reference of the prelink
(`XNU_FILESET_REFERENCE=1`) did not on the Mac: Xcode 27.2's clang makes an implicit declaration an error, and on
Linux `kernel/linux/clang-wrapper`'s `-w` hid it. The patch declares it at file scope, as `osfmk/kern/thread.c` and
`task.c` do. No code changes. The file is the hosted layer's own (`osfmk/arm64/hosted/`), so there is no
`#if XNU_HOSTED` around it. Built on the Mac (both kernels); not run.

## 0032-app-cryptex-authenticated.patch (amfi, 2026-11-11)

App cryptex daemons (`webinspectord`, `webbookmarksd`) crash-looped: AMFI's self constraint for their
trust cache category (1 or 2) needs the program on the root volume or on an authorized authapfs volume, and the App
volume is neither here (no personalized manifest, so APFS cannot authorize its root hash). The volume is read-only on
the block device xnuhost verifies chunk by chunk, which `oneios_cryptex_views` has just checked, so `VNOP_IOCTL`
answers `FSIOC_AUTH_FS` with success for that mount (`oneios_app_cryptex_authenticated` in
`bsd/kern/oneios_hosted_root.c`; compared by fsid, read-only, only when no vnode is passed to authenticate against).
User space cannot ask this ioctl. Other volumes go to APFS unchanged; AMFI still evaluates every other fact.
At the first answer the kernel logs
`OneiOS: AMFI launch constraints: App cryptex volume answered as authenticated (FSIOC_AUTH_FS)`. Full notes:
`kernel/prelink/README.md`, "AMFI and launch constraints". Run on the phone (boot of 2026-11-11 23:00, saved as
`c2`): the line came at +108 s; `webinspectord` and `webbookmarksd` started and the log has no crash line for them.

## 0047-systemos-authenticated.patch and 0048-feature-flags-off.patch (crashes, 2026-11-12)

Two loops of the boots of 2026-11-11/12 (`c2`, `diag`, `combo1` logs).

- **0047.** `SafariConfigurationSubscriber.xpc` (SafariFoundation, on the SystemOS volume) was killed at each launch:
  `AMFI: Launch Constraint Violation (enforcing), error info: c[4]p[1]m[1]e[0], (Constraint not matched)`. Its code
  signature carries a self launch constraint of its own (`ipsw macho info --sig`: `on-system-volume` or
  `on-authorized-authapfs-volume`, and `validation-category` 1; parent `is-init-proc`). The SystemOS volume is the upper
  layer of the root union, not the root mount (`MNT_ROOTFS` stays with os.img), and APFS cannot authorize it without a
  personalized manifest. `native_owner_cache_prepare` (kern_exec.c) checks that mount (read-only, APFS, on the device
  the image set names, which xnuhost verifies chunk by chunk) and now names it to `oneios_systemos_verified`;
  `oneios_app_cryptex_authenticated` answers `FSIOC_AUTH_FS` for it as for the App volume. First answer:
  `OneiOS: AMFI launch constraints: SystemOS volume answered as authenticated (FSIOC_AUTH_FS)`. Programs of the root
  volume are not affected (they meet `on-system-volume`), nor any other volume. Run on the phone: see `c3` below.
- **0048.** locationd crashed 17 times in one session, every time the same way: `pc=0x0`,
  `lr=_dispatch_lane_serial_drain+0x118` (`dsc.py --slide 0x124d8000`: `_dispatch_lane_invoke`,
  `_dispatch_root_queue_drain_deferred_wlh`, `_dispatch_workloop_worker_thread`), a serial queue bound to a workloop.
  At +0x118 libdispatch calls the item's `dx_invoke` (class word + 0x48). The item (`x25`/`x0`) lay at the same depth
  of another worker thread's stack in every crash, and its class word (`x28`) was that thread's TSD base (a saved
  `tpidrro_el0`), whose slot 9 (the semaphore cache) was 0, or once the port name `0x2c03` (`pc=0x2c03`). So the queue
  drained an item from a stack frame that had already returned: the shape of a `dispatch_sync` waiter that left early
  (inferred; the waiter was not caught). vphone-cli turns `CoreLocation/CLAutoCohort` off for iOS 27 guests
  (`patch-locationd-cohorting`, "Disable automatic CoreLocation cohort queues"), droidvm saw locationd's crash on a
  `Cohort:0` queue once a boot, and vphone's notes have 27.0's locationd failing with `EXC_ARM_PAC_FAIL` (the same item
  with PAC on). The flag is `FeatureComplete` in the root's `/System/Library/FeatureFlags/Domain/CoreLocation.plist`.
  The patch writes the override `/private/var/preferences/FeatureFlags/Domain/CoreLocation.plist` =
  `{CLAutoCohort = {Enabled = false}}` before launchd (that is `/Library/Preferences/FeatureFlags`, the override path
  libsystem_featureflags reads after the root's); the roottweak writes the same file into new seeds (docs/userland.md).
  `oneios-feature-off=DOMAIN/FLAG[,...]` (default `CoreLocation/CLAutoCohort`, `-` none). A file that names the flag
  is kept, another flag's file gets the entry, a binary file is left alone. Console:
  `oneios-features: CoreLocation/CLAutoCohort: off (written)`, later boots `... named there already: kept`. With the
  flag off CoreLocation logs `#Cohorting Process intended to sign up for auto cohorting but CoreLocation/CLAutoCohort=off`
  (user-space log, not on the console). What iOS loses: locationd's silos keep their own queues instead of shared
  cohort queues (an optimization of iOS 27; no feature depends on it as far as read). Run on the phone: see `c3`
  below.
- **0053** and the daemon policy's group `loops`: the speech audio units (`SiriAUSP`, `MacinTalkAUSP`, `MauiAUSP`,
  `KonaSynthesizer`, ExtensionKit extensions on the root volume, extension point `com.apple.AudioUnit-Speech`). In the
  14:12 session (`c3`, `sheng-exc-name` with their names) every one of 17 dumps is `exception type=0xa
  code=0x9000000`: EXC_CRASH for SIGKILL, from outside. They were started four at a time (four `xpcproxy` lines) and
  killed together 3-4 s later, wherever they were: most still in dyld (`mov x16, #0x126; svc` =
  `shared_region_check_np`, or dyld's fixups), one in libsystem. `oneios-sysfail` showed no failing file call of
  theirs. Bursts every 8-10 s for about 50 s. They are not refused by launch constraints (no AMFI violation, no
  constraint in their signatures, root volume). The four corpses at a time fill the kernel's corpse limit, so a crash
  in that window gets no report. iOS's own log of the sessions (`ios3-log.csv`) names both ends [F]: runningboardd
  launched all 84 of them for `osservice<com.apple.accessibility.axassetsd>:33` (axassetsd's TextToSpeech voice list:
  `Sending prepareUsing to managed extension`), and launchd killed each: `Extension is hanging on launch. Killing.`,
  `exited with exit reason (namespace: 7 code: 0x3) - XPC_EXIT_REASON_EXTENSION_CHECK_IN_TIMEOUT`: launchd's
  extension watchdog (`ExtensionWatchdog` in its built-in config) fires before a cold start under the hosted dyld has
  checked in. axassetsd then logs `Unable to acquire process assertion in beginUsing` and tries again. Settings'
  `Process[169] crashed: Preferences` is not theirs: the owner closed it from the app switcher, it did not exit within
  5 s, and SpringBoard ended it with `Watchdog Violation (0x8BADF00D)`; the corpse limit only took its report.
  The policy disables `com.apple.accessibility.axassetsd` (`daemons.toml`
  group `loops`); 0053 writes the same into a Data store made from an older seed: `oneios-disable-jobs=LABEL[,...]`
  (default `com.apple.accessibility.axassetsd`, `-` none), `<false/>` becomes `<true/>`, a missing entry is added,
  a missing file is written. Console: `oneios-jobs: com.apple.accessibility.axassetsd: disabled in disabled.plist
  (it was not)`, later boots `... disabled there already`. Cost: Accessibility's downloadable assets (more voices,
  Sound Recognition, Magnifier, Back Tap and image-caption models); the built-in voices stay. Settings' Spoken
  Content may still start the units when it lists voices. Built (HOSTED_OUT=crashes); the plist logic run on the Mac.
- **0053's default list, 2026-11-22**: `com.apple.accessibility.axassetsd,com.apple.aonsensed,com.apple.prngseedd`
  (the daemon policy's group `loops` has the same three; `docs/userland.md` 11). aonsensed: no AON sensing here (wifid
  and locationd log `AONSense.isAvailable,false`); it exits 0 and launchd starts it again as the publisher of
  `com.apple.proximity.launch`, every 10-20 s (21 starts in the session log of 2026-11-22 from 7790 s). prngseedd:
  `seputil --daemonize-reseed` (no SEP) is started again by its xpc activity about every 10 s (16 starts). Only the
  default string and the comment changed, line for line (the hunks' counts are the same; the changed hunk applies with
  no fuzz to a tree with the series, checked on the Mac). Not built yet: the phone's Data store gets the two entries
  at the first boot of a kernel built with it (console: `oneios-jobs: com.apple.aonsensed: disabled in disabled.plist
  (it was not)`).
- Checked in `c3` (kernel.kc 29bef797, with 0047 and 0048): `oneios-features: CoreLocation/CLAutoCohort: off
  (written)`; locationd ran as pid 59 for the whole session (570 s) with no exception; `SystemOS volume answered as
  authenticated` and SafariConfigurationSubscriber started.

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
| `oneios-tool kernel hosted-fixup` (was `kernel/hosted-fixup.py`; run by `build-hosted.sh`, then the seal again) | `mrs xN, MPIDR_EL1` -> `ldr wN, [x18]` (the vCPU index) in the linked kernel: 7 sites, XNU's and ArmCpu's. At EL0 Linux answers MPIDR_EL1 itself with one value for every core, and ArmCpu parks a secondary whose MPIDR is not its affinity. Done after the link because the text is under corecrypto's HMAC (a rewrite at boot fails the POST with -2074). **fable-prelink:** kexts of the collection that read MPIDR_EL1 need the same rewrite. |
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
| `keystore/OneiOSKeyStore.cpp`, `oneios-tool kernel gen keystore` | the archive `liboneioskeystore.a` (the component's service and user client, the start, the class keys, wrap and unwrap), added to the link by `kernel/build-hosted.sh` |
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

No numbered patch: an archive (`oneios-tool kernel gen hid`, hooked into `kernel/build-hosted.sh`) whose start,
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


## audio/: the speaker and the microphone (audio, 2026-11-12)

No numbered patch: an archive (`oneios-tool kernel gen audio`, hooked into `kernel/build-hosted.sh` like `hid/`)
whose start, `oneios_audio_start` (`audio/OneiOSAudio.c`), is the last call of `oneios_drivers_start` (weak in
`drivers/bridge/OneiOSBridge.c`). It holds UniXNU-droidvm's virtio-snd transport (`DroidVMVirtioSnd.c`, unchanged,
`-DDROIDVM_DEVICE_DYNAMIC=1`) and its two IOAudio2Devices (`DroidVMAudio.cpp`, staged with the boot arguments
`oneios-audio-out-uid`, `oneios-audio-in-uid`, `oneios-audio-latency`; its static initializer out of
`__mod_init_func`, as the HID archive's: `droidvm_audio_attach` runs it in its own module load). Before it the
hosted kernel had no audio device. Log lines, boot arguments and what was measured: `docs/hardware.md` section 6.
On the phone since `kernel.kc` `66faa3e8` (main 924a0d9, agent prelink's sessions of 2026-11-12): `ONEIOS AUDIO: on`,
both devices published, audiomxd opens them and plays (`docs/hardware.md` section 6, "In a session").

Since 2026-11-15 (fix-audio-latency) the staged source also gives the speaker a smaller ring (`oneios-audio-ring`,
default 1920 frames: iOS's I/O buffer is at most 0.375 of it, 720 frames, instead of 3072 of the 8192 ring), hands
its periods on as they start (output safety offset 240), publishes `oneios-audio-latency` (xnuhost passes the
helper's measured latency; default 720 frames, was 6480), fixes the ring's wrap for every device (8192 is not a
whole number of 240-frame periods: a period read, zeroed or written up to 208 frames past the ring's buffer; the
zero time stamp only every 245760 frames) and reports the speaker engine's timing (`ONEIOS AUDIO: speaker engine:`).
`drivers/audio/README.md`, `docs/hardware.md` section 6 "Latency end to end".

Since 2026-11-24 (fix-audio-late) every engine thread (speaker, mic, Actuator) is a time-constraint thread of the
guest's scheduler (period and constraint 5 ms, computation 2.5 ms) and waits for its period with
`TIMEOUT_URGENCY_SYS_CRITICAL` (no timer slop), behind the boot-arg `oneios-audio-rt` (default 1; 0: kernel priority
81 and `clock_delay_until`, as before). Before, a kernel thread at 81 waited behind the VM's pageout and compressor
threads (91) and the high thread calls (93), and `clock_delay_until`'s timer had the kernel tier's slop (up to 1 ms,
more at a user idle level): on the phone (2026-11-24 01:17, kernel e79886d9) the speaker took its periods 1.4-1.6 ms
late on average and up to 98 ms late while `VM_pageout_scan` used 14.7 s of CPU in those 30 s. The late counts are
also split: the counter line's `late` no longer counts the Actuator's periods (its `periods` never did), and a new
line `ONEIOS AUDIO: late periods: ...` counts the speaker's late periods by size, those the HAL had written only in
part, the Actuator's, and the realtime policy's result. Built: `xnu.macho` `6d40f8e0`, `kernel.kc` `853068df`
(`HOSTED_OUT=audiolate`, audit 0 failures, stubs 666 good); not booted. `docs/hardware.md` section 6, "Late periods".

Since 2026-11-25 (fix-audio-curve) the speaker's volume control reports -36..0 dB over its 128 steps and the engine
applies UniXNU-sheng's curve (c8f92b2 `hal/ShengAudioPlugIn.c`, BSD-2-Clause-Patent), -36 x (1 - step/128)^2 dB, to the
step iOS sets (a third of iOS's volume about -16 dB, half -9 dB), behind the boot-arg `oneios-audio-curve` (default 1;
0: linear over -64..0 dB, as before, where half of iOS's volume was about -32 dB and the phone of 2026-11-24 played at
`-29.0 dB` at step 70). `OneiOSAudio.c`'s report has a `speaker volume N of 128 (-X.Y dB applied ...)` line after a
change. Built with `HOSTED_OUT=audiovol` (docs/switches.md "Tests"); not booted. `docs/hardware.md` section 6, "The
volume curve".

### The Actuator device (media, 2026-11-12)

The archive also stages a third IOAudio2Device into `DroidVMAudio.cpp`: UID `Actuator` (VirtualAudio's
`VirtualAudioDevice_Actuator`: Core Haptics, UI and keyboard taps), mono S16 at 48 kHz, the speaker's engine, whose
periods go to `drivers/haptics` (`oneios_haptic_actuator_play`, the host's actuator) instead of virtio-snd. Published
only when `drivers/haptics` has the host's actuator (`oneios-actuator=0`: never; `oneios-actuator-channels=2`:
stereo). `drivers/haptics` publishes `/product/haptics closed-loop` only when the device exists. Built
(`HOSTED_OUT=media`); not run. `docs/hardware.md` section 5 has the design, the log lines and the way back.

## codec/: the video encoder (hevc-encoder, hevc-guest, 2026-11-15)

No numbered patch: an archive (`oneios-tool kernel gen codec`, hooked into `kernel/build-hosted.sh` after `audio/`)
whose start, `oneios_codec_start` (`codec/OneiOSCodec.c`), is the last call of `oneios_drivers_start` (weak in
`drivers/bridge/OneiOSBridge.c`). It holds UniXNU-droidvm's virtio-media transport (`DroidVMVirtioMedia.c`,
`-DDROIDVM_DEVICE_DYNAMIC=1`; since 2026-11-25 staged with edit 7, its event queue on the device's interrupt with a
guard, `oneios-codec-irq=0` the poll: `codec/README.md` "The transport on the interrupt"), its `AppleVideoToolboxParavirtualizationDriver` and user client
(`DroidVMVTParavirt.cpp`, unchanged; its static initializer out of `__mod_init_func`, as the audio archive's:
`droidvm_vt_paravirt_attach` runs it in its own module load), and the sessions behind it (`DroidVMVTHost.cpp`, staged
with five edits, `codec/README.md`: the hvcC's profile, tier and level bytes taken from the SPS without its emulation
prevention bytes; iOS 27's tile session; small HEVC sessions coded at 96x96 and cropped back; `jcap`; and since
2026-11-23 10-bit pictures, `x420`/`xf20` surfaces sent to xnuhost's device as P010 for HEVC Main10).
With xnuhost's `encoder=auto` (`/arm-io/virtio-media-0`, `droidvm-vt-paravirt=1`) the kernel publishes iOS's own
paravirtualized VideoToolbox codecs, so CMPhoto finds a hardware HEVC encoder and writes HEIC; without it (the
default) the classes are registered and no service is published, and iOS is as before. Why this route, the boot
arguments, the log lines (`ONEIOS CODEC: ...`), the limits and what was built and booted: `codec/README.md`;
`docs/hardware.md` section 7g. Built and booted on the Linux PC (`HOSTED_OUT=hevc`); on main with 0081 and 0082
built and prelinked on the Mac on 2026-11-22 (`HOSTED_OUT=codec`, audit 0 failures, stubs 666 good); on the phone on
2026-11-22 and 2026-11-23 (no tile coded yet: `codec/README.md`, "The tile session on the phone"); the 10-bit edit is
built and host-tested, not yet booted.

## 0052-rt-computation-held.patch (media, 2026-11-12)

`thread_policy_set(THREAD_TIME_CONSTRAINT_POLICY)` (osfmk/kern/thread_policy.c) refuses a computation above
`max_rt_quantum`, 50 ms in the guest's ticks. The guest's counter is the phone's (19.2 MHz): 50 ms is 960000 ticks.
iOS's audio server asks 1200000 ticks (50 ms at Apple's 24 MHz) for the HAL's I/O thread when its buffer is about
60 ms or more (`configure_thread_for_realtime - thread_policy_set failed. Cycle Size: 1536000 Thread Parameters:
1536000 1200000 1536000 1` in every boot's iOS log); the I/O thread then stopped after 0 frames, and each system
sound waited out the mixer's 3 s timeout. With the patch a computation above the maximum is held to it when the
constraint is at least the maximum, as the code below already raises a computation under half the constraint. After
macOS-DroidVM's xnu-12377 patch 0039 (the same board clock). The guest's scheduler only: xnuhost's vCPUs are normal
Linux threads, and no host thread changes its policy. Built (`HOSTED_OUT=media`); not run. `docs/hardware.md`
section 6 ("Very slow" playback).

## 0056-camera-plugin-mount.patch (camera, 2026-11-12)

- **Why.** CMCapture 27 loads the camera's FigCaptureDevice maker itself: when its probe (`fcc_probeCameraDriverService`)
  finds a service of class `AppleH9CamIn` it `dlopen()`s `/System/Library/MediaCapture/H9ISP.mediacapture` and calls
  `H9ISPCaptureDeviceCreate` (read in the 24A446 cache; `userland/camera/README.md`). The drivers archive publishes
  `AppleH9CamIn` (`drivers/camera/OneiOSCameraService.cpp`). The folder is empty on the sealed OS volume, so this
  patch shows the Data seed's `/private/var/oneios/addons/MediaCapture` over it: a read-only bindfs union made with
  UniXNU's `native_gpu_addon_mount` (scratch offset 0xa00), called in `native_gpu_addon_prepare` before 0036's call.
- **Log.** `oneios-camera-addon: /private/var/oneios/addons/MediaCapture over /System/Library/MediaCapture: stage N,
  status E`. Stage 5 status 0: mounted, `H9ISP.mediacapture` visible. Stage 1 status 2: this Data store has no
  plug-in (an older seed): nothing is mounted, and CMCapture logs that it could not load the dylib.
- **Off.** `oneios-camera-plugin=0`: no mount (and no AppleH9CamIn). `sheng-gpu-addon=0` skips it too (that function
  returns early).
- **State.** Built on the Mac (`HOSTED_OUT=cam`, on the series through 0051: before 0052 landed); not run.

## 0057-swap-folder.patch (mem, 2026-11-12)

- **Why.** XNU's compressor swaps to `/private/var/vm/swapfileN` (`SWAP_FILE_NAME` in
  `osfmk/vm/vm_compressor_backing_store_internal.h`, opened `O_CREAT | O_TRUNC | O_NOFOLLOW_ANY` by
  `vm_swapfile_open`, `bsd/vm/vm_compressor_backing_file.c`). Nothing in the kernel makes the folder: iOS's own Data
  volume has it, the image set's seeds did not. In the 15:35 session (phone, 2026-11-12) every swap file failed
  ("Failed to open swap file", errno 2 and 30) while `VM_pageout_scan` took about 95% of a vCPU. Those two are early
  tries before the Data mount (below, "errno 2 and 30"); after a failure the creation thread tries again, 15 s or more
  later, when a swap-out asks for room (`vm_swapfile_creation_delay_ns`), so swap can start once the folder is there.
- **What.** (1) `oneios_swap_folder` in `kern_exec.c`, called last in the pre-launchd list (after 0053): makes
  `/private/var/vm` root:wheel 0755 with 0004's `oneios_kern_mkdir`, or gives a folder there that owner and mode;
  anything else there (a file, a link) is left alone and said. `oneios-swap-folder=0`: nothing done. The roottweak's
  `data_layout` puts the folder in new seeds too. (2) `oneios_swapfile_failed` in `vm_compressor_backing_file.c`,
  after XNU's own line: for each errno the first time, then every 64th failure, the path, the seconds since boot (by
  the calendar), and
  the deepest folder of the path that is there with its volume and whether that is read-only.
- **Log.** `oneios-swap: /private/var/vm: made, root:wheel 0755 (0), on /private/var (apfs, read-write)` (later boots:
  `there already`). If a swap file still fails: `oneios-swap: /private/var/vm/swapfile0: errno N at S s (failure K);
  DIR is there, on MOUNT (TYPE, read-only|read-write)`. Then XNU's `VM Swap Subsystem is ON` stays as before, and swap
  in use shows as files in `/private/var/vm` and as `vm.swapusage` (`sysctl vm.swapusage` in iOS).
- **errno 2 and 30, settled on the PC (2026-11-12, pc-kernel).** Two boots of the Linux-built kernel under qemu
  (`--ram auto` = 14,080 MiB, and `--ram 2G`; logs in `out/run-pc-auto/`, git-ignored) show both errors before the Data
  volume is mounted, and neither is a swap file on the Data volume:
  - errno 2, 4 s in, on `/private/var/vm/swapfile` (no number): the freezer's budget probe (`vm_swap_max_budget`,
    `CONFIG_FREEZE`, before any swap file exists), before the root is mounted (`rootvnode is not set`; the line says
    `nothing is there, on ?`). Its failure only keeps the freezer's default daily budget.
  - errno 30, 7 s in, on `/private/var/vm/swapfile0`, right after `VM Swap Subsystem is ON` and followed by XNU's
    `low swap: failed to create swapfile`: the creation thread's first swap file, made as soon as swap starts. The line
    says `/private/var/vm is there, on / (apfs, read-only)`: the sealed root's own `/private/var/vm`, under the Data
    mount. The thread then sleeps until a swap-out wakes it (`vm_swapfile_create_needed`, from the swap-out paths of
    `vm_compressor_backing_store.c`) and tries again 15 s or more after the failure.
  - Then 0057's line, 11 s in (365 s on a first boot under qemu, which waits for the Data store):
    `oneios-swap: /private/var/vm: there already (0), on /private/var (apfs, read-write)` (the roottweak's seed has the
    folder, root:wheel 0755, read back from the Data volume after the run).
  - Not seen on the PC: a swap file. In 20 minutes at 2 GiB (to SpringBoard and the daemons, no panic) nothing asked for
    a swap-out, so the thread never tried again and `/private/var/vm` stayed empty. Whether swap starts under the phone's
    pressure is for the phone's log: a `Failed to open swap file` after the Data mount (and its `oneios-swap:` line), or
    `swapfile0` in `/private/var/vm`. **Swap serves only the freezer on iOS** (read in xnu-12377, 2026-11-12): with
    `CONFIG_FREEZE` the default `vm_compressor_mode` is `VM_PAGER_FREEZER_WITH_SWAP` (0x20; the PC's log: `"vm_compressor_mode"
    is 32`), which sets `freezer_swap_is_active` and not `swap_is_active` (`vm_config_init`, `vm_pageout.c`); a
    general compressed segment goes on the swap-out queue only with `VM_CONFIG_SWAP_IS_ACTIVE` (`C_SWAPOUT_REG`,
    `vm_compressor.c`), so only frozen apps' segments (`C_SWAPOUT_FREEZER`), donated early ones (`C_SWAPOUT_DONATE`) and,
    with `vm_compressor_scavenger_enabled=1` (off by default), old ones (`C_SWAPOUT_RIPE`) are swapped. Swap does not
    relieve the pageout scan of the 15:35 session; [I] it helps only when iOS's freezer freezes apps, which user space
    asks for. `/defaults kern.vm_compressor` in the device tree would set another mode (the phone's log line says).
- **The time in the failure line** is seconds since boot from the calendar (`clock_get_calendar_microtime` less
  `clock_get_boottime_microtime`): the hosted `mach_absolute_time` is the host's counter (wall time under qemu, the
  phone's own uptime on the phone); the first build printed `at 1794488403 s`.
- **State.** Written on the Linux PC (2026-11-12): every one of the 23 patches of the build's series that touch
  `kern_exec.c` (arch, OS, device, hosted through 0056) and then this one apply with `git apply` to xnu-12377.121.6's
  file, and the swap file hunk to its `vm_compressor_backing_file.c`. Compiled on the Linux PC once libdispatch was
  installed (`HOSTED_OUT=pc` at `7652df1`; with the time fix `HOSTED_OUT=pc2`, `kernel.kc` `cdd98e7e`; no warnings from
  it) and run on the PC as above. Not run on the phone.

## 0058-free-page-report.patch (mem, 2026-11-12)

**The goal: iOS keeps all of its ram=auto memory; the host gets back only pages iOS has freed.** (The owner's
decision, 2026-11-12: no pressure feedback, no cache cap, no change to XNU's free targets, memorystatus or jetsam.)

- **Why.** xnuhost's RAM is a memfd: the host holds a page from its first touch until the guest gives it back
  (`ram_discard`, a hole punched in the memfd), and before this patch the kernel never gave anything back. Stock arm64
  XNU enters a freed page at the tail of its free queue and allocates from the head (FIFO), so even a small working set
  walks through every never-touched page: at `ram=auto` on the PC (14,080 MiB) the host held 8,567 MiB after 7 min
  while XNU itself had 11,310 MiB free, and 12,266 MiB after 16.5 min (9,048 MiB free in XNU), when the PC's
  MemAvailable fell to 903 MiB (the run below). What fills the memfd is not the file cache: it is free pages that the
  boot's reads and allocations went through. On the phone (9.5 GiB guest of about 11.2 GiB) that memory would never
  go back to One UI.
- **What.** (`osfmk/vm/vm_resident.c`, `vm_page.h`, `vm_pageout.c`, `arm64/hosted/hosted_pmap.h`,
  `arm64/hosted/free_report.h`; all `#if XNU_HOSTED`.)
  - Only pages on XNU's own free queue are given back: memory iOS is not using at all. Cached, compressed and wired
    pages are never touched, and nothing changes `vm_page_free_count` (but for the moment a batch is out, below), the
    free target or what memorystatus counts.
  - The free queues keep two kinds of page in order: `[pages iOS freed: the host holds them][pages given back]`. A
    freed page enters at the head (taken first: memory the host still holds), a given-back one at the tail (taken
    last). Pages released at startup were never touched and enter as given back. `vm_page`'s spare bit
    (`__vmp_reserved3`) is `vmp_hosted_reported`; per colour queue, a count of held pages and the first given-back
    page, all under the free queue lock (`vm_page_free_queue_enter/remove/grab`).
  - A thread (`oneios_free_report`, utility priority) wakes every second. When the held free pages pass keep +
    hysteresis it gives back down to keep, coldest first, in batches: up to 64 pages taken off the queue with its lock,
    given back as runs of consecutive pages with the lock dropped and interrupts masked for each host call, put back
    at the tail with the lock (waiters woken, memorystatus updated). keep = `vm_page_free_target` +
    `oneios-free-report-keep` MiB (32): the pages the next allocations take; under pressure XNU keeps its free count
    between its free minimum and target, so pages that cycle through the free queue are never given back. While a
    batch is out (at most 256 KiB with 4 KiB pages, for the host calls' time, only far above the free target) the free
    count is that much lower. Nothing can allocate a page whose hole is being punched; the contiguous allocator does not
    take a page that is on no queue (`vm_page_is_relocatable`: `VM_PAGE_NOT_ON_Q`).
  - The decisions (`xh_fr_batch`: the hysteresis and the batch; `xh_fr_runs`: sorted runs; `xh_fr_keep_per_queue`)
    are `osfmk/arm64/hosted/free_report.h`, the same file as `runtime/xnuhost/include/xnuhost/free_report.h`; the host
    tests check that the two are one and test the functions.
  - Deferred unmaps (`docs/booter.md` 2.6): XNU's pmap removes every mapping of a page before it reaches the free
    queue; xnuhost may still have a stopped task's unmap queued. The punch drops the page from that task's process too,
    and the task cannot run before the stub has carried the queue out (test kernel: "ram_discard of a page whose unmap
    is queued"). With `hosted_mapcheck=1` each page given back is checked: no task mapping recorded for any 4 KiB part
    and `pmap_verify_free`; a failure prints `hosted check: page 0x... is given back to the host (0058) while N task
    mapping(s) of it remain`.
- **Boot arguments.** `oneios-free-report=0` (off: stock FIFO order, nothing given back; the `oneios-mem:` lines stay),
  `oneios-free-report-keep=MiB` (32), `oneios-free-report-hysteresis=MiB` (16), `oneios-free-report-batch=PAGES` (64,
  the most), `oneios-free-report-ms=MS` (1000), `oneios-free-report-log=SECONDS` (30; 0: no lines). A check, not a setting:
  `oneios-free-report-stress=1` makes keep 0, so every held free page above the hysteresis goes back and pages come
  back used at once (with `hosted_mapcheck=1`: the races of the paths above).
- **Log.** At the pageout daemon's start: `oneios-mem: free-page reporting on (oneios-free-report=1): keep 16192 pages
  (free target 8000 + 32 MiB), hysteresis 16 MiB, batch 64 pages, every 1000 ms; 3506783 of 3506783 free pages never
  touched`. Every 30 s, three lines (the console cuts a line past about 256 characters):
  `oneios-mem: free F MiB (G given back, H held), active, inactive, speculative, throttled, wired, compressor C MiB
  holding P MiB`, `oneios-mem: purgeable, file-backed, anonymous, secluded MiB; memorystatus available M MiB; RAM T
  MiB`, `oneios-mem: given back X MiB since boot in B batches, R runs, N rounds (0 failed, 0 out of order), D ms`.
  xnuhost's `memory:` line adds what it was given (`given back N MiB of free pages in C calls (F failed, T ms)`).
- **Measured on the PC (2026-11-12, qemu-aarch64, 8 vCPUs, `--ram auto` = 14,080 MiB, the first boot of a fresh Data
  store, `HOSTED_OUT=fpr`, `out/kernel/fpr-24A446/kernel.kc`: `216b73ba` for the control, `c8d36270` with it on; they
  differ from the committed patch only in how the `oneios-mem:` lines are cut and in the stress check).** The same kernel with `oneios-free-report=0` (control)
  and with the default (on), 18 min each; the control was stopped at 1,003 s by the run's watchdog (the PC's
  MemAvailable under 1,000 MiB). XNU's view is its `oneios-mem:` lines (MiB):

  | | control, 420 s | on, 420 s | control, 990 s | on, 990 s |
  |---|---|---|---|---|
  | memfd held by the host | 8,567 | 3,160 | 12,266 | 4,973 |
  | host MemAvailable | 4,588 | 8,903 | 1,084 | 3,769 |
  | XNU free | 11,310 | 11,032 | 9,048 | 9,084 |
  | XNU active / inactive / speculative / wired | 78 / 78 / 2,099 / 481 | 137 / 85 / 2,306 / 485 | 942 / 165 / 3,259 / 629 | 929 / 157 / 3,258 / 616 |
  | XNU RAM, compressor | 13,761, 0 | 13,761, 0 | 13,761, 0 | 13,761, 0 |

  iOS's usable memory is the same (its RAM 13,761 MiB, free plus cache plus in use within the run-to-run spread;
  memorystatus available 13,088 MiB at 990 s with it on), and the host holds about what XNU uses (active + inactive +
  speculative + wired = 4,960 MiB at 990 s). Stages: launchd 306 s / 292 s, SpringBoard 361 s / 333 s (control / on),
  no panic in either. With it on nothing reached keep + hysteresis, so 0 MiB was given back: the head-first order
  alone keeps the memfd at the working set; giving back is for bulk frees (an app's exit, a purge). A third boot
  (4 GiB, `hosted_mapcheck=1 oneios-free-report-stress=1 oneios-free-report-hysteresis=1`, the final patch,
  `out/kernel/fpr2-24A446/kernel.kc` `ff87a7ee`) gives back pages all the time: launchd 267 s, SpringBoard 310 s and the daemons (analyticsd 716 s) in 840 s, no panic, no `hosted check:` line, and
  893 MiB given back in 3,650 batches (215,240 runs; 0 failed, 0 out of order), also under pressure (free 28 MiB at
  810 s: by design this check gives back pages that cycle, 35 s of the thread's time in 840 s; the default keep
  never does). A first `hosted_mapcheck=1` boot with the default keep (4 GiB): launchd 299 s, SpringBoard 366 s, no
  panic, no `hosted check:` line, nothing given back.
- **The cost to iOS** (test kernel, `make -C runtime/xnuhost bench-arm64` (the C runtime of that date; today `runtime/xnuhost/check-arm64.sh bench`), under qemu on the PC; times about 10x the
  phone's): `ram_discard` of one 16 KiB page 5.2 us, of 1 MiB in one run 90 us (1.4 us a page); zero-filling a 16 KiB
  page that was given back 11.7 us against 6.0 us resident (the host's first touch of each 4 KiB part): +5.7 us per
  16 KiB, about 0.36 ms per MiB used again, paid only for pages given back and reused (never for pages cycling
  through keep). With the order change the boots were faster, not slower (above). So the default is on.
- **Not run on the phone.** Risks: a host with shmem huge pages for the memfd (`shmem_enabled` `always`/`within_size`,
  or a per-size mTHP setting) may split or merely zero a punched range instead of freeing it (this PC: `advise`; the
  phone's: unknown, read `/sys/kernel/mm/transparent_hugepage/shmem_enabled`); a page XNU frees while the GPU's
  renderer still holds its udmabuf stays held by Linux (the import pins it) and the guest's reuse gets a new page (a
  guest bug either way).

## 0060-ls-boot-state.patch (fix-boot-slow, 2026-11-13)

The phone's boot of 2026-11-12 23:45 (kernel.kc through 0058, a Data store kept from earlier boots) built the lock
screen and the Home Screen piece by piece: the clock, the wallpaper, the icons, then the labels one by one. In the
first two minutes `oneios-cpu` had lsd at 90-92 % (+40 s, +58 s) and iconservicesagent at 115 % (+108 s).

**The two stores, read in 24A446** (CoreServices in the dyld cache; `/System/Library/CoreServices/iconservicesagent`):

- lsd keeps its database in its data container's `Library/Caches/com.apple.LaunchServices-33554481-v2.csstore`
  (beside it `SystemDataOnly-...csstore`, the system content's copy). `_LSDatabaseCreateFromPersistentStore` first
  looks for the recovery file `com.apple.LaunchServices.error`: when it is there lsd logs `_LSCreateStoreWithFileContents
  found a recovery file, so creating a new empty store` and registers everything again. When it is not, lsd makes it,
  loads the database, runs `_LSDatabaseClean` on it ("Cleaning new database object": it registers the CoreTypes bundle,
  the extension points, the canonical names and the frameworks' localized strings again, at every start, on a loaded
  database as on a new one) and only then removes the file. A database is also rebuilt when its header says the
  seeding did not complete (`Database seeding is incomplete, need to rebuild`; set when MobileInstallation answers with
  partial results) or names another build (`Local database thinks system build version is ...`). LaunchServices removes
  the recovery file itself when its sync marker `com.apple.LaunchServices.syncInterrupted` says a reboot cut the last
  save (`Previous sync was interrupted, presumably by a reboot. Removing any extant recovery file.`);
  `com.apple.LaunchServices.dirty` marks unsaved changes and goes at each save.
- iconservicesagent keeps the icons it drew in `systemgroup.com.apple.lsd.iconscache/Library/Caches/
  com.apple.IconsCache/<UUID>/` (`*.isdata`, `store.index`, `store-source-registry.map`, `__cache_info__`). At start
  (`-[ISMutableIconCache initWithURL:]`) it empties the folder unless `__cache_info__` holds this build's
  `ProductBuildVersion`, the folder is owned by `_iconservices` (uid 276) and has no ACL. Each icon carries a validation
  token, LaunchServices' database UUID and sequence number (`Generated image for icon %@ -- Validation token DB UUID %@
  seq No %llu`); one whose token no longer matches is drawn again (`Rejected cached image ... validation state`).

**What the PC showed** (qemu-aarch64, 8 vCPUs, 4 GiB, the PC image set; one Data store booted again and again, its
copies for the comparisons; boots 1-6 shared the box with two other agents' boots and kernel builds, load 13-24 on 4
CPUs; boot 7 had it alone; boots 8 and 9 (not in the table) were stopped before lsd started, 8 by its time limit at
load 16, 9 to keep to one boot at a time; 10 and 11 ran one at a time through the box's qemu queue, `flock
/tmp/oneios-qemu.lock`):

| Boot | Data store | Kernel | launchd | lsd | first frame | lsd / iconservicesagent CPU-s, 30 min from launchd |
|---|---|---|---|---|---|---|
| 1 | new | fpr2 (through 0058) | +874 s | +2050 s | +2096 s | 168 / 2 |
| 3 | after 1 (no database saved) | fpr2 | +531 s | +1392 s | none in 4067 s | lsd 137 to the end |
| 4 | after 3, its recovery file left | 0060 (first build) | +1088 s | +1272 s | +1613 s | 1298 / 1666 (60 min): lsd built and seeded the database |
| 5, 6 | after 4: database saved, 52 of 52 apps | 0060 / fpr2 | +515 s | +1508 / +1450 s | none in 3600 s | 27 / 0, 47 / 0 (60 min) |
| 7 | the same, alone on the box | 0060 | +229 s | +308 s | +679 s | 75 / 160 |
| 10 | a copy of the store after 6: the database (52 of 52 apps) and boot 6's recovery file | fpr2 | +310 s | +381 s | +1306 s | 57 / 0; containermanagerd 160 |
| 11 | another copy of the same | 0060 (merged tree, `kernel.kc` `22f8c09d`) | +413 s | +966 s | +1449 s | 71 / 277 |

**Before and after, the same store** (boots 10 and 11, one at a time; the box's load differed, so their times do not
compare). Both kernels' 0043 said `names 52 of 52 apps of /Applications: kept`. Boot 10 (no 0060), iOS's own log: lsd
`_LSCreateStoreWithFileContents found a recovery file, so creating a new empty store`, `result: (null)`, then `Seeding
database with UID: 501`: everything registered again, containermanagerd busy, and 45 minutes later the session was
killed with the new database not yet saved (on disk still the old one, with lsd's new recovery file beside it: the next
boot does it all again). Boot 11 (0060): `oneios-ls-state: ... com.apple.LaunchServices.error 0 bytes, modified 11065 s
ago: removed (0), lsd loads the database`; lsd made its own recovery file, `result: <private> options: 7` (the database
loaded), no seeding, removed the file, saved the database again; iconservicesagent served the 72 stored icons (2
`.isdata` rewritten, 70 untouched).

- **The database is kept and loaded** (boot 7's own log): `oneios-ls: ... names 52 of 52 apps of /Applications: kept`;
  lsd found no recovery file, loaded it (`result: <private> options: 7`), no `Seeding`, no rebuild. Before it removed
  its recovery file it ran `_LSDatabaseClean`: `Registering CoreTypes bundle` (`Error -50 registering path
  /System/Library/CoreServices/MobileCoreTypes.bundle`), 42 `Unable to create bundleRef`, `Registering extension
  points` (`Failed to create node for framework (null)`), canonical names, localized strings: 26.8 s of iOS's clock,
  about 5 min of real time on this PC (the guest's clock ran about 12 times slow here, below). On the phone, 10 or more
  times faster, that is about the first minute of lsd at 90 %: **the phone's lsd minute is most likely this per-start
  work, not a rebuild** (a guess for the phone: the next boot's log says).
- **A kill during that window throws the database away.** Under load (boots 5 and 6) lsd was still in it 35 minutes
  later, its recovery file in place, when the session was killed; boot 3 ended the same way. Every session here ends
  with the kernel killed (a return, a panic, xnuhost's exit), never with lsd's own shutdown, so the next lsd finds the
  file and starts empty: a full registration (boot 4's `Noting rebuild statistics`: 346 apps, 928 plug-ins,
  `systemContentScanTime` 1439 s, `totalRebuildTime` 1454.5 s) and a new database, whose new UUID makes every drawn
  icon's token stale. On the phone that is a return or a panic within lsd's first minute.
- **The icon store is kept and valid across boots** on the PC: `__cache_info__` is `{ProductBuildVersion: 24A446,
  language: en}`, the `<UUID>` folder is `_iconservices`' with no ACL (its parents carry the container ACL, which is not
  looked at). Boot 4 drew 72 icons on the CPU (no 3D GPU on the PC); boot 7 found them (`72 icons (.isdata)`) and wrote
  no new `.isdata`; its iconservicesagent (160 CPU-s) served them. So the phone's 115 % at +108 s is not the store being
  thrown away when the database was kept; a guess: the Metal path's own cost at each boot (metal2vulkan's pipeline
  cache, which the plug-in's client path puts in `_CS_DARWIN_USER_CACHE_DIR` or, failing that, the user temp folder that
  launchd's `dirs_cleaner` empties at every boot: `external/NativeMetal/NativeAdrenoMetalPlugin.mm` `clientDevice`).
  Not looked at on the phone.
- 0036's stamp and 0037's semaphore did what they say (`generation 1: kept`; no `Reboot detected, performing first
  boot work`).

**The change.** Before launchd, after 0043's keep or drop and 0036/0037:

- `oneios-ls-state:` one line per `com.apple.LaunchServices*` and `SystemDataOnly-*` entry of every lsd data container
  (the databases, the recovery file, `.syncInterrupted`, `.dirty`, `.InstallJournal`, the app protection store): size
  and age.
- A recovery file there is removed (`oneios-ls-recovery=0` keeps it). For lsd the kill of the whole kernel is a reboot,
  not its own crash, which is the case LaunchServices handles itself by removing the file; lsd makes the file again
  before it loads, so a database that crashes lsd is still thrown away at lsd's next start in the same session. 0043 has
  already checked that the database names every app.
- `oneios-icons-state:` one line per `com.apple.IconsCache` folder: the drawn icons (`.isdata`), `store.index`'s size
  and age, `__cache_info__`'s size (-1: missing, and iconservicesagent then starts the folder over).
- `oneios-store-report=0` turns the step off.

The lines on the PC (this patch before the merge of main, `kernel.kc` `06b06e68`, boot 7; the merged tree's
`22f8c09d` gave the same lines in boot 11):

```
oneios-ls: D1F3ECC9-8A4D-42E8-8A88-F6F96F1A8E6E/com.apple.LaunchServices-33554481-v2.csstore: 11448320 bytes, names 52 of 52 apps of /Applications: kept
oneios-ls-state: lsd D1F3ECC9-...: com.apple.LaunchServices.unregisteredtypes-501.db 12288 bytes, modified 6197 s ago
oneios-ls-state: lsd D1F3ECC9-...: com.apple.LaunchServicesAppProtectionStore.plist 264 bytes, modified 7289 s ago
oneios-ls-state: lsd D1F3ECC9-...: com.apple.LaunchServices-33554481-v2.csstore 11448320 bytes, modified 4614 s ago (the database)
oneios-ls-state: lsd D1F3ECC9-...: SystemDataOnly-com.apple.LaunchServices-33554481-v2.csstore 11200440 bytes, modified 6283 s ago (the system content's database)
oneios-icons-state: 15E09D24-AF82-354E-BECD-43DCC0B36EFC: 72 icons (.isdata) of 75 entries, store.index 720896 bytes (modified 6189 s ago), __cache_info__ 91 bytes
```

and with a recovery file left (boot 4's store; the first build said "lsd loads the database" there):
`oneios-ls-state: lsd D1F3ECC9-...: com.apple.LaunchServices.error 0 bytes, modified 4975 s ago: removed (0) (no
database here: lsd builds one)`.

**Not changed: what it costs** (the hosting's share; PC boot 1, lsd's busiest 30 s, lsd 78-92 %): 154,027 task traps
(5,125/s: 60,108 system calls, 88,431 faults, 5,488 interrupts); the hand-offs (trap to vCPU and back) 56.6 and 65.4 us
at the mean (10 us at p50), about 19 s of the 30 s summed over the vCPUs; on cpu0 and cpu1 45 % of the wall time was
kernel, 55 % an iOS thread's turn. lsd's calls are file system work, by kernel time: `madvise` 14,156 x 145 us,
`open_nocancel` 3,661 x 636 us, `open` 1,697 x 1,264 us, `workq_kernreturn` 3,151 x 586 us, `getdirentries64` 2,912 x
580 us, `getattrlist` 1,619 x 946 us, `mach_msg2` 6,123 x 333 us. These are qemu's times (about ten times the phone's);
the phone's earlier boot had syscall p99 1.3 ms, fault p99 0.4 ms and 40-45 % kernel on cpu0 and cpu1. So the work is
iOS's own (walking bundles, reading Info.plists and localizations, container queries), each step a trap the hosting
makes dearer; doing less of it at each start is worth more than a cheaper trap, and the part that could be cut is
Apple's `_LSDatabaseClean`, which this patch does not touch.

Also seen on the PC, not changed: the guest's wall clock (iOS's log times) ran about 12 times slower than real time
(boot 7: 109 `oneios-cpu` lines, 10 s apart on the console, span 91 s of iOS's log) and jumped between boots (launchd at
20:38 UTC in boot 1, 14:18 in boot 3); rapportd logged 221,791 lines in boot 3's first minutes
(`ServiceDiscovery:DiscoveryCache After pruning N records`), logd at 30-80 %; `oneios-tool`'s own APFS reader refuses
these Data stores (`short read at 1152903912689233920`: it reads block 0's container superblock, an old checkpoint
whose object map is gone) and falls back to apfs-fuse.

## 0061-ubt-count.patch (fix-wifi-late-2, 2026-11-13)

0026's user backtraces (`oneios-ubt=NAME,...`, `oneios-ubt-at=`, `oneios-ubt-every=`) as a sampler of a boot's first
minutes: `oneios-ubt-count=N` stops after N reports (default 0: no end, as before), and each report now starts with
`oneios-ubt: report K at +S s` (seconds after bsd_init's late start, where 0026's thread starts). Why: on the phone
(2026-11-13, main c1223f8) configd named en0 89 s after launchd with IOKit already quiet, and what its threads did in
those 89 s is not in xnuhost's log. For the next boot, in the image's `xnuhost.conf`: `oneios-ubt=configd,wifid,logd
oneios-ubt-at=20 oneios-ubt-every=10 oneios-ubt-count=18` (+20 s to +190 s); give each line's addresses to `oneios-tool
sym` (`docs/hardware.md` 3, "en0 late"). Compiled in the Linux PC's kernel (`HOSTED_OUT=fixwifi`) and prelinked (`kernel.kc` `84d3304c`: audit 0 failures, `stubs-check.py` 666 good). On the PC (2026-11-13, `oneios-ubt=configd oneios-ubt-at=850 oneios-ubt-every=45 oneios-ubt-count=6`): reports 1 to 6 at +850 to +1075 s, then `oneios-ubt: 6 reports, the last (oneios-ubt-count)`; what they showed is in `docs/hardware.md` 3. Not on the phone.

## 0062-iokit-hardware-probe-trace.patch (fixbt, 2026-11-13)

A debugging aid for a daemon that runs and never opens its hardware (bluetoothd on the phone, 2026-11-12 23:45: on,
no transport opened). For the processes of the boot argument `oneios-iokit=NAME[,NAME...]` (default
`bluetoothd,BlueTool`; `oneios-iokit=-` for none) the kernel logs one `oneios-iokit:` line per distinct request:

- IOKit matching (`io_service_get_matching_service(s)`): the matching dictionary on one line and what was found
  (`match {IONameMatch=bluetooth} -> IOPlatformDevice "bluetooth"`, `-> none (0x2c0)`), `match all` for an iterator;
- matching notifications (`io_service_add_notification*`: `notify IOServiceFirstMatch {...}`): a daemon that waits
  for a service shows what it waits for;
- registry paths (`io_registry_entry_from_path`, `_ool`);
- property reads (`io_registry_entry_get_property`, `_bytes`, `_recursively`, `_bin`/`_bin_buf`: what
  `IORegistryEntrySearchCFProperty` uses): name, plane, recursive or not, the entry, found or not;
- user client opens (`io_service_open_extended`): class, type, result;
- kernel controls looked up by name (`CTLIOCGINFO`, `bsd/kern/kern_control.c`): `kernel control
  "com.apple.uart.sk.bluetooth" -> id N` or `-> not registered (errno 2)`;
- `kern.hv_vmm_present` reads (`bsd/kern/hvg_sysctl.c`, after Droid-VM-UniXNU's `owner-hide-vmm`; added 2026-11-14):
  `sysctl kern.hv_vmm_present -> 1 (a virtual machine)` for a traced process, and for any process the boot argument
  `owner-hide-vmm=NAME,...` applied to (matched on `proc_best_name`, the long name), once per process:
  `-> 0 (the virtual machine hidden by owner-hide-vmm)`. A daemon that decides "virtual" with no such line asked
  something else (MobileGestalt's cached `IsVirtualDevice`, the device tree's `/defaults vmm-present`: the latter shows
  as a `property "vmm-present"` line).

A request is logged when first made, when its answer changes, and at its 16th, 64th, 256th, ... asking (a polling loop
shows with its count), at most 800 lines a boot. The property wrappers rename the original routines
(`hosted_impl_...`) and call them; nothing else changes. Built in `HOSTED_OUT=fixbt` on 2026-11-13 (see
`docs/hardware.md` 4 for what it showed on the PC); not run on the phone.

## 0066-free-page-runs.patch (hang, 2026-11-13)

**0058's give-back in long runs, and where its time goes.** On the phone (2026-11-12 23:45 boot) 0058 gave back 205
MiB as 52,602 runs in 822 batches: every run one 4 KiB page (this kernel's page) and one `ram_discard`. xnuhost spent
570 ms in those calls (11 us each); the guest's counter said 131,425 ms.

- **What the old counter was.** `hosted_fr_stats.ticks` ran from before the free queue's lock was taken to after the
  batch was back on the queue: the 1 s sleep between looks was never in it, but the lock wait, any time the utility
  thread was preempted (after every host call, when interrupts come back on) and interrupts handled on it were. Boot A
  below (0058's way, on the PC under load) shows what that time is: 49,144 ms by the clock, 1,567 ms on a CPU; 41,622
  ms of it was the host calls' stretch, and one hold of the free queue's lock lasted 772 ms (the mutex's holder was
  preempted). So the phone's 131 s was almost all the thread waiting for a CPU between its 52,602 calls [inference:
  the phone's line had no split; the PC's does], not work, and not the host.
- **What.** (`osfmk/vm/vm_resident.c`, `osfmk/arm64/hosted/free_report_runs.h`; all `#if XNU_HOSTED`, on 0058's code.)
  - Each cold page brings the free pages next to it in physical memory: up to `oneios-free-report-run` pages in one run
    (256 = 1 MiB; 1: none, 0058's way), ahead first, then behind, found through `vm_page_find_canonical` (O(1) in
    `vm_pages[]`) and taken only if on the regular free queue, held by the host (not given back) and from a colour
    queue still above its share of keep. Not with `HAS_MTE` (the free state is shared with the tagged classes there).
  - A batch is up to `oneios-free-report-batch` pages (1024 = 4 MiB, the most; 0058: 64), only as far as the free
    queues hold that much above keep, in whole slices; a small excess goes as before.
  - The free queue's lock is held for at most 128 pages at a time, taking or putting back, and taken in spin mode for
    the taking (preemption off: the hold is never the thread's wait for a CPU).
  - A batch is given back as its runs, heap-sorted by page and joined (`xh_fr_sorted_runs`), one host call each.
  - A fourth `oneios-mem:` line splits the time: `give-back time T ms: lock waits W, taking K (longest hold H us), host
    calls C, putting back P; on a CPU U ms; per MiB T/M us (U/M on a CPU); runs longest L pages, N neighbours`. "On a
    CPU" is the thread's own run time (`recount`); the guest cannot see a vCPU the host has descheduled, so that time
    is in it too. The start line adds `runs N pages`.
- **The decisions** (`xh_fr_sort`, `xh_fr_sorted_runs`, `xh_fr_batch_pages`, `xh_fr_neighbours`) are
  `free_report_runs.h`, the same file as `runtime/xnuhost/include/xnuhost/free_report_runs.h`; the host tests check
  they are one and test them (`tests/test_xnuhost.c`, "free report": the heap sort against 0058's insertion sort on 200
  orders up to 1024 pages, duplicates, the batch and neighbour rules).
- **Measured on the PC** (2026-11-13, qemu-aarch64, 8 vCPUs, `--ram 4G`, a fresh Data store each, `HOSTED_OUT=hang`,
  `kernel.kc` `5b823f11` for A, `62245ded` for B; boot arguments of `docs/building.md` 5 plus
  `oneios-free-report-stress=1 oneios-free-report-hysteresis=1`, which give back pages all the time; 960 s each, one
  after the other, the PC shared with other agents and swapping guest RAM: xnuhost's new line said 1,058 MiB of the
  guest's RAM swapped out by the PC at the end of A, 2,181 MiB at the end of B). A is `oneios-free-report-run=1
  oneios-free-report-batch=64` (0058's way; A's kernel also still took the lock as 0058 did, not in spin mode); B is
  the defaults. Both reached SpringBoard (A at 317 s, B at 305 s of the run), no panic. The last lines (MiB, ms, us):

  | | A (0058's way) | B (0066) |
  |---|---|---|
  | given back | 950 MiB in 3,894 batches | 1,093 MiB in 423 batches |
  | runs (host calls) | 233,969 (1.04 pages a run, longest 8) | 134,392 (2.08 pages a run, longest 615; 144,640 pages came as neighbours) |
  | xnuhost's time in the calls | 976 ms (1.03 ms per MiB) | 669 ms (0.61 ms per MiB) |
  | the guest's time by the clock | 49,144 ms (51.7 ms per MiB) | 45,497 ms (41.6 ms per MiB) |
  | of it: host calls / taking / putting back / lock waits | 41,622 / 2,982 / 4,528 / 2 | 35,348 / 3,013 / 7,131 / 1 |
  | the thread on a CPU | 1,567 ms (1,649 us per MiB) | 1,234 ms (1,129 us per MiB) |
  | longest hold of the free queue's lock | 772 ms | 711 ms |

  So per MiB 0066 costs the guest 32 % less of a CPU and xnuhost 41 % less, in a ninth of the batches; by the clock
  the thread waits for a CPU all the same (97 % of its clock time is not on a CPU, in both). On this PC the freed pages
  were mostly scattered (a run of two pages on average), so the runs were short; a bulk free (an app's exit) gives
  longer ones. The longest hold stayed at 0.7 s even in spin mode: the vCPU itself was off the PC's CPUs, or the
  `vm_page` entries it walked were in the PC's swap (2 GiB of the guest's RAM was) [inference]; the guest cannot
  prevent either. The clock time is not a cost to iOS (the pages of a batch are off the free queue meanwhile, 4 MiB at
  most, only far above the free target), but it is what the phone's 131 s were.
- **Not run on the phone.** What to read there: the fourth `oneios-mem:` line's per MiB on a CPU and by the clock, and
  `runs` against `batches` (0058's were 64 runs a batch).

## 0070-log-persist.patch (fix-wifi-join-2, 2026-11-14)

`oneios-log-persist=SUBSYSTEM[,SUBSYSTEM...]` (default: none, nothing written) keeps the info-level messages of those
log subsystems in the unified log: before launchd it writes `/private/var/preferences/Logging/Subsystems/SUBSYSTEM.plist`
on the Data volume (`/Library/Preferences` is a link to `../private/var/preferences` in 24A446's OS image), with
`DEFAULT-OPTIONS` `Level` `Enable` and `Persist` both `Info`, root 0644, and makes `Logging` and `Logging/Subsystems`
when missing. A file already there is left alone. The file stays in the Data store; delete it to go back. Why: CoreWiFi
says why `_CWFFilteredScanResults` drops a scan result (`[corewifi] Filtering based on maxAge (...)` and 15 other
reasons, subsystem `com.apple.WiFiManager`) only at the info level, and auto-join saw no result in the phone's boots of
2026-11-13 (`docs/hardware.md` 3, "Wi-Fi does not join"). For the next boot: `oneios-log-persist=com.apple.WiFiManager`
in the image's `xnuhost.conf`. Line: `oneios-logpersist: com.apple.WiFiManager: info kept (... written, 0)`, or `... is
there already: left alone`. [I] That logd honours the Data volume's file: `/Library/Preferences/Logging/Subsystems` is
the path iOS frameworks write their own overrides to (SafariShared, VoiceControl in 24A446's cache); not seen working
yet on the phone. **On the PC** (2026-11-14, `wifi=sim`, `kernel.kc` `552d08f1` from this branch, a fresh Data store): `oneios-logpersist: com.apple.WiFiManager: info kept (...)` before launchd, and iOS's log then held 14 info-level `com.apple.WiFiManager` entries of wifid (`[corewifi] ADDED LOCAL/DAEMON XPC CONNECTION ...`, `Mega WiFi profile is NOT installed`, ...); the system has no logging preference of its own for that subsystem (HomeKit and containermanager, whose info lines were kept too, have theirs), so logd honours the Data volume's file. Final build `kernel.kc` `bf31eb0d` (audit 0 failures, `prelink stubs-check` 666 good).

## 0067-ram-fixed.patch (fix-fixed-memory, 2026-11-13; in the series since fix-ram-fit, 2026-11-14)

**0058's give-back becomes the host's choice.** With xnuhost's `ram-giveback=0` the RAM is held fixed for the session
(`docs/booter.md` 2.2, "Fixed RAM"), and the host says so with `XH_FEATURE_RAM_FIXED` (bit 3 of `xh_info.features`, added
to the kernel's copy of `hostcall.h`). The patch: `hosted_fr_giveback`, set once in `hosted_free_report_start` from that bit
(`oneios-free-report-giveback=1` gives back anyway, `=0` never); the reporting loop runs only with it. The free queues keep
0058's order (freed pages first) and the `oneios-mem:` lines stay; with give-back off, their "given back" count is the
free pages never touched since boot (released at startup). A new start line: `oneios-mem: free pages given back to the
host: no (the host holds the RAM fixed); the free queues take the pages iOS freed first (0058)`.

- **State.** Written against `vm_resident.c` after 0066 and `hostcall.h` after 0003. Compiled on the Linux PC
  (2026-11-14, `HOSTED_OUT=fixram`, the series of main `f7bb1c3` plus 0067: `xnu.macho` `f9a4b99e`, 1541 s; no warning
  from it), prelinked (`kernel.kc` `e128c27b`: audit 0 failures, 2711 paired; `oneios-tool prelink stubs-check` 666 good,
  0 bad). Booted on the PC twice (`docs/booter.md` 2.2, "Fixed RAM", the A/B table): with `ram-giveback=1` the start
  line says `free pages given back to the host: yes (the host does not hold the RAM fixed)`, with `ram-giveback=0` `no
  (the host holds the RAM fixed)` and xnuhost counted no `ram_discard` call; no panic in 960 s each. Not run on the phone.
- **Without it** xnuhost's `ram-giveback=0` still works: the first `ram_discard` of 0058/0066 gets -EPERM and the kernel
  stops for the boot (`oneios-mem: ram_discard(...) failed: -1; free pages are no longer given back`); one batch's pages
  go back to the head of the free queue.
- **0058/0066 now (fix-ram-locked, 2026-11-15):** give-back is opt-in. xnuhost's default is `ram-giveback=0`, the RAM
  prefaulted and locked (`docs/booter.md` 2.2, "Fixed RAM"), so with this patch the kernel starts with `oneios-mem: free
  pages given back to the host: no (the host holds the RAM fixed)` and its reporting loop never runs (no CPU spent on
  give-back rounds; the `oneios-mem:` lines stay). Why: the phone's session of main `3233fed` (`ram-giveback=1`,
  `ram=auto` at a fitted 6,656 MiB) still had Linux swap iOS's RAM, 138 -> 604 MiB at up to 37.9 MB/s of swap-outs,
  with about 1.9 GB of other page cache there: a size that fits is not enough under Samsung's swappiness, only the
  lock is. `ram-giveback=1` gives 0058/0066's way back with one setting. No kernel change: 0067 already does what the
  new default needs.

## 0071-cache-maintenance-fault-read.patch (fix-webkit-icache, 2026-11-14)

**A cache maintenance fault is a read.** The phone's WebContent crash report (2026-11-14 06:03, main `9f6dc38`):
`KERN_PROTECTION_FAILURE`, "(Data Abort) byte write Permission fault", in `sys_icache_invalidate` of JSC's first JIT
thunk (`JSC::LLInt::createJSGateThunk`), on the read-execute alias of the JIT region. A `dc cvau` / `ic ivau` that
faults reports ESR ISS.WnR = 1 **and** ISS.CM = 1 (Linux passes the ESR of a user address on whole); 0002's trap path
read WnR alone: `hosted_trap_access` said `XH_PROT_WRITE`, `hosted_user_fill` refused it on the read-execute page, and
`hosted_trap_fault_esr` rebuilt the syndrome as a permission fault with WnR and no CM, which `inspect_data_abort`
(sleh.c: a write when WnR is set and either CM is clear or the fault is a permission fault) turned into a write
`vm_fault` on a read-execute entry. The patch, `osfmk/arm64/hosted/`:

- `hosted_user.c`: `hosted_esr_is_cache_maintenance` (EC 0x24 with CM); `hosted_trap_access` returns `XH_PROT_READ` for
  it before it looks at the stub's `access` (so a runtime older than the stub's own fix is covered too);
  `hosted_trap_fault_esr` gives it to XNU as the hardware reported it, CM and WnR kept, with the fault status an
  access flag fault (level 3) instead of a permission fault: XNU's CM rule makes it a read, and `arm_fast_fault` is
  still tried before `vm_fault` (it is for every fault that is not a translation fault; its `was_af_fault` argument is
  unused). Stores and loads are unchanged (a permission fault, WnR for a write).
- `hosted_machine.c`: the kernel's own faults (`hosted_exception_upcall`'s fill) take CM as a read too. (The kernel
  runs at EL0 on the host, so it can only run the cache maintenance EL0 may, all of which need read access only.)
- `hosted_selftest=1`: a new check, "a cache maintenance fault (CM, WnR) reaches XNU as a read with CM kept; a store as
  a write", on synthetic traps (the stub's old `XH_PROT_WRITE` included).

The runtime half is in the stub (`runtime/abi/src/stub.rs` `fault_access`, host test `fault_access_of_syndromes`):
either half alone fixes the crash. Tests: `docs/booter.md` 2.6 and 6 (`test_jit_icache`; qemu-user never faults on
cache maintenance, so only arm64 hardware runs the fault itself).

- **Built and booted on the PC (2026-11-14).** Written and built as 0070 and renamed to 0071 before the merge (0070 is
  fix-wifi-join-2's): the rename changed only the number in comments. `HOSTED_OUT=fixic` built in 844 s; the prelink's
  audit 0 failures, `stubs-check` 666 good. A PC boot (qemu-aarch64, 8 vCPUs, `--ram 4G`, a new data store,
  `hosted_selftest=1`, 1500 s): `hosted selftest: 29 passed, 0 failed` with the new check `ok`; SpringBoard and
  backboardd up (backboardd opened the framebuffer), no panic; the only exception was chronod's
  (`type=0xa`), which an earlier PC boot on main had too. No WebContent process ran (Safari is not opened on the PC),
  and qemu does not fault on cache maintenance, so the fix itself is proven on the phone only.

## 0073-mac-exec-label-report.patch (2026-11-20; ported from Musicalista-OneiOS-A56)

A debugging aid, off by default. When an exec's credential update is refused, the console has only the hook's error
(a launcher's `xpcproxy` exiting with a security-policy code, nothing naming the policy, and a daemon then waited on
forever). 0073 wraps `mac_cred_label_update_execve`'s two `mac_error_select` calls: one line per (executable, policy,
error), at most 200 per boot (`oneios-mac-exec: NAME: policy P refused the credential update (error E)`), and with
`oneios-mac-exec-skip=NAME[,NAME...]` a Sandbox refusal for those executables is ignored (they run unsandboxed).
Other policies are never skipped. Applies cleanly to xnu-12377.121.6 (checked with `patch -p1 --dry-run`); the kernel
is not rebuilt here, so the first boot with it proves it.

## gpu/: the blobs' kinds and ages, `oneios-cpu`'s span (hang, 2026-11-13; the archive, not a numbered patch)

No XNU source changes, so no patch in the series: `oneios-tool kernel gen gpu` compiles a staged
`DroidVMVirtioGpu3D.cpp` (`staged/gpu.rs` `GPU3D`: weak hooks at a blob's creation, deferred close and release), and
`gpu/OneiOSAdreno.cpp` keeps a table of the live blobs (24,576 at most) and prints `oneios-gpu-kinds:` and
`oneios-gpu-ages:` per client with the GPU report (`docs/gpu.md` 5.1: own blobs in the task or the kernel, mappable;
wired by the VM tag of the caller's mapping; sizes under 1 MiB; ages; closed blobs still held). `oneios-cpu:` now divides
by the time measured since its last walk and prints those seconds (it divided by the nominal 10 s: a late report in a
stall inflated every share). Built into the kernel of 0066's boot B and booted on the PC (no 3D there: the hooks are
never called, the table is never made; the `oneios-cpu` lines are). Not run on the phone.
Since 2026-11-14 (fix-thermal-log) an `oneios-cpu-sys:` line follows each `oneios-cpu:` line with the same processes'
kernel time (`docs/gpu.md` 5.1); compiled alone with the kernel build's own flags, not yet in a built kernel.
Since 2026-11-15 (fix-host-overhead) `oneios-cpu:`'s "all" is the processors' busy time (the span times the CPUs, less
the time they idled: hosted 0075's `hosted_processors_idle_mach`), and each process's share is capped at it
(`gpu/cpu_window.h`, the same file runtime/xnuhost's host tests check). The cause of a window over 800 % on 8 vCPUs,
besides the old nominal divisor: recount adds a thread's time to its task only when the thread changes state, so time
from earlier windows lands whole in the window where a long-running thread finally switched, and "all" was the sum of
those deltas with no bound. A process's lump can still land in one window, but never past what the CPUs did in it.
Since 2026-11-15 (fix-hapticd) the report thread also runs when no Metal accelerator is published (a host with no 3D:
the PC under qemu-aarch64), so a PC boot has `oneios-cpu:` lines too; its GPU lines stay quiet there (nothing submits).

## 0075-host-overhead-per-cpu-stats-perf-hint.patch (fix-host-overhead, 2026-11-15)

From the phone session of main 3233fed (30 s): `1021984 user stays; 684514 syscalls (bsd 473429, 26066040 us; mach
211370, 8850591 us ...); 330746 faults, 49701 filled from the tables (11417628 us), 281312 to vm_fault (6309604 us),
281229 refilled; ... pmap: user maps 331008, unmaps 414758 ... TLB lock taken 232732, 2775 waited (125098 us waiting
...)`. What the patch changes:

- **The counters are per vCPU.** `hosted_user_stats` (runs, syscalls, faults, ...), the audit tick (an atomic add on every
  return to user mode) and 0041's per-call counters (`hosted_perf`, atomics on shared lines) were written by every vCPU on
  every stay: about 34,000 stays a second on the phone, each taking the line from the vCPU that wrote it last. Now an array
  of one 128-byte-aligned set per vCPU; the report adds them up.
- **The fill's time is its own.** The fill runs with interrupts on, and the interrupt that ended the stay is often taken
  there, with its preemption: the thread then waited while other threads ran, and that was counted as the fill's. 49,701
  fills "took" 11.4 s per 30 s (fill_t also counts the 281,312 attempts that went on to vm_fault: 34 us each on average),
  against 22 us per vm_fault. A fill during which the thread was switched out (`thread->c_switch` moved) is now counted
  apart (`fills switched out N`) and not timed. Nothing in the fill's own work changed: the number is now honest.
- **A kernel entry made again unchanged maps nothing in the host.** `pmap_enter_options_addr` reads the kernel's leaf
  entry before and after the enter (`hosted_kernel_pte`); when it is the same valid entry, its host mapping exists (made by
  `hosted_kernel_enter` when the entry was made, and a flush maps a still valid entry again at once), so no `kernel_map`:
  each one is an mmap in xnuhost with its mmap_lock taken for writing, which holds every vCPU's page faults meanwhile. The
  PC's idle boot made 220,270 kernel maps per 30 s with 1,468 kernel unmaps. The count: `kernel entries made again
  unchanged N (no host map)`. Measured on the PC boot (2,900,519 kernel maps in 2,400 s): 0, so there the kernel maps are
  new entries and the guess was wrong; the check stays (one walk per kernel enter) for the phone's count.
- **The hosted CLPC's hint** (`hostcall.h` `perf_hint`, ABI unchanged: a reserved word of `struct xh_cpu`): before each
  user stay the kernel writes the class of the thread (`thread_get_perfcontrol_class`, darwin background from the task's
  policy): realtime, UI and above-UI floor 1024, user-initiated 768, utility and background (and darwin-background) no
  floor and, since 2026-11-16, no ceiling of ours (fix-host-overhead's 512 ceiling slowed chronod's widget extensions;
  the owner's no-caps rule ended it). xnuhost's `thread_run` turns it into the uclamp of the Linux thread that runs the user code
  (`docs/booter.md` 2.6a, 2.6b). RunningBoard's background and suspended roles reach it through the task's QoS clamp. The
  same word is the role of the submitting thread for host code on the vCPU (the GPU path, fix-gpu-perf). `perf hints N`
  counts the changes.
- **`hosted_processors_idle_mach`** for the oneios-cpu window (above).
- **The GPU path's CLPC signal** (fix-gpu-perf #42 declares both weak in `gpu/OneiOSAdreno.cpp`, this defines them):
  `oneios_clpc_thread_role(thread)`, the task's effective role (`TASK_POLICY_ROLE`, RunningBoard's), darwin background
  as `TASK_DARWINBG_APPLICATION`, a realtime thread of a task without a role as `TASK_GRAPHICS_SERVER`; and
  `oneios_clpc_frame_deadline(thread)`, the deadline of the thread's work interval as its last notify gave it
  (`kern_work_interval_notify` stores it in the interval, `wi_hosted_deadline`; 0 outside one). Compiled with the tree
  and built into the kernel; on the PC (no 3D) nothing calls them.

The report's new line: `oneios-perf:   0075: fills switched out N (not in the fill time); kernel entries made again
unchanged M (no host map); perf hints H`. Built and booted on the PC (qemu-aarch64; before/after in `docs/booter.md`
2.6c); not run on the phone. On the phone: the 0075 line, the fill time per fill against before, `kernel maps` in the
pmap line, and xnuhost's `perf:   queue:` and `thermal: 30 s: host CPU ... sys ...` lines.

## 0089-smp-scheduler-init-completed.patch (fix-hosted-smp-scheduler-init, 2026-11-23)

The fault: 0085's E -> P spill did nothing on the phone. From `oneios_board_late_start` it printed `oneios-edge: not all
CPUs are up yet: the edges stay XNU's`, long after all 8 vCPUs had entered the kernel; moved to the end of
`sched_cpu_init_completed` (main `6376994`), it printed nothing at all (the boot of 2026-11-23 02:16).

Cause: `sched_cpu_init_completed` never ran. XNU calls it through `ml_cpu_init_completed` once every CPU has been
registered (`ml_processor_register`) and booted (`processor_boot`). Its only arm64 caller is AppleARMSMP
(`iokit/Kernel/arm/AppleARMSMP.cpp`, all under `USE_APPLEARMSMP`), which this board (BCM2837's board config) does not
build. Here UniXNU's ArmCpu registers each vCPU, and `ml_processor_register` boots it itself (its `!USE_APPLEARMSMP`
branch). ArmCpu calls `ml_cpu_init_completed` only with its boot-arg `armcpu-edge=1`, which nothing sets (its
`processor_start` call is a no-op in this XNU, `processor_start_from_kext`). Apple's AppleARMPlatform imports the
function too, but its CPU driver does not run here. So `pcs_init_completed` stayed false all session, and the whole
completion step never ran: the Edge scheduler's default edges and each pset's `max_parallel_cores` and
`max_parallel_clusters` (`sched_edge_cpu_init_completed`), the realtime spill rules (`rt_init_completed`), the wait for
every CPU (`check_all_cpus_are_done_starting`), the `sched_update_powered_cores` thread, and 0085 at its end.

What the patch changes (`XNU_HOSTED`):

- `osfmk/arm64/machine_routines.c` `ml_processor_register`: after its `processor_boot`, `hosted_cpu_booted(processor)`
  (`osfmk/arm64/hosted/hosted_smp.c`) counts the boots. The last of `ml_get_topology_info()->num_cpus` starts a kernel
  thread (`oneios_cpu_init_completed`, `MAXPRI_KERNEL`) that calls `ml_cpu_init_completed`, as AppleARMSMP does after
  its `processor_boot` loop. A thread, not the caller: the caller is ArmCpu's start, which after `ml_processor_register`
  still sets the IOCPU's number and state, and the step waits for every vCPU to reach `processor_up`. It is not
  called when `machine_info.logical_cpu_max` differs from that count (`sched_cpu_init_completed` asserts that every one
  of `logical_cpu_max` CPUs is online), and not with `oneios-smp-complete=0` (the old way).
- `ml_cpu_init_completed`: exactly once. `hosted_cpu_init_completed_first` lets the first call through; a later one
  (ArmCpu's, with `armcpu-edge=1`) is one log line instead of the scheduler's `sched_cpu_init_completed called twice`
  panic.
- `osfmk/kern/processor.c`: `cpu_boot_timeout_secs` 10 s instead of 1. The completion step now waits for each vCPU
  (`processor_wait_for_start`) and panics on a timeout, as on an iPhone; a vCPU is a host thread that xnuhost makes at
  PSCI CPU_ON and One UI's scheduler runs beside its own load (under qemu-user, an emulated one). The boot-arg still
  sets it.

The assertions of the step hold here: all 8 processors are booted before it (the count), each is in
`pcs_effective.pcs_online_cores` once it ran `processor_up` (the wait comes before the asserts), `ml_cpu_can_exit` is
false for every CPU without `USE_APPLEARMSMP` (so `pcs_required_online_system` is all of them: no vCPU is ever powered
down), and no `processor_boot` comes after (`processor.c` asserts `!sched_is_cpu_init_completed()` there).
`sched_edge_config_verify` is advisory on this board (UniXNU's patch). What else changes once `pcs_init_completed` is
true: `sched_override_available_cores_for_sleep` no longer panics ("Attempting to sleep before all CPUS are
registered"), and the recommended cores are masked with the (full) powerdown-recommended set.

Lines: `oneios-smp: all 8 CPUs booted (the last cpu N): ml_cpu_init_completed (the scheduler's completion step) on
its own thread`, then 0085's `oneios-edge: E -> P migration and steal allowed on ...`, then `oneios-smp: the
scheduler's completion step done in M ms (every CPU running; pcs_init_completed 1)`. M is the margin against the
10 s timeout. A second call: `oneios-smp: ml_cpu_init_completed called again ...`.

Not folded into 0085: it is its own cause (the completion step), and it changes more than the spill (the Edge
defaults, the realtime rules, the powered-cores thread, the sleep path); 0085 stays the spill alone.

Tested (build laptop, 2026-11-23): written by the fix-hosted-smp-scheduler-init helper as 0088, renumbered 0089 (0088 is the
CMCapture model folder). `HOSTED_OUT=combo3 ONEIOS_CRYPTO=original kernel/build-hosted.sh` with 0084-0089: 174 patches, `xnu.macho`
sha256 `ae21dcbf...`, releasable; the prelink: `kernel.kc` sha256 `988c2a7b...`, audit 2711 paired, 93 on the stub, 0
failures; `prelink stubs-check` 666 good, 0 bad; `kernel/tests/host.sh` 8 of 8; the `oneios-smp:` strings are in the kernel.
Not booted under qemu (the owner asked for the combined phone boot first); staged on the phone 2026-11-23 for that boot.

On the phone (2026-11-23 03:28, kernel `988c2a7b`): `oneios-smp: all 8 CPUs booted ...`, `oneios-edge: E -> P migration
and steal allowed on 6 edges ...`, `oneios-smp: the scheduler's completion step done in 10 ms (... pcs_init_completed
1)`. vCPU 2-7 carried work (22-38% idle where they were 80-99%), and the threads went back and forth between the
clusters: 0091.

## 0091-edge-no-pullback-ipi-counts.patch (fix-fps-scheduler-migration, 2026-11-23)

The fault: with 0089, page switches felt slower than in the boots before, and the host did far more work for the same
screens. xnuhost's lines of the 2026-11-23 03:28 boot against the boot before (0085 inactive), per 30 s: task traps
48.8k/s against 17-18k/s, `release: N iOS threads put with another vCPU's core (sched_setaffinity)` 70,200 against
about 15,000, `kick` 24,419 (mean 136 us, p99 3.7 ms) against about 950, vCPU futex sleeps 1.33 million against
about 130,000; the host at 567-711% of 800 with the SoC at 95 C (thermal "heavy" for 85 s). The work itself spread as
0085 meant (every `oneios-cpu:` line "all 800%" while apps started, the RPC round trip 645 us against 55,786 us, a woken
thread's wait for its IPI 4.2 ms against 345 ms); the moves and kicks did not have to.

Cause (`osfmk/kern/sched_clutch.c`, read in this tree): once the completion step has run, the Edge scheduler takes a
spilled thread back to its preferred cluster at once, in three places. An E CPU that goes idle and finds a P CPU
running a thread that prefers E sends that CPU a rebalance IPI (`sched_edge_balance`, from `processor_idle`; until it
does, `sched_edge_processor_idle` returns no thread instead of stealing); a P CPU's `csw_check` preempts such a thread
when the E cluster's load metric is 0 (`sched_edge_thread_avoid_processor`: "compaction") or when any cluster of its
preferred type has that load ("running rebalance"). With two E vCPUs that go idle and busy thousands of times a
second and 0085's weight 0 sending the threads to P again, a thread rarely ran twice on the same vCPU: 70,200 moves
against about 131,000 dispatches in the same 30 s (the `wake:` line's 52,376 plus 78,772 others). On an iPhone the IPI
and the move cost about a microsecond; here an IPI to a busy vCPU is a kick of the iOS thread it runs (a signal; the
`kick` line), each move a `sched_setaffinity` of that thread to the new vCPU's core (`release:`), and the thread
lands on an A520, about half an A720's speed.

What the patch changes (`XNU_HOSTED`; `oneios-edge-pullback=0`, the default):

- `sched_edge_balance`: an E CPU going idle sends no rebalance IPI; it counts the case when a spilled thread was there
  to take (`sched_edge_foreign_running_thread_available`, the lock-free scan XNU itself uses).
- `sched_edge_processor_idle`: an E CPU then goes on to steal by the edges' weights (XNU's P -> E edge: 64 us of
  scheduling latency, steal allowed) instead of returning no thread for the rebalance.
- `sched_edge_thread_avoid_processor`: a thread that prefers E, running on a P CPU, is not preempted by the
  compaction and running-rebalance rules; it runs until it blocks or its quantum ends, and its next wake-up places it
  by the edges again (E when E has room). The rules above them (a bound thread, an edge that no longer allows it, the
  shared-resource policies, realtime, stir-the-pot) are unchanged.
- A thread that prefers P and runs on E (the UI's threads when P was full) is still pulled to an idle P CPU: only
  the move from a P CPU to an E CPU is left out.
- `sched_ipi_action` (`osfmk/kern/sched_prim.c`): each IPI the scheduler sends is counted by its event (bound thread,
  preempt, SMT rebalance, spill, rebalance, realtime preempt) and by whether the CPU was idle or busy.
- `oneios-edge-pullback=1` is XNU's way (the IPIs and preemptions are made, and counted).

The line, every perf period (`osfmk/arm64/hosted/hosted_smp.c` `hosted_edge_report`, after 0081's): `oneios-perf:
0091: spilled threads kept on P: N pull-back IPIs not sent, M preemptions not made; pull-back IPIs sent K
(oneios-edge-pullback=0); scheduler IPIs to idle/busy CPUs: preempt a/b, spill a/b, rebalance a/b, realtime a/b, bound
a/b, other a/b`. The busy counts are the kicks the scheduler asked for; xnuhost's `kick` and `perf:   queue:` lines say
what they cost.

Not changed: 0085's weights (default and higher at 0: the spill that took the RPC round trip from 56 ms to 0.6 ms),
the steal of an idle P CPU from E, the UI's P preference. An A/B without a rebuild: `oneios-edge-pullback=1` against
the default in two boots with the same screens.

Tested (build laptop, 2026-11-23): `HOSTED_OUT=fps ONEIOS_CRYPTO=original kernel/build-hosted.sh` with 0084-0089 and 0091 (no 0090 in this tree), `xnu.macho`
sha256 `9a9e0a3c...`; the prelink: `kernel.kc` sha256 `ca1606e5a804383c426cc486756622779398d999699f583164434eca746fe966`, audit 2711
paired, 93 on the stub, 0 failures; `prelink stubs-check` 666 good, 0 bad; `kernel/tests/host.sh` 8 of 8; the `0091:`
line and the `oneios-edge-pullback` boot-arg are in the kernel. `git apply --check` in a tree of 0001-0089 with the camera
helper's 0090 applied (`t3/camera-preview-stream-fix`, `0090-camera-plugin-boot-addon.patch`, only `bsd/kern/kern_exec.c`):
0091 applies after it. Numbered 0091 for that 0090. Not booted under qemu or on the phone yet. On the phone:
the `0091:` line (pull-backs not sent; preempt IPIs to busy CPUs), and xnuhost's `release:` and `kick` lines back near
the boot before 0089 (about 15,000 moves and 950 kicks in 30 s) while `perf: cpuN:` still shows vCPU 2-7 busy.

## 0086-audio-tunings-actuator-chain.patch (fix-audio-volume-haptics, 2026-11-22)

The fault: no haptic is felt. With 0078 audiomxd's haptic server starts and plays, and the Actuator device runs I/O,
but every period is silence (`ONEIOS HAPTIC: counters: ... 61920 periods (309600 ms), 0 with samples, 0 loud`;
audiomxd's `RTAID [ ... node=Actuator-Output ... ] -- [ rms:[-120.000000], peaks:[-240.000000] ]`). VirtualAudio
makes the Actuator's virtual stream with the DSP chain `d42_actuator_general` and finds no strips file for it:
`RoutingSettings_Aspen.cpp:700 Cannot find strips file oneios,xnuhost/VAD/d42_actuator_general`. The chain turns
the haptic synth's 2-channel stream into the actuator's one channel; the iPhone's is
`/Library/Audio/Tunings/AID8022/VAD/actuator_general.{austrip,dspg}` (in 2 -> AUHapticsProtection -> 1 ->
AUVolumeForHaptics -> out; AID8022 is the V54's `/product/audio acoustic-id`). `docs/hardware.md` section 5, "The
Actuator played silence".

What the patch changes (`bsd/kern/kern_exec.c`, `XNU_HOSTED`, in 0078's step before launchd):
`oneios_audio_tunings_folder()` makes `/private/var/oneios/addons/AudioTunings/oneios,xnuhost` a folder of links
instead of 0078's one link to the board (an existing link there is removed first; entries already there are kept):
- `Haptics -> ../V54/Haptics` (`../NAME/Haptics` with `oneios-audio-tunings=NAME`): the preset, as 0078;
- `VAD/d42_actuator_general.austrip -> ../../AID8022/VAD/actuator_general.austrip` and the same for `.dspg`
  (`oneios-audio-tunings-vad=0`: not made).
The links resolve in 0078's read-only union over `/Library/Audio/Tunings` to the sealed folders; nothing of Apple's is
copied. After the mount the step looks the strip up through `/Library/Audio/Tunings`. One line (replaces 0078's):
`oneios-audio-tunings: /private/var/oneios/addons/AudioTunings over /Library/Audio/Tunings: oneios,xnuhost/Haptics ->
V54/Haptics, oneios,xnuhost/VAD/d42_actuator_general -> AID8022/VAD/actuator_general (N entries made), the Actuator's
strip visible; stage 5, status 0 (...)`; an error: `oneios-audio-tunings: oneios,xnuhost's folder: error N (0078's
link, if there, kept)`. The first boot of a store with 0078's link says 5 entries made (the folder, Haptics, VAD and
the two strip links), later boots 0.

Numbered 0086 (0085 is the Edge spill patch): 0084 is the camera helper's MobileGestalt cache patch, which adds a function after 0078's; the two
apply in either order (checked with `patch --dry-run` on the 0083 tree, both orders).

Tested (Mac, 2026-11-23): `HOSTED_OUT=audio kernel/build-hosted.sh` applied the series with 0086 (962 s, `xnu.macho`
sha256 `dbf9fc9f...`, no warning in the new code); `KERNEL=out/kernel/current-hosted-audio/xnu.macho
OUT=out/kernel/audio-24A446 kernel/prelink/build.sh`: `kernel.kc` 96,845,824 bytes, sha256 `a50202ea0aa3a71c...`,
audit 2712 paired, 0 failures; `prelink stubs-check` 666 good, 0 bad; `kernel/tests/host.sh` 8 of 8. Not on the phone. The
lines that prove it on the phone: the `oneios-audio-tunings:` line above with `the Actuator's strip visible`; in iOS's
log no `Cannot find strips file oneios,xnuhost/VAD/d42_actuator_general`, RTAID's `node=Actuator-Output` above -120 dB
while a haptic plays; `ONEIOS HAPTIC: counters: ... N with samples ... N requests to the motor` and xnuhost's
`haptics: 60 s: ... +N played` with N > 0.

## 0085-edge-e-to-p-spill.patch (fix-launch-control-center-fitness, 2026-11-22)

The owner's report (main `23561aa`, kernel 0082 + 0083 + codec): "we are close to 120 Hz but these laggy parts need
fixes": app opening animations lag, Control Center is laggy, Fitness opens to a black screen. iOS's log of the same
boot later (`s9b-ioslog`): SpringBoard 90 hangs (214.9 s in all, the longest 21.08 s, 29 over 1 s) and chronod 126
watchdog kills of widget extensions that did not answer in time (`getAllDescriptors`, timelines; 5 to 30 s of wall time).

What was found (`out/session-logs/2026-11-22/kbd-debug/xnuhost-s9.log`, `-s8-final`, `-s6-final`; `s9-ioslog`,
`s9b-ioslog`; the backboardd samples of `live10.log` with `oneios-tool log stacks --proc backboardd`):

- **The render server is not GPU-bound.** `gpu: 10 s level:` while it animates: 200-320 frames in 10 s, the frame
  interval 43-82 ms, GPU time at the top 1.4-5.6 ms (over the 8.3 ms budget only in a few frames); the display helper
  posts each frame in 10-13 ms (`display: 10 s:`).
- **Two vCPUs do the work and six wait.** xnuhost's `perf: cpuN:` lines, every 30 s period after the first frame, in
  all three boots: vCPU 0 and 1 (E, the A520s) 0% idle; vCPU 2 10-28%, 3 5-17%, 4 2-13%, 5 1-8%, 6 0-7%, 7 (the X4)
  0-5% busy, i.e. 85-99% idle, the fastest core the most idle of all.
- **Why.** The kernel is AMP (`droidvm-amp`: an E and a P processor set, the Edge scheduler). DroidVMCpufreq
  (`DroidVMCpufreq: thread groups prefer cluster 0 (E), classes 0x188 cluster 1 (P)`) gives every thread group the E
  cluster and only the UI, above-UI and realtime classes the P cluster. XNU's default edges
  (`osfmk/kern/sched_clutch.c` `sched_edge_cpu_init_completed`): P -> E `weighted_spill`, **E -> P `no_spill`** (no
  migration, no steal). On an iPhone CLPC changes the edges and the groups' clusters as the load changes; here no CLPC
  runs. So every default, user-initiated, utility and background thread stays on the two A520s however idle the P
  cores are: daemons and XPC services an app launch waits on, an app's workers, widget extensions, and in the render
  server metal2vulkan's GPU submission thread (a default-class thread, DroidVMCpufreq's own comment): the render thread
  waits for it every frame (`CA::OGL::MetalContext::flush` -> `dispatch_group_wait` in the plug-in, `live10.log`).

What the patch changes (`osfmk/arm64/hosted/hosted_smp.c` `hosted_sched_edge_spill`, called at the end
of `sched_cpu_init_completed` in `osfmk/kern/sched_prim.c`, `XNU_HOSTED`): once all CPUs are up
(`sched_is_cpu_init_completed`; until 2026-11-23 the call was in `oneios_board_late_start`, which on the phone ran before
that point: the boot of 2026-11-23 logged `oneios-edge: not all CPUs are up yet: the edges stay XNU's`, so the edges
were never opened; and on this board `sched_cpu_init_completed` itself ran only since 0089, so the 02:16 boot with
the call there printed nothing), `sched_edge_matrix_set` opens the E -> P edges: migration and steal allowed for the
fixed-priority, foreground, user-initiated and default buckets at weight 0, utility at 64 and background at 1000. The
weight is compared with the clusters' load, their average scheduling latency in us (0 with an idle CPU): a thread goes
to P when P has an idle CPU or waits less, background only when E's threads wait a millisecond longer (RunningBoard
clamps a widget extension or a suspended app to background QoS). A thread still starts on its preferred cluster when
that has a free core. P -> E and the realtime edges stay XNU's. `oneios-edge-spill=0` is the old way, `=1` keeps
background on E (an iPhone's rule), `=2` is the default. One line: `oneios-edge: E -> P migration and steal allowed on
6 edges (1 E and 1 P clusters): fixed-priority, foreground, user-initiated and default at weight 0, utility at 64,
background at 1000 (us of scheduling latency; oneios-edge-spill=2; XNU's default: none)`.

What it costs: background work may now run on the A720s and the X4 when the A520s are behind (more power and heat
than before; `thermal:` lines watch it, and xnuhost's thermal masks still apply).

Fitness is not a render failure (same logs): SpringBoard logs `scene content state changed: ready` for it (its first
frame was committed), the app is active (deactivation reasons 0) and in dark mode, and it logs `Waiting to show
onboarding until initial queries complete`. Its first screen waits for fitcored's Fitness+ queries
(`WorkoutPlanClient.queryActiveWorkoutPlan ended in 82.3 s`, `UpNextQueueClient.queryUpNextQueue ended in 80.4 s`,
`ServiceSubscriptionClient.fetchServiceSubscription ended in 93.3 s`), which go through AppleMediaServices' bag with no
network (`Network is down`, Wi-Fi joined 0) and no storefront (`AMSErrorDomain Code=205`); `CA Telemetry timedout after 20
seconds due to app launch has not reached all responsive milestones`; then the onboarding comes (`onboarding
coordinator phase did change: 1 ... is presenting onboarding: 1`). The services it looks up and does not find
(amsengagementd, itunescloudd's in-app messages, fairplayd) fail at once (`error = 3`); they are the daemon policy's
on purpose. This patch takes the CPU part out (fitcored and healthd are default and utility threads); the network part
stays until the guest is on Wi-Fi.

Tested (Mac, 2026-11-23): `HOSTED_OUT=anim kernel/build-hosted.sh` applied the series with 0085 and built `xnu.macho` in
447 s (25,646,672 bytes, sha256 `555a98d6...`; `_hosted_sched_edge_spill`, `_sched_edge_matrix_set` and the four
`oneios-edge:` texts are in it; no warning in the new code); no reference kernel (it does not link since 0074, see 0083).
`KERNEL=out/kernel/current-hosted-anim/xnu.macho OUT=out/kernel/anim-24A446 kernel/prelink/build.sh`: `kernel.kc`
96,845,824 bytes, sha256 `7b1057e2...`, audit 2,712 paired, 92 on the stub, 9 data unpaired, 0 failures; `prelink
stubs-check` 666 good, 0 bad; `kernel/tests/host.sh` 8 of 8. Not booted yet. On the phone these lines prove it: the
kernel's `oneios-edge: E -> P migration and steal allowed on 6 edges (1 E and 1 P clusters) ...`; xnuhost's `perf:
cpuN:` lines with vCPU 2-7 busy whenever vCPU 0-1 are (not 85-99% idle beside two 0%-idle vCPUs); `gpu: 10 s level:`
during an app launch and Control Center with a frame interval near the GPU time (not 43-82 ms); `oneios-tool ioslog
health` with SpringBoard's hangs and chronod's watchdog kills far under 90 (214.9 s) and 126; `thermal:` lines for the
cost.

## 0083-dictation-status-navd.patch (fix-spotlight-keyboard, 2026-11-22)

The owner's reports: in Spotlight the keyboard never comes up; in Notes one character comes, then a long lag; on the
Wi-Fi password field of Settings the keyboard comes up but each key takes very long. All three on a fresh Data store
with no network.

What was found (iOS's log of the fresh boot `out/session-logs/2026-11-22/fresh/ioslog.txt` and of the Wi-Fi boot
`s6-ioslog/ioslog.txt`; `xnuhost-s5.log` and `xnuhost-s6.log` with `oneios-sysfail` and `oneios-ubt`; AssistantServices
and UIKitCore of the 24A446 cache disassembled with `ipsw dyld disass`, the full cache copied out of `systemos.img`):

- **The loop.** Spotlight's main-thread samples (`log stacks`, unslid with the boot's slide and named with
  `ipsw dyld a2s`): `-[UIDictationController dictationConnnectionDidChangeAvailability:]` -> `postNotificationName:`
  `UIKeyboardDictationAvailabilityDidChangeNotification` -> `-[UISystemKeyboardDockController updateDockItemsVisibility]`
  -> `configuredGlyphWithName:` -> `+[UIImage imageNamed:inBundle:...]` -> `_UIImageCollectImagePathsForPath`, the
  12 failing `faccessat` of `CoreGlyphsPriority.bundle/mic...` per lookup (the catalog has `microphone`; `mic` is
  an alias in CoreGlyphs' `name_aliases.strings`). Spotlight held 103-107% of a core with ~172000 such faccessat per
  30 s; Settings looked up `mic` 277 times in the 1.3 s before `oneios-sysfail-max` and its main thread stayed busy for
  more than 389 s (HangTracer's 5, 9, 17, 34, 71, 157, 389 s are one hang reported at doubling lengths).
- **Why it never ends.** The dock observes that notification (`viewDidLoad`) and, when the device is unlocked,
  `+[UIDictationController dictationIsFunctional]` calls `-[AFDictationConnection dictationIsAvailableForLanguage:]` with
  no "dictation enabled" check. With no network (`AFNetworkAvailability isAvailable` NO) that calls
  `forcedOfflineDictationIsAvailableForLanguage:synchronous:`, which caches at `self+0x20` the keys of CFPreferences
  `Offline Dictation Status` (domain `com.apple.assistant.support`, read by `-[AFPreferences offlineDictationStatus]`,
  only an `NSDictionary` counts) whose `Installed` is true, and each time it fills the cache it dispatches
  `_availabilityChanged` -> `_tellSpeechDelegateAvailabilityChanged` -> UIKit's `dictationConnnectionDidChangeAvailability:`.
  With no value the cache is `[nil keysOfEntriesPassingTest:]` = nil: it is filled again at every check, and every check
  posts again. On an iPhone assistantd writes the status; the daemon policy disables assistantd (group base).
- **What it did to the keyboard.** Spotlight's main thread was 13.78 s busy at launch, so InputUI's keyboard arbiter
  dropped Spotlight's client (`timed out with 1 assertions remaining, disconnecting`, then `Failed to access ... remote
  service ... Code=4099`): no keyboard. (The arbiter's wait looks like 0.1 s in `ioslog show` before 2026-11-22: its
  times were 52 times too short, fixed in `oneios-tool ioslog`, see tools/oneios-tool/README.md "ioslog".)
- **navd.** In the same Wi-Fi boot GeneralMapsWidget logged ~28000 `Opening connection to navigation server` /
  `failed to do a bootstrap look-up: xpc_error=No such process` for `com.apple.navigationService` (navd, disabled by
  group base), with no back-off: `oneios-cpu` GeneralMapsWidget 54-103%, geod 30-46%, launchd 13-24% next to Settings.

What the patch changes (`bsd/kern/kern_exec.c`, `XNU_HOSTED`, run before launchd with 0044 and 0078):

- `oneios_dictation_status()`: `/private/var/mobile/Library/Preferences/com.apple.assistant.support.plist` is written
  when missing as XML with `Offline Dictation Status` = `{}` (mobile 501:501, 0600); an XML file without the key gets it
  after its top `<dict>`; a file that names it is kept; a binary file without it (cfprefsd's) is left alone and said so.
  An empty dictionary means "no offline language installed": the cache becomes an empty set, filled once, posted once.
  New seeds get the same from the root tweaks (`userland/config/prefs.toml` group `dictation`, docs/userland.md 3.1).
  `oneios-dictation-status=0` is the old way. One line: `oneios-dictation: Offline Dictation Status: {} (written,
  mobile 0600) (/private/var/mobile/Library/Preferences/com.apple.assistant.support.plist)`, or `named there already:
  kept`, or `... binary plist without it: left alone (the loop stays)`.
- 0044's default `oneios-enable-jobs` list is now `com.apple.analyticsd,com.apple.navd` (the daemon policy keeps navd
  from now on: `userland/config/daemons.toml`). Its line: `oneios-jobs: com.apple.navd: enabled in disabled.plist (it was
  disabled)`.

What it costs: dictation stays unavailable offline (it was: no language is installed); navd runs (idle unless
something navigates).

Tested (Mac, 2026-11-22): `HOSTED_OUT=kbd kernel/build-hosted.sh` applied the series with 0083 and built `xnu.macho`
in 155 s (sha256 `f1f5cd7f...`; the strings `com.apple.analyticsd,com.apple.navd` and `oneios-dictation: ...` are in
it; no warning in the new code). `XNU_FILESET_REFERENCE=1` does not link since 0074 (`_native_owner_cache_original_root`
undefined in the RELEASE reference; 0082's tree has no `reference.macho` either), so the collection is without it:
`KERNEL=out/kernel/current-hosted-kbd/xnu.macho OUT=out/kernel/kbd-24A446 kernel/prelink/build.sh` gave `kernel.kc`
96,813,056 bytes, sha256 `7593e99e...`, audit 2,712 paired, 0 failures; `prelink stubs-check` 666 good, 0 bad;
`kernel/tests/host.sh` 7 of 7. The root tweaks' tests check the new seed file (mobile 0600, the empty dictionary) and
the policy (navd kept, base 127). Not booted yet. On the phone these lines prove it: `oneios-dictation: Offline
Dictation Status: {} (written, mobile 0600)` (or `named there already: kept` on the next boot) and `oneios-jobs:
com.apple.navd: enabled in disabled.plist (it was disabled)` in `xnuhost.log`; with the keyboard up in Settings or
Spotlight, `oneios-cpu` without Preferences or Spotlight near 100% and without GeneralMapsWidget, geod and launchd at
20-100%; with `oneios-sysfail=Preferences,Spotlight oneios-sysfail-enoent=1`, the `CoreGlyphsPriority.bundle/mic`
probes once per keyboard (12 lines), not hundreds of times; in iOS's log no `timed out with 1 assertions remaining,
disconnecting` for Spotlight, key taps in Spotlight, and Spotlight and Settings hangs under a second or two.

**On the phone (2026-11-22 23:37, this `kernel.kc` `7593e99e...`, the fresh Data store of 21:28, no network, the
clean `xnuhost.conf`): the keyboard works** (the owner: "finally keyboard is working"). `xnuhost.log`:
`oneios-jobs: com.apple.navd: enabled in disabled.plist (it was disabled)` and `oneios-dictation: Offline Dictation
Status: {} (written, mobile 0600)`. iOS's log (`oneios-tool phone pull-log`, `ioslog keyboard`), against the same
store's boot without 0083 (21:28): key taps in Spotlight 30 (0 before); Spotlight hangs 9, 3.8 s in all, the longest
0.50 s (27, 86.4 s, the longest 13.78 s before); `oneios-cpu` Spotlight 72% and 55% while typing, then off the list
(103-107% all the time before). Still in the log, for later: `Keyboard screen not found for window (null)` 21 times at
Spotlight's launch, and SpringBoard 10 hangs (44.4 s in all, the longest 12.36 s).

## 0090-camera-plugin-boot-addon.patch (fix-camera-preview-stream, 2026-11-23)

- **Why.** The capture plug-in lived only in the Data seed (`/private/var/oneios/addons/MediaCapture`, 0056), so a
  rebuilt plug-in reached the phone only with a new seed, which starts the Data store over. The phone boot of 2026-11-23
  ran the old plug-in for that reason. UniXNU already writes a boot addon into the Data volume before launchd
  (`native_gpu_addon_install`: the `kernel.kc.gpuaddon` sidecar, format NGPUADD2, every file under `/private/var/ngpu`
  whose content or mode differs), but 0056 never looked there, and 0031 switched the Metal plug-in's folder to
  `/private/var/ngpu/addons` for any addon at all.
- **What.** (`bsd/kern/kern_exec.c`) The install loop counts the files that are on the volume after it (written or
  already equal) by top folder: `addons/` (the Metal plug-in) and `camera/` (the capture plug-in). 0031's union takes
  `/private/var/ngpu/addons` only when the addon carried Metal files; 0056's takes `/private/var/ngpu/camera/MediaCapture`
  when it carried camera files, else the seed's folders, as before. Nothing of Apple's is written. The plug-in's CDHash
  must be in the image's `overlay/userland/owner-trust.json` (xnuhost's owner trust module), as the seed's must.
- **Tools.** `oneios-tool ios addon OUT --camera out/userland/camera [--metal out/userland/NativeAdrenoMetal.bundle]`
  writes the sidecar and `OUT.cdhashes`; `oneios-tool phone addon OUT` stages it as `image/kernel.kc.gpuaddon` with the
  phone's `owner-trust.json` plus those CDHashes and a manifest for both (`phone addon --remove` takes the sidecar away).
- **Log.** `oneios-camera-addon: boot addon: N files (-1 none), C of the capture plug-in (camera/), M of the Metal
  plug-in (addons/)`, then 0056's line, whose source is `/private/var/ngpu/camera/MediaCapture` with such an addon.
  0031's `oneios-gpu-addon: ... over /System/Library/Extensions` keeps `/private/var/oneios/addons/Extensions` for a
  camera-only addon.
- **Off.** Without the sidecar nothing changes. `oneios-camera-plugin=0` and `sheng-gpu-addon=0` as for 0056.

Numbered 0090: 0089 was the last number on main (2026-11-23); the frame-rate helper's 0091 applies after it.
Tested (the build laptop, 2026-11-23): `HOSTED_OUT=cam2 ONEIOS_CRYPTO=original kernel/build-hosted.sh` applied the series
with 0090 (no fuzz, no compiler warning from the kernel sources) and built `xnu.macho` (sha256 `3ebbc5637e4ee3de...`; both
new strings in it). `KERNEL=out/kernel/current-hosted-cam2/xnu.macho OUT=out/kernel/cam2-24A446 kernel/prelink/build.sh`:
`kernel.kc` 96,239,616 bytes, sha256 `c50ca941a445b494...` (`kernel.kc.hashes` `61096d9322aa59d1...`), audit 2,711 paired,
93 on the stub, 0 failures; `prelink stubs-check` 666 good, 0 bad; `kernel/tests/host.sh` 8 of 8. Not booted yet.

## 0088-cmcapture-model-folder.patch (fix-audio-camera-model, 2026-11-23)

The v0.1.16 boot had no sound, no volume and no haptics: VirtualAudio never started (`Precondition failure:
webcamRecordingDSPList && "Need valid DSPConfig for webcam recording"`, `VA Init Status: 1`). The cause was the camera's
model name: with `/product unique-model` "V54AP" (9ae1f0c, so that CMCapture finds `CMCapture.framework/V54/`)
AVFCapture's AVGestalt finds V54 in its table of 41 iPhone models and answers its webcam key true in audiomxd, and
VirtualAudio's RoutingSettings_N71 then builds a webcam subport with no DSP list (input processing is off here).
`docs/hardware.md` 7, "The model name stopped all audio", has the code read in the shared cache.

The loader now gives "OneiOSAP" (xnuhost's `camera-model=`, `docs/booter.md`), in no AVGestalt table; CMCapture's
folder for it is `OneiOS` (the name less "AP"). The patch (`bsd/kern/kern_exec.c`, `oneios_cmcapture_model`, in
`native_gpu_addon_prepare` after 0078's `oneios_audio_tunings`): it reads `/product unique-model` from the device tree,
takes the name less "AP" (letters and digits only), and when `CMCapture.framework/NAME/AVCaptureSession.plist` is not
there it makes `/private/var/oneios/addons/CMCapture` (and its parents) with one link `NAME -> V54` (made when missing;
an entry that is there is left alone) and shows that folder over `/System/Library/PrivateFrameworks/CMCapture.framework`
read-only: `native_gpu_addon_mount`, the union mount of 0031, 0056 and 0078 (scratch 0xa00, as theirs: they are done by
then). Stage 4 proves the sealed `V54/AVCaptureSession.plist` is still seen through the union, stage 5 that
`NAME/AVCaptureSession.plist` is. Nothing of Apple's is copied.
- A name whose folder is sealed (`camera-model=V54AP`: `V54`): `... is there already ...: nothing mounted`.
- No `unique-model` (no camera listed): `no /product unique-model (no camera): nothing to do`.
- `oneios-cmcapture-model=0`: off; `=NAME`: the link points at `CMCapture.framework/NAME` instead of `V54`.
- One line: `oneios-cmcapture-model: /private/var/oneios/addons/CMCapture over
  /System/Library/PrivateFrameworks/CMCapture.framework: OneiOS -> V54 (link made 0), stage 5, status 0 (stage 5 status
  0: mounted, V54's tables visible as OneiOS's)`.

Numbered 0088: 0087 was the last number on main and on every open branch (`git branch -r`, 2026-11-23).
Tested (the build laptop, 2026-11-23): `HOSTED_OUT=cammodel ONEIOS_CRYPTO=original HOSTED_SERIES=<this series>
kernel/build-hosted.sh` applied the series with 0088 (no fuzz, no warning in `kern_exec.c`) and built `xnu.macho` in 636 s
(sha256 `84eb830f9608db41...`; the `oneios-cmcapture-model:` strings are in it). `KERNEL=out/kernel/current-hosted-cammodel/xnu.macho
OUT=out/kernel/cammodel-24A446 kernel/prelink/build.sh` gave `kernel.kc` 96,272,384 bytes, sha256 `71d7db1449994d58...`
(`kernel.kc.hashes` `22a4e86ed7bdbbae...`), audit 2,711 paired, 93 on the stub, 0 failures; `prelink stubs-check` 666 good,
0 bad; `kernel/tests/host.sh` 8 of 8. Not booted yet. The phone check: `docs/hardware.md` 7, "The model name stopped all
audio".

## 0087-bluetoothd-hci-transport.patch (fix-ios-bluetooth-power, 2026-11-23)

The owner's request: Bluetooth must work in iOS sessions (and AirDrop needs it first). In every session bluetoothd
logged `Bluetooth is running inside a Virtual Machine, so skipping chip initialization` and `PowerState:3`, and never
opened our transport, though xnuhost had the phone's HAL 1.1 and the guest's driver was on
(`out/session-logs/2026-11-22/s8-ioslog`, `s9b-ioslog`, `kbd-debug/xnuhost-s9.log`).

Cause, in 24A446's `/usr/sbin/bluetoothd` (docs/hardware.md 4, "Bluetooth powers on", has the addresses): in a VM its
power init passes only with transport number 10, and the same number picks the HCI transport, where 10 is BTVC, a
simulated controller in its own process. But the HCI transport choice takes `BT_HCI_TRANSPORT=TRANSPORT:DRIVER` from
the environment first and then does not read the number. Its launchd job is on the sealed root.

The patch (`bsd/kern/kern_exec.c`, `exec_extract_strings`): `char oneios_bluetoothd_env[64]`, filled by the Bluetooth
driver (`drivers/bluetooth/oneios_bt.c`, `hci_env`: `BT_HCI_TRANSPORT=H4:POSIX` when the host serves a controller;
the driver also sets the nub's `transport-encoding` to 10 and answers vendor commands as BTVC does). After the
environment of an exec is copied in, when the saved path is `/usr/sbin/bluetoothd` and the environment has no
`BT_HCI_TRANSPORT=`, the string is added (one more string and pointer slot, counted against `NCARGS` as the others).
Lines: `oneios-bt: bluetoothd gets BT_HCI_TRANSPORT=H4:POSIX (docs/hardware.md 4)` per exec, or `oneios-bt:
bluetoothd's environment names it already, kept: ...`. Off: Bluetooth off, `oneios_bt=0` or `oneios-bt-hci=0` leave the
string empty, and nothing is added. Cost: one `strcmp` per exec.

Renumbered 0087 and made again on 0086 (2026-11-23): the same code; it now applies with no fuzz after 0084-0086.
Tested (Mac, 2026-11-23): `HOSTED_OUT=bt kernel/build-hosted.sh` applied the series with it (then numbered 0084, on a main without 0084-0086) and built `xnu.macho` in
153 s (25,642,560 bytes, sha256 `be81a8a2...`; the `oneios-bt:` and `ONEIOS BT: bluetoothd: transport-encoding ...`
strings are in it; no warning in the new lines). `KERNEL=out/kernel/current-hosted-bt/xnu.macho
OUT=out/kernel/bt-24A446 kernel/prelink/build.sh` gave `kernel.kc` 96,862,208 bytes, sha256 `68b6317f2bc96f56...`,
audit 2,712 paired, 0 failures; `prelink stubs-check` 666 good, 0 bad; `kernel/tests/host.sh` 8 of 8;
`drivers/tests/host.sh` with the new `bluetooth/test_bt_env.c`. Not booted yet. The phone check: docs/hardware.md 4,
"Bluetooth powers on".

## 0082-dyld-rw-data-const.patch (fix-dyld-cache-unnesting, 2026-11-22)

The owner's report: app labels show late on every boot, and apps open and close slowly. In every session log each
process, from launchd, fsck and mount on, printed `triggered unnest of range 0x1fa600000->0x1fc600000 of DYLD shared
region` (280-505 a session; xpcproxy 102 times in one). 0080's `oneios-srprot:` lines (launchd's first 24) show what
dyld does, and `oneios-top: shared region:` showed 1,278-16,835 vm_protect calls and 133-1,916 unnests per 30 s, 0 of
them no-ops (session `out/session-logs/2026-11-22/kbd-debug`, runtime 0.1.15).

What was found (dyld-27062 of 24A446, `/usr/lib/dyld` copied out of `os.img`; the cache copied out of
`systemos.img`; `otool`, `ipsw dyld info/dump/a2s`):

- **Who calls.** launchd's dyld is at `0x1008e7000`. pc `0x1008f13ac` is `__kernelrpc_mach_vm_protect_trap`+8, lr
  `0x10093a388` is in `_vm_protect`, frame `0x10093a20c` is the region block of
  `ProcessConfig::DyldCache::makeDataConstWritable`, `0x1008e8b54` is `DyldSharedCache::forEachRegion`, `0x10093a2c0`
  is `makeDataConstWritable`, `0x1008e8998` is `forEachCache`. The small ranges (frames `0x10096c120`...) are
  `lsl::MemoryManager::writeProtect`: dyld's protected memory, made writable around its work because this host has no
  TPRO.
- **The ranges.** The slide of that boot is `0x1a688000`. The six prot 0x13 ranges (`0x1fa768000-0x1fc594000` and five
  more) are exactly the cache's six `CONST_DATA` mappings (three `__DATA_CONST`, three large `__AUTH_CONST`: about
  367 MiB); the three small ones are its three `__TPRO_CONST` mappings (0x24000, 0x4000, 0x4000).
  `makeDataConstWritable` takes every `CONST_DATA` region and skips `CONST_TPRO_DATA`.
- **Why.** `prepare` calls `Loader::adjustFunctionVariantsInDyldCache`. The cache (header `0x208`) has 7 function-variant
  patch sites, all in `__TPRO_CONST`, all of kind 1 (per process) and flag bit 2 = apple[] `has_sec_transition` (MTE):
  five in QuartzCore (`__builtin_available.mte_enabled.ifunc.27` or `__builtin_available.yes`), one in libdispatch
  (`_dispatch_tagging_enabled` or `_tagging_enabled_mte_enabled`), one in AVConference (`__builtin_available.mte_enabled.ifunc`
  or `.yes`). They are 0 in the file, so dyld writes
  all 7 in every process. Before the first write it calls `DyldCacheDataConstLazyScopedWriter::makeWriteable`, which runs
  `makeDataConstWritable(true)` unless `enableReadOnlyDataConst` is off or apple[] has `dyld_hw_tpro_pagers`; the
  scope's end runs it again with READ. Apple's kernelcache of 24A446 has the strings `dyld_hw_tpro=1` and
  `dyld_hw_tpro_pagers=1` (TPRO hardware); ours cannot pass them. So every process here made 367 MiB of the nested
  shared region `VM_PROT_COPY|READ|WRITE`, which unnests it (0080's no-op shortcut excludes `VM_PROT_COPY`: its count
  stayed 0), and then faulted those pages into its own tables again. Not the cause: no root and no interposing (the
  root tweaks write only the Data volume; the GPU plug-in is not in fsck or mount), and the ObjC optimizations are on
  (0080).
- **The switch.** `ProcessConfig::Process` sets `enableReadOnlyDataConst = !(commpage dyld flags & 0x200000)`
  (`_COMM_PAGE_DYLD_FLAGS`, `0xfffffc160`). With it off, launchd's dyld maps the `CONST_DATA` mappings of the shared
  region with init protection READ|WRITE (copy-on-write, as `__DATA`; without `VM_PROT_TPRO`, which
  `shared_region_tpro_protect` ignores here anyway), and `makeWriteable` returns at once. dyld keeps the bit when
  launchd writes `kern.dyld_flags` (`setupDyldCommPage` clears only bits 0-2, 6 and 35).

What the patch changes (`bsd/kern/kern_sysctl.c`, all under `XNU_HOSTED`):

- `hosted_dyld_flags_init` (`STARTUP(LOCKDOWN)`: after `vm_commpage_init`, before `bsd_init` starts launchd) writes the
  commpage's dyld flags with bit 0x200000 and prints `oneios-dyld-flags: commpage dyld flags 0x200000 (...)`.
- `sysctl_dyld_flags` prints `oneios-dyld-flags: launchd set kern.dyld_flags 0x...` at launchd's write (once per launchd
  start) and adds the bit again if that write lacks it (`, 0x200000 added again`).
- `oneios-dyld-rw-data-const=0`: not done; the commpage line says so (the old way, for a comparison).

What it costs: `__DATA_CONST` of the shared cache is writable copy-on-write in every process, as it was before iOS
made it read-only; a stray write changes only that process's copy of one page. TPRO, which protects it on an iPhone, is
not on this host in any case. dyld's `__TPRO_CONST` protects (12 calls per process start, 3 small ranges) stay.

Tested (Mac, 2026-11-22): `HOSTED_OUT=dyldrw kernel/build-hosted.sh` applied the series with 0082 and built
`xnu.macho` in 366 s (`_hosted_dyld_flags_init` with its `LOCKDOWN` startup entry and the `oneios-dyld-rw-data-const`
tunable are in it); `kernel/prelink/build.sh` gave `out/kernel/dyldrw-24A446/kernel.kc` (96,813,056 bytes, sha256
`17729490...`; audit 2,712 paired, 0 failures; `prelink stubs-check` 666 good, 0 bad); `kernel/tests/host.sh` 7 of 7.
Both copies of dyld (`/usr/lib/dyld` and the cache's at `0x180482000`) have the same `makeWriteable` bytes. Not booted
yet (no PC; the phone test is the next step). On the phone, these lines prove it: `oneios-dyld-flags: commpage dyld flags 0x200000` and `oneios-dyld-flags:
launchd set kern.dyld_flags 0x...` with bit 0x200000 set and no `, 0x200000 added again`; no `triggered unnest of range ... of DYLD shared
region` line at all (the lowest writable mapping is now `__DATA_CONST`'s start, and nothing below it is unnested);
`oneios-srprot:` lines only for the three `__TPRO_CONST` ranges (`0x24000` and `0x4000` long), none with prot 0x13
over a range of MiB; `oneios-top: shared region:` with about half the vm_protect calls per process start and 6 fewer
unnests per start; and fewer `vm_fault` and fills per 30 s in `oneios-perf` while apps start.

## 0081-irq-host-calls-rpc-stats.patch (fix-mach-rpc, 2026-11-15)

The keyboard lagged everywhere (phone session 8644ae9-b: Preferences at 102-104 % while typing in Settings' search, its main
thread sampled in `_xpc_connection_send_message_with_reply_sync`; `oneios-perf` mach traps 37 us each). A synchronous
XPC message is one `mach_msg2` trap that sends and receives the reply: the server's thread is woken with `thread_go` on an
idle processor by an IPI, and the reply wakes the client's vCPU the same way (`docs/booter.md` 2.6d).

- **The IPI is a host call.** `cpu_signal_internal` (`osfmk/arm/cpu_common.c`) calls `hosted_ipi_send(cpu_phys_id)` first:
  xnuhost's `ipi` host call, SGI 0 (enabled by ArmCpu on every core; ArmCpu takes SGI 0 as an IPI too). Deferred IPIs the
  same (IOCPU sends those at once anyway). Before, `PE_cpu_signal` -> `ArmCpu::signalCPU` -> `spiIpiSend`: a store to
  GICD_ISPENDR, a trapped store (SIGSEGV, `xh_mmio_emulate`, the distributor's recompute of every CPU's line).
- **The timer and IPIs are taken by host call.** `sleh_irq` (`osfmk/arm64/sleh.c`) calls `hosted_irq_dispatch` instead of
  the registered handler: `irq_take` (xnuhost's new host call; `hostcall.h`, appended to the vector) acknowledges the
  highest pending interrupt only when it is the timer (INTID 27: `rtclock_intr(0)`, as ArmCpu calls it), an SGI or an IPI
  SPI (`cpu_signal_handler`, the handler `ml_processor_register` gives ArmCpu), and `irq_eoi` ends it; when it is a
  device's, it comes back with `XH_INTID_UNTAKEN`, stays pending, and ArmCpu's `dispatch` takes it (and anything after it)
  with its trapped `ICC_IAR1_EL1`/`ICC_EOIR1_EL1` as before. Before, every interrupt went through ArmCpu: three trapped
  system registers (IAR, EOIR, IAR again for 1023) and, for an IPI SPI, a trapped GICD_ICPENDR store.
- `oneios-fast-irq=0` (boot argument): ArmCpu's way for everything, as before; `oneios-fast-irq=2`: the two paths in turn,
  one `oneios-perf` period each (the `0081:` line says which: an A/B test in one boot, under the same load; any mix is safe,
  ArmCpu takes SGI 0 as an IPI and `irq_take` takes ArmCpu's IPI SPIs). An xnuhost without `irq_take` (vector
  size) gets ArmCpu's way too.
- **The lasting lines** of each `oneios-perf:` period:
  - `rpc:` Mach RPCs (a `mach_msg2_trap` -47 or `mach_msg_trap` -31/-32 with `MACH_SEND_MSG | MACH_RCV_MSG` in x1): the
    number, the round trip from the trap to the thread's next return to user mode (the reply, an error, a timeout; the
    server's own work included) as mean and maximum, and four bands (under 100 us, to 1 ms, to 10 ms, over), per second.
    A thread's stamp is `machine.hosted_rpc_at` (the `reserved5` word of `struct machine_thread` under `XNU_HOSTED`: the
    layout Apple's kexts see is unchanged), cleared with the twin.
  - `wake:` threads dispatched after an IPI to their CPU (`thread_dispatch`'s `latency`, `osfmk/kern/sched_prim.c`): made
    runnable -> IPI sent (the waker's scheduler) -> the interrupt taken on this CPU (the host: the futex wake or signal of
    the vCPU's thread, its way out of `cpu_idle`) -> running (the idle exit, the switch); the other dispatches' mean wait.
  - `0081:` interrupts by host call (timer, IPI) and by ArmCpu's trapped registers, with the mean time of a dispatch on
    each path (the handler's own work included); IPIs sent each way.
  - `syscalls:` 0041's bsd and mach calls split into those that came back to their own thread (the call's own kernel
    work) and those that blocked (to the vCPU's next user entry or idle: the switch and the next thread's way back).

Tested: built with the series (`HOSTED_OUT=rpc`), prelinked (audit and stubs-check below), and booted on the PC: see
`docs/booter.md` 2.6d for the bench and the boots. Not run on the phone. On the phone: the four lines in a session's
`xnuhost.log`, `oneios-fast-irq=1` in the `0081:` line, `by ArmCpu's trapped registers` near zero, the `rpc:` mean and
maximum while typing, and xnuhost's `emulated: insn 0xd538cc..` (ICC_IAR1_EL1) and `0xd518cc..` (ICC_EOIR1_EL1) counts at
the stop or on SIGQUIT (millions before, devices' only after).
## 0080-shared-region-protect.patch (fix-dyld-objc, 2026-11-15)

The keyboard lagged everywhere (phone session 8644ae9-b: `oneios-cpu: Preferences[177] 102-104%, backboardd 34-44%,
kbd[159] 12-16%`; `oneios-perf` per 30 s: 295,255 faults, 216,049 of them to vm_fault, 295,247 user maps and 281,109
unmaps: map churn), and every process logged `triggered unnest of range ... of DYLD shared region` once, from launchd
on. What was found, from the source and the PC's logs:

- **The unnest.** The range is always the 32 MiB block that holds the start of `dyld_shared_cache_arm64e.25.dylddata`'s
  `__DATA_CONST` (with the boot's slide: `0x1fd200000->0x1ff200000` at slide `0x1d130000`; that block holds the
  `__DATA_CONST` of libobjc, dyld itself and the other first images). XNU logs an unnest only below the lowest writable
  mapping, so the read-only `__DATA_CONST` is the one that shows; `__TPRO_CONST` and the `__AUTH_CONST` above it unnest
  without a line. It comes from `vm_map_protect`: its exclusive range lock (`vm_range_lock_pmap_unnest_and_clip`,
  `vm/vm_map_lock.c`) unnests the block and copies the constant submap's entries into the task's map before anything
  looks at whether the protection changes. dyld 24A446 runs from the cache (`dyld MachO: 0x180482000`) and has two
  shared-cache protect paths: `ProcessConfig::DyldCache::makeDataConstWritable` (`marking shared cache range ...`) and
  `lsl::MemoryManager`'s protected memory (`__TPRO_CONST,__allocator`), which on a device whose kernel does not pass
  `dyld_hw_tpro=1` (TPRO: per-thread write permission in hardware, which this host does not have) is made writable and
  read-only again with `mach_vm_protect` around dyld's work. `mach_vm_protect` (-14) is the PC's top Mach trap while
  processes start (7,375 per 30 s at 151 us each, about 90 per process launch).
- **The map churn of those protects (ours).** `pmap_protect` flushed its whole range whatever changed, and here a flush
  is an unmap of the task's host mappings (`hosted_pmap.h`): every page of the range is faulted in again at its next
  use. `vm_map_protect` asks the pmap for the new protection minus write when it adds write, which changes no entry; so
  each time dyld made its memory writable the whole range was unmapped for nothing, then each write went to vm_fault and
  each read was filled again.
- **The other slow paths of the phone's samples are iOS's own.** `objc::ObjectHashTable::forEachObject` under
  `_dyld_for_each_objc_protocol` is the lookup in the cache's preoptimized protocol table (libobjc calls it only when
  its shared-cache optimizations are on, so they are on); `DyldSharedCache::forEachImageTextSegment` under
  `findImageMappedAt` is dyld's search of the cache's text segments for an address in a `r-x` mapping (the cache
  header is valid and the cache is where dyld expects it); `lsl::Vector<dyld_image_info>::insert` is an image added to
  the image list by a dlopen. Nothing of ours makes them slower than on an iPhone, except that each dyld API call that
  makes its memory writable costs protects (above).

What the patch changes:

- **pmap_protect flushes only what changed** (`arm/pmap/pmap.c` `pmap_protect_options_internal`,
  `hosted_protect_changes` in `hosted_pmap.h`): the span of the entries whose host mapping changed (the page or
  `hosted_pte_prot`); entries that are the same keep their host mappings. `oneios-protect-flush-all=1` flushes the
  whole range as before.
- **A protect of the nested shared region that changes nothing leaves it nested** (`vm/vm_map.c`
  `vm_map_protect_hosted_noop`): not the maximum, no `VM_PROT_COPY`, and every entry of the range (in the constant
  submap, read under a shared range lock) already has the asked protection (no JIT or TPRO entry): `KERN_SUCCESS`, no
  exclusive lock, no unnest. `oneios-sr-protect-noop=0` is the old way.
- **`oneios-top=N`** (default 6, 0 off; printed with oneios-perf, each period): `oneios-top: shared region: V vm_protect
  calls, N that changed nothing (left nested), U unnests; pmap_protect entries flushed F, kept mapped K (unchanged)`,
  then the N processes the kernel's trap time went to most: `oneios-top: NAME[PID] cpu C%, T traps U us: vm_fault F (U
  us), fills L, own unmaps M; calls NUMBER COUNT xMEAN_US ... (-N: Mach traps); vm_protect P (shared region S, unnests
  X, last prot 0x.. from pc 0x.. lr 0x..)`. `cpu` is the process's user and system time since the report before
  (TASK_POWER_INFO; -1 the first time it is listed), which the PC otherwise lacks (`oneios-cpu` comes from the GPU
  archive, with 3D only). **`oneios-srprot:`** lines: the first 24 vm_protects of the shared region in a boot, with the
  saved pc and lr and four return addresses (`oneios-tool sym` names them).

The first build (`HOSTED_OUT=dyldobjc`, 744 s; prelink audit 0 failures, `stubs-check` 666 good, 0 bad;
`kernel/tests/host.sh` 5 of 5) stopped 1 s into its first PC boot (`oneios-protect-flush-all=1
oneios-sr-protect-noop=0`, qemu-aarch64, 8 vCPUs, 2560 MiB), right after `VM bootstrap: 30 maps, 128 entries and 16
nodes available`, xnuhost exit 4.

**Continuation, 2026-11-16: the early halt is fixed and the phone boots through the former `newfs_apfs` panic.**
`vm_shared_region_create` now sets `hosted_shared_region_ready` only after it has made a region; both the vm_map shortcut
and `hosted_top_note_protect` return before that. This covers the reporting call too, which still ran in the original
halt's `oneios-sr-protect-noop=0` boot. Mac build `HOSTED_OUT=phonefix`; `kernel.kc` `f171b71e...`, prelink audit 2,712
pairs and 0 failures. On the S24 Ultra (`ram=auto`, 6,912 MiB, `shmem_enabled=advise`): VM bootstrap continued,
`newfs_apfs` finished instead of SIGSEGV, the seed verified (6,030 files, 14,816 folders), and xnuhost reported the
first frame with content and `session/ready`. Log: `out/session-logs/2026-11-16/phonefix-xnuhost.log`. No PC A/B run yet.
The report showed **0 shared-region no-ops** and continuing unnests, so the no-op half is not proven effective; the
pmap report did show entries kept mapped (48,376 unchanged in one 30 s period). The typing comparison is still open.
**2026-11-22:** the no-op count stays 0 because every shared-region protect of dyld carries `VM_PROT_COPY`, which the
shortcut excludes. The big ones were dyld's `makeDataConstWritable` over all of `__DATA_CONST`; 0082 removes them.

## 0076-compressor-jetsam-report.patch (fix-ios-compressor, 2026-11-15)

Three more lines in 0058's `oneios-mem:` report (every 30 s), from counters XNU keeps anyway, and one debugging tool, off by
default:

- `oneios-mem: compressor: holds P MiB of pages in C MiB of RAM (D MiB of compressed data): ratio R; room for L MiB of
  pages, low on space no|yes`: `c_segment_pages_compressed` against `VM_PAGE_COMPRESSOR_COUNT` (the compression ratio
  as the RAM sees it, segment overhead in), `compressor_bytes_used`, `c_segment_pages_compressed_limit` (the pool times
  `kern.compressor_pool_multiplier`, `vm_compressor_set_size`) and `vm_compressor_low_on_space()`.
- `oneios-mem: jetsam: available A MiB; at critical, idle, soft, reaper ... MiB; task limit N MB, entitled E MB;
  hw.memsize M MiB, memsize_physical P MiB` and `oneios-mem: jetsam: kills since boot K (idle-exit I, CAUSE N, ...)`:
  `memorystatus_available_pages` against `memstat_critical/idle/soft/reaper_threshold`, `max_task_footprint_mb` and
  `memorystatus_entitled_max_task_footprint_mb` (the `/defaults` limits the loader gives: xnuhost's `ram-view`),
  `max_mem` and `max_mem_actual`, and `memorystatus_kill_counts` summed over the bands, named as memorystatus names them
  (`hosted_memstat_report` in `kern_memorystatus.c`, called from `hosted_fr_log`).
- `oneios-compress-sweep=SECONDS` (boot argument; 0, off, by default): once, at the first report after that many seconds
  of uptime, every anonymous page goes to the compressor (`vm_pageout_anonymous_pages`, what memorystatus_control's
  compressor sweep and the freezer use); the pages fault back in as they are used. Lines `oneios-mem: compressor sweep
  (oneios-compress-sweep=N): A MiB anonymous, ... to the compressor` and `oneios-mem: compressor sweep: 0; ...`, then the
  ratio in the next reports. For a boot that never runs short of file pages (every PC boot so far), to see the
  compressor's path work here end to end (the pmap's disconnect, the compressor threads, the fault back) and its ratio on
  the guest's own memory.

Why: the phone's session of main 3233fed showed `compressor 0 MiB holding 0 MiB` and 486 jetsam lines, and nothing said
which kills they were, what the thresholds were or what the compressor would hold. What the code says about the
compressor and jetsam, and the mode the owner asked for: `docs/booter.md` 2.2, "iOS's own compressor and the memory view".

## 0049-wifi-association-event.patch (radio2, 2026-11-12)

`kev_post_msg` (bsd/kern/kern_event.c) refuses an event that does not fit one mbuf without a cluster (`MLEN`), with
`EMSGSIZE`. iOS 27's wifid reads a 540-byte record from the association event (ASSOC_DONE: ten 52-byte access point
records, the join's result at 0x214 and a flag that it is valid at 0x218; an empty event is not safe on 24A446).
The hosted Wi-Fi driver (`net/`) posts its events as kernel events, because it has no IO80211Family user client,
the pipe these events take on Apple's hardware. On the phone on 2026-11-12 the supplicant joined an open network in
3 s, the driver posted SSID_CHANGED, BSSID_CHANGED and LINK_CHANGED (32 bytes) and got `EMSGSIZE` (40) for
ASSOC_DONE: wifid never learned of the join and Settings stayed at "connecting". With the patch an event of
`KEV_VENDOR_APPLE` and `KEV_IEEE80211_CLASS` may be as long as an mbuf cluster (`MCLBYTES`) and is then built in one
(`m_getcl`); every other event keeps the limit. It is UniXNU-sheng's `sheng-wifi-association-event.patch`, under
`#if XNU_HOSTED`. Built (`HOSTED_OUT=radio`); not yet run.

## net/: the Wi-Fi, en0 on the phone's radio (radio, radio2, 2026-11-12)

No numbered patch: an archive (`oneios-tool kernel gen net`, hooked into `kernel/build-hosted.sh` like `hid/`) whose
start, `oneios_net_start` (`net/OneiOSWifi.cpp`), `oneios_drivers_start` calls before the audio's (weak in
`drivers/bridge/OneiOSBridge.c`). Before it the hosted kernel had no network driver and iOS no en0. It holds
`IO80211Controller` and `IO80211Interface` (the class names wifid and configd know) as subclasses of the
collection's **original** `IOEthernetController` and `IOEthernetInterface` (Apple's IONetworkingFamily stays in the
collection: 25 of its 130 roots link it), a station whose scans and joins are messages to xnuhost's Wi-Fi bridge
(`drivers/net/oneios_wifi_proto.h`, in-band on the virtio-net queues), and UniXNU-droidvm's virtio-net transport
(`DroidVMVirtioNet.c`, staged: multicast passes its receive filter). The classes are compiled against
IONetworkingFamily-188's headers, which the build checks against `net/IONetworking-24A446-v54.json` (250 and 221
vtable slots, every discriminator); 159 original functions are bound through `.original` stubs, none of them
non-virtual. `net/README.md` has the files, the check, the boot argument (`oneios-wifi=0`) and the log lines;
`docs/hardware.md` section 3 the design, the host's side and what ran. Built on the Mac (`HOSTED_OUT=radio`) and
prelinked: the import audit passes (0 failures) and `oneios-tool prelink stubs-check` finds 666 good stubs, 0 bad.

## display/: colour, depth and the panel (fix-display-colour, 2026-11-13)

No numbered patch: the display archive (`oneios-tool kernel gen display`) only. What iOS's render server renders
into here (8-bit sRGB `BGRA`, or `RGhA` half floats narrowed to 8 bits by the CPU; a real iPhone's 10-bit `w40a` is
refused by this display), what QuartzCore and MobileGestalt read about the panel, and the real iPhone18,2 tree's EDR
and nits values: `display/README.md`, "Colour, depth and the panel". In the archive: with `oneios-display-wide=1`
(xnuhost's `display-format=rgba16f`) half-float frames go to the virtio scanout as they are (LegacyDisplayProbe.cpp's
offer and DroidVMVirtioGpuDisplay.cpp's resources carry the scanout format: `staged/display.rs` `DISPLAY`, `SCANOUT`);
`OneiOSDisplay.cpp` logs `oneios-display: half-float frames: ...`, the display service's properties once
(`oneios-display: properties: ...`) and each surface format the compositor swaps (`oneios-display: the compositor
swaps format 'BGRA' ...: taken`). Without the boot argument the frames take the path they took before. Not compiled
yet: the kernel build, prelink and a PC boot are the next step (`display/README.md`).

## 0069-display-power-request.patch (fix-brightness-touch, 2026-11-14)

iOS's display power for the host. backboardd turns its display off and on with
`IOMobileFramebufferRequestPowerChange(fb, state)`, which is `IOConnectCallScalarMethod(selector 12, { state })` (24A446
IOMobileFramebuffer `kern_RequestPowerChange`; QuartzCore's `CA::IOMobileFramebuffer::set_power_state`). This family
answers it `kIOReturnUnsupported` without a platform subclass, so the request reached nothing. In
`iokit/Kernel/IOUserClient.cpp` (`is_io_connect_method`, beside 0029's trace), after the method and whatever it
answered, a call on `IOMobileFramebufferUserClient` with selector 12 and one scalar goes to
`oneios_display_power_request` (weak; `drivers/brightness/OneiOSBacklight.cpp`), which logs `ONEIOS BACKLIGHT: display
off: backboardd[N] asked its framebuffer (power change 0, answered 0x...)` and sends `OB_BRIGHT_POWER`: the host sleeps
the touch IC and floors the panel at off, brings both back at on (`runtime/bridges/brightness/README.md`). The method's
answer and arguments are not changed. `oneios-display-power-hook=0`: not passed. Checked: the patched file passes the
kernel's recorded compile command with `-fsyntax-only`; built on the Linux PC (2026-11-14, `HOSTED_OUT=fixbr`, series through
0069, xnu.macho 122efae5, 974 s); prelinked: audit 0 failures (2711 paired), `stubs-check` 666 good, 0 bad (kernel.kc
7632e390). Not booted; the
PC cannot press a phone's power key into a lit display. Whether backboardd calls it at the lock in this guest is [U] until a phone boot's
log shows the line.

## 0078-audio-tunings-haptics.patch (fix-hapticd, 2026-11-15)

Core Haptics' server for the guest. audiomxd publishes `com.apple.audio.hapticd` (Core Haptics' server: every UI and
keyboard haptic, the Actuator device's only client) and, at its start, loads its synth preset from
`/Library/Audio/Tunings/<hardware model>/Haptics/hapticengineconfig.plist`. The guest's hardware model is the tree's
first `compatible`, `oneios,xnuhost` (`runtime/loader/src/machine.rs`, no identity tree), so the file was not found and
the server gave up (iOS's log of the PC boot of 2026-11-14 with `haptics=sim`, `--proc audiomxd`):
`-[AVHapticServer loadSynthPreset]: ERROR: Unable to load the configuration plist ... NSFilePath=/Library/Audio/Tunings/oneios,xnuhost/Haptics/hapticengineconfig.plist ... No such file or directory`,
`-[AVHapticServer init]: ERROR: Failed to set up SSS client -- server bailing out`, `HapticServerManager.mm:43 ...
ERROR: Haptic Engine & Server Failure`. launchd still holds the service's port, so each client's first call waits for
a reply that never comes (`AVHapticClient.mm:1138 ... Initial XPC call to server timed out. Invalidating connection
to prevent hang`, with haptics on or off: the phone's sessions of 2026-11-15, Preferences 180 and SpringBoard 42 while
typing; Settings at 102-110 % of a CPU), and nothing ever played on the Actuator device (`0 StartIO` in every
`ONEIOS HAPTIC: counters:` line). The OS volume has the iPhone's own folder, `/Library/Audio/Tunings/V54` (only its
`Haptics`: the preset, whose waveforms it names by their absolute `/Library/Audio/Tunings/V54/Haptics/...` paths).

In `bsd/kern/kern_exec.c`, after 0070's step in `native_gpu_addon_prepare` (before launchd): the Data folder
`/private/var/oneios/addons/AudioTunings` (and its parents) is made when missing, gets one link `oneios,xnuhost ->
V54` when it has no entry of that name, and is shown read-only over `/Library/Audio/Tunings` with 0031's union
(`native_gpu_addon_mount`, bindfs, `MNT_RDONLY | MNT_UNION`; scratch 0xa00, as 0056's mount). The sealed folders stay
visible below, and the link resolves to the sealed V54 folder: nothing of Apple's is copied or changed, and an
existing Data store gets it at its next boot. `oneios-audio-tunings=0`: nothing made or mounted (the A/B switch);
`oneios-audio-tunings=NAME`: the link points at `NAME`. One line:
`oneios-audio-tunings: /private/var/oneios/addons/AudioTunings over /Library/Audio/Tunings: oneios,xnuhost -> V54 (link
made 0), stage 5, status 0 (stage 5 status 0: ...)`; stage 1 status N: the folder could not be looked up, stage 2: the
mount refused (N), stage 3/4: mounted, but the V54 folder or the preset through the link not found. Since 0086
(2026-11-22) `oneios,xnuhost` is a folder of links (`Haptics` and the Actuator's VAD strip) and the line says so:
"0086" above.

## 0084-mobilegestalt-cache-identity.patch (enable-camera-app, 2026-11-22)

MobileGestalt answers from the device tree and keeps the answers in its cache
(`/private/var/containers/Shared/SystemGroup/systemgroup.com.apple.mobilegestaltcache/Library/Caches/com.apple.MobileGestalt.plist`),
which it builds again only for another OS build (`CacheVersion` 24A446). The base's `native_gestalt_cache_drop`
(`bsd/kern/kern_exec.c`, in `native_gpu_addon_prepare`) removed it at every boot, so every boot ran its first seconds
without one: `Could not open .../com.apple.MobileGestalt.plist: No such file or directory` from 144 processes in the
first 8 s of the phone's boot of 2026-11-22 17:01 (271 in 21 s at 16:30), until MobileGestaltHelper had made it again.

- xnuhost's loader writes `/chosen oneios-identity` (8 bytes): FNV-1a 64 of what MobileGestalt can read that stays the
  same from boot to boot (the root's properties, `/product`, `/arm-io`, `/buttons`, `/bluetooth`, `/chosen`'s own
  properties and its display mode; `xl_machine_identity`, `docs/booter.md`).
- Before launchd, in place of the drop, `oneios_mg_cache` reads it and compares it with the marker
  `/private/var/oneios/mobilegestalt-identity` (16 hex digits). They differ, or there is no marker: the cache is removed
  (the base's `native_gestalt_cache_drop`) and the marker written, so a device-tree change (the camera nodes,
  `docs/hardware.md` 7) reaches an existing Data store once. The same: the cache stays.
- `oneios-mg-cache=2`: removed at every boot (the old way, the A/B switch); `=0`: never removed; 1 (default): as above.
  No `oneios-identity` in the tree (an older xnuhost): removed at every boot, as before.
- One line: `oneios-mg-cache: identity 1a2b3c4d5e6f7081 (marker none): cache removed (0), marker written (0)`; `...
  (marker the same): cache kept`; `cache removed (2)` when there was no cache.
- The phone's proof: the first boot of an existing store has `(marker none): cache removed (0)`; the second has `cache
  kept` and no `Could not open .../com.apple.MobileGestalt.plist` in iOS's log.

Tested (Mac, 2026-11-22): built in `HOSTED_OUT=cam` from the series (the full build, not `hosted-dev.sh`): `xnu.macho`
sha256 `ad0a2993d910cdf7...`, 167 patches; prelinked (`KERNEL=out/kernel/current-hosted-cam/xnu.macho
OUT=out/kernel/cam-24A446 kernel/prelink/build.sh`): `kernel.kc` sha256 `3c66f79ee04fbcbf...`, audit 2712 paired, 92 on
the stub, 0 failures; `prelink stubs-check`: 666 good, 0 bad. The loader's hash in `runtime/loader/tests/loader.rs`.
Not run on the phone yet. Rebased on 0083 (2026-11-23): the camera helper made it on a tree without 0083, so it did not
apply on main (`patch failed: bsd/kern/kern_exec.c:9710`); now its function comes after 0083's `oneios_dictation_status`,
the same code, and 0085 and 0086 apply after it.

## 0092-enable-jobs-policy.patch (enable-jobs, 2026-11-23)

**Why.** The daemon policy (`userland/config/daemons.toml`) disabled vphone's lists: the groups base (126 jobs),
extra (4) and signin (2), and fairplayd.H2. On the phone's Rust-runtime boot of 2026-11-23 "Add New Wallpaper" loaded
forever: PosterBoard's `ATXFaceSuggestionClient` looks up `com.apple.proactive.FaceSuggestions.xpc` (duetexpertd's),
launchd answers `No such process`, PosterBoard logs `_pushFaceGalleryConfigurationUpdate: no gallery received? Erroring
out` (07:38:51 UTC) and never asks again. The same boot's other failed lookups were the same kind: `kvsd` (syncdefaultsd)
50 times, `replicatorservices` 30, `mobilemail.services.xpc` 27, `proactive.infoSuggestion.xpc` 10, `nfcd.hwmanager` 127.
The owner asked for every job but the crash loops. Since then the three groups are off by default, duetexpertd is in
`[keep]` and fairplayd.H2 left the hardware group; a new seed disables only the replace jobs, aned and the loops.

**What.** A Data store made from an older seed still has those 134 jobs in its `disabled.plist`. 0044's
`oneios_jobs_enable` gets a built-in default list (`oneios_jobs_enable_default`: analyticsd and navd as before, then the
134, 3.4 KiB; the buffer is a static 4096 bytes, since the function runs once before launchd's exec). `oneios-enable-jobs=`
still replaces the list (`-` for none). It logs one line per job it enables and one summary line, not one line per label:

```text
oneios-jobs: com.apple.duetexpertd: enabled in disabled.plist (it was disabled)
oneios-jobs: 134 of 136 labels enabled in disabled.plist, 2 not disabled there (0092)
```

The next boot after that shows `0 of 136 ... 136 not disabled there` (118 and 120 since 0093 took the telemetry
jobs out of the list). 0053's disable list runs after it, so the
crash loops stay off. `tools/oneios-tool/crates/oneios-tweaks/tests/roottweak.rs`
(`policy_the_kernels_enable_list_is_what_the_policy_runs_again`) checks that the list is the policy's.

**Watch in its first boot:** fairplayd.H2 (droidvm saw its services hang lockdownd and accountsd on the iPad without the
FairPlayIOKit kext; this kernel does not carry it either), and `oneios-cpu` / `oneios-mem` for the jobs that run again.
A job that loops goes back with `boot-args-add=oneios-disable-jobs=` (0053's list and it) and then into the policy's
loops group.

## 0093-telemetry-jobs-off.patch (telemetry-jobs-off, 2026-11-23)

**Why.** In the session of 2026-11-23 18:24 (kernel 0092, `out/session-logs/2026-11-23-mac-s1824/`) the host ran at
700-735% of 800 and every guest CPU was busy (`oneios-cpu: ... (all 800%)`). After the Wi-Fi join osanalyticshelper
used 55-103% of a CPU for minutes (`oneios-cpu`; `oneios-top: osanalyticshelper[259] cpu 82%, 95384 traps 13753485 us:
vm_fault 70895 (10449945 us)`), after a `brk` trap (`tasks: pid 26474 ... insn 0xd4200020` at 83764.8; which process is
not in that log) and ReportCrashService. 0092 had enabled the analytics and telemetry uploaders of vphone's base list
too. The owner: turn off what a session does not need, analytics and crash-report submission.

**What.** The daemon policy has a new group, `telemetry`, on by default (`userland/config/daemons.toml`: osanalyticshelper,
out of `[keep]`, and 16 analytics, telemetry and diagnostics jobs out of base). For a Data store from an older seed:
0053's default disable list (`oneios_jobs_disable_default`, 0.6 KiB in a static buffer of 1024) is the loops and the
telemetry group, and 0092's enable list loses the 16 (otherwise 0092 enabled them and 0053 disabled them again: two
writes of `disabled.plist` at each boot). ReportCrash stays: it writes a crash's `.ips` itself (24A446's OSAnalytics:
`+[OSALog createForSubmission:metadata:options:error:writing:]` with a writing block does not go through
osanalyticshelper). `oneios-disable-jobs=` still replaces the list. The first boot logs:

```text
oneios-jobs: 118 of 120 labels enabled in disabled.plist, 2 not disabled there (0092)
oneios-jobs: com.apple.osanalytics.osanalyticshelper: disabled in disabled.plist (it was not)
oneios-jobs: com.apple.analyticsagent: disabled in disabled.plist (it was enabled there)
```

`tools/oneios-tool/crates/oneios-tweaks/tests/roottweak.rs` checks both lists against the policy
(`policy_the_kernels_enable_list_is_what_the_policy_runs_again`, `policy_the_kernels_disable_list_is_the_loops_and_the_telemetry`).
Built on the Linux laptop with `ONEIOS_CRYPTO=original` (xnu.macho `22bf1532...`, 471 s) and prelinked (`kernel.kc`
`40eb1263...`: audit 0 failures, stubs-check 666 good, 0 bad), with the Wi-Fi signal change of `net/`; not booted
yet. **Watch in its first boot:** no
osanalyticshelper in `oneios-cpu`; a crash still leaves an `.ips` under `/private/var/mobile/Library/Logs/CrashReporter`.

## 0094-osanalytics-agent-off.patch (osanalytics-agent-off, 2026-11-23)

**Why.** The session of 2026-11-23 20:23 ran 0093 (`kernel.kc 40eb12633c45`, `out/session-logs/2026-11-23-mac-s2023/`):
`oneios-jobs: com.apple.osanalytics.osanalyticshelper: disabled there already`, and still `osanalyticshelper[268]` at
15-49% of a CPU from about 91120 to the return (`oneios-cpu: ... osanalyticshelper[268] 49%`; `oneios-top:
osanalyticshelper[268] ... 55341 traps ... vm_fault 29005`). 24A446 has two jobs that run
`/System/Library/CoreServices/osanalyticshelper` (all 661 plists of `os.img`'s `/System/Library/LaunchDaemons` read with
`oneios-tool image apfs-copy` and `plistutil`): `com.apple.osanalytics.osanalyticshelper` (`server-init`, the Mach
services) and `com.apple.osanalytics.osanalyticshelper.agent` (`agent-init`, `LaunchEvents` `com.apple.notifyd.matching`
`com.apple.trial.NamespaceUpdate.STEEL_ROSE_CHECK_IN_COHORT`: a Trial namespace update, which comes after the network is
up). 0093 disabled only the first.

**What.** 0093's `oneios_jobs_disable_default` gets `com.apple.osanalytics.osanalyticshelper.agent` (0.6 KiB of its
1024), and the daemon policy's telemetry group has it (18 labels; `userland/config/daemons.toml`), so a new seed's
`disabled.plist` has it too. `roottweak.rs` reads 0094's added label with 0093's list and checks them against the policy.
Tested: `cargo test -p oneios-tweaks` on the laptop. **Watch in its first boot:** `oneios-jobs:
com.apple.osanalytics.osanalyticshelper.agent: disabled in disabled.plist (it was not)`, and no osanalyticshelper in
`oneios-cpu` after the Wi-Fi join.

## 0095-thread-call-cpu.patch (thread-call-cpu, 2026-11-23)

**Why.** 0051's `kernel_task` line names threads, and a thread call thread runs any callout of its group. In the session
of 2026-11-23 20:23 (`out/session-logs/2026-11-23-mac-s2023/`) the top of it was `thread call high #2 26553870` us in a
30 s period (88% of a vCPU; `thread call high #1 14734207` in another, `thread call high #2 27749056` in the 18:24
session), with every vCPU busy. The high group runs every `thread_call_allocate()`d callout: Mach timers (`mk_timer`),
BSD `timeout()`, IOKit power management, the kexts' own calls. Nothing named the one that used the CPU.

**What.** `thread_call_thread` (`osfmk/kern/thread_call.c`) takes the thread's CPU time (`recount_current_thread_time_mach`,
read with interrupts masked: `hosted_tc_cpu_now`) and the wall time around each callout and adds them to the callout
function's slot (256, open addressing on the unslid address, lock-free counters; `table full` in the line when a function found no slot). The oneios-perf report prints,
after 0051's line:

```text
oneios-perf:   thread calls: 91234 callouts, 26553870 us of CPU (88% of a vCPU), 30 functions; most: 0xfffffff007a1b2c4 81234 x 310 us (wall 320, max 9000) ...
oneios-perf:   thread calls, more: 0xfffffff00ac32420 21 x 124 us (wall 145, max 882) ...
```

Each line stays under 240 characters (the console copies a printf through a 256-byte buffer); the functions that do not
fit go on `thread calls, more:` lines, 2 or 3 lines in all. The 8 functions with the most CPU in the period: the count, the mean CPU, the mean wall (above the CPU: the callout
blocks), the longest callout's CPU. Name an address with `oneios-tool sym kernel --kc kernel.kc --kernel xnu.macho
--slide 0 ADDR` (the address is unslid). Cost: two clock reads, two masks of interrupts (a store to the hosted CPU's
word each, no host call) and four atomic adds per callout. **Watch in its first boot:** the line every 30 s; the
function behind `thread call high #2`, then a fix of that cause.

**The panic of its first version (fixed 2026-11-23, fix/tc-recount-ints).** The first 0095 read the CPU time right
after `enable_ints_and_unlock()`, and `recount_current_thread_time_mach()` asserts interrupts off (recount.c:753: an
interrupt between its read of the thread's total and of the CPU's last snapshot makes the sum wrong). The phone,
2026-11-23 22:16 (kernel.kc `72a3e56f`, the integration build, takeover mode), 7 s after xnuhost's start, on cpu0:
`panic(cpu 0 caller 0xfffffff00b4e2bf8): recount.c:753 Assertion failed: ml_get_interrupts_enabled() == FALSE`;
`oneios-tool sym kernel --return` on its frames: `recount_current_thread_time_mach+0x194 (recount.c:753)` called from
`thread_call_thread+0x210 (thread_call.c:1921)`, 0095's first read. The same kernel under qemu-aarch64 (docs/building.md
section 5) panicked the same way 1.8 s into the boot. The fix: `hosted_tc_cpu_now()` masks, reads and restores, as
0066's `hosted_fr_cpu_now()` does. `kernel/tests/host.sh` now refuses an added line of any hosted patch that calls one
of recount's current-thread readers (`_time_mach`, `_times`, `_usage`, `_perf_level_usage`, `_energy_nj`) without a
mask in the 4 lines before it or `interrupts off` written on the line; it catches the first 0095.

**The second fault, behind the first.** With the read masked, the same qemu boot stopped 1.6 s in: `PAC failure from
kernel with IA key while authing x16` at `thread_call_thread+0x288` (`hosted_tc_count`, inlined). The first 0095 took
the function's address with `ptrauth_strip(func, ptrauth_key_function_pointer)` on the typed `thread_call_func_t`:
clang strips it (`xpaci`), then turns the result into an integer by authenticating it with the type's discriminator
(`autia x16` with `0x73d8`), and a stripped pointer never authenticates. The fix: `ptrauth_strip(ptrauth_nop_cast(void *,
func), ...)`, the raw bits then a strip; one `xpaci` and no authentication (the host clang with the kernel's PAC flags
shows both code sequences). `kernel/tests/host.sh` also refuses an added `ptrauth_strip(..., ptrauth_key_function_pointer)`
whose argument is neither `(void *)` nor `ptrauth_nop_cast(`; it catches the first 0095 too.

**The cut line.** With both fixed, the qemu boot ran 25 minutes without a panic, and 25 of its 49 `thread calls:` lines
ended at character 255 with no newline, the next `oneios-perf:   rpc:` line run into them. The console copies a
printf through `SERIAL_CONS_BUF_SIZE` (256 bytes, osfmk/console/serial_protos.h), and the first 0095 printed up to
900. Now the summary and the functions go out on lines under 240 characters (`thread calls, more:` for the rest).

Tested (2026-11-23, the Linux PC): `kernel/tests/host.sh` (10 passed; both new checks fail on the first 0095);
`kernel/build-hosted.sh` with `HOSTED_OUT=tcrec ONEIOS_CRYPTO=original` (`releasable`) and `oneios-tool prelink
build` (`stubs: 666 good, 0 bad`; `audit: f4aa58e63c554784: 2711 paired, 93 functions on the stub, 9 data unpaired; 0
failures`). Under qemu-aarch64 (docs/building.md section 5's command, a new Data volume, 8 vCPUs, 4 GiB, the runtime
of the integration build): kernel.kc `72a3e56f` panics at recount.c:753 1.8 s in; `39bbcfb1` (the mask only) at the
PAC failure 1.6 s in; `863495f2` (both) runs 25 minutes with no panic, its lines cut; `f4aa58e6` (all three) runs 25
minutes with no panic: `hello, launchd` at +186 s, SpringBoard at +217 s, 49 `thread calls:` lines and 94 `thread
calls, more:` lines, none over 254 characters. The busiest callouts there, named with `oneios-tool sym kernel --slide
0`: `workq_add_new_threads_call` (top in 36 of 49 periods), `IOTimerEventSource::timeoutAndRelease`,
`delayed_event_delivery`, `filt_timerexpire`; at most 2% of a vCPU. Logs: `out/session-logs/2026-11-23-pc-tcrec/`.
Not tested: a phone boot (the takeover mode, the high group's 88% of a vCPU). Seen and not fixed: 0051's `kernel_task:`
line is cut at 255 characters too when its threads' names are long, and the next line (0095's) then runs onto it.

## 0096-telemetry-jobs-switch.patch (integration-switches, 2026-11-23)

**Why.** The owner (2026-11-23): every change of a test boot has a switch, so a fault can be pinned by turning one
off (`docs/switches.md`). 0093 and 0094 change which jobs run; their only way back was a full `oneios-disable-jobs=`
list, and that leaves a store that 0093 changed with the telemetry jobs still disabled (0092's enable list no longer has
them).

**What.** Boot-arg `oneios-telemetry-jobs=1` (in the image's `xnuhost.conf`: `boot-args-add=oneios-telemetry-jobs=1`):
`oneios_jobs_disable` starts from 0053's three loops instead of 0093's list, and `oneios_jobs_enable` (0092) gets
the telemetry labels after its own (0093's list less the loops, read from it: one list to keep). One line:
`oneios-jobs: oneios-telemetry-jobs=1: the telemetry group is enabled (0093 and 0094 off)`. Default 0: 0093 and 0094 as
they are. An explicit `oneios-disable-jobs=` still replaces the disable list. The enable list with the telemetry labels
is 3.5 KiB of its 4 KiB buffer; a cut list says so (`the enable list is cut at 4096 bytes (0096)`).

Tested: `git apply --check` after 0095 on the patched tree; built and prelinked with the series (docs/switches.md has
the build). Not booted with `oneios-telemetry-jobs=1`.

## 0097-wired-owners-pressure-order.patch (fix/wired-names, 2026-11-23)

**Why.** The session of 2026-11-23 22:19-22:30 (integration build 74c56ef, kernel `b5f5378a`;
`out/session-logs/2026-11-23-mac-int/live2.log`) had `oneios-mem: free 25 MiB ... wired 3195` of 5302 MiB, the pageout
scan at 7-17 s of CPU in each 30 s, and no line naming who held the wired pages. Read from that log: the wired count
follows the GPU's blob totals, line for line (`oneios-gpu-memory: ... own 2044 (2044 MiB), wired 629`). At 97907 s
wired was 3133 MiB with 2172 + 370 MiB of blobs, at 98040 s 3195 with 2044 + 629. Every blob is wired guest RAM: the
gpu archive's own pages are `prepare()`d in the client's task until the blob goes, and an imported range is wired the
same way. So about 2.0-2.7 GiB of the 3.2 GiB was GPU memory of every client (backboardd 917+300, WebKit's GPU process
475+267, the widget renderer 179, MapKit 115, iconservicesagent 104, SpringBoard 93, cameracaptured 88 MiB), not
backboardd's 1.2 GiB alone. About 0.5 GiB was the rest. The same log has `memorystatus available` at 816-875 MiB, just
above the warning line (795 MiB), and `long-idle-exit 8`: the level reached warning (the long-idle kills come only
there) and fell back.

At a rise XNU notifies one process at a time, 250 ms apart, the least important band first, the largest first within a
band. Apple's privileged listeners go before all (`vm_pressure_select_optimal_candidate_to_notify`). The processes that
hold the GPU memory are the most important ones, so they were last. A round over 150-300 listeners takes 40-75 s. A level
that falls back before the round reaches them (the long-idle kills free a little) ends it there. Core Animation's pooled
textures, metal2vulkan's kept and spare blocks (the base's 0019-0031 patches, `release_spare`) and NativeMetal's held
imports (`watchImportPressure`) all wait for that note (docs/gpu.md, "Who keeps a blob").

**What.**
- `oneios-wired` lines every `oneios-wired-log` seconds (60; 0: never), from the free-page thread's `oneios-mem` report
  (`vm_resident.c`). The numbers are XNU's own accounting by site, what `zprint` reads (`vm_page_diagnose`), plus the
  gpu archive's blob totals (`droidvm_virtgpu_live`, weak):

  ```text
  oneios-wired: 3195 MiB: kernelcache 96, boot 223 (stolen 40, booter 12), zones 180 of a 512 MiB map; compressor 269 apart; GPU blobs: own 2044, imported 629 (counted with their owners too)
  oneios-wired: owners (MiB): IOKIT 2690, ZONE 180, RESERVED 150, kext com.apple.iokit.IOSurface 120, ..., rest 40, unnamed 12
  oneios-wired: zones in use (MiB): VM.objects 40, ...
  ```

  (The numbers in the sample are made up; the format is the build's.) "owners" are the 10 largest sites that add to the
  wired count: a tag (`VM_KERN_MEMORY_*` without the prefix), a kext by its identifier, or a kernel site by its unslid
  address (`oneios-tool sym kernel` names it). The hosted drivers are kernel code, so their wired pages (the GPU's blob
  pages, the display's, the disks') come under `IOKIT`, and the GPU's own totals in the first line say how much of
  `IOKIT` is the GPU's. "unnamed" is the wired count less every site's. "compressor ... apart": the compressor's pages
  are not in XNU's wired count. Cost: one `vm_page_diagnose` a minute (a few hundred KiB buffer, freed again, and one
  pass over the zones).
- An `oneios-pressure` pair after them (`kern_memorystatus_notify.c`, `hosted_pressure_report`): the level, the
  `memorystatus_level`, the available pages against the warning, normal-again (115%) and critical lines. Then, since the
  last pair, the rises and falls, the notes sent by level and how many the rule below picked, the longest round (from
  the first note at a level to the last listener that wanted it), and the newest four notes (name[pid], level W/C/N,
  footprint, how long ago).
- **The fix, `oneios-pressure-big=MB`** (512; `0` is XNU's order, the behaviour before 0097; one boot-arg,
  `boot-args-add=oneios-pressure-big=0` in the image's `xnuhost.conf`, docs/switches.md). At a rise, a process whose
  footprint is at least that many MiB and that was not yet notified at that level is picked first, the largest first.
  So the render server and the apps that draw hear of the warning in the first 250 ms steps of the round, not after
  150-300 smaller processes. Privileged listeners still go first, each process is still notified once per level, and
  nothing else in the order, the delays or the levels changes. Whether a GPU client's footprint counts its blob pages
  (they are purgeable memory the client owns) is not checked yet: the last-notes column shows the footprints.

Tested: `git apply --check` after 0096 on the patched tree; built release-style (`ONEIOS_CRYPTO=original`) and
prelinked (docs/switches.md has the build). First built with the series less 0095 (its `recount.c:753` panic was
fixed apart); merged 2026-11-24 after the fixed 0095 and built with the whole series (`HOSTED_OUT=wired`, 182 patches,
`xnu.macho` `32f88a27`, releasable), prelinked (`kernel.kc` `e79886d9`, audit 0 failures, stubs 666 good). Under
qemu-aarch64 (docs/building.md section 5, a new Data volume, 8 vCPUs, 4 GiB, the integration build's runtime) it ran
the 20 minutes of its timeout with no panic: `hello, launchd` at +209 s, SpringBoard at +247 s, 39 `thread calls:`
lines, 20 `oneios-wired` reports (one a minute; wired 178 MiB at the start, 529 at the end: `ZONE` 136, `PTE` 120,
`RESERVED` 87, `SECURITY` 63, IOSurface 35, `unnamed` 45; GPU blobs 0, qemu has no KGSL) and 20 `oneios-pressure`
pairs (level normal throughout, 2804 MiB available at the end, no rise), each line under 240 characters. Logs:
`out/session-logs/2026-11-24-pc-wiredm/`. Not tested: a pressure rise (so `big first`), a GPU client's blobs in the
counts, a phone boot. **Watch in its first boot:** the `oneios-wired` lines (`IOKIT` near the GPU's own +
imported, `unnamed` small), and in `oneios-pressure` a warning round whose first notes are backboardd and the other
large GPU clients (`big first` above 0), then `oneios-gpu-memory` own bytes falling within seconds of the rise.

## 0098-disable-allowed.patch (fix/disable-allowed, 2026-11-24)

**Why.** In the session of 2026-11-24 01:17 (`out/session-logs/2026-11-24-mac-gpumem/live.log`, kernel `e79886d9` with
0093, 0094 and 0096) the boot logged `oneios-jobs: com.apple.osanalytics.osanalyticshelper: disabled there already`
and `...osanalyticshelper.agent: disabled in disabled.plist`, and still `osanalyticshelper[263]` ran at 31-77% of a CPU
from the first crash on (ReportCrash[259] at 108542.6, the helper right after), crossed its 15 MB limit (`process
osanalyticshelpe [263] crossed memory high watermark (15 MB); EXC_RESOURCE`), was started again as `[441]` and ran at
54-85%. **The cause** is in the sealed root, not in disabled.plist: 24A446's
`/System/Library/LaunchDaemons/com.apple.jetsamproperties.V54.plist` marks 19 jobs `DisableAllowed = false`, and
launchd (`Ignoring disabled state because DisableAllowed=false: %s`, a string of 24A446's `/sbin/launchd`) runs such a
job whatever disabled.plist says. The 19: ReportCrash, ReportCrash.Root, ReportCrashService, osanalyticshelper and its
`.agent`, analyticsd, diagnosticextensionsd, symptomsd-diag, OTATaskingAgent, mobile.softwareupdated, logd, logd_helper,
logd_reporter, mobileactivationd, mobile.obliteration, DumpPanic, sysdiagnose, sysdiagnose_helper, pluginkit.pkd (read
with `oneios-tool image apfs-copy` from `out/image/24A446/os.img` and plistutil). Of them the daemon policy disables
osanalyticshelper, its agent, diagnosticextensionsd, symptomsd-diag (telemetry), OTATaskingAgent and softwareupdated
(replace): all six could run. The image tool knew (`tweaks.rs`, the contract's `DisableAllowed` line); only the `rw`
layout could act on it (vphone's `patch-jetsam-disable-allowed`). So 0094 (the agent label) was not the missing piece.

**What.** Before launchd, in `native_gpu_addon_prepare` before 0031's daemons mount: the sealed plist (binary, 129739
bytes) is read, every key string object `DisableAllowed` (marker `0x5e` and 14 bytes; the plist's writer did not unique
them: 19 copies) becomes `OneiOSDisAllow`, of the same length, so no offset of the plist moves and every other property
(priorities, memory limits) stays. The copy is written to `/private/var/ngpu/daemons` (only when it differs), which
0031 already shows over `/System/Library/LaunchDaemons` as a read-only union (the sealed files stay visible beside it).
launchd then honours disabled.plist for every job; no job is disabled that disabled.plist does not name. Switch
`oneios-disable-allowed` (docs/switches.md): `1` (default, product type V54), `NAME` (another product type's plist), `0`
(nothing written, an earlier boot's copy removed: iOS's rule again). One line, after the mount:

```text
oneios-disable-allowed: com.apple.jetsamproperties.V54.plist: 19 DisableAllowed marks renamed (written 0; -1: the same
    as before), launchd sees 19 renamed through /System/Library/LaunchDaemons (daemons mount stage 4, status 2)
```

Stage 4 status 2 is expected: the union is mounted and shows the sealed `com.apple.backboardd.plist`; 0031's own probe
(`org.blu-sharky.adrenoprobed.plist`) is not there. Not done when `sheng-gpu-addon=0` (0031's function returns first).

Tested: `kernel/tests/host.sh` builds `oneios_disable_allowed_marks_in` from the patch's own lines with
`tests/test_disable_allowed.c` (made-up plists: 3 marks renamed in place, a longer string, the bare bytes and a key at
the end left alone, an XML plist refused). The same rename of the real 24A446 plist (sed on a copy in the scratch
folder): 266 bytes changed (19 x 14), plistutil reads it, its XML is the original's with only the key renamed.
`git apply --check` after 0097 on the patched tree; built and prelinked (see "0100"). **Not booted.** Watch in the first boot: the
line above with 19 and 19; no `osanalyticshelper` in `oneios-cpu`; in iOS's log (`phone pull-log`) no `Ignoring disabled
state because DisableAllowed=false` and `JetsamProperties loaded from path`; a crash still leaves an `.ips`. **Risk:**
if launchd refused a plist from the union, it would run without jetsam properties (no priorities or limits): its
`JetsamProperties not loaded` in iOS's log says so; `oneios-disable-allowed=0` takes the copy away.

## 0100-kill-reason-line.patch (fix/disable-allowed, 2026-11-24)

**Why.** In the same session PosterBoard's wallpaper extensions were started again every 7-22 s with no touch in
between (MercuryPosterExtension 409, 417, 420, 429, 436 between 108666 and 108726; Collections, Aegir, Rhizome, Weather,
Unity2025 likewise), at 50-206% of a CPU each. xnuhost logged no trap of theirs, `oneios-mem` no jetsam kill and no
`crossed memory high watermark` of theirs: nothing said who ended them or why.

**What.** A lasting line (no switch: logging) when a process ends another with a reason (`terminate_with_payload`, what
`terminate_with_reason` calls: runningboardd, FrontBoard, launchd's extension watchdog) or sends another single process
SIGKILL with kill(2), at most 600 lines a boot:

```text
oneios-kill: KILLER[pid] -> TARGET[pid]: namespace N code 0xC flags 0xF: "the caller's text, its first 80 bytes"
oneios-kill: KILLER[pid] -> TARGET[pid]: SIGKILL (kill)
```

The namespace and code are XNU's `os_reason` ones (`sys/reason.h`: 15 RunningBoard, 10 SpringBoard, 7 libxpc, 20
watchdog); the text is copied in only for the line. A process ending itself is not counted. In `kern_sig.c`, no other
patch's file. Numbered 0100: 0099 is left to the audio fix (fix/audio-late).

Tested: `git apply --check` after 0098 on the patched tree. Built with 0098 on the laptop (`HOSTED_OUT=disallow
ONEIOS_CRYPTO=original`, 184 patches, 367 s): `xnu.macho` `7ecd6622`, releasable; prelinked to
`out/kernel/24A446-disallow/kernel.kc` `a07f5426` (`.hashes` `80688378`), audit 0 failures, stubs-check 666 good 0 bad;
the lines of 0098 and 0100 are in it (`strings`). **Not booted** (no phone boot yet; no qemu boot: the owner's rule of
2026-11-24). Watch in the first boot: `oneios-kill` lines naming the posters' killer and reason (FrontBoard's scene
watchdog, RunningBoard's, or launchd's), which decides the next fix.

Merged with fix/audio-late (`oneios-audio-rt`, the audio archive) and built again from the merged tree (`HOSTED_OUT=disallow
ONEIOS_CRYPTO=original`, 184 patches, releasable): `xnu.macho` `0f51c0e3`; `out/kernel/24A446-merged/kernel.kc` `af1321f7`
(`.hashes` `ef107ec1`), audit 0 failures, stubs-check 666 good 0 bad; `kernel/tests/host.sh` 11 of 11. Not booted.

## 0101-unmap-fanout.patch (fix/unmap-fanout, 2026-11-24)

**Why.** The phone, 2026-11-24 01:17-01:22 (kernel.kc `e79886d9`, xnuhost `35f74348`, takeover mode;
`out/session-logs/2026-11-24-mac-gpumem/live.log`), ran at 620-724% host CPU (sys 243-387%) at 94-96 C. Per 30 s:
`pmap: user maps 843359-970500, unmaps 11139390-12352829`, and in the 0045 line `unmaps: 492823-983077 for a task's
own flushes, 10316293-11860063 from 37017-42548 shared-region or commpage flushes`. So the unmaps were 12-13 times the
maps, and 90% of them came from about 1,400 shared-region flushes a second, each sent to the 240-290 tasks whose 32 MiB
block bit (0045) was set. Nearly every task has filled a page in libsystem's blocks, and almost none had the flushed
page. Each order took the task's TLB word and queued a `task_unmap` in xnuhost (`task_unmap 11647376 queued`). In the
same periods `VM_pageout_scan` used 14.7-18.1 s of CPU per 30 s and 408,000-550,000 faults went to vm_fault's path
(45-49 s of CPU). What issues them, read from the code: the pageout's reference clear (`arm_force_fast_fault`, an
unmap: the page then waits for its access-flag fault), then at the next touch `arm_fast_fault` -> `arm_clear_fast_fault`,
which sets the bit back on every mapping of the page and flushed each one again (for the shared region, again to every
task of the block), and which ran on the exception path (sleh's `handle_user_abort`, the return, the refill).

**The change, three parts, each behind a boot-arg (docs/switches.md):**

1. `oneios-nested-exact=1` (0: 0045's block bits only). A task that fills a page of the shared region first gets a
   bitmap of that page's 32 MiB block (1 KiB, 8192 bits; `hosted_nested_prepare`, in thread context before the fill's
   locks; a table of 511 pointers per task). The fill sets the page's bit under the task's TLB word, as 0045 set the
   block bit. A shared-region flush sends `task_unmap` only to a task with a bit set in its range and clears those bits
   after the unmap (`hosted_nested_forget`, also on a task's own flushes). A block with no bitmap (the allocation
   failed) keeps 0045's rule. The bitmaps go with the task.
2. `oneios-upgrade-flush=0` (1: the flush as before). An entry changed to the same page with no less access needs no
   host unmap: the host maps no more than the old entry allowed (every change that takes access away flushes), so an
   access the host refuses faults and is filled from the new entry. `arm_clear_fast_fault` (the reference or modified
   fault taken) and `pmap_enter_pte` (the same page entered again with write added or wired) keep the tasks' host
   mappings (`hosted_upgrade_only`). The kernel pmap keeps its flush (it maps its entries again at once).
3. `oneios-fill-fast-fault=1` (0: the exception path as before). When a user fault's fill fails only because the entry
   waits for its reference fault (no access flag) or modified fault (read-only, was writable), the trap path calls
   `arm_fast_fault` itself and fills the page at once (`hosted_user_fill_trap`). On an iPhone this is the cheap
   access-flag fault.

The modified bit stays exact (docs/gpu.md 2.5 item 4): a clear of it still takes write away in the host (an unmap).
The referenced bit stays exact too: its clear still unmaps the page in every task that has it, now only those.

**Lines (lasting).** Each `oneios-perf` period, two lines under 240 characters:

```text
oneios-perf:   0101: flushes/task unmaps: remove F/U, protect F/U, page_protect F/U, refmod F/U
oneios-perf:   0101: upgrade F/U, enter F/U, tables F/U, other F/U; kept mapped K; fast fills N (not M); nested exact=1: B KiB
```

F is the user, shared-region and commpage flushes by cause (`hosted_flush_why`, set at each flush site of pmap.c:
`remove` pmap_remove, `protect` pmap_protect, `page_protect` pmap_page_protect, `refmod` the reference/modified clears,
`upgrade` arm_clear_fast_fault, `enter` pmap_enter replacing an entry, `tables` table frees, nest and unnest), U the
task unmaps they gave. `kept mapped`: entries changed with no unmap (part 2). `fast fills`: faults filled on the fill
path (part 3); `not`: the fast fault ran but the fill still failed (the access needs vm_fault). `B KiB`: the bitmaps and
tables alive (part 1's memory). Cost: two loads and a store per flush, the counts per vCPU.

**Tested.** `git apply --check` after 0100 on the patched tree; built release-style (`HOSTED_OUT=unmap
ONEIOS_CRYPTO=original`) and prelinked (the build is in docs/switches.md "Tests"). `kernel/tests/host.sh`. Two qemu
boots of the first build (then numbered 0098, the same code) ran side by side for about 23 minutes each, with the same
binaries: one with all four switches of this change off (`oneios-nested-exact=0 oneios-upgrade-flush=1
oneios-fill-fast-fault=0`, xnuhost `timer-coalesce=0`), one at the defaults. They were stopped there by the owner's rule
of 2026-11-24 (no qemu boots to check a change), so they are not a full boot test, but both reached launchd and
SpringBoard with no panic. Summed over the same span (logs `out/session-logs/2026-11-24-pc-unmap/`):

| | switches off | defaults |
|---|---|---|
| user maps / unmaps | 2,725,593 / 1,180,885 | 2,745,122 / 598,940 |
| `upgrade` flushes / task unmaps | 492,909 / 492,909 | 0 / 0 (485,134 kept mapped) |
| `page_protect` flushes / task unmaps | 1,129 / 33,141 | 1,182 / 3,947 |
| shared-region flushes / task unmaps (0045 line) | 1,338 / 35,586 | 1,342 / 3,455 |
| faults filled on the fill path / to vm_fault's path | 1,487,701 / 1,235,340 | 1,970,826 / 772,566 (436,286 fast fills, 17 not) |
| xnuhost `timer_set` / `timer_settime` | 4,433,745 / 4,207,380 | 4,455,773 / 2,950,554 (1,271,656 kept, 338,051 re-armed) |
| bitmaps | - | 25,383 KiB at the end |

qemu-user makes few shared-region flushes (the pageout hardly runs in 4 GiB); the phone made 30 times as many, so
part 1 is measured there. Part 1's memory is the cost to watch: 25 MiB of bitmaps under qemu (about 80 blocks per task
[from the totals]); on the phone with 300-400 tasks perhaps 30-50 MiB [guess]. `oneios-nested-exact=0` frees it.
**Not booted on the phone.** Watch in its first boot: the 0045 line's `from N shared-region ... flushes` unmaps near N
times a few, not times 250; `pmap: ... unmaps` near the maps; `0101: ... fast fills` near the old `to vm_fault` count
and `to vm_fault (us)` a fraction of 45-49 s; `refmod` the largest cause left; `nested exact=1: B KiB` against the
wired count.

## 0102-0106: the other Galaxy A56 fork's iOS 16.5 fixes (port/a56-full, 2026-11-24)

Taken from Musicalista/OneiOS-A56 (branch `main` to `c83b9ca`, its last push of 2026-11-22; OneiOS issue 73,
[docs/a56.md](../../../docs/a56.md) "The second port"), renumbered to the next free numbers (fork → here: 0078 → 0102,
0079 → 0103, 0080 → 0104, its change of 0043 → 0105, 0076 → 0106). None of them does anything for iOS 27: 0103-0105
act only on boot arguments that xnuhost adds for an image set of build 20F66 (`IOS16_BOOT_ARGS`,
`runtime/xnuhost/src/config.rs`), 0102 only on one nobody passes, 0106 only when xnuhost asks for a graceful return
(`return-graceful-ms` > 0: the galaxy-a56 host row only). Built into the release kernel
`xnu-12377.121.6-ios-27.0.1-24A446-16bd44f9d737` (branch `release-kernel-a56`: v0.1.22's series plus these five and 0107) and
prelinked with the audit's 0 failures; `git apply` on main's patched tree (to 0101) too. **Not booted** (no A56 here,
no qemu boot); the fork's own phone results are quoted below.

- **0102** `oneios-dq-head=N` (iokit/Kernel/IODataQueue.cpp, `sendDataAvailableNotification`): the first N entries at a
  data queue's head (0x20..0x800 bytes, 0x200 shown), in hex, one line per 32 bytes: `oneios-dq-head: I size 0xS @0xO:
  ...`. IOHIDFamily's event queues to backboardd write their entries without `IOSharedDataQueue::enqueue`; the
  notification goes out when a write made the queue non-empty, so its head is that write. A lasting tool, off without
  the argument. The fork's 0074 and 0075 (an `enqueue` dump that saw nothing of the HID queues, and notification
  counters) are not taken: 0102 is the one that found the cause.
- **0103** `oneios-hid16=1` (the same file): the A56's touch. iOS 27's IOHIDFamily writes a digitizer event as 0xa0
  bytes (x/y/z as doubles at 16/24/32, six words at 40..64, three doubles at 64..88, the orientation type at 88, six
  doubles at 96..144, three words at 144..156) and puts a 28-byte header before every child event; 20F66's IOKit
  (`IOHIDEventCreateWithBytes`, `IOHIDEventGetFloatValueWithOptions`, read by the fork in its shared cache) reads a
  0x68-byte event (x/y/z as IOFixed at 16/20/24, the words at 28..52, three floats at 52..64, the orientation type at
  64, six floats at 68..92, three words at 92..104) back to back after one header. So backboardd counted every touch
  and found no finger. At each "data available" notification the queue's unread elements made only of iOS 27
  digitizer events are rewritten in place (header kept, IOFixed and floats from the doubles' bits without floating
  point, the rest of the entry zeroed); a rewritten element no longer matches, so a second pass leaves it alone. An
  element written while the queue was not empty is rewritten at the next notification if still unread (else read
  unconverted: the fork's limit, kept). Lines: `oneios-hid16: digitizer events rewritten for iOS 16.5 (first: N
  events)`, then `oneios-hid16: N touch elements rewritten for iOS 16.5` every 1000. The fork's A56 (2026-11-21,
  kernel.kc `e831dec7`): touch works, 371 frames in the test session against about 10. `kernel/tests/host.sh` builds
  the patch's own conversion and runs it on an element made like the fork's dumped entry (`tests/test_hid16.c`: x
  0.50002 → 32769, y 0.43588 → 28565, finger, identity 2, mask 0x804, radius 5.0; a second pass, another event type,
  another sender, the edges).
- **0104** `oneios-grant=PROC:ENTITLEMENT[,...]` (iokit/Kernel/IOUserClient.cpp, `copyClientEntitlement`): when AMFI
  has no value for the entitlement and the process name (as `proc_name` gives it) and the entitlement match a listed
  pair, the answer is `kOSBooleanTrue`. 20F66's configd (InterfaceNamer) lacks
  `com.apple.networking.ionetworkstack.user-client`, which iOS 27's `IONetworkStackUserClient::initWithTask` wants, so
  the open failed (`0xe00002c2`), en0 was never named and wifid found no Wi-Fi interface ("Device manager not
  present"). Line once per pair: `oneios-grant: configd[N] given com.apple.networking.ionetworkstack.user-client`. The
  fork's A56 (kernel.kc `00d8b9cc`): the grant line, `ONEIOS WIFI: first naming (en0 attached ...) by configd`, en0 up
  and wifid's Apple80211 requests; no scan or join seen there.
- **0105** `oneios-ls-keep=3` (bsd/kern/kern_exec.c, 0043's code): 0043's test (`=2`, the default) with the
  /Applications bundles that have no `Info.plist` of their own left out. 20F66 has `DisplayCal.app`, a macOS bundle
  (`Contents/Info.plist`) that lsd never registers; counted, no database ever named every app, so every boot dropped
  it and SpringBoard's main thread waited about 66 s for lsd after the first frame (the fork's A56). The fork ran with
  `oneios-ls-keep=1` (keep every one); 3 keeps the drop of a database saved before the registration ended.
- **0106** (bsd/kern/kern_sig.c): `oneios_power_request()` over the bridge's weak default
  (`drivers/bridge/OneiOSBridge.c`): a thread call that sends every `logd` `SIGTERM` (what launchd sends it at a
  power-off), so its buffers reach the Data store before xnuhost's kill; launchd starts a new logd. Not a power-off:
  iOS keeps running to xnuhost's deadline. Line: `oneios-power: return asked: SIGTERM to logd (N process(es))`. The
  fork: iOS's log on the Data store held only the first ~40 s of each boot before it. A later clean power-off replaces
  this definition.

The fork's 0077 (`oneios-log-debug`, Debug logging profiles written before launchd) is not taken: a debugging aid of
its touch hunt, which 0102-0103 closed. Its series ends at 0080; every other patch of it is in this series (docs/a56.md).

## 0107-ios16-port-policy.patch (a56-full, 2026-11-24)

- **The fault** (OneiOS issue 73, the A56 user's logs of 2026-11-24 20:05 and 20:31: runtime 0.1.22/0.1.23, kernel
  `7d540ae9e518`, our 20F66 image): iOS 16.5 reached the lock screen, then SpringBoard died again and again (46 times
  in the second boot, which then hung at the Apple logo), and so did assistantd, coreduetd, contactsd, searchd,
  IMDPersistenceAgent and maild: `ERROR: [NAME:PID] EXC_GUARD AST: type=0x1 flavor=F ...`, then `FATAL exception:
  type=0xc reason=0x17`, then `[NAME: killed] exiting with signal 9`, and "Too many corpses being created" for the
  widgets after them. Not jetsam: "kills since boot 0", 2.3 GiB available. Two flavors only, in both boots (20 + 5
  of 0x42, 4 + 22 of 0x6):
  - `flavor=0x42 target=0x4010 code=0x2000004200004010`: kGUARD_EXC_INVALID_MPO_ENTITLEMENT. 0x4010 is
    `MPO_WEAK_REPLY_PORT` (0x4000, iOS 16's `MPO_PROVISIONAL_REPLY_PORT`) | `MPO_INSERT_SEND_RIGHT`: iOS 16's
    libraries construct provisional reply ports, and iOS 27's `mach_port_construct` wants an entitlement for that
    (osfmk/ipc/mach_port.c's entitlement step, `IPC_SEC_POLICY_DISALLOW_CONSTRUCT_WEAK_REPLY_PORT_WITHOUT_ENTITLEMENT`,
    enforced on iOS by `ipc_sec_policy_should_apply_weak_reply_port`, ipc_policy.c);
  - `flavor=0x6 target=0x1 subcode=0x22` (searchd): kGUARD_EXC_EXCEPTION_BEHAVIOR_ENFORCE, osfmk/kern/ipc_tt.c's
    `set_exception_behavior_violation`, by the tunable `thid_should_crash` (default true).
- **The fix**: with `oneios-ios16-port-policy=1` the weak reply port policy's callback says "not applied" (the
  construct goes on, as on iOS 16's own kernel) and `set_exception_behavior_violation` ignores the violation (as with
  `thid_should_crash=0`, for every task). Its default is the value of `oneios-legacy-registered-ports` (hosted 0068),
  which every 20F66 image's boot arguments carry (roottweak, 2026-11-21), so the A56 user only updates the kernel;
  iOS 27 has neither, and both policies stay as they are there. One line at boot, beside 0068's:
  `oneios-ios16-port-policy: on: weak reply port construct and exception behaviour not enforced (iOS 16 userland)`.
- **The proof on the A56**: that line; no `EXC_GUARD AST` with flavor 0x42 or 0x6 and no `FATAL exception: type=0xc
  reason=0x17` for SpringBoard and the daemons above; SpringBoard stays up past the lock screen. Built into the
  release kernel (docs/releases.md); not booted (no A56 here).

## 0108-a56-gpu-plugin.patch and the GPU archive's A56 accelerator (a56-gpu, 2026-11-24)

The guest kernel's half of the Galaxy A56's GPU Metal ([docs/a56-gpu.md](../../../docs/a56-gpu.md) 5 has the whole of
it: the fault, the rule, the sandbox, the lines).

- **The fault** (read from the code; the A56 has no amdgpu host yet). The start counted the host's 3D features
  alone and then published NativeAdreno's accelerator (`MetalPluginName` `NativeAdrenoMetal`) on any 3D host unless
  `oneios-gpu-metal=0`. On an amdgpu host that publish is wrong: Turnip needs an msm context, and on iOS 16.5 the
  accelerator handed backboardd a Metal device that could not draw. 20F66's `oneios-gpu-metal=0` hid this.
  The kernel itself sends no msm ccmd outside `vgpu-selftest=1`, whose staged form already branches on the context
  type. So there was no panic and no hang.
- **The fix, in the GPU archive** (`gpu/`, built by `oneios-tool kernel gen gpu`, no numbered patch):
  - `OneiOSGpu.cpp` reads the DRM capset's `context_type`.
  - `OneiOSAdreno.cpp` decides with `gpu/OneiOSGpuChoice.h`:
    - Adreno on an MSM capset (or none read), by the old rule;
    - nothing on a capset that is read and is not MSM;
    - the A56's accelerator with `oneios-gpu-amd=1` on an AMDGPU capset, whatever `oneios-gpu-metal` says;
    - never Adreno with `oneios-gpu-amd=1`.
  - The A56's accelerator is the same `AGXAccelerator`, with these differences:
    - `MetalPluginName` `OneiOSRadeonMetal`, `MetalPluginClassName` `OneiOSRadeonMetalDevice`, `OneiOSDrmContext`
      `amdgpu`;
    - only open type 0x4149 and the plug-in's markers;
    - Metal's and GL's discovery keys hidden from the processes of `oneios-gpu-amd-cpu` (by default backboardd,
      SpringBoard and the rest of Jesvi-iOS's "core renderers"), in `copyProperty` and in `serializeProperties`;
    - one line for each of the first 32 DRM contexts.
  - The staged edits are `staged/gpu.rs` `GPU`, `GPU3D` and `ACCELERATOR`.
  - **OpenGL ES** (the GL stage, 2026-11-25; docs/a56-gpu.md 4a and 5.3): with `oneios-gpu-amd-gl=1` (xnuhost adds it
    when the gpuaddon carries `OneiOSRadeonGL.bundle`) the A56's accelerator names `IOGLESBundleName` `OneiOSRadeonGL`
    for every process (`oneios_gpu_gles_bundle`, staged into `DroidVMGpu.cpp`'s `setProperty`; `AppleMetalGLRenderer`
    as before otherwise), and the `oneios-gpu-amd-cpu` processes see it: only the four Metal keys stay hidden
    (`OneiOSGpuChoice.h` `oneios_gpu_cpu_hides`), and `serializeProperties` removes each key by the same rule as
    `copyProperty`. Tested: `test_choice.c` (the bundle name for every kind and value, what the CPU-composition
    processes see), `cargo test --test golden` (15 of 15). Not built into a kernel here: **the A56 release kernel needs a
    rebuild** on top of release-kernel-a56 0888d97 with this archive.
- **0108** (kern_exec.c, kern_descrip.c: 0031's and 0039's code):
  - 0039's list file is named after the published `MetalPluginName` (the archive's `oneios_gpu_metal_plugin`, weak;
    `NativeAdrenoMetal` without it).
  - One `oneios-gpu-addon:` line says whether a plug-in other than NativeAdrenoMetal is visible under
    `/System/Library/Extensions`.
  - 0031's dyld lines name `OneiOSRadeon*` files.
- **The S24 Ultra and iOS 27 are unchanged:**
  - The capset is MSM and no `oneios-gpu-amd` is passed, so the decision is the old rule. `test_choice.c` checks it
    for every gpu state and `oneios-gpu-metal` value.
  - The names are the old literals, the hooks answer false, 0039's file is `NativeAdrenoMetal.clients`, and 0108's
    new line is not printed.
  - The one difference is two read-only capset queries at the start.
- **Tested:** `kernel/tests/host.sh` (the new `gpu: the accelerator's choice ...` case, `gpu/tests/test_choice.c`), `cargo
  test --test golden` in `tools/oneios-tool/crates/oneios-kernel` (15 of 15, with the edit lists on the real pinned
  sources), 0108 applied in order on main's series (190 patches) and on `release-kernel-a56`'s (183), and the hosted
  build below. **Not booted** (no A56 here, no qemu boot: qemu has no 3D).
- **The build** (build laptop, 2026-11-24): `HOSTED_OUT=a56gpu ONEIOS_CRYPTO=original kernel/build-hosted.sh`, 376 s:
  - `xnu-12377.121.6-ios-27.0.1-24A446-b14aa6445129`, `xnu.macho` 24,717,776 bytes, SHA-256 `39fba0f6bee80890267eaaedb7947da746e43cbb90e1330271c765776e86f878`;
  - 192 patches (main's series to 0108), releasable;
  - `kernel gen gpu`: no warning in any GPU archive object's log; `adreno/status.json` compiled and linked;
    `OneiOSAdreno.o` defines the new hooks, and `AGXAccelerator::serializeProperties` is in the archive.
  - The prelink, `KERNEL=out/kernel/current-hosted-a56gpu/xnu.macho OUT=out/kernel/24A446-a56gpu kernel/prelink/build.sh`
    (the 24A446 collection, which the A56's 20F66 image set also carries): `kernel.kc` 96,321,536 bytes, SHA-256
    `f090804df3460214c776a4d065692d5d9b3b494d81c04f7e7cd4b9517ce73693`, audit 2711 paired, 93 on the stub, 0 failures.
  - This is main's series, not the release branch's. It is not a release kernel.
- **The release branch, prepared, not published** (build laptop, 2026-11-24):
  - The branch: local `a56gpu/release-kernel-a56`, pushed to origin under that name. It is `release-kernel-a56`
    (`82fa3b0`) plus this change's kernel part (`b5e706a`: `gpu/`, 0108 and its series entry, the edit lists and
    their tests, `kernel/tests/host.sh`'s gpu case, this section).
  - The build: `HOSTED_OUT=a56gpu-rel ONEIOS_CRYPTO=original kernel/build-hosted.sh`, 442 s.
    `xnu-12377.121.6-ios-27.0.1-24A446-f8def7dad866`, `xnu.macho` 24,657,296 bytes, SHA-256
    `21bc9f9f2337cfd62cea9b66d96d777d7430fc9ec10c22626a4070b133528419`, 184 patches, releasable.
  - Its prelink: `kernel.kc` 96,272,384 bytes, `780ca6ec5efff7ebe1d3c29ae9eb854cbf76b88b0ab3130f2e49e2aefef04668`,
    audit 2711 paired, 93 on the stub, 0 failures.
  - Tests there: `kernel/tests/host.sh` 9 of 9, `cargo test --test golden` 15 of 15.
  - To release it, on the Mac's say:
    1. `git push origin a56gpu/release-kernel-a56:release-kernel-a56`.
    2. `kernel/publish.sh --hosted a56gpu-rel --upload` in that worktree
       (`/home/akshay/Documents/GitHub/OneiOS/out/a56gpu-relwt`).
    3. On main, `kernel/release-kernel.txt` gets
       `kernel-xnu-12377.121.6-ios-27.0.1-24A446-f8def7dad866 <the archive's SHA-256>`, and docs/releases.md its paragraph.
  - Not booted.
  - **Superseded, not published** (2026-11-24, fix/a56-ios16-faults): `release-kernel-a56` is now `0888d97`, this
    kernel part (`2786c3d`, the same change as `b5e706a`) plus 0109, published as
    `xnu-12377.121.6-ios-27.0.1-24A446-181d8bf2d60a` and pinned in `kernel/release-kernel.txt` (docs/releases.md): one
    A56 kernel with both. `f8def7dad866` was never uploaded, and the push above would no longer be a fast-forward.

## 0109-ios16-arm64-slices.patch (fix/a56-ios16-faults, 2026-11-24)

- **The fault** (OneiOS issue 73, the A56 user's boot of 2026-11-24 21:47: runtime 0.1.24, kernel `16bd44f9d737`, our
  20F66 image): webbookmarksd died 27 times in the boot, about every 10 s, `trap kind 2 signal 11 far 0x61f80001c0b0
  esr 0x92000004 ... pc 0x1dfc9056c` (the first one at pc 0x100a3b56c), and it was the only process with
  `shared_region: ... [216(webbookmarksd)] check_np(...) vm_shared_region_start_address() returned 0x1` (two lines,
  as from launchd before the first cache is mapped). 20F66 is iOS 16.5 of the iPhone X, an A11 without pointer
  authentication: its shared cache is `dyld_shared_cache_arm64` (no arm64e cache) and `/usr/lib/dyld` is arm64 only.
  Its App cryptex (`/System/Cryptexes/App`) is shared across iPhones, and `/System/Cryptexes/App/usr/libexec/
  webbookmarksd` is fat: arm64 and arm64e (caps PAC00). An A11's kernel grades arm64e 0 (its host subtype is
  ARM64_V8) and runs the arm64 slice. This kernel is arm64e: `ml_grade_binary` gave the arm64e slice 12 and the arm64
  one 9, so the arm64e slice ran, in a shared region of its own (the region is keyed by the CPU subtype) with no cache
  in it, and the first arm64e chained pointer nobody had resolved faulted (`far` = `x0` = 0x61f80001c0b0, a pointer
  with its fixup bits still in it). The 20F66 cache's slide and the slices were read on the build laptop
  (`oneios-tool sym find|callers`, `file`).
- **The fix**: with `oneios-arm64e-slices=0`, `grade_arm64e_binary` (bsd/dev/arm/kern_machdep.c) answers 0: a fat
  binary runs its arm64 slice, as on the iPhone X, and an arm64e-only one is refused at exec (it could not run on an
  arm64 userland anyway). The default is 0 when `oneios-legacy-registered-ports` (hosted 0068) is on, which every
  20F66 image's boot arguments carry (roottweak's 20F66 `xnuhost.conf`), and 1 otherwise: iOS 27 grades as before
  and the S24 Ultra's binaries (arm64e) run as they do. So the A56 user only updates the kernel. One line at boot:
  `oneios-arm64e-slices: off: fat binaries run their arm64 slice, arm64e-only ones are refused (iOS 16 userland, arm64
  shared cache)`. OneiOS's own guest helpers are arm64e builds for iOS 27; the A56's seed has none (`OneiOS launch
  policy: no /private/var/oneios/bin/oneios-init in this seed`), and the A56's Metal plug-in is arm64 (docs/a56-gpu.md).
- **The proof on the A56**: that line; no `check_np` line for webbookmarksd; no `trap ... far 0x61f80001c0b0` and no
  `Process[N] crashed: webbookmarksd`. Built into the release kernel `xnu-12377.121.6-ios-27.0.1-24A446-181d8bf2d60a` (`release-kernel-a56`
  `0888d97`, with 0108; prelink audit 0 failures; docs/releases.md); not booted (no A56 here, no
  qemu boot).
