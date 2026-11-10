// SPDX-License-Identifier: MIT
// The provider-HAL back end: android.hardware.camera.provider.ICameraProvider/internal/0 (AIDL, over
// /dev/binder) called the way cameraserver calls it, the way a GSI's framework reaches Samsung's
// vendor half. It does not go through cameraserver, so nothing of system_server is needed: this is
// the route for a session (README.md, "Which route").
//
// libbinder_ndk carries the transactions (its C API); the parcels are written by hand here to the
// AIDL definitions of hardware/interfaces/camera/{provider,device}/aidl (android15-release, interface
// versions 1 to 3; a later field is behind each parcelable's size and is skipped). Layout rules of
// the NDK AIDL back end: a parcelable is int32 1 (not null), int32 size (counting itself), the
// fields; a union is int32 1, int32 tag, the value; an array is int32 count, the elements; a method
// is transaction FIRST_CALL_TRANSACTION + its place in the interface.
//
// One stream: YCbCr_420_888 at the asked size into gralloc buffers of our own (AHardwareBuffer),
// a request per free buffer, the picture read with AHardwareBuffer_lockPlanes when the HAL returns it.
#include "camera.h"
#include <android/binder_ibinder.h>
#include <android/binder_parcel.h>
#include <android/binder_status.h>
#include <android/hardware_buffer.h>
#include <dlfcn.h>
#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define PROVIDER   "android.hardware.camera.provider.ICameraProvider"
#define DEVICE     "android.hardware.camera.device.ICameraDevice"
#define SESSION    "android.hardware.camera.device.ICameraDeviceSession"
#define CALLBACK   "android.hardware.camera.device.ICameraDeviceCallback"
// The AIDL hash of android.hardware.camera.device version 3 (aidl_api/.../3/.hash).
#define DEVICE_V3_HASH "ac7c7f74993871015f73daff1ea0c7f4d4f85ca7"

enum { PROVIDER_GET_ID_LIST = 2, PROVIDER_GET_DEVICE = 3 };
enum { DEVICE_GET_CHARACTERISTICS = 0, DEVICE_OPEN = 4 };
enum { SESSION_CLOSE = 0, SESSION_CONFIGURE_STREAMS = 1, SESSION_DEFAULT_SETTINGS = 2, SESSION_FLUSH = 3, SESSION_REQUEST_QUEUE = 4,
       SESSION_RESULT_QUEUE = 5, SESSION_PROCESS_REQUEST = 7 };
enum { CALLBACK_NOTIFY = 0, CALLBACK_RESULT = 1, CALLBACK_REQUEST_BUFFERS = 2, CALLBACK_RETURN_BUFFERS = 3 };
enum { METHOD_INTERFACE_VERSION = 16777214, METHOD_INTERFACE_HASH = 16777213 };     // what every AIDL interface answers
enum { FORMAT_YCBCR_420_888 = 0x23, DATASPACE_JFIF = 0x08c20000, MAX_BUFFERS = 12 };

typedef struct { int version, numFds, numInts; int data[]; } native_handle;

// Platform symbols of libbinder_ndk and libnativewindow that the NDK's link libraries leave out.
static AIBinder *(*check_service)(const char *instance);
static void (*start_thread_pool)(void);
static bool (*set_thread_pool_max)(uint32_t count);
static void (*mark_vintf)(AIBinder *binder);
static const native_handle *(*buffer_handle)(const AHardwareBuffer *buffer);

typedef struct { AHardwareBuffer *buffer; bool held, sent; } hal_buffer;

static struct {
  bool ready;
  AIBinder_Class *provider_class, *device_class, *session_class, *callback_class;
  AIBinder *provider, *callback;
  struct { char id[32]; AIBinder *device; } devices[CAM_MAX_LIST];
  int ndevices;
  // the running stream
  AIBinder *session;
  pthread_mutex_t lock;
  pthread_cond_t wake;
  pthread_t submitter;
  bool running, submitting, hal_buffers;
  int width, height, max_inflight, inflight, nbuffers;
  hal_buffer buffers[MAX_BUFFERS];
  uint8_t *settings;
  size_t settings_size;
  uint32_t next_frame;
  struct { uint32_t frame; uint64_t time; } stamps[64];
  cam_frame_fn frame;
  cam_error_fn error;
  void *ctx;
} hal = { .lock = PTHREAD_MUTEX_INITIALIZER, .wake = PTHREAD_COND_INITIALIZER };

