#!/bin/bash
# tools/oneios-tool/tests/log/run.sh   every `oneios-tool log` subcommand on real xnuhost logs.
#
# Fails when a run panics or ends with a status other than 0, or when two runs of one command differ. Prints the
# wall time and the size of every output. The outputs stay in out/rustport-log/ to be read.
#
# Logs: ONEIOS_LOGS="a.log b.log ..." or every *.log of ONEIOS_LOG_DIR (default out/logs). The tool:
# ONEIOS_TOOL=path (default tools/oneios-tool/target/release/oneios-tool). `stacks` names addresses when
# `oneios-tool sym` has an index (ONEIOS_DSC_DIR builds it; without both it prints the addresses and says so).
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
tool=${ONEIOS_TOOL:-$root/tools/oneios-tool/target/release/oneios-tool}
work=$root/out/rustport-log
[ -x "$tool" ] || { echo "no tool at $tool: build it or set ONEIOS_TOOL" >&2; exit 2; }
logs=${ONEIOS_LOGS:-$(ls "${ONEIOS_LOG_DIR:-$root/out/logs}"/*.log 2>/dev/null)}
[ -n "$logs" ] || { echo "no logs: set ONEIOS_LOGS or ONEIOS_LOG_DIR" >&2; exit 2; }
mkdir -p "$work"
status=0
TIMEFORMAT=%R
started=$SECONDS
check() {  # check NAME ARGS...: run twice, compare, report
  local name=$1; shift
  local took code
  took=$( { time "$tool" log "$@" > "$work/$name.txt" 2> "$work/$name.err"; echo $? > "$work/$name.status"; } 2>&1 )
  code=$(cat "$work/$name.status")
  "$tool" log "$@" > "$work/$name.again" 2>/dev/null
  if [ "$code" != 0 ] || grep -q "panicked" "$work/$name.err"; then
    echo "FAIL   $name: status $code: $(head -2 "$work/$name.err")"; status=1
  elif ! cmp -s "$work/$name.txt" "$work/$name.again"; then
    echo "FAIL   $name: two runs differ"; status=1
  else
    printf 'ok     %-34s %6s s  %7d lines  %s\n' "$name" "$took" "$(wc -l < "$work/$name.txt")" "$(head -1 "$work/$name.err")"
  fi
  rm -f "$work/$name.again" "$work/$name.status"
}
first=""
for log in $logs; do
  base=$(basename "$log" .log)
  for command in stacks timeline gpu cpu display perf; do
    check "$base.$command" "$command" "$log"
  done
  check "$base.stacks-summary" stacks "$log" --summary
  check "$base.stacks-raw" stacks "$log" --no-sym
  for command in gpu cpu display perf; do
    check "$base.$command.json" "$command" "$log" --json
    check "$base.$command.csv" "$command" "$log" --csv
    python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$work/$base.$command.json.txt" || { echo "FAIL   $base.$command.json: not JSON"; status=1; }
  done
  [ -n "$first" ] && check "diff.$(basename "$first" .log).$base" diff "$first" "$log"
  [ -z "$first" ] && first=$log
done
echo "wall time: $((SECONDS - started)) s; outputs in $work"
[ $status = 0 ] && echo "all ran" || echo "FAILURES"
exit $status
