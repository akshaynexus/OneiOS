# Building and testing OneiOS

Everything builds on a Linux x86_64 PC. No Mac is needed for the boot path; a Mac (Xcode 27.1) is only needed for
optional pieces named below. Long steps run in tmux with `scripts/bg.sh NAME CMD...` (log in `logs/NAME.log`, exit
code in `logs/NAME.exit`, state with `scripts/bg.sh -s NAME`). tmux does not pass environment variables: put them
inside the command (`scripts/bg.sh x env VAR=1 cmd`).

Nothing of Apple's is ever committed or released: the IPSW parts, the image set and `kernel.kc` stay in `inputs/`
and `out/`.

## 0. Prerequisites

| Tool | For | Notes |
|---|---|---|
| python3 3.11+, zstd, tmux, git, curl | everything | |
| clang (stock, recent) | the kernel | the kernel build uses stock clang plus xnu-linux-toolchain's ld64 (set up by the kernel scripts, no root) |
| Android NDK 28.2 | xnuhost, the helpers, the phone tools | `$HOME/Android/Sdk/ndk/28.2.13676358`, or `$NDK` / `$ANDROID_NDK_HOME` |
| Android SDK build-tools (`d8`) and a JDK (`javac`) | the display helper's dex | |
| apfs-fuse | reading the OS volume for the Data seed | github.com/sgan81/apfs-fuse; read-only, no root |
| qemu-user (`qemu-aarch64`) | booting xnuhost and the kernel on the PC | |
| `ipsw` CLI | AEA decryption | `scripts/fetch/get-ipsw-cli.sh` installs the pinned version to `out/bin/ipsw` |
| mksh, dash (optional) | testing the phone scripts the way Android runs them | |
| gh | releases | |

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

- `HOSTED_OUT=<name>` gives you your own tree and output; without it the build uses the default tree
  (`out/kernel-hosted`, `out/kernel/current-hosted/`), which belongs to the lead.
- Incremental work on one patch: `kernel/hosted-dev.sh` (see [kernel/patches/oneios-hosted/README.md](../kernel/patches/oneios-hosted/README.md)).
- The reference for the prelink (optional, worth thousands of paired functions; rebuild only when the source
  changes): `scripts/bg.sh ref env XNU_FILESET_REFERENCE=1 HOSTED_OUT=me kernel/build-hosted.sh` (see
  [kernel/prelink/README.md](../kernel/prelink/README.md) for where it lands).
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
- `guest` lines in the log are the kernel's console. `kill -QUIT <xnuhost pid>` logs every vCPU and every iOS
  thread; `qemu-aarch64 -g PORT` plus gdb (`set architecture aarch64`, `target remote :PORT`) debugs the kernel.
- What the PC cannot show: seccomp (qemu-user has none; a marked trap instruction stands in), real timing, the
  phone's GPU, panel, HALs, SELinux and KernelSU.

## 6. On the phone

**The checks (One UI running, nothing switched):** in the OneOS fork app, iOS 27 (OneiOS) card → Hardware checks →
Run all, then Share all. Or by hand, from `STAGE/bin` pushed to `/data/local/tmp`, as root: `gpuctl`,
`displayctl test`, `bridgectl cpu|haptic|haptic-pattern|bt|audio`, `brightctl info|levels|als`,
`camctl --backend ndk list|capture`. Each tool's README has its exact commands and what a pass looks like.

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
the prerelease `oneios-<version>`. Locally: a `read:packages` token in `~/.config/twidget/github.properties`
(`ghUsername`, `ghAccessToken`), then `cd app-android && ./gradlew test assembleFullDebug`.

## 8. Releases

A tag `vN.N.N` on `main` runs `.github/workflows/release.yml`, which builds the runtime package with the pinned NDK
and attaches it and its `.sha256` to the release. No kernel, no image. See [releases.md](releases.md).
