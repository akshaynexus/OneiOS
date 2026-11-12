// SPDX-License-Identifier: MIT
// The Bluetooth HAL client (bt/hci_hidl.c) against a real binder driver: a private binderfs on the
// build host (the PC's kernel has CONFIG_ANDROID_BINDERFS; `make test-binder` mounts one in a user
// namespace, no root needed). A child process plays hwservicemanager (the context manager) and
// android.hardware.bluetooth@1.1::IBluetoothHci/default, and reads every parcel the way libhwbinder
// does (readCString tokens, readBuffer/readEmbeddedBuffer with their parent checks), independently
// of hidl_parcel.c's reader. It calls back like the QTI HAL: from its own thread, two-way, and for a
// command before it answers the command (a HAL that holds a lock across the callback).
//
// Scenarios: a 1.1 HAL (probe, open, interfaceChain on our callback, initializationComplete, HCI Reset
// and Read BD_ADDR answered with Command Complete, ACL echoed, close); a 1.0-only HAL; no HAL
// registered; a HAL that never completes initialisation (the timeout); a HAL that dies.
// Not tested here: the real libhwbinder, SELinux, the QTI HAL itself. That is `bridgectl bt` on the phone.
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include "../bt/hidl_parcel.h"
#include <errno.h>
#include <fcntl.h>
#include <linux/android/binder.h>
#include <linux/android/binderfs.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int failures, checks;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); fflush(stdout); } } while (0)

static const char *device;
static void log_line(int level, const char *line) { printf("  [%s] %s\n", level == 0 ? "E" : level == 1 ? "I" : "D", line); fflush(stdout); }

// ================================ the fake hwservicemanager + HAL (child) ================================
enum { MODE_OK, MODE_V10, MODE_ABSENT, MODE_SILENT };
#define HAL_PTR 0x48414cull

static int sfd;
static int smode;
static int sfail;
#define SCHECK(cond) do { if (!(cond)) { sfail++; printf("FAKE HAL FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); fflush(stdout); } } while (0)

static void swrite(const void *data, size_t length)
{
  struct binder_write_read bwr = { .write_size = length, .write_buffer = (binder_uintptr_t)(uintptr_t)data };
  while (bwr.write_consumed < bwr.write_size)
    if (ioctl(sfd, BINDER_WRITE_READ, &bwr) < 0 && errno != EINTR) { perror("fake hal: write"); _exit(90); }
}

