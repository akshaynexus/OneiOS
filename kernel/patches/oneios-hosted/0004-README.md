# 0004-root-mount-launchd and `storage/`: the hosted kernel's disks, root, Data and launchd

Owner: fable-root2 (took over from fable-root). Date: 2026-11-10. The xnuhost side is commit `3b57b73`
(`runtime/xnuhost/src/imageset.c`): chunk checks, trust caches, `/chosen/root-matching`, each disk's identity.

**State, plainly.** It builds and links into the hosted kernel (from scratch, `HOSTED_OUT=root`). **None of it has
run in a kernel**: the standalone `xnu.macho` panics at `image4 interface not available @kern_trustcache.c:592`,
before `bsd_init`, and the disks need `kernel.kc` anyway (IOStorageFamily and APFS are Apple's kexts). The seed
unpack is the one part that was run: its code, compiled on the PC against POSIX stand-ins, unpacks the real
`data.seed` (1174 MiB) to the same tree as `userland/helpers/oneios-seed.c` and checks its hash.

## Parts

| Where | What |
|---|---|
| `storage/OneiOSHostedDisk.cpp` | One `IOBlockStorageDevice` for every xnuhost disk (virtio-blk on a virtio-mmio window; nodes `virtio-mmio-N` and `data-store`). Reads, writes and flush (`VIRTIO_BLK_T_FLUSH`) where the device is not read-only. Registers through the `mmio_read`/`mmio_write` host calls, no mapping; one 1 MiB bounce buffer; the queue is polled (xnuhost serves it inside the notify). After UniXNU's NativeVirtioBlock. |
| `storage/OneiOSHostedStorage.cpp` | `native_source_storage_start` (called by `bsd_init` after the IOKit bootstrap): loads IOStorageFamily and APFS, registers the source-built `IOBlockStorageDevice` below the original class and the driver, adds the personality. No IOStorageFamily (a kernel outside a collection): a console line, no disks. |
| `oneios-tool kernel gen storage` | The archive `build/oneios-hosted-storage/liboneioshostedstorage.a`: IOStorageFamily's `IOBlockStorageDevice.cpp` (pin `7edb88f`, registration replaced) and the two files above; the static initializers of the two classes are made late ones. `kernel/build-hosted.sh` adds it to its copy of `board.toml`; the hosted defconfig force-loads it. |
| `0004` `osfmk/arm64/hosted/hosted_blk.c` | `oneios_hosted_mmio_read32/write32`: the two host calls, masked. |
| `0004` `bsd/kern/oneios_hosted_root.c` | The Data volume's first start, the seed unpack, the launch policy (below). |
| `0004` `bsd/kern/kern_exec.c` | UniXNU's `native_owner_cache_prepare` takes the SystemOS and OS volume UUIDs from the disks' nodes; `native_owner_data_prepare` takes the Data device from `oneios_hosted_data_device` and unpacks the seed after the mount. |
| `0004` `bsd/vfs/vfs_syscalls.c` | `oneios_kern_mkdir/symlink/unlink`: the static `mkdir1at`, `symlinkat_internal` and `unlink1` by kernel paths. |
| `kernel/device/configs/oneios_hosted_defconfig` | UniXNU's owner-root options, as its DroidVM iPadOS 27 board: `OWNER_ROOT_AUDIT`, `OWNER_ROOT_TRUST`, `OWNER_CACHE_MOUNT`, `OWNER_CACHE_STARTUP`, `OWNER_BOOT_MANIFEST`, `OWNER_DATA_MOUNT`, `OWNER_UNION_SYSTEMOS`, `BOARD_LAUNCH_POLICY`, and `SOURCE_STORAGE`. |

## Verified root

xnuhost checks every 1 MiB chunk of `os.img`, `systemos.img` and `app.img` before the guest reads it and puts
`oneios,verified` (the SHA-256 of the chunk hash list) in the disk's node. That property is the disk's policy in the
kernel: `native_verified_block_root_authorized(dev)` is true when the media with that device number sits on a
`OneiOSHostedDisk` that has the property and that the device offers as read-only. UniXNU's signed block hashes
(VerifiedRoot, root policies) are not linked and not asked for. `CONFIG_XNU_SOURCE_STORAGE=y` only tells UniXNU's
build script that the verified storage is a source-built archive (its condition for the root audit); it brings no
code. The boot manifest hash (`OWNER_BOOT_MANIFEST`) is made from SHA-384 of each disk's `oneios,verified` and its
volume UUID. `CONFIG_XNU_OWNER_ROOT_VOLUME_UUID` in the defconfig is a placeholder the build script insists on.

## The path from `bsd_init` to launchd

1. Root: IOKit's `root-matching` (xnuhost: the OS volume's UUID) finds the OS volume on disk 0; `bsd_init` accepts it
   because its disk is verified (`OWNER_ROOT_TRUST`).