// ---- parcels ------------------------------------------------------------------------------------
static int32_t put_begin(AParcel *p)
{
  AParcel_writeInt32(p, 1);
  int32_t start = AParcel_getDataPosition(p);
  AParcel_writeInt32(p, 0);
  return start;
}
static void put_end(AParcel *p, int32_t start)
{
  int32_t end = AParcel_getDataPosition(p);
  AParcel_setDataPosition(p, start);
  AParcel_writeInt32(p, end - start);
  AParcel_setDataPosition(p, end);
}
// Enters a parcelable: false when it is null or broken. *end is where it ends.
static bool get_begin(const AParcel *p, int32_t *end)
{
  int32_t present = 0, size = 0;
  if (AParcel_readInt32(p, &present) != STATUS_OK || !present) return false;
  int32_t start = AParcel_getDataPosition(p);
  if (AParcel_readInt32(p, &size) != STATUS_OK || size < 4 || size > (1 << 28)) return false;
  *end = start + size;
  return true;
}
static bool skip_parcelable(const AParcel *p)
{
  int32_t end;
  return get_begin(p, &end) && AParcel_setDataPosition(p, end) == STATUS_OK;
}

typedef struct { uint8_t *data; int32_t length; } bytes;
static bool bytes_allocator(void *ctx, int32_t length, int8_t **out)
{
  bytes *b = ctx;
  if (length < 0) return false;
  b->data = malloc(length ? (size_t)length : 1);
  b->length = length;
  *out = (int8_t *)b->data;
  return b->data != NULL;
}
static bool string_allocator(void *ctx, int32_t length, char **out)
{
  // length counts the terminator; -1 is a null string.
  char *into = ctx;
  if (length < 0) { into[0] = 0; return true; }
  if (length > 256) return false;
  *out = into;
  return true;
}

static void put_metadata(AParcel *p, const uint8_t *data, size_t size)
{
  int32_t s = put_begin(p);
  AParcel_writeByteArray(p, (const int8_t *)data, (int32_t)size);
  put_end(p, s);
}
static bool get_metadata(const AParcel *p, bytes *out)
{
  int32_t end;
  out->data = NULL; out->length = 0;
  if (!get_begin(p, &end) || AParcel_readByteArray(p, out, bytes_allocator) != STATUS_OK) return false;
  return AParcel_setDataPosition(p, end) == STATUS_OK;
}
static void put_handle(AParcel *p, const native_handle *h)
{
  int32_t s = put_begin(p);
  AParcel_writeInt32(p, h ? h->numFds : 0);
  for (int i = 0; h && i < h->numFds; i++) AParcel_writeParcelFileDescriptor(p, h->data[i]);
  AParcel_writeInt32Array(p, h ? h->data + h->numFds : NULL, h ? h->numInts : 0);
  put_end(p, s);
}
static void put_stream_buffer(AParcel *p, int32_t stream, int64_t id, const native_handle *h)
{
  int32_t s = put_begin(p);
  AParcel_writeInt32(p, stream);
  AParcel_writeInt64(p, id);
  put_handle(p, h);
  AParcel_writeInt32(p, 0);                // BufferStatus.OK
  put_handle(p, NULL);                     // no acquire fence
  put_handle(p, NULL);                     // no release fence
  put_end(p, s);
}

// A call: `fill` writes the arguments; the reply is returned after its status was read (NULL on failure).
static AParcel *call(AIBinder *binder, uint32_t method, const char *what, void (*fill)(AParcel *p, void *arg), void *arg)
{
  AParcel *in = NULL, *out = NULL;
  binder_status_t rc = AIBinder_prepareTransaction(binder, &in);
  if (rc == STATUS_OK && fill) fill(in, arg);
  if (rc == STATUS_OK) rc = AIBinder_transact(binder, FIRST_CALL_TRANSACTION + method, &in, &out, 0);
  if (rc != STATUS_OK) { cam_log("hal: %s: binder status %d", what, rc); if (in) AParcel_delete(in); return NULL; }
  AStatus *status = NULL;
  if (AParcel_readStatusHeader(out, &status) != STATUS_OK || !status) { cam_log("hal: %s: no status in the reply", what); AParcel_delete(out); return NULL; }
  if (!AStatus_isOk(status)) {
    // The HAL's errors are service-specific (camera/common Status): 1 illegal argument, 2 camera in use,
    // 3 too many cameras in use, 4 operation not supported, 5 camera disconnected, 6 internal error.
    cam_log("hal: %s: exception %d, camera status %d", what, AStatus_getExceptionCode(status), AStatus_getServiceSpecificError(status));
    AStatus_delete(status);
    AParcel_delete(out);
    return NULL;
  }
  AStatus_delete(status);
  return out;
}

