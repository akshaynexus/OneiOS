// SPDX-License-Identifier: MIT
// camctl: tries the camera bridge on the phone without xnuhost (root; One UI may be running).
//
//   camctl [options] list                     the cameras of the back end, with the role each one gets
//   camctl [options] capture                  opens a camera, takes frames out of the ring, writes one as a PNG
//   camctl [options] helper PATH              the same through the helper process (PATH = oneios-camera), the
//                                             way xnuhost uses it: socket, ring descriptor, frame notes
// options:
//   --backend auto|ndk|hal|test   auto: ndk while system_server runs, else hal
//   --camera WHICH                wide (default), front, ultrawide, tele3, tele5, id:<camera id>, or a list index
//   --size WxH                    default 1280x720 (at most 1920x1080)
//   --fps N                       default 30
//   --frames N                    default 60
//   --out FILE.png                default /data/local/tmp/oneios-camera-<camera>.png
//   --nv12 FILE                   also the same frame's raw NV12
//   -v
// Exit: 0 frames arrived and the picture was written; 1 not; 2 usage.
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include "bridges/camera.h"
#include "camera.h"
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t wake = PTHREAD_COND_INITIALIZER;
static uint64_t notes;
static int ended;

static void note(void)
{
  pthread_mutex_lock(&lock);
  notes++;
  pthread_cond_signal(&wake);
  pthread_mutex_unlock(&lock);
}
static void core_frame(void *ctx, uint64_t sequence) { (void)ctx; (void)sequence; note(); }
static void core_error(void *ctx, int error) { (void)ctx; pthread_mutex_lock(&lock); ended = error; pthread_cond_signal(&wake); pthread_mutex_unlock(&lock); }
static void link_event(void *ctx, int kind, int status)
{
  (void)ctx;
  if (kind == BR_CAMERA_FRAME) note();
  else if (status) core_error(NULL, status);
}

// Waits for a note newer than *seen, at most ms. False on a time-out or when the stream ended.
static bool wait_note(uint64_t *seen, int ms)
{
  struct timespec until;
  clock_gettime(CLOCK_REALTIME, &until);
  until.tv_sec += ms / 1000;
  until.tv_nsec += (long)(ms % 1000) * 1000000l;
  if (until.tv_nsec >= 1000000000l) { until.tv_nsec -= 1000000000l; until.tv_sec++; }
  pthread_mutex_lock(&lock);
  while (notes == *seen && !ended) if (pthread_cond_timedwait(&wake, &lock, &until) == ETIMEDOUT) break;
  bool fresh = notes != *seen;
  *seen = notes;
  pthread_mutex_unlock(&lock);
  return fresh;
}

static double now(void)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}

static void print_camera(int index, const cam_info *c)
{
  printf("%2d  id %-4s %-9s %s  %5.2f mm (35 mm equivalent %3.0f)  sensor %dx%d  turned %3d  %s%s\n", index, c->id, cam_role_name(c->role),
         c->facing == OC_FACING_FRONT ? "front" : c->facing == OC_FACING_BACK ? "back " : "other", (double)c->focal_mm, (double)c->equiv35,
         c->pixel_w, c->pixel_h, c->orientation, c->logical ? "logical " : "", c->hal_buffers ? "hal-buffers" : "");
  printf("      YUV sizes:");
  int shown = 0;
  for (int i = 0; i < c->nsizes; i++)
    if (c->sizes[i].w <= (int)OC_MAX_WIDTH && c->sizes[i].h <= (int)OC_MAX_HEIGHT && shown++ < 12) printf(" %dx%d", c->sizes[i].w, c->sizes[i].h);
  printf("%s (%d in all)\n      fps ranges:", shown > 12 ? " ..." : "", c->nsizes);
  for (int i = 0; i < c->nfps; i++) printf(" %d-%d", c->fps[i].lo, c->fps[i].hi);
  printf("\n");
}

typedef struct { uint64_t frames, missed, first_seq, last_seq, first_ns, last_ns; double first_wall, last_wall; } stats;

static void count_frame(stats *s, const struct oc_frame *f)
{
  double wall = now();
  if (!s->frames) { s->first_seq = f->sequence; s->first_ns = f->timestamp_ns; s->first_wall = wall; }
  else if (f->sequence > s->last_seq + 1) s->missed += f->sequence - s->last_seq - 1;
  s->last_seq = f->sequence; s->last_ns = f->timestamp_ns; s->last_wall = wall;
  s->frames++;
}

