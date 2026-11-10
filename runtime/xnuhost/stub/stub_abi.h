// SPDX-License-Identifier: MIT
// What xnuhost and the stub inside every iOS task process share: the layout of the stub's region
// of the task's address space and the pages both sides read and write.
#ifndef XNUHOST_STUB_ABI_H
#define XNUHOST_STUB_ABI_H

#include <stdint.h>
#include "xnuhost/hostcall.h"

// The region: 1 GiB at the top of what iOS may not use (xh_info.user_va_limit is its base).
#define XS_REGION_SIZE    0x40000000ull
#define XS_TEXT_MAX       0x10000ull     // the stub's code, read + execute
#define XS_CONFIG_OFF     0x10000ull     // struct xs_config, private
#define XS_BOOTSTACK_OFF  0x20000ull     // the service thread's stack
#define XS_BOOTSTACK_SIZE 0x10000ull
#define XS_SERVICE_OFF    0x100000ull    // struct xs_service_page (a slot of the trap memfd)
#define XS_THREADS_OFF    0x200000ull    // thread areas
#define XS_AREA_SIZE      0x40000ull     // per thread: trap page, guard, signal stack, guard
#define XS_SLOT_SIZE      0x4000ull      // a slot of the trap memfd
#define XS_SIGSTACK_OFF   0x8000ull
#define XS_SIGSTACK_SIZE  0x34000ull
#define XS_MAX_THREADS    ((XS_REGION_SIZE - XS_THREADS_OFF) / XS_AREA_SIZE)

#define XS_FILTER_MAX 64
struct xs_filter_insn { uint16_t code; uint8_t jt, jf; uint32_t k; };

struct xs_config {
  uint64_t base;                       // of the region
  int32_t  ram_fd, trap_fd;
  uint32_t service_slot;               // this task's service page in the trap memfd
  uint32_t kick_signal;                // the signal xnuhost sends to force a trap
  int32_t  uid;                        // setresuid/setresgid target, -1 = keep
  uint32_t filter_length;              // 0 = no seccomp (hosts without it; svc is then not trapped)
  struct xs_filter_insn filter[XS_FILTER_MAX];
};

// One per iOS thread, shared (trap memfd slot): the first page of the thread's area in the task.
struct xs_trap_page {
  uint32_t to_task;                    // futex: xnuhost sets 1 to release the thread
  uint32_t to_host;                    // futex: the stub sets 1 when the thread has trapped
  uint32_t tid;                        // Linux thread id; the kernel zeroes it when the thread exits
  uint32_t command;                    // XS_RUN or XS_EXIT, read by the stub when released
  uint32_t pac_set, pac_enabled;       // apply PR_PAC_SET_ENABLED_KEYS before running
  uint32_t booted, reserved;
  struct xh_trap trap;
  struct xh_user_state state;
};
#define XS_RUN  0u
#define XS_EXIT 1u

struct xs_op {
  uint32_t op, prot;
  uint64_t va, size, offset;
  int64_t  result;                     // 0 or a negative errno
};
#define XS_OP_MAP      1u              // mmap(va, size, prot, MAP_SHARED|MAP_FIXED, ram_fd, offset)
#define XS_OP_UNMAP    2u
#define XS_OP_PROTECT  3u
#define XS_OP_CLONE    4u              // new thread: area index in va, trap memfd offset in offset
#define XS_OP_EXIT     5u              // exit_group
#define XS_OPS_MAX     128

// One per task, shared: orders for the task's service thread (its first Linux thread).
struct xs_service_page {
  uint32_t request;                    // futex: xnuhost sets 1 when ops are ready
  uint32_t done;                       // futex: the stub sets 1 when they are done
  uint32_t ready;                      // futex: the stub sets 1 once the task process is set up
  uint32_t count;
  int32_t  pid, setup_error;
  struct xs_op ops[XS_OPS_MAX];
};

_Static_assert(sizeof(struct xs_trap_page) <= 0x1000, "the trap page is one 4 KiB page");
_Static_assert(sizeof(struct xs_service_page) <= XS_SLOT_SIZE, "the service page fits a slot");
_Static_assert(sizeof(struct xs_config) <= 0x1000, "the config fits a page");

#endif
