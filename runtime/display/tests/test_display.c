// SPDX-License-Identifier: MIT
// Host tests of the display helper's core: the display.sock protocol against a serving thread
// (what xnuhost does), the letterbox fit, the damage rectangles, the row copy; and displayctl
// against a stand-in helper. No phone, no Apple files.
//   test_display [path to a host build of displayctl]
#define _GNU_SOURCE
#include "xhd.h"
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int checks, failed;
static int as_helper(const char *session);
#define CHECK(c) do { checks++; if (!(c)) { failed++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); } } while (0)

static void test_fit(void)
{
  struct xhd_fit f;
  xhd_fit(1320, 2868, 1440, 3120, &f);                 // the iPhone frame on the S24 Ultra's panel
  CHECK(f.w == 1436 && f.h == 3120 && f.x == 2 && f.y == 0);
  xhd_fit(1320, 2868, 1080, 2340, &f);                 // One UI at FHD+
  CHECK(f.w == 1077 && f.h == 2340 && f.x == 1 && f.y == 0);
  xhd_fit(1440, 3120, 1440, 3120, &f);
  CHECK(f.w == 1440 && f.h == 3120 && f.x == 0 && f.y == 0);
  xhd_fit(720, 1560, 1440, 3120, &f);                  // same aspect, scaled up
  CHECK(f.w == 1440 && f.h == 3120 && f.x == 0 && f.y == 0);
  xhd_fit(1920, 1080, 1440, 3120, &f);                 // wide in tall: bars above and below
  CHECK(f.w == 1440 && f.h == 810 && f.x == 0 && f.y == 1155);
  xhd_fit(1320, 2868, 3120, 1440, &f);                 // tall in wide: bars left and right
  CHECK(f.h == 1440 && f.w == 663 && f.x == 1228 && f.y == 0);
  xhd_fit(1, 10000, 100, 100, &f);                     // never thinner than a pixel
  CHECK(f.w == 1 && f.h == 100);
  xhd_fit(16384, 16384, 16384, 16384, &f);             // no overflow at the largest sizes
  CHECK(f.w == 16384 && f.h == 16384);
  xhd_fit(0, 10, 100, 100, &f);
  CHECK(f.w == 0 && f.h == 0);
  for (uint32_t sw = 1; sw < 400; sw += 7) for (uint32_t sh = 1; sh < 400; sh += 11) {
    xhd_fit(sw, sh, 144, 312, &f);
    if (!(f.w >= 1 && f.h >= 1 && f.x + f.w <= 144 && f.y + f.h <= 312 && (f.w == 144 || f.h == 312))) { CHECK(!"fit inside the panel, touching two edges"); return; }
  }
  CHECK(1);
}

static void test_rects(void)
{
  struct xhd_rect r = { 10, 20, 30, 40 };
  CHECK(xhd_rect_clip(&r, 100, 100) && r.x == 10 && r.y == 20 && r.w == 30 && r.h == 40);
  r = (struct xhd_rect){ 90, 95, 30, 40 };
  CHECK(xhd_rect_clip(&r, 100, 100) && r.w == 10 && r.h == 5);
  r = (struct xhd_rect){ 100, 0, 1, 1 };
  CHECK(!xhd_rect_clip(&r, 100, 100) && r.w == 0);
  r = (struct xhd_rect){ 5, 5, 0xffffffffu, 0xffffffffu };              // a width that would wrap
  CHECK(xhd_rect_clip(&r, 100, 100) && r.w == 95 && r.h == 95);
  r = (struct xhd_rect){ 0xfffffff0u, 0, 0x20, 1 };
  CHECK(!xhd_rect_clip(&r, 100, 100));
  r = (struct xhd_rect){ 0, 0, 0, 5 };
  CHECK(!xhd_rect_clip(&r, 100, 100));
  struct xhd_rect u = { 0, 0, 0, 0 }, a = { 10, 10, 5, 5 }, b = { 50, 2, 10, 10 }, none = { 7, 7, 0, 0 };
  xhd_rect_union(&u, &a);
  CHECK(u.x == 10 && u.y == 10 && u.w == 5 && u.h == 5);
  xhd_rect_union(&u, &b);
  CHECK(u.x == 10 && u.y == 2 && u.w == 50 && u.h == 13);
  xhd_rect_union(&u, &none);
  CHECK(u.x == 10 && u.y == 2 && u.w == 50 && u.h == 13);
}

