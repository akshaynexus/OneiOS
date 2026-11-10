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
5. `align.py`: what the matcher left, by link order (ours inside a source file, then the reference's). Inferred,
   with the error rate measured on known pairs in its output; `--explain ADDRESS` prints a window.
6. `emit.py`: XnuPrelink's fileset emitter with our roots, the aligned pairs and the reviewed records
   (`reviewed-24A446-v54.json`: functions, data, one pointer site, the AMFI initializer; each with evidence and a
   hash of the original bytes it was read from). Apple's kernel entry is replaced by ours, kexts stay where they
   are, the collection's header takes our kernel's CPU subtype (the loader compares them).
7. `record.py`.

`port-fixture.py` carried 50 of XnuPrelink's reviewed iPad identities over (functions with the same bytes here);
rerun it only when that fixture changes. `unmapped.py` names the original function behind a stub panic.

## Which kexts (`roots.py`)

Every kext of the collection is a root, except one all of whose IOKit personalities attach to hardware xnuhost
does not present (an Apple SoC block, I2C/SPI/SPMI part, RTBuddy/AFK/AOP endpoint, SEP endpoint, PCIe, USB,
Thunderbolt, accessory port), and five decided by hand (`FORCE_OUT`: `kec.pthread`, the SEP keystore and its two
clients, the ANE HAL). The profile's 31 roots are kept whatever their personalities. A left-out kext that a kept one
links against returns for its code only, without personalities. Today: 319 in the kernelcache, 134 roots, 154 in the
closure (20 returned dependencies), 165 absent, 139 fileset entries (15 of the closure are codeless KPIs).
IOGPUFamily and what it links are roots; AGXG18P (the GPU's own driver) is left out, so NativeAdreno's
`AGXAccelerator` binding for this kernelcache is not staged yet (`docs/gpu.md`).

## The board's drivers

xnuhost's virtio block, input and GPU guest drivers are **not** kexts: they are archives linked into `xnu.macho` by
the kernel build (`0005-gpu-3d` is in; storage and HID are `kernel/patches/oneios-hosted/storage` and Q4 of the
board). The prelink takes whatever kernel it is given; a driver that subclasses an original class
(`IOMobileFramebufferLegacy`, `IOHIDDevice`) needs that class's addresses in this kernelcache, which are not derived
here yet.

## AMFI and launch constraints

- `early_security_init`: AMFI's kmod start does nothing here (NULL `_realmain`); its policy registration is the
  eight-instruction wrapper at `0xfffffe00092329cc`, which `OSKext::start` calls through
  `_NativePrelinkEarlySecurityInit` (`native-security-init.patch`). Evidence in the reviewed file.
- AMFI's `PAGE_SHIFT_CONST` slot (`0xfffffe000812db68`) is bound to `unixnu_kernel_stack_shift`: the CoreEntitlements
  stack check assumes 16 KiB-aligned stacks.
- Launch constraints are the kernel's: `zzyc-owner-skip-launch-constraints.patch` (boot argument `sheng-no-lc=1`)
  and `/chosen/amfi-only-platform-code`. Nothing in the collection changes for them.

## What is not done

- **The pairing is incomplete.** A kext call to an original kernel function with no pair goes to
  `_unixnu_fileset_unmapped`, which panics with the caller; `unmapped.py ADDRESS` names it, and the answer goes into
  the reviewed file. The record has the counts. Unpaired *data* keeps its collection address, which is some other
  object of our kernel: a wrong read, not a panic.
- The class list (`abi/original-classes.json` of the iPad profile) and the `AGXAccelerator` binding for this
  kernelcache.
- No run on the phone.
