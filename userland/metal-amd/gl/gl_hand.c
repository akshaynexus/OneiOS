/*
 * Copyright (c) 2026 iOSPlus10 contributors, OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * OneiOSRadeonGL: the GLI context's hand-written slots. CoreAnimation calls every GL function through the table
 * -[EAGLContext getMacroContextPrivate] gives ({rctx, disp[1036]}), as disp[slot](rctx, arguments)
 * (gli-slots.txt; gli_dispatch.c is generated from it). Most slots go straight to Mesa; these are written here
 * because Apple's GL accepts what Mesa's GLES does not:
 *
 *   textures    Apple's ES keeps a 2D and a rectangle binding per unit (Mesa's ES only 2D): the shadows of
 *               struct ngl_state, rectangle textures are 2D ones and their coordinates are scaled (shaders.c)
 *   formats     internal format GL_RGBA with format GL_BGRA (glTexImage2D)
 *   shaders     rewritten before they compile (shaders.c); failures logged with the source
 *   draws       the units bound as CA asked, rect scales, framebuffer fetch emulated, a barrier when a draw reads
 *               the texture it draws into; a draw with no program and a texture gets a plain program of ours
 *               (Jesvi-iOS's plain_draw_begin: CA's lock-screen wallpaper and clock are drawn so); in copy mode
 *               the framebuffer's surface is marked to be written back (surface.c)
 *   flushes     glFlush, glFinish and the APPLE fence/object waits write the copied surfaces back first (view.c)
 *
 * From NegroniGL's eagl.c (iOSPlus10, MIT), whose own sources were ios-sailfish's tools/sweetgl/eagl.c and HackiOS's
 * userland/src/eagl.c (MIT). The Objective-C EAGLContext shim of that file is not taken: the bundle is reached by
 * IOGLESBundleName alone (plugin/), so Apple's EAGLContext runs and calls our GLI entries. Added here: UseProgram,
 * UniformMatrix4fv, Clear, Finish, DeleteRenderbuffers and the plain draw (after Jesvi-iOS userland/src/eagl.c,
 * a13e406, MIT, Copyright (c) 2025-2026 the HakiOS authors: its plain_vertex/plain_fragment and the attribute
 * layout position 0, colour 1, texture coordinates 2 in the texture's pixels; the rest of its CA fixes are not
 * ported: docs/a56-gpu.md 4a).
 */
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "oneios_gl.h"

__thread struct ngl_gli *ngl_cur;
int ngl_check_errors;

void
ngl_bind(struct ngl_gli *g)
{
	struct ngl_gli *old = ngl_cur;
	ngl_make_current(g ? g->ctx : NULL);
	ngl_cur = g;
	/* a context destroyed on another thread while current here is freed only now, after Mesa switched away from
	 * it (plugin/gli_context.c gliDestroyContext) */
	if (old != g) {
		ngl_gli_bound(old, g);
	}
}

void
ngl_unsupported(int slot)
{
	static unsigned char seen[NGL_SLOTS];
	if (slot >= 0 && slot < NGL_SLOTS && !seen[slot]) {
		seen[slot] = 1;
		ngl_logc('W', "GLI", "unsupported GLI slot %d (%s)", slot, ngl_slot_names[slot]);
	}
}

void
ngl_post(int slot)
{
	static unsigned char logged[NGL_SLOTS];
	GLenum e = glGetError();
	if (e != GL_NO_ERROR && slot >= 0 && slot < NGL_SLOTS && logged[slot] < 3) {
		logged[slot]++;
		ngl_logc('W', "GLI", "GL error %#x after slot %d %s", e, slot, ngl_slot_names[slot]);
	}
}

/* ---- texture units: Apple's GL keeps a 2D and a rect binding per unit, Mesa's ES only 2D ---- */

int
ngl_active_unit(void)
{
	GLint active = GL_TEXTURE0;
	glGetIntegerv(GL_ACTIVE_TEXTURE, &active);
	return (active - GL_TEXTURE0) & (NGL_UNITS - 1);
}

