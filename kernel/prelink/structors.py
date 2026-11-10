#!/usr/bin/env python3
"""Pair the constructors and base destructors of kernel classes that kexts subclass, by the vtable they store.

usage: structors.py --xnuprelink DIR --kernelcache KC --native NATIVE --map MAP --reviewed reviewed.json --out structors.json

OSDefineMetaClassAndStructors emits, for a class X, X::X(const OSMetaClass *), (with default structors) X::X(), and
X::~X(), in that order, after the file's static initializer. In the original kernel each constructor is
    mov w8, #1; str w8, [x0, #8]; adrp/add x16, <X's vtable>; add x16, x16, #16; ...; pacda; str x16, [x0]
(the inlined OSObject constructor, then the vtable) possibly followed by member initializers, and a trivial base
destructor is `mov x8, #-1; str x8, [x0]; ret`. Kext subclasses call them directly, so each is a kext-facing
function, and none has a string or a named callee for the matcher to go by.

For every vtable the pairing or the reviewed file names (__ZTV<X>), the unpaired original functions whose first
instructions store that vtable + 16 into [x0] after the retain count are, in address order, C2EPK11OSMetaClass and
C2Ev; the unpaired function that directly follows them and is the trivial destructor is D2Ev. A name is used only
when the native kernel defines it once. Factories (X::withFoo) are paired by the class they allocate and the init
slot they call; see the code. Output: pairs in align.py's format ('by': 'vtable').
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for name in ('xnuprelink', 'kernelcache', 'native', 'map', 'reviewed', 'out'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--classes', help='kclasses.py output: more vtables by name')
    args = parser.parse_args()
    sys.path.insert(0, args.xnuprelink)
    from kc_image import Image, scan
    raw = Path(args.kernelcache).read_bytes()
    pairing = json.loads(Path(args.map).read_text())
    reviewed = json.loads(Path(args.reviewed).read_text())
    cache = Image(raw)
    kernel = Image(raw, cache.entries['com.apple.kernel'][1])
    starts = sorted(kernel.function_starts())
    paired = {int(k, 16) for k in pairing['functions']} | {int(r['original'], 16) for r in reviewed.get('functions', [])}
    vtables = {int(k, 16): v['name'] for k, v in pairing['data'].items() if (v.get('name') or '').startswith('__ZTV')}
    if args.classes:
        vtables.update({int(r['original'], 16): r['native_symbol'] for r in json.loads(Path(args.classes).read_text())['data']
                        if r['native_symbol'].startswith('__ZTV')})
    vtables.update({int(r['original'], 16): r['native_symbol'] for r in reviewed.get('data', [])
                    if r['native_symbol'].startswith('__ZTV')})
    native = {}
    for names in Image(Path(args.native).read_bytes()).symbols().values():
        for name in names:
            native[name] = native.get(name, 0) + 1
    exec_ranges = [(s['va'], s['va'] + s['size']) for s in kernel.segments if s['name'] == '__TEXT_EXEC']
    constructors = {}
    for i, start in enumerate(starts[:-1]):
        if not any(b <= start < e for b, e in exec_ranges):
            continue
        size = min(starts[i + 1] - start, 48)
        try:
            blob = kernel.read(start, size)
        except ValueError:
            continue
        words = struct.unpack_from(f'<{size // 4}I', blob)
        # mov w8, #1 ; str w8, [x0, #8] within the first three instructions
        if 0x52800028 not in words[:3] or 0xb9000808 not in words[:4]:
            continue
        _, refs, _ = scan(blob, start)
        for _, ref, kind in refs:
            if kind == 'add' and ref in vtables:
                constructors.setdefault(ref, []).append((start, starts[i + 1]))
                break
    pairs = []

    def add(address, name, cls, why):
        if address in paired or native.get(name) != 1:
            return
        pairs.append({'original': hex(address), 'native_symbol': name, 'score': 3.0, 'by': 'vtable',
                      'shared_features': 1, 'features': 1, 'between': [cls, why]})
        paired.add(address)

    for vtable, found in sorted(constructors.items()):
        cls = vtables[vtable][len('__ZTV'):]
        found.sort()
        # The macro's pair comes first and adjacent; later functions that store the vtable (constructors with
        # member initializers, inlined copies) are left to a review.
        found = found[:2] if len(found) >= 2 and found[0][1] == found[1][0] else found[:1] if len(found) == 1 else []
        if not found:
            continue
        add(found[0][0], f'__ZN{cls}C2EPK11OSMetaClass', vtables[vtable], 'first function that stores the vtable')
        if len(found) == 2:
            add(found[1][0], f'__ZN{cls}C2Ev', vtables[vtable], 'second function that stores the vtable')
        after = found[-1][1]
        try:
            trivial = kernel.read(after, 16)
        except ValueError:
            continue
        if struct.unpack('<4I', trivial)[1:] == (0x92800008, 0xf9000008, 0xd65f03c0):
            add(after, f'__ZN{cls}D2Ev', vtables[vtable], 'the trivial destructor that follows the constructors')
    # Factories. X::withFoo(args) is: allocate from X::gMetaClass (the metaclass's virtual alloc, vtable +104), then
    # the object's virtual initWithFoo(args) at some slot. The slot names the init method in our vtable of X, and
    # the factory is the same name without "init" ("initWithCapacity" -> "withCapacity"), when our kernel has it.
    metaclasses = {int(k, 16): v['name'] for k, v in pairing['data'].items()
                   if (v.get('name') or '').endswith('10gMetaClassE')}
    if args.classes:
        metaclasses.update({int(r['original'], 16): r['native_symbol'] for r in json.loads(Path(args.classes).read_text())['data']
                            if r['native_symbol'].endswith('10gMetaClassE')})
    native_image = Image(Path(args.native).read_bytes())
    native_symbols, native_fixups = native_image.symbols(), native_image.fixups()
    address_of = {name: address for address, names in native_symbols.items() for name in names}
    factories, candidates = 0, {}
    for i, start in enumerate(starts[:-1]):
        if start in paired or not any(b <= start < e for b, e in exec_ranges):
            continue
        size = starts[i + 1] - start
        if not 96 <= size <= 240:
            continue
        blob = kernel.read(start, size)
        _, refs, _ = scan(blob[:64], start)
        named = [metaclasses[ref] for _, ref, kind in refs if kind == 'add' and ref in metaclasses]
        if len(named) != 1:
            continue
        words = struct.unpack_from(f'<{size // 4}I', blob)
        offsets = []
        for at, word in enumerate(words):
            if word & 0xffe00fff == 0xf8400e08:                       # ldr x8, [x16, #imm]!
                offsets.append(word >> 12 & 0x1ff)
            elif word == 0x8b110210 and at and words[at - 1] & 0xffe0001f == 0xd2800011:   # mov x17, #imm; add x16, x16, x17
                offsets.append(words[at - 1] >> 5 & 0xffff)
        # alloc, init, and at most the release of a failed init (vtable +40): nothing else is called virtually
        if len(offsets) < 2 or offsets[0] != 104 or any(offset != 40 for offset in offsets[2:]) or len(offsets) > 3:
            continue
        cls = named[0][len('__ZN'):-len('10gMetaClassE')]
        vtable = address_of.get('__ZTV' + cls)
        slot = native_fixups.get(vtable + 16 + offsets[1]) if vtable else None
        if not slot:
            continue
        for init in native_symbols.get(slot[0], ()):
            prefix = f'__ZN{cls}'
            rest = init[len(prefix):]
            digits = ''.join(ch for ch in rest[:3] if ch.isdigit())
            if not init.startswith(prefix) or not digits or not rest[len(digits):].startswith('initWith'):
                continue
            method = rest[len(digits):len(digits) + int(digits)][4:]              # WithFoo
            method = method[0].lower() + method[1:]
            factory = f'{prefix}{len(method)}{method}{rest[len(digits) + int(digits):]}'
            candidates.setdefault(factory, []).append((start, named[0], f'allocates, then the virtual {init} at vtable +{offsets[1]}'))
    for factory, found in sorted(candidates.items()):
        if len(found) == 1:            # a larger function that begins with an inlined factory looks the same: one only
            before = len(pairs)
            add(found[0][0], factory, found[0][1], found[0][2])
            factories += len(pairs) - before
    # VNOP wrappers. VNOP_FOO(...) of bsd/vfs/kpi_vfs.c builds its argument structure with a_desc = &vnop_foo_desc and
    # calls the vnode's operation at that descriptor's offset; the descriptors are paired by name (the matcher, and
    # reviewed records from their vdesc_name strings). An unpaired function whose only named data is one such
    # descriptor is that wrapper, when exactly one function qualifies.
    descriptors = {int(k, 16): v['name'] for k, v in pairing['data'].items()
                   if (v.get('name') or '').startswith('_vnop_') and v['name'].endswith('_desc')}
    descriptors.update({int(r['original'], 16): r['native_symbol'] for r in reviewed.get('data', [])
                        if r['native_symbol'].startswith('_vnop_') and r['native_symbol'].endswith('_desc')})
    # Other functions inline a wrapper (vn_close has VNOP_CLOSE's body): only inside kpi_vfs.c's address range, which
    # the wrappers the matcher did pair mark out, and only for a name no function has yet.
    paired_wrappers = sorted(int(k, 16) for k, v in pairing['functions'].items() if (v.get('name') or '').startswith('_VNOP_'))
    wrapper_names = {v['name'] for v in pairing['functions'].values() if (v.get('name') or '').startswith('_VNOP_')}
    wrapper_names |= {r['native_symbol'] for r in reviewed.get('functions', [])}
    low, high = (paired_wrappers[0], paired_wrappers[-1]) if paired_wrappers else (0, 0)
    wrappers, vnops = {}, 0
    for i, start in enumerate(starts[:-1]):
        if start in paired or not low - 0x1000 <= start <= high + 0x1000:
            continue
        size = starts[i + 1] - start
        if not 40 <= size <= 1200:
            continue
        _, refs, _ = scan(kernel.read(start, min(size, 160)), start)
        named = {descriptors[ref] for _, ref, kind in refs if ref in descriptors}
        if len(named) == 1:
            wrappers.setdefault(next(iter(named)), []).append(start)
    for descriptor, found in sorted(wrappers.items()):
        name = '_VNOP_' + descriptor[len('_vnop_'):-len('_desc')].upper()
        if len(found) == 1 and name not in wrapper_names:
            before = len(pairs)
            add(found[0], name, descriptor, 'the one unpaired function in kpi_vfs.c\'s range that passes this operation descriptor')
            vnops += len(pairs) - before
    record = {'kernelcache_sha256': hashlib.sha256(raw).hexdigest(), 'vtables_named': len(vtables), 'factories': factories,
              'vnop_wrappers': vnops,
              'classes_with_constructors': len(constructors), 'pairs': pairs}
    Path(args.out).write_text(json.dumps(record, indent=1) + '\n')
    print(f'structors: {len(pairs) - factories - vnops} constructors and destructors of {len(constructors)} classes paired by their vtable, {factories} factories by their init slot, {vnops} VNOP wrappers by their descriptor '
          f'({len(vtables)} vtables named)')


if __name__ == '__main__':
    main()
