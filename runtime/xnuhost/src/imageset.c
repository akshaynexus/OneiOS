// SPDX-License-Identifier: MIT
// The image set on xnuhost's side: see imageset.h. Portable C (the host tests build it).
#define _GNU_SOURCE
#include "imageset.h"
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#define FAIL(text) do { *error = (text); goto bad; } while (0)

// ---- SHA-256 ------------------------------------------------------------------------------------
static const uint32_t K[64] = {
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
  0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
  0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
  0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
  0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
  0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2 };
#define ROR(x, n) (((x) >> (n)) | ((x) << (32 - (n))))

static void sha_block(xh_sha256 *s, const uint8_t *p)
{
  uint32_t w[64], v[8];
  for (int i = 0; i < 16; i++) w[i] = (uint32_t)p[i * 4] << 24 | (uint32_t)p[i * 4 + 1] << 16 | (uint32_t)p[i * 4 + 2] << 8 | p[i * 4 + 3];
  for (int i = 16; i < 64; i++)
    w[i] = w[i - 16] + (ROR(w[i - 15], 7) ^ ROR(w[i - 15], 18) ^ (w[i - 15] >> 3)) + w[i - 7] + (ROR(w[i - 2], 17) ^ ROR(w[i - 2], 19) ^ (w[i - 2] >> 10));
  memcpy(v, s->h, sizeof v);
  for (int i = 0; i < 64; i++) {
    uint32_t t1 = v[7] + (ROR(v[4], 6) ^ ROR(v[4], 11) ^ ROR(v[4], 25)) + ((v[4] & v[5]) ^ (~v[4] & v[6])) + K[i] + w[i];
    uint32_t t2 = (ROR(v[0], 2) ^ ROR(v[0], 13) ^ ROR(v[0], 22)) + ((v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]));
    v[7] = v[6]; v[6] = v[5]; v[5] = v[4]; v[4] = v[3] + t1; v[3] = v[2]; v[2] = v[1]; v[1] = v[0]; v[0] = t1 + t2;
  }
  for (int i = 0; i < 8; i++) s->h[i] += v[i];
}

void xh_sha256_init(xh_sha256 *s)
{
  static const uint32_t start[8] = { 0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19 };
  memcpy(s->h, start, sizeof start);
  s->bytes = 0;
  s->fill = 0;
}

void xh_sha256_update(xh_sha256 *s, const void *data, size_t length)
{
  const uint8_t *p = data;
  s->bytes += length;
  if (s->fill) {
    size_t take = 64 - s->fill < length ? 64 - s->fill : length;
    memcpy(s->block + s->fill, p, take);
    s->fill += (unsigned)take; p += take; length -= take;
    if (s->fill < 64) return;
    sha_block(s, s->block);
    s->fill = 0;
  }
  for (; length >= 64; p += 64, length -= 64) sha_block(s, p);
  if (length) { memcpy(s->block, p, length); s->fill = (unsigned)length; }
}

void xh_sha256_final(xh_sha256 *s, uint8_t out[32])
{
  uint64_t bits = s->bytes * 8;
  uint8_t pad[72] = { 0x80 }, tail[8];
  for (int i = 0; i < 8; i++) tail[i] = (uint8_t)(bits >> (56 - i * 8));
  xh_sha256_update(s, pad, (s->fill < 56 ? 56 : 120) - s->fill);
  xh_sha256_update(s, tail, 8);
  for (int i = 0; i < 8; i++) { out[i * 4] = (uint8_t)(s->h[i] >> 24); out[i * 4 + 1] = (uint8_t)(s->h[i] >> 16); out[i * 4 + 2] = (uint8_t)(s->h[i] >> 8); out[i * 4 + 3] = (uint8_t)s->h[i]; }
}

// ---- files --------------------------------------------------------------------------------------
// The whole file with a NUL after it (not counted), or NULL.
static uint8_t *slurp(const char *dir, const char *name, size_t limit, size_t *size)
{
  char path[700];
  struct stat st;
  if (snprintf(path, sizeof path, "%s/%s", dir, name) >= (int)sizeof path) { errno = ENAMETOOLONG; return NULL; }
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return NULL;
  uint8_t *data = NULL;
  if (fstat(fd, &st) == 0 && st.st_size >= 0 && (uint64_t)st.st_size <= limit && (data = malloc((size_t)st.st_size + 1))) {
    size_t done = 0;
    while (done < (size_t)st.st_size) {
      ssize_t n = read(fd, data + done, (size_t)st.st_size - done);
      if (n < 0 && errno == EINTR) continue;
      if (n <= 0) { free(data); data = NULL; break; }
      done += (size_t)n;
    }
    if (data) { data[done] = 0; *size = done; }
  } else if (!data) errno = errno ? errno : EFBIG;
  close(fd);
  return data;
}

