#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""The record of the original IOEthernetController and IOEthernetInterface in the iPhone18,2 24A446 kernelcache, for
the bindings of build-oneios-net.py (the Wi-Fi controller and interface subclass them):

  kernel/patches/oneios-hosted/net/layout.py [--kernelcache KC] [--prelink-report JSON] [--out FILE]

  default inputs   out/kernel-inputs/24A446/kernelcache.macho, out/kernel/24A446/kernel.kc.prelink.json
  default output   IONetworking-24A446-v54.json beside this script (addresses, sizes and discriminators only: no
                   Apple code, so it is committed; the build reads it and never the kernelcache)

What is read, and from where:

  class, size, parent, vtable, slots   kernel/prelink/classes.py (the class's static initializer, getMetaClass, the
                                       vtable whose slot 7 points at it, each slot's authenticated fixup: target and
                                       pointer-authentication discriminator)
  family                               per slot: the target lies in com.apple.iokit.IONetworkingFamily's
                                       __TEXT_EXEC (a function of the family) or not (the kernel's: IOService's,
                                       OSObject's, __cxa_pure_virtual)
  constructor                          the first function that stores vtable + 16 into [x0] (classes.py gives its start), checked to be
                                       `bti c; pacibsp; stp; mov; bl <parent constructor>` (the class's
                                       Class(const OSMetaClass *))
  base_destructor                      the first `bti c; b <the family's code>` after the constructors: the class has
                                       no member with a destructor, so its destructor is a branch to its parent's
"""
import argparse
import json
from pathlib import Path
import struct
import subprocess
import sys

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
CLASSES = ['IOEthernetController', 'IOEthernetInterface']
BUNDLE = 'com.apple.iokit.IONetworkingFamily'
BTI_C, PACIBSP = 0xd503245f, 0xd503237f


def fail(text):
    sys.exit('net layout: ' + text)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--kernelcache', type=Path, default=REPO / 'out/kernel-inputs/24A446/kernelcache.macho')
    parser.add_argument('--prelink-report', type=Path, default=REPO / 'out/kernel/24A446/kernel.kc.prelink.json')
    parser.add_argument('--xnuprelink', type=Path, default=REPO / 'external/Droid-VM-UniXNU/kits/XnuPrelink')
    parser.add_argument('--out', type=Path, default=HERE / 'IONetworking-24A446-v54.json')
    args = parser.parse_args()
    raw_record = args.out.with_suffix('.classes.tmp')
    subprocess.run([sys.executable, '-I', str(REPO / 'kernel/prelink/classes.py'), '--xnuprelink', str(args.xnuprelink),
                    '--kernelcache', str(args.kernelcache), '--prelink-report', str(args.prelink_report),
                    '--out', str(raw_record), *(f'{c}@{BUNDLE}' for c in CLASSES)], check=True, stdout=subprocess.DEVNULL)
    record = json.loads(raw_record.read_text())
    raw_record.unlink()

    sys.path.insert(0, str(args.xnuprelink))
    from kc_image import Image
    raw = args.kernelcache.read_bytes()
    cache = Image(raw)
    family = Image(raw, cache.entries[BUNDLE][1])
    code = next(s for s in family.segments if s['name'] == '__TEXT_EXEC' and s['filesize'])
    low, high = code['va'], code['va'] + code['filesize']

    def word(address):
        return struct.unpack_from('<I', raw, cache.offset(address, 4))[0]

    def branch(address, link):
        instruction = word(address)
        if instruction >> 26 != (0x25 if link else 0x05):
            return None
        offset = instruction & 0x3ffffff
        return address + 4 * (offset - (1 << 26) if offset & 1 << 25 else offset)

    output = {'kernelcache_sha256': record['kernelcache_sha256'], 'addresses': record['addresses'], 'bundle': BUNDLE,
              'family_text': [hex(low), hex(high)], 'classes': {}}
    for name in CLASSES:
        cls = record['classes'][name]
        if cls.get('vtable') is None or not cls['stores_vtable']:
            fail(f'{name}: no vtable or constructor found')
        slide = int(cls['meta_class_linked'], 16) - int(cls['meta_class'], 16)
        constructor = int(cls['stores_vtable'][0]['original'], 16)
        parent_constructor = branch(constructor + 16, True)
        if word(constructor) != BTI_C or word(constructor + 4) != PACIBSP or parent_constructor is None:
            fail(f'{name}: the function at {constructor:#x} is not `bti c; pacibsp; stp; mov; bl <parent constructor>`')
        last_store = max(int(s['original'], 16) for s in cls['stores_vtable'] if int(s['original'], 16) < int(cls['get_meta_class'], 16))
        destructor = None
        for at in range(last_store, int(cls['get_meta_class'], 16), 4):
            target = branch(at + 4, False) if word(at) == BTI_C else None
            if target is not None and low <= target < high:
                destructor, parent_destructor = at, target
                break
        if destructor is None:
            fail(f'{name}: no `bti c; b <parent destructor>` between its constructors and getMetaClass')
        slots = [{'slot': e['slot'], 'original': e['original'], 'linked': e['linked'], 'diversity': e['diversity'],
                  'family': low <= int(e['original'], 16) < high} for e in cls['slot_table']]
        output['classes'][name] = {
            'size': cls['size'], 'meta_class': cls['meta_class'], 'meta_class_linked': cls['meta_class_linked'],
            'parent_meta_class': cls['parent_meta_class'], 'get_meta_class': cls['get_meta_class'],
            'vtable': cls['vtable'], 'vtable_linked': cls['vtable_linked'],
            'constructor': {'original': hex(constructor), 'linked': hex(constructor + slide), 'calls_parent_constructor': hex(parent_constructor)},
            'base_destructor': {'original': hex(destructor), 'linked': hex(destructor + slide), 'branches_to_parent_destructor': hex(parent_destructor)},
            'slots': slots,
        }
        print(f'net layout: {name} {cls["size"]} bytes, {len(slots)} slots ({sum(s["family"] for s in slots)} the family\'s), '
              f'constructor {constructor:#x}, base destructor {destructor:#x}')
    args.out.write_text(json.dumps(output, indent=1) + '\n')


if __name__ == '__main__':
    main()
