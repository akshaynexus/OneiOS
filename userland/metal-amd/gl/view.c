/*
 * Copyright (c) 2026 iOSPlus10 contributors, OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * OneiOSRadeonGL: CAEAGLLayer drawables (the "native window") as renderbuffer storage, and the present; the flush.
 * From NegroniGL's view.c (iOSPlus10, MIT); its table facts are of 20H392's QuartzCore, whose code at these places
 * is the same as 20F66's at other addresses (OpenGLES is the same code: docs/a56-gpu.md 4a).
 *
 * The native window is QuartzCore's _CAEAGLNativeWindow (-[CAEAGLLayer nativeWindow]) [NegroniGL, 20H392]: a
 * table of functions, each taking the window as argument 0:
 *   +0x00 uint32 version (5)
 *   +0x08 bool attach(nw, fourcc, callbacks)  takes a reference, copies callbacks
 *         {ctx, create(ctx, surface), destroy(ctx, surface), preflight(ctx, w, h, flags) or NULL}
 *   +0x10 void detach(nw)                     drops the reference
 *   +0x18 IOSurfaceRef begin_iosurface(nw)    the next drawable surface; NULL until attached
 *   +0x20 bool swap(nw, mode, double time)    queues the surface to CoreAnimation
 *   +0x30 uint32 begin_properties(nw)         (version >= 2) orientation flags
 * Present: flush, then swap(nw, version >= 5 && time != 0 ? (after ? 5 : 3) : 1, time); the next frame draws into
 * the next begin_iosurface surface.
 *
 * The flush (every glFlush, glFinish, present and APPLE wait): the copied surfaces drawn into since the last one go
 * back into their IOSurfaces (surface.c; nothing to do in place), then glFlush. On the A56 the guest's display path
 * reads the surface after CoreAnimation's swap, so the write back must be done when the flush returns: it maps the
 * resource for reading, which waits for the GPU. (NegroniGL's kernel fences and its RedwoodWaitsForGPU switch are
 * not taken: the A56's kernel has neither.)
 *
 * Lines (os_log; the first of each also as a kernel text row, log.c): the first drawable bound, the first present,
 * the rate at the 600th (presents a second since the first, and the copies' bytes), every 3600th after.
 * Rule: never crash the caller. A NULL pointer or a bad surface fails with false (EAGL returns NO).
 */
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include <CoreFoundation/CoreFoundation.h>
#include <IOSurface/IOSurfaceRef.h>

#include "oneios_gl.h"

struct ngl_nw {                 /* _CAEAGLNativeWindow function table (head only) */
	uint32_t version;
	uint32_t pad;
	bool (*attach)(void *nw, uint32_t fourcc, void *const *callbacks);
	void (*detach)(void *nw);
	IOSurfaceRef (*begin_iosurface)(void *nw);
	bool (*swap)(void *nw, uint32_t mode, double t);
	void *collect;
	uint32_t (*begin_properties)(void *nw);
};

static unsigned presents;
static double first_present_ms;

static double
ms_now(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (double)ts.tv_sec * 1e3 + (double)ts.tv_nsec / 1e6;
}

unsigned
ngl_presents(void)
{
	return __atomic_load_n(&presents, __ATOMIC_RELAXED);
}

/*
 * The frames of a process that draws without a CAEAGLLayer (backboardd's render server: CoreAnimation renders into
 * the display's IOSurfaces and swaps them itself, so no present is ever called): a flush after draws is a frame.
 * The first is said, and the rate at the 600th and every 3600th after (with the copies' bytes).
 */
static unsigned frames;
static double first_frame_ms;

void
ngl_flush(void)
{
	unsigned back = ngl_surf_write_back();
	glFlush();
	struct ngl_state *st = ngl_cur ? ngl_cur->st : NULL;
	if (st == NULL || st->writes == 0) {
		return;
	}
	st->writes = 0;
	unsigned n = __atomic_add_fetch(&frames, 1, __ATOMIC_RELAXED);
	double now = ms_now();
	if (n == 1) {
		first_frame_ms = now;
		ngl_logc('I', "GLI", "first frame drawn on %s: %u copied surfaces written back", ngl_screen_name(), back);
	} else if (n == 600 || n % 3600 == 0) {
		struct ngl_surf_stats s;
		ngl_surf_stats(&s);
		double secs = (now - first_frame_ms) / 1e3;
		ngl_logc('I', "GLI", "%u frames, %.1f a second since the first; copies: %u surfaces, %u uploads (%llu MB), "
		    "%u write backs (%llu MB)", n, secs > 0 ? (double)(n - 1) / secs : 0.0, s.surfaces, s.uploads,
		    (unsigned long long)(s.upload_bytes >> 20), s.write_backs, (unsigned long long)(s.write_back_bytes >> 20));
	}
}

