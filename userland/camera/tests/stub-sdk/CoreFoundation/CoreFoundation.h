// Stand-in declarations for the syntax check of OneiOSCamera.c on a machine without an iPhoneOS SDK
// (tests/host.sh). Written here from the public API's documented shapes; not Apple's header.
#ifndef STUB_COREFOUNDATION_H
#define STUB_COREFOUNDATION_H
#include <stdint.h>
typedef const void *CFTypeRef;
typedef const struct stub_allocator *CFAllocatorRef;
typedef const struct stub_number *CFNumberRef;
typedef const struct stub_dictionary *CFDictionaryRef;
typedef const struct stub_string *CFStringRef;
typedef long CFIndex;
typedef struct { CFIndex version; void *retain, *release, *describe, *equal, *hash; } CFDictionaryKeyCallBacks;
typedef struct { CFIndex version; void *retain, *release, *describe, *equal; } CFDictionaryValueCallBacks;
extern const CFAllocatorRef kCFAllocatorDefault;
extern const CFDictionaryKeyCallBacks kCFTypeDictionaryKeyCallBacks;
extern const CFDictionaryValueCallBacks kCFTypeDictionaryValueCallBacks;
enum { kCFNumberSInt32Type = 3 };
CFNumberRef CFNumberCreate(CFAllocatorRef allocator, CFIndex type, const void *value);
CFDictionaryRef CFDictionaryCreate(CFAllocatorRef allocator, const void **keys, const void **values, CFIndex count,
    const CFDictionaryKeyCallBacks *key_callbacks, const CFDictionaryValueCallBacks *value_callbacks);
void CFRelease(CFTypeRef object);
#endif
