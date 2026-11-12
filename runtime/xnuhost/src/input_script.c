// SPDX-License-Identifier: MIT
// A scripted event source for the virtio-input device: touches and key presses from a text file, for a host
// with no touchscreen (the PC under qemu-aarch64) and for repeatable checks of the guest's input path.
// Setting: input-script=FILE (--input-script FILE). It runs beside the evdev reader, or alone with --no-input.
//
//   # comment
//   abs CODE MIN MAX [RES]   an axis of the device's configuration (what input.c takes from EVIOCGABS on the
//                            phone). Applied when the file is read, before the guest starts: the guest's driver
//                            takes the device as a touch screen only if it has ABS_MT_POSITION_X/Y (0x35, 0x36).
//   ready                    wait until the guest's driver runs the device (DRIVER_OK)
//   wait MS
//   ev TYPE CODE VALUE       one evdev event, as written
//   syn                      ev 0 0 0: the end of a frame; events reach the guest at a frame's end
//   tap X Y [MS]             a finger in slot 0 at X, Y (the axes' own units), lifted MS later (default 80)
//   swipe X0 Y0 X1 Y1 [MS]   a finger in slot 0 moved from one point to the other in MS (default 300), 16 ms a step
//   key CODE [MS]            a key pressed and released MS later (default 100): 116 power, 115 / 114 volume up / down
//   repeat [N]               the steps after `ready` (or all, without one) again: forever, or N more times
//
// Numbers are decimal or 0x hexadecimal. A bad line stops xnuhost's start with its number. Each frame's end (`syn`, and
// the end of every step of tap, swipe and key) is preceded by EV_MSC / MSC_TIMESTAMP with the time it is sent.
#include "xnuhost.h"
#include <ctype.h>
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

enum { EV_SYN_ = 0, EV_KEY_ = 1, EV_ABS_ = 3, BTN_TOUCH_ = 0x14a,
       ABS_MT_SLOT_ = 0x2f, ABS_MT_POSITION_X_ = 0x35, ABS_MT_POSITION_Y_ = 0x36, ABS_MT_TRACKING_ID_ = 0x39 };

static xh_input_step *steps;
static unsigned nsteps;
static xh_virtio *target;
static pthread_t thread;
static volatile bool running;

static bool push(xh_input_step **list, unsigned *n, unsigned *cap, xh_input_step step)
{
  if (*n == *cap) {
    unsigned grown = *cap ? *cap * 2 : 64;
    xh_input_step *more = realloc(*list, grown * sizeof **list);
    if (!more) return false;
    *list = more;
    *cap = grown;
  }
  (*list)[(*n)++] = step;
  return true;
}

static bool event(xh_input_step **list, unsigned *n, unsigned *cap, unsigned type, unsigned code, int32_t value)
{
  return push(list, n, cap, (xh_input_step){ .kind = XH_STEP_EVENT, .type = (uint16_t)type, .code = (uint16_t)code, .value = value });
}

static bool pause_ms(xh_input_step **list, unsigned *n, unsigned *cap, long ms)
{
  return push(list, n, cap, (xh_input_step){ .kind = XH_STEP_WAIT, .value = (int32_t)ms });
}

// Up to `max` numbers after the directive; the count, or -1 for something that is not a number.
static int numbers(char *p, long *out, int max)
{
  int n = 0;
  char *save = NULL;
  for (char *word = strtok_r(p, " \t", &save); word; word = strtok_r(NULL, " \t", &save)) {
    char *end;
    errno = 0;
    long v = strtol(word, &end, 0);
    if (*end || errno || n == max) return -1;
    out[n++] = v;
  }
  return n;
}

