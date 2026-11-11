// SPDX-License-Identifier: MIT
// The Camera2 NDK back end: ACameraManager through cameraserver ("media.camera"). The provider HAL
// does the ISP work, so the pictures arrive finished (YUV_420_888).
//
// This is the way with One UI's framework running. In a session it is expected NOT to work
// (README.md, "Which route"): cameraserver asks system_server's permission_checker service for
// android.permission.CAMERA for every client outside its own process, waits 10 s for the service
// and then denies. backend_hal.c is the way there.
//
// Binder threads (README.md, "The first phone run"): everything cameraserver sends back comes as a
// binder call INTO this process: the device's callbacks (capture started, result, error) and the
// picture buffers themselves (the AImageReader's buffer queue lives here; cameraserver's request
// thread calls its dequeueBuffer and queueBuffer). An app has binder threads from zygote. A plain
// executable has none, and libcamera2ndk starts none: openCamera and createCaptureSession still
// work (cameraserver's calls back during our own call are served on the calling thread), and then
// cameraserver's first dequeueBuffer waits for ever. So the pool is started here, before anything.
//
// Threads: the NDK's device callbacks arrive on the camera manager's own looper thread, the
// pictures on the AImageReader's own looper thread; neither is ours to make.
#include "camera.h"
#include <camera/NdkCameraManager.h>
#include <camera/NdkCameraMetadataTags.h>
#include <errno.h>
#include <media/NdkImageReader.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>

// meta.c reads the same numbers from the HAL's buffers.
_Static_assert(CAM_TAG_AE_TARGET_FPS_RANGE == ACAMERA_CONTROL_AE_TARGET_FPS_RANGE && CAM_TAG_AE_AVAILABLE_FPS_RANGES == ACAMERA_CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES &&
               CAM_TAG_LENS_FACING == ACAMERA_LENS_FACING && CAM_TAG_FOCAL_LENGTHS == ACAMERA_LENS_INFO_AVAILABLE_FOCAL_LENGTHS &&
               CAM_TAG_CAPABILITIES == ACAMERA_REQUEST_AVAILABLE_CAPABILITIES && CAM_TAG_STREAM_CONFIGURATIONS == ACAMERA_SCALER_AVAILABLE_STREAM_CONFIGURATIONS &&
               CAM_TAG_SENSOR_ORIENTATION == ACAMERA_SENSOR_ORIENTATION && CAM_TAG_PHYSICAL_SIZE == ACAMERA_SENSOR_INFO_PHYSICAL_SIZE &&
               CAM_TAG_PIXEL_ARRAY_SIZE == ACAMERA_SENSOR_INFO_PIXEL_ARRAY_SIZE && ACAMERA_LENS_FACING_FRONT == OC_FACING_FRONT &&
               ACAMERA_LENS_FACING_BACK == OC_FACING_BACK && ACAMERA_REQUEST_AVAILABLE_CAPABILITIES_LOGICAL_MULTI_CAMERA == 11 &&
               AIMAGE_FORMAT_YUV_420_888 == 0x23, "camera metadata tag numbers");

float cam_ndk_zoom;

static struct {
  ACameraManager *manager;
  ACameraDevice *device;
  AImageReader *reader;
  ACaptureSessionOutputContainer *outputs;
  ACaptureSessionOutput *output;
  ACameraOutputTarget *target;
  ACaptureRequest *request;
  ACameraCaptureSession *session;
  pthread_mutex_t lock;
  bool running;
  cam_frame_fn frame;
  cam_error_fn error;
  void *ctx;
  // What came back since the start (atomic; printed at the stop and by the callbacks).
  unsigned active, ready, closed, started, completed, failed, lost, sequences, images, bad_images, device_errors;
} ndk = { .lock = PTHREAD_MUTEX_INITIALIZER };

static unsigned bump(unsigned *counter) { return __atomic_add_fetch(counter, 1, __ATOMIC_RELAXED); }
static unsigned count_of(const unsigned *counter) { return __atomic_load_n(counter, __ATOMIC_RELAXED); }
// Whether the n-th callback of a kind is printed: all with -vv, the first three and every 30th with -v.
static bool shown(unsigned n) { return cam_verbose >= 2 || (cam_verbose == 1 && (n <= 3 || n % 30 == 0)); }

