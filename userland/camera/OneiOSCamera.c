/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOSCamera: the capture plug-in's log, off switch and load report. The plug-in is one dylib that iOS 27's
 * CMCapture loads itself, as /System/Library/MediaCapture/H9ISP.mediacapture, when its probe finds the kernel's
 * AppleH9CamIn (drivers/camera/OneiOSCameraService.cpp; README.md has the whole route). The device it makes:
 * OneiOSCaptureDevice.c.
 *
 * Before 2026-11-12 this file replaced +[BWFigCaptureDeviceVendor sharedCaptureDeviceVendor] (HakiOS's route on
 * iOS 16, where nothing loaded a plug-in by itself). iOS 27 does not need that: the vendor takes the create function
 * from the plug-in it loads (FigCaptureGetCaptureDeviceCreateFunction). Nothing of CMCapture is replaced.
 *
 * Files it reads: /private/var/oneios/camera/off (exists: no device). Log: oneios_camera_say().
 */
#include <errno.h>
#include <fcntl.h>
#include <os/log.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "OneiOSCamera.h"

#define OFF_FILE "/private/var/oneios/camera/off"
#define LOG_FILE "/private/var/oneios/log/camera.log"

void
oneios_camera_say(const char *fmt, ...)
{
	static int console = -2;
	char line[512];
	va_list ap;
	int n;
	FILE *f;

	n = snprintf(line, sizeof(line), "ONEIOS CAMERA: ios: %s[%d]: ", getprogname(), (int)getpid());
	va_start(ap, fmt);
	vsnprintf(line + n, sizeof(line) - (size_t)n - 1, fmt, ap);
	va_end(ap);
	os_log(OS_LOG_DEFAULT, "%{public}s", line);
	if (console == -2) {
		console = open("/dev/console", O_WRONLY | O_NOCTTY | O_CLOEXEC);
	}
	n = (int)strlen(line);
	line[n++] = '\n';
	line[n] = 0;
	if (console >= 0) {
		(void)write(console, line, (size_t)n);
	}
#ifdef ONEIOS_CAMERA_SAY_STDERR
	(void)write(2, line, (size_t)n);        /* the host tests */
#endif
	if ((f = fopen(LOG_FILE, "a")) != NULL) {
		fputs(line, f);
		fclose(f);
	}
}

bool
oneios_camera_off(void)
{
	static bool said;
	bool off = access(OFF_FILE, F_OK) == 0;

	if (off && !said) {
		said = true;
		oneios_camera_say("%s exists: off, no device", OFF_FILE);
	}
	return off;
}

__attribute__((constructor))
static void
oneios_camera_load(void)
{
	oneios_camera_say("loaded (H9ISPCaptureDeviceCreate for CMCapture's vendor)%s", access(OFF_FILE, F_OK) == 0 ? "; off" : "");
}
