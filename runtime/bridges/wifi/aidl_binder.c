// SPDX-License-Identifier: MIT
// The kernel's binder protocol for a stable-AIDL client (aidl.h): /dev/binder through the UAPI, no Android library.
// One looper thread takes the calls into our objects (the supplicant's callbacks are one-way) and the death
// notifications; any thread makes calls, one at a time. A call the callee makes back into us before it answers
// comes to the calling thread (the driver sends it down the same transaction stack) and is answered there.
#define _GNU_SOURCE
#include "aidl.h"
#include <errno.h>
#include <stdlib.h>
#include <string.h>

#ifdef __linux__
#include <fcntl.h>
#include <linux/android/binder.h>
#include <poll.h>
#include <pthread.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#define MAP_SIZE (1024u * 1024u - 2u * 4096u)
#define CALL_TIMEOUT_MS 5000

struct ab {
  int fd;
  void *map;
  ab_incoming_fn incoming;
  ab_death_fn death;
  void *ctx;
  pthread_t looper;
  bool looping;
  int wake[2];
  pthread_mutex_t call_lock;       // one call at a time
};

static int bwrite(int fd, const void *data, size_t length)
{
  struct binder_write_read bwr = { .write_size = length, .write_buffer = (binder_uintptr_t)(uintptr_t)data };
  while (bwr.write_consumed < bwr.write_size) {
    if (ioctl(fd, BINDER_WRITE_READ, &bwr) < 0 && errno != EINTR && errno != EAGAIN) return -errno;
  }
  return 0;
}

static void free_buffer(int fd, binder_uintptr_t buffer)
{
  struct { uint32_t cmd; binder_uintptr_t buffer; } __attribute__((packed)) out = { BC_FREE_BUFFER, buffer };
  (void)bwrite(fd, &out, sizeof out);
}

static void send_parcel(int fd, uint32_t cmd, uint32_t handle, uint32_t code, uint32_t flags, const ab_parcel *p)
{
  struct { uint32_t cmd; struct binder_transaction_data tr; } __attribute__((packed)) out;
  memset(&out, 0, sizeof out);
  out.cmd = cmd;
  out.tr.target.handle = handle;
  out.tr.code = code;
  out.tr.flags = flags;
  out.tr.data_size = p->size;
  out.tr.offsets_size = p->nobjects * sizeof(binder_size_t);
  out.tr.data.ptr.buffer = (binder_uintptr_t)(uintptr_t)p->data;
  out.tr.data.ptr.offsets = (binder_uintptr_t)(uintptr_t)p->offsets;
  (void)bwrite(fd, &out, sizeof out);
}

static void reply_status(int fd, int32_t status)
{
  struct { uint32_t cmd; struct binder_transaction_data tr; } __attribute__((packed)) out;
  memset(&out, 0, sizeof out);
  out.cmd = BC_REPLY;
  out.tr.flags = TF_STATUS_CODE;
  out.tr.data_size = sizeof status;
  out.tr.data.ptr.buffer = (binder_uintptr_t)(uintptr_t)&status;
  (void)bwrite(fd, &out, sizeof out);
}

// A call into one of our objects.
static void incoming(ab *b, const struct binder_transaction_data *tr)
{
  ab_reader r;
  ab_parcel *reply = malloc(sizeof *reply);          // not on a thread's stack, not static: calls nest
  if (!reply) { free_buffer(b->fd, tr->data.ptr.buffer); if (!(tr->flags & TF_ONE_WAY)) reply_status(b->fd, -ENOMEM); return; }
  ab_init(reply);
  ab_reader_init(&r, (const void *)(uintptr_t)tr->data.ptr.buffer, tr->data_size, (const void *)(uintptr_t)tr->data.ptr.offsets, tr->offsets_size);
  int rc = b->incoming ? b->incoming(b->ctx, tr->cookie, tr->code, tr->flags & TF_ONE_WAY ? AB_ONE_WAY : 0, &r, reply) : AB_UNKNOWN_TRANSACTION;
  free_buffer(b->fd, tr->data.ptr.buffer);           // also lets the driver hand over the object's next one-way call
  if (!(tr->flags & TF_ONE_WAY)) {
    if (rc || reply->overflow) reply_status(b->fd, rc ? rc : AB_UNKNOWN_TRANSACTION);
    else send_parcel(b->fd, BC_REPLY, 0, 0, 0, reply);
  }
  free(reply);
}

