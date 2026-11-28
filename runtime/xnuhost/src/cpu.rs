// SPDX-License-Identifier: MIT
//! Port of cpu.c. vCPUs: one host thread per CPU of the guest, running the kernel's code natively at EL0.
//! The role of ArmCpu (https://github.com/UniXNU/ArmCpu 1604083) seen from the other side: where ArmCpu starts cores
//! with PSCI, takes the timer on INTID 27 and sends SGIs, this module is the PSCI call's effect (a thread), the timer
//! (a POSIX timer that signals the thread) and the SGI (a signal or a futex wake). Interrupt delivery is in
//! docs/booter.md, "Interrupts".
//!
//! The state the kernel, the signal handlers and other threads share is atomics: a [`Vcpu`] is `Sync`, a load or
//! store of one compiles to the plain load or store the C made. Counters a vCPU keeps for itself are [`Counter`]s
//! (one writer, no locked add).
//!
//! The calling vCPU ([`current`]) is found from the stack pointer, not from a thread-local: host code of a vCPU runs
//! only on its host stack (host calls) or its signal stack, and on aarch64-linux-android Rust's thread-locals go
//! through pthread keys.

use crate::perf::{Counter, Perf};
use crate::sysreg::SysregState;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering::*};
use xnuhost_abi::{XhCpu, XhFrame, XhUpcalls};

pub use crate::config::MAX_CPUS;

static TIMEBASE_NUM: AtomicU32 = AtomicU32::new(1);
static TIMEBASE_DEN: AtomicU32 = AtomicU32::new(1);

/// Records the guest virtual timebase scale `(num, den)` (`droidvm-timebase-hz / CNTFRQ_EL0`).
pub fn set_timebase_scale(num: u32, den: u32) {
    let (n, d) = if num != 0 && den != 0 { (num, den) } else { (1, 1) };
    TIMEBASE_NUM.store(n, Relaxed);
    TIMEBASE_DEN.store(d, Relaxed);
}

/// Active virtual timebase scale `(num, den)`.
pub fn timebase_scale() -> (u32, u32) {
    (TIMEBASE_NUM.load(Relaxed), TIMEBASE_DEN.load(Relaxed))
}

/// Scales a raw host counter value into guest `mach_absolute_time` ticks (matching `timebase_from_counter`).
#[inline]
pub fn scale_counter(c: u64, num: u32, den: u32) -> u64 {
    if num == den || den == 0 {
        return c;
    }
    let (num, den) = (u64::from(num), u64::from(den));
    (c / den).wrapping_mul(num).wrapping_add((c % den).wrapping_mul(num) / den)
}

/// Current host counter scaled to the guest's `mach_absolute_time` rate (for HID scan timestamps).
#[inline]
pub fn guest_counter() -> u64 {
    let (num, den) = timebase_scale();
    scale_counter(counter(), num, den)
}

/// Effective guest timebase frequency in Hz for a host counter frequency `host_hz`.
pub fn guest_timebase_hz(host_hz: u64) -> u64 {
    let (num, den) = timebase_scale();
    scale_counter(host_hz, num, den)
}

/// `struct xh_cpu` with every field an atomic: the kernel, the signal handlers and other vCPUs read and write it at
/// once. The same layout as [`XhCpu`] (checked below); the kernel gets a `*mut XhCpu` to it.
#[repr(C)]
#[derive(Debug, Default)]
pub struct CpuBlock {
    pub index: AtomicU32,
    /// K: 1 = interrupts masked (the kernel's DAIF.I)
    pub irq_masked: AtomicU32,
    /// not 0 = the interrupt line is asserted (read by K)
    pub irq_pending: AtomicU32,
    pub in_host: AtomicU32,
    pub kernel_tpidr: AtomicU64,
    pub kernel_x18: AtomicU64,
    pub host_tpidr: AtomicU64,
    pub host_x18: AtomicU64,
    pub host_sp: AtomicU64,
    pub kernel_data: AtomicU64,
    /// K: shadow of TPIDR_EL1 for trapped mrs/msr (current thread)
    pub tpidr_el1: AtomicU64,
    /// K: what the user thread this vCPU runs needs (XH_HINT_*), written by the kernel before thread_run (0075)
    pub perf_hint: AtomicU32,
    perf_reserved: AtomicU32,
    reserved: [AtomicU64; 6],
}

macro_rules! same_offsets {
    ($($f:ident),*) => { $(const _: () = assert!(std::mem::offset_of!(CpuBlock, $f) == std::mem::offset_of!(XhCpu, $f));)* };
}
same_offsets!(
    index,
    irq_masked,
    irq_pending,
    in_host,
    kernel_tpidr,
    kernel_x18,
    host_tpidr,
    host_x18,
    host_sp,
    kernel_data,
    tpidr_el1,
    perf_hint,
    perf_reserved,
    reserved
);
const _: () = assert!(std::mem::size_of::<CpuBlock>() == std::mem::size_of::<XhCpu>());

impl CpuBlock {
    const fn new() -> CpuBlock {
        CpuBlock {
            index: AtomicU32::new(0),
            irq_masked: AtomicU32::new(0),
            irq_pending: AtomicU32::new(0),
            in_host: AtomicU32::new(0),
            kernel_tpidr: AtomicU64::new(0),
            kernel_x18: AtomicU64::new(0),
            host_tpidr: AtomicU64::new(0),
            host_x18: AtomicU64::new(0),
            host_sp: AtomicU64::new(0),
            kernel_data: AtomicU64::new(0),
            tpidr_el1: AtomicU64::new(0),
            perf_hint: AtomicU32::new(0),
            perf_reserved: AtomicU32::new(0),
            reserved: [const { AtomicU64::new(0) }; 6],
        }
    }

    /// The pointer the kernel and the host calls pass.
    pub fn as_ptr(&self) -> *mut XhCpu {
        self as *const CpuBlock as *mut XhCpu
    }

    pub fn index(&self) -> usize {
        self.index.load(Relaxed) as usize
    }
}

/// How often a vCPU trapped into the host, by cause (the `cpu` lines of the log and SIGQUIT's dump).
#[derive(Debug, Default)]
pub struct CpuStats {
    pub irqs: Counter,
    pub mmio: Counter,
    pub sysreg: Counter,
    pub traps: Counter,
    /// interrupts acknowledged (any thread may add: an atomic add)
    pub acks: AtomicU64,
    /// the count of acks when the last interrupt upcall began
    pub acks_mark: Counter,
    /// frame_resume re-raises; INTID 27 and SGIs acknowledged
    pub reraised: Counter,
    pub timer: Counter,
    pub sgi: Counter,
}

impl CpuStats {
    const fn new() -> CpuStats {
        CpuStats {
            irqs: Counter::new(),
            mmio: Counter::new(),
            sysreg: Counter::new(),
            traps: Counter::new(),
            acks: AtomicU64::new(0),
            acks_mark: Counter::new(),
            reraised: Counter::new(),
            timer: Counter::new(),
            sgi: Counter::new(),
        }
    }
}

/// `struct xh_vcpu`: the ABI block first, host-private data after it.
#[repr(C)]
#[derive(Debug)]
pub struct Vcpu {
    pub cpu: CpuBlock,
    thread: AtomicUsize,
    pub tid: AtomicI32,
    pub started: AtomicBool,
    entry: AtomicU64,
    args: [AtomicU64; 3],
    boot_stack: AtomicUsize,
    host_stack: AtomicUsize,
    sig_stack: AtomicUsize,
    timer: AtomicI32,
    pub timer_armed: AtomicBool,
    pub timer_deadline: AtomicU64,
    /// timer-coalesce: the deadline the host timer was last armed for, 0 when it is not armed or has fired (its signal
    /// clears it first). Written on the vCPU's own thread only (timer_set and the timer signal).
    pub timer_host_at: AtomicU64,
    /// 1 while in cpu_idle
    pub idle: AtomicU32,
    pub sys: SysregState,
    pub stat: CpuStats,
    /// The iOS thread this vCPU has released and waits for in thread_run, for the direct kick (task::kick):
    /// `run_word` is (release number << 1) | 1 once a kick was sent for that release, 0 while no thread is released;
    /// `run_pid` and `run_tid` are written before the word. `run_kicked_at`: the counter when a raiser kicked (perf).
    /// `run_seq`: the release numbers (the vCPU's own thread only).
    pub run_word: AtomicU64,
    pub run_pid: AtomicI32,
    pub run_tid: AtomicU32,
    pub run_seq: AtomicU64,
    pub run_kicked_at: AtomicU64,
    /// The uclamp.min this vCPU's own thread carries ([`own_uclamp`], set on that thread only; 0 = Linux's default).
    pub own_uclamp: AtomicU32,
    /// b'E' or b'P' as the CPU plan made it (0: no plan), for the perf lines.
    pub kind: AtomicU8,
    pub perf: Perf,
}

impl Vcpu {
    const fn new() -> Vcpu {
        Vcpu {
            cpu: CpuBlock::new(),
            thread: AtomicUsize::new(0),
            tid: AtomicI32::new(0),
            started: AtomicBool::new(false),
            entry: AtomicU64::new(0),
            args: [const { AtomicU64::new(0) }; 3],
            boot_stack: AtomicUsize::new(0),
            host_stack: AtomicUsize::new(0),
            sig_stack: AtomicUsize::new(0),
            timer: AtomicI32::new(-1),
            timer_armed: AtomicBool::new(false),
            timer_deadline: AtomicU64::new(0),
            timer_host_at: AtomicU64::new(0),
            idle: AtomicU32::new(0),
            sys: SysregState::new(),
            stat: CpuStats::new(),
            run_word: AtomicU64::new(0),
            run_pid: AtomicI32::new(0),
            run_tid: AtomicU32::new(0),
            run_seq: AtomicU64::new(0),
            run_kicked_at: AtomicU64::new(0),
            own_uclamp: AtomicU32::new(0),
            kind: AtomicU8::new(0),
            perf: Perf::new(),
        }
    }

