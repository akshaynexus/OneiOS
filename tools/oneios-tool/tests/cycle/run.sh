#!/bin/bash
# oneios-tool phone log, phone status --watch and cycle, against the stand-in phone and a stand-in OneiOS checkout
# whose build scripts are stand-ins: no phone, no kernel build. Needs tmux (scripts/bg.sh) and zstd.
#
#   tools/oneios-tool/tests/cycle/run.sh        one line per check, then `ALL OK` (exit 0) or `FAILED: N`
#
# phone log: --list, --file, --tail, the named filters, --grep, --since, --markers (a stage once), --save,
# --save-auto, --analyze; --follow on a log that grows, with adb dropping in the middle (no line lost or twice)
# and a Ctrl-C that ends it with status 0 and a whole saved file; --follow right after a switch waits out the last
# session's log (there until the switch replaces it, 40 s on the phone, 3 s here); a drop with the same file carries
# on after the last line, a drop that comes back to a new file (same first line, another inode) reads it from its
# start; status --watch.
# cycle: --dry-run (nothing started, no adb call); a whole cycle (kernel, prelink with the stubs check, runtime
# package, host tests, stage of only what differs, switch, markers, hold, return, the folder with summary.md); a
# second cycle that stages nothing; a panic, a boot that never gets ready, Ctrl-C in the hold with and without
# --stay; a failed collect step (summary.md's head names it, status 1); a split run (`--only stage,switch,watch`,
# then `--only return,collect` with the same name: the summary reads the markers from the saved xnuhost.log and
# says so); an audit failure and bad stubs stop it before the phone is touched; no switch without the return script,
# during an import, or when the preflight refuses; one phone lock. Every case ends the stand-in session it left
# (end_case), and at the end, a failure or Ctrl-C none is left running (lib.sh's traps).
# Env: TOOL, KEEP=1 (keep out/rustport-cycle), FAKE_PROC=host|stand-in (as tests/phone/run.sh).
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
TOOL=${TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
W=${WORK:-$ROOT/out/rustport-cycle}
[ -x "$TOOL" ] || { echo "run.sh: no $TOOL" >&2; exit 2; }
. "$HERE/../phone/lib.sh"
rm -rf "$W"; mkdir -p "$W"
export ADB=$HERE/../phone/fake-adb.sh FAKE_ADB_LOG=$W/adb.log FAKE_ROOT_LOG=$W/root.log
[ -n "${FAKE_PROC:-}" ] || { [ -r /proc/self/cmdline ] && FAKE_PROC=host || FAKE_PROC=stand-in; }
export FAKE_PROC; echo "(the stand-in's /proc: $FAKE_PROC)"
unset ANDROID_SERIAL FAKE_NO_ROOT FAKE_DEVICES ONEIOS_DSC_INDEX
FORK=$(fork_dir)
[ -n "$FORK" ] || { echo "run.sh: the cycle's checks need the OneOS fork's root script (ONEOS_DIR, or ../OneOS beside the main checkout)"; exit 2; }
export FAKE_FORK_ROOT_SH=$FORK/app-android/app/src/full/assets/root/oneios_root.sh ONEOS_DIR=$FORK
P=$W/phone; export FAKE_PHONE=$P; make_phone "$P"
S=$P/data/local/tmp/oneios
plog() { "$TOOL" phone log "$@"; }

# ---- phone log on a finished session's log
"$TOOL" phone switch > /dev/null 2>&1; wait_for 10 test -e "$S/session/ready"
"$TOOL" phone return > /dev/null 2>&1; wait_for 10 no_session "$P"
L=$S/session/xnuhost.log; n=$(wc -l < "$L")
check "log --list: the names, no folder listed" 0 "^xnuhost +[0-9]+ bytes  changed [0-9]+  session/xnuhost.log" plog --list
check "log --list: a log that is not there" 0 "^display +missing" plog --list
plog > "$W/all.txt" 2>/dev/null && cmp -s "$W/all.txt" "$L" && ok "log: the whole file ($n lines)" || bad "log: the whole file"
[ "$(plog --tail 3 2>/dev/null)" = "$(tail -n 3 "$L")" ] && ok "log --tail 3" || bad "log --tail"
check "log --file switch" 0 "=== OneOS app switch .* versionCode 115 ===" plog --file switch
check "log --file return" 0 "One UI restored in place" plog --file return
check "log --file of no known name" 1 "one of xnuhost, switch, return, outcome, display, watchdog or logs/xnuhost-<epoch>.log" plog --file ../../etc/passwd
m=$(plog --markers 2>/dev/null)
[ "$(printf '%s\n' "$m" | grep -c 'backboardd\[')" = 1 ] && [ "$(printf '%s\n' "$m" | wc -l)" = 11 ] && printf '%s\n' "$m" | tail -n 1 | grep -q "xnuhost I exit 0" \
  && ok "log --markers: nine stages once each, the stop and the exit" || bad "log --markers" "$(printf '%s' "$m" | cut -c1-60 | tr '\n' '|')"
for f in "gpu oneios-gpu-report 1" "hid oneios-hid 1" "wifi ONEIOS.WIFI 1" "display ONEIOS.BACKLIGHT 1" "panic stopping: 2"; do
  set -- $f; got=$(plog --$1 2>/dev/null)
  printf '%s\n' "$got" | grep -Eq "$2" && [ "$(printf '%s\n' "$got" | grep -Evc "oneios-gpu-report|$2")" -lt "$3" ] && ok "log --$1" || bad "log --$1" "$(printf '%s' "$got" | cut -c1-70 | tr '\n' '|')"
done
[ "$(plog --grep 'touch (down|up)' --gpu 2>/dev/null | grep -Evc 'oneios-gpu-report|oneios-hid')" = 0 ] && [ "$(plog --grep 'touch (down|up)' 2>/dev/null | wc -l)" = 1 ] && ok "log --grep, and with a named filter (any of them)" || bad "log --grep"
check "log --grep of a bad pattern" 1 "--grep \(: " plog --grep "("
t=$(sed -n '8p' "$L" | sed 's/^\[ *\([0-9.]*\)\].*/\1/')
[ "$(plog --since "$t" 2>/dev/null)" = "$(tail -n +8 "$L")" ] && ok "log --since $t: from that stamp on" || bad "log --since"
plog --tail 5 --save "$W/saved/tail.log" > /dev/null 2>&1 && [ "$(cat "$W/saved/tail.log")" = "$(tail -n 5 "$L")" ] && ok "log --save: what was read, its folder made" || bad "log --save"
# repo0 is a repository of its own: --analyze looks for the sym index in the main checkout of the folder's
# repository, which would otherwise be the checkout that holds out/ (the Mac's has an index: 2026-11-12)
(cd "$W" && mkdir -p repo0/scripts repo0/oneos && touch repo0/scripts/bg.sh repo0/oneos/interface.md && cd repo0 && git init -q . && plog --gpu --save-auto > /dev/null 2> "$W/auto.err")
auto=$(ls "$W"/repo0/out/session-logs/*/live-*.log 2>/dev/null | head -n 1)
[ -n "$auto" ] && cmp -s "$auto" "$L" && ok "log --save-auto: out/session-logs/DATE/live-HHMMSS.log, every line though --gpu printed few" || bad "log --save-auto" "$(cat "$W/auto.err")"
check "log --analyze: oneios-tool log timeline of what was read" 0 "^--- oneios-tool log timeline .*live-[0-9]+\.log" sh -c "cd '$W/repo0' && '$TOOL' phone log --analyze 2>&1"
check "log --analyze without a sym index says so" 0 "no sym index" sh -c "cd '$W/repo0' && '$TOOL' phone log --analyze 2>&1"
check "log: no root" 1 "no root on the phone" env FAKE_NO_ROOT=1 "$TOOL" phone log
end_case

# ---- --follow right after a switch: the last session's log is still there (the phone's switch replaces it ~40 s in),
# then adb drops once this session's log is being read (the phone, 2026-11-12: the last session's log was printed,
# and after the drop the whole current one a second time)
sleep 1.1   # the old log's last change is a second before the switch, as on the phone
cp "$L" "$W/old.log"; echo 3 > "$P/.fake/log-delay"; rm -f "$S/session/ready"   # the last session's ready is no answer
"$TOOL" phone switch > /dev/null 2>&1
FAKE_CUT=$W/cut "$TOOL" phone log --follow --save "$W/sw.log" > "$W/sw.out" 2> "$W/sw.err" & fpid=$!
# this session's ready (the file), not the last session's "guest ready" line
wait_for 30 test -e "$S/session/ready"; sleep 1; touch "$W/cut"; wait_for 10 grep -q "adb dropped" "$W/sw.err"; sleep 1.5
kill -INT $fpid; wait $fpid
grep -q "it is the last session's log; waiting for this session's" "$W/sw.err" && ok "log --follow right after a switch: says the log there is the last session's and waits" || bad "log --follow after a switch: no wait" "$(tr '\n' '|' < "$W/sw.err")"
k=$(wc -l < "$W/sw.out")
! grep -q "xnuhost I exit 0" "$W/sw.out" && [ "$k" -ge 13 ] && cmp -s "$W/sw.out" <(head -n "$k" "$L") \
  && ok "log --follow right after a switch: none of the last session's lines, this session's $k from its first" || bad "log --follow after a switch: the lines" "$k lines: $(head -n 2 "$W/sw.out" | tr '\n' '|')"
grep -q "adb dropped" "$W/sw.err" && ! grep -q "new file" "$W/sw.err" && [ "$(sort "$W/sw.out" | uniq -d | wc -l)" = 0 ] \
  && ok "log --follow: a drop in this session's log carries on after the last line read (no line twice)" || bad "log --follow: the drop after a switch" "$(sort "$W/sw.out" | uniq -d | wc -l) lines twice: $(tr '\n' '|' < "$W/sw.err")"
rm -f "$P/.fake/log-delay"
"$TOOL" phone return > /dev/null 2>&1; wait_for 10 no_session "$P"
end_case

# ---- --follow: a growing log, adb dropping, Ctrl-C
echo 0.3 > "$P/.fake/step"
"$TOOL" phone switch > /dev/null 2>&1; wait_for 10 has_session "$P"
touch "$W/drop"
FAKE_DROP=$W/drop "$TOOL" phone log --follow --save "$W/follow.log" > "$W/follow.out" 2> "$W/follow.err" &
fpid=$!
wait_for 20 grep -q "guest ready: first frame" "$W/follow.out" && ok "log --follow: the lines come as the log grows, in a session" || bad "log --follow: no ready line" "$(tail -n 2 "$W/follow.err" | tr '\n' '|')"
grep -q "adb dropped after [0-9]* lines; connecting again" "$W/follow.err" && [ ! -e "$W/drop" ] && ! grep -Eq "new file|was replaced" "$W/follow.err" && ok "log --follow: adb dropped once and the follow connected again to the same file" || bad "log --follow: the drop" "$(cat "$W/follow.err" | tr '\n' '|')"
sleep 1; kill -INT $fpid; wait $fpid; st=$?
[ $st = 0 ] && grep -q "stopped after [0-9]* lines" "$W/follow.err" && ok "log --follow: Ctrl-C ends it cleanly (status 0)" || bad "log --follow: Ctrl-C" "status $st"
k=$(wc -l < "$W/follow.log")
[ "$k" -ge 13 ] && cmp -s "$W/follow.log" <(head -n "$k" "$L") && cmp -s "$W/follow.out" "$W/follow.log" && ok "log --follow: the $k saved lines are the phone's first $k, none lost or twice across the drop" || bad "log --follow: the saved lines" "$k lines"
"$TOOL" phone log --follow --tail 2 --markers > "$W/follow2.out" 2>/dev/null & fpid=$!
sleep 1.5; kill -INT $fpid; wait $fpid
[ "$(wc -l < "$W/follow2.out")" -le 3 ] && ! grep -Evq 'SpringBoard\[|backboardd\[|guest ready' "$W/follow2.out" && ok "log --follow --tail 2 --markers: starts near the end, prints markers only" || bad "log --follow --tail" "$(cut -c1-60 "$W/follow2.out" | tr '\n' '|')"
"$TOOL" phone status --watch 1 > "$W/watch.out" 2>&1 & wpid=$!
sleep 3.5; kill -INT $wpid; wait $wpid; st=$?
[ $st = 0 ] && [ "$(grep -c '^--- 20' "$W/watch.out")" -ge 2 ] && grep -q "^session: open" "$W/watch.out" && ok "status --watch 1: again every second until Ctrl-C (status 0)" || bad "status --watch" "status $st, $(grep -c '^--- ' "$W/watch.out") looks"
# a drop that comes back to a new file: the old one moved away, a new one begun with the same first line
touch "$W/drop2"
FAKE_DROP=$W/drop2 FAKE_DROP_NEW=1 "$TOOL" phone log --follow --save "$W/new.log" > "$W/new.out" 2> "$W/new.err" & fpid=$!
wait_for 20 grep -q "is a new file" "$W/new.err"; wait_for 10 sh -c '[ "$(wc -l < "$1")" -ge 4 ]' sh "$L"; sleep 1; kill -INT $fpid; wait $fpid
python3 -I - "$W/new.log" "$S/logs/xnuhost-dropped.log" "$L" <<'PY2' && ok "log --follow: a drop back to a new file reads it from its start (the old file's lines, then the new one's, none twice)" || bad "log --follow: the drop to a new file" "$(tr '\n' '|' < "$W/new.err")"
import sys
saved, old, new = (open(f).read().splitlines() for f in sys.argv[1:])
a = 0
while a < min(len(saved), len(old)) and saved[a] == old[a]: a += 1
rest = saved[a:]
# what came before the drop is the old file's head; what came after is the new file's head, from its first line
assert a >= 10 and len(rest) >= 3 and rest == new[:len(rest)], (a, len(rest), rest[:2], new[:2])
PY2
"$TOOL" phone return > /dev/null 2>&1; wait_for 10 no_session "$P"
echo 0.1 > "$P/.fake/step"
end_case

# ---- a stand-in OneiOS checkout whose builds are stand-ins
R=$W/repo; mkdir -p "$R/scripts" "$R/oneos" "$R/kernel/prelink" "$R/kernel/patches/oneios-hosted" "$R/runtime" "$R/out/image/24A446/overlay/userland"
cp "$ROOT/scripts/bg.sh" "$R/scripts/bg.sh"; echo stand-in > "$R/oneos/interface.md"; echo "a patch" > "$R/kernel/patches/oneios-hosted/0001.patch"
cat > "$R/kernel/build-hosted.sh" <<'B'
#!/bin/bash
d=out/kernel/current-hosted-$HOSTED_OUT; mkdir -p "$d"
echo "hosted kernel, reference=${XNU_FILESET_REFERENCE:-0}, build $(cat "$d/n" 2>/dev/null || echo 0)" > "$d/xnu.macho"; echo reference > "$d/reference.macho"
echo "build-hosted: $d/xnu.macho"
B
cat > "$R/kernel/prelink/build.sh" <<'B'
#!/bin/bash
[ ! -e audit-fails ] || { echo "audit: 3 failures"; echo "prelink: the audit failed" >&2; exit 1; }
mkdir -p out/kernel/24A446
# a made-up collection whose stubs `prelink stubs-check` reads: 666 good ones, or 6 of them bad
if [ -e stubs-bad ]; then good=660 bad=6; else good=666 bad=0; fi
"$(cat tool-path)" debug stub-kc out/kernel/24A446/kernel.kc $good $bad "collection of $(cat "$KERNEL") with $(basename "$REFERENCE")"; echo hashes > out/kernel/24A446/kernel.kc.hashes
echo "audit: 2711 paired, 0 failures"; echo "prelink: out/kernel/24A446/kernel.kc"
B
echo "$TOOL" > "$R/tool-path"
cat > "$R/runtime/package.sh" <<'B'
#!/bin/bash
d=$(mktemp -d); mkdir -p "$d/bin" out/release
echo "xnuhost $1" > "$d/bin/xnuhost"
python3 -I - "$d" "$1" <<'PY'
import hashlib, json, os, sys
d, v = sys.argv[1:3]
b = open(f"{d}/bin/xnuhost", "rb").read()
json.dump({"format": 1, "name": "oneios-runtime", "version": v, "arch": "arm64", "session_abi": 1, "image_formats": [1], "xnuhost": "bin/xnuhost",
           "files": [{"path": "bin/xnuhost", "size": len(b), "sha256": hashlib.sha256(b).hexdigest(), "mode": "0755"}]}, open(f"{d}/runtime.json", "w"))
PY
tar -C "$d" -cf - runtime.json bin | zstd -q -f -o "out/release/oneios-runtime-$1-arm64.tar.zst"; rm -rf "$d"
echo "package: out/release/oneios-runtime-$1-arm64.tar.zst"
B
printf '#!/bin/bash\n[ ! -e tests-fail ] || { echo "loader FAIL"; exit 1; }\necho "all pass"\n' > "$R/runtime/host-tests.sh"
chmod +x "$R"/kernel/*.sh "$R"/kernel/prelink/build.sh "$R"/runtime/*.sh
cp "$S/image/manifest.json" "$R/out/image/24A446/manifest.json"; cp "$S/image/overlay/userland/xnuhost.conf" "$R/out/image/24A446/overlay/userland/xnuhost.conf"
(cd "$R" && git init -q . && git add -A && git -c user.name=t -c user.email=t@example.invalid commit -q -m stand-in)
export ONEIOS_ROOT=$R
cycle() { (cd "$R" && "$TOOL" cycle "$@"); }
D=$R/out/session-logs/$(date +%F)
calls() { grep -c . "$W/adb.log"; }

# ---- dry run
before=$(calls)
out=$(cycle --dry-run --kernel-out t --runtime-version 0.1.7 --tests --name dry 2>&1); st=$?
[ $st = 0 ] && [ "$(calls)" = "$before" ] && [ ! -e "$R/logs" ] && [ ! -e "$D/dry" ] && ok "cycle --dry-run: status 0, no adb call, no job, no folder" || bad "cycle --dry-run touched something" "status $st"
for want in "scripts/bg.sh cycle-kernel env HOSTED_OUT=t XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh" "scripts/bg.sh cycle-prelink env KERNEL=.*/xnu.macho REFERENCE=.*/reference.macho kernel/prelink/build.sh" "scripts/bg.sh cycle-runtime runtime/package.sh 0.1.7" "scripts/bg.sh cycle-tests runtime/host-tests.sh" "phone sums; stage image \(only the files whose sha256 differs\)" "check the way back" "write session/return.request"; do
  printf '%s\n' "$out" | grep -Eq "^would: $want" && ok "cycle --dry-run: $(printf '%s' "$want" | cut -c1-60)" || bad "cycle --dry-run: $want" "$(printf '%s' "$out" | tr '\n' '|' | cut -c1-300)"
done
check "cycle --only with a word that is no step" 1 "boot is not a step \(build, tests, stage, switch, watch, hold, return, collect\)" cycle --only boot
check "cycle --dry-run --only watch,return" 0 "^cycle c1: watch, return \(dry run" cycle --dry-run --only watch,return --name c1

# ---- the whole cycle
out=$(cycle --kernel-out t --runtime-version 0.1.7 --tests --hold 1 --name full 2>&1); st=$?
[ $st = 0 ] && ok "cycle: the whole cycle ends with status 0" || bad "cycle: the whole cycle" "status $st: $(printf '%s' "$out" | tail -n 4 | tr '\n' '|')"
for want in "cycle-kernel: exit 0" "cycle-prelink: exit 0" "stubs: 666 good, 0 bad" "cycle-runtime: exit 0" "cycle-tests: exit 0" "against the phone's sums: kernel.kc, kernel.kc.hashes differ; the runtime differs" "the phone's sums are the local files'" "switch started \(versionCode 115\)" "kernel +\[" "launchd +\[" "backboardd +\[" "SpringBoard +\[" "ready +\[" "ready after [0-9]+ s" "holding the session for 1 s" "One UI restored in place \(Enforcing\)\. \([0-9]+ s after the request\); return.outcome: inplace" "summary.md in"; do
  printf '%s\n' "$out" | grep -Eq "$want" && ok "cycle: $(printf '%s' "$want" | cut -c1-70)" || bad "cycle: $want" "$(printf '%s' "$out" | grep -E 'sums|stub|exit' | tr '\n' '|' | cut -c1-300)"
done
cmp -s "$R/out/kernel/24A446/kernel.kc" "$S/image/kernel.kc" && grep -q "^version=0.1.7" "$S/runtime.conf" && grep -q "reference=1" "$S/image/kernel.kc" && grep -q "reference.macho" "$S/image/kernel.kc" \
  && ok "cycle: the phone has the new collection (built with XNU_FILESET_REFERENCE=1 and the reference) and the new runtime" || bad "cycle: what is staged"
[ "$(cat "$S/image/data.seed")" = "seed stand-in" ] && ok "cycle: data.seed is untouched" || bad "cycle: data.seed"
no_session "$P" && ok "cycle: One UI is back, no session" || bad "cycle: a session is left"
for f in xnuhost.log switch.log return.log return.outcome sums.txt summary.md; do [ -s "$D/full/$f" ] || bad "cycle: $f is missing in out/session-logs/DATE/full"; done
cmp -s "$D/full/xnuhost.log" "$S/session/xnuhost.log" && ok "cycle: out/session-logs/DATE/full has the session's log, the fork's logs and sums.txt" || bad "cycle: the collected log"
for want in "^# Cycle full" "every step that ran ended well" "prelink: audit 0 failures \(build.sh exit 0\), stubs: 666 good, 0 bad" "app source: 0.4.86 \(versionCode 115\)" "app installed: 0.4.86 \(versionCode 115\)" "^\| stage \| [0-9.]+ \| ok \|" "^\| return \| [0-9.]+ \| ok \|" "^- \+[0-9]+ s ready: .*guest ready: first frame" "^## Panics" "^none$" "One UI restored in place \(Enforcing\)" "  image/kernel.kc$"; do
  grep -Eq "$want" "$D/full/summary.md" && ok "summary.md: $(printf '%s' "$want" | cut -c1-60)" || bad "summary.md: $want"
done
end_case

# ---- again: nothing changed, nothing built, nothing staged
before=$(grep -c "push" "$W/adb.log")
out=$(cycle --kernel-out t --hold 0 --name again 2>&1); st=$?
[ $st = 0 ] && printf '%s\n' "$out" | grep -q "kernel: up to date" && printf '%s\n' "$out" | grep -q "prelink: up to date" && printf '%s\n' "$out" | grep -q "no image file differs" && [ "$(grep -c push "$W/adb.log")" = "$before" ] \
  && ok "cycle again: kernel and collection up to date, no file differs, nothing pushed" || bad "cycle again" "status $st: $(printf '%s' "$out" | grep -E 'kernel|prelink|differ' | tr '\n' '|')"
touch -d '+1 minute' "$R/kernel/patches/oneios-hosted/0002.patch"
check "cycle: a newer patch rebuilds the kernel and the collection" 0 "cycle-kernel: exit 0" cycle --kernel-out t --only build --name patch
end_case

# ---- a boot that fails: the way back is still taken
touch "$P/.fake/panic"
out=$(cycle --skip build,tests,stage --hold 60 --name panic 2>&1); st=$?
[ $st = 1 ] && printf '%s\n' "$out" | grep -q "PANIC .*panic(cpu 0" && printf '%s\n' "$out" | grep -q "a panic: returning" && no_session "$P" && grep -q "^- \`.*panic(cpu 0 caller" "$D/panic/summary.md" && grep -q "stopped\*\*: the boot did not reach its first frame" "$D/panic/summary.md" \
  && ok "cycle: a panic returns at once (no hold), status 1, the panic and the way back in summary.md" || bad "cycle: a panic" "status $st: $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
rm "$P/.fake/panic"; end_case; touch "$P/.fake/hang"
out=$(cycle --only switch,watch,return,collect --ready-timeout 3 --name hang 2>&1); st=$?
[ $st = 1 ] && printf '%s\n' "$out" | grep -q "no first frame within 3 s: returning" && no_session "$P" && grep -q "One UI restored" "$D/hang/summary.md" \
  && ok "cycle: no first frame within --ready-timeout returns, status 1" || bad "cycle: --ready-timeout" "status $st: $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
rm "$P/.fake/hang"; end_case
# Ctrl-C in the hold
(cd "$R" && exec "$TOOL" cycle --skip build,tests,stage --name ctrlc > "$W/ctrlc.out" 2>&1) & cpid=$!
wait_for 30 grep -q "holding the session until Ctrl-C" "$W/ctrlc.out"; kill -INT $cpid; wait $cpid; st=$?
[ $st = 0 ] && grep -q "One UI restored" "$W/ctrlc.out" && no_session "$P" && grep -q "hold: Ctrl-C after" "$D/ctrlc/summary.md" && ok "cycle: Ctrl-C in the hold returns to One UI and collects (status 0)" || bad "cycle: Ctrl-C" "status $st: $(tail -n 3 "$W/ctrlc.out" | tr '\n' '|')"
end_case
(cd "$R" && exec "$TOOL" cycle --skip build,tests,stage --stay --name stay > "$W/stay.out" 2>&1) & cpid=$!
wait_for 30 grep -q "holding the session until Ctrl-C" "$W/stay.out"
check "cycle: a second cycle while one holds the phone is refused" 1 "the phone is taken: .*phone.lock is locked by another cycle \(pid [0-9]+ cycle stay\)" cycle --skip build,tests --name second
kill -INT $cpid; wait $cpid; st=$?
[ $st = 0 ] && has_session "$P" && grep -q "the session was left open (--stay)" "$D/stay/summary.md" && ok "cycle --stay: Ctrl-C leaves the session open and says how to end it" || bad "cycle --stay" "status $st: $(tail -n 3 "$W/stay.out" | tr '\n' '|')"
check "cycle: no switch into an open session" 1 "a session is open: return to One UI first" cycle --only switch --name open
out=$(cycle --only return,collect --name back 2>&1); st=$?
[ $st = 0 ] && no_session "$P" && grep -q "One UI restored" "$D/back/summary.md" && ok "cycle --only return,collect: ends the session that was left open" || bad "cycle --only return,collect" "status $st"
end_case

# ---- summary.md says what its Steps table says, and a run that did not watch says where its markers are from
# (the phone, 2026-11-13: "every step that ran ended well" above `collect | failed: ioslog boots ended with status 1`,
# and "Markers: none seen" in the summary of `--only return,collect` after `--only stage,switch,watch`)
# a failed collect step: the stand-in's data.img is no APFS store, so --pull-data's `ioslog boots` ends with status 1
out=$(cycle --skip build,tests,stage --hold 0 --pull-data --name failcollect 2>&1); st=$?
M=$D/failcollect/summary.md
[ $st = 1 ] && printf '%s\n' "$out" | grep -q "oneios-tool cycle: collect: ioslog boots ended with status 1" && no_session "$P" \
  && ok "cycle: a failed collect step ends the cycle with status 1 (after the return), and says which step" || bad "cycle: a failed collect step" "status $st: $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
grep -Eq "^\| collect \| [0-9.]+ \| failed: ioslog boots ended with status 1 \|" "$M" && grep -q "the result: \*\*failed step\*\*: collect (ioslog boots ended with status 1)$" "$M" && ! grep -q "ended well" "$M" \
  && ok "summary.md: the head names the step the Steps table has as failed (collect), not 'every step that ran ended well'" || bad "summary.md: the head of a failed collect" "$(grep -E 'the result|collect' "$M" | tr '\n' '|')"
grep -Eq "^\| watch \| [0-9.]+ \| ok \|" "$M" && grep -Eq "^\| return \| [0-9.]+ \| ok \|" "$M" && grep -Eq "^- \+[0-9]+ s ready: " "$M" && [ -s "$D/failcollect/xnuhost.log" ] && cmp -s "$D/failcollect/data.img" "$S/data/data.img" \
  && ok "summary.md: the steps before it are ok, the markers are the watch's, the logs and the store's copy are in the folder" || bad "summary.md: the rest of a failed collect" "$(grep -E '^\| ' "$M" | tr '\n' '|')"
grep -Eq "^\| watch \| [0-9.]+ \| failed: the boot did not reach its first frame \(watch: a panic before the first frame\) \|" "$D/panic/summary.md" && grep -q "; \*\*failed step\*\*: watch$" "$D/panic/summary.md" \
  && ok "summary.md of the panic: the watch step is failed in the table and named in the head" || bad "summary.md of the panic: the watch step" "$(grep -E 'the result|watch' "$D/panic/summary.md" | tr '\n' '|')"
end_case
# a split run: watch in one run, return and collect in the next, the same name
out=$(cycle --only stage,switch,watch --name split 2>&1); st=$?
[ $st = 0 ] && has_session "$P" && [ ! -e "$D/split/summary.md" ] && printf '%s\n' "$out" | grep -Eq "ready after [0-9]+ s" \
  && ok "cycle --only stage,switch,watch: the boot is watched, the session stays open, no summary yet" || bad "cycle --only stage,switch,watch" "status $st: $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
out=$(cycle --only return,collect --name split 2>&1); st=$?
M=$D/split/summary.md
[ $st = 0 ] && no_session "$P" && grep -q "the result: every step that ran ended well$" "$M" && grep -Eq "^\| return \| [0-9.]+ \| ok \|" "$M" && ! grep -q "^| watch " "$M" \
  && ok "cycle --only return,collect with the same name: returns and writes the summary (no watch step in its table)" || bad "cycle --only return,collect of a split run" "status $st: $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
grep -q "^The watch step did not run in this run (the boot was watched by an earlier run, or not at all): the markers were read from \`xnuhost.log\` in this folder (the collect step's copy of the phone's \`session/xnuhost.log\`)\.$" "$M" && ! grep -q "none seen" "$M" \
  && ok "summary.md of a run that did not watch: says so, and where the markers were read from" || bad "summary.md of a split run: where the markers are from" "$(grep -A3 '^## Markers' "$M" | tr '\n' '|')"
miss=""; for want in "^- kernel: " "^- launchd: " "^- backboardd: " "^- SpringBoard: " "^- ready: .*guest ready: first frame" "^- end: .*xnuhost I exit 0"; do grep -Eq "$want" "$M" || miss="$miss $want"; done
[ -z "$miss" ] && [ "$(grep -c '^- backboardd: ' "$M")" = 1 ] && [ "$(sed -n '/^## Markers/,/^## Panics/p' "$M" | grep -c '^- ')" = "$("$TOOL" phone log --markers 2>/dev/null | wc -l)" ] \
  && ok "summary.md of a split run: the markers of the saved xnuhost.log, each stage once (as phone log --markers has them)" || bad "summary.md of a split run: the markers" "missing:$miss; $(sed -n '/^## Markers/,/^## Panics/p' "$M" | cut -c1-50 | tr '\n' '|')"
end_case

# ---- what stops a cycle before the phone is touched, or before the switch
before=$(calls); touch "$R/audit-fails"
check "cycle: an audit failure stops the cycle" 1 "cycle-prelink ended with exit code 1" cycle --kernel-out t --force prelink --name audit
rm "$R/audit-fails"; touch "$R/stubs-bad"
check "cycle: stubs that are not all good stop the cycle" 1 "the collection's stubs are not all good: stubs: 660 good, 6 bad" cycle --kernel-out t --force prelink --name stubs
rm "$R/stubs-bad"; touch "$R/tests-fail"
check "cycle: failing host tests stop the cycle" 1 "cycle-tests ended with exit code 1: loader FAIL" cycle --tests --only tests,stage --name tests
rm "$R/tests-fail"
[ "$(calls)" = "$before" ] && ok "cycle: none of the three touched the phone" || bad "cycle: the phone was touched after a failed build or test"
mv "$S/oneios_return.sh" "$W/return.keep"
check "cycle: no switch without the return script" 1 "no switch: oneios_return.sh is not staged" cycle --only switch,watch --name noreturn
mv "$W/return.keep" "$S/oneios_return.sh"; mkdir "$S/image.new"
check "cycle: no switch while an import is in progress" 1 "no switch: an import of an image is in progress \(image.new\)" cycle --only switch --name import
rmdir "$S/image.new"; echo "the battery is at 4 percent (stand-in)" > "$P/.fake/preflight-refuses"
check "cycle: no switch when the preflight refuses" 1 "no switch: the preflight refuses: the battery is at 4 percent" cycle --only switch --name preflight
rm "$P/.fake/preflight-refuses"
check "cycle: no root" 1 "no root on the phone" env FAKE_NO_ROOT=1 ONEIOS_ROOT="$R" "$TOOL" cycle --only stage --name noroot
no_session "$P" && ok "none of the refused switches started a session" || bad "a refused switch started a session"
if grep -Eqi 'find |ls -R|du |/sys/power|reboot' "$W/adb.log" "$W/root.log"; then bad "a tree walk, /sys/power or a restart was sent to the phone" "$(grep -Ehi 'find |ls -R|du |/sys/power|reboot' "$W/adb.log" "$W/root.log" | head -n 2)"
else ok "nothing sent to the phone walks a tree, writes /sys/power or restarts ($(calls) adb calls, their root scripts too)"; fi

no_session_left "cycle suite"; tmux kill-session -t oneios-cycle-kernel 2>/dev/null
[ -n "${KEEP:-}" ] || rm -rf "$W"
[ $FAILED = 0 ] && echo "ALL OK" || { echo "FAILED: $FAILED"; exit 1; }
