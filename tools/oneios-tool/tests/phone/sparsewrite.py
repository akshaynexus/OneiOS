# The script `oneios-tool phone sparse-write` replaces: the Mac's out/session-logs/2026-11-12/sparsewrite.py
# (docs/newtodo.md "How to resume" step 2: `adb exec-out "su -c '... gzip -1 -c .../data.img'" | gunzip -c |
# python3 -I sparsewrite.py data.img`). Kept unchanged below as the reference of sparse-compare.sh: stdin to
# argv[1], a 4 MiB read that is all zeros is skipped with a seek, the length is set at the end.
import sys, os
out = open(sys.argv[1], 'wb'); B = 4 << 20; z = bytes(B); off = 0
r = sys.stdin.buffer
while True:
    b = r.read(B)
    if not b: break
    if b != z[:len(b)]:
        out.seek(off); out.write(b)
    off += len(b)
out.truncate(off); out.close(); print('bytes', off)
