// SPDX-License-Identifier: MIT
// oneios-display: starts the display helper. The layer needs the framework's SurfaceControl
// classes, so the helper is a dex run by app_process (no zygote, no system_server); this finds
// the dex and the native library next to itself and execs app_process, keeping its pid.
//   oneios-display --session DIR [--takeover] [--wait SECONDS] ...   (see README.md)
#define _GNU_SOURCE
#include <errno.h>
#include <libgen.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv)
{
  char self[PATH_MAX], dex[PATH_MAX + 64], lib[PATH_MAX + 64];
  ssize_t n = readlink("/proc/self/exe", self, sizeof self - 1);
  if (n <= 0) { perror("oneios-display: /proc/self/exe"); return 2; }
  self[n] = 0;
  char *dir = dirname(self);
  snprintf(dex, sizeof dex, "%s/../lib/oneios-display.dex", dir);         // the runtime package's layout
  if (access(dex, R_OK)) snprintf(dex, sizeof dex, "%s/oneios-display.dex", dir);   // one folder (a test push)
  snprintf(lib, sizeof lib, "%s/liboneios-display.so", dir);
  if (access(dex, R_OK) || access(lib, R_OK)) {
    fprintf(stderr, "oneios-display: %s or %s is missing\n", dex, lib);
    return 2;
  }
  const char *process = getenv("ONEIOS_APP_PROCESS");
  if (!process) process = "/system/bin/app_process";
  setenv("CLASSPATH", dex, 1);
  char **args = calloc((size_t)argc + 6, sizeof *args);
  if (!args) return 2;
  int k = 0;
  args[k++] = "app_process";
  args[k++] = "/system/bin";
  args[k++] = "OneiOSDisplay";
  args[k++] = "--lib";
  args[k++] = lib;
  for (int i = 1; i < argc; i++) args[k++] = argv[i];
  execv(process, args);
  fprintf(stderr, "oneios-display: %s: %s\n", process, strerror(errno));
  return 2;
}