    pub fn index(&self) -> usize {
        self.cpu.index()
    }

    /// The vCPU's pthread (0 before it started), for its CPU clock (perf.rs).
    pub fn pthread(&self) -> usize {
        self.thread.load(Acquire)
    }

    /// The vCPU of a `struct xh_cpu *` the kernel passed.
    ///
    /// # Safety
    /// `cpu` must be the block of one of [`VCPUS`] (the kernel only has those).
    pub unsafe fn from_cpu<'a>(cpu: *const XhCpu) -> &'a Vcpu {
        // SAFETY: CpuBlock is Vcpu's first field (repr(C)) and the caller passes one of VCPUS.
        unsafe { &*(cpu as *const Vcpu) }
    }
}

static VCPUS: [Vcpu; MAX_CPUS] = [const { Vcpu::new() }; MAX_CPUS];
static NCPU: AtomicUsize = AtomicUsize::new(0);

/// vCPU `index` (any of the MAX_CPUS blocks; only the first [`ncpu`] are in use).
pub fn vcpu(index: usize) -> &'static Vcpu {
    &VCPUS[index]
}

pub fn ncpu() -> usize {
    NCPU.load(Relaxed)
}

/// The vCPUs in use.
pub fn vcpus() -> &'static [Vcpu] {
    &VCPUS[..ncpu()]
}

// ---- the kernel's upcalls (set by the set_upcalls host call) ------------------------------------------------------
type IrqFn = unsafe extern "C" fn(*mut XhCpu, *mut XhFrame);
type EntryFn = unsafe extern "C" fn(*mut XhCpu, u64);

/// `g_upcalls`, `g_upcalls_set`: the kernel's entry points, as numbers (0 = none) so that the signal handlers and
/// kernel-context code read them with one load.
#[derive(Debug)]
pub struct Upcalls {
    irq: AtomicUsize,
    exception: AtomicUsize,
    cpu_entry: AtomicUsize,
    set: AtomicBool,
}

pub static UPCALLS: Upcalls = Upcalls {
    irq: AtomicUsize::new(0),
    exception: AtomicUsize::new(0),
    cpu_entry: AtomicUsize::new(0),
    set: AtomicBool::new(false),
};

fn fn_bits<F: Copy>(f: Option<F>) -> usize {
    const { assert!(std::mem::size_of::<Option<F>>() == std::mem::size_of::<usize>()) };
    // SAFETY: Option of a function pointer is one pointer-sized word, 0 for None (asserted above).
    unsafe { std::mem::transmute_copy::<Option<F>, usize>(&f) }
}

fn bits_fn<F: Copy>(bits: usize) -> Option<F> {
    // SAFETY: `bits` was made by fn_bits from an Option<F> of the same F (the accessors below pair them).
    unsafe { std::mem::transmute_copy::<usize, Option<F>>(&bits) }
}

impl Upcalls {
    pub fn install(&self, u: &XhUpcalls) {
        self.irq.store(fn_bits(u.irq), Release);
        self.exception.store(fn_bits(u.exception), Release);
        self.cpu_entry.store(fn_bits(u.cpu_entry), Release);
        self.set.store(true, Release);
    }
    pub fn irq(&self) -> Option<IrqFn> {
        bits_fn(self.irq.load(Acquire))
    }
    pub fn exception(&self) -> Option<IrqFn> {
        bits_fn(self.exception.load(Acquire))
    }
    pub fn cpu_entry(&self) -> Option<EntryFn> {
        bits_fn(self.cpu_entry.load(Acquire))
    }
    pub fn is_set(&self) -> bool {
        self.set.load(Acquire)
    }
}

// ---- hooks main sets before the vCPUs start ----------------------------------------------------------------------
/// Puts a Linux thread (0 = the caller) on the host CPUs of a vCPU. Called by each vCPU thread for itself and for
/// the iOS thread a vCPU runs, when that vCPU changes.
pub type PlaceFn = fn(tid: i32, vcpu: usize);
/// The guest's cpufreq request for a vCPU as a utilisation clamp (uclamp.min, 0-1024; DroidVMCpufreq's SET_PERF).
/// thread_run gives the iOS thread it releases [`release_floor`] of it: the vCPU thread itself waits while user code
/// runs, so a clamp on it alone never reached iOS's own code (host-overhead, 2026-11-15).
pub type UclampFn = fn(vcpu: usize) -> u32;

static PLACE: AtomicUsize = AtomicUsize::new(0);
static UCLAMP: AtomicUsize = AtomicUsize::new(0);
/// drm2kgsl's vCPU clamp while a constrained queue animates (its patch 0008 calls drm2kgsl_vcpu_uclamp): 0 = none.
/// Only [`release_floor`] reads it: the frame's threads get it at their release, nothing else (2026-11-23).
pub static GPU_VCPU_BOOST: AtomicU32 = AtomicU32::new(0);

/// The uclamp.min an iOS thread gets at its release, and its vCPU's thread with it (the kernel's work after the
/// thread's trap is done for it): `hint` the kernel's perf_hint (0075), `request` its vCPU's request ([`UclampFn`]),
/// `boost` drm2kgsl's animation clamp ([`GPU_VCPU_BOOST`]).
/// - no hint (a kernel before 0075): the request or the boost, the higher;
/// - utility, background, darwin-background: 0, and no ceiling (the owner's rule; a ceiling of 512 slowed chronod's
///   widget extensions until 2026-11-16, #57);
/// - else the class's floor (realtime, UI, above-UI 1024, user-initiated 768) or the request, the higher, raised to the
///   boost only when that is not 0: the frame's threads (their class, or an interactive group's default-class thread,
///   for which the guest asks) get the animation's clamp; a daemon's default-class thread, which neither marks, does not.
///
/// Until 2026-11-23 the boost went to every vCPU thread directly (drm2kgsl_vcpu_uclamp) and to every iOS thread that
/// was not background: the session of 2026-11-23 20:23 carried `vCPU uclamp.min 1024` for 470 of its 651 s, with the
/// eight vCPUs 34-58% of a core each, the host 637-760% of 800 and the SoC at 94-98 C, while lsd, PerfPowerServices,
/// chronod and CommCenter (default class) were the busiest processes.
pub fn release_floor(hint: u32, request: u32, boost: u32) -> u32 {
    if UCLAMP_ALL.load(Relaxed) {
        return release_floor_all(hint, request, boost);
    }
    release_floor_work(hint, request, boost)
}

/// uclamp-rule=all (docs/switches.md, session.rs sets it once at the start): the rule before 03d85ac. drm2kgsl's boost
/// goes on every vCPU thread ([`drm2kgsl_vcpu_uclamp`]) and on every iOS thread that is not background; the cpufreq
/// device sets the vCPU thread's clamp itself; a vCPU thread keeps its clamp while idle ([`own_uclamp`] does nothing).
pub static UCLAMP_ALL: AtomicBool = AtomicBool::new(false);

/// timer-coalesce (fix/unmap-fanout, 2026-11-24; docs/switches.md; session.rs sets it once at the start). The phone
/// (2026-11-24 01:17) made 497,376 timer_set calls per 30 s, 495,092 of them a timer_settime: XNU sets its next
/// deadline at nearly every dispatch (the new thread's quantum), each later than the one before. On: a deadline later
/// than the one the host timer is armed for, and a cancel, leave the host timer as it is; its signal then finds the
/// deadline not reached (or nothing armed) and arms the host timer for the deadline instead of raising the line (one
/// timer_settime per early signal, no interrupt). An earlier deadline arms the host timer at once, as before. Off: every
/// new deadline and every cancel is a timer_settime, and every timer signal raises the line (before).
pub static TIMER_COALESCE: AtomicBool = AtomicBool::new(true);

/// The release floor of uclamp-rule=all (before 03d85ac): the request or the boost, the higher; a background thread
/// 0; a UI or realtime thread at least its class's floor.
pub fn release_floor_all(hint: u32, request: u32, boost: u32) -> u32 {
    use xnuhost_abi::{xh_hint_floor, XH_HINT_BACKGROUND, XH_HINT_VALID};
    let mut lo = request.max(boost);
    if hint & XH_HINT_VALID != 0 {
        if hint & XH_HINT_BACKGROUND != 0 {
            lo = 0;
        } else {
            lo = lo.max(xh_hint_floor(hint));
        }
    }
    lo.min(1024)
}

/// The release floor of uclamp-rule=work (the default, 03d85ac): see [`release_floor`].
pub fn release_floor_work(hint: u32, request: u32, boost: u32) -> u32 {
    use xnuhost_abi::{xh_hint_floor, XH_HINT_BACKGROUND, XH_HINT_VALID};
    let max = |a: u32, b: u32| if a > b { a } else { b };
    let lo = if hint & XH_HINT_VALID == 0 {
        max(request, boost)
    } else if hint & XH_HINT_BACKGROUND != 0 {
        0
    } else {
        let base = max(request, xh_hint_floor(hint));
        if base == 0 {
            0
        } else {
            max(base, boost)
        }
    };
    if lo > 1024 {
        1024
    } else {
        lo
    }
}

