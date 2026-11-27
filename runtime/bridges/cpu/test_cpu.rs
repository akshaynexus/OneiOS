// SPDX-License-Identifier: MIT
//! The CPU bridge's host tests (C: tests/test_bridges.c, test_cpu): stand-in /sys/devices/system/cpu folders of an
//! SM8650, an SM8750, an SM8850 and a host with one kind of core.

use super::*;
use crate::hw::testdir::TempDir;

/// A /sys/devices/system/cpu shaped like an SM8650's (made-up frequencies and capacities, the real core kinds):
/// cpu0-1 A520, cpu2-6 A720 in two frequency domains, cpu7 X4.
pub(crate) fn make_sm8650(dir: &TempDir, root: &str) {
    const A520: &str = "364800 460800 556800 672000 787200 902400 1017600 1132800 1248000 1344000 1459200 1574400 \
                        1689600 1804800 1920000 2035200 2150400 2265600";
    const A720: &str = "499200 614400 729600 844800 960000 1075200 1190400 1286400 1401600 1497600 1612800 1708800 \
                        1824000 1920000 2035200 2131200 2188800 2246400 2323200 2380800 2438400 2515200 2572800 2630400 \
                        2707200 2764800 2841600 2899200 2956800 3014400 3072000 3148800";
    type Cpu<'a> = (u32, u32, u32, u32, u32, Option<&'a str>); // capacity, max, min, policy, MIDR, table
    #[rustfmt::skip]
    let cpus: [Cpu; 8] = [
        (325, 2265600, 364800, 0, 0x410fd801, Some(A520)),
        (325, 2265600, 364800, 0, 0x410fd801, None),
        (792, 3148800, 499200, 2, 0x411fd811, Some(A720)),
        (792, 3148800, 499200, 2, 0x411fd811, None), (792, 3148800, 499200, 2, 0x411fd811, None),
        (790, 2956800, 499200, 5, 0x411fd811, Some("499200 2956800")), (790, 2956800, 499200, 5, 0x411fd811, None),
        (1024, 3052800, 480000, 7, 0x411fd821, Some("480000 3052800")), // the top (3398400) only as a boost frequency
    ];
    for (i, (capacity, max, min, policy, midr, table)) in cpus.into_iter().enumerate() {
        let put = |rel: &str, text: String| dir.put(&format!("{root}/devices/system/cpu/cpu{i}/{rel}"), text);
        put("cpu_capacity", format!("{capacity}\n"));
        put("cpufreq/cpuinfo_max_freq", format!("{max}\n"));
        put("cpufreq/cpuinfo_min_freq", format!("{min}\n"));
        put("cpufreq/related_cpus", format!("{policy} {i}\n"));
        put("regs/identification/midr_el1", format!("0x{midr:016x}\n"));
        if let Some(table) = table {
            put("cpufreq/scaling_available_frequencies", format!("{table} \n"));
        }
        if i == 7 {
            put("cpufreq/scaling_boost_frequencies", "3398400 \n".into());
        }
        put("cache/index0/level", "1\n".into());
        put("cache/index0/type", "Data\n".into());
        put("cache/index0/size", if i < 2 { "32K\n" } else { "64K\n" }.into());
        put("cache/index2/level", "2\n".into());
        put("cache/index2/type", "Unified\n".into());
        put(
            "cache/index2/size",
            if i == 7 {
                "2048K\n"
            } else if i < 2 {
                "256K\n"
            } else {
                "512K\n"
            }
            .into(),
        );
    }
}

/// A /sys/devices/system/cpu shaped like the Galaxy S25's SM8750 (Snapdragon 8 Elite for Galaxy): cpu0-5 six
/// performance Oryon cores, cpu6-7 two prime ones. Capacities, frequencies and the MIDR part are made up; what is real
/// is the shape: implementer 0x51 (Qualcomm), two kinds of core, none of them little.
fn make_sm8750(dir: &TempDir, root: &str) {
    for i in 0..8 {
        let prime = i >= 6;
        let put = |rel: &str, text: String| dir.put(&format!("{root}/devices/system/cpu/cpu{i}/{rel}"), text);
        put("cpu_capacity", if prime { "1024\n" } else { "790\n" }.into());
        put("cpufreq/cpuinfo_max_freq", if prime { "4473600\n" } else { "3532800\n" }.into());
        put("cpufreq/cpuinfo_min_freq", "384000\n".into());
        put("cpufreq/related_cpus", format!("{} {i}\n", if prime { 6 } else { 0 }));
        put("regs/identification/midr_el1", "0x00000000510f0010\n".into());
    }
}

