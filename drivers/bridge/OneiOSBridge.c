// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// The guest end of the OneiOS bridge channel: a virtio-mmio device at /arm-io/bridge, compatible
// "oneios,bridge", whose host end is runtime/bridges/devices/bridge_dev.c in xnuhost. It carries the
// small messages of oneios_bridge_proto.h (haptic requests, Bluetooth HCI packets).
//
// The transport (window lookup, split virtqueues from one contiguous region, a worker that sleeps on
// the device's interrupt with a polling guard) follows UniXNU-droidvm's virtio-net transport
// (https://github.com/Droid-VM/UniXNU-droidvm, commit f9121cd, package/DroidVMVirtioNet.c and
// DroidVMDmaRegion.h, BSD-2-Clause-Patent). Changes: two message queues instead of ethernet frames, a
// hello exchange, per-channel handlers.
//
// oneios-bridge=0 leaves the device alone.
#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#include <kern/thread.h>
#include <kern/clock.h>
#include <kern/locks.h>
#include <kern/sched_prim.h>
#include <kern/misc_protos.h>
#include <machine/machine_routines.h>
#include <pexpert/pexpert.h>
#include <pexpert/device_tree.h>
#include <vm/vm_kern_xnu.h>
#include "oneios_bridge.h"

extern void IOSleep(unsigned milliseconds);
extern void delay(int usec);
extern uint64_t kvtophys(vm_offset_t va);
// ArmCpu (the GIC): a handler for one SPI, run in the primary interrupt context before the EOI; 0 when attached.
extern int armcpu_spi_attach(uint32_t intid, bool edge, void (*handler)(void *), void *context, uint32_t *readback, uint64_t *router);

enum {
    OB_SLOTS = 16,                  // messages in flight per direction
    OB_POLL_MS = 10,
    OB_GUARD_MS = 100,              // with the interrupt, the worker looks anyway after this long
    VIRTQ_DESC_F_WRITE = 2, VIRTQ_AVAIL_F_NO_INTERRUPT = 1,
};
struct ob_desc { uint64_t address; uint32_t length; uint16_t flags, next; };
_Static_assert(sizeof(struct ob_desc) == 16 && sizeof(struct ob_header) == 8, "wire layout");

// One page per queue: descriptors at 0, the driver (available) ring at 1024, the device (used) ring at 2048.
static struct {
    uintptr_t registers;
    vm_offset_t region;             // 2 ring pages, then OB_SLOTS transmit and OB_SLOTS receive buffers
    uint64_t region_pa;
    uint8_t *tx_ring, *rx_ring;
    uint8_t (*tx_data)[OB_MAX_MESSAGE], (*rx_data)[OB_MAX_MESSAGE];
    uint16_t tx_head, rx_used;
    bool up, interrupt;
    volatile uint32_t channels;     // from the host's hello
    struct { oneios_bridge_handler fn; void *context; } listener[OB_CH_COUNT];
} ob;

static LCK_GRP_DECLARE(ob_group, "oneios_bridge");
static LCK_MTX_DECLARE(ob_tx_lock, &ob_group);

// For a host rig reading guest memory, and for the log.
volatile uint32_t oneios_bridge_stage;          // 1 found, 2 mapped, 3 up, 4 worker running, 5 hello answered
volatile uint64_t oneios_bridge_counts[4];      // sent, received, send failures, interrupts
char oneios_bridge_status[64];

// A trapped MMIO access must be a single load/store with a plain base register.
static uint32_t ob_read32(unsigned offset) {
    uint32_t value;
    __asm__ volatile("ldr %w0, [%1]" : "=r"(value) : "r"(ob.registers + offset) : "memory");
    return value;
}
static void ob_write32(unsigned offset, uint32_t value) {
    __asm__ volatile("str %w0, [%1]\n\tdsb oshst" :: "r"(value), "r"(ob.registers + offset) : "memory");
}
static void ob_write64(unsigned offset, uint64_t value) {
    ob_write32(offset, (uint32_t)value);
    ob_write32(offset + 4, (uint32_t)(value >> 32));
}
static inline void ob_barrier(void) { __asm__ volatile("dmb osh" ::: "memory"); }
static uint64_t ob_pa(const void *p) { return ob.region_pa + ((vm_offset_t)p - ob.region); }