/// The hint's class for the perf report's `qos:` line: 0 realtime, 1 UI (UI, above-UI, user-initiated), 2 default (no
/// class of the kernel's: default, kernel, a kernel before 0075), 3 background (utility, background, darwin-background).
pub const fn hint_class(hint: u32) -> usize {
    use xnuhost_abi::{XH_HINT_BACKGROUND, XH_HINT_REALTIME, XH_HINT_UI, XH_HINT_VALID};
    if hint & XH_HINT_VALID == 0 {
        2
    } else if hint & XH_HINT_REALTIME != 0 {
        0
    } else if hint & XH_HINT_UI != 0 {
        1
    } else if hint & XH_HINT_BACKGROUND != 0 {
        3
    } else {
        2
    }
}

/// Sets the uclamp.min of vCPU `v`'s own thread (call it on that thread) when it differs from what the thread carries:
/// the floor of the work it does now. thread_run gives it the floor of the iOS thread it releases, [`idle`] 0, the guest's
/// cpufreq request its value. One sched_setattr per change, none otherwise (perf: `vCPU threads N`).
pub fn own_uclamp(v: &Vcpu, lo: u32) {
    if UCLAMP_ALL.load(Relaxed) {
        return; // uclamp-rule=all: the cpufreq device and drm2kgsl set the vCPU thread's clamp, as before 03d85ac
    }
    let lo = lo.min(1024);
    if v.own_uclamp.load(Relaxed) == lo {
        return;
    }
    // KEEP_POLICY | KEEP_PARAMS | UTIL_CLAMP_MIN: the policy stays SCHED_OTHER. A host without uclamp (ENOSYS,
    // EINVAL) is not asked again for the same value.
    if crate::task::sched_setattr_uclamp(0, 0x18 | 0x20, lo, 0) == 0 || xh_core::sys::errno() != libc::EINTR {
        v.own_uclamp.store(lo, Relaxed);
    }
    v.perf.count(crate::perf::Count::VcpuUclampSets, 1);
}

pub fn set_place(f: Option<PlaceFn>) {
    PLACE.store(fn_bits(f), Release);
}
pub fn place_hook() -> Option<PlaceFn> {
    bits_fn(PLACE.load(Acquire))
}
pub fn set_uclamp(f: Option<UclampFn>) {
    UCLAMP.store(fn_bits(f), Release);
}
pub fn uclamp_hook() -> Option<UclampFn> {
    bits_fn(UCLAMP.load(Acquire))
}

/// Why [`start`] did not start a vCPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    /// No such vCPU, no entry and no cpu_entry upcall, or no thread.
    Invalid,
    /// It is on already.
    Already,
}

/// The level of a CPU's virtual timer line (INTID 27), for the GIC.
pub fn timer_due(index: usize) -> bool {
    index < ncpu() && {
        let v = &VCPUS[index];
        v.timer_armed.load(Acquire) && counter() >= v.timer_deadline.load(Acquire)
    }
}

/// Counter ticks to nanoseconds without overflow for any realistic count.
pub fn ticks_to_ns(ticks: u64, hz: u64) -> u64 {
    ticks / hz * 1_000_000_000 + ticks % hz * 1_000_000_000 / hz
}

pub use machine::*;

// ---- the real vCPUs: AArch64 Linux (the phone, an arm64 PC, qemu-user) ---------------------------------------------
#[cfg(all(xh_machine, not(test)))]
mod machine {
    use super::*;
    use crate::{gic, hostcalls, mem, session, sysreg};
    use std::ptr;
    use xh_core::log::signal_line;
    use xnuhost_abi::{XH_FRAME_EXCEPTION, XH_FRAME_IRQ, XH_HALT_PANIC};

    /// another thread raised this CPU's interrupt line
    pub(crate) fn sig_kick() -> i32 {
        sigrtmin() + 1
    }
    /// this CPU's timer is due
    fn sig_timer() -> i32 {
        sigrtmin() + 2
    }
    /// debugging: log where this CPU is (SIGQUIT to xnuhost)
    fn sig_dump() -> i32 {
        sigrtmin() + 4
    }
    pub(crate) fn sigrtmin() -> i32 {
        extern "C" {
            fn __libc_current_sigrtmin() -> libc::c_int;
        }
        // SAFETY: a libc query without arguments (glibc and bionic both have it).
        unsafe { __libc_current_sigrtmin() }
    }

    /// per vCPU, aligned: [cpu pointer page][signal stack]
    pub(crate) const SIGSTACK_BLOCK: usize = 0x10_0000;
    const BOOT_STACK: usize = 0x4_0000;
    const HOST_STACK: usize = 0x8_0000;
    const SIGSTACK_SKIP: usize = 0x1_0000;

