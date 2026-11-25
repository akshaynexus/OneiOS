# Releases

Owner: release-kernel. Scripts: `runtime/package.sh`, `runtime/package/`, `.github/workflows/release.yml`,
`.github/scripts/`, `kernel/publish.sh`.

## What a release is

Up to two assets and their checksums on a GitHub release of `akshaynexus/OneiOS`, from one run of
`.github/workflows/release.yml` (below, "CI and releases"):

```
oneios-runtime-<version>-arm64.tar.zst             the runtime package (this repository's code)
oneios-runtime-<version>-arm64.tar.zst.sha256      one sha256sum line: "<64 hex>  <file name>"
oneios-tool-<version>-android-arm64                the package's bin/oneios-tool by itself, with the runtime (from
oneios-tool-<version>-android-arm64.sha256         the release after v0.1.9; for a developer's adb push)
OneOS-oneios-<versionName>-debug.apk               the OneOS app: the fork (submodule app/) at the head of its branch
OneOS-oneios-<versionName>-debug.apk.sha256        one sha256sum line
```

A tag push releases both. A dispatch can release the runtime only or the APK only; a release without the runtime is
never marked latest, because the app's runtime download reads `releases/latest` and refuses a release that has no
`oneios-runtime-*-arm64.tar.zst` (the fork's `OneiosReleases.parse`). The APK's names match none of the app's
runtime patterns, so the app ignores them. The release notes name the OneiOS commit, the fork commit, the APK's
versionName and versionCode, its SHA-256, the key it was signed with and the run that built it.

The runtime package:

The format is the one the OneOS fork reads (`oneos/interface.md` section 1): a zstd frame (level 19, 8 MiB
window) around a ustar archive of regular files, `runtime.json` first, no top folder.

| In the package | What |
|---|---|
| `runtime.json` | manifest, below |
| `bin/xnuhost`, other `bin/*` | the booter and its helpers, arm64 Android, built from the Rust workspace `runtime/Cargo.toml` by `runtime/build.sh` (cargo, linked by NDK 28.2.13676358's clang; [runtime/RUST.md](../runtime/RUST.md)). Without that script: a placeholder (`runtime/package/placeholder-xnuhost.rs`, built by rustc alone, 1104 bytes; the C one was 968) that prints `xnuhost: not built yet ...` and exits 2 |
| `bin/gpuctl`, `bin/bridgectl`, `bin/wifictl`, `bin/brightctl`, `bin/batteryctl`, `bin/thermalctl`, `bin/displayctl`, `bin/camctl`, `bin/encctl` | the phone checks, each a tool of its own that runs as root with One UI up (`docs/gpu.md`, `runtime/bridges/README.md`, `runtime/bridges/brightness/README.md`, `runtime/bridges/battery/README.md`, `runtime/bridges/thermal/README.md`, `runtime/display/README.md`, `runtime/bridges/camera/README.md`, `runtime/bridges/encoder/README.md`). The fork's **Hardware checks** screen runs them from the staged runtime (`oneios_root.sh check <id>`, the fork's `docs/oneios.md`) |
| `bin/oneios-tool` | the Rust tools (`tools/oneios-tool`) for `aarch64-linux-android`, linked for API 30 by the NDK's clang and stripped (`runtime/package.sh` step 1b, [building.md](building.md) section 4). The OneOS app runs it as root from the staged runtime, one `oneios-tool serve` per flow (`oneios_root.sh tool`, [../oneos/interface.md](../oneos/interface.md) section 9) |
| `lib/profiles/*.toml` | this repository's IPSW profiles (`profiles/`: the members' names, sizes and hashes, no Apple file), and its host records (`profiles/host-*.toml`: what is known of each host phone, read by no code; the S24 Ultra's names every model number, the SM-S928W too), for the phone's `oneios-tool serve` `fetch`: the OneOS app's Make iOS image fetches the IPSW's members with `$STAGE/runtime/lib/profiles/ios-27.0.1-iPhone18-2.toml`, and on a Galaxy A56 with `ios-16.5-a56.toml` (iOS 16.5 20F66, Apple's pins since 2026-11-23; docs/a56.md; `runtime/package/test.sh` checks it is packed) (a package path has no comma; `runtime/package.sh` step 1c) |
| `scripts/*` | `runtime/scripts/*`, when there are any |
| `kernel/xnu.macho`, `kernel/xnu.macho.build.json` | the XNU that `kernel/` builds from Apple's open source, and its build record. Only when a kernel was built |
| `lib/guest/NativeAdrenoMetal.bundle/*`, `lib/guest/NativeAdrenoMetal.build.json`, `lib/guest/camera/H9ISP.mediacapture`, `lib/guest/NOTICE` | optional guest plug-ins. `scripts/release/release.sh` builds the Metal and capture plug-ins from pinned sources and passes them to `runtime/package.sh`; the Actions workflow `metal-plugin.yml` builds the Metal plug-in only and uploads a separate artifact. A package includes them only when `METAL_DIR` and `CAMERA_DIR` are passed. Both plug-ins are signed ad hoc; no Apple code (the check below). The phone's Make iOS image seeds a matching Metal bundle ([image-format.md](image-format.md), "The Metal plug-in of an image made on the phone"). `lib/guest/NOTICE` lists their source licences. About 36 MB unpacked |
| `NOTICE`, `LICENSE` | licences; with a kernel, the APSL notice (`kernel/NOTICE.kernel`) |

## What is never in a release

Nothing of Apple's: no IPSW, no kernelcache, no cryptex or disk image, no trust cache, no kext, and no kernel
collection (the source-built XNU prelinked with Apple's kexts). Those are the **image set**, which each owner
builds on their own machine from Apple's restore files (`scripts/image/`), and which the app imports from a file.

`oneios-tool package` (`tools/oneios-tool/crates/oneios-package/src/pack.rs`; it was `runtime/package/pack.py` until
2026-11-13, and refuses exactly what that script refused: 91 frozen cases, `tools/oneios-tool/tests/packagepy`)
enforces this on every package, and `runtime/package/test.sh` tests it:

- by name: `kernelcache*`, `*cryptex*`, `.ipsw`, `.dmg`, `.aea`, `.img`, `.im4p`, `.img4`, `.kc`, `.trustcache`,
  `BuildManifest.plist` and the rest of `.gitignore`'s firmware names;
- by content: IMG4/IM4P containers, AEA archives, disk images, dyld caches, and **every Mach-O**;
- the one Mach-O allowed is our own XNU: an arm64 `MH_EXECUTE` with a build record next to it
  (`<name>.build.json`) written by `oneios-tool kernel build-record`: `builder` is `oneios-kernel`, `sha256` is the file's,
  `prelinked` and `kernelcache_kexts_added` are false, `apple_binary_inputs` is empty. A kernel collection
  (`MH_FILESET`) is refused with or without a record.
- the other Mach-Os allowed are under `lib/guest/` only: our iOS plug-ins. Each must be a thin arm64 `MH_DYLIB` or
  `MH_BUNDLE` (an executable, a kext, a kernel collection or a fat file is refused), signed ad hoc for iOS (Apple's
  own code is never ad hoc signed) with an identifier that is not `com.apple.*`; a file of `NAME.bundle` must have the
  CDHash that `NAME.build.json` beside the bundle lists for it. The Metal bundle goes whole (its `oneios-target`
  and a record of the same target), the capture plug-in only with it. On the bundle the laptop built on
  2026-11-24 (main c62f974) and on the capture plug-in the check passes; `oneios-package`'s tests and
  `runtime/package/test.sh` part 5 refuse a `com.apple.*` library that is not ad hoc, an executable, an unsigned
  library, a bundle file whose CDHash is not its record's, and a bundle without its record or target.

A refusal ends with exit code 3 and no package. CI also refuses firmware-like names, Mach-O files and files over
5 MiB in git (`.github/scripts/check-tracked-files.sh`), and a package with a member outside `runtime.json`,
`NOTICE`, `LICENSE`, `bin/`, `lib/`, `scripts/` and `kernel/xnu.macho` with its record (the runtime job, which never
checks out the fork's tree `app/`).

**The APK** carries no package set, no system image and no firmware: nothing of Apple's, Samsung's, Google's or
OPPO's. The build packs the Android package sets only with `-Poneos.packSets=true`, which CI never passes; the app
downloads them by SHA-256, and the iOS image set is imported by its owner. The APK keeps its files
content-addressed (`assets/oneos/files/<sha256>`) and names them in its own lists (`assets/oneos/sets/<set>.sha256`,
and `scripts.sha256` for the session scripts). `.github/scripts/check-apk.sh APK app/app-android/packages` (the apk
job, before the upload) unpacks every entry of the APK, prints its `assets/` with their sizes and, for a
content-addressed file, the name its hash has in those lists, and refuses it (exit 3) when an entry:

- has the SHA-256 of a package set's file (the fork's `app-android/packages/<set>.sha256` lists, 373 files on
  2026-11-15; `OneOSGuest.apk`, the fork's own in-guest app, excepted);
- has, itself or by its hash in the APK's lists, a firmware, image or OTA name: Apple's (as above), `.img`,
  `.simg`, `.erofs`, `.ext4`, `payload.bin`, `.new.dat`, Samsung's `.tar.md5`, `.pit`, `.lz4`, `sboot*.bin`, OPPO's
  `.ozip`, `.ofp`, `.ops`, an image's `.apex`, `.odex`, `.vdex`, `.oat`, or a `.tar`;
- is a Mach-O, an IMG4 container, an AEA archive, a dyld cache or an Android sparse image, or from 1 MiB an ext4,
  erofs or APFS volume or a `.dmg`. A `CAFEBABE` file is a fat Mach-O only when its next word is under 45; a Java
  class has its version there (kotlinx-coroutines' `DebugProbesKt.bin` at the APK's top, which the first run
  refused, is one);
- is over 64 MiB (the largest real entry, the debug `guest_manager.apk`, was 45,548,812 bytes in the first run;
  the first version of the check had 32 MiB and refused it).

Tested on stand-in APKs (2026-11-15, Linux): one made of the fork's `switcher/assets` and root scripts passes, and
so do a Java class, a 45 MB file and a file with `OneOSGuest.apk`'s listed hash; one each with a `system.img`, a
sparse image, a Mach-O, a fat Mach-O, an AEA file, an Odin `.tar.md5`, an `.ozip`, an ext4 volume, a 70 MB file, a
file whose hash a set list names, and a file that the APK's own list names `vendor_boot.img` is refused. On the
real APK (fork 0.5.14, versionCode 143, 2026-11-15): the first run (43705081715) refused it for the two
false refusals above; with the fix, run 43710905603 passes it: 3310 entries, every content-addressed file named
by the APK's `scripts.sha256` (the largest `guest_manager.apk`, 45,548,812 bytes), no set file among them.

## What a release must be able to load

A package with a library or a dynamic helper that Android's linker would refuse is refused too (exit code 3, no
package). `runtime/package.sh` runs `runtime/package/elf-check.sh` on the stage, after `runtime/build.sh` (which runs
it on its own stage, and fails when the check does) and before the manifest: every NEEDED library of every dynamic arm64 ELF file
(`bin/liboneios-display.so`, `bin/oneios-audio`, `bin/oneios-camera`, `bin/camctl`) must be one of the NDK's API 30
stubs or a library in the package, and every undefined symbol must be defined by one of the file's NEEDED libraries
([building.md](building.md) section 4 has the rule, the check and its tests). The refusal names the file, the symbol
and the library that has it:

```
bin/liboneios-display.so: symbol ANativeWindow_getBuffersDataSpace is in none of its NEEDED libraries (NEEDED: libandroid.so, libm.so, libdl.so, libc.so); it is in libnativewindow.so
package: refused: Android cannot load a file of the package (the lines above; runtime/package/elf-check.sh, docs/releases.md)
```

That is the library of `1f7dd1f` (PR #15): it linked, and the phone could not load it (2026-11-14: the display helper
ended with `UnsatisfiedLinkError`, the panel stayed on the OneOS splash). No tagged release has that commit
(`git tag --contains 1f7dd1f` is empty; the last tag is v0.1.8); the phone ran a build of `main`.
`runtime/package/test.sh` makes such a library (`runtime/package/elf-fault.rs`, built by rustc as a cdylib with the
NDK's clang, without `-z defs` and without `libnativewindow`), puts it in the display helper's place and expects the refusal. The check reads the files only: what it
cannot see is in [building.md](building.md) section 4.

## runtime.json

```json
{
  "format": 1, "name": "oneios-runtime", "version": "0.1.0", "arch": "arm64",
  "session_abi": 1, "image_formats": [1], "xnuhost": "bin/xnuhost",
  "git": { "commit": "<40 hex>", "dirty": false },
  "min_oneos_fork": "0.4.74",
  "placeholder": false,
  "kernel": { "path": "kernel/xnu.macho", "build_id": "...", "sha256": "...", "xnu": "xnu-12377.121.6",
              "profile": "ios-27.0.1-24A446", "record": "kernel/xnu.macho.build.json" },
  "files": [ { "path": "bin/xnuhost", "size": 1234567, "sha256": "<64 hex>", "mode": "0755" } ]
}
```

The first seven keys and `files` are the fork's format 1. Added by this repository: `git` (the commit the
package was built from; `dirty` when `runtime/` or `kernel/` had uncommitted changes), `min_oneos_fork`
(`runtime/package/min-oneos-fork`: the lowest version of the fork that can run this package; the oneos agent
raises it when the session ABI or the package layout changes), `placeholder` (true while `bin/xnuhost` is the
placeholder), `kernel` (null without one), `metal` (`{"bundle": "lib/guest/NativeAdrenoMetal.bundle", "record":
"lib/guest/NativeAdrenoMetal.build.json", "target": "iPhone18,2 24A446", "cdhashes": [...]}`, null without one) and `camera`
(`{"path": "lib/guest/camera", "cdhash": "..."}`, null without one): what the phone's `serve make-image` takes. `files` lists every file except `runtime.json`.

## Versions

- `N.N.N` (semantic versioning) for releases, `N.N.N-suffix` for pre-releases (`0.1.0-rc1`); the tag is `v` + version.
- A tag push is the whole release: the tag fires `release.yml` and every job after it runs by itself; a hand
  `workflow run` dispatch only picks the runtime or the APK alone.
- The APK's versionName and versionCode need no hand bump: `app-android/app/build.gradle.kts` reads the fork's git
  history (the commits since OneiOS 0.1.10's fork commit on top of 143 for the code, the same count in the name's
  patch number), and the apk step only checks the code grew past 143 (`AppUpdate` compares codes).
- The CI caches the pinned NDK, the Rust toolchain and cargo between runs (`actions/cache`): the first run after a
  change to the pin or `Cargo.lock` saves them, the next runs restore in seconds and skip the installs.
- 0.x while nothing boots. The minor number goes up when the session ABI, the image format or the kernel-to-booter
  interface changes; the patch number for everything else.
- `session_abi` and `image_formats` are what the fork checks at run time. A package with a new `session_abi` needs a
  new fork: raise `runtime/package/min-oneos-fork` in the same commit.
- The kernel has its own identity, the **build id** `xnu-<xnu version>-<profile>-<first 12 hex of the series hash>`
  (`oneios-tool kernel build-record`). The same sources, patches and configuration give the same id on any machine.

## Building a package on a Mac

`runtime/package.sh` runs on a Linux x86_64 PC (as CI does) and on a Mac. On a Mac it takes the NDK from
`$HOME/Library/Android/sdk/ndk/28.2.13676358` (the revision is checked as on Linux) and builds with its
`darwin-x86_64` tools (`runtime/build.sh` and cargo's linker `runtime/ndk-cc` choose them by `uname -s`). macOS's `tar` is bsdtar, so the archive is written by
`oneios-tool package archive` (then `pack.py archive`): the same ustar layout, owner 0:0, time `SOURCE_DATE_EPOCH`; for the same files its
bytes equal GNU tar 1.35's (checked 2026-11-12). The `.sha256` is `sha256sum`'s, or `shasum -a 256`'s where there is
no `sha256sum`: the same line. The Apple-file checks are the same `package check`; `runtime/package/test.sh` passed
on the Mac with `pack.py` (19 tests; with the Rust command it passes on Linux, 19 tests with the same output: **not run on
the Mac since**, where `package.sh` now needs Rust for `oneios-tool`). The phone binaries built on the Mac had the same SHA-256 before and after the port's source
changes; they were not compared with a Linux build of the same commit.

```sh
runtime/host-tests.sh                                  # every runtime host test (in Apple's container on a Mac)
runtime/package.sh 0.1.7-dev                           # -> out/release/oneios-runtime-0.1.7-dev-arm64.tar.zst
```

## How the OneOS fork consumes it

1. Takes the latest release of `akshaynexus/OneiOS` (or a pinned tag), downloads the `.tar.zst` and the `.sha256`.
2. Checks the SHA-256 of the archive, unpacks it, and checks every file against `runtime.json` (size, SHA-256, mode);
   a file that is not listed, or a listed file that is missing, refuses the whole package.
3. Stages it as `/data/local/tmp/oneios/runtime/` and starts `runtime.json`'s `xnuhost` with the command line of
   `oneos/interface.md` section 4. With the placeholder the start ends at once with exit code 2 and the fork returns
   to One UI: that is the download-and-verify test.
4. "I have the file" in the app takes the same `.tar.zst` from storage (a build before a release: `runtime/package.sh
   0.1.0-dev`, then `adb push`).

## CI and releases

The "build and release" workflow, `.github/workflows/release.yml`, builds the runtime and app. A separate,
manual-only `.github/workflows/metal-plugin.yml` builds the Metal plug-in for a selected target. The jobs in
"build and release" are:

```
plan ──┬── runtime    ubuntu-24.04   the runtime package and its checks
       ├── apk        ubuntu-24.04   the OneOS app (submodule app/) and its checks
       ├── apk-macos  xcode-27       ./gradlew test of the same app on a Mac
       └───────────── release        needs runtime and apk; a tag or a dispatch only
```

`runtime`, `apk` and `apk-macos` start together after `plan`. The runtime job does not build or download the optional
guest plug-ins.

| Trigger | Builds | Releases |
|---|---|---|
| a pull request, a push to `main` | runtime, apk, apk-macos | nothing (the runtime package and APK are Actions artifacts) |
| a tag `vN.N.N` or `vN.N.N-suffix` on `main` | all three build jobs | the runtime and the APK on the release of the tag |
| a dispatch on `main` (Actions, "build and release", Run workflow; inputs `version` and `both` / `runtime only` / `apk only`) | runtime and/or apk as selected; apk-macos with apk | the release `v<version>`, its tag made at the dispatched commit when missing |
| a dispatch of `metal-plugin` (Actions; input `target`, default `iPhone18,2 24A446`) | the selected Metal plug-in on `xcode-27` | no release; a separate Actions artifact |

- **plan**: whether the run releases, the version (`0.0.0-ci` when not), what it builds. A release's version must be
  `N.N.N[-suffix]` and its commit on `main`. A tag whose release exists already (published from a build machine by
  `scripts/release/release.sh`, "Releasing from a build machine" below) does not release again: its run builds and
  tests only.
- **metal-plugin**: manual-only Xcode 27 build on `xcode-27`. It fetches the pinned public sources, builds
  `NativeAdrenoMetal.bundle` and its `.gpuaddon` sidecar for the selected target, and uploads the bundle, build record,
  check report and build log as a separate artifact. It has no guest image-set input, so it accepts the absence of the
  image-specific linker stubs and uses Xcode's SDK stubs; missing Xcode SDK or linker requirements fail before the build.
  It does not publish a release or feed the runtime job.
- **runtime**: what `ci.yml` and the old `release.yml` did, on every build. `app/` must be an empty submodule
  folder (this job never checks the fork out); no firmware and no Python in git (`check-tracked-files.sh`: a
  tracked `*.py`, or an executable with a python shebang, fails it since 2026-11-21); `bash -n`, `shellcheck -S error`; NDK 28.2.13676358, Rust and
  **oneios-tool** (`.github/scripts/setup-build.sh`: a current stable Rust with `rustfmt` and `clippy`, the targets
  `aarch64-linux-android` (the runtime and oneios-tool for the phone) and `aarch64-unknown-none` (the task stub and
  the test kernel), `qemu-user` (the test kernel's boots), `cargo build --release --manifest-path
  tools/oneios-tool/Cargo.toml`, a few minutes; `runtime/target` is cached with the toolchain, keyed on both
  `Cargo.lock` files); Droid-VM's virglrenderer (below); the host tests
  (`runtime/package/elf-check-test.sh`: the link rule of shared objects and the NEEDED and symbol check, above;
  `runtime/package/test.sh`; `kernel/tests/host.sh`, which runs `kernel series-check` and `kernel build-record
  --self-test`; every executable `*/tests/host.sh` of another area, among them `runtime/tests/host.sh`: the Rust
  workspace's `cargo test`, `cargo fmt --check`, `cargo clippy -D warnings` and `runtime/xnuhost/check-arm64.sh smoke
  check`); the pinned kernel when
  `kernel/release-kernel.txt` names one (fetched from `akshaynexus/OneiOS`, where `kernel/publish.sh` publishes it);
  `runtime/package.sh <version>` (the Apple-file refusal and elf-check of the stage, plus `bin/oneios-tool` built for
  the phone; optional plug-ins are included only when their directories are supplied); the drm2kgsl check and
  `runtime/package/elf-check.sh` on
  the unpacked archive; the members check (above); `.github/scripts/check-phone-tool.sh <version>` ("oneios-tool for
  the phone", below). Artifact `oneios-runtime`: the `.tar.zst`, its `.sha256`, `.runtime.json`, and
  `oneios-tool-<version>-android-arm64` with its `.sha256`. Its limit is 45 min (the tool's Android build comes on
  top of the host one).
- **apk**: what the fork's `.github/workflows/oneios-apk.yml` does, from OneiOS. `git submodule update --init
  --remote app` (the head of the branch `.gitmodules` names, `oneios`; the commit recorded in OneiOS is printed beside
  it); the token for tribalfs' packages (below); JDK 21 (Temurin); Gradle (`setup-gradle`, its open-source `basic`
  cache); Android SDK platform 37 and build-tools 37.0.0; the debug key of the secret `ONEIOS_DEBUG_KEYSTORE_B64`
  (set since 2026-11-22: the key of `secrets/oneios-debug.keystore`, "The debug key" below; without the secret a key
  of the run's own); `app/scripts/build-guest-manager.sh --debug`; mksh;
  `./gradlew --max-workers=3 test assembleFullDebug` in `app/app-android` (what `oneios-tool app build` runs); the
  APK's versionName and versionCode from `aapt2 dump badging` (the versionCode must be `app/build.gradle.kts`'s);
  `check-apk.sh` (above). Artifact `oneos-apk`: `OneOS-oneios-<versionName>-debug.apk` and its `.sha256`.
  The commit recorded for `app/` is `ff2e4a6` (2026-11-23: fork branch `sm8850-support` on `5568231`, for `oneios`: the tribalfs packages from their three GitHub Packages registries with the environment's token first, JitPack never asked for them (YMGPwcca/OneOS `ab28274`, `590b4a6`); an early return writes the shmem THP mode back too, which the switch now writes only under its stop lock (part of their `403d8b4`); the Galaxy S26 Ultra warned with what is known, not refused (their refusal not taken: `docs/s26.md`); Gradle tests on the build laptop (:core:model:test :core:safety:test :core:root:testDebugUnitTest): 706, 0 failures) on `5568231` (2026-11-23: fork branch `s928w-support` on `726b66a`, for `oneios`: every S24 Ultra model number, the Canadian SM-S928W with its US bootloader and vendor model too, gets the SM-S928B's iOS 27 card, gate and switch, host-tested in `OneiosTest`, `ProfilesTest` and `OneiosScriptTest`; Gradle tests on the build laptop (:core:model:test :core:safety:test :core:root:testDebugUnitTest): 703, 0 failures) on `726b66a` (2026-11-23: fork `oneios` with `0cd45ad`, the boot screen's bar, and `2ef79b6`, CI signs with the shared key, merged into the upstream merge; the merged tree's Gradle tests on the build laptop: 701, 0 failures) on `6abbf5b` (2026-11-22: upstream OneOS 0.5.32 to 0.5.35 merged into `oneios` (`925a978`: Debloat, PixelOS 17, the Z Fold8's One UI 9.5 files, Bring apps search, Wireless debugging paired in a guest; no iOS script, root verb, stage path or session ABI changed), then the core:root tests' `head` stand-in for `head -c 0` on a Mac (`6abbf5b`); version 0.5.65 (194); on the Mac `./gradlew test assembleFullDebug` built 0.5.64 (193) at `925a978`, 700 tests, 1 failure (that `head`), then `:core:root` 320 tests, 0 failures, 6 skipped; not run on the phone; before it `43fc1b3`: a check that another job blocks waits visibly and starts by itself, the button spins only while a job runs; before it `bc837b4`: status quick by the files' ids, its steps and seconds shown, 20 s without an answer from su is "Root didn't answer", the setup steps on iOS 27's page, clearer runtime and image choices, live hardware-check output, debug builds signed with the shared key; 689 tests, 0 failures, 76 skipped; before it `5ffc71c`, the CI doc on the key, and `84b0ba5`: the pairing files without XnuPrelink's Python sources, kb
  format 2, 688 tests, 0 failures, 76 skipped; before it `7e34fa4`, 2026-11-21: Make iOS image with `make-image` on a
  fresh phone, 688 tests, 0 failures, 74 skipped, an end-to-end test against the real tool; before that `53abb04`): upstream OneOS 0.5.14 to 0.5.31 merged into `oneios`
  (`2d2f26b`, `728db89`), the on-phone kernel update (`133b64a`) and Update the kernel fetching Apple's kernelcache
  on the phone (`53abb04`). Tested on the Linux PC: `./gradlew test` 679 tests, 0 failures, 75 skipped (the shell-script
  suites, which need dash, busybox or mksh, and one test gated on a real tool) and `assembleFullDebug`; not run on the
  phone.
- **apk-macos**: the same checkout, token, JDK, SDK and guest app on `macos-15`, then `./gradlew --continue test`
  with Homebrew's folders taken off `PATH`, so that core:root's script tests use the harness's stand-ins over
  macOS's own `stat`, `shasum`, `perl` and `awk` (a Mac without coreutils). Every test's start and end is in the
  log (`--init-script .github/scripts/gradle-test-events.gradle`, failures with their whole exception). A watchdog
  stops Gradle after 45 minutes (30 until 2026-11-23: a good run takes about 29 on main, and slower runners were
  still passing tests at 30) and first prints the processes and the test JVMs' threads (`jstack`), so a hung
  test is named and the reports still come out. On a failure the job summary lists the failed tests; the reports
  and results are the artifact `oneos-tests-macos`. Not a gate of the release. The macOS results are in
  "macOS: the fork's unit tests" below.
- **release**: needs runtime and apk (each a success, or skipped because the dispatch asked for the other only).
  Checks the commit is on `main` and that the tag, when it exists, is at the built commit (a tag is never moved);
  downloads the artifacts, checks their `.sha256` (the runtime's and the phone tool's; and that the APK is the file
  the apk job checked), writes the
  notes, creates the release (a pre-release when the version has a suffix; `--latest=false` without a runtime), or
  adds this run's notes to an existing one. Assets are added, never replaced: an asset already on the release with
  the same SHA-256 (GitHub's `digest`) is left, one with other bytes stops the job.
- Nothing of Apple's is downloaded. Actions are pinned by commit. A rerun needs Actions write, which the agents'
  token may not have; a push starts a new run.

**tribalfs' packages.** The SESL9 and oneui-design libraries come from GitHub Packages
(`maven.pkg.github.com/tribalfs`), whose Maven registry wants a login even for public packages.
`.github/scripts/packages-token.sh` (the fork's first step, shared by apk and apk-macos) tries the run's own
`GITHUB_TOKEN` (`permissions: packages: read`) on one pom, prints GitHub's answer when it is a refusal, and then
tries the optional repository secret `GH_PACKAGES_TOKEN` (a token with `read:packages`; `GH_PACKAGES_USER` with it,
else the owner's login). The fork's runs read them with their `GITHUB_TOKEN` (HTTP 200, run 43149217074,
2026-11-12). **OneiOS's own `GITHUB_TOKEN` reads them too**: HTTP 200 for the probe pom in the first runs of
this workflow (2026-11-15, runs 43705081715 and 43705167315, both the ubuntu and the macOS job), and the Gradle
builds resolved every tribalfs dependency with it. GitHub lets any repository's `GITHUB_TOKEN` read public
packages; tribalfs' are public. The secret path stays in case GitHub changes that or the packages become private;
no secret is needed today.

**macOS: the fork's unit tests** (the goal: `./gradlew test` passes on the owner's Mac, so that `oneios-tool cycle
--force app` runs there without `--no-test`). Every run of 2026-11-15 passed ubuntu's `./gradlew test
assembleFullDebug` for every fork commit below. On `macos-15` (macOS 15.8.2, Homebrew off `PATH`) every other
module's tests passed, and `:core:root:testDebugUnitTest` gave:

| Run | Fork commit | `:core:root:testDebugUnitTest` on macOS |
|---|---|---|
| 43705081715 (#43) | `820253b`, the head of `oneios` | `theLogVerbsListTheSessionLogsAndReadThemInWholeLines`, `aCheckRunsTheStagedToolAndReportsItsOutputAndExitCode` and `aProcessThatEndsLateAfterSigkillIsWaitedForAtGateA` (`OneiosScriptTest`) failed, then the task hung until the job's 60 minutes |
| 43705167315 (#44, test only) | `479d474`, `fix-macos-tests-2` (OneOS PR #7) | hung until the job's 60 minutes (no test events in the log yet) |
| 43710905603 (#43), 43710921989 (#44) | `820253b`; `479d474` | with the test events and the watchdog: PR #7 fixes the first two failures (431 passed); `aProcessThatEndsLate...AtGateA` fails on both; both hang in `aHungToolIsEndedAtTheLimitAndWhatItChangedIsPutBack`. The watchdog's process list and `jstack`: `oneios_root.sh check levels` and its stand-in `brightctl levels` still running after 25 minutes, the test reading their output |
| 43714546247 (#44) | `8c6fae1`, `fix-macos-tests-3` (OneOS PR #9: #7 and a third round: `now` without `date +%N`, a `cat` stand-in for `/proc/PID/cmdline`) | no hang: the task ran to its end, 270 tests, 4 failed (`aProcessThatEndsLate...AtGateA`, `theReturnsLogSaysWhoReturnedAndHow`, `aChatteringHoldOfTheRockerStillReturns`, `StageScriptTest.removeSystemDeletesOnlyTheSystemSetUpNow`), 2 skipped |
| 43717760387 (#44) | `406f008`, `fix-macos-tests-3` (a fourth round for those four) | 270 tests, 1 failed: `theReturnsLogSaysWhoReturnedAndHow` (the return log's lines twice); Gate A, the chatter and the `find` test pass |
| 43720242712 (#44) | `433cb0c`, `fix-macos-tests-3` (the return log) | **passes**: `./gradlew test` BUILD SUCCESSFUL on macOS, core:root's 270 tests with the 2 skipped as before. The same run's ubuntu job failed one test, `aReadyGuestIsLeftRunningPastTheDeadline` (a race of the test at its wakelock line, passed in every run before; fixed in `200263f`) |

The causes and the fixes are in OneOS PR #9 and the fork's `docs/oneios.md` ("The third round", "The fourth
round"): the tests' stand-ins and timing on a Mac, and one line of `oneios_return.sh` that is read only with a test
root (`ONEIOS_TEST_OUT_IS_LOG`, set by the test where there is no `/proc`: the return log written once); on the
phone the script is as before. The head of `oneios` stays red on
macOS until #9 (or #7 and #9) is merged; the job is no release gate.
On the owner's Mac (2026-11-22, not CI; JDK 21, dash, Homebrew's GNU tools on `PATH`), the fork at its upstream 0.5.35
merge (`925a978`): `./gradlew test assembleFullDebug` 700 tests, 1 failure, `StageScriptTest.debloatWritesTheListOrNothing`
(BSD's `head` refuses `head -c 0`, which upstream's debloat verb sends for an empty list); with the tests' `head`
stand-in (`6abbf5b`) `:core:root:testDebugUnitTest` 320 tests, 0 failures, 6 skipped.

**The 3D GPU in a release.** The runtime job fetches Droid-VM's virglrenderer (`scripts/setup/get-references.sh virglrenderer`) before the host tests and the package, on every build, and refuses a package whose `xnuhost` or `gpuctl` was built without drm2kgsl. Without that checkout both build 2D-only and only say so at run time: runtime 0.1.1 was published that way and its GPU check failed on the phone with "this build has no drm2kgsl". 0.1.3 is the first release with the check (0.1.2's run failed to link: a stale 2D-only `gpu3d.o` from the host tests beside the real drm2kgsl; the fetch now runs before the host tests and `gpu3d.o` rebuilds when the drm2kgsl setting changes). Before 2026-11-15 only the tag's run fetched it; pull requests built 2D-only.

**oneios-tool for the phone (from 2026-11-15, branch `app-oneios-tool`; no tagged release has it yet).**
`.github/scripts/check-phone-tool.sh VERSION` checks `oneios-tool-VERSION-android-arm64`: its `.sha256` line names it
and matches; it is byte for byte the package's `bin/oneios-tool`, which `runtime.json` lists with that SHA-256 and mode
0755; it is an AArch64 PIE for `/system/bin/linker64` with no `.symtab` and no DWARF section (Rust's allocated `.debug_gdb_scripts` stays). **Why in the runtime
package and not in the APK** (as `lib/arm64-v8a/liboneios-tool.so`):
- *Root's exec and SELinux:* the fork's root script already runs the staged runtime's `bin/` files as root, and only
  the one whose SHA-256 it recorded at staging (`check`: the phone checks; `tool`: this one). The same rule covers the
  tool with no new path. An APK's native folder (`/data/app/~~.../lib/arm64`, `apk_data_file`) is a second place root
  would exec from, with a label of its own and a path that changes with every install; not proven on the phone.
- *Updates apart from the app:* the tool's code lives in OneiOS and changes with the runtime and the kernel (its
  `prelink`, `image` and `ioslog` follow them); in the package it updates with each runtime release, without a new APK,
  and the version the app talks to is the one beside the xnuhost it serves.
- *Size:* 14,756,048 bytes stripped, and the `0.0.0-ci` package with it 5,709,276 bytes (CI run 43721746429 of `aad064f`, with `serve`; v0.1.9 without the tool: 805,244);
  in the APK every user would carry it, also those who never set up iOS.
- *What it costs:* no tool before a runtime is staged. The flows need a staged runtime anyway (the Make iOS image step
  starts with the runtime download), and the runtime's own import keeps root's shell checks, so no step needs the
  tool before it is there.

## The kernel in a release

Apple's kernelcache is in no release, runtime or APK. The fork's Update the kernel downloads each phone's own copy
from Apple's server with the runtime's `oneios-tool` (`fetch` of the profile's `kernelcache` member, about 23 MB,
checked against the profile's size, CRC-32 and SHA-384) and decodes it on the phone (`im4p`; the result's SHA-256
must be the fork's `kb/kb.json` `kernelcache_sha256`), then prelinks the package's `kernel/xnu.macho` into
`kernel.kc` there (`oneos/interface.md`, "Update the kernel"). So the runtime package must keep shipping
`lib/profiles/ios-27.0.1-iPhone18-2.toml`, a `bin/oneios-tool` with `fetch`, `im4p` and `prelink`, and a releasable
kernel. Measured on the Linux PC 2026-11-21: fetch 12.4 s, im4p 0.1 s, prelink 2.8 s and 698 MB; not run on a phone.

Make iOS image on the phone (fork `7e34fa4`) needs a runtime release whose `bin/oneios-tool` is built from OneiOS main at
or after `b70fa16` (`make-image` with the AEA decryption) and whose `runtime.json` has `kernel`. v0.1.12's tool has no
`make-image`, so with it the app stages the latest release and, when that is still v0.1.12, says "This runtime can't
make the image yet" (v0.1.13 is the first release with it). The app compares the release asset's version (`oneios-runtime-<version>-arm64.tar.zst`) with
`runtime.json`'s `version`; they must be the same text, or the app downloads the runtime again on every run. The APK
carries only the pairing files in `assets/kb/`, nothing of Apple's; the phone fetches the IPSW members and the AEA keys
from Apple. On the Linux PC the whole set from nothing (`oneios-tool image build`, empty inputs, the v0.1.12 kernel)
took 322 s with the download, peak 1,032 MiB, 37.1 GB asked (docs/agents.md, 2026-11-21).

The pairing files' format (2026-11-22): the fork's `assets/kb/` is format 2 (`kb.json` `"kb": 2`: `match/` holds the
reviewed fixtures alone; format 1 also carried XnuPrelink's three Python sources, which the matcher signature of the
tools up to v0.1.13 was over). A runtime's `bin/oneios-tool` reads format 2 when its `serve` hello has `"kb": 2`
(OneiOS main at or after `de6b429`; v0.1.13 and older have no `kb` and refuse a `match/`
without `fileset_match.py`). So an APK of format 2 goes with a runtime release after v0.1.13: with an older staged
runtime, Update the kernel stops with "get the latest OneiOS runtime" and Make iOS image stages the newest one.

Make iOS image and the packed profiles (issue 75, 2026-11-23): a runtime packs every file of `profiles/` in
`lib/profiles/` (v0.1.13 ... v0.1.15: four, v0.1.16: five, two of them host records). The app's `make-image` up to fork
`ff2e4a6` (APK 0.5.68 of v0.1.16 and the CI prereleases up to 0.5.74) named no `profile`, and the tools up to v0.1.16
take a default only from a folder of one file: every Make iOS image was refused `usage` ("holds 5 profiles: give
\"profile\"") before its download, which the app says as "OneOS and the runtime don't agree". The fork names the
profile since `62a2383`, so an APK with it works with the v0.1.16 runtime as it is; the tool after v0.1.16 also
chooses it itself (the staged kernel's iOS for iPhone18,2; `tools/oneios-tool/README.md` `make-image`). The same
screen showed "oneios-tool 0.1.0 · 8 threads": `hello`'s `tool` was the crate's version in every release up to
v0.1.16. runtime/package.sh now builds the phone's tool with `ONEIOS_VERSION=VERSION`, and its hello says the
release's version.

The kernel is not built in CI. A build machine builds it (`kernel/README.md`), and:

1. `kernel/publish.sh` packs a kernel folder (`xnu.macho`, its build record `xnu.macho.build.json`, the series
   report `series.txt`) as `<build id>.tar.zst` (`xnu-12377.121.6-ios-27.0.1-24A446-<12 hex>.tar.zst`) in
   `out/kernel/publish/`, prints its SHA-256 and the line for step 2, and with `--upload` uploads it to the release
   `kernel-<build id>` of this repository. It runs the runtime package's own check first (`oneios-tool package
   check`: a record of this builder, of these bytes, not prelinked, no kernelcache kext, no restricted source).
   Which folder: `kernel/publish.sh --hosted NAME` is `out/kernel/current-hosted-NAME` (`kernel/build-hosted.sh
   HOSTED_OUT=NAME`; `--hosted lead` is `out/kernel/current-hosted`); `KERNEL_DIR=DIR` any folder; neither:
   `out/kernel/current` (the VM kernel of `kernel/build-linux.sh`, which xnuhost does not run). Before 2026-11-21
   the script still called the deleted `runtime/package/pack.py`, so it could not pack anything.
2. `kernel/release-kernel.txt` gets one line, `kernel-<build id> <sha256 of the archive>`, in a commit.
3. The runtime job (of every build, not only a release) downloads that archive, checks the SHA-256, and `runtime/package.sh` checks the build record
   again before the kernel goes into the package as `kernel/xnu.macho` and `kernel/xnu.macho.build.json`;
   `runtime.json`'s `"kernel"` is then `{"path": "kernel/xnu.macho", "build_id", "sha256", "xnu", "profile",
   "record": "kernel/xnu.macho.build.json"}`, taken from the record.

Without `kernel/release-kernel.txt` the runtime package has no kernel (`"kernel": null`).

**The kernel to release is the hosted one, built with `ONEIOS_CRYPTO=original`** (the owner's call of 2026-11-21):
the OneOS app's update pulls the runtime package, pairs its `kernel/xnu.macho` with the phone's own Apple
kernelcache (`oneios-tool serve prelink`) and boots the `kernel.kc` it made. The default hosted kernel
(`ONEIOS_CRYPTO=source`) links UniXNU's source corecrypto (corecrypto-2023, Apple's Internal Use License): its
record lists it in `restricted_sources`, says `"releasable": false`, and both `kernel/publish.sh` and
`runtime/package.sh` refuse it. The steps for a release kernel:

```sh
scripts/bg.sh kernel-release env HOSTED_OUT=release ONEIOS_CRYPTO=original kernel/build-hosted.sh
#   -> out/kernel/current-hosted-release/{xnu.macho, xnu.macho.build.json, series.txt}; the build fails when the
#      record is not "releasable": true
scripts/bg.sh prelink-release env KERNEL=out/kernel/current-hosted-release/xnu.macho OUT=out/kernel/24A446-release kernel/prelink/build.sh
#   audit and stubs-check must pass; then a PC boot (docs/building.md section 5) with --kernel out/kernel/24A446-release/kernel.kc
kernel/publish.sh --hosted release          # pack and check; --upload only for the build to release
```

The hosted build record (`oneios-tool kernel build-record finish --kernel FILE --post-link STEP...`, written by
`kernel/build-hosted.sh` since 2026-11-21) is the record of the file the build leaves, after `kernel hosted-fixup`
and (source only) the seal: its `sha256`, `bytes` and `macho` are that file's; `patches` are the board series and
`kernel/patches/oneios-hosted/series` in order (plus `optional/original-corecrypto-only.patch` for original);
`profile` is the OS profile (`ios-27.0.1-24A446`); `defconfig` `oneios_hosted_defconfig`; `hosted: true` and
`post_link` (`["hosted-fixup"]` for original, `["seal", "hosted-fixup", "seal"]` for source) are its own fields, and
the post-link steps are part of the build id. Whether the source corecrypto is linked is counted, not assumed:
`linked.XNU_SOURCE_CORECRYPTO14` gives how many of the archive's global symbols
(`tree/build/corecrypto-2023/libcorecrypto_kernel_noasm-namespaced.a`, 1191) are external symbols of the kernel
(`in_kernel`): 878 for a source kernel, 0 for original. A configuration that builds the archive is restricted when
`in_kernel` is not 0 or the archive cannot be read. The 18 `cc*` symbols an original kernel still has are XNU's own
APSL `osfmk/corecrypto` (`ccsha256_di`, `ccdrbg_*`), not the archive's.

The first release candidate (2026-11-21, not uploaded; the lead publishes it after review): build
`xnu-12377.121.6-ios-27.0.1-24A446-703a6b34e73f`, `xnu.macho` 24,488,792 bytes, SHA-256
`326e2144d512d78990d6c0cd0e2c7ffdf2696b0fdb03cb05827a2be59b5a040c`, 166 patches, `"releasable": true`; archive
`out/kernel/publish/xnu-12377.121.6-ios-27.0.1-24A446-703a6b34e73f.tar.zst`, line
`kernel-xnu-12377.121.6-ios-27.0.1-24A446-703a6b34e73f 4cf558f572443d2f6e9f14d629ab4705fa596ec52b595861735a46b1fa61d980`.
CI's step was replayed with it (the archive unpacked, `REQUIRE_KERNEL=1 KERNEL_DIR=... runtime/package.sh
0.0.0-kcheck`): `runtime.json`'s `"kernel"` names that build id and SHA-256. An earlier build of the same day
(`...-5f699bc0f23e`) is not a candidate: it has the old hosted 0068 and dies at launchd. What was proven for it: kernel/README.md "Releases and licences".

The kernel of v0.1.16 (2026-11-23, built on the build laptop, uploaded): `xnu-12377.121.6-ios-27.0.1-24A446-776f6c56613a`,
`xnu.macho` 24,626,200 bytes, SHA-256 `b074db5e18603205d547d2ed083f7ea9be2a5dffb8f9278ef8e4ebe70035be06`, hosted patches
to 0087 (0084 MobileGestalt cache, 0085 Edge spill, 0086 Actuator chain, 0087 bluetoothd's HCI transport),
`"releasable": true`; archive 6,439,987 bytes, SHA-256 `e841b035...`, release `kernel-xnu-...-776f6c56613a`. Its prelink
(`out/kernel/24A446-release/kernel.kc`, SHA-256 `f499f3ae...`): audit 2711 paired, 93 on the stub, 0 failures;
`prelink stubs-check` 666 good, 0 bad; `kernel/tests/host.sh` 8 of 8. Not booted on the phone before the release.

The kernel of v0.1.18 (2026-11-23, built on the build laptop in the `t3/enable-jobs-0092` worktree, uploaded from the
main checkout with `kernel/publish.sh --hosted jobs --upload`): `xnu-12377.121.6-ios-27.0.1-24A446-c889b14b24f3`,
`xnu.macho` 24,608,768 bytes, SHA-256 `72b67dff4838e9677bcd65747cadff8fa89fb04ae8fc10a9c70242884b91aa88`, `main`'s hosted
series to 0092 (after v0.1.16's: 0088 the camera's model name `OneiOSAP`, the fix for no sound; 0089 the scheduler's
completion step; 0090 the boot add-on; 0091 no pull-back of spilled threads; 0092 the jobs policy), original
corecrypto, `"releasable": true`. Its `series.txt` checked against `main` before the upload: the 174 tracked patches
and the 3 generated ones (equal to v0.1.16's) all match. Archive 6,438,794 bytes, SHA-256 `60b86a55...`, release
`kernel-xnu-...-c889b14b24f3`. Its prelink (`out/kernel/jobs-24A446/kernel.kc`, SHA-256 `aef14856...`): audit 0
failures, `kernel/tests/host.sh` 8 of 8. **Booted on the phone before the release** (2026-11-23 13:45, `boot-jobs`:
0092's 134 jobs, the wallpaper gallery loads; that session ended in xnuhost's GPU renderer, the fault v0.1.18's
runtime fixes, not in the kernel).

The kernel of v0.1.22 (2026-11-24, built on the build laptop in the `release-kernel-a56` worktree and branch:
`096f1e76`, the commit that pinned c889b14b24f3, plus main's 0004 (`a59eb64`) and 0012 (`54f47be`), uploaded from
there with `kernel/publish.sh --hosted release-a56 --upload`): `xnu-12377.121.6-ios-27.0.1-24A446-7d540ae9e518`,
`xnu.macho` 24,656,672 bytes, SHA-256 `0a37d06ed64d61b7e66d13f97fd90ca97348ff57c66f0f983685d1b83d8dbdf4`, the same
hosted series to 0092 as v0.1.18's kernel, with one change: hosted 0004 runs `newfs_apfs` again without `-x` when it
exits 64 (usage). iOS 16.5's `newfs_apfs` has no `-x`, and the Galaxy A56's first boot of our image panicked with
"offline scratch Data mount failed: 5" (issue 73, [a56.md](a56.md)). 0012's hunk is moved onto that loop. Main's later
patches (0093 to 0101) are left out on purpose: the S24 Ultra keeps the kernel it booted. Original corecrypto,
`"releasable": true`, 177 patches. Archive 6,460,689 bytes, SHA-256 `4ffa3f609ff15f35c3de47d19561197629ad752c36a4c50b5819c65230cf3e64`,
release `kernel-xnu-...-7d540ae9e518`. Its prelink (`out/kernel/24A446-release-a56/kernel.kc`, SHA-256
`f5ddb36983d81c60...`): audit 2711 paired, 93 on the stub, 0 failures. **Not booted** on any phone before the release
(the owner's 2026-11-24 rule: no qemu boot; the A56 user's boot is the test).

The kernel pinned after v0.1.23 (2026-11-24, `port/a56-full`; for the next release; v0.1.23 has v0.1.22's kernel): built on the build laptop in the
`release-kernel-a56` worktree and branch (`82fa3b0`: v0.1.22's kernel, 096f1e76's series to 0092 with main's 0004 and
0012, plus the new hosted 0102-0107), `HOSTED_OUT=release-a56c ONEIOS_CRYPTO=original kernel/build-hosted.sh`, uploaded
from there with `kernel/publish.sh --hosted release-a56c --upload`: `xnu-12377.121.6-ios-27.0.1-24A446-16bd44f9d737`,
`xnu.macho` 24,675,504 bytes, SHA-256 `03a777fd9767e2c3bc89d1d84f6987c238b6fa265c5f956c346e4c25863cf5a7`, 183 patches,
`"releasable": true`. Archive 6,449,789 bytes, SHA-256
`6c198b67494ee366290bb126184a1c7d04b406d55784349ae58c05fd08736ba7` (downloaded again from the release and checked),
release `kernel-xnu-...-16bd44f9d737`. What it adds: the other Galaxy A56 fork's iOS 16.5 kernel fixes (touch 0103,
configd's IONetworkStack entitlement 0104, LaunchServices' keep 0105, logd's flush at a graceful return 0106, the
queue-head dump 0102) and 0107, iOS 27's port policy left off for an iOS 16 userland (the EXC_GUARD kills of
SpringBoard and the daemons in the A56 logs of 2026-11-24 20:05 and 20:31; [a56.md](a56.md) "The second port"). Each
is off for iOS 27: 0103-0105 act on boot arguments xnuhost adds only for a 20F66 image set, 0107 on 0068's
`oneios-legacy-registered-ports=1`, which only a 20F66 image passes, 0106 only on a graceful return (the galaxy-a56
row), so the S24 Ultra runs v0.1.22's kernel code. Main's 0093-0101 are still left out. Its prelink
(`out/kernel/24A446-release-a56c/kernel.kc`, SHA-256 `cb7d7e30287f3be5...`): audit 2711 paired, 93 on the stub, 0
failures. `kernel/tests/host.sh` 12 of 12 (with 0103's conversion test). **Not booted** on any phone. (A first build
without 0107, `...-8b2943c5003a`, was uploaded an hour before and deleted when 0107 came; nothing had pinned it.)

The kernel pinned after v0.1.24 (2026-11-24, fix/a56-ios16-faults; for the next release; v0.1.24 has `16bd44f9d737`):
built on the build laptop in the `a56-ios16-relk` worktree, branch `a56ios16/release-kernel-a56`, pushed as
`release-kernel-a56` (`0888d97`): `16bd44f9d737`'s branch (`82fa3b0`) plus main's 0108 (`b5e706a` cherry-picked as
`2786c3d`: the A56's GPU accelerator, the kernel part the a56-gpu work had prepared on `a56gpu/release-kernel-a56` as
`f8def7dad866` and not published; this kernel carries it, so there is one A56 kernel) and hosted 0109 (an iOS 16
userland's fat binaries run their arm64 slice: webbookmarksd's crash loop, [a56.md](a56.md) "The faults of the v0.1.24
boot"). `GH_TOKEN=$(gh auth token) HOSTED_OUT=a56ios16-rel ONEIOS_CRYPTO=original kernel/build-hosted.sh` (325 s; the
token because the DriverKit headers' GitHub API fetch had hit the anonymous rate limit), uploaded from there with
`kernel/publish.sh --hosted a56ios16-rel --upload`: `xnu-12377.121.6-ios-27.0.1-24A446-181d8bf2d60a`, `xnu.macho`
24,676,528 bytes, SHA-256 `3a57807cd4ec5a9e9ac93bbed620ac3bb97d23cfb921efd1623310bded645814`, 185 patches,
`"releasable": true`. Archive 6,481,558 bytes, SHA-256 `e5ec24121664951a7f6f1c71a2a49996b5244519062642fc6b864f91be5122b0`
(downloaded again from the release and checked), release `kernel-xnu-...-181d8bf2d60a`. Both new patches are off for
iOS 27: 0108 acts only with `oneios-gpu-amd=1` on an AMDGPU capset, 0109 only with 0068's
`oneios-legacy-registered-ports=1`, which only a 20F66 image passes. Its prelink
(`out/kernel/24A446-a56ios16-rel/kernel.kc`, 96,288,768 bytes, SHA-256 `0dc78acf9d5df493...`): audit 2711 paired, 93 on
the stub, 0 failures. `kernel/tests/host.sh` there 9 of 9. **Not booted** on any phone (no A56 here, no qemu boot).
The runtime half of the same fixes (the CoreImage text fixup, the iOS 16.5 product-id and serial number) is xnuhost's,
on main: the next runtime release carries both.

## Released

| Tag | Date | Commit | What |
|---|---|---|---|
| [`v0.1.0`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.0) | 2026-11-10 | `ba306c5` | The first release, runtime only (`"kernel": null`, `"placeholder": false`): `bin/xnuhost`, the helpers (`oneios-audio`, `oneios-net.sh`, `oneios-camera`, `oneios-display` with `liboneios-display.so` and `lib/oneios-display.dex`) and the five phone checks (`gpuctl`, `bridgectl`, `brightctl`, `displayctl`, `camctl`). 13 files, 502,337 bytes packed, `min_oneos_fork` 0.4.74. Made for the fork's Hardware checks screen (fork 0.4.75). Nothing in it has run on the phone; without an image set and a kernel collection its xnuhost boots nothing |
| [`v0.1.7`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.7) | 2026-11-12 | `8067cea` | Runtime only (`"kernel": null`): `bin/xnuhost` (deferred unmaps, `ram=auto`, the display helper's socket kept on transient send errors), the helpers and the eight phone checks (`thermalctl` and `batteryctl` beside the five of 0.1.0, and `wifictl`). 17 files, 745,405 bytes packed, `min_oneos_fork` 0.4.74. Tests before the tag on the Mac: `runtime/package/test.sh` 19 pass, `kernel/tests/host.sh` 5 pass; the release run (43269875806) passed the host tests and the drm2kgsl check. Staged on the phone with `oneios-tool phone stage runtime` from the downloaded asset (SHA-256 checked); `oneios-tool phone sums` equals every file. The first release staged on the phone; 0.1.1-0.1.6 were published (not listed here) and the phone ran local `-dev` builds until now. Not booted yet |
| [`v0.1.8`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.8) | 2026-11-12 | `ea06d46` | Runtime only (`"kernel": null`, `"placeholder": false`): 17 files, 745,567 bytes packed, `min_oneos_fork` 0.4.74. Release run 43277047626 |
| [`v0.1.9`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.9) | 2026-11-15 | `8644ae9` | Runtime only (`"kernel": null`): xnuhost and the bridges of #26-#42 (locked RAM by default, the hosted CLPC signals, the GPU level by deadline and per-context priority, the host-overhead fixes, the Wi-Fi scan reuse, the S Pen (`pen=auto`), `hw-backend=samsung|treble`). 17 files, 805,244 bytes packed, `min_oneos_fork` 0.4.74. Tests before the tag on the Mac: `elf-check-test.sh`, `runtime/package/test.sh`, `kernel/tests/host.sh` 5 of 5; the release run 43703261330 passed. Checked after it: the asset against its `.sha256`, `runtime.json` first. Not staged or booted on the phone yet |
| [`v0.1.12`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.12) | 2026-11-21 | `9288628` | The first release with a kernel: the runtime package carries the hosted XNU `xnu-12377.121.6-ios-27.0.1-24A446-703a6b34e73f` (`ONEIOS_CRYPTO=original`, no source corecrypto, from the release [`kernel-xnu-...-703a6b34e73f`](https://github.com/akshaynexus/OneiOS/releases/tag/kernel-xnu-12377.121.6-ios-27.0.1-24A446-703a6b34e73f), archive sha256 `4cf558f5...`) as `kernel/xnu.macho`, `runtime.json` `"kernel"` naming it, `lib/profiles/` with the iPhone18,2 profile the phone fetches the kernelcache with, and `bin/oneios-tool` (`fetch`, `im4p`, `prelink`). 26 files, 12,197,310 bytes packed, `min_oneos_fork` 0.4.74. Beside it the APK `OneOS-oneios-0.5.46-debug.apk` (versionCode 175, 85,564,815 bytes; fork `oneios` head: upstream OneOS 0.5.31 merged, Update the kernel fetching Apple's kernelcache on the phone, `assets/kb/` with `kernelcache_sha256` `349c4692...`) and `oneios-tool-0.1.12-android-arm64`. Also in it: the host device table, the fork ports of 2026-11-21, the GPU lane fix, the image set's `bridges=`/`wifi=`/`bluetooth=`/`haptics=`, 0068 opt-in. Release run 44885786548 (the main run 44883569529 before it: the kernel downloaded, `sha256 ... OK`, packaged). Checked after it: every asset against its `.sha256`, `runtime.json`'s kernel, the APK's `aapt2 badging` and its `assets/kb/`. The kernel PC-booted to SpringBoard; not staged or booted on a phone yet |
| [`v0.1.13`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.13) | 2026-11-21 | `c92c087` | The first release whose `bin/oneios-tool` has `make-image`: a fresh phone builds the whole iOS image set itself (the IPSW members and the AEA keys fetched from Apple, the APFS copy, the Rust root tweaks, seed-pack, prelink, the lists). Same kernel as v0.1.12 (`xnu-12377.121.6-ios-27.0.1-24A446-703a6b34e73f`, `runtime.json` `"kernel"` naming `kernel/xnu.macho`). 26 files, 12,933,407 bytes packed, `min_oneos_fork` 0.4.74. Beside it the APK `OneOS-oneios-0.5.47-debug.apk` (versionCode 176, 85,603,471 bytes; fork `7e34fa4`: Make iOS image on the phone) and `oneios-tool-0.1.13-android-arm64` (17,473,936 bytes). Also in it: Python retired (serde_json, the tar crate, the plist crate), `app version` reading the fork's git-counted version, the serve request-id fix. Release run 44933682731 (the main run 44915463438 before it). Checked after it: every asset against its `.sha256`, `runtime.json`'s kernel, `make-image` in both tools, the APK's `apkanalyzer` version 0.5.47 / 176. Not staged, built or booted on a phone yet |
| [`v0.1.14`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.14) | 2026-11-22 | `7e4a41b` | The first release published from the build machine by `scripts/release/release.sh` (runtime, APK and host tests in parallel, 260 s), not by the workflow; the tag's run 45028107814 found the release already there and builds and tests only. The first runtime whose `bin/oneios-tool` reads the pairing files of kb format 2 (`serve` hello `"kb": 2`, the matcher signature over the fixtures alone, no XnuPrelink Python: `de6b429`). Same kernel as v0.1.12. Runtime `oneios-runtime-0.1.14-arm64.tar.zst`, 12,938,585 bytes, 26 files, SHA-256 `094a1272e346752c5ef5170a19fd0bf6fc6f1c17fcd62d2eda2c1f276897984b`; `oneios-tool-0.1.14-android-arm64`, 17,509,600 bytes, `d16f3013…`. APK `OneOS-oneios-0.5.49-debug.apk` (versionCode 178, fork `5ffc71c`, no `.py`), 85,581,942 bytes, `5535f6fa…`, the first signed with the shared debug key `8b3be966…` ("The debug key"): it needs one uninstall of an earlier APK. Checked after publishing: every asset downloaded, its `.sha256` and the APK's signer. Not yet run on the phone |
| [`v0.1.15`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.15) | 2026-11-22 | `7409ab6` | Published from the build machine by `scripts/release/release.sh` (the first run's tmux job ended on a hangup during the host tests before it published anything; the second ran to the end, exit 0). The app's fix for the page that stayed on "Checking" (fork `bc837b4`): root's status by the files' ids, nothing hashed (the switch still hashes); each step with its seconds and Stop; "Root didn't answer" after 20 s without su; the setup steps on iOS 27's page; the runtime and image choices as lists with the recommended way first; live hardware-check output. Same kernel and runtime code as v0.1.14. Runtime `oneios-runtime-0.1.15-arm64.tar.zst`, 12,938,616 bytes, 26 files, SHA-256 `6c5f7fc3eccdf567661d8bb9f3bf5d6e783c5c07619a1b41c18e9b640baba6fd`; `oneios-tool-0.1.15-android-arm64`, 17,509,600 bytes, `d16f3013…` (the same bytes as v0.1.14's). APK `OneOS-oneios-0.5.50-debug.apk` (versionCode 179, fork `bc837b4`), 85,631,930 bytes, `d95cb0c6…`, signed with the shared debug key `8b3be966…`: it installs over 0.5.49 without an uninstall. Checked after publishing: every asset's digest on the release against the files built here, the tag at `7409ab6`. Not yet run on the phone |
| [`v0.1.17`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.17) | 2026-11-23 | `d51efaf` | Published from the build laptop by `scripts/release/release.sh` (worktree at origin/main, exit 0). The first release with the **Rust runtime** (PR #76: xnuhost, the loader, the bridges and the display helper in Rust; the C removed), booted on the phone the same day with the 0.1.16 kernel (docs/booter.md 2.6a, "On the phone, Rust against C"; the return to One UI clean). The first with **SM-S928W support** (issue 74, `07ef2110`: every S24 Ultra model number gets the S928B's path; the app at fork `4416d6f`, which has `5568231`). Same kernel as v0.1.16 (`kernel/release-kernel.txt`). Runtime `oneios-runtime-0.1.17-arm64.tar.zst`, 13,959,885 bytes, 29 files, SHA-256 `c2cf1da836b7a42b6004cea3817a1ae9e66b940ec1729ff953b17ffa3e3e4319`; `oneios-tool-0.1.17-android-arm64`, 17,898,896 bytes, `54343f42…`. APK `OneOS-oneios-0.5.78-debug.apk` (versionCode 207, fork `4416d6f`), 85,989,977 bytes, `58e61f81…`, signed with the shared debug key `8b3be966…`: it installs over 0.5.49 and later without an uninstall. The release package itself not staged on a phone yet (the phone ran a runtime built from `00c174d`, the same runtime code) |
| [`v0.1.18`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.18) | 2026-11-23 | `973e8f7` | Published from the build laptop by `scripts/release/release.sh 0.1.18` in the main checkout at origin/main (exit 0; the first `--build-only` run failed in its runtime lane on a race between two `runtime/build.sh` at once, fixed by `973e8f7` and tested with two builds at once). **A new kernel**: `xnu-12377.121.6-ios-27.0.1-24A446-c889b14b24f3` (release `kernel-xnu-...-c889b14b24f3`, archive `60b86a55…`): `main`'s hosted series to 0092, so it brings 0088 (the camera's model name `OneiOSAP`: sound, volume and haptics, which the 0087 kernel of v0.1.16 and v0.1.17 lacked), 0089-0091 (the scheduler's completion step, no pull-back of spilled threads), 0090 (the boot add-on) and 0092 (every job but the crash loops); it is the kernel the phone booted on 2026-11-23 at 13:45. The runtime: drm2kgsl patch 0009 (a guest request naming id 0 no longer ends xnuhost: the wallpaper gallery's abort, docs/gpu.md 2.4a), each refused GPU blob logged with its check. Runtime `oneios-runtime-0.1.18-arm64.tar.zst`, 13,958,081 bytes, 29 files, SHA-256 `bfdcca48628f771a21116e1c27b4b45c81637628845ec9257a2b736a2b3e6a70`, `runtime.json` kernel `c889b14b24f3`; `oneios-tool-0.1.18-android-arm64`, 17,898,896 bytes, `20764b86…`. APK `OneOS-oneios-0.5.78-debug.apk`, the same app as v0.1.17 (versionCode 207, fork `4416d6f`), 85,997,625 bytes, `c1b4b4b6…`, the shared debug key. Checked after it: every asset against its `.sha256` (downloaded from the release), `runtime.json`'s version and kernel. The camera plug-in fix (`a5c244d`) is in the add-on, which is built on its own, not in this release. The release package itself not staged on a phone yet |
| [`v0.1.19`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.19) | 2026-11-23 | `f293cd5` | Published from the build laptop by `scripts/release/release.sh 0.1.19` (under `scripts/bg.sh`) in the main checkout at origin/main (exit 0; runtime, apk and tests lanes ok). **The switch fix**: the app at fork `6e3e387` (`oneios`), which has `6437eef`: `oneios_switch.sh` step [3/5] reads system_server's watchdog fds with one `ls -l` per process, not one `readlink` per fd (1522 fds took 48 s on the phone on 2026-11-23 18:19 with 0.5.78 and runtime 0.1.18, and the watcher returned to One UI with "the switch did not start xnuhost within 60 s"; the same script staged by hand at 18:24 booted iOS). **The versions line**: xnuhost logs `versions: runtime ... (git ...), kernel ..., kernel.kc ..., image ..., app versionCode N` at its start and again after `stopping:` ([booter.md](booter.md) 2.9b), and `oneios-tool log summary` prints it as its second line; the app passes its versionCode (`ONEIOS_APP_VERSION_CODE`, fork `6e3e387`). Same kernel as v0.1.18 (`xnu-12377.121.6-ios-27.0.1-24A446-c889b14b24f3`, `kernel/release-kernel.txt`). Runtime `oneios-runtime-0.1.19-arm64.tar.zst`, 13,963,784 bytes, SHA-256 `86c3515d710a5dc52ed2707b1e1ccb1f0915623b1e18cb2c4015cb727ef7e860`, `runtime.json` version 0.1.19, commit `f293cd5`, not dirty; `oneios-tool-0.1.19-android-arm64`, 17,906,640 bytes, `b343766a…`. APK `OneOS-oneios-0.5.80-debug.apk` (versionCode 209, fork `6e3e387`), 85,997,849 bytes, SHA-256 `71ce78ef4f12472bee768dd0c5ef2cc65f775668b57357b202e2dbc59bf5cf13`, the shared debug key `8b3be966…`. Checked after it: every asset downloaded from the release against its `.sha256`, `runtime.json`'s version, commit and kernel, the versions line's text in the packed `bin/xnuhost`. Not staged or booted on a phone yet (the Mac boots it) |
| [`v0.1.20`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.20) | 2026-11-23 | `410a6dc` | Published from the build laptop by `scripts/release/release.sh 0.1.20` (under `scripts/bg.sh`) in the main checkout at origin/main (exit 0; runtime, apk and tests lanes ok). The first two runs failed their apk lane on the fork's `OneiosScriptTest.theInnerShellWaitsForXnuhostAndRecordsHowItEnded`: SIGINT ignored in the lane, then in a Gradle daemon left by the first run. That was fixed in `3e2ee15` and `410a6dc` ("Releasing from a build machine"). For the **Galaxy A56** user of issue 73: the app at fork `47c5804` (`oneios`, which has `bd4bbf2`): the A56 path, which makes iOS 16.5 (20F66) with `lib/profiles/ios-16.5-a56.toml` (packed, the same bytes as `profiles/`). Our own 20F66 image, `image apfs-check` and `image verify` reading every compressed file as iOS does (`1b57c1a`, [a56.md](a56.md)). Since v0.1.19 also: drm2kgsl 0010 and the guest's 3D blob sizes (65532 entries, `78c4b86`); the Wi-Fi bridge's ring wait, 60 s traffic line and scans while joined (`cbf97ea`); `ram=auto` counting the iOS tasks, and a lasting host-short-of-memory line (`3b8742a`, merged to main by its helper during this release); pull-log's empty boot (`b05f335`); xnuhost's fatal-signal lines and never a kill of pid 0 (`df485a0`); the app's record of how xnuhost ended (fork `bd4bbf2`). Same kernel as v0.1.18 and v0.1.19 (`xnu-12377.121.6-ios-27.0.1-24A446-c889b14b24f3`, `kernel/release-kernel.txt`), which already has the A56 patches. **Main's later kernel changes are not in it** (0093, the Wi-Fi driver's RSSI event). Runtime `oneios-runtime-0.1.20-arm64.tar.zst`, 14,016,978 bytes, 29 files, SHA-256 `5a72d104d8bae7fb5dfe6e36581df61cbf1a41c1a54f1fd6e690844c87ef8a25`, `runtime.json` version 0.1.20, commit `410a6dc`, not dirty, kernel `c889b14b24f3` (`kernel/xnu.macho` `72b67dff…`); `oneios-tool-0.1.20-android-arm64`, 18,047,600 bytes, `fa036e3b…`. APK `OneOS-oneios-0.5.82-debug.apk` (versionCode 211, fork `47c5804`), 86,036,221 bytes, SHA-256 `353f805dc443c2bd70f4b7225b14c6ccbd7a70a31c81280475700b6995903986`, the shared debug key `8b3be966…`. Checked after it: every asset downloaded from the release against its `.sha256`; `runtime.json`'s version, commit and kernel; `lib/profiles/ios-16.5-a56.toml` in the package. The release notes carry the A56 install steps. **Nothing of it has been booted on a phone yet.** The A56 path is untested on an A56, and touch on the A56 is expected not to work (a56.md, "Not yet proven") |
| [`v0.1.21`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.21) | 2026-11-24 | `943ed26` | Published from the build laptop by `scripts/release/release.sh 0.1.21` in the main checkout at origin/main (exit 0 on the third run). The first run failed in its apk lane, with "SDK location not found" (fixed: `release.sh` exports `ANDROID_HOME`), and in its tests lane, because `encctl decode` failed 1 run in 3 (fixed: the stand-in decoder keeps a picture while every output buffer is taken, `bb41c75`). The second run failed on `cargo fmt` of that test (`943ed26`). For the **Galaxy A56** user of issue 73: the app at fork `85322d8` (`oneios`). The session's read-only remount names the source and the target, because three A56 switches stopped before xnuhost with "`mount: can't find /data/oneios_switch.sh in /proc/mounts`", status 70 ([a56.md](a56.md), "The first runs on an A56"). The two session scripts are checked read-only, and a failed remount prints its mountinfo line. Since v0.1.20, main's runtime also has the integration build of 2026-11-24 (every change behind a switch, [switches.md](switches.md)), **not yet booted on the phone**. Same kernel as v0.1.18 to v0.1.20 (`xnu-12377.121.6-ios-27.0.1-24A446-c889b14b24f3`, `kernel/release-kernel.txt`): main's hosted patches 0093 to 0101 are not in it. Runtime `oneios-runtime-0.1.21-arm64.tar.zst`, 14,133,442 bytes, SHA-256 `3df0bed069195d80e4ae534e1e1d626783bdacd01b8cd435ba27e963120ed99a`; `oneios-tool-0.1.21-android-arm64`, 18,087,056 bytes, `3155ae97…`. APK `OneOS-oneios-0.5.88-debug.apk` (versionCode 217, fork `85322d8`), 86,042,945 bytes, SHA-256 `8b652b1c2accd584ca89880d5b51c0c2618eb4386a6003bf9db27f808318bf38`, the shared debug key `8b3be966…`. Checked after it: the APK downloaded from the release against its `.sha256`, and its `assets/oneios/oneios_switch.sh` has the two-path remount. Not staged or booted on a phone yet |
| [`v0.1.22`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.22) | 2026-11-24 | `a7d059e` | Published from the build laptop by `scripts/release/release.sh 0.1.22` in the main checkout at origin/main (exit 0 on the first run). **A new kernel** for the **Galaxy A56** of issue 73: `xnu-12377.121.6-ios-27.0.1-24A446-7d540ae9e518` ("The kernel of v0.1.22" above). It is v0.1.18's series to 0092 with hosted 0004's `newfs_apfs` run again without `-x` on exit 64. iOS 16.5's `newfs_apfs` has no `-x`, and v0.1.21's A56 boot panicked with "offline scratch Data mount failed: 5" ([a56.md](a56.md)). The S24 Ultra gets the same kernel code as before, apart from that retry. The runtime is the same code as v0.1.21. The app is the same APK as v0.1.21 (`OneOS-oneios-0.5.88-debug.apk`, versionCode 217, fork `85322d8`), built again, 86,042,945 bytes. Runtime `oneios-runtime-0.1.22-arm64.tar.zst`, 14,132,703 bytes, SHA-256 `fe605ac28411ff9b18604dbdd94a9b701b2965d2f5ce58600748bae1c37b771a`, `runtime.json` version 0.1.22, kernel build id `...-7d540ae9e518` (`kernel/xnu.macho` `0a37d06e…`, with the retry's line). Checked after it: the runtime downloaded from the release against its `.sha256`, its `runtime.json` and `kernel/xnu.macho`. The A56 user takes it with Get the runtime, then Update the kernel and Activate the phone-built kernel. Not booted on a phone yet |
| [`v0.1.23`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.23) | 2026-11-24 | `4f59927` | Published from the build laptop by `scripts/release/release.sh 0.1.23 --no-metal` (exit 0 on the first run). **No Metal plug-in in the runtime** (`"metal": null`): the owner has not yet confirmed the licence terms of distributing metal2vulkan (GPL-2.0-or-later / LGPL-3.0-or-later), which `3c2be4e`'s Metal-in-the-runtime would bring. For the **Galaxy A56** of issue 73: the app at fork `976b1fc`: Update the kernel takes the A56's iOS 16.5 (20F66) set, whose kernel.kc is the 24A446 one by design (it refused "the staged image set is 20F66"); the page, the Switch button and the switching line name the staged set's iOS ("iOS 16.5 (OneiOS)"). Since v0.1.22 also: the fork orumanayurpanjayat-ux/OneiOS's scanout row reads (`6a5562d`) and the Metal target preflight with the missing-bundle refusal taken out (`a879630`, `c62f974`). Same kernel as v0.1.22 (`...-7d540ae9e518`). Runtime `oneios-runtime-0.1.23-arm64.tar.zst`, 14,160,291 bytes, SHA-256 `f7c82c11502399a22ba9f946fcd315ff1059b2ac26cc1e4aaddb1d76e2935692`; `oneios-tool-0.1.23-android-arm64`, 18,120,112 bytes. APK `OneOS-oneios-0.5.89-debug.apk` (fork `976b1fc`), 86,043,381 bytes, SHA-256 `87e3d977f3869835d7d5c38ec88f4b7485b9dd0ff400861ad4dcf30046a0649b`, the shared debug key. Checked after it: both downloaded from the release against their `.sha256`, `runtime.json`'s kernel and `metal`, the fix in the APK's dex. Not booted on a phone yet |
| [`v0.1.24`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.24) | 2026-11-24 | `6fa0b67` | Published from the build laptop by `scripts/release/release.sh 0.1.24` (exit 0 on the third run; the first stopped in the runtime lane on `cargo +1.98.0` with /usr/bin/cargo first in PATH, fixed in `fada0b6`; the second in the tests lane on test_hidl_binder's split log lines, fixed in `6fa0b67`). **The first release with the Metal plug-in in the runtime** (`lib/guest/`: NativeAdrenoMetal.bundle, its build record, the capture plug-in, NOTICE; `3c2be4e`): an image made on the phone for iPhone18,2 24A446 gets it. **A new A56 kernel** `xnu-12377.121.6-ios-27.0.1-24A446-16bd44f9d737` (v0.1.22's series plus hosted 0102-0107: touch for iOS 16.5, configd's Wi-Fi grant, the LaunchServices rule, logd's graceful stop, and 0107, iOS 16's port-policy fix for SpringBoard's EXC_GUARD kills; "The kernel of v0.1.24" above is the helper's paragraph). The second A56 port (`e3f2b86`: the 720x1560 @2x frame at 60 Hz from the galaxy-a56 row, the idle jobs, the AIDL vibrator) and the AIDL Bluetooth HAL client (`e449655`). iOS 27 on the A56 is not planned (no GPU path for the Xclipse). App unchanged (`OneOS-oneios-0.5.89-debug.apk`, fork `976b1fc`, SHA-256 `87e3d977f3869835d7d5c38ec88f4b7485b9dd0ff400861ad4dcf30046a0649b`). Runtime `oneios-runtime-0.1.24-arm64.tar.zst`, 23,645,031 bytes, SHA-256 `e548db39547ae55103537c05eb07153b852c5adf809cca0a75a891f47f4957c3`. Checked after it: the runtime downloaded from the release against its `.sha256`, `runtime.json`'s kernel, `lib/guest/`, the 0107 and oneios-hid16 strings in the kernel, the AIDL Bluetooth and iOS 16 boot-argument strings in xnuhost. Not booted on a phone yet |
| [`v0.1.25`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.25) | 2026-11-24 | `66c7cd9` | Published from the build laptop by `scripts/release/release.sh 0.1.25` (exit 0 on the first run), with the Metal plug-in in the runtime. For the **Galaxy A56** (issue 73, the v0.1.24 boot): the A56 kernel `xnu-12377.121.6-ios-27.0.1-24A446-181d8bf2d60a` (hosted 0108 and 0109: an iOS 16 userland's fat binaries run their arm64 slice, webbookmarksd's crash); for a 20F66 image xnuhost patches CoreImage's GL context maker to return none (SpringBoard's pc-0 crash with Bold Text: no GL and no Metal; `ios16-fixup=0` turns it off) and gives `/product` the iPhone X's product-id, a product-description and a made-up serial `ONEIOS16A56` (bluetoothd's chip factory, About's Model and Serial) (`a580aa4`); `oneios-tool sym find` and `sym callers` (`497a609`). App unchanged (0.5.89). Runtime `oneios-runtime-0.1.25-arm64.tar.zst`, 23,738,820 bytes, SHA-256 `1711a5d7c90862a0dcd5518f24be2b151653f8dbfa7d48e35bfa328142dd9bd7`. Checked after it: the runtime from the release against its `.sha256`, `runtime.json`'s kernel, `lib/guest/`, the 0109 string in the kernel and the 20F66 fixup and serial strings in xnuhost. Not booted on a phone yet |


Every release above holds the runtime package only (the old `release.yml`); the fork's APK was published on the
fork's own prereleases `oneios-<version>`. The first tag after the one workflow above is the first release with
the APK beside the runtime.

Checked after the workflow ran: the asset's SHA-256 against its `.sha256`, `runtime.json` first, regular files only.

## Cutting a release

```sh
git pull --rebase origin main
runtime/package/elf-check-test.sh && runtime/package/test.sh && kernel/tests/host.sh   # the tests CI runs
runtime/host-tests.sh                                  # the runtime's other host tests (Apple's container on a Mac)
runtime/package.sh 0.1.0                               # optional: look at out/release/*.runtime.json
git tag -a v0.1.0 -m "OneiOS runtime 0.1.0" && git push origin v0.1.0
# or, without a tag of your own: Actions, "build and release", Run workflow on main, with the version and what to build
gh workflow run release.yml --ref main -f version=0.1.0 -f build=both      # "runtime only", "apk only"
```

The workflow does the rest: the runtime from this commit, the APK from the head of the fork's branch `oneios`
(not from the commit recorded for `app/`). Check the release page for the four files (two with "runtime only" or
"apk only") and the commits in the notes, then tell the oneos agent the tag (`docs/agents.md`). To redo a broken
release: delete the release and the tag, fix, tag again; never replace an asset under the same version once the
fork may have downloaded it (the workflow refuses to: an asset with other bytes under the same name stops it).

### Releasing from a build machine

`scripts/release/release.sh` builds the same files on this machine and publishes them, without waiting for the
runners. It runs three lanes in parallel, each with its log in `out/release-local/VERSION/logs/`:
- **runtime**: the Metal plug-in and the capture plug-in built from HEAD (`userland/metal/build.sh` with
  `METAL_OUT=out/release-local/VERSION/guest`, its work in `out/metal`; `userland/camera/build.sh`), then
  `runtime/package.sh` with them (`METAL_DIR`, `CAMERA_DIR`, `REQUIRE_METAL=1`) and the kernel pinned in
  `kernel/release-kernel.txt` (downloaded and checked as CI does, cached in `out/release-local/kernel/`), then CI's
  package checks (drm2kgsl, `elf-check.sh`, the members, `check-phone-tool.sh`) and the bundle in runtime.json
  (its target and CDHashes go into the notes);
- **apk**: `app/` as it is checked out, signed with the debug key below, then `check-apk.sh`, plus a check that
  the APK's signer is that key;
- **tests**: `check-tracked-files.sh`, `bash -n`, shellcheck when it is installed, and the host tests.

When all three pass, it checks every `.sha256` and writes the same notes as CI. It then creates the release as a
draft with every asset and makes it public, which creates the tag at HEAD. The tag's CI run sees the release
already exists (plan job), so it builds and tests again and uploads nothing.

```sh
git pull --rebase origin main            # HEAD must be on origin/main, and app/'s commit on the fork's origin/oneios
scripts/bg.sh release scripts/release/release.sh 0.1.14 --build-only   # build and check only: out/release-local/0.1.14/files
scripts/bg.sh release scripts/release/release.sh 0.1.14                # build, check and publish v0.1.14
scripts/bg.sh -s release                                               # its state and log (logs/release.log)
```

The script exports `ANDROID_HOME` as the SDK it found, `~/Android/Sdk` included. Gradle reads the SDK from that
variable or from `local.properties` only. A run under tmux started from ssh had neither, and v0.1.21's first run
failed in the guest's build with "SDK location not found". It also puts `~/.cargo/bin` first in `PATH` when rustup
is there: the Metal bridge's build runs `cargo +1.98.0`, which only rustup's proxy takes, and under tmux from ssh
`/usr/bin/cargo` came first (v0.1.24's first run: "error: no such command: `+1.98.0`" in the runtime lane).

Options:
- `--runtime-only` and `--apk-only` are CI's "runtime only" and "apk only"; an APK-only release is not marked
  latest.
- `--skip-tests` leaves out the tests lane.
- `--metal-dir DIR` packs a Metal plug-in built before (`userland/metal/build.sh`'s output folder; `DIR/camera/` with
  `H9ISP.mediacapture` packs the capture plug-in too) instead of building one; `--no-metal` makes a runtime without
  them, and the notes say that an image made on the phone with it composites on the CPU. The Metal build needs the
  iOS cross toolchain and the guest's stubs ([building.md](building.md) section 9: `scripts/setup/ios-toolchain.sh`
  says "ok"); on the build laptop it took 7 minutes on 2026-11-24 with `out/metal` from an earlier build. A runtime
  that CI builds (`release.yml`) has no `METAL_DIR`, so no Metal plug-in: publish runtimes from the build machine.

The script refuses:
- uncommitted changes to tracked files, in OneiOS or in `app/`;
- a HEAD that is not on `origin/main`, or an `app/` commit that is not on the fork's `origin/oneios`;
- a tag or release that already exists.

Notes on the build:
- Unlike CI, the APK is built from the commit checked out in `app/`, not from the head of `oneios`. `git -C app
  pull` first if you want the branch head.
- The lanes start with job control on (`set -m`) and stdin from `/dev/null`. A plain background command of a script
  has SIGINT and SIGQUIT ignored, and every process of the lane inherits that. The first v0.1.20 run (2026-11-23)
  failed its apk lane on that: the fork's `OneiosScriptTest.theInnerShellWaitsForXnuhostAndRecordsHowItEnded` (since
  fork `bd4bbf2`) checks that xnuhost's stand-in does not ignore them, and it saw `SigIgn 0x2`. The same test passes
  in a foreground `./gradlew test` and in CI. The apk lane's Gradle runs with `--no-daemon` too: the second v0.1.20
  run (with `set -m`) failed the same way, because it reused the Gradle daemon the first run had started with SIGINT
  ignored (pid 732505, `SigIgn 0x2`), and a daemon's test workers inherit its ignored signals.
- Tested on the Linux PC (20 cores) on 2026-11-22 with `scripts/release/release.sh 0.1.14 --build-only` at `e1b55be`,
  `app/` at `5ffc71c`. All three lanes passed in 240 s, with Gradle's and cargo's caches warm. The files: the runtime
  package (12,938,612 bytes, 26 members, kernel in), `oneios-tool-0.1.14-android-arm64`, and
  `OneOS-oneios-0.5.49-debug.apk` (no `.py`, signer `8b3be966…`, the key below). The publish was tried with v0.1.14 (below, "Released").

### The debug key

Every OneOS APK in a release is signed with one debug key, `secrets/oneios-debug.keystore`. It was made on the Linux
build machine on 2026-11-22 and copied to the Mac's checkout (the build host now) on 2026-11-23, certificate checked
with `keytool -list -v`. CI: until fork `2ef79b6` (2026-11-23) the fork's runs did not sign with it (0.5.51 and 0.5.52
had keys of their own), the fork's `docs/oneios.md` "CI".
- **The key:** PKCS12, RSA 2048, valid for 10,000 days; alias `androiddebugkey`, store and key password `android`,
  as Android's own debug keys. Certificate SHA-256
  `8b3be966893342e8ca63cbb9b2097e038cfb30c7bccb1296c2baa25654217599`.
- **Where it lives:**
  - `secrets/` is git-ignored, so the key is never committed.
  - The same bytes, base64, are the secret `ONEIOS_DEBUG_KEYSTORE_B64` of akshaynexus/OneiOS and akshaynexus/OneOS.
  - CI writes the secret to `~/.android/debug.keystore`. `scripts/release/release.sh` passes the file to Gradle
    (`-Pandroid.injected.signing.*`). Since fork `bc837b4` every debug build in this checkout (`./gradlew assembleFullDebug`,
    `oneios-tool app build`, `cycle`) signs with it too: the fork's `app/build.gradle.kts` takes the file when it is there.
- **What it gives:** an APK from CI and one from the build machine install over each other.
- **The first APK signed with it** has to replace one signed with an older key by uninstalling it. That applies to
  v0.1.13's 0.5.47 and every earlier release, each signed with a run's own key.
- **Keep a copy:** GitHub does not give a secret back. If `secrets/oneios-debug.keystore` is lost, the next key
  means one more uninstall. To set the secret again from the file (for example after making a new key):

```sh
for r in akshaynexus/OneiOS akshaynexus/OneOS; do base64 -w0 secrets/oneios-debug.keystore | gh secret set ONEIOS_DEBUG_KEYSTORE_B64 -R "$r"; done
```