static bool ob_buffers(void) {
    const uint64_t size = 2 * 4096 + 2 * (uint64_t)OB_SLOTS * OB_MAX_MESSAGE;
    vm_offset_t address = 0;
    if (ob.region) return true;
    if (kmem_alloc_contig(kernel_map, &address, size, 0, 0, 0, KMA_KOBJECT | KMA_ZERO | KMA_PERMANENT, VM_KERN_MEMORY_IOKIT) != KERN_SUCCESS) return false;
    ob.region = address;
    ob.region_pa = kvtophys(address);
    uint8_t *at = (uint8_t *)address;
    ob.tx_ring = at;
    ob.rx_ring = at + 4096;
    ob.tx_data = (uint8_t (*)[OB_MAX_MESSAGE])(at + 2 * 4096);
    ob.rx_data = (uint8_t (*)[OB_MAX_MESSAGE])(at + 2 * 4096 + (uint64_t)OB_SLOTS * OB_MAX_MESSAGE);
    return true;
}

static bool ob_reset(void) {
    ob_write32(0x70, 0);
    for (unsigned waited = 0; ob_read32(0x70); ++waited) {
        if (waited > 1000) return false;
        delay(1);
    }
    return true;
}

static bool ob_queue(unsigned queue, uint8_t *page) {
    ob_write32(0x30, queue);
    if (ob_read32(0x44) || ob_read32(0x34) < OB_SLOTS) return false;
    memset(page, 0, 4096);
    *(volatile uint16_t *)(page + 1024) = VIRTQ_AVAIL_F_NO_INTERRUPT;
    uint64_t physical = ob_pa(page);
    ob_barrier();
    ob_write32(0x38, OB_SLOTS);
    ob_write64(0x80, physical);
    ob_write64(0x90, physical + 1024);
    ob_write64(0xa0, physical + 2048);
    ob_write32(0x44, 1);
    return true;
}

static const char *ob_bring_up(void) {
    if (!ob_reset()) return "reset";
    ob_write32(0x70, 1);
    ob_write32(0x70, 3);
    ob_write32(0x14, 1);
    if (!(ob_read32(0x10) & 1)) return "no VIRTIO_F_VERSION_1";
    ob_write32(0x24, 0); ob_write32(0x20, 0);
    ob_write32(0x24, 1); ob_write32(0x20, 1);
    ob_write32(0x70, 11);
    if (!(ob_read32(0x70) & 8)) return "features refused";
    if (ob_read32(0x100) != OB_PROTO_VERSION) return "another protocol version";
    if (!ob_queue(OB_QUEUE_TX, ob.tx_ring)) return "transmit queue";
    if (!ob_queue(OB_QUEUE_RX, ob.rx_ring)) return "receive queue";
    // Every receive buffer is posted before DRIVER_OK.
    struct ob_desc *descriptors = (struct ob_desc *)ob.rx_ring;
    volatile uint16_t *available = (volatile uint16_t *)(ob.rx_ring + 1024);
    for (uint16_t i = 0; i < OB_SLOTS; ++i) {
        descriptors[i] = (struct ob_desc){ob_pa(ob.rx_data[i]), OB_MAX_MESSAGE, VIRTQ_DESC_F_WRITE, 0};
        available[2 + i] = i;
    }
    ob_barrier();
    available[1] = OB_SLOTS;
    ob.rx_used = 0;
    ob.tx_head = 0;
    ob_write32(0x70, 15);
    if ((ob_read32(0x70) & 0x4f) != 15) return "device not ready";
    ob_barrier();
    ob_write32(0x50, OB_QUEUE_RX);
    return NULL;
}

int oneios_bridge_send(uint16_t channel, uint16_t type, const void *payload, uint32_t length) {
    if (!ob.up || length > OB_MAX_PAYLOAD) { oneios_bridge_counts[2]++; return -1; }
    lck_mtx_lock(&ob_tx_lock);
    volatile uint16_t *used = (volatile uint16_t *)(ob.tx_ring + 2048);
    ob_barrier();
    if ((uint16_t)(ob.tx_head - used[1]) >= OB_SLOTS) {       // the host has not taken the earlier ones
        lck_mtx_unlock(&ob_tx_lock);
        oneios_bridge_counts[2]++;
        return -1;
    }
    uint16_t slot = ob.tx_head % OB_SLOTS;
    uint8_t *buffer = ob.tx_data[slot];
    struct ob_header header = {channel, type, length};
    memcpy(buffer, &header, sizeof(header));
    if (length) memcpy(buffer + sizeof(header), payload, length);
    struct ob_desc *descriptors = (struct ob_desc *)ob.tx_ring;
    volatile uint16_t *available = (volatile uint16_t *)(ob.tx_ring + 1024);
    descriptors[slot] = (struct ob_desc){ob_pa(buffer), (uint32_t)sizeof(header) + length, 0, 0};
    available[2 + slot] = slot;
    ob_barrier();
    available[1] = ++ob.tx_head;
    ob_barrier();
    ob_write32(0x50, OB_QUEUE_TX);
    lck_mtx_unlock(&ob_tx_lock);
    oneios_bridge_counts[0]++;
    return 0;
}

