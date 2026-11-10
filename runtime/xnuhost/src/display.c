// SPDX-License-Identifier: MIT
// Where the guest's frames go. Three back ends (docs/booter.md, "Display and input"):
//   shm   The frame lives in a memfd. A display helper connects to <session>/display.sock, receives
//         the memfd and the geometry, and is told of every changed rectangle. In a OneOS takeover
//         session One UI's surfaceflinger and composer keep running and hold the panel, so a root
//         SurfaceControl layer (OneOS's boot-status helper pattern) is the only way to the glass;
//         the helper owns that layer and copies from the memfd. This is the default.
//   drm   Direct KMS scanout of a dumb buffer on /dev/dri/card0, for a session in which
//         surfaceflinger and the composer are stopped (xnuhost is then the DRM master).
//   none  The frame is kept in memory only (tests, headless bring-up).
#define _GNU_SOURCE
#include "xnuhost.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <unistd.h>

static xh_display display;
static enum { BACK_NONE, BACK_SHM, BACK_DRM } backend;

xh_display *xh_display_get(void) { return display.pixels ? &display : NULL; }

// ---- shm ----------------------------------------------------------------------------------------
// Wire format, little-endian, one SOCK_SEQPACKET message each:
//   hello  { "XHD1", u32 width, u32 height, u32 stride, u32 format = 1 (B,G,R,X bytes) } + the memfd (SCM_RIGHTS)
//   flush  { "XHDF", u32 x, u32 y, u32 w, u32 h }
struct shm_message { char magic[4]; uint32_t a, b, c, d; };
static int shm_fd = -1, listen_fd = -1, client_fd = -1;
static pthread_mutex_t client_lock = PTHREAD_MUTEX_INITIALIZER;
static char socket_path[600];

static void *accept_main(void *arg)
{
  (void)arg;
  for (;;) {
    int fd = accept4(listen_fd, NULL, NULL, SOCK_CLOEXEC);
    if (fd < 0) { if (errno == EINTR) continue; return NULL; }
    struct shm_message hello = { { 'X', 'H', 'D', '1' }, display.width, display.height, display.stride, 1 };
    char control[CMSG_SPACE(sizeof(int))];
    struct iovec iov = { &hello, sizeof hello };
    struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof control };
    memset(control, 0, sizeof control);
    struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
    cm->cmsg_level = SOL_SOCKET;
    cm->cmsg_type = SCM_RIGHTS;
    cm->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cm), &shm_fd, sizeof(int));
    if (sendmsg(fd, &msg, MSG_NOSIGNAL) < 0) { close(fd); continue; }
    pthread_mutex_lock(&client_lock);
    if (client_fd >= 0) close(client_fd);          // the newest helper wins
    client_fd = fd;
    pthread_mutex_unlock(&client_lock);
    LOGI("display: a helper connected");
    xh_display_flush(0, 0, display.width, display.height);
  }
}

static int shm_init(const xh_config *c)
{
  size_t bytes = (size_t)display.stride * display.height;
  shm_fd = (int)syscall(SYS_memfd_create, "xnuhost-frame", 1u /* MFD_CLOEXEC */);
  if (shm_fd < 0 || ftruncate(shm_fd, (off_t)bytes)) return -1;
  display.pixels = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, shm_fd, 0);
  if (display.pixels == MAP_FAILED) { display.pixels = NULL; return -1; }
  if (!*c->session) return 0;                       // no session folder: nobody to connect
  struct sockaddr_un addr = { .sun_family = AF_UNIX };
  snprintf(socket_path, sizeof socket_path, "%s/display.sock", c->session);
  if (strlen(socket_path) >= sizeof addr.sun_path) { LOGE("display: session path too long for a socket"); return -1; }
  strcpy(addr.sun_path, socket_path);
  unlink(socket_path);
  listen_fd = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
  if (listen_fd < 0 || bind(listen_fd, (struct sockaddr *)&addr, sizeof addr) || listen(listen_fd, 2)) {
    LOGE("display: %s: %s", socket_path, strerror(errno));
    return -1;
  }
  pthread_t thread;
  return pthread_create(&thread, NULL, accept_main, NULL) ? -1 : 0;
}

