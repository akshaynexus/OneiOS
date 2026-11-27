# Credits: work ported from forks

The projects this one builds on are credited in [README.md](README.md) "Credits" and, with their licences, in
[docs/references.md](docs/references.md). This file names the people whose changes in a fork of this repository were
ported here: which commit, what it did, and where it is now. A fork of this repository is under this repository's
terms; each port's commit message names its source as well. The last section names the other iOS-on-Android ports whose code
was taken file by file (not forks of this repository).

## orumanayurpanjayat-ux/OneiOS

Authors: **leusernamealadeen** and **orumanayurpanjayat-ux**. Ported 2026-11-25 (branch `port/oruma-2026-11-24`), not
booted on a phone at the time of the port.

| Fork commit | What it did | Here |
|---|---|---|
| `84cb2ef` "Force Refresh-Rate" (leusernamealadeen) | The display helper asks surfaceflinger for the native mode and opens its refresh ranges in every takeover, not only when the size differs (One UI can leave the panel at its native resolution with app refresh capped at 60 Hz, Motion smoothness: Standard), and compares the active mode id as well as its size | Re-applied on `OneiOSDisplay.pick` (the active mode id), `isNative`, `takeoverLine`; tests in `runtime/display/tests/TestModes.java`; `runtime/display/README.md` "The panel" |
| `d157b43` "Shader-Fixes" (leusernamealadeen) | Applied the Adreno shader fixes `userland/metal/patches/metal2vulkan` 0101 and 0102 in the Metal bridge in every build | Applied behind a switch, on by default since the merge into main (2026-11-25): `0110-shader-fix-switch.patch`, `userland/metal/ShaderFix.inc`, boot argument `oneios-m2v-shader-fix=0` turns it off (docs/switches.md) |
| `79cf4fb`, `d5018d0` (leusernamealadeen) | `.github/workflows/release.yml` downloads the pinned kernel from akshaynexus/OneiOS, so a fork's CI finds it | As a fallback: this repository's release of the tag first, akshaynexus/OneiOS when a fork has none (docs/releases.md) |
| `076c1a3`, `56e1430` (leusernamealadeen, orumanayurpanjayat-ux) | A Metal build job in CI, then removed again; `METAL_LICENSES_DIR` in `runtime/package.sh` | Not ported: the job is gone in the fork too, and nothing here would set `METAL_LICENSES_DIR` |
| `be130197`, `23d30821`, `29600c17`, `10849627`, `8bcb43df`, `ded595d0`, `2d3dc620`, `62a87e78` (leusernamealadeen; ported 2026-11-27, not booted) | `kernel/build-hosted.sh` header fix (also fixed independently by `MRDinar/OneiOS-A56x` `12032ee9`), Bash 3.2 empty-array fix & SDK stub fallback & `.gpuaddon.cdhashes` in `userland/metal/build.sh` / `metal.rs` / `metal-plugin.yml`, runtime boot-addon fallback (`addon.rs`, `imageset.rs`, `session/board.rs`, `runtime/package.sh`), and `sheng-vsync-fixed=1` in `staged/display.rs` | Ported onto `main` (`port/fork-survey-2026-11-27`) |

## burugou05-wq/OneiOS

Author: **burugou05-wq**. Ported 2026-11-27 (`port/fork-survey-2026-11-27`), host-tested, not booted on a phone here.

| Fork commit | What it did | Here |
|---|---|---|
| `e85405f1` "feat(s26): support Galaxy S26 base models, KernelSU --ro-bind, and README" | Galaxy S26 base models (`galaxy-s26` SM8850 `m1q` / `SC-51G` and `galaxy-s26-exynos` `m1s` / `s5e9965`), shared-model disambiguation, host profiles, `verify-host.sh`, SC-51G Oryon CPU test, `docs/s26.md`, and `xnuhost --ro-bind` direct `mount(2)` remount helper for KernelSU BusyBox 1.36 | `runtime/xnuhost/src/session.rs`, `runtime/bridges/hw/host_device.rs`, `tools/oneios-tool/crates/oneios-phone/src/host.rs`, `profiles/host-galaxy-s26-*.toml`, `scripts/port/verify-host.sh`, `docs/s26.md` |

## NoNameBuilder/OneiOS

Author: **NoNameBuilder**. Ported 2026-11-27 (`port/fork-survey-2026-11-27`), host-tested, not booted on a phone here.

