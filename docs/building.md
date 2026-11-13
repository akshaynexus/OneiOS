# Building and testing OneiOS

Everything builds on a Linux x86_64 PC, iOS user-space code included (section 9); no Mac is needed. The runtime
(section 4: xnuhost, the helpers, the phone tools, the package) also builds on a Mac, and its host tests run from
the Mac in an arm64 Linux container. Long steps run in tmux with `scripts/bg.sh NAME CMD...` (log in `logs/NAME.log`, exit
code in `logs/NAME.exit`, state with `scripts/bg.sh -s NAME`). tmux does not pass environment variables: put them
inside the command (`scripts/bg.sh x env VAR=1 cmd`).

Nothing of Apple's is ever committed or released: the IPSW parts, the image set and `kernel.kc` stay in `inputs/`
and `out/`.

## 0. Prerequisites

| Tool | For | Notes |
|---|---|---|
| python3 3.11+, zstd, tmux, git, curl, patch | everything | `patch`: xnuhost's Makefile applies `runtime/xnuhost/drm2kgsl/patches` to a copy of drm2kgsl |
| Rust 1.88+ (`cargo`; rustup) | `tools/oneios-tool`: the kernel's board archives and build record (section 2), the kernel collection's build (section 3), the image tools and the phone workflow (section 10) | the build scripts run `cargo build --release --manifest-path tools/oneios-tool/Cargo.toml` when the binary is missing or older than its sources (`scripts/lib/oneios-tool.sh`); `ONEIOS_TOOL=PATH` names a built one. Built with 1.98.0; needs a C compiler (`sha2`'s `asm`) |
| clang (stock, recent) | the kernel | the kernel build uses stock clang plus xnu-linux-toolchain's ld64 (set up by the kernel scripts, no root) |
| libdispatch with BlocksRuntime (Linux only) | the kernel's toolchain (cctools' ld64) | Arch: `libdispatch` (6.2.2 on the PC of 2026-11-12: `/usr/include/dispatch`, `/usr/lib/libdispatch.so`, `libBlocksRuntime.so`); Debian/Ubuntu: `libdispatch-dev libblocksruntime-dev`. Without it `kernel/linux/setup-toolchain.sh` stops and no kernel builds. Also git, cmake, make, flex, bison, autoconf, automake, pkg-config ([kernel/README.md](../kernel/README.md)) |
| Android NDK 28.2 | xnuhost, the helpers, the phone tools | `$HOME/Android/Sdk/ndk/28.2.13676358` (Linux), `$HOME/Library/Android/sdk/ndk/28.2.13676358` (macOS), or `$NDK` / `$ANDROID_NDK_HOME` |
| Android SDK build-tools (`d8`) and a JDK (`javac`) | the display helper's dex | the newest `build-tools/*/d8` beside the NDK's SDK; on a Mac the Homebrew JDK 17 (`brew install openjdk@17`), elsewhere `javac` from `PATH` |
| Apple's `container` 1.7.1 (Mac only) | the runtime's host tests from a Mac | `brew install container`, then `container system start`; `runtime/host-tests.sh`, section 4 |
| apfs-fuse (Linux only) | reading the volumes for the Data seed and the hook | github.com/sgan81/apfs-fuse; read-only, no root. On macOS `hdiutil` does it, no root (section 1) |
| qemu-user (`qemu-aarch64`) | booting xnuhost and the kernel on the PC | |
| `ipsw` CLI | AEA decryption | `scripts/fetch/get-ipsw-cli.sh` installs the pinned version to `out/bin/ipsw` |
| mksh, dash (optional) | testing the phone scripts the way Android runs them | |
| gh | releases | |
| macOS: Xcode 27 with the iOS platform. Linux: xtool and an Xcode `.xip`. Both: rustup 1.98.0 with `rust-src`, meson, ninja, glslang, glslc, spirv-val, bison, flex, LLVM's `llvm-as`/`llvm-objcopy`/`llvm-ar`, PyYAML, mako | iOS user-space code: the toolchain and the Metal plug-in | section 9; `scripts/setup/ios-toolchain.sh` says what is missing |

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
                                                    # (without inputs/24A446/ it fetches only those, 24 MB)
