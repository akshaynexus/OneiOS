#!/usr/bin/env python3
"""Write a stand-in xnuhost session log with the kernel panic of 2026-11-12 (the phone, 23:15), for a kernel
collection and its unprelinked kernel that are on this machine.

  make-log.py KC KERNEL [--expect FILE] > LOG

The Mac's log and kernel of that panic are not on every machine, and another build has other addresses. So the
log is made from the files at hand: for each function of the panic's backtrace (the list below, in the panic's
order, with what `atos -o xnu.macho -arch arm64e` printed on the Mac), a return address inside that function of
KERNEL (the instruction after its call of the function above it in the backtrace, where it calls that one with a
`bl`; else after the middle one of its calls), moved to KC's address by the test's own reader (kcmap.py). The fault's pc and lr are addresses of the running kernel: the collection's address plus the slide of
the log's `memory:` line. The lines have the formats of runtime/xnuhost/src/log.c and of hosted_panic
(kernel/patches/oneios-hosted/0001-hosted-machine-layer.patch).

--expect FILE: one line per address written, `KIND ADDRESS-IN-LOG KERNEL-ADDRESS FUNCTION`, for compare.py.
A function that is not a symbol of this kernel (inlined in this build) is left out and named in a `#` line.
"""
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import kcmap  # noqa: E402

# The backtrace of the panic, outermost last: (function, what atos printed for the Mac's kernel).
FRAMES = [
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
]
CALLER = "sleh_synchronous_sp1"
PC = "__ubsan_handle_mul_overflow_minimal"
# The Mac's lr was 0x7138 bytes after tcp_input's frame: in compute_iaj when it is a function of its own.
LR = ["compute_iaj", "tcp_input"]
RAM = 0x2000004000


def is_call(word):
    # bl; blr; blraa/blrab/blraaz/blrabz
    return (word & 0xFC000000) == 0x94000000 or (word & 0xFFFFFC1F) == 0xD63F0000 or (word & 0xFEFFF800) == 0xD63F0800


