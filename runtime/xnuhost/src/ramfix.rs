// SPDX-License-Identifier: MIT
//! Port of ramfix.c. The guest's RAM, held fixed for the session (docs/booter.md 2.2, "Fixed RAM"; the owner's
//! decision of 2026-11-13: iOS's RAM must not grow and shrink during a session; the default since fix-ram-locked,
//! 2026-11-15: iOS's RAM is never swapped). xnuhost takes the whole memfd once, at the start: a thread of its own at
//! nice 19 prefaults it in steps (mlock, which also locks it, where Linux allows; where it does not the prefault
//! stops), and nothing is given back until the process ends (ram_discard is refused; kernel 0067 does not ask). The
//! main loop runs the valve. The exit frees all of it at once (the memfd's last mapping and descriptor go with the
//! session's processes). Portable: the host tests run it on memfds of their own, with a failing mlock put in.
//!
//! Never: a realtime class, a lower OOM score, another cgroup, a write outside the process (docs/booter.md 2.9). Read
//! only: /proc/meminfo, /proc/vmstat, /proc/self/{status,stat,cgroup}, the cgroup's memory files, KGSL's per-process
//! files.

use crate::config::{meminfo, proc_values};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering::*};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use xh_core::fs::read_line;
use xh_core::logi;
use xh_core::sys::strerror;

/// A prefault step: one mlock call or 4096 page reads.
pub const STEP: u64 = 16 << 20;

/// What the prefault calls to lock and unlock; the tests put failing ones here. Errors are errno values.
pub trait MemLock: Send + Sync {
    fn lock(&self, at: *const u8, len: usize) -> Result<(), i32>;
    fn unlock(&self, at: *const u8, len: usize);
}

/// mlock and munlock, as bionic makes them.
#[derive(Debug, Default)]
pub struct SystemLock;

impl MemLock for SystemLock {
    fn lock(&self, at: *const u8, len: usize) -> Result<(), i32> {
        // SAFETY: mlock only pins pages of our own mapping; a bad range is an error return.
        if unsafe { libc::mlock(at as *const libc::c_void, len) } == 0 {
            Ok(())
        } else {
            Err(xh_core::sys::errno())
        }
    }
    fn unlock(&self, at: *const u8, len: usize) {
        // SAFETY: as above.
        unsafe { libc::munlock(at as *const libc::c_void, len) };
    }
}

/// What is asked (before the start).
#[derive(Clone)]
pub struct Options {
    /// lock what is prefaulted (mlock), step by step; the prefault stops at the first failure
    pub lock: bool,
    /// why lock is off, for the log ("ram-lock=0", "more than the host has free")
    pub lock_why: Option<String>,
    /// bytes per step; 0 = STEP
    pub step: u64,
    /// the valve: host MemAvailable under this while locked: munlock once; 0 = never
    pub unlock_below: u64,
    /// where the prefault and the valve read MemAvailable; None: the prefault does not look
    pub meminfo: Option<String>,
    pub locker: Arc<dyn MemLock>,
    /// ram-ondemand: the RAM is locked on fault (lock_on_fault), not prefaulted: `locked` is the range so marked, not
    /// pages in the host's RAM
    pub on_fault: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            lock: false,
            lock_why: None,
            step: 0,
            unlock_below: 0,
            meminfo: None,
            locker: Arc::new(SystemLock),
            on_fault: false,
        }
    }
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("lock", &self.lock)
            .field("lock_why", &self.lock_why)
            .field("step", &self.step)
            .field("unlock_below", &self.unlock_below)
            .field("meminfo", &self.meminfo)
            .field("on_fault", &self.on_fault)
            .finish()
    }
}

/// `xh_ramfix`: what is asked, and what the prefault did (written by it, read by the log and the valve).
#[derive(Debug, Default)]
pub struct Ramfix {
    pub opts: Options,
    /// bytes; [0, locked) is locked
    pub size: AtomicU64,
    pub prefaulted: AtomicU64,
    pub locked: AtomicU64,
    /// the errno of the mlock that failed (EPERM, ENOMEM, EAGAIN); what was locked before stays
    pub lock_error: AtomicI32,
    pub locking: AtomicBool,
    pub done: AtomicBool,
    pub stop: AtomicBool,
    pub unlocked: AtomicBool,
    /// the prefault stopped: host MemAvailable under unlock_below (short_available)
    pub short_stop: AtomicBool,
    /// the prefault's time
    pub ms: AtomicU64,
    /// MemAvailable when the valve opened / when the prefault stopped short
    pub unlock_available: AtomicU64,
    pub short_available: AtomicU64,
    thread: Mutex<Option<JoinHandle<()>>>,
}

fn ms_now() -> u64 {
    xh_core::time::monotonic_ms()
}

impl Ramfix {
    pub fn new(opts: Options) -> Ramfix {
        Ramfix { opts, ..Ramfix::default() }
    }

    /// ram-ondemand: [0, size) was locked on fault (lock_on_fault) by the caller. Nothing to prefault (done); the valve
    /// unlocks the range as for a whole lock; the resident count looks at all of it (unlocked_from).
    pub fn on_demand(mut opts: Options, size: u64) -> Ramfix {
        opts.lock = true;
        opts.on_fault = true;
        let r = Ramfix::new(opts);
        r.size.store(size, Relaxed);
        r.locked.store(size, Relaxed);
        r.done.store(true, Release);
        r
    }

