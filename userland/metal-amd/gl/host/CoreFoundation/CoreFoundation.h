/*
 * Copyright (c) 2026 OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * The Linux host test's stand-in for the few CoreFoundation calls OneiOSRadeonGL makes (userland/metal-amd/gl):
 * a reference count on the stand-in IOSurfaces of iosurface_host.c. Not Apple's header; nothing of Apple's is in it.
 */
#ifndef ONEIOS_GL_HOST_CF_H
#define ONEIOS_GL_HOST_CF_H

typedef const void *CFTypeRef;
typedef unsigned long CFTypeID;

CFTypeRef CFRetain(CFTypeRef object);
void CFRelease(CFTypeRef object);
CFTypeID CFGetTypeID(CFTypeRef object);

#endif
