/*
 * Copyright (c) 2026 iOSPlus10 contributors, OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * OneiOSRadeonGL: IOSurfaces as GL texture and renderbuffer storage.
 *
 * CoreAnimation renders into IOSurfaces (EAGL's -attachImage:toCoreSurface:invertedRender: and its CAEAGLLayer
 * drawables: the surface becomes a framebuffer's colour buffer) and samples them (-texImageIOSurface:..., the
 * applications' layers). Both reach us as gliSetInteger(0x38e, {surface ID, target, ...}) or through view.c. Like
 * EGL images on Linux, the surface becomes a pipe_resource that the frontend binds with
 * glEGLImageTargetTexture2DOES / glEGLImageTargetRenderbufferStorageOES (get_egl_image below). Two ways:
 *
 *   in place (softpipe)  the surface is a softpipe display target that sw_winsys.c maps with IOSurfaceLock; the
 *                        CPU draws into it and samples it where it is. Nothing is copied. (NegroniGL's way, which
 *                        on its Adreno imported the surface's pages into the GPU through its kernel.)
 *   copied (a GPU)       the A56's Darwin vdrm imports no IOSurface pages (docs/a56-gpu.md 3 and 4: every BO is
 *                        Mesa's own, guest-allocated), so each IOSurface gets a resource of ours (struct ngl_surf,
 *                        one per surface per process, shared by every context): filled from the surface when the
 *                        surface's seed moved since (at the bind and before each draw that samples it), and
 *                        written back into the surface at the flush that follows a draw or clear into it (view.c
 *                        ngl_flush: glFlush, the present, glFinish). This is the Metal layer's IOSurface way too
 *                        ("as copies", section 4). Its cost is the bytes: a 720x1560 BGRA frame is 4.5 MB each way.
 *   ONEIOS_GL_SURFACE=copy takes the copied way on softpipe as well (the host test runs both).
 *
 * Multi-plane surfaces (video 420v/420f): a texture of one plane is filled by a CPU copy (plane_copy, NegroniGL's,
 * idea from HackiOS surface_to_texture, MIT, Copyright (c) 2025-2026 the HakiOS authors); render targets with
 * planes are refused.
 */
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "pipe/p_screen.h"
#include "pipe/p_context.h"
#include "pipe/p_state.h"
#include "frontend/api.h"
#include "frontend/winsys_handle.h"
#include "util/u_inlines.h"
#include "util/box.h"
#include "util/format/u_format.h"
#include "drm-uapi/drm_fourcc.h"

#include "oneios_gl.h"

struct ngl_image {
	struct pipe_resource *res;
	enum pipe_format format;
	unsigned internalformat;
};

static bool
get_egl_image(struct pipe_frontend_screen *fs, void *image, struct st_egl_image *out)
{
	(void)fs;
	struct ngl_image *img = image;
	pipe_resource_reference(&out->texture, img->res);
	out->format = img->format;
	out->level = 0;
	out->layer = 0;
	out->internalformat = img->internalformat;
	return true;
}

static bool
validate_egl_image(struct pipe_frontend_screen *fs, void *image)
{
	(void)fs;
	return image != NULL;
}

void
ngl_image_hooks(struct pipe_frontend_screen *fs)
{
	fs->get_egl_image = get_egl_image;
	fs->validate_egl_image = validate_egl_image;
}

/* GL format/type CA passed -> Gallium format and the GL internal format */
static enum pipe_format
gl_format(GLenum format, GLenum type, unsigned *ifmt)
{
	bool half = type == GL_HALF_FLOAT || type == GL_HALF_FLOAT_OES;
	switch (format) {
	case GL_BGRA_EXT:
		*ifmt = GL_RGBA8;
		return PIPE_FORMAT_B8G8R8A8_UNORM;
	case GL_RGBA:
		*ifmt = half ? GL_RGBA16F : GL_RGBA8;
		return half ? PIPE_FORMAT_R16G16B16A16_FLOAT : PIPE_FORMAT_R8G8B8A8_UNORM;
	case GL_LUMINANCE_ALPHA:
		*ifmt = GL_LUMINANCE_ALPHA;
		return PIPE_FORMAT_L8A8_UNORM;
	case GL_LUMINANCE:
		*ifmt = GL_LUMINANCE;
		return PIPE_FORMAT_L8_UNORM;
	case GL_ALPHA:
		*ifmt = GL_ALPHA;
		return PIPE_FORMAT_A8_UNORM;
	case GL_RED:
		*ifmt = half ? GL_R16F : GL_R8;
		return half ? PIPE_FORMAT_R16_FLOAT : PIPE_FORMAT_R8_UNORM;
	case GL_RG:
		*ifmt = half ? GL_RG16F : GL_RG8;
		return half ? PIPE_FORMAT_R16G16_FLOAT : PIPE_FORMAT_R8G8_UNORM;
	default:
		return PIPE_FORMAT_NONE;
	}
}

