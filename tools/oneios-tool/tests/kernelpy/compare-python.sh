#!/bin/bash
# The kernel build's small steps against the Python scripts they replace, on this machine's real inputs: both run
# side by side, on the same files. It ran before the scripts were retired (2026-11-13: README "kernelpy" has the
# result); since then the scripts are gone, and it says so and ends with status 2. compare-golden.sh is what runs
# today (made-up inputs, the Python scripts' frozen output).
#
#   tools/oneios-tool/tests/kernelpy/compare-python.sh WORK ENVFILE [UNFIXED]
#
#   WORK      UniXNU's work directory of a finished hosted build
#             (out/kernel-hosted-<name>/unixnu/oneios/<os profile>/oneios_hosted)
#   ENVFILE   the archive step's environment of that build (tests/kernelgen/save-env.sh): the drivers are compiled
#             in it, as `kernel gen board` compiles them
#   UNFIXED   the linked kernel before kernel/build-hosted.sh's fixup (default: WORK's
#             tree/build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837/mach.development.bcm2837.unstripped)
#
# Compared: `drivers` (--check, and the whole build: every object, the archive's members, objects.json, stdout) in
# WORK's tree with the real compiler; `hosted-fixup` (the kernel it leaves, its line); `iig-fix` on the Linux
# toolchain's iig (external/xnu-linux-toolchain/toolchain/iig; the fixed copy again); `series-check` on this
# checkout. Linux (bwrap for the /usr/bin shims, as tests/kernelgen/compare-python.sh).
set -uo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
[ $# -ge 2 ] || { sed -n '2,19p' "$0" >&2; exit 2; }
for script in drivers/build.py kernel/hosted-fixup.py kernel/linux/iig-fix.py kernel/tests/series-check.py; do
  [ -f "$ROOT/$script" ] || { echo "compare-python: $script is not there (retired): tests/kernelpy/compare-golden.sh" >&2; exit 2; }
done
WORK=$(cd "$1" && pwd); ENVFILE=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
TREE=$WORK/tree
RECORDS=$TREE/build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837
UNFIXED=${3:-$RECORDS/mach.development.bcm2837.unstripped}
TOOL=${ONEIOS_TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
S=${SCRATCH:-$ROOT/out/kernelpy-compare}; rm -rf "$S"; mkdir -p "$S"
fail=0
same() { if [ "$2" = "$3" ]; then echo "same       $1"; else fail=$((fail + 1)); echo "DIFFERENT  $1"; diff <(echo "$2") <(echo "$3") | head -6; fi; }

# in_step CMD...: CMD in the archive step's environment and directory, with UniXNU's /usr/bin shims where the
# host has none (tests/kernelgen/compare-python.sh has the same function).
in_step() {
  local wrap=() shim toolchain
  if [ "$(uname -s)" = Linux ] && ! { [ -x /usr/bin/xcrun ] && [ -x /usr/bin/plutil ] && [ -x /usr/bin/codesign ] && [ -x /bin/csh ]; }; then
    toolchain=$(tr '\0' '\n' < "$ENVFILE" | sed -n 's/^UNIXNU_TOOLCHAIN=//p')
    wrap=(bwrap --dev-bind / / --overlay-src /usr/bin --tmp-overlay /usr/bin)
    for shim in xcrun plutil codesign csh; do
      [ -e "/usr/bin/$shim" ] || wrap+=(--ro-bind "$toolchain/usrbin/$shim" "/usr/bin/$shim")
    done
    wrap+=(--)
  fi
  "${wrap[@]}" env -i ENVFILE="$ENVFILE" bash --noprofile --norc -c '
    while IFS= read -r -d "" kv; do
      name=${kv%%=*}
      case "$name" in ""|*[!A-Za-z0-9_]*|[0-9]*) ;; *) export "$kv" ;; esac
    done < "$ENVFILE"
    unset ENVFILE; cd "$1" && shift && exec "$@"' bash "$TREE" "$@"
}
listing() { ( cd "$1" && find . -type f ! -name '*.a' | LC_ALL=C sort | xargs sha256sum; echo members; ar t liboneiosdrivers.a 2>/dev/null ); }

