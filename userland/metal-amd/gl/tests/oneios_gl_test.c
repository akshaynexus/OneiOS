/*
 * Copyright (c) 2026 OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * OneiOSRadeonGL's host test (userland/metal-amd/gl/host-test.sh runs it, on softpipe, on the build laptop). It drives
 * the GL layer the way iOS 16.5 does: EAGL's GLI entry points (gliGetVersion, gliChoosePixelFormat,
 * gliCreateContextWithShared with two 1036-slot tables, gliSetInteger 0x38e/0x399, gliBindViewES, gliPresentViewES,
 * gliDestroyContext), and every GL call through the dispatch table as CoreAnimation does: disp[slot](rctx, args).
 * IOSurfaces are host/iosurface_host.c's stand-ins; the CAEAGLLayer's native window is a stand-in table below.
 *
 *   oneios_gl_test             every check below; "oneios-gl-test: N passed, M failed", exit 0 when all pass
 *   oneios_gl_test --no-screen the build has no GPU screen and softpipe is not asked for: the gate closes and says why
 *
 * ONEIOS_GL_SOFTPIPE=1 is set by host-test.sh; ONEIOS_GL_SURFACE=copy makes the IOSurfaces copies (the A56's way).
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "oneios_gl.h"
#include "gld.h"

int gliGetVersion(int *, int *, int *);
int gliChoosePixelFormat(void **, const int *);
int gliCreateContextWithShared(void **, void *, void *, void *, void *, uint32_t);
int gliDestroyContext(void *);
int gliSetInteger(void *, uint32_t, const int32_t *);
int gliBindViewES(void *, void *, uint32_t, uint32_t);
int gliPresentViewES(void *, uint32_t, double);

static int passed, failed;

static void
check(const char *area, bool ok, const char *fmt, ...) __attribute__((format(printf, 3, 4)));

#include <stdarg.h>
static void
check(const char *area, bool ok, const char *fmt, ...)
{
	char what[400];
	va_list ap;
	va_start(ap, fmt);
	vsnprintf(what, sizeof(what), fmt, ap);
	va_end(ap);
	printf("oneios-gl-test: %s: %s: %s\n", ok ? "pass" : "FAIL", area, what);
	ok ? passed++ : failed++;
}

/* ---- the slots, by name (gli-slots.txt), called as CoreAnimation calls them ---- */

static void *const *disp;
static void *rctx;

static void *
slot(const char *name)
{
	for (int i = 0; i < NGL_SLOTS; i++) {
		if (strcmp(ngl_slot_names[i], name) == 0) {
			return disp[i];
		}
	}
	fprintf(stderr, "oneios-gl-test: no slot %s\n", name);
	exit(2);
}

#define S(name, type) ((type)slot(name))
typedef void (*v_e)(void *, GLenum);
typedef void (*v_u)(void *, GLuint);
typedef void (*v_eu)(void *, GLenum, GLuint);
typedef void (*v_iu)(void *, GLsizei, GLuint *);
typedef void (*v_icu)(void *, GLsizei, const GLuint *);
typedef void (*v_ffff)(void *, GLfloat, GLfloat, GLfloat, GLfloat);
typedef void (*v_iiii)(void *, GLint, GLint, GLsizei, GLsizei);
typedef void (*v_b)(void *, GLbitfield);
typedef void (*v_)(void *);
typedef GLuint (*u_e)(void *, GLenum);
typedef GLuint (*u_)(void *);
typedef GLenum (*e_e)(void *, GLenum);
typedef void (*v_eeeui)(void *, GLenum, GLenum, GLenum, GLuint, GLint);
typedef void (*v_eeeu)(void *, GLenum, GLenum, GLenum, GLuint);
typedef void (*v_eii)(void *, GLenum, GLint, GLsizei);
typedef void (*v_uiscs)(void *, GLuint, GLsizei, const GLchar *const *, const GLint *);
typedef void (*v_uiep)(void *, GLuint, GLint, GLenum, GLboolean, GLsizei, const void *);
typedef void (*v_epe)(void *, GLenum, GLsizeiptr, const void *, GLenum);
typedef void (*v_iiiieep)(void *, GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, void *);
typedef void (*v_iibf)(void *, GLint, GLsizei, GLboolean, const GLfloat *);
typedef void (*v_uiu)(void *, GLuint, GLuint, const GLchar *);
typedef void (*v_ueip)(void *, GLuint, GLenum, GLint *);

