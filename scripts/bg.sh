#!/bin/bash
# Run a long job in tmux so it survives the session: scripts/bg.sh NAME COMMAND...
# scripts/bg.sh -s NAME shows its state; the log is logs/NAME.log, the exit code logs/NAME.exit.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
mkdir -p "$ROOT/logs"
if [ "${1:-}" = -s ]; then
  n=$2
  if tmux has-session -t "oneios-$n" 2>/dev/null; then echo "$n: running"; else echo "$n: exit $(cat "$ROOT/logs/$n.exit" 2>/dev/null || echo '?')"; fi
  tail -5 "$ROOT/logs/$n.log" 2>/dev/null || true
  exit 0
fi
n=$1; shift
rm -f "$ROOT/logs/$n.exit"
cmd=$(printf '%q ' "$@")
tmux new-session -d -s "oneios-$n" -c "$ROOT" "bash -c '$cmd > logs/$n.log 2>&1; echo \$? > logs/$n.exit'"
echo "started $n: logs/$n.log"
