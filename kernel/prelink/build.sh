#!/bin/bash
# Build OneiOS's kernel collection: the hosted XNU prelinked with the kexts of the iPhone18,2 iOS 27.0.1 kernelcache.
#   scripts/bg.sh prelink kernel/prelink/build.sh
#   scripts/bg.sh prelink env PRELINK_IMPL=rust kernel/prelink/build.sh     (the Rust steps: the same kernel.kc)
# Inputs  out/kernel/current-hosted/xnu.macho (kernel/build-hosted.sh; KERNEL=FILE for another),
#         out/kernel-inputs/24A446/kernelcache.macho (scripts/image/kernel-inputs.sh),
#         optionally REFERENCE=FILE, the RELEASE kernel of the same source with symbols
#         (XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh leaves it in the tree's build/xnu/fileset-reference/).
# Output  out/kernel/24A446/kernel.kc, kernel.kc.hashes (1 MiB chunk hashes), kernel.kc.build.json (the record),
#         kernel.kc.prelink.json (the emitter's report) and kernel.kc.binds.json (audit.py: every kext import's binding).
#         The build fails when audit.py finds an import off the known-good pairs (pairs-24A446.json) or on the stub
#         without a reason in stubs-24A446.json. Made from Apple's kexts: never committed, never released. Steps and files: README.md here.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
HERE=$ROOT/kernel/prelink
BUILD=24A446
KERNEL=${KERNEL:-$ROOT/out/kernel/current-hosted/xnu.macho}
CACHE=${KERNELCACHE:-$ROOT/out/kernel-inputs/$BUILD/kernelcache.macho}
PROFILE=$ROOT/kernel/unixnu/profiles/os/ios-27.0.1-$BUILD/profile.toml
REVIEWED=$HERE/reviewed-$BUILD-v54.json
# The known-good pairs and the stub list (audit.py checks the collection against both; README "Known-good pairs").
PAIRS=$HERE/pairs-$BUILD.json
STUBS=$HERE/stubs-$BUILD.json
# XnuPrelink: external/XnuPrelink when there is one, else the kernel base's submodule at the board's pin
# (external/Droid-VM-UniXNU/kits/XnuPrelink, checked out by kernel/build-hosted.sh).
XNUPRELINK=${XNUPRELINK:-$ROOT/external/XnuPrelink}
[ -f "$XNUPRELINK/fileset_emit.py" ] || [ ! -f "$ROOT/external/Droid-VM-UniXNU/kits/XnuPrelink/fileset_emit.py" ] \
  || XNUPRELINK=$ROOT/external/Droid-VM-UniXNU/kits/XnuPrelink
# PRELINK_IMPL=python|rust: which implementation runs the steps (default python: the scripts here and XnuPrelink's
# fileset_match.py; rust: `oneios-tool prelink STEP`, the same files byte for byte and about ten times faster,
# tools/oneios-tool/README.md "prelink"). ONEIOS_TOOL names the binary (default
# tools/oneios-tool/target/release/oneios-tool: cargo build --release --manifest-path tools/oneios-tool/Cargo.toml).
PRELINK_IMPL=${PRELINK_IMPL:-python}
ONEIOS_TOOL=${ONEIOS_TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
# llvm-nm and llvm-mc: Homebrew's LLVM is keg-only on a Mac (not on PATH).
for d in /opt/homebrew/opt/llvm/bin /usr/local/opt/llvm/bin; do if [ -d "$d" ]; then PATH=$d:$PATH; fi; done
export PATH
case $PRELINK_IMPL in
python)
  for t in llvm-nm llvm-mc; do command -v $t >/dev/null || { echo "prelink: missing $t (LLVM; macOS: brew install llvm)" >&2; exit 1; }; done
  python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' || { echo "prelink: python3 3.11 or newer needed (tomllib)" >&2; exit 1; } ;;
rust)
  [ -x "$ONEIOS_TOOL" ] || { echo "prelink: PRELINK_IMPL=rust: no $ONEIOS_TOOL (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml, or set ONEIOS_TOOL)" >&2; exit 1; }
  [ -z "${X18:-}" ] || command -v llvm-mc >/dev/null || { echo "prelink: X18=1 needs llvm-mc (LLVM)" >&2; exit 1; } ;;
