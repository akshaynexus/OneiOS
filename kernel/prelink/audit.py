#!/usr/bin/env python3
"""audit.py: what every kext import of kernel.kc is bound to, checked against the known-good pairs and the stub list.

usage: audit.py --xnuprelink DIR --kernelcache ORIGINAL_KC --collection kernel.kc
                [--pairs pairs-24A446.json] [--stubs stubs-24A446.json] [--reviewed reviewed.json]
                [--against OTHER.kc] [--report kernel.kc.binds.json] [--draft FILE]

An import is a chained-fixup site in a kext of the collection whose target in Apple's kernelcache is inside Apple's
kernel (a __auth_got slot, a vtable entry, a pointer in a table). The collection rebinds it to our kernel: to a
function or object (named here by the kernel's own symbol table inside kernel.kc), to the panic stub
_unixnu_fileset_unmapped, or, for data with no pair, to its own collection address ("unpaired": some other object of
our kernel, a wrong read).

Checks (each failure is printed; the exit status is 1 when there is one):
  - an original on the stub, or unpaired data, that has no entry in --stubs (its reason) fails; its callers are
    counted (kext code that branches to the import's stub or loads its slot);
  - an entry of --stubs that is bound now fails too (a new pair must be reviewed into --pairs first);
  - a bound original whose name is not the one --pairs records for it fails (alias names at the same address agree);
    the name is our kernel's external definition (a local function of the same name is another function); an entry
    with "leaf" agrees with any native function whose instructions are the original's: a body that only
    computes registers and returns, so its identity does not matter);
  - a bound original that --pairs does not list fails ("new pair").
--reviewed: its "sites" are checked one by one (an original pointer site bound to a named object) and left out of the
per-target check. --against: the a/b/c comparison with another collection (paired there, stubbed here; stubbed there,
paired here; paired to different functions), for a toolchain or matcher change. --draft writes the pairs of this
collection in the format of --pairs (with "why": "draft"), for review; it never replaces the committed list.
"""
import argparse
import bisect
import collections
import hashlib
import json
from pathlib import Path
import struct
import sys

BTI = {0xd503245f, 0xd503249f, 0xd50324df, 0xd503241f}
RET = {0xd65f03c0, 0xd65f0bff, 0xd65f0fff}
STUB = '_unixnu_fileset_unmapped'


def leaf(image, va):
    """The instructions of a function that only computes registers and returns (BTI left out), else None."""
    try:
        words = struct.unpack('<12I', image.read(va, 48))
    except ValueError:
        return None
    body = []
    for word in words:
        if word in BTI:
            continue
        if word in RET:
            return tuple(body + [0xd65f03c0])
        # data processing on registers only; not ADR/ADRP (an address, which differs between kernels)
        if (word >> 25) & 0xf not in (0b1000, 0b1001, 0b0101, 0b1101) or word & 0x1f000000 == 0x10000000:
            return None
        body.append(word)
        if len(body) > 8:
            return None
    return None


class Original:
    def __init__(self, path, Image):
        self.raw = Path(path).read_bytes()
        self.sha256 = hashlib.sha256(self.raw).hexdigest()
        self.top = Image(self.raw)
        self.kernel = Image(self.raw, self.top.entries['com.apple.kernel'][1])
        ranges = self.kernel.mapped()
        self.inside = lambda a: any(b <= a < e for b, e in ranges)
        self.owners = []
        for name, (_, at) in self.top.entries.items():
            if name != 'com.apple.kernel':
                for s in Image(self.raw, at).segments:
                    if s['size'] and s['name'] != '__LINKEDIT':
                        self.owners.append((s['va'], s['va'] + s['size'], name))
        self.owners.sort()
        self._starts = [o[0] for o in self.owners]
        self.sites = {}
        for site, (target, *_) in self.top.fixups().items():
            if self.inside(target) and not self.inside(site):
                owner = self.owner(site)
                if owner:
                    self.sites[site] = (target, owner)

    def owner(self, address):
        i = bisect.bisect_right(self._starts, address) - 1
        return self.owners[i][2] if i >= 0 and address < self.owners[i][1] else None

    def is_function(self, target):
        section = self.kernel.section(target)
        return bool(section and section['segment'] == '__TEXT_EXEC')