static void test_hello_check(void)
{
  struct xhd_message m = { { 'X', 'H', 'D', '1' }, 1320, 2868, 5280, 1 };
  off_t size = (off_t)5280 * 2868;
  CHECK(xhd_check_hello(&m, sizeof m, size) == 0);
  CHECK(xhd_check_hello(&m, sizeof m, size + 4096) == 0);
  CHECK(xhd_check_hello(&m, sizeof m, size - 1) == -EINVAL);            // a frame larger than its memfd
  CHECK(xhd_check_hello(&m, sizeof m - 4, size) == -EPROTO);
  struct xhd_message bad = m;
  bad.magic[3] = 'F';
  CHECK(xhd_check_hello(&bad, sizeof bad, size) == -EPROTO);
  bad = m; bad.a = 0;
  CHECK(xhd_check_hello(&bad, sizeof bad, size) == -EINVAL);
  bad = m; bad.b = 20000;
  CHECK(xhd_check_hello(&bad, sizeof bad, (off_t)1 << 40) == -EINVAL);
  bad = m; bad.c = 5276;                                                // a stride shorter than a row
  CHECK(xhd_check_hello(&bad, sizeof bad, size) == -EINVAL);
  bad = m; bad.c = 5281;
  CHECK(xhd_check_hello(&bad, sizeof bad, size * 2) == -EINVAL);
  bad = m; bad.d = 2;                                                   // a format this helper does not know
  CHECK(xhd_check_hello(&bad, sizeof bad, size) == -EINVAL);
  bad = m; bad.a = 0x40000001u;                                         // width * 4 wraps to 4
  CHECK(xhd_check_hello(&bad, sizeof bad, size) == -EINVAL);
}

static void test_copy(void)
{
  enum { W = 7, H = 5, SS = 40, DS = 32 };                              // strides wider than the rows
  uint8_t src[SS * H], dst[DS * H], want[DS * H];
  for (size_t i = 0; i < sizeof src; i++) src[i] = (uint8_t)(i * 7 + 1);
  memset(dst, 0xee, sizeof dst);
  memset(want, 0xee, sizeof want);
  struct xhd_rect r = { 2, 1, 4, 3 };
  xhd_copy(dst, DS, src, SS, r, 0);
  for (uint32_t y = 1; y < 4; y++) memcpy(want + y * DS + 8, src + y * SS + 8, 16);
  CHECK(!memcmp(dst, want, sizeof dst));                                // only the rectangle, bytes as they are
  xhd_copy(dst, DS, src, SS, r, 1);
  int ok = 1;
  for (uint32_t y = 1; y < 4; y++) for (uint32_t x = 2; x < 6; x++) {
    const uint8_t *s = src + y * SS + x * 4, *d = dst + y * DS + x * 4;
    if (d[0] != s[2] || d[1] != s[1] || d[2] != s[0] || d[3] != 0xff) ok = 0;
  }
  CHECK(ok);                                                            // B and R swapped, alpha opaque
  CHECK(dst[0] == 0xee && dst[DS * 4] == 0xee && dst[DS + 7] == 0xee && dst[DS + 24] == 0xee);
}

// ---- the protocol against a serving thread --------------------------------------------------
struct server { char path[200]; int delay_ms; uint32_t width, height; int mode; int done; };

static void *serve(void *arg)
{
  struct server *s = arg;
  usleep((useconds_t)s->delay_ms * 1000);                               // the helper is already waiting for the socket
  int listener = xhd_listen(s->path);
  int fd = accept(listener, NULL, NULL);
  size_t bytes = (size_t)s->width * 4 * s->height;
  int memfd = xhd_memfd("test-frame", s->mode == 1 ? bytes / 2 : bytes);
  if (s->mode != 1) {
    uint8_t *p = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, memfd, 0);
    for (size_t i = 0; i < bytes; i++) p[i] = (uint8_t)(i % 251);
    munmap(p, bytes);
  }
  if (s->mode == 2) {                                                   // a hello with no descriptor
    struct xhd_message m = { { 'X', 'H', 'D', '1' }, s->width, s->height, s->width * 4, 1 };
    send(fd, &m, sizeof m, 0);
  } else xhd_send_hello(fd, s->width, s->height, s->width * 4, memfd);
  if (s->mode == 0) {
    xhd_send_flush(fd, 0, 0, s->width, s->height);                      // what xnuhost sends at a connection
    char ack;
    recv(fd, &ack, 1, 0);                                               // the test read the full flush
    xhd_send_flush(fd, 10, 10, 20, 20);
    xhd_send_flush(fd, 100, 5, 20, 20);
    xhd_send_flush(fd, s->width - 4, s->height - 4, 50, 50);            // reaches past the frame: clipped
    xhd_send_flush(fd, s->width + 5, 0, 5, 5);                          // outside: dropped
    struct xhd_message junk = { { 'N', 'O', 'P', 'E' }, 0, 0, 9999, 9999 };
    send(fd, &junk, sizeof junk, 0);                                    // unknown: ignored
    send(fd, "short", 5, 0);
    recv(fd, &ack, 1, 0);
  }
  close(memfd);
  close(fd);
  close(listener);
  s->done = 1;
  return NULL;
}