*) echo "prelink: PRELINK_IMPL is python or rust, not $PRELINK_IMPL" >&2; exit 1 ;;
esac
# step NAME ARGS...: one step of the build, by the chosen implementation. The Python step is NAME.py here
# (fileset_match.py of XnuPrelink for match); the Rust step takes the same arguments (match also the XnuPrelink
# checkout, for its reviewed fixtures and the matcher signature; record also this checkout).
step() {
  local name=$1; shift
  if [ "$PRELINK_IMPL" = rust ]; then
    case $name in
    match) "$ONEIOS_TOOL" prelink match "$@" --xnuprelink "$XNUPRELINK" ;;
    record) "$ONEIOS_TOOL" prelink record "$@" --repo "$ROOT" ;;
    *) "$ONEIOS_TOOL" prelink "$name" "$@" ;;
    esac
  elif [ "$name" = match ]; then python3 "$XNUPRELINK/fileset_match.py" "$@"
  else python3 "$HERE/$name.py" "$@"
  fi
}
. "$ROOT/kernel/lib/host.sh"
OUT=${OUT:-$ROOT/out/kernel/$BUILD}
WORK=$OUT/work
REFERENCE=${REFERENCE:-}
for f in "$KERNEL" "$CACHE" "$XNUPRELINK/fileset_emit.py"; do
  [ -f "$f" ] || { echo "prelink: $f is missing" >&2; exit 1; }
done
sha() { sha256 "$1"; }
if [ "$PRELINK_IMPL" = rust ]; then want=$("$ONEIOS_TOOL" prelink pin "$PROFILE")
else want=$(python3 -I -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1],"rb"))["kernel"]["prelink"]["kernelcache_sha256"])' "$PROFILE"); fi
[ "$(sha "$CACHE")" = "$want" ] || { echo "prelink: $CACHE is not the kernelcache the profile pins ($want)" >&2; exit 1; }
mkdir -p "$WORK"

# 1. The kernel as the emitter's native input (its one weak bind resolved).
cp "$KERNEL" "$WORK/xnu.macho"
step native-input "$WORK/xnu.macho" "$WORK/native.macho" "$WORK/native-input.json"
# The reference the same way: Xcode's ld leaves weak-lookup binds in it too (7 on the Mac, 2026-11-12), and the matcher
# reads its fixups. A reference without binds is copied unchanged.
if [ -n "$REFERENCE" ]; then
  step native-input "$REFERENCE" "$WORK/reference.macho" "$WORK/reference-input.json"
  REFERENCE=$WORK/reference.macho
fi
# 1b. The lead's and fable-user's reviewed records (lead/, user/) merged into ours for this build: ours win, a
# disagreement is printed and recorded (merge-reviewed.py).
OWN_REVIEWED=$REVIEWED
REVIEWED=$WORK/reviewed.merged.json
step merge-reviewed --reviewed "$OWN_REVIEWED" --native "$WORK/native.macho" --out "$REVIEWED" \
  $(ls "$HERE"/lead/reviewed-*-"$BUILD"-*.json "$HERE"/user/reviewed-*-"$BUILD"-*.json 2>/dev/null)
# 2. Which kexts.
step roots "$CACHE" "$WORK/select.json" "$PROFILE"
# 3. The kexts for the hosted kernel: TPIDR_EL1 -> TPIDR_EL0, and what else a process cannot execute.
rm -f "$WORK/kernelcache.hosted.macho"
step hosted "$CACHE" "$WORK/kernelcache.hosted.macho" "$WORK/hosted.json" --closure "$WORK/select.json" ${X18:+--x18}
INPUT=$CACHE; [ -f "$WORK/kernelcache.hosted.macho" ] && INPUT=$WORK/kernelcache.hosted.macho
# 4. The pairing of the original kernel with ours (reused while both inputs, the reference and XnuPrelink's matcher are
# the same: a pairing from another XnuPrelink is refused by its emitter).
MATCHER=$XNUPRELINK/fileset_match.py; [ "$PRELINK_IMPL" = rust ] && MATCHER=$ONEIOS_TOOL
KEY="$(sha "$INPUT") $(sha "$WORK/native.macho") ${REFERENCE:+$(sha "$REFERENCE")} $(sha "$MATCHER") $(sha "$XNUPRELINK/fileset_match.py")"
if [ ! -f "$WORK/match.map.json" ] || [ "$(cat "$WORK/match.key" 2>/dev/null)" != "$KEY" ]; then
  step match "$INPUT" "$WORK/native.macho" --out "$WORK/match.map.json" \
    ${REFERENCE:+--reference-kernel "$REFERENCE"}
  echo "$KEY" > "$WORK/match.key"
fi
# 5. What the matcher left unpaired, by link order: the kernel's own order (source order inside a file), then the
#    reference's. Inferred pairs, each listed in the report with the error rate measured on known pairs. Then the
#    constructors and base destructors of classes kexts subclass, by the vtable they store, and the virtual methods
#    of every vtable paired by name, slot by slot; before both, the kernel's class objects by the names their static
#    initializers register (metaclass, metaClass and superClass pointers, vtable).
ALIGNED=()
step align --xnuprelink "$XNUPRELINK" --kernelcache "$INPUT" --native "$WORK/native.macho" \
  --map "$WORK/match.map.json" --reference "$WORK/native.macho" ${REFERENCE:+--features "$REFERENCE"} \
  --also "$REVIEWED" --out "$WORK/aligned-native.json"
