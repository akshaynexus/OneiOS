# Building and testing OneiOS

Everything builds on a Linux x86_64 PC, iOS user-space code included (section 9); no Mac is needed. Long steps run in tmux with `scripts/bg.sh NAME CMD...` (log in `logs/NAME.log`, exit
code in `logs/NAME.exit`, state with `scripts/bg.sh -s NAME`). tmux does not pass environment variables: put them
inside the command (`scripts/bg.sh x env VAR=1 cmd`).

Nothing of Apple's is ever committed or released: the IPSW parts, the image set and `kernel.kc` stay in `inputs/`
and `out/`.

## 0. Prerequisites

| Tool | For | Notes |
|---|---|---|
| python3 3.11+, zstd, tmux, git, curl, patch | everything | `patch`: xnuhost's Makefile applies `runtime/xnuhost/drm2kgsl/patches` to a copy of drm2kgsl |
| clang (stock, recent) | the kernel | the kernel build uses stock clang plus xnu-linux-toolchain's ld64 (set up by the kernel scripts, no root) |
| Android NDK 28.2 | xnuhost, the helpers, the phone tools | `$HOME/Android/Sdk/ndk/28.2.13676358`, or `$NDK` / `$ANDROID_NDK_HOME` |
| Android SDK build-tools (`d8`) and a JDK (`javac`) | the display helper's dex | |
| apfs-fuse | reading the OS volume for the Data seed | github.com/sgan81/apfs-fuse; read-only, no root |
| qemu-user (`qemu-aarch64`) | booting xnuhost and the kernel on the PC | |
| `ipsw` CLI | AEA decryption | `scripts/fetch/get-ipsw-cli.sh` installs the pinned version to `out/bin/ipsw` |
| mksh, dash (optional) | testing the phone scripts the way Android runs them | |
| gh | releases | |
| xtool, an Xcode `.xip`, rustup 1.98.0 with `rust-src`, meson, ninja, glslang, glslc, spirv-val, bison, flex, PyYAML | iOS user-space code: the cross toolchain and the Metal plug-in | section 9; `scripts/setup/ios-toolchain.sh` says what is missing |

Source references (shallow clones into `external/`, git-ignored):

```sh
scripts/setup/get-references.sh        # the main table of docs/references.md (-l lists, -a adds the extras)
kernel/fetch.sh                        # the kernel's sources at the commits of kernel/pins.sh
```

## 1. The iOS 27 image set

```sh
P='profiles/ios-27.0.1-iPhone18,2.toml'
scripts/fetch/fetch.sh --list "$P"                  # optional: compare the profile with Apple's IPSW (about 1 MiB)
scripts/bg.sh fetch scripts/fetch/fetch.sh "$P"     # 10.6 GiB of the IPSW's members from Apple, into inputs/24A446/
scripts/image/kernel-inputs.sh "$P"                 # Apple's kernelcache and device tree -> out/kernel-inputs/24A446/
```

The set is built after `kernel.kc` exists (step 3), so that it carries the kernel:

```sh
scripts/bg.sh image scripts/image/build.sh "$P" --kernel out/kernel/24A446/kernel.kc
# -> out/image/24A446/ and out/image/OneiOS_iOS27.0.1_24A446_iPhone18-2.tar.zst (+ .sha256), about 11 GB
scripts/image/verify.sh out/image/OneiOS_iOS27.0.1_24A446_iPhone18-2.tar.zst
```

`--no-kernel` builds a set without one (for work on the images only). The userland hook (`userland/image-hook`)
runs by default. Format: [image-format.md](image-format.md).

## 2. The kernel

```sh
scripts/bg.sh kernel-hosted env HOSTED_OUT=me kernel/build-hosted.sh
# -> out/kernel/current-hosted-me/xnu.macho (the hosted kernel: board series + kernel/patches/oneios-hosted/series)
```

