// SPDX-License-Identifier: MIT
//! Port of task.c. iOS tasks as Linux processes (docs/booter.md, "Tasks"). Linux and AArch64 only.
//!   factory   forked from xnuhost before it has threads or the RAM mapped. It never execs, so it and every task it
//!             forks hold xnuhost's pointer-authentication keys: one key set for the kernel and all iOS tasks, which
//!             is what arm64e's shared cache needs.
//!   task      a fork of the factory that runs only the stub (runtime/xnuhost/stub): empty address space, a service
//!             thread that carries out map/unmap/protect/clone orders, and one Linux thread per iOS thread whose
//!             system calls and faults arrive as signals.
//! Darling (https://github.com/darlinghq/darling) serves the same calls another way: its own libsystem_kernel calls
//! darlingserver over a socket. iOS's libsystem_kernel is Apple's and issues `svc #0x80` itself, so here the call is
//! trapped (seccomp, SIGSYS) and the transport is a shared page and two futex words.

use xnuhost_abi::{XsFilterInsn, XS_FILTER_MAX};

/// The hand-off's spin defaults (spin-host, spin-task), in microseconds: docs/booter.md 2.6a has the measurement.
pub const SPIN_HOST_US: u32 = 0;
pub const SPIN_TASK_US: u32 = 0;

// ---- the seccomp filter (portable: unit-tested on the build host) ------------------------------------------------
const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
const BPF_JGE_K: u16 = 0x35;
const BPF_RET_K: u16 = 0x06;
const SECCOMP_RET_TRAP: u32 = 0x0003_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const AUDIT_ARCH_AARCH64: u32 = 0xc000_00b7;

/// Where a filter instruction jumps: the next one, or one of the labels.
#[derive(Clone, Copy, PartialEq, Eq)]
enum To {
    Next,
    Trap,
    Allow,
    Kill,
    List,
}

/// Every system call traps (SIGSYS), except the stub's own: those whose instruction pointer is inside the stub's text
/// and whose number the stub uses. mmap from the stub must name one of the two memfds or no file. iOS code issues
/// `svc #0x80` with XNU's number in x16; Linux would read x8 and run something else, so nothing issued from outside
/// the stub may reach Linux. Returns the instruction count, 0 when the text crosses a 4 GiB line.
pub fn filter(f: &mut [XsFilterInsn; XS_FILTER_MAX], text_lo: u64, text_hi: u64, ram_fd: i32, trap_fd: i32) -> u32 {
    const ALLOWED: [u32; 11] = [
        98,  // futex
        93,  // exit
        94,  // exit_group
        132, // sigaltstack
        139, // rt_sigreturn
        167, // prctl
        172, // getpid
        178, // gettid
        215, // munmap
        220, // clone
        226, // mprotect
    ];
    if (text_lo >> 32) != ((text_hi - 1) >> 32) {
        return 0; // the text must not cross a 4 GiB line
    }
    let mut prog: Vec<(u16, To, To, u32)> = Vec::with_capacity(XS_FILTER_MAX);
    let mut insn = |code: u16, t: To, fl: To, k: u32| prog.push((code, t, fl, k));
    insn(BPF_LD_W_ABS, To::Next, To::Next, 4); // architecture
    insn(BPF_JEQ_K, To::Next, To::Kill, AUDIT_ARCH_AARCH64);
    insn(BPF_LD_W_ABS, To::Next, To::Next, 12); // instruction pointer, high word
    insn(BPF_JEQ_K, To::Next, To::Trap, (text_lo >> 32) as u32);
    insn(BPF_LD_W_ABS, To::Next, To::Next, 8); // instruction pointer, low word
    insn(BPF_JGE_K, To::Next, To::Trap, text_lo as u32);
    insn(BPF_JGE_K, To::Trap, To::Next, text_hi as u32);
    insn(BPF_LD_W_ABS, To::Next, To::Next, 0); // system call number
    insn(BPF_JEQ_K, To::Next, To::List, 222); // mmap: its file descriptor is argument 4
    insn(BPF_LD_W_ABS, To::Next, To::Next, 16 + 4 * 8);
    insn(BPF_JEQ_K, To::Allow, To::Next, ram_fd as u32);
    insn(BPF_JEQ_K, To::Allow, To::Next, trap_fd as u32);
    insn(BPF_JEQ_K, To::Allow, To::Trap, 0xffff_ffff); // or anonymous memory
    let list = prog.len();
    prog.push((BPF_LD_W_ABS, To::Next, To::Next, 0));
    for nr in ALLOWED {
        prog.push((BPF_JEQ_K, To::Allow, To::Next, nr));
    }
    let trap = prog.len();
    prog.push((BPF_RET_K, To::Next, To::Next, SECCOMP_RET_TRAP));
    let allow = prog.len();
    prog.push((BPF_RET_K, To::Next, To::Next, SECCOMP_RET_ALLOW));
    let kill = prog.len();
    prog.push((BPF_RET_K, To::Next, To::Next, SECCOMP_RET_KILL_PROCESS));
    let at = |to: To, i: usize| -> u8 {
        match to {
            To::Next => 0,
            To::Trap => (trap - (i + 1)) as u8,
            To::Allow => (allow - (i + 1)) as u8,
            To::Kill => (kill - (i + 1)) as u8,
            To::List => (list - (i + 1)) as u8,
        }
    };
    for (i, &(code, t, fl, k)) in prog.iter().enumerate() {
        f[i] = XsFilterInsn { code, jt: at(t, i), jf: at(fl, i), k };
    }
    prog.len() as u32
}

// ---- the tasks' lasting line (portable: unit-tested) ---------------------------------------------------------------
/// The tasks' counts (docs/booter.md "Health lines"): made, refused, ended by the kernel, died on their own, fatal
/// traps; live now and the most at once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskCounts {
    pub made: u64,
    pub refused: u64,
    pub ended: u64,
    pub died: u64,
    pub fatal: u64,
    pub live: u32,
    pub most: u32,
}

/// One line when a count moved since `before`, every 60 s:
///   tasks: 60 s: +12 made, +10 ended by the kernel, +1 died on their own, +2 fatal traps, +0 refused; live 187 (most 190)
/// "died on their own": the Linux process of an iOS task ended without the kernel's destroy (each one also has a line
/// of its own, the first 32).
pub fn health_line(now: &TaskCounts, before: &TaskCounts, seconds: u64) -> Option<String> {
    let made = now.made - before.made;
    let ended = now.ended - before.ended;
    let died = now.died - before.died;
    let fatal = now.fatal - before.fatal;
    let refused = now.refused - before.refused;
    if made == 0 && ended == 0 && died == 0 && fatal == 0 && refused == 0 {
        return None;
    }
    Some(format!(
        "tasks: {seconds} s: +{made} made, +{ended} ended by the kernel, +{died} died on their own, +{fatal} fatal traps, \
         +{refused} refused; live {} (most {})",
        now.live, now.most
    ))
}

// ---- the trim (docs/booter.md 2.6, "The trim"; portable: unit-tested) -------------------------------------------
/// Whether a task is trimmed now: none of its threads is released (`running` 0), it has not been trimmed since its last
/// release, and that release is `idle` counter ticks old or more.
pub fn trim_due(now: u64, last_release: u64, running: u32, trimmed: bool, idle: u64) -> bool {
    running == 0 && !trimmed && now.saturating_sub(last_release) >= idle
}

/// The idle time a task needs before it is trimmed, seconds: task-trim-idle, under the host's pressure (MemAvailable
/// under the headroom and TRIM_PRESSURE_MARGIN) at most TASK_TRIM_IDLE_PRESSED; never under 1.
pub fn trim_idle_s(idle: u32, pressed: bool) -> u32 {
    if pressed { idle.min(crate::config::TASK_TRIM_IDLE_PRESSED) } else { idle }.max(1)
}

/// What the trim did, summed from the session's start (the trim's 60 s line).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrimCounts {
    /// tasks trimmed, of them under pressure, and orders that failed (not counting a task that died meanwhile)
    pub trims: u64,
    pub pressed: u64,
    pub failed: u64,
    /// the trimmed tasks' page tables (VmPTE) before and after, bytes
    pub pte_before: u64,
    pub pte_after: u64,
    /// the unmaps' time, ns
    pub ns: u64,
}

/// What the trim saw at its last look: the live tasks, those with a release in the last task-trim-idle seconds
/// ("awake"), and those trimmed and not released since.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrimLook {
    pub live: u32,
    pub awake: u32,
    pub trimmed: u32,
}

/// One line every 60 s while the trim is on (task-trim=1) and there are tasks:
///   tasks: trim: 60 s: +41 trimmed (+0 under pressure, 0 failed), page tables 83 MiB given back (2073 KiB a task, 12
///   KiB left), the unmaps 60 ms (1487 us a task, most 5 ms); now 312 live: 97 awake (a release in the last 30 s; most
///   140 since the last line), 188 trimmed
/// `most_awake` is the most awake tasks at any look since the last line: the figure ram-tasks-awake (config.rs) is for.
pub fn trim_line(
    now: &TrimCounts,
    before: &TrimCounts,
    seconds: u64,
    look: &TrimLook,
    most_awake: u32,
    most_ns: u64,
    idle_s: u32,
) -> Option<String> {
    let trims = now.trims - before.trims;
    if trims == 0 && look.live == 0 {
        return None;
    }
    let (pte_before, pte_after) = (now.pte_before - before.pte_before, now.pte_after - before.pte_after);
    let freed = pte_before.saturating_sub(pte_after);
    let ns = now.ns - before.ns;
    Some(format!(
        "tasks: trim: {seconds} s: +{trims} trimmed (+{} under pressure, {} failed), page tables {} MiB given back ({} KiB \
         a task, {} KiB left), the unmaps {} ms ({} us a task, most {} ms); now {} live: {} awake (a release in the last \
         {idle_s} s; most {most_awake} since the last line), {} trimmed",
        now.pressed - before.pressed,
        now.failed - before.failed,
        freed >> 20,
        (freed >> 10) / trims.max(1),
        (pte_after >> 10) / trims.max(1),
        ns / 1_000_000,
        ns / 1000 / trims.max(1),
        most_ns / 1_000_000,
        look.live,
        look.awake,
        look.trimmed
    ))
}

/// SIGKILL to an iOS task's Linux process, only to a pid of one (> 0): kill(0, ...) is the caller's own process
/// group (from xnuhost: xnuhost itself, without a word), and kill(-1, ...) as root every process of the phone. The
/// task table's pid is 0 while a slot is being made again, so a stale order read there would be that (a race: not
/// seen in a log; the phone's session of 2026-11-23 19:06 ended by a signal nothing logged). Returns whether it sent.
pub fn kill_task(pid: i32, kill: impl FnOnce(i32)) -> bool {
    if pid <= 0 {
        return false;
    }
    kill(pid);
    true
}

/// uclamp.min and uclamp.max of a Linux thread (sched_setattr(2), Linux 5.3): `flags` says which are set; the policy
/// and nice kept (KEEP_POLICY | KEEP_PARAMS in flags), SCHED_OTHER stays SCHED_OTHER (never a realtime policy:
/// CLAUDE.md, the way back to One UI). 0, or -1 with errno.
pub fn sched_setattr_uclamp(tid: i32, flags: u64, lo: u32, hi: u32) -> i32 {
    #[repr(C)]
    struct Attr {
        size: u32,
        policy: u32,
        flags: u64,
        nice: i32,
        priority: u32,
        runtime: u64,
        deadline: u64,
        period: u64,
        util_min: u32,
        util_max: u32,
    }
    let attr = Attr {
        size: std::mem::size_of::<Attr>() as u32,
        policy: 0,
        flags,
        nice: 0,
        priority: 0,
        runtime: 0,
        deadline: 0,
        period: 0,
        util_min: lo.min(1024),
        util_max: hi.min(1024),
    };
    // SAFETY: sched_setattr with a complete attr of its stated size.
    unsafe { libc::syscall(libc::SYS_sched_setattr, tid, &attr as *const Attr, 0) as i32 }
}

pub use machine::*;

