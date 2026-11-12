#!/bin/bash
# tools/oneios-tool/tests/prelink/compare-golden.sh   every step of kernel/prelink/build.sh against the frozen
# output of the Python steps it replaced (kernel/prelink/*.py and XnuPrelink's fileset_match.py, deleted from
# this repository after the comparison of 2026-11-12: tools/oneios-tool/README.md "prelink").
#
# The chain is run with `oneios-tool prelink STEP` on the real kernelcache and a hosted kernel; every file written
# and every step's stdout is hashed and compared with golden/KERNEL-SHA256-PREFIX.txt (name, size, SHA-256: no
# Apple bytes). A golden exists for the two Linux-built kernels the Python chain was run on:
#   13ed0b70...  (out/kernel/current-hosted-pc/xnu.macho, with its reference.macho ac72f98d...)
#   399f50fa...  (out/kernel/current-hosted-pc2/xnu.macho, with the pc tree's reference.macho)
# Without KERNEL only roots and hosted are run, against golden/kernelcache.txt. Another kernel has no golden:
# the script then says so and exits 3 (UPDATE=1 writes one from this run: a record of what the Rust chain gives,
# not a comparison with Python).
#
# Before hashing, in the JSON records and the stdout: the work folder is taken out, the commit of
# kernel.kc.build.json ("oneios") is replaced, and an absolute "path"/"input" keeps its file name only.
#
# Needs: KERNELCACHE (default out/kernel-inputs/24A446/kernelcache.macho), KERNEL, REFERENCE (optional),
# XNUPRELINK (default external/XnuPrelink of this checkout or of ONEIOS_MAIN), the tool built (or ONEIOS_TOOL).
# Work files: WORK (default out/rustport-prelink/; Apple's bytes: never commit). KEEP=1 keeps the big files.
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
tool=${ONEIOS_TOOL:-$root/tools/oneios-tool/target/release/oneios-tool}
cache=${KERNELCACHE:-$root/out/kernel-inputs/24A446/kernelcache.macho}
profile=$root/kernel/unixnu/profiles/os/ios-27.0.1-24A446/profile.toml
data=$root/kernel/prelink
work=${WORK:-$root/out/rustport-prelink}
kernel=${KERNEL:-}
reference=${REFERENCE:-}
xp=${XNUPRELINK:-$root/external/XnuPrelink}
[ -f "$xp/fileset_match.py" ] || xp=${ONEIOS_MAIN:-$root}/external/XnuPrelink
[ -x "$tool" ] || { echo "no tool at $tool: build it or set ONEIOS_TOOL" >&2; exit 2; }
[ -f "$cache" ] || { echo "no kernelcache at $cache: run scripts/image/kernel-inputs.sh or set KERNELCACHE" >&2; exit 2; }
rm -rf "$work"; mkdir -p "$work/rs"
d=$work/rs
TIMEFORMAT=%R
total=0
: > "$work/found.txt"
# norm FILE: the file as it is hashed
norm() {
  case $1 in
  *.json|*.out) sed -e "s#$d#DIR#g" -e "s#${d#$root/}#DIR#g" -e 's#"oneios": "[0-9a-f]*"#"oneios": "X"#' -e 's#"\(path\|input\)": "/[^"]*/\([^/"]*\)"#"\1": "\2"#' "$1" ;;
  *) cat "$1" ;;
  esac
}
# step NAME RUST-STEP OUTPUT-FILES ARGS...: `@` in ARGS is the work folder
step() {
  local name=$1 rust=$2 outputs=$3; shift 3
  local args=() a took f
  for a in "$@"; do args+=("${a//@/$d}"); done
  took=$( { time "$tool" prelink "$rust" "${args[@]}" > "$work/$name.out" 2> "$work/$name.err"; echo $? > "$work/$name.status"; } 2>&1 )
  total=$(awk "BEGIN{print $total + $took}")
  printf '%s %s %s\n' "$(norm "$work/$name.out" | sha256sum | cut -c1-64)" "$(cat "$work/$name.status")" "stdout:$name" >> "$work/found.txt"
  for f in $outputs; do
    [ -f "$d/$f" ] || { echo "MISSING $name: $f ($(tail -c 200 "$work/$name.err"))"; continue; }
    printf '%s %s %s\n' "$(norm "$d/$f" | sha256sum | cut -c1-64)" "$(norm "$d/$f" | wc -c)" "$f" >> "$work/found.txt"
  done
  printf 'ran    %-20s %6s s  %s\n' "$name" "$took" "$(tail -1 "$work/$name.out" | sed "s#$d#DIR#g" | head -c 110)"
}

