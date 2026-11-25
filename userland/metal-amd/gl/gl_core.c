/*
 * Copyright (c) 2026 iOSPlus10 contributors, OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * OneiOSRadeonGL: Mesa (Gallium + its GLES 3.x frontend) as a library inside iOS 16.5 processes.
 *
 * A minimal Gallium frontend, as NegroniGL's negronigl.c (iOSPlus10) and Mesa's frontends/hgl: one pipe_screen,
 * GLES contexts made current without a drawable. CoreAnimation draws into framebuffers only, whose colour
 * buffers are IOSurfaces (surface.c, view.c); nothing here needs a window system.
 *
 * The screen (ngl_init, once per process):
 *   radeonsi  oneios_radeonsi_screen_create (contract K3, the guest Mesa stage's), when this library was built in
 *             a tree that has it (-DONEIOS_GL_RADEONSI=1: userland/metal-amd/gl/build.sh MESA_SRC MESA_BUILD)
 *   softpipe  Mesa on the CPU when there is no radeonsi screen, in every process but backboardd (softpipe_allowed:
 *             CoreImage and GL applications get a correct GL; backboardd keeps CoreAnimation's faster CPU renderer),
 *             or as ONEIOS_GL_SOFTPIPE and the boot argument oneios-gl-softpipe=0|1 say
 *   Neither: no screen, and the gate (below) gives the process no GL: its EAGLContexts stay nil, as without us.
 *
 * Changes against NegroniGL: the screen (freedreno over its msm emulation -> K3 radeonsi), no gpu-policy, no
 * EAGL shim constructor (the bundle is reached through IOGLESBundleName only), the gate reads the bundle's own
 * files and the boot arguments, the self test takes its IOSurface from the caller, the probe export.
 */
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#ifdef __APPLE__
#include <sys/sysctl.h>
#endif

#include "pipe/p_screen.h"
#include "pipe/p_context.h"
#include "frontend/api.h"
#include "state_tracker/st_context.h"
#include "softpipe/sp_public.h"
#include "frontend/sw_winsys.h"

#include "oneios_gl.h"

#ifndef ONEIOS_GL_MESA_VERSION
#define ONEIOS_GL_MESA_VERSION "?"
#endif
#ifndef ONEIOS_GL_RADEONSI
#define ONEIOS_GL_RADEONSI 0
#endif

struct ngl_ctx {
	struct st_context *st;
	int api;
};

static pthread_mutex_t init_lock = PTHREAD_MUTEX_INITIALIZER;
static struct pipe_frontend_screen *fscreen;
static bool tried, soft;
static char no_screen[320];

bool
ngl_env_on(const char *name, bool dflt)
{
	const char *v = getenv(name);
	if (v == NULL || v[0] == 0) {
		return dflt;
	}
	return !(v[0] == '0' || v[0] == 'n' || v[0] == 'N' || v[0] == 'f' || v[0] == 'F');
}

/* NAME=VALUE in kern.bootargs (the session's boot-args of xnuhost.conf); a bare NAME is 1 */
bool
ngl_boot_arg(const char *name, unsigned *value)
{
#ifdef __APPLE__
	char args[1024] = "";
	size_t n = sizeof(args) - 1;
	if (sysctlbyname("kern.bootargs", args, &n, NULL, 0) != 0) {
		return false;
	}
	args[n < sizeof(args) ? n : sizeof(args) - 1] = 0;
	size_t l = strlen(name);
	for (char *p = strstr(args, name); p != NULL; p = strstr(p + 1, name)) {
		if ((p == args || p[-1] == ' ') && (p[l] == '=' || p[l] == ' ' || p[l] == 0)) {
			if (value != NULL) {
				*value = p[l] == '=' ? (unsigned)strtoul(p + l + 1, NULL, 0) : 1;
			}
			return true;
		}
	}
#else
	(void)name;
	(void)value;
#endif
	return false;
}

const char *
ngl_version(void)
{
	return "OneiOSRadeonGL (Mesa " ONEIOS_GL_MESA_VERSION ", "
#if ONEIOS_GL_RADEONSI
	    "radeonsi through oneios_radeonsi_screen_create"
#else
	    "no GPU screen in this build"
#endif
	    "; softpipe when asked)";
}

bool
ngl_soft(void)
{
	return soft;
}

