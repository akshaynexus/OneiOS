# GPU: virtio-gpu 3D on the phone's Adreno

Owner: fable-gpu. Date: 2026-11-10. The Metal path of OneiOS, as Droid-VM runs it on the same SoC: the guest's
Metal plug-in runs Turnip, Turnip speaks the drm/msm protocol over virtio-gpu's DRM native context, and the host
serves that protocol on `/dev/kgsl-3d0` with Droid-VM's `drm2kgsl`.

**State, plainly.** The host side is written, builds for Android and passes its tests on the PC. Nothing here has
run on the phone: no phone was on adb. `gpuctl` (section 4) is the first thing to run there. The guest's kernel
driver builds into the hosted kernel and is not run yet (the hosted kernel does not reach `bsd_init` without
`kernel.kc`). The guest's Metal user client and the Metal plug-in are not done (section 6).

## 1. The path

```
iOS process: Metal -> NativeAdrenoMetal.bundle (NativeMetal, metal2vulkan, Turnip)          userland/, not built
   | IOKit: AGXDeviceUserClient, open type 0x4149, DROIDVM_VIRTGPU_* methods                 kernel.kc, not done
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

The same script compiles NativeAdreno's pieces as Droid-VM builds them (user client, IOKit surface, execbuf, memory,
PM4 encoder, `DroidVMGpu.cpp`) into `build/oneios-gpu/adreno/` as a compile check and does **not** link them;
`adreno/status.json` says what compiled: all seven objects do, against this XNU, with Droid-VM's edits to
`NativeAdrenoPublish.cpp` and `NativeAdrenoMemory.cpp` (its `build-droidvm-gpu.py`, run up to its accelerator step;
the kits `AdrenoPM4` and `ArmSmmu4K` are pinned in `scripts/setup/get-references.sh`). See section 6 for why they are
not linked.

Checked: `HOSTED_OUT=gpu kernel/build-hosted.sh` from scratch builds and seals the kernel with the archive
(`_oneios_gpu3d_start`, `_droidvm_virtgpu_call`, `DroidVMVirtioGpu::start` defined in `xnu.macho`). Not run.

## 6. Not done

1. **Anything on the phone.** Run `gpuctl` first; then `xnuhost --gpu 3d` with a kernel that has `0005` and
   `vgpu-selftest=1` in the boot arguments.
2. **The Metal user client in the kernel.** User space reaches `droidvm_virtgpu_call` through NativeAdreno's
   `AGXDeviceUserClient` under a staged `AGXAccelerator` (open type `0x4149`). Droid-VM makes that accelerator from
   reviewed inputs of one kernelcache (`build-adreno740-accelerator.py`, iPad17,3: class sizes, bound addresses in
   IOGPUFamily) and needs three XNU patches of its board (`OSMetaClassAdoptSuperclass`, the sandbox grant,
   `native_pmap_user_mappings`). For iPhone18,2 the inputs must be redone against the kernelcache in
   `kernel/prelink/`. Until then no iOS process can open the GPU. From fable-prelink (2026-11-10): IOGPUFamily is in
   `kernel.kc`; the accelerator binds to its `IOAcceleratorES` base class, so Apple's `AGXG18P` (left out of the
   collection) is not needed; kext entries carry no symbols, so the metaclass, constructor, destructor and class size
   have to be read from `out/kernel-inputs/24A446/kernelcache.macho` as Droid-VM's
   `scripts/data/ioaccelerator-base-abi-24A446.json` records them for the iPad.
3. **The Metal plug-in.** `userland/roottweak` already places `out/userland/NativeAdrenoMetal.bundle` at
   `/System/Library/Extensions/` and records it in the contract (`metal`); the bundle itself (NativeMetal b0d6a07,
   metal2vulkan + patches, Turnip's darwin backend) needs a Mac to build and its QuartzCore hooks redone for the
   iPhone18,2 shared cache (`docs/agents.md`, references -> userland). The kernel side will publish
   `MetalPluginName = NativeAdrenoMetal`, `MetalPluginClassName = NativeAdrenoMetalDevice`; boot arguments
   `vgpu-3d=1 droidvm-gpu-metal=1 droidvm-gpu-clients=...`.
4. **Hosted mappings of the pool and of guest blobs.** The driver maps the pool with
   `IOMemoryDescriptor::withPhysicalAddress` and blobs into tasks; under the hosted pmap these are pages of the RAM
   memfd outside the kernel's window (the pool) and ordinary pages (blobs). Not run.
5. **Zero-copy scanout**, EDID (refresh rate), cursor: not done; the display is a row copy per flush.
6. **Blobs larger than udmabuf's limits** (section 4) are refused by the kernel; there is no fallback by address.
