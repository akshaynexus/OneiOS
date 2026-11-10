// SPDX-License-Identifier: MIT
// The Camera2 NDK back end: ACameraManager through cameraserver ("media.camera"). The provider HAL
// does the ISP work, so the pictures arrive finished (YUV_420_888).
//
// This is the way with One UI's framework running. In a session it is expected NOT to work
// (README.md, "Which route"): cameraserver asks system_server's permission_checker service for
// android.permission.CAMERA for every client outside its own process, waits 10 s for the service
// and then denies. backend_hal.c is the way there.
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
} ndk = { .lock = PTHREAD_MUTEX_INITIALIZER };

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

static int ndk_list(cam_info *out, int max)
{
  if (!ndk.manager) ndk.manager = ACameraManager_create();
  ACameraIdList *ids = NULL;
  camera_status_t status = ndk.manager ? ACameraManager_getCameraIdList(ndk.manager, &ids) : ACAMERA_ERROR_UNKNOWN;
  if (status != ACAMERA_OK || !ids) { cam_log("ndk: no camera list from cameraserver (status %d)", status); return -error_of(status); }
  int count = 0;
  for (int i = 0; i < ids->numCameras && count < max; i++) {
    ACameraMetadata *meta = NULL;
    if (ACameraManager_getCameraCharacteristics(ndk.manager, ids->cameraIds[i], &meta) != ACAMERA_OK || !meta) continue;
    cam_info *c = &out[count++];
    memset(c, 0, sizeof *c);
    snprintf(c->id, sizeof c->id, "%s", ids->cameraIds[i]);
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
    ACameraMetadata_free(meta);
  }
  ACameraManager_deleteCameraIdList(ids);
  return count;
}