void oneios_bridge_listen(uint16_t channel, oneios_bridge_handler fn, void *context) {
    if (channel >= OB_CH_COUNT) return;
    ob.listener[channel].context = context;
    ob_barrier();
    ob.listener[channel].fn = fn;
}

bool oneios_bridge_ready(void) { return ob.up && ob.channels != 0; }
uint32_t oneios_bridge_channels(void) { return ob.channels; }

static void ob_dispatch(const uint8_t *message, uint32_t written) {
    struct ob_header header;
    if (written < sizeof(header)) return;
    memcpy(&header, message, sizeof(header));
    if (header.length > written - sizeof(header)) return;
    const uint8_t *payload = message + sizeof(header);
    if (header.channel == OB_CH_CONTROL) {
        if (header.type == OB_CTL_HELLO && header.length >= sizeof(struct ob_ctl_hello)) {
            struct ob_ctl_hello hello;
            memcpy(&hello, payload, sizeof(hello));
            ob.channels = hello.channels;
            oneios_bridge_stage = 5;
            snprintf(oneios_bridge_status, sizeof(oneios_bridge_status), "up, host channels %#x", hello.channels);
            printf("ONEIOS BRIDGE: on: host protocol %u, channels %#x (1 control, 2 haptics, 4 Bluetooth)\n", hello.version, hello.channels);
        }
        return;
    }
    if (header.channel < OB_CH_COUNT && ob.listener[header.channel].fn)
        ob.listener[header.channel].fn(ob.listener[header.channel].context, header.type, payload, header.length);
}

static void ob_rx_poll(void) {
    struct ob_desc *descriptors = (struct ob_desc *)ob.rx_ring;
    volatile uint16_t *available = (volatile uint16_t *)(ob.rx_ring + 1024);
    volatile uint16_t *used = (volatile uint16_t *)(ob.rx_ring + 2048);
    const volatile uint32_t *ring = (const volatile uint32_t *)(ob.rx_ring + 2052);
    uint16_t head = available[1];
    unsigned returned = 0;
    while (used[1] != ob.rx_used) {
        ob_barrier();
        uint32_t id = ring[(ob.rx_used % OB_SLOTS) * 2];
        uint32_t written = ring[(ob.rx_used % OB_SLOTS) * 2 + 1];
        if (id < OB_SLOTS) {
            if (written <= OB_MAX_MESSAGE) { oneios_bridge_counts[1]++; ob_dispatch(ob.rx_data[id], written); }
            descriptors[id] = (struct ob_desc){ob_pa(ob.rx_data[id]), OB_MAX_MESSAGE, VIRTQ_DESC_F_WRITE, 0};
            available[2 + (head + returned) % OB_SLOTS] = (uint16_t)id;
            ++returned;
        }
        ++ob.rx_used;
    }
    if (returned) {
        ob_barrier();
        available[1] = (uint16_t)(head + returned);
        ob_barrier();
        ob_write32(0x50, OB_QUEUE_RX);
        uint32_t interrupts = ob_read32(0x60) & 3;
        if (interrupts) ob_write32(0x64, interrupts);
    }
}

// The device's interrupt, in the primary interrupt context: the line is level, so it is acknowledged here.
static void ob_interrupt(void *context) {
    (void)context;
    uint32_t status = ob_read32(0x60) & 3;
    if (status) ob_write32(0x64, status);
    __atomic_fetch_add(&oneios_bridge_counts[3], 1, __ATOMIC_RELAXED);
    thread_wakeup_prim((event_t)&oneios_bridge_counts, FALSE, THREAD_AWAKENED);
}

static void ob_worker(void *parameter, wait_result_t result) {
    (void)parameter; (void)result;
    oneios_bridge_stage = 4;
    struct ob_ctl_hello hello = {OB_PROTO_VERSION, 0};
    (void)oneios_bridge_send(OB_CH_CONTROL, OB_CTL_HELLO, &hello, sizeof(hello));
    // The receive queue's VIRTQ_AVAIL_F_NO_INTERRUPT is off while the worker sleeps on the interrupt.
    if (ob.interrupt) { *(volatile uint16_t *)(ob.rx_ring + 1024) = 0; ob_barrier(); }
    for (;;) {
        const uint64_t seen = __atomic_load_n(&oneios_bridge_counts[3], __ATOMIC_RELAXED);
        ob_rx_poll();
        if (!ob.interrupt) { IOSleep(OB_POLL_MS); continue; }
        uint64_t deadline;
        clock_interval_to_deadline(OB_GUARD_MS, NSEC_PER_MSEC, &deadline);
        assert_wait_deadline((event_t)&oneios_bridge_counts, THREAD_UNINT, deadline);
        if (__atomic_load_n(&oneios_bridge_counts[3], __ATOMIC_RELAXED) != seen) clear_wait(current_thread(), THREAD_AWAKENED);
        else (void)thread_block(THREAD_CONTINUE_NULL);
    }
}