/* GL upload format/type of a surface by its IOSurface pixel format (0: CA's own, by size) */
static void
upload_format(uint32_t fourcc, size_t bpe, GLenum *format, GLenum *type)
{
	*type = GL_UNSIGNED_BYTE;
	switch (fourcc) {
	case 0:
		*format = bpe == 4 ? GL_BGRA_EXT : bpe == 8 ? GL_RGBA : bpe == 1 ? GL_RED : 0;
		*type = bpe == 8 ? GL_HALF_FLOAT : GL_UNSIGNED_BYTE;
		break;
	case 'BGRA': *format = GL_BGRA_EXT; break;
	case 'RGBA': *format = GL_RGBA; break;
	case 'RGhA': *format = GL_RGBA; *type = GL_HALF_FLOAT; break;
	case 'LA08': *format = GL_LUMINANCE_ALPHA; break;
	case 'A008': *format = GL_ALPHA; break;
	case 'L008': *format = GL_RED; break;
	case 'L016': *format = GL_RG; break;
	default: *format = 0;
	}
}

static enum pipe_format
surface_format(uint32_t fourcc, size_t bpe, unsigned *ifmt)
{
	GLenum f = 0, t = 0;
	upload_format(fourcc, bpe, &f, &t);
	return f ? gl_format(f, t, ifmt) : PIPE_FORMAT_NONE;
}

/* bytes of one texel for an upload format/type (HackiOS bytes_a_pixel, same table) */
static unsigned
texel_bytes(GLenum format, GLenum type)
{
	unsigned parts = format == GL_RGBA || format == GL_BGRA_EXT ? 4 : format == GL_RGB ? 3 :
	    format == GL_RG || format == GL_LUMINANCE_ALPHA ? 2 : 1;
	switch (type) {
	case GL_UNSIGNED_BYTE: return parts;
	case GL_HALF_FLOAT: case GL_HALF_FLOAT_OES: case GL_UNSIGNED_SHORT: return 2 * parts;
	case GL_FLOAT: return 4 * parts;
	default: return 0;
	}
}

/* sized internal format for an ES 3 upload of format/type (0 = not handled) */
static GLenum
upload_internal(GLenum format, GLenum type)
{
	bool half = type == GL_HALF_FLOAT || type == GL_HALF_FLOAT_OES;
	if (type != GL_UNSIGNED_BYTE && !half) return 0;
	switch (format) {
	case GL_LUMINANCE: case GL_LUMINANCE_ALPHA: case GL_ALPHA: return half ? 0 : format;
	case GL_RED: return half ? GL_R16F : GL_R8;
	case GL_RG: return half ? GL_RG16F : GL_RG8;
	case GL_RGBA: return half ? GL_RGBA16F : GL_RGBA;
	case GL_BGRA_EXT: return half ? 0 : GL_BGRA_EXT;
	default: return 0;
	}
}

/* ---- the copied surfaces ---- */

struct ngl_surf {
	struct ngl_surf *next;
	IOSurfaceRef surface;           /* retained */
	uint32_t id, w, h;
	size_t texel;                   /* bytes of one pixel */
	struct pipe_resource *res;
	enum pipe_format format;
	uint32_t seed;                  /* the surface seed our resource holds (after an upload or a write back) */
	bool seeded;                    /* seed is valid (false: never uploaded or written back) */
	bool written;                   /* drawn into since the last write back */
	bool invert;                    /* memory row 0 is GL's top row (CA's invertedRender): rows turned over both ways */
	unsigned refs;                  /* texture and renderbuffer names (struct ngl_state) that show it */
	unsigned used;                  /* the write-back round of its last bind */
};