// ---- the callback the HAL calls -----------------------------------------------------------------
static void reply_ok(AParcel *out)
{
  AStatus *ok = AStatus_newOk();
  AParcel_writeStatusHeader(out, ok);
  AStatus_delete(ok);
}

// A buffer came back: in a result (the request it belonged to is done) or unused.
static void buffer_done(int64_t id, uint32_t frame, bool ok, bool result)
{
  pthread_mutex_lock(&hal.lock);
  if (result && hal.inflight > 0) hal.inflight--;
  if (id < 1 || id > hal.nbuffers) { pthread_cond_broadcast(&hal.wake); pthread_mutex_unlock(&hal.lock); return; }
  hal_buffer *b = &hal.buffers[id - 1];
  if (ok && hal.running && b->held) {
    uint64_t time = hal.stamps[frame % 64].frame == frame ? hal.stamps[frame % 64].time : 0;
    AHardwareBuffer_Planes planes;
    if (AHardwareBuffer_lockPlanes(b->buffer, AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN, -1, NULL, &planes) == 0) {
      if (planes.planeCount >= 3 && planes.planes[0].data && planes.planes[1].data && planes.planes[2].data) {
        cam_yuv picture = { .y = planes.planes[0].data, .cb = planes.planes[1].data, .cr = planes.planes[2].data,
                            .y_stride = (int)planes.planes[0].rowStride, .c_stride = (int)planes.planes[1].rowStride,
                            .c_step = (int)planes.planes[1].pixelStride, .width = hal.width, .height = hal.height };
        hal.frame(hal.ctx, &picture, time);
      }
      AHardwareBuffer_unlock(b->buffer, NULL);
    }
  }
  b->held = false;
  pthread_cond_broadcast(&hal.wake);
  pthread_mutex_unlock(&hal.lock);
}

// Reads a NativeHandle that holds a fence and waits for it (at most a second).
static void wait_fence(const AParcel *in)
{
  int32_t end, count = 0;
  if (!get_begin(in, &end)) return;
  if (AParcel_readInt32(in, &count) == STATUS_OK)
    for (int32_t i = 0; i < count && i < 4; i++) {
      int fd = -1;
      if (AParcel_readParcelFileDescriptor(in, &fd) != STATUS_OK) break;
      if (fd >= 0) { struct pollfd wait = { .fd = fd, .events = POLLIN }; (void)poll(&wait, 1, 1000); close(fd); }
    }
  AParcel_setDataPosition(in, end);
}

// One StreamBuffer of a result or of returnStreamBuffers.
static bool take_stream_buffer(const AParcel *in, uint32_t frame, bool with_picture)
{
  int32_t end, stream = 0, status = 1;
  int64_t id = 0;
  if (!get_begin(in, &end)) return false;
  bool ok = AParcel_readInt32(in, &stream) == STATUS_OK && AParcel_readInt64(in, &id) == STATUS_OK && skip_parcelable(in) &&
            AParcel_readInt32(in, &status) == STATUS_OK && skip_parcelable(in);
  if (ok) wait_fence(in);
  buffer_done(id, frame, ok && with_picture && status == 0, with_picture);
  return AParcel_setDataPosition(in, end) == STATUS_OK;
}

static void on_notify(const AParcel *in)
{
  int32_t count = 0;
  if (AParcel_readInt32(in, &count) != STATUS_OK) return;
  for (int32_t i = 0; i < count; i++) {
    int32_t present = 0, tag = -1, end, frame = 0;
    if (AParcel_readInt32(in, &present) != STATUS_OK || !present || AParcel_readInt32(in, &tag) != STATUS_OK || !get_begin(in, &end)) return;
    if (AParcel_readInt32(in, &frame) != STATUS_OK) return;
    if (tag == 1) {                                          // ShutterMsg { frameNumber, timestamp, readoutTimestamp }
      int64_t time = 0;
      if (AParcel_readInt64(in, &time) != STATUS_OK) return;
      pthread_mutex_lock(&hal.lock);
      hal.stamps[(uint32_t)frame % 64].frame = (uint32_t)frame;
      hal.stamps[(uint32_t)frame % 64].time = (uint64_t)time;
      pthread_mutex_unlock(&hal.lock);
    } else if (tag == 0) {                                   // ErrorMsg { frameNumber, errorStreamId, errorCode }
      int32_t stream = 0, code = 0;
      (void)AParcel_readInt32(in, &stream);
      (void)AParcel_readInt32(in, &code);
      cam_log("hal: error %d for frame %d (1 device, 2 request, 3 result, 4 buffer)", code, frame);
      if (code == 1) {
        pthread_mutex_lock(&hal.lock);
        cam_error_fn fn = hal.running ? hal.error : NULL;
        pthread_mutex_unlock(&hal.lock);
        if (fn) fn(hal.ctx, EIO);
      }
    }
    if (AParcel_setDataPosition(in, end) != STATUS_OK) return;
  }
}