bool
ngl_in_place(void)
{
	static int copy = -1;
	if (copy < 0) {
		const char *e = getenv("ONEIOS_GL_SURFACE");
		copy = e != NULL && strcmp(e, "copy") == 0;
	}
	return soft && !copy;
}

const char *
ngl_no_screen_why(void)
{
	return no_screen[0] ? no_screen : "the screen was not made yet";
}

/*
 * Whether softpipe stands in when there is no radeonsi screen:
 *   ONEIOS_GL_SOFTPIPE=1|0 in the environment wins (the host test, the probe);
 *   the boot argument oneios-gl-softpipe=1: every process, backboardd too (a test boot of the EAGL road in the render
 *   server before radeonsi works); oneios-gl-softpipe=0: no process;
 *   neither: every process but backboardd. CoreImage has no CPU renderer on iOS 16 and dies on a nil EAGLContext
 *   (docs/gpu.md 5.2), and a GL application has nothing else: a correct GL on the CPU is better than none. backboardd
 *   keeps CoreAnimation's own CPU renderer, which is faster than CoreAnimation on softpipe.
 */
static bool __attribute__((unused))
softpipe_allowed(void)
{
	unsigned v = 0;
	if (getenv("ONEIOS_GL_SOFTPIPE") != NULL && getenv("ONEIOS_GL_SOFTPIPE")[0] != 0) {
		return ngl_env_on("ONEIOS_GL_SOFTPIPE", false);
	}
	if (ngl_boot_arg("oneios-gl-softpipe", &v)) {
		return v != 0;
	}
	const char *prog = getprogname();
	return prog == NULL || strcmp(prog, "backboardd") != 0;
}

/* st_api_create_context calls fscreen->get_param unconditionally (st_manager.c): without it the first context
 * jumps to address 0 (NegroniGL's first phone run, 2026-10-25). ST_MANAGER_BROKEN_INVALIDATE = 0: no window
 * drawables (CoreAnimation draws into framebuffers on IOSurface images), nothing for glViewport to invalidate. */
static int
ngl_get_param(struct pipe_frontend_screen *fs, enum st_manager_param param)
{
	(void)fs;
	(void)param;
	return 0;
}

bool
ngl_init(void)
{
	pthread_mutex_lock(&init_lock);
	if (fscreen == NULL && !tried) {
		tried = true;
		struct pipe_screen *screen = NULL;
		char why[256] = "";
#if ONEIOS_GL_RADEONSI
		if (!ngl_env_on("ONEIOS_GL_SOFTPIPE_ONLY", false)) {
			screen = oneios_radeonsi_screen_create(why, sizeof(why));
		}
#else
		snprintf(why, sizeof(why), "this OneiOSRadeonGL was built without radeonsi (softpipe-only Mesa)");
#endif
#ifdef GALLIUM_SOFTPIPE            /* driver_softpipe's define: softpipe is in this Mesa build */
		if (screen == NULL && softpipe_allowed()) {
			struct sw_winsys *ws = ngl_sw_winsys_create();
			screen = ws ? softpipe_create_screen(ws) : NULL;
			if (screen == NULL && ws != NULL) {
				ws->destroy(ws);
			}
			soft = screen != NULL;
		}
#endif
		if (screen != NULL) {
			struct pipe_frontend_screen *f = calloc(1, sizeof(*f));
			if (f == NULL) {
				screen->destroy(screen);
				snprintf(no_screen, sizeof(no_screen), "out of memory");
			} else {
				f->screen = screen;
				f->get_param = ngl_get_param;  /* called by every st_api_create_context */
				ngl_image_hooks(f);     /* before any context: st reads them at creation */
				fscreen = f;
				ngl_logc('I', "SCR", "screen %s (%s)%s", screen->get_name(screen), screen->get_vendor(screen),
				    soft ? ", softpipe: the CPU draws" : "");
			}
		} else {
			snprintf(no_screen, sizeof(no_screen), "no radeonsi screen (%s); softpipe not allowed in this process "
			    "(backboardd keeps CoreAnimation's CPU renderer; oneios-gl-softpipe=1, ONEIOS_GL_SOFTPIPE=1)", why[0] ? why : "?");
			ngl_logc('E', "SCR", "%s", no_screen);
		}
	}
	bool ok = fscreen != NULL;
	pthread_mutex_unlock(&init_lock);
	return ok;
}

