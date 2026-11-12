#!/bin/bash
# tools/oneios-tool/tests/ioslog/compare.sh ARCHIVE.logarchive
#
# `oneios-tool ioslog` against the manual steps it replaces (docs/newtodo.md "How to resume" step 2: mount the
# Data volume, copy db/diagnostics/{Persist,Special,Signpost,HighVolume,timesync} and db/uuidtext/* into a
# .logarchive, run Mandiant's `unifiedlog_iterator -m log-archive -i ARCHIVE -f csv -o OUT`).
#
#   1  archive:  ioslog show ARCHIVE -f csv              ==  unifiedlog_iterator on ARCHIVE          (cmp)
#   2  jsonl:    ioslog show ARCHIVE -f jsonl            ==  unifiedlog_iterator -f jsonl            (cmp; JSONL=1)
#   3  image:    a Data store image is made from ARCHIVE's files (tests/ioslog/mkapfs.py: three volumes, the
#                log in the third as db/diagnostics and db/uuidtext, block 0 one transaction behind);
#                apfs-fuse must show the same tree as the folder the image was made from       (diff -r)
#   4  save:     ioslog save IMAGE X.logarchive          ==  the manual copy out of apfs-fuse's mount (diff -r)
#   5  show:     ioslog show IMAGE -f csv                ==  unifiedlog_iterator on the manual copy  (cmp)
#   6  mounted:  ioslog show <apfs-fuse mount>/root -f csv == the same                               (cmp)
#   7  block 0:  ioslog show IMAGE --block0 must find no log (the files are in the newest checkpoint only)
#   8  filters:  ioslog show with --boot/--proc/--type/--grep keeps exactly the CSV rows a check in Python keeps
#
# Needs: the tool built; REF_ITER = Mandiant's unifiedlog_iterator built from the commit Cargo.lock pins
# (git clone https://github.com/mandiant/macos-unifiedlogs; cargo build --release --manifest-path
# examples/Cargo.toml -p unifiedlog_iterator); apfs-fuse for steps 3, 4 and 6 (Linux; without it they are skipped
# and step 5 compares with unifiedlog_iterator on ioslog's own saved archive). ARCHIVE: any .logarchive, for
# example one of Mandiant's test archives (BUILDING.md there) or the Mac's out/session-logs/*/ios3.logarchive.
# Work files: WORK (default out/rustport-ioslog), several times the archive's size. KEEP=1 keeps them.
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
tool=${ONEIOS_TOOL:-$root/tools/oneios-tool/target/release/oneios-tool}
ref=${REF_ITER:?set REF_ITER to Mandiant\'s unifiedlog_iterator (see this script\'s header)}
archive=${1:?usage: compare.sh ARCHIVE.logarchive}
archive=$(cd "$archive" && pwd)
work=${WORK:-$root/out/rustport-ioslog}/$(basename "$archive" .logarchive)
[ -x "$tool" ] || { echo "no tool at $tool: build it or set ONEIOS_TOOL" >&2; exit 2; }
[ -x "$ref" ] || { echo "no unifiedlog_iterator at $ref" >&2; exit 2; }
fuse=1; command -v apfs-fuse >/dev/null || fuse=0
rm -rf "$work"; mkdir -p "$work/refrun"
status=0
TIMEFORMAT=%R
ok() { echo "ok     $*"; }
bad() { echo "FAIL   $*"; status=1; }
same() { if cmp -s "$2" "$3"; then ok "$1: the same ($(wc -c < "$2") bytes)"; else bad "$1: $2 and $3 differ"; fi; }
# unifiedlog_iterator appends to its output and writes issues.jsonl into the working directory
reference() {  # reference ARCHIVE FORMAT OUT
  rm -f "$3"
  (cd "$work/refrun" && "$ref" -m log-archive -i "$1" -f "$2" -o "$3") || bad "unifiedlog_iterator on $1"
}

echo "== 1 archive"
t=$( { time reference "$archive" csv "$work/ref.csv"; } 2>&1 ); echo "unifiedlog_iterator: $t s, $(wc -l < "$work/ref.csv") lines"
t=$( { time "$tool" ioslog show "$archive" -f csv -o "$work/ours.csv" 2> "$work/ours.err"; } 2>&1 ); echo "ioslog show:         $t s, $(cat "$work/ours.err")"
same "csv of the archive" "$work/ours.csv" "$work/ref.csv"

if [ -n "${JSONL:-}" ]; then
  echo "== 2 jsonl"
  reference "$archive" jsonl "$work/ref.jsonl"
  "$tool" ioslog show "$archive" -f jsonl -o "$work/ours.jsonl" 2>/dev/null
  same "jsonl of the archive" "$work/ours.jsonl" "$work/ref.jsonl"
fi

echo "== 3 image"
# The Data volume as the phone has it: /private/var is the volume's root, so the log is db/diagnostics and
# db/uuidtext. No logdata.LiveData.tracev3 there (only `log collect` writes one).
stage=$work/stage
mkdir -p "$stage/db/diagnostics" "$stage/db/uuidtext" "$stage/root/log" "$stage/mobile/Library"
for d in Persist Special Signpost HighVolume timesync; do [ -d "$archive/$d" ] && cp -R "$archive/$d" "$stage/db/diagnostics/"; done
for e in "$archive"/*; do
  n=$(basename "$e")
  case "$n" in Persist|Special|Signpost|HighVolume|timesync|Extra|Info.plist|logdata.LiveData.tracev3) ;; *) cp -R "$e" "$stage/db/uuidtext/" ;; esac
done
echo "not a log" > "$stage/root/log/other.txt"
ln -s ../root/log "$stage/mobile/Library/Logs"
mkdir -p "$work/small/a"; echo hello > "$work/small/a/file"
python3 -I "$here/mkapfs.py" "$work/data.img" "Preboot=$work/small" "xART=" "Data=$stage" --stale-block0 || { bad "mkapfs.py"; exit 1; }
"$tool" image apfs-info "$work/data.img" | grep '"name"' | tr -d '\n'; echo
manual=$work/manual.logarchive
if [ $fuse = 1 ]; then
  mkdir -p "$work/mnt"
  if apfs-fuse -o ro,vol=2 "$work/data.img" "$work/mnt" 2> "$work/fuse.err"; then
    trap 'fusermount3 -u "$work/mnt" 2>/dev/null' EXIT
    if diff -r --no-dereference "$stage" "$work/mnt/root" > "$work/fuse.diff" 2>&1; then
      ok "apfs-fuse reads the image as the folder it was made from ($(find "$stage" -type f | wc -l) files)"
    else
      bad "apfs-fuse and the staged folder differ: $(head -3 "$work/fuse.diff")"
    fi
    echo "== 4 save"
    # the manual steps
    mkdir -p "$manual"
    cp -R "$work/mnt/root/db/diagnostics/"{Persist,Special,Signpost,HighVolume,timesync} "$manual/" 2>/dev/null
    cp -R "$work/mnt/root/db/uuidtext/"* "$manual/"
  else
    bad "apfs-fuse cannot mount the image: $(head -2 "$work/fuse.err")"; fuse=0
  fi
else
  echo "skip   no apfs-fuse: steps 3, 4 and 6 not run"
fi
t=$( { time "$tool" ioslog save "$work/data.img" "$work/saved.logarchive" > "$work/save.out" 2>&1; } 2>&1 ) || true
echo "ioslog save: $t s, $(cat "$work/save.out")"
if [ $fuse = 1 ]; then
  if diff -r "$work/saved.logarchive" "$manual" > "$work/save.diff" 2>&1; then
    ok "ioslog save: the same files as the manual copy ($(find "$manual" -type f | wc -l) files, $(du -sk "$manual" | cut -f1) KiB)"
  else
    bad "ioslog save and the manual copy differ: $(head -3 "$work/save.diff")"
  fi
else
  manual=$work/saved.logarchive
fi

echo "== 5 show"
reference "$manual" csv "$work/ref-image.csv"
t=$( { time "$tool" ioslog show "$work/data.img" -f csv -o "$work/ours-image.csv" 2> "$work/ours-image.err"; } 2>&1 ); echo "ioslog show IMAGE: $t s, $(cat "$work/ours-image.err")"
same "csv of the image" "$work/ours-image.csv" "$work/ref-image.csv"

if [ $fuse = 1 ]; then
  echo "== 6 mounted"
  "$tool" ioslog show "$work/mnt/root" -f csv -o "$work/ours-mounted.csv" 2>/dev/null
  same "csv of the mounted volume" "$work/ours-mounted.csv" "$work/ref-image.csv"
fi

echo "== 7 block 0"
if "$tool" ioslog show "$work/data.img" --block0 -f csv -o "$work/block0.csv" 2> "$work/block0.err"; then
  bad "--block0 found a log in the older superblock"
else
  ok "--block0: $(head -1 "$work/block0.err")"
fi

echo "== 8 filters"
"$tool" ioslog boots "$work/data.img" > "$work/boots.txt" 2>&1 || bad "ioslog boots"
tail -n +2 "$work/boots.txt" | head -6
python3 -I "$here/filters.py" "$tool" "$work/data.img" "$work/ref-image.csv" "$work" || status=1

[ -n "${KEEP:-}" ] || { [ $fuse = 1 ] && fusermount3 -u "$work/mnt" 2>/dev/null; trap - EXIT; rm -rf "$work/stage" "$work/data.img" "$work/saved.logarchive" "$work/manual.logarchive" "$work"/*.csv "$work"/*.jsonl; }
[ $status = 0 ] && echo "ioslog compare: all the same" || echo "ioslog compare: FAILED"
exit $status
