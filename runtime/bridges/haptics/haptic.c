// SPDX-License-Identifier: MIT
// Haptics bridge: iOS vibration requests onto the Galaxy S24 Ultra's actuator.
//
// The actuator is an LRA driven by a Cirrus CS40L26A (I2C 0x40) under Samsung's sec_vibrator_inputff:
// /dev/input/event8 "sec_vibrator_inputff", EV_FF bits FF_PERIODIC, FF_SINE, FF_CUSTOM, FF_GAIN and
// no FF_RUMBLE (read on the owner's phone; docs/hardware.md). The driver's FF_SINE path programs the
// chip's buzz generator: period 4..10 ms (100..250 Hz), magnitude 0..255, length in ms
// (cs40l26_sine_upload, drivers/vibrator/cs/cs40l26/cs40l26.c in Samsung's kernel source). FF_GAIN
// takes 0..10000 (Samsung's intensity scale, gain / 100 = percent).
//
// One effect is kept and updated for every request. A worker thread does the ioctls (each is I2C
// traffic), so a request from a vCPU never waits; the newest request wins.
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#ifdef __linux__
#include <linux/input.h>
#include <sys/ioctl.h>
#endif

#define PERIOD_MIN_MS 4u        // CS40L26_BUZZGEN_PER_MIN
#define PERIOD_MAX_MS 10u       // CS40L26_BUZZGEN_PER_MAX
#define RESONANCE_MS  6u        // 166 Hz: a guess at the LRA's resonance until f0 is read on the phone
                                // (attribute f0_measured of the class sec_vib_inputff); whole milliseconds only
#define LEVEL_MAX     255u      // CS40L26_BUZZGEN_LEVEL_MAX
#define LEVEL_FLOOR   40u       // below this an LRA does not move enough to feel
#define LENGTH_MAX_MS 10000u

void br_haptic_map(unsigned intensity, unsigned frequency_hz, unsigned *period_ms, unsigned *magnitude)
{
  if (intensity > 1000) intensity = 1000;
  unsigned period = frequency_hz ? (1000 + frequency_hz / 2) / frequency_hz : RESONANCE_MS;
  if (period < PERIOD_MIN_MS) period = PERIOD_MIN_MS;
  if (period > PERIOD_MAX_MS) period = PERIOD_MAX_MS;
  *period_ms = period;
  // Any intensity above zero is felt: the floor up to full scale, linear.
  *magnitude = intensity ? LEVEL_FLOOR + (LEVEL_MAX - LEVEL_FLOOR) * intensity / 1000 : 0;
}

struct br_haptic {
  const br_haptic_backend *backend;
  void *ctx;
  pthread_t thread;
  pthread_mutex_t lock;
  pthread_cond_t wake, idle;
  bool quit, pending, busy;
  uint32_t on_ms;
  unsigned intensity, frequency_hz;
  int effect;                 // -1 until the first upload
  bool playing;
  unsigned failures;
};

static void *worker(void *arg)
{
  br_haptic *h = arg;
  pthread_mutex_lock(&h->lock);
  for (;;) {
    while (!h->pending && !h->quit) { h->busy = false; pthread_cond_broadcast(&h->idle); pthread_cond_wait(&h->wake, &h->lock); }
    if (h->quit && !h->pending) break;
    uint32_t on_ms = h->on_ms;
    unsigned intensity = h->intensity, frequency = h->frequency_hz;
    h->pending = false;
    h->busy = true;
    pthread_mutex_unlock(&h->lock);

    unsigned period, magnitude;
    br_haptic_map(intensity, frequency, &period, &magnitude);
    int rc = 0;
    if (!on_ms || !magnitude) {
      if (h->playing && h->effect >= 0) rc = h->backend->play(h->ctx, h->effect, false);
      h->playing = false;
    } else {
      int id = h->backend->upload(h->ctx, h->effect, on_ms > LENGTH_MAX_MS ? LENGTH_MAX_MS : on_ms, period, magnitude);
      if (id < 0) rc = id;
      else {
        h->effect = id;
        rc = h->backend->play(h->ctx, id, true);
        h->playing = rc == 0;
      }
    }
    if (rc < 0 && h->failures++ < 8) br_log(0, "haptics: request (%u ms, intensity %u) failed: %s", on_ms, intensity, strerror(-rc));
    pthread_mutex_lock(&h->lock);
  }
  h->busy = false;
  pthread_cond_broadcast(&h->idle);
  pthread_mutex_unlock(&h->lock);
  return NULL;
}

br_haptic *br_haptic_open_backend(const br_haptic_backend *backend, void *ctx)
{
  br_haptic *h = calloc(1, sizeof *h);
  if (!h) return NULL;
  h->backend = backend;
  h->ctx = ctx;
  h->effect = -1;
  pthread_mutex_init(&h->lock, NULL);
  pthread_cond_init(&h->wake, NULL);
  pthread_cond_init(&h->idle, NULL);
  if (backend->gain) (void)backend->gain(ctx, 10000);
  if (pthread_create(&h->thread, NULL, worker, h)) { free(h); return NULL; }
  return h;
}

void br_haptic_vib(br_haptic *h, uint32_t on_ms, unsigned intensity, unsigned frequency_hz)
{
  if (!h) return;
  pthread_mutex_lock(&h->lock);
  h->on_ms = on_ms; h->intensity = intensity; h->frequency_hz = frequency_hz;
  h->pending = true;
  pthread_cond_signal(&h->wake);
  pthread_mutex_unlock(&h->lock);
}