#[cfg(all(xh_machine, not(test)))]
mod machine {
    use super::*;
    use crate::config::Config;
    use crate::cpu::{self, Vcpu};
    use crate::perf::{self, Count, Step};
    use crate::{mem, session};
    use std::ptr::{self, addr_of_mut};
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering::*};
    use std::sync::{Condvar, Mutex, MutexGuard};
    use std::time::{Duration, Instant};
    use xh_core::sys::{errno, strerror};
    use xh_core::{loge, logi};
    use xnuhost_abi::*;

    const MAX_TASKS: usize = 1024;
    /// trap memfd slots: threads and service pages
    const MAX_SLOTS: usize = 8192;
    /// deferred maps and unmaps a task holds before they are ordered
    const PENDING_MAX: usize = 256;
    const AREA_BYTES: usize = XS_MAX_THREADS as usize / 8 + 1;
    fn kick_signal() -> i32 {
        cpu::sigrtmin() + 3
    }

    const ZERO_OP: XsOp = XsOp { op: 0, prot: 0, va: 0, size: 0, offset: 0, result: 0 };

    /// map-defer: task_map orders not sent yet. They go with the next release of one of the task's threads (carried
    /// out by that thread in the stub's handler before it runs) or at the head of the task's next order, whichever is
    /// first, so they keep their place before any later unmap or protect. Under `qlock`, which is held only to add to
    /// or take the queue: a task_map does not wait for a release whose thread is carrying out earlier maps (`lock`,
    /// about 100 us on the phone, while the kernel's caller holds the task's TLB lock with interrupts masked). A taker
    /// holds both.
    struct Queue {
        pending: [XsOp; PENDING_MAX],
        /// Unmaps in the queue.
        nunmaps: u32,
    }

    /// What `lock` guards: the vCPU + 1 its service thread was last put with (the place hook), and that epoch.
    struct Placed {
        vcpu: u8,
        epoch: u8,
    }

    struct Task {
        used: AtomicBool,
        dead: AtomicBool,
        /// destroyed by the kernel, its process not yet reported dead
        zombie: AtomicBool,
        /// the kernel destroys it (destroy): its exit is not "on its own"
        killing: AtomicBool,
        pid: AtomicI32,
        exit_status: AtomicU32,
        service_slot: AtomicU32,
        /// one order batch or one release's maps at a time
        lock: Mutex<Placed>,
        qlock: Mutex<Queue>,
        /// the queue's length, written under qlock, read by releases without it
        npending: AtomicU32,
        /// Threads of the task released and not yet back (in user code, or on their way to or from it). While it is 0
        /// nothing of the task can touch its memory, so an unmap waits in the queue for the next release (which
        /// carries it out before the thread runs) or the next order: the kernel's flush of a page of a task that is
        /// not running costs a queue add, not a round trip. No lock on the release's side (it would cost every trip
        /// two lock pairs): a release adds 1, then reads npending and unmaps_out; an unmap adds itself to the queue
        /// (under qlock), then reads running, and takes itself back and is ordered if a thread is counted. All four
        /// are sequentially consistent, so at least one side sees the other: either the release finds the unmap and
        /// carries it, or the unmap finds the thread and is ordered.
        running: AtomicU32,
        /// Whether a taker (a release that carries the queue, or an order) has taken unmaps and not yet seen them done.
        /// Set (under qlock, before npending goes to 0) and cleared only by a taker that holds `lock` the whole time.
        /// While it is set no other thread of the task may be released (it waits for `lock`), and read/write wait
        /// too: the kernel's flush has returned, so nothing may use the old mapping meanwhile. A taker whose batch has
        /// only maps sets nothing: a map that comes late costs a fault, not a stale page.
        unmaps_out: AtomicBool,
        /// The trim (docs/booter.md 2.6, "The trim"): the counter at the last release of one of its threads (at its
        /// creation before the first), written by every release; whether its host mappings were dropped since.
        last_release: AtomicU64,
        trimmed: AtomicBool,
    }

    impl Task {
        const fn new() -> Task {
            Task {
                used: AtomicBool::new(false),
                dead: AtomicBool::new(false),
                zombie: AtomicBool::new(false),
                killing: AtomicBool::new(false),
                pid: AtomicI32::new(0),
                exit_status: AtomicU32::new(0),
                service_slot: AtomicU32::new(0),
                lock: Mutex::new(Placed { vcpu: 0, epoch: 0 }),
                qlock: Mutex::new(Queue { pending: [ZERO_OP; PENDING_MAX], nunmaps: 0 }),
                npending: AtomicU32::new(0),
                running: AtomicU32::new(0),
                unmaps_out: AtomicBool::new(false),
                last_release: AtomicU64::new(0),
                trimmed: AtomicBool::new(false),
            }
        }
        fn dead(&self) -> bool {
            self.dead.load(Acquire)
        }
        fn pid(&self) -> i32 {
            self.pid.load(Relaxed)
        }
        fn lock(&self) -> MutexGuard<'_, Placed> {
            self.lock.lock().unwrap_or_else(|e| e.into_inner())
        }
        fn qlock(&self) -> MutexGuard<'_, Queue> {
            self.qlock.lock().unwrap_or_else(|e| e.into_inner())
        }
        fn index(&self) -> usize {
            (self as *const Task as usize - TASKS.as_ptr() as usize) / std::mem::size_of::<Task>()
        }
    }

    struct Thread {
        used: AtomicBool,
        task: AtomicU32,
        area: AtomicU32,
        /// vCPU + 1 it was last put with, and that epoch
        placed: AtomicU8,
        epoch: AtomicU8,
        /// the uclamp it carries, (min | max << 11) + 1 (0: never set; the uclamp hook)
        uclamp: AtomicU32,
        /// counter: its last release
        released_at: AtomicU64,
    }

    impl Thread {
        const fn new() -> Thread {
            Thread {
                used: AtomicBool::new(false),
                task: AtomicU32::new(0),
                area: AtomicU32::new(0),
                placed: AtomicU8::new(0),
                epoch: AtomicU8::new(0),
                uclamp: AtomicU32::new(0),
                released_at: AtomicU64::new(0),
            }
        }
    }

    /// Tables and the factory conversation (`T.lock`).
    struct Tables {
        slots: [u8; MAX_SLOTS / 8],
        /// thread areas in use, per task
        areas: [[u8; AREA_BYTES]; MAX_TASKS],
        reply_pid: i32,
        reply_ready: bool,
        /// the lasting line (tick); fatal is FATAL
        counts: TaskCounts,
    }

    static TASKS: [Task; MAX_TASKS] = [const { Task::new() }; MAX_TASKS];
    static THREADS: [Thread; MAX_SLOTS] = [const { Thread::new() }; MAX_SLOTS];
    static TABLES: Mutex<Tables> = Mutex::new(Tables {
        slots: [0; MAX_SLOTS / 8],
        areas: [[0; AREA_BYTES]; MAX_TASKS],
        reply_pid: 0,
        reply_ready: false,
        counts: TaskCounts { made: 0, refused: 0, ended: 0, died: 0, fatal: 0, live: 0, most: 0 },
    });
    static REPLY: Condvar = Condvar::new();
    static FATAL: AtomicU64 = AtomicU64::new(0);

    static AVAILABLE: AtomicBool = AtomicBool::new(false);
    static SECCOMP: AtomicBool = AtomicBool::new(false);
    /// map-defer (default on)
    static MAP_DEFER: AtomicBool = AtomicBool::new(true);
    /// counter ticks: spin-host, spin-task
    static SPIN_HOST: AtomicU64 = AtomicU64::new(0);
    static SPIN_TASK: AtomicU64 = AtomicU64::new(0);
    /// counter ticks: a kick not answered in this long is sent again (200 ms)
    static KICK_AGAIN: AtomicU64 = AtomicU64::new(0);
    static KICK_FAILED_SAID: AtomicBool = AtomicBool::new(false);
    /// of the stub region = the first address tasks may not use
    static BASE: AtomicU64 = AtomicU64::new(0);
    static SOCK: AtomicI32 = AtomicI32::new(-1);
    /// the whole trap memfd, mapped
    static TRAP: AtomicUsize = AtomicUsize::new(0);
    static FACTORY: AtomicI32 = AtomicI32::new(0);

    /// Changed by main's thermal step when the vCPUs' host CPUs change: each thread is placed again at its next run.
    pub static PLACE_EPOCH: AtomicU32 = AtomicU32::new(0);

    fn tables() -> MutexGuard<'static, Tables> {
        TABLES.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Message {
        kind: u32,
        pid: i32,
        slot: u32,
        status: u32,
    }
    const M_SPAWN: u32 = 1;
    const M_SPAWNED: u32 = 2;
    const M_EXITED: u32 = 3;
    const M_KILL: u32 = 4;

    fn send(sock: i32, m: &Message) -> bool {
        let n = std::mem::size_of::<Message>();
        // SAFETY: sends the message's bytes.
        unsafe { libc::send(sock, m as *const Message as *const libc::c_void, n, libc::MSG_NOSIGNAL) == n as isize }
    }

    fn recv(sock: i32) -> Option<Message> {
        let mut m = Message::default();
        let n = std::mem::size_of::<Message>();
        // SAFETY: receives into the message's bytes.
        let got = unsafe { libc::recv(sock, &mut m as *mut Message as *mut libc::c_void, n, 0) };
        (got == n as isize).then_some(m)
    }

    pub fn available() -> bool {
        AVAILABLE.load(Acquire)
    }
    /// System calls of tasks are trapped by a seccomp filter (the phone); else by text_fixup (qemu-user).
    pub fn seccomp() -> bool {
        SECCOMP.load(Acquire)
    }
    pub fn va_limit() -> u64 {
        BASE.load(Acquire)
    }

    fn futex(word: *mut u32, op: i32, value: u32, timeout: Option<&libc::timespec>) {
        let t = timeout.map_or(ptr::null(), |t| t as *const libc::timespec);
        // SAFETY: a futex call on a word of the trap memfd (mapped for the process's life); timeout is live or null.
        unsafe { libc::syscall(libc::SYS_futex, word, op, value, t, ptr::null::<u32>(), 0) };
    }

    fn slot_page(slot: u32) -> *mut u8 {
        (TRAP.load(Relaxed) + slot as usize * XS_SLOT_SIZE as usize) as *mut u8
    }
    fn trap_page(slot: u32) -> *mut XsTrapPage {
        slot_page(slot) as *mut XsTrapPage
    }
    fn service_page(slot: u32) -> *mut XsServicePage {
        slot_page(slot) as *mut XsServicePage
    }

    /// A word of a shared page as an atomic (the stub in the task process reads and writes it too).
    macro_rules! word {
        ($page:expr, $field:ident) => {
            // SAFETY: the page is a slot of the trap memfd, mapped for the process's life, aligned; the field is a
            // u32 the stub also accesses atomically.
            unsafe { AtomicU32::from_ptr(addr_of_mut!((*$page).$field)) }
        };
    }
    macro_rules! word64 {
        ($page:expr, $field:ident) => {
            // SAFETY: as for word!: an aligned u64 of a shared page.
            unsafe { AtomicU64::from_ptr(addr_of_mut!((*$page).$field)) }
        };
    }

    fn slot_alloc(t: &mut Tables) -> Option<u32> {
        let i = (1..MAX_SLOTS).find(|&i| t.slots[i / 8] & (1 << (i % 8)) == 0)?;
        t.slots[i / 8] |= 1 << (i % 8);
        // SAFETY: the slot's page is free (no thread or task uses it): ours to clear.
        unsafe { ptr::write_bytes(slot_page(i as u32), 0, XS_SLOT_SIZE as usize) };
        Some(i as u32)
    }

    fn slot_free(t: &mut Tables, slot: u32) {
        t.slots[slot as usize / 8] &= !(1 << (slot % 8));
    }

    // ---- the factory process -----------------------------------------------------------------------------------------
    fn factory_child(sock: i32, slot: u32) -> ! {
        let base = BASE.load(Relaxed);
        let config = (base + XS_CONFIG_OFF) as *mut XsConfig;
        // SAFETY: this is the forked task process: it sets its death signal, drops the factory's socket, writes its
        // slot into its own copy of the stub's config page and jumps into the stub, which never returns.
        unsafe {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            libc::close(sock);
            (*config).service_slot = slot;
            let stub: extern "C" fn() = std::mem::transmute::<usize, extern "C" fn()>(base as usize);
            stub();
            libc::_exit(72)
        }
    }

    fn factory_main(sock: i32) -> ! {
        // Single-threaded, a copy of xnuhost as it was before it mapped the RAM. Requests in, results and exits of
        // tasks out. When xnuhost goes away the socket closes and the factory and its tasks die.
        // SAFETY: only system calls from here on, in this forked process.
        unsafe {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            libc::prctl(libc::PR_SET_NAME, c"xnuhost-tasks".as_ptr());
            loop {
                let mut p = libc::pollfd { fd: sock, events: libc::POLLIN, revents: 0 };
                let n = libc::poll(&mut p, 1, 100);
                if n > 0 && p.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                    break;
                }
                if n > 0 && p.revents & libc::POLLIN != 0 {
                    let mut m = Message::default();
                    let size = std::mem::size_of::<Message>();
                    let got = libc::recv(sock, &mut m as *mut Message as *mut libc::c_void, size, 0);
                    if got == 0 {
                        break;
                    }
                    if got == size as isize && m.kind == M_SPAWN {
                        let pid = libc::fork();
                        if pid == 0 {
                            factory_child(sock, m.slot);
                        }
                        let r = Message {
                            kind: M_SPAWNED,
                            pid: if pid < 0 { -errno() } else { pid },
                            slot: m.slot,
                            status: 0,
                        };
                        send(sock, &r);
                    } else if got == size as isize && m.kind == M_KILL {
                        // Never 0 (the factory's group: every task) or -1 (every process of the phone).
                        kill_task(m.pid, |p| {
                            libc::kill(p, libc::SIGKILL);
                        });
                    }
                }
                let mut status = 0;
                loop {
                    let pid = libc::waitpid(-1, &mut status, libc::WNOHANG);
                    if pid <= 0 {
                        break;
                    }
                    send(sock, &Message { kind: M_EXITED, pid, slot: 0, status: status as u32 });
                }
            }
            libc::kill(0, libc::SIGKILL); // the factory's process group: itself and every task
            libc::_exit(0)
        }
    }

    fn mark_dead(t: &Task, status: u32) {
        t.exit_status.store(status, Relaxed);
        t.dead.store(true, Release);
        // Whoever waits on one of its pages must look again.
        let sp = service_page(t.service_slot.load(Relaxed));
        // SAFETY: field addresses of a mapped shared page.
        unsafe {
            futex(addr_of_mut!((*sp).done), libc::FUTEX_WAKE, i32::MAX as u32, None);
            futex(addr_of_mut!((*sp).ready), libc::FUTEX_WAKE, i32::MAX as u32, None);
        }
        let ti = t.index() as u32;
        for (s, th) in THREADS.iter().enumerate() {
            if th.used.load(Relaxed) && th.task.load(Relaxed) == ti {
                let tp = trap_page(s as u32);
                // SAFETY: as above.
                unsafe {
                    futex(addr_of_mut!((*tp).to_host), libc::FUTEX_WAKE, i32::MAX as u32, None);
                    futex(addr_of_mut!((*tp).tid), libc::FUTEX_WAKE, i32::MAX as u32, None);
                }
            }
        }
    }

    /// The task's slots go back to the pool. Only when its Linux process is gone: a slot is a page of the trap memfd
    /// that the process has mapped, and a thread of the old process that still waits on that page would answer the
    /// orders, or run the register state, of whoever gets the slot next. TABLES held.
    fn task_release(tb: &mut Tables, t: &Task) {
        if t.used.load(Relaxed) && tb.counts.live > 0 {
            tb.counts.live -= 1;
        }
        let ti = t.index() as u32;
        for (s, th) in THREADS.iter().enumerate() {
            if th.used.load(Relaxed) && th.task.load(Relaxed) == ti {
                th.used.store(false, Relaxed);
                slot_free(tb, s as u32);
            }
        }
        slot_free(tb, t.service_slot.load(Relaxed));
        t.used.store(false, Release);
        t.zombie.store(false, Relaxed);
    }

    fn reader_main() {
        while let Some(m) = recv(SOCK.load(Relaxed)) {
            let mut tb = tables();
            if m.kind == M_SPAWNED {
                tb.reply_pid = m.pid;
                tb.reply_ready = true;
                REPLY.notify_all();
            } else if m.kind == M_EXITED {
                for t in TASKS.iter() {
                    if !(t.used.load(Relaxed) && t.pid() == m.pid && !t.dead()) {
                        continue;
                    }
                    if !t.killing.load(Relaxed) {
                        // Not asked: the Linux process of an iOS task ended by itself (a host signal, the stub's exit).
                        // The iOS process is gone at once; the kernel finds out at its next trap or destroy.
                        let told = tb.counts.died;
                        tb.counts.died += 1;
                        if told < 32 {
                            let signal = m.status & 0x7f != 0;
                            logi!(
                                "tasks: pid {} died on its own (wait status 0x{:x}: {} {}), not asked by the kernel",
                                m.pid,
                                m.status,
                                if signal { "signal" } else { "exit" },
                                if signal { m.status & 0x7f } else { (m.status >> 8) & 0xff }
                            );
                        }
                    }
                    mark_dead(t, m.status);
                    if t.zombie.load(Relaxed) {
                        task_release(&mut tb, t);
                    }
                }
            }
        }
        if !session::stopping() {
            session::stop(XH_HALT_PANIC, 0, "the task factory is gone");
        }
    }

    /// Instruction cache maintenance for code written as data: [lo, hi).
    fn clear_cache(lo: usize, hi: usize) {
        let ctr: u64;
        // SAFETY: CTR_EL0 is readable at EL0 on Linux.
        unsafe { std::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags)) };
        let dline = 4usize << ((ctr >> 16) & 15);
        let iline = 4usize << (ctr & 15);
        // SAFETY: cache maintenance by address on our own mapping; no memory is changed.
        unsafe {
            for a in (lo & !(dline - 1)..hi).step_by(dline) {
                std::arch::asm!("dc cvau, {}", in(reg) a, options(nostack, preserves_flags));
            }
            std::arch::asm!("dsb ish", options(nostack, preserves_flags));
            for a in (lo & !(iline - 1)..hi).step_by(iline) {
                std::arch::asm!("ic ivau, {}", in(reg) a, options(nostack, preserves_flags));
            }
            std::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
        }
    }

    fn mmap(at: u64, size: usize, prot: i32, flags: i32, fd: i32) -> Option<usize> {
        // SAFETY: a new mapping; callers pass MAP_FIXED_NOREPLACE or no address.
        let p = unsafe { libc::mmap(at as usize as *mut libc::c_void, size, prot, flags, fd, 0) };
        (p != libc::MAP_FAILED).then_some(p as usize)
    }

    fn mprotect(at: usize, size: u64, prot: i32) -> bool {
        // SAFETY: protection of a part of the stub region, ours.
        unsafe { libc::mprotect(at as *mut libc::c_void, size as usize, prot) == 0 }
    }

    /// Forks the task factory. Must run before any other thread exists and before RAM is mapped.
    pub fn init(c: &Config) -> Result<(), crate::mem::SetupFailed> {
        let blob = crate::arch::STUB;
        let text = blob.len();
        if text == 0 || text as u64 > XS_TEXT_MAX {
            loge!("tasks: bad stub blob ({} bytes)", text);
            return Err(crate::mem::SetupFailed);
        }
        // The trap memfd is held for the process's life (the stub and every task map it): its descriptor is kept raw.
        let trap_fd = match xh_core::fd::memfd(c"xnuhost-traps", 0) {
            Ok(fd) => std::os::fd::IntoRawFd::into_raw_fd(fd),
            Err(e) => {
                loge!("tasks: trap memfd: {}", xh_core::sys::error_text(&e));
                return Err(crate::mem::SetupFailed);
            }
        };
        // SAFETY: our new descriptor.
        if unsafe { libc::ftruncate(trap_fd, (MAX_SLOTS as u64 * XS_SLOT_SIZE) as libc::off_t) } != 0 {
            loge!("tasks: trap memfd: {}", strerror(errno()));
            return Err(crate::mem::SetupFailed);
        }
        let Some(trap) =
            mmap(0, MAX_SLOTS * XS_SLOT_SIZE as usize, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, trap_fd)
        else {
            loge!("tasks: map traps: {}", strerror(errno()));
            return Err(crate::mem::SetupFailed);
        };
        TRAP.store(trap, Release);

        // The stub region: high in the address space, below the host's own mappings where it can be.
        let candidates = [0x7e_0000_0000u64, 0x7d_0000_0000, 0x7c_0000_0000, 0x70_0000_0000, 0x60_0000_0000];
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE | crate::mem::MAP_FIXED_NOREPLACE;
        let region = candidates.iter().find_map(|&at| {
            let p = mmap(at, XS_REGION_SIZE as usize, libc::PROT_NONE, flags, -1)?;
            if p as u64 != at {
                // SAFETY: our reservation, at the wrong place.
                unsafe { libc::munmap(p as *mut libc::c_void, XS_REGION_SIZE as usize) };
                return None;
            }
            Some(p)
        });
        let Some(b) = region else {
            loge!("tasks: no room for the stub region: {}", strerror(errno()));
            return Err(crate::mem::SetupFailed);
        };
        let base = b as u64;
        BASE.store(base, Release);
        let rw = libc::PROT_READ | libc::PROT_WRITE;
        if !mprotect(b, XS_TEXT_MAX, rw)
            || !mprotect(b + XS_CONFIG_OFF as usize, 0x1000, rw)
            || !mprotect(b + XS_BOOTSTACK_OFF as usize, XS_BOOTSTACK_SIZE, rw)
        {
            loge!("tasks: stub region: {}", strerror(errno()));
            return Err(crate::mem::SetupFailed);
        }
        // SAFETY: the region's text part is ours and writable now (above).
        unsafe { ptr::copy_nonoverlapping(blob.as_ptr(), b as *mut u8, text) };
        if !mprotect(b, XS_TEXT_MAX, libc::PROT_READ | libc::PROT_EXEC) {
            loge!("tasks: stub text not executable: {}", strerror(errno()));
            return Err(crate::mem::SetupFailed);
        }
        clear_cache(b, b + text);

        let config = (b + XS_CONFIG_OFF as usize) as *mut XsConfig;
        let m = mem::state();
        // SAFETY: the config page is ours and writable (above); fields are written in place.
        let cfg = unsafe { &mut *config };
        cfg.base = base;
        cfg.ram_fd = m.fd;
        cfg.trap_fd = trap_fd;
        cfg.kick_signal = kick_signal() as u32;
        cfg.uid = c.task_uid.map_or(-1, |u| u as i32);
        // The hand-off (xnuhost-abi's stub.rs): how long each side spins before it sleeps. Measured with the bench of
        // xnuhost/check-arm64.sh (docs/booter.md 2.6a): the defaults are what that measurement chose.
        let hz = cpu::counter_hz();
        SPIN_HOST.store(u64::from(c.spin_host.unwrap_or(SPIN_HOST_US)) * hz / 1_000_000, Relaxed);
        KICK_AGAIN.store(hz / 5, Relaxed);
        SPIN_TASK.store(u64::from(c.spin_task.unwrap_or(SPIN_TASK_US)) * hz / 1_000_000, Relaxed);
        MAP_DEFER.store(c.map_defer, Relaxed);
        // Is the filter usable here? (qemu-user and some kernels have no seccomp.) Asked in a child so that xnuhost
        // itself never carries a filter.
        let mut seccomp = false;
        // SAFETY: the child makes two system calls and exits.
        let probe = unsafe { libc::fork() };
        if probe == 0 {
            let allow = XsFilterInsn { code: BPF_RET_K, jt: 0, jf: 0, k: SECCOMP_RET_ALLOW };
            #[repr(C)]
            struct Prog {
                len: u16,
                filter: *const XsFilterInsn,
            }
            let prog = Prog { len: 1, filter: &allow };
            // SAFETY: as above.
            unsafe {
                libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
                let r = libc::syscall(libc::SYS_seccomp, 1, 0, &prog as *const Prog);
                libc::_exit(if r == 0 { 0 } else { 1 })
            }
        }
        let mut status = 0;
        // SAFETY: waits for our child.
        if probe > 0 && unsafe { libc::waitpid(probe, &mut status, 0) } == probe {
            seccomp = libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
        }
        cfg.filter_length = if seccomp { filter(&mut cfg.filter, base, base + text as u64, m.fd, trap_fd) } else { 0 };
        if seccomp && cfg.filter_length == 0 {
            loge!("tasks: filter construction failed");
            return Err(crate::mem::SetupFailed);
        }
        if !seccomp {
            logi!(
                "tasks: no seccomp filter on this host: `svc #0x80` is rewritten in text the kernel maps (text_fixup) and \
                 trapped as an undefined instruction; a svc in other code would reach the host"
            );
            xh_core::insn::TEXT_FIXUP_SVC.store(true, Relaxed);
        }

        let mut pair = [0i32; 2];
        // SAFETY: a socket pair into our array.
        if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, pair.as_mut_ptr()) } != 0 {
            loge!("tasks: socketpair: {}", strerror(errno()));
            return Err(crate::mem::SetupFailed);
        }
        // SAFETY: xnuhost is single-threaded here (the rule above); the child never returns from factory_main.
        let factory = unsafe { libc::fork() };
        if factory < 0 {
            loge!("tasks: fork: {}", strerror(errno()));
            return Err(crate::mem::SetupFailed);
        }
        if factory == 0 {
            // SAFETY: the factory's own copies of the descriptors.
            unsafe {
                libc::close(pair[0]);
                libc::setpgid(0, 0);
            }
            factory_main(pair[1]);
        }
        // SAFETY: our descriptors.
        unsafe {
            libc::close(pair[1]);
            libc::fcntl(pair[0], libc::F_SETFD, libc::FD_CLOEXEC);
        }
        SOCK.store(pair[0], Release);
        FACTORY.store(factory, Release);
        // xnuhost keeps the region reserved but needs none of it; leave it mapped so nothing else lands there.
        if std::thread::Builder::new().name("xnuhost-taskrd".into()).spawn(reader_main).is_err() {
            return Err(crate::mem::SetupFailed);
        }
        AVAILABLE.store(true, Release);
        SECCOMP.store(seccomp, Release);
        logi!(
            "tasks: factory pid {}, stub region 0x{:x}, stub {} bytes, filter {} instructions; hand-off spins {} us (vCPU) \
             and {} us (iOS thread), maps {}",
            factory,
            base,
            text,
            cfg.filter_length,
            SPIN_HOST.load(Relaxed) * 1_000_000 / hz,
            SPIN_TASK.load(Relaxed) * 1_000_000 / hz,
            if c.map_defer { "carried by the next release (map-defer=1)" } else { "ordered one by one (map-defer=0)" }
        );
        if c.task_trim {
            TRIM_IDLE_S.store(c.task_trim_idle.max(1), Relaxed);
            TRIM_ON.store(true, Relaxed);
            if std::thread::Builder::new().name("xnuhost-trim".into()).spawn(trim_main).is_err() {
                TRIM_ON.store(false, Relaxed);
                loge!("tasks: trim: no thread: idle tasks keep their host mappings");
            } else {
                logi!(
                    "tasks: trim: a task with no thread released for {} s has its host mappings dropped (task-trim=1): \
                     their page tables and VMAs go back to the host, and it faults in again what it touches; tasks idle \
                     {} s when host MemAvailable is under the headroom and {} MiB",
                    c.task_trim_idle.max(1),
                    trim_idle_s(c.task_trim_idle, true),
                    crate::config::TRIM_PRESSURE_MARGIN >> 20
                );
            }
        }
        Ok(())
    }

    /// Every 60 s from the main loop: the tasks' line when a count moved (health_line).
    pub fn tick() {
        static LAST: Mutex<(u64, TaskCounts)> =
            Mutex::new((0, TaskCounts { made: 0, refused: 0, ended: 0, died: 0, fatal: 0, live: 0, most: 0 }));
        let now = xh_core::time::monotonic_ns() / 1_000_000_000;
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if last.0 == 0 {
            last.0 = now;
            return;
        }
        if now - last.0 < 60 {
            return;
        }
        let mut c = tables().counts;
        c.fatal = FATAL.load(Relaxed);
        if let Some(line) = health_line(&c, &last.1, now - last.0) {
            logi!("{}", line);
        }
        if TRIM_ON.load(Relaxed) {
            let mut s = trim_state();
            let idle_s = TRIM_IDLE_S.load(Relaxed);
            if let Some(line) = trim_line(&s.counts, &s.said, now - last.0, &s.look, s.most_awake, s.most_ns, idle_s) {
                logi!("{}", line);
            }
            s.said = s.counts;
            s.most_awake = s.look.awake;
            s.most_ns = 0;
        }
        *last = (now, c);
    }

    // ---- the trim (docs/booter.md 2.6, "The trim") -------------------------------------------------------------------
    // Every iOS task is a Linux process, and each page the kernel fills for it is an mmap of one 16 KiB page of the RAM
    // memfd there (task_map): one VMA (pages next to each other in both the address and the memfd merge, most do not)
    // and a share of 4 KiB page tables. The host keeps all of it as long as the task lives, idle or not: on the phone
    // (2026-11-23) page tables 2.0-2.7 MiB a task (683-818 MiB for 204-353 tasks) and about as much again of kernel
    // memory no line named, 3.9 MiB a task in all, which ram=auto had to leave beside the guest. These mappings are the
    // pmap's TLB (hosted_pmap.h): dropping them is always allowed, and a page the task touches again faults and is
    // filled from the tables (the kernel's flush of a whole pmap does the same, hosted_tlb_flush_pmap). So a task none
    // of whose threads has been released for task-trim-idle seconds gets one unmap of its whole iOS range [0, the stub
    // region) from its service thread: Linux frees its VMAs and page tables there at once.

    /// task-trim; its idle time (seconds); the host near its headroom (memory.rs's pressure look: then 2 s).
    static TRIM_ON: AtomicBool = AtomicBool::new(false);
    static TRIM_IDLE_S: AtomicU32 = AtomicU32::new(crate::config::TASK_TRIM_IDLE);
    static TRIM_PRESSED: AtomicBool = AtomicBool::new(false);

    struct TrimState {
        counts: TrimCounts,
        /// the counts at the last line
        said: TrimCounts,
        look: TrimLook,
        /// the most awake tasks at a look, and the slowest unmap (ns), since the last line
        most_awake: u32,
        most_ns: u64,
        first_said: bool,
    }
    const NO_TRIMS: TrimCounts = TrimCounts { trims: 0, pressed: 0, failed: 0, pte_before: 0, pte_after: 0, ns: 0 };
    static TRIM: Mutex<TrimState> = Mutex::new(TrimState {
        counts: NO_TRIMS,
        said: NO_TRIMS,
        look: TrimLook { live: 0, awake: 0, trimmed: 0 },
        most_awake: 0,
        most_ns: 0,
        first_said: false,
    });
    fn trim_state() -> MutexGuard<'static, TrimState> {
        TRIM.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The main loop's pressure look (session/memory.rs): the host is near its headroom, or no longer.
    pub fn trim_pressure(pressed: bool) {
        TRIM_PRESSED.store(pressed, Relaxed);
    }

    /// The trim's thread, `xnuhost-trim`: nice 10 (below the vCPUs, never realtime), a look every 5 s (every second
    /// when the idle time is shorter: pressure, or the test kernel's task-trim-idle=1). A look reads each live task's
    /// atomics; a trim costs a read of the task's /proc status before and after and one order.
    fn trim_main() {
        // SAFETY: gettid and setpriority of this thread.
        unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10) };
        let hz = cpu::counter_hz().max(1);
        loop {
            let pressed = TRIM_PRESSED.load(Relaxed);
            let idle_s = trim_idle_s(TRIM_IDLE_S.load(Relaxed), pressed);
            std::thread::sleep(Duration::from_secs(u64::from(idle_s.min(5))));
            if SOCK.load(Acquire) < 0 {
                return; // the stop
            }
            let idle = u64::from(idle_s) * hz;
            let awake_for = u64::from(TRIM_IDLE_S.load(Relaxed)) * hz;
            let mut look = TrimLook::default();
            for t in TASKS.iter() {
                if !t.used.load(Acquire) || t.dead() || t.killing.load(Relaxed) || t.pid() <= 0 {
                    continue;
                }
                look.live += 1;
                let now = cpu::counter();
                let last = t.last_release.load(Relaxed);
                if now.saturating_sub(last) < awake_for {
                    look.awake += 1;
                }
                if trim_due(now, last, t.running.load(SeqCst), t.trimmed.load(Relaxed), idle) {
                    trim_one(t, (now - last) / hz, pressed);
                }
                look.trimmed += t.trimmed.load(Relaxed) as u32;
            }
            let mut s = trim_state();
            s.look = look;
            s.most_awake = s.most_awake.max(look.awake);
        }
    }

    /// One trim: the task's whole iOS range unmapped by its service thread (an order: the deferred maps and unmaps go
    /// first, then this one undoes them too). A release that comes meanwhile runs into faults and fills, as after any
    /// flush; `trimmed` is set before the order, so such a release clears it and the task counts as awake.
    fn trim_one(t: &Task, idle_s: u64, pressed: bool) {
        let pid = t.pid();
        let status = format!("/proc/{pid}/status");
        let pte = || xh_core::fs::proc_values(&status, &["VmPTE"])[0].unwrap_or(0);
        let before = pte();
        t.trimmed.store(true, Relaxed);
        let began = Instant::now();
        let rc = order_as(t, &[XsOp { op: XS_OP_UNMAP, va: 0, size: BASE.load(Relaxed), ..ZERO_OP }], false);
        let ns = began.elapsed().as_nanos() as u64;
        let mut s = trim_state();
        if rc < 0 {
            // A task that died meanwhile is no failure. Either way it is not tried again before its next release.
            if rc != neg(libc::ESRCH) {
                s.counts.failed += 1;
                if s.counts.failed <= 4 {
                    loge!("tasks: trim: pid {}: the unmap of its iOS range failed: {}", pid, rc);
                }
            }
            return;
        }
        drop(s);
        let after = pte();
        let mut s = trim_state();
        s.counts.trims += 1;
        s.counts.pressed += pressed as u64;
        s.counts.pte_before += before;
        s.counts.pte_after += after.min(before);
        s.counts.ns += ns;
        s.most_ns = s.most_ns.max(ns);
        if !s.first_said {
            s.first_said = true;
            logi!(
                "tasks: trim: the first: pid {} after {} s with no release: page tables {} KiB -> {} KiB (VmPTE), the unmap {} us",
                pid,
                idle_s,
                before >> 10,
                after >> 10,
                ns / 1000
            );
        }
    }

    /// The iOS tasks alive now (the host's pressure line, session/memory.rs).
    pub fn live() -> u32 {
        tables().counts.live
    }

    pub fn shutdown() {
        let sock = SOCK.swap(-1, AcqRel);
        if sock < 0 {
            return;
        }
        for t in TASKS.iter().filter(|t| t.used.load(Relaxed) && !t.dead()) {
            send(sock, &Message { kind: M_KILL, pid: t.pid(), slot: 0, status: 0 });
        }
        // SAFETY: our socket: the factory sees the hang-up and kills its group.
        unsafe {
            libc::shutdown(sock, libc::SHUT_RDWR);
            libc::close(sock);
        }
        let factory = FACTORY.load(Relaxed);
        if factory > 0 {
            let mut status = 0;
            for _ in 0..50 {
                // SAFETY: waits for our child without blocking.
                if unsafe { libc::waitpid(factory, &mut status, libc::WNOHANG) } != 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            // SAFETY: our child.
            unsafe { libc::kill(factory, libc::SIGKILL) };
        }
    }

    // ---- orders to a task's service thread ---------------------------------------------------------------------------
    fn task_of(handle: u64) -> Option<&'static Task> {
        if handle == 0 || handle > MAX_TASKS as u64 {
            return None;
        }
        let t = &TASKS[handle as usize - 1];
        t.used.load(Acquire).then_some(t)
    }

    fn neg(e: i32) -> i64 {
        -(e as i64)
    }

    /// Waits until the word is not 0, the task is dead, or the time is up: 0, -ESRCH, -ETIMEDOUT.
    fn wait_word(t: &Task, word: *mut u32, seconds: u64) -> i64 {
        // By the clock, not by the number of waits: a futex wait that a signal interrupts (the vCPU threads get them all
        // the time: kicks, timers) returns at once, and a hundred of those are not ten seconds.
        let until = Instant::now() + Duration::from_secs(seconds);
        // SAFETY: a word of a mapped shared page.
        let w = unsafe { AtomicU32::from_ptr(word) };
        loop {
            if w.load(Acquire) != 0 {
                return 0;
            }
            if t.dead() {
                return neg(libc::ESRCH);
            }
            if Instant::now() >= until {
                return neg(libc::ETIMEDOUT);
            }
            futex(word, libc::FUTEX_WAIT, 0, Some(&libc::timespec { tv_sec: 0, tv_nsec: 100_000_000 }));
        }
    }

    /// What every Linux thread of a task process is doing (state and system call from /proc), for the log, when the
    /// process does not answer: "tid state syscall-number first-arguments".
    fn log_task_threads(pid: i32) {
        let dir = match std::fs::read_dir(format!("/proc/{pid}/task")) {
            Ok(d) => d,
            Err(e) => {
                loge!("tasks: pid {}: no /proc entry ({})", pid, xh_core::sys::error_text(&e));
                return;
            }
        };
        for e in
            dir.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(|c: char| c.is_ascii_digit())).take(24)
        {
            let name = e.file_name().to_string_lossy().into_owned();
            let state = std::fs::read_to_string(format!("/proc/{pid}/task/{name}/stat"))
                .ok()
                .and_then(|l| l.rfind(") ").and_then(|i| l[i + 2..].chars().next()))
                .unwrap_or('?');
            let call = std::fs::read_to_string(format!("/proc/{pid}/task/{name}/syscall"))
                .ok()
                .map(|l| l.chars().take(79).collect::<String>().lines().next().unwrap_or("").to_owned())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| "?".to_owned());
            loge!("tasks: pid {} thread {}: state {}, syscall {}", pid, name, state, call);
        }
    }

    fn log_map_failure(t: &Task, op: &XsOp, how: &str) {
        static TOLD: AtomicU32 = AtomicU32::new(0);
        if op.result < 0 && TOLD.fetch_add(1, Relaxed) < 16 {
            loge!(
                "tasks: pid {}: a deferred {} (va 0x{:x}, size 0x{:x}, prot {}) failed {}: {}",
                t.pid(),
                if op.op == XS_OP_UNMAP { "unmap" } else { "map" },
                op.va,
                op.size,
                op.prot,
                how,
                op.result
            );
        }
    }

    /// A taker's queued unmaps are done (or its task is gone): other releases of the task may go again. `lock` held.
    fn unmaps_done(t: &Task) {
        if t.unmaps_out.load(Relaxed) {
            t.unmaps_out.store(false, SeqCst);
        }
    }

    /// Copies `ops` to the shared ops array at `at`.
    fn put_ops(dst: *mut XsOp, ops: &[XsOp]) {
        // SAFETY: dst is an ops array of a slot page with room (the callers check the counts).
        unsafe { ptr::copy_nonoverlapping(ops.as_ptr(), dst, ops.len()) };
    }

    fn get_op(src: *const XsOp, i: usize) -> XsOp {
        // SAFETY: an entry of an ops array of a slot page, inside its count.
        unsafe { ptr::read(src.add(i)) }
    }

    /// Takes the queue (qlock held): its entries go to `dst`; unmaps_out is set before npending goes to 0 (a release
    /// that reads npending 0 from here reads unmaps_out set). Returns how many.
    fn take_queue(t: &Task, q: &mut Queue, dst: *mut XsOp) -> usize {
        let n = t.npending.load(Relaxed) as usize;
        put_ops(dst, &q.pending[..n]);
        if q.nunmaps != 0 {
            t.unmaps_out.store(true, SeqCst);
            q.nunmaps = 0;
        }
        t.npending.store(0, SeqCst);
        n
    }

    fn order(t: &Task, ops: &[XsOp]) -> i64 {
        order_as(t, ops, true)
    }

    /// An order; `timed`: its time goes to the vCPU's perf steps (not for the trim's, which has its own line).
    fn order_as(t: &Task, ops: &[XsOp], timed: bool) -> i64 {
        let sp = service_page(t.service_slot.load(Relaxed));
        if ops.len() > XS_OPS_MAX - PENDING_MAX {
            return neg(libc::EINVAL);
        }
        let mut placed = t.lock();
        let mut q = t.qlock();
        if t.dead() {
            t.npending.store(0, SeqCst);
            q.nunmaps = 0;
            return neg(libc::ESRCH);
        }
        // The deferred maps and unmaps first, in their order: an unmap or protect after them must find them done.
        // SAFETY: the ops array of the task's service page.
        let sp_ops = unsafe { addr_of_mut!((*sp).ops) } as *mut XsOp;
        let first = take_queue(t, &mut q, sp_ops);
        drop(q);
        // A flush (ops empty) whose deferred queue was already drained by a released thread while we waited for `t.lock()`
        // has nothing left to send to the service thread (YMGPwcca/OneiOS bb950679).
        if first == 0 && ops.is_empty() {
            return 0;
        }
        // The service thread on the ordering vCPU's host CPU, as an iOS thread is (cpu-pin): the order is then a
        // switch on that core and back, not the wake-up of another core.
        let epoch = PLACE_EPOCH.load(Acquire) as u8;
        if let (Some(place), Some(v)) = (cpu::place_hook(), cpu::current()) {
            if placed.vcpu as usize != v.index() + 1 || placed.epoch != epoch {
                place(t.pid(), v.index());
                placed.vcpu = (v.index() + 1) as u8;
                placed.epoch = epoch;
            }
        }
        // SAFETY: room for first + ops (checked above: at most PENDING_MAX queued).
        put_ops(unsafe { sp_ops.add(first) }, ops);
        word!(sp, count).store((first + ops.len()) as u32, Relaxed);
        let t0 = cpu::counter();
        word!(sp, done).store(0, Release);
        word!(sp, request).store(1, Release);
        // SAFETY: a field address of the mapped service page.
        futex(unsafe { addr_of_mut!((*sp).request) }, libc::FUTEX_WAKE, 1, None);
        let began = Instant::now();
        // SAFETY: as above.
        let rc = wait_word(t, unsafe { addr_of_mut!((*sp).done) }, 10);
        let took_ms = began.elapsed().as_millis();
        // An order the task's service thread takes long over, or never answers, stalls the vCPU that gave it: say
        // which. (A flush has no order of its own: its first queued entry is named; the C read a null pointer there.)
        if rc != 0 || took_ms >= 1000 {
            let o = ops.first().copied().unwrap_or_else(|| get_op(sp_ops, 0));
            loge!(
                "tasks: pid {}: order {} (va 0x{:x}, size 0x{:x}, prot {}) {} after {} ms",
                t.pid(),
                o.op,
                o.va,
                o.size,
                o.prot,
                if rc != 0 { "was not answered" } else { "took long: answered" },
                took_ms
            );
        }
        if rc == neg(libc::ETIMEDOUT) {
            log_task_threads(t.pid());
        }
        if let Some(o) = ops.first().filter(|_| timed) {
            let step = match o.op {
                XS_OP_MAP => Some(Step::OrderMap),
                XS_OP_UNMAP | XS_OP_EXIT => Some(Step::OrderUnmap),
                XS_OP_PROTECT => Some(Step::OrderProtect),
                XS_OP_CLONE => Some(Step::OrderClone),
                _ => None,
            };
            if let Some(step) = step {
                perf::current().step(step, perf::ticks_ns(cpu::counter() - t0));
            }
        }
        let mut result = rc;
        if rc == 0 {
            for i in 0..first {
                log_map_failure(t, &get_op(sp_ops, i), "at the next order");
            }
            if let Some(r) = (0..ops.len()).map(|i| get_op(sp_ops, first + i).result).find(|&r| r < 0) {
                result = r;
            }
        }
        unmaps_done(t);
        drop(placed);
        result
    }

    static CREATING: Mutex<()> = Mutex::new(());

    pub fn create() -> Result<u64, i64> {
        if !available() && SOCK.load(Relaxed) < 0 {
            return Err(neg(libc::ENOSYS));
        }
        // One creation at a time, from the choice of the table entry to the factory's answer. The wait for the answer
        // gives TABLES up; without this, a second vCPU creating a task meanwhile took the same entry (not marked used
        // yet) and the same answer: two address spaces of the kernel then shared one handle and one Linux process (a
        // new process found the memory of another one in place), and the other process was lost.
        let _one = CREATING.lock().unwrap_or_else(|e| e.into_inner());
        let created = create_one();
        if created.is_err() {
            tables().counts.refused += 1;
        }
        created
    }

    fn create_one() -> Result<u64, i64> {
        let mut tb = tables();
        let Some(ti) = TASKS.iter().position(|t| !t.used.load(Relaxed)) else { return Err(neg(libc::EAGAIN)) };
        let Some(slot) = slot_alloc(&mut tb) else { return Err(neg(libc::EAGAIN)) };
        let t = &TASKS[ti];
        t.dead.store(false, Relaxed);
        t.zombie.store(false, Relaxed);
        t.killing.store(false, Relaxed);
        t.pid.store(0, Relaxed);
        t.exit_status.store(0, Relaxed);
        t.service_slot.store(slot, Relaxed);
        *t.lock() = Placed { vcpu: 0, epoch: 0 };
        t.qlock().nunmaps = 0;
        t.npending.store(0, Relaxed);
        t.running.store(0, Relaxed);
        t.unmaps_out.store(false, Relaxed);
        t.last_release.store(cpu::counter(), Relaxed);
        t.trimmed.store(false, Relaxed);
        tb.areas[ti] = [0; AREA_BYTES];
        tb.reply_ready = false;
        if !send(SOCK.load(Relaxed), &Message { kind: M_SPAWN, pid: 0, slot, status: 0 }) {
            slot_free(&mut tb, slot);
            return Err(neg(libc::EIO));
        }
        let until = Instant::now() + Duration::from_secs(10);
        while !tb.reply_ready {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            tb = REPLY.wait_timeout(tb, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        if !tb.reply_ready || tb.reply_pid <= 0 {
            slot_free(&mut tb, slot);
            return Err(if tb.reply_ready { tb.reply_pid as i64 } else { neg(libc::ETIMEDOUT) });
        }
        t.pid.store(tb.reply_pid, Relaxed);
        t.used.store(true, Release);
        tb.counts.made += 1;
        tb.counts.live += 1;
        tb.counts.most = tb.counts.most.max(tb.counts.live);
        drop(tb);
        let sp = service_page(slot);
        // SAFETY: a field address of the mapped service page.
        let rc = wait_word(t, unsafe { addr_of_mut!((*sp).ready) }, 10);
        // SAFETY: a field of the mapped service page, written by the stub before it set `ready` (seen above).
        let setup_error = unsafe { ptr::read_volatile(addr_of_mut!((*sp).setup_error)) };
        let handle = ti as u64 + 1;
        if rc != 0 || setup_error != 0 {
            loge!("tasks: task process {} did not set up ({}, stub error {})", t.pid(), rc, setup_error);
            destroy(handle);
            return Err(if rc != 0 { rc } else { i64::from(setup_error) });
        }
        Ok(handle)
    }

    pub fn destroy(handle: u64) -> i64 {
        let Some(t) = task_of(handle) else { return neg(libc::ESRCH) };
        // One read of the pid for every use below (kill_task: never 0 or below).
        let pid = t.pid();
        if pid <= 0 {
            loge!("tasks: destroy of handle {} with no process (pid {}): nothing killed", handle, pid);
            return neg(libc::ESRCH);
        }
        {
            let mut tb = tables();
            t.killing.store(true, Relaxed);
            tb.counts.ended += 1;
        }
        if !t.dead() {
            send(SOCK.load(Relaxed), &Message { kind: M_KILL, pid, slot: 0, status: 0 });
            // At once; the factory's report (the reader marks it dead) is what is waited for.
            kill_task(pid, |p| {
                // SAFETY: a signal to the task's process (p > 0).
                unsafe { libc::kill(p, libc::SIGKILL) };
            });
            // By the clock: a sleep returns early whenever a signal arrives, and the vCPU threads get them all the time.
            let began = Instant::now();
            while !t.dead() && began.elapsed() < Duration::from_secs(20) {
                std::thread::sleep(Duration::from_millis(2));
            }
            // A lasting line for a slow death (fix-widget-kill-loop): the vCPU waits here, and iOS's own exit
            // watchdogs (WidgetKit's "Extension process termination watchdogged pid (N) after Ns" gives an extension
            // 5 s) count it.
            let waited_ms = began.elapsed().as_millis();
            if waited_ms >= 250 {
                logi!(
                    "tasks: pid {} took {} ms to die (the kill to the factory's report; its vCPU waited)",
                    t.pid(),
                    waited_ms
                );
            }
        }
        let mut tb = tables();
        if t.dead() {
            task_release(&mut tb, t);
        } else {
            // Never seen so far. The slots stay taken until the factory reports the exit (reader_main).
            loge!("tasks: pid {} did not die in 20 s: its slots are kept until it does", t.pid());
            t.zombie.store(true, Relaxed);
        }
        0
    }

    /// The memfd's offset of RAM [phys, +size) (mem.rs: the file starts at fd_phys), None outside the RAM.
    fn ram_offset(phys: u64, size: u64) -> Option<u64> {
        let m = mem::state();
        (phys >= m.phys && phys - m.phys <= m.size && size <= m.size - (phys - m.phys)).then(|| phys - m.fd_phys)
    }

    fn user_range(va: u64, size: u64) -> bool {
        let base = BASE.load(Relaxed);
        size != 0
            && (va | size) & (u64::from(crate::hostcalls::info().host_page_size) - 1) == 0
            && va < base
            && size <= base - va
    }

    /// The deferred queue joins an op to the one before it where one host call does both (host-overhead,
    /// 2026-11-15): on the phone the kernel's maps and unmaps were 331,008 and 414,758 per 30 s, each an mmap or
    /// munmap of one page in the task (30-50 us there with the task's mmap_lock taken for writing), and many come in
    /// runs: the pmap flushes a range one page at a time (madvise, the pageout's reference clearing), and a fault that
    /// changes an entry is an unmap of the page and, at the return, the map of the same page again. XsOp::queue_join
    /// (xnuhost-abi's stub.rs, xs_queue_join) is the rule, unit-tested in xnuhost-abi. qlock held. Returns the entry as it was
    /// before when `op` went into it (for a take-back).
    fn queue_join(t: &Task, q: &mut Queue, op: &XsOp) -> Option<XsOp> {
        let n = t.npending.load(Relaxed) as usize;
        let last = q.pending[..n].last_mut()?;
        let before = *last;
        last.queue_join(op).then_some(before)
    }

    pub fn map(handle: u64, va: u64, size: u64, prot: u32, phys: u64) -> i64 {
        let Some(t) = task_of(handle) else { return neg(libc::ESRCH) };
        let Some(offset) = ram_offset(phys, size).filter(|_| user_range(va, size)) else { return neg(libc::EINVAL) };
        let op = XsOp { op: XS_OP_MAP, prot, va, size, offset, result: 0 };
        let p = perf::current();
        if MAP_DEFER.load(Relaxed) {
            // A TLB fill: it must be in place before any thread of the task touches the page again through this
            // entry, and before any later unmap or protect of the task; the queue gives both (npending). A fill that
            // comes late only costs the toucher one more fault, which the kernel fills again.
            let mut q = t.qlock();
            let dead = t.dead();
            let joined = !dead && queue_join(t, &mut q, &op).is_some();
            let n = t.npending.load(Relaxed) as usize;
            let queued = joined || (!dead && n < PENDING_MAX);
            if queued && !joined {
                q.pending[n] = op;
                t.npending.store(n as u32 + 1, SeqCst); // read by releases without qlock
            }
            drop(q);
            if queued {
                p.count(Count::MapsDeferred, 1);
                p.count(Count::OpsJoined, joined as u64);
                return 0;
            }
            if dead {
                return neg(libc::ESRCH);
            }
        }
        p.count(Count::MapsOrdered, 1);
        order(t, &[op])
    }

    pub fn unmap(handle: u64, va: u64, size: u64) -> i64 {
        let Some(t) = task_of(handle) else { return neg(libc::ESRCH) };
        if !user_range(va, size) {
            return neg(libc::EINVAL);
        }
        let op = XsOp { op: XS_OP_UNMAP, va, size, ..ZERO_OP };
        let p = perf::current();
        if MAP_DEFER.load(Relaxed) {
            // A TLB flush: once it returns, no thread of the task may use the old mapping. A task none of whose threads
            // is released (running == 0) uses nothing until its next release, which carries out the queue first; an
            // order given meanwhile takes the queue first too. So it waits in the queue, after the maps queued before
            // it. Whoever takes it holds back the task's other releases until it is done (unmaps_out). A full queue:
            // ordered, and the order takes the queue first.
            let mut q = t.qlock();
            let dead = t.dead();
            let n = t.npending.load(Relaxed) as usize;
            // Joined to the last entry, npending stays (it is not 0, so a release that counts itself after the load
            // below sees the queue and takes it, under qlock, with this unmap in it).
            let saved = if dead { None } else { queue_join(t, &mut q, &op) };
            let mut joined = saved.is_some();
            let mut queued = false;
            if !dead && (joined || n < PENDING_MAX) {
                if !joined {
                    q.pending[n] = op;
                }
                q.nunmaps += 1;
                t.npending.store(if joined { n as u32 } else { n as u32 + 1 }, SeqCst);
                queued = t.running.load(SeqCst) == 0;
                // A thread is counted: take the unmap back (a release that saw it waits for qlock to take the queue).
                if !queued {
                    q.nunmaps -= 1;
                    if let Some(before) = saved {
                        q.pending[n - 1] = before;
                    }
                    t.npending.store(n as u32, SeqCst);
                    joined = false;
                }
            }
            drop(q);
            if queued {
                p.count(Count::UnmapsDeferred, 1);
                p.count(Count::OpsJoined, joined as u64);
                return 0;
            }
            if dead {
                return neg(libc::ESRCH);
            }
        }
        p.count(Count::UnmapsOrdered, 1);
        order(t, &[op])
    }

    pub fn protect(handle: u64, va: u64, size: u64, prot: u32) -> i64 {
        let Some(t) = task_of(handle) else { return neg(libc::ESRCH) };
        if !user_range(va, size) {
            return neg(libc::EINVAL);
        }
        order(t, &[XsOp { op: XS_OP_PROTECT, prot, va, size, ..ZERO_OP }])
    }

    /// The deferred maps and unmaps of a task, ordered now (before a read or write of its memory from outside), and the
    /// unmaps a taker is carrying out waited for: a read through a mapping the kernel has flushed would see a page that
    /// may be another's by now, where it must fault (EFAULT, and the kernel fills the page).
    fn flush_pending(t: &Task) {
        if t.npending.load(Relaxed) != 0 {
            order(t, &[]);
        } else if t.unmaps_out.load(Acquire) {
            drop(t.lock());
        }
    }

    fn vm_rw(handle: u64, va: u64, buffer: *mut libc::c_void, size: u64, write: bool) -> i64 {
        let Some(t) = task_of(handle).filter(|t| !t.dead()) else { return neg(libc::ESRCH) };
        flush_pending(t);
        let local = libc::iovec { iov_base: buffer, iov_len: size as usize };
        let remote = libc::iovec { iov_base: va as usize as *mut libc::c_void, iov_len: size as usize };
        // SAFETY: the kernel's buffer of `size` bytes; the remote side is checked by the kernel (EFAULT).
        let n = unsafe {
            if write {
                libc::process_vm_writev(t.pid(), &local, 1, &remote, 1, 0)
            } else {
                libc::process_vm_readv(t.pid(), &local, 1, &remote, 1, 0)
            }
        };
        if n < 0 {
            neg(errno())
        } else {
            n as i64
        }
    }

    pub fn read(handle: u64, va: u64, buffer: *mut libc::c_void, size: u64) -> i64 {
        vm_rw(handle, va, buffer, size, false)
    }

    pub fn write(handle: u64, va: u64, buffer: *const libc::c_void, size: u64) -> i64 {
        vm_rw(handle, va, buffer as *mut libc::c_void, size, true)
    }

    // ---- threads -----------------------------------------------------------------------------------------------------
    fn area_free(tb: &mut Tables, ti: usize, area: usize) {
        tb.areas[ti][area / 8] &= !(1 << (area % 8));
    }

    pub fn thread_create(handle: u64) -> Result<u64, i64> {
        let Some(t) = task_of(handle) else { return Err(neg(libc::ESRCH)) };
        let ti = t.index();
        let mut tb = tables();
        let area = (0..XS_MAX_THREADS as usize).find(|&i| tb.areas[ti][i / 8] & (1 << (i % 8)) == 0);
        let slot = area.and_then(|_| slot_alloc(&mut tb));
        let (Some(area), Some(slot)) = (area, slot) else { return Err(neg(libc::EAGAIN)) };
        tb.areas[ti][area / 8] |= 1 << (area % 8);
        let th = &THREADS[slot as usize];
        th.task.store(ti as u32, Relaxed);
        th.area.store(area as u32, Relaxed);
        th.placed.store(0, Relaxed);
        th.epoch.store(0, Relaxed);
        th.uclamp.store(0, Relaxed);
        th.released_at.store(0, Relaxed);
        th.used.store(true, Release);
        drop(tb);

        let tp = trap_page(slot);
        let op = XsOp { op: XS_OP_CLONE, va: area as u64, offset: u64::from(slot) * XS_SLOT_SIZE, ..ZERO_OP };
        let mut rc = order(t, &[op]);
        if rc == 0 {
            // SAFETY: a field address of the mapped trap page.
            rc = wait_word(t, unsafe { addr_of_mut!((*tp).to_host) }, 10); // its first stop (XH_TRAP_BOOT)
                                                                           // The clone was answered and the new thread never reported: say what the host knows about it.
            if rc != 0 {
                // SAFETY: a probe of the process (signal 0).
                let there = unsafe { libc::kill(t.pid(), 0) } == 0;
                loge!(
                    "tasks: pid {}: the thread made in area {} did not reach its first stop ({}): tid {}, the process is {}",
                    t.pid(),
                    area,
                    if rc == neg(libc::ESRCH) { "the task died" } else { "10 s" },
                    word!(tp, tid).load(Acquire),
                    if there { "there" } else { "gone" }
                );
            }
        }
        if rc != 0 {
            let mut tb = tables();
            area_free(&mut tb, ti, area);
            th.used.store(false, Relaxed);
            slot_free(&mut tb, slot);
            return Err(rc);
        }
        Ok(u64::from(slot))
    }

    fn thread_of(handle: u64) -> Option<&'static Thread> {
        THREADS.get(handle as usize).filter(|th| th.used.load(Acquire))
    }

    pub fn thread_destroy(handle: u64) -> i64 {
        let Some(th) = thread_of(handle) else { return neg(libc::ESRCH) };
        let t = &TASKS[th.task.load(Relaxed) as usize];
        let tp = trap_page(handle as u32);
        if !t.dead() {
            // The thread is stopped in the stub's handler (the kernel destroys only threads that trapped).
            word!(tp, command).store(XS_EXIT, Relaxed);
            word!(tp, to_task).store(1, Release);
            // SAFETY: field addresses of the mapped trap page.
            futex(unsafe { addr_of_mut!((*tp).to_task) }, libc::FUTEX_WAKE, 1, None);
            // Linux zeroes tid when the thread is gone. Waited for by the clock (an interrupted futex wait returns at
            // once): until then the thread still has this slot's page, and the slot must not go to another thread,
            // whose register state it would take and run in this task's address space.
            let began = Instant::now();
            let mut tid;
            loop {
                tid = word!(tp, tid).load(Acquire);
                if tid == 0 || t.dead() || began.elapsed() >= Duration::from_secs(10) {
                    break;
                }
                // SAFETY: as above.
                futex(
                    unsafe { addr_of_mut!((*tp).tid) },
                    libc::FUTEX_WAIT,
                    tid,
                    Some(&libc::timespec { tv_sec: 0, tv_nsec: 20_000_000 }),
                );
            }
            if tid != 0 && !t.dead() {
                // Never seen so far. The slot and the area stay with the task; destroy takes them back.
                loge!(
                    "tasks: thread {} of pid {} did not exit in 10 s: its slot is kept until the task goes",
                    tid,
                    t.pid()
                );
                log_task_threads(t.pid());
                return 0;
            }
            let va = BASE.load(Relaxed) + XS_THREADS_OFF + u64::from(th.area.load(Relaxed)) * XS_AREA_SIZE;
            order(t, &[XsOp { op: XS_OP_UNMAP, va, size: XS_AREA_SIZE, ..ZERO_OP }]);
        }
        let mut tb = tables();
        area_free(&mut tb, t.index(), th.area.load(Relaxed) as usize);
        th.used.store(false, Relaxed);
        slot_free(&mut tb, handle as u32);
        0
    }

    pub fn thread_set_pac(handle: u64, enabled: u32) -> i64 {
        if thread_of(handle).is_none() {
            return neg(libc::ESRCH);
        }
        let tp = trap_page(handle as u32);
        word!(tp, pac_enabled).store(enabled & XH_PAC_ALL, Relaxed);
        word!(tp, pac_set).store(1, Relaxed);
        0
    }

    /// Debugging (SIGQUIT to xnuhost): every iOS thread, where it last trapped or was released, and whether it is in
    /// user code now (released and not yet back). A running thread is not disturbed.
    pub fn dump() {
        for (s, th) in THREADS.iter().enumerate() {
            if !th.used.load(Relaxed) {
                continue;
            }
            let t = &TASKS[th.task.load(Relaxed) as usize];
            let tp = trap_page(s as u32);
            let running = word!(tp, to_task).load(Acquire) != 0 || word!(tp, to_host).load(Acquire) != XS_HOST_TRAPPED;
            // SAFETY: a copy of the shared page's trap and state (a running thread may change them: for the log only).
            let (trap, st) = unsafe {
                (ptr::read_volatile(addr_of_mut!((*tp).trap)), ptr::read_volatile(addr_of_mut!((*tp).state)))
            };
            logi!(
                "thread {} of task pid {} (tid {}): {}; last trap kind {} far 0x{:x} insn 0x{:x}; pc 0x{:x} lr 0x{:x} sp 0x{:x} \
                 x16 0x{:x} x0 0x{:x} x1 0x{:x} x2 0x{:x}",
                s,
                t.pid(),
                word!(tp, tid).load(Relaxed),
                if t.dead() {
                    "task gone"
                } else if running {
                    "in user code since the state shown"
                } else {
                    "stopped in the kernel's hands"
                },
                trap.kind,
                trap.far,
                trap.insn,
                st.pc,
                st.x[30],
                st.sp,
                st.x[16],
                st.x[0],
                st.x[1],
                st.x[2]
            );
            // The return addresses up the frame-pointer chain, read from the task's process (scripts/debug names
            // them): what a thread that sits in one place is in the middle of. Nothing is printed when the read fails.
            if t.dead() {
                continue;
            }
            let mut line = String::new();
            let mut fp = st.x[29];
            for _ in 0..14 {
                if fp == 0 || fp & 7 != 0 {
                    break;
                }
                let mut pair = [0u64; 2];
                let local = libc::iovec { iov_base: pair.as_mut_ptr() as *mut libc::c_void, iov_len: 16 };
                let remote = libc::iovec { iov_base: fp as usize as *mut libc::c_void, iov_len: 16 };
                // SAFETY: reads 16 bytes of the task into `pair`.
                if unsafe { libc::process_vm_readv(t.pid(), &local, 1, &remote, 1, 0) } != 16 {
                    break;
                }
                line.push_str(&format!(" 0x{:x}", pair[1] & 0x7fff_ffff_ffff));
                if pair[0] <= fp {
                    break;
                }
                fp = pair[0];
            }
            if !line.is_empty() {
                logi!("thread {} frames:{}", s, line);
            }
        }
    }

    /// tgkill's result (cpu::tgkill does not give it): 0, or -1 with errno set.
    fn tgkill_result(pid: i32, tid: i32, sig: i32) -> libc::c_long {
        // SAFETY: tgkill of a thread of an iOS task process; a stale tid is an error return (ESRCH), not a fault.
        unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, sig) }
    }

    /// The direct kick (`xh_tasks_kick`; docs/booter.md 2.6a, "The kick"): the interrupt line of vCPU `v` rose
    /// (cpu::line, on another thread). When `v` waits in thread_run for an iOS thread it released, that thread is
    /// signalled straight away (one signal: the thread stops and hands the vCPU its trap) instead of the vCPU's
    /// thread. Until 2026-11-23 a raise of a busy vCPU's line always signalled the vCPU's thread, which sleeps in
    /// thread_run's futex on the same core as the iOS thread it released (cpu-pin): that thread had to be woken and
    /// scheduled there, ahead of the iOS thread it shares the core with, only to send the iOS thread the signal below.
    /// On the phone (2026-11-23 03:28, 0085 and 0089 active, every vCPU busy) that was 24,419 kicks in 30 s, mean
    /// 136 us, p99 3.7 ms. Whoever sets bit 0 of the release's run_word sends the kick, so one release gets one kick;
    /// a kick that lands after its thread is back stays pending in the stub's handler (it blocks every signal) and is
    /// a spurious interrupt trap at that thread's next release, which the kernel takes as an interrupt with nothing
    /// pending (hosted_user.c, XH_TRAP_INTERRUPT). True: the thread was kicked (now or already for this release);
    /// false: the caller signals the vCPU.
    pub fn kick(v: &Vcpu) -> bool {
        let w = v.run_word.load(Acquire);
        if w == 0 {
            return false; // no released thread: the vCPU's own signal
        }
        if w & 1 != 0 {
            return true; // kicked already for this release
        }
        let pid = v.run_pid.load(Relaxed);
        let tid = v.run_tid.load(Relaxed);
        // Sets the bit only if the word is still this release's (the release numbers are never reused): pid and tid,
        // read after the word with acquire and before this exchange with release, are that release's thread.
        if let Err(now) = v.run_word.compare_exchange(w, w | 1, AcqRel, Acquire) {
            // kicked by another raiser or the vCPU; 0 or a new release not kicked yet: the vCPU's own signal
            return now & 1 != 0;
        }
        v.run_kicked_at.store(cpu::counter(), Relaxed);
        if pid > 0 && tid != 0 && tgkill_result(pid, tid as i32, kick_signal()) == 0 {
            return true;
        }
        // Refused (the process is gone, or its queue of real-time signals is full): the bit off again, and the vCPU's
        // own signal, after which thread_run kicks (and says a refusal) as before.
        let _ = v.run_word.compare_exchange(w | 1, w, AcqRel, Relaxed);
        false
    }

    /// thread_run's end, for perf: the trap's kind and when it was handed to the kernel.
    fn run_done(v: &Vcpu, kind: u32) {
        let p = &v.perf;
        p.trap(kind);
        p.ret_at.set(cpu::counter());
        p.ret_idle.set(p.get(Count::TicksIdle));
        p.ret_kind.set(u64::from(kind));
    }

    #[inline(always)]
    fn spin_hint() {
        // SAFETY: a hint instruction.
        unsafe { std::arch::asm!("yield", options(nomem, nostack, preserves_flags)) };
    }

    /// The perf counts of an iOS thread's turn by its class (cpu::hint_class).
    const CLASS_TICKS: [Count; 4] = [Count::TicksRt, Count::TicksUi, Count::TicksDefault, Count::TicksBg];

    /// Runs the thread from `state` until it traps; `state` is then the trapped state. Blocks the vCPU.
    pub fn thread_run(v: &Vcpu, handle: u64, state: &mut XhUserState, trap: &mut XhTrap) -> i64 {
        let Some(th) = thread_of(handle) else { return neg(libc::ESRCH) };
        let t = &TASKS[th.task.load(Relaxed) as usize];
        let tp = trap_page(handle as u32);
        let p = &v.perf;
        let entered = cpu::counter();
        // The kernel's work since the last trap this vCPU handed it (its idle time taken out).
        let ret_at = p.ret_at.get();
        if ret_at != 0 {
            let gap = entered - ret_at;
            let idle = p.get(Count::TicksIdle) - p.ret_idle.get();
            let k = match p.ret_kind.get() as u32 {
                XH_TRAP_SYSCALL => Step::KSyscall,
                XH_TRAP_FAULT => Step::KFault,
                XH_TRAP_INTERRUPT => Step::KIrq,
                _ => Step::KOther,
            };
            p.step(k, perf::ticks_ns(gap.saturating_sub(idle)));
            p.ret_at.set(0);
        }
        *trap = XhTrap::default();
        if t.dead() {
            trap.kind = XH_TRAP_EXITED;
            trap.exit_status = t.exit_status.load(Relaxed);
            run_done(v, trap.kind);
            return 0;
        }
        // An interrupt already waiting is taken before any user instruction runs.
        if v.cpu.irq_pending.load(Relaxed) != 0 {
            trap.kind = XH_TRAP_INTERRUPT;
            run_done(v, trap.kind);
            return 0;
        }
        let index = v.index();
        // iOS code runs on the task's Linux thread, not on the vCPU's: it follows the vCPU's host CPUs.
        let epoch = PLACE_EPOCH.load(Acquire) as u8;
        if let Some(place) = cpu::place_hook() {
            if th.placed.load(Relaxed) as usize != index + 1 || th.epoch.load(Relaxed) != epoch {
                let tid = word!(tp, tid).load(Acquire);
                if tid != 0 {
                    place(tid as i32, index);
                    let was = th.placed.load(Relaxed) as usize;
                    if was != 0 && was <= cpu::ncpu() && cpu::vcpu(was - 1).kind.load(Relaxed) != v.kind.load(Relaxed) {
                        p.count(Count::PlacesAcross, 1);
                    }
                    th.placed.store((index + 1) as u8, Relaxed);
                    th.epoch.store(epoch, Relaxed);
                    p.count(Count::Places, 1);
                }
            }
        }
        // Its clamp (cpu::release_floor): the host's governor picks a core's clock from what runs there, and what runs
        // there while the vCPU waits is this thread. One sched_setattr when the value changes, none otherwise. The
        // kernel's hint for this thread (hostcall.h perf_hint, kernel 0075): a background thread gets no floor and no
        // ceiling of ours; a UI or realtime thread at least the floor it names; drm2kgsl's animation clamp only the
        // frame's threads. The ceiling of 1024 clears a stale one either way. The vCPU's own thread takes the same
        // floor (cpu::own_uclamp): the kernel's work after this thread's trap is done for it, and until 2026-11-23 the
        // vCPU threads carried whatever the guest or drm2kgsl had set last, idle or not.
        let hint = v.cpu.perf_hint.load(Relaxed);
        let class = cpu::hint_class(hint);
        if let Some(uclamp) = cpu::uclamp_hook() {
            let lo = cpu::release_floor(hint, uclamp(index), cpu::GPU_VCPU_BOOST.load(Relaxed));
            let hi = 1024u32;
            cpu::own_uclamp(v, lo);
            let want = (lo | hi << 11) + 1;
            let tid = word!(tp, tid).load(Acquire);
            if th.uclamp.load(Relaxed) != want && tid != 0 {
                // KEEP_POLICY | KEEP_PARAMS | UTIL_CLAMP_MIN | UTIL_CLAMP_MAX
                if sched_setattr_uclamp(tid as i32, 0x18 | 0x20 | 0x40, lo, hi) == 0 || errno() != libc::EINTR {
                    th.uclamp.store(want, Relaxed);
                }
                p.count(Count::UclampSets, 1);
            }
        }
        // The task's deferred maps and unmaps go with this release: the thread carries them out before it runs. The
        // task's lock is held until it says they are done, so no unmap or protect of the task can pass them; with
        // unmaps among them (unmaps_out) no other thread of the task is released before they are done either. The
        // thread counts as running from here (running, under qlock in the same hold that takes the queue).
        let mut drain: Option<MutexGuard<'_, Placed>> = None;
        let mut nops = 0usize;
        let ops = (tp as usize + XS_OPS_OFF as usize) as *mut XsOp;
        t.running.fetch_add(1, SeqCst); // counted first, then the queue is read (running)
        if t.npending.load(SeqCst) != 0 || t.unmaps_out.load(SeqCst) {
            // `lock`: also the wait for a taker that is carrying out unmaps (it holds `lock` until they are done).
            let guard = t.lock();
            let mut q = t.qlock();
            if t.npending.load(Relaxed) != 0 && !t.dead() {
                nops = take_queue(t, &mut q, ops);
                word!(tp, ops_done).store(0, Relaxed);
                word!(tp, nops).store(nops as u32, Relaxed);
                drain = Some(guard);
            }
            drop(q);
        }
        // SAFETY: the thread is stopped in its handler (to_host was set): the state is ours to write until to_task.
        unsafe { ptr::write(addr_of_mut!((*tp).state), *state) };
        word!(tp, command).store(XS_RUN, Relaxed);
        word64!(tp, spin_ticks).store(SPIN_TASK.load(Relaxed), Relaxed);
        word!(tp, to_host).store(XS_HOST_PENDING, Release);
        // From here a raise of this vCPU's line may kick the thread itself (kick): before the release, so that a raise
        // that comes first is a kick that stays pending in the stub and stops the thread before its first instruction.
        v.run_pid.store(t.pid(), Relaxed);
        v.run_tid.store(word!(tp, tid).load(Acquire), Relaxed);
        let seq = v.run_seq.load(Relaxed) + 1;
        v.run_seq.store(seq, Relaxed);
        v.run_word.store(seq << 1, SeqCst);
        let released = cpu::counter();
        p.count(Count::PrepTicks, released - entered); // placement, uclamp, the queue: host work before the hand-off
        th.released_at.store(released, Relaxed);
        // The trim: the task is awake again; if it was trimmed, it faults its pages in again from here.
        t.last_release.store(released, Relaxed);
        if t.trimmed.load(Relaxed) {
            t.trimmed.store(false, Relaxed);
        }
        // xnuhost-abi's stub.rs: store, then wake only a sleeper.
        word!(tp, to_task).store(1, Release);
        std::sync::atomic::fence(SeqCst);
        if word!(tp, task_sleeping).load(Relaxed) != 0 {
            // SAFETY: a field address of the mapped trap page.
            futex(unsafe { addr_of_mut!((*tp).to_task) }, libc::FUTEX_WAKE, 1, None);
            p.count(Count::TaskWakes, 1);
        }
        let (mut kicked, mut sleeping) = (false, false);
        let (mut kicked_at, mut kicked_last) = (0, 0);
        let kick_again = KICK_AGAIN.load(Relaxed);
        let spin_host = SPIN_HOST.load(Relaxed);
        let step = libc::timespec { tv_sec: 0, tv_nsec: 200_000_000 };
        let to_host = word!(tp, to_host);
        loop {
            // Keep this exact acquire snapshot for FUTEX_WAIT below: the stub publishes deferred-map completion as
            // XS_HOST_OPS_ACK on `to_host` itself, so an ACK arriving between this load and FUTEX_WAIT returns EAGAIN
            // instead of sleeping 200 ms holding the task lock (YMGPwcca/OneiOS 9e5a9933).
            let event = to_host.load(Acquire);
            if drain.is_some() && xs_host_event_has_ack(event) {
                for i in 0..nops {
                    log_map_failure(t, &get_op(ops, i), "in the released thread");
                }
                unmaps_done(t);
                drain = None;
                p.step(Step::Drain, perf::ticks_ns(cpu::counter() - released));
            }
            if drain.is_none() && event == XS_HOST_TRAPPED {
                break;
            }
            if t.dead() {
                v.run_word.store(0, SeqCst);
                if drain.take().is_some() {
                    unmaps_done(t);
                }
                if sleeping {
                    word!(tp, host_sleeping).store(0, Relaxed);
                }
                trap.kind = XH_TRAP_EXITED;
                trap.exit_status = t.exit_status.load(Relaxed);
                t.running.fetch_sub(1, SeqCst);
                let turn = cpu::counter() - released;
                p.count(Count::TicksUser, turn);
                p.count(CLASS_TICKS[class], turn);
                run_done(v, trap.kind);
                return 0;
            }
            // The vCPU's interrupt line rose (its timer or another CPU signalled this thread, which ended the wait
            // below): force the iOS thread to stop. Again at each futex step (200 ms) while it is not back: a kick that
            // did not get there (tgkill refused: the queue of real-time signals full) must not leave the vCPU deaf to
            // its interrupts and to the debugger's cross-call (a stackshot panics after debug-ack, 5 s). The first
            // refusal is said. The first kick of a release is whoever sets bit 0 of run_word: a raiser on another
            // thread (kick, which then signalled the thread already) or this loop; the second, 200 ms on, is always
            // this loop's. The counter is read only once the line is up: a hand-off that comes back with no interrupt
            // costs nothing more.
            if (v.cpu.irq_pending.load(Relaxed) != 0 || session::stopping())
                && (!kicked || cpu::counter() - kicked_last >= kick_again)
            {
                let tid = word!(tp, tid).load(Acquire);
                let mut send = kicked;
                if !kicked {
                    let w = v.run_word.load(Acquire);
                    send = w & 1 == 0 && v.run_word.compare_exchange(w, w | 1, AcqRel, Acquire).is_ok();
                }
                kicked_last = cpu::counter();
                if kicked {
                    p.count(Count::KicksAgain, 1);
                } else if send {
                    kicked_at = kicked_last;
                } else {
                    // a raiser's kick, sent from its own thread (kick)
                    let at = v.run_kicked_at.load(Relaxed);
                    kicked_at = if at >= released && at <= kicked_last { at } else { kicked_last };
                }
                if send
                    && tid != 0
                    && tgkill_result(t.pid(), tid as i32, kick_signal()) != 0
                    && !KICK_FAILED_SAID.swap(true, Relaxed)
                {
                    loge!(
                        "tasks: the kick of pid {} tid {} (vCPU {}'s interrupt) was refused: {}; it is sent again every 200 ms",
                        t.pid(),
                        tid,
                        v.index(),
                        xh_core::sys::strerror(xh_core::sys::errno())
                    );
                }
                kicked = true;
            }
            if !sleeping {
                // Spin a while: a thread that comes back at once (a system call after a few instructions, a fault) is
                // seen without a futex sleep and wake on either side.
                if spin_host != 0 && cpu::counter() - released < spin_host {
                    spin_hint();
                    continue;
                }
                word!(tp, host_sleeping).store(1, Relaxed);
                std::sync::atomic::fence(SeqCst);
                sleeping = true;
                continue; // look at the words once more before the first sleep
            }
            p.count(Count::HostSleeps, 1);
            futex(to_host.as_ptr(), libc::FUTEX_WAIT, event, Some(&step));
        }
        if sleeping {
            word!(tp, host_sleeping).store(0, Relaxed);
        }
        // No more direct kicks for this release; a raiser's kick that landed while the loop did not look (the trap came
        // back first) counts for the histogram when the trap is the interrupt it caused.
        let last_word = v.run_word.swap(0, SeqCst);
        let seen = cpu::counter();
        if !kicked && last_word & 1 != 0 {
            let at = v.run_kicked_at.load(Relaxed);
            if at >= released && at <= seen {
                kicked = true;
                kicked_at = at;
            }
        }
        t.running.fetch_sub(1, SeqCst);
        p.count(Count::TicksUser, seen - released);
        p.count(CLASS_TICKS[class], seen - released);
        // SAFETY: the thread trapped (to_host seen with acquire): its state and trap are written and stay until the
        // next release.
        unsafe {
            *state = ptr::read(addr_of_mut!((*tp).state));
            *trap = ptr::read(addr_of_mut!((*tp).trap));
        }
        // The two hand-offs of this stay (counter ticks of the same clock in both processes).
        let (woken, entered_stub) = (word64!(tp, t_woken).load(Relaxed), word64!(tp, t_enter).load(Relaxed));
        if woken >= released && woken <= seen {
            p.step(Step::Down, perf::ticks_ns(woken - released));
        }
        if entered_stub >= woken && entered_stub <= seen {
            p.step(Step::Up, perf::ticks_ns(seen - entered_stub));
        }
        if word!(tp, slept).load(Relaxed) != 0 {
            p.count(Count::TaskSleeps, 1);
        }
        if kicked && trap.kind == XH_TRAP_INTERRUPT {
            p.step(Step::Kick, perf::ticks_ns(seen - kicked_at));
        }
        if xh_core::insn::TEXT_FIXUP_SVC.load(Relaxed)
            && trap.kind == XH_TRAP_ILLEGAL
            && trap.insn == xh_core::insn::SVC_MARK
        {
            // The system call text_fixup marked: as a trapped svc reports it, with pc after the instruction.
            trap.kind = XH_TRAP_SYSCALL;
            trap.svc = 0x80;
            state.pc += 4;
        }
        run_done(v, trap.kind);
        v.stat.traps.add(1);
        // A trap no fault handler of the kernel can resolve by mapping a page: a data or instruction address outside
        // what an iOS task may use (a pointer that kept its authentication code, a wild jump), an undefined
        // instruction, a breakpoint. They are rare and end the process: said here with the registers, for a boot where
        // the kernel does not log that process's exceptions (the first 64 of a run).
        static TOLD: AtomicU32 = AtomicU32::new(0);
        let outside = trap.kind == XH_TRAP_FAULT && (trap.far < 0x1_0000_0000 || trap.far >= BASE.load(Relaxed));
        if outside || trap.kind == XH_TRAP_ILLEGAL || trap.kind == XH_TRAP_BRK {
            FATAL.fetch_add(1, Relaxed);
            if TOLD.fetch_add(1, Relaxed) < 64 {
                logi!(
                    "tasks: pid {} (tid {}): trap kind {} signal {} far 0x{:x} esr 0x{:x} access {} insn 0x{:x}; pc 0x{:x} \
                     lr 0x{:x} sp 0x{:x} fp 0x{:x} x0 0x{:x} x1 0x{:x} x2 0x{:x} x3 0x{:x} x8 0x{:x} x16 0x{:x} x17 0x{:x}",
                    t.pid(),
                    word!(tp, tid).load(Acquire),
                    trap.kind,
                    trap.signo,
                    trap.far,
                    trap.esr,
                    trap.access,
                    trap.insn,
                    state.pc,
                    state.x[30],
                    state.sp,
                    state.x[29],
                    state.x[0],
                    state.x[1],
                    state.x[2],
                    state.x[3],
                    state.x[8],
                    state.x[16],
                    state.x[17]
                );
            }
        }
        0
    }
}