static pthread_mutex_t surf_lock = PTHREAD_MUTEX_INITIALIZER;
static struct ngl_surf *surfs;
static unsigned nsurfs, rounds, nwritten;
static struct ngl_surf_stats stats;

#define SURF_KEEP_ROUNDS 600    /* an unreferenced surface not bound for this many flushes is let go */
#define SURF_MAX 128

void
ngl_surf_stats(struct ngl_surf_stats *out)
{
	pthread_mutex_lock(&surf_lock);
	*out = stats;
	out->surfaces = nsurfs;
	pthread_mutex_unlock(&surf_lock);
}

static void
surf_free(struct ngl_surf *e)
{
	pipe_resource_reference(&e->res, NULL);
	CFRelease(e->surface);
	free(e);
}

/* surf_lock held */
static void
surf_evict(void)
{
	for (struct ngl_surf **p = &surfs; *p != NULL;) {
		struct ngl_surf *e = *p;
		if (e->refs == 0 && !e->written && (rounds - e->used > SURF_KEEP_ROUNDS || nsurfs > SURF_MAX)) {
			*p = e->next;
			nsurfs--;
			surf_free(e);
			continue;
		}
		p = &e->next;
	}
}

/* surf_lock held; the surface's pixels into the resource (the current context's pipe) */
static bool
surf_upload(struct ngl_surf *e)
{
	struct pipe_context *pipe = ngl_pipe();
	if (pipe == NULL || IOSurfaceLock(e->surface, kIOSurfaceLockReadOnly, NULL) != 0) {
		return false;
	}
	const uint8_t *base = IOSurfaceGetBaseAddress(e->surface);
	size_t row = IOSurfaceGetBytesPerRow(e->surface);
	uint8_t *turned = NULL;
	if (base != NULL && e->invert && (turned = malloc(row * e->h)) != NULL) {
		for (uint32_t y = 0; y < e->h; y++) {
			memcpy(turned + (size_t)y * row, base + (size_t)(e->h - 1 - y) * row, row);
		}
		base = turned;
	}
	if (base != NULL) {
		struct pipe_box box;
		u_box_2d(0, 0, e->w, e->h, &box);
		pipe->texture_subdata(pipe, e->res, 0, 0, &box, base, (unsigned)row, 0);
		stats.uploads++;
		stats.upload_bytes += (uint64_t)row * e->h;
	}
	IOSurfaceUnlock(e->surface, kIOSurfaceLockReadOnly, NULL);
	free(turned);
	e->seed = IOSurfaceGetSeed(e->surface);
	e->seeded = true;
	return base != NULL;
}

/* surf_lock held; the resource's pixels back into the surface (the work was flushed by the caller) */
static bool
surf_write_back(struct ngl_surf *e)
{
	struct pipe_context *pipe = ngl_pipe();
	if (pipe == NULL) {
		return false;
	}
	struct pipe_box box;
	struct pipe_transfer *t = NULL;
	u_box_2d(0, 0, e->w, e->h, &box);
	const uint8_t *src = pipe->texture_map(pipe, e->res, 0, PIPE_MAP_READ, &box, &t);
	if (src == NULL) {
		return false;
	}
	bool ok = false;
	if (IOSurfaceLock(e->surface, 0, NULL) == 0) {
		uint8_t *dst = IOSurfaceGetBaseAddress(e->surface);
		size_t row = IOSurfaceGetBytesPerRow(e->surface), bytes = (size_t)e->w * e->texel;
		if (dst != NULL) {
			for (uint32_t y = 0; y < e->h; y++) {
				memcpy(dst + (size_t)(e->invert ? e->h - 1 - y : y) * row, src + (size_t)y * t->stride, bytes);
			}
			ok = true;
			stats.write_backs++;
			stats.write_back_bytes += (uint64_t)bytes * e->h;
		}
		IOSurfaceUnlock(e->surface, 0, NULL);
	}
	pipe->texture_unmap(pipe, t);
	e->seed = IOSurfaceGetSeed(e->surface);      /* our own write: not a reason to upload it again */
	e->seeded = true;
	return ok;
}