GLenum
ngl_tgt(GLenum target)
{
	struct ngl_state *st = ngl_cur ? ngl_cur->st : NULL;
	if (st != NULL) {
		int u = ngl_active_unit();
		GLuint want = target == GL_TEXTURE_2D ? st->unit_2d[u] : st->unit_rect[u];
		if (st->unit_held[u] != want) {
			glBindTexture(GL_TEXTURE_2D, want);
			st->unit_held[u] = want;
		}
	}
	return GL_TEXTURE_2D;
}

/* level-0 size of the texture bound to the active unit (rect scale without GL queries) */
void
ngl_tex_size(GLenum target, GLint level, GLsizei w, GLsizei h)
{
	struct ngl_state *st = ngl_cur ? ngl_cur->st : NULL;
	if (st == NULL || level != 0 || (target != GL_TEXTURE_2D && target != NGL_GL_TEXTURE_RECTANGLE)) {
		return;
	}
	GLint tex = 0;
	glGetIntegerv(GL_TEXTURE_BINDING_2D, &tex);
	if (tex > 0 && tex < NGL_NAMES) {
		st->tex_w[tex] = (uint16_t)w;
		st->tex_h[tex] = (uint16_t)h;
	}
}

/* ---- hand-written GLI slots (gen-gli.sh HAND) ---- */

static struct ngl_state *
st_of(void *rctx)
{
	return ((struct ngl_gli *)rctx)->st;
}

/* Apple's slot 591 is DeleteObjectARB: a shader or a program */
void
ngl_h_DeleteObject(void *rctx, GLuint obj)
{
	NGL_CUR(rctx);
	if (glIsProgram(obj)) {
		glDeleteProgram(obj);
	} else {
		ngl_shader_deleted(st_of(rctx), obj);
		glDeleteShader(obj);
	}
	NGL_POST(591);
}

void
ngl_h_ShaderSource(void *rctx, GLuint shader, GLsizei count, const GLchar *const *string, const GLint *length)
{
	NGL_CUR(rctx);
	size_t n = 0;
	for (GLsizei i = 0; i < count; i++) {
		n += length && length[i] >= 0 ? (size_t)length[i] : strlen(string[i]);
	}
	char *src = malloc(n + 1), *p = src;
	if (src == NULL) {
		glShaderSource(shader, count, string, length);
		return;
	}
	for (GLsizei i = 0; i < count; i++) {
		size_t l = length && length[i] >= 0 ? (size_t)length[i] : strlen(string[i]);
		memcpy(p, string[i], l);
		p += l;
	}
	*p = 0;
	src = ngl_shader_rewrite(st_of(rctx), shader, src);
	const GLchar *one = src;
	glShaderSource(shader, 1, &one, NULL);
	free(src);
	NGL_POST(595);
}

/* failures logged with the info log and the source: CA does not ask */
void
ngl_h_CompileShader(void *rctx, GLuint shader)
{
	NGL_CUR(rctx);
	glCompileShader(shader);
	GLint ok = 0;
	glGetShaderiv(shader, GL_COMPILE_STATUS, &ok);
	if (!ok) {
		/* self-check of the full-precision rewrite (shaders.c): compile again without the two defines */
		GLint len = 0;
		glGetShaderiv(shader, GL_SHADER_SOURCE_LENGTH, &len);
		char *src = len > 0 ? malloc((size_t)len + 1) : NULL;
		char *at = NULL;
		if (src != NULL) {
			glGetShaderSource(shader, len + 1, NULL, src);
			at = strstr(src, ngl_precision_prelude);
		}
		if (at != NULL) {
			size_t pl = strlen(ngl_precision_prelude);
			memmove(at, at + pl, strlen(at + pl) + 1);
			const GLchar *one = src;
			glShaderSource(shader, 1, &one, NULL);
			glCompileShader(shader);
			glGetShaderiv(shader, GL_COMPILE_STATUS, &ok);
			static unsigned logged;
			if (logged++ < 8) {
				ngl_logc('W', "SHDR", "shader %u: full precision refused by the compiler, compiled as written: %s",
				    shader, ok ? "ok" : "still fails");
			}
		}
		free(src);
	}
	if (!ok) {
		static char log[1024], src[4096];
		glGetShaderInfoLog(shader, sizeof(log), NULL, log);
		glGetShaderSource(shader, sizeof(src), NULL, src);
		ngl_logc('E', "SHDR", "shader %u compile failed: %.400s", shader, log);
		for (size_t i = 0, n = strlen(src); i < n; i += 400) {
			ngl_logc('W', "SHDR", "  src %.400s", src + i);
		}
	}
}

