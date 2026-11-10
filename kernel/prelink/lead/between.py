#!/usr/bin/env python3
"""between.py NAME_A NAME_B: the original kernel's functions between two paired ones, and the native kernel's between
the same two names, side by side by position (for pairing a block of small accessors by source order)."""
import bisect, json, subprocess, struct, sys
from pathlib import Path
S = Path(__file__).resolve().parent
R = Path('/home/akshay/Documents/GitHub/OneiOS/out/lead-wt')
sys.path.insert(0, str(R / 'external/XnuPrelink'))
from kc_image import Image
a, b = sys.argv[1:3]
ADD = len(sys.argv) > 3 and sys.argv[3] == "--add"
import hashlib
reviewed = json.loads((R / 'kernel/prelink/reviewed-24A446-v54.json').read_text())
lead = json.loads((S / 'lead-reviewed.json').read_text()) if (S / 'lead-reviewed.json').exists() else {'functions': []}
m = json.loads((S / 'kc/work/match.map.json').read_text())
report = json.loads((S / 'kc/kernel.kc.prelink.json').read_text())
name_of = {int(o, 16): v['name'] for o, v in m['functions'].items()}
for f in reviewed['functions'] + lead['functions']:
    name_of[int(f['original'], 16)] = f['native_symbol']
for group in report.get('reviewed_applied', {}).values() if isinstance(report.get('reviewed_applied'), dict) else []:
    if isinstance(group, list):
        for f in group:
            if isinstance(f, dict) and 'original' in f and ('native_symbol' in f or 'name' in f):
                name_of.setdefault(int(f['original'], 16), f.get('native_symbol') or f.get('name'))
orig_of = {v: k for k, v in name_of.items()}
stub = set()
sf = report.get('stubbed_functions')
if isinstance(sf, list):
    for x in sf:
        stub.add(int(x['original'] if isinstance(x, dict) else x, 16))
raw = (R / 'out/kernel-inputs/24A446/kernelcache.macho').read_bytes()
cache = Image(raw)
kernel = Image(raw, cache.entries[reviewed['entry']][1])
starts = sorted(kernel.function_starts())
oa, ob = orig_of[a], orig_of[b]
i, j = bisect.bisect_left(starts, oa), bisect.bisect_left(starts, ob)
native = []
for line in subprocess.run(['llvm-nm', '-n', str(R / 'out/kernel/current-hosted/xnu.macho')], capture_output=True, text=True).stdout.splitlines():
    p = line.split()
    if len(p) == 3 and p[1] in 'tTsS' and not p[2].startswith(('ltmp', 'l_', 'L')) and not p[2].endswith('.original'):
        native.append((int(p[0], 16), p[2]))
names = [n for _, n in native]
na, nb = names.index(a), names.index(b)
print(f'original: {j - i - 1} functions between; native: {nb - na - 1}')
def words(addr, size):
    w = struct.unpack_from('<%dI' % (size // 4), kernel.read(addr, size))
    return ' '.join('%08x' % x for x in w[:10])
orig_rows = [(starts[k], starts[k + 1] - starts[k]) for k in range(i, j + 1)]
nat_rows = [(native[k][1], native[k + 1][0] - native[k][0]) for k in range(na, nb + 1)]
for k in range(max(len(orig_rows), len(nat_rows))):
    o = orig_rows[k] if k < len(orig_rows) else None
    n = nat_rows[k] if k < len(nat_rows) else None
    left = f'{o[0]:#x} {o[1]:5d} {"STUB" if o[0] in stub else "    "} {name_of.get(o[0], "?"):40.40s}' if o else ' ' * 70
    right = f'{n[1]:5d} {n[0]}' if n else ''
    print(left, '|', right, '|', words(o[0], min(o[1], 40)) if o and o[0] not in name_of else '')

if ADD:
    if len(orig_rows) != len(nat_rows):
        sys.exit('pairblock: the counts differ; nothing added')
    bad = [(o, n) for o, n in zip(orig_rows, nat_rows) if (o[1] >= 100) != (n[1] >= 44) and o[0] not in name_of]
    if bad:
        sys.exit('pairblock: shapes disagree: ' + ', '.join(f'{o[0]:#x}/{o[1]} vs {n[0]}/{n[1]}' for o, n in bad))
    wrong = [(o, n) for o, n in zip(orig_rows, nat_rows) if o[0] in name_of and name_of[o[0]] != n[0]]
    if wrong:
        sys.exit('pairblock: existing pairs disagree with the position: ' + ', '.join(f'{o[0]:#x} is {name_of[o[0]]}, position says {n[0]}' for o, n in wrong))
    path = S / 'lead-reviewed.json'
    added = 0
    for o, n in zip(orig_rows, nat_rows):
        if o[0] in name_of:
            continue
        data = kernel.read(o[0], o[1])
        lead['functions'].append({'original': hex(o[0]), 'native_symbol': n[0],
            'check': {'address': hex(o[0]), 'size': o[1], 'sha256': hashlib.sha256(data).hexdigest()},
            'evidence': f'By position in a block of accessors: between {a} ({oa:#x}) and {b} ({ob:#x}) the original kernel and ours have the same number of functions ({len(orig_rows) - 2}), in the same order as the source file, every pair already known in the block agrees with its position, and each function has the shape of its partner (a locked update or a short leaf). ' + (sys.argv[4] if len(sys.argv) > 4 else '')})
        added += 1
    path.write_text(json.dumps(lead, indent=1) + '\n')
    print(f'pairblock: {added} records added ({len(lead["functions"])} in all)')