```

`kernel-inputs.sh` decodes the two IM4P members (the kernelcache's payload is LZFSE) with `oneios-tool image im4p` (since
2026-11-13 always; before, `imagetool.py im4p` when the
`lzfse` program is on `PATH`, and otherwise with `oneios-tool image im4p` (LZFSE by the `lzfse_rust` crate, no other
program; build it with `cargo build --release --manifest-path tools/oneios-tool/Cargo.toml`, section 10).
`IM4P_TOOL=PATH` names a `oneios-tool` binary and makes it the decoder in either case. Whichever ran, the script then
checks `kernelcache.macho` against the SHA-256 the kernel profile pins (`kernel/unixnu/profiles/os/ios-27.0.1-24A446/
profile.toml`, `kernelcache_sha256`) and stops when it differs. On the Linux PC on 2026-11-12, without `lzfse`:
78135296 bytes, sha256 `349c4692...0c57e3`, the pinned one (which was made with the `lzfse` program on the Mac), in
0.7 s. Not compared: the two decoders run side by side on one host (no `lzfse` program on the PC).

The set is built after `kernel.kc` exists (step 3), so that it carries the kernel:

```sh
scripts/bg.sh image scripts/image/build.sh "$P" --kernel out/kernel/24A446/kernel.kc
# -> out/image/24A446/ and out/image/OneiOS_iOS27.0.1_24A446_iPhone18-2.tar.zst (+ .sha256), about 11 GB
scripts/image/verify.sh out/image/OneiOS_iOS27.0.1_24A446_iPhone18-2.tar.zst
```

`--no-kernel` builds a set without one (for work on the images only). The userland hook (`userland/image-hook`)
runs by default. Format: [image-format.md](image-format.md).

**On macOS** the commands are the same and need no `sudo`. The fetch decrypts with the macOS `ipsw` release. The
image build attaches the three images with `hdiutil` (read-only, owners ignored) and reads the owners of
`/private/var` from the OS image's own APFS records (image-format.md section 8). Tools: Homebrew's `zstd`, and
`uv pip install --python "$(command -v python3)" pyyaml mako meson` for section 9. Run on the Mac on 2026-11-12:
the fetch took 62 minutes (10.6 GiB at about 3 MB/s, then both decryptions), the image build 2 minutes with the
archive (11.15 GB). The three images and the trust files are byte for byte the Linux PC's (the phone's manifest).

## 2. The kernel

Linux or macOS, the same command (the script picks the toolchain by `uname -s`):

```sh
scripts/bg.sh kernel-hosted env HOSTED_OUT=me kernel/build-hosted.sh
# -> out/kernel/current-hosted-me/xnu.macho (the hosted kernel: board series + kernel/patches/oneios-hosted/series)
```

- **Linux** (x86_64 or arm64): stock clang plus xnu-linux-toolchain's ld64, mig and iig, set up by the script into
  `out/toolchain` (no root; bubblewrap for the `/usr/bin` shims). Packages: [kernel/README.md](../kernel/README.md).
- **macOS 27**: Xcode's clang and iphoneos SDK at `/Applications/Xcode.app` (`XCODE_APP=` for another place) and
  Homebrew's `cmake ninja unifdef llvm lld` (installed when missing). The pin is Xcode 27.2 (the Mac build host since 2026-11-12); `ALLOW_XCODE=1` builds
  with another. No container, no Linux toolchain.
- One machine, one builder (the Mac of 2026-11-12): the default tree is then yours too. The command used there:
  `scripts/bg.sh kernel-hosted env HOSTED_OUT=lead ALLOW_XCODE=1 XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh`
  (`HOSTED_OUT=lead` is the default tree, `out/kernel/current-hosted/`). On an M4 (10 cores, Xcode 27.2): 270 s from an
  empty tree with UniXNU's source cache (`out/kernel-hosted/unixnu/feeds`) present, the RELEASE reference included;
  the first run also fetches that cache (about 170 MB from GitHub: 14 min on the link of 2026-11-12). An incremental
  rebuild: 30-100 s.
- On the Linux PC (x86_64, 4 CPUs, clang 23.1.4, 2026-11-12):
  `scripts/bg.sh k env HOSTED_OUT=pc XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh` took 683 s from an empty tree,
  the toolchain's first setup (`out/toolchain`), UniXNU's source cache and the RELEASE reference included. The log shows
  one `ld: symbol(s) not found` early on: that is the "bootstrap pass for the compile records" (XNU linked before the
  board's archives exist), and the build goes on. The prelink of a Linux-built kernel needs the pair list's
  toolchain entries ([kernel/prelink/README.md](../kernel/prelink/README.md), "A Linux-built kernel against the list").

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
  changes): `XNU_FILESET_REFERENCE=1` in the build's command. It lands next to the kernel as
  `out/kernel/current-hosted-me/reference.macho` (see [kernel/prelink/README.md](../kernel/prelink/README.md)).
- The build also makes the hosted board's archives from `kernel/patches/oneios-hosted/`, each by `oneios-tool kernel gen NAME`
  (section 10; the script builds the tool first when it is missing or stale, so the kernel build needs Rust): the disks (`storage`), the
  virtio GPU (`gpu`), software AES (`aes`), the speaker and microphone (`audio`: UniXNU-droidvm's virtio-snd and
  IOAudio2Devices from `external/UniXNU-droidvm`) and the display (`display`: needs `external/NativeMobileDisplay` and
  `external/UniXNU-droidvm` from `scripts/setup/get-references.sh`; its input for another kernelcache is remade with
  `oneios-tool kernel port-legacy-display`).
- The VM kernel (UniXNU's own board, not used by xnuhost): `scripts/bg.sh kernel kernel/build-linux.sh`; on a Mac,
  `kernel/build-macos.sh`. See [kernel/README.md](../kernel/README.md).

The kernel links Apple's `corecrypto` under its Internal Use License: it is built locally and never released.

## 3. The kernel collection (`kernel.kc`)

```sh
scripts/bg.sh prelink env KERNEL=out/kernel/current-hosted-me/xnu.macho REFERENCE=out/kernel/current-hosted-me/reference.macho kernel/prelink/build.sh
# -> out/kernel/24A446/kernel.kc (about 94 MB), kernel.kc.hashes, kernel.kc.build.json, kernel.kc.prelink.json,
#    kernel.kc.binds.json
```

Linux and macOS. The steps are `oneios-tool prelink` (section 10); the script builds the binary when it is missing or
older than its sources, so Rust is a prerequisite (section 0). No `python3` or `llvm-nm` is needed; LLVM's `llvm-mc`
only with `X18=1` (on a Mac Homebrew's keg-only `llvm`, which the script puts on `PATH`). The input is `out/kernel-inputs/24A446/kernelcache.macho` (section 1:
`scripts/image/kernel-inputs.sh`). It also writes `kernel.kc.hashes` (the 1 MiB chunk list of
[image-format.md](image-format.md)). On the M4: 46 s cold, 19 s with the pairing reused; 94,109,696 bytes, 92
original functions on the stub and 9 data unpaired (with the reference).

**The build fails** when the audit step (`oneios-tool prelink audit`) finds a kext import that is not bound as
`kernel/prelink/pairs-24A446.json` says, or that is on the stub without a reason in `kernel/prelink/stubs-24A446.json`
(it prints each one). A kernel that renames or drops a listed function, or a new root kext, needs its pairs reviewed
into those lists first (kernel/prelink/README.md, "Known-good pairs").

A new kernel needs a new `kernel.kc`: every native address in it is that kernel's. The records list each kext left out
and why, and the original functions still on the panic stub. Details: [kernel/prelink/README.md](../kernel/prelink/README.md).

## 4. The runtime (xnuhost, helpers, phone tools)

Build hosts: a Linux x86_64 PC, or a Mac (Apple Silicon; macOS 27 tested). `runtime/host.mk`, which every runtime
Makefile includes, picks the NDK's prebuilt tools by `uname -s` (`linux-x86_64` or `darwin-x86_64`; the darwin tools
are universal binaries) and the NDK's default place; `NDK=` overrides it everywhere. The scripts use no GNU-only
tool: sizes with `wc -c`, CPUs with `getconf _NPROCESSORS_ONLN`, checksums with `sha256sum` or `shasum -a 256`.

```sh
runtime/build.sh STAGE            # -> STAGE/bin/xnuhost (static), the bridges' helpers, the display helper,
                                  #    and the phone checks (gpuctl, bridgectl, wifictl, brightctl, batteryctl, thermalctl, displayctl, camctl)
runtime/package.sh 0.1.0-dev      # -> out/release/oneios-runtime-0.1.0-dev-arm64.tar.zst + .sha256 + runtime.json
runtime/host-tests.sh             # every host test of the runtime, one summary line per part (below)
runtime/build.sh --test           # Linux: the loader, xnuhost and display tests (xnuhost's with the Android binary on
                                  #   qemu-aarch64); macOS: the same as runtime/host-tests.sh
```

`external/virglrenderer` (`scripts/setup/get-references.sh virglrenderer`) must be there before a build that goes to
the phone: without it xnuhost and gpuctl build with the 2D GPU only and say so at run time ([releases.md](releases.md)).

`runtime/host-tests.sh` runs, and prints one line each for: `loader` (`make -C runtime/loader test`), `xnuhost`
(`make -C runtime/xnuhost test`: the unit tests, drm2kgsl and its leak test with LeakSanitizer, the image set, and
the Android xnuhost booting the test kernel), `display` (`make -C runtime/display test`), `bridges`
(`runtime/bridges/tests/host.sh`: the unit tests, `test-binder`, brightness, battery), `camera`
(`runtime/bridges/camera/tests/host.sh`) and `package` (`runtime/package/test.sh`, when the NDK is there).
A part that ends with exit 0 but skipped something shows the first "skipped" line. The exit code is 1 when a part
failed. Other host tests: `scripts/image/tests/run.sh`.

Checked on 2026-11-12, Linux x86_64 CachyOS (kernel 7.3.1-1-cachyos with binderfs built in, 4 CPUs, 15 GiB; GCC
16.2.1, clang 23.1.4, GNU make 4.4.1, Python 3.14, NDK 28.2.13676358, build-tools 35.0.0, Temurin JDK 17,
qemu-aarch64 11.1.3 user mode, no binfmt), `runtime/host-tests.sh` on a clean tree, then again after the changes
below (the same counts):

| Part | Result |
|---|---|
| `runtime/build.sh out/stage-linux` (clean tree) | 15 files: 14 in `bin/` (13 arm64 ELF and `oneios-net.sh`) and `lib/oneios-display.dex`; xnuhost 4,198,048 bytes, with drm2kgsl (3.4 MB of it is `task.c`'s state `T` in `.data`: it has initialisers, so it is not `.bss`) |
| loader | 160 checks, 0 failed |
| xnuhost | 13263 checks, 0 failed; display mode 5 scenarios, 0 failed; image set 37 checks, 0 failed; `check-arm64` under qemu-aarch64 `TESTKERNEL: all passed` and the restart test (8 restarts, then exit 3). **Not run here:** drm2kgsl, the leak test and test_xnuhost's udmabuf import ("no /dev/udmabuf here (Permission denied)"): the box's `/dev/udmabuf` is `root:kvm 0660` with an ACL for the seat's user only (`sddm`); LeakSanitizer itself works here. Giving the user access (the `kvm` group, or the seat) is a root step |
| display | 72 checks, 0 failed; mode choice 5 checks, 0 failed |
| bridges | 204 checks, 0 failed; wifi 132 checks, 0 failed; `test-binder` 38 checks (the Bluetooth HAL client) and 35 (the Wi-Fi supplicant client), 0 failed; brightness 74 checks, 0 failures; battery 57 checks, 0 failures; thermal 56 checks, 0 failed; audio helper 4051 checks, 0 failed; Android build ok |
| camera | 131 checks, 0 failed; Android build ok |
| package | 19 tests passed |
| `scripts/image/tests/run.sh` | 23 tests, OK |

`make -C runtime/xnuhost bench-arm64` also runs here, under qemu-aarch64 (about 2 s): its times are qemu's (null
system call 19.8 us mean, `task_unmap` 16.6 us; about ten times the Mac's container) and the task layer has no seccomp
filter under qemu-user (`svc #0x80` is rewritten by `text_fixup`), so they say only how a change moves a cost on the
same box. The test kernel's histogram now has steps of `hz / 24 MHz` ticks on a counter faster than 24 MHz (qemu's is
1 GHz): before, every p50/p90/p99 under qemu read 4095 ns, the table's last tick.

