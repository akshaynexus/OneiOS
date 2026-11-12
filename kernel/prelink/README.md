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

1. `native-input.py`: the kernel's weak-lookup binds resolved to its own definitions (Linux's ld64 left one,
   `native_source_storage_start`; Xcode 27.2's ld leaves seven: the `boot_progress_*` and `boot_log_export_put`
   defaults, `native_adreno_fences`, `native_ufs_dpu_map`), and the same for `REFERENCE`;
   XnuPrelink refuses an image with imports.
2. `roots.py`: which kexts (below).
3. `hosted.py`: the kexts for a kernel that is a process: `mrs/msr TPIDR_EL1` -> `TPIDR_EL0` (the current thread),
   `mrs xN, MPIDR_EL1` -> `ldr wN, [x18]` (the vCPU index), the packed-pointer `orr` of `smr_hash` (State below), before the pairing so the emitter's HMACs cover the
   rewritten code. The record lists every rewrite and every other instruction a process cannot execute.
4. `fileset_match.py` (XnuPrelink, `external/XnuPrelink`): pairs the functions and data of Apple's kernel that kexts
   reference with ours, by strings, call graphs, vtables and, with `REFERENCE`, names of the RELEASE build.
5. What the matcher left, inferred (each pair is listed in the report, `reviewed_applied.aligned`):
   - `align.py`: by link order, ours inside a source file with the reference's function of the same name as the
     shape (immediates, field offsets, named callees, strings), then the reference's own order. The error rate is
     measured on known pairs with their pairing hidden and written into the output (2 % today);
     `--explain ADDRESS` prints a window.
   - `structors.py`: constructors and base destructors of classes that kexts subclass, by the vtable they store, and
     factories (`X::withFoo`) by the class they allocate and the init slot they call.
   - `vtables.py`: every vtable paired by name, slot by slot (the public layouts are ABI). A slot overrides an
     order-inferred pair; its disagreements with other pairs are listed under `conflicts`.
6. `emit.py`: XnuPrelink's fileset emitter with our roots, the inferred pairs (slots and structors first) and the
   reviewed records, which win (`reviewed-24A446-v54.json`: functions, data, one pointer site, the AMFI
   initializer; each with evidence and a hash of the original bytes it was read from). Apple's kernel entry is
   replaced by ours, kexts stay where they are, the collection's header takes our kernel's CPU subtype (the loader
   compares them), and the kernel's `__DATA` becomes a root segment of its own (`__DATA_KERNEL`) at the end when it
   does not fit in front of the first kept kext's data (it is 10 MB with the virtio-gpu driver's static buffers).
7. `record.py`.

Before step 2, `merge-reviewed.py` merges the other agents' reviewed records into ours for the build (below).

`port-fixture.py` carried 50 of XnuPrelink's reviewed iPad identities over (functions with the same bytes here);
rerun it only when that fixture changes. `unmapped.py` names the original function behind a stub panic.

## Which kexts (`roots.py`)

Every kext of the collection is a root, except one all of whose IOKit personalities attach to hardware xnuhost
does not present (an Apple SoC block, I2C/SPI/SPMI part, RTBuddy/AFK/AOP endpoint, SEP endpoint, PCIe, USB,
Thunderbolt, accessory port), and seven decided by hand (`FORCE_OUT`: `kec.pthread`, the SEP keystore and its two
clients, the ANE HAL, FairPlayIOKit, and IOAudioCodecs, whose start waits for FairPlayIOKit). The profile's 31 roots are kept whatever their personalities. A left-out kext that a kept one
links against returns for its code only, without personalities. FairPlayIOKit is out although it is not hardware
(obfuscated calls through computed tables; reason in `roots.py`). Today (2026-11-12): 319 in the kernelcache, 130
roots, 150 in the closure (20 returned dependencies), 169 absent, 135 fileset entries (15 of the closure are codeless
KPIs). The counts of 2026-11-10 (133, 154, 139) were from before the last three `FORCE_OUT` entries.
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

