# External references: kernels and drivers that fit OneiOS's hardware

Survey date: 2026-11-10. Owner of this file: the references agent. Fetch the pinned repos with
`scripts/setup/get-references.sh` (`-l` lists them and their state; `external/` is git-ignored).

Scope: the Galaxy S24 Ultra (SM8650 "pineapple", Cortex-X4/A720/A520 with PAC, Adreno 750, GKI 6.1) running
iOS 27.0.1 (iPhone18,2, 24A446) under xnuhost. xnuhost is a process on One UI's kernel that gives XNU a
virtio-mmio board, so the "drivers that fit" are of two kinds: the **guest drivers** in the kernel collection
(they speak virtio) and the **host side** in xnuhost (it turns virtio into `/dev/kgsl-3d0`, SurfaceFlinger, evdev,
AAudio and sockets). Bare-metal drivers (GMU, SMMU, UFS, DSI, Wi-Fi radios) are not needed: Linux keeps the hardware.

Marks: **[F]** read in the named repo at the pinned commit. **[I]** this survey's inference. **[U]** unknown until
tried on the phone. Nothing here was run on the S24 Ultra; states are what each repo's own records say.

## 1. The finding that changes the plan

One published stack already runs **iPadOS 27.0.1 with Metal on an SM8650 / Adreno 750 phone**: Droid-VM's
guest, on a OnePlus Pad Pro ("5567", OPD2404, kernel 6.1.118, KGSL chip `0x43051401`, gpu id 6770) [F]
(`droidvm-meta-repo/PROGRESS.md`: 2026-11-04 "iPadOS 27 on 5567 (8 Gen 3, Adreno 750): Metal, hardware decode
and encode work"; 2026-11-08 "MetalCheck 41/41, GFXBench completes, UI and blur correct";
`plans/IOS_DRM2KGSL_MIGRATION.md` section 0). Same SoC, same GPU, same kernel generation and the same 24A446
build as ours. Its GPU path is:

```text
 Metal app -> NativeMetal plug-in -> metal2vulkan -> Turnip (darwin backend, tu_knl_darwin_virtgpu)
   -> IOKit user client 0x4149, methods 0x7200+ (NativeAdreno/DroidVMVirtgpuABI.h)
   -> guest kernel: UniXNU-droidvm package/DroidVMVirtioGpu3D.cpp (virtio-gpu, DRM native context, capset 6)
   -> host: virglrenderer src/drm/drm2kgsl/drm2kgsl_renderer.c (msm wire protocol served on KGSL ioctls)
   -> /dev/kgsl-3d0
```

Two consequences for OneiOS:

