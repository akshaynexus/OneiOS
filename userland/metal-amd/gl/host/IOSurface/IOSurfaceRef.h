/*
 * Copyright (c) 2026 OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * The Linux host test's stand-in for the IOSurface calls OneiOSRadeonGL makes (userland/metal-amd/gl): memory with a
 * width, height, row length, pixel format, an ID and a seed that moves when a writer unlocks it, as IOSurface's
 * does. Written from the calls' documented meaning; not Apple's header; nothing of Apple's is in it.
 */
#ifndef ONEIOS_GL_HOST_IOSURFACE_H
#define ONEIOS_GL_HOST_IOSURFACE_H

#include <stddef.h>
#include <stdint.h>
#include <CoreFoundation/CoreFoundation.h>

typedef struct __IOSurface *IOSurfaceRef;
typedef uint32_t IOSurfaceID;
enum { kIOSurfaceLockReadOnly = 1, kIOSurfaceLockAvoidSync = 2 };

CFTypeID IOSurfaceGetTypeID(void);
IOSurfaceRef IOSurfaceLookup(IOSurfaceID id);
IOSurfaceID IOSurfaceGetID(IOSurfaceRef s);
size_t IOSurfaceGetWidth(IOSurfaceRef s);
size_t IOSurfaceGetHeight(IOSurfaceRef s);
size_t IOSurfaceGetBytesPerRow(IOSurfaceRef s);
size_t IOSurfaceGetBytesPerElement(IOSurfaceRef s);
uint32_t IOSurfaceGetPixelFormat(IOSurfaceRef s);
size_t IOSurfaceGetAllocSize(IOSurfaceRef s);
size_t IOSurfaceGetPlaneCount(IOSurfaceRef s);
size_t IOSurfaceGetWidthOfPlane(IOSurfaceRef s, size_t plane);
size_t IOSurfaceGetHeightOfPlane(IOSurfaceRef s, size_t plane);
size_t IOSurfaceGetBytesPerRowOfPlane(IOSurfaceRef s, size_t plane);
size_t IOSurfaceGetBytesPerElementOfPlane(IOSurfaceRef s, size_t plane);
void *IOSurfaceGetBaseAddress(IOSurfaceRef s);
void *IOSurfaceGetBaseAddressOfPlane(IOSurfaceRef s, size_t plane);
int IOSurfaceLock(IOSurfaceRef s, uint32_t options, uint32_t *seed);
int IOSurfaceUnlock(IOSurfaceRef s, uint32_t options, uint32_t *seed);
uint32_t IOSurfaceGetSeed(IOSurfaceRef s);

/* the test's own: a single-plane surface of w x h, 4 bytes a pixel ('BGRA', or 0 for CA's own ARGB) */
IOSurfaceRef host_iosurface_create(size_t w, size_t h, uint32_t fourcc);
int host_iosurface_locks(IOSurfaceRef s);       /* locks not yet unlocked (0 after a balanced use) */

#endif
