#!/usr/bin/env python3
"""Check that an iOS Mach-O (executable, dylib or bundle) built here will load in the guest.

  scripts/ios/macho-check.py [--tbd DIR] [--sdk] [--local FILE ...] [--arch arm64e] FILE...

For each FILE:
1. It is a thin Mach-O for arm64e (or --arch), platform iOS (LC_BUILD_VERSION), an executable, dylib or bundle.
2. Every library it loads has a .tbd stub in DIR: out/ios27-tbd by default, the stubs scripts/ios/make-tbds.py
   makes from the guest's own iOS 27 shared cache; --sdk checks against the SDK of scripts/ios/env.sh instead.
   A library that travels with the file (the plug-in's Turnip) is named with --local and read with llvm-nm.
3. Every undefined symbol is bound to one of those libraries (two-level namespace) and that library, or one it
   re-exports, exports it for arm64e-ios. A strong symbol with no library (-undefined dynamic_lookup) fails:
   dyld refuses the file at load. Weak ones are counted (NULL when absent).
4. It is signed, with 4 KiB signature pages (scripts/ios/ios-sign.py).

Prints a report per file and exits 0 only when every check of every file passes. Needs llvm-nm, llvm-objdump
(on macOS Xcode's, through xcrun) and PyYAML. From macos_helios300's macho-check.py (the same check for macOS x86_64).
"""
import argparse
import os
import re
import shutil
import struct
import subprocess
import sys

import yaml

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
FILETYPES = {2: 'EXECUTE', 6: 'DYLIB', 8: 'BUNDLE'}
Loader = getattr(yaml, 'CSafeLoader', yaml.SafeLoader)


class TbdLoader(Loader):
    pass


TbdLoader.add_multi_constructor('!', lambda l, s, n: l.construct_mapping(n, deep=True))


def run(*a):
    if sys.platform == 'darwin' and not shutil.which(a[0]):      # Xcode has llvm-nm and llvm-objdump, off the PATH
        a = ('xcrun', *a)
    return subprocess.run(a, check=True, capture_output=True, text=True).stdout


def header(path):
    with open(path, 'rb') as f:
        data = f.read()
    magic, cputype, sub, filetype, ncmds, _, _, _ = struct.unpack_from('<IIIIIIII', data, 0)
    if magic != 0xFEEDFACF:
        return None
    info = {'arch': {(0x0100000C, 2): 'arm64e', (0x0100000C, 0): 'arm64'}.get((cputype, sub & 0xFFFFFF), 'cpu-%#x' % cputype),
            'type': FILETYPES.get(filetype, str(filetype)), 'platform': None, 'minos': None, 'sdk': None,
            'signature': None, 'bytes': len(data)}
    at = 32
    for _ in range(ncmds):
        cmd, size = struct.unpack_from('<II', data, at)
        if cmd == 0x32:
            platform, minos, sdk = struct.unpack_from('<III', data, at + 8)
            info.update(platform=platform, minos='%d.%d' % (minos >> 16, minos >> 8 & 255), sdk='%d.%d' % (sdk >> 16, sdk >> 8 & 255))
        elif cmd == 0x1D:
            offset, length = struct.unpack_from('<II', data, at + 8)
            info['signature'] = code_directory(data[offset:offset + length])
        at += size
    return info


def code_directory(sig):
    """(hash type, page size) of the first code directory of an embedded signature, or None."""
    if len(sig) < 12 or struct.unpack_from('>I', sig, 0)[0] != 0xFADE0CC0:
        return None
    for i in range(struct.unpack_from('>I', sig, 8)[0]):
        _, offset = struct.unpack_from('>II', sig, 12 + 8 * i)
        if struct.unpack_from('>I', sig, offset)[0] == 0xFADE0C02:
            return sig[offset + 37], 1 << sig[offset + 39]
    return None


def tbd_docs(path, target):
    with open(path) as f:
        text = f.read()
    if text.lstrip().startswith('{'):
        sys.exit(f'{path}: TBD v5 (JSON) is not handled')
    for d in yaml.load_all(text, Loader=TbdLoader):
        if not d:
            continue
        mine = lambda e: any(t == target for t in e.get('targets', []))
        syms, reex = set(), []
        for e in d.get('reexported-libraries', []) or []:
            if mine(e):
                reex += e.get('libraries', [])
        for key in ('exports', 'reexports'):
            for e in d.get(key, []) or []:
                if not mine(e):
                    continue
                for kind in ('symbols', 'weak-symbols', 'thread-local-symbols'):
                    syms.update(e.get(kind, []) or [])
                for c in e.get('objc-classes', []) or []:
                    syms.add('_OBJC_CLASS_$_' + c)
                    syms.add('_OBJC_METACLASS_$_' + c)
                for c in e.get('objc-eh-types', []) or []:
                    syms.add('_OBJC_EHTYPE_$_' + c)
                for c in e.get('objc-ivars', []) or []:
                    syms.add('_OBJC_IVAR_$_' + c)
        yield d['install-name'], syms, reex


def stub_for(root, install_name):
    base = install_name[:-len('.dylib')] if install_name.endswith('.dylib') else install_name
    cands = [root + base + '.tbd']
    m = re.match(r'(.*/([^/]+)\.framework)/Versions/[^/]+/([^/]+)$', install_name)
    if m:
        cands.append(root + m.group(1) + '/' + m.group(3) + '.tbd')
    return next((p for p in cands if os.path.exists(p)), None)


