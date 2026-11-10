// SPDX-License-Identifier: MIT
// xnuloader-inspect: what the loader sees in a kernel collection or an Apple device tree.
//   xnuloader-inspect kernel FILE     segments, entry, fileset members
//   xnuloader-inspect adt FILE        the tree (iOSPlus10's external/slop/tools/adt.py dump, in C)
//   xnuloader-inspect board NCPU      the tree xnuhost builds for its board, flat, on stdout
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "xnuloader/adt.h"
#include "xnuloader/machine.h"
#include "xnuloader/macho.h"

static uint8_t *slurp(const char *path, size_t *size)
{
  FILE *f = fopen(path, "rb");
  if (!f) { perror(path); exit(1); }
  fseek(f, 0, SEEK_END);
  long n = ftell(f);
  rewind(f);
  uint8_t *data = malloc(n > 0 ? (size_t)n : 1);
  if (n < 0 || !data || fread(data, 1, (size_t)n, f) != (size_t)n) { fprintf(stderr, "%s: read failed\n", path); exit(1); }
  fclose(f);
  *size = (size_t)n;
  return data;
}

int main(int argc, char **argv)
{
  const char *err = NULL;
  size_t size;
  if (argc == 3 && !strcmp(argv[1], "kernel")) {
    static xl_kernel k;
    uint8_t *data = slurp(argv[2], &size);
    if (xl_macho_parse(data, size, &k, &err)) { fprintf(stderr, "%s: %s\n", argv[2], err); return 1; }
    printf("%s %s base 0x%llx end 0x%llx entry 0x%llx\n", k.fileset ? "MH_FILESET" : "MH_EXECUTE",
           k.cpusubtype == 2 ? "arm64e" : "arm64", (unsigned long long)k.base, (unsigned long long)k.end, (unsigned long long)k.entry);
    for (unsigned i = 0; i < k.nseg; i++)
      printf("  %-16s va 0x%llx size 0x%llx file 0x%llx+0x%llx prot %u\n", k.seg[i].name, (unsigned long long)k.seg[i].vmaddr,
             (unsigned long long)k.seg[i].vmsize, (unsigned long long)k.seg[i].fileoff, (unsigned long long)k.seg[i].filesize, k.seg[i].prot);
    for (unsigned i = 0; i < k.nentries; i++)
      printf("  member %-48s va 0x%llx file 0x%llx\n", k.entries[i].name, (unsigned long long)k.entries[i].vmaddr, (unsigned long long)k.entries[i].fileoff);
    return 0;
  }
  if (argc == 3 && !strcmp(argv[1], "adt")) {
    uint8_t *data = slurp(argv[2], &size);
    xl_adt_node *root = xl_adt_parse(data, size, &err);
    if (!root) { fprintf(stderr, "%s: %s\n", argv[2], err); return 1; }
    xl_adt_dump(root, stdout, 0);
    return 0;
  }
  if (argc == 3 && !strcmp(argv[1], "board")) {
    static const xl_device_kind kinds[] = { XL_DEV_BLOCK, XL_DEV_BLOCK, XL_DEV_BLOCK, XL_DEV_DATA_STORE, XL_DEV_INPUT, XL_DEV_GPU };
    xl_machine m;
    memset(&m, 0, sizeof m);
    xl_machine_board(&m, (unsigned)atoi(argv[2]), kinds, 6);
    m.timebase_hz = 19200000;
    m.dram_base = 0x800000000ull;
    m.dram_size = 0x100000000ull;
    xl_adt_node *root = xl_machine_tree(&m, &err);
    if (!root) { fprintf(stderr, "board: %s\n", err); return 1; }
    size = xl_adt_flat_size(root);
    uint8_t *flat = malloc(size);
    xl_adt_flatten(root, flat);
    fwrite(flat, 1, size, stdout);
    return 0;
  }
  fprintf(stderr, "usage: xnuloader-inspect kernel FILE | adt FILE | board NCPU\n");
  return 2;
}
