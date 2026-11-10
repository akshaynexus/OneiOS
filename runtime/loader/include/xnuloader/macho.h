// SPDX-License-Identifier: BSD-2-Clause-Patent
// Kernel Mach-O / kernel collection parser of the OneiOS loader.
// Ported from DarwinBoot (https://github.com/UniXNU/DarwinBoot, commit 8544957,
// DarwinBootPkg/Driver/DarwinBoot.c: ParseKernelHeader, ParseKernel), Copyright (c) 2026 DarwinBoot
// contributors, BSD-2-Clause-Patent. Changes: plain C without EDK2, error strings, segment names,
// no 256 MiB window limit (the limit is the caller's), a symbol lookup and a fileset entry list.
#ifndef XNULOADER_MACHO_H
#define XNULOADER_MACHO_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define XL_MAX_SEGMENTS 64
#define XL_MAX_ENTRIES  1024

#define XL_MH_MAGIC_64      0xfeedfacfu
#define XL_CPU_TYPE_ARM64   0x0100000cu
#define XL_MH_EXECUTE       2u
#define XL_MH_FILESET       12u
#define XL_LC_SEGMENT_64    0x19u
#define XL_LC_UNIXTHREAD    0x5u
#define XL_LC_SYMTAB        0x2u
#define XL_LC_FILESET_ENTRY 0x80000035u
#define XL_KERNEL_ID        "com.apple.kernel"

typedef struct {
  char     name[17];
  uint64_t vmaddr, vmsize, fileoff, filesize;
  uint32_t prot;            // initprot: 1 read, 2 write, 4 execute
} xl_segment;

typedef struct {
  char     name[64];        // bundle identifier
  uint64_t vmaddr, fileoff; // of the member's Mach-O header
} xl_fileset_entry;

typedef struct {
  xl_segment seg[XL_MAX_SEGMENTS];
  unsigned   nseg;
  uint64_t   base, end;     // lowest segment address, highest segment end (link addresses)
  uint64_t   entry;         // LC_UNIXTHREAD pc (link address)
  uint32_t   cpusubtype;    // low 24 bits: 0 arm64, 2 arm64e
  bool       fileset;       // MH_FILESET kernel collection
  uint64_t   kernel_header; // file offset of the kernel's own header (0 for MH_EXECUTE)
  xl_fileset_entry entries[XL_MAX_ENTRIES];
  unsigned   nentries;
} xl_kernel;

// Parses an arm64/arm64e MH_EXECUTE kernel or an MH_FILESET kernel collection. Returns 0 and fills
// *kernel, or -1 with *error set to a static string. Nothing in the file is trusted: every offset
// and size is checked against `size`.
int xl_macho_parse(const uint8_t *data, size_t size, xl_kernel *kernel, const char **error);

// Value of an external or local symbol of the Mach-O whose header is at `header` (the kernel's own
// header: kernel->kernel_header). Returns 0 and the link address, -1 if absent or the tables are bad.
int xl_macho_symbol(const uint8_t *data, size_t size, uint64_t header, const char *name, uint64_t *value);

// File offset of link address `va`, or -1 if no segment's file bytes cover [va, va+len).
int64_t xl_macho_offset(const xl_kernel *kernel, uint64_t va, uint64_t len);

#endif
