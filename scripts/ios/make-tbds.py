#!/usr/bin/env python3
"""Make .tbd stubs of the guest's own iOS 27 libraries, from the dyld shared cache of the image set.

  scripts/ios/make-tbds.py [--cache DSC] [--out DIR] [--jobs N] [NAME ...]

Without NAME every image of the cache gets a stub (4696 in 24A446; about 6 minutes on 20 threads, 1.5 GB);
with names (install names, or their last component: Metal, libobjc.A.dylib) only those. Stubs that exist are kept.

  --cache   the main cache file. Default: out/ios27-cache/dyld_shared_cache_arm64e when it is there (a copy of
            /System/Library/Caches/com.apple.dyld of systemos.img); otherwise out/image/24A446/systemos.img is
            mounted for the run and unmounted after (Linux: apfs-fuse; macOS: hdiutil, read-only, no root).
  --out     default out/ios27-tbd. Laid out like an SDK: /usr/lib/libSystem.B.dylib -> usr/lib/libSystem.B.tbd,
            /System/Library/Frameworks/Metal.framework/Metal -> System/Library/Frameworks/Metal.framework/Metal.tbd.

The stubs are derived from Apple's binaries: they stay in out/ and are never committed. `ipsw dyld tbd`
(out/bin/ipsw, scripts/fetch/get-ipsw-cli.sh) writes each one; out/ios27-tbd/tbd.json records the cache they are from.
"""
import argparse
import concurrent.futures
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def stub_path(out, install_name):
    rel = install_name.lstrip('/')
    if rel.endswith('.dylib'):
        rel = rel[:-len('.dylib')]
    return os.path.join(out, rel + '.tbd')


def make(ipsw, cache, out, name):
    target = stub_path(out, name)
    if os.path.exists(target):
        return None
    with tempfile.TemporaryDirectory(dir=os.path.join(out, '.tmp')) as tmp:
        r = subprocess.run([ipsw, 'dyld', 'tbd', cache, name, '-o', tmp], capture_output=True, text=True)
        made = os.listdir(tmp)
        if r.returncode or len(made) != 1:
            return f'{name}: {(r.stderr or r.stdout).strip().splitlines()[-1:] or "no stub written"}'
        os.makedirs(os.path.dirname(target), exist_ok=True)
        os.replace(os.path.join(tmp, made[0]), target)
    return None


def aliases(out):
    """-lSystem looks for libSystem.tbd: a link beside each versioned stub of usr/lib (libSystem.B, libc++.1, libz.1.2.x), as an SDK has."""
    lib = os.path.join(out, 'usr/lib')
    for name in sorted(os.listdir(lib)) if os.path.isdir(lib) else []:
        m = re.fullmatch(r'(lib[^.]+(?:\+\+)?)((?:\.[A-Z0-9]+)+)\.tbd', name)
        if m and not os.path.lexists(os.path.join(lib, m.group(1) + '.tbd')):
            os.symlink(name, os.path.join(lib, m.group(1) + '.tbd'))


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--cache')
    p.add_argument('--out', default=os.path.join(REPO, 'out/ios27-tbd'))
    p.add_argument('--image', default=os.path.join(REPO, 'out/image/24A446/systemos.img'))
    p.add_argument('--jobs', type=int, default=os.cpu_count())
    p.add_argument('names', nargs='*')
    a = p.parse_args()
    ipsw = os.path.join(REPO, 'out/bin/ipsw')
    if not os.access(ipsw, os.X_OK):
        sys.exit('make-tbds: no out/bin/ipsw (scripts/fetch/get-ipsw-cli.sh)')
    cache, mount = a.cache, None
    copy = os.path.join(REPO, 'out/ios27-cache/dyld_shared_cache_arm64e')
    if not cache and os.path.isfile(copy):
        cache = copy
    os.makedirs(os.path.join(a.out, '.tmp'), exist_ok=True)
    try:
        if not cache:
            if not os.path.isfile(a.image):
                sys.exit(f'make-tbds: no {a.image} (build the image set first, or give --cache)')
            mount = tempfile.mkdtemp(prefix='mnt-', dir=os.path.join(REPO, 'out'))
            if sys.platform == 'darwin':
                subprocess.run(['hdiutil', 'attach', '-readonly', '-nobrowse', '-noverify', '-noautoopen', '-owners', 'off',
                                '-imagekey', 'diskimage-class=CRawDiskImage', '-mountpoint', mount, a.image],
                               check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                volume = mount
            else:
                subprocess.run(['apfs-fuse', '-o', 'ro', a.image, mount], check=True)
                volume = os.path.join(mount, 'root')
            cache = os.path.join(volume, 'System/Library/Caches/com.apple.dyld/dyld_shared_cache_arm64e')
        listing = json.loads(subprocess.run([ipsw, 'dyld', 'info', '--dylibs', '--json', cache], check=True,
                                            capture_output=True, text=True).stdout)
        images = [d['name'] for d in listing['dylibs']]
        names = images
        if a.names:
            names = []
            for want in a.names:
                hits = [i for i in images if i == want or os.path.basename(i) == want]
                if len(hits) != 1:
                    sys.exit(f'make-tbds: {want}: {len(hits)} images of the cache have that name')
                names += hits
        failed = []
        with concurrent.futures.ThreadPoolExecutor(a.jobs) as pool:
            for n, problem in enumerate(pool.map(lambda name: make(ipsw, cache, a.out, name), names), 1):
                if problem:
                    failed.append(problem)
                if n % 500 == 0:
                    print(f'make-tbds: {n} of {len(names)}', flush=True)
        aliases(a.out)
        record = {'cache': os.path.relpath(cache, REPO) if not mount else 'systemos.img',
                  'uuid': listing.get('uuid'), 'images_in_cache': len(images),
                  'stubs': sum(len([f for f in files if f.endswith('.tbd')]) for _, _, files in os.walk(a.out)),
                  'ipsw': subprocess.run([ipsw, 'version'], capture_output=True, text=True).stdout.strip()}
        with open(os.path.join(a.out, 'tbd.json'), 'w') as f:
            json.dump(record, f, indent=2)
            f.write('\n')
        for problem in failed[:20]:
            print('make-tbds: failed:', problem, file=sys.stderr)
        print(f'make-tbds: {record["stubs"]} stubs in {a.out} ({len(failed)} failed)')
        return 1 if failed else 0
    finally:
        if mount:
            subprocess.run(['hdiutil', 'detach', '-quiet', mount] if sys.platform == 'darwin' else ['fusermount3', '-u', mount])
            if not os.path.ismount(mount):
                shutil.rmtree(mount, ignore_errors=True)


if __name__ == '__main__':
    sys.exit(main())