| Fork commit | What it did | Here |
|---|---|---|
| `08c64e13`, `3c4f5976` | Reopen speaker as a legacy mixer track when an MMAP stream stalls (takes no frames during probe or playback) | `runtime/bridges/audio/aaudio_helper.rs`, `test_audio_helper.rs`, `runtime/xnuhost/src/session/bridges.rs` |
| `5abb28ff`, `0d0ba1f4`, `2c7ca0ef` | Read `ISupplicant.getInterfaceVersion` (v3 on S25), start/restore `IWifi/default` when One UI Wi-Fi was off (`wifi/chip.rs`), opt-in `wifi=cell` over tap + selector-safe `oneios-net.sh` (`test-oneios-net.sh`) | `runtime/bridges/wifi/`, `runtime/bridges/net/`, `runtime/xnuhost/src/config.rs`, `runtime/host-tests.sh` |
| `120df342`, `2a021be7`, `23b9b85f`, `9bed1282`, `9da51441`, `23cb7469`, `d1406034` | `/product display-corner-radius`, panel-derived `artwork-device-subtype` (2340 on 1080x2340), `galaxy-s25` built-in side keys & treble thermal SKIN row, thermal open-line `case_from`, opt-in `thermal-cap=0` (default kept `1` on `main`) | `runtime/loader/src/machine.rs`, `runtime/bridges/thermal/session.rs`, `runtime/bridges/hw/host_device.rs`, `runtime/xnuhost/src/config.rs`, `tools/oneios-tool/crates/oneios-phone/src/host.rs` |
| `3f0443fc`, `bc753278`, `7fd9bd72`, `47d370ad`, `6c945aa5` | `roottweak` writes `pen=auto` for non-20F66 images; `kb.rs` scans both XnuPrelink fixture folders; `virtio_gpu` `lanes` test posts all chains before one kick; Metal build Bash 3.2 & SDK stub fixes | `tools/oneios-tool/crates/oneios-tweaks/`, `oneios-phone/src/kb.rs`, `runtime/xnuhost/src/virtio_gpu/tests.rs`, `userland/metal/build.sh` |

## YMGPwcca/OneiOS

Author: **Nguyễn Tuấn Nghĩa (YMGPwcca)**. Ported 2026-11-27 (`port/fork-survey-2026-11-27`), rewritten into the Rust runtime, host-tested, not booted on a phone here.

| Fork commit | What it did | Here |
|---|---|---|
| `9e5a9933` "fix(xnuhost): publish deferred map ACK on the actual futex word" | Publish `XS_HOST_OPS_ACK` on `XsTrapPage.to_host` and pass observed `event` to `FUTEX_WAIT`, fixing a lost-wakeup race that could stall a vCPU up to 200 ms holding the task lock | `runtime/abi/src/stub.rs`, `runtime/xnuhost/stub/src/main.rs`, `runtime/xnuhost/src/task.rs` |
| `bb950679` "perf(xnuhost): skip acknowledged empty service flushes" | Return early in `order_as()` when `first == 0 && ops.is_empty()` instead of waking the service thread for a 0-op flush | `runtime/xnuhost/src/task.rs` |
| `257f7ee7` "perf(xnuhost): avoid redundant host timer disarms" | Skip redundant `timer_settime(0)` when `timer_host_at == 0` (host timer already disarmed) | `runtime/xnuhost/src/cpu.rs` |

## Other ports (not forks of this repository)

Code or numbers taken from other iOS-on-Android ports, credited here as well as in [docs/references.md](docs/references.md)
and in the file that carries them.

