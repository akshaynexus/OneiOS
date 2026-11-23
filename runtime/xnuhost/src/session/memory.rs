// SPDX-License-Identifier: MIT
//! main.c's lines about the guest's RAM: the choice at the start (ram=auto, what fits), the RAM held fixed (ramfix),
//! and the main loop's lasting lines: the host's memory every host-mem-log seconds, the host swapping iOS's RAM, the
//! lock's valve, the 2 MiB pages.

use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

use xh_core::{loge, logi};
use xnuhost_abi::XH_FEATURE_RAM_FIXED;

use super::mono_s;
use crate::config::{self, Config, RamBound};
use crate::discard::{self, DISCARD, FIXED_REFUSED};
use crate::mem;
use crate::ramfix::{self, Counters, Ramfix};

/// The host at its lowest (low_watch): a new look at each 64 MiB further down.
const LOW_STEP: u64 = 64 << 20;

/// The memfd's held bytes (st_blocks: swapped pages count too).
fn held(fd: i32) -> Option<u64> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat fills st for a descriptor; its result is checked before st is read.
    (fd >= 0 && unsafe { libc::fstat(fd, st.as_mut_ptr()) } == 0)
        // SAFETY: fstat succeeded, so st is filled.
        .then(|| unsafe { st.assume_init() }.st_blocks as u64 * 512)
}

/// The RAM's bytes in the host's RAM from `from` to the end (mincore of that part; [0, from) counted as in).
fn resident_from(m: &mem::Mem, from: u64) -> u64 {
    if from >= m.size {
        return m.size;
    }
    from + discard::ram_resident(m.host.wrapping_add(from as usize), m.size - from, xh_core::sys::page_size())
}

/// What the main loop's memory lines remember between two looks.
#[derive(Default)]
pub struct MemoryWatch {
    /// the fixed RAM (None: not started, or ram-giveback=1's way)
    pub ramfix: Option<&'static Ramfix>,
    /// the `memory: fixed RAM` line's counters at its last line (first: the start)
    was: Option<Counters>,
    host_last: Option<u64>,
    swap_last: Option<u64>,
    swap_next: u64,
    swap_was: (u64, u64),
    huge_said: bool,
    valve_last: Option<u64>,
    /// the host's pressure line (pressure_watch): its state, its last look and the swap-ins then
    pressure: config::Pressure,
    pressure_last: Option<u64>,
    pressure_swap_ins: u64,
    /// the trim's pressure mode (task.rs, "The trim"): on under the headroom and TRIM_PRESSURE_MARGIN
    trim_pressed: bool,
    /// the host before the prefault (the hold line, held_tick): its memory by user, MemFree, MemAvailable, the THP
    /// counters; and whether the hold line was written
    start: Option<HostLook>,
    held_said: bool,
    /// the host at its lowest MemAvailable since the start (the breakdown at the stop, hostmem.rs): when (s), MemAvailable,
    /// the line's text; the last walk of the processes (when, its list); the start (s)
    low: Option<(u64, u64, String)>,
    low_procs: Option<(u64, String)>,
    started: Option<u64>,
}

/// One look at the host's memory (the hold line).
struct HostLook {
    users: Vec<(&'static str, u64)>,
    free: u64,
    available: u64,
    thp: (u64, u64),
}

/// The host's memory by user now (config::host_users), with the guest's held bytes.
fn host_look(config: &Config) -> Option<HostLook> {
    let v = config::proc_values(&config.ram_meminfo, &config::HOST_MEMORY_KEYS);
    let available = v[9]?;
    let host = config::HostMemory::from_values(&v);
    let guest = held(mem::state().fd).unwrap_or(0).min(host.shmem);
    let kgsl = ramfix::kgsl_value(std::process::id() as i32, "kernel").unwrap_or(0);
    let users = config::host_users(&host, guest, kgsl, ramfix::zram_pool());
    Some(HostLook { users, free: host.free, available, thp: ramfix::thp_file_counts() })
}

impl MemoryWatch {
    fn all_locked(&self, m: &mem::Mem) -> bool {
        self.ramfix.is_some_and(|r| !r.unlocked.load(Relaxed) && r.locked.load(Relaxed) == m.size && m.size != 0)
    }