void oneios_bridge_start(void) {
    static bool tried;
    uint32_t enabled = 1;
    DTEntry armio, node;
    const void *value;
    unsigned int size;
    uint64_t ranges[3], reg[2];
    if (tried) return;
    tried = true;
    PE_parse_boot_argn("oneios-bridge", &enabled, sizeof(enabled));
    if (!enabled) { printf("ONEIOS BRIDGE: off: oneios-bridge=0\n"); return; }
    if (SecureDTLookupEntry(NULL, "/arm-io", &armio) != kSuccess ||
        SecureDTGetProperty(armio, "ranges", &value, &size) != kSuccess || size < sizeof(ranges)) return;
    memcpy(ranges, value, sizeof(ranges));
    if (SecureDTLookupEntry(NULL, "/arm-io/bridge", &node) != kSuccess) {
        printf("ONEIOS BRIDGE: off: no /arm-io/bridge in the device tree (haptics and Bluetooth have no host)\n");
        return;
    }
    oneios_bridge_stage = 1;
    if (SecureDTGetProperty(node, "reg", &value, &size) != kSuccess || size != sizeof(reg)) return;
    memcpy(reg, value, sizeof(reg));
    if (reg[0] < ranges[0] || reg[1] < 0x108 || reg[1] > ranges[2] || reg[0] - ranges[0] > ranges[2] - reg[1]) return;
    ob.registers = ml_io_map((vm_offset_t)(reg[0] - ranges[0] + ranges[1]), 0x1000);
    if (!ob.registers || ob_read32(0x00) != 0x74726976 || ob_read32(0x04) != 2 || ob_read32(0x08) != OB_VIRTIO_DEVICE_ID) {
        printf("ONEIOS BRIDGE: off: no bridge device at /arm-io/bridge\n");
        return;
    }
    oneios_bridge_stage = 2;
    if (!ob_buffers()) { printf("ONEIOS BRIDGE: off: no memory for the queues\n"); return; }
    const char *why = ob_bring_up();
    if (why) {
        printf("ONEIOS BRIDGE: off: %s\n", why);
        (void)ob_reset();
        return;
    }
    oneios_bridge_stage = 3;
    ob.up = true;
    // The window's interrupt: xnuhost's tree calls it "interrupts", DarwinBoot's "droidvm,intid". Without one the worker polls.
    uint32_t intid = 0;
    if ((SecureDTGetProperty(node, "droidvm,intid", &value, &size) == kSuccess && size == sizeof(intid)) ||
        (SecureDTGetProperty(node, "interrupts", &value, &size) == kSuccess && size == sizeof(intid))) {
        memcpy(&intid, value, sizeof(intid));
        ob.interrupt = armcpu_spi_attach(intid, false, ob_interrupt, NULL, NULL, NULL) == 0;
    }
    thread_t thread;
    if (kernel_thread_start(ob_worker, NULL, &thread) == KERN_SUCCESS) thread_deallocate(thread);
    else printf("ONEIOS BRIDGE: no worker thread: nothing from the host is received\n");
}

// Touch, buttons and the light sensor's HID device: the hosted kernel's HID archive (kernel/patches/oneios-hosted/hid,
// OneiOSHidInput.cpp), which subclasses the kernel collection's IOHIDDevice and so is linked only into a kernel built
// for that collection (kernel/build-hosted.sh). Weak: a kernel linked without the archive starts none.
__attribute__((weak)) void oneios_hid_input_start(void) {}
// The speaker and the microphone: the hosted kernel's audio archive (kernel/patches/oneios-hosted/audio,
// OneiOSAudio.c: UniXNU-droidvm's virtio-snd transport and IOAudio2Devices). Weak: a kernel without it has none.
__attribute__((weak)) void oneios_audio_start(void) {}
// The Wi-Fi, en0 on the phone's radio: the hosted kernel's network archive (kernel/patches/oneios-hosted/net,
// OneiOSWifi.cpp: the Wi-Fi controller and interface under the collection's IOEthernetController and
// IOEthernetInterface, UniXNU-droidvm's virtio-net transport). Weak: a kernel without it has no network interface.
__attribute__((weak)) void oneios_net_start(void) {}

void oneios_drivers_start(void) {
    oneios_bridge_start();
    (void)oneios_haptic_start();
    oneios_vibrator_start();
    oneios_bt_start();
    oneios_backlight_start();
    oneios_battery_start();
    oneios_thermal_start();
    oneios_camera_start();
    oneios_hid_input_start();   // after the backlight: the light sensor's device listens to its readings
    oneios_net_start();
    oneios_audio_start();
}
