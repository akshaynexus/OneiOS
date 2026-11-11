#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""The record of the original IOHIDDevice in the iPhone18,2 24A446 kernelcache, for OriginalHidDeviceABI.h and the
bindings of build-oneios-hid.py:

  kernel/patches/oneios-hosted/hid/layout.py [--kernelcache KC] [--prelink-report JSON] [--out FILE]

  default inputs   out/kernel-inputs/24A446/kernelcache.macho, out/kernel/24A446/kernel.kc.prelink.json
  default output   IOHIDDevice-24A446-v54.json beside this script (addresses, sizes and discriminators only: no
                   Apple code, so it is committed; the build reads it and never the kernelcache)

What is read, and from where (each item is evidence for a line of the header):

  class, size, parent, vtable, slots   kernel/prelink/classes.py: the class's static initializer (metaclass object,
                                       name string, parent metaclass, `mov w3, #size`), the getMetaClass function,
                                       the vtable whose slot 7 points at it, each slot's authenticated fixup (target,
                                       pointer-authentication discriminator)
  family                               per slot: the target lies in com.apple.iokit.IOHIDFamily's __TEXT_EXEC, so the
                                       slot is IOHIDDevice's own function (an override of IOService's, or one of its
                                       own virtuals); otherwise it is the kernel's (inherited, or __cxa_pure_virtual)
  constructor                          the one function that stores vtable + 16 into [x0]; checked to be
                                       `bti c; pacibsp; stp; mov; bl <parent constructor>`
  base_destructor                      the function between the constructor and getMetaClass; checked to be
                                       `bti c; b <parent destructor>` (the class has no member with a destructor)
  abstract                             slots 0 and 1 (the complete and deleting destructors) are `bti c; brk #1`:
                                       the class is abstract, nothing ever destroys a plain IOHIDDevice
"""
import argparse
import json
from pathlib import Path
import struct
import subprocess
import sys

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
CLASS = 'IOHIDDevice'
BUNDLE = 'com.apple.iokit.IOHIDFamily'
BTI_C, PACIBSP, BRK_1 = 0xd503245f, 0xd503237f, 0xd4200020


def fail(text):
    sys.exit('hid layout: ' + text)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--kernelcache', type=Path, default=REPO / 'out/kernel-inputs/24A446/kernelcache.macho')
    parser.add_argument('--prelink-report', type=Path, default=REPO / 'out/kernel/24A446/kernel.kc.prelink.json')
    parser.add_argument('--xnuprelink', type=Path, default=REPO / 'external/Droid-VM-UniXNU/kits/XnuPrelink')
    parser.add_argument('--out', type=Path, default=HERE / 'IOHIDDevice-24A446-v54.json')
    args = parser.parse_args()
    raw_record = args.out.with_suffix('.classes.tmp')
    subprocess.run([sys.executable, '-I', str(REPO / 'kernel/prelink/classes.py'), '--xnuprelink', str(args.xnuprelink),
                    '--kernelcache', str(args.kernelcache), '--prelink-report', str(args.prelink_report),
                    '--out', str(raw_record), f'{CLASS}@{BUNDLE}'], check=True, stdout=subprocess.DEVNULL)
    record = json.loads(raw_record.read_text())
    raw_record.unlink()
    cls = record['classes'][CLASS]
    if cls.get('vtable') is None or len(cls['stores_vtable']) != 1:
        fail(f'{CLASS}: one vtable and one constructor expected')

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
        """The target of the b (or bl) at address, or None."""
        instruction = word(address)
        if instruction >> 26 != (0x25 if link else 0x05):
            return None
        offset = instruction & 0x3ffffff
        return address + 4 * (offset - (1 << 26) if offset & 1 << 25 else offset)

    slide = int(cls['meta_class_linked'], 16) - int(cls['meta_class'], 16)
    constructor = int(cls['stores_vtable'][0]['original'], 16)
    getter = int(cls['get_meta_class'], 16)
    destructor = getter - 8
    parent_constructor = branch(constructor + 16, True)
    parent_destructor = branch(destructor + 4, False)
    if word(constructor) != BTI_C or word(constructor + 4) != PACIBSP or parent_constructor is None:
        fail(f'the function at {constructor:#x} is not `bti c; pacibsp; stp; mov; bl <parent constructor>`')
    if word(destructor) != BTI_C or parent_destructor is None or not constructor < destructor < getter:
        fail(f'no `bti c; b <parent destructor>` between the constructor and getMetaClass ({destructor:#x})')
    slots = []
    for entry in cls['slot_table']:
        target = int(entry['original'], 16)
        slots.append({'slot': entry['slot'], 'original': entry['original'], 'linked': entry['linked'],
                      'diversity': entry['diversity'], 'family': low <= target < high})
    abstract = all(word(int(slots[i]['original'], 16)) == BTI_C and word(int(slots[i]['original'], 16) + 4) == BRK_1 for i in (0, 1))
    kernel_targets = [s['original'] for s in slots if not s['family']]
    output = {
        'kernelcache_sha256': record['kernelcache_sha256'],
        'addresses': record['addresses'],
        'class': CLASS, 'bundle': cls['bundle'], 'size': cls['size'],
        'meta_class': cls['meta_class'], 'meta_class_linked': cls['meta_class_linked'],
        'parent_meta_class': cls['parent_meta_class'],
        'static_initializer_site': cls['static_initializer_site'],
        'get_meta_class': cls['get_meta_class'],
        'vtable': cls['vtable'], 'vtable_linked': cls['vtable_linked'],
        'family_text': [hex(low), hex(high)],
        'constructor': {'original': hex(constructor), 'linked': hex(constructor + slide),
                        'calls_parent_constructor': hex(parent_constructor)},
        'base_destructor': {'original': hex(destructor), 'linked': hex(destructor + slide),
                            'branches_to_parent_destructor': hex(parent_destructor)},
        'abstract': abstract,
        # a kernel target that more than one slot has, and that is not an IOService method: __cxa_pure_virtual
        'kernel_targets_repeated': sorted({t for t in kernel_targets if kernel_targets.count(t) > 1}),
        'slots': slots,
    }
    args.out.write_text(json.dumps(output, indent=1) + '\n')
    own = [s['slot'] for s in slots if s['family']]
    print(f'hid layout: {args.out.name}: {CLASS} {cls["size"]} bytes, {len(slots)} slots, {len(own)} of them the family\'s '
          f'({[s for s in own if s < 168]} below 168), constructor {constructor:#x}, base destructor {destructor:#x}, '
          f'abstract {abstract}')


if __name__ == '__main__':
    main()
