// SPDX-License-Identifier: MIT
// oneios-seed: unpacks the image set's Data seed (data.seed, a pax tar; docs/image-format.md section 5) into
// a new, empty Data volume on the guest's first start.
//
// Nothing on Linux writes iOS 27's APFS, so the image build ships the Data volume's content as a tar and the
// guest makes the volume itself: Apple's newfs_apfs (from the OS volume) formats the store, the kernel mounts
// it, and this program fills it, as root, with the seed's numeric owners, modes, links and extended attributes.
//
//   oneios-seed [--hash SHA256] ARCHIVE TARGET
//
// ARCHIVE is the seed as a file or as the raw block device the host presents it on. TARGET is the mounted new
// volume. With --hash, TARGET/.oneios-seed records the seed: a volume that has the same hash is left alone
// (exit 0), one with another hash is not touched (exit 3: the caller starts the store over, as UniXNU's Data
// store does). Exit 1 on a malformed archive or a failed write; the marker is written last, so a volume that
// was only half filled is filled again.
//
// Reads what image-build writes and nothing more: ustar headers, pax extended headers (path, linkpath, size,
// uid, gid, LIBARCHIVE.xattr.<name> in base64), GNU long names; folders, regular files and symbolic links.
// Anything else (devices, hard links, a path with ".." or through a symbolic link) stops it.
//
// Builds for the guest with an iOS SDK and for the host with cc (tests/test_helpers.py).
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/xattr.h>
#include <unistd.h>

#define BLOCK 512
#define PATH_LIMIT 4096
#define MAX_XATTRS 32

struct xattr { char *name; unsigned char *value; size_t size; };
static struct xattr xattrs[MAX_XATTRS];
static int xattr_count;
static char pax_path[PATH_LIMIT], pax_link[PATH_LIMIT], gnu_path[PATH_LIMIT], gnu_link[PATH_LIMIT];
static long long pax_size = -1, pax_uid = -1, pax_gid = -1;
static int archive = -1;
static unsigned long long files, dirs, links, bytes, xattr_set, xattr_failed, chown_skipped;

static void die(const char *what, const char *path) {
  fprintf(stderr, "oneios-seed: %s%s%s%s%s\n", what, path ? ": " : "", path ? path : "", errno ? ": " : "", errno ? strerror(errno) : "");
  exit(1);
}

static void bad(const char *what, const char *path) { errno = 0; die(what, path); }

// Reads exactly n bytes; 0 at a clean end of the archive before the first byte.
static int read_all(void *buffer, size_t n) {
  size_t have = 0;
  while (have < n) {
    ssize_t got = read(archive, (char *)buffer + have, n - have);
    if (got < 0) { if (errno == EINTR) continue; die("read", "archive"); }
    if (!got) { if (!have) return 0; bad("the archive ends inside an entry", NULL); }
    have += (size_t)got;
  }
  return 1;
}

static long long number(const unsigned char *field, size_t size) {
  long long value = 0;
  if (field[0] & 0x80) {  // base-256, GNU and pax writers for values that do not fit octal
    for (size_t i = 1; i < size; i++) value = (value << 8) | field[i];
    return value;
  }
  for (size_t i = 0; i < size && field[i]; i++) {
    if (field[i] == ' ') continue;
    if (field[i] < '0' || field[i] > '7') bad("a header number is not octal", NULL);
    value = value * 8 + (field[i] - '0');
  }
  return value;
}

static void clear_entry_state(void) {
  for (int i = 0; i < xattr_count; i++) { free(xattrs[i].name); free(xattrs[i].value); }
  xattr_count = 0;
  pax_path[0] = pax_link[0] = gnu_path[0] = gnu_link[0] = 0;
  pax_size = pax_uid = pax_gid = -1;
}

static int base64_value(int c) {
  if (c >= 'A' && c <= 'Z') return c - 'A';
  if (c >= 'a' && c <= 'z') return c - 'a' + 26;
  if (c >= '0' && c <= '9') return c - '0' + 52;
  if (c == '+') return 62;
  if (c == '/') return 63;
  return -1;
}

