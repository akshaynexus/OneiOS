#!/usr/bin/env python3
"""The layout of original (Apple) classes in the kernelcache, for board drivers that subclass them.

usage: classes.py --xnuprelink DIR --kernelcache KC [--prelink-report kernel.kc.prelink.json] --out classes.json CLASS...

A board driver that subclasses a class of a kept kext (Droid-VM's NativeAES under IOAESAccelerator, the display
under IOMobileFramebufferLegacy, the HID device under IOHIDDevice, NativeAdreno under the AGX accelerator) is
linked into the kernel and needs the class's addresses in the collection: its metaclass object, instance size,
parent, vtable and slots, constructor and destructors. Kext entries carry no symbols, so each is read from code:

  metaclass, size, parent   the class's static initializer: x0 = the metaclass object, x1 = the class name string,
                            x2 = the parent's metaclass, w3 = the instance size, then the OSMetaClass constructor
  getMetaClass              the function that returns the metaclass object (adrp/add x0; ret)
  vtable                    the table whose slot 7 points at getMetaClass (slot 7 of every OSObject vtable)
  slots                     its entries while they are pointers; slot 0 and 1 are the complete and deleting destructors
  constructors              functions that store vtable + 16 into [x0]

Addresses are original kernelcache VAs; with --prelink-report each also gets its address as linked in kernel.kc
(original + collection slide). These are addresses and names, no Apple code: the output may be committed.
A class is looked for in every kext entry; give CLASS as Name or Name@bundle.id.

How a board archive reaches them: a native stub `bti c; b <target>` with an N_ABS symbol `<stub>.original` at the
linked address (XnuPrelink's emitter recomputes the branch after it places the kernel, README there). An absolute
address in a literal, as Droid-VM's SoftwareAES-24A446.S has, is wrong under xnuhost: the collection is slid.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import sys

RET = 0xd65f03c0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--xnuprelink', required=True)
    parser.add_argument('--kernelcache', required=True)
    parser.add_argument('--prelink-report')
    parser.add_argument('--out', required=True)
    parser.add_argument('classes', nargs='+', metavar='CLASS')
    args = parser.parse_args()
    sys.path.insert(0, args.xnuprelink)
    from kc_image import Image, scan
    raw = Path(args.kernelcache).read_bytes()
    cache = Image(raw)
    fixups = cache.fixups()
    slide = int(json.loads(Path(args.prelink_report).read_text())['collection_slide'], 16) if args.prelink_report else None
    linked = (lambda a: hex((a + slide) & (1 << 64) - 1)) if slide is not None else (lambda a: None)
    entries = {name: Image(raw, at) for name, (_, at) in cache.entries.items()}

    def word(address):
        return struct.unpack_from('<I', raw, cache.offset(address, 4))[0]

    def find(name, bundle):
        needle = name.encode() + b'\0'
        for bundle_id, image in entries.items():
            if bundle and bundle_id != bundle:
                continue
            code = next((s for s in image.segments if s['name'] == '__TEXT_EXEC' and s['filesize']), None)
            text = [s for s in image.segments if s['name'] in ('__TEXT', '__DATA_CONST') and s['filesize']]
            if not code:
                continue
            strings = set()
            for segment in text:
                blob = raw[segment['fileoff']:segment['fileoff'] + segment['filesize']]
                at = blob.find(needle)
                while at >= 0:
                    if at == 0 or blob[at - 1] == 0:
                        strings.add(segment['va'] + at)
                    at = blob.find(needle, at + 1)
            if not strings:
                continue
            blob = raw[code['fileoff']:code['fileoff'] + code['filesize']]
            _, refs, _ = scan(blob, code['va'], with_origins=True)
            by_site = {here: (target, kind) for here, target, kind, *_ in refs}
            for here, target, kind, *_ in refs:
                if target not in strings or kind != 'add' or word(here) & 31 != 1:
                    continue
                # the static initializer sets x0 just before; the MetaClass constructors take x0 from the caller
                meta = parent = size = None
                for back in range(here - 8, here - 28, -4):
                    if back in by_site and word(back) & 31 == 0 and by_site[back][1] == 'add':
                        meta = by_site[back][0]
                        break
                if meta is None:
                    continue
                for forward in range(here + 4, here + 32, 4):
                    instruction = word(forward)
                    if forward in by_site and instruction & 31 == 2:
                        parent, how = by_site[forward]
                        if how == 'ldst':                       # through the kext's GOT
                            parent = fixups[parent][0] if parent in fixups else None
                    if instruction & 0xffe0001f == 0x52800003:      # mov w3, #size
                        size = instruction >> 5 & 0xffff
                    if instruction >> 26 == 0x25:                   # bl: the OSMetaClass constructor
                        break
                return bundle_id, image, code, refs, meta, parent, size, here
        return None

    result = {}
    for item in args.classes:
        name, _, bundle = item.partition('@')
        found = find(name, bundle)
        if not found:
            result[name] = {'error': 'no static initializer with this class name found'}
            continue
        bundle_id, image, code, refs, meta, parent, size, site = found
        starts = sorted(s for s in image.function_starts() if code['va'] <= s < code['va'] + code['size'])
        record = {'bundle': bundle_id, 'size': hex(size) if size is not None else None,
                  'meta_class': hex(meta), 'meta_class_linked': linked(meta),
                  'parent_meta_class': hex(parent) if parent else None,
                  'static_initializer_site': hex(site)}
        getter = None
        for here, target, kind, *_ in refs:
            if target == meta and kind == 'add' and word(here) & 31 == 0 and word(here + 4) == RET:
                getter = here - 4
                if word(getter - 4) == 0xd503245f:                  # bti c
                    getter -= 4
                break
        record['get_meta_class'] = hex(getter) if getter else None
        vtables = []
        if getter:
            for slot, fix in fixups.items():
                if fix[0] == getter:
                    base = slot - 16 - 7 * 8
                    if base not in fixups and base + 8 not in fixups and base + 16 in fixups:
                        vtables.append(base)
        if len(vtables) == 1:
            vtable = vtables[0]
            slots, at = [], vtable + 16
            while at in fixups:
                target, auth, key, diversity, address_diversity = fixups[at]
                slots.append({'slot': len(slots), 'original': hex(target), 'linked': linked(target),
                              'diversity': hex(diversity) if diversity is not None else None})
                at += 8
            record.update(vtable=hex(vtable), vtable_linked=linked(vtable), slots=len(slots),
                          destructor_complete=slots[0]['original'] if slots else None,
                          destructor_deleting=slots[1]['original'] if len(slots) > 1 else None,
                          slot_table=slots)
            constructors = []
            for here, target, kind, *_ in refs:
                register = word(here) & 31
                if target == vtable and kind == 'add' and word(here + 4) == 0x91004000 | register << 5 | register:   # add xN, xN, #16
                    start = max((s for s in starts if s <= here), default=None)
                    if start and start not in constructors:
                        constructors.append(start)
            record['stores_vtable'] = [{'original': hex(c), 'linked': linked(c)} for c in sorted(constructors)]
        else:
            record['vtable'] = None
            record['vtable_candidates'] = [hex(v) for v in vtables]
        result[name] = record
    output = {'kernelcache_sha256': hashlib.sha256(raw).hexdigest(),
              'addresses': 'original kernelcache VAs' + ('; "linked" = as placed in kernel.kc (original + collection slide '
                                                         + hex(slide) + ')' if slide is not None else ''),
              'classes': result}
    Path(args.out).write_text(json.dumps(output, indent=1) + '\n')
    for name, record in result.items():
        if 'error' in record:
            print(f'classes: {name}: {record["error"]}')
        else:
            print(f'classes: {name} in {record["bundle"]}: size {record["size"]}, metaclass {record["meta_class"]}, '
                  f'vtable {record.get("vtable")}, {record.get("slots", 0)} slots, '
                  f'{len(record.get("stores_vtable", []))} function(s) store the vtable')


if __name__ == '__main__':
    main()
