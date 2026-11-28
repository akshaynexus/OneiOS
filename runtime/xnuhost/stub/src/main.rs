// SPDX-License-Identifier: MIT
//! The stub: the only Linux code inside an iOS task process (stub.c in Rust). Freestanding, position independent,
//! linked into a flat blob (stub.ld; build.sh) that xnuhost's task factory copies to the stub region. It does three
//! things:
//! - `xs_entry`: turns a forked child into an empty address space with nothing but this region, installs the signal
//!   handlers and the seccomp filter, and becomes the service thread;
//! - the service thread: carries out xnuhost's orders (map, unmap, protect, new thread);
//! - `xs_handler`: every system call, fault and forced stop of an iOS thread lands here as a signal; it hands the
//!   register state to xnuhost through the thread's trap page and waits to be resumed.
//!
//! The design is gVisor's systrap platform and User-Mode Linux's skas mode; the code is new.
//!
//! The blob has no relocations: code and constants are reached PC-relative (the static relocation model, adrp/adr),
//! nothing is written outside the stack and the shared pages (no .data, no .bss), no pointer is stored in a constant,
//! and no code path can panic (build.sh refuses a blob with a .got, .data or .bss, or with a panic path). Shared words
//! the other side waits on are atomics; the rest of the trap page is plain memory, as in the C.
#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{fence, AtomicU32, Ordering};
use xnuhost_abi::*;

const NR_FUTEX: usize = 98;
const NR_EXIT: usize = 93;
const NR_EXIT_GROUP: usize = 94;
const NR_SIGALTSTACK: usize = 132;
const NR_RT_SIGACTION: usize = 134;
const NR_SETRESUID: usize = 147;
const NR_SETRESGID: usize = 149;
const NR_PRCTL: usize = 167;
const NR_GETPID: usize = 172;
const NR_GETTID: usize = 178;
const NR_MUNMAP: usize = 215;
const NR_MMAP: usize = 222;
const NR_MPROTECT: usize = 226;
const NR_SECCOMP: usize = 277;

const FUTEX_WAIT: usize = 0;
const FUTEX_WAKE: usize = 1;
const PROT_RW: usize = 3;
const MAP_SHARED: usize = 1;
const MAP_PRIVATE: usize = 2;
const MAP_FIXED: usize = 0x10;
const MAP_ANONYMOUS: usize = 0x20;
const SIGILL: i32 = 4;
const SIGTRAP: i32 = 5;
const SIGBUS: i32 = 7;
const SIGFPE: i32 = 8;
const SIGSEGV: i32 = 11;
const SIGSYS: i32 = 31;
const SA_SIGINFO: u64 = 4;
const SA_RESTORER: u64 = 0x0400_0000;
const SA_ONSTACK: u64 = 0x0800_0000;
/// VM | FS | FILES | SIGHAND | THREAD | SYSVSEM | CHILD_CLEARTID
const CLONE_THREAD_FLAGS: usize = 0x0025_0f00;
const PR_SET_NO_NEW_PRIVS: usize = 38;
const PR_PAC_SET_ENABLED_KEYS: usize = 60;
const SECCOMP_SET_MODE_FILTER: usize = 1;
const FPSIMD_MAGIC: u32 = 0x4650_8001;
const ESR_MAGIC: u32 = 0x4553_5201;
const SVE_MAGIC: u32 = 0x5356_4501;
const ENOSYS: i64 = -38;
const EINVAL: i64 = -22;

extern "C" {
    /// the blob's first byte = the region's base (stub.ld)
    static __xs_start: u8;
}

/// A system call with six arguments.
///
/// # Safety
/// The call's arguments must be valid for it.
#[inline(always)]
unsafe fn sys6(nr: usize, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) -> i64 {
    let r: i64;
    // SAFETY: the caller passes arguments valid for the call; svc clobbers only x0 (memory and flags are assumed
    // clobbered by asm! without options).
    unsafe {
        asm!("svc #0", in("x8") nr, inlateout("x0") a => r, in("x1") b, in("x2") c, in("x3") d, in("x4") e,
             in("x5") f, options(nostack));
    }
    r
}