// ---- drm ----------------------------------------------------------------------------------------
#if defined(__linux__) && __has_include(<drm/drm.h>)
#include <drm/drm.h>
#include <drm/drm_mode.h>
static int drm_fd = -1;
static uint32_t drm_fb;
static uint8_t *drm_map;
static unsigned drm_w, drm_h, drm_stride;

static int drm_init(void)
{
  drm_fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
  if (drm_fd < 0) { LOGE("display: /dev/dri/card0: %s", strerror(errno)); return -1; }
  if (ioctl(drm_fd, DRM_IOCTL_SET_MASTER, 0)) { LOGE("display: not the DRM master (is surfaceflinger's composer still running?): %s", strerror(errno)); return -1; }
  uint32_t crtcs[16], connectors[16], encoders[16];
  struct drm_mode_card_res res = { .crtc_id_ptr = (uintptr_t)crtcs, .count_crtcs = 16, .connector_id_ptr = (uintptr_t)connectors,
                                   .count_connectors = 16, .encoder_id_ptr = (uintptr_t)encoders, .count_encoders = 16 };
  if (ioctl(drm_fd, DRM_IOCTL_MODE_GETRESOURCES, &res)) { LOGE("display: KMS resources: %s", strerror(errno)); return -1; }
  for (uint32_t i = 0; i < res.count_connectors && i < 16; i++) {
    struct drm_mode_modeinfo modes[32];
    uint32_t conn_encoders[8];
    struct drm_mode_get_connector conn = { .connector_id = connectors[i], .modes_ptr = (uintptr_t)modes, .count_modes = 32,
                                           .encoders_ptr = (uintptr_t)conn_encoders, .count_encoders = 8 };
    if (ioctl(drm_fd, DRM_IOCTL_MODE_GETCONNECTOR, &conn) || conn.connection != 1 || conn.count_modes == 0) continue;
    struct drm_mode_modeinfo mode = modes[0];
    for (uint32_t m = 0; m < conn.count_modes && m < 32; m++) if (modes[m].type & DRM_MODE_TYPE_PREFERRED) { mode = modes[m]; break; }
    struct drm_mode_get_encoder enc = { .encoder_id = conn.encoder_id ? conn.encoder_id : conn_encoders[0] };
    if (ioctl(drm_fd, DRM_IOCTL_MODE_GETENCODER, &enc)) continue;
    uint32_t crtc = enc.crtc_id;
    for (uint32_t k = 0; !crtc && k < res.count_crtcs && k < 16; k++) if (enc.possible_crtcs & (1u << k)) crtc = crtcs[k];
    if (!crtc) continue;
    struct drm_mode_create_dumb dumb = { .width = mode.hdisplay, .height = mode.vdisplay, .bpp = 32 };
    if (ioctl(drm_fd, DRM_IOCTL_MODE_CREATE_DUMB, &dumb)) { LOGE("display: dumb buffer: %s", strerror(errno)); return -1; }
    struct drm_mode_fb_cmd fb = { .width = dumb.width, .height = dumb.height, .pitch = dumb.pitch, .bpp = 32, .depth = 24, .handle = dumb.handle };
    struct drm_mode_map_dumb map = { .handle = dumb.handle };
    if (ioctl(drm_fd, DRM_IOCTL_MODE_ADDFB, &fb) || ioctl(drm_fd, DRM_IOCTL_MODE_MAP_DUMB, &map)) { LOGE("display: framebuffer: %s", strerror(errno)); return -1; }
    drm_map = mmap(NULL, dumb.size, PROT_READ | PROT_WRITE, MAP_SHARED, drm_fd, (off_t)map.offset);
    if (drm_map == MAP_FAILED) return -1;
    memset(drm_map, 0, dumb.size);
    struct drm_mode_crtc set = { .crtc_id = crtc, .fb_id = fb.fb_id, .set_connectors_ptr = (uintptr_t)&connectors[i], .count_connectors = 1,
                                 .mode = mode, .mode_valid = 1 };
    if (ioctl(drm_fd, DRM_IOCTL_MODE_SETCRTC, &set)) { LOGE("display: mode set %ux%u: %s", mode.hdisplay, mode.vdisplay, strerror(errno)); return -1; }
    drm_fb = fb.fb_id; drm_w = dumb.width; drm_h = dumb.height; drm_stride = dumb.pitch;
    LOGI("display: KMS %ux%u on connector %u, crtc %u", drm_w, drm_h, connectors[i], crtc);
    return 0;
  }
  LOGE("display: no connected KMS connector");
  return -1;
}

