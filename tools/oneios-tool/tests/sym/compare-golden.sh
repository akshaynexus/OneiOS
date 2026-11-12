#!/bin/bash
# tools/oneios-tool/tests/sym/compare-golden.sh   oneios-tool sym against the frozen output of scripts/debug/dsc.py
# (the Python symbolizer it replaced, deleted on 2026-11-12 after 18 of 18 address sets gave the same bytes on the
# Mac's real logs and on Linux: tools/oneios-tool/README.md "sym").
#
# The address sets come from two stand-in logs that tests/sym/make-log.py writes from the dyld shared cache of
# 24A446 (real return addresses, frames drawn at random with a fixed seed; the script makes them):
#   a   whole backtraces of the logs (oneios-ubt thread lines, sheng-ubt frames), no --slide
#   b   the same with --slide
#   c   addresses outside the cache, with and without --slide
#   d   two addresses: several slides tie (the "NOT reliable" text)
#   e   a large set (4000 addresses of one log), with --slide and without
# golden/24A446.txt holds per set: its name, the SHA-256 of its arguments, the SHA-256 of dsc.py's stdout and its
# exit status (no Apple bytes). A set whose arguments are not the golden's (another cache) fails as "other
# arguments". UPDATE=1 writes the file from this run (a record of what the tool gives, not a comparison).
#
# Needs: ONEIOS_DSC_DIR (the folder with dyld_shared_cache_arm64e, its subcaches and .symbols; from the image:
# oneios-tool image apfs-copy out/image/24A446/systemos.img System/Library/Caches/com.apple.dyld DIR), the tool
# built (or ONEIOS_TOOL=path), python3 (make-log.py and the set maker below). Work files: out/rustport-sym/.
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
tool=${ONEIOS_TOOL:-$root/tools/oneios-tool/target/release/oneios-tool}
work=$root/out/rustport-sym
golden=$here/golden/24A446.txt
: "${ONEIOS_DSC_DIR:?set ONEIOS_DSC_DIR to the folder that holds dyld_shared_cache_arm64e}"
[ -x "$tool" ] || { echo "no tool at $tool: build it or set ONEIOS_TOOL" >&2; exit 2; }
rm -rf "$work"; mkdir -p "$work/logs"
logs=$work/logs
python3 -I "$here/make-log.py" "$ONEIOS_DSC_DIR" "$logs/standin-a.log" || exit 2
python3 -I "$here/make-log.py" "$ONEIOS_DSC_DIR" "$logs/standin-b.log" --slide 0x124d8000 --seed 7 --stacks 350 || exit 2