static GLuint
program_of(const char *vs, const char *fs)
{
	GLuint s[2], p = S("glCreateProgram", u_)(rctx);
	const char *src[2] = {vs, fs};
	GLenum kind[2] = {GL_VERTEX_SHADER, GL_FRAGMENT_SHADER};
	for (int i = 0; i < 2; i++) {
		s[i] = S("glCreateShader", u_e)(rctx, kind[i]);
		S("glShaderSource", v_uiscs)(rctx, s[i], 1, &src[i], NULL);
		S("glCompileShader", v_u)(rctx, s[i]);
		S("glAttachShader", v_eu)(rctx, p, s[i]);
	}
	S("glBindAttribLocation", v_uiu)(rctx, p, 0, "p");
	S("glLinkProgram", v_u)(rctx, p);
	GLint linked = 0;
	S("glGetProgramiv", v_ueip)(rctx, p, GL_LINK_STATUS, &linked);
	return linked ? p : 0;
}

static const char red_vs[] = "attribute vec2 p;\nvoid main() { gl_Position = vec4(p, 0.0, 1.0); }\n";
static const char red_fs[] = "precision mediump float;\nvoid main() { gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";
/* CA's style: a rectangle sampler with coordinates in pixels (shaders.c rewrites it) */
static const char tex_vs[] = "attribute vec2 p;\nvarying vec2 t;\nvoid main() { t = (p * 0.5 + 0.5) * 256.0; "
    "gl_Position = vec4(p, 0.0, 1.0); }\n";
static const char tex_fs[] = "#extension GL_ARB_texture_rectangle : enable\nprecision mediump float;\nvarying vec2 t;\n"
    "uniform sampler2DRect img;\nvoid main() { gl_FragColor = texture2DRect(img, t); }\n";

static void
quad(GLuint prog, bool whole)
{
	static const float tri[] = {-1, -1, 1, -1, -1, 1}, all[] = {-1, -1, 1, -1, -1, 1, 1, 1};
	GLuint vbo;
	S("glGenBuffers", v_iu)(rctx, 1, &vbo);
	S("glBindBuffer", v_eu)(rctx, GL_ARRAY_BUFFER, vbo);
	S("glBufferData", v_epe)(rctx, GL_ARRAY_BUFFER, whole ? sizeof(all) : sizeof(tri), whole ? all : tri, GL_STATIC_DRAW);
	S("glEnableVertexAttribArray", v_u)(rctx, 0);
	S("glVertexAttribPointer", v_uiep)(rctx, 0, 2, GL_FLOAT, GL_FALSE, 0, NULL);
	S("glUseProgram", v_u)(rctx, prog);
	S("glDrawArrays", v_eii)(rctx, whole ? GL_TRIANGLE_STRIP : GL_TRIANGLES, 0, whole ? 4 : 3);
	S("glDisableVertexAttribArray", v_u)(rctx, 0);
	S("glBindBuffer", v_eu)(rctx, GL_ARRAY_BUFFER, 0);
	S("glDeleteBuffers", v_icu)(rctx, 1, &vbo);
}

