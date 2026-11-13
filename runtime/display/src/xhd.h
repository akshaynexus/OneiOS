// SPDX-License-Identifier: MIT
// The display helper's portable core: xnuhost's display.sock protocol (runtime/xnuhost/src/display.c,
// docs/booter.md 2.8), the letterbox fit, and the row copy. No Android here, so the host tests run it.
#ifndef ONEIOS_XHD_H
#define ONEIOS_XHD_H
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

#include "../../xnuhost/include/xnuhost/pixels.h"

// The hello's format: what the frame's pixels hold (xnuhost's pixels.h says it in full).
#define XHD_FORMAT_BGRX    XH_PIXELS_BGRX8      // bytes B,G,R,X, 8-bit sRGB: iOS's 'BGRA' framebuffer
#define XHD_FORMAT_RGBA16F XH_PIXELS_RGBA16F    // R,G,B,A half floats, linear extended sRGB: iOS's 'RGhA'
#define XHD_MAX_SIDE 16384u

// The layer's buffer layouts the copy writes (Android's AHARDWAREBUFFER_FORMAT_* values).
#define XHD_BUFFER_RGBX_8888 2                   // bytes R,G,B,X
#define XHD_BUFFER_BGRA_8888 5                   // bytes B,G,R,A
#define XHD_BUFFER_RGBA_FP16 22                  // R,G,B,A half floats

// Android's dataspaces (ADataSpace) the helper tags the buffers with.
#define XHD_DATASPACE_UNKNOWN      0
#define XHD_DATASPACE_SRGB         142671872     // STANDARD_BT709 | TRANSFER_SRGB | RANGE_FULL
#define XHD_DATASPACE_DISPLAY_P3   143261696     // STANDARD_DCI_P3 | TRANSFER_SRGB | RANGE_FULL
#define XHD_DATASPACE_SCRGB        411107328     // STANDARD_BT709 | TRANSFER_SRGB | RANGE_EXTENDED
#define XHD_DATASPACE_SCRGB_LINEAR 406913024     // STANDARD_BT709 | TRANSFER_LINEAR | RANGE_EXTENDED
// The buffer and dataspace for a frame format: B,G,R,A with sRGB for 8-bit frames (the colours iOS composited them
// in), RGBA_FP16 with SCRGB_LINEAR for half-float ones (the same numbers, the same meaning). 0 for an unknown format.
int xhd_buffer_for(uint32_t format, int *dataspace);
const char *xhd_dataspace_name(int dataspace);
// --dataspace's names: auto (-1), unknown, srgb, p3, scrgb, scrgb-linear. -2 for a name it does not know.
int xhd_dataspace_parse(const char *name);

// One SOCK_SEQPACKET message, little-endian:
//   hello  { "XHD1", width, height, stride, format } + the frame memfd (SCM_RIGHTS)
//   flush  { "XHDF", x, y, w, h }
// and from the helper, straight after connecting (xnuhost waits for it before it sizes the frame in a session):
//   panel  { "XHDP", width, height, hz, 0 }
struct xhd_message { char magic[4]; uint32_t a, b, c, d; };

struct xhd_rect { uint32_t x, y, w, h; };            // w == 0: empty

struct xhd_frame {
  uint32_t width, height, stride, format;
  int memfd;
  const uint8_t *pixels;                              // read-only map of the memfd
  size_t bytes;
};

// The session's folder is writable by the guest, so nothing from the socket is trusted:
// 0 when the hello describes a frame that fits in memfd_size bytes, else -EPROTO/-EINVAL.
int xhd_check_hello(const struct xhd_message *m, size_t length, off_t memfd_size);
// Connect to the socket at path, waiting up to wait_ms for it to appear; *stop != 0 ends the wait.
// Refuses anything that is not a socket (a link included). Returns the fd or -errno.
int xhd_connect(const char *path, int wait_ms, const volatile int *stop);
// Tell xnuhost the panel's mode. 0 or -errno.
int xhd_send_panel(int fd, uint32_t width, uint32_t height, uint32_t hz);
// Receive the hello and map the frame. 0 or -errno.
int xhd_recv_hello(int fd, struct xhd_frame *frame, int timeout_ms);
void xhd_frame_close(struct xhd_frame *frame);
// Read every pending flush without blocking, adding each (clipped to the frame) to *damage.
// 0 the socket is open, -1 it closed.
int xhd_drain(int fd, const struct xhd_frame *frame, struct xhd_rect *damage);

int xhd_rect_clip(struct xhd_rect *r, uint32_t width, uint32_t height);   // 0 when nothing is left
void xhd_rect_union(struct xhd_rect *into, const struct xhd_rect *r);

// The largest rectangle with the source's aspect inside dw x dh, centred.
struct xhd_fit { uint32_t x, y, w, h; };
void xhd_fit(uint32_t sw, uint32_t sh, uint32_t dw, uint32_t dh, struct xhd_fit *fit);

// Copy rectangle r of a frame in `format` (XHD_FORMAT_*) into a buffer of the same size laid out as `buffer`
// (XHD_BUFFER_*; dst_stride in bytes). A frame into a buffer of its own layout is copied as it is (B,G,R,X into
// B,G,R,A, half floats into RGBA_FP16); B,G,R,X into R,G,B,X swaps (alpha 0xff); half floats into an 8-bit buffer
// are encoded to sRGB and clamped (what is outside sRGB is lost); 8-bit into RGBA_FP16 is decoded to linear.
// 0, or -EINVAL for a format or buffer it does not know.
int xhd_copy(uint8_t *dst, size_t dst_stride, int buffer, const uint8_t *src, size_t src_stride, uint32_t format, struct xhd_rect r);

// The serving side (xnuhost's), for displayctl and the tests.
int xhd_listen(const char *path);                                        // fd or -errno
int xhd_send_hello(int fd, uint32_t width, uint32_t height, uint32_t stride, uint32_t format, int memfd);
int xhd_send_flush(int fd, uint32_t x, uint32_t y, uint32_t w, uint32_t h);
int xhd_memfd(const char *name, size_t bytes);                           // fd or -errno
#endif
