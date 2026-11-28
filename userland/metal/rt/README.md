# The Metal bridge on metal2vulkan-rt (port in progress)

The owner's decision of 2026-11-23: "We need to refactor our patches on top of the new upstream." The add-on moves
from metal2vulkan `624bab3` (2026-10-29) to the newest `development` of
[steelbrain/metal2vulkan](https://github.com/steelbrain/metal2vulkan), where the runtime was rewritten as
`metal2vulkan-runtime` 0.2 ("rt": one path for every device, no vendor identity, no environment gate). Our work is
carried as a new, smaller patch set on top. The old add-on (624bab3, `userland/metal/build.sh`) stays the default and
the fallback until a phone boot of the new one passes (docs/switches.md, the add-on row).

Pin of this port: `development` **6462199fa** (2026-11-23 02:28 -0800), 9,217 commits after 624bab3.

What is here:

| Path | What |
|---|---|
| `patches/NNNN-*.patch` | Our changes to rt, applied in name order to the pin (`git apply`); each starts with a one-line reason |
| `bridge/src/driver.rs` | The Vulkan driver opened with no loader (Turnip's dylib on iOS; ANV or lavapipe on Linux) |
| `bridge/src/indices.rs` | The texture and sampler indices rt asks the consumer to number (rt's "Set 1 and index grants") |
| `bridge/src/census.rs` | A library's census: every kernel and the library's vertex/fragment pairs made into pipeline states |
| `bridge/src/bin/m2v-rt-check.rs` | A lasting tool for the build laptop: the floor check, one compute dispatch, the census |

## Milestone 2: no Vulkan loader on iOS (done, tested on the laptop)

iOS has no Khronos loader. rt's `Instance::new` and `Instance::with_drivers` both load a loader
(`ash::Entry::load_from`), and `with_drivers` also needs `VK_LUNARG_direct_driver_loading`, which only a loader
offers. **Patch 0001** adds `Instance::from_driver(entry, extensions)`: the driver's own instance entry
(`vk_icdGetInstanceProcAddr`, else `vkGetInstanceProcAddr`) is the instance's entry, with no loader. Turnip's iOS
dylib exports both (`llvm-nm` of `out/userland/NativeAdrenoMetal.bundle/libvulkan_freedreno.dylib`); ANV exports
only the first. The bridge `dlopen`s the driver, calls `vk_icdNegotiateLoaderICDInterfaceVersion` (7) where the driver
exports it, and never closes the library.

**Patch 0002** makes `VK_KHR_surface` and `VK_KHR_swapchain` optional. Turnip for iOS is built with
`-Dplatforms=macos`, and `tu_wsi.h` defines `TU_USE_WSI_PLATFORM` only for Wayland, XCB, Xlib and display, so it lists
neither; rt never calls them (only a consumer's swapchain does), and our frontend presents through its own scanout.
Upstream's guards of the floor (the README table, `profiles/runtime-floor.json`, the error text) are updated in the
same patch.

Tested on the build laptop (2026-11-23), `m2v-rt-check` built with the pin + 0001 + 0002:

```
$ out/m2v-rt/target/release/m2v-rt-check /usr/lib/libvulkan_intel.so
driver /usr/lib/libvulkan_intel.so: entry vk_icdGetInstanceProcAddr
Intel(R) Iris(R) Xe Graphics (ADL GT2): above the floor
compute: 1000 of 1000 outputs right
```

and rt's unit tests: `cargo test -p metal2vulkan-runtime --lib`, 188 passed, 0 failed, 2 ignored (before the guards
were updated: the same 185 and the 3 guards failing).

## The Adreno 750 against rt's floor (Turnip over Mesa's noop DRM shim)

Turnip of `external/mesa-droidvm` cecc0e9 with every patch of `adreno-compiler-inputs.json`, built for Linux x86-64
(`-Dvulkan-drivers=freedreno -Dfreedreno-kmds=msm -Dtools=drm-shim`, in `out/m2v-rt/mesa`), run as an Adreno 750 by the
noop shim (`FD_CHIP_ID=0x43051401 LD_PRELOAD=.../libfreedreno_noop_drm_shim.so`). The answers come from Turnip's
own tables for the A750 (`fd_dev_info`), not from hardware; the memory types come from the msm kernel driver, not
the darwin one (below).

```
Turnip Adreno (TM) 750: above the floor
  float16 true storage_read_without_format true int64 atomics buffer true shared false float64 false
  maximal_reconvergence false subgroup_uniform_control_flow true texel_offset_operand true attachment_reads true feedback_loop true
  indirect command buffers (VK_EXT_device_generated_commands): no: refused (IndirectCommandsKind::Extension)
```

- Every required feature, limit (update-after-bind: 131,072 images, 24,000 samplers, ...), the queue family and the
  time domains pass (with patch 0002).
- **`VK_EXT_device_generated_commands`: Turnip does not have it, so rt refuses every Metal indirect command buffer**
  (`IndirectCommandsKind::Extension`). The old runtime ran them on a host step. Known gap; WebKit's WebGPU render
  bundles use them (Droid-VM 0038's notes); the system's own clients were not seen using them (not checked on the
  phone).
- Memory: Shared needs a host-visible, coherent and cached type. Turnip's darwin backend has it when the host's capset
  says `has_cached_coherent` (`tu_knl_darwin_virtgpu.cc`), and drm2kgsl answers 1
  (`external/virglrenderer/src/drm/drm2kgsl/drm2kgsl_renderer.c:3034`). Read from the source, not run.
- The compute dispatch runs through Turnip's compiler and submit (the shim executes nothing, so the outputs are
  zeros: "999 of 1000 outputs wrong" is the expected answer there).

## The census of the guest's own libraries (Turnip a750, noop shim)

`m2v-rt-check DRIVER --census out/m2v-rt/metallibs/*.metallib`, the 26 metallibs of 24A446's QuartzCore, CoreImage,
RenderBox, IconRendering, VectorKit and CoreUI (copied out of `os.img` with `oneios-tool image apfs-copy`; Apple files,
kept in `out/`):

| Library | Kernels | Vertex/fragment pairs | Time (x86, this laptop) |
|---|---|---|---|
| QuartzCore `default.metallib` (249 functions) | 38 ok, 3 failed, 13 tile functions not tried | 13 ok, 8 failed | 164 ms |
| CoreImage `default.metallib` | 60 ok | - | 71 ms |
| CoreImage `ci_ubershader.metallib` | 90 ok | - | 212 ms |
| VectorKit `default.metallib` | 3 ok | - | 2 ms |
| the `*_bin.metallib` and `archive.metallib` files | not libraries (binary archives: `Container(BadMagic)`) | | |

The failures:

- QuartzCore's 3 `variable_blur_*_compute` kernels: "unsupported: IdentityDomains (an encoder value the function reads
  as TextureLevels(...))": rt's identity domains do not lower a texture's level count read by the shader yet
  (`metal2vulkan/src/lower/select.rs`, `identity_refusal`). An upstream gap; to fix (a patch) or to wait for.
- `path_blit_vert_lp[fh]` + `path_blit_frag_lp[fh]`: "ColorAttachment { index: 2, metal: NilWithError }": the
  fragment function reads color 2 and the census gives one attachment. This is Droid-VM 0015's case (QuartzCore makes
  these pipelines for passes without that attachment); still needed, to port.
- 6 pairs "VertexAttribute { attribute: 0 }": the census gives no vertex descriptor to vertex functions with
  `[[stage_in]]`. The census's own limit, not a translator failure.

The first-boot cost on the phone (every pipeline translated, lowered and compiled again, since rt's cache keys are
new) is not measured. On this laptop the census of the libraries above took 0.45 s; the phone's cores are slower and
the real clients make many more variants. To be measured with the bridge's self-test on the phone.

## Milestone 1: the inventory of our stack on 624bab3

The stack on 624bab3, as `userland/metal/build.sh` builds it today:

1. **NativeMetal's bridge** (`external/NativeMetal/bridge`, b0d6a07): `NativeMetalBridge.rs` (364 lines, the
   self-test) and `NativeMetalObjects.rs` (3,371 lines): 183 C functions (`NativeMetalBridge.h`,
   `NativeMetalObjects.h`) over the old runtime, called by the frontend `NativeBridgeMetalFrontend.mm` (5,460 lines)
   and the plug-in `NativeAdrenoMetalPlugin.mm`.
