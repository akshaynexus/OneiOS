// SPDX-License-Identifier: MIT
// Android's camera_metadata buffer, read without libcamera_metadata (not an NDK library), and what
// the helper derives from a camera's characteristics: which of the phone's cameras it is, the
// picture sizes and frame rates it gives.
//
// Layout (AOSP system/media/camera/src/camera_metadata.c, stable since Android 5): a 48-byte header
// { size, version, flags, entry_count, entry_capacity, entries_start, data_count, data_capacity,
// data_start, padding, vendor_id (u64) }, then 16-byte entries { tag, count, offset-or-4-bytes,
// type, 3 reserved }: values of at most 4 bytes are in the entry, the others at data_start + offset.
#include "camera.h"
#include <errno.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const uint8_t type_size[6] = { 1, 4, 4, 8, 8, 8 };
static uint32_t rd32(const uint8_t *p) { uint32_t v; memcpy(&v, p, 4); return v; }

// The entry of `tag`: its offset in the buffer and its values' offset. 0 or -errno.
static int locate(const uint8_t *buffer, size_t size, uint32_t tag, size_t *entry_at, size_t *data_at, uint32_t *count, uint8_t *type)
{
  if (size < 48) return -EINVAL;
  uint32_t total = rd32(buffer), entries = rd32(buffer + 12), entries_start = rd32(buffer + 20), data_count = rd32(buffer + 24), data_start = rd32(buffer + 32);
  if (total > size || entries_start > total || (uint64_t)entries * 16 > total - entries_start || data_start > total || data_count > total - data_start) return -EINVAL;
  for (uint32_t i = 0; i < entries; i++) {
    size_t at = entries_start + (size_t)i * 16;
    if (rd32(buffer + at) != tag) continue;
    uint32_t n = rd32(buffer + at + 4);
    uint8_t t = buffer[at + 12];
    if (t > 5) return -EINVAL;
    uint64_t bytes = (uint64_t)n * type_size[t];
    size_t values = at + 8;
    if (bytes > 4) {
      uint32_t offset = rd32(buffer + at + 8);
      if (offset > data_count || bytes > data_count - offset) return -EINVAL;
      values = (size_t)data_start + offset;
    }
    *entry_at = at; *data_at = values; *count = n; *type = t;
    return 0;
  }
  return -ENOENT;
}

int cam_meta_find(const uint8_t *buffer, size_t size, uint32_t tag, cam_meta_entry *entry)
{
  size_t at, values;
  int rc = locate(buffer, size, tag, &at, &values, &entry->count, &entry->type);
  if (rc) return rc;
  entry->data = buffer + values;
  return 0;
}

int cam_meta_set_i32(uint8_t *buffer, size_t size, uint32_t tag, const int32_t *values, unsigned count)
{
  size_t at, data;
  uint32_t have;
  uint8_t type;
  int rc = locate(buffer, size, tag, &at, &data, &have, &type);
  if (rc) return rc;
  if (type != CAM_META_INT32 || have != count) return -EINVAL;
  memcpy(buffer + data, values, (size_t)count * 4);
  return 0;
}

static int32_t i32_at(const cam_meta_entry *e, uint32_t i) { int32_t v; memcpy(&v, e->data + (size_t)i * 4, 4); return v; }
static float f32_at(const cam_meta_entry *e, uint32_t i) { float v; memcpy(&v, e->data + (size_t)i * 4, 4); return v; }

void cam_info_from_meta(cam_info *info, const uint8_t *buffer, size_t size)
{
  cam_meta_entry e;
  info->facing = OC_FACING_EXTERNAL;
  if (!cam_meta_find(buffer, size, CAM_TAG_LENS_FACING, &e) && e.type == CAM_META_BYTE && e.count >= 1) info->facing = e.data[0];
  if (!cam_meta_find(buffer, size, CAM_TAG_SENSOR_ORIENTATION, &e) && e.type == CAM_META_INT32 && e.count >= 1) info->orientation = i32_at(&e, 0);
  if (!cam_meta_find(buffer, size, CAM_TAG_FOCAL_LENGTHS, &e) && e.type == CAM_META_FLOAT && e.count >= 1) info->focal_mm = f32_at(&e, 0);
  if (!cam_meta_find(buffer, size, CAM_TAG_PHYSICAL_SIZE, &e) && e.type == CAM_META_FLOAT && e.count >= 2) { info->physical_w = f32_at(&e, 0); info->physical_h = f32_at(&e, 1); }
  if (!cam_meta_find(buffer, size, CAM_TAG_PIXEL_ARRAY_SIZE, &e) && e.type == CAM_META_INT32 && e.count >= 2) { info->pixel_w = i32_at(&e, 0); info->pixel_h = i32_at(&e, 1); }
  if (!cam_meta_find(buffer, size, CAM_TAG_CAPABILITIES, &e) && e.type == CAM_META_BYTE)
    for (uint32_t i = 0; i < e.count; i++) if (e.data[i] == 11) info->logical = 1;      // LOGICAL_MULTI_CAMERA
  // The HAL asks for its buffers itself (requestStreamBuffers) when this says HIDL_DEVICE_3_5 (0).
  if (!cam_meta_find(buffer, size, CAM_TAG_BUFFER_MANAGEMENT, &e) && e.type == CAM_META_BYTE && e.count >= 1) info->hal_buffers = 1;
  info->nsizes = 0;
  if (!cam_meta_find(buffer, size, CAM_TAG_STREAM_CONFIGURATIONS, &e) && e.type == CAM_META_INT32)
    for (uint32_t i = 0; i + 3 < e.count && info->nsizes < CAM_MAX_SIZES; i += 4)
      if (i32_at(&e, i) == 0x23 && i32_at(&e, i + 3) == 0) {                             // YCbCr_420_888, output
        info->sizes[info->nsizes].w = i32_at(&e, i + 1);
        info->sizes[info->nsizes].h = i32_at(&e, i + 2);
        info->nsizes++;
      }
  info->nfps = 0;
  if (!cam_meta_find(buffer, size, CAM_TAG_AE_AVAILABLE_FPS_RANGES, &e) && e.type == CAM_META_INT32)
    for (uint32_t i = 0; i + 1 < e.count && info->nfps < 32; i += 2) {
      info->fps[info->nfps].lo = i32_at(&e, i);
      info->fps[info->nfps].hi = i32_at(&e, i + 1);
      info->nfps++;
    }
  cam_info_finish(info);
}

