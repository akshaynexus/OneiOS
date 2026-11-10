// SPDX-License-Identifier: MIT
// Host tests of imageset.c on generated stand-in files (no Apple files): the manifest reader, chunk
// verification, the trust cache sidecar from IM4P files, the seed helper disk.
// With ONEIOS_IMAGE_SET=DIR it also reads a real set's manifest, hash lists and trust caches.
#define _GNU_SOURCE
#include "imageset.h"
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int checks, failed;
#define CHECK(cond) do { checks++; if (!(cond)) { failed++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

static char dir[256];

static void put(const char *name, const void *data, size_t size)
{
  char path[512];
  snprintf(path, sizeof path, "%s/%s", dir, name);
  FILE *f = fopen(path, "wb");
  if (!f || fwrite(data, 1, size, f) != size) { perror(path); exit(2); }
  fclose(f);
}

static void hex(const uint8_t *d, size_t n, char *out) { for (size_t i = 0; i < n; i++) sprintf(out + i * 2, "%02x", d[i]); }

static void digest_of(const void *data, size_t size, char *out)
{
  uint8_t d[32];
  xh_sha256 s;
  xh_sha256_init(&s);
  xh_sha256_update(&s, data, size);
  xh_sha256_final(&s, d);
  hex(d, 32, out);
}

// An IM4P around a version 2 trust cache module with `count` entries whose first byte is `first + i`.
static size_t im4p(uint8_t *out, const char *type, unsigned count, uint8_t first)
{
  size_t payload = 24 + count * 24, body = 6 + 6 + 3 + 4 + payload;
  uint8_t *p = out;
  *p++ = 0x30; *p++ = 0x82; *p++ = (uint8_t)(body >> 8); *p++ = (uint8_t)body;
  *p++ = 0x16; *p++ = 4; memcpy(p, "IM4P", 4); p += 4;
  *p++ = 0x16; *p++ = 4; memcpy(p, type, 4); p += 4;
  *p++ = 0x16; *p++ = 1; *p++ = '1';
  *p++ = 0x04; *p++ = 0x82; *p++ = (uint8_t)(payload >> 8); *p++ = (uint8_t)payload;
  memset(p, 0, payload);
  p[0] = 2; memset(p + 4, 0xab, 16); p[20] = (uint8_t)count;
  for (unsigned i = 0; i < count; i++) { p[24 + i * 24] = (uint8_t)(first + i); p[24 + i * 24 + 20] = 2; }
  return (size_t)(p + payload - out);
}

static size_t manifest_length;
static char manifest[8192];
static void entry(const char *path, const void *data, size_t size, const char *more)
{
  char digest[65];
  digest_of(data, size, digest);
  put(path, data, size);
  manifest_length += (size_t)snprintf(manifest + manifest_length, sizeof manifest - manifest_length,
                                      "%s\n    {\"path\": \"%s\", \"size\": %zu, \"sha256\": \"%s\"%s}", strstr(manifest, "\"path\"") ? "," : "", path, size, digest, more);
}