    /// The guest's RAM held fixed for the session (ramfix.rs, docs/booter.md 2.2 "Fixed RAM"): the kernel is told
    /// before it starts (XH_FEATURE_RAM_FIXED: no free pages given back), and the whole memfd is prefaulted, locked
    /// where Linux allows and it fits, on a nice-19 thread while the boot goes on. ram-giveback=1: 0058's way, nothing
    /// of this.
    pub fn ram_fix_start(&mut self, config: &Config) {
        self.was = Some(ramfix::sample());
        self.start = host_look(config);
        self.started = Some(mono_s());
        // The host in full before the guest takes anything (hostmem.rs): where the RAM that MemAvailable does not
        // count is, and the processes that are left (the takeover's kept services among them).
        let b = crate::hostmem::breakdown(&config.ram_meminfo, true);
        if !b.is_empty() {
            logi!("memory: the host at the start (before the guest's RAM is held; MiB): {b}");
        }
        if config.ram_ondemand {
            self.on_demand_start(config);
            return;
        }
        if config.ram_giveback {
            logi!("memory: ram-giveback=1: the guest's RAM is faulted in as it is used and its free pages are given back (0058); not prefaulted, not locked");
            return;
        }
        crate::hostcalls::update_info(|i| i.features |= XH_FEATURE_RAM_FIXED);
        if !config.ram_prefault {
            logi!("memory: the guest's RAM is held for the session, not prefaulted (ram-prefault=0); free pages are not given back");
            return;
        }
        let m = mem::state();
        let facts = ramfix::facts();
        let mut opts = ramfix::Options {
            lock: config.ram_lock,
            lock_why: (!config.ram_lock).then(|| "ram-lock=0".to_owned()),
            step: 0,
            unlock_below: config.ram_unlock_below,
            meminfo: Some(config.ram_meminfo.clone()),
            ..ramfix::Options::default()
        };
        // A lock is safe only with room beside it: the RAM must be within what fits (config.rs, ram_fit) at the start.
        if opts.lock && config.ram_fit_available != 0 && m.size > config.ram_fit {
            opts.lock = false;
            let r: &'static Ramfix = Box::leak(Box::new(Ramfix::new(opts)));
            // Not prefaulted either: an unlocked page iOS has not used is the first the host swaps (PC boot of
            // 2026-11-14).
            logi!(
                "memory: the guest's RAM is held for the session: {} MiB, not locked and not prefaulted (more than fits beside the session's needs: {} MiB); free pages are not given back (ram-giveback=0); {}",
                m.size >> 20,
                config.ram_fit >> 20,
                facts
            );
            // No prefault runs: done, so that the `memory: fixed RAM` line says "held, not locked" (it said "prefault
            // running" for the whole session before 2026-11-15) and the 2 MiB pages line comes at once.
            r.done.store(true, Release);
            self.ramfix = Some(r);
            return;
        }
        let r: &'static Ramfix = Box::leak(Box::new(Ramfix::new(opts)));
        self.ramfix = Some(r);
        let valve = if r.opts.lock {
            format!(
                " (the valve: unlocked once if host MemAvailable falls under {} MiB, checked once a second)",
                r.opts.unlock_below >> 20
            )
        } else {
            String::new()
        };
        logi!(
            "memory: the guest's RAM is held for the session: {} MiB, prefaulted on a thread at nice 19 in {} MiB steps, {}{}; free pages are not given back (ram-giveback=0); {}",
            m.size >> 20,
            ramfix::STEP >> 20,
            if r.opts.lock { "locked as it goes (mlock)" } else { r.opts.lock_why.as_deref().unwrap_or("") },
            valve,
            facts
        );
        if ramfix::start(r, m.host, m.size).is_err() {
            loge!("memory: the guest's RAM is not prefaulted");
        }
    }

