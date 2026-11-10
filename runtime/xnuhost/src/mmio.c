// SPDX-License-Identifier: MIT
// The board's device windows. The kernel reaches them either by loads and stores into the trapped
// I/O aperture (emulated here from the faulting instruction) or by the mmio_read/mmio_write host calls.
#include "xnuhost.h"
#include <string.h>

#define MAX_REGIONS 32
static xh_mmio_region regions[MAX_REGIONS];
static unsigned nregions;

int xh_mmio_add(const xh_mmio_region *r)
{
  if (nregions == MAX_REGIONS) return -1;
  for (unsigned i = 0; i < nregions; i++)
    if (r->base < regions[i].base + regions[i].size && regions[i].base < r->base + r->size) return -1;
  regions[nregions++] = *r;
  return 0;
}

static const xh_mmio_region *find(uint64_t pa)
{
  for (unsigned i = 0; i < nregions; i++)
    if (pa >= regions[i].base && pa - regions[i].base < regions[i].size) return &regions[i];
  return NULL;
}

bool xh_mmio_covers(uint64_t pa) { return find(pa) != NULL; }

uint64_t xh_mmio_read(uint64_t pa, unsigned size)
{
  const xh_mmio_region *r = find(pa);
  if (!r || !r->read) return 0;                    // unmapped bus address: reads as zero
  uint64_t v = r->read(r->opaque, pa - r->base, size);
  return size >= 8 ? v : v & ((1ull << (size * 8)) - 1);
}

void xh_mmio_write(uint64_t pa, unsigned size, uint64_t value)
{
  const xh_mmio_region *r = find(pa);
  if (r && r->write) r->write(r->opaque, pa - r->base, size, size >= 8 ? value : value & ((1ull << (size * 8)) - 1));
}

static uint64_t extend(uint64_t v, const xh_mem_insn *m)
{
  if (m->sign_extend && m->size < 8) {
    unsigned bits = m->size * 8;
    if (v & (1ull << (bits - 1))) v |= ~0ull << bits;
    if (!m->to64) v &= 0xffffffffull;
  }
  return v;
}

bool xh_mmio_emulate(xh_regs *regs, uint32_t insn, uint64_t pa)
{
  xh_mem_insn m;
  if (!xh_decode_mem(insn, &m)) return false;
  if (m.load) {
    xh_reg_set(regs, m.rt, extend(xh_mmio_read(pa, m.size), &m));
    if (m.pair) xh_reg_set(regs, m.rt2, extend(xh_mmio_read(pa + m.size, m.size), &m));
  } else {
    xh_mmio_write(pa, m.size, xh_reg_get(regs, m.rt));
    if (m.pair) xh_mmio_write(pa + m.size, m.size, xh_reg_get(regs, m.rt2));
  }
  if (m.writeback) {
    if (m.rn == 31) regs->sp += (uint64_t)m.offset;
    else regs->x[m.rn] += (uint64_t)m.offset;
  }
  regs->pc += 4;
  return true;
}
