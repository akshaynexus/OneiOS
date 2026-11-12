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

**Experimental. On the phone (S24 Ultra, 2026-11-11) the boot reaches launchd, backboardd and SpringBoard, and backboardd's first frames are on the panel: the Apple boot logo at 1440x3120. Whether it goes on to SpringBoard's own screen is not yet seen.**

Touch on the phone: in that session the DriverKit event driver was waited for, so the touch screen and the buttons
reached backboardd only 233 s after they started (then 447 events, 11 contacts). The kernel now gives both to
Apple's kernel event driver at once (`kernel/patches/oneios-hosted/hid/README.md`, "Which event driver"); built,
not yet run on the phone. Whether a swipe up unlocks is not known.

Second phone session (2026-11-11, with the Metal plug-in mounted): touch attached at once, the battery was
published, backboardd opened the AGX device user client, and the kernel then died in a data abort: a wrong pair in
the collection (`IOMemoryDescriptor::setOwnership` bound to `CreateMapping`, `kernel/prelink/README.md`). Corrected;
the collection builds; not yet run on the phone.

Later sessions (2026-11-11): with the corrected collection and the Metal plug-in's grant the phone reaches the
lock screen and, after unlock, the Home Screen, composited on the GPU at 40 to 60 frames a second; touch works.
The Home Screen had no apps and SpringBoard died once on unlock (an assertion about a missing process identity):
the removable system apps were never shown in `/Applications`. Hosted `0035` shows them, `0037` keeps them
registered after the first boot of a Data store, `0036` drops icons drawn blank by earlier boots; built, not yet run.

First Mac builds on the phone (2026-11-12): the Mac `kernel.kc` panics in the first 2 s, in both builds tried
(507 stubs pointing past the collection with XnuPrelink `b74ed7c`; then an original kernel function left on the
stub, called by Sandbox; `kernel/prelink/README.md`, "On the phone"; agent prelink audits it). The Mac runtime
0.1.7-dev with the Linux `kernel.kc` of 2026-11-11 boots to `session/ready` in about 80 s and to the lock screen;
the Linux runtime 0.1.6 does the same, and the owner unlocked it to the Home Screen (empty: that kernel has no
0035-0037). In both, the screen drew far fewer frames than on 2026-11-11 (0-20 per 10 s; cause not known) and the
Volume Up + Volume Down chord did not return; five returns by `session/return.request` or after a panic all came back
to One UI in place. Numbers per area: `docs/hardware.md` 0.1; the test cycle: `docs/building.md` section 6.

The Mac `kernel.kc` with the known-good pairs (2026-11-12, agent prelink; `kernel/prelink/README.md`, "Known-good
pairs" and "On the phone (2026-11-12, prelink)"): kernel and collection from origin/main `924a0d9` (series through
0039), runtime 0.1.8-dev. Boot frame at +102 s, then about 11 minutes of lsd registering the staged apps (the first
boot of 0035/0037 on this Data store) while SpringBoard waited for the LaunchServices database; then the lock screen,
the owner's swipe unlocked it, and the Home Screen showed the apps (their icons do not draw right yet). No panic and
no call to the panic stub in 23 minutes. This set is the phone's baseline now.

On the PC (xnuhost under `qemu-aarch64`, 8 vCPUs, the real iOS 27.0.1 `24A446` image set, `kernel.kc` with the
iPhone18,2 kexts), the boot gets through:

- corecrypto's POST, AppleImage4, AMFI, Seatbelt, the trust caches, IOKit matching, `bsd_init`;
- the root mount from the image set (verified by xnuhost), swap, pid 1;
- the Data volume's first start: `newfs_apfs` (the first user process), the dyld shared cache's 4K code signatures
  (5.8 GiB hashed once per boot, about 3 minutes under qemu), the cache mapped and slid, the volume mounted, the
  seed unpacked and verified by the kernel;
- **launchd**: all of its early boot tasks ("Early boot complete. Continuing system boot."), then its services:
  123 spawned and about 190 processes in 16 minutes, with **`backboardd` and `SpringBoard` running to the end of
  the run** (series 0001-0025, 2026-11-11; the run was stopped by its time limit, no kernel panic). The only
  service that died was `chronod` (abort in QuartzCore's `query_displays`).

**SpringBoard finishes launching**: 15 minutes into a PC boot its main thread is in `UIApplicationMain`'s run loop
(`SBSystemAppMain` -> `UIApplicationMain` -> `GSEventRunModal`) and stays there (sampled every 5 minutes for half
an hour with `oneios-ubt`); backboardd sits in its run loops with CoreAnimation's render server and IOMFB server
threads up.

Not reached: a frame of iOS's on the display, `session/ready`. The guest has an IOMobileFramebuffer since `0020`
(`kernel/patches/oneios-hosted/display/`: it starts, backboardd opens it), and `display.sock` shows one flat colour
about twice a second. On the PC the guest has no 3D GPU (`oneios-gpu: the host offers no 3D`, so no Metal
accelerator); whether that is why backboardd composites nothing is the likely reason, not a proven one: the phone,
with KGSL, is where it shows. Touch and the side keys are
in (`kernel/patches/oneios-hosted/hid/`): a Digitizer / Touch Screen service and a buttons service that backboardd
opens and gets events from, fed from xnuhost's input device (scripted touches on the PC; with no frame, nothing
to see react).
The processes that died with "Text page corruption detected", dyld assertions or a lost Linux thread no longer do
(`0025` and xnuhost's task layer: 14 minutes past early boot with 129 processes and none of those). The key store is in since `0022`: an AppleKeyStore
service in software (no passcode, class keys from a constant, key wrap by AES; **not a security boundary**), with
which launchd's keybag task ends in `DONE MKB_INIT` instead of "FATAL KEYBAG ERROR: failed to load system bag";
securityd wraps and unwraps its keys through it. A host with one kind of CPU (or one vCPU) used to assert in the
scheduler at launchd's first `workq_open`; `0016` fixes that (booted with eight CPUs of one kind). Detail: [kernel/patches/oneios-hosted/README.md](kernel/patches/oneios-hosted/README.md)
and the board, [docs/agents.md](docs/agents.md).

Built and tested on the PC, not yet on the phone: xnuhost and its loader, the GPU path (drm2kgsl on KGSL), the
display helper, the hardware bridges (CPU, network, Bluetooth, haptics, audio, brightness and the light sensor,
camera), the image pipeline, the runtime package and releases, and the OneOS fork's iOS 27 card, session scripts and
Hardware checks, and the iOS user-space cross toolchain with the Metal plug-in built for the iPhone's shared cache
(not loaded by any boot yet). Not done: the camera's iOS-side capture device, auto-brightness past Apple's light
sensor service (it attaches to our HID device; no reading has gone through it). Never: passcode, Face ID, Wallet, calls, SMS.

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