struct pipe_screen *
ngl_screen(void)
{
	return fscreen ? fscreen->screen : NULL;
}

const char *
ngl_screen_name(void)
{
	struct pipe_screen *s = ngl_screen();
	return s ? s->get_name(s) : "none";
}

struct ngl_ctx *
ngl_create(int api, struct ngl_ctx *share)
{
	if (!ngl_init()) {
		return NULL;
	}
	struct st_context_attribs attribs = {
		.profile = API_OPENGLES2,
		.major = api >= 3 ? 3 : 2,
		/* 3.1: glGetTexLevelParameteriv (rectangle texture sizes in gl_hand.c) */
		.minor = api >= 3 ? 1 : 0,
	};
	enum st_context_error err = ST_CONTEXT_SUCCESS;
	struct st_context *st = st_api_create_context(fscreen, &attribs, &err, share ? share->st : NULL);
	if (st == NULL && attribs.major == 3 && attribs.minor == 1) {
		attribs.minor = 0;              /* a driver without ES 3.1 */
		st = st_api_create_context(fscreen, &attribs, &err, share ? share->st : NULL);
	}
	if (st == NULL) {
		ngl_logc('E', "CTX", "GLES %d.%d context failed: %d", attribs.major, attribs.minor, err);
		return NULL;
	}
	struct ngl_ctx *c = calloc(1, sizeof(*c));
	if (c == NULL) {
		st_destroy_context(st);
		return NULL;
	}
	c->st = st;
	c->api = attribs.major;
	st->frontend_context = c;
	return c;
}

bool
ngl_make_current(struct ngl_ctx *c)
{
	return st_api_make_current(c ? c->st : NULL, NULL, NULL);
}

void
ngl_destroy(struct ngl_ctx *c)
{
	if (c == NULL) {
		return;
	}
	if (st_api_get_current() == c->st) {
		st_api_make_current(NULL, NULL, NULL);
	}
	st_destroy_context(c->st);
	free(c);
}

void
ngl_ctx_flush(struct ngl_ctx *c)
{
	if (c != NULL) {
		st_context_flush(c->st, 0, NULL, NULL, NULL);
	}
}

struct pipe_context *
ngl_pipe(void)
{
	struct st_context *st = st_api_get_current();
	return st ? st->pipe : NULL;
}

/* ---- self tests ---- */

static double
ms_now(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (double)ts.tv_sec * 1e3 + (double)ts.tv_nsec / 1e6;
}

static GLuint
shader(GLenum type, const char *src)
{
	GLuint s = glCreateShader(type);
	glShaderSource(s, 1, &src, NULL);
	glCompileShader(s);
	GLint ok = 0;
	glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
	if (!ok) {
		char log[256] = "";
		glGetShaderInfoLog(s, sizeof(log), NULL, log);
		ngl_logc('E', "TEST", "selftest shader: %s", log);
	}
	return s;
}

static GLuint
red_program(void)
{
	GLuint prog = glCreateProgram();
	glAttachShader(prog, shader(GL_VERTEX_SHADER,
	    "#version 300 es\nlayout(location = 0) in vec2 p;\nvoid main() { gl_Position = vec4(p, 0.0, 1.0); }\n"));
	glAttachShader(prog, shader(GL_FRAGMENT_SHADER,
	    "#version 300 es\nprecision mediump float;\nout vec4 c;\nvoid main() { c = vec4(1.0, 0.0, 0.0, 1.0); }\n"));
	glLinkProgram(prog);
	return prog;
}

/* clear blue, red triangle over the lower left half of the bound 256x256 framebuffer */
static void
draw_test_frame(GLuint prog)
{
	static const float tri[] = {-1, -1, 1, -1, -1, 1};
	GLuint vbo;
	glGenBuffers(1, &vbo);
	glBindBuffer(GL_ARRAY_BUFFER, vbo);
	glBufferData(GL_ARRAY_BUFFER, sizeof(tri), tri, GL_STATIC_DRAW);
	glEnableVertexAttribArray(0);
	glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, 0);
	glUseProgram(prog);
	glViewport(0, 0, 256, 256);
	glClearColor(0, 0, 1, 1);
	glClear(GL_COLOR_BUFFER_BIT);
	glDrawArrays(GL_TRIANGLES, 0, 3);
	glDisableVertexAttribArray(0);
	glBindBuffer(GL_ARRAY_BUFFER, 0);
	glDeleteBuffers(1, &vbo);
}