// A parcel being read, with libhwbinder's checks.
typedef struct { const uint8_t *d; size_t n, pos; const binder_size_t *off; size_t noff, next; bool bad; } rd;
static void rd_init(rd *r, const struct binder_transaction_data *tr)
{
  *r = (rd){ (const uint8_t *)(uintptr_t)tr->data.ptr.buffer, tr->data_size, 0, (const binder_size_t *)(uintptr_t)tr->data.ptr.offsets,
             tr->offsets_size / sizeof(binder_size_t), 0, false };
}
static const char *rd_cstr(rd *r)
{
  if (r->bad || r->pos >= r->n) { r->bad = true; return ""; }
  const char *s = (const char *)r->d + r->pos;
  size_t len = strnlen(s, r->n - r->pos);
  if (len == r->n - r->pos) { r->bad = true; return ""; }
  r->pos += (len + 1 + 3) & ~3ul;
  return s;
}
static int32_t rd_i32(rd *r)
{
  int32_t v = 0;
  if (r->bad || r->pos + 4 > r->n) { r->bad = true; return 0; }
  memcpy(&v, r->d + r->pos, 4);
  r->pos += 4;
  return v;
}
// readObject: the object at the read position must be the next one in the offsets.
static const void *rd_object(rd *r, size_t size)
{
  if (r->bad || r->next >= r->noff || r->off[r->next] != r->pos || r->pos + size > r->n) { r->bad = true; return NULL; }
  const void *o = r->d + r->pos;
  r->pos += size;
  r->next++;
  return o;
}
// readBuffer (parent < 0) / readEmbeddedBuffer: type PTR, the length asked for, the parent link.
// Objects may sit at any 4-byte offset (the driver's rule), so they are copied out.
static bool rd_buffer(rd *r, size_t length, long parent, size_t parent_offset, size_t *index, struct binder_buffer_object *b)
{
  *index = r->next;
  const void *at = rd_object(r, sizeof *b);
  if (!at) return false;
  memcpy(b, at, sizeof *b);
  bool ok = b->hdr.type == BINDER_TYPE_PTR && b->length == length;
  if (parent < 0) ok = ok && !(b->flags & BINDER_BUFFER_FLAG_HAS_PARENT);
  else {
    ok = ok && (b->flags & BINDER_BUFFER_FLAG_HAS_PARENT) && b->parent == (binder_size_t)parent && b->parent_offset == parent_offset;
    // libhwbinder: the parent's pointer at that offset is this buffer (the driver fixed it up)
    struct binder_buffer_object p;
    memcpy(&p, r->d + r->off[parent], sizeof p);
    uint64_t pointer;
    memcpy(&pointer, (const uint8_t *)(uintptr_t)p.buffer + parent_offset, 8);
    ok = ok && pointer == b->buffer;
  }
  if (!ok) r->bad = true;
  return ok;
}
static bool rd_vec(rd *r, const uint8_t **data, size_t *length, bool string)
{
  size_t top_index, child_index;
  struct binder_buffer_object top, child;
  if (!rd_buffer(r, 16, -1, 0, &top_index, &top)) return false;
  struct hb_hidl_ref ref;
  memcpy(&ref, (const void *)(uintptr_t)top.buffer, sizeof ref);
  if (!rd_buffer(r, ref.size + (string ? 1u : 0u), (long)top_index, 0, &child_index, &child)) return false;
  *data = (const uint8_t *)(uintptr_t)child.buffer;
  *length = ref.size;
  if (string && (*data)[ref.size]) { r->bad = true; return false; }
  return true;
}

// A parcel being written.
typedef struct { uint8_t d[1024]; size_t n; binder_size_t off[8]; size_t noff; uint64_t buffers; struct hb_hidl_ref refs[4]; unsigned nref; } wr;
static void wr_bytes(wr *w, const void *p, size_t n) { memcpy(w->d + w->n, p, n); memset(w->d + w->n + n, 0, ((n + 3) & ~3ul) - n); w->n += (n + 3) & ~3ul; }
static void wr_object(wr *w, const void *o, size_t n) { w->off[w->noff++] = w->n; wr_bytes(w, o, n); }
static void wr_i32(wr *w, int32_t v) { wr_bytes(w, &v, 4); }
static void wr_vec(wr *w, const uint8_t *data, size_t length)
{
  struct hb_hidl_ref *ref = &w->refs[w->nref++];
  *ref = (struct hb_hidl_ref){ (uint64_t)(uintptr_t)data, (uint32_t)length, 0, { 0 } };
  size_t parent = w->noff;
  struct binder_buffer_object top = { .hdr.type = BINDER_TYPE_PTR, .buffer = (uintptr_t)ref, .length = sizeof *ref };
  wr_object(w, &top, sizeof top);
  struct binder_buffer_object child = { .hdr.type = BINDER_TYPE_PTR, .flags = BINDER_BUFFER_FLAG_HAS_PARENT, .buffer = (uintptr_t)data,
                                        .length = length, .parent = parent, .parent_offset = 0 };
  wr_object(w, &child, sizeof child);
  w->buffers += 16 + ((length + 7) & ~7ul);
}
static struct binder_transaction_data_sg make_sg(const wr *w, uint32_t handle, uint32_t code, uint32_t flags)
{
  struct binder_transaction_data_sg sg;
  memset(&sg, 0, sizeof sg);
  sg.transaction_data.target.handle = handle;
  sg.transaction_data.code = code;
  sg.transaction_data.flags = flags;
  sg.transaction_data.data_size = w->n;
  sg.transaction_data.offsets_size = w->noff * sizeof(binder_size_t);
  sg.transaction_data.data.ptr.buffer = (uintptr_t)w->d;
  sg.transaction_data.data.ptr.offsets = (uintptr_t)w->off;
  sg.buffers_size = w->buffers;
  return sg;
}
static void sreply(const wr *w)
{
  struct { uint32_t cmd; struct binder_transaction_data_sg sg; } __attribute__((packed)) out;
  memset(&out, 0, sizeof out);
  out.cmd = BC_REPLY_SG;
  out.sg = make_sg(w, 0, 0, 0);
  swrite(&out, sizeof out);
}
static void sfree(binder_uintptr_t buffer)
{
  struct { uint32_t cmd; binder_uintptr_t b; } __attribute__((packed)) out = { BC_FREE_BUFFER, buffer };
  swrite(&out, sizeof out);
}

