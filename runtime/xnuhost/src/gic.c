// SPDX-License-Identifier: MIT
// The interrupt controller of xnuhost's board: a GICv3 with the register set ArmCpu
// (https://github.com/UniXNU/ArmCpu, commit 1604083, ArmCpu.cpp) programs and reads: distributor and
// redistributor frames by MMIO, the CPU interface by ICC_* system registers. A hosted kernel can use
// the irq_* host calls instead; both drive this state. Priorities are not modelled beyond the
// priority mask being zero or not: the board's interrupts are few and XNU runs them at one level.
#include "xnuhost.h"
#include <string.h>

#define NIDS (32 + XH_GIC_SPIS)
#define WORDS (NIDS / 32)

typedef struct {
  uint32_t enabled, pending, level, active;   // SGIs 0-15 and PPIs 16-31
  uint32_t group_enable;                      // ICC_IGRPEN1_EL1
  uint32_t pmr;
  uint32_t waker;
  uint8_t  priority[32];
} gic_cpu;

static struct {
  pthread_mutex_t lock;
  unsigned ncpu;
  uint32_t ctlr;
  uint32_t enabled[WORDS], pending[WORDS], level[WORDS], active[WORDS];   // word 0 unused
  uint8_t  priority[NIDS];
  uint8_t  target[NIDS];                      // CPU index, from GICD_IROUTER
  uint32_t icfgr[NIDS / 16];
  gic_cpu  cpu[XH_MAX_CPUS];
  uint64_t dist_base, redist_base;
} g;

void (*xh_gic_notify)(unsigned cpu, bool line);

// Highest-priority-by-number pending and enabled interrupt of a CPU, or NONE. Lock held.
static uint32_t best(unsigned c)
{
  gic_cpu *p = &g.cpu[c];
  if (!p->group_enable || p->pmr == 0) return XH_INTID_NONE;
  uint32_t level = p->level | (xh_cpu_timer_due(c) ? 1u << XH_INTID_VTIMER : 0);     // the timer line is a level
  uint32_t local = (p->pending | level) & p->enabled & ~p->active;
  if (local) return (uint32_t)__builtin_ctz(local);
  if (!(g.ctlr & 2) && !(g.ctlr & 1)) return XH_INTID_NONE;
  for (unsigned w = 1; w < WORDS; w++) {
    uint32_t bits = (g.pending[w] | g.level[w]) & g.enabled[w] & ~g.active[w];
    while (bits) {
      unsigned id = w * 32 + (unsigned)__builtin_ctz(bits);
      if (g.target[id] == c) return id;
      bits &= bits - 1;
    }
  }
  return XH_INTID_NONE;
}

// Tells every CPU the state of its line. Called with the lock held; the notify hook must not take it.
static void update(void)
{
  if (!xh_gic_notify) return;
  for (unsigned c = 0; c < g.ncpu; c++) xh_gic_notify(c, best(c) != XH_INTID_NONE);
}

void xh_gic_init(unsigned ncpu, uint64_t dist_base, uint64_t redist_base)
{
  // Recursive: the notify hook, called with the lock held, asks xh_gic_pending for the calling CPU.
  pthread_mutexattr_t attr;
  pthread_mutexattr_init(&attr);
  pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE);
  pthread_mutex_init(&g.lock, &attr);
  memset(g.enabled, 0, sizeof g.enabled);
  memset(g.pending, 0, sizeof g.pending);
  memset(g.level, 0, sizeof g.level);
  memset(g.active, 0, sizeof g.active);
  memset(g.target, 0, sizeof g.target);
  memset(g.cpu, 0, sizeof g.cpu);
  g.ncpu = ncpu;
  g.ctlr = 0;
  g.dist_base = dist_base;
  g.redist_base = redist_base;
  for (unsigned c = 0; c < XH_MAX_CPUS; c++) { g.cpu[c].pmr = 0xff; g.cpu[c].waker = 6; }
}

bool xh_gic_pending(unsigned cpu)
{
  pthread_mutex_lock(&g.lock);
  bool yes = best(cpu) != XH_INTID_NONE;
  pthread_mutex_unlock(&g.lock);
  return yes;
}

