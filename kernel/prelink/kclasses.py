#!/usr/bin/env python3
"""Name the class objects of the original kernel: each class's metaclass, metaClass and superClass pointers, vtable.

usage: kclasses.py --xnuprelink DIR --kernelcache KC --native NATIVE --map MAP --reviewed reviewed.json --out kclasses.json

Kexts name these as data: X::metaClass for OSDynamicCast, X's vtable for a subclass's super calls, X::gMetaClass as
a subclass's parent. The matcher pairs only some of them; an unpaired one keeps its collection address, which is
another object of our kernel (a cast that always fails, a super call into nothing). Every class says its own name:

  gMetaClass   the static initializer passes x0 = the object, x1 = the class name string, x2 = the parent's
               metaclass to the OSMetaClass constructor
  metaClass,   OSDefineMetaClass emits `X::metaClass = &X::gMetaClass` and `X::superClass = &Parent::gMetaClass` as
  superClass   two adjacent constant pointers: the pair whose first points at X's object and second at its parent's
  vtable       the table whose slot 7 is the function that returns X's metaclass object (getMetaClass)

A name is used when our kernel defines it once. A pair the matcher or a reviewed record already has stands; a
disagreement is listed under "conflicts". Output: {"data": [...]} for emit.py (--aligned), vtables.py and structors.py.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import struct
import sys

RET, BTI_C = 0xd65f03c0, 0xd503245f


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for name in ('xnuprelink', 'kernelcache', 'native', 'map', 'reviewed', 'out'):
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    sys.path.insert(0, args.xnuprelink)
    from kc_image import Image, scan
    raw = Path(args.kernelcache).read_bytes()
    pairing = json.loads(Path(args.map).read_text())
    reviewed = json.loads(Path(args.reviewed).read_text())
    cache = Image(raw)
    kernel = Image(raw, cache.entries['com.apple.kernel'][1])
    fixups = cache.fixups()
    native = {}
    for names in Image(Path(args.native).read_bytes()).symbols().values():
        for name in names:
            native[name] = native.get(name, 0) + 1
    known = {int(k, 16): v['name'] for k, v in pairing['data'].items() if v.get('name')}
    known.update({int(r['original'], 16): r['native_symbol'] for r in reviewed.get('data', [])})

    def word(address):
        return struct.unpack_from('<I', raw, cache.offset(address, 4))[0]

    classes = {}                                           # metaclass object -> (name, parent metaclass object)
    getters = {}                                           # function -> metaclass object it returns
    for segment in kernel.segments:
        if segment['name'] != '__TEXT_EXEC' or not segment['filesize']:
            continue
        blob = raw[segment['fileoff']:segment['fileoff'] + segment['filesize']]
        _, refs, _ = scan(blob, segment['va'])
        by_site = {here: (target, kind) for here, target, kind in refs}
        for here, target, kind in refs:
            if kind != 'add':
                continue
            register = word(here) & 31
            if register == 0 and word(here + 4) == RET:
                start = here - 4
                getters[start - 4 if word(start - 4) == BTI_C else start] = target
            if register != 1:
                continue
            try:
                name = kernel.cstring(target)
            except Exception:
                name = None
            if not name or not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', name):
                continue
            meta = parent = None
            for back in (here - 8, here - 12, here - 16):
                if back in by_site and word(back) & 31 == 0 and by_site[back][1] == 'add':
                    meta = by_site[back][0]
                    break
            for forward in range(here + 4, here + 28, 4):
                if forward in by_site and word(forward) & 31 == 2 and by_site[forward][1] == 'add':
                    parent = by_site[forward][0]
                if word(forward) >> 26 == 0x25:
                    break
            if meta is not None and meta not in classes:
                classes[meta] = (name, parent)
    data_const = [(s['va'], s['va'] + s['size']) for s in kernel.segments if s['name'] == '__DATA_CONST']
    inside = lambda a: any(b <= a < e for b, e in data_const)   # noqa: E731
    pointing = {}
    for site, fix in fixups.items():
        if fix[0] in classes and inside(site):
            pointing.setdefault(fix[0], []).append(site)
    slot7 = {}
    for site, fix in fixups.items():
        if fix[0] in getters and inside(site):
            base = site - 16 - 7 * 8
            if base not in fixups and base + 8 not in fixups:
                slot7.setdefault(getters[fix[0]], []).append(base)
    pairs, conflicts = [], []

    def add(address, symbol, why):
        if native.get(symbol) != 1:
            return
        if address in known:
            if known[address] != symbol:
                conflicts.append({'original': hex(address), 'paired': known[address], 'class_says': symbol})
            return
        known[address] = symbol
        pairs.append({'original': hex(address), 'native_symbol': symbol, 'by': 'class', 'why': why})

    for meta, (name, parent) in sorted(classes.items()):
        prefix = f'__ZN{len(name)}{name}'
        add(meta, prefix + '10gMetaClassE', f'the static initializer registers this object as "{name}"')
        for site in pointing.get(meta, ()):
            after = fixups.get(site + 8)
            if parent is not None and after and after[0] == parent:
                add(site, prefix + '9metaClassE', f'points at {name}\'s metaclass object, followed by a pointer to its parent\'s')
                add(site + 8, prefix + '10superClassE', f'the pointer after {name}::metaClass, at its parent\'s metaclass object')
        for site in pointing.get(meta, ()):
            # Any other constant pointer to the object (a subclass's superClass, when the subclass was not found
            # above) has the value of X::metaClass: paired with it, which is right for every read.
            add(site, prefix + '9metaClassE', f'a constant pointer to {name}\'s metaclass object: the value of {name}::metaClass')
        tables = slot7.get(meta, ())
        if len(tables) == 1:
            add(tables[0], '__ZTV' + prefix[4:], f'slot 7 is the function that returns {name}\'s metaclass object')
    record = {'kernelcache_sha256': hashlib.sha256(raw).hexdigest(), 'classes': len(classes), 'data': pairs,
              'conflicts': conflicts}
    Path(args.out).write_text(json.dumps(record, indent=1) + '\n')
    kinds = {}
    for pair in pairs:
        key = 'vtable' if pair['native_symbol'].startswith('__ZTV') else pair['native_symbol'].rsplit('E', 1)[0][-9:]
        kinds[key] = kinds.get(key, 0) + 1
    print(f'kclasses: {len(classes)} kernel classes; {len(pairs)} class objects newly named {kinds}; '
          f'{len(conflicts)} disagree with an earlier pair')


if __name__ == '__main__':
    main()