static unsigned char *base64(const char *text, size_t length, size_t *size) {
  unsigned char *out = malloc(length / 4 * 3 + 3);
  if (!out) die("malloc", NULL);
  size_t n = 0;
  unsigned accumulator = 0;
  int bits = 0;
  for (size_t i = 0; i < length; i++) {
    if (text[i] == '=') break;
    int v = base64_value((unsigned char)text[i]);
    if (v < 0) bad("an extended attribute is not base64", NULL);
    accumulator = (accumulator << 6) | (unsigned)v;
    bits += 6;
    if (bits >= 8) { bits -= 8; out[n++] = (unsigned char)(accumulator >> bits); }
  }
  *size = n;
  return out;
}

// The records of a pax extended header: "LENGTH key=value\n", LENGTH counting the whole record.
static void pax_records(char *data, size_t size) {
  size_t at = 0;
  while (at < size) {
    char *end;
    unsigned long length = strtoul(data + at, &end, 10);
    if (*end != ' ' || length < 5 || at + length > size || data[at + length - 1] != '\n') bad("a pax record is malformed", NULL);
    char *key = end + 1, *eq = memchr(key, '=', (size_t)(data + at + length - key));
    if (!eq) bad("a pax record has no value", NULL);
    *eq = 0;
    char *value = eq + 1;
    size_t value_size = (size_t)(data + at + length - 1 - value);
    data[at + length - 1] = 0;
    if (!strcmp(key, "path") || !strcmp(key, "linkpath")) {
      if (value_size >= PATH_LIMIT) bad("a path is too long", NULL);
      memcpy(!strcmp(key, "path") ? pax_path : pax_link, value, value_size + 1);
    } else if (!strcmp(key, "size")) pax_size = atoll(value);
    else if (!strcmp(key, "uid")) pax_uid = atoll(value);
    else if (!strcmp(key, "gid")) pax_gid = atoll(value);
    else if (!strncmp(key, "LIBARCHIVE.xattr.", 17)) {
      if (xattr_count == MAX_XATTRS) bad("too many extended attributes on one entry", NULL);
      xattrs[xattr_count].name = strdup(key + 17);
      xattrs[xattr_count].value = base64(value, value_size, &xattrs[xattr_count].size);
      xattr_count++;
    }  // mtime, atime and the rest: not carried
    at += length;
  }
}

static char *read_body(long long size) {
  if (size < 0 || size > 16 * 1024 * 1024) bad("an extended header is too large", NULL);
  size_t padded = ((size_t)size + BLOCK - 1) / BLOCK * BLOCK;
  char *body = malloc(padded + 1);
  if (!body) die("malloc", NULL);
  if (padded && !read_all(body, padded)) bad("the archive ends inside an entry", NULL);
  body[size] = 0;
  return body;
}

// A path of the seed: relative, no "..", and none of its parent folders is a symbolic link in the target.
static void check_path(const char *path) {
  if (!path[0] || path[0] == '/') bad("an entry's path is empty or absolute", path);
  char prefix[PATH_LIMIT];
  size_t n = strlen(path);
  if (n >= sizeof prefix) bad("a path is too long", path);
  for (size_t i = 0; i <= n; i++) {
    if (path[i] != '/' && path[i]) continue;
    size_t start = i;
    while (start && path[start - 1] != '/') start--;
    if (i - start == 2 && path[start] == '.' && path[start + 1] == '.') bad("an entry's path has ..", path);
    if (!path[i]) break;
    memcpy(prefix, path, i);
    prefix[i] = 0;
    struct stat st;
    if (!lstat(prefix, &st) && S_ISLNK(st.st_mode)) bad("an entry's path goes through a symbolic link", path);
  }
}

static void own(const char *path, long long uid, long long gid) {
  if (lchown(path, (uid_t)uid, (gid_t)gid)) {
    // Not root (the host tests): owners cannot be set; the guest always runs this as root.
    if (errno == EPERM && geteuid()) { chown_skipped++; return; }
    die("chown", path);
  }
}

