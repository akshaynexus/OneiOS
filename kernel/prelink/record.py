#!/usr/bin/env python3
"""The build record of kernel.kc (kernel.kc.build.json): inputs, tools, what went in, what was left out and why."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def git(path, *command):
    try:
        return subprocess.check_output(['git', '-C', str(path), *command], text=True, stderr=subprocess.DEVNULL).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def fileset_entries(data):
    ncmds, = struct.unpack_from('<I', data, 16)
    off, out = 32, []
    for _ in range(ncmds):
        cmd, length = struct.unpack_from('<II', data, off)
        if cmd == 0x80000035:
            va, fileoff, name_at = struct.unpack_from('<QQI', data, off + 8)
            out.append({'name': data[off + name_at:data.index(b'\0', off + name_at)].decode(), 'address': hex(va),
                        'file_offset': hex(fileoff)})
        off += length
    return out


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('work', 'kernel', 'kernelcache', 'reviewed', 'xnuprelink', 'output'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--reference')
    args = parser.parse_args()
    work = Path(args.work)
    select = json.loads((work / 'select.json').read_text())
    hosted = json.loads((work / 'hosted.json').read_text())
    prelink = json.loads((work / 'kernel.kc.prelink.json').read_text())
    pairing = json.loads((work / 'match.map.json').read_text())
    reviewed = json.loads(Path(args.reviewed).read_text())
    image = (work / 'kernel.kc').read_bytes()
    entries = fileset_entries(image)
    root = Path(__file__).resolve().parents[2]
    stubbed = sorted({target for targets in prelink['stubbed'].values() for target in targets})
    missing = sorted({target for targets in prelink['missing'].values() for target in targets})
    record = {
        'output': 'kernel.kc', 'format': 'MH_FILESET', 'bytes': len(image), 'sha256': hashlib.sha256(image).hexdigest(),
        'releasable': False,
        'why_not_releasable': 'contains the kexts of Apple\'s kernelcache (and the kernel links UniXNU\'s source corecrypto)',
        'inputs': {
            'kernel': {'path': str(Path(args.kernel).resolve()), 'sha256': sha(args.kernel)},
            'kernelcache': {'path': str(Path(args.kernelcache).resolve()), 'sha256': sha(args.kernelcache),
                            'what': 'iPhone18,2 iOS 27.0.1 (24A446) kernelcache.release.v54, decoded'},
            'reference_kernel': {'path': str(Path(args.reference).resolve()), 'sha256': sha(args.reference)} if args.reference else None,
            'reviewed': {'path': str(Path(args.reviewed).resolve().relative_to(root)), 'sha256': sha(args.reviewed)},
        },
        'tools': {
            'oneios': git(root, 'rev-parse', 'HEAD'),
            'xnuprelink': git(args.xnuprelink, 'rev-parse', 'HEAD'),
            'matcher_version': pairing.get('matcher_version'), 'matcher_signature': pairing.get('matcher_signature'),
        },
        'native_input': json.loads((work / 'native-input.json').read_text())['weak_binds_resolved'],
        'kexts': {
            'in_kernelcache': select['kernelcache_executables'],
            'roots': select['roots'],
            'dependencies_without_personalities': prelink['personalities_dropped'],
            'closure': select['closure'],
            'left_out': {name: entry['reason'] + ('' if 'returns_as_dependency_of' not in entry else
                                                  '; its code returns as a dependency, without its personalities')
                         for name, entry in select['left_out'].items()},
            'left_out_and_absent': sorted(set(select['left_out']) - set(select['closure'])),
        },
        'fileset_entries': entries,
        'hosted': {key: hosted[key] for key in ('convention', 'tpidr_el1_rewrites', 'mpidr_el1_rewrites', 'kernel_pointer_rewrites', 'kext_code_bytes_scanned',
                                                'privileged_instructions_total', 'privileged_instructions_by_kext',
                                                'x18_checked', 'x18_instructions_by_kext')},
        'amfi': {
            'early_security_init': prelink['early_security_init'],
            'page_shift_site': [site for site in prelink['reviewed_applied']['sites']],
            'launch_constraints': 'kernel side: zzyc-owner-skip-launch-constraints.patch (boot argument sheng-no-lc=1) '
                                  'and /chosen/amfi-only-platform-code; nothing in the collection is changed for them',
        },
        'pairing': {
            'statistics': pairing.get('stats'),
            'reviewed_applied': {key: len(value) for key, value in prelink['reviewed_applied'].items()},
            'reviewed_hand': sum(1 for r in reviewed.get('functions', []) if not r.get('ported')),
            'reviewed_ported': sum(1 for r in reviewed.get('functions', []) if r.get('ported')),
            'original_functions_bound_to_the_panic_stub': len(stubbed),
            'original_data_left_at_collection_addresses': len(missing),
            'detail': 'kernel.kc.prelink.json (stubbed, missing: per kext)',
        },
        'mapping_complete': prelink['mapping_complete'],
        'runtime_qualified': False,
    }
    Path(args.output).write_text(json.dumps(record, indent=1) + '\n')
    print(f'record: {len(entries)} fileset entries ({len(select["roots"])} roots, {len(select["closure"])} in the closure, '
          f'{len(record["kexts"]["left_out_and_absent"])} kexts absent), {len(stubbed)} original functions on the stub, '
          f'{len(missing)} data targets unpaired')


if __name__ == '__main__':
    main()