void xh_gic_set_spi(unsigned intid, bool level)
{
  if (intid < 32 || intid >= NIDS) return;
  pthread_mutex_lock(&g.lock);
  if (level) g.level[intid / 32] |= 1u << (intid % 32); else g.level[intid / 32] &= ~(1u << (intid % 32));
  update();
  pthread_mutex_unlock(&g.lock);
}

void xh_gic_set_ppi(unsigned cpu, unsigned intid, bool level)
{
  if (cpu >= g.ncpu || intid < 16 || intid >= 32) return;
  pthread_mutex_lock(&g.lock);
  if (level) g.cpu[cpu].level |= 1u << intid; else g.cpu[cpu].level &= ~(1u << intid);
  update();
  pthread_mutex_unlock(&g.lock);
}

void xh_gic_send_sgi(unsigned cpu, unsigned sgi)
{
  if (cpu >= g.ncpu || sgi >= 16) return;
  pthread_mutex_lock(&g.lock);
  g.cpu[cpu].pending |= 1u << sgi;
  update();
  pthread_mutex_unlock(&g.lock);
}

uint32_t xh_gic_ack(unsigned cpu)
{
  pthread_mutex_lock(&g.lock);
  uint32_t id = best(cpu);
  if (id < 32) {
    g.cpu[cpu].pending &= ~(1u << id);
    g.cpu[cpu].active |= 1u << id;
  } else if (id != XH_INTID_NONE) {
    g.pending[id / 32] &= ~(1u << (id % 32));
    g.active[id / 32] |= 1u << (id % 32);
  }
  update();
  pthread_mutex_unlock(&g.lock);
  return id;
}

void xh_gic_eoi(unsigned cpu, uint32_t intid)
{
  pthread_mutex_lock(&g.lock);
  if (intid < 32) g.cpu[cpu].active &= ~(1u << intid);
  else if (intid < NIDS) g.active[intid / 32] &= ~(1u << (intid % 32));
  update();                                   // a level line still high is pending again
  pthread_mutex_unlock(&g.lock);
}

void xh_gic_enable(unsigned cpu, uint32_t intid, bool enable)
{
  pthread_mutex_lock(&g.lock);
  if (intid < 32) {
    if (enable) g.cpu[cpu].enabled |= 1u << intid; else g.cpu[cpu].enabled &= ~(1u << intid);
  } else if (intid < NIDS) {
    if (enable) { g.enabled[intid / 32] |= 1u << (intid % 32); g.target[intid] = (uint8_t)cpu; }
    else g.enabled[intid / 32] &= ~(1u << (intid % 32));
  }
  if (enable) { g.ctlr |= 3; g.cpu[cpu].group_enable = 1; }             // the host-call path needs no register setup
  update();
  pthread_mutex_unlock(&g.lock);
}