# The address sets: one file per case, the arguments one per line.
python3 - "$work" "$logs"/*.log <<'EOF' || exit 2
import re, sys
work, logs = sys.argv[1], sys.argv[2:]
thread = re.compile(r'^\[[^\]]*\] guest\s+oneios-ubt: \S+\[\d+\] thread 0x[0-9a-f]+ state 0x[0-9a-f]+:((?: 0x[0-9a-f]+)+)$')
frame = re.compile(r'sheng-ubt sheng-exc: #\d+ fp=0x[0-9a-f]+ ret=(0x[0-9a-f]+)$')
first_lr = re.compile(r'sheng-ubt sheng-exc: pc=0x[0-9a-f]+ lr=(0x[0-9a-f]+) ')
def write(name, args):
    open(f'{work}/{name}.args', 'w').write(''.join(a + '\n' for a in args))
whole, every, crashes = [], [], []
for path in logs:
    stacks, crash, got = [], None, False
    for line in open(path, errors='replace'):
        line = line.rstrip('\n')
        m = thread.match(line)
        if m:
            stacks.append(m.group(1).split())
            continue
        m = first_lr.search(line)
        if m:
            crash = [m.group(1)]
            continue
        m = frame.search(line)
        if m and crash is not None:
            crash.append(m.group(1))
        elif crash and len(crash) >= 4 and not got:
            crashes.append(crash); crash = None; got = True
    long = [s for s in stacks if len(s) >= 12]
    whole += long[:2]
    if not every and len(stacks) > 300:
        every = [a for s in stacks for a in s]
if not whole and not crashes:
    sys.exit('no oneios-ubt thread line of 12 addresses and no sheng-ubt backtrace in the logs')
names = []
for i, s in enumerate(whole[:4] + crashes[:2]):
    write(f'a{i + 1}', s); names.append((f'a{i + 1}', s))
first = (whole or crashes)[0]
write('c1', ['0x1007c9084', '0x16f1648f8', '0x0', '0x7fffffffffff', '0xffffffffffffffff'])
write('c2', ['--slide', '0xedb4000', '0x1007c9084', '0x16f1648f8', '0x0', '0x7fffffffffff', '0x180000000'])
write('d1', first[1:3])
write('d2', first[:1])
if every:
    large = every[:4000]
    write('e-large', large)                     # --slide is added by the script
open(f'{work}/cases', 'w').write(' '.join(n for n, _ in names) + '\n')
EOF

TIMEFORMAT=%R
: > "$work/found.txt"
run() {  # run NAME ARGS...
  local name=$1; shift
  local took
  took=$( { time "$tool" sym "$@" > "$work/$name.out" 2> "$work/$name.err"; echo $? > "$work/$name.status"; } 2>&1 )
  printf '%s %s %s %s\n' "$name" "$(printf '%s\n' "$@" | sha256sum | cut -c1-64)" "$(sha256sum < "$work/$name.out" | cut -c1-64)" "$(cat "$work/$name.status")" >> "$work/found.txt"
  printf 'ran      %-10s %5d arguments  %7d bytes  status %s  %6s s\n' "$name" "$#" "$(wc -c < "$work/$name.out")" "$(cat "$work/$name.status")" "$took"
}
args_of() { tr '\n' ' ' < "$work/$1.args"; }

"$tool" sym index || exit 2
for name in $(cat "$work/cases"); do
  run "$name" $(args_of "$name")
  slide=$(sed -n '1s/^slide \(0x[0-9a-f]*\) .*/\1/p' "$work/$name.out")
  [ -n "$slide" ] && run "b-$name" --slide "$slide" $(args_of "$name")
done
run c1 $(args_of c1)
run c2 $(args_of c2)
run d1 $(args_of d1)
run d2 $(args_of d2)
if [ -f "$work/e-large.args" ]; then
  slide=$(sed -n '1s/^slide \(0x[0-9a-f]*\) .*/\1/p' "$work/a1.out")
  run e1-slide --slide "${slide:-0x0}" $(args_of e-large)
  run e2 $(args_of e-large)
fi
if [ -n "${UPDATE:-}" ]; then mkdir -p "$here/golden"; cp "$work/found.txt" "$golden"; echo "wrote $golden (this run's output, not a comparison)"; exit 0; fi
status=0
while read -r name args out code; do
  want=$(grep "^$name " "$golden") || { echo "NO GOLDEN $name"; status=1; continue; }
  set -- $want
  if [ "$2" != "$args" ]; then echo "OTHER ARGUMENTS $name (not the golden's address set: another cache or log?)"; status=1
  elif [ "$3" != "$out" ] || [ "$4" != "$code" ]; then echo "DIFFERS  $name (stdout or status is not dsc.py's): $work/$name.out"; status=1; fi
done < "$work/found.txt"
[ "$(wc -l < "$work/found.txt")" = "$(wc -l < "$golden")" ] || { echo "the run has $(wc -l < "$work/found.txt") sets, the golden $(wc -l < "$golden")"; status=1; }
[ $status = 0 ] && echo "sym compare: $(wc -l < "$golden") sets the same as dsc.py's frozen output" || echo "sym compare: FAILED"
exit $status