bool
ngl_selftest(void)
{
	double t0 = ms_now();
	struct ngl_ctx *c = ngl_create(3, NULL);
	if (c == NULL || !ngl_make_current(c)) {
		ngl_logc('E', "TEST", "selftest: no context");
		ngl_destroy(c);
		return false;
	}
	double t_ctx = ms_now() - t0;
	ngl_logc('I', "TEST", "GL_VERSION %s / GL_RENDERER %s / GLSL %s", glGetString(GL_VERSION),
	    glGetString(GL_RENDERER), glGetString(GL_SHADING_LANGUAGE_VERSION));
	GLuint tex, fbo;
	glGenTextures(1, &tex);
	glBindTexture(GL_TEXTURE_2D, tex);
	glTexStorage2D(GL_TEXTURE_2D, 1, GL_RGBA8, 256, 256);
	glGenFramebuffers(1, &fbo);
	glBindFramebuffer(GL_FRAMEBUFFER, fbo);
	glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
	GLenum fbs = glCheckFramebufferStatus(GL_FRAMEBUFFER);
	GLuint prog = red_program();
	GLint linked = 0;
	glGetProgramiv(prog, GL_LINK_STATUS, &linked);
	double t1 = ms_now();
	draw_test_frame(prog);
	static uint8_t px[256 * 256 * 4];
	glReadPixels(0, 0, 256, 256, GL_RGBA, GL_UNSIGNED_BYTE, px);
	double t_draw = ms_now() - t1;
	GLenum err = glGetError();
	const uint8_t *a = &px[(32 * 256 + 32) * 4], *b = &px[(224 * 256 + 224) * 4];
	bool ok = fbs == GL_FRAMEBUFFER_COMPLETE && linked && err == GL_NO_ERROR &&
	    a[0] == 255 && a[1] == 0 && a[2] == 0 && a[3] == 255 && b[0] == 0 && b[1] == 0 && b[2] == 255 && b[3] == 255;
	ngl_logc(ok ? 'I' : 'E', "TEST", "selftest fbo %#x linked %d err %#x px(32,32) %02x%02x%02x%02x px(224,224) "
	    "%02x%02x%02x%02x context %.1f ms draw+readback %.2f ms: %s", fbs, linked, err, a[0], a[1], a[2], a[3], b[0],
	    b[1], b[2], b[3], t_ctx, t_draw, ok ? "PASS" : "FAIL");
	glBindFramebuffer(GL_FRAMEBUFFER, 0);
	glDeleteFramebuffers(1, &fbo);
	glDeleteTextures(1, &tex);
	glDeleteProgram(prog);
	ngl_make_current(NULL);
	ngl_destroy(c);
	return ok;
}

/*
 * A 256x256 BGRA IOSurface as the colour buffer (the road CoreAnimation's render targets take: gliSetInteger
 * 0x38e then a framebuffer), a triangle drawn, the surface read on the CPU through its own mapping after the
 * flush (copy mode: the flush writes the surface back, surface.c).
 */