2. `load_init_program`: `native_owner_cache_prepare` mounts the SystemOS volume (UUID from the `systemos` disk's
   node) over `/` as a read-only union and publishes the boot manifest.
3. `native_owner_data_prepare` -> `oneios_hosted_data_device`: the Data volume is **the APFS volume on the disk
   with the role `data`**, whatever its UUID. A store without a container (`NXSB` not at byte 32) is formatted first:
   `/sbin/newfs_apfs -e -x -v Data /dev/diskN`, started with `posix_spawn` from the init process and waited for with
   `wait4(WNOHANG)` (300 s). Then the mount at `/private/var`.
4. `oneios_hosted_data_seed`: `/private/var/.oneios-seed` holds the seed's SHA-256.
   - The same as the `seed` disk's `oneios,sha256`: nothing to do.
   - No marker: the seed (a pax tar on the `seed` disk, `oneios,bytes` long) is unpacked **in the kernel** with
     the rules of `userland/helpers/oneios-seed.c` (numeric owners, modes, symbolic links, `LIBARCHIVE.xattr.*`; no
     `..`, no absolute path, no path through a symbolic link; only files, folders and links). Everything read is
     hashed; the marker is written last and only when the hash is the manifest's. A wrong hash is `EAUTH`: the boot
     panics ("offline scratch Data mount failed") and the volume stays unmarked.
   - Another hash: the first 64 MiB of the store are zeroed below the storage stack and the guest restarts
     (`XH_HALT_RESTART`, xnuhost exit 3). The next start finds a blank store and formats it.
   The user-space helper is not used and need not be in the set (`overlay/userland/bin/oneios-seed`,
   xnuhost's `seed-helper` disk: both can go).
5. `native_board_launch_policy` (before each launchd exec, once): folders under `/private/var/oneios/preboot`
   shown over `/private/preboot` (bindfs union), `/System` and `/usr` bound at
   `/private/preboot/Cryptexes/OS/{System,usr}`, the App volume (UUID from the `app` disk's node, verified) mounted
   read-only at `/private/preboot/Cryptexes/App`; then `/private/var/oneios/bin/oneios-init` is spawned if the seed
   has it, and a timer gives its task a bootstrap port once a launchd job has one. Never fatal. `disabled.plist` is
   not written here: the seed carries it, owned by root.
6. `load_init_program_at_path("/sbin/launchd")`: unchanged.

## Tested

- `HOSTED_OUT=root kernel/build-hosted.sh` from scratch with the committed series and this patch: links, sealed.
- Boot of that kernel under `qemu-aarch64 -cpu cortex-a710` with `out/image/24A446`: xnuhost lists five disks with
  their roles; the kernel stops at `image4 interface not available`, as without this patch.
- The seed section of `oneios_hosted_root.c` on the PC (kernel calls replaced by POSIX ones) with the real
  `data.seed`: 6024 files, 14813 folders, 1 link; hash equal to the manifest's; the tree equal to the helper's
  (content, types, modes, link targets); the same seed again does nothing; another hash takes the start-over path.
  Owners could not be checked (not root), and the seed has no extended attributes.

## Not tested (needs `kernel.kc`, then a session)

Everything in the kernel: the driver against xnuhost's device (queue, writes, flush), the class registration below
Apple's IOStorageFamily, the provider chain from an APFS volume to the disk, the root mount, the union mount,
`newfs_apfs` (its options and whether `posix_spawn` from the init process runs it; whether APFS publishes the new
volume without a rescan), the unpack through XNU's VFS, the bindfs views and the App mount, the service manager and
its bootstrap port, the start-over (zero and restart), launchd's exec.
