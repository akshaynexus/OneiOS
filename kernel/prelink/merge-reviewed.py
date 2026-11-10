#!/usr/bin/env python3
"""Merge the other agents' reviewed function records into the owner's reviewed file for one build.

usage: merge-reviewed.py --reviewed OWNER.json --native native.macho --out merged.json EXTRA.json...

kernel/prelink/reviewed-*.json is fable-prelink's; the lead and fable-user keep their own records beside it
(lead/reviewed-lead-*.json, user/reviewed-user-*.json: functions only, each with its evidence and byte check).
The owner's record wins for an original both have; a disagreement is printed and listed in the output under
`merge.disagreements`, so it gets looked at instead of silently decided. A record whose native symbol the kernel
does not define is left out and listed (`merge.no_symbol`). EXTRA files are taken in the order given.
"""
import argparse
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--reviewed', required=True)
    parser.add_argument('--native', required=True)
    parser.add_argument('--out', required=True)
    parser.add_argument('extra', nargs='*')
    args = parser.parse_args()
    merged = json.loads(Path(args.reviewed).read_text())
    defined = {line.split()[2] for line in subprocess.run(['llvm-nm', '--defined-only', args.native], capture_output=True,
                                                          text=True, check=True).stdout.splitlines() if len(line.split()) == 3}
    have = {record['original']: record['native_symbol'] for record in merged['functions']}
    added, disagreements, missing = {}, [], []
    for path in args.extra:
        source = str(Path(path).name)
        count = 0
        for record in json.loads(Path(path).read_text()).get('functions', []):
            original = hex(int(record['original'], 16))
            if original in have:
                if have[original] != record['native_symbol']:
                    disagreements.append({'original': original, 'kept': have[original], 'other': record['native_symbol'],
                                          'from': source})
                continue
            if record['native_symbol'] not in defined:
                missing.append({'original': original, 'native_symbol': record['native_symbol'], 'from': source})
                continue
            merged['functions'].append(dict(record, original=original, source=source))
            have[original] = record['native_symbol']
            count += 1
        added[source] = count
    merged['merge'] = {'added': added, 'disagreements': disagreements, 'no_symbol': missing}
    Path(args.out).write_text(json.dumps(merged, indent=1) + '\n')
    print(f'merge-reviewed: {sum(added.values())} record(s) added from {len(args.extra)} file(s) '
          f'({", ".join(f"{k}: {v}" for k, v in added.items())}); {len(disagreements)} disagreement(s), '
          f'{len(missing)} without a native symbol')
    for item in disagreements:
        print(f'  disagree {item["original"]}: kept {item["kept"]}, {item["from"]} has {item["other"]}')


if __name__ == '__main__':
    main()
