// SPDX-License-Identifier: MIT
// xnuhost internals.
#ifndef XNUHOST_INTERNAL_H
#define XNUHOST_INTERNAL_H

#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <pthread.h>
#include "xnuhost/hostcall.h"

#define XH_MAX_CPUS 8

// ---- log (log.c) --------------------------------------------------------------------------------
enum { XH_LOG_ERROR, XH_LOG_INFO, XH_LOG_DEBUG };
void xh_log_open(const char *path, int level);
int  xh_log_level(void);
void xh_log(int level, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
void xh_log_raw(const char *text, size_t length);       // guest console bytes, line-buffered
// Async-signal-safe: fixed text and hex numbers straight to the log descriptor.
void xh_log_signal(const char *text, uint64_t a, uint64_t b);
void xh_cpu_dump_all(void);
#define LOGE(...) xh_log(XH_LOG_ERROR, __VA_ARGS__)
#define LOGI(...) xh_log(XH_LOG_INFO, __VA_ARGS__)
#define LOGD(...) xh_log(XH_LOG_DEBUG, __VA_ARGS__)

// ---- configuration (config.c) -------------------------------------------------------------------
#define XH_MAX_DISKS 6
typedef struct {
  char path[512];
  bool writable;
  bool data_store;          // the "droidvm,data-store" window
  char seed[512];           // read-only image under a writable store ("" = none)
} xh_disk_config;

typedef struct {
  int   session_abi;
  char  mode[16];           // "takeover" (the OneOS session) or "bench" (a shell, no session files)
  char  runtime[512], image[512], data[512], session[512];
  char  kernel[512];        // default <image>/kernel.kc
  char  identity[512];      // decoded device tree of the device, default <image>/identity.adt if present
  char  boot_args[1024];
  unsigned ncpu;
  uint64_t ram_bytes;
  xh_disk_config disk[XH_MAX_DISKS];
  unsigned ndisk;
  char  display[16];        // "shm", "drm", "none"
  char  gpu[8];             // "auto" (3D when the host has KGSL, udmabuf and a dma heap), "3d" (or do not start), "2d"
  unsigned width, height;   // guest framebuffer; 0 = the panel's native (largest) mode, 1440x3120 where it cannot be read
  unsigned hz;              // the guest display's refresh rate; 0 = the panel's highest at that mode, 120 where unknown
  unsigned scale;           // UI scale, pixels per point: 3 (480x1040 points, the default) or 2 (720x1560); 0 = the image set's
  int   scale_fixup;        // the QuartzCore text fixups a scale of 3 needs (xh_scale_fixups): 1, 0, -1 = unset (on)
  char  text_fixups[1024];  // text-fixup=FILE,OFFSET,ORIGINAL,REPLACEMENT entries for /chosen text-fixups, NUL-separated
  unsigned text_fixups_length;
  bool  input;              // read /dev/input
  char  input_script[512];  // a file of scripted touches and keys (input_script.c), "" = none
  bool  tasks;              // start the task factory
  int   task_uid;           // uid of iOS task processes, -1 = xnuhost's own
  int   log_level;
  bool  legacy_boot_args;
  int   setway_skip;        // a trapped dc set/way ends the guest's set/way loop of its level at once (sysreg.c): 1 (default), 0, -1 = unset
  bool  selftest;
  int   bridges;            // runtime/bridges devices (network, sound, haptics, Bluetooth, CPU placement): 1, 0, -1 = by mode
  char  clusters[XH_MAX_CPUS + 1]; // "EEPPPPPP": the cluster type of each vCPU, overriding the bridges' plan ("" = the plan, else all P)
  char  bluetooth[16];      // "auto" (hidl when the bridges are on and the phone's HAL is registered, else virtual),
                            // "virtual" (no controller, transport 10) or "hidl" (the phone's controller, transport 3)
  char  wifi[8];            // "auto" (the phone's wlan0 for iOS's en0 when the bridges are on), "off" (no network
                            // device), "tap" (the old route: a tap and bin/oneios-net.sh's NAT)
} xh_config;

void xh_config_defaults(xh_config *c);
// Applies "key=value" lines of a file (missing file is fine). Returns -1 on a bad line.
int  xh_config_file(xh_config *c, const char *path);
int  xh_config_set(xh_config *c, const char *key, const char *value);
// Parses argv (and the ONEIOS_* environment). Returns 0, or -1 after printing what is wrong.
int  xh_config_args(xh_config *c, int argc, char **argv);
// Fills what depends on other values (kernel path, the image set's disks). Returns -1 on a problem.
int  xh_config_finish(xh_config *c);
uint64_t xh_parse_size(const char *text, bool *ok);

// The display's mode plumbing (config.c; docs/booter.md 2.8).
#define XH_PANEL_WIDTH  1440u          // the Galaxy S24 Ultra's panel, where the host's cannot be read
#define XH_PANEL_HEIGHT 3120u
#define XH_PANEL_HZ     120u
typedef struct { unsigned width, height, hz; } xh_panel_mode;
// The panel's native mode among the modes of its connector: the largest, at its highest refresh rate. Not the
// current one: One UI's resolution setting may run the panel at 1080x2340.
bool xh_panel_native(const xh_panel_mode *modes, unsigned count, xh_panel_mode *native);
// Whether a DRM connector of this type (DRM_MODE_CONNECTOR_*) can be the built-in panel.
bool xh_panel_connector(unsigned type);
// The display helper's report on display.sock: { "XHDP", width, height, hz, 0 }. false: not a usable one.
bool xh_panel_report(const void *message, size_t length, xh_panel_mode *panel);
// The guest's mode: width, height and hz where configured (command line, then the image set's xnuhost.conf), else
// the mode of the panel (NULL: not known), else 1440x3120 at 120 Hz.
xh_panel_mode xh_display_mode(const xh_config *c, const xh_panel_mode *panel);
// Adds the display driver's two switches to the kernel's command line unless it already has them:
// offline-legacy-unit-scale=SCALE (/chosen display-scale) and sheng-vsync=HZ (the vsync tick and, above 60 Hz,
// the refresh properties). -1: no room.
int  xh_display_boot_args(char *args, size_t size, unsigned scale, unsigned hz);
// Appends the text fixups a UI scale needs on this build's dyld shared cache to a NUL-separated list
// (/chosen text-fixups). Returns the list's new length; unknown builds and a scale of 2 add nothing.
unsigned xh_scale_fixups(const char *build, unsigned scale, char *list, unsigned length, unsigned size);

// ---- registers of a trapped kernel context (arch) -----------------------------------------------
typedef struct { uint64_t x[31], sp, pc, pstate; } xh_regs;
static inline uint64_t xh_reg_get(const xh_regs *r, unsigned n) { return n == 31 ? 0 : r->x[n]; }
static inline void xh_reg_set(xh_regs *r, unsigned n, uint64_t v) { if (n != 31) r->x[n] = v; }

// ---- instruction decoding (insn.c) --------------------------------------------------------------
typedef struct {
  bool     load;            // load (to rt) or store (from rt)
  unsigned size;            // access bytes: 1, 2, 4, 8
  unsigned rt;              // 31 = zero register
  bool     sign_extend;     // loads
  bool     to64;            // sign-extend to 64 bits (else 32)
  bool     writeback;       // pre/post index: base updated by `offset`
  unsigned rn;              // base register (31 = sp)
  int64_t  offset;          // writeback amount
  bool     pair;            // ldp/stp: second register rt2 at address + size
  unsigned rt2;
} xh_mem_insn;
// Decodes an AArch64 integer load/store. Returns false for anything else (SIMD, exclusive, ...).
bool xh_decode_mem(uint32_t insn, xh_mem_insn *out);

typedef struct {
  bool     read;            // mrs (to rt) or msr (from rt)
  unsigned op0, op1, crn, crm, op2, rt;
} xh_sys_insn;
#define XH_SYSREG(op0, op1, crn, crm, op2) ((op0) << 14 | (op1) << 11 | (crn) << 7 | (crm) << 3 | (op2))
static inline unsigned xh_sys_key(const xh_sys_insn *s) { return XH_SYSREG(s->op0, s->op1, s->crn, s->crm, s->op2); }
// mrs/msr with a register operand. Returns false otherwise.
bool xh_decode_sys(uint32_t insn, xh_sys_insn *out);
// TPIDRRO_EL0 reads rewritten to TPIDR_EL0 reads in [text, text+size). Returns the count.
uint32_t xh_text_fixup(uint32_t *text, uint64_t size);

// ---- guest RAM and the I/O aperture (mem.c) -----------------------------------------------------
typedef struct {
  int      fd;              // memfd of the RAM
  uint8_t *host;            // where the window is mapped in xnuhost (= what the kernel sees)
  uint64_t phys, size;
  uint8_t *io_host;         // PROT_NONE reservation for the board's devices
  uint64_t io_phys, io_size;
  uint64_t kva, kva_size;   // reservation for the kernel's own mappings
} xh_mem;
extern xh_mem g_mem;
// Creates the RAM memfd (before the task factory forks).
int  xh_mem_create(uint64_t size);
// Maps the window at an address congruent to `low21` modulo 2 MiB; that address is also its
// physical address (g_mem.phys == g_mem.host).
int  xh_mem_map(uint64_t low21);
// Reserves the PROT_NONE aperture the board's device windows live in.
int  xh_mem_io(uint64_t phys, uint64_t size);
int  xh_mem_kva(void);
int64_t xh_mem_kernel_map(uint64_t va, uint64_t size, uint32_t prot, uint64_t phys);
int64_t xh_mem_kernel_unmap(uint64_t va, uint64_t size);
void xh_mem_map_limit(void);                       // raises vm.max_map_count when it can
// Host pointer for guest physical [pa, pa+len), NULL if not inside RAM.
void *xh_ram(uint64_t pa, uint64_t len);

// ---- MMIO dispatch (mmio.c) ---------------------------------------------------------------------
typedef struct xh_mmio_region {
  uint64_t base, size;
  const char *name;
  void *opaque;
  uint64_t (*read)(void *opaque, uint64_t offset, unsigned size);
  void (*write)(void *opaque, uint64_t offset, unsigned size, uint64_t value);
} xh_mmio_region;
int      xh_mmio_add(const xh_mmio_region *region);
uint64_t xh_mmio_read(uint64_t pa, unsigned size);
void     xh_mmio_write(uint64_t pa, unsigned size, uint64_t value);
bool     xh_mmio_covers(uint64_t pa);
// Emulates the load/store `insn` at regs->pc against physical address pa; advances pc.
bool     xh_mmio_emulate(xh_regs *regs, uint32_t insn, uint64_t pa);

// ---- interrupt controller: a GICv3 as ArmCpu drives it (gic.c) ----------------------------------
// INTIDs 32..1019. The board's devices use the low ones (32 + n); ArmCpu's software IPIs
// (boot-arg armcpu-spi-ipi=1) are SPIs 1008 + core, pended by a GICD_ISPENDR write.
#define XH_GIC_SPIS 992
#define XH_GIC_IPI_SPI 1008
void     xh_gic_init(unsigned ncpu, uint64_t dist_base, uint64_t redist_base);
void     xh_gic_set_spi(unsigned intid, bool level);          // device line
void     xh_gic_set_ppi(unsigned cpu, unsigned intid, bool level);
void     xh_gic_send_sgi(unsigned cpu, unsigned sgi);
uint32_t xh_gic_ack(unsigned cpu);
void     xh_gic_eoi(unsigned cpu, uint32_t intid);
void     xh_gic_enable(unsigned cpu, uint32_t intid, bool enable);
bool     xh_gic_pending(unsigned cpu);
// ICC_* system registers. Returns false if the register is not the GIC's.
bool     xh_gic_sysreg(unsigned cpu, const xh_sys_insn *insn, uint64_t *value);
// Set by the machine layer: called with the state of a CPU's interrupt line whenever it may have changed.
extern void (*xh_gic_notify)(unsigned cpu, bool line);
void     xh_gic_map(void);                                    // registers the MMIO frames

// ---- system-register and PSCI emulation (sysreg.c) ----------------------------------------------
typedef struct {
  uint64_t cntv_ctl, cntv_cval;
  uint64_t daif;
  uint64_t shadow[64];      // other EL1 registers the kernel writes and reads back
  unsigned shadow_key[64];
  unsigned nshadow;
} xh_sysreg_state;
typedef struct xh_vcpu xh_vcpu;
// Emulates the undefined instruction `insn` trapped in kernel context. Returns false if it is not
// one the host emulates. Advances regs->pc itself.
bool xh_sysreg_emulate(xh_vcpu *vcpu, xh_regs *regs, uint32_t insn);

// ---- vCPUs (cpu.c, arch) ------------------------------------------------------------------------
struct xh_vcpu {
  struct xh_cpu cpu;        // first: the ABI block
  pthread_t thread;
  int       tid;
  bool      started;
  uint64_t  entry, arg0, arg1, arg2;
  uint8_t  *boot_stack, *host_stack, *sig_stack;
  void     *timer;          // timer_t
  bool      timer_armed;
  uint64_t  timer_deadline;
  uint32_t  idle;           // 1 while in cpu_idle
  xh_sysreg_state sys;
  uint64_t  stat_irqs, stat_mmio, stat_sysreg, stat_traps;
  uint64_t  stat_acks, acks_mark;     // interrupts acknowledged; the count when the last interrupt upcall began
  uint64_t  stat_reraised, stat_timer, stat_sgi;   // frame_resume re-raises; INTID 27 and SGIs acknowledged
};
extern xh_vcpu g_vcpu[XH_MAX_CPUS];
extern unsigned g_ncpu;
extern struct xh_upcalls g_upcalls;
extern bool g_upcalls_set;

int      xh_cpu_init(unsigned ncpu);
// Starts vCPU `index` at `entry` (x0..x2 = args). entry 0: upcalls->cpu_entry(cpu, a0).
int      xh_cpu_start(unsigned index, uint64_t entry, uint64_t a0, uint64_t a1, uint64_t a2);
void     xh_cpu_line(unsigned index, bool line);     // the interrupt line of a CPU (the GIC's notify hook)
void     xh_cpu_timer_set(xh_vcpu *v, uint64_t deadline);
void     xh_cpu_idle(xh_vcpu *v);
extern bool xh_setway_skip;         // sysreg.c: config.setway_skip (the idle set/way flush, docs/booter.md 2.5)
bool     xh_cpu_timer_due(unsigned index);            // the level of a CPU's timer line (INTID 27)
void     xh_cpu_frame_resume(xh_vcpu *v, const struct xh_frame *frame);
uint64_t xh_counter(void);                            // CNTVCT_EL0 (host builds: a monotonic clock)
uint64_t xh_counter_hz(void);
xh_vcpu *xh_cpu_self(void);                           // the calling vCPU, NULL on other threads
// Set before the vCPUs start: puts a Linux thread (0 = the caller) on the host CPUs of a vCPU. Called
// by each vCPU thread for itself and for the iOS thread a vCPU runs, when that vCPU changes.
extern void (*xh_cpu_place)(int tid, unsigned vcpu);

// ---- machine stop (main.c) ----------------------------------------------------------------------
void xh_stop(uint32_t reason, uint32_t code, const char *why);     // any thread; returns
bool xh_stopping(void);
void xh_guest_ready(void);

// ---- host-call vector (hostcalls.c, arch thunks) ------------------------------------------------
extern struct xh_info g_info;
const struct xh_vector *xh_vector(void);

// ---- devices ------------------------------------------------------------------------------------
// PL011 console (uart.c)
void xh_uart_init(uint64_t base);

// Disks (blk.c)
typedef struct xh_blk xh_blk;
xh_blk  *xh_blk_open(const xh_disk_config *config, const char **error);
uint64_t xh_blk_bytes(const xh_blk *b);
bool     xh_blk_writable(const xh_blk *b);
int      xh_blk_read(xh_blk *b, uint64_t offset, void *buffer, uint64_t length);
int      xh_blk_write(xh_blk *b, uint64_t offset, const void *buffer, uint64_t length);
int      xh_blk_flush(xh_blk *b);
void     xh_blk_close(xh_blk *b);

// virtio-mmio transport, version 2, split queues (virtio.c)
#define XH_VIRTIO_QUEUES 4
// The largest queue a driver may ask for. 1024 as crosvm offers: Droid-VM's virtio-gpu driver needs 512 for its
// control queue (two descriptors for each of 256 slots) and refused the device at 256.
#define XH_VIRTIO_QSIZE  1024
typedef struct xh_virtio xh_virtio;
typedef struct { uint8_t *data; uint32_t length; bool writable; } xh_viov;
typedef struct {
  uint32_t device_id;
  uint64_t features;                    // device features besides VERSION_1
  unsigned nqueues;
  // A buffer chain arrived on `queue`: iov[0..n). Returns the bytes written into writable parts;
  // the transport puts the chain on the used ring and raises the interrupt.
  // Returning -1 keeps the chain (the device completes it later with xh_virtio_push).
  int64_t (*handle)(xh_virtio *dev, unsigned queue, uint16_t head, xh_viov *iov, unsigned n);
  uint32_t (*config_read)(xh_virtio *dev, uint32_t offset, unsigned size);
  void (*config_write)(xh_virtio *dev, uint32_t offset, unsigned size, uint32_t value);
  void (*reset)(xh_virtio *dev);
  // Queues (bit per index) the transport does not pop on a kick: the device takes their buffers
  // itself with xh_virtio_pop when it has data (device-to-driver queues). kick() says buffers came.
  uint32_t manual;
  void (*kick)(xh_virtio *dev, unsigned queue);
} xh_virtio_ops;
xh_virtio *xh_virtio_create(uint64_t base, unsigned spi, const xh_virtio_ops *ops, void *opaque, const char *name);
void      *xh_virtio_opaque(xh_virtio *dev);
// The device's lock (recursive): what a device's own threads take around its state, so that it
// nests the same way in them as in the vCPU that handles a register access.
void       xh_virtio_lock(xh_virtio *dev);
void       xh_virtio_unlock(xh_virtio *dev);
// Takes the next available chain of `queue` without the guest's kick (device-initiated data, as
// input events). Returns the count of parts, 0 if the guest has posted none.
unsigned   xh_virtio_pop(xh_virtio *dev, unsigned queue, uint16_t *head, xh_viov *iov, unsigned max);
void       xh_virtio_push(xh_virtio *dev, unsigned queue, uint16_t head, uint32_t written);
void       xh_virtio_config_changed(xh_virtio *dev);
// Whether the guest's driver has set DRIVER_OK: the device's queues are in use.
bool       xh_virtio_ready(xh_virtio *dev);
// Test hook: guest memory translation (default xh_ram).
extern void *(*xh_virtio_translate)(uint64_t pa, uint64_t len);
uint64_t   xh_virtio_mmio_read(xh_virtio *dev, uint64_t offset, unsigned size);
void       xh_virtio_mmio_write(xh_virtio *dev, uint64_t offset, unsigned size, uint64_t value);

xh_virtio *xh_virtio_blk_create(uint64_t base, unsigned spi, xh_blk *disk);
xh_virtio *xh_virtio_input_create(uint64_t base, unsigned spi);
xh_virtio *xh_virtio_gpu_create(uint64_t base, unsigned spi, unsigned width, unsigned height);

// Display (display.c): the scanout the guest draws.
typedef struct {
  unsigned width, height, stride;       // pixels, pixels, bytes; XRGB8888 (BGRA in memory)
  uint8_t *pixels;                      // xnuhost's shadow of the visible frame
} xh_display;
int   xh_display_init(const xh_config *c, unsigned *width, unsigned *height, unsigned *hz);
xh_display *xh_display_get(void);
// The rectangle changed (already copied into pixels).
void  xh_display_flush(unsigned x, unsigned y, unsigned w, unsigned h);
void  xh_display_close(void);

// Input (input.c): /dev/input/event* into the virtio-input device.
int   xh_input_start(xh_virtio *dev, unsigned width, unsigned height);
void  xh_input_stop(void);
// The display showed a guest frame (display.c): the end of input.c's touch-to-frame measurement.
void  xh_input_frame_shown(void);
// One reader's filter, the part of input.c the host tests run. An event as read, its time on CLOCK_MONOTONIC.
#define XH_INPUT_SLOTS 16
typedef struct { int64_t time_ns; uint16_t type, code; int32_t value; } xh_input_ev;
// A device's state as its ioctls give it: per slot the tracking id (-1 none) and position; the current slot; the side
// keys held (bit 0 volume down, 1 volume up, 2 power).
typedef struct { int32_t id[XH_INPUT_SLOTS], x[XH_INPUT_SLOTS], y[XH_INPUT_SLOTS]; unsigned slot, keys; } xh_input_state;
// What one gesture (first finger down to last finger up) looked like: logged when it ends.
typedef struct {
  unsigned frames, began, held;                   // touch frames; contacts begun; frames the guest had no buffers for
  bool     still;                                 // the guest showed no frame for 100 ms before the touch-down
  int64_t  first_scan_ns, last_scan_ns, gap_min_ns, gap_max_ns, read_sum_ns, read_min_ns, read_max_ns;
} xh_input_timing;
typedef struct xh_input_source {
  xh_virtio *dev;
  char       name[24];
  bool       touch, dropping;                     // dropping: after SYN_DROPPED, until the next SYN_REPORT
  int        fd;
  int      (*read_state)(struct xh_input_source *s, xh_input_state *state);   // 0 when read
  unsigned   slot, sent_keys, began, frame_began, drops;
  int32_t    sent_id[XH_INPUT_SLOTS];             // the tracking id the guest was last sent per slot
  xh_input_timing timing;
} xh_input_source;
void  xh_input_source_init(xh_input_source *s, xh_virtio *dev, const char *name, bool touch);
// Events read at now_ns (CLOCK_MONOTONIC) = now_counter (CNTVCT, hz ticks a second), to the guest.
void  xh_input_feed(xh_input_source *s, const xh_input_ev *ev, unsigned n, int64_t now_ns, uint64_t now_counter, uint64_t hz);
// virtio-input side
void  xh_virtio_input_event(xh_virtio *dev, uint16_t type, uint16_t code, int32_t value);
void  xh_virtio_input_set_abs(xh_virtio *dev, unsigned axis, int32_t min, int32_t max, int32_t res);
unsigned xh_virtio_input_backlog(xh_virtio *dev);   // events still waiting for the guest's buffers
// Input script (input_script.c): touches and key presses from a text file (input-script=FILE), for a host with no
// touchscreen and for repeatable checks. The file's format is at the top of input_script.c.
typedef enum { XH_STEP_ABS, XH_STEP_READY, XH_STEP_WAIT, XH_STEP_EVENT, XH_STEP_REPEAT } xh_input_step_kind;
typedef struct { xh_input_step_kind kind; uint16_t type, code; int32_t value, min, max; } xh_input_step;
// The file's text as steps (malloc'ed); -1 and the line's number for a line that is not understood.
int   xh_input_script_parse(const char *text, xh_input_step **steps, unsigned *count, unsigned *bad_line);
int   xh_input_script_start(xh_virtio *dev, const char *path);   // 0: running, or no path; -1: unreadable or a bad line
void  xh_input_script_stop(void);

// ---- iOS tasks (task.c; aarch64 Linux only) -----------------------------------------------------
// Forks the task factory. Must run before any other thread exists and before RAM is mapped.
int     xh_tasks_init(const xh_config *c);
bool    xh_tasks_available(void);      // tasks can run and their system calls reach the kernel
void    xh_tasks_dump(void);           // debugging: log every iOS thread
bool    xh_tasks_seccomp(void);        // ... trapped by a seccomp filter (the phone); else by text_fixup (qemu-user)
#define XH_SVC_MARK 0x00005c80u        // udf #0x5c80: what text_fixup leaves in place of `svc #0x80` without seccomp
extern bool xh_text_fixup_svc;
uint64_t xh_tasks_va_limit(void);
void    xh_tasks_shutdown(void);
int64_t xh_task_create(uint64_t *task);
int64_t xh_task_destroy(uint64_t task);
int64_t xh_task_map(uint64_t task, uint64_t va, uint64_t size, uint32_t prot, uint64_t phys);
int64_t xh_task_unmap(uint64_t task, uint64_t va, uint64_t size);
int64_t xh_task_protect(uint64_t task, uint64_t va, uint64_t size, uint32_t prot);
int64_t xh_task_read(uint64_t task, uint64_t va, void *buffer, uint64_t size);
int64_t xh_task_write(uint64_t task, uint64_t va, const void *buffer, uint64_t size);
int64_t xh_thread_create(uint64_t task, uint64_t *thread);
int64_t xh_thread_destroy(uint64_t thread);
int64_t xh_thread_run(xh_vcpu *v, uint64_t thread, struct xh_user_state *state, struct xh_trap *trap);
int64_t xh_thread_set_pac(uint64_t thread, uint32_t enabled);

#endif