    /// One step: locked (and so in the host's RAM) when the lock still holds, else read once a page through the
    /// mapping (a read of a hole through a shared mapping fills it with a zeroed page: shmem has no shared zero
    /// page). Neither changes a byte the guest wrote. Not fallocate: the pages it allocates stay "not up to date"
    /// until first touched, so mincore (the `memory:` line's "in the host's RAM") does not count them, and the
    /// guest's first touch still takes a fault.
    /// False when the prefault should stop here: the lock was asked for and failed. A page prefaulted and not locked
    /// is one iOS has not used yet, so it is the coldest page there is and the first the host swaps out (the PC,
    /// 2026-11-14: 841 MiB of a prefaulted, unlocked 4 GiB guest swapped out in 15 min, against 6 MiB faulted in as
    /// used); the rest is faulted in as iOS uses it.
    /// What was locked before the failure stays locked (fix-ram-locked, 2026-11-15; before, it was unlocked again,
    /// "all or nothing"). Unlocking it only made those pages prefaulted-and-unlocked, the very pages the host swaps
    /// first; the part that could not be locked is faulted in as used either way. Where the lock fails at the first
    /// step (EPERM: no CAP_IPC_LOCK; ENOMEM: an RLIMIT_MEMLOCK under one step, the PC's 8 MiB) nothing was locked and
    /// nothing is prefaulted. Where it fails part way because the host is short, the valve (MemAvailable under
    /// ram-unlock-below) decides as for a whole lock.
    fn step(&self, base: *mut u8, at: u64, length: u64) -> bool {
        if self.locking.load(Relaxed) {
            match self.opts.locker.lock(base.wrapping_add(at as usize), length as usize) {
                Ok(()) => {
                    self.locked.store(at + length, Relaxed);
                    return true;
                }
                Err(e) => {
                    self.lock_error.store(if e != 0 { e } else { libc::EINVAL }, Relaxed);
                    self.locking.store(false, Relaxed);
                    return false;
                }
            }
        }
        let page = xh_core::sys::page_size();
        for i in (0..length).step_by(page as usize) {
            // SAFETY: [base + at, +length) is inside the mapping the caller of prefault gave; a volatile read so
            // that it is made.
            unsafe { std::ptr::read_volatile(base.add((at + i) as usize)) };
        }
        true
    }

    /// [base, base + size), a shared mapping of the RAM memfd, synchronously, step by step (stops early on stop).
    ///
    /// `base` must be the start of a mapping of at least `size` bytes, which stays mapped meanwhile.
    pub fn prefault(&self, base: *mut u8, size: u64) {
        let chunk = if self.opts.step != 0 { self.opts.step } else { STEP };
        let t0 = ms_now();
        self.size.store(size, Relaxed);
        self.locking.store(self.opts.lock, Relaxed);
        self.lock_error.store(0, Relaxed);
        let mut at = 0;
        while at < size && !self.stop.load(Relaxed) {
            let length = (size - at).min(chunk);
            // The host's MemAvailable before each step (one read of /proc/meminfo per 16 MiB): what fits was reckoned
            // at the start, and if the host is short now the prefault stops where it is; the valve then decides about
            // what is locked.
            if let (Some(path), true) = (&self.opts.meminfo, self.opts.unlock_below != 0) {
                if let Some((_, available)) = meminfo(path) {
                    if available < self.opts.unlock_below {
                        self.short_stop.store(true, Relaxed);
                        self.short_available.store(available, Relaxed);
                        break;
                    }
                }
            }
            if !self.step(base, at, length) {
                break;
            }
            self.prefaulted.store(at + length, Relaxed);
            at += chunk;
        }
        self.ms.store(ms_now() - t0, Relaxed);
        self.done.store(true, Release);
    }

    /// Marks the prefault done without one (main: the RAM is not prefaulted).
    pub fn mark_done(&self) {
        self.done.store(true, Release);
    }

    /// The valve: true when it opened now (the RAM unlocked).
    pub fn valve(&self, base: *mut u8, available: u64) -> bool {
        let locked = self.locked.load(Relaxed);
        if locked == 0
            || self.unlocked.load(Relaxed)
            || self.opts.unlock_below == 0
            || available >= self.opts.unlock_below
        {
            return false;
        }
        self.opts.locker.unlock(base, locked as usize);
        self.unlocked.store(true, Relaxed);
        self.unlock_available.store(available, Relaxed);
        self.locked.store(0, Relaxed);
        true
    }

    /// The valve as the main loop runs it (nice 0, once a second): reads MemAvailable from the meminfo file only
    /// while the prefault is done and something is locked. Some(what it read, whether it opened now).
    ///
    /// From the main loop rather than the prefault's nice-19 thread: under load (eight busy vCPUs at nice 0) a
    /// nice-19 thread gets a few percent of a core and could wait seconds to run, which is when the valve is wanted.
    /// One read of /proc/meminfo a second while something is locked, nothing otherwise; never during the prefault (it
    /// checks itself).
    pub fn valve_tick(&self, base: *mut u8) -> Option<(u64, bool)> {
        if !self.done.load(Acquire)
            || self.unlocked.load(Relaxed)
            || self.opts.unlock_below == 0
            || self.locked.load(Relaxed) == 0
        {
            return None;
        }
        let (_, now) = meminfo(self.opts.meminfo.as_deref()?)?;
        Some((now, self.valve(base, now)))
    }

    /// Where the part of the guest's RAM that the host may have swapped out begins: [0, locked) is resident while
    /// this process's VmLck covers it, so a resident count (mincore) need only look at [returned, size).
    pub fn unlocked_from(&self, vm_locked: u64) -> u64 {
        // Locked on fault: the range is marked, its pages come in as iOS touches them; count them all.
        if self.opts.on_fault {
            return 0;
        }
        let locked = self.locked.load(Relaxed);
        // VmLck is the size of this process's locked mappings: when it does not cover what the prefault locked (it
        // should always), count everything.
        if locked != 0 && !self.unlocked.load(Relaxed) && vm_locked >= locked {
            locked
        } else {
            0
        }
    }

