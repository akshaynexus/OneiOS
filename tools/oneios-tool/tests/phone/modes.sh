#!/bin/bash
# The stand-in phone's suites (tests/phone, tests/app, tests/cycle) in each /proc mode this host has:
#   FAKE_PROC=host      the phone's commands read this machine's /proc (Linux only)
#   FAKE_PROC=stand-in  they read the stand-in's own /proc (fake-rewrite.sh, fake-session.sh): macOS's only mode,
#                       and on Linux the same path forced, so that what the Mac runs is run here too
#
#   tools/oneios-tool/tests/phone/modes.sh [SUITE...]   SUITE: phone, app, cycle (default: all three)
#
# One line per suite and mode with its counts, each suite's output in out/rustport-modes/MODE-SUITE.txt, then
# `ALL OK` (exit 0) or `FAILED: N` (the suites that failed). After each suite no stand-in session may be running.
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
O=$ROOT/out/rustport-modes; mkdir -p "$O"
suites=("$@"); [ ${#suites[@]} -gt 0 ] || suites=(phone app cycle)
modes=(stand-in); [ ! -r /proc/self/cmdline ] || modes=(host stand-in)
failed=0
for m in "${modes[@]}"; do
  for s in "${suites[@]}"; do
    [ -x "$HERE/../$s/run.sh" ] || { echo "modes.sh: no suite $s" >&2; exit 2; }
    t0=$(date +%s)
    FAKE_PROC=$m "$HERE/../$s/run.sh" > "$O/$m-$s.txt" 2>&1; st=$?
    left=$(ps -A -o command= 2>/dev/null | awk '$1 == "fake-xnuhost" && index($0, "/out/rustport-")' | wc -l | tr -d ' ')
    printf '%-9s %-6s status %d: %3d ok, %2d failed, %s s%s\n' "$m" "$s" "$st" "$(grep -c '^ok ' "$O/$m-$s.txt")" \
      "$(grep -c '^FAILED  ' "$O/$m-$s.txt")" "$(($(date +%s) - t0))" "$([ "$left" = 0 ] || echo ", $left stand-in session(s) left running")"
    [ "$st" = 0 ] && [ "$left" = 0 ] || { failed=$((failed + 1)); grep '^FAILED  ' "$O/$m-$s.txt" | cut -c1-160 | sed 's/^/    /'; }
  done
done
[ $failed = 0 ] && echo "ALL OK" || { echo "FAILED: $failed"; exit 1; }
