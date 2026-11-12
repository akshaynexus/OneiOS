// SPDX-License-Identifier: MIT
// Touch and the side keys, read straight from /dev/input/event* as OneOS's root session leaves
// them: zygote and system_server are gone, so there is no InputDispatcher and nothing else reads
// the touchscreen. The devices are not grabbed (EVIOCGRAB): the switch's own Volume Up + Volume
// Down watcher reads the key device beside us (oneos/interface.md, section 6).
//
// Each touch frame goes to the guest with its scan time: an EV_MSC / MSC_TIMESTAMP event before the
// SYN_REPORT whose value is the low 32 bits of the time the kernel stamped the frame with, in counter
// ticks (CNTVCT_EL0, the guest's mach_absolute_time; 19.2 MHz on the S24 Ultra, so the value wraps
// every 223 s). The guest's transport uses it as the HID report's time and for its latency counts
// (kernel/patches/oneios-hosted/hid/README.md, "Latency"). A kernel without that code ignores it.
//
// SYN_DROPPED (the kernel's buffer for this reader overflowed: the input thread did not run for about
// 8 frames) is handled as evdev's documentation says: the events up to the next SYN_REPORT are
// dropped and the device's state is read again (EVIOCGMTSLOTS, EVIOCGKEY). The difference from what
// the guest was last sent goes to it as frames of their own: a lift lost in the drop would otherwise
// leave a finger down in iOS until the next contact in that slot.
//
// While fingers are down the reader counts what it sees and logs one line when the last finger lifts
// (input: touch: ...): the frames and their spacing (the touch IC's report rate), how long after the
// scan the reader got each frame, and, for a touch on a still screen, how long until the guest showed
// its next frame (xh_input_frame_shown, called from the display).
#define _GNU_SOURCE
#include "xnuhost.h"
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

// ---- the filter of one device's events (portable: tests/test_xnuhost.c) ----------------------------
// Linux's event codes (linux/input-event-codes.h), spelled out so that this part builds on any host.
enum { EV_SYN_ = 0, EV_KEY_ = 1, EV_ABS_ = 3, EV_MSC_ = 4, SYN_REPORT_ = 0, SYN_DROPPED_ = 3, MSC_TIMESTAMP_ = 5,
       KEY_VOLUMEDOWN_ = 114, KEY_VOLUMEUP_ = 115, KEY_POWER_ = 116, BTN_TOUCH_ = 0x14a,
       ABS_MT_SLOT_ = 0x2f, ABS_MT_TOUCH_MAJOR_ = 0x30, ABS_MT_POSITION_X_ = 0x35, ABS_MT_POSITION_Y_ = 0x36,
       ABS_MT_TRACKING_ID_ = 0x39, ABS_MT_PRESSURE_ = 0x3a };
static const uint16_t side_keys[3] = { KEY_VOLUMEDOWN_, KEY_VOLUMEUP_, KEY_POWER_ };

static int64_t last_shown_ns;         // the guest's last frame (CLOCK_MONOTONIC), 0 = none yet
static int64_t down_ns;               // a touch-down on a still screen that waits for its frame, 0 = none
static int64_t down_frame_ns = -1;       // the wait of the last such touch-down, -1 = none measured
static int64_t monotonic_ns(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return (int64_t)t.tv_sec * 1000000000 + t.tv_nsec; }

// The screen counts as still when the guest showed no frame for this long before a touch-down.
#define STILL_NS 100000000

void xh_input_frame_shown(void)
{
  int64_t now = monotonic_ns(), down = __atomic_load_n(&down_ns, __ATOMIC_ACQUIRE);
  __atomic_store_n(&last_shown_ns, now, __ATOMIC_RELEASE);
  if (down && now >= down && __atomic_compare_exchange_n(&down_ns, &down, 0, false, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE))
    __atomic_store_n(&down_frame_ns, now - down, __ATOMIC_RELEASE);
}

