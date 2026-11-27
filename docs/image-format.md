# The OneiOS image set (format 1)

Owner: image-build. Code: `profiles/`, `scripts/fetch/`, `scripts/image/`. Tests: `cargo test -p oneios-image -p oneios-imagebuild -p oneios-tweaks` (section 9).

The image set is to OneiOS what `system.raw` plus its overlays are to OneOS: the guest system, built on the
owner's machine from the vendor's firmware, pinned by size and SHA-256, staged in a folder on the phone. For
OneOS the firmware is an Android OTA and the builder is MysticGSI. Here it is Apple's restore file (IPSW) for
iPhone18,2, iOS 27.0.1 (24A446), and the builder is `scripts/image/build.sh`, or the same steps in one process,
`oneios-tool image build` (`serve`'s `make-image`, which the OneOS app runs on the phone: no PC; since 2026-11-21,
with the root tweaks and the AEA decryption in it; tools/oneios-tool/README.md "image build"). Both give the same
set from the same inputs, file for file.

Nothing of the set is in git or in a release: it is Apple's software. A release carries the scripts; each
owner fetches from Apple and builds.

```sh
scripts/fetch/fetch.sh --list 'profiles/ios-27.0.1-iPhone18,2.toml'      # compare the profile with Apple's file, about 1 MiB
scripts/bg.sh fetch scripts/fetch/fetch.sh 'profiles/ios-27.0.1-iPhone18,2.toml'    # 10.6 GiB -> inputs/24A446/
scripts/bg.sh image scripts/image/build.sh 'profiles/ios-27.0.1-iPhone18,2.toml'    # -> out/image/24A446/ and the .tar.zst
scripts/image/verify.sh out/image/OneiOS_iOS27.0.1_24A446_iPhone18-2.tar.zst
```

## 1. What is in a set

`out/image/<build>/`, and the same paths inside the archive and under `/data/local/tmp/oneios/image/` on the phone:

| Path | What | Format |
| --- | --- | --- |
| `manifest.json` | the set's description; every other file's size and SHA-256 | section 3 |
| `os.img` | the OS volume (`OS`) | raw APFS container, read-only |
| `systemos.img` | the SystemOS cryptex (`Cryptex1,SystemOS`): the dyld shared cache, `/System/Cryptexes/OS` | raw APFS container, read-only |
| `app.img` | the AppOS cryptex (`Cryptex1,AppOS`): Safari's daemons, `/System/Cryptexes/App` | raw APFS container, read-only |
| `os.img.hashes`, `systemos.img.hashes`, `app.img.hashes`, `kernel.kc.hashes` | SHA-256 of each 1 MiB chunk of the file | section 4 |
| `trust/static.trustcache`, `trust/system.trustcache`, `trust/app.trustcache` | the trust caches of the three volumes (`StaticTrustCache`, `Cryptex1,SystemTrustCache`, `Cryptex1,AppTrustCache`) | IM4P, as shipped |
| `trust/os.root_hash`, `trust/system.root_hash`, `trust/app.root_hash` | the sealed volumes' root hashes | as shipped |
| `data.seed` | what the guest's Data volume starts from | section 5 |
| `kernel.kc` | the kernel collection: OneiOS's XNU with Apple's kexts prelinked. Made by the kernel build (`kernel/`), carried here because the kexts are Apple's | the kernel build's |
| `overlay/...` | files the userland hook adds for the runtime to hand to the guest (owner trust bundle, Metal plugin, tools). This project's own files | plain files |

### The read-only images: raw APFS, unchanged

`os.img`, `systemos.img` and `app.img` are the IPSW's disk images **byte for byte** (the OS and SystemOS
ones after AEA decryption; iOS 16's OS image, a UDIF disk image, as the bytes of its one `Apple_APFS` partition:
`oneios-tool image udif-raw`, below). Each is a raw APFS container: the container superblock (`NXSB`) is at offset 0,
with no partition table and no UDIF wrapper. The guest kernel mounts them with Apple's own APFS kext through
the booter's block device, read-only.

This is the plan's choice (section 4: UniXNU's release layout, `os.img` and `systemos.img`), kept for these reasons:

- The guest kernel already has the file system (the APFS kext is on its root list). Any other format
  (EROFS, squashfs, HFS+ as iOSPlus10 uses on phones without APFS) needs a file system XNU does not have, or
  a rebuilt tree that no longer matches Apple's seal and trust caches.
- The images are sealed volumes with Apple's root hashes. Unchanged bytes keep the seal, so the booter can
  use UniXNU's verified-root scheme as it is.
- Unchanged bytes are the same for everyone who builds from the same IPSW, so one size and SHA-256 per build
  pin them, as `Images.kt` pins a MysticGSI image.
- arm64e binaries and the dyld shared cache stay as Apple built them; the S24 Ultra's cores have PAC. No
  rewrite step.