static const char *status_name(camera_status_t status)
{
  switch (status) {
  case ACAMERA_OK: return "ok";
  case ACAMERA_ERROR_UNKNOWN: return "ERROR_UNKNOWN";
  case ACAMERA_ERROR_INVALID_PARAMETER: return "ERROR_INVALID_PARAMETER";
  case ACAMERA_ERROR_CAMERA_DISCONNECTED: return "ERROR_CAMERA_DISCONNECTED";
  case ACAMERA_ERROR_NOT_ENOUGH_MEMORY: return "ERROR_NOT_ENOUGH_MEMORY";
  case ACAMERA_ERROR_METADATA_NOT_FOUND: return "ERROR_METADATA_NOT_FOUND";
  case ACAMERA_ERROR_CAMERA_DEVICE: return "ERROR_CAMERA_DEVICE";
  case ACAMERA_ERROR_CAMERA_SERVICE: return "ERROR_CAMERA_SERVICE";
  case ACAMERA_ERROR_SESSION_CLOSED: return "ERROR_SESSION_CLOSED";
  case ACAMERA_ERROR_INVALID_OPERATION: return "ERROR_INVALID_OPERATION";
  case ACAMERA_ERROR_STREAM_CONFIGURE_FAIL: return "ERROR_STREAM_CONFIGURE_FAIL";
  case ACAMERA_ERROR_CAMERA_IN_USE: return "ERROR_CAMERA_IN_USE";
  case ACAMERA_ERROR_MAX_CAMERA_IN_USE: return "ERROR_MAX_CAMERA_IN_USE";
  case ACAMERA_ERROR_CAMERA_DISABLED: return "ERROR_CAMERA_DISABLED";
  case ACAMERA_ERROR_PERMISSION_DENIED: return "ERROR_PERMISSION_DENIED";
  case ACAMERA_ERROR_UNSUPPORTED_OPERATION: return "ERROR_UNSUPPORTED_OPERATION";
  default: return "?";
  }
}

// ACameraDevice_StateCallbacks.onError's codes, with what cameraserver means by each.
static const char *device_error_name(int error)
{
  switch (error) {
  case ERROR_CAMERA_IN_USE: return "ERROR_CAMERA_IN_USE (another client has it)";
  case ERROR_MAX_CAMERAS_IN_USE: return "ERROR_MAX_CAMERAS_IN_USE";
  case ERROR_CAMERA_DISABLED: return "ERROR_CAMERA_DISABLED (a policy took it: device policy, the camera privacy switch, or this uid counted as idle)";
  case ERROR_CAMERA_DEVICE: return "ERROR_CAMERA_DEVICE (the camera device failed: the HAL, or a stream's buffers)";
  case ERROR_CAMERA_SERVICE: return "ERROR_CAMERA_SERVICE (cameraserver failed or died)";
  default: return "?";
  }
}

static int error_of(camera_status_t status)
{
  switch (status) {
  case ACAMERA_OK: return 0;
  case ACAMERA_ERROR_PERMISSION_DENIED: return EACCES;
  case ACAMERA_ERROR_CAMERA_IN_USE: case ACAMERA_ERROR_MAX_CAMERA_IN_USE: return EBUSY;
  case ACAMERA_ERROR_CAMERA_DISABLED: return EPERM;
  case ACAMERA_ERROR_CAMERA_DISCONNECTED: return ENODEV;
  case ACAMERA_ERROR_INVALID_PARAMETER: return EINVAL;
  default: return EIO;
  }
}

static int ndk_init(void)
{
  int rc = cam_binder_threads();
  if (rc) cam_log("ndk: no binder threads (%s): cameraserver's callbacks and picture buffers cannot arrive", strerror(-rc));
  else if (cam_verbose) cam_log("ndk: binder threads started (cameraserver's callbacks and buffer queue calls arrive on them)");
  if (!ndk.manager) ndk.manager = ACameraManager_create();
  return ndk.manager ? 0 : -EIO;
}