// ---- the HAL's reader thread: callbacks into the client, two-way, one at a time ----
typedef struct { uint32_t code; uint8_t data[64]; size_t length; } job;
static pthread_mutex_t jlock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t jcond = PTHREAD_COND_INITIALIZER;
static job jobs[16];
static unsigned jhead, jcount, jdone, jposted;
static uint32_t cb_handle;
static bool have_cb;

// One two-way call to the client's callback; true when a normal reply came. `check` sees the reply.
static bool scall(uint32_t code, const wr *w, bool (*check)(rd *))
{
  struct { uint32_t cmd; struct binder_transaction_data_sg sg; } __attribute__((packed)) out;
  memset(&out, 0, sizeof out);
  out.cmd = BC_TRANSACTION_SG;
  out.sg = make_sg(w, cb_handle, code, TF_ACCEPT_FDS);
  swrite(&out, sizeof out);
  for (;;) {
    uint32_t buf[64];
    struct binder_write_read bwr = { .read_size = sizeof buf, .read_buffer = (uintptr_t)buf };
    if (ioctl(sfd, BINDER_WRITE_READ, &bwr) < 0) { if (errno == EINTR) continue; return false; }
    for (const uint8_t *p = (const uint8_t *)buf, *end = p + bwr.read_consumed; p + 4 <= end;) {
      uint32_t cmd;
      memcpy(&cmd, p, 4);
      p += 4;
      if (cmd == BR_REPLY) {
        struct binder_transaction_data tr;
        memcpy(&tr, p, sizeof tr);
        bool ok = !(tr.flags & TF_STATUS_CODE);
        if (ok) {
          rd r;
          rd_init(&r, &tr);
          ok = rd_i32(&r) == 0 && !r.bad && (!check || check(&r));
        }
        sfree(tr.data.ptr.buffer);
        return ok;
      }
      if (cmd == BR_DEAD_REPLY || cmd == BR_FAILED_REPLY) return false;
      p += _IOC_SIZE(cmd);
    }
  }
}

static bool check_chain(rd *r)
{
  size_t top, array, index;
  struct binder_buffer_object v, a, b;
  if (!rd_buffer(r, 16, -1, 0, &top, &v)) return false;
  struct hb_hidl_ref vec, items[3];
  memcpy(&vec, (const void *)(uintptr_t)v.buffer, sizeof vec);
  if (vec.size != 3 || !rd_buffer(r, 3 * 16, (long)top, 0, &array, &a)) return false;
  memcpy(items, (const void *)(uintptr_t)a.buffer, sizeof items);
  static const char *const want[] = { BT_CALLBACKS_1_1, BT_CALLBACKS_1_0, HIDL_BASE };
  for (unsigned i = 0; i < 3; i++)
    if (!rd_buffer(r, items[i].size + 1u, (long)array, i * 16u, &index, &b) || strcmp((const char *)(uintptr_t)b.buffer, want[i])) return false;
  return !r->bad;
}