`manifest.json` records for each: the container's offset in the file (0), block size, container UUID and
each volume's name, UUID, role, case sensitivity and seal flag, read from the APFS structures at build time.
The kernel build pins the OS volume's UUID (the root is matched by it).

Checked against Apple's files of 24A446 (fetched and decrypted on the Linux PC for the phone's set of 2026-11-11 and again
on the Mac on 2026-11-12, the same SHA-256 both times): `os.img` 10,359,930,880 bytes (`bba6accc...`), `systemos.img`
6,157,238,272 (`a41d72a6...`), `app.img` 16,777,216 (`37f1a44e...`, volume `AppCryptex`
`27C33F9D-750F-4413-8E83-25E83EA25A6F`); each is raw APFS. `build.sh` stops with a clear line if an image is
anything else (a UDIF image has to be converted to raw first: a profile marks such a member `udif = true`, section 2).

**iOS 16.5 (20F66), the Galaxy A56's set** (`profiles/ios-16.5-a56.toml`, docs/a56.md; built on the Linux PC 2026-11-23): the
iPhone X restore file's OS image `096-04088-077.dmg` (4,728,546,953 bytes) is UDIF: a GPT disk of six partitions, its
`Apple_APFS` partition 10,993,664 sectors in 3,628 chunks (stored, LZFSE and ignored). `image udif-raw` makes
`os.img` of it: 5,628,755,968 bytes, `9f430a58...`, volume `SydneyF20F66.D22D221OS` (apfs-info: role system, not
sealed by its incompatible-features flag); the partition table's CRC-32 `3abcd8a4` is checked, and the SHA-256 is the
profile's `plain_sha256`. Its SystemOS (2,726,297,600 bytes) and AppOS (10,485,760) images are raw APFS as shipped. Every
file of the set is from that one restore file except `kernel.kc` (24A446's kexts: docs/a56.md).

## 2. Profile

`profiles/<name>.toml` pins one build, as MysticGSI's patch sets and UniXNU's `profiles/os/*/profile.toml` do.

| Table | Keys |
| --- | --- |
| top | `schema = 1` |
| `[os]` | `name`, `version`, `build`, `device`, `board`, `chip_id`, `board_id`, `arch` |
| `[ipsw]` | `url` (Apple's CDN), `size`, `sha256` (whole file), `metadata` (where the URL came from), `variant` (the BuildManifest identity), `need_gib` |
| `[[member]]` | `role`, `component` (BuildManifest key), `path` (ZIP member), `size`, `crc32`, then `sha384` or `sha256`, or `aea = true`; `udif = true` (a volume whose `.dmg` is a UDIF disk image) with `plain_sha256` |
| `[image]` | `format = 1`, `id`, `stage`, `archive`, `chunk`, `seed`, `kernel` |
| `[image.files]` | role -> path in the set |
| `[data]` | `volume_uuid`, `volume_name`, `store_gib`, `source`, `epoch` |

How a member is pinned:

- `size` and `crc32` come from the IPSW's ZIP directory and are checked for every member.
- A member that is not AEA carries `sha384`: the `Digest` of its component in BuildManifest.plist, which is
  the SHA-384 of the file as shipped (checked on all nine such members of 24A446). `BuildManifest.plist`
  and `Restore.plist` carry `sha256`.
- A UDIF member (`udif = true`, since 2026-11-23: iOS 16's OS image) carries `sha384` (the BuildManifest digest of the
  `.dmg`, which the fetch checks) and `plain_sha256` (of the raw APFS image `image udif-raw` makes of it). `image build`
  and `make-image` convert it in their `decrypt` step, name the raw image `<member>.raw` in INDIR, record it in
  `inputs.json` as an AEA member's plain image is (`record-plain`) and remove the `.dmg`; a recorded raw image that is
  still there is not fetched again. The manifest's entry says `"converted": "udif"` in its `source`.
- An AEA member (`OS`, `Cryptex1,SystemOS`) carries `sha256` (of the encrypted member: the fetch checks it) and
  `plain_sha256` (of the decrypted image: `oneios-tool image aea-decrypt --profile` and `image manifest` check it),
  both pinned for 24A446 on 2026-11-21 (two fetches from Apple and two decryptions, `ipsw` and `aea-decrypt`, gave
  the same). Beside them is the archive's own authentication at decryption, with the key Apple's key service gives
  for that archive: every byte of the archive is under an HMAC (its header and auth data, each cluster's header,
  each segment, the padding), so a wrong key or a changed byte stops the decryption with nothing written
  (`tools/oneios-tool/README.md` "aea-decrypt"). For a new build, `inputs/<build>/inputs.json` holds both
  SHA-256s after the first full fetch: copy them into the profile.

Roles: `os`, `systemos`, `app`, `trust-static`, `trust-system`, `trust-app`, `roothash-os`,
`roothash-system`, `roothash-app` (all in the set); `kernelcache`, `devicetree` (inputs of the kernel build
and the booter: `scripts/image/kernel-inputs.sh` unpacks them to `out/kernel-inputs/<build>/`);
`buildmanifest`, `restore`.

iOS 27.0.1 (24A446), iPhone18,2: 13 of the IPSW's 165 members, 10.59 GiB of 11.84 GiB.

| Role | Member | Bytes |
| --- | --- | --- |
| os | `043-69487-662.dmg.aea` | 8 992 587 776 |
| systemos | `043-70037-715.dmg.aea` | 2 348 810 240 |
| app | `043-69297-793.dmg` | 16 777 216 |
| trust-static | `Firmware/043-69487-662.dmg.aea.trustcache` | 92 017 |
| trust-system | `Firmware/043-70037-715.dmg.aea.trustcache` | 3 191 |
| trust-app | `Firmware/043-69297-793.dmg.trustcache` | 863 |
| roothash-os, -system, -app | `Firmware/<image>.root_hash` | 229 each |
| kernelcache | `kernelcache.release.v54` | 23 390 449 |
| devicetree | `Firmware/all_flash/DeviceTree.v54ap.im4p` | 68 480 |
| buildmanifest, restore | `BuildManifest.plist`, `Restore.plist` | 816 945, 1 093 |

iOS 27.2.1 (24B156), iPhone18,2 (`profiles/ios-27.2.1-iPhone18,2.toml`, beside 24A446's, 2026-11-25; the only build
Apple signs since 2026-11-09, per api.ipsw.me): 13 of the IPSW's 165 members, 10.91 GiB of 12.25 GiB, the same roles.
Pins read on 2026-11-20 from the ZIP directory and BuildManifest (`image check-ipsw`: "ok ... matches", again on
2026-11-25); the SystemOS image's `sha256` and `plain_sha256` from a fetch and `aea-decrypt` on the build laptop
(2026-11-25, 3 min); the OS image's are not pinned yet (not fetched: it needs about 20 GiB of room at once, the
encrypted file beside the plain image). The decoded kernelcache is 78,512,128 bytes, SHA-256 `f6f31ac4...e9a5939`,
`xnu-13432.42.3~28` (24A446's is `xnu-13432.2.10~2`: the same XNU branch). 24A446 stays the default profile until a
phone boot of 24B156 passes.

| Role | Member | Bytes |
| --- | --- | --- |
| os | `141-10103-223.dmg.aea` | 9 261 023 232 |
| systemos | `141-10617-235.dmg.aea` | 2 415 919 104 |
| app | `141-08824-252.dmg` | 16 777 216 |
| kernelcache | `kernelcache.release.v54` | 23 545 189 |

## 3. `manifest.json`

```json
{
  "format": 1,
  "name": "oneios-image",
  "id": "ios-27.0.1-24A446-iPhone18-2",
  "os": "iOS",
  "version": "27.0.1",
  "build": "24A446",
  "device": "iPhone18,2",
  "board": "v54ap",
  "arch": "arm64e",
  "chip_id": 33104,
  "board_id": 14,
  "profile": { "name": "ios-27.0.1-24A446-iPhone18,2", "file": "ios-27.0.1-iPhone18,2.toml", "sha256": "<64 hex>" },
  "source": { "ipsw_url": "https://updates.cdn-apple.com/...", "ipsw_size": 12708916186, "ipsw_sha256": "<64 hex>" },
  "stage": "/data/local/tmp/oneios/image",
  "chunk": 1048576,
  "data": { "seed": "data.seed", "volume_uuid": "1B7DB2B7-5ECA-4FDA-98AF-55DF8632E355", "volume_name": "Data",
            "case_sensitive": true, "store_gib": 16 },
  "kernel": "kernel.kc",
  "tweaks": { "hook": "oneios-tool image tweaks", "hook_sha256": "<64 hex>", "commit": "<git>", "dirty": false, "info": {} },
  "files": [
    { "path": "os.img", "size": 10359930880, "sha256": "<64 hex>", "role": "os", "format": "apfs-raw",
      "read_only": true, "hashes": "os.img.hashes",
      "source": { "component": "OS", "member": "043-69487-662.dmg.aea", "decrypted": true },
      "apfs": { "container_offset": 0, "block_size": 4096, "block_count": 2529280, "container_uuid": "<UUID>",
                "volumes": [ { "name": "...", "uuid": "<UUID>", "role": "system", "case_sensitive": true, "sealed": true } ] } },
    { "path": "os.img.hashes", "size": 316224, "sha256": "<64 hex>", "role": "hashes", "format": "oneios-hash-list-1" },
    { "path": "trust/static.trustcache", "size": 92017, "sha256": "<64 hex>", "role": "trust-static", "format": "img4-payload",
      "source": { "component": "StaticTrustCache", "member": "Firmware/043-69487-662.dmg.aea.trustcache", "decrypted": false } },
    { "path": "data.seed", "size": 1159813120, "sha256": "<64 hex>", "role": "data-seed", "format": "tar",
      "root": "/private/var", "entries": 18325, "added": 12, "changed": 5 }
  ]
}
```

(The sizes in the example are placeholders except the trust cache's.)

Rules:

- `files` lists every file of the set except `manifest.json`, sorted by `path`. `path`, `size` and `sha256`
  are always there; these three and the top-level `format`, `name`, `os`, `version`, `build`, `device` are
  what the OneOS fork's verifier reads (`oneos/interface.md`, section 2).
- A path is relative, `/`-separated, each segment `[A-Za-z0-9_][A-Za-z0-9._+-]*`. Regular files only.
- `role` says what the file is for: `os`, `systemos`, `app`, `trust-*`, `roothash-*`, `data-seed`, `kernel`,
  `overlay`, `hashes`. `hashes` on an entry names its chunk hash list, which is itself a listed file.
- `kernel` is `null` in a set built with `--no-kernel`; the runtime cannot start such a set.
- `tweaks` is `null` when no userland hook ran.
- The manifest has no build time in it: the same inputs, hook and kernel give the same manifest bytes.
- It is written by serde_json with an indent of 2 (`oneios-tool image manifest`, `image build`): the layout
  Python's `json.dump(indent=2)` had for this file, so the fork's `manifest_rekernel` (an awk that reads the
  `"path"`, `"size"` and `"sha256"` lines of `kernel.kc`'s entry) reads both. Readers parse it and compare values
  (`image verify`, `phone image-check`, the fork's verifier, xnuhost); none compares its bytes with a manifest made
  again, so a set built before 2026-11-21 verifies with the new tool (checked: `image verify out/image/24A446`, the
  set of 2026-11-11, 20 files, ok).

### Version rules

- `format` is one integer. A reader refuses a manifest whose `format` is larger than the one it knows, and
  one without `name` = `oneios-image`. The runtime package lists the formats it boots in `runtime.json`
  `image_formats`; the switch refuses a set that is not in the list.
- Added keys do not change `format`; readers ignore keys they do not know.
- `format` goes up when a reader that ignores the change would do the wrong thing: a file's on-disk format
  changes (the hash list layout, the seed's tar conventions, the images no longer raw APFS), a required
  file is added or removed, or the meaning of an existing key changes.
- A new iOS build, device or tweak set is a new `id` and `build` under the same `format`.
- The profile has its own number (`schema`), which only this repository's tools read.

## 4. Chunk hash list (`NAME.hashes`, `oneios-hash-list-1`)

For a file read block by block for as long as the guest runs (the three images and the kernel collection),
so the booter can check what it reads without hashing 10 GiB at each start.

```text
offset  size  field
0       8     magic "OIHL1\0\0\0"
8       4     chunk size in bytes, little endian (the manifest's "chunk": 1048576)
12      4     hash algorithm: 1 = SHA-256
16      8     size of NAME in bytes
24      8     number of chunks = ceil(size / chunk)
32      32*n  SHA-256 of chunk 0, 1, ...; the last chunk is the file's remaining bytes, not padded
```

The list's own SHA-256 is in the manifest, so one hash of a few hundred KiB at start vouches for every
chunk read later. `scripts/image/verify.sh` checks each image against its list.

## 5. Data seed (`data.seed`)

The guest's Data volume has to be APFS, made by Apple's own APFS code: iOS 27 uses case-sensitive volumes
with expanded records, and no Linux tool writes those. So there are two forms, named by the entry's `format`:

**`tar` (the default, built on Linux and macOS).** A tar (ustar headers, pax records where needed) of what goes into the new Data volume's root,
which iOS sees as `/private/var`. It starts as the OS volume's own `/private/var`; then the userland hook
changes it (section 6).

- Entries are relative paths without `./`, sorted by byte order. Only folders, regular files and symbolic
  links. Hard links in the source become separate files.
- `uid`, `gid` and `mode` are the image's (numeric; `uname` and `gname` are empty). An entry the hook adds
  takes the owner of the folder it is in and the mode it was created with; `$ONEIOS_OWNERS` overrides.
- `mtime` is the image's for untouched entries and the profile's `[data] epoch` for added or rewritten ones.
- Extended attributes are pax records `LIBARCHIVE.xattr.<name>` with the value in base64 (bsdtar restores
  them). `com.apple.decmpfs`, `com.apple.ResourceFork` and `com.apple.fs.symlink` are not carried: the tar
  holds the plain content.
- The same inputs and hook give the same bytes.
- The headers are the `tar` crate's (`oneios-tool image seed-pack`, since 2026-11-21): ustar, a name that is not
  ASCII as its UTF-8 bytes, a name or link target too long for ustar as a GNU long name (`././@LongLink`, types
  `L` and `K`), a pax extended header only for an entry's xattrs, two zero blocks at the end. `oneios-seed`
  (`userland/helpers/oneios-seed.c`) reads all of these, and so do GNU tar and bsdtar. Before 2026-11-21 the seed
  was Python tarfile's pax format byte for byte (a `path` record for such names, a checksum of six digits, the
  file padded to 10240 bytes). Both forms unpack to the same tree: on 24A446 the two seeds of the same inputs
  (`27ec4e10...`, 1,267,793,920 bytes, and `5406d527...`, 1,265,063,424 bytes) list the same 20,851 entries with the
  same names, modes, owners, times, sizes and link targets; three property lists the root tweaks write differ in
  their bytes (now the `plist` crate's binary writer), not in their values.

  The guest turns it into a volume on its first start (booter and userland): make an APFS container in the
  Data store (`data/` on the stage, `store_gib` GiB, sparse) with one case-sensitive volume named
  `volume_name` with UUID `volume_uuid` (`newfs_apfs -e -x -U`, from the OS volume), unpack the seed into it
  as root with numeric owners, and record the seed's SHA-256 beside the store. A store whose recorded seed
  hash differs from the manifest's belongs to another seed and is started over, as UniXNU's Data store does.
  So a set built with a new seed starts the phone's Data store over on its first boot; a set already staged keeps
  its own seed and manifest, and nothing re-derives them.

**`apfs-raw` (`build.sh --seed apfs`, macOS only).** The same content already unpacked into an APFS volume
image with that UUID (`scripts/image/seed-apfs-macos.sh`: `newfs_apfs -e -x`, as UniXNU-droidvm's
`build-droidvm-data-seed.py`). The guest then only copies it into the store. It needs `sudo` (the files are
written with their owners). Written, not run: the Mac session of 2026-11-12 had no `sudo`.

## 6. The userland hook

`build.sh` runs the userland hook once, after it has copied `/private/var` and before it packs the seed: by
default (since 2026-11-21) the Rust one, `oneios-tool image tweaks hook --repo ROOT` (the port of the Python
`userland/image-hook`, deleted on 2026-11-21 after the two gave the same seed and overlay byte for byte); `--hook
FILE` any other executable. build.sh records the hook in the manifest's `tweaks` (`hook`, `hook_sha256` of the
tool's binary or of the hook's file, `commit` and `dirty` from git, `info`: the hook's summary), written with
`oneios-tool json set` (a Python block until 2026-11-21: the same keys and values). The read-only images are never changed, so every tweak to the system is a change to the
Data seed, a file in `overlay/`, or kernel policy. Hook API 1:

| Variable | Meaning |
| --- | --- |
| `ONEIOS_HOOK_API` | `1` |
| `ONEIOS_PROFILE`, `ONEIOS_BUILD`, `ONEIOS_VERSION`, `ONEIOS_DEVICE`, `ONEIOS_BOARD` | the profile's path and identity |
| `ONEIOS_INPUTS` | `inputs/<build>/` (BuildManifest, trust caches, kernelcache, device tree) |
| `ONEIOS_OS_ROOT`, `ONEIOS_SYSTEMOS_ROOT`, `ONEIOS_APP_ROOT` | the three volumes' roots, mounted read-only |
| `ONEIOS_DATA` | writable copy of `/private/var`: add, rewrite or delete here |
| `ONEIOS_OVERLAY` | empty folder; files put here become `overlay/...` of the set |
| `ONEIOS_OWNERS` | file to append owner rules to: `PATH<tab>UID<tab>GID<tab>MODE` and an optional `<tab>R` for the whole tree below. `PATH` is relative to `ONEIOS_DATA`, `MODE` octal or `-` |
| `ONEIOS_INFO` | file to write one JSON value to; it becomes `tweaks.info` in the manifest |
| `ONEIOS_EPOCH`, `ONEIOS_DATA_UUID` | the profile's `[data]` values |
| `ONEIOS_ALLOW_NO_METAL=1` | explicitly allow an intentionally CPU-only S24 Ultra image: `scripts/image/build.sh` skips its Metal check, and the hook places no bundle |

For `ONEIOS_DEVICE=iPhone18,2`, the built-in hook checks a given `NativeAdrenoMetal.bundle`: its `oneios-target`
and the adjacent `NativeAdrenoMetal.build.json` must name `<device> <build>`, or the hook stops before it replaces
the image set (a bundle built for another image, or one with no target or record, is refused). A missing bundle is
not refused by the hook. On a PC, `scripts/image/build.sh` refuses an S24 Ultra image with no matching bundle before
it mounts anything; set `ONEIOS_ALLOW_NO_METAL=1` there only when CPU composition is intentional. `--no-hook` /
`--no-tweaks` still explicitly skips the hook.

**The Metal plug-in of an image made on the phone (2026-11-24).** The OneOS app's Make iOS image (`serve
make-image`) sends no `metal_plugin` or `camera_plugin`. The runtime package carries both since this change
(`lib/guest/NativeAdrenoMetal.bundle` with its build record, `lib/guest/camera/H9ISP.mediacapture`; runtime.json
`metal` and `camera`; [releases.md](releases.md)), and `make-image` takes the staged runtime's when the request
gives none and the bundle's `oneios-target` is the image's `<device> <build>` (`Tweak::for_image` in
`crates/oneios-imagebuild`). The hook then seeds them as a PC build does: the bundle in
`/private/var/oneios/addons/Extensions`, the capture plug-in in `/private/var/oneios/addons/MediaCapture`, their
A runtime with another iOS build's plug-ins (`lib/guest/<build>/`, runtime.json `metals` and `cameras`, 2026-11-25:
iOS 27.2.1's 24B156) offers them all: `make-image` takes the one whose `oneios-target` is the image's, the default's
first, with the capture plug-in of that target. With no `xnu` or `kb` in the request it also takes the kernel of the
image's build (`runtime.json` `kernels`, by the profile's `[os] build`) and `<stage>/kb-<build>/`; a build with no
`kernels` entry (24A446, the A56's 20F66) gets `kernel` and `kb/` as before.

CDHashes in `overlay/userland/owner-trust.json`. The build says where its plug-in came from in one line, `Metal
plug-in: ...`, and in its `ok` event's `metal` field:

| Line | When |
| --- | --- |
| `Metal plug-in: STAGE/runtime/lib/guest/NativeAdrenoMetal.bundle (the staged runtime's, for iPhone18,2 24A446)` | the request gave none and the runtime's is for this image |
| `Metal plug-in: none: the staged runtime's (...) is for iPhone18,2 24A446, this image is D B (CPU composition)` | a runtime for the S24 Ultra's iOS 27 and an image of another iOS (another phone's fork): neither plug-in is placed, the build goes on |
| `Metal plug-in: none: none given, and the staged runtime carries none (CPU composition)` | a runtime before this change, one released with `--no-metal`, or `"runtime_plugins": false` in the request |
| `Metal plug-in: PATH (given)` | the request's `metal_plugin` (the preflight above checks it) |
| `Metal plug-in: none (ONEIOS_ALLOW_NO_METAL=1)`, `none (no root tweaks)` | as they say |

Why a phone-made image without a bundle is still not refused (decided 2026-11-24): the phone cannot build the
bundle, so a refusal would stop Make iOS image on every runtime without one, while the image without it boots and
composites on the CPU, as every phone-made image did before. The line above and the `ok` event's `metal` say it.
A runtime update with a new plug-in reaches an image made before it without a new image (issue #86, 2026-11-28): the
release packs the plug-in also as the runtime's boot addon (`kernel/gpuaddon`, `kernel/<build>/gpuaddon`), and xnuhost
hands it to the kernel when the set has no `kernel.kc.gpuaddon` of its own and the plug-in's `oneios-target` names the
set's build, with its CDHashes in that boot's trust cache; the set's files are not changed ([gpu.md](gpu.md), "A
runtime update brings its plug-in"). A set's own add-on (`oneios-tool phone addon`) still wins.

`oneios-tool image tweaks hook` also reads `ONEIOS_OS_IMAGE`: the OS volume's raw image, read with the tool's own
APFS reader instead of the mounted `ONEIOS_OS_ROOT` (the phone mounts nothing; the same seed). The hooks read only
the OS volume.

The hook runs as the build user, not root: it cannot `chown`, which is what `ONEIOS_OWNERS` is for. A
non-zero exit stops the build and leaves the previous set in place. The manifest records the hook's path
(`oneios-tool image tweaks` for the Rust hook), SHA-256 (of the tool's binary for the Rust hook), the repository
commit and whether `userland/` (for the Rust hook also `tools/oneios-tool/crates/oneios-tweaks`) had uncommitted
changes.

## 7. The archive, and how the OneOS fork verifies and stages it

`out/image/<archive>` (`OneiOS_iOS27.0.1_24A446_iPhone18-2.tar.zst`) and `<archive>.sha256` (one
`sha256sum` line). The owner copies the archive to the phone and imports it in the fork's app; it is never
uploaded.

- zstd, window 2^27 (`--long=27`; the fork accepts up to 2^31).
- tar (the `tar` crate's ustar headers; a size past 8 GiB in the base-256 form, which the fork's reader takes):
  `manifest.json` first, then the files in manifest order, folders before their files. Regular
  files and folders only, mode 0644/0755, owner 0, time 0.

How it maps to OneOS (`app-android/core/model/.../Images.kt`):

| OneOS | OneiOS |
| --- | --- |
| `gzName`, `gzBytes`, `gzSha256`: the download | the `.tar.zst` and its `.sha256`. Built locally, so the app cannot have its hash built in; the `.sha256` file guards the copy to the phone |
| `rawBytes`, `rawSha256`: the unpacked `system.raw`, checked again by root | every file's `size` and `sha256` in `manifest.json`. For the three images and the trust files these are the same for everyone who builds 24A446, so the app can also pin them in its table |
| `buildId`, `sdk`, `family` | `build`, `version`, `device`, `format` |
| stage `$STAGE/system.raw` | stage `/data/local/tmp/oneios/image/` with the set's paths |
| ext4 magic check at byte 1080 | `NXSB` at byte 32 of each image; `format` checked against the runtime's `image_formats` |

The fork's steps (its side is `oneos/interface.md`):

1. If `<archive>.sha256` is beside the file, compare.
2. Stream the tar. The first member must be `manifest.json`: parse it, refuse `format` > 1 or another `name`.
3. For each further member: refuse anything that is not a regular file or folder, a path that breaks the
   rule above, or a file not in `files`; hash while writing; refuse a wrong size or SHA-256.
4. After the last member: refuse if a listed file did not come.
5. Root writes the files under `image/` and checks each again (`image.sha256`, `image.ids`).

`scripts/image/verify.sh ARCHIVE` does steps 1 to 4 on the build machine, and `verify.sh FOLDER` checks an
unpacked set and each image against its hash list; both are tested against tampered sets.

**Compressed files (since 2026-11-23).** `oneios-tool image verify FOLDER` (not `--quick`; `image build` and `make-image`
run it last) also reads every compressed file of each read-only volume (`os.img`, `systemos.img`, `app.img`) as iOS
reads it (`image apfs-check`, tools/oneios-tool/README.md): the hashes say the files are the ones the manifest names,
not that iOS can read them. A set whose OS image another writer made fails here, before a boot (OneiOS issue 73: the
other A56 fork's set, whose `dyld` iOS refused at its first start). On the Linux PC: the 24A446 set verifies in 6 s,
the 20F66 set too (OS 165,519, SystemOS 1,277, AppOS 608 compressed files, all readable). The archive form
(`verify ARCHIVE`) streams and does not read the volumes; its folder, once unpacked, does.

## 8. Building: tools, and what needs macOS

The same commands on Linux and on macOS; neither needs root. `build.sh` picks the way to read the images by
`uname -s`. `oneios-tool image build` mounts nothing on any system: it reads `/private/var` with the tool's own APFS
reader (the owners, modes, times and xattrs from the volume's records, the files' contents from its extents,
compressed files decoded) and writes the tree and the meta file `tree-copy` writes from a mount; on 24A446 its seed
is build.sh's (Linux, apfs-fuse) byte for byte.

| Step | Linux | macOS |
| --- | --- | --- |
| List and fetch IPSW members | `oneios-tool image check-ipsw`, `fetch` | same (run on the Mac, 2026-11-12, with the Python tool of then) |
| AEA decryption | `oneios-tool image aea-decrypt` (since 2026-11-21; both images of 24A446 on the Linux PC, to the pinned `plain_sha256`). Before: `ipsw` 3.1.743 (installed by `scripts/fetch/get-ipsw-cli.sh`, pinned by SHA-256; the script went on 2026-11-21, when nothing needed `ipsw` any more) | the same command (not run on the Mac yet); before, the `ipsw` macOS arm64 release (run on the Mac: both images of 24A446) |
| Read the volumes (the seed's `/private/var`, the hook's three roots) | `apfs-fuse`, no root: owners as stored | `hdiutil attach -readonly -owners off`, no root: every file shows as the user's and is readable; the owners of `/private/var` come from the OS image's own APFS records (`imagetool.py tree-copy --owners-from` then, `oneios-tool image` since), checked entry by entry against the mount |
| Seed as tar | yes | yes (xattrs read with libc's `listxattr`, which Python lacks on macOS) |
| Seed as APFS (`--seed apfs`) | **no**: nothing on Linux writes this APFS | `newfs_apfs`, as root (`sudo`). Not run |
| Hash lists, manifest, archive | `oneios-tool image`, `zstd`; `sha256sum` (fetch.sh's `SHA256SUMS`) | the same; `shasum -a 256`, Homebrew's `zstd` |
| Kernel inputs | `oneios-tool image im4p` (LZFSE in Rust; `scripts/image/kernel-inputs.sh`) | same |

**The owners on macOS.** A volume a user attaches with owners honoured keeps root's folders closed to that user,
and attaching as root needs `sudo`. So `build.sh` attaches with owners ignored and reads the real owner, group and
mode of every entry below `/private/var` from the image itself: `oneios-tool image` (its APFS reader; `imagetool.py`'s `ApfsTree` before 2026-11-13) walks the volume's
file-system tree (inode, directory and xattr records; no file data). On a sealed volume that tree is hashed and its
index entries number their children relative to the root's object id (24A446: root 1158, children 1158 + n). The
copy stops if an entry's type, mode or xattr names differ between the mount and the records, or if an entry is in
one and not in the other. `oneios-tool image apfs-owners IMAGE [DIR]` prints what it reads.

**A store that was used: block 0 is old, and the newest checkpoint may be cut off.** The read-only images above are
written once, so block 0's superblock is current and `apfs-info` and `apfs-owners` read it as it is (imagetool.py's
reader, no checks beyond its own). The Data store (`data/data.img`) is different: Apple's APFS writes block 0's copy
of the container superblock only at a clean unmount, every session ends with the kernel killed, and a block that a
later transaction frees is used again a few transactions on. Seen on a store written by iOS 27's own APFS on the PC
(2026-11-13, `oneios-tool image apfs-checkpoints`; 94 container superblocks in a checkpoint descriptor area of 188
blocks, transactions 105 to 198): block 0 still held transaction 14, whose object map block had become a node of the
extent reference tree; of the 94 checkpoints only the newest ten passed, every older one names at least one block
that was written again since. So a reader of a used store must (as "Mounting an Apple File System Partition" of
Apple's reference says, and as apfs-fuse does) take block 0 only for the place of the checkpoint descriptor area
(`nx_xp_desc_base`, `nx_xp_desc_blocks`), read every container superblock there, and use the one with the highest
transaction that is whole; and since the last one may be half written, check before it trusts, and go on to the
checkpoint before when a check fails (apfs-fuse stops at the newest superblock with a good checksum):

- a block address against the container's block count and the file's length, before the block is read;
- an object's header: the Fletcher-64 checksum of the block (`o_cksum`), its type, its object id (a physical
  object's is its own address, a virtual one's is the id it was looked up by, the container superblock's is 1) and
  a transaction (`o_xid`) that is not newer than the checkpoint's: a newer one means the block was written again;
- a checkpoint's superblock is the last block of its own run in the descriptor area (`nx_xp_desc_index`,
  `nx_xp_desc_len`, modulo the area's length), the blocks before it are its checkpoint map
  (`checkpoint_map_phys_t`, the last one marked `CHECKPOINT_MAP_LAST`), and its container UUID is block 0's (a
  superblock of a container that was there before the store was made again is not a candidate);
- a tree node: root or not, its level one below its parent's, its table and every key and value inside the node.
  The nodes of a sealed volume's hashed file-system tree, the root too, carry no header (32 zero bytes, the flag
  `BTNODE_NOHEADER`): only their shape can be checked.

The container's object map is `OMAP_MANUALLY_MANAGED` (no snapshots), so it keeps one mapping per volume: an older
state of a volume is reached through an older checkpoint's object map, not through a history in the newest one
(the format, keys of object id and transaction, would allow one). File contents carry no checksum in APFS.
`oneios-tool`'s reader for used stores does all of this (`tools/oneios-tool/README.md` "The APFS reader").

Checked on 2026-11-12: the Mac's `data.seed` and the phone's (built on the Linux PC with apfs-fuse, same hook) hold
the same 20,847 entries with the same owners, modes, times, links, xattrs (none: `/private/var` of 24A446 has only
`com.apple.decmpfs`, `com.apple.ResourceFork` and one `com.apple.fs.symlink`, all dropped) and contents, except the
two plug-in binaries, which each host builds differently (`docs/gpu.md` section 7). The OS volume's `/private/var`
has 20,799 entries (19,988 owned by mobile 501:501, 770 by root); every one agreed between the mount and the records.

`build.sh` checks its tools before anything else and stops with one line (`build: missing tools: ...`), then
its inputs (`build: missing inputs in ...: <names>`), the kernel collection and the hook. It builds in
`out/image/.work-<build>/` and replaces `out/image/<build>/` only after the new set verified.

Disk: 30 GiB for the fetch (both encrypted files next to their decrypted images; the encrypted ones are
removed after), then about 17 GiB for the set (hard links to `inputs/` when on the same volume) and about as
much again for the archive.

## 9. Tests

`cargo test --release --manifest-path tools/oneios-tool/Cargo.toml -p oneios-image -p oneios-imagebuild -p oneios-tweaks`:
the image tool's own tests on generated stand-in files (a ZIP laid out like an IPSW, APFS images made by
`tools/oneios-tool/tests/support/mkapfs.rs`, the code of `oneios-tool debug mkapfs`, trust files and kernels), the
image build in one process (`make-image`) and the root tweaks. Since 2026-11-23 also `apfs::decmpfs::tests::layout_as_ios_reads_it`
(Apple's resource map passes, the other fork's header copy in it fails, the chunk tables) and `udif::tests` (the UDIF
converter); the stand-in volumes of `fixtures.rs` have a root folder now, so `verify` reads them. `scripts/image/tests/run.sh` (23 tests of the
Python `imagetool.py`, `build.sh` from stand-in inputs among them) went with that script on 2026-11-21; `build.sh`
itself is checked by a run on the real inputs (24A446 on 2026-11-21, after its Python blocks were replaced:
`data.seed` `27ec4e10...`, the overlay and `manifest.json` byte for byte as with the Python record writer;
`tools/oneios-tool/README.md` "image tweaks").
