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
| Rust 1.88+ (`cargo`; rustup) | `tools/oneios-tool`: the kernel's board archives and build record (section 2), the kernel collection's build (section 3), the image tools and the phone workflow (section 10) | the build scripts run `cargo build --release --manifest-path tools/oneios-tool/Cargo.toml` when the binary is missing or older than its sources (`scripts/lib/oneios-tool.sh`); `ONEIOS_TOOL=PATH` names a built one. Built with 1.98.0; needs a C compiler (`sha2`'s `asm`). `runtime/package.sh` also builds it for the phone: `rustup target add aarch64-linux-android` (section 4) |
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
P='profiles/ios-27.2.1-iPhone18,2.toml'
scripts/fetch/fetch.sh --list "$P"                  # optional: compare the profile with Apple's IPSW (about 1 MiB)
scripts/bg.sh fetch scripts/fetch/fetch.sh "$P"     # 10.9 GiB of the IPSW's members from Apple, into inputs/24B156/
scripts/image/kernel-inputs.sh "$P"                 # Apple's kernelcache and device tree -> out/kernel-inputs/24B156/
                                                    # (without inputs/24B156/ it fetches only those, 24 MB)
```

`kernel-inputs.sh` decodes the two IM4P members (the kernelcache's payload is LZFSE) with `oneios-tool image im4p` (since
2026-11-13 always; before, `imagetool.py im4p` when the
`lzfse` program is on `PATH`, and otherwise with `oneios-tool image im4p` (LZFSE by the `lzfse_rust` crate, no other
program; build it with `cargo build --release --manifest-path tools/oneios-tool/Cargo.toml`, section 10).
`IM4P_TOOL=PATH` names a `oneios-tool` binary and makes it the decoder in either case. Whichever ran, the script then
checks `kernelcache.macho` against the SHA-256 the kernel profile pins (`kernel/unixnu/profiles/os/ios-27.0.1-24A446/
profile.toml`, `kernelcache_sha256`) and stops when it differs. On the Linux PC on 2026-11-12, without `lzfse`:
78135296 bytes, sha256 `349c4692...0c57e3` (24A446's kernelcache, the then-pinned one; the profile now pins
24B156's: 78512128 bytes, `f6f31ac4...e9a5939`), in
0.7 s. Not compared: the two decoders run side by side on one host (no `lzfse` program on the PC).

The set is built after `kernel.kc` exists (step 3), so that it carries the kernel:

```sh
scripts/bg.sh image scripts/image/build.sh "$P" --kernel out/kernel/24A446/kernel.kc
# -> out/image/24B156/ and out/image/OneiOS_iOS27.2.1_24B156_iPhone18-2.tar.zst (+ .sha256), about 11 GB
scripts/image/verify.sh out/image/OneiOS_iOS27.2.1_24B156_iPhone18-2.tar.zst
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
                                  #    and the phone checks (gpuctl, bridgectl, wifictl, brightctl, batteryctl, thermalctl, displayctl, camctl);
                                  #    then the NEEDED and symbol check of the stage (runtime/package/elf-check.sh, below)
runtime/package.sh 0.1.0-dev      # -> out/release/oneios-runtime-0.1.0-dev-arm64.tar.zst + .sha256 + runtime.json,
                                  #    with bin/oneios-tool (below), and oneios-tool-0.1.0-dev-android-arm64 + .sha256
runtime/package/elf-check.sh DIR  # can Android load every library and dynamic helper under DIR? (a stage, or one file)
runtime/host-tests.sh             # every host test of the runtime, one summary line per part (below)
runtime/build.sh --test           # Linux: the loader, xnuhost and display tests (xnuhost's with the Android binary on
                                  #   qemu-aarch64); macOS: the same as runtime/host-tests.sh
```

**`bin/oneios-tool`, the Rust tools for the phone** (2026-11-15). `runtime/package.sh` builds `tools/oneios-tool` for
`aarch64-linux-android` (`cargo build --release --target aarch64-linux-android -p oneios-tool`, linked by the NDK's
`aarch64-linux-android30-clang`; needs `rustup target add aarch64-linux-android`), strips it with the NDK's
`llvm-strip --strip-all`, checks that it is a PIE for `/system/bin/linker64` with no `.symtab` and no DWARF section (`.debug_info`, `.debug_line`, ...; Rust's small allocated `.debug_gdb_scripts` stays),
and puts it in the package as `bin/oneios-tool` (it then goes through the package's elf-check and the Apple-file check
like every other file) and beside it as `out/release/oneios-tool-VERSION-android-arm64` with its `.sha256` (the same
bytes). `ONEIOS_TOOL_ANDROID=PATH` takes a binary built elsewhere; `CARGO_TARGET_DIR` is honoured. The OneOS app runs
it as root from the staged runtime (`oneios_root.sh tool`, [oneos/interface.md](../oneos/interface.md) section 9).
The first build of the Android target took 23 min on this 4-CPU PC with `-j 2` (the Mac: 26 s); later package runs
reuse cargo's build. `runtime/package/test.sh` checks that the package lists `bin/oneios-tool` with mode 0755 and that
the file beside it is the same; `.github/scripts/check-phone-tool.sh VERSION` does the same in CI and the release.

`external/virglrenderer` (`scripts/setup/get-references.sh virglrenderer`) must be there before a build that goes to
the phone: without it xnuhost and gpuctl build with the 2D GPU only and say so at run time ([releases.md](releases.md)).

`runtime/host-tests.sh` runs, and prints one line each for: `loader` (`make -C runtime/loader test`), `xnuhost`
(`make -C runtime/xnuhost test`: the unit tests, drm2kgsl and its leak test with LeakSanitizer, the image set, and
the Android xnuhost booting the test kernel), `display` (`make -C runtime/display test`), `bridges`
(`runtime/bridges/tests/host.sh`: the unit tests, `test-binder`, brightness, battery), `camera`
(`runtime/bridges/camera/tests/host.sh`), `links` (`runtime/package/elf-check-test.sh`: the link rule and the check
of the next section) and `package` (`runtime/package/test.sh`); the last two when the NDK is there.
A part that ends with exit 0 but skipped something shows the first "skipped" line. The exit code is 1 when a part
failed. Other host tests: `scripts/image/tests/run.sh`.

### Shared objects: the link rule and the NEEDED check

A shared object links with symbols left undefined; Android's linker refuses it when it is loaded. That reached the
phone on 2026-11-14: `liboneios-display.so` of `1f7dd1f` called `ANativeWindow_getBuffersDataSpace` and
`ANativeWindow_setBuffersDataSpace`, which the NDK has in `libnativewindow.so`, and linked `-landroid -lm` only. The
build passed, `app_process` said `dlopen failed: cannot locate symbol "ANativeWindow_getBuffersDataSpace" referenced
by liboneios-display.so`, and the panel stayed on the OneOS splash (`f7bb1c3` linked `-lnativewindow`). Two things
now stop such a file on the build machine:

1. **The link rule.** `runtime/host.mk` has `ANDROID_SO_LDFLAGS := -Wl,-z,defs`, and every `-shared` link line of a
   runtime Makefile carries `$(ANDROID_SO_LDFLAGS)`: a symbol that no library named on the line has is a link error
   (`ld.lld: error: undefined symbol: ...`). The runtime builds one shared object, `liboneios-display.so`
   (`runtime/display/Makefile`); a new one takes the same variable, and `runtime/package/elf-check-test.sh` fails
   when a `-shared` line has none. Executables need no flag: lld fails on an undefined symbol in an executable by
   default, a static one (xnuhost, the phone checks) and a dynamic one (`oneios-audio`, `oneios-camera`, `camctl`)
   alike. The test links one of each with a missing symbol and expects the failure.
2. **The check**, `runtime/package/elf-check.sh [--api N] PATH...` (POSIX shell; the NDK's `llvm-readelf` and
   `llvm-nm`; `PATH` is a stage or one file). For every arm64 ELF file with a dynamic section:
   - each NEEDED library must be a stub of the NDK for API 30
     (`toolchains/llvm/prebuilt/<host>/sysroot/usr/lib/aarch64-linux-android/30/*.so`: the libraries every Android
     of that level has; 30 is the level the runtime Makefiles compile for) or a library that is in the stage itself
     (by its SONAME, or by its file name when that ends in `.so`);
   - each undefined dynamic symbol must be defined by one of the file's own NEEDED libraries, a versioned reference
     (`fopen@LIBC`) with that version. Weak references are not checked (Android loads a file with one unresolved).

   A failure is one line on stderr and exit 1 (2: a wrong command line, or no NDK), for example:

   ```
   bin/liboneios-display.so: symbol ANativeWindow_getBuffersDataSpace is in none of its NEEDED libraries (NEEDED: libandroid.so, libm.so, libdl.so, libc.so); it is in libnativewindow.so
   libuser.so: NEEDED library libonefixture.so is not an API 30 stub of the NDK and not a library the runtime ships
   ```

   The end of the first line names every stub (and shipped library) that has the symbol, or says that none has.
   A good file is one line on stdout (`elf-check: bin/oneios-audio: NEEDED libaaudio.so, libdl.so, libc.so; 67
   undefined symbols, each defined by one of them`), and the last line counts the files. It runs at the end of
   `runtime/build.sh STAGE` (the build fails), in `runtime/package.sh` on the package's content (the package is
   refused, exit 3: [releases.md](releases.md)) and in the release workflow on the archive. It reads only what the
   file says of itself: it does not load anything, and a library that the phone's Android lacks although the NDK
   lists it for API 30 is not found by it.

`runtime/package/elf-check-test.sh` (the `links` part of `runtime/host-tests.sh`, and a step of CI) tests both, in a
temporary folder: the display helper's library by its own Makefile rule with `1f7dd1f`'s libraries
(`make DISPLAY_LIBS='-landroid -lm'`: the link fails on both symbols), the same without the rule's flags, which is
`1f7dd1f`'s link (it links, and the check fails with the line above), and as the Makefile links it now (both
pass); the same three with `runtime/package/elf-fault.c`, a test input that makes the same two calls and does not
depend on the display helper's source; the executables; a NEEDED library that is no stub (refused alone, accepted
when the stage ships it) and a symbol that nothing has; and a stage of `runtime/build.sh`, as built and with the
faulty library in the display helper's place. `runtime/package/test.sh` adds the package's side: the two runs of
the check in the log of a good package, and the refusal of a package with the faulty library.

Checked on 2026-11-14 on the Linux PC of the next table (NDK 28.2.13676358), in a tree without
`external/virglrenderer` (xnuhost and gpuctl 2D-only, which changes nothing for this check: both are static). Not
run on a Mac, and nothing of it has been on the phone (the phone's part is `f7bb1c3`'s library, loaded there by hand).

| Part | Result |
|---|---|
| `runtime/build.sh out/stage-links` (a new stage) | 15 files: 14 in `bin/` (13 arm64 ELF and `oneios-net.sh`) and `lib/oneios-display.dex`. The check: 4 dynamic ELF files, 9 static, 0 failures. `bin/liboneios-display.so` needs libandroid, libnativewindow, libm, libdl, libc (46 undefined symbols); `bin/oneios-audio` libaaudio, libdl, libc (67); `bin/oneios-camera` and `bin/camctl` libcamera2ndk, libmediandk, libbinder_ndk, libnativewindow, libdl, libm, libc (126 and 152) |
| `runtime/package/elf-check-test.sh` (`links`) | 13 tests passed. With `1f7dd1f`'s libraries the rule's link fails (`ld.lld: error: undefined symbol: ANativeWindow_getBuffersDataSpace`, and `_setBuffersDataSpace`); `1f7dd1f`'s own link passes and the check prints the line above for both symbols, exit 1; the library as the Makefile links it passes both |
| `runtime/package/test.sh` (`package`) | 22 tests passed (19 before: the check's two runs in a good package's log, the refusal of the faulty library with exit 3 and no package, and the symbol and `libnativewindow.so` named in it) |
| `runtime/host-tests.sh`, the other parts | loader 160 checks; xnuhost 13840, display mode 0 failed, image set 37, `check-arm64` `TESTKERNEL: all passed` and the restart test (not run: drm2kgsl and its leak test, which a tree without `external/virglrenderer` does not build, and the udmabuf import: no access to `/dev/udmabuf`, as on 2026-11-12); display 111 and mode choice 10; bridges 289, wifi 162, `test-binder` 38 and 35, brightness 74, battery 65, thermal 64, audio helper 11105 (some timing checks not checked: the box had a load of 14-18 from other builds), Android build ok; camera 131, Android build ok. 0 failed in each. The first run here failed in `check-arm64` (the test kernel's timer and task checks, under qemu-aarch64 at that load, while the suite compiled its tests beside it); `check-arm64` alone and the whole suite again passed |

Checked on 2026-11-12, Linux x86_64 CachyOS (kernel 7.3.1-1-cachyos with binderfs built in, 4 CPUs, 15 GiB; GCC
16.2.1, clang 23.1.4, GNU make 4.4.1, Python 3.14, NDK 28.2.13676358, build-tools 35.0.0, Temurin JDK 17,
qemu-aarch64 11.1.3 user mode, no binfmt), `runtime/host-tests.sh` on a clean tree, then again after the changes
below (the same counts):

| Part | Result |
|---|---|
| `runtime/build.sh out/stage-linux` (clean tree) | 15 files: 14 in `bin/` (13 arm64 ELF and `oneios-net.sh`) and `lib/oneios-display.dex`; xnuhost 4,198,048 bytes, with drm2kgsl (3.4 MB of it is `task.c`'s state `T` in `.data`: it has initialisers, so it is not `.bss`) |
| loader | 160 checks, 0 failed |
| xnuhost | 13263 checks, 0 failed (13782 on 2026-11-13 with the fixed RAM's tests, fix-fixed-memory; `check-arm64`'s second boot now runs with `ram-giveback=0`; 13885 on 2026-11-15, fix-ram-locked, the second boot with the new defaults and the clean fallback); display mode 5 scenarios, 0 failed; image set 37 checks, 0 failed; `check-arm64` under qemu-aarch64 `TESTKERNEL: all passed` and the restart test (8 restarts, then exit 3). **Not run here:** drm2kgsl, the leak test and test_xnuhost's udmabuf import ("no /dev/udmabuf here (Permission denied)"): the box's `/dev/udmabuf` is `root:kvm 0660` with an ACL for the seat's user only (`sddm`); LeakSanitizer itself works here. Giving the user access (the `kvm` group, or the seat) is a root step |
| display | 72 checks, 0 failed; mode choice 5 checks, 0 failed |
| bridges | 204 checks, 0 failed; wifi 132 checks, 0 failed; `test-binder` 38 checks (the Bluetooth HAL client) and 35 (the Wi-Fi supplicant client), 0 failed; brightness 74 checks, 0 failures; battery 57 checks, 0 failures; thermal 56 checks, 0 failed (79 on 2026-11-15, fix-ram-locked); audio helper 4051 checks, 0 failed; Android build ok |
| camera | 131 checks, 0 failed; Android build ok |
| package | 19 tests passed |
| `scripts/image/tests/run.sh` | 23 tests, OK |

Since 2026-11-13 (fix-audio-latency): bridges 281 checks (with virtio-snd's periods-in-flight check) and the audio
helper 11,101 checks (about 50 s; it prints `stage:` lines, the latency per stage, `docs/hardware.md`
section 6). Its timing checks (no underrun, the slips, the level, the latency bounds) need the host to run the test's
own threads on time: when they ran more than 4 ms late (other builds on the same CPUs) each such check is skipped
with a line `the host ran this test's threads up to N ms late: ... not checked`, and the count is lower (the run of 11,101 had 9 skipped, at a
load of 17-22 on the 4-CPU PC: the box was never idle that day).

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
3. runs `runtime/package/elf-check-test.sh` (the `links` part) and `runtime/package/test.sh` on the Mac (both need the NDK).

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
- `make -C runtime/xnuhost rpc-arm64` (fix-mach-rpc, 2026-11-15; the same runners) times the round trip of a synchronous
  Mach message between two tasks as the hosted kernel makes it (two user stays, two IPIs, two wakes of an idle vCPU),
  once per interrupt path: `RPC_MODES="0 1 2"` = ArmCpu's trapped GIC registers (the kernel before hosted 0081), host
  calls (0081), one vCPU without an IPI (the floor). Each mode is its own run, with xnuhost's `perf:` lines and
  `emulated:` counts at the stop; `RPC_SET="--set cpu-pin=2"` pins, `RPC_SCALE=2` doubles the 4,000 round trips
  (`docs/booter.md` 2.6d has the numbers). On the phone: the same command line, as the parallel test's (2.6c step 2).
  Not part of `make test`.
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
  ([kernel/patches/oneios-hosted/README.md](../kernel/patches/oneios-hosted/README.md), 0058). Since 0066 a fourth line
  splits the give-back's time (`give-back time T ms: lock waits ..., host calls ...; on a CPU U ms; per MiB ...`);
  `oneios-free-report-run=1 oneios-free-report-batch=64` gives back the way 0058 did, and
  `oneios-free-report-stress=1 oneios-free-report-hysteresis=1` makes a PC boot give back all the time (how 0066 was
  measured). xnuhost's second `memory:` line says how much of the guest's RAM the PC itself swapped out. Since fix-ram-locked (2026-11-15) the RAM is held fixed and locked by default ([booter.md](booter.md) 2.2, "Fixed RAM": prefaulted on a thread, `mlock`ed, no give-back); on this PC the `mlock` fails at the first step with ENOMEM (RLIMIT_MEMLOCK 8 MiB, not root), so nothing is prefaulted and the RAM is faulted in as iOS uses it, which the log says (`not locked (mlock: Out of memory), so not prefaulted`); free pages are still not given back. `--set ram-giveback=1` is 0058's way (free pages given back, as before 2026-11-15).
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
  boot ended, so there it costs the boot (cause not looked into; another such boot left a 470 MB `qemu_xnuhost_*.core` in the working directory, exit 131); `oneios-ubt=` is the safe way. `qemu-aarch64 -g PORT` plus gdb (`set architecture aarch64`, `target remote :PORT`) debugs the kernel.
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
- Haptics on the PC: add `--set haptics=sim`: a stand-in actuator behind the bridge channel (the PC has no
  vibrator), with the bridges off. The guest then publishes the Actuator audio device and `/product/haptics`
  (`ONEIOS HAPTIC: on: host actuator ...`, `DroidVMAudio: published ... Actuator=1`), and logs who opens it
  (`ONEIOS HAPTIC: Actuator: user client N opened by audiomxd`) and `ONEIOS HAPTIC: counters: ...` every 30 s while
  a figure moves; xnuhost logs what came over the bridge (`haptics: effect N played on the stand-in actuator`,
  `haptics: counters: ...`). VirtualAudio's own lines: `oneios-tool ioslog show out/run/data/data.img --proc
  audiomxd` (`actuator port`, `Actuator`). [hardware.md](hardware.md) section 5, "Haptics do nothing".
- Touch and keys on the PC: add `--input-script runtime/xnuhost/tools/input-demo.script` (keep `--no-input`: the
  evdev reader would open the PC's own devices). The script gives xnuhost's input device a panel's axes and, once
  the guest's driver runs it, repeats a tap, a swipe up from the bottom edge and the three side keys
  ([booter.md](booter.md) 2.8). The kernel's `oneios-hid:` lines say what attached and what arrived
  ([the HID archive's README](../kernel/patches/oneios-hosted/hid/README.md)). The script also has a pen (a hover, two
  strokes, the side button), left out unless `--set pen=auto` is added: then the guest gets the pen device
  `/arm-io/input-1` and `oneios-pen=1`, and the `pen:` and `oneios-hid: pen:` lines follow it ([hardware.md](hardware.md) 7f).
- Typing on the PC: `--input-script runtime/xnuhost/tools/keyboard-typing.script` waits 7 minutes for SpringBoard,
  then unlocks, opens Spotlight's search field (a swipe down on the Home Screen raises the keyboard) and taps 40 keys
  0.5 s apart, every 80 s or so. With `--set haptics=sim` each key is a Core Haptics transient: count
  `Initial XPC call to server timed out` in iOS's log, the `haptics: effect N played` lines and Preferences', Spotlight's
  or SpringBoard's `oneios-cpu:` ([hardware.md](hardware.md) 5, "Core Haptics' server gave up"). Since 2026-11-15 a PC
  boot has `oneios-cpu:` lines (the GPU report thread runs without a Metal accelerator too).
  An app load for memory checks: `--input-script runtime/xnuhost/tools/input-apps.script` (after 18 min, a tap on each
  place of the Home Screen's first page and the dock, 40 s for the app, then Home; again and again), the same load for a
  before and an after run ([booter.md](booter.md) 2.2, "iOS's own compressor and the memory view"). On a loaded PC give
  the first wait more (a copy with `wait 2400000`).
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
  frame of the guest's own that is not all black (never a frame of the boot console; with every frame black, `ready-content-wait` seconds, 120, after the first: [booter.md](booter.md) 2.8, "the ready rule"); the log's `display: the guest's first frame of its own: all black` line is where the rule before 2026-11-15 was ready. Boot argument `oneios-display=0` leaves the display out. The frame is
  1440x3120 at 120 Hz and @3x unless told otherwise: `--width W --height H`, `--hz N`, `--set scale=2`
  ([booter.md](booter.md) 2.8); the log's `display: shm, guest frame ...` line and the capture's `hello` say what it is.
  The frame's pixels: `display: the frame's pixels: B,G,R,X 8-bit sRGB, 4 bytes a pixel` (the default);
  `--set display-format=rgba16f` makes them half floats and passes the guest `oneios-display-wide=1`, which
  `display-capture.py` cannot read (it takes format 1 only). What iOS's render server swapped and what the display
  service publishes: the kernel's `oneios-display: the compositor swaps format ...` and `oneios-display: properties:`
  lines (`kernel/patches/oneios-hosted/display/README.md`, "Colour, depth and the panel"). `--set
  identity=out/kernel-inputs/24B156/devicetree.bin` gives the guest the iPhone's own `/product` ([booter.md](booter.md) 1).
- Metal's kernel side: add `oneios-gpu-metal=1 oneios-adreno-selftest=1` to the boot arguments (the accelerator is
  not published on a host without 3D unless asked). About ten seconds in: `droidvm-gpu: Metal accelerator stage 5
  status 0` and two `oneios-adreno: selftest:` lines (IOKit's match, the plug-in's open and one call;
  [gpu.md](gpu.md) 5.1). No 3D call can run here: qemu has no KGSL.
- Typing on the PC: `--input-script runtime/xnuhost/tools/input-typing.script` (keep `--no-input`) goes Home after
  18 minutes, opens Search with a swipe down (its keyboard comes up) and taps 100 keys in 70 s, again and again. Who the
  kernel's time goes to while it types: the `oneios-top:` lines every oneios-perf period (hosted `0080`, `oneios-top=N`,
  default 6): per process its CPU (`cpu C%`, which the PC's log otherwise lacks), its traps, faults to vm_fault, fills,
  host unmaps and most used calls, and its vm_protects of the shared region; `oneios-srprot:` lines name the first 24
  shared-region protects with where they came from (`oneios-tool sym`).
- A user process that neither dies nor does its job: boot arguments `oneios-ubt=NAME[,NAME...]`
  (`oneios-ubt-at=SECONDS`, default 420; `oneios-ubt-every=SECONDS` to repeat; `oneios-ubt-count=N` to stop after N
  reports, each headed `oneios-ubt: report K at +S s`, `0061`) make the kernel print where every
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

The app's checkout is the submodule `app/` (section 7): clone with `git clone --recurse-submodules`, and
`git submodule update --remote app` brings it to the head of the fork's branch `oneios` before an `app` or `cycle`
build.

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
$T phone log --list                # the logs by name: xnuhost, switch, return, outcome, display, watchdog, usb, logs/xnuhost-<epoch>.log
$T phone log --file usb            # the session's USB watcher: the gadget and adbd, its repairs (oneos/interface.md 6a)
$T phone log --grep 'xnuhost I usb: ' # xnuhost's own "usb: state ..., config ..., UDC ..." lines (docs/booter.md 2.9a)
$T phone status --watch            # the status again every 5 s
$T phone return                    # session/return.request, then waits for "One UI restored in place"
# only during a session (refused without one: these are One UI's then); xnuhost and iOS are not touched:
$T phone display-restart           # the display helper again: ends the one control/display.pid names (SIGTERM, then SIGKILL),
                                   #   starts it with the switch script's own command, waits for "first frame posted"
$T phone display-restart --lib runtime/display/build/android/liboneios-display.so --dex runtime/display/build/android/oneios-display.dex
                                   #   first a new library and/or dex into the staged runtime (sha256 printed for both sides,
                                   #   refused when the phone's copy differs; runtime.sha256 and runtime.ids follow)
$T phone touch status              # /sys/class/sec/tsp/enabled: 1 on, 0 off
$T phone touch wake                # 2,0 then 2,1 to it (the touch IC sleeps after iOS's brightness was 0), then reads it back
$T phone backlight status          # the panel's level, max_brightness, and One UI's own value (control/base/backlight)
$T phone backlight set 5350        # 1..12750; the return writes One UI's own value back
$T phone cpu                       # each CPU's core, domain, clocks and governor; the cpusets; xnuhost's and iOS's threads (read only)
$T phone cpu --trace 30 --out cpu.pftrace   # also a 30 s Perfetto trace (sched, freq, idle), copied here
$T phone pull-data OUT --ioslog    # the Data store's copy (no session open), then ioslog boots, checks and health
                                   #   (the health report: ioslog-health.txt beside OUT, its lines printed). In chunks
                                   #   (--chunk 256 MiB), each checked against the phone's sha256 and tried again when its
                                   #   stream breaks (--retries 5); after a stop, the same command again goes on (OUT.part,
                                   #   OUT.part.progress; --restart begins again; --verify end: the sums after the last chunk)
#   one stream by hand (no resume, no sums): adb exec-out "su -c '/data/adb/ksu/bin/busybox gzip -1 -c /data/local/tmp/oneios/data/data.img'" | $T phone sparse-write OUT --gunzip
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
| `build` | Only what changed. The hosted kernel (`scripts/bg.sh cycle-kernel env HOSTED_OUT=NAME XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh`) when `--kernel-out NAME` is given and `xnu.macho` is older than the newest file of `kernel/patches/oneios-hosted` (or missing). The collection (`kernel/prelink/build.sh` with `KERNEL=` and `REFERENCE=` of that tree) when the kernel is newer than `out/kernel/24A446/kernel.kc`; **the cycle stops unless the build ends with status 0 (the audit: 0 failures) and `oneios-tool prelink stubs-check` says `0 bad`**. The runtime (`runtime/package.sh V`, unpacked into `out/cycle/runtime-V`) with `--runtime-version V`; `--runtime DIR` takes one that is built. The image parts (`kernel.kc`, `kernel.kc.hashes`; not `manifest.json` or `xnuhost.conf`: `out/image/24A446` is the Mac's own set, not the phone's) are gathered in `out/cycle/image`; `--image DIR` takes a folder instead, which is also where iOS plug-ins built elsewhere go. The iOS plug-ins build on macOS only (`--force plugins`). The app: its source's version and its checkout are said (the checkout `oneios-tool app` finds: section 7; `app/` of this checkout unless one is named with `--oneos DIR` or `ONEOS_DIR`); `--force app` runs `app build` and `app install` on it, and ends the cycle when there is no checkout (an empty `app/`: `git submodule update --init --remote app`). `--force kernel,prelink,...` builds a part whatever its state. Long builds run in tmux and the cycle waits for their exit code |
| `tests` | `--tests`: `runtime/host-tests.sh`; a failure stops the cycle before the phone is touched |
| `stage` | `phone sums`, then `stage image` with only the files whose SHA-256 differs from the phone's and `stage runtime` when a file of it differs, then `phone sums` again, which must equal the local hashes (`sums.txt`). Files staged with no `manifest.json` of their own get one made from the phone's (`phone manifest`); then `phone image-check`. `data.seed` is never staged |
| `switch` | First the way back: root, no session, `oneios_return.sh`, `oneios_switch.sh` and `bin/oneios_root.sh` staged, no import in progress (`runtime.new`, `image.new`), and the root script's preflight says `ready 1`. Then `phone switch` |
| `watch` | Reads `session/xnuhost.log`'s new lines every 2 s, and after a switch only once the file changed at or after the phone's second of the switch (until the switch replaces it, about 40 s in, it is the last session's: its first-frame line made the watch say "ready after 0 s", 2026-11-12) and prints each marker once with its time: xnuhost, kernel, darwin, bsd-root, init, launchd, backboardd, SpringBoard, ready (the first frame), PANIC, end. On a panic, when the session ends by itself, or after `--ready-timeout` (300 s) without the first frame: straight to the return, and the cycle's status is 1 |
| `hold` | `--hold S` seconds, else until Ctrl-C. Ctrl-C returns, unless `--stay`, which leaves the session open (end it with `phone return` or `cycle --only return,collect`). A panic or the session's own end during the hold returns |
| `return` | `session/return.request`, then waits (180 s) for `One UI restored` in what `return.log` gained |
| `collect` | `out/session-logs/DATE/NAME/`: `xnuhost.log`, the last 2000 lines of `switch.log` and `return.log`, `return.outcome`, `sums.txt`, and `summary.md` (versions, hashes, each step's time and result, the markers with their times, the panics, the way back). `--pull-data` adds the Data store's copy and `ioslog` (boots, checks, health). **`ioslog-health.txt`** (iOS's own counts of the last boot: hangs, watchdog kills, XPC timeouts, jetsam kills, crashes, stuck idle demotions, encode failures; `oneios-tool ioslog health`, section 10) is written from the folder's `data.img` (this run's `--pull-data`, or an earlier run's of the same `--name`), else from an `ioslog.txt` in the folder (iOS's log as text, `ioslog show ... -o`); its head and one line per kind are printed and go into `summary.md`; a folder with neither gets a line that says so. The summary's result line names every step its Steps table has as failed (`every step that ran ended well` only when none did), and a failed `collect` ends the cycle with status 1. A run that did not watch (`--only return,collect` after `--only stage,switch,watch` with the same `--name`) reads the markers from the saved `xnuhost.log` (this folder's copy, else the phone's `session/xnuhost.log`) and says so |

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
   `out/image/24B156`: it differs from the phone's, and a new seed makes the Data store start over.
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

   **The display helper, the touch IC and the panel's level during a session** have commands (`oneios-tool phone
   display-restart`, `phone touch wake`, `phone backlight set N`; `tools/oneios-tool/README.md`, "The session's own
   commands"). In the session of 2026-11-14 they did not exist yet and the three were done with adb. The forms by
   hand, as the fallback (what the commands send; only named files, as root, only while a session runs, never under
   `/sys/power`):

   ```sh
   # the display helper again: the switch script's own command, and its pid where the return looks for it
   adb shell "su -c '/data/adb/ksu/bin/busybox setsid /data/local/tmp/oneios/runtime/bin/oneios-display --session /data/local/tmp/oneios/session --takeover --wait 60 </dev/null >>/data/local/tmp/oneios/control/display.log 2>&1 & echo \$! > /data/local/tmp/oneios/control/display.pid'"
   adb shell "su -c 'tail -n 12 /data/local/tmp/oneios/control/display.log'"   # oneios-display: layer up, first frame posted
   # the touch IC, asleep after iOS's brightness was 0 (the node reads 0): what One UI's input HAL writes at panel on
   adb shell "su -c 'echo 2,0 > /sys/class/sec/tsp/enabled; echo 2,1 > /sys/class/sec/tsp/enabled; cat /sys/class/sec/tsp/enabled'"   # 1: on
   # the panel's level, 1..12750 (max_brightness beside it); the return writes One UI's own value back
   adb shell "su -c 'echo 5350 > /sys/class/backlight/panel0-backlight/brightness'"
   ```

   The command also ends the old helper first (the pid in `control/display.pid`, only when its
   `/proc/PID/cmdline` is a display helper's: SIGTERM, then SIGKILL), sets the new one's OOM score to 0 and puts it
   in the cgroup `oneios_session`, as the switch does; by hand, an older helper leaves by itself when the new one
   connects (xnuhost closes its socket).
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
scripts/serve/serve-image.sh out/image/OneiOS_iOS27.2.1_24B156_iPhone18-2.tar.zst 4   # 4 tunnels; prints the URLs
# on the phone (Termux, pkg install aria2): aria2c -x 16 -s 16 -k 8M -o <name> <url1> <url2> <url3> <url4>
scripts/serve/serve-image.sh --stop                                                    # when done
```

URLs are also in `logs/serve-urls.txt`; each tunnel's log is `logs/serve-tN.log`. Check the download against the
`.sha256` (same URL plus `.sha256`), then in the app: iOS 27 (OneiOS) → I have the file → the download.

## 7. The OneOS fork app

### The checkout: the submodule `app/`

The fork (`https://github.com/akshaynexus/OneOS`, branch `oneios`) is the submodule `app/` of this repository
(`.gitmodules`: `branch = oneios`). Clone both at once, or add it to a checkout you have:

```sh
git clone --recurse-submodules https://github.com/akshaynexus/OneiOS     # app/ at the commit OneiOS recorded
git submodule update --init app            # an existing checkout: the same
git submodule update --remote app          # app/ at the head of the fork's branch oneios (what CI builds)
git -C app switch oneios                   # to work on the fork in app/: a branch instead of a detached head
```

`--remote` moves only your `app/`; `git status` then shows `app` as modified. The commit OneiOS records for
`app/` moves forward only in a commit made to pin it (`git commit -m "app: pin the fork at <sha>" --
app`). CI does not use the recorded commit: every build takes the head of `oneios` (`git submodule update --init
--remote app`, [releases.md](releases.md) "CI and releases"). Nothing of OneiOS walks into `app/`:
`git ls-files` (the CI checks, the host-test loop, `check-tracked-files.sh`) lists it as one entry, the runtime
package takes `runtime/`, `kernel/` and `LICENSE` only, and the runtime job never checks it out.

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

The checkout is the first of: `--oneos DIR`; `ONEOS_DIR`; the current folder when it is a checkout; `app/` of the
OneiOS checkout (the submodule: the default); `app/` of the OneiOS main checkout when the command runs in a worktree;
`OneOS` beside the OneiOS main checkout (the clone from before the submodule). A checkout named by hand comes before
`app/`, and a named folder that is no checkout is an error, not a reason to take `app/`. A checkout is a folder with
`app-android/app/build.gradle.kts` and `switcher/assets/oneios_switch.sh`, whatever its name. An empty `app/` (the
submodule is not initialised) is not one: the tool says so with the command, `git submodule update --init --remote app`,
and looks further; with nothing found it ends with status 1 and lists the places. Each command's first line says
which checkout and how it was found; `$T app dir` prints the path alone. `cycle` (section 6) uses the same lookup.
By hand, and what the tool runs:

Repo `akshaynexus/OneOS`, branch `oneios`. Every OneiOS pull request, push to `main` and release builds the APK
from the head of that branch (`./gradlew test assembleFullDebug`, and `./gradlew test` on macOS; the job `apk` of
`.github/workflows/release.yml`, [releases.md](releases.md)). A push to `oneios` itself runs the fork's own
`.github/workflows/oneios-apk.yml`: it builds the full debug APK (the workflow's own `GITHUB_TOKEN` reads the SESL
packages), runs the unit tests and publishes the prerelease `oneios-<version>` on the fork (0.4.75 is the first
with the Hardware checks screen). Locally: a `read:packages` token in `~/.config/twidget/github.properties`
(`ghUsername`, `ghAccessToken`), then `cd app-android && ./gradlew test assembleFullDebug`.

**oneios-tool in the app (fork branch `app-oneios-tool`, PR akshaynexus/OneOS#10; not run on the phone yet).** The app
runs the staged runtime's `bin/oneios-tool serve` as root, one process per flow (`oneios_root.sh tool work|read`, the
fork's `docs/oneios.md` "oneios-tool in the app"): **Make iOS image** (Set up iOS 27's image step, and Tools on iOS
27's page: the tool's `fetch` of the IPSW's members, then `make-image` and `tweaks`, which the tool still refuses),
**Check every file** (Tools), **CPU, clocks and threads** (Hardware checks), and in Session logs **Analyse the last
session**, **iOS's own log: boots** and **last boot**. The screen shows the tool's steps, progress, time left, space,
pauses and refusals as the tool sends them (`tools/oneios-tool/README.md`, "serve"), with **Pause**/**Resume** when the
tool takes them, "No network" while Android has none (the app then pauses the request and resumes it when the
network is back), and "Resume: <flow>" after OneOS was ended in the middle of a flow. On a PC the tests run the app's
side against a stand-in `serve` and the real one (`OneiosToolSessionTest`, `OneiosToolScriptTest`,
`OneiosToolRealTest`).

**The phone test plan (for the Mac, which has the phone; nothing of it has run yet).** One UI up, KernelSU granting
root to OneOS, no iOS session. Save each step's screen (`adb exec-out screencap -p > step.png`) and what it says.

1. *The APK and the runtime.* The APK of the fork's branch, from CI (the `oneos-apk` artifact of the draft that points
   `app/` at it) or built here: `oneios-tool app build && oneios-tool app install` (same signing key, or the tool
   refuses; never uninstall). The runtime of this branch with `bin/oneios-tool` and `lib/profiles/`:
   `runtime/package.sh 0.1.10-dev`, then in the app iOS 27 → Set up iOS 27 → Get the runtime → I have the file
   (the `.tar.zst` pushed with `adb push out/release/oneios-runtime-0.1.10-dev-arm64.tar.zst /sdcard/Download/`).
   Expect: the runtime step done, "every file checked". `oneios-tool phone sums` lists `bin/oneios-tool`.
