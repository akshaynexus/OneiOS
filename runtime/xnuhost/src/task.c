// SPDX-License-Identifier: MIT
// iOS tasks as Linux processes (docs/booter.md, "Tasks"). Linux and AArch64 only.
//   factory   forked from xnuhost before it has threads or the RAM mapped. It never execs, so it
//             and every task it forks hold xnuhost's pointer-authentication keys: one key set for
//             the kernel and all iOS tasks, which is what arm64e's shared cache needs.
//   task      a fork of the factory that runs only the stub (stub/stub.c): empty address space, a
//             service thread that carries out map/unmap/protect/clone orders, and one Linux thread
//             per iOS thread whose system calls and faults arrive as signals.
// How Darling's darlingserver serves the same calls from Linux was the reference for what a trap
// must carry (https://github.com/darlinghq/darlingserver ca9236e: per-thread RPC with the caller's
// full register state); the transport here is a shared page and two futex words instead of a socket.
#define _GNU_SOURCE
#include "xnuhost.h"
#include "../stub/stub_abi.h"
#include <errno.h>
#include <stddef.h>
#include <string.h>

// ---- the seccomp filter (portable: unit-tested on the build host) -------------------------------
#define BPF_LD_W_ABS 0x20
#define BPF_JEQ_K 0x15
#define BPF_JGE_K 0x35
#define BPF_RET_K 0x06
#define SECCOMP_RET_TRAP_ 0x00030000u
#define SECCOMP_RET_ALLOW_ 0x7fff0000u
#define SECCOMP_RET_KILL_PROCESS_ 0x80000000u
#define AUDIT_ARCH_AARCH64_ 0xc00000b7u

// Every system call traps (SIGSYS), except the stub's own: those whose instruction pointer is
// inside the stub's text and whose number the stub uses. mmap from the stub must name one of the
// two memfds or no file. iOS code issues `svc #0x80` with XNU's number in x16; Linux would read x8
// and run something else, so nothing issued from outside the stub may reach Linux.
unsigned xh_task_filter(struct xs_filter_insn *f, uint64_t text_lo, uint64_t text_hi, int ram_fd, int trap_fd)
{
  static const uint32_t allowed[] = { 98 /* futex */, 93 /* exit */, 94 /* exit_group */, 132 /* sigaltstack */,
                                      139 /* rt_sigreturn */, 167 /* prctl */, 172 /* getpid */, 178 /* gettid */,
                                      215 /* munmap */, 220 /* clone */, 226 /* mprotect */ };
  enum { N = sizeof allowed / sizeof *allowed, NEXT = 0, TRAP, ALLOW, KILL, LIST, LABELS };
  uint8_t jt[XS_FILTER_MAX] = { 0 }, jf[XS_FILTER_MAX] = { 0 };
  unsigned where[LABELS] = { 0 }, n = 0;
  if ((text_lo >> 32) != ((text_hi - 1) >> 32)) return 0;      // the text must not cross a 4 GiB line
#define INSN(c, t, fl, kk) do { f[n] = (struct xs_filter_insn){ (uint16_t)(c), 0, 0, (uint32_t)(kk) }; jt[n] = (t); jf[n] = (fl); n++; } while (0)
  INSN(BPF_LD_W_ABS, NEXT, NEXT, 4);                           // architecture
  INSN(BPF_JEQ_K, NEXT, KILL, AUDIT_ARCH_AARCH64_);
  INSN(BPF_LD_W_ABS, NEXT, NEXT, 12);                          // instruction pointer, high word
  INSN(BPF_JEQ_K, NEXT, TRAP, text_lo >> 32);
  INSN(BPF_LD_W_ABS, NEXT, NEXT, 8);                           // instruction pointer, low word
  INSN(BPF_JGE_K, NEXT, TRAP, text_lo);
  INSN(BPF_JGE_K, TRAP, NEXT, text_hi);
  INSN(BPF_LD_W_ABS, NEXT, NEXT, 0);                           // system call number
  INSN(BPF_JEQ_K, NEXT, LIST, 222);                            // mmap: its file descriptor is argument 4
  INSN(BPF_LD_W_ABS, NEXT, NEXT, 16 + 4 * 8);
  INSN(BPF_JEQ_K, ALLOW, NEXT, ram_fd);
  INSN(BPF_JEQ_K, ALLOW, NEXT, trap_fd);
  INSN(BPF_JEQ_K, ALLOW, TRAP, 0xffffffffu);                   // or anonymous memory
  where[LIST] = n;
  INSN(BPF_LD_W_ABS, NEXT, NEXT, 0);
  for (unsigned i = 0; i < N; i++) INSN(BPF_JEQ_K, ALLOW, NEXT, allowed[i]);
  where[TRAP] = n;
  INSN(BPF_RET_K, NEXT, NEXT, SECCOMP_RET_TRAP_);
  where[ALLOW] = n;
  INSN(BPF_RET_K, NEXT, NEXT, SECCOMP_RET_ALLOW_);
  where[KILL] = n;
  INSN(BPF_RET_K, NEXT, NEXT, SECCOMP_RET_KILL_PROCESS_);
#undef INSN
  for (unsigned i = 0; i < n; i++) {
    f[i].jt = jt[i] ? (uint8_t)(where[jt[i]] - (i + 1)) : 0;
    f[i].jf = jf[i] ? (uint8_t)(where[jf[i]] - (i + 1)) : 0;
  }
  return n;
}