A driver that subclasses an original class needs that class's addresses in this kernelcache: `classes.py` reads
them from the kexts' code (metaclass, size, parent, vtable and slots, constructors, destructors), and
`classes-24A446-v54.json` has them for IOAESAccelerator, IOCryptoAccelerator, IOMobileFramebufferLegacy,
IOHIDDevice, IOSurface, IOGPUDevice, IOGPU, IOAcceleratorES, IOGPUDeviceUserClient, and AGXG18P's AGXAccelerator
and AGXDeviceUserClient (for reference: AGXG18P itself is left out of the collection). `classes.py` also reads an
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
- `hosted.py` rewrites APFS's eleven inline `smr_hash_array_decode` (`orr xD, xN, #0x00ff000000000000` ->
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

The same command on macOS (python3 3.11+; Homebrew's keg-only `llvm` for `llvm-nm` and `llvm-mc`, which `build.sh`
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
Apple's own kernel, so more functions match by shape. Nothing of this collection has run. `lead/imports.py` (the
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
   `kernel/prelink/stubs-check.py KERNEL.KC` checks every stub against its `.original` (507 good on the Linux and the
   `9b950f2` collection, 0 on the `b74ed7c` one).
2. `eae11352...` (XnuPrelink `9b950f2`, the same kernel; all 507 stubs at their targets, 97 originals on the stub,
   10 data unpaired): 1.5 s in, after `BSD root`, the Data mount, the GPU add-on mount and `oneios-icons: the
   drawn-icon store had another generation`, `panic(cpu 0 ...): fileset: original component called an unmapped
   kernel function from 0x20038daa8c`. `0xfffffff00a8daa8c` is in `com.apple.security.sandbox`'s `__TEXT_EXEC`,
   called from `mac_posixsem_check_create` <- `sem_open` <- `native_gpu_addon_prepare` <- `load_init_program`. So
   Sandbox's posix-semaphore check calls an original kernel function that the Mac pairing left on the panic stub
   (the Linux pairing had it). Which one: `unmapped.py` on this collection. Not looked at further: the owner moved
   the audit of every import of the Mac collection against the Linux one to the agent `prelink`.

The phone's baseline stays the Linux-built `kernel.kc` of 2026-11-11 until that audit is done
(`docs/building.md`, "Test cycle on the phone"). The audit is the next section.

## Known-good pairs and the import audit (2026-11-12, prelink)

The pairing of a kernelcache's imports must not move when the toolchain that builds our kernel changes. Before this,
it did: `fileset_match.py` pairs by code shape, and the Xcode 27.2 kernel and reference have other shapes than the
Linux PC's. So the result of the pairing is now a committed list, and every build is checked against it.

- `pairs-24A446.json`: 2,710 known-good pairs (2,463 functions, 247 data): every kext import of the collection that
  is bound, original -> our symbol, each with its `why`. `"leaf": true` marks an original that only computes registers
  and returns (`mov w0, #45; ret`): any of our functions with the same instructions is right for it.
- `stubs-24A446.json`: the 94 originals left on the panic stub and the 9 data targets left unpaired, each with its
  reason, the kexts that import it and the count of call sites in their code.
- `emit.py --pairs --stubs` applies the list over the matcher, the inferred pairs and the reviewed records, and leaves
  the stub list unpaired. Names resolve to our kernel's **external** symbol: Xcode's kernel has local functions with
  the names of external ones (two `_kalloc_type_impl`, `_kalloc_type_var_impl`, `_kfree_type_var_impl`), and the old
  `setdefault` over all symbols could take the local one (it did for those three in a first build of this list; the
  audit caught it).
- `audit.py` (step 6b of `build.sh`, 3.5 s) reads every chained-fixup site of the kexts whose target is in Apple's
  kernel, the target it has in the built `kernel.kc`, and our kernel's symbol there. **The build fails** when an
  import is on the stub or unpaired without an entry in the stub list, when a stub-list entry is bound, when a bound
  import's symbol is not the list's (aliases at the same address agree; a leaf agrees by its instructions), and
  when a bound import is not in the list. `out/kernel/24A446/kernel.kc.binds.json` lists every import's binding.
  `--against OTHER.kc` prints the a/b/c comparison below for any two collections; `--draft FILE` writes a
  collection's pairs in the list's format for review (never over the committed list).
- A new pair (another kernelcache, a kext added to the roots, a kernel function renamed) fails the build until it
  is reviewed into the list with its `why`. A kernel without a listed symbol fails `emit.py` by name.

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
  not relocate (above). `stubs-check.py` covers it.
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
  unpaired, 0 audit failures, 507 `.original` stubs good. Against Linux: (a) 1 (`0xfffffe000b271b70`, above), (c) the
  7 decided for the Mac, the rest equal or leaf-equal.

### What is still on the stub (`stubs-24A446.json`)

Every one of the 94 functions and 9 data has callers in kext code; none was called in the Linux boot to the Home
Screen, where all of them were on the stub or unpaired too. That is the evidence, not a proof for every later path:
- 56 are Tightbeam, the exclaves IPC (`tb_*` names): this kernel has no `tb_*` function and no exclaves. Importers:
  AppleALSColorSensor, AppleFirmwareKit, AppleH16ANEInterface, ExclavesAudioKext, on their exclave paths.
- 8 are imported only by kexts that never start (personalities dropped: RTBuddy, AppleSEPManager, ApplePTD,
  AppleEmbeddedAudio, AppleHIDTransport, IOSerialFamily, AppleSMC).
- 38 are not chosen yet: the names between their neighbours in the import tables are several (`imports.py` prints
  them) and the code has not told them apart. A stub panic from one of these names it (`unmapped.py`); its pair goes
  into the list.

### Hot spots and timings (for the later Rust port)

On the M4: one `match_images` (the original against the reference) takes 20 s, the matcher with a reference runs two
(the native kernel's and the reference's; about 40 s by that, not timed on its own; reused while its inputs are the
same). In it: function digests and string tables of
22,000 + 27,000 functions, and the length LCS of `anchor_reference_order` (a table of up to 40,000 cells per gap,
four rounds). `kc_image.Image.fixups()` decodes 592,421 chained fixups of the kernelcache in about 1 s (pure Python
`struct` per pointer); `audit.py` decodes three such tables and scans the stubbed slots' kexts for callers: 3.5 s.
A warm build (pairing reused) took 19 s on 2026-11-12: the inputs and `hosted.py` 2 s, the two `align.py` runs 4 s,
`kclasses.py`, `structors.py` and `vtables.py` 2 s, `emit.py` 8 s (XnuPrelink's emitter: layout, the fixup chains,
the HMACs), `audit.py` 3 s, the record and the chunk hashes 1 s.

## Records of several agents, and the import-order check

- `reviewed-24A446-v54.json` is fable-prelink's. The lead keeps `lead/reviewed-lead-24A446-v54.json` and fable-user
  `user/reviewed-user-24A446-v54.json` (functions only). `build.sh` merges them for each build
  (`merge-reviewed.py`, into `work/reviewed.merged.json`): the owner's record wins for an original both have, and
  every disagreement is printed and written under `merge.disagreements`. There is none today: the three of
  2026-11-11 were settled with the lead by the code (`0xfffffe000abd8788` is `lck_ticket_destroy`,
  `0xfffffe000ae04840` is `decmpfs_hides_xattr`, `0xfffffe000ade775c` tests I/O policy bit 15 and is bound to
  UniXNU's `vfs_context_iopolicy_unassigned_0x8000`; the lead dropped its records in `99fbf4b`).
- **Import order** (the lead's `lead/imports.py`): a kext's `__auth_got` is sorted by imported symbol name, in runs
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
  are the candidates (`imports.py` prints them); the code picks among them. A boot on the PC costs about ten
  minutes to the first user process, so a batch is paired this way and then confirmed with one boot.

## What is not done

- **The pairing is incomplete.** A kext call to an original kernel function with no pair goes to
  `_unixnu_fileset_unmapped`, which panics with the caller; `unmapped.py ADDRESS` names it and its paired
  neighbours, and the answer goes into the reviewed file. When the "from" address is inside our kernel, a kernel
  function made a virtual call on a kext's object: the slot names the method. Unpaired *data* keeps its
  collection address, which is some other object of our kernel: a wrong read, not a panic.
- **Inferred pairs can be wrong** (2 % of the order pairs on known functions; three seen in the boot so far, each
  now a reviewed record). XnuPrelink's own order rule also made one (`lck_mtx_gate_close`). A strange fault in a
  kernel function reached from a kext is worth one `unmapped.py --target` look at the caller's pair.
- The class list (`abi/original-classes.json` of the iPad profile); `x18` use in kexts is not checked by default
  (`X18=1 build.sh`, slow).
- `record.py` fails when `OUT` resolves outside the repository it runs from (a worktree with `out` as a symbolic
  link): `work/kernel.kc` is complete by then.
- The Mac collection with the known-good pairs: see "Known-good pairs" for its phone run.
