// SPDX-License-Identifier: MIT
//! The host's memory in full (docs/booter.md 2.2, "The host's breakdown"; 2026-11-23, the owner's 10 GB goal): where
//! the RAM that MemAvailable does not count is. The takeover session of 2026-11-23 21:11 had MemTotal 11084 MiB and
//! MemAvailable 8458 MiB at xnuhost's start (after the switch's stops), and no line said what held the other 2626 MiB:
//! the kernel's own (slab, stacks, page tables, vmalloc, per-CPU), the kept processes, zram's pool, the dma-buf heaps'
//! page pools, KGSL's pool, the vendor's regions (Samsung's RBIN, ION), the watermarks MemAvailable keeps back.
//!
//! One line at the start (before the guest's RAM is held) and one at the session's lowest MemAvailable (written at the
//! stop): `memory: the host at the start ...` and `memory: the host at its lowest ...`. Read only, and cheap: a few
//! small files, one walk of /proc/PID/status (at most once a minute). The pure parts below are host-tested.

use std::fmt::Write as _;

use xh_core::fs::read_line;

const MIB: u64 = 1 << 20;

/// /proc/meminfo's keys that the line prints in its own words (or that are not RAM of their own: sums, limits, swap,
/// shares of other figures, the address space). Every other key with 1 MiB or more is printed as the kernel names it:
/// the vendor's (Samsung's Rbin*, ION_*, GPU*, ...), CMA, and what a newer kernel adds.
const STANDARD: &[&str] = &[
    "MemTotal",
    "MemFree",
    "MemAvailable",
    "Buffers",
    "Cached",
    "SwapCached",
    "Active",
    "Inactive",
    "Active(anon)",
    "Inactive(anon)",
    "Active(file)",
    "Inactive(file)",
    "Unevictable",
    "Mlocked",
    "SwapTotal",
    "SwapFree",
    "Zswap",
    "Zswapped",
    "Dirty",
    "Writeback",
    "AnonPages",
    "Mapped",
    "Shmem",
    "KReclaimable",
    "Slab",
    "SReclaimable",
    "SUnreclaim",
    "KernelStack",
    "ShadowCallStack",
    "PageTables",
    "SecPageTables",
    "NFS_Unstable",
    "Bounce",
    "WritebackTmp",
    "CommitLimit",
    "Committed_AS",
    "VmallocTotal",
    "VmallocUsed",
    "VmallocChunk",
    "Percpu",
    "HardwareCorrupted",
    "AnonHugePages",
    "ShmemHugePages",
    "ShmemPmdMapped",
    "FileHugePages",
    "FilePmdMapped",
    "Unaccepted",
    "HugePages_Total",
    "HugePages_Free",
    "HugePages_Rsvd",
    "HugePages_Surp",
    "Hugepagesize",
    "Hugetlb",
    "DirectMap4k",
    "DirectMap2M",
    "DirectMap1G",
];

/// /proc/meminfo as (key, bytes): `kB` values times 1024, others (HugePages_*) as printed.
pub fn parse_meminfo(text: &str) -> Vec<(String, u64)> {
    text.lines()
        .filter_map(|l| {
            let (key, rest) = l.split_once(':')?;
            let mut words = rest.split_whitespace();
            let n: u64 = words.next()?.parse().ok()?;
            let unit = if words.next() == Some("kB") { 1024 } else { 1 };
            Some((key.trim().to_owned(), n * unit))
        })
        .collect()
}

fn get(m: &[(String, u64)], key: &str) -> u64 {
    m.iter().find(|(k, _)| k == key).map_or(0, |(_, v)| *v)
}