static void on_result(const AParcel *in)
{
  int32_t count = 0;
  if (AParcel_readInt32(in, &count) != STATUS_OK) return;
  for (int32_t i = 0; i < count; i++) {
    int32_t end, frame = 0, buffers = 0;
    int64_t fmq = 0;
    if (!get_begin(in, &end)) return;
    // CaptureResult { frameNumber, fmqResultSize, result, outputBuffers, ... }
    if (AParcel_readInt32(in, &frame) != STATUS_OK || AParcel_readInt64(in, &fmq) != STATUS_OK || !skip_parcelable(in) || AParcel_readInt32(in, &buffers) != STATUS_OK) return;
    for (int32_t k = 0; k < buffers; k++) if (!take_stream_buffer(in, (uint32_t)frame, true)) return;
    if (AParcel_setDataPosition(in, end) != STATUS_OK) return;
  }
}

// The HAL asks for buffers itself (HAL buffer management): BufferRequest[] in, the status and
// StreamBufferRet[] out. Every free buffer of ours may go out.
static void on_request_buffers(const AParcel *in, AParcel *out)
{
  int32_t count = 0, wanted[4] = { 0 }, streams[4] = { 0 };
  bool all = true;
  if (AParcel_readInt32(in, &count) != STATUS_OK || count < 0 || count > 4) count = 0;
  for (int32_t i = 0; i < count; i++) {
    int32_t end;
    if (!get_begin(in, &end) || AParcel_readInt32(in, &streams[i]) != STATUS_OK || AParcel_readInt32(in, &wanted[i]) != STATUS_OK ||
        AParcel_setDataPosition(in, end) != STATUS_OK) { count = i; break; }
  }
  reply_ok(out);
  int32_t status_at = AParcel_getDataPosition(out);
  AParcel_writeInt32(out, 0);                                // BufferRequestStatus, set below
  AParcel_writeInt32(out, count);
  pthread_mutex_lock(&hal.lock);
  for (int32_t i = 0; i < count; i++) {
    int32_t ret = put_begin(out);                            // StreamBufferRet { streamId, val }
    AParcel_writeInt32(out, streams[i]);
    AParcel_writeInt32(out, 1);                              // StreamBuffersVal: not null
    int have = 0;
    for (int b = 0; b < hal.nbuffers; b++) if (!hal.buffers[b].held) have++;
    if (!hal.running || streams[i] != 0 || have < wanted[i]) {
      AParcel_writeInt32(out, 0);                            // tag error
      AParcel_writeInt32(out, !hal.running || streams[i] != 0 ? 3 : 1);   // StreamBufferRequestError: STREAM_DISCONNECTED : NO_BUFFER_AVAILABLE
      all = false;
    } else {
      AParcel_writeInt32(out, 1);                            // tag buffers
      AParcel_writeInt32(out, wanted[i]);
      for (int b = 0, given = 0; b < hal.nbuffers && given < wanted[i]; b++) {
        hal_buffer *buffer = &hal.buffers[b];
        if (buffer->held) continue;
        put_stream_buffer(out, 0, b + 1, buffer->sent ? NULL : buffer_handle(buffer->buffer));
        buffer->held = buffer->sent = true;
        given++;
      }
    }
    put_end(out, ret);
  }
  pthread_mutex_unlock(&hal.lock);
  int32_t end = AParcel_getDataPosition(out);
  AParcel_setDataPosition(out, status_at);
  AParcel_writeInt32(out, all ? 0 : 1);                      // OK : FAILED_PARTIAL
  AParcel_setDataPosition(out, end);
}

