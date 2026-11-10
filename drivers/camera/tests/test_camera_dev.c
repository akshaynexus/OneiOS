// SPDX-License-Identifier: MIT
// Host test of the camera node's pure parts (../oneios_camera.h): the commands a reader writes, the
// request that goes to the host, which frames from the host are handed on, the header a reader gets.
#include "oneios_camera.h"
#include <stddef.h>
#include <stdio.h>
#include <string.h>

static int failures, checks;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

int main(void)
{
  struct oca_state s;
  oca_defaults(&s);
  CHECK(s.sensor == ONEIOS_CAMERA_WIDE && s.width == 1280 && s.height == 720 && s.fps == 30 && !s.started && !s.restart);
  // The roles of the wire are the sensors of the ABI.
  CHECK((int)ONEIOS_CAMERA_WIDE == (int)OC_ROLE_WIDE && (int)ONEIOS_CAMERA_FRONT == (int)OC_ROLE_FRONT && (int)ONEIOS_CAMERA_ULTRAWIDE == (int)OC_ROLE_ULTRAWIDE &&
        (int)ONEIOS_CAMERA_TELE3 == (int)OC_ROLE_TELE3 && (int)ONEIOS_CAMERA_TELE5 == (int)OC_ROLE_TELE5);

  // Before the start a selection asks for no restart; after it, only a change does.
  struct oneios_camera_command c = { .command = ONEIOS_CAMERA_SENSOR, .a = ONEIOS_CAMERA_FRONT };
  CHECK(oca_command(&s, &c) == 0 && s.sensor == ONEIOS_CAMERA_FRONT && !s.restart);
  s.started = true;
  CHECK(oca_command(&s, &c) == 0 && !s.restart);
  c.a = ONEIOS_CAMERA_TELE5;
  CHECK(oca_command(&s, &c) == 0 && s.sensor == ONEIOS_CAMERA_TELE5 && s.restart);
  c.a = 0;
  CHECK(oca_command(&s, &c) == 19 && s.sensor == ONEIOS_CAMERA_TELE5);
  c.a = 6;
  CHECK(oca_command(&s, &c) == 19);
  s.restart = false;
  c = (struct oneios_camera_command){ .command = ONEIOS_CAMERA_FORMAT, .a = 1920, .b = 1080, .c = 60 };
  CHECK(oca_command(&s, &c) == 0 && s.width == 1920 && s.height == 1080 && s.fps == 60 && s.restart);
  s.restart = false;
  CHECK(oca_command(&s, &c) == 0 && !s.restart);
  c.c = 0;                                                   // 0 = 30 frames a second
  CHECK(oca_command(&s, &c) == 0 && s.fps == 30 && s.restart);
  static const uint32_t bad[][3] = { { 1922, 1080, 30 }, { 1920, 1082, 30 }, { 0, 0, 30 }, { 641, 480, 30 }, { 640, 481, 30 }, { 640, 480, 241 }, { 14, 480, 30 } };
  for (unsigned i = 0; i < sizeof bad / sizeof bad[0]; i++) {
    c = (struct oneios_camera_command){ .command = ONEIOS_CAMERA_FORMAT, .a = bad[i][0], .b = bad[i][1], .c = bad[i][2] };
    CHECK(oca_command(&s, &c) == 22 && s.width == 1920 && s.height == 1080);
  }
  // What a raw-sensor reader sends is taken and does nothing.
  for (uint32_t ignored = ONEIOS_CAMERA_EXPOSURE; ignored <= ONEIOS_CAMERA_MODE; ignored++) {
    if (ignored == ONEIOS_CAMERA_SENSOR) continue;
    struct oca_state before = s;
    c = (struct oneios_camera_command){ .command = ignored, .a = 1000, .b = 512 };
    CHECK(oca_command(&s, &c) == 0 && !memcmp(&before, &s, sizeof s));
  }
  c = (struct oneios_camera_command){ .command = 77 };
  CHECK(oca_command(&s, &c) == 22);

  struct oc_request r;
  oca_request(&s, &r);
  CHECK(r.code == OC_CMD_START && r.role == OC_ROLE_TELE5 && r.width == 1920 && r.height == 1080 && r.fps == 30);

  // Frames from the host: only one whose sizes agree with each other and with what was written.
  struct oc_frame f = { .magic = OC_FRAME_MAGIC, .sequence = 7, .timestamp_ns = 0x1122334455667788ull, .width = 1280, .height = 720, .stride = 1280,
                        .format = OC_FMT_NV12, .bytes = 1280 * 720 * 3 / 2, .role = OC_ROLE_FRONT, .orientation = 270, .fps = 30 };
  CHECK(oca_frame_ok(&f, OC_FRAME_DATA + f.bytes));
  CHECK(!oca_frame_ok(&f, OC_FRAME_DATA + f.bytes - 1) && !oca_frame_ok(&f, 10));
  struct oc_frame g = f;
  g.magic = 1; CHECK(!oca_frame_ok(&g, OC_BUFFER_BYTES));
  g = f; g.bytes++; CHECK(!oca_frame_ok(&g, OC_BUFFER_BYTES));
  g = f; g.width = 1921; g.stride = 1921; g.bytes = 1921 * 720 * 3 / 2; CHECK(!oca_frame_ok(&g, OC_BUFFER_BYTES));
  g = f; g.height = 0x80000000u; CHECK(!oca_frame_ok(&g, OC_BUFFER_BYTES));        // sizes that wrap around
  g = f; g.stride = 640; g.bytes = 640 * 720 * 3 / 2; CHECK(!oca_frame_ok(&g, OC_BUFFER_BYTES));
  g = f; g.format = 9; CHECK(!oca_frame_ok(&g, OC_BUFFER_BYTES));
  g = (struct oc_frame){ .magic = OC_FRAME_MAGIC }; CHECK(!oca_frame_ok(&g, OC_FRAME_DATA));   // an empty buffer
  g = f; g.width = 1920; g.height = 1080; g.stride = 1920; g.bytes = OC_FRAME_BYTES_MAX; CHECK(oca_frame_ok(&g, OC_BUFFER_BYTES));

  struct oneios_camera_frame h;
  memset(&h, 0xee, sizeof h);
  oca_header(&h, &f, OC_FACING_FRONT, 123456789);
  CHECK(!memcmp(h.magic, ONEIOS_CAMERA_MAGIC, 8) && h.header_bytes == 120 && h.width == 1280 && h.height == 720 && h.stride == 1280 && h.format == ONEIOS_CAMERA_NV12);
  CHECK(h.orientation == 270 && h.facing == 0 && h.sequence == 7 && h.time == 123456789 && h.fps == 30 && h.sensor == ONEIOS_CAMERA_FRONT);
  CHECK(((uint64_t)h.host_time_hi << 32 | h.host_time_lo) == f.timestamp_ns && h.focus == ONEIOS_CAMERA_NO_FOCUS && h.gain == h.gain_unity && h.exposure_lines == 0 && h.sensor_w == 0);
  // The layout a NCFRAME1 reader expects (iOSPlus10 userland/camera/negronicam.h).
  CHECK(offsetof(struct oneios_camera_frame, width) == 12 && offsetof(struct oneios_camera_frame, stride) == 20 && offsetof(struct oneios_camera_frame, format) == 24 &&
        offsetof(struct oneios_camera_frame, sequence) == 40 && offsetof(struct oneios_camera_frame, time) == 48 && offsetof(struct oneios_camera_frame, sensor) == 92 &&
        offsetof(struct oneios_camera_frame, mode) == 100);
  printf("camera node: %d checks, %d failed\n", checks, failures);
  return failures ? 1 : 0;
}