    fn say_done(&self) {
        let locked = self.locked.load(Relaxed);
        let (prefaulted, size, err) =
            (self.prefaulted.load(Relaxed), self.size.load(Relaxed), self.lock_error.load(Relaxed));
        let short_by = if self.short_stop.load(Relaxed) {
            format!(
                "; stopped at {} MiB: host MemAvailable {} MiB is under ram-unlock-below ({} MiB)",
                prefaulted >> 20,
                self.short_available.load(Relaxed) >> 20,
                self.opts.unlock_below >> 20
            )
        } else {
            String::new()
        };
        let why = if locked != 0 && locked == size {
            "locked (mlock: the host cannot swap it)".to_owned()
        } else if locked != 0 && err != 0 {
            format!(
                "{} MiB of it locked (mlock: {} past that; what is locked stays locked), not prefaulted further: the \
                 rest is faulted in as iOS uses it",
                locked >> 20,
                strerror(err)
            )
        } else if locked != 0 {
            format!("{} MiB of it locked", locked >> 20)
        } else if !self.opts.lock {
            format!("not locked ({})", self.opts.lock_why.as_deref().unwrap_or("ram-lock=0"))
        } else if err != 0 {
            format!(
                "not locked (mlock: {}), so not prefaulted: the guest's RAM is faulted in as iOS uses it (an \
                 unlocked, unused page is the first the host swaps)",
                strerror(err)
            )
        } else {
            "not locked".to_owned()
        };
        logi!(
            "memory: the guest's RAM is held: {} MiB prefaulted in {} ms, {}{}{}",
            prefaulted >> 20,
            self.ms.load(Relaxed),
            why,
            if self.opts.lock { "" } else { ", read once a page" },
            short_by
        );
    }
}

/// ram-ondemand (option C, docs/booter.md 2.2 "On demand"): mlock2(MLOCK_ONFAULT) of [at, at + len), so that each
/// page is locked as it is first touched and the host never swaps it, while a page never touched costs nothing and a
/// page given back (a hole punched) is freed. Err(errno); ENOSYS where there is no mlock2.
pub fn lock_on_fault(at: *const u8, len: usize) -> Result<(), i32> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: mlock2 only marks pages of our own mapping; a bad range is an error return. The raw call: bionic has
        // mlock2 only from API 30.
        let r = unsafe { libc::syscall(libc::SYS_mlock2, at as usize, len, libc::MLOCK_ONFAULT as libc::c_long) };
        if r == 0 {
            Ok(())
        } else {
            Err(xh_core::sys::errno())
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = (at, len);
        Err(libc::ENOSYS)
    }
}

/// The prefault on a thread of its own at nice 19 (SCHED_OTHER); the thread ends with the prefault.
pub fn start(r: &'static Ramfix, base: *mut u8, size: u64) -> Result<(), crate::mem::SetupFailed> {
    let base = base as usize;
    let spawned = std::thread::Builder::new().name("xnuhost-ram".into()).spawn(move || {
        // Below every vCPU and task thread (nice 0): a prefault takes no CPU time the guest wants. SCHED_OTHER, never
        // realtime. The thread ends with the prefault; the valve runs in the main loop (valve_tick), not at nice 19.
        // SAFETY: gettid and setpriority of this thread.
        unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 19) };
        r.prefault(base as *mut u8, size);
        if !r.stop.load(Relaxed) {
            r.say_done();
        }
    });
    match spawned {
        Ok(handle) => {
            *r.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
            Ok(())
        }
        Err(e) => {
            xh_core::loge!("memory: no thread for the prefault: {}", xh_core::sys::error_text(&e));
            Err(crate::mem::SetupFailed)
        }
    }
}

pub fn stop(r: &Ramfix) {
    r.stop.store(true, Relaxed);
    if let Some(t) = r.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = t.join();
    }
}

// ---- what Linux says about locking here (read only), once at the start ---------------------------------------------

/// CapEff's bit 14 (CAP_IPC_LOCK: mlock past RLIMIT_MEMLOCK); None when unreadable.
fn cap_ipc_lock() -> Option<bool> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find_map(|l| l.strip_prefix("CapEff:"))?;
    Some(u64::from_str_radix(line.trim(), 16).unwrap_or(0) >> 14 & 1 == 1)
}

fn memlock_limit() -> libc::rlimit {
    let mut l = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit fills the struct.
    unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut l) };
    l
}

/// Whether a lock of the whole RAM can work here: CAP_IPC_LOCK in CapEff, or RLIMIT_MEMLOCK unlimited. The size
/// ram=auto may take depends on it (config: the headroom with the lock is smaller, there is no zram working set for
/// the guest), so it is asked before the size is known: a lock of any size works with either.
pub fn can_lock() -> bool {
    cap_ipc_lock() == Some(true) || memlock_limit().rlim_cur == libc::RLIM_INFINITY
}

/// One line of what decides whether a lock can work here: RLIMIT_MEMLOCK, CAP_IPC_LOCK, the cgroup's memory limits.
pub fn facts() -> String {
    let l = memlock_limit();
    let limit =
        if l.rlim_cur == libc::RLIM_INFINITY { "unlimited".to_owned() } else { format!("{} KiB", l.rlim_cur >> 10) };
    // cgroup v2: "0::/path"; the memory controller's limits of that cgroup, if it has them
    let cg = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("0::").map(str::to_owned)))
        .unwrap_or_else(|| "?".to_owned());
    let file = |name: &str| {
        if cg == "?" {
            "?".to_owned()
        } else {
            read_line(format!("/sys/fs/cgroup{cg}/{name}")).unwrap_or_else(|| "-".to_owned())
        }
    };
    let (max, swap, high) = (file("memory.max"), file("memory.swap.max"), file("memory.high"));
    let cap = match cap_ipc_lock() {
        None => "?",
        Some(true) => "yes",
        Some(false) => "no",
    };
    format!("RLIMIT_MEMLOCK {limit}, CAP_IPC_LOCK {cap}; cgroup {cg} (memory.max {max}, memory.high {high}, memory.swap.max {swap})")
}

// ---- 2 MiB host pages (fix-ram-hugepages, 2026-11-15) --------------------------------------------------------------
// The RAM is a memfd (udmabuf takes no other file) that every iOS task maps in 16 KiB pieces. So of Linux's 2 MiB
// pages:
// - shmem THP: a memfd is on the kernel's internal shmem mount, governed by shmem_enabled; with "advise" a mapping
//   with MADV_HUGEPAGE gets 2 MiB folios, "always"/"within_size"/"force" give them anyway, "never" (the kernel's
//   default) and "deny" never. A task's 16 KiB mapping of a 2 MiB folio is mapped with 4 KiB entries, which Linux
//   allows.
// - hugetlb (MFD_HUGETLB): every mmap of such a file must be 2 MiB-aligned in offset and length, which a task's 16 KiB
//   page is not, and GKI 6.1's defconfig has no CONFIG_HUGETLBFS. Read for the log only.
// - Nothing global is written here (the phone's rule: what is changed is restored): where shmem_enabled is "never",
//   the OneOS fork's switch would write "advise" and its return "never" (oneos/interface.md); xnuhost only reads.

