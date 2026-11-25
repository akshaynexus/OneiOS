/*
 * Copyright (c) 2026 iOSPlus10 contributors, OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * OneiOSRadeonGL.bundle's GLI contexts on Mesa (NegroniGL's plugin/gli_context.c, iOSPlus10). EAGL's private block
 * (-[EAGLContext commonInitWithAPI:properties:], calloc 0x40e8) has +0x20 GLIContext, +0x28 and +0x2088 two
 * dispatch tables of 1036 slots each. It calls, through _gli_plugin+0x30:
 *   gliCreateContextWithShared(priv+0x20, pixelformat, gfx_shared_state, priv+0x28, priv+0x2088, flags)
 * flags 0x10/0x20/0x40 = API 1/2/3. Every EAGLSharegroup has its own gfx shared state, so the shared-state pointer
 * names the sharegroup. We make a Mesa GLES context (gl_core.c ngl_create), store a struct ngl_gli as the GLIContext
 * at *out, and fill both dispatch tables with ngl_dispatch (the 1036-slot table generated from gli-slots.txt, with
 * gl_hand.c's slots). Every glFoo() of OpenGLES then does rctx = [priv+0x20]; jump [priv+0x28 + 8*slot], i.e.
 * ngl_dispatch[slot](the ngl_gli, args); CoreAnimation calls the same table from -getMacroContextPrivate.
 *
 * gliSetInteger (+0x50) carries -texImageIOSurface: (and -attachImage:toCoreSurface:invertedRender:, which calls it)
 * as pname 0x38e {surface ID, target, internal format, w, h, format, type, plane, 0} then 0x399 {target, invert}.
 * 0x38e binds the surface the right way up; an 0x399 with invert set binds it again turned over (surface.c: then
 * always a copy, its rows turned over both ways: Jesvi-iOS's eagl.c does the same). NegroniGL ignored the invert.
 * gliGetInteger (+0x58) backs -getParameter:. Rule: never crash or block the caller; fail with a CGL error.
 *
 * Threads: EAGL may destroy a context on a thread other than the one where it is current. Mesa still points at it
 * there, so a context that is ngl_cur on another thread is only marked (zombie) and freed by ngl_gli_bound() when
 * that thread switches away (gl_hand.c ngl_bind).
 */
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "oneios_gl.h"
#include "gld.h"

/*
 * Mesa object sharing inside one EAGLSharegroup: a new context shares with any live context of the same gfx shared
 * state. share_lock covers the list and is held across ngl_create, so the context we share with cannot be freed
 * while Mesa takes its shared state (destroy unlinks under the lock first).
 */
static pthread_mutex_t share_lock = PTHREAD_MUTEX_INITIALIZER;
static struct ngl_gli *contexts;        /* live contexts, linked by share_next */
static pthread_mutex_t bind_lock = PTHREAD_MUTEX_INITIALIZER;   /* binds / zombie of every context */
static unsigned made;

static void
share_unlink(struct ngl_gli *g)
{
	pthread_mutex_lock(&share_lock);
	for (struct ngl_gli **p = &contexts; *p != NULL; p = &(*p)->share_next) {
		if (*p == g) {
			*p = g->share_next;
			break;
		}
	}
	g->share_next = NULL;
	pthread_mutex_unlock(&share_lock);
}

static void
reap(struct ngl_gli *g)
{
	for (GLuint n = 0; n < NGL_NAMES; n++) {        /* the copied surfaces it showed (no GL call) */
		ngl_surf_forget_texture(g->st, n);
		ngl_surf_forget_renderbuffer(g->st, n);
	}
	ngl_destroy(g->ctx);    /* binds == 0: Mesa has it current on no thread */
	free(g->st);
	free(g);
}

/* gl_hand.c ngl_bind: this thread switched from old to g (either may be NULL) */
void
ngl_gli_bound(struct ngl_gli *old, struct ngl_gli *g)
{
	struct ngl_gli *dead = NULL;
	pthread_mutex_lock(&bind_lock);
	if (g != NULL) {
		g->binds++;
	}
	if (old != NULL && --old->binds <= 0 && old->zombie) {
		dead = old;
	}
	pthread_mutex_unlock(&bind_lock);
	if (dead != NULL) {
		reap(dead);
		ngl_logc('I', "CTX", "GLI %p freed now that its last thread switched away", (void *)dead);
	}
}

struct ngl_gli *
ngl_gli_new(int api, void *shared)
{
	struct ngl_gli *g = calloc(1, sizeof(*g));
	struct ngl_state *st = g ? calloc(1, sizeof(*st)) : NULL;
	if (st == NULL) {
		free(g);
		return NULL;
	}
	pthread_mutex_lock(&share_lock);
	struct ngl_gli *lead = NULL;
	for (struct ngl_gli *c = contexts; c != NULL && shared != NULL; c = c->share_next) {
		if (c->shared == shared) {
			lead = c;
			break;
		}
	}
	struct ngl_ctx *ctx = ngl_create(api, lead ? lead->ctx : NULL);
	if (ctx != NULL) {
		g->rctx = g;
		g->api = (unsigned)api;
		g->ctx = ctx;
		g->st = st;
		g->shared = shared;
		g->share_next = contexts;
		contexts = g;
		memcpy(g->disp, ngl_dispatch, sizeof(g->disp));
	}
	pthread_mutex_unlock(&share_lock);
	if (ctx == NULL) {
		free(st);
		free(g);
		return NULL;
	}
	ngl_check_errors = ngl_env_on("ONEIOS_GL_ERRORS", false);
	if (__atomic_fetch_add(&made, 1, __ATOMIC_RELAXED) < 4) {
		ngl_logc('I', "CTX", "GLES %d context %p on %s%s", api, (void *)g, ngl_screen_name(),
		    lead ? " (shares objects)" : "");
	}
	return g;
}

