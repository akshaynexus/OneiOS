#!/usr/bin/env python3
"""The build record of a OneiOS XNU build, and its place in out/kernel.

  build-record.py finish --host linux|macos --work DIR --config NAME --out OUT
      DIR is UniXNU's work directory of the build (out/kernel/unixnu/oneios/<os profile>/<config>): its
      kernel/kernel.macho (the native kernel, --skip-prelink), xnu-inputs.json (source commit and every patch with
      its SHA-256) and .config. Writes OUT/builds/<build id>/{xnu.macho, xnu.macho.build.json, series.txt} and
      points OUT/current at it.
  build-record.py --self-test

The record is what runtime/package/pack.py reads before it lets a Mach-O into a release (docs/releases.md):
builder, sha256, prelinked, kernelcache_kexts_added, apple_binary_inputs, restricted_sources.
The build id names the inputs, not the machine: xnu-<version>-<profile>-<12 hex>, the hex being a hash of the XNU
commit, every patch's SHA-256 in order, the configuration and the UniXNU commit.
"""
import argparse
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
from pathlib import Path

BUILDER = 'oneios-kernel'
ROOT = Path(__file__).resolve().parents[1]
# Sources that may be linked into a local build but not into a release (their licence does not allow redistribution).
RESTRICTED = {'XNU_SOURCE_CORECRYPTO14': 'corecrypto-2023 (github.com/ONLYA/corecrypto-2023: Apple corecrypto, Internal Use License)'}


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def macho_facts(path):
    """Type, architecture, segments and entry of a 64-bit Mach-O."""
    data = Path(path).read_bytes()
    magic, cputype, cpusubtype, filetype, ncmds = struct.unpack_from('<IiiII', data)
    if magic != 0xfeedfacf:
        raise SystemExit(f'{path}: not a 64-bit Mach-O')
    facts = {'filetype': filetype, 'cputype': cputype & 0xffffffff, 'cpusubtype': cpusubtype & 0xffffff, 'segments': {}, 'entry': None,
             'chained_fixups': False}
    offset = 32
    for _ in range(ncmds):
        command, size = struct.unpack_from('<II', data, offset)
        if command == 0x19:      # LC_SEGMENT_64
            name = data[offset + 8:offset + 24].rstrip(b'\0').decode()
            vmaddr, vmsize = struct.unpack_from('<QQ', data, offset + 24)
            facts['segments'][name] = {'vmaddr': hex(vmaddr), 'vmsize': hex(vmsize)}
        elif command == 0x5:     # LC_UNIXTHREAD: flavor, count, then ARM_THREAD_STATE64 (x0-x28, fp, lr, sp, pc)
            flavor, _count = struct.unpack_from('<II', data, offset + 8)
            if flavor == 6:
                facts['entry'] = hex(struct.unpack_from('<Q', data, offset + 16 + 32 * 8)[0])
        elif command == 0x80000034:   # LC_DYLD_CHAINED_FIXUPS
            facts['chained_fixups'] = True
        offset += size
    return facts


def parse_config(text):
    result = {}
    for line in text.splitlines():
        line = line.strip()
        if line and not line.startswith('#') and '=' in line:
            key, value = line.split('=', 1)
            result[key.removeprefix('CONFIG_')] = value.strip('"')
    return result


def git_head(path):
    try:
        return subprocess.run(['git', '-C', str(path), 'rev-parse', 'HEAD'], capture_output=True, text=True, check=True).stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def relative_patch(path):
    """A patch path as a reader finds it: relative to the repository (external/..., kernel/...)."""
    path = Path(path).resolve()
    try:
        return str(path.relative_to(ROOT))
    except ValueError:
        return str(path)


def build_id(xnu_version, profile, xnu_commit, patches, config, unixnu_commit, unixnu_patches):
    digest = hashlib.sha256()
    digest.update(json.dumps({'xnu': xnu_commit, 'patches': [p['sha256'] for p in patches], 'config': sorted(config.items()),
                              'unixnu': unixnu_commit, 'unixnu_patches': unixnu_patches}, sort_keys=True).encode())
    return f'xnu-{xnu_version}-{profile}-{digest.hexdigest()[:12]}'


