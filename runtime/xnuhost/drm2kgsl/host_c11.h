// SPDX-License-Identifier: MIT
// For the host build of the virglrenderer sources (the Makefile's test_drm2kgsl), included first: mesa's C11 threads
// shim defines once_flag and call_once itself, and a new glibc's <stdlib.h> declares them too. glibc's go first under
// their own names; the shim's get other names.
#include <stdlib.h>
#undef ONCE_FLAG_INIT
#define once_flag virgl_once_flag
#define call_once virgl_call_once
