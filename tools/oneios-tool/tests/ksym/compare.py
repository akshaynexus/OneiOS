#!/usr/bin/env python3
"""Compare `oneios-tool sym kernel` with llvm-symbolizer, address by address. Run by compare.sh.

  compare.py --tool BIN --kc KC --kernel KERNEL --log LOG --ref FILE --index DIR
             [--expect FILE] [--dsym PATH] [--sample N] [--symbolizer llvm-symbolizer]

  --ref FILE   what llvm-symbolizer reads: the DWARF file of a dSYM of KERNEL (file:line and inlined frames are
               compared), or KERNEL itself (no DWARF at hand: function names only)
  --dsym PATH  give the tool this dSYM (else it finds the DWARF itself: in KERNEL, beside it, or through KERNEL's
               debug map)
  --expect     make-log.py's list: which kernel address and function each address of the log must get
  --sample N   how many more addresses of the kernel's code to compare (default 20000; 0: none)

Checks, in order:
  1. every address of the log's panic blocks: the tool's kernel address is the reference's question, and the
     tool's function, file, line and inlined frames are the reference's answer;
  2. the running addresses (pc, lr): collection address + slide = the address in the log;
  3. --expect: the kernel address and the function make-log.py wrote each address for;
  4. the collection address of each function, three ways: the tool's (--symbol), this test's own reader with the
     segment's move (kcmap.py forward), and the collection's own symbol table (kcmap.py own);
  5. every instruction of every function the panic names, and N more addresses spread over the kernel's code.

Names are compared as the files have them (mangled: `llvm-symbolizer --no-demangle`, the tool's `raw_function`
and `raw_symbol`), so two demanglers' spellings do not count. llvm-symbolizer names the outermost function from
the symbol table; where an address has two names (`_arp_ip_handle_input` and `_inet_arp_handle_input`) the two
tools may print different ones: that counts as the same when both are symbols of one address, and is counted.
Exit status 1 when anything differs.
"""
import bisect
import json
import os
import random
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import kcmap  # noqa: E402


def option(argv, name, default=None):
    return argv[argv.index(name) + 1] if name in argv else default


def base(path):
    return path.rsplit("/", 1)[-1]


def reference(symbolizer, ref, addrs):
    """Per address: the frames, the outermost function first, as (function, file, line)."""
    if not addrs:
        return []
    text = "".join(f"{a:#x}\n" for a in addrs)
    command = [symbolizer, f"--obj={ref}", "--inlining", "--no-demangle", "--output-style=JSON"]
    out = subprocess.run(command, input=text, capture_output=True, text=True, check=True).stdout
    rows = []
    for line in out.splitlines():
        if not line.strip():
            continue
        entry = json.loads(line)
        frames = []
        for symbol in reversed(entry.get("Symbol") or []):
            name = symbol.get("FunctionName") or ""
            frames.append(("" if name == "??" else name, symbol.get("FileName") or "", int(symbol.get("Line") or 0)))
        rows.append(frames)
    if len(rows) != len(addrs):
        raise SystemExit(f"{symbolizer}: {len(rows)} answers for {len(addrs)} addresses")
    return rows


def tool_frames(named):
    """The tool's frames with the names as the DWARF has them (mangled), as the reference is asked for them."""
    return [(f.get("raw_function", f["function"]), f["file"], int(f["line"])) for f in named.get("frames", [])]


def raw_symbol(named):
    """The tool's symbol as the symbol table has it."""
    if named.get("raw_symbol"):
        return named["raw_symbol"]
    return "_" + named["symbol"] if named.get("symbol") else ""


class Compare:
    def __init__(self, table):
        self.table = table
        self.aliases = 0
        self.alias_example = ""

    def outer_same(self, named, dwarf_name, theirs):
        """Is the reference's outermost function the tool's? The reference takes that name from the symbol table."""
        mine = raw_symbol(named)
        if theirs in (dwarf_name, mine, mine[1:]):
            return True
        # Another name of the same address.
        here = self.table.get(mine)
        for candidate in (theirs, "_" + theirs):
            if here is not None and self.table.get(candidate) == here:
                self.aliases += 1
                self.alias_example = self.alias_example or f"{mine} / {candidate}"
                return True
        return False

    def differ(self, named, ref):
        """Why the tool's answer is not the reference's; None when it is."""
        mine = tool_frames(named)
        if not any(file for _, file, _ in ref):
            # The reference has the symbol table only: one frame, the function's name.
            theirs = ref[-1][0] if ref else ""
            if mine:
                return f"the tool has lines, the reference none ({theirs})"
            if theirs and not self.outer_same(named, None, theirs):
                return f"function {raw_symbol(named)} | reference {theirs}"
            return None
        if len(mine) != len(ref):
            return f"{len(mine)} frame(s) | reference {len(ref)}: {' > '.join(f'{n} {base(f)}:{l}' for n, f, l in ref)}"
        for depth, ((name, file, line), (rname, rfile, rline)) in enumerate(zip(mine, ref)):
            if name != rname and not (depth == 0 and self.outer_same(named, name, rname)):
                return f"frame {depth}: function {name} | reference {rname}"
            if base(file) != base(rfile) or line != rline:
                return f"frame {depth} ({name}): {base(file)}:{line} | reference {base(rfile)}:{rline}"
        return None