#if defined(__aarch64__) && defined(__linux__)
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdlib.h>
#include <linux/futex.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern const uint8_t xh_stub_blob[], xh_stub_blob_end[];

#define MAX_TASKS 1024
#define MAX_SLOTS 8192                       // trap memfd slots: threads and service pages
#define KICK_SIGNAL (SIGRTMIN + 3)

typedef struct {
  bool used, dead;
  int pid;
  uint32_t exit_status;
  uint32_t service_slot;
  pthread_mutex_t lock;                      // one order batch at a time
  uint8_t areas[XS_MAX_THREADS / 8 + 1];     // thread areas in use
} task;

typedef struct { bool used; uint16_t task; uint16_t area; } thread;

static struct {
  bool available;
  uint64_t base;                             // of the stub region = the first address tasks may not use
  int trap_fd, sock;
  uint8_t *trap;                             // the whole trap memfd, mapped
  pid_t factory;
  pthread_mutex_t lock;                      // tables and the factory conversation
  pthread_cond_t reply;
  int reply_pid; bool reply_ready;
  task tasks[MAX_TASKS];
  thread threads[MAX_SLOTS];
  uint8_t slots[MAX_SLOTS / 8];
  pthread_t reader;
} T = { .lock = PTHREAD_MUTEX_INITIALIZER, .reply = PTHREAD_COND_INITIALIZER, .trap_fd = -1, .sock = -1 };

struct message { uint32_t kind; int32_t pid; uint32_t slot, status; };
enum { M_SPAWN = 1, M_SPAWNED, M_EXITED, M_KILL };

bool xh_tasks_available(void) { return T.available; }
uint64_t xh_tasks_va_limit(void) { return T.base; }

static long futex(uint32_t *word, int op, uint32_t value, const struct timespec *timeout)
{
  return syscall(SYS_futex, word, op, value, timeout, NULL, 0);
}

static void *slot_page(uint32_t slot) { return T.trap + (uint64_t)slot * XS_SLOT_SIZE; }

static int slot_alloc(void)
{
  for (unsigned i = 1; i < MAX_SLOTS; i++)
    if (!(T.slots[i / 8] & (1 << (i % 8)))) { T.slots[i / 8] |= (uint8_t)(1 << (i % 8)); memset(slot_page(i), 0, XS_SLOT_SIZE); return (int)i; }
  return -1;
}
static void slot_free(uint32_t slot) { T.slots[slot / 8] &= (uint8_t)~(1 << (slot % 8)); }

// ---- the factory process ------------------------------------------------------------------------
static __attribute__((noreturn)) void factory_child(uint32_t slot)
{
  struct xs_config *config = (struct xs_config *)(uintptr_t)(T.base + XS_CONFIG_OFF);
  prctl(PR_SET_PDEATHSIG, SIGKILL);
  close(T.sock);
  config->service_slot = slot;
  ((void (*)(void))(uintptr_t)T.base)();
  _exit(72);
}

