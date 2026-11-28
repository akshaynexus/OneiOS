# OneiOS

> iOS 27 next to One UI on a rooted Samsung Galaxy S24 Ultra — switched to and back the way OneOS switches Androids. Nothing flashed, the image lives in a folder, One UI's kernel keeps running.

**Status: experimental, under active development.** On the phone it boots to the Home Screen (unlock works, GPU-composited, touch, keyboard and S Pen work) and returns to One UI. See [what works](#what-works) and [known issues](#known-issues) below — both are kept honest. The phone results below are of the C runtime: the host runtime (xnuhost, its loader, the bridges, the display helper) was rewritten in Rust on 2026-11-23 ([runtime/RUST.md](runtime/RUST.md)); its host tests pass and it boots the test kernel under qemu, but **no phone has run the Rust runtime yet**.

OneOS's guests are Android systems that run unchanged on One UI's Linux kernel. iOS's userland needs Apple's kernel (XNU), so OneiOS runs a source-built XNU **as a process on One UI's kernel** (`runtime/xnuhost`, Rust): iOS's processes are Linux processes whose system calls and faults go to it. How it fits together: [docs/architecture.md](docs/architecture.md).

## Quick start

Prerequisites: the build host is a Mac (Xcode, NDK 28.2, Rust 1.95+ with the targets `aarch64-linux-android` and `aarch64-unknown-none`; Linux-only host tests run in Apple's `container`), or a Linux x86_64 PC with `qemu-aarch64`; the pinned Apple IPSW pieces (fetched, never committed). Full flow: [docs/building.md](docs/building.md).

```sh
P=iphone18,2-ios27.0.1-24A446
scripts/fetch/fetch.sh --list "$P"          # compare the profile with Apple's IPSW (~1 MiB)
scripts/bg.sh fetch scripts/fetch/fetch.sh "$P"   # the IPSW members from Apple, into inputs/
kernel/fetch.sh                             # the kernel's sources at the commits of kernel/pins.sh
scripts/bg.sh image scripts/image/build.sh "$P" --kernel out/kernel/24A446/kernel.kc
scripts/bg.sh kernel-hosted env HOSTED_OUT=me kernel/build-hosted.sh
scripts/bg.sh prelink env KERNEL=out/kernel/current-hosted-me/xnu.macho kernel/prelink/build.sh
runtime/build.sh STAGE                       # -> STAGE/bin/xnuhost and helpers
```

Long jobs run in tmux via `scripts/bg.sh NAME COMMAND...`; check them with `scripts/bg.sh -s NAME`.

Every recent change has a switch in the image's `xnuhost.conf` (or a kernel boot-arg), so one boot tests them all and
one setting turns one back: [docs/switches.md](docs/switches.md) has the table and a sample conf.

## What works

| Area | Phone (S24 Ultra) | PC (qemu-aarch64) | Notes |
|---|---|---|---|
| Boot to Home Screen | Yes — lock screen, swipe unlock, apps shown (hosted 0035–0037) | Yes — launchd, SpringBoard, backboardd | [docs/booting](docs/building.md) §6 |
| GPU compositing | Yes, via virtio-gpu: 40–58 fps on the idle lock screen (2026-11-11); 20–32 fps in launches and Control Center (2026-11-22, cause found: see "Known issues") | Yes, frames captured | [docs/gpu.md](docs/gpu.md) |
| Touch + buttons | Yes | Scripts (`--input-script`) | HID via kernel event driver |
| Battery | Published to iOS | Stand-in tested | Real readings on phone |
| Wi-Fi | Joins and gets its DHCP lease; scan rate cut 20→5/min (reuse works); the late enable proven (2026-11-22: `en0` up 0.05 s after configd names it). 2026-11-23 18:24: pages did not load or were very slow, the bars never moved: a traffic line, a receive ring that waits instead of dropping, low-priority scans while joined and the signal event (RSSI_CHANGED) built. 19:40 on the phone: they work (RSSI_CHANGED posted, 605 MiB to iOS in a minute); the uplink dropped frames on a full send buffer (`EAGAIN`) and the driver took no low-priority scan: frames now wait for `POLLOUT`, scans while joined answered from the last scan for 120 s, host-tested, not booted | Sim radio (`wifi=sim`) | [docs/hardware.md](docs/hardware.md) |
| Keyboard | Yes (hosted 0083, 2026-11-22): Spotlight took 30 key taps, its hangs 3.8 s in all (86.4 s before) | Host-tested | The busy keyboard was the dictation availability loop, kernel README "0083" |
| Haptics | The haptic server starts (0078); the Actuator then played silence (no DSP chain): hosted 0086 merged, phone proof pending | Host-tested | [docs/hardware.md](docs/hardware.md) §5 |
| Interrupts / Mach RPC | — | Verified: 41k host-call IPIs vs 110 trapped | `oneios-perf` `rpc:`/`0081:` lines |
| Return to One UI | Yes — chord, SIGKILL backstop | n/a | Never touches side keys, realtime, or `/sys/power` |
| Host test suites | n/a | All green: the Rust runtime's `cargo test --workspace` and clippy, the test kernel's five boots under qemu (`runtime/xnuhost/check-arm64.sh`); the prelink audit 0 failures | `runtime/host-tests.sh`, [runtime/RUST.md](runtime/RUST.md) |

### The owner's report of 2026-11-25 (Galaxy S24 Ultra, OneiOS v0.1.3x)

- **Boot:** iOS boots on the phone, semi-stable.
- **Audio:** works, but laggy. Volume control, the volume keys and the power key work.
- **Thermal:** the phone runs hot.
- **Wi-Fi and Bluetooth:** work.
- **S Pen:** works as an Apple Pencil with pressure sensitivity and hover on iOS 27.2.1 (the owner, 2026-11-28, runtime
  v0.1.44 and later: [the post](https://www.reddit.com/r/hackios/comments/1y7j0oz/s_pen_emulated_as_apple_pencil_on_ios_27/)).
  The rebuilt 27.2.1 image had lost `pen=auto`; since v0.1.41 the S24 Ultra's host row turns it on.
- **Haptics:** work.
- **Camera:** work in progress: the Camera app shows a black preview.
- **Video playback:** works, but laggy.
- **Brightness control:** works (the slider). Auto-brightness from the front light sensor (v0.1.29 and later) is not
  confirmed yet.

## Known issues

| Issue | State | Tracking |
|---|---|---|
| SpringBoard died once on unlock (missing process identity assertion) | Historical, not reproduced since | git history, agents board |
| Launch, Control Center and widget lag; low frame rate on page changes | 2026-11-23: hosted 0089 makes the scheduler's completion step run (proven on the phone: `oneios-smp: all 8 CPUs booted`, `oneios-edge: E -> P migration and steal allowed`, all 8 vCPUs busy); that started a move storm (70,200 thread moves in 30 s, frame interval 110 ms): hosted 0091 (no pull-back of spilled threads), xnuhost's direct kick and the display helper's earlier answer are merged, not booted yet. The plan to 120 fps (backboardd's faults, the frame copy, the fence time): docs/gpu.md 2.5 | kernel README "0089", "0091"; docs/HANDOFF-2026-11-23.md |
| The display glitched on UI changes (page changes, widget updates) | Fixed (each layer buffer filled with what it lacks, `69e2013`); on the phone 2026-11-23 the glitch is gone | [runtime/display/README.md](runtime/display/README.md) |
| Fitness shows black for 80–90 s at its first start | Not a render fault: its first screen waits on Fitness+ calls that fail slowly with no network (2026-11-22); 0085 removes the CPU part, the rest needs Wi-Fi joined | [docs/userland.md](docs/userland.md) §11 |
| iOS froze about 6 minutes after a boot (two boots of 2026-11-23) | Fixed: the encoder helper's open waited for ever on a vCPU, so a stackshot's cross-call panicked the guest and the session did not end. The waits are bounded now; on the phone (2026-11-23) the helper hung twice, was killed after 2.5 s each time, and iOS went on with no panic. Why MediaCodec's open hangs is still open | docs/booter.md 2.9c, docs/hardware.md 7g |
| Typing heat (Preferences 100%+ of a CPU, 2026-11-15) | The keyboard's busy main thread was the dictation availability loop (hosted 0083, the keyboard works on the phone); Preferences' CPU while typing not measured again since | [docs/hardware.md](docs/hardware.md) §5 |
| Wi-Fi scan and join slow | Scan reuse 20→5/min works; result/join latency still slow on the phone (cause open). The enable no longer waits on configd (`oneios-wifi-up`, proven 2026-11-22) | [docs/hardware.md](docs/hardware.md) |
| Audio: MMAP start gap, lateness metric | Landed on PC, phone proof pending | [docs/hardware.md](docs/hardware.md) |
| Volume keys and slider did nothing, then no sound at all | Fixed and proven on the phone 2026-11-23 (the owner: the ringtone plays, the volume keys work; `DroidVMAudio: speaker volume 116 of 128`): the Speaker's hardware volume control and the host at full scale (2026-11-22), and the camera's model name `OneiOSAP` (hosted 0088), because `V54AP` made VirtualAudio build a webcam route whose precondition failed, so audio never started | [docs/hardware.md](docs/hardware.md) §6 |
| Haptics: ringtones do not vibrate | Phone 2026-11-23: keyboard haptics work (Core Haptics through the Actuator and 0086's chain); a ringtone's play or change gives no vibration (that path is the system sound's vibration, not Core Haptics: a guess, not traced). `oneios-tool ioslog haptics` (on main; `phone pull-log` writes `ioslog-haptics.txt`) shows a haptic's path step by step; the boot of 2026-11-23 has no ringtone in its pulled log (`system-sound: with vibration 0, sound only 1`), so the next boot plays one | docs/hardware.md §5; docs/HANDOFF-2026-11-23.md |
| HEVC encode | Works on the phone (the owner, 2026-11-23). Before that (2026-11-22) iOS 27's tile messages were answered: widgets render (no more white fallbacks), encode-fail 0, the session loop gone. The 10-bit frames (P010) since 2026-11-24 are not booted yet | [codec README](kernel/patches/oneios-hosted/codec/README.md) |
| xnuhost ended in the GPU renderer with the wallpaper gallery open (`hash_table.c:311 ... key_pointer_is_reserved`, 2026-11-23 13:51; the watcher brought One UI back) | 0009 (booted 18:24) was not the whole fix: xnuhost ended the same way at 18:35 (`out/session-logs/2026-11-23-mac-s1824`). Fixed on main, not booted. The device refused Unity2025Poster's 80 MiB blobs (20480 entries; its limit was the display's 16384, the guest's 3D driver sends up to 65532): the device now takes 65532. The refused BO kept res_id 0 in drm2kgsl's blob table, and its context's end searched the resource table for 0: drm2kgsl patch 0010 (`drm_context.c`). Host tests `refused_blob_then_context_end` (reproduced the abort, passes now), `blob_entries_of_the_guest_driver`; 0009's `zero_ids` still passes. The `gpu: 10 s: host blobs` line ends with `refused R` now. The wallpapers render black and the host swaps while the gallery is open (5,312 blobs, 2,441 MiB) | docs/gpu.md 2.4a, 2.4b; docs/HANDOFF-2026-11-23.md 4a, 4b |
| Camera: the app opens, the preview is black | Phone 2026-11-23: the back stream is kept now (the VM hidden from cameracaptured) and asked for, then CMCapture stopped on the plug-in's missing sensor ID and formats, and our Metal plug-in refused a heap and a function (cameracaptured crashed and restarted). Fixed on main (the plug-in's `ModuleInfo` sensor ID and CMCapture's format rule; hosted 0090 brings a new plug-in at boot); the Metal fixes are in a bundle built on the laptop (2026-11-23, branch c merged). Not booted. Boot of 13:45: the Camera app opened on Front Camera, which had no format (V54 keeps them in deviceInfoBase files the plug-in did not read), so its session had no input; fixed on main (commit `a5c244d`, with CMCapture's companion steps and WhiteBalanceGain), not booted | docs/hardware.md §7, userland/camera/README.md |
| No sound, no volume, no haptics (v0.1.16) | Fixed and proven on the phone 2026-11-23 (the ringtone plays, the volume keys work): the camera's model name is `OneiOSAP` (hosted 0088), because `V54AP` made VirtualAudio build a webcam route whose precondition failed | docs/hardware.md §6 |
| Brightness: any level gives almost the lowest brightness | Phone 2026-11-23 (the owner): every level ended near the dimmest. Cause found in 24A446's CoreBrightness: it used frame-synchronous brightness (`UseSyncBrightnessTransactionForDisplay: 1`), which goes to CoreAnimation, and QuartzCore has no brightness handler here; the backlight service got only a truncated float and slider positions of 0. Fixed on main, not booted: backboardd's `useSyncBrightnessTransactionForDisplay:` answers NO so CoreBrightness sends `brightness-nits`, `LmidProduct`/`LminProduct` as 16.16 nits, `brightness-nits` wins over the slider position | drivers/brightness/README.md "Why every level was the dimmest"; docs/hardware.md §7a |
| Auto-brightness via light sensor | Attaches, no readings through it yet | [docs/hardware.md](docs/hardware.md) |
| Shared-region protect no-op count still 0 | Optimization unproven | kernel README "0080" |
| Mac kernel panics in first 2 s | Old news, Mac runtime boots with Linux `kernel.kc` | `kernel/prelink/README.md` |
| Kernels built from `main` between the A56 merge (`b2fa083`) and 2026-11-21 died at launchd (`mach_ports_lookup() failed: -300`, panic `initproc exited`) | Fixed: hosted 0068 is opt-in (`oneios-legacy-registered-ports=1`); PC boot to SpringBoard again | `kernel/patches/oneios-hosted/README.md` (A56 section) |
| Never goals | Passcode, Face ID, Wallet, calls, SMS | — |

## Devices

Primary target: the rooted Galaxy S24 Ultra. S25, S25 Ultra, S26 Ultra and Galaxy A-series run in forks; portable work lands here. Full map: [docs/forks.md](docs/forks.md).

| | Device | Status | Best result |
|---|---|---|---|
| ![Galaxy S24 Ultra](docs/images/s24-ultra.jpg) | Galaxy S24 Ultra (SM-S928, SM8650) — **this repo** | Boots to Home Screen, returns to One UI | Daily driver target |
| ![Galaxy S25 Ultra](docs/images/s25-ultra.jpg) | Galaxy S25 Ultra (SM-S938, SM8750) — [YumikoHayamato/OneiOS](https://github.com/YumikoHayamato/OneiOS) (branch `s25-ultra-sm8750`), denis fork, [s25ver snapshot](https://github.com/xaxaapple/s25ver) | SpringBoard, touch and the key return on device (fork sessions 1-7); ported: Oryon topology, Adreno 830 fences, the thermal trips rule, udmabuf ENOMEM fallback | Big-only-SoC bug fixed for all |
| | Galaxy S25 (SM-S931B, SM8750) — [NoNameBuilder/OneiOS](https://github.com/NoNameBuilder/OneiOS) | Source-level port, no on-device run; ported: host profile, stand-in tests, firmware facts | `panel-fallback=1080x2340@120` by detection |
| ![Galaxy S26 Ultra](docs/images/s26-ultra.jpg) | Galaxy S26 Ultra (SM-S988, SM8850) — [YMGPwcca/OneiOS](https://github.com/YMGPwcca/OneiOS) | ~600 s iOS run on device | Haptics + backend tests ported |
| ![Galaxy A56](docs/images/a56.jpg) | Galaxy A56 (SM-A566E, Exynos 1580) — [Musicalista/OneiOS-A56](https://github.com/Musicalista/OneiOS-A56) | iOS 16.5 to lock screen on device; touch works in the fork since 2026-11-21 (its kernel patch 0079: here hosted 0103 since 2026-11-24, in the release kernel `...-16bd44f9d737`, not booted here) | GPU template, bootstrap, triage tools; since 2026-11-23 our own iOS 16.5 image and the OneOS app's A56 path; since 2026-11-24 the fork's touch, Wi-Fi interface, LaunchServices, log flush, frame and idle-jobs fixes and its AIDL Bluetooth client ([docs/a56.md](docs/a56.md) "The second port"; on an A56 our runtime reached the lock screen on 2026-11-24, touch did not work then); since 2026-11-24 a GPU path for its Xclipse 540 (a Rust amdgpu renderer in xnuhost, Mesa radeonsi on Darwin, Jesvi-iOS's Metal layer and iOSPlus10's OpenGL ES layer so backboardd can composite on it, the accelerator in the kernel): built and host-tested, **not booted** ([docs/a56-gpu.md](docs/a56-gpu.md)) |

Device photos: official Samsung product images ([samsung.com](https://www.samsung.com/us/smartphones/galaxy-s25-ultra/), Samsung Newsroom press kits), © Samsung Electronics — used for device identification. A56 photo: [Captainmorlypogi1959 via Wikimedia Commons](https://commons.wikimedia.org/wiki/File:Samsung_Galaxy_A56_5G_2025_(cropped).jpg) (CC BY-SA 4.0).

## Layout

```
docs/architecture.md  how the parts fit; docs/building.md  how to build and test; docs/*.md  each part in detail
docs/forks.md         device forks: what runs where, what was ported back, the multi-device pattern
profiles/             the pinned IPSW profile (iPhone18,2 iOS 27.0.1 24A446)
scripts/fetch/        fetch only the needed IPSW members from Apple's CDN (never from here)
scripts/image/        build the image set (manifest, SHA-256, .tar.zst)
userland/             iOS 27 root and Data tweaks (the image hook), guest helpers, the camera plug-in
kernel/               XNU 12377 (UniXNU / Droid-VM fork) built on Linux; oneios-hosted/ patches; prelink/ (kernel.kc)
runtime/              xnuhost and its loader, the display helper, the hardware bridges, the runtime package (Rust: runtime/RUST.md)
drivers/              iOS-side drivers for the bridges (CPU, network, Bluetooth, haptics, audio, brightness, camera)
oneos/                the contract with the OneOS fork (akshaynexus/OneOS, branch oneios)
```

## Rules

- **No Apple files in git or in releases.** IPSWs, cryptexes, kernelcaches, kexts, `kernel.kc` and every image built
  from them stay in `out/` and `inputs/` (git-ignored). Scripts fetch them from Apple and build them on the owner's
  machine. Releases carry only this project's own code (scripts, runtime binaries, the OneOS patches). The default
  kernel is built locally (it links Apple's corecrypto); a release carries only the hosted `ONEIOS_CRYPTO=original`
  `xnu.macho` (no Apple code in it), which each phone prelinks into its own `kernel.kc` ([docs/releases.md](docs/releases.md)).
- Long jobs run in tmux: `scripts/bg.sh NAME COMMAND...`.
- Every change updates the docs that describe it: [CLAUDE.md](CLAUDE.md).

## License

No license file yet — all rights reserved until the owner adds one.

## Credits

[OneOS](https://github.com/fahadalijaved/OneOS) (fahadalijaved) and
[OneLineage](https://github.com/WeWaffles/OneLineage) (WeWaffles): the container hot-switch.
[MysticGSI](https://github.com/MysticGSI/mysticgsi): the image build model.
[UniXNU](https://github.com/UniXNU/UniXNU) and [Droid-VM](https://github.com/Droid-VM): XNU on non-Apple
ARM64, drm2kgsl, the guest drivers. [iOSPlus10](https://github.com/akshaynexus/iOSPlus10): XNU and iOS userland on
Qualcomm phones, haptics and brightness, and NegroniGL (the A56's OpenGL ES layer). [Jesvi-iOS](https://github.com/jesvi-eulah/Jesvi-iOS) (jesvi-eulah): Metal on Mesa Gallium for iOS 16.5 (the AIR frontend and Metal
device of the A56's plug-in). [OneiOS-A56](https://github.com/Musicalista/OneiOS-A56) (Musicalista): the A56 port and its
amdgpu native-context renderer the Rust one is written from. [Darling](https://github.com/darlinghq/darling): Mach and XNU on Linux.
Changes ported from forks of this repository, by their authors: [CREDITS.md](CREDITS.md).
