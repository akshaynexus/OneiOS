"""roottweak: the command the image build runs.

  python3 -I -m roottweak apply --root OS_TREE --data DATA_TREE --out OUT_DIR [--layout sealed|rw] [options]
  python3 -I -m roottweak daemons [--group NAME ...]         the daemon policy, as text
  python3 -I -m roottweak cdhash FILE ...                    CDHash and signature facts of signed Mach-O files

apply writes into the trees and, in OUT_DIR:
  tweaks.manifest.json   every path changed, with the owner and mode the guest needs (the build may not be root)
  boot-contract.json     what the booter and the kernel must provide (device tree, launch policy, mounts)
  owner-trust.json       CDHashes of our own binaries, for the owner trust cache
  report.txt, report.json  one line per item: applied, kept, not-found, off, contract
"""
import argparse
import json
import os
import sys

from . import SCHEMA, config, macho, tweaks
from .tree import ROOT, Manifest, Tree, TreeError


def _apply(a):
    manifest = Manifest()
    layout = a.layout
    root = Tree(a.root, 'root', manifest, readonly=layout == 'sealed')
    if a.data:
        data = Tree(a.data, 'data', manifest)
    elif layout == 'rw':
        # one volume: the Data tree is the root's /private/var
        data = Tree(root.host('private/var'), 'data', manifest)
    else:
        raise TreeError('--layout sealed needs --data DIR, the tree that becomes the Data volume (/private/var)')
    if os.path.commonpath([data.top, root.top]) == data.top:
        raise TreeError('--data must not contain --root')
    os.makedirs(a.out, exist_ok=True)
    # A run over trees an earlier run changed makes less itself (a directory is there already), but the image
    # build needs every owner: what the earlier manifest says of paths that still exist is carried over.
    earlier = os.path.join(a.out, 'tweaks.manifest.json')
    if os.path.isfile(earlier):
        with open(earlier) as f:
            old = json.load(f)
        if old.get('schema') == SCHEMA and old.get('layout') == layout:
            trees = {'root': root, 'data': data}
            for e in old.get('entries', []):
                tree = trees.get(e.get('volume'))
                if tree and e.get('type') in ('file', 'dir', 'link') and tree.lexists(e['path']):
                    manifest.entries[(e['volume'], e['path'])] = e
    options = {
        'build': a.build, 'any_version': a.any_version, 'groups': a.group, 'without_groups': a.no_group, 'keep': a.keep,
        'disable': a.disable, 'keep_recover_on_crash': a.keep_recover_on_crash, 'helpers': a.helpers,
        'metal_plugin': a.metal_plugin, 'metal_clients': a.metal_client, 'appos': a.appos,
        'no_prefs': [g for g in ('setup', 'migration', 'watchdogs', 'awake', 'coreanimation') if getattr(a, f'no_{g}')],
        'lighter_visuals': a.lighter_visuals, 'plain_keybag': a.plain_keybag,
    }
    cx = tweaks.Context(root, data, layout, config.device(a.config), config.daemons(a.config), config.prefs(a.config), options)
    for tweak in tweaks.ALL:
        cx.run(tweak)

    lines = [f'{r["state"]:9} {r["item"]}: {r["note"]}' for r in cx.results]
    report = '\n'.join([f'roottweak schema {SCHEMA}, layout {layout}, build {cx.build}'] + lines) + '\n'
    data.tweak = 'report'
    data.write(f'{tweaks.ONEIOS}/roottweak-report.txt', report.encode(), ROOT, 0o644)

    contract = {'schema': SCHEMA, 'layout': layout, 'build': cx.build, 'data_mount': '/private/var', **cx.contract}
    trust = {'schema': SCHEMA, 'what': 'CDHashes of OneiOS\'s own Mach-O files in the guest; add them as one owner module of '
             'the trust cache bundle (UniXNU-droidvm ipados27/scripts/build-droidvm-trust.py --owner reads the "cdhashes" list)',
             'cdhashes': sorted({t['cdhash'] for t in cx.trusted}), 'files': cx.trusted}
    outputs = {
        'tweaks.manifest.json': {'schema': SCHEMA, 'layout': layout,
                                 'volumes': {'root': 'the OS volume (/)', 'data': 'the Data volume (/private/var)'},
                                 'entries': manifest.sorted()},
        'boot-contract.json': contract,
        'owner-trust.json': trust,
        'report.json': {'schema': SCHEMA, 'layout': layout, 'build': cx.build, 'results': cx.results},
    }
    for name, value in outputs.items():
        with open(os.path.join(a.out, name), 'w') as f:
            json.dump(value, f, indent=2, sort_keys=True)
            f.write('\n')
    with open(os.path.join(a.out, 'report.txt'), 'w') as f:
        f.write(report)
    sys.stdout.write(report)
    off = [r for r in cx.results if r['state'] in ('off', 'not-found') or r['item'].startswith('warning')]
    if off:
        print(f'\n{len(off)} items are off, not found or warnings: read them above.')
    return 0