/* the copy of surface s, made when there is none (surf_lock held) */
static struct ngl_surf *
surf_get(IOSurfaceRef s, enum pipe_format pf, uint32_t w, uint32_t h)
{
	uint32_t id = IOSurfaceGetID(s);
	for (struct ngl_surf *e = surfs; e != NULL; e = e->next) {
		if (e->id == id && e->surface == s && e->format == pf && e->w == w && e->h == h) {
			return e;
		}
	}
	struct pipe_screen *screen = ngl_screen();
	if (screen == NULL) {
		return NULL;
	}
	struct pipe_resource templ = {
		.target = PIPE_TEXTURE_2D, .format = pf, .width0 = w, .height0 = h, .depth0 = 1, .array_size = 1,
		.bind = PIPE_BIND_SAMPLER_VIEW | PIPE_BIND_RENDER_TARGET,
	};
	struct ngl_surf *e = calloc(1, sizeof(*e));
	if (e == NULL || (e->res = screen->resource_create(screen, &templ)) == NULL) {
		free(e);
		return NULL;
	}
	e->surface = (IOSurfaceRef)CFRetain(s);
	e->id = id;
	e->w = w;
	e->h = h;
	e->texel = util_format_get_blocksize(pf);
	e->format = pf;
	e->next = surfs;
	surfs = e;
	nsurfs++;
	surf_evict();
	return e;
}

static void
surf_unref(struct ngl_surf *e)
{
	if (e != NULL && e->refs > 0) {
		e->refs--;
	}
}

void
ngl_surf_forget_texture(struct ngl_state *st, GLuint tex)
{
	if (st == NULL || tex >= NGL_NAMES || st->tex_surf[tex] == NULL) {
		return;
	}
	pthread_mutex_lock(&surf_lock);
	surf_unref(st->tex_surf[tex]);
	st->tex_surf[tex] = NULL;
	pthread_mutex_unlock(&surf_lock);
}

void
ngl_surf_forget_renderbuffer(struct ngl_state *st, GLuint rb)
{
	if (st == NULL || rb >= NGL_NAMES || st->rb_surf[rb] == NULL) {
		return;
	}
	pthread_mutex_lock(&surf_lock);
	surf_unref(st->rb_surf[rb]);
	st->rb_surf[rb] = NULL;
	pthread_mutex_unlock(&surf_lock);
}

/* before a draw: the texture held on unit u shows a copied surface whose seed moved -> upload it again */
void
ngl_surf_refresh(struct ngl_state *st, int unit, int *active)
{
	(void)active;
	GLuint tex = st->unit_held[unit];
	struct ngl_surf *e = tex < NGL_NAMES ? st->tex_surf[tex] : NULL;
	if (e == NULL || e->written || (e->seeded && IOSurfaceGetSeed(e->surface) == e->seed)) {
		return;
	}
	pthread_mutex_lock(&surf_lock);
	if (!e->written) {
		surf_upload(e);
	}
	pthread_mutex_unlock(&surf_lock);
}

/* a draw or clear into the bound draw framebuffer: its colour buffer, if a copied surface, is to be written back */
void
ngl_surf_note_write(struct ngl_state *st)
{
	if (st == NULL || __atomic_load_n(&nsurfs, __ATOMIC_RELAXED) == 0) {
		return;
	}
	GLint fb = 0, type = 0, name = 0;
	glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &fb);
	if (fb == 0) {
		return;
	}
	glGetFramebufferAttachmentParameteriv(GL_DRAW_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
	    GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE, &type);
	glGetFramebufferAttachmentParameteriv(GL_DRAW_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
	    GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME, &name);
	struct ngl_surf *e = NULL;
	if (name > 0 && name < NGL_NAMES) {
		e = type == GL_TEXTURE ? st->tex_surf[name] : type == GL_RENDERBUFFER ? st->rb_surf[name] : NULL;
	}
	if (e != NULL && !e->written) {
		pthread_mutex_lock(&surf_lock);
		if (!e->written) {
			e->written = true;
			nwritten++;
		}
		pthread_mutex_unlock(&surf_lock);
	}
	st->writes++;
}

