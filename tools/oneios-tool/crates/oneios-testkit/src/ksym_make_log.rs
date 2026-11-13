//! `oneios-tool debug ksym-make-log` (was tests/ksym/make-log.py): write a stand-in xnuhost session log with the
//! kernel panic of 2026-11-12 (the phone, 23:15), for a kernel collection and its unprelinked kernel that are on
//! this machine.
//!
//!   debug ksym-make-log KC KERNEL [--expect FILE] > LOG
//!
//! The Mac's log and kernel of that panic are not on every machine, and another build has other addresses. So the
//! log is made from the files at hand: for each function of the panic's backtrace (the list below, in the panic's
//! order, with what `atos -o xnu.macho -arch arm64e` printed on the Mac), a return address inside that function of
//! KERNEL (the instruction after its call of the function above it in the backtrace, where it calls that one with a
//! `bl`; else after the middle one of its calls), moved to KC's address by the test's own reader (kcmap.rs). The
//! fault's pc and lr are addresses of the running kernel: the collection's address plus the slide of the log's
//! `memory:` line. The lines have the formats of runtime/xnuhost/src/log.c and of hosted_panic
//! (kernel/patches/oneios-hosted/0001-hosted-machine-layer.patch).
//!
//! --expect FILE: one line per address written, `KIND ADDRESS-IN-LOG KERNEL-ADDRESS FUNCTION`, for ksym-compare.
//! A function that is not a symbol of this kernel (inlined in this build) is left out and named in a `#` line.

use std::collections::HashMap;
use std::fmt::Write as _;

use anyhow::{Context, Result};

use crate::kcmap::{self, Seg};

/// The backtrace of the panic, outermost last: (function, what atos printed for the Mac's kernel).
const FRAMES: [(&str, &str); 17] = [
    ("__ubsan_handle_mul_overflow_minimal", "ubsan_minimal.c:165"),
    ("Assert", "debug.c:768"),
    ("sleh_synchronous_sp1", "sleh.c:671"),
    ("handle_kernel_abort", "sleh.c:2570"),
    ("sleh_synchronous", "sleh.c:1000"),
    ("hosted_exception_upcall", "hosted_machine.c:477"),
    ("hosted_exception_upcall_entry", "+ 24"),
    ("tcp_input", "tcp_input.c:3723"),
    ("ip_proto_dispatch_in", "ip_input.c:0"),
    ("ip_input_second_pass", "ip_input.c:1663"),
    ("ip_proto_input", "ip_input.c:1735"),
    ("proto_input", "kpi_protocol.c:282"),
    ("ether_inet_input", "ether_inet_pr_module.c:230"),
    ("dlil_ifproto_input", "dlil_input.c:349"),
    ("dlil_input_packet_list_common", "dlil_input.c:1111"),
    ("dlil_input_thread_cont", "dlil_input.c:1259"),
    ("Call_continuation", "+ 112"),
];
const CALLER: &str = "sleh_synchronous_sp1";
const PC: &str = "__ubsan_handle_mul_overflow_minimal";
/// The Mac's lr was 0x7138 bytes after tcp_input's frame: in compute_iaj when it is a function of its own.
const LR: [&str; 2] = ["compute_iaj", "tcp_input"];
const RAM: u64 = 0x20_0000_4000;

/// bl; blr; blraa/blrab/blraaz/blrabz
fn is_call(word: u32) -> bool {
    (word & 0xFC00_0000) == 0x9400_0000 || (word & 0xFFFF_FC1F) == 0xD63F_0000 || (word & 0xFEFF_F800) == 0xD63F_0800
}

/// The kernel as this helper reads it.
struct Kernel<'a> {
    data: &'a [u8],
    segs: Vec<Seg>,
    table: HashMap<String, u64>,
    starts: Vec<u64>,
    in_kc: HashMap<String, Seg>,
}