int xh_input_script_parse(const char *text, xh_input_step **out, unsigned *count, unsigned *bad_line)
{
  xh_input_step *list = NULL;
  unsigned n = 0, cap = 0, line = 0, tracking = 0;
  char *copy = strdup(text);
  bool ok = copy != NULL;
  for (char *row = copy, *next; ok && row && *row; row = next) {
    next = strchr(row, '\n');
    if (next) *next++ = 0;
    line++;
    char *hash = strchr(row, '#');
    if (hash) *hash = 0;
    while (isspace((unsigned char)*row)) row++;
    size_t length = strlen(row);
    while (length && isspace((unsigned char)row[length - 1])) row[--length] = 0;
    if (!*row) continue;
    char *args = row + strcspn(row, " \t");
    if (*args) *args++ = 0;
    long v[5];
    int c = numbers(args, v, 5);
    if (!strcmp(row, "abs") && (c == 3 || c == 4) && v[0] >= 0 && v[0] < 0x40 && v[1] < v[2]) {
      ok = push(&list, &n, &cap, (xh_input_step){ .kind = XH_STEP_ABS, .code = (uint16_t)v[0], .min = (int32_t)v[1], .max = (int32_t)v[2],
                                                  .value = c == 4 ? (int32_t)v[3] : 0 });
    } else if (!strcmp(row, "ready") && c == 0) {
      ok = push(&list, &n, &cap, (xh_input_step){ .kind = XH_STEP_READY });
    } else if (!strcmp(row, "wait") && c == 1 && v[0] >= 0) {
      ok = pause_ms(&list, &n, &cap, v[0]);
    } else if (!strcmp(row, "ev") && c == 3 && v[0] >= 0 && v[0] <= 0xffff && v[1] >= 0 && v[1] <= 0xffff) {
      ok = event(&list, &n, &cap, (unsigned)v[0], (unsigned)v[1], (int32_t)v[2]);
    } else if (!strcmp(row, "syn") && c == 0) {
      ok = event(&list, &n, &cap, EV_SYN_, 0, 0);
    } else if ((!strcmp(row, "tap") && (c == 2 || c == 3)) || (!strcmp(row, "swipe") && (c == 4 || c == 5))) {
      bool swipe = row[0] == 's';
      long x0 = v[0], y0 = v[1], x1 = swipe ? v[2] : x0, y1 = swipe ? v[3] : y0;
      long ms = swipe ? (c == 5 ? v[4] : 300) : (c == 3 ? v[2] : 80);
      long moves = swipe ? (ms / 16 > 0 ? ms / 16 : 1) : 0;
      ok = ms >= 0 && event(&list, &n, &cap, EV_ABS_, ABS_MT_SLOT_, 0) && event(&list, &n, &cap, EV_ABS_, ABS_MT_TRACKING_ID_, (int32_t)++tracking) &&
           event(&list, &n, &cap, EV_ABS_, ABS_MT_POSITION_X_, (int32_t)x0) && event(&list, &n, &cap, EV_ABS_, ABS_MT_POSITION_Y_, (int32_t)y0) &&
           event(&list, &n, &cap, EV_KEY_, BTN_TOUCH_, 1) && event(&list, &n, &cap, EV_SYN_, 0, 0);
      for (long i = 1; ok && i <= moves; i++)
        ok = pause_ms(&list, &n, &cap, ms / moves) && event(&list, &n, &cap, EV_ABS_, ABS_MT_POSITION_X_, (int32_t)(x0 + (x1 - x0) * i / moves)) &&
             event(&list, &n, &cap, EV_ABS_, ABS_MT_POSITION_Y_, (int32_t)(y0 + (y1 - y0) * i / moves)) && event(&list, &n, &cap, EV_SYN_, 0, 0);
      ok = ok && (swipe || pause_ms(&list, &n, &cap, ms)) && event(&list, &n, &cap, EV_ABS_, ABS_MT_TRACKING_ID_, -1) &&
           event(&list, &n, &cap, EV_KEY_, BTN_TOUCH_, 0) && event(&list, &n, &cap, EV_SYN_, 0, 0);
    } else if (!strcmp(row, "key") && (c == 1 || c == 2) && v[0] > 0 && v[0] <= 0xffff && (c == 1 || v[1] >= 0)) {
      ok = event(&list, &n, &cap, EV_KEY_, (unsigned)v[0], 1) && event(&list, &n, &cap, EV_SYN_, 0, 0) && pause_ms(&list, &n, &cap, c == 2 ? v[1] : 100) &&
           event(&list, &n, &cap, EV_KEY_, (unsigned)v[0], 0) && event(&list, &n, &cap, EV_SYN_, 0, 0);
    } else if (!strcmp(row, "repeat") && (c == 0 || (c == 1 && v[0] > 0))) {
      ok = push(&list, &n, &cap, (xh_input_step){ .kind = XH_STEP_REPEAT, .value = c ? (int32_t)v[0] : -1 });
    } else ok = false;
  }
  free(copy);
  if (!ok) { free(list); if (bad_line) *bad_line = line; return -1; }
  *out = list;
  *count = n;
  return 0;
}

