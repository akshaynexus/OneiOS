// SPDX-License-Identifier: MIT
// oneios-mg: asks MobileGestalt what the guest thinks it is, and checks it against what OneiOS expects.
//
//   oneios-mg                      prints the identity keys below, one "key=value" line each
//   oneios-mg KEY...               prints those keys
//   oneios-mg --expect FILE        FILE has "key=value" lines (roottweak writes identity.expect from the device
//                                  profile); prints each as ok/DIFFERS/missing and exits 1 if any differs
//
// The identity reaches MobileGestalt from the device tree the booter builds (model, device class, display
// values); nothing in the root is edited for it on a sealed root. This tool is the check that it arrived:
// SpringBoard builds the phone interface only when DeviceClass is iPhone, and lays the screen out by
// main-screen-scale and the artwork subtype. Run at boot by oneios-init (services.d/identity.conf), its
// output is log/identity.log.
//
// MGCopyAnswer is private API of /usr/lib/libMobileGestalt.dylib; it is looked up at run time so the build
// needs only the public SDK. Guest only (CoreFoundation): there is no host build.
#include <CoreFoundation/CoreFoundation.h>
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>

static CFTypeRef (*copy_answer)(CFStringRef);

static const char *const defaults[] = {
  "ProductType", "HWModelStr", "DeviceClass", "DeviceClassNumber", "ProductName", "ProductVersion", "BuildVersion",
  "main-screen-width", "main-screen-height", "main-screen-scale", "main-screen-pitch", "ArtworkDeviceSubType",
  "DeviceName", "HasBaseband", "telephony", "cellular-data", NULL,
};

// The answer as text: strings as they are, numbers and booleans as decimal; other types as "<type>".
static int answer(const char *key, char *out, size_t size) {
  CFStringRef name = CFStringCreateWithCString(NULL, key, kCFStringEncodingUTF8);
  CFTypeRef value = name ? copy_answer(name) : NULL;
  if (name) CFRelease(name);
  if (!value) return 0;
  CFTypeID type = CFGetTypeID(value);
  if (type == CFStringGetTypeID()) {
    if (!CFStringGetCString(value, out, (CFIndex)size, kCFStringEncodingUTF8)) snprintf(out, size, "<string>");
  } else if (type == CFBooleanGetTypeID()) {
    snprintf(out, size, "%d", CFBooleanGetValue(value) ? 1 : 0);
  } else if (type == CFNumberGetTypeID()) {
    double number = 0;
    CFNumberGetValue(value, kCFNumberDoubleType, &number);
    if (number == (long long)number) snprintf(out, size, "%lld", (long long)number);
    else snprintf(out, size, "%g", number);
  } else {
    snprintf(out, size, "<type %lu>", (unsigned long)type);
  }
  CFRelease(value);
  return 1;
}

static int expect(const char *file) {
  FILE *f = fopen(file, "r");
  if (!f) { perror(file); return 2; }
  char line[512], have[256];
  int differs = 0;
  while (fgets(line, sizeof line, f)) {
    size_t n = strlen(line);
    while (n && (line[n - 1] == '\n' || line[n - 1] == '\r')) line[--n] = 0;
    char *eq = strchr(line, '=');
    if (!n || line[0] == '#' || !eq) continue;
    *eq = 0;
    const char *want = eq + 1;
    if (!answer(line, have, sizeof have)) { printf("missing  %s (expected %s)\n", line, want); differs = 1; }
    else if (strcmp(have, want)) { printf("DIFFERS  %s=%s (expected %s)\n", line, have, want); differs = 1; }
    else printf("ok       %s=%s\n", line, have);
  }
  fclose(f);
  printf("ONEIOS-GUEST identity=%s\n", differs ? "differs" : "ok");
  return differs;
}

int main(int argc, char **argv) {
  void *library = dlopen("/usr/lib/libMobileGestalt.dylib", RTLD_NOW);
  copy_answer = library ? (CFTypeRef (*)(CFStringRef))dlsym(library, "MGCopyAnswer") : NULL;
  if (!copy_answer) { fprintf(stderr, "oneios-mg: no MGCopyAnswer: %s\n", dlerror()); return 2; }
  if (argc == 3 && !strcmp(argv[1], "--expect")) return expect(argv[2]);
  char have[256];
  const char *const *keys = argc > 1 ? (const char *const *)(argv + 1) : defaults;
  for (int i = 0; keys[i]; i++) {
    if (answer(keys[i], have, sizeof have)) printf("%s=%s\n", keys[i], have);
    else printf("%s: no answer\n", keys[i]);
  }
  return 0;
}
