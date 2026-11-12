#!/bin/bash
# tools/oneios-tool/tests/prelink/compare-python.sh   the ported steps of kernel/prelink/build.sh against the Python
# steps on the real kernelcache: each step is run by both with the same arguments into its own folder; stdout, the
# exit status and every file written must be the same, byte for byte (cmp).
#
#   roots         roots.py KERNELCACHE select.json PROFILE              (and without PROFILE)
#   hosted        hosted.py KERNELCACHE kernelcache.hosted.macho hosted.json --closure select.json   (and without
#                 --closure; X18=1 also with --x18, which needs llvm-mc and takes minutes in Python)
#
# Needs: KERNELCACHE (default out/kernel-inputs/24A446/kernelcache.macho; scripts/image/kernel-inputs.sh), the tool
# built (or ONEIOS_TOOL=path), python3 3.11+. Work files: out/rustport-prelink/ (Apple's bytes: never commit).
# The steps that are not ported (native-input, merge-reviewed, match, align, kclasses, structors, vtables, emit,
# audit, record) are not compared here: there is nothing to compare yet.
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
tool=${ONEIOS_TOOL:-$root/tools/oneios-tool/target/release/oneios-tool}
cache=${KERNELCACHE:-$root/out/kernel-inputs/24A446/kernelcache.macho}
profile=$root/kernel/unixnu/profiles/os/ios-27.0.1-24A446/profile.toml
py=$root/kernel/prelink
work=${WORK:-$root/out/rustport-prelink}
[ -x "$tool" ] || { echo "no tool at $tool: build it or set ONEIOS_TOOL" >&2; exit 2; }
[ -f "$cache" ] || { echo "no kernelcache at $cache: run scripts/image/kernel-inputs.sh or set KERNELCACHE" >&2; exit 2; }
rm -rf "$work"; mkdir -p "$work"
status=0
TIMEFORMAT=%R
# step NAME PYTHON-SCRIPT RUST-STEP ARGS...: `@` in ARGS is the side's own folder
step() {
  local name=$1 script=$2 rust=$3; shift 3
  local side args a took_py took_rs
  for side in py rs; do
    mkdir -p "$work/$name/$side"
    args=()
    for a in "$@"; do args+=("${a//@/$work/$name/$side}"); done
    if [ $side = py ]; then
      took_py=$( { time python3 "$py/$script" "${args[@]}" > "$work/$name/py.out" 2> "$work/$name/py.err"; echo $? > "$work/$name/py.status"; } 2>&1 )
    else
      took_rs=$( { time "$tool" prelink "$rust" "${args[@]}" > "$work/$name/rs.out" 2> "$work/$name/rs.err"; echo $? > "$work/$name/rs.status"; } 2>&1 )
    fi
  done
  # the folder names differ in the messages only
  sed -i "s#$work/$name/py#DIR#g" "$work/$name/py.out"; sed -i "s#$work/$name/rs#DIR#g" "$work/$name/rs.out"
  local bad=""
  cmp -s "$work/$name/py.out" "$work/$name/rs.out" || bad="$bad stdout"
  cmp -s "$work/$name/py.status" "$work/$name/rs.status" || bad="$bad status"
  [ "$(cat "$work/$name/py.status")" = 0 ] || bad="$bad python-failed($(head -c 200 "$work/$name/py.err"))"
  diff -rq "$work/$name/py" "$work/$name/rs" > "$work/$name/files.diff" 2>&1 || bad="$bad files($(head -2 "$work/$name/files.diff" | tr '\n' ' '))"
  local files; files=$(find "$work/$name/py" -type f | wc -l)
  if [ -z "$bad" ]; then
    printf 'same   %-28s %d files  python %s s  rust %s s  %s\n' "$name" "$files" "$took_py" "$took_rs" "$(head -c 110 "$work/$name/py.out")"
  else
    printf 'DIFFER %-28s%s\n' "$name" "$bad"; status=1
  fi
}

step roots roots.py roots "$cache" @/select.json "$profile"
step roots-no-profile roots.py roots "$cache" @/select.json
cp "$work/roots/py/select.json" "$work/select.json"
step hosted hosted.py hosted "$cache" @/kernelcache.hosted.macho @/hosted.json --closure "$work/select.json"
step hosted-every-kext hosted.py hosted "$cache" @/kernelcache.hosted.macho @/hosted.json
[ -z "${X18:-}" ] || step hosted-x18 hosted.py hosted "$cache" @/kernelcache.hosted.macho @/hosted.json --closure "$work/select.json" --x18
[ -n "${KEEP:-}" ] || find "$work" -name '*.macho' -delete
[ $status = 0 ] && echo "prelink compare: every ported step the same as Python" || echo "prelink compare: FAILED"
exit $status