    /// ram-ondemand=1 (option C, docs/booter.md 2.2 "On demand"; off by default, against the owner's fixed-RAM rule of
    /// 2026-11-15, for the owner to choose): the RAM is locked on fault, never prefaulted, and its free pages are given
    /// back (no XH_FEATURE_RAM_FIXED: kernel 0058 reports them). The valve runs as for a whole lock.
    fn on_demand_start(&mut self, config: &Config) {
        let m = mem::state();
        let over = m.size.saturating_sub(config.ram_fit);
        match ramfix::lock_on_fault(m.host, m.size as usize) {
            Ok(()) => {
                let opts = ramfix::Options {
                    unlock_below: config.ram_unlock_below,
                    meminfo: Some(config.ram_meminfo.clone()),
                    ..ramfix::Options::default()
                };
                self.ramfix = Some(Box::leak(Box::new(Ramfix::on_demand(opts, m.size))));
                logi!(
                    "memory: ram-ondemand=1 (option C, against the fixed-RAM rule): the guest's RAM, {} MiB ({} MiB of it more than fits locked beside the session), is faulted in as iOS uses it and each page is locked as it comes in (mlock2 MLOCK_ONFAULT: the host never swaps it); iOS's free pages are given back (0058); the valve: unlocked once if host MemAvailable falls under {} MiB (then the host may swap it); past that the kernel's OOM killer ends xnuhost and the watcher returns to One UI; {}",
                    m.size >> 20,
                    over >> 20,
                    config.ram_unlock_below >> 20,
                    ramfix::facts()
                );
            }
            Err(e) => loge!(
                "memory: ram-ondemand=1: mlock2(MLOCK_ONFAULT) of {} MiB failed ({}): the guest's RAM is faulted in as used and given back when free, not locked: the host may swap it",
                m.size >> 20,
                xh_core::sys::strerror(e)
            ),
        }
    }

    /// Every 5 s: the guest's RAM that the host swapped out (held, st_blocks, less in the host's RAM, mincore), said in
    /// plain words once past 64 MiB and again at each further 256 MiB (swap_note): a swapped page stalls whichever vCPU
    /// or task touches it next (the phone, 2026-11-14: 2126 MiB swapped out, Settings frozen). Read only. Cheap
    /// (fix-ram-locked, 2026-11-15): while the whole RAM is locked nothing of it can be swapped, so nothing is read at
    /// all; with part of it locked the mincore covers only the rest.
    pub fn swap_watch(&mut self, config: &Config) {
        let now = mono_s();
        if self.swap_last.is_some_and(|last| now - last < 5) {
            return;
        }
        let m = mem::state();
        if self.all_locked(&m) {
            self.swap_last = None; // the next look (after a valve) starts afresh
            return;
        }
        // The host's swap traffic over the last interval (/proc/vmstat pswpin, pswpout: pages; zram on the phone, where
        // it is also heat: 2026-11-14, about 9.5 MB/s in and 8 MB/s out while iOS's RAM was swapped).
        let (now_ins, now_outs) = ramfix::vmstat_swaps().unwrap_or((0, 0));
        let seconds = self.swap_last.map_or(0, |last| now - last);
        let (was_ins, was_outs) = self.swap_was;
        let (ins, outs) = if seconds != 0 && now_ins >= was_ins && now_outs >= was_outs {
            ((now_ins - was_ins) / seconds, (now_outs - was_outs) / seconds)
        } else {
            (0, 0)
        };
        self.swap_was = (now_ins, now_outs);
        self.swap_last = Some(now);
        if m.host.is_null() {
            return;
        }
        let Some(held) = held(m.fd) else { return };
        let r = self.ramfix;
        let locked = r.map_or(0, |r| r.locked.load(Relaxed));
        let unlocked = r.is_some_and(|r| r.unlocked.load(Relaxed));
        let in_ram = resident_from(&m, if unlocked { 0 } else { locked });
        let swapped = held.saturating_sub(in_ram);
        if !config::swap_note(swapped, &mut self.swap_next) {
            return;
        }
        let available = config::meminfo(&config.ram_meminfo).map_or(0, |(_, a)| a);
        let page = xh_core::sys::page_size();
        loge!(
            "memory: the host swaps iOS's RAM: {} MiB (host MemAvailable {} MiB); host swap-ins {} pages/s ({:.1} MB/s), swap-outs {} pages/s ({:.1} MB/s)",
            swapped >> 20,
            available >> 20,
            ins,
            (ins * page) as f64 / 1e6,
            outs,
            (outs * page) as f64 / 1e6
        );
    }

