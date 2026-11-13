# Paused work (2026-11-12): what is done, what is next, how to resume

The owner paused the work on 2026-11-12 at about 15:55 IST. This file is the hand-over. Read it with
`README.md` "Status", `docs/agents.md` (the board) and the docs it names. Facts marked **proven** ran on the phone
(SM-S928B, KernelSU, One UI); **built** means pushed and host-tested, not run on the phone.

## State at the pause

- `main` is at `2c7bbb5`. The build host is the Mac (Xcode 27.2, the NDK, Apple's `container` for the host tests).
- The phone is in One UI ("One UI restored in place (Enforcing)" at 15:55:27), with root (KernelSU; root is not
  persistent: a reboot needs a manual re-root by the owner). No session runs.
- **Staged on the phone** (`/data/local/tmp/oneios/`): runtime 0.1.12-dev (from `7d6d6a3`), `kernel.kc` `2fdc2d16`
  (from `7d6d6a3`, series through 0056), the image set of `7d6d6a3` (new `data.seed` `f201d6ca` with the Metal
  plug-in `f3b2c620` and the camera plug-in `789bcb69`, their `owner-trust.json`), the app from the fork's
  `f964d6e`, and the session scripts of `f964d6e`. The Data store is the one that boot made fresh (apps registered;
  the LaunchServices database is kept from now on, 0043). `xnuhost.conf` is the baseline one plus
  `oneios-ubt=SpringBoard,backboardd oneios-ubt-at=600 oneios-ubt-every=120`; the baseline file is
  `out/session-logs/2026-11-12/baseline-conf/xnuhost.conf` (sha256 `01679caa...`).
- **Not on the phone yet:** `2c7bbb5` (the display helper fix): the next runtime build has it.
- The phone lock (`out/session-logs/2026-11-12/phone-lock.sh`) is held as `parent-hold`; the scratchpad it lived in
  is temporary: a resumed session makes its own lock or none.

## Proven on the phone (2026-11-12)

| Area | Result | Commits |
|---|---|---|
| Mac build | kernel, `kernel.kc` with the pair audit, runtime, image set, iOS code (Metal, camera), the app | 657e8e6, 365a24f, 89e4172, 1367a18 |
| Boot | no panics; the Apple logo at about +50 s; apps on the Home Screen | 1eed3da |
| Touch | works from the first swipe (Contact Identifier below 30: backboardd's `MAX_PATH_COUNT`); 0.7 ms scan to driver | a6346c1 |
| Way back | the chord (rocker chatter), a return during the switch wins | fork 7839e4b, f964d6e |
| CPU caps | One UI's caps lifted for a session and restored, `cpufreq_limit` too | fork 588b155, 20c6678 |
| GPU | Metal for every process (`*`); render frames 2.1-2.3 ms, 228-284 per 10 s (15:35 boot) | 3e8a668, 0a2e084 |
| Wi-Fi | scan (37-64 networks), join of an open network, the 540-byte join event (0049), DHCP: iOS got 172.20.4.69 | 9240f1a, 04ab894, a034098 |
| Audio | iOS plays through the speaker (owner heard it) | 4947947, abacbe0, 5e426f3 |
| Boot stalls | LaunchServices database kept (52 of 52 apps), analyticsd on, locationd's crash loop gone | 5134651, 292d64a |
| Brightness | CoreBrightness now sends requests (`request 458 from backboardd {brightness-nits} -> level 65535`) | 7d6d6a3 |
| Camera | `AppleH9CamIn` published, the plug-in mounted over `/System/Library/MediaCapture` (the app not tried) | 15467af |
| Haptics | the Actuator audio device and `/product/haptics closed-loop = 1` published (not felt) | 5e426f3 |

## Open faults, in order

1. **Memory pressure stalls the UI** (15:35 boot, fresh store). The guest has 4,070 MiB; GPU blobs hold about
   1.6 GB and backboardd's share grows 760 -> 930 MiB in 30 s; swap does not work ("Failed to open swap file",
   errno 2 and 30); `VM_pageout_scan` takes ~95% of a vCPU; every page it frees fans out unmaps to almost every task
   (1.47 million round trips per 30 s, ~1.5 vCPUs); page fills ~660 us. Fixes, in order:
   - **Deferred unmaps: built and host-tested (2026-11-12), not on the phone.** A task with no running thread gets
     its unmaps queued (up to 256 with the maps) and carried out by the stub before its next run. The WIP
     (`a97687f`) let another thread of the task run while a release or an order was still carrying the queued
     unmaps out; fixed (`task.unmaps_out`), with a two-vCPU check in the test kernel (`docs/booter.md` 2.6). The PC
     benchmark (qemu): the `task_unmap` call 17-18 us -> 1.0-1.2 us; a fault, fill and unmap ~53 -> ~36 us. On the phone:
     read the `perf:` line's `task_unmap N queued, M ordered` and the `VM_pageout_scan` share.
   - **RAM: the owner's decision: iOS gets at least 90% of the phone's RAM. `ram=auto` built and host-tested
     (2026-11-12), the default; not on the phone.** Guest = min(90% of MemTotal, MemTotal - 1.5 GiB) in 256 MiB steps
     (about 9.5 GiB on this phone); `ram-percent=`, `ram-host-floor=`, `host-mem-log=` (30 s), also in the image set's
     `xnuhost.conf`; the choice and the host's MemAvailable in the log (`docs/booter.md` 1). Checked: the window,
     `/chosen dram-size`, `boot_args.memSize`, `xh_info.ram_size` (test kernel); the GPU pool is fixed (8 MiB of
     response buffers; blobs are guest pages). **A large guest boots (the PC, 2026-11-12):** `ram=auto` on the PC
     (MemTotal 15,872 MiB) gave 14,080 MiB; XNU set up its ranges (`zones` 24G, the same at 2 GiB) and booted through
     launchd to SpringBoard in 420 s under qemu with no panic. OOM score unchanged.
   - **Free pages back to the host: hosted 0058, built and run on the PC (2026-11-12), not on the phone.** The goal: iOS
     keeps all of its ram=auto memory; the host gets back only pages iOS has freed (the owner's decision: no pressure
     feedback, no cache cap, XNU's targets, memorystatus and jetsam unchanged). What filled the memfd was not the file
     cache: stock XNU's free queue is FIFO, so the boot walked through every never-touched page and the host kept all of
     them (control boot at `ram=auto`, 14,080 MiB: 12,266 MiB held at 990 s while XNU had 9,048 MiB free; the PC's
     MemAvailable fell under 1 GiB and the run was stopped). 0058 enters freed pages at the free queue's head and
     never-touched or given-back ones at its tail, and gives back free pages above the free target + 32 MiB with
     hysteresis (`ram_discard`, a hole in the memfd). Same boot with it: 4,973 MiB held at 990 s, XNU's view the same
     (free 9,084 MiB; active, inactive, speculative, wired within 15 MiB of the control's), launchd 292 s and SpringBoard
     333 s (control 306 s and 361 s), no panic; `hosted_mapcheck=1` boot: launchd, SpringBoard and the daemons, no panic, no `hosted check:` line, 893 MiB given back with
     `oneios-free-report-stress=1` (4 GiB). Cost: a page given back and
     used again costs the host's refill (+5.7 us per 16 KiB under qemu); on by default, `oneios-free-report=0` turns it
     off (`kernel/patches/oneios-hosted/README.md`, 0058). On the phone: the `memory:` line's `holds` against
     `oneios-mem:`'s in-use figures; if `host MemAvailable` still falls under ~300 MiB, lower `ram-percent` in the
     image's `xnuhost.conf`; check `/sys/kernel/mm/transparent_hugepage/shmem_enabled` (a shmem huge page setting
     may keep punched pages).
   - **Swap: built (the Linux PC, 2026-11-12), not on the phone.** Hosted 0057 makes `/private/var/vm` root:wheel 0755 before
     launchd in every Data store and, on each swap file failure, logs which volume the folder was on and whether it
     was read-only; the roottweak's `data_layout` puts the folder in new seeds. errno 30 is not settled (the sealed
     root before the Data mount, or a Data volume gone read-only: `kernel/patches/oneios-hosted/README.md`, 0057).
     **Settled on the PC:** both are early tries before the Data mount (errno 2: the freezer's budget probe before the
     root is mounted; errno 30: the creation thread's first `swapfile0` on the sealed root's read-only
     `/private/var/vm`); 0057's folder is then there on the Data volume. No swap file was made in 20 min at 2 GiB on
     the PC (no swap-out asked). **Swap serves only frozen apps on iOS (read in xnu-12377 and seen on the PC):** iOS's
     kernel (`CONFIG_FREEZE`) defaults to `VM_PAGER_FREEZER_WITH_SWAP` (`vm_compressor_mode is 32` in the PC's log): the
     compressor is on, `swap_is_active` is off and `freezer_swap_is_active` on, so a general compressed segment is never
     queued for swap-out (`C_SWAPOUT_REG` needs `VM_CONFIG_SWAP_IS_ACTIVE`); only the freezer's segments
     (`C_SWAPOUT_FREEZER`, frozen apps), donated early segments (`C_SWAPOUT_DONATE`) and, with the boot argument
     `vm_compressor_scavenger_enabled=1` (off), old ones (`C_SWAPOUT_RIPE`) go to swap. So swap cannot relieve the
     pageout scan of the 15:35 session; [I] it helps only once iOS's freezer freezes apps (userland decides that; not
     seen on the PC). The device tree's `/defaults kern.vm_compressor` could set another mode (not set in the PC's
     image; the phone's log line `vm_compressor_mode is N` says). On the phone: a `Failed to open swap file` after the
     Data mount, or `swapfile0`. Compiled and prelinked on the Linux PC (`HOSTED_OUT=pc`,
     `kernel.kc` `ffbe95f7...`; `kernel/prelink/README.md`, "A Linux-built kernel against the list").
   - **backboardd's GPU memory growth** (gpu): read, not fixed (2026-11-12). Upstream NativeMetal (at our pin)
     already frees the runtime's empty 64 MiB blocks every 2 s even while backboardd draws (`watchIdleMemory`),
     drops held surface imports and every kept block on a memory-pressure rise and keeps no spare room while it stays
     raised (`watchImportPressure`, `NativeBridgeMetalFrontend.mm`). metal2vulkan keeps up to 2 empty blocks a pool
     and frees a stale one only when a placement of that pool is returned (`memory.rs` `Pool::release`/`stale`), which
     the 2 s timer covers. So the 170 MiB per 30 s is one of: IOSurface imports CoreAnimation still retains
     ("wired"), partly used blocks, or blobs the guest released that the host still holds. **On the phone:** the
     kernel's GPU report's clients line (own vs wired MiB for backboardd) and sizes line ("64+" count) every 10 s
     across the 30 s of growth, and `m2v-mem.txt` in backboardd's temporary folder on the Data volume (copied with the
     logarchive procedure below); then the fix goes where the number is (iOS side: built on the Mac).
