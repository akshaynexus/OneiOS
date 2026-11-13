#!/bin/bash
# `oneios-tool rangeserve` against the frozen answers of scripts/serve/rangeserve.py, which it replaced on
# 2026-11-13: the status, the headers that matter (Content-Length, Content-Range, Accept-Ranges, Content-Type of a
# file) and the SHA-256 of the body, for whole files, ranges of every form, ranges a file does not have, and the
# names that are not served.
#
#   tools/oneios-tool/tests/servepy/compare-golden.sh     every request against golden/cases.txt; exit 1 on a difference
#   UPDATE=1 ...                                          write golden/cases.txt from this run instead
#   IMPL=python ...                                       start rangeserve.py instead: only at a commit that still
#                                                         has it (the golden was frozen with IMPL=python UPDATE=1)
#
# The folder served is made here (made-up bytes); the server binds 127.0.0.1 on PORT (default 18787) and is ended
# at the end. Needs curl. Not compared: the Server and Date headers and the body of a 404 (Python's own page).
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
TOOL=${ONEIOS_TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
IMPL=${IMPL:-rust}; PORT=${PORT:-18787}
[ -x "$TOOL" ] || { echo "compare-golden: no $TOOL (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)" >&2; exit 2; }
command -v curl >/dev/null || { echo "compare-golden: no curl" >&2; exit 2; }
W=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/servepy.XXXXXX")" && pwd -P)
sha() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
mkdir -p "$W/dir/sub"
i=0; while [ $i -lt 3000 ]; do printf 'line %05d of a made-up file\n' $i; i=$((i + 1)); done > "$W/dir/set.tar.zst"
printf 'abc  set.tar.zst\n' > "$W/dir/set.tar.zst.sha256"; : > "$W/dir/empty"; echo hidden > "$W/dir/.hidden"; echo inner > "$W/dir/sub/inner"
echo "a name with a space" > "$W/dir/a b.txt"; ln -s set.tar.zst "$W/dir/link.zst"; ln -s sub "$W/dir/dirlink"
if [ "$IMPL" = python ]; then
  [ -f "$ROOT/scripts/serve/rangeserve.py" ] || { echo "compare-golden: scripts/serve/rangeserve.py is not there (retired): IMPL=python cannot run" >&2; exit 2; }
  python3 -I "$ROOT/scripts/serve/rangeserve.py" "$W/dir" "$PORT" 2> "$W/server.log" &
else
  "$TOOL" rangeserve "$W/dir" "$PORT" 2> "$W/server.log" &
fi
SERVER=$!
trap 'kill $SERVER 2>/dev/null; [ "${KEEP:-0}" = 1 ] || rm -rf "$W"' EXIT
n=0; until curl -s -o /dev/null "http://127.0.0.1:$PORT/empty" || [ $n -ge 50 ]; do n=$((n + 1)); sleep 0.1; done
FOUND=$W/found.txt; : > "$FOUND"
# q NAME PATH [curl options...]: one request: the status, the headers compared, the body's hash and length
q() {
  local name=$1 path=$2; shift 2
  curl -s -D "$W/h" -o "$W/b" "$@" "http://127.0.0.1:$PORT$path"
  {
    echo "== $name"
    head -1 "$W/h" | tr -d '\r' | cut -d' ' -f2
    case "$(head -1 "$W/h")" in *" 404 "*) ;; *) tr -d '\r' < "$W/h" | grep -iE '^(content-length|content-range|accept-ranges):' | tr 'A-Z' 'a-z' | LC_ALL=C sort ;; esac
    tr -d '\r' < "$W/h" | grep -i '^content-type: application/octet-stream' | tr 'A-Z' 'a-z'
    case "$(head -1 "$W/h") $*" in *" 404 "*) echo "body: (the server's own page)" ;; *" -I"*) echo "body: none (HEAD)" ;; *) echo "body: $(wc -c < "$W/b" | tr -d ' ') bytes, sha256 $(sha < "$W/b" | cut -c1-64)" ;; esac
  } >> "$FOUND"
}
r() { q "$1" /set.tar.zst -H "Range: $2"; }
q "GET the file" /set.tar.zst
q "HEAD the file" /set.tar.zst -I
q "GET the .sha256" /set.tar.zst.sha256
q "GET an empty file" /empty
q "GET a link to a file" /link.zst
q "GET a name with a space (percent-encoded), with a query" '/a%20b.txt?part=1'
r "range: the first hundred bytes" "bytes=0-99"
r "range: from an offset to the end" "bytes=80000-"
r "range: an end past the file" "bytes=83900-999999999"
r "range: the last 50 bytes" "bytes=-50"
r "range: the last bytes, more than the file has" "bytes=-999999999"
r "range: one byte" "bytes=5-5"
r "range: a start alone, no hyphen" "bytes=5"
r "range: blanks around the numbers" "bytes= 10-19 "
q "HEAD with a range" /set.tar.zst -I -H "Range: bytes=10-19"
r "range: a start at the file's end (416)" "bytes=87000-"
r "range: an end before the start (416)" "bytes=9-3"
r "range: the last 0 bytes (416)" "bytes=-0"
r "range: no numbers (416)" "bytes=-"
r "range: letters (416)" "bytes=a-b"
r "range: two ranges (the whole file)" "bytes=0-1,5-6"
r "range: another unit (the whole file)" "items=0-1"
q "range of an empty file (416)" /empty -H "Range: bytes=0-9"
q "404: no such file" /missing
q "404: a hidden file" /.hidden
q "404: a folder" /sub
q "404: a file in a folder" /sub/inner
q "404: a file in a folder, the slash encoded" '/sub%2Finner'
q "404: a link to a folder" /dirlink
q "404: the folder itself" /
q "404: the folder above" '/%2e%2e'
kill $SERVER 2>/dev/null
if [ "${UPDATE:-0}" = 1 ]; then
  mkdir -p "$HERE/golden"; cp "$FOUND" "$HERE/golden/cases.txt"
  echo "compare-golden: golden/cases.txt written from IMPL=$IMPL ($(grep -c '^== ' "$FOUND") cases)"; exit 0
fi
block() { awk -v n="== $1" '$0 == n {on=1; print; next} /^== / {on=0} on' "$2"; }
same=0; differ=0
while read -r name; do
  if [ "$(block "$name" "$FOUND")" = "$(block "$name" "$HERE/golden/cases.txt")" ]; then same=$((same + 1)); echo "same       $name"
  else differ=$((differ + 1)); echo "DIFFERENT  $name"; diff <(block "$name" "$HERE/golden/cases.txt") <(block "$name" "$FOUND") | head -8; fi
done < <(grep '^== ' "$HERE/golden/cases.txt" | sed 's/^== //')
[ "$(grep -c '^== ' "$FOUND")" = "$(grep -c '^== ' "$HERE/golden/cases.txt")" ] || { differ=$((differ + 1)); echo "DIFFERENT  the list of cases"; }
echo "servepy (IMPL=$IMPL): $same the same, $differ different"
[ $differ = 0 ]
