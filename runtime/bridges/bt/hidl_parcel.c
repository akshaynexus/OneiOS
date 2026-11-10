// SPDX-License-Identifier: MIT
// HIDL parcels for the Bluetooth HAL client (hidl_parcel.h).
#include "hidl_parcel.h"
#include <string.h>

void hb_parcel_init(hb_parcel *p)
{
  p->size = 0; p->nobjects = 0; p->buffers_size = 0; p->scratch_used = 0; p->overflow = false;
}

static uint8_t *reserve(hb_parcel *p, size_t bytes)
{
  size_t padded = (bytes + 3) & ~(size_t)3;
  if (p->overflow || padded > HB_PARCEL_DATA - p->size) { p->overflow = true; return NULL; }
  uint8_t *at = p->data + p->size;
  memset(at, 0, padded);
  p->size += padded;
  return at;
}

static void *scratch(hb_parcel *p, size_t bytes)
{
  size_t padded = (bytes + 7) & ~(size_t)7;
  if (p->overflow || padded > HB_PARCEL_SCRATCH - p->scratch_used) { p->overflow = true; return NULL; }
  void *at = p->scratch + p->scratch_used;
  memset(at, 0, padded);
  p->scratch_used += padded;
  return at;
}

void hb_put_token(hb_parcel *p, const char *interface)
{
  size_t n = strlen(interface) + 1;
  uint8_t *at = reserve(p, n);
  if (at) memcpy(at, interface, n);
}

void hb_put_i32(hb_parcel *p, int32_t value)
{
  uint8_t *at = reserve(p, 4);
  if (at) memcpy(at, &value, 4);
}

static unsigned put_object(hb_parcel *p, const void *object, size_t size)
{
  if (p->nobjects == HB_PARCEL_OBJECTS) { p->overflow = true; return 0; }
  size_t offset = p->size;
  uint8_t *at = reserve(p, size);
  if (!at) return 0;
  memcpy(at, object, size);
  p->offsets[p->nobjects] = offset;
  return p->nobjects++;
}

unsigned hb_put_buffer(hb_parcel *p, const void *buffer, size_t length)
{
  struct hb_buffer_object o = { .type = HB_TYPE_PTR, .buffer = (uint64_t)(uintptr_t)buffer, .length = length };
  p->buffers_size += (length + 7) & ~(uint64_t)7;
  return put_object(p, &o, sizeof o);
}

unsigned hb_put_embedded(hb_parcel *p, const void *buffer, size_t length, unsigned parent, size_t parent_offset)
{
  struct hb_buffer_object o = { .type = HB_TYPE_PTR, .flags = HB_BUFFER_HAS_PARENT, .buffer = (uint64_t)(uintptr_t)buffer,
                                .length = length, .parent = parent, .parent_offset = parent_offset };
  p->buffers_size += (length + 7) & ~(uint64_t)7;
  return put_object(p, &o, sizeof o);
}

void hb_put_string(hb_parcel *p, const char *text)
{
  struct hb_hidl_ref *ref = scratch(p, sizeof *ref);
  if (!ref) return;
  ref->pointer = (uint64_t)(uintptr_t)text;
  ref->size = (uint32_t)strlen(text);
  unsigned parent = hb_put_buffer(p, ref, sizeof *ref);
  hb_put_embedded(p, text, ref->size + 1u, parent, 0);
}

void hb_put_bytes(hb_parcel *p, const uint8_t *bytes, size_t length)
{
  struct hb_hidl_ref *ref = scratch(p, sizeof *ref);
  if (!ref) return;
  ref->pointer = (uint64_t)(uintptr_t)bytes;
  ref->size = (uint32_t)length;
  unsigned parent = hb_put_buffer(p, ref, sizeof *ref);
  hb_put_embedded(p, bytes, length, parent, 0);
}