2. **NativeMetal's 24 patches** (`bridge/patches/0001..0024`).
3. **Droid-VM-UniXNU's 28 patches** (`patches/metal2vulkan/0011..0038`, 62d0b11), applied after NativeMetal's by its
   `scripts/build-adreno-metal-bridge.py`.
4. **OneiOS's** `userland/metal/patches/metal2vulkan/0101..0103` (staged as 9xxx; never applied: the base's script
   applies a fixed list) and `trace-metal-vulkan-init.patch` (applied by `ios metal-stage-edit`).
5. **`m2v-spare-command-pools.patch`** (branch `fix/gpu-blobs` 50873fe, not on `main`).

On the new tip (6462199fa), `git apply --check`: of the 56 patches, 4 apply (NativeMetal 0009, OneiOS 0101, 0102 and
0103's file but not its hunk: 0103 fails), the rest fail; 41 of them change a file that is gone.

Decisions: **upstream** (the new tip does it), **bridge** (the consumer's job in rt's design: the bridge does it),
**port** (still needed: a patch here), **drop** (old-runtime internals or diagnostics with no counterpart),
**check** (unknown: the census or a phone boot decides; ported only on evidence).

### NativeMetal bridge patches

| Patch | What it did | On rt | Becomes |
|---|---|---|---|
| 0001 optional-constant-output-members | SCCP: an unsupplied optional function constant's aggregate members undef alone | file exists, hunk fails | check |
| 0002 resource-wait-storage-dependencies | which resources a command buffer uses, for the frontend's waits | file gone | bridge (the bridge records what each command buffer binds) |
| 0003 cache-device-entry-point-priming | shader priming by device entry points | gone; rt's `pipeline_cache_dir` and stage entries | drop |
| 0004 fold-constant-global-field-loads | folds loads of constant globals' fields | gone | check |
| 0005 skip-draws-without-a-pipeline | a draw whose pipeline the driver refused binds nothing | gone | bridge (skip the draw, count it) |
| 0006 batch-barriers-and-key-hash-neutral-maps | old runtime's batch internals | gone | drop |
| 0007 optional-shader-cache-directory | a cache directory a sandboxed process may not write | `pipeline_cache_dir: Option`, a failed write is a miss | upstream |
| 0008 llvm-bitreverse | `llvm.bitreverse` | `metal2vulkan-air/src/intrinsics/llvm.rs` | upstream |
| 0009 patch-with-no-control-points | `[[patch(quad, 0)]]` | still applies; upstream still refuses 0 | **rt 0007** (applies as it was) |
| 0010 core-image-kernel-libraries | Core Image kernel libraries (file type 1) | `FunctionInfo` names them; the census parses CoreImage's | upstream |
| 0011 two-plane-shadow-as-memory-lies | a two-plane surface's shadow | gone; rt's `Surface`/`PlaneSpec` | bridge (over rt's surfaces) |
| 0012 srgb-extended-range-blend-rounded-model | extended-range sRGB blends (Astronomy wallpaper) | gone; rt has extended-range lowering | check |
| 0013 growing-memory-blocks | pooled blocks from 4 MiB | 16 MiB units, idle allocations freed after 250 ms, one reserve per class | upstream |
| 0014, 0015 in-place written textures | textures over surfaces and host pages written in place | gone / fails; rt's textures over imports read their memory's bytes | **port, not done**: rt makes a texture over imported host memory an optimal image of its own, copied in and back once a submission (every IOSurface); to measure on the phone first |
| 0016 refuse-rebinds | no call logs for rebinds | gone | drop |
| 0017 heap-texture-size-a-multiple-of-its-alignment | heap texture answers rounded to their alignment | heaps are the consumer's in rt | bridge (round the answer) |
| 0018 memory-census-bytes | live device memory by kind | rt's `Gauges` | upstream (the bridge maps `nmb_device_memory_counts`) |
| 0019 allocation-sites | call stacks of 4 MiB+ allocations | rt's `SiteAllocations` (8 sites) | drop (rt's sites instead) |
| 0020 | not applied | | drop |
| 0021 spare-image-blocks-and-room-keeper | spare dedicated image blocks, a room keeper thread | rt's texture blocks keep one empty, idle frees | upstream |
| 0022, 0024 stage trace, diagnostics switch | each command buffer's stages, the `sheng-diag` switch | gone | drop for now (the frontend's `nmb_stage_ring_read` answers no records; rt's `Metrics` instead) |
| 0023 quality-of-service-on-ios | rt threads at user-interactive on iOS | rt sets no QoS (4 threads per device, `device.rs` `spawn`) | **rt 0004** (`m2v-submit`, `m2v-complete` user-interactive; the rest user-initiated) |

### Droid-VM-UniXNU patches (62d0b11)

| Patch | What it did | On rt | Becomes |
|---|---|---|---|
| 0011 rotated-turns-on-the-2d-engine | rotated framebuffer textures through Turnip's 2D engine | rt has no rotated textures | check (the iPhone's framebuffer may not be rotated) |
| 0012 queue-global-priority | `METAL2VULKAN_QUEUE_PRIORITY` for the render server | rt takes no queue priority (README) | **rt 0006** (`DeviceOptions::queue_global_priority`; the bridge reads the plug-in's variable, retries at the default where the driver says `NOT_PERMITTED`) |
| 0013 trim-kept-memory | frees kept empty blocks on request | idle frees after 250 ms | upstream |
| 0014 quarter-turn-plane-row | a quarter-turned texture over a plane | no rotated textures | check (with 0011) |
| 0015 unattached-color-reads | `[[color(n)]]` with no attachment reads zero | census: still refused (`path_blit`) | **rt 0003** (also lets `color_inputs_attached` pass: upstream refuses on purpose, as macOS's Metal does; iOS's makes them). Census: QuartzCore's 2 `path_blit` pairs made |
| 0016 specialized-name-links | `specializedName` links | `LinkedFunction::with_specialized_name` | upstream |
| 0017 spares-within-the-address-space | spares held to 1/16 of the GPU VA | no spares | drop |
| 0018 specialized-linked-functions-closed | linked functions with constants | `LinkedFunction.constants` | check |
| 0019 pooled-block-size-from-the-environment | 16 MiB blocks | rt's unit is 16 MiB | upstream |
| 0020 hold-back-memory-under-pressure | no kept room under pressure | rt keeps one 16 MiB reserve per class | drop (bridge: nothing to hold back but its own pools) |
| 0021, 0023, 0024 Turnip DontCare stores | Metal's DontCare/MultisampleResolve stores as `DONT_CARE` (37 ms -> 9 ms a pass of 64 blended draws) | rt stores them (`pass.rs`: `STORE`) | **rt 0005** (`DeviceOptions::dont_care_stores`), **off by default since 2026-11-28**: not sound where rt splits a pass (below, "DontCare stores and split passes"); the A/B is the switch `oneios-dont-care=1` |
| 0022 trim-spare-command-pools | spare command pools trimmed | rt's reaper frees pools past the idle ones it keeps | upstream (per the other helper; to confirm by the GPU blob count on the phone) |
| 0025 full-barriers-switch | a diagnostic | | drop |
| 0026 copy-back-drops-carried-shadows | shadow copy-back | rt's own shadows | check |
| 0027 keep-room-cap | `METAL2VULKAN_KEEP_ROOM_MIB` | no keep-room | drop |
| 0028 placement-sites | a diagnostic | rt's `SiteAllocations` | drop |
| 0029 memoryless-backings-cover | memoryless backings shared | **rt does not build Memoryless textures** (`Gap::MemorylessTexture`) | bridge (memoryless as Private textures, shared per size and format) |
| 0030 idle-merge-scratch-spares | shadows' scratches freed when idle | rt's shadows | check |
| 0031 backing-cover-switch-and-pool-report | diagnostics | | drop |
| 0032..0035 resident uses, shared maps, encoder residency, wide pointers | backports of upstream's series (2026-11-03..05) | upstream since | upstream |
| 0036 words-indexed-from-one-address | psb word index from one address | `psb::split`-style code upstream (`lower/access`) | check |
| 0037 plain-cube-directions | cube directions skip Metal's tie handling where they may | rt's sampler lowering (`face::metal_ties`) | check |
| 0038 settled-gpu-writes-read-at-commit | ICB records read at commit | ICBs refused on Turnip (no DGC) | drop until ICBs run |

### OneiOS's own

| Patch | What it did | On rt | Becomes |
|---|---|---|---|
| 0101 large-indexed-constants-to-blob | large run-time-indexed constants read from the constant blob | applies | check (never applied before; ported only on a census or phone failure) |
| 0102 blob-constant-gep-pointers | GEP constants into the blob | applies | check (with 0101) |
| 0103 block-bytes-per-process | `M2V_BLOCK_MB` per process | rt's 16 MiB unit | drop |
| `trace-metal-vulkan-init.patch` | `ONEIOS_VK_INIT` lines of the instance's making | file gone | bridge (the bridge logs its own driver open and floor answer) |
| `m2v-spare-command-pools.patch` (fix/gpu-blobs) | spare pools follow recent use | see Droid-VM 0022 | upstream (to confirm on the phone: the plug-in's weak `m2v_command_pool_census` answers nothing on rt, so the CP row stays silent) |

### The other effort's fixes (`fix/m2v-update`, on 624bab3, not committed when this was written)

The owner, 2026-11-24: the two metal2vulkan efforts are one stack on rt; the 624bab3 add-on stays only as the fallback.

| Its change | On rt | Becomes |
|---|---|---|
| `m2v-idle-memory.patch`: the idle trim frees the old runtime's spare dedicated image blocks (`METAL2VULKAN_IDLE_SPARES`, boot word `oneios-idle-spares=0`) | rt keeps no spare image blocks: an idle allocation goes after 250 ms, one 16 MiB reserve per class (rt README, "Memory blocks and buffers") | unnecessary on rt (the switch does nothing there) |
| `turnip-idle-bo-cache.patch`: Turnip's freed-BO cache trimmed after 60 s idle (boot word `oneios-bo-cache-idle=0`) | Turnip is the same in both add-ons | shared: `userland/metal/build.sh stage` builds the same Mesa for both runtimes, so it reaches the rt add-on once it is on `main` |
| `turnip-submit-split.patch` (Droid-VM/UniXNU fb9fb47): a kernel submission ends only where a command buffer begins (the GPU hang at a render server's start), no switch | Turnip is the same in both add-ons | shared, as the row above |
| The frontend edit of NativeMetal 3c46b5e: a compute pipeline rebuilt with its unset constants zero keeps its function until the reflection is read (cameracaptured's crash), no switch | the frontend is the same in both add-ons (`ios metal-stage-edit` edits it with or without `--rt`) | shared |
| The GPU memory row (`CommandPoolRow.inc`'s `GM` line, weak `m2v_memory_census`) | rt has the figures in its gauges | the bridge answers `m2v_memory_census` from rt's gauges (`abi/device.rs`; words in its comment); `s` (spares) is 0 on rt |
| The render-pass split counters (the `RP` line) | rt counted no splits | **rt 0008**: `Metrics::render_pass_splits` and `render_pass_barrier_splits`; the bridge counts the render encoders |

Its findings that hold for rt too: the pipeline cache and preprocess-pool commits do not matter on Turnip (no
`VK_EXT_device_generated_commands`, so rt refuses indirect command buffers there). The posters'
`OutOfMemory(Device)` it saw on the old runtime is to be checked on the phone with the rt add-on.

## Milestones 4 to 6

**4, our fixes, on rt** (`patches/`, each with its one-line reason at the top):

| Patch | Is | Test (build laptop, 2026-11-24) |
|---|---|---|
| 0001 instance-from-a-driver | the loader-free instance | ANV through it: compute, render and the IOSurface path right |
| 0002 presenting-is-optional | surface and swapchain optional | the floor's guards updated; rt unit tests |
| 0003 unattached-color-reads | Droid-VM 0015 | census: QuartzCore pairs 13 -> 15 of 21 (`path_blit_vert_lp[fh]`); the translator's tests of the touched areas: 224 passed, 3 failed before the tests were updated (the one that pinned the refusal now pins the zero read, the inputs encoding takes the new field, a tessellation test's spirv-val temp-file race passes alone); those 3 pass after |
| 0004 quality-of-service-on-apple | NativeMetal 0023 | builds for arm64e; not run (Linux sets nothing) |
| 0005 dont-care-stores | Droid-VM 0021, off by default (2026-11-28) | `tests::pass` checks both mappings |
| 0006 queue-global-priority | Droid-VM 0012 | ANV refuses high (`NOT_PERMITTED`): the bridge makes the device at the default; Turnip (noop shim) takes it |
| 0007 patch-with-no-control-points | NativeMetal 0009 | applies as it was |
| 0008 render-pass-split-counts | the other effort's RP counters | `abi_smoke`'s census check |
| 0009 complete-poll-device-lost | poll `vkWaitSemaphores` every 100 ms while GPU work is in flight so Turnip's `vk_device_check_status` detects KGSL GPU resets instead of hanging forever | rt unit tests (`build.sh test`) |
| 0010 adreno-large-constants-to-blob | port OneiOS 0101 + 0102: large indexed constant globals (>4 KiB) go to the constant blob and `blob_ptr` resolves constant pointer casts / GEPs | rt + translator unit tests (`build.sh test`) |
| 0011 ycbcr-two-plane-textures | GPU compute conversion of all 27 two-plane YCbCr/RGB formats (`YCBCR8_420_2P`, `_sRGB`, `RGB8_420_2P`, 10-bit unpacked/packed, 4:2:0/4:2:2/4:4:4) over IOSurface buffers into optimal `RGBA16Float` images on submission (`over::record_side`) | rt unit tests (`build.sh test`) + `abi_smoke` on Intel Vulkan (`YCBCR8_420_2P`, `RGB8_420_2P`, `YCBCR8_420_2P_sRGB`, views) |

With all eleven: rt's unit tests (`build.sh test`), `abi_smoke` and `m2v-rt-check --selftest` pass.

**v0.1.45 fixes (2026-11-28, host-tested; phone verification next):**
1. **Two-plane YCbCr / RGB video surfaces (`rt 0011`, `abi/texture.rs`, `abi/device.rs`)**: `backboardd` (2,011 `YCBCR84202P` refusals) and `CollectionsPoster` (538 `RGB84202P` refusals) still asked for two-plane YCbCr/RGB Metal textures over decoded video IOSurfaces even with `supportsYCBCRFormats=0`, leaving video black. Patch `0011` adds a self-contained Vulkan 1.3 compute shader (`ycbcr.rs`, `ycbcr_spv.rs`) matching Apple Silicon's 4:2:0 (9/3/3/1) / 4:2:2 (3/1) / 4:4:4 (1/1) chroma filtering and fixed-point BT.601/BT.709/BT.2020 video/full-range conversion, dispatched during `over::record_side` into an optimal `RGBA16Float` image. `supportsYCBCRFormats` and `supportsYCBCRPackedFormats` now answer `1` (PQ/XR/12-bit remain `0`), and `nmb_texture_new_view` maps YCbCr view formats to the `RGBA16Float` backing format.
2. **Intra-encoder memory barriers (`abi/command.rs`, `oneios-ios/src/metal.rs`)**: `NativeBridgeMetalFrontend.mm` implemented `NBComputeEncoder::memoryBarrierWithScope:` as a no-op and left `NBRenderEncoder`'s `memoryBarrierWithScope:afterStages:beforeStages:`, `memoryBarrierWithResources:count:afterStages:beforeStages:`, and `textureBarrier` unimplemented (silently swallowed by `NBObject::forwardInvocation:`). After a render pass's first `feedback_moves` transition, subsequent backdrop/composite draws reading the render target never set `self.owed`, omitting `vkCmdPipelineBarrier2` between color writes and fragment reads on Adreno 750 (`sysmem` CCU cache-line corruption during Home Screen/Settings/Safari page swipes), and compute encoders dropped intra-encoder barriers. `nmb_encoder_memory_barrier` now forwards all five selectors to `rt`'s `RenderCommandEncoder::memory_barrier()` and `ComputeCommandEncoder::memory_barrier()`.
3. **Sandbox-safe `RtSwitches.inc`**: `rtSwitches()` now still applies `TU_DEBUG=sysmem,nocb` when `sysctlbyname("kern.bootargs", ...)` is blocked by an app/daemon sandbox, and logs one `os_log` line (`OneiOS rt switches: ...`) recording the active `TU_DEBUG`, `dont_care`, and `queue_priority` values.

### DontCare stores and split passes (2026-11-28)

The owner's boot of v0.1.40 (2026-11-27, rt add-on, `oneios-dont-care=1`, then the default): garbage in the middle of
the screen while SpringBoard, Settings and Safari change pages. Read in rt's code at the pin:

- `pass.rs` `Target::info` fixes each attachment's store op as `vkCmdBeginRendering` records, and gives a rendering
  begun again (`resumed`) the load op `LOAD`. Its module documentation (lines 7-12) says why rt stores every action:
  "a pass may end its rendering before the encoder ends and begin it again, loading what it stored".
- `render_encoder.rs` `interrupt` ends the rendering, records, and begins it again with `resumed` true. Its callers:
  `ordered` (a memory barrier after a draw that wrote memory, or the barrier an attachment read owes), `feedback_moves`
  (a draw that reads its own attachment through a texture: the backdrop and blur reads of a compositor),
  `shadow_copies` (the same without the feedback loop), `results_owed` (a visibility result's copies) and
  `draw_reaches` (copies of a texture over a buffer, `crate::over`, before a draw that samples it). None of them is
  known as the rendering begins.
- rt 0005 made `DontCare` and `MultisampleResolve` stores `DONT_CARE`. On Turnip a `DONT_CARE` store writes no tile
  back to memory, so at a split the rendering before it is lost and the next one loads stale memory: the attachment
  holds garbage where the earlier draws were (for a stencil or depth attachment, the later draws test against
  garbage; for a multisampled one, the next resolve resolves stale samples over them). Whether the 624bab3 runtime's
  Droid-VM 0021 has the same fault at its own splits was not checked (that add-on is not the one releases ship).

Since Vulkan takes the store op at the beginning and rt cannot know there whether the encoder will split, the only
sound choice is to store: the bridge sets `dont_care_stores` only for `METAL2VULKAN_DONT_CARE_STORES=1` (boot word
`oneios-dont-care=1`, an A/B of the speed). The resolve itself is unchanged either way (`Target::info` names the resolve
view and mode whatever the store op). What it costs is Droid-VM 0021's figure, a 4x attachment's samples stored per
tile (37 ms against 9 ms for 64 blended full-screen 4x draws in gfxprobe); a 1x attachment's store is one tile write,
as every `Store` pass does. Keeping `DONT_CARE` soundly needs a rendering whose begin is recorded after its end is
known (each segment's draws in a secondary command buffer, the begin chosen at `interrupt` or `endEncoding`): not done.
`turnip-submit-split.patch` splits Turnip's kernel submissions only where a command buffer begins, never a pass.

Proof on the phone: the same transitions with no garbage band on the default (`oneios-dont-care=0` in xnuhost's
`switches:` line), and the band back with `boot-args-add=oneios-dont-care=1`; iOS's log (`oneios-tool phone pull-log`)
`oneios-gpu-text: backboardd[N] RP enc +E inst +I split +S barrier +B` with `S` above 0 shows that the render server's
passes do split. Not booted yet.

Not ported (with why): the in-place textures over host imports (NativeMetal 0014/0015: rt's copies of every
IOSurface's bytes; a larger change to rt's `over` path, to do once the phone shows its cost), the rotated textures
(Droid-VM 0011/0014: the iPhone's framebuffer may not be rotated; check), and the "check" rows above (on evidence only).

**5, the build.** `METAL_RUNTIME=rt userland/metal/build.sh` builds the same plug-in with this bridge: `stage` passes
`--rt` to `oneios-tool ios metal-stage-edit` (the plug-in script links the library `userland/metal/rt/build.sh ios` puts
in `build/adreno-metal-bridge/ios27` instead of building the old one) and appends `RtSwitches.inc`; its folders are
`out/metal-rt` and `out/userland-rt`. The plug-in imports the old runtime's telemetry weakly, so the bridge defines
`m2v_command_pool_census`, `m2v_placement_sites` and `m2v_pool_report` (answering nothing) and `m2v_memory_census`
(above). One boot add-on with the camera plug-in: `oneios-tool ios addon OUT --metal out/userland-rt/NativeAdrenoMetal.bundle
--camera out/userland/camera`.

The first-boot cost of re-translation (rt's cache keys are all new, so every pipeline is lowered and compiled again
once, then kept in the plug-in's cache directory): not measured on the phone. On the laptop (x86, one thread) the census
of QuartzCore and two CoreImage libraries took 0.11 + 0.05 + 0.14 s through Turnip's compiler (noop shim); the phone's
cores are slower and the real clients make many more variants than one per function.

**6, the fallback.** Both add-ons are kept: the 624bab3 one (`METAL_RUNTIME` unset, the default) and the rt one; the
Mac stages either with `oneios-tool phone addon FILE` (docs/switches.md, "The Metal add-on").

## Milestone 3: the bridge on rt (tested on the laptop; in the rt add-on since milestone 5)

`bridge/src/abi/` implements all 184 C functions of NativeMetal's `NativeMetalBridge.h` and `NativeMetalObjects.h`
(b0d6a07) on rt, the ABI unchanged: `nm` of the static library against the headers' list, 184 and 184, none missing or
extra. The frontend's other bridge calls (`nmb_slowest_pass`, `nmb_memory_row`, `nmb_memory_top`, `m2v_placement_sites`,
`m2v_pool_report`) are weak in the plug-in or defined by the frontend itself.

What the bridge does that the old runtime did (rt's README, "What the consumer implements"):

| Piece | How |
|---|---|
| The driver | `driver.rs`: `dlopen` of Turnip's dylib, its `vk_icdGetInstanceProcAddr`, `Instance::from_driver` (rt 0001) |
| Indices | `indices.rs`: rt asks the consumer to number every texture and sampler (1..65,535 and 1..3,999), constexpr samplers one index per state |
| The retire gate | `abi/device.rs` `RetireGate`: what a released object leaves (an index, a small buffer's slot, a heap range) is handed out again only once every command buffer made before the release retired (rt's rule for indices; the bridge's for its own ranges) |
| Buffers | `abi/memory.rs`: more than 32 KiB, a block of its own; up to 32 KiB, a slot of a 256 KiB slab of one power-of-two size (256 B..32 KiB), so small buffers do not take rt's 64 KiB granule each; no-copy buffers are `import_host_memory` with Metal's deallocator as the import's release |
| Heaps | one block per heap and the bridge's first-fit placement for automatic heaps; heap textures' size answers rounded up to their alignment (NativeMetal 0017) |
| Textures | rt's, at the bridge's indices; Memoryless storage as Private (rt builds no Memoryless texture); the options word, rotation, framebufferOnly, isDrawable and purgeable state kept by the handle |
| Argument encoders | the translator's `ArgumentBufferLayout`, written by the bridge through the buffer's mapping (8 bytes an argument) |
| Reflection | the translator's, as the old bridge's JSON (the same code) |
| Binary archives | their additions make the state (rt's pipeline cache directory keeps it); a serialized archive is an empty file |
| Device answers | the old runtime's answers on the Adreno 750 (dumped on the laptop: 28 counts, 23 flags, the public queries), the Vulkan limits read where they depend on the device; `supportsFamily:` true for Apple1-3 and Common1-2 as before |
| Telemetry rows | rt's `Metrics` and `Gauges` where they count the same thing; the stage ring, allocation sites and no-copy remaps answer nothing |

Not made by this bridge (each call fails with its reason in `nmb_last_error`): two-plane `RGB10A8_2P_XR10` / 12-bit / PQ / XR YCbCr textures over a surface
(`supportsYCBCRFormats12`, `PQ`, `XR` answer NO; 8-bit and 10-bit SDR two-plane YCbCr/RGB formats are converted on the GPU by rt 0011), sparse heaps, a tessellation factor scale other than 1, patches drawn with a
control point index buffer, blit option `RowLinearPVRTC`, pipeline states' resource IDs (0; rt refuses indirect command
buffers on Turnip anyway).

Tests on the build laptop (2026-11-24), `userland/metal/rt/build.sh check`, the Intel GPU through ANV and the direct path:

```
$ out/m2v-rt/target/release/abi_smoke /usr/lib/libvulkan_intel.so     # 35 checks through NativeMetal's headers
... 0 failed
$ out/m2v-rt/target/release/m2v-rt-check /usr/lib/libvulkan_intel.so --selftest --qc out/m2v-rt/metallibs/QuartzCore-default.metallib
selftest 0x0 (142 ms): compute_bad 0 render_bad 0 import_bad 0 qc functions 249 kernels 38 ok 3 failed, pairs 13 ok 8 failed
```

`abi_smoke` covers: the device answers (69 answered, 6 without an answer as before), small, large and no-copy buffers
(its deallocator runs once rt frees the import), the fixture's kernel and quad through the ABI's descriptors, getBytes and
replaceRegion, a view, a heap with a buffer and a texture, a sampler, a depth/stencil state, an argument encoder, a shared
event a command buffer signals, a fence, blits, the memory counts. The arm64e-apple-ios static library builds
(nightly-2026-11-03, `-Z build-std`, 1 min 36 s, 34 MB); it has not run on the phone.

Known performance gap found while porting: rt makes a texture over imported host memory (every IOSurface: the render
server's client surfaces) an optimal image of its own and copies the bytes in and back once a submission (rt README,
`Buffer::new_texture`), which NativeMetal 0014/0015 had removed on the old runtime. To port (milestone 4).

## Issue #81: backboardd's blur over a wide-colour attachment (2026-11-27)

v0.1.36 was the first release made with `--metal-rt`, so it shipped this add-on. On an SM-S928B (issue #81, OneOS
0.5.97) backboardd aborted three times a boot ("Metal failed to build render pipeline"), launchd rebooted iOS
("critical process crashes: backboardd"), and xnuhost ended with a kernel panic. The refusal, cut in the crash report
at "at metada", read whole on the build laptop with the new `--pair` mode (Turnip a750, noop shim):

```
$ m2v-rt-check libvulkan_freedreno.so --pair metallibs/QuartzCore-default.metallib downsample_blur_vert_lph alpha_threshold_lph \
    BGRA8Unorm BGRA8Unorm_sRGB RGBA16Float A8Unorm R8Unorm R16Float BGR10_XR BGRA10_XR BGRA10_XR_sRGB BGR10_XR_sRGB RGBA8Unorm_sRGB
pair downsample_blur_vert_lph+alpha_threshold_lph MTLPixelFormatBGR10_XR (554) over: the translator made no pipeline of
  downsample_blur_vert_lph, alpha_threshold_lph: unsupported: Format; needs NotImplemented at metadata of entry
  alpha_threshold_lph, color attachment 0
  (the same for BGRA10_XR, BGRA10_XR_sRGB, BGR10_XR_sRGB)
pair downsample_blur_vert_lph+alpha_threshold_lph: 40 of 44 attachments made (each format plain, blended, alpha-masked, colour-masked)
```

rt refuses **every blend into Apple's extended-range forms** (`BGR10_XR`, `BGRA10_XR`, `RGB10A8_2P_XR10` and their
`_sRGB` twins) on every device (`metal2vulkan/src/lower/stage_io/extended_color.rs`, `plan`: "not implemented on any
device"), and every blend into its padded 10-bit forms (`RGB10A2Unorm_sRGB`, `R10Unorm_X6`, `RG10Unorm_X12`, their
`_sRGB` twins: `FramebufferFetch`); it makes no texture of them at all (`Gap::TextureFormat`). The 624bab3 add-on
blended them in the shader (NativeMetal 0012).

**The fix (the bridge, 2026-11-27): the rt add-on holds those forms in formats rt renders and blends**
(`bridge/src/abi/mod.rs`, `stored`), of the same bytes a pixel: `BGR10_XR`(`_sRGB`) in `RG11B10Float`,
`BGRA10_XR`(`_sRGB`) in `RGBA16Float`, `RGB10A2Unorm_sRGB` in `RGB10A2Unorm`, `R10Unorm_X6`(`_sRGB`) in `R16Unorm`,
`RG10Unorm_X12`(`_sRGB`) in `RG16Unorm`. Textures, views, render pipeline attachments and the linear alignment go
through it; `nmb_texture_describe` answers the format asked for. The values are kept (floats keep values past 1;
`RG11B10Float` drops negatives), not Apple's bytes: every producer and consumer of such a texture is this runtime, as
backboardd's own framebuffer is 8-bit (the frontend withholds `supportsExtendedXR10Formats` from it) [I: a CPU read of
such a texture's bytes would see the other encoding; none seen]. `RGB10A8_2P_XR10` (two planes) is not mapped. On the
laptop (Turnip a750, noop shim):

```
$ m2v-rt-check libvulkan_freedreno.so --pair metallibs/QuartzCore-default.metallib downsample_blur_vert_lph alpha_threshold_lph \
    BGR10_XR BGR10_XR_sRGB BGRA10_XR BGRA10_XR_sRGB RGB10A2Unorm_sRGB R10Unorm_X6 R10Unorm_X6_sRGB RG10Unorm_X12 RG10Unorm_X12_sRGB BGRA8Unorm
pair downsample_blur_vert_lph+alpha_threshold_lph: 40 of 40 attachments made (...)
$ ... the same with --raw (rt alone)
pair downsample_blur_vert_lph+alpha_threshold_lph: 29 of 40 attachments made (...)
```

the QuartzCore census unchanged (kernels 38 of 41, pairs 15 of 21), `abi_smoke` over ANV 0 failed, the bridge's unit
test of the mapping (`abi::tests`). v0.1.37 shipped the 624bab3 add-on while this was open; the next `--metal-rt`
release carries the fix. Not booted on a phone yet: the proof is backboardd with no `sheng-exit ... Metal failed to
build render pipeline` and SpringBoard (9 of 9).

`--pair LIB VERTEX FRAGMENT [FORMAT...] [--raw]` (a lasting tool): one vertex/fragment pair, which the name-matched census
never makes, built with a vertex descriptor from the vertex function's own `[[attribute(n)]]` list (buffer 30). Each
format (a Metal name with or without `MTLPixelFormat`, or its number; none given: all 301) is tried plain, with Core
Animation's premultiplied "over" blend, alpha-masked and colour-masked, held as the plug-in holds it (`--raw`: as rt is
asked), and every refusal is printed whole.

## Issue #86: the same abort on v0.1.38, from the image's old plug-in (2026-11-28)

The reporter of #81 booted v0.1.38 (git `686ac41`, the bridge's fix above) on the same SM-S928B and 24A446 image, and
backboardd aborted three times with the same cut message (`... alpha_threshold_lph: unsupported: Format; needs
NotImplemented at metada`). **The cause was not rt or the bridge: the plug-in that ran was v0.1.36's.** An image keeps
the plug-in its Data seed was made with (`/private/var/oneios/addons/Extensions`), and nothing replaced it: the log has
`oneios-gpu-addon: boot addon: -1 files (-1 none)`, the same `data.seed` (1202 MiB, checkpoint xid 15) as the #81
boots, and a Data store carried on from them (xid 300 then 420; a new seed starts the store over, image-format.md 5).
v0.1.38's plug-in builds this pair for every format the bridge holds (`--pair` over all 301 formats, Turnip a750,
noop shim, 2026-11-28): the only refusals of the `needs NotImplemented at metadata` kind left are the "over" blend into
`RGB10A8_2P_XR10` and `RGB10A8_2P_XR10_sRGB` (two planes, not mapped by `stored`), which backboardd's 8-bit
framebuffer has no reason to use [I: not seen in any log].

**The fix** (xnuhost and the release, gpu.md "A runtime update brings its plug-in"): the release packs each plug-in
also as the runtime's boot addon (`kernel/gpuaddon`, `kernel/24B156/gpuaddon`), and xnuhost gives it to the kernel
when the image set has none of its own and the plug-in's `oneios-target` names the set's build, so the kernel writes
the runtime's plug-in over the seed's at every boot.

**v0.1.42 boot on SM-S928B (2026-11-28, iOS 27.2.1 24B156, takeover):** the boot addon worked (`gpuaddon:
/data/runtime/kernel/24B156/gpuaddon: 9517872 bytes, 2 CDHashes trusted`, `oneios-gpu-addon: boot addon: 5 files, 5
written`), making this the first boot where the 24B156 image ran `metal2vulkan-rt`. `backboardd` still aborted three
times on `spec=Pb3a8BsovXmw_TaltA3Xhf_Isrc` (`downsample_blur_vert_lph` + `alpha_threshold_lph`: `unsupported: Format;
needs NotImplemented at metada`), and `launchd` triggered shutdown at +66.8 s. `'b3a8'` is `MTLPixelFormatRGB10A8_2P_XR10`
(the exact two-plane XR10 format `abi::stored` had skipped above). `abi::stored` now also maps `RGB10A8_2P_XR10` /
`RGB10A8_2P_XR10_sRGB` to `RGBA16Float` (same 8 bytes/texel as rt's `R32G32_UINT`, with hardware blending and alpha)
and `R16Bfloat`/`RG16Bfloat`/`RGBA16Bfloat` to `R16Float`/`RG16Float`/`RGBA16Float`; `nmb_device_linear_bytes_per_row`
reads `pixel_format_info` of the stored format; and `nmb_texture_new_on_surface` allows single-plane views (such as
NV12 video planes) over 2-plane IOSurfaces instead of rejecting whenever `chroma_bytes_per_row != 0`.

**v0.1.43 boot on SM-S928B (2026-11-28, iOS 27.2.1 24B156, takeover):** `backboardd` no longer aborted, `SpringBoard`
started, and `oneios-display` accepted swaps 1..129, **but every frame was all black** (`backboardd fences 1`, then `0`
thereafter). `ioslog` showed three bridge bugs:
1. **`minimumLinearTextureAlignmentForPixelFormat:` returned Turnip's 512-byte linear image pitch rounding** (`abi/device.rs`),
   so `backboardd` logged `Failed to allocate MTLTexture from IOSurface 0x2. Row bytes (5888) not 512 aligned` and
   `IOSurface 0x1. Row bytes (5760) not 512 aligned` on every frame and never rendered into its display framebuffer.
   `rt`'s `Buffer::new_texture` takes any whole-texel pitch (and uses its optimal-image over-copy path for every
   `import_host_memory` IOSurface anyway); the bridge now answers 16 bytes for every uncompressed colour format (matching
   Apple Metal and the old runtime) and refuses compressed/depth/stencil formats.
2. **`reached()` gave texture indices to depth/stencil `RENDER_TARGET`-only textures** (`abi/texture.rs`), which `rt`
   refuses (`a texture no shader reaches takes no index`) — 932,353 refusals across `iconservicesagent` (554k),
   `SpringBoard` (347k, causing hangs and a crash), `CollectionsPoster` (30k), `MapKit` (whose missing depth target then
   failed `AttachmentFormats`), and `MercuryPosterExtension`. `reached(pf, usage)` now excludes depth/stencil formats when
   only `RENDER_TARGET` is set, matching `rt`.
3. **`no_copy` failed when `length` was not a multiple of 4096** (`abi/memory.rs`, e.g. `iconservicesagent` importing
   52224 and 210944 bytes in 16 KiB pages). `no_copy` now rounds the `import_host_memory` window out to 4096-byte
   boundaries (strictly within the OS pages `[pointer, pointer + length)` already occupies) and places the `rt::Buffer`
   at the exact `(offset, length)`.

**v0.1.44-test boot on SM-S928B (2026-11-28, iOS 27.2.1 24B156, takeover, `display=drm`):** the three fixes above ended
the black screen (`guest ready: first frame with content`, up to 101.3 fps, `p50` 8.4 ms, 0 alignment/fmt-260/no-copy
errors, 0 `backboardd` crashes), and exposed four more runtime/bridge faults fixed here:
1. **Stuck screen after KGSL GPU reset (`rt 0009`)**: at +297.7 s `backboardd` queue 1 hit a KGSL reset
   (`kgsl_note_reset: queue 1: the GPU was reset with this context guilty`; `GPU_COMMAND failed: Resource deadlock would
   occur`). `complete::run` called `vkWaitSemaphores(..., u64::MAX)`, so Mesa's `vk_common_WaitSemaphores` never returned
   to call `vk_device_check_status()`, `DeviceLost` (code 11) never reached `endRenderServerAfterGpuLoss()`, and
   `backboardd` stayed frozen instead of restarting. `0009-complete-poll-device-lost.patch` waits `LOSS_POLL` (100 ms)
   while GPU work is in flight (still sleeping on `Condvar` when idle).
2. **`iconservicesagent` 15-crash loop (`abi/device.rs`, `abi/shader.rs`)**: `supportsFloat16BCubicFiltering` answered
   `true` while `rt` refuses `SamplerMinMagFilter::Cubic` (`Gap::CubicSampler`, 18 refusals). The bridge now answers
   `false` to `supportsFloat16BCubicFiltering` and clamps `SamplerMinMagFilter::Cubic` to `Linear` in `nmb_sampler_new`.
3. **1,013 `YCBCR84202P` refusals during Safari video playback (`abi/device.rs`)**: `supportsYCBCRFormats12` answered
   `false`, leaving `_MTLDevice`'s `supportsYCBCRFormats` / `supportsYCBCRPackedFormats` / `PQ` / `XR` answering `YES`
   for `Apple3`. All six selectors now answer `false` so CoreAnimation uses separate `R8Unorm` + `RG8Unorm` planes.
4. **Center-of-screen glitch on 8-bit BGRA page transitions (`RtSwitches.inc`) & Adreno large constants (`rt 0010`)**:
   `RtSwitches.inc` now sets `TU_DEBUG=sysmem,nocb` on 8-bit BGRA displays (when `oneios-display-wide=1` and
   `oneios-gmem=1` are not set, still overridable by `sheng-tu-debug=`), avoiding Turnip GMEM tile/feedback-loop
   corruption on Adreno 750; `0010-adreno-large-constants-to-blob.patch` ports OneiOS's Adreno fixes `0101`/`0102`
   (runtime-indexed constant tables > 4 KiB to the constant blob, and cast/constant-GEP blob pointers) into `rt`.

Not the cause either: the conformance list's `padded-fetch-*` refusals (`known-gaps-rt.tsv`, "at metadata of entry
pfx_fetch_read, argument 1": a fragment function that reads its padded attachment). `alpha_threshold_lph` does not read
its attachment (its refusal names no argument), and through the bridge the padded forms are held in `R16Unorm`,
`RG16Unorm` and `RGB10A2Unorm` (`stored`) [I: a fetching pair over them not tried]; those rows stay rt's own gaps.