bool
ngl_surface_selftest(IOSurfaceRef s)
{
	if (s == NULL || IOSurfaceGetWidth(s) < 256 || IOSurfaceGetHeight(s) < 256) {
		ngl_logc('E', "TEST", "surface selftest: no 256x256 IOSurface");
		return false;
	}
	struct ngl_gli *g = ngl_gli_new(3, NULL);
	if (g == NULL) {
		return false;
	}
	NGL_CUR(g);
	GLuint tex, fbo;
	glGenTextures(1, &tex);
	glBindTexture(GL_TEXTURE_2D, tex);
	bool bound = ngl_bind_surface(s, GL_TEXTURE_2D, GL_BGRA_EXT, GL_UNSIGNED_BYTE, 0, false, "selftest");
	glGenFramebuffers(1, &fbo);
	glBindFramebuffer(GL_FRAMEBUFFER, fbo);
	glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
	GLenum fbs = glCheckFramebufferStatus(GL_FRAMEBUFFER);
	GLuint prog = red_program();
	draw_test_frame(prog);
	ngl_surf_note_write(g->st);
	ngl_flush();
	glFinish();
	IOSurfaceLock(s, kIOSurfaceLockReadOnly, NULL);
	const uint8_t *base = IOSurfaceGetBaseAddress(s);
	size_t row = IOSurfaceGetBytesPerRow(s);
	/* GL row 32 from the bottom = memory row 32 (GL's origin is the first row in memory) */
	const uint8_t *a = base + 32 * row + 32 * 4, *b = base + 224 * row + 224 * 4;
	bool ok = bound && fbs == GL_FRAMEBUFFER_COMPLETE && a[2] == 255 && a[0] == 0 && b[0] == 255 && b[2] == 0;
	ngl_logc(ok ? 'I' : 'E', "TEST", "surface selftest (%s) bound %d fbo %#x BGRA(32,32) %02x%02x%02x%02x "
	    "BGRA(224,224) %02x%02x%02x%02x: %s", ngl_in_place() ? "in place" : "copied", bound, fbs, a[0], a[1], a[2],
	    a[3], b[0], b[1], b[2], b[3], ok ? "PASS" : "FAIL");
	IOSurfaceUnlock(s, kIOSurfaceLockReadOnly, NULL);
	glBindFramebuffer(GL_FRAMEBUFFER, 0);
	glDeleteFramebuffers(1, &fbo);
	ngl_h_DeleteTextures(g, 1, &tex);
	glDeleteProgram(prog);
	extern int gliDestroyContext(void *rctx);
	gliDestroyContext(g);
	return ok;
}

/* ---- the gate ---- */

/* 1 = exists, 0 = ENOENT, -1 = cannot check */
static int
exists(const char *path)
{
	struct stat sb;
	if (stat(path, &sb) == 0) {
		return 1;
	}
	return errno == ENOENT ? 0 : -1;
}

/* name listed in the file at path: 1 yes, 0 no, -1 listed as "-name", -2 no file */
static int
listed(const char *path, const char *prog)
{
	FILE *f = fopen(path, "r");
	if (f == NULL) {
		return -2;
	}
	int r = 0;
	char line[512];
	while (r >= 0 && fgets(line, sizeof(line), f) != NULL) {
		char *save = NULL;
		for (char *w = strtok_r(line, " ,\t\r\n", &save); w != NULL; w = strtok_r(NULL, " ,\t\r\n", &save)) {
			if (w[0] == '#') {
				break;
			}
			if (w[0] == '-' && strcmp(w + 1, prog) == 0) {
				r = -1;
				break;
			}
			if (strcmp(w, "*") == 0 || strcmp(w, prog) == 0) {
				r = 1;
			}
		}
	}
	fclose(f);
	return r;
}

/* a BGRA IOSurface of w x h (the preflight's and the probe's), NULL when none can be made */
static IOSurfaceRef
new_surface(int w, int h)
{
#ifdef __APPLE__
	const void *keys[] = {kIOSurfaceWidth, kIOSurfaceHeight, kIOSurfaceBytesPerElement, kIOSurfacePixelFormat,
	    kIOSurfaceBytesPerRow};
	int bpe = 4, fmt = 'BGRA', pitch = w * 4;
	CFNumberRef vals[5] = {CFNumberCreate(NULL, kCFNumberIntType, &w), CFNumberCreate(NULL, kCFNumberIntType, &h),
	    CFNumberCreate(NULL, kCFNumberIntType, &bpe), CFNumberCreate(NULL, kCFNumberIntType, &fmt),
	    CFNumberCreate(NULL, kCFNumberIntType, &pitch)};
	CFDictionaryRef props = CFDictionaryCreate(NULL, keys, (const void **)vals, 5, &kCFTypeDictionaryKeyCallBacks,
	    &kCFTypeDictionaryValueCallBacks);
	IOSurfaceRef s = IOSurfaceCreate(props);
	CFRelease(props);
	for (int i = 0; i < 5; i++) {
		CFRelease(vals[i]);
	}
	return s;
#else
	extern IOSurfaceRef host_iosurface_create(size_t w, size_t h, uint32_t fourcc);
	return host_iosurface_create((size_t)w, (size_t)h, 'BGRA');
#endif
}