/* the BGRA bytes at memory (x, y) of a surface, as one word b | g << 8 | r << 16 | a << 24 */
static uint32_t
mem_px(IOSurfaceRef s, int x, int y)
{
	IOSurfaceLock(s, kIOSurfaceLockReadOnly, NULL);
	const uint8_t *p = (const uint8_t *)IOSurfaceGetBaseAddress(s) + (size_t)y * IOSurfaceGetBytesPerRow(s) + x * 4;
	uint32_t v = p[0] | p[1] << 8 | p[2] << 16 | (uint32_t)p[3] << 24;
	IOSurfaceUnlock(s, kIOSurfaceLockReadOnly, NULL);
	return v;
}

static void
fill(IOSurfaceRef s, uint32_t bgra)
{
	IOSurfaceLock(s, 0, NULL);                      /* a writer: the seed moves at the unlock */
	uint8_t *base = IOSurfaceGetBaseAddress(s);
	for (size_t y = 0; y < IOSurfaceGetHeight(s); y++) {
		for (size_t x = 0; x < IOSurfaceGetWidth(s); x++) {
			memcpy(base + y * IOSurfaceGetBytesPerRow(s) + x * 4, &bgra, 4);
		}
	}
	IOSurfaceUnlock(s, 0, NULL);
}

/* gliSetInteger 0x38e then 0x399, as -texImageIOSurface:...invert: does */
static int
tex_image_iosurface(IOSurfaceRef s, bool invert)
{
	int32_t p[9] = {(int32_t)IOSurfaceGetID(s), GL_TEXTURE_2D, GL_RGBA, 256, 256, GL_BGRA_EXT, GL_UNSIGNED_BYTE, 0, 0};
	int32_t q[2] = {GL_TEXTURE_2D, invert};
	int r = gliSetInteger(rctx, 0x38e, p);
	return r ? r : gliSetInteger(rctx, 0x399, q);
}

#define RED    0xffff0000u      /* BGRA bytes 00 00 ff ff */
#define GREEN  0xff00ff00u
#define BLUE   0xff0000ffu
#define YELLOW 0xffffff00u

/* ---- the CAEAGLLayer's native window (view.c's table), three drawables in turn ---- */
struct fake_nw {
	uint32_t version, pad;
	bool (*attach)(void *, uint32_t, void *const *);
	void (*detach)(void *);
	IOSurfaceRef (*begin_iosurface)(void *);
	bool (*swap)(void *, uint32_t, double);
	void *collect;
	uint32_t (*begin_properties)(void *);
	IOSurfaceRef drawables[3];
	int next, attached, swaps;
	IOSurfaceRef swapped[8];
};
static bool nw_attach(void *nw, uint32_t fourcc, void *const *cb) { (void)cb; ((struct fake_nw *)nw)->attached += fourcc == 'BGRA'; return true; }
static void nw_detach(void *nw) { ((struct fake_nw *)nw)->attached--; }
static IOSurfaceRef nw_begin(void *nw) { struct fake_nw *f = nw; return f->drawables[f->next % 3]; }
static uint32_t nw_props(void *nw) { (void)nw; return 0; }
static bool
nw_swap(void *nw, uint32_t mode, double t)
{
	(void)mode;
	(void)t;
	struct fake_nw *f = nw;
	if (f->swaps < 8) {
		f->swapped[f->swaps] = f->drawables[f->next % 3];
	}
	f->swaps++;
	f->next++;
	return true;
}

static int
no_screen(void)
{
	const char *why = NULL;
	bool open = ngl_gate("backboardd", NULL, &why);
	check("no-screen", !open && why != NULL && strstr(why, "softpipe not allowed") != NULL,
	    "the gate closes without a screen: %s", why ? why : "(no reason)");
	return failed ? 1 : 0;
}

