#!/usr/bin/env python3
"""Where two builds of the hosted kernel differ, byte by byte: kernel-diff.py A.macho B.macho

Two builds of the same source are not the same file: XNU's version string carries the build's time, the linker
gives each link its own LC_UUID, the symbol table's N_OSO entries carry each object file's modification time, and
the build seals the kernel with an HMAC. This names the place of
every differing byte (the Mach-O header and load commands, a section, a symbol table entry and its type, the
string table) and says whether all of them are of those four kinds.
Exit status: 0 the two kernels differ only there (or not at all), 1 they differ elsewhere, 2 usage.
"""
import struct
import sys
from collections import Counter

if len(sys.argv) != 3:
    sys.exit(__doc__.split('\n')[0])
a, b = (open(p, 'rb').read() for p in sys.argv[1:])
if len(a) != len(b):
    print(f'sizes differ: {len(a)} and {len(b)} bytes')
    sys.exit(1)


def layout(data):
    """[(start, end, name)] of the header's parts and of every section, the symbol table and the string table."""
    magic, _, _, _, ncmds, sizeofcmds = struct.unpack_from('<6I', data)
    assert magic == 0xfeedfacf, 'not a 64-bit Mach-O'
    places, offset, symtab = [], 32, None
    for _ in range(ncmds):
        cmd, size = struct.unpack_from('<2I', data, offset)
        kind = {0x1b: 'LC_UUID', 0x19: 'LC_SEGMENT_64', 0x2: 'LC_SYMTAB'}.get(cmd, f'load command {cmd:#x}')
        places.append((offset, offset + size, f'header: {kind}'))
        if cmd == 0x19:
            segment = data[offset + 8:offset + 24].rstrip(b'\0').decode()
            for i in range(struct.unpack_from('<I', data, offset + 64)[0]):
                at = offset + 72 + 80 * i
                name = data[at:at + 16].rstrip(b'\0').decode()
                length, fileoff = struct.unpack_from('<QI', data, at + 40)
                if data[at + 64] & 0xff != 1:           # not zero-fill
                    places.append((fileoff, fileoff + length, f'{segment},{name}'))
        elif cmd == 0x2:
            symtab = struct.unpack_from('<4I', data, offset + 8)
        offset += size
    return places, symtab


places, (symoff, nsyms, stroff, strsize) = layout(a)
found = Counter()
strings = set()
for at in range(len(a)):
    if a[at] == b[at]:
        continue
    if symoff <= at < symoff + nsyms * 16:
        index, field = divmod(at - symoff, 16)
        kind = a[symoff + index * 16 + 4]
        what = {0x66: 'N_OSO', 0x64: 'N_SO', 0x24: 'N_FUN', 0x2e: 'N_BNSYM', 0x4e: 'N_ENSYM'}.get(kind, f'type {kind:#x}')
        found[f'symbol table: {what} entries, the {"value" if field >= 8 else "name, type or section"}'] += 1
    elif stroff <= at < stroff + strsize:
        found['string table'] += 1
    else:
        name = next((n for s, e, n in places if s <= at < e), 'outside every section')
        if not name.startswith('header'):
            start = at
            while start > 0 and 32 <= a[start - 1] < 127:
                start -= 1
            end = at
            while end < len(a) and 32 <= a[end] < 127:
                end += 1
            if end - start >= 16:
                strings.add((name, a[start:end].decode()[:100], b[start:end].decode()[:100]))
                name += ' (in a string)'
        found[name] += 1
for name, count in sorted(found.items()):
    print(f'{count:6d} bytes  {name}')
for name, one, two in sorted(strings):
    print(f'  {name}: {one!r}\n  {" " * len(name)}  {two!r}')
total = sum(found.values())
# the time in the version string (__TEXT,__const has it twice), the link's UUID, the objects' times, the seal
KINDS = (('the build\'s time in the version string', lambda name: name.endswith('(in a string)')),
         ('the link\'s UUID', lambda name: name == 'header: LC_UUID'),
         ('the object files\' modification times (N_OSO)', lambda name: name.startswith('symbol table: N_OSO entries, the value')),
         ('the seal', lambda name: 'hmac' in name.lower()))
other = sorted(name for name in found if not any(test(name) for _, test in KINDS))
if other:
    print(f'{total} bytes differ; NOT only the build\'s time, the UUID, the objects\' times and the seal: {other}')
    sys.exit(1)
kinds = [kind for kind, test in KINDS if any(test(name) for name in found)]
print(f'{total} bytes differ: {", ".join(kinds)}; nothing else' if total else 'the same file')
