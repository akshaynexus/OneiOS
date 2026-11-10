// SPDX-License-Identifier: MIT
// The image set as xnuhost reads it (docs/image-format.md, format 1): manifest.json, the chunk hash
// lists of the read-only images, Apple's trust caches (IM4P) and the userland overlay.
//
// xnuhost is the root of trust of a hosted kernel (it holds the kernel's memory), so the verified
// root of UniXNU (signed block hashes checked by the guest's NativeVirtioBlock) is done on this
// side: every 1 MiB chunk of os.img, systemos.img and app.img is checked against its hash list
// before the guest sees a byte of it, and the hash list against the manifest. The kernel is told
// which disk is which, and that it is verified, in the disk's device tree node (imageset.c).
#ifndef XNUHOST_IMAGESET_H
#define XNUHOST_IMAGESET_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "xnuloader/adt.h"
#include "xnuloader/boot.h"

#define XH_IMAGESET_FILES 64

typedef struct {
  char     path[96];            // relative, as in the manifest
  char     role[24];
  char     hashes[96];          // its chunk hash list ("" = none)
  char     volume_uuid[40];     // first APFS volume ("" = not an image)
  uint64_t size;
  uint8_t  sha256[32];
} xh_imageset_file;

typedef struct {
  char     dir[512];
  char     build[16];
  char     data_uuid[40], data_name[32];
  unsigned store_gib;
  unsigned nfiles;
  xh_imageset_file file[XH_IMAGESET_FILES];
} xh_imageset;

// 0 = read; 1 = the folder has no manifest.json (a bench folder: nothing is verified, and the log
// says so); -1 = a manifest that cannot be used, *error says why.
int xh_imageset_load(const char *dir, xh_imageset *set, const char **error);
// The entry whose file is `path` (absolute, under set->dir) or whose manifest path is `path`.
const xh_imageset_file *xh_imageset_find(const xh_imageset *set, const char *path);
const xh_imageset_file *xh_imageset_role(const xh_imageset *set, const char *role);

// ---- chunk verification of a read-only image ----
typedef struct xh_verify xh_verify;
// Reads and checks the hash list of `file` (its SHA-256 is the manifest's). NULL with *error set.
xh_verify *xh_verify_open(const xh_imageset *set, const xh_imageset_file *file, const char **error);
// Checks every chunk that [offset, offset + length) touches and that was not checked before, by
// reading it whole from fd. 0 = all good; -1 = a chunk differs or cannot be read.
int  xh_verify_range(xh_verify *v, int fd, uint64_t offset, uint64_t length);
void xh_verify_close(xh_verify *v);
// The list's SHA-256: what the kernel publishes as the identity of a verified disk.
const uint8_t *xh_verify_digest(const xh_verify *v);
struct xh_blk;
void xh_blk_set_verify(struct xh_blk *b, xh_verify *verify);      // blk.c

// ---- trust caches ----
// DarwinBoot's sidecar (u32 count, u32 offsets[count], version 2 modules) from the set's three
// IM4P trust caches and, when overlay/userland/owner-trust.json lists CDHashes, one owner module.
// malloc'd; { NULL, 0 } with *error set when a file is missing or is not what it should be.
xl_blob xh_imageset_trustcaches(const xh_imageset *set, const char **error);
// The payload of an IM4P ("IM4P", type, description, OCTET STRING): pointer into `data`.
bool xh_im4p_payload(const uint8_t *data, size_t size, const char type[4], const uint8_t **payload, size_t *length);

// ---- the seed helper ----
// overlay/userland/bin/oneios-seed as a disk: copied into a memfd padded to 512 bytes, checked
// against the manifest. Writes "/proc/self/fd/N" to path. 0 = made; 1 = the set has none; -1 = bad.
int xh_imageset_helper_disk(const xh_imageset *set, char *path, size_t path_size, uint64_t *bytes, const char **error);

// ---- the device tree ----
// What the hosted kernel's storage reads from a disk's node (kernel/patches/oneios-hosted, README):
//   oneios,role         "os" | "systemos" | "app" | "data" | "seed" | "seed-helper"
//   oneios,verified     32 bytes, the SHA-256 of the disk's chunk hash list (only on a disk whose
//                       reads xnuhost checks)
//   oneios,volume-uuid  the APFS volume to mount from it (for "data": the UUID asked of newfs_apfs)
//   oneios,volume-name  "data" only
//   oneios,bytes        u64, the file's length ("seed", "seed-helper": the disk is padded)
//   oneios,sha256       64 hex digits + NUL ("seed": what oneios-seed records in the volume)
typedef struct {
  const char    *role;
  const uint8_t *verified;      // NULL = not verified
  const char    *volume_uuid, *volume_name, *sha256_hex;
  uint64_t       bytes;
} xh_disk_identity;
void xh_imageset_describe(xl_adt_node *node, const xh_disk_identity *identity);

// SHA-256 (FIPS 180-4), for the hash lists and the manifest's file hashes.
typedef struct { uint32_t h[8]; uint64_t bytes; uint8_t block[64]; unsigned fill; } xh_sha256;
void xh_sha256_init(xh_sha256 *s);
void xh_sha256_update(xh_sha256 *s, const void *data, size_t length);
void xh_sha256_final(xh_sha256 *s, uint8_t out[32]);

#endif
