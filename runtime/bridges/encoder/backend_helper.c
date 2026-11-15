// SPDX-License-Identifier: MIT
// The device-side encoder backend (encoder.h) that runs the real encoder in the oneios-encoder helper process
// (helper.c), because MediaCodec is a shared library and xnuhost is static. It spawns the helper once, multiplexes
// the encoder sessions over a UNIX socket (encoder_link.h), and buffers the coded access units the helper returns for
// poll(). Static, libc only, like the rest of libonebridges.
#define _GNU_SOURCE
#include "encoder.h"
#include "encoder_link.h"
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

enum { HELPER_MAX_SESSIONS = 8, HELPER_CODED_RING = 32 };

struct coded { uint8_t *bytes; uint32_t size; int64_t pts; int key, eos; };

struct br_enc_session {
  uint32_t id;
  bool used, open_ok, open_done;
  struct coded ring[HELPER_CODED_RING];
  unsigned head, tail;
  bool got_eos;
};

static struct {
  int fd;
  pid_t pid;
  pthread_t thread;
  pthread_mutex_t lock;
  pthread_cond_t cond;                     // open replies and new coded buffers
  bool started, gone, lock_init;
  struct br_enc_session sessions[HELPER_MAX_SESSIONS];
  char path[512];
} H;

static int write_all(int fd, const void *p, size_t n) {
  const uint8_t *b = p;
  while (n) { ssize_t w = write(fd, b, n); if (w <= 0) { if (w < 0 && errno == EINTR) continue; return -1; } b += w; n -= (size_t)w; }
  return 0;
}
static int read_all(int fd, void *p, size_t n) {
  uint8_t *b = p;
  while (n) { ssize_t r = read(fd, b, n); if (r <= 0) { if (r < 0 && errno == EINTR) continue; return -1; } b += r; n -= (size_t)r; }
  return 0;
}

static struct br_enc_session *session_by_id(uint32_t id) {
  for (unsigned i = 0; i < HELPER_MAX_SESSIONS; i++) if (H.sessions[i].used && H.sessions[i].id == id) return &H.sessions[i];
  return NULL;
}

// The receive loop: open replies, coded buffers and EOS from the helper.
static void *receive(void *arg) {
  (void)arg;
  for (;;) {
    struct enc_msg m;
    if (read_all(H.fd, &m, sizeof m)) break;
    uint8_t *payload = NULL;
    if (m.len) { payload = malloc(m.len); if (!payload) break; if (read_all(H.fd, payload, m.len)) { free(payload); break; } }
    pthread_mutex_lock(&H.lock);
    struct br_enc_session *s = session_by_id(m.session);
    if (m.type == ENC_MSG_OPEN_REPLY && s) { s->open_ok = m.session != 0 && m.a != 0; s->open_done = true; }
    else if (m.type == ENC_MSG_CODED && s && payload) {
      if (s->head - s->tail < HELPER_CODED_RING) {
        s->ring[s->head % HELPER_CODED_RING] = (struct coded){ payload, m.len, m.pts, (int)(m.a & 1), (int)((m.a >> 1) & 1) };
        s->head++;
        payload = NULL;
      }
    } else if (m.type == ENC_MSG_EOS && s) s->got_eos = true;
    pthread_cond_broadcast(&H.cond);
    pthread_mutex_unlock(&H.lock);
    free(payload);
  }
  pthread_mutex_lock(&H.lock);
  H.gone = true;
  pthread_cond_broadcast(&H.cond);
  pthread_mutex_unlock(&H.lock);
  return NULL;
}

// Spawns the helper and waits for its HELLO. 0 or -errno.
static int helper_start(void) {
  if (H.started) return H.gone ? -EPIPE : 0;
  int sv[2];
  if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) return -errno;
  posix_spawn_file_actions_t fa;
  posix_spawn_file_actions_init(&fa);
  posix_spawn_file_actions_adddup2(&fa, sv[1], 3);
  char three[] = "3";
  char *const argv[] = { H.path, three, NULL };
  int rc = posix_spawn(&H.pid, H.path, &fa, NULL, argv, environ);
  posix_spawn_file_actions_destroy(&fa);
  close(sv[1]);
  if (rc) { close(sv[0]); return -rc; }
  H.fd = sv[0];
  struct enc_msg hello;
  if (read_all(H.fd, &hello, sizeof hello) || hello.type != ENC_MSG_HELLO || hello.a != ENC_LINK_VERSION) {
    close(H.fd); return -EPROTO;
  }
  H.started = true;
  pthread_create(&H.thread, NULL, receive, NULL);
  return 0;
}

