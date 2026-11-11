# GPU: virtio-gpu 3D on the phone's Adreno

Owner: fable-gpu. Date: 2026-11-10. The Metal path of OneiOS, as Droid-VM runs it on the same SoC: the guest's
Metal plug-in runs Turnip, Turnip speaks the drm/msm protocol over virtio-gpu's DRM native context, and the host
serves that protocol on `/dev/kgsl-3d0` with Droid-VM's `drm2kgsl`.

**State, plainly.** The host side is written, builds for Android and passes its tests on the PC. On the phone only
`gpuctl` has run (2026-11-11, runtime 0.1.3, as root from the fork's Hardware checks): KGSL opened, the context and
`GET_PARAM` worked, and the first buffer import failed with ENOMEM. The cause was found in Samsung's KGSL source and is
fixed here (section 4.1); the fix has **not** run on the phone yet, `gpuctl` of the next runtime release shows whether
it holds. The guest's kernel
driver builds into the hosted kernel. Metal's kernel side is linked (2026-11-11, native-adreno; section 5):
NativeAdreno's `AGXAccelerator` under the collection's `IOAcceleratorES` and its `AGXDeviceUserClient` over
`droidvm_virtgpu_call`. On the PC it registers, IOKit matches it and the Metal plug-in's connection opens from
inside the kernel; qemu has no KGSL, so no 3D context, blob or submit has run anywhere. The Metal plug-in is built on
the PC for the iPhone18,2 24A446 shared cache and placed by the image hook (section 7); no boot has loaded it.

## 1. The path

```
iOS process: Metal -> NativeAdrenoMetal.bundle (NativeMetal, metal2vulkan, Turnip)          userland/metal, built, not run
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
| `src/gpu3d.c` | `xh_gpu3d_udmabufs`: guest physical segments of the RAM memfd as dma-bufs (`UDMABUF_CREATE_LIST`), one where the blob fits udmabuf's limits, several in a row ("chunks") where it does not |
| `src/gpu3d_kgsl.c` | The renderer: drm2kgsl, with what virglrenderer.c and crosvm's rutabaga are around it in a DroidVM; the chunk lists of resources made of several udmabufs |
| `drm2kgsl/config.h` | The `config.h` of the virglrenderer sources the Makefile compiles |
| `drm2kgsl/patches/*.patch` | This project's changes to `drm2kgsl_renderer.c` (section 2.1). The Makefile applies them, in name order, to a copy in `build/android/drm2kgsl/patched/` and compiles that; `external/virglrenderer` stays at its pin |
| `drm2kgsl/oneios_drm2kgsl.h` | The two functions the patch adds for `gpu3d_kgsl.c` |
| `drm2kgsl/host_c11.h` | Only for the host test's build of those sources |
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
  the renderer makes udmabufs of them, hands them to the context and KGSL imports each and binds them at the GPU
  address the guest chose. Nothing is copied.
- **udmabuf's limits.** The kernel caps one udmabuf at `size_limit_mb` MiB and `list_limit` runs of adjacent pages:
  64 and 1024 on the S24 Ultra (the kernel's defaults). A guest blob can be 256 MiB of single pages
  (`MaxBlobEntries` in `DroidVMVirtioGpu3D.cpp`), and an 18 MiB framebuffer of scattered 16 KiB pages is already 1152
  runs. So `xh_gpu3d_udmabufs` cuts a blob into chunks, in the blob's order: adjacent segments are joined, and a chunk
  ends where the next page would pass either limit (a run is cut in the middle when the chunk is full). Each chunk is
  one udmabuf and one KGSL object; drm2kgsl binds them one after another at the blob's GPU address with a single
  `BIND_RANGES`. A blob that fits is one udmabuf, as before. The limits used are the kernel's
  (`/sys/module/udmabuf/parameters`), but never more than 64 MiB and 1024, so the PC splits as the phone does; not
  readable means the defaults. xnuhost logs them at start (`guest pages in udmabufs of up to 64 MiB and 1024 runs`).
  Nothing in the kernel is changed for this (section 4.2 has why not).
- A resource made of several udmabufs: virglrenderer's resource holds one fd (the first chunk's); the whole list is
  kept by `gpu3d_kgsl.c` under the resource id until the resource is gone, and a second context that attaches the
  resource (`CTX_ATTACH_RESOURCE`: an IOSurface shared between processes, the display's surface) imports every chunk
  again. Cost: one fd per chunk in xnuhost plus one per chunk in each context that uses the resource.
- The pool: 8 MiB of the memfd between the kernel's window and the boot framebuffer (`main.c` `GPU_POOL_BYTES`),
  named to the guest as `/arm-io/virtio-gpu` `drm-host-pool` (base, size) and to drm2kgsl as its arena. It holds each
  context's 16 KiB response buffer; drm2kgsl keeps its first 2 MiB unused. The kernel's window is 8 MiB smaller
  for it unless `--gpu 2d`.

### 2.1 The drm2kgsl patch (`drm2kgsl/patches/0001-vbo-headroom-and-chunked-guest-blobs.patch`)

Two changes to Droid-VM's `drm2kgsl_renderer.c` at the pin, marked `ONEIOS` in the code:

1. **The VBO leaves room.** drm2kgsl reserves the guest's GPU address range as one virtual buffer object, the
   largest of 1 TiB, 256, 192, 128, 64, 16, 4 GiB the kernel grants. It now takes a size only if a second
   reservation of `DRM2KGSL_VBO_HEADROOM_GB` (default 32; 0 turns the check off) fits beside one of that size, and
   steps down otherwise. Both are tried first as reservations without the zero page mapped
   (`KGSL_MEMFLAGS_VBO_NO_MAP_ZERO`) and released, then the VBO is made. Section 4.1 has why: on this phone the
   256 GiB size is exactly KGSL's whole range.
2. **Chunked guest blobs.** `drm2kgsl_renderer_set_guest_blob_chunks` (several dma-bufs parked for the `get_blob`
   that follows, which imports each and binds them in a row) and `drm2kgsl_renderer_attach_chunks` (a second context
   importing such a resource). A chunked object is mapped for `GEM_UPLOAD` as its chunks in one view, rebound by
   `GEM_SET_IOVA` as a whole, and freed chunk by chunk (unbind, free, close). With one dma-buf everything goes the
   way it went.

To change it: edit a copy of the pinned file, `diff -u` against the original into `drm2kgsl/patches/`, `make`.
A new pin of virglrenderer that the patch no longer applies to stops the build (`patch -F0`).

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
- The split, same file, real udmabufs read back page by page: with the limits set to 8 pages and 3 runs
  (`xh_gpu3d_udmabuf_set_limits`), 20 pages in one run become 8 + 8 + 4, seven separate pages 3 + 3 + 1, two joined
  segments and a third are cut where the first udmabuf is full, exactly the limit is one udmabuf and one page more
  is not; with the limits as on the phone, 72 MiB in one run become 64 + 8 MiB (the second begins where the first
  ends) and 1100 separate pages 1024 + 76. The PC's own limit is 2147483647 MiB, and setting it to 64 needs root,
  so the kernel *refusing* a 65 MiB udmabuf was not seen here; the phone's `gpuctl nodes` printed 64.
- `tests/test_drm2kgsl.c` (`build/host/test_drm2kgsl`, built when `external/virglrenderer` is there): the renderer
  with the patched drm2kgsl on the PC against a stand-in for KGSL's memory ioctls, `ioctl` and `open` wrapped at link
  time, the udmabufs real. The stand-in gives GPU addresses as Samsung's driver does (section 4.1): one range of
  256 GiB, a VBO as large as the range allowed, every object an address of its own. Two processes: (a) with the room
  check off the VBO is 256 GiB and the first import fails with drm2kgsl's `GPUOBJ_IMPORT failed` ENOMEM, the phone's
  result; (b) with it the VBO is 192 GiB, a 16 KiB buffer is one import and one range, 72 MiB two imports bound in a
  row by one call (64 + 8 MiB at the right offsets), 1100 pages two (1024 + 76), a blob shorter than its segments
  is bound only to its size, one longer is refused with nothing left imported, a second context imports both chunks
  and binds them at its own address on `GEM_SET_IOVA`, and at the end every chunk is unbound and freed (only the
  VBO is left). 26 checks. It models KGSL from its source; it is not KGSL.
- The loader's `drm-host-pool` property (`runtime/loader` tests).
- `check-arm64`: the Android binary with the 3D code linked still boots the test kernel under `qemu-aarch64` (2D:
  no KGSL there).

Not tested on the PC: anything the GPU does (submits, fences), and KGSL itself. That is `gpuctl`.

## 4. `gpuctl`: the check on the phone

Built by `runtime/build.sh STAGE` as `STAGE/bin/gpuctl` (static). As root on the S24 Ultra:

```sh
adb push STAGE/bin/gpuctl /data/local/tmp/gpuctl
adb shell su -c 'chmod 755 /data/local/tmp/gpuctl; /data/local/tmp/gpuctl'        # nodes, routes, probe, submit, large
adb shell su -c '/data/local/tmp/gpuctl routes'                                   # one step alone: nodes|routes|probe|submit|large
adb shell su -c '/data/local/tmp/gpuctl submit -v'                                # with drm2kgsl's own log lines
```

The fork's Hardware checks screen runs it with no argument. The steps:

- `gpuctl nodes` opens the three device nodes, prints udmabuf's limits and the limits xnuhost splits by.
- `gpuctl routes` asks KGSL directly, by raw ioctls and without drm2kgsl, in processes of its own. First each way
  memory could reach the GPU, 1 MiB each, one line each with `PASS` or `FAIL <step>: <error> (<errno>)` per step:
  a **udmabuf** of memfd pages imported (`GPUOBJ_IMPORT`; what xnuhost does), a **`/dev/dma_heap/system`** buffer
  imported, a **`/dev/dma_heap/qcom,system`** buffer imported, KGSL's own **`GPUOBJ_ALLOC`** mapped; for each also
  the bind into a 4 GiB VBO and a CPU mapping written and read. Then the **address room**: a 64 KiB udmabuf imported
  beside a VBO of each size drm2kgsl tries, largest first, until one passes
  (`VBO of 256 GiB at 0x4000000000: granted; a udmabuf import beside it FAIL: Out of memory (12)`, then
  `VBO of 192 GiB ...: PASS`). Its summary line names the size that fills the range and the one that leaves room.
  Alone, its last line is `ok` or `FAILED at: routes: ...` (the udmabuf route or the room failed); in a whole run it
  only reports (`routes: FAIL: ...`) and the run goes on.
- `gpuctl probe` opens KGSL through drm2kgsl and prints the capset a guest would get (expect chip `0x43051400`, the
  Adreno 750, and on this phone `GPU addresses 0x4000000000 + 192 GiB`, not 256).
- `gpuctl submit` does what one guest context does: the context, its response buffer from the pool, `GET_PARAM`
  answered there, a submit queue, 16 KiB of its own "guest" memory imported through udmabuf and bound at a GPU
  address, one command buffer of 32 `CP_NOP`s run by the GPU, and its fence retired without a new GPU fault.
- `gpuctl large` does the same with buffers one udmabuf cannot hold, split as xnuhost splits them: **96 MiB** in one
  run of pages (two udmabufs of 64 and 32 MiB) with commands run from its first 256 bytes and from its last 64 KiB,
  and **1100 separate pages** (4.4 MiB, two udmabufs of 1024 and 76 runs) with commands run from its last page. A
  chunk bound at the wrong place leaves the GPU reading KGSL's zero page there: a fault, and the step fails. Each
  line prints the size, the count of udmabufs and the GPU address. It pins about 100 MiB while it runs.

The last line is `ok` or `FAILED at: <step>: <reason>`. It leaves nothing behind: the KGSL objects and the memory go
with the process.

What a failure means:

| Step | Likely cause |
|---|---|
| a node does not open | SELinux domain or permissions of the shell; run it from the session's domain |
| `udmabuf over the RAM memfd` | the kernel's udmabuf refuses the memfd (seal) or `list_limit`/`size_limit_mb` |
| `KGSL refused the probe` | no `KGSL_MEMFLAGS_VBO` on this kernel: drm2kgsl cannot place buffers at guest addresses |
| `the buffer's pages`, "Out of memory" | KGSL has no GPU address left for the object: the VBO fills its range (section 4.1). `routes`' address-room lines say which size does |
| `the buffer's pages`, another error | the udmabuf itself (`gpu: udmabuf over ...` in the lines above: seal, limits), or the bind into the VBO; `routes` shows the same steps without drm2kgsl |
| `routes`: udmabuf `FAIL GPUOBJ_IMPORT` with a small VBO, a heap `PASS` | this KGSL does not take udmabufs: the guest-pages design does not work here, see section 4.2 for the other route |
| `large` fails where `submit` passes | the split: a later udmabuf not imported, or bound at the wrong offset (`-v` shows drm2kgsl's lines) |
| `the fence retiring` | the command did not run, or this KGSL lacks the timestamp wait drm2kgsl uses |

### 4.1 The first run on the phone, and its cause

2026-11-11, SM-S928B, kernel `6.1.162-android14-11`, runtime 0.1.3, as root (KernelSU) from the fork's app:

```
/dev/kgsl-3d0 opens, /dev/udmabuf opens, /dev/dma_heap/system opens
/sys/module/udmabuf/parameters/list_limit    1024
/sys/module/udmabuf/parameters/size_limit_mb 64
gpu: drm2kgsl on /dev/kgsl-3d0: chip 0x43051400 (gpu id 6770), GPU addresses 0x4000000000 + 256 GiB, 1 priorities; pool 8 MiB
context 1: response buffer at pool offset 0x200000 (cache mode 1), responses from byte 16
GET_PARAM: gpu id 6770, chip 0x43051400, this context's GPU addresses 0x4000000000 + 8 GiB
  ERROR drm2kgsl: kgsl_import_dmabuf:818: GPUOBJ_IMPORT failed: Out of memory
FAILED at: the buffer's pages (udmabuf -> KGSL import -> bind): Out of memory
```

`dmesg` for the whole import: one line, `kgsl kgsl-3d0: Skipped access check`.

**The cause** [I, from the source below and these lines; `gpuctl routes` confirms or refutes it on the phone]: the
VBO drm2kgsl reserved took every GPU address KGSL has for the process, and the import had none left. In Samsung's
`graphics-kernel` ([android_kernel_samsung_sm8650-modules@3e11ddc](https://github.com/samsung-sm8650/android_kernel_samsung_sm8650-modules/tree/3e11ddcb2b63ce5af65e229e4f340ba6221bddee/qcom/opensource/graphics-kernel),
the same in `487d9a6`; Qualcomm's `gfx-kernel.lnx.15.0`/`16.0` agree):

- `kgsl_iommu.h`: a 64-bit process's objects get addresses from `KGSL_IOMMU_VA_BASE64 0x4000000000` to
  `KGSL_IOMMU_VA_END64 0x8000000000`: 256 GiB. A VBO may be as large as that range (`gpumem_max_va_size`), which is
  why 1 TiB was refused and 256 GiB granted, at `0x4000000000`: the whole range.
- `kgsl_ioctl_gpuobj_import` -> `kgsl_setup_dma_buf`: attach, map (udmabuf's `dma_map_sgtable`), then
  `verify_secure_access`, which prints `Skipped access check` for a buffer that is not from Qualcomm's mem-buf
  exporter **and returns 0**. So the udmabuf was attached and mapped; that line is not a refusal.
- then `kgsl_mem_entry_attach_and_map` -> `kgsl_mem_entry_attach_to_process` -> `kgsl_mmu_get_gpuaddr`: every object
  gets a GPU address of its own in that same range, whether it will be bound into a VBO or not.
  `_get_unmapped_area` finds no gap and returns `-ENOMEM`; nothing is logged.

A dma-heap buffer or a `GPUOBJ_ALLOC` takes the same path to the same ENOMEM, so the exporter was not the problem
and another exporter would not have helped. Why Droid-VM's phones do not hit this is not known (a guard page on
their kernels would make the 256 GiB rung fail and leave 192; a guess). The other places that can return ENOMEM
silently on this path are the SMMU mapping of the object at its own address (`_kgsl_iommu_map`: nothing mapped, or
no guard page); `routes` tells them apart: an import with only a 4 GiB VBO passes if the cause is the address range.

**The fix**: drm2kgsl takes a VBO size only if 32 GiB of addresses remain beside it (section 2.1), which on this
phone gives a 192 GiB VBO (32 contexts of 6 GiB) and 64 GiB for the objects' own addresses. Each imported object
uses its size there (aligned to 1 MiB from 1 MiB up), so 64 GiB is far more than a guest's RAM.

### 4.2 The routes considered

| Route | Verdict |
|---|---|
| **Guest pages through udmabuf, chunked** (chosen) | What the guest driver, Turnip and drm2kgsl already do (`MSM_BO_GUEST_ALLOC`); no copy; no kernel setting changed. Needs the VBO fix above and the split for udmabuf's limits. Unproven until `gpuctl` passes on the phone |
| **Host-allocated memory** (KGSL `GPUOBJ_ALLOC`, or a `/dev/dma_heap/system` / `qcom,system` buffer imported), mapped into the guest as a host blob (`BLOB_MEM_HOST3D`, `MAP_BLOB`) | Not built. It would not have avoided the failure (same address allocation). drm2kgsl at this pin has no host allocator for buffer objects (`GEM_NEW` without `MSM_BO_GUEST_ALLOC` is refused with ENOTSUP; `kgsl_alloc_thp_bo` and `kgsl_alloc_arena_bo` are left in the file uncalled), the guest's Turnip and `DroidVMVirtioGpu3D.cpp` allocate from guest RAM, and under xnuhost a host buffer would have to be placed in the guest's physical window, which is the RAM memfd (today only the 8 MiB pool is). It is the route to build **if `gpuctl routes` shows the udmabuf import failing with a small VBO while a heap or `GPUOBJ_ALLOC` passes**: `routes` tests all three so that run decides |
| **Raising `size_limit_mb` and `list_limit` as root** | Possible, not used. Both are `module_param(..., 0644)` in `drivers/dma-buf/udmabuf.c` ([Samsung's kernel@809f7ed](https://github.com/samsung-sm8650/android_kernel_samsung_sm8650/blob/809f7edaad95be2f60ce09f577fe5104d8940214/drivers/dma-buf/udmabuf.c)), so root can write `/sys/module/udmabuf/parameters/*` on a built-in udmabuf (not tried on the phone; SELinux may still refuse the write). It is a system-wide setting that lets every process with `/dev/udmabuf` (crw-rw-rw- here) pin more unevictable shmem per buffer; xnuhost can be killed, so the restore would have to be the fork's stop path on every exit; and `list_limit` would have to be 65536 for a 256 MiB blob of single pages. The split needs none of that |
| One udmabuf of all guest RAM, bound by offset | Refused by the kernel (several GiB against 64 MiB); it would also pin all of guest RAM |

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
| `droidvm-gpu-clients=a,b,...` or `*` | the processes besides `backboardd` granted the user client (DroidVMGpu.cpp `grantedProcesses`). Not given: `WidgetRenderer_Default,*Poster*,iconservicesagent,SBRendererService`, the list of the image's `NativeAdrenoMetal.clients`; empty: `backboardd` alone |
| `vgpu-3d=1` | what `GETPARAM(DEFAULT_3D)` answers the plug-in (DroidVMVirtioGpu3D.cpp reads it with default 0; `vgpu-3d=0` also turns the driver off) |
| `oneios-adreno-selftest=1` | after the publish: IOKit's match of `IOAcceleratorES`, an in-kernel open with type `0x4149`, `GETPARAM(ABI_VERSION)` through the tunnel; two `oneios-adreno: selftest:` lines |

**How iOS finds the plug-in (`0031-gpu-addon-mount.patch`).** Metal loads
`/System/Library/Extensions/<MetalPluginName>.bundle`. That folder is on the sealed system volume, so UniXNU's
`native_gpu_addon_prepare` shows a Data folder over it as a read-only bindfs union, after the Data seed is unpacked and
before launchd. Before 0031 that never happened here: the mount wanted `sheng-gpu-addon=1`, which no session passes, and
looked in `/private/var/ngpu/addons`, while the image hook places the bundle in `/private/var/oneios/addons/Extensions`.
Nothing logged it, so `backboardd` simply had no Metal device and CoreAnimation composited on the CPU (phone, 2026-11-11:
no `AGX*` user-client call from backboardd). Now:

- The mount is on by default in the hosted kernel; `sheng-gpu-addon=0` turns it off.
- The source is the Data seed's folder, or `/private/var/ngpu/addons` if a GPU addon was handed to this boot:
  `kernel.kc.gpuaddon` beside the kernel (`userland/metal/build.sh addon` makes it as
  `out/userland/NativeAdrenoMetal.gpuaddon`, xnuhost passes it on, `native_ram_block_gpu_addon` in `gpu/OneiOSAdreno.cpp`
  finds it). The kernel writes its files into Data first, so a rebuilt plug-in needs no new seed. Its CDHashes must still be in
  the image's `overlay/userland/owner-trust.json`.
- The grant was not the gate for `backboardd`: `grantedProcesses` always names it. The other Metal clients are granted by default too (table below). Naming them was not enough: with both class chains adopted the sandbox decides the open, and the iPhone's profiles refused `iconservicesagent` every open (`0xe00002e2`, the home screen without icons). `0033-gpu-grant-decides.patch`: the grant decides for the processes it names; the rest are the sandbox's, as before.

Log lines (always on):

```
oneios-gpu-addon: boot addon: -1 files (-1 none), 0 written, 0 unchanged, 0 failed, last error 0
oneios-gpu-addon: /private/var/oneios/addons/Extensions over /System/Library/Extensions: stage 5, status 0 (...)
oneios-gpu-plugin: backboardd[53] code signature of NativeAdrenoMetal: error 0
oneios-gpu-plugin: backboardd[53] library validation of NativeAdrenoMetal: error 0
oneios-gpu-open: backboardd[53] AGXAccelerator type 0x4149 -> 0x0, AGXDeviceUserClient
```

Reading them, in order: `oneios-gpu-addon` stage 5 means iOS can see the plug-in; `oneios-gpu-plugin` is dyld loading it (no
line: nobody tried, or the vnode had no name; an error on the first line is the trust cache, on the second AMFI's library
validation); `oneios-gpu-open` with `0x41476800` is the plug-in's code running in that process.

Stages of the mount: 1 source found, 2 mounting, 3 mounted, 4 the sealed `AGXMetalG18P.bundle` visible through it, 5 the
plug-in's `Info.plist` visible too. `oneios-gpu-open` is one line per open of an `AGX*` service, at most 400 per boot. The
plug-in's progress markers arrive the same way, as open types `0x4147xxxx` answered unsupported by design: `0x6800` entered,
`0x6801` Turnip found, `0x6802` device made, `0x6803` handed to Metal's discovery, `0x68f1` no Turnip, `0x68f2` no device.

**Is CoreAnimation drawing on the GPU?** (`gpu/OneiOSAdreno.cpp`, `report`). `oneios-iouc` cannot say: it prints the first
call of each class and selector only, stops at 600 lines, and every Turnip op goes through one selector with the op in the
payload. The tunnel's own counters (`DroidVMVirtioGpu3D.cpp`) are printed instead, every `oneios-gpu-report` seconds
(default 10, `0` off) while one changed:

```
oneios-gpu-report: 10 s: submits +612 (fenced +600), render server frames +598 (mean 3100 us, max ever 41000 us, <1/<2/<4/<8/<16/<32 ms ...), contexts +0, blobs +14, maps +3, fence timeouts +0, refusals +0 (...), clients 3, live blobs 210 (96 MiB)
oneios-gpu-clients: backboardd 80 MiB, SpringBoard 12 MiB
```

"render server frames" are backboardd's fenced submissions (the tunnel counts the compositor's, `droidvm_virtgpu_rs_fences`):
one or more for every frame CoreAnimation draws with Metal, with the time from submission to the GPU being done. While the
screen changes (swipe on the lock screen) they should grow by about the frame rate; if the swaps grow and they do not,
CoreAnimation draws on the CPU. A device that was made but never used shows contexts and blobs with no submits.

`0xe00002d6` (`kIOReturnTimeout`) from the tunnel is mostly not a stall. Turnip's Darwin backend polls fences with a timeout of
0 (`dw_wait_fence_raw(dev, fence, 0)` as it sweeps its retired buffers, and `vkGetFenceStatus`), and the tunnel answers an
unsignalled fence that way; those are not counted. A real wait runs in 100 ms slices and sleeps on the device's interrupt, and
only one that runs out is counted, as "fence timeouts" in the report. In `oneios-iouc` the polls show as status changes
between `0x0` and `0xe00002d6` on the one tunnel selector.

The plug-in's own path in backboardd, by its markers (`oneios-gpu-open` types `0x4147xxxx`): `6800`, `6801`, `6A..` (what the
render server reads for its framebuffer format), `681x` (which cache folder served), `6802` device made, `6803` handed to
Metal; 3 s later its scanout thread opens type `0x4148` (`6700`, `6701`, `6702`). So an open of type `0x4148` by
backboardd already means the device was made.

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
   from a user process, and every call that needs the host's renderer. The seven kernel imports of the kexts on Metal's path
   that were on the panic stub are paired (2026-11-11, fable-prelink, `kernel/prelink/README.md`): IOGPUFamily's
   `thread_set_allocation_name`, `IOMemoryMap::wireRange` (also IOMobileGraphicsFamily's) and
   `IOSharedDataQueue::withEntries`; IOSurface's `IOMemoryDescriptor::ktraceEmitPhysicalSegments` and
   `IOMultiMemoryDescriptor::withDescriptors`; IOMobileGraphicsFamily's
   `IOFilterInterruptEventSource::filterInterruptEventSource` and `PEGetModelName`. Paired from the import order
   and the code; no caller has reached them in a boot yet. IOGPUFamily, IOSurface and IOMobileGraphicsFamily have
   no import on the stub now.
   NativeAdreno itself calls kext code only through the two bound stubs; the plug-in's
   IOSurface use and IOGPUFamily's own user clients can reach the others.
3. **The Metal plug-in running.** It is built and placed (section 7); nothing has loaded it. The kernel side publishes
   `MetalPluginName = NativeAdrenoMetal`, `MetalPluginClassName = NativeAdrenoMetalDevice`; the interface and the
   boot arguments are in section 5.1.
4. **Hosted mappings of the pool and of guest blobs.** The driver maps the pool with
   `IOMemoryDescriptor::withPhysicalAddress` and blobs into tasks; under the hosted pmap these are pages of the RAM
   memfd outside the kernel's window (the pool: allowed by `0024-gpu-pool.patch`) and ordinary pages (blobs). Not run.
5. **Zero-copy scanout**, EDID (refresh rate), cursor: not done; the display is a row copy per flush.
6. **`gpuctl` passing on the phone** with the VBO fix and the split (section 4.1): not run yet. Until it does,
   whether this KGSL binds several imported udmabufs in a row and runs commands from them is unproven.

## 7. The Metal plug-in (`userland/metal`)

Built on the Linux PC with the iOS cross toolchain ([building.md](building.md) section 9); no Mac.

```sh
scripts/bg.sh metal userland/metal/build.sh      # stage, turnip, plugin, finish, addon (each also runs alone)
```

**What it builds**, all for `arm64e-apple-ios27.0`:

| Piece | Source | Notes |
|---|---|---|
| `libvulkan_freedreno.dylib` | `external/mesa-droidvm` cecc0e9 (Droid-VM/mesa, `droidvm`) + the 30 patches listed in Droid-VM-UniXNU `scripts/data/adreno-compiler-inputs.json` (`patches/mesa/`: `turnip-darwin-kmd`, `turnip-darwin-virtgpu`, ...) | meson cross build, `-Dvulkan-drivers=freedreno -Dfreedreno-kmds=darwin`, release. Its 5633 weak flat-namespace imports are bound to the image itself (`macho-weak-self-binds.py`) |
| `libadreno_metal_bridge.a` | `external/metal2vulkan` 624bab3 + NativeMetal `bridge/patches` 0001-0024 (without 0020) + Droid-VM-UniXNU `patches/metal2vulkan` 0011-0038, behind `bridge/NativeMetalBridge.rs` | Rust 1.98.0, target `arm64e-apple-ios`, std built from `rust-src`, fat LTO. No `Cargo.lock` is pinned upstream: the crates are the newest that fit at build time |
| `NativeAdrenoMetal` | `external/NativeMetal` b0d6a07 (`NativeAdrenoMetalPlugin.mm`, `NativeBridgeMetalFrontend.mm`), `external/AdrenoPM4`, the bridge | The Rust objects' callback imports go through `RustPacBoundary.c`; their indirect branches through its trampolines (`rust-pac-rewrite.py`) |

The steps are the kernel base's own scripts (`build-turnip-darwin.py`, `build-adreno-metal-bridge.py`,
`build-adreno-metal-plugin.py`, `--target ios27`), run from a staged copy of its tree in `out/metal/root` with
`userland/metal/shim/` first in `PATH`: `xcrun`, `codesign`, `strip` and `nm` are answered by `scripts/ios` and
cctools. The staged copies differ from upstream in four lines, which `stage` prints: meson's build-machine compiler,
ninja's job count, a longer limit for the Rust build, and no `TurnipReplay.h` (it is recorded by a Mac-only host
simulation; the plug-in includes it only when it exists, so the prefix-replay diagnostic is absent). Also not made:
`build/prime-list.txt` (the image's `.metallib` paths for shader priming; the plug-in primes nothing without it).
The script's probe daemon `adrenoprobed` is built beside the bundle (`out/metal/root/build/adreno-metal-plugin27/`)
and not placed.

**The bundle** (`out/userland/NativeAdrenoMetal.bundle`): `Info.plist` (`org.blu-sharky.NativeAdrenoMetal`, principal
class `NativeAdrenoMetalDevice`, MinimumOSVersion 27.0), `NativeAdrenoMetal`, `libvulkan_freedreno.dylib` (opened by
path by the plug-in), `oneios-target`. Both binaries are signed by `scripts/ios/ios-sign.py`;
`NativeAdrenoMetal.build.json` beside the bundle records the SDK, the stubs' cache, the source pins, sizes and CDHashes.

**The shared cache addresses.** Upstream's plug-in writes QuartzCore's options at fixed addresses of the iPad17,3
24A446 cache, relative to `CARenderServerStart`. They were re-found in this guest's cache
(`out/image/24A446/systemos.img`, cache UUID `77BC8C63-93DA-3E9B-BB9C-0BE2AF0C0E17`) and are recorded in
`userland/metal/hooks/iPhone18,2-24A446.json`; `stage` rewrites the staged `NativeAdrenoMetalPlugin.mm` from that file
and refuses to go on if the arm64e code holds an address the file does not name.

| What | iPad17,3 (upstream) | iPhone18,2 |
|---|---|---|
| `_CARenderServerStart` (the anchor) | `0x1846dce88` | `0x1846dce88` |
| `CA::WindowServer::IOMFBDisplay::framebuffer_pixel_format`; its `tbnz w8, #0` (word `0x37000608`) | `0x184461838`; `0x184461928` | the same |
| `debug_values[21]` (`CA_DISPLAY_EDR_HEADROOM`) | `0x1e7021014` | `0x1e7025014` |
| `debug_values[81]` (`CA_FP16_MIN_HEADROOM`) | `0x1e7021104` | `0x1e7025104` |
| `_ca_debug_options` (+128 force FP16, +156 EDR mirroring) | `0x1e702116c` | `0x1e702516c` |
| `CADeviceIsVirtualized::is_virtualized` | `0x1e701bdd9` | `0x1e701fdd9` |
| `CADeviceIsVirtualized::once` | `0x1e7020780` | `0x1e7024780` |
| the constant CFString of that query (a diagnostic note) | `0x1e9021680` | `0x1e9025680` |

QuartzCore's code is at the same addresses in both caches; its `__DATA_DIRTY` and the CFString are 0x4000 higher in
the iPhone's. Placed as upstream builds it, the plug-in would have written the headroom, the threshold, the option
byte and the once token 16 KiB below their places in backboardd. How they were found:

```sh
apfs-fuse -o ro out/image/24A446/systemos.img MNT       # the cache: MNT/root/System/Library/Caches/com.apple.dyld/
C=out/ios27-cache/dyld_shared_cache_arm64e              # (a copy of that folder; ipsw reads it much faster)
out/bin/ipsw dyld symaddr $C -i QuartzCore              # the cache has QuartzCore's local symbols:
#   0x1846dce88 _CARenderServerStart                     0x184461838 __ZN2CA12WindowServer12IOMFBDisplay24framebuffer_pixel_formatEv
#   0x184541f10 ___CADeviceIsVirtualized_block_invoke    0x1844eef00 __ZL10init_debugPv
#   0x1e7024fc0 __ZL12debug_values    0x1e702516c _ca_debug_options
#   0x1e701fdd9 __ZZ21CADeviceIsVirtualizedE14is_virtualized    0x1e7024780 __ZZ21CADeviceIsVirtualizedE4once
out/bin/ipsw dyld disass $C --vaddr 0x184461838          # framebuffer_pixel_format reads exactly these:
#   0x184461888 ldr s8, [0x1e7025000 + 0x104]     0x1844618c4 ldrb [.. + 0x1ec]    0x1844618d0 ldrb [.. + 0x208]
#   0x184461914 ldr x8, [0x1e7024000 + 0x780]     0x184461924 ldrb [0x1e701f000 + 0xdd9]
#   0x184461928 08 06 00 37  tbnz w8, #0, 0x1844619e8
out/bin/ipsw dyld disass $C --vaddr 0x184541f10          # adrp/add x0 = 0x1e9025680; strb w0, [0x1e701fdd9]
```

`debug_values[21]` is read by `CA::WindowServer::Display::edr_headroom` (0x18441f638, which that function calls):
`0x18441f660 ldr s0, [0x1e7025000 + 0x14]`. **RenderBox, WebGPU, ANGLE:** the frontend tells each a GPU
family by the caller's image path (`dladdr`), no address: all three images are in this cache under the paths the
code tests. The frontend's `quartzCoreCallers` diagnostic still holds offsets of the iOS 17 QuartzCore, as upstream
has it on the iPad; it only reads inside QuartzCore's image.

**Verified** (2026-11-11, SDK 26.5 headers, iOS 27 stubs): the bundle builds; `scripts/ios/macho-check.py` passes for
both binaries (every library has a stub of 24A446 and every bound import is exported by it; Turnip's 5633 weak
entrypoints are bound to itself); the image hook places the bundle and `owner-trust.json` lists both CDHashes
(sizes and hashes: `out/userland/NativeAdrenoMetal.build.json`).
**Not verified:** anything at run time. No guest has loaded the bundle: until `0031-gpu-addon-mount.patch` no kernel
showed it to iOS (section 5.1), and that patch has compiled but not run. Whether dyld and AMFI of this guest accept the binaries, whether the
writes above have the effect they have on the iPad, and whether MetalCheck passes are all open.

