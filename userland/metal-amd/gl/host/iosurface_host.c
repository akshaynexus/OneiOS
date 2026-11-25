/*
 * Copyright (c) 2026 OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * The Linux host test's stand-in IOSurfaces (IOSurface/IOSurfaceRef.h): enough of IOSurface's behaviour for
 * OneiOSRadeonGL's tests. A surface is plain memory with an ID (looked up by IOSurfaceLookup, +1), a reference count
 * (CFRetain/CFRelease), and a seed that moves when a lock that was not read-only is unlocked, as IOSurface's seed
 * moves when the surface is written. Locks are counted, so the test can say every lock was unlocked.
 */
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <IOSurface/IOSurfaceRef.h>

#define MAGIC 0x494f5346u       /* 'IOSF' */
#define MAX_SURFACES 256

struct __IOSurface {
	uint32_t magic;
	int refs, locks;
	IOSurfaceID id;
	size_t w, h, row, bpe;
	uint32_t fourcc, seed;
	uint8_t *mem;
};

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static struct __IOSurface *table[MAX_SURFACES];
static IOSurfaceID next_id = 1;

CFTypeID IOSurfaceGetTypeID(void) { return 0x10f5; }
CFTypeID CFGetTypeID(CFTypeRef o) { return o && ((const struct __IOSurface *)o)->magic == MAGIC ? 0x10f5 : 0; }

CFTypeRef
CFRetain(CFTypeRef o)
{
	pthread_mutex_lock(&lock);
	((struct __IOSurface *)(uintptr_t)o)->refs++;
	pthread_mutex_unlock(&lock);
	return o;
}

void
CFRelease(CFTypeRef o)
{
	struct __IOSurface *s = (struct __IOSurface *)(uintptr_t)o;
	pthread_mutex_lock(&lock);
	int left = --s->refs;
	if (left == 0) {
		for (int i = 0; i < MAX_SURFACES; i++) {
			if (table[i] == s) {
				table[i] = NULL;
			}
		}
	}
	pthread_mutex_unlock(&lock);
	if (left == 0) {
		s->magic = 0;
		free(s->mem);
		free(s);
	}
}

IOSurfaceRef
host_iosurface_create(size_t w, size_t h, uint32_t fourcc)
{
	struct __IOSurface *s = calloc(1, sizeof(*s));
	if (s == NULL) {
		return NULL;
	}
	s->magic = MAGIC;
	s->refs = 1;
	s->w = w;
	s->h = h;
	s->bpe = 4;
	s->row = (w * 4 + 63) / 64 * 64;        /* a row length that is not w * 4, as IOSurface's often is not */
	s->fourcc = fourcc;
	s->mem = calloc(s->row, h);
	pthread_mutex_lock(&lock);
	s->id = next_id++;
	for (int i = 0; i < MAX_SURFACES; i++) {
		if (table[i] == NULL) {
			table[i] = s;
			break;
		}
	}
	pthread_mutex_unlock(&lock);
	return s;
}

IOSurfaceRef
IOSurfaceLookup(IOSurfaceID id)
{
	struct __IOSurface *found = NULL;
	pthread_mutex_lock(&lock);
	for (int i = 0; i < MAX_SURFACES && found == NULL; i++) {
		if (table[i] != NULL && table[i]->id == id) {
			found = table[i];
			found->refs++;
		}
	}
	pthread_mutex_unlock(&lock);
	return found;
}

IOSurfaceID IOSurfaceGetID(IOSurfaceRef s) { return s->id; }
size_t IOSurfaceGetWidth(IOSurfaceRef s) { return s->w; }
size_t IOSurfaceGetHeight(IOSurfaceRef s) { return s->h; }
size_t IOSurfaceGetBytesPerRow(IOSurfaceRef s) { return s->row; }
size_t IOSurfaceGetBytesPerElement(IOSurfaceRef s) { return s->bpe; }
uint32_t IOSurfaceGetPixelFormat(IOSurfaceRef s) { return s->fourcc; }
size_t IOSurfaceGetAllocSize(IOSurfaceRef s) { return s->row * s->h; }
size_t IOSurfaceGetPlaneCount(IOSurfaceRef s) { (void)s; return 0; }
size_t IOSurfaceGetWidthOfPlane(IOSurfaceRef s, size_t p) { return p == 0 ? s->w : 0; }
size_t IOSurfaceGetHeightOfPlane(IOSurfaceRef s, size_t p) { return p == 0 ? s->h : 0; }
size_t IOSurfaceGetBytesPerRowOfPlane(IOSurfaceRef s, size_t p) { return p == 0 ? s->row : 0; }
size_t IOSurfaceGetBytesPerElementOfPlane(IOSurfaceRef s, size_t p) { return p == 0 ? s->bpe : 0; }
void *IOSurfaceGetBaseAddress(IOSurfaceRef s) { return s->mem; }
void *IOSurfaceGetBaseAddressOfPlane(IOSurfaceRef s, size_t p) { return p == 0 ? s->mem : NULL; }
uint32_t IOSurfaceGetSeed(IOSurfaceRef s) { return __atomic_load_n(&s->seed, __ATOMIC_ACQUIRE); }
int host_iosurface_locks(IOSurfaceRef s) { return __atomic_load_n(&s->locks, __ATOMIC_ACQUIRE); }

int
IOSurfaceLock(IOSurfaceRef s, uint32_t options, uint32_t *seed)
{
	(void)options;
	__atomic_add_fetch(&s->locks, 1, __ATOMIC_ACQ_REL);
	if (seed != NULL) {
		*seed = IOSurfaceGetSeed(s);
	}
	return 0;
}

int
IOSurfaceUnlock(IOSurfaceRef s, uint32_t options, uint32_t *seed)
{
	if (!(options & kIOSurfaceLockReadOnly)) {
		__atomic_add_fetch(&s->seed, 1, __ATOMIC_ACQ_REL);   /* a writer's unlock: the surface changed */
	}
	__atomic_sub_fetch(&s->locks, 1, __ATOMIC_ACQ_REL);
	if (seed != NULL) {
		*seed = IOSurfaceGetSeed(s);
	}
	return 0;
}