static binder_status_t callback_transact(AIBinder *binder, transaction_code_t code, const AParcel *in, AParcel *out)
{
  (void)binder;
  switch (code) {
  case FIRST_CALL_TRANSACTION + CALLBACK_NOTIFY: on_notify(in); reply_ok(out); return STATUS_OK;
  case FIRST_CALL_TRANSACTION + CALLBACK_RESULT: on_result(in); reply_ok(out); return STATUS_OK;
  case FIRST_CALL_TRANSACTION + CALLBACK_REQUEST_BUFFERS: on_request_buffers(in, out); return STATUS_OK;
  case FIRST_CALL_TRANSACTION + CALLBACK_RETURN_BUFFERS: {
    int32_t count = 0;
    if (AParcel_readInt32(in, &count) == STATUS_OK) for (int32_t i = 0; i < count; i++) if (!take_stream_buffer(in, 0, false)) break;
    reply_ok(out);
    return STATUS_OK;
  }
  case FIRST_CALL_TRANSACTION + METHOD_INTERFACE_VERSION: reply_ok(out); AParcel_writeInt32(out, 3); return STATUS_OK;
  case FIRST_CALL_TRANSACTION + METHOD_INTERFACE_HASH: reply_ok(out); AParcel_writeString(out, DEVICE_V3_HASH, (int32_t)strlen(DEVICE_V3_HASH)); return STATUS_OK;
  }
  return STATUS_UNKNOWN_TRANSACTION;
}
static void *class_create(void *args) { return args; }
static void class_destroy(void *data) { (void)data; }
static binder_status_t no_transact(AIBinder *b, transaction_code_t c, const AParcel *in, AParcel *out) { (void)b; (void)c; (void)in; (void)out; return STATUS_UNKNOWN_TRANSACTION; }

// ---- set-up -------------------------------------------------------------------------------------
static int hal_init(void)
{
  if (hal.ready) return 0;
  check_service = (AIBinder *(*)(const char *))dlsym(RTLD_DEFAULT, "AServiceManager_checkService");
  start_thread_pool = (void (*)(void))dlsym(RTLD_DEFAULT, "ABinderProcess_startThreadPool");
  set_thread_pool_max = (bool (*)(uint32_t))dlsym(RTLD_DEFAULT, "ABinderProcess_setThreadPoolMaxThreadCount");
  mark_vintf = (void (*)(AIBinder *))dlsym(RTLD_DEFAULT, "AIBinder_markVintfStability");
  buffer_handle = (const native_handle *(*)(const AHardwareBuffer *))dlsym(RTLD_DEFAULT, "AHardwareBuffer_getNativeHandle");
  if (!check_service || !start_thread_pool || !set_thread_pool_max || !mark_vintf || !buffer_handle) {
    cam_log("hal: this Android's libbinder_ndk or libnativewindow lacks a platform symbol (service %d, pool %d/%d, vintf %d, handle %d)",
            check_service != NULL, start_thread_pool != NULL, set_thread_pool_max != NULL, mark_vintf != NULL, buffer_handle != NULL);
    return -ENOSYS;
  }
  hal.provider_class = AIBinder_Class_define(PROVIDER, class_create, class_destroy, no_transact);
  hal.device_class = AIBinder_Class_define(DEVICE, class_create, class_destroy, no_transact);
  hal.session_class = AIBinder_Class_define(SESSION, class_create, class_destroy, no_transact);
  hal.callback_class = AIBinder_Class_define(CALLBACK, class_create, class_destroy, callback_transact);
  if (!hal.provider_class || !hal.device_class || !hal.session_class || !hal.callback_class) return -ENOMEM;
  (void)set_thread_pool_max(4);
  start_thread_pool();                       // the HAL's calls into the callback arrive on these threads
  hal.provider = check_service(PROVIDER "/internal/0");
  if (!hal.provider) { cam_log("hal: no service " PROVIDER "/internal/0"); return -ENOENT; }
  if (!AIBinder_associateClass(hal.provider, hal.provider_class)) { cam_log("hal: the provider is not an " PROVIDER); return -EPROTO; }
  hal.callback = AIBinder_new(hal.callback_class, &hal);
  if (!hal.callback) return -ENOMEM;
  mark_vintf(hal.callback);                  // a vendor process may call it only with this
  hal.ready = true;
  return 0;
}

typedef struct { char names[CAM_MAX_LIST][96]; int count; } name_list;
static bool names_allocator(void *ctx, int32_t length) { name_list *l = ctx; l->count = length < 0 ? 0 : length > CAM_MAX_LIST ? CAM_MAX_LIST : length; return length <= CAM_MAX_LIST; }
static bool name_allocator(void *ctx, size_t index, int32_t length, char **out)
{
  name_list *l = ctx;
  if (index >= CAM_MAX_LIST || length > 96) return false;
  if (length < 0) { l->names[index][0] = 0; return true; }
  *out = l->names[index];
  return true;
}
static void fill_string(AParcel *p, void *arg) { const char *s = arg; AParcel_writeString(p, s, (int32_t)strlen(s)); }
static void fill_binder(AParcel *p, void *arg) { AParcel_writeStrongBinder(p, arg); }
static void fill_int(AParcel *p, void *arg) { AParcel_writeInt32(p, *(int32_t *)arg); }

