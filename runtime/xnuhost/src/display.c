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
#include <poll.h>
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
//   hello  { "XHD1", u32 width, u32 height, u32 stride, u32 format } + the memfd (SCM_RIGHTS); format 1: B,G,R,X 8-bit
//          sRGB, 2: R,G,B,A half floats, linear extended sRGB (include/xnuhost/pixels.h, display-format=)
//   flush  { "XHDF", u32 x, u32 y, u32 w, u32 h }
// and from the helper, once, straight after it connects, whatever xnuhost does with it:
//   panel  { "XHDP", u32 width, u32 height, u32 hz, u32 0 }   the mode surfaceflinger runs the panel in
struct shm_message { char magic[4]; uint32_t a, b, c, d; };
static int shm_fd = -1, listen_fd = -1, client_fd = -1, first_client = -1;
static pthread_mutex_t client_lock = PTHREAD_MUTEX_INITIALIZER;
static char socket_path[600];

static void helper_attach(int fd)
{
  struct shm_message hello = { { 'X', 'H', 'D', '1' }, display.width, display.height, display.stride, display.format };
  char control[CMSG_SPACE(sizeof(int))];
  struct iovec iov = { &hello, sizeof hello };
  struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof control };
  memset(control, 0, sizeof control);
  struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
  cm->cmsg_level = SOL_SOCKET;
  cm->cmsg_type = SCM_RIGHTS;
  cm->cmsg_len = CMSG_LEN(sizeof(int));
  memcpy(CMSG_DATA(cm), &shm_fd, sizeof(int));
  if (sendmsg(fd, &msg, MSG_NOSIGNAL) < 0) { close(fd); return; }
  pthread_mutex_lock(&client_lock);
  if (client_fd >= 0) close(client_fd);          // the newest helper wins
  client_fd = fd;
  pthread_mutex_unlock(&client_lock);
  LOGI("display: a helper connected");
  xh_display_flush(0, 0, display.width, display.height);
}

static void *accept_main(void *arg)
{
  (void)arg;
  for (;;) {
    int fd = accept4(listen_fd, NULL, NULL, SOCK_CLOEXEC);
    if (fd < 0) { if (errno == EINTR) continue; return NULL; }
    helper_attach(fd);
  }
}

static int wait_readable(int fd, int timeout_ms)
{
  struct pollfd p = { fd, POLLIN, 0 };
  int ready;
  do ready = poll(&p, 1, timeout_ms); while (ready < 0 && errno == EINTR);
  return ready;
}

// The socket first: the helper that reports the panel's mode connects to it before the frame has a size.
static int shm_listen(const xh_config *c)
{
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
  return 0;
}

// The mode surfaceflinger runs the panel in, from the helper the fork's switch starts beside xnuhost. Bounded: a
// session without a helper shows nothing anyway, and must still boot. A helper that connected but reports nothing
// keeps its connection and gets the hello all the same.
#define HELPER_CONNECT_MS 10000
#define HELPER_REPORT_MS  8000                      // under the 10 s a helper waits for the hello
static bool helper_report(xh_panel_mode *panel)
{
  if (listen_fd < 0) return false;
  if (wait_readable(listen_fd, HELPER_CONNECT_MS) <= 0) { LOGI("display: no helper connected within %d s", HELPER_CONNECT_MS / 1000); return false; }
  first_client = accept4(listen_fd, NULL, NULL, SOCK_CLOEXEC);
  if (first_client < 0) return false;
  if (wait_readable(first_client, HELPER_REPORT_MS) <= 0) { LOGI("display: the helper did not report the panel's mode"); return false; }
  struct shm_message m;
  ssize_t n = recv(first_client, &m, sizeof m, MSG_DONTWAIT);
  if (n >= 0 && xh_panel_report(&m, (size_t)n, panel)) return true;
  LOGI("display: the helper's report of the panel's mode was not usable");
  return false;
}

static int shm_frame(void)
{
  size_t bytes = (size_t)display.stride * display.height;
  shm_fd = (int)syscall(SYS_memfd_create, "xnuhost-frame", 1u /* MFD_CLOEXEC */);
  if (shm_fd < 0 || ftruncate(shm_fd, (off_t)bytes)) return -1;
  display.pixels = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, shm_fd, 0);
  if (display.pixels == MAP_FAILED) { display.pixels = NULL; return -1; }
  if (first_client >= 0) { helper_attach(first_client); first_client = -1; }
  if (listen_fd < 0) return 0;
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

// The panel's largest mode, without taking the display: the modes of the connected built-in panel's connector are
// read (no DRM master is needed for that, and nothing is set). Only a fallback in a session: this list need not
// hold the mode surfaceflinger runs (the helper's report does), and the Virtual connectors have to be skipped.
static bool panel_native(xh_panel_mode *native)
{
  int fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
  if (fd < 0) return false;
  bool found = false;
  uint32_t connectors[16];
  struct drm_mode_card_res res = { .connector_id_ptr = (uintptr_t)connectors, .count_connectors = 16 };
  if (!ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res))
    for (uint32_t i = 0; !found && i < res.count_connectors && i < 16; i++) {
      static struct drm_mode_modeinfo modes[256];      // the kernel copies none of them if they do not all fit
      struct drm_mode_get_connector conn = { .connector_id = connectors[i], .modes_ptr = (uintptr_t)modes, .count_modes = 256 };
      if (ioctl(fd, DRM_IOCTL_MODE_GETCONNECTOR, &conn) || conn.connection != 1 || !xh_panel_connector(conn.connector_type) || conn.count_modes > 256) continue;
      xh_panel_mode list[256];
      unsigned n = 0;
      for (uint32_t m = 0; m < conn.count_modes; m++) {
        unsigned hz = modes[m].vrefresh;
        if (!hz && modes[m].htotal && modes[m].vtotal) hz = (unsigned)(((uint64_t)modes[m].clock * 1000 + (uint64_t)modes[m].htotal * modes[m].vtotal / 2) / ((uint64_t)modes[m].htotal * modes[m].vtotal));
        list[n++] = (xh_panel_mode){ modes[m].hdisplay, modes[m].vdisplay, hz };
      }
      found = xh_panel_native(list, n, native);
    }
  close(fd);
  return found;
}

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
static bool panel_native(xh_panel_mode *native) { (void)native; return false; }
#endif

