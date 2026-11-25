/*
 * Copyright (c) 2026 OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * oneios-gl-probe: the lasting guest tool of the Galaxy A56's OpenGL ES (userland/metal-amd/gl). It loads
 * OneiOSRadeonGL.bundle outside EAGL and runs its oneios_gl_probe: the screen (radeonsi through K3, or softpipe), a
 * triangle drawn and read back, an IOSurface drawn into and read on the CPU. One report line on standard output, the
 * bundle's own lines on standard error (ONEIOS_GL_LOG=stderr), and the same records in iOS's log
 * (com.oneios.radeongl). Exit 0 when every step passed.
 *
 *   oneios-gl-probe [BUNDLE_EXECUTABLE]   default /System/Library/Extensions/OneiOSRadeonGL.bundle/OneiOSRadeonGL
 *   ONEIOS_GL_SOFTPIPE=1 oneios-gl-probe  the same on softpipe (the EAGL road's checks without the GPU)
 */
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>

int
main(int argc, char **argv)
{
	const char *path = argc > 1 ? argv[1] : "/System/Library/Extensions/OneiOSRadeonGL.bundle/OneiOSRadeonGL";
	setenv("ONEIOS_GL_LOG", "stderr", 0);
	void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
	if (h == NULL) {
		printf("oneios-gl-probe: FAIL: dlopen %s: %s\n", path, dlerror());
		return 2;
	}
	int (*probe)(char *, size_t) = (int (*)(char *, size_t))dlsym(h, "oneios_gl_probe");
	if (probe == NULL) {
		printf("oneios-gl-probe: FAIL: %s has no oneios_gl_probe\n", path);
		return 2;
	}
	char report[512] = "";
	int r = probe(report, sizeof(report));
	printf("oneios-gl-probe: %s: %s\n", r == 0 ? "PASS" : "FAIL", report);
	return r == 0 ? 0 : 1;
}
