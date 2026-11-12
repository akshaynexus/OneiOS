#!/bin/bash
# The stand-in phone's session (started by fake-root.sh's switch as a process whose name holds "xnuhost"):
# writes session/xnuhost.pid, lets session/xnuhost.log grow with the lines of a boot (the framing of
# runtime/xnuhost/src/log.c, the stages of oneos/interface.md section 8), makes session/ready, and returns when
# session/return.request appears: the restore line in return.log, return.outcome.
#   $P/.fake/step     seconds between log lines (default 0.2)
#   $P/.fake/panic    the boot ends in a panic instead of the first frame
#   $P/.fake/hang     the boot stops before the first frame and waits
#   $P/.fake/no-return  a return.request is not answered (a return that hangs)
#   $P/.fake/log-delay  seconds before the last session's xnuhost.log is moved to logs/ and this one's begun (the
#                     phone's switch does it about 40 s in; default 0)
# While it runs it keeps $P/.fake/proc/PID/cmdline (its argv, NUL-separated, as /proc/PID/cmdline holds it): the
# stand-in's /proc, read instead of the host's with FAKE_PROC=stand-in (fake-rewrite.sh). The entry is removed
# when the session ends, also by SIGTERM, SIGINT or SIGHUP (fake-adb.sh drops one left by SIGKILL).
P=$1
S=$P/data/local/tmp/oneios
step=$(cat "$P/.fake/step" 2>/dev/null || echo 0.2)
mkdir -p "$S/session" "$S/logs"
rm -f "$S/session/ready" "$S/session/return.request"
trap 'rm -rf "$P/.fake/proc/$$"' EXIT
trap 'exit 143' TERM; trap 'exit 130' INT; trap 'exit 129' HUP
mkdir -p "$P/.fake/proc/$$"
printf 'fake-xnuhost\0%s\0%s\0' "$0" "$P" > "$P/.fake/proc/$$/cmdline"
echo $$ > "$S/session/xnuhost.pid"
d=$(cat "$P/.fake/log-delay" 2>/dev/null || echo 0); [ "$d" = 0 ] || sleep "$d"
[ ! -f "$S/session/xnuhost.log" ] || mv "$S/session/xnuhost.log" "$S/logs/xnuhost-$(date +%s).log"
t0=$(date +%s.%N)
line() { printf '[%9.3f] %s\n' "$(echo "$(date +%s.%N) $t0" | awk '{print $1-$2}')" "$*" >> "$S/session/xnuhost.log"; }
asked() { [ -e "$S/session/return.request" ]; }
boot=(
  "xnuhost I xnuhost: session ABI 1, mode takeover"
  "xnuhost I cpu0: entering the kernel at 0xfffffe0007004000"
  "guest     Darwin Kernel Version 27.0.0: stand-in"
  "guest     ONEIOS BACKLIGHT: level 512"
  "guest     BSD root: disk0s1, major 1, minor 4"
  "guest     load_init_program: attempting to load /sbin/launchd"
  "guest     oneios-gpu-report: 600 frames 10 s"
  "guest     com.apple.xpc.launchd|2026-11-12 stand-in"
  "guest     oneios-hid: touch down 100 200"
  "guest     backboardd[66]: stand-in first line"
  "guest     ONEIOS WIFI: scan done"
  "guest     SpringBoard[70]: stand-in first line"
)
for l in "${boot[@]}"; do
  asked && break
  line "$l"; sleep "$step"
done
if [ -e "$P/.fake/panic" ]; then
  line "guest     panic(cpu 0 caller 0xfffffe0007123456): stand-in panic text"
  line "xnuhost I stopping: the guest halted on cpu0 (reason XH_HALT_PANIC, code 0)"
elif [ ! -e "$P/.fake/hang" ] && ! asked; then
  line "xnuhost I guest ready: first frame"
  touch "$S/session/ready"
fi
n=0
while ! asked; do
  sleep "$step"; n=$((n + 1))
  [ $((n % 5)) != 0 ] || line "guest     oneios-gpu-report: 600 frames 10 s"
  [ $n -lt 3000 ] || break
done
while [ -e "$P/.fake/no-return" ]; do sleep "$step"; done
line "xnuhost I stopping: signal 15 (reason XH_HALT_SIGNAL, code 15)"
line "xnuhost I exit 0"
{
  echo "=== return $(date '+%F %T') asked: $(cat "$S/session/return.request" 2>/dev/null) ==="
  echo "One UI restored in place (Enforcing)."
} >> "$S/return.log"
echo "inplace $(date +%s) fake-boot-id" > "$S/return.outcome"
rm -f "$S/session/xnuhost.pid" "$S/session/return.request"
