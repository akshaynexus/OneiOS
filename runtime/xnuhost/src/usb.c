// SPDX-License-Identifier: MIT
// The phone's USB gadget in xnuhost's log (docs/booter.md 2.9a): a line at the start and on every change,
//   usb: state <sys.usb.state>, config <sys.usb.config>, UDC <sys.usb.controller> <its state>
// so that a session's log says when adb's USB function, or the controller under it, went away (phone, 2026-11-13:
// a cable plugged in during an iOS session, and the Mac saw no USB device at all). Read only: three properties of
// Android's property area, read as bionic does (__system_property_find once a property exists, its serial to see
// that it changed, __system_property_read_callback for its value), and one named sysfs file,
// /sys/class/udc/<sys.usb.controller>/state. /sys/class/udc is never listed: the controller is found by its name.
// Portable: the property source is a parameter (bionic's on the phone, a stand-in in the host tests).
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include "xnuhost.h"

#if defined(__ANDROID__)
#include <sys/system_properties.h>
#endif

static const char *const prop_names[XH_USB_PROPS] = { "sys.usb.state", "sys.usb.config", "sys.usb.controller" };

bool xh_usb_controller_ok(const char *name)
{
  size_t n = strlen(name);
  if (!n || n >= XH_USB_VALUE || !strcmp(name, ".") || !strcmp(name, "..")) return false;
  for (size_t i = 0; i < n; i++) {
    const char c = name[i];
    if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '.' || c == '_' || c == ':' || c == '-'))
      return false;
  }
  return true;
}

// The first line of a small named file, without its line break; "" when it cannot be read.
static void read_line(const char *path, char *out, size_t size)
{
  out[0] = 0;
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return;
  ssize_t n = read(fd, out, size - 1);
  close(fd);
  if (n < 0) n = 0;
  out[n] = 0;
  out[strcspn(out, "\r\n")] = 0;
}

static const char *shown(const char *value) { return *value ? value : "(none)"; }

bool xh_usb_poll(xh_usb_watch *w, const xh_usb_source *src)
{
  for (unsigned i = 0; i < XH_USB_PROPS; i++) {
    uint32_t serial = 0;
    if (!src || !src->serial(src->ctx, i, prop_names[i], &serial)) {        // no such property (yet)
      w->value[i][0] = 0;
      w->known[i] = false;
      continue;
    }
    if (w->known[i] && serial == w->serial[i]) continue;                     // unchanged: the value read before
    if (!src->value(src->ctx, i, prop_names[i], w->value[i], sizeof w->value[i])) w->value[i][0] = 0;
    w->serial[i] = serial;
    w->known[i] = true;
  }
  const char *controller = w->value[XH_USB_CONTROLLER];
  char state[64] = "";
  bool named = xh_usb_controller_ok(controller);
  if (named) {
    char path[sizeof w->root + 64 + XH_USB_VALUE];
    snprintf(path, sizeof path, "%s/sys/class/udc/%s/state", w->root, controller);
    read_line(path, state, sizeof state);
  }
  char line[sizeof w->line];
  snprintf(line, sizeof line, "usb: state %s, config %s, UDC %s %s", shown(w->value[XH_USB_STATE]), shown(w->value[XH_USB_CONFIG]),
           named ? controller : *controller ? "(not a name)" : "(none)", *state ? state : "(unreadable)");
  bool changed = !w->logged || strcmp(line, w->line);
  memcpy(w->line, line, sizeof line);
  w->logged = true;
  return changed;
}

#if defined(__ANDROID__)
// Bionic's property area. A prop_info, once found, stays where it is for the life of the area: found once, then
// only its serial is read on each poll (as __system_property_wait does), and its value when that changed.
typedef struct { const prop_info *pi[XH_USB_PROPS]; } bionic_ctx;
static bionic_ctx bionic;

static bool bionic_serial(void *ctx, unsigned i, const char *name, uint32_t *serial)
{
  bionic_ctx *b = ctx;
  if (!b->pi[i]) b->pi[i] = __system_property_find(name);
  if (!b->pi[i]) return false;
  *serial = __system_property_serial(b->pi[i]);
  return true;
}

struct read_into { char *out; size_t size; };
static void got_value(void *cookie, const char *name, const char *value, uint32_t serial)
{
  (void)name; (void)serial;
  struct read_into *r = cookie;
  snprintf(r->out, r->size, "%s", value);
}

static bool bionic_value(void *ctx, unsigned i, const char *name, char *out, size_t size)
{
  (void)name;
  bionic_ctx *b = ctx;
  if (!b->pi[i]) return false;
  struct read_into r = { out, size };
  __system_property_read_callback(b->pi[i], got_value, &r);
  return true;
}

static const xh_usb_source bionic_source = { bionic_serial, bionic_value, &bionic };
const xh_usb_source *xh_usb_bionic(void) { return &bionic_source; }
#else
const xh_usb_source *xh_usb_bionic(void) { return NULL; }
#endif
