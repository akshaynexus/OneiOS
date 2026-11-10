// SPDX-License-Identifier: BSD-2-Clause-Patent
// Ported from DarwinBoot (https://github.com/UniXNU/DarwinBoot, commit 8544957), Copyright (c) 2026
// DarwinBoot contributors. See include/xnuloader/macho.h for the list of changes.
#include "xnuloader/macho.h"
#include <string.h>

static uint32_t u32(const uint8_t *p) { uint32_t v; memcpy(&v, p, 4); return v; }
static uint64_t u64(const uint8_t *p) { uint64_t v; memcpy(&v, p, 8); return v; }

#define FAIL(text) do { *error = (text); return -1; } while (0)

// One Mach-O header at file offset `header`: the collection's own (header 0) or a member's.
static int parse_header(const uint8_t *data, size_t size, size_t header, xl_kernel *k, const char **error)
{
  if (header > size || size - header < 32) FAIL("file too small for a Mach-O header");
  if (u32(data + header) != XL_MH_MAGIC_64) FAIL("not a 64-bit Mach-O");
  if (u32(data + header + 4) != XL_CPU_TYPE_ARM64) FAIL("not an arm64 Mach-O");
  uint32_t subtype = u32(data + header + 8) & 0x00ffffff;
  if (subtype != 0 && subtype != 2) FAIL("CPU subtype is neither arm64 nor arm64e");
  uint32_t type = u32(data + header + 12);
  if (type != XL_MH_EXECUTE && !(header == 0 && type == XL_MH_FILESET)) FAIL("not MH_EXECUTE or a top-level MH_FILESET");
  if (u32(data + header + 20) > size - header - 32) FAIL("load commands run past the file");

  memset(k, 0, sizeof *k);
  k->cpusubtype = subtype;
  k->fileset = type == XL_MH_FILESET;
  uint32_t count = u32(data + header + 16);
  size_t end = header + 32 + u32(data + header + 20), offset = header + 32;
  bool has_entry = false;
  for (uint32_t i = 0; i < count; i++) {
    if (end - offset < 8) FAIL("truncated load command");
    uint32_t command = u32(data + offset), length = u32(data + offset + 4);
    if (length < 8 || (length & 7) || length > end - offset) FAIL("bad load command length");
    if (command == XL_LC_SEGMENT_64) {
      if (length < 72 || u32(data + offset + 64) > (length - 72) / 80) FAIL("bad segment command");
      if (u64(data + offset + 32) != 0) {
        if (k->nseg == XL_MAX_SEGMENTS) FAIL("too many segments");
        xl_segment *s = &k->seg[k->nseg++];
        memcpy(s->name, data + offset + 8, 16);
        s->name[16] = 0;
        s->vmaddr = u64(data + offset + 24);
        s->vmsize = u64(data + offset + 32);
        s->fileoff = u64(data + offset + 40);
        s->filesize = u64(data + offset + 48);
        s->prot = u32(data + offset + 60);
        if (s->vmaddr > UINT64_MAX - s->vmsize || s->filesize > s->vmsize || s->fileoff > size ||
            s->filesize > size - s->fileoff) FAIL("segment outside the file or wrapping");
      }
    } else if (command == XL_LC_UNIXTHREAD) {
      // ARM_THREAD_STATE64 (flavor 6, 68 words): pc is the 33rd 64-bit register.
      if (has_entry || length != 288 || u32(data + offset + 8) != 6 || u32(data + offset + 12) != 68)
        FAIL("bad or repeated LC_UNIXTHREAD");
      k->entry = u64(data + offset + 272);
      has_entry = true;
    }
    offset += length;
  }
  if (offset != end) FAIL("load commands do not fill their size");
  if (!has_entry) FAIL("no LC_UNIXTHREAD entry point");
  if (k->nseg == 0) FAIL("no segments");
  k->base = UINT64_MAX;
  bool executable = false;
  for (unsigned i = 0; i < k->nseg; i++) {
    const xl_segment *s = &k->seg[i];
    if (s->vmaddr < k->base) k->base = s->vmaddr;
    if (s->vmaddr + s->vmsize > k->end) k->end = s->vmaddr + s->vmsize;
    if (k->entry >= s->vmaddr && k->entry - s->vmaddr < s->filesize && (s->prot & 4)) executable = true;
    for (unsigned o = 0; o < i; o++)
      if (s->vmaddr < k->seg[o].vmaddr + k->seg[o].vmsize && k->seg[o].vmaddr < s->vmaddr + s->vmsize)
        FAIL("segments overlap");
  }
  if (!executable) FAIL("entry point is not in an executable segment's file bytes");
  if (k->entry & 3) FAIL("entry point is not instruction aligned");
  if (k->base & 0xfff) FAIL("kernel base is not page aligned");
  return 0;
}