impl Kernel<'_> {
    /// (start, end, segment) of a function of the kernel: up to the next symbol, inside its segment.
    fn extent(&self, name: &str) -> Option<(u64, u64, &Seg)> {
        let addr = *self.table.get(&format!("_{name}"))?;
        let seg = self.segs.iter().find(|s| s.holds(addr)).expect("a symbol outside every segment");
        let next = self.starts.partition_point(|&s| s <= addr);
        let top = seg.vmaddr + seg.vmsize;
        Some((addr, self.starts.get(next).map_or(top, |&s| s.min(top)), seg))
    }

    fn words(&self, start: u64, end: u64, seg: &Seg) -> Vec<u32> {
        let off = (seg.fileoff + (start - seg.vmaddr)) as usize;
        (0..((end - start) / 4) as usize).map(|i| u32::from_le_bytes(self.data[off + 4 * i..off + 4 * i + 4].try_into().unwrap())).collect()
    }

    fn to_kc(&self, addr: u64, seg: &Seg) -> u64 {
        addr.wrapping_add(self.in_kc[&seg.name].vmaddr).wrapping_sub(seg.vmaddr)
    }

    /// The address after the function's first `bl` to `callee` (the frame the real backtrace has above it), else
    /// after the middle one of its calls (after its second instruction when it calls nothing): (kernel, collection).
    fn return_address(&self, name: &str, callee: Option<&str>) -> Option<(u64, u64)> {
        let (start, end, seg) = self.extent(name)?;
        let code = self.words(start, end, seg);
        let after = |i: usize| start + 4 * i as u64 + 4;
        let calls: Vec<u64> = code.iter().enumerate().filter(|&(i, &w)| is_call(w) && after(i) < end).map(|(i, _)| after(i)).collect();
        let mut addr = if calls.is_empty() { start + 8 } else { calls[calls.len() / 2] };
        if let Some(target) = callee.and_then(|c| self.table.get(&format!("_{c}"))) {
            for (i, &w) in code.iter().enumerate() {
                if (w & 0xFC00_0000) == 0x9400_0000 && after(i) < end {
                    let offset = (w & 0x3FF_FFFF) as i64 - (((w & 0x200_0000) as i64) << 1);
                    if (start + 4 * i as u64).wrapping_add((4 * offset) as u64) == *target {
                        addr = after(i);
                        break;
                    }
                }
            }
        }
        Some((addr, self.to_kc(addr, seg)))
    }
}