1. **Do not build on the KGSL proxy** (`crosvm --droidvm-gpu`, NativeAdreno's own 0x4147 user client). Droid-VM
   retired it on 2026-10-25: `UniXNU-droidvm/package/DroidVMGpu.cpp` says the proxy "is gone", and the virtio-gpu
   path measured about twice its speed (GFXBench ALU 2: about 50 fps against about 22) with the same 41/41
   MetalCheck [F] (`IOS_DRM2KGSL_MIGRATION.md`, M6). NativeAdreno's hardware classes are stubs in that guest.
2. **xnuhost's virtio-gpu device needs the 3D half**, and the host half already exists as one MIT file.
   xnuhost's device (then `virtio_gpu.c`) was 2D only on 2026-11-10 and said Metal "goes another way" (since then the 3D half is in: `runtime/xnuhost/src/virtio_gpu.rs` and `src/gpu3d_kgsl.rs` on drm2kgsl, Rust since 2026-11-23, [gpu.md](gpu.md)); the proven way is the
   one above. With it, the guest's GPU drivers, the plug-in and Turnip are taken unchanged.

## 2. Use this for ...

### GPU (Metal on the Adreno 750)

| Layer | Take | From (pin in `get-references.sh` unless noted) | Licence | Fit |
| --- | --- | --- | --- | --- |
| Host: KGSL | `src/drm/drm2kgsl/drm2kgsl_renderer.c` (3081 lines), `msm_kgsl.h`, with virglrenderer's `drm_context`, `drm_fence`, `msm/msm_proto.h` | `virglrenderer` 7e17c8e (Droid-VM, `wip/hackintosh`) | MIT | **Direct**: runs on SM8650 KGSL today. Link it (or the whole library with only the drm renderer) into xnuhost and feed it from the virtio-gpu device |
| Host: device model | `devices/src/virtio/gpu/{protocol.rs,virtio_gpu.rs,boot_pool.rs}`, `rutabaga_gfx/src/virgl_renderer.rs`, `aarch64/src/fdt.rs` (the drm-host pool node) | `crosvm` f82707d | BSD-3-Clause | Reference for the device in xnuhost (`runtime/xnuhost/src/virtio_gpu.rs`; C until 2026-11-23): capset 6, `CTX_CREATE`, `RESOURCE_CREATE_BLOB` (HOST3D, HOST3D_GUEST + `CREATE_GUEST_HANDLE`), `MAP_BLOB` answered with `MAP_INFO_POOL`, `SUBMIT_3D` with ring fences |
| Guest kernel: transport and user client | `package/DroidVMVirtioGpu.cpp`, `DroidVMVirtioGpu3D.cpp`, `DroidVMVirtioGpuDisplay.cpp`, `DroidVMGpu.cpp`; `ipados27/scripts/build-droidvm-gpu.py`, `link-droidvm-gpu-sources.py` | `UniXNU-droidvm` f9121cd (already in `kernel/pins.sh`) | BSD-2-Clause-Patent | **Direct**: unchanged if xnuhost's device behaves as crosvm's |
| Guest kernel: accelerator | `NativeAdrenoUserClient.cpp`, `NativeAdrenoPublish.cpp`, `NativeAdrenoSubmit.cpp`, `NativeAdrenoMemory.cpp`, `NativeAdrenoAccelerator.cpp`, `DroidVMVirtgpuABI.h`, `NativeAdrenoABI.h` | `NativeAdreno` 627836d (= `drivers/Adreno740` of the kernel base 62d0b11) | MIT | **Direct** (only these files; the GMU, SMMU, ring and firmware files are sheng's hardware) |
| Guest kernel: policy | `droidvm-gpu-sandbox-first.patch`, `droidvm-gpu-grant-apps.patch`, `droidvm-surface-gpu-wait.patch`, `droidvm-osmetaclass-adopt-superclass.patch`, `zzw-native-sandbox-grant-patterns.patch` | `UniXNU-droidvm` | BSD-2-Clause-Patent | Already in `kernel/device/patches/xnu-12377/series` |
| User space: plug-in | `NativeAdrenoMetalPlugin.mm`, `NativeBridgeMetalFrontend.mm`, `bridge/`, `shaders/`, `tools/` | `NativeMetal` b0d6a07 (= `drivers/AdrenoMetal` of the kernel base) | MIT | **Direct** for the API; its QuartzCore and RenderBox hooks are found per shared cache, so they must be re-found in the iPhone18,2 cache (`macOS-DroidVM/docs/TODO-gpu.md`) |
| User space: translator | metal2vulkan `624bab3` (`development`) + `patches/metal2vulkan/0001..0038` | steelbrain/metal2vulkan, patches in `Droid-VM-UniXNU` 62d0b11 (pin: `scripts/data/gpu-metal-vulkan-inputs.json`) | GPL-2.0 / LGPL-3.0 parts | Direct. In the table as `metal2vulkan` since 2026-11-11 (`userland/metal/build.sh` builds it; the shallow clone is 1.8 GB with its fixtures). Never linked into the kernel. Kept at `624bab3` on 2026-11-23: upstream `development` (`6462199fa`) replaced this runtime with a new one (`5d725aeec`), 1 of the 51 bridge patches applies there and the bridge names 47 types it has not (`docs/gpu.md` 7, "metal2vulkan's upstream"); OneiOS adds `userland/metal/m2v-spare-command-pools.patch` and `m2v-idle-memory.patch` |
| User space: translator, the rt port | metal2vulkan `6462199fa` (`development`, 2026-11-23) + `userland/metal/rt/patches` | steelbrain/metal2vulkan (the same clone: `userland/metal/rt/build.sh source` fetches `development` and makes a worktree in `out/m2v-rt/src`) | GPL-2.0 / LGPL-3.0 parts | Direct. The port of the bridge to `metal2vulkan-runtime` 0.2 (`userland/metal/rt/README.md`); not in an add-on yet |
| User space: Vulkan driver | Mesa: Droid-VM/mesa `cecc0e9` (`droidvm`; the pin of `scripts/data/adreno-compiler-inputs.json`, in the table as `mesa-droidvm`; `gpu-metal-vulkan-inputs.json` names upstream `c823a83e` as the reference) + `patches/mesa/*` (the 30 that file lists: `turnip-darwin-kmd`, `turnip-darwin-virtgpu`, `-virtgpu-resets`, `-lazy-bos`, `-bo-slot-before-reuse`, A8xx backports); built by `scripts/build-turnip-darwin.py`, plug-in by `build-adreno-metal-plugin.py`, `build-adreno-metal-bridge.py`, staged by `add-gpu-plugin-data.py` | Mesa upstream, patches in `Droid-VM-UniXNU` | MIT | Direct: A750 (`0x43051400` family) is in upstream Mesa's device table. Check that the build has the A750 fix Droid-VM carries in its phone-side Turnip (BR-only `RB_CCU` registers, 9452b5e9090: without it KGSL logs `CP: AHB bus error` every frame, `plans/ACCEPTANCE_REPORT_20260816.md`) |
| Tests | `package/metalcheck`, `package/gfxprobe`, `package/gfxbench` | `UniXNU-droidvm` | BSD-2-Clause-Patent | The acceptance tests of docs/plan.md M5 |

What xnuhost has to supply for drm2kgsl, as crosvm does [F] (`drm2kgsl_renderer.c` header and lines 300-1020,
2419, 2821, 2880):

- one KGSL **VBO** per context covering the guest's whole GPU VA range (`KGSL_MEMFLAGS_VBO`,
  `IOCTL_KGSL_GPUMEM_BIND_RANGES`); on SM8650 the probe gets 128 GiB, cut into 64 slices of 2 GiB (one per GPU client);
- guest pages as dma-bufs: **`/dev/udmabuf`** windows over the guest RAM memfd (sealed `F_SEAL_SHRINK`), imported
  with `GPUOBJ_IMPORT` (`KGSL_USER_MEM_TYPE_DMABUF`); host-allocated BOs from `/dev/dma_heap/system`;
- the guest's fence numbers passed through as KGSL timestamps (`KGSL_CONTEXT_USER_GENERATED_TS`);
- the 8 MiB "drm-host" pool for the contexts' 16 KiB response shmems, published in the device tree as
  `/arm-io/virtio-gpu` `drm-host-pool` (`UniXNU-droidvm/ipados27/scripts/make-droidvm-adt.py`, crosvm `--pre-alloc drm-host-mb=8`);
- environment switches worth keeping: `DRM2KGSL_PRIORITIES=3` (render server on its own ringbuffer),
  `DRM2KGSL_PWR_CONSTRAINT`, `NCTX_NO_WAITTS`.

### Frame rate and camera code taken from other ports (fix/s24-fps-heat-camera, 2026-11-25)

| Ours | Taken from | Licence of the file | What |
|---|---|---|---|
| `userland/metal/DisplayLinks.inc` | [qiumochen/UniXNU-zizhan](https://github.com/qiumochen/UniXNU-zizhan) 42629e5 `userland/ui_compat18.c` `display_link_rates` (itself from hakios 7caa457) | SPDX MIT in the file (the repo's LICENSE is BSD-2-Clause-Patent) | the in-process animation manager's display links at the panel's rate; matched by part of the class name for iOS 27's AnimationKit, all three link makers hooked |
| `userland/metal/SnapshotFormat.inc` | [rafsanbasunia/ios-garnet](https://github.com/rafsanbasunia/ios-garnet) f6276db `userland/src/inject_sb.c` `snapshot_output_format` | MIT (the HakiOS authors; `userland/src/` of its two-licence LICENSE) | SpringBoard's app snapshots JPEG or PNG, not ATX |
| `userland/camera/OneiOSCaptureDevice.c` `post()` | [jesvi-eulah/Jesvi-iOS](https://github.com/jesvi-eulah/Jesvi-iOS) a13e406 `userland/src/camera_device.c` `post()` | MIT (the HakiOS authors) | the stream's Start and Stop notifications |

Ideas only, no code: zizhan's ASTC on (`73df2a4`, `e3be892`; ours is the loader's `astc=` with NativeMetal's own
layout) and its 60..120 reading of Core Animation's rates (our `sheng-vsync-min`).

### GPU (Metal on the Galaxy A56's Xclipse 540, iOS 16.5; added 2026-11-24)

The A56 port's design and what each stage takes: [a56-gpu.md](a56-gpu.md) section 1. In short:

| Layer | Take | From (pin in `get-references.sh`) | Licence | Fit |
| --- | --- | --- | --- | --- |
| Metal on Gallium, no Vulkan | `tools/mesa/air/` (AIR frontend, `air_metal.c`), `userland/src/redwoodmetal.m`, `metal_plugin.plist` | `Jesvi-iOS` a13e406 (main table) | MIT (outside `xnu/`; `xnu/` is APSL) | Ported onto radeonsi instead of Panfrost; its notices kept. Its CoreAnimation render server stayed off Metal (a56-gpu.md 1). No `prebuilt/`, no `.py` |
| Host: amdgpu native context | `runtime/xnuhost/amdgpu/src/amdgpu_renderer.c`, `src/gpu3d_xclipse.c` (commit `e7d56f2`) | `OneiOS-A56` c83b9ca (main table) | MIT per file (the repository states none) | Rewritten in Rust as `runtime/xnuhost/src/gpu3d_amdgpu.rs`; nothing copied as is |
| Guest: amdgpu virtio, radeonsi, ACO | `src/amd/common/virtio`, `src/virtio/vdrm`, `src/gallium/drivers/radeonsi`, `src/gallium/winsys/amdgpu` | `mesa-droidvm` cecc0e9 | MIT | Built for Darwin arm64 (iOS 16.5) with a Darwin vdrm backend over `DroidVMVirtgpuABI.h` |
| Guest: libdrm for Darwin | `xf86drm.h`, `include/drm`, `amdgpu/` (the VA manager) | libdrm 2.4.133, the release Mesa's `subprojects/libdrm.wrap` pins (sha256 checked; fetched by `userland/metal-amd/build.sh`, not by `get-references.sh`) | MIT | Headers and libdrm_amdgpu built for iOS 16.5; the DRM calls are `src/oneios/oneios_drm.c` of the Mesa patches (a56-gpu.md 3) |
| Ideas only | Panfrost on Darwin (Pixel 6a); a HakiOS XNU; a Mali driver for XNU | `pixelios-kernel` f084ce5, `pixelios-userland` d5b2079 (MIT); `hakios-xnu-kernel` 62e09f8 (no licence); `mapple_gpu_mali` f893fed (GPL-3.0) (extra table) | as said | read only; nothing of the last two goes into our files |

### Display

| Take | From | Licence | Fit |
| --- | --- | --- | --- |
| `NativeLegacyDisplay.cpp`, `NativeMobileDisplay.cpp`, `NativeGOPPresenter.cpp`, `OriginalMobileFramebufferABI.h` under the original `IOMobileFramebufferLegacy`; built by `ipados27/scripts/build-droidvm-legacy-display.py` with `legacy-display-24A446.json` | `NativeMobileDisplay` ed08bd4 (fork of UniXNU/GopFramebuffer; = `drivers/GopFramebuffer` of the kernel base) | BSD-2-Clause-Patent | **Direct** |
| `package/DroidVMVirtioGpuDisplay.cpp` (scanout client: retains each IOSurface's pages until `RESOURCE_UNREF`), `LegacyDisplayProbe.cpp` | `UniXNU-droidvm` | BSD-2-Clause-Patent | **Direct**; xnuhost's `virtio_gpu.rs` (`virtio_gpu.c` until 2026-11-23) is its device |
| Boot arguments `offline-legacy-*`, `droidvm-display-hz=120`, `ca-triple-buffer=1`, `offline-legacy-unit-scale` | `UniXNU-droidvm/ipados27/board.toml` `[payload] kernel_args` | | Direct; the scale is the iPad's @2x rule, an iPhone18,2 wants @3x (userland's `device-iphone18,2.toml`) |
| Host scanout without a copy: `plans/SCANOUT_INPUT_REWIRE.md`, `DISPLAY_PIPELINE_DECOUPLING.md`; the guest frame is a dma-buf the host presents | `droidvm-meta-repo` a18ec72 | GPL-2.0-or-later (docs) | Design reference for the SurfaceControl layer: with drm2kgsl the frame is already a KGSL-imported buffer, so an AHardwareBuffer/SurfaceControl present needs no CPU copy [I] |

### Input

| Take | From | Licence | Fit |
| --- | --- | --- | --- |
| `NativeHidInput.cpp` (digitizer, Home/side buttons, keyboard, pen) under the original IOHIDFamily; `ipados27/scripts/build-droidvm-hid-input.py`, `hid-device-24A446.json`, `package/HidInputProbe.cpp` | `NativeHidInput` 24f1f3f (UniXNU/ShengHidInput at the commit UniXNU-droidvm's `build-droidvm-hid-input.py` pins for iPadOS 27: its arm64e source; the feed's 5f3a37c is the arm64 iOS 17 one) | BSD-2-Clause-Patent | **Used**: `kernel/patches/oneios-hosted/hid` stages it for the phone; the layout of the original class is that folder's `OriginalHidDeviceABI.h` (the iPad fixture's addresses do not apply; its discriminators are the same) |
| `IOHIDFamily/IOHIDDevice.h`, `IOHIDDeviceTypes.h`, `IOHIDDevice.cpp`, `IOHIDEventDriver.cpp`: the declarations `OriginalHidDeviceABI.h` is reduced from, and what the family does with a report | `IOHIDFamily` e31e90a (apple-oss-distributions, tag IOHIDFamily-2238.120.5, the OS profile's pin) | APSL-2.0 | Reference: read while writing the header and when a HID boot stops; nothing is compiled from it |
| `package/DroidVMVirtioInput.c` (virtio-input: multi-touch protocol B, pen, trackpad, keys, motion sensors, vibrator), `DroidVMMotion.c`, `DroidVMSensorProcessor.cpp` | `UniXNU-droidvm` | BSD-2-Clause-Patent | **Direct**: evdev events map one to one onto virtio-input, so xnuhost's `virtio_input.c` forwards `/dev/input/event*` |
| Boot arguments for iOS 27: `sheng-touch-split` (the touch screen must be a HID device of its own: backboardd takes digitizer events only from a Digitizer service) | `UniXNU-droidvm/ipados27/board.toml` | | Direct. Drop the iPad parts: `DroidVMTrackpadHid.inc`, `AppleTopCaseDriverV2` roots, pencil |
| Sensor, GNSS, haptics host notes | `droidvm-meta-repo/todo/input-sensors.md` | | Reference |

### Audio

| Take | From | Licence | Fit |
| --- | --- | --- | --- |
| `package/DroidVMAudio.cpp`: two `IOAudio2Device`s with UIDs "Speaker" and "Mic", which VirtualAudio turns into routable ports; `DroidVMVirtioSnd.c`: the virtio-snd transport (48 kHz S16, 240-frame periods) | `UniXNU-droidvm`, built by `ipados27/scripts/build-droidvm-devices.py` | BSD-2-Clause-Patent | **Direct**; the host side is new: a virtio-snd device in xnuhost over AAudio (crosvm's is `devices/src/virtio/snd/`; its AAudio helper is in the DroidVM app, GPL-3.0, not pinned here) |
| Why UID "Codec" is the wrong target, DSP chain notes | `droidvm-meta-repo/todo/audio-media.md`, `macOS-DroidVM/docs/k27-audio.md` | | Reference |
| The speaker's volume curve, -36 x (1 - s)^2 dB (`kMinDb`, `toDb`): a third of full volume about -16 dB instead of -42.5 dB on our linear -64..0 dB range (half: -9 instead of -32 dB) | [UniXNU/UniXNU-sheng](https://github.com/UniXNU/UniXNU-sheng) c8f92b2 `hal/ShengAudioPlugIn.c` 204-210 (iPadOS 27.0.1 24A446) | BSD-2-Clause-Patent | **Taken** (2026-11-25, fix/audio-volume-rt): the staged `DroidVMAudio.cpp`'s gain per step (`staged/audio.rs` block "fix-audio-curve", `oneios-audio-curve`), docs/hardware.md 6 "The volume curve"; CREDITS.md |

### Network

| Take | From | Licence | Fit |
| --- | --- | --- | --- |
| `package/DroidVMVirtioNet.c` (virtio-net transport, interrupt-driven receive), `DroidVMVirtioEthernet.cpp` + `DroidVMVirtioNetIf.cpp` (IOEthernetController on IONetworkingFamily-177: `en0`, DHCP by iOS itself) | `UniXNU-droidvm` | BSD-2-Clause-Patent | **Direct**; host side: a virtio-net device in xnuhost on a tap (takeover) or user-mode NAT (overlay) |
| `package/DroidVMWifi.inc`: an `IO80211Controller` with no radio so wifid and Settings have a Wi-Fi interface; `droidvm-wifi-ssid=NAME` shows one joined network | `UniXNU-droidvm`; request layouts from `UniXNU-sheng` `package/WifiNet.cpp` (extra table) | BSD-2-Clause-Patent | Direct. A real bridge to One UI's Wi-Fi state (SSID, signal) is new work |
| IONetworkingFamily-188's headers (`IOEthernetController.h`, `IOEthernetInterface.h` and what they include), to compile subclasses of the kernelcache's original classes | [apple-oss-distributions/IONetworkingFamily](https://github.com/apple-oss-distributions/IONetworkingFamily) `bb910e2` (tag `IONetworkingFamily-188`), cloned by `oneios-tool kernel gen net` into `out/sources/IONetworkingFamily` | APSL 2.0 | Headers only; checked slot by slot against the kernelcache at each build (`kernel/patches/oneios-hosted/net/README.md`) |
| The supplicant's stable AIDL interface, version 2 (method order = transaction codes, parcelables' fields), and libbinder's wire format (`Parcel.cpp`, `Status.cpp`, `Stability.cpp`) | AOSP `hardware/interfaces/wifi/supplicant/aidl/aidl_api/android.hardware.wifi.supplicant/2` (hash `5e5c6d9e...`, the one the phone's supplicant answers), `frameworks/native/libs/binder` | Apache 2.0 | Read, not copied: `runtime/bridges/wifi/supplicant.rs` holds the numbers, `runtime/bridges/hal/aidl.rs` and `hal/binder.rs` the format (`supplicant.h` and `aidl_parcel.c` until 2026-11-23) |
| 24A446's association request (656 bytes) and ASSOC_DONE event (540 bytes), the ASSOCIATE_RESULT codes 9 and 21, the key ciphers 6 and 10 | `UniXNU-sheng` `package/WifiNet.cpp` (6bc0ce1), a driver that joins real networks on this OS build | BSD-2-Clause-Patent | Layouts and numbers taken into `kernel/patches/oneios-hosted/net/OneiOSWifi.cpp` |
| The Skywalk UART pipe iOS 27's bluetoothd opens (`com.apple.uart.sk.bluetooth`: the control's options, the kernel pipe provider, the ring rules) | [UniXNU/QcaBluetooth](https://github.com/UniXNU/QcaBluetooth) `986d631` `src/qbt_transport.c` (from daimile2's Renoir XNU Bluetooth); `native-ios27.md` for what its bluetoothd adapter changes | APSL-2.0 (that file; the repository's Linux parts are GPL-2.0 and are not used) | Adapted into `drivers/bluetooth/oneios_bt_sk.c` (APSL-2.0) with the UART owner replaced by the bridge's transport core. Its GENI UART, firmware and bluetoothd patches are not used: the phone's HAL owns the controller |

### Kernel, CPU, storage, power

| Take | From | Licence | Fit |
| --- | --- | --- | --- |
| XNU base and iPadOS 27 patch series | `Droid-VM-UniXNU` 62d0b11, `UniXNU-droidvm` f9121cd (both in `kernel/pins.sh`) | BSD-2-Clause-Patent | In use. The pins are the newest heads as of this survey |
| PAC facts for this SoC: all three core types pass (`ID_AA64ISAR2` APA3=5, GPA3=1; SVE and SME hidden) | `droidvm-meta-repo/plans/IPADOS27_BUMP.md` | | Confirms the owner's rule: arm64e as Apple built it; no PAC-stripping port |
| Disks: `NativeVirtioBlock` + patches `ipados27/patches/VirtioMmioBlock/0001..0006`, `DroidVMDataStore.c`, `DroidVMSourceStorage.cpp`, `build-droidvm-source-storage.py`, root policies (`build-droidvm-root-policies.py`) | `UniXNU-droidvm`; UniXNU/VirtioMmioBlock, RamBlock, VerifiedRoot | BSD-2-Clause-Patent | **Direct** against xnuhost's `virtio_blk.c` |
| `DroidVMBattery.cpp` (goldfish battery -> `IOPMPowerSource`), `DroidVMCpufreq.c` (XNU's perf-control class -> host uclamp per vCPU thread) | `UniXNU-droidvm`; device side `crosvm/devices/src/bat.rs`, `virtcpufreq_v2.rs` | BSD-2-Clause-Patent / BSD-3-Clause | **Direct**; cpufreq is what lets the Linux governor follow the guest's bursts (0.2-7 ms instead of 40-116 ms) |
| `AppleKeyStoreState`, `SoftwareAES` (`DroidVMSoftwareAES.cpp`, `SoftwareAES-24A446.S`), `KeyStoreProbe.cpp`, `KeyStoreWrap.inc` | UniXNU components + `UniXNU-droidvm`. Both components are submodules of the kernel base at its pin (`external/Droid-VM-UniXNU/drivers/AppleKeyStoreState` 42ff83e, `drivers/SoftwareAES` 4a45326), checked out by `kernel/lib/stage.sh`; no row of their own in `get-references.sh` | BSD-2-Clause-Patent | **In use**, no SEP: `kernel/patches/oneios-hosted/aes` and `keystore` (the service and its state answers are the component's; the class keys and the key wrap are OneiOS's, where Droid-VM's `KeyStoreWrap.inc` only adds a header) |
| Video codecs: `DroidVMVTHost.cpp`, `DroidVMVTParavirt.cpp`, `DroidVMVirtioMedia.c` over virtio-media | `UniXNU-droidvm`; Droid-VM/virtio-media `32d7366` | BSD-2-Clause-Patent | **In use** (hevc-encoder, 2026-11-15): the three files are staged into the `codec` archive (`kernel/patches/oneios-hosted/codec`); since 2026-11-25 `DroidVMVirtioMedia.c` with edit 7, its event queue on the device's interrupt, after UniXNU-droidvm `96e8c1f` (lateautumn233, completion interrupts instead of polling) and `2cd4c8e` (HuJK, either ADT name for the interrupt, polling where there is none), both read at origin, newer than the f9121cd pin (fix/codec-irq); the host side is OneiOS's own virtio-media device + MediaCodec helper (`runtime/bridges/encoder`), so iOS's paravirtualized VideoToolbox encodes HEVC and CMPhoto writes HEIC. Host done and PC-tested; the guest archive's generator and a kernel build are the next step (`docs/hardware.md` 7g) |
| iPhone identity on iOS 27: `profiles/os/ios-27.0.1-24A446-iphone17,1/profile.toml` | `UniXNU-shennong` 363cc77 (extra table) | none stated: read only | The only other public iPhone iOS 27.0.1 profile; compare its prelink pins and root list with `kernel/unixnu/profiles/os/ios-27.0.1-24A446`. Its board stops in `_arm_init` |
| Debug shell and tools in the guest | `ownervisor` 65fc2c7, `applebox` 6e3b471 | MIT / Apple open-source licences | Direct (userland overlay, owner trust bundle) |

## 3. Ranked survey

Rank is usefulness for the S24 Ultra under xnuhost. "State" is the repo's own claim.

| # | Repo and pin | Licence | Drives | SoC / GPU | State | Fit |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | [Droid-VM/UniXNU-droidvm](https://github.com/Droid-VM/UniXNU-droidvm) f9121cd `wip/hackintosh` | BSD-2-Clause-Patent | Guest drivers for GPU, display, input, audio, net, disks, battery, cpufreq, codecs; XNU patches; ADT | crosvm on Gunyah; hosts SM8650/A750 and SM8850/A840 | iPadOS 27.0.1 to the home screen with Metal, sound, network, persistent Data | Direct (the kernel base) |
| 2 | [Droid-VM/virglrenderer](https://github.com/Droid-VM/virglrenderer) 7e17c8e `wip/hackintosh` | MIT | GPU: msm protocol on KGSL | A7xx and A8xx KGSL; measured on SM8650 | Works (daily path of the guest above) | Direct: host side of xnuhost's GPU |
| 3 | [Droid-VM/NativeAdreno](https://github.com/Droid-VM/NativeAdreno) 627836d | MIT | GPU user client and memory in the guest kernel | any (virtio-gpu); native part A740/SM8550 | Works | Direct (four files and the ABI header) |
| 4 | [Droid-VM/NativeMetal](https://github.com/Droid-VM/NativeMetal) b0d6a07 | MIT | Metal plug-in | Turnip targets | MetalCheck 41/41 on A750, WebGL 2, WebGPU samples | Direct; hooks re-found for the iPhone cache |
| 5 | [Droid-VM/UniXNU](https://github.com/Droid-VM/UniXNU) 62d0b11 `wip/droidvm` | BSD-2-Clause-Patent | XNU arm64e series, Mesa and metal2vulkan patch sets, build scripts | | Builds the guest above | Direct (in `kernel/pins.sh`) |
| 6 | [Droid-VM/NativeMobileDisplay](https://github.com/Droid-VM/NativeMobileDisplay) ed08bd4 | BSD-2-Clause-Patent | IOMobileFramebuffer backend, vsync, direct scanout | any framebuffer | Works | Direct |
| 7 | [UniXNU/ShengHidInput](https://github.com/UniXNU/ShengHidInput) 24f1f3f | BSD-2-Clause-Patent | HID digitizer, buttons, keyboard | any | Works on iOS 27 with `sheng-touch-split` | Direct |
| 8 | [Droid-VM/crosvm](https://github.com/Droid-VM/crosvm) f82707d `wip/hackintosh` | BSD-3-Clause | The device side of every virtio device above | | Works | Reference for xnuhost's devices (C until 2026-11-23, Rust since) |
| 9 | [Droid-VM/droidvm-meta-repo](https://github.com/Droid-VM/droidvm-meta-repo) a18ec72 `wip/hackintosh` | GPL-2.0-or-later with extra permissions | Plans, measurements, guest Mesa patches | SM8650, SM8750, SM8850 | Current to 2026-11-10 | Reference; `plans/IOS_SUBSYSTEM_ANDROID_FEASIBILITY.md` weighs our design (it advises a small ABI test bed first) |
| 10 | [Droid-VM/macOS-DroidVM](https://github.com/Droid-VM/macOS-DroidVM) 77f7614 | none stated: read only | macOS 27 guest: xnu-12377 port notes, GPU TODO, vtpv contract, Linux toolchain layer | | Boots to the desktop | Reference |
| 11 | [Droid-VM/gunyah_host_mod](https://github.com/Droid-VM/gunyah_host_mod) 44e8ae5 | GPL-2.0 | Host modules: `udmabuf` for GKI 6.1, Gunyah share | GKI 6.1/6.6/6.12 | Works on OPD2404 6.1.118 | Only if modules load on this phone [U] |
| 11a | [UniXNU/XnuPrelink](https://github.com/UniXNU/XnuPrelink) 9b950f2 (`get-references.sh XnuPrelink`, `external/XnuPrelink`) | see repo | `kernel/prelink/build.sh`'s matcher and fileset emitter; from 2b7baca it retargets the hosted archives' `<stub>.original` branches | iPhone18,2 24A446 imports bound (f1d80d3, 9b950f2) | Builds the phone's `kernel.kc` | **Use it**: the kernel base's submodule (b74ed7c) leaves the 507 stubs pointing past the collection (`kernel/prelink/README.md`, "On the phone") |
| 12 | [UniXNU/UniXNU](https://github.com/UniXNU/UniXNU) 1446f44 (head 00f47f6) | BSD-2-Clause-Patent | Upstream of 5: DarwinBoot, XnuPrelink, VerifiedRoot, AdrenoMetal, Adreno740, AirNir, AdrenoPM4 | SM8550/A740 native | Metal fully working on sheng | Reference; Droid-VM's forks carry what we use. No SM8650 or A750 component exists in the org |
| 13 | [YX2748198202-lab/UniXNU-shennong](https://github.com/YX2748198202-lab/UniXNU-shennong) 363cc77 | none | Bare-metal port; an iPhone17,1 iOS 27.0.1 profile | SM8650 | Stops in `_arm_init` on iOS 27 | Profile comparison only |
| 14 | [lonzzi/UniXNU-houji](https://github.com/lonzzi/UniXNU-houji) 99beb10 | mixed | Bare-metal A750: GMU, real command submission, desktop on iOS 16 | SM8650 / A750 | Partial (frame drops, colour faults) | Not needed (KGSL drives the GPU); proof that A740 code ports to A750 |
| 15 | [tycykp/UniXNU-OnePlus-Ace5](https://github.com/tycykp/UniXNU-OnePlus-Ace5) aaa100d, [kerrinz/UniXNU-zorn](https://github.com/kerrinz/UniXNU-zorn) ef13dcc, [Asultop/UniXNU-NX769J](https://github.com/Asultop/UniXNU-NX769J) 2a265af, [loolooc/UniXNU-meizu21](https://github.com/loolooc/UniXNU-meizu21) f6b6aab, [Axiang-dev/UniXNU-caihong](https://github.com/Axiang-dev/UniXNU-caihong) 1e3d718, [SangLuoCN/UniXNU-Nubia-Z60U](https://github.com/SangLuoCN/UniXNU-Nubia-Z60U) df60483, [peiyuesheng666/UniXNU-PHY120](https://github.com/peiyuesheng666/UniXNU-PHY120) 0dfaf5f, [Fearless109/hakios-sm8650](https://github.com/Fearless109/hakios-sm8650) ea80367 | BSD-2-Clause-Patent or none | Bare-metal SM8650 boards (UEFI, UFS, touch, Wi-Fi on giulia) | SM8650 | iOS 16 / iPadOS 17 desktop at best, CPU-rendered; no Metal | Not useful under xnuhost |
| 16 | [ediwiuwj/tahoe-on-linux](https://github.com/ediwiuwj/tahoe-on-linux) 2595c97 | MIT | macOS 26 x86_64 userland on Linux, BSD and Mach traps translated in user space | PC | launchd as pid 1, about 400 services, WindowServer starts | Design reference for the trap layer (approach 1a of docs/plan.md) |
| 17 | [YahiaAngelo/vphone-linux](https://github.com/YahiaAngelo/vphone-linux) 9c75e88 | see repo | reims-vgpu + metal2vulkan for Apple's own ParavirtualizedGraphics | QEMU | Works on PCs | Fallback GPU path only |
| 18 | [oosexgang/hakios-xnu-kernel](https://github.com/oosexgang/hakios-xnu-kernel) 63fc2b4 (read at 62e09f8, 2026-11-25) | MIT for its own work, APSL-2.0 for what extends XNU or came from HakiOS (its README "Licence"; GitHub detects none) | HakiOS: own loader, drivers and "hakios-metal" (a Metal driver without Vulkan: 110 fps page swipes on A642L) | SM7325 | iOS 16.5 | Watch: a second Metal design, bare-metal and xnu-8796 today |
| 19 | [dipa-studio/ios-sweet](https://github.com/dipa-studio/ios-sweet) 45263be and its forks | MIT | xnu-8796 bare-metal, iOS 16, CPU-rendered | SM7150 and many others | Home screen | Not useful: no PAC userland, no GPU through Linux |
| 20 | [DenialOfGravity/ios-redfin](https://github.com/DenialOfGravity/ios-redfin) 23019ab4 `main` (cefcd91 `redfin-int`) | MIT | `phone/sensors/`: a Qualcomm SEE client (QMI service 400 over QRTR, protobuf), sensors as SPU HID services (0971, 0972), auto-brightness's cached re-post and True Tone (1298) | SM7250 Pixel 5 | iOS 16.7.16: every sensor, auto-brightness, cameras, 2-way audio | **Ported** (fix/sensors-als, 2026-11-25; not booted): `sns.c`, `pb.c`, `phone/wifi/qmi.c` and `sensor.c`'s sequence onto Linux's `AF_QIPCRTR` (`runtime/bridges/sensors`), `test_sensd.c`'s byte vectors (`sensors/tests.rs`), 1298's cached re-post (`drivers/brightness/OneiOSALSDevice.cpp`); [ports-2026-11-25-hw.md](ports-2026-11-25-hw.md) sections 1-2 |
| 21 | [coreycusick/ios-guacamole](https://github.com/coreycusick/ios-guacamole) 53e6c5e `ios27` | MIT (camera files HakiOS MIT) | `userland/camera/`: an iOS 27.2 FigCaptureDevice plug-in and `capture_hooks.c` (the capture graph's start fixes) | SM8150 OnePlus 7 Pro | iOS 27.2: live preview and photos (2026-11-18), speakers | **Port** (2026-11-25): camera input for `fix/s24-fps-heat-camera`, [ports-2026-11-25-hw.md](ports-2026-11-25-hw.md) section 3 |

The per-area plan for sensors, auto-brightness, camera, audio and Bluetooth taken from these and from UniXNU-sheng
(`hal/ShengAudioPlugIn.c`'s volume curve, iPadOS 27.0.1) and UniXNU-droidvm (its motion HID classes) is
[ports-2026-11-25-hw.md](ports-2026-11-25-hw.md) (2026-11-25, research only: nothing ported then; the volume curve was taken
the same day on fix/audio-volume-rt, section 2 "Audio" above).

**Ported for the sensors (fix/sensors-als, 2026-11-25; host-tested and built, not booted):**

| Source @ commit, file | Licence | Here |
| --- | --- | --- |
| DenialOfGravity/ios-redfin 23019ab4 `phone/sensors/sns.c`, `pb.c`, `sensor.c`, `test_sensd.c`, `phone/wifi/qmi.c` (Copyright (c) 2026 dipa-studio) | MIT | `runtime/bridges/sensors/sns.rs`, `pb.rs`, `mod.rs`, `tests.rs` (MIT): the SEE client on Linux's QRTR |
| ios-redfin 23019ab4 `xnu/patches/1298-redfin-auto-brightness-true-tone.patch` (the cached sample re-posted within 15 ms of a wake), `0971` (the SPU usage pairs, `setIntegerValue` of the illuminance) | MIT | `drivers/brightness/OneiOSALSDevice.cpp` (MIT), `kernel/patches/oneios-hosted/hid/OneiOSALSEvents.cpp` |
| Droid-VM/UniXNU-droidvm f9121cd `package/DroidVMMotionHid.inc`, `DroidVMSensorProcessor.cpp`, `DroidVMMotion.c` (row 1, the pinned checkout) | BSD-2-Clause-Patent | staged into the HID archive by `oneios-tool kernel gen hid` (`staged/hid_motion.rs`: the compass kind, frame-driven ticks, the transport's magnetometer axes) |
| UniXNU/ShengHidInput 24f1f3f `NativeHidInput.cpp` (row 7): the event driver's vtable copy with its 220 checked discriminators | BSD-2-Clause-Patent | `kernel/patches/oneios-hosted/hid/OneiOSALSEvents.cpp` wraps the light sensor's event driver the same way (the table exported from the staged copy) |
| jesvi-eulah/Jesvi-iOS a13e406 `userland/src/inject_sensors.c` (pinned in `scripts/setup/get-references.sh`) | MIT (`userland/`) | what CoreBrightness checks on an ALS service (Built-In, VendorID 0x5ac, `ALSCurveInfo`, field 0xC000B): `drivers/brightness/OneiOSALSDevice.cpp` |

### The ios-sweet fork network

492 forks listed on 2026-11-10 (the API's counter says 900). Filtered by description and by forks/stars:

- **SM8650, Adreno 750, Galaxy S24: none.** Nearest: `LumenMarch/ios-nuwa` (SM8550), `palazik/ios-dodge` (SM8750,
  xnu-8796), `xNekom/ios-mondrian` and `vizPilot/ios-b4q` (SM8475).
- **Samsung:** `syncmodz/ios-s9` 646570b (Galaxy S9, SDM845, MIT: home screen, CPU-rendered, flashed to `boot` from
  TWRP), `tomaswarner/ios-dm3q` (S23 Ultra, SM8550) and `vizPilot/ios-b4q` (Z Flip 4): READMEs still the upstream
  text, no recorded boot. `chenzikang/z3q-port`, `dmyslisz/ios-a71`, Exynos ports (`ios-great-exynos`, `ios-o1s`,
  `ios-z3s-opus`). UniXNU side: `kmu1757/UniXNU-y2q` (Tab S7+/S20+, SM8250, iPadOS 18.7),
  `RigoLigoRLC/unixnu-adreno6xx` (Tab S7 FE, A642L). All need an unlocked bootloader and replace Android; none
  runs beside One UI.
- **Emulator forks** (`lgrachov/ios-qemu`, `alxb421/ios-vm`, `MRHRTZ/ios-emulator`): iOS 16 on QEMU's virt board.
- The ports iOSPlus10 lists (ios-flame, ios-sailfish, Bergamota/Chlorophyll, hackios, ios-topaz, ios-apollo,
  UniXNU-pong, UniXNU-zeta) are pinned in `/home/akshay/Documents/GitHub/iOSPlus10/scripts/setup/get-references.sh`
  and are not repeated here. For OneiOS: UniXNU-zeta is the non-PAC shared-cache rewrite the owner ruled out;
  sailfish/flame/hackios GPU work is Mesa freedreno + EAGL shims for iOS 16 on bare metal (superseded by row 4);
  UniXNU-pong (A730) and UniXNU-sheng (A740) are the native Turnip-on-XNU precedents the Droid-VM stack grew from.

The UniXNU catalogue (`Portsmith-iOS` 8794f29, `ports/catalog/unixnu-devices.toml`, 265 records) lists eight
SM8650 boards (row 15 and `houji` forks); none has Metal.

### Ports read for the GPU, heat and stability (2026-11-25)

Read for [ports-2026-11-25-gpu.md](ports-2026-11-25-gpu.md) (what to port, ranked, with switches and proof lines). No
pin changed; the commits are those read. Taken since (fix/s24-fps-heat-camera, 2026-11-25, not booted): Turnip's submit split
(`fb9fb47`, `userland/metal/turnip-submit-split.patch`), the reflection crash fix (`3c46b5e`, a frontend edit of
`ios metal-stage-edit`) and the write-only present buffers (`cdb297bbf`, rewritten in Rust in
`runtime/display/src/xhd/present.rs`; CREDITS.md).

| Repo, commit read | Licence | What is in it for us |
| --- | --- | --- |
| [Droid-VM/NativeMetal](https://github.com/Droid-VM/NativeMetal) `935c468` (`wip/hackintosh`, 153 commits past our b0d6a07) | MIT | purgeable discard, single-sample default, system shader cache, YCbCr planes, wallpaper freeze, the cameracaptured reflection crash fix `3c46b5e` |
| [Droid-VM/UniXNU](https://github.com/Droid-VM/UniXNU) `8789b8d` (`wip/hackintosh`, 32 past our 62d0b11) | BSD-2-Clause-Patent | the stack on metal2vulkan `555b13d5` (`421e982`); Turnip's submit split at command buffers (`fb9fb47`, the render-server start hang) |
| [Droid-VM/UniXNU-droidvm](https://github.com/Droid-VM/UniXNU-droidvm) `283f251` (70 past our f9121cd) | BSD-2-Clause-Patent | virtio-media and virtio-snd on completion interrupts (`96e8c1f`, `793bb45`, `2cd4c8e`) |
| [Droid-VM/virglrenderer](https://github.com/Droid-VM/virglrenderer) `2529a9f` (`wip/hackintosh`) | MIT | no guest queue in Android UI's KGSL ringbuffer (`5faa56f`) |
| [Droid-VM/NativeAdreno](https://github.com/Droid-VM/NativeAdreno) `cdaa0bb` | MIT | native sheng driver work only |
| [Droid-VM/droidvm-meta-repo](https://github.com/Droid-VM/droidvm-meta-repo) `1b5dda8` | GPL-2.0-or-later (docs) | PROGRESS.md: the measurements behind the rows above |
| [YumikoHayamato/OneiOS](https://github.com/YumikoHayamato/OneiOS) `273ed5e` (`s25-ultra-sm8750`) | none stated (a fork of this repo) | the display helper's write-only buffers (`cdb297bbf`); XNU in a crosvm VM under Gunyah on the S25 Ultra (`docs/native.md` there) |
| [qiumochen/UniXNU-zizhan](https://github.com/qiumochen/UniXNU-zizhan) `88f4eeb` | BSD-2-Clause-Patent | the GPU's lowest level with the display off (`17ccd80`) |
| [Allenwdk/UniXNU-uke](https://github.com/Allenwdk/UniXNU-uke) `55909f4` (`docs/GPU-IOS27.md`), [Allenwdk/AdrenoMetal](https://github.com/Allenwdk/AdrenoMetal), [UniXNU/AdrenoMetal](https://github.com/UniXNU/AdrenoMetal) | BSD-2-Clause-Patent; MIT; MIT | a 293-entry 24A446 priming list; the "R2507" runtime line |
| [DenialOfGravity/ios-redfin](https://github.com/DenialOfGravity/ios-redfin) `23019ab` (NOTES.md), [oosexgang/hakios-metal](https://github.com/oosexgang/hakios-metal) | MIT | native-driver ideas only; the ALS repost on wake (1298) |
| [superboyyy/hakios-arm64e-dada](https://github.com/superboyyy/hakios-arm64e-dada) `f1ae906` (`gpu2/`) | none stated | nothing new beyond 0101/0102 |
| [kerrinz/UniXNU-zorn](https://github.com/kerrinz/UniXNU-zorn), [tenku-png/UniXNU-caihong](https://github.com/tenku-png/UniXNU-caihong), [alpersamur3/asoft-ios-android](https://github.com/alpersamur3/asoft-ios-android), [UniXNU/UniXNU-sheng](https://github.com/UniXNU/UniXNU-sheng) | BSD-2-Clause-Patent; none; MIT; BSD-2-Clause-Patent | nothing to take for these areas |

## 4. What the S24 Ultra still lacks that nobody has published

1. **drm2kgsl without crosvm.** It is written against virglrenderer's context API and crosvm's guest-memory memfd.
   xnuhost must either link virglrenderer and present the same blob/pool behaviour, or lift the file. Nobody has
   run it outside crosvm.
2. **`/dev/udmabuf` on Samsung's kernel [U].** drm2kgsl imports guest pages only through udmabuf. On Droid-VM's
   6.1 phones the device comes from a module (`gunyah_host_mod`, "udmabuf: provide or repair"). If the S24 Ultra's
   kernel has no udmabuf and no module loads, the missing piece is an import by address
   (`KGSL_USER_MEM_TYPE_ADDR` on xnuhost's own mapping of guest RAM) or dma-heap-backed guest RAM. Neither exists
   anywhere. Check first: `ls -l /dev/udmabuf /dev/dma_heap/system` and whether the SELinux domain may open them.
3. **KGSL VBO on One UI 9's KGSL [U]**: `KGSL_MEMFLAGS_VBO` and `GPUMEM_BIND_RANGES` work on OnePlus's SM8650
   6.1.118; Samsung's build is expected to match but is unverified. The probe is the first 60 lines of
   `drm2kgsl_renderer_probe`.
4. **An iPhone (not iPad) on this stack.** Every working guest is iPad17,3 or iPad7,1. Unpublished for iPhone18,2:
   the plug-in's QuartzCore/RenderBox hook addresses, `legacy-display-24A446.json` and `hid-device-24A446.json`
   for the iPhone kernelcache, @3x scale, Dynamic Island metrics, CommCenter idling without a baseband.
5. **A display path to SurfaceFlinger without the DroidVM app.** crosvm presents through the app's Surface; a root
   SurfaceControl/AHardwareBuffer presenter for a KGSL-imported frame is new.
6. **virtio-snd, virtio-net and battery device sides on Android** (AAudio, tap/NAT, `/sys/class/power_supply`; written since in `runtime/bridges`, in C, and in Rust since 2026-11-23, [hardware.md](hardware.md)):
   crosvm's are Rust and partly inside the DroidVM app.
7. **Wi-Fi state bridge** (One UI's SSID, RSSI and scan results into `DroidVMWifi.inc`'s station), telephony, GNSS
   and haptics for an iOS guest: Droid-VM has the host and Linux-guest halves only (2026-11-10).
8. **Any native or Samsung-specific work for the S24 series**: no XNU port, no ios-sweet fork, no UniXNU board.
9. **A user-mode XNU.** Still unpublished anywhere; Droid-VM's own feasibility note stops at a recommendation.

## 5. Licences and rules

- Kernel-linked sources we take are BSD-2-Clause-Patent or MIT. metal2vulkan (GPL-2.0-or-later or LGPL-3.0-or-later)
  and Mesa (MIT) are user space, never linked into `kernel.kc`: built into the Metal plug-in. Since 2026-11-24 a
  release's runtime package carries that plug-in as a binary (`lib/guest/`, [releases.md](releases.md)), so it is
  distributed: `lib/guest/NOTICE` holds the licence texts of every source it is built from and each source's URL and
  commit (the corresponding source; OneiOS's own patches to them are in `userland/metal/` at the package's commit).
  Not checked: the licence notices of the Rust crates metal2vulkan's Cargo.lock pulls in (most say MIT or Apache-2.0;
  a guess, not read one by one).
- `droidvm-meta-repo` and `gunyah_host_mod` are GPL: read the plans, do not copy text or code into MIT files.
- `macOS-DroidVM`, `UniXNU-shennong`, `hakios-sm8650`, `UniXNU-NX769J`, `UniXNU-meizu21`, `UniXNU-PHY120` state no
  licence: read only.
- Code adapted from a reference goes into our own file with a comment naming the repo, file and commit.
- No firmware, no Apple files and no release assets are fetched by `get-references.sh`. `UniXNU/AdrenoFirmware` is
  source for the GMU's HFI protocol, not a blob, and is not needed here.
- A pin moves by a new commit in `scripts/setup/get-references.sh` (or `kernel/pins.sh` for the kernel's four), with
  the reason in the commit message.
