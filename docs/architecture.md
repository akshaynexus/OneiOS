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
 ├─ audioserver, camera HAL, BT HAL, vibrator evdev, backlight sysfs, ALS ◄── bridges in xnuhost
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
| Image set | `profiles/`, `scripts/fetch/`, `scripts/image/` | Fetches only the needed members of Apple's iPhone18,2 27.0.1 IPSW from Apple's CDN, decrypts the AEA images, and builds `out/image/24A446/`: `os.img`, `systemos.img`, `app.img` (Apple's APFS images, unchanged), trust caches, the Data seed, the overlay, `kernel.kc`, `manifest.json`, packed as a `.tar.zst`. Never published. | [image-format.md](image-format.md) |
| Userland tweaks | `userland/` | The image hook: iOS 27 root and Data tweaks for this host (identity, daemon policy, preferences), applied to the Data seed and overlay; Apple's images stay unchanged. | [userland.md](userland.md) |
| Kernel | `kernel/` | XNU 12377 from Droid-VM's UniXNU fork, built on Linux (`build-linux.sh`, the VM kernel; `build-hosted.sh`, the kernel for xnuhost with `kernel/patches/oneios-hosted/`). Every hosted change is under `XNU_HOSTED`: the kernel runs at `0x2000000000` (the S24's user VA ends at 2^39), with no MMU of its own, interrupts as host upcalls. | [kernel/README.md](../kernel/README.md), [hosted patches](../kernel/patches/oneios-hosted/README.md) |
| Kernel collection | `kernel/prelink/` | `kernel.kc`: the hosted kernel prelinked with Apple's iPhone18,2 kexts (about 154; those for absent hardware are left out with a reason). Apple's kexts call Apple's kernel; the prelink pairs each called function with ours (matchers, a RELEASE reference build, vtables, reviewed records). Unpaired calls go to a panic stub. Built locally; never committed or released (Apple's kexts). | [kernel/prelink/README.md](../kernel/prelink/README.md) |
| xnuhost | `runtime/xnuhost/`, `runtime/loader/` | The host: loads `kernel.kc` (DarwinBoot's loader, ported), builds the device tree and `boot_args`, runs vCPUs as host threads, emulates the interrupt controller, timer and privileged registers, serves the virtio devices, runs iOS tasks and hands their traps to XNU (`hostcall.h`, ABI 1). Checks every 1 MiB of the iOS images against the manifest as it is read. | [booter.md](booter.md) |
| GPU | `runtime/xnuhost` (virtio-gpu 3D), `kernel/patches/oneios-hosted/gpu/` | DRM native context served by Droid-VM's drm2kgsl on the Adreno 750 through KGSL, guest pages shared by udmabuf; the 2D scanout for the first frame. | [gpu.md](gpu.md) |
| Display | `runtime/display/` | `oneios-display`: on One UI's side, an `app_process` helper that puts xnuhost's frames (`session/display.sock`) on a full-screen surfaceflinger layer at the panel's mode. | [runtime/display/README.md](../runtime/display/README.md) |
| Hardware bridges | `runtime/bridges/`, `drivers/` | Host halves (in xnuhost or small helpers) and iOS-side drivers for: CPU topology and clocks, network (`en0` over a tap with its own routing), Bluetooth (the HIDL HAL over hwbinder), haptics (the CS40L26 force-feedback device), audio (AAudio), brightness (backlight sysfs) and the light sensor, camera (the camera provider HAL in a session, the NDK on One UI). | [hardware.md](hardware.md) |
| Runtime package | `runtime/package.sh`, `.github/workflows/` | `oneios-runtime-<ver>-arm64.tar.zst` with `runtime.json`: xnuhost, the helpers and the phone check tools. Published on GitHub releases. No kernel, nothing of Apple's. | [releases.md](releases.md) |
| OneOS fork | `akshaynexus/OneOS`, branch `oneios` | The "iOS 27 (OneiOS)" card (S24 Ultra only): downloads the runtime, imports and verifies the image set, Hardware checks, and the session scripts: arm the way back, stop zygote, start xnuhost and the display helper, return in place or by reboot. | [oneos/interface.md](../oneos/interface.md), the fork's `docs/oneios.md` |

## A session, step by step

1. **Stage** (in the app, One UI running): the runtime package and the image set are verified against their
   manifests and staged as root under `/data/local/tmp/oneios/{runtime,image,data,session,control}`.
2. **Arm the way back:** the watcher, the Volume Up + Volume Down readers and a wakelock start before anything stops.
3. **Stop One UI's framework:** zygote (and so system_server). The kernel, vendor HALs, surfaceflinger and the
   composer keep running.
4. **Start xnuhost** in a mount namespace with the stage over `/data`, in its own cgroup, then `oneios-display`.
5. **Boot:** xnuhost loads `kernel.kc`; XNU mounts the OS volume (verified by xnuhost), on first start formats the
   Data volume and unpacks the seed, mounts the cryptexes, starts launchd; backboardd draws, xnuhost writes
   `session/ready`.
6. **Return:** Volume Up + Volume Down for 3 s, a return request from inside iOS, xnuhost exiting, or no `ready` in
   time. The return ends xnuhost (SIGTERM, then SIGKILL), restores One UI's backlight, restarts the composer and
   surfaceflinger, restarts zygote; any failed check reboots into One UI instead. Power + Volume Down (Samsung's
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
- Not Apple's kernel binary: XNU is built from Apple's open source; only Apple's kexts and userland are Apple's,
  taken from the owner's own IPSW download and never redistributed.