void
ngl_h_LinkProgram(void *rctx, GLuint program)
{
	NGL_CUR(rctx);
	glLinkProgram(program);
	GLint ok = 0;
	glGetProgramiv(program, GL_LINK_STATUS, &ok);
	if (ok) {
		ngl_program_linked(st_of(rctx), program);
	} else {
		static char log[1024];
		glGetProgramInfoLog(program, sizeof(log), NULL, log);
		ngl_logc('E', "SHDR", "program %u link failed: %.400s", program, log);
	}
}

void
ngl_h_UseProgram(void *rctx, GLuint program)
{
	NGL_CUR(rctx);
	st_of(rctx)->program = program;
	glUseProgram(program);
	NGL_POST(600);
}

/* CA hands every program of its own the matrix from one place of its (Jesvi-iOS): kept for the plain draw */
void
ngl_h_UniformMatrix4fv(void *rctx, GLint location, GLsizei count, GLboolean transpose, const GLfloat *value)
{
	NGL_CUR(rctx);
	st_of(rctx)->matrix = value;
	glUniformMatrix4fv(location, count, transpose, value);
	NGL_POST(620);
}

const GLubyte *
ngl_h_GetString(void *rctx, GLenum name)
{
	NGL_CUR(rctx);
	const GLubyte *s = glGetString(name);
	if (name == GL_EXTENSIONS) {
		return (const GLubyte *)ngl_extensions((const char *)s);
	}
	return s;
}

/* Apple's ES takes internalformat GL_RGBA with format GL_BGRA; Mesa wants GL_BGRA for both */
void
ngl_h_TexImage2D(void *rctx, GLenum target, GLint level, GLint internalformat, GLsizei width, GLsizei height,
    GLint border, GLenum format, GLenum type, const GLvoid *pixels)
{
	NGL_CUR(rctx);
	struct ngl_state *st = st_of(rctx);
	target = NGL_TGT(target);
	ngl_surf_forget_texture(st, st->unit_held[ngl_active_unit()]);
	ngl_set_swizzle(st, target, false);
	if (format == GL_BGRA_EXT && (internalformat == GL_RGBA || internalformat == GL_RGBA8)) {
		internalformat = GL_BGRA_EXT;
	}
	glTexImage2D(target, level, internalformat, width, height, border, format, type, pixels);
	NGL_POST(301);
	ngl_tex_size(target, level, width, height);
}

void
ngl_h_BindTexture(void *rctx, GLenum target, GLuint texture)
{
	NGL_CUR(rctx);
	struct ngl_state *st = st_of(rctx);
	if (target == GL_TEXTURE_2D || target == NGL_GL_TEXTURE_RECTANGLE) {
		int u = ngl_active_unit();
		*(target == GL_TEXTURE_2D ? &st->unit_2d[u] : &st->unit_rect[u]) = texture;
		st->unit_held[u] = texture;
		glBindTexture(GL_TEXTURE_2D, texture);
		if (target == NGL_GL_TEXTURE_RECTANGLE && texture != 0 && texture < NGL_NAMES && !st->tex_rect[texture]) {
			/* a rectangle texture's defaults are not a 2D one's: no mipmaps (a 2D texture's default minifying filter
			 * wants them, and one level of it alone is incomplete: it samples black) and clamped to its edges */
			st->tex_rect[texture] = 1;
			glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
			glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
			glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
		}
	} else {
		glBindTexture(target, texture);
	}
	NGL_POST(5);
}

