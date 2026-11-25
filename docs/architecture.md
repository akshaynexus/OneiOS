# Architecture

How OneiOS runs iOS 27 next to One UI, and which part does what. Detail lives in the per-part docs linked below.

## The idea in one paragraph

OneOS switches a rooted Galaxy into another Android by stopping One UI's framework (zygote, system_server) and
starting the guest's framework on the same Linux kernel, with Samsung's vendor half (kernel modules, HALs,
audioserver, cameraserver, the display composer) left running (Project Treble). iOS cannot run on Linux's system
calls, so OneiOS runs Apple's kernel, **XNU, as an ordinary Linux process** (`xnuhost`) on One UI's kernel. iOS's
own processes are Linux processes whose system calls and faults go to that XNU. The image, the switch and the way
back stay exactly OneOS-shaped.

```
 One UI's Linux kernel (unchanged, KernelSU root)          Samsung vendor half keeps running
 ├─ surfaceflinger + composer HAL ◄── oneios-display (layer with iOS's frames)
 ├─ /dev/kgsl-3d0, /dev/udmabuf  ◄── xnuhost's virtio-gpu 3D (drm2kgsl)
 ├─ audioserver, camera HAL, BT HAL, vibrator evdev, backlight sysfs, ALS, MediaCodec ◄── bridges in xnuhost
 ├─ wlan0 (AF_PACKET, nl80211) + the vendor's wpa_supplicant (AIDL) ◄── the Wi-Fi bridge: iOS's en0, no NAT
 └─ xnuhost  (one Linux process; mount namespace with the stage over /data; its own cgroup)
     ├─ one host thread per vCPU, GICv3 model, timer, PSCI, EL1 system registers emulated
     ├─ virtio-mmio devices: block (the image set), input (evdev), GPU, sound, net, camera, bridge channel
     ├─ XNU 12377 (UniXNU / Droid-VM fork + OneiOS hosted patches) + Apple's iPhone18,2 kexts = kernel.kc
     └─ iOS tasks = Linux processes forked from one factory (one PAC key set); a stub traps svc and faults
         └─ launchd → daemons → backboardd → SpringBoard  (iOS 27.0.1, Apple's binaries unchanged)
```

## The parts

