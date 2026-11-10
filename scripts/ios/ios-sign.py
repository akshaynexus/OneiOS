#!/usr/bin/env python3
"""Ad-hoc code signature for an iOS Mach-O, made on Linux, and its CDHash.

  scripts/ios/ios-sign.py [--identifier ID] [--entitlements PLIST] [--json] FILE...

What `codesign --force --sign - --pagesize 4096 [--entitlements PLIST] --identifier ID FILE` makes on a Mac: an
embedded signature with one SHA-256 code directory (version 0x20400, flags adhoc, 4 KiB pages: this guest's
kernel runs 4 KiB pages), an empty requirement set, the entitlements as XML and as DER when given, and an empty
CMS blob. The file is signed in place (cctools' codesign_allocate makes the room; a signature that is there is
replaced). Thin 64-bit files only.

Prints one line per file, the same line `userland/tweak cdhash` reads back from the file:
  <cdhash> <arch> pages=4096 <identifier> <file>
The guest runs a file whose CDHash is in a trust cache; ours reach it through owner-trust.json (docs/userland.md).

Not made: the bundle seal (_CodeSignature/CodeResources, the Info.plist slot). The kernel checks neither for a
library that dyld maps; only the CDHash has to be trusted.
"""
import argparse
import hashlib
import json
import os
import plistlib
import struct
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
MH_MAGIC_64, MH_EXECUTE = 0xFEEDFACF, 2
LC_SEGMENT_64, LC_CODE_SIGNATURE = 0x19, 0x1D
PAGE_SHIFT = 12
PAGE = 1 << PAGE_SHIFT
CS_ADHOC = 0x2
CS_EXECSEG_MAIN_BINARY = 0x1
SLOT_REQUIREMENTS, SLOT_ENTITLEMENTS, SLOT_DER = 2, 5, 7


def parse(data):
    magic, cputype, cpusubtype, filetype, ncmds, _, _, _ = struct.unpack_from('<IIIIIIII', data, 0)
    if magic != MH_MAGIC_64:
        raise ValueError('not a thin 64-bit Mach-O')
    out = {'cputype': cputype, 'cpusubtype': cpusubtype, 'filetype': filetype, 'signature': None, 'text': None}
    at = 32
    for _ in range(ncmds):
        cmd, size = struct.unpack_from('<II', data, at)
        if cmd == LC_CODE_SIGNATURE:
            out['signature'] = struct.unpack_from('<II', data, at + 8)
        elif cmd == LC_SEGMENT_64 and data[at + 8:at + 24].rstrip(b'\0') == b'__TEXT':
            out['text'] = struct.unpack_from('<QQ', data, at + 40)   # fileoff, filesize
        at += size
    return out


def arch_name(info):
    if info['cputype'] == 0x0100000C:
        return 'arm64e' if info['cpusubtype'] & 0xFFFFFF == 2 else 'arm64'
    if info['cputype'] == 0x01000007:
        return 'x86_64'
    raise ValueError('cpu type %#x' % info['cputype'])


def blob(magic, body):
    return struct.pack('>II', magic, 8 + len(body)) + body