static void test_protocol(const char *dir)
{
  struct server s = { .delay_ms = 300, .width = 64, .height = 48 };
  snprintf(s.path, sizeof s.path, "%s/display.sock", dir);
  pthread_t thread;
  pthread_create(&thread, NULL, serve, &s);
  CHECK(xhd_connect(s.path, 0, NULL) == -ENOENT);                       // not there yet, and no wait asked
  int fd = xhd_connect(s.path, 5000, NULL);                             // waits for xnuhost to make it
  CHECK(fd >= 0);
  struct xhd_frame frame;
  CHECK(xhd_recv_hello(fd, &frame, 5000) == 0);
  CHECK(frame.width == 64 && frame.height == 48 && frame.stride == 256 && frame.format == XHD_FORMAT_BGRX && frame.bytes == 256 * 48);
  int same = 1;
  for (size_t i = 0; i < frame.bytes; i++) if (frame.pixels[i] != (uint8_t)(i % 251)) same = 0;
  CHECK(same);                                                          // the shared frame, mapped
  struct xhd_rect damage = { 0, 0, 0, 0 };
  for (int i = 0; i < 200 && !damage.w; i++) { CHECK(xhd_drain(fd, &frame, &damage) == 0); usleep(10000); }
  CHECK(damage.x == 0 && damage.y == 0 && damage.w == 64 && damage.h == 48);
  damage = (struct xhd_rect){ 0, 0, 0, 0 };
  CHECK(xhd_drain(fd, &frame, &damage) == 0 && damage.w == 0);          // nothing pending, and it did not block
  send(fd, "a", 1, 0);
  usleep(200000);
  CHECK(xhd_drain(fd, &frame, &damage) == 0);
  // 10,10 20x20 + 100,5 20x20 (clipped to 64 wide: dropped, x >= width) + the corner 60,44 4x4
  CHECK(damage.x == 10 && damage.y == 10 && damage.w == 54 && damage.h == 38);
  send(fd, "b", 1, 0);
  pthread_join(thread, NULL);
  damage = (struct xhd_rect){ 0, 0, 0, 0 };
  CHECK(xhd_drain(fd, &frame, &damage) == -1);                          // xnuhost closed: the session is over
  xhd_frame_close(&frame);
  CHECK(frame.pixels == NULL && frame.memfd == -1);
  close(fd);

  // Refused hellos: a frame larger than its memfd, and no memfd at all.
  for (int mode = 1; mode <= 2; mode++) {
    s = (struct server){ .delay_ms = 0, .width = 64, .height = 48, .mode = mode };
    snprintf(s.path, sizeof s.path, "%s/display.sock", dir);
    pthread_create(&thread, NULL, serve, &s);
    fd = xhd_connect(s.path, 5000, NULL);
    CHECK(fd >= 0);
    int e = xhd_recv_hello(fd, &frame, 5000);
    CHECK(e == (mode == 1 ? -EINVAL : -EPROTO));
    CHECK(frame.pixels == NULL);
    close(fd);
    pthread_join(thread, NULL);
  }

  // Not a socket: a link (even to a socket) and a plain file are refused; a stop ends the wait.
  char other[256], link_path[256];
  snprintf(other, sizeof other, "%s/real.sock", dir);
  snprintf(link_path, sizeof link_path, "%s/link.sock", dir);
  int listener = xhd_listen(other);
  CHECK(listener >= 0);
  CHECK(symlink(other, link_path) == 0);
  CHECK(xhd_connect(link_path, 0, NULL) == -ENOTSOCK);
  close(listener);
  unlink(link_path);
  unlink(other);
  FILE *plain = fopen(link_path, "w");
  fclose(plain);
  CHECK(xhd_connect(link_path, 1000, NULL) == -ENOTSOCK);
  unlink(link_path);
  volatile int stop = 1;
  CHECK(xhd_connect(link_path, 60000, &stop) == -ENOENT);               // returns at once
  // A socket nobody listens on any more: refused, after the wait.
  listener = xhd_listen(other);
  close(listener);
  CHECK(xhd_connect(other, 200, NULL) == -ECONNREFUSED);
  unlink(other);
  // A hello that never comes.
  int pair[2];
  CHECK(socketpair(AF_UNIX, SOCK_SEQPACKET, 0, pair) == 0);
  CHECK(xhd_recv_hello(pair[0], &frame, 100) == -ETIMEDOUT);
  close(pair[1]);
  CHECK(xhd_recv_hello(pair[0], &frame, 100) == -ECONNRESET);
  close(pair[0]);
  unlink(s.path);
}