static bool hex_bytes(const char *text, uint8_t *out, size_t n)
{
  for (size_t i = 0; i < n * 2; i++) {
    char c = text[i];
    int v = c >= '0' && c <= '9' ? c - '0' : c >= 'a' && c <= 'f' ? c - 'a' + 10 : c >= 'A' && c <= 'F' ? c - 'A' + 10 : -1;
    if (v < 0) return false;
    out[i / 2] = (uint8_t)(i & 1 ? out[i / 2] | v : v << 4);
  }
  return text[n * 2] == 0;
}

// ---- JSON, as much as the manifest needs --------------------------------------------------------
// Values are addressed by a pointer to their first character inside one NUL-terminated text.
static const char *js_ws(const char *p) { while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++; return p; }

static const char *js_string_end(const char *p)         // p at the opening quote; after the closing one, or NULL
{
  for (p++; *p && *p != '"'; p++) if (*p == '\\' && !*++p) return NULL;
  return *p ? p + 1 : NULL;
}

static const char *js_skip(const char *p, int depth)    // after the value at p, or NULL
{
  p = js_ws(p);
  if (depth > 32) return NULL;
  if (*p == '"') return js_string_end(p);
  if (*p == '{' || *p == '[') {
    char close = *p == '{' ? '}' : ']';
    bool object = *p == '{';
    p = js_ws(p + 1);
    if (*p == close) return p + 1;
    for (;;) {
      if (object) {
        if (*p != '"' || !(p = js_string_end(p))) return NULL;
        p = js_ws(p);
        if (*p++ != ':') return NULL;
      }
      if (!(p = js_skip(p, depth + 1))) return NULL;
      p = js_ws(p);
      if (*p == close) return p + 1;
      if (*p++ != ',') return NULL;
      p = js_ws(p);
    }
  }
  const char *start = p;
  while (*p && *p != ',' && *p != '}' && *p != ']' && *p != ' ' && *p != '\n' && *p != '\r' && *p != '\t') p++;
  return p > start ? p : NULL;
}

static const char *js_get(const char *object, const char *key)   // the value of key in the object at `object`, or NULL
{
  if (!object) return NULL;
  const char *p = js_ws(object);
  size_t length = strlen(key);
  if (*p != '{') return NULL;
  p = js_ws(p + 1);
  while (*p == '"') {
    const char *name = p + 1, *end = js_string_end(p);
    if (!end) return NULL;
    p = js_ws(end);
    if (*p++ != ':') return NULL;
    p = js_ws(p);
    if ((size_t)(end - 1 - name) == length && !memcmp(name, key, length)) return p;
    if (!(p = js_skip(p, 0))) return NULL;
    p = js_ws(p);
    if (*p == ',') p = js_ws(p + 1);
  }
  return NULL;
}

static const char *js_first(const char *array)          // first element, or NULL (empty, or not an array)
{
  if (!array) return NULL;
  const char *p = js_ws(array);
  if (*p != '[') return NULL;
  p = js_ws(p + 1);
  return *p == ']' ? NULL : p;
}

static const char *js_next(const char *element)
{
  const char *p = js_skip(element, 0);
  if (!p) return NULL;
  p = js_ws(p);
  return *p == ',' ? js_ws(p + 1) : NULL;
}

static bool js_str(const char *value, char *out, size_t size)   // plain strings only: an escape is refused
{
  if (!value || *value != '"') return false;
  const char *end = strchr(value + 1, '"');
  if (!end || (size_t)(end - value - 1) >= size || memchr(value + 1, '\\', (size_t)(end - value - 1))) return false;
  memcpy(out, value + 1, (size_t)(end - value - 1));
  out[end - value - 1] = 0;
  return true;
}

static bool js_u64(const char *value, uint64_t *out)
{
  if (!value || *value < '0' || *value > '9') return false;
  char *end;
  errno = 0;
  *out = strtoull(value, &end, 10);
  return !errno && (*end == ',' || *end == '}' || *end == ']' || *end == ' ' || *end == '\n' || *end == '\r' || *end == 0);
}

