#!/bin/bash
# Serve an image set archive (and its .sha256) to the owner's phone over several Cloudflare quick tunnels at once,
# so a download manager can fetch it in parallel parts from all of them. Private use only: the archive holds Apple's
# files and is never published; the URLs are random and the tunnels end with this script's sessions.
#
#   scripts/serve/serve-image.sh FILE [TUNNELS] [PORT]    start (default 4 tunnels, port 8787); prints the URLs
#   scripts/serve/serve-image.sh --stop                   end the server and every tunnel
#
# The server (scripts/serve/rangeserve.py) binds 127.0.0.1 and answers byte ranges; each tunnel is its own
# cloudflared process in tmux (oneios-serve-http, oneios-serve-tN). URLs: logs/serve-urls.txt.
set -uo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
mkdir -p "$ROOT/logs"
if [ "${1:-}" = --stop ]; then
  for s in $(tmux ls -F '#S' 2>/dev/null | grep -E '^oneios-serve-'); do tmux kill-session -t "$s"; done
  echo "stopped"; exit 0
fi
FILE=$(realpath "${1:?usage: serve-image.sh FILE [TUNNELS] [PORT] | --stop}")
N=${2:-4}; PORT=${3:-8787}
[ -f "$FILE" ] || { echo "no file $FILE"; exit 2; }
command -v cloudflared >/dev/null || { echo "cloudflared is not installed"; exit 2; }
SERVE="$ROOT/logs/serve"; rm -rf "$SERVE"; mkdir -p "$SERVE"
ln -sf "$FILE" "$SERVE/"; [ -f "$FILE.sha256" ] && ln -sf "$FILE.sha256" "$SERVE/"
NAME=$(basename "$FILE")
tmux kill-session -t oneios-serve-http 2>/dev/null
tmux new-session -d -s oneios-serve-http "python3 $ROOT/scripts/serve/rangeserve.py $SERVE $PORT 2> $ROOT/logs/serve-http.log"
for i in $(seq 1 "$N"); do
  tmux kill-session -t "oneios-serve-t$i" 2>/dev/null
  : > "$ROOT/logs/serve-t$i.log"
  tmux new-session -d -s "oneios-serve-t$i" "cloudflared tunnel --no-autoupdate --url http://127.0.0.1:$PORT > $ROOT/logs/serve-t$i.log 2>&1"
done
URLS=()
for i in $(seq 1 "$N"); do
  for _ in $(seq 1 45); do
    u=$(grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' "$ROOT/logs/serve-t$i.log" | head -1)
    [ -n "$u" ] && break; sleep 1
  done
  [ -n "$u" ] && URLS+=("$u/$NAME") || echo "tunnel $i did not come up (logs/serve-t$i.log)"
done
printf '%s\n' "${URLS[@]}" > "$ROOT/logs/serve-urls.txt"
echo "file: $NAME ($(( $(stat -c%s "$FILE") / 1048576 )) MiB)"
printf 'url: %s\n' "${URLS[@]}"
echo
echo "aria2c (Termux: pkg install aria2), all tunnels at once, 16 connections:"
echo "  aria2c -x 16 -s 16 -k 8M -o $NAME ${URLS[*]}"
echo "stop: scripts/serve/serve-image.sh --stop"