class Collection:
    """kernel.kc: each original import site's bound target, named."""

    def __init__(self, path, original, Image):
        self.raw = Path(path).read_bytes()
        self.sha256 = hashlib.sha256(self.raw).hexdigest()
        top = Image(self.raw)
        self.top = top
        self.kernel = Image(self.raw, top.entries['com.apple.kernel'][1])
        self.symbols = self.kernel.symbols()
        self.addresses = sorted(self.symbols)
        # name -> its external definition, or its one local one (emit.py resolves names the same way)
        self.by_name, local = {}, collections.defaultdict(set)
        data, at = self.raw, self.kernel.command(0x2)[0]
        symoff, count, stroff, _ = struct.unpack_from('<4I', data, at + 8)
        for index in range(count):
            strx, kind, _, _, value = struct.unpack_from('<IBBHQ', data, symoff + 16 * index)
            if kind & 0xe0 or kind & 0x0e != 0x0e:
                continue
            name = data[stroff + strx:data.index(b'\0', stroff + strx)].decode()
            if kind & 1:
                self.by_name.setdefault(name, value)
            else:
                local[name].add(value)
        for name, values in local.items():
            if name not in self.by_name and len(values) == 1:
                self.by_name[name] = next(iter(values))
        self.stub = self.by_name.get(STUB)
        self.slide = top.base - original.top.base
        mapped = self.kernel.mapped()
        fixups = top.fixups()
        self.bound = {}                       # site -> ('stub' | 'unpaired' | 'other' | name, address)
        for site, (target, owner) in original.sites.items():
            found = fixups.get(site + self.slide)
            if found is None:
                continue                      # a kext this collection left out
            value = found[0]
            if value == self.stub:
                self.bound[site] = ('stub', value)
            elif value == target + self.slide:
                self.bound[site] = ('unpaired', value)
            elif any(b <= value < e for b, e in mapped):
                self.bound[site] = (self.name(value), value)
            else:
                self.bound[site] = ('other', value)

    def name(self, value):
        i = bisect.bisect_right(self.addresses, value) - 1
        if i < 0:
            return f'{value:#x}'
        at = self.addresses[i]
        names = '|'.join(sorted(set(self.symbols[at])))
        return names if at == value else f'{names}+{value - at:#x}'

    def names(self, value):
        return set(self.symbols.get(value, ()))


def targets(original, collection, skip_sites=()):
    """original target -> {'bound': set of bound strings, 'address': set, 'kexts': set, 'sites': [...]}."""
    found = collections.defaultdict(lambda: {'bound': set(), 'address': set(), 'kexts': set(), 'sites': []})
    for site, (bound, value) in collection.bound.items():
        if site in skip_sites:
            continue
        target, owner = original.sites[site]
        entry = found[target]
        entry['bound'].add(bound)
        entry['address'].add(value)
        entry['kexts'].add(owner)
        entry['sites'].append(site)
    return found


