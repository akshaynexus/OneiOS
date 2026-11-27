// SPDX-License-Identifier: MIT
//! What xnuhost and the stub inside every iOS task process share (the C's `stub_abi.h`): the layout of the stub's
//! region of the task's address space and the pages both sides read and write. Both sides are Rust and use this
//! module, so there is no header to check it against: the asserts below check that the pages fit, its tests the
//! syndromes and the deferred queue's joins.

use crate::hostcall::{XhTrap, XhUserState, XH_PROT_EXEC, XH_PROT_READ, XH_PROT_WRITE};

// The region: 1 GiB at the top of what iOS may not use (xh_info.user_va_limit is its base).
pub const XS_REGION_SIZE: u64 = 0x4000_0000;
/// the stub's code, read + execute
pub const XS_TEXT_MAX: u64 = 0x10000;
/// struct xs_config, private
pub const XS_CONFIG_OFF: u64 = 0x10000;
/// the service thread's stack
pub const XS_BOOTSTACK_OFF: u64 = 0x20000;
pub const XS_BOOTSTACK_SIZE: u64 = 0x10000;
/// struct xs_service_page (a slot of the trap memfd)
pub const XS_SERVICE_OFF: u64 = 0x100000;
/// thread areas
pub const XS_THREADS_OFF: u64 = 0x200000;
/// per thread: trap page, guard, signal stack, guard
pub const XS_AREA_SIZE: u64 = 0x40000;
/// a slot of the trap memfd
pub const XS_SLOT_SIZE: u64 = 0x4000;
pub const XS_SIGSTACK_OFF: u64 = 0x8000;
pub const XS_SIGSTACK_SIZE: u64 = 0x34000;
pub const XS_MAX_THREADS: u64 = (XS_REGION_SIZE - XS_THREADS_OFF) / XS_AREA_SIZE;

pub const XS_FILTER_MAX: usize = 64;

/// `struct xs_filter_insn`: a classic BPF instruction (struct sock_filter).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct XsFilterInsn {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

/// `struct xs_config`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct XsConfig {
    /// of the region
    pub base: u64,
    pub ram_fd: i32,
    pub trap_fd: i32,
    /// this task's service page in the trap memfd
    pub service_slot: u32,
    /// the signal xnuhost sends to force a trap
    pub kick_signal: u32,
    /// setresuid/setresgid target, -1 = keep
    pub uid: i32,
    /// 0 = no seccomp (hosts without it; svc is then not trapped)
    pub filter_length: u32,
    pub filter: [XsFilterInsn; XS_FILTER_MAX],
}

/// `struct xs_op`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct XsOp {
    pub op: u32,
    pub prot: u32,
    pub va: u64,
    pub size: u64,
    pub offset: u64,
    /// 0 or a negative errno
    pub result: i64,
}

/// `struct xs_trap_page`: one per iOS thread, shared (trap memfd slot): the first page of the thread's area in the
/// task. The hand-off each way (docs/booter.md 2.6a): the side that waits spins on its word for a while (xnuhost:
/// spin-host; the stub: spin_ticks, which xnuhost sets), then says it sleeps (host_sleeping, task_sleeping) and waits
/// in a futex; the side that posts stores the word, then issues FUTEX_WAKE only if the other says it sleeps. Both use
/// a full barrier between their store and their load (store word / load flag; store flag / load word), so one of
/// them always sees the other's store.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct XsTrapPage {
    /// futex: xnuhost sets 1 to release the thread
    pub to_task: u32,
    /// futex: the stub sets XS_HOST_OPS_ACK when deferred maps finish, or XS_HOST_TRAPPED when the thread has trapped
    pub to_host: u32,
    /// Linux thread id; the kernel zeroes it when the thread exits
    pub tid: u32,
    /// XS_RUN or XS_EXIT, read by the stub when released
    pub command: u32,
    /// apply PR_PAC_SET_ENABLED_KEYS before running
    pub pac_set: u32,
    pub pac_enabled: u32,
    pub booted: u32,
    pub reserved: u32,
    /// xnuhost waits for to_host in a futex (else it spins or is not waiting)
    pub host_sleeping: u32,
    /// the stub waits for to_task in a futex
    pub task_sleeping: u32,
    /// the stub: its last wait for a release ended in a futex sleep
    pub slept: u32,
    /// xnuhost: maps the released thread carries out before it runs (XS_OPS_OFF)
    pub nops: u32,
    /// the stub: set when they are done (results in each op), then to_host is woken
    pub ops_done: u32,
    pub reserved2: u32,
    /// xnuhost: counter ticks the stub spins for its release before it sleeps
    pub spin_ticks: u64,
    /// the stub: CNTVCT_EL0 when its handler began, when it saw its release
    pub t_enter: u64,
    pub t_woken: u64,
    pub trap: XhTrap,
    pub state: XhUserState,
}
pub const XS_RUN: u32 = 0;
pub const XS_EXIT: u32 = 1;
/// `XsTrapPage.to_host` futex values: publishing the deferred-map acknowledgement on `to_host` itself lets
/// `FUTEX_WAIT(to_host, event)` return `EAGAIN` if the stub finishes its maps before the host enters the wait
/// (YMGPwcca/OneiOS 9e5a9933: previously `ops_done = 1` + `FUTEX_WAKE(to_host)` while `to_host == 0` could lose
/// the wakeup and stall the host vCPU up to 200 ms holding the task lock).
pub const XS_HOST_PENDING: u32 = 0;
pub const XS_HOST_TRAPPED: u32 = 1;
pub const XS_HOST_OPS_ACK: u32 = 2;

