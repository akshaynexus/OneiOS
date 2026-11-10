// SPDX-License-Identifier: MIT
// Disks backed by the image set's files. The role of UniXNU's RamBlock and VirtioMmioBlock back ends
// (https://github.com/UniXNU/RamBlock 60a54ce, https://github.com/UniXNU/VirtioMmioBlock 4e1013e):
// there the guest keeps a seed in RAM and changed pages in a store window; here Linux's page cache
// and a sparse file do both, so the store is simply a raw image the size of the Data disk.
//   read-only: os.img, systemos.img, app.img (raw APFS containers), opened O_RDONLY.
//   writable:  <data>/data.img, created sparse on the first start. A seed that is itself a disk image
//              (not a tar) is copied in, holes kept; a tar seed is the userland's to unpack.
#define _GNU_SOURCE
#include "xnuhost.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

struct xh_blk { int fd; uint64_t bytes; bool writable; };

#define DATA_DEFAULT_BYTES (32ull << 30)      // sparse: costs what the guest writes

static bool is_tar(int fd)
{
  char magic[6] = { 0 };
  return pread(fd, magic, 5, 257) == 5 && !memcmp(magic, "ustar", 5);
}

// Copies a seed image into a new store, skipping blocks of zeros so the store stays sparse.
static int copy_seed(int from, int to, uint64_t bytes)
{
  enum { CHUNK = 1 << 20 };
  static const char zeros[4096];
  char *buffer = malloc(CHUNK);
  if (!buffer) return -1;
  int rc = 0;
  for (uint64_t at = 0; at < bytes && !rc;) {
    ssize_t n = pread(from, buffer, CHUNK, (off_t)at);
    if (n <= 0) { rc = -1; break; }
    for (ssize_t b = 0; b < n; b += 4096) {
      size_t len = (size_t)(n - b < 4096 ? n - b : 4096);
      if (!memcmp(buffer + b, zeros, len)) continue;
      if (pwrite(to, buffer + b, len, (off_t)(at + (uint64_t)b)) != (ssize_t)len) { rc = -1; break; }
    }
    at += (uint64_t)n;
  }
  free(buffer);
  return rc;
}

xh_blk *xh_blk_open(const xh_disk_config *config, const char **error)
{
  struct stat st;
  xh_blk *b = calloc(1, sizeof *b);
  if (!b) { *error = "out of memory"; return NULL; }
  b->fd = -1;
  b->writable = config->writable;
  if (!config->writable) {
    b->fd = open(config->path, O_RDONLY | O_CLOEXEC);
    if (b->fd < 0 || fstat(b->fd, &st)) { *error = "cannot open the image"; goto bad; }
  } else {
    b->fd = open(config->path, O_RDWR | O_CLOEXEC);
    if (b->fd < 0 && errno == ENOENT) {
      // First start: a new store, written under a temporary name so a crash leaves no half store.
      char temporary[600];
      snprintf(temporary, sizeof temporary, "%s.new", config->path);
      int fd = open(temporary, O_RDWR | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
      if (fd < 0) { *error = "cannot create the Data store"; goto bad; }
      uint64_t bytes = DATA_DEFAULT_BYTES;
      int seed = *config->seed ? open(config->seed, O_RDONLY | O_CLOEXEC) : -1;
      bool image = seed >= 0 && !is_tar(seed) && fstat(seed, &st) == 0 && st.st_size >= 4096;
      if (image && (uint64_t)st.st_size > bytes) bytes = (uint64_t)st.st_size;
      int rc = ftruncate(fd, (off_t)bytes);
      if (!rc && image) rc = copy_seed(seed, fd, (uint64_t)st.st_size);
      if (!rc) rc = fsync(fd);
      if (seed >= 0) close(seed);
      if (rc || rename(temporary, config->path)) { close(fd); unlink(temporary); *error = "cannot fill the new Data store"; goto bad; }
      b->fd = fd;
    }
    if (b->fd < 0 || fstat(b->fd, &st)) { *error = "cannot open the Data store"; goto bad; }
  }
  b->bytes = (uint64_t)st.st_size & ~511ull;
  if (b->bytes == 0) { *error = "the image is empty"; goto bad; }
  return b;
bad:
  if (b->fd >= 0) close(b->fd);
  free(b);
  return NULL;
}

uint64_t xh_blk_bytes(const xh_blk *b) { return b->bytes; }
bool xh_blk_writable(const xh_blk *b) { return b->writable; }

int xh_blk_read(xh_blk *b, uint64_t offset, void *buffer, uint64_t length)
{
  if (offset > b->bytes || length > b->bytes - offset) return -1;
  for (uint64_t done = 0; done < length;) {
    ssize_t n = pread(b->fd, (char *)buffer + done, length - done, (off_t)(offset + done));
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) return -1;
    done += (uint64_t)n;
  }
  return 0;
}

int xh_blk_write(xh_blk *b, uint64_t offset, const void *buffer, uint64_t length)
{
  if (!b->writable || offset > b->bytes || length > b->bytes - offset) return -1;
  for (uint64_t done = 0; done < length;) {
    ssize_t n = pwrite(b->fd, (const char *)buffer + done, length - done, (off_t)(offset + done));
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) return -1;
    done += (uint64_t)n;
  }
  return 0;
}

int xh_blk_flush(xh_blk *b) { return b->writable ? fdatasync(b->fd) : 0; }

void xh_blk_close(xh_blk *b)
{
  if (!b) return;
  if (b->writable) fsync(b->fd);
  close(b->fd);
  free(b);
}
