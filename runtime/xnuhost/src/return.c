// SPDX-License-Identifier: MIT
// The return's graceful-shutdown state machine (docs/booter.md "The return"). Pure and unit-tested
// (tests/test_xnuhost.c); main.c feeds it the return signals and the clock and does what it says. The way back to
// One UI never depends on it: the deadline is absolute, and anything unexpected ends the wait at once.
#include "xnuhost.h"

void xh_return_init(xh_return_state *st)
{
  st->active = false;
  st->started_ms = st->deadline_ms = 0;
}

xh_return_decision xh_return_on_signal(xh_return_state *st, int graceful_ms, bool guest_ready, uint64_t now_ms)
{
  if (st->active) {
    return XH_RETURN_STOP_SECOND;              // a second return request (or the chord again): stop now
  }
  if (graceful_ms <= 0 || !guest_ready) {
    return XH_RETURN_STOP_NOW;                 // graceful off, or iOS is not up to shut down: the immediate stop
  }
  st->active = true;
  st->started_ms = now_ms;
  st->deadline_ms = now_ms + (uint64_t)graceful_ms;
  return XH_RETURN_GRACEFUL;                   // ask iOS to shut down and keep running until it does or the deadline
}

bool xh_return_expired(const xh_return_state *st, uint64_t now_ms)
{
  return st->active && now_ms >= st->deadline_ms;
}

uint64_t xh_return_elapsed_ms(const xh_return_state *st, uint64_t now_ms)
{
  return st->active && now_ms >= st->started_ms ? now_ms - st->started_ms : 0;
}