static void fill_info(cam_info *c, const char *id, const char *parent, const ACameraMetadata *meta)
{
  memset(c, 0, sizeof *c);
  snprintf(c->id, sizeof c->id, "%s", id);
  if (parent) snprintf(c->parent, sizeof c->parent, "%s", parent);
  c->facing = OC_FACING_EXTERNAL;
  ACameraMetadata_const_entry e;
  if (ACameraMetadata_getConstEntry(meta, ACAMERA_LENS_FACING, &e) == ACAMERA_OK && e.count >= 1) c->facing = e.data.u8[0];
  if (ACameraMetadata_getConstEntry(meta, ACAMERA_SENSOR_ORIENTATION, &e) == ACAMERA_OK && e.count >= 1) c->orientation = e.data.i32[0];
  if (ACameraMetadata_getConstEntry(meta, ACAMERA_LENS_INFO_AVAILABLE_FOCAL_LENGTHS, &e) == ACAMERA_OK && e.count >= 1) c->focal_mm = e.data.f[0];
  if (ACameraMetadata_getConstEntry(meta, ACAMERA_SENSOR_INFO_PHYSICAL_SIZE, &e) == ACAMERA_OK && e.count >= 2) { c->physical_w = e.data.f[0]; c->physical_h = e.data.f[1]; }
  if (ACameraMetadata_getConstEntry(meta, ACAMERA_SENSOR_INFO_PIXEL_ARRAY_SIZE, &e) == ACAMERA_OK && e.count >= 2) { c->pixel_w = e.data.i32[0]; c->pixel_h = e.data.i32[1]; }
  if (ACameraMetadata_getConstEntry(meta, ACAMERA_REQUEST_AVAILABLE_CAPABILITIES, &e) == ACAMERA_OK)
    for (uint32_t k = 0; k < e.count; k++) if (e.data.u8[k] == ACAMERA_REQUEST_AVAILABLE_CAPABILITIES_LOGICAL_MULTI_CAMERA) c->logical = 1;
  if (ACameraMetadata_getConstEntry(meta, ACAMERA_SCALER_AVAILABLE_STREAM_CONFIGURATIONS, &e) == ACAMERA_OK)
    for (uint32_t k = 0; k + 3 < e.count && c->nsizes < CAM_MAX_SIZES; k += 4)
      if (e.data.i32[k] == AIMAGE_FORMAT_YUV_420_888 && e.data.i32[k + 3] == 0) { c->sizes[c->nsizes].w = e.data.i32[k + 1]; c->sizes[c->nsizes].h = e.data.i32[k + 2]; c->nsizes++; }
  if (ACameraMetadata_getConstEntry(meta, ACAMERA_CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES, &e) == ACAMERA_OK)
    for (uint32_t k = 0; k + 1 < e.count && c->nfps < 32; k += 2) { c->fps[c->nfps].lo = e.data.i32[k]; c->fps[c->nfps].hi = e.data.i32[k + 1]; c->nfps++; }
  cam_info_finish(c);
}

// The cameras cameraserver lists, then the physical cameras behind each logical one that are not in
// the list themselves (the S24 Ultra's 3x and 5x sit behind logical camera 0): those are opened
// through their logical camera with a physical stream (ndk_start).
static int ndk_list(cam_info *out, int max)
{
  int rc = ndk_init();
  if (rc) return rc;
  ACameraIdList *ids = NULL;
  camera_status_t status = ACameraManager_getCameraIdList(ndk.manager, &ids);
  if (status != ACAMERA_OK || !ids) { cam_log("ndk: no camera list from cameraserver: status %d %s", status, status_name(status)); return -error_of(status); }
  int count = 0;
  for (int i = 0; i < ids->numCameras && count < max; i++) {
    ACameraMetadata *meta = NULL;
    status = ACameraManager_getCameraCharacteristics(ndk.manager, ids->cameraIds[i], &meta);
    if (status != ACAMERA_OK || !meta) { cam_log("ndk: camera %s: no characteristics: status %d %s", ids->cameraIds[i], status, status_name(status)); continue; }
    fill_info(&out[count++], ids->cameraIds[i], NULL, meta);
    ACameraMetadata_free(meta);
  }
  int listed = count;
  for (int i = 0; i < listed; i++) {
    if (!out[i].logical) continue;
    ACameraMetadata *meta = NULL;
    size_t members = 0;
    const char *const *member = NULL;
    if (ACameraManager_getCameraCharacteristics(ndk.manager, out[i].id, &meta) != ACAMERA_OK || !meta) continue;
    if (!ACameraMetadata_isLogicalMultiCamera(meta, &members, &member)) members = 0;
    if (cam_verbose) cam_log("ndk: logical camera %s has %zu physical cameras", out[i].id, members);
    for (size_t m = 0; m < members && count < max; m++) {
      bool known = false;
      for (int k = 0; k < count; k++) if (!strcmp(out[k].id, member[m])) known = true;
      if (cam_verbose) cam_log("ndk:   physical camera %s%s", member[m], known ? " (listed by itself too)" : "");
      if (known) continue;
      ACameraMetadata *physical = NULL;
      status = ACameraManager_getCameraCharacteristics(ndk.manager, member[m], &physical);
      if (status != ACAMERA_OK || !physical) { cam_log("ndk: physical camera %s of %s: no characteristics: status %d %s", member[m], out[i].id, status, status_name(status)); continue; }
      fill_info(&out[count++], member[m], out[i].id, physical);
      ACameraMetadata_free(physical);
    }
    ACameraMetadata_free(meta);                // the member names are the metadata's
  }
  ACameraManager_deleteCameraIdList(ids);
  return count;
}

