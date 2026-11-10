// SPDX-License-Identifier: MIT
// xnuhost: boots a source-built XNU as a process on Android's Linux kernel (docs/booter.md).
#define _GNU_SOURCE
#include "xnuhost.h"
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include "bridges/devices.h"
#include "xnuloader/boot.h"
#include "xnuloader/machine.h"
#include "xnuloader/macho.h"

#define EXIT_GUEST_OFF 0
#define EXIT_SETUP 2
#define EXIT_RESTART 3
#define EXIT_PANIC 4

static xh_config config;
static struct { uint32_t requested, reason, code; char why[96]; } stop_state;
static pthread_mutex_t stop_lock = PTHREAD_MUTEX_INITIALIZER;
static bool guest_ready;
static uint64_t boot_fb_pa;
static unsigned fb_width, fb_height;

bool xh_stopping(void) { return __atomic_load_n(&stop_state.requested, __ATOMIC_ACQUIRE) != 0; }

void xh_stop(uint32_t reason, uint32_t code, const char *why)
{
  pthread_mutex_lock(&stop_lock);
  if (!stop_state.requested) {
    stop_state.reason = reason;
    stop_state.code = code;
    snprintf(stop_state.why, sizeof stop_state.why, "%s", why);
    __atomic_store_n(&stop_state.requested, 1, __ATOMIC_RELEASE);
  }
  pthread_mutex_unlock(&stop_lock);
}

static void session_file(const char *name, const char *content)
{
  char path[600];
  if (!*config.session || snprintf(path, sizeof path, "%s/%s", config.session, name) >= (int)sizeof path) return;
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
  if (fd < 0) { LOGE("session: %s: %s", path, strerror(errno)); return; }
  (void)!write(fd, content, strlen(content));
  close(fd);
}

// The guest's first frame is on screen (or the kernel said so): the switch's dead-man timer stops.
void xh_guest_ready(void)
{
  if (__atomic_exchange_n(&guest_ready, true, __ATOMIC_ACQ_REL)) return;
  session_file("ready", "ready\n");
  LOGI("guest ready: first frame");
}

static uint8_t *read_file(const char *path, size_t *size, size_t limit)
{
  struct stat st;
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return NULL;
  uint8_t *data = NULL;
  if (fstat(fd, &st) == 0 && st.st_size > 0 && (size_t)st.st_size <= limit && (data = malloc((size_t)st.st_size))) {
    size_t done = 0;
    while (done < (size_t)st.st_size) {
      ssize_t n = read(fd, data + done, (size_t)st.st_size - done);
      if (n <= 0) { free(data); data = NULL; break; }
      done += (size_t)n;
    }
    *size = (size_t)st.st_size;
  }
  close(fd);
  return data;
}

// A sidecar of the kernel collection, DarwinBoot's naming: <kernel>.<suffix>.
static xl_blob sidecar(const char *suffix, size_t limit)
{
  char path[600];
  xl_blob blob = { 0 };
  size_t size = 0;
  if (snprintf(path, sizeof path, "%s.%s", config.kernel, suffix) >= (int)sizeof path) return blob;
  blob.data = read_file(path, &size, limit);
  blob.size = blob.data ? size : 0;
  if (blob.data) LOGI("sidecar %s: %zu bytes", path, size);
  return blob;
}

#if defined(__aarch64__) && defined(__linux__)
#include <sys/auxv.h>
#include <sys/prctl.h>
#ifndef HWCAP_PACA
#define HWCAP_PACA (1 << 30)
#endif

static uint64_t pac_sign(uint64_t value)
{
  register uint64_t x17 __asm__("x17") = value, x16 __asm__("x16") = 0x1234;
  __asm__ volatile("hint #8" : "+r"(x17) : "r"(x16));       // pacia1716: a NOP without PAC
  return x17;
}

