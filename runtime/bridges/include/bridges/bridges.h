// SPDX-License-Identifier: MIT
// Host bridges of OneiOS: the Linux/Android side of each device the guest's drivers (drivers/) use.
// Everything here is plain C with no dependency on xnuhost's internals; the virtio device models
// that sit on xnuhost's transport are in bridges/devices.h. docs/hardware.md explains each device.
#ifndef ONEIOS_BRIDGES_H
#define ONEIOS_BRIDGES_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

// Log sink (default: stderr). xnuhost points it at its own log.
extern void (*br_log_sink)(int level, const char *line);     // level 0 error, 1 info, 2 debug
void br_log(int level, const char *fmt, ...) __attribute__((format(printf, 2, 3)));

// ---- CPU: host topology, the vCPU plan, thread placement ----------------------------------------
#define BR_MAX_CPUS 16
#define BR_MAX_VCPUS 8
#define BR_MAX_FREQS 40

typedef struct {
  bool     present;
  unsigned capacity;                      // cpu_capacity (0..1024); 0 = not reported
  unsigned policy;                        // cpufreq policy id (first CPU of the frequency domain)
  uint32_t min_khz, max_khz;              // cpuinfo_min_freq, cpuinfo_max_freq
  uint32_t freq_khz[BR_MAX_FREQS];        // scaling_available_frequencies, ascending
  unsigned nfreq;
  unsigned part;                          // MIDR part number (0xd80 A520, 0xd81 A720, 0xd82 X4), 0 = unknown
  uint32_t l1d_bytes, l1i_bytes, l2_bytes, l3_bytes;
  char     type;                          // 'P' or 'E', set by br_cpu_read
} br_host_cpu;

typedef struct {
  unsigned    ncpu;                       // highest present index + 1
  br_host_cpu cpu[BR_MAX_CPUS];
  unsigned    n_p, n_e;
} br_cpu_topology;

// Reads <root>/devices/system/cpu (root = "/sys" on the phone; tests pass a fixture). Returns 0, or
// -1 when no CPU was found. CPUs of the lowest capacity are E when capacities differ, else all P.
int br_cpu_read(const char *sysfs_root, br_cpu_topology *out);

typedef struct {
  char     type;                          // 'P' or 'E': the ADT cluster-type of this vCPU
  unsigned cluster_id;                    // 0 = E cluster, 1 = P cluster (E first, as on Apple SoCs)
  uint64_t affinity;                      // host CPUs its thread (and the iOS thread it runs) may use
  unsigned capacity;                      // of the fastest host CPU in the mask
  uint32_t min_khz, max_khz;
  uint32_t freq_khz[BR_MAX_FREQS];
  unsigned nfreq;
  uint32_t l1d_bytes, l1i_bytes, l2_bytes, l3_bytes;
} br_vcpu;

typedef struct {
  unsigned nvcpu, n_p, n_e;
  br_vcpu  v[BR_MAX_VCPUS];
} br_cpu_plan;

// vCPUs for the guest: E vCPUs first (the boot CPU is an E core on iPhones), then P. With pin = true
// and nvcpu == host CPUs each vCPU gets one host CPU; otherwise a vCPU may run on any core of its
// kind and Linux places it. Returns 0, -1 on bad arguments.
int br_cpu_plan_make(const br_cpu_topology *t, unsigned nvcpu, bool pin, br_cpu_plan *out);
// Sets the affinity of Linux thread `tid` (0 = caller) to vCPU `index`'s mask. Returns 0 or -errno.
int br_cpu_place(int tid, const br_cpu_plan *plan, unsigned index);
// Utilisation clamp of `tid`: asks Linux's governor for at least `khz` on the cores of vCPU `index`
// (0 drops the request). Returns 0 or -errno. Linux keeps clock control: this is a hint.
int br_cpu_request(int tid, const br_cpu_plan *plan, unsigned index, uint32_t khz);
// The uclamp.min value br_cpu_request uses (0..1024).
unsigned br_cpu_util_for(const br_cpu_plan *plan, unsigned index, uint32_t khz);

