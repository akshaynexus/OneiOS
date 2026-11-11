# Releases

Owner: release-kernel. Scripts: `runtime/package.sh`, `runtime/package/`, `.github/workflows/`, `kernel/publish.sh`.

## What a release is

One asset and its checksum on a GitHub release of `akshaynexus/OneiOS`:

```
oneios-runtime-<version>-arm64.tar.zst
oneios-runtime-<version>-arm64.tar.zst.sha256      one sha256sum line: "<64 hex>  <file name>"
```

The format is the one the OneOS fork reads (`oneos/interface.md` section 1): a zstd frame (level 19, 8 MiB
window) around a ustar archive of regular files, `runtime.json` first, no top folder.

| In the package | What |
|---|---|
| `runtime.json` | manifest, below |
| `bin/xnuhost`, other `bin/*` | the booter and its helpers, arm64 Android, built by `runtime/build.sh` with NDK 28.2.13676358. Until that script exists: a 968-byte placeholder that prints `xnuhost: not built yet ...` and exits 2 |
| `bin/gpuctl`, `bin/bridgectl`, `bin/brightctl`, `bin/batteryctl`, `bin/displayctl`, `bin/camctl` | the phone checks, each a tool of its own that runs as root with One UI up (`docs/gpu.md`, `runtime/bridges/README.md`, `runtime/bridges/brightness/README.md`, `runtime/bridges/battery/README.md`, `runtime/display/README.md`, `runtime/bridges/camera/README.md`). The fork's **Hardware checks** screen runs them from the staged runtime (`oneios_root.sh check <id>`, the fork's `docs/oneios.md`) |
| `scripts/*` | `runtime/scripts/*`, when there are any |
| `kernel/xnu.macho`, `kernel/xnu.macho.build.json` | the XNU that `kernel/` builds from Apple's open source, and its build record. Only when a kernel was built |
| `NOTICE`, `LICENSE` | licences; with a kernel, the APSL notice (`kernel/NOTICE.kernel`) |

## What is never in a release

Nothing of Apple's: no IPSW, no kernelcache, no cryptex or disk image, no trust cache, no kext, and no kernel
collection (the source-built XNU prelinked with Apple's kexts). Those are the **image set**, which each owner
builds on their own machine from Apple's restore files (`scripts/image/`), and which the app imports from a file.

`runtime/package/pack.py` enforces this on every package, and `runtime/package/test.sh` tests it:

- by name: `kernelcache*`, `*cryptex*`, `.ipsw`, `.dmg`, `.aea`, `.img`, `.im4p`, `.img4`, `.kc`, `.trustcache`,
  `BuildManifest.plist` and the rest of `.gitignore`'s firmware names;
- by content: IMG4/IM4P containers, AEA archives, disk images, dyld caches, and **every Mach-O**;
- the one Mach-O allowed is our own XNU: an arm64 `MH_EXECUTE` with a build record next to it
  (`<name>.build.json`) written by `kernel/build-record.py`: `builder` is `oneios-kernel`, `sha256` is the file's,
  `prelinked` and `kernelcache_kexts_added` are false, `apple_binary_inputs` is empty. A kernel collection
  (`MH_FILESET`) is refused with or without a record.

A refusal ends with exit code 3 and no package. CI also refuses firmware-like names, Mach-O files and files over
5 MiB in git (`.github/scripts/check-tracked-files.sh`).

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
- 0.x while nothing boots. The minor number goes up when the session ABI, the image format or the kernel-to-booter
  interface changes; the patch number for everything else.
- `session_abi` and `image_formats` are what the fork checks at run time. A package with a new `session_abi` needs a
  new fork: raise `runtime/package/min-oneos-fork` in the same commit.
- The kernel has its own identity, the **build id** `xnu-<xnu version>-<profile>-<first 12 hex of the series hash>`
  (`kernel/build-record.py`). The same sources, patches and configuration give the same id on any machine.

## How the OneOS fork consumes it

1. Takes the latest release of `akshaynexus/OneiOS` (or a pinned tag), downloads the `.tar.zst` and the `.sha256`.
2. Checks the SHA-256 of the archive, unpacks it, and checks every file against `runtime.json` (size, SHA-256, mode);
   a file that is not listed, or a listed file that is missing, refuses the whole package.
3. Stages it as `/data/local/tmp/oneios/runtime/` and starts `runtime.json`'s `xnuhost` with the command line of
   `oneos/interface.md` section 4. With the placeholder the start ends at once with exit code 2 and the fork returns
   to One UI: that is the download-and-verify test.
4. "I have the file" in the app takes the same `.tar.zst` from storage (a build before a release: `runtime/package.sh
   0.1.0-dev`, then `adb push`).

## CI

- `ci.yml` (pull requests, pushes to `main`), `ubuntu-24.04`: no firmware in git; `bash -n`, `py_compile` and
  `shellcheck -S error`; NDK 28.2.13676358 through the SDK's `sdkmanager` and the `aarch64-linux-android` Rust
  target; the host tests (`runtime/package/test.sh`, `kernel/tests/host.sh`, and any executable `*/tests/host.sh` of
  another area); `runtime/package.sh 0.0.0-ci`, kept as an artifact for 14 days.
- `release.yml` (a tag `v*`): checks that the tag is a version and its commit is on `main`, runs the tests, packages
  and uploads the asset and its `.sha256` to the release of the tag (created if missing; a pre-release when the
  version has a suffix).
- Neither downloads anything of Apple's. Actions are pinned by commit.

**The 3D GPU in a release.** `release.yml` fetches Droid-VM's virglrenderer (`scripts/setup/get-references.sh virglrenderer`) before packaging, and refuses to publish a package whose `xnuhost` or `gpuctl` was built without drm2kgsl. Without that checkout both build 2D-only and only say so at run time: runtime 0.1.1 was published that way and its GPU check failed on the phone with "this build has no drm2kgsl". 0.1.3 is the first release with the check (0.1.2's run failed to link: a stale 2D-only `gpu3d.o` from the host tests beside the real drm2kgsl; the fetch now runs before the host tests and `gpu3d.o` rebuilds when the drm2kgsl setting changes).

