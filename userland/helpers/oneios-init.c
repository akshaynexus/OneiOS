// SPDX-License-Identifier: MIT
// oneios-init: starts and supervises OneiOS's own services in the iOS guest, outside launchd.
//
// launchd on iOS 27 runs only the labels of its own compiled job cache, so a service of ours has no label to
// run under on a sealed root. As Droid-VM/ownervisor does for UniXNU-droidvm, one manager is started once at
// boot as root (by the kernel's launch policy on a sealed root, by a launchd job on an edited root) and
// supervises the rest. This is a smaller manager than ownervisor: C only, no Foundation, no sockets.
//
//   $ONEIOS_ROOT (default /private/var/oneios)
//     services.d/NAME.conf   a service (below)
//     enabled/NAME           present: started at boot
//     log/NAME.log           its stdout and stderr (kept to 1 MiB, one older file)
//     state/status           one line per service: name pid state starts last-exit
//     run/oneios-init.pid    this process (locked: a second manager exits)
//
//   NAME.conf: lines of key=value, # comments.
//     arg=/private/var/oneios/bin/dropbear     one per argument, the first is the program (required)
//     env=NAME=value                           added to the manager's environment
//     uid=501  gid=501                         numbers; without them the service runs as the manager (root)
//     keepalive=1                              start again when it exits (default 0: run once)
//     throttle=5                               at least this many seconds between two starts (default 5)
//     bootstrap=1                              wait for launchd's bootstrap port first (default 1; Darwin)
//
//   SIGHUP: read services.d and enabled again (new ones start, removed or disabled ones stop).
//   SIGUSR1: write state/status now. SIGTERM: stop every service (SIGTERM, 5 s, SIGKILL) and exit.
//
// Started by the kernel while launchd is being exec'd, this task has no bootstrap port, and a service spawned
// then could never reach launchd. The kernel copies launchd's port into this task once launchd is up
// (UniXNU-droidvm package/DroidVMLaunchPolicy.c, droidvm_fix_ownervisor_bootstrap); services with bootstrap=1
// wait for it, up to BOOTSTRAP_WAIT seconds, and are started either way after that.
//
// Builds for the guest with an iOS SDK (helpers/build.sh) and for the host (cc) for the tests.
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#ifdef __APPLE__
#include <mach/mach.h>
#endif

#define MAX_SERVICES 32
#define MAX_ARGS 32
#define MAX_ENV 16
#define NAME_MAX_LEN 48
#define LOG_LIMIT (1024 * 1024)
#define BOOTSTRAP_WAIT 90
#define STOP_WAIT 5

struct service {
  char name[NAME_MAX_LEN];
  char *args[MAX_ARGS + 1];
  char *env[MAX_ENV + 1];
  int uid, gid, keepalive, throttle, bootstrap;
  int enabled, present;  // present: seen in the last scan
  pid_t pid;
  time_t started;
  unsigned starts;
  int last_status, done;  // done: ran once and is not kept alive
};

static struct service services[MAX_SERVICES];
static int service_count;
static char root[512];
static volatile sig_atomic_t got_hup, got_term, got_usr1, got_chld;
static time_t boot;

static void on_signal(int sig) {
  if (sig == SIGHUP) got_hup = 1;
  else if (sig == SIGTERM || sig == SIGINT) got_term = 1;
  else if (sig == SIGUSR1) got_usr1 = 1;
  else if (sig == SIGCHLD) got_chld = 1;
}

static void say(const char *format, ...) {
  char line[512];
  va_list ap;
  va_start(ap, format);
  vsnprintf(line, sizeof line, format, ap);
  va_end(ap);
  fprintf(stderr, "oneios-init: %s\n", line);
  fflush(stderr);
}

static int path(char *out, size_t size, const char *a, const char *b) {
  int n = b ? snprintf(out, size, "%s/%s/%s", root, a, b) : snprintf(out, size, "%s/%s", root, a);
  return n > 0 && (size_t)n < size ? 0 : -1;
}

// Service names become file names: letters, digits, '-', '_' and '.', not starting with a dot.
static int name_ok(const char *name) {
  size_t n = strlen(name);
  if (!n || n >= NAME_MAX_LEN || name[0] == '.') return 0;
  for (size_t i = 0; i < n; i++) {
    char c = name[i];
    if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '-' || c == '_' || c == '.')) return 0;
  }
  return 1;
}

static void free_config(struct service *s) {
  for (int i = 0; s->args[i]; i++) { free(s->args[i]); s->args[i] = NULL; }
  for (int i = 0; s->env[i]; i++) { free(s->env[i]); s->env[i] = NULL; }
}

