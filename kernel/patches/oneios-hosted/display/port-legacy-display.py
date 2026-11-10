#!/usr/bin/env python3
"""Carry Droid-VM's reviewed layout of the original IOMobileFramebufferLegacy (iPad17,3, 24A446) over to the
iPhone18,2 kernelcache of the same build, and check every carried number against this kernelcache's bytes.

usage: port-legacy-display.py --xnuprelink external/XnuPrelink --kernelcache out/kernel-inputs/24A446/kernelcache.macho
           [--fixture external/UniXNU-droidvm/ipados27/scripts/legacy-display-24A446.json]
           [--classes kernel/prelink/classes-24A446-v54.json] [--out legacy-display-24A446-v54.json]

Droid-VM's fixture (legacy-display-24A446.json) records, for the iPad kernelcache: the class's 379 vtable slots with
the pointer-authentication diversity of each, the names under which the owned subclass declares them (a name whose
discriminator is the slot's diversity), which slots the backend overrides, the fields of the display object and of
a swap request it reads, and the original functions it calls. kernel/prelink/classes.py already reads the same
class from the iPhone kernelcache (metaclass, size, vtable, slots, diversities, constructor). What is left is to
show that the family's code is the same code at another address, so that the reviewed meanings carry over:

  slots      every slot whose target is inside IOMobileGraphicsFamily's text sits at one constant distance from the
             iPad's (the kext was laid out identically); every other slot is the kernel's and is taken from this
             kernelcache. Diversities must be equal slot by slot, except the pure-virtual slots: their table entry is
             __cxa_pure_virtual under some diversity, and what matters is the diversity original callers authenticate
             with (the fixture's, evidence there), which must appear as `movk xN, #d, lsl #48` in this family's text.
  bindings   the family's own functions move by that distance; IOSurface's by the one distance at which all eight of
             the fixture's byte checks hold in this kernelcache's IOSurface text (exactly one may exist).
  fields     unchanged (same code); the two instructions the fixture checks them by are read back here.

The output records each byte check as its address, length and SHA-256 ("checks"), not the bytes.

The output holds addresses, offsets and names only (no Apple code) and is the input of build-oneios-display.py.
Rerun when the kernelcache changes; it fails rather than guess.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import sys

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
CLASS = 'IOMobileFramebufferLegacy'
FAMILY, SURFACE = 'com.apple.iokit.IOMobileGraphicsFamily', 'com.apple.iokit.IOSurface'
FAMILY_BINDINGS = ('_native_legacy_vsync_notify', '_native_legacy_set_refresh_properties')
SURFACE_BINDINGS = ('_native_legacy_surface_descriptor', '_native_legacy_surface_width', '_native_legacy_surface_height',
                    '_native_legacy_surface_stride', '_native_legacy_surface_format', '_native_legacy_surface_planes',
                    '_native_legacy_surface_create', '_native_legacy_surface_allocate')


def fail(text):
    sys.exit('port-legacy-display: ' + text)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--xnuprelink', default=str(REPO / 'external/XnuPrelink'))
    parser.add_argument('--kernelcache', default=str(REPO / 'out/kernel-inputs/24A446/kernelcache.macho'))
    parser.add_argument('--fixture', default=str(REPO / 'external/UniXNU-droidvm/ipados27/scripts/legacy-display-24A446.json'))
    parser.add_argument('--classes', default=str(REPO / 'kernel/prelink/classes-24A446-v54.json'))
    parser.add_argument('--out', default=str(HERE / 'legacy-display-24A446-v54.json'))
    args = parser.parse_args()
    sys.path.insert(0, args.xnuprelink)
    from kc_image import Image
    raw = Path(args.kernelcache).read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    record = json.loads(Path(args.classes).read_text())
    if record['kernelcache_sha256'] != digest:
        fail(f'{args.classes} is for another kernelcache')
    ipad = json.loads(Path(args.fixture).read_text())
    cls = record['classes'].get(CLASS) or fail(f'{args.classes} has no {CLASS}')
    if cls['bundle'] != FAMILY or cls['size'] != ipad['instance_size'] or cls['slots'] != len(ipad['slots']):
        fail(f'{CLASS} here is {cls["size"]} bytes with {cls["slots"]} slots; the fixture describes {ipad["instance_size"]} and {len(ipad["slots"])}')
    cache = Image(raw)
    text = {}
    for bundle in (FAMILY, SURFACE):
        if bundle not in cache.entries:
            fail(f'{bundle} is not in this kernelcache')
        segment = next(s for s in Image(raw, cache.entries[bundle][1]).segments if s['name'] == '__TEXT_EXEC')
        text[bundle] = (segment['va'], segment['va'] + segment['size'])

    def read(address, size):
        at = cache.offset(address, size)
        return raw[at:at + size]

    def inside(address, bundle):
        return text[bundle][0] <= address < text[bundle][1]

    # ---- slots
    here = [(int(e['original'], 16), int(e['diversity'], 16)) for e in sorted(cls['slot_table'], key=lambda e: e['slot'])]
    there = [(int(t, 16), int(d, 16)) for t, d in ipad['slots']]
    moves = {h[0] - t[0] for h, t in zip(here, there) if inside(h[0], FAMILY)}
    if len(moves) != 1:
        fail(f'the family\'s slots are not at one distance from the iPad\'s: {sorted(map(hex, moves))}')
    move = moves.pop()
    pure_here = {i for i, h in enumerate(here) if h[0] == here[214][0]}
    pure_there = {i for i, t in enumerate(there) if t[0] == there[214][0]}
    if pure_here != pure_there:
        fail(f'the pure-virtual slots differ: {sorted(pure_here ^ pure_there)}')
    family_code = read(text[FAMILY][0], text[FAMILY][1] - text[FAMILY][0])
    words = set(struct.unpack_from(f'<{len(family_code) // 4}I', family_code))

    def authenticated(diversity):                 # movk xN, #diversity, lsl #48
        return any(0xf2e00000 | diversity << 5 | reg in words for reg in range(31))

    slots, callers = [], {}
    for i, (h, t) in enumerate(zip(here, there)):
        if inside(t[0] + move, FAMILY) != inside(h[0], FAMILY) or (inside(h[0], FAMILY) and h[0] != t[0] + move):
            fail(f'slot {i}: {h[0]:#x} here, {t[0]:#x} on the iPad: not the same function of the family')
        diversity = h[1]
        if i in pure_here:
            diversity = t[1]                       # the callers', as the fixture has it
            if h[1] != t[1]:
                if not authenticated(t[1]):
                    fail(f'slot {i} is pure and no call in the family authenticates with the fixture\'s {t[1]:#x}')
                callers[str(i)] = {'diversity': f'{t[1]:#x}', 'table_entry': f'{h[1]:#x}',
                                   'evidence': 'pure slot: the table entry is __cxa_pure_virtual; the family\'s text has a '
                                               f'movk xN, #{t[1]:#x}, lsl #48 (the call sites\' discriminator, as the iPad fixture records it)'
                                               + ('' if authenticated(h[1]) else f' and none with the table entry\'s {h[1]:#x}')}
        elif h[1] != t[1]:
            fail(f'slot {i}: diversity {h[1]:#x} here, {t[1]:#x} on the iPad')
        slots.append([f'{h[0]:#x}', f'{diversity:#x}'])

    # ---- bindings
    constructor = [int(e['original'], 16) for e in cls['stores_vtable']]
    if len(constructor) != 1 or constructor[0] != int(ipad['bindings']['__ZN25IOMobileFramebufferLegacyC2EPK11OSMetaClass'], 16) + move:
        fail('the constructor that stores the vtable is not the iPad\'s moved')
    bindings = {
        '__ZN25IOMobileFramebufferLegacyC2EPK11OSMetaClass': constructor[0],
        '__ZN25IOMobileFramebufferLegacyD2Ev': int(ipad['bindings']['__ZN25IOMobileFramebufferLegacyD2Ev'], 16) + move,
        '__ZN25IOMobileFramebufferLegacy10gMetaClassE': int(cls['meta_class'], 16),
        '__ZTV25IOMobileFramebufferLegacy': int(cls['vtable'], 16) - 16,
    }
    if int(ipad['vtable'], 16) - int(ipad['bindings']['__ZTV25IOMobileFramebufferLegacy'], 16) != 16:
        fail('the fixture\'s vtable symbol is not 16 bytes before slot 0')
    for name in FAMILY_BINDINGS:
        bindings[name] = int(ipad['bindings'][name], 16) + move
        if not inside(bindings[name], FAMILY) or read(bindings[name], 4) != bytes.fromhex('5f2403d5'):
            fail(f'{name}: no function entry (bti c) at {bindings[name]:#x}')
    checks = {int(a, 16): bytes.fromhex(b) for a, b in ipad['checks']}
    surface_checks = {a: b for a, b in checks.items() if a in {int(ipad['bindings'][n], 16) for n in SURFACE_BINDINGS}}
    if len(surface_checks) != len(SURFACE_BINDINGS):
        fail('the fixture does not check every IOSurface binding')
    anchor, pattern = next(iter(surface_checks.items()))
    surface_code = read(text[SURFACE][0], text[SURFACE][1] - text[SURFACE][0])
    surface_moves, at = [], surface_code.find(pattern)
    while at >= 0:
        candidate = text[SURFACE][0] + at - anchor
        if at % 4 == 0 and all(inside(a + candidate, SURFACE) and read(a + candidate, len(b)) == b for a, b in surface_checks.items()):
            surface_moves.append(candidate)
        at = surface_code.find(pattern, at + 4)
    if len(surface_moves) != 1:
        fail(f'{len(surface_moves)} placements of IOSurface\'s eight functions fit the fixture\'s checks (one must)')
    for name in SURFACE_BINDINGS:
        bindings[name] = int(ipad['bindings'][name], 16) + surface_moves[0]
    moved_checks = []
    for address, expected in checks.items():
        address += surface_moves[0] if address in surface_checks else move
        if read(address, len(expected)) != expected:
            fail(f'check at {address:#x}: {read(address, len(expected)).hex()}, expected {expected.hex()}')
        # the place, the length and a hash: the output carries no instruction bytes
        moved_checks.append([f'{address:#x}', len(expected), hashlib.sha256(expected).hexdigest()])
    if read(bindings['__ZN25IOMobileFramebufferLegacyD2Ev'], 4) != bytes.fromhex('5f2403d5'):
        fail('no function entry at the base destructor')
    slide = int(cls['meta_class_linked'], 16) - int(cls['meta_class'], 16)
    out = {
        'about': 'The original IOMobileFramebufferLegacy of the iPhone18,2 24A446 kernelcache for the owned display backend '
                 '(NativeMobileDisplay): Droid-VM\'s reviewed iPad fixture carried over and checked by port-legacy-display.py. '
                 'Addresses are original kernelcache VAs; build-oneios-display.py adds the collection slide.',
        'kernelcache_sha256': digest,
        'ported_from': {'fixture': Path(args.fixture).name, 'kernelcache_sha256': ipad['kernelcache_sha256'],
                        'family_move': f'{move:#x}' if move >= 0 else f'-{-move:#x}',
                        'surface_move': f'{surface_moves[0]:#x}' if surface_moves[0] >= 0 else f'-{-surface_moves[0]:#x}',
                        'family_slots_at_that_distance': sum(inside(h[0], FAMILY) for h in here)},
        'collection_slide': f'{slide:#x}',
        'family_text': [f'{text[FAMILY][0]:#x}', f'{text[FAMILY][1]:#x}'],
        'vtable': cls['vtable'],
        'instance_size': cls['size'],
        'slots': slots,
        'methods': ipad['methods'],
        'leaves': ipad['leaves'],
        'bindings': {name: f'{address:#x}' for name, address in bindings.items()},
        'checks': moved_checks,
        'display': ipad['display'],
        'request': ipad['request'],
        'names': ipad['names'],
        'caller_diversity': callers,
    }
    Path(args.out).write_text(json.dumps(out, indent=1) + '\n')
    print(f'port-legacy-display: {args.out}: {len(slots)} slots ({out["ported_from"]["family_slots_at_that_distance"]} of the family, moved '
          f'{out["ported_from"]["family_move"]}), IOSurface moved {out["ported_from"]["surface_move"]}, {len(moved_checks)} byte checks hold, '
          f'{len(callers)} pure slots on the callers\' diversity')


if __name__ == '__main__':
    main()