static __attribute__((noreturn)) void factory_main(int sock)
{
  // Single-threaded, a copy of xnuhost as it was before it mapped the RAM. Requests in, results and
  // exits of tasks out. When xnuhost goes away the socket closes and the factory and its tasks die.
  prctl(PR_SET_PDEATHSIG, SIGKILL);
  prctl(PR_SET_NAME, "xnuhost-tasks");
  T.sock = sock;
  for (;;) {
    struct pollfd p = { .fd = sock, .events = POLLIN };
    int n = poll(&p, 1, 100);
    if (n > 0 && (p.revents & (POLLHUP | POLLERR))) break;
    if (n > 0 && (p.revents & POLLIN)) {
      struct message m;
      ssize_t got = recv(sock, &m, sizeof m, 0);
      if (got == 0) break;
      if (got == (ssize_t)sizeof m && m.kind == M_SPAWN) {
        pid_t pid = fork();
        if (pid == 0) factory_child(m.slot);
        struct message r = { .kind = M_SPAWNED, .pid = pid < 0 ? -errno : pid, .slot = m.slot };
        send(sock, &r, sizeof r, MSG_NOSIGNAL);
      } else if (got == (ssize_t)sizeof m && m.kind == M_KILL) {
        kill(m.pid, SIGKILL);
      }
    }
    int status;
    pid_t pid;
    while ((pid = waitpid(-1, &status, WNOHANG)) > 0) {
      struct message r = { .kind = M_EXITED, .pid = pid, .status = (uint32_t)status };
      send(sock, &r, sizeof r, MSG_NOSIGNAL);
    }
  }
  kill(0, SIGKILL);            // the factory's process group: itself and every task
  _exit(0);
}

static void mark_dead(task *t, uint32_t status)
{
  t->dead = true;
  t->exit_status = status;
  // Whoever waits on one of its pages must look again.
  struct xs_service_page *sp = slot_page(t->service_slot);
  futex(&sp->done, FUTEX_WAKE, INT32_MAX, NULL);
  futex(&sp->ready, FUTEX_WAKE, INT32_MAX, NULL);
  for (unsigned s = 0; s < MAX_SLOTS; s++)
    if (T.threads[s].used && &T.tasks[T.threads[s].task] == t) {
      struct xs_trap_page *tp = slot_page(s);
      futex(&tp->to_host, FUTEX_WAKE, INT32_MAX, NULL);
      futex(&tp->tid, FUTEX_WAKE, INT32_MAX, NULL);
    }
}

static void *reader_main(void *arg)
{
  (void)arg;
  struct message m;
  while (recv(T.sock, &m, sizeof m, 0) == (ssize_t)sizeof m) {
    pthread_mutex_lock(&T.lock);
    if (m.kind == M_SPAWNED) {
      T.reply_pid = m.pid;
      T.reply_ready = true;
      pthread_cond_broadcast(&T.reply);
    } else if (m.kind == M_EXITED) {
      for (unsigned i = 0; i < MAX_TASKS; i++)
        if (T.tasks[i].used && T.tasks[i].pid == m.pid && !T.tasks[i].dead) mark_dead(&T.tasks[i], m.status);
    }
    pthread_mutex_unlock(&T.lock);
  }
  if (!xh_stopping()) xh_stop(XH_HALT_PANIC, 0, "the task factory is gone");
  return NULL;
}