static void on_image(void *context, AImageReader *reader)
{
  (void)context;
  AImage *image = NULL;
  media_status_t got = AImageReader_acquireLatestImage(reader, &image);
  if (got != AMEDIA_OK || !image) {
    if (shown(bump(&ndk.bad_images)) || count_of(&ndk.bad_images) <= 3) cam_log("ndk: image available, but acquireLatestImage: media status %d", got);
    return;
  }
  cam_yuv picture = { 0 };
  uint8_t *y = NULL, *cb = NULL, *cr = NULL;
  int length = 0, step = 0;
  int32_t width = 0, height = 0, y_stride = 0, c_stride = 0, format = 0;
  int64_t time = 0;
  bool ok = AImage_getPlaneData(image, 0, &y, &length) == AMEDIA_OK && AImage_getPlaneData(image, 1, &cb, &length) == AMEDIA_OK &&
            AImage_getPlaneData(image, 2, &cr, &length) == AMEDIA_OK && AImage_getPlaneRowStride(image, 0, &y_stride) == AMEDIA_OK &&
            AImage_getPlaneRowStride(image, 1, &c_stride) == AMEDIA_OK && AImage_getPlanePixelStride(image, 1, &step) == AMEDIA_OK &&
            AImage_getWidth(image, &width) == AMEDIA_OK && AImage_getHeight(image, &height) == AMEDIA_OK;
  (void)AImage_getTimestamp(image, &time);
  (void)AImage_getFormat(image, &format);
  unsigned n = bump(&ndk.images);
  if (shown(n)) cam_log("ndk: image %u: %dx%d format 0x%x, rows %d/%d apart, chroma step %d, time %lld ns%s", n, width, height, format, y_stride, c_stride, step,
                        (long long)time, ok ? "" : " (its planes could not be read)");
  if (ok) {
    picture = (cam_yuv){ .y = y, .cb = cb, .cr = cr, .y_stride = y_stride, .c_stride = c_stride, .c_step = step, .width = width, .height = height };
    pthread_mutex_lock(&ndk.lock);
    if (ndk.running) ndk.frame(ndk.ctx, &picture, (uint64_t)time);
    pthread_mutex_unlock(&ndk.lock);
  } else if (bump(&ndk.bad_images) <= 3) cam_log("ndk: an image's planes could not be read (%dx%d format 0x%x)", width, height, format);
  AImage_delete(image);
}

static void report(int error)
{
  cam_error_fn fn = NULL;
  pthread_mutex_lock(&ndk.lock);
  if (ndk.running) fn = ndk.error;
  pthread_mutex_unlock(&ndk.lock);
  if (fn) fn(ndk.ctx, error);
}