static void *reader(void *arg)
{
  (void)arg;
  for (;;) {
    pthread_mutex_lock(&jlock);
    while (!jcount) pthread_cond_wait(&jcond, &jlock);
    job j = jobs[jhead];
    jhead = (jhead + 1) % 16;
    jcount--;
    pthread_mutex_unlock(&jlock);
    wr w = { .n = 0 };
    bool ok;
    if (j.code == HB_INTERFACE_CHAIN) { wr_bytes(&w, HIDL_BASE, strlen(HIDL_BASE) + 1); ok = scall(j.code, &w, check_chain); }
    else if (j.code == BT_CB_INIT_COMPLETE) { wr_bytes(&w, BT_CALLBACKS_1_0, strlen(BT_CALLBACKS_1_0) + 1); wr_i32(&w, 0); ok = scall(j.code, &w, NULL); }
    else { wr_bytes(&w, BT_CALLBACKS_1_0, strlen(BT_CALLBACKS_1_0) + 1); wr_vec(&w, j.data, j.length); ok = scall(j.code, &w, NULL); }
    SCHECK(ok);
    printf("  fake HAL: callback %#x answered: %s\n", j.code, ok ? "yes" : "NO");
    fflush(stdout);
    pthread_mutex_lock(&jlock);
    jdone++;
    pthread_cond_broadcast(&jcond);
    pthread_mutex_unlock(&jlock);
  }
  return NULL;
}

static unsigned post(uint32_t code, const uint8_t *data, size_t length)
{
  pthread_mutex_lock(&jlock);
  job *j = &jobs[(jhead + jcount) % 16];
  j->code = code;
  j->length = length;
  if (length) memcpy(j->data, data, length);
  jcount++;
  unsigned ticket = ++jposted;
  pthread_cond_broadcast(&jcond);
  pthread_mutex_unlock(&jlock);
  return ticket;
}
static void wait_job(unsigned ticket)
{
  pthread_mutex_lock(&jlock);
  while (jdone < ticket) pthread_cond_wait(&jcond, &jlock);
  pthread_mutex_unlock(&jlock);
}