int xh_display_init(const xh_config *c, unsigned *width, unsigned *height, unsigned *hz)
{
  backend = !strcmp(c->display, "drm") ? BACK_DRM : !strcmp(c->display, "shm") ? BACK_SHM : BACK_NONE;
  if (backend == BACK_DRM && drm_init()) return -1;
  if (backend == BACK_SHM && shm_listen(c)) return -1;
  // The guest's frame: what is configured, else the mode the display helper reports (surfaceflinger's, after its
  // --takeover has asked for the native one), else the largest of the built-in panel's DRM modes, else 1440x3120
  // at 120 Hz, the Galaxy S24 Ultra's. With the KMS back end, the mode that was set.
  xh_panel_mode panel = { 0, 0, 0 };
  // Only a session reads the host's panel: a bench run's host display is somebody's monitor.
  bool session = !strcmp(c->mode, "takeover"), pinned = c->width && c->height && c->hz;
  bool reported = false, known = false;
  if (backend == BACK_DRM) { panel = (xh_panel_mode){ drm_w, drm_h, 0 }; known = true; }
  else if (session && !pinned) {
    reported = backend == BACK_SHM && helper_report(&panel);
    known = reported || panel_native(&panel);
  }
  xh_panel_mode mode = xh_display_mode(c, known ? &panel : NULL);
  display.width = mode.width;
  display.height = mode.height;
  *hz = mode.hz;
  display.format = c->display_format ? c->display_format : XH_PIXELS_BGRX8;
  display.bpp = xh_pixels_bpp(display.format);
  display.stride = display.width * display.bpp;
  if (backend == BACK_SHM) {
    if (shm_frame()) { LOGE("display: shared frame: %s", strerror(errno)); return -1; }
  } else {
    display.pixels = calloc((size_t)display.stride, display.height);
    if (!display.pixels) return -1;
  }
  *width = display.width;
  *height = display.height;
  if (pinned && session) LOGI("display: the mode is the configured one (width, height, hz); the panel's was not asked for");
  else if (reported) LOGI("display: the display helper reports the panel at %ux%u, %u Hz", panel.width, panel.height, panel.hz);
  else if (known) LOGI("display: the built-in panel's largest DRM mode is %ux%u at %u Hz", panel.width, panel.height, panel.hz);
  else LOGI("display: the panel's modes were not read (%s): %ux%u at %u Hz assumed", session ? "no helper's report and no connected built-in panel in /dev/dri/card0" : "a bench run", XH_PANEL_WIDTH, XH_PANEL_HEIGHT, XH_PANEL_HZ);
  LOGI("display: %s, guest frame %ux%u at %u Hz, @%ux (%ux%u points)", c->display, display.width, display.height, mode.hz, c->scale,
       c->scale ? display.width / c->scale : 0, c->scale ? display.height / c->scale : 0);
  LOGI("display: the frame's pixels: %s, %u bytes a pixel, stride %u (display-format=%s)", xh_pixels_name(display.format), display.bpp,
       display.stride, display.format == XH_PIXELS_RGBA16F ? "rgba16f" : "bgrx");
  return 0;
}

void xh_display_flush(unsigned x, unsigned y, unsigned w, unsigned h)
{
  xh_input_frame_shown();                        // input.c's touch-to-frame measurement
  if (backend == BACK_DRM) drm_flush(x, y, w, h);
  if (backend != BACK_SHM) return;
  pthread_mutex_lock(&client_lock);
  if (client_fd >= 0) {
    struct shm_message m = { { 'X', 'H', 'D', 'F' }, x, y, w, h };
    // Only a helper that is gone loses its socket. A full queue (EAGAIN) or a passing shortage (ENOBUFS, ENOMEM)
    // drops this flush only: the next one names the damage again. On 2026-11-12 (15:35 session) the helper's socket
    // was closed after 7 frames with no line saying why, and the panel stayed dark while iOS drew.
    if (send(client_fd, &m, sizeof m, MSG_NOSIGNAL | MSG_DONTWAIT) < 0 && errno != EAGAIN && errno != EINTR) {
      if (errno == EPIPE || errno == ECONNRESET || errno == ENOTCONN) {
        LOGI("display: the helper's socket is gone (%s): closed", strerror(errno));
        close(client_fd); client_fd = -1;
      } else {
        static unsigned dropped;
        if (dropped++ % 1000 == 0) LOGI("display: a flush to the helper failed (%s): dropped, the socket kept (%u so far)", strerror(errno), dropped);
      }
    }
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
