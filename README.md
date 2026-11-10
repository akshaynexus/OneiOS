# OneiOS

iOS 27 next to One UI on a rooted Samsung Galaxy S24 Ultra (SM-S928, SM8650), switched to and back
the way [OneOS](https://github.com/fahadalijaved/OneOS) switches to another Android: nothing flashed,
the image lives in a folder, One UI's kernel keeps running.

OneOS's guests are Android systems that run unchanged on One UI's Linux kernel. iOS's userland needs
Apple's kernel (XNU), so OneiOS runs a source-built XNU **as a process on One UI's kernel**
(`runtime/xnuhost`): iOS's processes are Linux processes whose system calls and faults go to it. The
image, the switch and the return stay OneOS-shaped. How it fits together: [docs/architecture.md](docs/architecture.md).
How to build and test it: [docs/building.md](docs/building.md).

## Status

**Experimental. iOS does not boot to the home screen yet. Nothing has run on the phone yet.**

On the PC (xnuhost under `qemu-aarch64`, 8 vCPUs, the real iOS 27.0.1 `24A446` image set, `kernel.kc` with the
iPhone18,2 kexts), the boot gets through:

- corecrypto's POST, AppleImage4, AMFI, Seatbelt, the trust caches, IOKit matching, `bsd_init`;
- the root mount from the image set (verified by xnuhost), swap, pid 1;
- the Data volume's first start: `newfs_apfs` (the first user process), the dyld shared cache's 4K code signatures
  (5.8 GiB hashed once per boot, about 3 minutes under qemu), the cache mapped and slid, the volume mounted, the
  seed unpacked and verified by the kernel;
- **launchd**: all of its early boot tasks ("Early boot complete. Continuing system boot."), then about 125
  processes, among them `SpringBoard`, which starts and exits (signal 5) again and again.

Not reached: a SpringBoard that stays up, a frame on the display, `session/ready`. What is missing for that, as
seen in the log: the guest has an IOMobileFramebuffer since `0020` (`kernel/patches/oneios-hosted/display/`: it
starts, the console reaches `display.sock` through it, and backboardd opens it), but backboardd has not submitted a
frame: in the run that got there it died in a workqueue thread's start first. There is no touch digitizer yet,
and some processes die with "Text page corruption detected". The key store is in since `0022`: an AppleKeyStore
service in software (no passcode, class keys from a constant, key wrap by AES; **not a security boundary**), with
which launchd's keybag task ends in `DONE MKB_INIT` instead of "FATAL KEYBAG ERROR: failed to load system bag";
securityd wraps and unwraps its keys through it. A host with one kind of CPU (or one vCPU) used to assert in the
scheduler at launchd's first `workq_open`; `0016` fixes that (booted with eight CPUs of one kind). Detail: [kernel/patches/oneios-hosted/README.md](kernel/patches/oneios-hosted/README.md)
and the board, [docs/agents.md](docs/agents.md).

Built and tested on the PC, not yet on the phone: xnuhost and its loader, the GPU path (drm2kgsl on KGSL), the
display helper, the hardware bridges (CPU, network, Bluetooth, haptics, audio, brightness and the light sensor,
camera), the image pipeline, the runtime package and releases, and the OneOS fork's iOS 27 card, session scripts and
Hardware checks. Not done: the camera's iOS-side capture device, auto-brightness's last link into CoreBrightness,
the Metal plug-in build for the iPhone cache. Never: passcode, Face ID, Wallet, calls, SMS.

## Layout

```
docs/architecture.md  how the parts fit; docs/building.md  how to build and test; docs/*.md  each part in detail
profiles/             the pinned IPSW profile (iPhone18,2 iOS 27.0.1 24A446)
scripts/fetch/        fetch only the needed IPSW members from Apple's CDN (never from here)
scripts/image/        build the image set (MysticGSI-style manifest, SHA-256, .tar.zst)
userland/             iOS 27 root and Data tweaks (the image hook), guest helpers, the camera plug-in
kernel/               XNU 12377 (UniXNU / Droid-VM fork) built on Linux; oneios-hosted/ patches; prelink/ (kernel.kc)
runtime/              xnuhost and its loader, the display helper, the hardware bridges, the runtime package
drivers/              iOS-side drivers for the bridges (CPU, network, Bluetooth, haptics, audio, brightness, camera)
oneos/                the contract with the OneOS fork (akshaynexus/OneOS, branch oneios)
```

## Rules

- **No Apple files in git or in releases.** IPSWs, cryptexes, kernelcaches, kexts, `kernel.kc` and every image built
  from them stay in `out/` and `inputs/` (git-ignored). Scripts fetch them from Apple and build them on the owner's
  machine. Releases carry only this project's own code (scripts, runtime binaries, the OneOS patches); the kernel is
  built locally (it links Apple's corecrypto).
- Long jobs run in tmux: `scripts/bg.sh NAME COMMAND...`.
- Every change updates the docs that describe it: [CLAUDE.md](CLAUDE.md).

## Credits

[OneOS](https://github.com/fahadalijaved/OneOS) (fahadalijaved) and
[OneLineage](https://github.com/WeWaffles/OneLineage) (WeWaffles): the container hot-switch.
[MysticGSI](https://github.com/MysticGSI/mysticgsi): the image build model.
[UniXNU](https://github.com/UniXNU/UniXNU) and [Droid-VM](https://github.com/Droid-VM): XNU on non-Apple
ARM64, drm2kgsl, the guest drivers. [iOSPlus10](https://github.com/akshaynexus/iOSPlus10): XNU and iOS userland on
Qualcomm phones, haptics and brightness. [Darling](https://github.com/darlinghq/darling): Mach and XNU on Linux.