| Source | What it did | Licence | Here |
|---|---|---|---|
| [UniXNU/UniXNU-sheng](https://github.com/UniXNU/UniXNU-sheng) `c8f92b2` `hal/ShengAudioPlugIn.c` (`kMinDb`, `toDb`, lines 204-210; Xiaomi Pad 6S Pro, iPadOS 27.0.1 24A446) | Fixed the same quiet speaker on the same build by mapping the volume s to -36 x (1 - s)^2 dB | BSD-2-Clause-Patent | The speaker's volume curve of the staged `DroidVMAudio.cpp` (BSD-2-Clause-Patent), generated by `tools/oneios-tool/crates/oneios-kernel/src/staged/audio.rs` block "fix-audio-curve", behind `oneios-audio-curve` (default 1), 2026-11-25 (branch `fix/audio-volume-rt`); not booted at the time of the port. `docs/hardware.md` section 6, "The volume curve" |
## Droid-VM/UniXNU-droidvm (upstream, not a fork)

An upstream project (credited with its licence in docs/references.md); listed here because a change of it newer than
our pin was ported by itself. Authors: **lateautumn233** and **HuJK** (BSD-2-Clause-Patent). Ported 2026-11-25 (branch
`fix/codec-irq`), not booted on a phone at the time of the port.

| Upstream commit | What it did | Here |
|---|---|---|
| `96e8c1f` "virtio-media: completion interrupts instead of polling" (lateautumn233) | `DroidVMVirtioMedia.c`: the device's level SPI attached, the event worker and command waiters woken by it instead of a 1 ms poll; events copied under the transport's lock and delivered without it | Edit 7 of the codec archive (`tools/oneios-tool/crates/oneios-kernel/src/staged/codec_transport.rs`), with a guard timeout and the command queue kept `NO_INTERRUPT` (xnuhost answers commands inside the notify, and honours the flag on that device: `Device::quiet_queues`); boot argument `oneios-codec-irq=0` keeps the poll (docs/switches.md) |
| `2cd4c8e` "either ADT name for the window's interrupt, and polling where there is none" (HuJK) | The INTID from `droidvm,intid` or `gic-spi`; a window with neither, or an SPI that cannot be attached, polled as before | The same, with `interrupts` too (`OneiOSCodecWait.h` `oneios_media_polled_why`) |

## YumikoHayamato/OneiOS

The Galaxy S25 Ultra fork (branch `s25-ultra-sm8750`). Author: **YumikoHayamato** (commits signed "Stiven"). Ported
2026-11-25 (branch `fix/s24-fps-heat-camera`), not booted on a phone at the time of the port.

| Fork commit | What it did | Here |
|---|---|---|
| `cdb297bbf` "native path, step 6" (the display part) | With `ONEIOS_DISPLAY_PRESENT=ahb` the display helper posts its frames in three write-only AHardwareBuffers (`CPU_WRITE_RARELY`) on a child of its layer with ASurfaceTransaction, each filled with the damage since it was last filled, at most two in flight, refilled after its release fence; the window path kept when anything is missing. Measured on the S25: 85% of the helper's time had been the CPU caches' maintenance of the window's whole buffer at each lock and post; 60 frames a second at 49.5% of a core became 114 at 17.1% | Rewritten in Rust on the run loop's `Layer` (`runtime/display/src/xhd/present.rs`, the NDK half in `src/jni.rs`), behind xnuhost's `display-present=ahb` (default) through the `options` message (docs/switches.md); the child made from the layer's window rather than by the Java half; host tests `test_present_*`. Their copy-loop floor (`ONEIOS_DISPLAY_UCLAMP_MIN`), the frame-time line, `displayctl pool` and the PL030 kernel patch of the same commit are not ported (a VM's path) |
| `b4619f8b` "buttons: the power key reaches iOS at once (LockKeyDelay 0)" (ported 2026-11-27, not booted) | Publishes `LockKeyDelay = 0` in the buttons event service's `HIDEventServiceProperties` so backboardd's `IOHIDKeyboardFilter` does not hold power key-down for 150 ms (`oneios-hid-lock-key-delay=N` overrides) | `kernel/patches/oneios-hosted/hid/OneiOSHidInput.cpp`, `kernel/patches/oneios-hosted/hid/README.md` |

## Other ports' code taken file by file

### The sensors and auto-brightness (fix/sensors-als, 2026-11-25; host-tested and built, not booted)

| Source | Authors, licence | What was taken | Here |
|---|---|---|---|
| [DenialOfGravity/ios-redfin](https://github.com/DenialOfGravity/ios-redfin) 23019ab4 `phone/sensors/` (`sns.c`, `pb.c`, `sensor.c`, `test_sensd.c`), `phone/wifi/qmi.c` | dipa-studio (Copyright (c) 2026), MIT | the Qualcomm SEE client's message build and parse, the lookup/attribute/stream sequence and data types, the independently encoded test vectors | `runtime/bridges/sensors/` (Rust; the QRTR transport is Linux's own) |
| ios-redfin 23019ab4 `xnu/patches/1298-redfin-auto-brightness-true-tone.patch`, `0971-redfin-sensor-hid-services.patch` | dipa-studio, MIT | the last light reading re-posted within 15 ms of a wake (CoreBrightness's 250 ms first-sample timeout); the SPU usage pairs and the illuminance field | `drivers/brightness/OneiOSALSDevice.cpp`, `kernel/patches/oneios-hosted/hid/OneiOSALSEvents.cpp` |
| [Droid-VM/UniXNU-droidvm](https://github.com/Droid-VM/UniXNU-droidvm) f9121cd `package/DroidVMMotionHid.inc`, `DroidVMSensorProcessor.cpp`, `DroidVMMotion.c` | Droid-VM, BSD-2-Clause-Patent | the motion sensors as an iPad's SPU HID devices and their command channel, proven on iPadOS 27 24A446 | staged into the HID archive (`kernel/patches/oneios-hosted/hid`, `oneios-tool kernel gen hid`) |
| [UniXNU/ShengHidInput](https://github.com/UniXNU/ShengHidInput) 24f1f3f `NativeHidInput.cpp` | UniXNU, BSD-2-Clause-Patent | the event driver's vtable copy (dispatch wrapped, every slot's discriminator checked) | `kernel/patches/oneios-hosted/hid/OneiOSALSEvents.cpp` |
| [jesvi-eulah/Jesvi-iOS](https://github.com/jesvi-eulah/Jesvi-iOS) a13e406 `userland/src/inject_sensors.c` | Jesvi-iOS (HakiOS), MIT (`userland/`) | what CoreBrightness checks on an ALS service: Built-In, VendorID 0x5ac, `ALSCurveInfo`, the illuminance field 0xC000B | `drivers/brightness/OneiOSALSDevice.cpp` |