    /// Every 5 s: the host's pressure (docs/booter.md 2.2, "The host's pressure"). One line when MemAvailable falls
    /// under the headroom (ram-session-reserve) or the host swaps in PRESSURE_SWAP_IN a second or more, with the host's
    /// memory by user, largest first, and xnuhost's OOM score; one line when it is over (30 s calm). Nothing in
    /// between: the memory lines every host-mem-log seconds go on. The phone, 2026-11-23 19:06: xnuhost ended without a
    /// line at MemAvailable 1077 MiB and swap-ins of 42 MB/s, and nothing said what held the host's RAM. Cheap: one read
    /// of /proc/meminfo and one of /proc/vmstat every 5 s; the rest (zram, KGSL, the OOM score) once per event.
    pub fn pressure_watch(&mut self, config: &Config) {
        let now = mono_s();
        if self.pressure_last.is_some_and(|last| now - last < 5) {
            return;
        }
        let (ins, _) = ramfix::vmstat_swaps().unwrap_or((0, 0));
        let seconds = self.pressure_last.map_or(0, |last| now - last);
        let page = xh_core::sys::page_size();
        let rate = if seconds != 0 && ins >= self.pressure_swap_ins {
            (ins - self.pressure_swap_ins) * page / seconds
        } else {
            0
        };
        self.pressure_last = Some(now);
        self.pressure_swap_ins = ins;
        let v = config::proc_values(&config.ram_meminfo, &config::HOST_MEMORY_KEYS);
        let Some(available) = v[9] else { return };
        let reserve = config.ram_session_reserve;
        self.low_watch(config, now, available);
        // The trim's pressure mode (task.rs, "The trim"): with MemAvailable under the headroom and its margin, every task
        // idle 2 s is trimmed; off again 128 MiB above that (no flapping at the line).
        let near = reserve + config::TRIM_PRESSURE_MARGIN;
        let pressed = available < if self.trim_pressed { near + config::PRESSURE_CALM_MARGIN } else { near };
        if pressed != self.trim_pressed && config.task_trim {
            self.trim_pressed = pressed;
            crate::task::trim_pressure(pressed);
            let what = if pressed {
                format!(
                    "under the headroom and {} MiB: tasks idle {} s are trimmed",
                    config::TRIM_PRESSURE_MARGIN >> 20,
                    crate::task::trim_idle_s(config.task_trim_idle, true)
                )
            } else {
                format!("tasks idle {} s are trimmed again", config.task_trim_idle)
            };
            logi!("tasks: trim: host MemAvailable {} MiB: {}", available >> 20, what);
        }
        match self.pressure.step(now, available, rate, seconds, reserve) {
            None => {}
            Some(config::PressureNote::Start { short, swapping }) => {
                let host = config::HostMemory::from_values(&v);
                let m = mem::state();
                let pid = std::process::id() as i32;
                let guest = held(m.fd).unwrap_or(0).min(host.shmem);
                let kgsl = ramfix::kgsl_value(pid, "kernel").unwrap_or(0);
                let blobs = ramfix::kgsl_value(pid, "imported_mem").unwrap_or(0);
                let users = config::host_users(&host, guest, kgsl, ramfix::zram_pool());
                let swap_used = v[10].unwrap_or(0).saturating_sub(v[11].unwrap_or(0));
                let tasks = crate::task::live();
                let per_task = if tasks != 0 {
                    format!(
                        "; page tables {} KiB for each of {} live tasks",
                        (host.page_tables >> 10) / tasks as u64,
                        tasks
                    )
                } else {
                    String::new()
                };
                let (score, adj) = ramfix::oom_score();
                let why = match (short, swapping) {
                    (true, true) => format!(
                        "MemAvailable {} MiB, under the {} MiB headroom, and swap-ins of {:.1} MB/s",
                        available >> 20,
                        reserve >> 20,
                        rate as f64 / 1e6
                    ),
                    (true, false) => {
                        format!("MemAvailable {} MiB, under the {} MiB headroom", available >> 20, reserve >> 20)
                    }
                    _ => format!("swap-ins of {:.1} MB/s (MemAvailable {} MiB)", rate as f64 / 1e6, available >> 20),
                };
                loge!(
                    "memory: the host is short of memory: {}; its RAM by user (MiB): {}{}; GPU blobs {} MiB, inside the guest's RAM; host swap {} MiB used; xnuhost's oom_score {} (oom_score_adj {}: the kernel's OOM killer ends the highest first)",
                    why,
                    config::host_users_text(&users),
                    per_task,
                    blobs >> 20,
                    swap_used >> 20,
                    score.map_or("-".to_owned(), |s| s.to_string()),
                    adj.map_or("-".to_owned(), |s| s.to_string())
                );
            }
            Some(config::PressureNote::Over { seconds, lowest, swapped_in }) => {
                logi!(
                    "memory: the host's shortage is over after {} s: lowest MemAvailable {} MiB, swap-ins {} MiB in all; now MemAvailable {} MiB",
                    seconds,
                    lowest >> 20,
                    swapped_in >> 20,
                    available >> 20
                );
            }
        }
    }

