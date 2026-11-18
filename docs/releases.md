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
| `bin/xnuhost`, other `bin/*` | the booter and its helpers, arm64 Android, built by `runtime/build.sh` with NDK 28.2.13676358. Until that script exists: a 968-byte placeholder that prints `xnuhost: not built yet ...` and exits 2 |
| `bin/gpuctl`, `bin/bridgectl`, `bin/wifictl`, `bin/brightctl`, `bin/batteryctl`, `bin/displayctl`, `bin/camctl` | the phone checks, each a tool of its own that runs as root with One UI up (`docs/gpu.md`, `runtime/bridges/README.md`, `runtime/bridges/brightness/README.md`, `runtime/bridges/battery/README.md`, `runtime/display/README.md`, `runtime/bridges/camera/README.md`). The fork's **Hardware checks** screen runs them from the staged runtime (`oneios_root.sh check <id>`, the fork's `docs/oneios.md`) |
| `bin/oneios-tool` | the Rust tools (`tools/oneios-tool`) for `aarch64-linux-android`, linked for API 30 by the NDK's clang and stripped (`runtime/package.sh` step 1b, [building.md](building.md) section 4). The OneOS app runs it as root from the staged runtime, one `oneios-tool serve` per flow (`oneios_root.sh tool`, [../oneos/interface.md](../oneos/interface.md) section 9) |
| `lib/profiles/*.toml` | this repository's IPSW profiles (`profiles/`: the members' names, sizes and hashes, no Apple file), for the phone's `oneios-tool serve` `fetch`: the OneOS app's Make iOS image fetches the IPSW's members with `$STAGE/runtime/lib/profiles/ios-27.0.1-iPhone18-2.toml` (a package path has no comma; `runtime/package.sh` step 1c) |
| `scripts/*` | `runtime/scripts/*`, when there are any |
| `kernel/xnu.macho`, `kernel/xnu.macho.build.json` | the XNU that `kernel/` builds from Apple's open source, and its build record. Only when a kernel was built |
| `NOTICE`, `LICENSE` | licences; with a kernel, the APSL notice (`kernel/NOTICE.kernel`) |

## What is never in a release

