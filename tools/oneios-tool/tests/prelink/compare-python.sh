#!/bin/bash
# tools/oneios-tool/tests/prelink/compare-python.sh   every step of kernel/prelink/build.sh, Rust against Python,
# on the real kernelcache and a hosted kernel: each step is run by both with the same arguments into its own
# folder; stdout, the exit status and every file written must be the same, byte for byte (cmp). Each side's
# later steps read its own earlier outputs, so the last comparison is the whole chain's kernel.kc.
#
#   roots, hosted          the kernelcache alone (also without the profile, and without --closure)
#   native-input           the kernel, and the reference kernel when REFERENCE is set
#   merge-reviewed, match, align (native order; reference order with REFERENCE), kclasses, structors, vtables,
#   emit, audit, record    as build.sh runs them
#
# Needs: KERNELCACHE (default out/kernel-inputs/24A446/kernelcache.macho; scripts/image/kernel-inputs.sh),
# KERNEL (the hosted xnu.macho; without it only roots and hosted are compared), REFERENCE (optional: the RELEASE
# reference kernel), XNUPRELINK (default external/XnuPrelink of this checkout or of ONEIOS_MAIN), the tool built
# (or ONEIOS_TOOL=path), python3 3.11+, llvm-nm (the Python merge-reviewed). Work files: WORK (default
# out/rustport-prelink/; Apple's bytes: never commit). KEEP=1 keeps the big files. X18=1 adds hosted --x18.
# The Python steps are the ones of PYTHON_PRELINK (default kernel/prelink of this checkout); the goldens of a
# finished run are written to WORK/golden.txt (file names, sizes and SHA-256 only).
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
tool=${ONEIOS_TOOL:-$root/tools/oneios-tool/target/release/oneios-tool}
cache=${KERNELCACHE:-$root/out/kernel-inputs/24A446/kernelcache.macho}
profile=$root/kernel/unixnu/profiles/os/ios-27.0.1-24A446/profile.toml
py=${PYTHON_PRELINK:-$root/kernel/prelink}
data=$root/kernel/prelink
work=${WORK:-$root/out/rustport-prelink}
kernel=${KERNEL:-}
reference=${REFERENCE:-}
xp=${XNUPRELINK:-$root/external/XnuPrelink}
[ -f "$xp/fileset_emit.py" ] || xp=${ONEIOS_MAIN:-$root}/external/XnuPrelink
[ -x "$tool" ] || { echo "no tool at $tool: build it or set ONEIOS_TOOL" >&2; exit 2; }
[ -f "$cache" ] || { echo "no kernelcache at $cache: run scripts/image/kernel-inputs.sh or set KERNELCACHE" >&2; exit 2; }
rm -rf "$work"; mkdir -p "$work"
status=0
TIMEFORMAT=%R
export PYTHONDONTWRITEBYTECODE=1
total_py=0; total_rs=0
# step NAME PYTHON-SCRIPT RUST-STEP ARGS...: `@` in ARGS is the side's own folder; a Rust-only argument is
# written `RUST:arg`.
step() {
  local name=$1 script=$2 rust=$3; shift 3
  local side args a took_py took_rs
  for side in py rs; do
    mkdir -p "$work/$side"
    args=()
    for a in "$@"; do
      case $a in RUST:*) [ $side = rs ] && args+=("${a#RUST:}"); continue;; esac
      args+=("${a//@/$work/$side}")
    done
    if [ $side = py ]; then
      took_py=$( { time python3 "$script" "${args[@]}" > "$work/$name.py.out" 2> "$work/$name.py.err"; echo $? > "$work/$name.py.status"; } 2>&1 )
    else
      took_rs=$( { time "$tool" prelink "$rust" "${args[@]}" > "$work/$name.rs.out" 2> "$work/$name.rs.err"; echo $? > "$work/$name.rs.status"; } 2>&1 )
    fi
  done
  total_py=$(awk "BEGIN{print $total_py + $took_py}"); total_rs=$(awk "BEGIN{print $total_rs + $took_rs}")
  # the folder names differ in the messages and in the records' paths only
  sed -i "s#$work/py#DIR#g" "$work/$name.py.out"; sed -i "s#$work/rs#DIR#g" "$work/$name.rs.out"
  local bad=""
  cmp -s "$work/$name.py.out" "$work/$name.rs.out" || bad="$bad stdout"
  cmp -s "$work/$name.py.status" "$work/$name.rs.status" || bad="$bad status"
  [ "$(cat "$work/$name.py.status")" = "${EXPECT:-0}" ] || bad="$bad python-status-$(cat "$work/$name.py.status")($(tail -c 200 "$work/$name.py.err"))"
  local f files=0
  for f in $OUTPUTS; do
    files=$((files + 1))
    if [ -n "${PATHS:-}" ]; then      # a record that names its own folder: compare with the folder's name taken out
      cmp -s <(sed -e "s#$work/py#DIR#g" -e "s#${work#$root/}/py#DIR#g" "$work/py/$f") <(sed -e "s#$work/rs#DIR#g" -e "s#${work#$root/}/rs#DIR#g" "$work/rs/$f") || bad="$bad $f"
    else
      cmp -s "$work/py/$f" "$work/rs/$f" || bad="$bad $f"
    fi
  done
  if [ -z "$bad" ]; then
    printf 'same   %-18s %d files  python %7s s  rust %6s s  %s\n' "$name" "$files" "$took_py" "$took_rs" "$(tail -1 "$work/$name.py.out" | head -c 100)"
  else
    printf 'DIFFER %-18s%s\n' "$name" "$bad"; status=1
  fi
}