static void set_xattrs(const char *path) {
  for (int i = 0; i < xattr_count; i++) {
#ifdef __APPLE__
    int r = setxattr(path, xattrs[i].name, xattrs[i].value, xattrs[i].size, 0, XATTR_NOFOLLOW);
#else
    int r = lsetxattr(path, xattrs[i].name, xattrs[i].value, xattrs[i].size, 0);
#endif
    if (r) { xattr_failed++; fprintf(stderr, "oneios-seed: xattr %s on %s: %s\n", xattrs[i].name, path, strerror(errno)); }
    else xattr_set++;
  }
}

static void extract(const char *path, int type, mode_t mode, long long uid, long long gid, long long size, const char *target) {
  char clean[PATH_LIMIT];
  while (path[0] == '.' && path[1] == '/') path += 2;
  size_t n = strlen(path);
  if (n >= sizeof clean) bad("a path is too long", path);
  memcpy(clean, path, n + 1);
  while (n > 1 && clean[n - 1] == '/') clean[--n] = 0;
  if (!strcmp(clean, ".")) {  // the volume's own top: only its owner and mode
    if (type != '5') bad("the top entry is not a folder", NULL);
    own(".", uid, gid);
    if (chmod(".", mode)) die("chmod", ".");
    return;
  }
  check_path(clean);
  if (type == '5') {
    if (size) bad("a folder with content", clean);
    if (mkdir(clean, 0700) && errno != EEXIST) die("mkdir", clean);
    struct stat st;
    if (lstat(clean, &st) || !S_ISDIR(st.st_mode)) bad("something that is not a folder is in the way", clean);
    own(clean, uid, gid);
    if (chmod(clean, mode)) die("chmod", clean);
    set_xattrs(clean);
    dirs++;
  } else if (type == '2') {
    if (size) bad("a symbolic link with content", clean);
    if (!target[0]) bad("a symbolic link without a target", clean);
    if (unlink(clean) && errno != ENOENT) die("unlink", clean);
    if (symlink(target, clean)) die("symlink", clean);
    own(clean, uid, gid);
    set_xattrs(clean);
    links++;
  } else {
    int fd = open(clean, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, 0600);
    if (fd < 0) die("open", clean);
    static char buffer[1 << 16];
    long long left = size;
    while (left > 0) {
      size_t want = left > (long long)sizeof buffer ? sizeof buffer : (size_t)left;
      size_t padded = (want + BLOCK - 1) / BLOCK * BLOCK;  // only the last chunk is not a multiple of a block
      if (!read_all(buffer, padded)) bad("the archive ends inside an entry", clean);
      for (size_t done = 0; done < want;) {
        ssize_t wrote = write(fd, buffer + done, want - done);
        if (wrote < 0) { if (errno == EINTR) continue; die("write", clean); }
        done += (size_t)wrote;
      }
      left -= (long long)want;
    }
    // Owner before mode: a change of owner clears the set-id bits.
    if (fchown(fd, (uid_t)uid, (gid_t)gid)) {
      if (errno == EPERM && geteuid()) chown_skipped++;
      else die("chown", clean);
    }
    if (fchmod(fd, mode)) die("chmod", clean);
    if (close(fd)) die("close", clean);
    set_xattrs(clean);
    files++;
    bytes += (unsigned long long)size;
  }
}

static int marker(const char *hash) {
  char have[160] = "";
  FILE *f = fopen(".oneios-seed", "r");
  if (!f) return 0;
  if (!fgets(have, sizeof have, f)) have[0] = 0;
  fclose(f);
  have[strcspn(have, "\n")] = 0;
  return !strcmp(have, hash) ? 1 : 2;
}

