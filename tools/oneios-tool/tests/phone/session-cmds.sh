# Sourced by tests/phone/run.sh (with lib.sh's helpers, HERE, W and TOOL set): the session's own commands,
# `phone display-restart`, `phone touch status|wake` and `phone backlight status|set`, against a stand-in phone of
# its own ($W/sessphone) that has what they touch:
#   - a display helper in the staged runtime (runtime/bin/oneios-display: a copy of fake-display.sh, with a library
#     and a dex beside it and their lines in runtime.sha256 and runtime.ids), which the stand-in session starts as
#     the fork's switch does and the stand-in return ends by control/display.pid (fake-session.sh);
#   - the stand-in's sysfs (fake-rewrite.sh maps /sys/... to it): sys/class/sec/tsp/enabled with the touch IC's
#     driver (.fake/drivers, run after each root script: the node reads 1 after `2,1` was written, 0 after `1,1`;
#     with .fake/tsp-dead it stays 0), sys/class/backlight/panel0-backlight/brightness and max_brightness, and the
#     session's cgroup file sys/fs/cgroup/oneios_session/cgroup.procs;
#   - readlink and nsenter stand-ins: /proc/PID/ns/mnt reads the same for the shell and for init (with
#     .fake/other-ns: not the same), and `nsenter -t 1 -m -- CMD` runs CMD and is noted in .fake/nsenter.log.
# Checked: every command refused without a session (and root asked for first); a restart that works (the old helper
# ended by SIGTERM, the new one started with the switch's arguments, pid file, log and cgroup, only what the log
# gained printed, xnuhost untouched), through nsenter when the shell is not in init's mount namespace, with a
# readiness line that comes in two pieces, with an old helper that takes a SIGKILL, with a pid file that names
# another process; a helper that never gets ready (the timeout) and one that ends at once (the 2026-11-14 library
# fault); the refusals while the switch is still starting and while a return is under way; --lib and --dex (the
# copy, the sums on both sides, the mode, runtime.sha256 and runtime.ids), a copy that is not the file sent, a file
# of the wrong kind, a file the staged runtime does not have; touch status and wake, an IC that stays off, a
# missing node; backlight status and set, the range; a return during a restart; the return afterwards, which puts
# One UI's level back; no helper left running; only the named /sys files in what was sent.
session_cmd_checks() {
  local P=$W/sessphone before=${FAKE_PHONE:-}
  local S=$P/data/local/tmp/oneios C=$P/data/local/tmp/oneios/control
  local TSP=$P/sys/class/sec/tsp/enabled BL=$P/sys/class/backlight/panel0-backlight CG=$P/sys/fs/cgroup/oneios_session/cgroup.procs
  local c out st old new xp pid sums f want
  export FAKE_PHONE=$P
  make_phone "$P"
  sphone() { "$TOOL" phone "$@"; }
  # an arm64 shared object's first bytes (\177ELF, 64-bit, e_type 3, e_machine 0xb7), then a line that tells two apart
  elf() { printf '\177ELF\002\001\001\000\000\000\000\000\000\000\000\000\003\000\267\000%s\n' "$1"; }
  dex() { printf 'dex\n035\000%s\n' "$1"; }
  helper_pid() { cat "$C/display.pid" 2>/dev/null; }
  alive() { kill -0 "$1" 2>/dev/null; }
  gone() { ! kill -0 "$1" 2>/dev/null; }
  # ps, not the pid file: a helper whose pid file names another is found too (its command: bash <the helper> ...)
  helpers_left() { ps -A -o pid= -o command= 2>/dev/null | awk -v d="$S/runtime/bin/oneios-display" '$3 == d { print $1 }'; }
  frames() { grep -c "first frame posted (" "$C/display.log" 2>/dev/null; }
  frame_after() { [ "$(frames)" -gt "$1" ]; }
  # a new session whose helper has its first frame up (the last session's ready file and pid file are not this one's)
  session_up() {
    local n; n=$(frames)
    rm -f "$S/session/ready" "$C/display.pid"
    "$TOOL" phone switch > /dev/null 2>&1
    wait_for 10 test -e "$S/session/ready" && wait_for 5 test -s "$C/display.pid" && wait_for 5 frame_after "${n:-0}"
  }

  mkdir -p "$P/sys/class/sec/tsp" "$BL" "$(dirname "$CG")" "$S/runtime/lib"
  echo 1 > "$TSP"; echo 3200 > "$BL/brightness"; echo 12750 > "$BL/max_brightness"; : > "$CG"
  cp "$HERE/fake-display.sh" "$S/runtime/bin/oneios-display"; chmod 755 "$S/runtime/bin/oneios-display"
  elf "library 1" > "$S/runtime/bin/liboneios-display.so"; chmod 755 "$S/runtime/bin/liboneios-display.so"
  dex "dex 1" > "$S/runtime/lib/oneios-display.dex"; chmod 644 "$S/runtime/lib/oneios-display.dex"
  (cd "$S/runtime" && : > ../runtime.sha256 && for f in bin/xnuhost bin/oneios-display bin/liboneios-display.so lib/oneios-display.dex; do
    sha256sum "$f" >> ../runtime.sha256; echo "$(stat -c '%d:%i:%s:%Y' "$f") $f" >> ../runtime.ids; done)
  cat > "$P/.fake/drivers" <<'SH'
#!/bin/sh
# the touch IC's driver: what tsp/enabled reads after what the last root script wrote to it
n=$FAKE_PHONE/sys/class/sec/tsp/enabled
case "$(cat "$n" 2>/dev/null)" in
2,1) if [ -e "$FAKE_PHONE/.fake/tsp-dead" ]; then echo 0 > "$n"; else echo 1 > "$n"; fi ;;
1,1) echo 0 > "$n" ;;
esac
SH
  cat > "$P/.fake/bin/readlink" <<'SH'
