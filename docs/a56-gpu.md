# The Galaxy A56's GPU: Metal and OpenGL ES on the Xclipse 540 (iOS 16.5, 20F66)

Owner's ask (2026-11-24): "port it fully and give it for the next update". GPU-drawn Metal for iOS 16.5 on the
Galaxy A56 (SM-A566E, Exynos 1580, Samsung Xclipse 540 = AMD RDNA3, host render node `/dev/dri/renderD128`, Linux
`amdgpu`/`sgpu`). Until this port the A56 runs `gpu=2d`: the CPU composites a 720x1560 @2x frame (v0.1.24,
[a56.md](a56.md) "The second port").

**State, plainly.** Nothing of this file has run on an A56: there is none here and no qemu boot (owner's rule). Each
section says what is built, what its host tests cover, and which A56 log lines would prove it. The S24 Ultra, iOS 27
and the Adreno path are unchanged by all of it: every part is behind the `galaxy-a56` host row, an amdgpu render
node, the image's build (`20F66`) or the A56's own Metal plug-in.

## 1. The design and its references

```
iOS 16.5 process: Metal -> the A56 plug-in (the AIR Metal layer of Jesvi-iOS, on Mesa Gallium)   section 4
   | Mesa radeonsi (ACO) -> amdgpu winsys -> amdgpu virtio native context (src/amd/common/virtio) section 3
   | vdrm on Darwin: IOKit AGXDeviceUserClient open type 0x4149, DROIDVM_VIRTGPU_* methods
hosted XNU: the accelerator and DroidVMVirtioGpu3D.cpp (virtio-gpu, capset 6, DRM context)      section 5
   | virtio-mmio /arm-io/virtio-gpu
xnuhost: src/virtio_gpu.rs -> src/gpu3d.rs -> src/gpu3d_amdgpu.rs (Rust)                         section 2
   | ioctls on /dev/dri/renderD128 (amdgpu), guest pages as udmabufs
```

The Adreno path is the same shape with Turnip, metal2vulkan and drm2kgsl ([gpu.md](gpu.md) 1); here there is no
Vulkan: Metal goes to Gallium directly, as in Jesvi-iOS.

OpenGL ES (section 4a) is a second user of the same radeonsi: `OneiOSRadeonGL.bundle`, Mesa's GLES frontend over the K3
screen, which iOS's OpenGLES loads by the accelerator's `IOGLESBundleName`. backboardd's CoreAnimation composites on it
(its OpenGL ES renderer) while it stays off the Metal plug-in.

| Reference | Pin (`scripts/setup/get-references.sh`) | Licence | What is taken |
|---|---|---|---|
| `jesvi-eulah/Jesvi-iOS` (iOS 16.5 on the Galaxy S20, Exynos 990 Mali-G77; HakiOS-based) | `a13e406` (main table) | MIT outside `xnu/` (its `LICENSE`, `NOTICE`); APSL-2.0 in `xnu/` | `tools/mesa/air/*.c,*.h` (the AIR frontend: AIR bitcode into NIR; `air_metal.c`, Metal's needs of Gallium), the Mesa patches that wire them (`air-frontend`, `air-metal`), `userland/src/redwoodmetal.m` and its plug-in plist (the Metal device classes): copied with their notices into our tree and credited (section 4). Not taken: `prebuilt/` (binaries), every `.py` tool (no Python here), the Panfrost and freedreno patches (another GPU), `xnu/` |
| `Musicalista/OneiOS-A56` (the other A56 fork) | `c83b9ca` (main table; the GPU work is its `e7d56f2`) | per file: `amdgpu_renderer.c` keeps AMD's MIT notice (virglrenderer's), `gpu3d_xclipse.c` is `SPDX MIT`; the repository states none | the protocol and the self-test's steps of `runtime/xnuhost/amdgpu/src/amdgpu_renderer.c` and `src/gpu3d_xclipse.c`, rewritten in Rust (section 2); the guest probe's steps (`userland/metal/amdgpu/`). No file copied as is |
| Mesa (`Droid-VM/mesa` `cecc0e9`, `mesa-droidvm`) | main table | MIT | radeonsi, ACO, the amdgpu winsys, `src/amd/common/virtio` (the guest half of the amdgpu native context, wire `amdgpu_virtio_proto.h`), `src/virtio/vdrm` |
| `Droid-VM/NativeAdreno` `627836d` | main table | MIT | `DroidVMVirtgpuABI.h`: the user client's virtgpu methods the guest's vdrm calls |
| libdrm 2.4.133 (the release Mesa cecc0e9's `subprojects/libdrm.wrap` pins) | sha256 `fc68f9d0ba2ea63c9432a299e14fea09fad7a8a66e8039fcd7802ca59f77b4f5`, fetched by `userland/metal-amd/build.sh` into `out/metal-amd/dl` (not `get-references.sh`) | MIT | `xf86drm.h`, `include/drm`, the amdgpu library (its VA manager) built for Darwin (section 3) |
| `Droid-VM-UniXNU` `patches/mesa/turnip-darwin-virtgpu.patch` | kernel base (`kernel/pins.sh`) | MIT | the model of a Darwin vdrm backend (`tu_knl_darwin_virtgpu.cc`) |
| `PixOS-Project/pixelios-kernel`, `pixelios-userland` (Pixel 6a, Mali via Panfrost) | `f084ce5`, `d5b2079` (extra table) | MIT | ideas only so far |
| `oosexgang/hakios-xnu-kernel` | `62e09f8` (extra table) | none stated | ideas only, read only |
| `open-mapple/mapple_gpu_mali` | `f893fed` (extra table) | GPL-3.0 | ideas only: no code into our MIT or APSL files |

**A limit of the design, found in Jesvi-iOS's own record (read 2026-11-24).** Jesvi-iOS's Metal runs per process:
CoreImage, Photos, Maps, posters and games through GL-on-Metal (`docs/SHIPPING.md`, 2026-11-14 and 11-16), with
"metaloff" for PosterBoard, mediaserverd and others, and its kernel "filters discovery for incompatible core
renderers". CoreAnimation's render server in backboardd on it crashed at start and was reverted
(`docs/devices/x1slte/display.md` line 72: "Trying CoreAnimation on Metal in backboardd live ... crashed it at start
(stack overflow ...): reverted"). The A56's low frame rate is backboardd's CPU composition, so this layer alone does
not raise it: backboardd on this Metal is open work (section 6).

## 2. Host: the Xclipse native context in xnuhost (`src/gpu3d_amdgpu.rs`)

**State (2026-11-24): written, host-tested on stand-ins, built for Android; not run on an A56** (none here). The
amdgpu native context of virglrenderer, rewritten in Rust with raw ioctls (no libdrm, no C renderer linked), behind
the same `Gpu3d` trait as drm2kgsl (`src/gpu3d.rs`), on `/dev/dri/renderD128`.

| File (`runtime/xnuhost/`) | What |
|---|---|
| `src/gpu3d_amdgpu.rs` | The renderer: the device's figures and the capset (`probe`), the contexts (each its own DRM file and GPU VM), the response shmem in the pool, every `AMDGPU_CCMD_*`, the resources and their udmabuf chunks, the 10 s line, the choice (`choose`). Credits AMD's MIT notice of virglrenderer's `amdgpu_renderer.c` in its head |
| `src/gpu3d_amdgpu/uapi.rs` | The kernel's structs and ioctl numbers (drm.h, amdgpu_drm.h as Mesa cecc0e9 has them) and the guest's wire as offsets (amdgpu_virtio_proto.h, drm_hw.h, libdrm's `struct amdgpu_gpu_info`) |
| `src/gpu3d_amdgpu/layout.c` | Host builds only (`build.rs`): every size, offset and number of `uapi.rs` from those headers (Mesa's from `external/mesa-droidvm`, libdrm's `amdgpu.h` from `pkg-config libdrm_amdgpu` or `/usr/include/libdrm`); test `the_layout_is_the_headers` compares them one by one |
| `src/gpu3d_amdgpu/fences.rs` | The virtio fences: one thread (`amdgpu-fences`) blocks in `poll(2)` on the oldest sync_file of each ring and an eventfd; no timeout, no sleep |
| `src/gpu3d_amdgpu/addon.rs` | Whether the kernel's gpuaddon sidecar (NGPUADD2) carries `addons/OneiOSRadeonMetal.bundle/OneiOSRadeonMetal` and its `oneios-target`, and which build that names (read with `xh_core::inflate`, a zlib decoder added for it) |
| `src/gpu3d_amdgpu/selftest.rs` | The self-test (below), and the guest-side encoder of Mesa's requests the tests reuse |
| `src/gpu3d_amdgpu/tests.rs`, `tests/standin.rs` | 21 host tests on a stand-in amdgpu behind the renderer's `Dev` table (below) |
| `tools/gpuctl.rs` | `gpuctl amdgpu [NODE] [-v]`: the self-test alone on the phone; `gpuctl addon FILE [BUILD]`: the plug-in check of a gpuaddon |

**Sources, and where this follows upstream.** Written from virglrenderer's `src/drm/amdgpu/amdgpu_renderer.c`
(`external/virglrenderer` 7e17c8e has upstream's file; the A56 fork's `runtime/xnuhost/amdgpu/src/amdgpu_renderer.c`
is that file plus its responses in xnuhost's pool, and its `gpu3d_xclipse.c` the self-test's steps), its
`drm_context.c` and `drm_fence.c`, and the libdrm 2.4.133 calls the C makes (Mesa's own `libdrm.wrap` tarball,
read for `amdgpu_query_sw_info`, `amdgpu_va_manager_init2`, `amdgpu_query_buffer_size_alignment`,
`amdgpu_bo_query_info`, `amdgpu_cs_query_fence_status`), as ioctls. Upstream's semantics are kept (the copy of each
request zero-extended to its size, the response shadowed and copied up to the guest's length, the seqno after each
command, a failing command ending the stream, the checks of each command) except where K2 or the cost says
otherwise:

- **GEM_NEW allocates nothing** (K2): it records blob_id, size, alignment, heap and flags. The `RESOURCE_CREATE_BLOB`
  that follows (HOST3D_GUEST, the same blob_id, **the same size**: another size is refused) brings the guest's
  pages; `gpu3d::udmabufs` makes one or more udmabufs ("chunks", up to 64 MiB and 1024 runs each), each imported with
  `PRIME_FD_TO_HANDLE`. BO_VA_OP maps each chunk the range covers at its place (`va + chunk offset - range offset`;
  a map that fails half way unmaps the pieces before it); CLEAR, PRT and sparse (`res_id` 0) ranges go through once.
  A CS_SUBMIT's BO list names every chunk of each resource with its priority; a user fence (`CHUNK_ID_FENCE`) names
  the chunk that holds its offset. A HOST3D blob with blob_id != 0 is refused (EINVAL, one line).
- **The capset says `has_vm_always_valid = 0`** (upstream 1). With 1, Mesa's amdgpu winsys makes its BOs
  `AMDGPU_GEM_CREATE_VM_ALWAYS_VALID` and leaves them out of the BO lists (`amdgpu_bo.c` 639, `amdgpu_cs.cpp` 1968);
  an imported BO cannot be per-VM always valid, and the kernel would not make it resident. **The guest Mesa stage
  needs nothing for this**: it reads the bit from the capset.
- **BO_QUERY_INFO answers from the GEM_NEW and the SET_METADATA the guest sent** (upstream asks the kernel, which knows
  its import: GTT, one chunk's size). A display surface (GUEST blob, no GEM_NEW) answers GTT and its size.
- **The shmem's heap figures are written once**, when the shmem is made (upstream re-queries them after every BO:
  about five ioctls per BO, and the host's heaps hold none of the guest's BOs).
- **The response shmem is in the pool** (as the fork and as drm2kgsl's msm shmem): first-fit from the 8 MiB
  drm-host pool, page-granular, zeroed when handed out, its first 2 MiB never used (drm2kgsl's guard: the same
  guest-visible offsets); MAP_BLOB answers its offset with MAP_CACHE_CACHED. Freed at its unref.
- **One syncobj per ring of a context**, made at its first submission and kept (upstream one per CS_SUBMIT); the CS
  puts its fence there (`SYNCOBJ_OUT`), and `submit_fence` exports it as a sync_file only when the ring had work
  since its last fence. A fence with no work since the ring's previous fence retires after that one, in order
  (upstream retires it at once even with earlier fences of the ring running); ring 0 and an idle ring at once. No
  in-fence (the device has none to give; the fork has none either).
- **The rings** are the HW IP rings counted once at the open (instance 0, each IP type's `available_rings`, in type
  order: the guest's `virtio_ring_mapping`), not per context.
- **Each context opens the node again** (as upstream: its own DRM file and GPU VM per guest process; the kernel's
  implicit sync covers what two share). A display surface is imported into a context when it is attached; unref
  closes every context's handles of it.
- **The capset's gpu_info** is Mesa's `ac_drm_query_gpu_info` (libdrm's `amdgpu_query_gpu_info_init`): DEV_INFO, then
  GB_ADDR_CONFIG by `AMDGPU_INFO_READ_MMR_REG` 0x263e (a refusal fails the probe: the guest's addrlib needs it), the
  pre-Vega registers only below family 141. `address32_hi` as libdrm computes it from the high VA range;
  `alignments` = pte_fragment_size, gart_page_size. The marketing name is not libdrm's table (the host has no
  `amdgpu.ids`): `AMD Radeon (DEVICE_ID)`, or `Samsung Xclipse` on an `sgpu` driver.

**The driver name is a guess in part.** The node is taken when DRM_IOCTL_VERSION says `amdgpu` ([F] the A56 fork's
`docs/a56-guide.md`: "amdgpu (v3.49.0) over /dev/dri/renderD128") or `sgpu` (Samsung's own name for its Xclipse driver:
a guess, not seen on an A56). `gpu-node=PATH` (xnuhost.conf or `--set`) names another node.

**Protocol coverage** (every ccmd Mesa cecc0e9's `amdgpu_virtio*.c` sends): QUERY_INFO (passthrough of any
`AMDGPU_INFO_*`, up to 1 MiB, the kernel's errno in `ret`), GEM_NEW, BO_VA_OP (MAP, UNMAP, REPLACE, CLEAR; sparse),
CS_SUBMIT (BO_HANDLES, FENCE, DEPENDENCIES, IB: the four Mesa forwards; its SYNCOBJ chunks become virtgpu syncobjs in
the guest and never reach the host; up to 128 chunks, the IB 8 words), SET_METADATA (every chunk, kept for
BO_QUERY_INFO), BO_QUERY_INFO, CREATE_CTX (and its DESTROY flag), RESERVE_VMID (and UNRESERVE), SET_PSTATE (GET and
SET), CS_QUERY_FENCE_STATUS (WAIT_CS, a relative timeout made absolute as libdrm does; fence 0 is done). **Not
served:** the in-fence (above); `resource_map` of a host BO (there are none: K2); `export_opaque_handle` (nothing on
the host imports a guest BO).

**The self-test** (`selftest.rs`; xnuhost runs it at the start before it picks this renderer, `gpuctl amdgpu` alone):
1. the fork's steps by raw ioctls: a udmabuf of a sealed memfd imported (PRIME), mapped (GEM_VA), an amdgpu context, a
   BO list, an IB of eight `0x80000000` (the fork's GFX PACKET2 NOP, PASS on the A56 2026-11-13/14 in its record)
   submitted, its fence waited for (WAIT_CS, 1 s);
2. the same through the renderer's own Gpu3d calls over a memfd of its own (8 MiB, pool in its last 4): the capset
   (592 bytes, context type 2), a context, its shmem from the pool, QUERY_INFO DEV_INFO answered there (the seqno),
   GEM_NEW and a HOST3D_GUEST blob of 3 udmabufs (one per page, 3 pages apart), CREATE_CTX, BO_VA_OP over the three,
   CS_SUBMIT of the NOP IB (in the BO's last chunk) with the BO in its list on the GFX ring, the virtio fence retired
   through the retire callback (at most 2 s).
Nothing outlives it (its memfd, renderer, fence thread and DRM files go when it returns); no realtime thread.

**The choice** (`session/board.rs`, `gpu3d_amdgpu::choose`). The `galaxy-a56` row's default is now `gpu=auto`
(`runtime/bridges/hw/host_device.rs`; gpu=2d before). With `gpu=auto` or `gpu=3d`:
- a host with `/dev/kgsl-3d0`, or one with no amdgpu render node: the KGSL path, its code and lines unchanged (the
  amdgpu checks are not even asked: test `the_choice_of_renderer`);
- an amdgpu node without KGSL: the renderer when **(a)** the kernel's gpuaddon sidecar carries the OneiOSRadeonMetal
  plug-in whose `oneios-target` names the image set's build, and **(b)** the self-test passes; then
  `oneios-gpu-amd=1` is added to the boot arguments (K5);
- otherwise 2D exactly as before, one line saying why; with `gpu=3d` the session refuses to start instead.
`gpu=2d` (the command line, the image's `xnuhost.conf`) gives the 2D path with no GPU pool, as before. Under `gpu=auto`
an A56 whose gpuaddon has no A56 plug-in is laid out as `gpu=2d` too, before the RAM is laid out
(`gpu3d_amdgpu::early_2d`, test `an_a56_without_its_plugin_is_laid_out_as_2d`): no GPU pool, the window and device
tree of the 2D session before this renderer (fixed in the integration of 2026-11-24; the first version kept the pool).
Only a failing self-test (b) with the plug-in present leaves the 8 MiB pool reserved and unused, as on a KGSL phone
whose KGSL fails.

**Host tests** (`cargo test -p xnuhost --lib gpu3d_amdgpu`, in `runtime/host-tests.sh`): the capset's bytes;
`address32_hi` without a high range; only an amdgpu or sgpu node; a DRM file per context, EEXIST, a wrong capset;
the shmem in the pool past 2 MiB (its header, heaps, a second refused, a HOST3D blob with blob_id != 0 refused, the
range zeroed and reused after unref); QUERY_INFO in the shmem (seqno, a refused query's errno, two in one submit, the
guest's length bound, too large, bad lengths, unknown command, trailing bytes, a response outside the shmem); GEM_NEW
and a 3-chunk blob, BO_QUERY_INFO from GEM_NEW, SET_METADATA on each chunk; every refusal of blobs (no GEM_NEW, size
mismatch, blob_id 0 or over 32 bits, GEM_NEW's padding, blob_id in use, size 0, async error counts); a display
surface imported where attached; VA maps across chunks (whole, a part starting and ending inside chunks, REPLACE,
CLEAR, sparse, refusals, a failed piece unmapping the ones before it); a submission's BO list expansion, the user
fence's chunk, dependencies, the ring's syncobj made once, an IB outside the list's maps refused by the stand-in; every
CS refusal (ring 0, ring 8, unknown context or resource, two lists, an IB of 6 words, chunk 0x0a, fence offsets,
129 chunks, descriptors past the request, data past it); contexts, pstate, VMID, fence queries; fences retiring in
order when the later work finishes first, rings apart, a context's fences dropped with it; the self-test passing and
failing (a GPU that never finishes, a refused import, no node); the plug-in in the sidecar (found, other build, none,
NativeAdreno only, no oneios-target, not NGPUADD2, cut); the choice (S24 Ultra stays KGSL, A56 with/without the
plug-in, self-test pass/fail, gpu=3d); a blob of 1100 pages through the real `/dev/udmabuf` (two or more chunks
mapped end to end); the layout. The config's tests: the A56 row's `gpu=auto` and `gpu-node=`. `xh_core::inflate`'s
tests (stored, fixed and dynamic blocks; every refusal).

**The A56 log lines that prove it** (xnuhost.log; `oneios-tool log` / `phone log --grep gpu`):
```
host: galaxy-a56 (Galaxy A56 (SM-A566, Exynos 1580)), matched by ro.product.model=SM-A566E; read: ...; defaults: gpu=auto, panel-fallback=1080x2340@120, width=720, height=1560, scale=2, hz=60, return-graceful-ms=4000, astc=0, hw-backend-haptics=treble, hw-backend-fallback=1
sidecar <kernel>.gpuaddon: N bytes
gpu: self-test: PASS raw: guest memory (a udmabuf of a sealed memfd) imported into /dev/dri/renderD128 and mapped at GPU address 0x...
gpu: self-test: PASS raw: an IB of 8 GFX NOPs submitted (context N, sequence N)
gpu: self-test: PASS raw: the GPU ran it (fence N done)
gpu: amdgpu on /dev/dri/renderD128: amdgpu 3.49.0, device 0x.... rev 0x.. family ..., GPU addresses 0x...-0x... (high 0x...-0x...), GB_ADDR_CONFIG 0x........, rings GFXx1 COMPUTEx. DMAx. ... (N virtio rings); pool 4 MiB; ...   (the self-test's renderer)
gpu: context 1 xnu:self-test: amdgpu, its own GPU VM on /dev/dri/renderD128, N rings
gpu: self-test: PASS renderer: a context, its shmem at pool offset 0x200000, QUERY_INFO answered there (device 0x....)
gpu: self-test: PASS renderer: GEM_NEW and a guest blob of 3 udmabufs, mapped as one BO at GPU address 0x...
gpu: self-test: PASS renderer: CS_SUBMIT of the NOP IB on ring 1 (GFX), its virtio fence retired through the retire callback in N us
gpu: amdgpu on /dev/dri/renderD128: ... pool 8 MiB; ...                                                (the session's renderer)
gpu: 3D on amdgpu (/dev/dri/renderD128): the gpuaddon's OneiOSRadeonMetal plug-in (iPhone10,3 20F66) and the self-test passed; oneios-gpu-amd=1
gpu: context N xnu:backboardd: amdgpu, its own GPU VM on /dev/dri/renderD128, N rings              (the first guest context)
gpu: 10 s host: amdgpu fences N, fence->retired mean N us max N us; submissions N, mean N us max N us; blobs made N, mean N us max N us (N udmabufs)
```
The `oneios-gpu-amd=1` at the end of the `gpu: 3D on amdgpu` line is read back from the boot arguments after the word was added (K5; "the command line's oneios-gpu-amd= is kept" when the command line named it already). Without the plug-in or with a failed
self-test, instead: `gpu: 2D only: the gpuaddon has no OneiOSRadeonMetal plug-in (the A56's Metal)` (or `the kernel
has no gpuaddon sidecar, so no ...`, `the OneiOSRadeonMetal plug-in is for "...", the image set is build ...`, `the
amdgpu self-test failed (gpu: self-test: FAIL at STEP: WHY)`). Errors are `gpu: amdgpu: ...` lines (the first 64,
then every 1000th). By hand: `adb shell su -c '/data/local/tmp/gpuctl amdgpu'` (last line `ok` or `FAILED at: ...`).
`gpuctl addon FILE [BUILD]` reads a gpuaddon (the staged `kernel.kc.gpuaddon`, or `out/metal-amd/OneiOSRadeonMetal.gpuaddon`)
the way the choice does (`plugin_in_addon`): `FILE: the OneiOSRadeonMetal plug-in, N files, oneios-target "iPhone10,3
20F66"` and `ok`, or `FAILED at: FILE: WHY` with the choice's own reason. On the build laptop (2026-11-24) the integrated
addon gives `4 files, oneios-target "iPhone10,3 20F66"` and `ok`; with BUILD `24A446` it is refused (`is for "iPhone10,3
20F66", the image set is build 24A446`), and the S24 Ultra's `NativeAdrenoMetal.gpuaddon` with `the gpuaddon has no
OneiOSRadeonMetal plug-in`.

**What the other stages must match:** the kernel stage publishes the A56 accelerator only with `oneios-gpu-amd=1`
(K5) and passes the DROIDVM_VIRTGPU_CREATE_BLOB size unchanged (the host refuses a size other than GEM_NEW's); the
guest's GEM_NEW precedes its blob in the same order Mesa's `vdrm_bo_create` sends them (the host takes GEM_NEW's
record when the blob comes). The Metal stage's sidecar must hold `addons/OneiOSRadeonMetal.bundle/OneiOSRadeonMetal`
and `addons/OneiOSRadeonMetal.bundle/oneios-target` (`oneios-tool ios addon`'s `metal_files` names only
NativeAdrenoMetal today). **Not done here:** nothing has run on an A56; the IO coherence of imported guest pages on
the Xclipse (the guest maps them cacheable; whether the GPU snoops GTT there is [U]: a render test on the phone
shows it); a per-context GPU policy like KGSL's 2.2b (none).

## 3. Guest: Mesa radeonsi for Darwin over the virtio amdgpu context

**What is built (2026-11-24, build laptop):** Mesa `mesa-droidvm` cecc0e9 with `userland/metal-amd/patches/mesa`
(0001-0006; 0001-4999 are this stage's, `5xxx-air-*.patch` the Metal layer's, applied after them), cross-built by
`userland/metal-amd/build.sh` for `arm64-apple-ios16.5` (K4: the 20F66 stubs in `out/ios16-tbd`, SDK 26.5 from xtool,
ld64-956.6, clang 23): `-Dgallium-drivers=radeonsi -Damdgpu-virtio=true -Dllvm=disabled -Damd-use-llvm=false
-Doneios-radeonsi-screen=true`, release, no GL, EGL, GBM or Vulkan. Out of it, in `out/metal-amd/ios`:

| File | What |
|---|---|
| `liboneios_radeonsi.dylib` | radeonsi with ACO, its amdgpu winsys, Mesa's amdgpu virtio context and the Darwin vdrm backend in one dylib (17,368,448 bytes, not stripped) that exports one symbol, K3's `oneios_radeonsi_screen_create(why, why_size)` (`src/gallium/targets/oneios-radeonsi/oneios_radeonsi.h`); install name `@loader_path/liboneios_radeonsi.dylib` (it goes beside the plug-in that links it) |
| `oneios-amdgpu-probe` | the lasting guest tool (below), 17,405,216 bytes |
| `oneios-radeonsi.build.txt` | the SDK, target, stubs, linker, compiler, the Mesa and libdrm pins, each patch's `cksum`, sizes, CDHashes |

Both are arm64, minos 16.5, signed by `oneios-tool ios sign` (4 KiB pages) and pass `oneios-tool ios macho-check --arch
arm64 --tbd out/ios16-tbd` (205 and 206 imports, every one exported by a 20F66 library: libSystem, libc++, libz, IOKit).

**ACO without LLVM works at this pin**, with two patches: radeonsi's gfx/compute code is built only with a GL, Vulkan,
CL or video-encode frontend (`with_gfx_compute`), so the new `-Doneios-radeonsi-screen=true` counts as one (0001); and
radeonsi wanted libelf whenever it had gfx/compute, though with ACO it makes raw binaries (`SI_SHADER_BINARY_RAW`) and
opens no ELF: libelf is now required only with LLVM, and `ac_rtld.c` without libelf only says it cannot read ELF (0002).
No LLVM build is needed.

**Darwin has no DRM (0001, `src/oneios`).** The build takes libdrm's own headers and its amdgpu library from the libdrm
release Mesa's wrap pins (`subprojects/libdrm.wrap`: 2.4.133, sha256 `fc68f9d0...`, MIT; `build.sh` fetches it once
into `out/metal-amd/dl` and checks the hash), built for Darwin: its VA manager is what the amdgpu virtio context uses,
its ioctl paths answer ENODEV. `src/oneios/oneios_drm.c` implements the xf86drm calls Mesa and libdrm_amdgpu make. The
GPU's descriptor is a descriptor of `/dev/null` (`oneios_drm_open()`; the approach of Jesvi-iOS's
`tools/mesa/redwood-drm/redwood_drm.c`, no code taken): `drmGetVersion` says `virtio_gpu` for it, so
`radeonsi_screen_create` takes its virtio path, `drmGetPrimaryDeviceNameFromFd` names one device (the amdgpu virtio
context's dedup keeps one connection per process), `drmGetDevice2` a PCI device 0000:00:00.0. PRIME and syncobj calls
answer ENOSYS. AMD's addrlib reads `__APPLE__` as Apple's own driver build (a private `addrExtDef.h`): `HAVE_TSERVER` is
its switch for the standard headers (0001). ACO's check for the CLRX disassembler calls `system()`, which iOS lacks
(0006).

**The Darwin vdrm backend (0003, `src/virtio/vdrm/vdrm_darwin.c`)**, chosen by `vdrm_device_connect` when
`HAVE_VDRM_DARWIN` (the Darwin build) is set; Linux keeps `vdrm_virtgpu.c` and `vdrm_vpipe.c` unchanged. It follows
Droid-VM-UniXNU's `patches/mesa/turnip-darwin-virtgpu.patch` (`tu_knl_darwin_virtgpu.cc`, MIT): `IOServiceMatching
("IOAcceleratorES")` (or the class `ONEIOS_VDRM_SERVICE` names), `IOServiceOpen` type 0x4149, the tunnel selector found by
the probe record `{'NTNL', 0xffffffff}`, then each method as `{'NTNL', 0x7200 + op}` and its record of
`DroidVMVirtgpuABI.h` (NativeAdreno 627836d, copied into the patch unchanged):

| vdrm | Method | Notes |
|---|---|---|
| connect | GETPARAM (ABI_VERSION, CREATE_GUEST_HANDLE), GET_CAPS capset 6, CONTEXT_INIT {6, 64 rings}, CREATE_BLOB + MAP of the shmem | refuses a kernel without CREATE_GUEST_HANDLE (K2 needs it); a capset of another context type gives no device, silently, as on Linux |
| shmem | CREATE_BLOB HOST3D, MAPPABLE, 16 KiB, blob_id 0, no ccmd; MAP (the host's pool) | K2; responses at the host's `rsp_mem_offset` |
| bo_create | CREATE_BLOB HOST3D_GUEST, flags as Mesa asks + CREATE_GUEST_HANDLE, size, blob_id, followed by the driver's ccmd (amdgpu's GEM_NEW) | K2 for every BO with blob_id != 0; handle and resource id from the reply (no RESOURCE_INFO call) |
| bo_map | MAP, once per BO; kept until GEM_CLOSE | 0004: amdgpu virtio's `munmap` is not called on Darwin (the kernel's mapping object would point at a hole); a placed map is refused |
| bo_close, close | GEM_CLOSE (after flushing buffered ccmds); the shmem's; IOServiceClose | the kernel frees a BO's pages once the context's submissions are done |
| execbuf, flush | EXECBUFFER {flags, ring, size} + commands; FENCE when the caller has out-syncobjs or waits | ccmds on ring 0; a CS_SUBMIT on its virtio ring (GFX 1, then compute, SDMA ... as amdgpu virtio numbers them) |
| wait_fence, syncobj waits | WAIT_FENCE {ring, fence, timeout} | sleeps in the kernel; a fence known retired asks nothing; no polling of ours. `vdrm_host_sync`'s seqno check after a fenced ccmd finds the host done (the ring-0 fence retires after the host processed it) |
| (`vdrm_darwin_bo_create_user`) | CREATE_BLOB_USER over the caller's 4 KiB-aligned pages, with its GEM_NEW | for an IOSurface import later; nothing in the winsys calls it yet |

Syncobjs (0003, `vdrm_darwin_get_sync`; 0004 hands it to the winsys in place of `util_sync_provider_drm`): a syncobj is
unsignalled, signalled, or holds the (ring, fence) of the EXECBUFFER that names it as an out-syncobj. Waits follow DRM:
a syncobj with no fence is -EINVAL unless WAIT_FOR_SUBMIT, which waits on a condition until a submission gives it one;
the timeout is absolute (os_time_get_nano); -ETIME. An EXECBUFFER's in-syncobjs on another ring are waited for (in
WAIT_FENCE) before it is sent; on its own ring the host keeps the order. Binary only: no `timeline_wait`, so
`has_timeline_syncobj` is false; no syncobj or sync-file descriptors, no dma-bufs (import and export answer ENOTSUP).
`vdrm_bo_wait` (the winsys's wait for a BO it did not fence itself) waits for everything submitted on every ring:
the ABI names no BOs in a submission.

**amdgpu winsys and screen (0004, 0005).** The amdgpu winsys on Darwin compares two GPU descriptors as equal only when
they are the same descriptor (Darwin cannot compare file descriptions; it logged a warning for each screen).
`oneios_radeonsi_screen_create` opens the GPU descriptor, parses Gallium's and radeonsi's option tables (as the pipe
loader merges them), calls `radeonsi_screen_create` (the virtio path) and closes its descriptor (the winsys keeps a
duplicate); NULL with the reason in `why`, from `vdrm_darwin_last_error()` when the connection failed. On Linux the
same file opens the first `virtio_gpu` render node instead (for the host build). The library's sources and exported
names are lists in `src/gallium/targets/oneios-radeonsi/meson.build` (`oneios_radeonsi_sources`,
`oneios_radeonsi_exports`) that the Metal layer's 5xxx patches may extend; `build.sh mesa` then runs
`userland/metal-amd/air/build.sh MESA_SOURCE BUILD_DIR` when it exists.

**`oneios-amdgpu-probe`** (0005, `oneios_amdgpu_probe.c`), on the A56 from a root shell, prints one `PASS` or `FAIL`
line a step and exits with the number of FAILs. The steps are the other A56 fork's guest probe and host self-test, over
Mesa's own amdgpu virtio context (`amdvgpu_*`) and the Darwin backend: the device, the capset's facts (marketing name,
family, device id, revisions, GFX IP version and rings, VA range, CUs), a 4 KiB GTT guest BO and its CPU view, its GPU
address (BO_VA_OP), a context, eight GFX10+ type-2 NOPs (0x80000000, the dwords the fork's A56 self-test ran) submitted
with an out-syncobj, the syncobj waited (WAIT_FENCE) and the host's CS_QUERY_FENCE_STATUS, the reset state. With
`--screen`: `oneios_radeonsi_screen_create`, the screen's name and caps, a context, a 64 KiB buffer cleared with
`clear_buffer` and a 64x64 RGBA8 texture with `clear_texture`, each flushed, fenced and read back word for word.

**Host tests** (`userland/metal-amd/host-test.sh`; `METAL_AMD=1 runtime/host-tests.sh` runs it as its `metal-amd`
part), on the build laptop, 2026-11-24: `patches` pass (0001-0006 apply in order to a fresh clone of cecc0e9); `linux`
pass (the same Mesa builds for Linux with radeonsi, the amdgpu virtio context and ACO alone; `liboneios_radeonsi.so`
exports `oneios_radeonsi_screen_create` and nothing else; the probe links); `vdrm` pass, 51 of 51 checks of
`tests/vdrm_darwin_test.c`: `vdrm_darwin.c` and Mesa's `amdgpu_virtio*.c` compiled for Linux with `HAVE_VDRM_DARWIN`,
the IOKit calls a fake of the kernel's user client (`tests/fake-iokit`) that plays a minimal amdgpu host for the
ccmds (QUERY_INFO, CREATE_CTX, CS_SUBMIT, CS_QUERY_FENCE_STATUS). It checks the open type, the selector probe, the
capset and context records, the shmem blob, that a GTT BO is HOST3D_GUEST with CREATE_GUEST_HANDLE | MAPPABLE and its
GEM_NEW ccmd (same blob_id, alloc_size, heap) and a NO_CPU_ACCESS VRAM BO HOST3D_GUEST with CREATE_GUEST_HANDLE alone,
one MAP per BO, sync ccmds as fenced ring-0 EXECBUFFERs and their WAIT_FENCE, a GFX CS_SUBMIT fenced on ring 1, a
compute one on ring 2 whose in-syncobj on ring 1 is waited for first, syncobj waits (one WAIT_FENCE, then none;
-EINVAL; -ETIME with WAIT_FOR_SUBMIT; WAIT_ANY), CREATE_BLOB_USER, the closes. The connect line the test prints:
`vdrm-darwin: connected: IOAcceleratorES open type 0x4149, tunnel selector 5, ABI 1, capset 6 context type 2 (drm
3.61.0), context 16, shmem handle 1 (16 KiB, responses at 256), guest-allocated BOs`.

**Not done, exactly.** Nothing of it has run on an A56 or in iOS (there is none here; no qemu boot). Untested beyond
the fake: everything past the ccmd records (the host's answers, radeonsi's screen creation over a real capset, ACO's
shaders on the Xclipse, the clears). Paths that are stubbed or answer "unsupported" on Darwin: dma-buf import and
export, PRIME, sync files and syncobj descriptors (`fence_import_sync_file`, `fence_export_sync_file`, the
`export_signalled_sync_file` of the winsys fail), timeline syncobjs (off), placed BO maps, BO lists in EXECBUFFER
(amdgpu virtio sends none), the drm-ioctl (non-virtio) amdgpu paths, `drmGetCap` (no modifiers), the shader cache
(`-Dshader-cache=disabled`: every process compiles its shaders again), video (radeonsi's VCN code is linked but no
video frontend is built). `vdrm_darwin_bo_create_user` exists but the winsys has no import that calls it (IOSurfaces:
the Metal layer's). The binaries are not stripped and not placed in any image. WAIT_ANY over several syncobjs waits
for the first that has a fence (it returns when one is signalled, perhaps later than DRM would).

**The A56 lines that prove it** (once the kernel publishes the accelerator with `oneios-gpu-amd=1` and xnuhost serves
capset 6 with amdgpu): `oneios-amdgpu-probe` prints `PASS device: amdgpu virtio context, host amdgpu DRM 3.N`,
`PASS facts: "<the Xclipse's marketing name from the capset>" family N device 0x....`, `PASS bo: 4096 bytes,
resource N`, `PASS va: ...`, `PASS submit: GFX NOP IB of 8 dwords, host seqno 1`, `PASS fence: signalled N us after the
submit; host says seqno 1 expired`, `PASS reset: none`, then with `--screen` `PASS screen: "<radeonsi's name: the
marketing name and the LLVM-free ACO>" ...`, `PASS clear buffer: 65536 bytes of 0x5a0e1a56 read back`, `PASS clear
texture: 64x64 RGBA8 of 0xff0e1a56 read back`, and `PASS: 0 failed`. iOS's log (unified log, `phone pull-log`) gets
`vdrm-darwin: connected: IOAcceleratorES open type 0x4149, tunnel selector N, ABI 1, capset 6 context type 2 (drm 3.N),
context N, shmem handle N (16 KiB, responses at N), guest-allocated BOs` from every process that opens the GPU, and
`oneios-radeonsi: screen "..." (AMD, ...), N MiB of memory, compute 1` for every screen; a failure gives
`vdrm-darwin: <why>` or `oneios-radeonsi: radeonsi: no screen: <why>`. xnuhost's side of the same submit is section 2's.

**What the other stages must match:** the kernel's user client answers GETPARAM `CREATE_GUEST_HANDLE` = 1 (or nothing
connects), GET_CAPS of capset 6 with context type 2 and `virgl_renderer_capset_drm` as Mesa cecc0e9 lays it out
(`drm_hw.h`, with `ENABLE_DRM_AMDGPU`: the amdgpu union holds `amdgpu_buffer_size_alignments` and `amdgpu_gpu_info` of
libdrm 2.4.133), CONTEXT_INIT with 64 rings, the shmem's `rsp_mem_offset` set by the host, EXECBUFFER FENCE on any
ring below 64 (amdgpu virtio numbers ring 0 for ccmds and 1.. for the hardware rings in the order of `AMD_IP_*`), and a
service whose class matches `IOAcceleratorES`. The Metal layer's library compiles this target's sources, include directories and dependencies
(`oneios_radeonsi_sources`, `_incs`, `_deps`) into `libOneiOSRadeonAir.dylib` (5002, since the integration of
2026-11-24), so the bundle carries one library with radeonsi in it; `liboneios_radeonsi.dylib` stays a library of its
own for other users.

**RADV over the same backend (read, not built).** RADV's winsys initialises through the same `ac_drm_device_initialize(fd,
is_virtio)` and so the same amdgpu virtio context and vdrm backend; its physical device probe opens a render node by
path and takes the virtio path when `drmGetVersion` says `virtio_gpu` (`radv_physical_device.c`). What it would need:
(1) a device to enumerate: `vk_instance.c` lists devices with `drmGetDevices2`, which `oneios_drm.c` answers with none
(it could return one device whose render node is `/dev/null`, the GPU descriptor); (2) its syncobj type from the amdgpu
device's sync provider: `radv_amdgpu_winsys_query_info` calls `vk_drm_syncobj_get_type(fd)`, which makes a DRM provider
on the descriptor (ENOSYS here) where `vk_drm_syncobj_get_type_from_provider(ac_drm_device_get_sync_provider(dev))`
would take the Darwin one (binary; RADV emulates timelines then); (3) a WSI: Mesa's `VK_EXT_metal_surface` layer, as
Turnip's Darwin build uses. A guess, not tried: three small patches and the Vulkan runtime's build for Darwin would give
NativeMetal/metal2vulkan a RADV path later.

## 4. Guest: the AIR Metal layer and the A56's Metal plug-in

**What it is.** `userland/metal-amd`: Jesvi-iOS a13e406's Metal-on-Gallium layer, copied with its MIT notice
(`userland/metal-amd/NOTICE`: every file taken, what was changed, what was not taken) and moved onto the A56:

```
Metal (iOS 16.5) -> OneiOSRadeonMetalDevice (plugin/redwoodmetal.m, Objective-C, in OneiOSRadeonMetal.bundle)
   | dlopen of libOneiOSRadeonAir.dylib beside it; the rwa_* calls of air/air_metal.h (no Mesa header on this side)
libOneiOSRadeonAir: air_metal.c (pipelines, resources, draws and dispatches on one pipe_context)
   + the AIR frontend (air_bitcode, air_module, air_to_nir, air_intrinsics: Apple's shader bitcode -> NIR)
   + Mesa cecc0e9's Gallium; the screen: oneios_radeonsi_screen_create(why, n) (K3, the guest Mesa stage's, section 3)
```

- The screen (`air_metal.c` `rwa_device_open`): with `-Doneios-air-screen=radeonsi` the A56's radeonsi through K3;
  Jesvi's freedreno and Panfrost screens over its xf86drm stand-in are gone, and so is the plug-in's own GPU client
  (Jesvi opened its kernel's RedwoodGPU/RedwoodMali and made buffers there): every buffer, texture and fence is
  Mesa's, so the kernel services K3 names are reached by Mesa's Darwin vdrm only. Declared in
  `air/oneios_radeonsi_screen.h`; `build.sh MESA_SRC MESA_BUILD` sets the option when the tree defines the function.
- RDNA3 values, each commented where it is: `threadExecutionWidth` is the smallest size in radeonsi's
  `subgroup_sizes` mask (32: radeonsi compiles compute as wave32 on gfx10 and later); `maxThreadsPerThreadgroup`
  1024 (radeonsi's `max_threads_per_block`); linear and IOSurface row alignments 256 (Mesa `ac_surface.c`
  `ac_surface_get_pitch_align`, gfx9 to gfx11; Jesvi answered Panfrost's 64); threadgroup memory stays 16 KiB (the
  feature profile's table; RDNA3 has 64); the device's name is Mesa's renderer string with "(OneiOS)".
- Mesa 25.0.7 (Jesvi's) to cecc0e9 (26.3-devel): the API changes listed in the NOTICE; one reader fix (LLVM 22's
  `MODULE_CODE_GUIDLIST` record is skipped). Mesa wiring: `patches/mesa/5001-air-frontend.patch` (the guarded
  `subdir('air')`) and `5002-air-metal-library.patch` (`src/gallium/targets/air-metal`: `libOneiOSRadeonAir` and
  `air_metal_test`, read after every driver; options `oneios-air`, which also counts as a graphics and compute build
  (`with_gfx_compute`: without OpenGL or Vulkan Mesa builds NIR as headers only, and radeonsi's gfx part needs it
  too), and `oneios-air-screen`). Both apply to cecc0e9 with `git apply --check`.

**Built and tested (build laptop, 2026-11-24; [building.md](building.md) section 9).** `host-test.sh`: Mesa cecc0e9
with the 5xxx patches, softpipe, `air_metal_test` driving the library through `air_metal.h` alone. 8 of 8 pass:
the frontend (`phiconst.ll`: the edge constant survives the phi), a compute dispatch (`tid_store.ll`, ours: 16
threads write `tid * 3 + 5`; width 32), a render pipeline (`fragpos.ll` into RGBA32Float: pixel (0,0) is
`0.5 0.5 0 1`, Metal's top-left origin), argument buffers (`argbuf_set.ll`/`argbuf_direct.ll`: 0 of 20 checks
failed: members bound at their own indices, offsets, five wrong encodings refused by name), the IOSurface refusals,
and the open without a GPU screen. Inputs: four hand-written `.ll` of Jesvi's and one of ours, assembled with
llvm-as; no Apple metallib. Darwin: `build.sh mesa-ios` (the same Mesa for arm64-apple-ios16.5, softpipe only) and
`build.sh plugin`: `OneiOSRadeonMetal` and `libOneiOSRadeonAir.dylib` PASS `oneios-tool ios macho-check --arch arm64`
against `out/ios16-tbd` (171 and 149 symbols bound; Metal 306.6.4, iOS 16.5's), signed with 4 KiB pages; the
gpuaddon is 5 files. The `radeonsi` branch of `air_metal.c` compiles (host compiler; its one undefined symbol is
`oneios_radeonsi_screen_create`). **Not tested:** anything on radeonsi or ACO (the host test is softpipe's), the
bundle inside iOS, the guest Mesa stage's library (not built yet).

**Metal, area by area** (what the code does; on the A56 nothing has run):

| Area | State |
|---|---|
| Discovery, device | `-initWithAcceleratorPort:` makes the device, opens the screen; feature profile 1 (Apple1, Apple2, Common1: the lowest MPS accepts); `-name`, limits, `registryID` |
| Buffers | Mesa buffers, CPU-visible (shared storage); `newBufferWithBytesNoCopy:` as a mirror copied in and back; `gpuAddress` 0; no buffer without a screen |
| Textures | 42 pixel formats, 10 texture types, views, mipmaps, readback; multisample textures but **no resolve** (`supportsSampleCount:` answers 1 only); a texture over a buffer needs the driver's handle export (`resource_get_handle`, an FD): **not known to work on radeonsi over vdrm** |
| IOSurface textures (CAMetalLayer drawables, the compositor's surfaces) | **as copies**: a texture of ours filled from the surface when its seed moves, and written back after the GPU when it was a render target (not after a compute or blit write). Jesvi imported in place through its kernel (kRedwoodSurface); the A56 has no such import. A 720x1560 BGRA drawable is 4.5 MB each way a frame (a cost, not measured) |
| Compute | AIR bitcode -> NIR -> the driver; `dispatchThreadgroups`, `dispatchThreads`, threadgroup memory, argument buffers |
| Render | vertex + fragment (a generated vertex stage when none is named), vertex descriptors, blending, depth/stencil, indexed and non-indexed draws, per-attachment load and store actions, viewport depth range |
| Blit | buffer copy and fill, texture copy, mipmap generation |
| Argument buffers | a recorder: `MTLArgumentEncoder` records what is set and the draw binds it; a buffer written by the CPU or a kernel instead is refused by name |
| Not implemented (nil or a refusal by name) | heaps, fences, indirect command buffers, function tables, acceleration structures, mesh, tessellation, tile shading, stage-in, sparse textures, `newBufferWithIOSurface:`, shared textures, resource groups, counters; events and libraries are `_MTLDevice`'s own (not measured) |
| GL-on-Metal (iOS's OpenGL ES) | not served: Apple's GL hands Metal shaders as GLIR, which this layer does not read (Jesvi: such games stay black) |

**backboardd, and the UI's frame rate (decided).** The plug-in gives backboardd **no device** by default, so the
A56's UI is composited as today (gpu=2d, the CPU): **this stage does not raise the A56's UI frame rate.** Why:
Jesvi's own record (`docs/devices/x1slte/display.md`, 2026-11-14): CoreAnimation's render server on this Metal
"crashed it at start (stack overflow, pc 0x205aa4a8c): reverted", and the cause was not found; Jesvi's smooth UI is
CoreAnimation's **OpenGL ES** renderer (`CA::OGL::GLESContext`) on Mesa through its own EAGL (`userland/src/eagl.c`,
`inject.c`), never Metal; Jesvi's kernel hides the accelerator's discovery properties from "incompatible core
renderers". Nothing in the code shows the crash gone, and the drawables above are CPU copies, which the render
server would pay for every surface. How: `rm_offered` in `redwoodmetal.m` returns nil from `-initWithAcceleratorPort:`
in backboardd, mediaserverd, PosterBoard, AegirPoster and WeatherPoster (Jesvi's metaloff list) -- nil is "no
device", the road NativeAdrenoMetal takes in every process it does not serve; `OneiOSRadeonMetal.clients` beside the
bundle overrides the list ([userland.md](userland.md) 7.2); `oneios-radeon-backboardd=1` hands backboardd the device
for a test boot. The ways to a faster A56 UI, guesses until measured: CoreAnimation's GL renderer on Mesa (Jesvi's
road: an EAGL over Mesa's GL frontend on radeonsi; built since, section 4a: OneiOSRadeonGL) or backboardd on this Metal once the crash is
understood and IOSurfaces import in place (a Darwin vdrm import of the surface's wired pages, sections 3 and 5).

**The log lines that prove it** (the A56 has not run them). In `xnuhost.log` (`oneios-tool phone log`), from the
kernel's text rows (hosted 0039; 63 characters):

```
oneios-gpu-text: backboardd[NN] RM I DEV declined: backboardd is one of the processes kept off this Metal...
oneios-gpu-text: SomeApp[NN] RM I DEV offered: every process but backboardd and Jesvi's metaloff list...
oneios-gpu-text: SomeApp[NN] RM I DEV OneiOSRadeonMetal in SomeApp[NN], accelerator 0x...
oneios-gpu-text: SomeApp[NN] RM I DEV compiler library from /System/Library/Extensions/OneiOSRadeonMetal.bundle/...
oneios-gpu-text: SomeApp[NN] RM I DEV screen AMD Radeon ... (radeonsi, ...)      <- the GPU is this process's
oneios-gpu-text: SomeApp[NN] RM E DEV screen no radeonsi screen: <the guest Mesa stage's reason>   <- or not
oneios-gpu-text: SomeApp[NN] RM I PSO compute pipeline made: ... / pipeline 1: ...  <- the first pipeline compiled
```

The same records whole are in iOS's log (os_log subsystem `com.oneios.radeonmetal`, `oneios-tool phone pull-log`).
Before any of them: the kernel's `oneios-gpu-plugin` / `oneios-gpu-open` lines of the plug-in's load (section 5).
The renderer string is radeonsi's own (shape only here: `AMD Radeon ...`; a guess until the A56 prints it).

**Not done.** The bundle is placed by no tool (the image hook knows `NativeAdrenoMetal.bundle`; the kernel's addon
mount looks for that name: section 5). No in-place IOSurface import. Since the integration of 2026-11-24 (in main) the bundle is
built from the guest Mesa stage's tree: `userland/metal-amd/build.sh mesa` (0001-0006, then the hook: 5001-5002,
`-Doneios-air-screen=radeonsi`) and `userland/metal-amd/air/build.sh plugin out/metal-amd/build-ios` give
`libOneiOSRadeonAir.dylib` of 16,912,464 bytes with radeonsi, ACO and the Darwin vdrm backend in it; both Mach-Os
`macho-check` PASS against the 20F66 stubs (175 and 206 imports bound). Not run in iOS. Panfrost's counters in
`air_metal.c` (`rwa_gpu_cache_stats`) compile out and answer -1.

**What the other stages must match.** Guest Mesa (section 3): define `struct pipe_screen
*oneios_radeonsi_screen_create(char *why, size_t why_size)` (`air/oneios_radeonsi_screen.h`) in a file under
`src/gallium` or `src/amd` reached through `driver_radeonsi` (5002 links `driver_radeonsi`), configure with
`-Doneios-air=true` (or OpenGL), and call `userland/metal-amd/air/build.sh MESA_SRC MESA_BUILD` after its Mesa step;
the screen is opened once per Metal device and destroyed with `screen->destroy`. Kernel (section 5): the accelerator's
class name starts with `AGX` (hosted 0031/0039 log the plug-in's opens and text rows only then) and answers opens of
types `0x4147xxxx` with an error; `MetalPluginName`/`MetalPluginClassName` per K3; the addon mount (0031) and its
plug-in log (`oneios-gpu-plugin`, which matches `NativeAdrenoMetal` and `libvulkan_freedreno` by name) need the A56's
names (`OneiOSRadeonMetal`, `libOneiOSRadeonAir`) and 20F66's sealed driver bundle.

## 4a. OpenGL ES for CoreAnimation: OneiOSRadeonGL

Built and host-tested (2026-11-25, branch `t3/a56-radeonsi-gl-path`), not booted. The A56's low UI frame rate is
backboardd's CPU composition. The Metal layer above leaves backboardd alone (Jesvi-iOS's CoreAnimation render server
on that Metal crashed at start). The iOS 16 ports that composite their UI on a GPU through Mesa do it with
CoreAnimation's **OpenGL ES** renderer: Jesvi-iOS on the S20, iOSPlus10's NegroniGL (the owner's port) and ios-spes
(about 20 frames a second, NegroniGL's notes). This stage is that road on the A56's radeonsi.

**How backboardd picks its renderer on 20F66.** Read in `out/ios16-cache` with `out/ipsw-cli/ipsw` (`dyld symaddr`,
`dyld disass --quiet`, `dyld a2s`, `dyld dump`; unslid addresses):
- `CA::WindowServer::EAGLServer::renderer()` (0x1882e2548) tries Metal first: `CAMetalContextCreate()` (0x18837b718, a
  `dispatch_once` around `MTLCreateSystemDefaultDevice`), then `CA::OGL::new_metal_context` (0x18847efcc).
- With no Metal device it takes the GL road, but only while the byte `CA::WindowServer::enable_ogl` (0x1dcf3c1b9) is
  set. The EAGLServer constructor sets it once (0x1883d1c6c..0x1883d1c98) from `getenv("CA_ENABLE_OGL")`: unset gives 1,
  a value gives `atoi(value) != 0`.
- The GL road: `[[EAGLContext alloc] initWithAPI:3 properties:]` (then API 2 if that fails), `setParameter:0x260 to:0`,
  and `-API` must answer 3 (`cmp w0, #3` at 0x1882e272c). Then `CA::OGL::GLESContext(ctx)` (0x188400a18) and a renderer.
- No context, or not API 3: no renderer, and CoreAnimation draws on the CPU. That is the A56 today.

So no `CA_*` switch is needed (GL is on by default; `CA_ENABLE_OGL=0` would turn it off). The switch is discovery:
- backboardd must find no Metal device. The kernel keeps the Metal keys hidden from it, as before.
- The accelerator's `IOGLESBundleName` must name a GL driver that answers EAGL.

Jesvi-iOS reads the same addresses on the same cache (its `docs/ru/metal.md`, `userland/src/inject.c`).

**What it is.** `userland/metal-amd/gl`: NegroniGL (iOSPlus10 `userland/negronigl`, 146e889, MIT) ported to 20F66 and
to the A56. Every file taken and every change is listed in `userland/metal-amd/gl/NOTICE`.

```
backboardd's CoreAnimation (CA::OGL::GLESContext), CoreImage's GL context, GL applications
  | Apple's OpenGLES.framework: -[EAGLSharegroup initWithAPI:] -> eagl_init -> GLEngine's gliInitializeLibrary
  | -> libGFXShared's gfxInitializeLibrary: the accelerator's IOGLESBundleName = OneiOSRadeonGL
  | -> dlopen /System/Library/Extensions/OneiOSRadeonGL.bundle/OneiOSRadeonGL, gldInitializeLibrary
OneiOSRadeonGL: the gate; ngl_gli_register: OpenGLES's _gli_plugin[0] = this bundle, so eagl_init binds our 18 gli*
  | gliCreateContextWithShared fills EAGL's two 1036-slot dispatch tables; every glFoo() of OpenGLES and every call
  | CoreAnimation makes through -getMacroContextPrivate is disp[slot](rctx, ...) -> Mesa's GLES (st/mesa)
Mesa cecc0e9 Gallium: radeonsi through oneios_radeonsi_screen_create (K3, compiled into the bundle), or softpipe
```

| File | What |
|---|---|
| `gl_core.c` | the screen, the GLES contexts, the self tests, the gate, the probe |
| `gl_hand.c` | the slots written by hand (below), and the draw preparation |
| `surface.c` | IOSurfaces as texture and renderbuffer storage |
| `view.c` | CAEAGLLayer drawables and the present; the flush; the frame and present lines |
| `shaders.c` | NegroniGL's shader rewrites (rectangle samplers, framebuffer fetch, highp) |
| `sw_winsys.c` | softpipe's winsys: IOSurfaces drawn in place |
| `log.c` | os_log `com.oneios.radeongl`, and a few records as the kernel's text rows |
| `gen-gli.sh` | the dispatch table from `gli-slots.txt` and Mesa's generated `dispatch.h`; POSIX sh + awk, replacing NegroniGL's `gen-gli.py` |
| `plugin/gen-gld.sh` | the 129 GLD names' stubs and the export list; replacing `gen-gld.py` |
| `plugin/` | `gld.c` (libGFXShared's side), `gli_table.c` (the 20F66 register), `gli_context.c`, `gli_view.c`, `Info.plist` |
| `meson.build`, `patches/mesa/5101-oneios-gl-target.patch` | the Mesa target `src/gallium/targets/oneios-gl`. The hook sits after the DRI target, so it applies on the pin and after the guest Mesa stage's 0005 and the Metal layer's 5002 |
| `host/`, `tests/oneios_gl_test.c` | the host test's stand-in IOSurfaces and its driver |
| `tests/oneios_gl_probe.c` | `oneios-gl-probe`, the lasting guest tool |

**The 20F66 facts it relies on, checked.** `gli_table.c` writes `_gli_plugin[0]` only when all five code words below
are 20F66's (else it logs `OpenGLES is not 20F66's (words ...)` and writes nothing).

| Symbol | 20F66 | Code words checked |
|---|---|---|
| `EAGLGetVersion` | 0x1d315a878 | b4000060 52800028 |
| `eagl_init` + 0x134 | 0x1d315a578 | f902d2e0 (`str x0, [x23, #0x5a0]`: `_gli_plugin[0]` = GLEngine's handle) |
| `eagl_init` + 0x200 | 0x1d315a644 | 911682b5 f94002a0 (`add x21, x21, #0x5a0; ldr x0, [x21]`: the dlsym loop, x20 8 to 0x98) |
| `_gli_plugin` | 0x1e27715a0 | (none) |
| `_eagl_initialized` | 0x1e2771644 | (none) |

- The 18 names of `_eagl_gli_names` (0x1d315b350, 0x20 bytes each) are NegroniGL's list, in the same order.
- libGFXShared's `_gfx_gld_names` (0x1dab679f0) has the same 129 names in the same order as `plugin/gld-names.txt`
  (compared with `diff`).
- OpenGLES's code is NegroniGL's 20H392 code at the same page offsets. For example
  `-[EAGLContext commonInitWithAPI:properties:]` is at 0x1d31593f0 on 20F66 and at 0x1d3f513f0 on 20H392. The same
  holds for `renderbufferStorage:fromDrawable:`, `presentRenderbuffer:`, `texImageIOSurface:...invert:` and
  `getMacroContextPrivate`.
- `-attachImage:toCoreSurface:invertedRender:` (0x1d3159c60) maps the surface's format and calls
  `texImageIOSurface:...invert:`, which reaches us as `gliSetInteger(0x38e)` then `(0x399)`.
- `gli-slots.txt` (409 named slots) is HackiOS's 20F66 table. Jesvi-iOS's `eagl_slots.txt` (also 20F66) has the same
  409 slots.

**IOSurfaces.** The A56's Darwin vdrm imports no IOSurface pages (section 3: every BO is Mesa's own). So on radeonsi
each IOSurface CoreAnimation binds (the display's swap surfaces, the applications' layers) gets a resource of ours
(`struct ngl_surf`, one per surface per process):
- it is filled from the surface when the surface's seed has moved, at the bind and before each draw that samples it;
- it is written back into the surface at the first flush after a draw or clear into it (`glFlush`, `glFinish`, the
  APPLE waits, a present). The write back maps the resource for reading, which waits for the GPU, so the surface is
  whole when CoreAnimation swaps it;
- an inverted surface (`invertedRender:YES`, 0x399 with invert) has its rows turned over both ways, as Jesvi-iOS does;
- unreferenced copies not bound for 600 flushes (or past 128) are let go.

This is the Metal layer's IOSurface road too. Its cost is the bytes: a 720x1560 BGRA frame is 4.5 MB written back per
frame, plus an upload of every layer that changed. Whether that beats CoreAnimation's own CPU renderer on the A56 is
**not known** (a guess until measured: the GPU does the blending, blurs and filters; the copies are memcpy-speed, and
UMA). softpipe draws into IOSurfaces in place. `ONEIOS_GL_SURFACE=copy` takes the copied road there too, which is how
the host test checks it.

**What CoreAnimation needs of Mesa's GLES, carried over:**
- from NegroniGL: Apple's 2D and rectangle binding per unit (rectangle textures as 2D ones, coordinates scaled by a
  uniform), BGRA with `GL_RGBA` as internal format, framebuffer fetch emulated where Mesa lacks it, a texture barrier
  when a draw samples its own target, `lowp`/`mediump` defined to `highp`, shader failures logged with their source;
- new here, a rectangle texture's own defaults (`GL_LINEAR`, `GL_CLAMP_TO_EDGE`). NegroniGL left it with 2D's
  mipmapped default, which made a one-level texture incomplete, and it sampled black. The host test found it;
- from Jesvi-iOS: the draw CoreAnimation makes with no program and a texture on unit 0 (the lock screen's wallpaper
  and clock) gets a plain program of ours, with CA's arrays and its last matrix;
- GL_APPLE_fence on flush and finish, so that a test never spins.

Not ported from Jesvi-iOS's 4,223-line `eagl.c`:
- the home indicator's lookup table;
- the vibrant and masking colour matrices;
- the shadow blur and the 8x downsample;
- the variable blur;
- the census and the trace switches.
CoreAnimation asks for those through its private context (function numbers Jesvi reads at `ca_context + 0x10`).
Where they are missing, those effects draw nothing or draw plain [a guess from Jesvi's notes; not seen here].

**The gate** (`ngl_gate`, once per process, in `gldInitializeLibrary`). A closed gate leaves OpenGLES as without us:
GLEngine's table, our `gldChoosePixelFormat` refuses, EAGLContexts are nil.
- `ONEIOS_GL=1|0` in the environment wins.
- `OneiOSRadeonGL.bundle/off` exists, or cannot be checked: closed.
- `OneiOSRadeonGL.bundle/OneiOSRadeonGL.clients`, when it is there: `*`, `NAME` or `-NAME`; not listed: closed. No
  such file: every process.
- A screen. radeonsi first. softpipe stands in, when radeonsi does not open, in every process but backboardd:
  - why: CoreImage has no CPU renderer on iOS 16 and dies on a nil EAGLContext (docs/gpu.md 5.2), and a GL
    application has nothing else;
  - backboardd keeps CoreAnimation's own CPU renderer, faster than CoreAnimation on softpipe;
  - the boot argument `oneios-gl-softpipe=1` gives backboardd softpipe too (a test boot of the EAGL road before radeonsi
    works); `=0` gives no process softpipe.
- In backboardd (or with `ONEIOS_GL_PREFLIGHT=1`): the triangle and IOSurface self tests must pass first. This is
  NegroniGL's and ios-spes's preflight.

**Reaching backboardd (kernel, xnuhost, placement).**
- Kernel (section 5): `oneios-gpu-amd-gl=1` makes the A56 accelerator's `IOGLESBundleName` `OneiOSRadeonGL` for every
  process. Without it the name is `AppleMetalGLRenderer`, which our Metal layer cannot serve (it reads no GLIR). With
  it, the `oneios-gpu-amd-cpu` processes (backboardd, SpringBoard, ...) still have the four Metal keys hidden but see
  `IOGLESBundleName`. Without `oneios-gpu-amd=1` nothing changes.
- xnuhost: when it picks the amdgpu renderer (section 2) and the same gpuaddon carries
  `addons/OneiOSRadeonGL.bundle/OneiOSRadeonGL` with a matching `oneios-target`, it adds `oneios-gpu-amd-gl=1`. Then it
  leaves out the CoreImage text fixup of docs/gpu.md 5.2, because our bundle answers CoreImage's EAGLContext. The fixup
  goes only when xnuhost found the bundle itself **and** the boot arguments' first `oneios-gpu-amd-gl=` word is `=1`
  (`config::ios16_gl_driver`). A hand-written `=1` without the bundle keeps the fixup. `boot-args=...
  oneios-gpu-amd-gl=0` in the image's `xnuhost.conf` turns GL off and keeps the fixup.
- Placement: `oneios-tool ios gpuaddon METAL_BUNDLE OUT --gl GL_BUNDLE` (and `ios addon --gl`) puts the bundle at
  `addons/OneiOSRadeonGL.bundle/...` beside the Metal plug-in. It gets no `.publish` marker (that is the Metal
  plug-in's). Hosted 0031's union shows `addons/` as `/System/Library/Extensions`, and 0031/0108's dyld lines already
  name `OneiOSRadeon*` files. `userland/metal-amd/air/build.sh addon` (run by `plugin`) adds it when
  `$METAL_AMD_OUT/gl/OneiOSRadeonGL.bundle` exists.

**Built (build laptop, 2026-11-25).** [building.md](building.md) section 9 has the commands.
- `userland/metal-amd/gl/build.sh host`: Mesa cecc0e9 + 5101, GLES, softpipe; `oneios_gl_test`.
- `build.sh mesa-ios` + `plugin`: the softpipe-only bundle for arm64-apple-ios16.5.
  - `OneiOSRadeonGL`: MH_BUNDLE arm64, minos 16.5, 14,093,408 bytes, chained fixups, no `dyld_stub_binder`;
  - exactly its 151 exports (129 GLD, 2, 18 GLI, `gliTerminateLibrary`, `oneios_gl_probe`);
  - 220 imports, all bound by `oneios-tool ios macho-check --arch arm64` against `out/ios16-tbd`: PASS.
- **The A56 bundle with radeonsi**: `userland/metal-amd/build.sh mesa` with the new hook, in a scratch
  `METAL_AMD_OUT` (the guest Mesa stage's 0001-0006 and the Metal layer's 5001-5002, then
  `userland/metal-amd/gl/build.sh MESA_SRC MESA_BUILD`).
  - The hook turns on `-Dgles2=enabled` and adds softpipe to `-Dgallium-drivers` (radeonsi,softpipe: the stand-in
    above);
  - `OneiOSRadeonGL`: MH_BUNDLE arm64, minos 16.5, 20,197,168 bytes, radeonsi, ACO, the vdrm backend and softpipe in
    it, 265 imports all bound: PASS;
  - `oneios-gl-probe` 50,368 bytes: PASS;
  - `air/build.sh plugin` on the same tree: `OneiOSRadeonMetal.gpuaddon` with both bundles, 8 files, 8,819,328 bytes.

**Tested on the host** (`userland/metal-amd/gl/host-test.sh`, `METAL_AMD=1 runtime/host-tests.sh`).
`tests/oneios_gl_test` drives the GL layer as iOS does. It uses EAGL's GLI entries, and every GL call goes through the
1036-slot table as CoreAnimation calls it (`disp[slot](rctx, ...)`), over stand-in IOSurfaces. **22 of 22 pass with the
surfaces in place, 23 of 23 copied, and the no-screen gate**:
- the 20F66 code words match, and 20H392's are refused;
- the gate (files, clients list, `ONEIOS_GL`, the preflight);
- EAGL's start (version, pixel format flags 0x225dd, both tables filled);
- a render target: clear and triangle, read in the surface's memory after `glFlush`;
- an inverted target: its rows turned over;
- a sampled surface through a rectangle sampler in pixels, and its next frame after the "app" rewrote it (the seed);
- a draw with no program;
- a CAEAGLLayer drawable bound (`gliBindViewES` twice) and presented twice into two drawables;
- the self tests and the probe;
- the context's end with every IOSurface lock released.
`ONEIOS_GL_ERRORS=1` shows no GL error on any slot. 63 of the 409 named slots have no Mesa function (ES 1 fixed-point
and fixed-function calls, the debug markers, `glFramebufferParameteriAPPLE`, `glResolveMultisampleFramebufferAPPLE`,
`glCopyTextureLevelsAPPLE`); they log once and return 0.

**Not tested:**
- anything on radeonsi or in iOS;
- that CoreAnimation's 20F66 EAGLServer accepts our context (API 3: it should; not run);
- the CAEAGLLayer native window's layout on 20F66 (NegroniGL read it on 20H392's QuartzCore; QuartzCore was not
  compared here).

**Would backboardd composite on the GPU** with this stage and the guest Mesa radeonsi (section 3)? As written, yes, if
all of these hold on the A56:
1. radeonsi opens in backboardd: the K3 screen, the vdrm open type 0x4149, the host's amdgpu context (section 2);
2. the preflight passes;
3. CoreAnimation's shaders compile on Mesa;
4. the copies keep up.

What is still missing for it:
- a boot (no A56 here);
- the A56 release kernel built with this GPU archive change (on top of release-kernel-a56 0888d97: not built here);
- an image whose kernel's gpuaddon carries both bundles (the next release's staging);
- an in-place IOSurface import in the Darwin vdrm (the copies' cost);
- Jesvi-iOS's other CoreAnimation fixes, when the A56 shows which effects are missing.

**The A56 lines that would prove it.**

In `xnuhost.log` (`oneios-tool phone log`):
```
gpu: 3D on amdgpu (/dev/dri/renderD128): the gpuaddon's OneiOSRadeonMetal plug-in (iPhone10,3 20F66) and the self-test passed; oneios-gpu-amd=1
gpu: OpenGL ES: the gpuaddon's OneiOSRadeonGL (iPhone10,3 20F66, 3 files); oneios-gpu-amd-gl=1: CoreAnimation composites on it
ios16: CoreImage's GL fallback fixup left out: oneios-gpu-amd-gl=1, OneiOSRadeonGL answers its EAGLContext
oneios-gpu-amd: the A56 accelerator: ... IOGLESBundleName OneiOSRadeonGL; kept off Metal: backboardd,SpringBoard,... (on OpenGL ES, oneios-gpu-amd-gl=1)
oneios-gpu-amd: backboardd[N] kept off the Metal plug-in: MetalPluginName hidden (oneios-gpu-amd-cpu); OpenGL ES OneiOSRadeonGL seen (oneios-gpu-amd-gl=1): GPU composition
oneios-gpu-text: backboardd[N] RG I GATE OneiOSRadeonGL.bundle in backboardd[N] at 0x... (service 0x...
oneios-gpu-text: backboardd[N] RG I SCR screen AMD Radeon ... (...)                 <- radeonsi's renderer string (shape a guess)
oneios-gpu-text: backboardd[N] RG I GATE preflight PASS: screen AMD Radeon ..., triangle PASS, IOSurface PASS, N ms
oneios-gpu-text: backboardd[N] RG I GATE open for backboardd: screen AMD Radeon ... (IOSurfaces copied), 63 GLI slo
oneios-gpu-text: backboardd[N] RG I GLI GLI table installed in OpenGLES's _gli_plugin (20F66, slide 0x...)
oneios-gpu-text: backboardd[N] RG I CTX GLES 3 context 0x... on AMD Radeon ...     <- CoreAnimation's EAGLContext
oneios-gpu-text: backboardd[N] RG I GLI first frame drawn on AMD Radeon ...: 1 copied surfaces written back
oneios-gpu-amd: backboardd[N] DRM context 16 on capset 6 (the host's context type 2, AMDGPU)
display: 10 s: ...                                                                  <- xnuhost's frame counts: the rate
```
Rows are cut at 63 characters (hosted 0039). The whole records are in iOS's log: subsystem `com.oneios.radeongl`,
`oneios-tool phone pull-log`. iOS's log also has, at the 600th and every 3600th frame,
`GLI 600 frames, N a second since the first; copies: S surfaces, U uploads (M MB), W write backs (M MB)`.

The failures say why: `RG E GATE closed for backboardd: <why>; EAGLContexts stay nil (CPU composition)`,
`RG E GLI GLI register: OpenGLES is not 20F66's ...`, `RG E SCR no radeonsi screen (<K3's reason>); softpipe not
allowed in this process ...`, `RG E SHDR shader N compile failed: ...`.

On the phone, `oneios-gl-probe` (from `$METAL_AMD_OUT/gl`, run over the phone's shell as root) prints
`oneios-gl-probe: PASS: OneiOSRadeonGL (Mesa ..., radeonsi ...); screen ...; triangle PASS; IOSurface PASS (copied)`.

**For the other stages.**
- Guest Mesa (section 3): build with `-Dgles2=enabled` and softpipe beside radeonsi (the hook does it), and keep
  `oneios_radeonsi_screen_create` in `targets/oneios-radeonsi/oneios_radeonsi.c`: the GL target compiles that file in.
  An in-place IOSurface import (a BO over a surface's wired pages) would remove the copies.
- Kernel: the GPU archive change of section 5.3 ("OpenGL ES") goes into the next A56 release kernel.

## 5. Guest kernel: the accelerator on an amdgpu host

Built and host-tested (2026-11-24, branch `t3/a56-amdgpu-kernel-support`), not booted: the GPU archive
(`kernel/patches/oneios-hosted/gpu/`: `OneiOSGpu.cpp`, `OneiOSAdreno.cpp`, the new `OneiOSGpuChoice.h`; the staged
edits `staged/gpu.rs` `GPU`, `GPU3D` and the new `ACCELERATOR` of `oneios-tool kernel gen gpu`) and hosted
`0108-a56-gpu-plugin.patch`.

### 5.1 What the A56 release kernel did on an AMDGPU capset (read from the code at 39ad96e)

The kernel itself never sends an msm ccmd or reads an msm field outside `vgpu-selftest=1`, and nothing in it waits on
the capset, so an amdgpu host caused no panic and no hang. The fault was the publish:

- `OneiOSGpu.cpp:53-60` set gpu state 3 from the virtio features alone (VIRGL, RESOURCE_BLOB, CONTEXT_INIT,
  CREATE_GUEST_HANDLE); the capset was never read.
- `OneiOSAdreno.cpp:109-114` (`oneios_adreno_wanted`): state 3 published NativeAdreno's accelerator unless
  `oneios-gpu-metal=0`. So on an amdgpu host:
  - a 20F66 image with roottweak's `oneios-gpu-metal=0`: nothing published. Safe.
  - without it (an image whose `xnuhost.conf` lacks it; iOS 27 on an A56; `oneios-gpu-metal=1`): `AGXAccelerator` with
    `MetalPluginName` `NativeAdrenoMetal` (`DroidVMGpu.cpp:284`) on a host that is not msm. A wrong publish. On iOS 16.5,
    stock Metal opens it as an IOGPU device (type 1, refused `0xe00002c7` at `NativeAdrenoPublish.cpp:715`), and
    backboardd gets a Metal device that cannot draw ([a56.md](a56.md), "Why 16.5 needed each piece"). On iOS 27,
    NativeAdrenoMetal's Turnip asks capset 6 for an msm context [guess, not run: its vdrm then finds context type 2 and
    makes no device].
  - No KGSL anywhere: NativeAdreno's hardware classes are never attached (`DroidVMGpu.cpp:176`, a 0x4147 open fails
    not ready), and `native_adreno_publish` (`NativeAdrenoPublish.cpp:782`) touches no GPU.
- The user client's virtgpu methods do not depend on the driver (`DroidVMVirtioGpu3D.cpp`): `getCaps` (435) passes
  the host's capset through, `contextInit` (450) sends CTX_CREATE with the caller's capset id, `createBlob` (647) sends
  the caller's ccmd first and then RESOURCE_CREATE_BLOB with its blob_mem (HOST3D, or HOST3D_GUEST over wired guest
  pages), flags, blob_id and size. That is what K2 needs.
- `vgpu-selftest=1`: the staged selftest already branches on the capset (`staged/gpu.rs` `GPU3D`, port-a56 8f889d0):
  AMDGPU sends `AMDGPU_CCMD_QUERY_INFO(ACCEL_WORKING)`, MSM sends `GET_PARAM(GPU_ID)`, and any other type stops at
  stage 3 (unsupported).

### 5.2 The fix: which accelerator, from the capset

`OneiOSGpu.cpp` now reads the DRM capset's `context_type` once at the start when the state is 3 (GET_CAPSET_INFO by
index, then GET_CAPSET 6: two read-only queries the host answers as it does for every Metal process's vdrm). It puts
the result in `oneios_gpu3d_context_type`: 1 MSM, 2 AMDGPU, 0 not read. `OneiOSAdreno.cpp` decides once, before
launchd, with `oneios_gpu_accelerator_choice` (`gpu/OneiOSGpuChoice.h`, a pure function):

| Host and boot arguments | Published |
|---|---|
| MSM capset (or none read), no `oneios-gpu-amd=1` | as before: the Adreno accelerator when the state is 3, or as `oneios-gpu-metal` / `droidvm-gpu-metal` says |
| capset read and not MSM (AMDGPU, or any other), no `oneios-gpu-amd=1` | **nothing**, whatever `oneios-gpu-metal` says |
| `oneios-gpu-amd=1`, 3D with an AMDGPU capset | **the A56's accelerator** (5.3), with or without `oneios-gpu-metal=0` |
| `oneios-gpu-amd=1`, anything else (no 3D, MSM, not read) | nothing: never the Adreno one, because the host said its GPU is an AMD one |

How `oneios-gpu-amd=1` (K5) combines with 20F66's `oneios-gpu-metal=0`: `oneios-gpu-metal` applies only to the Adreno
accelerator. That word was written against NativeAdreno's publish (stock Metal's type-1 open), and roottweak writes it
into every 20F66 image's `xnuhost.conf`. The A56 keeps its image, so if `=0` also stopped the A56's accelerator, the
image would have to be made again. To turn the A56's accelerator off: leave out `oneios-gpu-amd` (or give `=0`), or
use `gpu=2d` on the host (no 3D, state 2).

The S24 Ultra and iOS 27 stay the same: their host's capset is MSM and xnuhost never passes `oneios-gpu-amd`, so the
first row applies. It is the old `oneios_adreno_wanted` rule (`test_choice.c` checks this for every state and
`oneios-gpu-metal` value), and every hook below answers what the Adreno code did before. The only difference on the
S24 is the two capset queries at the start. No line is logged for an MSM capset.

### 5.3 The A56's accelerator

**The class.** It is the same `AGXAccelerator` (NativeAdreno's `NativeAdrenoAccelerator.cpp`, a subclass of the
collection's `IOAcceleratorES`, with its chain adopted as `AGXAccelerator -> IOGPU -> IOAcceleratorES`) and the same user
client (`AGXDeviceUserClient -> IOGPUDeviceUserClient`). Only one accelerator is published in a boot. Why not a
sibling class:
- Metal's discovery on 20F66 (Jesvi-iOS `userland/src/metal_props.c`, `metal_dev.m`: `IOServiceMatching("IOAcceleratorES")`,
  then `MetalPluginName` -> `/System/Library/Extensions/<name>.bundle`, then `MetalPluginClassName`, then
  `[[class alloc] initWithAcceleratorPort:service]`) needs only an `IOAcceleratorES` with the two keys.
- The sandbox's profiles name these classes and their IOGPU chain, so the grants already made for them work here.
- A new class would need its own binding to `IOAcceleratorES` (the same 24A446 collection on the A56) and would gain
  nothing.

Its properties:
- `MetalPluginName` `OneiOSRadeonMetal` and `MetalPluginClassName` `OneiOSRadeonMetalDevice` (K3);
- `IOGLESBundleName` `AppleMetalGLRenderer`, as before (OpenGL ES on the process's Metal device; Jesvi-iOS names the
  same one on 20F66);
- `NativeSandboxGrantProcess`: the client list, as before;
- `OneiOSDrmContext` `"amdgpu"`, new, for the plug-in and for ioreg.

`DroidVMGpu.cpp`'s literal names are now `oneios_gpu_metal_plugin()` and `oneios_gpu_metal_class()` (staged `GPU`).
For the Adreno accelerator these return the old strings.

**The open types** (staged `ACCELERATOR`, `oneios_gpu_open_refused`):
- Served: 0x4149, `DROIDVM_VIRTGPU_OPEN_TYPE`. Each connection is one DRM context, with the methods of
  `DroidVMVirtgpuABI.h` through `{'NTNL', 0x7200 + op}` (`NativeAdrenoPublish.cpp:699`, `droidvm_virtgpu_call`).
- Passed on: NativeAdreno's progress markers 0x4147xxxx (xxxx nonzero). They make no connection; they are the
  plug-in's text rows, `oneios-gpu-text`.
- Refused with `kIOReturnUnsupported`: every other type. That includes 0x4147 (NativeAdreno's KGSL-shaped GPU core),
  0x4148 (the display hand-off) and 1 (iOS's IOGPU device). The first 16 refusals of a boot are logged.

The `NativeAdrenoGpu` service is still published (`native_adreno_publish`; the user client serves only after it), and
its own 0x4147 opens still fail not ready. There is no Adreno self-test. `vgpu-selftest=1` and
`oneios-adreno-selftest=1` (an in-kernel 0x4149 open and `GETPARAM(ABI_VERSION)`) are generic virtgpu and stay
available.

**Which processes may open it (the sandbox).** These rules are unchanged from the Adreno accelerator:
- 0033: the grant (`NativeSandboxGrantProcess` on the service) decides both the service open and the user client's
  MACF check for the processes it names (`0033-gpu-grant-decides.patch` lines 15 and 27). The rest are the sandbox
  profiles'.
- The grant's list is `droidvm-gpu-clients`. Without it the default is `*`, so every process may open.
- A 0x4149 connection that MACF or the filter resolver refuses gets `NativeSandboxGrant` (the message filter).
- Not known: which 20F66 processes the iPhone X's own profiles let open it without the grant (not run).

**CPU composition kept: Jesvi-iOS's filter, in the kernel (`oneios-gpu-amd-cpu`).** For the processes on that list,
the A56 accelerator answers its five discovery keys as absent:
- the keys: `MetalPluginName`, `MetalPluginClassName`, `IOGLESBundleName`, `IOGLESMetalBundleName`,
  `IOGLESDefaultUseMetal`;
- where: `AGXAccelerator::copyProperty`, all three forms, which IOKit's single-property and search reads call
  (`IOCopyPropertyCompatible`), and a new `serializeProperties` override for the whole-table read
  (`IORegistryEntryCreateCFProperties`). Each answer is for the process that reads; the registry is not changed;
- the effect: those processes find no plug-in and no GL driver, and draw on the CPU as with no accelerator.

**OpenGL ES for those processes (`oneios-gpu-amd-gl=1`, the GL stage, 2026-11-25; section 4a).** The rule is
`OneiOSGpuChoice.h` `oneios_gpu_gles_bundle_for` and `oneios_gpu_cpu_hides`, checked by `test_choice.c`. With
`oneios-gpu-amd-gl=1` on the A56's accelerator:
- `IOGLESBundleName` is `OneiOSRadeonGL` (`oneios_gpu_gles_bundle()`, staged into `DroidVMGpu.cpp`'s `setProperty`)
  for every process;
- the CPU-composition processes still read the four Metal keys as absent (`MetalPluginName`, `MetalPluginClassName`,
  `IOGLESMetalBundleName`, `IOGLESDefaultUseMetal`) but see `IOGLESBundleName`. So backboardd finds no Metal device,
  and CoreAnimation takes its OpenGL ES renderer on the A56's GL;
- `serializeProperties` now removes each key by the same `oneios_gpu_discovery_hidden` as `copyProperty`.
Without it (not given or `=0`: xnuhost adds it only when the gpuaddon carries the GL bundle), the accelerator names
`AppleMetalGLRenderer` and all five keys stay hidden, as before. The Adreno accelerator always names
`AppleMetalGLRenderer` (`test_choice.c` checks it). **The A56 release kernel needs a rebuild** with this GPU archive,
on top of release-kernel-a56 0888d97 (not built here).

The default list is `backboardd`, `SpringBoard`, `mediaserverd`, `PosterBoard`, `CollectionsPoster`, `WeatherPoster`,
`SBRendererService`, `MobileSlideShow` (`ONEIOS_GPU_AMD_CPU_DEFAULT`). This is Jesvi-iOS's
`redwood_graphics_policy.h` list; its 15- and 16-character command aliases are matched by the name matcher. **So
backboardd keeps CPU composition by default**: CoreAnimation's render server on Jesvi's Metal crashed at start and was
reverted (section 1), and nothing here shows it working. Forms of the boot argument:
- `oneios-gpu-amd-cpu=a,b`: another list (`X*` matches a prefix, `*` every process);
- `oneios-gpu-amd-cpu=` (empty): no process keeps the CPU. This is the test of backboardd on this Metal.

The plug-in's own allow-list (the Metal stage) still applies beside this filter: 0108 writes the grant's list as
`OneiOSRadeonMetal.clients` beside the bundle, as 0039 writes `NativeAdrenoMetal.clients`.

**0108's plug-in hooks.** In `kern_exec.c`:
- 0039's list file is named after the accelerator's `MetalPluginName`.
- One line says whether `/System/Library/Extensions/OneiOSRadeonMetal.bundle/Info.plist` is visible through 0031's
  union. 0031's stage-5 check looks for `NativeAdrenoMetal.bundle` and the sealed `AGXMetalG18P.bundle`, which the
  iPhone X image does not have, so on 20F66 its line says stage 3: mounted.

In `kern_descrip.c`, 0031's dyld lines also name `OneiOSRadeon*` files.

### 5.4 Tests

- `kernel/tests/host.sh`: the new `gpu/tests/test_choice.c` checks the choice for every state, capset,
  `oneios-gpu-metal` and `oneios-gpu-amd`, against the old rule on MSM. It also checks the CPU list's matcher, the
  discovery keys and the open types. It runs with ASan and UBSan.
- `cargo test -p oneios-kernel --test golden`: the three edit lists on made-up sources (`synthetic/gpu`, `gpu3d`,
  `accelerator`). The new `the_gpu_edits_fit_the_pinned_sources` checks them on the real pinned `DroidVMGpu.cpp`,
  `DroidVMVirtioGpu3D.cpp` and `NativeAdrenoAccelerator.cpp`: each anchor there once.
- `HOSTED_OUT=a56gpu ONEIOS_CRYPTO=original kernel/build-hosted.sh` on the build laptop (main's series to 0108, 192
  patches): `xnu-12377.121.6-ios-27.0.1-24A446-b14aa6445129`, releasable, no warning in the GPU archive. Its prelink
  `kernel.kc` `f090804d...`: audit 0 failures. The same on `release-kernel-a56` (branch `a56gpu/release-kernel-a56`):
  `...-f8def7dad866`, prelink audit 0 failures, never uploaded: superseded by the release kernel
  `xnu-12377.121.6-ios-27.0.1-24A446-181d8bf2d60a` (`release-kernel-a56` 0888d97: 0108 and 0109 on 16bd44f9d737's
  branch; pinned in `kernel/release-kernel.txt`, docs/releases.md), the one A56 kernel with this stage in it. 0108 also applies on `release-kernel-a56`'s series
  (to 0092 and 0102-0107: `git apply` of all 183 patches in order on a scratch tree).
- **Not done:** any boot. There is no A56 here and no qemu boot (qemu has no 3D: none of these lines would show).
  The plug-in, Mesa and the host renderer are the other stages.

### 5.5 The lines that prove it on an A56

With the host's amdgpu renderer and `oneios-gpu-amd=1`:

```
oneios-gpu: virtio GPU with the DRM native context (features 0x...)
oneios-gpu: DRM capset (N bytes) context type 2 (AMDGPU): no Adreno accelerator; the A56's with oneios-gpu-amd=1 on AMDGPU
oneios-gpu-amd: the A56 accelerator: AGXAccelerator, MetalPluginName OneiOSRadeonMetal, MetalPluginClassName OneiOSRadeonMetalDevice, open type 0x4149 alone (DRM context type 2); IOGLESBundleName AppleMetalGLRenderer; kept off Metal: backboardd,SpringBoard,... (CPU composition)
droidvm-gpu: published for the virtio GPU
droidvm-gpu: Metal accelerator stage 5 status 0
oneios-gpu-addon: /System/Library/Extensions/OneiOSRadeonMetal.bundle/Info.plist (the accelerator's MetalPluginName): visible (0)
oneios-gpu-amd: backboardd[N] kept off the Metal plug-in: MetalPluginName hidden (oneios-gpu-amd-cpu); no GL either: CPU composition
oneios-gpu-plugin: NAME[N] code signature of OneiOSRadeonMetal: error 0
oneios-gpu-open: NAME[N] AGXAccelerator type 0x4149 -> 0x0, AGXDeviceUserClient
oneios-gpu-amd: NAME[N] DRM context 16 on capset 6 (the host's context type 2, AMDGPU)
```

The safe refusals:
- An amdgpu host without `oneios-gpu-amd`: the capset line, then `oneios-adreno: no Metal accelerator: the host's DRM
  context type is 2 (AMDGPU), not MSM; the A56's accelerator is published with oneios-gpu-amd=1 alone (gpu state 3)`.
- `oneios-gpu-amd=1` on a host with no 3D or an MSM capset: `oneios-gpu-amd: no Metal accelerator: oneios-gpu-amd=1,
  but gpu state S and DRM context type T ...`.
- The plug-in is not in the image: `... OneiOSRadeonMetal.bundle/Info.plist (the accelerator's MetalPluginName): not
  there: Metal finds no plug-in and every process draws on the CPU (2)`.
- Another open type: `oneios-gpu-amd: NAME[N] open type 0x1 refused: the A56 accelerator serves type 0x4149 (virtgpu)
  alone`.

**For the other stages:**
- The host: CTX_CREATE's `context_init` carries the capset id in bits 0-7 and OneiOS's hint (role, QoS) in bits 8-23.
  SUBMIT_3D's three padding bytes carry the hint too (`staged/gpu.rs`). The host must mask the capset id with 0xff and
  ignore the rest.
- The host: the context name is `xnu:<process>`.
- The plug-in: it opens the port `initWithAcceleratorPort:` gives it, with type 0x4149. It may check
  `OneiOSDrmContext`.
- Mesa: `GETPARAM(DEFAULT_3D)` answers the boot argument `vgpu-3d`.

## 6. Safety, switches, and what is left

- `gpu=2d` (the command line, the image's `overlay/userland/xnuhost.conf`, or the `galaxy-a56` row) always gives the
  2D path of today. The way back to One UI is not touched by any part of this port.
- The Metal plug-in (section 4) gives backboardd no device unless `oneios-radeon-backboardd=1` is in the boot
  arguments (a test boot only); `OneiOSRadeonMetal.clients` beside the bundle names the processes that get one;
  `oneios-metal-profile=N` changes the feature profile (default 1). A process without a device keeps today's road.
- OpenGL ES (section 4a): xnuhost adds `oneios-gpu-amd-gl=1` when the gpuaddon carries `OneiOSRadeonGL.bundle` **and the
  kernel reads that switch** (its kernel.kc names `oneios-gpu-amd-gl`: the A56 kernel `5966e71ce2aa` on; an older one, as
  the pinned `181d8bf2d60a`, gives `gpu: OpenGL ES: the kernel does not read oneios-gpu-amd-gl ...: Metal alone, CPU
  composition, the CoreImage fixup kept`, since on it SpringBoard would find no GL driver with the fixup left out);
  `boot-args=... oneios-gpu-amd-gl=0` in `xnuhost.conf` keeps the GL bundle unnamed (CPU composition, and the CoreImage
  fixup of docs/gpu.md 5.2 back). In iOS, `OneiOSRadeonGL.bundle/off` closes the gate in every process,
  `OneiOSRadeonGL.clients` names the processes, and `oneios-gl-softpipe=0|1` sets where softpipe stands in.

### What an A56 user does (2026-11-25): no computer, no new image

Everything below is built and host-tested; **nothing of it has run on an A56** (no A56 here, no qemu boot).

**What ships.** The release's runtime carries the two A56 bundles beside the Adreno plug-in (`lib/guest/`,
`runtime.json` `metal_a56`, runtime/package.sh 3c; release.sh builds them, or `--metal-amd-dir out/metal-amd`), and its
`bin/oneios-tool` has `image a56-gpu-place`. The app (OneOS fork, `oneios` with `99a7b0e`, "oneios-a56-gpu-place")
places them at Update the kernel.

**The taps** (the OneOS app, OneiOS page, on the A56 with the iOS 16.5 image it already has):
1. **Get the runtime** (the new release): stages it, with `lib/guest/OneiOS*.bundle` and the new `bin/oneios-tool`.
2. **Update the kernel**: builds kernel.kc on the phone from the release kernel the runtime names (for the GL half,
   `xnu-...-5966e71ce2aa`, pinned since 2026-11-25; an older runtime names `181d8bf2d60a`: the Metal half only), then **Activate**.
   The activation runs `bin/oneios-tool image a56-gpu-place` on the 20F66 set and moves in `image/kernel.kc.gpuaddon`,
   the merged `overlay/userland/owner-trust.json` and `manifest.json`, with `image.sha256` and `image.ids` rewritten;
   the page then says "The A56's GPU addon (Metal and OpenGL ES for iOS 16.5) is in place too: ...". An iOS 27 set
   never runs this step. Make iOS image (a new 20F66 image) places the same addon before its lists: owner-trust.json
   and its manifest entry before the `verify` step, `kernel.kc.gpuaddon` after it (the sidecar is not in the
   manifest, which `verify` refuses: issue #87, fixed 2026-11-28, `a56gpu::place_listed` then `write_sidecar`; unit
   test `make_image_places_the_sidecar_after_the_verify`; not yet booted on an A56).
3. **Switch to iOS 16.5**: xnuhost finds the addon, runs its amdgpu self-test, and turns the GPU path on.

**On and off.** On: the addon in the set and `gpu=auto` (the `galaxy-a56` row's default) with the self-test passing;
OpenGL ES for backboardd also needs the kernel to read `oneios-gpu-amd-gl` (`5966e71ce2aa` on). Off: `gpu=2d` in the
image's `overlay/userland/xnuhost.conf` (2D, laid out as before); `boot-args-add=oneios-gpu-amd-gl=0` keeps backboardd
on the CPU while apps keep Metal; Update the kernel's **Restore** puts back the set's previous owner-trust.json and
addon (none before the first placing). A failed placing leaves the set as it was, the kernel activated, the session 2D.

**The proof, in order** (`a56 gpu: ...` in the update's receipt, then `xnuhost.log` with `oneios-tool log summary`,
then iOS's log with `phone pull-log`):
1. the receipt: `a56 gpu: placed: ...`; the finish's `@@RESULT a56-gpu placed kernel.kc.gpuaddon SHA (8819312 bytes), owner-trust.json SHA`;
2. `host: galaxy-a56 (...); defaults: gpu=auto, ...`, `sidecar .../kernel.kc.gpuaddon: N bytes`;
3. the self-test: `gpu: self-test: PASS raw: ...` (three lines), `gpu: self-test: PASS renderer: ...` (three lines);
4. `gpu: 3D on amdgpu (/dev/dri/renderD128): the gpuaddon's OneiOSRadeonMetal plug-in (iPhone10,3 20F66) and the
   self-test passed; oneios-gpu-amd=1`, then `gpu: OpenGL ES: the gpuaddon's OneiOSRadeonGL (...); oneios-gpu-amd-gl=1:
   CoreAnimation composites on it` (or, on an older kernel, `gpu: OpenGL ES: the kernel does not read oneios-gpu-amd-gl
   ...: Metal alone, CPU composition, the CoreImage fixup kept`) and `ios16: CoreImage's GL fallback fixup left out: ...`;
5. the kernel: `oneios-gpu: DRM capset (...) context type 2 (AMDGPU)...`, `oneios-gpu-amd: the A56 accelerator: AGXAccelerator,
   MetalPluginName OneiOSRadeonMetal ...`, `oneios-gpu-addon: .../OneiOSRadeonMetal.bundle/Info.plist ...: visible (0)`;
6. a process's device: `oneios-gpu-open: NAME[N] AGXAccelerator type 0x4149 -> 0x0`, `gpu: context N xnu:NAME: amdgpu ...`,
   iOS's `vdrm-darwin: connected: ...` and `oneios-radeonsi: screen "..."`;
7. backboardd: `oneios-gpu-amd: backboardd[N] kept off the Metal plug-in ...; OpenGL ES OneiOSRadeonGL seen`,
   `RG I CTX GLES 3 context ... on AMD Radeon ...`, `RG I GLI first frame drawn ...`;
8. the frame rate: `display: 10 s:` lines against the 2D session's.

**Left, exactly:**
- **The release: done.** v0.1.26 (2026-11-25, [releases.md](releases.md)) carries the kernel `5966e71ce2aa` and the
  two A56 bundles (`runtime.json` `metal_a56`); app 0.5.90. Checked after it: the assets, every runtime file, the kernel.
- **A boot.** Unknowns only a boot shows: whether the Xclipse reads the guest's cached pages coherently (section 2),
  whether radeonsi's screen opens over the Darwin vdrm (section 3), whether CoreAnimation's GL shaders compile on
  Mesa and the per-frame IOSurface copies (4.5 MB written back per 720x1560 frame) beat CPU composition (4a), the
  sandbox's verdicts on 20F66 (section 5). The app's root script ran under bash and busybox ash in its tests, not
  under the phone's mksh.
- **Not ported:** an in-place IOSurface import (the vdrm's `CREATE_BLOB_USER` exists, no winsys calls it), the shader
  cache, timeline syncobjs, Jesvi-iOS's other CoreAnimation fixes (home indicator, colour matrices, blurs), Metal heaps,
  fences, indirect command buffers and the rest of section 4's list; RADV on the same Darwin vdrm (section 3: three
  small patches and the Vulkan runtime for Darwin, a guess) for NativeMetal/metal2vulkan later.

**The app's step (OneOS `oneios-a56-gpu-place` 99a7b0e, host-tested, not booted).** On an iOS 16.5 (20F66) set,
Update the kernel → Activate runs, after the kernel is activated, the staged runtime's `bin/oneios-tool image
a56-gpu-place --stage STAGE --out make-kernel/out/a56gpu` (its SHA-256 checked against runtime.sha256 first). On
`@@A56GPU placed: ...` root checks the three files against the line (`NGPUADD2`, at most 64 MiB, the manifest naming
the new kernel.kc and owner-trust.json), then moves `image/kernel.kc.gpuaddon`, `image/overlay/userland/owner-trust.json`
and `image/manifest.json` in and rewrites `image.sha256` and `image.ids`. `none`, an older runtime's tool, `error` or a
failure leave the set as it was (an addon staged by `phone addon` stays), with the kernel activated and the session
2D. The finish prints `@@RESULT a56-gpu placed|none|skipped ...`, the receipt gets `a56 gpu: ...`, and the activation
message says what happened. `kernel.prev/` keeps the old owner-trust.json and addon (`gpu.kept`), and Restore puts them
back; since the same commit Restore also sets `image.ids` again for every file it moves back (before it, a restored set
failed the switch's preflight with "kernel.kc changed since it was staged", on the S24 Ultra too). The real tool's line
was checked against the script's parse on the build laptop (the integrated bundles, a stage of the 20F66 set's
manifest and owner-trust: the parse, the sums and the file set agree; `gpuctl addon` accepts the addon).

## State at the hand-off of 2026-11-25

**In main** (pushed): stages 1-5 of this file: references, the host renderer (`gpu3d_amdgpu`, `gpuctl amdgpu`,
`gpuctl addon`, `early_2d`), guest Mesa radeonsi for Darwin, the AIR Metal layer, OpenGL ES (OneiOSRadeonGL), the
kernel accelerator (0108 and the GL key), the integrated bundle build. `runtime/host-tests.sh` passed on main.

**On branch `a56-gpu-ship`** (in main as `ae30bc2`..`20f92c2`, linear: the merge commit was flattened by the push's rebase):
`21f9293` the release carries the A56 bundles (package.sh 3c, `ios bundle-record`, runtime.json `metal_a56`,
release.sh `--metal-amd-dir`/`--no-metal-amd`) and `image a56-gpu-place` (also in Make iOS image); `228678f` OpenGL ES
only on a kernel that reads `oneios-gpu-amd-gl` (keeps SpringBoard's CoreImage fixup on 181d8bf2d60a); `c055978` the
placed line matches the app's parse, section 6's user steps; the hand-off commit: this note and the `app` pin moved to
the fork's `oneios` `99a7b0e` (already pushed there, a fast-forward of 976b1fc: Update the kernel places the addon).
Tests run on the branch: `runtime/package/test.sh` 31/31, `cargo test` of oneios-ios, oneios-package, oneios-imagebuild
(a56gpu 5), oneios-serve; xnuhost 133 with fmt and clippy (host, android); the app's `./gradlew test` 742, 0 failures
(the helper's run, on 99a7b0e). The end-to-end check (out/a56-e2e: a package of the real bundles, `image a56-gpu-place`
on the 20F66 set's manifest and owner-trust, the app's parse, `gpuctl addon`) passed.

**Done since (2026-11-25, the build laptop):**
1. Merged (`ae30bc2`..`20f92c2` on main; the tree of the `--no-ff` merge `98e4a3b`, flattened by the push's rebase); `METAL_AMD=1 runtime/host-tests.sh` on that tree: workspace,
   lint, lint-android, port, metal-amd, metal-amd-air, metal-amd-gl, links, package (31/31) pass, boot skipped
   (no qemu), exit 0 (`out/a56ship-hosttests.log`). Pushed; the worktree and branch `a56-gpu-ship` removed.
2. The kernel: `kernel/publish.sh --hosted a56gl-rel --upload` in `out/a56gl-relwt`; downloaded back, SHA-256
   `e64762251fb8c00a291b6295a4c13d18ee3ae817590482dc1e996350fc365a18`; `kernel/release-kernel.txt` pins
   `kernel-xnu-12377.121.6-ios-27.0.1-24A446-5966e71ce2aa` ([releases.md](releases.md)).

**Next, in order:**
1. Done: v0.1.26, `scripts/release/release.sh 0.1.26 --metal-amd-dir out/metal-amd` (exit 0, 2026-11-25; [releases.md](releases.md)).
2. A boot on an A56 (section 6's taps and proof lines).
3. Not done by owner's choice of this session: the licence rule change relayed on 2026-11-25 (code from repos with no
   stated licence) was refused by the permission check; the owner sets it in docs/references.md section 5 directly.

No tmux job of this work is running (the `hold` session keeps the server alive). Logs: `out/a56-gpu-helpers/`.