def _daemons(a):
    d = config.daemons(a.config)
    chosen = d.selected(a.group, a.no_group, a.keep)
    print(f'keep ({len(d.keep)}): never disabled')
    for label in sorted(d.keep):
        print(f'  {label}' + (f'  # {d.keep_reasons[label]}' if label in d.keep_reasons else ''))
    print(f'\nidle ({len(d.idle)}): kept running without their hardware')
    for label, why in sorted(d.idle.items()):
        print(f'  {label}  # {why}')
    print(f'\nreplace ({len(d.replace)}): disabled, done by the host')
    for label, why in sorted(d.replace.items()):
        print(f'  {label}  # {why}')
    for name, group in sorted(d.groups.items()):
        on = all(label in chosen for label in group['labels'])
        print(f'\ngroup {name} ({len(group["labels"])}, {"on" if on else "off"}): {group["source"]}')
        for label in group['labels']:
            print(f'  {label}')
    print(f'\n{len(chosen)} jobs disabled with this selection')
    return 0


def _cdhash(a):
    status = 0
    for file in a.files:
        try:
            with open(file, 'rb') as f:
                for s in macho.describe(f.read()):
                    print(f'{s["cdhash"]} {s["arch"]} pages={s["page_size"]} adhoc={int(s["adhoc"])} '
                          f'platform={s["platform"]} minos={s["minos"]} id={s["identifier"]} {file}')
        except (OSError, macho.MachOError) as e:
            print(f'{file}: {e}', file=sys.stderr)
            status = 1
    return status


def main(argv=None):
    p = argparse.ArgumentParser(prog='roottweak', description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest='command', required=True)

    def policy(s):
        s.add_argument('--config', help='another userland/config directory')
        s.add_argument('--group', action='append', default=[], metavar='NAME', help='also disable this daemon group')
        s.add_argument('--no-group', action='append', default=[], metavar='NAME', help='do not disable this group (or "replace")')
        s.add_argument('--keep', action='append', default=[], metavar='LABEL', help='keep this job although a group names it')

    s = sub.add_parser('apply', help='apply the tweaks to an extracted root and its Data tree')
    s.add_argument('--root', required=True, help='the extracted OS volume (/)')
    s.add_argument('--data', help='the tree that becomes the Data volume (/private/var); with --layout rw default ROOT/private/var')
    s.add_argument('--out', required=True, help='where the manifest, the boot contract and the report go')
    s.add_argument('--layout', choices=('sealed', 'rw'), default='sealed',
                   help='sealed: the OS volume is Apple\'s image, nothing is written to it (default). rw: a root we write')
    policy(s)
    s.add_argument('--disable', action='append', default=[], metavar='LABEL', help='disable this job, even one the policy keeps')
    s.add_argument('--build', help='the build number, where SystemVersion.plist has none')
    s.add_argument('--any-version', action='store_true', help='apply to a root that is not the profile\'s iOS version')
    s.add_argument('--helpers', metavar='DIR', help='signed guest binaries: the output of userland/helpers/build.sh')
    s.add_argument('--metal-plugin', metavar='BUNDLE', help='a built, signed Metal plugin bundle (NativeAdrenoMetal.bundle)')
    s.add_argument('--metal-client', action='append', default=[], metavar='NAME', help='a process that gets the Metal device ("*": all)')
    s.add_argument('--appos', metavar='DIR', help='the extracted Cryptex1,AppOS volume')
    for group in ('setup', 'migration', 'watchdogs', 'awake', 'coreanimation'):
        s.add_argument(f'--no-{group}', action='store_true', help=f'leave the "{group}" preferences out')
    s.add_argument('--lighter-visuals', action='store_true', help='Reduce Motion and Reduce Transparency on')
    s.add_argument('--plain-keybag', action='store_true', help='write ios-sweet\'s plain systembag.kb (only for a kernel that reads one)')
    s.add_argument('--keep-recover-on-crash', action='store_true', help='rw: leave RecoverOnCrash in the jetsam properties')
    s.set_defaults(run=_apply)

    s = sub.add_parser('daemons', help='print the daemon policy')
    policy(s)
    s.set_defaults(run=_daemons)

    s = sub.add_parser('cdhash', help='CDHash of signed Mach-O files')
    s.add_argument('files', nargs='+')
    s.set_defaults(run=_cdhash)

    a = p.parse_args(argv)
    try:
        return a.run(a)
    except (TreeError, config.ConfigError) as e:
        print(f'roottweak: {e}', file=sys.stderr)
        return 1