/// The meminfo part of the line, in MiB: "MemTotal T, MemFree F, MemAvailable A; anonymous .., page cache .. (dirty ..),
/// shmem .., mlocked .., unevictable ..; the kernel's: unreclaimable slab .., reclaimable slab .., kernel stacks ..,
/// page tables .., vmalloc .., per-CPU ..; swap U of S used (cached ..); other keys: CmaTotal .., RbinTotal .., ...".
pub fn meminfo_text(m: &[(String, u64)]) -> String {
    let g = |k: &str| get(m, k) / MIB;
    let mut s = format!(
        "MemTotal {}, MemFree {}, MemAvailable {}; anonymous {}, page cache {} (dirty {}), shmem {}, mlocked {}, unevictable {}; the kernel's: unreclaimable slab {}, reclaimable slab {}, kernel stacks {}, page tables {}, vmalloc {}, per-CPU {}; swap {} of {} used (swap cache {})",
        g("MemTotal"),
        g("MemFree"),
        g("MemAvailable"),
        g("AnonPages"),
        get(m, "Cached").saturating_sub(get(m, "Shmem")) / MIB,
        g("Dirty"),
        g("Shmem"),
        g("Mlocked"),
        g("Unevictable"),
        g("SUnreclaim"),
        g("SReclaimable"),
        g("KernelStack"),
        g("PageTables"),
        g("VmallocUsed"),
        g("Percpu"),
        get(m, "SwapTotal").saturating_sub(get(m, "SwapFree")) / MIB,
        g("SwapTotal"),
        g("SwapCached"),
    );
    let others: Vec<String> = m
        .iter()
        .filter(|(k, v)| *v >= MIB && !STANDARD.contains(&k.as_str()))
        .map(|(k, v)| format!("{k} {}", v / MIB))
        .collect();
    if !others.is_empty() {
        let _ = write!(s, "; other keys: {}", others.join(", "));
    }
    s
}

/// One zram device's mm_stat (orig_data_size compr_data_size mem_used_total mem_limit mem_used_max same_pages
/// pages_compacted huge_pages ...) as "zram0: P MiB of pages in R MiB of RAM (compressed C, most R' MiB; same pages
/// N), backing_dev B"; None when the line has fewer than five numbers.
pub fn zram_text(name: &str, mm_stat: &str, backing: Option<&str>, page: u64) -> Option<String> {
    let f: Vec<u64> = mm_stat.split_whitespace().filter_map(|w| w.parse().ok()).collect();
    if f.len() < 5 {
        return None;
    }
    let same = f.get(5).map_or(String::new(), |n| format!("; same pages {}", n * page / MIB));
    Some(format!(
        "{name}: {} MiB of pages in {} MiB of RAM (compressed {}, most {} MiB{same}), backing_dev {}",
        f[0] / MIB,
        f[2] / MIB,
        f[1] / MIB,
        f[4] / MIB,
        backing.map(str::trim).filter(|b| !b.is_empty()).unwrap_or("?")
    ))
}

/// The high watermarks of /proc/zoneinfo, summed (pages): what MemAvailable keeps back of MemFree (with lowmem_reserve,
/// not counted here).
pub fn zoneinfo_high(text: &str) -> u64 {
    text.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            (w.next() == Some("high")).then(|| w.next()?.parse::<u64>().ok()).flatten()
        })
        .sum()
}

/// One process of the walk: its Name, RssAnon, RssShmem and VmSwap (bytes).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Proc {
    pub name: String,
    pub anon: u64,
    pub shmem: u64,
    pub swap: u64,
}

/// /proc/PID/status's fields of a Proc; None without a Name (a kernel thread has no RssAnon: 0).
pub fn parse_status(text: &str) -> Option<Proc> {
    let mut p = Proc::default();
    let kb = |rest: &str| rest.split_whitespace().next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0) * 1024;
    for l in text.lines() {
        let Some((key, rest)) = l.split_once(':') else { continue };
        match key {
            "Name" => p.name = rest.trim().to_owned(),
            "RssAnon" => p.anon = kb(rest),
            "RssShmem" => p.shmem = kb(rest),
            "VmSwap" => p.swap = kb(rest),
            _ => {}
        }
    }
    (!p.name.is_empty()).then_some(p)
}