unsigned
ngl_frames(void)
{
	return __atomic_load_n(&frames, __ATOMIC_RELAXED);
}

/* callbacks CA calls when it makes or drops a drawable surface for us */
static bool
cb_create(void *ctx, IOSurfaceRef s)
{
	(void)ctx;
	(void)s;
	return true;
}

static void
cb_destroy(void *ctx, IOSurfaceRef s)
{
	(void)ctx;
	(void)s;                /* our view_surface holds its own reference */
}

static IOSurfaceRef
valid_surface(IOSurfaceRef s)
{
	return s != NULL && CFGetTypeID(s) == IOSurfaceGetTypeID() ? s : NULL;
}

/* make the drawable surface s the storage of renderbuffer rb (current context = g) */
static bool
bind_storage(struct ngl_gli *g, GLuint rb, IOSurfaceRef s)
{
	GLint was = 0;
	glGetIntegerv(GL_RENDERBUFFER_BINDING, &was);
	if ((GLuint)was != rb) {
		glBindRenderbuffer(GL_RENDERBUFFER, rb);
	}
	/* surface.c reads the GL format from the IOSurface pixel format (format/type 0 = infer) */
	bool ok = ngl_bind_surface(s, GL_RENDERBUFFER, 0, 0, 0, false, "renderbufferStorage");
	if ((GLuint)was != rb) {
		glBindRenderbuffer(GL_RENDERBUFFER, (GLuint)was);
	}
	if (ok) {
		if (g->view_surface != NULL) {
			CFRelease(g->view_surface);
		}
		g->view_surface = (IOSurfaceRef)CFRetain(s);
	}
	return ok;
}

/* detach the drawable and release its surface. No GL calls (safe from dealloc on any thread). */
void
ngl_view_unbind(struct ngl_gli *g)
{
	if (g == NULL) {
		return;
	}
	struct ngl_nw *nw = g->view;
	if (nw != NULL && nw->detach != NULL) {
		nw->detach(nw);         /* drops the reference attach took */
	}
	if (g->view_surface != NULL) {
		CFRelease(g->view_surface);
		g->view_surface = NULL;
	}
	g->view = NULL;
	g->view_rb = 0;
	g->view_colorformat = 0;
}

/*
 * Attach (native_window != NULL) or detach (NULL) the drawable. On attach, the caller has bound the renderbuffer
 * that receives the storage (glBindRenderbuffer before -renderbufferStorage:fromDrawable:). true = ok.
 */
bool
ngl_view_bind(struct ngl_gli *g, void *native_window, unsigned colorformat, unsigned retained)
{
	if (g == NULL) {
		return false;
	}
	ngl_view_unbind(g);                     /* drop the previous drawable, if any */
	if (native_window == NULL) {
		return true;                    /* detach only */
	}
	struct ngl_nw *nw = native_window;
	if (nw->attach == NULL || nw->detach == NULL || nw->begin_iosurface == NULL || nw->swap == NULL) {
		ngl_logc('E', "VIEW", "renderbufferStorage: native window %p has no function table", native_window);
		return false;
	}
	NGL_CUR(g);
	GLint rb = 0;
	glGetIntegerv(GL_RENDERBUFFER_BINDING, &rb);
	if (rb <= 0) {
		ngl_logc('E', "VIEW", "renderbufferStorage: no renderbuffer bound");
		return false;
	}
	/* 'BGRA' for every colour format: the one drawable format surface.c binds (GLEngine maps GL_RGB565 to 'L565'
	 * and two wide formats to 10-bit ones) */
	g->view_cb[0] = g;
	g->view_cb[1] = (void *)cb_create;
	g->view_cb[2] = (void *)cb_destroy;
	g->view_cb[3] = NULL;
	if (!nw->attach(nw, 'BGRA', g->view_cb)) {
		ngl_logc('E', "VIEW", "renderbufferStorage: drawable attach failed (colorformat %#x)", colorformat);
		return false;
	}
	g->view = nw;
	if (nw->version >= 2 && nw->begin_properties != NULL) {
		(void)nw->begin_properties(nw);         /* orientation flags; rotation is not applied */
	}
	IOSurfaceRef s = valid_surface(nw->begin_iosurface(nw));
	if (s == NULL || !bind_storage(g, (GLuint)rb, s)) {
		static int logged;
		if (!__atomic_exchange_n(&logged, 1, __ATOMIC_RELAXED)) {
			ngl_logc('E', "VIEW", "renderbufferStorage: no usable IOSurface from the drawable (colorformat %#x)",
			    colorformat);
		}
		ngl_view_unbind(g);
		return false;
	}
	g->view_rb = (GLuint)rb;
	g->view_colorformat = colorformat;
	static int logged;
	if (!__atomic_exchange_n(&logged, 1, __ATOMIC_RELAXED)) {
		ngl_logc('I', "VIEW", "drawable IOSurface %u (%zux%zu) is renderbuffer %d (colorformat %#x, retained %u, "
		    "window version %u)", IOSurfaceGetID(s), IOSurfaceGetWidth(s), IOSurfaceGetHeight(s), rb, colorformat,
		    retained, nw->version);
	}
	return true;
}