def der_len(n):
    if n < 0x80:
        return bytes([n])
    raw = n.to_bytes((n.bit_length() + 7) // 8, 'big')
    return bytes([0x80 | len(raw)]) + raw


def der(tag, body):
    return bytes([tag]) + der_len(len(body)) + body


def der_value(v):
    """Entitlements as CoreEntitlements' DER: a dictionary is [CONTEXT 16] of SEQUENCE { UTF8String key, value }."""
    if isinstance(v, bool):
        return der(0x01, b'\xff' if v else b'\x00')
    if isinstance(v, int):
        return der(0x02, v.to_bytes(v.bit_length() // 8 + 1, 'big', signed=True))
    if isinstance(v, str):
        return der(0x0C, v.encode())
    if isinstance(v, (list, tuple)):
        return der(0x30, b''.join(der_value(x) for x in v))
    if isinstance(v, dict):
        return der(0xB0, b''.join(der(0x30, der(0x0C, k.encode()) + der_value(v[k])) for k in sorted(v)))
    raise ValueError(f'entitlement value of type {type(v).__name__}')


def signature(data, info, identifier, entitlements):
    limit = info['signature'][0]
    ident = identifier.encode() + b'\0'
    special = {SLOT_REQUIREMENTS: blob(0xFADE0C01, struct.pack('>I', 0))}
    if entitlements is not None:
        special[SLOT_ENTITLEMENTS] = blob(0xFADE7171, entitlements)
        special[SLOT_DER] = blob(0xFADE7172, der(0x70, der(0x02, b'\x01') + der_value(plistlib.loads(entitlements))))
    n_special = max(special)
    n_code = (limit + PAGE - 1) // PAGE
    header = 88
    hash_offset = header + len(ident) + 32 * n_special
    slots = b''.join(hashlib.sha256(special[n]).digest() if n in special else bytes(32) for n in range(n_special, 0, -1))
    pages = b''.join(hashlib.sha256(data[i:min(i + PAGE, limit)]).digest() for i in range(0, limit, PAGE))
    text_off, text_size = info['text']
    length = hash_offset + 32 * n_code
    directory = struct.pack('>IIIIIIIIIBBBBIIIIQQQQ', 0xFADE0C02, length, 0x20400, CS_ADHOC, hash_offset, header, n_special,
                            n_code, limit, 32, 2, 0, PAGE_SHIFT, 0, 0, 0, 0, 0, text_off, text_size,
                            CS_EXECSEG_MAIN_BINARY if info['filetype'] == MH_EXECUTE else 0) + ident + slots + pages
    assert len(directory) == length
    parts = [(0, directory)] + [(n, special[n]) for n in sorted(special)] + [(0x10000, blob(0xFADE0B01, b''))]
    at = 12 + 8 * len(parts)
    index, body = b'', b''
    for kind, part in parts:
        index += struct.pack('>II', kind, at + len(body))
        body += part
    return struct.pack('>III', 0xFADE0CC0, at + len(body), len(parts)) + index + body, hashlib.sha256(directory).digest()[:20].hex()


def sign(path, identifier, entitlements, allocate):
    with open(path, 'rb') as f:
        data = f.read()
    info = parse(data)
    arch = arch_name(info)
    end = info['signature'][0] if info['signature'] else len(data)
    ent_room = 0 if entitlements is None else 2 * len(entitlements) + 256
    room = 12 + 8 * 5 + 88 + len(identifier) + 1 + 32 * 7 + 32 * ((end + 16 + PAGE - 1) // PAGE + 1) + 12 + 8 + ent_room
    room = (room + 15) & ~15
    mode = os.stat(path).st_mode & 0o777
    with tempfile.NamedTemporaryFile(dir=os.path.dirname(os.path.abspath(path)), delete=False) as tmp:
        tmp.close()
        try:
            subprocess.run([allocate, '-i', path, '-a', arch, str(room), '-o', tmp.name], check=True)
            with open(tmp.name, 'rb') as f:
                data = bytearray(f.read())
            info = parse(data)
            offset, size = info['signature']
            sig, cdhash = signature(data, info, identifier, entitlements)
            if len(sig) > size or offset + size != len(data):
                raise ValueError(f'signature of {len(sig)} bytes for {size} bytes of room')
            data[offset:offset + size] = sig + bytes(size - len(sig))
            with open(tmp.name, 'wb') as f:
                f.write(data)
            os.chmod(tmp.name, mode)
            os.replace(tmp.name, path)
        finally:
            if os.path.exists(tmp.name):
                os.unlink(tmp.name)
    return {'file': path, 'cdhash': cdhash, 'arch': arch, 'page_size': PAGE, 'identifier': identifier, 'hash': 'sha256',
            'bytes': len(data), 'entitlements': entitlements is not None}


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--identifier', help='default: the file name')
    p.add_argument('--entitlements', metavar='PLIST')
    p.add_argument('--json', action='store_true', help='print the records as JSON')
    p.add_argument('files', nargs='+')
    a = p.parse_args()
    allocate = os.environ.get('IOS_CODESIGN_ALLOCATE') or os.path.join(
        os.environ.get('IOS_CCTOOLS') or os.path.join(REPO, 'out/ios-toolchain/cctools/bin'), 'codesign_allocate')
    if not os.access(allocate, os.X_OK):
        sys.exit(f'ios-sign: no codesign_allocate at {allocate} (scripts/setup/ios-toolchain.sh --ld builds cctools)')
    entitlements = None
    if a.entitlements:
        with open(a.entitlements, 'rb') as f:
            entitlements = f.read()
        plistlib.loads(entitlements)
    records = []
    for path in a.files:
        try:
            records.append(sign(path, a.identifier or os.path.basename(path), entitlements, allocate))
        except (ValueError, OSError, subprocess.CalledProcessError) as e:
            sys.exit(f'ios-sign: {path}: {e}')
    if a.json:
        json.dump(records, sys.stdout, indent=2)
        print()
    else:
        for r in records:
            print(f'{r["cdhash"]} {r["arch"]} pages={r["page_size"]} {r["identifier"]} {r["file"]}')


if __name__ == '__main__':
    main()
