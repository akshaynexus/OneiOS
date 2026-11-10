// SPDX-License-Identifier: MIT
#include "xnuhost.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static int log_fd = 2, log_level = XH_LOG_INFO;
static pthread_mutex_t log_lock = PTHREAD_MUTEX_INITIALIZER;
static char console_line[512];
static size_t console_used;

void xh_log_open(const char *path, int level)
{
  log_level = level;
  if (path && *path) {
    int fd = open(path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    if (fd >= 0) log_fd = fd;
    else fprintf(stderr, "xnuhost: cannot open log %s: %s\n", path, strerror(errno));
  }
}

static void emit(const char *tag, const char *text, size_t length)
{
  char head[48];
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  int n = snprintf(head, sizeof head, "[%6ld.%03ld] %s ", (long)ts.tv_sec, ts.tv_nsec / 1000000, tag);
  struct iovec { const void *base; size_t len; };
  (void)!write(log_fd, head, (size_t)n);
  (void)!write(log_fd, text, length);
  if (!length || text[length - 1] != '\n') (void)!write(log_fd, "\n", 1);
}

void xh_log(int level, const char *fmt, ...)
{
  if (level > log_level) return;
  char line[1024];
  va_list ap;
  va_start(ap, fmt);
  int n = vsnprintf(line, sizeof line, fmt, ap);
  va_end(ap);
  if (n < 0) return;
  if ((size_t)n >= sizeof line) n = sizeof line - 1;
  pthread_mutex_lock(&log_lock);
  emit(level == XH_LOG_ERROR ? "xnuhost E" : level == XH_LOG_INFO ? "xnuhost I" : "xnuhost D", line, (size_t)n);
  pthread_mutex_unlock(&log_lock);
}

void xh_log_raw(const char *text, size_t length)
{
  pthread_mutex_lock(&log_lock);
  for (size_t i = 0; i < length; i++) {
    char c = text[i];
    if (c == '\r') continue;
    if (c == '\n' || console_used == sizeof console_line - 1) {
      emit("guest    ", console_line, console_used);
      console_used = 0;
      if (c == '\n') continue;
    }
    console_line[console_used++] = c;
  }
  pthread_mutex_unlock(&log_lock);
}

void xh_log_signal(const char *text, uint64_t a, uint64_t b)
{
  char line[160];
  size_t n = 0;
  static const char prefix[] = "xnuhost !! ";
  memcpy(line, prefix, sizeof prefix - 1);
  n = sizeof prefix - 1;
  for (; *text && n < 96; text++) line[n++] = *text;
  uint64_t values[2] = { a, b };
  for (int v = 0; v < 2; v++) {
    line[n++] = ' '; line[n++] = '0'; line[n++] = 'x';
    for (int shift = 60; shift >= 0; shift -= 4) line[n++] = "0123456789abcdef"[(values[v] >> shift) & 15];
  }
  line[n++] = '\n';
  (void)!write(log_fd, line, n);
}
