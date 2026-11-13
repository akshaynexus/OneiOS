#!/bin/bash
# oneios-tool phone pull-data against the stand-in phone (fake-adb.sh): the copy in chunks, its retries, its
# resume and its check against the phone's sums. No phone and no root needed.
#
#   tools/oneios-tool/tests/phone/pull-resume.sh    one line per check (ok | FAILED), then `ALL OK` (exit 0) or `FAILED: N`
#   SPEED=4096:280 ...                              also the time of a copy of a 4096 MiB store with about 280 MiB
#                                                   of data (debug mkstream phone): the one gzip stream of the first
#                                                   port by hand, then pull-data with --verify chunk and --verify end
#
# The store is 40 MiB and 12345 bytes, copied with --chunk 4 (11 chunks: 7 hold data, 4 are zeros; the last is the
# 12345 bytes): data at the start, a chunk without a zero, a chunk that is zeros but for its last byte, data
# across the edge of two chunks, data off a block's edge, and "the end" in the last bytes.
#
# The stand-in phone's busybox is replaced by one that does what $FAKE_PHONE/.fake/pull asks of the Nth `gzip`
# (the Nth chunk that is copied; a chunk that is copied again counts again):
#   cut       the stream ends after `cut-bytes` bytes: an incomplete deflate stream (`cut-status`: adb's status)
#   short     the range is cut to `short-bytes` before it is packed: a whole stream of too few bytes
#   flip      the range's first byte is another: a whole stream of the right length that is not the phone's data
#   hold      the stream stands still after `hold-bytes` bytes until .fake/pull/go is there (at most 30 s)
#   change    change.sh runs first: the store changes while it is copied
# What is checked: a plain copy; a stream that breaks once, three times, and more often than --retries (the
# command gives up, says how to go on, and the next run goes on); a run that is killed (SIGKILL), stopped
# (SIGTERM) or interrupted (Ctrl-C: SIGINT) in the middle of a chunk and run again; a second run beside the
# first; a stream that stalls; a chunk whose data is not the phone's (copied again, also under --verify end);
# --verify end, and a copy begun with it finished with --verify chunk; the store's size and its content
# changing during the copy, and a copy of the old store that is not gone on from; --restart; --chunk; a .part
# without a progress file and a progress file that is not one; the refusal while a session is open. After every
# copy that ends well: `cmp` with the phone's file, not more bytes on disk than the phone's file holds
# (st_blocks: the zeros are holes), no .part and no progress file left.
# Env: TOOL (the binary), WORK (the scratch folder, default out/rustport-pull), KEEP=1 (keep it),
# FAKE_PROC=host|stand-in (as tests/phone/run.sh).
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
TOOL=${TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
W=${WORK:-$ROOT/out/rustport-pull}
[ -x "$TOOL" ] || { echo "pull-resume.sh: no $TOOL (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)" >&2; exit 2; }
. "$HERE/lib.sh"
rm -rf "$W"; mkdir -p "$W"
export ADB=$HERE/fake-adb.sh FAKE_ADB_LOG=$W/adb.log FAKE_ROOT_LOG=$W/root.log
[ -n "${FAKE_PROC:-}" ] || { [ -r /proc/self/cmdline ] && FAKE_PROC=host || FAKE_PROC=stand-in; }
export FAKE_PROC; echo "(the stand-in's /proc: $FAKE_PROC)"
unset ANDROID_SERIAL FAKE_NO_ROOT FAKE_DEVICES
P=$W/phone; export FAKE_PHONE=$P
make_phone "$P"
S=$P/data/local/tmp/oneios
F=$P/.fake/pull
SRC=$S/data/data.img
MIB=1048576
SIZE=$((40 * MIB + 12345))

# ---- the stand-in phone's busybox, with the faults
cat > "$P/data/adb/ksu/bin/busybox" <<'SH'
#!/bin/bash
F=$FAKE_PHONE/.fake/pull
[ "$1" = gzip ] && [ -d "$F" ] || exec "$@"
n=$(($(cat "$F/gzip.n" 2>/dev/null || echo 0) + 1)); echo $n > "$F/gzip.n"
asked() { grep -qx "$n" "$F/$1" 2>/dev/null; }
if asked change; then sh "$F/change.sh"; fi
if asked flip; then { head -c 1 | tr '\000-\377' '\001-\377\000'; cat; } | "$@"; exit; fi
if asked short; then head -c "$(cat "$F/short-bytes")" | "$@"; exit; fi
if asked cut; then "$@" | head -c "$(cat "$F/cut-bytes")"; exit "$(cat "$F/cut-status" 2>/dev/null || echo 0)"; fi
if asked hold; then
  "$@" | { head -c "$(cat "$F/hold-bytes")"; touch "$F/holding"; i=0; while [ ! -e "$F/go" ] && [ $i -lt 300 ]; do sleep 0.1; i=$((i + 1)); done; cat; }
  exit
fi
exec "$@"
SH
chmod +x "$P/data/adb/ksu/bin/busybox"
# faults [NAME VALUE]...: the faults of the next runs (none without arguments); the count of gzip calls begins again
faults() {
  release
  rm -rf "$F"; mkdir -p "$F"
  while [ $# -gt 1 ]; do printf '%s\n' $2 > "$F/$1"; shift 2; done
}
# release: a stream that is held goes on, and what the stand-in phone still runs of it ends
release() {
  [ -d "$F" ] || return 0
  touch "$F/go"
  wait_for 10 none_held || echo "(a stand-in busybox is still running)"
}
# none_held: no busybox of the stand-in phone runs (one that holds a stream stays until it is released)
none_held() { [ -z "$(pgrep -f "$P/data/adb/ksu/bin/busybox")" ]; }
gzips() { cat "$F/gzip.n" 2>/dev/null || echo 0; }

# ---- the store
put() { dd of="$SRC" bs=64k oflag=seek_bytes seek="$1" conv=notrunc status=none; }
make_store() {
  rm -f "$SRC"; truncate -s "$SIZE" "$SRC"
  printf 'first block' | put 0
  head -c $((3 * MIB)) /dev/urandom | put $((MIB / 2))                # chunk 1: data with zeros in front and behind
  head -c $((4 * MIB)) /dev/urandom | put $((8 * MIB))                # chunk 3: no zero run (chunk 2 is zeros)
  printf 'Z' | put $((16 * MIB - 1))                                  # chunk 4: zeros but for its last byte
  head -c "$MIB" /dev/urandom | put $((20 * MIB - MIB / 2))           # across the edge of chunks 5 and 6
  head -c 100000 /dev/urandom | put $((37 * MIB + 12345))             # chunk 10, off a block's edge (7, 8, 9 are zeros)
  printf 'the end' | put $((SIZE - 7))                                # chunk 11: the 12345 bytes behind the last whole chunk
  touch -d '2026-11-12 10:00:00' "$SRC"                               # any later write gives it another time
  measure
}
on_disk() { sync "$1"; echo $(($(stat -c %b "$1") * 512)); }
measure() { SRC_HELD=$(on_disk "$SRC"); }
make_store
echo "(the store: $SIZE bytes, $SRC_HELD on disk; 11 chunks of 4 MiB, 7 with data)"

# pull TAG NAME [options]: `phone pull-data $W/NAME.img --chunk 4 --backoff 0 options`; the output in $W/TAG.out,
# the adb calls in $W/TAG.adb, the status in $st
pull() {
  TAG=$1; local name=$2; shift 2
  FAKE_ADB_LOG=$W/$TAG.adb "$TOOL" phone pull-data "$W/$name.img" --backoff 0 "$@" > "$W/$TAG.out" 2>&1; st=$?
}
status() { [ "$st" = "$2" ] && ok "$1: status $2" || bad "$1: status" "$st, wanted $2: $(tail -n 3 "$W/$TAG.out" | cut -c1-300 | tr '\n' '|')"; }
# said NAME PATTERN [TAG]: a line of the run's output matches
said() { grep -Eq -- "$2" "$W/${3:-$TAG}.out" && ok "$1" || bad "$1" "no /$2/ in: $(tail -n 4 "$W/${3:-$TAG}.out" | cut -c1-300 | tr '\n' '|')"; }
# same NAME FILE: FILE is the phone's store with its holes, and nothing of the copy's own is left
same() {
  local name=$1 f=$2 held
  cmp "$f" "$SRC" > "$W/cmp.out" 2>&1 || { bad "$name: the copy is not the phone's data.img" "$(head -n 1 "$W/cmp.out")"; return; }
  held=$(on_disk "$f")
  [ "$held" -le $((SRC_HELD + 65536)) ] || { bad "$name: the zeros are not holes" "$held bytes on disk, the phone's file holds $SRC_HELD"; return; }
  [ ! -e "$f.part" ] && [ ! -e "$f.part.progress" ] || { bad "$name: .part or its progress file is left"; return; }
  ok "$name: the copy is the phone's data.img (cmp), $held bytes on disk (the phone's file: $SRC_HELD), no .part and no progress file"
}
# kept NAME FILE: a copy that did not end keeps its .part (the store's length) and its progress file, and is not FILE yet
kept() {
  [ ! -e "$2" ] && [ "$(stat -c %s "$2.part" 2>/dev/null)" = "$SIZE" ] && [ -s "$2.part.progress" ] \
    && ok "$1: no $(basename "$2") yet; .part (the store's length) and .part.progress stay" || bad "$1: what stays" "$(ls -l "$2"* 2>&1 | tr '\n' '|')"
}
C="--chunk 4"

# ---- a plain copy
faults
pull plain plain $C; status "a copy" 0
same "a copy" "$W/plain.img"
said "the summary: bytes written, bytes over adb, the time, MB/s of the store and over adb" "^$W/plain.img: $SIZE bytes, [0-9]+ of them written, [0-9]+ bytes over adb \([0-9]+ s, [0-9.]+ MB/s, [0-9.]+ MB/s over adb\); 11 chunk\(s\) of 4 MiB: 7 copied, 4 of zeros not copied, 0 retried, every chunk's sha256 is the phone's$"
said "a chunk's line: its bytes, its time, its MB/s, and the run so far" "^chunk 3/11 at 0.01 GiB: 4194304 bytes, 4194304 written, [0-9]+ over adb, [0-9.]+ s, [0-9.]+ MB/s \([0-9.]+ MB/s over adb\); the phone's sha256 is the copy's \| this run: 0.01 GiB in [0-9]+ s, [0-9.]+ MB/s \([0-9.]+ MB/s over adb\)$"
said "a chunk of zeros is a hole and is not copied" "^chunk 2/11 at 0.00 GiB: zeros \(the phone's sha256\): a hole, not copied; [0-9.]+ s, [0-9.]+ MB/s"
[ "$(gzips)" = 7 ] && ok "7 streams for the 7 chunks with data" || bad "the streams" "$(gzips) gzip calls"
dd="/data/adb/ksu/bin/busybox dd if=/data/local/tmp/oneios/data/data.img bs=1M skip=[0-9]+ count=[0-9]+ 2>/dev/null"
[ "$(grep -c "exec-out" "$W/plain.adb")" = 18 ] && ! grep "exec-out" "$W/plain.adb" | grep -Evq "^-s FAKE0001 exec-out su -c '$dd \| /data/adb/ksu/bin/busybox (gzip -1 -c|sha256sum)'$" \
  && ok "every stream is a range of the one named file: dd skip= count= into gzip -1 or sha256sum (18: 11 sums, 7 copies)" || bad "the streams sent" "$(grep exec-out "$W/plain.adb" | grep -Ev "sha256sum'$|gzip -1 -c'$" | head -n 2 | tr '\n' '|') ($(grep -c exec-out "$W/plain.adb"))"
grep -q "exec-out su -c '/data/adb/ksu/bin/busybox dd if=/data/local/tmp/oneios/data/data.img bs=1M skip=40 count=1 2>/dev/null | /data/adb/ksu/bin/busybox gzip -1 -c'" "$W/plain.adb" \
  && ok "the last chunk is the 12345 bytes behind 40 MiB (skip=40 count=1)" || bad "the last chunk's range"
pull one one --chunk 64; status "--chunk 64 (one chunk for the whole store)" 0
same "--chunk 64" "$W/one.img"
pull many many --chunk 1; status "--chunk 1 (41 chunks)" 0
same "--chunk 1" "$W/many.img"
said "--chunk 1: 41 chunks, 13 with data" "41 chunk\(s\) of 1 MiB: 13 copied, 28 of zeros not copied"
pull default default; status "no --chunk (256 MiB: one chunk)" 0
same "no --chunk" "$W/default.img"
check "--chunk 0 is refused" 1 "--chunk 0: a chunk is at least 1 MiB" "$TOOL" phone pull-data "$W/zero.img" --chunk 0

# ---- a stream that breaks in the middle of a chunk
faults cut 2 cut-bytes 1500000
pull once once $C; status "a stream that breaks once (1.5 MB into the 4.2 MB of chunk 3)" 0
said "the break is said with the chunk and where it starts again" "^chunk 3/11 at 0.01 GiB: reading the stream: .* The chunk starts again at offset 8388608 in 0 s \(retry 1 of 5\)$"
said "the summary counts the retry" "7 copied, 4 of zeros not copied, 1 retried"
same "a stream that breaks once" "$W/once.img"
[ "$(gzips)" = 8 ] && ok "only the broken chunk was copied again (8 streams)" || bad "the streams after a break" "$(gzips)"
faults cut "2 3 4" cut-bytes 1500000 cut-status 1
pull thrice thrice $C; status "a stream that breaks three times, adb ending with status 1" 0
said "the third retry of the same chunk" "^chunk 3/11 at 0.01 GiB: .* The chunk starts again at offset 8388608 in 0 s \(retry 3 of 5\)$"
said "the summary counts three retries" "7 copied, 4 of zeros not copied, 3 retried"
same "a stream that breaks three times" "$W/thrice.img"
faults short 3 short-bytes 1000000
pull short short $C; status "a whole stream of too few bytes" 0
said "too few bytes are said, and the chunk starts again" "^chunk 4/11 at 0.01 GiB: the stream gave 1000000 bytes, the range has 4194304 .* The chunk starts again at offset 12582912"
same "a whole stream of too few bytes" "$W/short.img"

# ---- more breaks than --retries: the command gives up and says how to go on; the next run goes on
faults cut "2 3 4" cut-bytes 1500000
pull giveup giveup $C --retries 2; status "more breaks than --retries 2" 1
said "giving up names the chunk, the count and the last error" "chunk 3/11 at 0.01 GiB \(offset 8388608\) failed 3 time\(s\), the last time: reading the stream: "
said "giving up says how to go on" "2 of 11 chunks are done and stay in $W/giveup.img.part \($W/giveup.img.part.progress lists them\): run the same command again and it goes on at chunk 3; --restart begins again"
kept "after giving up" "$W/giveup.img"
[ "$(grep -c '^copied ' "$W/giveup.img.part.progress")" = 2 ] && [ "$(grep -c '^verified ' "$W/giveup.img.part.progress")" = 2 ] && grep -Eq "^copied 0 4194304 [0-9a-f]{64} [0-9]+ [0-9]+$" "$W/giveup.img.part.progress" \
  && ok "the progress file: each chunk's length and sha256 as written, and the chunks verified" || bad "the progress file" "$(tr '\n' '|' < "$W/giveup.img.part.progress" | cut -c1-400)"
check "going on with another --chunk is refused" 1 "was begun with --chunk 4: go on with that" "$TOOL" phone pull-data "$W/giveup.img" --chunk 8
faults
pull giveup2 giveup; status "the same command again" 0
said "it says where it goes on" "^going on with $W/giveup.img.part: 2 of 11 chunks are copied, 2 of them verified; the first that is not done is chunk 3/11 at 0.01 GiB$"
said "it copies only what was not done (6 chunks, 3 of zeros)" "6 copied, 3 of zeros not copied, 0 retried, every chunk's sha256 is the phone's \(this run; 2 chunks were copied before"
! grep -q "skip=0 \|skip=4 " "$W/giveup2.adb" && [ "$(gzips)" = 6 ] && ok "the chunks done before were not read again (no skip=0, no skip=4; 6 streams)" || bad "the second run read a chunk that was done" "$(gzips) streams"
same "a copy that gave up and went on" "$W/giveup.img"

# ---- a run that is killed in the middle of a chunk, a second run beside it
faults hold 2 hold-bytes 1500000
FAKE_ADB_LOG=$W/kill.adb "$TOOL" phone pull-data "$W/kill.img" $C --backoff 0 > "$W/kill.out" 2>&1 & pid=$!
wait_for 30 test -e "$F/holding" && ok "a stream stands still in the middle of chunk 3" || bad "the held stream did not come"
check "a second run of the same copy is refused while the first writes" 1 "another oneios-tool is writing this copy" "$TOOL" phone pull-data "$W/kill.img" $C
kill -KILL $pid; wait $pid 2>/dev/null
kept "after SIGKILL" "$W/kill.img"
cmp -s -n $((8 * MIB)) "$W/kill.img.part" "$SRC" && ! cmp -s -n $((12 * MIB)) "$W/kill.img.part" "$SRC" \
  && ok "the .part of the killed run: chunks 1 and 2 whole, chunk 3 in part" || bad "the .part of the killed run"
faults
pull kill2 kill; status "the killed copy, run again" 0
said "it goes on at the chunk that was being copied" "the first that is not done is chunk 3/11"
same "a copy that was killed and run again" "$W/kill.img"
for sig in TERM INT; do
  faults hold 4 hold-bytes 300000
  FAKE_ADB_LOG=$W/$sig.adb "$TOOL" phone pull-data "$W/$sig.img" $C --backoff 0 > "$W/$sig.out" 2>&1 & pid=$!
  wait_for 30 test -e "$F/holding" || bad "SIG$sig: the held stream did not come"
  kill -$sig $pid; wait $pid; st=$?; TAG=$sig
  status "SIG$sig in the middle of chunk 5" 1
  said "SIG$sig: the run says where it stopped and how to go on" "stopped at chunk 5/11 at 0.02 GiB \(Ctrl-C\)\. 4 of 11 chunks are done and stay in .* run the same command again and it goes on at chunk 5"
  kept "after SIG$sig" "$W/$sig.img"
  faults
  pull ${sig}2 $sig; status "after SIG$sig, the same command again" 0
  same "a copy stopped by SIG$sig and run again" "$W/$sig.img"
done

# ---- a stream that stalls
faults hold 2 hold-bytes 1500000
pull stall stall $C --stall 1; status "a stream that stands still (--stall 1)" 0
said "the stall is said, and the chunk starts again" "^chunk 3/11 at 0.01 GiB: reading the stream: the stream gave nothing for 1 s and was ended \(--stall\) .* The chunk starts again at offset 8388608"
same "a stream that stalled" "$W/stall.img"

# ---- a chunk whose data is not the phone's: copied again
faults flip 3
pull flip flip $C; status "a chunk whose first byte is not the phone's" 0
said "the difference is said with both sums" "^chunk 4/11 at 0.01 GiB: the copy's sha256 [0-9a-f]{16} is not the phone's [0-9a-f]{16} of the same range .* The chunk starts again at offset 12582912"
said "the chunk is copied again and then has the phone's sum" "^chunk 4/11 at 0.01 GiB: 4194304 bytes, 4096 written, .* the phone's sha256 is the copy's \(copied again: the copy before it had another\)"
same "a chunk that differed (the byte the first copy wrote where the store has a zero is gone)" "$W/flip.img"

# ---- --verify end
faults
pull end end $C --verify end; status "--verify end" 0
said "--verify end copies every chunk, then asks for the sums" "^every chunk is copied; asking the phone for each chunk's sha256$"
said "--verify end: 11 chunks copied, none skipped" "11 copied, 0 of zeros not copied, 0 retried, every chunk's sha256 is the phone's$"
[ "$(gzips)" = 11 ] && [ "$(grep -c "sha256sum'$" "$W/end.adb")" = 11 ] && [ "$(grep -n "sha256sum'$" "$W/end.adb" | head -n 1 | cut -d: -f1)" -gt "$(grep -n "gzip -1 -c'$" "$W/end.adb" | tail -n 1 | cut -d: -f1)" ] \
  && ok "--verify end: 11 streams, then 11 sums" || bad "--verify end: the order of streams and sums"
same "--verify end" "$W/end.img"
faults flip 4
pull endflip endflip $C --verify end; status "--verify end with a chunk that is not the phone's" 0
said "--verify end: the chunk that differs is copied again when its sum is asked" "^chunk 4/11 at 0.01 GiB: 4194304 bytes, 4096 written, .* the phone's sha256 is the copy's \(copied again: the copy before it had another\)"
same "--verify end with a chunk that differed" "$W/endflip.img"
faults cut 6 cut-bytes 100000
pull mixed mixed $C --verify end --retries 0; status "--verify end, a break and --retries 0" 1
said "--retries 0 gives up at the first break" "chunk 6/11 at 0.02 GiB \(offset 20971520\) failed 1 time\(s\).* 0 of 11 chunks are done"
faults
pull mixed2 mixed; status "a copy begun with --verify end, gone on with --verify chunk" 0
said "the chunks copied before are checked, not copied again" "^chunk 5/11 at 0.02 GiB: the phone's sha256 is the copy's \([0-9.]+ s\)"
[ "$(gzips)" = 3 ] && ok "only the chunks that were not copied are (3 streams)" || bad "the streams of the mixed copy" "$(gzips)"
same "a copy begun with --verify end and gone on with --verify chunk" "$W/mixed.img"

# ---- the store changes while it is copied
printf '%s\n' "printf x >> '$SRC'" > "$W/grow.sh"
faults change 4; cp "$W/grow.sh" "$F/change.sh"
pull grow grow $C; status "the store grows during the copy" 1
said "the size that changed is said, and the copy is not kept" "data.img changed on the phone while it was copied \(size $SIZE -> $((SIZE + 1)), modified [0-9]+ -> [0-9]+\): the copy may mix two states of it and is not kept\. $W/grow.img.part stays; begin again with --restart"
kept "after the store grew" "$W/grow.img"
faults
pull grow2 grow $C; status "going on with a copy of a store that changed" 1
said "a copy of the old store is not gone on from" "the phone's data.img is not the one $W/grow.img.part was begun from \(then $SIZE bytes, modified [0-9]+; now $((SIZE + 1)) bytes, modified [0-9]+\): the chunks copied are of the old one\. Begin again: --restart"
make_store
printf '%s\n' "printf X | dd of='$SRC' bs=1 seek=5 conv=notrunc status=none" > "$W/poke.sh"
faults change 4; cp "$W/poke.sh" "$F/change.sh"
pull poke poke $C; status "a byte of a chunk that is done changes during the copy (the same size)" 1
said "the time that changed is said, and the copy is not kept" "data.img changed on the phone while it was copied \(size $SIZE -> $SIZE, modified [0-9]+ -> [0-9]+\)"
kept "after the store's content changed" "$W/poke.img"
! cmp -s "$W/poke.img.part" "$SRC" && ok "that .part is indeed not the store (every chunk had the phone's sum when it was copied)" || bad "the .part of the changed store is the store"
measure
faults
pull poke2 poke $C; status "going on after the content changed" 1
said "the store's time is not the copy's: refused" "is not the one $W/poke.img.part was begun from"
pull poke3 poke $C --restart; status "--restart" 0
said "--restart says what it throws away" "^--restart: what $W/poke.img.part and $W/poke.img.part.progress hold is thrown away$"
said "--restart copies every chunk again" "7 copied, 4 of zeros not copied, 0 retried, every chunk's sha256 is the phone's$"
same "--restart" "$W/poke.img"

# ---- a .part without a progress file, a progress file that is not one
head -c 3000000 /dev/urandom > "$W/old.img.part"
pull old old $C; status "a .part without a progress file (the first port's)" 0
said "it begins again and says so" "^$W/old.img.part has no progress file \($W/old.img.part.progress\): beginning again$"
same "a copy over a .part without a progress file" "$W/old.img"
truncate -s "$SIZE" "$W/odd.img.part"; echo '{"chunks": []}' > "$W/odd.img.part.progress"
pull odd odd $C; status "a progress file that is not this tool's" 1
said "it is refused, with --restart as the way" "$W/odd.img.part.progress is not a progress file this tool can go on from \(--restart begins again\): its first line is"
pull odd2 odd $C --restart; status "--restart over it" 0
same "--restart over a foreign progress file" "$W/odd.img"

# ---- no session may be open
"$TOOL" phone switch > "$W/switch.out" 2>&1; wait_for 5 has_session "$P" || bad "the stand-in session did not start" "$(tail -n 2 "$W/switch.out" | tr '\n' '|')"
pull session session $C; status "while a session is open" 1
said "a session is open: refused" "a session is open: return to One UI first"
[ ! -e "$W/session.img" ] && [ ! -e "$W/session.img.part" ] && [ ! -e "$W/session.img.part.progress" ] && ok "while a session is open: no file is made" || bad "while a session is open: a file was made"
"$TOOL" phone return > "$W/return.out" 2>&1; wait_for 10 no_session "$P" || end_session "$P"
end_case

# ---- nothing but the one named file is read
if cat "$W"/*.adb "$W/root.log" | grep -Eqi 'find |ls -R|du |/sys/power|reboot'; then bad "a tree walk, /sys/power or a restart was sent to the phone"
else ok "nothing sent to the phone walks a tree, writes /sys/power or restarts ($(cat "$W"/*.adb | wc -l) adb calls, their root scripts too)"; fi
release
none_held && ok "no stand-in busybox is left running" || bad "a stand-in busybox is left running"

# ---- the time (not a check of the tool: what the stand-in costs on this machine)
if [ -n "${SPEED:-}" ]; then
  mib=${SPEED%%:*}; data=${SPEED#*:}; [ "$data" != "$SPEED" ] || data=$((mib * 7 / 100))
  faults
  "$TOOL" debug mkstream phone "$mib" "$data" | "$TOOL" phone sparse-write "$SRC" > /dev/null 2>&1 || bad "speed: the store"
  SIZE=$(stat -c %s "$SRC"); measure
  echo "--- time: a store of $mib MiB with about $data MiB of data ($SRC_HELD bytes on disk); load:$(uptime | sed 's/.*load average[s]*://')"
  t0=$(date +%s.%N)
  "$ADB" exec-out "su -c '/data/adb/ksu/bin/busybox gzip -1 -c /data/local/tmp/oneios/data/data.img'" | "$TOOL" phone sparse-write "$W/speed0.img" --gunzip --expect-size "$SIZE" > "$W/speed0.out" 2>&1 \
    && cmp -s "$W/speed0.img" "$SRC" && ok "speed: the one stream of the first port: the store's bytes" || bad "speed: the one stream"
  echo "        one gzip -1 stream | sparse-write --gunzip (the first port):  $(echo "$(date +%s.%N) $t0 $SIZE" | awk '{ printf "%.1f s, %.0f MB/s", $1 - $2, $3 / 1e6 / ($1 - $2) }')"
  rm -f "$W/speed0.img"
  for how in "--verify chunk" "--verify end" "--verify chunk --chunk 64" "--verify chunk --chunk 1024"; do
    t0=$(date +%s.%N)
    pull speed speed $how
    [ "$st" = 0 ] && cmp -s "$W/speed.img" "$SRC" && ok "speed: pull-data $how: the store's bytes" || bad "speed: pull-data $how" "status $st: $(tail -n 2 "$W/speed.out" | cut -c1-300 | tr '\n' '|')"
    echo "        pull-data $how: $(echo "$(date +%s.%N) $t0 $SIZE" | awk '{ printf "%.1f s, %.0f MB/s", $1 - $2, $3 / 1e6 / ($1 - $2) }'); $(tail -n 1 "$W/speed.out" | sed 's/^[^:]*: //')"
    rm -f "$W/speed.img"
  done
fi

no_session_left "pull-resume"
[ -n "${KEEP:-}" ] || rm -rf "$W"
[ $FAILED = 0 ] && echo "ALL OK" || { echo "FAILED: $FAILED"; exit 1; }
