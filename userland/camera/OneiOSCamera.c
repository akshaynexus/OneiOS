/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOSCamera: the capture plug-in for iOS 27's capture server (cameracaptured; mediaserverd on a
 * release where the capture servers live there). The route is the one of HakiOS and iOSPlus10
 * (iOSPlus10 userland/camera/media_hooks.c, from HakiOS userland/src/inject_media.c, MIT License,
 * Copyright (c) 2025-2026 the HakiOS authors): Apple's own path (AppleH16CamIn and its ISP firmware
 * protocol) cannot be served by another camera, so +[BWFigCaptureDeviceVendor
 * sharedCaptureDeviceVendor], which gives nil without Apple's ISP driver, is made to give a vendor
 * whose device-create function is ours, and that device's stream hands the capture graph
 * CVPixelBuffers.
 *
 * What is here, and what is not (README.md):
 *   - the constructor: the right process only, an off switch, a report of what the loaded CMCapture
 *     has (every class and method the route replaces, found by name at run time: no address table,
 *     so nothing to re-find for the iPhone18,2 cache but the report to read);
 *   - the frame pump: /dev/oneios-camera (NV12 from One UI's camera HAL) into IOSurface-backed
 *     420f CVPixelBuffers from a pool, one copy per frame, handed to a callback;
 *   - the vendor hook, installed only when a device-create function is linked in.
 *   - NOT here: that function, oneios_camera_device_create(): the FigCaptureDevice and
 *     FigCaptureStream objects with CMCapture 27's vtables (iOSPlus10's camera_device.c is that
 *     object for iOS 16.5's layout). It has to be written against the 24A446 shared cache, which is
 *     not on the build machine. Until then the plug-in loads, reports, and replaces nothing.
 *
 * NOT COMPILED for iOS: there is no iPhoneOS SDK on the build machine (build.sh). The host test
 * checks its syntax against stand-in headers (tests/stub-sdk) and runs the reader it uses.
 *
 * Files it reads: /private/var/oneios/camera/off (exists: do nothing).
 * Log: syslog, and /private/var/oneios/log/camera.log.
 */
#include <CoreFoundation/CoreFoundation.h>
#include <CoreVideo/CoreVideo.h>
#include <dlfcn.h>
#include <errno.h>
#include <mach-o/dyld.h>
#include <objc/message.h>
#include <objc/runtime.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <syslog.h>
#include <unistd.h>
#include "OneiOSCamera.h"
#include "oneios_camera_reader.h"

#define OFF_FILE "/private/var/oneios/camera/off"
#define LOG_FILE "/private/var/oneios/log/camera.log"

/* The device-create function a later file supplies (FigCaptureDevice for CMCapture 27). */
extern int oneios_camera_device_create(const void *allocator, const void *options, void **out) __attribute__((weak_import));

static void
say(const char *fmt, ...)
{
	char line[512];
	va_list ap;
	FILE *f;

	va_start(ap, fmt);
	vsnprintf(line, sizeof(line), fmt, ap);
	va_end(ap);
	syslog(LOG_NOTICE, "ONEIOS CAMERA: ios: %s", line);
	if ((f = fopen(LOG_FILE, "a")) != NULL) {
		fprintf(f, "%s[%d]: %s\n", getprogname(), (int)getpid(), line);
		fclose(f);
	}
}

/* ---- the frame pump ---- */
static struct {
	pthread_t thread;
	bool running;                   /* atomic */
	uint32_t sensor, width, height, fps;
	oneios_camera_frame_fn fn;
	void *ctx;
} pump;

static CVPixelBufferPoolRef
make_pool(uint32_t width, uint32_t height)
{
	/* 420f, IOSurface-backed: what the capture graph's nodes and the preview layer take without a copy. */
	int32_t format = kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, w = (int32_t)width, h = (int32_t)height;
	CFNumberRef f = CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt32Type, &format);
	CFNumberRef cw = CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt32Type, &w), ch = CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt32Type, &h);
	CFDictionaryRef surface = CFDictionaryCreate(kCFAllocatorDefault, NULL, NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
	const void *keys[4] = { kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey };
	const void *values[4] = { f, cw, ch, surface };
	CFDictionaryRef attributes = CFDictionaryCreate(kCFAllocatorDefault, keys, values, 4, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
	CVPixelBufferPoolRef pool = NULL;

	if (CVPixelBufferPoolCreate(kCFAllocatorDefault, NULL, attributes, &pool) != kCVReturnSuccess) {
		pool = NULL;
	}
	CFRelease(attributes);
	CFRelease(surface);
	CFRelease(ch);
	CFRelease(cw);
	CFRelease(f);
	return pool;
}

static void *
pump_run(void *arg)
{
	oneios_camera_reader reader;
	CVPixelBufferPoolRef pool = NULL;
	uint32_t pool_w = 0, pool_h = 0;
	unsigned quiet = 0;
	int e;

	(void)arg;
	if ((e = oneios_camera_reader_open(&reader, NULL)) != 0 ||
	    (e = oneios_camera_reader_select(&reader, pump.sensor, pump.width, pump.height, pump.fps)) != 0) {
		say("pump: %s: %s", ONEIOS_CAMERA_DEVICE, strerror(-e));
		if (reader.scratch) {
			oneios_camera_reader_close(&reader);
		}
		__atomic_store_n(&pump.running, false, __ATOMIC_RELEASE);
		return NULL;
	}
	while (__atomic_load_n(&pump.running, __ATOMIC_ACQUIRE)) {
		struct oneios_camera_frame header;
		const uint8_t *picture;
		CVPixelBufferRef buffer = NULL;

		if ((e = oneios_camera_reader_next(&reader, &header, &picture)) != 0) {
			/* No frame in time, or the camera stopped on the host: the next read starts it again. */
			if (quiet++ % 30 == 0) {
				say("pump: no frame: %s", strerror(-e));
			}
			if (e != -EAGAIN) {
				usleep(100000);
			}
			continue;
		}
		quiet = 0;
		if (!pool || header.width != pool_w || header.height != pool_h) {   /* the host gave the nearest size it has */
			if (pool) {
				CFRelease(pool);
			}
			pool = make_pool(header.width, header.height);
			pool_w = header.width;
			pool_h = header.height;
			say("pump: camera %u, %ux%u at %u fps, turned %u", header.sensor, header.width, header.height, header.fps, header.orientation);
		}
		if (!pool || CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool, &buffer) != kCVReturnSuccess || !buffer) {
			continue;               /* the graph holds every buffer: this frame is dropped */
		}
		if (CVPixelBufferLockBaseAddress(buffer, 0) == kCVReturnSuccess) {
			e = oneios_camera_copy_planes(&header, picture, CVPixelBufferGetBaseAddressOfPlane(buffer, 0),
			    CVPixelBufferGetBytesPerRowOfPlane(buffer, 0), CVPixelBufferGetBaseAddressOfPlane(buffer, 1),
			    CVPixelBufferGetBytesPerRowOfPlane(buffer, 1), header.width, header.height);
			CVPixelBufferUnlockBaseAddress(buffer, 0);
			if (e == 0) {
				pump.fn(pump.ctx, buffer, &header);
			}
		}
		CVPixelBufferRelease(buffer);
	}
	if (pool) {
		CFRelease(pool);
	}
	oneios_camera_reader_close(&reader);    /* the kernel stops the camera */
	return NULL;
}