/// What [`huge_choose`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Huge {
    #[default]
    None,
    Shmem,
}

/// `xh_huge_choice`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HugeChoice {
    pub mode: Huge,
    /// the bracketed value ("advise"), "" if unreadable
    pub shmem_enabled: String,
    /// hpage_pmd_size (bytes), 0 if unreadable
    pub pmd_size: u64,
    /// HugePages_Free (pages), Hugepagesize (bytes)
    pub hugetlb_free: u64,
    pub hugetlb_size: u64,
    /// the reason, for the log
    pub why: String,
}

/// 2 MiB host pages for the guest's RAM: read only, at the start, named files under `root` ("" on the phone):
/// sys/kernel/mm/transparent_hugepage/{shmem_enabled, hpage_pmd_size} and proc/meminfo's HugePages_Free and
/// Hugepagesize. Never a folder walk; nothing is written. want: asked for (ram-huge=1) and the RAM is held fixed (a
/// give-back punches pieces smaller than 2 MiB, each split).
pub fn huge_choose(root: &str, want: bool) -> HugeChoice {
    let mut c = HugeChoice::default();
    let thp = format!("{root}/sys/kernel/mm/transparent_hugepage");
    if let Some(text) = read_line(format!("{thp}/shmem_enabled")) {
        // "always within_size [advise] never deny force": the bracketed one is in force
        if let Some((_, rest)) = text.split_once('[') {
            if let Some((value, _)) = rest.split_once(']') {
                if value.len() < 16 {
                    c.shmem_enabled = value.to_owned();
                }
            }
        }
    }
    if let Some(text) = read_line(format!("{thp}/hpage_pmd_size")) {
        c.pmd_size = xh_core::parse::leading_u64(text.trim(), 10).map_or(0, |(v, _)| v);
    }
    let v = proc_values(format!("{root}/proc/meminfo"), &["HugePages_Free", "Hugepagesize"]);
    c.hugetlb_free = v[0].unwrap_or(0);
    c.hugetlb_size = v[1].unwrap_or(0);
    let s = c.shmem_enabled.as_str();
    if !want {
        c.why = "not asked (ram-huge=0, or ram-giveback=1: a give-back's pieces would split them)".to_owned();
    } else if c.pmd_size != 2 << 20 {
        c.why = format!("no (hpage_pmd_size {}: no transparent huge pages of 2 MiB on this kernel)", c.pmd_size);
    } else if ["advise", "always", "within_size", "force"].contains(&s) {
        c.mode = Huge::Shmem;
    } else if s.is_empty() {
        c.why = "no (shmem_enabled unreadable)".to_owned();
    } else {
        c.why = format!("no (shmem_enabled {s}: a memfd gets 4 KiB pages; the fork's switch would set advise and its return restore {s})");
    }
    c
}

// ---- the counters of the `memory:` lines ---------------------------------------------------------------------------
/// This process's memory mapped with 2 MiB entries (/proc/self/smaps_rollup: ShmemPmdMapped, FilePmdMapped,
/// AnonHugePages), bytes; None where there is no smaps_rollup. The kernel walks every mapping of the process for it
/// (the kernel's heap and the tasks' maps are many), so it is read at the prefault's end and at the stop only, not
/// every host-mem-log.
pub fn pmd_mapped_self() -> Option<(u64, u64, u64)> {
    let v = proc_values("/proc/self/smaps_rollup", &["ShmemPmdMapped", "FilePmdMapped", "AnonHugePages"]);
    v.iter().any(Option::is_some).then(|| (v[0].unwrap_or(0), v[1].unwrap_or(0), v[2].unwrap_or(0)))
}

/// The host's pswpin and pswpout (pages since boot), one read of /proc/vmstat: None when it cannot be read.
pub fn vmstat_swaps() -> Option<(u64, u64)> {
    let v = proc_values("/proc/vmstat", &["pswpin", "pswpout"]);
    Some((v[0]?, v[1]?))
}

/// Whether this process has transparent huge pages turned off (/proc/self/status `THP_enabled: 0`, PR_SET_THP_DISABLE);
/// None when the line is not there (Linux before 5.0).
pub fn thp_disabled_self() -> Option<bool> {
    thp_disabled_in("/proc/self/status")
}

/// [`thp_disabled_self`] of a named status file (the tests').
pub fn thp_disabled_in(path: &str) -> Option<bool> {
    proc_values(path, &["THP_enabled"])[0].map(|v| v == 0)
}

/// The host's file and shmem THP counters (/proc/vmstat): 2 MiB folios allocated, and attempts that fell back to 4 KiB
/// pages (no free 2 MiB block). Neither moves when shmem never tries (shmem_enabled, VM_HUGEPAGE or THP_enabled say no).
pub fn thp_file_counts() -> (u64, u64) {
    let v = proc_values("/proc/vmstat", &["thp_file_alloc", "thp_file_fallback"]);
    (v[0].unwrap_or(0), v[1].unwrap_or(0))
}

/// `xh_ramfix_counters`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub vm_locked: u64,
    pub vm_pte: u64,
    /// the host's Mlocked: left 0 here (the caller has it from its own read of /proc/meminfo)
    pub mlocked: u64,
    pub swap_ins: u64,
    pub swap_outs: u64,
    pub major_faults: u64,
    pub minor_faults: u64,
}

/// VmLck and VmPTE of this process (bytes), the host's pswpin/pswpout and this process's minflt and majflt (counts):
/// one read each of /proc/self/status, /proc/vmstat and /proc/self/stat.
pub fn sample() -> Counters {
    let v = proc_values("/proc/self/status", &["VmLck", "VmPTE"]);
    let (swap_ins, swap_outs) = vmstat_swaps().unwrap_or((0, 0));
    let mut c = Counters {
        vm_locked: v[0].unwrap_or(0),
        vm_pte: v[1].unwrap_or(0),
        swap_ins,
        swap_outs,
        ..Counters::default()
    };
    // /proc/self/stat: minflt is the 10th field and majflt the 12th, after "(comm)" which may hold spaces
    if let Ok(text) = std::fs::read_to_string("/proc/self/stat") {
        if let Some((_, rest)) = text.rsplit_once(')') {
            let fields: Vec<&str> = rest.split(' ').collect();
            let num = |i: usize| fields.get(i).and_then(|f| f.trim().parse().ok()).unwrap_or(0);
            c.minor_faults = num(8);
            c.major_faults = num(10);
        }
    }
    c
}

