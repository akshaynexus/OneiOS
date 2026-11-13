#!/bin/bash
# Saves the environment UniXNU's driver gives a board archive step, for compare-python.sh: waits for an archive
# step of the kernel build in WORK (a `build-oneios-*.py` script or `oneios-tool kernel gen`) and copies its
# /proc/PID/environ to FILE. Start it beside the build; it ends when it has the file. Linux only (/proc).
#
#   tools/oneios-tool/tests/kernelgen/save-env.sh WORK FILE
#
#   WORK   UniXNU's work directory of the build (out/kernel-hosted-<name>/unixnu/oneios/<os profile>/oneios_hosted)
set -uo pipefail
[ $# -eq 2 ] || { sed -n '2,9p' "$0" >&2; exit 2; }
[ -d /proc/self ] || { echo "save-env: no /proc here (Linux only)" >&2; exit 2; }
WORK=$1 FILE=$2
while :; do
  for p in /proc/[0-9]*; do
    [ "$(readlink "$p/cwd" 2>/dev/null)" = "$WORK/tree" ] || continue
    c=$(tr '\0' ' ' < "$p/cmdline" 2>/dev/null) || continue
    case "$c" in
      python3\ */scripts/build-oneios-*|*oneios-tool\ kernel\ gen\ *)
        if cp "$p/environ" "$FILE" 2>/dev/null && [ -s "$FILE" ]; then echo "save-env: $FILE ($c)"; exit 0; fi ;;
    esac
  done
  sleep 0.3
done