// Reads what the driver has for this thread. reply != NULL: a call waits; *done becomes 1 when its reply came
// (copied to *reply, its buffer still to be freed) or -errno when the call failed. 0, or 1 when there was nothing.
static int bread(ab *b, struct binder_transaction_data *reply, int *done)
{
  uint32_t buffer[128];
  struct binder_write_read bwr = { .read_size = sizeof buffer, .read_buffer = (binder_uintptr_t)(uintptr_t)buffer };
  if (ioctl(b->fd, BINDER_WRITE_READ, &bwr) < 0) return errno == EAGAIN || errno == EINTR ? 1 : -errno;
  if (!bwr.read_consumed) return 1;
  const uint8_t *p = (const uint8_t *)buffer, *end = p + bwr.read_consumed;
  while (p + 4 <= end) {
    uint32_t cmd;
    memcpy(&cmd, p, 4);
    p += 4;
    size_t size = _IOC_SIZE(cmd);
    if (size > (size_t)(end - p)) break;
    switch (cmd) {
    case BR_INCREFS: case BR_ACQUIRE: {
      struct { uint32_t cmd; struct binder_ptr_cookie pc; } __attribute__((packed)) out;
      out.cmd = cmd == BR_INCREFS ? BC_INCREFS_DONE : BC_ACQUIRE_DONE;
      memcpy(&out.pc, p, sizeof out.pc);
      (void)bwrite(b->fd, &out, sizeof out);
      break;
    }
    case BR_TRANSACTION_SEC_CTX: case BR_TRANSACTION: {
      struct binder_transaction_data tr;
      memcpy(&tr, p, sizeof tr);
      incoming(b, &tr);
      break;
    }
    case BR_REPLY: {
      struct binder_transaction_data tr;
      memcpy(&tr, p, sizeof tr);
      if (reply && done && !*done) { *reply = tr; *done = 1; }
      else free_buffer(b->fd, tr.data.ptr.buffer);
      break;
    }
    case BR_DEAD_REPLY: case BR_FAILED_REPLY: case BR_FROZEN_REPLY:
      if (done && !*done) *done = cmd == BR_DEAD_REPLY ? -EPIPE : -EIO;
      break;
    case BR_DEAD_BINDER: {
      struct { uint32_t cmd; binder_uintptr_t cookie; } __attribute__((packed)) out = { BC_DEAD_BINDER_DONE, 0 };
      memcpy(&out.cookie, p, sizeof out.cookie);
      (void)bwrite(b->fd, &out, sizeof out);
      if (b->death) b->death(b->ctx, out.cookie);
      break;
    }
    default: break;                       // BR_NOOP, BR_TRANSACTION_COMPLETE, BR_SPAWN_LOOPER, BR_RELEASE, BR_DECREFS, ...
    }
    p += size;
  }
  return 0;
}

int ab_call(ab *b, uint32_t handle, uint32_t code, const ab_parcel *p, int32_t *specific, ab_reply_fn on_reply, void *reply_ctx)
{
  if (specific) *specific = 0;
  if (p->overflow) return -E2BIG;
  pthread_mutex_lock(&b->call_lock);
  send_parcel(b->fd, BC_TRANSACTION, handle, code, TF_ACCEPT_FDS, p);
  struct binder_transaction_data reply;
  int done = 0, rc = 0;
  while (!done) {
    rc = bread(b, &reply, &done);
    if (rc < 0) break;
    if (rc == 1 && !done) {
      struct pollfd wait = { .fd = b->fd, .events = POLLIN };
      if (poll(&wait, 1, CALL_TIMEOUT_MS) == 0) { rc = -ETIMEDOUT; break; }
    }
  }
  if (rc < 0 || done < 0) { pthread_mutex_unlock(&b->call_lock); return rc < 0 ? rc : done; }
  rc = 0;
  if (reply.flags & TF_STATUS_CODE) {
    int32_t status = -EIO;
    if (reply.data_size >= 4) memcpy(&status, (const void *)(uintptr_t)reply.data.ptr.buffer, 4);
    rc = status < 0 ? status : -EIO;
    if (rc == AB_UNKNOWN_TRANSACTION) rc = -ENOSYS;
    else if (rc < -4095) rc = -EPROTO;               // android's own status values
  } else {
    ab_reader r;
    ab_reader_init(&r, (const void *)(uintptr_t)reply.data.ptr.buffer, reply.data_size, (const void *)(uintptr_t)reply.data.ptr.offsets, reply.offsets_size);
    if (code == AB_INTERFACE || code == AB_PING) rc = on_reply ? on_reply(reply_ctx, &r) : 0;   // no status header
    else {
      int32_t exception = ab_get_status(&r, specific);
      if (r.bad) rc = -EPROTO;
      else if (exception) rc = -EREMOTEIO;
      else if (on_reply) rc = on_reply(reply_ctx, &r);
    }
  }
  free_buffer(b->fd, reply.data.ptr.buffer);
  pthread_mutex_unlock(&b->call_lock);
  return rc;
}

int ab_oneway(ab *b, uint32_t handle, uint32_t code, const ab_parcel *p)
{
  if (p->overflow) return -E2BIG;
  pthread_mutex_lock(&b->call_lock);
  send_parcel(b->fd, BC_TRANSACTION, handle, code, TF_ONE_WAY | TF_ACCEPT_FDS, p);
  pthread_mutex_unlock(&b->call_lock);
  return 0;
}