void br_haptic_stop(br_haptic *h) { br_haptic_vib(h, 0, 0, 0); }

void br_haptic_flush(br_haptic *h)
{
  if (!h) return;
  pthread_mutex_lock(&h->lock);
  while (h->pending || h->busy) pthread_cond_wait(&h->idle, &h->lock);
  pthread_mutex_unlock(&h->lock);
}

void br_haptic_close(br_haptic *h)
{
  if (!h) return;
  br_haptic_stop(h);                       // the motor never outlives the bridge
  pthread_mutex_lock(&h->lock);
  h->quit = true;
  pthread_cond_signal(&h->wake);
  pthread_mutex_unlock(&h->lock);
  pthread_join(h->thread, NULL);
  if (h->backend->close) h->backend->close(h->ctx);
  free(h);
}

// ---- the evdev force-feedback back end ----------------------------------------------------------
#ifdef __linux__
#define BIT_AT(bits, n) ((bits)[(n) / 8] >> ((n) % 8) & 1)

static bool has_periodic(int fd)
{
  unsigned char bits[FF_MAX / 8 + 1] = { 0 };
  if (ioctl(fd, EVIOCGBIT(EV_FF, sizeof bits), bits) < 0) return false;
  return BIT_AT(bits, FF_PERIODIC) && BIT_AT(bits, FF_SINE);
}

int br_haptic_find(const char *dev_dir, char *path, size_t size)
{
  if (!dev_dir) dev_dir = "/dev/input";
  DIR *d = opendir(dev_dir);
  if (!d) return -1;
  int found = -1;
  char best[300] = "";
  struct dirent *e;
  while ((e = readdir(d))) {
    if (strncmp(e->d_name, "event", 5)) continue;
    char node[300], name[80] = "";
    snprintf(node, sizeof node, "%s/%s", dev_dir, e->d_name);
    int fd = open(node, O_RDWR | O_CLOEXEC);
    if (fd < 0) continue;
    if (has_periodic(fd)) {
      (void)ioctl(fd, EVIOCGNAME(sizeof name), name);
      // Samsung's wrapper first; any other periodic device otherwise.
      if (!strcmp(name, "sec_vibrator_inputff") || found < 0) { snprintf(best, sizeof best, "%s", node); found = 0; }
      if (!strcmp(name, "sec_vibrator_inputff")) { close(fd); break; }
    }
    close(fd);
  }
  closedir(d);
  if (found == 0) snprintf(path, size, "%s", best);
  return found;
}

static int ff_upload(void *ctx, int id, unsigned length_ms, unsigned period_ms, unsigned magnitude)
{
  int fd = (int)(intptr_t)ctx;
  struct ff_effect effect;
  memset(&effect, 0, sizeof effect);
  effect.type = FF_PERIODIC;
  effect.id = (short)id;
  effect.replay.length = (unsigned short)length_ms;
  effect.u.periodic.waveform = FF_SINE;
  effect.u.periodic.period = (unsigned short)period_ms;
  effect.u.periodic.magnitude = (short)magnitude;
  if (ioctl(fd, EVIOCSFF, &effect) < 0) return -errno;
  return effect.id;
}

static int ff_event(int fd, unsigned code, int value)
{
  struct input_event event;
  memset(&event, 0, sizeof event);
  event.type = EV_FF;
  event.code = (unsigned short)code;
  event.value = value;
  return write(fd, &event, sizeof event) == (ssize_t)sizeof event ? 0 : -errno;
}

static int ff_play(void *ctx, int id, bool on) { return ff_event((int)(intptr_t)ctx, (unsigned)id, on); }
static int ff_gain(void *ctx, unsigned gain) { return ff_event((int)(intptr_t)ctx, FF_GAIN, (int)gain); }
static void ff_close(void *ctx) { close((int)(intptr_t)ctx); }      // closing erases the file's effects

static const br_haptic_backend evdev_backend = { .upload = ff_upload, .play = ff_play, .gain = ff_gain, .close = ff_close };

br_haptic *br_haptic_open(const char *dev_dir)
{
  char node[300];
  if (br_haptic_find(dev_dir, node, sizeof node)) { br_log(0, "haptics: off: no force-feedback device with FF_PERIODIC + FF_SINE under %s", dev_dir ? dev_dir : "/dev/input"); return NULL; }
  int fd = open(node, O_RDWR | O_CLOEXEC);
  if (fd < 0) { br_log(0, "haptics: off: %s: %s", node, strerror(errno)); return NULL; }
  br_haptic *h = br_haptic_open_backend(&evdev_backend, (void *)(intptr_t)fd);
  if (!h) { close(fd); return NULL; }
  br_log(1, "haptics: on: %s (sine effects, period %u..%u ms, level %u..%u)", node, PERIOD_MIN_MS, PERIOD_MAX_MS, LEVEL_FLOOR, LEVEL_MAX);
  return h;
}
#else
int br_haptic_find(const char *dev_dir, char *path, size_t size) { (void)dev_dir; (void)path; (void)size; return -1; }
br_haptic *br_haptic_open(const char *dev_dir) { (void)dev_dir; return NULL; }
#endif