/*
 * Whether this process gets OneiOSRadeonGL (asked once, from gldInitializeLibrary). A closed gate leaves OpenGLES as
 * it was without us: GLEngine's table, our gldChoosePixelFormat refuses, EAGLContexts are nil, and CoreAnimation
 * keeps its CPU renderer (CA::WindowServer::EAGLServer::renderer() finds no context).
 *   1. ONEIOS_GL=1|0 in the environment wins (tools).
 *   2. BUNDLE/off exists, or cannot be checked (anything but ENOENT): closed (a kill switch must not fail open).
 *   3. BUNDLE/OneiOSRadeonGL.clients, when there: "*" every process, "NAME", "-NAME" never; not listed: closed.
 *      No such file: every process (the kernel's IOGLESBundleName, oneios-gpu-amd-gl=1, is the switch).
 *   4. A screen (ngl_init): radeonsi, or softpipe only when asked.
 *   5. In backboardd (or with ONEIOS_GL_PREFLIGHT=1): the triangle and the IOSurface self tests must pass before
 *      CoreAnimation's render server is handed a context (NegroniGL's / ios-spes's preflight); ONEIOS_GL_PREFLIGHT=0
 *      skips it.
 */
bool
ngl_gate(const char *prog, const char *bundle_dir, const char **why)
{
	static char buf[600];
	const char *e = getenv("ONEIOS_GL");
	*why = NULL;
	if (e != NULL && e[0] != 0) {
		if (!ngl_env_on("ONEIOS_GL", false)) {
			*why = "ONEIOS_GL=0";
			return false;
		}
	} else if (bundle_dir != NULL) {
		char path[700];
		snprintf(path, sizeof(path), "%s/off", bundle_dir);
		int x = exists(path);
		if (x != 0) {
			snprintf(buf, sizeof(buf), x > 0 ? "%s exists" : "cannot check %s: fail closed", path);
			*why = buf;
			return false;
		}
		snprintf(path, sizeof(path), "%s/OneiOSRadeonGL.clients", bundle_dir);
		int l = listed(path, prog);
		if (l == -1 || l == 0) {
			snprintf(buf, sizeof(buf), l == 0 ? "not listed in %s" : "listed as -%s in %s", l == 0 ? path : prog,
			    path);
			*why = buf;
			return false;
		}
	}
	if (!ngl_init()) {
		*why = ngl_no_screen_why();
		return false;
	}
	bool pre = strcmp(prog, "backboardd") == 0;
	if (getenv("ONEIOS_GL_PREFLIGHT") != NULL && getenv("ONEIOS_GL_PREFLIGHT")[0] != 0) {
		pre = ngl_env_on("ONEIOS_GL_PREFLIGHT", false);
	}
	if (pre) {
		double t0 = ms_now();
		IOSurfaceRef s = new_surface(256, 256);
		bool tri = ngl_selftest();
		bool surf = tri && s != NULL && ngl_surface_selftest(s);
		if (s != NULL) {
			CFRelease(s);
		}
		ngl_logc(tri && surf ? 'I' : 'E', "GATE", "preflight %s: screen %s, triangle %s, IOSurface %s, %.0f ms",
		    tri && surf ? "PASS" : "FAIL", ngl_screen_name(), tri ? "PASS" : "FAIL", surf ? "PASS" : "FAIL",
		    ms_now() - t0);
		if (!(tri && surf)) {
			*why = "the preflight failed (triangle or IOSurface self test)";
			return false;
		}
	}
	return true;
}

/*
 * The lasting probe (oneios-gl-probe on the phone, tests/oneios_gl_test.c --bundle): the screen, the triangle and an
 * IOSurface round trip in this process, one line each into the log, and a one-line report. 0 = all PASS.
 */
int
oneios_gl_probe(char *report, size_t size)
{
	bool screen = ngl_init();
	IOSurfaceRef s = screen ? new_surface(256, 256) : NULL;
	bool tri = screen && ngl_selftest();
	bool surf = tri && s != NULL && ngl_surface_selftest(s);
	if (s != NULL) {
		CFRelease(s);
	}
	if (report != NULL && size > 0) {
		snprintf(report, size, "%s; screen %s%s; triangle %s; IOSurface %s (%s)", ngl_version(),
		    screen ? ngl_screen_name() : "none: ", screen ? "" : ngl_no_screen_why(), tri ? "PASS" : "FAIL",
		    surf ? "PASS" : "FAIL", ngl_in_place() ? "in place" : "copied");
	}
	return screen && tri && surf ? 0 : 1;
}
