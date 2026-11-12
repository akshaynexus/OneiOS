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
   `runtime/xnuhost/src/virtio_gpu.c` is 2D only today and says Metal "goes another way"; the proven way is the
   one above. With it, the guest's GPU drivers, the plug-in and Turnip are taken unchanged.

## 2. Use this for ...

### GPU (Metal on the Adreno 750)

| Layer | Take | From (pin in `get-references.sh` unless noted) | Licence | Fit |
| --- | --- | --- | --- | --- |
| Host: KGSL | `src/drm/drm2kgsl/drm2kgsl_renderer.c` (3081 lines), `msm_kgsl.h`, with virglrenderer's `drm_context`, `drm_fence`, `msm/msm_proto.h` | `virglrenderer` 7e17c8e (Droid-VM, `wip/hackintosh`) | MIT | **Direct**: runs on SM8650 KGSL today. Link it (or the whole library with only the drm renderer) into xnuhost and feed it from the virtio-gpu device |
| Host: device model | `devices/src/virtio/gpu/{protocol.rs,virtio_gpu.rs,boot_pool.rs}`, `rutabaga_gfx/src/virgl_renderer.rs`, `aarch64/src/fdt.rs` (the drm-host pool node) | `crosvm` f82707d | BSD-3-Clause | Reference for the C device in xnuhost: capset 6, `CTX_CREATE`, `RESOURCE_CREATE_BLOB` (HOST3D, HOST3D_GUEST + `CREATE_GUEST_HANDLE`), `MAP_BLOB` answered with `MAP_INFO_POOL`, `SUBMIT_3D` with ring fences |
| Guest kernel: transport and user client | `package/DroidVMVirtioGpu.cpp`, `DroidVMVirtioGpu3D.cpp`, `DroidVMVirtioGpuDisplay.cpp`, `DroidVMGpu.cpp`; `ipados27/scripts/build-droidvm-gpu.py`, `link-droidvm-gpu-sources.py` | `UniXNU-droidvm` f9121cd (already in `kernel/pins.sh`) | BSD-2-Clause-Patent | **Direct**: unchanged if xnuhost's device behaves as crosvm's |
| Guest kernel: accelerator | `NativeAdrenoUserClient.cpp`, `NativeAdrenoPublish.cpp`, `NativeAdrenoSubmit.cpp`, `NativeAdrenoMemory.cpp`, `NativeAdrenoAccelerator.cpp`, `DroidVMVirtgpuABI.h`, `NativeAdrenoABI.h` | `NativeAdreno` 627836d (= `drivers/Adreno740` of the kernel base 62d0b11) | MIT | **Direct** (only these files; the GMU, SMMU, ring and firmware files are sheng's hardware) |
| Guest kernel: policy | `droidvm-gpu-sandbox-first.patch`, `droidvm-gpu-grant-apps.patch`, `droidvm-surface-gpu-wait.patch`, `droidvm-osmetaclass-adopt-superclass.patch`, `zzw-native-sandbox-grant-patterns.patch` | `UniXNU-droidvm` | BSD-2-Clause-Patent | Already in `kernel/device/patches/xnu-12377/series` |
| User space: plug-in | `NativeAdrenoMetalPlugin.mm`, `NativeBridgeMetalFrontend.mm`, `bridge/`, `shaders/`, `tools/` | `NativeMetal` b0d6a07 (= `drivers/AdrenoMetal` of the kernel base) | MIT | **Direct** for the API; its QuartzCore and RenderBox hooks are found per shared cache, so they must be re-found in the iPhone18,2 cache (`macOS-DroidVM/docs/TODO-gpu.md`) |
| User space: translator | metal2vulkan `624bab3` (`development`) + `patches/metal2vulkan/0001..0038` | steelbrain/metal2vulkan, patches in `Droid-VM-UniXNU` 62d0b11 (pin: `scripts/data/gpu-metal-vulkan-inputs.json`) | GPL-2.0 / LGPL-3.0 parts | Direct. In the table as `metal2vulkan` since 2026-11-11 (`userland/metal/build.sh` builds it; the shallow clone is 1.8 GB with its fixtures). Never linked into the kernel |
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

### Display

| Take | From | Licence | Fit |
| --- | --- | --- | --- |
| `NativeLegacyDisplay.cpp`, `NativeMobileDisplay.cpp`, `NativeGOPPresenter.cpp`, `OriginalMobileFramebufferABI.h` under the original `IOMobileFramebufferLegacy`; built by `ipados27/scripts/build-droidvm-legacy-display.py` with `legacy-display-24A446.json` | `NativeMobileDisplay` ed08bd4 (fork of UniXNU/GopFramebuffer; = `drivers/GopFramebuffer` of the kernel base) | BSD-2-Clause-Patent | **Direct** |
| `package/DroidVMVirtioGpuDisplay.cpp` (scanout client: retains each IOSurface's pages until `RESOURCE_UNREF`), `LegacyDisplayProbe.cpp` | `UniXNU-droidvm` | BSD-2-Clause-Patent | **Direct**; xnuhost's `virtio_gpu.c` is its device |
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

### Network

| Take | From | Licence | Fit |
| --- | --- | --- | --- |
| `package/DroidVMVirtioNet.c` (virtio-net transport, interrupt-driven receive), `DroidVMVirtioEthernet.cpp` + `DroidVMVirtioNetIf.cpp` (IOEthernetController on IONetworkingFamily-177: `en0`, DHCP by iOS itself) | `UniXNU-droidvm` | BSD-2-Clause-Patent | **Direct**; host side: a virtio-net device in xnuhost on a tap (takeover) or user-mode NAT (overlay) |
| `package/DroidVMWifi.inc`: an `IO80211Controller` with no radio so wifid and Settings have a Wi-Fi interface; `droidvm-wifi-ssid=NAME` shows one joined network | `UniXNU-droidvm`; request layouts from `UniXNU-sheng` `package/WifiNet.cpp` (extra table) | BSD-2-Clause-Patent | Direct. A real bridge to One UI's Wi-Fi state (SSID, signal) is new work |
| IONetworkingFamily-188's headers (`IOEthernetController.h`, `IOEthernetInterface.h` and what they include), to compile subclasses of the kernelcache's original classes | [apple-oss-distributions/IONetworkingFamily](https://github.com/apple-oss-distributions/IONetworkingFamily) `bb910e2` (tag `IONetworkingFamily-188`), cloned by `kernel/patches/oneios-hosted/net/build-oneios-net.py` into `out/sources/IONetworkingFamily` | APSL 2.0 | Headers only; checked slot by slot against the kernelcache at each build (`kernel/patches/oneios-hosted/net/README.md`) |
| The supplicant's stable AIDL interface, version 2 (method order = transaction codes, parcelables' fields), and libbinder's wire format (`Parcel.cpp`, `Status.cpp`, `Stability.cpp`) | AOSP `hardware/interfaces/wifi/supplicant/aidl/aidl_api/android.hardware.wifi.supplicant/2` (hash `5e5c6d9e...`, the one the phone's supplicant answers), `frameworks/native/libs/binder` | Apache 2.0 | Read, not copied: `runtime/bridges/wifi/supplicant.h` holds the numbers, `aidl_parcel.c` the format |
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
| Video codecs: `DroidVMVTHost.cpp`, `DroidVMVTParavirt.cpp`, `DroidVMVirtioMedia.c` over virtio-media | `UniXNU-droidvm`; Droid-VM/virtio-media `32d7366` | BSD-2-Clause-Patent | Later; the host side would be MediaCodec |
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
| 8 | [Droid-VM/crosvm](https://github.com/Droid-VM/crosvm) f82707d `wip/hackintosh` | BSD-3-Clause | The device side of every virtio device above | | Works | Reference for xnuhost's C devices |
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
| 18 | [oosexgang/hakios-xnu-kernel](https://github.com/oosexgang/hakios-xnu-kernel) 63fc2b4 | see repo | HakiOS: own loader, drivers and "hakios-metal" (a Metal driver without Vulkan: 110 fps page swipes on A642L) | SM7325 | iOS 16.5 | Watch: a second Metal design, bare-metal and xnu-8796 today |
| 19 | [dipa-studio/ios-sweet](https://github.com/dipa-studio/ios-sweet) 45263be and its forks | MIT | xnu-8796 bare-metal, iOS 16, CPU-rendered | SM7150 and many others | Home screen | Not useful: no PAC userland, no GPU through Linux |

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
6. **virtio-snd, virtio-net and battery device sides in C on Android** (AAudio, tap/NAT, `/sys/class/power_supply`):
   crosvm's are Rust and partly inside the DroidVM app.
7. **Wi-Fi state bridge** (One UI's SSID, RSSI and scan results into `DroidVMWifi.inc`'s station), telephony, GNSS
   and haptics for an iOS guest: Droid-VM has the host and Linux-guest halves only (2026-11-10).
8. **Any native or Samsung-specific work for the S24 series**: no XNU port, no ios-sweet fork, no UniXNU board.
9. **A user-mode XNU.** Still unpublished anywhere; Droid-VM's own feasibility note stops at a recommendation.

## 5. Licences and rules

- Kernel-linked sources we take are BSD-2-Clause-Patent or MIT. metal2vulkan (GPL/LGPL parts) and Mesa (MIT) are
  user-space files in the overlay, built on the owner's machine; they are never linked into `kernel.kc`.
- `droidvm-meta-repo` and `gunyah_host_mod` are GPL: read the plans, do not copy text or code into MIT files.
- `macOS-DroidVM`, `UniXNU-shennong`, `hakios-sm8650`, `UniXNU-NX769J`, `UniXNU-meizu21`, `UniXNU-PHY120` state no
  licence: read only.
- Code adapted from a reference goes into our own file with a comment naming the repo, file and commit.
- No firmware, no Apple files and no release assets are fetched by `get-references.sh`. `UniXNU/AdrenoFirmware` is
  source for the GMU's HFI protocol, not a blob, and is not needed here.
- A pin moves by a new commit in `scripts/setup/get-references.sh` (or `kernel/pins.sh` for the kernel's four), with
  the reason in the commit message.