ALIGNED+=(--aligned "$WORK/aligned-native.json")
if [ -n "$REFERENCE" ]; then
  step align --xnuprelink "$XNUPRELINK" --kernelcache "$INPUT" --native "$WORK/native.macho" \
    --map "$WORK/match.map.json" --reference "$REFERENCE" --position --also "$REVIEWED" --also "$WORK/aligned-native.json" --out "$WORK/aligned-reference.json"
  ALIGNED+=(--aligned "$WORK/aligned-reference.json")
fi
step kclasses --xnuprelink "$XNUPRELINK" --kernelcache "$INPUT" --native "$WORK/native.macho" \
  --map "$WORK/match.map.json" --reviewed "$REVIEWED" --out "$WORK/kclasses.json"
step structors --xnuprelink "$XNUPRELINK" --kernelcache "$INPUT" --native "$WORK/native.macho" \
  --map "$WORK/match.map.json" --reviewed "$REVIEWED" --classes "$WORK/kclasses.json" --out "$WORK/structors.json"
WEAK=(); for f in "$WORK/aligned-native.json" "$WORK/aligned-reference.json"; do [ -f "$f" ] && WEAK+=(--weak "$f"); done
[ -n "$REFERENCE" ] || rm -f "$WORK/aligned-reference.json"
step vtables --xnuprelink "$XNUPRELINK" --kernelcache "$INPUT" --native "$WORK/native.macho" \
  --map "$WORK/match.map.json" --reviewed "$REVIEWED" --classes "$WORK/kclasses.json" --also "$WORK/structors.json" "${WEAK[@]}" \
  --out "$WORK/vtables.json"
# emit.py takes the first pair it is given for a function: slots and structors before the order-inferred ones.
ALIGNED=(--aligned "$WORK/kclasses.json" --aligned "$WORK/vtables.json" --aligned "$WORK/structors.json" "${ALIGNED[@]}")
# 6. The collection.
step emit --xnuprelink "$XNUPRELINK" --kernelcache "$INPUT" --original-kernelcache "$CACHE" \
  --native "$WORK/native.macho" --map "$WORK/match.map.json" --roots "$WORK/select.json" --reviewed "$REVIEWED" \
  "${ALIGNED[@]}" --pairs "$PAIRS" --stubs "$STUBS" --output "$WORK/kernel.kc" --report "$WORK/kernel.kc.prelink.json"
# 6b. Every kext import of the collection: bound as the known-good list says, or on the stub list with its reason.
#     A failure stops the build before kernel.kc is put in place (work/kernel.kc.binds.json lists every import).
step audit --xnuprelink "$XNUPRELINK" --kernelcache "$CACHE" --collection "$WORK/kernel.kc" \
  --pairs "$PAIRS" --stubs "$STUBS" --reviewed "$REVIEWED" --report "$WORK/kernel.kc.binds.json"
# 7. The record, then the outputs in place.
step record --work "$WORK" --kernel "$KERNEL" --kernelcache "$CACHE" --reviewed "$REVIEWED" \
  --xnuprelink "$XNUPRELINK" ${REFERENCE:+--reference "$REFERENCE"} --output "$WORK/kernel.kc.build.json"
cp "$WORK/kernel.kc.prelink.json" "$OUT/kernel.kc.prelink.json"
cp "$WORK/kernel.kc.build.json" "$OUT/kernel.kc.build.json"
cp "$WORK/kernel.kc.binds.json" "$OUT/kernel.kc.binds.json"
cp "$WORK/kernel.kc" "$OUT/kernel.kc.partial" && mv "$OUT/kernel.kc.partial" "$OUT/kernel.kc"
# 8. Its chunk hash list (docs/image-format.md: SHA-256 of each 1 MiB chunk), as the image set carries it.
if [ "$PRELINK_IMPL" = rust ]; then "$ONEIOS_TOOL" image hashes "$OUT/kernel.kc" "$OUT/kernel.kc.hashes" >/dev/null
else python3 -I "$ROOT/scripts/image/imagetool.py" hashes "$OUT/kernel.kc" "$OUT/kernel.kc.hashes" >/dev/null; fi
echo "prelink: $OUT/kernel.kc ($(file_size "$OUT/kernel.kc") bytes, sha256 $(sha "$OUT/kernel.kc" | cut -c1-16)...)"