int
main(int argc, char **argv)
{
	if (argc > 1 && strcmp(argv[1], "--no-screen") == 0) {
		return no_screen();
	}
	char dir[] = "/tmp/oneios-gl-test-XXXXXX";
	if (mkdtemp(dir) == NULL) {
		return 2;
	}
	char path[200];
	const char *why = NULL;

	/* the 20F66 code words: the register writes nothing on another OpenGLES */
	struct ngl_gli_words w20F66 = {{0xb4000060u, 0x52800028u}, 0xf902d2e0u, {0x911682b5u, 0xf94002a0u}};
	struct ngl_gli_words w20H392 = {{0xb4000060u, 0x52800028u}, 0xf906d2e0u, {0x913682b5u, 0xf94002a0u}};
	check("gli", ngl_gli_words_match(&w20F66) && !ngl_gli_words_match(&w20H392),
	    "20F66's eagl_init words match, 20H392's (NegroniGL's) do not");
	check("gli", ngl_dispatch_missing < 80, "%u of 409 named slots have no Mesa function (CoreAnimation calls none of "
	    "them but the debug markers and glFramebufferParameteriAPPLE)", ngl_dispatch_missing);

	/* the gate */
	setenv("ONEIOS_GL_PREFLIGHT", "0", 1);
	check("gate", ngl_gate("backboardd", dir, &why) && why == NULL, "no files: open");
	snprintf(path, sizeof(path), "%s/OneiOSRadeonGL.clients", dir);
	FILE *f = fopen(path, "w");
	fputs("SomeApp,-backboardd\n", f);
	fclose(f);
	bool g = ngl_gate("backboardd", dir, &why);
	check("gate", !g && why && strstr(why, "-backboardd"), "-backboardd: %s", why);
	check("gate", ngl_gate("SomeApp", dir, &why), "a listed name: open");
	f = fopen(path, "w");
	fputs("*\n", f);
	fclose(f);
	snprintf(path, sizeof(path), "%s/off", dir);
	f = fopen(path, "w");
	fclose(f);
	g = ngl_gate("backboardd", dir, &why);
	check("gate", !g && why && strstr(why, "/off exists"), "BUNDLE/off: %s", why);
	unlink(path);
	setenv("ONEIOS_GL", "0", 1);
	g = ngl_gate("backboardd", dir, &why);
	check("gate", !g && why && strcmp(why, "ONEIOS_GL=0") == 0, "ONEIOS_GL=0: %s", why);
	unsetenv("ONEIOS_GL");
	setenv("ONEIOS_GL_PREFLIGHT", "1", 1);
	check("gate", ngl_gate("backboardd", dir, &why), "the preflight (triangle and IOSurface self tests) passes on %s",
	    ngl_screen_name());
	snprintf(path, sizeof(path), "%s/OneiOSRadeonGL.clients", dir);
	unlink(path);
	rmdir(dir);

	/* EAGL's start: version, pixel format, a context of API 3 with two tables */
	int a = 0, b = 0, c = 0;
	void *pf = NULL;
	int version = gliGetVersion(&a, &b, &c), chosen = gliChoosePixelFormat(&pf, NULL);
	check("eagl", version != 0 && chosen == 0 && pf != NULL &&
	    (((struct ngl_pixel_format *)pf)->flags & 0x20100) == 0x20100, "gliGetVersion %d.%d %#x, pixel format flags %#x",
	    a, b, c, pf ? ((struct ngl_pixel_format *)pf)->flags : 0);
	static void *t1[NGL_SLOTS], *t2[NGL_SLOTS];
	int r = gliCreateContextWithShared(&rctx, pf, (void *)0x5ea, t1, t2, 0x40);
	check("eagl", r == 0 && rctx != NULL && memcmp(t1, ngl_dispatch, sizeof(t1)) == 0 &&
	    memcmp(t2, ngl_dispatch, sizeof(t2)) == 0, "gliCreateContextWithShared API 3: %#x, both tables filled", r);
	if (rctx == NULL) {
		goto out;
	}
	disp = t1;
	typedef const GLubyte *(*s_e)(void *, GLenum);
	const char *renderer = (const char *)S("glGetString", s_e)(rctx, GL_RENDERER);
	check("eagl", renderer != NULL, "GL_RENDERER %s through slot %s", renderer ? renderer : "(none)", "glGetString");

	/* a render target IOSurface (CA's -attachImage:toCoreSurface:): clear green, red triangle, flush, read memory */
	IOSurfaceRef target = host_iosurface_create(256, 256, 'BGRA');
	GLuint tex = 0, fbo = 0;
	S("glGenTextures", v_iu)(rctx, 1, &tex);
	S("glBindTexture", v_eu)(rctx, GL_TEXTURE_2D, tex);
	check("surface", tex_image_iosurface(target, false) == 0, "gliSetInteger 0x38e + 0x399: IOSurface %u as texture %u",
	    IOSurfaceGetID(target), tex);
	S("glGenFramebuffers", v_iu)(rctx, 1, &fbo);
	S("glBindFramebuffer", v_eu)(rctx, GL_FRAMEBUFFER, fbo);
	S("glFramebufferTexture2D", v_eeeui)(rctx, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
	GLenum fbs = S("glCheckFramebufferStatus", e_e)(rctx, GL_FRAMEBUFFER);
	GLuint red = program_of(red_vs, red_fs);
	S("glViewport", v_iiii)(rctx, 0, 0, 256, 256);
	S("glClearColor", v_ffff)(rctx, 0, 1, 0, 1);
	S("glClear", v_b)(rctx, GL_COLOR_BUFFER_BIT);
	quad(red, false);
	uint8_t px[4] = {0};
	S("glReadPixels", v_iiiieep)(rctx, 32, 32, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
	S("glFlush", v_)(rctx);
	uint32_t m1 = mem_px(target, 32, 32), m2 = mem_px(target, 224, 224);
	check("render", fbs == GL_FRAMEBUFFER_COMPLETE && red != 0 && px[0] == 255 && px[1] == 0 && m1 == RED && m2 == GREEN,
	    "%s: framebuffer %#x, program %u, GL (32,32) %02x%02x%02x%02x; after glFlush the IOSurface's memory (32,32) %08x "
	    "(red %08x), (224,224) %08x (green %08x)", ngl_in_place() ? "in place" : "copied", fbs, red, px[0], px[1], px[2],
	    px[3], m1, RED, m2, GREEN);

	/* inverted (-attachImage:...invertedRender:YES): memory row 0 is GL's top row */
	GLuint tex_inv = 0;
	IOSurfaceRef inv = host_iosurface_create(256, 256, 'BGRA');
	S("glGenTextures", v_iu)(rctx, 1, &tex_inv);
	S("glBindTexture", v_eu)(rctx, GL_TEXTURE_2D, tex_inv);
	tex_image_iosurface(inv, true);
	S("glFramebufferTexture2D", v_eeeui)(rctx, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex_inv, 0);
	S("glClear", v_b)(rctx, GL_COLOR_BUFFER_BIT);
	quad(red, false);
	S("glFlush", v_)(rctx);
	uint32_t i1 = mem_px(inv, 40, 200), i2 = mem_px(inv, 200, 40);
	check("invert", i1 == RED && i2 == GREEN, "memory (40,200) is GL (40,55), in the triangle: %08x (red); memory (200,40) "
	    "is GL (200,215), outside it: %08x (green; the right way up it would be red)", i1, i2);

	/* a sampled IOSurface (an app's layer: -texImageIOSurface:), a rectangle sampler in pixels, then the app writes it */
	IOSurfaceRef layer = host_iosurface_create(256, 256, 'BGRA');
	fill(layer, BLUE);
	GLuint src = 0;
	S("glFramebufferTexture2D", v_eeeui)(rctx, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
	S("glActiveTexture", v_e)(rctx, GL_TEXTURE0);
	S("glGenTextures", v_iu)(rctx, 1, &src);
	S("glBindTexture", v_eu)(rctx, NGL_GL_TEXTURE_RECTANGLE, src);
	int32_t p[9] = {(int32_t)IOSurfaceGetID(layer), NGL_GL_TEXTURE_RECTANGLE, GL_RGBA, 256, 256, GL_BGRA_EXT,
	    GL_UNSIGNED_BYTE, 0, 0};
	r = gliSetInteger(rctx, 0x38e, p);
	GLuint texp = program_of(tex_vs, tex_fs);
	quad(texp, true);
	S("glFlush", v_)(rctx);
	uint32_t s1 = mem_px(target, 128, 128);
	fill(layer, YELLOW);            /* the app draws its next frame into the same surface */
	quad(texp, true);
	S("glFlush", v_)(rctx);
	uint32_t s2 = mem_px(target, 128, 128);
	check("sample", r == 0 && texp != 0 && s1 == BLUE && s2 == YELLOW, "a rectangle sampler over IOSurface %u: %08x "
	    "(blue %08x); after the app wrote it again, the next draw shows %08x (yellow %08x)", IOSurfaceGetID(layer), s1,
	    BLUE, s2, YELLOW);

	/* a draw with no program (CA's lock-screen wallpaper): our plain program, CA's arrays and matrix */
	static const GLfloat identity[16] = {1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1};
	static const GLfloat pos[] = {-1, -1, 0, 1, 1, -1, 0, 1, -1, 1, 0, 1, 1, 1, 0, 1};
	static const GLfloat col[] = {1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1};
	static const GLfloat tc[] = {0, 0, 256, 0, 0, 256, 256, 256};
	fill(layer, BLUE);
	S("glUseProgram", v_u)(rctx, texp);
	S("glUniformMatrix4fv", v_iibf)(rctx, -1, 1, GL_FALSE, identity);   /* CA gives its programs the matrix */
	S("glUseProgram", v_u)(rctx, 0);
	S("glBindTexture", v_eu)(rctx, GL_TEXTURE_2D, src);
	r = gliSetInteger(rctx, 0x38e, (int32_t[9]){(int32_t)IOSurfaceGetID(layer), GL_TEXTURE_2D, GL_RGBA, 256, 256,
	    GL_BGRA_EXT, GL_UNSIGNED_BYTE, 0, 0});
	S("glClearColor", v_ffff)(rctx, 0, 0, 0, 1);
	S("glClear", v_b)(rctx, GL_COLOR_BUFFER_BIT);
	S("glEnableVertexAttribArray", v_u)(rctx, 0);
	S("glVertexAttribPointer", v_uiep)(rctx, 0, 4, GL_FLOAT, GL_FALSE, 0, pos);
	S("glVertexAttribPointer", v_uiep)(rctx, 1, 4, GL_FLOAT, GL_FALSE, 0, col);
	S("glVertexAttribPointer", v_uiep)(rctx, 2, 2, GL_FLOAT, GL_FALSE, 0, tc);
	S("glDrawArrays", v_eii)(rctx, GL_TRIANGLE_STRIP, 0, 4);
	S("glDisableVertexAttribArray", v_u)(rctx, 0);
	S("glFlush", v_)(rctx);
	uint32_t pl = mem_px(target, 100, 100);
	check("plain", r == 0 && pl == BLUE, "a draw with no program and a texture is drawn: %08x (blue %08x)", pl, BLUE);

	/* a CAEAGLLayer: renderbufferStorage:fromDrawable: (gliBindViewES twice), two frames presented */
	struct fake_nw nw = {.version = 5, .attach = nw_attach, .detach = nw_detach, .begin_iosurface = nw_begin,
	    .swap = nw_swap, .begin_properties = nw_props};
	for (int i = 0; i < 3; i++) {
		nw.drawables[i] = host_iosurface_create(256, 256, 'BGRA');
	}
	GLuint rb = 0, vfb = 0;
	S("glGenRenderbuffers", v_iu)(rctx, 1, &rb);
	S("glBindRenderbuffer", v_eu)(rctx, GL_RENDERBUFFER, rb);
	gliBindViewES(rctx, NULL, 0, 0);
	int bound = gliBindViewES(rctx, &nw, 0, 0x8058);
	S("glGenFramebuffers", v_iu)(rctx, 1, &vfb);
	S("glBindFramebuffer", v_eu)(rctx, GL_FRAMEBUFFER, vfb);
	S("glFramebufferRenderbuffer", v_eeeu)(rctx, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_RENDERBUFFER, rb);
	GLenum vfs = S("glCheckFramebufferStatus", e_e)(rctx, GL_FRAMEBUFFER);
	S("glClearColor", v_ffff)(rctx, 1, 0, 0, 1);
	S("glClear", v_b)(rctx, GL_COLOR_BUFFER_BIT);
	int p1 = gliPresentViewES(rctx, 0, 0.0);
	S("glClearColor", v_ffff)(rctx, 0, 0, 1, 1);
	S("glClear", v_b)(rctx, GL_COLOR_BUFFER_BIT);
	int p2 = gliPresentViewES(rctx, 0, 0.0);
	uint32_t d1 = mem_px(nw.drawables[0], 10, 10), d2 = mem_px(nw.drawables[1], 10, 10);
	check("view", bound == 1 && vfs == GL_FRAMEBUFFER_COMPLETE && p1 == 1 && p2 == 1 && nw.swaps == 2 &&
	    nw.swapped[0] == nw.drawables[0] && nw.swapped[1] == nw.drawables[1] && d1 == RED && d2 == BLUE,
	    "bind %d, framebuffer %#x, presents %d %d, %d swaps; drawable 1 %08x (red), drawable 2 %08x (blue)", bound, vfs,
	    p1, p2, nw.swaps, d1, d2);
	gliBindViewES(rctx, NULL, 0, 0);
	check("view", nw.attached == 0, "the drawable is detached (%d attach references left)", nw.attached);

	/* the self tests the gate and the probe run */
	IOSurfaceRef st = host_iosurface_create(256, 256, 'BGRA');
	check("selftest", ngl_selftest() && ngl_surface_selftest(st), "the triangle and the IOSurface self tests");
	char report[400];
	check("probe", oneios_gl_probe(report, sizeof(report)) == 0, "%s", report);

	/* the end of the context: its copied surfaces let go */
	S("glDeleteRenderbuffers", v_icu)(rctx, 1, &rb);
	S("glDeleteTextures", v_icu)(rctx, 1, &tex);
	check("destroy", gliDestroyContext(rctx) == 0 && ngl_cur == NULL, "gliDestroyContext, nothing current");
	int locks = host_iosurface_locks(target) + host_iosurface_locks(inv) + host_iosurface_locks(layer) +
	    host_iosurface_locks(nw.drawables[0]) + host_iosurface_locks(nw.drawables[1]);
	check("destroy", locks == 0, "every IOSurfaceLock was unlocked once the context was gone (%d left)", locks);
	struct ngl_surf_stats ss;
	ngl_surf_stats(&ss);
	printf("oneios-gl-test: copies: %u surfaces, %u uploads (%llu bytes), %u write backs (%llu bytes); %u frames, %u "
	    "presents\n", ss.surfaces, ss.uploads, (unsigned long long)ss.upload_bytes, ss.write_backs,
	    (unsigned long long)ss.write_back_bytes, ngl_frames(), ngl_presents());
	if (!ngl_in_place()) {
		check("copies", ss.uploads >= 3 && ss.write_backs >= 6, "the copied way did its uploads (%u) and write backs (%u)",
		    ss.uploads, ss.write_backs);
	}
	CFRelease(target);
	CFRelease(inv);
	CFRelease(layer);
	CFRelease(st);
	for (int i = 0; i < 3; i++) {
		CFRelease(nw.drawables[i]);
	}
out:
	printf("oneios-gl-test: %d passed, %d failed\n", passed, failed);
	return failed ? 1 : 0;
}
