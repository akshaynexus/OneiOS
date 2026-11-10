// SPDX-License-Identifier: MIT
// The host-call vector (xnuhost/hostcall.h). Each entry is an assembly thunk (arch_aarch64.S) that
// switches from the kernel's context to the host's and calls the xh_impl_ function here.
#define _GNU_SOURCE
#include "xnuhost.h"
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

struct xh_info g_info;

#if defined(__aarch64__) && defined(__linux__)
#include <fcntl.h>
#include <linux/falloc.h>
#include <sys/mman.h>

#define V(cpu) ((xh_vcpu *)(cpu))

void xh_impl_console_write(struct xh_cpu *cpu, const char *text, uint64_t length) { (void)cpu; xh_log_raw(text, length); }

void xh_impl_halt(struct xh_cpu *cpu, uint32_t reason, uint32_t code)
{
  char why[64];
  snprintf(why, sizeof why, "the guest halted on cpu%u", cpu->index);
  xh_stop(reason, code, why);
  for (;;) pause();
}

void xh_impl_ready(struct xh_cpu *cpu) { (void)cpu; xh_guest_ready(); }

int64_t xh_impl_set_upcalls(struct xh_cpu *cpu, const struct xh_upcalls *upcalls)
{
  (void)cpu;
  if (!upcalls || !upcalls->irq) return -EINVAL;
  g_upcalls = *upcalls;
  g_upcalls_set = true;
  return 0;
}

uint64_t xh_impl_wall_time(struct xh_cpu *cpu) { (void)cpu; return (uint64_t)time(NULL); }

int64_t xh_impl_cpu_start(struct xh_cpu *cpu, uint32_t index, uint64_t entry, uint64_t arg)
{
  (void)cpu;
  int rc = xh_cpu_start(index, entry, arg, 0, 0);
  return rc == 0 ? 0 : rc == -2 ? -EALREADY : -EINVAL;
}

void xh_impl_cpu_idle(struct xh_cpu *cpu) { xh_cpu_idle(V(cpu)); }

uint32_t xh_impl_irq_ack(struct xh_cpu *cpu)
{
  uint32_t id = xh_gic_ack(cpu->index);
  xh_cpu_line(cpu->index, false);                  // recomputed: more may be pending
  return id;
}

void xh_impl_irq_eoi(struct xh_cpu *cpu, uint32_t intid) { xh_gic_eoi(cpu->index, intid); xh_cpu_line(cpu->index, false); }
void xh_impl_irq_enable(struct xh_cpu *cpu, uint32_t intid, uint32_t enable) { xh_gic_enable(cpu->index, intid, enable != 0); xh_cpu_line(cpu->index, false); }
void xh_impl_ipi(struct xh_cpu *cpu, uint32_t target, uint32_t sgi) { (void)cpu; xh_gic_send_sgi(target, sgi); }
void xh_impl_timer_set(struct xh_cpu *cpu, uint64_t deadline) { xh_cpu_timer_set(V(cpu), deadline); }
void xh_impl_frame_resume(struct xh_cpu *cpu, const struct xh_frame *frame) { xh_cpu_frame_resume(V(cpu), frame); }

uint64_t xh_impl_io_map(struct xh_cpu *cpu, uint64_t phys, uint64_t size)
{
  (void)cpu;
  if (phys >= g_mem.io_phys && phys - g_mem.io_phys <= g_mem.io_size && size <= g_mem.io_size - (phys - g_mem.io_phys))
    return (uint64_t)(uintptr_t)g_mem.io_host + (phys - g_mem.io_phys);
  // RAM outside the kernel's window (the boot framebuffer) is ordinary memory in the aperture.
  return (uint64_t)(uintptr_t)xh_ram(phys, size);
}

uint64_t xh_impl_mmio_read(struct xh_cpu *cpu, uint64_t phys, uint32_t size) { (void)cpu; return xh_mmio_read(phys, size); }
void xh_impl_mmio_write(struct xh_cpu *cpu, uint64_t phys, uint32_t size, uint64_t value) { xh_mmio_write(phys, size, value); xh_cpu_line(cpu->index, false); }

int64_t xh_impl_ram_discard(struct xh_cpu *cpu, uint64_t phys, uint64_t size)
{
  (void)cpu;
  uint8_t *p = xh_ram(phys, size);
  if (!p || ((phys | size) & (g_info.host_page_size - 1))) return -EINVAL;
  // The pages go back to Linux and read as zero afterwards, in every mapping of them.
  return fallocate(g_mem.fd, FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, (off_t)(phys - g_mem.phys), (off_t)size) ? -errno : 0;
}

uint32_t xh_impl_text_fixup(struct xh_cpu *cpu, void *text, uint64_t size) { (void)cpu; return xh_text_fixup(text, size); }