// ---- CPU interface ------------------------------------------------------------------------------
bool xh_gic_sysreg(unsigned c, const xh_sys_insn *s, uint64_t *value)
{
  unsigned key = xh_sys_key(s);
  gic_cpu *p = &g.cpu[c];
  switch (key) {
  case XH_SYSREG(3, 0, 12, 12, 0):            // ICC_IAR1_EL1
    if (s->read) *value = xh_gic_ack(c);
    return true;
  case XH_SYSREG(3, 0, 12, 12, 1):            // ICC_EOIR1_EL1
    if (!s->read) xh_gic_eoi(c, (uint32_t)(*value & 0xffffff));
    return true;
  case XH_SYSREG(3, 0, 12, 12, 2):            // ICC_HPPIR1_EL1
    pthread_mutex_lock(&g.lock);
    if (s->read) *value = best(c);
    pthread_mutex_unlock(&g.lock);
    return true;
  case XH_SYSREG(3, 0, 12, 8, 0):             // ICC_IAR0_EL1: no group 0 interrupts
  case XH_SYSREG(3, 0, 12, 8, 2):             // ICC_HPPIR0_EL1
    if (s->read) *value = XH_INTID_NONE;
    return true;
  case XH_SYSREG(3, 0, 12, 12, 5):            // ICC_SRE_EL1: system registers on, bypass off
    if (s->read) *value = 7;
    return true;
  case XH_SYSREG(3, 0, 12, 12, 7):            // ICC_IGRPEN1_EL1
    pthread_mutex_lock(&g.lock);
    if (s->read) *value = p->group_enable; else { p->group_enable = *value & 1; update(); }
    pthread_mutex_unlock(&g.lock);
    return true;
  case XH_SYSREG(3, 0, 4, 6, 0):              // ICC_PMR_EL1
    pthread_mutex_lock(&g.lock);
    if (s->read) *value = p->pmr; else { p->pmr = *value & 0xff; update(); }
    pthread_mutex_unlock(&g.lock);
    return true;
  case XH_SYSREG(3, 0, 12, 11, 5): {          // ICC_SGI1R_EL1
    if (s->read) return true;
    unsigned sgi = (unsigned)(*value >> 24) & 15;
    if (*value & (1ull << 40)) {              // IRM: all but self
      for (unsigned t = 0; t < g.ncpu; t++) if (t != c) xh_gic_send_sgi(t, sgi);
    } else if (((*value >> 16) & 0xff) == 0) {    // Aff1 0: the board's CPUs are Aff0 0..7
      for (unsigned t = 0; t < g.ncpu && t < 16; t++) if (*value & (1ull << t)) xh_gic_send_sgi(t, sgi);
    }
    return true;
  }
  case XH_SYSREG(3, 0, 12, 12, 3):            // ICC_BPR1_EL1
  case XH_SYSREG(3, 0, 12, 12, 4):            // ICC_CTLR_EL1
  case XH_SYSREG(3, 0, 12, 12, 6):            // ICC_IGRPEN0_EL1
  case XH_SYSREG(3, 0, 12, 8, 1):             // ICC_EOIR0_EL1
  case XH_SYSREG(3, 0, 12, 8, 3):             // ICC_BPR0_EL1
  case XH_SYSREG(3, 0, 12, 11, 1):            // ICC_DIR_EL1
  case XH_SYSREG(3, 0, 12, 11, 3):            // ICC_RPR_EL1
  case XH_SYSREG(3, 0, 12, 11, 7):            // ICC_SGI0R_EL1
    if (s->read) *value = key == XH_SYSREG(3, 0, 12, 11, 3) ? 0xff : 0;
    return true;
  }
  return false;
}

// ---- distributor --------------------------------------------------------------------------------
static uint32_t *bank(uint64_t offset, unsigned *word)
{
  *word = (unsigned)(offset & 0x7f) / 4;
  if (*word == 0 || *word >= WORDS) return NULL;
  switch (offset & ~0x7full) {
  case 0x100: case 0x180: return g.enabled;
  case 0x200: case 0x280: return g.pending;
  case 0x300: case 0x380: return g.active;
  }
  return NULL;
}

static uint64_t dist_read(void *opaque, uint64_t off, unsigned size)
{
  (void)opaque;
  uint64_t v = 0;
  unsigned word;
  pthread_mutex_lock(&g.lock);
  if (off == 0x0) v = g.ctlr;
  else if (off == 0x4) v = WORDS - 1;                          // GICD_TYPER: ITLinesNumber
  else if (off == 0x8) v = 0x43b;                              // GICD_IIDR: implementer Arm
  else if (off == 0xffe8) v = 0x3b;                            // GICD_PIDR2: architecture revision 3
  else if (off >= 0x100 && off < 0x400) {
    uint32_t *b = bank(off, &word);
    if (b) v = b[word] | (b == g.pending ? g.level[word] : 0);
  } else if (off >= 0x400 && off < 0x400 + NIDS) {
    for (unsigned i = 0; i < size && off - 0x400 + i < NIDS; i++) v |= (uint64_t)g.priority[off - 0x400 + i] << (i * 8);
  } else if (off >= 0xc00 && off < 0xc00 + NIDS / 4) v = g.icfgr[(off - 0xc00) / 4];
  else if (off >= 0x6000 && off < 0x6000 + 8 * NIDS) {
    v = g.target[(off - 0x6000) / 8];
    if (off & 4) v = 0;
  }
  pthread_mutex_unlock(&g.lock);
  return v;
}