    /// [lo, hi) of the vCPU signal stacks, read by xh_sig_entry (arch.rs).
    #[no_mangle]
    pub(crate) static xh_sig_arena: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    /// [lo, hi) of the vCPUs' boot and host stacks, BOOT_STACK + HOST_STACK each.
    static STACKS: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];

    #[inline(always)]
    fn stack_pointer() -> usize {
        let sp: usize;
        // SAFETY: reads sp.
        unsafe { std::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags)) };
        sp
    }

    /// The calling vCPU, `None` on other threads (or on a vCPU thread before it entered the kernel): from the stack
    /// it runs on, its host stack (a host call) or its signal stack (a signal).
    #[inline]
    pub fn current() -> Option<&'static Vcpu> {
        let sp = stack_pointer();
        let n = ncpu();
        let lo = STACKS[0].load(Relaxed);
        if sp > lo && sp <= STACKS[1].load(Relaxed) {
            let i = (sp - lo - 1) / (BOOT_STACK + HOST_STACK);
            return (i < n).then(|| &VCPUS[i]);
        }
        let lo = xh_sig_arena[0].load(Relaxed) as usize;
        if sp > lo && sp <= xh_sig_arena[1].load(Relaxed) as usize {
            let i = (sp - lo - 1) / SIGSTACK_BLOCK;
            return (i < n).then(|| &VCPUS[i]);
        }
        None
    }

    #[inline]
    pub fn counter() -> u64 {
        let v: u64;
        // SAFETY: reads the virtual counter, readable at EL0.
        unsafe { std::arch::asm!("mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack, preserves_flags)) };
        v
    }

    pub fn counter_hz() -> u64 {
        let v: u64;
        // SAFETY: reads the counter's frequency, readable at EL0.
        unsafe { std::arch::asm!("mrs {}, cntfrq_el0", out(reg) v, options(nomem, nostack, preserves_flags)) };
        v
    }

    fn futex(word: &AtomicU32, op: i32, value: u32, timeout: Option<&libc::timespec>) {
        let t = timeout.map_or(ptr::null(), |t| t as *const libc::timespec);
        // SAFETY: a futex call on a live word; a timeout is a live timespec or null.
        unsafe { libc::syscall(libc::SYS_futex, word.as_ptr(), op, value, t, ptr::null::<u32>(), 0) };
    }

    pub(crate) fn tgkill(pid: i32, tid: i32, sig: i32) {
        // SAFETY: a signal to a thread; a gone one is an error return.
        unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, sig) };
    }

    fn getpid() -> i32 {
        // SAFETY: no arguments.
        unsafe { libc::getpid() }
    }

    // ---- the interrupt line ----------------------------------------------------------------------------------------
    /// The interrupt line of a CPU (the GIC's notify). Lowering is done only by the CPU itself, by recomputing:
    /// `pending` is the GIC's answer (the GIC passes its own, under its lock).
    pub fn line_with(index: usize, line: bool, pending: impl FnOnce() -> bool) {
        if index >= ncpu() {
            return;
        }
        let v = &VCPUS[index];
        let me = current();
        let is_me = me.is_some_and(|m| ptr::eq(m, v));
        if !line {
            // Only the CPU itself lowers its line, and by recomputing: a store of 0 from another thread could
            // overwrite a 1 the timer signal just set.
            if is_me {
                v.cpu.irq_pending.store(0, Release);
                if pending() {
                    v.cpu.irq_pending.store(1, Release);
                }
            }
            return;
        }
        // A line that is up already was signalled when it rose (host-overhead, 2026-11-15). The GIC tells every CPU
        // its line at each change (an ack, an EOI, an SGI on any vCPU), so a vCPU with an interrupt pending while it
        // runs masked got a signal per change elsewhere: each a tgkill here and a signal delivery there that only
        // returned (the kernel takes a pending line when it unmasks, hosted_irq_unmask; a host call when it
        // returns; thread_run and cpu_idle look at the word before they wait). The line goes down only by the CPU
        // itself, which then recomputes it under the GIC's lock, after this raise's change (the caller holds that
        // lock), so a raise skipped here is never lost.
        if !is_me && v.cpu.irq_pending.load(Acquire) != 0 {
            if let Some(m) = me {
                m.perf.count(crate::perf::Count::KicksSkipped, 1);
            }
            return;
        }
        v.cpu.irq_pending.store(1, SeqCst);
        if is_me || !v.started.load(Acquire) {
            return;
        }
        if let Some(m) = me {
            m.perf.count(crate::perf::Count::Kicks, 1);
        }
        if v.idle.load(Acquire) != 0 {
            futex(&v.cpu.irq_pending, libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG, 1, None);
        } else if crate::task::kick(v) {
            // it waits for an iOS thread: that thread is kicked
            if let Some(m) = me {
                m.perf.count(crate::perf::Count::KicksDirect, 1);
            }
        } else {
            tgkill(getpid(), v.tid.load(Acquire), sig_kick());
        }
    }

    /// `xh_cpu_line`: from outside the GIC (host calls, the timer): a lowering asks the GIC.
    pub fn line(index: usize, line: bool) {
        line_with(index, line, || gic::pending(index))
    }

    fn settime(v: &Vcpu, spec: &libc::itimerspec) {
        // SAFETY: the kernel's timer_settime on the vCPU's own timer id; spec is live.
        unsafe {
            libc::syscall(libc::SYS_timer_settime, v.timer.load(Relaxed), 0, spec as *const _, ptr::null::<u8>())
        };
    }

    pub fn timer_set(v: &Vcpu, deadline: u64) {
        let index = v.index();
        v.perf.count(crate::perf::Count::TimerSets, 1);
        // The same future deadline again (XNU sets it at each dispatch and idle exit, often unchanged): the host timer
        // is armed for it already, so no timer_settime (host-overhead, 2026-11-15: the PC's boot made 2,000 a second,
        // each a host call).
        if deadline != u64::MAX
            && v.timer_armed.load(Acquire)
            && v.timer_deadline.load(Acquire) == deadline
            && deadline > counter()
        {
            line(index, false);
            return;
        }
        let coalesce = TIMER_COALESCE.load(Relaxed);
        if deadline == u64::MAX {
            v.timer_armed.store(false, Release);
            let host_at = v.timer_host_at.load(Acquire);
            if host_at == 0 {
                // Already disarmed (never armed, already fired, or already cleared): skip a redundant timer_settime(0)
                // syscall (YMGPwcca/OneiOS 257f7ee7).
                line(index, false);
                return;
            }
            if coalesce {
                // timer-coalesce: the host timer stays armed; its signal finds nothing armed and raises nothing
                v.perf.count(crate::perf::Count::TimerKept, 1);
                line(index, false);
                return;
            }
            v.perf.count(crate::perf::Count::TimerCalls, 1);
            v.timer_host_at.store(0, Release);
            settime(v, &ZERO_SPEC);
            line(index, false);
            return;
        }
        v.timer_deadline.store(deadline, Release);
        v.timer_armed.store(true, Release);
        let now = counter();
        if deadline <= now {
            if v.timer_host_at.load(Acquire) != 0 {
                v.perf.count(crate::perf::Count::TimerCalls, 1);
                v.timer_host_at.store(0, Release);
                settime(v, &ZERO_SPEC);
            }
            line(index, gic::pending(index));
            return;
        }
        // timer-coalesce: armed for an earlier time still to come. It fires then, and its signal arms the host timer for
        // this deadline (read after the stores above: a signal between the two sees this deadline and arms for it).
        let host_at = v.timer_host_at.load(Acquire);
        if coalesce && host_at != 0 && host_at <= deadline && host_at > now {
            v.perf.count(crate::perf::Count::TimerKept, 1);
            line(index, false);
            return;
        }
        v.perf.count(crate::perf::Count::TimerCalls, 1);
        arm_host(v, deadline, now);
        line(index, false);
    }

    const ZERO_SPEC: libc::itimerspec = libc::itimerspec {
        it_interval: libc::timespec { tv_sec: 0, tv_nsec: 0 },
        it_value: libc::timespec { tv_sec: 0, tv_nsec: 0 },
    };

    /// The vCPU's host timer at `deadline` (> `now`). On the vCPU's own thread (timer_set, or its timer signal).
    fn arm_host(v: &Vcpu, deadline: u64, now: u64) {
        let mut spec = ZERO_SPEC;
        v.timer_host_at.store(deadline, Release);
        let ns = ticks_to_ns(deadline - now, hostcalls::info().timebase_hz);
        spec.it_value.tv_sec = (ns / 1_000_000_000) as libc::time_t;
        spec.it_value.tv_nsec = (ns % 1_000_000_000) as libc::c_long + 1;
        if spec.it_value.tv_nsec >= 1_000_000_000 {
            spec.it_value.tv_sec += 1;
            spec.it_value.tv_nsec -= 1_000_000_000;
        }
        settime(v, &spec);
    }

    /// The vCPU's timer signal (xh_cpu_signal): whether to raise the line. With timer-coalesce the host timer may have
    /// been armed for an earlier deadline than the one set now, or a cancelled one: then it is armed for the deadline
    /// (one timer_settime) and nothing is raised. Without, every signal raises the line, as before.
    fn timer_signal(v: &Vcpu) -> bool {
        v.timer_host_at.store(0, Release);
        if !TIMER_COALESCE.load(Relaxed) {
            return true;
        }
        if !v.timer_armed.load(Acquire) {
            return false;
        }
        let (now, deadline) = (counter(), v.timer_deadline.load(Acquire));
        if deadline > now {
            v.perf.count(crate::perf::Count::TimerRearms, 1);
            arm_host(v, deadline, now);
            return false;
        }
        true
    }

    pub fn idle(v: &Vcpu) {
        let began = counter();
        v.idle.store(1, Release);
        // An idle vCPU's thread carries no floor: what wakes it (a timer, an IPI) is the kernel's own short work, and a
        // clamp left from the last thread it ran made each of those wakes ask the host for that thread's clock (the
        // guest's request "keeps its request" while idle, DroidVMCpufreq.c). Its next release or request sets it again.
        own_uclamp(v, 0);
        while v.cpu.irq_pending.load(Acquire) == 0 {
            if session::stopping() {
                // SAFETY: waits for a signal.
                unsafe { libc::pause() };
                continue;
            }
            // Sleep until woken, but no longer than to the timer's deadline: inside an emulated WFI the timer's
            // signal is blocked, so the deadline is watched here.
            let mut step = libc::timespec { tv_sec: 1, tv_nsec: 0 };
            if v.timer_armed.load(Acquire) {
                let (now, deadline, hz) = (counter(), v.timer_deadline.load(Relaxed), hostcalls::info().timebase_hz);
                if deadline <= now {
                    if gic::pending(v.index()) {
                        v.cpu.irq_pending.store(1, Release);
                        break;
                    }
                    // due but not enabled or masked at the GIC: nothing to deliver; sleep the step
                } else if (deadline - now) / hz == 0 {
                    step = libc::timespec {
                        tv_sec: 0,
                        tv_nsec: ((deadline - now) * 1_000_000_000 / hz) as libc::c_long + 1000,
                    };
                }
            }
            futex(&v.cpu.irq_pending, libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG, 0, Some(&step));
        }
        v.idle.store(0, Release);
        v.perf.count(crate::perf::Count::TicksIdle, counter() - began);
    }

    /// drm2kgsl (its patch 0008) while a constrained queue animates: the value only, which [`release_floor`] gives the
    /// frame's threads at their next release (thread_run), and their vCPU's thread with it. No vCPU thread is set here
    /// any more: until 2026-11-23 all eight carried 1024 through every animation, idle or busy with background work.
    /// Returns the vCPUs that take it (the started ones).
    #[no_mangle]
    pub extern "C" fn drm2kgsl_vcpu_uclamp(value: u32) -> libc::c_int {
        let value = value.min(1024);
        GPU_VCPU_BOOST.store(value, Relaxed);
        if !UCLAMP_ALL.load(Relaxed) {
            return vcpus().iter().filter(|v| v.started.load(Acquire)).count() as libc::c_int;
        }
        // uclamp-rule=all (docs/switches.md): every started vCPU thread gets the clamp, as before 03d85ac.
        let mut set = 0;
        for v in vcpus() {
            let tid = v.tid.load(Acquire);
            if !v.started.load(Acquire) || tid <= 0 {
                continue;
            }
            // KEEP_POLICY | KEEP_PARAMS | UTIL_CLAMP_MIN
            if crate::task::sched_setattr_uclamp(tid, 0x18 | 0x20, value, 0) == 0 {
                set += 1;
            }
        }
        set
    }

    // ---- signals ---------------------------------------------------------------------------------------------------
    /// Linux's arm64 signal frame (arch/arm64/include/uapi/asm/{sigcontext,ucontext}.h): the kernel's layout, not
    /// libc's, so that frame_resume can build one for rt_sigreturn.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct StackT {
        ss_sp: *mut libc::c_void,
        ss_flags: i32,
        ss_size: usize,
    }
    #[repr(C, align(16))]
    struct SigContext {
        fault_address: u64,
        regs: [u64; 31],
        sp: u64,
        pc: u64,
        pstate: u64,
        reserved: Reserved,
    }
    /// __reserved, aligned to 16 as the kernel's.
    #[repr(C, align(16))]
    struct Reserved([u8; 4096]);
    #[repr(C)]
    struct UContext {
        uc_flags: u64,
        uc_link: *mut UContext,
        uc_stack: StackT,
        uc_sigmask: u64,
        unused: [u8; 1024 / 8 - 8],
        uc_mcontext: SigContext,
    }
    #[repr(C)]
    struct CtxHead {
        magic: u32,
        size: u32,
    }
    #[repr(C)]
    struct FpsimdContext {
        head: CtxHead,
        fpsr: u32,
        fpcr: u32,
        vregs: [u128; 32],
    }
    const FPSIMD_MAGIC: u32 = 0x4650_8001;
    const ESR_MAGIC: u32 = 0x4553_5201;
    const _: () = assert!(std::mem::offset_of!(UContext, uc_mcontext) == 176);
    const _: () = assert!(std::mem::offset_of!(SigContext, reserved) == 288);
    const _: () = assert!(std::mem::size_of::<FpsimdContext>() == 528);

    /// The siginfo fields read here (the kernel's layout on 64-bit: si_addr at 16).
    #[repr(C)]
    struct SigInfo {
        si_signo: i32,
        si_errno: i32,
        si_code: i32,
        pad: i32,
        si_addr: u64,
        rest: [u64; 13],
    }
    const _: () = assert!(std::mem::size_of::<SigInfo>() == 128);

    /// The record of `magic` in a signal frame's __reserved area.
    fn record(uc: &UContext, magic: u32) -> Option<*const CtxHead> {
        let base = uc.uc_mcontext.reserved.0.as_ptr();
        let mut at = 0usize;
        while at + std::mem::size_of::<CtxHead>() <= uc.uc_mcontext.reserved.0.len() {
            // SAFETY: inside the 4096-byte reserved area; records are 8-aligned (the area is 16-aligned).
            let head = unsafe { &*(base.add(at) as *const CtxHead) };
            if head.magic == magic {
                return Some(head);
            }
            if head.magic == 0 || (head.size as usize) < std::mem::size_of::<CtxHead>() {
                break;
            }
            at += head.size as usize;
        }
        None
    }

    fn fpsimd_of(uc: &UContext) -> Option<&FpsimdContext> {
        // SAFETY: a record with the FPSIMD magic is a struct fpsimd_context (the kernel wrote it).
        record(uc, FPSIMD_MAGIC).map(|p| unsafe { &*(p as *const FpsimdContext) })
    }

    fn esr_of(uc: &UContext) -> u64 {
        // SAFETY: a record with the ESR magic is a struct esr_context: { head, u64 esr }.
        record(uc, ESR_MAGIC).map_or(0, |p| unsafe { *((p as *const u8).add(8) as *const u64) })
    }

    extern "C" {
        fn xh_upcall_tramp();
        fn xh_sig_entry(signo: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void);
        fn xh_arch_enter(cpu: *mut XhCpu, entry: u64, a0: u64, a1: u64, a2: u64, sp: u64) -> !;
        fn xh_arch_sigreturn(cpu: *mut XhCpu, sigframe: *mut libc::c_void, tpidr: u64, unused: u64, masked: u32) -> !;
    }

    /// Saves the interrupted kernel context in a frame on its own stack and makes the signal return into the kernel's
    /// upcall instead (xh_upcall_tramp), with interrupts masked.
    fn divert(v: &Vcpu, uc: &mut UContext, kind: u32, upcall: usize, far: u64, esr: u64, info: Option<&SigInfo>) {
        let mc = &mut uc.uc_mcontext;
        let at = (mc.sp - 256 - std::mem::size_of::<XhFrame>() as u64) & !15;
        let fp = fpsimd_of(uc).map(|f| (f.fpsr, f.fpcr, f.vregs));
        let mc = &mut uc.uc_mcontext;
        let mut frame = XhFrame { sp: mc.sp, pc: mc.pc, pstate: mc.pstate, kind, far, esr, ..XhFrame::default() };
        frame.x = mc.regs;
        if let Some((fpsr, fpcr, vregs)) = fp {
            frame.fpsr = fpsr;
            frame.fpcr = fpcr;
            for (out, r) in frame.v.iter_mut().zip(vregs) {
                out.lo = r as u64;
                out.hi = (r >> 64) as u64;
            }
        }
        frame.irq_masked = v.cpu.irq_masked.load(Relaxed);
        frame.signo = info.map_or(0, |i| i.si_signo as u32);
        frame.sigcode = info.map_or(0, |i| i.si_code as u32);
        if kind == XH_FRAME_IRQ {
            v.stat.acks_mark.set(v.stat.acks.load(Relaxed));
        }
        // SAFETY: the frame goes on the interrupted kernel stack, 256 bytes below its sp (its red zone kept), which
        // the kernel owns and is mapped; the kernel's upcall reads it there.
        unsafe { ptr::write(at as *mut XhFrame, frame) };
        v.cpu.irq_masked.store(1, Relaxed);
        mc.regs[0] = v.cpu.as_ptr() as u64;
        mc.regs[1] = at;
        mc.regs[2] = upcall as u64;
        mc.sp = at;
        mc.pc = xh_upcall_tramp as *const () as u64;
        v.stat.irqs.add(1);
    }

    fn maybe_interrupt(v: &Vcpu, uc: &mut UContext) {
        let irq = UPCALLS.irq.load(Acquire);
        if v.cpu.irq_pending.load(Relaxed) != 0
            && v.cpu.irq_masked.load(Relaxed) == 0
            && v.cpu.in_host.load(Relaxed) == 0
            && irq != 0
        {
            divert(v, uc, XH_FRAME_IRQ, irq, 0, 0, None);
        }
    }

    fn host_fault(signo: i32, info: &SigInfo, uc: &UContext, what: &str) -> ! {
        signal_line(what, signo as u64, info.si_addr);
        signal_line("  pc, sp", uc.uc_mcontext.pc, uc.uc_mcontext.sp);
        signal_line("  x30, esr", uc.uc_mcontext.regs[30], esr_of(uc));
        fatal_line(signo, info);
        // xnuhost's own frame records: which code of ours (symbolize against the xnuhost binary).
        frames(uc.uc_mcontext.regs[29], 16, |_| true, "  host frame: fp, lr");
        die_of(signo)
    }

    /// Signals whose default action ends xnuhost and that nothing of ours takes (the faults have host_fault, the
    /// stop signals the main loop): an abort (bionic's, Rust's panic=abort, the allocator's), a seccomp or bad system
    /// call, a timer or a signal some other process sent. Each ends xnuhost as before, with a line first: until
    /// 2026-11-23 they ended it without a word (the phone, 19:06: xnuhost.log just stops).
    pub(crate) const LAST_WORDS: [i32; 12] = [
        libc::SIGABRT,
        libc::SIGSYS,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGALRM,
        libc::SIGVTALRM,
        libc::SIGPROF,
        libc::SIGXCPU,
        libc::SIGXFSZ,
        libc::SIGSTKFLT,
        libc::SIGIO,
        libc::SIGPWR,
    ];

    /// One of LAST_WORDS: the line, where it was (on a host thread or in a host call, xnuhost's frames), then the
    /// signal's own death.
    fn last_words(signo: i32, info: &SigInfo, uc: &UContext, v: Option<&Vcpu>) -> ! {
        fatal_line(signo, info);
        signal_line("  pc, sp", uc.uc_mcontext.pc, uc.uc_mcontext.sp);
        signal_line("  x30, esr", uc.uc_mcontext.regs[30], esr_of(uc));
        match v {
            Some(v) if v.cpu.in_host.load(Relaxed) == 0 => {
                signal_line("  in the kernel: cpu, pc", v.index() as u64, uc.uc_mcontext.pc)
            }
            _ => frames(uc.uc_mcontext.regs[29], 16, |_| true, "  host frame: fp, lr"),
        }
        die_of(signo)
    }

    /// `xnuhost !! fatal signal N (NAME) on thread 'T' (tid N): code C, sent by pid P uid U | address A`.
    fn fatal_line(signo: i32, info: &SigInfo) {
        let mut name = [0u8; 17];
        // SAFETY: PR_GET_NAME writes at most 16 bytes, NUL included, into a buffer of 17; prctl is a system call.
        unsafe { libc::prctl(libc::PR_GET_NAME, name.as_mut_ptr() as libc::c_ulong, 0, 0, 0) };
        let len = name.iter().position(|&c| c == 0).unwrap_or(16);
        // SAFETY: gettid has no preconditions.
        let tid = unsafe { libc::gettid() };
        // si_code <= 0: a process sent it (SI_USER, SI_TKILL, SI_QUEUE); its _kill record (si_pid, si_uid) is where a
        // fault's si_addr is.
        let sender = (info.si_code <= 0).then_some((info.si_addr as u32 as i32, (info.si_addr >> 32) as u32));
        xh_core::log::fatal_line(&xh_core::log::FatalSignal {
            signo,
            code: info.si_code,
            thread: &name[..len],
            tid,
            sender,
            addr: info.si_addr,
        });
    }

    /// Ends xnuhost of `signo` itself, so the switch's inner shell sees the signal (not an exit code): its default
    /// action, unblocked (a handler runs with its signals blocked), raised on this thread.
    fn die_of(signo: i32) -> ! {
        // SAFETY: the default action, the signal unblocked on this thread, then the signal: the process ends of it.
        // _exit is the backstop should it not.
        unsafe {
            libc::signal(signo, libc::SIG_DFL);
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, signo);
            tgkill(getpid(), libc::gettid(), signo);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, ptr::null_mut());
            libc::_exit(128 + signo)
        }
    }

    /// A frame record chain [fp, lr] from `fp`, as far as `ok` allows, each pair to `line`.
    fn frames(mut fp: u64, depth: usize, ok: impl Fn(u64) -> bool, text: &str) {
        for _ in 0..depth {
            if fp == 0 || fp & 7 != 0 || !ok(fp) {
                break;
            }
            let mut vec = 0u8;
            // SAFETY: mincore only asks whether the page is mapped.
            if unsafe { libc::mincore((fp & !0xfff) as *mut libc::c_void, 0x1000, &mut vec) } != 0 {
                break;
            }
            // SAFETY: the page is mapped (asked just above) and fp is 8-aligned: a frame record of two words.
            let rec = unsafe { std::slice::from_raw_parts(fp as *const u64, 2) };
            signal_line(text, fp, rec[1]);
            if rec[0] <= fp {
                break;
            }
            fp = rec[0];
        }
    }

    fn dump(v: &Vcpu, uc: &UContext) {
        let m = &uc.uc_mcontext;
        let c = &v.cpu;
        signal_line("cpu state: index, pc", c.index.load(Relaxed) as u64, m.pc);
        signal_line("  x30, sp", m.regs[30], m.sp);
        signal_line("  irq_masked, irq_pending", c.irq_masked.load(Relaxed) as u64, c.irq_pending.load(Relaxed) as u64);
        let in_host = c.in_host.load(Relaxed);
        signal_line("  in_host, kernel tpidr", in_host as u64, c.kernel_tpidr.load(Relaxed));
        if in_host != 0 {
            // Inside a host call: xnuhost's own frame records (symbolize against the xnuhost binary), which say which
            // call the vCPU is in (idle, thread_run, a task order, a device).
            frames(m.regs[29], 12, |_| true, "  host frame: fp, lr");
        } else {
            // Kernel code: the argument and callee-saved registers too (a loop's counters and objects are in them).
            for i in (0..30).step_by(2) {
                signal_line(
                    if i < 10 { "  x(even), x(odd) from x0" } else { "  x(even), x(odd)" },
                    m.regs[i],
                    m.regs[i + 1],
                );
            }
            // The kernel's frame records, as far as they stay inside the RAM window or the kernel's mappings.
            let ram_virt = hostcalls::info().ram_virt;
            frames(m.regs[29], 24, |fp| fp >= ram_virt && fp < 0x3c_0000_0000, "  frame: fp, lr");
        }
    }

    /// Every signal of a vCPU thread, from xh_sig_entry (arch.rs) with the host's TPIDR_EL0 and x18 in place; `cpu`
    /// is the vCPU whose signal stack it runs on, null on another thread.
    #[no_mangle]
    pub(crate) unsafe extern "C" fn xh_cpu_signal(
        signo: i32,
        info: *mut libc::c_void,
        context: *mut libc::c_void,
        cpu: *mut XhCpu,
    ) {
        // SAFETY: Linux passes a ucontext and a siginfo for an SA_SIGINFO handler.
        let (uc, info) = unsafe { (&mut *(context as *mut UContext), &*(info as *const SigInfo)) };
        // SAFETY: xh_sig_entry passes a VCPUS block (from its signal stack's first word) or null.
        let v = (!cpu.is_null()).then(|| unsafe { Vcpu::from_cpu(cpu) });
        // errno of this thread (the host's TLS is in place), put back when the handler returns.
        let saved_errno = xh_core::sys::errno();
        let restore = || xh_core::sys::set_errno(saved_errno);
        if signo == sig_dump() {
            if let Some(v) = v {
                dump(v, uc);
            }
            return restore();
        }
        if signo == sig_kick() || signo == sig_timer() {
            let Some(v) = v else { return };
            // timer-coalesce: a signal before the deadline (or after a cancel) arms the host timer again, raises nothing
            if signo == sig_timer() && timer_signal(v) {
                v.cpu.irq_pending.store(1, Release);
            }
            maybe_interrupt(v, uc);
            return restore();
        }
        if LAST_WORDS.contains(&signo) {
            last_words(signo, info, uc, v);
        }
        // A synchronous trap. In host context (ours or a non-vCPU thread's) it is a bug of xnuhost.
        let v = match v {
            Some(v) if v.cpu.in_host.load(Relaxed) == 0 => v,
            Some(_) => host_fault(signo, info, uc, "fault inside a host call: signal, address"),
            None => host_fault(signo, info, uc, "fault on a host thread: signal, address"),
        };
        let mc = &uc.uc_mcontext;
        let mut regs = xh_core::regs::Regs { x: mc.regs, sp: mc.sp, pc: mc.pc, pstate: mc.pstate };
        let far = info.si_addr;
        let mut handled = false;
        if signo == libc::SIGSEGV || signo == libc::SIGBUS {
            let m = mem::state();
            let io = m.io_host as u64;
            if far >= io && far - io < m.io_size && regs.pc != far {
                v.stat.mmio.add(1);
                // SAFETY: pc is the faulting kernel instruction: mapped, 4-aligned.
                let insn = unsafe { *(regs.pc as *const u32) };
                handled = xh_core::mmio::bus().emulate(&mut regs, insn, m.io_phys + (far - io));
            }
        } else if signo == libc::SIGILL {
            // in_host covers the emulation: PSCI CPU_ON and WFI call into the machine layer.
            v.cpu.in_host.store(1, Relaxed);
            // SAFETY: as above: the undefined kernel instruction at pc.
            let insn = unsafe { *(regs.pc as *const u32) };
            handled = sysreg::emulate(v, &mut regs, insn);
            v.cpu.in_host.store(0, Relaxed);
        }
        let mc = &mut uc.uc_mcontext;
        if handled {
            mc.regs = regs.x;
            mc.sp = regs.sp;
            mc.pc = regs.pc;
            maybe_interrupt(v, uc);
            return restore();
        }
        let exception = UPCALLS.exception.load(Acquire);
        if exception != 0 {
            // A SIGTRAP's esr_context is never its own: Linux sends every SIGTRAP (brk, single step, watchpoint)
            // through send_user_sigtrap(), which leaves thread.fault_code alone, and fault_code is written only when a
            // fault signal is delivered. On a vCPU thread it is the last software-TLB fill's data abort, and the
            // kernel took a UBSan soft trap (brk #UBSAN_SOFT_TRAP_SIGNED_OF in tcp_input's compute_iaj_meat) for a
            // data abort at pc: "Unexpected fault in kernel physical aperture" (phone, 2026-11-12 23:15). Without a
            // syndrome the kernel reads the brk at pc (hosted_kernel_esr).
            let esr = if signo == libc::SIGTRAP { 0 } else { esr_of(uc) };
            divert(v, uc, XH_FRAME_EXCEPTION, exception, far, esr, Some(info));
            return restore();
        }
        let mc = &uc.uc_mcontext;
        signal_line("kernel fault with no exception upcall: signal, address", signo as u64, far);
        signal_line("  pc, sp", mc.pc, mc.sp);
        signal_line("  x30, esr", mc.regs[30], esr_of(uc));
        let m = mem::state();
        let insn = if regs.pc >= m.host as u64 && regs.pc - (m.host as u64) < m.size {
            // SAFETY: inside the RAM window, which is mapped.
            unsafe { *(regs.pc as *const u32) as u64 }
        } else {
            0
        };
        signal_line("  instruction, cpu", insn, v.index() as u64);
        session::stop(XH_HALT_PANIC, 0, "kernel fault");
        loop {
            // SAFETY: waits for a signal.
            unsafe { libc::pause() };
        }
    }

    // ---- frame_resume: restore a full kernel context ---------------------------------------------------------------
    /// The signal mask vCPU threads run with (the kernel's 64-bit sigset).
    static VCPU_MASK: AtomicU64 = AtomicU64::new(0);

    fn sigbit(sig: i32) -> u64 {
        1u64 << (sig - 1)
    }

    /// pthread_sigmask(SIG_SETMASK) to the kernel's 64-bit mask `mask` (libc leaves out the signals it reserves, as
    /// for the C's sigfillset).
    fn set_mask(mask: u64) {
        let mut set: libc::sigset_t = unsafe_zeroed();
        const { assert!(std::mem::size_of::<libc::sigset_t>() >= 8) };
        // SAFETY: on glibc and bionic (LP64) a sigset_t starts with the kernel's mask word, signal n at bit n - 1.
        unsafe {
            ptr::write_unaligned(&mut set as *mut libc::sigset_t as *mut u64, mask);
            libc::pthread_sigmask(libc::SIG_SETMASK, &set, ptr::null_mut());
        }
    }

    fn unsafe_zeroed<T>() -> T {
        // SAFETY: used for C structs (sigset_t, sigaction, stack_t) whose all-zero value is valid.
        unsafe { std::mem::zeroed() }
    }

    /// The kernel's frame_resume: does not return.
    pub fn frame_resume(v: &Vcpu, frame: &XhFrame) -> ! {
        // Linux's own signal return restores every register at once; build the frame it expects
        // (arch/arm64/kernel/signal.c: struct rt_sigframe = siginfo, ucontext) on the host stack.
        #[repr(C, align(16))]
        struct RtSigframe {
            info: SigInfo,
            uc: UContext,
        }
        let mut sf: RtSigframe = unsafe_zeroed();
        let sig_stack = v.sig_stack.load(Relaxed);
        sf.uc.uc_stack = StackT {
            ss_sp: (sig_stack + SIGSTACK_SKIP) as *mut _,
            ss_flags: 0,
            ss_size: SIGSTACK_BLOCK - SIGSTACK_SKIP,
        };
        sf.uc.uc_sigmask = VCPU_MASK.load(Relaxed);
        let mc = &mut sf.uc.uc_mcontext;
        mc.regs = frame.x;
        mc.sp = frame.sp;
        mc.pc = frame.pc;
        mc.pstate = frame.pstate & 0xf000_0000;
        let mut fp = FpsimdContext {
            head: CtxHead { magic: FPSIMD_MAGIC, size: std::mem::size_of::<FpsimdContext>() as u32 },
            fpsr: frame.fpsr,
            fpcr: frame.fpcr,
            vregs: [0; 32],
        };
        for (r, v) in fp.vregs.iter_mut().zip(&frame.v) {
            *r = u128::from(v.lo) | u128::from(v.hi) << 64;
        }
        // SAFETY: the fpsimd record and the zero end record go at the start of the 4096-byte reserved area, which is
        // 16-aligned (the end record stays zero from the zeroed frame).
        unsafe { ptr::write(mc.reserved.0.as_mut_ptr() as *mut FpsimdContext, fp) };
        // From here to the signal return nothing may divert this context again.
        set_mask(u64::MAX);
        // An exception return with the line up takes the interrupt at once. Here: a kick to ourselves, which stays
        // pending until the signal return opens the mask, and then diverts the resumed context into the interrupt
        // upcall. Not after an interrupt upcall that acknowledged nothing (a kernel with no handler for this CPU
        // yet): that would only come straight back.
        if frame.irq_masked == 0
            && v.cpu.irq_pending.load(Acquire) != 0
            && (frame.kind != XH_FRAME_IRQ || v.stat.acks.load(Relaxed) != v.stat.acks_mark.get())
        {
            v.stat.reraised.add(1);
            tgkill(getpid(), v.tid.load(Relaxed), sig_kick());
        }
        // SAFETY: sf is a complete rt_sigframe on this (host) stack; xh_arch_sigreturn moves sp to it and makes the
        // rt_sigreturn system call, which restores the kernel context and never returns here.
        unsafe {
            xh_arch_sigreturn(
                v.cpu.as_ptr(),
                &mut sf as *mut RtSigframe as *mut libc::c_void,
                v.cpu.kernel_tpidr.load(Relaxed),
                0,
                frame.irq_masked,
            )
        }
    }

    // ---- threads ---------------------------------------------------------------------------------------------------
    fn vcpu_main(v: &'static Vcpu) -> ! {
        let index = v.index();
        // SAFETY: gettid and pthread_self have no preconditions.
        let (tid, me) = unsafe { (libc::gettid(), libc::pthread_self()) };
        v.tid.store(tid, Release);
        v.thread.store(me as usize, Release);
        let (tpidr, x18): (u64, u64);
        // SAFETY: reads this thread's TPIDR_EL0 and x18 (the host's, restored around every host call and signal).
        unsafe {
            std::arch::asm!("mrs {}, tpidr_el0", "mov {}, x18", out(reg) tpidr, out(reg) x18, options(nomem, nostack))
        };
        v.cpu.host_tpidr.store(tpidr, Relaxed);
        v.cpu.host_x18.store(x18, Relaxed);
        v.cpu.host_sp.store((v.host_stack.load(Relaxed) + HOST_STACK) as u64, Relaxed);
        v.cpu.in_host.store(1, Relaxed);
        v.cpu.irq_masked.store(1, Relaxed); // as a CPU leaves reset: DAIF masked
        let sig_stack = v.sig_stack.load(Relaxed);
        // SAFETY: the first word of the vCPU's signal block is its CPU pointer (xh_sig_entry reads it); the block is
        // ours, mapped read-write.
        unsafe { ptr::write(sig_stack as *mut u64, v.cpu.as_ptr() as u64) };
        let ss = libc::stack_t {
            ss_sp: (sig_stack + SIGSTACK_SKIP) as *mut _,
            ss_flags: 0,
            ss_size: SIGSTACK_BLOCK - SIGSTACK_SKIP,
        };
        // SAFETY: the signal stack is ours for the thread's life.
        unsafe { libc::sigaltstack(&ss, ptr::null_mut()) };
        // The kernel's sigevent: { value, signo, notify, thread id }.
        #[repr(C)]
        struct SigEvent {
            value: u64,
            signo: i32,
            notify: i32,
            tid: i32,
            pad: [i32; 11],
        }
        let ev = SigEvent { value: 0, signo: sig_timer(), notify: 4 /* SIGEV_THREAD_ID */, tid, pad: [0; 11] };
        let mut id: i32 = -1;
        // SAFETY: the kernel's timer_create with a live sigevent and timer id.
        let r = unsafe {
            libc::syscall(libc::SYS_timer_create, libc::CLOCK_MONOTONIC, &ev as *const SigEvent, &mut id as *mut i32)
        };
        if r != 0 {
            xh_core::loge!("cpu{}: timer_create: {}", index, xh_core::sys::strerror(xh_core::sys::errno()));
            session::stop(XH_HALT_PANIC, 0, "no timer");
            loop {
                // SAFETY: waits for a signal (the stop ends the process).
                unsafe { libc::pause() };
            }
        }
        v.timer.store(id, Release);
        if let Some(place) = place_hook() {
            place(0, index);
        }
        set_mask(VCPU_MASK.load(Relaxed));
        let (mut entry, mut a0, mut a1, a2) =
            (v.entry.load(Relaxed), v.args[0].load(Relaxed), v.args[1].load(Relaxed), v.args[2].load(Relaxed));
        if entry == 0 {
            entry = UPCALLS.cpu_entry.load(Acquire) as u64;
            a1 = a0;
            a0 = v.cpu.as_ptr() as u64;
        }
        xh_core::logi!("cpu{}: entering the kernel at 0x{:x}", index, entry);
        v.started.store(true, Release);
        // SAFETY: the boot stack is this vCPU's, mapped; the kernel's entry takes it from here for good.
        unsafe { xh_arch_enter(v.cpu.as_ptr(), entry, a0, a1, a2, (v.boot_stack.load(Relaxed) + BOOT_STACK) as u64) }
    }

    fn mmap_anon(size: usize) -> Option<usize> {
        // SAFETY: a new private anonymous mapping anywhere.
        let p = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        (p != libc::MAP_FAILED).then_some(p as usize)
    }

    pub fn init(ncpu: usize) -> Result<(), crate::mem::SetupFailed> {
        if ncpu == 0 || ncpu > MAX_CPUS {
            return Err(crate::mem::SetupFailed);
        }
        // One arena for all signal stacks, 1 MiB aligned, so xh_sig_entry finds the CPU from sp.
        let raw = mmap_anon(SIGSTACK_BLOCK * (ncpu + 1)).ok_or(crate::mem::SetupFailed)?;
        let stacks = mmap_anon((BOOT_STACK + HOST_STACK) * ncpu).ok_or(crate::mem::SetupFailed)?;
        let aligned = (raw + SIGSTACK_BLOCK - 1) & !(SIGSTACK_BLOCK - 1);
        xh_sig_arena[0].store(aligned as u64, Relaxed);
        xh_sig_arena[1].store((aligned + SIGSTACK_BLOCK * ncpu) as u64, Relaxed);
        STACKS[0].store(stacks, Relaxed);
        STACKS[1].store(stacks + (BOOT_STACK + HOST_STACK) * ncpu, Relaxed);
        for (i, v) in VCPUS[..ncpu].iter().enumerate() {
            v.cpu.index.store(i as u32, Relaxed);
            v.sig_stack.store(aligned + SIGSTACK_BLOCK * i, Relaxed);
            v.boot_stack.store(stacks + (BOOT_STACK + HOST_STACK) * i, Relaxed);
            v.host_stack.store(stacks + (BOOT_STACK + HOST_STACK) * i + BOOT_STACK, Relaxed);
        }
        NCPU.store(ncpu, Release);

        // vCPU threads take the two interrupt signals and the synchronous traps; everything else (SIGTERM, SIGINT,
        // ...) is the main thread's.
        let open =
            [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGTRAP, libc::SIGFPE, libc::SIGSYS, libc::SIGABRT];
        let mut mask = u64::MAX;
        for sig in open.into_iter().chain([sig_kick(), sig_timer(), sig_dump()]) {
            mask &= !sigbit(sig);
        }
        VCPU_MASK.store(mask, Relaxed);
        let mut sa: libc::sigaction = unsafe_zeroed();
        sa.sa_sigaction = xh_sig_entry as *const () as usize;
        sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
        // SAFETY: a full mask in the handler's sigaction, then the handlers: xh_sig_entry is made for exactly these.
        unsafe {
            libc::sigfillset(&mut sa.sa_mask);
            for sig in [
                libc::SIGSEGV,
                libc::SIGBUS,
                libc::SIGILL,
                libc::SIGTRAP,
                libc::SIGFPE,
                sig_kick(),
                sig_timer(),
                sig_dump(),
            ] {
                libc::sigaction(sig, &sa, ptr::null_mut());
            }
            // The last words (LAST_WORDS), where the signal still has its default action: a handler someone else
            // set (or SIG_IGN) stays.
            for sig in LAST_WORDS {
                let mut old: libc::sigaction = unsafe_zeroed();
                if libc::sigaction(sig, ptr::null(), &mut old) == 0 && old.sa_sigaction == libc::SIG_DFL {
                    libc::sigaction(sig, &sa, ptr::null_mut());
                }
            }
        }
        Ok(())
    }

    pub fn dump_all() {
        for v in vcpus() {
            if v.started.load(Acquire) {
                tgkill(getpid(), v.tid.load(Relaxed), sig_dump());
            }
        }
    }

    static START: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Starts vCPU `index` at `entry` (x0..x2 = args). entry 0: upcalls->cpu_entry(cpu, a0).
    pub fn start(index: usize, entry: u64, a0: u64, a1: u64, a2: u64) -> Result<(), StartError> {
        if index >= ncpu() {
            return Err(StartError::Invalid);
        }
        let v: &'static Vcpu = &VCPUS[index];
        let _lock = START.lock().unwrap_or_else(|e| e.into_inner());
        if v.thread.load(Acquire) != 0 || v.started.load(Acquire) {
            return Err(StartError::Already); // already on
        }
        if entry == 0 && UPCALLS.cpu_entry.load(Acquire) == 0 {
            return Err(StartError::Invalid);
        }
        v.entry.store(entry, Relaxed);
        for (slot, a) in v.args.iter().zip([a0, a1, a2]) {
            slot.store(a, Relaxed);
        }
        // The new thread starts with every signal blocked and opens its own mask.
        let mut all: libc::sigset_t = unsafe_zeroed();
        let mut old: libc::sigset_t = unsafe_zeroed();
        // SAFETY: masks of this thread, put back after the spawn.
        unsafe {
            libc::sigfillset(&mut all);
            libc::pthread_sigmask(libc::SIG_SETMASK, &all, &mut old);
        }
        let spawned = std::thread::Builder::new().name(format!("xnu-cpu{index}")).spawn(move || vcpu_main(v));
        // SAFETY: as above.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &old, ptr::null_mut()) };
        match spawned {
            Ok(_) => {
                // marks it on until the thread itself stores its pthread
                v.thread.compare_exchange(0, 1, AcqRel, Relaxed).ok();
                Ok(())
            }
            Err(_) => Err(StartError::Invalid),
        }
    }
}

