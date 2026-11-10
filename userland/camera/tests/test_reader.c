// SPDX-License-Identifier: MIT
// Host test of the plug-in's reader against stand-ins for /dev/oneios-camera: a file that holds one
// frame as the node gives it (the header the kernel's own code makes, then the picture), and files
// that hold what must be refused.
#include "oneios_camera_reader.h"
#include "oneios_camera.h"             // the kernel's pure parts: the same header a real read gets
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int failures, checks;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

static void put(const char *path, const void *a, size_t na, const void *b, size_t nb)
{
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
  if (fd < 0 || write(fd, a, na) != (ssize_t)na || (nb && write(fd, b, nb) != (ssize_t)nb)) abort();
  close(fd);
}

int main(void)
{
  enum { W = 64, H = 48, STRIDE = 80 };                       // a host picture with row padding
  char dir[] = "/tmp/oneios-reader-XXXXXX", path[64];
  static uint8_t picture[STRIDE * H * 3 / 2], y[H][72], cbcr[H / 2][96];
  if (!mkdtemp(dir)) return 2;
  snprintf(path, sizeof path, "%s/node", dir);
  for (unsigned i = 0; i < sizeof picture; i++) picture[i] = (uint8_t)(i % STRIDE < W ? i * 31 + i / STRIDE : 0xee);
  struct oc_frame wire = { .magic = OC_FRAME_MAGIC, .sequence = 41, .timestamp_ns = 5000000000ull, .width = W, .height = H, .stride = STRIDE,
                           .format = OC_FMT_NV12, .bytes = sizeof picture, .role = OC_ROLE_ULTRAWIDE, .orientation = 90, .fps = 30 };
  struct oneios_camera_frame made, h;
  oca_header(&made, &wire, OC_FACING_BACK, 777);
  put(path, &made, sizeof made, picture, sizeof picture);

  oneios_camera_reader r;
  const uint8_t *got = NULL;
  CHECK(oneios_camera_reader_open(&r, "/nonexistent/oneios-camera") == -ENOENT);
  CHECK(oneios_camera_reader_open(&r, path) == 0);
  CHECK(oneios_camera_reader_next(&r, &h, &got) == 0 && h.width == W && h.height == H && h.stride == STRIDE && h.sequence == 41 &&
        h.sensor == ONEIOS_CAMERA_ULTRAWIDE && h.orientation == 90 && h.facing == 1 && h.time == 777 && !memcmp(got, picture, sizeof picture));
  CHECK(r.frames == 1 && r.missed == 0);
  // Into planes with other strides: the picture's bytes, none of its padding, nothing outside the rows.
  memset(y, 0x11, sizeof y); memset(cbcr, 0x22, sizeof cbcr);
  CHECK(oneios_camera_copy_planes(&h, got, &y[0][0], 72, &cbcr[0][0], 96, W, H) == 0);
  int ok = 1;
  for (int row = 0; row < H; row++) if (memcmp(y[row], picture + row * STRIDE, W) || y[row][W] != 0x11) ok = 0;
  for (int row = 0; row < H / 2; row++) if (memcmp(cbcr[row], picture + (H + row) * STRIDE, W) || cbcr[row][W] != 0x22) ok = 0;
  CHECK(ok);
  CHECK(oneios_camera_copy_planes(&h, got, &y[0][0], 72, &cbcr[0][0], 96, W + 2, H) == -EINVAL);
  CHECK(oneios_camera_copy_planes(&h, got, &y[0][0], W - 1, &cbcr[0][0], 96, W, H) == -EINVAL);
  CHECK(oneios_camera_reader_next(&r, &h, &got) == -EAGAIN);  // the file's end stands for "no frame in time"
  oneios_camera_reader_close(&r);

  // The commands reach the node as the ABI's 24 bytes.
  put(path, "", 0, NULL, 0);
  CHECK(oneios_camera_reader_open(&r, path) == 0 && oneios_camera_reader_select(&r, ONEIOS_CAMERA_FRONT, 1280, 720, 0) == 0);
  oneios_camera_reader_close(&r);
  struct oneios_camera_command sent[2];
  int fd = open(path, O_RDONLY);
  CHECK(read(fd, sent, sizeof sent) == (ssize_t)sizeof sent);
  close(fd);
  CHECK(sent[0].command == ONEIOS_CAMERA_SENSOR && sent[0].a == ONEIOS_CAMERA_FRONT && sent[1].command == ONEIOS_CAMERA_FORMAT && sent[1].a == 1280 && sent[1].b == 720);
  // And the kernel's command code takes them.
  struct oca_state state;
  oca_defaults(&state);
  CHECK(oca_command(&state, &sent[0]) == 0 && oca_command(&state, &sent[1]) == 0 && state.sensor == ONEIOS_CAMERA_FRONT && state.fps == 30);

  // What is not a frame is refused: a short read, another magic, a raw-sensor frame, sizes that do not fit.
  struct oneios_camera_frame bad = made;
  CHECK(oneios_camera_header_check(&made, sizeof made + sizeof picture) == 0);
  CHECK(oneios_camera_header_check(&made, sizeof made + sizeof picture - (STRIDE - W)) == 0);    // without the last row's padding
  CHECK(oneios_camera_header_check(&made, sizeof made + sizeof picture - (STRIDE - W) - 1) == -EPROTO);
  CHECK(oneios_camera_header_check(&made, 100) == -EPROTO);
  bad.magic[0] = 'X'; CHECK(oneios_camera_header_check(&bad, sizeof made + sizeof picture) == -EPROTO);
  bad = made; bad.format = ONEIOS_CAMERA_RAW10; CHECK(oneios_camera_header_check(&bad, sizeof made + sizeof picture) == -EPROTO);
  bad = made; bad.width = 4000; bad.stride = 4000; CHECK(oneios_camera_header_check(&bad, sizeof made + sizeof picture) == -EPROTO);
  bad = made; bad.height = 0xfffffffe; CHECK(oneios_camera_header_check(&bad, sizeof made + sizeof picture) == -EPROTO);
  bad = made; bad.stride = W - 2; CHECK(oneios_camera_header_check(&bad, sizeof made + sizeof picture) == -EPROTO);
  bad = made; bad.header_bytes = 64; CHECK(oneios_camera_header_check(&bad, sizeof made + sizeof picture) == -EPROTO);
  put(path, &bad, sizeof bad, picture, 100);
  CHECK(oneios_camera_reader_open(&r, path) == 0 && oneios_camera_reader_next(&r, &h, &got) == -EPROTO);
  oneios_camera_reader_close(&r);
  // A gap in the sequence is counted.
  CHECK(oneios_camera_reader_open(&r, path) == 0);
  r.frames = 1; r.last_sequence = 30;
  put(path, &made, sizeof made, picture, sizeof picture);
  CHECK(oneios_camera_reader_next(&r, &h, &got) == 0 && r.missed == 10);
  oneios_camera_reader_close(&r);

  unlink(path);
  rmdir(dir);
  printf("camera reader: %d checks, %d failed\n", checks, failures);
  return failures ? 1 : 0;
}
