// SPDX-License-Identifier: MIT
// PL011 at the board's "uart0": XNU's serial console (its VMAPPLE_UART path is a PL011 driver).
// Transmit only: bytes go to the log as "guest" lines.
#include "xnuhost.h"

static uint64_t uart_read(void *opaque, uint64_t off, unsigned size)
{
  (void)opaque; (void)size;
  switch (off) {
  case 0x18: return 0x90;            // UARTFR: transmit FIFO empty, receive FIFO empty
  case 0xfe0: return 0x11;           // peripheral ID: PL011
  case 0xfe4: return 0x10;
  case 0xfe8: return 0x14;
  case 0xfec: return 0x00;
  case 0xff0: return 0x0d;           // PrimeCell ID
  case 0xff4: return 0xf0;
  case 0xff8: return 0x05;
  case 0xffc: return 0xb1;
  }
  return 0;
}

static void uart_write(void *opaque, uint64_t off, unsigned size, uint64_t value)
{
  (void)opaque; (void)size;
  if (off == 0) { char c = (char)value; xh_log_raw(&c, 1); }
}

void xh_uart_init(uint64_t base)
{
  xh_mmio_add(&(xh_mmio_region){ .base = base, .size = 0x1000, .name = "uart0", .read = uart_read, .write = uart_write });
}