int xh_tasks_init(const xh_config *c)
{
  size_t text = (size_t)(xh_stub_blob_end - xh_stub_blob);
  if (text == 0 || text > XS_TEXT_MAX) { LOGE("tasks: bad stub blob (%zu bytes)", text); return -1; }
  T.trap_fd = (int)syscall(SYS_memfd_create, "xnuhost-traps", 0u);
  if (T.trap_fd < 0 || ftruncate(T.trap_fd, (off_t)MAX_SLOTS * XS_SLOT_SIZE)) { LOGE("tasks: trap memfd: %s", strerror(errno)); return -1; }
  T.trap = mmap(NULL, (size_t)MAX_SLOTS * XS_SLOT_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, T.trap_fd, 0);
  if (T.trap == MAP_FAILED) { LOGE("tasks: map traps: %s", strerror(errno)); return -1; }

  // The stub region: high in the address space, below the host's own mappings where it can be.
  static const uint64_t candidates[] = { 0x7e00000000ull, 0x7d00000000ull, 0x7c00000000ull, 0x7000000000ull, 0x6000000000ull };
  void *region = MAP_FAILED;
  for (unsigned i = 0; i < sizeof candidates / sizeof *candidates && region == MAP_FAILED; i++) {
    region = mmap((void *)(uintptr_t)candidates[i], XS_REGION_SIZE, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE | MAP_FIXED_NOREPLACE, -1, 0);
    if (region != MAP_FAILED && (uint64_t)(uintptr_t)region != candidates[i]) { munmap(region, XS_REGION_SIZE); region = MAP_FAILED; }
  }
  if (region == MAP_FAILED) { LOGE("tasks: no room for the stub region: %s", strerror(errno)); return -1; }
  T.base = (uint64_t)(uintptr_t)region;
  uint8_t *b = region;
  if (mprotect(b, XS_TEXT_MAX, PROT_READ | PROT_WRITE) || mprotect(b + XS_CONFIG_OFF, 0x1000, PROT_READ | PROT_WRITE) ||
      mprotect(b + XS_BOOTSTACK_OFF, XS_BOOTSTACK_SIZE, PROT_READ | PROT_WRITE)) { LOGE("tasks: stub region: %s", strerror(errno)); return -1; }
  memcpy(b, xh_stub_blob, text);
  if (mprotect(b, XS_TEXT_MAX, PROT_READ | PROT_EXEC)) { LOGE("tasks: stub text not executable: %s", strerror(errno)); return -1; }
  __builtin___clear_cache((char *)b, (char *)b + text);

  struct xs_config *config = (struct xs_config *)(b + XS_CONFIG_OFF);
  config->base = T.base;
  config->ram_fd = g_mem.fd;
  config->trap_fd = T.trap_fd;
  config->kick_signal = (uint32_t)KICK_SIGNAL;
  config->uid = c->task_uid;
  // Is the filter usable here? (qemu-user and some kernels have no seccomp.) Asked in a child so
  // that xnuhost itself never carries a filter.
  bool seccomp = false;
  pid_t probe = fork();
  if (probe == 0) {
    struct xs_filter_insn allow = { BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW_ };
    struct { uint16_t length; struct xs_filter_insn *filter; } program = { 1, &allow };
    prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    _exit(syscall(SYS_seccomp, 1, 0, &program) == 0 ? 0 : 1);
  }
  int status = 0;
  if (probe > 0 && waitpid(probe, &status, 0) == probe) seccomp = WIFEXITED(status) && WEXITSTATUS(status) == 0;
  config->filter_length = seccomp ? xh_task_filter(config->filter, T.base, T.base + text, g_mem.fd, T.trap_fd) : 0;
  if (seccomp && !config->filter_length) { LOGE("tasks: filter construction failed"); return -1; }
  if (!seccomp) LOGI("tasks: no seccomp filter on this host: system calls of tasks are NOT trapped (faults are)");

  int pair[2];
  if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, pair)) { LOGE("tasks: socketpair: %s", strerror(errno)); return -1; }
  T.factory = fork();
  if (T.factory < 0) { LOGE("tasks: fork: %s", strerror(errno)); return -1; }
  if (T.factory == 0) {
    close(pair[0]);
    setpgid(0, 0);
    factory_main(pair[1]);
  }
  close(pair[1]);
  T.sock = pair[0];
  fcntl(T.sock, F_SETFD, FD_CLOEXEC);
  // xnuhost keeps the region reserved but needs none of it; leave it mapped so nothing else lands there.
  if (pthread_create(&T.reader, NULL, reader_main, NULL)) return -1;
  T.available = seccomp;
  LOGI("tasks: factory pid %d, stub region 0x%llx, stub %zu bytes, filter %u instructions", T.factory,
       (unsigned long long)T.base, text, config->filter_length);
  return 0;
}

void xh_tasks_shutdown(void)
{
  if (T.sock < 0) return;
  for (unsigned i = 0; i < MAX_TASKS; i++)
    if (T.tasks[i].used && !T.tasks[i].dead) { struct message m = { .kind = M_KILL, .pid = T.tasks[i].pid }; send(T.sock, &m, sizeof m, MSG_NOSIGNAL); }
  shutdown(T.sock, SHUT_RDWR);       // the factory sees the hang-up and kills its group
  close(T.sock);
  T.sock = -1;
  if (T.factory > 0) { int status; for (int i = 0; i < 50 && waitpid(T.factory, &status, WNOHANG) == 0; i++) usleep(20000); kill(T.factory, SIGKILL); }
}