static int report(const stats *s, const struct oc_frame *f, const uint8_t *nv12, const char *out, const char *raw, double open_seconds)
{
  if (!s->frames) { printf("no frame arrived\n"); return 1; }
  // The camera's rate: frames it made (read or missed) over the sensor's own time; ours: what was read.
  uint64_t made = s->last_seq - s->first_seq;
  double sensor = s->last_ns > s->first_ns ? (double)(s->last_ns - s->first_ns) / 1e9 : 0, wall = s->last_wall - s->first_wall;
  printf("frames: %llu read, %llu missed by the reader; first frame %.0f ms after the start\n", (unsigned long long)s->frames, (unsigned long long)s->missed, open_seconds * 1000);
  if (made && sensor > 0) printf("fps: %.2f by the sensor's time", (double)made / sensor);
  else printf("fps: no sensor time");
  if (s->frames > 1 && wall > 0) printf(", %.2f read (wall clock)\n", (double)(s->frames - 1) / wall);
  else printf("\n");
  uint64_t sum = 0;
  for (uint32_t row = 0; row < f->height; row++) for (uint32_t x = 0; x < f->width; x++) sum += nv12[(size_t)row * f->stride + x];
  printf("picture: %ux%u NV12, sequence %llu, mean luma %.1f%s\n", f->width, f->height, (unsigned long long)f->sequence,
         (double)sum / ((double)f->width * f->height), sum < (uint64_t)f->width * f->height * 3 ? " (black: a covered lens or no light?)" : "");
  uint8_t *rgb = malloc((size_t)f->width * f->height * 3);
  if (!rgb) return 1;
  cam_nv12_to_rgb(nv12, (int)f->width, (int)f->height, (int)f->stride, rgb);
  int rc = cam_png_write(out, rgb, (int)f->width, (int)f->height);
  free(rgb);
  if (rc) { printf("%s: %s\n", out, strerror(-rc)); return 1; }
  printf("wrote %s\n", out);
  if (raw) {
    FILE *file = fopen(raw, "wb");
    if (!file || fwrite(nv12, 1, f->bytes, file) != f->bytes) { printf("%s: %s\n", raw, strerror(errno)); if (file) fclose(file); return 1; }
    fclose(file);
    printf("wrote %s (%u bytes, NV12 %ux%u)\n", raw, f->bytes, f->width, f->height);
  }
  return 0;
}