// Reads NAME.conf into s (its run state is kept). 0, or -1 with a line said.
static int read_config(struct service *s) {
  char file[640], conf[NAME_MAX_LEN + 8], line[1024];
  snprintf(conf, sizeof conf, "%s.conf", s->name);
  if (path(file, sizeof file, "services.d", conf)) return -1;
  // A service file anyone else can write would run their program as root.
  struct stat st;
  if (stat(file, &st) || !S_ISREG(st.st_mode)) return -1;
  if (st.st_uid != geteuid() || (st.st_mode & 022)) {
    say("%s: services.d/%s.conf must belong to uid %d and not be group- or world-writable; ignored", s->name, s->name, (int)geteuid());
    return -1;
  }
  FILE *f = fopen(file, "r");
  if (!f) return -1;
  free_config(s);
  s->uid = s->gid = -1;  // as the manager
  s->keepalive = 0;
  s->throttle = 5;
  s->bootstrap = 1;
  int nargs = 0, nenv = 0, bad = 0;
  while (fgets(line, sizeof line, f)) {
    size_t n = strlen(line);
    while (n && (line[n - 1] == '\n' || line[n - 1] == '\r')) line[--n] = 0;
    if (!n || line[0] == '#') continue;
    char *eq = strchr(line, '=');
    if (!eq) { bad = 1; break; }
    *eq = 0;
    const char *key = line, *value = eq + 1;
    if (!strcmp(key, "arg")) {
      if (nargs == MAX_ARGS) { bad = 1; break; }
      s->args[nargs++] = strdup(value);
    } else if (!strcmp(key, "env")) {
      if (nenv == MAX_ENV || !strchr(value, '=')) { bad = 1; break; }
      s->env[nenv++] = strdup(value);
    } else if (!strcmp(key, "uid")) s->uid = atoi(value);
    else if (!strcmp(key, "gid")) s->gid = atoi(value);
    else if (!strcmp(key, "keepalive")) s->keepalive = atoi(value) != 0;
    else if (!strcmp(key, "throttle")) s->throttle = atoi(value);
    else if (!strcmp(key, "bootstrap")) s->bootstrap = atoi(value) != 0;
    else { bad = 1; break; }
  }
  fclose(f);
  s->args[nargs] = NULL;
  s->env[nenv] = NULL;
  if (s->throttle < 1) s->throttle = 1;
  if (bad || !nargs || s->args[0][0] != '/' || s->uid < -1 || s->gid < -1) {
    say("%s: services.d/%s.conf is not a service (key=value lines, arg= first an absolute path); ignored", s->name, s->name);
    free_config(s);
    return -1;
  }
  return 0;
}

static struct service *find(const char *name) {
  for (int i = 0; i < service_count; i++)
    if (!strcmp(services[i].name, name)) return &services[i];
  return NULL;
}

static void scan(void) {
  char dir[640], file[640];
  for (int i = 0; i < service_count; i++) services[i].present = 0;
  if (path(dir, sizeof dir, "services.d", NULL)) return;
  DIR *d = opendir(dir);
  if (!d) { say("no %s: nothing to start", dir); return; }
  struct dirent *e;
  while ((e = readdir(d))) {
    size_t n = strlen(e->d_name);
    if (n <= 5 || strcmp(e->d_name + n - 5, ".conf")) continue;
    char name[NAME_MAX_LEN];
    if (n - 5 >= sizeof name) continue;
    memcpy(name, e->d_name, n - 5);
    name[n - 5] = 0;
    if (!name_ok(name)) continue;
    struct service *s = find(name);
    if (!s) {
      if (service_count == MAX_SERVICES) { say("%s: more than %d services; ignored", name, MAX_SERVICES); continue; }
      s = &services[service_count];
      memset(s, 0, sizeof *s);
      snprintf(s->name, sizeof s->name, "%s", name);
      if (read_config(s)) continue;
      service_count++;
    } else if (read_config(s)) {
      continue;  // present stays 0: a running one is stopped
    }
    s->present = 1;
    int enabled = !path(file, sizeof file, "enabled", s->name) && !access(file, F_OK);
    if (enabled && !s->enabled) s->done = 0;  // enabled again: a run-once service runs once more
    s->enabled = enabled;
  }
  closedir(d);
}

static void write_status(void) {
  char file[640], tmp[660];
  if (path(file, sizeof file, "state", "status")) return;
  snprintf(tmp, sizeof tmp, "%s.tmp", file);
  FILE *f = fopen(tmp, "w");
  if (!f) return;
  for (int i = 0; i < service_count; i++) {
    struct service *s = &services[i];
    const char *state = s->pid > 0 ? "running" : !s->present ? "removed" : !s->enabled ? "disabled" : s->done ? "done" : "waiting";
    fprintf(f, "%s %d %s %u %d\n", s->name, (int)s->pid, state, s->starts, s->last_status);
  }
  fclose(f);
  rename(tmp, file);
}