static int hal_list(cam_info *out, int max)
{
  int rc = hal_init();
  if (rc) return rc;
  AParcel *reply = call(hal.provider, PROVIDER_GET_ID_LIST, "getCameraIdList", NULL, NULL);
  if (!reply) return -EIO;
  static name_list names;
  memset(&names, 0, sizeof names);
  binder_status_t status = AParcel_readStringArray(reply, &names, names_allocator, name_allocator);
  AParcel_delete(reply);
  if (status != STATUS_OK) { cam_log("hal: getCameraIdList: the list could not be read (%d)", status); return -EIO; }
  int count = 0;
  for (int i = 0; i < hal.ndevices; i++) AIBinder_decStrong(hal.devices[i].device);
  hal.ndevices = 0;
  for (int i = 0; i < names.count && count < max && count < CAM_MAX_LIST; i++) {
    // "device@1.1/internal/0": the id is what follows the last slash.
    const char *id = strrchr(names.names[i], '/');
    id = id ? id + 1 : names.names[i];
    AParcel *got = call(hal.provider, PROVIDER_GET_DEVICE, "getCameraDeviceInterface", fill_string, names.names[i]);
    AIBinder *device = NULL;
    if (!got) continue;
    status = AParcel_readStrongBinder(got, &device);
    AParcel_delete(got);
    if (status != STATUS_OK || !device || !AIBinder_associateClass(device, hal.device_class)) { if (device) AIBinder_decStrong(device); continue; }
    bytes meta;
    got = call(device, DEVICE_GET_CHARACTERISTICS, "getCameraCharacteristics", NULL, NULL);
    bool ok = got && get_metadata(got, &meta);
    if (got) AParcel_delete(got);
    if (!ok) { AIBinder_decStrong(device); continue; }
    cam_info *c = &out[count];
    memset(c, 0, sizeof *c);
    snprintf(c->id, sizeof c->id, "%s", id);
    cam_info_from_meta(c, meta.data, (size_t)meta.length);
    free(meta.data);
    snprintf(hal.devices[hal.ndevices].id, sizeof hal.devices[0].id, "%s", id);
    hal.devices[hal.ndevices++].device = device;
    count++;
  }
  return count;
}

// ---- the stream ---------------------------------------------------------------------------------
static void fill_configuration(AParcel *p, void *arg)
{
  (void)arg;
  int32_t config = put_begin(p);                             // StreamConfiguration
  AParcel_writeInt32(p, 1);                                  //   streams: one
  int32_t stream = put_begin(p);                             //   Stream
  AParcel_writeInt32(p, 0);                                  //     id
  AParcel_writeInt32(p, 0);                                  //     streamType OUTPUT
  AParcel_writeInt32(p, hal.width);
  AParcel_writeInt32(p, hal.height);
  AParcel_writeInt32(p, FORMAT_YCBCR_420_888);
  AParcel_writeInt64(p, (int64_t)AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN);     // usage: the consumer's
  AParcel_writeInt32(p, DATASPACE_JFIF);
  AParcel_writeInt32(p, 0);                                  //     rotation 0
  AParcel_writeString(p, "", 0);                             //     physicalCameraId
  AParcel_writeInt32(p, 0);                                  //     bufferSize
  AParcel_writeInt32(p, -1);                                 //     groupId: none
  int32_t mode = 0;
  AParcel_writeInt32Array(p, &mode, 1);                      //     sensorPixelModesUsed: DEFAULT
  AParcel_writeInt64(p, 1);                                  //     dynamicRangeProfile STANDARD
  AParcel_writeInt64(p, 0);                                  //     useCase DEFAULT
  AParcel_writeInt32(p, -1);                                 //     colorSpace: unspecified
  put_end(p, stream);
  AParcel_writeInt32(p, 0);                                  //   operationMode NORMAL_MODE
  put_metadata(p, NULL, 0);                                  //   sessionParams: none
  AParcel_writeInt32(p, 0);                                  //   streamConfigCounter
  AParcel_writeBool(p, false);                               //   multiResolutionInputImage
  AParcel_writeInt64(p, 0);                                  //   logId
  put_end(p, config);
}

