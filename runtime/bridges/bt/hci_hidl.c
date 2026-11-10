// SPDX-License-Identifier: MIT
// Bluetooth back end: the phone's Bluetooth HAL, spoken to directly over /dev/hwbinder.
//
// On the Galaxy S24 Ultra the controller is a Qualcomm WCN785x ("hamilton", DT node bt_kiwi) behind
// the HIDL service android.hardware.bluetooth@1.1::IBluetoothHci/default (init service
// vendor.bluetooth-1-1-qti). The HAL owns power, the UART and the firmware download; its only client
// is Android's Bluetooth app, which is gone in a session (it is a zygote child), so the HAL is free.
// This file is that client: IServiceManager::get, initialize(callbacks), sendHciCommand/AclData/
// ScoData/IsoData, and a callback object the HAL calls with events and data.
//
// No Android library is linked (xnuhost is static): the binder protocol is used through the kernel's
// UAPI and the parcels are built by hidl_parcel.c. Needs root (KernelSU's domain): hwservicemanager
// checks the caller's SELinux context for "find", and the HAL must be allowed to call back.
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include "hidl_parcel.h"
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
#define DEATH_COOKIE 0x4f6e6942ull

typedef struct {
  int fd;
  void *map;
  br_hci *hci;
  pthread_t looper;
  int command[2];                 // pipe: 'o' open, 'c' close, 'q' quit
  pthread_mutex_t lock;           // service handle
  uint32_t service;
  bool have_service, v1_1;
  bool initialized;
} hidl_bt;

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

static void reply_parcel(int fd, const hb_parcel *p)
{
  struct { uint32_t cmd; struct binder_transaction_data_sg sg; } __attribute__((packed)) out;
  memset(&out, 0, sizeof out);
  out.cmd = BC_REPLY_SG;
  out.sg.transaction_data.data_size = p->size;
  out.sg.transaction_data.offsets_size = p->nobjects * sizeof(binder_size_t);
  out.sg.transaction_data.data.ptr.buffer = (binder_uintptr_t)(uintptr_t)p->data;
  out.sg.transaction_data.data.ptr.offsets = (binder_uintptr_t)(uintptr_t)p->offsets;
  out.sg.buffers_size = p->buffers_size;
  (void)bwrite(fd, &out, sizeof out);
}

// A transaction for our callback object.
static void incoming(hidl_bt *b, const struct binder_transaction_data *tr)
{
  hb_reader r;
  static _Thread_local hb_parcel reply;
  uint8_t type;
  const uint8_t *packet;
  size_t length;
  int32_t status;
  hb_reader_init(&r, (const void *)(uintptr_t)tr->data.ptr.buffer, tr->data_size, (const void *)(uintptr_t)tr->data.ptr.offsets, tr->offsets_size);
  int rc = bt_callback_transact(tr->code, &r, &reply, &type, &packet, &length, &status);
  if (!rc && type) br_hci_deliver(b->hci, type, packet, length);        // before the buffer is freed
  if (!rc && tr->code == BT_CB_INIT_COMPLETE) {
    b->initialized = status == 0;
    br_log(status ? 0 : 1, "bluetooth: HAL initialisation %s (status %d)", status ? "failed" : "complete", status);
    br_hci_set_state(b->hci, status == 0 ? 1 : 2);
  }
  free_buffer(b->fd, tr->data.ptr.buffer);
  if (tr->flags & TF_ONE_WAY) return;
  if (rc || reply.overflow) reply_status(b->fd, rc ? rc : HB_UNKNOWN_TRANSACTION);
  else reply_parcel(b->fd, &reply);
}

// Reads what the driver has for this thread. reply != NULL: a call is waiting; *done becomes 1 when
// its reply came (copied to *reply, its buffer still to be freed by the caller) or -errno when the
// call failed. Returns 0, or 1 when there was nothing to read.
static int bread(hidl_bt *b, struct binder_transaction_data *reply, int *done)
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
      pthread_mutex_lock(&b->lock);
      b->have_service = false;
      pthread_mutex_unlock(&b->lock);
      b->initialized = false;
      br_log(0, "bluetooth: the HAL process died");
      br_hci_set_state(b->hci, 2);
      break;
    }
    default: break;                       // BR_NOOP, BR_TRANSACTION_COMPLETE, BR_SPAWN_LOOPER, BR_RELEASE, BR_DECREFS, ...
    }
    p += size;
  }
  return 0;
}

