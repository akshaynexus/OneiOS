#!/bin/bash
# The prelink's hand tools against the frozen output of the Python scripts they replaced (retired 2026-11-13):
# kernel/prelink/{classes,stubs-check,unmapped,port-fixture}.py and kernel/prelink/lead/{between,imports}.py ->
# `oneios-tool prelink classes | stubs-check | unmapped | port-fixture | lead-between | lead-imports`.
#
#   tools/oneios-tool/tests/prelinkpy/compare-golden.sh                       the cases that need the kernelcache
#                                                                             alone, against golden/kernelcache.txt
#   tools/oneios-tool/tests/prelinkpy/compare-golden.sh PRELINK_OUT KERNEL    also the lead tools' cases, against
#                                                                             golden/KERNEL-SHA256-PREFIX.txt
#   UPDATE=1 ...     write the golden from this run instead
#   IMPL=python ...  run the Python scripts instead of the commands: only at a commit that still has them (the
#                    goldens were frozen with IMPL=python UPDATE=1 at the last such commit, then checked with the
#                    commands)
#
#   PRELINK_OUT   the output folder of kernel/prelink/build.sh for KERNEL (out/kernel/24A446: its
#                 work/match.map.json and kernel.kc.prelink.json are what the lead tools read)
#   KERNEL        the hosted kernel that collection was built from (xnu.macho). A golden exists for the Linux-built
#                 kernel 13ed0b70... (the main checkout's out/kernel/current-hosted-pc, collection ffbe95f7...);
#                 another kernel has none: the script says so and exits 3 (UPDATE=1 writes one: a record, not a
#                 comparison with Python).
#
# A case is: the command's stdout (hashed where it is long), its stderr, its status, and the SHA-256 of the file it
# wrote; the scratch folder's path is taken out. No Apple byte is kept: names, addresses, sizes and hashes only.
# The inputs: KERNELCACHE (default out/kernel-inputs/24A446/kernelcache.macho: Apple's, read in place), XNUPRELINK
# (default external/XnuPrelink: its reviewed fixture, for port-fixture), made-up collections of `oneios-tool debug
# stub-kc` for stubs-check, and a made-up prelink report (a slide and a link address) for classes and unmapped.
# `unmapped` prints llvm-mc's disassembly, whose text follows the LLVM version: those lines are counted, not hashed.
# Env: ONEIOS_TOOL (the binary), KEEP=1 (keep the scratch folder), TMPDIR, GOLDEN=DIR (another place for the goldens)
# and FULL=1 (the disassembly hashed too: with GOLDEN, for a comparison on one machine). Needs llvm-mc and llvm-nm.
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
TOOL=${ONEIOS_TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
IMPL=${IMPL:-rust}
CACHE=${KERNELCACHE:-$ROOT/out/kernel-inputs/24A446/kernelcache.macho}
XP=${XNUPRELINK:-$ROOT/external/XnuPrelink}
[ -f "$XP/fixtures/fileset-24A446-symbols.json" ] || XP=$ROOT/external/Droid-VM-UniXNU/kits/XnuPrelink
[ $# -eq 0 ] || [ $# -eq 2 ] || { sed -n '2,29p' "$0" >&2; exit 2; }
[ -x "$TOOL" ] || { echo "compare-golden: no $TOOL (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)" >&2; exit 2; }
[ -f "$CACHE" ] || { echo "compare-golden: no kernelcache at $CACHE (scripts/image/kernel-inputs.sh, or KERNELCACHE=FILE)" >&2; exit 2; }
W=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/prelinkpy.XXXXXX")" && pwd -P)
[ "${KEEP:-0}" = 1 ] || trap 'rm -rf "$W"' EXIT
FOUND=$W/found.txt; : > "$FOUND"
sha() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
D=$ROOT/kernel/prelink
GOLDEN=${GOLDEN:-$HERE/golden}

# step NAME ARGS...: one tool, by the chosen implementation. REPO is the checkout the lead tools work in.
step() {
  local name=$1; shift
  if [ "$IMPL" = python ]; then
    local script
    case "$name" in
      lead-between) script=$REPO/kernel/prelink/lead/between.py ;;   # they find their checkout from their own place
      lead-imports) script=$REPO/kernel/prelink/lead/imports.py ;;
      *) script=$D/$name.py ;;
    esac
    [ -f "$script" ] || { echo "compare-golden: $script is not there (retired): IMPL=python cannot run" >&2; exit 2; }
    python3 -I "$script" "$@"
  else
    case "$name" in
      lead-*) "$TOOL" prelink "$name" "$@" --repo "$REPO" ;;
      *) "$TOOL" prelink "$name" "$@" ;;
    esac
  fi
}
# a NAME COMMAND...: one case: stdout (as it is up to 12 lines, else its hash and line count; the disassembly's
# lines counted apart), stderr, the status.
a() {
  local name=$1; shift
  "$@" > "$W/case.out" 2> "$W/case.err"; local status=$?
  # (lead-between prints an unpaired function's first words: Apple's code, which no golden keeps)
  sed -e "s#$W#W#g" -e "s#$ROOT#ROOT#g" -e 's/| \([0-9a-f]\{8\} \)*[0-9a-f]\{8\}$/| (its first words)/' "$W/case.out" > "$W/case.txt"
  if [ -n "${FULL:-}" ]; then cp "$W/case.txt" "$W/case.said"; else grep -Ev '^  0x[0-9a-f]+:' "$W/case.txt" > "$W/case.said"; fi
  {
    echo "== $name"
    if [ "$(wc -l < "$W/case.said")" -le 12 ]; then cat "$W/case.said"; else echo "stdout: $(wc -l < "$W/case.said" | tr -d ' ') lines, sha256 $(sha < "$W/case.said" | cut -c1-64)"; fi
    n=$(grep -Ec '^  0x[0-9a-f]+:' "$W/case.txt"); [ "$n" = 0 ] || echo "disassembly: $n lines"
    sed -e "s#$W#W#g" -e 's/^/stderr: /' "$W/case.err"; echo "status $status"
  } >> "$FOUND"
  [ -z "${SHOW:-}" ] || cat "$W/case.txt"
}
wrote() { local f; for f in "$@"; do if [ -f "$f" ]; then echo "sha256 $(sha < "$f" | cut -c1-64) $(wc -c < "$f" | tr -d ' ') bytes ${f#"$W"/}"; else echo "not written: ${f#"$W"/}"; fi >> "$FOUND"; done; }
REPO=$ROOT