def main(argv):
    args = [a for a in argv[1:] if not a.startswith("--")]
    expect_path = argv[argv.index("--expect") + 1] if "--expect" in argv else None
    if expect_path in args:
        args.remove(expect_path)
    if len(args) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    kc, kernel = kcmap.open_map(args[0]), kcmap.open_map(args[1])
    kc_top = kcmap.segments(kc, kcmap.commands(kc, 0)[1])
    link_base = min(s[1] for s in kc_top)
    slide = (RAM - link_base) & 0xFFFFFFFFFFFFFFFF
    in_kc = {s[0]: s for s in kcmap.segments(kc, kcmap.kernel_member(kc))}
    _, cmds = kcmap.commands(kernel, 0)
    segs = kcmap.segments(kernel, cmds)
    table = kcmap.symbols(kernel, cmds)
    starts = sorted(set(table.values()))

    def extent(name):
        """(start, end, segment) of a function of the kernel: up to the next symbol, inside its segment."""
        addr = table.get("_" + name)
        if addr is None:
            return None
        seg = next(s for s in segs if s[1] <= addr < s[1] + s[2])
        import bisect
        nxt = bisect.bisect_right(starts, addr)
        end = min(starts[nxt] if nxt < len(starts) else seg[1] + seg[2], seg[1] + seg[2])
        return addr, end, seg

    def words(start, end, seg):
        off = seg[3] + (start - seg[1])
        return struct.unpack_from(f"<{(end - start) // 4}I", kernel, off)

    def to_kc(addr, seg):
        return addr + in_kc[seg[0]][1] - seg[1]

    def return_address(name, callee=None):
        """The address after the function's first `bl` to `callee` (the frame the real backtrace has above it),
        else after the middle one of its calls (after its second instruction when it calls nothing)."""
        found = extent(name)
        if found is None:
            return None
        start, end, seg = found
        code = words(start, end, seg)
        calls = [start + 4 * i + 4 for i, w in enumerate(code) if is_call(w) and start + 4 * i + 4 < end]
        addr = calls[len(calls) // 2] if calls else start + 8
        target = table.get("_" + callee) if callee else None
        for i, w in enumerate(code):
            if target is not None and (w & 0xFC000000) == 0x94000000 and start + 4 * i + 4 < end:
                offset = (w & 0x3FFFFFF) - ((w & 0x2000000) << 1)
                if start + 4 * i + 4 * offset == target:
                    addr = start + 4 * i + 4
                    break
        return addr, to_kc(addr, seg)

    # The pc: the first brk of the handler (the trap it ends in), else its first instruction.
    start, end, seg = extent(PC)
    brk = [start + 4 * i for i, w in enumerate(words(start, end, seg)) if (w & 0xFFE0001F) == 0xD4200000]
    pc = brk[0] if brk else start
    pc_kc = to_kc(pc, seg)
    expect = []
    notes = []
    frames = []
    lr_name = next(n for n in LR if extent(n))
    above = None
    for name, _atos in FRAMES:
        # The panic's first frame line is the fault's pc itself (0xfffffff00b54c094 on the Mac, running 0x200454c094).
        # tcp_input's frame is the fault's own: the function above it is the one the lr is in.
        got = (pc, pc_kc) if name == PC else return_address(name, lr_name if name == "tcp_input" else above)
        if got is None:
            notes.append(f"# make-log: {name} is not a symbol of this kernel (inlined in this build): no frame for it")
            continue
        above = name
        frames.append(got[1])
        expect.append(("frame", got[1], got[0], name))
    caller = return_address(CALLER)
    lr = return_address(lr_name, PC)
    expect.insert(0, ("lr", (lr[1] + slide) & 0xFFFFFFFFFFFFFFFF, lr[0], lr_name))
    expect.insert(0, ("pc", (pc_kc + slide) & 0xFFFFFFFFFFFFFFFF, pc, PC))
    expect.insert(0, ("caller", caller[1], caller[0], CALLER))

    run_pc, run_lr = (pc_kc + slide) & 0xFFFFFFFFFFFFFFFF, (lr[1] + slide) & 0xFFFFFFFFFFFFFFFF
    out = []
    out.append(f"[    0.011] xnuhost I kernel: /data/local/tmp/oneios/stage/kernel.kc, collection arm64e, link base {link_base:#x}, {len(kc_top)} segments, {len(kcmap.members(kc))} members")
    out.append(f"[    0.019] xnuhost I memory: 9728 MiB at {RAM:#x}, physical = virtual (slide {slide:#x}); I/O aperture 0x8000000")
    out.append("[    0.250] guest     Darwin Kernel Version 27.0.0: stand-in line of tests/ksym/make-log.py")
    out.extend(notes)
    out.append("[ 4120.730] guest     oneios-net: stand-in line before the panic")
    out.append(
        f"[ 4120.731] guest     panic(cpu 4 caller {caller[1]:#x}): Unexpected fault in kernel physical aperture at pc 0x{run_pc:016x}, lr 0x{run_lr:016x} (saved state: 0x00000020178a3690)"
    )
    for row in range(0, 28, 4):
        regs = "  ".join(f"x{r}:{' ' if r < 10 else ''} 0x{(0x1000 + r):016x}" for r in range(row, row + 4))
        out.append(f"[ 4120.731] guest     \t  {regs}")
    out.append(f"[ 4120.731] guest     \t  x28: 0x0000000000000000 fp:  0x00000020178a3a10  lr:  0x{run_lr:016x}  sp:  0x00000020178a39f0")
    out.append(f"[ 4120.731] guest     \t  pc:  0x{run_pc:016x} cpsr: 0x60400204         esr: 0x00000000f2005519  far: 0x0000000000000000")
    for addr in frames:
        out.append(f"[ 4120.731] guest       frame 0x{addr:016x}")
    out.append("[ 4120.740] xnuhost I stopping: the guest panicked")
    out.append("[ 4120.912] xnuhost I exit 3")
    sys.stdout.write("\n".join(out) + "\n")
    if expect_path:
        with open(expect_path, "w") as f:
            for kind, shown, kernel_addr, name in expect:
                f.write(f"{kind} {shown:#x} {kernel_addr:#x} {name}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
