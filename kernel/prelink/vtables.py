#!/usr/bin/env python3
"""Pair the virtual methods of kernel classes slot by slot, for every vtable that is paired by name.

usage: vtables.py --xnuprelink DIR --kernelcache KC --native NATIVE --map MAP --reviewed reviewed.json
                  [--also pairs.json ...] --out vtables.json

A kext's subclass inherits the kernel class's methods through its own vtable, and the kernel calls them there
(IORegistryEntry::init calls getProperty through the object's vtable). The public classes' vtable layouts are ABI:
slot n of the original __ZTV<X> and slot n of ours are the same method. For each vtable paired by name (the pairing's
data and the reviewed file) the slots are walked while both sides have a pointer; a slot whose original function is
unpaired takes the name of our function in that slot.

A vtable is used only when the slots that are already paired agree: at least five agreeing slots, and at most one
in ten disagreeing (a disagreement is either a wrong earlier pair or a layout that differs: both are listed in the
output under "conflicts", for review; an order-inferred pair (--weak) gives way to the slot, any other stands:
the original folds identical functions, so a trivial method often carries another function's name with the same code). Pure-virtual and deleted slots (our ___cxa_pure_virtual) are skipped.
Output: pairs in align.py's format ('by': 'slot').
"""
import argparse
import hashlib
import json
from pathlib import Path
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for name in ('xnuprelink', 'kernelcache', 'native', 'map', 'reviewed', 'out'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--also', action='append', default=[], help='earlier pairs that stand (structors.py)')
    parser.add_argument('--weak', action='append', default=[], help='earlier inferred pairs (align.py) that a slot overrides')
    args = parser.parse_args()
    sys.path.insert(0, args.xnuprelink)
    from kc_image import Image
    raw = Path(args.kernelcache).read_bytes()
    pairing = json.loads(Path(args.map).read_text())
    reviewed = json.loads(Path(args.reviewed).read_text())
    cache = Image(raw)
    kernel = Image(raw, cache.entries['com.apple.kernel'][1])
    code = [(s['va'], s['va'] + s['size']) for s in kernel.segments if 'EXEC' in s['name']]
    fixups = cache.fixups()
    native = Image(Path(args.native).read_bytes())
    symbols = native.symbols()
    native_fixups = native.fixups()
    by_name, count = {}, {}
    for address, names in symbols.items():
        for name in names:
            by_name[name] = address
            count[name] = count.get(name, 0) + 1
    name_of = {int(k, 16): v['name'] for k, v in pairing['functions'].items() if v.get('name')}
    for path in args.also:
        for pair in json.loads(Path(path).read_text())['pairs']:
            name_of.setdefault(int(pair['original'], 16), pair['native_symbol'])
    weak = {}
    for path in args.weak:
        for pair in json.loads(Path(path).read_text())['pairs']:
            if int(pair['original'], 16) not in name_of:
                weak[int(pair['original'], 16)] = pair['native_symbol']
    name_of.update(weak)
    for record in reviewed.get('functions', []):
        name_of[int(record['original'], 16)] = record['native_symbol']
        weak.pop(int(record['original'], 16), None)
    vtables = {int(k, 16): v['name'] for k, v in pairing['data'].items() if (v.get('name') or '').startswith('__ZTV')}
    vtables.update({int(r['original'], 16): r['native_symbol'] for r in reviewed.get('data', [])
                    if r['native_symbol'].startswith('__ZTV')})
    pairs, conflicts, used = [], [], 0
    for original, name in sorted(vtables.items()):
        if count.get(name) != 1:
            continue
        ours = by_name[name]
        slots = []
        for slot in range(4096):
            left = fixups.get(original + 16 + 8 * slot)
            right = native_fixups.get(ours + 16 + 8 * slot)
            if not left or not right or not any(b <= left[0] < e for b, e in code):
                break
            slots.append((slot, left[0], sorted(symbols.get(right[0], ()))))
        agree = [s for s in slots if s[1] in name_of and name_of[s[1]] in s[2]]
        differ = [s for s in slots if s[1] in name_of and s[2] and name_of[s[1]] not in s[2]]
        firm = [s for s in differ if s[1] not in weak]          # an order-inferred pair is no evidence against a slot
        usable_vtable = len(agree) >= 5 and len(firm) * 10 <= len(agree)
        for slot, target, names in differ:
            conflicts.append({'vtable': name, 'slot': slot, 'original': hex(target), 'paired': name_of[target],
                              'slot_says': names[0],
                              'overridden': usable_vtable and target in weak and weak[target] == name_of[target]})
        if not usable_vtable:
            continue
        used += 1
        for slot, target, names in slots:
            if target in name_of and not (target in weak and name_of[target] not in names):
                continue
            # No "name already used" test here: the original has a second copy of some vtables (IOService's) whose
            # entries are thunks to the same methods, so two original addresses rightly share one name.
            usable = [n for n in names if count.get(n) == 1 and 'pure_virtual' not in n]
            if not usable:
                continue
            name_of[target] = usable[0]
            pairs.append({'original': hex(target), 'native_symbol': usable[0], 'score': 3.0, 'by': 'slot',
                          'shared_features': len(agree), 'features': len(slots), 'between': [name, f'slot {slot}']})
    record = {'kernelcache_sha256': hashlib.sha256(raw).hexdigest(), 'vtables_named': len(vtables),
              'vtables_used': used, 'pairs': pairs, 'conflicts': conflicts}
    Path(args.out).write_text(json.dumps(record, indent=1) + '\n')
    print(f'vtables: {len(pairs)} methods paired by slot in {used} of {len(vtables)} named vtables; '
          f'{len(conflicts)} slots disagree with an earlier pair')


if __name__ == '__main__':
    main()