int64_t xh_impl_task_create(struct xh_cpu *cpu, uint64_t *task) { (void)cpu; return xh_task_create(task); }
int64_t xh_impl_task_destroy(struct xh_cpu *cpu, uint64_t task) { (void)cpu; return xh_task_destroy(task); }
int64_t xh_impl_task_map(struct xh_cpu *cpu, uint64_t task, uint64_t va, uint64_t size, uint32_t prot, uint64_t phys) { (void)cpu; return xh_task_map(task, va, size, prot, phys); }
int64_t xh_impl_task_unmap(struct xh_cpu *cpu, uint64_t task, uint64_t va, uint64_t size) { (void)cpu; return xh_task_unmap(task, va, size); }
int64_t xh_impl_task_protect(struct xh_cpu *cpu, uint64_t task, uint64_t va, uint64_t size, uint32_t prot) { (void)cpu; return xh_task_protect(task, va, size, prot); }
int64_t xh_impl_task_read(struct xh_cpu *cpu, uint64_t task, uint64_t va, void *buffer, uint64_t size) { (void)cpu; return xh_task_read(task, va, buffer, size); }
int64_t xh_impl_task_write(struct xh_cpu *cpu, uint64_t task, uint64_t va, const void *buffer, uint64_t size) { (void)cpu; return xh_task_write(task, va, buffer, size); }
int64_t xh_impl_thread_create(struct xh_cpu *cpu, uint64_t task, uint64_t *thread) { (void)cpu; return xh_thread_create(task, thread); }
int64_t xh_impl_thread_destroy(struct xh_cpu *cpu, uint64_t thread) { (void)cpu; return xh_thread_destroy(thread); }
int64_t xh_impl_thread_run(struct xh_cpu *cpu, uint64_t thread, struct xh_user_state *state, struct xh_trap *trap) { return xh_thread_run(V(cpu), thread, state, trap); }
int64_t xh_impl_thread_set_pac(struct xh_cpu *cpu, uint64_t thread, uint32_t enabled) { (void)cpu; return xh_thread_set_pac(thread, enabled); }

int64_t xh_impl_kernel_map(struct xh_cpu *cpu, uint64_t va, uint64_t size, uint32_t prot, uint64_t phys) { (void)cpu; return xh_mem_kernel_map(va, size, prot, phys); }
int64_t xh_impl_kernel_unmap(struct xh_cpu *cpu, uint64_t va, uint64_t size) { (void)cpu; return xh_mem_kernel_unmap(va, size); }

// A load or store on a device page the kernel mapped itself (IOMemoryMap): the kernel found the
// physical address in its page table and hands the fault back.
int64_t xh_impl_mmio_emulate(struct xh_cpu *cpu, struct xh_frame *frame, uint64_t phys)
{
  if (!frame || phys < g_mem.io_phys || phys - g_mem.io_phys >= g_mem.io_size) return -EINVAL;
  // The instruction is kernel text: in the RAM window (the kernel collection is loaded there).
  uint32_t *insn = xh_ram(frame->pc - g_info.ram_virt + g_info.ram_phys, 4);
  if (!insn || (frame->pc & 3) || frame->pc < g_info.ram_virt) return -EINVAL;
  xh_regs regs;
  memcpy(regs.x, frame->x, sizeof regs.x);
  regs.sp = frame->sp; regs.pc = frame->pc; regs.pstate = frame->pstate;
  if (!xh_mmio_emulate(&regs, *insn, phys)) return -ENOTSUP;
  memcpy(frame->x, regs.x, sizeof regs.x);
  frame->sp = regs.sp; frame->pc = regs.pc;
  V(cpu)->stat_mmio++;
  xh_cpu_line(cpu->index, false);                  // a device write may have changed this CPU's line
  return 0;
}

#define THUNK(name) extern void xh_hc_##name(void);
THUNK(console_write) THUNK(halt) THUNK(ready) THUNK(set_upcalls) THUNK(wall_time) THUNK(cpu_start) THUNK(cpu_idle)
THUNK(irq_ack) THUNK(irq_eoi) THUNK(irq_enable) THUNK(ipi) THUNK(timer_set) THUNK(frame_resume) THUNK(io_map)
THUNK(mmio_read) THUNK(mmio_write) THUNK(ram_discard) THUNK(text_fixup) THUNK(task_create) THUNK(task_destroy)
THUNK(task_map) THUNK(task_unmap) THUNK(task_protect) THUNK(task_read) THUNK(task_write) THUNK(thread_create)
THUNK(thread_destroy) THUNK(thread_run) THUNK(thread_set_pac) THUNK(kernel_map) THUNK(kernel_unmap) THUNK(mmio_emulate)

const struct xh_vector *xh_vector(void)
{
  static struct xh_vector v;
#define SET(name) v.name = (__typeof__(v.name))xh_hc_##name
  v.abi = XH_ABI_VERSION;
  v.size = sizeof v;
  v.info = &g_info;
  SET(console_write); SET(halt); SET(ready); SET(set_upcalls); SET(wall_time); SET(cpu_start); SET(cpu_idle);
  SET(irq_ack); SET(irq_eoi); SET(irq_enable); SET(ipi); SET(timer_set); SET(frame_resume); SET(io_map);
  SET(mmio_read); SET(mmio_write); SET(ram_discard); SET(text_fixup); SET(task_create); SET(task_destroy);
  SET(task_map); SET(task_unmap); SET(task_protect); SET(task_read); SET(task_write); SET(thread_create);
  SET(thread_destroy); SET(thread_run); SET(thread_set_pac); SET(kernel_map); SET(kernel_unmap); SET(mmio_emulate);
  return &v;
}

#else
const struct xh_vector *xh_vector(void) { return NULL; }
#endif
