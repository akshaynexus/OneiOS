// SPDX-License-Identifier: MIT
// The real display.c against a stand-in helper on the real socket: where the guest's mode comes from.
#define _GNU_SOURCE
#include "xnuhost.h"
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

// display.c ends input.c's touch-to-frame measurement at each shown frame; input.c is not linked here.
void xh_input_frame_shown(void) {}

struct message { char magic[4]; uint32_t a, b, c, d; };
static struct { char path[300]; uint32_t w, h, stride, format; off_t memfd_size; int ok; } helper;

static uint32_t report_w = 1440, report_h = 3120, report_hz = 120;

static void *helper_main(void *arg)
{
  (void)arg;
  struct sockaddr_un addr = { .sun_family = AF_UNIX };
  strcpy(addr.sun_path, helper.path);
  int fd = socket(AF_UNIX, SOCK_SEQPACKET, 0);
  for (int i = 0; i < 300 && connect(fd, (struct sockaddr *)&addr, sizeof addr); i++) usleep(10000);
  struct message panel = { { 'X', 'H', 'D', 'P' }, report_w, report_h, report_hz, 0 }, hello;
  send(fd, &panel, sizeof panel, MSG_NOSIGNAL);
  char control[CMSG_SPACE(sizeof(int))];
  struct iovec iov = { &hello, sizeof hello };
  struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof control };
  if (recvmsg(fd, &msg, 0) == (ssize_t)sizeof hello && !memcmp(hello.magic, "XHD1", 4)) {
    int memfd;
    memcpy(&memfd, CMSG_DATA(CMSG_FIRSTHDR(&msg)), sizeof memfd);
    struct stat st;
    fstat(memfd, &st);
    helper.w = hello.a; helper.h = hello.b; helper.stride = hello.c; helper.format = hello.d; helper.memfd_size = st.st_size;
    helper.ok = 1;
  }
  return NULL;
}

// In a child: display.c's state is once per process. 0 when the frame, the hello and the time are as expected.
static int run(const char *dir, const char *mode, unsigned cw, unsigned ch, unsigned chz, unsigned w, unsigned h, unsigned hz, double most_seconds)
{
  pid_t child = fork();
  if (child) { int status = 1; waitpid(child, &status, 0); return WIFEXITED(status) ? WEXITSTATUS(status) : 99; }
  xh_config c;
  xh_config_defaults(&c);
  strcpy(c.mode, mode);
  strcpy(c.session, dir);
  c.scale = 3; c.width = cw; c.height = ch; c.hz = chz;
  snprintf(helper.path, sizeof helper.path, "%s/display.sock", dir);
  pthread_t thread;
  pthread_create(&thread, NULL, helper_main, NULL);
  struct timespec t0, t1;
  clock_gettime(CLOCK_MONOTONIC, &t0);
  unsigned gw = 0, gh = 0, ghz = 0;
  if (xh_display_init(&c, &gw, &gh, &ghz)) _exit(2);
  clock_gettime(CLOCK_MONOTONIC, &t1);
  if (gw != w || gh != h || ghz != hz) { fprintf(stderr, "guest %ux%u at %u\n", gw, gh, ghz); _exit(3); }
  if ((t1.tv_sec - t0.tv_sec) + (t1.tv_nsec - t0.tv_nsec) / 1e9 > most_seconds) _exit(4);
  pthread_join(thread, NULL);
  if (!helper.ok || helper.w != w || helper.h != h || helper.stride != w * 4 || helper.format != 1) _exit(5);
  if (helper.memfd_size != (off_t)((size_t)w * 4 * h)) _exit(6);       // what the guest's scanout has to fit: 17.97 MB at 1440x3120
  xh_display *d = xh_display_get();
  if (!d || d->width != w || d->height != h || d->stride != w * 4) _exit(7);
  _exit(0);
}

int main(void)
{
  char dir[] = "/tmp/xnuhost-display-XXXXXX";
  if (!mkdtemp(dir)) return 2;
  xh_log_open(NULL, XH_LOG_ERROR);
  int failures = 0, r;
#define CASE(name, ...) do { r = run(dir, __VA_ARGS__); printf("%s: %s (%d)\n", name, r ? "FAILED" : "ok", r); failures += r != 0; } while (0)
  CASE("a session takes the helper's report", "takeover", 0, 0, 0, 1440, 3120, 120, 5.0);
  report_w = 1260; report_h = 2800; report_hz = 90;                      // not the default: only the report can give this
  CASE("another panel's report is the guest's mode", "takeover", 0, 0, 0, 1260, 2800, 90, 5.0);
  report_w = 1440; report_h = 3120; report_hz = 120;
  CASE("a configured mode wins, without waiting", "takeover", 1080, 2340, 60, 1080, 2340, 60, 1.0);
  CASE("a bench run assumes the S24 Ultra's, without waiting", "bench", 0, 0, 0, 1440, 3120, 120, 1.0);
  char command[100];
  snprintf(command, sizeof command, "rm -rf %s", dir);
  if (system(command)) failures++;
  printf("display mode tests: %d failed\n", failures);
  return failures != 0;
}