static void serve(const struct binder_transaction_data *tr)
{
  rd r;
  rd_init(&r, tr);
  wr w = { .n = 0 };
  bool reply = !(tr->flags & TF_ONE_WAY);
  if (tr->target.ptr == 0) {                                      // hwservicemanager
    SCHECK(tr->code == 1);
    SCHECK(!strcmp(rd_cstr(&r), HIDL_MANAGER));
    const uint8_t *fq, *instance;
    size_t fq_n, instance_n;
    SCHECK(rd_vec(&r, &fq, &fq_n, true) && rd_vec(&r, &instance, &instance_n, true) && !r.bad);
    SCHECK(!strcmp((const char *)instance, "default"));
    bool is11 = !strcmp((const char *)fq, BT_HCI_1_1), is10 = !strcmp((const char *)fq, BT_HCI_1_0);
    SCHECK(is11 || is10);
    bool registered = smode == MODE_ABSENT ? false : smode == MODE_V10 ? is10 : true;
    printf("  fake hwservicemanager: get %s/%s: %s\n", fq, instance, registered ? "the HAL" : "null");
    wr_i32(&w, 0);
    struct flat_binder_object o = { .hdr.type = BINDER_TYPE_BINDER, .flags = 0x17f, .binder = registered ? HAL_PTR : 0, .cookie = registered ? HAL_PTR : 0 };
    if (registered) wr_object(&w, &o, sizeof o);
    else wr_bytes(&w, &o, sizeof o);                               // libhwbinder: a null binder has no offset entry
  } else {                                                         // IBluetoothHci
    SCHECK(tr->target.ptr == HAL_PTR);
    const char *token = rd_cstr(&r);
    const uint8_t *data;
    size_t n;
    switch (tr->code) {
    case BT_INITIALIZE_1_1: case BT_INITIALIZE: {
      SCHECK(!strcmp(token, tr->code == BT_INITIALIZE_1_1 ? BT_HCI_1_1 : BT_HCI_1_0));
      SCHECK(smode != MODE_V10 || tr->code == BT_INITIALIZE);
      const void *at = rd_object(&r, sizeof(struct flat_binder_object));
      struct flat_binder_object o = { 0 };
      if (at) memcpy(&o, at, sizeof o);
      SCHECK(at && o.hdr.type == BINDER_TYPE_HANDLE);
      if (at && o.hdr.type == BINDER_TYPE_HANDLE) {
        cb_handle = o.handle;
        have_cb = true;
        struct { uint32_t c1, h1, c2, h2; } __attribute__((packed)) refs = { BC_INCREFS, cb_handle, BC_ACQUIRE, cb_handle };
        swrite(&refs, sizeof refs);
      }
      printf("  fake HAL: initialize%s\n", tr->code == BT_INITIALIZE_1_1 ? "_1_1" : "");
      if (smode != MODE_SILENT && have_cb) { post(HB_INTERFACE_CHAIN, NULL, 0); post(BT_CB_INIT_COMPLETE, NULL, 0); }
      break;
    }
    case BT_SEND_COMMAND: {
      SCHECK(!strcmp(token, BT_HCI_1_0) && rd_vec(&r, &data, &n, false) && n >= 3 && n == 3u + data[2]);
      uint16_t opcode = (uint16_t)(data[0] | data[1] << 8);
      uint8_t event[16] = { 0x0e, 4, 1, data[0], data[1], 0 };
      size_t length = 6;
      if (opcode == 0x1009) { event[1] = 10; memcpy(event + 6, "\x66\x55\x44\x33\x22\x11", 6); length = 12; }   // Read BD_ADDR
      printf("  fake HAL: command %#06x\n", opcode);
      wait_job(post(BT_CB_EVENT, event, length));                 // the event goes out before the command returns
      break;
    }
    case BT_SEND_ACL:
      SCHECK(!strcmp(token, BT_HCI_1_0) && rd_vec(&r, &data, &n, false));
      post(BT_CB_ACL, data, n);
      break;
    case BT_CLOSE:
      SCHECK(!strcmp(token, BT_HCI_1_0));
      printf("  fake HAL: close\n");
      if (reply) { wr_i32(&w, 0); sreply(&w); }
      sfree(tr->data.ptr.buffer);
      fflush(stdout);
      _exit(sfail ? 1 : 0);
    default:
      SCHECK(!"unknown HAL code");
    }
    SCHECK(!r.bad);
    wr_i32(&w, 0);
  }
  sfree(tr->data.ptr.buffer);
  if (reply) sreply(&w);
}

static void fake_hal(int mode, int ready)
{
  smode = mode;
  sfd = open(device, O_RDWR | O_CLOEXEC);
  // The previous scenario's HAL was killed: the driver frees its context manager in deferred work after the
  // process is gone, so a new one can see EBUSY for a moment (seen on Linux 6.18 in a VM). Retry for up to 2 s.
  int rc = -1;
  if (sfd >= 0 && mmap(NULL, 1 << 20, PROT_READ, MAP_PRIVATE, sfd, 0) != MAP_FAILED)
    for (int tries = 0; tries < 200; tries++) {
      rc = ioctl(sfd, BINDER_SET_CONTEXT_MGR, 0);
      if (rc == 0 || errno != EBUSY) break;
      usleep(10000);
    }
  if (rc < 0) { perror("fake hal"); _exit(91); }
  pthread_t t;
  pthread_create(&t, NULL, reader, NULL);
  uint32_t enter = BC_ENTER_LOOPER;
  swrite(&enter, sizeof enter);
  (void)!write(ready, "r", 1);
  close(ready);
  for (;;) {
    uint32_t buf[128];
    struct binder_write_read bwr = { .read_size = sizeof buf, .read_buffer = (uintptr_t)buf };
    if (ioctl(sfd, BINDER_WRITE_READ, &bwr) < 0) { if (errno == EINTR) continue; perror("fake hal: read"); _exit(92); }
    for (const uint8_t *p = (const uint8_t *)buf, *end = p + bwr.read_consumed; p + 4 <= end;) {
      uint32_t cmd;
      memcpy(&cmd, p, 4);
      p += 4;
      if (cmd == BR_TRANSACTION) { struct binder_transaction_data tr; memcpy(&tr, p, sizeof tr); serve(&tr); }
      else if (cmd == BR_INCREFS || cmd == BR_ACQUIRE) {
        struct { uint32_t cmd; struct binder_ptr_cookie pc; } __attribute__((packed)) out;
        out.cmd = cmd == BR_INCREFS ? BC_INCREFS_DONE : BC_ACQUIRE_DONE;
        memcpy(&out.pc, p, sizeof out.pc);
        swrite(&out, sizeof out);
      }
      p += _IOC_SIZE(cmd);
    }
  }
}

