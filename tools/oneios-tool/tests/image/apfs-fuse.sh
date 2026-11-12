#!/bin/bash
# tools/oneios-tool/tests/image/apfs-fuse.sh IMAGE [VOLUME [FOLDER]]
#
# The apfs-fuse backend of `oneios-tool image apfs-ls`, `apfs-copy` and `ioslog` (`--apfs fuse`, and the fallback
# of `--apfs auto`) against our own reader (`--apfs own`) on one volume of a raw APFS image. Linux, apfs-fuse and
# fusermount3 on PATH; nothing needs root.
#   - apfs-ls of the root and of FOLDER (default: the first folder of the root) with both readers: the same text;
#   - apfs-copy of FOLDER with both: the same files (diff -r);
#   - a file that is not APFS, a folder that is not there: an error, status 1;
#   - --apfs auto when our reader refuses (ONEIOS_APFS_TEST_REFUSE=1 makes it): the answer comes through apfs-fuse;
#   - Ctrl-C in the middle of a copy through apfs-fuse: status 130;
#   - after each of these nothing is left mounted and the mount's folder is gone;
#   - IOSLOG_IMAGE=FILE (a Data-store image with a log): `ioslog show -f csv` with both readers, the same file.
# One line per check, then `apfs-fuse: ok` (exit 0) or `apfs-fuse: FAIL`.
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
tool=${ONEIOS_TOOL:-$root/tools/oneios-tool/target/release/oneios-tool}
image=${1:?usage: apfs-fuse.sh IMAGE [VOLUME [FOLDER]]}
volume=${2:-0}
work=${WORK:-$root/out/rustport-apfs-fuse}
command -v apfs-fuse >/dev/null || { echo "apfs-fuse.sh: no apfs-fuse on PATH" >&2; exit 2; }
[ -x "$tool" ] || { echo "apfs-fuse.sh: no tool at $tool" >&2; exit 2; }
rm -rf "$work"; mkdir -p "$work"
fail=0
ok() { echo "ok      $1"; }
bad() { echo "FAILED  $1${2:+: $2}"; fail=1; }
left() { # nothing of ours mounted, no mount folder left
  ! grep -q " ${TMPDIR:-/tmp}/oneios-apfs-" /proc/mounts && ! ls -d "${TMPDIR:-/tmp}"/oneios-apfs-* >/dev/null 2>&1
}
clean() { left && ok "$1: nothing left mounted, the mount's folder is gone" || bad "$1: a mount or its folder is left" "$(grep oneios-apfs /proc/mounts | head -n 2)"; }
t() { "$tool" image "$@" --volume "$volume"; }

t apfs-ls "$image" --apfs own > "$work/ls.own" 2>&1; t apfs-ls "$image" --apfs fuse > "$work/ls.fuse" 2>&1
cmp -s "$work/ls.own" "$work/ls.fuse" && [ -s "$work/ls.own" ] && ok "apfs-ls /: both readers print the same ($(wc -l < "$work/ls.own") entries)" || bad "apfs-ls /" "$(diff "$work/ls.own" "$work/ls.fuse" | head -n 3 | tr '\n' '|')"
folder=${3:-$(awk '$1 ~ /^04/ {print $5; exit}' "$work/ls.own")}
t apfs-ls "$image" "$folder" --apfs own > "$work/ls2.own" 2>&1; t apfs-ls "$image" "$folder" --apfs fuse > "$work/ls2.fuse" 2>&1
cmp -s "$work/ls2.own" "$work/ls2.fuse" && [ -s "$work/ls2.own" ] && ok "apfs-ls /$folder: the same ($(wc -l < "$work/ls2.own") entries)" || bad "apfs-ls /$folder" "$(diff "$work/ls2.own" "$work/ls2.fuse" | head -n 3 | tr '\n' '|')"
clean "apfs-ls"
t apfs-copy "$image" "$folder" "$work/own" --apfs own > "$work/copy.own" 2>&1; s1=$?
t apfs-copy "$image" "$folder" "$work/fuse" --apfs fuse > "$work/copy.fuse" 2>&1; s2=$?
[ $s1 = 0 ] && [ $s2 = 0 ] && diff -r --no-dereference "$work/own" "$work/fuse" > "$work/diff.txt" 2>&1 \
  && [ "$(sed 's/ (.*//' "$work/copy.own")" = "$(cat "$work/copy.fuse")" ] && ok "apfs-copy /$folder: the same files with both readers ($(cat "$work/copy.own"))" \
  || bad "apfs-copy /$folder" "status $s1 $s2: $(head -n 2 "$work/diff.txt" | tr '\n' '|') $(cat "$work/copy.fuse")"
