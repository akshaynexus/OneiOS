// SPDX-License-Identifier: MIT
// bin/oneios-camera: the camera helper process. xnuhost is a static binary and the camera needs
// Android's shared libraries (libcamera2ndk, libbinder_ndk, libnativewindow), so, like oneios-audio,
// a small dynamic executable does it: it opens the camera xnuhost asks for and puts each frame, as
// NV12 with its time, into a memfd ring that xnuhost maps.
//
//   oneios-camera FD [BACKEND]     FD: a SOCK_SEQPACKET to xnuhost (camera.h, CAM_MSG_*)
//                                  BACKEND: auto (default), ndk, hal, test; or $ONEIOS_CAMERA_BACKEND
//
// It starts and ends with the session: it exits when its socket closes or on SIGTERM, and it dies
// with xnuhost (PR_SET_PDEATHSIG). It reads no input device, asks for no realtime scheduling,
// touches nothing under /sys, and holds the camera only between START and STOP.
#define _GNU_SOURCE
#include "camera.h"
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <unistd.h>

static int channel = -1;
static volatile sig_atomic_t quit;
static int stream_error;                  // atomic: the errno a running stream ended with

static void on_signal(int number) { (void)number; quit = 1; }

static int send_message(const struct cam_msg *message, const void *payload, size_t length, int fd)
{
  struct iovec iov[2] = { { (void *)(uintptr_t)message, sizeof *message }, { (void *)(uintptr_t)payload, length } };
  union { struct cmsghdr header; char space[CMSG_SPACE(sizeof(int))]; } control;
  struct msghdr packet = { .msg_iov = iov, .msg_iovlen = length ? 2 : 1 };
  if (fd >= 0) {
    memset(&control, 0, sizeof control);
    packet.msg_control = &control;
    packet.msg_controllen = sizeof control.space;
    control.header.cmsg_level = SOL_SOCKET;
    control.header.cmsg_type = SCM_RIGHTS;
    control.header.cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(&control.header), &fd, sizeof(int));
  }
  return sendmsg(channel, &packet, MSG_NOSIGNAL | MSG_DONTWAIT) < 0 ? -errno : 0;
}

// A frame is in the ring. A full socket drops the note, not the frame: the reader takes the newest.
static void on_frame(void *ctx, uint64_t sequence)
{
  (void)ctx;
  struct cam_msg note = { .kind = CAM_MSG_FRAME, .seq = sequence };
  (void)send_message(&note, NULL, 0, -1);
}

static void on_error(void *ctx, int error)
{
  (void)ctx;
  __atomic_store_n(&stream_error, error ? error : EIO, __ATOMIC_RELEASE);
}

static void send_state(const cam_core *core, int status)
{
  struct cam_msg state = { .kind = CAM_MSG_STATE, .status = (uint32_t)status, .a = core->running, .b = (uint32_t)core->width,
                           .c = (uint32_t)core->height, .d = (uint32_t)core->fps };
  (void)send_message(&state, NULL, 0, -1);
}

int main(int argc, char **argv)
{
  static cam_core core;
  if (argc < 2) { fprintf(stderr, "usage: oneios-camera FD [auto|ndk|hal|test]   (started by xnuhost; camctl tries it by hand)\n"); return 2; }
  channel = atoi(argv[1]);
  const char *backend = argc > 2 ? argv[2] : getenv("ONEIOS_CAMERA_BACKEND");
  if (getenv("ONEIOS_CAMERA_VERBOSE")) cam_verbose = 1;
  (void)prctl(PR_SET_PDEATHSIG, SIGTERM);
  struct sigaction action = { .sa_handler = on_signal };
  sigaction(SIGTERM, &action, NULL);
  sigaction(SIGINT, &action, NULL);
  sigaction(SIGHUP, &action, NULL);
  signal(SIGPIPE, SIG_IGN);

  core.on_frame = on_frame;
  core.on_error = on_error;
  int rc = cam_core_init(&core, backend);
  if (rc && !core.ring.ring) { cam_log("no frame ring: %s", strerror(-rc)); return 1; }
  if (rc) cam_log("%s: no cameras: %s", core.backend ? core.backend->name : "back end", strerror(-rc));
  else cam_log("%s: %d cameras", core.backend->name, core.ncameras);

  struct cam_msg hello = { .kind = CAM_MSG_HELLO, .a = CAM_RING_VERSION };
  if (send_message(&hello, NULL, 0, core.ring.fd)) { cam_log("no xnuhost on descriptor %d: %s", channel, strerror(errno)); return 1; }
  struct oc_camera list[OC_MAX_CAMERAS];
  uint32_t count = 0;
  // One camera per role goes to the guest; the cameras without a role stay the helper's.
  for (int i = 0; i < core.ncameras && count < OC_MAX_CAMERAS; i++)
    if (core.cameras[i].role != OC_ROLE_OTHER && core.cameras[i].role != OC_ROLE_NONE) cam_info_to_wire(&core.cameras[i], &list[count++]);
  struct cam_msg cameras = { .kind = CAM_MSG_CAMERAS, .a = count };
  (void)send_message(&cameras, list, count * sizeof list[0], -1);

  while (!quit) {
    struct pollfd wait = { .fd = channel, .events = POLLIN };
    int ready = poll(&wait, 1, 200);
    int ended = __atomic_exchange_n(&stream_error, 0, __ATOMIC_ACQ_REL);
    if (ended && core.running) { cam_core_stop(&core); send_state(&core, ended); }
    if (ready < 0 && errno != EINTR) break;
    if (ready <= 0) continue;
    struct cam_msg message;
    ssize_t got = recv(channel, &message, sizeof message, MSG_DONTWAIT);
    if (got == 0 || (got < 0 && errno != EAGAIN && errno != EINTR)) break;      // xnuhost is gone
    if (got < (ssize_t)sizeof message) continue;
    if (message.kind == CAM_MSG_START) {
      const cam_info *camera = NULL;
      for (int i = 0; i < core.ncameras && !camera; i++) if (core.cameras[i].role == message.a) camera = &core.cameras[i];
      rc = camera ? cam_core_start(&core, camera, (int)message.b, (int)message.c, (int)message.d) : -ENODEV;
      send_state(&core, -rc);
    } else if (message.kind == CAM_MSG_STOP) {
      cam_core_stop(&core);
      send_state(&core, 0);
    }
  }
  cam_core_stop(&core);                     // closes the camera: the HAL keeps nothing of ours
  return 0;
}
