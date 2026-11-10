#!/usr/bin/env python3
"""Emit kernel.kc: XnuPrelink's fileset emitter with OneiOS's roots and reviewed pins.

usage: emit.py --xnuprelink DIR --kernelcache KC --original-kernelcache KC0 --native NATIVE --map MAP
               --roots select.json --reviewed reviewed.json --output kernel.kc --report kernel.kc.prelink.json

XnuPrelink (external/XnuPrelink at the pin of kernel/device/board.toml) pairs the original kernel's functions and
data with the native kernel's (MAP, fileset_match.py) and rebinds the kexts onto the native kernel. Its reviewed
fixtures are pinned to the iPad kernelcache; this driver adds OneiOS's for the iPhone one (reviewed-*.json) without
changing that checkout:
  functions, data   written into the pairing ('how': 'reviewed'), the native side by symbol;
  (--aligned)       align.py's pairs, inferred from link order ('how': 'aligned'); a reviewed record wins;
  sites             one original pointer site bound to a named native object (fileset_emit.reviewed_sites);
  early_security_init  the original routine OSKext::start calls after the component starts.
KC is the kernelcache the emitter reads (hosted.py's output, or the original when nothing was rewritten); KC0 is the
original, which the reviewed records are pinned to and checked against. A reference whose target stays unpaired is
bound to the kernel's stub _unixnu_fileset_unmapped (a panic that names the caller) and listed in the report.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for name in ('xnuprelink', 'kernelcache', 'original-kernelcache', 'native', 'map', 'roots', 'reviewed', 'output', 'report'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--aligned', action='append', default=[], help='align.py output: pairs inferred from link order')
    args = parser.parse_args()
    sys.path.insert(0, args.xnuprelink)
    import fileset_emit
    from kc_image import Image

    raw = Path(args.kernelcache).read_bytes()
    original_raw = Path(args.original_kernelcache).read_bytes()
    native_data = Path(args.native).read_bytes()
    payload = json.loads(Path(args.map).read_text())
    reviewed = json.loads(Path(args.reviewed).read_text())
    roots = json.loads(Path(args.roots).read_text())['roots']
    if hashlib.sha256(original_raw).hexdigest() != reviewed['kernelcache_sha256']:
        raise SystemExit('emit: the reviewed records are for another kernelcache')
    original = Image(original_raw)

    def check(record):
        if not record.get('evidence'):
            raise SystemExit(f'emit: a reviewed record needs its evidence: {record}')
        pin = record.get('check')
        if pin:
            found = hashlib.sha256(original.read(int(pin['address'], 16), pin['size'])).hexdigest()
            if found != pin['sha256']:
                raise SystemExit(f'emit: the original bytes at {pin["address"]} are not the reviewed ones')

    names = {}
    for address, symbols in Image(native_data).symbols().items():
        for name in symbols:
            names.setdefault(name, address)
    applied = {'functions': [], 'functions_absent': [], 'data': [], 'data_inferred': [], 'sites': [], 'aligned': []}
    for path in args.aligned:
        for pair in json.loads(Path(path).read_text()).get('data', []):      # kclasses.py: class objects by name
            if pair['original'] in payload['data'] or pair['native_symbol'] not in names:
                continue
            payload['data'][pair['original']] = {'native': hex(names[pair['native_symbol']]), 'name': pair['native_symbol'],
                                                  'how': 'class', 'sections': ['?', '?']}
            applied['data_inferred'].append({'original': pair['original'], 'native_symbol': pair['native_symbol']})
        for pair in json.loads(Path(path).read_text()).get('pairs', []):
            if pair['original'] in payload['functions'] or pair['native_symbol'] not in names:
                continue
            payload['functions'][pair['original']] = {'native': hex(names[pair['native_symbol']]),
                                                       'name': pair['native_symbol'], 'addend': 0, 'how': 'aligned'}
            applied['aligned'].append({'original': pair['original'], 'native_symbol': pair['native_symbol'],
                                       'score': pair['score']})
    for record in reviewed.get('functions', []):
        check(record)
        if record['native_symbol'] not in names:
            if record.get('optional'):          # a carried-over identity of a function this configuration lacks
                applied['functions_absent'].append(record['native_symbol'])
                continue
            raise SystemExit(f'emit: {record["native_symbol"]} is not in this kernel')
        previous = payload['functions'].get(record['original'])
        payload['functions'][record['original']] = {'native': hex(names[record['native_symbol']]),
                                                     'name': record['native_symbol'], 'addend': 0, 'how': 'reviewed'}
        applied['functions'].append({'original': record['original'], 'native_symbol': record['native_symbol'],
                                     'replaced': previous.get('name') if previous else None})
    for record in reviewed.get('data', []):
        check(record)
        if record['native_symbol'] not in names:
            raise SystemExit(f'emit: {record["native_symbol"]} is not in this kernel')
        payload['data'][record['original']] = {'native': hex(names[record['native_symbol']]), 'name': record['native_symbol'],
                                                'how': 'reviewed', 'sections': record.get('sections', ['?', '?'])}
        applied['data'].append({'original': record['original'], 'native_symbol': record['native_symbol']})

    # XnuPrelink puts the kernel's __DATA where Apple's kernel had its own, in front of the first kept kext's data.
    # Ours can be larger than that room (the virtio-gpu driver alone has 9 MB of static buffers) and more kexts are
    # kept here than on UniXNU's boards. When it does not fit, it goes to the end of the collection as a root
    # segment of its own (__DATA_KERNEL), like __KLDDATA and __BOOTDATA; the empty __PRELINK_DATA marker stays at
    # the start of the collection's __DATA, which arm_vm_init reads as "kext data from here to the end of __DATA".
    import fileset_layout
    upstream_validate, upstream_segment = fileset_layout.Layout.validate, fileset_layout.Layout.segment
    moved = {}

    def segment(self, name):
        return upstream_segment(self, '__DATA' if name == '__DATA_KERNEL' else name)

    def validate(self):
        data = upstream_segment(self, '__DATA')
        begin = self.placements['__DATA']
        if any(begin < e and b < begin + data['size'] for b, e in self.occupied):
            room = min(b for b, e in self.occupied if b >= begin) - begin
            address = self.allocate(data['size'])
            self.reserve('__DATA', address)
            self.reserve('__PRELINK_DATA', self.roots['__DATA']['va'])
            self.roots['__DATA_KERNEL'] = dict(data, name='__DATA_KERNEL', va=address,
                                               fileoff=address - self.cache.base, filesize=data['size'])
            self.size = self.cursor
            moved.update(size=hex(data['size']), room=hex(room), address=hex(address))
        upstream_validate(self)
    fileset_layout.Layout.validate, fileset_layout.Layout.segment = validate, segment

    upstream_sites = fileset_emit.reviewed_sites

    def sites(inputs, native, collection_slide):
        found = upstream_sites(inputs, native, collection_slide)
        for record in reviewed.get('sites', []):
            check(record)
            if record['native_symbol'] not in names:
                raise SystemExit(f'emit: {record["native_symbol"]} is not in this kernel')
            found[(int(record['site'], 16) + collection_slide) & fileset_emit.MASK64] = names[record['native_symbol']]
            applied['sites'].append({'site': record['site'], 'native_symbol': record['native_symbol']})
        return found
    fileset_emit.reviewed_sites = sites

    early = {}
    record = reviewed.get('early_security_init')
    if record:
        check(record)
        early[record['bundle']] = int(record['original'], 16)

    image, report = fileset_emit.emit_images(raw, native_data, payload, roots, '_unixnu_fileset_unmapped', early)
    # The collection's header keeps Apple's CPU subtype (0xc0000002: arm64e, kernel ABI version bit); its kernel is
    # now ours (0x80000002). The loader (runtime/loader/src/macho.c) wants the collection and its kernel to agree,
    # and XNU does not read the collection's subtype, so the header takes the native kernel's.
    image = bytearray(image)
    collection_subtype, = struct.unpack_from('<I', image, 8)
    native_subtype, = struct.unpack_from('<I', native_data, 8)
    struct.pack_into('<I', image, 8, native_subtype)
    image = bytes(image)
    report.update(candidate_sha256=hashlib.sha256(image).hexdigest(),
                  collection_cpusubtype={'original': hex(collection_subtype), 'written': hex(native_subtype)})
    stubbed = {}
    for owner, targets in report['stubbed'].items():
        for target in targets:
            stubbed.setdefault(target, []).append(owner)
    report.update(kernelcache=str(Path(args.kernelcache).resolve()), native=str(Path(args.native).resolve()),
                  map=str(Path(args.map).resolve()), roots=roots, reviewed=str(Path(args.reviewed).resolve()),
                  reviewed_applied=applied, kernel_data_moved=moved or None, original_kernelcache_sha256=reviewed['kernelcache_sha256'],
                  stubbed_functions=len(stubbed),
                  missing_data=len({t for targets in report['missing'].values() for t in targets}))
    fileset_emit.write_candidate(image, report, args.output, args.report, allow_missing=True)


if __name__ == '__main__':
    main()