NGL_GLD int
gliCreateContextWithShared(void **out, void *pf, void *shared, void *disp1, void *disp2, uint32_t flags)
{
	(void)pf;
	if (out == NULL) {
		return NGL_ERR_BAD_CONTEXT;
	}
	*out = NULL;
	int api = (flags & 0x40) ? 3 : (flags & 0x20) ? 2 : 1;
	struct ngl_gli *g = ngl_gli_new(api, shared);
	if (g == NULL) {
		ngl_logc('E', "CTX", "gliCreateContextWithShared: no Mesa GLES %d context", api);
		return NGL_ERR_BAD_CONTEXT;
	}
	if (disp1 != NULL) {
		memcpy(disp1, ngl_dispatch, sizeof(void *) * NGL_SLOTS);
	}
	if (disp2 != NULL) {
		memcpy(disp2, ngl_dispatch, sizeof(void *) * NGL_SLOTS);
	}
	*out = g;
	return 0;
}

/* -[EAGLContext dealloc] calls this, then zeroes both dispatch tables */
NGL_GLD int
gliDestroyContext(void *rctx)
{
	struct ngl_gli *g = rctx;
	if (g == NULL) {
		return 0;
	}
	ngl_view_unbind(g);             /* detach the drawable (view.c, no GL calls) */
	if (ngl_cur == g) {
		ngl_bind(NULL);         /* Mesa leaves g on this thread */
	}
	share_unlink(g);
	pthread_mutex_lock(&bind_lock);
	bool later = g->binds > 0;      /* still ngl_cur on another thread */
	if (later) {
		g->zombie = true;
	}
	pthread_mutex_unlock(&bind_lock);
	if (later) {
		ngl_logc('W', "CTX", "GLI %p destroyed while current on another thread: freed when it switches", (void *)g);
		return 0;
	}
	reap(g);
	return 0;
}

/* params of pname 0x38e: {surface ID, target, internal format, w, h, format, type, plane, 0} */
static int
set_iosurface(struct ngl_gli *g, const int32_t *p, bool invert)
{
	IOSurfaceRef s = IOSurfaceLookup((IOSurfaceID)p[0]);
	if (s == NULL) {
		ngl_logc('W', "SURF", "gliSetInteger 0x38e: IOSurface %d not found", p[0]);
		return NGL_ERR_BAD_STATE;
	}
	NGL_CUR(g);
	bool ok = ngl_bind_surface(s, NGL_TGT((GLenum)p[1]), (GLenum)p[5], (GLenum)p[6], (uint32_t)p[7], invert,
	    invert ? "texImageIOSurface (inverted)" : "texImageIOSurface");
	CFRelease(s);
	return ok ? 0 : NGL_ERR_BAD_STATE;
}

NGL_GLD int
gliSetInteger(void *rctx, uint32_t pname, const int32_t *params)
{
	struct ngl_gli *g = rctx;
	if (g == NULL) {
		return NGL_ERR_BAD_CONTEXT;
	}
	if (pname == 0x38e && params != NULL) {
		memcpy(g->pending, params, sizeof(g->pending));
		g->has_pending = true;
		return set_iosurface(g, params, false);
	}
	if (pname == 0x399) {
		/* {target, invert}: the texture was bound by 0x38e; inverted, it is bound again turned over */
		bool invert = params != NULL && params[1] != 0;
		bool again = invert && g->has_pending;
		g->has_pending = false;
		return again ? set_iosurface(g, g->pending, true) : 0;
	}
	if (pname == 0x263 || pname == 0x264) {
		/* -setBlockFence:onQueue: (NegroniGL): we never call the block back, so refuse (the method returns NO)
		 * instead of letting a caller wait for it */
		return NGL_ERR_BAD_ATTRIBUTE;
	}
	return 0;               /* MTEngine/version/label/profiler integers, CA's 0x260: accept and ignore */
}

NGL_GLD int
gliGetInteger(void *rctx, uint32_t pname, void *out)
{
	if ((struct ngl_gli *)rctx == NULL) {
		return NGL_ERR_BAD_CONTEXT;
	}
	if (out == NULL) {
		return 0;
	}
	if (pname == 0x7aa) {
		*(void **)out = NULL;   /* GLCGetProfilerStorage reads 8 bytes: no storage */
	} else {
		*(int32_t *)out = 0;    /* e.g. 0x139 (isMultiThreaded): no */
	}
	return 0;
}