// ---- the callbacks: every one is counted, and printed (session and device ones always) ----------
static void on_disconnected(void *context, ACameraDevice *device)
{
  (void)context;
  cam_log("ndk: device %s: disconnected (cameraserver took the camera away: another client with a higher priority, a policy, or cameraserver restarted)", ACameraDevice_getId(device));
  report(ENODEV);
}
static void on_device_error(void *context, ACameraDevice *device, int error)
{
  (void)context;
  bump(&ndk.device_errors);
  cam_log("ndk: device %s: error %d %s", ACameraDevice_getId(device), error, device_error_name(error));
  report(error == ERROR_CAMERA_DISABLED ? EPERM : error == ERROR_CAMERA_IN_USE || error == ERROR_MAX_CAMERAS_IN_USE ? EBUSY : EIO);
}
static void on_session_closed(void *context, ACameraCaptureSession *session) { (void)context; (void)session; bump(&ndk.closed); if (cam_verbose) cam_log("ndk: session: closed"); }
static void on_session_ready(void *context, ACameraCaptureSession *session)
{
  (void)context; (void)session;
  unsigned n = bump(&ndk.ready);
  if (cam_verbose) cam_log("ndk: session: ready (idle: no request is being processed)%s", n == 1 ? "" : " again");
}
static void on_session_active(void *context, ACameraCaptureSession *session)
{
  (void)context; (void)session;
  bump(&ndk.active);
  if (cam_verbose) cam_log("ndk: session: active (cameraserver began processing requests)");
}
static void on_capture_started(void *context, ACameraCaptureSession *session, const ACaptureRequest *request, int64_t timestamp)
{
  (void)context; (void)session; (void)request;
  unsigned n = bump(&ndk.started);
  if (shown(n)) cam_log("ndk: capture %u started: sensor time %lld ns", n, (long long)timestamp);
}
static void on_capture_completed(void *context, ACameraCaptureSession *session, ACaptureRequest *request, const ACameraMetadata *result)
{
  (void)context; (void)session; (void)request;
  unsigned n = bump(&ndk.completed);
  if (!shown(n)) return;
  ACameraMetadata_const_entry e;
  long long time = ACameraMetadata_getConstEntry(result, ACAMERA_SENSOR_TIMESTAMP, &e) == ACAMERA_OK && e.count >= 1 ? (long long)e.data.i64[0] : -1;
  cam_log("ndk: capture %u completed: result's sensor time %lld ns", n, time);
}
static void on_capture_failed(void *context, ACameraCaptureSession *session, ACaptureRequest *request, ACameraCaptureFailure *failure)
{
  (void)context; (void)session; (void)request;
  unsigned n = bump(&ndk.failed);
  if (n <= 5 || shown(n))
    cam_log("ndk: capture failed (%u): frame %lld, reason %d %s, sequence %d, picture %s", n, (long long)failure->frameNumber, failure->reason,
            failure->reason == CAPTURE_FAILURE_REASON_FLUSHED ? "FLUSHED (dropped by an abort or a stop)" : "ERROR (the camera or cameraserver dropped the frame)",
            failure->sequenceId, failure->wasImageCaptured ? "was made" : "was not made");
}
static void on_sequence_completed(void *context, ACameraCaptureSession *session, int sequence, int64_t frame)
{
  (void)context; (void)session;
  bump(&ndk.sequences);
  if (cam_verbose) cam_log("ndk: capture sequence %d ended at frame %lld", sequence, (long long)frame);
}
static void on_sequence_aborted(void *context, ACameraCaptureSession *session, int sequence)
{
  (void)context; (void)session;
  bump(&ndk.sequences);
  cam_log("ndk: capture sequence %d aborted before any frame", sequence);
}
static void on_buffer_lost(void *context, ACameraCaptureSession *session, ACaptureRequest *request, ACameraWindowType *window, int64_t frame)
{
  (void)context; (void)session; (void)request; (void)window;
  unsigned n = bump(&ndk.lost);
  if (n <= 5 || shown(n)) cam_log("ndk: picture buffer lost (%u): frame %lld (the camera made no picture for our reader)", n, (long long)frame);
}