static void drm_flush(unsigned x, unsigned y, unsigned w, unsigned h)
{
  // The guest frame is copied to the panel's buffer, centred if their sizes differ.
  unsigned ox = drm_w > display.width ? (drm_w - display.width) / 2 : 0, oy = drm_h > display.height ? (drm_h - display.height) / 2 : 0;
  for (unsigned row = y; row < y + h && row + oy < drm_h; row++) {
    unsigned width = x + w + ox <= drm_w ? w : (x + ox < drm_w ? drm_w - x - ox : 0);
    memcpy(drm_map + (size_t)(row + oy) * drm_stride + (size_t)(x + ox) * 4, display.pixels + (size_t)row * display.stride + (size_t)x * 4, (size_t)width * 4);
  }
  struct drm_clip_rect clip = { (unsigned short)(x + ox), (unsigned short)(y + oy), (unsigned short)(x + ox + w), (unsigned short)(y + oy + h) };
  struct drm_mode_fb_dirty_cmd dirty = { .fb_id = drm_fb, .clips_ptr = (uintptr_t)&clip, .num_clips = 1 };
  ioctl(drm_fd, DRM_IOCTL_MODE_DIRTYFB, &dirty);    // command-mode panels need it; others ignore it
}
#else
static int drm_init(void) { LOGE("display: built without KMS support"); return -1; }
static void drm_flush(unsigned x, unsigned y, unsigned w, unsigned h) { (void)x; (void)y; (void)w; (void)h; }
static unsigned drm_w, drm_h;
#endif

int xh_display_init(const xh_config *c, unsigned *width, unsigned *height)
{
  backend = !strcmp(c->display, "drm") ? BACK_DRM : !strcmp(c->display, "shm") ? BACK_SHM : BACK_NONE;
  if (backend == BACK_DRM && drm_init()) return -1;
  // The guest's frame: what was asked for, else the panel's mode, else the S24 Ultra's panel.
  display.width = c->width ? c->width : backend == BACK_DRM ? drm_w : 1440;
  display.height = c->height ? c->height : backend == BACK_DRM ? drm_h : 3120;
  display.stride = display.width * 4;
  if (backend == BACK_SHM) {
    if (shm_init(c)) { LOGE("display: shared frame: %s", strerror(errno)); return -1; }
  } else {
    display.pixels = calloc((size_t)display.stride, display.height);
    if (!display.pixels) return -1;
  }
  *width = display.width;
  *height = display.height;
  LOGI("display: %s, guest frame %ux%u", c->display, display.width, display.height);
  return 0;
}

void xh_display_flush(unsigned x, unsigned y, unsigned w, unsigned h)
{
  if (backend == BACK_DRM) drm_flush(x, y, w, h);
  if (backend != BACK_SHM) return;
  pthread_mutex_lock(&client_lock);
  if (client_fd >= 0) {
    struct shm_message m = { { 'X', 'H', 'D', 'F' }, x, y, w, h };
    if (send(client_fd, &m, sizeof m, MSG_NOSIGNAL | MSG_DONTWAIT) < 0 && errno != EAGAIN) { close(client_fd); client_fd = -1; }
  }
  pthread_mutex_unlock(&client_lock);
}

void xh_display_close(void)
{
  if (listen_fd >= 0) { close(listen_fd); unlink(socket_path); }
#if defined(__linux__) && __has_include(<drm/drm.h>)
  if (drm_fd >= 0) { ioctl(drm_fd, DRM_IOCTL_DROP_MASTER, 0); close(drm_fd); }
#endif
}
