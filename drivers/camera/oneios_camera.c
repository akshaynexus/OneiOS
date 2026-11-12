// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// The guest end of the OneiOS camera device: a virtio-mmio device at /arm-io/camera, compatible
// "oneios,camera", whose host end is runtime/bridges/camera/virtio_camera.c in xnuhost. It carries
// commands (which camera, what picture size) and whole NV12 frames (oneios_camera_proto.h).
//
// The transport (window lookup, split virtqueues from one contiguous region) follows
// ../bridge/OneiOSBridge.c, which follows UniXNU-droidvm's virtio-net transport
// (https://github.com/Droid-VM/UniXNU-droidvm, commit f9121cd, package/DroidVMVirtioNet.c,
// BSD-2-Clause-Patent). Changes: a command queue answered in place, a frame queue of three large
// buffers, no worker thread: the device node's reader polls (oneios_camera_dev.c).
//
// oneios-camera=0 leaves the device alone (no /dev/oneios-camera: the capture plug-in then offers no camera).
#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#include <kern/thread.h>
#include <kern/clock.h>
#include <kern/sched_prim.h>
#include <kern/misc_protos.h>
#include <machine/machine_routines.h>
#include <pexpert/pexpert.h>
#include <pexpert/device_tree.h>
#include <vm/vm_kern_xnu.h>
#include "oneios_camera.h"

extern void delay(int usec);
extern uint64_t kvtophys(vm_offset_t va);
// ArmCpu (the GIC): a handler for one SPI, run in the primary interrupt context before the EOI; 0 when attached.
extern int armcpu_spi_attach(uint32_t intid, bool edge, void (*handler)(void *), void *context, uint32_t *readback, uint64_t *router);

enum {
    OC_QSIZE = 16,                  // descriptors of each queue
    OC_BUFFERS = 3,                 // frame buffers: one being read, one filled meanwhile, one spare
    OC_BUFFER_SPAN = (OC_BUFFER_BYTES + 4095) & ~4095,
    VIRTQ_DESC_F_NEXT = 1, VIRTQ_DESC_F_WRITE = 2, VIRTQ_AVAIL_F_NO_INTERRUPT = 1,
    OC_ERROR_IO = 5, OC_ERROR_AGAIN = 35,       // EIO, EAGAIN (the Mach universe has no errno.h)
};
struct oc_desc { uint64_t address; uint32_t length; uint16_t flags, next; };
_Static_assert(sizeof(struct oc_desc) == 16, "wire layout");

// One page per queue: descriptors at 0, the driver (available) ring at 1024, the device (used) ring at 2048.
static struct {
    uintptr_t registers;
    vm_offset_t region;             // 2 ring pages, 1 command page (request at 0, reply at 1024), then the frame buffers
    uint64_t region_pa;
    uint8_t *control_ring, *frame_ring, *command, *buffers;
    uint16_t control_head, frame_head, frame_used;
    bool up, interrupt;
} oc;

int oneios_camera_event;
// For a host rig reading guest memory, and for the log.
volatile uint32_t oneios_camera_stage;          // 1 found, 2 mapped, 3 up
volatile uint64_t oneios_camera_counts[4];      // commands, frames taken, frames passed over, interrupts

// A trapped MMIO access must be a single load/store with a plain base register.
static uint32_t oc_read32(unsigned offset) {
    uint32_t value;
    __asm__ volatile("ldr %w0, [%1]" : "=r"(value) : "r"(oc.registers + offset) : "memory");
    return value;
}
static void oc_write32(unsigned offset, uint32_t value) {
    __asm__ volatile("str %w0, [%1]\n\tdsb oshst" :: "r"(value), "r"(oc.registers + offset) : "memory");
}
static void oc_write64(unsigned offset, uint64_t value) {
    oc_write32(offset, (uint32_t)value);
    oc_write32(offset + 4, (uint32_t)(value >> 32));
}
static inline void oc_barrier(void) { __asm__ volatile("dmb osh" ::: "memory"); }
static uint64_t oc_pa(const void *p) { return oc.region_pa + ((vm_offset_t)p - oc.region); }
static uint8_t *oc_buffer(unsigned index) { return oc.buffers + (uint64_t)index * OC_BUFFER_SPAN; }

static bool oc_memory(void) {
    const uint64_t size = 3 * 4096 + (uint64_t)OC_BUFFERS * OC_BUFFER_SPAN;
    vm_offset_t address = 0;
    if (oc.region) return true;
    if (kmem_alloc_contig(kernel_map, &address, size, 0, 0, 0, KMA_KOBJECT | KMA_ZERO | KMA_PERMANENT, VM_KERN_MEMORY_IOKIT) != KERN_SUCCESS) return false;
    oc.region = address;
    oc.region_pa = kvtophys(address);
    uint8_t *at = (uint8_t *)address;
    oc.control_ring = at;
    oc.frame_ring = at + 4096;
    oc.command = at + 2 * 4096;
    oc.buffers = at + 3 * 4096;
    return true;
}

static bool oc_reset(void) {
    oc_write32(0x70, 0);
    for (unsigned waited = 0; oc_read32(0x70); ++waited) {
        if (waited > 1000) return false;
        delay(1);
    }
    return true;
}

