# kernel/prelink: `kernel.kc`

Owner: fable-prelink. The hosted XNU (`kernel/build-hosted.sh`) prelinked with the kexts of the iPhone18,2
iOS 27.0.1 (24A446) kernelcache, as one `MH_FILESET` that xnuhost loads.

```sh
scripts/bg.sh prelink env REFERENCE=out/kernel/current-hosted/reference.macho kernel/prelink/build.sh
```

| Input | From |
|---|---|
| `out/kernel/current-hosted/xnu.macho` (`KERNEL=FILE` for another) | `kernel/build-hosted.sh` |
| `out/kernel-inputs/24A446/kernelcache.macho` | `scripts/image/kernel-inputs.sh` (the profile pins its SHA-256) |
| `REFERENCE=FILE`, optional but worth 4,800 more paired functions (on the Mac: 299 fewer originals on the stub) | `XNU_FILESET_REFERENCE=1 HOSTED_OUT=<you> kernel/build-hosted.sh`: the same source as a RELEASE kernel with LTO and symbols, copied to `out/kernel/current-hosted*/reference.macho` (the tree's `build/xnu/fileset-reference/` has the original). Never booted; it need not be rebuilt for each kernel |

| Output (`out/kernel/24A446/`) | |
|---|---|
| `kernel.kc` | the collection, 93 MB. Made from Apple's kexts: **never committed, never released** |
| `kernel.kc.build.json` | the record: inputs and hashes, roots, each kext left out with its reason, the fileset entries, the rewrites, the AMFI pins, counts |
| `kernel.kc.prelink.json` | the emitter's report: per kext, the original kernel functions bound to the panic stub (`stubbed`) and the data left unpaired (`missing`) |

A build takes about 80 s on the Linux PC; on the Mac 45 s cold (mac-kernel's figure, before the audit step) and 19 s warm (the pairing is reused while the
kernel, the kernelcache, the reference and XnuPrelink's matcher are the same). A new kernel needs a new `kernel.kc`: every native address in it is that kernel's.

## Steps (`build.sh`)

Every step is `oneios-tool prelink STEP` (`tools/oneios-tool`, Rust; `build.sh` builds the binary when it is missing
or older than its sources, `ONEIOS_TOOL=PATH` names another). The steps were Python scripts here (`STEP.py`) and
XnuPrelink's `fileset_match.py`, `fileset_emit.py`, `fileset_segmented.py` and `fileset_layout.py` until 2026-11-12:
they were retired after every step's files and stdout were compared byte for byte on two Linux-built kernels
(`13ed0b70` and `399f50fa`: `kernel.kc` `ffbe95f7` and `cdd98e7e` from both chains; `tools/oneios-tool/README.md`
"prelink"). `tools/oneios-tool/tests/prelink/compare-golden.sh` runs the chain against the hashes of a recorded run
(`tests/prelink/golden/`: the released kernel `326e2144` and the kernelcache alone, 2026-11-22; the Python chain's
goldens were removed that day). Each Rust module carries its
Python original's description. The build takes 14 s (Python: about two minutes) and needs neither `python3` nor
`llvm-nm`; `llvm-mc` only with `X18=1`. XnuPrelink's checkout (`external/XnuPrelink`, pin `9b950f2`) is still read
for its reviewed fixtures (`fixtures/`) only; the matcher signature, written into the pairing and checked by the
emitter, is over the Rust matcher's version and those fixtures (until 2026-11-22 over XnuPrelink's Python sources
too; the same `kernel.kc` either way); `matcher.rs`, `emit.rs` and `layout.rs` are ports of that pin and must follow
it when it moves. The tools run by hand, not part of the build, are commands too since 2026-11-13 (they were
`classes.py`, `unmapped.py`, `port-fixture.py`, `stubs-check.py`, `lead/imports.py` and `lead/between.py`; their
output is frozen in `tools/oneios-tool/tests/prelinkpy`): `oneios-tool prelink classes`, `prelink unmapped`,
`prelink port-fixture`, `prelink stubs-check KERNEL.KC`, `prelink lead-imports [--add] [--kext ID]` and
`prelink lead-between NAME_A NAME_B [--add [NOTE]]` (each takes its script's arguments; the two lead tools read the
files of this checkout or of `--repo CHECKOUT`, and write `kernel/prelink/lead/lead-reviewed.json`).

**The whole build in one process (2026-11-15): `oneios-tool prelink build`.** For the phone, which has no bash, and
for the OneOS app's `oneios-tool serve` (its `prelink` request): the steps below with the same arguments and files,
`stubs-check` added before `kernel.kc` is put in place, `X18=1` left out. Steps that wait for nothing of each other
run at the same time there (`native-input` and `merge-reviewed` beside `roots` and `hosted`; the two `align` beside
`kclasses` and `structors`; `audit`, `stubs-check`, `record` and the hash list), and in both ways `hosted` and
`emit` read the kexts' code in threads. `build.sh` is unchanged and runs one step after the other. Compared on the
Linux-built kernel `13ed0b70` (`tools/oneios-tool/tests/prelink/compare-build.sh`): `kernel.kc` `ffbe95f7` from
`build.sh`, from `prelink build` with one thread and with four, and from `serve`; 24 of 24 files the same; 13.7 s
(`build.sh`) and 11.7 s (`prelink build`) on 4 CPUs. `prelink record` takes `--no-git`, `--oneios-commit` and
`--xnuprelink-commit` for a host without git (the record's two commits are then the ones given, or null). **Not run
on the phone.** Options and numbers: `tools/oneios-tool/README.md`, "prelink build, and the threads".

1. `prelink native-input`: the kernel's weak-lookup binds resolved to its own definitions (Linux's ld64 left one,
   `native_source_storage_start`; Xcode 27.2's ld leaves seven: the `boot_progress_*` and `boot_log_export_put`
   defaults, `native_adreno_fences`, `native_ufs_dpu_map`), and the same for `REFERENCE`;
   XnuPrelink refuses an image with imports.
2. `prelink roots`: which kexts (below).
3. `prelink hosted`: the kexts for a kernel that is a process: `mrs/msr TPIDR_EL1` -> `TPIDR_EL0` (the current thread),
   `mrs xN, MPIDR_EL1` -> `ldr wN, [x18]` (the vCPU index), the packed-pointer `orr` of `smr_hash` (State below), before the pairing so the emitter's HMACs cover the
   rewritten code. The record lists every rewrite and every other instruction a process cannot execute.
4. `prelink match` (a port of XnuPrelink's `fileset_match.py`, `external/XnuPrelink`): pairs the functions and data of Apple's kernel that kexts
   reference with ours, by strings, call graphs, vtables and, with `REFERENCE`, names of the RELEASE build.
5. What the matcher left, inferred (each pair is listed in the report, `reviewed_applied.aligned`):
   - `prelink align`: by link order, ours inside a source file with the reference's function of the same name as the
     shape (immediates, field offsets, named callees, strings), then the reference's own order. The error rate is
     measured on known pairs with their pairing hidden and written into the output (2 % today);
     `--explain ADDRESS` prints a window.
   - `prelink structors`: constructors and base destructors of classes that kexts subclass, by the vtable they store, and
     factories (`X::withFoo`) by the class they allocate and the init slot they call.
   - `prelink vtables`: every vtable paired by name, slot by slot (the public layouts are ABI). A slot overrides an
     order-inferred pair; its disagreements with other pairs are listed under `conflicts`.
6. `prelink emit`: XnuPrelink's fileset emitter with our roots, the inferred pairs (slots and structors first) and the
   reviewed records, which win (`reviewed-24A446-v54.json`: functions, data, one pointer site, the AMFI
   initializer; each with evidence and a hash of the original bytes it was read from). Apple's kernel entry is
   replaced by ours, kexts stay where they are, the collection's header takes our kernel's CPU subtype (the loader
   compares them), and the kernel's `__DATA` becomes a root segment of its own (`__DATA_KERNEL`) at the end when it
   does not fit in front of the first kept kext's data (it is 10 MB with the virtio-gpu driver's static buffers).
7. `prelink record`.

Before step 2, `prelink merge-reviewed` merges the other agents' reviewed records into ours for the build (below).

`prelink port-fixture` (then `port-fixture.py`) carried 50 of XnuPrelink's reviewed iPad identities over (functions with the same bytes here);
rerun it only when that fixture changes. `prelink unmapped` names the original function behind a stub panic.

`prelink emit` fills the kernel's source-corecrypto POST field (`__fips_hmacs`) after it moves the kernel's text. A
kernel built with `ONEIOS_CRYPTO=original` has no such field: the emitter then seals nothing and the report says
`"corecrypto_hmac_sha256": "none: no source corecrypto in the kernel"` (a OneiOS change to the port, not in
XnuPrelink). With one field it does exactly what it did before. Such a kernel's collection is
`OUT=out/kernel/24A446-nocc` in docs/building.md section 2.

## Which kexts (`prelink roots`)

Every kext of the collection is a root, except one all of whose IOKit personalities attach to hardware xnuhost
does not present (an Apple SoC block, I2C/SPI/SPMI part, RTBuddy/AFK/AOP endpoint, SEP endpoint, PCIe, USB,
Thunderbolt, accessory port), and seven decided by hand (`FORCE_OUT`: `kec.pthread`, the SEP keystore and its two
clients, the ANE HAL, FairPlayIOKit, and IOAudioCodecs, whose start waits for FairPlayIOKit; since 2026-11-25 also
the sensor processor AppleSPU and the four kexts that link it, below). The profile's 31 roots are kept whatever their personalities. A left-out kext that a kept one
links against returns for its code only, without personalities. FairPlayIOKit is out although it is not hardware
(obfuscated calls through computed tables; reason in `prelink roots`). Today (2026-11-12): 319 in the kernelcache, 130
roots, 150 in the closure (20 returned dependencies), 169 absent, 135 fileset entries (15 of the closure are codeless
KPIs). The counts of 2026-11-10 (133, 154, 139) were from before the last three `FORCE_OUT` entries.

**The sensor processor is out (2026-11-25, fix/motion-spu-clash).** Five more `FORCE_OUT` entries:
`com.apple.driver.AppleSPU` and every kept kext that links it, AppleALSColorSensor, AppleEmbeddedLightSensor,
AOPTouchKext and AppleHIDTransportSCMCommon (a library; it links AppleHIDTransport, which links AppleSPU). v0.1.29
panicked at kernel start on the S24 Ultra with AppleSPU in the collection: "Kext com.apple.driver.AppleSPU class
AppleSPUHIDDevice is a duplicate; kext __kernel__ already has a class by that name". The HID archive registers classes
of AppleSPU's names for the motion devices (`kernel/patches/oneios-hosted/hid`, "Motion sensors"), and IOKit loads the
kext of **every** personality that passes `matchPassive`, whatever its probe score (`IOService::probeCandidates` calls
`IOCatalogue::isModuleLoaded`, which loads it at once), so a personality of a higher score cannot keep it out:
AppleSPU's `AppleSPUGestureDriver` (`{0xff0c, 3}`, Transport SPU) matched the motion devices. A left-out kext that
returns as a dependency loads with its user, so the four that link it go too: AppleALSColorSensor's `AppleSPUCT7xx`
personalities match the SPU light sensor (`{0xff00, 4}`, any transport; its exclave paths are on the stub as well).
None of the five has a device here; the light sensor's event driver is the kernel's IOHIDEventDriver. UniXNU-droidvm's
iPad kernelcache has no AppleSPU at all. Counts on both kernelcaches (24A446 and 24B156) after: 125 roots (130
before), 194 left out (19 returned as dependencies), 144 in the closure (150: AppleHIDTransport is gone as well), 129
fileset entries (135). With the release kernels of main 0f4ad03 (`current-hosted-rel24a` `f7ed2b9a`,
`current-hosted-rel272` `e5129d1e`): `audit: 2707 paired, 82 functions on the stub, 9 data unpaired; 0 failures`
(24A446) and `2706 paired, 82 on the stub, 10 data unpaired; 0 failures` (24B156): 10 fewer functions on the stub
than before (the left-out kexts' Tightbeam imports), `prelink stubs-check` 666 good, 0 bad on both. The pairing files
(`pairs-*`, `stubs-*`, the reviewed records) are unchanged, so is the app's kb (`oneios-tool app kb 24A446 --check`,
`24B156 --check`: none changed); the phone's prelink takes the rule from the runtime's own `bin/oneios-tool`, so a
runtime built from this source leaves AppleSPU out on the phone too. A collection prelinked before still has it: xnuhost
then gives no motion device, and the kernel registers no motion class. The kernels of this fix (`HOSTED_OUT=motion24a
ONEIOS_CRYPTO=original`, `xnu.macho` `37792182...`; `HOSTED_OUT=motion272 ... ONEIOS_OS_PROFILE=ios-27.2.1-24B156`,
`a10fe0a4...`; 193 patches, releasable, on the build laptop): `kernel.kc` `ca2d9187...` (`.hashes` `dd1c8eb1...`,
2707 paired, 82 on the stub, 9 data unpaired, 0 failures) and `08f46a72...` (`.hashes` `842711cd...`, 2706 paired, 82
on the stub, 10 data unpaired, 0 failures), `stubs-check` 666 good, 0 bad on both, no AppleSPU fileset entry in
either. Not booted.
IOGPUFamily and what it links are roots; AGXG18P (the GPU's own driver) is left out. NativeAdreno's `AGXAccelerator`
is linked into the kernel under IOGPUFamily's `IOAcceleratorES` (`kernel/patches/oneios-hosted/gpu`, `docs/gpu.md`
5.1): two more `native_original_branches`, the class's constructor and base destructor.

## The board's drivers

xnuhost's virtio block, input and GPU guest drivers are **not** kexts: they are archives linked into `xnu.macho` by
the kernel build (`0005-gpu-3d` is in; storage is `kernel/patches/oneios-hosted/storage`, the display
`kernel/patches/oneios-hosted/display`, HID Q4b of the board). The prelink takes whatever kernel it is given; a
driver that subclasses an original class (`IOMobileFramebufferLegacy`, `IOHIDDevice`) needs that class's addresses
in this kernelcache (next paragraph). The display's archive brings 428 branches into IOMobileGraphicsFamily and
IOSurface (`native_original_branches`); both kexts are roots and keep their personalities.

A driver that subclasses an original class needs that class's addresses in this kernelcache: `oneios-tool prelink classes` reads
them from the kexts' code (metaclass, size, parent, vtable and slots, constructors, destructors), and
`classes-24A446-v54.json` has them for IOAESAccelerator, IOCryptoAccelerator, IOMobileFramebufferLegacy,
IOHIDDevice, IOSurface, IOGPUDevice, IOGPU, IOAcceleratorES, IOGPUDeviceUserClient, and AGXG18P's AGXAccelerator
and AGXDeviceUserClient (for reference: AGXG18P itself is left out of the collection). `prelink classes` also reads an
initializer that keeps the metaclass in a callee-saved register and passes it on as the next class's parent
(IOAcceleratorES, then IOGPU; the parent of IOGPUDevice found this way is 256 bytes back at most and not checked).
The lead's hosted AES archive (`kernel/patches/oneios-hosted/aes`, `0008-software-aes.patch`) is the worked
example: stubs `bti c; b` with an N_ABS `<stub>.original` at the linked address, which XnuPrelink's emitter
retargets when it places the kernel; an absolute address in a literal is wrong under xnuhost's slide.

`IOAESAccelerator` is in (AppleImage4's start waits for it). The key store is in too
(`kernel/patches/oneios-hosted/keystore`, `0022-keystore.patch`: UniXNU's AppleKeyStoreState, a class of its own
named `AppleKeyStore`, bound to nothing in the kernelcache). For it `com.apple.driver.AppleSEPKeyStore` must stay
out of the roots (`FORCE_OUT`): with the original class in the collection the name is taken and the stand-in does
not start.

The video codec archive (`kernel/patches/oneios-hosted/codec`, `oneios-tool kernel gen codec`; hevc-guest,
2026-11-15) is in the kernel too. Its two classes carry Apple's names, `AppleVideoToolboxParavirtualizationDriver`
and `AppleVideoToolboxParavirtualizationUserClient` (the sandbox names the user client a process may open), and are
classes of their own on `IOService` and `IOUserClient`, bound to nothing in the kernelcache: the iPhone18,2
kernelcache has no kext with those classes (the two names are in `kernelcache.macho` three times, all among the
sandbox kext's profile literals at file offsets 12.5 to 13.7 million, beside `AppleAVE2UserClient`'s rule; none is a
class name of a kext: Apple's profiles for iOS already name the user client), so no root is left out for it and no
class record is needed. It subclasses no original class
and calls no original function by address: IOSurface is reached through `IOService` matching and the user client's
virtual `externalMethod`. So it needs **no pairing record**: `pairs-24A446.json`, `stubs-24A446.json` and the
reviewed records are unchanged. Built on the Linux PC from `origin/main` `fb4ec88` plus the archive
(`HOSTED_OUT=hevc XNU_FILESET_REFERENCE=1`, 716 s from an empty tree; `xnu.macho` `972b9132...`, 24,961,392 bytes):
`kernel.kc` `e4ffcabd1d95f25e...` (96,141,312 bytes; main's without the archive is 96,059,392), `audit: 2711 paired,
93 functions on the stub, 9 data unpaired; 0 failures` (the figures of main's collection), `prelink stubs-check`:
666 good, 0 bad. Booted on the PC (`kernel/patches/oneios-hosted/codec/README.md`); not on the phone. On main with
0081 and 0082, on the Mac (2026-11-22, `HOSTED_OUT=codec`, no reference kernel): `out/kernel/codec-24A446/kernel.kc`
`5add18fab1cb64ce...` (96,845,824 bytes; `dyldrw-24A446`'s without the archive is 96,813,056), `audit: 2712 paired,
92 functions on the stub, 9 data unpaired; 0 failures`, `prelink stubs-check` 666 good, 0 bad; again no pairing
record was needed. With iOS 27's tile session in the staged host (2026-11-22, `HOSTED_OUT=codecfix`):
`out/kernel/codecfix-24A446/kernel.kc` `852adb8be21b525d...` (96,845,824 bytes), audit 2712 paired, 92 on the stub, 9
data unpaired, 0 failures, `prelink stubs-check` 666 good, 0 bad; no pairing record needed. With hosted 0085 (the Edge
scheduler's E -> P edges, 2026-11-23, `HOSTED_OUT=anim`): `out/kernel/anim-24A446/kernel.kc` `7b1057e2e0f9908a...`
(96,845,824 bytes), audit 2712 paired, 92 on the stub, 9 data unpaired, 0 failures, `prelink stubs-check` 666 good,
0 bad; no pairing record needed.

## AMFI and launch constraints

- `early_security_init`: AMFI's kmod start does nothing here (NULL `_realmain`); its policy registration is the
  eight-instruction wrapper at `0xfffffe00092329cc`, which `OSKext::start` calls through
  `_NativePrelinkEarlySecurityInit` (`native-security-init.patch`). Evidence in the reviewed file.
- AMFI's `PAGE_SHIFT_CONST` slot (`0xfffffe000812db68`) is bound to `unixnu_kernel_stack_shift`: the CoreEntitlements
  stack check assumes 16 KiB-aligned stacks.
- Launch constraints are the kernel's: `zzyc-owner-skip-launch-constraints.patch` (boot argument `sheng-no-lc=1`)
  and `/chosen/amfi-only-platform-code`. Nothing in the collection changes for them.
- AMFI's log line `Launch Constraint Violation (enforcing), error info: c[C]p[P]m[M]e[E]` (LaunchConstraintPolicy.cpp,
  read from the 24A446 kernelcache): C is whose trust cache category was checked (1 the launched process, 2 its parent,
  3 the responsible process), P which constraint of that category failed (1 self, 2 parent, 3 responsible), M the kind
  of failure (1 `Constraint not matched`), and E the category for such a failure (255 where none applies). So
  `c[1]p[1]m[1]e[1]` is "the launched process's category 1, its self constraint did not match". The boot argument
  `amfi_enforce_launch_constraints` can only turn enforcement on in this build (a zero value is ignored), so there is no
  boot argument that relaxes it, and this build leaves AMFI's switches alone.
- App cryptex (`0032-app-cryptex-authenticated.patch`, kernel side, no collection change): the trust cache's App
  entries are category 1 (14 programs) and 2 (20). The self constraint of both is `on-system-volume` (`MNT_ROOTFS`) or
  `on-authorized-authapfs-volume`, which is `VNOP_IOCTL(vp, FSIOC_AUTH_FS, {NULL, 0})`, and category 1 also needs
  launch type 1 and validation category 1. App daemons fail it here: the App volume is not the root, and APFS would need
  a root hash authorized by a personalized Image4 manifest, which this guest does not have. The patch makes the ioctl
  answer success for that one mount (the volume `oneios_cryptex_views` has just checked to be read-only on the block
  device xnuhost verifies chunk by chunk; `Cryptexes/App` is an APFS mount, not bindfs, so the program's vnode has
  that mount). Everything else stays: AMFI still evaluates launch type, validation category, the parent constraint
  and the trust cache entry, signature checks are unchanged, and no other volume answers differently. It is a
  deliberate relaxation for a personal guest, on the same footing as the software key store: not a security boundary.
- SystemOS volume (`0047-systemos-authenticated.patch`): programs there that carry a self launch constraint in their own
  code signature (`c[4]` in the log line; SafariConfigurationSubscriber.xpc: `on-system-volume` or
  `on-authorized-authapfs-volume`, `validation-category` 1) failed it, because the volume is the upper layer of the root
  union and not `MNT_ROOTFS`. The mount `native_owner_cache_prepare` has checked (read-only, APFS, the verified device)
  answers `FSIOC_AUTH_FS` the same way. Run on the phone (2026-11-12 14:12): the line came and SafariConfigurationSubscriber started.

## State (2026-11-11)

Under xnuhost in `qemu-aarch64 -cpu cortex-a710` (1 vCPU, 2 GiB), with the series `0001`-`0013`, the image set
`out/image/24A446` and `oneios-data-wait=3000` in the boot arguments (the first start's dyld phase takes about nine
minutes under qemu): Apple corecrypto's POST, AppleImage4, AMFI (early initializer), the sandbox and vnguard
policies, the trust caches, IOKit matching, the five hosted disks, `bsd_init`, the APFS containers, the root mount
(`BSD root: disk6s1`, `mount-complete volume ...OS`), the swap setup, pid 1's exec path (the kernel mounts the
system cryptex volume and publishes the boot manifest hash), Image4's magazine and nonce slots, and the first user
process: pid 2, `/sbin/newfs_apfs` for the Data volume's first start, exec'ed through AMFI's and the sandbox's
hooks. Its dyld maps the shared cache through the shared region (`0013`); the kernel hashes each cache file for
4K pages first (5.8 GB in about nine minutes here). `newfs_apfs` formats the volume, the kernel mounts it at
`/private/var`, unpacks and verifies the seed (6024 files, 60 s), mounts the App cryptex, and execs `/sbin/launchd`,
which runs user code. It stops at launchd's first `workq_open`, in the kernel's own scheduler:
`sched_clutch.c:6589 Assertion failed: low_node != high_node` (`sched_edge_qos_max_parallelism`; one processor set
node, standard topology). That is the hosted kernel's, not a pair.
APFS also logs "failed to find named root snapshot: Need authenticator (81)" and goes on.

Besides pairs, two things were needed for this:
- `prelink hosted` rewrites APFS's eleven inline `smr_hash_array_decode` (`orr xD, xN, #0x00ff000000000000` ->
  `and xD, xN, #0xff00ffffffffffff`): the kexts restore bits 48..55 of a packed kernel pointer to 0xff, and hosted
  kernel addresses are low.
- Kernel `0010`: the published boot manifest hash is mirrored onto the `/chosen` entry Image4 looked up before the
  platform expert's nub replaced it (the lead's `0011` then stops that nub from being made).

Debugging: `qemu-aarch64 -g PORT` and gdb (`set architecture aarch64`, `target remote`) work on xnuhost; kernel.kc
is at 0x2000004000, so an original address A is at A - 0xfffffe0007004000 + 0x2000004000.

Pairing: the matcher with the reference, the inferred pairs (order, structors and factories, slots, kernel class
objects), 624 functions and 27 data reviewed by hand here, the lead's and fable-user's records (below), and
50 carried over from XnuPrelink's iPad fixture. 214 original functions that kexts reference are on the panic stub
and 18 data targets are unpaired; `kernel.kc.build.json` lists them per kext. That was the Linux build; the Mac build
of 2026-11-12 (below) has 97 and 10.

## On a Mac (2026-11-12, mac-kernel)

The same command on macOS (Homebrew's keg-only `llvm` for `llvm-nm` and `llvm-mc`, which `build.sh`
puts on `PATH`; XnuPrelink is `external/XnuPrelink` when it exists, else the kernel base's submodule
`external/Droid-VM-UniXNU/kits/XnuPrelink` at `b74ed7c`). **Use `external/XnuPrelink` at `9b950f2`**
(`scripts/setup/get-references.sh XnuPrelink`): `b74ed7c` does not retarget the hosted archives' `<stub>.original`
branches (that came in UniXNU/XnuPrelink `2b7baca`, "absolute adapter branches"; its report key is
`native_original_branches`). The table below was made with `b74ed7c`; see "On the phone" under it. Inputs: the hosted kernel and reference built with Xcode
27.2 (`kernel/README.md` "macOS"), the series through `0038`, and `out/kernel-inputs/24A446/kernelcache.macho`
(`scripts/image/kernel-inputs.sh`; its SHA-256 is the profile's). Results:

| | Mac, with `REFERENCE` | Mac, without | Linux PC (this README's figures; the size is the phone's `kernel.kc` of 2026-11-11 14:25, before 0035-0037) |
|---|---|---|---|
| `kernel.kc` | 94,109,696 bytes | 94,109,696 | 93,585,408 |
| original functions on the stub | 97 | 396 | 214 |
| data targets unpaired | 10 | 85 | 18 |
| emitter's `stubbed` (functions bound to the stub, all kexts) | 210 | 1,692 | not in the docs |
| fileset entries / roots / closure | 135 / 130 / 150 | the same | 139 / 133 / 154 (README of 2026-11-10) |
| time | 45 s (cold, pairing included) | | about 80 s |

Fewer stubs on the Mac is expected and **not yet a proof of better pairs**: Xcode's clang generates code closer to
Apple's own kernel, so more functions match by shape. Nothing of this collection has run. `prelink lead-imports` (the
import-order check below; now run from the repository, paths relative) on it: 63 adjacent names out of order, 57 of
them `pure-leaf` pairs (bodies that return a constant, as before), and 6 others for the prelink's owner to look at:
`OSDictionary` C1 before `dictEntry::compare` (2 kexts, an aliased constructor), `vfs_context_is_dataless_manipulator`
before `vfs_context_iopolicy_unassigned_0x8000` (the settled record), `random_cpu_init` (string pair) before
`ml_set_interrupts_enabled`, `OSUnserializeXML` (reference order) before `OSUnserialize` (aligned), and
`SecureDTFindEntry` twice (reference symbol and reviewed). XnuPrelink's own tests (`python3 -m unittest`, 59) pass.

### On the phone (2026-11-12, bringup): two panics (both resolved: 1 by XnuPrelink `9b950f2`, 2 by the known-good pairs below)

Both Mac collections were booted on the S24 Ultra with the Mac runtime 0.1.7-dev (logs in
`out/phone-baseline/2026-11-12/session1-xnuhost.log` and `session2-xnuhost.log`, git-ignored).

1. `d01f7674...` (XnuPrelink `b74ed7c`): 1.1 s into the kernel, right after `droidvm-gpu: published for the virtio
   GPU`, `panic(cpu 0 ...): Undefined kernel instruction: pc=0x20068c3c50 instr=0`. The call chain is
   `oneios_gpu3d_start` -> `oneios_adreno_start` -> `sheng_gpu_power_probe` -> `native_adreno_accelerator_allocate`
   -> the stub `IOAcceleratorES::IOAcceleratorES(OSMetaClass const*)` (`bti c; b ...`). The stub's branch went to
   `0xfffffff00d8c3c50`, past the end of the collection: the `.original` value `0xfffffff00a04bc50` plus the move of
   the kernel's `__TEXT_EXEC` (`0xfffffff0072cc000` linked, `0xfffffff00ab44000` in the collection). **All 507
   `.original` stubs** of that collection pointed past it; in the phone's Linux-built `kernel.kc` of 2026-11-11 all
   507 point at their `.original` address. Cause: `b74ed7c` relocates only what the split-segment records name, and
   a branch to an absolute symbol has no record. Fixed by building with `external/XnuPrelink` at `9b950f2`.
   `build.sh` now also keys the reused pairing on XnuPrelink's `fileset_match.py` (the newer emitter refuses a
   pairing made by the older matcher: "Pairing predates the current matching checks").
   `oneios-tool prelink stubs-check KERNEL.KC` checks every stub against its `.original` (507 good on the Linux and the
   `9b950f2` collection, 0 on the `b74ed7c` one).
2. `eae11352...` (XnuPrelink `9b950f2`, the same kernel; all 507 stubs at their targets, 97 originals on the stub,
   10 data unpaired): 1.5 s in, after `BSD root`, the Data mount, the GPU add-on mount and `oneios-icons: the
   drawn-icon store had another generation`, `panic(cpu 0 ...): fileset: original component called an unmapped
   kernel function from 0x20038daa8c`. `0xfffffff00a8daa8c` is in `com.apple.security.sandbox`'s `__TEXT_EXEC`,
   called from `mac_posixsem_check_create` <- `sem_open` <- `native_gpu_addon_prepare` <- `load_init_program`. So
   Sandbox's posix-semaphore check calls an original kernel function that the Mac pairing left on the panic stub
   (the Linux pairing had it). Which one: `prelink unmapped` on this collection. Not looked at further: the owner moved
   the audit of every import of the Mac collection against the Linux one to the agent `prelink`.

The phone's baseline stays the Linux-built `kernel.kc` of 2026-11-11 until that audit is done
(`docs/building.md`, "Test cycle on the phone"). The audit is the next section.

## Known-good pairs and the import audit (2026-11-12, prelink)

The pairing of a kernelcache's imports must not move when the toolchain that builds our kernel changes. Before this,
it did: `fileset_match.py` pairs by code shape, and the Xcode 27.2 kernel and reference have other shapes than the
Linux PC's. So the result of the pairing is now a committed list, and every build is checked against it.

- `pairs-24A446.json`: 2,712 known-good pairs (2,465 functions, 247 data): every kext import of the collection that
  is bound, original -> our symbol, each with its `why`. `"leaf": true` marks an original that only computes registers
  and returns (`mov w0, #45; ret`): any of our functions with the same instructions is right for it.
  `"absent"` (a reason) marks a name that a kernel of the other toolchain does not define: without it, the original
  stays on the stub (below, "A Linux-built kernel against the list").
- `stubs-24A446.json`: the 92 originals left on the panic stub and the 9 data targets left unpaired, each with its
  reason, the kexts that import it and the count of call sites in their code.
- `prelink emit --pairs --stubs` applies the list over the matcher, the inferred pairs and the reviewed records, and leaves
  the stub list unpaired. Names resolve to our kernel's **external** symbol: Xcode's kernel has local functions with
  the names of external ones (two `_kalloc_type_impl`, `_kalloc_type_var_impl`, `_kfree_type_var_impl`), and the old
  `setdefault` over all symbols could take the local one (it did for those three in a first build of this list; the
  audit caught it).
- `prelink audit` (step 6b of `build.sh`, 3.5 s) reads every chained-fixup site of the kexts whose target is in Apple's
  kernel, the target it has in the built `kernel.kc`, and our kernel's symbol there. **The build fails** when an
  import is on the stub or unpaired without an entry in the stub list, when a stub-list entry is bound, when a bound
  import's symbol is not the list's (aliases at the same address agree; a leaf agrees by its instructions), and
  when a bound import is not in the list. `out/kernel/24A446/kernel.kc.binds.json` lists every import's binding.
  `--against OTHER.kc` prints the a/b/c comparison below for any two collections; `--draft FILE` writes a
  collection's pairs in the list's format for review (never over the committed list).
- A new pair (another kernelcache, a kext added to the roots, a kernel function renamed) fails the build until it
  is reviewed into the list with its `why`. A kernel without a listed symbol fails `prelink emit` by name.

### The audit of 2026-11-12: Linux `9476a867` (booted to the Home Screen) against Mac `eae11352`

Per original import target of the 150-kext closure (functions and data):

| | count |
|---|---|
| (a) paired on Linux, stub or unpaired on Mac | 5: `proc_self`, `get_bsdtask_info`, `clock_delay_until`, `ml_get_topology_info`, data `gIOParentMatchKey` |
| (b) stub or unpaired on Linux, paired on Mac | 130 (121 functions, 9 data) |
| (c) paired to different functions | 79: 66 pure leaves with the same instructions on both sides (`err_*`/`nop_*` vnops, `return 0` slots), 13 others |
| the same pair on both | 2,495 |
| stub or unpaired on both | 103 |

- **Panic 2** (`from 0x20038daa8c`): the call at `0xfffffe000a8daa88` in Sandbox goes through the slot
  `0xfffffe0008424138` to the original `0xfffffe000b0f2f10`, `proc_self` (220 bytes). It is in (a): the Linux
  collection bound it to `_proc_self`, the Mac one to the stub.
- **Panic 1** (`pc=0x20068c3c50`, frame `0xfffffff00d8c3c50`) is in none of the three: it is not an import of a kext
  but a branch of ours into one (the `IOAcceleratorES` constructor's `<stub>.original`), which XnuPrelink `b74ed7c` did
  not relocate (above). `prelink stubs-check` covers it.
- **Why the Mac pairing lost (a)**: Apple's kernel inlines what the Xcode-built reference calls. `proc_self` and
  `get_bsdtask_info` carry Apple's inlined `zone_id_require` (the zone-index check), `clock_delay_until` the inlined
  `mach_absolute_time`, and the reference calls both; so their lengths differ from the reference's by more than the
  10 % that `anchor_reference_order` allows, and no other rule pairs them. The same cause made a wrong pair in (c):
  Apple inlined `random_cpu_init` (and its string) into `ml_processor_register`, and the string anchor gave
  `0xfffffe000ad59054` (872 bytes, AppleARMPlatform's CPU registration) the Xcode kernel's 116-byte `random_cpu_init`.
  Why the Linux pairing had them is not known any more (its pairing files were on the PC); a guess: the Linux
  toolchain's reference inlined the same callees.
- (c), decided in the list (each `why` has the evidence): Linux's pair for `ml_processor_register`; the Mac's for
  `nop_pagein` (the original returns 22), `SHA1Final` (two arguments), `ifaddr_get_ia6_flags` (two arguments),
  `OSAction::CreateWithTypeName`, `ifnet_input_extended`, `sbappendrecord` (reference symbols, score 1.0),
  `gIOKitDebugKey` (in IOSerialFamily's import order; Linux's `gIOCommandPoolSizeKey` was not). Five originals that
  are `ret`, `return 0` or `return kIOReturnUnsupported` had Mac pairs with real bodies (`IOCPU::detach`,
  `IOPlatformExpert::getGMTTimeOfDay`, `IOBSDMountChange`, `IOBSDLowSpaceUnlinkKernelCore`, `OSObject::_Dispatch`), as
  had `OSObject::Dispatch` (a leaf in the Linux kernel, a real dispatch in Xcode's): each is bound to a function with
  the original's instructions, as on Linux. `0xfffffe000b271b70` (the lead's `dictEntry::compare`) is on the stub:
  the original is 100 bytes with a call and a string, the comparator 28; it sorts after `OSDictionary` C1 (its
  constructor, now paired); its only importer, AppleEmbeddedAudio, never starts.
- (b): 108 of 121 functions are as close to the reference's function of that name as the pairs both agree on
  (instruction similarity 0.8 or more); the 13 below were read one by one (notes in their `why`).
- The new collection `3476435a` (the same kernel, `pairs`/`stubs` applied): 2,710 paired, 94 on the stub, 9 data
  unpaired, 0 audit failures, 507 `.original` stubs good. Then two more pairs from the stub list (`vm_map_copy_discard`
  for AMFI and lifs, `IOPMrootDomain::isLPWMode` for IOAccessoryManager; evidence in their `why`): `cdc47859`, 2,712
  paired, 92 on the stub. Against Linux: (a) 1 (`0xfffffe000b271b70`, above), (c) the
  7 decided for the Mac, the rest equal or leaf-equal.

### What is still on the stub (`stubs-24A446.json`)

Every one of the 92 functions and 9 data has callers in kext code; none was called in the Linux boot to the Home
Screen, where all of them were on the stub or unpaired too. That is the evidence, not a proof for every later path:
- 56 are Tightbeam, the exclaves IPC (`tb_*` names): this kernel has no `tb_*` function and no exclaves. Importers:
  AppleALSColorSensor, AppleFirmwareKit, AppleH16ANEInterface, ExclavesAudioKext, on their exclave paths.
- 8 are imported only by kexts that never start (personalities dropped: RTBuddy, AppleSEPManager, ApplePTD,
  AppleEmbeddedAudio, AppleHIDTransport, IOSerialFamily, AppleSMC).
- 1 is `0xfffffe000b271b70` (above: the lead's record contradicted by the code).
- 36 (functions and data) are not chosen yet: the names between their neighbours in the import tables are several (`prelink lead-imports` prints
  them) and the code has not told them apart. A stub panic from one of these names it (`prelink unmapped`); its pair goes
  into the list.

### A Linux-built kernel against the list (2026-11-12, pc-kernel)

The list was drafted from the Xcode 27.2 kernel. The first Linux build since (the Linux PC, clang 23.1.4 and
xnu-linux-toolchain, `HOSTED_OUT=pc` at `7652df1`, series through `0057`, RELEASE reference) stopped in `prelink emit` on two
names Xcode's kernel has and Linux's does not:
- `0xfffffe000b13ba30` (`bti c; ret`, a leaf) was named by the clang block `___lck_mtx_gate_wait_block_invoke_2`; Linux
  clang spells block functions `..._block_invoke.NNN`, numbered by the file. Now named by `_panic_display_hibb`, an empty
  external function both kernels define (the list's own "both" pair of `0xfffffe000ab5a55c`).
- `0xfffffe000b2ce6a4` is `IODataQueueDispatchSource::DataAvailable_Invoke` with four arguments (the fourth an
  `OSMetaClass`, OSAction's when null; it checks the action's class against it and calls the handler). Xcode's iig
  generates that one; the Linux toolchain's iig generates only the three-argument one, which returns
  `kIOReturnUnsupported` (`0xe00002c7`): not the same function. The entry now has `"absent"`: in a kernel without the
  name, `prelink emit` leaves the original on the stub and `prelink audit` accepts that for this entry only (counted `absent`; bound
  to anything there, it fails). The Linux collection of 2026-11-11, which booted to the Home Screen, had it on the stub too.

Result: `kernel.kc` `ffbe95f7...` (93,700,096 bytes; the pairing cold): 2,711 paired, 93
on the stub (the 92 of the stub list and the `absent` one), 9 data unpaired, 0 audit failures; `prelink stubs-check`: 666
good, 0 bad. On an Xcode kernel the two entries bind as before. Not booted on the phone.

### On the phone (2026-11-12, prelink): the Mac collection boots to the Home Screen

Set: the hosted kernel built from a clean tree at origin/main `924a0d9` (series through `0039`, every archive on main;
`HOSTED_OUT=prelink`, 240 s), `kernel.kc` `66faa3e8` (94,224,384 bytes; 2,712 paired, 92 on the stub, 9 data
unpaired, 0 audit failures, 666 `.original` stubs good; the pairing cold, 70 s), runtime 0.1.8-dev (`285204a4`),
the app 588b155, the phone's seed and Data store of 2026-11-11. Logs: `out/phone-baseline/2026-11-12/prelink/`.
- Session 1 (10:18-10:25): first frame (backboardd's boot frame) at +89 s; launchd, SpringBoard and the daemons ran,
  no panic and no stub call in 6.5 min. Ended by my return.request while lsd was still registering apps.
- Session 2 (10:28-10:51, `oneios-ubt=SpringBoard,backboardd,lsd`): boot frame at +102 s; then 11 min of lsd at
  65-120 % (the first registration of the staged apps, 0035/0037) while SpringBoard's main thread waited for the
  LaunchServices database lock (`_LSContextInitCommon`) and another thread on a synchronous XPC reply from chronod
  (`-[CHSChronoServicesConnection _subscribeToExtensions]`). At +11 min the lock screen drew (swaps 3 -> 282 in
  100 s, PosterBoard and the widgets), the owner's swipe unlocked it, and the Home Screen showed the apps (the icons
  do not draw right yet: agent gpu). No panic and no call to the panic stub in the session.
- The session ended at 10:51:54 with a reboot of the phone that was not the session's: a host kernel panic in a
  `grep` of another agent reading the Wi-Fi driver's sysfs node `runtime_pm` (`hdd_sysfs_runtime_pm_show` ->
  `hif_rtpm_log_debug_stats`, a BRK in `qca_cld3_kiwi_v2`), from `/proc/last_kmsg`; xnuhost's log ends quietly.
- Touch: the owner's swipe at 10:40:57 drew a guest frame and unlocked; injected swipes (`touch` writing the
  touch screen's evdev node) arrived in the guest's HID but drew no frame, in both sessions.
- So the Mac collection boots at least as far as the Linux one (which had the Home Screen empty, without 0035-0037),
  and it is the phone's baseline now.

### Hot spots and timings (for the later Rust port)

On the M4: one `match_images` (the original against the reference) takes 20 s, the matcher with a reference runs two
(the native kernel's and the reference's; about 40 s by that, not timed on its own; reused while its inputs are the
same). In it: function digests and string tables of
22,000 + 27,000 functions, and the length LCS of `anchor_reference_order` (a table of up to 40,000 cells per gap,
four rounds). `kc_image.Image.fixups()` decodes 592,421 chained fixups of the kernelcache in about 1 s (pure Python
`struct` per pointer); `prelink audit` decodes three such tables and scans the stubbed slots' kexts for callers: 3.5 s.
A warm build (pairing reused) took 19 s on 2026-11-12: the inputs and `prelink hosted` 2 s, the two `prelink align` runs 4 s,
`prelink kclasses`, `prelink structors` and `prelink vtables` 2 s, `prelink emit` 8 s (XnuPrelink's emitter: layout, the fixup chains,
the HMACs), `prelink audit` 3 s, the record and the chunk hashes 1 s.

## Records of several agents, and the import-order check

- `reviewed-24A446-v54.json` is fable-prelink's. The lead keeps `lead/reviewed-lead-24A446-v54.json` and fable-user
  `user/reviewed-user-24A446-v54.json` (functions only). `build.sh` merges them for each build
  (`prelink merge-reviewed`, into `work/reviewed.merged.json`): the owner's record wins for an original both have, and
  every disagreement is printed and written under `merge.disagreements`. There is none today: the three of
  2026-11-11 were settled with the lead by the code (`0xfffffe000abd8788` is `lck_ticket_destroy`,
  `0xfffffe000ae04840` is `decmpfs_hides_xattr`, `0xfffffe000ade775c` tests I/O policy bit 15 and is bound to
  UniXNU's `vfs_context_iopolicy_unassigned_0x8000`; the lead dropped its records in `99fbf4b`).
- **Import order** (the lead's `prelink lead-imports`): a kext's `__auth_got` is sorted by imported symbol name, in runs
  (one per library it links). A paired name that sits out of order between two neighbours that are in order with
  each other is wrong, or an alias. On 2026-11-11 this check found 53 such names in the map. Corrected by reviewed
  records since: three of my own records, the vnode type tests, the publish notification symbols,
  `psignal_sigkill_*`, `cs_blob_reset_cache`, `socd_client_trace`, `MD5Init`, `OSObject::metaClass`,
  `cluster_push`, `ifnet_hdrlen`/`ifnet_set_hdrlen`, `ifaddr_findbestforaddr`, `ml_get_max_cpu_number`,
  `IODMAEventSource::dmaEventSource` (and its metaclass object), `ml_page_protection_type`,
  `kern_packet_trace_event`, and the lead's (`rootdev`, `set/getMapperOptions`, `task_is_app_suspended`).
  **`setOwnership`, decided by a panic** (lead, 2026-11-11): `0xfffffe000b34e6a8` is
  `IOGeneralMemoryDescriptor::setOwnership` (it tests the memory reference and continues in its super's), and the
  function after it, `0xfffffe000b34e6e4`, is `IOMemoryDescriptor::setOwnership` (the lock, a loop of
  `mach_memory_entry_ownership`, the two casts). The second had been paired with `IOMemoryDescriptor::CreateMapping`
  because the first held its name and `CreateMapping` was the one free name in the range. On the phone IOGPUFamily
  called it for backboardd's first Metal resources: `CreateMapping_Impl` ran with (descriptor, task, tag, options)
  and stored through a NULL `map`, a kernel data abort. A name left out of order is a wrong call waiting for its
  first caller: none is to be left for a function with arguments.
  **Still out of order and left**: `OSAction::SetAbortedHandler`/`Create_Impl` (DriverKit, not used here), the data `gIOCommandPoolSizeKey`
  and `gIOKitDebug` (one table each), and 17 of the matcher's `pure-leaf` pairs: functions whose whole body is
  `ret` or `mov w0, #N; ret` given to some other function with the same body. Those return what Apple's returned
  whatever their name. One of them is deliberately left: `0xfffffe000ab827b8` returns 0 and is imported by 17
  kexts where `kern_config_is_development` sorts; bound to this DEVELOPMENT kernel's function it would answer
  true and switch the kexts' debug paths on.
- Not paired on purpose: `0xfffffe000abcf114` (AMFI, IOSurface; frees through a zone *view* in x0 where this
  source's `kern_os_zfree` takes a zone: the argument differs, so it stays on the stub until a caller shows what
  it passes) and `0xfffffe000b0dbf28` (sandbox; no counterpart in this source).
- What the import order cannot do: tell two names apart that sort between the same neighbours, such as the
  overloads of one method. `0xfffffe000b391b04` was first recorded as `IOReportLegend::addLegendEntry` and is
  `addReporterLegend(IOReporter *, const char *, const char *)`: IOHIDFamily's source names the call, and with the
  wrong one `IOHIDDevice::start` failed (fable-hid, 2026-11-11). A pair chosen among several candidates by argument
  count alone is a guess until a caller or the kext's source confirms it.
- Metal's path (2026-11-11): the seven imports of IOGPUFamily, IOSurface and IOMobileGraphicsFamily that
  `docs/gpu.md` listed are paired (`thread_set_allocation_name`, `IOMemoryMap::wireRange`,
  `IOSharedDataQueue::withEntries`, `IOMemoryDescriptor::ktraceEmitPhysicalSegments`,
  `IOMultiMemoryDescriptor::withDescriptors`, `IOFilterInterruptEventSource::filterInterruptEventSource`,
  `PEGetModelName`); the five that Q4a noted for IOMobileGraphicsFamily and IOSurface are among them. Those three
  kexts have no import on the stub now. Each record says what in the code decided among the names the import
  order allowed; none was reached by a caller in a boot of mine.
- Pairing ahead of the boot: for an unpaired import, the names between its neighbours in every table that has it
  are the candidates (`prelink lead-imports` prints them); the code picks among them. A boot on the PC costs about ten
  minutes to the first user process, so a batch is paired this way and then confirmed with one boot.

## Original corecrypto: the release kernel (2026-11-21, release-kernel)

The kernel a release carries is `kernel/build-hosted.sh` with `ONEIOS_CRYPTO=original` (docs/releases.md "The kernel
in a release"): no source corecrypto in `xnu.macho`, so the native kernel has no `__fips_hmacs` field to seal; the
emitter's no-seal path says so in the report (`statistics.corecrypto_hmac_sha256`: `none: no source corecrypto in
the kernel`), the collection's own `com.apple.kec.corecrypto` authenticates itself, and the record's
`why_not_releasable` reads `contains the kexts of Apple's kernelcache (the kernel links no source corecrypto)`.

Run on the Linux PC (20 cores), kernel `xnu-12377.121.6-ios-27.0.1-24A446-703a6b34e73f` (24,488,792 bytes), with the
code the phone runs (`oneios-tool prelink build`, the same steps `serve prelink` takes; no reference kernel, as on a
phone):

```sh
oneios-tool prelink build --kernel out/kernel/current-hosted-release/xnu.macho \
  --kernelcache out/kernel-inputs/24A446/kernelcache.macho --profile kernel/unixnu/profiles/os/ios-27.0.1-24A446/profile.toml \
  --reviewed kernel/prelink/reviewed-24A446-v54.json --more-reviewed kernel/prelink/lead/reviewed-lead-24A446-v54.json \
  --more-reviewed kernel/prelink/user/reviewed-user-24A446-v54.json --pairs kernel/prelink/pairs-24A446.json \
  --stubs kernel/prelink/stubs-24A446.json --xnuprelink external/XnuPrelink --out out/kernel/24A446-release3 --repo .
```

- 2.8 s, 698 MB peak RSS (zsh `time`'s max RSS); `stubs: 666 good, 0 bad`; `audit: 276aadac762bcbc7: 2711 paired, 93
  functions on the stub, 9 data unpaired; 0 failures`; 96,206,848 bytes. The same SHA-256 from a second run. The
  source-crypto kernel of the same tree (`kernel/prelink/build.sh`, the script path) gives the same counts
  (2711 / 93 / 9, 0 failures) and is sealed (`corecrypto_hmac_sha256` a digest).
- PC boot (docs/building.md section 5's command, `--kernel out/kernel/24A446-release3/kernel.kc`, a new Data volume,
  8 vCPUs, 4 GiB, `--display none`): the kernel's crypto ran on the collection's corecrypto (AppleImage4, the APFS
  mounts and the Data volume's first start, AMFI, the keybag); `hello, launchd` at +184 s; `Returned from calling the
  FIPS_POST function in the corecrypto.dylib: result = true` at +189 s; `Early boot complete. Continuing system boot.`
  at +201 s; `SpringBoard[32]` running at +217 s; the session's `ready` (the guest's first frame of its own) at
  +314 s; SpringBoard bootstrapped PosterBoard (pid 147) in iOS's own log; no panic in 15 minutes. The reference,
  a source-crypto kernel of 2026-11-20 (`logs/rpc-boot.log` there): SpringBoard at +217 s, PosterBoard by +387 s.
- The first boot of an original kernel from `main` stopped at launchd: `pid 1 exited -- ... mach_ports_lookup()
  failed: -300`, then `panic(...): initproc exited`. A source-crypto kernel of the same `main` stopped the same way:
  the cause was hosted 0068 (the A56 port), not the crypto mode; fixed in 0068 (series comment).
- Not proven: the prelink's time and memory on the phone (the phone runs the same code on its own kernelcache:
  expect seconds, not minutes, and under 1 GB, as here, but not measured), and any boot of this kernel on the phone.

## Another build: iOS 27.2.1 (24B156), 2026-11-25 (ios272)

24B156's kernelcache (`out/kernel-inputs/24B156/kernelcache.macho`, SHA-256 `f6f31ac4...e9a5939`,
xnu-13432.42.3) has the same 301 fileset entries as 24A446's, at other addresses. Every record of this folder and of
the board archives that names an original address is per build (`*-24A446*`, `*-24B156*`); `build.sh` takes
`BUILD=24B156` (its kernelcache, profile `ios-27.2.1-24B156`, pairs, stubs and reviewed files) and `KERNEL=` a hosted
kernel built with `ONEIOS_OS_PROFILE=ios-27.2.1-24B156` (docs/building.md 2). One `xnu.macho` cannot serve both: the
board archives compile the original classes' addresses into it.

How the 24B156 files were made, and how to make them for a next build:

- **The class records, read again from the new kernelcache** (the same commands that wrote 24A446's, which reproduce
  the committed 24A446 files byte for byte): `oneios-tool prelink classes --xnuprelink external/Droid-VM-UniXNU/kits/XnuPrelink
  --kernelcache out/kernel-inputs/24B156/kernelcache.macho --prelink-report out/kernel/24A446/kernel.kc.prelink.json
  --out kernel/prelink/classes-24B156-v54.json` with the eleven classes, each its own argument; the same with
  `IOAESAccelerator` and `IOAcceleratorES` alone for `aes/IOAESAccelerator-24B156-v54.json` and
  `gpu/IOAcceleratorES-24B156-v54.json`; `oneios-tool kernel layout hid|net --kernelcache ... --out
  kernel/patches/oneios-hosted/{hid/IOHIDDevice,net/IONetworking}-24B156-v54.json`. (The prelink report only gives
  the collection slide, 0x1f000000000, for the `linked` addresses; check it again in 24B156's own report.) The
  records differ from 24A446's in addresses only (sizes, slot counts, every slot's diversity the same), but for
  IOMobileFramebufferLegacy 0x1dd8 -> 0x1de0 bytes, IOGPU 0x2e8/216 slots -> 0x2f0/217 and AGXAccelerator
  0xcdf8/474 -> 0xce00/475 (nothing of ours subclasses those two).
- **The address-keyed records, carried over** by `oneios-tool prelink port` (`oneios-prelink/src/port.rs`; the rules
  in its header): `--from` 24A446's kernelcache, `--to` 24B156's, `--record IN=OUT` for `pairs`, `stubs`,
  `reviewed`, `lead/reviewed-lead`, `user/reviewed-user` and the display record, with `--fields display --fields
  request` for the display record's field offsets. It pairs 211,361 of 219,799 functions (196,667 of the same
  shape) and 183,615 data addresses in 4 s; every check (address, size, SHA-256) is read again from 24B156's bytes.
  `early_security_init` lands on `0xfffffe000927b430`, the wrapper derived by hand on 2026-11-20.
  Its two methods agree: the port of the four class records above gives the same addresses as reading them again
  (only the family ranges' end bounds differ by a few bytes, which bind nothing).
- **The display record** (`display/legacy-display-24B156-v54.json`): ported; the 379 vtable slots read again from
  both vtables (all where the map put them), the 17 field offsets voted from 24B156's code (all the same; `options`
  has no instruction in the family and is kept), the 10 checks the same bytes. `instance_size` set to 0x1de0 from
  the class record: `oneios-tool kernel gen display` now refuses a record whose kernelcache, size, vtable or slot
  count differs from `classes-<build>-v54.json`'s (a unit test holds both builds' committed records to that).
- **Left out by the port, to review against the audit**: pairs `IODMACommand::Dispatch`, `IOInterruptDispatchSource::metaClass`,
  six `vnop_*_desc` objects and `gIOKitDebugKey`; the stub of IOSerialFamily's data import `0xfffffe000b727e14`;
  reviewed `csproc_get_platform_path` and `vnop_searchfs_desc`. The audit of the 24B156 collection says which of them
  a kext of 24B156 still imports.
- **The audit of the first 24B156 collection** (`BUILD=24B156 KERNEL=out/kernel/current-hosted-ios272/xnu.macho
  kernel/prelink/build.sh`, collection `27e24d90`) had 10 failures, exactly the 10 records the port had left out, at
  their 24B156 addresses. Settled (2026-11-25):
  - the five `vnop_*_desc` objects (`searchfs`, `compound_open`, `renamex`, `compound_rename`, `verify`): `prelink
    port`'s new rule "named by its content" (a data object whose pointer at +0..+24 points to a C string, carried to
    the one TO object pointing to the same string at the same offset: the `vdesc_name` at +8) puts them at
    `0xfffffe000b770900`, `...c30`, `...d30`, `...d70`, `...fb0`: the addresses the audit named. The port now leaves
    out 4 pairs, 1 stub and 1 reviewed record (`csproc_get_platform_path`, which no collection of either build binds);
  - `IODMACommand::Dispatch` (`0xfffffe000b328984`), `IOInterruptDispatchSource::metaClass` (`0xfffffe0007ed7e58`)
    and `vnop_default_desc` (`0xfffffe000b778b20`): 24B156's own matcher pairs each to the name 24A446's known-good list
    has; added to `pairs-24B156.json` with that in their `why`;
  - IOSerialFamily's two data imports (`0xfffffe000b77fe14`, 24A446's stub `0xfffffe000b727e14`; and
    `0xfffffe000b809168`, in 24A446 paired with `gIOKitDebugKey` by import order alone): on the stub, with 24A446's
    reason (IOSerialFamily's personalities are dropped, it never starts).
- **The collection** (`09df28336b6f8342`, 96,698,368 bytes): audit **0 failures** (2,710 paired, 93 functions on
  the stub, 10 data unpaired), `prelink stubs-check` **666 good, 0 bad**. The class layouts read again with this
  collection's own prelink report are the same bytes (the slide 0x1f000000000 holds). The phone's way, `oneios-tool
  prelink build` from the fork's `assets/kb-24B156/` alone (`oneios-tool app kb 24B156`), gives the same `kernel.kc`
  byte for byte. Not booted.

## What is not done

- **The pairing is incomplete.** A kext call to an original kernel function with no pair goes to
  `_unixnu_fileset_unmapped`, which panics with the caller; `prelink unmapped ADDRESS` names it and its paired
  neighbours, and the answer goes into the reviewed file. When the "from" address is inside our kernel, a kernel
  function made a virtual call on a kext's object: the slot names the method. Unpaired *data* keeps its
  collection address, which is some other object of our kernel: a wrong read, not a panic.
- **Inferred pairs can be wrong** (2 % of the order pairs on known functions; three seen in the boot so far, each
  now a reviewed record). XnuPrelink's own order rule also made one (`lck_mtx_gate_close`). A strange fault in a
  kernel function reached from a kext is worth one `prelink unmapped --target` look at the caller's pair.
- The class list (`abi/original-classes.json` of the iPad profile); `x18` use in kexts is not checked by default
  (`X18=1 build.sh`, slow).
- `prelink record` fails when `OUT` resolves outside the repository it runs from (a worktree with `out` as a symbolic
  link): `work/kernel.kc` is complete by then.
- The phone run of the Mac collection: "On the phone (2026-11-12, prelink)" above.