static void sleep_ms(long ms)
{
  for (; ms > 0 && running; ms -= 50) {
    long part = ms < 50 ? ms : 50;
    nanosleep(&(struct timespec){ .tv_sec = 0, .tv_nsec = part * 1000000L }, NULL);
  }
}

static void *script_main(void *arg)
{
  (void)arg;
  unsigned start = 0;
  int32_t left = 0;                            // repeats still to run; 0 before the first `repeat` is met
  bool counting = false;
  for (unsigned i = 0; i < nsteps && running; i++) {
    const xh_input_step *s = &steps[i];
    switch (s->kind) {
    case XH_STEP_ABS: break;                   // applied when the file was read
    case XH_STEP_READY:
      while (running && !xh_virtio_ready(target)) sleep_ms(200);
      if (running) LOGI("input script: the guest's driver runs the input device");
      start = i + 1;
      break;
    case XH_STEP_WAIT: sleep_ms(s->value); break;
    case XH_STEP_EVENT:
      // A frame's end carries its time, as input.c's touch frames do (EV_MSC / MSC_TIMESTAMP, the counter's low bits):
      // the guest counts its latency from here.
      if (s->type == EV_SYN_ && s->code == 0) xh_virtio_input_event(target, 4, 5, (int32_t)(uint32_t)xh_counter());
      xh_virtio_input_event(target, s->type, s->code, s->value);
      break;
    case XH_STEP_REPEAT:
      if (s->value < 0) { i = start - 1; break; }
      if (!counting) { counting = true; left = s->value; }
      if (left > 0) { left--; i = start - 1; } else counting = false;
      break;
    }
  }
  if (running) LOGI("input script: done");
  return NULL;
}

int xh_input_script_start(xh_virtio *dev, const char *path)
{
  if (!path || !*path) return 0;
  FILE *f = fopen(path, "r");
  if (!f) { LOGE("input script %s: %s", path, strerror(errno)); return -1; }
  static char text[65536];
  size_t n = fread(text, 1, sizeof text - 1, f);
  bool whole = feof(f);
  fclose(f);
  text[n] = 0;
  unsigned bad = 0;
  if (!whole || xh_input_script_parse(text, &steps, &nsteps, &bad)) {
    LOGE("input script %s: %s", path, whole ? "bad line" : "longer than 64 KiB");
    if (whole) LOGE("input script %s: line %u", path, bad);
    return -1;
  }
  unsigned events = 0;
  for (unsigned i = 0; i < nsteps; i++) {
    if (steps[i].kind == XH_STEP_ABS) xh_virtio_input_set_abs(dev, steps[i].code, steps[i].min, steps[i].max, steps[i].value);
    else if (steps[i].kind == XH_STEP_EVENT) events++;
  }
  target = dev;
  running = true;
  if (pthread_create(&thread, NULL, script_main, NULL)) { running = false; return -1; }
  LOGI("input script: %s, %u steps (%u events)", path, nsteps, events);
  return 0;
}

void xh_input_script_stop(void)
{
  if (!running) return;
  running = false;
  pthread_join(thread, NULL);
}
