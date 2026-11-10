// SPDX-License-Identifier: MIT
// mkkernel ELF KERNEL.KC DISK: wraps the test kernel (a static AArch64 ELF, tests/testkernel.c) as an
// MH_EXECUTE Mach-O the loader accepts, and writes the test disk. Nothing of Apple's is involved.
#include <elf.h>
#include <stdio.h>
#include "fixture.h"

int main(int argc, char **argv)
{
  if (argc != 4) { fprintf(stderr, "usage: mkkernel ELF KERNEL DISK\n"); return 2; }
  FILE *f = fopen(argv[1], "rb");
  if (!f) { perror(argv[1]); return 1; }
  static uint8_t elf[8 << 20];
  size_t size = fread(elf, 1, sizeof elf, f);
  fclose(f);
  Elf64_Ehdr *eh = (Elf64_Ehdr *)elf;
  if (size < sizeof *eh || memcmp(eh->e_ident, ELFMAG, 4) || eh->e_machine != EM_AARCH64) { fprintf(stderr, "not an AArch64 ELF\n"); return 1; }
  Elf64_Phdr *ph = (Elf64_Phdr *)(elf + eh->e_phoff);
  // _start is the first instruction of the text, one page (the Mach-O header's) above the base.
  uint64_t base = eh->e_entry - 0x1000, file_end = 0, mem_end = 0;
  for (int i = 0; i < eh->e_phnum; i++) {
    if (ph[i].p_type != PT_LOAD || ph[i].p_memsz == 0) continue;
    if (ph[i].p_filesz && ph[i].p_vaddr + ph[i].p_filesz > file_end) file_end = ph[i].p_vaddr + ph[i].p_filesz;
    if (ph[i].p_vaddr + ph[i].p_memsz > mem_end) mem_end = ph[i].p_vaddr + ph[i].p_memsz;
  }
  uint64_t text_size = (file_end - base + 0x3fff) & ~0x3fffull;
  uint64_t bss_size = mem_end > base + text_size ? ((mem_end - base - text_size + 0x3fff) & ~0x3fffull) : 0x4000;
  uint8_t *out = calloc(1, text_size);
  for (int i = 0; i < eh->e_phnum; i++) {
    if (ph[i].p_type != PT_LOAD || !ph[i].p_filesz) continue;
    // A segment that starts below the text carries only the ELF's own headers there.
    uint64_t skip = ph[i].p_vaddr < base + 0x1000 ? base + 0x1000 - ph[i].p_vaddr : 0;
    if (skip < ph[i].p_filesz) memcpy(out + (ph[i].p_vaddr + skip - base), elf + ph[i].p_offset + skip, ph[i].p_filesz - skip);
  }
  uint8_t *c = out + 32;
  c += fx_segment(c, "__TEXT", base, text_size, 0, text_size, 7);        // code and initialised data together
  c += fx_segment(c, "__DATA", base + text_size, bss_size, 0, 0, 3);
  c += fx_thread(c, eh->e_entry);
  fx_w32(out, 0xfeedfacf); fx_w32(out + 4, 0x100000c); fx_w32(out + 8, 0); fx_w32(out + 12, 2);
  fx_w32(out + 16, 3); fx_w32(out + 20, (uint32_t)(c - out - 32));
  f = fopen(argv[2], "wb");
  if (!f || fwrite(out, 1, text_size, f) != text_size) { perror(argv[2]); return 1; }
  fclose(f);
  // The disk: 2048 sectors; sector n starts with "ONEIOSTD", its number, then (n ^ 0x5a) bytes.
  f = fopen(argv[3], "wb");
  if (!f) { perror(argv[3]); return 1; }
  for (uint32_t n = 0; n < 2048; n++) {
    uint8_t sector[512];
    memset(sector, (uint8_t)(n ^ 0x5a), sizeof sector);
    memcpy(sector, "ONEIOSTD", 8);
    memcpy(sector + 8, &n, 4);
    fwrite(sector, 1, sizeof sector, f);
  }
  fclose(f);
  printf("mkkernel: base 0x%llx, text 0x%llx, bss 0x%llx, entry 0x%llx\n", (unsigned long long)base, (unsigned long long)text_size,
         (unsigned long long)bss_size, (unsigned long long)eh->e_entry);
  return 0;
}
