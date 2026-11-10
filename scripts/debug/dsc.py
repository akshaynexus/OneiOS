#!/usr/bin/env python3
"""scripts/debug/dsc.py [--slide HEX] ADDR...   name user addresses of a boot log (the kernel's sheng-exc / sheng-ubt
lines) in the image set's dyld shared cache.

The cache is read from the directory ONEIOS_DSC_DIR (the one that holds dyld_shared_cache_arm64e and its .NN and
.symbols files); to get one without copying 6 GiB:
    mkdir -p out/dsc && apfs-fuse out/image/24A446/systemos.img out/dsc
    export ONEIOS_DSC_DIR=out/dsc/root/System/Library/Caches/com.apple.dyld      (fusermount3 -u out/dsc when done)
Apple's files: read in place, nothing of them is written but out/dsc-text.pickle (the images' address ranges). Without --slide the slide is found from the addresses
themselves: the one multiple of 0x4000 below maxSlide for which the most addresses sit right after a call instruction
(give return addresses of one backtrace). Prints image + nearest symbol (local symbols from the .symbols file, else the
image's own symbol table) for each address.
"""
import bisect, mmap, os, pickle, struct, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if 'ONEIOS_DSC_DIR' not in os.environ:
    sys.exit(__doc__)
DIR = Path(os.environ['ONEIOS_DSC_DIR']).resolve()
MAIN = DIR / 'dyld_shared_cache_arm64e'


def mm(path):
    f = open(path, 'rb')
    return mmap.mmap(f.fileno(), 0, prot=mmap.PROT_READ)


class Cache:
    def __init__(self):
        self.files = {}
        main = self.file(MAIN)
        self.base, self.size, self.max_slide = struct.unpack_from('<QQQ', main, 0xE0)
        self.maps = []          # (vmaddr, size, mmap, fileoff)
        self.add_mappings(main)
        sub_off, sub_count = struct.unpack_from('<II', main, 0x188)
        for i in range(sub_count):
            entry = main[sub_off + 56 * i: sub_off + 56 * (i + 1)]
            suffix = entry[24:].split(b'\0')[0].decode()
            self.add_mappings(self.file(Path(str(MAIN) + suffix)))
        self.maps.sort(key=lambda m: m[0])
        self.starts = [m[0] for m in self.maps]
        img_off, img_count = struct.unpack_from('<II', main, 0x1C0)
        self.images = []
        for i in range(img_count):
            addr, _, _, path_off, _ = struct.unpack_from('<QQQII', main, img_off + 32 * i)
            end = main.find(b'\0', path_off)
            self.images.append((addr, main[path_off:end].decode()))

    def file(self, path):
        if path not in self.files:
            self.files[path] = mm(path)
        return self.files[path]

    def add_mappings(self, m):
        off, count = struct.unpack_from('<II', m, 0x10)
        for i in range(count):
            addr, size, foff, _, _ = struct.unpack_from('<QQQII', m, off + 32 * i)
            self.maps.append((addr, size, m, foff))

    def where(self, va):
        i = bisect.bisect_right(self.starts, va) - 1
        if i < 0:
            return None
        addr, size, m, foff = self.maps[i]
        return (m, foff + va - addr) if va < addr + size else None

    def read(self, va, n):
        w = self.where(va)
        return w[0][w[1]:w[1] + n] if w else None

    def text_ranges(self):
        cache = ROOT / 'out/dsc-text.pickle'
        if cache.exists():
            return pickle.loads(cache.read_bytes())
        out = []
        for index, (addr, path) in enumerate(self.images):
            for name, vmaddr, vmsize in self.segments(addr):
                if name == '__TEXT':
                    out.append((vmaddr, vmaddr + vmsize, index))
        out.sort()
        cache.write_bytes(pickle.dumps(out))
        return out

    def commands(self, addr):
        head = self.read(addr, 32)
        ncmds, = struct.unpack_from('<I', head, 16)
        at = addr + 32
        for _ in range(ncmds):
            cmd, size = struct.unpack('<II', self.read(at, 8))
            yield cmd, self.read(at, size)
            at += size

    def segments(self, addr):
        for cmd, data in self.commands(addr):
            if cmd == 0x19:
                yield data[8:24].split(b'\0')[0].decode(), *struct.unpack_from('<QQ', data, 24)

    def symbols(self, index):
        """(value, name) of an image: its own symbol table and the cache's local symbols."""
        addr, _ = self.images[index]
        out = []
        linkedit = None
        for cmd, data in self.commands(addr):
            if cmd == 0x19 and data[8:24].split(b'\0')[0] == b'__LINKEDIT':
                vmaddr, _, fileoff, _ = struct.unpack_from('<QQQQ', data, 24)
                linkedit = self.where(vmaddr)
            if cmd == 0x2 and linkedit:
                symoff, nsyms, stroff, strsize = struct.unpack_from('<IIII', data, 8)
                m = linkedit[0]
                for i in range(nsyms):
                    strx, kind, _, _, value = struct.unpack_from('<IBBHQ', m, symoff + 16 * i)
                    if value and (kind & 0x0e) == 0x0e:
                        end = m.find(b'\0', stroff + strx)
                        out.append((value, m[stroff + strx:end].decode(errors='replace')))
        sym = self.file(Path(str(MAIN) + '.symbols'))
        local_off, = struct.unpack_from('<Q', sym, 0x48)
        nlist_off, nlist_count, str_off, str_size, ent_off, ent_count = struct.unpack_from('<IIIIII', sym, local_off)
        for i in range(ent_count):
            dylib, start, count = struct.unpack_from('<QII', sym, local_off + ent_off + 16 * i)
            if dylib == addr - self.base:
                for k in range(start, start + count):
                    strx, kind, _, _, value = struct.unpack_from('<IBBHQ', sym, local_off + nlist_off + 16 * k)
                    if value:
                        end = sym.find(b'\0', local_off + str_off + strx)
                        out.append((value, sym[local_off + str_off + strx:end].decode(errors='replace')))
                break
        out.sort()
        return out