void hb_put_string_vec(hb_parcel *p, const char *const *texts, unsigned count)
{
  struct hb_hidl_ref *vec = scratch(p, sizeof *vec), *items = scratch(p, count * sizeof *items);
  if (!vec || !items) return;
  vec->pointer = (uint64_t)(uintptr_t)items;
  vec->size = count;
  unsigned parent = hb_put_buffer(p, vec, sizeof *vec);
  unsigned array = hb_put_embedded(p, items, count * sizeof *items, parent, 0);
  for (unsigned i = 0; i < count; i++) {
    items[i].pointer = (uint64_t)(uintptr_t)texts[i];
    items[i].size = (uint32_t)strlen(texts[i]);
    hb_put_embedded(p, texts[i], items[i].size + 1u, array, i * sizeof *items);
  }
}

void hb_put_binder(hb_parcel *p, uint64_t pointer, uint64_t cookie)
{
  // 0x7f: the lowest priority the object accepts; 0x100: FLAT_BINDER_FLAG_ACCEPTS_FDS.
  struct hb_flat_object o = { .type = HB_TYPE_BINDER, .flags = 0x17f, .binder_or_handle = pointer, .cookie = cookie };
  put_object(p, &o, sizeof o);
}

// ---- reading ------------------------------------------------------------------------------------
void hb_reader_init(hb_reader *r, const void *data, size_t size, const void *offsets, size_t offsets_bytes)
{
  r->data = data; r->size = size; r->offsets = offsets; r->nobjects = (unsigned)(offsets_bytes / 8);
  r->position = 0; r->next_object = 0; r->bad = false;
}

const char *hb_get_token(hb_reader *r)
{
  if (r->bad || r->position >= r->size) { r->bad = true; return NULL; }
  const char *text = (const char *)r->data + r->position;
  size_t n = strnlen(text, r->size - r->position);
  if (n == r->size - r->position) { r->bad = true; return NULL; }
  r->position += (n + 1 + 3) & ~(size_t)3;
  return text;
}

int32_t hb_get_i32(hb_reader *r)
{
  int32_t v = 0;
  if (r->bad || r->size < 4 || r->position > r->size - 4) { r->bad = true; return 0; }
  memcpy(&v, r->data + r->position, 4);
  r->position += 4;
  return v;
}

// The object at the read position, which must be the next in the offsets list.
static const void *next_object(hb_reader *r, size_t size)
{
  if (r->bad || r->next_object >= r->nobjects) { r->bad = true; return NULL; }
  uint64_t offset;
  memcpy(&offset, &r->offsets[r->next_object], 8);
  if (offset != r->position || size > r->size || offset > r->size - size) { r->bad = true; return NULL; }
  r->next_object++;
  r->position += size;
  return r->data + offset;
}

bool hb_get_bytes(hb_reader *r, const uint8_t **bytes, size_t *length)
{
  struct hb_buffer_object top, child;
  struct hb_hidl_ref ref;
  unsigned parent_index = r->next_object;
  const void *o = next_object(r, sizeof top);
  if (!o) return false;
  memcpy(&top, o, sizeof top);
  o = next_object(r, sizeof child);
  if (!o) return false;
  memcpy(&child, o, sizeof child);
  if (top.type != HB_TYPE_PTR || child.type != HB_TYPE_PTR || top.length != sizeof ref || !top.buffer ||
      !(child.flags & HB_BUFFER_HAS_PARENT) || child.parent != parent_index || child.parent_offset != 0) { r->bad = true; return false; }
  memcpy(&ref, (const void *)(uintptr_t)top.buffer, sizeof ref);
  if (ref.size != child.length || (ref.size && ref.pointer != child.buffer)) { r->bad = true; return false; }
  *bytes = (const uint8_t *)(uintptr_t)child.buffer;
  *length = ref.size;
  return true;
}

bool hb_get_handle(hb_reader *r, uint32_t *handle)
{
  struct hb_flat_object flat;
  const void *o = next_object(r, sizeof flat);
  *handle = 0;
  if (!o) return false;
  memcpy(&flat, o, sizeof flat);
  if (flat.type != HB_TYPE_HANDLE) return false;
  *handle = (uint32_t)flat.binder_or_handle;
  return true;
}

