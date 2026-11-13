#!/bin/bash
# The board archive generators (`oneios-tool kernel gen NAME`) against the frozen output of the Python scripts they
# replaced: after a hosted kernel build, the files the generators wrote into its tree are compared with
# tests/kernelgen/golden/ (the Python scripts' files of the build on Linux, 2026-11-13, at commit ea06d46).
#
#   tools/oneios-tool/tests/kernelgen/compare-golden.sh WORK
#   UPDATE=1 tools/oneios-tool/tests/kernelgen/compare-golden.sh WORK      # record WORK's files as the goldens
#
#   WORK   UniXNU's work directory of a finished build: out/kernel-hosted-<name>/unixnu/oneios/<os profile>/oneios_hosted
#          (TREE_WAS=PATH: the tree's path as the logs have it, when WORK is a copy of a build's tree/build and logs)
#
# What is frozen, and how:
#   golden/build/...      the generated files that are this project's own text, whole, at their place below the
#                         tree's build/: the bindings (*.S), OriginalLegacyFramebufferABI.h, the probe classes, the
#                         crumbs header, hid's and net's report.json, the GPU's adreno/status.json
#   golden/staged.sha256  the staged copies of other projects' sources (and of Apple's IOBlockStorageDevice.cpp):
#                         SHA-256, size and name only
#   golden/stdout.txt     what the nine generators print (the archive steps' logs in build order), with the tree's
#                         and the checkout's paths taken out and without the lines of drivers/build.py
# Not frozen: objects and archives (they carry the tree's path and the compiler's version), the compilers' logs,
# objects.json and build-report.json (the SHA-256 of every source: they change with each source change).
#
# The goldens follow the sources: a change of a binding's inputs (a record, a header, a new call of an original
# function), of an edit list, of OneiOSSideKeys.inc or OneiOSTouchTime.inc, or of an external pin changes them.
# Then run with UPDATE=1 in the same commit and say why.
# Exit status: 0 all the same, 1 a difference, 2 usage.
set -uo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
[ $# -eq 1 ] && [ -d "$1/tree/build" ] || { sed -n '2,12p' "$0" >&2; exit 2; }
WORK=$(cd "$1" && pwd); BUILD=$WORK/tree/build; GOLDEN=$HERE/golden
TEXT="oneios-aes/IOAESAccelerator-bindings.S oneios-aes/IOAESAccelerator-originals.S
oneios-gpu/adreno/accelerator/IOAcceleratorES-bindings.S oneios-gpu/adreno/accelerator/IOAcceleratorES-originals.S oneios-gpu/adreno/status.json
native-legacy-display/OriginalLegacyFramebufferABI.h native-legacy-display/IOMobileFramebufferLegacy-bindings.S
native-legacy-display/IOMobileFramebufferLegacy-originals.S
oneios-hid/OneiOSHidLayoutCheck.cpp oneios-hid/IOHIDDevice-bindings.S oneios-hid/IOHIDDevice-originals.S oneios-hid/report.json
oneios-net/OneiOSNetCheck.cpp oneios-net/ionetworking-include/oneios-mbuf-crumbs.h oneios-net/IONetworking-bindings.S
oneios-net/IONetworking-originals.S oneios-net/report.json"
STAGED="oneios-aes/NativeAES.cpp oneios-keystore/NativeKeyStoreEvents.cpp oneios-audio/DroidVMAudio.cpp oneios-hid/NativeHidInput.cpp
oneios-hid/DroidVMVirtioInput.c oneios-net/DroidVMVirtioNet.c native-legacy-display/LegacyDisplayProbe.cpp
oneios-hosted-storage/IOBlockStorageDevice.cpp oneios-gpu/adreno/root/kits/droidvm/DroidVMGpu.cpp
oneios-gpu/adreno/root/scripts/build-droidvm-gpu.py"

sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
staged() { for f in $STAGED; do printf '%s  %s  %s\n' "$(sha "$BUILD/$f")" "$(wc -c < "$BUILD/$f" | tr -d ' ')" "$f"; done; }
# The generators' output: the logs of the archive steps that are ours, in build order (UniXNU's driver names a log
# after the step's program: archive-NN-build-oneios-<name>.log for a script, archive-NN-oneios-tool.log for the command).
said() {
  local log
  for log in "$WORK"/logs/archive-*-build-oneios-*.log "$WORK"/logs/archive-*-oneios-tool.log; do
    if [ -f "$log" ]; then echo "$log"; fi
  done | sort | while read -r log; do
    sed -e "s#${TREE_WAS:-$WORK/tree}#TREE#g" -e "s#$ROOT#REPO#g" "$log" | grep -v '^drivers: ' || true
  done
}

if [ "${UPDATE:-0}" = 1 ]; then
  rm -rf "$GOLDEN"; mkdir -p "$GOLDEN/build"
  for f in $TEXT; do mkdir -p "$GOLDEN/build/$(dirname "$f")"; cp "$BUILD/$f" "$GOLDEN/build/$f"; done
  staged > "$GOLDEN/staged.sha256"
  said > "$GOLDEN/stdout.txt"
  echo "compare-golden: recorded $(echo $TEXT | wc -w | tr -d ' ') files, $(echo $STAGED | wc -w | tr -d ' ') hashes and the generators' output of $WORK"
  exit 0
fi
fail=0; same=0
for f in $TEXT; do
  if cmp -s "$BUILD/$f" "$GOLDEN/build/$f"; then same=$((same + 1)); else echo "differs: $f"; fail=1; fi
done
hashes=$(staged | diff - "$GOLDEN/staged.sha256") || { echo "differs: the staged sources (staged.sha256)"; echo "$hashes" | sed 's/^/    /'; fail=1; }
output=$(said | diff - "$GOLDEN/stdout.txt") || { echo "differs: the generators' output (stdout.txt)"; echo "$output" | sed 's/^/    /' | head -20; fail=1; }
echo "compare-golden: $same of $(echo $TEXT | wc -w | tr -d ' ') generated files the same; the staged sources' hashes $([ -z "$hashes" ] && echo "the same ($(echo $STAGED | wc -w | tr -d ' '))" || echo differ); the generators' output $([ -z "$output" ] && echo "the same" || echo differs)"
exit $fail
