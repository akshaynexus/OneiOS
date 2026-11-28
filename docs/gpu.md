# GPU: virtio-gpu 3D on the phone's Adreno

> **The Galaxy A56 (Xclipse 540, iOS 16.5)** has its own GPU port, Metal on Mesa radeonsi without Vulkan: [a56-gpu.md](a56-gpu.md) (2026-11-24).

Owner: fable-gpu. Date: 2026-11-10. The Metal path of OneiOS, as Droid-VM runs it on the same SoC: the guest's
Metal plug-in runs Turnip, Turnip speaks the drm/msm protocol over virtio-gpu's DRM native context, and the host
serves that protocol on `/dev/kgsl-3d0` with Droid-VM's `drm2kgsl`.

**State, plainly (2026-11-10; the host side is Rust since 2026-11-23, section 2, and that build has not run on the
phone).** The host side is written, builds for Android and passes its tests on the PC. On the phone only
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
xnuhost: src/virtio_gpu.rs (the device) -> src/gpu3d.rs -> src/gpu3d_kgsl.rs                 Rust, tested on the PC
   | function calls, same process
drm2kgsl (external/virglrenderer src/drm/drm2kgsl, linked in) -> ioctl on /dev/kgsl-3d0      ran on the phone (2026-11-23)
```

## 2. Host side (`runtime/xnuhost`)

The host side is Rust since 2026-11-23 (branch `rust-runtime`; until then the same files in C: `src/virtio_gpu.c`,
`src/gpu3d.h`/`.c`, `src/gpu3d_kgsl.c`, `tools/gpuctl.c`, built by `runtime/xnuhost/Makefile`). Every behaviour, log
line and test case was kept ([runtime/RUST.md](../runtime/RUST.md)); the Rust GPU path first ran on the phone on
2026-11-23 (SpringBoard's and the posters' contexts through drm2kgsl, `gpu: context N ...` lines; docs/booter.md 2.6a).

| File | What |
|---|---|
| `src/virtio_gpu.rs` | The device. 2D as before (the first frame still comes through it). With a renderer: features VIRGL, RESOURCE_BLOB, CONTEXT_INIT, CREATE_GUEST_HANDLE; one capset (6, DRM); `CTX_CREATE/DESTROY`, `CTX_ATTACH/DETACH_RESOURCE`, `RESOURCE_CREATE_BLOB`, `RESOURCE_MAP_BLOB` (answered `MAP_INFO_POOL` + the offset in the pool), `RESOURCE_UNMAP_BLOB`, `SUBMIT_3D`, `SET_SCANOUT_BLOB` |
| `src/gpu3d.rs` | What the device asks of a renderer (the `Gpu3d` trait), so the tests can put a recording one there; guest physical segments of the RAM memfd as dma-bufs (`UDMABUF_CREATE_LIST`), one where the blob fits udmabuf's limits, several in a row ("chunks") where it does not |
| `src/gpu3d_kgsl.rs` | The renderer: drm2kgsl, with what virglrenderer.c and crosvm's rutabaga are around it in a DroidVM; the chunk lists of resources made of several udmabufs, the stats, the log lines |
| `src/gpu3d_kgsl/policy.rs` | The GPU's per-process policy as pure functions: each context's class, its KGSL priority, the level by frame deadline (section 2.2b); tests in `policy/tests.rs` |
| `drm2kgsl/` (crate `drm2kgsl-sys`) | `build.rs` builds the renderer's C from `external/virglrenderer` (below) and a small glue; `src/` is the safe Rust front (one lock over the renderer's tables) |
| `drm2kgsl/config.h` | The `config.h` of the virglrenderer sources `build.rs` compiles (`-imacros`) |
| `drm2kgsl/patches/*.patch` | This project's changes to `drm2kgsl_renderer.c` (section 2.1) and, from 0010, `drm_context.c`. Each patch names its one file on its `+++ b/` line; `build.rs` applies them, in name order, to a copy of that file in cargo's `OUT_DIR` and compiles the copy; `external/virglrenderer` stays at its pin |
| `drm2kgsl/oneios_drm2kgsl.h` | The functions the patches add for the renderer's front |
| `drm2kgsl/oneios_gpu_policy.h` | The C policy header the patched renderer still includes for its own two uses (`og_class_name`, `og_kgsl_priority`); `policy.rs` is the policy, and its test `the_renderers_header_agrees` checks the two say the same |
| `drm2kgsl/glue/oneios_glue.c`, `.h` | Only what Rust cannot reach: virglrenderer's C structures with function pointers (read and called by id) and the three variadic or `va_list` functions the sources expect of their surroundings (Android's liblog and mesa's `os_misc.c`), which go to xnuhost's log |
| `drm2kgsl/glue/oneios_sys.c`, `.h` | Host builds only: every `open` and `ioctl` of the renderer through a table the tests set (the stand-in KGSL), instead of the C tests' `-Wl,--wrap` |
| `drm2kgsl/glue/oneios_kgsl_layout.c` | Host builds only: the sizes and offsets of drm2kgsl's `msm_kgsl.h` that the tests' stand-in KGSL mirrors in Rust (`src/gpu3d_kgsl/tests/kgsl.rs`), checked by `layout_matches_msm_kgsl_h` |
| `drm2kgsl/host_c11.h` | Only for the host build of those sources (mesa's C11 threads shim beside a new glibc) |
| `src/gpu3d_amdgpu.rs`, `src/gpu3d_amdgpu/` | The Galaxy A56's renderer: the amdgpu native context (Mesa's protocol) on an amdgpu render node by raw ioctls, its self-test, and the choice between it, KGSL's and 2D (a host with `/dev/kgsl-3d0` keeps this path unchanged): [a56-gpu.md](a56-gpu.md) 2 |
| `tools/gpuctl.rs` | The renderer alone, for the phone (section 4); the bin `gpuctl` |

**drm2kgsl is linked in, statically. There is no helper process.** `runtime/xnuhost/drm2kgsl/build.rs` compiles
sixteen sources of
`external/virglrenderer` (Droid-VM's fork at the pin of `scripts/setup/get-references.sh`): the renderer, the DRM
context and fence code, the context and resource tables and the mesa utilities under them. `virglrenderer.c` itself
is left out: it needs the GL renderer (epoxy, EGL) and libdrm, which a static Android binary has not. The three
symbols it wants from Android's liblog and mesa's `os_misc.c` are in `glue/oneios_glue.c` and go to xnuhost's log.
Without `external/virglrenderer` the crate still builds (`drm2kgsl_sys::AVAILABLE` false, a cargo warning) and xnuhost is the 2D-only one, which says so at start (`gpu: 2D only: this build has no drm2kgsl`); `runtime/build.sh` refuses such a stage unless `XNUHOST_2D_ONLY=1`.

**Memory.**
- The RAM memfd is now created sealable and sealed against shrinking (`src/mem.rs`): udmabuf takes no other memfd.
- Guest pages: every buffer object is the guest's own memory (`MSM_BO_GUEST_ALLOC`, a `HOST3D_GUEST` blob with
  `CREATE_GUEST_HANDLE`). The device checks the segments (guest RAM, whole 4 KiB pages, at least the blob's size),
  the renderer makes udmabufs of them, hands them to the context and KGSL imports each and binds them at the GPU
  address the guest chose. Nothing is copied.
- **udmabuf's limits.** The kernel caps one udmabuf at `size_limit_mb` MiB and `list_limit` runs of adjacent pages:
  64 and 1024 on the S24 Ultra (the kernel's defaults). A guest blob can be 256 MiB of single pages
  (`MaxBlobEntries` in `DroidVMVirtioGpu3D.cpp`, 65532 entries), which the device takes since 2026-11-23 (section
  2.4b: an 80 MiB blob is 20480 entries and 20 udmabufs), and an 18 MiB framebuffer of scattered 16 KiB pages is
  already 1152 runs. So `gpu3d::udmabufs` cuts a blob into chunks, in the blob's order: adjacent segments are joined, and a chunk
  ends where the next page would pass either limit (a run is cut in the middle when the chunk is full). Each chunk is
  one udmabuf and one KGSL object; drm2kgsl binds them one after another at the blob's GPU address with a single
  `BIND_RANGES`. A blob that fits is one udmabuf, as before. The limits used are the kernel's
  (`/sys/module/udmabuf/parameters`), but never more than 64 MiB and 1024, so the PC splits as the phone does; not
  readable means the defaults. xnuhost logs them at start (`guest pages in udmabufs of up to 64 MiB and 1024 runs`).
  Nothing in the kernel is changed for this (section 4.2 has why not).
  **When the kernel refuses one with ENOMEM** (2026-11-22, ported from `YumikoHayamato/OneiOS` 51124a6): a kernel before
  6.10 keeps a udmabuf's page array in one kmalloc, 8 bytes a page, so a 64 MiB udmabuf needs 128 KiB of contiguous
  memory (order 5); under fragmentation that fails although RAM is free (dmesg `page allocation failure: order:5`).
  The S25 Ultra's 6.6 kernel did it to `gpuctl large`. Then the size limit halves (down to 4 MiB, for the rest of the
  process) and the blob is split again: more chunks, which the chunked path above already takes. One line each time:
  `xnuhost: gpu: udmabuf: the kernel refused a 64 MiB udmabuf (ENOMEM: its page array needs contiguous memory);
  udmabufs of at most 32 MiB from now on`. Every phone (the S24 Ultra's 6.1 too); nothing changes until a refusal.
  Host test (`src/virtio_gpu/tests.rs`, `test_xnuhost` in the C): a stand-in kernel that refuses lists over 16 MiB gets 72 MiB as 16 + 16 + 16 + 16 + 8, the limit
  stays 16 MiB, and another error (EINVAL) is returned as it is with the limit kept.
- A resource made of several udmabufs: virglrenderer's resource holds one fd (the first chunk's); the whole list is
  kept by `gpu3d_kgsl.rs` under the resource id until the resource is gone, and a second context that attaches the
  resource (`CTX_ATTACH_RESOURCE`: an IOSurface shared between processes, the display's surface) imports every chunk
  again. Cost: one fd per chunk in xnuhost plus one per chunk in each context that uses the resource.
- The pool: 8 MiB of the memfd between the kernel's window and the boot framebuffer (`src/session/board.rs` `GPU_POOL_BYTES`),
  named to the guest as `/arm-io/virtio-gpu` `drm-host-pool` (base, size) and to drm2kgsl as its arena. It holds each
  context's 16 KiB response buffer; drm2kgsl keeps its first 2 MiB unused. The kernel's window is 8 MiB smaller
  for it unless `--gpu 2d`.

- **No separate GPU pool (fix-fixed-memory, 2026-11-13: asked for, not built, and why).** The owner asked for a
  preallocated host pool (1.0-1.3 GiB, `gpu-pool=`) serving the guest's blobs as udmabufs over pool ranges. Read end to
  end, there is no per-blob host allocation to pool: every Turnip BO is `HOST3D_GUEST` / `MSM_BO_GUEST_ALLOC`
  (`DroidVMVirtioGpu3D.cpp` `createBlob`: `taskPages`/`guestPages` from XNU's own VM; drm2kgsl's `gem_new` refuses any
  other: "this host allocates no BO backing"), and the udmabufs are over **the guest's RAM memfd**. backboardd's
  936 MiB of "own task" blobs in the c2d281a session were part of the guest's 6,254 MiB in the host's RAM, not beside
  it. A host pool would be memory iOS cannot use unless the guest kernel driver carved its BOs from a host range
  (Droid-VM's `--pre-alloc drm-guest-mb` with a guest-side buddy allocator): a change in `DroidVMVirtioGpu3D.cpp`, the
  widgets agent's IOSurface path, a larger `0024` pool range (64 MiB today, and pool pages fail `copyin`), and nothing
  of it runs on the PC (no KGSL). So the GPU's memory is held fixed by holding the RAM fixed (`ram-giveback=0`, the
  default since fix-ram-locked, 2026-11-15;
  [booter.md](booter.md) 2.2): the blobs' pages are prefaulted and locked with the rest and never given back. What stays
  per blob on the host is KGSL's and udmabuf's own kernel memory (page arrays, sg tables, IOMMU tables: about 1 % of the
  blob bytes [estimate]); the fixed-RAM `memory:` line prints KGSL's per-process files for xnuhost so the next phone boot
  shows whether that grows. The cost per blob (udmabuf, import, bind: `blobs made N, mean X us`) is unchanged.

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

### 2.2 `drm2kgsl/patches/0002-power-constraint-at-the-start-of-a-burst.patch`

drm2kgsl runs a compositor queue's submissions at the GPU's highest level (a KGSL power constraint) while it submits at
`DRM2KGSL_PWR_CONSTRAINT_MIN_RATE` a second (60; upstream). iOS's render server draws sparse frames for a touch or a clock tick,
and with that rule alone each ran at the governor's idle clock: 22-31 ms from submission to done. Constraining every submission
(`MIN_RATE=0`, phone, 2026-11-11) gave 8-10 ms, but also held the top clock through a steady 40-50 frames a second that a still
lock screen kept drawing (section 5.1), with the SoC at 68 C. The patch adds: the first `DRM2KGSL_PWR_CONSTRAINT_BURST` (2)
submissions after a pause of 100 ms or more are constrained too. So a touch's first frames and a sparse update run at the top
clock, a sustained animation at 60 a second or more as upstream, and anything slower and steady is the governor's. The rate
rule's hysteresis (it stays on down to half its rate) follows the rate rule's own answer, not a burst's, or a burst would have
kept a 45 fps redraw constrained. `MIN_RATE=0` still constrains every submission; `BURST=0` is upstream's rule alone. Only
queues that ask are constrained (the render server's: `context N: constrained ...` in the log). drm2kgsl's matching vCPU
clamp looked for threads named `crosvm_vcpu*` (xnuhost's are `xnu-cpuN`: `vCPU uclamp.min 1024: 0 of 0 threads`); since 2026-11-15 its
patch 0008 hands the value to xnuhost (`drm2kgsl_vcpu_uclamp`). Since 2026-11-23 xnuhost only stores it and gives it, at
each release, to the frame's threads (a UI, above-UI, realtime or user-initiated thread, or one whose vCPU the guest asked
for its top) and their vCPU threads with them; before, every vCPU thread and every iOS thread but background ones carried
it through each animation (`docs/booter.md` 2.6e).

### 2.2a `drm2kgsl/patches/0005-clock-held-between-frames.patch` (2026-11-12)

**Since 2026-11-15 the rule below is `gpu-level=hold` only; by default xnuhost decides the level per frame (section 2.2b)
and drives this hold itself. 2.2's burst and rate rules and 0006's follow rule apply only before xnuhost sets a level
(gpuctl).**

On the phone with every app drawing (diagnostic session of 2026-11-12, clocks uncapped) the render server's frames took
5-18 ms from submission to done at 231 MHz with the owner swiping, KGSL 15 % busy: the burst rule constrained a
frame's own submission, KGSL dropped the constraint about 80 ms (its idle timer) after it, and the governor, seeing
10-20 % busy, kept the lowest level, so every frame started at the bottom and the clock ramped inside it. A sustained
animation (whose submissions keep the constraint alive) ran the same frames in about 2 ms (2026-11-11). Now:

- every submission of the render server's queue carries the constraint (`DRM2KGSL_PWR_CONSTRAINT_MIN_RATE` default 0;
  the burst rule stays for a non-zero rate);
- while that queue submits and for `DRM2KGSL_PWR_HOLD_MS` (150; 0 turns it off) after its last submission, a thread
  of the renderer submits 4 `CP_NOP`s every 40 ms on a KGSL context of its own (its own `/dev/kgsl-3d0` file, kernel
  timestamps) that carries the same constraint, so KGSL renews it and the next frame starts at the top level; then
  KGSL drops it and the governor decides as before;
- KGSL's thermal limit (`thermal_pwrlevel`) still caps the level the constraint asks for.
- `0006-hold-follows-every-client.patch`: within `DRM2KGSL_PWR_FOLLOW_MS` (1000; 0 turns it off) of the render
  server's last submission, any other context's submission extends the hold too. On the phone (14:12 boot,
  2026-11-12) the hold made the render server's frames 2.9-4.0 ms, but opening Settings its frames took 25 ms with the
  hold started 3 times in 10 s: the app's own GPU work, which the render server waits for, ran at 231 MHz. A client
  that keeps submitting with the screen still does not hold the clock past that second.

Log: once `drm2kgsl: clock hold: the top level from the render server's submissions to 150 ms after them` (or
`clock hold: off (reason)`), and in the 10 s host line `clock held +N times, keep-alives +M` (a hold is a run from
the first frame after a pause to 150 ms after the last; M about 25 per second of holding). What to expect: KGSL
`gpuclk` at the top level while the screen moves and for 150 ms after, the governor's level otherwise; the host
line's `submit->retired` near 2 ms. Heat: the GPU at the top level whenever iOS draws (a still screen draws nothing);
the thermal agent's caps apply. Host tests pass with it (they do not drive a deadline queue, so the hold itself is
not exercised off the phone); not yet run on the phone.

### 2.2b The level by deadline, and each context's priority by its owner (fix-gpu-perf, 2026-11-15)

**Switches (2026-11-23, docs/switches.md):** `gpu-down=bottom` judges the way down at KGSL's lowest clock (the rule
before 3b36ff0); `gpu-blob-entries=16384` gives back the blob limit before 78c4b86; `uclamp-rule=all` sends drm2kgsl's
animation clamp to every vCPU thread again (before 03d85ac).

**What the path did until now (from the code).** The render server's (backboardd's) Turnip queue asks drm2kgsl for a
deadline queue (`DRM2KGSL_SUBMITQUEUE_DEADLINE`); drm2kgsl made its KGSL context with `KGSL_CONTEXT_PWR_CONSTRAINT` and,
with 0005's `MIN_RATE=0`, set KGSL's highest level (`KGSL_CONSTRAINT_PWR_MAX`) on it at **every** submission; 0005's hold
kept a keep-alive context renewing that constraint to 150 ms after each render-server submission, and 0006 let any other
context's submission within a second of one extend the hold (WidgetRenderer, iconservicesagent and the poster extensions
included). The phone's log says it: `context 4: power constraint at the highest level`, `context 5: constrained while it
submits at an animation's rate`, `clock hold: the top level from the render server's submissions to 150 ms after them`.
So the GPU sat at its top clock for every frame the render server drew, needed or not (a still lock screen redrawing at
40-50 a second: the whole time), and background work kept it there. Every queue had the same KGSL priority (drm2kgsl
offers one timeline, so `KGSL_CONTEXT_PRIORITY_UNDEF`, the kernel's default), so a background context's work queued
level with the front app's and the render server's. On a real iPhone the AGX firmware picks the GPU's clock per frame from
Core Animation's work-interval deadlines; nothing of that reached KGSL here.

**Now** (`src/gpu3d_kgsl/policy.rs`, `src/gpu3d_kgsl.rs`, `drm2kgsl/patches/0008-deadline-level-and-class-priority.patch`; the C header `drm2kgsl/oneios_gpu_policy.h`, the policy's first form, is still included by the patched renderer for two functions):

- **The level follows the frame's deadline.** xnuhost decides it for every GPU submission (a SUBMIT_3D with a
  `GEM_SUBMIT`) of the render server, and of any other non-background context within a second of a render-server frame.
  The budget is the work interval's deadline when the guest sends one (the CPU agent's signal, below), otherwise the frame
  cadence: 8.3 ms (120 Hz) for a frame after a pause, the render server's average frame interval during a burst, at most
  16.7 ms. A frame's GPU time is measured on the host from its submission (or the ring's previous retirement, if later) to
  its KGSL timestamp's retirement, and averaged per level (top, governor's, the first frame after a pause). A frame gets
  the top level (`PWR_MAX` on the deadline queue, and the keep-alive hold to 150 ms after it) only if it would take more
  than 80 % of its budget at the governor's clock: after a pause, its time there if known, else its time at the top times
  KGSL's clock ratio (`/sys/class/kgsl/kgsl-3d0/gpu_available_frequencies`, read once: top over lowest, 4.0 if unreadable);
  during a burst at the governor's, as soon as one frame (nearly) misses. At the top it comes back to the governor after 8
  frames in a row that would fit at the governor's clock in a burst (32, then up to 64, after a frame missed at the
  governor's): their time at the top times what the governor's clock cost this work when its frames last ran there
  (`gov_x16`, learned from each burst frame at the governor's, bounded by KGSL's top over lowest), or twice it before
  that is measured. Until 2026-11-23 this used KGSL's top over lowest, x12.50 on the S24 Ultra (80-1000 MHz): only a
  frame under 0.53 ms at the top could fit at 120 Hz, so the level never came down (the session of 2026-11-23 20:23:
  `frames 548, at the top 548; up +0 down +0`, frames 2.3 ms at the top and 1.8 ms at the governor's), and every
  vCPU and iOS thread carried drm2kgsl's `uclamp.min 1024` for most of the session (section 2.2: it follows the
  constraint).
  The 10 s line says the ratio in use: `the governor's N us (xR of the top's, measured|guessed)`. Easy frames
  (clock ticks, a still screen's redraws, typing, light animations) leave the clock to KGSL's governor; heavy ones (a
  120 Hz scroll of 3 ms frames) keep the top for as long as they come. Nothing new is written anywhere: KGSL's own
  `KGSL_PROP_PWR_CONSTRAINT` as before, on our own KGSL contexts, inside KGSL's thermal limit.
- **Background contexts at the lowest level at a still screen, and only when nobody waits for it** (`og_lowest_for`,
  fix-gpu-widgets). Their queues are made with `KGSL_CONTEXT_PWR_CONSTRAINT` and carry `KGSL_CONSTRAINT_PWR_MIN` from
  their next submission once no counted frame came for 500 ms, `NONE` again otherwise (an ioctl only at a change). KGSL's
  lowest level is the whole GPU's, so it is not asked while any non-background context has a fence not yet retired (that
  work would run at the lowest clock too: a widget renderer drawing at a still screen counts no frame), while another
  process reads the context's surfaces, or once the context's own oldest fence has waited 50 ms (`bg_bound_us`; aging:
  that work runs at the governor's clock and is done well inside Metal's 100 ms wait slice). A submission of any
  non-background context within 200 ms of a background lowest level gets the top level once (the render server's first
  frame did already): KGSL keeps a constraint until it expires and lets only a faster one end it sooner. KGSL takes a
  constraint only if it is faster than the one in force, so this never slows the render server's top level. Background
  contexts never start or extend the hold. `gpu-priority=0` turns the lowest level off.
- **Each context's KGSL priority is its owner's class**, given when its queue is made (DRAWCTXT_CREATE's priority bits;
  KGSL cannot change it later): the render server and the front app 4 (the second ringbuffer, which preempts the third),
  everything else 8, background included. From #42 to fix-gpu-widgets background was 12 (the fourth ringbuffer): KGSL runs
  a lower ringbuffer only while every higher one is empty, so those fences had no bound, and a queue keeps the class its
  context had when the queue was made (by name, or by the role at the context's creation) for good. Within one
  ringbuffer KGSL takes its contexts in turn: that is a background context's minimum share now. None takes the first
  ringbuffer (0-3, Android's compositor's). A priority this KGSL refuses: the queue is made without one, and said once.
- **The class** (`og_class_of`), in order: the name `xnu:backboardd` (the render server); the name `SpringBoard` (front);
  **whether the render server reads the context's surfaces**: xnuhost notes the first guest page of every blob a context
  makes over guest pages (`og_pages`, a cache of 8192 entries); when the render server's context makes a blob over the
  same first page as another process's (an IOSurface it imports, as the guest's `oneios-gpu-pixels` "pages of another
  process's surfaces" matches them), that context is `shown` and never background again, whatever its name or role says
  (`gpu: context N xnu:NAME: the render server reads its surfaces: default now, not background`); then **process names**
  that never draw what the user sees: `photoanalysisd`, `mediaanalysisd`, `assetsd`, `searchd`, `analyticsd`, `suggestd`
  (background; each one's reason is in `src/gpu3d_kgsl/policy.rs`), replaceable with `ONEIOS_GPU_BACKGROUND=` /
  `ONEIOS_GPU_FRONT=` in xnuhost's environment; then **the guest's hint**: the submitting task's effective role
  (`TASK_POLICY_ROLE`, which RunningBoard sets: foreground, control, graphics server, user-init -> front; background,
  throttle, non-UI, DarwinBG -> background), the thread's DarwinBG and its QoS (maintenance, background, utility ->
  background). The hint comes with every fenced submission, so an app sent to the background stops counting for the level
  at once. An older guest sends none: the name decides. Until fix-gpu-widgets the name list also held `WidgetRenderer*`,
  `*Widget*`, `*Poster*`, `iconservices*` and `*Thumbnail*`: home-screen, lock-screen, Today and StandBy widgets, the lock
  screen's poster and the icons, all of which the user sees, ran at priority 12 and at the lowest level.
- **The hint's channel** (guest: `gpu/OneiOSAdreno.cpp` `oneios_gpu_hint`, called from the staged
  `DroidVMVirtioGpu3D.cpp` on the client's own thread, `tools/oneios-tool/crates/oneios-kernel/src/staged/gpu.rs`): CTX_CREATE's
  `context_init` bits 8-23 (beside the capset id) and the SUBMIT_3D header's three padding bytes: [0] `0x80` | `0x40`
  DarwinBG | role, [1] QoS, [2] time to the frame's deadline in 250 us units. The host reads them in `virtio_gpu.rs` and
  hands them to the renderer (`Gpu3d::hint`). **The CPU agent's signal** (fix-host-overhead's hosted CLPC: thread groups,
  work intervals, RunningBoard roles) is read through two weak functions in `OneiOSAdreno.cpp`, whose defaults say "not
  known": `int oneios_clpc_thread_role(thread_t)` (the thread group's role as a `task_role_t`, or -1) and `uint64_t
  oneios_clpc_frame_deadline(thread_t)` (the work interval's deadline in mach absolute time, or 0). A strong definition in
  that work replaces them; nothing else changes (docs/agents.md).
- **Switches** (xnuhost.conf or `--set`): `gpu-level=deadline` (default), `=hold` (0005/0006's rule: every render-server
  frame at the top, to 150 ms after it), `=governor` (never asked); `gpu-priority=0` (the queues' priority as the guest asks,
  upstream's rule, and no queue made able to carry the lowest level, so neither applies). drm2kgsl's own `DRM2KGSL_PWR_HOLD_MS` (150) still sets the hold's length. Before xnuhost sets a level
  (gpuctl) drm2kgsl keeps 0002-0006's rules.

Log lines (xnuhost's log):

```
gpu: level: by deadline (the top only for a frame that would miss 8.3 ms at 120 Hz, or its work interval's deadline, at the governor's clock); KGSL's clocks 231-903 MHz (top/lowest x3.87); priority by the owner's class on (render server and front 4, default and background 8; background at the lowest level at a still screen); background by name (never on screen): photoanalysisd,mediaanalysisd,assetsd,searchd,analyticsd,suggestd
drm2kgsl: clock hold: the top level only for frames that need it (xnuhost's deadline policy), to 150 ms after them
drm2kgsl: context 5: a deadline queue: at the top level only for frames that would miss their deadline (xnuhost's policy)
drm2kgsl: queue 7 of xnu:photoanalysisd: background, KGSL priority 8
gpu: context 12 xnu:photoanalysisd: background (by name; role 0, QoS 4), KGSL priority 8
gpu: context 24 xnu:WidgetRenderer_Default: background (by the guest's role; role 6, QoS 5), KGSL priority 8
gpu: context 24 xnu:WidgetRenderer_Default: the render server reads its surfaces: default now, not background
gpu: 10 s level: frames 610, at the top 40; up +2 down +2; missed at the governor's +0, over budget +1; GPU time at the top 900 us, the governor's 2100 us, after a pause 2600 us; frame interval 8400 us; background submissions at the lowest +35 (kept off it: others' work queued, surfaces read, or waited 50 ms +12; ended by others' work +3)
gpu: 10 s fences over 100 ms (each one of the guest's fence timeouts): xnu:photoanalysisd 2 (max 130 ms; background), xnu:Foo 0 (max 0 ms, one waiting now 240 ms; default)
drm2kgsl: context 9: power constraint at the lowest level
```

The `drm2kgsl:` facts above are `xnuhost I` lines. drm2kgsl prints them at virglrenderer's WARNING level (so that
the library's threshold passes them); until 2026-11-22 xnuhost mapped WARNING to `xnuhost E`, and all 28 `E` lines of
a session were such facts. Only `drm_err` (ERROR) is an `xnuhost E` line now (`gpu3d_kgsl.rs`, docs/booter.md 2.9b).

(examples, not from a run.) `gpu: context` comes once per context (two per process) and when its class changes: a
process whose contexts keep coming back is making Metal devices again and again. The 10 s line says how much of the
render server's work needed the top; `over budget` is frames that took longer than their budget at any level (jank).
A frame interval far above the GPU time is not the GPU's: on 2026-11-22 (s9) the render server drew at 43-82 ms intervals
with 1.4-5.6 ms of GPU time at the top, because its render thread waits every frame for metal2vulkan's submission thread
(`CA::OGL::MetalContext::flush` -> `dispatch_group_wait` in the plug-in), a default-class thread that the Edge scheduler
kept on the two saturated E vCPUs; hosted `0085` lets it run on the P cores (kernel/patches/oneios-hosted/README.md,
"0085"; xnuhost's `perf: cpuN:` lines show the clusters' load).
**`gpu: 10 s fences over 100 ms`** (fix-gpu-widgets, a lasting line, only when there are any) names the processes whose
fences took 100 ms or more from submission to retirement on the host in those 10 s (count, longest, class), and any
whose oldest fence has been waiting that long now. Metal's runtime waits for a fence in slices of 100 ms and waits again
after each (`metal2vulkan` `command_buffer/completion.rs` `WAIT_SLICE`; a wait never fails on time, it only counts); the
tunnel counts each slice that runs out as one of `oneios-gpu-report`'s `fence timeouts`. So that guest figure is "fences
over 100 ms", and this line says whose.

**The widgets' GPU starvation (fix-gpu-widgets, 2026-11-15).** On the phone (main 8644ae9 with the v0.1.9 runtime,
`ram=10G pen=auto`): `gpu: context 24 xnu:WidgetRenderer_Default: background (by name (STOPGAP list); role 1, QoS 6),
KGSL priority 12` and `drm2kgsl: queue 9 of xnu:WidgetRenderer_Default: background, KGSL priority 12`; `gpu: 10 s host:
backboardd fences 84, submit->retired mean 6654 us max 49783 us; ... other fences 85, mean 181112 us`; the guest's
`oneios-gpu-report: ... fence timeouts +5`; no KGSL fault, reset or crash. The guest's role was foreground (1) at QoS
user-interactive (6), so the name list alone made it background. From the code: priority 12 is the fourth ringbuffer,
run only while the other three are empty; and the screen was still, so every submission of WidgetRenderer_Default (and of
the other listed processes) put the whole GPU at KGSL's lowest level for KGSL's expiry after it, while its own six
surfaces' work queued behind itself. Proven from the counts: the fences of every process but the render server took
181 ms on average and five of Metal's 100 ms wait slices ran out. Not proven: how much of the 181 ms each rule caused
(the host's line gave no per-process figure then; the new `fences over 100 ms` line does). The white widgets themselves
have another cause (chronod cannot save widget snapshots without an HEVC encoder: `fix-widget-snapshots`); this fix is
the starvation and the fence timeouts. The fix: the name list keeps only daemons that never draw what the user sees, a
context whose surfaces the render server reads is never background, no class is put under the default's ringbuffer, and
the lowest level is not asked while anyone else's work is queued, while someone reads the context's surfaces, or after its
own fence has waited 50 ms (above). The phone lines that will prove it: `gpu: context N xnu:WidgetRenderer_Default:
default` or `... the render server reads its surfaces: default now, not background`, `drm2kgsl: queue N of
xnu:WidgetRenderer_Default: default, KGSL priority 8`; `gpu: 10 s host: ... other fences N, mean` in single-digit ms
(below 10000 us); the guest's `oneios-gpu-report: ... fence timeouts +0`; no `gpu: 10 s fences over 100 ms` line naming
WidgetRenderer_Default.

**Tested on the PC** (no KGSL; the C tests of 2026-11, ported to Rust on 2026-11-23, section 3): `tests/test_gpu_policy.c` (now `src/gpu3d_kgsl/policy/tests.rs`; 166 checks: classes from
names, hints and `shown`, priorities, the surface table, the level for each kind of stream, deadlines, background, the
lowest level's rules, the follow window, the three modes, and the starvation model below) runs here. Its last part runs the same frame streams under `gpu-level=hold` and `=deadline` (frames at the governor's
lowest clock taken as 4 times their top-level time, the hold 150 ms), the share of time the clock is held at the top:

| Stream (10 s) | hold (before) | deadline (now) | frames over budget, both |
|---|---|---|---|
| still lock screen redrawing, 45 frames/s of 0.4 ms | 100 % | 3.1 % | 0 |
| clock tick, 1 frame/s of 0.3 ms | 15 % | 1.5 % | 0 |
| typing, a key every 150 ms: 3 frames of 1 ms at 120 Hz | 100 % | 1.7 % | 0 |
| scrolling, 120 frames/s of 3 ms | 100 % | 100 % | 0 |
| a light animation, 60 frames/s of 0.8 ms | 100 % | 2.7 % | 0 |

These are the model's numbers, not the phone's. **The starvation model** (fix-gpu-widgets, `starvation()` in the same
test): a GPU in 50 us ticks with KGSL's rules as the model takes them (ringbuffers by strict priority, a higher one
preempting; in a ringbuffer a job runs to its end and the contexts take turns, up to 5 jobs each; the top clock while a
`PWR_MAX` holds, 4 times slower while a `PWR_MIN` holds (80 ms after the submission that set it), 2 times slower at the
governor's: assumptions, not measurements). A still home screen, 10 s: once a second WidgetRenderer_Default (a non-UI
process at utility QoS to the guest, the worst its hint can say) redraws 6 surfaces, one fence each, and 2 ms after the
last the render server composites 3 frames of it; photoanalysisd computes all the time (8 ms jobs). Under #42's rules
(widget background by name, priority 12, the lowest level whenever the screen is still) and now:

| Case | WidgetRenderer fences: mean / max, #42 -> now | over 50 ms | over 100 ms | photoanalysisd max | time at the lowest |
|---|---|---|---|---|---|
| small widgets (1 ms a surface) | 34.6 / 83.5 ms -> 8.7 / 19.4 ms | 10 -> 0 | 0 -> 0 | 51.0 -> 32.0 ms | 45.9 % -> 44.2 % |
| heavy widgets (5 ms a surface) | 88.1 / 154.2 ms -> 19.5 / 38.9 ms | 50 -> 0 | 22 -> 0 | 105.5 -> 32.0 ms | 43.2 % -> 42.1 % |
| heavy, SpringBoard draws a frame too | 26.5 / 44.9 ms -> 22.2 / 41.9 ms | 0 -> 0 | 0 -> 0 | 36.1 -> 34.0 ms | 41.9 % -> 41.8 % |

(In the third case SpringBoard's frame, a counted one, ended the lowest level under #42's rules too.) The heavy case
under #42's rules is the phone's pattern (a mean near 90 ms, fences over Metal's 100 ms slice); the time the GPU spends
at its lowest level hardly changes, so the power it saves stays. `test_xnuhost` (now `three_d`): CTX_CREATE's hint bits and SUBMIT_3D's
padding reach the renderer. `test_drm2kgsl`'s policy scenarios (now `classes` and `shown_surfaces` in `src/gpu3d_kgsl/tests.rs`) run on this PC with a stand-in for `/dev/udmabuf` (a
memfd per UDMABUF_CREATE_LIST: `/dev/udmabuf` is root:kvm here) and on the Mac's container with the real one: queue
priorities by class (the render server 4 with its constraint flag, iconservicesagent 8 without it, an app 4 in front,
8 with the flag sent to the background, photoanalysisd 8 with the flag; 13 checks), and a context whose surfaces the
render server reads (WidgetRenderer_Default non-UI: background, a queue with the flag; after the render server's blob
over its surface's first page: `default now, not background`, its next queues without the flag, also after a new hint;
two contexts of one process over the same pages change nothing; 17 checks). The memory scenarios still need the real
`/dev/udmabuf`. fix-gpu-widgets on this PC: `test_gpu_policy` 166 checks, `test_drm2kgsl` 13 + 17, `test_xnuhost` 14032 (on main 03591b6),
`test_imageset` 37, `test_display_mode`, `check-arm64` (the Android xnuhost booting the test kernel under qemu-aarch64)
all pass; `test_gpu_leaks` is skipped (no `/dev/udmabuf`). No PC boot for it: qemu has no KGSL, so no GPU context, class,
fence or level of this code runs there. **Not run on the phone.** The guest side (from fix-gpu-perf; fix-gpu-widgets
changes no guest code, so `kernel.kc` stays as it is): `HOSTED_OUT=fixgpu` built
(`xnu.macho` 55ea1a18..., `_oneios_gpu_hint` and the two weak CLPC functions in it), prelinked (audit 0 failures, 2711
paired; `stubs-check` 666 good; `kernel.kc` 49f718cb...), and booted on the PC (qemu-aarch64, `--ram 4G`,
`oneios-gpu-metal=1 oneios-adreno-selftest=1`, `--set gpu-level=deadline --set gpu-priority=1`): the accelerator at stage 5,
both self-test lines as before, launchd at +282 s, SpringBoard and backboardd at +364 s, 10 minutes with no panic, stopped
by hand. qemu has no 3D, so no context, hint or level ran there. **Not run on the phone.** The phone test is in
`docs/newtodo.md` (update of 2026-11-15).

### 2.3 `drm2kgsl/patches/0004-gpu-address-room-by-process.patch`

Every GPU context gets its own part of the VBO's GPU addresses (a KGSL page table is per process, and every guest
process shares xnuhost's). Upstream cut the VBO into 32 equal slices: 6 GiB each here, so 32 contexts. Every iOS
process that draws with Metal holds two (phone, 2026-11-12: backboardd, WidgetRenderer_Default and
iconservicesagent, `contexts +2` each), so that was 16 processes, too few once every process may have a device. The
patch cuts slices of two sizes from the VBO in 1 GiB units as contexts come, first fit: a context whose name (the
guest's `xnu:<process>`) is in `DRM2KGSL_VA_RENDER_NAMES` (default `xnu:backboardd`) gets `DRM2KGSL_VA_RENDER_GB`
(12; the render server maps every client's surfaces into its own addresses, and an iPadOS one used 1.8 GB of a 2 GB
slice at an idle home screen), any other `DRM2KGSL_VA_CLIENT_GB` (2, upstream's minimum: Turnip takes the size as
the context's whole budget). On this phone's 192 GiB: the render server's two and 84 others, 42 processes. A
context with no room is not made: one log line (`context xnu:NAME: no room for 2 GiB of GPU addresses (N contexts
hold M of 192 GiB): not made`), the device answers `CTX_CREATE` with `ERR_OUT_OF_MEMORY`, the guest's context init
fails, the plug-in reports no device and the process draws without the GPU (the path an unlisted process takes).
Each context's own line: `context xnu:NAME: GPU addresses [base, +N GB); N contexts hold M of 192 GiB`
(drm2kgsl's debug log). Host tests: `test_drm2kgsl` and `test_gpu_leaks` (now `src/gpu3d_kgsl/tests.rs`) pass with it; not yet run on the phone.

### 2.4 `drm2kgsl/patches/0007-import-coherence-read-back.patch` (widgets, 2026-11-13)

Nothing in the guest cleans or invalidates a cache for GPU memory: the guest's pages are imported into KGSL
`KGSL_MEMFLAGS_IOCOHERENT` (upstream drm2kgsl's `kgsl_guest_memflags`, with `DRM2KGSL_NONCOHERENT` unset), so the GPU snoops the
CPUs' caches, and an IOSurface one process's GPU writes is the same memory another process's GPU or CPU reads (the
plug-in's own `sys_dcache_flush` of a surface is off on iOS 27: `NativeBridgeMetalFrontend.mm` `kSurfaceFlush`). KGSL
drops that flag without saying so where its MMU has no IO coherence (`kgsl_memdesc_init`); then a CPU write can stay in
a cache the GPU never reads, and a GPU write can be overwritten later by an older line a CPU writes back (a surface the
guest's kernel zeroed with the CPU and an app then drew with the GPU would go blank again). Whether this KGSL keeps it
has never been looked at. The patch reads each import's flags back (`GPUOBJ_INFO`: the first 256 imports, then every
64th) and counts the outcomes: kept IO coherence, dropped by KGSL, imported without it as asked (drm2kgsl's attach of
another context's dma-buf asks flags 0), not readable. Log: one drm2kgsl line for the first import of each outcome
(`guest import: KGSL kept IO coherence: asked flags 0x8c000000, now 0x... (cache mode 3)`, or `... DROPPED IO coherence
...`), and in the 10 s host report, the first time and again whenever a non-coherent count moves:

```
gpu: KGSL imports N, read back C: IO-coherent K, coherence dropped by KGSL D, not coherent as asked P, not readable U
```

`D` above 0 on the phone means guest memory is not coherent with the GPU: the fix would then be cache maintenance where
the guest hands memory to the GPU (or `DRM2KGSL_NONCOHERENT` turned the other way), not more counters. Host test:
`test_drm2kgsl`'s third scenario (now `coherence`; the stand-in KGSL keeps or, as an MMU without IO coherence would, drops the flag; an
attach asks none): built on the Linux PC (2026-11-13), not run there (no access to `/dev/udmabuf`, section 3); the Mac's
container runs it. Not yet run on the phone.

### 2.4a `drm2kgsl/patches/0009-id-0-is-no-key.patch` (2026-11-23)

A guest request that names id 0 ended xnuhost. Mesa's hash tables reserve the NULL key, and drm_context's resource and
blob tables and drm2kgsl's queue table use the guest's id as the key, so a search for 0 is `hash_table.c:311:
hash_table_search: Assertion "!key_pointer_is_reserved(ht, key)" failed` (abort). The phone did it at 13:51 on
2026-11-23 (`out/session-logs/2026-11-23/boot-jobs/xnuhost-final.log`, the wallpaper gallery open): two
`RESOURCE_CREATE_BLOB` refused (`command 10c response 1205`), two `RESOURCE_UNREF` of the ids the host never made
(`command 102 response 1203`), then the abort. The host test below reproduces that line, and its backtrace is
`kgsl_ccmd_gem_set_iova` -> `kgsl_object_from_res_id` -> `drm_context_get_object_from_res_id(0)`: a guest BO whose
blob was refused has no resource id, and the guest still clears its GPU address with `GEM_SET_IOVA` of res_id 0. The
device's own commands were never the path: `virtio_gpu.rs` refuses resource and context 0 itself, and
virglrenderer's resource and context tables (`util_hash_table_get`) answer key 0 with "not found".

The patch makes 0 "no such id" before each of drm2kgsl's searches: `kgsl_object_from_res_id` (GEM_SET_IOVA,
GEM_UPLOAD, the BOs of GEM_SUBMIT, attach, export), `kgsl_object_from_blob_id` (also a blob id above 32 bits, which
`drm_context_retrieve_object_from_blob_id` asserts), `get_blob`'s res_id check, GEM_SUBMIT's queue lookup and
`SUBMITQUEUE_CLOSE`. Each answers with its usual error (`Could not lookup obj: res_id=0 (ignored, not escalated)`, `No
obj: res_id=0`, `unknown submitqueue: 0`, `-EINVAL`). Host test: `zero_ids` in `src/gpu3d_kgsl/tests.rs` (section 3)
aborted with the phone's assertion before the patch and passes with it. Not yet run on the phone.

Why the two blobs were refused (`INVALID_PARAMETER`): the guest logs only the response, and `virtio_gpu.rs` said
nothing. Since this change each refused `RESOURCE_CREATE_BLOB` is one line with the check that failed (the first 32,
then every 1024th; the answer is unchanged). The 18:24 session's lines then named the entry count (section 2.4b):

```
virtio-gpu: blob R of context C refused (K so far): mem M flags F, S bytes, E entries: entry I at A, L bytes, is not guest RAM
```

The other checks it names: no such blob memory; the size or entry count (at most 65532, the guest driver's
`MaxBlobEntries`, section 2.4b; guest memory needs entries); the request holds fewer entry bytes than it says; an entry
that is not whole 4 KiB pages; entries that hold less than the blob. Host test: `virtio_gpu::tests::three_d` (its
not-guest-RAM case checks the line). Every `gpu: 10 s: host blobs` line ends with `refused R`, the refused blobs since
the start (2026-11-23).

### 2.4b `drm2kgsl/patches/0010-object-without-res-id.patch` and the guest's blob sizes (2026-11-23)

0009 was not the whole fix. The phone ended again with the same `hash_table.c:311` line at 84283.8 s in the 18:24
session (runtime 0.1.18 with 0009, `out/session-logs/2026-11-23-mac-s1824/xnuhost-final.log`). Before it the device
refused 20 blobs of one wallpaper, Unity2025Poster (contexts 83 and 87), all with the entry-count check:
`virtio-gpu: blob 9814 of context 83 refused (1 so far): mem 3 flags 0x9, 83886080 bytes, 20480 entries: size or
entry count (at most 16384; guest memory needs entries)` (one was 75022336 bytes, 18316 entries).

**Two causes.**
- **The limit.** `virtio_gpu.rs` `MAX_BLOB_ENTRIES` was 16384, `DroidVMVirtioGpu.h` `MaxEntries`: the display's limit
  for a 2D surface. The guest's 3D blobs have their own, `DroidVMVirtioGpu3D.cpp` `MaxBlobEntries =
  (HugeRequest - sizeof(CreateBlob)) / sizeof(Entry)` = (1 MiB - 56) / 16 = **65532** (one entry a page, up to 256 MiB
  of ordinary wired pages, `droidvm_virtgpu_contiguous = 0`). The device now takes that (the request is the 1 MiB
  `command` already accepts). `udmabufs` makes an 80 MiB blob of 20480 single pages into 20 udmabufs of 1024 pages;
  drm2kgsl binds them in one `BIND_RANGES` (section 2.1). `MAX_BACKING` (2D) and the display's `DIRECT_SEGS` stay
  16384: they are the display's `MaxEntries` (a blob with more segments is copied, not posted).
- **The abort.** The guest's `GEM_NEW` makes the BO with its blob id first (drm2kgsl puts it in the context's blob
  table). The device refused the `CREATE_BLOB`, so the renderer never gave the BO a res_id: it stayed in the blob
  table with res_id 0. At the context's end `drm_context_deinit` frees what is in the blob table (`free_blob` ->
  `drm_context_free_object` -> `drm_context_remove_object`), and `remove_object` searched the resource table for
  res_id 0: mesa's reserved key. The host test's backtrace is exactly that, from `ctx_destroy`
  (`virgl_context_remove` -> `kgsl_renderer_destroy` -> `drm_context_deinit`). 0009 patched only
  `drm2kgsl_renderer.c`; this is in `drm_context.c`, which `build.rs` now patches too.

**The patch** (`drm_context.c`): `drm_context_remove_object` returns for an object with res_id 0 (in no resource
table); `drm_context_get_object_from_res_id(0)` is NULL; `drm_context_res_id_unused(0)` is false (0 is never free to
take); `drm_context_retrieve_object_from_blob_id` answers blob id 0, or one over 32 bits, with NULL (its assert is
gone). So no res_id or blob id of the guest's reaches a search with the reserved key in `drm_context.c`; 0009 covers
the queue table in `drm2kgsl_renderer.c`. The refused BO is freed at its context's end as before (KGSL keeps nothing:
the test checks only the VBO is left).

**Host tests** (section 3): `gpu3d_kgsl::tests::refused_blob_then_context_end` is the guest's sequence (`GEM_NEW`
with a blob id, no `CREATE_BLOB`, the unref of the refused resource, `GEM_SET_IOVA` and a submit naming res_id 0, the
context's end, then a new context's first buffers and submits, and a second refused BO beside another context). It
aborted with the phone's `hash_table.c:311` line before the patch and passes with it. `virtio_gpu::tests::
blob_entries_of_the_guest_driver`: blobs of 20480, 18316 and 65532 entries are made, 65533 is refused with its line;
`three_d`'s `udmabufs_here` splits the 20480 single pages into 20 udmabufs of 1024 pages.

**On the phone** (not booted yet): no `hash_table.c` line, no `virtio-gpu: blob ... refused` line for the 80 MiB
blobs, and `gpu: 10 s: host blobs N (M MiB), made +A unref'd +B, refused 0`. The guest's `command 10c response 1205`
lines for them go; a `command 102 response 1203` is then only an unref of an id the host never made.

To change it: edit a copy of the pinned file, `diff -u` against the original into `drm2kgsl/patches/` (one file a patch, named on its `+++ b/` line), then `cargo build -p xnuhost` (`drm2kgsl/build.rs` applies the series again).
A new pin of virglrenderer that the patch no longer applies to stops the build (`patch -F0`).

**Threads and fences.** With a renderer the control queue is served by one thread of the device, never by a vCPU's
trap handler. A command fenced on a context's ring (`FLAG_FENCE | FLAG_RING_IDX`) is answered at once but its chain
goes back to the guest only when the ring passes the fence (drm2kgsl's timeline threads call back); a fence on the
device's own ring is returned at once. A context that is destroyed returns its held commands. A device reset
destroys the contexts and blobs on the device's thread and drops what was held.

**Two lanes** (2026-11-12). The queue's thread serves the render server's contexts (named `xnu:backboardd` at
`CTX_CREATE`), context creation and every command of no context (the display's flush, the 2D commands) at once; every
other context's commands go, in their order, to a second thread, the slow lane. A mutex makes the two take turns at the
device's tables and the renderer, so the render server's frame waits for at most one of an app's blobs (1-30 ms each on
the phone, 64-102 per 10 s while Settings opened), not for all of them. Since 2026-11-21 that bound holds: the slow
lane starts no blob while the queue's thread has commands to serve (`fast_waiting`). Before, the mutex was not fair:
the slow lane's thread, unlocking and locking it again on its own CPU between blobs, mostly won it back, and between
the lane choice of an app's next blob and the render server's submit it took the next blob's turn, so a frame could
wait for several blobs. A fast-lane command that names a resource the
slow lane has not made yet (an app's surface the render server attaches, the unref of an app's blob) waits for the slow
lane's commands that came before it and runs again. The count of those commands is read only then, under the lock that wait takes (2026-11-28, from YMGPwcca/OneiOS
`8be9ec18`: only the queue's thread queues slow-lane commands, so it is the count at the command's place), not before
every fast command: a lock and unlock fewer per command. A device reset drops the slow lane's chains of the old queue. Test:
`test_virtio_gpu3d` (now `three_d`; all its contexts are the slow lane's now, and one case holds two of an app's blobs: the render
server's submission is answered before both, or right after the first when the slow lane had started it, never after
the second; 8 runs green with every core of the PC busy, where the old one-blob check failed 2 of 3: it asked for the
submission before the one blob, which the slow lane may rightly have started first; the test's `driver_send_at` puts
the third outstanding chain at descriptor 2, clear of the other two). The test cannot force the scheduling that showed the unfair mutex; that part of the fix is from the
code. Not yet run on the phone.

**Display.** `SET_SCANOUT_BLOB` makes a guest blob the scanout; `RESOURCE_FLUSH` copies its rows from the guest's
pages to the display (a CPU copy per frame: crosvm's zero-copy import to the compositor is not done here).
Since 2026-11-12 the copy goes straight from the guest's pages into the display's buffer, walking the blob's segments
once per flush; before, each of the 3120 rows went through a row buffer and searched the segment list from its start
(about 1100 segments of 16 KiB pages for a 1440x3120 frame). On an M4, for that frame: 4.2-10.6 ms before, 2.0-3.3 ms
after (a stand-alone copy of both loops; not measured on the phone). Since 2026-11-28 a scanout whose layout is the display's
(B,G,R,X/A onto B,G,R,X, half floats onto half floats) is copied byte for byte per segment, without the per-segment
conversion call and the split-pixel carry (YMGPwcca/OneiOS `8be9ec18`; host-tested, not measured on the phone). xnuhost logs the copy every 10 s while frames
come: `display: 10 s: flushes N, copy mean X us max Y us (Z MiB)`. The fast lane's thread does the copy, so the render server's
next commands wait behind it. The guest's `oneios-display-report` `done=` of a swap (128-211 ms on the phone,
2026-11-12) runs from the swap's validation to its transfer: the frame's GPU work, then this flush, each behind
whatever the device thread is doing (blob creations: 0.8 ms each, up to 45 ms in a storm).

**Display.** `SET_SCANOUT_BLOB` makes a guest blob the scanout. At `RESOURCE_FLUSH` its rows go from the guest's pages
to the display in one CPU copy: made by the helper from guest RAM (direct frames, `docs/booter.md` 2.8), with the flush
answered when the frame is in the layer; without a direct helper xnuhost copies them into the memfd, walking the
blob's segments once per flush (before, each of the 3120 rows went through a row buffer and searched the segment list
from its start: about 1100 segments of 16 KiB pages for a 1440x3120 frame). On an M4, for that frame: 4.2-10.6 ms
before, 2.0-3.3 ms after (a stand-alone copy of both loops; not measured on the phone). Not zero copies: see 2.8 for
why. xnuhost logs the copy every 10 s while frames come (`display: 10 s: flushes N, copy mean X us max Y us (Z MiB)`;
the direct path's own 10 s line is in 2.8).
The guest side is `kernel/patches/oneios-hosted/display/` (its README): iOS's framebuffer hands each of
`backboardd`'s surfaces to this device, as a guest blob with a renderer and as a 2D resource over the surface's
pages without one (the PC). What the 3D path still needs from the display is listed there.

**Options.** `--gpu auto` (default: 3D when `/dev/kgsl-3d0`, `/dev/udmabuf` and `/dev/dma_heap/system` open and
KGSL takes the probe), `--gpu 3d` (or do not start), `--gpu 2d`. The reason for 2D is in the log (`gpu: 2D only: ...`).
The default is the host phone's when its table entry has one (`docs/hardware.md` 7h: the Galaxy A56, which has no KGSL and no
Xclipse 3D path here (`docs/a56.md`, "The Xclipse host half": the other fork's AMDGPU renderer left iOS's UI black),
is `2d`); the command line and the image set's `gpu=` win over it.
drm2kgsl's own switches are environment variables it reads (`DRM2KGSL_PRIORITIES`, `CROSVM_DRM2KGSL_DIAG=1`).

**Open files.** Every guest BO is an fd per udmabuf chunk in xnuhost and one more in each context that imports it,
and every Metal client is a KGSL fd and a dma-heap fd. The renderer's start raises the soft `RLIMIT_NOFILE` to the
hard limit (no privilege needed; `gpu: open files: soft limit N -> M` in the log when it changed). Droid-VM's crosvm
ran out of fds under GFXBench before it did the same.

**The host's share of a frame.** The guest's GPU report times a render-server frame from submission to the fence it
sees (9-11 ms on the phone on 2026-11-12, with KGSL 7 % busy). `gpu3d_kgsl.rs` times the host's part, one line per 10 s
while fences retire: `gpu: 10 s host: backboardd fences N, submit->retired mean X us max Y us; submit calls mean X us max
Y us; other fences N, mean X us; blobs made N, mean X us max Y us (udmabufs X us of it); clock held +N times, keep-alives +M`. "blobs made" is the host's part of each guest BO
(udmabufs of its pages, drm2kgsl's import and bind): every Turnip BO is one (a process drawing icons held 1335 under 1 MiB). "submit->retired" runs from the fence reaching the renderer to drm2kgsl's retire
callback (the GPU's work and KGSL's wake-up); "submit calls" is the time spent in drm2kgsl's submit. What the guest
sees beyond that is the virtio round trip and the guest's own wake-ups. The render server's contexts are those named
`xnu:backboardd`. Not yet run on the phone.

**Session rules** (`docs/booter.md` 2.9): nothing new outside the process. KGSL contexts, the VBO and the dma-bufs
are descriptors of xnuhost and go with it at SIGKILL; drm2kgsl's fence threads are ordinary threads.

### 2.5 One frame end to end, and the way to 120 fps (fix-fps-scheduler-migration, 2026-11-23)

At 120 Hz a frame has 8.3 ms. The numbers below are from the phone's boot of 2026-11-23 03:28 (main `b99dcfe` with
SM-S928W, kernel `988c2a7b`: hosted 0084-0089, 0085's spill active), xnuhost's own lines, unless another source is
named. "Active" is the 10 s periods with page switches (30023-30047 in that log), "still" the clock-only periods
after them (about 100 frames in 10 s).

| Step of a frame | Line | Still | Active |
|---|---|---|---|
| frame interval of the render server | `gpu: 10 s level: ... frame interval` | 95-99 ms (only the clock) | 13.7-23.4 ms (43-73 fps) |
| GPU time of a frame at the top clock | `gpu: 10 s level: ... GPU time at the top` | 1.4-1.7 ms | 2.3-8.2 ms |
| the render server's fence, submit -> retired | `gpu: 10 s host: backboardd fences` | 1.6 ms (max 2-4 ms) | 7.6-11.9 ms (max 34-90 ms) |
| metal2vulkan's submit call in drm2kgsl | `... submit calls mean` | 49-51 us | 23-48 us |
| the display helper's answer to a frame | `display: 10 s: ... (posted to in the layer: mean` (since 2026-11-23 `(its answer: mean`) | 5.1-5.2 ms | 8.2-9.1 ms (max 30-40 ms) |
| the same, split (an older helper, 2026-11-22 `kbd-debug/display-s3.log`, still screen) | `oneios-display: 10 s: ...` | guest pages read 5.5 ms, buffer wait 4.1 ms, copy 0.2 ms | not logged |
| the render server's own CPU | `oneios-top: backboardd` (30 s) | 28% | 158%, 98,068 vm_faults (4.1 s in vm_fault), 158,612 own unmaps |
| the host | `thermal: 30 s` | 567% of 800 | 711% of 800, SoC 95 C ("heavy" for 85 s) |

The boot before (kernel `81b85dd4`, 0085 not active) had the same GPU figures (fences 5.9-13.4 ms active, 1.6 ms
still) and active frame intervals of 15-35 ms; the GPU is not where the newest boot lost time. What the frame waits
for, in order:

1. **The display helper's answer (fixed here).** The guest's display driver posts its next frame only after xnuhost
   answers its flush (`DroidVMVirtioGpuDisplay.cpp`), and xnuhost answers when the helper says `done`. Until this
   change the helper said it after `ANativeWindow_lock` (which waits for a free buffer, to the panel's vsync when the
   queue is full), the fill and the post, though the frame was in the helper's own snapshot before all three. The helper
   now answers as soon as a changed frame is in its snapshot (`runtime/display/src/jni.rs`, the C's `jni.c` then; `runtime/display/README.md`,
   "Frames"). What it removes from the guest's path: the buffer wait (4.1 ms mean in the 2026-11-22 log) and the fill.
   What stays: the read of the guest's pages into the snapshot (5.5 ms then). Phone lines that prove it: xnuhost's
   `display: 10 s: ... (its answer: mean X ms ...)` against the 5.1-9.1 ms above, and the helper's `oneios-display:
   10 s: ... guest pages read ... buffer wait ...` (`phone pull-log` or `control/display.log`): the answer should be
   about the read.
2. **The read itself: a CPU copy of up to 18 MB a frame** (1440x3120x4; the helper hashes each tile and copies the
   changed ones). 5.5 ms of one core for a still screen, more when the host is busy (8-9 ms active above, with every
   core taken). The fix is not a faster copy but none: a GPU copy into the layer's buffer (KGSL can import both the
   compositor's surface, a udmabuf of guest pages it has already, and the gralloc buffer, a dma-buf), or a scanout
   of the guest's surface itself. Not done; it needs the gralloc buffer's dma-buf on xnuhost's side (the helper is
   another process: the fd over `display.sock`) and a KGSL blit of drm2kgsl's. [guess: a blit of 18 MB at the GPU's
   bandwidth is well under 1 ms.]
3. **The fence path under load.** Active, the render server's fences took 7.6-11.9 ms from submission to retirement
   for 2.3-8.2 ms of GPU time: the rest is other contexts' work ahead on the GPU and the host's way back (KGSL's
   timestamp wake, drm2kgsl's fence thread, the guest's interrupt, backboardd's wake), on a host with every core busy.
   Next: split the line into queue wait, GPU run and the way back (a lasting part of `gpu: 10 s host`), then act on
   the largest.
4. **The render server's faults.** 98,068 vm_faults (4,114 ms in vm_fault) and 158,612 unmaps of its own in the 30 s
   in which it drew 767 frames (167 + 279 + 321 in the `gpu: 10 s level` lines): about 128 faults, 207 unmaps and 5.4
   ms of fault time per frame, spread over its threads (how much of it is on the render thread's path is not known).
   This is the largest CPU cost per frame in these lines. Next: find which mappings it makes and drops per frame
   (`oneios-tool log stacks`, the 0080 `oneios-top` line; an IOSurface mapped and unmapped per frame is a guess) and keep
   them mapped.
   **Read from the same session's `oneios-top` lines (2026-11-23, build laptop; not a fix yet).** The guess of a
   per-frame IOSurface map is not what the lines show. backboardd's top calls in that 30 s: `-47` (mach_msg2) 41,409,
   `478` (bsdthread_ctl) 37,218, **`75` (madvise) 32,255**, `-18` (mach_port_deallocate) 20,313; no mach_vm_map or
   munmap among the six. Idle (17-18% CPU) it is 7,380 madvise calls, 16,363 faults and 32,782 own unmaps per 30 s.
   The faults follow the madvise calls at about 0.5 per own unmap, and the kernel's `oneios-perf` rows count nearly
   every vm_fault as `refilled` (a page that was there before). That fits libmalloc giving back freed pages with
   `madvise(MADV_FREE_REUSABLE)` [guess: the advice is not logged]. XNU then clears the pages' referenced and modified
   bits (`deactivate_pages_in_object`, `DEACTIVATE_KILL`: `pmap_clear_refmod_options`). On an iPhone that costs one
   cheap access-flag fault at the next touch. In the hosted pmap, clearing the bits flushes the host mapping
   (`task_unmap`, counted as "own unmaps"), so the next touch is a full fault of 30-40 us. What is left to do: (a) a
   lasting count of madvise by advice in the `oneios-top` line, to prove the guess; (b) in the hosted pmap, a clear of
   the referenced bit alone that keeps the host mapping (the page counts as referenced: only the pageout's LRU order
   loses precision), and a clear of the modified bit that write-protects the host page instead of unmapping it, so
   reads after the madvise do not fault. The modified bit must stay exact: a clean internal page with no compressor
   copy is freed by the pageout, so a write that leaves no fault would lose data.
   **Hosted 0101 (fix/unmap-fanout, 2026-11-24) does part of (b).** The clear still unmaps (the referenced bit stays
   exact), but the touch after it no longer takes the full fault: the fill path sees an entry that waits for its
   reference or modified fault, takes it there (`arm_fast_fault`) and maps the page at once (`oneios-fill-fast-fault`), and
   setting the bit again unmaps nothing (`oneios-upgrade-flush`). Its `oneios-perf:   0101:` lines count both (`fast fills`,
   `kept mapped`) and the flushes by cause (`refmod` is this one). Not done: the write-protect instead of the unmap for a
   clear of the modified bit alone. Not run on the phone yet.
5. **The host's CPU time.** With 0085 and 0089 all eight vCPUs carry iOS's work, so the display helper and xnuhost's
   GPU threads share the cores with it; at 711% and a 95 C SoC the phone also slows its clocks. Hosted `0091` and
   xnuhost's direct kick (`docs/booter.md` 2.6a, "The kick") remove the host work of the thread moves and of the
   kicks (70,200 moves and 24,419 kicks in that 30 s period, 136 us each). Phone lines: `perf:   release:`, `kick`,
   `perf:   queue: ... (N of them by a kick of the iOS thread ...)` and the kernel's `oneios-perf:   0091:` line.

The order to work in, by the size of each cost per frame against the 8.3 ms: 4 (5.4 ms of faults), 2 (5.5 ms of copy
on the CPU), 3 (5-9 ms of fence time beyond the GPU's work), then 5. 1 and the host work of 5 are done in this change
(not yet booted on the phone). The Rust runtime (branch rust-runtime, 2026-11-23) has both: xnuhost's line
`display: 10 s: ... (its answer: mean X ms ...)` in `runtime/xnuhost/src/display.rs` and the direct kick in
`src/task.rs` `kick` (check-arm64's boots passed under qemu-aarch64; not on the phone).

### 2.5a One frame's stages in one line, and the helper's two threads (fix/frame-path, 2026-11-24)

The owner (2026-11-24): app open and close animations run at 10 fps or worse; the goal is the panel's 120. What the
newest session said (`out/session-logs/2026-11-24-mac-gpumem/live.log`, kernel `e79886d9`, takeover): 8-454 frames
per 10 s in `display: 10 s:` (1-45 fps), the helper's answer 7-18 ms mean (max 25-52 ms), backboardd's fences 5-19 ms
for 2-6 ms of GPU time at the top, backboardd 94-120% of a vCPU, every vCPU 0% idle and 35-44% waiting for a host CPU,
the host at 691-737% of 800 with a 95 C SoC. The 10 s means mix still and moving periods, and nothing said how a frame
splits between the render server's CPU, the GPU and the way to the panel.

**The `frame:` line (lasting, xnuhost's log, `src/frame_path.rs`).** Every 10 s while the guest draws, p50/p95/max in
milliseconds of each stage, from events xnuhost already sees: the render server's fenced submissions and their
retirement (`gpu3d_kgsl.rs`), the guest's flush of its scanout (`virtio_gpu.rs`), the helper's answer (`display.rs`)
and the touch frames sent to the guest (`input.rs`):

```text
frame: 10 s: 137 guest frames (13.7/s), 120 of them under 0.5 s apart; ms p50/p95/max: interval 8.4/33.0/120.0;
  flush -> the next frame's first GPU submission (the render server's CPU and its waits) 5.0/20.0/60.0; first
  submission -> flush 3.0/9.0/20.0, of it GPU (submission -> retired, 119 fences) 2.0/6.0/18.0 and retired -> the
  guest's flush 1.0/4.0/10.0; the helper's answer 6.0/12.0/40.0; touch -> next flush 20.0/60.0/90.0 (45 touches);
  3 flushes with no GPU work before them
```

(the numbers are an example, not a run; one line in the log.) A frame is the span between two flushes. "flush -> the
next frame's first GPU submission" is what the render server did before its GPU work: CoreAnimation's commit, the
layout and encoding, and its waits (for the vsync tick, a Mach reply from an app, a CPU). "first submission -> flush" is
its GPU work and the guest's way back to the swap (the fence's interrupt, the display driver's wake,
`DroidVMVirtioGpuDisplay.cpp`); "retired -> the guest's flush" is that way back alone. "the helper's answer" is how
long the guest's flush waited for the display helper. Only intervals under 0.5 s count (a frame after a pause is no
frame rate). How to read it in an app-open: the stage whose p50 is near the interval's p50 is where the frame goes.
If "flush -> first submission" is most of it, the render server's CPU is (the kernel cost of its faults and calls, the
unmap-storm work; `oneios-top: backboardd`); if "GPU" is, the GPU level and the queue (2.2b); if "retired -> flush",
the guest's interrupt and scheduling; if the helper's answer, the copy. Host tests: `frame_path::tests::stages` (a
stream of frames with known stages gives those stages back, a pause is no interval, a flush with no GPU work is
counted), `period` (the 10 s period and what spans two). Cost: a lock and a few stores per event, nothing when idle.

**Fixed: the helper's answer waited for the post before it.** The helper ran one loop: read a frame into its snapshot,
answer, lock a buffer of the layer (to the panel's vsync when the queue is full), fill, post, and only then look at the
socket again. A frame that came while the loop waited for the buffer or filled it was read late: the helper's line of
2026-11-23 19:06 (`out/session-logs/2026-11-23-mac-s1904/return.log`) has the read starting 5.5 ms after xnuhost sent
the frame, the read 6.8 ms, the buffer wait 3.6 ms; xnuhost's answer 7-18 ms mean is about that sum. The guest's
display driver posts its next frame only after that answer (it lands the frame in flight first), so the late start was
on the path of every frame. Now the read and the post are two threads (`runtime/display/src/xhd/run.rs`; README,
"Frames"): the reader answers as soon as the frame is in the snapshot, the poster posts the newest picture. What it
removes from the answer: the late start (5.5 ms mean in that line). What stays: the read itself (6.8 ms there), a CPU
copy of up to 18 MB, which only a copy on the GPU or a scanout of the guest's surface removes (2.5 item 2). The run
loop is portable now and host-tested (`test_run_answers_before_the_post`, `test_run_ends`, `test_run_serial`).
Switch: `display-post=serial` (docs/switches.md). Proof on the phone: `display: 10 s: ... (its answer: mean X ms` down
from 7-18 ms toward the read's time, the helper's `oneios-display: 10 s: ... from Y ms after xnuhost sent the frame`
under 1 ms, and its new `posted mean Z ms ... after the read's answer` (the buffer wait and fill, now off the guest's
path); the `frame:` line's "the helper's answer". Not run on the phone.

**Scanout row reads (2026-11-24; not measured on the S24 Ultra).** `Snapshot::update` now calls the guest-memory
walker once for each full scanline, then hashes up to 24 smaller tiles from that cached row (60 pixels each at the
S24 Ultra's 1440-pixel width; four tiles remain for small frames). The previous path revisited the row's segment list
for each of its four tiles. This lowers repeated mapping-table traversal and makes localized buffer updates narrower;
it still scans every guest row and does not remove the CPU copy. The next phone measurement is the existing
`oneios-display: 10 s` read timing alongside xnuhost's `frame:` stages; no new timing-only instrumentation was added.

### 2.5b The animation rate, ASTC and the pools' room (fix/s24-fps-heat-camera, 2026-11-25)

From the gpumem session of 2026-11-24 (`docs/s24-fps-heat-camera.md`: free memory 1-36 MiB, 2-3 GiB of it GPU blobs,
the pageout and the unmap storm on every core) and UniXNU-zizhan's measured changes. Each is one switch
(docs/switches.md); none has run on a phone.

- **The animation rate.** The in-process animation manager (AnimationKit's in iOS 27, `oneios-tool sym cstr`) asks its
  CADisplayLink for 20..120 a second, 30 preferred, and the guest's display offered 24..120 Hz: light animations ran
  at 24-40 Hz on a panel that refreshes at 120 whatever is drawn. Two fixes: `userland/metal/DisplayLinks.inc` (in
  every process that loads the Metal plug-in but backboardd) gives that manager's links the display's own rate, ported
  from zizhan 42629e5 (their moving fps 56.7-58.4 -> 95.3), switch `oneios-dl-full=0`; and the kernel's display
  reports a 60..120 Hz range (`sheng-vsync-min`, default 60, `kernel/patches/oneios-hosted/display/README.md`).
- **ASTC.** xnuhost's `astc=1` (default) keeps APPLE2 and MTL2,2 in `/product graphics-featureset-fallbacks`, so
  MobileGestalt answers DeviceSupportsASTC and CoreUI and Core Animation give ASTC images to the GPU instead of
  decoding them with ImageIO on the CPU (zizhan 46.9 -> 88.6 moving fps). NativeMetal's frontend lays the blocks out
  itself (`DVMLinearTextureLayout`, a watermark refuses another GPU's layout) and the Adreno samples ASTC LDR. Both
  were dropped because SplashBoard then wrote app snapshots as ATX (blank app-switcher cards, and seconds of CPU per
  snapshot); `userland/metal/SnapshotFormat.inc` (from ios-garnet) keeps SpringBoard's snapshots JPEG or PNG. Switches
  `astc=0`, `oneios-snapshot-astc=1`. The A56 keeps `astc=0` (its host table entry).
- **The pools' room.** metal2vulkan kept a free range of half a block (32 MiB) in each pool and made a new 64 MiB block
  whenever no block had that range in one piece: backboardd's GM row `p 13/440M u 264M`, 176 MiB of wired guest RAM
  unplaced. `userland/metal/KeepRoom.inc` sets `METAL2VULKAN_KEEP_ROOM_MIB` (Droid-VM patch 0027) to 8 in every
  client; a placement past it makes its block when it is placed, as before. Switch `oneios-keep-room=MIB` (0: the half
  block). 624bab3 add-on only. Proof: the GM rows' `p` nearer `u`, `oneios-mem: free` higher, fewer
  `VM_pageout_scan` and `VM_compressor` us in `oneios-perf: kernel_task`.
- **The compile deadline.** metal2vulkan's `Budget::standard` (20 s) failed cameracaptured's compute pipelines
  (`Compile(Budget(Time))`) with every core busy. `userland/metal/m2v-compile-deadline.patch` reads
  `METAL2VULKAN_COMPILE_DEADLINE_S` (1-600), which `CompileDeadline.inc` sets to 120 in every client but backboardd
  (a frame of the render server waits for its compiles; another client waits at its pipeline's creation, off the frame
  path). Switch `oneios-compile-s=0`. 624bab3 add-on only.
- **The display helper's buffers write-only** (the second round, from `docs/ports-2026-11-25-gpu.md` G1). The window's
  buffers are CPU-cached, and each `ANativeWindow_lock` and post cleaned and invalidated the CPU's caches over the whole
  18 MB buffer (YumikoHayamato/OneiOS cdb297bbf measured 85% of the helper's time in that maintenance on the S25, the
  copy 10%). xnuhost's `display-present=ahb` (default) has the helper post three `CPU_WRITE_RARELY` AHardwareBuffers on
  a child of its layer with ASurfaceTransaction, falling back to the window by itself (`runtime/display/README.md`
  "Write-only buffers"). Switch `display-present=window`. Step 1 of the zero-copy panel pool.
- **Turnip's kernel submissions cut at command buffers** (Droid-VM/UniXNU fb9fb47, `userland/metal/turnip-submit-split.patch`).
  A list of more than 128 buffers was cut every 128, sometimes inside a command buffer; the later chunk then ran on BV
  as well as BR with no thread control of its own and the GPU hung (`gmu: GPU hang detected`, the guest stalled 1-2 s;
  Droid-VM: 5-10% of backboardd's starts, 0 in 80 after). Now a submission ends before a buffer whose first packet is
  a CP_THREAD_CONTROL. No switch (a hang fix). Both add-ons (the same Turnip). Proof: no GPU hang or `gpu:` fault line
  at a render server start; the one-time line `darwin: N command buffers' streams with no beginning among them` should
  not appear.
- **cameracaptured's crash in the reflection** (Droid-VM/NativeMetal 3c46b5e, a frontend edit in `frontend_camera_edits`):
  a compute pipeline rebuilt with its unset function constants zero kept the rebuilt function only inside the retry's
  block; ARC freed it there, and `nmb_compute_pipeline_reflection` read a released function (SIGSEGV at 0 during
  cameracaptured's CCPortrait precompilation on Droid-VM, every boot). The function now lives to the end of the call.
  No switch (a crash fix). Proof: no cameracaptured crash report naming `nmb_compute_pipeline_reflection`.

Read and found right already: our guest display waits for the fence taken at the swap (`DroidVMVirtioGpuDisplay.cpp`
reads `droidvm_virtgpu_submitted()` at the offer), the fix zizhan made in 49dee66. Not done: the CPU copy of the frame
itself (2.5 item 2; the write-only buffers take its cache maintenance away, not the copy); the plan of a zero-copy
panel pool is in `docs/s24-fps-heat-camera.md` "The zero-copy panel pool".

## 3. Tests on the PC

`cd runtime && cargo test -p xnuhost -p drm2kgsl-sys` (also part of `runtime/host-tests.sh` and `runtime/build.sh
--test`). These are the C tests of `runtime/xnuhost/tests/` ported case for case on 2026-11-23 (commit `c61e7a5`: 34
tests pass with drm2kgsl and in the 2D-only build); the numbers of checks below are the C tests' unless said:

- `three_d` in `src/virtio_gpu/tests.rs` (`test_virtio_gpu3d` of `tests/test_xnuhost.c` in the C), the device against a
  recording renderer: capsets, contexts, the three kinds of blob with good and bad segments, `MAP_BLOB`'s pool answer,
  `SUBMIT_3D` bytes reaching the renderer whole, fences held and retired in order per ring, a context destroyed with a
  command held, a blob as the scanout, the 2D commands beside them (`two_d`), a device reset, the two lanes. Then a
  generated stream: 4000 commands of every type and of none, with random lengths, ids, flags and segments; each gets
  exactly one answer or error (the C ran it under ASan and UBSan). `XNUHOST_TEST_SEED=N` changes the stream (144 seeds
  were run with the C).
- `blob_entries_of_the_guest_driver`, same file: `CREATE_BLOB`s of 20480, 18316 and 65532 entries (the guest's
  `MaxBlobEntries`) are made, 65533 is refused with its line (section 2.4b).
- `gpu3d::udmabuf` against the real `/dev/udmabuf` of the PC: three segments of a sealed memfd read back in order
  through the dma-buf, a write seen in the memfd, an unsealed memfd refused.
- The split, same file, real udmabufs read back page by page: with the limits set to 8 pages and 3 runs
  (`gpu3d::udmabuf_set_limits`), 20 pages in one run become 8 + 8 + 4, seven separate pages 3 + 3 + 1, two joined
  segments and a third are cut where the first udmabuf is full, exactly the limit is one udmabuf and one page more
  is not; with the limits as on the phone, 72 MiB in one run become 64 + 8 MiB (the second begins where the
  first ends), 1100 separate pages 1024 + 76, and 20480 separate pages (an 80 MiB blob, section 2.4b) 20 udmabufs of
  1024 pages. The PC's own limit is 2147483647 MiB, and setting it to 64 needs root,
  so the kernel *refusing* a 65 MiB udmabuf was not seen here; the phone's `gpuctl nodes` printed 64. The ENOMEM
  halving against a stand-in `UDMABUF_CREATE_LIST` (section 2). Without `/dev/udmabuf` these say so and are skipped.
- `src/gpu3d_kgsl/tests.rs` (`tests/test_drm2kgsl.c` in the C; built when `external/virglrenderer` is there, else each
  scenario prints `skipped (no drm2kgsl in this build)`): the renderer with the patched drm2kgsl on the PC against a
  stand-in for KGSL's memory ioctls (`src/gpu3d_kgsl/tests/kgsl.rs`, reached through `drm2kgsl-sys`'s open/ioctl hooks,
  `glue/oneios_sys.c`; the C wrapped them at link time), the udmabufs real; each scenario runs in a child process of the
  test binary, as the C's did. The stand-in gives GPU addresses as Samsung's driver does (section 4.1): one range of
  256 GiB, a VBO as large as the range allowed, every object an address of its own. `whole_range`: with the room
  check off the VBO is 256 GiB and the first import fails with drm2kgsl's `GPUOBJ_IMPORT failed` ENOMEM, the phone's
  result; `with_room`: the VBO is 192 GiB, a 16 KiB buffer is one import and one range, 72 MiB two imports bound in a
  row by one call (64 + 8 MiB at the right offsets), 1100 pages two (1024 + 76), a blob shorter than its segments
  is bound only to its size, one longer is refused with nothing left imported, a second context imports both chunks
  and binds them at its own address on `GEM_SET_IOVA`, and at the end every chunk is unbound and freed (only the
  VBO is left). `a830` is the base S25's Adreno 830 (chip 0x44050000, gpu_id 6850, 12 MiB gmem, 192 GiB of
  VA): the 256 GiB rung then leaves no import room and the renderer takes the 128 GiB one, and a GEM_SUBMIT on a queue
  shows the default is the WAITTIMESTAMP_CTXTID path (no TIMESTAMP_EVENT) while `NCTX_NO_WAITTS` asks for
  TIMESTAMP_EVENT(FENCE) (patch 0007's A/B switch). Also `coherence` (patch 0007's read-back, section 2.4), `classes`
  and `shown_surfaces` (section 2.2b), and `zero_ids` (patch 0009, section 2.4a: every device entry point and msm
  request with resource, context, blob or queue id 0 is refused, and the context still makes a buffer and submits), and
  `refused_blob_then_context_end` (patch 0010, section 2.4b: a BO whose blob the device refused, then its context's
  end, and new contexts' first submits). The C had 36 checks in the memory scenarios (3 + 23 + 10). It models KGSL from
  its source; it is not KGSL. `layout_matches_msm_kgsl_h` checks the stand-in's mirror of `msm_kgsl.h` against the header
  (`glue/oneios_kgsl_layout.c`).
- `leaks` in the same file (`tests/test_gpu_leaks.c` in the C, which ran under LeakSanitizer): the same renderer and
  stand-in through 30 cycles each of 72 MiB, 16 KiB and 1100-page context blobs (create, upload, attach to a second
  context, its own GPU address there, detach, unref), display blobs in two contexts, contexts destroyed with their own,
  others' and display resources attached, blobs whose `get_blob` fails, and a reset; open fds and live KGSL objects
  must come back each time, and in place of LeakSanitizer a second round of the cycles must leave the heap where the
  first did (2026-11-23: +128 bytes). The C test found two leaks (fixed by
  `drm2kgsl/patches/0003-free-objects-of-a-failed-get-blob.patch`): a `get_blob` that failed after taking its object
  out of the blob table left the object, its KGSL objects and its udmabufs reachable from nowhere (+2 fds and 2 KGSL
  objects for a chunked blob, +1 and 1 single; a guest reaches it with a GEM_NEW at a GPU address outside its range or a
  size unlike the blob's), and a context destroyed with a dma-buf still parked for a `get_blob` kept that fd (956 checks
  in the C). `CYCLES=N` for more. Not covered off the phone: KGSL itself, submits and fences, the guest's driver and Metal.
- `src/gpu3d_kgsl/policy/tests.rs` (`tests/test_gpu_policy.c` in the C): the policy of section 2.2b alone, no KGSL and
  no udmabuf, so it runs on every host; it prints the before/after table of section 2.2b. `the_renderers_header_agrees`
  checks `drm2kgsl/oneios_gpu_policy.h`'s two functions against the Rust policy.
- The loader's `drm-host-pool` property (`runtime/loader/tests/loader.rs`).
- `runtime/xnuhost/check-arm64.sh check`: the static Android xnuhost with the 3D code linked still boots the test kernel
  under `qemu-aarch64` (2D: no KGSL there).

Not tested on the PC: anything the GPU does (submits, fences), and KGSL itself. That is `gpuctl`.

## 4. `gpuctl`: the check on the phone

Built by `runtime/build.sh STAGE` as `STAGE/bin/gpuctl` (static; the bin `gpuctl` of the crate `xnuhost`,
`runtime/xnuhost/tools/gpuctl.rs`, the port of `tools/gpuctl.c` with the same steps, lines and verdicts; the Rust one
has not run on the phone yet). As root on the S24 Ultra:

```sh
adb push STAGE/bin/gpuctl /data/local/tmp/gpuctl
adb shell su -c 'chmod 755 /data/local/tmp/gpuctl; /data/local/tmp/gpuctl'        # nodes, routes, probe, submit, large
adb shell su -c '/data/local/tmp/gpuctl routes'                                   # one step alone: nodes|routes|probe|submit|large
adb shell su -c '/data/local/tmp/gpuctl submit -v'                                # with drm2kgsl's own log lines
```

The fork's Hardware checks screen runs it with no argument. On the Galaxy A56 (no KGSL) `gpuctl amdgpu [NODE] [-v]`
runs the amdgpu renderer's self-test instead (not part of the run of all): the steps and lines of
[a56-gpu.md](a56-gpu.md) section 2, last line `ok` or `FAILED at: ...`. The steps:

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
- `oneios-tool kernel gen gpu`: a board archive of the hosted build only (`kernel/build-hosted.sh` adds it to the board's
  archives of the build's device repository and `liboneiosgpu.a` to the force-loaded archives; was `gpu/build-oneios-gpu.py`). It compiles `gpu/OneiOSGpu.cpp`
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
| `DroidVMGpu` | `external/UniXNU-droidvm/package/DroidVMGpu.cpp`, from a staged copy with four changes (`staged/gpu.rs` of `oneios-tool kernel gen gpu`) | NativeAdreno's hardware classes never attached; the publish; the accelerator's registration |
| `NativeAdrenoAccelerator.late` | `external/NativeAdreno/NativeAdrenoAccelerator.cpp` with `external/NativeMetal/OriginalAcceleratorABI.h`, staged since a56-gpu (2026-11-24, `staged/gpu.rs` `ACCELERATOR`: the A56 accelerator's open types and CPU-composition filter, hooks that answer false for the Adreno one) | `AGXAccelerator`, a subclass of the collection's `IOAcceleratorES`; its static initializer is out of `__mod_init_func` and runs after IOGPUFamily has loaded |
| `IOAcceleratorES-bindings`, `IOAcceleratorES-originals` | generated from `gpu/IOAcceleratorES-24A446-v54.json` | the parent's constructor and base destructor as stubs `bti c; b <name>.original`, its metaclass as an absolute symbol |
| `OneiOSAdreno` | `gpu/OneiOSAdreno.cpp`, `gpu/OneiOSGpuChoice.h` | the start (since a56-gpu, 2026-11-24: which accelerator, from the DRM capset's context type and `oneios-gpu-amd`: [a56-gpu.md](a56-gpu.md) 5), the bound-parent check, the superclass slide, the self-test, weak stand-ins for sheng's display hand-off and log export |

**The binding.** Apple's `AGXG18P` is not in the collection, so the names `AGXAccelerator` and `AGXDeviceUserClient`
are free. The parent is `IOAcceleratorES` of `com.apple.iokit.IOGPUFamily` (a root of `kernel.kc`), read from the
iPhone18,2 kernelcache by `oneios-tool prelink classes`: metaclass `0xfffffe000b976c78`, 136 bytes, 168 slots of which
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
| `oneios-gpu-metal=0` / `=1` | never / always publish (the Adreno accelerator). Not given: published when the host's virtio GPU offers 3D (`oneios_gpu3d_state` 3), so not on the PC. Since a56-gpu (2026-11-24) the start reads the DRM capset's context type: one that is read and is not MSM (the A56's AMDGPU) never gets this accelerator, whatever this argument says (`oneios-adreno: no Metal accelerator: the host's DRM context type is 2 (AMDGPU), not MSM; ...`), nor does a boot with `oneios-gpu-amd=1`; on an MSM capset the rule is unchanged. Droid-VM's `droidvm-gpu-metal` is read when this one is absent. iOS 16.5 (20F66) sessions pass `=0` (docs/a56.md): that build's stock Metal opens the accelerator as an IOGPU device client (type 1), which NativeAdreno refuses (`0xe00002c7`), and backboardd had a Metal device that could not draw. Without the accelerator the panel is still black for another reason: iOS 27's IOMobileGraphicsFamily refuses 20F66's swap record (kernel 0072) |
| `droidvm-gpu-clients=a,b,...` or `*` | the processes besides `backboardd` that get a Metal device: the grant of the user client (DroidVMGpu.cpp `grantedProcesses`) and, since `0039`, the plug-in's `NativeAdrenoMetal.clients`, which the kernel writes beside the bundle at every boot from the same list (`oneios_gpu_client_list` in `gpu/OneiOSAdreno.cpp`). Forms: a name, `X*`, `*X*`, `@apps`, `*`. Not given: `*`, every process (since 2026-11-12, the owner's decision; before: `WidgetRenderer_Default,*Poster*,iconservicesagent,SBRendererService`, and SpringBoard, the apps, the keyboard and WebKit drew on the CPU). The default is in the kernel, not in xnuhost's boot arguments: the image's `overlay/userland/xnuhost.conf` carries its own `boot-args`, which replace xnuhost's default, so only a kernel default reaches every image without a new image or seed; empty: `backboardd` alone |
| `oneios-gpu-plugin-clients=...` | a test: the plug-in's list alone (the processes it names ask for a device; the grant decides for the names it holds, the sandbox for the rest). Not given: the list above |
| `vgpu-3d=1` | what `GETPARAM(DEFAULT_3D)` answers the plug-in (DroidVMVirtioGpu3D.cpp reads it with default 0; `vgpu-3d=0` also turns the driver off) |
| `oneios-gpu-read-wait=1` | a test switch (widgets, 2026-11-13; off by default): the render server's command buffers wait, as they name the IOSurfaces they sample, for other processes' GPU work begun before them that writes those surfaces (the driver's own `awaitWriters`, at most 100 ms: what a newer plug-in asks for and this pin does not). One line at the render server's first surface ticket: `oneios-gpu-read-wait: on ...` or `off`; its waits are `oneios-gpu-sync`'s "reads that waited for writers" |
| `oneios-adreno-selftest=1` | after the publish: IOKit's match of `IOAcceleratorES`, an in-kernel open with type `0x4149`, `GETPARAM(ABI_VERSION)` through the tunnel; two `oneios-adreno: selftest:` lines |
| `oneios-gpu-amd=1` | (a56-gpu, 2026-11-24; xnuhost adds it when, and only when, it picks its amdgpu renderer) with 3D and an AMDGPU DRM capset: the Galaxy A56's accelerator instead of the Adreno one: the same `AGXAccelerator` with `MetalPluginName` `OneiOSRadeonMetal`, `MetalPluginClassName` `OneiOSRadeonMetalDevice`, `OneiOSDrmContext` `amdgpu`, serving open type `0x4149` (and the plug-in's markers) alone; published whatever `oneios-gpu-metal` says (20F66's `=0` is against the Adreno accelerator only). With anything else (no 3D, an MSM capset): nothing, never the Adreno accelerator. Not given or `=0`: the rule above. [a56-gpu.md](a56-gpu.md) 5, `gpu/OneiOSGpuChoice.h` |
| `oneios-gpu-amd-cpu=a,b,...` | the processes that keep CPU composition on the A56's accelerator: its five discovery keys (`MetalPluginName`, `MetalPluginClassName`, `IOGLESBundleName`, `IOGLESMetalBundleName`, `IOGLESDefaultUseMetal`) read as absent there, as Jesvi-iOS's kernel does for its "incompatible core renderers". Not given: `backboardd,SpringBoard,mediaserverd,PosterBoard,CollectionsPoster,WeatherPoster,SBRendererService,MobileSlideShow`; empty: none (the test of backboardd on that Metal); `X*` a prefix, `*` every process. One `oneios-gpu-amd: NAME[PID] kept off the Metal plug-in: KEY hidden (oneios-gpu-amd-cpu); ...` line for each of the first 32 processes. With `oneios-gpu-amd-gl=1` they see `IOGLESBundleName` (only the four Metal keys hidden). No effect on the Adreno accelerator |
| `oneios-gpu-amd-gl=1` | (the A56 GPU port's GL stage, 2026-11-25; [a56-gpu.md](a56-gpu.md) 4a) on the A56's accelerator: `IOGLESBundleName` `OneiOSRadeonGL` (the A56's OpenGL ES, `userland/metal-amd/gl`) for every process instead of `AppleMetalGLRenderer`, and the `oneios-gpu-amd-cpu` processes see it, so backboardd's CoreAnimation composites on OpenGL ES (no Metal device: its GL renderer). xnuhost adds it with `oneios-gpu-amd=1` when the gpuaddon carries `OneiOSRadeonGL.bundle`; `=0` or not given: as before. Rule: `gpu/OneiOSGpuChoice.h` `oneios_gpu_gles_bundle_for`, `oneios_gpu_cpu_hides` (`test_choice.c`). No effect on the Adreno accelerator |

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
  `out/userland/NativeAdrenoMetal.gpuaddon` with its `.cdhashes` list, xnuhost passes it on — or falls back to
  the runtime's `kernel/<build>/gpuaddon`, else `kernel/gpuaddon`, with its `.cdhashes`, when the image set has none
  and every plug-in in it names the set's build in its `oneios-target` (`runtime/xnuhost/src/addon.rs`) — and
  `native_ram_block_gpu_addon` in `gpu/OneiOSAdreno.cpp` finds it). The kernel writes its files into Data first, so a
  rebuilt plug-in needs no new seed. Its CDHashes must still be in the image's `overlay/userland/owner-trust.json` (or
  folded in from `<runtime>/kernel/gpuaddon.cdhashes` on the runtime fallback).
- The grant was not the gate for `backboardd`: `grantedProcesses` always names it. The other Metal clients are granted by default too (table below). Naming them was not enough: with both class chains adopted the sandbox decides the open, and the iPhone's profiles refused `iconservicesagent` every open (`0xe00002e2`, the home screen without icons). `0033-gpu-grant-decides.patch`: the grant decides for the processes it names; the rest are the sandbox's, as before.

Log lines (always on):

```
oneios-gpu-addon: boot addon: -1 files (-1 none), 0 written, 0 unchanged, 0 failed, last error 0
oneios-gpu-addon: /private/var/oneios/addons/Extensions over /System/Library/Extensions: stage 5, status 0 (...)
oneios-gpu-plugin: backboardd[53] code signature of NativeAdrenoMetal: error 0
oneios-gpu-plugin: backboardd[53] library validation of NativeAdrenoMetal: error 0
oneios-gpu-open: backboardd[53] AGXAccelerator type 0x4149 -> 0x0, AGXDeviceUserClient
```

Since `0039` (gpu, 2026-11-12; built, not yet run), three more:

```
oneios-gpu-list: /private/var/oneios/addons/Extensions/NativeAdrenoMetal.clients "WidgetRenderer_Default,...": written (0)
oneios-gpu-text: iconservicesagent[95] no-device device: Vulkan(VkResultCode(-3))
oneios-gpu-askers: backboardd 3, SpringBoard 2, iconservicesagent 1, ...
```

`oneios-gpu-list` is the plug-in's list as the kernel wrote it for this boot (`unchanged (-1)` when the file already
held it). `oneios-gpu-text` is one text row a process's plug-in streamed (its reason for having no device, the render
server's phase lines), whole; before 0039 each character was an `oneios-gpu-open` line of type `0x414770cc`.
`oneios-gpu-askers` (with the GPU report, when it changes) names the processes that read the accelerator's
`MetalPluginName`, the first twelve, with counts: who asked for a Metal device. A process that asked and has no line in
`oneios-gpu-clients` got no device (the list did not name it, the open was refused, or the device failed: its
`oneios-gpu-text` line says which).

**The boot addon carries more than the Metal plug-in (hosted 0090, 2026-11-23).** `oneios-tool ios addon OUT --metal
out/userland/NativeAdrenoMetal.bundle --camera out/userland/camera` writes one sidecar with both (`addons/...` and
`camera/MediaCapture/H9ISP.mediacapture`) and `OUT.cdhashes`; `oneios-tool phone addon OUT` stages it as
`image/kernel.kc.gpuaddon` with the phone's `owner-trust.json` plus those CDHashes. 0031 now takes
`/private/var/ngpu/addons` only when the addon has files under `addons/` (before 0090: any addon), so a camera-only addon
leaves the seed's Metal plug-in in place; `oneios-camera-addon: boot addon: N files (-1 none), C of the capture plug-in
(camera/), M of the Metal plug-in (addons/)` says what this boot's addon held. `phone addon --remove` takes it away.

**An image made on the phone gets the Metal plug-in from the runtime (2026-11-24).** Before this, the phone's Make
iOS image (`serve make-image`) placed no plug-in (the app gives none, the runtime carried none), and iOS composited
on the CPU unless a developer staged the boot add-on from the Mac. Now a release's runtime carries the bundle and
the capture plug-in in `lib/guest/` (`scripts/release/release.sh` builds them from HEAD; `runtime/package.sh`
`METAL_DIR`, `CAMERA_DIR`; [releases.md](releases.md)), and `make-image` seeds the staged runtime's into the image
when the bundle's `oneios-target` is the image's, with their CDHashes in `owner-trust.json`
([image-format.md](image-format.md), "The Metal plug-in of an image made on the phone"). The same log lines as
above prove it on such an image: `oneios-gpu-addon: ... stage 5`, `oneios-gpu-plugin: backboardd[N] code signature
of NativeAdrenoMetal: error 0`, `oneios-gpu-open: backboardd[N] AGXAccelerator type 0x4149 -> 0x0,
AGXDeviceUserClient`.

**A runtime update brings its plug-in to an image made before it (issue #86, 2026-11-28).** Before this, an image kept
the plug-in its Data seed was made with, whatever runtime booted it: on an SM-S928B an image made on v0.1.36 (whose
metal2vulkan-rt plug-in refused the blur over `BGR10_XR`, issue #81) and booted by v0.1.38 (whose plug-in has the
fix) still ran v0.1.36's plug-in (`oneios-gpu-addon: boot addon: -1 files (-1 none)`: nothing replaced the seed's;
the same seed and a Data store carried on from the #81 boots), and backboardd aborted three times with `Metal failed
to build render pipeline`. xnuhost had a fallback to the runtime's `kernel/gpuaddon` (v0.1.39), but no release
carried one. Now `scripts/release/release.sh` packs each plug-in it builds also as the runtime's boot addon
(`runtime/package.sh` `METAL_BOOT_ADDON=1`: the default build's `NativeAdrenoMetal.gpuaddon` as `kernel/gpuaddon`,
24B156's as `kernel/24B156/gpuaddon`, each with its `.cdhashes`; about 9.5 MB each), and xnuhost, when the image set
has no `kernel.kc.gpuaddon`, takes `kernel/<the set's build>/gpuaddon`, else `kernel/gpuaddon`, only when every
`addons/*.bundle/oneios-target` in it names the set's build; its CDHashes join this boot's trust cache
(`imageset.rs` `trustcaches_trusting`), so the image's `owner-trust.json` is not rewritten. The kernel writes the
addon's files into the Data store at every boot and mounts them instead of the seed's (0031, 0090). An image of
another build (the A56's 20F66) keeps its own plug-in. Log lines (xnuhost, always on):

```
gpuaddon: /data/runtime/kernel/gpuaddon: N bytes, 2 CDHashes trusted, plug-ins for iPhone18,2 24A446 (the runtime's; the image set has no kernel.kc.gpuaddon): they replace the Data store's
gpuaddon: .../kernel/gpuaddon: its NativeAdrenoMetal plug-in is for "iPhone18,2 24A446", the image set is build 20F66: the runtime's add-on left out (the image's own plug-in stays)
```

and in the guest `oneios-gpu-addon: boot addon: N files ...` (N > 0, written or unchanged) and `oneios-camera-addon: ... M of the Metal plug-in
(addons/)` with M > 0. Host-tested (`addon::tests`, `runtime/package/test.sh`); not booted on a phone yet: the proof on
an image made by v0.1.36-v0.1.38 is the lines above and backboardd with no `sheng-exit ... Metal failed to build render
pipeline`, SpringBoard 9 of 9.

**What the camera needs (fix-camera-preview-stream, 2026-11-23).** cameracaptured's capture graph (CMCapture,
CMImaging) is a Metal client; on the phone (main 44d9c49) two answers of the frontend at b0d6a07 stopped it, and
cameracaptured crashed (SIGSEGV) about 2 s later, again at each restart. Both are fixed as text edits of the staged
frontend (`oneios-tool ios metal-stage-edit`, `frontend_camera_edits` in `crates/oneios-ios/src/metal.rs`; build.sh's
stage step prints one `metal: staged ...` line for each):
- `native-metal: heap refused: resource options: ... UndefinedValue { ty: "MTLResourceOptions", raw: 6291968 }`, then
  `Fig assert: "_heap" ... Failed to allocate metal heap` (FigMetalAllocator.m:878): FigMetalAllocator's heaps set bits
  21 and 22 (0x600000 over `0x200`, tracked), which metal2vulkan's `ResourceOptions::from_raw` refuses (it takes only
  bits 0-9 and the private bit 24). The frontend now takes such bits off for heaps, heap buffers, buffers, no-copy
  buffers, texture descriptors and heap size queries (`nbKnownOptions`), and says each new set once:
  `native-metal: heap options 0x600200: bits 0x600000 are not Metal's known ones; made without them`. [U] what the
  two bits ask Apple's driver for; the heap works without them.
- `native-metal: missing NBLibrary newFunctionWithName:constantValues:pipelineLibrary:error: (answered nil)` and
  `Fig assert: "function" ... function is nil` (FigMetalContext.m:565): CMImaging's `-[FigMetalContext
  computePipelineStateFor:constants:additionalPipelineOptions:reflection:]` (read in the 24A446 cache) makes each compute
  function with that selector when it has a pipeline library. NBLibrary answers it (and its `completionHandler:` form)
  with the function `newFunctionWithName:constantValues:error:` makes; the pipeline library is not used.
- (2026-11-25, fix/s24-fps-heat-camera, ported from Droid-VM/NativeMetal 3c46b5e, MIT) A SIGSEGV at 0 in
  `nmb_compute_pipeline_reflection` (`Function::clone`): `newComputePipelineStateWithDescriptor:options:reflection:error:`
  retries a kernel that reads an unset function constant with those constants zero, but held the rebuilt function in
  the retry's block alone; ARC freed it at the block's end and the reflection read it. Droid-VM's cameracaptured died
  so at its CCPortrait precompilation every boot. The eleventh edit declares the rebuilt function before the block.
  That it is our "cameracaptured restarted" of 2026-11-23 is a guess: ReportCrash's log names the function if so.
Tested: the edits' unit test (`cargo test -p oneios-ios`), and on the build laptop `userland/metal/build.sh` staged all ten
edits (one `metal: staged ...` line each). **Built:** the laptop's Mesa step stopped before the frontend was
compiled (2026-11-23), in `ir3_lexer.c` (`use of undeclared identifier 'Dl_info'`, `disk_cache.h:86`): the laptop's flex 2.6.10 writes
`#define _POSIX_C_SOURCE 1` into the lexer, and Darwin's `dlfcn.h` then hides `Dl_info` and `dladdr` (`_DARWIN_C_SOURCE`
would show them). Not caused by these edits. Fixed since (031edda: Mesa's cross c, cpp and objc args get
`-D_DARWIN_C_SOURCE`, a `metal: staged ...` line): on 2026-11-24 the laptop built the whole bundle from main c62f974
in 7 minutes with its work in `out/metal` (Turnip set up again; CDHashes `acba1d3c...` NativeAdrenoMetal,
`1078358a...` libvulkan_freedreno.dylib), and the capture plug-in after it.
The stage step also stopped at once in a fresh `METAL_WORK` (the Turnip trace patch was applied before the Mesa
clone); `build.sh` applies it after the clone now.

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
oneios-gpu-report: 10 s: submits +612 (fenced +600); frames +598 mean 3100 us max ever 41000 us; <1/<2/<4/<8/<16/<32 ms +0/+12/+400/+150/+30/+6 longer +0; fence timeouts +0; refusals +0 (op 0 0x0)
oneios-gpu-memory: blobs +14 closed +9 (deferred +2, waiting 3); live 210: own 150 (64 MiB), wired 60 (32 MiB); contexts +0 clients 3
oneios-gpu-clients: backboardd 40+20 MiB, iconservicesagent 16+0 MiB
oneios-gpu-sizes: backboardd <1 120, <2 30, <4 12, <8 4, <16 2, <32 6, <64 0, 64+ 3 MiB blobs
```

Three lines because IOLog cuts a line at 256 bytes: the first phone runs lost the memory figures that way. "own" blobs are
pages the driver allocated (Turnip's buffers and textures): memory that exists for that client. "wired" blobs are the caller's
own pages wired for the GPU (`CREATE_BLOB_USER`: an IOSurface or a no-copy buffer imported, often another process's window
surface); those pages belong to the surface and are counted again wherever it is imported. The clients line gives own+wired
MiB per process; the memory line comes whenever the live counts change, the clients line too.

**Who keeps a still screen drawing.** `oneios-cpu: 10 s: backboardd[53] 62%, SpringBoard[30] 35%, ... (all 140%)`, every
report: the eight busiest processes in the interval, as a percentage of one CPU, from each task's recorded user and system time,
over the time measured since the last report (its seconds start the line; before 2026-11-13 the nominal 10 s, which a
late report in a stall inflated: 1,440 % on eight vCPUs). `kernel_task` likely includes its idle threads [inference: `oneios-perf`'s
`kernel_task: ... most:` counts them as kernel_task's, 198 of 206 s on the PC; this line was not seen on the PC, which
has no GPU report]: a high share there is not work until `most:` names a thread that is not `idle`.
Since 2026-11-14 a second line follows it, `oneios-cpu-sys: 10 s: backboardd[52] 180%, ... (kernel time of the same
processes)`: each one's system time (TASK_POWER_INFO `total_system`) over the same span, so a process's share splits into
kernel work (system calls, faults, IOKit calls: the framebuffer swap's CPU copy or half-float conversion is
backboardd's) and its own code (`docs/hardware.md` 7d, "It runs damn hot"). Compiled alone with the kernel's flags,
not yet in a booted kernel.
The phone's run of 2026-11-11 drew 40-58 frames a second for ten minutes with nobody touching the lock screen, from the lock
screen's first frame until the first SpringBoard (pid 30) aborted; after its restart the same screen drew 0 a second and
frames came only with touches. Before iconservicesagent had a GPU device (an earlier boot) the screen idled. Which process
drives those frames is what this line shows: a client that commits (SpringBoard busy beside backboardd) or the render server
alone running an animation of its own.

Reading a leak: "own" or the live count rising across screens that come back to the same state, while "closed" lags "blobs".
The sizes line (one per client, with the clients line) counts live blobs by size: metal2vulkan's pooled blocks are 64 MiB
buffers, so growth in "64+" is the runtime's pools (empty blocks it keeps, trimmed after 2 s idle, `NativeBridgeMetalFrontend.mm`
`watchIdleMemory`) and growth below it is resources (windows, textures). The plug-in's own pool report (`/private/var/ngpu/debug/client-mem`)
writes `m2v-mem.txt` in the process's temporary folder on the guest's Data volume, which nothing reads out of a phone session yet.
(The runtime's pooled blocks start at 4 MiB and double with the pool up to 64 MiB, NativeMetal `bridge/patches/0013`, so a
pool's first blocks are in the 4-32 MiB columns too.)

**Who keeps a blob, and who frees it** (read 2026-11-13, hang). A blob's life, from the top:

| Holder | What it keeps | When it lets go |
|---|---|---|
| Core Animation (backboardd's render server) and the app | its `MTLTexture`s, `MTLBuffer`s, render targets, its own texture caches; the `IOSurface`s of every window it composites | its own policy: on memory pressure (iOS's, inside the guest), when the layer goes |
| NativeMetal (`NativeBridgeMetalFrontend.mm`) | every imported IOSurface while anything in the process still retains it (`gHeldImports`) | the next import, a command buffer's completion, or a memory-pressure rise (`watchImportPressure`) |
| metal2vulkan (`memory.rs` pools) | the blocks resources are placed in (dedicated allocations for large ones) and up to two empty blocks a pool; a dedicated image block whose image went, as a spare for 10 s | an empty block after 2 s (`watchIdleMemory`), every kept block on a pressure rise; never while placements live in it; a spare past its 10 s at the next return or take, and since 2026-11-23 also at the 2 s idle trim (`m2v-idle-memory.patch`, "The idle memory" below) |
| metal2vulkan (`recycle.rs`) | images and buffers it made, parked as their last holder goes for the next of the same making (at most 32 images and 128 MiB, 64 buffers and 64 MiB), each with its placement | past their second at the next park or take; since 2026-11-23 also at the 2 s idle trim (`m2v-idle-memory.patch`) |
| metal2vulkan (`spares.rs`) | spare command pools, each with every command buffer it made and, in Turnip, the last command stream BO of each of their streams (8-64 KiB each; Turnip's reset keeps it); spare descriptor pools (at most 256 of each list) | a pool spare for 20 s, all but the 8 newest (`watchIdleMemory`); all but 2 on a pressure rise; since 2026-11-23 also a pool given back that holds more than 8 command buffers past the most one command buffer used of it in the last 10 s (`userland/metal/m2v-spare-command-pools.patch`, below) |
| Turnip (`tu_knl_darwin.cc`) | a freed BO until its fence is done (the retire queue), then in its BO cache: at most 16 MiB of BOs up to 1 MiB and 96 MiB of larger ones (`TU_DARWIN_BO_CACHE_SMALL_MB`, `TU_DARWIN_BO_CACHE_MB`; 8 and 16 MiB in every process but the render server) | past the budgets, oldest first: the close below; all at a pressure rise; since 2026-11-23 a BO no allocation took for 30 s, every 60 s (`turnip-idle-bo-cache.patch`) |
| the kernel (`DroidVMVirtioGpu3D.cpp`) | a closed blob while the client still has fenced work made before the close ("deferred", "waiting") | at the client's next method or fence wait once that work is done (`reapPending`); every blob at the connection's close |
| xnuhost (`virtio_gpu.rs`, `gpu3d_kgsl.rs`, drm2kgsl) | the resource, its udmabufs and KGSL's import of them | at `RESOURCE_UNREF`: detached from every context (drm2kgsl unbinds and frees the GPU object), the chunks closed |

**Every blob is wired guest RAM, and the holders let go only when they hear of memory pressure (2026-11-23 22:19,
hosted 0097).** The session's `oneios-mem` wired count (3195 of 5302 MiB) followed `oneios-gpu-memory`'s own + wired
bytes line for line (2044 + 629 MiB at the end). That is the GPU memory of every client, not the render server's alone
(backboardd 917+300, WebKit's GPU process 475+267, the widget renderer 179, MapKit 115 MiB, ...): about 2.0-2.7 GiB.
About 0.5 GiB was the rest. Every holder in the table above gives its memory back at a pressure rise. XNU tells
processes of a rise one at a time, 250 ms apart, the least important first, so the GPU clients came last in a round of
40-75 s, and a level that fell back could end the round before them. Hosted 0097 names the wired owners every 60 s
(`oneios-wired`), says who heard of each rise (`oneios-pressure`) and, with `oneios-pressure-big=512` (docs/switches.md),
notifies a process of 512 MiB or more first. A 16 MiB pooled block (`METAL2VULKAN_BLOCK_MIB`) was not tried again: the
plug-in's own note says it hung the render server's GPU context on 3 of 5 app cycles on the iPadOS guest.

So "blobs +N closed +M" with N far above M (the phone, 2026-11-12 23:45: backboardd +182, closed +17 in 10 s) means the
client has not closed them: the holder is in the process (Core Animation, NativeMetal, metal2vulkan or Turnip's cache, the
last bounded at 112 MiB), not the kernel or the host. Two lines per client name which of its memory it is (2026-11-13; the
gpu archive's staged `DroidVMVirtioGpu3D.cpp`, `tools/oneios-tool/crates/oneios-kernel/src/staged/gpu.rs` `GPU3D`, and
`OneiOSAdreno.cpp` `blobs::`), printed with the sizes lines for each client that made or let go of a blob in the interval
or still has closed ones held:

```
oneios-gpu-kinds: backboardd own task 900 (700 MiB), kernel 2 (1), mappable 650; wired IOSurface 80 (140 MiB), malloc 3 (2), other 1 (1); made +182 gone +17
oneios-gpu-ages: backboardd <16K 600, <64K 300, <256K 200, <1M 92; age <10 s 120, <1 min 300, <10 min 500, older 72; held closed 3 (1 MiB, oldest 2 s)
```

- "own task" / "kernel": this driver's pages for the blob (Turnip's BOs), in the client's task or (where that failed) the
  kernel's map; "mappable": those made with the blob flag `USE_MAPPABLE`, what the CPU writes (command streams, shared and
  staging buffers, descriptor sets) against GPU-only images.
- "wired": the caller's own pages (`CREATE_BLOB_USER`) by the VM tag of its mapping at that address: `IOSurface`
  (`VM_MEMORY_IOSURFACE`, an imported surface), `malloc` (a no-copy buffer over malloc'd memory) and the rest.
- The small sizes (under 1 MiB, where the phone's growth was), the ages of every live blob (from its creation), and "held
  closed": blobs closed while the client's work was in flight, with the oldest such wait. Up to 24,576 live blobs are
  followed (`table full` says when more are not counted).
- How to read them: small "own task" blobs, mostly mappable and growing in the older columns, are kept BOs (command streams
  or descriptor pools the runtime keeps, or Turnip's cache past its budget: a bug there); unmappable ones of a few MiB are
  textures Core Animation keeps; "wired IOSurface" growing is surfaces kept imported; "held closed" with an oldest age of
  minutes is GPU work that never finishes (a fence lost). A burst that the next quiet 10 s returns ("gone" catching up with
  "made", the counts falling) is a busy UI, not a leak.

**The command pools held the render server's small blobs (2026-11-23).** Sessions s1824, s1904 and s2023: backboardd
held 2,149-3,391 blobs under 1 MiB (`oneios-gpu-sizes` `<1`), all "own task" and mappable, 1,432 of them under 16 KiB,
1,077 under 64 KiB, most 1-10 minutes old, `held closed 0`. They grew by 200-630 in each 10 s an app opened (`made +632 gone
+13`) and stayed at idle; they fell by 500-660 at once only at a memory pressure rise (`made +4 gone +664`), which is the
trim of the spare command pools to their 2 newest. Turnip's command buffers keep the last BO of each of their 7 streams
across resets (`tu_cs_reset`, reset flags not read: 8 KiB tile-store and sub streams, 16 KiB and up the others), and
metal2vulkan's spare pools keep every command buffer they made (a secondary for each pass of the largest command buffer
they served), so each pool in the rotation kept its largest frame forever; the idle trim never reached the newest 8. Not
measured per BO name (no such line yet): the sizes and the drops at the pressure trims are what tie them to the pools.
The fix (`userland/metal/m2v-spare-command-pools.patch`, metal2vulkan `spares.rs`, applied after the trace patch by
`ios metal-stage-edit`): a pool given back counts the most command buffers one command buffer used of it in a 10 s
window; at the first give-back past the window, a pool holding more than that plus 8 is released (its BOs go to Turnip's
cache, past its budget to the host) and the next taker makes one. A pool that serves the same command buffers each frame
is never released, so a steady screen makes no pools. Host tests: the runtime's `tests::spares` (12, 4 new: the slack,
the window, steady use, the census). Switch: the boot argument `oneios-pool-cut=0` (`boot-args-add=oneios-pool-cut=0` in
the image set's `xnuhost.conf`) keeps the pools as before (the plug-in sets `METAL2VULKAN_POOL_USE_WINDOW_S=0`; the
runtime reads 1-3600 s, default 10). The bytes are another matter: backboardd's 0.8-1.8 GiB were mostly 5-21 blobs of 64
MiB and more (pooled blocks of its textures), which fell only when Core Animation let textures go at a pressure rise;
this fix does not change them.

A lasting line, each Metal client's command pools (`userland/metal/CommandPoolRow.inc`, appended to the plug-in), every
60 s when the command buffers moved by 32 or more or a pool went:

```
oneios-gpu-text: backboardd[64] CP 9/120 cut +2/80 idle +1/12 tuc 11M
```

Live pools / the command buffers made from them; `cut`: pools released since the last row for holding past their use,
/ their command buffers; `idle`: pools the idle trim released, / theirs; `tuc`: Turnip's cache of freed BOs now. With the
fix, backboardd's command buffers should stay in the low hundreds at a still screen and its `<1` blobs should fall back
after an app closes (the `oneios-gpu-kinds` `gone` moving without a pressure rise).

**The idle memory (2026-11-23, fix/m2v-update).** Session mac-int (2026-11-23 22:20,
`out/session-logs/2026-11-23-mac-int/live2.log`): at the end the Metal clients' own GPU blobs were 1.7-2.4 GiB
(`oneios-gpu-memory` own 2,358-2,662), backboardd 917 MiB own + 300 wired, WebKit's GPU process 475 + 267, and the
poster extensions got `no-device device: OutOfMemory(Device)`. backboardd's bytes were mostly 7 blobs of 64 MiB and more
and 11 of 32-64 MiB (`oneios-gpu-sizes`), WebKit's 5 and 2; backboardd's Turnip cache held `tuc 78M`. Three holders
that let go only when something else happened, never at a still screen, read in the code:

- metal2vulkan's recyclers (`recycle.rs`) park an image or a buffer it made as its last holder goes, for the next of the
  same making, up to 32 images and 128 MiB and 64 buffers and 64 MiB, "none parked longer than a second"; but the second
  is checked only as another object is parked or taken. A process that went still kept all of them, each a placement
  that keeps its block (a 64 MiB image a whole dedicated block).
- the spare dedicated image blocks (`memory.rs`, NativeMetal bridge patch 0021): an image's dedicated block kept 10 s
  for the next image of its size; aged only as a placement is returned or a dedicated image is made.
- Turnip's cache of freed BOs (`tu_knl_darwin.cc`): emptied only past its budgets or at a pressure rise.

The fix: `userland/metal/m2v-idle-memory.patch` (metal2vulkan `memory.rs`, `recycle.rs`, `work_counts.rs`, applied
after the pool patch): the plug-in's idle trim (`trim_kept_memory`, every 2 s, `watchIdleMemory`) also frees the parked
images and buffers past their second and the spares past their 10 s. And `userland/metal/turnip-idle-bo-cache.patch`
(Turnip `tu_knl_darwin.cc`, appended to the Mesa list after the trace patch): `tu_darwin_bo_cache_trim_idle(age_ms)`
gives back the cached BOs no allocation took for that long; the plug-in's row thread calls it every 60 s with 30000. A
busy process takes its parked objects and cached BOs back within the spans, so both reach a still one only. Switches:
`boot-args-add=oneios-idle-spares=0` (the plug-in sets `METAL2VULKAN_IDLE_SPARES=0`) and `oneios-bo-cache-idle=0` (no
call), `docs/switches.md`.

Two lasting rows with the command pool row (`userland/metal/CommandPoolRow.inc`), from `m2v_memory_census` (the
runtime's census as of its last idle trim):

```
oneios-gpu-text: backboardd[64] GM p 7/448M u 210M e 2/128M d 11/400M s 3/150M bo -78M
oneios-gpu-text: backboardd[64] RP enc +36000 inst +36100 split +100 barrier +0
```

`GM` (every 60 s when a figure moved by 8 MiB, or the trim gave BOs back): `p` pooled blocks with a placement / MiB, `u`
MiB placed in them, `e` kept empty pooled blocks / MiB, `d` dedicated blocks / MiB, `s` spare image blocks / MiB, `bo`
MiB of Turnip's cache given back since the last row. So `p` far above `u` is room in pooled blocks (the 64 MiB block size
and the room kept ahead, below), `d` is large images, `s` spares. `RP` (every 5 minutes while encoders end): render
encoders ended, their render pass instances, the splits, of them at the program's memory barriers (section 7,
"Mobile").

Tested: metal2vulkan's own runtime tests on the build laptop on the patched stack (624bab3 + the 51 bridge patches +
the trace and pool patches + this one): the lib suite 1148 passed, the same 4 failing as on the stack without it
(`buffer_texture` x2, `exit_guard` x2); the device tests (`--cfg m2v_gpu`, Intel Iris Xe, ANV) 1397 passed, 15
failing, and the stack without it 13 failing in a parallel run: 10 in both (`buffer_texture` x2, `exit_guard` x2,
`completion_here` x2, `recorded_at_the_call`, `render_stage_digests::a_second_device...`, `ycbcr_device`,
`pipeline_wait_order`), the other 5 and 3 timing tests that failed in one run only; those 8 (and 2 neighbours) run one at a time all pass with
the patch (on the stack without it `teardown_threads` fails); the new `tests::idle_spares` makes a 4096x4096
render target (a 64 MiB dedicated block), drops it, and checks that a trim within the second keeps the parked image,
one past it frees it into a spare, one within 10 s keeps the spare and one past it frees it, and the census words.
Turnip's change is built for the phone; no host test reaches the darwin backend. Built on the build laptop
(`userland/metal/build.sh all`, own `METAL_WORK`): `NativeAdrenoMetal` CDHash `4b2f5d98...`, `libvulkan_freedreno.dylib`
`e674f5ee...` (both export the new entry points); with the camera plug-in (`fda818f2...`) the add-on
`combined.gpuaddon` is 12,327,696 bytes, sha256 `fa8e8c3a...`. Not booted. The proof on the
phone: the `GM` rows' `s` and `bo` at a still screen; backboardd's and WebKit's `oneios-gpu-kinds` own-task MiB falling
at a still screen with `gone` moving and no pressure rise (`oneios-mem`), and `tuc` near 0 in the `CP` rows of a still
process; frames no slower (`oneios-gpu-report` means).

The host's side, in xnuhost's log every 10 s while it changes (`virtio_gpu.rs`, the blob note): `gpu: 10 s: host blobs N (M
MiB), made +A unref'd +B`. N against the guest's `oneios-gpu-memory` live count says whether the host keeps blobs the guest
let go (N larger by more than the display's few 2D surfaces: a host leak); equal, the holding is the guest's.

**Where a surface's pixels go** (widgets, 2026-11-13). On the phone (2026-11-12 23:45) every widget was blank or white
while `WidgetRenderer_Default[174]` had a Metal device (`library validation of NativeAdrenoMetal: error 0`,
`libvulkan_freedreno.dylib: error 0`, `AGXAccelerator` opened 4 times, `WidgetRenderer_ 28+3 MiB` in the clients line).
How a widget reaches the screen, read in 24A446 and in the plug-in (not seen running): `WidgetRenderer_Default` is a
hidden app (`com.apple.chrono.WidgetRenderer-Default`, `WidgetRenderer.framework`) that hosts each widget's archived
SwiftUI view in a scene SpringBoard shows; SwiftUI draws through RenderBox (`RenderBox hosting configured - async? %d,
opaque: %d` in its log, or `RenderBox hosting disabled`), which renders with Metal in the widget renderer's own process
into IOSurfaces (RenderBox's CAMetalLayer path; NativeMetal presents a drawable when its command buffer completes) or
lets Core Animation draw; the render server (backboardd) then composites those IOSurfaces with its own Metal device. The
plug-in makes a texture over an IOSurface by importing the surface's mapping in the process
(`newTextureWithDescriptor:iosurface:plane:` -> a no-copy buffer -> `CREATE_BLOB_USER`, the caller's pages wired), so
each process imports the same physical pages as a blob of its own (a resource of its own on the host, its own udmabufs
over the same RAM memfd range, its own KGSL import): nothing is copied anywhere on this path, and nothing on it zeroes
or clears a surface. The steps where the image can be lost, and the line that shows each (the staged driver's hooks,
`staged/gpu.rs` `GPU3D`, `OneiOSAdreno.cpp` `trail::`, printed with the GPU report while they change):

| Step | Line | What says the image went missing there |
|---|---|---|
| The widget renderer draws with Metal at all | `oneios-gpu-surfaces: WidgetRenderer_ submits +N; writes +W ...` | `submits` 0 (no GPU work); submits but `writes +0` (it renders into no IOSurface: private textures only, or Core Animation draws it) |
| Its GPU work into a surface finishes | the same line: `done +D (closed +C)` | `done` far below `writes`, or only `closed` (the command buffers never complete) |
| The pages of the surface get the pixels | `oneios-gpu-pixels: WidgetRenderer_ S surfaces (M MiB): zero Z, white W, flat F, drawn D; changed X` | its surfaces `zero` (the GPU wrote elsewhere, or not into a format the pages show: a texture the runtime keeps apart and never copies back), `white` (what it drew is white) |
| The render server imports the same pages | `oneios-gpu-pixels: backboardd ...; pages of another process's surfaces N (WidgetRenderer_)` | `N` 0 while the widget renderer has surfaces: the render server never textures them, or gets other pages |
| The render server reads the surface after the write | `oneios-gpu-surfaces: WidgetRenderer_ ... on S surfaces: R since read by another (backboardd), U by none, T read while written` | `R` 0 (no GPU read of them), `T` high (it samples them while the widget's GPU work is still writing: on KGSL the render server's queue runs first, and this plug-in pin never asks the kernel's `SurfaceAwaitWriters` wait) |
| CPU access to GPU-drawn surfaces | `oneios-gpu-locks: 10 s: NAME n (GPU-written g, read-only r)` and `oneios-gpu-sync: ... CPU locks +L ... waited +A, gave up +B` | a snapshotter reading them (`gave up`: the lock waited 500 ms for GPU work that did not end) |
| The host keeps the GPU coherent with the CPUs | `gpu: KGSL imports ... coherence dropped by KGSL D` (xnuhost, section 2.4) | `D` above 0 |

The lines (examples, not from a run):

```
oneios-gpu-sync: 10 s: tickets +40 ended +40 (closed +0, not kept +0); CPU locks +12 (read-only +10, no wait +0): waited +2, gave up +0, mean 900 us, max ever 4000 us; reads that waited for writers +0, gave up +0, max ever 0 us
oneios-gpu-surfaces: WidgetRenderer_ submits +40; writes +24 done +24 (closed +0) on 6 surfaces: 5 since read by another (backboardd), 1 by none, 0 read while written; reads +2: others' +0 (while written +0), with no GPU writer +2
oneios-gpu-pixels: WidgetRenderer_ 6 surfaces (5 MiB): zero 0, white 1, flat 0, drawn 5; changed 3; pages of another process's surfaces 0 (-)
oneios-gpu-pixels: backboardd 40 surfaces (90 MiB): zero 2, white 4, flat 3, drawn 31; changed 9; pages of another process's surfaces 33 (SpringBoard)
oneios-gpu-locks: 10 s: SpringBoard 12 (GPU-written 3, read-only 10); other processes 0; surfaces forgotten for room 0
```

- `oneios-gpu-sync` is the tunnel's own IOSurface counters (`DroidVMVirtioGpu3D.cpp` `surfaceAccess`, `surfaceTrap`,
  `awaitWriters`), which nothing printed before: tickets (one per command buffer that names surfaces) begun and ended,
  ended because the connection closed, uses not kept (its table of 4096 full); CPU locks of IOSurfaces
  (`IOSurfaceLock`) and how many waited for GPU work, gave up after `droidvm_surface_wait_ms` (500), their mean wait;
  the render server's waits for other processes' writers (only a plug-in that asks for them: this pin does not).
- `oneios-gpu-surfaces`, one line per client that submitted or named a surface since the last report, by IOSurface ID
  (from what each command buffer names as it commits: NativeMetal's surfacesync, the surfaces it renders into and those
  it samples): its submissions; its writes begun and ended; the distinct surfaces whose last write ended in the
  interval, and of those how many another client read since (and who), none did, or another read while a write was in
  flight; its own reads of other clients' surfaces (and of surfaces no GPU wrote: a CPU-drawn backing store).
- `oneios-gpu-pixels`, one line per client whose figures changed, by blob: each IOSurface the client imported for the
  GPU (`CREATE_BLOB_USER` over a mapping tagged `VM_MEMORY_IOSURFACE`: the "wired IOSurface" of `oneios-gpu-kinds`),
  read here from its guest pages (`phystokv`, the identity on the RAM window): 8 pages spread over the surface and 16
  pixels in each, all zero, white (every byte 0xf0 or more), one other value, or drawn; "changed" is the import whose
  pixels differ from the last report's; "pages of another process's surfaces" is the import whose first page is the
  first page of an IOSurface another client imported (one physical range seen by both), with the client most of them
  share with. Up to 1536 imports are followed (`table full: some imports not read` when more are not).
- `oneios-gpu-locks`: who locked GPU-used surfaces with the CPU in the interval (the first eight processes).

A client slot is reused when its connection closes and another opens: a line names the slot's current process. What was
checked (Linux PC, 2026-11-13): `kernel gen gpu` in a hosted kernel build compiled `OneiOSAdreno.cpp` with no warning
and the staged driver with the hooks of the trail, and linked `liboneiosgpu.a`; the staged driver with the
`oneios-gpu-read-wait` hook too, compiled alone with the build's own command for an iokit source (it was added after
that build's tool was made); `cargo test -p oneios-kernel --test golden` 10 of 10. **Not done** (the work was paused):
the whole kernel's link, the prelink (`kernel.kc`, audit, stubs check) and a PC boot with these hooks; qemu has no 3D, so
a PC boot would only show that the kernel boots with them, none of these lines. Not yet run on the phone.

**The next phone boot (widgets, 2026-11-13).** The kernel and runtime of this change, the image's `xnuhost.conf`
`boot-args` as the baseline's plus `oneios-iouc=WidgetRenderer_Default,chronod` (0029: every IOSurface user-client call
those two make whose answer changes, so a refused lookup or create shows). Unlock, look at a home screen page with
widgets for a minute (add one from the gallery if none), swipe to it and away twice, and return. From xnuhost's log:
`oneios-gpu-surfaces`, `oneios-gpu-pixels`, `oneios-gpu-locks`, `oneios-gpu-sync` (all of them), `oneios-gpu-clients`,
`oneios-gpu-kinds` and `oneios-gpu-sizes` for `WidgetRenderer_` and `backboardd`, `oneios-gpu-text` and
`oneios-gpu-open`/`oneios-gpu-plugin` for `WidgetRenderer_Default`, `oneios-iouc: WidgetRenderer_Default` and
`chronod`, drm2kgsl's `guest import: KGSL ...` and the host's `gpu: KGSL imports ...`. From iOS's own log
(`oneios-tool phone pull-data data.img`, then):

```
oneios-tool ioslog show data.img --boot last --proc WidgetRenderer_Default --type error,fault
oneios-tool ioslog show data.img --boot last --proc WidgetRenderer_Default --grep 'native-metal|refused|Unsupported|444|_RB4|MTL|Metal|IOSurface|drawable'
oneios-tool ioslog show data.img --boot last --proc WidgetRenderer_Default --grep 'RenderBox|Rendering view|placeholder|Placeholder|Archive was nil|Content load failed|Snapshot|archive'
oneios-tool ioslog show data.img --boot last --proc chronod --grep 'placeholder|timeline|archive|snapshot|extension|reload|fail|error'
oneios-tool ioslog show data.img --boot last --proc backboardd --grep 'native-metal|refused|Unsupported|IOSurface|surface import'
oneios-tool ioslog show data.img --boot last --proc SpringBoard --grep 'widget|Widget|WidgetRenderer|scene|placeholder'
oneios-tool ioslog show data.img --boot last --subsystem com.apple.chrono
oneios-tool ioslog checks data.img --boot last
```

What each would mean, as guesses to test: `native-metal: ... Unsupported ... 444` or `_RB4` in the widget renderer is
RenderBox drawing widgets into its own two-plane 4:4:4 IOSurfaces, which NativeMetal does not render into (Droid-VM's
note in `build-adreno-metal-plugin.py`: the iPad's Weather widget showed rows of stray colour until RenderBox was told
`MTLGPUFamilyApple2` by the caller's image path), with `oneios-gpu-pixels: WidgetRenderer_ ... zero` beside it; then
the fix is the plug-in's (built on the Mac, a new trust list and image seed). `placeholder` / `Archive was nil` /
`Content load failed` is the widget's content never reaching the renderer (chronod and the widget extensions: no GPU
cause). Drawn widget surfaces (`drawn`) that backboardd shares the pages of and reads after the write, while the screen
shows white, put the loss in the render server's composition. If `read while written` is well above 0, a second
session with `oneios-gpu-read-wait=1` added says whether the order of the two processes' GPU work is the cause
(widgets drawn with it, blank without). For the keyboard hang (`docs/newtodo.md` 11, 12): the image's
`xnuhost.conf` `boot-args` as the baseline's, with its `oneios-ubt...` part replaced by

```
oneios-ubt=Spotlight,kbd,backboardd oneios-ubt-at=300 oneios-ubt-every=10 oneios-ubt-count=150 oneios-iouc=kbd,Spotlight oneios-sysfail=kbd,Spotlight
```

(`oneios-ubt-count` is 0061's, on `fix-wifi-late-2`: without it the samples go on every 10 s to the end). Then unlock,
open Settings > Wi-Fi, tap a network and type its password for a minute, and return. From xnuhost's log, the lines
from 2 minutes before the keyboard to the return: `oneios-gpu-memory`, `oneios-gpu-clients`, `oneios-gpu-sizes`,
`oneios-gpu-kinds`, `oneios-gpu-ages` (backboardd, Spotlight, kbd, Preferences), `gpu: 10 s: host blobs`, `gpu: 10 s
host:`, `display: ... flushes`, `oneios-cpu:`, `oneios-perf:` (all of its lines, with `kernel_task: ... most:`),
`oneios-mem:` (four lines), both `memory:` lines, `thermal:`, `input: touch:`, `oneios-ubt:` (each report's lines;
name a thread's addresses with `oneios-tool sym ADDR...`, then `--slide`), `oneios-iouc: kbd`/`Spotlight`,
`oneios-sysfail: kbd`/`Spotlight`. From iOS's own log (`oneios-tool phone pull-data data.img`, then, with `--boot last`
and `--since`/`--until` around the keyboard):

```
oneios-tool ioslog show data.img --boot last --proc kbd --type error,fault
oneios-tool ioslog show data.img --boot last --proc kbd --grep 'ANE|Espresso|E5RT|e5rt|[Nn]eural|LanguageModel|[Mm]odel|timeout|retry'
oneios-tool ioslog show data.img --boot last --proc Spotlight --type error,fault
oneios-tool ioslog show data.img --boot last --proc Spotlight --grep 'ANE|Espresso|E5RT|MTL|Metal|GPU|embedding|query|index|timeout|retry'
oneios-tool ioslog show data.img --boot last --proc InputUI --grep '[Kk]eyboard|session|timeout|fail'
oneios-tool ioslog show data.img --boot last --proc SpringBoard --grep 'UIKeyboard|RemoteKeyboard|[Kk]eyboardArbiter|InputUI|focus'
oneios-tool ioslog show data.img --boot last --proc Preferences --grep '[Kk]eyboard|password|join|timeout'
oneios-tool ioslog show data.img --boot last --proc backboardd --grep '[Kk]eyboard|IOSurface|texture|memory|watchdog|hang|stall'
oneios-tool ioslog show data.img --boot last --subsystem com.apple.TextInput
oneios-tool ioslog show data.img --boot last --proc kernel --grep 'memorystatus|jetsam|idle_demotion|stuck'
oneios-tool ioslog checks data.img --boot last
```

The phone's numbers before this split (backboardd 20 -> 383 -> 431 -> 460 -> 543 -> 925 MiB in 16 min, 2026-11-11) mixed both
kinds; the steps at 543 and 925 came after SpringBoard restarts (it aborted on `processIdentity`, not a GPU fault). The
plug-in keeps each imported surface while anything else in backboardd still retains it and lets it go at the next import, a
command buffer's completion or memory pressure (`NativeBridgeMetalFrontend.mm`, `surfaceBuffer`), and trims the runtime's
empty blocks every 2 s; whether a dead SpringBoard's windows are let go is what "wired" after a restart will show.

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

### 5.2 iOS 16.5 without a Metal device: CoreImage's OpenGL ES fallback (fix/a56-ios16-faults, 2026-11-24)

**The fault (the Galaxy A56, OneiOS issue 73, boot of 2026-11-24 21:47, v0.1.24):** after the user turned Bold Text on
(Settings > Accessibility > Display & Text Size; iOS restarts SpringBoard for it), SpringBoard died at every start, 9
times: `trap kind 2 signal 11 far 0x0 esr 0x82000005 access 4; pc 0x0 lr 0x1c261c58c ... x0 0x0 x1 0xb71`, status 0xb.
Read in 20F66's `dyld_shared_cache_arm64` on the build laptop (`out/ios16-cache`; the shared region's slide 0x2db08000,
from the `oneios-srprot` lines' `vm_protect 0x203478000-0x208960000`, which is the `.27.dylddata` mapping
0x1d5970000 + 0x54e8000, slid; `oneios-tool sym --slide 0x2db08000`, `sym find`, `sym callers`, `ipsw dyld a2s`):

- lr = `CI::GLContext::init() + 0xcc`: `blr x8` with `x8` = the GL dispatch table's glDisable and `w1` = 0xb71
  (`GL_DEPTH_TEST`). The table comes from `GetMacroContext` -> `wrapGetMacroContext`, which zeroes 712 bytes and fills
  them only when `-[EAGLContext getMacroContextPrivate]` answers; there was no EAGL context, so the table stayed zero.
- How it got there: `-[CIContext initWithOptions:]` (0x19497ef1c unslid) makes a Metal context when
  `CIMetalCopyDefaultDevice` has a device, and otherwise calls `+[CIContext internalGLContextWithOptions:]`
  (0x1949bd62c), which tail-calls `internalContextWithEAGLContext:nil options:` -> `CI::GLContext(ShareContextInfo,
  ...)` -> `init`. `kCIContextUseSoftwareRenderer` (`software_renderer`) does not avoid it, `CIGLIsUsable` is
  `mov w0, #1` (`wrapGLIsUsable`), and iOS 16's CoreImage has no CPU renderer. The A56 has no Metal device on iOS
  16.5 (`oneios-adreno: no Metal accelerator ...`, gpu=2d) and no OpenGL ES driver of any kind.
- Who asked: with Bold Text, UIKit's styled images, `+[CUIShapeEffectStack sharedCIContext]` -> `contextWithOptions:`
  (the owner's iOSPlus10 port found the same fault on 20H392, its `docs/boldtext-crash.md`, with a whole backtrace:
  the same `CI::GLContext::init() + 204`). Any app that reaches a CIContext without Metal can die the same way.

**The fix:** a text fixup xnuhost adds for an image set of build 20F66 (`config::ios16_fixups`,
`runtime/xnuhost/src/config.rs`): `dyld_shared_cache_arm64.07,a162c,e30302aa88512490,000080d2c0035fd6`, the first two
instructions of `internalGLContextWithOptions:` (`mov x3, x2; adrp x8, ...`) become `mov x0, #0; ret`. The GL
fallback answers no context, `initWithOptions:` returns nil (its own path when the GL context is NULL), and CoreUI's
`_newFlattenedImageFromShapeCGImage:...` sends `render:toBitmap:...` to nil: the styled image is a blank bitmap instead
of a crash [I: read in the code, not seen]. A Metal device is taken before the fallback, so the A56's Metal plug-in
(docs/a56-gpu.md) is not affected when it is there, and no other caller of the fallback exists in the cache (`sym
callers`). `--set ios16-fixup=0` leaves it out (the A/B switch). The runtime alone: no image remake, no kernel.

**What it does not do:** draw. With Bold Text on, glyphs or symbols that UIKit styles through CoreUI may show blank
[I]. The full fix is a GL that works without a GPU or a Metal device: iOSPlus10's `userland/negronigl` (EAGL on
Mesa: freedreno, or softpipe on the CPU, with the GLI dispatch slots CoreImage reads) is that, written for 20H392
(its GLI table offsets are 20H392's, its route for apps untested on a phone there): a port to 20F66 and to the A56
(softpipe, or radeonsi with the Metal work) is a separate job, not done. iOSPlus10's other stop-gap, Bold Text kept
off in the root, is not taken: a fresh 20F66 Data store has the switch off already, and the user's own choice stays.
**The proof on the A56:** `ios16: 1 text fixup(s) added: ...` and `text fixups: 3 for /chosen text-fixups`, the kernel's
`text-fixup: dyld_shared_cache_arm64.07+0xa162c (8 bytes) applied`, and with Bold Text on no `trap ... pc 0x0 lr
0x1c261c58c` (the lr moves with the slide: `CI::GLContext::init + 0xcc`) and no `crashed: SpringBoard`.

**With the A56's OpenGL ES** ([a56-gpu.md](a56-gpu.md) 4a, 2026-11-25) the fixup is left out. That holds when xnuhost
picked the amdgpu renderer, found `OneiOSRadeonGL.bundle` in the same gpuaddon, and the guest's boot arguments say
`oneios-gpu-amd-gl=1` (`config::ios16_gl_driver`). The bundle answers CoreImage's EAGLContext: on radeonsi, or on
softpipe when radeonsi does not open in that process (every process but backboardd). So CoreImage draws Bold Text's
styled images instead of a blank bitmap: the full fix this section names. Its line is `ios16: CoreImage's GL fallback
fixup left out: oneios-gpu-amd-gl=1, OneiOSRadeonGL answers its EAGLContext`. A hand-written `oneios-gpu-amd-gl=1`
without the bundle keeps the fixup (the kernel would name a bundle that is not there). `oneios-gpu-amd-gl=0` in the
conf keeps it too. Test: `ios16_coreimage_fixup_is_left_out_only_with_the_a56_gl_driver`.

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
5. **Zero-copy scanout**, EDID (refresh rate), cursor: not done; the display is one row copy per flush, by the helper
   (direct frames). A GPU blit would need a host Vulkan or KGSL path that imports a udmabuf; not done.
6. **`gpuctl` passing on the phone** with the VBO fix and the split (section 4.1): not run yet. Until it does,
   whether this KGSL binds several imported udmabufs in a row and runs commands from them is unproven.

## 7. The Metal plug-in (`userland/metal`)

**Build notes (2026-11-23, integration-switches; shader fixes enabled 2026-11-24).**
`build.sh turnip` removes `build/mesa-tu-ios27` when the inputs of its cross file changed (the SDK, the compilers,
the base script), since an existing directory only gets `meson configure`. The `9xxx` copies of
`patches/metal2vulkan`: since 2026-11-25 90101, 90102 and the switch 90110 are in the bridge's list (`ios
metal-stage-edit`), **on by default since the merge into main (2026-11-25)**, `oneios-m2v-shader-fix=0` turns them off (docs/switches.md, the
plug-in's `ShaderFix.inc` sets `METAL2VULKAN_SHADER_FIX=1` unless it says 0); 90103 is in none (it does not apply on the bridge's
stack). With the switch, a run-time-indexed constant table past 4 KiB is read from the constant blob (an Adreno backs a
thread's private copy per fiber) and constant GEP and cast pointers into that blob are lowered; the device profile's
fingerprint takes the switch, so the shader cache never serves a translation of the other setting. Ported from
orumanayurpanjayat-ux/OneiOS `d157b43`, which applied both in every build; not booted with the switch on (the first boot
with it on translates each shader again: the profile's fingerprint changed).

**metal2vulkan's upstream (2026-11-23, fix/m2v-update): the pin stays 624bab3.** Upstream `development` at
`6462199fa` (2026-11-23) is 9,217 commits newer. It is a new runtime: `metal2vulkan-rt`, written from 2026-11-17
(`28b6b0b9d`), replaced the runtime this build uses (`5d725aeec`, 2026-11-20, "Delete metal2vulkan-runtime") and took
its name; its `src` has 177 files where the pin's has 562. Tried on the build laptop (`git apply --check` of the base's
recorded list in order): 1 of the 51 bridge patches applies (NativeMetal 0009), and 68 of the 120 files they touch are
gone. The bridge (`bridge/NativeMetalObjects.rs`, `NativeMetalBridge.rs`, 3,735 lines) names 47 runtime types the new
one has not (among them `Heap`, `HeapDescriptor`, `BinaryArchive`, `ArgumentEncoder`, `IoSurfacePlane`, `HostMemory`,
`PurgeableState`, `ShaderCacheOptions`, `DynamicLibrary`, `Function`, the GPU family rows, `MessageSink`): its README's
"What the consumer implements" makes heaps, archives, reflection and labels the consumer's, over its `Block` and
`HeapRange`. Its `Instance` loads a Vulkan loader by name, or the consumer's drivers through a loader with
`VK_LUNARG_direct_driver_loading`; iOS has no loader (the bridge opens Turnip's dylib itself). Its floor (Vulkan 1.3
and the README's list): Turnip's descriptor and compute limits in `tu_device.cc` meet it (read, not run). So the update
is a port, not a rebase. What is left for it: (1) an `Instance` over Turnip's `vkGetInstanceProcAddr` without a loader
(a patch, or Khronos's loader built for iOS); (2) the bridge's heaps, archives, argument encoders, IOSurface imports,
purgeable states and functions written over the new API; (3) each of the 51 patches judged again (most mend code paths
the new runtime does not have); (4) a phone boot of every Metal client (backboardd, SpringBoard, WebKit, the camera,
the posters). Several days of work and boots; started as the rt port (next paragraph), built only with `METAL_RUNTIME=rt`.

**Upstream commits looked at for our faults** (named in the task, then `git log 624bab3..5d725aeec~1` of the old
runtime: 2,981 commits on its sources, 995 with words of memory, blocks, pools, spares, free, idle, release or cache in
their subject; those below are the ones about memory held at rest):

| Commit | What | Taken | Why |
|---|---|---|---|
| `50196ba82`, `d20c812be` | the preprocess pool holds nothing at rest; its 64 MiB bound and quarter-second free | no | new runtime; and the pool is `VK_EXT_device_generated_commands`' preprocess memory, which Turnip does not offer: never reached on Adreno |
| `3674f98a6` | a pooled block that holds no place goes back | no | the validation crate's replay player (a test tool) of the new runtime |
| `4c808ffa6` | every free of device memory on the block-maker thread | no | new runtime; the old one's free thread (`b8ed1b818`, 2026-11-03) is not in the pin and conflicts in 5 files |
| `39b0efc6e` | a release waits for the command buffers committed before it | no | new runtime; in the pin a committed command buffer holds every resource it uses until it completes (`texture.rs` `Drop for Image`), and freed memory waits for the batches in flight (`freed_memory.rs`) |
| `91fed9449`, `015ece492` | the pipeline cache bound (128 MiB) and its counters | no | new runtime's one cache; the pin keeps one `VkPipelineCache` per record family, at most `LIVE` (`shader_cache/driver.rs`) |
| `c03422017` | private memory past 16 KiB in the runtime's arena | no | new runtime (`arena.rs`); whether our clients' kernels use that much private memory is not measured (Turnip gives each such pipeline a private memory BO of its own) |
| `89483d65c` (2026-11-12, old runtime) | an idle device lets go of its kept empty blocks and parked objects | ported | the same fault as "The idle memory" (section 5.1): its idle-sweep thread needs the free thread and the 11-08..11-11 block changes (8 files conflict); `userland/metal/m2v-idle-memory.patch` does the same from the plug-in's 2 s trim |
| `ac9e2adf6`, `011434681`, `5ef0971b1`, `01207df53` (2026-11-09..13, old runtime) | spare command pools follow their recent use | no | the same fault and cure as `m2v-spare-command-pools.patch`, which stays (still applies, still needed); `ac9e2adf6` conflicts with Droid-VM 0022 in `spares.rs` and needs `6ee9db2b6`'s pending audit (12 files conflict) |
| `e1d9d034b`, `05dcfce04`, `074d8e762` ... (2026-11-08..09) | pooled blocks made ahead | no | more memory held, not less; for desktop GPUs whose block making stalls submissions |
| `2483178f5`, `186d327fe`, `6be762136` (2026-11-08) | emptied blocks kept within a heap budget | no | keeps more empty blocks than the pin's two a pool; the pin's 1 s plus the plug-in's 2 s trim is less |
| `653060d6e`, `930e3aa49`, `6346da260` (2026-11-09..11) | image blocks of 68 MiB so four quarter-block images fit | no | the same size question as the 16 MiB blocks below; not measured on the phone |

**Mobile (a tiler: Adreno pays for each attachment load and store), read in the pin's runtime:**

- Load and store actions map one to one (`render_target.rs` `load_op`, `store_op`): `Load`/`Clear`/`DontCare` to
  `LOAD`/`CLEAR`/`DONT_CARE`; `DontCare` and `MultisampleResolve` stores to `DONT_CARE` on Turnip (Droid-VM 0021,
  `DriverFact::DontCareStoreIsOneOfMetals`; 0024's per-class mask). Nothing to add. **On the rt add-on (what releases
  ship since v0.1.36) the same mapping, rt 0005, is off by default since 2026-11-28**: rt fixes a store as a rendering
  begins and begins a split pass's next rendering with `LOAD` (`render_encoder.rs` `interrupt`: memory barriers,
  attachment reads' feedback moves and shadow copies, copies of textures over buffers), so a `DONT_CARE` store lost the
  draws before each split on Turnip (read in the code as the cause of v0.1.40's garbage band in page transitions;
  `userland/metal/rt/README.md`, "DontCare stores and split passes"; the A/B is `oneios-dont-care=1`).
- Memoryless textures: `TRANSIENT_ATTACHMENT` usage and the `LAZILY_ALLOCATED` type first (`image_plan.rs`,
  `memory.rs` `candidates`); Turnip's darwin backend offers that type (`has_lazy_bos`, `TU_DARWIN_LAZY_BOS`), its memory
  made only where a pass renders in system memory. A pass that splits renders a memoryless color into a backing image
  (`attachment_stand_in.rs`; Droid-VM 0029 shares them). Nothing to add.
- Passes: one Vulkan render pass per Metal render encoder, split only where Metal's order needs it (the program's
  memory barriers, tile dispatches, attached reads, feedback, shape changes: `work_counts.rs` `instance_splits_*`);
  each split stores and loads every attachment. Consecutive encoders on the same attachments are not merged (Metal's
  own driver does not merge them either); merging a `Store` then `Load` pair with nothing between is a possible tiler
  win, not done: the new `RP` row (above) first says how often passes split on the phone.
- Barriers: Turnip folds a barrier's stages into three (CP, BV, BR; `tu_flush_for_stage` in `tu_cmd_buffer.cc`) and
  waits for idle whenever the source stage is not before the destination, with the cache flushes its access bits ask;
  between two passes (fragment or compute to the next pass's reads) that wait comes whatever the masks say, so a
  narrower stage mask saves little there (read in the code, not measured). The full-barrier switch (Droid-VM 0025) is a
  diagnostic and stays off.
- The GPU block size: 64 MiB (`BLOCK_BYTES`). 16 MiB (`METAL2VULKAN_BLOCK_MIB`, Droid-VM 0019, from
  `/private/var/ngpu/debug/block-mib`) hung the render server's GPU context in 3 of 5 app open and close cycles (0 of 5
  at 64; NativeMetal `NativeAdrenoMetalPlugin.mm` `boundPooledBlocks`). Why is not known: the only record is that note,
  which says that with 16 MiB more images take dedicated blocks and spares (anything past 8 MiB). Not fixed upstream in
  the old runtime (its blocks stayed 64 and 68 MiB); the new runtime has no pooled blocks of its own (the consumer
  takes them). Left at 64; the `GM` row now says how much of the pooled bytes hold placements.
- Turnip's own knobs: the plug-in sets `TU_DEBUG=nocb` in every Metal process (concurrent binning off: BV ran ahead
  of BR and the two stopped waiting on each other inside a pass, int-261p, int-262, int-265), and `sysmem,nocb` with
  `sheng-fb-bgra=1` (and since 2026-11-28, in `metal2vulkan-rt`'s `RtSwitches.inc`, `sysmem,nocb` by default for 8-bit
  BGRA displays whenever `oneios-display-wide=1`, `oneios-gmem=1`, and `sheng-tu-debug=` are absent, to avoid Turnip
  GMEM tile corruption and slower latency on 8-bit BGRA framebuffers). Must stay off in a normal boot:
  `TU_DEBUG=gmem`, `forcebin`, `nolrz`, `noubwc`, any `FD_MESA_DEBUG`,
  `IR3_SHADER_DEBUG` or `MESA_SHADER_CACHE_DISABLE`-style switch beyond what the plug-in sets, and `TU_DARWIN_BO_CACHE=0`.
  The base's Turnip build drops every `TU_`, `FD_MESA_`, `IR3_`, `NIR_` and `MESA_SHADER_` variable of the build host.

**The first boot after this add-on.** The translator's records (`v1/<build>/`) are keyed by the translator's source,
which did not change, so nothing is translated again. Turnip's `pipelineCacheUUID` is the build's `-Dtu-build-id`, a
hash of the Mesa revision and the source diff (`build-turnip-darwin.py`), and `turnip-idle-bo-cache.patch` changes the
diff (so does `turnip-submit-split.patch`, 2026-11-25: the add-on after it recompiles once more): every pipeline's driver record misses once, and Turnip compiles it again (NIR to ir3) at its first use. How long
that takes on the phone is not measured (no line times it); each app's first opening after the add-on pays for its own
pipelines, and later boots hit.

**The port to metal2vulkan-rt (2026-11-23, active):** [`userland/metal/rt/README.md`](../userland/metal/rt/README.md). Upstream rewrote the runtime
(`metal2vulkan-runtime` 0.2); the inventory of our 56 patches on it, the loader-free instance (rt patch 0001, run on
the laptop through ANV: 1000 of 1000 outputs right), Turnip as an Adreno 750 against rt's floor (above it with rt
patch 0002; no `VK_EXT_device_generated_commands`, so rt refuses Metal indirect command buffers there) and a census
of the guest's own metallibs. `METAL_RUNTIME=rt userland/metal/build.sh` builds the plug-in with the bridge rewritten on
rt (all 184 functions of NativeMetal's C ABI plus `nmb_encoder_memory_barrier` for intra-encoder compute/render
barriers) and rt patches 0001-0011 (including `0009` 100 ms `vkWaitSemaphores` polling while GPU work is in flight so
KGSL resets surface `VK_ERROR_DEVICE_LOST`, `0010` porting OneiOS's large indexed constant blob fixes `0101`/`0102`,
and `0011` GPU compute conversion for two-plane YCbCr/RGB IOSurface video textures), in `out/metal-rt`; the 624bab3
build stays as fallback (docs/switches.md, "The Metal add-on").

Built with the toolchain of [building.md](building.md) section 9: Xcode's on macOS, the cross toolchain on Linux. The
same command on both; every link is against this guest's own iOS 27 stubs (`out/ios27-tbd`).

```sh
scripts/bg.sh metal userland/metal/build.sh      # stage, turnip, plugin, finish, addon (each also runs alone)
```

**What it builds**, all for `arm64e-apple-ios27.0`:

| Piece | Source | Notes |
|---|---|---|
| `libvulkan_freedreno.dylib` | `external/mesa-droidvm` cecc0e9 (Droid-VM/mesa, `droidvm`) + the 30 patches listed in Droid-VM-UniXNU `scripts/data/adreno-compiler-inputs.json` (`patches/mesa/`: `turnip-darwin-kmd`, `turnip-darwin-virtgpu`, ...), then this project's `userland/metal/trace-turnip-init.patch`, `turnip-idle-bo-cache.patch` and `turnip-submit-split.patch` (Droid-VM/UniXNU fb9fb47: a kernel submission ends only where a command buffer begins; appended to the staged list by `ios metal-stage-edit`) | meson cross build, `-Dvulkan-drivers=freedreno -Dfreedreno-kmds=darwin`, release. Its 5633 weak flat-namespace imports are bound to the image itself (`macho-weak-self-binds.py`) |
| `libadreno_metal_bridge.a` | `external/metal2vulkan` 624bab3 + NativeMetal `bridge/patches` 0001-0024 (without 0020) + Droid-VM-UniXNU `patches/metal2vulkan` 0011-0038 + this project's `userland/metal/trace-metal-vulkan-init.patch`, `m2v-spare-command-pools.patch` and `m2v-idle-memory.patch` (appended to the bridge script's list by `ios metal-stage-edit`) + `userland/metal/patches/metal2vulkan/` 0101, 0102 and 0110 (staged as 9xxx and appended after those, the Adreno-generic shader-translation fixes of hakios-arm64e-dada and their switch, off without `oneios-m2v-shader-fix=1`; 0103 staged but in no list; `0104…NEEDS-REBASE` stays out, and the Mesa/nativemetal sets stay out as the wrong tree), behind `bridge/NativeMetalBridge.rs` | Rust 1.98.0, target `arm64e-apple-ios`, std built from `rust-src`, fat LTO. No `Cargo.lock` is pinned upstream: the crates are the newest that fit at build time |
| `NativeAdrenoMetal` | `external/NativeMetal` b0d6a07 (`NativeAdrenoMetalPlugin.mm`, `NativeBridgeMetalFrontend.mm`), `external/AdrenoPM4`, the bridge | The Rust objects' callback imports go through `RustPacBoundary.c`; their indirect branches through its trampolines (`rust-pac-rewrite.py`) |

The steps are the kernel base's own scripts (`build-turnip-darwin.py`, `build-adreno-metal-bridge.py`,
`build-adreno-metal-plugin.py`, `--target ios27`), run from a staged copy of its tree in `out/metal/root` with
`userland/metal/shim/` first in `PATH`: `xcrun`'s clang is `scripts/ios/ios-clang` (so the links go through `ios-ld`
and the stubs), `codesign` is `oneios-tool ios codesign` (over `ios sign`), `strip` and `nm` are the linker's siblings; on macOS every
other `xcrun` question, and any about another SDK (rustc's `--sdk macosx` for the crates' build scripts), goes to
Xcode's `xcrun`, and `SDKROOT` is left unset so those build scripts link for the Mac. The staged copies differ from
upstream in the lines `stage` prints: meson's build-machine compiler, ninja's job count, `-D_DARWIN_C_SOURCE` in Mesa's
cross `c_args`, `cpp_args` and `objc_args` (flex 2.6.10, on the build laptop since 2026-11, writes `#define
_POSIX_C_SOURCE 1` into each scanner it makes, and Darwin's `dlfcn.h` then hides the `Dl_info` that `ir3_lexer.c` gets from
`disk_cache.h`; macOS's flex 2.6.4 writes no such line, so the Mac never saw it; a file that sets no POSIX level of
its own is at Darwin's full level already, so the define changes nothing for it), a longer limit for the Rust build,
no `TurnipReplay.h` (it is recorded by a Mac-only host simulation; the plug-in includes it only when it
exists, so the prefix-replay diagnostic is absent), and, only when the compiler knows the warning (clang 23 on the
Linux PC, not Xcode 27.2's clang 21), `-Wno-error=unused-but-set-global`. Also not made:
`build/prime-list.txt` (the image's `.metallib` paths for shader priming; the plug-in primes nothing without it).
The script's probe daemon `adrenoprobed` is built beside the bundle (`out/metal/root/build/adreno-metal-plugin27/`)
and not placed.

**The bundle** (`out/userland/NativeAdrenoMetal.bundle`): `Info.plist` (`org.blu-sharky.NativeAdrenoMetal`, principal
class `NativeAdrenoMetalDevice`, MinimumOSVersion 27.0), `NativeAdrenoMetal`, `libvulkan_freedreno.dylib` (opened by
path by the plug-in), `oneios-target`. Both binaries are signed by `oneios-tool ios sign` (through the `codesign` shim);
`NativeAdrenoMetal.build.json` beside the bundle records the SDK, the stubs' cache, the source pins, sizes and CDHashes.

**Appended for backboardd** (`stage`, after the address check): `drivers/brightness/BacklightMode.inc` (BLControl,
`docs/hardware.md` 7a) and, since 2026-11-13, `userland/metal/DisplayWide.inc`: when the kernel's boot arguments have
`oneios-display-wide=1` (xnuhost's `display-format=rgba16f`), a constructor sets `CA_FORCE_FP16_FRAMEBUFFER=1` and
writes that option's stored byte (`_ca_debug_options` + 128, `0x1e702516c` + the slide), so the render server's
framebuffer is `RGhA` half floats and the display path keeps them whole
(`kernel/patches/oneios-hosted/display/README.md`, "Colour, depth and the panel"). iOS's log then has `OneiOS display:
half-float framebuffer asked (...)`. The headroom, threshold and virtualized overrides below stay as they are. Not
built or compiled yet (the plug-in builds on the Mac).

**Lasting debug tooling, off by default** (`stage`): `userland/metal/trace-turnip-init.patch` (into
`patches/mesa/` and the staged mesa-work tree, and appended to `adreno-compiler-inputs.json`),
`trace-metal-vulkan-init.patch` (into `patches/metal2vulkan/`), and `trace-metal-surfaces.patch` (into `patches/`,
applied to the staged frontend after the upstream replacements). Each only logs when its environment variable is
set (`ONEIOS_VK_INIT`, `ONEIOS_SURFACE_TRACE`), so a normal build is unchanged.

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

**iOS 27.2.1 (24B156)** has its own hooks file, `userland/metal/hooks/iPhone18,2-24B156.json` (cache UUID
`78B55CFF-20F4-3A39-81AF-B6530B7E9789`, read 2026-11-25 from 24B156's SystemOS cryptex with `image apfs-copy`, then
`oneios-tool sym find`, `sym find --no-index` for the data symbols and `sym dis` of the three functions; no `ipsw`).
Everything moved, and two of QuartzCore's bool debug options got other numbers (300 options, 24A446 has 293), so the
hooks file also has `"options"`: `oneios-tool ios metal-stage-edit` rewrites every `_ca_debug_options + N` of the
plug-in with the build's N and puts the build's constants at the top of the staged plug-in as `#define`s
(`ONEIOS_CA_RENDER_SERVER_START`, `ONEIOS_CA_DEBUG_OPTIONS`, `ONEIOS_CA_OPTION_<NAME>`) for the OneiOS files appended
to it (`DisplayWide.inc` uses them; it has no address of its own any more). Built with
`ONEIOS_METAL_TARGET="iPhone18,2 24B156" userland/metal/build.sh` (output `out/userland-24B156`, or `METAL_OUT`).
Built and checked on the build laptop 2026-11-25 (the Turnip and plug-in checks PASS); **not run in a guest**.

| What | 24A446 (iPhone18,2) | 24B156 (iPhone18,2) |
|---|---|---|
| `_CARenderServerStart` (the anchor) | `0x1846dce88` | `0x1845ce544` |
| `framebuffer_pixel_format`; its `tbnz w8, #0` (word `0x37000608`) | `0x184461838`; `0x184461928` | `0x18434cff4`; `0x18434d0e4` |
| `debug_values[21]` (`CA_DISPLAY_EDR_HEADROOM`) | `0x1e7025014` | `0x1edf3e474` |
| `debug_values[81]` (`CA_FP16_MIN_HEADROOM`) | `0x1e7025104` | `0x1edf3e564` |
| `_ca_debug_options` (force FP16, EDR mirroring) | `0x1e702516c` (+128, +156) | `0x1edf3e5cc` (+129, +159) |
| `CADeviceIsVirtualized::is_virtualized` | `0x1e701fdd9` | `0x1edf390c1` |
| `CADeviceIsVirtualized::once` | `0x1e7024780` | `0x1edf3a908` |
| the constant CFString of that query | `0x1e9025680` | `0x1e2280290` |

QuartzCore's code is at the same addresses in both caches; its `__DATA_DIRTY` and the CFString are 0x4000 higher in
the iPhone's. Placed as upstream builds it, the plug-in would have written the headroom, the threshold, the option
byte and the once token 16 KiB below their places in backboardd. How they were found (with blacktop's `ipsw`
program, a reverse-engineering tool installed by hand from its releases: no script of this project needs it since
2026-11-21, when `scripts/fetch/get-ipsw-cli.sh` went; `oneios-tool image apfs-copy out/image/24A446/systemos.img
System/Library/Caches/com.apple.dyld out/ios27-cache` makes the cache's copy without a mount):

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
**Built on the Mac** (2026-11-12, Xcode 27.2: SDK 27.2, Apple clang 21.0.0, ld-27106.1; Rust 1.98.0; stubs of
cache `77BC8C63`, 4723 stubs): `scripts/bg.sh metal userland/metal/build.sh`, 6 minutes 12 seconds with the Rust crates
already fetched (Turnip about 4 minutes, the bridge and the plug-in about 2). `macho-check.py` passes for both binaries
(259 of Turnip's 5892 imports bound to 24A446 stubs, 5633 weak ones bound to itself).

| File | Linux PC (2026-11-11, SDK 26.5, clang 23, ld64-956.6) | Mac (2026-11-12, SDK 27.2, Apple clang 21, Xcode's ld) |
|---|---|---|
| `NativeAdrenoMetal` | 19,091,344 bytes, CDHash `18535d2ab99e64fc2afa989e5000fc02c9e46f61` | 18,894,416 bytes, CDHash `efcefba302d65696438ff061c0a2c298aab86bff` |
| `libvulkan_freedreno.dylib` | 16,738,288 bytes, CDHash `23299678fa1606669218773f458e53c2583f3bc4` | 16,871,920 bytes, CDHash `2899d180e9da5beea0aa7220a12c4f2af39064d9` |

They differ because the compilers, the linkers and the SDK headers differ (the sources and the stubs are the same).
A set built from either lists its own two CDHashes in `owner-trust.json`; the plug-in of one set does not run
under the other's trust list. `ios-sign.py` is not a cause: it writes what Apple's codesign writes (Turnip's dylib
re-signed by both: the same bytes).

**Not verified:** anything at run time. No guest has loaded the bundle: until `0031-gpu-addon-mount.patch` no kernel
showed it to iOS (section 5.1), and that patch has compiled but not run. Whether dyld and AMFI of this guest accept the binaries, whether the
writes above have the effect they have on the iPad, and whether MetalCheck passes are all open.