static void dist_write(void *opaque, uint64_t off, unsigned size, uint64_t value)
{
  (void)opaque;
  unsigned word;
  pthread_mutex_lock(&g.lock);
  if (off == 0x0) g.ctlr = (uint32_t)value & 0x37;
  else if (off >= 0x100 && off < 0x400) {
    uint32_t *b = bank(off, &word);
    if (b) { if (off & 0x80) b[word] &= ~(uint32_t)value; else b[word] |= (uint32_t)value; }
  } else if (off >= 0x400 && off < 0x400 + NIDS) {
    for (unsigned i = 0; i < size && off - 0x400 + i < NIDS; i++) g.priority[off - 0x400 + i] = (uint8_t)(value >> (i * 8));
  } else if (off >= 0xc00 && off < 0xc00 + NIDS / 4) g.icfgr[(off - 0xc00) / 4] = (uint32_t)value;
  else if (off >= 0x6000 && off < 0x6000 + 8 * NIDS && !(off & 4)) {
    unsigned cpu = (unsigned)value & 0xff;                     // Aff0
    g.target[(off - 0x6000) / 8] = (uint8_t)(cpu < g.ncpu ? cpu : 0);
  }
  update();
  pthread_mutex_unlock(&g.lock);
}

// ---- redistributors: RD_base then SGI_base, 64 KiB each, per CPU --------------------------------
static uint64_t redist_read(void *opaque, uint64_t off, unsigned size)
{
  (void)opaque; (void)size;
  unsigned c = (unsigned)(off / 0x20000);
  uint64_t r = off % 0x20000, v = 0;
  if (c >= g.ncpu) return 0;
  gic_cpu *p = &g.cpu[c];
  pthread_mutex_lock(&g.lock);
  if (r == 0x8) v = (uint64_t)c << 8 | (c == g.ncpu - 1 ? 0x10 : 0);     // GICR_TYPER low: number, Last
  else if (r == 0xc) v = c;                                              // GICR_TYPER high: affinity
  else if (r == 0x14) v = p->waker;
  else if (r == 0xffe8) v = 0x3b;
  else if (r == 0x10100 || r == 0x10180) v = p->enabled;
  else if (r == 0x10200 || r == 0x10280) v = p->pending | p->level;
  else if (r == 0x10300 || r == 0x10380) v = p->active;
  else if (r == 0x10080) v = 0xffffffff;                                 // GICR_IGROUPR0: all group 1
  else if (r >= 0x10400 && r < 0x10420) memcpy(&v, p->priority + (r - 0x10400), size < 8 ? size : 8);
  pthread_mutex_unlock(&g.lock);
  if (r == 0x8 && size == 8) v |= (uint64_t)c << 32;
  return v;
}

static void redist_write(void *opaque, uint64_t off, unsigned size, uint64_t value)
{
  (void)opaque;
  unsigned c = (unsigned)(off / 0x20000);
  uint64_t r = off % 0x20000;
  if (c >= g.ncpu) return;
  gic_cpu *p = &g.cpu[c];
  pthread_mutex_lock(&g.lock);
  if (r == 0x14) p->waker = (value & 2) ? 6 : 0;               // ChildrenAsleep follows ProcessorSleep
  else if (r == 0x10100) p->enabled |= (uint32_t)value;
  else if (r == 0x10180) p->enabled &= ~(uint32_t)value;
  else if (r == 0x10200) p->pending |= (uint32_t)value;
  else if (r == 0x10280) p->pending &= ~(uint32_t)value;
  else if (r == 0x10300) p->active |= (uint32_t)value;
  else if (r == 0x10380) p->active &= ~(uint32_t)value;
  else if (r >= 0x10400 && r < 0x10420) memcpy(p->priority + (r - 0x10400), &value, size < 8 ? size : 8);
  update();
  pthread_mutex_unlock(&g.lock);
}

void xh_gic_map(void)
{
  xh_mmio_add(&(xh_mmio_region){ .base = g.dist_base, .size = 0x10000, .name = "gicd", .read = dist_read, .write = dist_write });
  xh_mmio_add(&(xh_mmio_region){ .base = g.redist_base, .size = 0x20000ull * g.ncpu, .name = "gicr", .read = redist_read, .write = redist_write });
}