pub fn run(kc_path: &str, kernel_path: &str, expect_path: Option<&str>) -> Result<i32> {
    let (kc, kernel_map) = (kcmap::open_map(kc_path)?, kcmap::open_map(kernel_path)?);
    let kc_top = kcmap::segments(&kc, &kcmap::commands(&kc, 0)?.1);
    let link_base = kc_top.iter().map(|s| s.vmaddr).min().context("the collection has no segment")?;
    let slide = RAM.wrapping_sub(link_base);
    let (_, cmds) = kcmap::commands(&kernel_map, 0)?;
    let table = kcmap::symbols(&kernel_map, &cmds);
    let mut starts: Vec<u64> = table.values().copied().collect();
    starts.sort_unstable();
    starts.dedup();
    let kernel = Kernel {
        data: &kernel_map,
        segs: kcmap::segments(&kernel_map, &cmds),
        table,
        starts,
        in_kc: kcmap::by_name(kcmap::segments(&kc, &kcmap::kernel_member(&kc)?)),
    };

    // The pc: the first brk of the handler (the trap it ends in), else its first instruction.
    let (start, end, seg) = kernel.extent(PC).with_context(|| format!("{PC} is not a symbol of this kernel"))?;
    let brk = kernel.words(start, end, seg).iter().position(|&w| (w & 0xFFE0_001F) == 0xD420_0000);
    let pc = brk.map_or(start, |i| start + 4 * i as u64);
    let pc_kc = kernel.to_kc(pc, seg);
    let mut expect: Vec<(&str, u64, u64, &str)> = Vec::new();
    let mut notes = Vec::new();
    let mut frames = Vec::new();
    let lr_name = *LR.iter().find(|n| kernel.extent(n).is_some()).context("no function for the lr in this kernel")?;
    let mut above: Option<&str> = None;
    for (name, _atos) in FRAMES {
        // The panic's first frame line is the fault's pc itself (0xfffffff00b54c094 on the Mac, running 0x200454c094).
        // tcp_input's frame is the fault's own: the function above it is the one the lr is in.
        let got = if name == PC { Some((pc, pc_kc)) } else { kernel.return_address(name, if name == "tcp_input" { Some(lr_name) } else { above }) };
        let Some((addr, addr_kc)) = got else {
            notes.push(format!("# make-log: {name} is not a symbol of this kernel (inlined in this build): no frame for it"));
            continue;
        };
        above = Some(name);
        frames.push(addr_kc);
        expect.push(("frame", addr_kc, addr, name));
    }
    let caller = kernel.return_address(CALLER, None).with_context(|| format!("{CALLER} is not a symbol of this kernel"))?;
    let lr = kernel.return_address(lr_name, Some(PC)).context("the lr's function")?;
    let (run_pc, run_lr) = (pc_kc.wrapping_add(slide), lr.1.wrapping_add(slide));
    expect.insert(0, ("lr", run_lr, lr.0, lr_name));
    expect.insert(0, ("pc", run_pc, pc, PC));
    expect.insert(0, ("caller", caller.1, caller.0, CALLER));

    let mut out = String::new();
    writeln!(out, "[    0.011] xnuhost I kernel: /data/local/tmp/oneios/stage/kernel.kc, collection arm64e, link base {link_base:#x}, {} segments, {} members", kc_top.len(), kcmap::members(&kc)?.len())?;
    writeln!(out, "[    0.019] xnuhost I memory: 9728 MiB at {RAM:#x}, physical = virtual (slide {slide:#x}); I/O aperture 0x8000000")?;
    writeln!(out, "[    0.250] guest     Darwin Kernel Version 27.0.0: stand-in line of oneios-tool debug ksym-make-log")?;
    for note in &notes {
        writeln!(out, "{note}")?;
    }
    writeln!(out, "[ 4120.730] guest     oneios-net: stand-in line before the panic")?;
    writeln!(out, "[ 4120.731] guest     panic(cpu 4 caller {:#x}): Unexpected fault in kernel physical aperture at pc 0x{run_pc:016x}, lr 0x{run_lr:016x} (saved state: 0x00000020178a3690)", caller.1)?;
    for row in (0..28).step_by(4) {
        let regs: Vec<String> = (row..row + 4).map(|r| format!("x{r}:{} 0x{:016x}", if r < 10 { " " } else { "" }, 0x1000 + r)).collect();
        writeln!(out, "[ 4120.731] guest     \t  {}", regs.join("  "))?;
    }
    writeln!(out, "[ 4120.731] guest     \t  x28: 0x0000000000000000 fp:  0x00000020178a3a10  lr:  0x{run_lr:016x}  sp:  0x00000020178a39f0")?;
    writeln!(out, "[ 4120.731] guest     \t  pc:  0x{run_pc:016x} cpsr: 0x60400204         esr: 0x00000000f2005519  far: 0x0000000000000000")?;
    for addr in &frames {
        writeln!(out, "[ 4120.731] guest       frame 0x{addr:016x}")?;
    }
    writeln!(out, "[ 4120.740] xnuhost I stopping: the guest panicked")?;
    writeln!(out, "[ 4120.912] xnuhost I exit 3")?;
    print!("{out}");
    if let Some(path) = expect_path {
        let lines: String = expect.iter().map(|(kind, shown, addr, name)| format!("{kind} {shown:#x} {addr:#x} {name}\n")).collect();
        std::fs::write(path, lines).with_context(|| path.to_string())?;
    }
    Ok(0)
}