// ---- hosts that cannot run the kernel, and the unit tests ---------------------------------------------------------
#[cfg(any(not(xh_machine), test))]
mod machine {
    use super::*;

    pub fn counter() -> u64 {
        xh_core::time::monotonic_ns()
    }
    pub fn counter_hz() -> u64 {
        1_000_000_000
    }
    pub fn current() -> Option<&'static Vcpu> {
        None
    }
    pub fn line(index: usize, line: bool) {
        line_with(index, line, || false)
    }
    pub fn line_with(index: usize, line: bool, _pending: impl FnOnce() -> bool) {
        if index < ncpu() {
            VCPUS[index].cpu.irq_pending.store(line as u32, Relaxed);
        }
    }
    pub fn timer_set(v: &Vcpu, deadline: u64) {
        v.timer_armed.store(deadline != u64::MAX, Relaxed);
        v.timer_deadline.store(deadline, Relaxed);
    }
    pub fn idle(_v: &Vcpu) {}
    pub fn init(ncpu: usize) -> Result<(), crate::mem::SetupFailed> {
        NCPU.store(ncpu, Relaxed);
        for (i, v) in VCPUS[..ncpu].iter().enumerate() {
            v.cpu.index.store(i as u32, Relaxed);
        }
        Ok(())
    }
    pub fn dump_all() {}
    pub fn start(index: usize, _entry: u64, _a0: u64, _a1: u64, _a2: u64) -> Result<(), StartError> {
        if index >= ncpu() {
            return Err(StartError::Invalid);
        }
        if VCPUS[index].started.swap(true, Relaxed) {
            return Err(StartError::Already);
        }
        Ok(())
    }
    pub fn frame_resume(_v: &Vcpu, _frame: &XhFrame) -> ! {
        unreachable!("frame_resume needs AArch64 Linux")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    /// The machine's global state (the vCPUs, the GIC) is one per process: the tests that use it take this lock.
    pub(crate) static MACHINE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn machine() -> std::sync::MutexGuard<'static, ()> {
        MACHINE.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The floor a release gives (2026-11-23): drm2kgsl's animation clamp only reaches the frame's threads.
    #[test]
    fn release_floor() {
        use super::{hint_class, release_floor};
        use xnuhost_abi::{XH_HINT_BACKGROUND, XH_HINT_REALTIME, XH_HINT_UI, XH_HINT_VALID};
        let (rt, ui, uinit) = (
            XH_HINT_VALID | XH_HINT_REALTIME | XH_HINT_UI | 1024,
            XH_HINT_VALID | XH_HINT_UI | 1024,
            XH_HINT_VALID | XH_HINT_UI | 768,
        );
        let (def, bg) = (XH_HINT_VALID, XH_HINT_VALID | XH_HINT_BACKGROUND);
        // a daemon's default-class thread, its vCPU not asked for anything: nothing, animating or not
        assert_eq!(release_floor(def, 0, 0), 0);
        assert_eq!(release_floor(def, 0, 1024), 0);
        // an interactive group's default-class thread (the guest asks for its vCPU's top): the animation's clamp
        assert_eq!(release_floor(def, 512, 0), 512);
        assert_eq!(release_floor(def, 512, 1024), 1024);
        // the classes' floors, the boost on top
        assert_eq!(release_floor(uinit, 0, 0), 768);
        assert_eq!(release_floor(uinit, 0, 1024), 1024);
        assert!(release_floor(ui, 0, 0) == 1024 && release_floor(rt, 0, 0) == 1024);
        // background and utility: 0 whatever was asked
        assert!(release_floor(bg, 1024, 1024) == 0 && release_floor(bg, 0, 0) == 0);
        // a kernel before 0075 (no hint): the request or the boost, as before
        assert!(
            release_floor(0, 300, 0) == 300 && release_floor(0, 0, 1024) == 1024 && release_floor(0, 0, 4096) == 1024
        );
        assert_eq!(
            [hint_class(rt), hint_class(ui), hint_class(uinit), hint_class(def), hint_class(bg), hint_class(0)],
            [0, 1, 1, 2, 3, 2]
        );
        // uclamp-rule=all (docs/switches.md): the rule before 03d85ac: the boost reaches every thread not background
        use super::release_floor_all;
        assert!(release_floor_all(def, 0, 1024) == 1024 && release_floor_all(def, 0, 0) == 0);
        assert!(release_floor_all(uinit, 0, 0) == 768 && release_floor_all(bg, 1024, 1024) == 0);
        assert!(release_floor_all(0, 300, 0) == 300 && release_floor_all(ui, 0, 0) == 1024);
    }

    #[test]
    fn ticks() {
        assert_eq!(super::ticks_to_ns(24_000_000, 24_000_000), 1_000_000_000);
        // ten years of a 24 MHz counter
        let ticks = 24_000_000u64 * 86_400 * 3650;
        assert_eq!(super::ticks_to_ns(ticks + 12, 24_000_000), ticks / 24 * 1000 + 500);
    }

    /// test_xnuhost.c's test_cpu_window: the oneios-cpu window's arithmetic (host-overhead 2026-11-15) is the kernel's
    /// own C (kernel/patches/oneios-hosted/gpu/cpu_window.h, included by gpu/OneiOSAdreno.cpp), so the checks stay C:
    /// compiled with the host's cc and run. Skipped, with the reason, where there is no cc.
    #[test]
    fn cpu_window() {
        const CHECKS: &str = r#"#include <stdint.h>
#include <stdio.h>
#include "cpu_window.h"
static int failures;
#define CHECK(c) do { if (!(c)) { failures++; printf("FAIL line %d: %s\n", __LINE__, #c); } } while (0)
int main(void)
{
  const uint64_t S = 1000000000ull;
  // The phone's window: 8 vCPUs, the report on time (10 s), a stall's lump of 70 s of one task's time absorbed in it.
  // Before: all = the tasks' sum, 144 s of 10 s = 1441%. Now: the busy time, at most 8 x 10 s.
  const uint64_t span = oneios_cpu_span_ns(10 * S, 10);
  CHECK(span == 10 * S);
  const uint64_t busy = oneios_cpu_busy_ns(span, 8, 6 * S);            // the CPUs idled 6 s of their 80
  CHECK(busy == 74 * S && oneios_cpu_percent(busy, span) == 740);
  CHECK(oneios_cpu_percent(oneios_cpu_capped_ns(70 * S, busy, 8), span) == 700);   // a lump: within the busy time
  CHECK(oneios_cpu_percent(oneios_cpu_capped_ns(90 * S, busy, 8), span) == 740);   // never more than the CPUs did
  CHECK(oneios_cpu_percent(oneios_cpu_capped_ns(3 * S, busy, 8), span) == 30);
  // all idle, or more idle than the span (a clock step): 0, never a wrapped number
  CHECK(oneios_cpu_busy_ns(span, 8, 80 * S) == 0 && oneios_cpu_busy_ns(span, 8, 99 * S) == 0);
  // the report thread woke late: the measured 30 s, not the nominal 10 (the 2026-11-13 cause)
  CHECK(oneios_cpu_span_ns(30 * S, 10) == 30 * S && oneios_cpu_percent(oneios_cpu_busy_ns(30 * S, 8, 0), 30 * S) == 800);
  // no measurement (under a second): the nominal span
  CHECK(oneios_cpu_span_ns(0, 10) == 10 * S && oneios_cpu_span_ns(S / 2, 10) == 10 * S);
  // a kernel without hosted 0075 (no processor count): shares are not capped
  CHECK(oneios_cpu_capped_ns(90 * S, 0, 0) == 90 * S);
  CHECK(oneios_cpu_percent(5, 0) == 0);
  printf("cpu_window: %d failed\n", failures);
  return failures != 0;
}
"#;
        let header = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/patches/oneios-hosted/gpu");
        let dir = std::env::temp_dir().join(format!("xnuhost-cpu-window-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (src, exe) = (dir.join("test_cpu_window.c"), dir.join("test_cpu_window"));
        std::fs::write(&src, CHECKS).unwrap();
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
        let built = std::process::Command::new(&cc)
            .args(["-std=c11", "-O1", "-Wall", "-Wextra", "-Werror", "-I"])
            .arg(&header)
            .arg(&src)
            .arg("-o")
            .arg(&exe)
            .status();
        match built {
            Err(e) => println!("cpu_window: no C compiler ({cc}: {e}): skipped"),
            Ok(s) => {
                assert!(s.success(), "cpu_window.h's checks do not compile");
                let out = std::process::Command::new(&exe).output().unwrap();
                let text = String::from_utf8_lossy(&out.stdout);
                assert!(out.status.success() && text.contains("cpu_window: 0 failed"), "{text}");
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