## The kernel in a release

The kernel is not built in the release job. A build machine builds it (`kernel/README.md`), and:

1. `kernel/publish.sh` packs `out/kernel/current/` (`xnu.macho`, its build record, the series report) as
   `xnu-<build id>.tar.zst` and uploads it to the release `kernel-<build id>` of this repository. It runs the same
   Apple-file check first.
2. `kernel/release-kernel.txt` gets one line, `kernel-<build id> <sha256 of the archive>`, in a commit.
3. The release job downloads that archive, checks the SHA-256, and `runtime/package.sh` checks the build record
   again before the kernel goes into the package.

Without `kernel/release-kernel.txt` the release is runtime-only (`"kernel": null`).

## Released

| Tag | Date | Commit | What |
|---|---|---|---|
| [`v0.1.0`](https://github.com/akshaynexus/OneiOS/releases/tag/v0.1.0) | 2026-11-10 | `ba306c5` | The first release, runtime only (`"kernel": null`, `"placeholder": false`): `bin/xnuhost`, the helpers (`oneios-audio`, `oneios-net.sh`, `oneios-camera`, `oneios-display` with `liboneios-display.so` and `lib/oneios-display.dex`) and the five phone checks (`gpuctl`, `bridgectl`, `brightctl`, `displayctl`, `camctl`). 13 files, 502,337 bytes packed, `min_oneos_fork` 0.4.74. Made for the fork's Hardware checks screen (fork 0.4.75). Nothing in it has run on the phone; without an image set and a kernel collection its xnuhost boots nothing |

Checked after the workflow ran: the asset's SHA-256 against its `.sha256`, `runtime.json` first, regular files only.

## Cutting a release

```sh
git pull --rebase origin main
runtime/package/test.sh && kernel/tests/host.sh        # the tests CI runs
runtime/package.sh 0.1.0                               # optional: look at out/release/*.runtime.json
git tag -a v0.1.0 -m "OneiOS runtime 0.1.0" && git push origin v0.1.0
```

The `release` workflow does the rest. Check the release page for the two files, then tell the oneos agent the tag
(`docs/agents.md`). To redo a broken release: delete the release and the tag, fix, tag again; never replace an
asset under the same version once the fork may have downloaded it.