### The host tests from a Mac

The host tests are Linux tests (binderfs, udmabuf, user namespaces, LeakSanitizer, glibc): `make test` refuses to
start on macOS and names `runtime/host-tests.sh`. On a Mac that script:

1. builds the Android parts with the Mac's NDK (`make -C runtime/loader android`, `runtime/xnuhost all
   build/android/testkernel.elf`, `runtime/bridges all`, `runtime/bridges/camera all`, `runtime/display all`), so
   the container needs no NDK;
2. runs `runtime/host-tests.sh --suite` (with `STRICT=1 PREBUILT=1`) in an arm64 Ubuntu 24.04 container of Apple's `container` tool (each container
   is its own small VM on Virtualization.framework). The image `oneios-hosttests:2` is built with `container build`
   from the Containerfile inside the script the first time (gcc 13, clang 18, OpenJDK 17, zstd, patch, and the kernel
   build's bc, bison, flex, libelf-dev, libssl-dev). The checkout is mounted at its own path (`-v ROOT:ROOT`); nothing
   else of the Mac is. On arm64 the Android xnuhost runs as it is (static, bionic), so `check-arm64` boots the test
   kernel without qemu. `PREBUILT=1` runs the xnuhost part as `make test -o build/android/xnuhost -o
   build/android/testkernel.elf`: make keeps the Mac's binaries and does not walk their prerequisites (the bridges'
   and camera's sub-makes, the gpu3d stamp), so nothing there can call the NDK the container does not have. Before,
   a source that looked newer than its object over the mount made make call the NDK's clang in the container
   (`aarch64-linux-android30-clang: No such file`, error 127 on `build/android/gpu3d.o`; reproduced on Linux with
   `NDK=/nonexistent` and a touched source). A missing pre-built binary is a FAIL that names it. The fix is checked on
   Linux (`NDK=/nonexistent PREBUILT=1 runtime/host-tests.sh --suite` after touching `src/gpu3d.c` and the bridges'
   and camera's sources: every part passes, nothing asks for the NDK); not yet run on a Mac;
3. runs `runtime/package/test.sh` on the Mac.

**The container's kernel.** The default kernel of `container` (6.18.56) has no binderfs, no udmabuf and no modules
(`# CONFIG_ANDROID_BINDER_IPC is not set`, `# CONFIG_UDMABUF is not set`, `# CONFIG_MODULES is not set`), so
`test-binder`, drm2kgsl, the leak test and test_xnuhost's udmabuf checks could not run with it. The first run of
`runtime/host-tests.sh` (or `runtime/host-tests.sh --kernel`, to make it again) builds one, in a container, about 15
minutes on an M4 with 10 CPUs:

- the source: Linux 6.18.56 from cdn.kernel.org, SHA-256 checked (`4633bbda...efbf89`), kept in `out/host-tests/`;
- the configuration: the default kernel's own (`/proc/config.gz`, saved as `out/host-tests/config.default`) plus
  `ANDROID_BINDER_IPC=y`, `ANDROID_BINDERFS=y`, `ANDROID_BINDER_DEVICES=""` and `UDMABUF=y`
  (`out/host-tests/config.oneios`);
- the result: `out/host-tests/vmlinux-6.18.56-oneios` (arm64 `Image`, 25 MB). Only the suite's container boots it
  (`container run --kernel`); `container system kernel set` is not used, and the default kernel of other containers
  does not change. `KERNEL=FILE` names another kernel.

On this path a skipped `test-binder` or a missing `/dev/udmabuf` is a FAIL, not a skip.

**Capabilities.** `--cap-drop ALL --cap-add SETFCAP`, nothing else, no privileged mode (`container` has none).
`test-binder`'s `unshare -Urm` maps uid 0 in the new user namespace, and Linux allows that only with `CAP_SETFCAP`
(with all capabilities dropped: `unshare: write failed /proc/self/uid_map: Operation not permitted`). The binderfs mount
in that namespace and LeakSanitizer's ptrace of its own threads need nothing more; the container has no seccomp filter.

Once per Mac:

```sh
brew install container
container system start --enable-kernel-install      # the services and the default kernel
runtime/host-tests.sh                                # first run: the image (a few minutes) and the kernel (about 15)
```

`IMAGE=`, `KERNEL=`, `JOBS=` (make jobs and the container's CPUs; default: the Mac's) and `MEMORY=` (default 8G)
override the image, the kernel, the CPUs and the memory. The project uses neither docker nor colima any more.

Checked on 2026-11-12, Mac (M4, 10 CPUs, macOS 27, Xcode 27.2, NDK 28.2.13676358, Homebrew JDK 17, `container` 1.7.1
with the kernel above, 10 CPUs and 8 GiB, 4 KiB pages). The counts are the same as the first run on colima's VM
(Ubuntu kernel 6.8.0-117 with binder_linux):

| Part | Result |
|---|---|
| `runtime/build.sh STAGE` (clean tree) | 13 files: the 12 `bin/` files, all arm64 ELF, and `lib/oneios-display.dex`; xnuhost 1,371,200 bytes, with drm2kgsl |
| loader | 160 checks, 0 failed |
| xnuhost | 13248 checks, 0 failed (udmabuf checked, blobs past its limits split); display mode 5 scenarios, 0 failed; drm2kgsl 3 + 23 checks, 0 failed; leak scenarios 956 checks, 0 failed, LeakSanitizer no leaks; image set 37 checks, 0 failed; `check-arm64` `TESTKERNEL: all passed` and the restart test (8 restarts, then exit 3) |
| display | 72 checks, 0 failed; mode choice 5 checks, 0 failed |
| bridges | 204 checks, 0 failed; wifi 132 checks, 0 failed; `test-binder` 38 checks (the Bluetooth HAL client) and 35 (the Wi-Fi supplicant client), 0 failed; brightness 74 checks, 0 failures; battery 57 checks, 0 failures; thermal 56 checks, 0 failed (2026-11-12, after the Wi-Fi bridge) |
| camera | 131 checks, 0 failed |
| package (on the Mac) | 19 tests passed (the Apple-file refusals among them) |

What the port changed, and why (none of it changes a phone binary: every staged file had the same SHA-256 before and
after):

