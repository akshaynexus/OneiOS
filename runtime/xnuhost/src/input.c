// SPDX-License-Identifier: MIT
// Touch and the side keys, read straight from /dev/input/event* as OneOS's root session leaves
// them: zygote and system_server are gone, so there is no InputDispatcher and nothing else reads
// the touchscreen. The devices are not grabbed (EVIOCGRAB): the switch's own Volume Up + Volume
// Down watcher reads the key device beside us (oneos/interface.md, section 6).
#define _GNU_SOURCE
#include "xnuhost.h"
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#if defined(__linux__)
#include <linux/input.h>
#include <sys/ioctl.h>

#define MAX_DEVICES 8
static struct pollfd fds[MAX_DEVICES];
static bool is_touch[MAX_DEVICES];
static unsigned nfds;
static xh_virtio *target;
static pthread_t thread;
static volatile bool running;

static bool bit(const unsigned long *bits, unsigned n) { return bits[n / (8 * sizeof(long))] >> (n % (8 * sizeof(long))) & 1; }

static void *input_main(void *arg)
{
  (void)arg;
  while (running) {
    if (poll(fds, nfds, 500) <= 0) continue;
    for (unsigned i = 0; i < nfds; i++) {
      if (!(fds[i].revents & POLLIN)) continue;
      struct input_event ev[64];
      ssize_t n = read(fds[i].fd, ev, sizeof ev);
      for (ssize_t e = 0; e < n / (ssize_t)sizeof *ev; e++) {
        const struct input_event *v = &ev[e];
        if (v->type == EV_SYN) { if (v->code == SYN_REPORT) xh_virtio_input_event(target, EV_SYN, SYN_REPORT, 0); }
        else if (v->type == EV_KEY) {
          if (v->code == KEY_VOLUMEDOWN || v->code == KEY_VOLUMEUP || v->code == KEY_POWER || (is_touch[i] && v->code == BTN_TOUCH))
            xh_virtio_input_event(target, EV_KEY, v->code, v->value);
        } else if (v->type == EV_ABS && is_touch[i]) {
          if (v->code == ABS_MT_SLOT || v->code == ABS_MT_TRACKING_ID || v->code == ABS_MT_POSITION_X || v->code == ABS_MT_POSITION_Y ||
              v->code == ABS_MT_PRESSURE || v->code == ABS_MT_TOUCH_MAJOR)
            xh_virtio_input_event(target, EV_ABS, v->code, v->value);
        }
      }
    }
  }
  return NULL;
}

int xh_input_start(xh_virtio *dev, unsigned width, unsigned height)
{
  (void)width; (void)height;
  target = dev;
  for (unsigned i = 0; i < 32 && nfds < MAX_DEVICES; i++) {
    char path[32], name[64] = "";
    snprintf(path, sizeof path, "/dev/input/event%u", i);
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NONBLOCK);
    if (fd < 0) continue;
    unsigned long abs[(ABS_MAX + 8 * sizeof(long)) / (8 * sizeof(long))] = { 0 }, key[(KEY_MAX + 8 * sizeof(long)) / (8 * sizeof(long))] = { 0 };
    ioctl(fd, EVIOCGBIT(EV_ABS, sizeof abs), abs);
    ioctl(fd, EVIOCGBIT(EV_KEY, sizeof key), key);
    ioctl(fd, EVIOCGNAME(sizeof name - 1), name);
    bool touch = bit(abs, ABS_MT_POSITION_X) && bit(abs, ABS_MT_POSITION_Y);
    bool keys = bit(key, KEY_VOLUMEDOWN) || bit(key, KEY_VOLUMEUP) || bit(key, KEY_POWER);
    if (!touch && !keys) { close(fd); continue; }
    if (touch) {
      // The guest gets the panel's own coordinate ranges and scales them to its frame.
      static const unsigned axes[] = { ABS_MT_SLOT, ABS_MT_TRACKING_ID, ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_PRESSURE, ABS_MT_TOUCH_MAJOR };
      for (unsigned a = 0; a < sizeof axes / sizeof *axes; a++) {
        struct input_absinfo info;
        if (bit(abs, axes[a]) && ioctl(fd, EVIOCGABS(axes[a]), &info) == 0) xh_virtio_input_set_abs(dev, axes[a], info.minimum, info.maximum, info.resolution);
      }
    }
    LOGI("input: %s \"%s\"%s%s", path, name, touch ? " touch" : "", keys ? " keys" : "");
    is_touch[nfds] = touch;
    fds[nfds++] = (struct pollfd){ .fd = fd, .events = POLLIN };
  }
  if (!nfds) { LOGI("input: no touchscreen or key device under /dev/input"); return 0; }
  running = true;
  return pthread_create(&thread, NULL, input_main, NULL) ? -1 : 0;
}

void xh_input_stop(void)
{
  if (!running) return;
  running = false;
  pthread_join(thread, NULL);
  for (unsigned i = 0; i < nfds; i++) close(fds[i].fd);
}
#else
int xh_input_start(xh_virtio *dev, unsigned width, unsigned height) { (void)dev; (void)width; (void)height; return 0; }
void xh_input_stop(void) {}
#endif