// A synchronous call. on_reply sees the reply before its buffer is released. Returns 0 or -errno
// (a status-code reply gives that code).
static int call(hidl_bt *b, uint32_t handle, uint32_t code, const hb_parcel *p, int (*on_reply)(hidl_bt *, hb_reader *))
{
  if (p->overflow) return -E2BIG;
  struct { uint32_t cmd; struct binder_transaction_data_sg sg; } __attribute__((packed)) out;
  memset(&out, 0, sizeof out);
  out.cmd = BC_TRANSACTION_SG;
  out.sg.transaction_data.target.handle = handle;
  out.sg.transaction_data.code = code;
  out.sg.transaction_data.flags = TF_ACCEPT_FDS;
  out.sg.transaction_data.data_size = p->size;
  out.sg.transaction_data.offsets_size = p->nobjects * sizeof(binder_size_t);
  out.sg.transaction_data.data.ptr.buffer = (binder_uintptr_t)(uintptr_t)p->data;
  out.sg.transaction_data.data.ptr.offsets = (binder_uintptr_t)(uintptr_t)p->offsets;
  out.sg.buffers_size = p->buffers_size;
  int rc = bwrite(b->fd, &out, sizeof out);
  if (rc) return rc;
  struct binder_transaction_data reply;
  int done = 0;
  while (!done) {
    rc = bread(b, &reply, &done);
    if (rc < 0) return rc;
    if (rc == 1 && !done) {
      struct pollfd wait = { .fd = b->fd, .events = POLLIN };
      if (poll(&wait, 1, CALL_TIMEOUT_MS) == 0) return -ETIMEDOUT;
    }
  }
  if (done < 0) return done;
  rc = 0;
  if (reply.flags & TF_STATUS_CODE) {
    int32_t status = -EIO;
    if (reply.data_size >= 4) memcpy(&status, (const void *)(uintptr_t)reply.data.ptr.buffer, 4);
    rc = status ? status : -EIO;
  } else {
    hb_reader r;
    hb_reader_init(&r, (const void *)(uintptr_t)reply.data.ptr.buffer, reply.data_size, (const void *)(uintptr_t)reply.data.ptr.offsets, reply.offsets_size);
    int32_t exception = hb_get_i32(&r);
    if (r.bad || exception) rc = -EPROTO;
    else if (on_reply) rc = on_reply(b, &r);
  }
  free_buffer(b->fd, reply.data.ptr.buffer);
  return rc;
}

static int take_service(hidl_bt *b, hb_reader *r)
{
  uint32_t handle;
  if (!hb_get_handle(r, &handle)) return -ENOENT;
  // Our own references, taken before the reply buffer (which holds the transaction's) is freed.
  struct { uint32_t c1, h1, c2, h2, c3, h3; binder_uintptr_t cookie; } __attribute__((packed)) out =
    { BC_INCREFS, handle, BC_ACQUIRE, handle, BC_REQUEST_DEATH_NOTIFICATION, handle, DEATH_COOKIE };
  int rc = bwrite(b->fd, &out, sizeof out);
  if (rc) return rc;
  pthread_mutex_lock(&b->lock);
  b->service = handle;
  b->have_service = true;
  pthread_mutex_unlock(&b->lock);
  return 0;
}

static void drop_service(hidl_bt *b)
{
  pthread_mutex_lock(&b->lock);
  bool had = b->have_service;
  uint32_t handle = b->service;
  b->have_service = false;
  pthread_mutex_unlock(&b->lock);
  if (!had) return;
  struct { uint32_t c0, h0; binder_uintptr_t cookie; uint32_t c1, h1, c2, h2; } __attribute__((packed)) out =
    { BC_CLEAR_DEATH_NOTIFICATION, handle, DEATH_COOKIE, BC_RELEASE, handle, BC_DECREFS, handle };
  (void)bwrite(b->fd, &out, sizeof out);
}

static void connect_hal(hidl_bt *b)
{
  static _Thread_local hb_parcel p;
  // The 1.1 name first: a 1.1 service is registered under both, a 1.0 one only under the second.
  b->v1_1 = true;
  bt_parcel_get_service(&p, BT_HCI_1_1, "default");
  int rc = call(b, 0, 1 /* IServiceManager::get */, &p, take_service);
  if (rc) {
    b->v1_1 = false;
    bt_parcel_get_service(&p, BT_HCI_1_0, "default");
    rc = call(b, 0, 1, &p, take_service);
  }
  if (rc) {
    br_log(0, "bluetooth: off: no android.hardware.bluetooth@1.x::IBluetoothHci/default from hwservicemanager: %s%s", strerror(rc < 0 ? -rc : rc),
           rc == -ENOENT ? " (not registered, or SELinux denies find to this domain)" : "");
    br_hci_set_state(b->hci, 2);
    return;
  }
  bt_parcel_initialize(&p, b->v1_1, (uint64_t)(uintptr_t)b);
  rc = call(b, b->service, b->v1_1 ? BT_INITIALIZE_1_1 : BT_INITIALIZE, &p, NULL);
  if (rc) {
    br_log(0, "bluetooth: off: IBluetoothHci::initialize%s: %s", b->v1_1 ? "_1_1" : "", strerror(rc < 0 ? -rc : rc));
    drop_service(b);
    br_hci_set_state(b->hci, 2);
    return;
  }
  br_log(1, "bluetooth: HAL %s found, initialising (power and firmware are the HAL's)", b->v1_1 ? "1.1" : "1.0");
}

