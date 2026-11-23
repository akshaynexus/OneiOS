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
| 0009 patch-with-no-control-points | `[[patch(quad, 0)]]` | still applies; upstream still refuses 0 | port |
| 0010 core-image-kernel-libraries | Core Image kernel libraries (file type 1) | `FunctionInfo` names them; the census parses CoreImage's | upstream |
| 0011 two-plane-shadow-as-memory-lies | a two-plane surface's shadow | gone; rt's `Surface`/`PlaneSpec` | bridge (over rt's surfaces) |
| 0012 srgb-extended-range-blend-rounded-model | extended-range sRGB blends (Astronomy wallpaper) | gone; rt has extended-range lowering | check |
| 0013 growing-memory-blocks | pooled blocks from 4 MiB | 16 MiB units, idle allocations freed after 250 ms, one reserve per class | upstream |
| 0014, 0015 in-place written textures | textures over surfaces and host pages written in place | gone / fails; rt's textures over imports read their memory's bytes | check (bridge over rt's imports) |
| 0016 refuse-rebinds | no call logs for rebinds | gone | drop |
| 0017 heap-texture-size-a-multiple-of-its-alignment | heap texture answers rounded to their alignment | heaps are the consumer's in rt | bridge (round the answer) |
| 0018 memory-census-bytes | live device memory by kind | rt's `Gauges` | upstream (the bridge maps `nmb_device_memory_counts`) |
| 0019 allocation-sites | call stacks of 4 MiB+ allocations | rt's `SiteAllocations` (8 sites) | drop (rt's sites instead) |
| 0020 | not applied | | drop |
| 0021 spare-image-blocks-and-room-keeper | spare dedicated image blocks, a room keeper thread | rt's texture blocks keep one empty, idle frees | upstream |
| 0022, 0024 stage trace, diagnostics switch | each command buffer's stages, the `sheng-diag` switch | gone | drop for now (the frontend's `nmb_stage_ring_read` answers no records; rt's `Metrics` instead) |
| 0023 quality-of-service-on-ios | rt threads at user-interactive on iOS | rt sets no QoS (4 threads per device, `device.rs` `spawn`) | port |

### Droid-VM-UniXNU patches (62d0b11)

| Patch | What it did | On rt | Becomes |
|---|---|---|---|
| 0011 rotated-turns-on-the-2d-engine | rotated framebuffer textures through Turnip's 2D engine | rt has no rotated textures | check (the iPhone's framebuffer may not be rotated) |
| 0012 queue-global-priority | `METAL2VULKAN_QUEUE_PRIORITY` for the render server | rt takes no queue priority (README) | port (a `DeviceOptions` field, not a variable) |
| 0013 trim-kept-memory | frees kept empty blocks on request | idle frees after 250 ms | upstream |
| 0014 quarter-turn-plane-row | a quarter-turned texture over a plane | no rotated textures | check (with 0011) |
| 0015 unattached-color-reads | `[[color(n)]]` with no attachment reads zero | census: still refused (`path_blit`) | port |
| 0016 specialized-name-links | `specializedName` links | `LinkedFunction::with_specialized_name` | upstream |
| 0017 spares-within-the-address-space | spares held to 1/16 of the GPU VA | no spares | drop |
| 0018 specialized-linked-functions-closed | linked functions with constants | `LinkedFunction.constants` | check |
| 0019 pooled-block-size-from-the-environment | 16 MiB blocks | rt's unit is 16 MiB | upstream |
| 0020 hold-back-memory-under-pressure | no kept room under pressure | rt keeps one 16 MiB reserve per class | drop (bridge: nothing to hold back but its own pools) |
| 0021, 0023, 0024 Turnip DontCare stores | Metal's DontCare/MultisampleResolve stores as `DONT_CARE` (37 ms -> 9 ms a pass of 64 blended draws) | rt stores them (`pass.rs`: `STORE`) | port (a `DeviceOptions` field the bridge sets on Turnip) |
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
| `m2v-spare-command-pools.patch` (fix/gpu-blobs) | spare pools follow recent use | see Droid-VM 0022 | upstream (to confirm) |

## What is left (milestones 3 to 6)

- **3, the bridge**: written (below, "Milestone 3"); not yet in an add-on.
- **4, our fixes**: the **port** rows above, each its own patch here (0009, 0015, 0023, the queue priority, the
  DontCare stores).
- **5, the build**: `userland/metal/build.sh` with a switch that builds this bridge instead of the base's; the add-on
  with the camera plug-in.
- **6, the fallback**: both add-ons for the Mac, the row in `docs/switches.md`.

## Milestone 3: the bridge on rt (written, tested on the laptop; not yet in an add-on)

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

Not made by this bridge (each call fails with its reason in `nmb_last_error`): two-plane YCbCr textures over a surface
(`supportsYCBCRFormats12` answers NO), sparse heaps, a tessellation factor scale other than 1, patches drawn with a
control point index buffer, blit option `RowLinearPVRTC`, pipeline states' resource IDs (0; rt refuses indirect command
buffers on Turnip anyway).

Tests on the build laptop (2026-11-24), `userland/metal/rt/build.sh check`, the Intel GPU through ANV and the direct path:

```
$ out/m2v-rt/target/release/abi_smoke /usr/lib/libvulkan_intel.so     # 34 checks through NativeMetal's headers
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
