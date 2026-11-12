// SPDX-License-Identifier: MIT
// iOS tasks as Linux processes (docs/booter.md, "Tasks"). Linux and AArch64 only.
//   factory   forked from xnuhost before it has threads or the RAM mapped. It never execs, so it
//             and every task it forks hold xnuhost's pointer-authentication keys: one key set for
//             the kernel and all iOS tasks, which is what arm64e's shared cache needs.
//   task      a fork of the factory that runs only the stub (stub/stub.c): empty address space, a
//             service thread that carries out map/unmap/protect/clone orders, and one Linux thread
//             per iOS thread whose system calls and faults arrive as signals.
// Darling (https://github.com/darlinghq/darling) serves the same calls another way: its own
// libsystem_kernel calls darlingserver over a socket. iOS's libsystem_kernel is Apple's and issues
// `svc #0x80` itself, so here the call is trapped (seccomp, SIGSYS) and the transport is a shared
// page and two futex words.
#define _GNU_SOURCE
#include "xnuhost.h"
#include "../stub/stub_abi.h"
#include <errno.h>
#include <dirent.h>
#include <stdio.h>
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

// XH_HOST_TEST: the host unit tests (tests/test_xnuhost.c) take the filter alone, also on an arm64 Linux host.
#if defined(__aarch64__) && defined(__linux__) && !defined(XH_HOST_TEST)
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
#define XS_PENDING_MAX 64                    // deferred maps a task holds before they are ordered

typedef struct {
  bool used, dead;
  bool zombie;                               // destroyed by the kernel, its process not yet reported dead
  int pid;
  uint32_t exit_status;
  uint32_t service_slot;
  pthread_mutex_t lock;                      // one order batch or one release's maps at a time
  uint8_t areas[XS_MAX_THREADS / 8 + 1];     // thread areas in use
  // map-defer: task_map orders not sent yet. They go with the next release of one of the task's threads (carried out
  // by that thread in the stub's handler before it runs) or at the head of the task's next order, whichever is first,
  // so they keep their place before any later unmap or protect. Under `qlock`, which is held only to add to or take
  // the queue: a task_map does not wait for a release whose thread is carrying out earlier maps (`lock`, about 100 us
  // on the phone, while the kernel's caller holds the task's TLB lock with interrupts masked). A taker holds both.
  pthread_mutex_t qlock;
  unsigned npending;
  struct xs_op pending[XS_PENDING_MAX];
  uint8_t service_placed, service_epoch;     // the vCPU + 1 its service thread was last put with (xh_cpu_place)
} task;

typedef struct {
  bool used; uint16_t task; uint16_t area; uint8_t placed; /* vCPU + 1 it was last put with */ uint8_t epoch;
  uint64_t released_at;                      // counter: its last release (perf.c, "down")
} thread;
// Changed by main.c's thermal step when the vCPUs' host CPUs change: each thread is placed again at its next run.
unsigned xh_cpu_place_epoch;

