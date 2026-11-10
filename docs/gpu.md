# GPU: virtio-gpu 3D on the phone's Adreno

Owner: fable-gpu. Date: 2026-11-10. The Metal path of OneiOS, as Droid-VM runs it on the same SoC: the guest's
Metal plug-in runs Turnip, Turnip speaks the drm/msm protocol over virtio-gpu's DRM native context, and the host
serves that protocol on `/dev/kgsl-3d0` with Droid-VM's `drm2kgsl`.

**State, plainly.** The host side is written, builds for Android and passes its tests on the PC. Nothing here has
run on the phone: no phone was on adb. `gpuctl` (section 4) is the first thing to run there. The guest's kernel
driver builds into the hosted kernel. Metal's kernel side is linked (2026-11-11, native-adreno; section 5):
NativeAdreno's `AGXAccelerator` under the collection's `IOAcceleratorES` and its `AGXDeviceUserClient` over
`droidvm_virtgpu_call`. On the PC it registers, IOKit matches it and the Metal plug-in's connection opens from
inside the kernel; qemu has no KGSL, so no 3D context, blob or submit has run anywhere. The Metal plug-in is not
built (section 6).

## 1. The path

```
iOS process: Metal -> NativeAdrenoMetal.bundle (NativeMetal, metal2vulkan, Turnip)          userland/, not built
   | IOKit: AGXAccelerator -> AGXDeviceUserClient, open type 0x4149, DROIDVM_VIRTGPU_*       gpu/ archive, opens on the PC
hosted XNU: DroidVMVirtioGpu3D.cpp (droidvm_virtgpu_call) -> DroidVMVirtioGpu.cpp            0005, builds
   | virtio-mmio: /arm-io/virtio-gpu, control queue
xnuhost: src/virtio_gpu.c (the device) -> src/gpu3d.h -> src/gpu3d_kgsl.c                    written, tested on the PC
   | function calls, same process
drm2kgsl (external/virglrenderer src/drm/drm2kgsl, linked in) -> ioctl on /dev/kgsl-3d0      not run on the phone
```

## 2. Host side (`runtime/xnuhost`)

| File | What |
|---|---|
| `src/virtio_gpu.c` | The device. 2D as before (the first frame still comes through it). With a renderer: features VIRGL, RESOURCE_BLOB, CONTEXT_INIT, CREATE_GUEST_HANDLE; one capset (6, DRM); `CTX_CREATE/DESTROY`, `CTX_ATTACH/DETACH_RESOURCE`, `RESOURCE_CREATE_BLOB`, `RESOURCE_MAP_BLOB` (answered `MAP_INFO_POOL` + the offset in the pool), `RESOURCE_UNMAP_BLOB`, `SUBMIT_3D`, `SET_SCANOUT_BLOB` |
| `src/gpu3d.h` | What the device asks of a renderer (`xh_gpu3d`), so the tests can put a recording one there |
| `src/gpu3d.c` | `xh_gpu3d_udmabuf`: guest physical segments of the RAM memfd as one dma-buf (`UDMABUF_CREATE_LIST`) |
| `src/gpu3d_kgsl.c` | The renderer: drm2kgsl, with what virglrenderer.c and crosvm's rutabaga are around it in a DroidVM |
| `drm2kgsl/config.h` | The `config.h` of the virglrenderer sources the Makefile compiles |
| `tools/gpuctl.c` | The renderer alone, for the phone (section 4) |

