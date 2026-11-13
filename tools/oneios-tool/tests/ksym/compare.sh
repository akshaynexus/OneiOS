#!/usr/bin/env bash
# oneios-tool sym kernel against llvm-symbolizer on a real kernel collection and its unprelinked kernel.
#
#   tools/oneios-tool/tests/ksym/compare.sh                   # the pair `sym kernel` finds itself: the newest
#                                                             # out/kernel/*/kernel.kc and the xnu.macho its record names
#   tools/oneios-tool/tests/ksym/compare.sh KC KERNEL [LOG]   # these files; LOG: a real xnuhost session log
#
# Without LOG the log is a stand-in that `oneios-tool debug ksym-make-log` writes from the two files: the backtrace of the panic of
# 2026-11-12 with this kernel's addresses (the Mac's log and kernel are not on every machine). With the Mac's
# files: compare.sh PATH/kernel.kc PATH/xnu.macho PATH/xnuhost.log, and DSYM=PATH/xnu.macho.dSYM when the
# kernel's object files are not on this machine (a Mac-built kernel's debug map names the Mac's paths).
#
# What runs (`oneios-tool debug ksym-compare` has the checks: crates/oneios-testkit): the first call (it builds the index, in a folder of its own) and five
# later calls are timed; every address of the log, every instruction of the functions it names and SAMPLE more
# addresses are compared with `llvm-symbolizer --inlining` on a dSYM that dsymutil makes from the same kernel (or
# DSYM); then the same again with the tool reading that dSYM instead of the debug map, and with the collection
# alone (--no-kernel).
#
# Environment: ONEIOS_TOOL (the binary; default: this workspace's target/release/oneios-tool), DSYM, SAMPLE
# (default 20000), KEEP=1 (keep the work folder), TMPDIR.
# Exit status: 0 the same or skipped (a line says why), 1 a difference, 2 a wrong command line.
# Nothing of Apple's is written outside the work folder; the index there is deleted with it.
set -u
here=$(cd "$(dirname "$0")" && pwd)
tool=${ONEIOS_TOOL:-$here/../../target/release/oneios-tool}
sample=${SAMPLE:-20000}

