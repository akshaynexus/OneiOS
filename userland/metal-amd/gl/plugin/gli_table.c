/*
 * Copyright (c) 2026 iOSPlus10 contributors, OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * OneiOSRadeonGL.bundle's GLI table: the 18 entry points OpenGLES's eagl_init binds into _gli_plugin, and
 * ngl_gli_register() that hands them over. NegroniGL's plugin/gli_table.c (iOSPlus10), its 20H392 addresses
 * replaced by 20F66's (iOS 16.5, iPhone10,3: the A56's image), read off out/ios16-cache with out/ipsw-cli/ipsw
 * (`dyld symaddr --image OpenGLES`, `dyld disass --vaddr ... --quiet`, `dyld dump`; docs/a56-gpu.md 4a):
 *
 *   symbol                 20F66 (unslid)  20H392 (NegroniGL)  what is there on 20F66
 *   EAGLGetVersion         0x1d315a878     0x1d3f52878         b4000060 (cbz x0, +0xc), 52800028 (mov w8, #1)
 *   eagl_init              0x1d315a444     0x1d3f52444
 *     + 0x134              0x1d315a578     0x1d3f52578         f902d2e0 str x0, [x23, #0x5a0]: _gli_plugin[0] = dlopen
 *     + 0x200              0x1d315a644     0x1d3f52644         911682b5 add x21, x21, #0x5a0; f94002a0 ldr x0, [x21]:
 *                                                              the dlsym loop over _eagl_gli_names, x20 = 8 .. 0x98
 *   _gli_plugin            0x1e27715a0     0x1e25b0da0         OpenGLES __DATA_DIRTY.__common
 *   _eagl_initialized      0x1e2771644     0x1e25b0e44         (_gli_plugin + 0xa4 on both)
 *   _eagl_gli_names        0x1d315b350     0x1d3f53350         the 18 names below, 0x20 bytes each, same order
 * OpenGLES's code is the same on both builds at the same page offsets (-[EAGLContext commonInitWithAPI:properties:]
 * 0x1d31593f0, renderbufferStorage:fromDrawable: 0x1d31598f4, presentRenderbuffer: 0x1d3159b48, texImageIOSurface:
 * ...invert: 0x1d3159efc, getMacroContextPrivate 0x1d315a11c: NegroniGL's 0x1d3f5xxxx with the same low bits).
 * The whole cache shares one slide: slide = &EAGLGetVersion - its unslid address.
 *
 * How EAGL picks up the table: -[EAGLSharegroup initWithAPI:sharedWithCompute:] calls eagl_init. eagl_init dlopens
 * GLEngine, stores the handle at _gli_plugin[0], calls gliInitializeLibrary; that calls libGFXShared's
 * gfxInitializeLibrary, which reads IOGLESBundleName = "OneiOSRadeonGL" off the accelerator (the A56 kernel's, with
 * oneios-gpu-amd-gl=1), dlopens this bundle and calls gldInitializeLibrary -> ngl_gli_register (below): we put our
 * own handle at _gli_plugin[0]. Back in eagl_init, its loop dlsym()s each of the 18 names from _gli_plugin[0] into
 * _gli_plugin[+0x08 .. +0x90]: ours. Then gliGetVersion (+0x80) must be nonzero and gliChoosePixelFormat (+0x08)
 * must give a pixel format; _eagl_initialized is set and EAGLContexts work.
 *
 *   +0x08 gliChoosePixelFormat   +0x10 gliDestroyPixelFormat   +0x18 gliQueryRendererInfo
 *   +0x20 gliDestroyRendererInfo +0x28 gliCreateContext        +0x30 gliCreateContextWithShared
 *   +0x38 gliDestroyContext      +0x40 gliAttachDrawable       +0x48 gliAttachDrawableWithOptions
 *   +0x50 gliSetInteger          +0x58 gliGetInteger           +0x60 gliGetAttribute
 *   +0x68 gliSetAttribute        +0x70 gliCopyAttributes       +0x78 gliGetMemoryAnnotationList
 *   +0x80 gliGetVersion          +0x88 gliBindViewES           +0x90 gliPresentViewES
 * and gliTerminateLibrary, which eagl_dealloc dlsym()s and calls without a NULL check.
 *
 * Rule: nothing is written unless every code word is what 20F66 has there; no call crashes or blocks.
 */
#include <dlfcn.h>
#include <pthread.h>
#include <stdint.h>
#include <string.h>

