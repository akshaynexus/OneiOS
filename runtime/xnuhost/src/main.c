// SPDX-License-Identifier: MIT
// xnuhost: boots a source-built XNU as a process on Android's Linux kernel (docs/booter.md).
#define _GNU_SOURCE
#include "xnuhost.h"
#include "gpu3d.h"
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
#include "bridges/camera.h"
#include "imageset.h"
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
static unsigned fb_width, fb_height, fb_hz;

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
#include <pthread.h>
#include <sys/auxv.h>
#ifdef __ANDROID__
#include <android/api-level.h>
void android_set_application_target_sdk_version(int target);
#endif
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
  // Signing must also do something. One signature can be all zero bits by chance (1 in 128 with
  // the 7 signature bits of a 47-bit address space), so several values are tried.
  bool signs = false;
  for (uint64_t i = 0; i < 16 && !signs; i++) signs = pac_sign(probe + (i << 12)) != probe + (i << 12);
  return ok && signs;
}

// The same must hold for xnuhost's own threads: the kernel's code runs on the vCPU threads and signs
// pointers that every other vCPU and every task authenticates. Linux copies the keys to a new thread,
// but bionic's pthread start resets the thread's APIA key (prctl PR_PAC_RESET_KEYS) for programs whose
// target SDK is 31 or later, which a static binary is by default. main() lowers the target SDK first;
// this checks that it worked.
static void *pac_thread_probe(void *arg) { *(uint64_t *)arg = pac_sign(0x0000001234567000ull); return NULL; }
static bool pac_shared_by_threads(void)
{
  uint64_t theirs = 0;
  pthread_t thread;
  if (pthread_create(&thread, NULL, pac_thread_probe, &theirs)) return false;
  pthread_join(thread, NULL);
  return theirs == pac_sign(0x0000001234567000ull);
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
  bool by_fork = paca && pac_shared_by_fork(), by_thread = paca && pac_shared_by_threads();
  if (by_fork && by_thread) g_info.features |= XH_FEATURE_PAC;
  if (prctl(PR_PAC_SET_ENABLED_KEYS, 0, 0, 0, 0) == 0) g_info.features |= XH_FEATURE_PAC_KEYS_OFF;   // a no-op call: only asks
  LOGI("host: page %u, user VA %u bits, counter %llu Hz, PAC %s%s", g_info.host_page_size, g_info.user_va_bits,
       (unsigned long long)g_info.timebase_hz, paca ? ((g_info.features & XH_FEATURE_PAC) ? "yes, keys shared across fork and threads" : !by_fork ? "present but keys differ after fork" : "present but keys differ between threads") : "no",
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

// The pool behind /arm-io/virtio-gpu drm-host-pool, as Droid-VM's boards size it (--pre-alloc drm-host-mb=8):
// drm2kgsl keeps its first 2 MiB unused, the rest holds 16 KiB of response buffer per 3D context.
#define GPU_POOL_BYTES (8ull << 20)
static void gpu_log(int level, const char *line) { xh_log(level == 0 ? XH_LOG_ERROR : level == 1 ? XH_LOG_INFO : XH_LOG_DEBUG, "%s", line); }

// ---- the host bridges (runtime/bridges): CPU placement, cpufreq, haptics, Bluetooth, network, sound ---
static br_cpu_plan cpu_plan;
static br_haptic *haptic;
static br_camera *camera;          // the camera helper process (runtime/bridges/camera, bin/oneios-camera)
static br_bright *brightness;     // the panel's backlight and the light sensor (runtime/bridges/brightness)
static br_battery *battery;       // the phone's battery for iOS (runtime/bridges/battery)
static br_thermal *thermal;       // the phone's temperatures: iOS's pressure level and xnuhost's CPU step (runtime/bridges/thermal)
static br_cpu_topology cpu_host;  // the host's CPUs, and the plan as made: the thermal steps narrow it
static br_cpu_plan cpu_plan_base;
static bool cpu_planned;
extern unsigned xh_cpu_place_epoch;   // task.c: an iOS thread is placed again when it runs after this changes
void xh_sysreg_stats_log(void);       // sysreg.c: the most frequent trapped instructions
static bool net_up;
static bool wifi_up;                // wlan0 is the guest's en0 (runtime/bridges/wifi)

static void bridge_log(int level, const char *line) { xh_log(level == 0 ? XH_LOG_ERROR : level == 1 ? XH_LOG_INFO : XH_LOG_DEBUG, "%s", line); }
static void place(int tid, unsigned vcpu) { br_cpu_place(tid, &cpu_plan, vcpu); }
static int vcpu_tid(unsigned vcpu) { return vcpu < g_ncpu ? g_vcpu[vcpu].tid : 0; }
// The thermal bridge's step (br_thermal_cap_for): the vCPUs and the iOS threads they run keep to fewer, slower cores
// while the phone is hot. Affinities of xnuhost's own threads and its tasks' only: they end with the session, nothing
// outside the process is written, so a SIGKILL leaves nothing to restore.
static void thermal_step(void *ctx, const br_thermal_state *state)
{
  (void)ctx;
  if (!cpu_planned) return;
  br_cpu_plan_cap(&cpu_plan, &cpu_plan_base, &cpu_host, state->cap);
  __atomic_fetch_add(&xh_cpu_place_epoch, 1, __ATOMIC_ACQ_REL);
  for (unsigned i = 0; i < cpu_plan.nvcpu; i++) { int tid = vcpu_tid(i); if (tid > 0) br_cpu_place(tid, &cpu_plan, i); }
  uint64_t p = cpu_plan.nvcpu ? __atomic_load_n(&cpu_plan.v[cpu_plan.nvcpu - 1].affinity, __ATOMIC_ACQUIRE) : 0;
  LOGI("thermal: step %u (%s): P vCPUs on host CPUs 0x%llx%s", state->cap, br_thermal_level_name(state->level), (unsigned long long)p,
       state->cap ? "" : " (all of their kind)");
}
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

// ---- the image set (imageset.c): which disk is which volume, and the checks on what the guest reads ----
static xh_imageset image_set;
static bool have_image_set;
static xh_disk_identity disk_identity[XH_MAX_DISKS];
static char seed_sha256[65];

static int image_set_open(void)
{
  const char *error = NULL;
  int rc = xh_imageset_load(config.image, &image_set, &error);
  if (rc < 0) { LOGE("image set %s: %s", config.image, error); return -1; }
  if (rc > 0) { LOGI("image set: no manifest.json in %s: its disks are not verified and carry no identity", config.image); return 0; }
  have_image_set = true;
  const xh_imageset_file *seed = xh_imageset_role(&image_set, "data-seed");
  for (unsigned i = 0; seed && i < 32; i++) snprintf(seed_sha256 + i * 2, 3, "%02x", seed->sha256[i]);
  // The seed is unpacked by the kernel (0004: its node carries the hash); no helper disk.
  return 0;
}

// The identity of disk i in its device tree node; a read-only image of the set gets its chunk checks.
static int image_set_disk(unsigned i, xh_blk *disk)
{
  const char *error = NULL;
  const xh_disk_config *c = &config.disk[i];
  xh_disk_identity *id = &disk_identity[i];
  if (!have_image_set || id->role) return 0;
  if (c->data_store) {
    *id = (xh_disk_identity){ .role = "data", .volume_uuid = image_set.data_uuid, .volume_name = image_set.data_name, .sha256_hex = seed_sha256 };
    return 0;
  }
  const xh_imageset_file *f = xh_imageset_find(&image_set, c->path);
  if (!f || c->writable) return 0;
  if (!strcmp(f->role, "os") || !strcmp(f->role, "systemos") || !strcmp(f->role, "app")) {
    xh_verify *verify = xh_verify_open(&image_set, f, &error);
    if (!verify) { LOGE("disk %s: %s", c->path, error); return -1; }
    if (xh_blk_bytes(disk) != f->size) { LOGE("disk %s: %llu bytes, manifest.json lists %llu", c->path, (unsigned long long)xh_blk_bytes(disk), (unsigned long long)f->size); return -1; }
    xh_blk_set_verify(disk, verify);
    *id = (xh_disk_identity){ .role = f->role, .verified = xh_verify_digest(verify), .volume_uuid = f->volume_uuid };
  } else if (!strcmp(f->role, "data-seed")) {
    *id = (xh_disk_identity){ .role = "seed", .bytes = f->size, .sha256_hex = seed_sha256 };
  }
  return 0;
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

  // RAM: the kernel's window, then the GPU's pool (the memory of the renderer's that the guest maps: each 3D
  // context's response buffer), then the boot framebuffer the kernel's console draws into.
  uint64_t fb_bytes = ((uint64_t)fb_width * fb_height * 4 + 0x1fffff) & ~0x1fffffull;
  uint64_t pool_bytes = strcmp(config.gpu, "2d") ? GPU_POOL_BYTES : 0;
  uint64_t window = config.ram_bytes - fb_bytes - pool_bytes;
  if (xh_mem_map(kernel.base) || xh_mem_io(XL_BOARD_IO_BASE, XL_BOARD_IO_SIZE)) return -1;
  uint64_t phys = g_mem.phys;                       // physical = where it is mapped (mem.c)
  boot_fb_pa = phys + window + pool_bytes;
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
  if (image_set_open()) return -1;
  for (unsigned i = 0; i < config.ndisk; i++) {
    xh_blk *disk = xh_blk_open(&config.disk[i], &error);
    if (!disk) { LOGE("disk %s: %s (%s)", config.disk[i].path, error, strerror(errno)); return -1; }
    if (image_set_disk(i, disk)) return -1;
    if (!xh_virtio_blk_create(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, disk)) return -1;
    LOGI("disk %u: %s, %llu MiB, %s%s%s%s", ndev, config.disk[i].path, (unsigned long long)(xh_blk_bytes(disk) >> 20),
         config.disk[i].data_store ? "Data store" : config.disk[i].writable ? "writable" : "read-only",
         disk_identity[i].role ? ", role " : "", disk_identity[i].role ? disk_identity[i].role : "", disk_identity[i].verified ? ", chunks verified" : "");
    kinds[ndev++] = config.disk[i].data_store ? XL_DEV_DATA_STORE : XL_DEV_BLOCK;
  }
  xh_virtio *input = xh_virtio_input_create(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev);
  kinds[ndev++] = XL_DEV_INPUT;
  // The GPU: the 2D scanout always; with it the 3D half on the phone's Adreno (drm2kgsl on KGSL), when the host has it.
  xh_gpu3d *gpu3d = NULL;
  if (pool_bytes) {
    const char *why = "";
    gpu3d = xh_gpu3d_kgsl_open(g_mem.fd, phys, g_mem.size, xh_ram(phys + window, pool_bytes), phys + window, pool_bytes, gpu_log, &why);
    if (!gpu3d && !strcmp(config.gpu, "3d")) { LOGE("gpu: no 3D: %s", why); return -1; }
    if (!gpu3d) LOGI("gpu: 2D only: %s", why);
  }
  if (!input || !xh_virtio_gpu_create_3d(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, fb_width, fb_height, gpu3d)) return -1;
  kinds[ndev++] = XL_DEV_GPU;
  if (config.input && xh_input_start(input, fb_width, fb_height)) LOGE("input: could not start; continuing without");
  if (xh_input_script_start(input, config.input_script)) return -1;

  static xl_machine machine;
  machine.chip_id = 0x8150;
  if (gpu3d) { machine.gpu_pool_base = phys + window; machine.gpu_pool_size = pool_bytes; }
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
      cpu_host = host;
      cpu_plan_base = cpu_plan;
      cpu_planned = true;
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

    // Bluetooth: the phone's controller through its HAL (hidl), or none (virtual). auto: hidl when the HAL is
    // registered; the HAL itself is opened only when bluetoothd connects.
    bool hidl = !strcmp(config.bluetooth, "hidl");
    int hal = 0;
    if (!strcmp(config.bluetooth, "auto")) { hal = br_hci_hidl_probe(); hidl = hal > 0; }
    haptic = br_haptic_open(NULL);
    if (!config.touch_sleep) setenv("ONEIOS_TSP_OFF", "0", 1);   // touch-sleep=0: the bridge reads this
    brightness = br_bright_open(NULL);
    battery = br_battery_open(NULL);
    thermal = br_thermal_open(NULL);
    if (thermal) br_thermal_on_cap(thermal, thermal_step, NULL);
    br_bridge_config bridge = { .haptic = haptic, .hci_factory = hidl ? br_hci_open_hidl : NULL, .brightness = brightness, .battery = battery,
                                .thermal = thermal };
    bool have_bridge = br_bridge_create(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, &bridge) != NULL;
    if (have_bridge) kinds[ndev++] = XL_DEV_BRIDGE;
    if (hidl && have_bridge) {
      // The phone's controller, H4 over the bridge. bluetoothd's encodings (24A446, read from the binary): 1 H4,
      // 2 H4DS, 3 H4BC, 10 virtual. Plain H4 is the one UniXNU carries to this bluetoothd over the same Skywalk UART
      // pipe (drivers/bluetooth/oneios_bt_sk.c); the boot argument bt-transport-override=N tries another.
      machine.bt_transport = 1;
      machine.bt_speed = 3000000;
      bool address = br_bt_address("/mnt/vendor/efs/bluetooth/bt_addr", machine.bt_mac) == 0;
      if (!address) xl_machine_mac(&machine, 0, machine.bt_mac);
      LOGI("bluetooth: on: the phone's controller through its HAL%s, address %02x:%02x:%02x:%02x:%02x:%02x%s; the HAL is opened when bluetoothd connects",
           hal == 11 ? " 1.1" : hal == 10 ? " 1.0" : "", machine.bt_mac[0], machine.bt_mac[1], machine.bt_mac[2], machine.bt_mac[3],
           machine.bt_mac[4], machine.bt_mac[5], address ? "" : " (made up: /mnt/vendor/efs/bluetooth/bt_addr not read)");
    } else if (hidl) LOGI("bluetooth: off: no bridge device for the controller's packets");
    else LOGI("bluetooth: off (%s): iOS gets no controller%s", config.bluetooth,
              !strcmp(config.bluetooth, "virtual") ? "; bluetooth=hidl in xnuhost.conf (or --bluetooth hidl) for the phone's" : "");

    // The network: the phone's Wi-Fi. wlan0 is iOS's for the session (docs/hardware.md section 3): en0 has wlan0's
    // address, frames go between them as they are, scans and joins go to the vendor's Wi-Fi stack; no NAT.
    // wifi=tap: the old route (a tap and oneios-net.sh's NAT), for a bench.
    br_net_config net;
    br_net_config_defaults(&net);
    xl_machine_mac(&machine, 1, net.guest_mac);     // en0 (the tap's; the Wi-Fi's is wlan0's)
    if (!strcmp(config.wifi, "tap")) {
      if (br_virtio_net_create(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, &net, br_net_open_tap)) {
        kinds[ndev++] = XL_DEV_NET;
        net_up = net_script("up");                  // address, forwarding and NAT for the tap; undone at stop
      }
    } else if (!strcmp(config.wifi, "auto")) {
      br_net_open_fn wlan = br_wlan_prepare("wlan0", config.session, net.guest_mac);
      if (wlan && br_virtio_net_create_backend(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, &net, wlan, br_wlan_send)) {
        kinds[ndev++] = XL_DEV_NET;
        wifi_up = true;
      } else if (wlan) br_wlan_close();
    } else if (strcmp(config.wifi, "sim")) LOGI("wifi: off (wifi=off): iOS gets no network device");

    char helper[600];
    void *audio_ctx = NULL;
    const br_audio_backend *audio = NULL;
    if (*config.runtime && snprintf(helper, sizeof helper, "%s/bin/oneios-audio", config.runtime) < (int)sizeof helper) audio = br_audio_helper(helper, &audio_ctx);
    if (!audio) audio = br_audio_null(&audio_ctx);
    if (audio && br_virtio_snd_create(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, audio, audio_ctx)) kinds[ndev++] = XL_DEV_SND;

    // The camera: a helper process (Android's camera libraries are shared libraries) that opens a camera only
    // when the guest asks. ONEIOS_CAMERA=0 leaves it out; ONEIOS_CAMERA_BACKEND picks its back end (auto).
    const char *camera_switch = getenv("ONEIOS_CAMERA");
    if (ndev < XL_MAX_DEVICES && !(camera_switch && !strcmp(camera_switch, "0")) && *config.runtime &&
        snprintf(helper, sizeof helper, "%s/bin/oneios-camera", config.runtime) < (int)sizeof helper && access(helper, X_OK) == 0) {
      camera = br_camera_helper(helper, getenv("ONEIOS_CAMERA_BACKEND"));
      if (camera && br_camera_create(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, camera)) kinds[ndev++] = XL_DEV_CAMERA;
    }
  }

  // wifi=sim: a made-up radio (runtime/bridges/wifi/sim.c) for a boot with no phone, with the bridges on or off:
  // the guest's Wi-Fi driver and wifid get two networks to scan and join; no frame goes anywhere.
  if (!strcmp(config.wifi, "sim") && ndev < XL_MAX_DEVICES) {
    br_net_config net;
    br_net_config_defaults(&net);
    xl_machine_mac(&machine, 1, net.guest_mac);
    br_net_open_fn sim = br_wifi_sim_prepare(net.guest_mac);
    if (sim && br_virtio_net_create_backend(XL_BOARD_VIRTIO + ndev * XL_BOARD_VIRTIO_SIZE, XL_BOARD_VIRTIO_SPI + ndev, &net, sim, br_wifi_sim_send))
      kinds[ndev++] = XL_DEV_NET;
  }

  // clusters=EEPPPPPP: the cluster types by hand (a bench run with no host topology to plan from).
  for (unsigned i = 0; i < config.ncpu && config.clusters[i]; i++) machine.cpu_cluster[i] = config.clusters[i];

  // The device tree.
  xl_machine_board(&machine, config.ncpu, kinds, ndev);
  machine.timebase_hz = (uint32_t)g_info.timebase_hz;
  machine.dram_base = phys;
  machine.dram_size = window;
  machine.model = "iPhone18,2";
  machine.display_width = fb_width;
  machine.display_height = fb_height;
  machine.display_scale = config.scale;
  machine.display_hz = fb_hz;
  // Signed text the kernel changes at page-in (/chosen text-fixups): the configured ones, and what the UI scale needs.
  if (config.scale_fixup)
    config.text_fixups_length = xh_scale_fixups(have_image_set ? image_set.build : NULL, config.scale, config.text_fixups,
                                                config.text_fixups_length, sizeof config.text_fixups);
  if (config.text_fixups_length) {
    machine.text_fixups = config.text_fixups;
    machine.text_fixups_length = config.text_fixups_length;
    unsigned count = 0;
    for (unsigned i = 0; i < config.text_fixups_length; i++) count += !config.text_fixups[i];
    LOGI("text fixups: %u for /chosen text-fixups (UI scale @%ux)", count, config.scale);
  }
  machine.host_vector = (uint64_t)(uintptr_t)xh_vector();
  machine.user_va_limit = xh_tasks_va_limit();
  machine.host_page_size = g_info.host_page_size;
  if (getentropy(machine.random_seed, sizeof machine.random_seed)) { LOGE("no entropy for the kernel's seed"); return -1; }
  xl_blob ramdisk = sidecar("ramdisk", 1ull << 30), trust = sidecar("trustcache", 16 << 20), addon = sidecar("gpuaddon", 64 << 20),
          policies = sidecar("vroot", 128 << 20);
  // Apple's trust caches of the set (IM4P) and the owner module, as the sidecar the kernel takes, unless one is shipped.
  if (!trust.data && have_image_set && xh_imageset_find(&image_set, "trust/static.trustcache")) {
    trust = xh_imageset_trustcaches(&image_set, &error);
    if (!trust.data) { LOGE("trust caches of %s: %s", config.image, error); return -1; }
    LOGI("trust caches: %u modules from the image set, %zu bytes", (unsigned)trust.data[0], trust.size);
  }
  // The root: the OS volume, matched by its APFS volume UUID.
  const xh_imageset_file *os = have_image_set ? xh_imageset_role(&image_set, "os") : NULL;
  if (os && *os->volume_uuid) machine.root_volume_uuid = os->volume_uuid;
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

  // Each disk's identity for the kernel's storage (imageset.h).
  for (unsigned i = 0, blocks = 0; have_image_set && i < config.ndisk; i++) {
    char name[40];
    if (config.disk[i].data_store) snprintf(name, sizeof name, "arm-io/data-store");
    else snprintf(name, sizeof name, "arm-io/virtio-mmio-%u", blocks++);
    xh_imageset_describe(xl_adt_find(tree, name), &disk_identity[i]);
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
  xh_setway_skip = config.setway_skip != 0;
  LOGI("cpu: a trapped dc set/way %s (setway-skip=%d)", xh_setway_skip ? "ends the loop of its level at once" : "is one trap per line", xh_setway_skip ? 1 : 0);
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

// A guest restart (PSCI SYSTEM_RESET, XH_HALT_RESTART: iOS restarting itself, or the kernel starting the
// Data volume over for a new seed) is not the end of the session: for the OneOS fork xnuhost exiting means
// "back to One UI". xnuhost starts itself again in place, same pid, cgroup and namespace: exec ends every
// vCPU thread, the tasks went with xh_tasks_shutdown, and every descriptor but the log is closed (the RAM
// memfd would otherwise stay alive). XNUHOST_RESTARTS counts them: a guest that restarts 8 times in a row
// without ever getting ready is left, with exit 3.
static char **start_argv;
static void guest_restart(void)
{
  const char *counted = getenv("XNUHOST_RESTARTS");
  int n = counted ? atoi(counted) : 0;
  if (guest_ready) n = 0;
  if (n >= 8) { LOGE("the guest restarted %d times without getting ready: not starting it again", n); return; }
  char text[16];
  snprintf(text, sizeof text, "%d", n + 1);
  setenv("XNUHOST_RESTARTS", text, 1);
  LOGI("the guest asked for a restart: starting again (%d)", n + 1);
#ifdef SYS_close_range
  if (syscall(SYS_close_range, 3u, ~0u, 0u))
#endif
    for (int fd = 3; fd < 4096; fd++) close(fd);
  // XNUHOST_RUNNER: the emulator xnuhost itself runs under ("qemu-aarch64 -cpu cortex-a710"), which a PC with
  // no binfmt entry needs in front of the program; unset on the phone.
  const char *runner = getenv("XNUHOST_RUNNER");
  if (runner && *runner) {
    static char words[256];
    static char *args[64];
    int n = 0, argc = 0;
    snprintf(words, sizeof words, "%s", runner);
    for (char *w = strtok(words, " "); w && n < 8; w = strtok(NULL, " ")) args[n++] = w;
    while (start_argv[argc]) argc++;
    if (n && n + argc < 63) {
      for (int i = 0; i < argc; i++) args[n + i] = start_argv[i];
      execvp(args[0], args);
    }
  }
  execv("/proc/self/exe", start_argv);
  LOGE("restart: exec /proc/self/exe: %s", strerror(errno));
}

int main(int argc, char **argv)
{
  start_argv = argv;
#if defined(__ANDROID__) && defined(__aarch64__)
  // Before any thread exists: bionic gives each new thread of a program with target SDK >= 31 its own
  // APIA key. The kernel's vCPUs and the tasks need one key set (pac_shared_by_threads).
  android_set_application_target_sdk_version(30);
#endif
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
      LOGI("selftest: task factory: started; system-call trapping (seccomp): %s", xh_tasks_seccomp() ? "yes" : "NO");
      ok = ok && xh_tasks_seccomp();
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
  sigaddset(&stops, SIGTERM); sigaddset(&stops, SIGINT); sigaddset(&stops, SIGHUP); sigaddset(&stops, SIGQUIT);
  for (int s = SIGRTMIN; s <= SIGRTMIN + 3; s++) sigaddset(&stops, s);
  pthread_sigmask(SIG_BLOCK, &stops, NULL);
  signal(SIGPIPE, SIG_IGN);

  if (*config.session) {
    char pid[32];
    snprintf(pid, sizeof pid, "%d\n", getpid());
    session_file("xnuhost.pid", pid);
  }
  if (xh_display_init(&config, &fb_width, &fb_height, &fb_hz)) return EXIT_SETUP;
  // The display driver's scale and refresh rate go on the kernel's command line (a value given there stays).
  if (xh_display_boot_args(config.boot_args, sizeof config.boot_args, config.scale, fb_hz)) { LOGE("no room in the boot arguments for the display's switches"); return EXIT_SETUP; }
  if (boot()) { xh_tasks_shutdown(); return EXIT_SETUP; }

  sigset_t waited;
  sigemptyset(&waited);
  sigaddset(&waited, SIGTERM); sigaddset(&waited, SIGINT); sigaddset(&waited, SIGHUP); sigaddset(&waited, SIGQUIT);
  while (!xh_stopping()) {
    struct timespec tick = { 0, 100 * 1000 * 1000 };
    int signo = sigtimedwait(&waited, NULL, &tick);
    if (signo == SIGQUIT) { xh_cpu_dump_all(); xh_tasks_dump(); xh_sysreg_stats_log(); continue; }      // debugging: where each vCPU is
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
    LOGI("cpu%u: %s, %llu interrupts (%llu timer, %llu IPI acknowledged, %llu re-raised), %llu device accesses, %llu emulated instructions, %llu task traps", i,
         g_vcpu[i].started ? "started" : "never started",
         (unsigned long long)g_vcpu[i].stat_irqs, (unsigned long long)g_vcpu[i].stat_timer, (unsigned long long)g_vcpu[i].stat_sgi,
         (unsigned long long)g_vcpu[i].stat_reraised, (unsigned long long)g_vcpu[i].stat_mmio, (unsigned long long)g_vcpu[i].stat_sysreg,
         (unsigned long long)g_vcpu[i].stat_traps);
  xh_sysreg_stats_log();                    // which trapped instructions kept the vCPUs busy
  // Debugging aid: XNUHOST_DUMP=FILE writes the first 64 MiB of the kernel's window at the stop (XNUHOST_DUMP_MB=N
  // for more: a kernel collection's data is past 64 MiB).
  if (getenv("XNUHOST_DUMP")) {
    FILE *dump = fopen(getenv("XNUHOST_DUMP"), "wb");
    uint64_t limit = (uint64_t)(getenv("XNUHOST_DUMP_MB") ? strtoul(getenv("XNUHOST_DUMP_MB"), NULL, 0) : 64) << 20;
    if (dump) { fwrite(g_mem.host, 1, g_mem.size < limit ? g_mem.size : limit, dump); fclose(dump); }
  }
  xh_tasks_shutdown();
  if (haptic) br_haptic_close(haptic);      // stops the motor
  if (camera) br_camera_close(camera);      // the helper closes the camera and exits
  if (brightness) br_bright_close(brightness);      // One UI's backlight level again, the light sensor as it was
  if (battery) br_battery_close(battery);           // read only: the worker stops
  if (thermal) br_thermal_close(thermal);           // read only; the step was affinities of our own threads
  if (wifi_up) br_wlan_close();                     // the network left, the supplicant and wlan0 as One UI left them
  if (net_up) net_script("down");
  xh_input_script_stop();
  xh_input_stop();
  xh_display_close();
  sync();                                   // the Data store's pages
  if (stop_state.reason == XH_HALT_RETURN) session_file("return.request", "the guest asked to return\n");
  if (stop_state.reason == XH_HALT_RESTART) guest_restart();     // returns only if it could not
  int code = stop_state.reason == XH_HALT_RESTART ? EXIT_RESTART : stop_state.reason == XH_HALT_PANIC ? EXIT_PANIC :
             stop_state.code ? EXIT_PANIC : EXIT_GUEST_OFF;
  LOGI("exit %d", code);
  _exit(code);
}
