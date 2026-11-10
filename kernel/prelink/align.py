#!/usr/bin/env python3
"""Pair original kernel functions the matcher left unpaired, by link order against the reference kernel.

usage: align.py --xnuprelink DIR --kernelcache KC --reference REF --native NATIVE --map MAP --out aligned.json
               [--threshold 2.0] [--holdout 2500]

The original kernel and the RELEASE reference kernel (same source, link-time optimised) are linked in the same order.
For an unpaired original function that a kext references, take the nearest paired functions before and after it
(at most 60 functions apart) whose names the reference defines once and has in the same order about as far apart, and align the original functions between them
with the reference's functions between the same two names (Needleman-Wunsch; a pair scores by the overlap of its
named callees, strings and named data, by its size and by its call count). The function takes the name it is aligned
with when the pair's score is at least --threshold, the native kernel defines that name once and no other original
function has it. XnuPrelink's own order rule needs equal counts between the anchors; this one tolerates functions
that only one side has.

The same procedure run on already paired functions with their pairing hidden (--holdout N, a fixed sample) gives
the error rate, written into the output with every pair: these are inferred, not reviewed. emit.py writes them into
the pairing as 'how': 'aligned' after the matcher's own and before the reviewed records, which win.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import random
import struct
import sys

WINDOW, GAP = 60, -0.4


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for name in ('xnuprelink', 'kernelcache', 'reference', 'native', 'map', 'out'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--threshold', type=float, default=2.0)
    parser.add_argument('--holdout', type=int, default=2500)
    parser.add_argument('--position', action='store_true',
                        help='also pair the one function between two anchors by position (measure it: 1 %% wrong in the '
                             'reference order, 10 %% in a DEVELOPMENT kernel order)')
    parser.add_argument('--features', help='kernel whose function of the same name gives the features (the RELEASE reference, '
                        'closer to the original code than a DEVELOPMENT kernel), when --reference is only the order')
    parser.add_argument('--also', action='append', default=[], help='pairs of an earlier run (another order) or a reviewed file: read as paired')
    parser.add_argument('--explain', help='print the alignment of the window around this original function and stop')
    args = parser.parse_args()
    sys.path.insert(0, args.xnuprelink)
    from kc_image import Image, scan
    raw = Path(args.kernelcache).read_bytes()
    pairing = json.loads(Path(args.map).read_text())
    cache = Image(raw)
    kernel = Image(raw, cache.entries['com.apple.kernel'][1])
    kernel_ranges = kernel.mapped()
    starts = sorted(kernel.function_starts())
    index = {address: i for i, address in enumerate(starts)}
    name_of = {int(k, 16): v['name'] for k, v in pairing['functions'].items() if v.get('name')}
    data_of = {int(k, 16): v['name'] for k, v in pairing['data'].items() if v.get('name')}
    matched = set(name_of)
    for path in args.also:
        record = json.loads(Path(path).read_text())             # align.py output, or a reviewed file
        for pair in record.get('pairs', record.get('functions', [])):
            if 'pairs' in record:
                name_of.setdefault(int(pair['original'], 16), pair['native_symbol'])
            else:
                name_of[int(pair['original'], 16)] = pair['native_symbol']
    reference = Image(Path(args.reference).read_bytes())
    symbols = reference.symbols()
    code = [s for s in reference.segments if 'EXEC' in s['name']]
    ref_starts = sorted(a for a in symbols if any(s['va'] <= a < s['va'] + s['size'] for s in code))
    ref_index = {address: i for i, address in enumerate(ref_starts)}
    by_name = {}
    for address in ref_starts:
        for name in symbols[address]:
            by_name.setdefault(name, []).append(address)
    native_names = Counter(name for names in Image(Path(args.native).read_bytes()).symbols().values() for name in names)

    def ref_address(name):
        found = by_name.get(name)
        return found[0] if found and len(found) == 1 else None

    def features(image, start, end, call_name, data_name):
        try:
            blob = image.read(start, end - start)
        except ValueError:
            return Counter(), end - start, 0
        calls, refs, _ = scan(blob, start)
        found, count = Counter(), 0
        for _, target, _ in calls:
            if start <= target < end:
                continue
            count += 1
            name = call_name(target)
            if name:
                found['c:' + name] += 1
        for offset in range(0, len(blob) - 3, 4):
            word, = struct.unpack_from('<I', blob, offset)
            if word & 0x1f800000 == 0x12800000 and word >> 29 & 3 != 1:       # movn/movz/movk
                value = word >> 5 & 0xffff
                if value > 15:
                    found['i:%x' % value] += 1
            elif word & 0x7f800000 == 0x71000000 and (word >> 10 & 0xfff) > 8:  # cmp/subs #imm
                found['k:%x' % (word >> 10 & 0xfff)] += 1
            elif word & 0x3b000000 == 0x39000000 and (word >> 5 & 31) not in (29, 31):   # ldr/str [xN, #offset]
                value = (word >> 10 & 0xfff) << (word >> 30)
                if value >= 16:
                    found['o:%x' % value] += 1
        for _, ref, _ in refs:
            try:
                text = image.cstring(ref)
            except Exception:
                text = None
            if text and len(text) > 3:
                found['s:' + text] += 1
            else:
                name = data_name(ref)
                if name:
                    found['d:' + name] += 1
        return found, end - start, count

    first = lambda address: sorted(symbols[address])[0] if address in symbols else None  # noqa: E731
    cache_o, cache_r = {}, {}

    def original(i):
        if i not in cache_o:
            end = starts[i + 1] if i + 1 < len(starts) else starts[i] + 64
            cache_o[i] = features(kernel, starts[i], end, name_of.get, data_of.get)
        return cache_o[i]

    shape, shape_symbols, shape_starts, shape_by_name = None, {}, [], {}
    if args.features:
        shape = Image(Path(args.features).read_bytes())
        shape_symbols = shape.symbols()
        shape_code = [s for s in shape.segments if 'EXEC' in s['name']]
        shape_starts = sorted(a for a in shape_symbols if any(s['va'] <= a < s['va'] + s['size'] for s in shape_code))
        for position, address in enumerate(shape_starts):
            for name in shape_symbols[address]:
                shape_by_name.setdefault(name, []).append(position)
    shape_first = lambda address: sorted(shape_symbols[address])[0] if address in shape_symbols else None  # noqa: E731

    def ref(j):
        if j not in cache_r:
            end = ref_starts[j + 1] if j + 1 < len(ref_starts) else ref_starts[j] + 64
            other = [shape_by_name[name] for name in symbols[ref_starts[j]] if len(shape_by_name.get(name, ())) == 1]
            if other:
                at = other[0][0]
                end = shape_starts[at + 1] if at + 1 < len(shape_starts) else shape_starts[at] + 64
                cache_r[j] = features(shape, shape_starts[at], end, shape_first, shape_first)
            else:
                cache_r[j] = features(reference, ref_starts[j], end, first, first)
        return cache_r[j]

    def similarity(i, j, hidden):
        (fo, so, co), (fr, sr, cr) = original(i), ref(j)
        if hidden:
            fo = Counter({k: v for k, v in fo.items() if k[2:] not in hidden})
        shared, union = sum((fo & fr).values()), sum((fo | fr).values())
        score = (2.0 * shared / union if union else 0.0) + 1.0 - abs(so - sr) / max(so, sr) + (0.3 if co == cr else 0.0)
        # A function that names a class's metaclass (a factory: alloc, then init) is a member of that class. The
        # original's link order differs from ours around the end of a C++ file, where the window is the next
        # class's (OSSet::withCapacity landed on OSString::withString).
        if any(key.endswith('10gMetaClassE') and key not in fr for key in fo if key.startswith('d:')):
            score = min(score, args.threshold - 0.01)
        return score, shared, union

    def predict(target, hidden=frozenset(), explain=False):
        i = index.get(target)
        if i is None:
            return None
        anchor = lambda at: starts[at] in name_of and starts[at] not in hidden and ref_address(name_of[starts[at]]) is not None  # noqa: E731
        # The nearest anchors whose reference positions are in the same order and about as far apart: a pair the
        # matcher got wrong (its order rule does make some) would otherwise give a window somewhere else.
        before, after, at = [], [], i - 1
        while at >= 0 and i - at <= WINDOW and len(before) < 5:
            if anchor(at):
                before.append(at)
            at -= 1
        at = i + 1
        while at < len(starts) and at - i <= WINDOW and len(after) < 5:
            if anchor(at):
                after.append(at)
            at += 1
        chosen = None
        for a, b in sorted(((a, b) for a in before for b in after), key=lambda pair: pair[1] - pair[0]):
            ra, rb = ref_index[ref_address(name_of[starts[a]])], ref_index[ref_address(name_of[starts[b]])]
            if b - a <= WINDOW and 1 < rb - ra <= 3 * (b - a) + 8:
                chosen = (a, b, ra, rb)
                break
        if chosen is None:
            return None
        a, b, ra, rb = chosen
        left, right = list(range(a + 1, b)), list(range(ra + 1, rb))
        hidden_names = {name_of[h] for h in hidden if h in name_of}
        n, k = len(left), len(right)
        cost = [[0.0] * (k + 1) for _ in range(n + 1)]
        back = [[0] * (k + 1) for _ in range(n + 1)]
        for x in range(1, n + 1):
            cost[x][0], back[x][0] = x * GAP, 1
        for y in range(1, k + 1):
            cost[0][y], back[0][y] = y * GAP, 2
        scores = {}
        for x in range(1, n + 1):
            for y in range(1, k + 1):
                scores[x, y] = similarity(left[x - 1], right[y - 1], hidden_names)
                choices = (cost[x - 1][y - 1] + scores[x, y][0] - 1.0, cost[x - 1][y] + GAP, cost[x][y - 1] + GAP)
                best = max(range(3), key=choices.__getitem__)
                cost[x][y], back[x][y] = choices[best], best
        x, y = n, k
        if explain:
            rows = []
            while x > 0 or y > 0:
                step = back[x][y]
                if step == 0:
                    rows.append((hex(starts[left[x - 1]]), name_of.get(starts[left[x - 1]]), original(left[x - 1])[1],
                                 first(ref_starts[right[y - 1]]), ref(right[y - 1])[1], round(scores[x, y][0], 2)))
                    x, y = x - 1, y - 1
                elif step == 1:
                    rows.append((hex(starts[left[x - 1]]), name_of.get(starts[left[x - 1]]), original(left[x - 1])[1], '-', '', ''))
                    x -= 1
                else:
                    rows.append(('-', '', '', first(ref_starts[right[y - 1]]), ref(right[y - 1])[1], ''))
                    y -= 1
            print(f'between {name_of[starts[a]]} and {name_of[starts[b]]}: {n} original, {k} reference functions')
            for row in reversed(rows):
                print(' ', *row)
            return None
        strong, mine = [(0, 0), (n + 1, k + 1)], None        # the anchors, then every pair at or over the threshold
        while x > 0 or y > 0:
            step = back[x][y]
            if step == 0:
                if left[x - 1] == i:
                    mine = (x, y)
                if scores[x, y][0] >= args.threshold:
                    strong.append((x, y))
                x, y = x - 1, y - 1
            elif step == 1:
                x -= 1
            else:
                y -= 1
        result = lambda y, score, kind: (sorted(symbols[ref_starts[right[y - 1]]]),) + score + (  # noqa: E731
            name_of[starts[a]], name_of[starts[b]], kind)
        if mine and scores[mine][0] >= args.threshold:
            return result(mine[1], scores[mine], 'features')
        # Too little in the function itself (a thunk, an accessor): its position counts only when it is the one
        # function between two anchors on both sides. Longer equal runs were wrong one time in six on known pairs.
        x = left.index(i) + 1
        below = max(pair for pair in strong if pair[0] < x)
        above = min(pair for pair in strong if pair[0] > x)
        if args.position and n == 1 and k == 1:
            y = below[1] + x - below[0]
            return result(y, scores[x, y], 'position')
        if mine:
            return result(mine[1], scores[mine], 'weak')
        return None

    if args.explain:
        if predict(int(args.explain, 16), explain=True) is None and int(args.explain, 16) not in index:
            print('not a function start of the original kernel')
        return
    rng = random.Random(2)
    known = [address for address in matched if address in index]
    tally = Counter()
    for address in rng.sample(known, min(args.holdout, len(known))):
        found = predict(address, frozenset((address,)))
        if found and found[-1] != 'weak':
            tally[found[-1], name_of[address] in found[0]] += 1
    taken = set(name_of.values())
    targets = sorted({fix[0] for site, fix in cache.fixups().items()
                      if fix[0] in index and fix[0] not in name_of
                      and not any(b <= site < e for b, e in kernel_ranges)})
    pairs, refused, rounds = [], Counter(), 0
    while True:                 # each new pair is an anchor and a named callee for the next round
        rounds += 1
        added, refused = 0, Counter()
        for target in targets:
            if target in name_of:
                continue
            found = predict(target)
            if not found:
                refused['no alignment'] += 1
                continue
            names, score, shared, union, before, after, kind = found
            if kind == 'weak':
                refused['below the threshold'] += 1
                continue
            usable = [name for name in names if native_names.get(name) == 1 and name not in taken]
            if not usable:
                refused['name absent, ambiguous or already paired'] += 1
                continue
            taken.add(usable[0])
            pairs.append({'original': hex(target), 'native_symbol': usable[0], 'score': round(score, 2),
                          'shared_features': shared, 'features': union, 'between': [before, after], 'by': kind,
                          'round': rounds})
            added += 1
        if not added or rounds == 8:
            break
        for pair in pairs:
            name_of[int(pair['original'], 16)] = pair['native_symbol']
        cache_o.clear()
    record = {
        'kernelcache_sha256': hashlib.sha256(raw).hexdigest(),
        'reference_sha256': hashlib.sha256(Path(args.reference).read_bytes()).hexdigest(),
        'threshold': args.threshold,
        'holdout': {kind: {'right': tally[kind, True], 'wrong': tally[kind, False]} for kind in ('features', 'position')},
        'order_of': str(Path(args.reference).name), 'rounds': rounds,
        'unpaired_kext_targets': len(targets), 'refused': dict(refused), 'pairs': pairs,
    }
    Path(args.out).write_text(json.dumps(record, indent=1) + '\n')
    print(f'align: {len(pairs)} of {len(targets)} unpaired kext-facing functions paired by the order of {Path(args.reference).name} in {rounds} round(s) '
          f'(holdout: features {tally["features", False]} wrong of {tally["features", True] + tally["features", False]}, '
          f'position {tally["position", False]} wrong of {tally["position", True] + tally["position", False]}); refused: {dict(refused)}')


if __name__ == '__main__':
    main()
