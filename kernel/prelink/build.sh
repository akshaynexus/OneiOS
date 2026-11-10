#!/bin/bash
# Build OneiOS's kernel collection: the hosted XNU prelinked with the kexts of the iPhone18,2 iOS 27.0.1 kernelcache.
#   scripts/bg.sh prelink kernel/prelink/build.sh
# Inputs  out/kernel/current-hosted/xnu.macho (kernel/build-hosted.sh; KERNEL=FILE for another),
#         out/kernel-inputs/24A446/kernelcache.macho (scripts/image/kernel-inputs.sh),
#         optionally REFERENCE=FILE, the RELEASE kernel of the same source with symbols
#         (XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh leaves it in the tree's build/xnu/fileset-reference/).
# Output  out/kernel/24A446/kernel.kc, kernel.kc.build.json (the record) and kernel.kc.prelink.json (the emitter's
#         report). Made from Apple's kexts: never committed, never released. Steps and files: README.md here.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
HERE=$ROOT/kernel/prelink
BUILD=24A446
KERNEL=${KERNEL:-$ROOT/out/kernel/current-hosted/xnu.macho}
CACHE=${KERNELCACHE:-$ROOT/out/kernel-inputs/$BUILD/kernelcache.macho}
PROFILE=$ROOT/kernel/unixnu/profiles/os/ios-27.0.1-$BUILD/profile.toml
REVIEWED=$HERE/reviewed-$BUILD-v54.json
XNUPRELINK=${XNUPRELINK:-$ROOT/external/XnuPrelink}
OUT=${OUT:-$ROOT/out/kernel/$BUILD}
WORK=$OUT/work
REFERENCE=${REFERENCE:-}
for f in "$KERNEL" "$CACHE" "$XNUPRELINK/fileset_emit.py"; do
  [ -f "$f" ] || { echo "prelink: $f is missing" >&2; exit 1; }
done
sha() { sha256sum "$1" | cut -d' ' -f1; }
want=$(python3 -I -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1],"rb"))["kernel"]["prelink"]["kernelcache_sha256"])' "$PROFILE")
[ "$(sha "$CACHE")" = "$want" ] || { echo "prelink: $CACHE is not the kernelcache the profile pins ($want)" >&2; exit 1; }
mkdir -p "$WORK"

# 1. The kernel as the emitter's native input (its one weak bind resolved).
cp "$KERNEL" "$WORK/xnu.macho"
python3 "$HERE/native-input.py" "$WORK/xnu.macho" "$WORK/native.macho" "$WORK/native-input.json"
# 2. Which kexts.
python3 "$HERE/roots.py" "$CACHE" "$WORK/select.json" "$PROFILE"
# 3. The kexts for the hosted kernel: TPIDR_EL1 -> TPIDR_EL0, and what else a process cannot execute.
rm -f "$WORK/kernelcache.hosted.macho"
python3 "$HERE/hosted.py" "$CACHE" "$WORK/kernelcache.hosted.macho" "$WORK/hosted.json" --closure "$WORK/select.json" ${X18:+--x18}
INPUT=$CACHE; [ -f "$WORK/kernelcache.hosted.macho" ] && INPUT=$WORK/kernelcache.hosted.macho
# 4. The pairing of the original kernel with ours (reused while both inputs and the reference are the same).
KEY="$(sha "$INPUT") $(sha "$WORK/native.macho") ${REFERENCE:+$(sha "$REFERENCE")}"
if [ ! -f "$WORK/match.map.json" ] || [ "$(cat "$WORK/match.key" 2>/dev/null)" != "$KEY" ]; then
  python3 "$XNUPRELINK/fileset_match.py" "$INPUT" "$WORK/native.macho" --out "$WORK/match.map.json" \
    ${REFERENCE:+--reference-kernel "$REFERENCE"}
  echo "$KEY" > "$WORK/match.key"
fi
# 5. What the matcher left unpaired, by link order: the kernel's own order (source order inside a file), then the
#    reference's. Inferred pairs, each listed in the report with the error rate measured on known pairs.
ALIGNED=()
python3 "$HERE/align.py" --xnuprelink "$XNUPRELINK" --kernelcache "$INPUT" --native "$WORK/native.macho" \
  --map "$WORK/match.map.json" --reference "$WORK/native.macho" ${REFERENCE:+--features "$REFERENCE"} \
  --out "$WORK/aligned-native.json"
ALIGNED+=(--aligned "$WORK/aligned-native.json")
if [ -n "$REFERENCE" ]; then
  python3 "$HERE/align.py" --xnuprelink "$XNUPRELINK" --kernelcache "$INPUT" --native "$WORK/native.macho" \
    --map "$WORK/match.map.json" --reference "$REFERENCE" --also "$WORK/aligned-native.json" --out "$WORK/aligned-reference.json"
  ALIGNED+=(--aligned "$WORK/aligned-reference.json")
fi
# 6. The collection.
python3 "$HERE/emit.py" --xnuprelink "$XNUPRELINK" --kernelcache "$INPUT" --original-kernelcache "$CACHE" \
  --native "$WORK/native.macho" --map "$WORK/match.map.json" --roots "$WORK/select.json" --reviewed "$REVIEWED" \
  "${ALIGNED[@]}" --output "$WORK/kernel.kc" --report "$WORK/kernel.kc.prelink.json"
# 7. The record, then the outputs in place.
python3 "$HERE/record.py" --work "$WORK" --kernel "$KERNEL" --kernelcache "$CACHE" --reviewed "$REVIEWED" \
  --xnuprelink "$XNUPRELINK" ${REFERENCE:+--reference "$REFERENCE"} --output "$WORK/kernel.kc.build.json"
cp "$WORK/kernel.kc.prelink.json" "$OUT/kernel.kc.prelink.json"
cp "$WORK/kernel.kc.build.json" "$OUT/kernel.kc.build.json"
cp "$WORK/kernel.kc" "$OUT/kernel.kc.partial" && mv "$OUT/kernel.kc.partial" "$OUT/kernel.kc"
echo "prelink: $OUT/kernel.kc ($(stat -c %s "$OUT/kernel.kc") bytes, sha256 $(sha "$OUT/kernel.kc" | cut -c1-16)...)"
