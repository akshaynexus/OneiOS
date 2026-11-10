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
   `mrs xN, MPIDR_EL1` -> `ldr wN, [x18]` (the vCPU index), before the pairing so the emitter's HMACs cover the
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
IOGPUFamily and what it links are roots; AGXG18P (the GPU's own driver) is left out, so NativeAdreno's
`AGXAccelerator` binding for this kernelcache is not staged yet (`docs/gpu.md`).

## The board's drivers

xnuhost's virtio block, input and GPU guest drivers are **not** kexts: they are archives linked into `xnu.macho` by
the kernel build (`0005-gpu-3d` is in; storage and HID are `kernel/patches/oneios-hosted/storage` and Q4 of the
board). The prelink takes whatever kernel it is given; a driver that subclasses an original class
(`IOMobileFramebufferLegacy`, `IOHIDDevice`) needs that class's addresses in this kernelcache, which are not derived
here yet.

A driver that subclasses an original class needs that class's addresses in this kernelcache: `classes.py` reads
them from the kexts' code (metaclass, size, parent, vtable and slots, constructors, destructors), and
`classes-24A446-v54.json` has them for IOAESAccelerator, IOCryptoAccelerator, IOMobileFramebufferLegacy,
IOHIDDevice, IOSurface, IOGPUDevice, IOGPU and AGXAccelerator (AGXG18P itself is left out of the collection).
The archive reaches an original function through a native stub `bti c; b` with an N_ABS symbol
`<stub>.original` at the linked address; an absolute address in a literal is wrong under xnuhost's slide.

**The boot needs two of them now**: `IOAESAccelerator` (Droid-VM's SoftwareAES/NativeAES: AppleImage4's start waits
for it and holds IOResources' matching, so no disk gets its BSD client and the root volume never appears) and the
keystore (`AppleKeyStoreState`: AMFI and the Data volume).

## AMFI and launch constraints

- `early_security_init`: AMFI's kmod start does nothing here (NULL `_realmain`); its policy registration is the
  eight-instruction wrapper at `0xfffffe00092329cc`, which `OSKext::start` calls through
  `_NativePrelinkEarlySecurityInit` (`native-security-init.patch`). Evidence in the reviewed file.
- AMFI's `PAGE_SHIFT_CONST` slot (`0xfffffe000812db68`) is bound to `unixnu_kernel_stack_shift`: the CoreEntitlements
  stack check assumes 16 KiB-aligned stacks.
- Launch constraints are the kernel's: `zzyc-owner-skip-launch-constraints.patch` (boot argument `sheng-no-lc=1`)
  and `/chosen/amfi-only-platform-code`. Nothing in the collection changes for them.

## State (2026-11-10)

Under xnuhost in `qemu-aarch64 -cpu cortex-a710` (1 vCPU, 2 GiB), with a kernel that has
`0007-prelink-kext-start.patch`: Apple corecrypto's POST, AppleImage4, AMFI (early initializer), the sandbox and
vnguard policies load, `trust_cache_runtime_init` passes ("no external trust caches found"), IOKit matching starts
the kept drivers, and `bsd_init` begins ("MAC Framework successfully initialized", buffer headers). It stops two
seconds later at the next unpaired kernel function (IOTimeSyncFamily).

Pairing: 11,461 functions by the matcher with the reference, 984 inferred (518 by order, 163 structors and
factories, 327 by slot, less the overlaps), 143 reviewed by hand plus 4 data and one site, 50 carried over from
XnuPrelink's iPad fixture.
655 original functions that kexts reference are still on the panic stub (most only through vtables of classes
whose vtable is not named yet), and 55 data targets are unpaired.

## What is not done

- **The pairing is incomplete.** A kext call to an original kernel function with no pair goes to
  `_unixnu_fileset_unmapped`, which panics with the caller; `unmapped.py ADDRESS` names it and its paired
  neighbours, and the answer goes into the reviewed file. When the "from" address is inside our kernel, a kernel
  function made a virtual call on a kext's object: the slot names the method. Unpaired *data* keeps its
  collection address, which is some other object of our kernel: a wrong read, not a panic.
- **Inferred pairs can be wrong** (2 % of the order pairs on known functions; three seen in the boot so far, each
  now a reviewed record). XnuPrelink's own order rule also made one (`lck_mtx_gate_close`). A strange fault in a
  kernel function reached from a kext is worth one `unmapped.py --target` look at the caller's pair.
- The class list (`abi/original-classes.json` of the iPad profile) and the `AGXAccelerator` binding for this
  kernelcache; `x18` use in kexts is not checked by default (`X18=1 build.sh`, slow).
- No run on the phone.