static bool oc_queue(unsigned queue, uint8_t *page) {
    oc_write32(0x30, queue);
    if (oc_read32(0x44) || oc_read32(0x34) < OC_QSIZE) return false;
    memset(page, 0, 4096);
    *(volatile uint16_t *)(page + 1024) = VIRTQ_AVAIL_F_NO_INTERRUPT;
    uint64_t physical = oc_pa(page);
    oc_barrier();
    oc_write32(0x38, OC_QSIZE);
    oc_write64(0x80, physical);
    oc_write64(0x90, physical + 1024);
    oc_write64(0xa0, physical + 2048);
    oc_write32(0x44, 1);
    return true;
}

// Gives frame buffer `index` to the host.
static void oc_post(unsigned index) {
    struct oc_desc *descriptors = (struct oc_desc *)oc.frame_ring;
    volatile uint16_t *available = (volatile uint16_t *)(oc.frame_ring + 1024);
    descriptors[index] = (struct oc_desc){oc_pa(oc_buffer(index)), OC_BUFFER_BYTES, VIRTQ_DESC_F_WRITE, 0};
    available[2 + oc.frame_head % OC_QSIZE] = (uint16_t)index;
    oc_barrier();
    available[1] = ++oc.frame_head;
    oc_barrier();
}

static const char *oc_bring_up(void) {
    if (!oc_reset()) return "reset";
    oc_write32(0x70, 1);
    oc_write32(0x70, 3);
    oc_write32(0x14, 1);
    if (!(oc_read32(0x10) & 1)) return "no VIRTIO_F_VERSION_1";
    oc_write32(0x24, 0); oc_write32(0x20, 0);
    oc_write32(0x24, 1); oc_write32(0x20, 1);
    oc_write32(0x70, 11);
    if (!(oc_read32(0x70) & 8)) return "features refused";
    if (oc_read32(0x100) != OC_PROTO_VERSION) return "another protocol version";
    if (!oc_queue(OC_QUEUE_CONTROL, oc.control_ring)) return "command queue";
    if (!oc_queue(OC_QUEUE_FRAME, oc.frame_ring)) return "frame queue";
    oc.control_head = oc.frame_head = oc.frame_used = 0;
    // Every frame buffer is posted before DRIVER_OK.
    for (unsigned i = 0; i < OC_BUFFERS; ++i) oc_post(i);
    oc_write32(0x70, 15);
    if ((oc_read32(0x70) & 0x4f) != 15) return "device not ready";
    oc_barrier();
    oc_write32(0x50, OC_QUEUE_FRAME);
    return NULL;
}

bool oneios_camera_up(void) { return oc.up; }

int oneios_camera_command(const struct oc_request *request, struct oc_reply *reply) {
    if (!oc.up) return OC_ERROR_IO;
    struct oc_desc *descriptors = (struct oc_desc *)oc.control_ring;
    volatile uint16_t *available = (volatile uint16_t *)(oc.control_ring + 1024);
    volatile uint16_t *used = (volatile uint16_t *)(oc.control_ring + 2048);
    uint8_t *out = oc.command, *in = oc.command + 1024;
    memcpy(out, request, sizeof(*request));
    memset(in, 0, sizeof(*reply));
    descriptors[0] = (struct oc_desc){oc_pa(out), sizeof(*request), VIRTQ_DESC_F_NEXT, 1};
    descriptors[1] = (struct oc_desc){oc_pa(in), sizeof(*reply), VIRTQ_DESC_F_WRITE, 0};
    available[2 + oc.control_head % OC_QSIZE] = 0;
    oc_barrier();
    available[1] = ++oc.control_head;
    oc_barrier();
    oc_write32(0x50, OC_QUEUE_CONTROL);
    // The host answers while it handles the notification; the wait is a guard.
    for (unsigned waited = 0; used[1] != oc.control_head; ++waited) {
        if (waited > 2000) return OC_ERROR_IO;
        delay(100);
        oc_barrier();
    }
    memcpy(reply, in, sizeof(*reply));
    oneios_camera_counts[0]++;
    return 0;
}

void oneios_camera_release(unsigned buffer) {
    if (!oc.up || buffer >= OC_BUFFERS) return;
    oc_post(buffer);
    oc_write32(0x50, OC_QUEUE_FRAME);
}