/// True when `to_host` shows the released thread has finished any deferred ops (either acknowledged or already trapped).
#[inline]
pub const fn xs_host_event_has_ack(event: u32) -> bool {
    event == XS_HOST_OPS_ACK || event == XS_HOST_TRAPPED
}
// Deferred maps and unmaps (XS_OP_MAP, XS_OP_UNMAP) after the trap page, in the same slot.
pub const XS_OPS_OFF: u64 = 0x1000;
pub const XS_THREAD_OPS: u32 = 256;
/// mmap(va, size, prot, MAP_SHARED|MAP_FIXED, ram_fd, offset)
pub const XS_OP_MAP: u32 = 1;
pub const XS_OP_UNMAP: u32 = 2;
pub const XS_OP_PROTECT: u32 = 3;
/// new thread: area index in va, trap memfd offset in offset
pub const XS_OP_CLONE: u32 = 4;
/// exit_group
pub const XS_OP_EXIT: u32 = 5;
/// the task's deferred maps and unmaps (up to 256) and the order itself
pub const XS_OPS_MAX: usize = 320;

/// A map is mmap(MAP_SHARED | MAP_FIXED | XS_MAP_POPULATE): the page tables are filled in the same call, so the
/// toucher's retried instruction does not take a second (Linux) fault on the page it was just given.
pub const XS_MAP_POPULATE: u32 = 0x8000;

impl XsOp {
    /// `xs_queue_join`: the deferred queue (task.rs, map-defer) joins `op` to `self`, the entry before it, where one
    /// host call does what the two did, in the same order (host-overhead, 2026-11-15). Returns true and rewrites
    /// `self` when they join:
    /// - map + map: the next page of the same file range with the same protection: one longer map; the same range
    ///   again: the later map (MAP_FIXED replaces);
    /// - unmap + unmap: adjacent or overlapping ranges: one unmap of both;
    /// - unmap + map of the same range: the map alone (MAP_FIXED replaces the old mapping in one step);
    /// - map + unmap of the same range: the unmap alone (nothing ran between them that could use the map).
    ///
    /// Anything else (a protect, a clone, ranges that only overlap) stays as two entries.
    pub fn queue_join(&mut self, op: &XsOp) -> bool {
        let end = self.va.wrapping_add(self.size);
        let op_end = op.va.wrapping_add(op.size);
        let same = self.va == op.va && self.size == op.size;
        match (self.op, op.op) {
            (XS_OP_MAP, XS_OP_MAP) => {
                if same {
                    *self = *op;
                    true
                } else if self.prot == op.prot && end == op.va && self.offset.wrapping_add(self.size) == op.offset {
                    self.size = self.size.wrapping_add(op.size);
                    true
                } else {
                    false
                }
            }
            (XS_OP_UNMAP, XS_OP_UNMAP) => {
                if op.va > end || op_end < self.va {
                    return false;
                }
                let lo = op.va.min(self.va);
                self.size = op_end.max(end).wrapping_sub(lo);
                self.va = lo;
                true
            }
            (XS_OP_UNMAP, XS_OP_MAP) | (XS_OP_MAP, XS_OP_UNMAP) if same => {
                *self = *op;
                true
            }
            _ => false,
        }
    }
}

/// `struct xs_service_page`: one per task, shared: orders for the task's service thread (its first Linux thread).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct XsServicePage {
    /// futex: xnuhost sets 1 when ops are ready
    pub request: u32,
    /// futex: the stub sets 1 when they are done
    pub done: u32,
    /// futex: the stub sets 1 once the task process is set up
    pub ready: u32,
    pub count: u32,
    pub pid: i32,
    pub setup_error: i32,
    pub ops: [XsOp; XS_OPS_MAX],
}