/* at a flush: every copied surface drawn into goes back into its IOSurface (the current context's work first) */
unsigned
ngl_surf_write_back(void)
{
	unsigned n = 0;
	if (__atomic_load_n(&nwritten, __ATOMIC_RELAXED) == 0) {
		return 0;
	}
	if (ngl_cur != NULL) {
		ngl_ctx_flush(ngl_cur->ctx);
	}
	pthread_mutex_lock(&surf_lock);
	rounds++;
	for (struct ngl_surf *e = surfs; e != NULL; e = e->next) {
		if (e->written) {
			if (!surf_write_back(e)) {
				static unsigned logged;
				if (logged++ < 8) {
					ngl_logc('E', "SURF", "IOSurface %u (%ux%u): write back failed", e->id, e->w, e->h);
				}
			}
			e->written = false;
			n++;
		}
	}
	nwritten = 0;
	surf_evict();
	pthread_mutex_unlock(&surf_lock);
	return n;
}

/*
 * A texture from one plane of a multi-plane surface, copied by the CPU. CA's GL renderer samples video as one
 * texture per plane (luma, chroma) and converts in its shader [NegroniGL: a guess]. Each bind copies again.
 */
static bool
plane_copy(IOSurfaceRef s, GLenum target, GLenum format, GLenum type, uint32_t plane, const char *why)
{
	static unsigned logged;
	size_t planes = IOSurfaceGetPlaneCount(s);
	if (target == GL_RENDERBUFFER || (planes && plane >= planes) || (!planes && plane != 0)) return false;
	size_t w = planes ? IOSurfaceGetWidthOfPlane(s, plane) : IOSurfaceGetWidth(s);
	size_t h = planes ? IOSurfaceGetHeightOfPlane(s, plane) : IOSurfaceGetHeight(s);
	size_t row = planes ? IOSurfaceGetBytesPerRowOfPlane(s, plane) : IOSurfaceGetBytesPerRow(s);
	size_t bpe = planes ? IOSurfaceGetBytesPerElementOfPlane(s, plane) : IOSurfaceGetBytesPerElement(s);
	if (format == 0) {              /* CA did not say: by the plane's element size */
		format = bpe == 1 ? GL_LUMINANCE : bpe == 2 ? GL_LUMINANCE_ALPHA : bpe == 4 ? GL_BGRA_EXT : 0;
		type = GL_UNSIGNED_BYTE;
	}
	unsigned texel = texel_bytes(format, type);
	GLenum ifmt = upload_internal(format, type);
	uint32_t fourcc = IOSurfaceGetPixelFormat(s);
	if (!w || !h || !texel || !ifmt || row % texel != 0 || row / texel < w) {
		if (logged++ < 16) ngl_logc('W', "SURF", "%s: IOSurface %u plane %u of %zu ('%c%c%c%c') format %#x type %#x: "
		    "no CPU copy for it", why, IOSurfaceGetID(s), plane, planes, (char)(fourcc >> 24), (char)(fourcc >> 16),
		    (char)(fourcc >> 8), (char)fourcc, format, type);
		return false;
	}
	if (IOSurfaceLock(s, kIOSurfaceLockReadOnly, NULL) != 0) return false;
	const void *base = planes ? IOSurfaceGetBaseAddressOfPlane(s, plane) : IOSurfaceGetBaseAddress(s);
	if (base == NULL) {
		IOSurfaceUnlock(s, kIOSurfaceLockReadOnly, NULL);
		return false;
	}
	GLint row_length = 0, alignment = 4;
	glGetIntegerv(GL_UNPACK_ROW_LENGTH, &row_length);
	glGetIntegerv(GL_UNPACK_ALIGNMENT, &alignment);
	glPixelStorei(GL_UNPACK_ROW_LENGTH, (GLint)(row / texel));
	glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
	struct ngl_state *st = ngl_cur ? ngl_cur->st : NULL;
	GLuint tex = 0;
	glGetIntegerv(GL_TEXTURE_BINDING_2D, (GLint *)&tex);
	if (st) { ngl_surf_forget_texture(st, tex); ngl_set_swizzle(st, target, false); }
	for (int i = 0; i < 8 && glGetError() != GL_NO_ERROR; i++) {
		/* an older error must not be taken as this upload's */
	}
	glTexImage2D(target, 0, (GLint)ifmt, (GLsizei)w, (GLsizei)h, 0, format, type, base);
	GLenum err = glGetError();
	if (err != GL_NO_ERROR && ifmt != format) {
		/* an ES 2 context takes no sized formats: internal format = format (EXT_texture_rg for RED/RG) */
		glTexImage2D(target, 0, (GLint)format, (GLsizei)w, (GLsizei)h, 0, format, type, base);
		err = glGetError();
	}
	glPixelStorei(GL_UNPACK_ROW_LENGTH, row_length);
	glPixelStorei(GL_UNPACK_ALIGNMENT, alignment);
	IOSurfaceUnlock(s, kIOSurfaceLockReadOnly, NULL);
	if (err == GL_NO_ERROR) ngl_tex_size(target, 0, (GLsizei)w, (GLsizei)h);
	if (logged++ < 16) ngl_logc('I', "SURF", "%s: IOSurface %u plane %u of %zu ('%c%c%c%c') %zux%zu format %#x: "
	    "CPU copy%s", why, IOSurfaceGetID(s), plane, planes, (char)(fourcc >> 24), (char)(fourcc >> 16),
	    (char)(fourcc >> 8), (char)fourcc, w, h, format, err ? ": GL error" : "");
	return err == GL_NO_ERROR;
}