#include "oneios_gl.h"
#include "gld.h"

#define NGL_UNSLID_EAGLGetVersion    0x1d315a878ull
#define NGL_UNSLID_gli_plugin        0x1e27715a0ull
#define NGL_UNSLID_eagl_initialized  0x1e2771644ull
#define NGL_UNSLID_eagl_init_store   0x1d315a578ull     /* str x0, [x23, #0x5a0]  (_gli_plugin[0] = dlopen) */
#define NGL_UNSLID_eagl_init_loop    0x1d315a644ull     /* add x21, x21, #0x5a0 ; ldr x0, [x21] (dlsym loop) */

static const struct ngl_gli_words words_20F66 = {
	.version = {0xb4000060u, 0x52800028u}, .store = 0xf902d2e0u, .loop = {0x911682b5u, 0xf94002a0u},
};

bool
ngl_gli_words_match(const struct ngl_gli_words *w)
{
	return memcmp(w, &words_20F66, sizeof(*w)) == 0;
}

static pthread_mutex_t reg_lock = PTHREAD_MUTEX_INITIALIZER;
static void *engine;            /* GLEngine's handle that eagl_init stored and we replaced */
static void *self_handle;       /* our handle in _gli_plugin[0] while installed */

/*
 * gldInitializeLibrary calls this each time libGFXShared loads the bundle: on the first EAGLSharegroup, and again
 * after gliTerminateLibrary (below) let libGFXShared unload everything.
 */
bool
ngl_gli_register(void)
{
	void *ev = dlsym(RTLD_DEFAULT, "EAGLGetVersion");
	if (ev == NULL) {
		ngl_logc('E', "GLI", "GLI register: no EAGLGetVersion (OpenGLES not loaded?)");
		return false;
	}
	intptr_t slide = (intptr_t)(uintptr_t)ev - (intptr_t)NGL_UNSLID_EAGLGetVersion;
	const uint32_t *v = ev, *st = (const uint32_t *)(uintptr_t)((intptr_t)NGL_UNSLID_eagl_init_store + slide),
	    *lp = (const uint32_t *)(uintptr_t)((intptr_t)NGL_UNSLID_eagl_init_loop + slide);
	struct ngl_gli_words w = {.version = {v[0], v[1]}, .store = st[0], .loop = {lp[0], lp[1]}};
	if (!ngl_gli_words_match(&w)) {
		ngl_logc('E', "GLI", "GLI register: OpenGLES is not 20F66's (words %08x %08x %08x %08x %08x): not installed",
		    w.version[0], w.version[1], w.store, w.loop[0], w.loop[1]);
		return false;
	}
	void **gli = (void **)(uintptr_t)((intptr_t)NGL_UNSLID_gli_plugin + slide);
	const volatile uint8_t *inited = (const uint8_t *)(uintptr_t)((intptr_t)NGL_UNSLID_eagl_initialized + slide);
	pthread_mutex_lock(&reg_lock);
	if (self_handle != NULL && gli[0] == self_handle) {
		pthread_mutex_unlock(&reg_lock);
		return true;            /* already installed (gldInitializeLibrary per accelerator) */
	}
	/* eagl_init is mid-flight: GLEngine's handle is at gli[0] and EAGL is not initialized yet. Otherwise leave it
	 * (EAGL stays as it was, no crash). */
	if (*inited != 0 || gli[0] == NULL) {
		pthread_mutex_unlock(&reg_lock);
		ngl_logc('E', "GLI", "GLI register: unexpected state (initialized=%d, plugin0=%p); not installed",
		    (int)*inited, gli[0]);
		return false;
	}
	Dl_info info;
	void *self = NULL;
	if (dladdr((const void *)&ngl_gli_register, &info) && info.dli_fname != NULL) {
		self = dlopen(info.dli_fname, RTLD_LAZY | RTLD_NOLOAD | RTLD_NODELETE);
	}
	if (self == NULL) {
		pthread_mutex_unlock(&reg_lock);
		ngl_logc('E', "GLI", "GLI register: dlopen(self) failed");
		return false;
	}
	engine = gli[0];
	self_handle = self;
	gli[0] = self;                  /* eagl_init's dlsym loop now binds our gli* exports */
	pthread_mutex_unlock(&reg_lock);
	ngl_logc('I', "GLI", "GLI table installed in OpenGLES's _gli_plugin (20F66, slide %#lx)", (long)slide);
	return true;
}

