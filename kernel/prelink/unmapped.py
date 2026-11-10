#!/usr/bin/env python3
"""Name the original kernel function behind a "fileset: original component called an unmapped kernel function" panic.

usage: unmapped.py --xnuprelink DIR --kernelcache KC --report kernel.kc.prelink.json [--map MAP] [--lines N] ADDRESS...

With --target ADDRESS is the original function itself. Otherwise ADDRESS is the panic's "from" address (a return address: the call is the instruction before it) as xnuhost prints
it (0x20...), as linked in kernel.kc (0xfffffff0...) or as in the original kernelcache (0xfffffe00...). Prints the
kext that made the call, the original kernel function it called (through a stub or directly), the strings that
function references and its first instructions (llvm-mc), and the pairing the matcher had for it, if any. The answer
goes into reviewed-*.json as a "functions" record with its evidence.
"""
import argparse
import json
from pathlib import Path
import struct
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--xnuprelink', required=True)
    parser.add_argument('--kernelcache', required=True)
    parser.add_argument('--report', required=True)
    parser.add_argument('--map')
    parser.add_argument('--lines', type=int, default=24)
    parser.add_argument('--window', default='0x2000004000', help='where xnuhost placed the collection')
    parser.add_argument('--target', action='store_true', help='ADDRESS is an original kernel function, not a caller')
    parser.add_argument('address', nargs='+')
    args = parser.parse_args()
    sys.path.insert(0, args.xnuprelink)
    from kc_image import Image, scan, signed
    raw = Path(args.kernelcache).read_bytes()
    report = json.loads(Path(args.report).read_text())
    slide, link = int(report['collection_slide'], 16), int(report['native_link_address'], 16)
    pairs = json.loads(Path(args.map).read_text()) if args.map else {'functions': {}, 'data': {}}
    cache = Image(raw)
    kernel = Image(raw, cache.entries['com.apple.kernel'][1])
    kernel_ranges = kernel.mapped()
    fixups = cache.fixups()
    owners = []
    for name, (_, at) in cache.entries.items():
        for s in Image(raw, at).segments:
            if s['size'] and s['name'] != '__LINKEDIT':
                owners.append((s['va'], s['va'] + s['size'], name, s['name']))

    def owner(address):
        return next((f'{n} {seg}' for b, e, n, seg in owners if b <= address < e), '?')

    def word(address):
        return struct.unpack_from('<I', raw, cache.offset(address, 4))[0]

    def disassemble(address, count):
        blob = raw[cache.offset(address, 4 * count):][:4 * count]
        out = subprocess.run(['llvm-mc', '--disassemble', '-triple=arm64e', '-mattr=+v8.5a,+pauth,+bti'],
                             input=' '.join('0x%02x' % b for b in blob), capture_output=True, text=True).stdout
        lines = [line for line in out.splitlines() if line.startswith('\t') and not line.startswith('\t.')]
        return [f'  {address + 4 * i:#x}:{" ".join(line.split())}' for i, line in enumerate(lines)]

    def through_stub(target):
        """adrp x17; add x17; ldr x16, [x17]; braa x16, x17 -> the slot's fixup target."""
        if any(b <= target < e for b, e in kernel_ranges):
            return target, None
        _, refs, _ = scan(raw[cache.offset(target, 16):][:16], target)
        for _, slot, _ in refs:
            if slot in fixups:
                return fixups[slot][0], slot
        return None, None

    starts = sorted(kernel.function_starts())
    for text in args.address:
        address = int(text, 16)
        if address < 1 << 40:
            address = address - int(args.window, 16) + link
        if address >= link:
            address = (address - slide) & (1 << 64) - 1
        if args.target:
            target, slot = address, None
        else:
            call = address - 4
            instruction = word(call)
            print(f'{text}: call at {call:#x} (original), in {owner(call)}')
            if instruction & 0x7c000000 != 0x14000000:
                print('  not a direct branch:'); print('\n'.join(disassemble(call - 12, 5))); continue
            target, slot = through_stub(call + signed(instruction & 0x3ffffff, 26) * 4)
            if target is None:
                print('  the branch target is not a stub with a bound slot'); continue
        end = next((s for s in starts if s > target), target + 4 * args.lines)
        print(f'  original kernel function {target:#x}, {end - target} bytes' + (f', through the slot {slot:#x}' if slot else ''))
        print(f'  matcher: {pairs["functions"].get(hex(target))}')
        index = starts.index(target) if target in starts else None
        if index is not None:       # the original and the reference kernel are linked in the same order
            def near(step):
                found, at = [], index + step
                while 0 <= at < len(starts) and len(found) < 3 and abs(at - index) < 40:
                    name = pairs['functions'].get(hex(starts[at]), {}).get('name')
                    if name:
                        found.append(f'{name} ({(starts[at] - target) // 4:+d})')
                    at += step
                return found
            print(f'  paired neighbours before: {near(-1)}')
            print(f'  paired neighbours after:  {near(1)}')
        _, refs, _ = scan(raw[cache.offset(target, end - target):][:end - target], target)
        strings = []
        for _, ref, _ in refs:
            try:
                value = kernel.cstring(ref)
            except Exception:
                value = None
            if value and len(value) > 3 and value not in strings:
                strings.append(value)
        print(f'  strings: {strings[:8]}')
        print('\n'.join(disassemble(target, min(args.lines, (end - target) // 4))))


if __name__ == '__main__':
    main()