    /// The host at its lowest (hostmem.rs): at each new low of MemAvailable, 64 MiB under the last, a look in full (the
    /// processes walked at most once a minute), kept for the stop's line. Cheap: a handful of looks in a session.
    fn low_watch(&mut self, config: &Config, now: u64, available: u64) {
        if self.started.is_none() || self.low.as_ref().is_some_and(|l| available + LOW_STEP > l.1) {
            return;
        }
        let b = crate::hostmem::breakdown(&config.ram_meminfo, false);
        if b.is_empty() {
            return;
        }
        self.low = Some((now, available, b));
        // The processes (one read of each /proc/PID/status, the iOS tasks among them): at most once a minute.
        if self.low_procs.as_ref().is_none_or(|(walked, _)| now - walked >= 60) {
            self.low_procs = Some((now, crate::hostmem::procs_text(&crate::hostmem::walk_procs(), 14)));
        }
    }

    /// Every host-mem-log seconds (or `now`): what the host has left and how much of the guest's RAM it holds (the
    /// memfd's pages that were touched), what the guest gave back with ram_discard, the guest's RAM the host swapped,
    /// and the fixed RAM's state. The host's low-memory killer may end xnuhost when the host runs out: then the watcher
    /// returns to One UI. Read only.
    pub fn host_memory_tick(&mut self, config: &Config, now: bool) {
        let t = mono_s();
        if config.host_mem_log == 0 && !now {
            return;
        }
        if !now && self.host_last.is_some_and(|last| t - last < config.host_mem_log as u64) {
            return;
        }
        self.host_last = Some(t);
        // One read of /proc/meminfo for every figure here.
        let keys = [
            "MemTotal",
            "MemAvailable",
            "SwapTotal",
            "SwapFree",
            "PageTables",
            "Mlocked",
            "ShmemHugePages",
            "ShmemPmdMapped",
            "SUnreclaim",
            "KernelStack",
        ];
        let mi: Vec<u64> =
            config::proc_values(&config.ram_meminfo, &keys).into_iter().map(|v| v.unwrap_or(0)).collect();
        let (total, available, swap_total, swap_free, tables) = (mi[0], mi[1], mi[2], mi[3], mi[4]);
        let m = mem::state();
        let held = held(m.fd).unwrap_or(0);
        if total == 0 {
            return;
        }
        // What the guest gave back since the start (ram_discard: its free pages, kernel 0058), and the time it took.
        logi!(
            "memory: host MemAvailable {} MiB of {} MiB; the guest's RAM holds {} of {} MiB; given back {} MiB of free pages in {} calls ({} failed, {} ms)",
            available >> 20,
            total >> 20,
            held >> 20,
            m.size >> 20,
            DISCARD.bytes.load(Relaxed) >> 20,
            DISCARD.calls.load(Relaxed),
            DISCARD.failed.load(Relaxed),
            DISCARD.ns.load(Relaxed) / 1_000_000
        );
        // Where the host's memory is besides (2026-11-13): the guest's RAM that the host swapped out (zram on the
        // phone), the host's swap in use, and its page tables (each iOS task is a Linux process mapping the memfd). A
        // second line, so that the one above keeps its form for the log tools. The locked part is resident while VmLck
        // covers it: the mincore looks only at the rest (nothing with the whole RAM locked).
        let mut c = ramfix::sample();
        c.mlocked = mi[5];
        let from = self.ramfix.map_or(0, |r| r.unlocked_from(c.vm_locked));
        let in_ram = if m.host.is_null() { 0 } else { resident_from(&m, from) };
        // The iOS tasks' share (2026-11-23): the page tables less xnuhost's own, per live task, and the kernel's
        // unreclaimable slab (their VMAs) and stacks (their threads), so a log shows what the trim (task.rs) gives back.
        let tasks = crate::task::live();
        let per_task = if tasks != 0 {
            format!("; {} KiB a live task ({} live)", (tables.saturating_sub(c.vm_pte) >> 10) / tasks as u64, tasks)
        } else {
            String::new()
        };
        logi!(
            "memory: the guest's RAM: {} MiB in the host's RAM, {} MiB swapped out by the host; host swap {} of {} MiB used, page tables {} MiB (xnuhost's own {} MiB{}); 2 MiB pages: host ShmemHugePages {} MiB, ShmemPmdMapped {} MiB; unreclaimable slab {} MiB, kernel stacks {} MiB",
            in_ram >> 20,
            held.saturating_sub(in_ram) >> 20,
            swap_total.saturating_sub(swap_free) >> 20,
            swap_total >> 20,
            tables >> 20,
            c.vm_pte >> 20,
            per_task,
            mi[6] >> 20,
            mi[7] >> 20,
            mi[8] >> 20,
            mi[9] >> 20
        );
        // The fixed RAM (2026-11-13, ramfix.rs): what is held and locked, what was refused, and since the last line the
        // host's swap traffic (all processes) and this process's faults that needed the host's I/O. With the RAM held
        // and locked, `held` stays at the RAM's size, `swapped out` at 0, and the KGSL figures flat once the UI is up.
        let k = ramfix::kgsl_memory(std::process::id() as i32);
        let kgsl = if k.is_empty() { String::new() } else { format!("; KGSL: {k}") };
        let r = self.ramfix;
        let state = if config.ram_ondemand && r.is_some_and(|r| r.unlocked.load(Relaxed)) {
            "on demand, unlocked by the valve (ram-ondemand=1)"
        } else if config.ram_ondemand && r.is_some() {
            "on demand, each page locked as it comes in, free pages given back (ram-ondemand=1)"
        } else if config.ram_giveback {
            "given back as freed (ram-giveback=1)"
        } else if !config.ram_prefault {
            "held, not prefaulted"
        } else if !r.is_some_and(|r| r.done.load(Acquire)) {
            "held, prefault running"
        } else if r.is_some_and(|r| r.unlocked.load(Relaxed)) {
            "held, unlocked by the valve"
        } else if self.all_locked(&m) {
            "held and locked"
        } else if r.is_some_and(|r| r.locked.load(Relaxed) != 0) {
            "held, locked in part"
        } else {
            "held, not locked"
        };
        let d = |now: u64, was: fn(&Counters) -> u64| self.was.as_ref().map_or(0, |w| now.wrapping_sub(was(w)));
        logi!(
            "memory: fixed RAM: {}; prefaulted {} of {} MiB, locked {} MiB (VmLck; host Mlocked {} MiB); give-back refused {} calls; since the last line: host swap-ins +{}, swap-outs +{} pages, xnuhost major faults +{}, minor +{}{}",
            state,
            r.map_or(0, |r| r.prefaulted.load(Relaxed)) >> 20,
            m.size >> 20,
            c.vm_locked >> 20,
            c.mlocked >> 20,
            FIXED_REFUSED.load(Relaxed),
            d(c.swap_ins, |w| w.swap_ins),
            d(c.swap_outs, |w| w.swap_outs),
            d(c.major_faults, |w| w.major_faults),
            d(c.minor_faults, |w| w.minor_faults),
            kgsl
        );
        self.was = Some(c);
    }

