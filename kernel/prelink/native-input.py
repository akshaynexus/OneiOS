#!/usr/bin/env python3
"""The hosted kernel as XnuPrelink's native input: its weak-lookup binds resolved to its own definitions.

usage: native-input.py IN OUT REPORT.json

The hosted xnu.macho (LC_DYLD_CHAINED_FIXUPS, DYLD_CHAINED_PTR_ARM64E_KERNEL) imports the board's optional entry
points by weak lookup (ordinal BIND_SPECIAL_DYLIB_WEAK_LOOKUP, -3). Inside the kernel the only definition of each is
the kernel's own weak default; dyld's weak lookup would bind it there. XnuPrelink refuses images with chained imports,
so each such bind becomes the rebase to that definition, with its pointer-authentication fields unchanged, and the
import table is emptied. Nothing else changes; a bind with another ordinal, or with no definition, stops the step.
"""
import json
import struct
import sys

LC_SEGMENT_64, LC_SYMTAB, LC_DYLD_CHAINED_FIXUPS = 0x19, 0x2, 0x80000034
PTR_ARM64E_KERNEL = 7
WEAK_LOOKUP = 0xfd   # -3 as the 8-bit ordinal of DYLD_CHAINED_IMPORT


def main():
    src, dst, record = sys.argv[1:4]
    data = bytearray(open(src, 'rb').read())
    ncmds, = struct.unpack_from('<I', data, 16)
    off, segs, fixups, symtab = 32, [], None, None
    for _ in range(ncmds):
        cmd, length = struct.unpack_from('<II', data, off)
        if cmd == LC_SEGMENT_64:
            name, va, vmsize, fileoff, filesize = struct.unpack_from('<16s4Q', data, off + 8)
            segs.append((name.rstrip(b'\0').decode(), va, vmsize, fileoff, filesize))
        elif cmd == LC_DYLD_CHAINED_FIXUPS:
            fixups = struct.unpack_from('<II', data, off + 8)
        elif cmd == LC_SYMTAB:
            symtab = struct.unpack_from('<4I', data, off + 8)
        off += length
    base = next(va for name, va, _, fileoff, filesize in segs if fileoff == 0 and filesize)
    defined = {}
    symoff, nsyms, stroff, _ = symtab
    for i in range(nsyms):
        strx, kind, _, _, value = struct.unpack_from('<IBBHQ', data, symoff + 16 * i)
        if not kind & 0xe0 and kind & 0x0e == 0x0e:
            defined.setdefault(data[stroff + strx:data.index(b'\0', stroff + strx)].decode(), value)
    dataoff, _ = fixups
    version, starts_offset, imports_offset, symbols_offset, count, imports_format, _ = struct.unpack_from('<7I', data, dataoff)
    if imports_format != 1:
        raise SystemExit(f'native-input: imports format {imports_format}, expected DYLD_CHAINED_IMPORT (1)')
    imports = []
    for i in range(count):
        raw, = struct.unpack_from('<I', data, dataoff + imports_offset + 4 * i)
        ordinal, name_at = raw & 0xff, raw >> 9
        name = data[dataoff + symbols_offset + name_at:data.index(b'\0', dataoff + symbols_offset + name_at)].decode()
        if ordinal != WEAK_LOOKUP:
            raise SystemExit(f'native-input: import {name} has ordinal {ordinal}, not a weak lookup')
        if name not in defined:
            raise SystemExit(f'native-input: weak import {name} has no definition in the kernel')
        imports.append((name, defined[name]))
    starts = dataoff + starts_offset
    seg_count, = struct.unpack_from('<I', data, starts)
    rewritten = []
    for s in range(seg_count):
        info, = struct.unpack_from('<I', data, starts + 4 + 4 * s)
        if not info:
            continue
        at = starts + info
        _, page_size, pointer_format, segment_offset, _, page_count = struct.unpack_from('<IHHQIH', data, at)
        if pointer_format != PTR_ARM64E_KERNEL:
            raise SystemExit(f'native-input: pointer format {pointer_format} in segment {s}')
        for page in range(page_count):
            start, = struct.unpack_from('<H', data, at + 22 + 2 * page)
            if start == 0xffff:
                continue
            seg_va = base + segment_offset
            seg = next(x for x in segs if x[2] and x[1] <= seg_va < x[1] + x[2])   # not an empty segment at the same address (__LAST)
            loc = seg[3] + (seg_va - seg[1]) + page * page_size + start
            while True:
                raw, = struct.unpack_from('<Q', data, loc)
                if raw >> 62 & 1:                                   # a bind
                    ordinal = raw & 0xffff
                    name, target = imports[ordinal]
                    site = seg[1] + loc - seg[3]
                    if raw >> 63:
                        keep = raw & (0xffff << 32 | 1 << 48 | 3 << 49 | 0x7ff << 51 | 1 << 63)
                        raw = keep | (target - base)
                        rewritten.append({'site': hex(site), 'symbol': name, 'target': hex(target),
                                          'key': int(raw >> 49 & 3), 'diversity': hex(raw >> 32 & 0xffff),
                                          'address_diversity': bool(raw >> 48 & 1)})
                    else:           # a plain pointer: ordinal, 19-bit addend at bit 32 -> a 43-bit target offset
                        addend = raw >> 32 & 0x7ffff
                        if addend >> 18:
                            addend -= 1 << 19
                        raw = (raw & 0x7ff << 51) | (target + addend - base) & (1 << 43) - 1
                        rewritten.append({'site': hex(site), 'symbol': name, 'target': hex(target + addend),
                                          'key': None})
                    struct.pack_into('<Q', data, loc, raw)
                step = (raw >> 51) & 0x7ff
                if not step:
                    break
                loc += 4 * step
    if len({r['symbol'] for r in rewritten}) != len(imports):
        raise SystemExit('native-input: an import has no bind site')
    struct.pack_into('<I', data, dataoff + 16, 0)                  # imports_count
    open(dst, 'wb').write(data)
    json.dump({'input': src, 'output': dst, 'weak_binds_resolved': rewritten}, open(record, 'w'), indent=1)
    print(f'native-input: {len(rewritten)} weak bind(s) resolved to the kernel\'s own definitions')


if __name__ == '__main__':
    main()