**drm2kgsl is linked in, statically. There is no helper process.** The Makefile compiles sixteen sources of
`external/virglrenderer` (Droid-VM's fork at the pin of `scripts/setup/get-references.sh`): the renderer, the DRM
context and fence code, the context and resource tables and the mesa utilities under them. `virglrenderer.c` itself
is left out: it needs the GL renderer (epoxy, EGL) and libdrm, which a static Android binary has not. The three
symbols it wants from Android's liblog and mesa's `os_misc.c` are in `gpu3d_kgsl.c` and go to xnuhost's log.
Without `external/virglrenderer` xnuhost still builds, with the 2D device, and says so at start.

**Memory.**
- The RAM memfd is now created sealable and sealed against shrinking (`mem.c`): udmabuf takes no other memfd.
- Guest pages: every buffer object is the guest's own memory (`MSM_BO_GUEST_ALLOC`, a `HOST3D_GUEST` blob with
  `CREATE_GUEST_HANDLE`). The device checks the segments (guest RAM, whole 4 KiB pages, at least the blob's size),
  the renderer makes one udmabuf of them, hands it to the context and KGSL imports it and binds it at the GPU
  address the guest chose. Nothing is copied.
- The pool: 8 MiB of the memfd between the kernel's window and the boot framebuffer (`main.c` `GPU_POOL_BYTES`),
  named to the guest as `/arm-io/virtio-gpu` `drm-host-pool` (base, size) and to drm2kgsl as its arena. It holds each
  context's 16 KiB response buffer; drm2kgsl keeps its first 2 MiB unused. The kernel's window is 8 MiB smaller
  for it unless `--gpu 2d`.

**Threads and fences.** With a renderer the control queue is served by one thread of the device, never by a vCPU's
trap handler. A command fenced on a context's ring (`FLAG_FENCE | FLAG_RING_IDX`) is answered at once but its chain
goes back to the guest only when the ring passes the fence (drm2kgsl's timeline threads call back); a fence on the
device's own ring is returned at once. A context that is destroyed returns its held commands. A device reset
destroys the contexts and blobs on the device's thread and drops what was held.

**Display.** `SET_SCANOUT_BLOB` makes a guest blob the scanout; `RESOURCE_FLUSH` copies its rows from the guest's
pages to the display (a CPU copy per frame: crosvm's zero-copy import to the compositor is not done here).
The guest side is `kernel/patches/oneios-hosted/display/` (its README): iOS's framebuffer hands each of
`backboardd`'s surfaces to this device, as a guest blob with a renderer and as a 2D resource over the surface's
pages without one (the PC). What the 3D path still needs from the display is listed there.

**Options.** `--gpu auto` (default: 3D when `/dev/kgsl-3d0`, `/dev/udmabuf` and `/dev/dma_heap/system` open and
KGSL takes the probe), `--gpu 3d` (or do not start), `--gpu 2d`. The reason for 2D is in the log (`gpu: 2D only: ...`).
drm2kgsl's own switches are environment variables it reads (`DRM2KGSL_PRIORITIES`, `CROSVM_DRM2KGSL_DIAG=1`).

**Session rules** (`docs/booter.md` 2.9): nothing new outside the process. KGSL contexts, the VBO and the dma-bufs
are descriptors of xnuhost and go with it at SIGKILL; drm2kgsl's fence threads are ordinary threads.

## 3. Tests on the PC

`make -C runtime/xnuhost test` (also `runtime/build.sh --test`):

- `test_virtio_gpu3d` in `tests/test_xnuhost.c`, the device against a recording renderer: capsets, contexts, the
  three kinds of blob with good and bad segments, `MAP_BLOB`'s pool answer, `SUBMIT_3D` bytes reaching the renderer
  whole, fences held and retired in order per ring, a context destroyed with a command held, a blob as the scanout,
  the 2D commands beside them, a device reset. Then a generated stream: 4000 commands of every type and of none,
  with random lengths, ids, flags and segments; each gets exactly one answer or error, under ASan and UBSan.
  `XNUHOST_TEST_SEED=N` changes the stream (144 seeds were run).
- `xh_gpu3d_udmabuf` against the real `/dev/udmabuf` of the PC: three segments of a sealed memfd read back in order
  through the dma-buf, a write seen in the memfd, an unsealed memfd refused.
- The loader's `drm-host-pool` property (`runtime/loader` tests).
- `check-arm64`: the Android binary with the 3D code linked still boots the test kernel under `qemu-aarch64` (2D:
  no KGSL there).

Not tested anywhere: `gpu3d_kgsl.c` and drm2kgsl against a KGSL device. That is `gpuctl`.

## 4. `gpuctl`: the check on the phone

Built by `runtime/build.sh STAGE` as `STAGE/bin/gpuctl` (static). As root on the S24 Ultra:

```sh
adb push STAGE/bin/gpuctl /data/local/tmp/gpuctl
adb shell su -c 'chmod 755 /data/local/tmp/gpuctl; /data/local/tmp/gpuctl'        # nodes, probe, submit
adb shell su -c '/data/local/tmp/gpuctl submit -v'                                # with drm2kgsl's own log lines
```

`gpuctl nodes` opens the three device nodes and prints udmabuf's limits. `gpuctl probe` opens KGSL through drm2kgsl
and prints the capset a guest would get (expect chip `0x43051400`, the Adreno 750). `gpuctl submit` then does what
one guest context does: the context, its response buffer from the pool, `GET_PARAM` answered there, 16 KiB of its
own "guest" memory imported through udmabuf and bound at a GPU address, a submit queue, one command buffer of 32
`CP_NOP`s run by the GPU, and its fence retired. The last line is `ok` or `FAILED at: <step>: <reason>`. It leaves
nothing behind.

What a failure means:

| Step | Likely cause |
|---|---|
| a node does not open | SELinux domain or permissions of the shell; run it from the session's domain |
| `udmabuf over the RAM memfd` | the kernel's udmabuf refuses the memfd (seal) or `list_limit`/`size_limit_mb` |
| `KGSL refused the probe` | no `KGSL_MEMFLAGS_VBO` on this kernel: drm2kgsl cannot place buffers at guest addresses |
| `the buffer's pages` | KGSL does not import a udmabuf (`IOCTL_KGSL_GPUOBJ_IMPORT`), or the bind into the VBO failed |
| `the fence retiring` | the command did not run, or this KGSL lacks the timestamp wait drm2kgsl uses |

Limits to read off the `nodes` output: a guest blob is one udmabuf, so at most `list_limit` runs of adjacent pages
and `size_limit_mb` MiB (the kernel defaults are 1024 and 64; the guest allocates in 256 KiB pieces, so 64 MiB
blobs fit the count, and a larger blob needs `size_limit_mb` raised as root).

## 5. Guest kernel (`kernel/patches/oneios-hosted`)

Delivered in the hosted kernel, not as a kext:

- `0005-gpu-3d.patch`: under `#if XNU_HOSTED`, `oneios_board_late_start` calls `oneios_gpu3d_start` (weak default).
- `gpu/build-oneios-gpu.py`: a board archive of the hosted build only (`kernel/build-hosted.sh` copies it into the
  build's device repository and adds `liboneiosgpu.a` to the force-loaded archives). It compiles `gpu/OneiOSGpu.cpp`
  and, unchanged from `external/UniXNU-droidvm/package/`, `DroidVMVirtioGpu.cpp` (the transport) and
  `DroidVMVirtioGpu3D.cpp` (contexts, blobs over guest pages, fenced submissions, `droidvm_virtgpu_call`).
- The transport is compiled with `DROIDVM_VIRTIO_GPU_DYNAMIC=1`: its queues and command buffers (about 9 MB) are one
  wired allocation at start, not kernel bss. With them in bss the kernel's `__DATA` was 10.4 MB and did not fit the
  collection's stock layout (fable-prelink); it is 1.6 MB now.
- Boot arguments: `vgpu-3d=0` leaves the 3D half off; `vgpu-selftest=1` runs one context through the host at boot
  and logs the stage it reached (`oneios-gpu: selftest stage 8 ...` is the whole way: capset, context, response
  buffer mapped from the pool, `GET_PARAM(GPU_ID)` answered).

Checked: `HOSTED_OUT=gpu kernel/build-hosted.sh` from scratch builds and seals the kernel with the archive
(`_oneios_gpu3d_start`, `_droidvm_virtgpu_call`, `DroidVMVirtioGpu::start` defined in `xnu.macho`).

### 5.1 Metal's kernel side: NativeAdreno (native-adreno, 2026-11-11)

In the same archive (`liboneiosgpu.a`; `ONEIOS_GPU_ADRENO=0` in the build's environment leaves it out and only
compiles it; `build/oneios-gpu/adreno/status.json` says which). No kernel patch links it: `oneios_gpu3d_start` calls
`oneios_adreno_start`. `0024-gpu-pool.patch` is the one kernel change (below).

| Object | From | What |
|---|---|---|
| `NativeAdrenoPublish`, `NativeAdrenoUserClient`, `NativeAdrenoSubmit`, `NativeAdrenoMemory`, `NativeAdrenoCpu`, `AdrenoPM4.kit` | `external/NativeAdreno`, `external/AdrenoPM4`, built by Droid-VM's `build-droidvm-gpu.py` run up to its accelerator step (its edits to Publish and Memory included) | `AGXDeviceUserClient` (the user client), the published `NativeAdrenoGpu` service, execbuf and memory |
| `DroidVMGpu` | `external/UniXNU-droidvm/package/DroidVMGpu.cpp`, from a staged copy with four changes (`STAGED` in `build-oneios-gpu.py`) | NativeAdreno's hardware classes never attached; the publish; the accelerator's registration |
| `NativeAdrenoAccelerator.late` | `external/NativeAdreno/NativeAdrenoAccelerator.cpp` with `external/NativeMetal/OriginalAcceleratorABI.h` | `AGXAccelerator`, a subclass of the collection's `IOAcceleratorES`; its static initializer is out of `__mod_init_func` and runs after IOGPUFamily has loaded |
| `IOAcceleratorES-bindings`, `IOAcceleratorES-originals` | generated from `gpu/IOAcceleratorES-24A446-v54.json` | the parent's constructor and base destructor as stubs `bti c; b <name>.original`, its metaclass as an absolute symbol |
| `OneiOSAdreno` | `gpu/OneiOSAdreno.cpp` | the start, the bound-parent check, the superclass slide, the self-test, weak stand-ins for sheng's display hand-off and log export |

**The binding.** Apple's `AGXG18P` is not in the collection, so the names `AGXAccelerator` and `AGXDeviceUserClient`
are free. The parent is `IOAcceleratorES` of `com.apple.iokit.IOGPUFamily` (a root of `kernel.kc`), read from the
iPhone18,2 kernelcache by `kernel/prelink/classes.py`: metaclass `0xfffffe000b976c78`, 136 bytes, 168 slots of which
only 0, 1 and 7 are its own (so a plain `IOService`, as `OriginalAcceleratorABI.h` declares and as on the iPad);
the protected constructor at `0xfffffe000a04bc50` and the base destructor at `0xfffffe000a04bcc0` (0x78 and 8
before the complete destructor; read from the code: the constructor calls IOService's with the caller's metaclass
and stores the vtable, the destructor is `bti c; b IOService::~IOService`). The build refuses a class of another
shape. XnuPrelink retargets the two stubs (`kernel.kc.prelink.json`, `native_original_branches`, owner
`com.apple.iokit.IOGPUFamily`). The metaclass symbol's value is the link address, so under xnuhost's slide
`OneiOSAdreno.cpp` checks that the live `IOAcceleratorES` is the bound one and gives `AGXAccelerator`'s metaclass the
slid superclass before `OSMetaClass::postModLoad` (as `aes/` and `display/` do). The three XNU hooks Droid-VM's
board needs are already in this kernel's board series: `OSMetaClassAdoptSuperclass` (the accelerator's chain becomes
`AGXAccelerator -> IOGPU -> IOAcceleratorES`, the user client's `AGXDeviceUserClient -> IOGPUDeviceUserClient`, so the
sandbox profiles match them as Apple's), the sandbox grant (`NativeSandboxGrantProcess`), `native_pmap_user_mappings`.

**The start.** `oneios_board_late_start` (bsd_init) -> `oneios_gpu3d_start` -> `oneios_adreno_start` ->
`sheng_gpu_power_probe` (DroidVMGpu.cpp), which publishes NativeAdreno, loads IOGPUFamily and registers
`AGXAccelerator` on the platform expert with `MetalPluginName = NativeAdrenoMetal`,
`MetalPluginClassName = NativeAdrenoMetalDevice`, `IOGLESBundleName = AppleMetalGLRenderer` and
`NativeSandboxGrantProcess`. It runs on bsd_init's thread, not on a thread of its own as in Droid-VM: registering a
class holds OSMetaClass's lock until `postModLoad`, which takes the kext lock, and a kext load on another thread
holds the kext lock and waits for the class lock. With the display archive loading IOMobileGraphicsFamily right
after this start, the first boot of the two together hung at accelerator stage 2. (Every archive that registers a
class this way, `aes/`, `keystore/`, `display/` and this one, has that window against a kext load on some other
thread; none was seen.) Boot arguments:

| Argument | |
|---|---|
| `oneios-gpu-metal=0` / `=1` | never / always publish. Not given: published when the host's virtio GPU offers 3D (`oneios_gpu3d_state` 3), so not on the PC. Droid-VM's `droidvm-gpu-metal` is read when this one is absent |
| `droidvm-gpu-clients=a,b,...` or `*` | the processes besides `backboardd` granted the user client (DroidVMGpu.cpp `grantedProcesses`) |
| `vgpu-3d=1` | what `GETPARAM(DEFAULT_3D)` answers the plug-in (DroidVMVirtioGpu3D.cpp reads it with default 0; `vgpu-3d=0` also turns the driver off) |
| `oneios-adreno-selftest=1` | after the publish: IOKit's match of `IOAcceleratorES`, an in-kernel open with type `0x4149`, `GETPARAM(ABI_VERSION)` through the tunnel; two `oneios-adreno: selftest:` lines |

**The user-client interface** (what `NativeAdrenoMetal.bundle` and Turnip's vdrm backend call; nothing here differs
from Droid-VM's, the header is `external/NativeAdreno/DroidVMVirtgpuABI.h`):

1. `IOServiceGetMatchingService(IOServiceMatching("IOAcceleratorES"))`; the service's class is `AGXAccelerator`.
   Metal's own discovery reads `MetalPluginName` and loads `/System/Library/Extensions/NativeAdrenoMetal.bundle`,
   class `NativeAdrenoMetalDevice`.
2. `IOServiceOpen(service, mach_task_self(), 0x4149, &connection)`: an `AGXDeviceUserClient` for virtgpu alone (one
   connection is one DRM context). Type `0x4147` (NativeAdreno's own KGSL-style client) is refused here: there is no
   hardware behind it. Type `0x4148` (display hand-off) opens and answers through the display archive's presenter.
   Types `0x4147xxxx` with nonzero `xxxx` are progress markers, not connections (`kIOReturnUnsupported` by design).
3. Every call is `IOConnectCallStructMethod(connection, any selector, in, inSize, out, &outSize)` with `in` =
   `{uint32 'NTNL' (0x4e544e4c), uint32 method}` followed by the op's record, `method = 0x7200 + op`
   (`DROIDVM_VIRTGPU_GETPARAM` 0 ... `CREATE_BLOB_USER` 9, and DroidVMVirtioGpu3D.cpp's two after them), and `out`
   exactly the op's reply size. Input over 4096 bytes arrives as a memory descriptor (the same header first; at most
   65536 + 64 bytes).
4. `MAP` returns an address in the caller's task: the context's response buffer (from the host's pool) or a blob's
   guest pages. `EXECBUFFER` with `DROIDVM_VIRTGPU_EXEC_FENCE` answers a fence number, `WAIT_FENCE` waits for it.
5. Closing the connection (or the process dying) destroys the context and unmaps what it mapped.

**`0024-gpu-pool.patch`.** The pool (`/arm-io/virtio-gpu` `drm-host-pool`) is 8 MiB of the host's RAM object between
the kernel's window and the boot framebuffer. The driver maps a context's response buffer from it with
`IOMemoryDescriptor::withPhysicalAddress`, into the client's task and (self-test) at a kernel address. The hosted
pmap handed only window pages to the host's `task_map` / `kernel_map`; the patch adds the pool's range
(`hosted_pa_in_gpu_pool`: from the end of the window to the boot framebuffer, at most 64 MiB), as `0020` did for
the boot framebuffer. copyin/copyout on a pool page still fail (it is not behind `phystokv`).

**Tested on the PC** (`HOSTED_OUT=adreno`, the series through `0024`, display and key store archives linked,
`kernel.kc` from `kernel/prelink/build.sh`, `qemu-aarch64`, 8 vCPUs, boot arguments `oneios-gpu-metal=1
oneios-adreno-selftest=1`):

```
oneios-gpu: the host offers no 3D (features 0x100000000): display only
droidvm-gpu: published for the virtio GPU
droidvm-gpu: Metal accelerator stage 5 status 0
oneios-adreno: selftest: IOKit matched IOAcceleratorES: class AGXAccelerator (parent IOGPU), 136 bytes, MetalPluginName NativeAdrenoMetal, registered 1, chains 5
oneios-adreno: selftest: user client AGXDeviceUserClient opened with type 0x4149; GETPARAM(ABI_VERSION) through the tunnel: status 0, value 1 (the 3D device is not there: gpu state 2)
```

Stage 5 is "registered"; chains 5 is both class chains adopted. The same boot went on through the display's start
and the key store to `hello, launchd` (7 minutes; stopped there by its timeout, before `backboardd`). The kernel
booted had the series through `0022` and the pool patch; the full series with the lead's `0023` and `0024` was
then built from scratch and not booted. **Not tested:** anything past `GETPARAM`
(`CONTEXT_INIT`, blobs, `MAP`, `EXECBUFFER`, fences: they need the host's renderer, and qemu has no KGSL); the pool
mapping of `0024` (no pool property without a renderer); an open from a user process (the sandbox's verdict, the
grant, the user client's method filter); the accelerator's destructor path; anything on the phone. The static
member `AGXAccelerator::superClass` keeps the parent's unslid link address (nothing read it in this boot).

## 6. Not done

1. **Anything on the phone.** Run `gpuctl` first; then `xnuhost --gpu 3d` with a kernel that has `0005` and
   `vgpu-selftest=1` in the boot arguments.
2. **The Metal user client in the kernel: linked and opened in the kernel on the PC** (section 5.1). Left: an open
   from a user process, and every call that needs the host's renderer. Kernel imports of the kexts on Metal's path
   that are still on the panic stub (2026-11-11; fable-prelink's to pair, `unmapped.py --target` describes each):
   IOGPUFamily `0xfffffe000ac24ef4` (reads the current thread), `0xfffffe000b34f304` (among IOMemoryDescriptor's,
   also imported by IOMobileGraphicsFamily), `0xfffffe000b386e00` (before `IOCircularDataQueueCreateWithEntries`);
   IOSurface `0xfffffe000b3490e8` (among IOGeneralMemoryDescriptor's), `0xfffffe000b350780` (after
   IOMultiMemoryDescriptor's); IOMobileGraphicsFamily `0xfffffe000b335ec4` (five arguments, between
   IOFilterInterruptEventSource and IOTimerEventSource), `0xfffffe000b3597a8` (a platform-expert call before
   `PEHaltRestartInternal`). NativeAdreno itself calls kext code only through the two bound stubs; the plug-in's
   IOSurface use and IOGPUFamily's own user clients can reach the others.
3. **The Metal plug-in.** `userland/roottweak` already places `out/userland/NativeAdrenoMetal.bundle` at
   `/System/Library/Extensions/` and records it in the contract (`metal`); the bundle itself (NativeMetal b0d6a07,
   metal2vulkan + patches, Turnip's darwin backend) needs a Mac to build and its QuartzCore hooks redone for the
   iPhone18,2 shared cache (`docs/agents.md`, references -> userland). The kernel side publishes
   `MetalPluginName = NativeAdrenoMetal`, `MetalPluginClassName = NativeAdrenoMetalDevice`; the interface and the
   boot arguments are in section 5.1.
4. **Hosted mappings of the pool and of guest blobs.** The driver maps the pool with
   `IOMemoryDescriptor::withPhysicalAddress` and blobs into tasks; under the hosted pmap these are pages of the RAM
   memfd outside the kernel's window (the pool: allowed by `0024-gpu-pool.patch`) and ordinary pages (blobs). Not run.
5. **Zero-copy scanout**, EDID (refresh rate), cursor: not done; the display is a row copy per flush.
6. **Blobs larger than udmabuf's limits** (section 4) are refused by the kernel; there is no fallback by address.
