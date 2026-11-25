# kernel/: the OneiOS XNU

Owner: release-kernel. The XNU that xnuhost runs: **xnu-12377.121.6, arm64e with PAC, 4 KiB pages, iOS 27.0.1 /
iPhone18,2 profile**, built from Apple's open source with UniXNU's and Droid-VM's patches.

State (2026-11-10): **it compiles and links on this Linux PC in 91 s (clean, 20 cores), with the board's drivers of `drivers/` inside. It has never run.**

## Build

```sh
scripts/bg.sh kernel kernel/build-linux.sh       # Linux, no Mac. Or: kernel/build-macos.sh on a Mac
scripts/bg.sh -s kernel                          # state; log in logs/kernel.log
```

This is the VM kernel. The kernel xnuhost runs is the **hosted** one (`build-hosted.sh`, the same base plus
`patches/oneios-hosted/series`), which builds on Linux and on a Mac with the same command:

```sh
scripts/bg.sh kernel-hosted env HOSTED_OUT=lead ALLOW_XCODE=1 XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh
# -> out/kernel/current-hosted/xnu.macho, xnu.macho.build.json (its build record), series.txt, and with
#    XNU_FILESET_REFERENCE=1 reference.macho (the prelink's reference)
```

The hosted build record (since 2026-11-21) is the record of the `xnu.macho` the build leaves, after `oneios-tool
kernel hosted-fixup` and the seal (`kernel build-record finish --kernel FILE --post-link STEP...`): its SHA-256 and
size are that file's, its patches the board series and the hosted series in order, and it is written next to the
file (no `builds/` entry, no `current` link). `kernel/publish.sh --hosted NAME` packs that folder
([docs/releases.md](../docs/releases.md), "The kernel in a release"). The release kernel is
`HOSTED_OUT=release ONEIOS_CRYPTO=original` ("Releases and licences" below).

`ALLOW_XCODE=1` is for a Mac whose Xcode is not the pin's (27.2); Linux ignores it. Without
`XNU_FILESET_REFERENCE=1` the build skips the RELEASE reference. Mac run of 2026-11-12 (Apple M4, 10 cores,
Xcode 27.2 27B52, macOS 27.2.1): see "macOS" below.

| Output | What |
|---|---|
| `out/kernel/builds/<build id>/xnu.macho` | the native kernel: arm64e `MH_EXECUTE`, PIE, chained fixups, no Apple kext |
| `.../xnu.macho.build.json` | the build record: sources, every patch with its SHA-256, configuration, host, whether it may be released |
| `.../series.txt` | the 94 patches in order |
| `out/kernel/current` | link to the last build |

Last build: `xnu-12377.121.6-ios-27.0.1-24A446-62c8ff7800cd`, 23,735,992 bytes, clang 23.1.4 + ld64-956.6.
The build id is a hash of the inputs (XNU commit, patches, configuration, UniXNU commit, the board archives'
sources), not of the machine. The kernel's bytes are not reproducible: two builds of the same id differ.

Other entry points: `kernel/fetch.sh` (sources at the pins of `pins.sh`, 40 MB), `kernel/series-tree.sh` (the
patched tree with one commit per patch, for writing patches), `kernel/tests/host.sh` (CI), `kernel/publish.sh`.

### What is built from what

1. `external/Droid-VM-UniXNU` (`62d0b11`): UniXNU's build driver, its arm64e series (68 patches) and the Darwin 26
   patches (4). Why this fork and not upstream: `profiles/iphone18-2-ios27.md` section 1.
2. `kernel/unixnu/profiles/os/ios-27.0.1-24A446/`: the OS profile, copied into that checkout at each build.
   `ios-27.2.1-24B156/` is iOS 27.2.1's (the same XNU, series and roots; its IPSW and kernelcache pins):
   `ONEIOS_OS_PROFILE=ios-27.2.1-24B156 kernel/build-hosted.sh` builds the kernel for it. That is a second kernel,
   not the same one: the board archives bind each build's original classes at their addresses
   (`<Class>-<build>-v54.json`, picked by the device copy's `board.toml` `os_profile`; kernel/prelink/README.md
   "Another build").