/// The processes by name (the iOS tasks all share one), largest anonymous memory first, the first `top` of those with
/// 1 MiB or more: "surfaceflinger 92 (swap 31), android.hardware.graphics.composer 40, xnuhost-task x412 380 (shmem
/// 9), ...; all N processes: anonymous A MiB, swap S MiB".
pub fn procs_text(procs: &[Proc], top: usize) -> String {
    let mut by: Vec<(String, u32, Proc)> = Vec::new();
    for p in procs {
        match by.iter_mut().find(|(n, _, _)| *n == p.name) {
            Some((_, count, sum)) => {
                *count += 1;
                sum.anon += p.anon;
                sum.shmem += p.shmem;
                sum.swap += p.swap;
            }
            None => by.push((p.name.clone(), 1, p.clone())),
        }
    }
    by.sort_by_key(|(_, _, s)| std::cmp::Reverse(s.anon + s.swap));
    let listed: Vec<String> = by
        .iter()
        .filter(|(_, _, s)| s.anon + s.swap >= MIB)
        .take(top)
        .map(|(name, count, s)| {
            let n = if *count > 1 { format!(" x{count}") } else { String::new() };
            let mut extra = Vec::new();
            if s.swap >= MIB {
                extra.push(format!("swap {}", s.swap / MIB));
            }
            if s.shmem >= MIB {
                extra.push(format!("shmem {}", s.shmem / MIB));
            }
            let extra = if extra.is_empty() { String::new() } else { format!(" ({})", extra.join(", ")) };
            format!("{name}{n} {}{extra}", s.anon / MIB)
        })
        .collect();
    let anon: u64 = procs.iter().map(|p| p.anon).sum();
    let swap: u64 = procs.iter().map(|p| p.swap).sum();
    format!(
        "{}; all {} processes: anonymous {} MiB, swap {} MiB",
        if listed.is_empty() { "none of 1 MiB".to_owned() } else { listed.join(", ") },
        procs.len(),
        anon / MIB,
        swap / MIB
    )
}

// ---- the reads (the phone's files; each one may be missing elsewhere) ----------------------------------------------

/// Every process's status (the walk): a missing or unreadable one is left out.
pub fn walk_procs() -> Vec<Proc> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.flatten()
        .filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|e| std::fs::read_to_string(e.path().join("status")).ok())
        .filter_map(|t| parse_status(&t))
        .collect()
}