2. **The power key does nothing in iOS** since the haptics change (15:35 boot): the presses reach backboardd (34 HID
   events, 0 dropped) but no `request_power_change`. **Read in 24A446 (2026-11-12, `docs/hardware.md` 5, "The power
   key since the Actuator"):** ButtonResolver's AOP interface waits for Haptics.framework's button haptics loader and
   the SPU haptics driver, and libMobileGestalt reads `/product/haptics closed-loop`; [I] with closed-loop = 1 the
   resolver is never ready. Waits for the phone: boot with `oneios-actuator=0`, press the side key (lock, wake), read
   ButtonResolver's `HasHaptic` lines; the fix candidates are written there. Hypothesis (not proven): with closed-loop haptics, backboardd's
   BKButtonHapticsController (its queue waits forever for button "click states") now sits in the button path. Quick
   test: boot with `oneios-actuator=0`. Fix: satisfy the click-state setup, or tell iOS the side buttons have no
   haptics. Not started.
3. **The display helper dropped after 7 frames** (dark panel while iOS drew; restarted by hand). Fixed in `2c7bbb5`
   (xnuhost keeps the socket on transient send errors and logs; the helper logs why its read ended); host tests were
   started, not read; not on the phone.
4. **Brightness:** requests with the key `DisplayBrightness` were ignored ("no brightness in it"). **Handled (built,
   host-tested decoder, the driver compiled into the Linux PC's kernel of 2026-11-12, not on the phone):** 24A446's CoreBrightness sends the slider's position 0..1 (a
   float) or `{Brightness, Commit, Period}`; the driver takes it when unambiguous and logs what came
   (`drivers/brightness/README.md`, "DisplayBrightness"). If the log says `the integer 0 or 1`, the float was lost in
   IOKit's serialization and backboardd's side must send `brightness-nits`. Then check whether the
   slider moves the panel; auto-brightness: CoreBrightness refuses the light sensor (`AppleUSBALS: couldn't find the
   device version`).
5. **Camera:** try the Camera app; the log lines to read are in `drivers/camera/README.md` / `runtime/bridges/camera/README.md`.
   Waits for the phone. Steps: in One UI first `bin/camctl` (`runtime/bridges/camera/README.md`, "On the phone") to
   see the host's cameras; then a session with the image set that has the plug-in (`data.seed` with
   `H9ISP.mediacapture`, `oneios-camera-addon: ... stage 5, status 0` in the kernel's log), open Camera, and read
   cameracaptured's lines in iOS's log (`fcc_probeCameraDriverService` finding AppleH9CamIn, the plug-in's
   `H9ISPCaptureDeviceCreate`, its first frame).
6. **Audio latency** was ~130-140 ms on the host path (AAudio's fast/MMAP paths need a realtime thread, which is
   forbidden). **Built and host-tested 2026-11-13 (fix-audio-latency), not on the phone:** the stage shares and the
   changes are in `docs/hardware.md` section 6, "Latency per stage". The helper's level now follows the server's
   pulls (the burst of 1600 is the mixer's 33 ms period [I, AOSP's sizing rules]), its target comes back down after
   underruns, a backlog's silence is cut, AAudio's buffer is a burst plus the target plus 5 ms; LOW_LATENCY was
   studied (a blocking writer gets no FAST track and no realtime thread: AOSP source). In the host test's harness the
   guest-send-to-server-read time went 73 -> 33 ms (the phone's shape) and 268 -> 22 ms after a 200 ms start stall;
   predicted host path about 98-106 ms (Android's mixer and HAL after the read, 65-73 ms, are unchanged). The
   reported latency is unchanged (6480; 5040 predicted, set by boot argument after the phone measures; the kernel build of
   that default was stopped at the hand-off), LOW_LATENCY is opt-in (`ONEIOS_AUDIO_PERF=low`). On the phone: read the lines that section lists (`the server
   pulls N frames every M ms`, `margin`, `latency about N ms`) and time a key click against a recording.
7. **Bluetooth:** the uart.sk transport (e73c11d) has not been tried in iOS. Waits for the phone. Steps: a session,
   Settings > Bluetooth on, then the expected lines of `docs/hardware.md` 4 ("Expected lines, in order"), from
   `ONEIOS BT: dev: bluetoothd[N] opens /dev/btwake` to `bluetooth: the controller hears devices`; if they stop, the
   last `bluetoothd: command 0x....` line names the command.
8. **Hotel Wi-Fi:** iOS has an address; check that the captive-portal page shows and the internet works.
9. **CLAUDE.md's side-key rule** names event0/event2/event4, but the numbers move between boots (in the 15:35 boot
   event4 was the vibration motor). Proposed wording, by name: never grab `gpio-keys`, `pmic_pwrkey`, `pmic_resin`.
   Waits for the owner.
10. **WebKit content processes crash in a loop; Safari hangs** (2026-11-12 23:45 boot). Each `com.apple.WebKit`
    content process (pids 300-331, ~20 in 20 s) logs, in order, `triggered unnest of range ... of DYLD shared
    region`, then `CODE SIGNING: ... vm_map_entry_reinit_code_signing_after_copy: curprot cannot be write+execute.
    removing execute access`, then crashes; after ~20 corpses iOS says `Too many corpses being created`. MobileSafari
    sits at 70-101 % CPU (spinning on the content process it keeps losing). **Investigated 2026-11-12, not fixed; the
    host side is proven sound, the cause is phone-specific and needs the crash report.** What was ruled in/out:
    - The **CODE SIGNING line is benign** and matches a real device: `vm_map_entry_reinit_code_signing_after_copy`
      strips execute from the **copy** of a `used_for_jit` entry (not the executable one) on every iOS map, because
      `VM_MAP_POLICY_ALLOW_JIT_COPY` is always false on iOS (`VM_MAP_IS_ALIEN` is unconditionally true). It fires
      during WebKit's own `mach_vm_remap` of its JIT mirror and during corpse `vm_map_fork`. It is a marker, not the
      fault.
    - **a919ff3 reveals, it does not cause.** Before it, a task `brk` (a WebKit `RELEASE_ASSERT`/`__builtin_trap`,
      e.g. `brk #0xbb08`, or a PAC `brk #0xc471`) reached iOS as a kernel physical-aperture data abort and panicked
      the whole kernel (the 23:15 tcp_input panic was one such brk). After it, a task's `brk` becomes a clean
      `EXC_BREAKPOINT`, so each content process now dies on its own and respawns — the loop we see. The same CODE
      SIGNING line was on the 23:15 boot, i.e. the JIT path was exercised before too.
    - **SPRR/APRR fast JIT permissions are not used.** WebKit (`JSC::initializeJITPageReservation`) reads a commpage
      byte (`com.apple`'s fast-permissions version) to choose the inline `msr sprr_uperm_el0` path (which would be an
      illegal instruction on the S24's standard Arm cores) versus the separated-W^X-heaps path. Our kernel's commpage
      leaves that byte zero (open-source `commpage.c` never writes it), so WebKit takes separated heaps. The CODE
      SIGNING line (from the remap in that path) confirms it.
    - **The host carries separated-heaps JIT.** New test-kernel case `test_jit` (`runtime/xnuhost/tests/testkernel.c`,
      `make check-arm64`, passed under qemu-aarch64 2026-11-12): a W+X `task_map`, the same physical page mapped
      read-execute at a second address, a thread writing code through the read-write alias and running it through the
      read-execute alias with i-cache maintenance. So the hosted pmap/host-mmap W^X aliasing is not the cause.
    - **Open, phone-only:** (a) whether One UI's kernel (SELinux/KernelSU) permits a W+X mapping of the RAM memfd for
      a task process — qemu-user has no SELinux, so `test_jit` cannot cover it (`docs/booter.md` 7); if it refuses,
      `task.c` logs `tasks: pid N: a deferred map (va 0x..., prot 7) failed`. (b) a WebKit JIT assert under 0027's
      keys-off PAC on a 39-bit user VA (the `jitCagePtr`/arm64e JIT cage, same 39-bit class as the libobjc bug 0027
      fixed). The next boot needs the WebContent crash report to decide:
      `oneios-tool ioslog show data.img --proc com.apple.WebKit.WebContent` and `--proc MobileSafari`, plus
      ReportCrash's `.ips` under `/private/var/mobile/Library/Logs/CrashReporter` for `com.apple.WebKit.WebContent`:
      read the **exception type** and faulting **pc/address**. `EXC_BAD_INSTRUCTION` at an `msr sprr_uperm_el0`
      means the commpage byte is non-zero on the phone (force it to 0); `EXC_BAD_ACCESS` at an address with bits
      39-46 set means the keys-off/39-bit-VA pointer-tagging class; `EXC_BREAKPOINT` at a WebKit `RELEASE_ASSERT`
      names the failing check (file/line are in the `brk` operands). Also read xnuhost.log for the `deferred map ...
      prot 7 failed` line. The `IOUC RootDomainUserClient failed MACF in process pid 282, MobileSafari` line is a
      separate sandbox/entitlement denial for the IOPMrootDomain user client; harmless to JIT, noted for the hang.

## Later

- **Rust tools:** branch `rustport` (sym, log, image done and compared; prelink and ioslog not started);
  `tools/oneios-tool/todo_rust.md` there has the plan, the library rule (use libraries, keep only what is faster or
  has no library: our APFS reader, the hashing, the sym index, the log analyzer) and the APFS decision.
- The board's L1-L3 items (a direct KGSL path after measuring, the Rust ports, `min_runtime` in the image).
- Turnip sub-allocation and batched pipeline-cache writes (a plug-in rebuild with a new trust list).

## How to resume

1. Read this file, then `docs/agents.md`'s last notes. Saved logs (git-ignored): `out/session-logs/2026-11-12/`:
   the xnuhost logs of every session of the day (`combo1`, `diag`, `c2`, `c3`, `c5` = the 15:35 boot), iOS's own
   log of the 14:12 session (`ios3.logarchive`, parsed `ios3-log.csv`), the baseline config and `phone-baseline.txt`.
2. iOS's own log of a session: copy the Data store after the session (read only, no session running), fast:
   `oneios-tool phone pull-data data.img` (gzip -1 on the phone, the zero runs as holes here, the length checked
   against the phone's; `--ioslog` then reads the log out of the copy without a mount). By hand it is
   `adb exec-out "su -c '/data/adb/ksu/bin/busybox gzip -1 -c /data/local/tmp/oneios/data/data.img'" |
   oneios-tool phone sparse-write data.img --gunzip`. `sparse-write` replaced `gunzip -c | python3 -I
   sparsewrite.py data.img` (the script of 2026-11-12, kept as `tools/oneios-tool/tests/phone/sparsewrite.py`:
   6 min, 2.2 GB of blocks for the 32 GiB store): the same bytes, a quarter of the CPU, and fewer blocks on
   Linux's file systems; its macOS part (APFS) and `pull-data` on the phone have not run yet
   (`tools/oneios-tool/README.md` "The sparse copy"). The way of 2026-11-12, by hand on the Mac: attach the copy
   read-only
   (`hdiutil attach -readonly -noverify -nobrowse -imagekey diskimage-class=CRawDiskImage -nomount`), mount the
   Data volume `mount -t apfs -o rdonly,nobrowse,noowners`, copy `db/diagnostics/{Persist,Special,Signpost,
   HighVolume,timesync}` and `db/uuidtext/*` into a `.logarchive`, and parse it with Mandiant's macos-unifiedlogs
   (`unifiedlog_iterator -m log-archive -i ARCHIVE -f csv -o OUT`).
3. Build the set from `main` in a clean worktree (`git worktree add --detach out/parent-wt origin/main`, link
   `external/`): `scripts/bg.sh k env HOSTED_OUT=prelink XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh`, then
   `env REFERENCE=out/kernel/current-hosted-prelink/reference.macho KERNEL=out/kernel/current-hosted-prelink/xnu.macho
   kernel/prelink/build.sh` (the audit must show 0 failures), `runtime/package.sh VERSION`, the Metal and camera
   plug-ins (`userland/metal/build.sh`, `userland/camera/build.sh`, with `out/ios27-tbd` linked), and the image
   (`env INPUTS=<main>/inputs/24A446 OUT=out/image scripts/image/build.sh --no-pack PROFILE`) only when the seed or
   the trust list changes (a new seed starts the phone's Data store over).
4. Stage with `scripts/phone/stage.sh runtime DIR` / `image DIR` (docs/building.md, "Test cycle on the phone"), copy
   the fork's `oneios_switch.sh`/`oneios_return.sh` into the stage when the app is not used, and start a session
   with `busybox setsid sh /data/local/tmp/oneios/bin/oneios_root.sh switch 115 1`. End it with
   `touch /data/local/tmp/oneios/session/return.request`. On the phone read named files only, never a tree walk of
   sysfs or /proc (one panicked the phone's kernel on 2026-11-12).