/// Not AArch64 Linux (and the unit tests): the filter and the line above are all there is.
#[cfg(any(not(xh_machine), test))]
mod machine {
    pub fn init(_c: &crate::config::Config) -> Result<(), crate::mem::SetupFailed> {
        Ok(())
    }
    pub fn available() -> bool {
        false
    }
    pub fn seccomp() -> bool {
        false
    }
    pub fn dump() {}
    pub fn va_limit() -> u64 {
        0
    }
    pub fn shutdown() {}
    pub fn tick() {}
    pub fn trim_pressure(_pressed: bool) {}
    pub fn live() -> u32 {
        0
    }
    /// No tasks here: the caller signals the vCPU.
    pub fn kick(_v: &crate::cpu::Vcpu) -> bool {
        false
    }
    /// Changed by main's thermal step when the vCPUs' host CPUs change.
    pub static PLACE_EPOCH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small interpreter for the classic BPF the filter uses: loads of seccomp_data words, jumps, returns.
    fn bpf_run(f: &[XsFilterInsn], nr: u32, ip: u64, args: &[u64; 6], arch: u32) -> u32 {
        let mut data = [0u32; 16];
        data[0] = nr;
        data[1] = arch;
        data[2] = ip as u32;
        data[3] = (ip >> 32) as u32;
        for (i, a) in args.iter().enumerate() {
            data[4 + 2 * i] = *a as u32;
            data[5 + 2 * i] = (*a >> 32) as u32;
        }
        let (mut acc, mut pc) = (0u32, 0usize);
        while pc < f.len() {
            let i = f[pc];
            pc += 1;
            match i.code {
                BPF_LD_W_ABS => acc = data[(i.k / 4) as usize],
                BPF_JEQ_K => pc += if acc == i.k { i.jt } else { i.jf } as usize,
                BPF_JGE_K => pc += if acc >= i.k { i.jt } else { i.jf } as usize,
                BPF_RET_K => return i.k,
                _ => return 0xdead,
            }
        }
        0xdead
    }