def tool_version(command):
    try:
        out = subprocess.run(command, capture_output=True, text=True).stdout + subprocess.run(command, capture_output=True, text=True).stderr
        return out.strip().splitlines()[0] if out.strip() else None
    except FileNotFoundError:
        return None


def make_record(kernel, inputs, config, profile, host, config_name, unixnu, native_record=None):
    facts = macho_facts(kernel)
    if facts['filetype'] != 2:
        raise SystemExit(f'{kernel}: Mach-O type {facts["filetype"]}, not MH_EXECUTE: this is not the native kernel')
    if native_record is not None and (native_record.get('kernelcache_kexts_added') is not False or not native_record.get('prelink_skipped')):
        raise SystemExit(f'{kernel}: UniXNU\'s record says it was prelinked; the record is only written for the native kernel')
    patches = [{'path': relative_patch(p['path']), 'sha256': p['sha256']} for p in inputs['patches']]
    unixnu_commit = git_head(unixnu)
    unixnu_patches = [{'path': relative_patch(p), 'sha256': sha256(p)} for p in sorted((ROOT / 'kernel/patches/unixnu').glob('*.patch'))]
    xnu_version = config.get('_XNU_VERSION') or '12377.121.6'
    restricted = [text for key, text in RESTRICTED.items() if config.get(key) in ('y', '1')]
    return {
        'builder': BUILDER,
        'build_id': build_id(xnu_version, profile, inputs['commit'], patches, {k: v for k, v in config.items() if not k.startswith('_')},
                             unixnu_commit, [p['sha256'] for p in unixnu_patches]),
        'sha256': sha256(kernel),
        'bytes': os.path.getsize(kernel),
        'xnu': f'xnu-{xnu_version}',
        'xnu_commit': inputs['commit'],
        'profile': profile,
        'arch': 'arm64e' if facts['cpusubtype'] == 2 else 'arm64',
        'machine_config': 'BCM2837',
        'kernel_config': 'DEVELOPMENT',
        'defconfig': config_name,
        'config': {k: v for k, v in sorted(config.items()) if not k.startswith('_')},
        'macho': {'filetype': 'MH_EXECUTE', 'entry': facts['entry'], 'chained_fixups': facts['chained_fixups'],
                  'segments': facts['segments']},
        'prelinked': False,
        'kernelcache_kexts_added': False,
        'apple_binary_inputs': [],
        'restricted_sources': restricted,
        'releasable': not restricted,
        'sources': {'unixnu': unixnu_commit,
                    'unixnu_droidvm': git_head(ROOT / 'external/UniXNU-droidvm'),
                    'xnu_linux_toolchain': git_head(ROOT / 'external/xnu-linux-toolchain') if host == 'linux' else None,
                    'oneios': git_head(ROOT)},
        'unixnu_patches': unixnu_patches,
        'patches': patches,
        'host': {'os': host, 'compiler': tool_version(['clang', '--version']),
                 'linker': tool_version([str(ROOT / 'out/toolchain/cctools/bin/ld'), '-v']) if host == 'linux' else tool_version(['ld', '-v'])},
        'boot_verified': False,
    }


