// SPDX-License-Identifier: MIT
// liboneios-display.so: the native half of OneiOSDisplay (java/OneiOSDisplay.java). It owns the
// socket, the mapped frame and the copy into the layer's buffers; the Java half owns the layers.
#define _GNU_SOURCE
#include "xhd.h"
#include <android/native_window.h>
#include <android/native_window_jni.h>
#include <errno.h>
#include <fcntl.h>
#include <jni.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define FORMAT_BGRA_8888 XHD_BUFFER_BGRA_8888    // HAL_PIXEL_FORMAT_BGRA_8888: the guest's 8-bit bytes as they are
#define FORMAT_RGBX_8888 XHD_BUFFER_RGBX_8888
#define FORMAT_RGBA_FP16 XHD_BUFFER_RGBA_FP16    // the guest's half floats as they are

// ANativeWindow_setBuffersDataSpace and _get are API 28; the library is built for 30, so they are there.
struct context { int fd; struct xhd_frame frame; };

// SIGTERM, SIGINT and SIGHUP end the run loop through a pipe, so the Java half removes the layers.
static int signal_pipe[2] = { -1, -1 };
static volatile int stopping;
static void on_signal(int number)
{
  int saved = errno;
  char byte = (char)number;
  stopping = 1;
  if (signal_pipe[1] >= 0 && write(signal_pipe[1], &byte, 1) < 0) { }
  errno = saved;
}

JNIEXPORT jlong JNICALL Java_OneiOSDisplay_nOpen(JNIEnv *env, jclass type, jstring socket_path, jint wait_ms)
{
  (void)type;
  if (signal_pipe[0] < 0 && !pipe2(signal_pipe, O_CLOEXEC | O_NONBLOCK)) {
    struct sigaction action = { .sa_handler = on_signal, .sa_flags = SA_RESTART };
    sigemptyset(&action.sa_mask);
    sigaction(SIGTERM, &action, NULL);
    sigaction(SIGINT, &action, NULL);
    sigaction(SIGHUP, &action, NULL);
  }
  const char *path = (*env)->GetStringUTFChars(env, socket_path, NULL);
  if (!path) return 0;
  struct context *c = calloc(1, sizeof *c);
  int fd = c ? xhd_connect(path, wait_ms, &stopping) : -ENOMEM;
  if (fd < 0) fprintf(stderr, "oneios-display: %s: %s\n", path, stopping ? "stopped while waiting" : strerror(-fd));
  (*env)->ReleaseStringUTFChars(env, socket_path, path);
  if (fd < 0) { free(c); return 0; }
  c->fd = fd;
  c->frame.memfd = -1;
  return (jlong)(intptr_t)c;
}

// Reports the panel's mode, then takes xnuhost's hello. JNI_FALSE: no usable hello (nClose is still the caller's).
JNIEXPORT jboolean JNICALL Java_OneiOSDisplay_nHello(JNIEnv *env, jclass type, jlong handle, jint width, jint height, jint hz)
{
  (void)env; (void)type;
  struct context *c = (struct context *)(intptr_t)handle;
  int error = xhd_send_panel(c->fd, (uint32_t)width, (uint32_t)height, (uint32_t)hz);
  if (error) fprintf(stderr, "oneios-display: the panel's mode could not be sent: %s\n", strerror(-error));
  error = xhd_recv_hello(c->fd, &c->frame, 10000);
  if (error) fprintf(stderr, "oneios-display: no usable hello from xnuhost: %s\n", strerror(-error));
  return error ? JNI_FALSE : JNI_TRUE;
}

JNIEXPORT jintArray JNICALL Java_OneiOSDisplay_nInfo(JNIEnv *env, jclass type, jlong handle)
{
  (void)type;
  struct context *c = (struct context *)(intptr_t)handle;
  jint values[4] = { (jint)c->frame.width, (jint)c->frame.height, (jint)c->frame.stride, (jint)c->frame.format };
  jintArray array = (*env)->NewIntArray(env, 4);
  if (array) (*env)->SetIntArrayRegion(env, array, 0, 4, values);
  return array;
}

