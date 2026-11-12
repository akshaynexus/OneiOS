// SPDX-License-Identifier: MIT
// The parcels of a stable-AIDL transaction (aidl.h). Pure functions: tests/test_bridges.c checks them byte for byte.
#include "aidl.h"
#include <string.h>

struct flat_object { uint32_t type, flags; uint64_t binder_or_handle, cookie; };   // the kernel's flat_binder_object

void ab_init(ab_parcel *p)
{
  p->size = 0;
  p->nobjects = 0;
  p->overflow = false;
}

void ab_wipe(ab_parcel *p)
{
  volatile uint8_t *d = p->data;
  for (size_t i = 0; i < sizeof p->data; i++) d[i] = 0;
  ab_init(p);
}

static uint8_t *room(ab_parcel *p, size_t length)
{
  size_t padded = (length + 3u) & ~(size_t)3u;
  if (p->overflow || padded > sizeof p->data - p->size) { p->overflow = true; return NULL; }
  uint8_t *at = p->data + p->size;
  memset(at, 0, padded);
  p->size += padded;
  return at;
}

void ab_put_i32(ab_parcel *p, int32_t value)
{
  uint8_t *at = room(p, 4);
  if (at) memcpy(at, &value, 4);
}

void ab_put_string(ab_parcel *p, const char *text)
{
  if (!text) { ab_put_i32(p, -1); return; }
  size_t n = strlen(text);
  ab_put_i32(p, (int32_t)n);
  uint8_t *at = room(p, (n + 1) * 2);               // the units and a terminator
  for (size_t i = 0; at && i < n; i++) at[2 * i] = (uint8_t)text[i];
}

void ab_put_token(ab_parcel *p, const char *descriptor)
{
  ab_put_i32(p, (int32_t)0x80000000u);              // strict mode policy: none, penalties gathered
  ab_put_i32(p, -1);                                // no work source
  ab_put_i32(p, (int32_t)AB_HEADER_SYST);
  ab_put_string(p, descriptor);
}

void ab_put_bytes(ab_parcel *p, const uint8_t *bytes, size_t length)
{
  ab_put_i32(p, (int32_t)length);
  uint8_t *at = room(p, length);
  if (at && length) memcpy(at, bytes, length);
}

// listed: in the offsets array, for the kernel to translate. A null binder is not (libbinder's writeObject).
static void put_object(ab_parcel *p, const struct flat_object *o, int32_t stability, bool listed)
{
  if (listed && p->nobjects == AB_PARCEL_OBJECTS) { p->overflow = true; return; }
  size_t offset = p->size;
  uint8_t *at = room(p, sizeof *o);
  if (!at) return;
  memcpy(at, o, sizeof *o);
  if (listed) p->offsets[p->nobjects++] = offset;
  ab_put_i32(p, stability);
}

void ab_put_local(ab_parcel *p, uint64_t object)
{
  struct flat_object o = { AB_TYPE_BINDER, AB_LOCAL_FLAGS, object, object };
  put_object(p, &o, AB_STABILITY_VINTF, true);
}

void ab_put_handle(ab_parcel *p, uint32_t handle)
{
  struct flat_object o = { AB_TYPE_HANDLE, 0x100u | 0x13u, handle, 0 };
  put_object(p, &o, AB_STABILITY_VINTF, true);
}

void ab_put_null_binder(ab_parcel *p)
{
  struct flat_object o = { AB_TYPE_BINDER, 0x13u, 0, 0 };
  put_object(p, &o, 0, false);
}

void ab_put_ok(ab_parcel *p) { ab_put_i32(p, 0); }

void ab_put_service_error(ab_parcel *p, int32_t code)
{
  ab_put_i32(p, AB_EX_SERVICE_SPECIFIC);
  ab_put_string(p, "");                             // the message
  ab_put_i32(p, 0);                                 // no remote stack trace
  ab_put_i32(p, code);
}

void ab_reader_init(ab_reader *r, const void *data, size_t size, const void *offsets, size_t offsets_bytes)
{
  r->data = data;
  r->size = data ? size : 0;
  r->position = 0;
  r->offsets = offsets;
  r->nobjects = offsets ? (unsigned)(offsets_bytes / 8) : 0;
  r->bad = false;
}

static const uint8_t *take(ab_reader *r, size_t length)
{
  size_t padded = (length + 3u) & ~(size_t)3u;
  if (r->bad || padded < length || padded > r->size - r->position) { r->bad = true; return NULL; }
  const uint8_t *at = r->data + r->position;
  r->position += padded;
  return at;
}

int32_t ab_get_i32(ab_reader *r)
{
  int32_t v = 0;
  const uint8_t *at = take(r, 4);
  if (at) memcpy(&v, at, 4);
  return v;
}

