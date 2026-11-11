# kernel/prelink: `kernel.kc`

Owner: fable-prelink. The hosted XNU (`kernel/build-hosted.sh`) prelinked with the kexts of the iPhone18,2
iOS 27.0.1 (24A446) kernelcache, as one `MH_FILESET` that xnuhost loads.

```sh
scripts/bg.sh prelink env REFERENCE=out/kernel/24A446/reference/mach.release.bcm2837.unstripped.noctf kernel/prelink/build.sh
```

| Input | From |
|---|---|
| `out/kernel/current-hosted/xnu.macho` (`KERNEL=FILE` for another) | `kernel/build-hosted.sh` |
| `out/kernel-inputs/24A446/kernelcache.macho` | `scripts/image/kernel-inputs.sh` (the profile pins its SHA-256) |
| `REFERENCE=FILE`, optional but worth 4,800 more paired functions | `XNU_FILESET_REFERENCE=1 HOSTED_OUT=<you> kernel/build-hosted.sh`: the same source as a RELEASE kernel with LTO and symbols, left in the tree's `build/xnu/fileset-reference/`. Never booted; it need not be rebuilt for each kernel |

| Output (`out/kernel/24A446/`) | |
|---|---|
| `kernel.kc` | the collection, 93 MB. Made from Apple's kexts: **never committed, never released** |
| `kernel.kc.build.json` | the record: inputs and hashes, roots, each kext left out with its reason, the fileset entries, the rewrites, the AMFI pins, counts |
| `kernel.kc.prelink.json` | the emitter's report: per kext, the original kernel functions bound to the panic stub (`stubbed`) and the data left unpaired (`missing`) |

A build takes about 80 s (45 s of it the pairing, reused while the kernel, the kernelcache and the reference are
the same). A new kernel needs a new `kernel.kc`: every native address in it is that kernel's.

## Steps (`build.sh`)

1. `native-input.py`: the kernel's one weak bind (`native_source_storage_start`) resolved to its own definition;
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
(obfuscated calls through computed tables; reason in `roots.py`). Today: 319 in the kernelcache, 133 roots, 154 in
the closure (21 returned dependencies), 165 absent, 139 fileset entries (15 of the closure are codeless KPIs).
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
and 18 data targets are unpaired; `kernel.kc.build.json` lists them per kext.

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
- No run on the phone.