// ---- the manifest -------------------------------------------------------------------------------
static bool path_ok(const char *path)       // docs/image-format.md section 3: relative, plain segments
{
  if (!*path || *path == '/') return false;
  for (const char *p = path, *segment = path; ; p++) {
    if (*p == '/' || !*p) {
      if (p == segment || segment[0] == '.') return false;
      if (!*p) return true;
      segment = p + 1;
    } else if (!((*p >= 'a' && *p <= 'z') || (*p >= 'A' && *p <= 'Z') || (*p >= '0' && *p <= '9') || *p == '.' || *p == '_' || *p == '+' || *p == '-')) return false;
  }
}

int xh_imageset_load(const char *dir, xh_imageset *set, const char **error)
{
  size_t size = 0;
  char text[64];
  uint64_t number = 0;
  memset(set, 0, sizeof *set);
  if (snprintf(set->dir, sizeof set->dir, "%s", dir) >= (int)sizeof set->dir) { *error = "the image folder's path is too long"; return -1; }
  errno = 0;
  char *json = (char *)slurp(dir, "manifest.json", 4 << 20, &size);
  if (!json) { if (errno == ENOENT) return 1; *error = "manifest.json cannot be read"; return -1; }
  if (strlen(json) != size || !js_skip(json, 0)) FAIL("manifest.json is not JSON");
  if (!js_u64(js_get(json, "format"), &number) || number != 1) FAIL("manifest.json: format is not 1");
  if (!js_str(js_get(json, "name"), text, sizeof text) || strcmp(text, "oneios-image")) FAIL("manifest.json: name is not oneios-image");
  if (!js_str(js_get(json, "build"), set->build, sizeof set->build)) FAIL("manifest.json: no build");
  const char *data = js_get(json, "data");
  if (data && *data == '{') {
    if (!js_str(js_get(data, "volume_uuid"), set->data_uuid, sizeof set->data_uuid) ||
        !js_str(js_get(data, "volume_name"), set->data_name, sizeof set->data_name)) FAIL("manifest.json: data.volume_uuid or volume_name");
    if (js_u64(js_get(data, "store_gib"), &number) && number >= 1 && number <= 4096) set->store_gib = (unsigned)number;
  }
  for (const char *entry = js_first(js_get(json, "files")); entry; entry = js_next(entry)) {
    if (set->nfiles == XH_IMAGESET_FILES) FAIL("manifest.json lists more files than xnuhost reads");
    xh_imageset_file *f = &set->file[set->nfiles];
    char digest[72];
    if (!js_str(js_get(entry, "path"), f->path, sizeof f->path) || !path_ok(f->path)) FAIL("manifest.json: a file's path");
    if (!js_u64(js_get(entry, "size"), &f->size)) FAIL("manifest.json: a file's size");
    if (!js_str(js_get(entry, "sha256"), digest, sizeof digest) || !hex_bytes(digest, f->sha256, 32)) FAIL("manifest.json: a file's sha256");
    if (js_get(entry, "role") && !js_str(js_get(entry, "role"), f->role, sizeof f->role)) FAIL("manifest.json: a file's role");
    if (js_get(entry, "hashes") && (!js_str(js_get(entry, "hashes"), f->hashes, sizeof f->hashes) || !path_ok(f->hashes))) FAIL("manifest.json: a file's hash list");
    const char *volume = js_first(js_get(js_get(entry, "apfs"), "volumes"));
    if (volume && !js_str(js_get(volume, "uuid"), f->volume_uuid, sizeof f->volume_uuid)) FAIL("manifest.json: an APFS volume's uuid");
    set->nfiles++;
  }
  if (set->nfiles == 0) FAIL("manifest.json lists no files");
  free(json);
  return 0;
bad:
  free(json);
  return -1;
}

const xh_imageset_file *xh_imageset_find(const xh_imageset *set, const char *path)
{
  size_t dir = strlen(set->dir);
  if (*set->dir && !strncmp(path, set->dir, dir) && path[dir] == '/') path += dir + 1;
  for (unsigned i = 0; i < set->nfiles; i++) if (!strcmp(set->file[i].path, path)) return &set->file[i];
  return NULL;
}