- The kernel is built from the checkout whose `kernel/build-hosted.sh` you run: its `drivers/` and
  `kernel/patches/oneios-hosted/*` (the script exports `ONEIOS_REPO`). Before 2026-11-11 the archive scripts
  found the repository from the build directory's resolved path, so a worktree whose `out/` is a link into
  another checkout built **that** checkout's sources (the first testing image's kernel had the shared checkout's
  older `drivers/`: `oneios_drivers_start` without the HID start, no touch). A quick check of a built kernel:
  `llvm-objdump -d --disassemble-symbols=_oneios_drivers_start xnu.macho | grep oneios_hid_input_start`.
- `HOSTED_OUT=<name>` gives you your own tree and output; without it the build uses the default tree
  (`out/kernel-hosted`, `out/kernel/current-hosted/`), which belongs to the lead.
- The build compiles UniXNU's sources from `external/` (`scripts/setup/get-references.sh`): among them
  `NativeHidInput` at 24f1f3f for the touch screen and buttons; a checkout from before 2026-11-11 is older and the
  build says so (remove `external/NativeHidInput` and fetch again).
- Incremental work on one patch: `kernel/hosted-dev.sh` (see [kernel/patches/oneios-hosted/README.md](../kernel/patches/oneios-hosted/README.md)).
- The reference for the prelink (optional, worth thousands of paired functions; rebuild only when the source
  changes): `scripts/bg.sh ref env XNU_FILESET_REFERENCE=1 HOSTED_OUT=me kernel/build-hosted.sh` (see
  [kernel/prelink/README.md](../kernel/prelink/README.md) for where it lands).
- The build also makes the hosted board's archives from `kernel/patches/oneios-hosted/`: the disks (`storage`), the
  virtio GPU (`gpu`), software AES (`aes`) and the display (`display`: needs `external/NativeMobileDisplay` and
  `external/UniXNU-droidvm` from `scripts/setup/get-references.sh`; its input for another kernelcache is remade with
  `display/port-legacy-display.py`).
