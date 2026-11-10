#!/usr/bin/env python3
"""Carry XnuPrelink's reviewed function identities of another kernelcache of the same XNU over to this one.

usage: port-fixture.py --xnuprelink DIR --kernelcache KC --fixture fileset-24A446-symbols.json --reviewed reviewed.json

XnuPrelink's fixture for 24A446 is pinned to the iPad kernelcache (kernelcache.release.j820): each reviewed function is
recorded with its size and the SHA-256 of its bytes. A function of this kernelcache's kernel with the same size and the
same bytes is the same function (a position-dependent one never has the same bytes at another address, so only
functions without PC-relative references to moved targets carry over). Exactly one such function must exist; then the
record is written into REVIEWED's "functions" with its address here, marked "ported", and replaces any earlier
ported record. Hand-written records are kept. Nothing of the fixture's data or sites carries over this way.
"""
import argparse
import hashlib
import json
from pathlib import Path
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for name in ('xnuprelink', 'kernelcache', 'fixture', 'reviewed'):
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    sys.path.insert(0, args.xnuprelink)
    from kc_image import Image
    raw = Path(args.kernelcache).read_bytes()
    reviewed = json.loads(Path(args.reviewed).read_text())
    if hashlib.sha256(raw).hexdigest() != reviewed['kernelcache_sha256']:
        raise SystemExit('port-fixture: the reviewed file is for another kernelcache')
    fixture = json.loads(Path(args.fixture).read_text())
    cache = Image(raw)
    kernel = Image(raw, cache.entries[reviewed['entry']][1])
    starts = sorted(kernel.function_starts())
    wanted = {}
    for record in fixture['functions']:
        wanted.setdefault((record['original_size'], record['original_sha256']), record)
    found = {}
    for start, end in zip(starts, starts[1:]):
        size = end - start
        if not any(key[0] == size for key in wanted):
            continue
        try:
            digest = hashlib.sha256(kernel.read(start, size)).hexdigest()
        except ValueError:
            continue
        if (size, digest) in wanted:
            found.setdefault((size, digest), []).append(start)
    kept = [r for r in reviewed.get('functions', []) if not r.get('ported')]
    by_hand = {r['original'] for r in kept}
    ported, ambiguous = [], 0
    for key, addresses in sorted(found.items(), key=lambda item: item[1]):
        if len(addresses) != 1:
            ambiguous += 1
            continue
        record, address = wanted[key], f'{addresses[0]:#x}'
        if address in by_hand:
            continue
        ported.append({
            'original': address, 'native_symbol': record['native_symbol'], 'ported': True, 'optional': True,
            'check': {'address': address, 'size': key[0], 'sha256': key[1]},
            'evidence': f'Byte-identical ({key[0]} bytes) to {record["original"]} of the iPad kernelcache '
                        f'({fixture["inputs"]["kernelcache_sha256"][:12]}), reviewed in XnuPrelink\'s '
                        f'{Path(args.fixture).name}: {record["evidence"]}'})
    reviewed['functions'] = kept + ported
    Path(args.reviewed).write_text(json.dumps(reviewed, indent=1) + '\n')
    print(f'port-fixture: {len(ported)} of {len(wanted)} reviewed functions carried over '
          f'({ambiguous} with more than one identical function here, left out)')


if __name__ == '__main__':
    main()