- On an arm64 Linux host, `__aarch64__` code that the x86_64 PC never compiled is compiled for the tests.
  `tests/test_xnuhost.c` is built with `-DXH_HOST_TEST`, so `cpu.c` and `task.c` take the stand-ins they had on
  x86_64 (the real vCPU and task code needs the Android binary's assembly). `imageset.c`'s SHA-256 instructions are
  tested on the host: GCC needs `target("+sha2")`, clang (the NDK's) takes `target("sha2")`.
- Ubuntu's GCC turns on `_FORTIFY_SOURCE`: `tests/test_xnuhost.c` checks the result of `freopen`.
- Ubuntu's kernel and the container's kernel above build binderfs with no default devices: `test_hidl_binder` asks
  `binder-control` for `hwbinder`.
- `test_hidl_binder` started each scenario's fake HAL right after killing the last one. Linux frees a dead process's
  context manager in deferred work, so on 6.18 in the container the next HAL's `BINDER_SET_CONTEXT_MGR` failed with
  `EBUSY` ("fake hal: Device or resource busy", then `FAIL tests/test_hidl_binder.c:497`). A race in the test, not in
  the HAL client: the fake HAL now retries `EBUSY` for up to 2 s. Five runs in a row passed.
- `check-arm64` runs the Android binary without qemu on an arm64 Linux host.
- `make -C runtime/xnuhost bench-arm64` (in the container, after the Android build, with
  `-o build/android/xnuhost -o build/android/testkernel.elf` as above; on a Linux PC under qemu-aarch64, above) runs the test kernel's benchmarks
  of the trap path (boot argument `testkernel-bench`) and prints them with xnuhost's `perf:` report;
  `BENCH_SET="--set cpu-pin=0 --set map-defer=0"` measures other settings, `BENCH_SCALE=2` doubles the loops
  (`docs/booter.md` 2.6a has the numbers). Not part of `make test`.
- macOS's `tar` is bsdtar (no `--mtime`): `runtime/package.sh` writes the archive with `pack.py archive` where `tar`
  is not GNU tar. For the same files it is byte-identical to GNU tar 1.35's `--format=ustar` archive (checked).
- macOS's `wc -l` pads its count: `runtime/package/test.sh` strips it.

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
- `--ram 4G`: without it xnuhost takes `ram=auto` (90% of the PC's MemTotal or all but 1.5 GiB: 13.75 GiB
  of this 15 GiB box, which leaves too little for qemu and the build). It boots (2026-11-12: 14,080 MiB, to SpringBoard
  in 420 s). Before hosted 0058 the memfd filled with pages XNU had freed (12 GiB held after 16.5 min with 9 GiB free
  in XNU; the PC's MemAvailable fell under 1 GiB); with 0058 it holds about what XNU uses (5 GiB at 16.5 min).
  `--ram 9728M` is the phone's size ([booter.md](booter.md) 1).
- What XNU sees of its memory: `oneios-mem:` lines every 30 s (free, given back to the host, in use, memorystatus
  available); `oneios-free-report=0` turns 0058's free-page reporting off for a comparison
  ([kernel/patches/oneios-hosted/README.md](../kernel/patches/oneios-hosted/README.md), 0058).
- `oneios-data-wait=3000`: under qemu the first boot spends minutes re-deriving the shared cache's 4 KiB code
  hashes (emulated SHA); the committed default (300 s) is for the phone.
- `hosted_mapcheck=1` (boot argument, debugging only): the kernel records every page it maps into and unmaps
  from a task's Linux process and reports, as `hosted check:` lines, a page mapped over another with no
  flush between, a page the VM frees while a task still has it mapped, and (every 1024 returns to user
  mode and at a task's `brk`) a mapping the task's page table no longer allows. Costs memory and time.
- `guest` lines in the log are the kernel's console. `kill -QUIT <xnuhost pid>` (the pid in `SESSION/xnuhost.pid`,
  not `pkill`: the task factory has the same command line and dies of the signal) logs every vCPU and every iOS
  thread, with the thread's return addresses up its frame pointers where the host can read the task's memory (on
  the phone; under qemu-user it cannot, and only pc and lr are shown). Seen on the Linux PC on 2026-11-13 (qemu-aarch64
  11.1.3, a boot 30 minutes in, the pid from `xnuhost.pid`): `qemu: uncaught target signal 3 (Quit) - core dumped` and the
  boot ended, so there it costs the boot (cause not looked into); `oneios-ubt=` is the safe way. `qemu-aarch64 -g PORT` plus gdb (`set architecture aarch64`, `target remote :PORT`) debugs the kernel.
- A user process that dies: the kernel logs its registers and a backtrace (`sheng-exc`, `sheng-ubt` lines) for the
  processes named in the boot argument `sheng-exc-name=SpringBoard,backboardd,...` (default: backboardd, chronod,
  liveactivitiesd; launchd always). `oneios-tool sym ADDR...` names those addresses in the image set's dyld
  shared cache (it finds the slide from a whole backtrace; `ONEIOS_DSC_DIR` or `--dsc` names the cache folder, and
  after the first call the index in `out/dsc-index` is enough: section 10, `tools/oneios-tool/README.md` "sym").
- Wi-Fi on the PC: add `--set wifi=sim`: a made-up radio with two networks for the guest's Wi-Fi driver and wifid
  (`ONEIOS WIFI:` lines; [hardware.md](hardware.md) section 3). Not possible in the Mac's container: its Linux has no
  pointer authentication (`xnuhost E an arm64e kernel needs pointer authentication ...; this host has none`). The limit
  is the container's only: on the Linux PC qemu-aarch64's `-cpu cortex-a710` has it (`host: ... PAC yes`), and
  `wifi=sim` boots there (2026-11-13: en0 named, enabled by configd, wifid's attach; [hardware.md](hardware.md) 3, "en0
  late"). On that 4-CPU PC under other agents' load configd reached the naming 13-26 minutes into the boot, so give
  such a boot `timeout` 4000 s or more; iOS's own log can be read from the running boot's `data.img`
  (`oneios-tool ioslog show ... --proc configd`), not always after a `timeout` kill.
- Bluetooth on the PC: add `--set bluetooth=sim`: a stand-in controller (`runtime/bridges/bt/hci_sim.c`) behind the
  bridge channel, with the bridges off; `/bluetooth` gets transport-encoding 1 as with the phone's HAL. xnuhost logs
  `bluetooth: on: a stand-in controller (bluetooth=sim)`, then `bluetooth: sim: the stand-in controller is on` when
  bluetoothd connects, and with `-v` each command (`bluetooth: sim: command 0x0c03, ...`). The guest's lines start
  with `ONEIOS BT:`; the IOKit lookups of bluetoothd and BlueTool are `oneios-iokit:` lines (hosted 0062, on by default
  for those two; `oneios-iokit=NAME,...` for others). bluetoothd's own lines are in iOS's log after the run:
  `oneios-tool ioslog show out/run/data/data.img --proc bluetoothd` (section 10). A first boot on the 4-CPU PC
  reached bluetoothd about 30 minutes in (2026-11-13).
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
  frame pointers; `0026`). Give a long line's addresses to `oneios-tool sym` for the slide, then name the rest
  with `--slide`.
- Who opens the GPU, and whether the Metal plug-in's folder was mounted: `oneios-gpu-addon:` and `oneios-gpu-open:` lines,
  always on (`docs/gpu.md` 5.1).
- IOKit calls of a process: `oneios-iouc=NAME[,NAME...]` logs which user client methods those processes call and
  what they return (`oneios-iouc: backboardd[53] IOMobileFramebufferUserClient selector 5 -> 0x0 (call 1)`: the
  first call of each class and selector, each change of status, and calls 16, 64, 256, ...; `0029`).
- File system calls that fail: `oneios-sysfail=NAME[,NAME...]` (`*` for every process) prints each failing call
  that takes a path, with the path and the errno (`oneios-sysfail: installd[99] mkdir(...) -> errno 13`); `ENOENT`
  only with `oneios-sysfail-enoent=1`; `0034`.
- The display backend's counters: `oneios-display-report=SECONDS` prints, every so often, the vsync tick, swaps
  validated, accepted and refused, the backend's kick statistics, the power state the family's swap path tests
  (`display +7180`), the power manager's calls, and the last swaps' timeline (`0030`).
- What the PC cannot show: seccomp (qemu-user has none; a marked trap instruction stands in), real timing, the
  phone's GPU, panel, HALs, SELinux and KernelSU.

## 6. On the phone

### Test cycle on the phone

**Phone and app work goes through `oneios-tool`** (`phone`, `app`, `cycle`; `tools/oneios-tool/README.md` has every
option). Use adb, the shell scripts or other tools directly only when the CLI fails or lacks the command, then say
so and fix or extend the CLI. **The CLI has not run on the real phone yet** (written and tested against a stand-in
phone on a box without one, 2026-11-12): until its first run there, the commands of "The same by hand" below are
the ones that were run, and `scripts/phone/stage.sh` stays.

```sh
T=tools/oneios-tool/target/release/oneios-tool        # cargo build --release --manifest-path tools/oneios-tool/Cargo.toml
$T phone status                    # root, the installed app, the session, oneios_root.sh status (@@RESULT ready 1: a switch would start)
$T phone stage runtime DIR         # an unpacked runtime package        (scripts/phone/stage.sh runtime DIR)
$T phone stage image DIR           # kernel.kc, kernel.kc.hashes, manifest.json, overlay/... at their paths
$T phone sums                      # the staged files' sha256
$T phone image-check               # the staged manifest.json lists the staged files (switch checks it first)
$T phone manifest DIR --stage      # DIR's files (a new kernel.kc, kernel.kc.hashes) with the phone's manifest, their entries new
$T phone switch                    # oneios_root.sh switch <the installed app's versionCode> 1
$T phone log --follow --markers    # kernel, launchd, backboardd, SpringBoard, the first frame, a panic, as they come
                                   #   (right after a switch it waits for this session's log: the switch replaces it ~40 s in)
$T phone log --gpu --tail 200      # named filters: --panic --markers --display --gpu --wifi --hid --mem; --grep REGEX; --since SECONDS
$T phone log --follow --save-auto  # also into out/session-logs/DATE/live-HHMMSS.log; --analyze: timeline and stacks at the end
$T phone log --list                # the logs by name: xnuhost, switch, return, outcome, display, watchdog, logs/xnuhost-<epoch>.log
$T phone status --watch            # the status again every 5 s
$T phone return                    # session/return.request, then waits for "One UI restored in place"
$T phone pull-data OUT --ioslog    # the Data store's copy (no session open), then ioslog boots and checks
#   pull-data by hand: adb exec-out "su -c '/data/adb/ksu/bin/busybox gzip -1 -c /data/local/tmp/oneios/data/data.img'" | $T phone sparse-write OUT --gunzip
```

One command for the whole cycle (each step can be left out with `--only` or `--skip`; `--dry-run` says what each
would do and touches nothing):

```sh
$T cycle --kernel-out NAME --runtime-version 0.1.8-dev --tests --hold 120 --name gpu1
$T cycle --skip build,tests --image DIR --runtime DIR      # stage what is built already, switch, watch, hold until Ctrl-C, return, collect
$T cycle --only return,collect --name NAME                 # end a session that was left open (--stay) and take its logs
```

| Step | What it does |
|---|---|
| `build` | Only what changed. The hosted kernel (`scripts/bg.sh cycle-kernel env HOSTED_OUT=NAME XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh`) when `--kernel-out NAME` is given and `xnu.macho` is older than the newest file of `kernel/patches/oneios-hosted` (or missing). The collection (`kernel/prelink/build.sh` with `KERNEL=` and `REFERENCE=` of that tree) when the kernel is newer than `out/kernel/24A446/kernel.kc`; **the cycle stops unless the build ends with status 0 (the audit: 0 failures) and `oneios-tool prelink stubs-check` says `0 bad`**. The runtime (`runtime/package.sh V`, unpacked into `out/cycle/runtime-V`) with `--runtime-version V`; `--runtime DIR` takes one that is built. The image parts (`kernel.kc`, `kernel.kc.hashes`; not `manifest.json` or `xnuhost.conf`: `out/image/24A446` is the Mac's own set, not the phone's) are gathered in `out/cycle/image`; `--image DIR` takes a folder instead, which is also where iOS plug-ins built elsewhere go. The iOS plug-ins build on macOS only (`--force plugins`). The app: its source's version is said; `--force app` runs `app build` and `app install`. `--force kernel,prelink,...` builds a part whatever its state. Long builds run in tmux and the cycle waits for their exit code |
| `tests` | `--tests`: `runtime/host-tests.sh`; a failure stops the cycle before the phone is touched |
| `stage` | `phone sums`, then `stage image` with only the files whose SHA-256 differs from the phone's and `stage runtime` when a file of it differs, then `phone sums` again, which must equal the local hashes (`sums.txt`). Files staged with no `manifest.json` of their own get one made from the phone's (`phone manifest`); then `phone image-check`. `data.seed` is never staged |
| `switch` | First the way back: root, no session, `oneios_return.sh`, `oneios_switch.sh` and `bin/oneios_root.sh` staged, no import in progress (`runtime.new`, `image.new`), and the root script's preflight says `ready 1`. Then `phone switch` |
| `watch` | Reads `session/xnuhost.log`'s new lines every 2 s, and after a switch only once the file changed at or after the phone's second of the switch (until the switch replaces it, about 40 s in, it is the last session's: its first-frame line made the watch say "ready after 0 s", 2026-11-12) and prints each marker once with its time: xnuhost, kernel, darwin, bsd-root, init, launchd, backboardd, SpringBoard, ready (the first frame), PANIC, end. On a panic, when the session ends by itself, or after `--ready-timeout` (300 s) without the first frame: straight to the return, and the cycle's status is 1 |
| `hold` | `--hold S` seconds, else until Ctrl-C. Ctrl-C returns, unless `--stay`, which leaves the session open (end it with `phone return` or `cycle --only return,collect`). A panic or the session's own end during the hold returns |
| `return` | `session/return.request`, then waits (180 s) for `One UI restored` in what `return.log` gained |
| `collect` | `out/session-logs/DATE/NAME/`: `xnuhost.log`, the last 2000 lines of `switch.log` and `return.log`, `return.outcome`, `sums.txt`, and `summary.md` (versions, hashes, each step's time and result, the markers with their times, the panics, the way back). `--pull-data` adds the Data store's copy and `ioslog`. The summary's result line names every step its Steps table has as failed (`every step that ran ended well` only when none did), and a failed `collect` ends the cycle with status 1. A run that did not watch (`--only return,collect` after `--only stage,switch,watch` with the same `--name`) reads the markers from the saved `xnuhost.log` (this folder's copy, else the phone's `session/xnuhost.log`) and says so |

A cycle takes one phone lock (`out/phone.lock` of the main checkout, also from a worktree) and refuses when
another cycle holds it. That lock is this tool's own: **the coordinator's `phone-lock.sh` is still the lock between
agents** until the two are one.

### The same by hand (when the CLI fails or lacks a command)

What the area agents followed to try a build in a real session (bringup, 2026-11-12; every command was run). The
phone is the S24 Ultra on adb, KernelSU root, the OneOS fork 0.4.86 (versionCode 115) with its stage at
`/data/local/tmp/oneios`. One agent at a time: hold the phone lock first (the coordinator's `phone-lock.sh`), and
restore the baseline (`phone-baseline.txt` next to that script) before you release it.

1. **Stage** (One UI up, no session open; `scripts/phone/stage.sh` refuses during a session):

   ```sh
   scripts/phone/stage.sh runtime DIR        # DIR: an unpacked runtime package; through oneios_root.sh begin/receive/finish
   scripts/phone/stage.sh image DIR          # DIR: kernel.kc, kernel.kc.hashes, manifest.json, overlay/... at their paths
   scripts/phone/stage.sh sums               # the staged files' sha256
   ```

   Unpack a package with `mkdir -p D && zstd -dc out/release/oneios-runtime-V-arm64.tar.zst | tar -xf - -C D`.
   `image` replaces only the files given and writes their lines in `image.sha256` and `image.ids` again (the
   preflight checks both); give it a `manifest.json` whose entries list the staged files: xnuhost checks the set against it and
   refuses a file the manifest does not list (2026-11-12: another set's manifest ended a session 2 s in).
   `oneios-tool phone manifest DIR --stage` makes it from the phone's and stages both; `phone image-check` checks. Never stage `data.seed` from the Mac's
   `out/image/24A446`: it differs from the phone's, and a new seed makes the Data store start over.
   `runtime-asis DIR` puts back a backed-up stage whose files were replaced by hand.
2. **Start** (the app's own path, the same preflight and the tick):

   ```sh
   adb shell "su -c 'sh /data/local/tmp/oneios/bin/oneios_root.sh status'"       # @@RESULT ready 1: a switch would start
   adb shell "su -c 'sh /data/local/tmp/oneios/bin/oneios_root.sh switch 115 1'" # @@OK switch; 115 = the app's versionCode
   ```

   The switch takes about 60 s (the framework stops in about 40 s). Then `xnuhost I guest ready: first frame` comes
   about 80 s after `cpu0: entering the kernel` (Linux `kernel.kc` 9476a867 + runtime 0.1.7-dev, 2026-11-12).
3. **Watch** (each line once a stage is reached; the log of the last session stays in `session/` until the next
   switch moves it to `logs/`, so match the new `epoch` in `switch.log` first):

   ```sh
   adb shell "su -c 'grep epoch /data/local/tmp/oneios/switch.log | tail -1; tail -3 /data/local/tmp/oneios/switch.log'"
   adb shell "su -c 'grep -m1 -e \"guest ready\" -e \"panic(\" -e \"stopping:\" /data/local/tmp/oneios/session/xnuhost.log'"
   adb shell "su -c 'grep oneios-gpu-report /data/local/tmp/oneios/session/xnuhost.log | tail -3'"   # frames per 10 s
   adb exec-out "su -c 'cat /data/local/tmp/oneios/session/xnuhost.log'" > out/phone-baseline/DATE/sessionN-xnuhost.log
   ```

   A panic: the kernel's frames are unslid collection addresses; `oneios-tool prelink stubs-check KC` and
   `oneios-tool prelink unmapped` name what the collection got wrong. `adb logcat` on this phone is bind-mounted over by
   the owner's ksud: read logcat from a private mount namespace
   (`/data/adb/ksu/bin/busybox unshare -m --propagation private sh -c 'umount /system/bin/logcat; logcat -d'`).
4. **Measure** beside the session, read only: `scripts/phone/survey.sh session > out/phone-baseline/DATE/X.txt`
   (thermal zones, cpufreq, KGSL, battery, load, `bridgectl cpu`, `gpuctl nodes`, xnuhost's threads, OOM score,
   cgroup and the devices it holds; without `session`, the first five only, for One UI). What the numbers were on
   2026-11-12: `docs/hardware.md`, "Baseline on the phone". Do not run `bridgectl bt`, `net`, `haptic` or `audio` during a session: xnuhost's own bridges
   hold the Bluetooth HAL, the tap, the vibrator and the audio helper.
5. **Return**: end every session yourself. The Volume Up + Volume Down chord did not return in the sessions of
   2026-11-12 (agent touch is on it):

   ```sh
   adb shell "su -c 'echo AGENT: done > /data/local/tmp/oneios/session/return.request'"
   adb shell "su -c 'tail -1 /data/local/tmp/oneios/return.log; cat /data/local/tmp/oneios/return.outcome; pidof system_server'"
   ```

   The watcher sees the file within a second; the return takes about 45 s and ends with `One UI restored in place
   (Enforcing).` and `return.outcome` `inplace <epoch> <boot id>`. Five returns on 2026-11-12 (two after a panic,
   three asked) all came back in place: Gates A, B and C passed, user 0 unlocked 13-15 s after zygote.
6. **Restore the baseline** (`phone-baseline.txt`) and check it with `scripts/phone/stage.sh sums`.

### Hardware checks

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
`bridgectl cpu|haptic|haptic-pattern|bt|audio`, `wifictl status|scan` (read only; `join NAME PASSPHRASE` takes the Wi-Fi from One UI while it runs), `brightctl info|levels|als`, `batteryctl [watch S]`, `thermalctl [watch -i S -n N | top -i S]`
(read only; `watch` logs temperatures, clocks, battery current and the session's CPU, `top` xnuhost's busiest threads), `camctl --backend ndk list|capture`.
Each tool's README has its exact commands and what a pass looks like. **None of this has run on the phone yet.**

**Getting the image set onto the phone:** it is about 11 GB and never published. Serve it from the PC privately
over several Cloudflare quick tunnels at once, so a download manager fetches it in parallel parts from all of them
(the server, `oneios-tool rangeserve` (`scripts/serve/rangeserve.py` until 2026-11-13), binds 127.0.0.1 and answers byte ranges; Python's `http.server` does not):

```sh
scripts/serve/serve-image.sh out/image/OneiOS_iOS27.0.1_24A446_iPhone18-2.tar.zst 4   # 4 tunnels; prints the URLs
# on the phone (Termux, pkg install aria2): aria2c -x 16 -s 16 -k 8M -o <name> <url1> <url2> <url3> <url4>
scripts/serve/serve-image.sh --stop                                                    # when done
```

URLs are also in `logs/serve-urls.txt`; each tunnel's log is `logs/serve-tN.log`. Check the download against the
`.sha256` (same URL plus `.sha256`), then in the app: iOS 27 (OneiOS) → I have the file → the download.

## 7. The OneOS fork app

With `oneios-tool` (first choice; **not used for a real build or install yet**: the Linux PC builds no app and has no
JDK 21 or Android SDK, by the owner's decision, so the tool was tested there with stand-ins only):

```sh
T=tools/oneios-tool/target/release/oneios-tool
$T app version           # applicationId, versionName, versionCode, the commit; with a phone: the installed version and certificate
$T app doctor            # JDK 21, an Android SDK with platform 37, the GitHub Packages login (a token is never printed)
$T app build             # cd app-android && ./gradlew test assembleFullDebug (--no-test); then the APK's path, size, sha256, version
$T app install           # adb install -r of that APK; refuses another signing certificate (never uninstall: that wipes the app's data)
$T app install --release 0.4.86      # the CI prerelease oneios-0.4.86 instead, through gh
$T app stage-scripts     # the fork's oneios_switch.sh, oneios_return.sh, oneios_root.sh into the stage, sha256 before and after
```

The checkout is `--oneos DIR`, `ONEOS_DIR`, the current folder, or `OneOS` beside the OneiOS main checkout; each
command says which. By hand, and what the tool runs:

Repo `akshaynexus/OneOS`, branch `oneios`. A push to `oneios` runs `.github/workflows/oneios-apk.yml`: it builds
the full debug APK (the workflow's own `GITHUB_TOKEN` reads the SESL packages), runs the unit tests and publishes
the prerelease `oneios-<version>` (0.4.75 is the first with the Hardware checks screen). Locally: a `read:packages` token in `~/.config/twidget/github.properties`
(`ghUsername`, `ghAccessToken`), then `cd app-android && ./gradlew test assembleFullDebug`.

## 8. Releases

A tag `vN.N.N` on `main` runs `.github/workflows/release.yml`, which builds the runtime package with the pinned NDK
and attaches it and its `.sha256` to the release. No kernel, no image. See [releases.md](releases.md).
The first release is [`v0.1.0`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.0) (2026-11-10).

## 9. iOS user-space code (executables, dylibs, bundles): Xcode on macOS, a cross toolchain on Linux

The same scripts on both hosts (`scripts/ios/env.sh` picks the parts by `uname -s`):

- **macOS** (Apple Silicon, Xcode 27 with the iOS platform): Xcode's own clang, ld and iPhoneOS SDK, through
  `xcrun`. Nothing to build or install from Apple beyond Xcode.
- **Linux**: the method of `macos_helios300` (its `CROSS-BUILD.md`): the host's clang against the SDK that xtool
  extracts from Xcode, linked by Apple's ld64 built here, signed ad hoc here.

Either way every link goes against the **guest's own iOS 27 stubs** (`out/ios27-tbd`, made from 24A446's shared
cache) before the SDK's, and every file is signed by `scripts/ios/ios-sign.py`.

```sh
scripts/setup/ios-toolchain.sh          # one line: "ios toolchain: ok: SDK ..., <linker>, <clang>, iOS 27 stubs N"
                                        #        or "ios toolchain: missing: A; B" (each with the command that makes it)
scripts/bg.sh ios-ld scripts/setup/ios-toolchain.sh --ld      # Linux, once: ld64 with .tbd support -> out/ios-toolchain (about 6 min)
scripts/bg.sh ios-tbds scripts/setup/ios-toolchain.sh --tbd   # once per image set: the iOS 27 stubs -> out/ios27-tbd
scripts/setup/ios-toolchain.sh --test   # builds, signs and checks out/ios-test/hello
```

`--tbd` reads `out/image/24A446/systemos.img`. Before the image set exists, give it the decrypted SystemOS
image of the fetch: `scripts/setup/ios-toolchain.sh --tbd --image inputs/24A446/043-70037-715.dmg`.

| Part | What | macOS | Linux |
|---|---|---|---|
| SDK | headers (and stubs, as a fallback) | Xcode's: `xcrun --sdk iphoneos --show-sdk-path` (iPhoneOS27.2 with Xcode 27.2) | `xtool sdk install <Xcode.xip>` puts it under `~/.config/swiftpm/swift-sdks/darwin.artifactbundle/.../SDKs/`; the newest `iPhoneOS*.sdk` there is used |
| linker | reads `.tbd` stubs | Xcode's ld (`ld-27106.1` with Xcode 27.2) | Apple's ld64-956.6 with libtapi: `--ld` builds cctools-port at the kernel's pin (`kernel/pins.sh`) with tpoechtrager/apple-libtapi `2a57054` into `out/ios-toolchain`. The kernel's ld64 (`out/toolchain`) cannot read a `.tbd` |
| clang | arm64e | Xcode's (Apple clang 21 with Xcode 27.2) | the host's, LLVM 19 or newer |
| iOS 27 stubs | what 24A446 really exports | `--tbd` runs `scripts/ios/make-tbds.py`: one `.tbd` per image of the guest's own shared cache, written by `ipsw dyld tbd`, laid out like an SDK in `out/ios27-tbd/`. The cache is read from `out/ios27-cache/` when that copy exists, otherwise from the SystemOS image, attached with `hdiutil` (macOS) or `apfs-fuse` (Linux). Derived from Apple's binaries: never committed | same |
| signing | ad hoc, 4 KiB pages | `ios-sign.py` with Xcode's `codesign_allocate` | `ios-sign.py` with cctools' `codesign_allocate` |

`IOS_SDK`, `IOS_LD`, `IOS_CLANG`, `IOS_TBD` and the others in `env.sh` override each part. Every build prints the
SDK it used and writes it into its build record. The deployment target is 27.0 (`LC_BUILD_VERSION` minos 27.0);
the `sdk` field is the SDK's (27.2 from Xcode 27.2 on the Mac, 26.5 from xtool's on the Linux PC until the owner
installs Xcode 27.1's there with `xtool sdk install`).

The helpers in `scripts/ios/` (settings and their overrides are in `env.sh`):

```sh
scripts/ios/ios-clang -O2 x.c -framework CoreFoundation -o x                    # executable (ios-clang++ for C++/ObjC++)
scripts/ios/ios-clang -dynamiclib -install_name /usr/lib/libx.dylib x.c -o libx.dylib
scripts/ios/ios-clang++ -bundle -fobjc-arc x.mm -framework Metal -o X           # a bundle's executable
scripts/ios/ios-sign.py --identifier com.oneios.x [--entitlements e.plist] x    # ad-hoc signature; prints the CDHash
scripts/ios/macho-check.py x                                                    # every import against the iOS 27 stubs
```

- `ios-clang`: `clang -target arm64e-apple-ios27.0 -isysroot $IOS_SDK`, with `ios-ld` as the linker.
- `ios-ld`: the linker with the iOS 27 stubs as the first `-syslibroot` (so a symbol iOS 27.0.1 does not export
  fails the link) and `-fixup_chains` (Linux's ld64 writes the old `LC_DYLD_INFO` opcodes unless told; Xcode's ld
  writes chained fixups for iOS anyway and takes the flag).
- `ios-sign.py`: one SHA-256 code directory with 4 KiB pages (this guest's kernel runs 4 KiB pages), an empty
  requirement set, the entitlements as XML and DER when given, an empty CMS blob. It prints
  `<cdhash> <arch> pages=4096 <identifier> <file>`. **Its output is byte for byte what Apple's
  `codesign --force --sign - --pagesize 4096 [--entitlements PLIST] --identifier ID` writes**, so a file has the
  same CDHash whichever of the two signed it: compared with Xcode 27.2's codesign on 2026-11-12 for an executable
  (16 identifier lengths times no entitlements and 8 entitlement sets: 144 of 144 identical) and for Turnip's dylib.
  It leaves codesign's room for the signature (its length plus 8, rounded up to 16). No bundle seal
  (`_CodeSignature/CodeResources`): the kernel checks a mapped library by its CDHash alone.
- `macho-check.py`: arm64e, platform iOS, signed with 4 KiB pages, every library has a stub, every undefined symbol
  is exported by the library it is bound to (re-exports followed). `--local FILE` names a dylib that travels with
  the file; `--sdk` checks against the SDK's stubs instead. On macOS it runs Xcode's `llvm-nm` and `llvm-objdump`.

**Trust.** The guest runs a binary whose CDHash is in a trust cache. Ours get there through the image hook: it reads
the CDHash from each signed Mach-O it places and writes `overlay/userland/owner-trust.json` ([userland.md](userland.md)).
The Metal plug-in's build also writes its CDHashes into `out/userland/NativeAdrenoMetal.build.json`, and the hook
stops the image build when the placed bundle no longer matches that record.

Checked on the Mac on 2026-11-12 (Xcode 27.2, stubs of 24A446): `scripts/setup/ios-toolchain.sh` prints `ok` (SDK 27.2,
ld-27106.1, Apple clang 21.0.0, 4723 stubs); `--tbd` made the stubs from the fetched SystemOS image in 5 minutes
(cache `77BC8C63-93DA-3E9B-BB9C-0BE2AF0C0E17`, 4696 images, 0 failed); `--test` builds `out/ios-test/hello` (arm64e,
minos 27.0, sdk 27.2, 50224 bytes) and `macho-check.py` passes. The linker's trace (`-Wl,-t`) shows every library read
from `out/ios27-tbd`, and a call to a function no 24A446 library exports fails the link. `userland/helpers/build.sh`
builds the four helpers in 2 seconds; each passes `macho-check.py`.

Checked on the Linux PC on 2026-11-11 (SDK 26.5, stubs of 24A446): `--test` built `out/ios-test/hello` (50432 bytes:
ios-sign.py then left more room than codesign), `rcodesign print-signature-info` parsed the signature, and
`macho-check.py` passed.

**Not checked: that the guest runs any of it.** No boot has exec'ed a binary built by either host.

### The capture plug-in (the camera)

```sh
userland/camera/build.sh                # -> out/userland/camera/H9ISP.mediacapture, cdhashes.txt (2 s on the Mac)
userland/camera/tests/host.sh           # the reader; on a Mac also the device under macOS's own CMCapture (35 checks)
```

Then rebuild the image set (section 1): the image hook places `H9ISP.mediacapture` in the Data seed at
`/private/var/oneios/addons/MediaCapture` and lists its CDHash (`ONEIOS_CAMERA_PLUGIN=DIR` for another folder). That
is a new seed: the phone's Data store starts over. The kernel needs AppleH9CamIn and hosted 0056
([userland/camera/README.md](../userland/camera/README.md)). Checked on the Mac on 2026-11-12: arm64e, 4 KiB pages,
`macho-check.py` PASS against the 24A446 stubs (114 symbols bound). Not run in iOS.

### The Metal plug-in

```sh
scripts/setup/get-references.sh         # NativeMetal, NativeAdreno, AdrenoPM4, metal2vulkan, mesa-droidvm, Droid-VM-UniXNU
rustup toolchain install 1.98.0 && rustup +1.98.0 component add rust-src
# macOS: brew install ninja glslang shaderc spirv-tools bison llvm; uv pip install --python "$(command -v python3)" meson mako pyyaml
scripts/bg.sh metal userland/metal/build.sh
# -> out/userland/NativeAdrenoMetal.bundle, NativeAdrenoMetal.build.json, NativeAdrenoMetal.check.txt, NativeAdrenoMetal.gpuaddon
```

Then rebuild the image set (section 1): the hook places the bundle and lists its two CDHashes. What the build does,
what was changed for the iPhone's shared cache and what is verified: [gpu.md](gpu.md) section 7.

## 10. Rust tools (`oneios-tool`)

`tools/oneios-tool` is one binary with the project's host tools in Rust: `sym` (the backtrace symbolizer; it replaced
`scripts/debug/dsc.py`, deleted 2026-11-12), `log` (reads an xnuhost session log), `image` (`scripts/image/imagetool.py`), `ioslog` (iOS's
own unified log out of a copy of the phone's Data store, without a mount) and `prelink` (all twelve steps of
`kernel/prelink/build.sh`, behind `PRELINK_IMPL=rust`), and `kernel` (the kernel build's own steps: the
board archive generators `kernel gen NAME`, the build record, the records made from the kernelcache, and since 2026-11-13
the drivers' build, the fixup of the linked kernel, the series check and the KDK headers), `package` (the runtime package's
Apple-file check, manifest and archive) and `debug` (the tests' helpers). Since the third Linux round also `phone` (the adb
workflow of section 6: status, stage, sums, switch, return, log, the Data store's copy), `app` (the fork's app of
section 7: version, doctor, build, install, stage-scripts) and `cycle` (one test cycle: build, tests, stage, switch, watch, hold, return, collect).
**None of the three has run on the real phone yet**: sections 6 and 7 show the CLI first and, as the fallback, the
scripts and adb lines that were run. Commands, crates, and each tool's comparison with the Python tool it replaces:
[tools/oneios-tool/README.md](../tools/oneios-tool/README.md); what is left:
[tools/oneios-tool/todo_rust.md](../tools/oneios-tool/todo_rust.md).

```sh
cargo build --release --manifest-path tools/oneios-tool/Cargo.toml     # -> tools/oneios-tool/target/release/oneios-tool
cargo test  --release --manifest-path tools/oneios-tool/Cargo.toml     # unit tests, no Apple file
```

Optional on Linux: **apfs-fuse** (github.com/sgan81/apfs-fuse, with `fusermount3`), a second APFS reader that
`oneios-tool image apfs-ls`, `apfs-copy` and `ioslog` use with `--apfs fuse`, or by themselves when our own reader
refuses an image (`--apfs auto`, the default); the comparison scripts check our reader against it. Nothing needs
it: our reader reads the sealed volumes and their compressed files, so the dyld shared cache for `sym` comes out of
the image with `oneios-tool image apfs-copy out/image/24A446/systemos.img System/Library/Caches/com.apple.dyld DIR`
(no mount, Linux and macOS).

Rust 1.95 or newer (apk-info-zip 1.1.1 needs 1.95; built with 1.98.0 and 1.99.1); the first build downloads the crates and one git dependency
(macos-unifiedlogs, pinned by commit in `Cargo.lock`). Linux x86_64 and macOS arm64. `oneios-tool --help` and
`oneios-tool TOOL --help` list every command and option (one clap layout for all tools).

**The image scripts, the kernel builds, the prelink and the runtime package run no Python tool of this repository any more** (still Python: `scripts/ios/`, the root tweaks):
run the Rust tool only:

| Script | Switch | Effect |
|---|---|---|
| `scripts/image/build.sh`, `verify.sh`, `kernel-inputs.sh`, `scripts/fetch/fetch.sh` | none: Rust only since 2026-11-13 (`ONEIOS_TOOL=PATH` for another binary; `IMAGETOOL_IMPL` is gone; `scripts/image/imagetool.py` stays in the tree, unused, until the owner decides) | `oneios-tool image` runs every subcommand, `check-ipsw`, `fetch`, `record-plain`, `aea-info`, `pack` and the archive's `verify` too. Before that day: `oneios-tool image` runs profile, apfs-info, apfs-owners, hashes, tree-copy, seed-pack, manifest and the set folder's verify; pack and the archive's verify stay with the Python tool. Same files, byte for byte (README "image") |
| `scripts/image/kernel-inputs.sh` | none needed; `IM4P_TOOL=PATH` forces it | without the `lzfse` program the kernelcache is decoded by `oneios-tool image im4p`; the result is checked against the pinned SHA-256 either way (section 1) |
| `kernel/build-hosted.sh`, `kernel/build-linux.sh`, `kernel/build-macos.sh` | none: Rust only since 2026-11-13 (`ONEIOS_TOOL=PATH` for another binary) | the board archives (`storage`, `board`, `aes`, `keystore`, `net`, `hid`, `audio`, `gpu`, `display`) are built by `oneios-tool kernel gen NAME`, which UniXNU's driver runs from `board.toml`, and the VM kernel's build record by `oneios-tool kernel build-record`; the Python generators were deleted after every file they write was compared byte for byte, and their outputs are frozen in `tests/kernelgen/golden/` (README "kernel") |
| `kernel/prelink/build.sh` | none: Rust only since 2026-11-12 (`ONEIOS_TOOL=PATH` for another binary) | `oneios-tool prelink` runs all twelve steps; the Python steps were deleted after every step's output was compared byte for byte, and their outputs are frozen as hashes (`tests/prelink/compare-golden.sh`). 14 s instead of two minutes |
| `runtime/package.sh` (and CI) | none: Rust only since 2026-11-13 (`ONEIOS_TOOL=PATH` for another binary) | `oneios-tool package check\|manifest\|archive`: the Apple-file check, `runtime.json`, the archive without GNU tar; `runtime/package/pack.py` was deleted after 91 frozen cases and the 19 package tests gave the same |
| `kernel/tests/host.sh`, `drivers/tests/host.sh`, by hand | none | `oneios-tool kernel series-check`, `kernel drivers --check`, `kernel hosted-fixup`, `kernel kdk-headers`, `kernel iig-fix`, `prelink classes\|stubs-check\|unmapped\|port-fixture\|lead-between\|lead-imports`: the scripts of those names were deleted on 2026-11-13 (README, the group 2 section) |

`oneios-tool image seed-pack` writes `data.seed`'s members in parallel, each at its own offset (the same bytes as
`imagetool.py`'s; the SHA-256 is the one the trust list pins). `ONEIOS_IMAGE_TIMING=1` prints each phase's time;
`ONEIOS_SEED_SINK=map` (a mapped output, faster on tmpfs) and `plain` (no `copy_file_range`, the macOS path) choose
another way to write the same bytes. The floor and the numbers: README "seed-pack: where the time goes".

iOS's own log of a session, from a copy of the Data store (replaces the mount-and-copy steps of
[newtodo.md](newtodo.md) "How to resume" step 2; no mount, Linux and macOS):

```sh
oneios-tool ioslog boots data.img                                   # which boots the log holds
oneios-tool ioslog show data.img --boot last --proc backboardd      # text; -f csv|jsonl -o FILE for a file
oneios-tool ioslog checks data.img                                  # counts of the known problems' lines, last boot
oneios-tool ioslog save data.img out/session.logarchive             # the .logarchive the manual steps made
oneios-tool image apfs-checkpoints data.img                         # the store's checkpoints: which pass the checks, and why the others do not
```

Every `ioslog` command says on stderr which checkpoint of the store it read (`... is read at checkpoint xid N ...`)
and which newer ones it skipped and why: a store copied from the phone ends with a killed kernel, so its block 0
is old and its newest checkpoint may be half written. `--checkpoint XID` reads one checkpoint, `--block0` block
0's state. Tested on stand-in images (among them stores whose newest checkpoint is cut off), on Mandiant's test
archives (CSV byte for byte what `unifiedlog_iterator` writes) and on a Data store that iOS's own APFS wrote on
the PC. The first run on the phone's store (2026-11-13) failed with `short read at 1152903912689233920`: fixed
(README "The APFS reader"); **the phone's own store has not been read again since** (it stays on the Mac).

A kernel panic of a session (`panic(cpu N caller 0x...)`, the fault's pc and lr, the `frame 0x...` lines), named in
the `kernel.kc` that ran and the `xnu.macho` it was prelinked from: member and segment, function + offset,
file:line and inlined frames. The first call for a pair of files builds an index under `out/ksym-index/` (2 to 9 s);
later calls take milliseconds:

```sh
oneios-tool sym kernel xnuhost.log                                  # or: oneios-tool log panic xnuhost.log
oneios-tool sym kernel --kc out/kernel/24A446/kernel.kc --kernel out/kernel/current-hosted/xnu.macho xnuhost.log
oneios-tool sym kernel --slide 0x2ff9000000 0x200454c094            # an address of the running kernel, by hand
```

Without `--kc` and `--kernel` (or `ONEIOS_KC`, `ONEIOS_KERNEL`) it takes the newest `out/kernel/*/kernel.kc` and
the `xnu.macho` its prelink record names, says which and their SHA-256, and warns when the two do not belong
together. File:line needs the kernel's DWARF: the object files of the build tree that `xnu.macho`'s debug map names
(as `kernel/build-hosted.sh` left them when `xnu.macho` was linked), or a dSYM (`--dsym`). Compared with
`llvm-symbolizer` on three Linux-built kernels; **not yet run on a panic log from the phone** (README "sym
kernel": the address rules, the comparison, what was not run).