int main(void)
{
  const char *error = NULL;
  static const uint8_t abc[32] = { 0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23,
                                   0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad };
  uint8_t d[32];
  xh_sha256 s;
  xh_sha256_init(&s); xh_sha256_update(&s, "abc", 3); xh_sha256_final(&s, d);
  CHECK(!memcmp(d, abc, 32));
  // the same in pieces, across the block boundary
  static uint8_t million[1000000];
  memset(million, 'a', sizeof million);
  xh_sha256_init(&s);
  for (size_t at = 0; at < sizeof million; at += 977) xh_sha256_update(&s, million + at, sizeof million - at < 977 ? sizeof million - at : 977);
  xh_sha256_final(&s, d);
  CHECK(d[0] == 0xcd && d[1] == 0xc7 && d[2] == 0x6e && d[31] == 0xd0);

  snprintf(dir, sizeof dir, "%s/test-imageset-XXXXXX", getenv("TMPDIR") && *getenv("TMPDIR") ? getenv("TMPDIR") : "/tmp");
  if (!mkdtemp(dir)) { perror("mkdtemp"); return 2; }
  char sub[512];
  snprintf(sub, sizeof sub, "%s/trust", dir); mkdir(sub, 0700);
  snprintf(sub, sizeof sub, "%s/overlay", dir); mkdir(sub, 0700);
  snprintf(sub, sizeof sub, "%s/overlay/userland", dir); mkdir(sub, 0700);
  snprintf(sub, sizeof sub, "%s/overlay/userland/bin", dir); mkdir(sub, 0700);

  // An image of two and a half chunks, and its hash list.
  enum { CHUNK = 1 << 20, IMAGE = CHUNK * 2 + CHUNK / 2 };
  uint8_t *image = malloc(IMAGE), list[32 + 3 * 32] = "OIHL1";
  for (size_t i = 0; i < IMAGE; i++) image[i] = (uint8_t)(i * 2654435761u >> 24);
  list[8] = 0; list[9] = 0; list[10] = 0x10; list[11] = 0;           // 1 MiB
  list[12] = 1;
  list[16] = 0; list[17] = 0; list[18] = 0x28;                        // 0x280000
  list[24] = 3;
  for (unsigned c = 0; c < 3; c++) {
    xh_sha256_init(&s);
    xh_sha256_update(&s, image + (size_t)c * CHUNK, c == 2 ? CHUNK / 2 : CHUNK);
    xh_sha256_final(&s, list + 32 + c * 32);
  }
  uint8_t trust[3][1024], helper[700];
  size_t trust_size[3] = { im4p(trust[0], "trst", 3, 0x10), im4p(trust[1], "trcs", 2, 0x40), im4p(trust[2], "trca", 1, 0x80) };
  memset(helper, 0x5a, sizeof helper);
  static const char owner[] = "{\"schema\": 1, \"cdhashes\": [\"ff000000000000000000000000000000000000aa\", \"0100000000000000000000000000000000000001\","
                              " \"ff000000000000000000000000000000000000aa\"], \"files\": []}";
  manifest_length = (size_t)snprintf(manifest, sizeof manifest,
    "{\n  \"format\": 1, \"name\": \"oneios-image\", \"build\": \"24A446\",\n  \"data\": {\"seed\": \"data.seed\", \"volume_uuid\": \"1B7DB2B7-5ECA-4FDA-98AF-55DF8632E355\","
    " \"volume_name\": \"Data\", \"case_sensitive\": true, \"store_gib\": 16},\n  \"tweaks\": {\"info\": {\"nested\": [1, {\"x\": \"a\\\"b\"}]}},\n  \"files\": [");
  entry("data.seed", "seed", 4, ", \"role\": \"data-seed\", \"format\": \"tar\"");
  entry("os.img", image, IMAGE, ", \"role\": \"os\", \"hashes\": \"os.img.hashes\", \"apfs\": {\"container_offset\": 0, \"volumes\": [{\"name\": \"OS\", \"uuid\": \"BBA627F7-F5DC-47E9-BD09-F4186021465B\", \"sealed\": false}]}");
  entry("os.img.hashes", list, sizeof list, ", \"role\": \"hashes\"");
  entry("overlay/userland/bin/oneios-seed", helper, sizeof helper, ", \"role\": \"overlay\"");
  entry("overlay/userland/owner-trust.json", owner, sizeof owner - 1, ", \"role\": \"overlay\"");
  entry("trust/app.trustcache", trust[2], trust_size[2], ", \"role\": \"trust-app\"");
  entry("trust/static.trustcache", trust[0], trust_size[0], ", \"role\": \"trust-static\"");
  entry("trust/system.trustcache", trust[1], trust_size[1], ", \"role\": \"trust-system\"");
  manifest_length += (size_t)snprintf(manifest + manifest_length, sizeof manifest - manifest_length, "\n  ]\n}\n");
  put("manifest.json", manifest, manifest_length);

  static xh_imageset set;
  CHECK(xh_imageset_load(dir, &set, &error) == 0);
  CHECK(set.nfiles == 8 && !strcmp(set.build, "24A446") && set.store_gib == 16 && !strcmp(set.data_name, "Data"));
  CHECK(!strcmp(set.data_uuid, "1B7DB2B7-5ECA-4FDA-98AF-55DF8632E355"));
  char path[600];
  snprintf(path, sizeof path, "%s/os.img", dir);
  const xh_imageset_file *os = xh_imageset_find(&set, path);
  CHECK(os && os == xh_imageset_role(&set, "os") && os->size == IMAGE && !strcmp(os->volume_uuid, "BBA627F7-F5DC-47E9-BD09-F4186021465B"));
  CHECK(!xh_imageset_find(&set, "nothing.img") && xh_imageset_role(&set, "data-seed"));

  // chunk verification: the image as written, then with one byte of the last chunk changed
  xh_verify *v = os ? xh_verify_open(&set, os, &error) : NULL;
  CHECK(v != NULL);
  int fd = open(path, O_RDWR);
  if (v) {
    CHECK(xh_verify_range(v, fd, 0, 512) == 0);
    CHECK(xh_verify_range(v, fd, CHUNK - 512, 1024) == 0);              // across two chunks
    CHECK(xh_verify_range(v, fd, IMAGE - 512, 1024) == -1);             // past the end
    CHECK(xh_verify_range(v, fd, IMAGE, 0) == 0);
    uint8_t flipped = (uint8_t)~image[IMAGE - 1];
    CHECK(pwrite(fd, &flipped, 1, IMAGE - 1) == 1);
    CHECK(xh_verify_range(v, fd, 2 * CHUNK, 512) == -1);                // the changed chunk is refused
    CHECK(xh_verify_range(v, fd, 0, 2 * CHUNK) == 0);                   // the others still pass
    CHECK(pwrite(fd, &image[IMAGE - 1], 1, IMAGE - 1) == 1);
    CHECK(xh_verify_range(v, fd, 2 * CHUNK, CHUNK / 2) == 0);
    CHECK(!memcmp(xh_verify_digest(v), xh_imageset_find(&set, "os.img.hashes")->sha256, 32));
    xh_verify_close(v);
  }
  close(fd);
  // a hash list that is not the manifest's
  list[40] ^= 1;
  put("os.img.hashes", list, sizeof list);
  CHECK(!xh_verify_open(&set, os, &error) && error);
  list[40] ^= 1;
  put("os.img.hashes", list, sizeof list);

  // the trust cache sidecar: three of Apple's modules and the owner's (two CDHashes, the repeated one once)
  xl_blob blob = xh_imageset_trustcaches(&set, &error);
  CHECK(blob.data && xl_trustcaches_valid(blob.data, blob.size));
  if (blob.data) {
    CHECK(blob.data[0] == 4 && blob.size == 4 + 16 + (24 + 72) + (24 + 48) + (24 + 24) + (24 + 48));
    const uint8_t *mine = blob.data + blob.size - 72;
    CHECK(mine[0] == 2 && mine[20] == 2 && mine[24] == 0x01 && mine[48] == 0xff && mine[44] == 2);
    free((void *)blob.data);
  }
  const uint8_t *payload;
  size_t length;
  CHECK(xh_im4p_payload(trust[0], trust_size[0], "trst", &payload, &length) && length == 96 && payload[0] == 2);
  CHECK(!xh_im4p_payload(trust[0], trust_size[0], "trca", &payload, &length));
  CHECK(!xh_im4p_payload(trust[0], trust_size[0] - 1, "trst", &payload, &length));     // cut short
  CHECK(!xh_im4p_payload(trust[0], 3, "trst", &payload, &length));
  // a trust cache that is not the manifest's
  trust[1][trust_size[1] - 1] ^= 1;
  put("trust/system.trustcache", trust[1], trust_size[1]);
  blob = xh_imageset_trustcaches(&set, &error);
  CHECK(!blob.data && error);

  // the helper as a disk: padded to a sector, its true length reported
  uint64_t bytes = 0;
  CHECK(xh_imageset_helper_disk(&set, path, sizeof path, &bytes, &error) == 0 && bytes == sizeof helper);
  struct stat st;
  CHECK(stat(path, &st) == 0 && st.st_size == 1024);

  // the device tree properties
  xl_adt_node *node = xl_adt_new("virtio-mmio-0");
  xh_imageset_describe(node, &(xh_disk_identity){ .role = "os", .verified = abc, .volume_uuid = os ? os->volume_uuid : "" });
  CHECK(xl_adt_get(node, "oneios,role") && xl_adt_get(node, "oneios,verified") && xl_adt_get(node, "oneios,verified")->length == 32);
  CHECK(xl_adt_get(node, "oneios,volume-uuid") && !xl_adt_get(node, "oneios,bytes"));
  xl_adt_free(node);

  // manifests that must be refused, and a folder without one
  static const char *const refused[] = {
    "{\"format\": 2, \"name\": \"oneios-image\", \"build\": \"x\", \"files\": []}",
    "{\"format\": 1, \"name\": \"other\", \"build\": \"x\", \"files\": []}",
    "{\"format\": 1, \"name\": \"oneios-image\", \"build\": \"x\", \"files\": []}",
    "{\"format\": 1, \"name\": \"oneios-image\", \"build\": \"x\", \"files\": [{\"path\": \"../etc\", \"size\": 1, \"sha256\": \"00\"}]}",
    "{\"format\": 1, \"name\": \"oneios-image\", \"build\": \"x\", \"files\": [{\"path\": \"a\", \"size\": 1, \"sha256\": \"zz\"}]}",
    "{\"format\": 1, \"name\": \"oneios-image\", \"build\": \"x\", \"files\": [",
    "",
  };
  for (unsigned i = 0; i < sizeof refused / sizeof *refused; i++) {
    put("manifest.json", refused[i], strlen(refused[i]));
    error = NULL;
    CHECK(xh_imageset_load(dir, &set, &error) == -1 && error);
  }
  snprintf(path, sizeof path, "%s/manifest.json", dir);
  unlink(path);
  CHECK(xh_imageset_load(dir, &set, &error) == 1);
  free(image);

  // A real set, when one is given: every hash list opens, the first and last chunk of each image verify,
  // and Apple's trust caches make a sidecar the loader takes.
  const char *real = getenv("ONEIOS_IMAGE_SET");
  if (real && *real) {
    CHECK(xh_imageset_load(real, &set, &error) == 0);
    static const char *const roles[] = { "os", "systemos", "app" };
    for (unsigned i = 0; i < 3; i++) {
      const xh_imageset_file *f = xh_imageset_role(&set, roles[i]);
      CHECK(f && *f->volume_uuid);
      if (!f) continue;
      snprintf(path, sizeof path, "%s/%s", real, f->path);
      fd = open(path, O_RDONLY);
      v = xh_verify_open(&set, f, &error);
      CHECK(fd >= 0 && v);
      if (fd >= 0 && v) {
        CHECK(xh_verify_range(v, fd, 0, 4096) == 0);
        CHECK(xh_verify_range(v, fd, f->size - 4096, 4096) == 0);
      }
      xh_verify_close(v);
      if (fd >= 0) close(fd);
      printf("real set: %s volume %s, first and last chunk verified\n", f->path, f->volume_uuid);
    }
    blob = xh_imageset_trustcaches(&set, &error);
    CHECK(blob.data && xl_trustcaches_valid(blob.data, blob.size));
    if (blob.data) printf("real set: trust cache sidecar, %u modules, %zu bytes\n", (unsigned)blob.data[0], blob.size);
    else printf("real set: trust caches: %s\n", error);
    free((void *)blob.data);
  }

  char command[400];
  snprintf(command, sizeof command, "rm -rf '%s'", dir);
  if (system(command)) failed++;
  printf("test_imageset: %d checks, %d failed\n", checks, failed);
  return failed != 0;
}
