// Stand-in declarations for the syntax check (see CoreFoundation/CoreFoundation.h beside this).
#ifndef STUB_MACHO_DYLD_H
#define STUB_MACHO_DYLD_H
#include <stdint.h>
struct mach_header;
void _dyld_register_func_for_add_image(void (*func)(const struct mach_header *header, intptr_t slide));
#endif