int oneios_camera_poll(struct oc_frame *frame, const uint8_t **picture, unsigned *buffer) {
    if (!oc.up) return OC_ERROR_IO;
    volatile uint16_t *used = (volatile uint16_t *)(oc.frame_ring + 2048);
    const volatile uint32_t *ring = (const volatile uint32_t *)(oc.frame_ring + 2052);
    int result = OC_ERROR_AGAIN;
    unsigned held = OC_BUFFERS;
    oc_barrier();
    while (used[1] != oc.frame_used) {
        uint32_t id = ring[(oc.frame_used % OC_QSIZE) * 2];
        uint32_t written = ring[(oc.frame_used % OC_QSIZE) * 2 + 1];
        ++oc.frame_used;
        if (id >= OC_BUFFERS) continue;
        struct oc_frame header;
        memcpy(&header, oc_buffer(id), sizeof(header));
        if (written >= OC_FRAME_DATA && header.magic == OC_FRAME_MAGIC && header.status) {
            // The camera stopped: the reader hears it once; what it held is from before.
            if (held < OC_BUFFERS) { oneios_camera_release(held); held = OC_BUFFERS; }
            oneios_camera_release(id);
            result = (int)header.status;
            continue;
        }
        if (!oca_frame_ok(&header, written)) { oneios_camera_release(id); continue; }      // nothing in it, or nonsense
        if (held < OC_BUFFERS) { oneios_camera_release(held); oneios_camera_counts[2]++; }  // an older frame: pass over it
        held = id;
        *frame = header;
        result = 0;
    }
    uint32_t pending = oc_read32(0x60) & 3;
    if (pending) oc_write32(0x64, pending);
    if (result == 0 && held < OC_BUFFERS) {
        *picture = oc_buffer(held) + OC_FRAME_DATA;
        *buffer = held;
        oneios_camera_counts[1]++;
    }
    return result;
}

// The device's interrupt, in the primary interrupt context: the line is level, so it is acknowledged here.
static void oc_interrupt(void *context) {
    (void)context;
    uint32_t status = oc_read32(0x60) & 3;
    if (status) oc_write32(0x64, status);
    __atomic_fetch_add(&oneios_camera_counts[3], 1, __ATOMIC_RELAXED);
    thread_wakeup_prim((event_t)&oneios_camera_event, FALSE, THREAD_AWAKENED);
}

void oneios_camera_start(void) {
    static bool tried;
    uint32_t enabled = 1;
    DTEntry armio, node;
    const void *value;
    unsigned int size;
    uint64_t ranges[3], reg[2];
    if (tried) return;
    tried = true;
    PE_parse_boot_argn("oneios-camera", &enabled, sizeof(enabled));
    if (!enabled) { printf("ONEIOS CAMERA: off: oneios-camera=0\n"); return; }
    if (SecureDTLookupEntry(NULL, "/arm-io", &armio) != kSuccess ||
        SecureDTGetProperty(armio, "ranges", &value, &size) != kSuccess || size < sizeof(ranges)) return;
    memcpy(ranges, value, sizeof(ranges));
    if (SecureDTLookupEntry(NULL, "/arm-io/camera", &node) != kSuccess) {
        printf("ONEIOS CAMERA: off: no /arm-io/camera in the device tree\n");
        return;
    }
    oneios_camera_stage = 1;
    if (SecureDTGetProperty(node, "reg", &value, &size) != kSuccess || size != sizeof(reg)) return;
    memcpy(reg, value, sizeof(reg));
    if (reg[0] < ranges[0] || reg[1] < 0x108 || reg[1] > ranges[2] || reg[0] - ranges[0] > ranges[2] - reg[1]) return;
    oc.registers = ml_io_map((vm_offset_t)(reg[0] - ranges[0] + ranges[1]), 0x1000);
    if (!oc.registers || oc_read32(0x00) != 0x74726976 || oc_read32(0x04) != 2 || oc_read32(0x08) != OC_VIRTIO_DEVICE_ID) {
        printf("ONEIOS CAMERA: off: no camera device at /arm-io/camera\n");
        return;
    }
    oneios_camera_stage = 2;
    if (!oc_memory()) { printf("ONEIOS CAMERA: off: no memory for the frame buffers\n"); return; }
    const char *why = oc_bring_up();
    if (why) {
        printf("ONEIOS CAMERA: off: %s\n", why);
        (void)oc_reset();
        return;
    }
    oneios_camera_stage = 3;
    oc.up = true;
    // The window's interrupt: xnuhost's tree calls it "interrupts", DarwinBoot's "droidvm,intid". Without one the reader polls.
    uint32_t intid = 0;
    if ((SecureDTGetProperty(node, "droidvm,intid", &value, &size) == kSuccess && size == sizeof(intid)) ||
        (SecureDTGetProperty(node, "interrupts", &value, &size) == kSuccess && size == sizeof(intid))) {
        memcpy(&intid, value, sizeof(intid));
        oc.interrupt = armcpu_spi_attach(intid, false, oc_interrupt, NULL, NULL, NULL) == 0;
    }
    if (oc.interrupt) { *(volatile uint16_t *)(oc.frame_ring + 1024) = 0; oc_barrier(); }
    struct oc_request ask = {.code = OC_CMD_INFO};
    struct oc_reply cameras;
    uint32_t count = oneios_camera_command(&ask, &cameras) == 0 && cameras.status == 0 ? cameras.count : 0;
    printf("ONEIOS CAMERA: on: %u cameras from the host so far, %u frame buffers of %u bytes, %s\n", count, (unsigned)OC_BUFFERS,
           (unsigned)OC_BUFFER_BYTES, oc.interrupt ? "interrupt" : "polling");
    oneios_camera_dev_attach();
    oneios_camera_service_start(count);     // AppleH9CamIn: CMCapture's probe loads the capture plug-in for it
}