bool
ngl_bind_surface(IOSurfaceRef s, GLenum target, GLenum format, GLenum type, uint32_t plane, bool invert,
    const char *why)
{
	static unsigned logged, binds;
	if (s == NULL) return false;
	if (plane != 0 || IOSurfaceGetPlaneCount(s) > 1) return plane_copy(s, target, format, type, plane, why);
	size_t w = IOSurfaceGetWidth(s), h = IOSurfaceGetHeight(s), pitch = IOSurfaceGetBytesPerRow(s);
	unsigned ifmt = 0;
	enum pipe_format pf = format ? gl_format(format, type, &ifmt) :
	    surface_format(IOSurfaceGetPixelFormat(s), IOSurfaceGetBytesPerElement(s), &ifmt);
	bool argb = !format && IOSurfaceGetPixelFormat(s) == 0 && IOSurfaceGetBytesPerElement(s) == 4;
	if (argb) { pf = PIPE_FORMAT_R8G8B8A8_UNORM; ifmt = GL_RGBA8; }
	if (pf == PIPE_FORMAT_NONE || !w || !h || w > 16384 || h > 16384 || pitch > UINT32_MAX ||
	    pitch < w * util_format_get_blocksize(pf)) {
		if (logged++ < 32) ngl_logc('W', "SURF", "%s: IOSurface %u (%zux%zu pitch %zu '%c%c%c%c', GL format %#x): "
		    "not a format bound here", why, IOSurfaceGetID(s), w, h, pitch, (char)(IOSurfaceGetPixelFormat(s) >> 24),
		    (char)(IOSurfaceGetPixelFormat(s) >> 16), (char)(IOSurfaceGetPixelFormat(s) >> 8),
		    (char)IOSurfaceGetPixelFormat(s), format);
		return false;
	}
	struct ngl_state *st = ngl_cur ? ngl_cur->st : NULL;
	struct pipe_resource *res = NULL;
	struct ngl_surf *copy = NULL;
	const bool in_place = ngl_in_place() && !invert;
	if (in_place) {
		/* softpipe: sw_winsys.c maps the IOSurface itself (WINSYS_HANDLE_TYPE_SHARED, handle = its ID) */
		struct pipe_resource templ = {
			.target = PIPE_TEXTURE_2D, .format = pf, .width0 = (uint32_t)w,
			.height0 = (uint32_t)h, .depth0 = 1, .array_size = 1,
			.bind = PIPE_BIND_SAMPLER_VIEW | PIPE_BIND_RENDER_TARGET | PIPE_BIND_SHARED,
		};
		struct winsys_handle wh = {
			.type = WINSYS_HANDLE_TYPE_SHARED, .handle = IOSurfaceGetID(s),
			.stride = (unsigned)pitch, .format = pf, .modifier = DRM_FORMAT_MOD_LINEAR,
		};
		struct pipe_screen *screen = ngl_screen();
		res = screen && screen->resource_from_handle ?
		    screen->resource_from_handle(screen, &templ, &wh, PIPE_HANDLE_USAGE_FRAMEBUFFER_WRITE) : NULL;
	} else {
		pthread_mutex_lock(&surf_lock);
		copy = surf_get(s, pf, (uint32_t)w, (uint32_t)h);
		if (copy != NULL) {
			copy->used = rounds;
			if (copy->invert != invert) {
				copy->invert = invert;
				copy->seeded = false;   /* the other way up: what the resource holds is not the surface any more */
			}
			if (!copy->written && (!copy->seeded || IOSurfaceGetSeed(s) != copy->seed)) {
				surf_upload(copy);
			}
			pipe_resource_reference(&res, copy->res);
			copy->refs++;           /* the name below; dropped when it is bound to something else or deleted */
		}
		pthread_mutex_unlock(&surf_lock);
	}
	if (res == NULL) {
		if (logged++ < 32) ngl_logc('E', "SURF", "%s: IOSurface %u (%zux%zu pitch %zu, %s): %s failed", why,
		    IOSurfaceGetID(s), w, h, pitch, util_format_short_name(pf), in_place ? "import" : "copy");
		return false;
	}
	struct ngl_image img = {.res = res, .format = pf, .internalformat = ifmt};
	bool ok;
	if (target == GL_RENDERBUFFER) {
		/* the surface is the renderbuffer's storage (a CAEAGLLayer drawable, -attachImage: on a renderbuffer) */
		GLint rb = 0;
		glGetIntegerv(GL_RENDERBUFFER_BINDING, &rb);
		if (st != NULL && rb > 0) {
			ngl_surf_forget_renderbuffer(st, (GLuint)rb);
		}
		_mesa_EGLImageTargetRenderbufferStorageOES(target, (GLeglImageOES)&img);
		ok = glGetError() == GL_NO_ERROR;
		if (copy != NULL) {
			if (ok && st != NULL && rb > 0 && rb < NGL_NAMES) {
				st->rb_surf[rb] = copy;
			} else {
				pthread_mutex_lock(&surf_lock);
				surf_unref(copy);
				pthread_mutex_unlock(&surf_lock);
			}
		}
	} else {
		GLuint tex = 0;
		glGetIntegerv(GL_TEXTURE_BINDING_2D, (GLint *)&tex);
		if (st) { ngl_surf_forget_texture(st, tex); ngl_set_swizzle(st, target, argb); }
		_mesa_EGLImageTargetTexture2DOES(target, (GLeglImageOES)&img);
		ok = glGetError() == GL_NO_ERROR;
		if (ok) {
			ngl_tex_size(target, 0, (GLsizei)w, (GLsizei)h);
		}
		if (copy != NULL) {
			if (ok && st != NULL && tex > 0 && tex < NGL_NAMES) {
				st->tex_surf[tex] = copy;
			} else {
				pthread_mutex_lock(&surf_lock);
				surf_unref(copy);
				pthread_mutex_unlock(&surf_lock);
			}
		}
	}
	pipe_resource_reference(&res, NULL);
	if (binds++ < 4) {
		ngl_logc('I', "SURF", "%s: IOSurface %u %zux%zu (%s) bound %s as %s%s%s", why, IOSurfaceGetID(s), w, h,
		    util_format_short_name(pf), in_place ? "in place" : "as a copy",
		    target == GL_RENDERBUFFER ? "a renderbuffer" : "a texture", invert ? ", turned over" : "", ok ? "" : ": GL error");
	}
	return ok;
}

void
ngl_set_swizzle(struct ngl_state *st, GLenum target, bool argb)
{
	if (st == NULL) return;
	GLint tex = 0;
	glGetIntegerv(GL_TEXTURE_BINDING_2D, &tex);
	if (tex > 0 && tex < NGL_NAMES && st->tex_argb[tex] != argb) {
		st->tex_argb[tex] = argb;
		glTexParameteri(target, GL_TEXTURE_SWIZZLE_R, argb ? GL_ALPHA : GL_RED);
		glTexParameteri(target, GL_TEXTURE_SWIZZLE_G, argb ? GL_BLUE : GL_GREEN);
		glTexParameteri(target, GL_TEXTURE_SWIZZLE_B, argb ? GL_GREEN : GL_BLUE);
		glTexParameteri(target, GL_TEXTURE_SWIZZLE_A, argb ? GL_RED : GL_ALPHA);
	}
}