/// `sys(nr, args...)`: the missing arguments are 0.
macro_rules! sys {
    ($nr:expr $(, $a:expr)* $(,)?) => {{
        let args = [$($a as usize,)* 0, 0, 0, 0, 0, 0];
        // SAFETY: each call site passes the arguments its system call needs, pointers into the stub's region or the
        // task's shared pages.
        #[allow(unused_unsafe)]
        let r = unsafe { sys6($nr, args[0], args[1], args[2], args[3], args[4], args[5]) };
        r
    }};
}

/// A copy the compiler does not turn into a memcpy call (the blob has none): by 8-byte words when both ends and the
/// length allow it (every register block of a trap: x0-x30 and V0-V31 of the signal frame and the trap page, the esr),
/// else by bytes. Each trap copies about 1.5 KiB in and out; byte by byte (stub.c's copy, the Rust port's first
/// version) that was some 9000 instructions per trap, about 60% of the handler's (2026-11-23).
///
/// # Safety
/// `from` and `to` are valid for `n` bytes.
unsafe fn copy(to: *mut u8, from: *const u8, n: usize) {
    if (to as usize | from as usize | n) & 7 == 0 {
        let (to, from) = (to as *mut u64, from as *const u64);
        for i in 0..n / 8 {
            // SAFETY: the caller's ranges, 8-byte aligned (checked above).
            unsafe { write_volatile(to.add(i), read_volatile(from.add(i))) };
        }
        return;
    }
    for i in 0..n {
        // SAFETY: the caller's ranges.
        unsafe { write_volatile(to.add(i), read_volatile(from.add(i))) };
    }
}

/// The futex word at `p` as an atomic.
///
/// # Safety
/// `p` is a valid, aligned u32 that lives as long as the stub uses it (a shared page).
unsafe fn word<'a>(p: *mut u32) -> &'a AtomicU32 {
    // SAFETY: the caller's word; the other side uses it atomically too.
    unsafe { AtomicU32::from_ptr(p) }
}

fn futex_wait(w: &AtomicU32) {
    sys!(NR_FUTEX, w.as_ptr(), FUTEX_WAIT, 0, 0);
}

fn futex_wake(w: &AtomicU32) {
    sys!(NR_FUTEX, w.as_ptr(), FUTEX_WAKE, 0x7fff_ffff);
}

fn wait_for(w: &AtomicU32) {
    while w.load(Ordering::Acquire) == 0 {
        futex_wait(w);
    }
}

fn wake(w: &AtomicU32) {
    w.store(1, Ordering::Release);
    futex_wake(w);
}

fn counter() -> u64 {
    let v: u64;
    // SAFETY: reads the virtual counter, which EL0 may read.
    unsafe { asm!("isb", "mrs {}, cntvct_el0", out(reg) v, options(nostack)) };
    v
}

/// xnuhost-abi's stub.rs, the hand-off: store `event` on the futex word, then wake the other side only if it said it sleeps.
fn post(w: &AtomicU32, sleeping: &AtomicU32, event: u32) {
    w.store(event, Ordering::Release);
    fence(Ordering::SeqCst);
    if sleeping.load(Ordering::Relaxed) != 0 {
        futex_wake(w);
    }
}

/// Spins up to `ticks` for the word to become non-zero, then sleeps for it. Returns 1 if it slept.
fn await_word(w: &AtomicU32, sleeping: &AtomicU32, ticks: u64) -> u32 {
    if w.load(Ordering::Acquire) != 0 {
        return 0;
    }
    if ticks != 0 {
        let began = counter();
        while w.load(Ordering::Acquire) == 0 {
            if counter().wrapping_sub(began) > ticks {
                break;
            }
            // SAFETY: a hint instruction.
            unsafe { asm!("yield", options(nomem, nostack, preserves_flags)) };
        }
        if w.load(Ordering::Acquire) != 0 {
            return 0;
        }
    }
    sleeping.store(1, Ordering::Relaxed);
    fence(Ordering::SeqCst);
    while w.load(Ordering::Acquire) == 0 {
        futex_wait(w);
    }
    sleeping.store(0, Ordering::Relaxed);
    1
}