/// One of KGSL's per-process memory files of `pid` (`kernel`: KGSL's own kernel memory for it), bytes; None where
/// there is none.
pub fn kgsl_value(pid: i32, name: &str) -> Option<u64> {
    let v = read_line(format!("/sys/class/kgsl/kgsl/proc/{pid}/{name}"))?;
    v.split_whitespace().next()?.parse().ok()
}

/// zram's pools in the host's RAM (mem_used_total, the third figure of each /sys/block/zram*/mm_stat), bytes; 0 where
/// there is no zram.
pub fn zram_pool() -> u64 {
    let Ok(dir) = std::fs::read_dir("/sys/block") else { return 0 };
    dir.flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("zram"))
        .filter_map(|e| read_line(e.path().join("mm_stat")))
        .filter_map(|line| line.split_whitespace().nth(2)?.parse::<u64>().ok())
        .sum()
}

/// This process's oom_score and oom_score_adj (/proc/self): the kernel's OOM killer ends the highest score first.
/// Read only (CLAUDE.md: xnuhost never lowers its own).
pub fn oom_score() -> (Option<i64>, Option<i64>) {
    let num = |p: &str| read_line(p).and_then(|v| v.trim().parse().ok());
    (num("/proc/self/oom_score"), num("/proc/self/oom_score_adj"))
}