3. `kernel/device/`: OneiOS as a UniXNU device repository: `board.toml`, `configs/oneios_defconfig`,
   `socs/linuxhost`, and the board series `patches/xnu-12377/series`: 18 of Droid-VM's iPadOS 27 patches, one of
   xnu-linux-toolchain, three of ours.
4. `kernel/patches/unixnu/`: our changes to the driver itself (two: keep the submodules we checked out;
   `0002`, stub the owner chain's `native_owner_boot_manifest_status` in the RELEASE reference, which Xcode's ld
   does not leave undefined).
5. Driver archives linked in: ArmDTPlatform, ArmCpu, libpthread's kernel part, libfirehose, and UniXNU's source
   corecrypto (see "Releases").
6. The board's own archives, built between the two kernel passes by `oneios-tool kernel gen board` (`tools/oneios-tool`, Rust:
   the kernel scripts build it when it is missing or stale and put it on `PATH`; it was `device/scripts/build-oneios-board.py`) and
   force-loaded through `CONFIG_XNU_EXTRA_LD_ARCHIVES`: `liboneiosdrivers.a` (`oneios-tool kernel drivers`, the list `drivers/sources.txt`: the bridge
   transport, haptics, Bluetooth; 9 sources) and `liboneiosboard.a` (Droid-VM's `DroidVMCpufreq.c`, unchanged).
   `oneios_board_late_start()` (`oneios-board-start.patch`), called from `bsd_init` after the storage hook, runs
   `droidvm_cpufreq_attach()` and `oneios_drivers_start()`. The build record lists the ten sources.

The driver stops before the prelink (`--skip-prelink`): no kernelcache is read.

### Linux (`build-linux.sh`)

Host packages: `clang lld llvm git make cmake ninja python3 flex bison m4 autoconf automake pkg-config bubblewrap`,
libdispatch with BlocksRuntime (Arch: `swift` or `libdispatch`; Debian: `libdispatch-dev libblocksruntime-dev`),
`libbsd`, `libuuid`, and Rust (`cargo`, for `tools/oneios-tool`). No root, no SDK, no KDK. `python3` is for other
projects' scripts the build runs as they are (UniXNU's `./unixnu` driver and its scripts, xnu-linux-toolchain's
`iig`, Droid-VM's `build-droidvm-gpu.py`); this repository has no Python of its own since 2026-11-21.

- `linux/setup-toolchain.sh` builds [xnu-linux-toolchain](https://github.com/Allenwdk/xnu-linux-toolchain) (pin
  `909990e`) into `out/toolchain`: cctools-port ld64-956.6, mig, lzfse, unifdef, tcsh and the Python `iig`, all at
  pinned commits (its own bootstrap clones branch heads). A few minutes, once.
- `linux/unixnu-layout.sh` arranges those tools the way UniXNU's Linux kernel step expects its own toolchain
  (`out/toolchain/unixnu`), with the host's clang in place of the clang UniXNU builds from LLVM. Its links name the
  real path of `out/toolchain` (since 2026-11-25: a worktree whose `out/` linked to the main checkout's wrote its own
  path, and every link broke when that worktree was removed), and `build-hosted.sh` makes the layout again when its
  `ld` link is broken.
- `linux/clang-wrapper` is the compiler xnu's makefiles get: the arm64e triple, the ptrauth and destructor flags
  Apple's clang has by default, ARMv8.5 for the assembler, ld64 for links, warnings off. The PAC ABI flags
  (`-fptrauth-intrinsics -fptrauth-calls -fptrauth-returns`, from the A56 fork, 2026-11-21) are explicit: clang 23
  implies them for the arm64e triple (the cc1 line is the same with or without them), an older clang [I] may not;
  `kernel/tests/host.sh` checks that they reach cc1.
- xnu's makefiles call `/usr/bin/xcrun`, `/usr/bin/plutil`, `/usr/bin/codesign` and `/bin/csh`. The build runs inside
  bubblewrap with a temporary overlay on `/usr/bin` that has them; nothing on disk changes.

What differs from a build with Apple's clang (recorded in the build record's `host`):