JNIEXPORT jintArray JNICALL Java_OneiOSDisplay_nFit(JNIEnv *env, jclass type, jint sw, jint sh, jint dw, jint dh)
{
  (void)type;
  struct xhd_fit fit;
  xhd_fit((uint32_t)sw, (uint32_t)sh, (uint32_t)dw, (uint32_t)dh, &fit);
  jint values[4] = { (jint)fit.x, (jint)fit.y, (jint)fit.w, (jint)fit.h };
  jintArray array = (*env)->NewIntArray(env, 4);
  if (array) (*env)->SetIntArrayRegion(env, array, 0, 4, values);
  return array;
}

// Posts frames until the socket closes (0) or a signal asks to stop (1); negative on an error.
// Pacing: the next buffer is only handed out when surfaceflinger has taken the last one, so
// ANativeWindow_lock blocks to the panel's vsync, and every flush that came meanwhile is one post.
// The buffer layout and dataspace a frame format gets (xhd_buffer_for): {buffer, dataspace}.
JNIEXPORT jintArray JNICALL Java_OneiOSDisplay_nBuffer(JNIEnv *env, jclass type, jint format)
{
  (void)type;
  int dataspace = 0;
  jint values[2] = { (jint)xhd_buffer_for((uint32_t)format, &dataspace), (jint)dataspace };
  jintArray array = (*env)->NewIntArray(env, 2);
  if (array) (*env)->SetIntArrayRegion(env, array, 0, 2, values);
  return array;
}

// Tags the window's buffers with the dataspace: what the numbers in them mean, so that surfaceflinger turns them into
// the panel's colours (One UI's colour mode decides how; nothing of it is changed here). Logs what the window has.
static void set_dataspace(ANativeWindow *window, int dataspace)
{
  int error = ANativeWindow_setBuffersDataSpace(window, dataspace);
  int now = ANativeWindow_getBuffersDataSpace(window);
  if (error) fprintf(stderr, "oneios-display: the dataspace %s (%#x) was refused (%d); the buffers say %s (%#x)\n",
                     xhd_dataspace_name(dataspace), (unsigned)dataspace, error, xhd_dataspace_name(now), (unsigned)now);
  else fprintf(stderr, "oneios-display: buffers tagged %s (dataspace %#x)\n", xhd_dataspace_name(now), (unsigned)now);
}

static const char *format_name(int format)
{
  return format == FORMAT_BGRA_8888 ? "BGRA_8888" : format == FORMAT_RGBX_8888 ? "RGBX_8888" : format == FORMAT_RGBA_FP16 ? "RGBA_FP16" : "other";
}