def is_call(word):
    return (word & 0xfc000000) == 0x94000000 or (word & 0xfffffc1f) == 0xd63f0000 or (word & 0xfefff800) == 0xd63f0800


def main():
    args = sys.argv[1:]
    slide = None
    if args and args[0] == '--slide':
        slide = int(args[1], 16); args = args[2:]
    addrs = [int(a, 16) for a in args]
    c = Cache()
    inside = [a for a in addrs if c.base <= a < c.base + c.size + c.max_slide]
    if slide is None:
        best = (-1, 0); ties = 0
        for s in range(0, c.max_slide, 0x4000):
            score = 0
            for a in inside:
                w = c.read(a - s - 4, 4)
                if w and is_call(struct.unpack('<I', w)[0]):
                    score += 1
            if score > best[0]:
                best = (score, s); ties = 1
            elif score == best[0]:
                ties += 1
        slide = best[1]
        print(f'slide {slide:#x} ({best[0]} of {len(inside)} addresses follow a call' +
              (f'; {ties} slides fit as well: NOT reliable, give a whole backtrace (8 or more return addresses) or --slide)' if ties > 1 else ')'))
    ranges = c.text_ranges()
    starts = [r[0] for r in ranges]
    table = {}
    for a in addrs:
        u = a - slide
        i = bisect.bisect_right(starts, u) - 1
        if i < 0 or not ranges[i][0] <= u < ranges[i][1]:
            print(f'{a:#x}  (not in a cache image; unslid {u:#x})')
            continue
        index = ranges[i][2]
        if index not in table:
            table[index] = c.symbols(index)
        syms = table[index]
        k = bisect.bisect_right([v for v, _ in syms], u) - 1
        name = f'{syms[k][1]} + {u - syms[k][0]:#x}' if k >= 0 else '?'
        print(f'{a:#x}  {os.path.basename(c.images[index][1])}  {name}')


main()