// Fileset segment offsets are absolute in the collection. XNU rebases and signs its own fixup chains
// after entry; the loader only copies the collection's mappings.
int xl_macho_parse(const uint8_t *data, size_t size, xl_kernel *k, const char **error)
{
  const char *ignored;
  if (!error) error = &ignored;
  *error = NULL;
  if (parse_header(data, size, 0, k, error)) return -1;
  if (!k->fileset) return 0;

  static xl_kernel member;   // too large for a small stack; the loader is single-threaded here
  size_t offset = 32, end = 32 + u32(data + 20);
  bool found = false;
  while (offset < end) {
    uint32_t length = u32(data + offset + 4);
    if (u32(data + offset) == XL_LC_FILESET_ENTRY) {
      if (length < 40) FAIL("bad fileset entry");
      size_t name = u32(data + offset + 24), i;
      if (name < 32 || name >= length) FAIL("bad fileset entry name offset");
      for (i = name; i < length && data[offset + i] != 0; i++) {}
      if (i == length) FAIL("unterminated fileset entry name");
      uint64_t vmaddr = u64(data + offset + 8), header = u64(data + offset + 16);
      if (header > size || size - header < 32 || (header & 7)) FAIL("fileset member header outside the file");
      // Every indexed header must actually be loaded at its advertised address.
      bool covered = false;
      for (unsigned o = 0; o < k->nseg; o++) {
        const xl_segment *p = &k->seg[o];
        if (header >= p->fileoff && header - p->fileoff <= p->filesize &&
            32 <= p->filesize - (header - p->fileoff) && vmaddr == p->vmaddr + header - p->fileoff) covered = true;
      }
      if (!covered) FAIL("fileset member header is not mapped at its address");
      if (k->nentries == XL_MAX_ENTRIES) FAIL("too many fileset entries");
      xl_fileset_entry *e = &k->entries[k->nentries++];
      size_t n = i - name < sizeof e->name - 1 ? i - name : sizeof e->name - 1;
      memcpy(e->name, data + offset + name, n);
      e->name[n] = 0;
      e->vmaddr = vmaddr;
      e->fileoff = header;
      if (i - name == sizeof XL_KERNEL_ID - 1 && !memcmp(data + offset + name, XL_KERNEL_ID, sizeof XL_KERNEL_ID)) {
        if (found) FAIL("two com.apple.kernel entries");
        if (u32(data + header + 12) != XL_MH_EXECUTE || u32(data + header + 8) != u32(data + 8) ||
            !(u32(data + header + 24) & 0x80000000u)) FAIL("com.apple.kernel is not an MH_EXECUTE member of this collection");
        found = true;
        if (parse_header(data, size, header, &member, error)) return -1;
        if (member.entry != k->entry) FAIL("the collection's entry is not the kernel's");
        for (unsigned c = 0; c < member.nseg; c++) {
          const xl_segment *ch = &member.seg[c];
          covered = false;
          for (unsigned o = 0; o < k->nseg; o++) {
            const xl_segment *p = &k->seg[o];
            if (ch->vmaddr >= p->vmaddr && ch->vmaddr - p->vmaddr <= p->vmsize &&
                ch->vmsize <= p->vmsize - (ch->vmaddr - p->vmaddr) && !(ch->prot & ~p->prot) &&
                (ch->filesize == 0 ||
                 (ch->vmaddr - p->vmaddr <= p->filesize && ch->filesize <= p->filesize - (ch->vmaddr - p->vmaddr) &&
                  ch->fileoff == p->fileoff + ch->vmaddr - p->vmaddr))) covered = true;
          }
          if (!covered) FAIL("a kernel segment is not inside a collection segment");
        }
        k->kernel_header = header;
      }
    }
    offset += length;
  }
  if (!found) FAIL("the collection has no com.apple.kernel");
  return 0;
}

int64_t xl_macho_offset(const xl_kernel *k, uint64_t va, uint64_t len)
{
  for (unsigned i = 0; i < k->nseg; i++) {
    const xl_segment *s = &k->seg[i];
    if (va >= s->vmaddr && va - s->vmaddr <= s->filesize && len <= s->filesize - (va - s->vmaddr))
      return (int64_t)(s->fileoff + (va - s->vmaddr));
  }
  return -1;
}

int xl_macho_symbol(const uint8_t *data, size_t size, uint64_t header, const char *name, uint64_t *value)
{
  if (header > size || size - header < 32 || u32(data + header) != XL_MH_MAGIC_64) return -1;
  uint32_t count = u32(data + header + 16), bytes = u32(data + header + 20);
  if (bytes > size - header - 32) return -1;
  size_t offset = header + 32, end = offset + bytes;
  for (uint32_t i = 0; i < count && end - offset >= 8; i++) {
    uint32_t command = u32(data + offset), length = u32(data + offset + 4);
    if (length < 8 || length > end - offset) return -1;
    if (command == XL_LC_SYMTAB && length >= 24) {
      uint64_t symoff = u32(data + offset + 8), nsyms = u32(data + offset + 12);
      uint64_t stroff = u32(data + offset + 16), strsize = u32(data + offset + 20);
      if (symoff > size || nsyms > (size - symoff) / 16 || stroff > size || strsize > size - stroff) return -1;
      size_t want = strlen(name);
      for (uint64_t s = 0; s < nsyms; s++) {
        const uint8_t *n = data + symoff + s * 16;
        uint32_t strx = u32(n);
        if ((n[4] & 0xe0) || (n[4] & 0x0e) != 0x0e) continue;        // not a stab, defined in a section
        if (strx >= strsize || strsize - strx <= want) continue;
        if (!memcmp(data + stroff + strx, name, want + 1)) { *value = u64(n + 8); return 0; }
      }
      return -1;
    }
    offset += length;
  }
  return -1;
}