def check(path, root, target, arch, local):
    ok = True
    info = header(path)
    print(f'file: {path}')
    if info is None or info['type'] not in FILETYPES.values():
        print('FAIL not a thin 64-bit Mach-O executable, dylib or bundle')
        return False
    print(f'  Mach-O {info["arch"]} {info["type"]}, {info["bytes"]} bytes, platform {info["platform"]} minos {info["minos"]} sdk {info["sdk"]}')
    if info['arch'] != arch:
        print(f'FAIL {info["arch"]}, not {arch}')
        ok = False
    if info['platform'] != 2:
        print('FAIL not built for iOS (LC_BUILD_VERSION platform 2)')
        ok = False
    if info['signature'] is None:
        print('FAIL not signed (scripts/ios/ios-sign.py)')
        ok = False
    elif info['signature'] != (2, 4096):
        print(f'FAIL signature hash type {info["signature"][0]}, pages of {info["signature"][1]} bytes (want SHA-256, 4096)')
        ok = False
    else:
        print('  signed: SHA-256 code directory, 4096-byte pages')
    lc = run('llvm-objdump', '--macho', '--private-headers', path)
    own = re.search(r'cmd LC_ID_DYLIB.*?name (\S+)', lc, re.S)
    used = re.findall(r'cmd LC_(?:LOAD|LOAD_WEAK|REEXPORT|LOAD_UPWARD)_DYLIB\n\s+cmdsize \d+\n\s+name (\S+)', lc)
    if own:
        print(f'  install name {own.group(1)}')
    exports, short = {}, {}

    def load(name):
        if name in exports:
            return
        exports[name] = (set(), [])
        if name in local:
            syms = {l.split()[-1] for l in run('llvm-nm', '-gU', local[name]).splitlines() if l.strip()}
            exports[name] = (syms, [])
            return
        stub = stub_for(root, name)
        if stub:
            for n, s, r in tbd_docs(stub, target):
                exports[n] = (s, r)

    for u in used:
        short[os.path.basename(u).split('.')[0]] = u
        load(u)
        if u in local:
            print(f'  links {u} (travels with it: {local[u]})')
        elif stub_for(root, u) is None:
            print(f'FAIL {u}: no stub in {root}')
            ok = False
        else:
            print(f'  links {u}')

    def provides(name, sym, seen=()):
        if name in seen:
            return False
        load(name)
        s, r = exports.get(name, (set(), []))
        return sym in s or any(provides(x, sym, seen + (name,)) for x in r)

    bad, flat, weakflat, bound, n = [], [], [], 0, 0
    for l in run('llvm-nm', '-m', '--undefined-only', path).splitlines():
        if not l.strip():
            continue
        n += 1
        mm = re.match(r'\s*\(undefined\)\s+(weak )?external (\S+)(?: \(from (\S+)\))?(?: \(dynamically looked up\))?', l)
        if not mm:
            bad.append('unparsed: ' + l)
            continue
        weak, sym, lib = mm.group(1), mm.group(2), mm.group(3)
        if lib is None:
            (weakflat if weak else flat).append(sym)
        elif short.get(lib) is None or not provides(short[lib], sym):
            if weak:
                weakflat.append(sym)
            else:
                bad.append(f'{sym} (from {lib}) is not exported by its stub')
        else:
            bound += 1
    print(f'  {n} undefined symbols: {bound} bound to a library whose stub exports them ({target}), '
          f'{len(weakflat)} weak and absent or looked up dynamically (NULL when absent)')
    for s in flat:
        print(f'FAIL strong symbol with no library (dynamic_lookup): {s}')
        ok = False
    for b in bad:
        print(f'FAIL {b}')
        ok = False
    print('PASS' if ok else 'FAILED')
    return ok


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--tbd', default=os.environ.get('IOS_TBD') or os.path.join(REPO, 'out/ios27-tbd'))
    p.add_argument('--sdk', action='store_true', help='check against the SDK\'s stubs, not the iOS 27 ones')
    p.add_argument('--local', action='append', default=[], metavar='FILE', help='a dylib that travels with the files')
    p.add_argument('--arch', default='arm64e')
    p.add_argument('files', nargs='+')
    a = p.parse_args()
    root = a.tbd
    if a.sdk:
        root = subprocess.run(['sh', '-c', f'. "{REPO}/scripts/ios/env.sh" && printf %s "$IOS_SDK"'], check=True,
                              capture_output=True, text=True).stdout
    if not os.path.exists(os.path.join(root, 'usr/lib/libSystem.B.tbd')):
        sys.exit(f'macho-check: no stubs in {root} (scripts/ios/make-tbds.py makes the iOS 27 ones; --sdk uses the SDK\'s)')
    print(f'stubs: {root}')
    local = {}
    for f in a.local:
        own = re.search(r'cmd LC_ID_DYLIB.*?name (\S+)', run('llvm-objdump', '--macho', '--private-headers', f), re.S)
        if not own:
            sys.exit(f'macho-check: --local {f}: not a dylib')
        local[own.group(1)] = f
    results = [check(f, root, a.arch + '-ios', a.arch, local) for f in a.files]
    return 0 if all(results) else 1


if __name__ == '__main__':
    sys.exit(main())
