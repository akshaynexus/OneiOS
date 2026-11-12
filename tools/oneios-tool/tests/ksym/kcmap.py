#!/usr/bin/env python3
"""The test's own reader of a kernel collection, written apart from oneios-tool's (crates/oneios-ksym) so that
compare.sh does not check the tool against itself.

  kcmap.py members KC                    every fileset member: name, header address, segments, symbols kept
  kcmap.py forward KC KERNEL NAME...     per symbol of KERNEL (xnu.macho): its address there, the segment, and the
                                         address in KC's com.apple.kernel (the segment's own move added)
  kcmap.py own KC NAME...                the address of a symbol in com.apple.kernel's own symbol table in KC
                                         (a third source, when the collection keeps the kernel's symbols)

Only struct and mmap: no Mach-O library. Reads the files in place and writes nothing.
"""
import mmap
import struct
import sys

LC_SEGMENT_64 = 0x19
LC_SYMTAB = 0x2
LC_FILESET_ENTRY = 0x80000035
MH_MAGIC_64 = 0xFEEDFACF
MH_FILESET = 0xC


def open_map(path):
    with open(path, "rb") as f:
        return mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)


def commands(data, at):
    magic, _cpu, _sub, filetype, ncmds, _size, _flags, _res = struct.unpack_from("<8I", data, at)
    if magic != MH_MAGIC_64:
        raise SystemExit(f"no 64-bit Mach-O header at offset {at:#x}")
    off = at + 32
    out = []
    for _ in range(ncmds):
        cmd, size = struct.unpack_from("<2I", data, off)
        out.append((cmd, off, size))
        off += size
    return filetype, out


def cstr(raw):
    return raw.split(b"\0", 1)[0].decode("latin-1")


def segments(data, cmds):
    out = []
    for cmd, off, _size in cmds:
        if cmd == LC_SEGMENT_64:
            name = cstr(data[off + 8:off + 24])
            vmaddr, vmsize, fileoff, filesize = struct.unpack_from("<4Q", data, off + 24)
            out.append((name, vmaddr, vmsize, fileoff, filesize))
    return out


def symbols(data, cmds):
    """name -> address for the defined symbols (N_SECT, no stabs); empty without LC_SYMTAB."""
    out = {}
    for cmd, off, _size in cmds:
        if cmd == LC_SYMTAB:
            symoff, nsyms, stroff, strsize = struct.unpack_from("<4I", data, off + 8)
            for i in range(nsyms):
                strx, ntype, _sect, _desc, value = struct.unpack_from("<IBBHQ", data, symoff + 16 * i)
                if ntype & 0xE0 or (ntype & 0x0E) != 0x0E:
                    continue
                end = data.find(b"\0", stroff + strx, stroff + strsize)
                out.setdefault(data[stroff + strx:end].decode("latin-1"), value)
    return out


def members(data):
    filetype, cmds = commands(data, 0)
    if filetype != MH_FILESET:
        return [("(not a fileset)", 0, 0)]
    out = []
    for cmd, off, _size in cmds:
        if cmd == LC_FILESET_ENTRY:
            vmaddr, fileoff, name_at = struct.unpack_from("<QQI", data, off + 8)
            end = data.find(b"\0", off + name_at)
            out.append((data[off + name_at:end].decode("latin-1"), vmaddr, fileoff))
    return out


def kernel_member(data):
    for name, vmaddr, fileoff in members(data):
        if name == "com.apple.kernel":
            return commands(data, fileoff)[1]
    raise SystemExit("the collection has no com.apple.kernel member")


def main(argv):
    if len(argv) >= 3 and argv[1] == "members":
        data = open_map(argv[2])
        for name, vmaddr, fileoff in members(data):
            _, cmds = commands(data, fileoff)
            print(f"{name}  header {vmaddr:#x}  file {fileoff:#x}  symbols {len(symbols(data, cmds))}")
            for seg, addr, size, off, fsize in segments(data, cmds):
                print(f"    {seg:<18} {addr:#x} +{size:#x}  file {off:#x} +{fsize:#x}")
        return 0
    if len(argv) >= 5 and argv[1] == "forward":
        kc, kernel = open_map(argv[2]), open_map(argv[3])
        in_kc = {s[0]: s for s in segments(kc, kernel_member(kc))}
        _, cmds = commands(kernel, 0)
        segs = segments(kernel, cmds)
        table = symbols(kernel, cmds)
        status = 0
        for name in argv[4:]:
            addr = table.get(name, table.get("_" + name))
            if addr is None:
                print(f"{name} - - - (no such symbol in the kernel)")
                status = 1
                continue
            seg = next((s for s in segs if s[1] <= addr < s[1] + s[2]), None)
            if seg is None or seg[0] not in in_kc:
                print(f"{name} {addr:#x} {seg[0] if seg else '-'} - (the segment is not in the collection)")
                status = 1
                continue
            print(f"{name} {addr:#x} {seg[0]} {addr + in_kc[seg[0]][1] - seg[1]:#x}")
        return status
    if len(argv) >= 4 and argv[1] == "own":
        kc = open_map(argv[2])
        table = symbols(kc, kernel_member(kc))
        for name in argv[3:]:
            addr = table.get(name, table.get("_" + name))
            print(f"{name} {addr:#x}" if addr is not None else f"{name} -")
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