// ---- the signal frame, as the arm64 kernel lays it out ------------------------------------------
#[repr(C)]
#[allow(dead_code)] // the kernel's layout, read in part
struct KSiginfo {
    signo: i32,
    err: i32,
    code: i32,
    pad: i32,
    addr: u64,
    syscall: u32,
    arch: u32,
}

#[repr(C, align(16))]
#[allow(dead_code)]
struct Reserved([u8; 4096]);

#[repr(C)]
#[allow(dead_code)]
struct KSigcontext {
    fault_address: u64,
    regs: [u64; 31],
    sp: u64,
    pc: u64,
    pstate: u64,
    reserved: Reserved,
}

#[repr(C)]
#[allow(dead_code)]
struct KUcontext {
    flags: u64,
    link: u64,
    ss_sp: u64,
    ss_flags: i32,
    pad: i32,
    ss_size: u64,
    sigmask: [u8; 128],
    mc: KSigcontext,
}

/// The record of `magic` in the signal context's reserved area, or null.
///
/// # Safety
/// `mc` is the kernel's signal context.
unsafe fn find_record(mc: *mut KSigcontext, magic: u32) -> *mut u8 {
    // SAFETY: inside the context's reserved area, whose records the kernel wrote (head: u32 magic, u32 size).
    unsafe {
        let base = addr_of_mut!((*mc).reserved) as *mut u8;
        let mut at = 0usize;
        while at + 8 <= 4096 {
            let r = base.add(at) as *const u32;
            let (m, size) = (r.read(), r.add(1).read());
            if m == magic {
                return base.add(at);
            }
            if m == 0 || size < 8 {
                break;
            }
            at += size as usize;
        }
    }
    core::ptr::null_mut()
}

// xs_restorer: the signal return.
global_asm!(
    ".section .text.xs_restorer,\"ax\"",
    ".globl xs_restorer",
    ".type xs_restorer,%function",
    "xs_restorer:",
    "  mov x8, #139",
    "  svc #0",
);

extern "C" {
    fn xs_restorer();
    fn xs_clone(flags: usize, stack: usize, tid: *mut u32, tp: *mut XsTrapPage) -> i64;
}

fn config() -> *mut XsConfig {
    // SAFETY: the region's base plus the config's offset, inside the stub's region.
    unsafe { (addr_of!(__xs_start) as *mut u8).add(XS_CONFIG_OFF as usize) as *mut XsConfig }
}