// One key set for the kernel and every task: a pointer signed here must come out the same in a
// forked child (it would not after an exec, which is why nothing in the tree of xnuhost execs).
static bool pac_shared_by_fork(void)
{
  int pipe_fd[2];
  uint64_t probe = 0x0000001234567000ull, mine = pac_sign(probe), theirs = 0;
  if (pipe(pipe_fd)) return false;
  pid_t pid = fork();
  if (pid == 0) { uint64_t v = pac_sign(probe); (void)!write(pipe_fd[1], &v, 8); _exit(0); }
  close(pipe_fd[1]);
  bool ok = pid > 0 && read(pipe_fd[0], &theirs, 8) == 8 && theirs == mine;
  close(pipe_fd[0]);
  if (pid > 0) waitpid(pid, NULL, 0);
  return ok && mine != probe;
}

static unsigned user_va_bits(void)
{
  static const unsigned candidates[] = { 52, 48, 47, 42, 39, 36 };
  for (unsigned i = 0; i < sizeof candidates / sizeof *candidates; i++) {
    void *want = (void *)((1ull << candidates[i]) - 0x10000000ull), *p = mmap(want, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    if (p != MAP_FAILED) { munmap(p, 4096); if (p == want) return candidates[i]; }
  }
  return 0;
}

static void host_facts(void)
{
  g_info.abi = XH_ABI_VERSION;
  g_info.host_page_size = (uint32_t)sysconf(_SC_PAGESIZE);
  g_info.user_va_bits = user_va_bits();
  g_info.timebase_hz = xh_counter_hz();
  bool paca = getauxval(AT_HWCAP) & HWCAP_PACA;
  if (paca && pac_shared_by_fork()) g_info.features |= XH_FEATURE_PAC;
  if (prctl(PR_PAC_SET_ENABLED_KEYS, 0, 0, 0, 0) == 0) g_info.features |= XH_FEATURE_PAC_KEYS_OFF;   // a no-op call: only asks
  LOGI("host: page %u, user VA %u bits, counter %llu Hz, PAC %s%s", g_info.host_page_size, g_info.user_va_bits,
       (unsigned long long)g_info.timebase_hz, paca ? ((g_info.features & XH_FEATURE_PAC) ? "yes, keys shared across fork" : "present but keys differ after fork") : "no",
       (g_info.features & XH_FEATURE_PAC_KEYS_OFF) ? ", per-thread key control" : "");
}
#else
static void host_facts(void)
{
  g_info.abi = XH_ABI_VERSION;
  g_info.host_page_size = (uint32_t)sysconf(_SC_PAGESIZE);
  g_info.timebase_hz = xh_counter_hz();
  LOGI("host: not AArch64 Linux: the kernel cannot be entered here");
}
#endif

static void gic_line(unsigned cpu, bool line) { xh_cpu_line(cpu, line); }

// ---- the host bridges (runtime/bridges): CPU placement, cpufreq, haptics, Bluetooth, network, sound ---
static br_cpu_plan cpu_plan;
static br_haptic *haptic;
static bool net_up;

static void bridge_log(int level, const char *line) { xh_log(level == 0 ? XH_LOG_ERROR : level == 1 ? XH_LOG_INFO : XH_LOG_DEBUG, "%s", line); }
static void place(int tid, unsigned vcpu) { br_cpu_place(tid, &cpu_plan, vcpu); }
static int vcpu_tid(unsigned vcpu) { return vcpu < g_ncpu ? g_vcpu[vcpu].tid : 0; }
static uint64_t cpufreq_read(void *opaque, uint64_t offset, unsigned size) { return br_cpufreq_read(opaque, offset, size); }
static void cpufreq_write(void *opaque, uint64_t offset, unsigned size, uint64_t value) { br_cpufreq_write(opaque, offset, size, value); }

static bool net_script(const char *verb)
{
  char command[700];
  if (!*config.runtime || snprintf(command, sizeof command, "sh %s/bin/oneios-net.sh %s", config.runtime, verb) >= (int)sizeof command) return false;
  int status = system(command);
  if (status) LOGE("network: `oneios-net.sh %s` ended with status %d: the guest has no route out", verb, status);
  return status == 0;
}

static int boot(void)
{
  const char *error = NULL;
  static xl_kernel kernel;
  size_t image_size = 0;
  uint8_t *image = read_file(config.kernel, &image_size, 1ull << 31);
  if (!image) { LOGE("cannot read the kernel collection %s: %s", config.kernel, strerror(errno)); return -1; }
  if (xl_macho_parse(image, image_size, &kernel, &error)) { LOGE("%s: %s", config.kernel, error); return -1; }
  LOGI("kernel: %s, %s %s, link base 0x%llx, %u segments, %u members", config.kernel, kernel.fileset ? "collection" : "MH_EXECUTE",
       kernel.cpusubtype == 2 ? "arm64e" : "arm64", (unsigned long long)kernel.base, kernel.nseg, kernel.nentries);
  if (kernel.cpusubtype == 2 && !(g_info.features & XH_FEATURE_PAC)) { LOGE("an arm64e kernel needs pointer authentication with keys shared across fork; this host has none"); return -1; }

  // RAM: the kernel's window, then the boot framebuffer the kernel's console draws into.
  uint64_t fb_bytes = ((uint64_t)fb_width * fb_height * 4 + 0x1fffff) & ~0x1fffffull;
  uint64_t window = config.ram_bytes - fb_bytes;
  if (xh_mem_map(kernel.base) || xh_mem_io(XL_BOARD_IO_BASE, XL_BOARD_IO_SIZE)) return -1;
  uint64_t phys = g_mem.phys;                       // physical = where it is mapped (mem.c)
  boot_fb_pa = phys + window;
  if (xh_mem_kva() == 0) { g_info.kva_base = g_mem.kva; g_info.kva_size = g_mem.kva_size; }
  LOGI("memory: %llu MiB at 0x%llx, physical = virtual (slide 0x%llx); I/O aperture 0x%llx",
       (unsigned long long)(window >> 20), (unsigned long long)phys,
       (unsigned long long)((uintptr_t)g_mem.host - kernel.base), (unsigned long long)(uintptr_t)g_mem.io_host);

  // The board: GIC, console, disks, input, display.
  xl_device_kind kinds[XL_MAX_DEVICES];
  unsigned ndev = 0;
  xh_gic_init(config.ncpu, XL_BOARD_GIC_DIST, XL_BOARD_GIC_REDIST);
  xh_gic_notify = gic_line;
  xh_gic_map();
  xh_uart_init(XL_BOARD_UART);
  for (unsigned i = 0; i < config.ndisk; i++) {
    xh_blk *disk = xh_blk_open(&config.disk[i], &error);
    if (!disk) { LOGE("disk %s: %s (%s)", config.disk[i].path, error, strerror(errno)); return -1; }
    if (!xh_virtio_blk_create(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, disk)) return -1;
    LOGI("disk %u: %s, %llu MiB, %s", ndev, config.disk[i].path, (unsigned long long)(xh_blk_bytes(disk) >> 20),
         config.disk[i].data_store ? "Data store" : config.disk[i].writable ? "writable" : "read-only");
    kinds[ndev++] = config.disk[i].data_store ? XL_DEV_DATA_STORE : XL_DEV_BLOCK;
  }
  xh_virtio *input = xh_virtio_input_create(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev);
  kinds[ndev++] = XL_DEV_INPUT;
  if (!input || !xh_virtio_gpu_create(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, fb_width, fb_height)) return -1;
  kinds[ndev++] = XL_DEV_GPU;
  if (config.input && xh_input_start(input, fb_width, fb_height)) LOGE("input: could not start; continuing without");

  static xl_machine machine;
  machine.chip_id = 0x8150;
  // The guest's unique chip ID: made once, kept with the guest's data, so its MAC addresses and
  // everything keyed on the ID stay the same across sessions.
  if (*config.data) {
    char path[600];
    size_t size = 0;
    snprintf(path, sizeof path, "%s/unique-chip-id", config.data);
    uint8_t *saved = read_file(path, &size, 64);
    if (saved && size == 8) memcpy(&machine.unique_chip_id, saved, 8);
    else if (getentropy(&machine.unique_chip_id, 8) == 0) {
      machine.unique_chip_id &= 0x0000ffffffffffffull;
      int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
      if (fd >= 0) { (void)!write(fd, &machine.unique_chip_id, 8); close(fd); }
    }
    free(saved);
    machine.have_chip_id = machine.unique_chip_id != 0;
  }

  // The bridges. Each one whose Linux side is missing says so and is left out; nothing else changes.
  if (config.bridges) {
    br_log_sink = bridge_log;
    br_cpu_topology host;
    if (br_cpu_read("/sys", &host) == 0 && br_cpu_plan_make(&host, config.ncpu, false, &cpu_plan) == 0) {
      for (unsigned i = 0; i < config.ncpu; i++) machine.cpu_cluster[i] = cpu_plan.v[i].type;      // E vCPUs first, then P
      xh_cpu_place = place;
      br_cpufreq *cpufreq = br_cpufreq_create(&cpu_plan, vcpu_tid);
      if (cpufreq && xh_mmio_add(&(xh_mmio_region){ .base = XL_BOARD_CPUFREQ, .size = (uint64_t)config.ncpu * BR_CPUFREQ_WINDOW, .name = "cpufreq",
                                                    .opaque = cpufreq, .read = cpufreq_read, .write = cpufreq_write }) == 0) {
        machine.cpufreq_base = XL_BOARD_CPUFREQ;
        machine.cpufreq_size = (uint64_t)config.ncpu * BR_CPUFREQ_WINDOW;
      }
      LOGI("cpu: %u E + %u P vCPUs on the host's %u E + %u P cores", cpu_plan.n_e, cpu_plan.n_p, host.n_e, host.n_p);
    } else LOGI("cpu: no host topology under /sys: vCPUs are not placed");

    bool hidl = !strcmp(config.bluetooth, "hidl");
    haptic = br_haptic_open(NULL);
    br_bridge_config bridge = { .haptic = haptic, .hci_factory = hidl ? br_hci_open_hidl : NULL };
    if (br_bridge_create(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, &bridge)) kinds[ndev++] = XL_DEV_BRIDGE;
    if (hidl && br_bt_address("/mnt/vendor/efs/bluetooth/bt_addr", machine.bt_mac) == 0) {
      machine.bt_transport = 3;                     // the phone's controller, H4 over the bridge
      machine.bt_speed = 3000000;
    }

    br_net_config net;
    br_net_config_defaults(&net);
    xl_machine_mac(&machine, 1, net.guest_mac);     // en0
    if (br_virtio_net_create(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, &net, br_net_open_tap)) {
      kinds[ndev++] = XL_DEV_NET;
      net_up = net_script("up");                    // address, forwarding and NAT for the tap; undone at stop
    }

    char helper[600];
    void *audio_ctx = NULL;
    const br_audio_backend *audio = NULL;
    if (*config.runtime && snprintf(helper, sizeof helper, "%s/bin/oneios-audio", config.runtime) < (int)sizeof helper) audio = br_audio_helper(helper, &audio_ctx);
    if (!audio) audio = br_audio_null(&audio_ctx);
    if (audio && br_virtio_snd_create(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, audio, audio_ctx)) kinds[ndev++] = XL_DEV_SND;
  }

  // The device tree.
  xl_machine_board(&machine, config.ncpu, kinds, ndev);
  machine.timebase_hz = (uint32_t)g_info.timebase_hz;
  machine.dram_base = phys;
  machine.dram_size = window;
  machine.model = "iPhone18,2";
  machine.host_vector = (uint64_t)(uintptr_t)xh_vector();
  machine.user_va_limit = xh_tasks_va_limit();
  machine.host_page_size = g_info.host_page_size;
  if (getentropy(machine.random_seed, sizeof machine.random_seed)) { LOGE("no entropy for the kernel's seed"); return -1; }
  xl_blob ramdisk = sidecar("ramdisk", 1ull << 30), trust = sidecar("trustcache", 16 << 20), addon = sidecar("gpuaddon", 64 << 20),
          policies = sidecar("vroot", 128 << 20);
  machine.ramdisk = ramdisk.data != NULL;
  machine.trust_cache = trust.data != NULL;
  machine.gpu_addon = addon.data != NULL;
  machine.root_policies = policies.data != NULL;
  xl_adt_node *identity = NULL;
  if (*config.identity) {
    size_t size = 0;
    uint8_t *data = read_file(config.identity, &size, 16 << 20);
    identity = data ? xl_adt_parse(data, size, &error) : NULL;
    if (!identity) { LOGE("identity tree %s: %s", config.identity, data ? error : strerror(errno)); return -1; }
    machine.identity = identity;
  }
  xl_adt_node *tree = xl_machine_tree(&machine, &error);
  if (!tree) { LOGE("device tree: %s", error); return -1; }
  // A device tree shipped beside the kernel replaces the generated one node by node at the top
  // level, except the nodes that describe this machine.
  xl_blob shipped = sidecar("adt", 16 << 20);
  if (shipped.data) {
    xl_adt_node *other = xl_adt_parse(shipped.data, shipped.size, &error);
    if (!other) { LOGE("%s.adt: %s", config.kernel, error); return -1; }
    static const char *const ours[] = { "cpus", "arm-io", "psci", "pram" };
    for (unsigned i = 0; i < other->nchildren; i++) {
      const char *name = xl_adt_name(other->children[i]);
      bool keep = false;
      for (unsigned k = 0; k < sizeof ours / sizeof *ours; k++) keep |= !strcmp(name, ours[k]);
      if (keep || !strcmp(name, "chosen")) continue;
      size_t size = xl_adt_flat_size(other->children[i]);
      uint8_t *flat = malloc(size);
      xl_adt_flatten(other->children[i], flat);
      xl_adt_remove(tree, name);
      xl_adt_node *copy = xl_adt_parse(flat, size, &error), *slot = xl_adt_add(tree, name);
      // xl_adt_add made an empty node of that name: fill it from the copy
      for (unsigned p = 0; copy && p < copy->nprops; p++) xl_adt_set(slot, copy->props[p].name, copy->props[p].value, copy->props[p].length);
      for (unsigned ch = 0; copy && ch < copy->nchildren; ch++) {
        size_t csize = xl_adt_flat_size(copy->children[ch]);
        uint8_t *cflat = malloc(csize);
        xl_adt_flatten(copy->children[ch], cflat);
        xl_adt_node *sub = xl_adt_parse(cflat, csize, &error), *into = xl_adt_add(slot, xl_adt_name(copy->children[ch]));
        for (unsigned p = 0; sub && p < sub->nprops; p++) xl_adt_set(into, sub->props[p].name, sub->props[p].value, sub->props[p].length);
        xl_adt_free(sub);
        free(cflat);
      }
      xl_adt_free(copy);
      free(flat);
    }
    // /chosen: the shipped properties are added to ours; the machine's own stay.
    const xl_adt_node *their_chosen = xl_adt_child(other, "chosen");
    xl_adt_node *chosen = xl_adt_find(tree, "chosen");
    for (unsigned p = 0; their_chosen && p < their_chosen->nprops; p++)
      if (!xl_adt_get(chosen, their_chosen->props[p].name))
        xl_adt_set(chosen, their_chosen->props[p].name, their_chosen->props[p].value, their_chosen->props[p].length);
    xl_adt_free(other);
  }

  xl_boot_request request = {
    .image = { image, image_size }, .kernel = &kernel, .ram = g_mem.host, .window = window, .phys_base = phys,
    .virt_base = (uint64_t)(uintptr_t)g_mem.host, .tree = tree, .command_line = config.boot_args,
    .video = { boot_fb_pa, 1, (uint64_t)fb_width * 4, fb_width, fb_height, 32 },
    .legacy_boot_args = config.legacy_boot_args, .ramdisk = ramdisk, .trustcache = trust, .gpu_addon = addon, .root_policies = policies,
  };
  xl_boot_layout layout;
  if (xl_boot_place(&request, &layout, &error)) { LOGE("placing the kernel: %s", error); return -1; }
  __builtin___clear_cache((char *)g_mem.host, (char *)g_mem.host + layout.used);
  LOGI("placed: entry 0x%llx, boot_args 0x%llx, device tree %llu bytes, %llu MiB of the window used; boot-args \"%s\"",
       (unsigned long long)layout.entry_va, (unsigned long long)layout.args_va, (unsigned long long)layout.tree_size,
       (unsigned long long)(layout.used >> 20), config.boot_args);

  g_info.ncpu = config.ncpu;
  g_info.ram_phys = phys; g_info.ram_virt = (uint64_t)(uintptr_t)g_mem.host; g_info.ram_size = window;
  g_info.io_phys = g_mem.io_phys; g_info.io_virt = (uint64_t)(uintptr_t)g_mem.io_host; g_info.io_size = g_mem.io_size;
  g_info.user_va_min = 0x100000000ull;
  g_info.user_va_limit = xh_tasks_va_limit();
  if (xh_tasks_available()) g_info.features |= XH_FEATURE_TASKS;
  memcpy(g_info.video, request.video, sizeof g_info.video);
  free(image);
  xl_adt_free(tree);
  xl_adt_free(identity);

  if (xh_cpu_init(config.ncpu)) { LOGE("cannot set up %u CPUs", config.ncpu); return -1; }
  if (!xh_vector()) { LOGE("this build cannot enter the kernel (not AArch64 Linux)"); return -1; }
  return xh_cpu_start(0, layout.entry_va, layout.args_va, (uint64_t)(uintptr_t)xh_vector(), (uint64_t)(uintptr_t)&g_vcpu[0].cpu);
}

int main(int argc, char **argv)
{
  xh_config_defaults(&config);
  if (xh_config_args(&config, argc, argv) || xh_config_finish(&config)) return EXIT_SETUP;
  xh_log_open(NULL, config.log_level);
  LOGI("xnuhost: session ABI %d, mode %s, pid %d", config.session_abi, config.mode, getpid());
  host_facts();
  if (config.selftest) {
    // What must hold on a phone before anything else is worth trying.
    bool ok = g_info.host_page_size <= 16384 && g_info.user_va_bits >= 39 && (g_info.features & XH_FEATURE_PAC);
    int fd = (int)syscall(SYS_memfd_create, "xnuhost-selftest", 0u);
    void *x = fd >= 0 && ftruncate(fd, 16384) == 0 ? mmap(NULL, 16384, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_SHARED, fd, 0) : MAP_FAILED;
    LOGI("selftest: executable shared memfd mapping: %s", x != MAP_FAILED ? "yes" : strerror(errno));
    xh_mem_map_limit();
    ok = ok && x != MAP_FAILED;
    if (xh_mem_create(64 << 20) == 0 && xh_tasks_init(&config) == 0) {
      LOGI("selftest: task factory: started; system-call trapping (seccomp): %s", xh_tasks_available() ? "yes" : "NO");
      ok = ok && xh_tasks_available();
      xh_tasks_shutdown();
    } else ok = false;
    LOGI("selftest: %s", ok ? "this host can run xnuhost" : "this host lacks something xnuhost needs (see above)");
    return ok ? 0 : 1;
  }

  // Order matters: the RAM memfd and the task factory come first, while this process is still
  // single-threaded and has nothing of the guest mapped.
  xh_mem_map_limit();
  if (xh_mem_create(config.ram_bytes)) return EXIT_SETUP;
  if (config.tasks && xh_tasks_init(&config)) return EXIT_SETUP;

  // Signals: the main thread waits for the stop signals; every thread started from here on
  // inherits them blocked. vCPU threads open the ones they take themselves.
  sigset_t stops;
  sigemptyset(&stops);
  sigaddset(&stops, SIGTERM); sigaddset(&stops, SIGINT); sigaddset(&stops, SIGHUP);
  for (int s = SIGRTMIN; s <= SIGRTMIN + 3; s++) sigaddset(&stops, s);
  pthread_sigmask(SIG_BLOCK, &stops, NULL);
  signal(SIGPIPE, SIG_IGN);

  if (*config.session) {
    char pid[32];
    snprintf(pid, sizeof pid, "%d\n", getpid());
    session_file("xnuhost.pid", pid);
  }
  if (xh_display_init(&config, &fb_width, &fb_height)) return EXIT_SETUP;
  if (boot()) { xh_tasks_shutdown(); return EXIT_SETUP; }

  sigset_t waited;
  sigemptyset(&waited);
  sigaddset(&waited, SIGTERM); sigaddset(&waited, SIGINT); sigaddset(&waited, SIGHUP);
  while (!xh_stopping()) {
    struct timespec tick = { 0, 100 * 1000 * 1000 };
    int signo = sigtimedwait(&waited, NULL, &tick);
    if (signo > 0) {
      char why[48];
      snprintf(why, sizeof why, "signal %d", signo);
      xh_stop(XH_HALT_OFF, 0, why);
    }
    // Until the guest's display driver presents, show what its kernel console draws.
    xh_display *d = xh_display_get();
    uint8_t *fb = xh_ram(boot_fb_pa, (uint64_t)fb_width * fb_height * 4);
    if (!guest_ready && d && fb && memcmp(d->pixels, fb, (size_t)d->stride * d->height)) {
      memcpy(d->pixels, fb, (size_t)d->stride * d->height);
      xh_display_flush(0, 0, d->width, d->height);
    }
  }

  LOGI("stopping: %s (reason %u, code %u)", stop_state.why, stop_state.reason, stop_state.code);
  for (unsigned i = 0; i < g_ncpu; i++)
    LOGI("cpu%u: %llu interrupts, %llu device accesses, %llu emulated instructions, %llu task traps", i,
         (unsigned long long)g_vcpu[i].stat_irqs, (unsigned long long)g_vcpu[i].stat_mmio, (unsigned long long)g_vcpu[i].stat_sysreg,
         (unsigned long long)g_vcpu[i].stat_traps);
  // Debugging aid: XNUHOST_DUMP=FILE writes the first 64 MiB of the kernel's window at the stop.
  if (getenv("XNUHOST_DUMP")) {
    FILE *dump = fopen(getenv("XNUHOST_DUMP"), "wb");
    if (dump) { fwrite(g_mem.host, 1, g_mem.size < (64u << 20) ? g_mem.size : (64u << 20), dump); fclose(dump); }
  }
  xh_tasks_shutdown();
  if (haptic) br_haptic_close(haptic);      // stops the motor
  if (net_up) net_script("down");
  xh_input_stop();
  xh_display_close();
  sync();                                   // the Data store's pages
  if (stop_state.reason == XH_HALT_RETURN) session_file("return.request", "the guest asked to return\n");
  int code = stop_state.reason == XH_HALT_RESTART ? EXIT_RESTART : stop_state.reason == XH_HALT_PANIC ? EXIT_PANIC :
             stop_state.code ? EXIT_PANIC : EXIT_GUEST_OFF;
  LOGI("exit %d", code);
  _exit(code);
}