// ---- orders to a task's service thread ----------------------------------------------------------
static task *task_of(uint64_t handle)
{
  if (handle == 0 || handle > MAX_TASKS || !T.tasks[handle - 1].used) return NULL;
  return &T.tasks[handle - 1];
}

// Waits until *word is not 0, the task is dead, or the time is up. Returns 0, -ESRCH, -ETIMEDOUT.
static int wait_word(task *t, uint32_t *word, int seconds)
{
  struct timespec step = { 0, 100 * 1000 * 1000 };
  for (int i = 0; i < seconds * 10; i++) {
    if (__atomic_load_n(word, __ATOMIC_ACQUIRE)) return 0;
    if (t->dead) return -ESRCH;
    futex(word, FUTEX_WAIT, 0, &step);
  }
  return __atomic_load_n(word, __ATOMIC_ACQUIRE) ? 0 : -ETIMEDOUT;
}

static int64_t order(task *t, const struct xs_op *ops, unsigned count)
{
  struct xs_service_page *sp = slot_page(t->service_slot);
  int64_t result = 0;
  if (count > XS_OPS_MAX) return -EINVAL;
  pthread_mutex_lock(&t->lock);
  if (t->dead) { pthread_mutex_unlock(&t->lock); return -ESRCH; }
  memcpy(sp->ops, ops, count * sizeof *ops);
  sp->count = count;
  __atomic_store_n(&sp->done, 0, __ATOMIC_RELEASE);
  __atomic_store_n(&sp->request, 1, __ATOMIC_RELEASE);
  futex(&sp->request, FUTEX_WAKE, 1, NULL);
  int rc = wait_word(t, &sp->done, 10);
  if (rc) result = rc;
  else for (unsigned i = 0; i < count; i++) if (sp->ops[i].result < 0) { result = sp->ops[i].result; break; }
  pthread_mutex_unlock(&t->lock);
  return result;
}

int64_t xh_task_create(uint64_t *handle)
{
  if (!T.available && T.sock < 0) return -ENOSYS;
  pthread_mutex_lock(&T.lock);
  task *t = NULL;
  for (unsigned i = 0; i < MAX_TASKS && !t; i++) if (!T.tasks[i].used) t = &T.tasks[i];
  int slot = t ? slot_alloc() : -1;
  if (slot < 0) { pthread_mutex_unlock(&T.lock); return -EAGAIN; }
  memset(t, 0, sizeof *t);
  pthread_mutex_init(&t->lock, NULL);
  t->service_slot = (uint32_t)slot;
  struct message m = { .kind = M_SPAWN, .slot = (uint32_t)slot };
  T.reply_ready = false;
  if (send(T.sock, &m, sizeof m, MSG_NOSIGNAL) != (ssize_t)sizeof m) { slot_free((uint32_t)slot); pthread_mutex_unlock(&T.lock); return -EIO; }
  struct timespec until;
  clock_gettime(CLOCK_REALTIME, &until);
  until.tv_sec += 10;
  while (!T.reply_ready)
    if (pthread_cond_timedwait(&T.reply, &T.lock, &until)) break;
  if (!T.reply_ready || T.reply_pid <= 0) { slot_free((uint32_t)slot); pthread_mutex_unlock(&T.lock); return T.reply_ready ? T.reply_pid : -ETIMEDOUT; }
  t->pid = T.reply_pid;
  t->used = true;
  pthread_mutex_unlock(&T.lock);
  struct xs_service_page *sp = slot_page((uint32_t)slot);
  int rc = wait_word(t, &sp->ready, 10);
  if (rc || sp->setup_error) {
    LOGE("tasks: task process %d did not set up (%d, stub error %d)", t->pid, rc, sp->setup_error);
    xh_task_destroy((uint64_t)(t - T.tasks) + 1);
    return rc ? rc : sp->setup_error;
  }
  *handle = (uint64_t)(t - T.tasks) + 1;
  return 0;
}

int64_t xh_task_destroy(uint64_t handle)
{
  task *t = task_of(handle);
  if (!t) return -ESRCH;
  if (!t->dead) {
    struct message m = { .kind = M_KILL, .pid = t->pid };
    send(T.sock, &m, sizeof m, MSG_NOSIGNAL);
    for (int i = 0; i < 200 && !t->dead; i++) usleep(5000);       // the reader marks it dead on the factory's report
  }
  pthread_mutex_lock(&T.lock);
  for (unsigned s = 0; s < MAX_SLOTS; s++)
    if (T.threads[s].used && T.threads[s].task == t - T.tasks) { T.threads[s].used = false; slot_free(s); }
  slot_free(t->service_slot);
  t->used = false;
  pthread_mutex_unlock(&T.lock);
  return 0;
}