// displayctl with a stand-in for the helper: a shell script that takes the session, and leaves.
static const char *self_path;
static void test_displayctl(const char *displayctl, const char *dir)
{
  char helper[256], command[1024];
  snprintf(helper, sizeof helper, "%s/helper.sh", dir);
  FILE *f = fopen(helper, "w");
  fprintf(f, "#!/bin/sh\necho \"$@\" > %s/helper.args\nexit 7\n", dir);
  fclose(f);
  chmod(helper, 0755);
  snprintf(command, sizeof command, "TMPDIR=%s %s test --seconds 1 --size 64x48 --helper %s -- --hz 60 > %s/out 2>&1", dir, displayctl, helper, dir);
  int status = system(command);
  CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 1);                  // a helper that never connects is a failure
  snprintf(command, sizeof command, "grep -q 'session .*displayctl\\..* --wait 5 --hz 60' %s/helper.args", dir);
  CHECK(system(command) == 0);                                          // it was started on the session, options passed on
  snprintf(command, sizeof command, "grep -q 'ended before it connected' %s/out && ! ls -d %s/displayctl.* 2>/dev/null", dir, dir);
  CHECK(system(command) == 0);                                          // and its folder is gone
  snprintf(command, sizeof command, "rm -f %s/helper.sh %s/helper.args %s/out", dir, dir, dir);
  CHECK(system(command) == 0);
  // With a helper that speaks the protocol (this program, see main): the whole exchange passes.
  snprintf(command, sizeof command, "TMPDIR=%s %s test --seconds 1 --size 64x48 --helper %s > %s/out 2>&1", dir, displayctl, self_path, dir);
  status = system(command);
  CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
  snprintf(command, sizeof command, "grep -q 'removed its layer and exited 0' %s/out && grep -q '^displayctl: ok' %s/out && rm -f %s/out", dir, dir, dir);
  CHECK(system(command) == 0);
}

// This program as the helper displayctl starts (--session DIR --wait N): connects, takes the hello,
// reads flushes until the socket closes, and checks it saw the moving square's damage.
static int as_helper(const char *session)
{
  char path[300];
  snprintf(path, sizeof path, "%s/display.sock", session);
  int fd = xhd_connect(path, 5000, NULL);
  struct xhd_frame frame;
  if (fd < 0 || xhd_recv_hello(fd, &frame, 5000)) return 9;
  struct xhd_rect damage = { 0, 0, 0, 0 };
  int full = 0, partial = 0;
  for (int i = 0; i < 2000; i++) {
    int state = xhd_drain(fd, &frame, &damage);
    if (damage.w == frame.width && damage.h == frame.height) full++;
    else if (damage.w) partial++;
    damage = (struct xhd_rect){ 0, 0, 0, 0 };
    if (state < 0) return full && partial && frame.pixels[3] == 255 ? 0 : 8;
    usleep(5000);
  }
  return 7;
}

int main(int argc, char **argv)
{
  if (argc > 2 && !strcmp(argv[1], "--session")) return as_helper(argv[2]);
  self_path = argv[0];
  char dir[] = "/tmp/oneios-display-test.XXXXXX";
  if (!mkdtemp(dir)) { perror("mkdtemp"); return 1; }
  test_fit();
  test_rects();
  test_hello_check();
  test_copy();
  test_protocol(dir);
  if (argc > 1) test_displayctl(argv[1], dir);
  rmdir(dir);
  printf("%s: %d checks, %d failed\n", failed ? "FAILED" : "ok", checks, failed);
  return failed ? 1 : 0;
}
