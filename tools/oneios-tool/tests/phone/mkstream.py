#!/usr/bin/env python3
"""The streams of sparse-compare.sh, to stdout; the same bytes on every run.

  mkstream.py runs             zero runs and data runs shorter than 4 KiB, between 4 KiB and 4 MiB, and longer
                               (20 and 70 MiB of zeros too: APFS keeps a hole only from about 16 MiB), on the
                               edges of blocks and off them
  mkstream.py tail             data, then zeros that end 123 bytes into a block
  mkstream.py zero             9 MiB and 123 bytes of zeros
  mkstream.py empty            nothing
  mkstream.py lastbyte         33 MiB and 1000 bytes of zeros, the last byte not zero
  mkstream.py dense            5 MiB and 7 bytes without a zero run
  mkstream.py phone MIB DATA   a guess at the shape of the phone's Data store: MIB MiB, of which about DATA MiB
                               hold data, in runs of 4 KiB to tens of MiB with short holes inside; half of the
                               data does not compress (the real store is 32768 MiB with about 2100 of data)
"""
import random
import sys

KIB, MIB = 1 << 10, 1 << 20
out = sys.stdout.buffer
ZEROS = bytes(MIB)
rng = random.Random(0x0E105)


def zeros(n):
    while n > 0:
        out.write(ZEROS[:min(n, MIB)])
        n -= min(n, MIB)


def data(n, compressible=None):
    """n bytes with no zero: random bytes (0 mapped to 1), or one line of text again and again (gzip packs it small)."""
    if compressible is None:
        compressible = rng.random() < 0.5
    while n > 0:
        m = min(n, MIB)
        if compressible:
            line = b'%08x %s\n' % (rng.getrandbits(32), bytes(rng.choices(b'abcdefghijklmnopqrstuvwxyz ', k=40)))
            out.write((line * (m // len(line) + 1))[:m])
        else:
            out.write(rng.randbytes(m).replace(b'\0', b'\1'))
        n -= m


def runs():
    data(1)                                   # one byte, then
    for z, d in [
        (100, 50),                            # zero runs shorter than a block, inside one
        (4095, 1), (4096, 4096), (4097, 4095),
        (3000, 10000), (16384, 16384), (16385, 3),
        (65536 - 20000, 70000),               # a few blocks, not on an edge
        (MIB, MIB), (MIB + 1, 5),
        (4 * MIB - 1, 4 * MIB + 1),           # just under the script's 4 MiB
        (4 * MIB, 4 * MIB), (8 * MIB + 4096, 9 * MIB + 77),
        (15 * MIB, 4096), (20 * MIB, 123456), (70 * MIB, 8192),
        (12345, 1),
    ]:
        zeros(z)
        data(d)
    zeros(6 * MIB)                            # and a long run of zeros at the end


def phone(mib, data_mib):
    size, want = mib * MIB, data_mib * MIB
    regions = max(8, data_mib // 4)
    # each region: where it starts (a 4 KiB edge) and how long it is
    starts = sorted(rng.randrange(0, size // 4096) * 4096 for _ in range(regions))
    at = 0
    for i, start in enumerate(starts):
        end = starts[i + 1] if i + 1 < regions else size
        length = int(want / regions * rng.choice([0.01, 0.05, 0.25, 0.5, 1, 1, 2, 4.19]))
        length = max(4096, min(length, end - start) // 4096 * 4096)
        zeros(start - at)
        at = start
        while at < start + length:
            n = min(start + length - at, rng.choice([4, 8, 16, 64, 256, 1024, 4096]) * KIB)
            data(n)
            at += n
            if at < start + length:           # a short hole inside the region
                h = min(start + length - at, rng.choice([4, 4, 8, 16, 64, 512]) * KIB)
                zeros(h)
                at += h
    zeros(size - at)


kind = sys.argv[1]
if kind == 'runs':
    runs()
elif kind == 'tail':
    data(3 * MIB + 5000)
    zeros(2 * MIB + 123)
elif kind == 'zero':
    zeros(9 * MIB + 123)
elif kind == 'empty':
    pass
elif kind == 'lastbyte':
    zeros(33 * MIB + 999)
    out.write(b'\x01')
elif kind == 'dense':
    data(5 * MIB + 7)
elif kind == 'phone':
    phone(int(sys.argv[2]), int(sys.argv[3]))
else:
    sys.exit(__doc__)
