#!/usr/bin/env python3
"""Checks and manifest of the OneiOS runtime package (called by runtime/package.sh).

  pack.py check STAGE              refuse Apple files and anything the OneOS fork's unpacker rejects
  pack.py manifest STAGE VERSION GIT_COMMIT GIT_DIRTY MIN_FORK PLACEHOLDER   write STAGE/runtime.json

The manifest is the one oneos/interface.md section 1 defines (format 1), with these keys added:
git, min_oneos_fork, placeholder, kernel.

Apple rule (README "Rules"): a release carries nothing of Apple's. Refused by name: kernelcaches, cryptexes,
.ipsw, .dmg, .aea, .img and the other firmware names of .gitignore. Refused by content: IMG4/IM4P containers,
AEA archives, disk images, and every Mach-O. The one exception is the XNU that kernel/ builds from Apple's
open source: a Mach-O with a build record NAME.build.json next to it that names this builder, carries the
file's SHA-256, says no kernelcache kexts were added and lists no source whose licence forbids redistribution. A kernel collection (MH_FILESET) is refused with or
without a record: it holds Apple's kexts and belongs to the image set.
"""
import fnmatch
import hashlib
import json
import os
import re
import stat
import struct
import sys

SEGMENT = re.compile(r'^[A-Za-z0-9_][A-Za-z0-9._+-]*$')
APPLE_NAMES = ('kernelcache*', '*cryptex*', '*.ipsw', '*.dmg', '*.aea', '*.img', '*.simg', '*.erofs', '*.im4p',
               '*.img4', '*.im4m', '*.kc', '*.trustcache', 'buildmanifest.plist', 'restore.plist', '*.shsh*',
               'dyld_shared_cache*', '*.kext', '*.dext')