// Where the stream got to, from the counters: one line, and what it means when no picture came.
static void summary(void)
{
  unsigned images = count_of(&ndk.images), started = count_of(&ndk.started), completed = count_of(&ndk.completed), failed = count_of(&ndk.failed), lost = count_of(&ndk.lost);
  if (images && !cam_verbose) return;
  cam_log("ndk: callbacks: session active %u, ready %u, closed %u; captures started %u, completed %u, failed %u; buffers lost %u; device errors %u; images %u (unreadable %u)",
          count_of(&ndk.active), count_of(&ndk.ready), count_of(&ndk.closed), started, completed, failed, lost, count_of(&ndk.device_errors), images, count_of(&ndk.bad_images));
  if (images) return;
  if (count_of(&ndk.device_errors)) cam_log("ndk: stopped at: the device error above");
  else if (!started && !failed && !count_of(&ndk.active))
    cam_log("ndk: stopped at: no callback at all after setRepeatingRequest. cameraserver never began the request, or its calls into this process do not arrive "
            "(binder threads); see `dumpsys media.camera` and logcat (README.md, \"When no frame arrives\")");
  else if (!started && !failed) cam_log("ndk: stopped at: the session went active but the sensor started no capture (the HAL holds the request)");
  else if (failed || lost) cam_log("ndk: stopped at: captures fail or lose their buffer (the lines above have the reason)");
  else if (started && !completed) cam_log("ndk: stopped at: captures start but no result and no picture comes back (the HAL, or the buffer queue)");
  else cam_log("ndk: stopped at: results arrive without pictures: the reader's buffer queue (format, usage or size the HAL does not fill)");
}

static void ndk_teardown(void)
{
  camera_status_t status;
  if (ndk.session) {
    if ((status = ACameraCaptureSession_stopRepeating(ndk.session)) != ACAMERA_OK && cam_verbose) cam_log("ndk: stopRepeating: status %d %s", status, status_name(status));
    ACameraCaptureSession_close(ndk.session);
    ndk.session = NULL;
  }
  if (ndk.request) { ACaptureRequest_free(ndk.request); ndk.request = NULL; }
  if (ndk.target) { ACameraOutputTarget_free(ndk.target); ndk.target = NULL; }
  if (ndk.device) {
    if ((status = ACameraDevice_close(ndk.device)) != ACAMERA_OK) cam_log("ndk: closing the camera: status %d %s", status, status_name(status));
    ndk.device = NULL;
  }
  if (ndk.outputs) { ACaptureSessionOutputContainer_free(ndk.outputs); ndk.outputs = NULL; }
  if (ndk.output) { ACaptureSessionOutput_free(ndk.output); ndk.output = NULL; }
  if (ndk.reader) { AImageReader_delete(ndk.reader); ndk.reader = NULL; }
}

