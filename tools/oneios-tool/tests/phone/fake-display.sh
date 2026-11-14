#!/bin/bash
# The stand-in phone's display helper: a copy of this file is runtime/bin/oneios-display of a stand-in phone that
# has one (tests/phone/session-cmds.sh), started as the switch starts the real one
# (`busybox setsid runtime/bin/oneios-display --session DIR --takeover --wait 60 >> control/display.log`), by the
# stand-in session (fake-session.sh) and by `oneios-tool phone display-restart`. Like the real launcher it needs
# liboneios-display.so beside itself and ../lib/oneios-display.dex, and it writes the real helper's lines to stderr
# (runtime/display: `layer up`, `first frame posted`, `asked to stop`, `layer removed`). It leaves on SIGTERM, and
# when the stand-in xnuhost (session/xnuhost.pid) is gone, as the real one does when the socket closes; never later
# than two minutes after its start.
#   $P/.fake/display-broken   the library cannot be loaded: a linker error, exit 5 (the fault of 2026-11-14)
#   $P/.fake/display-hang     it connects and then never makes its layer (no readiness line)
#   $P/.fake/display-slow     the layer comes after 0.7 s and the first frame's line is written in two pieces 0.7 s apart
#   $P/.fake/display-deaf     SIGTERM is ignored (it takes a SIGKILL)
# Each is read once, first thing (a test may take the file away once the helper's `panel modes` line is there). Every start is appended to $P/.fake/display-starts (`PID ARGS...`).
# While it runs it keeps $P/.fake/proc/PID/cmdline (app_process's argv, as the real helper's /proc entry reads), the
# stand-in's /proc (FAKE_PROC=stand-in); with the host's /proc its command line holds this file's path.
P=${FAKE_PHONE:?}
S=$P/data/local/tmp/oneios
broken=; hang=; slow=; deaf=
[ ! -e "$P/.fake/display-broken" ] || broken=1
[ ! -e "$P/.fake/display-hang" ] || hang=1
[ ! -e "$P/.fake/display-slow" ] || slow=0.7
[ ! -e "$P/.fake/display-deaf" ] || deaf=1
here=$(cd "$(dirname "$0")" && pwd)
lib=$here/liboneios-display.so
dex=$here/../lib/oneios-display.dex
mkdir -p "$P/.fake/proc/$$"
{ printf 'app_process\0/system/bin\0OneiOSDisplay\0--lib\0%s\0' "$lib"; for a in "$@"; do printf '%s\0' "$a"; done; } > "$P/.fake/proc/$$/cmdline"
trap 'rm -rf "$P/.fake/proc/$$"' EXIT
echo "$$ $*" >> "$P/.fake/display-starts"
say() { echo "oneios-display: $*" >&2; }
[ -r "$lib" ] && [ -r "$dex" ] || { say "$dex or $lib is missing"; exit 2; }
if [ -n "$broken" ]; then
  echo "java.lang.UnsatisfiedLinkError: dlopen failed: cannot locate symbol \"ANativeWindow_getBuffersDataSpace\" referenced by \"$lib\"..." >&2
  exit 5
fi
stop=0
if [ -n "$deaf" ]; then trap '' TERM; else trap 'stop=1' TERM; fi
trap 'stop=1' INT HUP
xp=$(cat "$S/session/xnuhost.pid" 2>/dev/null)
[ -n "$xp" ] && kill -0 "$xp" 2>/dev/null || { say "$S/session/display.sock: No such file or directory"; exit 3; }
say "(stand-in) library $(sha256sum < "$lib" | cut -c1-12), dex $(sha256sum < "$dex" | cut -c1-12)"
say "panel modes: active 1080x2340 (up to 120 Hz), native 1440x3120 (up to 120 Hz)"
if [ -z "$hang" ]; then
  say "reporting the panel to xnuhost: 1440x3120 at 120 Hz"
  say "guest frame 1440x3120, stride 5888, format 1 (B,G,R,X 8-bit sRGB); layer buffers in format 5, dataspace 8810000"
  [ -z "$slow" ] || sleep "$slow"
  say "layer up"
  say "buffers tagged srgb (dataspace 0x8810000)"
  printf 'oneios-display: first frame posted (1440x3120 buffer, ' >&2
  [ -z "$slow" ] || sleep "$slow"
  echo "stride 1472, format 5 BGRA_8888, dataspace 0x8810000 srgb; the guest's frame: bgrx8888)" >&2
fi
n=0
while [ $stop = 0 ] && kill -0 "$xp" 2>/dev/null && [ $n -lt 1200 ]; do sleep 0.1; n=$((n + 1)); done
if [ $stop = 1 ]; then say "asked to stop after $n frames"; else say "xnuhost closed the socket"; say "the socket closed after $n frames"; fi
say "layer removed"
exit 0
