/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOSCaptureDevice: the FigCaptureDevice of the phone's cameras for iOS 27's capture server.
 *
 * Ported from iOSPlus10 userland/camera/camera_device.c, which is ported from HakiOS userland/src/camera_device.c
 * (MIT License, Copyright (c) 2025-2026 the HakiOS authors). What is ours: the iOS 27 entry point, the cameras the
 * kernel lists, the formats read from the device's own AVCaptureSession.plist, finished NV12 frames in place of
 * the raw sensor and its software ISP, the user client in place of a /dev node.
 *
 * How it is reached. CMCapture 27 probes for a camera driver; for AppleH9CamIn (drivers/camera/OneiOSCameraService.cpp)
 * it dlopen()s /System/Library/MediaCapture/H9ISP.mediacapture (this dylib, shown there by hosted patch 0056) and
 * hands H9ISPCaptureDeviceCreate to BWFigCaptureDeviceVendor, which calls it for every device it makes
 * (-[BWFigCaptureDeviceVendor _createDevice:reason:clientPID:figCaptureDevice:]: x0 the allocator, x1 the options,
 * x2 where the device goes; read in the 24A446 cache). No method of CMCapture is replaced.
 *
 * The objects are CoreMedia derived objects. Their layout, read in H16ISP.mediacapture of the 24A446 cache
 * (kH16ISPCaptureDeviceBaseClass, kH16ISPCaptureDeviceClass, kH16ISPCaptureStreamBaseClass, kH16ISPCaptureStreamClass),
 * is the one HakiOS used on iOS 16.5:
 *   base    version 1, storage size, equal, invalidate, finalize, copyDebugDescription, copyProperty, setProperty, ...
 *   device  version 3: ProcessingSessionCreate, RequestStreams, RelinquishStreams, PerformPearlCalibration
 *   stream  version 2: Start, Stop, AddAttachmentsToSampleBuffer
 * The property names are HakiOS's (iOS 16); which of them iOS 27 asks and which it asks that are not answered is
 * written to the log once per name ("ONEIOS CAMERA: ios: ... no answer for ..."): that list is the next step.
 *
 * A frame: the kernel's newest (NV12, at most 1920x1080, BT.601 full range) is scaled into a buffer of the pool
 * the graph gave each output (VideoOutputConfigurations' BufferPool) and handed to that output's
 * PixelBufferHandler with the host time of its arrival. 420f, 420v and BGRA buffers are filled; others are said once
 * and skipped.
 *
 * NOT RUN in iOS. Built for arm64e-apple-ios27.0 with Xcode, and run on the Mac against macOS 27's own CMCapture
 * (tests/host-mac.sh: BWFigCaptureDeviceVendor makes our device and the streams answer).
 */
#include <CoreFoundation/CoreFoundation.h>
#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "OneiOSCamera.h"
#include "oneios_camera_proto.h"
#include "oneios_camera_reader.h"

/* ---- CoreMedia's derived objects, as CMCapture 27 lays them out (see the top) ---- */
typedef struct OpaqueCMBaseObject *CMBaseObjRef;
typedef const void *CMBaseClassIDRef;

struct base_class {
	uint64_t version;
	uint64_t storage_size;
	Boolean (*equal)(CMBaseObjRef, CMBaseObjRef);
	OSStatus (*invalidate)(CMBaseObjRef);
	void (*finalize)(CMBaseObjRef);
	CFStringRef (*copy_debug_description)(CMBaseObjRef);
	OSStatus (*copy_property)(CMBaseObjRef, CFStringRef, CFAllocatorRef, void *);
	OSStatus (*set_property)(CMBaseObjRef, CFStringRef, CFTypeRef);
	OSStatus (*notification_barrier)(CMBaseObjRef);
	const void *reserved[3];
};
struct device_class {
	uint64_t version;               /* 3 */
	void *processing_session_create;
	OSStatus (*request_streams)(CMBaseObjRef, CFArrayRef, CFDictionaryRef);
	OSStatus (*relinquish_streams)(CMBaseObjRef, CFArrayRef, CFDictionaryRef);
	void *pearl_calibration;
	void *reserved[3];
};
struct stream_class {
	uint64_t version;               /* 2 */
	OSStatus (*start)(CMBaseObjRef);
	OSStatus (*stop)(CMBaseObjRef);
	OSStatus (*add_attachments)(CMBaseObjRef, CMSampleBufferRef, CFDictionaryRef);
	void *reserved[2];
};
struct vtable {
	const void *reserved;
	const struct base_class *base;
	const void *derived;
};

#define ERR_PARAM               (-12780)
#define ERR_UNSUPPORTED         (-12782)
#define ERR_NOT_AVAILABLE       (-12783)

static OSStatus (*derived_create)(CFAllocatorRef, const struct vtable *, CMBaseClassIDRef, CMBaseObjRef *);
static void *(*derived_storage)(CMBaseObjRef);
static CMBaseClassIDRef (*device_class_id)(void);
static CMBaseClassIDRef (*stream_class_id)(void);
static CFStringRef port_back, port_front;
static CFStringRef md_attachment;       /* kFigCaptureSampleBufferAttachmentKey_MetadataDictionary */
static CFStringRef md_port;             /* kFigCaptureStreamMetadata_PortType */

static CFStringRef
symbol_string(const char *name)
{
	CFStringRef *p = dlsym(RTLD_DEFAULT, name);

	return p != NULL ? *p : NULL;
}

static bool
find_functions(void)
{
	derived_create = dlsym(RTLD_DEFAULT, "CMDerivedObjectCreate");
	derived_storage = dlsym(RTLD_DEFAULT, "CMBaseObjectGetDerivedStorage");
	device_class_id = dlsym(RTLD_DEFAULT, "FigCaptureDeviceGetClassID");
	stream_class_id = dlsym(RTLD_DEFAULT, "FigCaptureStreamGetClassID");
	port_back = symbol_string("kFigCapturePortType_BackFacingCamera");
	port_front = symbol_string("kFigCapturePortType_FrontFacingCamera");
	md_attachment = symbol_string("kFigCaptureSampleBufferAttachmentKey_MetadataDictionary");
	md_port = symbol_string("kFigCaptureStreamMetadata_PortType");
	if (port_back == NULL) {
		port_back = CFSTR("PortTypeBack");
	}
	if (port_front == NULL) {
		port_front = CFSTR("PortTypeFront");
	}
	oneios_camera_say("CoreMedia: CMDerivedObjectCreate %s, FigCaptureDeviceGetClassID %s, FigCaptureStreamGetClassID %s, "
	    "metadata key %s", derived_create ? "found" : "MISSING", device_class_id ? "found" : "MISSING",
	    stream_class_id ? "found" : "MISSING", md_attachment ? "found" : "missing");
	return derived_create && derived_storage && device_class_id && stream_class_id;
}

/* ---- small helpers ---- */
static CFNumberRef
number_i(int64_t v)
{
	return CFNumberCreate(NULL, kCFNumberSInt64Type, &v);
}

static CFNumberRef
number_f(double v)
{
	return CFNumberCreate(NULL, kCFNumberDoubleType, &v);
}

static CFMutableDictionaryRef
new_dict(void)
{
	return CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
}

/* Puts `value` under `key` and releases it. */
static void
put(CFMutableDictionaryRef d, const char *key, CFTypeRef value)
{
	CFStringRef k = CFStringCreateWithCString(NULL, key, kCFStringEncodingUTF8);

	if (value != NULL) {
		CFDictionarySetValue(d, k, value);
		CFRelease(value);
	}
	CFRelease(k);
}

static bool
key_is(CFStringRef key, const char *name)
{
	char buffer[160];

	return key != NULL && CFGetTypeID(key) == CFStringGetTypeID() &&
	    CFStringGetCString(key, buffer, sizeof(buffer), kCFStringEncodingUTF8) && strcmp(buffer, name) == 0;
}

static double
number_of(CFTypeRef v, double otherwise)
{
	double n = otherwise;

	if (v != NULL && CFGetTypeID(v) == CFNumberGetTypeID()) {
		CFNumberGetValue(v, kCFNumberDoubleType, &n);
	} else if (v != NULL && CFGetTypeID(v) == CFBooleanGetTypeID()) {
		n = CFBooleanGetValue(v);
	}
	return n;
}

static CFMutableSetRef said_names;
static pthread_mutex_t said_lock = PTHREAD_MUTEX_INITIALIZER;

/* Says `what key = value` the first time this pair of what and key comes. */
static void
say_once(const char *what, CFStringRef key, CFTypeRef value)
{
	CFStringRef tag = CFStringCreateWithFormat(NULL, NULL, CFSTR("%s %@"), what, key);
	char name[160] = "?", text[240] = "";
	bool first;

	pthread_mutex_lock(&said_lock);
	if (said_names == NULL) {
		said_names = CFSetCreateMutable(NULL, 0, &kCFTypeSetCallBacks);
	}
	first = !CFSetContainsValue(said_names, tag);
	CFSetAddValue(said_names, tag);
	pthread_mutex_unlock(&said_lock);
	CFRelease(tag);
	if (!first) {
		return;
	}
	if (key != NULL) {
		CFStringGetCString(key, name, sizeof(name), kCFStringEncodingUTF8);
	}
	if (value != NULL) {
		CFStringRef d = CFCopyDescription(value);

		if (d != NULL) {
			CFStringGetCString(d, text, sizeof(text), kCFStringEncodingUTF8);
			CFRelease(d);
		}
		for (char *c = text; *c; c++) {
			*c = *c == '\n' ? ' ' : *c;
		}
	}
	oneios_camera_say("%s %s%s%s", what, name, value ? " = " : "", text);
}

/* ---- the properties each object lists (SupportedPropertiesDictionary: { name: { PropertyType, ReadWriteStatus } }) ---- */
struct prop {
	const char *name, *type, *access;
};

static const struct prop stream_props[] = {
	{ "PortType", "String", "ReadOnly" }, { "UniqueID", "String", "ReadOnly" }, { "NonLocalizedName", "String", "ReadOnly" },
	{ "SupportedFormatsArray", "Collection", "ReadOnly" }, { "FocalLength", "Number", "ReadOnly" },
	{ "LensFNumber", "Number", "ReadOnly" }, { "PixelSize", "Number", "ReadOnly" },
	{ "MaxZoomFactor", "Number", "ReadOnly" }, { "SupportedColorSpaces", "Collection", "ReadOnly" },
	{ "HasAutoFocusPositionSensor", "Boolean", "ReadOnly" }, { "Clock", "Clock", "ReadOnly" },
	{ "FormatIndex", "Number", "ReadWrite" }, { "MinimumFrameRate", "Number", "ReadWrite" }, { "MaximumFrameRate", "Number", "ReadWrite" },
	{ "MaximumAllowedFrameRate", "Number", "ReadWrite" }, { "VideoOutputConfigurations", "Collection", "ReadWrite" },
	{ "VideoOutputsEnabled", "Collection", "ReadWrite" }, { "VideoOutputHandlers", "Collection", "WriteOnly" },
	{ "DeferAdditionOfAttachments", "Boolean", "ReadWrite" }, { "TorchLevel", "Number", "ReadWrite" },
	{ "HighCurrentTorchEnabled", "Boolean", "ReadWrite" }, { "ColorSpace", "Number", "ReadWrite" }, { "ZoomFactor", "Number", "ReadWrite" },
	{ "ExposureBias", "Number", "ReadWrite" }, { "DeviceOrientation", "Number", "ReadWrite" }, { "FlashMode", "Number", "ReadWrite" },
	{ "StillImageCaptureNow", "Collection", "WriteOnly" }, { "LockAENow", "Boolean", "WriteOnly" }, { "UnlockAENow", "Boolean", "WriteOnly" },
	{ "LockAWBNow", "Boolean", "WriteOnly" }, { "UnlockAWBNow", "Boolean", "WriteOnly" }, { "MaxIntegrationTime", "Number", "ReadWrite" },
	{ "AEMaxGain", "Number", "ReadWrite" }, { "AEMinGain", "Number", "ReadWrite" }, { "FaceDetectionEnabled", "Boolean", "ReadWrite" },
	{ "VideoRecordingInProgress", "Boolean", "ReadWrite" }, { "MemoryPoolID", "Number", "ReadWrite" },
	{ "ChromaNoiseReductionEnabled", "Boolean", "ReadWrite" }, { "TemporalNoiseReductionEnabled", "Boolean", "ReadWrite" },
	{ "MultipleOutputSupportEnabled", "Boolean", "ReadWrite" }, { "VideoOutputsColorInfo", "Collection", "ReadWrite" },
	{ "RegionOfInterestForCameraControls", "Collection", "ReadWrite" }, { "AEMeteringMode", "Number", "ReadWrite" },
	{ "MaxFrameCoalescingDuration", "Number", "ReadWrite" }, { "LimitAERange", "Boolean", "ReadWrite" },
	{ "TimeMachineSuspendNow", "Boolean", "WriteOnly" }, { "TimeMachineResumeNow", "Boolean", "WriteOnly" },
	{ "AutoFocusNow", "Collection", "WriteOnly" }, { "ContinuousAutoFocusNow", "Collection", "WriteOnly" },
	{ "FocusNow", "Collection", "WriteOnly" }, { "LockAFNow", "Boolean", "WriteOnly" }, { "AFWindowParams", "Collection", "ReadWrite" },
	{ "AutoFocusMode", "Number", "ReadWrite" },
}, device_props[] = {
	{ "StreamArray", "Collection", "ReadOnly" }, { "Clock", "Clock", "ReadOnly" }, { "ClientIsActive", "Boolean", "ReadWrite" },
	{ "SupportedSynchronizedStreamsGroups", "Collection", "ReadOnly" }, { "MemoryPoolID", "Number", "ReadWrite" },
};

static CFDictionaryRef
supported(const struct prop *p, size_t n)
{
	CFMutableDictionaryRef all = new_dict();

	for (size_t i = 0; i < n; i++) {
		CFMutableDictionaryRef one = new_dict();

		put(one, "PropertyType", CFStringCreateWithCString(NULL, p[i].type, kCFStringEncodingUTF8));
		put(one, "ReadWriteStatus", CFStringCreateWithCString(NULL, p[i].access, kCFStringEncodingUTF8));
		put(all, p[i].name, one);
	}
	return all;
}

/* ---- the cameras ---- */
#define PF_420F 0x34323066u     /* '420f' */
#define PF_420V 0x34323076u     /* '420v' */
#define PF_BGRA 0x42475241u     /* 'BGRA' */
#define MAX_STREAMS 2
#define OUTPUTS 3
#define HELD 3                  /* pictures kept after they are given, so the pool hands out others (HakiOS) */
static const char *const output_names[OUTPUTS] = { "PrimaryScaler", "SecondaryScaler", "StillImage" };

struct camera {
	uint32_t role;                  /* ONEIOS_CAMERA_WIDE, ONEIOS_CAMERA_FRONT */
	const char *name;               /* the device's uniqueName in AVCaptureSession.plist */
	const char *unique_id;
	bool front;
	double focal_length, f_number, pixel_size;
};

/* The back wide camera and the front camera, named as the iPhone's are in its plist. */
static const struct camera known[MAX_STREAMS] = {
	{ ONEIOS_CAMERA_WIDE, "Back Camera", "com.apple.avfoundation.avcapturedevice.built-in_video:0", false, 6.3, 1.7, 1.0 },
	{ ONEIOS_CAMERA_FRONT, "Front Camera", "com.apple.avfoundation.avcapturedevice.built-in_video:1", true, 3.3, 2.2, 1.0 },
};

#ifndef ONEIOS_CAMERA_SESSION_PLIST
#define ONEIOS_CAMERA_SESSION_PLIST "/System/Library/PrivateFrameworks/CMCapture.framework/V54/AVCaptureSession.plist"
#endif

static CFPropertyListRef session_plist;

static CFPropertyListRef
read_plist(const char *path)
{
	CFStringRef s = CFStringCreateWithCString(NULL, path, kCFStringEncodingUTF8);
	CFURLRef url = CFURLCreateWithFileSystemPath(NULL, s, kCFURLPOSIXPathStyle, false);
	CFReadStreamRef in = url ? CFReadStreamCreateWithFile(NULL, url) : NULL;
	CFPropertyListRef p = NULL;

	if (in != NULL && CFReadStreamOpen(in)) {
		p = CFPropertyListCreateWithStream(NULL, in, 0, kCFPropertyListImmutable, NULL, NULL);
		CFReadStreamClose(in);
	}
	if (in) {
		CFRelease(in);
	}
	if (url) {
		CFRelease(url);
	}
	CFRelease(s);
	return p;
}

/* The plist's FigCaptureStreamFormats of the device with this uniqueName (or the first back/front one), or NULL. */
static CFDictionaryRef
plist_stream_formats(const struct camera *c)
{
	CFArrayRef devices;

	if (session_plist == NULL || CFGetTypeID(session_plist) != CFDictionaryGetTypeID()) {
		return NULL;
	}
	devices = CFDictionaryGetValue(session_plist, CFSTR("AVCaptureDevices"));
	if (devices == NULL || CFGetTypeID(devices) != CFArrayGetTypeID()) {
		return NULL;
	}
	for (int pass = 0; pass < 2; pass++) {
		for (CFIndex i = 0; i < CFArrayGetCount(devices); i++) {
			CFDictionaryRef d = CFArrayGetValueAtIndex(devices, i), ms;
			CFStringRef name;
			CFDictionaryRef f;
			char text[80] = "";

			if (CFGetTypeID(d) != CFDictionaryGetTypeID() || (name = CFDictionaryGetValue(d, CFSTR("uniqueName"))) == NULL ||
			    CFGetTypeID(name) != CFStringGetTypeID() || (ms = CFDictionaryGetValue(d, CFSTR("MultiStream"))) == NULL ||
			    CFGetTypeID(ms) != CFDictionaryGetTypeID() || (f = CFDictionaryGetValue(ms, CFSTR("FigCaptureStreamFormats"))) == NULL ||
			    CFGetTypeID(f) != CFDictionaryGetTypeID() || CFDictionaryGetCount(f) == 0) {
				continue;
			}
			CFStringGetCString(name, text, sizeof(text), kCFStringEncodingUTF8);
			if (pass == 0 ? strcmp(text, c->name) == 0 : strncmp(text, c->front ? "Front" : "Back", c->front ? 5 : 4) == 0) {
				return f;
			}
		}
	}
	return NULL;
}

static CFDictionaryRef
output_info(int width, int height)
{
	CFMutableDictionaryRef o = new_dict();
	CFNumberRef pf[3] = { number_i(PF_420F), number_i(PF_420V), number_i(PF_BGRA) };

	put(o, "SupportedPixelFormats", CFArrayCreate(NULL, (const void **)pf, 3, &kCFTypeArrayCallBacks));
	for (int i = 0; i < 3; i++) {
		CFRelease(pf[i]);
	}
	put(o, "NativeWidth", number_i(width));
	put(o, "NativeHeight", number_i(height));
	put(o, "MaxDownscaleFactor", number_f(32));
	put(o, "MaxScaleFactor", number_f(1));
	put(o, "MinScaleFactor", number_f(1.0 / 32));
	put(o, "OutputWidthAlignmentRequirement", number_i(16));
	return o;
}

static void
copy_entry(const void *key, const void *value, void *context)
{
	CFDictionarySetValue(context, key, value);
}

/* One stream format: the plist's entry (all its keys kept), with the keys a stream's own format carries. */
static CFDictionaryRef
format(CFStringRef name, CFDictionaryRef entry)
{
	CFMutableDictionaryRef f = new_dict(), outputs = new_dict();
	int w = (int)number_of(entry ? CFDictionaryGetValue(entry, CFSTR("SensorWidth")) : NULL, 1920);
	int h = (int)number_of(entry ? CFDictionaryGetValue(entry, CFSTR("SensorHeight")) : NULL, 1080);
	double fps = number_of(entry ? CFDictionaryGetValue(entry, CFSTR("MaxFrameRate")) : NULL, 30);
	bool binned = number_of(entry ? CFDictionaryGetValue(entry, CFSTR("Binned")) : NULL, 0) != 0;
	CMVideoFormatDescriptionRef fd = NULL;

	if (entry != NULL) {
		CFDictionaryApplyFunction(entry, copy_entry, f);
	}
	if (name != NULL) {
		CFDictionarySetValue(f, CFSTR("FormatName"), name);
	}
	if (CMVideoFormatDescriptionCreate(NULL, PF_420F, w, h, NULL, &fd) == 0 && fd != NULL) {
		put(f, "FormatDescription", fd);
	}
	put(f, "VideoMinFrameRate", number_f(2));
	put(f, "VideoMaxFrameRate", number_f(fps));
	put(f, "VideoIsBinned", CFRetain(binned ? kCFBooleanTrue : kCFBooleanFalse));
	put(f, "VideoBinningFactorHorizontal", number_i(binned ? 2 : 1));
	put(f, "VideoBinningFactorVertical", number_i(binned ? 2 : 1));
	put(f, "VideoFieldOfView", number_f(78));
	put(f, "VideoMinIntegrationTime", number_i(100));
	put(f, "VideoMaxIntegrationTime", number_i(1000000 / 15));
	put(f, "VideoRawBitDepth", number_i(10));
	put(f, "VideoMaxWidth", number_i(w));
	put(f, "VideoMaxHeight", number_i(h));
	put(f, "Experimental", CFRetain(kCFBooleanFalse));
	put(f, "SensorWidth", number_i(w));
	put(f, "SensorHeight", number_i(h));
	put(f, "ISOBase", number_f(32));
	put(f, "DefaultAEMaxGain", number_f(64));
	put(f, "SensorCropWidth", number_i(w));
	put(f, "SensorCropHeight", number_i(h));
	put(f, "PreGDCSensorCropWidth", number_i(w));
	put(f, "PreGDCSensorCropHeight", number_i(h));
	put(f, "PostGDCCropWidth", number_i(w));
	put(f, "PostGDCCropHeight", number_i(h));
	put(f, "GeometricDistortionCorrectedNativeWidth", number_i(w));
	put(f, "GeometricDistortionCorrectedNativeHeight", number_i(h));
	put(outputs, "PrimaryScaler", output_info(w / 2, h / 2));
	put(outputs, "SecondaryScaler", output_info(w / 2, h / 2));
	put(outputs, "StillImage", output_info(w, h));
	put(f, "SupportedOutputs", outputs);
	return f;
}

static CFComparisonResult
compare_names(const void *a, const void *b, void *context)
{
	(void)context;
	return CFStringCompare(a, b, 0);
}

/* The stream's formats: one for each of the plist's stream formats of this camera (so that every AVCaptureDevice
 * format of the plist resolves to one), sorted by name; without the plist, 1920x1080 at 30. */
static CFArrayRef
formats_for(const struct camera *c)
{
	CFDictionaryRef list = plist_stream_formats(c);
	CFMutableArrayRef out = CFArrayCreateMutable(NULL, 0, &kCFTypeArrayCallBacks);

	if (list != NULL) {
		CFIndex n = CFDictionaryGetCount(list);
		const void **keys = calloc((size_t)n, sizeof(void *)), **values = calloc((size_t)n, sizeof(void *));
		CFMutableArrayRef names = CFArrayCreateMutable(NULL, n, &kCFTypeArrayCallBacks);

		CFDictionaryGetKeysAndValues(list, keys, values);
		for (CFIndex i = 0; i < n; i++) {
			CFArrayAppendValue(names, keys[i]);
		}
		CFArraySortValues(names, CFRangeMake(0, n), compare_names, NULL);
		for (CFIndex i = 0; i < n; i++) {
			CFStringRef name = CFArrayGetValueAtIndex(names, i);
			CFDictionaryRef entry = CFDictionaryGetValue(list, name), f;

			if (CFGetTypeID(entry) != CFDictionaryGetTypeID()) {
				continue;
			}
			f = format(name, entry);
			CFArrayAppendValue(out, f);
			CFRelease(f);
		}
		CFRelease(names);
		free(keys);
		free(values);
	}
	if (CFArrayGetCount(out) == 0) {
		CFDictionaryRef f = format(NULL, NULL);

		CFArrayAppendValue(out, f);
		CFRelease(f);
	}
	return out;
}

/* ---- the stream ---- */
typedef void (^pixel_buffer_handler)(CVPixelBufferRef, CMTime);

struct output {
	const char *name;
	CFDictionaryRef config;         /* from VideoOutputConfigurations */
	bool enabled;
	void *handler;                  /* a block ^(CVPixelBufferRef, CMTime) */
	uint64_t delivered, refused;
	bool said_pool, said_format;
	CVPixelBufferRef held[HELD];
	unsigned held_next;
};

struct stream {
	const struct camera *camera;
	CFStringRef port;
	CFArrayRef formats;
	CFDictionaryRef supported, colour_info;
	int format_index;
	double min_fps, max_fps, zoom;
	unsigned stills_wanted;
	struct output out[OUTPUTS];
	pthread_mutex_t lock;
	pthread_t worker;
	volatile bool running, graph_running;
	bool worker_started;
	uint64_t frames;
};

static CMBaseObjRef all_streams[MAX_STREAMS];

static struct stream *
stream_of(CMBaseObjRef o)
{
	return derived_storage(o);
}

static int
output_index(CFTypeRef name)
{
	for (int i = 0; i < OUTPUTS; i++) {
		if (key_is(name, output_names[i])) {
			return i;
		}
	}
	return -1;
}

static void
let_go(struct output *out)
{
	for (unsigned i = 0; i < HELD; i++) {
		if (out->held[i] != NULL) {
			CVPixelBufferRelease(out->held[i]);
			out->held[i] = NULL;
		}
	}
}

static OSStatus
stream_copy_property(CMBaseObjRef o, CFStringRef key, CFAllocatorRef allocator, void *out)
{
	struct stream *s = stream_of(o);
	CFTypeRef value = NULL;

	(void)allocator;
	if (key_is(key, "PortType")) {
		value = CFRetain(s->port);
	} else if (key_is(key, "UniqueID") || key_is(key, "ModelID")) {
		value = CFStringCreateWithCString(NULL, s->camera->unique_id, kCFStringEncodingUTF8);
	} else if (key_is(key, "NonLocalizedName") || key_is(key, "LocalizedName")) {
		value = CFStringCreateWithCString(NULL, s->camera->name, kCFStringEncodingUTF8);
	} else if (key_is(key, "Manufacturer")) {
		value = CFRetain(CFSTR("Apple Inc."));
	} else if (key_is(key, "SupportedFormatsArray")) {
		value = CFRetain(s->formats);
	} else if (key_is(key, "FormatIndex")) {
		value = number_i(s->format_index);
	} else if (key_is(key, "MinimumFrameRate")) {
		value = number_f(s->min_fps);
	} else if (key_is(key, "MaximumFrameRate") || key_is(key, "MaximumAllowedFrameRate")) {
		value = number_f(s->max_fps);
	} else if (key_is(key, "Clock")) {
		value = CFRetain(CMClockGetHostTimeClock());
	} else if (key_is(key, "TorchLevel") || key_is(key, "ExposureBias")) {
		value = number_f(0);
	} else if (key_is(key, "VideoOutputConfigurations") || key_is(key, "VideoOutputsEnabled")) {
		/* Read back by the graph while it builds (HakiOS: a failed read tears the graph down). */
		bool configs = key_is(key, "VideoOutputConfigurations");
		CFMutableDictionaryRef d = new_dict();

		pthread_mutex_lock(&s->lock);
		for (int i = 0; i < OUTPUTS; i++) {
			CFStringRef name = CFStringCreateWithCString(NULL, output_names[i], kCFStringEncodingUTF8);

			if (configs && s->out[i].config != NULL) {
				CFDictionarySetValue(d, name, s->out[i].config);
			} else if (!configs) {
				CFDictionarySetValue(d, name, s->out[i].enabled ? kCFBooleanTrue : kCFBooleanFalse);
			}
			CFRelease(name);
		}
		pthread_mutex_unlock(&s->lock);
		value = d;
	} else if (key_is(key, "VideoOutputsColorInfo")) {
		pthread_mutex_lock(&s->lock);
		value = s->colour_info != NULL ? CFRetain(s->colour_info) : new_dict();
		pthread_mutex_unlock(&s->lock);
	} else if (key_is(key, "SupportedPropertiesDictionary")) {
		value = CFRetain(s->supported);
	} else if (key_is(key, "FocalLength")) {
		value = number_f(s->camera->focal_length);
	} else if (key_is(key, "LensFNumber")) {
		value = number_f(s->camera->f_number);
	} else if (key_is(key, "PixelSize")) {
		value = number_f(s->camera->pixel_size);     /* required by the graph (HakiOS) */
	} else if (key_is(key, "MaxZoomFactor")) {
		value = number_f(8);
	} else if (key_is(key, "ZoomFactor")) {
		value = number_f(s->zoom);
	} else if (key_is(key, "SupportedColorSpaces")) {
		CFNumberRef srgb = number_i(0);

		value = CFArrayCreate(NULL, (const void **)&srgb, 1, &kCFTypeArrayCallBacks);
		CFRelease(srgb);
	} else if (key_is(key, "ColorSpace") || key_is(key, "DeviceOrientation") || key_is(key, "FlashMode")) {
		value = number_i(0);
	} else if (key_is(key, "HasAutoFocusPositionSensor") || key_is(key, "HighCurrentTorchEnabled") ||
	    key_is(key, "DeferAdditionOfAttachments")) {
		value = CFRetain(kCFBooleanFalse);
	} else {
		say_once("stream: no answer for", key, NULL);
		return ERR_NOT_AVAILABLE;
	}
	*(CFTypeRef *)out = value;
	return 0;
}

static OSStatus
stream_set_property(CMBaseObjRef o, CFStringRef key, CFTypeRef value)
{
	struct stream *s = stream_of(o);

	if (key_is(key, "FormatIndex")) {
		int64_t i = (int64_t)number_of(value, -1);

		if (i < 0 || i >= CFArrayGetCount(s->formats)) {
			return ERR_PARAM;
		}
		s->format_index = (int)i;
		oneios_camera_say("stream %s: format %lld", s->camera->name, (long long)i);
		return 0;
	}
	if (key_is(key, "ZoomFactor")) {
		double zoom = number_of(value, 1);

		s->zoom = zoom < 1 ? 1 : zoom > 8 ? 8 : zoom;
		return 0;
	}
	if (key_is(key, "MinimumFrameRate") || key_is(key, "MaximumFrameRate")) {
		*(key_is(key, "MinimumFrameRate") ? &s->min_fps : &s->max_fps) = number_of(value, 30);
		say_once("stream: set", key, value);
		return 0;
	}
	if (key_is(key, "VideoOutputConfigurations") || key_is(key, "VideoOutputsEnabled") || key_is(key, "VideoOutputHandlers")) {
		bool configs = key_is(key, "VideoOutputConfigurations"), enabled = key_is(key, "VideoOutputsEnabled");
		CFIndex n;
		const void **keys, **values;

		say_once("stream: set", key, value);
		if (value == NULL || CFGetTypeID(value) != CFDictionaryGetTypeID()) {
			return 0;
		}
		n = CFDictionaryGetCount(value);
		keys = calloc((size_t)n + 1, sizeof(void *));
		values = calloc((size_t)n + 1, sizeof(void *));
		CFDictionaryGetKeysAndValues(value, keys, values);
		pthread_mutex_lock(&s->lock);
		for (CFIndex i = 0; i < n; i++) {
			int k = output_index(keys[i]);
			struct output *out;

			if (k < 0) {
				say_once("stream: an output not here:", keys[i], NULL);
				continue;
			}
			out = &s->out[k];
			if (configs) {
				if (out->config != NULL) {
					CFRelease(out->config);
				}
				let_go(out);
				out->config = CFGetTypeID(values[i]) == CFDictionaryGetTypeID() ? CFRetain(values[i]) : NULL;
			} else if (enabled) {
				out->enabled = number_of(values[i], 1) != 0;
			} else {
				CFTypeRef h = CFGetTypeID(values[i]) == CFDictionaryGetTypeID() ?
				    CFDictionaryGetValue(values[i], CFSTR("PixelBufferHandler")) : NULL;

				if (out->handler != NULL) {
					CFRelease(out->handler);
				}
				out->handler = h != NULL ? (void *)CFRetain(h) : NULL;
			}
		}
		pthread_mutex_unlock(&s->lock);
		free(keys);
		free(values);
		return 0;
	}
	if (key_is(key, "StillImageCaptureNow")) {
		pthread_mutex_lock(&s->lock);
		s->stills_wanted++;
		pthread_mutex_unlock(&s->lock);
		say_once("stream: a photograph asked for", key, value);
		return 0;
	}
	if (key_is(key, "VideoOutputsColorInfo")) {
		pthread_mutex_lock(&s->lock);
		if (s->colour_info != NULL) {
			CFRelease(s->colour_info);
		}
		s->colour_info = value != NULL && CFGetTypeID(value) == CFDictionaryGetTypeID() ? CFRetain(value) : NULL;
		pthread_mutex_unlock(&s->lock);
		return 0;
	}
	say_once("stream: set (taken, not used)", key, value);
	return 0;
}

/* ---- the frames ---- */
static CVPixelBufferRef
output_buffer(struct output *out)
{
	CFTypeRef value = out->config != NULL ? CFDictionaryGetValue(out->config, CFSTR("BufferPool")) : NULL;
	CVPixelBufferRef pb = NULL;

	if (value == NULL || CFGetTypeID(value) != CVPixelBufferPoolGetTypeID()) {
		if (!out->said_pool) {
			out->said_pool = true;
			oneios_camera_say("%s: no pool of the graph's in the configuration: no pictures", out->name);
		}
		return NULL;
	}
	for (unsigned i = 0; CVPixelBufferPoolCreatePixelBuffer(NULL, (CVPixelBufferPoolRef)value, &pb) != kCVReturnSuccess && i < HELD; i++) {
		unsigned oldest = (out->held_next + i) % HELD;

		if (out->held[oldest] != NULL) {
			CVPixelBufferRelease(out->held[oldest]);
			out->held[oldest] = NULL;
		}
	}
	if (pb == NULL && out->refused++ % 300 == 0) {
		oneios_camera_say("%s: the graph's pool has no buffer to spare (%llu frames so)", out->name,
		    (unsigned long long)out->refused);
	}
	return pb;
}

static inline uint8_t
clamp8(int v)
{
	return (uint8_t)(v < 0 ? 0 : v > 255 ? 255 : v);
}

int
oneios_camera_scale_nv12(const struct oneios_camera_frame *h, const uint8_t *picture, uint8_t *y, size_t y_stride, uint8_t *cbcr,
    size_t cbcr_stride, uint32_t width, uint32_t height, bool video_range)
{
	const uint8_t *src_c = picture + (size_t)h->stride * h->height;

	if (width < 2 || height < 2 || y_stride < width || cbcr_stride < width) {
		return -EINVAL;
	}
	for (uint32_t row = 0; row < height; row++) {
		const uint8_t *src = picture + (size_t)(row * h->height / height) * h->stride;
		uint8_t *dst = y + row * y_stride;

		for (uint32_t x = 0; x < width; x++) {
			int v = src[x * h->width / width];

			dst[x] = video_range ? (uint8_t)(16 + v * 219 / 255) : (uint8_t)v;
		}
	}
	for (uint32_t row = 0; row < height / 2; row++) {
		const uint8_t *src = src_c + (size_t)(row * (h->height / 2) / (height / 2)) * h->stride;
		uint8_t *dst = cbcr + row * cbcr_stride;

		for (uint32_t x = 0; x < width / 2; x++) {
			uint32_t sx = x * (h->width / 2) / (width / 2);
			int cb = src[2 * sx], cr = src[2 * sx + 1];

			dst[2 * x] = video_range ? (uint8_t)(16 + cb * 224 / 255) : (uint8_t)cb;
			dst[2 * x + 1] = video_range ? (uint8_t)(16 + cr * 224 / 255) : (uint8_t)cr;
		}
	}
	return 0;
}

int
oneios_camera_scale_bgra(const struct oneios_camera_frame *h, const uint8_t *picture, uint8_t *bgra, size_t stride, uint32_t width,
    uint32_t height)
{
	const uint8_t *src_c = picture + (size_t)h->stride * h->height;

	if (width < 1 || height < 1 || stride < (size_t)width * 4) {
		return -EINVAL;
	}
	for (uint32_t row = 0; row < height; row++) {
		uint32_t sy = row * h->height / height;
		const uint8_t *ys = picture + (size_t)sy * h->stride, *cs = src_c + (size_t)(sy / 2) * h->stride;
		uint8_t *dst = bgra + row * stride;

		for (uint32_t x = 0; x < width; x++) {
			uint32_t sx = x * h->width / width;
			int Y = ys[sx], cb = cs[sx & ~1u] - 128, cr = cs[(sx & ~1u) + 1] - 128;     /* BT.601 full range */

			dst[4 * x + 0] = clamp8(Y + (1772 * cb) / 1000);
			dst[4 * x + 1] = clamp8(Y - (344 * cb + 714 * cr) / 1000);
			dst[4 * x + 2] = clamp8(Y + (1402 * cr) / 1000);
			dst[4 * x + 3] = 255;
		}
	}
	return 0;
}

static void
deliver(struct stream *s, struct output *out, const struct oneios_camera_frame *f, const uint8_t *picture)
{
	CVPixelBufferRef pb = output_buffer(out);
	uint32_t pf, width, height;
	int e = -EINVAL;

	if (pb == NULL) {
		return;
	}
	pf = CVPixelBufferGetPixelFormatType(pb);
	width = (uint32_t)CVPixelBufferGetWidth(pb);
	height = (uint32_t)CVPixelBufferGetHeight(pb);
	if (CVPixelBufferLockBaseAddress(pb, 0) == kCVReturnSuccess) {
		if ((pf == PF_420F || pf == PF_420V) && CVPixelBufferGetPlaneCount(pb) == 2) {
			e = oneios_camera_scale_nv12(f, picture, CVPixelBufferGetBaseAddressOfPlane(pb, 0), CVPixelBufferGetBytesPerRowOfPlane(pb, 0),
			    CVPixelBufferGetBaseAddressOfPlane(pb, 1), CVPixelBufferGetBytesPerRowOfPlane(pb, 1), width, height, pf == PF_420V);
		} else if (pf == PF_BGRA) {
			e = oneios_camera_scale_bgra(f, picture, CVPixelBufferGetBaseAddress(pb), CVPixelBufferGetBytesPerRow(pb), width, height);
		}
		CVPixelBufferUnlockBaseAddress(pb, 0);
	}
	if (e != 0) {
		if (!out->said_format) {
			out->said_format = true;
			oneios_camera_say("%s: %ux%u '%c%c%c%c' buffers are not filled here", out->name, width, height, (char)(pf >> 24),
			    (char)(pf >> 16), (char)(pf >> 8), (char)pf);
		}
		CVPixelBufferRelease(pb);
		return;
	}
	CVBufferSetAttachment(pb, kCVImageBufferYCbCrMatrixKey, kCVImageBufferYCbCrMatrix_ITU_R_601_4, kCVAttachmentMode_ShouldPropagate);
	CVBufferSetAttachment(pb, kCVImageBufferColorPrimariesKey, kCVImageBufferColorPrimaries_ITU_R_709_2, kCVAttachmentMode_ShouldPropagate);
	CVBufferSetAttachment(pb, kCVImageBufferTransferFunctionKey, kCVImageBufferTransferFunction_ITU_R_709_2, kCVAttachmentMode_ShouldPropagate);
	if (md_attachment != NULL) {
		CFMutableDictionaryRef m = new_dict();

		if (md_port != NULL) {
			CFDictionarySetValue(m, md_port, s->port);
		}
		CVBufferSetAttachment(pb, md_attachment, m, kCVAttachmentMode_ShouldPropagate);
		CFRelease(m);
	}
	((pixel_buffer_handler)out->handler)(pb, CMClockMakeHostTimeFromSystemUnits(f->time));
	if (strcmp(out->name, "StillImage") != 0) {
		if (out->held[out->held_next] != NULL) {
			CVPixelBufferRelease(out->held[out->held_next]);
		}
		out->held[out->held_next] = pb;
		out->held_next = (out->held_next + 1) % HELD;
	} else {
		CVPixelBufferRelease(pb);
	}
	if (out->delivered++ == 0) {
		oneios_camera_say("%s %s: the first picture given (%ux%u '%c%c%c%c' from %ux%u)", s->camera->name, out->name, width, height,
		    (char)(pf >> 24), (char)(pf >> 16), (char)(pf >> 8), (char)pf, f->width, f->height);
	}
}

static void *
worker(void *context)
{
	CMBaseObjRef o = context;
	struct stream *s = stream_of(o);
	oneios_camera_reader reader;
	unsigned quiet = 0;
	int e;

	pthread_setname_np("oneios-camera");
	if ((e = oneios_camera_reader_open(&reader, NULL)) != 0 ||
	    (e = oneios_camera_reader_select(&reader, s->camera->role, 1920, 1080, 30)) != 0) {
		oneios_camera_say("stream %s: the kernel's camera: %s", s->camera->name, strerror(-e));
		if (reader.scratch) {
			oneios_camera_reader_close(&reader);
		}
		s->running = false;
		return NULL;
	}
	oneios_camera_say("stream %s: reading frames through %s", s->camera->name, reader.connect ? "AppleH9CamIn" : ONEIOS_CAMERA_DEVICE);
	while (s->running) {
		struct oneios_camera_frame header;
		const uint8_t *picture;

		if ((e = oneios_camera_reader_next(&reader, &header, &picture)) != 0) {
			if (quiet++ % 30 == 0) {
				oneios_camera_say("stream %s: no frame: %s", s->camera->name, strerror(-e));
			}
			usleep(e == -EAGAIN ? 10000 : 100000);       /* the kernel's EAGAIN comes after a 1 s wait */
			continue;
		}
		quiet = 0;
		if (s->frames++ == 0) {
			oneios_camera_say("stream %s: the first frame: %ux%u, camera %u, turned %u", s->camera->name, header.width, header.height,
			    header.sensor, header.orientation);
		}
		pthread_mutex_lock(&s->lock);
		for (int i = 0; s->graph_running && i < OUTPUTS; i++) {
			struct output *out = &s->out[i];
			bool still = i == 2;

			if (out->handler == NULL || out->config == NULL || !(out->enabled || i == 0) || (still && s->stills_wanted == 0)) {
				continue;
			}
			deliver(s, out, &header, picture);
			if (still) {
				s->stills_wanted--;
				oneios_camera_say("stream %s: the photograph is given", s->camera->name);
			}
		}
		pthread_mutex_unlock(&s->lock);
	}
	oneios_camera_reader_close(&reader);
	oneios_camera_say("stream %s: the frames stopped (%llu)", s->camera->name, (unsigned long long)s->frames);
	return NULL;
}

static OSStatus stream_stop(CMBaseObjRef o);

/* Frames are read from the reservation on (the camera takes a moment to open); given out once the graph starts. */
static OSStatus
stream_begin(CMBaseObjRef o, bool graph)
{
	struct stream *s = stream_of(o);

	if (graph) {
		pthread_mutex_lock(&s->lock);
		s->graph_running = true;
		pthread_mutex_unlock(&s->lock);
		oneios_camera_say("stream %s: the graph starts the stream (format %d)", s->camera->name, s->format_index);
	}
	if (s->running) {
		return 0;
	}
	if (s->worker_started) {
		pthread_join(s->worker, NULL);
		s->worker_started = false;
	}
	for (int i = 0; i < MAX_STREAMS; i++) {         /* one camera at a time: the kernel has one client */
		if (all_streams[i] != NULL && all_streams[i] != o && stream_of(all_streams[i])->running) {
			(void)stream_stop(all_streams[i]);
		}
	}
	s->running = true;
	s->frames = 0;
	if (pthread_create(&s->worker, NULL, worker, o) != 0) {
		s->running = false;
		return ERR_UNSUPPORTED;
	}
	s->worker_started = true;
	return 0;
}

static OSStatus
stream_start(CMBaseObjRef o)
{
	return stream_begin(o, true);
}

static OSStatus
stream_stop(CMBaseObjRef o)
{
	struct stream *s = stream_of(o);

	pthread_mutex_lock(&s->lock);
	s->graph_running = false;
	for (int i = 0; i < OUTPUTS; i++) {
		let_go(&s->out[i]);
	}
	pthread_mutex_unlock(&s->lock);
	s->running = false;
	if (s->worker_started) {
		pthread_join(s->worker, NULL);
		s->worker_started = false;
		oneios_camera_say("stream %s: stopped (%llu pictures given)", s->camera->name, (unsigned long long)s->out[0].delivered);
	}
	return 0;
}

static OSStatus
stream_add_attachments(CMBaseObjRef o, CMSampleBufferRef sbuf, CFDictionaryRef options)
{
	struct stream *s = stream_of(o);
	CVImageBufferRef image = sbuf != NULL ? CMSampleBufferGetImageBuffer(sbuf) : NULL;
	CFTypeRef ours = image != NULL && md_attachment != NULL ? CVBufferCopyAttachment(image, md_attachment, NULL) : NULL;

	(void)options;
	(void)s;
	if (ours != NULL) {
		if (CMGetAttachment(sbuf, md_attachment, NULL) == NULL) {
			CMSetAttachment(sbuf, md_attachment, ours, kCMAttachmentMode_ShouldPropagate);
		}
		CFRelease(ours);
	}
	return 0;
}

static OSStatus
stream_invalidate(CMBaseObjRef o)
{
	return stream_stop(o);
}

static void
stream_finalize(CMBaseObjRef o)
{
	struct stream *s = stream_of(o);

	(void)stream_stop(o);
	for (int i = 0; i < MAX_STREAMS; i++) {
		if (all_streams[i] == o) {
			all_streams[i] = NULL;
		}
	}
	for (int i = 0; i < OUTPUTS; i++) {
		if (s->out[i].config) {
			CFRelease(s->out[i].config);
		}
		if (s->out[i].handler) {
			CFRelease(s->out[i].handler);
		}
	}
	if (s->formats) {
		CFRelease(s->formats);
	}
	if (s->supported) {
		CFRelease(s->supported);
	}
	if (s->colour_info) {
		CFRelease(s->colour_info);
	}
	pthread_mutex_destroy(&s->lock);
}

static CFStringRef
stream_description(CMBaseObjRef o)
{
	return CFStringCreateWithFormat(NULL, NULL, CFSTR("<OneiOSCaptureStream: %s, the phone's camera %u>"), stream_of(o)->camera->name,
	    stream_of(o)->camera->role);
}

static const struct base_class stream_base = {
	1, sizeof(struct stream), NULL, stream_invalidate, stream_finalize, stream_description, stream_copy_property, stream_set_property,
	NULL, { NULL, NULL, NULL }
};
static const struct stream_class stream_derived = { 2, stream_start, stream_stop, stream_add_attachments, { NULL, NULL } };
static const struct vtable stream_vtable = { NULL, &stream_base, &stream_derived };

static CMBaseObjRef
stream_create(CFAllocatorRef allocator, const struct camera *camera)
{
	CMBaseObjRef o = NULL;
	struct stream *s;

	if (derived_create(allocator, &stream_vtable, stream_class_id(), &o) != 0 || o == NULL) {
		oneios_camera_say("the stream of %s cannot be made", camera->name);
		return NULL;
	}
	s = stream_of(o);
	memset(s, 0, sizeof(*s));
	pthread_mutex_init(&s->lock, NULL);
	s->camera = camera;
	s->port = camera->front ? port_front : port_back;
	s->min_fps = 2;
	s->max_fps = 30;
	s->zoom = 1;
	for (int i = 0; i < OUTPUTS; i++) {
		s->out[i].name = output_names[i];
	}
	s->formats = formats_for(camera);
	s->supported = supported(stream_props, sizeof(stream_props) / sizeof(stream_props[0]));
	return o;
}

/* ---- the device ---- */
struct device {
	CFArrayRef streams;
	CFDictionaryRef supported;
};

static OSStatus
device_copy_property(CMBaseObjRef o, CFStringRef key, CFAllocatorRef allocator, void *out)
{
	struct device *d = derived_storage(o);
	CFTypeRef value;

	(void)allocator;
	if (key_is(key, "StreamArray")) {
		value = CFRetain(d->streams);
	} else if (key_is(key, "Clock")) {
		value = CFRetain(CMClockGetHostTimeClock());
	} else if (key_is(key, "SupportedPropertiesDictionary")) {
		value = CFRetain(d->supported);
	} else if (key_is(key, "SupportedSynchronizedStreamsGroups")) {
		value = CFArrayCreate(NULL, NULL, 0, &kCFTypeArrayCallBacks);
	} else if (key_is(key, "ClientIsActive")) {
		value = CFRetain(kCFBooleanTrue);
	} else {
		say_once("device: no answer for", key, NULL);
		return ERR_NOT_AVAILABLE;
	}
	*(CFTypeRef *)out = value;
	return 0;
}

static OSStatus
device_set_property(CMBaseObjRef o, CFStringRef key, CFTypeRef value)
{
	(void)o;
	say_once("device: set (taken, not used)", key, value);
	return 0;
}

static OSStatus
device_request_streams(CMBaseObjRef o, CFArrayRef streams, CFDictionaryRef options)
{
	(void)o;
	(void)options;
	oneios_camera_say("device: streams asked for (%ld)", streams ? (long)CFArrayGetCount(streams) : 0L);
	/* The graph configures and reserves the streams here; HakiOS: frames begin with the reservation. */
	for (CFIndex i = 0; streams != NULL && i < CFArrayGetCount(streams); i++) {
		(void)stream_begin((CMBaseObjRef)CFArrayGetValueAtIndex(streams, i), false);
	}
	return 0;
}

static OSStatus
device_relinquish_streams(CMBaseObjRef o, CFArrayRef streams, CFDictionaryRef options)
{
	static unsigned said;

	(void)o;
	(void)options;
	if (said++ < 5) {
		oneios_camera_say("device: streams given back (%ld)", streams ? (long)CFArrayGetCount(streams) : 0L);
	}
	return 0;
}

static OSStatus
device_invalidate(CMBaseObjRef o)
{
	(void)o;
	return 0;
}

static void
device_finalize(CMBaseObjRef o)
{
	struct device *d = derived_storage(o);

	if (d->streams) {
		CFRelease(d->streams);
	}
	if (d->supported) {
		CFRelease(d->supported);
	}
}

static CFStringRef
device_description(CMBaseObjRef o)
{
	(void)o;
	return CFRetain(CFSTR("<OneiOSCaptureDevice: the Galaxy S24 Ultra's cameras through xnuhost>"));
}

static const struct base_class device_base = {
	1, sizeof(struct device), NULL, device_invalidate, device_finalize, device_description, device_copy_property, device_set_property,
	NULL, { NULL, NULL, NULL }
};
static const struct device_class device_derived = { 3, NULL, device_request_streams, device_relinquish_streams, NULL, { NULL, NULL, NULL } };
static const struct vtable device_vtable = { NULL, &device_base, &device_derived };

/* Which of the known cameras the kernel has: from its list, or (a reader without the list) the back one. */
static unsigned
cameras_there(const struct camera **out)
{
	oneios_camera_reader r;
	struct oc_camera list[OC_MAX_CAMERAS];
	unsigned n = 0, have = 0;
	int e = oneios_camera_reader_open(&r, NULL);

	if (e == 0) {
		have = oneios_camera_reader_cameras(&r, list, OC_MAX_CAMERAS);
		oneios_camera_reader_close(&r);
	} else if (e != -EBUSY) {
		oneios_camera_say("the kernel's camera: %s", strerror(-e));
		return 0;
	}
	for (unsigned k = 0; k < MAX_STREAMS; k++) {
		bool listed = have == 0 && known[k].role == ONEIOS_CAMERA_WIDE;

		for (unsigned i = 0; i < have; i++) {
			listed = listed || list[i].role == known[k].role;
		}
		if (listed) {
			out[n++] = &known[k];
		}
	}
	return n;
}

OSStatus H9ISPCaptureDeviceCreate(CFAllocatorRef allocator, CFDictionaryRef options, CMBaseObjRef *out);

__attribute__((visibility("default"))) OSStatus
H9ISPCaptureDeviceCreate(CFAllocatorRef allocator, CFDictionaryRef options, CMBaseObjRef *out)
{
	static bool usable;
	static unsigned calls;
	const struct camera *cams[MAX_STREAMS];
	CMBaseObjRef device = NULL, streams[MAX_STREAMS];
	struct device *d;
	unsigned n;

	if (out == NULL) {
		return ERR_PARAM;
	}
	*out = NULL;
	if (calls++ < 3) {
		say_once("device create: options", CFSTR("(the vendor's)"), options);
	}
	if (oneios_camera_off()) {
		return ERR_NOT_AVAILABLE;
	}
	if (!usable) {
		static bool tried;

		if (!tried) {
			tried = true;
			usable = find_functions();
			session_plist = read_plist(ONEIOS_CAMERA_SESSION_PLIST);
			oneios_camera_say("%s: %s", ONEIOS_CAMERA_SESSION_PLIST, session_plist ? "read" : "not read: formats of our own");
		}
		if (!usable) {
			return ERR_UNSUPPORTED;
		}
	}
	if ((n = cameras_there(cams)) == 0) {
		oneios_camera_say("no device: the kernel has no camera for iOS");
		return ERR_NOT_AVAILABLE;
	}
	for (unsigned i = 0; i < n; i++) {
		if ((streams[i] = stream_create(allocator, cams[i])) == NULL) {
			for (unsigned k = 0; k < i; k++) {
				CFRelease(streams[k]);
			}
			return ERR_UNSUPPORTED;
		}
	}
	if (derived_create(allocator, &device_vtable, device_class_id(), &device) != 0 || device == NULL) {
		for (unsigned k = 0; k < n; k++) {
			CFRelease(streams[k]);
		}
		oneios_camera_say("the device cannot be made");
		return ERR_UNSUPPORTED;
	}
	d = derived_storage(device);
	memset(d, 0, sizeof(*d));
	d->streams = CFArrayCreate(NULL, (const void **)streams, (CFIndex)n, &kCFTypeArrayCallBacks);
	d->supported = supported(device_props, sizeof(device_props) / sizeof(device_props[0]));
	for (unsigned i = 0; i < MAX_STREAMS; i++) {
		all_streams[i] = i < n ? streams[i] : NULL;
	}
	for (unsigned i = 0; i < n; i++) {
		oneios_camera_say("the device made: stream %u %s (%ld formats)", i, cams[i]->name, (long)CFArrayGetCount(stream_of(streams[i])->formats));
		CFRelease(streams[i]);
	}
	*out = device;
	return 0;
}