static int open_log(const char *name) {
  char file[640], old[660], leaf[NAME_MAX_LEN + 8];
  snprintf(leaf, sizeof leaf, "%s.log", name);
  if (path(file, sizeof file, "log", leaf)) return -1;
  struct stat st;
  if (!stat(file, &st) && st.st_size > LOG_LIMIT) {
    snprintf(old, sizeof old, "%s.1", file);
    rename(file, old);
  }
  return open(file, O_WRONLY | O_CREAT | O_APPEND | O_NOFOLLOW, 0640);
}

#ifdef __APPLE__
static int have_bootstrap(void) {
  mach_port_t port = MACH_PORT_NULL;
  if (task_get_special_port(mach_task_self(), TASK_BOOTSTRAP_PORT, &port) != KERN_SUCCESS) return 0;
  if (!MACH_PORT_VALID(port)) return 0;
  mach_port_deallocate(mach_task_self(), port);
  return 1;
}
#else
// The host build has no bootstrap port; ONEIOS_FAKE_BOOTSTRAP names a file that stands for it in the tests.
static int have_bootstrap(void) {
  const char *fake = getenv("ONEIOS_FAKE_BOOTSTRAP");
  return !fake || !access(fake, F_OK);
}
#endif

static void start(struct service *s) {
  int log = open_log(s->name);
  pid_t pid = fork();
  if (pid < 0) { say("%s: fork: %s", s->name, strerror(errno)); if (log >= 0) close(log); return; }
  if (!pid) {
    signal(SIGHUP, SIG_DFL); signal(SIGTERM, SIG_DFL); signal(SIGINT, SIG_DFL);
    signal(SIGUSR1, SIG_DFL); signal(SIGCHLD, SIG_DFL); signal(SIGPIPE, SIG_DFL);
    sigset_t none;
    sigemptyset(&none);
    sigprocmask(SIG_SETMASK, &none, NULL);
    setsid();
    int null = open("/dev/null", O_RDONLY);
    if (null >= 0) dup2(null, 0);
    if (log >= 0) { dup2(log, 1); dup2(log, 2); }
    for (int fd = 3; fd < 256; fd++) close(fd);
    for (int i = 0; s->env[i]; i++) putenv(s->env[i]);
    // Group first: after setuid the process may no longer change it.
    // A service given another user must not keep root's supplementary groups.
    if (!geteuid() && (s->uid > 0 || s->gid > 0)) {
      gid_t group = (gid_t)(s->gid >= 0 ? s->gid : s->uid);
      if (setgroups(1, &group)) { fprintf(stderr, "oneios-init: %s: setgroups: %s\n", s->name, strerror(errno)); _exit(126); }
    }
    if ((s->gid >= 0 && s->gid != (int)getegid() && setgid((gid_t)s->gid)) ||
        (s->uid >= 0 && s->uid != (int)geteuid() && setuid((uid_t)s->uid))) {
      fprintf(stderr, "oneios-init: %s: cannot become %d:%d: %s\n", s->name, s->uid, s->gid, strerror(errno));
      _exit(126);
    }
    execv(s->args[0], s->args);
    fprintf(stderr, "oneios-init: %s: exec %s: %s\n", s->name, s->args[0], strerror(errno));
    _exit(127);
  }
  if (log >= 0) close(log);
  s->pid = pid;
  s->started = time(NULL);
  s->starts++;
  say("%s: started, pid %d", s->name, (int)pid);
}

static void reap(void) {
  int status;
  pid_t pid;
  while ((pid = waitpid(-1, &status, WNOHANG)) > 0) {
    for (int i = 0; i < service_count; i++) {
      struct service *s = &services[i];
      if (s->pid != pid) continue;
      s->pid = 0;
      s->last_status = WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
      s->done = !s->keepalive;
      say("%s: pid %d ended, status %d%s", s->name, (int)pid, s->last_status, s->keepalive ? "; it is started again" : "");
    }
  }
}