    /// No kill of pid 0 (xnuhost's or the factory's own group) or below (-1: every process); a task's pid is killed.
    #[test]
    fn a_task_kill_never_names_a_group() {
        for pid in [0, -1, i32::MIN] {
            let mut sent = None;
            assert!(!kill_task(pid, |p| sent = Some(p)), "{pid}");
            assert_eq!(sent, None);
        }
        let mut sent = None;
        assert!(kill_task(4242, |p| sent = Some(p)));
        assert_eq!(sent, Some(4242));
    }

    #[test]
    fn seccomp_filter() {
        let mut f = [XsFilterInsn { code: 0, jt: 0, jf: 0, k: 0 }; XS_FILTER_MAX];
        let (lo, none) = (0x7e_0000_0000u64, [0u64; 6]);
        let hi = lo + 3000;
        let n = filter(&mut f, lo, hi, 5, 6) as usize;
        assert!(n > 0 && n <= XS_FILTER_MAX);
        let f = &f[..n];
        let (trap, allow, kill, arm) =
            (SECCOMP_RET_TRAP, SECCOMP_RET_ALLOW, SECCOMP_RET_KILL_PROCESS, AUDIT_ARCH_AARCH64);
        // iOS code: every system call traps, whatever Linux number x8 happens to hold
        assert_eq!(bpf_run(f, 98, 0x1_0000_4000, &none, arm), trap);
        assert_eq!(bpf_run(f, 222, 0x1_8000_1234, &none, arm), trap);
        assert_eq!(bpf_run(f, 93, lo - 4, &none, arm), trap);
        assert_eq!(bpf_run(f, 93, hi, &none, arm), trap);
        assert_eq!(bpf_run(f, 93, 0x7f_0000_0010, &none, arm), trap); // same low word, other high word
                                                                      // the stub: its own calls pass
        assert_eq!(bpf_run(f, 98, lo + 0x100, &none, arm), allow); // futex
        assert_eq!(bpf_run(f, 139, lo + 0x10, &none, arm), allow); // rt_sigreturn
        assert_eq!(bpf_run(f, 220, hi - 4, &none, arm), allow); // clone
        assert_eq!(bpf_run(f, 226, lo + 8, &none, arm), allow); // mprotect
                                                                // ... but nothing else from there (a jump into the stub's svc with a chosen x8)
        assert_eq!(bpf_run(f, 221, lo + 0x100, &none, arm), trap); // execve
        assert_eq!(bpf_run(f, 56, lo + 0x100, &none, arm), trap); // openat
        assert_eq!(bpf_run(f, 129, lo + 0x100, &none, arm), trap); // kill
                                                                   // mmap from the stub: only the two memfds or anonymous memory
        assert_eq!(bpf_run(f, 222, lo + 0x100, &[0, 0, 0, 0, 5, 0], arm), allow);
        assert_eq!(bpf_run(f, 222, lo + 0x100, &[0, 0, 0, 0, 6, 0], arm), allow);
        assert_eq!(bpf_run(f, 222, lo + 0x100, &[0, 0, 0, 0, u64::MAX, 0], arm), allow);
        assert_eq!(bpf_run(f, 222, lo + 0x100, &[0, 0, 0, 0, 7, 0], arm), trap);
        assert_eq!(bpf_run(f, 98, lo + 0x100, &none, 0xc000_003e), kill); // another architecture's call
        let mut g = [XsFilterInsn { code: 0, jt: 0, jf: 0, k: 0 }; XS_FILTER_MAX];
        assert_eq!(filter(&mut g, 0xffff_f000, 0x1_0000_1000, 5, 6), 0); // text across a 4 GiB line
    }