/* GL unbinds deleted textures from every unit: so do the shadows */
void
ngl_h_DeleteTextures(void *rctx, GLsizei n, const GLuint *textures)
{
	NGL_CUR(rctx);
	struct ngl_state *st = st_of(rctx);
	for (GLsizei i = 0; i < n; i++) {
		GLuint t = textures[i];
		ngl_surf_forget_texture(st, t);
		if (t < NGL_NAMES) {
			st->tex_argb[t] = 0;
			st->tex_rect[t] = 0;
			st->tex_w[t] = st->tex_h[t] = 0;
		}
		for (int u = 0; u < NGL_UNITS; u++) {
			if (st->unit_2d[u] == t) {
				st->unit_2d[u] = 0;
			}
			if (st->unit_rect[u] == t) {
				st->unit_rect[u] = 0;
			}
			if (st->unit_held[u] == t) {
				st->unit_held[u] = 0;
			}
		}
	}
	glDeleteTextures(n, textures);
	NGL_POST(59);
}

void
ngl_h_DeleteRenderbuffers(void *rctx, GLsizei n, const GLuint *renderbuffers)
{
	NGL_CUR(rctx);
	for (GLsizei i = 0; i < n; i++) {
		ngl_surf_forget_renderbuffer(st_of(rctx), renderbuffers[i]);
	}
	glDeleteRenderbuffers(n, renderbuffers);
	NGL_POST(667);
}

void
ngl_h_FramebufferTexture2D(void *rctx, GLenum target, GLenum attachment, GLenum textarget, GLuint texture, GLint level)
{
	NGL_CUR(rctx);
	glFramebufferTexture2D(target, attachment, textarget == NGL_GL_TEXTURE_RECTANGLE ? GL_TEXTURE_2D : textarget,
	    texture, level);
	NGL_POST(677);
}

/*
 * The flushes (view.c ngl_flush): the copied surfaces drawn into go back into their IOSurfaces, then glFlush.
 * CoreAnimation ends a frame with glFlush and then swaps; on the A56 the display reads the surface after that.
 */
void
ngl_h_Flush(void *rctx)
{
	NGL_CUR(rctx);
	ngl_flush();
}

void
ngl_h_Finish(void *rctx)
{
	NGL_CUR(rctx);
	ngl_flush();
	glFinish();
}

void
ngl_h_FinishObjectAPPLE(void *rctx, GLenum object, GLint name)
{
	(void)object;
	(void)name;
	ngl_h_Finish(rctx);
}

GLboolean
ngl_h_TestObjectAPPLE(void *rctx, GLenum object, GLuint name)
{
	(void)object;
	(void)name;
	ngl_h_Finish(rctx);
	return GL_TRUE;
}

void
ngl_h_FinishFenceAPPLE(void *rctx, GLuint fence)
{
	(void)fence;
	ngl_h_Finish(rctx);
}

/* ---- before each draw ---- */

/* name of the colour texture of the bound draw framebuffer, 0 if none */
static GLint
fb_texture(void)
{
	GLint fb = 0, type = 0, att = 0;
	glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &fb);
	if (fb == 0) {
		return 0;
	}
	glGetFramebufferAttachmentParameteriv(GL_DRAW_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
	    GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE, &type);
	if (type != GL_TEXTURE) {
		return 0;
	}
	glGetFramebufferAttachmentParameteriv(GL_DRAW_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
	    GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME, &att);
	return att;
}