const xh_imageset_file *xh_imageset_role(const xh_imageset *set, const char *role)
{
  for (unsigned i = 0; i < set->nfiles; i++) if (!strcmp(set->file[i].role, role)) return &set->file[i];
  return NULL;
}

// A listed file, whole, checked against the manifest's size and SHA-256.
static uint8_t *listed(const xh_imageset *set, const char *path, size_t limit, size_t *size, const char **error)
{
  const xh_imageset_file *f = xh_imageset_find(set, path);
  uint8_t digest[32];
  xh_sha256 sha;
  if (!f) { *error = "a file xnuhost needs is not in manifest.json"; return NULL; }
  uint8_t *data = slurp(set->dir, f->path, limit, size);
  if (!data) { *error = "a file of the image set cannot be read"; return NULL; }
  xh_sha256_init(&sha);
  xh_sha256_update(&sha, data, *size);
  xh_sha256_final(&sha, digest);
  if (*size != f->size || memcmp(digest, f->sha256, 32)) { free(data); *error = "a file of the image set is not the one manifest.json lists"; return NULL; }
  return data;
}

// ---- chunk verification -------------------------------------------------------------------------
struct xh_verify {
  uint8_t *list;                // the hash list file
  uint64_t size, count;         // of the image
  uint32_t chunk;
  uint8_t  digest[32];
  uint8_t *done;                // one bit per verified chunk
  uint8_t *buffer;              // one chunk
  pthread_mutex_t lock;
};

static uint32_t le32(const uint8_t *p) { return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24; }
static uint64_t le64(const uint8_t *p) { return le32(p) | (uint64_t)le32(p + 4) << 32; }

xh_verify *xh_verify_open(const xh_imageset *set, const xh_imageset_file *file, const char **error)
{
  size_t size = 0;
  xh_verify *v = calloc(1, sizeof *v);
  if (!v) { *error = "out of memory"; return NULL; }
  pthread_mutex_init(&v->lock, NULL);
  if (!*file->hashes) FAIL("the image has no chunk hash list");
  if (!(v->list = listed(set, file->hashes, 64 << 20, &size, error))) goto bad;
  if (size < 32 || memcmp(v->list, "OIHL1\0\0\0", 8) || le32(v->list + 12) != 1) FAIL("the chunk hash list is not oneios-hash-list-1 with SHA-256");
  v->chunk = le32(v->list + 8);
  v->size = le64(v->list + 16);
  v->count = le64(v->list + 24);
  if (v->chunk < 4096 || v->chunk > (16u << 20) || (v->chunk & 511) || v->size != file->size ||
      v->count != (v->size + v->chunk - 1) / v->chunk || v->count > (size - 32) / 32 || size != 32 + v->count * 32) FAIL("the chunk hash list does not describe its image");
  memcpy(v->digest, xh_imageset_find(set, file->hashes)->sha256, 32);
  v->done = calloc(1, (size_t)(v->count + 7) / 8 + 1);
  v->buffer = malloc(v->chunk);
  if (!v->done || !v->buffer) FAIL("out of memory");
  return v;
bad:
  xh_verify_close(v);
  return NULL;
}

int xh_verify_range(xh_verify *v, int fd, uint64_t offset, uint64_t length)
{
  if (!length) return 0;
  if (offset >= v->size || length > v->size - offset) return -1;
  int rc = 0;
  pthread_mutex_lock(&v->lock);
  for (uint64_t c = offset / v->chunk; c <= (offset + length - 1) / v->chunk && !rc; c++) {
    if (v->done[c / 8] & (1u << (c % 8))) continue;
    uint64_t at = c * v->chunk, want = v->size - at < v->chunk ? v->size - at : v->chunk;
    uint8_t digest[32];
    xh_sha256 sha;
    for (uint64_t have = 0; have < want && !rc;) {
      ssize_t n = pread(fd, v->buffer + have, (size_t)(want - have), (off_t)(at + have));
      if (n < 0 && errno == EINTR) continue;
      if (n <= 0) rc = -1; else have += (uint64_t)n;
    }
    if (rc) break;
    xh_sha256_init(&sha);
    xh_sha256_update(&sha, v->buffer, (size_t)want);
    xh_sha256_final(&sha, digest);
    if (memcmp(digest, v->list + 32 + c * 32, 32)) rc = -1;
    else v->done[c / 8] |= (uint8_t)(1u << (c % 8));
  }
  pthread_mutex_unlock(&v->lock);
  return rc;
}

