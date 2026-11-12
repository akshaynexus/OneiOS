# The OneiOS image set (format 1)

Owner: image-build. Code: `profiles/`, `scripts/fetch/`, `scripts/image/`. Tests: `scripts/image/tests/run.sh`.

The image set is to OneiOS what `system.raw` plus its overlays are to OneOS: the guest system, built on the
owner's machine from the vendor's firmware, pinned by size and SHA-256, staged in a folder on the phone. For
OneOS the firmware is an Android OTA and the builder is MysticGSI. Here it is Apple's restore file (IPSW) for
iPhone18,2, iOS 27.0.1 (24A446), and the builder is `scripts/image/build.sh`.

Nothing of the set is in git or in a release: it is Apple's software. A release carries the scripts; each
owner fetches from Apple and builds.

```sh
scripts/fetch/fetch.sh --list 'profiles/ios-27.0.1-iPhone18,2.toml'      # compare the profile with Apple's file, about 1 MiB
scripts/fetch/get-ipsw-cli.sh                                            # the AEA decryption tool, once
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
ones after AEA decryption). Each is a raw APFS container: the container superblock (`NXSB`) is at offset 0,
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
anything else (a UDIF image has to be converted to raw first).

## 2. Profile

`profiles/<name>.toml` pins one build, as MysticGSI's patch sets and UniXNU's `profiles/os/*/profile.toml` do.

| Table | Keys |
| --- | --- |
| top | `schema = 1` |
| `[os]` | `name`, `version`, `build`, `device`, `board`, `chip_id`, `board_id`, `arch` |
| `[ipsw]` | `url` (Apple's CDN), `size`, `sha256` (whole file), `metadata` (where the URL came from), `variant` (the BuildManifest identity), `need_gib` |
| `[[member]]` | `role`, `component` (BuildManifest key), `path` (ZIP member), `size`, `crc32`, then `sha384` or `sha256`, or `aea = true` |
| `[image]` | `format = 1`, `id`, `stage`, `archive`, `chunk`, `seed`, `kernel` |
| `[image.files]` | role -> path in the set |
| `[data]` | `volume_uuid`, `volume_name`, `store_gib`, `source`, `epoch` |

How a member is pinned:

- `size` and `crc32` come from the IPSW's ZIP directory and are checked for every member.
- A member that is not AEA carries `sha384`: the `Digest` of its component in BuildManifest.plist, which is
  the SHA-384 of the file as shipped (checked on all nine such members of 24A446). `BuildManifest.plist`
  and `Restore.plist` carry `sha256`.
- An AEA member (`OS`, `Cryptex1,SystemOS`) has no hash of the profile's own. Its integrity is the archive's
  own authentication at decryption, with the key Apple's key service gives for that archive, plus size and
  CRC-32. After the first full fetch, `inputs/<build>/inputs.json` holds the SHA-256 of both the encrypted
  and the decrypted file; copy them into the profile as `sha256` and `plain_sha256` to pin them for good.

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
  "tweaks": { "hook": "userland/image-hook", "hook_sha256": "<64 hex>", "commit": "<git>", "dirty": false, "info": {} },
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

**`tar` (the default, built on Linux and macOS).** A pax tar of what goes into the new Data volume's root,
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

  The guest turns it into a volume on its first start (booter and userland): make an APFS container in the
  Data store (`data/` on the stage, `store_gib` GiB, sparse) with one case-sensitive volume named
  `volume_name` with UUID `volume_uuid` (`newfs_apfs -e -x -U`, from the OS volume), unpack the seed into it
  as root with numeric owners, and record the seed's SHA-256 beside the store. A store whose recorded seed
  hash differs from the manifest's belongs to another seed and is started over, as UniXNU's Data store does.

**`apfs-raw` (`build.sh --seed apfs`, macOS only).** The same content already unpacked into an APFS volume
image with that UUID (`scripts/image/seed-apfs-macos.sh`: `newfs_apfs -e -x`, as UniXNU-droidvm's
`build-droidvm-data-seed.py`). The guest then only copies it into the store. It needs `sudo` (the files are
written with their owners). Written, not run: the Mac session of 2026-11-12 had no `sudo`.

## 6. The userland hook

`build.sh` runs `userland/image-hook` (or `--hook FILE`) once, after it has copied `/private/var` and before
it packs the seed. The read-only images are never changed, so every tweak to the system is a change to the
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

The hook runs as the build user, not root: it cannot `chown`, which is what `ONEIOS_OWNERS` is for. A
non-zero exit stops the build and leaves the previous set in place. The manifest records the hook's path,
SHA-256, the repository commit and whether `userland/` had uncommitted changes.

## 7. The archive, and how the OneOS fork verifies and stages it

`out/image/<archive>` (`OneiOS_iOS27.0.1_24A446_iPhone18-2.tar.zst`) and `<archive>.sha256` (one
`sha256sum` line). The owner copies the archive to the phone and imports it in the fork's app; it is never
uploaded.

- zstd, window 2^27 (`--long=27`; the fork accepts up to 2^31).
- pax tar: `manifest.json` first, then the files in manifest order, folders before their files. Regular
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

## 8. Building: tools, and what needs macOS

The same commands on Linux and on macOS; neither needs root. `build.sh` picks the way to read the images by
`uname -s`.

| Step | Linux | macOS |
| --- | --- | --- |
| List and fetch IPSW members | python3 3.11+ | same (run on the Mac, 2026-11-12) |
| AEA decryption | `ipsw` 3.1.743 (`scripts/fetch/get-ipsw-cli.sh`, pinned by SHA-256) | same, the macOS arm64 release (run on the Mac: both images of 24A446) |
| Read the volumes (the seed's `/private/var`, the hook's three roots) | `apfs-fuse`, no root: owners as stored | `hdiutil attach -readonly -owners off`, no root: every file shows as the user's and is readable; the owners of `/private/var` come from the OS image's own APFS records (`imagetool.py tree-copy --owners-from`), checked entry by entry against the mount |
| Seed as tar | yes | yes (xattrs read with libc's `listxattr`, which Python lacks on macOS) |
| Seed as APFS (`--seed apfs`) | **no**: nothing on Linux writes this APFS | `newfs_apfs`, as root (`sudo`). Not run |
| Hash lists, manifest | `oneios-tool image` (Rust; `scripts/image/build.sh` builds it when missing) | same |
| Archive (`pack`, its `verify`) | python3 (`scripts/image/imagetool.py`), `zstd` | same (Homebrew's `zstd`) |
| Kernel inputs | `oneios-tool` (`image im4p`, built by `scripts/image/kernel-inputs.sh` when missing); python3 only to fetch missing members | same |

**The owners on macOS.** A volume a user attaches with owners honoured keeps root's folders closed to that user,
and attaching as root needs `sudo`. So `build.sh` attaches with owners ignored and reads the real owner, group and
mode of every entry below `/private/var` from the image itself: `imagetool.py` `ApfsTree` walks the volume's
file-system tree (inode, directory and xattr records; no file data). On a sealed volume that tree is hashed and its
index entries number their children relative to the root's object id (24A446: root 1158, children 1158 + n). The
copy stops if an entry's type, mode or xattr names differ between the mount and the records, or if an entry is in
one and not in the other. `imagetool.py apfs-owners IMAGE [DIR]` prints what it reads.

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

`scripts/image/tests/run.sh`: 23 tests on generated stand-in files (a ZIP laid out like an IPSW, minimal
APFS superblocks and a small APFS file-system tree, hashed and not, fake trust files and kernel). They pass on Linux
and on macOS. They cover the profile checks, the ZIP member fetch with
resume and CRC failure, pin checks, the AEA header and IM4P readers, APFS facts, the owner reader of macOS builds, hash lists, the seed's
owner, mode and time rules and its reproducibility, manifest creation and every way verification refuses a
set or an archive, and `build.sh` from stand-in inputs to a verified archive with a hook.