static void send_event(xh_input_source *s, uint16_t type, uint16_t code, int32_t value)
{
  if (type == EV_ABS_ && code == ABS_MT_SLOT_) s->slot = value >= 0 && value < XH_INPUT_SLOTS ? (unsigned)value : XH_INPUT_SLOTS;
  else if (type == EV_ABS_ && code == ABS_MT_TRACKING_ID_ && s->slot < XH_INPUT_SLOTS) {
    if (value >= 0 && s->sent_id[s->slot] < 0) s->began++;
    s->sent_id[s->slot] = value < 0 ? -1 : value;
  } else if (type == EV_KEY_) {
    for (unsigned k = 0; k < 3; k++)
      if (code == side_keys[k]) s->sent_keys = value ? s->sent_keys | 1u << k : s->sent_keys & ~(1u << k);
  }
  xh_virtio_input_event(s->dev, type, code, value);
}

static unsigned contacts(const xh_input_source *s)
{
  unsigned n = 0;
  for (unsigned i = 0; i < XH_INPUT_SLOTS; i++) n += s->sent_id[i] >= 0;
  return n;
}

// The frame's scan time as the guest's counter: the counter now, less the time since the scan.
static uint32_t scan_ticks(int64_t scan_ns, int64_t now_ns, uint64_t now_counter, uint64_t hz)
{
  uint64_t ago = now_ns > scan_ns ? (uint64_t)(now_ns - scan_ns) : 0;
  uint64_t ticks = ago / 1000000000ull * hz + ago % 1000000000ull * hz / 1000000000ull;
  return (uint32_t)(now_counter - ticks);
}

static void log_gesture(xh_input_source *s)
{
  xh_input_timing *t = &s->timing;
  if (!t->frames) return;
  int64_t frame = __atomic_exchange_n(&down_frame_ns, -1, __ATOMIC_ACQ_REL);
  __atomic_store_n(&down_ns, 0, __ATOMIC_RELEASE);
  char shown[80] = "", held[64] = "";
  if (t->still && frame >= 0) snprintf(shown, sizeof shown, "; the guest's next frame %lld.%lld ms after the touch-down", (long long)(frame / 1000000), (long long)(frame / 100000 % 10));
  else if (t->still) snprintf(shown, sizeof shown, "; no guest frame after the touch-down");
  if (t->held) snprintf(held, sizeof held, "; %u frame%s waited for the guest's buffers", t->held, t->held == 1 ? "" : "s");
  int64_t span = t->last_scan_ns - t->first_scan_ns;
  LOGI("input: touch: %u frame%s in %lld ms, %u contact%s, a frame every %lld.%lld..%lld.%lld ms; read %lld/%lld/%lld us after the scan (min/avg/max)%s%s",
       t->frames, t->frames == 1 ? "" : "s", (long long)(span / 1000000), t->began, t->began == 1 ? "" : "s",
       (long long)(t->gap_min_ns / 1000000), (long long)(t->gap_min_ns / 100000 % 10), (long long)(t->gap_max_ns / 1000000), (long long)(t->gap_max_ns / 100000 % 10),
       (long long)(t->read_min_ns / 1000), (long long)(t->read_sum_ns / t->frames / 1000), (long long)(t->read_max_ns / 1000), held, shown);
  memset(t, 0, sizeof *t);
}

// One touch frame was sent: its timing.
static void count_frame(xh_input_source *s, int64_t scan_ns, int64_t now_ns)
{
  xh_input_timing *t = &s->timing;
  if (!t->frames && !contacts(s)) { s->frame_began = s->began; return; }     // no gesture: nothing was down nor is
  int64_t read = now_ns > scan_ns ? now_ns - scan_ns : 0;
  if (!t->frames) {
    t->first_scan_ns = scan_ns;
    t->read_min_ns = read;
    t->gap_min_ns = 0;
    // A touch-down on a still screen: the display's next frame is the guest's answer to it.
    int64_t shown = __atomic_load_n(&last_shown_ns, __ATOMIC_ACQUIRE);
    t->still = shown && scan_ns - shown >= STILL_NS;
    __atomic_store_n(&down_frame_ns, -1, __ATOMIC_RELEASE);
    __atomic_store_n(&down_ns, t->still ? scan_ns : 0, __ATOMIC_RELEASE);
  } else {
    int64_t gap = scan_ns - t->last_scan_ns;
    if (t->frames == 1 || gap < t->gap_min_ns) t->gap_min_ns = gap;
    if (gap > t->gap_max_ns) t->gap_max_ns = gap;
  }
  t->last_scan_ns = scan_ns;
  t->frames++;
  t->began += s->began - s->frame_began;
  s->frame_began = s->began;
  t->read_sum_ns += read;
  if (read < t->read_min_ns) t->read_min_ns = read;
  if (read > t->read_max_ns) t->read_max_ns = read;
  if (xh_virtio_input_backlog(s->dev)) t->held++;
  if (!contacts(s)) log_gesture(s);
}