def finish(args):
    work = Path(args.work)
    kernel = work / 'kernel/kernel.macho'
    if not kernel.is_file():
        raise SystemExit(f'{kernel} missing: the kernel step did not finish')
    inputs = json.loads((work / 'xnu-inputs.json').read_text())
    config = parse_config((work / '.config').read_text())
    native = json.loads((work / 'kernel/kernel.json').read_text()) if (work / 'kernel/kernel.json').is_file() else None
    record = make_record(kernel, inputs, config, work.parent.name, args.host, args.config, ROOT / 'external/Droid-VM-UniXNU', native)
    target = Path(args.out) / 'builds' / record['build_id']
    target.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(kernel, target / 'xnu.macho')
    (target / 'xnu.macho.build.json').write_text(json.dumps(record, indent=2) + '\n')
    (target / 'series.txt').write_text(''.join(f'{p["sha256"]}  {p["path"]}\n' for p in record['patches']))
    current = Path(args.out) / 'current'
    if current.is_symlink() or current.exists():
        current.unlink()
    current.symlink_to(Path('builds') / record['build_id'])
    print(f'kernel: {target / "xnu.macho"}')
    print(f'kernel: build id {record["build_id"]}, {record["arch"]}, {record["bytes"]} bytes, sha256 {record["sha256"]}')
    print(f'kernel: entry {record["macho"]["entry"]}, __TEXT at {record["macho"]["segments"].get("__TEXT", {}).get("vmaddr")}, '
          f'{len(record["patches"])} patches')
    if record['restricted_sources']:
        print('kernel: NOT releasable (runtime/package.sh refuses it): links ' + '; '.join(record['restricted_sources']))


def self_test():
    import tempfile
    with tempfile.TemporaryDirectory() as directory:
        kernel = Path(directory) / 'k'
        # header, one LC_SEGMENT_64 (__TEXT) and one LC_UNIXTHREAD with pc
        segment = struct.pack('<II16sQQQQIIII', 0x19, 72, b'__TEXT', 0xfffffff007004000, 0x4000, 0, 0x4000, 5, 5, 0, 0)
        thread = struct.pack('<IIII', 0x5, 16 + 34 * 8, 6, 68) + struct.pack('<33Q', *([0] * 32), 0xfffffff007008000) + struct.pack('<II', 0, 0)
        kernel.write_bytes(struct.pack('<IIIIIIII', 0xfeedfacf, 0x0100000c, 2 | (0x80 << 24), 2, 2, len(segment) + len(thread), 0, 0) + segment + thread)
        patch = Path(directory) / 'a.patch'
        patch.write_text('x')
        inputs = {'commit': 'c' * 40, 'patches': [{'path': str(patch), 'sha256': sha256(patch)}]}
        one = make_record(kernel, inputs, {'XNU_ARMCPU': 'y'}, 'ios-test', 'linux', 't_defconfig', Path(directory))
        assert one['arch'] == 'arm64e' and one['macho']['entry'] == '0xfffffff007008000', one['macho']
        assert one['macho']['segments']['__TEXT']['vmaddr'] == '0xfffffff007004000'
        assert one['releasable'] and one['build_id'].startswith('xnu-12377.121.6-ios-test-')
        two = make_record(kernel, inputs, {'XNU_ARMCPU': 'y', 'XNU_SOURCE_CORECRYPTO14': 'y'}, 'ios-test', 'linux', 't_defconfig', Path(directory))
        assert not two['releasable'] and two['build_id'] != one['build_id']
        again = make_record(kernel, inputs, {'XNU_ARMCPU': 'y'}, 'ios-test', 'macos', 't_defconfig', Path(directory))
        assert again['build_id'] == one['build_id'], 'the build id must not depend on the host'
        fileset = Path(directory) / 'f'
        fileset.write_bytes(struct.pack('<IIIIIIII', 0xfeedfacf, 0x0100000c, 2, 12, 0, 0, 0, 0))
        try:
            make_record(fileset, inputs, {}, 'ios-test', 'linux', 't', Path(directory))
        except SystemExit:
            pass
        else:
            raise AssertionError('a kernel collection got a record')
    print('build-record: self-test passed')


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('command', nargs='?', choices=['finish'])
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--host', choices=['linux', 'macos'])
    parser.add_argument('--work')
    parser.add_argument('--config')
    parser.add_argument('--out')
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if args.command != 'finish' or not (args.host and args.work and args.config and args.out):
        parser.error('finish needs --host, --work, --config and --out')
    finish(args)


if __name__ == '__main__':
    main()