static struct {
  bool available, seccomp;
  bool map_defer;                            // map-defer (default on)
  uint64_t spin_host, spin_task;             // counter ticks: spin-host, spin-task
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
bool xh_tasks_seccomp(void) { return T.seccomp; }
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

static void task_release(task *t);

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
        if (T.tasks[i].used && T.tasks[i].pid == m.pid && !T.tasks[i].dead) {
          mark_dead(&T.tasks[i], m.status);
          if (T.tasks[i].zombie) task_release(&T.tasks[i]);
        }
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
  // The hand-off (stub_abi.h): how long each side spins before it sleeps. Measured with `make bench-arm64`
  // (docs/booter.md 2.6a): the defaults are what that measurement chose.
  uint64_t hz = xh_counter_hz();
  T.spin_host = (uint64_t)(c->spin_host < 0 ? XH_SPIN_HOST_US : c->spin_host) * hz / 1000000;
  T.spin_task = (uint64_t)(c->spin_task < 0 ? XH_SPIN_TASK_US : c->spin_task) * hz / 1000000;
  T.map_defer = c->map_defer != 0;
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
  if (!seccomp) {
    LOGI("tasks: no seccomp filter on this host: `svc #0x80` is rewritten in text the kernel maps (text_fixup) and trapped as an undefined instruction; a svc in other code would reach the host");
    xh_text_fixup_svc = true;
  }

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
  T.available = true;
  T.seccomp = seccomp;
  LOGI("tasks: factory pid %d, stub region 0x%llx, stub %zu bytes, filter %u instructions; hand-off spins %u us (vCPU) and %u us "
       "(iOS thread), maps %s", T.factory, (unsigned long long)T.base, text, config->filter_length,
       (unsigned)(T.spin_host * 1000000 / hz), (unsigned)(T.spin_task * 1000000 / hz),
       T.map_defer ? "carried by the next release (map-defer=1)" : "ordered one by one (map-defer=0)");
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
  // By the clock, not by the number of waits: a futex wait that a signal interrupts (the vCPU threads get
  // them all the time: kicks, timers) returns at once, and a hundred of those are not ten seconds.
  struct timespec now, until;
  clock_gettime(CLOCK_MONOTONIC, &until);
  until.tv_sec += seconds;
  for (;;) {
    if (__atomic_load_n(word, __ATOMIC_ACQUIRE)) return 0;
    if (t->dead) return -ESRCH;
    clock_gettime(CLOCK_MONOTONIC, &now);
    if (now.tv_sec > until.tv_sec || (now.tv_sec == until.tv_sec && now.tv_nsec >= until.tv_nsec)) return -ETIMEDOUT;
    struct timespec step = { 0, 100 * 1000 * 1000 };
    futex(word, FUTEX_WAIT, 0, &step);
  }
}

static void log_task_threads(int pid);

static void log_map_failure(task *t, const struct xs_op *op, const char *how)
{
  static unsigned told;
  if (op->result < 0 && __atomic_fetch_add(&told, 1, __ATOMIC_RELAXED) < 16)
    LOGE("tasks: pid %d: a deferred map (va 0x%llx, size 0x%llx, prot %u) failed %s: %lld", t->pid, (unsigned long long)op->va,
         (unsigned long long)op->size, op->prot, how, (long long)op->result);
}

static int64_t order(task *t, const struct xs_op *ops, unsigned count)
{
  struct xs_service_page *sp = slot_page(t->service_slot);
  int64_t result = 0;
  if (count > XS_OPS_MAX - XS_PENDING_MAX) return -EINVAL;
  pthread_mutex_lock(&t->lock);
  pthread_mutex_lock(&t->qlock);
  if (t->dead) { t->npending = 0; pthread_mutex_unlock(&t->qlock); pthread_mutex_unlock(&t->lock); return -ESRCH; }
  // The deferred maps first, in their order: an unmap or protect after them must find them done.
  unsigned first = t->npending;
  memcpy(sp->ops, t->pending, first * sizeof *ops);
  t->npending = 0;
  pthread_mutex_unlock(&t->qlock);
  // The service thread on the ordering vCPU's host CPU, as an iOS thread is (cpu-pin): the order is then a switch
  // on that core and back, not the wake-up of another core.
  xh_vcpu *v = xh_cpu_self();
  const uint8_t epoch = (uint8_t)__atomic_load_n(&xh_cpu_place_epoch, __ATOMIC_ACQUIRE);
  if (xh_cpu_place && v && (t->service_placed != v->cpu.index + 1 || t->service_epoch != epoch)) {
    xh_cpu_place(t->pid, v->cpu.index);
    t->service_placed = (uint8_t)(v->cpu.index + 1);
    t->service_epoch = epoch;
  }
  if (count) memcpy(sp->ops + first, ops, count * sizeof *ops);
  sp->count = first + count;
  uint64_t t0 = xh_counter();
  __atomic_store_n(&sp->done, 0, __ATOMIC_RELEASE);
  __atomic_store_n(&sp->request, 1, __ATOMIC_RELEASE);
  futex(&sp->request, FUTEX_WAKE, 1, NULL);
  struct timespec began, ended;
  clock_gettime(CLOCK_MONOTONIC, &began);
  int rc = wait_word(t, &sp->done, 10);
  clock_gettime(CLOCK_MONOTONIC, &ended);
  // An order the task's service thread takes long over, or never answers, stalls the vCPU that gave it: say which.
  long took_ms = (long)((ended.tv_sec - began.tv_sec) * 1000 + (ended.tv_nsec - began.tv_nsec) / 1000000);
  if (rc || took_ms >= 1000)
    LOGE("tasks: pid %d: order %u (va 0x%llx, size 0x%llx, prot %u) %s after %ld ms", t->pid, ops[0].op, (unsigned long long)ops[0].va,
         (unsigned long long)ops[0].size, ops[0].prot, rc ? "was not answered" : "took long: answered", took_ms);
  if (rc == -ETIMEDOUT) log_task_threads(t->pid);
  if (count) {
    static const uint8_t which[] = { [XS_OP_MAP] = XP_ORDER_MAP, [XS_OP_UNMAP] = XP_ORDER_UNMAP, [XS_OP_PROTECT] = XP_ORDER_PROTECT,
                                     [XS_OP_CLONE] = XP_ORDER_CLONE, [XS_OP_EXIT] = XP_ORDER_UNMAP };
    xh_perf *p = xh_perf_self();
    if (ops[0].op <= XS_OP_EXIT && ops[0].op) xh_hist_add(&p->h[which[ops[0].op]], xh_ticks_ns(xh_counter() - t0));
  }
  if (rc) result = rc;
  else {
    for (unsigned i = 0; i < first; i++) log_map_failure(t, &sp->ops[i], "at the next order");
    for (unsigned i = 0; i < count; i++) if (sp->ops[first + i].result < 0) { result = sp->ops[first + i].result; break; }
  }
  pthread_mutex_unlock(&t->lock);
  return result;
}