static void
tex_size_of(struct ngl_state *st, GLuint tex, int unit, GLint *w, GLint *h)
{
	*w = *h = 0;
	if (tex < NGL_NAMES) {
		*w = st->tex_w[tex];
		*h = st->tex_h[tex];
	}
	if (*w <= 0 || *h <= 0) {
		GLint active = GL_TEXTURE0;
		glGetIntegerv(GL_ACTIVE_TEXTURE, &active);
		glActiveTexture(GL_TEXTURE0 + unit);
		glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, w);
		glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, h);
		glActiveTexture(active);
	}
}

/* which units the program's samplers read (read once per link; CA sets them at link time) */
static uint16_t
program_units(struct ngl_state *st, GLint prog)
{
	if (prog <= 0 || prog >= NGL_NAMES) {
		return 0;
	}
	if (!(st->prog_units[prog] & 0x8000)) {
		uint16_t m = 0x8000;
		GLint n = 0;
		glGetProgramiv(prog, GL_ACTIVE_UNIFORMS, &n);
		for (GLint i = 0; i < n; i++) {
			char name[64];
			GLint size = 0, unit = 0;
			GLenum t = 0;
			glGetActiveUniform(prog, i, sizeof(name), NULL, &size, &t, name);
			if (t == GL_SAMPLER_2D) {
				glGetUniformiv(prog, glGetUniformLocation(prog, name), &unit);
				m |= unit >= 0 && unit < NGL_FBFETCH_UNIT ? (uint16_t)(1u << unit) : 0;
			}
		}
		st->prog_units[prog] = m;
	}
	return st->prog_units[prog];
}

static void
draw_prep(struct ngl_state *st)
{
	GLint prog = (GLint)st->program;
	struct ngl_rect_prog *rp = prog > 0 && prog < NGL_NAMES && st->rect_prog[prog].program ? &st->rect_prog[prog] : NULL;
	GLint rect_unit = -1;
	if (rp != NULL) {
		rect_unit = 0;
		if (rp->sampler >= 0) {
			glGetUniformiv(prog, rp->sampler, &rect_unit);
		}
	}
	int active = -1;
	for (int u = 0; u < NGL_FBFETCH_UNIT; u++) {
		GLuint want = u == rect_unit ? st->unit_rect[u] : st->unit_2d[u];
		if (st->unit_held[u] != want) {
			if (active < 0) {
				active = ngl_active_unit();
			}
			glActiveTexture(GL_TEXTURE0 + u);
			glBindTexture(GL_TEXTURE_2D, want);
			st->unit_held[u] = want;
		}
		if (want != 0) {
			ngl_surf_refresh(st, u, &active);
		}
	}
	if (active >= 0) {
		glActiveTexture(GL_TEXTURE0 + active);
	}
	if (rp != NULL && rp->scale >= 0 && rect_unit >= 0 && rect_unit < NGL_UNITS) {
		GLint w, h;
		tex_size_of(st, st->unit_rect[rect_unit], rect_unit, &w, &h);
		glUniform2f(rp->scale, w > 0 ? 1.0f / (GLfloat)w : 1.0f, h > 0 ? 1.0f / (GLfloat)h : 1.0f);
	}
	ngl_surf_note_write(st);
	GLint fbt = prog > 0 ? fb_texture() : 0;
	if (fbt <= 0) {
		return;
	}
	bool barrier = false;
	if (prog < NGL_NAMES && st->fbf_sampler[prog] > 0) {
		/* emulated framebuffer fetch: the colour texture on its own unit */
		GLint w, h, act = GL_TEXTURE0;
		glGetIntegerv(GL_ACTIVE_TEXTURE, &act);
		glActiveTexture(GL_TEXTURE0 + NGL_FBFETCH_UNIT);
		glBindTexture(GL_TEXTURE_2D, (GLuint)fbt);
		st->unit_held[NGL_FBFETCH_UNIT] = (GLuint)fbt;
		glActiveTexture(act);
		tex_size_of(st, (GLuint)fbt, NGL_FBFETCH_UNIT, &w, &h);
		glUniform1i(st->fbf_sampler[prog] - 1, NGL_FBFETCH_UNIT);
		if (st->fbf_scale[prog] > 0) {
			glUniform2f(st->fbf_scale[prog] - 1, w > 0 ? 1.0f / (GLfloat)w : 1.0f, h > 0 ? 1.0f / (GLfloat)h : 1.0f);
		}
		barrier = true;
	}
	/* CA reads the destination by sampling the texture it renders into: earlier draws must land first */
	uint16_t units = program_units(st, prog);
	for (int u = 0; u < NGL_FBFETCH_UNIT && !barrier; u++) {
		barrier = (units & (1u << u)) && st->unit_held[u] == (GLuint)fbt;
	}
	if (barrier) {
		glTextureBarrierNV();
	}
}

