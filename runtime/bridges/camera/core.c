// SPDX-License-Identifier: MIT
// The camera helper's core: one back end, one running stream, its frames converted to NV12 straight
// into the ring. Also the test back end (a moving picture without a camera).
#define _GNU_SOURCE
#include "camera.h"
#include <dirent.h>
#include <errno.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

int cam_verbose;

void cam_log(const char *fmt, ...)
{
  char line[400];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(line, sizeof line, fmt, ap);
  va_end(ap);
  fprintf(stderr, "oneios-camera: %s\n", line);
}

bool cam_system_server_running(void)
{
  DIR *proc = opendir("/proc");
  bool found = false;
  struct dirent *e;
  if (!proc) return false;
  while (!found && (e = readdir(proc))) {
    if (e->d_name[0] < '1' || e->d_name[0] > '9') continue;
    char path[64], name[32] = "";
    snprintf(path, sizeof path, "/proc/%.20s/cmdline", e->d_name);
    FILE *f = fopen(path, "r");
    if (!f) continue;
    if (fgets(name, sizeof name, f) && !strcmp(name, "system_server")) found = true;
    fclose(f);
  }
  closedir(proc);
  return found;
}

// ---- the stream -------------------------------------------------------------------------------
static void core_frame(void *ctx, const cam_yuv *picture, uint64_t timestamp_ns)
{
  cam_core *core = ctx;
  if (!core->running || picture->width <= 0 || picture->height <= 0 || (picture->width & 1) || (picture->height & 1)) return;
  uint32_t bytes = (uint32_t)picture->width * (uint32_t)picture->height * 3 / 2;
  if (bytes > core->ring.slot_bytes) return;
  unsigned slot;
  uint8_t *dst = cam_ring_begin(&core->ring, &slot);
  cam_yuv_to_nv12(picture, dst, picture->width);
  uint64_t sequence = ++core->sequence;
  struct oc_frame frame = { .magic = OC_FRAME_MAGIC, .sequence = sequence, .timestamp_ns = timestamp_ns, .width = (uint32_t)picture->width,
                            .height = (uint32_t)picture->height, .stride = (uint32_t)picture->width, .format = OC_FMT_NV12, .bytes = bytes,
                            .role = core->camera->role, .orientation = (uint32_t)core->camera->orientation, .fps = (uint32_t)core->fps };
  cam_ring_commit(&core->ring, slot, &frame);
  if (++core->frames == 1) core->first_ns = timestamp_ns;
  core->last_ns = timestamp_ns;
  if (core->on_frame) core->on_frame(core->ctx, sequence);
}

static void core_error(void *ctx, int error)
{
  cam_core *core = ctx;
  cam_log("%s: the stream ended: %s", core->backend->name, strerror(error));
  if (core->on_error) core->on_error(core->ctx, error);
}

int cam_core_init(cam_core *core, const char *backend)
{
  const cam_backend *chosen = NULL;
  if (!backend || !strcmp(backend, "auto")) {
#ifdef __ANDROID__
    // With One UI's framework running, cameraserver's own way (it keeps its book of who has the
    // camera). In a session cameraserver refuses every client (no permission service): the HAL.
    chosen = cam_system_server_running() ? &cam_backend_ndk : &cam_backend_hal;
#else
    chosen = &cam_backend_test;
#endif
  } else if (!strcmp(backend, "test")) chosen = &cam_backend_test;
#ifdef __ANDROID__
  else if (!strcmp(backend, "ndk")) chosen = &cam_backend_ndk;
  else if (!strcmp(backend, "hal")) chosen = &cam_backend_hal;
#endif
  if (!chosen) return -EINVAL;
  core->backend = chosen;
  if (core->ring.ring == NULL) {
    int rc = cam_ring_create(&core->ring, CAM_RING_SLOTS, OC_FRAME_BYTES_MAX);
    if (rc) return rc;
  }
  int count = chosen->list(core->cameras, CAM_MAX_LIST);
  if (count < 0) return count;
  core->ncameras = count;
  cam_assign_roles(core->cameras, count);
  return 0;
}

const cam_info *cam_core_find(const cam_core *core, const char *which)
{
  if (!strncmp(which, "id:", 3)) {
    for (int i = 0; i < core->ncameras; i++) if (!strcmp(core->cameras[i].id, which + 3)) return &core->cameras[i];
    return NULL;
  }
  uint32_t role = cam_role_from_name(which);
  if (role) {
    for (int i = 0; i < core->ncameras; i++) if (core->cameras[i].role == role) return &core->cameras[i];
    return NULL;
  }
  char *end;
  long index = strtol(which, &end, 10);
  return *which && !*end && index >= 0 && index < core->ncameras ? &core->cameras[index] : NULL;
}

int cam_core_start(cam_core *core, const cam_info *camera, int width, int height, int fps)
{
  int w, h, lo, hi;
  if (core->running) cam_core_stop(core);
  if (!fps) fps = 30;
  if (cam_pick_size(camera, width, height, &w, &h)) { cam_log("camera %s has no picture size within %ux%u", camera->id, OC_MAX_WIDTH, OC_MAX_HEIGHT); return -ENOENT; }
  (void)cam_pick_fps(camera, fps, &lo, &hi);
  core->camera = camera;
  core->width = w;
  core->height = h;
  core->fps = hi;
  core->frames = 0;
  core->first_ns = core->last_ns = 0;
  core->running = true;
  int rc = core->backend->start(camera, w, h, lo, hi, core_frame, core_error, core);
  if (rc) { core->running = false; return rc; }
  cam_log("%s: camera %s (%s) %dx%d at %d-%d fps", core->backend->name, camera->id, cam_role_name(camera->role), w, h, lo, hi);
  return 0;
}