step roots roots "select.json" "$cache" @/select.json "$profile"
step roots-no-profile roots "select-no-profile.json" "$cache" @/select-no-profile.json
step hosted hosted "kernelcache.hosted.macho hosted.json" "$cache" @/kernelcache.hosted.macho @/hosted.json --closure @/select.json
step hosted-every-kext hosted "every.hosted.macho every.hosted.json" "$cache" @/every.hosted.macho @/every.hosted.json
golden=$here/golden/kernelcache.txt
if [ -n "$kernel" ]; then
  [ -f "$kernel" ] || { echo "no kernel at $kernel" >&2; exit 2; }
  golden=$here/golden/$(sha256sum < "$kernel" | cut -c1-16).txt
  step native-input native-input "native.macho native-input.json" "$kernel" @/native.macho @/native-input.json
  ref=(); features=()
  if [ -n "$reference" ]; then
    step native-input-ref native-input "reference.macho reference-input.json" "$reference" @/reference.macho @/reference-input.json
    ref=(--reference-kernel @/reference.macho); features=(--features @/reference.macho)
  fi
  step merge-reviewed merge-reviewed "reviewed.merged.json" --reviewed "$data/reviewed-24A446-v54.json" --native @/native.macho \
    --out @/reviewed.merged.json $(ls "$data"/lead/reviewed-*-24A446-*.json "$data"/user/reviewed-*-24A446-*.json 2>/dev/null)
  step match match "match.map.json" @/kernelcache.hosted.macho @/native.macho --out @/match.map.json "${ref[@]}" --xnuprelink "$xp"
  common=(--xnuprelink "$xp" --kernelcache @/kernelcache.hosted.macho --native @/native.macho --map @/match.map.json)
  step align-native align "aligned-native.json" "${common[@]}" --reference @/native.macho "${features[@]}" --also @/reviewed.merged.json --out @/aligned-native.json
  aligned=(--aligned @/aligned-native.json); weak=(--weak @/aligned-native.json)
  if [ -n "$reference" ]; then
    step align-reference align "aligned-reference.json" "${common[@]}" --reference @/reference.macho --position --also @/reviewed.merged.json --also @/aligned-native.json --out @/aligned-reference.json
    aligned+=(--aligned @/aligned-reference.json); weak+=(--weak @/aligned-reference.json)
  fi
  step kclasses kclasses "kclasses.json" "${common[@]}" --reviewed @/reviewed.merged.json --out @/kclasses.json
  step structors structors "structors.json" "${common[@]}" --reviewed @/reviewed.merged.json --classes @/kclasses.json --out @/structors.json
  step vtables vtables "vtables.json" "${common[@]}" --reviewed @/reviewed.merged.json --classes @/kclasses.json --also @/structors.json "${weak[@]}" --out @/vtables.json
  step emit emit "kernel.kc kernel.kc.prelink.json" --xnuprelink "$xp" --kernelcache @/kernelcache.hosted.macho --original-kernelcache "$cache" \
    --native @/native.macho --map @/match.map.json --roots @/select.json --reviewed @/reviewed.merged.json \
    --aligned @/kclasses.json --aligned @/vtables.json --aligned @/structors.json "${aligned[@]}" \
    --pairs "$data/pairs-24A446.json" --stubs "$data/stubs-24A446.json" --output @/kernel.kc --report @/kernel.kc.prelink.json
  step audit audit "kernel.kc.binds.json" --xnuprelink "$xp" --kernelcache "$cache" --collection @/kernel.kc \
    --pairs "$data/pairs-24A446.json" --stubs "$data/stubs-24A446.json" --reviewed @/reviewed.merged.json --report @/kernel.kc.binds.json
  step audit-draft-against audit "draft.json against.binds.json" --xnuprelink "$xp" --kernelcache "$cache" --collection @/kernel.kc \
    --reviewed @/reviewed.merged.json --against @/kernel.kc --draft @/draft.json --report @/against.binds.json
  mkdir -p "$d/work"; for f in select.json hosted.json kernel.kc.prelink.json match.map.json native-input.json kernel.kc; do ln -sf "../$f" "$d/work/$f"; done
  step record record "kernel.kc.build.json" --work @/work --kernel "$kernel" --kernelcache "$cache" --reviewed @/reviewed.merged.json \
    --xnuprelink "$xp" ${reference:+--reference @/reference.macho} --output @/kernel.kc.build.json --repo "$root"
  echo "kernel.kc: $(sha256sum < "$d/kernel.kc" | cut -c1-64)"
  python3 "$data/stubs-check.py" "$d/kernel.kc" 2>&1 | tail -1
fi
echo "total: $total s"
sort -k3 "$work/found.txt" > "$work/found.sorted.txt"
[ -n "${KEEP:-}" ] || find "$work" \( -name '*.macho' -o -name 'kernel.kc' \) -type f -delete
if [ -n "${UPDATE:-}" ]; then
  mkdir -p "$here/golden"; cp "$work/found.sorted.txt" "$golden"; echo "wrote $golden (this run's output, not a comparison)"; exit 0
fi
[ -f "$golden" ] || { echo "prelink compare: no golden for this kernel ($golden)"; exit 3; }
if diff <(sort -k3 "$golden") "$work/found.sorted.txt" > "$work/golden.diff"; then
  echo "prelink compare: $(wc -l < "$golden") hashes the same as the Python chain's ($(basename "$golden"))"
else
  echo "prelink compare: FAILED against $(basename "$golden")"; sed 's/^/  /' "$work/golden.diff" | head -40; exit 1
fi
