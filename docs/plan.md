# iOS 27 as a OneOS-style guest on the Galaxy S24 Ultra: engineering plan

Date: 2026-11-10. Research only: nothing here has been run on the phone.

Scope (set by the owner): one phone, the Galaxy S24 Ultra (SM8650, Adreno 750, One UI, Android 17,
locked bootloader, KernelSU 3.3.0 loaded late, SELinux enforcing). Target guest: iOS 27.0.1 userland of
iPhone18,2. Main track: iOS 27 as a OneOS/OneLineage-style guest on the running One UI kernel. The
Gunyah VM is a side note (section 8). The OnePlus phones and the native reboot-switch are dropped
(section 9).

Marks: **[F]** fact from a source (link or path given). **[I]** inference of this plan, not demonstrated
anywhere. **[U]** unknown until measured on the phone.

Clones used: `/tmp/claude-1000/-home-akshay-Documents-GitHub-vphone-android/1618c98d-fa5e-42dc-ad67-bee1581c14f2/scratchpad/clones/`
(`OneOS`, `OneLineage`, `UniXNU`, `UniXNU-droidvm`, `macOS-DroidVM`, `DroidVM`, `DroidVM.wiki`,
`gunyah_host_mod`, `iOSPlus10`). Paths below are relative to those clones or to this repository.

## 1. Verdict