static pid_t start_hal(int mode)
{
  int ready[2];
  if (pipe(ready)) return -1;
  fflush(stdout);
  pid_t pid = fork();
  if (pid == 0) { close(ready[0]); fake_hal(mode, ready[1]); }
  close(ready[1]);
  char c;
  if (read(ready[0], &c, 1) != 1) pid = -1;
  close(ready[0]);
  return pid;
}

// ================================ the client (parent) ================================
typedef struct { pthread_mutex_t lock; int state; unsigned states, packets; uint8_t type[8]; uint8_t data[8][64]; size_t length[8]; } client;
static void c_rx(void *ctx, uint8_t type, const uint8_t *packet, size_t length)
{
  client *c = ctx;
  pthread_mutex_lock(&c->lock);
  if (c->packets < 8 && length <= 64) { c->type[c->packets] = type; memcpy(c->data[c->packets], packet, length); c->length[c->packets] = length; }
  c->packets++;
  pthread_mutex_unlock(&c->lock);
}
static void c_state(void *ctx, int state) { client *c = ctx; pthread_mutex_lock(&c->lock); c->state = state; c->states++; pthread_mutex_unlock(&c->lock); }
static int c_get(client *c, bool packets) { pthread_mutex_lock(&c->lock); int v = packets ? (int)c->packets : c->state; pthread_mutex_unlock(&c->lock); return v; }
static bool wait_for(client *c, bool packets, int value, unsigned ms)
{
  for (unsigned i = 0; i < ms / 10; i++) { if (c_get(c, packets) == value) return true; usleep(10000); }
  return c_get(c, packets) == value;
}
static int child_exit(pid_t pid, unsigned ms)
{
  for (unsigned i = 0; i < ms / 10; i++) {
    int status;
    if (waitpid(pid, &status, WNOHANG) == pid) return WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
    usleep(10000);
  }
  kill(pid, SIGKILL);
  waitpid(pid, NULL, 0);
  return -1;
}

static const uint8_t reset[] = { 0x03, 0x0c, 0 }, read_address[] = { 0x09, 0x10, 0 }, acl[] = { 0x01, 0x00, 4, 0, 0xde, 0xad, 0xbe, 0xef };

static void full_session(int mode)
{
  printf("%s HAL\n", mode == MODE_OK ? "1.1" : "1.0-only");
  pid_t hal = start_hal(mode);
  CHECK(hal > 0);
  CHECK(br_hci_hidl_probe() == (mode == MODE_OK ? 11 : 10));
  client c = { .lock = PTHREAD_MUTEX_INITIALIZER, .state = -1 };
  br_hci *h = br_hci_open_hidl(c_rx, c_state, &c);
  CHECK(h != NULL);
  if (!h) return;
  CHECK(br_hci_send(h, 1, reset, sizeof reset) == -ENOTCONN);           // not open yet
  CHECK(br_hci_open(h) == 0);
  CHECK(wait_for(&c, false, 1, 3000));                                  // initializationComplete(SUCCESS)
  CHECK(br_hci_send(h, 1, reset, sizeof reset) == 0);
  CHECK(br_hci_send(h, 1, read_address, sizeof read_address) == 0);
  CHECK(br_hci_send(h, 2, acl, sizeof acl) == 0);
  CHECK(wait_for(&c, true, 3, 3000));
  pthread_mutex_lock(&c.lock);
  CHECK(c.type[0] == 4 && c.length[0] == 6 && !memcmp(c.data[0], "\x0e\x04\x01\x03\x0c\x00", 6));
  CHECK(c.type[1] == 4 && c.length[1] == 12 && !memcmp(c.data[1] + 6, "\x66\x55\x44\x33\x22\x11", 6));
  CHECK(c.type[2] == 2 && c.length[2] == sizeof acl && !memcmp(c.data[2], acl, sizeof acl));
  pthread_mutex_unlock(&c.lock);
  br_hci_close(h);
  CHECK(c_get(&c, false) == 0);
  CHECK(child_exit(hal, 3000) == 0);                                    // the fake HAL saw close and no bad parcel
}