Nothing of Apple's is put on a GitHub release: no IPSW, no kernelcache, no cryptex or disk image, no trust cache, no
kext, and no kernel collection (the source-built XNU prelinked with Apple's kexts). Those are the **image set**, which
the owner builds from Apple's restore files (`scripts/image/`) and imports into the app from a file. The optional CI
image job uploads only an age-encrypted, one-day Actions artifact; see [building.md](building.md#ci-full-image-set).

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
`runtime/package/test.sh` makes such a library (`runtime/package/elf-fault.c`, linked `-landroid -lm` without
`-z defs`), puts it in the display helper's place and expects the refusal. The check reads the files only: what it
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
placeholder), `kernel` (null without one). `files` lists every file except `runtime.json`.

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
`darwin-x86_64` tools (`runtime/host.mk`). macOS's `tar` is bsdtar, so the archive is written by
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

One workflow, `.github/workflows/release.yml` ("build and release"; `ci.yml` and the runtime-only `release.yml` of
before 2026-11-15 were merged into it). Its jobs:

```
plan ──┬── runtime    ubuntu-24.04      the runtime package and its checks
       ├── apk        ubuntu-24.04      the OneOS app (submodule app/) and its checks
       ├── apk-macos  macos-15          ./gradlew test of the same app on a Mac
       ├── image      self-hosted Linux the full image set; manual only; encrypted artifact
       └───────────── release           needs runtime and apk; a tag or a release dispatch only
```

`runtime`, `apk` and `apk-macos` run on their own runners after `plan`; the image job starts only for its manual choices.

| Trigger | Builds | Releases |
|---|---|---|
| a pull request, a push to `main` | runtime, apk, apk-macos | nothing (the runtime package and the APK are artifacts for 14 days) |
| a tag `vN.N.N` or `vN.N.N-suffix` on `main` | all three | the runtime and the APK on the release of the tag |
| a dispatch on `main` with `both` / `runtime only` / `apk only` | what was asked (apk-macos with the APK) | the release `v<version>`, its tag made at the dispatched commit when missing |
| a dispatch on `main` with `image set` | fetch and verify the full image set and hosted kernel; age-encrypted artifact, one day | nothing |
| a dispatch on `main` with `full build` | runtime, APK, and the full image set | runtime and APK only; the image set remains an encrypted Actions artifact |

- **plan**: whether the run releases, the version (`0.0.0-ci` when not), what it builds. A release's version must be
  `N.N.N[-suffix]` and its commit on `main`.
- **runtime**: what `ci.yml` and the old `release.yml` did, on every build. `app/` must be an empty submodule
  folder (this job never checks the fork out); no firmware in git (`check-tracked-files.sh`); `bash -n`,
  `py_compile`, `shellcheck -S error`; NDK 28.2.13676358, the `aarch64-linux-android` Rust target and
  **oneios-tool** (`.github/scripts/setup-build.sh`: a current stable Rust, `cargo build --release --manifest-path
  tools/oneios-tool/Cargo.toml`, a few minutes); Droid-VM's virglrenderer (below); the host tests
  (`runtime/package/elf-check-test.sh`: the link rule of shared objects and the NEEDED and symbol check, above;
  `runtime/package/test.sh`; `kernel/tests/host.sh`, which runs `kernel series-check` and `kernel build-record
  --self-test`; every executable `*/tests/host.sh` of another area); the pinned kernel when
  `kernel/release-kernel.txt` names one; `runtime/package.sh <version>` (the Apple-file refusal and the elf-check of
  the stage, and `bin/oneios-tool` built for the phone); the drm2kgsl check and `runtime/package/elf-check.sh` on
  the unpacked archive; the members check (above); `.github/scripts/check-phone-tool.sh <version>` ("oneios-tool for
  the phone", below). Artifact `oneios-runtime`: the `.tar.zst`, its `.sha256`, `.runtime.json`, and
  `oneios-tool-<version>-android-arm64` with its `.sha256`. Its limit is 45 min (the tool's Android build comes on
  top of the host one).
- **apk**: what the fork's `.github/workflows/oneios-apk.yml` does, from OneiOS. `git submodule update --init
  --remote app` (the head of the branch `.gitmodules` names, `oneios`; the commit recorded in OneiOS is printed beside
  it); the token for tribalfs' packages (below); JDK 21 (Temurin); Gradle (`setup-gradle`, its open-source `basic`
  cache); Android SDK platform 37 and build-tools 37.0.0; the debug key of the secret `ONEIOS_DEBUG_KEYSTORE_B64`
  when this repository has it (else a key of the run's own); `app/scripts/build-guest-manager.sh --debug`; mksh;
  `./gradlew --max-workers=3 test assembleFullDebug` in `app/app-android` (what `oneios-tool app build` runs); the
  APK's versionName and versionCode from `aapt2 dump badging` (the versionCode must be `app/build.gradle.kts`'s);
  `check-apk.sh` (above). Artifact `oneos-apk`: `OneOS-oneios-<versionName>-debug.apk` and its `.sha256`.
- **apk-macos**: the same checkout, token, JDK, SDK and guest app on `macos-15`, then `./gradlew --continue test`
  with Homebrew's folders taken off `PATH`, so that core:root's script tests use the harness's stand-ins over
  macOS's own `stat`, `shasum`, `perl` and `awk` (a Mac without coreutils). Every test's start and end is in the
  log (`--init-script .github/scripts/gradle-test-events.gradle`, failures with their whole exception). A watchdog
  stops Gradle after 30 minutes and first prints the processes and the test JVMs' threads (`jstack`), so a hung
  test is named and the reports still come out. On a failure the job summary lists the failed tests; the reports
  and results are the artifact `oneos-tests-macos`. Not a gate of the release. The macOS results are in
  "macOS: the fork's unit tests" below.
- **image**: manual `image set` or `full build` only, and only from `main`. It runs on a self-hosted Linux x86_64
  runner (label `oneios-image` by default; 16 GiB RAM, 100 GiB free disk, clang 23+, FUSE and passwordless `sudo`).
  It fetches the pinned Apple restore members, builds the hosted kernel and audited kernel collection from that
  kernelcache, runs the normal image hook, packs and verifies the image archive, encrypts it to the dispatch's age
  public recipient, then uploads only the encrypted file, checksums, and build receipt for one day. A cleanup step
  removes this run's plaintext inputs, image set, and kernel collection even when an earlier step fails. No private
  age key is stored in Actions. The image build requires 30 GiB for restore files and decrypted images; 100 GiB free
  disk leaves room for the kernel build and archive. The standard GitHub Linux runner's 14 GB disk cannot run this
  job. An approximately 11 GiB artifact can exceed the account's included Actions storage and incur storage or
  transfer charges; see [building.md](building.md#ci-full-image-set) for key setup and decryption.
- **release**: needs runtime and apk (each a success, or skipped because the dispatch asked for the other only); if the
  dispatch also requested the image, that job must succeed before a release can proceed.
  Checks the commit is on `main` and that the tag, when it exists, is at the built commit (a tag is never moved);
  downloads the artifacts, checks their `.sha256` (the runtime's and the phone tool's; and that the APK is the file
  the apk job checked), writes the
  notes, creates the release (a pre-release when the version has a suffix; `--latest=false` without a runtime), or
  adds this run's notes to an existing one. Assets are added, never replaced: an asset already on the release with
  the same SHA-256 (GitHub's `digest`) is left, one with other bytes stops the job.
- The regular CI and release jobs do not download Apple files; only the manual image job does. Actions are pinned by
  commit. A rerun needs Actions write, which the agents' token may not have; a push starts a new run.

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

The kernel is not built in CI. A build machine builds it (`kernel/README.md`), and:

1. `kernel/publish.sh` packs `out/kernel/current/` (`xnu.macho`, its build record, the series report) as
   `xnu-<build id>.tar.zst` and uploads it to the release `kernel-<build id>` of this repository. It runs the same
   Apple-file check first.
2. `kernel/release-kernel.txt` gets one line, `kernel-<build id> <sha256 of the archive>`, in a commit.
3. The runtime job (of every build, not only a release) downloads that archive, checks the SHA-256, and `runtime/package.sh` checks the build record
   again before the kernel goes into the package.

Without `kernel/release-kernel.txt` the runtime package has no kernel (`"kernel": null`).

## Released

| Tag | Date | Commit | What |
|---|---|---|---|
| [`v0.1.0`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.0) | 2026-11-10 | `ba306c5` | The first release, runtime only (`"kernel": null`, `"placeholder": false`): `bin/xnuhost`, the helpers (`oneios-audio`, `oneios-net.sh`, `oneios-camera`, `oneios-display` with `liboneios-display.so` and `lib/oneios-display.dex`) and the five phone checks (`gpuctl`, `bridgectl`, `brightctl`, `displayctl`, `camctl`). 13 files, 502,337 bytes packed, `min_oneos_fork` 0.4.74. Made for the fork's Hardware checks screen (fork 0.4.75). Nothing in it has run on the phone; without an image set and a kernel collection its xnuhost boots nothing |
| [`v0.1.7`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.7) | 2026-11-12 | `8067cea` | Runtime only (`"kernel": null`): `bin/xnuhost` (deferred unmaps, `ram=auto`, the display helper's socket kept on transient send errors), the helpers and the eight phone checks (`thermalctl` and `batteryctl` beside the five of 0.1.0, and `wifictl`). 17 files, 745,405 bytes packed, `min_oneos_fork` 0.4.74. Tests before the tag on the Mac: `runtime/package/test.sh` 19 pass, `kernel/tests/host.sh` 5 pass; the release run (43269875806) passed the host tests and the drm2kgsl check. Staged on the phone with `oneios-tool phone stage runtime` from the downloaded asset (SHA-256 checked); `oneios-tool phone sums` equals every file. The first release staged on the phone; 0.1.1-0.1.6 were published (not listed here) and the phone ran local `-dev` builds until now. Not booted yet |
| [`v0.1.8`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.8) | 2026-11-12 | `ea06d46` | Runtime only (`"kernel": null`, `"placeholder": false`): 17 files, 745,567 bytes packed, `min_oneos_fork` 0.4.74. Release run 43277047626 |
| [`v0.1.9`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.9) | 2026-11-15 | `8644ae9` | Runtime only (`"kernel": null`): xnuhost and the bridges of #26-#42 (locked RAM by default, the hosted CLPC signals, the GPU level by deadline and per-context priority, the host-overhead fixes, the Wi-Fi scan reuse, the S Pen (`pen=auto`), `hw-backend=samsung|treble`). 17 files, 805,244 bytes packed, `min_oneos_fork` 0.4.74. Tests before the tag on the Mac: `elf-check-test.sh`, `runtime/package/test.sh`, `kernel/tests/host.sh` 5 of 5; the release run 43703261330 passed. Checked after it: the asset against its `.sha256`, `runtime.json` first. Not staged or booted on the phone yet |

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