// After SYN_DROPPED: what the device holds now, against what the guest was sent.
static void resync(xh_input_source *s, int64_t now_ns, uint64_t now_counter, uint64_t hz)
{
  xh_input_state st;
  s->drops++;
  if (!s->read_state || s->read_state(s, &st)) {
    LOGI("input: %s: events dropped by the kernel (the reader was too slow); the device's state could not be read again", s->name);
    return;
  }
  if (s->touch) {
    // Lifts first, in a frame of their own: a slot whose contact ended, or holds another contact now.
    bool lifted = false;
    for (unsigned i = 0; i < XH_INPUT_SLOTS; i++) {
      if (s->sent_id[i] < 0 || st.id[i] == s->sent_id[i]) continue;
      send_event(s, EV_ABS_, ABS_MT_SLOT_, (int32_t)i);
      send_event(s, EV_ABS_, ABS_MT_TRACKING_ID_, -1);
      lifted = true;
    }
    if (lifted) {
      send_event(s, EV_KEY_, BTN_TOUCH_, contacts(s) > 0);
      send_event(s, EV_MSC_, MSC_TIMESTAMP_, (int32_t)scan_ticks(now_ns, now_ns, now_counter, hz));
      send_event(s, EV_SYN_, SYN_REPORT_, 0);
      count_frame(s, now_ns, now_ns);
    }
    for (unsigned i = 0; i < XH_INPUT_SLOTS; i++) {
      if (st.id[i] < 0) continue;
      send_event(s, EV_ABS_, ABS_MT_SLOT_, (int32_t)i);
      if (s->sent_id[i] != st.id[i]) send_event(s, EV_ABS_, ABS_MT_TRACKING_ID_, st.id[i]);
      send_event(s, EV_ABS_, ABS_MT_POSITION_X_, st.x[i]);
      send_event(s, EV_ABS_, ABS_MT_POSITION_Y_, st.y[i]);
    }
    send_event(s, EV_ABS_, ABS_MT_SLOT_, (int32_t)st.slot);   // the device's current slot: its next events may not name it
    send_event(s, EV_KEY_, BTN_TOUCH_, contacts(s) > 0);
  }
  for (unsigned k = 0; k < 3; k++)
    if ((st.keys ^ s->sent_keys) & 1u << k) send_event(s, EV_KEY_, side_keys[k], (st.keys >> k) & 1);
  if (s->touch) send_event(s, EV_MSC_, MSC_TIMESTAMP_, (int32_t)scan_ticks(now_ns, now_ns, now_counter, hz));
  send_event(s, EV_SYN_, SYN_REPORT_, 0);
  if (s->touch) count_frame(s, now_ns, now_ns);
  LOGI("input: %s: events dropped by the kernel (the reader was too slow, %u time%s); the state was read again: %u contact%s down", s->name,
       s->drops, s->drops == 1 ? "" : "s", contacts(s), contacts(s) == 1 ? "" : "s");
}

