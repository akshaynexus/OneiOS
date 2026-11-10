#!/usr/bin/env python3
"""Apple's kexts under the hosted kernel: the thread register rewrite and the list of privileged instructions.

usage: hosted.py KERNELCACHE OUT REPORT.json [--closure select.json] [--x18]

The hosted kernel (kernel/patches/oneios-hosted, hosted.h) keeps the current thread in TPIDR_EL0 and the vCPU block
(struct xh_cpu *) in x18; TPIDR_EL1 does not exist for a process. Every `mrs xN, TPIDR_EL1` / `msr TPIDR_EL1, xN`
in the executable segments of the fileset's kext entries (never the original kernel entry: it is replaced) becomes
the same instruction on TPIDR_EL0, one word for one word. Every `mrs xN, MPIDR_EL1` becomes `ldr wN, [x18]`, the
vCPU index at offset 0 of the vCPU block, as kernel/hosted-fixup.py does for the kernel itself (at EL0 Linux
answers MPIDR_EL1 with a constant). Every `orr xD, xN, #0x00ff000000000000` becomes
`and xD, xN, #0xff00ffffffffffff`: the inline smr_hash_array_decode of <kern/smr_hash.h> keeps the array's order in
bits 48..55 of the pointer and restores them to 0xff, which is right for a kernel address on hardware and wrong here,
where kernel addresses are low (the hosted kernel's own copy clears them, smr_hash.h under XNU_HOSTED). This runs on the kernelcache before the pairing and the
emission, so the emitter's component HMACs are computed over the rewritten code. OUT is written only when a word
changed; with no rewrite the build uses KERNELCACHE as it is.

The report also counts, per kext, every other system-register, DAIF and EL1 `sys` instruction a process cannot
execute (they reach
xnuhost's SIGILL emulation), and with --x18 every instruction that names x18/w18 (llvm-mc; a kext that writes x18
loses the vCPU block).
"""
import argparse
from collections import Counter, defaultdict
import hashlib
import json
import struct
import subprocess

LC_SEGMENT_64, LC_FILESET_ENTRY = 0x19, 0x80000035
TPIDR_EL1 = (3, 0, 13, 0, 4)
MPIDR_EL1 = (3, 0, 0, 0, 5)
TPIDR_EL0_FIELD = 0x1bd040       # op0=3 op1=3 CRn=13 CRm=0 op2=2, bits 20..5
# Readable or writable at EL0 under Linux: not counted.
EL0 = {(3, 3, 13, 0, 2), (3, 3, 13, 0, 3), (3, 3, 4, 2, 0), (3, 3, 4, 4, 0), (3, 3, 4, 4, 1),
       (3, 3, 0, 0, 1), (3, 3, 0, 0, 7), (3, 3, 14, 0, 0), (3, 3, 14, 0, 1), (3, 3, 14, 0, 2), (3, 3, 2, 4, 0),
       (3, 3, 2, 4, 1), (3, 3, 4, 2, 5), (3, 3, 4, 2, 6), (3, 3, 13, 0, 5), (3, 3, 13, 0, 7)}


def entries(data):
    ncmds, = struct.unpack_from('<I', data, 16)
    off, out = 32, []
    for _ in range(ncmds):
        cmd, length = struct.unpack_from('<II', data, off)
        if cmd == LC_FILESET_ENTRY:
            _, fileoff, name_at = struct.unpack_from('<QQI', data, off + 8)
            out.append((data[off + name_at:data.index(b'\0', off + name_at)].decode(), fileoff))
        off += length
    return out


def exec_segments(data, at):
    ncmds, = struct.unpack_from('<I', data, at + 16)
    off = at + 32
    for _ in range(ncmds):
        cmd, length = struct.unpack_from('<II', data, off)
        if cmd == LC_SEGMENT_64:
            name, va, _, fileoff, filesize, _, initprot = struct.unpack_from('<16s4QII', data, off + 8)
            if initprot & 4 and filesize:
                yield name.rstrip(b'\0').decode(), va, fileoff, filesize
        off += length


def register(word):
    return (2 + (word >> 19 & 1), word >> 16 & 7, word >> 12 & 15, word >> 8 & 15, word >> 5 & 7)


def name_of(key):
    return {TPIDR_EL1: 'TPIDR_EL1', (3, 3, 4, 2, 1): 'DAIF'}.get(key, 'S%d_%d_C%d_C%d_%d' % key)