const uint8_t *xh_verify_digest(const xh_verify *v) { return v->digest; }

void xh_verify_close(xh_verify *v)
{
  if (!v) return;
  free(v->list); free(v->done); free(v->buffer);
  pthread_mutex_destroy(&v->lock);
  free(v);
}

// ---- trust caches -------------------------------------------------------------------------------
// One DER header at *p: tag and length; false when it does not fit before `end`.
static bool der(const uint8_t **p, const uint8_t *end, uint8_t tag, size_t *length)
{
  const uint8_t *q = *p;
  if (end - q < 2 || q[0] != tag) return false;
  size_t n = q[1];
  q += 2;
  if (n & 0x80) {
    unsigned bytes = n & 0x7f;
    if (bytes == 0 || bytes > 4 || (size_t)(end - q) < bytes) return false;
    for (n = 0; bytes; bytes--) n = n << 8 | *q++;
  }
  if ((size_t)(end - q) < n) return false;
  *p = q;
  *length = n;
  return true;
}

bool xh_im4p_payload(const uint8_t *data, size_t size, const char type[4], const uint8_t **payload, size_t *length)
{
  const uint8_t *p = data, *end = data + size;
  size_t n;
  if (!der(&p, end, 0x30, &n)) return false;
  end = p + n;
  if (!der(&p, end, 0x16, &n) || n != 4 || memcmp(p, "IM4P", 4)) return false;
  p += n;
  if (!der(&p, end, 0x16, &n) || n != 4 || memcmp(p, type, 4)) return false;
  p += n;
  if (!der(&p, end, 0x16, &n)) return false;          // the description
  p += n;
  if (!der(&p, end, 0x04, &n)) return false;
  *payload = p;
  *length = n;
  return true;
}

static int entry_order(const void *a, const void *b) { return memcmp(a, b, 20); }

// The owner module: version 2, a UUID derived from its content, the CDHashes of
// overlay/userland/owner-trust.json sorted, each as a SHA-256 hash (type 2) with no flags.
static uint8_t *owner_module(const xh_imageset *set, size_t *length, const char **error)
{
  size_t size = 0, count = 0, room = 0;
  uint8_t *module = NULL;
  *length = 0;
  if (!xh_imageset_find(set, "overlay/userland/owner-trust.json")) return NULL;         // a set without the userland overlay
  char *json = (char *)listed(set, "overlay/userland/owner-trust.json", 4 << 20, &size, error);
  if (!json) return NULL;
  for (const char *e = js_first(js_get(json, "cdhashes")); e; e = js_next(e)) {
    char text[48];
    if (count == room) {
      room = room ? room * 2 : 64;
      uint8_t *grown = realloc(module, 24 + room * 24);
      if (!grown) FAIL("out of memory");
      module = grown;
    }
    uint8_t *entry = module + 24 + count * 24;
    memset(entry, 0, 24);
    if (!js_str(e, text, sizeof text) || !hex_bytes(text, entry, 20)) FAIL("owner-trust.json: a CDHash is not 40 hex digits");
    entry[20] = 2;
    count++;
  }
  free(json);
  json = NULL;
  if (!count) { free(module); return NULL; }
  qsort(module + 24, count, 24, entry_order);
  size_t kept = 1;
  for (size_t i = 1; i < count; i++)
    if (memcmp(module + 24 + (kept - 1) * 24, module + 24 + i * 24, 20)) memmove(module + 24 + kept++ * 24, module + 24 + i * 24, 24);
  uint8_t digest[32];
  xh_sha256 sha;
  xh_sha256_init(&sha);
  xh_sha256_update(&sha, module + 24, kept * 24);
  xh_sha256_final(&sha, digest);
  module[0] = 2; module[1] = module[2] = module[3] = 0;
  memcpy(module + 4, digest, 16);
  module[20] = (uint8_t)kept; module[21] = (uint8_t)(kept >> 8); module[22] = (uint8_t)(kept >> 16); module[23] = (uint8_t)(kept >> 24);
  *length = 24 + kept * 24;
  return module;
bad:
  free(json);
  free(module);
  return NULL;
}