/*
 * eagl_dealloc, when the last EAGLSharegroup goes, calls gliDestroyPixelFormat, then dlsym(_gli_plugin[0],
 * "gliTerminateLibrary") and calls the result without a NULL check, then dlclose(_gli_plugin[0]) (our handle) and
 * clears it and _eagl_initialized. We balance what eagl_init did for us: gfxReleaseLibrary (libGFXShared destroys
 * our devices; the bundle stays mapped, RTLD_NODELETE) and dlclose of GLEngine's handle. The next EAGLSharegroup runs
 * eagl_init again and ngl_gli_register installs the table again.
 */
NGL_GLD void
gliTerminateLibrary(void)
{
	pthread_mutex_lock(&reg_lock);
	void *eng = engine;
	engine = NULL;
	self_handle = NULL;
	pthread_mutex_unlock(&reg_lock);
	void (*release)(void) = (void (*)(void))dlsym(RTLD_DEFAULT, "gfxReleaseLibrary");
	if (release != NULL) {
		release();
	}
	if (eng != NULL) {
		dlclose(eng);
	}
	ngl_logc('I', "GLI", "gliTerminateLibrary: last EAGLSharegroup gone, libGFXShared released");
}

/* +0x08: eagl_init stores *out at _gli_pixelformat; -[EAGLSharegroup init..] needs flags bit 0x100 (and 0x20000
 * for API 3), which ngl_pf.flags (0x225dd, gld.c) has. ngl_pf.device_id = our gld device. */
NGL_GLD int
gliChoosePixelFormat(void **out, const int *attribs)
{
	(void)attribs;
	if (out != NULL) {
		*out = &ngl_pf;
	}
	static int logged;
	if (!__atomic_exchange_n(&logged, 1, __ATOMIC_RELAXED)) {
		ngl_logc('I', "GLI", "gliChoosePixelFormat: OneiOSRadeonGL pixel format (device %#x, flags %#x)",
		    ngl_pf.device_id, ngl_pf.flags);
	}
	return 0;
}

NGL_GLD int
gliDestroyPixelFormat(void *pf)
{
	(void)pf;                       /* ngl_pf is static */
	return 0;
}

/* +0x80: eagl_init needs a nonzero return; GLEngine reports (2, 3, 0x20000) here. */
NGL_GLD int
gliGetVersion(int *a, int *b, int *c)
{
	if (a != NULL) {
		*a = 2;
	}
	if (b != NULL) {
		*b = 3;
	}
	if (c != NULL) {
		*c = 0x20000;
	}
	return 1;
}

int gliCreateContextWithShared(void **out, void *pf, void *shared, void *disp1, void *disp2, uint32_t flags);

/* +0x28: the non-shared context entry (macOS CGL's name). iOS EAGL uses gliCreateContextWithShared. */
NGL_GLD int
gliCreateContext(void **out, void *pf, void *disp1, void *disp2, uint32_t flags)
{
	return gliCreateContextWithShared(out, pf, NULL, disp1, disp2, flags);
}

/* +0x18/+0x20: renderer-info queries. No GLD renderer info is published. */
NGL_GLD int
gliQueryRendererInfo(void *a, void *b, void *c)
{
	(void)a;
	(void)b;
	(void)c;
	return NGL_ERR_BAD_RENDERER_INFO;
}

NGL_GLD int
gliDestroyRendererInfo(void *info)
{
	(void)info;
	return 0;
}

/* +0x60..+0x78: attribute/annotation queries, not used by the iOS EAGL path; succeed as no-ops. */
NGL_GLD int
gliGetAttribute(void *ctx, int attr, int *value)
{
	(void)ctx;
	(void)attr;
	if (value != NULL) {
		*value = 0;
	}
	return 0;
}

NGL_GLD int
gliSetAttribute(void *ctx, int attr, const int *value)
{
	(void)ctx;
	(void)attr;
	(void)value;
	return 0;
}

NGL_GLD int
gliCopyAttributes(void *dst, void *src, uint32_t mask)
{
	(void)dst;
	(void)src;
	(void)mask;
	return 0;
}

NGL_GLD int
gliGetMemoryAnnotationList(void *ctx, void *out, void *count)
{
	(void)ctx;
	if (out != NULL) {
		*(void **)out = NULL;
	}
	if (count != NULL) {
		*(uint32_t *)count = 0;
	}
	return 0;
}