void xh_input_feed(xh_input_source *s, const xh_input_ev *ev, unsigned n, int64_t now_ns, uint64_t now_counter, uint64_t hz)
{
  for (unsigned e = 0; e < n; e++) {
    const xh_input_ev *v = &ev[e];
    if (s->dropping) {
      if (v->type == EV_SYN_ && v->code == SYN_REPORT_) { s->dropping = false; resync(s, now_ns, now_counter, hz); }
      continue;
    }
    if (v->type == EV_SYN_) {
      if (v->code == SYN_DROPPED_) { s->dropping = true; continue; }
      if (v->code != SYN_REPORT_) continue;
      if (s->touch) send_event(s, EV_MSC_, MSC_TIMESTAMP_, (int32_t)scan_ticks(v->time_ns, now_ns, now_counter, hz));
      send_event(s, EV_SYN_, SYN_REPORT_, 0);
      if (s->touch) count_frame(s, v->time_ns, now_ns);
    } else if (v->type == EV_KEY_) {
      bool side = v->code == KEY_VOLUMEDOWN_ || v->code == KEY_VOLUMEUP_ || v->code == KEY_POWER_;
      if (side || (s->touch && v->code == BTN_TOUCH_)) send_event(s, EV_KEY_, v->code, v->value);
    } else if (v->type == EV_ABS_ && s->touch) {
      if (v->code == ABS_MT_SLOT_ || v->code == ABS_MT_TRACKING_ID_ || v->code == ABS_MT_POSITION_X_ || v->code == ABS_MT_POSITION_Y_ ||
          v->code == ABS_MT_PRESSURE_ || v->code == ABS_MT_TOUCH_MAJOR_)
        send_event(s, EV_ABS_, v->code, v->value);
    }
  }
}

void xh_input_source_init(xh_input_source *s, xh_virtio *dev, const char *name, bool touch)
{
  memset(s, 0, sizeof *s);
  s->dev = dev;
  s->touch = touch;
  s->fd = -1;
  snprintf(s->name, sizeof s->name, "%s", name);
  for (unsigned i = 0; i < XH_INPUT_SLOTS; i++) s->sent_id[i] = -1;
}

#if defined(__linux__)
#include <linux/input.h>
#include <sys/ioctl.h>

#define MAX_DEVICES 8
static struct pollfd fds[MAX_DEVICES];
static xh_input_source sources[MAX_DEVICES];
static unsigned nfds;
static pthread_t thread;
static volatile bool running;

static bool bit(const unsigned long *bits, unsigned n) { return bits[n / (8 * sizeof(long))] >> (n % (8 * sizeof(long))) & 1; }

// The device's state now, by ioctl (after SYN_DROPPED).
static int read_state(xh_input_source *s, xh_input_state *st)
{
  memset(st, 0, sizeof *st);
  for (unsigned i = 0; i < XH_INPUT_SLOTS; i++) st->id[i] = -1;
  unsigned long key[(KEY_MAX + 8 * sizeof(long)) / (8 * sizeof(long))] = { 0 };
  if (ioctl(s->fd, EVIOCGKEY(sizeof key), key) < 0) return -1;
  for (unsigned k = 0; k < 3; k++) if (bit(key, side_keys[k])) st->keys |= 1u << k;
  if (!s->touch) return 0;
  struct { uint32_t code; int32_t values[XH_INPUT_SLOTS]; } slots;
  static const unsigned codes[3] = { ABS_MT_TRACKING_ID, ABS_MT_POSITION_X, ABS_MT_POSITION_Y };
  int32_t *into[3] = { st->id, st->x, st->y };
  for (unsigned c = 0; c < 3; c++) {
    memset(&slots, 0, sizeof slots);
    slots.code = codes[c];
    if (ioctl(s->fd, EVIOCGMTSLOTS(sizeof slots), &slots) < 0) return -1;
    memcpy(into[c], slots.values, sizeof slots.values);
  }
  struct input_absinfo info;
  if (ioctl(s->fd, EVIOCGABS(ABS_MT_SLOT), &info) < 0) return -1;
  st->slot = info.value >= 0 && info.value < XH_INPUT_SLOTS ? (unsigned)info.value : 0;
  for (int i = info.maximum + 1; i >= 0 && i < XH_INPUT_SLOTS; i++) st->id[i] = -1;    // past the device's slots: none
  return 0;
}