# ---- drivers: the same output place for both (an object's debug information names it)
out=$TREE/build/oneios-drivers-compare
for side in py rs; do
  rm -rf "$out"
  if [ $side = py ]; then run=(python3 -I "$ROOT/drivers/build.py"); else run=("$TOOL" kernel drivers --repo "$ROOT"); fi
  in_step "${run[@]}" --records "$RECORDS" --out "$out" --check > "$S/$side.check.out" 2> "$S/$side.check.err"; echo "status $?" >> "$S/$side.check.out"
  rm -rf "$out"
  in_step "${run[@]}" --records "$RECORDS" --out "$out" > "$S/$side.build.out" 2> "$S/$side.build.err"; echo "status $?" >> "$S/$side.build.out"
  listing "$out" > "$S/$side.files"
done
rm -rf "$out"
same "drivers --check: stdout, stderr, status" "$(cat "$S/py.check.out" "$S/py.check.err")" "$(cat "$S/rs.check.out" "$S/rs.check.err")"
same "drivers: stdout, stderr, status" "$(cat "$S/py.build.out" "$S/py.build.err")" "$(cat "$S/rs.build.out" "$S/rs.build.err")"
same "drivers: $(grep -c '  ' "$S/py.files") files (objects, objects.json) and the archive's members" "$(cat "$S/py.files")" "$(cat "$S/rs.files")"
# and what the build itself left (the board archive's step)
if [ -d "$TREE/build/oneios-drivers" ]; then
  same "drivers: the objects are the build's own" "$(cd "$TREE/build/oneios-drivers/objects" && sha256sum * | sed 's/ .*//' | sort)" "$(grep 'objects/' "$S/rs.files" | sed 's/ .*//' | sort)"
fi

# ---- hosted-fixup
if [ -f "$UNFIXED" ]; then
  cp "$UNFIXED" "$S/py.macho"; cp "$UNFIXED" "$S/rs.macho"
  same "hosted-fixup: its line and status" "$(python3 -I "$ROOT/kernel/hosted-fixup.py" "$S/py.macho" 2>&1; echo "status $?")" "$("$TOOL" kernel hosted-fixup "$S/rs.macho" 2>&1; echo "status $?")"
  same "hosted-fixup: the kernel ($(wc -c < "$S/py.macho") bytes)" "$(sha256sum < "$S/py.macho")" "$(sha256sum < "$S/rs.macho")"
  cmp -s "$S/py.macho" "$UNFIXED" && { fail=$((fail + 1)); echo "DIFFERENT  hosted-fixup changed nothing: $UNFIXED is fixed already"; }
  rm -f "$S/py.macho" "$S/rs.macho"
else echo "skipped    hosted-fixup: no $UNFIXED"; fi

# ---- iig-fix
iig=$ROOT/external/xnu-linux-toolchain/toolchain/iig
if [ -f "$iig" ]; then
  cp "$iig" "$S/py.iig"; cp "$iig" "$S/rs.iig"
  same "iig-fix: status" "$(python3 -I "$ROOT/kernel/linux/iig-fix.py" "$S/py.iig" 2>&1; echo "status $?")" "$("$TOOL" kernel iig-fix "$S/rs.iig" 2>&1; echo "status $?")"
  same "iig-fix: the toolchain's iig, fixed" "$(sha256sum < "$S/py.iig")" "$(sha256sum < "$S/rs.iig")"
  same "iig-fix: the fixed copy again" "$(python3 -I "$ROOT/kernel/linux/iig-fix.py" "$S/py.iig" 2>&1; echo "status $?")" "$("$TOOL" kernel iig-fix "$S/rs.iig" 2>&1; echo "status $?")"
else echo "skipped    iig-fix: no $iig"; fi

# ---- series-check
same "series-check: this checkout" "$(cd "$ROOT" && python3 -I kernel/tests/series-check.py 2>&1; echo "status $?")" "$(cd "$ROOT" && "$TOOL" kernel series-check 2>&1; echo "status $?")"
echo "kernelpy compare-python: $fail different"
[ $fail = 0 ]
