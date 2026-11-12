# kernel/patches/oneios-hosted: XNU as a process under xnuhost

Owner: hosted-xnu. The machine layer that lets xnu-12377 run with no EL1 and no MMU, against xnuhost's
host-call ABI (`runtime/xnuhost/include/xnuhost/hostcall.h`). Date: 2026-11-10.

**State, plainly.** The hosted kernel builds on Linux and boots under xnuhost in `qemu-aarch64` through
`arm_init`, the whole VM bootstrap and into the first kernel thread (`kernel_bootstrap_thread`). It stops in
corecrypto's start-up self-test. No user-mode code path has run. Nothing has run on the phone. Details below.

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

## 0034-syscall-failures.patch and 0035-staged-apps-on.patch (lead, 2026-11-11)

Found on the phone: SpringBoard reached the Home Screen with an empty Dock and no icon, the App Library was empty,
and on unlock SpringBoard died in an assertion of FrontBoardServices, `Invalid condition not satisfying:
processIdentity` (`+[FBSSceneClientIdentity identityForProcessIdentity:]` with nil, called from SpringBoard's scene
request for Spotlight while the lock screen's views appeared: named with `scripts/debug/dsc.py` from the `sheng-exc`
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
  after the Wi-Fi driver published it in prelink's session 2); these lines name what it waits for.
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


## audio/: the speaker and the microphone (audio, 2026-11-12)

No numbered patch: an archive (`audio/build-oneios-audio.py`, hooked into `kernel/build-hosted.sh` like `hid/`)
whose start, `oneios_audio_start` (`audio/OneiOSAudio.c`), is the last call of `oneios_drivers_start` (weak in
`drivers/bridge/OneiOSBridge.c`). It holds UniXNU-droidvm's virtio-snd transport (`DroidVMVirtioSnd.c`, unchanged,
`-DDROIDVM_DEVICE_DYNAMIC=1`) and its two IOAudio2Devices (`DroidVMAudio.cpp`, staged with the boot arguments
`oneios-audio-out-uid`, `oneios-audio-in-uid`, `oneios-audio-latency`; its static initializer out of
`__mod_init_func`, as the HID archive's: `droidvm_audio_attach` runs it in its own module load). Before it the
hosted kernel had no audio device. Log lines, boot arguments and what was measured: `docs/hardware.md` section 6.
On the phone since `kernel.kc` `66faa3e8` (main 924a0d9, agent prelink's sessions of 2026-11-12): `ONEIOS AUDIO: on`,
both devices published, audiomxd opens them and plays (`docs/hardware.md` section 6, "In a session").

### The Actuator device (media, 2026-11-12)

The archive also stages a third IOAudio2Device into `DroidVMAudio.cpp`: UID `Actuator` (VirtualAudio's
`VirtualAudioDevice_Actuator`: Core Haptics, UI and keyboard taps), mono S16 at 48 kHz, the speaker's engine, whose
periods go to `drivers/haptics` (`oneios_haptic_actuator_play`, the host's actuator) instead of virtio-snd. Published
only when `drivers/haptics` has the host's actuator (`oneios-actuator=0`: never; `oneios-actuator-channels=2`:
stereo). `drivers/haptics` publishes `/product/haptics closed-loop` only when the device exists. Built
(`HOSTED_OUT=media`); not run. `docs/hardware.md` section 5 has the design, the log lines and the way back.

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
- **The cost to iOS** (test kernel, `make -C runtime/xnuhost bench-arm64`, under qemu on the PC; times about 10x the
  phone's): `ram_discard` of one 16 KiB page 5.2 us, of 1 MiB in one run 90 us (1.4 us a page); zero-filling a 16 KiB
  page that was given back 11.7 us against 6.0 us resident (the host's first touch of each 4 KiB part): +5.7 us per
  16 KiB, about 0.36 ms per MiB used again, paid only for pages given back and reused (never for pages cycling
  through keep). With the order change the boots were faster, not slower (above). So the default is on.
- **Not run on the phone.** Risks: a host with shmem huge pages for the memfd (`shmem_enabled` `always`/`within_size`,
  or a per-size mTHP setting) may split or merely zero a punched range instead of freeing it (this PC: `advise`; the
  phone's: unknown, read `/sys/kernel/mm/transparent_hugepage/shmem_enabled`); a page XNU frees while the GPU's
  renderer still holds its udmabuf stays held by Linux (the import pins it) and the guest's reuse gets a new page (a
  guest bug either way).

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

No numbered patch: an archive (`net/build-oneios-net.py`, hooked into `kernel/build-hosted.sh` like `hid/`) whose
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
prelinked: the import audit passes (0 failures) and `stubs-check.py` finds 666 good stubs, 0 bad.