#!/bin/sh
# /proc/PID/ns/mnt as the phone answers it: one mount namespace (with .fake/other-ns the shell is not in init's).
# Everything else is the host's readlink.
case "$*" in
*/proc/1/ns/mnt) echo "mnt:[4026531841]"; exit 0 ;;
*/ns/mnt) if [ -e "$FAKE_PHONE/.fake/other-ns" ]; then echo "mnt:[4026533333]"; else echo "mnt:[4026531841]"; fi; exit 0 ;;
esac
IFS=:
for d in $PATH; do
  [ "$d" = "$FAKE_PHONE/.fake/bin" ] || [ ! -x "$d/readlink" ] || exec "$d/readlink" "$@"
done
echo "readlink: only the stand-in's is on PATH" >&2; exit 127
SH
  cat > "$P/.fake/bin/nsenter" <<'SH'
#!/bin/sh
# nsenter -t 1 -m -- CMD...: noted, then CMD (the stand-in has one mount namespace)
echo "$*" >> "$FAKE_PHONE/.fake/nsenter.log"
while [ $# -gt 0 ] && [ "$1" != -- ]; do shift; done
shift; exec "$@"
SH
  chmod +x "$P/.fake/drivers" "$P/.fake/bin/readlink" "$P/.fake/bin/nsenter"

  # ---- refused without a session: these are One UI's then
  for c in "display-restart" "touch status" "touch wake" "backlight status" "backlight set 5350"; do
    check "no session: $c is refused" 1 "no session is open: .* is One UI's while no session runs.*Nothing was changed" sphone $c
  done
  echo $$ > "$S/session/xnuhost.pid"   # a live process that is not an xnuhost
  check "a pid that is not an xnuhost's is no session: touch wake is refused" 1 "no session is open" sphone touch wake
  rm -f "$S/session/xnuhost.pid"
  [ "$(cat "$TSP")" = 1 ] && [ "$(cat "$BL/brightness")" = 3200 ] && [ ! -e "$C/display.pid" ] && [ ! -e "$P/.fake/display-starts" ] \
    && ok "no session: nothing was written and no helper started" || bad "no session: something was written or started"
  for c in "display-restart" "touch wake" "backlight set 5350"; do
    check "no root: $c says so" 1 "no root on the phone FAKE0001.*Nothing was changed" env FAKE_NO_ROOT=1 "$TOOL" phone $c
  done

  # ---- a restart that works
  session_up && ok "a session with a display helper (started as the switch starts it)" || bad "the stand-in session's helper" "$(tail -n 2 "$C/display.log" 2>/dev/null | tr '\n' '|')"
  old=$(helper_pid); xp=$(session_pid "$P")
  out=$(sphone display-restart 2>&1); st=$?; new=$(helper_pid)
  [ $st = 0 ] && ok "display-restart: status 0" || bad "display-restart" "status $st: $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
  for want in "old helper: pid $old ended on SIGTERM" "new helper: pid $new \(control/display.pid\), started as the switch does, in init's mount namespace" \
      "  oneios-display: layer up" "  oneios-display: first frame posted \(1440x3120 buffer" "display helper: pid $new runs, first frame posted [0-9.]+ s after the start" \
      "session: open \(xnuhost was not touched"; do
    printf '%s\n' "$out" | grep -Eq "^$want" && ok "display-restart: $want" || bad "display-restart: $want" "$(printf '%s' "$out" | tr '\n' '|')"
  done
  [ -n "$new" ] && [ "$new" != "$old" ] && gone "$old" && alive "$new" && ok "display-restart: the old helper is gone, the new one runs and control/display.pid names it" || bad "display-restart: the helpers" "old $old new $new"
  grep -q "oneios-display: asked to stop after" "$C/display.log" && [ "$(grep -c 'oneios-display: layer removed' "$C/display.log")" = 1 ] \
    && ok "display-restart: the old helper left on SIGTERM (asked to stop, layer removed)" || bad "display-restart: the old helper's end" "$(tail -n 12 "$C/display.log" | tr '\n' '|')"
  [ "$(printf '%s\n' "$out" | grep -c 'first frame posted (')" = 1 ] && [ "$(grep -c 'first frame posted (' "$C/display.log")" = 2 ] && ! printf '%s\n' "$out" | grep -q "asked to stop" \
    && ok "display-restart: only what the new helper wrote is printed (the log keeps both helpers' lines)" || bad "display-restart: the lines printed"
  [ "$(tail -n 1 "$P/.fake/display-starts")" = "$new --session $S/session --takeover --wait 60" ] \
    && ok "display-restart: the switch's arguments (--session <stage>/session --takeover --wait 60)" || bad "display-restart: the arguments" "$(tail -n 1 "$P/.fake/display-starts")"
  [ "$(cat "$CG")" = "$new" ] && ok "display-restart: the new helper is put in the session's cgroup (oneios_session/cgroup.procs)" || bad "display-restart: the cgroup" "$(cat "$CG")"
  [ "$(session_pid "$P")" = "$xp" ] && alive "$xp" && ok "display-restart: xnuhost is the same process, still running" || bad "display-restart: xnuhost"
  [ ! -e "$P/.fake/nsenter.log" ] && ok "display-restart: no nsenter when the shell is in init's mount namespace" || bad "display-restart: nsenter was used"

  # ---- a shell in another mount namespace: through nsenter, as the switch re-enters init's
  touch "$P/.fake/other-ns"
  check "display-restart from another mount namespace: through nsenter into init's" 0 "started as the switch does, through nsenter into init's mount namespace" sphone display-restart
  grep -q "^-t 1 -m -- .*busybox setsid .*runtime/bin/oneios-display --session " "$P/.fake/nsenter.log" 2>/dev/null \
    && ok "display-restart: nsenter -t 1 -m -- busybox setsid <the helper>" || bad "display-restart: the nsenter call" "$(cat "$P/.fake/nsenter.log" 2>/dev/null)"
  rm -f "$P/.fake/other-ns"

  # ---- a readiness line that is still being written when the log is read
  touch "$P/.fake/display-slow"
  out=$(sphone display-restart 2>&1); st=$?
  [ $st = 0 ] && [ "$(printf '%s\n' "$out" | grep -c "^  oneios-display: first frame posted (1440x3120 buffer, stride 1472, .*bgrx8888)$")" = 1 ] && [ "$(printf '%s\n' "$out" | grep -c 'first frame posted (')" = 1 ] \
    && ok "display-restart: a line written in two pieces is printed once, whole" || bad "display-restart: a line in two pieces" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  rm -f "$P/.fake/display-slow"

  # ---- an old helper that does not leave on SIGTERM
  touch "$P/.fake/display-deaf"; sphone display-restart > /dev/null 2>&1; rm -f "$P/.fake/display-deaf"
  old=$(helper_pid)
  check "display-restart: a helper that ignores SIGTERM gets SIGKILL after --kill-after" 0 "old helper: pid $old was still there 1 s after SIGTERM: SIGKILL" sphone display-restart --kill-after 1
  wait_for 3 gone "$old" && alive "$(helper_pid)" && ok "display-restart: the killed helper is gone and the new one runs" || bad "display-restart: after the SIGKILL"

  # ---- a helper that does not come up
  touch "$P/.fake/display-hang"
  old=$(helper_pid)
  out=$(sphone display-restart --timeout 2 2>&1); st=$?; new=$(helper_pid)
  [ $st = 1 ] && printf '%s\n' "$out" | grep -Eq "no \`oneios-display: first frame posted\` in control/display.log 2 s after the start; the helper \(pid $new\) still runs and is left running.*its last lines: .*panel modes" \
    && ok "display-restart: a helper that never gets ready ends at --timeout, with the log's lines" || bad "display-restart: the timeout" "status $st: $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
  alive "$new" && has_session "$P" && alive "$xp" && ok "display-restart: after the timeout the helper is left running and the session goes on" || bad "display-restart: after the timeout"
  rm -f "$P/.fake/display-hang"
  touch "$P/.fake/display-broken"
  out=$(sphone display-restart 2>&1); st=$?; new=$(helper_pid)
  [ $st = 1 ] && printf '%s\n' "$out" | grep -Eq "the new display helper \(pid $new\) ended before its first frame; its last lines: .*cannot locate symbol \"ANativeWindow_getBuffersDataSpace\"" \
    && printf '%s\n' "$out" | grep -q "^  java.lang.UnsatisfiedLinkError: dlopen failed" \
    && ok "display-restart: a helper that ends at once (the library fault of 2026-11-14) is said, with its lines" || bad "display-restart: a helper that ends" "status $st: $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
  has_session "$P" && alive "$xp" && ok "display-restart: the session goes on without a helper" || bad "display-restart: the session after a helper that ended"
  rm -f "$P/.fake/display-broken"
  check "display-restart: a pid file that names a process that is gone" 0 "control/display.pid named pid $new, which is not a display helper .*left alone" sphone display-restart
  # a pid used again by another process: never signalled
  old=$(helper_pid); sleep 300 & pid=$!
  echo "$pid" > "$C/display.pid"
  check "display-restart: a pid file that names another process" 0 "control/display.pid named pid $pid, which is not a display helper .*left alone" sphone display-restart
  alive "$pid" && ok "display-restart: the other process was not signalled" || bad "display-restart: the other process was signalled"
  kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
  # the helper the pid file no longer named: the real one leaves when the new one connects; here it is ended by hand
  kill "$old" 2>/dev/null; wait_for 3 gone "$old"

  # ---- not while the switch is still starting, not while a return is under way
  pid=$(helper_pid)
  echo starting > "$C/state"
  check "display-restart while the switch is still starting: refused" 1 "the switch is still starting the session .*Nothing was changed" sphone display-restart
  echo returning > "$C/state"
  check "display-restart while the session returns (control/state): refused" 1 "the session is returning to One UI \(control/state: returning\).*Nothing was changed" sphone display-restart
  echo running > "$C/state"
  touch "$P/.fake/no-return"; echo "session-cmds: asked" > "$S/session/return.request"
  check "display-restart after a return was asked: refused" 1 "the session is returning to One UI .*session/return.request is there.*Nothing was changed" sphone display-restart
  [ "$(helper_pid)" = "$pid" ] && alive "$pid" && ok "display-restart refused: the helper was not touched" || bad "display-restart refused: the helper"
  rm -f "$P/.fake/no-return"; wait_for 10 no_session "$P" || end_session "$P"
  wait_for 5 gone "$pid" && ok "the return ended the helper by control/display.pid" || bad "the return left the helper running"
  end_case

  # ---- --lib and --dex
  session_up || bad "a second session"
  xp=$(session_pid "$P")
  elf "library 2" > "$W/new.so"; dex "dex 2" > "$W/new.dex"; echo "not a library" > "$W/notes.txt"
  pid=$(helper_pid); sums=$(cat "$S/runtime.sha256")
  cp "$FAKE_ADB_LOG" "$W/adb.before"
  check "--lib of a file that is not an arm64 shared object: refused" 1 "notes.txt is not an arm64 shared object .*nothing was sent to the phone" sphone display-restart --lib "$W/notes.txt"
  check "--dex of a library: refused" 1 "new.so is not a dex .*nothing was sent to the phone" sphone display-restart --dex "$W/new.so"
  check "--lib of a file that is not there" 1 "--lib $W/none.so" sphone display-restart --lib "$W/none.so"
  [ -z "$(tail -n +$(($(wc -l < "$W/adb.before") + 1)) "$FAKE_ADB_LOG" | grep -vx devices)" ] && ok "a wrong local file: nothing was asked of the phone (adb devices only)" || bad "a wrong local file: the phone was asked" "$(tail -n 2 "$FAKE_ADB_LOG" | tr '\n' '|')"
  out=$(FAKE_PUSH_CORRUPT=1 "$TOOL" phone display-restart --lib "$W/new.so" 2>&1); st=$?
  [ $st = 1 ] && printf '%s\n' "$out" | grep -q "^--lib: local  $(sha256sum < "$W/new.so" | cut -d' ' -f1)  $W/new.so" \
    && printf '%s\n' "$out" | grep -Eq "^--lib: phone  [0-9a-f]{64}  /data/local/tmp/oneios-display-tmp/liboneios-display.so \(the copy adb pushed\)" \
    && ! printf '%s\n' "$out" | grep -q "^--lib: phone  $(sha256sum < "$W/new.so" | cut -d' ' -f1)" \
    && printf '%s\n' "$out" | grep -q "the copy on the phone does not match. The staged runtime/bin/liboneios-display.so is unchanged and the display helper was not touched" \
    && ok "--lib: a copy on the phone that is not the file sent is refused, with both sums" || bad "--lib: a copy that does not match" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  [ "$(cat "$S/runtime.sha256")" = "$sums" ] && grep -q "library 1" "$S/runtime/bin/liboneios-display.so" && [ "$(helper_pid)" = "$pid" ] && alive "$pid" \
    && [ ! -e "$S/runtime/bin/liboneios-display.so.part" ] && [ ! -e "$P/data/local/tmp/oneios-display-tmp" ] \
    && ok "--lib refused: the staged library, runtime.sha256 and the running helper are as before; nothing left behind" || bad "--lib refused: something changed"
  mv "$S/runtime/lib/oneios-display.dex" "$W/dex.aside"
  check "--dex when the staged runtime has no dex: refused" 1 "runtime/lib/oneios-display.dex is not in the staged runtime .*the display helper was not touched" sphone display-restart --dex "$W/new.dex"
  mv "$W/dex.aside" "$S/runtime/lib/oneios-display.dex"
  [ "$(helper_pid)" = "$pid" ] && alive "$pid" && ok "--dex refused: the running helper was not touched" || bad "--dex refused: the helper"
  out=$(sphone display-restart --lib "$W/new.so" --dex "$W/new.dex" 2>&1); st=$?; new=$(helper_pid)
  [ $st = 0 ] && ok "display-restart --lib --dex: status 0" || bad "display-restart --lib --dex" "status $st: $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
  for f in "--lib new.so bin/liboneios-display.so 755" "--dex new.dex lib/oneios-display.dex 644"; do
    set -- $f; want=$(sha256sum < "$W/$2" | cut -d' ' -f1)
    printf '%s\n' "$out" | grep -q "^$1: local  $want  $W/$2 ($(stat -c %s "$W/$2") bytes)" && printf '%s\n' "$out" | grep -q "^$1: phone  $want  runtime/$3 (mode $4; " \
      && ok "$1: the same SHA-256 printed for the local file and the phone's" || bad "$1: the sums printed" "$(printf '%s' "$out" | grep -- "^$1" | tr '\n' '|')"
    cmp -s "$W/$2" "$S/runtime/$3" && [ "$(stat -c %a "$S/runtime/$3")" = "$4" ] && ok "$1: the staged runtime/$3 is the new file, its mode kept ($4)" || bad "$1: the staged file"
    grep -q "^$want  $3$" "$S/runtime.sha256" && grep -q "^$(stat -c '%d:%i:%s:%Y' "$S/runtime/$3") $3$" "$S/runtime.ids" \
      && ok "$1: its lines in runtime.sha256 and runtime.ids are the new file's" || bad "$1: runtime.sha256 or runtime.ids"
  done
  (cd "$S/runtime" && sha256sum -c --quiet ../runtime.sha256 > /dev/null 2>&1) && [ "$(wc -l < "$S/runtime.sha256")" = 4 ] && [ "$(wc -l < "$S/runtime.ids")" = 4 ] \
    && [ "$(stat -c %a "$S/runtime.sha256")" = 600 ] \
    && ok "--lib --dex: every staged runtime file is the one runtime.sha256 lists (what the switch's preflight hashes), 4 lines each" || bad "--lib --dex: runtime.sha256 against the files"
  printf '%s\n' "$out" | grep -q "the staged runtime now differs from the package runtime.conf names" && ok "--lib --dex: says the runtime is no longer the package's" || bad "--lib --dex: the note"
  printf '%s\n' "$out" | grep -q "^  oneios-display: (stand-in) library $(sha256sum < "$W/new.so" | cut -c1-12), dex $(sha256sum < "$W/new.dex" | cut -c1-12)" \
    && gone "$pid" && alive "$new" && ok "--lib --dex: the new helper runs with the new library and dex" || bad "--lib --dex: the new helper's files" "$(printf '%s' "$out" | grep stand-in)"
  [ ! -e "$S/runtime/bin/liboneios-display.so.part" ] && [ ! -e "$S/runtime/lib/oneios-display.dex.part" ] && [ ! -e "$P/data/local/tmp/oneios-display-tmp" ] && [ ! -e "$S/runtime.sha256.new" ] \
    && ok "--lib --dex: no .part, no pushed copy and no half-written list left" || bad "--lib --dex: something was left behind"
  [ "$(session_pid "$P")" = "$xp" ] && alive "$xp" && ok "--lib --dex: xnuhost is the same process, still running" || bad "--lib --dex: xnuhost"

  # ---- the touch IC
  check "touch status: on" 0 "^touch: /sys/class/sec/tsp/enabled reads 1 \(on\)" sphone touch status
  echo 0 > "$TSP"
  check "touch status: off" 0 "^touch: /sys/class/sec/tsp/enabled reads 0 \(off: the touch IC sleeps" sphone touch status
  [ "$(cat "$TSP")" = 0 ] && ok "touch status: only reads" || bad "touch status wrote the node"
  out=$(sphone touch wake 2>&1); st=$?
  [ $st = 0 ] && printf '%s\n' "$out" | grep -q "^touch: wrote 2,0 then 2,1 to /sys/class/sec/tsp/enabled (it read 0 before)" && printf '%s\n' "$out" | grep -q "^touch: /sys/class/sec/tsp/enabled reads 1 (on)" \
    && [ "$(cat "$TSP")" = 1 ] && ok "touch wake: 2,0 then 2,1, and the node reads 1 again" || bad "touch wake" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  grep -Fxq 'echo 2,0 > /sys/class/sec/tsp/enabled && echo 2,1 > /sys/class/sec/tsp/enabled' "$FAKE_ROOT_LOG" && ok "touch wake: the two writes sent, in order, to the named node" || bad "touch wake: the script sent"
  touch "$P/.fake/tsp-dead"; echo 0 > "$TSP"
  check "touch wake: an IC that stays off is status 1" 1 "the touch IC did not wake: /sys/class/sec/tsp/enabled reads 0, not 1" sphone touch wake
  rm -f "$P/.fake/tsp-dead"; mv "$TSP" "$W/tsp.aside"
  check "touch status: a phone without the node says so" 1 "reading /sys/class/sec/tsp/enabled failed on the phone.*No such file" sphone touch status
  mv "$W/tsp.aside" "$TSP"; echo 1 > "$TSP"

  # ---- the panel's level
  out=$(sphone backlight status 2>&1); st=$?
  [ $st = 0 ] && printf '%s\n' "$out" | grep -q "^backlight: 3200 of 12750 (/sys/class/backlight/panel0-backlight/brightness)" \
    && printf '%s\n' "$out" | grep -q "^One UI's own value: 3200 (control/base/backlight, read at the switch): the return writes it back" \
    && ok "backlight status: the level, max_brightness and One UI's own value" || bad "backlight status" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  out=$(sphone backlight set 5350 2>&1); st=$?
  [ $st = 0 ] && printf '%s\n' "$out" | grep -q "^backlight: 5350 written (it was 3200), the node reads 5350 of 12750" \
    && printf '%s\n' "$out" | grep -q "^One UI's own value: 3200 .*: the return writes it back" && [ "$(cat "$BL/brightness")" = 5350 ] \
    && ok "backlight set 5350: written, read back, and it says the return writes One UI's 3200 back" || bad "backlight set" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  check "backlight status after a set: the new level, One UI's value unchanged" 0 "^backlight: 5350 of 12750" sphone backlight status
  check "backlight set 0: refused" 1 "the level 0 is outside 1..12750 .*Nothing was written" sphone backlight set 0
  check "backlight set above max_brightness: refused" 1 "the level 12751 is outside 1..12750" sphone backlight set 12751
  check "backlight set of no number: refused by the command line" 2 "invalid value 'bright'" sphone backlight set bright
  [ "$(cat "$BL/brightness")" = 5350 ] && [ "$(cat "$BL/max_brightness")" = 12750 ] && ok "backlight set refused: nothing written" || bad "backlight set refused: the node changed"
  check "backlight set 12750: the top of the range" 0 "^backlight: 12750 written \(it was 5350\)" sphone backlight set 12750
  check "backlight set 1: the bottom of the range" 0 "^backlight: 1 written \(it was 12750\), the node reads 1 of 12750" sphone backlight set 1
  sphone backlight set 5350 > /dev/null 2>&1

  # ---- the return after them: One UI's level is back, the helper is ended, no session
  pid=$(helper_pid)
  check "return after the session commands: One UI's restore line" 0 "One UI restored in place \(Enforcing\)\. \([0-9]+ s\)" sphone return --note "session-cmds: done"
  [ "$(cat "$BL/brightness")" = 3200 ] && grep -q "^backlight: /sys/class/backlight/panel0-backlight/brightness back to 3200$" "$S/return.log" \
    && ok "return: One UI's own level (3200) is back after a manual 5350" || bad "return: the panel's level" "$(cat "$BL/brightness")"
  wait_for 5 gone "$pid" && no_session "$P" && ok "return: the restarted helper was ended by control/display.pid; no session" || bad "return: the helper or the session stayed"
  for c in "display-restart" "touch wake" "backlight set 5350"; do
    check "after the return: $c is refused again" 1 "no session is open" sphone $c
  done
  [ "$(cat "$BL/brightness")" = 3200 ] && ok "after the return: One UI's level was not written" || bad "after the return: the level was written"
  check "after the return: phone sums lists the new library" 0 "^$(sha256sum < "$W/new.so" | cut -d' ' -f1)  bin/liboneios-display.so$" sphone sums
  end_case

  # ---- a return while a restart waits for its helper: the return works, the restart says the helper ended
  session_up || bad "a third session"
  touch "$P/.fake/display-hang"
  sphone display-restart --timeout 30 > "$W/restart.out" 2>&1 & c=$!
  wait_for 5 grep -q "^new helper: pid" "$W/restart.out"; new=$(helper_pid)
  rm -f "$P/.fake/display-hang"
  check "return while a display-restart waits: One UI's restore line" 0 "One UI restored in place \(Enforcing\)" sphone return
  wait "$c"; st=$?
  [ $st = 1 ] && grep -q "the new display helper (pid $new) ended before its first frame" "$W/restart.out" \
    && ok "display-restart during a return: ends with status 1 and says the helper ended" || bad "display-restart during a return" "status $st: $(tail -n 2 "$W/restart.out" | tr '\n' '|')"
  wait_for 5 gone "$new" && no_session "$P" && ok "return during a restart: the helper is gone, no session" || bad "return during a restart: something stayed"
  end_case

  # ---- nothing left, and only named files of sysfs
  sleep 0.3
  out=$(helpers_left)
  [ -z "$out" ] && ok "no stand-in display helper is left running" || { bad "stand-in display helpers left running" "$out"; kill -9 $out 2>/dev/null; }
  out=$(grep -o '/sys/[^ ;")]*' "$FAKE_ROOT_LOG" | sort -u | tr '\n' ' ')
  [ "$out" = "/sys/class/backlight/panel0-backlight/brightness /sys/class/backlight/panel0-backlight/max_brightness /sys/class/sec/tsp/enabled /sys/fs/cgroup/oneios_session/cgroup.procs " ] \
    && ok "the only /sys files sent to the phone: tsp/enabled, the panel's brightness and max_brightness, the session's cgroup.procs" || bad "the /sys files sent" "$out"
  grep -o '/proc/[^ ;")]*' "$FAKE_ROOT_LOG" | sort -u | grep -Evq '^/proc/(\$p|\$1|\$\$|1|\$DP|[0-9]+)/(cmdline|ns/mnt|oom_score_adj)$' \
    && bad "the /proc files sent" "$(grep -o '/proc/[^ ;")]*' "$FAKE_ROOT_LOG" | sort -u | tr '\n' ' ')" || ok "the only /proc files sent: cmdline, ns/mnt and oom_score_adj of one named pid each"
  unset -f sphone elf dex helper_pid alive gone helpers_left session_up frames frame_after
  if [ -n "$before" ]; then export FAKE_PHONE=$before; else unset FAKE_PHONE; fi
}