// The access a SIGSEGV or SIGBUS of a task thread was, as XH_PROT_* (0: unknown), from the syndrome in the signal
// frame's esr_context (for a user address Linux passes the hardware's ESR whole: arch/arm64/mm/fault.c set_thread_esr
// sanitizes kernel addresses only) and, without one, from whether the fault is at pc.
// A data abort with ISS.CM = 1 is a cache maintenance instruction by address (dc cvau, dc cvac, dc civac, dc cvap,
// ic ivau: what JSC's sys_icache_invalidate runs on the read-execute alias of its JIT region). The hardware reports it
// with WnR = 1, but it needs read access only (Linux's is_write_abort and XNU's inspect_data_abort take it as a read):
// a write here made the kernel refuse the fill of the read-execute page and fail the fault (docs/booter.md 2.6).
pub const XS_ESR_EC_SHIFT: u32 = 26;
pub const XS_ESR_WNR: u64 = 1 << 6;
pub const XS_ESR_CM: u64 = 1 << 8;

/// `xs_fault_access`: the access a task fault was (XH_PROT_*, 0 = unknown), from its syndrome or, without one, from
/// whether it is at pc.
pub const fn fault_access(esr: u64, far: u64, pc: u64) -> u32 {
    let ec = (esr >> XS_ESR_EC_SHIFT) & 0x3f;
    match ec {
        0x20 | 0x21 => XH_PROT_EXEC, // instruction abort (lower EL, same EL)
        0x24 | 0x25 if esr & XS_ESR_WNR != 0 && esr & XS_ESR_CM == 0 => XH_PROT_WRITE,
        0x24 | 0x25 => XH_PROT_READ,
        _ if far == pc => XH_PROT_EXEC,
        _ => 0,
    }
}

const _: () = assert!(core::mem::size_of::<XsTrapPage>() as u64 <= XS_OPS_OFF, "the trap page is one 4 KiB page");
const _: () = assert!(
    XS_OPS_OFF + XS_THREAD_OPS as u64 * core::mem::size_of::<XsOp>() as u64 <= XS_SLOT_SIZE,
    "a thread's deferred maps fit its slot"
);
const _: () = assert!(core::mem::size_of::<XsServicePage>() as u64 <= XS_SLOT_SIZE, "the service page fits a slot");
const _: () = assert!(core::mem::size_of::<XsConfig>() <= 0x1000, "the config fits a page");

#[cfg(test)]
mod tests {
    use super::*;

    // The syndromes are the hardware's, as Linux passes them on in esr_context for a user address: EC 0x24 (data
    // abort from a lower EL) with IL, ISS.CM (bit 8), ISS.WnR (bit 6) and the fault status code.
    #[test]
    fn fault_access_of_syndromes() {
        let (pc, far) = (0x1a0004000, 0xc998c0000);
        assert_eq!(fault_access(0x92000047, far, pc), XH_PROT_WRITE); // str: WnR, translation fault level 3
        assert_eq!(fault_access(0x9200004f, far, pc), XH_PROT_WRITE); // str: WnR, permission fault level 3
        assert_eq!(fault_access(0x92000007, far, pc), XH_PROT_READ); // ldr: translation fault level 3
                                                                     // dc cvau / ic ivau / dc civac on a page the host has not mapped: CM and WnR, translation fault (levels 0-3);
                                                                     // on a mapped page it may not read: CM and WnR, permission fault. A read either way (WebKit's
                                                                     // sys_icache_invalidate on the read-execute alias of a JIT page, phone 2026-11-14: it was taken as a write).
        for fsc in 4..=7 {
            assert_eq!(fault_access(0x92000140 | fsc, far, pc), XH_PROT_READ);
        }
        assert_eq!(fault_access(0x9200014f, far, pc), XH_PROT_READ);
        assert_eq!(fault_access(0x9200010b, far, pc), XH_PROT_READ); // CM without WnR (access flag fault)
        assert_eq!(fault_access(0x96000147, far, pc), XH_PROT_READ); // same EL (EC 0x25), CM and WnR
        assert_eq!(fault_access(0x82000007, far, pc), XH_PROT_EXEC); // instruction abort
        assert_eq!(fault_access(0x82000147, far, pc), XH_PROT_EXEC); // instruction abort: bits 6 and 8 mean nothing
        assert_eq!(fault_access(0, pc, pc), XH_PROT_EXEC); // no syndrome, at pc
        assert_eq!(fault_access(0, far, pc), 0); // no syndrome: unknown
    }

    fn qop(op: u32, va: u64, size: u64, prot: u32, offset: u64) -> XsOp {
        XsOp { op, prot, va, size, offset, result: 0 }
    }