// Starts what is due. The seconds until the next thing is due (1..60).
static int tick(void) {
  time_t now = time(NULL);
  int wait = 60, bootstrap = have_bootstrap(), late = now - boot >= BOOTSTRAP_WAIT;
  static int said_late;
  for (int i = 0; i < service_count; i++) {
    struct service *s = &services[i];
    if (s->pid > 0) {
      if (!s->present || !s->enabled) { say("%s: stopped (%s)", s->name, s->present ? "disabled" : "removed"); kill(s->pid, SIGTERM); }
      continue;
    }
    if (!s->present || !s->enabled || s->done) continue;
    if (s->bootstrap && !bootstrap && !late) {
      int left = (int)(BOOTSTRAP_WAIT - (now - boot));
      if (left < wait) wait = left;
      continue;
    }
    if (s->bootstrap && !bootstrap && !said_late) {
      said_late = 1;
      say("no bootstrap port after %d s: services start without launchd", BOOTSTRAP_WAIT);
    }
    int since = (int)(now - s->started);
    if (s->starts && since < s->throttle) {
      if (s->throttle - since < wait) wait = s->throttle - since;
      continue;
    }
    start(s);
  }
  // Without a bootstrap port yet, look again soon: the kernel hands it over within seconds of launchd's start.
  if (!bootstrap && !late && wait > 1) wait = 1;
  return wait < 1 ? 1 : wait;
}

static void stop_all(void) {
  for (int i = 0; i < service_count; i++)
    if (services[i].pid > 0) kill(services[i].pid, SIGTERM);
  for (int waited = 0; waited < STOP_WAIT * 10; waited++) {
    reap();
    int left = 0;
    for (int i = 0; i < service_count; i++) left += services[i].pid > 0;
    if (!left) return;
    usleep(100000);
  }
  for (int i = 0; i < service_count; i++)
    if (services[i].pid > 0) { say("%s: did not stop, killed", services[i].name); kill(services[i].pid, SIGKILL); }
  reap();
}

int main(int argc, char **argv) {
  const char *r = getenv("ONEIOS_ROOT");
  if (argc == 2 && argv[1][0] == '/') r = argv[1];
  else if (argc != 1) { fprintf(stderr, "usage: oneios-init [/absolute/root]\n"); return 2; }
  if (!r || !*r) r = "/private/var/oneios";
  if (strlen(r) >= sizeof root - 128) { fprintf(stderr, "oneios-init: root path too long\n"); return 2; }
  snprintf(root, sizeof root, "%s", r);

  char file[640];
  static const char *const dirs[] = {"log", "state", "run"};
  for (unsigned i = 0; i < sizeof dirs / sizeof dirs[0]; i++)
    if (!path(file, sizeof file, dirs[i], NULL)) mkdir(file, 0755);
  if (path(file, sizeof file, "run", "oneios-init.pid")) return 2;
  int lock = open(file, O_RDWR | O_CREAT | O_NOFOLLOW, 0644);
  if (lock < 0 || flock(lock, LOCK_EX | LOCK_NB)) {
    fprintf(stderr, "oneios-init: %s: %s\n", file, lock < 0 ? strerror(errno) : "another oneios-init is running");
    return 1;
  }
  char pid[32];
  int n = snprintf(pid, sizeof pid, "%d\n", (int)getpid());
  if (ftruncate(lock, 0) || write(lock, pid, (size_t)n) != n) say("cannot write %s", file);

  // The manager's own lines go to its log; stderr may be nothing when the kernel starts it.
  int log = open_log("oneios-init");
  if (log >= 0) { dup2(log, 2); close(log); }

  struct sigaction sa;
  memset(&sa, 0, sizeof sa);
  sa.sa_handler = on_signal;
  sigaction(SIGHUP, &sa, NULL); sigaction(SIGTERM, &sa, NULL); sigaction(SIGINT, &sa, NULL);
  sigaction(SIGUSR1, &sa, NULL); sigaction(SIGCHLD, &sa, NULL);
  signal(SIGPIPE, SIG_IGN);
  // Signals are taken only inside sigsuspend-like waits below, so none is lost between a check and a sleep.
  sigset_t block, old;
  sigemptyset(&block);
  sigaddset(&block, SIGHUP); sigaddset(&block, SIGTERM); sigaddset(&block, SIGINT);
  sigaddset(&block, SIGUSR1); sigaddset(&block, SIGCHLD);
  sigprocmask(SIG_BLOCK, &block, &old);

  boot = time(NULL);
  say("root %s, pid %d, uid %d", root, (int)getpid(), (int)geteuid());
  scan();
  for (;;) {
    if (got_term) break;
    if (got_hup) { got_hup = 0; say("reading the services again"); scan(); }
    if (got_chld) { got_chld = 0; reap(); }
    int wait = tick();
    write_status();
    got_usr1 = 0;
    if (got_term || got_hup || got_chld) continue;
    // Sleep until a signal or until something is due: alarm + sigsuspend with the handled signals open.
    struct sigaction al;
    memset(&al, 0, sizeof al);
    al.sa_handler = on_signal;
    sigaction(SIGALRM, &al, NULL);
    alarm((unsigned)wait);
    sigsuspend(&old);
    alarm(0);
  }
  say("stopping");
  stop_all();
  write_status();
  return 0;
}