static void *input_main(void *arg)
{
  (void)arg;
  while (running) {
    if (poll(fds, nfds, 500) <= 0) continue;
    for (unsigned i = 0; i < nfds; i++) {
      if (!(fds[i].revents & POLLIN)) continue;
      struct input_event ev[64];
      ssize_t n = read(fds[i].fd, ev, sizeof ev);
      if (n <= 0) continue;
      int64_t now_ns = monotonic_ns();
      uint64_t now_counter = xh_counter();
      xh_input_ev batch[64];
      unsigned count = (unsigned)(n / (ssize_t)sizeof *ev);
      for (unsigned e = 0; e < count; e++)
        batch[e] = (xh_input_ev){ (int64_t)ev[e].input_event_sec * 1000000000 + (int64_t)ev[e].input_event_usec * 1000, ev[e].type, ev[e].code, ev[e].value };
      xh_input_feed(&sources[i], batch, count, now_ns, now_counter, g_info.timebase_hz);
    }
  }
  return NULL;
}

int xh_input_start(xh_virtio *dev, unsigned width, unsigned height)
{
  (void)width; (void)height;
  bool have_touch = false;
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
    // One touchscreen: the multitouch device that is the screen itself (INPUT_PROP_DIRECT). The S24 Ultra also has
    // "sec_touchpad", a multitouch pointer device with the same axes: its events went into the same guest device as
    // the screen's (one slot state for two sources) and its ranges replaced the screen's. Not taken.
    if (touch) {
      unsigned long props[(INPUT_PROP_MAX + 8 * sizeof(long)) / (8 * sizeof(long))] = { 0 };
      bool direct = ioctl(fd, EVIOCGPROP(sizeof props), props) >= 0 && bit(props, INPUT_PROP_DIRECT);
      if (!direct || have_touch) {
        LOGI("input: %s \"%s\": multitouch, not taken (%s)", path, name, direct ? "a touchscreen is already taken" : "not the screen: no INPUT_PROP_DIRECT");
        touch = false;
      }
    }
    if (!touch && !keys) { close(fd); continue; }
    // Event times on CLOCK_MONOTONIC, for this reader only (the clock is a property of the open file, not of the
    // device): the scan times the guest gets are taken from them.
    int clock = CLOCK_MONOTONIC;
    bool monotonic = ioctl(fd, EVIOCSCLOCKID, &clock) == 0;
    char ranges[160] = "";
    if (touch) {
      have_touch = true;
      // The guest gets the panel's own coordinate ranges and scales them to its frame.
      static const unsigned axes[] = { ABS_MT_SLOT, ABS_MT_TRACKING_ID, ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_PRESSURE, ABS_MT_TOUCH_MAJOR };
      struct input_absinfo x = { 0 }, y = { 0 }, slots = { 0 }, major = { 0 }, pressure = { 0 };
      for (unsigned a = 0; a < sizeof axes / sizeof *axes; a++) {
        struct input_absinfo info;
        if (!bit(abs, axes[a]) || ioctl(fd, EVIOCGABS(axes[a]), &info)) continue;
        xh_virtio_input_set_abs(dev, axes[a], info.minimum, info.maximum, info.resolution);
        if (axes[a] == ABS_MT_POSITION_X) x = info;
        if (axes[a] == ABS_MT_POSITION_Y) y = info;
        if (axes[a] == ABS_MT_SLOT) slots = info;
        if (axes[a] == ABS_MT_TOUCH_MAJOR) major = info;
        if (axes[a] == ABS_MT_PRESSURE) pressure = info;
      }
      snprintf(ranges, sizeof ranges, " (x %d..%d, y %d..%d, %d slots, major 0..%d, pressure 0..%d%s)", x.minimum, x.maximum, y.minimum,
               y.maximum, slots.maximum + 1, major.maximum, pressure.maximum, monotonic ? "" : "; event times not on CLOCK_MONOTONIC");
    }
    LOGI("input: %s \"%s\"%s%s%s", path, name, touch ? " touch" : "", ranges, keys ? " keys" : "");
    xh_input_source_init(&sources[nfds], dev, touch ? "touch" : name, touch);
    sources[nfds].fd = fd;
    sources[nfds].read_state = read_state;
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