MACHO_MAGICS = {b'\xcf\xfa\xed\xfe', b'\xce\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xfe\xed\xfa\xce',
                b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca', b'\xca\xfe\xba\xbf', b'\xbf\xba\xfe\xca'}
MH_EXECUTE, MH_FILESET = 2, 12
BUILDER = 'oneios-kernel'
RECORD_SUFFIX = '.build.json'
MANIFEST = 'runtime.json'


class Refused(Exception):
    pass


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def files_of(stage):
    """Relative paths of every entry below stage; refuses links and special files."""
    result = []
    for directory, names, files in os.walk(stage):
        names.sort()
        for name in sorted(names + files):
            path = os.path.join(directory, name)
            relative = os.path.relpath(path, stage)
            mode = os.lstat(path).st_mode
            if not (stat.S_ISREG(mode) or stat.S_ISDIR(mode)):
                raise Refused(f'{relative}: not a regular file or directory')
            for segment in relative.split(os.sep):
                if not SEGMENT.match(segment):
                    raise Refused(f'{relative}: path segment {segment!r} is outside [A-Za-z0-9_][A-Za-z0-9._+-]*')
            if stat.S_ISREG(mode):
                result.append(relative)
    return result


def content_kind(path):
    """What Apple container or binary the file is, or None."""
    size = os.path.getsize(path)
    with open(path, 'rb') as stream:
        head = stream.read(64)
        tail = b''
        if size >= 512:
            stream.seek(size - 512)
            tail = stream.read(4)
    if head[:4] in MACHO_MAGICS:
        return 'Mach-O'
    if head[:1] == b'\x30' and any(tag in head[:24] for tag in (b'IM4P', b'IMG4', b'IM4M', b'IM4R')):
        return 'IMG4 container'
    if head[:4] == b'AEA1':
        return 'AEA archive'
    if tail == b'koly' or head[:4] == b'koly':
        return 'disk image (UDIF)'
    if head[:4] in (b'dyld', b'NXSB') or head[32:36] == b'NXSB':
        return 'dyld cache or APFS container'
    if head[:8] == b'bplist00' or head[:4] == b'IMG4':
        return None
    return None


def own_kernel(path, relative):
    """True when a Mach-O is the XNU kernel/ built: an arm64 MH_EXECUTE with a matching build record."""
    with open(path, 'rb') as stream:
        header = stream.read(32)
    if header[:4] != b'\xcf\xfa\xed\xfe':
        raise Refused(f'{relative}: a Mach-O that is not a 64-bit little-endian image')
    cputype, _, filetype = struct.unpack_from('<III', header, 4)
    if filetype == MH_FILESET:
        raise Refused(f'{relative}: a kernel collection (MH_FILESET) holds Apple\'s kexts; it belongs to the image set')
    if cputype != 0x0100000c or filetype != MH_EXECUTE:
        raise Refused(f'{relative}: a Mach-O that is not an arm64 kernel executable')
    record_path = path + RECORD_SUFFIX
    if not os.path.isfile(record_path):
        raise Refused(f'{relative}: a Mach-O without a build record ({os.path.basename(record_path)})')
    try:
        with open(record_path) as stream:
            record = json.load(stream)
    except ValueError as error:
        raise Refused(f'{relative}: unreadable build record: {error}')
    if record.get('builder') != BUILDER:
        raise Refused(f'{relative}: the build record is not from kernel/ (builder {record.get("builder")!r})')
    if record.get('sha256') != sha256(path):
        raise Refused(f'{relative}: the build record is for another file (SHA-256 differs)')
    if record.get('kernelcache_kexts_added') is not False or record.get('prelinked') is not False:
        raise Refused(f'{relative}: the build record does not say "no kernelcache kexts, not prelinked"')
    if record.get('apple_binary_inputs') != []:
        raise Refused(f'{relative}: the build record lists Apple binary inputs: {record.get("apple_binary_inputs")}')
    if record.get('restricted_sources') != []:
        raise Refused(f'{relative}: the kernel links sources that may not be redistributed: {record.get("restricted_sources")}')
    if not record.get('build_id'):
        raise Refused(f'{relative}: the build record has no build_id')
    return record


def check(stage):
    """Return the build record of the kernel in the stage (None without one); raise Refused for anything else of Apple's."""
    kernel = None
    for relative in files_of(stage):
        path = os.path.join(stage, relative)
        name = os.path.basename(relative).lower()
        kind = content_kind(path)
        if kind == 'Mach-O':
            record = own_kernel(path, relative)
            if kernel:
                raise Refused(f'{relative}: a second kernel in one package')
            kernel = dict(record, path=relative)
            continue
        if kind:
            raise Refused(f'{relative}: {kind}')
        for pattern in APPLE_NAMES:
            if fnmatch.fnmatch(name, pattern):
                raise Refused(f'{relative}: the name matches {pattern} (Apple firmware)')
    return kernel


def manifest(stage, version, commit, dirty, min_fork, placeholder):
    kernel = check(stage)
    entries = []
    for relative in files_of(stage):
        if relative == MANIFEST:
            continue
        path = os.path.join(stage, relative)
        executable = os.stat(path).st_mode & 0o111
        os.chmod(path, 0o755 if executable else 0o644)
        entries.append({'path': relative.replace(os.sep, '/'), 'size': os.path.getsize(path), 'sha256': sha256(path),
                        'mode': '0755' if executable else '0644'})
    if not any(entry['path'] == 'bin/xnuhost' and entry['mode'] == '0755' for entry in entries):
        raise Refused('bin/xnuhost is missing or not executable')
    document = {
        'format': 1,
        'name': 'oneios-runtime',
        'version': version,
        'arch': 'arm64',
        'session_abi': 1,
        'image_formats': [1],
        'xnuhost': 'bin/xnuhost',
        'git': {'commit': commit, 'dirty': dirty == '1'},
        'min_oneos_fork': min_fork,
        'placeholder': placeholder == '1',
        'kernel': None if kernel is None else {
            'path': kernel['path'], 'build_id': kernel['build_id'], 'sha256': kernel['sha256'],
            'xnu': kernel.get('xnu'), 'profile': kernel.get('profile'), 'record': kernel['path'] + RECORD_SUFFIX},
        'files': entries,
    }
    with open(os.path.join(stage, MANIFEST), 'w') as stream:
        json.dump(document, stream, indent=2)
        stream.write('\n')
    os.chmod(os.path.join(stage, MANIFEST), 0o644)


def main(argv):
    try:
        if len(argv) == 3 and argv[1] == 'check':
            check(argv[2])
        elif len(argv) == 8 and argv[1] == 'manifest':
            manifest(*argv[2:])
        else:
            sys.exit(__doc__)
    except Refused as refusal:
        print(f'package: refused: {refusal}', file=sys.stderr)
        sys.exit(3)


if __name__ == '__main__':
    main(sys.argv)