    /// The window's 2 MiB pages (fix-ram-hugepages): once when the prefault is done (or at once without one) and at
    /// the stop. smaps_rollup walks every mapping of this process, so not every host-mem-log. ShmemPmdMapped is what
    /// xnuhost maps with 2 MiB entries: with shmem THP and the RAM prefaulted, the whole window; the tasks map the same
    /// folios 4 KiB at a time.
    pub fn huge_report(&self, when: &str) {
        let m = mem::state();
        if m.host.is_null() {
            return;
        }
        let Some((shmem, file, anon)) = ramfix::pmd_mapped_self() else { return };
        let c = ramfix::sample();
        // With 4 KiB entries xnuhost's window alone takes 8 bytes per 4 KiB page: the size / 512 that 2 MiB entries save.
        logi!(
            "memory: 2 MiB pages{}: xnuhost maps {} of {} MiB of the guest's RAM with 2 MiB entries (ShmemPmdMapped; FilePmdMapped {}, AnonHugePages {} MiB); its page tables {} MiB (VmPTE; 4 KiB entries for the whole window would be {} MiB)",
            when,
            shmem >> 20,
            m.size >> 20,
            file >> 20,
            anon >> 20,
            c.vm_pte >> 20,
            (m.size / 512) >> 20
        );
        // None of it in 2 MiB pages: why (2026-11-23: the phone said `yes` and then 0 of 5632 MiB). The host's THP
        // counters since the start: an allocation tried and failed (no free 2 MiB block) moves thp_file_fallback;
        // neither moving means shmem never tried (THP_enabled 0, shmem_enabled, or no VM_HUGEPAGE on the mapping).
        if shmem == 0 {
            let (alloc, fallback) = ramfix::thp_file_counts();
            let (alloc0, fallback0) = self.start.as_ref().map_or((alloc, fallback), |s| s.thp);
            let defrag = xh_core::fs::read_line("/sys/kernel/mm/transparent_hugepage/defrag").unwrap_or_default();
            let thp = match ramfix::thp_disabled_self() {
                Some(true) => "0 (PR_SET_THP_DISABLE)",
                Some(false) => "1",
                None => "?",
            };
            let why = if fallback != fallback0 {
                "tried, no free 2 MiB block"
            } else if alloc == alloc0 {
                "never tried"
            } else {
                "some given"
            };
            logi!(
                "memory: 2 MiB pages: none: THP_enabled {}; since the start the host's thp_file_alloc +{}, thp_file_fallback +{} ({}); defrag {}",
                thp,
                alloc.wrapping_sub(alloc0),
                fallback.wrapping_sub(fallback0),
                why,
                defrag.trim()
            );
        }
    }