static int ndk_start(const cam_info *camera, int width, int height, int fps_lo, int fps_hi, cam_frame_fn frame, cam_error_fn error, void *ctx)
{
  static ACameraDevice_StateCallbacks device_callbacks = { .onDisconnected = on_disconnected, .onError = on_device_error };
  static ACameraCaptureSession_stateCallbacks session_callbacks = { .onClosed = on_session_closed, .onReady = on_session_ready, .onActive = on_session_active };
  static ACameraCaptureSession_captureCallbacks capture_callbacks = {
    .onCaptureStarted = on_capture_started, .onCaptureCompleted = on_capture_completed, .onCaptureFailed = on_capture_failed,
    .onCaptureSequenceCompleted = on_sequence_completed, .onCaptureSequenceAborted = on_sequence_aborted, .onCaptureBufferLost = on_buffer_lost,
  };
  AImageReader_ImageListener listener = { .onImageAvailable = on_image };
  ANativeWindow *window = NULL;
  int32_t range[2] = { fps_lo, fps_hi };
  // A physical camera behind a logical one: the logical camera is opened, the stream is the physical one's.
  const char *open_id = camera->parent[0] ? camera->parent : camera->id;
  const char *step = "ACameraManager_create";
  camera_status_t status = ACAMERA_ERROR_UNKNOWN;
  media_status_t media = AMEDIA_OK;
  int sequence = -1;
  if (ndk_init()) goto failed;
  ndk.frame = frame; ndk.error = error; ndk.ctx = ctx;
  ndk.active = ndk.ready = ndk.closed = ndk.started = ndk.completed = ndk.failed = ndk.lost = ndk.sequences = ndk.images = ndk.bad_images = ndk.device_errors = 0;
  step = "openCamera";
  if ((status = ACameraManager_openCamera(ndk.manager, open_id, &device_callbacks, &ndk.device)) != ACAMERA_OK) goto failed;
  if (cam_verbose) cam_log("ndk: camera %s opened%s%s", open_id, camera->parent[0] ? " for its physical camera " : "", camera->parent[0] ? camera->id : "");
  // The reader: YUV_420_888 that the CPU reads (the usage the buffer queue tells cameraserver), 4 pictures deep.
  step = "AImageReader_newWithUsage"; status = ACAMERA_ERROR_UNKNOWN;
  if ((media = AImageReader_newWithUsage(width, height, AIMAGE_FORMAT_YUV_420_888, AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN, 4, &ndk.reader)) != AMEDIA_OK) goto failed;
  step = "AImageReader_setImageListener";
  if ((media = AImageReader_setImageListener(ndk.reader, &listener)) != AMEDIA_OK) goto failed;
  step = "AImageReader_getWindow";
  if ((media = AImageReader_getWindow(ndk.reader, &window)) != AMEDIA_OK || !window) goto failed;
  step = camera->parent[0] ? "ACaptureSessionPhysicalOutput_create" : "ACaptureSessionOutput_create";
  if ((status = ACaptureSessionOutputContainer_create(&ndk.outputs)) != ACAMERA_OK) goto failed;
  status = camera->parent[0] ? ACaptureSessionPhysicalOutput_create(window, camera->id, &ndk.output) : ACaptureSessionOutput_create(window, &ndk.output);
  if (status != ACAMERA_OK || (status = ACaptureSessionOutputContainer_add(ndk.outputs, ndk.output)) != ACAMERA_OK) goto failed;
  // The repeating request: the preview template, its one target the reader's window.
  step = "createCaptureRequest(TEMPLATE_PREVIEW)";
  if ((status = ACameraDevice_createCaptureRequest(ndk.device, TEMPLATE_PREVIEW, &ndk.request)) != ACAMERA_OK) goto failed;
  step = "request target";
  if ((status = ACameraOutputTarget_create(window, &ndk.target)) != ACAMERA_OK || (status = ACaptureRequest_addTarget(ndk.request, ndk.target)) != ACAMERA_OK) goto failed;
  if ((status = ACaptureRequest_setEntry_i32(ndk.request, ACAMERA_CONTROL_AE_TARGET_FPS_RANGE, 2, range)) != ACAMERA_OK)
    cam_log("ndk: the request did not take the frame rate %d-%d: status %d %s (the template's rate is used)", fps_lo, fps_hi, status, status_name(status));
  if (cam_ndk_zoom > 0) {
    status = ACaptureRequest_setEntry_float(ndk.request, ACAMERA_CONTROL_ZOOM_RATIO, 1, &cam_ndk_zoom);
    cam_log("ndk: zoom ratio %.2f in the request: status %d %s", (double)cam_ndk_zoom, status, status_name(status));
  }
  pthread_mutex_lock(&ndk.lock);
  ndk.running = true;
  pthread_mutex_unlock(&ndk.lock);
  step = "createCaptureSession";
  if ((status = ACameraDevice_createCaptureSession(ndk.device, ndk.outputs, &session_callbacks, &ndk.session)) != ACAMERA_OK) goto failed;
  if (cam_verbose) cam_log("ndk: session made: one %s YUV_420_888 stream %dx%d, CPU-read usage", camera->parent[0] ? "physical" : "plain", width, height);
  step = "setRepeatingRequest";
  if ((status = ACameraCaptureSession_setRepeatingRequest(ndk.session, &capture_callbacks, 1, &ndk.request, &sequence)) != ACAMERA_OK) goto failed;
  if (cam_verbose) cam_log("ndk: repeating request set: sequence %d, preview template, %d-%d fps", sequence, fps_lo, fps_hi);
  return 0;
failed:
  cam_log("ndk: %s failed for camera %s: status %d %s, media status %d%s", step, camera->id, status, status_name(status), media,
          status == ACAMERA_ERROR_PERMISSION_DENIED ? " (permission denied: cameraserver could not ask system_server; use the hal back end)" : "");
  pthread_mutex_lock(&ndk.lock);
  ndk.running = false;
  pthread_mutex_unlock(&ndk.lock);
  ndk_teardown();
  return -error_of(status);
}

static void ndk_stop(void)
{
  pthread_mutex_lock(&ndk.lock);
  ndk.running = false;                       // from here no frame is handed on
  pthread_mutex_unlock(&ndk.lock);
  summary();
  ndk_teardown();
}

const cam_backend cam_backend_ndk = { "ndk", ndk_list, ndk_start, ndk_stop };