void cam_info_finish(cam_info *info)
{
  // 35 mm equivalent: the full frame's diagonal (43.27 mm) over the sensor's.
  float diagonal = sqrtf(info->physical_w * info->physical_w + info->physical_h * info->physical_h);
  info->equiv35 = info->focal_mm > 0 && diagonal > 0 ? info->focal_mm * 43.27f / diagonal : 0;
}

// The S24 Ultra's rear lenses are 13, 24, 67 and 111 mm equivalent. The limits sit between them.
static uint32_t role_of(const cam_info *c)
{
  if (c->facing == OC_FACING_FRONT) return OC_ROLE_FRONT;
  if (c->facing != OC_FACING_BACK) return OC_ROLE_OTHER;
  if (c->equiv35 <= 0) return OC_ROLE_WIDE;                  // not said: the main camera
  if (c->equiv35 < 18) return OC_ROLE_ULTRAWIDE;
  if (c->equiv35 < 45) return OC_ROLE_WIDE;
  if (c->equiv35 < 90) return OC_ROLE_TELE3;
  return OC_ROLE_TELE5;
}

void cam_assign_roles(cam_info *list, int count)
{
  bool taken[OC_ROLE_OTHER + 1] = { false };
  // The first camera of each kind in the list's order has the role (Android lists the main rear and
  // front cameras first); a plain camera is preferred to a logical one made of several.
  for (int pass = 0; pass < 2; pass++)
    for (int i = 0; i < count; i++) {
      if (pass == 0 ? list[i].logical : !list[i].logical) continue;
      uint32_t role = role_of(&list[i]);
      if (role != OC_ROLE_OTHER && taken[role]) role = OC_ROLE_OTHER;
      taken[role] = true;
      list[i].role = role;
    }
}

int cam_pick_size(const cam_info *info, int width, int height, int *out_w, int *out_h)
{
  int best = -1, fallback = -1;
  for (int i = 0; i < info->nsizes; i++) {
    int w = info->sizes[i].w, h = info->sizes[i].h;
    if (w <= 0 || h <= 0 || w > (int)OC_MAX_WIDTH || h > (int)OC_MAX_HEIGHT || (w & 1) || (h & 1)) continue;
    if (w == width && h == height) { best = i; break; }
    if (w <= width && h <= height && (int64_t)w * height == (int64_t)h * width && (best < 0 || w > info->sizes[best].w)) best = i;
    if (fallback < 0 || (int64_t)w * h > (int64_t)info->sizes[fallback].w * info->sizes[fallback].h) fallback = i;
  }
  if (best < 0) best = fallback;
  if (best < 0) return -ENOENT;
  *out_w = info->sizes[best].w;
  *out_h = info->sizes[best].h;
  return 0;
}

int cam_pick_fps(const cam_info *info, int fps, int *lo, int *hi)
{
  int best = -1, best_cost = 0;
  for (int i = 0; i < info->nfps; i++) {
    // A fixed range at the rate is best; then the nearest top rate, the narrower range first.
    int cost = abs(info->fps[i].hi - fps) * 100 + (info->fps[i].hi - info->fps[i].lo);
    if (best < 0 || cost < best_cost) { best = i; best_cost = cost; }
  }
  if (best < 0) { *lo = *hi = fps; return -ENOENT; }
  *lo = info->fps[best].lo;
  *hi = info->fps[best].hi;
  return 0;
}

static const char *const role_names[] = { "none", "wide", "front", "ultrawide", "tele3", "tele5", "other" };
const char *cam_role_name(uint32_t role) { return role <= OC_ROLE_OTHER ? role_names[role] : "other"; }
uint32_t cam_role_from_name(const char *name)
{
  for (uint32_t i = 1; i < OC_ROLE_OTHER; i++) if (!strcmp(name, role_names[i])) return i;
  return OC_ROLE_NONE;
}

void cam_info_to_wire(const cam_info *info, struct oc_camera *out)
{
  int w = 0, h = 0, top = 0;
  (void)cam_pick_size(info, (int)OC_MAX_WIDTH, (int)OC_MAX_HEIGHT, &w, &h);
  for (int i = 0; i < info->nfps; i++) if (info->fps[i].hi > top) top = info->fps[i].hi;
  *out = (struct oc_camera){ .role = info->role, .facing = (uint32_t)info->facing, .orientation = (uint32_t)info->orientation,
                             .max_width = (uint32_t)w, .max_height = (uint32_t)h, .max_fps = (uint32_t)(top ? top : 30),
                             .focal_um = (uint32_t)(info->focal_mm * 1000.0f + 0.5f), .equiv35_mm = (uint32_t)(info->equiv35 + 0.5f) };
}
