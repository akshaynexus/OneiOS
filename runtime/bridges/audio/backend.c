// SPDX-License-Identifier: MIT
// Audio back ends for the virtio-snd device: the AAudio helper process and a null one.
//
// Why a helper: AAudio is a shared library (libaaudio.so) and xnuhost is a static binary. The
// helper (bin/oneios-audio, audio/aaudio_helper.c) is a small dynamic executable that plays what it
// is sent through One UI's audioserver, which keeps running in a session, and sends back what the
// microphone records.
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include "audio_proto.h"
#include <errno.h>
#include <poll.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

extern char **environ;

// ---- null ---------------------------------------------------------------------------------------
static int null_start(void *ctx, bool output, unsigned rate, unsigned channels) { (void)ctx; (void)output; (void)rate; (void)channels; return 0; }
static int null_stop(void *ctx, bool output) { (void)ctx; (void)output; return 0; }
static int null_write(void *ctx, const int16_t *pcm, unsigned frames) { (void)ctx; (void)pcm; return (int)frames; }
static int null_read(void *ctx, int16_t *pcm, unsigned frames)
{
  (void)ctx;
  usleep(frames * 1000000u / 48000u);          // silence in real time
  memset(pcm, 0, frames * 2);
  return (int)frames;
}
static void null_close(void *ctx) { (void)ctx; }
static const br_audio_backend null_backend = { null_start, null_stop, null_write, null_read, null_close };
const br_audio_backend *br_audio_null(void **ctx) { *ctx = NULL; return &null_backend; }

// ---- the helper ---------------------------------------------------------------------------------
typedef struct { int fd; pid_t pid; unsigned in_channels; } helper;

static int send_message(helper *h, const struct au_header *header, const void *payload, size_t length)
{
  struct iovec iov[2] = { { (void *)(uintptr_t)header, sizeof *header }, { (void *)(uintptr_t)payload, length } };
  struct msghdr message = { .msg_iov = iov, .msg_iovlen = payload ? 2 : 1 };
  // Never block a vCPU on a helper that is behind: a full socket drops the period.
  return sendmsg(h->fd, &message, MSG_DONTWAIT | MSG_NOSIGNAL) < 0 ? -errno : 0;
}

static int helper_start(void *ctx, bool output, unsigned rate, unsigned channels)
{
  helper *h = ctx;
  struct au_header header = { .kind = AU_START, .output = output, .channels = (uint8_t)channels, .value = rate };
  if (!output) h->in_channels = channels;
  return send_message(h, &header, NULL, 0);
}

static int helper_stop(void *ctx, bool output)
{
  struct au_header header = { .kind = AU_STOP, .output = output };
  return send_message(ctx, &header, NULL, 0);
}

static int helper_write(void *ctx, const int16_t *pcm, unsigned frames)
{
  if (frames > AU_MAX_FRAMES) frames = AU_MAX_FRAMES;
  struct au_header header = { .kind = AU_PCM, .output = 1, .channels = 2, .value = frames };
  return send_message(ctx, &header, pcm, frames * 4u) ? 0 : (int)frames;
}

static int helper_read(void *ctx, int16_t *pcm, unsigned frames)
{
  helper *h = ctx;
  struct { struct au_header header; int16_t pcm[AU_MAX_FRAMES]; } packet;
  struct pollfd wait = { .fd = h->fd, .events = POLLIN };
  for (;;) {
    if (poll(&wait, 1, 100) <= 0) return 0;
    ssize_t got = recv(h->fd, &packet, sizeof packet, MSG_DONTWAIT);
    if (got < (ssize_t)sizeof packet.header) return got == 0 ? -EPIPE : 0;
    if (packet.header.kind == AU_STATE) { if (packet.header.value) br_log(0, "audio: helper: %s stream: %s", packet.header.output ? "speaker" : "microphone", strerror((int)packet.header.value)); continue; }
    if (packet.header.kind != AU_PCM || packet.header.output) continue;
    unsigned have = (unsigned)(((size_t)got - sizeof packet.header) / 2);
    if (have > packet.header.value) have = packet.header.value;
    if (have > frames) have = frames;
    memcpy(pcm, packet.pcm, have * 2);
    return (int)have;
  }
}

static void helper_close(void *ctx)
{
  helper *h = ctx;
  close(h->fd);                                  // the helper exits when its socket closes
  free(h);
}

static const br_audio_backend helper_backend = { helper_start, helper_stop, helper_write, helper_read, helper_close };

const br_audio_backend *br_audio_helper(const char *path, void **ctx)
{
  int pair[2];
  if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, pair)) { br_log(0, "audio: off: socketpair: %s", strerror(errno)); return NULL; }
  helper *h = calloc(1, sizeof *h);
  posix_spawn_file_actions_t actions;
  posix_spawn_file_actions_init(&actions);
  posix_spawn_file_actions_adddup2(&actions, pair[1], 3);       // the helper's end is its descriptor 3
  char *argv[] = { (char *)(uintptr_t)path, (char *)"3", NULL };
  int rc = h ? posix_spawn(&h->pid, path, &actions, NULL, argv, environ) : ENOMEM;
  posix_spawn_file_actions_destroy(&actions);
  close(pair[1]);
  if (rc) { br_log(0, "audio: off: %s: %s", path, strerror(rc)); close(pair[0]); free(h); return NULL; }
  h->fd = pair[0];
  br_log(1, "audio: on: helper %s (pid %d)", path, (int)h->pid);
  *ctx = h;
  return &helper_backend;
}