typedef struct { uint32_t frame; int buffer; bool settings; } request;
static void fill_request(AParcel *p, void *arg)
{
  const request *r = arg;
  AParcel_writeInt32(p, 1);                                  // requests: one
  int32_t s = put_begin(p);                                  // CaptureRequest
  AParcel_writeInt32(p, (int32_t)r->frame);
  AParcel_writeInt64(p, 0);                                  //   fmqSettingsSize: the settings are here
  put_metadata(p, r->settings ? hal.settings : NULL, r->settings ? hal.settings_size : 0);   // empty = as the request before
  put_stream_buffer(p, -1, 0, NULL);                         //   inputBuffer: none
  AParcel_writeInt32(p, 0);                                  //   inputWidth
  AParcel_writeInt32(p, 0);                                  //   inputHeight
  AParcel_writeInt32(p, 1);                                  //   outputBuffers: one
  if (r->buffer < 0) put_stream_buffer(p, 0, 0, NULL);       //   the HAL asks for it (requestStreamBuffers)
  else put_stream_buffer(p, 0, r->buffer + 1, hal.buffers[r->buffer].sent ? NULL : buffer_handle(hal.buffers[r->buffer].buffer));
  AParcel_writeInt32(p, 0);                                  //   physicalCameraSettings: none
  put_end(p, s);
  AParcel_writeInt32(p, 0);                                  // cachesToRemove: none
}

static void *submit(void *arg)
{
  (void)arg;
  pthread_mutex_lock(&hal.lock);
  while (hal.submitting) {
    int buffer = -1;
    if (!hal.hal_buffers) for (int b = 0; b < hal.nbuffers; b++) if (!hal.buffers[b].held) { buffer = b; break; }
    if (hal.inflight >= hal.max_inflight || (!hal.hal_buffers && buffer < 0)) {
      struct timespec until;
      clock_gettime(CLOCK_REALTIME, &until);
      until.tv_sec += 1;
      // With the HAL's own buffer management a failed request returns no buffer: after a second
      // without one, the count of requests in flight starts again.
      if (pthread_cond_timedwait(&hal.wake, &hal.lock, &until) == ETIMEDOUT && hal.hal_buffers) hal.inflight = 0;
      continue;
    }
    request r = { .frame = hal.next_frame, .buffer = buffer, .settings = hal.next_frame == 0 };
    hal.next_frame++;
    hal.inflight++;
    if (buffer >= 0) hal.buffers[buffer].held = true;
    pthread_mutex_unlock(&hal.lock);
    AParcel *reply = call(hal.session, SESSION_PROCESS_REQUEST, "processCaptureRequest", fill_request, &r);
    pthread_mutex_lock(&hal.lock);
    if (buffer >= 0) hal.buffers[buffer].sent = true;
    if (!reply) {
      if (buffer >= 0) hal.buffers[buffer].held = false;
      cam_error_fn fn = hal.running ? hal.error : NULL;
      hal.submitting = false;
      pthread_mutex_unlock(&hal.lock);
      if (fn) fn(hal.ctx, EIO);
      return NULL;
    }
    AParcel_delete(reply);
  }
  pthread_mutex_unlock(&hal.lock);
  return NULL;
}

static void hal_teardown(void)
{
  if (hal.session) {
    AParcel *reply = call(hal.session, SESSION_FLUSH, "flush", NULL, NULL);
    if (reply) AParcel_delete(reply);
    reply = call(hal.session, SESSION_CLOSE, "close", NULL, NULL);       // returns when the HAL holds nothing of ours
    if (reply) AParcel_delete(reply);
    AIBinder_decStrong(hal.session);
    hal.session = NULL;
  }
  for (int b = 0; b < hal.nbuffers; b++) if (hal.buffers[b].buffer) AHardwareBuffer_release(hal.buffers[b].buffer);
  memset(hal.buffers, 0, sizeof hal.buffers);
  hal.nbuffers = 0;
  hal.inflight = 0;
  free(hal.settings);
  hal.settings = NULL;
}