| Difference | Effect |
|---|---|
| no `__builtin_xnu_type_*` (`portable-kalloc-type-builtins.patch`) | every kalloc type goes to the data heap: weaker isolation, same function |
| `__ptrauth(..., "authenticates-null-values")` is not available (`oneios-stock-clang-ptrauth.patch`) | a null `ucred.cr_label` or IOCircularDataQueue pointer is stored as 0, where Apple's clang stores it signed. A kext built with Apple's clang that loads such a null field fails its authentication. [U] whether the original sandbox or AMFI kext ever sees a null `cr_label` |
| the Python `iig` (`oneios-no-apple-iig.patch`) | no DriverKit RPC: `*_Invoke` answer unsupported, no dext runs |
| warnings off for the kernel's sources | a wrong pointer type in old code stays quiet; errors still stop the build |
| stock clang 23 code generation | the same instruction set (ARMv8.3 + PAC by default); not compared against an Xcode build |

The Mac build is the reference for all five. Nobody has compared the two kernels yet.

### macOS (`build-macos.sh` and `build-hosted.sh`)

Needs macOS 27 with **Xcode 27.2** at `/Applications/Xcode.app` (`XCODE_APP=`; another version only with
`ALLOW_XCODE=1`), its iphoneos SDK, and Homebrew's `cmake ninja unifdef llvm lld` (installed when missing;
`kernel/lib/host.sh` `xcode_setup`, shared by both scripts). No xnu-linux-toolchain, no bubblewrap: Xcode has `xcrun`,
`plutil`, `codesign`, `ld64` and `mig`, and macOS has `/bin/csh`. UniXNU's driver takes its macOS path
(`scripts/unixnu/kernel.py`: Xcode's clang, the iphoneos SDK). Two Mac rules that Linux gets from
`linux/clang-wrapper`'s `-w`: `XNU_BUILD_WERROR=0` for xnu's sources, and `CCC_OVERRIDE_OPTIONS='# +-Wno-error'` for
the board's archive builders, which replay the compile records with `-Werror` (`oneios-tool kernel drivers`): Xcode 27.2 warns
on Apple's own `EXTERNAL_HEADERS/corecrypto/ccchacha20poly1305.h` (`-Wextern-c-compat`). The warnings stay in the
archive logs.

`build-macos.sh` exports `MACOS_VERSION=26.5` (the macOS release of xnu-12377.121.6) and checks it against the OS
profile. `KDK_HEADERS=1` adds a headers-only `KDKROOT` (`oneios-tool kernel kdk-headers`: about 200 headers of KDK 26.5.2 25F84 read
over HTTP, no 1.2 GB download), as iOSPlus10's `scripts/ios26/build-kernel.sh` does; off by default because UniXNU's
series replaces those headers with public ones.

**Tested (2026-11-12, mac-kernel):** `build-hosted.sh` on an Apple M4 Mac (10 cores, 16 GB, macOS 27.2.1, Xcode
27.2 27B52, iphoneos SDK 27.2), the whole hosted series through `0038`, from an empty `out/`; then
`kernel/prelink/build.sh` (`kernel/prelink/README.md`). Times and sizes are in [docs/building.md](../docs/building.md)
2. Found on the way and fixed: `drivers/camera/oneios_camera_dev.c` (an uninitialized buffer that `uiomove` fills:
`-Wuninitialized-const-pointer`), `0038-proc-pid-declared.patch` (the RELEASE reference). `build-macos.sh` (the VM
kernel) shares the Xcode setup but **has not run** on the Mac. Nobody has compared a Mac kernel with a Linux one.
The GNU-only commands of the kernel scripts (`sed -i`, `stat -c`, `nproc`, `sha256sum`) go through `lib/host.sh`;
`kernel/tests/host.sh` checks those helpers on both (Mac, and Linux in an arm64 `python:3.13-slim` container).

## What the booter gets (kernel -> booter)

| | |
|---|---|
| File | `out/kernel/current/xnu.macho` (native), later `kernel.kc` in the image set (the same kernel prelinked with the original kexts) |
| Format | Mach-O 64, `CPU_TYPE_ARM64` / `CPU_SUBTYPE_ARM64E` (caps PAC00), `MH_EXECUTE`, `MH_PIE`, `LC_DYLD_CHAINED_FIXUPS` |
| Link addresses | `__TEXT` `0xfffffff007004000`, `__PRELINK_TEXT` `0xfffffff004004000`; segments 16 KiB aligned; last address `0xfffffff0087c4000` in the last build |
| Entry | `LC_UNIXTHREAD` pc `0xfffffff007545488` in the last build (`_start`, `osfmk/arm64/start.s`); it moves with every change, read it from the file or the record |
| Entry state the kernel expects **today** | as DarwinBoot gives it: EL1, MMU and caches off, at the **physical** address of `_start`, `x0` = physical address of `boot_args`, interrupts masked |
| `boot_args` | revision 2, version 2: `virtBase`, `physBase`, `memSize`, `topOfKernelData`, `Video` (six 64-bit values), `machineType`, `deviceTreeP`, `deviceTreeLength`, `CommandLine[1024]`, `bootFlags`, `memSizeActual` (UniXNU boot chain, "DarwinBoot to XNU": offsets 0x00 to 0x478) |
| Self-relocation | built with chained fixups (`CONFIG_XNU_KERNEL_CHAINED_FIXUPS=y`): the kernel rebases itself for the slide it is loaded at |
| Page size | 4 KiB kernel (`BCM2837` config, `__ARM64_PMAP_SUBPAGE_L1__`), 16 KiB user ABI |
| Device tree | Apple format. ArmDTPlatform needs `/cpus`, `/arm-io`, `/defaults`, `/options` and a CHRP NVRAM proxy in `/chosen`; ArmCpu the GICv3 windows, the timer and `/psci` (`runtime/loader` already builds these from Droid-VM's `make-droidvm-adt.py`). Identity: `kernel/profiles/iphone18-2-ios27.md` section 2 |
| Interrupts, timer, CPUs | ArmCpu: GICv3 (distributor, redistributors), IPIs as SGIs or SPIs (`armcpu-self-ipi=1`, `armcpu-spi-ipi=1`), the virtual timer PPI 27, secondary cores by PSCI `CPU_ON` (`hvc`/`smc`) |
| PAC | `HAS_NATIVE_PAC`: at each CPU's start the kernel writes ten key registers (`AP{IA,IB,DA,DB,GA}Key{Lo,Hi}_EL1`) from one stored key set. A host must ignore these writes and let the kernel and every task use the process's own keys |
| Timebase | `droidvm-timebase-24mhz`: userland sees 24 MHz; the boot argument `droidvm-timebase-hz` gives the target |
| Boot arguments to start from | Droid-VM's `ipados27/board.toml` `kernel_args`, with its `sheng-`/`droidvm-` display values replaced as in `kernel/profiles/iphone18-2-ios27.md` section 4. For the GPU: `vgpu-3d=1 droidvm-gpu-metal=1` |
| CPU topology | `__ARM_AMP__` (`droidvm-amp.patch`): **every `/cpus` node must carry `cluster-type`** (`"E"` or `"P"`), or the kernel panics in `cluster_type_to_pset_type()`. `/arm-io/cpufreq` is DroidVMCpufreq's window (`drivers/cpu/README.md`) |
| Board drivers | ArmDTPlatform, ArmCpu, the bridge transport, haptics, Bluetooth and cpufreq. **No storage, display, input or GPU driver yet.** See "Not done" |
| Linking a board archive | build it in `oneios-tool kernel gen board` (`tools/oneios-tool/crates/oneios-kernel/src/build_oneios_board.rs`) into `{tree}/build/oneios-NAME/` with an `objects.json` (path and SHA-256 of each source), add it to `CONFIG_XNU_EXTRA_LD_ARCHIVES` in the defconfig, and start it from `oneios_board_late_start()`; storage starts from `native_source_storage_start()` (weak), just before |

Privileged instructions, for the trap layer (`llvm-objdump -d` of the first build, `b41b5b1d4a8e`, before the board
archives and AMP): 875 `mrs`/`msr`
of 85 distinct registers, 127 `wfe`, 101 `tlbi`, 85 `dc`, 4 `ic`, 4 `at`, 4 `wfi`, 6 `smc`, 1 `hvc`.

## Not done

1. **The hosted machine layer.** `runtime/xnuhost/include/xnuhost/hostcall.h` describes a kernel entered with
   `x0` = virtual `boot_args`, `x1` = the host vector, `x2` = the vCPU block, no MMU, interrupts as a software flag
   and tasks through `task_map`/`thread_run`. This kernel has none of that: its `start.s`, `locore.s`, pmap,
   `cpu.c`, context switch and copyio are the EL1 ones. It can only run where EL1 is emulated or trapped
   (the plan's M2: a `LINUXHOST` machine configuration beside `BCM2837`, new pmap, kernel linked low).
   The kernel is also linked in the high half (`0xfffffff0...`), which a Linux process cannot map.
2. **The board's drivers.** Droid-VM's source storage (NativeVirtioBlock, RamBlock, the Data store, verified root),
   legacy display, HID input, virtio-gpu and devices archives are not in `board.toml`. They are the ones xnuhost's
   virtio models need (references agent, `docs/agents.md`). Two of their reviewed inputs
   (`legacy-display-24A446.json`, `hid-device-24A446.json`) are for the iPad kernelcache and must be redone for
   `kernelcache.release.v54`; the storage archive needs the owner keys. For the hosted kernel the storage, GPU and
   display archives exist (`patches/oneios-hosted/storage`, `gpu`, `display`; the display's input is redone there,
   `display/legacy-display-24A446-v54.json`); the VM kernel still has none of them.
3. **The prelink** (`kernel.kc`): needs the AMFI address, the class list and the reviewed fileset of the iPhone
   kernelcache (`profiles/iphone18-2-ios27.md` section 3). `kernelcache_sha256` is pinned.
4. A comparison of a Mac kernel with a Linux one (the hosted kernel builds on both since 2026-11-12).
5. Any boot.

## Releases and licences

- XNU is under the Apple Public Source License 2.0; a kernel in a release carries `kernel/NOTICE.kernel` and its
  build record names every modification (APSL 2.2).
- **The VM kernel and the default hosted kernel (`ONEIOS_CRYPTO=source`) may not be released**, and
  `runtime/package.sh` leaves them out: UniXNU's kernel recipe links its source corecrypto (`corecrypto-2023`,
  Apple's Internal Use License, no redistribution), and the ArmCpu and pthread recipes require it to be built. The
  record says `"releasable": false`.
- **The release kernel is the hosted `ONEIOS_CRYPTO=original` one** (the owner's call of 2026-11-21: the OneOS
  app's update downloads it with the runtime package and prelinks it on the phone with the phone's own kernelcache).
  `kernel/build-hosted.sh ONEIOS_CRYPTO=original` (docs/building.md section 2) links no source corecrypto: the
  kernel collection's own `com.apple.kec.corecrypto` serves the crypto KPIs once prelinked. Its record counts it:
  0 of the corecrypto-2023 archive's 1191 global symbols are external symbols of the kernel (a source kernel: 878),
  `"restricted_sources": []`, `"releasable": true`; the build fails otherwise. The 18 `cc*` symbols such a kernel
  has are XNU's own `osfmk/corecrypto` (APSL). The other linked parts are under the licences listed at the end of
  this section. A prelinked `kernel.kc` is never releasable (Apple's kexts): each phone prelinks its own.
- Proven for the first candidate, `xnu-12377.121.6-ios-27.0.1-24A446-703a6b34e73f` (2026-11-21, Linux PC): the build
  (318 s, 20 cores), the record (`"releasable": true`), `kernel/publish.sh --hosted release` and `runtime/package.sh`
  with it (`REQUIRE_KERNEL=1`: `runtime.json` names it); the prelink with the phone's code (`oneios-tool prelink
  build`, no reference: 2.8 s, 698 MB peak RSS, audit 0 failures, stubs 666 good / 0 bad, 93 originals on the stub,
  96,206,848 bytes, the same SHA-256 twice); the PC boot to SpringBoard and PosterBoard, no panic: see
  [kernel/prelink/README.md](prelink/README.md), "Original corecrypto". **Not proven**: the prelink's time and memory
  on the phone, a boot on the phone.
- Never in git or a release: the kernelcache, a prelinked `kernel.kc`, KDK headers (`out/`).
- UniXNU, UniXNU-droidvm: BSD-2-Clause-Patent. xnu-linux-toolchain: see its repository. libdispatch (firehose):
  Apache 2.0. libpthread, libplatform: APSL 2.0.