// Thermal steps (runtime/bridges/thermal): the host CPUs a vCPU of `type` may use at step `cap`. 0: all of its kind;
// 1: P without the frequency domain of the highest top clock (the X4 here); 2: P limited to the domain of the lowest
// top clock, and the E cores; 3: the E cores only. A step that would leave no CPU keeps the one before it.
uint64_t br_cpu_thermal_mask(const br_cpu_topology *t, char type, unsigned cap);
// plan's affinities from base's (br_cpu_plan_make), each limited to its step's CPUs (base's mask when they do not meet),
// with the step's CPUs of the other kind added (step 2: a P vCPU also on the E cores). Each word is stored atomically:
// placement may read plan meanwhile.
void br_cpu_plan_cap(br_cpu_plan *plan, const br_cpu_plan *base, const br_cpu_topology *t, unsigned cap);
// cpu-pin: vCPU `vcpu`'s one host CPU, the rank-th CPU of plan's mask among base's vCPUs of its kind, while every vCPU
// of that kind can have a CPU of the mask to itself; otherwise (more vCPUs than CPUs, as at a thermal step) the whole
// mask, so that Linux spreads them. 0 for a vCPU the plan does not have.
uint64_t br_cpu_pin_mask(const br_cpu_plan *plan, const br_cpu_plan *base, unsigned vcpu);

// The virtual cpufreq device DroidVMCpufreq.c drives (Linux's virtual-cpufreq register layout): one
// 4 KiB window per vCPU. xnuhost registers [base, base + nvcpu * 0x1000) and forwards accesses here.
#define BR_CPUFREQ_WINDOW 0x1000u
typedef struct br_cpufreq br_cpufreq;
// tid_of(vcpu) gives the Linux thread to clamp when the guest writes (NULL: the calling thread).
br_cpufreq *br_cpufreq_create(const br_cpu_plan *plan, int (*tid_of)(unsigned vcpu));
uint64_t    br_cpufreq_read(br_cpufreq *d, uint64_t offset, unsigned size);
void        br_cpufreq_write(br_cpufreq *d, uint64_t offset, unsigned size, uint64_t value);
uint32_t    br_cpufreq_requested(const br_cpufreq *d, unsigned vcpu);    // last kHz asked, 0 = none
void        br_cpufreq_destroy(br_cpufreq *d);

// ---- haptics ------------------------------------------------------------------------------------
// The S24 Ultra's actuator: an LRA on a Cirrus CS40L26, exposed by Samsung's sec_vibrator_inputff as
// an evdev force-feedback device (FF_PERIODIC with FF_SINE or FF_CUSTOM, FF_GAIN; no FF_RUMBLE).
typedef struct {
  // Uploads (or updates) the one effect: a sine of `period_ms` and `magnitude` (0..255) for
  // `length_ms`. Returns the effect id (>= 0) or -errno.
  int  (*upload)(void *ctx, int id, unsigned length_ms, unsigned period_ms, unsigned magnitude);
  int  (*play)(void *ctx, int id, bool on);
  int  (*gain)(void *ctx, unsigned gain);            // 0..10000 (Samsung's scale)
  void (*close)(void *ctx);
} br_haptic_backend;

typedef struct br_haptic br_haptic;
// Finds the force-feedback device under `dev_dir` ("/dev/input"; NULL = that) and starts the worker.
// NULL when there is none or it cannot be opened (the reason is logged).
br_haptic *br_haptic_open(const char *dev_dir);
// The same on a caller's back end (tests, another actuator).
br_haptic *br_haptic_open_backend(const br_haptic_backend *backend, void *ctx);
// Vibrate from now for on_ms (0 = stop) at intensity 0..1000 and frequency_hz (0 = resonance).
// Never blocks: the newest request replaces one that has not started.
void br_haptic_vib(br_haptic *h, uint32_t on_ms, unsigned intensity, unsigned frequency_hz);
void br_haptic_stop(br_haptic *h);
// Waits until the worker has carried out everything asked so far (tests, shutdown).
void br_haptic_flush(br_haptic *h);
void br_haptic_close(br_haptic *h);
// The mapping, exposed for tests: iOS intensity/frequency to the effect's parameters.
void br_haptic_map(unsigned intensity, unsigned frequency_hz, unsigned *period_ms, unsigned *magnitude);
// The evdev back end's device search: writes the node path of the first device whose EV_FF bits
// have FF_PERIODIC. Returns 0 or -1.
int  br_haptic_find(const char *dev_dir, char *path, size_t size);

