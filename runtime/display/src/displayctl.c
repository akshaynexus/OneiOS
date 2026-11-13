// SPDX-License-Identifier: MIT
// displayctl: the on-phone test of the display helper. It stands in for xnuhost: a frame in a
// memfd, served on a display.sock of its own, with a moving pattern; oneios-display shows it on
// a top layer. With One UI running (README.md):
//   displayctl test [--seconds N] [--size WxH] [--helper PATH] [-- helper options]
// The layer goes away when the socket closes; the helper is ended if it does not leave by itself.
#define _GNU_SOURCE
#include "xhd.h"
#include <errno.h>
#include <libgen.h>
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static uint32_t W = 1440, H = 3120;     // the guest's frame: the panel's native mode
static uint32_t FORMAT = XHD_FORMAT_BGRX, BPP = 4;
static uint8_t *pixels;
static volatile int interrupted;

// The wide band (--format rgba16f): the lowest quarter of the frame, eight patches from the left: sRGB's red, Display
// P3's red, sRGB's green, P3's green, sRGB's blue, P3's blue (as linear extended sRGB: components below 0 and above 1),
// white at 1.0 and white at 2.0 (EDR). On a path that keeps wide colour each P3 patch is more saturated than the
// sRGB one beside it; on one that clips to sRGB the two are the same. The 2.0 white is brighter than the 1.0 one only
// where surfaceflinger gives the layer HDR headroom.
static const float wide[8][3] = {
  { 1, 0, 0 }, { 1.2249f, -0.0420f, -0.0197f }, { 0, 1, 0 }, { -0.2247f, 1.0419f, -0.0786f },
  { 0, 0, 1 }, { 0, 0, 1.0979f }, { 1, 1, 1 }, { 2, 2, 2 },
};

// One pixel from 8-bit sRGB bytes (b, g, r) in the frame's format.
static void put8(uint8_t *p, uint8_t b, uint8_t g, uint8_t r)
{
  if (FORMAT == XHD_FORMAT_BGRX) { p[0] = b; p[1] = g; p[2] = r; p[3] = 255; return; }
  const uint16_t *t = xh_srgb8_to_half();
  uint16_t px[4] = { t[r], t[g], t[b], 0x3c00u };
  memcpy(p, px, 8);
}
static void on_signal(int n) { (void)n; interrupted = 1; }

static double now(void)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (double)t.tv_sec + t.tv_nsec / 1e9;
}

// Eight colour bars with a white frame two pixels wide (so a cut-off edge shows), B,G,R,X.
static void background(uint32_t x, uint32_t y, uint32_t w, uint32_t h)
{
  static const uint8_t bars[8][3] = { { 255, 255, 255 }, { 0, 255, 255 }, { 255, 255, 0 }, { 0, 255, 0 },
                                      { 255, 0, 255 }, { 0, 0, 255 }, { 255, 0, 0 }, { 40, 40, 40 } };
  for (uint32_t row = y; row < y + h && row < H; row++) {
    uint8_t *p = pixels + (size_t)row * W * BPP + (size_t)x * BPP;
    for (uint32_t col = x; col < x + w && col < W; col++, p += BPP) {
      int edge = col < 2 || row < 2 || col >= W - 2 || row >= H - 2;
      if (!edge && FORMAT == XHD_FORMAT_RGBA16F && row >= H - H / 4) {
        const float *c = wide[(uint64_t)col * 8 / W];
        uint16_t px[4] = { xh_half_from_float(c[0]), xh_half_from_float(c[1]), xh_half_from_float(c[2]), 0x3c00u };
        memcpy(p, px, 8);
        continue;
      }
      const uint8_t *c = bars[(uint64_t)col * 8 / W];
      uint8_t shade = (uint8_t)(128 + (uint64_t)row * 127 / H);      // darker at the top
      if (edge) put8(p, 255, 255, 255);
      else put8(p, (uint8_t)(c[0] * shade / 255), (uint8_t)(c[1] * shade / 255), (uint8_t)(c[2] * shade / 255));
    }
  }
}

static void box(uint32_t x, uint32_t y, uint32_t side)
{
  for (uint32_t row = y; row < y + side && row < H; row++) {
    uint8_t *p = pixels + (size_t)row * W * BPP + (size_t)x * BPP;
    for (uint32_t col = x; col < x + side && col < W; col++, p += BPP) put8(p, 255, 255, 255);
  }
}

static int usage(void)
{
  fprintf(stderr, "usage: displayctl test [--seconds N] [--size WxH] [--format bgrx|rgba16f] [--helper PATH] [-- helper options]\n");
  return 2;
}