int main(int argc, char **argv)
{
  const char *backend = "auto", *which = "wide", *out = NULL, *raw = NULL, *command = NULL, *helper = NULL;
  int width = 1280, height = 720, fps = 30, frames = 60;
  for (int i = 1; i < argc; i++) {
    const char *a = argv[i], *v = i + 1 < argc ? argv[i + 1] : NULL;
    if (!strcmp(a, "-v")) cam_verbose = 1;
    else if (!strcmp(a, "--backend") && v) { backend = v; i++; }
    else if (!strcmp(a, "--camera") && v) { which = v; i++; }
    else if (!strcmp(a, "--size") && v && sscanf(v, "%dx%d", &width, &height) == 2) i++;
    else if (!strcmp(a, "--fps") && v) { fps = atoi(v); i++; }
    else if (!strcmp(a, "--frames") && v) { frames = atoi(v); i++; }
    else if (!strcmp(a, "--out") && v) { out = v; i++; }
    else if (!strcmp(a, "--nv12") && v) { raw = v; i++; }
    else if (!command && (!strcmp(a, "list") || !strcmp(a, "capture"))) command = a;
    else if (!command && !strcmp(a, "helper") && v) { command = a; helper = v; i++; }
    else { command = NULL; break; }
  }
  if (!command || frames < 1 || fps < 1 || width < 2 || height < 2 || width > (int)OC_MAX_WIDTH || height > (int)OC_MAX_HEIGHT) {
    fprintf(stderr, "usage: camctl [--backend auto|ndk|hal|test] [--camera wide|front|ultrawide|tele3|tele5|id:ID|INDEX] [--size WxH] [--fps N]\n"
                    "              [--frames N] [--out FILE.png] [--nv12 FILE] [-v] list | capture | helper PATH-TO-oneios-camera\n");
    return 2;
  }
  char default_out[128];
  snprintf(default_out, sizeof default_out, "/data/local/tmp/oneios-camera-%s.png", strncmp(which, "id:", 3) ? which : which + 3);
  if (!out) out = default_out;
  printf("system_server: %s\n", cam_system_server_running() ? "running (One UI's framework is up)" : "not running (a session, or the framework is stopped)");

  static uint8_t picture[OC_FRAME_BYTES_MAX];
  setvbuf(stdout, NULL, _IOLBF, 0);
  struct oc_frame frame = { 0 }, kept = { 0 };
  stats s = { 0 };
  uint64_t seen = 0;

  if (!strcmp(command, "helper")) {
    // As xnuhost does it: the helper process, its socket, the ring it passes.
    br_camera *camera = br_camera_helper(helper, backend);
    struct oc_camera list[OC_MAX_CAMERAS];
    if (!camera) return 1;
    br_camera_listen(camera, link_event, NULL);
    int count = br_camera_list(camera, list, (int)OC_MAX_CAMERAS, 15000);
    printf("helper: %d cameras\n", count);
    for (int i = 0; i < count; i++)
      printf("  %-9s %s  up to %ux%u at %u fps, %u mm equivalent, turned %u\n", cam_role_name(list[i].role), list[i].facing == OC_FACING_FRONT ? "front" : "back ",
             list[i].max_width, list[i].max_height, list[i].max_fps, list[i].equiv35_mm, list[i].orientation);
    uint32_t role = cam_role_from_name(which);
    if (!role) { printf("helper: --camera takes a role here (wide, front, ultrawide, tele3, tele5)\n"); br_camera_close(camera); return 2; }
    double started = now(), first = 0;
    int rc = br_camera_start(camera, role, (uint32_t)width, (uint32_t)height, (uint32_t)fps);
    if (rc) { printf("helper: start: %s\n", strerror(-rc)); br_camera_close(camera); return 1; }
    while (s.frames < (uint64_t)frames && !ended && now() - started < 20 + frames / (double)fps * 3) {
      if (!wait_note(&seen, s.frames ? 2000 : 15000)) break;
      if (br_camera_frame(camera, &frame, picture, OC_FRAME_BYTES_MAX)) continue;
      if (!s.frames) first = now() - started;
      count_frame(&s, &frame);
      kept = frame;
    }
    if (ended) printf("helper: the stream ended: %s\n", strerror(ended));
    (void)br_camera_stop(camera);
    rc = report(&s, &kept, picture, out, raw, first);
    br_camera_close(camera);
    return rc;
  }

  static cam_core core;
  core.on_frame = core_frame;
  core.on_error = core_error;
  int rc = cam_core_init(&core, backend);
  if (rc) { printf("%s: no camera list: %s\n", core.backend ? core.backend->name : backend, strerror(-rc)); return 1; }
  printf("back end: %s, %d cameras\n", core.backend->name, core.ncameras);
  if (!strcmp(command, "list")) {
    for (int i = 0; i < core.ncameras; i++) print_camera(i, &core.cameras[i]);
    return core.ncameras ? 0 : 1;
  }
  const cam_info *camera = cam_core_find(&core, which);
  if (!camera) { printf("no camera '%s' (camctl list shows them)\n", which); return 1; }
  // The reader's own mapping of the ring, as xnuhost has it.
  cam_ring_map reader;
  int copy = dup(core.ring.fd);
  if (copy < 0 || (rc = cam_ring_attach(&reader, copy))) { printf("ring: %s\n", strerror(copy < 0 ? errno : -rc)); return 1; }
  uint64_t last = 0;
  double started = now(), first = 0;
  rc = cam_core_start(&core, camera, width, height, fps);
  if (rc) { printf("%s: camera %s did not start: %s\n", core.backend->name, camera->id, strerror(-rc)); return 1; }
  while (s.frames < (uint64_t)frames && !ended && now() - started < 20 + frames / (double)fps * 3) {
    if (!wait_note(&seen, s.frames ? 2000 : 15000)) break;
    if (cam_ring_read(&reader, &last, &frame, picture, OC_FRAME_BYTES_MAX)) continue;
    if (!s.frames) first = now() - started;
    count_frame(&s, &frame);
    kept = frame;
  }
  if (ended) printf("%s: the stream ended: %s\n", core.backend->name, strerror(ended));
  cam_core_stop(&core);
  printf("camera: id %s (%s), asked %dx%d at %d fps, got %dx%d at %d fps\n", camera->id, cam_role_name(camera->role), width, height, fps, core.width, core.height, core.fps);
  rc = report(&s, &kept, picture, out, raw, first);
  cam_ring_close(&reader);
  return rc;
}
