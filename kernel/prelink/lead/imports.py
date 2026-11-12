#!/usr/bin/env python3
"""stubs.py [--add] [--kext ID]: pair unknown kernel imports of the kexts by the order of each kext's import table.

A kext's __auth_got (the slots its import stubs load) is in the alphabetical order of the imported symbols. Every
slot whose target is already paired gives a name; an unpaired target between two named slots must be a symbol
between those names. The bounds from every kext that imports the same target are intersected, and the kernel's
exported functions between them that no other original is paired with are the candidates.

Prints: how well the known names obey the order (pairs that break it are suspects), and for each unpaired target
its bounds and candidates. --add writes the targets with exactly one candidate to scratch/lead-reviewed.json.
"""
import bisect, hashlib, json, struct, subprocess, sys
from pathlib import Path

S = Path(__file__).resolve().parent
R = S.parents[2]                                   # the repository (was the lead's worktree on the Linux PC)
_X = R / 'external/XnuPrelink'
sys.path.insert(0, str(_X if (_X / 'kc_image.py').is_file() else R / 'external/Droid-VM-UniXNU/kits/XnuPrelink'))
import os, shutil
for _d in ('/opt/homebrew/opt/llvm/bin', '/usr/local/opt/llvm/bin'):     # llvm-nm: keg-only on a Mac
    if os.path.isdir(_d):
        os.environ['PATH'] = _d + os.pathsep + os.environ['PATH']
KC = R / 'out/kernel/24A446'                         # kernel/prelink/build.sh's output
from kc_image import Image

ADD = '--add' in sys.argv
ONLY = sys.argv[sys.argv.index('--kext') + 1] if '--kext' in sys.argv else None
reviewed = json.loads((R / 'kernel/prelink/reviewed-24A446-v54.json').read_text())
lead_path = S / 'lead-reviewed.json'
lead = json.loads(lead_path.read_text()) if lead_path.exists() else {'functions': []}
m = json.loads((KC / 'work/match.map.json').read_text())
report = json.loads((KC / 'kernel.kc.prelink.json').read_text())
raw = (R / 'out/kernel-inputs/24A446/kernelcache.macho').read_bytes()
cache = Image(raw)
kernel = Image(raw, cache.entries[reviewed['entry']][1])
starts = sorted(kernel.function_starts())
start_set = set(starts)
kernel_ranges = [(s['va'], s['va'] + s['size']) for s in kernel.segments if s['size']]

name_of, how_of = {}, {}
for o, v in m['functions'].items():
    name_of[int(o, 16)] = v['name']; how_of[int(o, 16)] = v['how']
for o, v in m['data'].items():
    name_of[int(o, 16)] = v['name']; how_of[int(o, 16)] = 'data:' + v['how']
for key in ('aligned', 'functions'):
    for f in report['reviewed_applied'].get(key, []):
        name_of[int(f['original'], 16)] = f['native_symbol']; how_of[int(f['original'], 16)] = 'applied:' + key
for d in report['reviewed_applied'].get('data_inferred', []) + report['reviewed_applied'].get('data', []):
    if 'original' in d and 'native_symbol' in d:
        name_of[int(d['original'], 16)] = d['native_symbol']; how_of[int(d['original'], 16)] = 'data'
for f in reviewed['functions'] + lead['functions']:
    name_of[int(f['original'], 16)] = f['native_symbol']; how_of[int(f['original'], 16)] = 'reviewed'
for d in reviewed.get('data', []):
    if 'original' in d and 'native_symbol' in d:
        name_of[int(d['original'], 16)] = d['native_symbol']; how_of[int(d['original'], 16)] = 'reviewed-data'
taken = set(name_of.values())

# The kernel's exported symbols (what a kext can import), in strcmp order.
exported = set()
for line in subprocess.run(['llvm-nm', '-g', '--defined-only', str(R / 'out/kernel/current-hosted/xnu.macho')], capture_output=True, text=True).stdout.splitlines():
    p = line.split()
    if len(p) == 3 and not p[2].endswith('.original'):
        exported.add(p[2])
exported_sorted = sorted(exported)
# Every native function symbol by address, to bound a target by its paired neighbours in the original's own order.
native_at = {}
for line in subprocess.run(['llvm-nm', '-n', str(R / 'out/kernel/current-hosted/xnu.macho')], capture_output=True, text=True).stdout.splitlines():
    p = line.split()
    if len(p) == 3 and not p[2].endswith('.original'):
        native_at.setdefault(p[2], int(p[0], 16))
by_addr = {}
for n, a in native_at.items(): by_addr.setdefault(a, []).append(n)
def aliases(n): return by_addr.get(native_at.get(n), [n]) if n in native_at else [n]
paired_starts = sorted(a for a in name_of if a in start_set and name_of[a] in native_at)


