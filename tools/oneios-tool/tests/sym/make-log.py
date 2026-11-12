#!/usr/bin/env python3
"""A stand-in xnuhost log for tests/sym/compare-python.sh and tests/log/run.sh on a host that has the dyld shared
cache but no log from the phone.

    make-log.py DSC_DIR OUT.log [--slide HEX] [--stacks N] [--seed N]

Writes `oneios-ubt` thread lines (12 to 14 return addresses each) and one `sheng-ubt sheng-exc` crash backtrace in
the format xnuhost prints. The addresses are real: each is the word after a `bl` in the cache's executable
mappings, plus the slide (default 0xedb4000, a multiple of 0x4000 as dyld's slides are), so both symbolizers find
the slide and name them. The stacks are not real call chains: frames are drawn at random. Nothing of the cache is
written but those addresses.
"""
import os
import random
import struct
import sys

args = sys.argv[1:]


def option(name, default):
    if name in args:
        i = args.index(name)
        value = args[i + 1]
        del args[i:i + 2]
        return int(value, 0)
    return default


slide = option("--slide", 0xEDB4000)
stacks = option("--stacks", 400)
seed = option("--seed", 1)
if len(args) != 2:
    sys.exit(__doc__)
dsc, out = args
rng = random.Random(seed)

# every file's executable mappings: (address, size, file, offset)
texts = []
for name in sorted(os.listdir(dsc)):
    if not name.startswith("dyld_shared_cache_arm64e") or name.endswith((".symbols", ".dylddata", ".dyldreadonly", ".dyldlinkedit")):
        continue
    path = os.path.join(dsc, name)
    with open(path, "rb") as f:
        head = f.read(0x200)
        if head[:4] != b"dyld":
            continue
        offset, count = struct.unpack_from("<II", head, 0x10)
        f.seek(offset)
        for _ in range(count):
            address, size, fileoff, _maxprot, initprot = struct.unpack("<QQQII", f.read(32))
            if initprot & 4 and size > 0x8000:
                texts.append((address, size, path, fileoff))
if not texts:
    sys.exit(f"make-log: no executable mapping in {dsc}")


def return_addresses(count):
    """Addresses right after a call, from random places of the executable mappings."""
    found = []
    while len(found) < count:
        address, size, path, fileoff = rng.choice(texts)
        at = rng.randrange(0, size - 0x4000, 4)
        with open(path, "rb") as f:
            f.seek(fileoff + at)
            block = f.read(0x4000)
        for i in range(0, len(block) - 3, 4):
            if struct.unpack_from("<I", block, i)[0] & 0xFC000000 == 0x94000000:
                found.append(address + at + i + 4 + slide)
                break
    return found


lines = ["[    0.000001] xnuhost: stand-in log made by tools/oneios-tool/tests/sym/make-log.py"]
clock = 1.0
for n in range(stacks):
    clock += rng.random() / 10
    frames = return_addresses(rng.randint(12, 14))
    lines.append("[%12.6f] guest  oneios-ubt: %s[%d] thread 0x%x state 0x%x:%s" % (
        clock, rng.choice(["backboardd", "SpringBoard", "launchd", "locationd"]), 30 + n % 40, 0x400 + n, 5,
        "".join(" 0x%x" % a for a in frames)))
crash = return_addresses(7)
lines.append("[%12.6f] guest  sheng-ubt sheng-exc: pc=0x%x lr=0x%x sp=0x16f1648f0" % (clock + 1, crash[0] - 4, crash[1]))
for i, address in enumerate(crash[2:]):
    lines.append("[%12.6f] guest  sheng-ubt sheng-exc: #%d fp=0x16f164%03x ret=0x%x" % (clock + 1, i, 0x900 + 16 * i, address))
lines.append("[%12.6f] xnuhost: stopping: stand-in log ends" % (clock + 2))
with open(out, "w") as f:
    f.write("\n".join(lines) + "\n")
print(f"{out}: {stacks} thread lines and one crash backtrace, slide {slide:#x}")