/// The zram devices, each zram_text; "" without one.
fn zram_all(page: u64) -> String {
    let Ok(dir) = std::fs::read_dir("/sys/block") else { return String::new() };
    let mut names: Vec<String> =
        dir.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("zram")).collect();
    names.sort();
    names
        .iter()
        .filter_map(|n| {
            let stat = read_line(format!("/sys/block/{n}/mm_stat"))?;
            zram_text(n, &stat, read_line(format!("/sys/block/{n}/backing_dev")).as_deref(), page)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A file of one number (bytes, or `kb` KiB): None when it is missing or not a number.
fn number(path: &str, unit: u64) -> Option<u64> {
    read_line(path)?.split_whitespace().next()?.parse::<u64>().ok().map(|n| n * unit)
}

/// The pools and the vm settings beside meminfo: the dma-buf heaps' page pools (GKI's total_pools_kb), KGSL's global
/// counters, the watermarks (min_free_kbytes, watermark_scale_factor, the zones' high marks) and swappiness.
fn pools_text(page: u64) -> String {
    let mut parts = Vec::new();
    if let Some(b) = number("/sys/kernel/dma_heap/total_pools_kb", 1024) {
        parts.push(format!("dma-buf heap pools {} MiB", b / MIB));
    }
    let kgsl: Vec<String> = ["page_alloc", "vmalloc", "coherent", "secure", "mapped"]
        .iter()
        .filter_map(|n| number(&format!("/sys/class/kgsl/kgsl/{n}"), 1).map(|b| format!("{n} {}", b / MIB)))
        .collect();
    if !kgsl.is_empty() {
        parts.push(format!("KGSL (MiB): {}", kgsl.join(", ")));
    }
    let mut vm = Vec::new();
    if let Some(k) = number("/proc/sys/vm/min_free_kbytes", 1) {
        vm.push(format!("min_free_kbytes {k}"));
    }
    for n in ["watermark_scale_factor", "extra_free_kbytes", "swappiness"] {
        if let Some(v) = number(&format!("/proc/sys/vm/{n}"), 1) {
            vm.push(format!("{n} {v}"));
        }
    }
    if let Ok(z) = std::fs::read_to_string("/proc/zoneinfo") {
        let high = zoneinfo_high(&z) * page;
        if high != 0 {
            vm.push(format!("the zones' high watermarks {} MiB", high / MIB));
        }
    }
    if !vm.is_empty() {
        parts.push(vm.join(", "));
    }
    parts.join("; ")
}

/// The line's text: meminfo in full (`meminfo` the file xnuhost reads, ram-meminfo), zram, the pools and vm settings,
/// and with `procs` the walk's top 14 by name. "" when meminfo cannot be read.
pub fn breakdown(meminfo: &str, procs: bool) -> String {
    let Ok(text) = std::fs::read_to_string(meminfo) else { return String::new() };
    let m = parse_meminfo(&text);
    if get(&m, "MemTotal") == 0 {
        return String::new();
    }
    let page = xh_core::sys::page_size();
    let mut s = meminfo_text(&m);
    let z = zram_all(page);
    if !z.is_empty() {
        let _ = write!(s, "; {z}");
    }
    let p = pools_text(page);
    if !p.is_empty() {
        let _ = write!(s, "; {p}");
    }
    if procs {
        let _ = write!(s, "; processes by anonymous memory (MiB): {}", procs_text(&walk_procs(), 14));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str = "MemTotal:       11350000 kB\nMemFree:          900000 kB\nMemAvailable:    8661000 kB\n\
Buffers:            2048 kB\nCached:          6000000 kB\nShmem:            102400 kB\nAnonPages:        409600 kB\n\
SUnreclaim:       307200 kB\nSReclaimable:     204800 kB\nKernelStack:       40960 kB\nPageTables:        61440 kB\n\
VmallocUsed:      153600 kB\nPercpu:            12288 kB\nSwapTotal:      12581888 kB\nSwapFree:       12021760 kB\n\
CmaTotal:         409600 kB\nCmaFree:              0 kB\nRbinTotal:        716800 kB\nRbinAlloced:        512 kB\n\
HugePages_Total:       0\nHugepagesize:       2048 kB\n";

    #[test]
    fn the_meminfo_part() {
        let m = parse_meminfo(MEMINFO);
        assert_eq!(get(&m, "MemTotal"), 11350000 * 1024);
        assert_eq!(get(&m, "HugePages_Total"), 0);
        let t = meminfo_text(&m);
        assert!(t.starts_with("MemTotal 11083, MemFree 878, MemAvailable 8458; anonymous 400, page cache 5759"), "{t}");
        assert!(t.contains("the kernel's: unreclaimable slab 300, reclaimable slab 200, kernel stacks 40, page tables 60, vmalloc 150, per-CPU 12"), "{t}");
        assert!(t.contains("swap 547 of 12287 used"), "{t}");
        // the vendor's and CMA as the kernel names them; under 1 MiB (CmaFree, RbinAlloced) left out
        assert!(t.ends_with("; other keys: CmaTotal 400, RbinTotal 700"), "{t}");
    }

    #[test]
    fn zram_and_zones() {
        let page = 4096;
        let t =
            zram_text("zram0", "2317352960 760217600 789577728 0 812646400 12000 0 0 0", Some("none\n"), page).unwrap();
        assert_eq!(t, "zram0: 2210 MiB of pages in 753 MiB of RAM (compressed 725, most 775 MiB; same pages 46), backing_dev none");
        assert!(zram_text("zram0", "1 2 3", None, page).is_none());
        let z = "Node 0, zone   Normal\n  pages free     1000\n        min      5000\n        low      7000\n        high     9000\nNode 0, zone  Movable\n        high     1000\n";
        assert_eq!(zoneinfo_high(z), 10000);
    }

    #[test]
    fn processes_by_name() {
        let s = "Name:\tsurfaceflinger\nRssAnon:\t   94208 kB\nRssShmem:\t     512 kB\nVmSwap:\t   31744 kB\n";
        let p = parse_status(s).unwrap();
        assert_eq!(
            p,
            Proc { name: "surfaceflinger".into(), anon: 94208 * 1024, shmem: 512 * 1024, swap: 31744 * 1024 }
        );
        // a kernel thread: a name, nothing else
        assert_eq!(parse_status("Name:\tkswapd0\nState:\tS\n").unwrap().anon, 0);
        assert!(parse_status("State:\tS\n").is_none());
        let task = |a| Proc { name: "xnuhost-task".into(), anon: a, shmem: 2 << 20, swap: 0 };
        let list = vec![p, task(1 << 20), task(1 << 20), Proc { name: "kswapd0".into(), ..Proc::default() }];
        assert_eq!(
            procs_text(&list, 14),
            "surfaceflinger 92 (swap 31), xnuhost-task x2 2 (shmem 4); all 4 processes: anonymous 94 MiB, swap 31 MiB"
        );
        assert_eq!(procs_text(&list, 1), "surfaceflinger 92 (swap 31); all 4 processes: anonymous 94 MiB, swap 31 MiB");
    }
}