def x18_users(data, segments):
    """Instructions that name x18 or w18, by disassembly (llvm-mc), per segment."""
    found = []
    for name, va, fileoff, filesize in segments:
        step = 1 << 20
        for start in range(0, filesize, step):
            blob = data[fileoff + start:fileoff + min(filesize, start + step)]
            text = ' '.join('0x%02x' % b for b in blob)
            out = subprocess.run(['llvm-mc', '--disassemble', '-triple=arm64e', '-mattr=+v8.5a,+pauth,+bti'],
                                 input=text, capture_output=True, text=True).stdout
            lines = [line for line in out.splitlines() if line.startswith('\t') and not line.startswith('\t.')]
            if len(lines) != len(blob) // 4:
                found.append({'address': hex(va + start), 'instruction': 'undecoded range (data in code)'})
                continue
            for index, line in enumerate(lines):
                tokens = line.replace(',', ' ').replace('[', ' ').replace(']', ' ').replace('!', ' ').split()
                if 'x18' in tokens[1:] or 'w18' in tokens[1:]:
                    found.append({'address': hex(va + start + 4 * index), 'instruction': ' '.join(line.split())})
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('kernelcache')
    parser.add_argument('out')
    parser.add_argument('report')
    parser.add_argument('--closure', help='select.py record: report only the kexts of its closure')
    parser.add_argument('--x18', action='store_true', help='also list instructions that name x18 (slow)')
    args = parser.parse_args()
    source = open(args.kernelcache, 'rb').read()
    data = bytearray(source)
    closure = set(json.load(open(args.closure))['closure']) if args.closure else None
    rewrites, mpidr, pointers, privileged, x18, scanned = [], [], [], defaultdict(Counter), {}, 0
    for name, at in entries(data):
        if name == 'com.apple.kernel':
            continue
        segments = list(exec_segments(data, at))
        for segment, va, fileoff, filesize in segments:
            scanned += filesize
            for offset in range(fileoff, fileoff + filesize - 3, 4):
                word, = struct.unpack_from('<I', data, offset)
                if closure is None or name in closure:
                    if word & 0xfffff0df == 0xd50340df:                 # msr DAIFSet / DAIFClr, #imm
                        privileged[name]['msr DAIFClr' if word & 0x20 else 'msr DAIFSet'] += 1
                    elif word & 0xfff80000 == 0xd5080000 and (word >> 16 & 7) != 3:   # sys (tlbi, at, dc at EL1)
                        privileged[name]['sys #%d, C%d, C%d, #%d' % (word >> 16 & 7, word >> 12 & 15, word >> 8 & 15,
                                                                      word >> 5 & 7)] += 1
                if word & 0xfffffc00 == 0xb2501c00:                     # orr xD, xN, #0x00ff000000000000
                    new = 0x9248dc00 | (word & 0x3ff)                   # and xD, xN, #0xff00ffffffffffff
                    struct.pack_into('<I', data, offset, new)
                    pointers.append({'kext': name, 'segment': segment, 'address': hex(va + offset - fileoff),
                                     'old': f'{word:#010x}', 'new': f'{new:#010x}',
                                     'instruction': f'orr x{word & 31}, x{word >> 5 & 31}, #0xff000000000000 -> '
                                                    f'and x{word & 31}, x{word >> 5 & 31}, #0xff00ffffffffffff'})
                    continue
                if word & 0xffd00000 != 0xd5100000:
                    continue
                key = register(word)
                read = bool(word >> 21 & 1)
                if key == TPIDR_EL1:
                    new = (word & 0xffe0001f) | TPIDR_EL0_FIELD
                    struct.pack_into('<I', data, offset, new)
                    rewrites.append({'kext': name, 'segment': segment, 'address': hex(va + offset - fileoff),
                                     'old': f'{word:#010x}', 'new': f'{new:#010x}',
                                     'instruction': f'{"mrs" if read else "msr"} x{word & 31}, TPIDR_EL1 -> TPIDR_EL0'})
                elif key == MPIDR_EL1 and read:
                    new = 0xb9400240 | (word & 31)          # ldr wN, [x18]: the vCPU index (kernel/hosted-fixup.py)
                    struct.pack_into('<I', data, offset, new)
                    mpidr.append({'kext': name, 'segment': segment, 'address': hex(va + offset - fileoff),
                                  'old': f'{word:#010x}', 'new': f'{new:#010x}',
                                  'instruction': f'mrs x{word & 31}, MPIDR_EL1 -> ldr w{word & 31}, [x18]'})
                elif key not in EL0 and (closure is None or name in closure):
                    privileged[name][('mrs ' if read else 'msr ') + name_of(key)] += 1
        if args.x18 and (closure is None or name in closure):
            users = x18_users(data, segments)
            if users:
                x18[name] = users
    changed = bytes(data) != source
    if changed:
        open(args.out, 'wb').write(data)
    totals = Counter()
    for counts in privileged.values():
        totals.update(counts)
    report = {
        'kernelcache_sha256': hashlib.sha256(source).hexdigest(),
        'output_sha256': hashlib.sha256(data).hexdigest(),
        'output_written': changed,
        'kext_code_bytes_scanned': scanned,
        'convention': 'TPIDR_EL0 = current thread (TPIDR_EL1 on hardware), x18 = struct xh_cpu * of the vCPU',
        'tpidr_el1_rewrites': rewrites,
        'mpidr_el1_rewrites': mpidr,
        'kernel_pointer_rewrites': pointers,
        'privileged_instructions_scope': 'closure' if closure is not None else 'every kext entry',
        'privileged_instructions_total': dict(sorted(totals.items(), key=lambda item: -item[1])),
        'privileged_instructions_by_kext': {name: dict(sorted(counts.items())) for name, counts in sorted(privileged.items())},
        'x18_checked': args.x18,
        'x18_instructions_by_kext': x18,
    }
    json.dump(report, open(args.report, 'w'), indent=1)
    print(f'hosted: {len(rewrites)} TPIDR_EL1, {len(mpidr)} MPIDR_EL1 and {len(pointers)} kernel pointer rewrite(s) in '
          f'{scanned} bytes of kext code; '
          f'{sum(totals.values())} other privileged system-register instruction(s) in {len(privileged)} kext(s)'
          + (f'; x18 named in {len(x18)} kext(s)' if args.x18 else ''))


if __name__ == '__main__':
    main()
