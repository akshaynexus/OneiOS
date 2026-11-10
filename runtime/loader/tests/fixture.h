// SPDX-License-Identifier: MIT
// Generated Mach-O fixtures for the loader tests: no Apple binaries. The layout follows DarwinBoot's
// Test/make-fixtures.py (a __TEXT with the header and code, a zero-fill __DATA, LC_UNIXTHREAD).
#ifndef XL_TEST_FIXTURE_H
#define XL_TEST_FIXTURE_H
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define FX_BASE 0xfffffff007004000ull

typedef struct { uint8_t *data; size_t size; } fx_image;

static inline void fx_w32(uint8_t *p, uint32_t v) { memcpy(p, &v, 4); }
static inline void fx_w64(uint8_t *p, uint64_t v) { memcpy(p, &v, 8); }

static inline size_t fx_segment(uint8_t *p, const char *name, uint64_t addr, uint64_t size, uint64_t off, uint64_t len, uint32_t prot)
{
  memset(p, 0, 72);
  fx_w32(p, 0x19); fx_w32(p + 4, 72);
  memcpy(p + 8, name, strlen(name) < 16 ? strlen(name) : 16);
  fx_w64(p + 24, addr); fx_w64(p + 32, size); fx_w64(p + 40, off); fx_w64(p + 48, len);
  fx_w32(p + 56, prot); fx_w32(p + 60, prot);
  return 72;
}

static inline size_t fx_thread(uint8_t *p, uint64_t pc)
{
  memset(p, 0, 288);
  fx_w32(p, 5); fx_w32(p + 4, 288); fx_w32(p + 8, 6); fx_w32(p + 12, 68);
  fx_w64(p + 272, pc);
  return 288;
}

// MH_EXECUTE at `base`: __TEXT (header, `code` at +0x1000; text_size bytes, at least 0x2000) and a
// zero-fill __DATA of data_size after it. With `symbols`, an LC_SYMTAB with
// _fixture_symbol = base + 0x1008 (tables inside __TEXT's file bytes at 0x800).
static inline fx_image fx_kernel_sized(uint64_t base, uint32_t subtype, const uint8_t *code, size_t code_size, int symbols,
                                       size_t text_size, size_t data_size)
{
  fx_image im = { calloc(1, text_size), text_size };
  uint8_t *c = im.data + 32;
  c += fx_segment(c, "__TEXT", base, text_size, 0, text_size, 5);
  c += fx_segment(c, "__DATA", base + text_size + 0x2000, data_size, 0, 0, 3);
  c += fx_thread(c, base + 0x1000);
  uint32_t ncmds = 3;
  if (symbols) {
    fx_w32(c, 2); fx_w32(c + 4, 24); fx_w32(c + 8, 0x800); fx_w32(c + 12, 1); fx_w32(c + 16, 0x810); fx_w32(c + 20, 32);
    c += 24; ncmds++;
    uint8_t *n = im.data + 0x800;
    fx_w32(n, 1); n[4] = 0x0f; n[5] = 1; fx_w64(n + 8, base + 0x1008);
    memcpy(im.data + 0x811, "_fixture_symbol", 16);
  }
  fx_w32(im.data, 0xfeedfacf); fx_w32(im.data + 4, 0x100000c); fx_w32(im.data + 8, subtype); fx_w32(im.data + 12, 2);
  fx_w32(im.data + 16, ncmds); fx_w32(im.data + 20, (uint32_t)(c - im.data - 32));
  if (code_size > text_size - 0x1000) abort();
  if (code) memcpy(im.data + 0x1000, code, code_size);
  return im;
}

static inline fx_image fx_kernel(uint64_t base, uint32_t subtype, const uint8_t *code, size_t code_size, int symbols)
{
  return fx_kernel_sized(base, subtype, code, code_size, symbols, 0x2000, 0x1000);
}

// MH_FILESET kernel collection: collection header page at `base`, then the kernel member (an
// MH_EXECUTE as above, MH_DYLIB_IN_CACHE set, file offsets absolute) at +0x4000, then one kext
// member "com.example.kext" at +0xc000.
static inline fx_image fx_collection(uint64_t base, uint32_t subtype, const uint8_t *code, size_t code_size)
{
  fx_image im = { calloc(1, 0x10000), 0x10000 };
  uint64_t kbase = base + 0x4000;
  fx_image k = fx_kernel(kbase, subtype, code, code_size, 1);
  memcpy(im.data + 0x4000, k.data, k.size);
  free(k.data);
  uint8_t *m = im.data + 0x4000;
  fx_w32(m + 24, 0x80000000u);                            // MH_DYLIB_IN_CACHE
  fx_w64(m + 32 + 40, 0x4000);                            // __TEXT fileoff, absolute
  fx_w32(m + 32 + 72 + 72 + 288 + 8, 0x4000 + 0x800);     // symoff
  fx_w32(m + 32 + 72 + 72 + 288 + 16, 0x4000 + 0x810);    // stroff
  uint8_t *x = im.data + 0xc000;                          // the kext member: header only
  fx_w32(x, 0xfeedfacf); fx_w32(x + 4, 0x100000c); fx_w32(x + 8, subtype); fx_w32(x + 12, 11); fx_w32(x + 24, 0x80000000u);
  uint8_t *c = im.data + 32;
  c += fx_segment(c, "__TEXT", base, 0x4000, 0, 0x4000, 1);
  c += fx_segment(c, "__PRELINK_TEXT", kbase, 0x2000, 0x4000, 0x2000, 5);
  c += fx_segment(c, "__DATA", kbase + 0x4000, 0x1000, 0, 0, 3);
  c += fx_segment(c, "__KEXT", base + 0xc000, 0x4000, 0xc000, 0x4000, 5);
  c += fx_thread(c, kbase + 0x1000);
  const char *names[2] = { "com.apple.kernel", "com.example.kext" };
  const uint64_t offs[2] = { 0x4000, 0xc000 };
  for (int i = 0; i < 2; i++) {
    memset(c, 0, 56);
    fx_w32(c, 0x80000035u); fx_w32(c + 4, 56); fx_w64(c + 8, base + offs[i]); fx_w64(c + 16, offs[i]); fx_w32(c + 24, 32);
    strcpy((char *)c + 32, names[i]);
    c += 56;
  }
  fx_w32(im.data, 0xfeedfacf); fx_w32(im.data + 4, 0x100000c); fx_w32(im.data + 8, subtype); fx_w32(im.data + 12, 12);
  fx_w32(im.data + 16, 7); fx_w32(im.data + 20, (uint32_t)(c - im.data - 32));
  return im;
}

// One version 2 trust cache module with `entries` sorted CDHashes, in the sidecar envelope.
static inline fx_image fx_trustcache(unsigned entries)
{
  fx_image im = { calloc(1, 8 + 24 + entries * 24), 8 + 24 + entries * 24 };
  fx_w32(im.data, 1); fx_w32(im.data + 4, 8);
  fx_w32(im.data + 8, 2);
  for (int i = 0; i < 16; i++) im.data[12 + i] = (uint8_t)i;
  fx_w32(im.data + 28, entries);
  for (unsigned e = 0; e < entries; e++) {
    uint8_t *h = im.data + 32 + e * 24;
    memset(h, 0x10 + e, 20);
    h[20] = 2;
  }
  return im;
}

#endif