| Part | Where | What it does | Doc |
|---|---|---|---|
| Image set | `profiles/`, `scripts/fetch/`, `scripts/image/` | Fetches only the needed members of Apple's iPhone18,2 27.0.1 IPSW from Apple's CDN, decrypts the AEA images (`oneios-tool image aea-decrypt`, the key from Apple's key service; on the phone `serve`'s `aea`), and builds `out/image/24A446/`: `os.img`, `systemos.img`, `app.img` (Apple's APFS images, unchanged), trust caches, the Data seed, the overlay, `kernel.kc`, `manifest.json`, packed as a `.tar.zst`. Never published. | [image-format.md](image-format.md) |
| Userland tweaks | `userland/` | The image hook: iOS 27 root and Data tweaks for this host (identity, daemon policy, preferences), applied to the Data seed and overlay; Apple's images stay unchanged. | [userland.md](userland.md) |
| Kernel | `kernel/` | XNU 12377 from Droid-VM's UniXNU fork, built on Linux (`build-linux.sh`, the VM kernel; `build-hosted.sh`, the kernel for xnuhost with `kernel/patches/oneios-hosted/`). Every hosted change is under `XNU_HOSTED`: the kernel runs at `0x2000000000` (the S24's user VA ends at 2^39), with no MMU of its own, interrupts as host upcalls. Hardware the phone cannot give the guest is stood in for in software inside the kernel: the AES accelerator (`aes/`) and the key store (`keystore/`: the AppleKeyStore service with no passcode, class keys from a constant, key wrap in software; there is no Secure Enclave, and this is **not a security boundary**). | [kernel/README.md](../kernel/README.md), [hosted patches](../kernel/patches/oneios-hosted/README.md) |
| Kernel collection | `kernel/prelink/` | `kernel.kc`: the hosted kernel prelinked with Apple's iPhone18,2 kexts (about 154; those for absent hardware are left out with a reason). Apple's kexts call Apple's kernel; the prelink pairs each called function with ours (matchers, a RELEASE reference build, vtables, reviewed records). Unpaired calls go to a panic stub. Built locally; never committed or released (Apple's kexts). | [kernel/prelink/README.md](../kernel/prelink/README.md) |
| xnuhost | `runtime/xnuhost/`, `runtime/loader/` (Rust: [runtime/RUST.md](../runtime/RUST.md)) | The host: loads `kernel.kc` (DarwinBoot's loader, ported), builds the device tree and `boot_args`, runs vCPUs as host threads, emulates the interrupt controller, timer and privileged registers, serves the virtio devices, runs iOS tasks and hands their traps to XNU (`hostcall.h`, ABI 1). Checks every 1 MiB of the iOS images against the manifest as it is read. | [booter.md](booter.md) |
| GPU | `runtime/xnuhost` (virtio-gpu 3D), `kernel/patches/oneios-hosted/gpu/` | DRM native context served by Droid-VM's drm2kgsl on the Adreno 750 through KGSL, guest pages shared by udmabuf; the 2D scanout where there is no renderer. On the Galaxy A56 (Xclipse, no KGSL) the amdgpu native context instead, in Rust on `/dev/dri/renderD128` (`gpu3d_amdgpu`, [a56-gpu.md](a56-gpu.md) 2), when the gpuaddon carries the A56's Metal plug-in and its self-test passes. In the kernel: the virtio-gpu 3D driver and NativeAdreno's `AGXAccelerator` (under the collection's `IOAcceleratorES`) with the user client the Metal plug-in opens. | [gpu.md](gpu.md) |
| iOS user-space builds | `scripts/ios/`, `scripts/setup/ios-toolchain.sh`, `userland/metal/` | The cross toolchain on Linux (clang, the xtool SDK's headers, ld64, stubs made from the guest's own iOS 27 shared cache, an ad-hoc signer, a link check) and the build of the Metal plug-in `NativeAdrenoMetal.bundle` (NativeMetal, metal2vulkan, Turnip) for the iPhone18,2 cache. The image hook places the bundle and lists its CDHashes for the owner trust cache. | [building.md](building.md) section 9, [gpu.md](gpu.md) section 7 |
| Display driver (guest) | `kernel/patches/oneios-hosted/display/` | iOS's framebuffer: UniXNU's NativeMobileDisplay as a subclass of the collection's original `IOMobileFramebufferLegacy`, started on the boot video (the phone panel's native mode, 1440x3120; `/chosen display-scale` 3 or 2), with a vsync tick at the panel's 120 Hz and the refresh properties of a 24 to 120 Hz panel; `backboardd`'s swaps go to xnuhost's virtio-gpu scanout surface by surface and from there to `session/display.sock`. | [display/README.md](../kernel/patches/oneios-hosted/display/README.md) |
| Display | `runtime/display/` | `oneios-display`: on One UI's side, an `app_process` helper that puts xnuhost's frames (`session/display.sock`) on a full-screen surfaceflinger layer at the panel's mode, the buffers tagged with the frame's dataspace (8-bit sRGB by default; half floats, linear extended sRGB, with xnuhost's `display-format=rgba16f`). | [runtime/display/README.md](../runtime/display/README.md) |
| Touch and buttons | `runtime/xnuhost/src/input.rs`, `kernel/patches/oneios-hosted/hid/` | The touchscreen and the three side keys, read from evdev without a grab, through xnuhost's virtio-input device to two HID devices in the kernel (UniXNU's NativeHidInput under the collection's `IOHIDDevice`): a Digitizer / Touch Screen service and the buttons, which Apple's HID stack and backboardd take as an iPhone's. | [hardware.md](hardware.md) 7b, [the archive's README](../kernel/patches/oneios-hosted/hid/README.md) |
| Touch and buttons | `runtime/xnuhost/src/input.rs`, `kernel/patches/oneios-hosted/hid/` | The touchscreen and the three side keys, read from evdev without a grab, through xnuhost's virtio-input device to two HID devices in the kernel (UniXNU's NativeHidInput under the collection's `IOHIDDevice`): a Digitizer / Touch Screen service and the buttons, which Apple's HID stack hands to backboardd as an iPhone's. | [hardware.md](hardware.md) 7b, [the archive's README](../kernel/patches/oneios-hosted/hid/README.md) |
| Pen (S Pen as an Apple Pencil; off by default) | `runtime/xnuhost/src/pen.rs`, `kernel/patches/oneios-hosted/hid/` (`staged/hid_pen.rs` of `oneios-tool kernel gen hid`) | With `pen=auto`: the phone's pen digitizer (`sec_e-pen`), found by its bits through one replaceable lookup (`pen::lookup`), read from evdev without a grab, as a second virtio-input device (`/arm-io/input-1`, fixed ranges) to a third NativeHidInput device, Digitizer / Stylus, which backboardd takes as its pencil digitizer and UIKit types as `UITouchTypePencil`; guest boot argument `oneios-pen=1` (xnuhost adds it). | [hardware.md](hardware.md) 7f, [the archive's README](../kernel/patches/oneios-hosted/hid/README.md) "Pen" |
| Hardware bridges | `runtime/bridges/`, `drivers/` | Host halves (in xnuhost or small helpers) and iOS-side drivers for: CPU topology and clocks, network (`en0` over a tap with its own routing), Bluetooth (the HIDL HAL over hwbinder), haptics (the CS40L26 force-feedback device), audio (AAudio), brightness (backlight sysfs) and the light sensor, the motion sensors and the front light sensor (the sensor hub over QRTR, `runtime/bridges/sensors`; off by default, `sensors=auto`), the battery (`power_supply` sysfs), camera (the camera provider HAL in a session, the NDK on One UI; in iOS, `AppleH9CamIn` and the capture plug-in `H9ISP.mediacapture` that CMCapture loads for it), and the video encoder and decoder (iOS's paravirtualized VideoToolbox over Android's MediaCodec, so CMPhoto writes HEIC and, since 2026-11-24, Safari's video is decoded by the phone's hardware: `runtime/bridges/encoder`, `kernel/patches/oneios-hosted/codec`). | [hardware.md](hardware.md) |
| Runtime package | `runtime/package.sh`, `.github/workflows/` | `oneios-runtime-<ver>-arm64.tar.zst` with `runtime.json`: xnuhost, the helpers and the phone check tools. Published on GitHub releases. No kernel, nothing of Apple's. | [releases.md](releases.md) |
| OneOS fork | `akshaynexus/OneOS`, branch `oneios` | The "iOS 27 (OneiOS)" card (S24 Ultra only): downloads the runtime, imports and verifies the image set, Hardware checks, and the session scripts: arm the way back, stop zygote, start xnuhost and the display helper, return in place or by reboot. | [oneos/interface.md](../oneos/interface.md), the fork's `docs/oneios.md` |