/// Every signal of an iOS thread. It runs on the thread's own signal stack, which is inside its area.
///
/// # Safety
/// Called by the kernel as an SA_SIGINFO handler.
#[no_mangle]
unsafe extern "C" fn xs_handler(signo: i32, info: *mut KSiginfo, uc: *mut KUcontext) {
    let sp: usize;
    // SAFETY: reads the stack pointer.
    unsafe { asm!("mov {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags)) };
    let tp = (sp & !(XS_AREA_SIZE as usize - 1)) as *mut XsTrapPage;
    let config = config();
    // SAFETY: tp is the thread's trap page (its area's first page, shared with xnuhost), uc the kernel's frame.
    unsafe {
        let mc = addr_of_mut!((*uc).mc);
        // Fast-path MACH_ARM_TRAP_ABSTIME (-3) and MACH_ARM_TRAP_CONTTIME (-4) when XNU runs with a scaled
        // virtual timebase (droidvm-timebase-hz=24000000 over a 19.2 MHz counter): USER_TIMEBASE_NONE sends every
        // userspace mach_absolute_time()/mach_continuous_time() through svc #0x80. Hosted XNU never sleeps
        // (cpu_base_timebase == 0, mach_absolutetime_asleep == 0), so returning timebase_from_counter(CNTVCT_EL0)
        // here matches handle_mach_absolute_time_trap/handle_mach_continuous_time_trap exactly without a vCPU
        // round-trip.
        if signo == SIGSYS && (*tp).booted != 0 {
            let num = (*config).timebase_num as u64;
            let den = (*config).timebase_den as u64;
            let x16 = (*mc).regs[16] as i64;
            if num != den
                && den != 0
                && (x16 == -3 || x16 == -4)
                && read_volatile((*mc).pc.wrapping_sub(4) as *const u32) == 0xd400_1001
            {
                let c = counter();
                (*mc).regs[0] = (c / den).wrapping_mul(num).wrapping_add((c % den).wrapping_mul(num) / den);
                return;
            }
        }
        let st = addr_of_mut!((*tp).state);
        let trap = addr_of_mut!((*tp).trap);

        (*tp).t_enter = counter();
        copy(addr_of_mut!((*st).x) as *mut u8, addr_of!((*mc).regs) as *const u8, 31 * 8);
        (*st).sp = (*mc).sp;
        (*st).pc = (*mc).pc;
        (*st).pstate = (*mc).pstate;
        asm!("mrs {}, tpidr_el0", out(reg)(*st).tsd, options(nomem, nostack, preserves_flags));
        let fp = find_record(mc, FPSIMD_MAGIC);
        if !fp.is_null() {
            copy(addr_of_mut!((*st).fpsr) as *mut u8, fp.add(8), 4);
            copy(addr_of_mut!((*st).fpcr) as *mut u8, fp.add(12), 4);
            copy(addr_of_mut!((*st).v) as *mut u8, fp.add(16), 32 * 16);
        }
        let esr = find_record(mc, ESR_MAGIC);
        (*trap).signo = signo as u32;
        (*trap).esr = 0;
        if !esr.is_null() {
            copy(addr_of_mut!((*trap).esr) as *mut u8, esr.add(8), 8);
        }
        (*trap).far = 0;
        (*trap).svc = 0;
        (*trap).access = 0;
        (*trap).insn = 0;
        (*trap).exit_status = 0;

        let pc = (*st).pc;
        let insn_at = |at: u64| read_volatile(at as *const u32);
        if (*tp).booted == 0 {
            (*tp).booted = 1;
            (*trap).kind = XH_TRAP_BOOT;
        } else if signo == SIGSYS {
            (*trap).kind = XH_TRAP_SYSCALL;
            (*trap).far = pc.wrapping_sub(4);
            (*trap).insn = insn_at(pc.wrapping_sub(4));
            if (*trap).insn & 0xffe0_001f == 0xd400_0001 {
                (*trap).svc = ((*trap).insn >> 5) & 0xffff;
            }
        } else if signo == SIGSEGV || signo == SIGBUS {
            (*trap).kind = XH_TRAP_FAULT;
            (*trap).far = (*info).addr;
            (*trap).access = fault_access((*trap).esr, (*info).addr, pc); // a cache maintenance fault (ISS.CM) is a read
        } else if signo == SIGILL {
            (*trap).kind = XH_TRAP_ILLEGAL;
            (*trap).far = pc;
            (*trap).insn = insn_at(pc);
        } else if signo == SIGTRAP {
            (*trap).kind = XH_TRAP_BRK;
            (*trap).far = pc;
            // Linux's SIGTRAP carries no syndrome of its own: its esr_context is the task's last fault's (cpu.rs says
            // why), which the kernel would report as that fault. The instruction gives the kernel the brk's immediate.
            (*trap).esr = 0;
            (*trap).insn = insn_at(pc);
        } else if signo == SIGFPE {
            (*trap).kind = XH_TRAP_FPE;
            (*trap).far = pc;
        } else if signo == (*config).kick_signal as i32 {
            (*trap).kind = XH_TRAP_INTERRUPT;
        } else {
            (*trap).kind = XH_TRAP_ILLEGAL;
        }

        let to_task = word(addr_of_mut!((*tp).to_task));
        let to_host = word(addr_of_mut!((*tp).to_host));
        let host_sleeping = word(addr_of_mut!((*tp).host_sleeping));
        to_task.store(0, Ordering::Release);
        if (*trap).kind == XH_TRAP_BOOT {
            wake(to_host); // thread_create waits for this one in a timed futex (XS_HOST_TRAPPED == 1)
        } else {
            post(to_host, host_sleeping, XS_HOST_TRAPPED);
        }
        (*tp).slept = await_word(to_task, word(addr_of_mut!((*tp).task_sleeping)), (*tp).spin_ticks);
        (*tp).t_woken = counter();
        if (*tp).command == XS_EXIT {
            sys!(NR_EXIT, 0);
        }

        // Maps, and unmaps given while none of the task's threads ran, that the kernel gave for this task since its
        // last order (task.rs, map-defer): carried out here, in their order, before the thread runs again, while
        // xnuhost holds the task's order lock; then XS_HOST_OPS_ACK is published on to_host so a host FUTEX_WAIT
        // cannot miss the wakeup.
        let nops = (*tp).nops;
        if nops != 0 {
            let ops = (tp as *mut u8).add(XS_OPS_OFF as usize) as *mut XsOp;
            for i in 0..nops.min(XS_THREAD_OPS) as usize {
                let op = ops.add(i);
                let r = match (*op).op {
                    XS_OP_MAP => sys!(
                        NR_MMAP,
                        (*op).va,
                        (*op).size,
                        (*op).prot & 7,
                        MAP_SHARED | MAP_FIXED | XS_MAP_POPULATE as usize,
                        (*config).ram_fd,
                        (*op).offset
                    ),
                    XS_OP_UNMAP => sys!(NR_MUNMAP, (*op).va, (*op).size),
                    _ => ENOSYS,
                };
                (*op).result = if r > 0 { 0 } else { r };
            }
            (*tp).nops = 0;
            word(addr_of_mut!((*tp).ops_done)).store(1, Ordering::Release);
            post(to_host, host_sleeping, XS_HOST_OPS_ACK);
        }

        if (*tp).pac_set != 0 {
            (*tp).pac_set = 0;
            sys!(NR_PRCTL, PR_PAC_SET_ENABLED_KEYS, 0xf, (*tp).pac_enabled & 0xf, 0, 0);
        }
        copy(addr_of_mut!((*mc).regs) as *mut u8, addr_of!((*st).x) as *const u8, 31 * 8);
        (*mc).sp = (*st).sp;
        (*mc).pc = (*st).pc;
        (*mc).pstate = (*st).pstate & 0xf000_0000; // NZCV; EL0t, interrupts unmasked
        if !fp.is_null() {
            copy(fp.add(8), addr_of!((*st).fpsr) as *const u8, 4);
            copy(fp.add(12), addr_of!((*st).fpcr) as *const u8, 4);
            copy(fp.add(16), addr_of!((*st).v) as *const u8, 32 * 16);
        }
        // A thread with live SVE state (none of iOS's code has any, but the host's may) is restored from the SVE
        // record instead: V[n] is the low 128 bits of Z[n]. Record: head, u16 vl, ..., then Z0..Z31.
        let sve = find_record(mc, SVE_MAGIC);
        if !sve.is_null() {
            let size = (sve.add(4) as *const u32).read() as usize;
            let vl = (sve.add(8) as *const u16).read() as usize;
            if vl >= 16 && size >= 16 + 32 * vl {
                for n in 0..32 {
                    copy(sve.add(16 + n * vl), (addr_of!((*st).v) as *const u8).add(n * 16), 16);
                }
            }
        }
        asm!("msr tpidr_el0, {}", in(reg) (*st).tsd, options(nomem, nostack, preserves_flags));
    }
}

// ---- threads ------------------------------------------------------------------------------------
/// A new thread starts here on its own signal stack: it registers that stack and stops with a breakpoint, which the
/// handler reports as XH_TRAP_BOOT. It never comes back from the handler: xnuhost replaces its whole register state.
///
/// # Safety
/// `tp` is the new thread's trap page, mapped by new_thread.
#[no_mangle]
unsafe extern "C" fn xs_thread_start(tp: *mut XsTrapPage) -> ! {
    #[repr(C)]
    struct Stack {
        sp: u64,
        flags: i32,
        pad: i32,
        size: u64,
    }
    let stack = Stack { sp: tp as u64 + XS_SIGSTACK_OFF, flags: 0, pad: 0, size: XS_SIGSTACK_SIZE };
    sys!(NR_SIGALTSTACK, &stack as *const Stack, 0);
    // SAFETY: the trap page's tid word.
    unsafe { word(addr_of_mut!((*tp).tid)) }.store(sys!(NR_GETTID) as u32, Ordering::Release);
    // SAFETY: a breakpoint: the handler reports it as the thread's first stop.
    unsafe { asm!("brk #0xf00", options(nostack)) };
    loop {
        sys!(NR_EXIT, 1);
    }
}

// xs_clone(flags, stack, tid, tp): clone(2); the child runs xs_thread_start(tp).
global_asm!(
    ".section .text.xs_clone,\"ax\"",
    ".globl xs_clone",
    ".type xs_clone,%function",
    "xs_clone:",
    "  mov x9, x3", // the child's argument survives in a register
    "  mov x4, x2", // ctid (arm64 order: flags, stack, ptid, tls, ctid)
    "  mov x3, #0",
    "  mov x8, #220",
    "  svc #0",
    "  cbz x0, 1f",
    "  ret",
    "1:",
    "  mov x29, #0",
    "  mov x30, #0",
    "  mov x0, x9",
    "  b xs_thread_start",
);

fn new_thread(config: *mut XsConfig, index: u64, offset: u64) -> i64 {
    if index >= XS_MAX_THREADS {
        return EINVAL;
    }
    // SAFETY: the config page is the stub's own.
    let (base, trap_fd) = unsafe { ((*config).base, (*config).trap_fd) };
    let area = (base + XS_THREADS_OFF + index * XS_AREA_SIZE) as usize;
    let r = sys!(NR_MMAP, area, XS_SLOT_SIZE, PROT_RW, MAP_SHARED | MAP_FIXED, trap_fd, offset);
    if r < 0 {
        return r;
    }
    let r = sys!(
        NR_MMAP,
        area + XS_SIGSTACK_OFF as usize,
        XS_SIGSTACK_SIZE,
        PROT_RW,
        MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
        -1i64,
        0
    );
    if r < 0 {
        return r;
    }
    let tp = area as *mut XsTrapPage;
    let stack = area + (XS_SIGSTACK_OFF + XS_SIGSTACK_SIZE) as usize;
    // SAFETY: the area was just mapped: the trap page and the signal stack the child starts on.
    let r = unsafe { xs_clone(CLONE_THREAD_FLAGS, stack, addr_of_mut!((*tp).tid), tp) };
    if r < 0 {
        r
    } else {
        0
    }
}

// ---- the service thread -------------------------------------------------------------------------
fn service(config: *mut XsConfig, sp: *mut XsServicePage) -> ! {
    // SAFETY: sp is the task's service page (mapped by xs_main), config the stub's own page.
    unsafe {
        let request = word(addr_of_mut!((*sp).request));
        loop {
            wait_for(request);
            let ops = addr_of_mut!((*sp).ops) as *mut XsOp;
            for i in 0..((*sp).count as usize).min(XS_OPS_MAX) {
                let op = ops.add(i);
                match (*op).op {
                    XS_OP_MAP => {
                        (*op).result = sys!(
                            NR_MMAP,
                            (*op).va,
                            (*op).size,
                            (*op).prot & 7,
                            MAP_SHARED | MAP_FIXED | XS_MAP_POPULATE as usize,
                            (*config).ram_fd,
                            (*op).offset
                        );
                        if (*op).result > 0 {
                            (*op).result = 0;
                        }
                    }
                    XS_OP_UNMAP => (*op).result = sys!(NR_MUNMAP, (*op).va, (*op).size),
                    XS_OP_PROTECT => (*op).result = sys!(NR_MPROTECT, (*op).va, (*op).size, (*op).prot & 7),
                    XS_OP_CLONE => (*op).result = new_thread(config, (*op).va, (*op).offset),
                    XS_OP_EXIT => {
                        sys!(NR_EXIT_GROUP, 0);
                    }
                    _ => (*op).result = ENOSYS,
                }
            }
            request.store(0, Ordering::Release);
            wake(word(addr_of_mut!((*sp).done)));
        }
    }
}

/// The service thread's start (xs_entry's): the task's address space emptied, the handlers, the identity and the
/// seccomp filter set up, then the orders.
#[no_mangle]
extern "C" fn xs_main() -> ! {
    let config = config();
    // SAFETY: the config page, which xnuhost wrote before the fork.
    let (base, trap_fd, service_slot, kick, uid, filter_length) = unsafe {
        let c = &*config;
        (c.base, c.trap_fd, c.service_slot, c.kick_signal, c.uid, c.filter_length)
    };
    let end = base + XS_REGION_SIZE;

    // Everything the child inherited from the factory goes: the task's address space is XNU's to fill.
    sys!(NR_MUNMAP, 0, base);
    for bits in [52u32, 48, 47, 42, 39, 36] {
        let top = 1u64 << bits;
        if top > end && sys!(NR_MUNMAP, end, top - end) == 0 {
            break;
        }
    }

    let sp = (base + XS_SERVICE_OFF) as *mut XsServicePage;
    let mut r =
        sys!(NR_MMAP, sp, XS_SLOT_SIZE, PROT_RW, MAP_SHARED | MAP_FIXED, trap_fd, service_slot as u64 * XS_SLOT_SIZE);
    if r < 0 {
        sys!(NR_EXIT_GROUP, 70);
    }

    #[repr(C)]
    struct Action {
        handler: usize,
        flags: u64,
        restorer: usize,
        mask: u64,
    }
    let action = Action {
        handler: xs_handler as *const () as usize,
        flags: SA_SIGINFO | SA_ONSTACK | SA_RESTORER,
        restorer: xs_restorer as *const () as usize,
        mask: !0, // no nesting: a second signal waits for sigreturn
    };
    for signal in [SIGSYS, SIGSEGV, SIGBUS, SIGILL, SIGTRAP, SIGFPE, kick as i32] {
        if r < 0 {
            break;
        }
        r = sys!(NR_RT_SIGACTION, signal, &action as *const Action, 0, 8);
    }
    if r >= 0 && uid >= 0 {
        r = sys!(NR_SETRESGID, uid, uid, uid);
        if r >= 0 {
            r = sys!(NR_SETRESUID, uid, uid, uid);
        }
    }
    if r >= 0 && filter_length != 0 {
        #[repr(C)]
        struct Program {
            length: u16,
            filter: *const XsFilterInsn,
        }
        // SAFETY: the address of the filter, which lives in the config page.
        let filter = unsafe { addr_of!((*config).filter) } as *const XsFilterInsn;
        let program = Program { length: filter_length as u16, filter };
        r = sys!(NR_PRCTL, PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        if r >= 0 {
            r = sys!(NR_SECCOMP, SECCOMP_SET_MODE_FILTER, 0, &program as *const Program);
        }
    }
    // SAFETY: the service page was just mapped.
    unsafe {
        (*sp).pid = sys!(NR_GETPID) as i32;
        (*sp).setup_error = if r < 0 { r as i32 } else { 0 };
        wake(word(addr_of_mut!((*sp).ready)));
    }
    if r < 0 {
        sys!(NR_EXIT_GROUP, 71);
    }
    service(config, sp)
}

// The blob's first instruction: move to the service thread's stack and go.
global_asm!(
    ".section .text.entry,\"ax\"",
    ".globl xs_entry",
    ".type xs_entry,%function",
    "xs_entry:",
    "  adr x9, xs_entry", // = the region's base
    "  mov x10, #{stack_top}",
    "  add sp, x9, x10",
    "  mov x29, #0",
    "  mov x30, #0",
    "  b xs_main",
    stack_top = const XS_BOOTSTACK_OFF + XS_BOOTSTACK_SIZE,
);

/// No code path of the stub panics (build.sh checks the blob has none); a panic would end the task.
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        sys!(NR_EXIT_GROUP, 72);
    }
}
