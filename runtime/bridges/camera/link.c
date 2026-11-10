// SPDX-License-Identifier: MIT
// xnuhost's end of the camera helper (helper.c, bin/oneios-camera): starts it, takes the frame ring's
// descriptor and the camera list from it, and reads frames out of the ring. Static and libc only,
// like the rest of libonebridges.
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include "bridges/camera.h"
#include "camera.h"
#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <spawn.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

struct br_camera {
  int fd;
  pid_t pid;
  pthread_t thread;
  pthread_mutex_t lock;
  cam_ring_map ring;                      // ring.ring != NULL once the helper's hello came
  bool listed, gone;
  uint32_t ncameras;
  struct oc_camera cameras[OC_MAX_CAMERAS];
  uint64_t last;                          // the newest frame read
  br_camera_event_fn fn;
  void *ctx;
};

static void event(br_camera *c, int kind, int status)
{
  pthread_mutex_lock(&c->lock);
  br_camera_event_fn fn = c->fn;
  void *ctx = c->ctx;
  pthread_mutex_unlock(&c->lock);
  if (fn) fn(ctx, kind, status);
}

static void *receive(void *arg)
{
  br_camera *c = arg;
  for (;;) {
    struct { struct cam_msg message; struct oc_camera cameras[OC_MAX_CAMERAS]; } packet;
    union { struct cmsghdr header; char space[CMSG_SPACE(sizeof(int))]; } control;
    struct iovec iov = { &packet, sizeof packet };
    struct msghdr header = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = &control, .msg_controllen = sizeof control.space };
    ssize_t got = recvmsg(c->fd, &header, MSG_CMSG_CLOEXEC);
    if (got < 0 && errno == EINTR) continue;
    if (got <= 0) break;                                      // the helper ended, or br_camera_close shut the socket
    int passed = -1;
    for (struct cmsghdr *m = CMSG_FIRSTHDR(&header); m; m = CMSG_NXTHDR(&header, m))
      if (m->cmsg_level == SOL_SOCKET && m->cmsg_type == SCM_RIGHTS && m->cmsg_len >= CMSG_LEN(sizeof(int))) memcpy(&passed, CMSG_DATA(m), sizeof(int));
    if (got < (ssize_t)sizeof packet.message) { if (passed >= 0) close(passed); continue; }
    switch (packet.message.kind) {
    case CAM_MSG_HELLO: {
      cam_ring_map ring;
      int rc = passed >= 0 && packet.message.a == CAM_RING_VERSION ? cam_ring_attach(&ring, passed) : -EPROTO;
      if (rc) { br_log(0, "camera: the helper's frame ring is not usable: %s", strerror(-rc)); if (passed >= 0) close(passed); break; }
      passed = -1;
      pthread_mutex_lock(&c->lock);
      if (c->ring.ring) cam_ring_close(&c->ring);
      c->ring = ring;
      pthread_mutex_unlock(&c->lock);
      break;
    }
    case CAM_MSG_CAMERAS: {
      uint32_t count = packet.message.a;
      size_t have = ((size_t)got - sizeof packet.message) / sizeof(struct oc_camera);
      if (count > have) count = (uint32_t)have;
      pthread_mutex_lock(&c->lock);
      memcpy(c->cameras, packet.cameras, count * sizeof(struct oc_camera));
      c->ncameras = count;
      c->listed = true;
      pthread_mutex_unlock(&c->lock);
      br_log(1, "camera: the helper has %u cameras", count);
      break;
    }
    case CAM_MSG_STATE:
      if (packet.message.status) br_log(0, "camera: the helper's stream ended: %s", strerror((int)packet.message.status));
      else if (packet.message.a) br_log(1, "camera: on: %ux%u at %u fps", packet.message.b, packet.message.c, packet.message.d);
      event(c, BR_CAMERA_STATE, (int)packet.message.status);
      break;
    case CAM_MSG_FRAME:
      event(c, BR_CAMERA_FRAME, 0);
      break;
    }
    if (passed >= 0) close(passed);
  }
  pthread_mutex_lock(&c->lock);
  c->gone = true;
  c->listed = true;
  pthread_mutex_unlock(&c->lock);
  event(c, BR_CAMERA_STATE, EPIPE);
  return NULL;
}