// ---- network ------------------------------------------------------------------------------------
// A frame sink/source for the guest's en0 (virtio-net). The tap back end needs root.
typedef struct br_net br_net;
typedef void (*br_net_rx_fn)(void *ctx, const uint8_t *frame, size_t length);
typedef struct {
  char     ifname[16];                    // tap name, default "oneios0"
  uint8_t  guest_mac[6];                  // en0's address
  uint32_t host_ip, guest_ip, netmask;    // host byte order; defaults 192.168.77.1 / .2 / 255.255.255.0
  uint32_t dns[2];                        // handed to iOS by the built-in DHCP answer
  uint32_t lease_s;
  bool     dhcp;                          // answer DHCP in the bridge (default true)
} br_net_config;
void    br_net_config_defaults(br_net_config *c);
// Opens the tap (does not configure addresses or routes: net/oneios-net.sh does) and starts the
// receive thread, which calls rx for each frame for the guest.
br_net *br_net_open_tap(const br_net_config *c, br_net_rx_fn rx, void *ctx);
// A back end over a descriptor that carries one frame per read/write (tests: a socketpair).
br_net *br_net_open_fd(const br_net_config *c, int fd, br_net_rx_fn rx, void *ctx);
// A frame from the guest. DHCP requests are answered here when c->dhcp. Returns 0 or -errno.
int     br_net_send(br_net *n, const uint8_t *frame, size_t length);
void    br_net_close(br_net *n);
// Builds the DHCP reply to `frame` if it is a DHCPDISCOVER/REQUEST from the guest. Returns the
// reply's length (written to out, at most out_size) or 0 when the frame is not one.
size_t  br_dhcp_reply(const br_net_config *c, const uint8_t *frame, size_t length, uint8_t *out, size_t out_size);

// ---- Bluetooth ----------------------------------------------------------------------------------
// One HCI controller. Packet types are the H4 indicators (1 command, 2 ACL, 3 SCO, 4 event, 5 ISO).
typedef void (*br_hci_rx_fn)(void *ctx, uint8_t type, const uint8_t *packet, size_t length);
typedef void (*br_hci_state_fn)(void *ctx, int state);       // OB_HCI_STATE_*
typedef struct br_hci br_hci;
typedef struct {
  int  (*open)(void *ctx);                 // start initialisation; the state callback reports the result
  int  (*send)(void *ctx, uint8_t type, const uint8_t *packet, size_t length);
  void (*close)(void *ctx);
} br_hci_backend;
br_hci *br_hci_create(const br_hci_backend *backend, void *backend_ctx, br_hci_rx_fn rx, br_hci_state_fn state, void *ctx);
int     br_hci_open(br_hci *h);
int     br_hci_send(br_hci *h, uint8_t type, const uint8_t *packet, size_t length);
void    br_hci_close(br_hci *h);
void    br_hci_destroy(br_hci *h);
// For back ends: deliver a packet or a state change to the owner.
void    br_hci_deliver(br_hci *h, uint8_t type, const uint8_t *packet, size_t length);
void    br_hci_set_state(br_hci *h, int state);
int     br_hci_state(br_hci *h);
// Checks one HCI packet's own length field against `length`. Returns true when consistent.
bool    br_hci_packet_ok(uint8_t type, const uint8_t *packet, size_t length);
// The phone's HAL: android.hardware.bluetooth@1.0/1.1 IBluetoothHci/default over /dev/hwbinder
// (bt/hci_hidl.c; the service on the S24 Ultra is vendor.bluetooth-1-1-qti). Needs root.
// Two threads: one answers the HAL's callbacks, one makes the calls (br_hci_send only queues, up to
// 512 packets). ONEIOS_HWBINDER names another binder device than /dev/hwbinder (the PC test).
br_hci *br_hci_open_hidl(br_hci_rx_fn rx, br_hci_state_fn state, void *ctx);
// Asks hwservicemanager for the HAL once and lets it go: 11 (1.1) or 10 (1.0) when it is registered and
// this domain may find it, else -errno (-ENODEV: no binder device). Logs why when it is not found.
int     br_hci_hidl_probe(void);
// The controller's address as the phone stores it (text "AA:BB:.." or 6 raw bytes). 0 or -1.
int     br_bt_address(const char *path, uint8_t out[6]);

// ---- audio --------------------------------------------------------------------------------------
// PCM sink and source for the guest's virtio-snd card: 48 kHz S16LE, stereo out, mono in.
typedef struct {
  int  (*start)(void *ctx, bool output, unsigned rate, unsigned channels);
  int  (*stop)(void *ctx, bool output);
  // Output: queue `frames`; returns frames taken. Input: fill up to `frames`; returns frames given.
  int  (*write)(void *ctx, const int16_t *pcm, unsigned frames);
  int  (*read)(void *ctx, int16_t *pcm, unsigned frames);
  void (*close)(void *ctx);
} br_audio_backend;
// The AAudio helper process (audio/aaudio_helper.c, bin/oneios-audio): started with `path`, fed over
// a socket pair. NULL when it cannot be started. *ctx receives the back end's context.
const br_audio_backend *br_audio_helper(const char *path, void **ctx);
// A back end that drops output and gives silence (no helper, tests).
const br_audio_backend *br_audio_null(void **ctx);

#endif