JNIEXPORT jint JNICALL Java_OneiOSDisplay_nRun(JNIEnv *env, jclass type, jlong handle, jobject surface, jint format, jint dataspace)
{
  (void)type;
  struct context *c = (struct context *)(intptr_t)handle;
  const struct xhd_frame *f = &c->frame;
  ANativeWindow *window = ANativeWindow_fromSurface(env, surface);
  if (!window) { fprintf(stderr, "oneios-display: no native window for the layer\n"); return -1; }
  ANativeWindow_setBuffersGeometry(window, (int32_t)f->width, (int32_t)f->height, format);
  set_dataspace(window, dataspace);
  struct xhd_rect pending = { 0, 0, f->width, f->height };
  unsigned long frames = 0;
  int result;
  for (;;) {
    struct pollfd fds[2] = { { c->fd, POLLIN, 0 }, { signal_pipe[0], POLLIN, 0 } };
    int ready = poll(fds, 2, pending.w ? 0 : -1);
    if (ready < 0 && errno != EINTR) { result = -3; break; }
    if (stopping || (ready > 0 && fds[1].revents)) { result = 1; break; }
    if (xhd_drain(c->fd, f, &pending) < 0) { result = 0; break; }
    if (!pending.w) continue;
    // The first posts fill whole buffers; after that the window copies back what the last frame
    // had and hands out the rectangle that has to be drawn (it may be larger than what was asked).
    ARect dirty = { (int32_t)pending.x, (int32_t)pending.y, (int32_t)(pending.x + pending.w), (int32_t)(pending.y + pending.h) };
    if (frames < 4) dirty = (ARect){ 0, 0, (int32_t)f->width, (int32_t)f->height };
    ANativeWindow_Buffer buffer;
    int error = ANativeWindow_lock(window, &buffer, &dirty);
    if (error) {
      if (frames == 0 && format != FORMAT_RGBX_8888) {
        // No B,G,R,A buffers here: R,G,B,X and a swap. No RGBA_FP16 ones: B,G,R,A, the half floats encoded to sRGB
        // and clamped (what is outside sRGB is lost), tagged sRGB.
        int next = format == FORMAT_RGBA_FP16 ? FORMAT_BGRA_8888 : FORMAT_RGBX_8888;
        fprintf(stderr, "oneios-display: no buffer in format %d %s (%d), using %s\n", format, format_name(format), error, format_name(next));
        format = next;
        ANativeWindow_setBuffersGeometry(window, (int32_t)f->width, (int32_t)f->height, format);
        if (dataspace == XHD_DATASPACE_SCRGB_LINEAR || dataspace == XHD_DATASPACE_SCRGB) set_dataspace(window, dataspace = XHD_DATASPACE_SRGB);
        continue;
      }
      fprintf(stderr, "oneios-display: the layer's buffer could not be locked (%d)\n", error);
      result = -2;
      break;
    }
    struct xhd_rect r = { 0, 0, 0, 0 };
    if (dirty.left < 0) dirty.left = 0;
    if (dirty.top < 0) dirty.top = 0;
    if (dirty.right > dirty.left && dirty.bottom > dirty.top) {
      r = (struct xhd_rect){ (uint32_t)dirty.left, (uint32_t)dirty.top, (uint32_t)(dirty.right - dirty.left), (uint32_t)(dirty.bottom - dirty.top) };
      uint32_t width = (uint32_t)buffer.width < f->width ? (uint32_t)buffer.width : f->width;
      uint32_t height = (uint32_t)buffer.height < f->height ? (uint32_t)buffer.height : f->height;
      // The buffer's own format decides the copy (RGBA_8888 and RGBX_8888 are both R,G,B bytes).
      int layout = buffer.format == FORMAT_BGRA_8888 || buffer.format == FORMAT_RGBA_FP16 ? buffer.format : FORMAT_RGBX_8888;
      size_t bytes = layout == FORMAT_RGBA_FP16 ? 8 : 4;
      if (buffer.width > 0 && buffer.height > 0 && buffer.stride >= buffer.width && xhd_rect_clip(&r, width, height))
        xhd_copy(buffer.bits, (size_t)buffer.stride * bytes, layout, f->pixels, f->stride, f->format, r);
    }
    ANativeWindow_unlockAndPost(window);
    if (frames++ == 0)
      fprintf(stderr, "oneios-display: first frame posted (%dx%d buffer, stride %d, format %d %s, dataspace %#x %s; the guest's frame: %s)\n",
              buffer.width, buffer.height, buffer.stride, buffer.format, format_name(buffer.format), (unsigned)ANativeWindow_getBuffersDataSpace(window),
              xhd_dataspace_name(ANativeWindow_getBuffersDataSpace(window)), xh_pixels_name(f->format));
    pending = (struct xhd_rect){ 0, 0, 0, 0 };
  }
  ANativeWindow_release(window);
  fprintf(stderr, "oneios-display: %s after %lu frames\n", result == 0 ? "the socket closed" : result == 1 ? "asked to stop" : "error", frames);
  return result;
}

JNIEXPORT void JNICALL Java_OneiOSDisplay_nClose(JNIEnv *env, jclass type, jlong handle)
{
  (void)env; (void)type;
  struct context *c = (struct context *)(intptr_t)handle;
  if (!c) return;
  close(c->fd);
  xhd_frame_close(&c->frame);
  free(c);
}
