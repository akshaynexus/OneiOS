// Stand-in declarations for the syntax check (see CoreFoundation/CoreFoundation.h beside this).
#ifndef STUB_COREVIDEO_H
#define STUB_COREVIDEO_H
#include <CoreFoundation/CoreFoundation.h>
#include <stddef.h>
typedef struct stub_pixel_buffer *CVPixelBufferRef;
typedef struct stub_pixel_buffer_pool *CVPixelBufferPoolRef;
typedef int32_t CVReturn;
typedef uint64_t CVOptionFlags;
enum { kCVReturnSuccess = 0 };
enum { kCVPixelFormatType_420YpCbCr8BiPlanarFullRange = 0x34323066 };   // '420f'
extern const CFStringRef kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey;
CVReturn CVPixelBufferPoolCreate(CFAllocatorRef allocator, CFDictionaryRef pool_attributes, CFDictionaryRef buffer_attributes, CVPixelBufferPoolRef *out);
CVReturn CVPixelBufferPoolCreatePixelBuffer(CFAllocatorRef allocator, CVPixelBufferPoolRef pool, CVPixelBufferRef *out);
CVReturn CVPixelBufferLockBaseAddress(CVPixelBufferRef buffer, CVOptionFlags flags);
CVReturn CVPixelBufferUnlockBaseAddress(CVPixelBufferRef buffer, CVOptionFlags flags);
void *CVPixelBufferGetBaseAddressOfPlane(CVPixelBufferRef buffer, size_t plane);
size_t CVPixelBufferGetBytesPerRowOfPlane(CVPixelBufferRef buffer, size_t plane);
void CVPixelBufferRelease(CVPixelBufferRef buffer);
#endif