A literal copy of OneOS cannot work for iOS, and the plan does not pretend otherwise: OneOS guests are
Android frameworks, so they run unchanged on One UI's Linux kernel, binder and vendor HALs. iOS userland
needs XNU: about 560 BSD system calls, the Mach traps and MIG interfaces, and Apple's closed kernel
extensions (IOSurface, IOGPUFamily, IOMobileGraphicsFamily, sandbox, AMFI, APFS). The workable way to
keep everything else about OneOS (an image in a folder, session scripts that stop One UI's framework
and start the guest on the same kernel boot, a return without flashing, the phone's own GPU and input
drivers) is to **run XNU itself as an ordinary Linux process on One UI's kernel** ("xnuhost"): the
source-built xnu-12377 that UniXNU already runs under iPadOS 27.0.1 userland, with Apple's original
kexts linked in, and with the hardware layer replaced by a gVisor-style layer (iOS processes are Linux
processes whose system calls and page faults are trapped to the XNU process). This reuses UniXNU's
kernel patches, drivers and Metal plugin instead of rewriting XNU's behaviour, gives native CPU speed
without a hypervisor, and needs root only for the OneOS-style takeover, not for running. It is new work
(nobody has published a user-mode XNU), sized at **45 to 80 person-weeks to SpringBoard with GPU and
the switch app**, with the two largest risks retired in the first three milestones. Rewriting the
XNU personality by hand (Darling's way) reaches a command line sooner and SpringBoard probably never.
If the first three milestones fail, the fallback is the Gunyah VM, which is the cheapest route on paper
but depends on a hypervisor that is unreliable on this SoC with a kernel we cannot replace.

## 2. What OneOS does, and what carries over to an iOS guest

| OneOS mechanism | Source | For an iOS guest |
| --- | --- | --- |
| Guest image = `system.raw` (built from OEM firmware with MysticGSI) plus patched framework files, staged as root in `/data/local/tmp/oneos`, accepted only by size and SHA-256 | [F] `OneOS/README.md` "How it works", `docs/new-guest-systems.md` | Same shape: read-only system images built from the owner's restore files on a build machine, plus our kernel and overlay, pinned by SHA-256 (section 4) |
| `oneos_switch.sh`: all refusals before the first change; then stops One UI's zygote and system_server; kernel, vendor services and radio keep running | [F] `OneOS/switcher/assets/oneos_switch.sh` lines 11-35, README step 3 | Same: preflight, then stop the framework. An iOS guest needs far fewer of One UI's daemons |
| Guest starts in a mount namespace with `/system` = image and `/data` = stage; `zygote_exec` stands in for init | [F] `OneOS/switcher/native/zygote_launcher.c`, `docs/zygote-sockets.md` | `xnuhost` stands in for the whole kernel: it is started in a namespace that contains only the stage and the device nodes it uses |
| Guards on the guest framework (own vold, no weaver/gatekeeper writes, storage and installd guards, app ids from 15000) | [F] `OneOS/docs/new-guest-systems.md` "Safety patches" | Mostly not needed [I]: an iOS guest has no binder client and no path to One UI's vold, keystore or gatekeeper. The one guard that matters is the namespace: nothing of One UI's `/data` is visible |
| Return in place when a baseline taken at the switch still holds, else reboot; failsafe reboot on any failed check | [F] `OneOS/docs/inplace-return.md` | Same structure, shorter: kill the guest tree, check nothing of the stage is mounted or mapped, start zygote. Reboot as failsafe |
| Volume Up + Volume Down for 3 s returns | [F] `OneOS/README.md` | Same: `xnuhost` reads the key events itself |
| Works with temporary root; root is reactivated after a reboot return | [F] `OneLineage/README.md` Requirements ("Can be temp-root too"); `OneOS/README.md` step 8 | Same. A reboot return loses KernelSU until it is loaded again |
| Has run on the S24 Ultra (SM-S928) with KernelSU | [F] `OneOS/README.md` status table (Infinity-X 4.1) | The stop/start mechanics are proven on this phone model |

What does not carry over: everything under the guest's libc. An Android guest uses One UI's kernel
ABI directly. The next section is the replacement.

## 3. The approaches

Per-phone columns are for the S24 Ultra only. Effort is person-weeks for one experienced engineer,
to SpringBoard with touch and GPU, unless stated. All speed figures are expectations [I].

| # | Approach | Feasibility | Speed expected | Root needs | Effort | Biggest risk |
| --- | --- | --- | --- | --- | --- | --- |
| 1a | **Hand-written XNU personality in user space** (Darling's design: `darlingserver` for Mach IPC, translated BSD calls, hand-written IOKit shims) | CLI: yes. SpringBoard: unlikely | Native CPU; IPC through a server | None to run; root for takeover | CLI 8-12; SpringBoard 150-300+, open-ended | Bug-compatible rewrites of IOSurface, IOGPU, IOMobileFramebuffer, sandbox, APFS semantics for closed userland that changes every release |
| 1b | **User-mode XNU ("xnuhost")**: real XNU plus Apple's kexts as a Linux process; iOS tasks are trapped Linux processes | Plausible; novel | Native CPU; system calls and page faults cost a trap (several times a native one); GPU direct on KGSL | None to run [I]; root for takeover, tun networking, real file ownership | **45-80** (launchd+shell 22-34 of them) | Apple's kexts and XNU's machine layer assume EL1; the port of that layer is new |
| 1c | **Kernel-module personality** (Mach IPC and Mach-O loader inside Linux, as Cycada did with a custom kernel) | Poor on this phone | Best of the three | Must load our own module into Samsung's kernel through KernelSU, every boot | 100+ | [U] whether an out-of-tree module loads at all here; a module bug panics the phone and drops the temporary root; one build per kernel release |
| 2 | **Gunyah VM** (crosvm + UniXNU guest, DroidVM) | Demonstrated on SM8650 for iPadOS 27; [U] on Samsung's kernel | Near native CPU; GPU through virtio-gpu to KGSL | Root every boot; 3-4 kernel modules | 8-14 | SM8650's Gunyah memory bug needs kernel patches a locked bootloader cannot take (section 8) |
| 3 | Native reboot-switch | Not possible: locked bootloader | - | - | - | Dropped |
| 4 | Apple's own vresearch101 kernel under Gunyah | Not possible | - | - | - | That kernel runs under SPTM/TXM, which need Apple's GXF and SPRR CPU features ([F] `docs/ARCHITECTURE.md` "CPU: apple-gxf"); Cortex cores do not have them and a guest cannot emulate them |

Why 1b over 1a. Darling gained aarch64 Linux host support on 2026-11-06 and runs bash, launchctl,
Perl and Python as native arm64 Mach-O [F]
(https://github.com/darlinghq/darling/pull/1818). But it runs a libSystem **it builds from source**,
whose system-call layer calls Darling's translation instead of trapping. iOS 27's libsystem_kernel
is inside Apple's dyld shared cache and issues `svc #0x80` itself, so the trap layer has to be built
either way, and after it 1a still has to re-create every kernel behaviour that closed daemons depend
on. 1b builds the same trap layer and then puts the real kernel behind it. UniXNU shows that the
real kernel with iOS-family 27.0.1 kexts brings SpringBoard up once its platform is provided [F]
(`UniXNU/docs/devices/droidvm.md`, `UniXNU-droidvm/ipados27/board.toml`).

Why not 1c. KernelSU's own module is loaded on this phone, but that says nothing about a second
module: whether Samsung's kernel accepts an unsigned out-of-tree module through the ordinary
`init_module` path under the late-loaded KernelSU is [U]. Plan 1b treats a module as an optional
accelerator at most (section 5.4), never as a requirement.

## 4. The iOS 27 "image" and its stage

Analogue of OneOS's `system.raw` + package set + stage. Everything Apple's is built on the owner's
build machine from the owner's iPhone18,2 27.0.1 restore files and never enters git or a release
(same rule as OneOS's images, `OneOS/README.md` "Where the images come from").

Stage folder: `/data/local/tmp/vphone-ios/` (root-owned, mode 700).

| File | What it is | Built from / with |
| --- | --- | --- |
| `os.img`, `systemos.img` | The OS and SystemOS volumes, read-only, each with a hash manifest | UniXNU's release layout [F] (`UniXNU-droidvm/README.md`: `os.img`, `systemos.img`, `kernel.macho`, `release.json`); source: the IPSW's OS and SystemOS cryptex images |
| `app.img` + trust caches | AppOS cryptex and the static/system trust caches | UniXNU `[ipsw] fetch` list [F] (`UniXNU/profiles/os/ipados-27.0.1-24A446/profile.toml`) |
| `kernel.kc` | Source-built xnu-12377.121.6 with the Darwin 26 system calls, plus the original iPhone18,2 kexts prelinked, plus our host drivers | UniXNU `XnuPrelink` and a new OS profile (section 6) |
| `xnuhost` | ELF executable: loads `kernel.kc`, provides the "hardware" (section 5) | New; Rust, in `android/vphone-native` next to `launcher.rs` |
| `stub` | Small static ELF every iOS task runs inside | New |
| `data.seed`, `data.store` | Data volume seed (setup skipped, preferences) and a sparse file of changed blocks. The stage's `/data` | UniXNU's Data store format [F] (`UniXNU-droidvm/README.md` "Data disk", `package/DroidVMDataStore.c`); `scripts/seed-data-skip-setup.py` |
| `overlay/` | Metal plugin bundle, owner trust bundle with its CDHashes, debug tools (`applebox`, `ownervisor`, dropbear) | Droid-VM/NativeMetal, `metal2vulkan` (also our submodule), Turnip; `make-owner-trust-bundle.py` [F] |
| `image.json` | Sizes and SHA-256 of all of the above; the app refuses anything else | As `OneOS/app-android/.../Images.kt` |

The existing vphone VM disk is **not** the image. Its Data volume's keys live in that VM's sepOS
storage [I], and its kernel is the vresearch101 one (approach 4). It stays useful as an oracle: a
running, correct iOS 27.0.1 where the same daemon's system calls, IORegistry and logs can be compared
(`vphoned` gives a root shell).

Session scripts (new, modelled on `OneOS/switcher/assets/`, shipped as app assets):

- `ios_switch.sh`: preflight (root, `image.json` hashes, battery >= 30 %, temperature, nothing left
  mounted under the stage, enough free RAM after the stop), then baseline (boot id, pids of vold,
  keystore2, surfaceflinger, the Wi-Fi state), then stop zygote and keep it stopped
  (`oneos_switch.sh` [4b/6] pattern).
- `ios_start.sh`: new mount namespace with the stage, `/dev/kgsl-3d0`, the input event nodes, a tun
  device and nothing else of `/data`; start `xnuhost`; start the display helper.
- `return_to_oneui.sh`: kill the `xnuhost` tree, check no process is left in the namespace and no
  stage file is mapped, release the display layer, `start zygote`; any failed check ends in the
  reboot failsafe (`OneOS/docs/inplace-return.md` gates A and "Before anything starts").
- Two session modes. **Overlay** (development, milestones 0-6): One UI stays up, the guest is a
  full-screen activity of the existing app, no root. **Takeover** (milestone 7): the OneOS-style
  session above.

## 5. xnuhost: what must run under iOS 27 so launchd, backboardd and SpringBoard come up

```text
 One UI's Linux kernel (unchanged)
 ├─ xnuhost  (one Linux process: "the XNU kernel")
 │    ├─ kernel.kc: xnu-12377 (UniXNU arm64e + iOS 27 profile) + original iPhone18,2 kexts
 │    │     + host drivers: HostBlock, HostDisplay, HostHid, HostGpu, HostNet, HostAudio
 │    ├─ host layer (Rust): memory (one memfd = guest RAM), task/thread control, timers,
 │    │     "interrupts" (eventfd threads), the Linux side of each host driver
 │    └─ N vCPU threads running XNU's scheduler
 └─ one Linux process per iOS task (launchd, backboardd, SpringBoard, ...)
      └─ stub + the task's memory, mapped from the memfd by xnuhost's order;
         every svc and fault traps to xnuhost
```

### 5.1 Trap layer (the gVisor "systrap"/User-Mode-Linux pattern)

- **System calls.** iOS code issues `svc #0x80` with the number in x16; Linux would read x8 and run
  an arbitrary Linux call. Each task process installs a seccomp filter that returns `SECCOMP_RET_TRAP`
  for every system call not issued from the stub's own page. The stub's SIGSYS handler passes the
  register frame to `xnuhost` through a shared page and a futex; `xnuhost` enters XNU's normal
  `sleh_synchronous`/`handle_svc` path with that frame as the saved state [I].
- **Page faults.** A task's address space starts empty. A fault is a SIGSEGV in the stub, forwarded
  the same way; XNU's `vm_fault` resolves it; the pmap layer answers with "map memfd offset P at VA V
  with protection R" and the stub does the `mmap(MAP_FIXED|MAP_SHARED)`. `pmap_remove`,
  `pmap_protect` and `pmap_page_protect` become `munmap`/`mprotect` orders. This replaces
  `osfmk/arm64/sptm`/`pmap.c` with a new, much smaller pmap [I].
- **Page size.** One UI's kernel has 4 KiB pages [U, expected]; iOS userland assumes 16 KiB. UniXNU
  already runs XNU with 4 KiB stage-1 pages under 16 KiB userland, including the dyld pager and the
  code-signature hashes [F] (`UniXNU/docs/xnu/four-k-page-tables.md`;
  `UniXNU-droidvm/ipados27/patches/xnu-12377/droidvm-dyld-16k-peek-first.patch`,
  `droidvm-native4k-once.patch`). Those patches carry over unchanged.
- **Threads.** One Linux thread per iOS thread inside the task's process; XNU's scheduler runs on N
  vCPU threads in `xnuhost` and "returns to user" by releasing the task thread until its next trap.
  A timer signal forces a trap for preemption [I].
- **Kernel's own privileged instructions.** XNU's source is ours, so `msr`/`mrs` of EL1 registers,
  `eret`, TLB and interrupt-mask operations become calls into the host layer in a new machine
  configuration beside the `BCM2837` one UniXNU builds on [F for the base:
  `DEVELOPMENT_ARM64_BCM2837` in `profile.toml`; I for the new one]. Apple's kexts are binaries: any
  EL1 instruction inlined in them must be rewritten at prelink time or emulated from SIGILL.
  How many there are is [U]; milestone 0 counts them.
- **Kernel addresses.** XNU and the kexts expect kernel pointers in the high half; a Linux process
  has only the low half (39 or 48 bits [U]). The kernel collection is linked at a low base (UniXNU's
  prelink already applies a slide, `slide = "-0x1000000"` [F]) and the source constants
  (`VM_MIN_KERNEL_ADDRESS`, the pointer-packing bases) move with it. Kext code that hard-codes the
  high range is the second thing milestone 0 counts [U].

### 5.2 PAC

- The cores have PAC (`paca pacg`; Cortex PAC is what lets arm64e userland run on Qualcomm phones
  [F] `iOSPlus10/README.md`), and Linux enables it for user space with five keys per process, new at
  every `exec`, inherited by `fork` [F, Linux arm64 pointer-authentication documentation].
- iOS expects the A keys to be the same in every process that shares a dyld cache region, and XNU
  signs shared-cache pointers on the process's behalf. UniXNU and iOSPlus10 solve this on real
  hardware with **one global key set** [F] (`iOSPlus10/docs/ios26-port-status.md` "PAC";
  `UniXNU/docs/porting/arm64e.md` requirements). The same model falls out here for free: `xnuhost`
  and every task process are forks of one process and never `exec`, so all hold the same keys, and a
  pointer signed inside `xnuhost` authenticates in any task [I]. No key needs to be written, so no
  ptrace key access and no module.
- arm64 (not arm64e) third-party apps must run with the keys off (UniXNU:
  `droidvm-native-pac-user-jop.patch`, `-ib.patch` [F]). Linux has
  `prctl(PR_PAC_SET_ENABLED_KEYS)` for exactly this [F]; whether One UI's 6.1 kernel has it is [U].

### 5.3 The thread register

iOS user code reads its thread-specific data base from `TPIDRRO_EL0`. Linux leaves that register at 0
for every native task [F] (Darling hit it: "dyld's `_tlv_get_addr` read `TPIDRRO_EL0`, which Linux
always leaves at 0", PR 1818). User space cannot set it. Two ways, decided by milestone 0:

1. **Text fixup, no root** [I]: when a page is mapped executable, rewrite
   `mrs xN, TPIDRRO_EL0` to `mrs xN, TPIDR_EL0` (same length), keep the TSD base in `TPIDR_EL0`
   (user-writable, per thread on Linux), and rewrite the few original `TPIDR_EL0` reads (the CPU
   number) to return 0. UniXNU already patches user text after signature validation (its "text
   fixups" of QuartzCore and `droidvm-text-fixup-bitmap.patch` [F]); this reuses that mechanism.
   Risk: a literal that happens to equal the instruction encoding.
2. **Small kernel module** [U]: sets `TPIDRRO_EL0` per thread at context switch. Only if modules load
   on this phone, and only as an optimisation.

### 5.4 Mach IPC, launchd, XPC, code signing, sandbox

All of these are XNU's own code or Apple's own kexts running unchanged, which is the point of 1b:

- Mach ports, `mach_msg`, vouchers, kqueue/workloops, workqueues, ulocks, memory entries, the shared
  region: XNU source. launchd and XPC see a real kernel.
- AMFI, CoreTrust, AppleImage4, sandbox, IOSurface, IOGPUFamily, IOMobileGraphicsFamily, IOHIDFamily,
  APFS: the original kexts, on UniXNU's root list [F] (`profile.toml` `roots`). UniXNU's trust model
  carries over: original trust caches plus an owner trust bundle for our own binaries, AMFI started
  early through a pinned address, sandbox with grant patches for the GPU
  (`zzw-native-sandbox-grant-patterns.patch`, `droidvm-gpu-sandbox-first.patch`) [F].
- SEP: there is none. UniXNU's `AppleKeyStoreState` and `SoftwareAES` components stand in ("keychain
  items store through AppleKeyStore wrap and unwrap; bluetoothd runs" [F],
  `UniXNU/docs/devices/droidvm.md`). No passcode, no Face ID, no Secure Element.

### 5.5 Host drivers (the "vendor HAL" side)

Each replaces a virtio driver of the DroidVM board with a direct use of One UI's kernel.

| Guest need | UniXNU-droidvm piece it replaces [F] | Host side on One UI |
| --- | --- | --- |
| Root and Data disks | `NativeVirtioBlock`, `NativeRamBlock`, `DroidVMDataStore.c`, verified root | `pread`/`pwrite` on the stage files |
| Display (backboardd's IOMobileFramebuffer) | `GopFramebuffer`/`DroidVMVirtioGpuDisplay.cpp` under the original `IOMobileFramebufferLegacy` | Overlay mode: the app's Surface. Takeover: a root SurfaceControl layer on One UI's surfaceflinger, as OneOS's `OneOSBootStatus.dex` draws without ActivityManager [F] (`OneOS/docs/boot-status.md`); zero-copy through AHardwareBuffer [I] |
| Touch, keys, sensors | `NativeHidInput`, `DroidVMVirtioInput.c` | Overlay: the app's touch events (our `touch.rs`). Takeover: `/dev/input/event*` |
| GPU (Metal) | NativeMetal plugin + metal2vulkan + Turnip in the guest, virtio-gpu native context to the host's `drm2kgsl` | Same plugin and Turnip; the kernel side forwards the KGSL calls to `/dev/kgsl-3d0` directly and backs GPU buffers with the task's memfd pages (the import path DroidVM's host uses: udmabuf, `gunyah_host_mod/README.md` "udmabuf") [I] |
| Network | `DroidVMVirtioNet.c` + `DroidVMVirtioEthernet.cpp` on IONetworkingFamily-177 | Overlay: user-mode NAT as today (`net.rs`). Takeover: tun device; Wi-Fi association kept by driving One UI's wpa_supplicant [I] |
| Audio | virtio-snd device | AAudio, as `audio.rs` does now |
| Video codecs | VideoToolbox paravirtualisation over virtio-media (`706338e`, `1479852`) | Later; the vtpv message contract is the one reims-vgpu documents (`macOS-DroidVM/docs/reims-vgpu-vtpv-contract.md`) |

**Can the IOGPU / ParavirtualizedGraphics user-client protocol feed reims-vgpu directly?** No.
reims-vgpu is the host end of the *device* protocol that Apple's AppleParavirtGPU kext speaks; the
user-client ABI between the Metal driver bundle and that kext is inside the kext. It could be reused
only by also loading the AppleParavirtGPU kexts of the vresearch101 kernelcache into `xnuhost` and
giving them an emulated device backed by reims-vgpu in the same process [I]. That keeps our tested
renderer but mixes kexts of two kernelcaches. It is the fallback GPU path (milestone 6), not the first.

## 6. iOS, not iPadOS: what carries over and what differs

Everything UniXNU and DroidVM have published at 27.0.1 is iPadOS (iPad17,3 J820AP, build 24A446) [F]
(`UniXNU/profiles/os/ipados-27.0.1-24A446/profile.toml`). No iPhone profile exists. iOS and iPadOS
27.0.1 share the Darwin release and kext families, so most of it should transfer [I]:

| Piece | Carries over | What changes for iOS 27.0.1 on iPhone18,2 |
| --- | --- | --- |
| XNU base: xnu-12377.121.6 "with the Darwin 26 system calls added", arm64e profile, 4 KiB patches, `ipados27-bringup.patch`, scheduler/idle patches | Yes [I] | The VM-specific ones (`non-apple-vcpu-*`, `droidvm-amp`, `droidvm-timebase-24mhz`) are replaced by the xnuhost machine layer. If iPhone userland calls a system call the iPad one does not, add it to the series [U] |
| `XnuPrelink` and the kext root list | Tool yes | New pins from the iPhone18,2 kernelcache: its SHA-256, the AMFI early-init address, `abi/original-classes.json`, the reviewed fileset, the regression policy. This is a new OS profile `ios-27.0.1-<build>`; it is review work, not design work |
| Verified root, owner trust bundle, Data seed and store | Yes | New volume UUIDs and manifests; the seed's skip-setup preferences need the iPhone setup panes (SIM, Face ID) added [I] |
| `AppleKeyStoreState`, `SoftwareAES` | Yes [I] | An iPhone has more SEP clients (biometrickitd, Secure Element, Wallet); keep them from blocking boot [U] |
| Device identity | Mechanism yes | ADT `model`/`target-type`/`compatible` for iPhone18,2 instead of `iPad17,3`/`J820AP`; drop `ipad_identity = true` (`UniXNU-droidvm/board.toml`); MobileGestalt must report a phone so SpringBoard builds the phone UI |
| Input | Digitizer path yes | Drop the iPad-only work: Magic Trackpad (`AppleTopCaseDriverV2`), Apple Pencil, pointer rendering fixes (`0c3fa5d`, `b7396ee`, `8d8f023`). Add the iPhone's side buttons as HID |
| Display | IOMFB backend yes | Portrait phone panel at @3x instead of the iPad's @1x fixups (`ffeb241`) and landscape handling; display metrics come from the iPhone's device-tree values. Dynamic Island and always-on layouts follow the identity [U] |
| Metal plugin (NativeMetal + metal2vulkan + Turnip), IOSurface/GPU wait rules (`e29d087`, `7651f2b`), sandbox GPU grants | Yes: user space and kext families are the same [I] | QuartzCore/RenderBox hooks in the plugin are target-specific (`macOS-DroidVM/docs/TODO-gpu.md` reference table) and must be re-found in the iPhone shared cache |
| Telephony | Nothing to carry: the iPad profile is Wi-Fi only | CommCenter and the baseband-dependent daemons must idle without a baseband. Our vphone VM shows iPhone userland doing that on a board with no baseband [F] (`docs/ARCHITECTURE.md`); copy its device-tree shape. No calls or SMS |
| Other iPhone hardware daemons | - | Haptics, UWB, NFC, satellite, extra cameras: disable or stub their launchd jobs [I] |
| Bluetooth virtual controller, Wi-Fi interface for wifid, app install (`ipatool`, `lsdiag`), `applebox`, `ownervisor` | Yes [I] | - |

From the older native ports (iOSPlus10 on SM8450, and the ports it lists): what transfers is method,
not code: the PAC result above, the practice of applying one patch series to two XNU versions with
rules (`ios26-port-status.md` section 1), and the userland root tweaks list (`root-tweaks.py`).
Their SoC drivers (UFS, Adreno register code, Wi-Fi) are not needed, because One UI's kernel keeps
driving the hardware.

## 7. Milestones

Effort in person-weeks (pw). Each ends with a test on the S24 Ultra. Milestones 0-2 are go/no-go
gates; if one fails, stop and take section 8.

### M0. Phone and kext probes (1-2 pw)

Deliverables:
- `tools/xnuhost-probe/` (new): one static arm64 binary, run both from `adb shell` (through KernelSU)
  and from inside the app's sandbox, reporting: kernel release, page size, user VA bits; PAC features
  and that a pointer signed before `fork` authenticates in the child and not after `exec`;
  `PR_PAC_SET_ENABLED_KEYS`; a trapped `svc #0x80` round trip through seccomp and its cost in ns;
  executable shared memfd mappings in each SELinux domain; a remote-ordered `mmap` fault round trip
  and its cost; `TPIDRRO_EL0` reads 0 and `TPIDR_EL0` is writable; the fixed iOS addresses
  (commpage `0xFFFFFC000`, shared region `0x180000000`) are free.
- Kext census (PC): disassemble the iPhone18,2 kernelcache's root kexts and their dependencies; count
  EL1-only instructions and hard-coded high-half address tests per kext.
- Optional [U]: whether a harmless out-of-tree module loads (DroidVM's app does this from its Kernel
  Module tab, `gunyah_host_mod/README.md`); decides 5.3 option 2 and section 8.
- `docs/xnuhost-probes.md` with the numbers.

Test: the probe prints all results on the phone in both contexts. Gate: seccomp trap, executable
memfd and fork-shared PAC keys work; census shows a rewritable number of sites (tens to low
hundreds per kext, not thousands).

### M1. Trap layer runs an iOS binary (3-4 pw)

Deliverables: `stub`; the host layer's memory and trap code in `android/vphone-native/src/xnuhost/`;
a Mach-O loader for one **static** arm64e test binary (built by us) that makes raw XNU system calls
answered by a 200-line fake kernel (write, exit, mmap, fork).
Build on: gVisor's systrap design and User-Mode Linux's skas mode (design only); Darling's `mldr` for
loader details (`src/startup/mldr/`).
Test: the binary prints, forks and exits on the phone from `adb shell` without root; trap cost
measured against M0.

### M2. XNU boots in user mode, with Apple's kexts (16-24 pw; the heart of the plan)

Deliverables:
- New XNU machine configuration `LINUXHOST` beside `BCM2837` in the UniXNU tree (fork of
  https://github.com/UniXNU/UniXNU, patches in a new `patches/xnu-linuxhost/` series): new pmap,
  context switch, trap entry, timers, idle, copyio; kernel linked low.
- `xnuhost` loads the kernel collection, starts N vCPUs, reaches `bsd_init`.
- Prelink of the original kexts for this layout with the M0 rewrites (extend
  `kits/XnuPrelink/emit_prelink.py`), first corecrypto + IOSurface + IOHIDFamily, then the full root
  list.
- `HostBlock` driver; source-only kernel boots a small RAM root first (as UniXNU bring-up profiles
  do, `UniXNU/docs/devices/erhai.md`).

Test on the phone: kernel log shows IOKit matching quiet with the full root list; a static test
binary runs as pid 1; a kext self-test creates and locks an IOSurface. Gate: all original kexts
start. Start with the iPadOS 27.0.1 profile exactly as UniXNU pins it, so any failure is ours and
not the profile's.

### M3. launchd and a shell from the iOS 27 image (5-8 pw) - first user-visible milestone

Deliverables: the iPadOS profile's `os.img`/`systemos.img` first, then the new
`profiles/os/ios-27.0.1-<build>` (section 6) and its images; verified root; dyld shared cache mapped
with 16 KiB fixups; the thread-register solution of 5.3; Data seed and store; dropbear from the
overlay.
Test: on the phone, Apple's `launchd` is pid 1, `launchctl list` answers, a shell over the guest's
dropbear runs `sw_vers` (reporting iOS 27.0.1) and `ps`; 30 minutes without a kernel panic. Compare
`launchctl print system` with the vphone VM's.

### M4. backboardd and SpringBoard, CPU-rendered (8-12 pw)

Deliverables: `HostDisplay` under the original IOMobileFramebufferLegacy; `HostHid` digitizer;
`AppleKeyStoreState`, `SoftwareAES`; iPhone identity and device-tree values; telephony and hardware
daemons idled; app-side full-screen Surface in overlay mode (reuse the Compose screen and `touch.rs`).
Build on: `UniXNU-droidvm/ipados27/scripts/build-droidvm-legacy-display.py`,
`build-droidvm-hid-input.py`, `package/` sources.
Test: lock screen, swipe to the home screen, open Settings, on the phone's own screen inside the
app. Record boot-to-lock-screen time and frame rate (compare: about 1 minute in the VM with 4 vCPUs
[F]; 3-10 fps under our TCG).

### M5. Metal on the Adreno 750 (6-10 pw)

Deliverables: `HostGpu` (KGSL forwarder and buffer import); NativeMetal plugin, metal2vulkan (our
submodule's fixes included) and Turnip built for the iPhone shared cache; plugin in the owner trust
bundle; sandbox grants.
Build on: Droid-VM/NativeMetal, Droid-VM/UniXNU `patches/mesa/turnip-darwin-virtgpu.patch`,
`UniXNU-droidvm/package/DroidVMVirtioGpu3D.cpp` (as the contract to replace), Droid-VM/virglrenderer
(`drm2kgsl`, host-side KGSL use).
Test: UniXNU's `MetalCheck` and `gfxprobe` pass in the guest; home-screen paging at the panel's rate;
GFXBench runs (UniXNU signs it as a third-party app, `af625d8`). If the Turnip path stalls, try the
reims-vgpu path of 5.5.

### M6. Network, audio, apps (4-8 pw)

Deliverables: `HostNet` (user-mode NAT in overlay mode), `HostAudio` over AAudio, app install
(`ipatool`/`lsdiag`), thread placement and ADPF hints from `adpf.rs`/`rootboost.rs` applied to task
threads.
Test: Safari loads a page, a video plays with sound, one App Store app installed from an IPA runs.

### M7. The OneOS-style switch (4-6 pw; needs root)

Deliverables: `ios_switch.sh`, `ios_start.sh`, `return_to_oneui.sh`, the root display helper, direct
input, tun networking with Wi-Fi kept up, volume-key return, a session log, the app's Switch /
Back to One UI screens, `image.json` checks.
Build on: `OneOS/switcher/assets/oneos_switch.sh` (preflight and zygote stop), `return_to_oneui.sh`
and `docs/inplace-return.md` (baseline, gates, failsafe), `docs/boot-status.md` (root SurfaceControl
layer), `docs/zygote-sockets.md` (why One UI's zygote must stay stopped).
Test, over adb with the owner watching, as OneOS runs first sessions: switch; iOS home screen with
One UI's system_server gone; return in place to an unlocked One UI on the same boot id; then the
forced-failure cases (kill `xnuhost`, fill the stage) each end in One UI, by reboot at worst. Free
RAM before and after the switch recorded.

Totals: M0-M3 25-38 pw (launchd and a shell), M0-M5 39-60 pw (SpringBoard with GPU), all 47-74 pw.
The spread is M2; the estimate has no precedent to calibrate against.

## 8. Side note: the Gunyah VM on this phone

- UniXNU-droidvm boots iPadOS 27.0.1 to the lock and home screen under crosvm on Gunyah on SM8650,
  with GPU clients, input, network, persistent Data [F] (`UniXNU/docs/devices/droidvm.md`; commit log
  of https://github.com/Droid-VM/UniXNU-droidvm to 2026-11-10). The iOS work of section 6 applies to
  it unchanged.
- It needs root each boot, `/dev/gunyah` (present in this phone's root-boost log), and DroidVM's host
  modules loaded into the running kernel: `gunyah_host_share` (the pseudo-unprotected mode that
  shares RAM with the guest, "No kernel patch"), `gh_unmovable`, `udmabuf`, and on 6.1 kernels
  `gunyah_kvcalloc` [F] (`gunyah_host_mod/README.md`).
- SM8650 specifically: "fails to boot normally due to a ... Gunyah memory allocation issue ... may
  boot successfully with a small probability", fixed by two kernel patches [F]
  (`DroidVM.wiki/FAQ.md` item 4). The first patch (kvcalloc) is what the `gunyah_kvcalloc` module
  reproduces with kprobes [F]. The second (SCM VMID handling in `gunyah_qcom.c`,
  https://github.com/Andy312432/android_kernel_samsung_sm8650_gunyah/commit/b2ffff284b1f8b6cd79c280fb746b27fb6f0cbae)
  has no module that I found; with a locked bootloader it cannot be applied [I].
- So on this phone it rests on two unknowns: modules load at all [U], and VMs start reliably without
  the second patch [U]. One afternoon with the DroidVM app and a Linux guest answers both. If both
  hold, an iOS guest there is about 8-14 pw and can share milestones M3's profile work, M5's plugin
  and M7's switch.

## 9. Dropped

- OnePlus 15/11/10 and the native reboot-switch (Mu-Silicium + DarwinBoot): out of scope by the
  owner's decision; impossible on the S24 Ultra anyway (locked bootloader).
- Apple's vresearch101 kernel outside TCG (approach 4): needs GXF/SPRR.
- A kernel-module personality (1c) as the main design.

## 10. Open questions for the owner

1. Is "XNU as a process on One UI's kernel" acceptable as the meaning of "OneOS-style"? The guest is
   then staged, switched and returned like a OneOS system and uses One UI's kernel drivers, but iOS's
   system calls are served by XNU in user space, not by Linux. The stricter reading (Linux itself
   serving them, 1a/1c) is the one this plan rates as not finishable.
2. May M0 try loading one out-of-tree kernel module on the S24 Ultra (needed only for 5.3 option 2
   and section 8)? A bad load can panic the phone and drops the temporary root until it is loaded
   again.
3. Should the half-day Gunyah check of section 8 run before M1? If VMs start reliably it changes the
   cost picture by a factor of four or more.
4. Build machine: UniXNU's flow is verified on macOS 27 with Xcode 27.1; macOS-DroidVM carries a
   Linux toolchain layer (`compat/`, LLVM 21). Is the Mac available, or must everything build on the
   Linux PC?
5. Which restore file exactly (build number of the iPhone18,2 27.0.1 IPSW)? The new OS profile pins
   its kernelcache hash.
6. Takeover mode loses One UI's Wi-Fi manager, calls and notifications while iOS runs. Is that
   acceptable, or should overlay mode (One UI alive underneath, about 3-4 GB less RAM for the guest)
   be the shipped default?
7. Licences: UniXNU is BSD-2-Clause-Patent, DroidVM GPL-3.0, OneOS Apache-2.0 with OneLineage-derived
   scripts under WeWaffles' terms, this repository MIT. Reusing OneOS script text in the app needs
   the same permission OneOS obtained; writing our own scripts from its documented design does not.
8. No passcode, Face ID, Wallet, calls or SMS in the guest in any of these designs. Confirm that is
   in line with the goal.