2. *The notification permission.* Opening Set up iOS 27 the first time asks for notifications (Android 13+): allow it.
3. *Check every file* (iOS 27 → Tools). Expect: Starting oneios-tool, then two `sums` requests, each with its steps
   (Reading the list, Checking the files), progress and time left, and at the end Done with "N files, ... all as
   listed". With no image set staged the second is refused `not-found` and the screen says so in the tool's words.
   While it runs: `adb shell su -c 'ps -A -o PID,NI,CMD | grep oneios-tool'` shows one process at NI 10; after Done,
   none.
4. *CPU* (Hardware checks → With oneios-tool → CPU, clocks and threads): the tool's lines and its eight `cpu` rows.
5. *Session logs* → With oneios-tool: Analyse the last session (`log-list`, then the xnuhost log's last 4000 lines),
   iOS's own log: boots and last boot (`ioslog-*` on `data/data.img`; refused `not-found` before a first session).
6. *Make iOS image* (Tools → Make iOS image; about 10.6 GiB from Apple's server into `/data/local/tmp/oneios/make/
   inputs`: check `df -h /data` first). Expect: `fetch`'s steps (Reading the IPSW's directory, Downloading and checking
   the members), its `space` (need and free), progress with speed and time left.
   - *Pause, then Resume* after a minute: the progress stops and the line says Paused (or the tool's words); Resume
     goes on from the same byte count, not from 0. (Needs the tool of `tool-download-resume`; a tool without `pause`
     shows no Pause button.)
   - *Airplane mode* on in the middle: the line says No network (and the tool's `net` words with its next try);
     airplane mode off: it goes on by itself, without Resume. Airplane on, Pause, airplane off: it stays paused until
     Resume.
   - *OneOS ended in the middle*: `adb shell am force-stop com.fahadali.oneos` while it downloads. Expect: within
     10 s no `oneios-tool` process (`ps` as above; the tool ends at the end of its input, or root's `untool` ends
     it), the `.part` files kept under `make/inputs`. Open OneOS → iOS 27 → Tools → Make iOS image: the screen offers
     "Resume: Make iOS image"; tapped, `fetch` says the members already done are "already complete" and the rest go
     on from their parts.
   - At the end of `fetch`: "Not done, as the tool says" with `make-image: unknown: ...` and `tweaks: not-ported:
     The root tweaks are not in this tool yet ...`. Nothing is staged (`image/` unchanged).
7. *Stop*: start Check every file, Stop: "Stopped: what it made for this was removed", no `oneios-tool` process.
8. *No runtime*: Remove the iOS files (Manage), then Tools' rows are gone; Set up iOS 27 → Make iOS image says
   "oneios-tool did not start" with root's reason; the app does not crash.
9. Afterwards: `adb shell su -c 'rm -rf /data/local/tmp/oneios/make'` frees the 10.6 GiB (Apple's files: never copy
   them off the phone into git or a release).

The times of each request on the phone, without the app (`--request`: standard input's end would cancel a request),
as root with the runtime of this branch staged. The requests go in a small script that is pushed, so that no shell
between the Mac and the tool takes the JSON's quotes (the CLI has no command for this yet):

```sh
cat > /tmp/oneios-measure.sh <<'EOF'
S=/data/local/tmp/oneios; T=$S/runtime/bin/oneios-tool
for r in \
  '{"id":"1","cmd":"sums","dir":"/data/local/tmp/oneios/runtime","list":"/data/local/tmp/oneios/runtime.sha256","strict":true}' \
  '{"id":"2","cmd":"sums","dir":"/data/local/tmp/oneios/image","list":"/data/local/tmp/oneios/image.sha256","strict":true}' \
  '{"id":"3","cmd":"cpu"}' \
  '{"id":"4","cmd":"log-list"}' \
  '{"id":"5","cmd":"log","file":"xnuhost","tail":4000}' \
  '{"id":"6","cmd":"ioslog-boots","source":"/data/local/tmp/oneios/data/data.img"}' \
  '{"id":"7","cmd":"fetch","profile":"/data/local/tmp/oneios/runtime/lib/profiles/ios-27.2.1-iPhone18-2.toml","indir":"/data/local/tmp/oneios/make/inputs","only":["trust-system"],"resume":true}'; do
  echo "== $r"; time "$T" serve --stage "$S" --request "$r" | grep -E '"ev":"(ok|refused|failed)"'
done
EOF
adb push /tmp/oneios-measure.sh /data/local/tmp/oneios-measure.sh
adb shell su -c 'sh /data/local/tmp/oneios-measure.sh' 2>&1 | tee oneios-measure.txt
adb shell su -c 'rm /data/local/tmp/oneios-measure.sh'
```

Request 7 fetches one member of 3,191 bytes from Apple's server (on the Linux PC: 0.5 s, and "already complete" the
second time).

## 8. Releases

A tag `vN.N.N` on `main` (or a dispatch with a version) runs `.github/workflows/release.yml`, which builds the
runtime package with the pinned NDK and the fork's APK (submodule `app/`, the head of `oneios`) on two runners at
once, and attaches both with their `.sha256` to the release. No kernel, no image, no package set. See
[releases.md](releases.md).
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

`--tbd` reads `out/image/24B156/systemos.img`. Before the image set exists, give it the decrypted SystemOS
image of the fetch: `scripts/setup/ios-toolchain.sh --tbd --image inputs/24B156/141-10617-235.dmg`.

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
workflow of section 6: status, stage, sums, switch, return, log, the Data store's copy, and in a session the display
helper's restart, the touch IC's wake and the panel's level), `app` (the fork's app of
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
the image with `oneios-tool image apfs-copy out/image/24B156/systemos.img System/Library/Caches/com.apple.dyld DIR`
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
| `kernel/build-hosted.sh`, `kernel/build-linux.sh`, `kernel/build-macos.sh` | none: Rust only since 2026-11-13 (`ONEIOS_TOOL=PATH` for another binary) | the board archives (`storage`, `board`, `aes`, `keystore`, `net`, `hid`, `audio`, `gpu`, `display`) are built by `oneios-tool kernel gen NAME`, which UniXNU's driver runs from `board.toml`, and the VM kernel's build record by `oneios-tool kernel build-record`; the Python generators were deleted after every file they write was compared byte for byte, and their outputs are frozen in `tests/kernelgen/golden/` (README "kernel"); since 2026-11-13 `kernel gen gpu` compiles a staged `DroidVMVirtioGpu3D.cpp` with the GPU report's blob and surface-trail hooks (`staged/gpu.rs` `GPU3D`) |
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
oneios-tool ioslog health data.img [--detail] [-o FILE]             # the last boot's hangs, watchdog kills, XPC timeouts, jetsam
                                                                    #   kills, crashes, stuck idle demotions, encode failures:
                                                                    #   one line per kind (count, total, longest, top processes)
oneios-tool ioslog health                                           # on the phone itself, as root: the staged data/data.img
oneios-tool ioslog save data.img out/session.logarchive             # the .logarchive the manual steps made
oneios-tool image apfs-checkpoints data.img                         # the store's checkpoints: which pass the checks, and why the others do not
```

`ioslog health` gives the numbers that tell lag and heat apart, the same way for every session (the owner's request,
2026-11-15): HangTracer's `Hang detected: N s` per process (count, total seconds, the longest, how many over 1 s),
watchdog kills, XPC timeouts per client and service, the kernel's jetsam kills, crashes (the kernel's corpse lines),
memorystatus' `timed out stuck process`, and `CMPhotoCompressionSessionAddImage ... err =`. A count is a number of
log lines. `cycle`'s `collect` and `phone pull-data --ioslog` write the report with its details to
`ioslog-health.txt` and print its lines (section 6). The kinds are one table in
`tools/oneios-tool/crates/oneios-ioslog/src/health.rs` (`KINDS`): a new kind is a new row. The command, its output,
the kinds' texts, what was tested and what was not: [tools/oneios-tool/README.md](../tools/oneios-tool/README.md)
"health".

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

**On the phone itself: `oneios-tool serve` (2026-11-15).** The OneOS app runs the iOS steps on the phone through one
process it starts as root once per flow: a request per line on standard input, JSON lines back (progress with
bytes, percent and ETA, the room needed, a refusal's reason in plain words). One request runs at a time, the others
wait; a request is stopped by a `cancel` request, by the end of the app's pipe, by SIGTERM or by the idle timeout,
and a stop removes what the request wrote. The work runs in a bounded number of threads at nice 10 to 19, never in
a realtime class, and pauses or stops at the case's and the battery's limits (the thermal bridge's named files).
The protocol, every command and what is not in it yet (the root tweaks, the image build's own script, the AEA
decryption): [tools/oneios-tool/README.md](../tools/oneios-tool/README.md) "serve". The same requests run from a
command line (a PC, a test, a measurement on the phone):

```sh
oneios-tool serve --stage DIR --request '{"id":"1","cmd":"sums","dir":"DIR/image"}'     # the stage's SHA-256 check
oneios-tool serve --stage DIR --request '{"id":"1","cmd":"apfs-copy","image":"DIR/image/os.img","src":"/usr","dst":"DIR/work/usr"}'
oneios-tool serve --stage DIR --request '{"id":"1","cmd":"cpu"}' --request '{"id":"2","cmd":"log","markers":true}'   # phone cpu, phone log: the local forms
oneios-tool --jobs 4 --nice 15 image apfs-copy IMAGE SRC DST      # every command that runs alone: its threads and its nice value
oneios-tool prelink build --kernel K --kernelcache C --profile P --reviewed F --pairs F --stubs F --xnuprelink DIR --out DIR
                                                                  # kernel/prelink/build.sh's steps in one process (the phone has no bash)
tools/oneios-tool/tests/prelink/compare-build.sh                  # KERNEL=...: `prelink build` and serve's `prelink` against build.sh
adb shell su -c 'sh /data/local/tmp/serve-on-phone.sh'            # the measurement on the phone (tests/phone/serve-on-phone.sh; not run yet)
```

For Android arm64 it builds with the NDK (README "For the phone itself": 14.6 MB stripped). **Nothing of it has run
on the phone**: tested on stand-in inputs (21 tests that start the process, 12 checks on the stand-in phone) and,
on the Linux PC, on the real image set and kernel. There, with 4 CPUs: the APFS copy of 486 MB onto tmpfs 1.39 s
with one thread and 0.59 s with four (the same tree); the stage's SHA-256 check of eight 150 MB files 5.39 s and
1.53 s (the sums are `sha256sum`'s); `prelink build` 11.65 s where `build.sh` takes 13.71 s (the same `kernel.kc`,
`ffbe95f7...`, and the same 24 files). The disk and one large file's SHA-256 bound the rest (README "Measured on
the Linux PC").