static br_enc_session *helper_open(uint32_t codec, uint32_t width, uint32_t height) {
  pthread_mutex_lock(&H.lock);
  if (helper_start()) { pthread_mutex_unlock(&H.lock); return NULL; }
  struct br_enc_session *s = NULL;
  for (unsigned i = 0; i < HELPER_MAX_SESSIONS; i++) if (!H.sessions[i].used) { s = &H.sessions[i]; break; }
  if (!s) { pthread_mutex_unlock(&H.lock); return NULL; }
  memset(s, 0, sizeof *s);
  s->used = true;
  s->id = (uint32_t)(s - H.sessions) + 1;
  struct enc_msg m = { ENC_MSG_OPEN, s->id, 0, codec, width, height, 0 };
  if (write_all(H.fd, &m, sizeof m)) { s->used = false; pthread_mutex_unlock(&H.lock); return NULL; }
  while (!s->open_done && !H.gone) pthread_cond_wait(&H.cond, &H.lock);
  bool ok = s->open_ok;
  if (!ok) s->used = false;
  pthread_mutex_unlock(&H.lock);
  return ok ? s : NULL;
}

static int helper_feed(br_enc_session *s, const uint8_t *nv12, uint32_t len, int64_t pts_us, int force_key) {
  struct enc_msg m = { ENC_MSG_FEED, s->id, pts_us, (uint32_t)(force_key ? 1 : 0), 0, 0, len };
  pthread_mutex_lock(&H.lock);
  int rc = (write_all(H.fd, &m, sizeof m) || write_all(H.fd, nv12, len)) ? -EIO : 0;
  pthread_mutex_unlock(&H.lock);
  return rc;
}

static int helper_drain(br_enc_session *s) {
  struct enc_msg m = { ENC_MSG_DRAIN, s->id, 0, 0, 0, 0, 0 };
  pthread_mutex_lock(&H.lock);
  int rc = write_all(H.fd, &m, sizeof m) ? -EIO : 0;
  pthread_mutex_unlock(&H.lock);
  return rc;
}

static int helper_poll(br_enc_session *s, uint8_t *buf, uint32_t cap, int *key, int64_t *pts_us, int *eos) {
  *key = 0; *eos = 0; *pts_us = 0;
  pthread_mutex_lock(&H.lock);
  int bytes = 0;
  if (s->tail != s->head) {
    struct coded *c = &s->ring[s->tail % HELPER_CODED_RING];
    // A unit larger than the guest's coded buffer is dropped and said once: left at the ring's head it came back as
    // an error at every poll of the device's worker, for as long as the session lived, and no later frame got out.
    if (c->size > cap) {
      free(c->bytes); c->bytes = NULL;
      s->tail++;
      pthread_mutex_unlock(&H.lock);
      return -ENOSPC;
    }
    memcpy(buf, c->bytes, c->size);
    *key = c->key; *eos = c->eos; *pts_us = c->pts;
    bytes = (int)c->size;
    free(c->bytes); c->bytes = NULL;
    s->tail++;
  } else if (s->got_eos) { *eos = 1; s->got_eos = false; }
  pthread_mutex_unlock(&H.lock);
  return bytes;
}

static void helper_close(br_enc_session *s) {
  struct enc_msg m = { ENC_MSG_CLOSE, s->id, 0, 0, 0, 0, 0 };
  pthread_mutex_lock(&H.lock);
  if (H.started && !H.gone) write_all(H.fd, &m, sizeof m);
  while (s->tail != s->head) { free(s->ring[s->tail % HELPER_CODED_RING].bytes); s->tail++; }
  s->used = false;
  pthread_mutex_unlock(&H.lock);
}

static br_encoder_backend helper_backend = {
  .name = "mediacodec-helper", .open = helper_open, .feed = helper_feed, .drain = helper_drain,
  .poll = helper_poll, .close = helper_close,
};

const br_encoder_backend *br_encoder_backend_helper(const char *path) {
  if (!H.lock_init) {
    pthread_mutex_init(&H.lock, NULL);
    pthread_cond_init(&H.cond, NULL);
    H.lock_init = true;
  }
  snprintf(H.path, sizeof H.path, "%s", path);
  return &helper_backend;
}