bool ab_get_string(ab_reader *r, char *out, size_t size)
{
  if (size) out[0] = 0;
  int32_t n = ab_get_i32(r);
  if (r->bad) return false;
  if (n == -1) return true;                          // a null string
  if (n < 0 || (size_t)n > (r->size - r->position) / 2) { r->bad = true; return false; }
  const uint8_t *at = take(r, ((size_t)n + 1) * 2);
  if (!at) return false;
  for (size_t i = 0; i < (size_t)n && i + 1 < size; i++) {
    uint16_t unit = (uint16_t)(at[2 * i] | at[2 * i + 1] << 8);
    out[i] = unit >= 0x20 && unit < 0x7f ? (char)unit : '?';
    out[i + 1] = 0;
  }
  return true;
}

bool ab_get_bytes(ab_reader *r, const uint8_t **bytes, size_t *length)
{
  *bytes = NULL;
  *length = 0;
  int32_t n = ab_get_i32(r);
  if (r->bad) return false;
  if (n == -1) return true;
  if (n < 0) { r->bad = true; return false; }
  const uint8_t *at = take(r, (size_t)n);
  if (!at) return false;
  *bytes = at;
  *length = (size_t)n;
  return true;
}

bool ab_get_binder(ab_reader *r, uint32_t *handle, uint64_t *local)
{
  struct flat_object o;
  if (handle) *handle = 0;
  if (local) *local = 0;
  size_t offset = r->position;
  const uint8_t *at = take(r, sizeof o);
  if (!at) return false;
  memcpy(&o, at, sizeof o);
  (void)ab_get_i32(r);                               // the stability word
  if (r->bad) return false;
  bool listed = false;                               // the kernel translated only the objects of the offsets array
  for (unsigned i = 0; i < r->nobjects; i++) if (r->offsets[i] == offset) listed = true;
  if (o.type == AB_TYPE_HANDLE && listed) { if (handle) *handle = (uint32_t)o.binder_or_handle; return true; }
  if (o.type == AB_TYPE_BINDER && !o.binder_or_handle && !o.cookie) return false;       // null
  if (o.type == AB_TYPE_BINDER && listed) { if (local) *local = o.cookie; return false; }
  r->bad = true;
  return false;
}

bool ab_get_token(ab_reader *r, const char *descriptor)
{
  char name[128];
  (void)ab_get_i32(r);                               // strict mode policy
  (void)ab_get_i32(r);                               // work source
  (void)ab_get_i32(r);                               // 'SYST' (or a vendor libbinder's 'VNDR'): not compared
  return ab_get_string(r, name, sizeof name) && !strcmp(name, descriptor);
}

int32_t ab_get_status(ab_reader *r, int32_t *specific)
{
  char message[4];
  if (specific) *specific = 0;
  int32_t exception = ab_get_i32(r);
  if (exception == AB_EX_HAS_NOTED_APPOPS || exception == AB_EX_HAS_REPLY_HEADER) {   // a header to skip, then the code
    size_t start = r->position;
    int32_t size = ab_get_i32(r);
    if (r->bad || size < 4 || (size_t)size > r->size - start) { r->bad = true; return AB_BAD_TYPE; }
    r->position = start + (size_t)size;
    exception = exception == AB_EX_HAS_REPLY_HEADER ? 0 : ab_get_i32(r);
  }
  if (r->bad) return AB_BAD_TYPE;
  if (!exception) return 0;
  ab_get_string(r, message, sizeof message);
  size_t start = r->position;
  int32_t trace = ab_get_i32(r);                     // the remote stack trace's size, itself included; 0: none
  if (!r->bad && trace > 4 && (size_t)trace <= r->size - start) r->position = start + (size_t)trace;
  int32_t code = exception == AB_EX_SERVICE_SPECIFIC ? ab_get_i32(r) : exception;
  if (specific) *specific = r->bad ? exception : code;
  return exception;
}

size_t ab_get_parcelable(ab_reader *r)
{
  if (ab_get_i32(r) != 1 || r->bad) return 0;        // null
  size_t start = r->position;
  int32_t size = ab_get_i32(r);
  if (r->bad || size < 4 || (size_t)size > r->size - start) { r->bad = true; return 0; }
  return start + (size_t)size;
}

bool ab_standard(uint32_t code, const char *descriptor, int32_t version, const char *hash, ab_parcel *reply)
{
  ab_init(reply);
  switch (code) {
  case AB_INTERFACE: ab_put_string(reply, descriptor); return true;       // no status header: BBinder's own answer
  case AB_PING: return true;
  case AB_GET_VERSION: ab_put_ok(reply); ab_put_i32(reply, version); return true;
  case AB_GET_HASH: ab_put_ok(reply); ab_put_string(reply, hash); return true;
  default: return false;
  }
}