static int64_t task_create_one(uint64_t *handle);

int64_t xh_task_create(uint64_t *handle)
{
  if (!T.available && T.sock < 0) return -ENOSYS;
  // One creation at a time, from the choice of the table entry to the factory's answer. The wait for the
  // answer gives T.lock up; without this, a second vCPU creating a task meanwhile took the same entry (not
  // marked used yet) and the same answer: two address spaces of the kernel then shared one handle and one
  // Linux process (a new process found the memory of another one in place), and the other process was lost.
  static pthread_mutex_t creating = PTHREAD_MUTEX_INITIALIZER;
  pthread_mutex_lock(&creating);
  int64_t created = task_create_one(handle);
  pthread_mutex_unlock(&creating);
  return created;
}

static int64_t task_create_one(uint64_t *handle)
{
  pthread_mutex_lock(&T.lock);
  task *t = NULL;
  for (unsigned i = 0; i < MAX_TASKS && !t; i++) if (!T.tasks[i].used) t = &T.tasks[i];
  int slot = t ? slot_alloc() : -1;
  if (slot < 0) { pthread_mutex_unlock(&T.lock); return -EAGAIN; }
  memset(t, 0, sizeof *t);
  pthread_mutex_init(&t->lock, NULL);
  pthread_mutex_init(&t->qlock, NULL);
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

// What every Linux thread of a task process is doing (state and system call from /proc), for the log, when
// the process does not answer: "tid state syscall-number first-arguments".
static void log_task_threads(int pid)
{
  char path[64], line[160];
  snprintf(path, sizeof path, "/proc/%d/task", pid);
  DIR *dir = opendir(path);
  if (!dir) { LOGE("tasks: pid %d: no /proc entry (%s)", pid, strerror(errno)); return; }
  struct dirent *e;
  int shown = 0;
  while ((e = readdir(dir)) != NULL && shown < 24) {
    if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
    char state = '?';
    snprintf(path, sizeof path, "/proc/%d/task/%s/stat", pid, e->d_name);
    FILE *f = fopen(path, "r");
    if (f) { if (fgets(line, sizeof line, f)) { char *close = strrchr(line, ')'); if (close && close[1] == ' ') state = close[2]; } fclose(f); }
    line[0] = 0;
    snprintf(path, sizeof path, "/proc/%d/task/%s/syscall", pid, e->d_name);
    f = fopen(path, "r");
    if (f) { if (fgets(line, 80, f)) line[strcspn(line, "\n")] = 0; fclose(f); }
    LOGE("tasks: pid %d thread %s: state %c, syscall %s", pid, e->d_name, state, line[0] ? line : "?");
    shown++;
  }
  closedir(dir);
}

// True once `seconds` have passed since `since` (CLOCK_MONOTONIC).
static bool seconds_passed(const struct timespec *since, int seconds)
{
  struct timespec now;
  clock_gettime(CLOCK_MONOTONIC, &now);
  return now.tv_sec - since->tv_sec > seconds || (now.tv_sec - since->tv_sec == seconds && now.tv_nsec >= since->tv_nsec);
}

// The task's slots go back to the pool. Only when its Linux process is gone: a slot is a page of the trap
// memfd that the process has mapped, and a thread of the old process that still waits on that page would
// answer the orders, or run the register state, of whoever gets the slot next. T.lock held.
static void task_release(task *t)
{
  for (unsigned s = 0; s < MAX_SLOTS; s++)
    if (T.threads[s].used && T.threads[s].task == t - T.tasks) { T.threads[s].used = false; slot_free(s); }
  slot_free(t->service_slot);
  t->used = t->zombie = false;
}

int64_t xh_task_destroy(uint64_t handle)
{
  task *t = task_of(handle);
  if (!t) return -ESRCH;
  if (!t->dead) {
    struct message m = { .kind = M_KILL, .pid = t->pid };
    send(T.sock, &m, sizeof m, MSG_NOSIGNAL);
    kill(t->pid, SIGKILL);                 // at once; the factory's report (the reader marks it dead) is what is waited for
    // By the clock: usleep returns early whenever a signal arrives, and the vCPU threads get them all the time.
    struct timespec began, step = { 0, 2 * 1000 * 1000 };
    clock_gettime(CLOCK_MONOTONIC, &began);
    while (!t->dead && !seconds_passed(&began, 20)) nanosleep(&step, NULL);
  }
  pthread_mutex_lock(&T.lock);
  if (t->dead) task_release(t);
  else {
    // Never seen so far. The slots stay taken until the factory reports the exit (reader_main).
    LOGE("tasks: pid %d did not die in 20 s: its slots are kept until it does", t->pid);
    t->zombie = true;
  }
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
  xh_perf *p = xh_perf_self();
  if (T.map_defer) {
    // A TLB fill: it must be in place before any thread of the task touches the page again through this entry,
    // and before any later unmap or protect of the task; the queue gives both (task.npending). A fill that comes
    // late only costs the toucher one more fault, which the kernel fills again.
    pthread_mutex_lock(&t->qlock);
    bool queued = !t->dead && t->npending < XS_PENDING_MAX;
    if (queued) t->pending[t->npending++] = op;
    pthread_mutex_unlock(&t->qlock);
    if (queued) { p->maps_deferred++; return 0; }
    if (t->dead) return -ESRCH;
  }
  p->maps_ordered++;
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

// The deferred maps of a task, ordered now (before a read or write of its memory from outside).
static void flush_pending(task *t)
{
  if (__atomic_load_n(&t->npending, __ATOMIC_RELAXED)) order(t, NULL, 0);
}

int64_t xh_task_read(uint64_t handle, uint64_t va, void *buffer, uint64_t size)
{
  task *t = task_of(handle);
  if (!t || t->dead) return -ESRCH;
  flush_pending(t);
  struct iovec local = { buffer, size }, remote = { (void *)(uintptr_t)va, size };
  ssize_t n = process_vm_readv(t->pid, &local, 1, &remote, 1, 0);
  return n < 0 ? -errno : n;
}

int64_t xh_task_write(uint64_t handle, uint64_t va, const void *buffer, uint64_t size)
{
  task *t = task_of(handle);
  if (!t || t->dead) return -ESRCH;
  flush_pending(t);
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
  if (!rc) {
    rc = wait_word(t, &tp->to_host, 10);             // its first stop (XH_TRAP_BOOT)
    // The clone was answered and the new thread never reported: say what the host knows about it.
    if (rc) LOGE("tasks: pid %d: the thread made in area %d did not reach its first stop (%s): tid %u, the process is %s",
                 t->pid, area, rc == -ESRCH ? "the task died" : "10 s", __atomic_load_n(&tp->tid, __ATOMIC_ACQUIRE),
                 kill(t->pid, 0) == 0 ? "there" : "gone");
  }
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
    // Linux zeroes tid when the thread is gone. Waited for by the clock (an interrupted futex wait returns at
    // once): until then the thread still has this slot's page, and the slot must not go to another thread,
    // whose register state it would take and run in this task's address space.
    struct timespec began, step = { 0, 20 * 1000 * 1000 };
    clock_gettime(CLOCK_MONOTONIC, &began);
    uint32_t tid;
    while ((tid = __atomic_load_n(&tp->tid, __ATOMIC_ACQUIRE)) != 0 && !t->dead && !seconds_passed(&began, 10))
      futex(&tp->tid, FUTEX_WAIT, tid, &step);
    if (tid != 0 && !t->dead) {
      // Never seen so far. The slot and the area stay with the task; xh_task_destroy takes them back.
      LOGE("tasks: thread %u of pid %d did not exit in 10 s: its slot is kept until the task goes", tid, t->pid);
      log_task_threads(t->pid);
      return 0;
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

// Debugging (SIGQUIT to xnuhost): every iOS thread, where it last trapped or was released, and whether it is
// in user code now (released and not yet back). A running thread is not disturbed.
void xh_tasks_dump(void)
{
  for (unsigned s = 0; s < MAX_SLOTS; s++) {
    if (!T.threads[s].used) continue;
    task *t = &T.tasks[T.threads[s].task];
    struct xs_trap_page *tp = slot_page(s);
    bool running = __atomic_load_n(&tp->to_task, __ATOMIC_ACQUIRE) || !__atomic_load_n(&tp->to_host, __ATOMIC_ACQUIRE);
    LOGI("thread %u of task pid %d (tid %u): %s; last trap kind %u far 0x%llx insn 0x%x; pc 0x%llx lr 0x%llx sp 0x%llx x16 0x%llx x0 0x%llx x1 0x%llx x2 0x%llx", s, t->pid,
         tp->tid, t->dead ? "task gone" : running ? "in user code since the state shown" : "stopped in the kernel's hands", tp->trap.kind,
         (unsigned long long)tp->trap.far, tp->trap.insn, (unsigned long long)tp->state.pc, (unsigned long long)tp->state.x[30],
         (unsigned long long)tp->state.sp, (unsigned long long)tp->state.x[16], (unsigned long long)tp->state.x[0], (unsigned long long)tp->state.x[1], (unsigned long long)tp->state.x[2]);
    // The return addresses up the frame-pointer chain, read from the task's process (scripts/debug/dsc.py names
    // them): what a thread that sits in one place is in the middle of. Nothing is printed when the read fails.
    if (!t->dead) {
      char line[14 * 20 + 1];
      size_t used = 0;
      uint64_t fp = tp->state.x[29];
      for (int depth = 0; depth < 14 && fp && !(fp & 7); depth++) {
        uint64_t pair[2];
        struct iovec local = { pair, sizeof pair }, remote = { (void *)(uintptr_t)fp, sizeof pair };
        if (process_vm_readv(t->pid, &local, 1, &remote, 1, 0) != (ssize_t)sizeof pair) break;
        used += (size_t)snprintf(line + used, sizeof line - used, " 0x%llx", (unsigned long long)(pair[1] & 0x7fffffffffffULL));
        if (pair[0] <= fp) break;
        fp = pair[0];
      }
      if (used) LOGI("thread %u frames:%s", s, line);
    }
  }
}

// thread_run's end, for perf.c: the trap's kind and when it was handed to the kernel.
static void run_done(xh_vcpu *v, uint32_t kind)
{
  xh_perf *p = &v->perf;
  p->traps[kind & 15]++;
  p->ret_at = xh_counter();
  p->ret_idle = p->ticks_idle;
  p->ret_kind = kind;
}

int64_t xh_thread_run(xh_vcpu *v, uint64_t handle, struct xh_user_state *state, struct xh_trap *trap)
{
  thread *th = thread_of(handle);
  if (!th) return -ESRCH;
  task *t = &T.tasks[th->task];
  struct xs_trap_page *tp = slot_page((uint32_t)handle);
  xh_perf *p = &v->perf;
  uint64_t entered = xh_counter();
  // The kernel's work since the last trap this vCPU handed it (its idle time taken out).
  if (p->ret_at) {
    uint64_t gap = entered - p->ret_at, idle = p->ticks_idle - p->ret_idle;
    unsigned k = p->ret_kind == XH_TRAP_SYSCALL ? XP_K_SYSCALL : p->ret_kind == XH_TRAP_FAULT ? XP_K_FAULT :
                 p->ret_kind == XH_TRAP_INTERRUPT ? XP_K_IRQ : XP_K_OTHER;
    xh_hist_add(&p->h[k], xh_ticks_ns(gap > idle ? gap - idle : 0));
    p->ret_at = 0;
  }
  memset(trap, 0, sizeof *trap);
  if (t->dead) { trap->kind = XH_TRAP_EXITED; trap->exit_status = t->exit_status; run_done(v, trap->kind); return 0; }
  // An interrupt already waiting is taken before any user instruction runs.
  if (v->cpu.irq_pending) { trap->kind = XH_TRAP_INTERRUPT; run_done(v, trap->kind); return 0; }
  // iOS code runs on the task's Linux thread, not on the vCPU's: it follows the vCPU's host CPUs.
  const uint8_t epoch = (uint8_t)__atomic_load_n(&xh_cpu_place_epoch, __ATOMIC_ACQUIRE);
  if (xh_cpu_place && (th->placed != v->cpu.index + 1 || th->epoch != epoch)) {
    uint32_t tid = __atomic_load_n(&tp->tid, __ATOMIC_ACQUIRE);
    if (tid) { xh_cpu_place((int)tid, v->cpu.index); th->placed = (uint8_t)(v->cpu.index + 1); th->epoch = epoch; }
  }
  // The task's deferred maps go with this release: the thread carries them out before it runs. The task's lock is
  // held until it says they are done, so no unmap or protect of the task can pass them.
  bool draining = false;
  unsigned nops = 0;
  if (__atomic_load_n(&t->npending, __ATOMIC_RELAXED)) {
    pthread_mutex_lock(&t->lock);
    pthread_mutex_lock(&t->qlock);
    nops = t->npending;
    if (nops && !t->dead) {
      memcpy((uint8_t *)tp + XS_OPS_OFF, t->pending, nops * sizeof(struct xs_op));
      t->npending = 0;
      pthread_mutex_unlock(&t->qlock);
      tp->ops_done = 0;
      tp->nops = nops;
      draining = true;
    } else { pthread_mutex_unlock(&t->qlock); pthread_mutex_unlock(&t->lock); }
  }
  tp->state = *state;
  tp->command = XS_RUN;
  tp->spin_ticks = T.spin_task;
  __atomic_store_n(&tp->to_host, 0, __ATOMIC_RELEASE);
  uint64_t released = xh_counter();
  th->released_at = released;
  // stub_abi.h: store, then wake only a sleeper.
  __atomic_store_n(&tp->to_task, 1, __ATOMIC_RELEASE);
  __atomic_thread_fence(__ATOMIC_SEQ_CST);
  if (__atomic_load_n(&tp->task_sleeping, __ATOMIC_RELAXED)) { futex(&tp->to_task, FUTEX_WAKE, 1, NULL); p->task_wakes++; }
  bool kicked = false, sleeping = false;
  uint64_t kicked_at = 0;
  struct timespec step = { 0, 200 * 1000 * 1000 };
  for (;;) {
    if (draining && __atomic_load_n(&tp->ops_done, __ATOMIC_ACQUIRE)) {
      struct xs_op *ops = (struct xs_op *)((uint8_t *)tp + XS_OPS_OFF);
      for (unsigned i = 0; i < nops; i++) log_map_failure(t, &ops[i], "in the released thread");
      pthread_mutex_unlock(&t->lock);
      draining = false;
      xh_hist_add(&p->h[XP_DRAIN], xh_ticks_ns(xh_counter() - released));
    }
    if (!draining && __atomic_load_n(&tp->to_host, __ATOMIC_ACQUIRE)) break;
    if (t->dead) {
      if (draining) pthread_mutex_unlock(&t->lock);
      if (sleeping) __atomic_store_n(&tp->host_sleeping, 0, __ATOMIC_RELAXED);
      trap->kind = XH_TRAP_EXITED; trap->exit_status = t->exit_status;
      p->ticks_user += xh_counter() - released;
      run_done(v, trap->kind);
      return 0;
    }
    // The vCPU's interrupt line rose (its timer or another CPU signalled this thread, which ended
    // the wait below): force the iOS thread to stop, once.
    if ((v->cpu.irq_pending || xh_stopping()) && !kicked) {
      uint32_t tid = __atomic_load_n(&tp->tid, __ATOMIC_ACQUIRE);
      kicked_at = xh_counter();
      if (tid) syscall(SYS_tgkill, t->pid, tid, KICK_SIGNAL);
      kicked = true;
    }
    if (!sleeping) {
      // Spin a while: a thread that comes back at once (a system call after a few instructions, a fault) is seen
      // without a futex sleep and wake on either side.
      if (T.spin_host && xh_counter() - released < T.spin_host) { __asm__ volatile("yield"); continue; }
      __atomic_store_n(&tp->host_sleeping, 1, __ATOMIC_RELAXED);
      __atomic_thread_fence(__ATOMIC_SEQ_CST);
      sleeping = true;
      continue;                                  // look at the words once more before the first sleep
    }
    p->host_sleeps++;
    futex(&tp->to_host, FUTEX_WAIT, 0, &step);
  }
  if (sleeping) __atomic_store_n(&tp->host_sleeping, 0, __ATOMIC_RELAXED);
  uint64_t seen = xh_counter();
  p->ticks_user += seen - released;
  *state = tp->state;
  *trap = tp->trap;
  // The two hand-offs of this stay (counter ticks of the same clock in both processes).
  uint64_t woken = tp->t_woken, entered_stub = tp->t_enter;
  if (woken >= released && woken <= seen) xh_hist_add(&p->h[XP_DOWN], xh_ticks_ns(woken - released));
  if (entered_stub >= woken && entered_stub <= seen) xh_hist_add(&p->h[XP_UP], xh_ticks_ns(seen - entered_stub));
  if (tp->slept) p->task_sleeps++;
  if (kicked && trap->kind == XH_TRAP_INTERRUPT) xh_hist_add(&p->h[XP_KICK], xh_ticks_ns(seen - kicked_at));
  if (xh_text_fixup_svc && trap->kind == XH_TRAP_ILLEGAL && trap->insn == XH_SVC_MARK) {
    // The system call text_fixup marked: as a trapped svc reports it, with pc after the instruction.
    trap->kind = XH_TRAP_SYSCALL;
    trap->svc = 0x80;
    state->pc += 4;
  }
  run_done(v, trap->kind);
  v->stat_traps++;
  // A trap no fault handler of the kernel can resolve by mapping a page: a data or instruction address outside
  // what an iOS task may use (a pointer that kept its authentication code, a wild jump), an undefined instruction,
  // a breakpoint. They are rare and end the process: said here with the registers, for a boot where the kernel
  // does not log that process's exceptions (the first 64 of a run).
  static unsigned told;
  bool outside = trap->kind == XH_TRAP_FAULT && (trap->far < 0x100000000ull || trap->far >= T.base);
  if ((outside || trap->kind == XH_TRAP_ILLEGAL || trap->kind == XH_TRAP_BRK) && __atomic_fetch_add(&told, 1, __ATOMIC_RELAXED) < 64)
    LOGI("tasks: pid %d (tid %u): trap kind %u signal %u far 0x%llx esr 0x%llx access %u insn 0x%x; pc 0x%llx lr 0x%llx sp 0x%llx fp 0x%llx "
         "x0 0x%llx x1 0x%llx x2 0x%llx x3 0x%llx x8 0x%llx x16 0x%llx x17 0x%llx", t->pid, __atomic_load_n(&tp->tid, __ATOMIC_ACQUIRE),
         trap->kind, trap->signo, (unsigned long long)trap->far, (unsigned long long)trap->esr, trap->access, trap->insn,
         (unsigned long long)state->pc, (unsigned long long)state->x[30], (unsigned long long)state->sp, (unsigned long long)state->x[29],
         (unsigned long long)state->x[0], (unsigned long long)state->x[1], (unsigned long long)state->x[2], (unsigned long long)state->x[3],
         (unsigned long long)state->x[8], (unsigned long long)state->x[16], (unsigned long long)state->x[17]);
  return 0;
}

#else   // not AArch64 Linux: the filter above is all there is

int xh_tasks_init(const xh_config *c) { (void)c; return 0; }
bool xh_tasks_available(void) { return false; }
bool xh_tasks_seccomp(void) { return false; }
void xh_tasks_dump(void) {}
uint64_t xh_tasks_va_limit(void) { return 0; }
void xh_tasks_shutdown(void) {}

#endif