br_camera *br_camera_helper(const char *path, const char *backend)
{
  int pair[2];
  if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, pair)) { br_log(0, "camera: off: socketpair: %s", strerror(errno)); return NULL; }
  br_camera *c = calloc(1, sizeof *c);
  posix_spawn_file_actions_t actions;
  posix_spawn_file_actions_init(&actions);
  posix_spawn_file_actions_adddup2(&actions, pair[1], 3);       // the helper's end is its descriptor 3
  char *argv[] = { (char *)(uintptr_t)path, (char *)"3", (char *)(uintptr_t)backend, NULL };
  int rc = c ? posix_spawn(&c->pid, path, &actions, NULL, argv, environ) : ENOMEM;
  posix_spawn_file_actions_destroy(&actions);
  close(pair[1]);
  if (rc) { br_log(0, "camera: off: %s: %s", path, strerror(rc)); close(pair[0]); free(c); return NULL; }
  c->fd = pair[0];
  c->ring.fd = -1;
  pthread_mutex_init(&c->lock, NULL);
  if (pthread_create(&c->thread, NULL, receive, c)) {
    br_log(0, "camera: off: no thread for the helper");
    close(c->fd);                           // the helper exits when its socket closes
    free(c);
    return NULL;
  }
  br_log(1, "camera: helper %s (pid %d)", path, (int)c->pid);
  return c;
}

void br_camera_listen(br_camera *c, br_camera_event_fn fn, void *ctx)
{
  pthread_mutex_lock(&c->lock);
  c->fn = fn;
  c->ctx = ctx;
  pthread_mutex_unlock(&c->lock);
}

int br_camera_list(br_camera *c, struct oc_camera *out, int max, int wait_ms)
{
  for (int waited = 0;; waited += 5) {
    pthread_mutex_lock(&c->lock);
    bool listed = c->listed;
    int count = (int)c->ncameras < max ? (int)c->ncameras : max;
    if (listed) memcpy(out, c->cameras, (size_t)count * sizeof *out);
    pthread_mutex_unlock(&c->lock);
    if (listed) return count;
    if (waited >= wait_ms) return 0;
    struct timespec nap = { 0, 5000000 };
    nanosleep(&nap, NULL);
  }
}

static int send_message(br_camera *c, const struct cam_msg *message)
{
  // Never block a vCPU on a helper that is behind.
  return send(c->fd, message, sizeof *message, MSG_DONTWAIT | MSG_NOSIGNAL) < 0 ? -errno : 0;
}

int br_camera_start(br_camera *c, uint32_t role, uint32_t width, uint32_t height, uint32_t fps)
{
  pthread_mutex_lock(&c->lock);
  bool gone = c->gone;
  // What is in the ring now is from before this start.
  if (c->ring.ring) c->last = cam_ring_newest(&c->ring);
  pthread_mutex_unlock(&c->lock);
  if (gone) return -EPIPE;
  struct cam_msg message = { .kind = CAM_MSG_START, .a = role, .b = width, .c = height, .d = fps };
  return send_message(c, &message);
}

int br_camera_stop(br_camera *c)
{
  struct cam_msg message = { .kind = CAM_MSG_STOP };
  return send_message(c, &message);
}

int br_camera_frame(br_camera *c, struct oc_frame *frame, uint8_t *dst, size_t cap)
{
  pthread_mutex_lock(&c->lock);
  int rc = c->ring.ring ? cam_ring_read(&c->ring, &c->last, frame, dst, cap) : -EAGAIN;
  pthread_mutex_unlock(&c->lock);
  return rc;
}

void br_camera_close(br_camera *c)
{
  if (!c) return;
  br_camera_listen(c, NULL, NULL);
  shutdown(c->fd, SHUT_RDWR);               // the helper sees the end, closes the camera and exits
  pthread_join(c->thread, NULL);
  close(c->fd);
  cam_ring_close(&c->ring);
  // The helper needs a moment to close the camera; it is not waited for (PR_SET_PDEATHSIG and the
  // session's cgroup end it in any case).
  (void)waitpid(c->pid, NULL, WNOHANG);
  pthread_mutex_destroy(&c->lock);
  free(c);
}