/*
 * A draw CoreAnimation makes with no program (Jesvi-iOS userland/src/eagl.c plain_draw_begin): on iOS's library it
 * draws, in OpenGL ES it draws nothing, and the lock screen's wallpaper and clock go this way. With a texture on
 * the first unit it is drawn with a program of ours: CA's arrays (position 0, colour 1, texture coordinates 2 in
 * the texture's pixels), the matrix CA gives its own programs, colour times texture. Draws with no program and no
 * texture, and those that read the texture they draw into, stay undrawn (Jesvi: drawn, they painted the screen
 * white, or wrote unfinished pixels over SpringBoard's launch images). false: leave the draw as it is.
 */
static const char plain_vertex[] =
    "#version 300 es\n"
    "uniform highp mat4 vertex_matrix;\n"
    "in highp vec4 vertex_position;\n"
    "in lowp vec4 vertex_color;\n"
    "in highp vec2 vertex_texcoord0;\n"
    "out lowp vec4 color;\n"
    "out highp vec2 texcoord;\n"
    "void main() {\n"
    "  gl_Position = vertex_matrix * vertex_position;\n"
    "  color = vertex_color;\n"
    "  texcoord = vertex_texcoord0;\n"
    "}\n";
static const char plain_fragment[] =
    "#version 300 es\n"
    "uniform lowp sampler2D texture0;\n"
    "in lowp vec4 color;\n"
    "in highp vec2 texcoord;\n"
    "out lowp vec4 result;\n"
    "void main() {\n"
    "  result = color * texture(texture0, texcoord / vec2(textureSize(texture0, 0)));\n"
    "}\n";

static GLuint
plain_program(struct ngl_state *st)
{
	if (st->plain != 0) {
		return st->plain;
	}
	GLuint program = glCreateProgram();
	const GLchar *text[2] = {plain_vertex, plain_fragment};
	GLenum kinds[2] = {GL_VERTEX_SHADER, GL_FRAGMENT_SHADER};
	for (int i = 0; i < 2; i++) {
		GLuint s = glCreateShader(kinds[i]);
		glShaderSource(s, 1, &text[i], NULL);
		glCompileShader(s);
		glAttachShader(program, s);
		glDeleteShader(s);
	}
	glBindAttribLocation(program, 0, "vertex_position");
	glBindAttribLocation(program, 1, "vertex_color");
	glBindAttribLocation(program, 2, "vertex_texcoord0");
	glLinkProgram(program);
	GLint good = 0;
	glGetProgramiv(program, GL_LINK_STATUS, &good);
	ngl_logc(good ? 'I' : 'E', "DRAW", "a program of ours for the draws CoreAnimation makes with none: %s",
	    good ? "made" : "NOT MADE");
	st->plain = program;
	st->plain_matrix_loc = (GLuint)glGetUniformLocation(program, "vertex_matrix");
	st->plain_textured_loc = (GLuint)glGetUniformLocation(program, "texture0");
	return program;
}