int
oneios_camera_pump_start(uint32_t sensor, uint32_t width, uint32_t height, uint32_t fps, oneios_camera_frame_fn fn, void *ctx)
{
	if (__atomic_load_n(&pump.running, __ATOMIC_ACQUIRE)) {
		return EBUSY;
	}
	pump.sensor = sensor;
	pump.width = width;
	pump.height = height;
	pump.fps = fps;
	pump.fn = fn;
	pump.ctx = ctx;
	__atomic_store_n(&pump.running, true, __ATOMIC_RELEASE);
	if (pthread_create(&pump.thread, NULL, pump_run, NULL) != 0) {
		__atomic_store_n(&pump.running, false, __ATOMIC_RELEASE);
		return EAGAIN;
	}
	return 0;
}

void
oneios_camera_pump_stop(void)
{
	if (__atomic_exchange_n(&pump.running, false, __ATOMIC_ACQ_REL)) {
		pthread_join(pump.thread, NULL);
	}
}

/* ---- CMCapture: the report and the vendor hook ---- */
/* What the route replaces in iOS 16's CMCapture (iOSPlus10 media_hooks.c). The report says which are in this one. */
static const struct { const char *cls, *selector; bool instance; } wanted[] = {
	{ "BWFigCaptureDeviceVendor", "sharedCaptureDeviceVendor", false },
	{ "BWFigCaptureDeviceVendor", "initWithDeviceCreateFunction:", true },
	{ "BWAudioSourceNode", "start:", true },
	{ "FigVideoCaptureConnectionConfiguration", "videoStabilizationMethod", true },
	{ "FigVideoCaptureConnectionConfiguration", "videoStabilizationType", true },
	{ "FigCaptureStillImageSettings", "qualityPrioritization", true },
	{ "BWGNRNode", "renderSampleBuffer:forInput:", true },
	{ "BWNodeOutput", "emitSampleBuffer:", true },
	{ "BWCoreImageFilterRenderer", "prewarmPortraitFilterVersion:semanticStyleFilters:", true },
};