    #[test]
    fn tasks_health_line() {
        // tasks: nothing moved, no line; a move, one line with the differences and the live count
        let before = TaskCounts { made: 10, refused: 0, ended: 4, died: 0, fatal: 1, live: 6, most: 7 };
        let mut now = before;
        assert_eq!(health_line(&now, &before, 60), None);
        now.made += 3;
        now.ended += 2;
        now.died = 1;
        now.live = 7;
        now.most = 8;
        assert_eq!(
            health_line(&now, &before, 60).as_deref(),
            Some("tasks: 60 s: +3 made, +2 ended by the kernel, +1 died on their own, +0 fatal traps, +0 refused; live 7 (most 8)")
        );
    }

    #[test]
    fn the_trim_takes_only_idle_tasks_once() {
        let hz = 19_200_000u64; // the phone's counter
        let idle = 30 * hz;
        let now = 1000 * hz;
        // idle 30 s or more, nothing released, not trimmed yet: due
        assert!(trim_due(now, now - idle, 0, false, idle));
        assert!(trim_due(now, 0, 0, false, idle));
        // a thread released now, a release 29 s ago, or trimmed since its last release: not due
        assert!(!trim_due(now, now - idle, 1, false, idle));
        assert!(!trim_due(now, now - idle + 1, 0, false, idle));
        assert!(!trim_due(now, now - 2 * idle, 0, true, idle));
        // a release stamped after the look (another vCPU): not due, no underflow
        assert!(!trim_due(now, now + 5, 0, false, idle));
        // under pressure 2 s; never under 1 s; task-trim-idle=1 stays 1
        assert_eq!(trim_idle_s(30, false), 30);
        assert_eq!(trim_idle_s(30, true), 2);
        assert_eq!(trim_idle_s(1, true), 1);
        assert_eq!(trim_idle_s(0, false), 1);
    }

    #[test]
    fn the_trim_line() {
        let before =
            TrimCounts { trims: 10, pressed: 1, failed: 0, pte_before: 30 << 20, pte_after: 1 << 20, ns: 5_000_000 };
        let now = TrimCounts {
            trims: 51,
            pressed: 1,
            failed: 0,
            pte_before: (30 << 20) + 41 * 2085 * 1024,
            pte_after: (1 << 20) + 41 * 12 * 1024,
            ns: 5_000_000 + 41 * 1_487_000,
        };
        let look = TrimLook { live: 312, awake: 97, trimmed: 188 };
        assert_eq!(
            trim_line(&now, &before, 60, &look, 140, 5_100_000, 30).as_deref(),
            Some(
                "tasks: trim: 60 s: +41 trimmed (+0 under pressure, 0 failed), page tables 83 MiB given back (2073 KiB a \
                 task, 12 KiB left), the unmaps 60 ms (1487 us a task, most 5 ms); now 312 live: 97 awake (a release in \
                 the last 30 s; most 140 since the last line), 188 trimmed"
            )
        );
        // no task and nothing trimmed: no line
        assert_eq!(trim_line(&before, &before, 60, &TrimLook::default(), 0, 0, 30), None);
    }
}
