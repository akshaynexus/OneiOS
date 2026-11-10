"""Reads what the owner trust list needs from a signed Mach-O: its CDHash, identifier, architecture and
signature page size. Read-only; signing is done by the helper build (ldid or codesign).

The guest's AMFI accepts a binary whose CDHash is in a trust cache. Apple's files are in Apple's own trust
caches; ours go into an owner module the kernel build adds (UniXNU `scripts/make-owner-trust-bundle.py`),
which takes the list this module makes.
"""
import hashlib
import struct

MH_MAGIC_64 = 0xFEEDFACF
FAT_MAGIC, FAT_MAGIC_64 = 0xCAFEBABE, 0xCAFEBABF
CPU_TYPE_ARM64 = 0x0100000C
LC_CODE_SIGNATURE, LC_BUILD_VERSION = 0x1D, 0x32
CSMAGIC_EMBEDDED_SIGNATURE, CSMAGIC_CODEDIRECTORY = 0xFADE0CC0, 0xFADE0C02
CS_ADHOC = 0x2
# hashType -> (hashlib name, rank); the highest rank is the directory AMFI identifies the file by
HASHES = {1: ('sha1', 1), 3: ('sha256', 2), 2: ('sha256', 3), 4: ('sha384', 4)}
PLATFORMS = {1: 'macos', 2: 'ios', 3: 'tvos', 4: 'watchos', 6: 'maccatalyst', 7: 'iossimulator'}


class MachOError(Exception):
    pass


def _slices(data):
    """(offset, size) of each Mach-O in the file."""
    if len(data) < 8:
        raise MachOError('too short for a Mach-O')
    magic = struct.unpack('>I', data[:4])[0]
    if magic in (FAT_MAGIC, FAT_MAGIC_64):
        count = struct.unpack('>I', data[4:8])[0]
        if count > 16:
            raise MachOError('not a fat Mach-O')
        out = []
        for i in range(count):
            if magic == FAT_MAGIC:
                _, _, offset, size, _ = struct.unpack_from('>IIIII', data, 8 + 20 * i)
            else:
                _, _, offset, size, _, _ = struct.unpack_from('>IIQQII', data, 8 + 32 * i)
            out.append((offset, size))
        return out
    return [(0, len(data))]


def _code_directories(blob):
    if len(blob) < 12:
        raise MachOError('signature too short')
    magic, length, count = struct.unpack_from('>III', blob, 0)
    if magic != CSMAGIC_EMBEDDED_SIGNATURE or length > len(blob):
        raise MachOError('no embedded signature')
    out = []
    for i in range(count):
        _, offset = struct.unpack_from('>II', blob, 12 + 8 * i)
        if offset + 8 > length:
            raise MachOError('signature index out of range')
        blob_magic, blob_length = struct.unpack_from('>II', blob, offset)
        if blob_magic == CSMAGIC_CODEDIRECTORY:
            if offset + blob_length > length or blob_length < 44:
                raise MachOError('code directory out of range')
            out.append(blob[offset:offset + blob_length])
    return out


def _describe_slice(data):
    if len(data) < 32:
        raise MachOError('too short for a Mach-O')
    magic, cputype, cpusubtype, filetype, ncmds, sizeofcmds, _, _ = struct.unpack_from('<IIIIIIII', data, 0)
    if magic != MH_MAGIC_64:
        raise MachOError('not a 64-bit little-endian Mach-O')
    info = {'cputype': cputype, 'cpusubtype': cpusubtype, 'filetype': filetype, 'platform': None, 'minos': None}
    if cputype == CPU_TYPE_ARM64:
        info['arch'] = 'arm64e' if cpusubtype & 0x00FFFFFF == 2 else 'arm64'
    else:
        info['arch'] = 'cpu-%#x' % cputype
    signature = None
    at = 32
    for _ in range(ncmds):
        if at + 8 > min(len(data), 32 + sizeofcmds):
            raise MachOError('load commands out of range')
        cmd, size = struct.unpack_from('<II', data, at)
        if size < 8:
            raise MachOError('bad load command')
        if cmd == LC_CODE_SIGNATURE:
            offset, length = struct.unpack_from('<II', data, at + 8)
            if offset + length > len(data):
                raise MachOError('signature out of range')
            signature = data[offset:offset + length]
        elif cmd == LC_BUILD_VERSION:
            platform, minos = struct.unpack_from('<II', data, at + 8)
            info['platform'] = PLATFORMS.get(platform, str(platform))
            info['minos'] = '%d.%d' % (minos >> 16, (minos >> 8) & 0xFF)
        at += size
    if signature is None:
        raise MachOError('not signed (no LC_CODE_SIGNATURE)')
    best = None
    for directory in _code_directories(signature):
        version, flags, hash_offset, ident_offset = struct.unpack_from('>IIII', directory, 8)
        hash_type, page_shift = directory[37], directory[39]
        if hash_type not in HASHES:
            continue
        name, rank = HASHES[hash_type]
        if best is None or rank > best[0]:
            end = directory.find(b'\0', ident_offset)
            best = (rank, {
                'cdhash': hashlib.new(name, directory).digest()[:20].hex(),
                'hash': name,
                'identifier': directory[ident_offset:end].decode('utf-8', 'replace') if end > 0 else '',
                'adhoc': bool(flags & CS_ADHOC),
                'page_size': 1 << page_shift if page_shift else 0,
            })
    if best is None:
        raise MachOError('signature has no usable code directory')
    info.update(best[1])
    return info


def describe(data):
    """One record per architecture of a signed Mach-O file."""
    return [_describe_slice(data[offset:offset + size]) for offset, size in _slices(data)]


def is_macho(data):
    if len(data) < 4:
        return False
    return struct.unpack('<I', data[:4])[0] == MH_MAGIC_64 or struct.unpack('>I', data[:4])[0] in (FAT_MAGIC, FAT_MAGIC_64)