static IMP stock_vendor;
static id our_vendor;
static pthread_once_t vendor_once = PTHREAD_ONCE_INIT;

static void
make_vendor(void)
{
	id made = ((id (*)(Class, SEL))objc_msgSend)(objc_getClass("BWFigCaptureDeviceVendor"), sel_registerName("alloc"));

	our_vendor = ((id (*)(id, SEL, void *))objc_msgSend)(made, sel_registerName("initWithDeviceCreateFunction:"),
	    (void *)oneios_camera_device_create);
	say("vendor: %s", our_vendor ? "made with our device" : "initWithDeviceCreateFunction: gave nil");
}

static id
replacement_vendor(id self, SEL _cmd)
{
	id stock = stock_vendor ? ((id (*)(id, SEL))stock_vendor)(self, _cmd) : NULL;

	if (stock || access(ONEIOS_CAMERA_DEVICE, F_OK) != 0) {
		return stock;           /* Apple's own (never here), or no camera from the host: as stock */
	}
	pthread_once(&vendor_once, make_vendor);
	return our_vendor;
}

static void
image_added(const struct mach_header *header, intptr_t slide)
{
	static bool done;
	Dl_info info;
	Class cls;
	unsigned found = 0;

	(void)slide;
	if (done || !dladdr(header, &info) || !info.dli_fname || !strstr(info.dli_fname, "/CMCapture.framework/")) {
		return;
	}
	done = true;
	for (unsigned i = 0; i < sizeof(wanted) / sizeof(wanted[0]); i++) {
		Method m = NULL;

		if ((cls = objc_getClass(wanted[i].cls)) != NULL) {
			m = wanted[i].instance ? class_getInstanceMethod(cls, sel_registerName(wanted[i].selector)) :
			    class_getClassMethod(cls, sel_registerName(wanted[i].selector));
		}
		found += m != NULL;
		say("CMCapture: %c[%s %s]: %s", wanted[i].instance ? '-' : '+', wanted[i].cls, wanted[i].selector,
		    m ? "found" : cls ? "the class has no such method" : "no such class");
	}
	say("CMCapture: %s, %u of %u found; %s: %s", info.dli_fname, found, (unsigned)(sizeof(wanted) / sizeof(wanted[0])), ONEIOS_CAMERA_DEVICE,
	    access(ONEIOS_CAMERA_DEVICE, F_OK) == 0 ? "there" : "not there (no camera from the host)");

	cls = objc_getClass("BWFigCaptureDeviceVendor");
	Method shared = cls ? class_getClassMethod(cls, sel_registerName("sharedCaptureDeviceVendor")) : NULL;
	if (oneios_camera_device_create == NULL) {
		say("hook: off: no device-create function in this build (the FigCaptureDevice for CMCapture 27 is not written)");
	} else if (!shared || !class_getInstanceMethod(cls, sel_registerName("initWithDeviceCreateFunction:"))) {
		say("hook: off: this CMCapture has no BWFigCaptureDeviceVendor with a device-create function");
	} else {
		stock_vendor = method_setImplementation(shared, (IMP)replacement_vendor);
		say("hook: on: +[BWFigCaptureDeviceVendor sharedCaptureDeviceVendor]");
	}
}

__attribute__((constructor))
static void
oneios_camera_load(void)
{
	const char *name = getprogname();

	if (!name || (strcmp(name, "cameracaptured") != 0 && strcmp(name, "mediaserverd") != 0)) {
		return;
	}
	if (access(OFF_FILE, F_OK) == 0) {
		say("%s exists: off, nothing replaced", OFF_FILE);
		return;
	}
	_dyld_register_func_for_add_image(image_added);         /* also called for every image already there */
}
