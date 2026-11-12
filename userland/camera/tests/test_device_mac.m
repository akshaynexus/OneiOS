// SPDX-License-Identifier: MIT
// Host test of the capture plug-in's device on the Mac, against macOS 27's own CoreMedia and CMCapture (the same
// derived-object machinery and capture classes as iOS 27's; nothing of Apple's is in the repository). It checks:
//   - H9ISPCaptureDeviceCreate makes a FigCaptureDevice of CMCaptureDevice's class with one stream (the back camera),
//     whose formats come from an AVCaptureSession.plist (a stand-in written here) and whose properties answer;
//   - CMCapture's own BWFigCaptureDeviceVendor (macOS's), given our create function, makes the device (Apple's code calls ours);
//   - a frame from the stand-in node reaches the PixelBufferHandler of the PrimaryScaler output, scaled into a buffer
//     of the pool the "graph" (this test) gave, with the right levels.
// The node: a file with 48 bytes of room for the reader's two commands (its write()s move the offset), then one frame.
#import <Foundation/Foundation.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>
#include <dlfcn.h>
#include <mach/mach_time.h>
#include <objc/message.h>
#include <objc/runtime.h>
#include "OneiOSCamera.h"
#include "oneios_camera.h"

typedef struct OpaqueCMBaseObject *Obj;
struct base_class { uint64_t version, size; void *equal, *invalidate, *finalize, *desc;
  OSStatus (*copy)(Obj, CFStringRef, CFAllocatorRef, void *); OSStatus (*set)(Obj, CFStringRef, CFTypeRef); };
struct device_class { uint64_t version; void *pses; OSStatus (*request)(Obj, CFArrayRef, CFDictionaryRef); OSStatus (*relinquish)(Obj, CFArrayRef, CFDictionaryRef); };
struct stream_class { uint64_t version; OSStatus (*start)(Obj); OSStatus (*stop)(Obj); };
struct vtable { const void *reserved; const struct base_class *base; const void *derived; };
extern OSStatus H9ISPCaptureDeviceCreate(CFAllocatorRef, CFDictionaryRef, Obj *);

static int checks, failures;
#define CHECK(c) do { checks++; if (!(c)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); } } while (0)

static const struct vtable *vt(Obj o) { return ((const struct vtable *(*)(Obj))dlsym(RTLD_DEFAULT, "CMBaseObjectGetVTable"))(o); }
static CFTypeRef prop(Obj o, const char *name)
{
  CFTypeRef v = NULL;
  CFStringRef k = CFStringCreateWithCString(NULL, name, kCFStringEncodingUTF8);
  OSStatus e = vt(o)->base->copy(o, k, NULL, &v);
  CFRelease(k);
  return e ? NULL : v;
}