clean "apfs-copy"
out=$("$tool" image apfs-ls "$0" --apfs fuse 2>&1); st=$?
[ $st = 1 ] && printf '%s' "$out" | grep -q "apfs-fuse did not mount volume 0" && ok "--apfs fuse of a file that is not APFS: an error, status 1" || bad "--apfs fuse of a file that is not APFS" "status $st: $out"
clean "a failed mount"
out=$(t apfs-ls "$image" no/such/folder --apfs fuse 2>&1); st=$?
[ $st = 1 ] && printf '%s' "$out" | grep -q "no /no/such/folder in the volume" && ok "--apfs fuse of a folder that is not there: status 1" || bad "--apfs fuse of a missing folder" "status $st: $out"
clean "an error after the mount"
out=$(ONEIOS_APFS_TEST_REFUSE=1 "$tool" image apfs-ls "$image" --volume "$volume" 2> "$work/auto.err"); st=$?
[ $st = 0 ] && [ "$out" = "$(cat "$work/ls.own")" ] && grep -q "our APFS reader refused; reading through apfs-fuse instead" "$work/auto.err" && ok "--apfs auto (the default): when our reader refuses, apfs-fuse answers, and stderr says so" || bad "--apfs auto fallback" "status $st: $(cat "$work/auto.err" | tr '\n' '|')"
out=$(ONEIOS_APFS_TEST_REFUSE=1 "$tool" image apfs-ls "$image" --volume "$volume" --apfs own 2>&1); st=$?
[ $st = 1 ] && ok "--apfs own: no fallback" || bad "--apfs own fell back" "status $st"
clean "the fallback"
# Ctrl-C while a copy through the mount runs
"$tool" image apfs-copy "$image" / "$work/big" --volume "$volume" --apfs fuse > /dev/null 2>&1 & pid=$!
for i in $(seq 50); do grep -q " ${TMPDIR:-/tmp}/oneios-apfs-$pid-" /proc/mounts && break; sleep 0.1; done
grep -q " ${TMPDIR:-/tmp}/oneios-apfs-$pid-" /proc/mounts && ok "a copy through apfs-fuse: the volume is mounted while it runs" || echo "note    the copy ended before it was seen mounted"
kill -INT $pid 2>/dev/null; wait $pid; st=$?
[ $st = 130 ] || [ $st = 0 ] && ok "Ctrl-C in a copy through apfs-fuse: status $st" || bad "Ctrl-C" "status $st"
sleep 0.3; clean "Ctrl-C"
rm -rf "$work/big"
if [ -n "${IOSLOG_IMAGE:-}" ]; then
  "$tool" ioslog show "$IOSLOG_IMAGE" -f csv -o "$work/log.own.csv" --apfs own > /dev/null 2> "$work/ioslog.err"; s1=$?
  "$tool" ioslog show "$IOSLOG_IMAGE" -f csv -o "$work/log.fuse.csv" --apfs fuse > /dev/null 2>> "$work/ioslog.err"; s2=$?
  [ $s1 = 0 ] && [ $s2 = 0 ] && cmp -s "$work/log.own.csv" "$work/log.fuse.csv" && ok "ioslog show -f csv: the same file with both readers ($(stat -c %s "$work/log.own.csv") bytes)" || bad "ioslog --apfs fuse" "status $s1 $s2: $(tail -n 2 "$work/ioslog.err" | tr '\n' '|')"
  clean "ioslog"
fi
[ -n "${KEEP:-}" ] || rm -rf "$work"
[ $fail = 0 ] && echo "apfs-fuse: ok" || echo "apfs-fuse: FAIL"
exit $fail