use CoreKind::{E, P};

#[test]
fn cpu() {
    let tmp = TempDir::new("cpu");
    make_sm8650(&tmp, "sys");
    let t = Topology::read(&tmp.at("sys")).unwrap();
    assert!(t.ncpu() == 8 && t.n_e == 2 && t.n_p == 6);
    assert!(t.cpu[0].kind == E && t.cpu[1].kind == E && t.cpu[2].kind == P && t.cpu[7].kind == P);
    assert!(t.cpu[0].part == 0xd80 && t.cpu[2].part == 0xd81 && t.cpu[7].part == 0xd82);
    assert!(t.cpu[0].freq_khz.len() == 18 && t.cpu[0].freq_khz[0] == 364800 && t.cpu[0].freq_khz[17] == 2265600);
    assert!(t.cpu[1].freq_khz == [364800, 2265600]); // no table: the two ends
    assert!(t.cpu[7].l2_bytes == 2048 * 1024 && t.cpu[0].l1d_bytes == 32 * 1024 && t.cpu[5].policy == 5);
    // the X4's boost frequency joins its table and its top (scaling_boost_frequencies, host-overhead 2026-11-15)
    let x4 = &t.cpu[7];
    assert!(x4.freq_khz.len() == 3 && x4.freq_khz[2] == 3398400 && x4.max_khz == 3398400 && x4.boost_khz == 3398400);
    assert!(t.cpu[2].boost_khz == 0 && t.cpu[2].max_khz == 3148800);

    let plan = CpuPlan::make(&t, 8, false).unwrap();
    assert!(plan.n_e == 2 && plan.n_p == 6);
    assert!(plan.v[0].kind == E && plan.v[1].kind == E && plan.v[2].kind == P && plan.v[7].kind == P);
    assert!(plan.affinity(0) == 0x03 && plan.affinity(2) == 0xfc);
    assert!(plan.v[0].cluster_id == 0 && plan.v[7].cluster_id == 1);
    // each vCPU described as the core cpu-pin puts it on (vCPU i on CPU i), not as the fastest of its kind
    assert!(plan.v[2].max_khz == 3148800 && plan.v[2].capacity == 792 && plan.v[2].host_cpu == 2);
    let v7 = &plan.v[7];
    assert!(
        v7.max_khz == 3398400 && v7.capacity == 1024 && v7.host_cpu == 7 && *v7.freq_khz.last().unwrap() == 3398400
    );
    assert!(plan.v[5].max_khz == 2956800 && plan.v[0].max_khz == 2265600 && plan.v[0].host_cpu == 0);
    let plan = CpuPlan::make(&t, 8, true).unwrap();
    assert!(
        plan.affinity(0) == 0x01 && plan.affinity(1) == 0x02 && plan.affinity(2) == 0x80 && plan.affinity(3) == 0x04
    );
    assert!(plan.v[2].max_khz == 3398400 && plan.v[3].max_khz == 3148800);
    let plan = CpuPlan::make(&t, 4, false).unwrap();
    assert!(plan.n_e == 1 && plan.n_p == 3 && plan.v[0].kind == E && plan.v[1].kind == P);
    let plan = CpuPlan::make(&t, 1, false).unwrap();
    assert!(plan.n_e == 0 && plan.v[0].kind == P);
    assert!(CpuPlan::make(&t, 9, false).is_none() && CpuPlan::make(&t, 0, false).is_none());

    let plan = CpuPlan::make(&t, 8, false).unwrap();
    assert!(plan.util_for(7, 3398400) == 1024 && plan.util_for(7, 1699200) == 512 && plan.util_for(7, 0) == 0);
    assert_eq!(plan.util_for(0, 2265600), 325);
    let d = Cpufreq::new(&plan, None);
    assert_eq!(d.read(0x08, 4), 18); // vCPU 0: the E table
    d.write(0x0c, 4, 17);
    assert_eq!(d.read(0x10, 4), 2265600);
    assert!(d.read(7 * CPUFREQ_WINDOW + 0x08, 4) == 3 && d.read(7 * CPUFREQ_WINDOW + 0x14, 4) == 1);
    d.write(7 * CPUFREQ_WINDOW + 0x04, 4, 3398400);
    assert!(d.requested(7) == 3398400 && d.read(7 * CPUFREQ_WINDOW, 4) == 3398400);
    d.write(7 * CPUFREQ_WINDOW + 0x04, 4, 480000); // the lowest entry drops the request
    assert_eq!(d.requested(7), 0);
    assert_eq!(d.read(9 * CPUFREQ_WINDOW + 0x08, 4), 0);
    // A VMM that keeps the clamp itself (xnuhost): each change of a request reaches it once, as a uclamp.min.
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    let d = Cpufreq::with_apply(&plan, Box::new(move |cpu, util| log.lock().unwrap().push((cpu, util))));
    d.write(7 * CPUFREQ_WINDOW + 0x04, 4, 1699200);
    d.write(7 * CPUFREQ_WINDOW + 0x04, 4, 1699200); // the same request: nothing
    d.write(7 * CPUFREQ_WINDOW + 0x04, 4, 480000);
    assert_eq!(*seen.lock().unwrap(), [(7, 512), (7, 0)]);
    assert_eq!(d.requested(7), 0);

    // The S25's shape: Oryon cores, two speeds, no little ones. All P on the host; the plan lends the slowest as E.
    // Each vCPU is described as its own core (cpu-pin): the E vCPUs are cpu4-5, the P ones cpu0-3 and cpu6-7.
    make_sm8750(&tmp, "sm8750");
    let t = Topology::read(&tmp.at("sm8750")).unwrap();
    assert!(t.ncpu() == 8 && t.n_e == 0 && t.n_p == 8);
    assert!(t.cpu[0].implementer == 0x51 && t.cpu[0].part == 0x001 && t.cpu[0].kind == P && t.cpu[7].kind == P);
    let plan = CpuPlan::make(&t, 8, false).unwrap();
    assert!(plan.n_e == 2 && plan.n_p == 6);
    assert!(plan.v[0].kind == E && plan.v[1].kind == E && plan.v[2].kind == P && plan.v[7].kind == P);
    assert!(plan.affinity(0) == 0x30 && plan.affinity(2) == 0xcf); // E: cpu4-5 (the slowest), P: the rest
    let hosts: Vec<u32> = plan.v.iter().map(|v| v.host_cpu).collect();
    assert!(hosts[0] == 4 && hosts[1] == 5 && hosts[2] == 0 && hosts[6] == 6 && hosts[7] == 7);
    assert!(plan.v[0].max_khz == 3532800 && plan.v[2].max_khz == 3532800 && plan.v[6].max_khz == 4473600);
    let plan = CpuPlan::make(&t, 8, true).unwrap();
    let masks: Vec<u64> = (0..5).map(|i| plan.affinity(i)).collect();
    assert_eq!(masks, [0x10, 0x20, 0x40, 0x80, 0x01]);
    let plan = CpuPlan::make(&t, 4, false).unwrap();
    assert!(plan.n_e == 1 && plan.n_p == 3 && plan.v[0].kind == E && plan.v[1].kind == P);
    assert!(plan.v[0].host_cpu == 5 && plan.v[0].max_khz == 3532800 && plan.v[1].host_cpu == 0);
    let plan = CpuPlan::make(&t, 2, false).unwrap();
    assert!(plan.n_e == 1 && plan.n_p == 1);
    let plan = CpuPlan::make(&t, 1, false).unwrap();
    assert!(plan.n_e == 0 && plan.v[0].kind == P);

    // The Galaxy S26 Ultra's SM8850 (docs/s26.md), as YMGPwcca/OneiOS read it on its SM-S948B (port-sm8850-s26): cpu0-5
    // capacity 741, 384000..3628800 kHz; cpu6-7 capacity 1024, 768000..4742400 kHz; every MIDR 0x510f0021 (Qualcomm
    // Oryon). [F] the values; the related_cpus lists [I] (the fork wrote the policy's first CPU only). The same Oryon
    // rule as the S25: all P on the host, the two slowest lent as E vCPUs; nothing of the host's cpufreq is written.
    for i in 0..8 {
        let prime = i >= 6;
        let put = |rel: &str, text: &str| tmp.put(&format!("sm8850/devices/system/cpu/cpu{i}/{rel}"), text);
        put("cpu_capacity", if prime { "1024\n" } else { "741\n" });
        put("cpufreq/cpuinfo_max_freq", if prime { "4742400\n" } else { "3628800\n" });
        put("cpufreq/cpuinfo_min_freq", if prime { "768000\n" } else { "384000\n" });
        put("cpufreq/related_cpus", if prime { "6 7\n" } else { "0 1 2 3 4 5\n" });
        put("regs/identification/midr_el1", "0x510f0021\n");
    }
    let t = Topology::read(&tmp.at("sm8850")).unwrap();
    assert!(t.ncpu() == 8 && t.n_e == 0 && t.n_p == 8);
    assert!(
        t.cpu[0].implementer == 0x51 && t.cpu[0].capacity == 741 && t.cpu[0].kind == P && t.cpu[6].capacity == 1024
    );
    let plan = CpuPlan::make(&t, 8, false).unwrap();
    assert!(plan.n_e == 2 && plan.n_p == 6 && plan.v.len() == 8);
    assert!(plan.v[0].kind == E && plan.v[1].kind == E && plan.v[2].kind == P && plan.v[7].kind == P);
    assert!(
        plan.affinity(0) == 0x30 && plan.affinity(1) == 0x30 && plan.affinity(2) == 0xcf && plan.affinity(7) == 0xcf
    );
    let hosts: Vec<u32> = plan.v.iter().map(|v| v.host_cpu).collect();
    assert!(hosts[0] == 4 && hosts[1] == 5 && hosts[2] == 0 && hosts[7] == 7);
    assert!(plan.v[0].max_khz == 3628800 && plan.v[7].max_khz == 4742400);
    let plan = CpuPlan::make(&t, 8, true).unwrap();
    let masks: Vec<u64> = (0..8).map(|i| plan.affinity(i)).collect();
    assert!(masks[..5] == [0x10, 0x20, 0x40, 0x80, 0x01] && masks[7] == 0x08, "{masks:x?}");
    let plan = CpuPlan::make(&t, 4, false).unwrap();
    assert!(plan.n_e == 1 && plan.n_p == 3 && plan.v[0].kind == E);
    assert!(plan.v[0].host_cpu == 5 && plan.affinity(0) == 0x20); // one lent core; the prime ones stay P

    // Base Galaxy S26 (SC-51G m1q SM8850): cpu0-5 MIDR 0x512f0021, cpu6-7 MIDR 0x513f0021.
    for i in 0..8 {
        let prime = i >= 6;
        let put = |rel: &str, text: &str| tmp.put(&format!("s26m1q/devices/system/cpu/cpu{i}/{rel}"), text);
        put("cpu_capacity", if prime { "1024\n" } else { "741\n" });
        put("cpufreq/cpuinfo_max_freq", if prime { "4742400\n" } else { "3628800\n" });
        put("cpufreq/cpuinfo_min_freq", if prime { "768000\n" } else { "384000\n" });
        put("cpufreq/related_cpus", if prime { "6 7\n" } else { "0 1 2 3 4 5\n" });
        put("regs/identification/midr_el1", if prime { "0x00000000513f0021\n" } else { "0x00000000512f0021\n" });
    }
    let t = Topology::read(&tmp.at("s26m1q")).unwrap();
    assert!(t.ncpu() == 8 && t.n_e == 0 && t.n_p == 8 && t.cpu[0].implementer == 0x51 && t.cpu[6].implementer == 0x51);
    let plan = CpuPlan::make(&t, 8, false).unwrap();
    assert!(plan.n_e == 2 && plan.n_p == 6 && plan.v[0].host_cpu == 4 && plan.v[1].host_cpu == 5);

    // A host with one kind of core: all P.
    for i in 0..4 {
        tmp.put(&format!("flat/devices/system/cpu/cpu{i}/cpu_capacity"), "1024\n");
    }
    let t = Topology::read(&tmp.at("flat")).unwrap();
    assert!(t.n_p == 4 && t.n_e == 0);
    let plan = CpuPlan::make(&t, 4, false).unwrap();
    assert!(plan.n_e == 0 && plan.affinity(0) == 0xf);
    assert!(Topology::read("/nonexistent").is_none());
}