int main(void)
{
  @autoreleasepool {
    CHECK(dlopen("/System/Library/PrivateFrameworks/CMCaptureDevice.framework/CMCaptureDevice", RTLD_NOW) != NULL);
    CHECK(dlopen("/System/Library/PrivateFrameworks/CMCapture.framework/CMCapture", RTLD_NOW) != NULL);
    const char *node = getenv("ONEIOS_TEST_NODE"), *plist = getenv("ONEIOS_TEST_PLIST");
    // The stand-in plist: one back camera with two stream formats, as the device's own file names them.
    NSDictionary *fmts = @{ @"Photo": @{ @"SensorWidth": @4224, @"SensorHeight": @3024, @"MaxFrameRate": @60 },
                            @"Video-Binned-60": @{ @"Binned": @YES, @"SensorWidth": @2112, @"SensorHeight": @1512 } };
    NSDictionary *root = @{ @"AVCaptureDevices": @[ @{ @"uniqueName": @"Back Camera", @"MultiStream": @{ @"FigCaptureStreamFormats": fmts } } ] };
    CHECK([root writeToURL:[NSURL fileURLWithPath:@(plist)] error:nil]);
    // One frame, 64x48, rows 80 bytes apart: luma = column * 4, chroma 128 (grey).
    enum { W = 64, H = 48, STRIDE = 80 };
    static uint8_t picture[STRIDE * H * 3 / 2];
    for (int r = 0; r < H; r++) for (int x = 0; x < STRIDE; x++) picture[r * STRIDE + x] = (uint8_t)(x * 4);
    memset(picture + STRIDE * H, 128, STRIDE * H / 2);
    struct oc_frame wire = { .magic = OC_FRAME_MAGIC, .sequence = 7, .timestamp_ns = 1, .width = W, .height = H, .stride = STRIDE,
                             .format = OC_FMT_NV12, .bytes = sizeof picture, .role = OC_ROLE_WIDE, .orientation = 90, .fps = 30 };
    struct oneios_camera_frame header;
    oca_header(&header, &wire, OC_FACING_BACK, mach_absolute_time());
    NSMutableData *file = [NSMutableData dataWithLength:2 * sizeof(struct oneios_camera_command)];
    [file appendBytes:&header length:sizeof header];
    [file appendBytes:picture length:sizeof picture];
    CHECK([file writeToFile:@(node) atomically:NO]);

    Obj device = NULL;
    CHECK(H9ISPCaptureDeviceCreate(kCFAllocatorDefault, NULL, &device) == 0 && device != NULL);
    if (!device) { printf("camera device (Mac): %d checks, %d failed\n", checks, failures); return 1; }
    const void *(*device_class)(void) = dlsym(RTLD_DEFAULT, "FigCaptureDeviceGetClassID");
    const void *(*base_class_of)(Obj) = dlsym(RTLD_DEFAULT, "CMBaseObjectGetClassID");
    CHECK(base_class_of(device) == device_class());
    CFArrayRef streams = prop(device, "StreamArray");
    CHECK(streams && CFArrayGetCount(streams) == 1);
    Obj stream = (Obj)CFArrayGetValueAtIndex(streams, 0);
    CFStringRef port = prop(stream, "PortType");
    CFStringRef *back = dlsym(RTLD_DEFAULT, "kFigCapturePortType_BackFacingCamera");
    CHECK(port && back && CFEqual(port, *back));
    CFArrayRef formats = prop(stream, "SupportedFormatsArray");
    CHECK(formats && CFArrayGetCount(formats) == 2);
    if (formats && CFArrayGetCount(formats) == 2) {
      NSDictionary *f0 = (__bridge NSDictionary *)CFArrayGetValueAtIndex(formats, 0), *f1 = (__bridge NSDictionary *)CFArrayGetValueAtIndex(formats, 1);
      CHECK([f0[@"FormatName"] isEqual:@"Photo"] && [f0[@"SensorWidth"] intValue] == 4224 && [f0[@"VideoMaxFrameRate"] intValue] == 60);
      CHECK([f1[@"FormatName"] isEqual:@"Video-Binned-60"] && [f1[@"VideoIsBinned"] boolValue] && [f1[@"VideoMaxFrameRate"] intValue] == 30);
      CMFormatDescriptionRef fd = (__bridge CMFormatDescriptionRef)f0[@"FormatDescription"];
      CHECK(fd && CMVideoFormatDescriptionGetDimensions(fd).height == 3024);
    }
    CHECK(prop(stream, "PixelSize") != NULL && prop(stream, "Clock") != NULL && prop(stream, "SupportedPropertiesDictionary") != NULL);
    CHECK(prop(stream, "NoSuchProperty") == NULL);
    CHECK(prop(device, "SupportedPropertiesDictionary") != NULL);

    // Apple's vendor with our function: it calls H9ISPCaptureDeviceCreate and keeps the device.
    Class vendor_class = objc_getClass("BWFigCaptureDeviceVendor");
    CHECK(vendor_class != Nil);
    if (vendor_class) {
      id v = ((id (*)(id, SEL))objc_msgSend)(vendor_class, sel_registerName("alloc"));
      v = ((id (*)(id, SEL, void *))objc_msgSend)(v, sel_registerName("initWithDefaultDeviceCreateFunction:"), (void *)H9ISPCaptureDeviceCreate);
      CHECK(v != nil);
      int err = 0;
      id d = v ? ((id (*)(id, SEL, id, int *))objc_msgSend)(v, sel_registerName("copyDeviceForPublishingWithID:error:"), @"Default", &err) : nil;
      printf("vendor: device %s, error %d\n", d ? [[d description] UTF8String] : "(nil)", err);
      CHECK(d != nil && err == 0);
    }

    // The graph's side: a pool for PrimaryScaler, the handler, then reserve and start.
    CVPixelBufferPoolRef pool = NULL;
    NSDictionary *attrs = @{ (id)kCVPixelBufferPixelFormatTypeKey: @(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange),
                             (id)kCVPixelBufferWidthKey: @32, (id)kCVPixelBufferHeightKey: @24, (id)kCVPixelBufferIOSurfacePropertiesKey: @{} };
    CHECK(CVPixelBufferPoolCreate(NULL, NULL, (__bridge CFDictionaryRef)attrs, &pool) == 0);
    __block int given = 0; __block int y_left = -1, y_right = -1, c = -1;
    void (^handler)(CVPixelBufferRef, CMTime) = ^(CVPixelBufferRef pb, CMTime t) {
      (void)t;
      CVPixelBufferLockBaseAddress(pb, kCVPixelBufferLock_ReadOnly);
      const uint8_t *yp = CVPixelBufferGetBaseAddressOfPlane(pb, 0), *cp = CVPixelBufferGetBaseAddressOfPlane(pb, 1);
      y_left = yp[0]; y_right = yp[31]; c = cp[0];
      CVPixelBufferUnlockBaseAddress(pb, kCVPixelBufferLock_ReadOnly);
      given++;
    };
    const struct vtable *sv = vt(stream);
    CHECK(sv->base->set(stream, CFSTR("VideoOutputConfigurations"), (__bridge CFTypeRef)@{ @"PrimaryScaler": @{ @"BufferPool": (__bridge id)pool } }) == 0);
    CHECK(sv->base->set(stream, CFSTR("VideoOutputHandlers"), (__bridge CFTypeRef)@{ @"PrimaryScaler": @{ @"PixelBufferHandler": handler } }) == 0);
    CHECK(sv->base->set(stream, CFSTR("VideoOutputsEnabled"), (__bridge CFTypeRef)@{ @"PrimaryScaler": @YES }) == 0);
    CHECK(sv->base->set(stream, CFSTR("FormatIndex"), (__bridge CFTypeRef)@1) == 0);
    CHECK(sv->base->set(stream, CFSTR("FormatIndex"), (__bridge CFTypeRef)@9) != 0);
    CFDictionaryRef back_configs = prop(stream, "VideoOutputConfigurations");
    CHECK(back_configs && CFDictionaryGetCount(back_configs) == 1);
    const struct stream_class *sc = sv->derived;
    const struct device_class *dc = vt(device)->derived;
    CHECK(sc->version == 2 && dc->version == 3);
    CHECK(sc->start(stream) == 0);          // frames are given once the graph starts; the reader opens the node
    for (int i = 0; i < 300 && given == 0; i++) usleep(10000);
    CHECK(given >= 1);
    CHECK(y_left == 0 && y_right == 31 * 2 * 4 && c == 128);   // column 62 of 64 at x = 31 of 32
    CHECK(sc->stop(stream) == 0);
    CHECK(dc->request(device, streams, NULL) == 0 && dc->relinquish(device, streams, NULL) == 0);
    CHECK(sc->stop(stream) == 0);
    CFRelease(device);
    // The scalers alone: 420v levels and BGRA.
    uint8_t y[4 * 4], cc[2 * 4], bgra[4 * 4 * 4];
    CHECK(oneios_camera_scale_nv12(&header, picture, y, 4, cc, 4, 4, 4, true) == 0 && y[0] == 16 && cc[0] == (uint8_t)(16 + 128 * 224 / 255));
    CHECK(oneios_camera_scale_bgra(&header, picture, bgra, 16, 4, 4) == 0 && bgra[3] == 255 && bgra[4 * 2 + 1] == bgra[4 * 2]);
    CHECK(oneios_camera_scale_nv12(&header, picture, y, 2, cc, 4, 4, 4, false) == -EINVAL);
  }
  printf("camera device (Mac, macOS CMCapture): %d checks, %d failed\n", checks, failures);
  return failures != 0;
}