# ---- stubs-check: made-up collections
"$TOOL" debug stub-kc "$W/good.kc" 666 0; "$TOOL" debug stub-kc "$W/bad.kc" 660 14 "a note after the last byte"
a "stubs-check: 666 good stubs" step stubs-check "$W/good.kc"
a "stubs-check: 14 of 674 bad (the first ten are listed)" step stubs-check "$W/bad.kc"

# ---- classes: the classes of kernel/prelink/classes-24A446-v54.json, with the slide that record was made with
CLASSES="IOAESAccelerator IOCryptoAccelerator IOMobileFramebufferLegacy IOHIDDevice IOSurface IOGPUDevice IOGPU AGXAccelerator IOAcceleratorES IOGPUDeviceUserClient AGXDeviceUserClient"
echo '{"collection_slide": "0x1f000000000", "native_link_address": "0xfffffff007004000"}' > "$W/report.json"
a "classes: eleven classes, with their linked addresses" step classes --xnuprelink "$XP" --kernelcache "$CACHE" --prelink-report "$W/report.json" --out "$W/classes.json" $CLASSES
wrote "$W/classes.json"
{ echo "== classes: that record is the committed kernel/prelink/classes-24A446-v54.json"; cmp -s "$W/classes.json" "$D/classes-24A446-v54.json" && echo yes || echo NO; } >> "$FOUND"
a "classes: without a report, a class by its kext, a class that is not there" step classes --xnuprelink "$XP" --kernelcache "$CACHE" --out "$W/classes2.json" IOHIDDevice@com.apple.iokit.IOHIDFamily NoSuchClassAnywhere IOSurface@com.apple.iokit.IOHIDFamily
wrote "$W/classes2.json"

# ---- port-fixture: a made-up reviewed file (one record by hand, one that an earlier run ported)
SUM=$(sha < "$CACHE" | cut -c1-64)
reviewed() { printf '{\n "kernelcache_sha256": "%s",\n "entry": "com.apple.kernel",\n "about": "made-up",\n "functions": [\n  {"original": "0xfffffe0007000000", "native_symbol": "_by_hand", "evidence": "made-up"},\n  {"original": "0xfffffe0007000004", "native_symbol": "_old", "ported": true}\n ],\n "data": []\n}\n' "$1" > "$W/reviewed.json"; }
reviewed "$SUM"
a "port-fixture: XnuPrelink's reviewed functions carried over" step port-fixture --xnuprelink "$XP" --kernelcache "$CACHE" --fixture "$XP/fixtures/fileset-24A446-symbols.json" --reviewed "$W/reviewed.json"
wrote "$W/reviewed.json"
cp "$W/reviewed.json" "$W/ported.json"
a "port-fixture: again on its own output" step port-fixture --xnuprelink "$XP" --kernelcache "$CACHE" --fixture "$XP/fixtures/fileset-24A446-symbols.json" --reviewed "$W/reviewed.json"
wrote "$W/reviewed.json"
reviewed 0000
a "port-fixture: a reviewed file for another kernelcache" step port-fixture --xnuprelink "$XP" --kernelcache "$CACHE" --fixture "$XP/fixtures/fileset-24A446-symbols.json" --reviewed "$W/reviewed.json"
wrote "$W/reviewed.json"