def window(t, reach=24, span=0x8000):
    """Native names whose address lies between the native partners of the nearest paired functions around t."""
    i = bisect.bisect_left(paired_starts, t)
    best = None
    for lo in paired_starts[max(0, i - 3):i][::-1]:
        for hi in paired_starts[i:i + 3]:
            if bisect.bisect_left(starts, hi) - bisect.bisect_left(starts, lo) > reach:
                continue
            a, b = native_at[name_of[lo]], native_at[name_of[hi]]
            if 0 < b - a <= span and (best is None or b - a < best[1] - best[0]):
                best = (a, b, name_of[lo], name_of[hi])
    return best

base = cache.base


def in_kernel(va):
    return any(a <= va < b for a, b in kernel_ranges)


bounds = {}          # target -> [lower, upper, kexts]
violations, checked = [], 0
for ident, (va, fileoff) in sorted(cache.entries.items()):
    if ident == reviewed['entry'] or (ONLY and ident != ONLY):
        continue
    kext = Image(raw, fileoff)
    for sect in kext.sections:
        if sect['name'] not in ('__auth_got', '__got') or not sect['size']:
            continue
        slots = []
        for i in range(sect['size'] // 8):
            r, = struct.unpack_from('<Q', raw, kext.offset(sect['va'] + 8 * i, 8))
            t = base + (r & 0x3fffffff)
            slots.append(t if in_kernel(t) else None)
        known = [(i, name_of[t]) for i, t in enumerate(slots) if t in name_of]
        for (i, a), (j, b) in zip(known, known[1:]):
            checked += 1
            if not any(x < y for x in aliases(a) for y in aliases(b)) and slots[i] != slots[j]:
                violations.append((ident, sect['name'], a, b, how_of.get(slots[i]), how_of.get(slots[j]), slots[i], slots[j]))
        for i, t in enumerate(slots):
            if t is None or t in name_of:
                continue
            lower = next((name_of[slots[k]] for k in range(i - 1, -1, -1) if slots[k] in name_of), '')
            upper = next((name_of[slots[k]] for k in range(i + 1, len(slots)) if slots[k] in name_of), '\x7f')
            entry = bounds.setdefault(t, ['', '\x7f', []])
            entry[0] = max(entry[0], lower); entry[1] = min(entry[1], upper); entry[2].append(ident.split('.')[-1])

print(f'order of known names: {checked} adjacent pairs checked, {len(violations)} out of order')
for v in violations:
    print(f'  SUSPECT in {v[0]} {v[1]}: {v[2]} ({v[4]}, {v[6]:#x}) before {v[3]} ({v[5]}, {v[7]:#x})')
unique, added = 0, 0
for t, (lower, upper, kexts) in sorted(bounds.items()):
    lo = bisect.bisect_right(exported_sorted, lower); hi = bisect.bisect_left(exported_sorted, upper)
    candidates = [n for n in exported_sorted[lo:hi] if n not in taken]
    near = window(t) if t in start_set else None
    narrowed = [n for n in candidates if near and near[0] < native_at.get(n, 0) < near[1]] if near else []
    if len(candidates) != 1 and len(narrowed) == 1:
        candidates = narrowed
        how = f' It is also the only one of them that our kernel places between {near[2]} and {near[3]}, the paired functions on either side of this one in the original.'
    else:
        how = ''
    is_function = t in start_set
    flag = 'fn  ' if is_function else 'data'
    if len(candidates) == 1:
        unique += 1
    if not ADD or len(candidates) <= 6:
        print(f'{t:#x} {flag} {len(kexts):3d} kexts  ({lower} .. {upper}): ' + (', '.join(candidates) if len(candidates) <= 6 else f'{len(candidates)} candidates'))
    if ADD and len(candidates) == 1 and is_function and lower and upper != '\x7f':
        i = bisect.bisect_left(starts, t)
        size = starts[i + 1] - t
        lead['functions'].append({'original': hex(t), 'native_symbol': candidates[0],
            'check': {'address': hex(t), 'size': size, 'sha256': hashlib.sha256(kernel.read(t, size)).hexdigest()},
            'evidence': f'By the order of the kexts\' import tables (__auth_got is in the alphabetical order of the imported symbols): in the {len(kexts)} kext(s) that import this function ({", ".join(sorted(set(kexts))[:6])}) its slot lies between the slots of {lower} and {upper}, and {candidates[0]} is the only exported kernel function between those names that no other original function is paired with.' + how})
        taken.add(candidates[0]); added += 1
print(f'{len(bounds)} unpaired imported targets, {unique} with exactly one candidate' + (f', {added} added' if ADD else ''))
if ADD:
    lead_path.write_text(json.dumps(lead, indent=1) + '\n')