## The runtime's code

Everything that runs on One UI's side (xnuhost, the loader, the bridges and their helpers, the camera and encoder
helpers, the display helper, the phone checks) is Rust, one Cargo workspace, `runtime/Cargo.toml` (ported from C on
2026-11-23; it first booted iOS on the phone that day, docs/booter.md 2.6a). Its crates: `xh-core` (the log, the virtio-mmio transport, the
MMIO bus, the shared helpers), `xnuhost-abi` (the host-call ABI), `xnuloader`, `xnuhost` (with `drm2kgsl-sys`, Droid-VM's
renderer built from its pinned C), `onebridges`, `onecamera`, `oneencoder`, `oneios-display`, `oneios-switches` (the switch
catalog, which oneios-tool builds too); the task stub and the
test kernel are bare-metal Rust programs of their own. What stays C is what the guest side or another project owns:
the headers the hosted kernel and the guest drivers include (`runtime/xnuhost/include/xnuhost/*.h`,
`drivers/*/*_proto.h`, each mirrored in Rust and checked against the header by a test), drm2kgsl and its glue, and
`kernel/`, `drivers/`, `userland/`. The crate map, the conventions and the commands:
[runtime/RUST.md](../runtime/RUST.md).

## The backend layer: samsung and treble

Each hardware bridge has two forms of its Linux or Android side, chosen at start by the setting `hw-backend`
(`samsung`, the default, or `treble`) and, per bridge, `hw-backend-<bridge>` (`docs/booter.md` 1):

- **samsung**: today's paths on the Galaxy S24 Ultra (Samsung's nodes and device names), unchanged;
- **treble**: the vendor-neutral interfaces of any Treble/GSI phone: the stable AIDL HALs through servicemanager
  (health, thermal, vibrator; the client is the Wi-Fi bridge's binder code, no Android library), evdev found by its
  capabilities, the standard sysfs classes by named paths.

The choice lives in one place, `onebridges::hw` (`runtime/bridges/hw/hw_backend.rs`, pure, and `hw/hw_log.rs`),
which xnuhost's `config.rs` fills and its session (`session/bridges.rs`) hands to the bridges (`hw::hw_configure`)
before any opens. Each bridge asks it once at open (`hw::select`: one log line with the backend and why) and builds
its samsung or treble source behind the same API, so the bridge device, the drivers and iOS do not know which one
runs. A treble interface that is missing is one line (`hw::missing`); the bridge then stays off, or with
`hw-backend-fallback=1` uses its samsung form. Touch and the side keys have one lookup for both (evdev by
capabilities, never grabbed). The audit table of every path, and each bridge's status: [hardware.md](hardware.md) 7e.

## The host device table: one tree, many phones

Which phone xnuhost runs on is found at its start, not built in: `runtime/bridges/hw/host_device.rs` reads the
properties `ro.product.model`, `ro.product.device`, `ro.board.platform`, `ro.soc.model`, `ro.soc.manufacturer` (and as
fallbacks soc0's `machine` and the device tree's `compatible`) and matches them against one compiled-in table of known
hosts: the Galaxy S24 Ultra (the home device, every `SM-S928*` model number: B, U, U1, W, N, 0; no defaults of its own:
today's behaviour), S25 Ultra, S25, S26 Ultra and
A56. An entry holds only settings that already exist as options (an xnuhost setting `config.rs` merges as a device
default, or an environment variable a bridge reads), and goes under every explicit setting: the command line > the
image set's `xnuhost.conf` > the table > the built-in default. `host-device=NAME` forces an entry, `host-device=none`
turns detection off; an unknown phone keeps today's defaults. xnuhost logs one `host: ...` line at its start (the
entry, the property that matched, each default taken or not); the bridges' ctl tools and `oneios-tool phone status`
use the same table (oneios-tool's copy, `crates/oneios-phone/src/host.rs`, is checked against the bridges' by a test). A new phone is a new table row plus its
record, never a fork: [hardware.md](hardware.md) 7h has the table, what is marked unverified per phone, and the tests.
A row may also allow the opt-in Android takeover (`takeover: true`: the S24 Ultra and the S26 Ultra), which
`xnuhost --host-info` tells the OneOS switch before it stops anything.

## Two session modes: hosted (the default) and the opt-in Android takeover

`session-mode=hosted` is the session below. `session-mode=takeover` (the app's box on the Switch sheet, or the image's
`xnuhost.conf`; [oneos/interface.md](../oneos/interface.md) 4a) is the same session with more of the phone given to
iOS: today the native touchscreen grabbed for iOS alone and a CPU cap of the fork's from the case temperature (put back
by the return); later the stages of [plan-ios-first-session.md](plan-ios-first-session.md) (5a). The way back is the
same in both: the chord and the in-place return first, a restart only where the in-place return cannot put One UI
back. xnuhost's part: [booter.md](booter.md) 2.9d.