static int hal_start(const cam_info *camera, int width, int height, int fps_lo, int fps_hi, cam_frame_fn frame, cam_error_fn error, void *ctx)
{
  AIBinder *device = NULL;
  AParcel *reply;
  const char *step = "open";
  int rc = hal_init();
  if (rc) return rc;
  for (int i = 0; i < hal.ndevices; i++) if (!strcmp(hal.devices[i].id, camera->id)) device = hal.devices[i].device;
  if (!device) return -ENODEV;
  hal.width = width; hal.height = height;
  hal.frame = frame; hal.error = error; hal.ctx = ctx;
  hal.next_frame = 0;
  memset(hal.stamps, 0xff, sizeof hal.stamps);

  if (!(reply = call(device, DEVICE_OPEN, "open", fill_binder, hal.callback))) return -EBUSY;
  binder_status_t status = AParcel_readStrongBinder(reply, &hal.session);
  AParcel_delete(reply);
  if (status != STATUS_OK || !hal.session || !AIBinder_associateClass(hal.session, hal.session_class)) goto failed;
  // cameraserver takes both metadata queues before anything else; a HAL may expect them made. They
  // are not used: settings go in the request, and a result's metadata is not needed.
  if ((reply = call(hal.session, SESSION_REQUEST_QUEUE, "getCaptureRequestMetadataQueue", NULL, NULL))) AParcel_delete(reply);
  if ((reply = call(hal.session, SESSION_RESULT_QUEUE, "getCaptureResultMetadataQueue", NULL, NULL))) AParcel_delete(reply);

  step = "constructDefaultRequestSettings";
  int32_t template = 1;                                      // RequestTemplate.PREVIEW
  bytes settings;
  if (!(reply = call(hal.session, SESSION_DEFAULT_SETTINGS, step, fill_int, &template))) goto failed;
  bool ok = get_metadata(reply, &settings);
  AParcel_delete(reply);
  if (!ok || settings.length < 48) { if (ok) free(settings.data); goto failed; }
  hal.settings = settings.data;
  hal.settings_size = (size_t)settings.length;
  int32_t range[2] = { fps_lo, fps_hi };
  if (cam_meta_set_i32(hal.settings, hal.settings_size, CAM_TAG_AE_TARGET_FPS_RANGE, range, 2)) cam_log("hal: the preview template has no frame rate range: the camera's own rate is used");

  step = "configureStreams";
  if (!(reply = call(hal.session, SESSION_CONFIGURE_STREAMS, step, fill_configuration, NULL))) goto failed;
  // HalStream { id, overrideFormat, producerUsage, consumerUsage, maxBuffers, overrideDataSpace,
  // physicalCameraId, supportOffline, enableHalBufferManager }
  int32_t count = 0, end = 0, id = 0, format = 0, max_buffers = 0, dataspace = 0;
  int64_t producer = 0, consumer = 0;
  bool offline = false, hal_buffers = false;
  char physical[256];
  ok = AParcel_readInt32(reply, &count) == STATUS_OK && count == 1 && get_begin(reply, &end) && AParcel_readInt32(reply, &id) == STATUS_OK &&
       AParcel_readInt32(reply, &format) == STATUS_OK && AParcel_readInt64(reply, &producer) == STATUS_OK && AParcel_readInt64(reply, &consumer) == STATUS_OK &&
       AParcel_readInt32(reply, &max_buffers) == STATUS_OK;
  if (ok && AParcel_readInt32(reply, &dataspace) == STATUS_OK && AParcel_readString(reply, physical, string_allocator) == STATUS_OK &&
      AParcel_readBool(reply, &offline) == STATUS_OK && AParcel_getDataPosition(reply) < end) (void)AParcel_readBool(reply, &hal_buffers);
  AParcel_delete(reply);
  if (!ok) { cam_log("hal: configureStreams: the answer could not be read"); goto failed; }
  hal.hal_buffers = hal_buffers || camera->hal_buffers;
  hal.max_inflight = max_buffers < 1 ? 1 : max_buffers > MAX_BUFFERS - 1 ? MAX_BUFFERS - 1 : max_buffers;
  hal.nbuffers = hal.max_inflight + 1;
  if (cam_verbose) cam_log("hal: stream format %#x, producer usage %#llx, at most %d buffers, HAL buffer management %d", format, (unsigned long long)producer, max_buffers, hal.hal_buffers);

  step = "AHardwareBuffer_allocate";
  AHardwareBuffer_Desc desc = { .width = (uint32_t)width, .height = (uint32_t)height, .layers = 1, .format = (uint32_t)format,
                                .usage = (uint64_t)producer | (uint64_t)consumer | AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN };
  for (int b = 0; b < hal.nbuffers; b++)
    if (AHardwareBuffer_allocate(&desc, &hal.buffers[b].buffer) != 0 || !buffer_handle(hal.buffers[b].buffer)) { hal.nbuffers = b; goto failed; }

  step = "the request thread";
  pthread_mutex_lock(&hal.lock);
  hal.running = hal.submitting = true;
  pthread_mutex_unlock(&hal.lock);
  if (pthread_create(&hal.submitter, NULL, submit, NULL)) { hal.running = hal.submitting = false; goto failed; }
  return 0;
failed:
  cam_log("hal: camera %s: %s failed", camera->id, step);
  hal_teardown();
  return -EIO;
}

static void hal_stop(void)
{
  pthread_mutex_lock(&hal.lock);
  bool was = hal.running;
  hal.running = hal.submitting = false;      // from here no frame is handed on
  pthread_cond_broadcast(&hal.wake);
  pthread_mutex_unlock(&hal.lock);
  if (!was) return;
  pthread_join(hal.submitter, NULL);
  hal_teardown();
}

const cam_backend cam_backend_hal = { "hal", hal_list, hal_start, hal_stop };