int ab_acquire(ab *b, uint32_t handle)
{
  uint32_t out[4] = { BC_INCREFS, handle, BC_ACQUIRE, handle };
  return bwrite(b->fd, out, sizeof out);
}

void ab_release(ab *b, uint32_t handle)
{
  uint32_t out[4] = { BC_RELEASE, handle, BC_DECREFS, handle };
  (void)bwrite(b->fd, out, sizeof out);
}

int ab_watch(ab *b, uint32_t handle, uint64_t cookie)
{
  struct { uint32_t cmd, handle; binder_uintptr_t cookie; } __attribute__((packed)) out = { BC_REQUEST_DEATH_NOTIFICATION, handle, cookie };
  return bwrite(b->fd, &out, sizeof out);
}

static void *looper(void *arg)
{
  ab *b = arg;
  uint32_t enter = BC_ENTER_LOOPER;
  (void)bwrite(b->fd, &enter, sizeof enter);
  struct pollfd fds[2] = { { .fd = b->fd, .events = POLLIN }, { .fd = b->wake[0], .events = POLLIN } };
  for (;;) {
    if (poll(fds, 2, -1) < 0 && errno != EINTR) break;
    if (fds[1].revents & (POLLIN | POLLHUP)) break;
    if (fds[0].revents & POLLIN) while (bread(b, NULL, NULL) == 0) { }
  }
  uint32_t leave = BC_EXIT_LOOPER;
  (void)bwrite(b->fd, &leave, sizeof leave);
  return NULL;
}

ab *ab_open(const char *device, ab_incoming_fn incoming_fn, ab_death_fn death, void *ctx)
{
  const char *env = getenv("ONEIOS_BINDER");         // the PC test's binderfs; the phone's is /dev/binder
  if (!device) device = env && *env ? env : "/dev/binder";
  ab *b = calloc(1, sizeof *b);
  if (!b) return NULL;
  struct binder_version version = { 0 };
  uint32_t threads = 0;
  b->fd = open(device, O_RDWR | O_CLOEXEC | O_NONBLOCK);
  if (b->fd < 0) { int e = errno; free(b); errno = e; return NULL; }
  if (ioctl(b->fd, BINDER_VERSION, &version) < 0 || version.protocol_version != BINDER_CURRENT_PROTOCOL_VERSION ||
      (b->map = mmap(NULL, MAP_SIZE, PROT_READ, MAP_PRIVATE | MAP_NORESERVE, b->fd, 0)) == MAP_FAILED ||
      ioctl(b->fd, BINDER_SET_MAX_THREADS, &threads) < 0 || pipe2(b->wake, O_CLOEXEC)) {
    int e = errno ? errno : EPROTO;
    close(b->fd);
    free(b);
    errno = e;
    return NULL;
  }
  b->incoming = incoming_fn;
  b->death = death;
  b->ctx = ctx;
  pthread_mutex_init(&b->call_lock, NULL);
  b->looping = pthread_create(&b->looper, NULL, looper, b) == 0;
  return b;
}

int ab_context_manager(ab *b)
{
  for (int i = 0; i < 40; i++) {                      // a context manager that just died is freed in deferred work
    if (ioctl(b->fd, BINDER_SET_CONTEXT_MGR, 0) == 0) return 0;
    if (errno != EBUSY) return -errno;
    usleep(50000);
  }
  return -EBUSY;
}

void ab_close(ab *b)
{
  if (!b) return;
  if (b->looping) {
    (void)!write(b->wake[1], "", 1);
    pthread_join(b->looper, NULL);
  }
  munmap(b->map, MAP_SIZE);
  close(b->fd);                                       // the driver drops our objects and references
  close(b->wake[0]);
  close(b->wake[1]);
  pthread_mutex_destroy(&b->call_lock);
  free(b);
}
#else
ab  *ab_open(const char *device, ab_incoming_fn incoming, ab_death_fn death, void *ctx) { (void)device; (void)incoming; (void)death; (void)ctx; errno = ENOSYS; return NULL; }
int  ab_context_manager(ab *b) { (void)b; return -ENOSYS; }
int  ab_call(ab *b, uint32_t handle, uint32_t code, const ab_parcel *p, int32_t *specific, ab_reply_fn on_reply, void *reply_ctx)
{ (void)b; (void)handle; (void)code; (void)p; (void)specific; (void)on_reply; (void)reply_ctx; return -ENOSYS; }
int  ab_oneway(ab *b, uint32_t handle, uint32_t code, const ab_parcel *p) { (void)b; (void)handle; (void)code; (void)p; return -ENOSYS; }
int  ab_acquire(ab *b, uint32_t handle) { (void)b; (void)handle; return -ENOSYS; }
void ab_release(ab *b, uint32_t handle) { (void)b; (void)handle; }
int  ab_watch(ab *b, uint32_t handle, uint64_t cookie) { (void)b; (void)handle; (void)cookie; return -ENOSYS; }
void ab_close(ab *b) { (void)b; }
#endif