static bool
plain_draw_begin(struct ngl_state *st, GLint enabled[3])
{
	static const GLfloat identity[16] = {1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1};
	if (st->program != 0 || st->unit_2d[0] == 0 || (GLint)st->unit_2d[0] == fb_texture()) {
		return false;
	}
	GLuint program = plain_program(st);
	GLint good = 0;
	glGetProgramiv(program, GL_LINK_STATUS, &good);
	if (!good) {
		return false;
	}
	static unsigned said;
	if (said++ < 4) {
		ngl_logc('I', "DRAW", "a draw with no program, texture %u on the first unit: drawn with ours", st->unit_2d[0]);
	}
	glUseProgram(program);
	glUniformMatrix4fv((GLint)st->plain_matrix_loc, 1, GL_FALSE, st->matrix != NULL ? st->matrix : identity);
	glUniform1i((GLint)st->plain_textured_loc, 0);
	for (GLuint i = 0; i < 3; i++) {
		glGetVertexAttribiv(i, GL_VERTEX_ATTRIB_ARRAY_ENABLED, &enabled[i]);
		if (!enabled[i]) {
			glEnableVertexAttribArray(i);
		}
	}
	return true;
}

static void
plain_draw_end(struct ngl_state *st, const GLint enabled[3])
{
	for (GLuint i = 0; i < 3; i++) {
		if (!enabled[i]) {
			glDisableVertexAttribArray(i);
		}
	}
	glUseProgram(st->program);
}

void
ngl_h_Clear(void *rctx, GLbitfield mask)
{
	NGL_CUR(rctx);
	if (mask & GL_COLOR_BUFFER_BIT) {
		ngl_surf_note_write(st_of(rctx));
	}
	glClear(mask);
	NGL_POST(10);
}

void
ngl_h_DrawArrays(void *rctx, GLenum mode, GLint first, GLsizei count)
{
	NGL_CUR(rctx);
	struct ngl_state *st = st_of(rctx);
	GLint enabled[3];
	draw_prep(st);
	bool plain = st->program == 0 && plain_draw_begin(st, enabled);
	glDrawArrays(mode, first, count);
	if (plain) {
		plain_draw_end(st, enabled);
	}
	NGL_POST(65);
}

void
ngl_h_DrawElements(void *rctx, GLenum mode, GLsizei count, GLenum type, const GLvoid *indices)
{
	NGL_CUR(rctx);
	struct ngl_state *st = st_of(rctx);
	GLint enabled[3];
	draw_prep(st);
	bool plain = st->program == 0 && plain_draw_begin(st, enabled);
	glDrawElements(mode, count, type, indices);
	if (plain) {
		plain_draw_end(st, enabled);
	}
	NGL_POST(67);
}

/*
 * GL_APPLE_fence (slots 463-467), which Mesa's GLES has not. CoreAnimation does not call them (gli-slots.txt marks
 * them unseen: it fences with glFenceSync), but an application that polls glTestFenceAPPLE on a slot that answers 0
 * would spin for ever: so a fence is a flush, its test a finish that answers done.
 */
static GLuint fence_names;

void
ngl_h_GenFencesAPPLE(void *rctx, GLsizei n, GLuint *fences)
{
	NGL_CUR(rctx);
	for (GLsizei i = 0; i < n && fences != NULL; i++) {
		fences[i] = __atomic_add_fetch(&fence_names, 1, __ATOMIC_RELAXED);
	}
}

void
ngl_h_DeleteFencesAPPLE(void *rctx, GLsizei n, const GLuint *fences)
{
	(void)rctx;
	(void)n;
	(void)fences;
}

void
ngl_h_SetFenceAPPLE(void *rctx, GLuint fence)
{
	(void)fence;
	NGL_CUR(rctx);
	ngl_flush();
}

GLboolean
ngl_h_IsFenceAPPLE(void *rctx, GLuint fence)
{
	(void)rctx;
	return fence != 0 && fence <= __atomic_load_n(&fence_names, __ATOMIC_RELAXED);
}

GLboolean
ngl_h_TestFenceAPPLE(void *rctx, GLuint fence)
{
	(void)fence;
	ngl_h_Finish(rctx);
	return GL_TRUE;
}