OUTPUTS="select.json" step roots "$py/roots.py" roots "$cache" @/select.json "$profile"
OUTPUTS="select-no-profile.json" step roots-no-profile "$py/roots.py" roots "$cache" @/select-no-profile.json
OUTPUTS="kernelcache.hosted.macho hosted.json" step hosted "$py/hosted.py" hosted "$cache" @/kernelcache.hosted.macho @/hosted.json --closure @/select.json
OUTPUTS="every.hosted.macho every.hosted.json" step hosted-every-kext "$py/hosted.py" hosted "$cache" @/every.hosted.macho @/every.hosted.json
[ -z "${X18:-}" ] || OUTPUTS="x18.hosted.macho x18.hosted.json" step hosted-x18 "$py/hosted.py" hosted "$cache" @/x18.hosted.macho @/x18.hosted.json --closure @/select.json --x18
if [ -n "$kernel" ]; then
  [ -f "$kernel" ] || { echo "no kernel at $kernel" >&2; exit 2; }
  PATHS=1 OUTPUTS="native.macho native-input.json" step native-input "$py/native-input.py" native-input "$kernel" @/native.macho @/native-input.json
  ref=(); features=()
  if [ -n "$reference" ]; then
    PATHS=1 OUTPUTS="reference.macho reference-input.json" step native-input-ref "$py/native-input.py" native-input "$reference" @/reference.macho @/reference-input.json
    ref=(--reference-kernel @/reference.macho); features=(--features @/reference.macho)
  fi
  OUTPUTS="reviewed.merged.json" step merge-reviewed "$py/merge-reviewed.py" merge-reviewed --reviewed "$data/reviewed-24A446-v54.json" --native @/native.macho \
    --out @/reviewed.merged.json $(ls "$data"/lead/reviewed-*-24A446-*.json "$data"/user/reviewed-*-24A446-*.json 2>/dev/null)
  OUTPUTS="match.map.json" step match "$xp/fileset_match.py" match @/kernelcache.hosted.macho @/native.macho --out @/match.map.json "${ref[@]}" RUST:--xnuprelink "RUST:$xp"
  common=(--xnuprelink "$xp" --kernelcache @/kernelcache.hosted.macho --native @/native.macho --map @/match.map.json)
  OUTPUTS="aligned-native.json" step align-native "$py/align.py" align "${common[@]}" --reference @/native.macho "${features[@]}" --also @/reviewed.merged.json --out @/aligned-native.json
  aligned=(--aligned @/aligned-native.json); weak=(--weak @/aligned-native.json)
  if [ -n "$reference" ]; then
    OUTPUTS="aligned-reference.json" step align-reference "$py/align.py" align "${common[@]}" --reference @/reference.macho --position --also @/reviewed.merged.json --also @/aligned-native.json --out @/aligned-reference.json
    aligned+=(--aligned @/aligned-reference.json); weak+=(--weak @/aligned-reference.json)
  fi
  OUTPUTS="kclasses.json" step kclasses "$py/kclasses.py" kclasses "${common[@]}" --reviewed @/reviewed.merged.json --out @/kclasses.json
  OUTPUTS="structors.json" step structors "$py/structors.py" structors "${common[@]}" --reviewed @/reviewed.merged.json --classes @/kclasses.json --out @/structors.json
  OUTPUTS="vtables.json" step vtables "$py/vtables.py" vtables "${common[@]}" --reviewed @/reviewed.merged.json --classes @/kclasses.json --also @/structors.json "${weak[@]}" --out @/vtables.json
  PATHS=1 OUTPUTS="kernel.kc kernel.kc.prelink.json" step emit "$py/emit.py" emit --xnuprelink "$xp" --kernelcache @/kernelcache.hosted.macho --original-kernelcache "$cache" \
    --native @/native.macho --map @/match.map.json --roots @/select.json --reviewed @/reviewed.merged.json \
    --aligned @/kclasses.json --aligned @/vtables.json --aligned @/structors.json "${aligned[@]}" \
    --pairs "$data/pairs-24A446.json" --stubs "$data/stubs-24A446.json" --output @/kernel.kc --report @/kernel.kc.prelink.json
  OUTPUTS="kernel.kc.binds.json" step audit "$py/audit.py" audit --xnuprelink "$xp" --kernelcache "$cache" --collection @/kernel.kc \
    --pairs "$data/pairs-24A446.json" --stubs "$data/stubs-24A446.json" --reviewed @/reviewed.merged.json --report @/kernel.kc.binds.json
  OUTPUTS="draft.json against.binds.json" step audit-draft-against "$py/audit.py" audit --xnuprelink "$xp" --kernelcache "$cache" --collection @/kernel.kc \
    --reviewed @/reviewed.merged.json --against @/kernel.kc --draft @/draft.json --report @/against.binds.json
  for side in py rs; do mkdir -p "$work/$side/work"; for f in select.json hosted.json kernel.kc.prelink.json match.map.json native-input.json kernel.kc; do ln -sf "../$f" "$work/$side/work/$f"; done; done
  PATHS=1 OUTPUTS="kernel.kc.build.json" step record "$py/record.py" record --work @/work --kernel "$kernel" --kernelcache "$cache" --reviewed @/reviewed.merged.json \
    --xnuprelink "$xp" ${reference:+--reference @/reference.macho} --output @/kernel.kc.build.json RUST:--repo "RUST:$root"
  echo "kernel.kc: python $(sha256sum < "$work/py/kernel.kc" | cut -c1-64)  rust $(sha256sum < "$work/rs/kernel.kc" | cut -c1-64)"
  python3 "$py/stubs-check.py" "$work/rs/kernel.kc" 2>&1 | tail -1
fi
echo "total: python $total_py s, rust $total_rs s"
# The goldens: every output's name, size and SHA-256 (no Apple bytes), with the folder names taken out of the
# records that carry them.
( cd "$work/py" && for f in $(find . -maxdepth 1 -type f | sort); do
    printf '%s %s %s\n' "$(sed -e "s#$work/py#DIR#g" -e "s#${work#$root/}/py#DIR#g" "$f" | sha256sum | cut -c1-64)" "$(stat -c %s "$f")" "${f#./}"; done ) > "$work/golden.txt"
[ -n "${KEEP:-}" ] || find "$work" \( -name '*.macho' -o -name 'kernel.kc' \) -type f -delete
[ $status = 0 ] && echo "prelink compare: every step the same as Python" || echo "prelink compare: FAILED"
exit $status
