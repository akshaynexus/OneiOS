#!/bin/bash
# tools/oneios-tool/tests/sym/compare-python.sh   oneios-tool sym against scripts/debug/dsc.py on real addresses.
#
# Runs both on the same address sets taken from real xnuhost logs and compares the standard output byte for byte
# and the exit status; prints the wall time of every run. The sets:
#   a   whole backtraces of the logs (oneios-ubt thread lines, sheng-ubt frames), no --slide
#   b   the same with --slide
#   c   addresses outside the cache, with and without --slide
#   d   two addresses: several slides tie (the "NOT reliable" text)
#   e   a large set (a few thousand addresses of one log), with --slide and without
#
# Needs: ONEIOS_DSC_DIR (the folder with dyld_shared_cache_arm64e, its subcaches and .symbols), the tool built
# (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml; or ONEIOS_TOOL=path), and logs:
# ONEIOS_LOG_DIR=folder with *.log files that hold oneios-ubt or sheng-ubt lines (default out/logs).
# dsc.py writes out/dsc-text.pickle (git-ignored). Work files: out/rustport-sym/.
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
tool=${ONEIOS_TOOL:-$root/tools/oneios-tool/target/release/oneios-tool}
logs=${ONEIOS_LOG_DIR:-$root/out/logs}
work=$root/out/rustport-sym
: "${ONEIOS_DSC_DIR:?set ONEIOS_DSC_DIR to the folder that holds dyld_shared_cache_arm64e}"
[ -x "$tool" ] || { echo "no tool at $tool: build it or set ONEIOS_TOOL" >&2; exit 2; }
ls "$logs"/*.log >/dev/null 2>&1 || { echo "no *.log in $logs: set ONEIOS_LOG_DIR" >&2; exit 2; }
mkdir -p "$work" "$root/out"
rm -f "$work"/*.args "$work"/*.out "$work"/*.err "$work"/*.status

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

status=0
TIMEFORMAT=%R
compare() {  # compare NAME ARGS...
  local name=$1; shift
  local py rs py_status rs_status
  py=$( { time python3 "$root/scripts/debug/dsc.py" "$@" > "$work/$name.py.out" 2> "$work/$name.py.err"; echo $? > "$work/$name.py.status"; } 2>&1 )
  rs=$( { time "$tool" sym "$@" > "$work/$name.rs.out" 2> "$work/$name.rs.err"; echo $? > "$work/$name.rs.status"; } 2>&1 )
  py_status=$(cat "$work/$name.py.status"); rs_status=$(cat "$work/$name.rs.status")
  if cmp -s "$work/$name.py.out" "$work/$name.rs.out" && [ "$py_status" = "$rs_status" ]; then
    printf 'same     %-10s %5d arguments  %7d bytes  status %s  dsc.py %8s s  oneios-tool %6s s\n' "$name" "$#" "$(wc -c < "$work/$name.rs.out")" "$rs_status" "$py" "$rs"
  else
    printf 'DIFFERS  %-10s (status dsc.py %s, oneios-tool %s): diff %s %s\n' "$name" "$py_status" "$rs_status" "$work/$name.py.out" "$work/$name.rs.out"
    diff "$work/$name.py.out" "$work/$name.rs.out" | head -10
    status=1
  fi
}
args_of() { tr '\n' ' ' < "$work/$1.args"; }

"$tool" sym index || exit 2
echo "--- a: whole backtraces, no --slide; b: the same with the slide found"
for name in $(cat "$work/cases"); do
  compare "$name" $(args_of "$name")
  slide=$(sed -n '1s/^slide \(0x[0-9a-f]*\) .*/\1/p' "$work/$name.rs.out")
  [ -n "$slide" ] && compare "b-$name" --slide "$slide" $(args_of "$name")
done
echo "--- c: addresses outside the cache"
compare c1 $(args_of c1)
compare c2 $(args_of c2)
echo "--- d: too few addresses for one slide"
compare d1 $(args_of d1)
compare d2 $(args_of d2)
if [ -f "$work/e-large.args" ]; then
  echo "--- e: the large set"
  slide=$(sed -n '1s/^slide \(0x[0-9a-f]*\) .*/\1/p' "$work/a1.rs.out")
  compare e1-slide --slide "${slide:-0x0}" $(args_of e-large)
  compare e2 $(args_of e-large)
fi
[ $status = 0 ] && echo "all the same as dsc.py" || echo "DIFFERENCES"
exit $status
