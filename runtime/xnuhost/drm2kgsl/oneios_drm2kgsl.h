// SPDX-License-Identifier: MIT
// What drm2kgsl/patches add to Droid-VM's drm2kgsl_renderer.c for xnuhost (src/gpu3d_kgsl.c). The patches are applied
// to a copy of the file at build time (Makefile); external/virglrenderer stays at its pin.
#ifndef ONEIOS_DRM2KGSL_H
#define ONEIOS_DRM2KGSL_H
#include <stdint.h>

struct virgl_context;

// set_guest_blob_fd() for a blob whose pages come as `n` dma-bufs in the blob's order, `sizes[i]` bytes each (whole
// pages): parked for the get_blob() that follows, which imports each into KGSL and binds them one after another at
// the BO's GPU address. Takes the fds, also when it fails. With n == 1 it is set_guest_blob_fd(). 0 or a negative errno.
int drm2kgsl_renderer_set_guest_blob_chunks(struct virgl_context *ctx, uint64_t blob_id, const int *fds, const uint64_t *sizes, unsigned n);
// attach_resource() for a resource made of `n` dma-bufs by another context or by the display: this context imports
// each and knows them as `res_id` (bound when the guest sends GEM_SET_IOVA). The fds stay the caller's. Nothing
// happens when the context already has the resource. 0 or a negative errno.
int drm2kgsl_renderer_attach_chunks(struct virgl_context *ctx, uint32_t res_id, const int *fds, const uint64_t *sizes, unsigned n);
// patch 0005: how often the render server's clock hold started, and the keep-alive submissions it made, since the start.
void drm2kgsl_renderer_hold_stats(uint64_t *holds, uint64_t *keepalives);
// patch 0007: KGSL's imports since the start, those whose flags were read back (the first 256, then every 64th), and of
// those: asked IO-coherent and kept it, asked and lost it (KGSL dropped it), asked without it, flags not readable.
void drm2kgsl_renderer_import_stats(uint64_t *imports, uint64_t *checked, uint64_t *kept, uint64_t *dropped, uint64_t *plain,
                                    uint64_t *unreadable);
#endif