// A binderfs starts with the devices of the kernel's CONFIG_ANDROID_BINDER_DEVICES. A kernel built with none
// (Ubuntu's, and so colima's VM on a Mac) has only binder-control: ask it for the device by name.
static void binderfs_add(const char *path)
{
  const char *slash = strrchr(path, '/');
  if (!slash || access(path, F_OK) == 0) return;
  char control[512];
  snprintf(control, sizeof control, "%.*s/binder-control", (int)(slash - path), path);
  int fd = open(control, O_RDWR | O_CLOEXEC);
  if (fd < 0) return;
  struct binderfs_device d = { 0 };
  snprintf(d.name, sizeof d.name, "%s", slash + 1);
  if (ioctl(fd, BINDER_CTL_ADD, &d)) printf("test_hidl_binder: %s: BINDER_CTL_ADD %s: %s\n", control, d.name, strerror(errno));
  close(fd);
}

int main(void)
{
  device = getenv("ONEIOS_HWBINDER");
  if (device) binderfs_add(device);
  if (!device || access(device, R_OK | W_OK)) {
    printf("test_hidl_binder: set ONEIOS_HWBINDER to a binderfs hwbinder (make test-binder does)\n");
    return 77;
  }
  setvbuf(stdout, NULL, _IOLBF, 0);
  br_log_sink = log_line;
  signal(SIGPIPE, SIG_IGN);

  full_session(MODE_OK);
  full_session(MODE_V10);

  printf("no HAL registered\n");
  pid_t hal = start_hal(MODE_ABSENT);
  CHECK(br_hci_hidl_probe() == -ENOENT);
  client c = { .lock = PTHREAD_MUTEX_INITIALIZER, .state = -1 };
  br_hci *h = br_hci_open_hidl(c_rx, c_state, &c);
  CHECK(h && br_hci_open(h) == 0 && wait_for(&c, false, 2, 3000));      // failed: the guest hears it
  br_hci_close(h);
  kill(hal, SIGKILL); waitpid(hal, NULL, 0);

  printf("a HAL that never completes initialisation\n");
  setenv("ONEIOS_BT_INIT_TIMEOUT_MS", "300", 1);
  hal = start_hal(MODE_SILENT);
  client s = { .lock = PTHREAD_MUTEX_INITIALIZER, .state = -1 };
  h = br_hci_open_hidl(c_rx, c_state, &s);
  CHECK(h && br_hci_open(h) == 0);
  usleep(150000);
  CHECK(c_get(&s, false) != 2);                                         // still waiting
  CHECK(wait_for(&s, false, 2, 2000));
  br_hci_close(h);
  kill(hal, SIGKILL); waitpid(hal, NULL, 0);
  unsetenv("ONEIOS_BT_INIT_TIMEOUT_MS");

  printf("the HAL dies\n");
  hal = start_hal(MODE_OK);
  client d = { .lock = PTHREAD_MUTEX_INITIALIZER, .state = -1 };
  h = br_hci_open_hidl(c_rx, c_state, &d);
  CHECK(h && br_hci_open(h) == 0 && wait_for(&d, false, 1, 3000));
  kill(hal, SIGKILL); waitpid(hal, NULL, 0);
  CHECK(wait_for(&d, false, 2, 2000));                                  // the death notification
  CHECK(br_hci_send(h, 1, reset, sizeof reset) == -ENOTCONN);

  printf("%d of %d checks failed\n", failures, checks);
  return failures ? 1 : 0;
}