# ---- unmapped: original kernel functions (the first three that port-fixture carried over), and return addresses
# in a kext (after the instructions that follow IOHIDDevice's static initializer: its call of the OSMetaClass
# constructor goes through a stub to the kernel; the others are no direct branch)
FUNCS=$(grep -o '"original": "0x[0-9a-f]*"' "$W/ported.json" | sed -n '3,5p' | sed 's/.*"\(0x[0-9a-f]*\)"/\1/' | tr '\n' ' ')
SITE=$(awk '/"IOHIDDevice": \{/ {on=1} on && /"static_initializer_site"/ {gsub(/[",]/, ""); print $2; exit}' "$W/classes.json")
RETURNS=$(for n in 8 12 16 20 24 28 32 36; do printf '0x%x ' $((SITE + n)); done)
a "unmapped --target: three original kernel functions" step unmapped --xnuprelink "$XP" --kernelcache "$CACHE" --report "$W/report.json" --lines 6 --target $FUNCS
a "unmapped: eight return addresses after a static initializer (original addresses)" step unmapped --xnuprelink "$XP" --kernelcache "$CACHE" --report "$W/report.json" --lines 4 $RETURNS
LINKED=$(for n in 8 12 16 20 24 28 32 36; do printf '0x%x ' $((SITE + n + 0x1f000000000)); done)
a "unmapped: the same as linked in kernel.kc" step unmapped --xnuprelink "$XP" --kernelcache "$CACHE" --report "$W/report.json" --lines 4 $LINKED
golden=$GOLDEN/kernelcache.txt

# ---- the lead tools: a made-up checkout with what they read at the places they read it
if [ $# -eq 2 ]; then
  OUT=$(cd "$1" && pwd); KERNEL=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
  [ -f "$OUT/work/match.map.json" ] && [ -f "$OUT/kernel.kc.prelink.json" ] && [ -f "$KERNEL" ] || { echo "compare-golden: $OUT is no output of kernel/prelink/build.sh, or $KERNEL is not there" >&2; exit 2; }
  cp "$FOUND" "$W/kernelcache.found"; : > "$FOUND"
  REPO=$W/repo; mkdir -p "$REPO/kernel/prelink/lead" "$REPO/out/kernel/current-hosted" "$REPO/out/kernel-inputs/24A446"
  cp "$D/reviewed-24A446-v54.json" "$REPO/kernel/prelink/"
  for f in between.py imports.py; do [ ! -f "$D/lead/$f" ] || cp "$D/lead/$f" "$REPO/kernel/prelink/lead/"; done
  ln -s "$ROOT/external" "$REPO/external"; ln -s "$OUT" "$REPO/out/kernel/24A446"
  ln -s "$CACHE" "$REPO/out/kernel-inputs/24A446/kernelcache.macho"; ln -s "$KERNEL" "$REPO/out/kernel/current-hosted/xnu.macho"
  L=$REPO/kernel/prelink/lead/lead-reviewed.json
  a "lead-imports: every kext" step lead-imports
  a "lead-imports --kext com.apple.iokit.IOHIDFamily" step lead-imports --kext com.apple.iokit.IOHIDFamily
  a "lead-between: a block where every function is paired" step lead-between _vnode_isreg _vnode_isfifo
  a "lead-between --add: the counts differ" step lead-between _vnode_mount _vnode_mountedhere --add "a made-up note"; wrote "$L"
  a "lead-imports --add" step lead-imports --add; wrote "$L"
  a "lead-imports: after --add" step lead-imports
  a "lead-between --add: one unpaired function between two paired ones" step lead-between _vnode_istty _vnode_isrecycled --add "a made-up note"; wrote "$L"
  REPO=$ROOT
  golden=$GOLDEN/$(sha < "$KERNEL" | cut -c1-16).txt
fi

# ---- the result
if [ "${UPDATE:-0}" = 1 ]; then
  mkdir -p "$GOLDEN"; cp "$FOUND" "$golden"
  [ ! -f "$W/kernelcache.found" ] || cp "$W/kernelcache.found" "$GOLDEN/kernelcache.txt"
  echo "compare-golden: $golden written from IMPL=$IMPL ($(grep -c '^== ' "$FOUND") cases)"; exit 0
fi
block() { awk -v n="== $1" '$0 == n {on=1; print; next} /^== / {on=0} on' "$2"; }
same=0; differ=0
compare() { # FOUND GOLDEN
  [ -f "$2" ] || { echo "compare-golden: no golden for this kernel ($2)"; exit 3; }
  local name
  while read -r name; do
    if [ "$(block "$name" "$1")" = "$(block "$name" "$2")" ]; then same=$((same + 1)); echo "same       $name"
    else differ=$((differ + 1)); echo "DIFFERENT  $name"; diff <(block "$name" "$2") <(block "$name" "$1") | head -8; fi
  done < <(grep '^== ' "$2" | sed 's/^== //')
  [ "$(grep -c '^== ' "$1")" = "$(grep -c '^== ' "$2")" ] || { differ=$((differ + 1)); echo "DIFFERENT  the list of cases ($(basename "$2"))"; }
}
if [ -f "$W/kernelcache.found" ]; then compare "$W/kernelcache.found" "$GOLDEN/kernelcache.txt"; fi
compare "$FOUND" "$golden"
echo "prelinkpy (IMPL=$IMPL): $same the same, $differ different"
[ $differ = 0 ]