def show(named):
    frames = [(f["function"], f["file"], int(f["line"])) for f in named.get("frames", [])]
    name = named.get("symbol") or "?"
    offset = named.get("offset")
    text = f"{name}+{offset}" if offset else name
    if frames:
        text += "  (" + " > ".join((f"{n} " if i else "") + f"{base(f)}:{l}" for i, (n, f, l) in enumerate(frames)) + ")"
    return text


def run_tool(tool, env, args, stdin=None):
    done = subprocess.run([tool, "sym", "kernel", "-f", "json"] + args, input=stdin, capture_output=True, text=True, env=env)
    if done.returncode != 0:
        raise SystemExit(f"oneios-tool sym kernel {' '.join(args)}: status {done.returncode}\n{done.stderr}")
    return json.loads(done.stdout)


def main(argv):
    tool, kc_path, kernel_path = option(argv, "--tool"), option(argv, "--kc"), option(argv, "--kernel")
    log, ref, index = option(argv, "--log"), option(argv, "--ref"), option(argv, "--index")
    expect_path, dsym = option(argv, "--expect"), option(argv, "--dsym")
    sample = int(option(argv, "--sample", "20000"))
    symbolizer = option(argv, "--symbolizer", "llvm-symbolizer")
    if not all([tool, kc_path, kernel_path, log, ref, index]):
        print(__doc__, file=sys.stderr)
        return 2
    env = dict(os.environ, ONEIOS_KSYM_INDEX=index)
    files = ["--kc", kc_path, "--kernel", kernel_path] + (["--dsym", dsym] if dsym else [])
    failures = 0

    kc, kernel = kcmap.open_map(kc_path), kcmap.open_map(kernel_path)
    member = kcmap.kernel_member(kc)
    in_kc = {s[0]: s for s in kcmap.segments(kc, member)}
    own = kcmap.symbols(kc, member)
    _, cmds = kcmap.commands(kernel, 0)
    segs = kcmap.segments(kernel, cmds)
    table = kcmap.symbols(kernel, cmds)
    compare = Compare(table)

    def forward(addr):
        seg = next((s for s in segs if s[1] <= addr < s[1] + s[2] and s[0] in in_kc), None)
        return None if seg is None else addr + in_kc[seg[0]][1] - seg[1]

    report = run_tool(tool, env, files + [log])
    for warning in report["warnings"]:
        print(f"  tool warning: {warning}")
    if not report["panics"]:
        print("  FAIL: the tool found no panic block in the log")
        return 1
    lines_compared = ref != kernel_path
    print(f"1. the log's addresses against {symbolizer} ({'function, file:line and inlined frames' if lines_compared else 'NO DWARF at hand: function names only'})")
    named_functions = []
    for block in report["panics"]:
        for warning in block["warnings"]:
            print(f"  tool warning (panic at line {block['line']}): {warning}")
        entries = block["frames"]
        known = [e for e in entries if e.get("kernel_address")]
        answers = reference(symbolizer, ref, [int(e["kernel_address"], 16) for e in known])
        by_id = {id(e): a for e, a in zip(known, answers)}
        for e in entries:
            if id(e) not in by_id:
                where = f"{e.get('member')} {e.get('segment')}+{e.get('segment_offset')}" if e.get("member") else e.get("note", "not named")
                print(f"  --    {e['kind']:<6} {e['address']}  not compared (not in the kernel): {where}")
                continue
            why = compare.differ(e, by_id[id(e)])
            failures += why is not None
            print(f"  {'DIFF' if why else 'same'}  {e['kind']:<6} {e['address']}  {show(e)}" + (f"   <- {why}" if why else ""))
            raw = raw_symbol(e)
            if raw and raw not in named_functions:
                named_functions.append(raw)

    print("2. running addresses: collection address + slide = the log's address")
    for block in report["panics"]:
        slide = int(block["slide"], 16) if block.get("slide") else None
        frames_kc = {e["kc_address"]: e for e in block["frames"] if e["kind"] == "frame" and e.get("kc_address")}
        for e in block["frames"]:
            if e["space"] != "running":
                if e["kind"] in ("pc", "lr") and e["space"] == "outside":
                    failures += 1
                    print(f"  FAIL  {e['kind']} {e['address']}: {e.get('note')}")
                continue
            back = (int(e["kc_address"], 16) + slide) & 0xFFFFFFFFFFFFFFFF
            ok = back == int(e["address"], 16)
            failures += not ok
            twin = frames_kc.get(e["kc_address"])
            also = f"; the same place as frame {twin['address']} ({twin.get('symbol')})" if twin else ""
            print(f"  {'same' if ok else 'FAIL'}  {e['kind']:<6} {e['address']} - {slide:#x} = {e['kc_address']}  {e.get('symbol')}+{e.get('offset')}{also}")

    if expect_path:
        print("3. make-log.py's list: the kernel address and the function of each address")
        by_addr = {}
        for block in report["panics"]:
            for e in block["frames"]:
                by_addr[(e["kind"], int(e["address"], 16))] = e
        for line in open(expect_path):
            kind, shown, kernel_addr, name = line.split()
            e = by_addr.get((kind, int(shown, 16)))
            ok = e is not None and e.get("kernel_address") == kernel_addr and e.get("symbol") == name
            failures += not ok
            got = f"{e.get('kernel_address')} {e.get('symbol')}" if e else "not in the tool's output"
            print(f"  {'same' if ok else 'FAIL'}  {kind:<6} {shown}  {kernel_addr} {name}" + ("" if ok else f"   <- tool: {got}"))
    else:
        print("3. no --expect list (a real log): skipped")

    print("4. each function's address in the collection: the tool | kcmap.py with the segment's move | the collection's own symbol table")
    for raw in named_functions:
        if raw not in table:
            print(f"  --    {raw}: not a symbol of the kernel")
            continue
        mine = run_tool(tool, env, files + ["--symbol", raw[1:]])["addresses"]
        mine = sorted({a["kc_address"] for a in mine if a.get("kc_address") and raw_symbol(a) == raw and a.get("offset") == "0x0"})
        ours = forward(table[raw])
        kept = own.get(raw)
        ok = mine == [f"{ours:#x}"] and (kept is None or kept == ours)
        failures += not ok
        kept_text = f"{kept:#x}" if kept is not None else "not kept"
        print(f"  {'same' if ok else 'FAIL'}  {raw[1:]:<40} {', '.join(mine) or 'none'} | {ours:#x} | {kept_text}")

    # 5. whole functions, and a sample of the kernel's code.
    starts = sorted(set(table.values()))
    sweep = []
    for raw in named_functions:
        if raw not in table:
            continue
        start = table[raw]
        seg = next(s for s in segs if s[1] <= start < s[1] + s[2])
        nxt = bisect.bisect_right(starts, start)
        end = min(starts[nxt] if nxt < len(starts) else seg[1] + seg[2], seg[1] + seg[2])
        sweep.extend(range(start, end, 4))
    in_functions = len(sweep)
    code = next((s for s in segs if s[0] == "__TEXT_EXEC"), None)
    if sample and code:
        rng = random.Random(27)
        sweep.extend(code[1] + 4 * rng.randrange(code[2] // 4) for _ in range(sample))
    sweep = [a for a in sweep if forward(a) is not None]
    print(f"5. {in_functions} instructions of the {len(named_functions)} functions the panic names, and {len(sweep) - in_functions} more addresses of __TEXT_EXEC")
    if sweep:
        compare.aliases = 0
        mine = run_tool(tool, env, files + ["--stdin"], stdin="".join(f"{forward(a):#x}\n" for a in sweep))["addresses"]
        theirs = reference(symbolizer, ref, sweep)
        wrong_place = sum(1 for a, m in zip(sweep, mine) if m.get("kernel_address") != f"{a:#x}")
        diffs = [(a, m, why) for a, m, t in zip(sweep, mine, theirs) if (why := compare.differ(m, t)) is not None]
        inlined = sum(1 for m in mine if len(m.get("frames", [])) > 1)
        with_lines = sum(1 for m in mine if m.get("frames"))
        print(f"  {len(sweep)} addresses: {len(sweep) - len(diffs)} the same, {len(diffs)} differ; {with_lines} with file:line, {inlined} of them in inlined code; {wrong_place} mapped to another kernel address than asked")
        if compare.aliases:
            print(f"  note: at {compare.aliases} of the same ones the two tools print different names of one address (both are symbols there), e.g. {compare.alias_example}")
        failures += len(diffs) + wrong_place
        for a, m, why in diffs[:12]:
            print(f"  DIFF  {a:#x} (collection {m.get('kc_address')})  {show(m)}   <- {why}")
        if len(diffs) > 12:
            print(f"  ... and {len(diffs) - 12} more")

    print("6. the collection alone (--no-kernel): function + offset from the symbols it keeps for the kernel")
    alone = run_tool(tool, env, ["--kc", kc_path, "--no-kernel", log])
    if not own:
        print("  --    the collection keeps no symbols of the kernel: not compared")
    for full_block, alone_block in zip(report["panics"], alone["panics"]):
        if not own:
            break
        same = 0
        for full, lone in zip(full_block["frames"], alone_block["frames"]):
            if not full.get("kernel_address"):
                continue
            a, b = raw_symbol(full), raw_symbol(lone)
            ok = full.get("offset") == lone.get("offset") and (a == b or (a in own and own.get(a) == own.get(b)))
            same += ok
            failures += not ok
            if not ok:
                print(f"  FAIL  {full['kind']:<6} {full['address']}  {a}+{full.get('offset')} | alone: {b}+{lone.get('offset')}")
        print(f"  {same} address(es) of the panic at line {full_block['line']}: the same function + offset")

    print(f"{'FAIL' if failures else 'PASS'}: {failures} difference(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