void cam_core_stop(cam_core *core)
{
  if (!core->running) return;
  core->backend->stop();                    // returns when no frame callback runs any more
  core->running = false;
}

// ---- the test back end --------------------------------------------------------------------------
// Five made-up cameras with the S24 Ultra's lenses; the picture is colour bars with a bar that moves
// one step a frame, so a reader can tell frames apart and check their order.
static struct {
  pthread_t thread;
  bool run;
  int width, height, fps;
  cam_frame_fn frame;
  void *ctx;
} test;

static int test_list(cam_info *out, int max)
{
  static const struct { const char *id; int facing; float focal, pw, ph; int orientation; } lenses[5] = {
    { "0", OC_FACING_BACK, 6.3f, 9.83f, 7.37f, 90 }, { "1", OC_FACING_FRONT, 3.3f, 4.5f, 3.4f, 270 },
    { "2", OC_FACING_BACK, 2.2f, 5.9f, 4.4f, 90 }, { "3", OC_FACING_BACK, 7.9f, 4.1f, 3.1f, 90 }, { "4", OC_FACING_BACK, 18.6f, 5.8f, 4.35f, 90 },
  };
  int count = 0;
  for (int i = 0; i < 5 && count < max; i++, count++) {
    cam_info *c = &out[count];
    memset(c, 0, sizeof *c);
    snprintf(c->id, sizeof c->id, "%s", lenses[i].id);
    c->facing = lenses[i].facing; c->orientation = lenses[i].orientation;
    c->focal_mm = lenses[i].focal; c->physical_w = lenses[i].pw; c->physical_h = lenses[i].ph;
    c->pixel_w = 4000; c->pixel_h = 3000;
    static const int sizes[][2] = { { 4000, 3000 }, { 1920, 1080 }, { 1440, 1080 }, { 1280, 720 }, { 640, 480 }, { 320, 240 } };
    for (unsigned s = 0; s < 6; s++) { c->sizes[s].w = sizes[s][0]; c->sizes[s].h = sizes[s][1]; }
    c->nsizes = 6;
    c->fps[0].lo = 15; c->fps[0].hi = 30; c->fps[1].lo = 30; c->fps[1].hi = 30; c->fps[2].lo = 60; c->fps[2].hi = 60;
    c->nfps = 3;
    cam_info_finish(c);
  }
  return count;
}

void cam_test_picture(uint8_t *y, uint8_t *cb, uint8_t *cr, int width, int height, uint64_t n);
void cam_test_picture(uint8_t *y, uint8_t *cb, uint8_t *cr, int width, int height, uint64_t n)
{
  // Eight bars (white, yellow, cyan, green, magenta, red, blue, black) in Y, Cb, Cr.
  static const uint8_t bars[8][3] = { { 235, 128, 128 }, { 210, 16, 146 }, { 170, 166, 16 }, { 145, 54, 34 }, { 106, 202, 222 }, { 81, 90, 240 }, { 41, 240, 110 }, { 16, 128, 128 } };
  int marker = (int)(n * 8 % (uint64_t)width);
  for (int row = 0; row < height; row++)
    for (int x = 0; x < width; x++) {
      const uint8_t *bar = bars[x * 8 / width];
      bool mark = x >= marker && x < marker + 8;
      y[(size_t)row * (size_t)width + (size_t)x] = mark ? 255 : bar[0];
      if (!(row & 1) && !(x & 1)) {
        cb[(size_t)(row / 2) * (size_t)(width / 2) + (size_t)(x / 2)] = mark ? 128 : bar[1];
        cr[(size_t)(row / 2) * (size_t)(width / 2) + (size_t)(x / 2)] = mark ? 128 : bar[2];
      }
    }
}

static void *test_run(void *arg)
{
  (void)arg;
  size_t luma = (size_t)test.width * (size_t)test.height;
  uint8_t *planes = malloc(luma * 3 / 2);
  struct timespec next;
  clock_gettime(CLOCK_MONOTONIC, &next);
  for (uint64_t n = 0; planes && __atomic_load_n(&test.run, __ATOMIC_ACQUIRE); n++) {
    cam_test_picture(planes, planes + luma, planes + luma + luma / 4, test.width, test.height, n);
    cam_yuv picture = { .y = planes, .cb = planes + luma, .cr = planes + luma + luma / 4, .y_stride = test.width, .c_stride = test.width / 2,
                        .c_step = 1, .width = test.width, .height = test.height };
    test.frame(test.ctx, &picture, (uint64_t)next.tv_sec * 1000000000ull + (uint64_t)next.tv_nsec);
    next.tv_nsec += 1000000000l / test.fps;
    if (next.tv_nsec >= 1000000000l) { next.tv_nsec -= 1000000000l; next.tv_sec++; }
    clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &next, NULL);
  }
  free(planes);
  return NULL;
}

static int test_start(const cam_info *camera, int width, int height, int fps_lo, int fps_hi, cam_frame_fn frame, cam_error_fn error, void *ctx)
{
  (void)camera; (void)fps_lo; (void)error;
  test.width = width; test.height = height; test.fps = fps_hi > 0 ? fps_hi : 30;
  test.frame = frame; test.ctx = ctx;
  test.run = true;
  if (pthread_create(&test.thread, NULL, test_run, NULL)) { test.run = false; return -EAGAIN; }
  return 0;
}

static void test_stop(void)
{
  if (!test.run) return;
  __atomic_store_n(&test.run, false, __ATOMIC_RELEASE);
  pthread_join(test.thread, NULL);
}

const cam_backend cam_backend_test = { "test", test_list, test_start, test_stop };