    /// Once, when the prefault is done: what holding the guest's RAM cost the host, MemAvailable at the start less
    /// now, and by user (ram-hold-percent, config.rs: 5.9-7.3 % of 5632 MiB on 2026-11-23, nothing named it). Two reads
    /// of /proc/meminfo and of /proc/vmstat in a session.
    fn held_tick(&mut self, config: &Config) {
        // ram-ondemand holds nothing up front: there is no hold to measure (the 30 s lines show what comes in).
        if self.held_said || config.ram_ondemand || !self.ramfix.is_some_and(|r| r.done.load(Acquire)) {
            return;
        }
        self.held_said = true;
        let (Some(was), Some(now)) = (self.start.as_ref(), host_look(config)) else { return };
        let m = mem::state();
        let cost = was.available as i64 - now.available as i64 - m.size as i64;
        logi!(
            "memory: holding the guest's RAM: host MemAvailable {} -> {} MiB for {} MiB held, {:+} MiB beyond it ({:.1} % of it; ram-hold-percent {}); by user since the start (MiB): {}",
            was.available >> 20,
            now.available >> 20,
            m.size >> 20,
            cost / (1 << 20),
            cost as f64 * 100.0 / m.size.max(1) as f64,
            config.ram_hold_percent,
            config::host_change_text(&was.users, was.free, &now.users, now.free)
        );
    }

    pub fn huge_tick(&mut self, config: &Config) {
        self.held_tick(config);
        if self.huge_said || !mem::state().huge {
            return;
        }
        if config.ram_prefault && !self.ramfix.is_some_and(|r| r.done.load(Acquire)) {
            return;
        }
        self.huge_said = true;
        self.huge_report(if config.ram_prefault { " (the prefault done)" } else { "" });
    }

    /// Once a second while the guest's RAM is locked: the valve (ramfix.rs). On the main thread (nice 0, SCHED_OTHER),
    /// so busy vCPUs cannot starve it as they could the prefault's nice-19 thread, where it ran before 2026-11-15. One
    /// read of /proc/meminfo a second while locked, nothing otherwise. The munlock it may do (once a session) holds
    /// this loop for as long as it takes; the fork's kill is the backstop under everything.
    pub fn ram_valve_tick(&mut self) {
        let now = mono_s();
        if self.valve_last == Some(now) {
            return;
        }
        self.valve_last = Some(now);
        let Some(r) = self.ramfix else { return };
        if let Some((available, true)) = r.valve_tick(mem::state().host) {
            loge!(
                "memory: host MemAvailable {} MiB is under ram-unlock-below ({} MiB): the guest's RAM is unlocked for the rest of the session; the host may swap it now (it stays held, nothing is given back)",
                available >> 20,
                r.opts.unlock_below >> 20
            );
        }
    }