// ---- the Bluetooth HAL's parcels ----------------------------------------------------------------
void bt_parcel_get_service(hb_parcel *p, const char *fq_name, const char *instance)
{
  hb_parcel_init(p);
  hb_put_token(p, HIDL_MANAGER);
  hb_put_string(p, fq_name);
  hb_put_string(p, instance);
}

void bt_parcel_initialize(hb_parcel *p, bool v1_1, uint64_t callback_pointer)
{
  hb_parcel_init(p);
  hb_put_token(p, v1_1 ? BT_HCI_1_1 : BT_HCI_1_0);
  hb_put_binder(p, callback_pointer, callback_pointer);
}

void bt_parcel_send(hb_parcel *p, unsigned code, const uint8_t *packet, size_t length)
{
  hb_parcel_init(p);
  hb_put_token(p, code == BT_SEND_ISO ? BT_HCI_1_1 : BT_HCI_1_0);
  hb_put_bytes(p, packet, length);
}

int bt_callback_transact(uint32_t code, hb_reader *r, hb_parcel *reply, uint8_t *type, const uint8_t **packet, size_t *length, int32_t *status)
{
  static const char *const chain[] = { BT_CALLBACKS_1_1, BT_CALLBACKS_1_0, HIDL_BASE };
  static const uint8_t h4[] = { [BT_CB_EVENT] = 4, [BT_CB_ACL] = 2, [BT_CB_SCO] = 3, [BT_CB_ISO] = 5 };
  *type = 0; *packet = NULL; *length = 0; *status = -1;
  hb_parcel_init(reply);
  const char *token = hb_get_token(r);
  if (!token) return HB_UNKNOWN_TRANSACTION;
  hb_put_i32(reply, 0);                              // Status::ok()
  switch (code) {
  case BT_CB_INIT_COMPLETE:
    if (strcmp(token, BT_CALLBACKS_1_0)) return HB_UNKNOWN_TRANSACTION;
    *status = hb_get_i32(r);
    return r->bad ? HB_UNKNOWN_TRANSACTION : 0;
  case BT_CB_EVENT: case BT_CB_ACL: case BT_CB_SCO: case BT_CB_ISO:
    if (strcmp(token, code == BT_CB_ISO ? BT_CALLBACKS_1_1 : BT_CALLBACKS_1_0)) return HB_UNKNOWN_TRANSACTION;
    if (!hb_get_bytes(r, packet, length)) return HB_UNKNOWN_TRANSACTION;
    *type = h4[code];
    return 0;
  case HB_INTERFACE_CHAIN:
    hb_put_string_vec(reply, chain, 3);
    return 0;
  case HB_INTERFACE_DESCRIPTOR:
    hb_put_string(reply, BT_CALLBACKS_1_1);
    return 0;
  case HB_GET_HASH_CHAIN: {
    // hidl_vec<uint8_t[32]>: empty (nothing compares the hashes of a client's callback).
    struct hb_hidl_ref *vec = (struct hb_hidl_ref *)(void *)reply->scratch;
    memset(vec, 0, sizeof *vec);
    reply->scratch_used = sizeof *vec;
    unsigned parent = hb_put_buffer(reply, vec, sizeof *vec);
    hb_put_embedded(reply, NULL, 0, parent, 0);
    return 0;
  }
  case HB_GET_DEBUG_INFO: {
    // DebugInfo { int32 pid; uint64 ptr; int32 arch }: pid -1 and ptr 0 are "not told", arch 1 = 64 bit.
    struct { int32_t pid; uint64_t ptr; int32_t arch; } *info = (void *)reply->scratch;
    memset(info, 0, sizeof *info);
    info->pid = -1; info->arch = 1;
    reply->scratch_used = sizeof *info;
    hb_put_buffer(reply, info, sizeof *info);
    return 0;
  }
  case HB_PING: case HB_SET_HAL_INSTRUMENTATION: case HB_NOTIFY_SYSPROPS_CHANGED: case HB_DEBUG:
    return 0;
  }
  return HB_UNKNOWN_TRANSACTION;
}