static void on_image(void *context, AImageReader *reader)
{
  (void)context;
  AImage *image = NULL;
  if (AImageReader_acquireLatestImage(reader, &image) != AMEDIA_OK || !image) return;
  cam_yuv picture = { 0 };
  uint8_t *y = NULL, *cb = NULL, *cr = NULL;
  int length = 0, step = 0;
  int32_t width = 0, height = 0, y_stride = 0, c_stride = 0;
  int64_t time = 0;
  bool ok = AImage_getPlaneData(image, 0, &y, &length) == AMEDIA_OK && AImage_getPlaneData(image, 1, &cb, &length) == AMEDIA_OK &&
            AImage_getPlaneData(image, 2, &cr, &length) == AMEDIA_OK && AImage_getPlaneRowStride(image, 0, &y_stride) == AMEDIA_OK &&
            AImage_getPlaneRowStride(image, 1, &c_stride) == AMEDIA_OK && AImage_getPlanePixelStride(image, 1, &step) == AMEDIA_OK &&
            AImage_getWidth(image, &width) == AMEDIA_OK && AImage_getHeight(image, &height) == AMEDIA_OK;
  (void)AImage_getTimestamp(image, &time);
  if (ok) {
    picture = (cam_yuv){ .y = y, .cb = cb, .cr = cr, .y_stride = y_stride, .c_stride = c_stride, .c_step = step, .width = width, .height = height };
    pthread_mutex_lock(&ndk.lock);
    if (ndk.running) ndk.frame(ndk.ctx, &picture, (uint64_t)time);
    pthread_mutex_unlock(&ndk.lock);
  }
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
static void on_disconnected(void *context, ACameraDevice *device) { (void)context; (void)device; report(ENODEV); }
static void on_device_error(void *context, ACameraDevice *device, int error) { (void)context; (void)device; cam_log("ndk: device error %d", error); report(EIO); }
static void on_session(void *context, ACameraCaptureSession *session) { (void)context; (void)session; }

static void ndk_teardown(void)
{
  if (ndk.session) { (void)ACameraCaptureSession_stopRepeating(ndk.session); ACameraCaptureSession_close(ndk.session); ndk.session = NULL; }
  if (ndk.request) { ACaptureRequest_free(ndk.request); ndk.request = NULL; }
  if (ndk.target) { ACameraOutputTarget_free(ndk.target); ndk.target = NULL; }
  if (ndk.device) { (void)ACameraDevice_close(ndk.device); ndk.device = NULL; }
  if (ndk.outputs) { ACaptureSessionOutputContainer_free(ndk.outputs); ndk.outputs = NULL; }
  if (ndk.output) { ACaptureSessionOutput_free(ndk.output); ndk.output = NULL; }
  if (ndk.reader) { AImageReader_delete(ndk.reader); ndk.reader = NULL; }
}

static int ndk_start(const cam_info *camera, int width, int height, int fps_lo, int fps_hi, cam_frame_fn frame, cam_error_fn error, void *ctx)
{
  static ACameraDevice_StateCallbacks device_callbacks = { .onDisconnected = on_disconnected, .onError = on_device_error };
  static ACameraCaptureSession_stateCallbacks session_callbacks = { .onClosed = on_session, .onReady = on_session, .onActive = on_session };
  AImageReader_ImageListener listener = { .onImageAvailable = on_image };
  ANativeWindow *window = NULL;
  int32_t range[2] = { fps_lo, fps_hi };
  const char *step = "ACameraManager_create";
  camera_status_t status = ACAMERA_ERROR_UNKNOWN;
  if (!ndk.manager) ndk.manager = ACameraManager_create();
  if (!ndk.manager) goto failed;
  ndk.frame = frame; ndk.error = error; ndk.ctx = ctx;
  step = "openCamera";
  if ((status = ACameraManager_openCamera(ndk.manager, camera->id, &device_callbacks, &ndk.device)) != ACAMERA_OK) goto failed;
  step = "AImageReader_new"; status = ACAMERA_ERROR_UNKNOWN;
  if (AImageReader_new(width, height, AIMAGE_FORMAT_YUV_420_888, 4, &ndk.reader) != AMEDIA_OK || AImageReader_setImageListener(ndk.reader, &listener) != AMEDIA_OK ||
      AImageReader_getWindow(ndk.reader, &window) != AMEDIA_OK) goto failed;
  step = "session output";
  if ((status = ACaptureSessionOutputContainer_create(&ndk.outputs)) != ACAMERA_OK || (status = ACaptureSessionOutput_create(window, &ndk.output)) != ACAMERA_OK ||
      (status = ACaptureSessionOutputContainer_add(ndk.outputs, ndk.output)) != ACAMERA_OK) goto failed;
  step = "createCaptureRequest";
  if ((status = ACameraDevice_createCaptureRequest(ndk.device, TEMPLATE_PREVIEW, &ndk.request)) != ACAMERA_OK || (status = ACameraOutputTarget_create(window, &ndk.target)) != ACAMERA_OK ||
      (status = ACaptureRequest_addTarget(ndk.request, ndk.target)) != ACAMERA_OK) goto failed;
  (void)ACaptureRequest_setEntry_i32(ndk.request, ACAMERA_CONTROL_AE_TARGET_FPS_RANGE, 2, range);
  pthread_mutex_lock(&ndk.lock);
  ndk.running = true;
  pthread_mutex_unlock(&ndk.lock);
  step = "createCaptureSession";
  if ((status = ACameraDevice_createCaptureSession(ndk.device, ndk.outputs, &session_callbacks, &ndk.session)) != ACAMERA_OK) goto failed;
  step = "setRepeatingRequest";
  if ((status = ACameraCaptureSession_setRepeatingRequest(ndk.session, NULL, 1, &ndk.request, NULL)) != ACAMERA_OK) goto failed;
  return 0;
failed:
  cam_log("ndk: %s failed for camera %s: status %d%s", step, camera->id, status,
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
  ndk_teardown();
}

const cam_backend cam_backend_ndk = { "ndk", ndk_list, ndk_start, ndk_stop };