    // The deferred queue's joins (host-overhead 2026-11-15).
    #[test]
    fn queue_joins() {
        const P: u64 = 0x4000;
        let mut last = qop(XS_OP_MAP, 0x100000, P, 3, 0x8000);
        // the next page of the same file range, same protection: one map of two pages
        assert!(last.queue_join(&qop(XS_OP_MAP, 0x100000 + P, P, 3, 0x8000 + P)));
        assert_eq!(last, qop(XS_OP_MAP, 0x100000, 2 * P, 3, 0x8000));
        // the next page with another protection, or another file page, or a gap: two entries
        assert!(!last.queue_join(&qop(XS_OP_MAP, 0x100000 + 2 * P, P, 1, 0x8000 + 2 * P)) && last.size == 2 * P);
        assert!(!last.queue_join(&qop(XS_OP_MAP, 0x100000 + 2 * P, P, 3, 0x40000)));
        assert!(!last.queue_join(&qop(XS_OP_MAP, 0x100000 + 3 * P, P, 3, 0x8000 + 3 * P)));
        // the same range mapped again: the later map
        last = qop(XS_OP_MAP, 0x200000, P, 1, 0x1000 * 4);
        assert!(last.queue_join(&qop(XS_OP_MAP, 0x200000, P, 3, 0x40000)));
        assert!(last.prot == 3 && last.offset == 0x40000 && last.size == P);
        // unmaps: adjacent below and above, overlapping, apart
        last = qop(XS_OP_UNMAP, 0x300000, P, 0, 0);
        assert!(last.queue_join(&qop(XS_OP_UNMAP, 0x300000 + P, P, 0, 0)) && last.va == 0x300000 && last.size == 2 * P);
        assert!(last.queue_join(&qop(XS_OP_UNMAP, 0x300000 - P, P, 0, 0)));
        assert!(last.va == 0x300000 - P && last.size == 3 * P);
        assert!(last.queue_join(&qop(XS_OP_UNMAP, 0x300000, 4 * P, 0, 0)));
        assert!(last.va == 0x300000 - P && last.size == 5 * P);
        assert!(!last.queue_join(&qop(XS_OP_UNMAP, 0x300000 + 6 * P, P, 0, 0)) && last.size == 5 * P);
        // unmap then map of the same page (a fault that changed the entry): the map alone, which replaces the old page
        last = qop(XS_OP_UNMAP, 0x400000, P, 0, 0);
        assert!(last.queue_join(&qop(XS_OP_MAP, 0x400000, P, 3, 0x10000)));
        assert!(last.op == XS_OP_MAP && last.offset == 0x10000 && last.prot == 3);
        // map then unmap of the same page: the unmap alone; of another size: both
        assert!(last.queue_join(&qop(XS_OP_UNMAP, 0x400000, P, 0, 0)));
        assert!(last.op == XS_OP_UNMAP && last.va == 0x400000 && last.size == P);
        last = qop(XS_OP_MAP, 0x400000, P, 3, 0x10000);
        assert!(!last.queue_join(&qop(XS_OP_UNMAP, 0x400000, 2 * P, 0, 0)) && last.op == XS_OP_MAP);
        // unmap then map of a part of it: both (the rest must still go)
        last = qop(XS_OP_UNMAP, 0x500000, 2 * P, 0, 0);
        assert!(!last.queue_join(&qop(XS_OP_MAP, 0x500000, P, 3, 0)));
        assert!(last.op == XS_OP_UNMAP && last.size == 2 * P);
        // a protect or a clone never joins
        last = qop(XS_OP_PROTECT, 0x600000, P, 1, 0);
        assert!(!last.queue_join(&qop(XS_OP_PROTECT, 0x600000 + P, P, 1, 0)));
        assert!(!last.queue_join(&qop(XS_OP_UNMAP, 0x600000, P, 0, 0)));
    }

    #[test]
    fn host_event_states() {
        assert!(!xs_host_event_has_ack(XS_HOST_PENDING));
        assert!(xs_host_event_has_ack(XS_HOST_OPS_ACK));
        assert!(xs_host_event_has_ack(XS_HOST_TRAPPED));
        // Distinct non-zero values so FUTEX_WAIT(to_host, XS_HOST_PENDING) wakes on OPS_ACK,
        // and FUTEX_WAIT(to_host, XS_HOST_OPS_ACK) wakes on TRAPPED.
        assert_ne!(XS_HOST_PENDING, XS_HOST_OPS_ACK);
        assert_ne!(XS_HOST_OPS_ACK, XS_HOST_TRAPPED);
        assert_ne!(XS_HOST_PENDING, XS_HOST_TRAPPED);
    }
}