xl_blob xh_imageset_trustcaches(const xh_imageset *set, const char **error)
{
  static const char *const names[] = { "trust/static.trustcache", "trust/system.trustcache", "trust/app.trustcache" };
  xl_blob blob = { 0 };
  uint8_t *file[3] = { 0 }, *owner = NULL, *out = NULL;
  const uint8_t *module[4];
  size_t length[4], count = 0, total, owner_length = 0;
  *error = NULL;
  for (unsigned i = 0; i < 3; i++) {
    size_t size = 0;
    if (!xh_imageset_find(set, names[i])) { if (i == 0) FAIL("the image set has no trust/static.trustcache"); continue; }
    if (!(file[i] = listed(set, names[i], 16 << 20, &size, error))) goto bad;
    if (!xh_im4p_payload(file[i], size, "trst", &module[count], &length[count]) &&
        !xh_im4p_payload(file[i], size, "ltrs", &module[count], &length[count]) &&
        !xh_im4p_payload(file[i], size, "trcs", &module[count], &length[count]) &&
        !xh_im4p_payload(file[i], size, "trca", &module[count], &length[count])) FAIL("a trust cache of the image set is not an IM4P trust cache");
    if (length[count] < 24 || le32(module[count]) != 2 || length[count] != 24 + (uint64_t)le32(module[count] + 20) * 24)
      FAIL("a trust cache's payload is not a version 2 module (compressed or another version)");
    count++;
  }
  owner = owner_module(set, &owner_length, error);
  if (!owner && *error) goto bad;
  if (owner) { module[count] = owner; length[count++] = owner_length; }
  total = 4 + count * 4;
  for (size_t i = 0; i < count; i++) total += length[i];
  if (!(out = malloc(total))) FAIL("out of memory");
  size_t at = 4 + count * 4;
  out[0] = (uint8_t)count; out[1] = out[2] = out[3] = 0;
  for (size_t i = 0; i < count; i++) {
    uint8_t *slot = out + 4 + i * 4;
    slot[0] = (uint8_t)at; slot[1] = (uint8_t)(at >> 8); slot[2] = (uint8_t)(at >> 16); slot[3] = (uint8_t)(at >> 24);
    memcpy(out + at, module[i], length[i]);
    at += length[i];
  }
  if (!xl_trustcaches_valid(out, total)) FAIL("the trust caches do not make a sidecar the loader takes (entries unsorted?)");
  blob.data = out;
  blob.size = total;
  out = NULL;
bad:
  for (unsigned i = 0; i < 3; i++) free(file[i]);
  free(owner);
  free(out);
  return blob;
}

// ---- the seed helper ----------------------------------------------------------------------------
int xh_imageset_helper_disk(const xh_imageset *set, char *path, size_t path_size, uint64_t *bytes, const char **error)
{
  static const char name[] = "overlay/userland/bin/oneios-seed";
  size_t size = 0;
  if (!xh_imageset_find(set, name)) return 1;
  uint8_t *data = listed(set, name, 64 << 20, &size, error);
  if (!data) return -1;
  size_t padded = (size + 511) & ~(size_t)511;
  int fd = (int)syscall(SYS_memfd_create, "oneios-seed-helper", 0u);
  bool ok = size && fd >= 0 && ftruncate(fd, (off_t)padded) == 0 && pwrite(fd, data, size, 0) == (ssize_t)size &&
            snprintf(path, path_size, "/proc/self/fd/%d", fd) < (int)path_size;
  free(data);
  if (!ok) { if (fd >= 0) close(fd); *error = "the seed helper cannot be made a disk"; return -1; }
  *bytes = size;
  return 0;                     // the descriptor stays open: it is the disk
}

// ---- the device tree ----------------------------------------------------------------------------
void xh_imageset_describe(xl_adt_node *node, const xh_disk_identity *identity)
{
  if (!node || !identity->role) return;
  xl_adt_set_str(node, "oneios,role", identity->role);
  if (identity->verified) xl_adt_set(node, "oneios,verified", identity->verified, 32);
  if (identity->volume_uuid && *identity->volume_uuid) xl_adt_set_str(node, "oneios,volume-uuid", identity->volume_uuid);
  if (identity->volume_name && *identity->volume_name) xl_adt_set_str(node, "oneios,volume-name", identity->volume_name);
  if (identity->sha256_hex && *identity->sha256_hex) xl_adt_set_str(node, "oneios,sha256", identity->sha256_hex);
  if (identity->bytes) xl_adt_set_u64(node, "oneios,bytes", identity->bytes);
}