## A session, step by step

1. **Stage** (in the app, One UI running): the runtime package and the image set are verified against their
   manifests and staged as root under `/data/local/tmp/oneios/{runtime,image,data,session,control}`.
2. **Arm the way back:** the watcher, the Volume Up + Volume Down readers and a wakelock start before anything stops.
3. **Stop One UI's framework:** zygote (and so system_server). The kernel, vendor HALs, surfaceflinger and the
   composer keep running. With UsbDeviceManager gone, the fork's USB watcher keeps adb on the USB gadget across
   unplug and replug ([oneos/interface.md](../oneos/interface.md) 6a; its repairs are off, logged only, until a phone
   session turns them on with `usb-repair` in the stage).
4. **Start xnuhost** in a mount namespace with the stage over `/data`, in its own cgroup, then `oneios-display`. xnuhost
   sizes the guest's RAM from the host's MemAvailable less what the session needs beside it at its peak (the iOS
   tasks' page tables and kernel memory, KGSL's own, a headroom for the host; the GPU's blobs are the guest's own
   pages) and holds it prefaulted and locked for the session (`docs/booter.md` 2.2, "What fits", "Fixed RAM").
5. **Boot:** xnuhost loads `kernel.kc`; XNU mounts the OS volume (verified by xnuhost), on first start formats the
   Data volume and unpacks the seed, mounts the cryptexes, starts launchd; backboardd draws on the guest's framebuffer, and at its first frame that is not all black (not the boot console's; since 2026-11-15, `docs/booter.md` 2.8)
   xnuhost writes `session/ready`.
6. **Return:** Volume Up + Volume Down for 3 s, a return request from inside iOS, xnuhost exiting, or no `ready` in
   time. The return ends xnuhost (SIGTERM, then SIGKILL), restores One UI's backlight, restarts the composer and
   surfaceflinger, puts the USB gadget back as One UI had it (with `usb-repair`), restarts zygote; any failed check reboots into One UI instead. Power + Volume Down (Samsung's
   forced restart) is the backstop below all software.

## Why it is fast where it is fast, and slow where it is slow

- iOS code runs **natively** on the Cortex-X4/A720/A520 (PAC and all): no instruction emulation.
- Every iOS system call and Mach trap is caught (seccomp on the phone) and handed to XNU in another thread: each costs
  a round trip, so trap-heavy work is slower than on an iPhone.
- iOS signs code in 16 KiB pages and this kernel runs 4 KiB pages: at the first boot of a session the dyld shared
  cache's signatures are re-derived once (UniXNU's `cs_native4k_prepare`, parallel since `0014`): seconds on the
  phone's hardware SHA, minutes under qemu on a PC.

## What it is not

- Not a VM: no hypervisor, no Gunyah (an unsigned guest is refused on this firmware), no bootloader unlock.
- Not an emulator: no instruction translation (that is the vphone app's QEMU path).
- Not a secure store: no Secure Enclave, no passcode, no lock. The guest's key store is software with keys anyone can
  derive (`kernel/patches/oneios-hosted/keystore/README.md`); the Data store is as private as One UI keeps the file.
- Not Apple's kernel binary: XNU is built from Apple's open source; only Apple's kexts and userland are Apple's,
  taken from the owner's own IPSW download and never redistributed.
