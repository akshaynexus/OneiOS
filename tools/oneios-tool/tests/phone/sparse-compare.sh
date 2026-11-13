#!/bin/bash
# oneios-tool phone sparse-write against the script it replaces (sparsewrite.py beside this file: the Mac's, of
# docs/newtodo.md "How to resume" step 2): the same bytes and the same length, and not more blocks.
#
#   tools/oneios-tool/tests/phone/sparse-compare.sh            one line per check (ok | FAILED), then `ALL SAME`
#                                                              (exit 0) or `DIFFERENT: N` (exit 1); 20 s
#   MAP=1 ...                                                  also every file's data regions (SEEK_DATA, SEEK_HOLE)
#   SPEED=4096:280 ...                                         also the time of both on a stream of 4096 MiB with
#                                                              about 280 MiB of data (debug mkstream phone), three ways
#
# The streams (`oneios-tool debug mkstream`): `runs` (zero runs and data runs shorter than 4 KiB, between 4 KiB and 4 MiB, and
# longer, on the edges of blocks and off them), `tail` (ends inside a block), `zero` (only zeros), `empty`,
# `lastbyte` (a data byte at the very end), `dense` (no zero run), `phone` (64 MiB shaped like the Data store).
# Each goes through the script, through `phone sparse-write`, and through gzip -1 and `phone sparse-write
# --gunzip --expect-size`. Checked: the script's copy is the stream (cmp); the tool's two copies are the script's
# (cmp: bytes and length); both say the stream's length; the tool's copies do not hold more blocks than the
# script's (st_blocks after an fsync, 1 % and 64 KiB allowed for a file system that counts its extent blocks).
# The line after each stream's checks says what the file system holds for each copy.
#
# SPEED: the stream is written once as a sparse file (the reference of every timed copy's cmp), then timed (wall,
#   MB/s of the stream, CPU of each process): from the generator through gzip -1 (the pipe of the phone:
#   debug mkstream | gzip -1 -c | ...; and that source alone, which bounds both), from the packed stream (the reader
#   alone: the script needs gunzip -c in front), and from the sparse file with no gzip. The times mean something
#   only on a machine that does nothing else: the load is printed. (The gzip pipe is not fed from the sparse
#   file: reading 32 GiB of holes fills the page cache with zeros, and the next run pays for it.)
#
# Env: TOOL (the binary), WORK (the scratch folder, default out/sparse-compare of this checkout; it decides the
# file system that is tested), PYTHON (default python3: the frozen sparsewrite.py and this script's own file-system
# and timing reads), KEEP=1 (keep the scratch folder).
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
TOOL=${TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
W=${WORK:-$ROOT/out/sparse-compare}
PY=${PYTHON:-python3}
[ -x "$TOOL" ] || { echo "sparse-compare.sh: no $TOOL (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)" >&2; exit 2; }
FAILED=0
ok() { echo "ok      $1"; }
bad() { echo "FAILED  $1${2:+: $2}"; FAILED=$((FAILED + 1)); }
rm -rf "$W"; mkdir -p "$W"

# held FILE [map]: "LENGTH ALLOCATED REGIONS DATA" (bytes; the data regions as SEEK_DATA and SEEK_HOLE report them,
# REGIONS is - where the file system does not report them), then with `map` the regions, one per line.
held() {
  "$PY" -I - "$@" <<'PYEOF'
import errno, os, sys
fd = os.open(sys.argv[1], os.O_RDONLY)
os.fsync(fd)                      # APFS and btrfs count a file's blocks when it is written out
st = os.fstat(fd)
regions, at = [], 0
try:
    while at < st.st_size:
        try:
            data = os.lseek(fd, at, os.SEEK_DATA)
        except OSError as e:
            if e.errno != errno.ENXIO:
                raise
            break
        at = os.lseek(fd, data, os.SEEK_HOLE)
        regions.append((data, at))
    count = len(regions)
except (OSError, AttributeError):
    count = '-'
print(st.st_size, st.st_blocks * 512, count, sum(b - a for a, b in regions))
if len(sys.argv) > 2:
    for a, b in regions:
        print(f'          {a:#x}..{b:#x} ({b - a})')
PYEOF
}
show() { # LABEL FILE
  local h; h=$(held "$2" ${MAP:+map}) || { echo "        $1: ?"; return; }
  set -- "$1" $(printf '%s\n' "$h" | head -n 1) "$(printf '%s\n' "$h" | tail -n +2)"
  echo "        $1: $3 bytes held, $4 data regions of $5 bytes"
  [ -z "$6" ] || printf '%s\n' "$6"
}

# one NAME mkstream-args...
one() {
  local name=$1 s=$W/$1; shift
  "$TOOL" debug mkstream "$@" > "$s.bin" || { bad "$name: debug mkstream"; return; }
  local size; size=$(wc -c < "$s.bin" | tr -d ' ')
  local out
  out=$("$PY" -I "$HERE/sparsewrite.py" "$s.py.img" < "$s.bin" 2>&1) && [ "$out" = "bytes $size" ] && cmp -s "$s.py.img" "$s.bin" \
    && ok "$name ($size bytes): the script's copy is the stream" || { bad "$name: the script" "$out"; return; }
  out=$("$TOOL" phone sparse-write "$s.rs.img" < "$s.bin" 2>&1) && cmp "$s.rs.img" "$s.py.img" > "$s.cmp" 2>&1 \
    && ok "$name: sparse-write: the script's bytes and length" || bad "$name: sparse-write" "$out $(cat "$s.cmp" 2>/dev/null)"
  printf '%s\n' "$out" | grep -q "^$s.rs.img: $size bytes, " && ok "$name: sparse-write says $size bytes" || bad "$name: sparse-write's line" "$out"
  out=$(gzip -1 -c "$s.bin" | "$TOOL" phone sparse-write "$s.gz.img" --gunzip --expect-size "$size" 2>&1) && cmp "$s.gz.img" "$s.py.img" > "$s.cmp" 2>&1 \
    && ok "$name: gzip -1 | sparse-write --gunzip --expect-size $size: the script's bytes and length" || bad "$name: sparse-write --gunzip" "$out $(cat "$s.cmp" 2>/dev/null)"
  local py rs gz
  py=$(held "$s.py.img" | awk '{print $2}'); rs=$(held "$s.rs.img" | awk '{print $2}'); gz=$(held "$s.gz.img" | awk '{print $2}')
  if [ -n "$py" ] && [ -n "$rs" ] && [ "$rs" = "$gz" ] && [ "$rs" -le $((py + py / 100 + 65536)) ]; then ok "$name: not more blocks than the script's (script $py bytes, sparse-write $rs)"
  else bad "$name: blocks" "script $py bytes, sparse-write $rs, through gzip $gz"; fi
  show "the script   " "$s.py.img"; show "sparse-write " "$s.rs.img"
  rm -f "$s.bin" "$s.py.img" "$s.rs.img" "$s.gz.img" "$s.cmp"
}

echo "sparse-compare: $("$PY" -I -c 'import os,sys; v=os.statvfs(sys.argv[1]); print("f_frsize", v.f_frsize, "f_bsize", v.f_bsize)' "$W") in $W ($(uname -sm))"
one runs runs
one tail tail
one zero zero
one empty empty
one lastbyte lastbyte
one dense dense
one phone phone 64 6

# ---- the time
# timed LABEL STDIN(- for none) -- argv... [-- argv...]: the pipe's wall time and each process's CPU
timed() {
  "$PY" -I - "$SIZE" "$@" <<'PYEOF'
import os, subprocess, sys, time
size, label, stdin = int(sys.argv[1]), sys.argv[2], sys.argv[3]
stages = [[]]
for a in sys.argv[5:]:
    if a == '--':
        stages.append([])
    else:
        stages[-1].append(a)
start = time.monotonic()
prev = subprocess.DEVNULL if stdin == '-' else open(stdin, 'rb')
procs = []
for i, argv in enumerate(stages):
    last = i == len(stages) - 1
    p = subprocess.Popen(argv, stdin=prev, stdout=subprocess.DEVNULL if last else subprocess.PIPE, stderr=subprocess.DEVNULL)
    if prev is not subprocess.DEVNULL:
        prev.close()
    prev = p.stdout
    procs.append(p)
cpu, failed = [], []
for p, argv in zip(procs, stages):
    _, status, usage = os.wait4(p.pid, 0)
    name = next(a for a in argv[1:] if not a.startswith('-')) if 'python' in os.path.basename(argv[0]) else argv[0]
    cpu.append(f'{os.path.basename(name)} {usage.ru_utime + usage.ru_stime:.1f} s')
    if os.waitstatus_to_exitcode(status) != 0:
        failed.append(argv[0])
wall = time.monotonic() - start
print(f'{label}: {wall:6.1f} s, {size / 1e6 / wall:5.0f} MB/s; CPU: {", ".join(cpu)}' + (f'; FAILED: {failed}' if failed else ''))
sys.exit(1 if failed else 0)
PYEOF
}
if [ -n "${SPEED:-}" ]; then
  mib=${SPEED%%:*}; data=${SPEED#*:}; [ "$data" != "$SPEED" ] || data=$((mib * 7 / 100))
  echo "--- time: a stream of $mib MiB with about $data MiB of data; load:$(uptime | sed 's/.*load average[s]*://')"
  "$TOOL" debug mkstream phone "$mib" "$data" | "$TOOL" phone sparse-write "$W/speed.img" > /dev/null 2>&1 || bad "speed: the stream"
  SIZE=$(wc -c < "$W/speed.img" | tr -d ' ')
  gen=("$TOOL" debug mkstream phone "$mib" "$data")
  "${gen[@]}" | gzip -1 -c > "$W/speed.gz"
  show "the stream   " "$W/speed.img"
  echo "        packed with gzip -1: $(wc -c < "$W/speed.gz" | tr -d ' ') bytes"
  same() { cmp -s "$2" "$W/speed.img" && ok "speed: $1: the stream's bytes" || bad "speed: $1: not the stream"; show "$1" "$2"; rm -f "$2"; }
  timed "mkstream | gzip -1 -c > /dev/null (the source)     " - -- "${gen[@]}" -- gzip -1 -c || bad "speed: the source"
  timed "mkstream | gzip -1 -c | gunzip -c | sparsewrite.py   " - -- "${gen[@]}" -- gzip -1 -c -- gunzip -c -- "$PY" -I "$HERE/sparsewrite.py" "$W/a.img" || bad "speed: the script through gzip"
  same "the script   " "$W/a.img"
  timed "mkstream | gzip -1 -c | sparse-write --gunzip        " - -- "${gen[@]}" -- gzip -1 -c -- "$TOOL" phone sparse-write "$W/b.img" --gunzip --expect-size "$SIZE" || bad "speed: sparse-write through gzip"
  same "sparse-write " "$W/b.img"
  timed "gunzip -c < PACKED | sparsewrite.py                  " "$W/speed.gz" -- gunzip -c -- "$PY" -I "$HERE/sparsewrite.py" "$W/c.img" || bad "speed: the script from the packed stream"
  same "the script   " "$W/c.img"
  timed "sparse-write --gunzip < PACKED                       " "$W/speed.gz" -- "$TOOL" phone sparse-write "$W/d.img" --gunzip --expect-size "$SIZE" || bad "speed: sparse-write from the packed stream"
  same "sparse-write " "$W/d.img"
  timed "sparsewrite.py < FILE                                " "$W/speed.img" -- "$PY" -I "$HERE/sparsewrite.py" "$W/e.img" || bad "speed: the script from the file"
  same "the script   " "$W/e.img"
  timed "sparse-write < FILE                                  " "$W/speed.img" -- "$TOOL" phone sparse-write "$W/f.img" --expect-size "$SIZE" || bad "speed: sparse-write from the file"
  same "sparse-write " "$W/f.img"
fi

[ -n "${KEEP:-}" ] || rm -rf "$W"
[ $FAILED = 0 ] && echo "ALL SAME" || { echo "DIFFERENT: $FAILED"; exit 1; }