static int64_t ram_offset(uint64_t phys, uint64_t size)
{
  if (phys < g_mem.phys || phys - g_mem.phys > g_mem.size || size > g_mem.size - (phys - g_mem.phys)) return -1;
  return (int64_t)(phys - g_mem.phys);
}

static bool user_range(uint64_t va, uint64_t size)
{
  return size && !((va | size) & (g_info.host_page_size - 1)) && va < T.base && size <= T.base - va;
}

int64_t xh_task_map(uint64_t handle, uint64_t va, uint64_t size, uint32_t prot, uint64_t phys)
{
  task *t = task_of(handle);
  int64_t offset = ram_offset(phys, size);
  if (!t) return -ESRCH;
  if (offset < 0 || !user_range(va, size)) return -EINVAL;
  struct xs_op op = { .op = XS_OP_MAP, .prot = prot, .va = va, .size = size, .offset = (uint64_t)offset };
  return order(t, &op, 1);
}

int64_t xh_task_unmap(uint64_t handle, uint64_t va, uint64_t size)
{
  task *t = task_of(handle);
  if (!t) return -ESRCH;
  if (!user_range(va, size)) return -EINVAL;
  struct xs_op op = { .op = XS_OP_UNMAP, .va = va, .size = size };
  return order(t, &op, 1);
}

int64_t xh_task_protect(uint64_t handle, uint64_t va, uint64_t size, uint32_t prot)
{
  task *t = task_of(handle);
  if (!t) return -ESRCH;
  if (!user_range(va, size)) return -EINVAL;
  struct xs_op op = { .op = XS_OP_PROTECT, .prot = prot, .va = va, .size = size };
  return order(t, &op, 1);
}

int64_t xh_task_read(uint64_t handle, uint64_t va, void *buffer, uint64_t size)
{
  task *t = task_of(handle);
  if (!t || t->dead) return -ESRCH;
  struct iovec local = { buffer, size }, remote = { (void *)(uintptr_t)va, size };
  ssize_t n = process_vm_readv(t->pid, &local, 1, &remote, 1, 0);
  return n < 0 ? -errno : n;
}

int64_t xh_task_write(uint64_t handle, uint64_t va, const void *buffer, uint64_t size)
{
  task *t = task_of(handle);
  if (!t || t->dead) return -ESRCH;
  struct iovec local = { (void *)(uintptr_t)buffer, size }, remote = { (void *)(uintptr_t)va, size };
  ssize_t n = process_vm_writev(t->pid, &local, 1, &remote, 1, 0);
  return n < 0 ? -errno : n;
}

// ---- threads ------------------------------------------------------------------------------------
int64_t xh_thread_create(uint64_t handle, uint64_t *out)
{
  task *t = task_of(handle);
  if (!t) return -ESRCH;
  pthread_mutex_lock(&T.lock);
  int area = -1;
  for (unsigned i = 0; i < XS_MAX_THREADS && area < 0; i++)
    if (!(t->areas[i / 8] & (1 << (i % 8)))) area = (int)i;
  int slot = area >= 0 ? slot_alloc() : -1;
  if (slot < 0) { pthread_mutex_unlock(&T.lock); return -EAGAIN; }
  t->areas[area / 8] |= (uint8_t)(1 << (area % 8));
  T.threads[slot] = (thread){ .used = true, .task = (uint16_t)(t - T.tasks), .area = (uint16_t)area };
  pthread_mutex_unlock(&T.lock);

  struct xs_trap_page *tp = slot_page((uint32_t)slot);
  struct xs_op op = { .op = XS_OP_CLONE, .va = (uint64_t)area, .offset = (uint64_t)slot * XS_SLOT_SIZE };
  int64_t rc = order(t, &op, 1);
  if (!rc) rc = wait_word(t, &tp->to_host, 10);      // its first stop (XH_TRAP_BOOT)
  if (rc) {
    pthread_mutex_lock(&T.lock);
    t->areas[area / 8] &= (uint8_t)~(1 << (area % 8));
    T.threads[slot].used = false;
    slot_free((uint32_t)slot);
    pthread_mutex_unlock(&T.lock);
    return rc;
  }
  *out = (uint64_t)slot;
  return 0;
}