- The VM kernel (UniXNU's own board, not used by xnuhost): `scripts/bg.sh kernel kernel/build-linux.sh`; on a Mac,
  `kernel/build-macos.sh`. See [kernel/README.md](../kernel/README.md).

The kernel links Apple's `corecrypto` under its Internal Use License: it is built locally and never released.

## 3. The kernel collection (`kernel.kc`)

```sh
scripts/bg.sh prelink env KERNEL=out/kernel/current-hosted-me/xnu.macho REFERENCE=<the reference file> kernel/prelink/build.sh
# -> out/kernel/24A446/kernel.kc (about 100 MB), kernel.kc.build.json, kernel.kc.prelink.json   (about 80 s)
```

A new kernel needs a new `kernel.kc`: every native address in it is that kernel's. The records list each kext left out
and why, and the original functions still on the panic stub. Details: [kernel/prelink/README.md](../kernel/prelink/README.md).

## 4. The runtime (xnuhost, helpers, phone tools)

```sh
runtime/build.sh --test           # host unit tests of the loader and xnuhost, then the Android binary on qemu-aarch64
runtime/build.sh STAGE            # -> STAGE/bin/xnuhost (static), the bridges' helpers, the display helper,
                                  #    and the phone checks (gpuctl, bridgectl, brightctl, displayctl, camctl)
runtime/package.sh 0.1.0-dev      # -> out/release/oneios-runtime-0.1.0-dev-arm64.tar.zst + .sha256 + runtime.json
```

Each part has its own host tests too: `make -C runtime/xnuhost test`, `make -C runtime/display test`,
`make -C runtime/bridges/brightness test`, `make -C runtime/bridges/camera test`, `runtime/package/test.sh`,
`scripts/image/tests/run.sh`.

## 5. Booting on the PC (qemu-aarch64)

The Android binary runs on the PC under qemu's user-mode emulator; its system calls go to the PC's Linux kernel.

```sh
mkdir -p out/run/data out/run/session
timeout -s TERM -k 5 3600 qemu-aarch64 -cpu cortex-a710 STAGE/bin/xnuhost --mode bench \
  --image out/image/24A446 --kernel out/kernel/24A446/kernel.kc \
  --data out/run/data --session out/run/session --cpus 8 --ram 4G --display none --no-input \
  --set clusters=EEPPPPPP \
  --boot-args "debug=0x14e serial=3 -v kt_fixed_zones=120 -no_slto_panic ml-timeout-lock-panic-scale=40 tlto_us=0 \
armcpu-self-ipi=1 armcpu-spi-ipi=1 use_contiguous_hint=0 interrupt-masked-debug-mode=0 \
sched_preemption_disable_debug_mode=0 wdt=-1 sheng-smp=1 oneios-data-wait=3000" 2>&1 | tee out/run/boot.log
```

- `-cpu cortex-a710`, never qemu's default `max`: its 512-byte `DC ZVA` breaks XNU's `bzero`.
- `oneios-data-wait=3000`: under qemu the first boot spends minutes re-deriving the shared cache's 4 KiB code
  hashes (emulated SHA); the committed default (300 s) is for the phone.
- `hosted_mapcheck=1` (boot argument, debugging only): the kernel records every page it maps into and unmaps
  from a task's Linux process and reports, as `hosted check:` lines, a page mapped over another with no
  flush between, a page the VM frees while a task still has it mapped, and (every 1024 returns to user
  mode and at a task's `brk`) a mapping the task's page table no longer allows. Costs memory and time.
- `guest` lines in the log are the kernel's console. `kill -QUIT <xnuhost pid>` (the pid in `SESSION/xnuhost.pid`,
  not `pkill`: the task factory has the same command line and dies of the signal) logs every vCPU and every iOS
  thread, with the thread's return addresses up its frame pointers where the host can read the task's memory (on
  the phone; under qemu-user it cannot, and only pc and lr are shown); `qemu-aarch64 -g PORT` plus gdb (`set architecture aarch64`, `target remote :PORT`) debugs the kernel.
- A user process that dies: the kernel logs its registers and a backtrace (`sheng-exc`, `sheng-ubt` lines) for the
  processes named in the boot argument `sheng-exc-name=SpringBoard,backboardd,...` (default: backboardd, chronod,
  liveactivitiesd; launchd always). `scripts/debug/dsc.py ADDR...` names those addresses in the image set's dyld
  shared cache (it finds the slide from a whole backtrace; its header says how to give it the cache).
- Touch and keys on the PC: add `--input-script runtime/xnuhost/tools/input-demo.script` (keep `--no-input`: the
  evdev reader would open the PC's own devices). The script gives xnuhost's input device a panel's axes and, once
  the guest's driver runs it, repeats a tap, a swipe up from the bottom edge and the three side keys
  ([booter.md](booter.md) 2.8). The kernel's `oneios-hid:` lines say what attached and what arrived
  ([the HID archive's README](../kernel/patches/oneios-hosted/hid/README.md)).
- What user space did, after the run has stopped: the Data volume is in `--data`'s `data.img` and launchd keeps
  its own log on it (every job spawned or refused, every exit with its reason).
  `apfs-fuse -o ro,vol=2 out/run/data/data.img MNT`, then `MNT/root/log/com.apple.xpc.launchd/launchd.log`, and
  `python3 -I userland/tweak boot-check --data MNT/root` says whether launchd applied the daemon policy
  ([userland.md](userland.md) section 10); `fusermount3 -u MNT` before the next run.
- The display: the guest's framebuffer driver starts about ten seconds in (`oneios-display:` lines in the log: stage
  10 is a started framebuffer; then who opened it, the first swaps and frames). To see the picture, boot with
  `--display shm` instead of `none` and run, beside it,
  `kernel/patches/oneios-hosted/display/display-capture.py --session out/run/session --out out/run/frame`: it prints a
  line when the picture changed and writes `out/run/frame-NNN.png`. `out/run/session/ready` appears at the first
  frame that is not the boot console's. Boot argument `oneios-display=0` leaves the display out. The frame is
  1440x3120 at 120 Hz and @3x unless told otherwise: `--width W --height H`, `--hz N`, `--set scale=2`
  ([booter.md](booter.md) 2.8); the log's `display: shm, guest frame ...` line and the capture's `hello` say what it is.
- Metal's kernel side: add `oneios-gpu-metal=1 oneios-adreno-selftest=1` to the boot arguments (the accelerator is
  not published on a host without 3D unless asked). About ten seconds in: `droidvm-gpu: Metal accelerator stage 5
  status 0` and two `oneios-adreno: selftest:` lines (IOKit's match, the plug-in's open and one call;
  [gpu.md](gpu.md) 5.1). No 3D call can run here: qemu has no KGSL.
- A user process that neither dies nor does its job: boot arguments `oneios-ubt=NAME[,NAME...]`
  (`oneios-ubt-at=SECONDS`, default 420; `oneios-ubt-every=SECONDS` to repeat) make the kernel print where every
  thread of those processes is (`oneios-ubt:` lines: scheduler state, saved pc and lr, the return addresses up the
  frame pointers; `0026`). Give a long line's addresses to `scripts/debug/dsc.py` for the slide, then name the rest
  with `--slide`.
- IOKit calls of a process: `oneios-iouc=NAME[,NAME...]` logs which user client methods those processes call and
  what they return (`oneios-iouc: backboardd[53] IOMobileFramebufferUserClient selector 5 -> 0x0 (call 1)`: the
  first call of each class and selector, each change of status, and calls 16, 64, 256, ...; `0029`).
- The display backend's counters: `oneios-display-report=SECONDS` prints, every so often, the vsync tick, swaps
  validated, accepted and refused, the backend's kick statistics, the power state the family's swap path tests
  (`display +7180`), the power manager's calls, and the last swaps' timeline (`0030`).
- What the PC cannot show: seccomp (qemu-user has none; a marked trap instruction stands in), real timing, the
  phone's GPU, panel, HALs, SELinux and KernelSU.

## 6. On the phone

**The checks (One UI running, nothing switched):** in the OneOS fork app (0.4.75 or newer), Choose a system →
iOS 27 (OneiOS) → Open → **Hardware checks**. The screen uses the tools of the staged runtime (0.1.0 or newer has
them all); with none staged it offers *Download from OneiOS's releases* first. Then:

1. Turn Bluetooth and adaptive brightness off in One UI and close the Camera app (with either setting on, that
   row is skipped and says why).
2. **Run all**: one dialog says what will happen (colour bars for 5 s, two buzzes, a tone, the screen stepping dim
   to full, the camera used briefly), then twelve checks run in order, about two minutes: CPU layout, GPU, display
   layer, haptics, haptics pattern, audio, Bluetooth, brightness, brightness steps, light sensor (cover the back,
   then point it at a light), cameras, camera. Each row ends as Pass, Fail or Skipped with the tool's line; every
   row also has its own **Run**.
3. Tap a row for the full output (and, for the camera, the picture). **Share all** sends every log as one text.
4. The two rows under "Experimental" (the camera through Samsung's HAL directly) are not part of Run all.

Every check has a time limit (10 to 45 s); a tool that has not finished is ended and the backlight, the light
sensor's enable file and the display test's layer are put back. The checks start no session and stop nothing of
One UI's. How the screen runs them and what it tests: the fork's `docs/oneios.md`, "Hardware checks".

Or by hand, from `STAGE/bin` pushed to `/data/local/tmp`, as root: `gpuctl` (or one step of it:
`gpuctl nodes|routes|probe|submit|large`, [gpu.md](gpu.md) section 4), `displayctl test`,
`bridgectl cpu|haptic|haptic-pattern|bt|audio`, `brightctl info|levels|als`, `camctl --backend ndk list|capture`.
Each tool's README has its exact commands and what a pass looks like. **None of this has run on the phone yet.**

**Getting the image set onto the phone:** it is about 11 GB and never published. Serve it from the PC privately
over several Cloudflare quick tunnels at once, so a download manager fetches it in parallel parts from all of them
(the server, `scripts/serve/rangeserve.py`, binds 127.0.0.1 and answers byte ranges; Python's `http.server` does not):

```sh
scripts/serve/serve-image.sh out/image/OneiOS_iOS27.0.1_24A446_iPhone18-2.tar.zst 4   # 4 tunnels; prints the URLs
# on the phone (Termux, pkg install aria2): aria2c -x 16 -s 16 -k 8M -o <name> <url1> <url2> <url3> <url4>
scripts/serve/serve-image.sh --stop                                                    # when done
```

URLs are also in `logs/serve-urls.txt`; each tunnel's log is `logs/serve-tN.log`. Check the download against the
`.sha256` (same URL plus `.sha256`), then in the app: iOS 27 (OneiOS) → I have the file → the download.

## 7. The OneOS fork app

Repo `akshaynexus/OneOS`, branch `oneios`. A push to `oneios` runs `.github/workflows/oneios-apk.yml`: it builds
the full debug APK (the workflow's own `GITHUB_TOKEN` reads the SESL packages), runs the unit tests and publishes
the prerelease `oneios-<version>` (0.4.75 is the first with the Hardware checks screen). Locally: a `read:packages` token in `~/.config/twidget/github.properties`
(`ghUsername`, `ghAccessToken`), then `cd app-android && ./gradlew test assembleFullDebug`.

## 8. Releases

A tag `vN.N.N` on `main` runs `.github/workflows/release.yml`, which builds the runtime package with the pinned NDK
and attaches it and its `.sha256` to the release. No kernel, no image. See [releases.md](releases.md).
The first release is [`v0.1.0`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.0) (2026-11-10).

## 9. iOS user-space code (executables, dylibs, bundles) on Linux

The method of `macos_helios300` (its `CROSS-BUILD.md`): the host's clang against the SDK that xtool extracts from
Xcode, linked by Apple's ld64, signed ad hoc here. Nothing of it needs a Mac.

```sh
scripts/setup/ios-toolchain.sh          # one line: "ios toolchain: ok: SDK ..., ld64-956.6, clang ..., iOS 27 stubs N"
                                        #        or "ios toolchain: missing: A; B" (each with the command that makes it)
scripts/bg.sh ios-ld scripts/setup/ios-toolchain.sh --ld      # once: ld64 with .tbd support -> out/ios-toolchain (about 6 min)
scripts/bg.sh ios-tbds scripts/setup/ios-toolchain.sh --tbd   # once per image set: the iOS 27 stubs -> out/ios27-tbd (about 6 min)
scripts/setup/ios-toolchain.sh --test   # builds, signs and checks out/ios-test/hello
```

| Part | What | Where it comes from |
|---|---|---|
| SDK | headers (and stubs, as a fallback) | `xtool sdk install <Xcode.xip>` puts it under `~/.config/swiftpm/swift-sdks/darwin.artifactbundle/.../SDKs/`. The newest `iPhoneOS*.sdk` there is used; `IOS_SDK=DIR` names another. Every build prints the SDK it used and writes it into its build record |
| ld64 | Apple's ld64-956.6 with libtapi | `--ld` builds cctools-port at the kernel's pin (`kernel/pins.sh`) with tpoechtrager/apple-libtapi `2a57054` into `out/ios-toolchain`. The kernel's ld64 (`out/toolchain`) is built without libtapi and cannot read a `.tbd` |
| iOS 27 stubs | what 24A446 really exports | `--tbd` runs `scripts/ios/make-tbds.py`: one `.tbd` per image of the guest's own shared cache (4696; read from `out/ios27-cache/` when that copy exists, otherwise from `out/image/24A446/systemos.img` mounted with apfs-fuse), written by `ipsw dyld tbd`, laid out like an SDK in `out/ios27-tbd/` (1.5 GB). Derived from Apple's binaries: never committed |
| clang | the host's, LLVM 19 or newer (arm64e) | |

**The SDK today is iOS 26.5; the target is iOS 27.** xtool has only `iPhoneOS26.5.sdk` until the owner downloads
Xcode 27.1 from Apple (it needs their Apple ID) and runs:

```sh
xtool sdk install ~/Downloads/Xcode_27.1.xip
```

Until then code compiles against the 26.5 **headers** and links against the **iOS 27 stubs**: `ios-ld` gives
`out/ios27-tbd` to ld64 as the first `-syslibroot`, so a symbol iOS 27.0.1 does not export fails the link, and
`macho-check.py` checks the result against the same stubs. The deployment target is 27.0 either way
(`LC_BUILD_VERSION`: minos 27.0, sdk 26.5). What changes once the iOS 27 SDK is installed: it is picked up with no
edit (the newest `iPhoneOS*.sdk`), the printed SDK line and the build records say 27.x, iOS 27's own headers
(new API declarations, availability) are used, and the binaries' `sdk` field becomes 27.x. The stubs stay first:
they are this guest's build, the SDK's are the SDK's. Rebuild what was built (`userland/metal/build.sh`).

The helpers in `scripts/ios/` (settings and their overrides are in `env.sh`):

```sh
scripts/ios/ios-clang -O2 x.c -framework CoreFoundation -o x                    # executable (ios-clang++ for C++/ObjC++)
scripts/ios/ios-clang -dynamiclib -install_name /usr/lib/libx.dylib x.c -o libx.dylib
scripts/ios/ios-clang++ -bundle -fobjc-arc x.mm -framework Metal -o X           # a bundle's executable
scripts/ios/ios-sign.py --identifier com.oneios.x [--entitlements e.plist] x    # ad-hoc signature; prints the CDHash
scripts/ios/macho-check.py x                                                    # every import against the iOS 27 stubs
```

- `ios-clang`: `clang -target arm64e-apple-ios27.0 -isysroot $IOS_SDK`, with `ios-ld` as the linker.
- `ios-ld`: ld64 with the iOS 27 stubs before the SDK and `-fixup_chains` (this ld64 writes the old `LC_DYLD_INFO`
  opcodes unless told; Xcode's linker writes chained fixups for iOS).
- `ios-sign.py`: what `codesign --force --sign - --pagesize 4096` makes: one SHA-256 code directory with 4 KiB pages
  (this guest's kernel runs 4 KiB pages), the entitlements as XML and DER when given. It prints
  `<cdhash> <arch> pages=4096 <identifier> <file>`. No bundle seal (`_CodeSignature/CodeResources`): the kernel
  checks a mapped library by its CDHash alone.
- `macho-check.py`: arm64e, platform iOS, signed with 4 KiB pages, every library has a stub, every undefined symbol
  is exported by the library it is bound to (re-exports followed). `--local FILE` names a dylib that travels with
  the file; `--sdk` checks against the SDK's stubs instead.

**Trust.** The guest runs a binary whose CDHash is in a trust cache. Ours get there through the image hook: it reads
the CDHash from each signed Mach-O it places and writes `overlay/userland/owner-trust.json` ([userland.md](userland.md)).
The Metal plug-in's build also writes its CDHashes into `out/userland/NativeAdrenoMetal.build.json`, and the hook
stops the image build when the placed bundle no longer matches that record.

Checked on 2026-11-11 (SDK 26.5, stubs of 24A446): `--test` builds `out/ios-test/hello` (arm64e, minos 27.0,
chained fixups, 50432 bytes), `userland/tweak cdhash` reads back the CDHash `ios-sign.py` printed, `rcodesign
print-signature-info` parses the signature (ad hoc, SHA-256, `MAIN_BINARY`), and `macho-check.py` passes: its
libraries and 3 imports resolve in the iOS 27 stubs. **Not checked: that the guest runs it.** No boot has exec'ed a
binary built here. The DER entitlements are byte-identical to `rcodesign`'s for the same plist (booleans, strings, an
array, an integer); neither was compared with Apple's codesign.

### The Metal plug-in

```sh
scripts/setup/get-references.sh         # NativeMetal, NativeAdreno, AdrenoPM4, metal2vulkan, mesa-droidvm, Droid-VM-UniXNU
rustup toolchain install 1.98.0 && rustup +1.98.0 component add rust-src
scripts/bg.sh metal userland/metal/build.sh
# -> out/userland/NativeAdrenoMetal.bundle, NativeAdrenoMetal.build.json, NativeAdrenoMetal.check.txt
```

Then rebuild the image set (section 1): the hook places the bundle and lists its two CDHashes. What the build does,
what was changed for the iPhone's shared cache and what is verified: [gpu.md](gpu.md) section 7.