skip() { echo "tests/ksym/compare.sh: skipped: $*"; exit 0; }
[ $# -eq 0 ] || [ $# -eq 2 ] || [ $# -eq 3 ] || { sed -n '2,22p' "$0"; exit 2; }
[ -x "$tool" ] || skip "no oneios-tool at $tool (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)"
symbolizer=$(command -v llvm-symbolizer || true)
[ -n "$symbolizer" ] || skip "no llvm-symbolizer (the reference)"

work=$(mktemp -d "${TMPDIR:-/tmp}/ksym-compare.XXXXXX")
[ "${KEEP:-0}" = 1 ] || trap 'rm -rf "$work"' EXIT
export ONEIOS_KSYM_INDEX=$work/index

if [ $# -ge 2 ]; then
  kc=$1 kernel=$2
  [ -f "$kc" ] || skip "$kc is not there"
  [ -f "$kernel" ] || skip "$kernel is not there"
else
  # The tool's own choice; its first two lines name the files.
  picked=$("$tool" sym kernel index -j2 2>"$work/pick.err") || skip "no kernel collection found: $(tail -1 "$work/pick.err")"
  kc=$(printf '%s\n' "$picked" | sed -n 's/^kc      \(.*\)  sha256 .*/\1/p')
  kernel=$(printf '%s\n' "$picked" | sed -n 's/^kernel  \(.*\)  sha256 .*/\1/p')
  [ -n "$kernel" ] || skip "no unprelinked kernel found for $kc"
  rm -rf "$work/index"
fi
echo "kc      $kc"
echo "kernel  $kernel"
echo "load    $(cut -d' ' -f1-3 /proc/loadavg 2>/dev/null || uptime)"

expect=()
if [ $# -eq 3 ]; then
  log=$3
  [ -f "$log" ] || skip "$log is not there"
else
  log=$work/stand-in.log
  "$tool" debug ksym-make-log "$kc" "$kernel" --expect "$work/expect" >"$log" || { echo "debug ksym-make-log failed"; exit 1; }
  expect=(--expect "$work/expect")
  echo "log     a stand-in from debug ksym-make-log ($(grep -c ' frame 0x' "$log") frames)"
fi

# The reference's DWARF: DSYM, else a dSYM that dsymutil makes from the kernel's object files.
ref=
made=
if [ -n "${DSYM:-}" ]; then
  ref=$(find "$DSYM" -type f -path '*Contents/Resources/DWARF/*' 2>/dev/null | head -1)
  [ -n "$ref" ] || { [ -f "$DSYM" ] && ref=$DSYM; }
  [ -n "$ref" ] || skip "DSYM=$DSYM has no DWARF file"
elif command -v dsymutil >/dev/null; then
  if dsymutil -j 2 "$kernel" -o "$work/ref.dSYM" >"$work/dsymutil.log" 2>&1 && ! grep -q 'warning: .*\(No such file\|unable to open\|timestamp\)' "$work/dsymutil.log"; then
    ref=$(find "$work/ref.dSYM" -type f -path '*Contents/Resources/DWARF/*' | head -1)
    made=1
  else
    echo "dsymutil could not read the kernel's object files ($(grep -c warning "$work/dsymutil.log") warnings): no DWARF for the reference"
  fi
fi
if [ -z "$ref" ]; then
  ref=$kernel
  echo "reference: $symbolizer on the kernel's symbol table (function names only; give DSYM=... for file:line)"
else
  echo "reference: $symbolizer --inlining on $ref"
fi

now() { date +%s%N; }
ms() { echo $(( ($2 - $1) / 1000000 )); }
dsym_arg=()
[ -n "${DSYM:-}" ] && dsym_arg=(--dsym "$DSYM")

echo
echo "== the first call (builds the index) and five later ones"
t0=$(now)
"$tool" sym kernel --kc "$kc" --kernel "$kernel" "${dsym_arg[@]}" "$log" >"$work/first.txt" 2>"$work/first.err" || { cat "$work/first.err"; echo "FAIL: the tool"; exit 1; }
t1=$(now)
echo "first call: $(ms "$t0" "$t1") ms  ($(grep 'index built' "$work/first.err" | sed 's/.*: //'))"
later=()
for _ in 1 2 3 4 5; do
  t0=$(now)
  "$tool" sym kernel --kc "$kc" --kernel "$kernel" "${dsym_arg[@]}" "$log" >"$work/later.txt" 2>/dev/null
  t1=$(now)
  later+=("$(ms "$t0" "$t1")")
done
echo "later calls: ${later[*]} ms"
# The two outputs differ only in the index line ("built now in" / "built before in").
for f in first later; do sed 's/(built [a-z]* in [0-9.]* s)/(built)/' "$work/$f.txt" >"$work/$f.norm"; done
cmp -s "$work/first.norm" "$work/later.norm" || { echo "FAIL: a later call's output is not the first call's"; exit 1; }
echo
cat "$work/later.txt"

status=0
echo
echo "== against the reference (the tool's DWARF: ${DSYM:+DSYM}${DSYM:-what it finds itself})"
"$tool" debug ksym-compare --tool "$tool" --kc "$kc" --kernel "$kernel" --log "$log" --ref "$ref" --index "$work/index" \
  --sample "$sample" --symbolizer "$symbolizer" "${expect[@]}" "${dsym_arg[@]}" || status=1

if [ -n "$made" ]; then
  echo
  echo "== the same with the tool reading the dSYM (--dsym) instead of the debug map"
  "$tool" debug ksym-compare --tool "$tool" --kc "$kc" --kernel "$kernel" --log "$log" --ref "$ref" --index "$work/index-dsym" \
    --sample "$sample" --symbolizer "$symbolizer" "${expect[@]}" --dsym "$work/ref.dSYM" | grep -v '^  same'
  [ "${PIPESTATUS[0]}" = 0 ] || status=1
fi
[ "$status" = 0 ] && echo && echo "tests/ksym/compare.sh: PASS" || { echo; echo "tests/ksym/compare.sh: FAIL"; }
exit $status