/* make CA's next drawable surface the renderbuffer storage; false = none (view_surface unchanged) */
static bool
next_drawable(struct ngl_gli *g, struct ngl_nw *nw)
{
	if (g->view_rb == 0 || !glIsRenderbuffer(g->view_rb)) {
		return false;
	}
	IOSurfaceRef s = valid_surface(nw->begin_iosurface(nw));
	if (s == NULL) {
		static int logged;
		if (!__atomic_exchange_n(&logged, 1, __ATOMIC_RELAXED)) {
			ngl_logc('W', "VIEW", "present: no next drawable from CA (begin_iosurface NULL): retried at the next "
			    "present");
		}
		return false;
	}
	if (s == g->view_surface) {
		return true;
	}
	if (!bind_storage(g, g->view_rb, s)) {
		static int logged;
		if (!__atomic_exchange_n(&logged, 1, __ATOMIC_RELAXED)) {
			ngl_logc('E', "VIEW", "present: next drawable IOSurface %u not bound", IOSurfaceGetID(s));
		}
		return false;
	}
	return true;
}

/* the frame line: a present's count and the rate since the first (cheap: no GL call) */
static void
frame_line(unsigned n, bool ok, uint32_t mode, struct ngl_gli *g)
{
	double now = ms_now();
	if (n == 1) {
		first_present_ms = now;
		ngl_logc('I', "VIEW", "first present: swap %s (mode %u), drawable now IOSurface %u, screen %s",
		    ok ? "ok" : "refused", mode, g->view_surface ? IOSurfaceGetID(g->view_surface) : 0, ngl_screen_name());
		return;
	}
	if (n == 600 || n % 3600 == 0) {
		struct ngl_surf_stats s;
		ngl_surf_stats(&s);
		double secs = (now - first_present_ms) / 1e3;
		ngl_logc('I', "VIEW", "%u presents, %.1f a second since the first; copies: %u surfaces, %u uploads "
		    "(%llu MB), %u write backs (%llu MB)", n, secs > 0 ? (double)(n - 1) / secs : 0.0, s.surfaces, s.uploads,
		    (unsigned long long)(s.upload_bytes >> 20), s.write_backs, (unsigned long long)(s.write_back_bytes >> 20));
	}
}

/*
 * Present the renderbuffer: flush (the copies written back), swap hands the surface to CoreAnimation, then the
 * renderbuffer gets the next drawable surface. after_minimum / t: the -presentRenderbuffer:afterMinimumDuration:
 * / :atTime: forms (0, 0.0 = now).
 */
bool
ngl_view_present(struct ngl_gli *g, unsigned after_minimum, double t)
{
	if (g == NULL) {
		return false;
	}
	NGL_CUR(g);
	ngl_flush();
	struct ngl_nw *nw = g->view;
	if (nw == NULL) {
		static int logged;
		if (!__atomic_exchange_n(&logged, 1, __ATOMIC_RELAXED)) {
			ngl_logc('W', "VIEW", "present: no drawable attached");
		}
		return false;
	}
	/* the last present got no next drawable (view_surface cleared): get one now, before the swap */
	if (g->view_surface == NULL && !next_drawable(g, nw)) {
		static int logged;
		if (!__atomic_exchange_n(&logged, 1, __ATOMIC_RELAXED)) {
			ngl_logc('W', "VIEW", "present: still no drawable from CA: frame not presented (retried at every present)");
		}
		return false;
	}
	uint32_t mode = nw->version >= 5 && t != 0.0 ? (after_minimum ? 5 : 3) : 1;
	bool ok = nw->swap(nw, mode, t);
	/* the swapped surface is CA's now: never draw into it again. No next drawable (begin_iosurface NULL, e.g. all
	 * buffers out) = view_surface cleared, retried at the next present. */
	if (!next_drawable(g, nw) && g->view_surface != NULL) {
		CFRelease(g->view_surface);
		g->view_surface = NULL;
	}
	frame_line(__atomic_add_fetch(&presents, 1, __ATOMIC_RELAXED), ok, mode, g);
	return ok;
}