static void disconnect_hal(hidl_bt *b)
{
  static _Thread_local hb_parcel p;
  pthread_mutex_lock(&b->lock);
  bool have = b->have_service;
  uint32_t handle = b->service;
  pthread_mutex_unlock(&b->lock);
  if (!have) return;
  hb_parcel_init(&p);
  hb_put_token(&p, BT_HCI_1_0);
  (void)call(b, handle, BT_CLOSE, &p, NULL);
  b->initialized = false;
  drop_service(b);
}

static void *looper(void *arg)
{
  hidl_bt *b = arg;
  uint32_t enter = BC_ENTER_LOOPER;
  (void)bwrite(b->fd, &enter, sizeof enter);
  struct pollfd fds[2] = { { .fd = b->fd, .events = POLLIN }, { .fd = b->command[0], .events = POLLIN } };
  for (;;) {
    if (poll(fds, 2, -1) < 0 && errno != EINTR) break;
    if (fds[1].revents & POLLIN) {
      char c = 0;
      if (read(b->command[0], &c, 1) != 1) break;
      if (c == 'o') connect_hal(b);
      else if (c == 'c') disconnect_hal(b);
      else if (c == 'q') { disconnect_hal(b); break; }
    }
    if (fds[0].revents & POLLIN) while (bread(b, NULL, NULL) == 0) { }
  }
  uint32_t leave = BC_EXIT_LOOPER;
  (void)bwrite(b->fd, &leave, sizeof leave);
  return NULL;
}

static int hidl_open(void *ctx) { hidl_bt *b = ctx; return write(b->command[1], "o", 1) == 1 ? 0 : -errno; }
static void hidl_close(void *ctx) { hidl_bt *b = ctx; (void)!write(b->command[1], "c", 1); }

static int hidl_send(void *ctx, uint8_t type, const uint8_t *packet, size_t length)
{
  hidl_bt *b = ctx;
  static _Thread_local hb_parcel p;
  static const uint8_t code[] = { [1] = BT_SEND_COMMAND, [2] = BT_SEND_ACL, [3] = BT_SEND_SCO, [5] = BT_SEND_ISO };
  pthread_mutex_lock(&b->lock);
  bool have = b->have_service;
  uint32_t handle = b->service;
  pthread_mutex_unlock(&b->lock);
  if (!have || type > 5 || !code[type] || (type == 5 && !b->v1_1)) return -ENOTCONN;
  bt_parcel_send(&p, code[type], packet, length);
  return call(b, handle, code[type], &p, NULL);
}

static const br_hci_backend hidl_backend = { .open = hidl_open, .send = hidl_send, .close = hidl_close };

br_hci *br_hci_open_hidl(br_hci_rx_fn rx, br_hci_state_fn state, void *ctx)
{
  hidl_bt *b = calloc(1, sizeof *b);
  if (!b) return NULL;
  struct binder_version version = { 0 };
  uint32_t threads = 0;
  b->fd = open("/dev/hwbinder", O_RDWR | O_CLOEXEC | O_NONBLOCK);
  if (b->fd < 0) { br_log(0, "bluetooth: off: /dev/hwbinder: %s", strerror(errno)); free(b); return NULL; }
  if (ioctl(b->fd, BINDER_VERSION, &version) < 0 || version.protocol_version != BINDER_CURRENT_PROTOCOL_VERSION ||
      (b->map = mmap(NULL, MAP_SIZE, PROT_READ, MAP_PRIVATE | MAP_NORESERVE, b->fd, 0)) == MAP_FAILED ||
      ioctl(b->fd, BINDER_SET_MAX_THREADS, &threads) < 0 || pipe(b->command)) {
    br_log(0, "bluetooth: off: hwbinder setup (protocol %d): %s", version.protocol_version, strerror(errno));
    close(b->fd); free(b);
    return NULL;
  }
  pthread_mutex_init(&b->lock, NULL);
  b->hci = br_hci_create(&hidl_backend, b, rx, state, ctx);
  if (!b->hci || pthread_create(&b->looper, NULL, looper, b)) { br_log(0, "bluetooth: off: no looper thread"); close(b->fd); free(b); return NULL; }
  return b->hci;
}
#else
br_hci *br_hci_open_hidl(br_hci_rx_fn rx, br_hci_state_fn state, void *ctx) { (void)rx; (void)state; (void)ctx; return NULL; }
#endif