/// KGSL's per-process memory files of `pid` (/sys/class/kgsl/kgsl/proc/PID: "name value, ..." as KGSL prints
/// them), at most 12; "" where there is no KGSL.
pub fn kgsl_memory(pid: i32) -> String {
    let dir = format!("/sys/class/kgsl/kgsl/proc/{pid}");
    let Ok(entries) = std::fs::read_dir(&dir) else { return String::new() };
    let mut out = String::new();
    let mut shown = 0;
    for e in entries.flatten() {
        if shown == 12 {
            break;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Some(value) = read_line(format!("{dir}/{name}")) else { continue };
        // one number each, as KGSL prints them
        let digits = value.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !(value.len() == digits || value.as_bytes()[digits] == b' ') {
            continue;
        }
        let v: u64 = value[..digits].parse().unwrap_or(u64::MAX);
        out.push_str(&format!("{}{} {}", if shown > 0 { ", " } else { "" }, name, v));
        shown += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discard::tests::{memfd_held, TestRam};
    use crate::discard::{ram_discard_fd, ram_resident};
    use std::os::fd::RawFd;
    use std::sync::atomic::AtomicU32;

    /// The tests that lock real memory and read VmLck, one at a time (VmLck is the whole process's).
    static REAL_LOCKS: Mutex<()> = Mutex::new(());

    const M: u64 = 1 << 20;

    /// Locks nothing: allocates the step in the memfd as a real mlock populates it, `ok` times, then fails.
    struct FakeLock {
        fd: RawFd,
        base: usize,
        ok: AtomicU32,
        errno: AtomicI32,
        locks: AtomicU32,
        unlocks: AtomicU32,
        unlocked: AtomicU64,
    }

    impl FakeLock {
        fn new(ram: &TestRam, ok: u32, errno: i32) -> Arc<FakeLock> {
            Arc::new(FakeLock {
                fd: ram.raw(),
                base: ram.base as usize,
                ok: AtomicU32::new(ok),
                errno: AtomicI32::new(errno),
                locks: AtomicU32::new(0),
                unlocks: AtomicU32::new(0),
                unlocked: AtomicU64::new(0),
            })
        }
        fn reset(&self, ok: u32, errno: i32) {
            self.ok.store(ok, Relaxed);
            self.errno.store(errno, Relaxed);
            self.locks.store(0, Relaxed);
            self.unlocks.store(0, Relaxed);
            self.unlocked.store(0, Relaxed);
        }
    }

    impl MemLock for FakeLock {
        fn lock(&self, at: *const u8, len: usize) -> Result<(), i32> {
            let n = self.locks.fetch_add(1, Relaxed) + 1;
            if n > self.ok.load(Relaxed) {
                return Err(self.errno.load(Relaxed));
            }
            // SAFETY: fallocate on the test's memfd.
            let r =
                unsafe { libc::fallocate(self.fd, 0, (at as usize - self.base) as libc::off_t, len as libc::off_t) };
            if r == 0 {
                Ok(())
            } else {
                Err(xh_core::sys::errno())
            }
        }
        fn unlock(&self, _at: *const u8, len: usize) {
            self.unlocks.fetch_add(1, Relaxed);
            self.unlocked.fetch_add(len as u64, Relaxed);
        }
    }

    fn opts(lock: bool, step: u64, unlock_below: u64, locker: &Arc<FakeLock>) -> Options {
        Options { lock, step, unlock_below, locker: locker.clone(), ..Options::default() }
    }

    #[test]
    fn prefault_and_valve() {
        let size = 32 * M;
        let ram = TestRam::new(size).expect("memfd");
        let (base, fd) = (ram.base, ram.raw());
        // What the guest wrote before (the loader's kernel, early allocations) is kept: a prefault changes no byte.
        let b = ram.bytes();
        b[0] = 0x11;
        b[(5 * M + 7) as usize] = 0x22;
        b[(size - 1) as usize] = 0x33;
        assert_eq!(memfd_held(fd), 3 * 4096);
        let r = Ramfix::new(Options { step: 4 * M, ..Options::default() });
        r.prefault(base, size);
        assert!(
            r.done.load(Relaxed)
                && r.prefaulted.load(Relaxed) == size
                && r.locked.load(Relaxed) == 0
                && r.lock_error.load(Relaxed) == 0
        );
        assert_eq!(memfd_held(fd), size);
        assert!(
            b[0] == 0x11 && b[(5 * M + 7) as usize] == 0x22 && b[(size - 1) as usize] == 0x33 && b[M as usize] == 0
        );
        assert!(ram_resident(base, size, 4096) <= size);
        drop(ram);

        // A lock that fails part way (ENOMEM: RLIMIT_MEMLOCK, or the host has not the memory): the prefault stops (an
        // unlocked, unused page is the first the host swaps) and what was locked stays locked (fix-ram-locked,
        // 2026-11-15); the valve still covers it; the boot goes on, the errno is kept.
        let ram = TestRam::new(size).expect("memfd");
        let (base, fd) = (ram.base, ram.raw());
        let fake = FakeLock::new(&ram, 2, libc::ENOMEM);
        let r = Ramfix::new(opts(true, 4 * M, 256 * M, &fake));
        r.prefault(base, size);
        assert!(r.done.load(Relaxed) && r.lock_error.load(Relaxed) == libc::ENOMEM && r.locked.load(Relaxed) == 8 * M);
        assert!(!r.locking.load(Relaxed) && fake.locks.load(Relaxed) == 3 && fake.unlocks.load(Relaxed) == 0);
        assert!(r.prefaulted.load(Relaxed) == 8 * M && memfd_held(fd) == 8 * M);
        assert!(r.unlocked_from(8 * M) == 8 * M && r.unlocked_from(4 * M) == 0); // VmLck short of it: count all
        assert!(r.valve(base, 100 * M) && fake.unlocked.load(Relaxed) == 8 * M && r.locked.load(Relaxed) == 0);
        assert_eq!(r.unlocked_from(8 * M), 0);
        // EPERM at once (no CAP_IPC_LOCK, a zero RLIMIT_MEMLOCK): nothing to unlock.
        assert!(ram_discard_fd(fd, 0, 0, size, 0, size, 4096) == 0 && memfd_held(fd) == 0);
        fake.reset(0, libc::EPERM);
        let r = Ramfix::new(opts(true, 4 * M, 0, &fake));
        r.prefault(base, size);
        assert!(r.lock_error.load(Relaxed) == libc::EPERM && r.locked.load(Relaxed) == 0);
        assert!(
            fake.locks.load(Relaxed) == 1
                && fake.unlocks.load(Relaxed) == 0
                && r.prefaulted.load(Relaxed) == 0
                && memfd_held(fd) == 0
        );
        // A lock that holds: all locked, and the valve opens once, only under its floor.
        fake.reset(1000, 0);
        let r = Ramfix::new(opts(true, 4 * M, 256 * M, &fake));
        r.prefault(base, size);
        assert!(r.locked.load(Relaxed) == size && r.lock_error.load(Relaxed) == 0 && fake.unlocks.load(Relaxed) == 0);
        assert!(!r.valve(base, 300 * M) && r.locked.load(Relaxed) == size);
        assert!(r.valve(base, 200 * M) && r.unlocked.load(Relaxed) && r.locked.load(Relaxed) == 0);
        assert!(r.unlock_available.load(Relaxed) == 200 * M && fake.unlocked.load(Relaxed) == size);
        assert!(!r.valve(base, 100 * M) && fake.unlocks.load(Relaxed) == 1);
        let r = Ramfix::new(opts(true, 4 * M, 0, &fake)); // ram-unlock-below=0: never
        r.prefault(base, size);
        assert!(r.locked.load(Relaxed) == size && !r.valve(base, 0));
        // The valve as the main loop runs it: it reads MemAvailable (a file here) only once the prefault is done and
        // something is locked, and opens once under its level.
        let dir = std::env::temp_dir().join(format!("xnuhost-ramfix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plenty = dir.join("meminfo-plenty");
        let short_of = dir.join("meminfo-short");
        std::fs::write(
            &plenty,
            "MemTotal:       11350016 kB\nMemFree:          100000 kB\nMemAvailable:    1048576 kB\n",
        )
        .unwrap();
        std::fs::write(
            &short_of,
            "MemTotal:       11350016 kB\nMemFree:           50000 kB\nMemAvailable:     204800 kB\n",
        )
        .unwrap();
        fake.reset(1000, 0);
        let with = |path: &std::path::Path| Options {
            meminfo: Some(path.to_string_lossy().into_owned()),
            ..opts(true, 4 * M, 256 * M, &fake)
        };
        let r = Ramfix::new(with(&plenty));
        assert_eq!(r.valve_tick(base), None); // not done yet: not read
        r.prefault(base, size);
        assert!(r.locked.load(Relaxed) == size && !r.short_stop.load(Relaxed));
        assert_eq!(r.valve_tick(base), Some((1024 * M, false)));
        assert_eq!(fake.unlocks.load(Relaxed), 0);
        // the same prefault state, the short file
        let r2 = Ramfix::new(with(&short_of));
        r2.locked.store(size, Relaxed);
        r2.done.store(true, Relaxed);
        assert_eq!(r2.valve_tick(base), Some((200 * M, true)));
        assert!(r2.unlocked.load(Relaxed) && r2.locked.load(Relaxed) == 0 && fake.unlocked.load(Relaxed) == size);
        assert_eq!(r2.valve_tick(base), None); // once: nothing read after
        assert_eq!(fake.unlocks.load(Relaxed), 1);
        // The host short during the prefault: it stops before the next step, what is locked stays for the valve.
        assert!(ram_discard_fd(fd, 0, 0, size, 0, size, 4096) == 0 && memfd_held(fd) == 0);
        fake.reset(1000, 0);
        let r = Ramfix::new(with(&short_of));
        r.prefault(base, size);
        assert!(r.done.load(Relaxed) && r.short_stop.load(Relaxed) && r.short_available.load(Relaxed) == 200 * M);
        assert!(
            r.prefaulted.load(Relaxed) == 0
                && r.locked.load(Relaxed) == 0
                && fake.locks.load(Relaxed) == 0
                && memfd_held(fd) == 0
        );
        // All given back, then a prefault without a lock: a read of each page fills the memfd, every page up to date
        // (mincore counts it).
        ram.bytes()[(3 * M) as usize] = 0x44;
        let r = Ramfix::new(Options { step: 4 * M, ..Options::default() });
        r.prefault(base, size);
        assert!(r.prefaulted.load(Relaxed) == size && memfd_held(fd) == size && ram.bytes()[(3 * M) as usize] == 0x44);
        assert_eq!(ram_resident(base, size, 4096), size);
        // Stopped before it began (the session ended at once): nothing done, the thread can be joined.
        let r = Ramfix::new(Options::default());
        r.stop.store(true, Relaxed);
        r.prefault(base, size);
        assert!(r.done.load(Relaxed) && r.prefaulted.load(Relaxed) == 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn real_lock() {
        let _one = REAL_LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        // A real lock within this host's RLIMIT_MEMLOCK (8 MiB on the PC; root with CAP_IPC_LOCK has none): VmLck shows
        // it.
        let l = memlock_limit();
        if l.rlim_cur != libc::RLIM_INFINITY && l.rlim_cur < 6 * M {
            println!("ramfix: RLIMIT_MEMLOCK {} KiB here: the real lock is not tried", l.rlim_cur >> 10);
            return;
        }
        let ram = TestRam::new(4 * M).expect("memfd");
        let before = sample();
        let r = Ramfix::new(Options { lock: true, step: M, ..Options::default() });
        r.prefault(ram.base, 4 * M);
        let after = sample();
        assert!(r.locked.load(Relaxed) == 4 * M && r.lock_error.load(Relaxed) == 0 && memfd_held(ram.raw()) == 4 * M);
        assert!(after.vm_locked >= before.vm_locked + 4 * M);
        let v = Ramfix::new(Options { unlock_below: 1, ..Options::default() });
        v.locked.store(4 * M, Relaxed);
        assert!(v.valve(ram.base, 0));
        // (other tests of this binary may lock at once: VmLck back to at most what it was plus theirs)
        drop(ram);
        // Past the limit: ENOMEM (or EPERM), a clean fallback.
        if l.rlim_cur != libc::RLIM_INFINITY {
            let big = (l.rlim_cur + 8 * M) & !(M - 1);
            let ram = TestRam::new(big).expect("memfd");
            let r = Ramfix::new(Options { lock: true, step: big, ..Options::default() });
            r.prefault(ram.base, big);
            let e = r.lock_error.load(Relaxed);
            assert!(
                (e == libc::ENOMEM || e == libc::EPERM)
                    && r.locked.load(Relaxed) == 0
                    && r.prefaulted.load(Relaxed) == 0
            );
            assert_eq!(memfd_held(ram.raw()), 0);
        }
    }

    #[test]
    fn lock_on_fault_takes_pages_as_touched() {
        let _one = REAL_LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        // ram-ondemand (option C): the range is marked, a page comes in only when touched, a hole punched frees it, and
        // the valve's munlock ends it. Within this host's RLIMIT_MEMLOCK (the range counts in full in VmLck).
        let l = memlock_limit();
        if l.rlim_cur != libc::RLIM_INFINITY && l.rlim_cur < 6 * M {
            println!("ramfix: RLIMIT_MEMLOCK {} KiB here: the lock on fault is not tried", l.rlim_cur >> 10);
            return;
        }
        let ram = TestRam::new(4 * M).expect("memfd");
        let before = sample();
        lock_on_fault(ram.base, (4 * M) as usize).expect("mlock2 MLOCK_ONFAULT");
        assert!(sample().vm_locked >= before.vm_locked + 4 * M);
        assert_eq!(memfd_held(ram.raw()), 0, "nothing comes in before a touch");
        ram.bytes()[0] = 1;
        ram.bytes()[(2 * M) as usize] = 2;
        // (a host with shmem THP "always" brings 2 MiB at a touch: two pages or two 2 MiB folios)
        let held = memfd_held(ram.raw());
        assert!((2 * 4096..=4 * M).contains(&held), "{held}");
        // A hole punched (ram_discard's way, the free pages given back) frees a locked page.
        // SAFETY: fallocate on our own memfd.
        let punched = unsafe {
            libc::fallocate(ram.raw(), libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE, 0, (2 * M) as i64)
        };
        assert_eq!(punched, 0);
        let left = memfd_held(ram.raw());
        assert!(left < held && left != 0, "{left} of {held}");
        let r = Ramfix::on_demand(Options { unlock_below: 1, ..Options::default() }, 4 * M);
        assert!(r.done.load(Relaxed) && r.opts.on_fault && r.locked.load(Relaxed) == 4 * M);
        assert_eq!(r.unlocked_from(u64::MAX), 0, "the resident count looks at the whole RAM");
        assert!(r.valve(ram.base, 0) && r.unlocked.load(Relaxed) && r.locked.load(Relaxed) == 0);
    }

    #[test]
    fn thread() {
        // The thread: at nice 19, done on its own; the stop joins it.
        let size = 32 * M;
        let ram = TestRam::new(size).expect("memfd");
        let r: &'static Ramfix =
            Box::leak(Box::new(Ramfix::new(Options { meminfo: Some("/proc/meminfo".into()), ..Options::default() })));
        start(r, ram.base, size).unwrap();
        stop(r); // the join: it is not stopped before the first step... unless it had not begun
        assert!(r.done.load(Relaxed));
        let r: &'static Ramfix = Box::leak(Box::new(Ramfix::new(Options::default())));
        start(r, ram.base, size).unwrap();
        while !r.done.load(Acquire) {
            std::thread::yield_now();
        }
        assert!(r.prefaulted.load(Relaxed) == size && memfd_held(ram.raw()) == size);
        stop(r);
    }

    #[test]
    fn facts_and_counters() {
        // What the log says about locking here, and the counters (read only).
        let f = facts();
        assert!(f.contains("RLIMIT_MEMLOCK ") && f.contains("CAP_IPC_LOCK ") && f.contains("cgroup "), "{f}");
        let c = sample();
        assert!(c.major_faults < 1 << 40);
        assert!(c.vm_pte > 0 && c.minor_faults > 0); // VmPTE and minflt read (fix-ram-hugepages)
        assert_eq!(pmd_mapped_self().is_some(), std::path::Path::new("/proc/self/smaps_rollup").exists());
        assert_eq!(kgsl_memory(std::process::id() as i32), ""); // no KGSL here: ""
        let _ = can_lock();
    }

    #[test]
    fn huge_pages() {
        // 2 MiB host pages (fix-ram-hugepages): shmem_enabled's bracketed value decides, from named files under a root.
        let root = std::env::temp_dir().join(format!("xnuhost-hp-{}", std::process::id()));
        let thp = root.join("sys/kernel/mm/transparent_hugepage");
        std::fs::create_dir_all(&thp).unwrap();
        std::fs::create_dir_all(root.join("proc")).unwrap();
        let (se, pmd) = (thp.join("shmem_enabled"), thp.join("hpage_pmd_size"));
        std::fs::write(&pmd, "2097152\n").unwrap();
        std::fs::write(
            root.join("proc/meminfo"),
            "MemTotal:       11350016 kB\nHugePages_Total:       0\nHugePages_Free:        0\nHugepagesize:       2048 kB\n",
        )
        .unwrap();
        let r = root.to_string_lossy().into_owned();
        std::fs::write(&se, "always within_size [advise] never deny force\n").unwrap(); // this PC's
        let h = huge_choose(&r, true);
        assert!(
            h.mode == Huge::Shmem
                && h.shmem_enabled == "advise"
                && h.pmd_size == 2 << 20
                && h.hugetlb_free == 0
                && h.hugetlb_size == 2 << 20
        );
        let h = huge_choose(&r, false); // ram-huge=0 or ram-giveback=1
        assert!(h.mode == Huge::None && h.why.contains("not asked"));
        std::fs::write(&se, "always within_size advise [never] deny force\n").unwrap(); // Linux's default
        let h = huge_choose(&r, true);
        assert!(
            h.mode == Huge::None && h.shmem_enabled == "never" && h.why.contains("the fork's switch would set advise")
        );
        std::fs::write(&se, "always within_size advise never [deny] force\n").unwrap();
        let h = huge_choose(&r, true);
        assert!(h.mode == Huge::None && h.shmem_enabled == "deny");
        std::fs::write(&se, "[always] within_size advise never deny force\n").unwrap();
        let h = huge_choose(&r, true);
        assert!(h.mode == Huge::Shmem && h.shmem_enabled == "always");
        std::fs::write(&pmd, "524288\n").unwrap(); // 512 KiB: a 16 KiB-page kernel
        let h = huge_choose(&r, true);
        assert!(h.mode == Huge::None && h.pmd_size == 512 << 10 && h.why.contains("hpage_pmd_size"));
        std::fs::remove_file(&se).unwrap();
        std::fs::remove_file(&pmd).unwrap();
        let h = huge_choose(&r, true); // no THP at all
        assert!(h.mode == Huge::None && h.pmd_size == 0 && h.shmem_enabled.is_empty());
        assert_eq!(huge_choose("/nonexistent", true).mode, Huge::None);
        // THP_enabled of a status file (a parent's PR_SET_THP_DISABLE, mem.rs): 0 turned off, 1 on, no line unknown
        let status = root.join("status");
        std::fs::write(&status, "Name:\txnuhost\nTHP_enabled:\t0\nVmPTE:\t      48 kB\n").unwrap();
        assert_eq!(thp_disabled_in(&status.to_string_lossy()), Some(true));
        std::fs::write(&status, "Name:\txnuhost\nTHP_enabled:\t1\n").unwrap();
        assert_eq!(thp_disabled_in(&status.to_string_lossy()), Some(false));
        std::fs::write(&status, "Name:\txnuhost\n").unwrap();
        assert_eq!(thp_disabled_in(&status.to_string_lossy()), None);
        assert!(thp_disabled_self().is_some()); // Linux 5.0 or later here
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_return() {
        // The return: the process ends (killed, as the way back kills it) and the host has all of the RAM back at once.
        // 256 MiB prefaulted in a child; Shmem (memfds are shmem) rises by it and falls back when the child is gone.
        // Other agents' tmpfs files move Shmem too: up to three tries.
        let shmem_now = || crate::config::meminfo_value("/proc/meminfo", "Shmem").unwrap_or(0);
        let mut back = false;
        for attempt in 0..3 {
            let big = 256 * M;
            let mut ready = [0i32; 2];
            // SAFETY: a pipe into our array.
            if unsafe { libc::pipe(ready.as_mut_ptr()) } != 0 {
                break;
            }
            let s0 = shmem_now();
            // SAFETY: the child only maps, reads, writes one byte and waits; it never returns into the test harness.
            let child = unsafe { libc::fork() };
            if child == 0 {
                let ok = TestRam::new(big).is_some_and(|ram| {
                    Ramfix::new(Options { step: 16 * M, ..Options::default() }).prefault(ram.base, big);
                    let held = memfd_held(ram.raw()) == big;
                    std::mem::forget(ram);
                    held
                });
                let one = if ok { b'y' } else { b'n' };
                // SAFETY: write one byte to the pipe, then wait to be killed.
                unsafe {
                    libc::write(ready[1], &one as *const u8 as *const libc::c_void, 1);
                    loop {
                        libc::pause();
                    }
                }
            }
            let mut one = 0u8;
            // SAFETY: our pipe's ends; the child's pid.
            let held = unsafe {
                libc::close(ready[1]);
                libc::read(ready[0], &mut one as *mut u8 as *mut libc::c_void, 1) == 1 && one == b'y'
            };
            let s1 = shmem_now();
            // SAFETY: as above.
            unsafe {
                libc::kill(child, libc::SIGKILL);
                libc::waitpid(child, std::ptr::null_mut(), 0);
                libc::close(ready[0]);
            }
            let s2 = shmem_now();
            back = held && s1 + 32 * M >= s0 + big && s2 + 32 * M <= s1 && s2 <= s0 + 32 * M;
            if back {
                break;
            }
            println!(
                "ramfix: the return: Shmem {} -> {} -> {} MiB (try {})",
                s0 >> 20,
                s1 >> 20,
                s2 >> 20,
                attempt + 1
            );
        }
        assert!(back);
    }
}