static thread *thread_of(uint64_t handle)
{
  return handle < MAX_SLOTS && T.threads[handle].used ? &T.threads[handle] : NULL;
}

int64_t xh_thread_destroy(uint64_t handle)
{
  thread *th = thread_of(handle);
  if (!th) return -ESRCH;
  task *t = &T.tasks[th->task];
  struct xs_trap_page *tp = slot_page((uint32_t)handle);
  if (!t->dead) {
    // The thread is stopped in the stub's handler (the kernel destroys only threads that trapped).
    tp->command = XS_EXIT;
    __atomic_store_n(&tp->to_task, 1, __ATOMIC_RELEASE);
    futex(&tp->to_task, FUTEX_WAKE, 1, NULL);
    struct timespec step = { 0, 20 * 1000 * 1000 };
    for (int i = 0; i < 100 && !t->dead; i++) {      // the kernel zeroes tid when the thread is gone
      uint32_t tid = __atomic_load_n(&tp->tid, __ATOMIC_ACQUIRE);
      if (!tid) break;
      futex(&tp->tid, FUTEX_WAIT, tid, &step);
    }
    struct xs_op op = { .op = XS_OP_UNMAP, .va = T.base + XS_THREADS_OFF + (uint64_t)th->area * XS_AREA_SIZE, .size = XS_AREA_SIZE };
    order(t, &op, 1);
  }
  pthread_mutex_lock(&T.lock);
  t->areas[th->area / 8] &= (uint8_t)~(1 << (th->area % 8));
  th->used = false;
  slot_free((uint32_t)handle);
  pthread_mutex_unlock(&T.lock);
  return 0;
}

int64_t xh_thread_set_pac(uint64_t handle, uint32_t enabled)
{
  thread *th = thread_of(handle);
  if (!th) return -ESRCH;
  struct xs_trap_page *tp = slot_page((uint32_t)handle);
  tp->pac_enabled = enabled & XH_PAC_ALL;
  tp->pac_set = 1;
  return 0;
}

int64_t xh_thread_run(xh_vcpu *v, uint64_t handle, struct xh_user_state *state, struct xh_trap *trap)
{
  thread *th = thread_of(handle);
  if (!th) return -ESRCH;
  task *t = &T.tasks[th->task];
  struct xs_trap_page *tp = slot_page((uint32_t)handle);
  memset(trap, 0, sizeof *trap);
  if (t->dead) { trap->kind = XH_TRAP_EXITED; trap->exit_status = t->exit_status; return 0; }
  // An interrupt already waiting is taken before any user instruction runs.
  if (v->cpu.irq_pending) { trap->kind = XH_TRAP_INTERRUPT; return 0; }
  tp->state = *state;
  tp->command = XS_RUN;
  __atomic_store_n(&tp->to_host, 0, __ATOMIC_RELEASE);
  __atomic_store_n(&tp->to_task, 1, __ATOMIC_RELEASE);
  futex(&tp->to_task, FUTEX_WAKE, 1, NULL);
  bool kicked = false;
  struct timespec step = { 0, 200 * 1000 * 1000 };
  while (!__atomic_load_n(&tp->to_host, __ATOMIC_ACQUIRE)) {
    if (t->dead) { trap->kind = XH_TRAP_EXITED; trap->exit_status = t->exit_status; return 0; }
    // The vCPU's interrupt line rose (its timer or another CPU signalled this thread, which ended
    // the wait below): force the iOS thread to stop, once.
    if ((v->cpu.irq_pending || xh_stopping()) && !kicked) {
      uint32_t tid = __atomic_load_n(&tp->tid, __ATOMIC_ACQUIRE);
      if (tid) syscall(SYS_tgkill, t->pid, tid, KICK_SIGNAL);
      kicked = true;
    }
    futex(&tp->to_host, FUTEX_WAIT, 0, &step);
  }
  *state = tp->state;
  *trap = tp->trap;
  v->stat_traps++;
  return 0;
}

#else   // not AArch64 Linux: the filter above is all there is

int xh_tasks_init(const xh_config *c) { (void)c; return 0; }
bool xh_tasks_available(void) { return false; }
uint64_t xh_tasks_va_limit(void) { return 0; }
void xh_tasks_shutdown(void) {}

#endif
