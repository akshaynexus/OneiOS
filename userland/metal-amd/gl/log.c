/*
 * Copyright (c) 2026 iOSPlus10 contributors, OneiOS contributors
 * SPDX-License-Identifier: MIT
 *
 * OneiOSRadeonGL's log (NegroniGL's log.c, iOSPlus10, with the A56 Metal plug-in's channel of m2m.h):
 *
 *   every record  os_log, subsystem "com.oneios.radeongl", category CAT, "<sev> <message>" (iOS's own log:
 *                 `oneios-tool phone pull-log DIR`, then `oneios-tool ioslog` on it); and standard error with
 *                 ONEIOS_GL_LOG=stderr (always on the Linux host)
 *   some records  one text row of the kernel (hosted 0039), in xnuhost.log (`oneios-tool phone log`):
 *                     oneios-gpu-text: PROCESS[PID] RG <sev> <CAT> <message, cut at 63 characters>
 *                 E records (at most 12 a process) and the first 3 I/W records of the categories GATE, SCR, CTX,
 *                 VIEW and GLI: the bundle's load, the screen, the first contexts, the first drawables and presents.
 *                 A row is streamed as IOServiceOpen calls of types 0x41477fff (a new row) and 0x414770cc
 *                 (character cc) on the accelerator (the A56's answers each with an error and turns the row into one
 *                 line); a character is one call (about 70 us here), so the budget above bounds the cost: never per
 *                 frame. The accelerator is the one libGFXShared handed gldInitializeLibrary (ngl_log_service).
 */
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#ifdef __APPLE__
#include <os/log.h>
#include <IOKit/IOKitLib.h>
#include <mach/mach.h>
#endif

#include "oneios_gl.h"

#define ROW_MAX 63              /* the kernel's row (0039 hosted_gpu_row.text) */

static pthread_once_t once = PTHREAD_ONCE_INIT;
static int to_stderr;
#ifdef __APPLE__
static os_log_t oslog;
static io_service_t row_service = IO_OBJECT_NULL;
#endif

static void
log_open(void)
{
#ifdef __APPLE__
	const char *w = getenv("ONEIOS_GL_LOG");
	oslog = os_log_create("com.oneios.radeongl", "gl");
	to_stderr = w != NULL && strcmp(w, "stderr") == 0;
#else
	to_stderr = 1;
#endif
}

void
ngl_log_service(unsigned service)
{
#ifdef __APPLE__
	if (row_service != IO_OBJECT_NULL) {
		return;
	}
	if (service != 0 && IOObjectConformsTo(service, "IOAcceleratorES")) {
		IOObjectRetain(service);
		row_service = service;
		return;
	}
	/* NegroniGL read gldInitializeLibrary's first argument as the accelerators' services; if it is not one, the one
	 * IOAcceleratorES the A56 kernel publishes (AGXAccelerator) is found by matching, once */
	row_service = IOServiceGetMatchingService(MACH_PORT_NULL, IOServiceMatching("IOAcceleratorES"));
#else
	(void)service;
#endif
}

#ifdef __APPLE__
static void
row(const char *text)
{
	static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
	io_connect_t c = IO_OBJECT_NULL;
	pthread_mutex_lock(&lock);      /* one row at a time: the kernel keeps one pending row a process */
	if (IOServiceOpen(row_service, mach_task_self(), 0x41477fff, &c) == KERN_SUCCESS && c != IO_OBJECT_NULL) {
		IOServiceClose(c);
	}
	for (unsigned i = 0; text[i] != '\0' && i < ROW_MAX; i++) {
		unsigned char ch = (unsigned char)text[i];
		c = IO_OBJECT_NULL;
		if (IOServiceOpen(row_service, mach_task_self(), 0x41477000u | (ch >= 0x20 && ch < 0x7f ? ch : '?'), &c) ==
		    KERN_SUCCESS && c != IO_OBJECT_NULL) {
			IOServiceClose(c);
		}
	}
	pthread_mutex_unlock(&lock);
}

static bool
wants_row(char sev, const char *cat)
{
	static int errors, budget[5];
	static const char *const cats[5] = {"GATE", "SCR", "CTX", "VIEW", "GLI"};
	if (sev == 'E') {
		return __atomic_fetch_add(&errors, 1, __ATOMIC_RELAXED) < 12;
	}
	for (int i = 0; i < 5; i++) {
		if (strcmp(cat, cats[i]) == 0) {
			return __atomic_fetch_add(&budget[i], 1, __ATOMIC_RELAXED) < 3;
		}
	}
	return false;
}
#endif

void
ngl_logc(char sev, const char *cat, const char *fmt, ...)
{
	char line[640];
	va_list ap;
	pthread_once(&once, log_open);
	va_start(ap, fmt);
	int n = vsnprintf(line, sizeof(line) - 1, fmt, ap);
	va_end(ap);
	if (n < 0) {
		return;
	}
	if (n > (int)sizeof(line) - 2) {
		n = (int)sizeof(line) - 2;
	}
	line[n] = 0;
	for (char *nl = strchr(line, '\n'); nl != NULL; nl = strchr(nl, '\n')) {
		*nl = ' ';      /* one record, one line */
	}
#ifdef __APPLE__
	os_log_with_type(oslog, sev == 'E' ? OS_LOG_TYPE_ERROR : OS_LOG_TYPE_DEFAULT, "%{public}c %{public}s %{public}s",
	    sev, cat, line);
	if (row_service != IO_OBJECT_NULL && wants_row(sev, cat)) {
		char r[ROW_MAX + 1];
		snprintf(r, sizeof(r), "RG %c %s %s", sev, cat, line);
		row(r);
	}
#endif
	if (to_stderr) {
		char out[700];
		int m = snprintf(out, sizeof(out), "oneios-gl: %c %s %s\n", sev, cat, line);
		if (m > 0) {
			(void)write(2, out, (size_t)(m < (int)sizeof(out) ? m : (int)sizeof(out) - 1));
		}
	}
}