int main(int argc, char **argv) {
  const char *hash = NULL;
  int at = 1;
  if (argc >= 3 && !strcmp(argv[1], "--hash")) { hash = argv[2]; at = 3; }
  if (argc - at != 2) { fprintf(stderr, "usage: oneios-seed [--hash SHA256] ARCHIVE TARGET\n"); return 2; }
  if (hash) {
    size_t n = strlen(hash);
    if (n != 64 || strspn(hash, "0123456789abcdef") != 64) { fprintf(stderr, "oneios-seed: --hash wants 64 lower-case hex digits\n"); return 2; }
  }
  archive = open(argv[at], O_RDONLY);
  if (archive < 0) die("open", argv[at]);
  if (chdir(argv[at + 1])) die("chdir", argv[at + 1]);
  umask(0);
  if (hash) {
    int state = marker(hash);
    if (state == 1) { printf("oneios-seed: the volume already holds this seed\n"); return 0; }
    if (state == 2) { fprintf(stderr, "oneios-seed: the volume holds another seed; not touched\n"); return 3; }
  }
  unsigned char header[BLOCK];
  int zero_blocks = 0, entries = 0;
  while (read_all(header, BLOCK)) {
    int empty = 1;
    for (int i = 0; i < BLOCK; i++) if (header[i]) { empty = 0; break; }
    if (empty) { if (++zero_blocks == 2) break; continue; }
    zero_blocks = 0;
    unsigned sum = 0;
    for (int i = 0; i < BLOCK; i++) sum += (i >= 148 && i < 156) ? ' ' : header[i];
    if (sum != (unsigned)number(header + 148, 8)) bad("a header's checksum is wrong: not a tar, or damaged", NULL);
    long long size = number(header + 124, 12);
    int type = header[156] ? header[156] : '0';
    if (type == 'x') { char *body = read_body(size); pax_records(body, (size_t)size); free(body); continue; }
    if (type == 'g') { free(read_body(size)); continue; }  // global header: nothing of it applies to an entry
    if (type == 'L' || type == 'K') {
      char *body = read_body(size);
      if ((size_t)size >= PATH_LIMIT) bad("a path is too long", NULL);
      snprintf(type == 'L' ? gnu_path : gnu_link, PATH_LIMIT, "%s", body);
      free(body);
      continue;
    }
    char name[PATH_LIMIT], target[PATH_LIMIT];
    if (pax_path[0]) snprintf(name, sizeof name, "%s", pax_path);
    else if (gnu_path[0]) snprintf(name, sizeof name, "%s", gnu_path);
    else if (header[345] && !memcmp(header + 257, "ustar", 5)) snprintf(name, sizeof name, "%.155s/%.100s", header + 345, header);
    else snprintf(name, sizeof name, "%.100s", header);
    if (pax_link[0]) snprintf(target, sizeof target, "%s", pax_link);
    else if (gnu_link[0]) snprintf(target, sizeof target, "%s", gnu_link);
    else snprintf(target, sizeof target, "%.100s", header + 157);
    if (pax_size >= 0) size = pax_size;
    long long uid = pax_uid >= 0 ? pax_uid : number(header + 108, 8), gid = pax_gid >= 0 ? pax_gid : number(header + 116, 8);
    mode_t mode = (mode_t)(number(header + 100, 8) & 07777);
    if (type != '0' && type != '5' && type != '2' && type != '7') bad("an entry that is not a file, folder or symbolic link", name);
    extract(name, type == '7' ? '0' : type, mode, uid, gid, size, target);
    clear_entry_state();
    entries++;
  }
  if (!entries) bad("the archive has no entries", NULL);
  if (hash) {
    FILE *f = fopen(".oneios-seed", "w");
    if (!f || fprintf(f, "%s\n", hash) < 0 || fflush(f) || fsync(fileno(f)) || fclose(f)) die("write", ".oneios-seed");
  }
  sync();
  printf("oneios-seed: %llu files (%llu bytes), %llu folders, %llu links, %llu extended attributes", files, bytes, dirs, links, xattr_set);
  if (xattr_failed) printf(", %llu extended attributes NOT set", xattr_failed);
  if (chown_skipped) printf(", %llu owners not set (not root)", chown_skipped);
  printf("\n");
  return 0;
}
