/*
 * Copyright (c) 2026 iOSPlus10 contributors, OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * OneiOSRadeonGL.bundle internals shared by gld.c (the GLD exports), the generated gld_stubs.c and the GLI files.
 * From NegroniGL's plugin/gld.h (iOSPlus10); its facts were read off 20H392 and are checked again on 20F66
 * (docs/a56-gpu.md 4a: libGFXShared's 129 names are the same list in the same order).
 */
#ifndef ONEIOS_GL_GLD_H
#define ONEIOS_GL_GLD_H

#include <stdbool.h>
#include <stdint.h>

#define NGL_GLD __attribute__((visibility("default")))

/* CGLError-style codes that libGFXShared, GLEngine and EAGL use */
enum {
	NGL_ERR_BAD_ATTRIBUTE = 0x2710,
	NGL_ERR_BAD_PIXEL_FORMAT = 0x2712,
	NGL_ERR_BAD_RENDERER_INFO = 0x2713,
	NGL_ERR_BAD_CONTEXT = 0x2714,
	NGL_ERR_BAD_DRAWABLE = 0x2715,
	NGL_ERR_BAD_STATE = 0x2717,
	NGL_ERR_BAD_ALLOC = 0x2720,
	NGL_ERR_BAD_CONNECTION = 0x2721,
};

/*
 * gldGetVersion's 4th value: the driver ID (bits 8-15 only; libGFXShared rejects d & 0xffff00ff and gives 0x400 a
 * special meaning). Apple's AppleMetalGLRenderer uses 0x7f00, NegroniGL 0x4e00 ('N'); ours 0x5200 ('R').
 */
#define NGL_DRIVER_ID 0x5200u
#define NGL_GFX_PLUGIN_BIT 0x20000u     /* libGFXShared: plugin ID = driver ID | 0x20000 */

/*
 * The GLD pixel format record (Apple's GLRPixelFormat + 0x40, from GLRPixelFormat::preflight): +0 next, +8 device
 * ID (GLEngine ORs 0x20000 in), +0xc flags: 0x100 = usable by EAGL, 0x20000 = OpenGL ES 3 (EAGLSharegroup
 * -initWithAPI:sharedWithCompute: masks 0, 0, 0x20000 for API 1, 2, 3). EAGL reads only +0, +8 and +0xc.
 */
struct ngl_pixel_format {
	struct ngl_pixel_format *next;
	uint32_t device_id;
	uint32_t flags;
	uint32_t words[12];
};

extern struct ngl_pixel_format ngl_pf;

int ngl_gld_unexpected(int index, const char *name);

/*
 * gli_table.c: hand OpenGLES our 18 GLI entry points. gldInitializeLibrary calls this while eagl_init is still
 * inside gliInitializeLibrary (before eagl_init binds the _gli_plugin table). false: not installed (the reason is
 * logged), and EAGL stays on GLEngine, whose pixel format we refuse: nil contexts.
 */
bool ngl_gli_register(void);
/* the check of the 20F66 code words, on any memory (the host test runs it on a stand-in) */
struct ngl_gli_words { uint32_t version[2], store, loop[2]; };
bool ngl_gli_words_match(const struct ngl_gli_words *w);

#endif
