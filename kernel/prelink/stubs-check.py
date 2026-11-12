#!/usr/bin/env python3
"""stubs-check.py KERNEL.KC: every `<name>` stub of the kernel (`bti c; b <name>.original`, the hosted archives'
way into an original class) must branch to the value of its N_ABS `<name>.original` symbol, inside the collection.
Prints the count of good stubs and the first bad ones; exit 1 when one is bad.
A collection emitted by XnuPrelink b74ed7c has all 507 bad (kernel/prelink/README.md, "On the phone")."""
import struct
import sys

d = open(sys.argv[1], 'rb').read()


def commands(base):
    _, _, _, _, n, _, _, _ = struct.unpack_from('<IiiIIIII', d, base)
    off = base + 32
    for _ in range(n):
        c, s = struct.unpack_from('<II', d, off)
        yield c, off, s
        off += s


segments, kernel = [], None
for c, off, s in commands(0):
    if c == 0x19:
        vm, vs, fo, _ = struct.unpack_from('<QQQQ', d, off + 24)
        segments.append((vm, vs, fo))
    if c == 0x80000035:
        _, fo, eid = struct.unpack_from('<QQI', d, off + 8)
        if d[off + eid:off + s].split(b'\0')[0] == b'com.apple.kernel':
            kernel = fo


def word(a):
    for vm, vs, fo in segments:
        if vm <= a < vm + vs:
            return struct.unpack_from('<I', d, fo + a - vm)[0]
    return None


symbols = {}
for c, off, _ in commands(kernel):
    if c == 2:
        so, ns, stro, _ = struct.unpack_from('<IIII', d, off + 8)
        for i in range(ns):
            sx, _, _, _, v = struct.unpack_from('<IBBHQ', d, so + 16 * i)
            symbols[d[stro + sx:d.index(b'\0', stro + sx)].decode(errors='replace')] = v
good, bad = 0, []
for name, value in sorted(symbols.items()):
    if name.endswith('.original') or name + '.original' not in symbols:
        continue
    w = word(value + 4)
    target = None
    if w is not None and w & 0xfc000000 == 0x14000000:
        imm = w & 0x3ffffff
        target = value + 4 + (imm - 0x4000000 if imm & 0x2000000 else imm) * 4
    want = symbols[name + '.original']
    if target == want and word(target):
        good += 1
    else:
        bad.append(f'{name} at {value:#x}: branch to {target:#x} , .original {want:#x}' if target else f'{name}: not a branch')
print(f'stubs: {good} good, {len(bad)} bad')
for line in bad[:10]:
    print('  ' + line)
sys.exit(1 if bad else 0)
