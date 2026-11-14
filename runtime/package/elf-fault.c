// A test input of runtime/package/elf-check-test.sh and runtime/package/test.sh, never part of the runtime.
// It does what liboneios-display.so of 1f7dd1f did: it calls the NDK's dataspace functions, which libnativewindow.so
// has and libandroid.so has not. Linked as a shared object with `-landroid` only it links (without -z defs) and
// Android cannot load it: `cannot locate symbol "ANativeWindow_getBuffersDataSpace"` (the phone, 2026-11-14).
#include <android/native_window.h>

int oneios_elf_fault(ANativeWindow *window) {
  return ANativeWindow_setBuffersDataSpace(window, ANativeWindow_getBuffersDataSpace(window));
}

#ifdef ELF_FAULT_MAIN
int main(void) { return oneios_elf_fault(0); }
#endif