int main(int argc, char **argv)
{
  if (argc < 2 || strcmp(argv[1], "test")) return usage();
  double seconds = 5;
  char helper[PATH_MAX] = "";
  int extra = argc;
  for (int i = 2; i < argc; i++) {
    if (!strcmp(argv[i], "--")) { extra = i + 1; break; }
    if (i + 1 >= argc) return usage();
    if (!strcmp(argv[i], "--seconds")) seconds = atof(argv[++i]);
    else if (!strcmp(argv[i], "--size")) { if (sscanf(argv[++i], "%ux%u", &W, &H) != 2) return usage(); }
    else if (!strcmp(argv[i], "--helper")) snprintf(helper, sizeof helper, "%s", argv[++i]);
    else if (!strcmp(argv[i], "--format")) {
      const char *v = argv[++i];
      if (!strcmp(v, "bgrx")) FORMAT = XHD_FORMAT_BGRX;
      else if (!strcmp(v, "rgba16f")) FORMAT = XHD_FORMAT_RGBA16F;
      else return usage();
      BPP = xh_pixels_bpp(FORMAT);
    }
    else return usage();
  }
  if (W < 16 || H < 16 || W > 8192 || H > 8192 || seconds <= 0 || seconds > 3600) return usage();
  if (!*helper) {
    char self[PATH_MAX];
    ssize_t n = readlink("/proc/self/exe", self, sizeof self - 1);
    if (n <= 0) { perror("displayctl: /proc/self/exe"); return 2; }
    self[n] = 0;
    snprintf(helper, sizeof helper, "%s/oneios-display", dirname(self));
  }
  if (access(helper, X_OK)) { fprintf(stderr, "displayctl: %s: %s\n", helper, strerror(errno)); return 2; }

  const char *tmp = getenv("TMPDIR");
  char session[PATH_MAX], path[PATH_MAX + 32];
  snprintf(session, sizeof session, "%s/displayctl.XXXXXX", tmp && *tmp ? tmp : "/data/local/tmp");
  if (!mkdtemp(session)) { fprintf(stderr, "displayctl: %s: %s\n", session, strerror(errno)); return 2; }
  snprintf(path, sizeof path, "%s/display.sock", session);
  size_t bytes = (size_t)W * H * BPP;
  int memfd = xhd_memfd("displayctl-frame", bytes);
  int server = xhd_listen(path);
  pixels = memfd >= 0 ? mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, memfd, 0) : MAP_FAILED;
  int status = 1, client = -1;
  pid_t child = -1;
  if (server < 0 || pixels == MAP_FAILED) { fprintf(stderr, "displayctl: socket or frame: %s\n", strerror(errno)); goto out; }
  background(0, 0, W, H);
  signal(SIGINT, on_signal);
  signal(SIGTERM, on_signal);

  child = fork();
  if (child < 0) { perror("displayctl: fork"); goto out; }
  if (child == 0) {
    char **args = calloc((size_t)(argc - extra) + 6, sizeof *args);
    int k = 0;
    args[k++] = helper;
    args[k++] = "--session";
    args[k++] = session;
    args[k++] = "--wait";
    args[k++] = "5";
    for (int i = extra; i < argc; i++) args[k++] = argv[i];
    execv(helper, args);
    fprintf(stderr, "displayctl: %s: %s\n", helper, strerror(errno));
    _exit(127);
  }
  // The helper is a Java runtime starting up: a second or two.
  struct pollfd p = { server, POLLIN, 0 };
  double deadline = now() + 20;
  while (client < 0 && now() < deadline && !interrupted) {
    if (waitpid(child, &status, WNOHANG) == child) { fprintf(stderr, "displayctl: the helper ended before it connected (status %d)\n", status); child = -1; status = 1; goto out; }
    if (poll(&p, 1, 200) > 0) client = accept4(server, NULL, NULL, SOCK_CLOEXEC);
  }
  if (client < 0) { fprintf(stderr, "displayctl: the helper did not connect\n"); status = 1; goto out; }
  if (xhd_send_hello(client, W, H, W * BPP, FORMAT, memfd) || xhd_send_flush(client, 0, 0, W, H)) { perror("displayctl: hello"); status = 1; goto out; }
  printf("displayctl: helper %d connected; %ux%u pattern in %s for %.1f s\n", (int)child, W, H, xh_pixels_name(FORMAT), seconds);
  fflush(stdout);

  // A white square that bounces; each step flushes where it was and where it is.
  uint32_t side = W / 5, x = 2, y = 2, old_x = x, old_y = y;
  int dx = (int)(W / 90) + 1, dy = (int)(H / 150) + 1;
  unsigned long frames = 0;
  double start = now();
  int gone = 0;
  while (now() - start < seconds && !interrupted) {
    background(old_x, old_y, side, side);
    if ((int)x + dx < 2 || x + side + (uint32_t)(dx > 0 ? dx : 0) > W - 2) dx = -dx;
    if ((int)y + dy < 2 || y + side + (uint32_t)(dy > 0 ? dy : 0) > H - 2) dy = -dy;
    x = (uint32_t)((int)x + dx); y = (uint32_t)((int)y + dy);
    box(x, y, side);
    if (xhd_send_flush(client, old_x, old_y, side, side) == -EPIPE || xhd_send_flush(client, x, y, side, side) == -EPIPE) { gone = 1; break; }
    old_x = x; old_y = y;
    frames++;
    struct timespec nap = { 0, 1000000000 / 120 };
    nanosleep(&nap, NULL);
  }
  printf("displayctl: %lu frames in %.2f s%s\n", frames, now() - start, gone ? " (the helper closed the socket early)" : "");
  status = gone;
out:
  if (client >= 0) close(client);                 // the session ends: the helper removes its layer and exits
  if (server >= 0) close(server);
  if (child > 0) {
    int code = -1, waited = 0;
    for (int i = 0; i < 30 && !waited; i++) {
      if (waitpid(child, &code, WNOHANG) == child) waited = 1;
      else { struct timespec nap = { 0, 100000000 }; nanosleep(&nap, NULL); }
    }
    if (!waited) {
      fprintf(stderr, "displayctl: the helper did not leave within 3 s of the socket closing; ending it\n");
      kill(child, SIGTERM);
      sleep(1);
      if (waitpid(child, &code, WNOHANG) != child) { kill(child, SIGKILL); waitpid(child, &code, 0); }
      status = 1;
    } else if (!WIFEXITED(code) || WEXITSTATUS(code)) {
      fprintf(stderr, "displayctl: the helper ended with status 0x%x\n", code);
      status = 1;
    } else printf("displayctl: the helper removed its layer and exited 0\n");
  }
  unlink(path);
  rmdir(session);
  printf("displayctl: %s\n", status ? "FAILED" : "ok");
  return status ? 1 : 0;
}