    /// The stop: what was held to the end; the prefault ends.
    pub fn stop(&self) {
        if let Some((at, available, text)) = &self.low {
            let start = self.started.unwrap_or(0);
            let procs = self.low_procs.as_ref().map_or(String::new(), |(walked, list)| {
                format!(
                    "; processes by anonymous memory ({} s after the start; MiB): {list}",
                    walked.saturating_sub(start)
                )
            });
            logi!(
                "memory: the host at its lowest MemAvailable ({} MiB, {} s after the start; MiB): {}{}",
                available >> 20,
                at.saturating_sub(start),
                text,
                procs
            );
        }
        let locked = self.ramfix.map_or(0, |r| {
            ramfix::stop(r);
            r.locked.load(Relaxed)
        });
        logi!(
            "memory: the guest's RAM: {} MiB held to the end, {} MiB of it locked; the exit gives all of it back to the host",
            mem::state().size >> 20,
            locked >> 20
        );
    }
}

/// The guest's RAM as chosen (config.rs, ram=auto), once at the start.
pub fn ram_choice_log(config: &Config) {
    // What fits beside the session's needs at its peak (config.rs ram_fit, 2026-11-14, the terms of 2026-11-23):
    // MemAvailable at the start, less the headroom, the GPU's allowance, the tasks' page tables and kernel memory, and
    // the share of the guest that holding it costs; and what that leaves the host at the peak with the size chosen.
    let fits = if config.ram_fit_available != 0 {
        let t = config.fit_terms();
        let pagetables = if t.pagetable_percent != 0 {
            format!(" and {}% for page tables", t.pagetable_percent)
        } else {
            String::new()
        };
        // With the trim (task.rs): of the tasks, `awake` at the full cost and the others trimmed.
        let tasks = if t.awake < t.tasks {
            format!(
                "{} tasks, {} awake at {} KiB and {} trimmed at {} KiB",
                t.tasks,
                t.awake,
                t.task_cost >> 10,
                t.tasks - t.awake,
                t.idle_cost >> 10
            )
        } else {
            format!("{} tasks at {} KiB", t.tasks, t.task_cost >> 10)
        };
        format!(
            "what fits beside the session's needs{}: {} MiB (MemAvailable {} MiB less {} MiB headroom{}, {} MiB for the GPU's own, {} ({} MiB: their page tables and kernel memory) and {}% of the guest for holding it{}); the host keeps about {} MiB at the peak",
            if config.ram_fit_terms_old { " (ram-fit-terms=old: the terms before 2026-11-23)" } else { "" },
            config.ram_fit >> 20,
            config.ram_fit_available >> 20,
            t.reserve >> 20,
            if config.ram_lock_expected {
                " with the lock"
            } else if config.ram_lock {
                " (the lock is not expected to work here)"
            } else {
                ""
            },
            t.gpu >> 20,
            tasks,
            t.tasks_bytes() >> 20,
            t.hold_percent,
            pagetables,
            t.left_at_peak(config.ram_fit_available, config.ram_bytes) >> 20
        )
    } else {
        String::new()
    };
    if config.ram_auto_total == 0 {
        logi!(
            "memory: the guest gets {} MiB (ram={}){}{}",
            config.ram_bytes >> 20,
            if config.given("ram") { "SIZE" } else { "auto: no MemTotal, 4 GiB" },
            if fits.is_empty() { "" } else { "; " },
            fits
        );
        // A size given by hand that the host cannot hold: Linux will swap the guest (zram), and every page iOS touches
        // there stalls it.
        if config.ram_fit_available != 0 && config.ram_bytes > config.ram_fit {
            if config.ram_ondemand {
                loge!(
                    "memory: ram={} MiB with ram-ondemand=1 is {} MiB more than fits locked beside the session's needs ({} MiB): that part is backed only while iOS uses it; when iOS fills it the host runs short: the trim's pressure mode, then the valve (the host may swap iOS's RAM from there), then the kernel's OOM killer ends xnuhost and the watcher returns to One UI",
                    config.ram_bytes >> 20,
                    config.ram_bytes.saturating_sub(config.ram_fit) >> 20,
                    config.ram_fit >> 20
                );
                return;
            }
            loge!(
                "memory: ram={} MiB is more than this host can hold without swapping it ({} MiB fit): the host will swap iOS's RAM; give ram=auto or ram={}M",
                config.ram_bytes >> 20,
                config.ram_fit >> 20,
                (config.ram_fit >> 20) & !255
            );
        }
        return;
    }
    let (total, floor) = (config.ram_auto_total, config.ram_host_floor);
    let bound = match config.ram_auto_bound {
        Some(RamBound::Share) => "the share",
        Some(RamBound::Floor) => "the floor",
        Some(RamBound::Available) => "what fits",
        None => "?",
    };
    logi!(
        "memory: ram=auto: host MemTotal {} MiB, MemAvailable {} MiB; the guest gets {} MiB: the least of {}% ({} MiB), MemTotal less the host's floor of {} MiB ({} MiB) and {}, in 256 MiB steps ({} decides)",
        total >> 20,
        config.ram_auto_available >> 20,
        config.ram_bytes >> 20,
        config.ram_percent,
        (total / 100 * config.ram_percent as u64) >> 20,
        floor >> 20,
        total.saturating_sub(floor) >> 20,
        if config.ram_fit_on { fits.as_str() } else { "nothing else (ram-fit=0)" },
        bound
    );
}