def callers(original, Image, target_sites):
    """Per site: the kext code that calls through it (a branch to the stub that loads the slot, or a load of it)."""
    sys_path_scan = __import__('kc_image').scan
    signed = __import__('kc_image').signed
    by_kext = collections.defaultdict(set)
    for site in target_sites:
        by_kext[original.sites[site][1]].add(site)
    result = {}
    for kext, sites in by_kext.items():
        image = Image(original.raw, original.top.entries[kext][1])
        stubs = {}
        for section in image.sections:
            if section['name'] in ('__auth_stubs', '__stubs'):
                _, refs, _ = sys_path_scan(image.read(section['va'], section['size']), section['va'])
                for pc, ref, _ in refs:
                    if ref in sites:
                        stubs[pc - (pc - section['va']) % 16] = ref
        counts = collections.Counter()
        for section in image.sections:
            if section['segment'] != '__TEXT_EXEC' or section['name'] != '__text':
                continue
            code = image.read(section['va'], section['size'])
            for i in range(0, len(code), 4):
                word, = struct.unpack_from('<I', code, i)
                if word & 0x7c000000 == 0x14000000:
                    t = section['va'] + i + signed(word & 0x3ffffff, 26) * 4
                    if t in stubs:
                        counts[stubs[t]] += 1
            _, refs, _ = sys_path_scan(code, section['va'])
            for _, ref, _ in refs:
                if ref in sites:
                    counts[ref] += 1
        for site in sites:
            result[site] = counts[site]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--xnuprelink', required=True)
    parser.add_argument('--kernelcache', required=True)
    parser.add_argument('--collection', required=True)
    parser.add_argument('--pairs')
    parser.add_argument('--stubs')
    parser.add_argument('--reviewed')
    parser.add_argument('--against')
    parser.add_argument('--report')
    parser.add_argument('--draft')
    args = parser.parse_args()
    sys.path.insert(0, args.xnuprelink)
    from kc_image import Image

    original = Original(args.kernelcache, Image)
    collection = Collection(args.collection, original, Image)
    if collection.stub is None:
        raise SystemExit(f'audit: {args.collection} has no {STUB}')
    failures = []
    reviewed_sites = {}
    if args.reviewed:
        for record in json.loads(Path(args.reviewed).read_text()).get('sites', []):
            reviewed_sites[int(record['site'], 16)] = record['native_symbol']
    for site, symbol in reviewed_sites.items():
        bound = collection.bound.get(site)
        if bound and symbol not in collection.names(bound[1]):
            failures.append(f'site {site:#x}: reviewed {symbol}, bound to {bound[0]}')
    found = targets(original, collection, reviewed_sites)

    pairs = json.loads(Path(args.pairs).read_text()) if args.pairs else None
    stubs = json.loads(Path(args.stubs).read_text()) if args.stubs else None
    if pairs and pairs.get('kernelcache_sha256') != original.sha256:
        raise SystemExit('audit: the pairs list is for another kernelcache')
    known = {}
    for kind in ('functions', 'data'):
        for entry in (pairs or {}).get(kind, []):
            known[int(entry['original'], 16)] = entry
    stubbed = {int(e['original'], 16): e for e in (stubs or {}).get('functions', []) + (stubs or {}).get('data', [])}

    stub_sites = [s for t, e in found.items() if e['bound'] & {'stub', 'unpaired'} for s in e['sites']]
    calls = callers(original, Image, stub_sites)
    counts = collections.Counter()
    report = {'collection_sha256': collection.sha256, 'kernelcache_sha256': original.sha256, 'functions': {}, 'data': {}}
    for target in sorted(found):
        entry = found[target]
        kind = 'functions' if original.is_function(target) else 'data'
        bound = sorted(entry['bound'])
        row = {'bound': bound if len(bound) > 1 else bound[0], 'kexts': sorted(entry['kexts'])}
        report[kind][f'{target:#x}'] = row
        where = f'{target:#x} ({kind[:-1] if kind == "functions" else "data"}, {len(entry["kexts"])} kexts: ' \
                f'{", ".join(k.replace("com.apple.", "") for k in sorted(entry["kexts"])[:4])})'
        if len(bound) > 1:
            failures.append(f'{where}: bound to several targets {bound}')
            continue
        bound = bound[0]
        if bound in ('stub', 'unpaired'):
            row['callers'] = sum(calls.get(s, 0) for s in entry['sites'])
            counts[bound] += 1
            if stubs is not None and target not in stubbed:
                failures.append(f'{where}: {bound} without a reason in the stub list; '
                                f'{row["callers"]} call sites in the kexts')
            continue
        if bound == 'other':
            failures.append(f'{where}: bound outside our kernel')
            continue
        counts['paired'] += 1
        if stubs is not None and target in stubbed:
            failures.append(f'{where}: the stub list has it, but it is bound to {bound}: review it into the pairs')
            continue
        if pairs is None:
            continue
        want = known.get(target)
        if want is None:
            failures.append(f'{where}: new pair {bound}, not in the known-good list')
            continue
        address = next(iter(entry['address']))
        name, _, offset = want['native'].partition('+')
        ok = collection.by_name.get(name) == address - (int(offset, 16) if offset else 0)
        if not ok and want.get('leaf') and kind == 'functions':
            mine = leaf(collection.kernel, address)
            ok = mine is not None and mine == leaf(original.kernel, target)
        if not ok:
            failures.append(f'{where}: bound to {bound}, the known-good list says {want["native"]}')
    report['counts'] = dict(counts)
    if args.against:
        other = Collection(args.against, original, Image)
        theirs = targets(original, other, reviewed_sites)
        diff = collections.defaultdict(list)
        for target, entry in found.items():
            if target not in theirs:
                continue
            mine, them = sorted(entry['bound'])[0], sorted(theirs[target]['bound'])[0]
            paired = lambda b: b not in ('stub', 'unpaired', 'other')
            if paired(them) and mine in ('stub', 'unpaired'):
                diff['a'].append((target, them, mine))
            elif them in ('stub', 'unpaired') and paired(mine):
                diff['b'].append((target, them, mine))
            elif paired(them) and paired(mine):
                a = other.names(next(iter(theirs[target]['address'])))
                b = collection.names(next(iter(entry['address'])))
                same_body = original.is_function(target) and leaf(original.kernel, target) is not None and \
                    leaf(other.kernel, next(iter(theirs[target]['address']))) == \
                    leaf(collection.kernel, next(iter(entry['address'])))
                if not (a & b) and mine != them:
                    diff['c-leaf' if same_body else 'c'].append((target, them, mine))
        report['against'] = {'collection_sha256': other.sha256,
                             **{k: [{'original': f'{t:#x}', 'there': x, 'here': y} for t, x, y in v] for k, v in diff.items()}}
        print('against', args.against, {k: len(v) for k, v in sorted(diff.items())})
    if args.draft:
        draft = {'kernelcache_sha256': original.sha256, 'functions': [], 'data': []}
        for kind in ('functions', 'data'):
            for target, row in report[kind].items():
                if isinstance(row['bound'], str) and row['bound'] not in ('stub', 'unpaired', 'other'):
                    draft[kind].append({'original': target, 'native': row['bound'].split('|')[0], 'why': 'draft'})
        Path(args.draft).write_text(json.dumps(draft, indent=0) + '\n')
    report['failures'] = failures
    if args.report:
        Path(args.report).write_text(json.dumps(report, indent=1) + '\n')
    print(f'audit: {collection.sha256[:16]}: {counts["paired"]} paired, {counts["stub"]} functions on the stub, '
          f'{counts["unpaired"]} data unpaired; {len(failures)} failures')
    for line in failures[:60]:
        print('  FAIL ' + line)
    if len(failures) > 60:
        print(f'  ... {len(failures) - 60} more (the report lists them all)')
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
