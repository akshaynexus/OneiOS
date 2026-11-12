#!/bin/bash
# A stand-in phone for oneios-tool's phone, app and cycle commands: ADB=tools/oneios-tool/tests/phone/fake-adb.sh.
# It runs what the tool sends to the phone on this machine, inside a folder that plays the phone's file system.
#
#   FAKE_PHONE=DIR     the phone's root (DIR/data/local/tmp/oneios is the stage). tests/phone/run.sh makes one.
#   FAKE_DEVICES=TEXT  the lines `adb devices` lists (default: "FAKE0001<tab>device")
#   FAKE_NO_ROOT=1     su refuses, as on a phone whose root is gone after a restart
#   FAKE_DROP=FILE     a `tail -f` or `tail -F` (phone log --follow) is cut off after one second, once, while FILE
#                      exists (the fake removes it): adb dropping
#   FAKE_DROP_NEW=1    and while adb is away the session's log is replaced (moved to logs/xnuhost-dropped.log, a
#                      new file begun with the same first line, which the session goes on writing): a new file
#   FAKE_CUT=FILE      a `tail -f` or `tail -F` that is running is cut off when FILE appears (the fake removes it):
#                      adb dropping at the moment a test chooses
#   FAKE_ADB_LOG=FILE  every call is appended to FILE
#   FAKE_ROOT_LOG=FILE every script for root's shell (`su -c 'sh -s'`, on stdin) is appended to FILE, as the tool
#                      sent it
#   FAKE_PROC=host|stand-in  what /proc the phone's commands read. host: this machine's (Linux; the default where
#                      /proc/self/cmdline can be read). stand-in: the stand-in's own, $FAKE_PHONE/.fake/proc, where
#                      each stand-in session keeps PID/cmdline while it runs (the default on macOS, which has no
#                      /proc; FAKE_PROC=stand-in runs that path on Linux too). The tool's commands are the same.
#
# Commands: devices; [-s SERIAL] shell CMD; exec-out CMD; push LOCAL REMOTE; install [-r] [-d] APK.
# In CMD every /data/... path becomes $FAKE_PHONE/data/... (and /proc/... the stand-in's /proc with
# FAKE_PROC=stand-in: fake-rewrite.sh); `su -c X` runs X (`su -c 'sh -s'`: stdin); the
# stage's bin/oneios_root.sh is tests/phone/fake-root.sh, which hands everything but switch and return to the
# OneOS fork's own root script under its test root. dumpsys, getprop, pm and chown are stand-ins in $FAKE_PHONE/.fake/bin.
set -u
[ -z "${FAKE_ADB_LOG:-}" ] || printf '%s\n' "$*" >> "$FAKE_ADB_LOG"
P=${FAKE_PHONE:?FAKE_PHONE names the stand-in phone folder}
serial=
while [ $# -gt 0 ]; do
  case "$1" in
  -s) serial=$2; shift 2 ;;
  *) break ;;
  esac
done
devices=${FAKE_DEVICES-$'FAKE0001\tdevice'}
HERE=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # $0 may be a link (run.sh's bin/adb, for stage.sh)
if [ -z "${FAKE_PROC:-}" ]; then
  if [ -r /proc/self/cmdline ]; then FAKE_PROC=host; else FAKE_PROC=stand-in; fi
fi
case "$FAKE_PROC" in host|stand-in) ;; *) echo "fake-adb: FAKE_PROC=$FAKE_PROC is neither host nor stand-in" >&2; exit 2 ;; esac
export FAKE_PHONE=$P PATH="$P/.fake/bin:$PATH" ONEIOS_TEST_ROOT=$P FAKE_PROC FAKE_REWRITE=$HERE/fake-rewrite.sh
rewrite() { "$FAKE_REWRITE"; }
# the stand-in's /proc holds only live processes: the entry of a session that is gone (killed with SIGKILL, so
# its own trap did not run) is dropped, as the kernel drops it
if [ "$FAKE_PROC" = stand-in ] && [ -d "$P/.fake/proc" ]; then
  for d in "$P/.fake/proc"/*; do
    [ -e "$d" ] || continue
    kill -0 "${d##*/}" 2>/dev/null || rm -rf "$d"
  done
fi
case "${1:-}" in
devices)
  echo "List of devices attached"
  [ -z "$devices" ] || printf '%s\n' "$devices"
  echo ;;
shell|exec-out)
  [ -n "$serial" ] || [ "$(printf '%s\n' "$devices" | grep -c .)" = 1 ] || { echo "adb: more than one device/emulator" >&2; exit 1; }
  cmd=$(printf '%s' "$2" | rewrite)
  if [ -n "${FAKE_DROP:-}" ] && [ -e "$FAKE_DROP" ] && case "$cmd" in *"tail -n +"*" -f "*|*"tail -n +"*" -F "*) true ;; *) false ;; esac; then
    rm -f "$FAKE_DROP"
    timeout 1 bash -c "$cmd"
    if [ -n "${FAKE_DROP_NEW:-}" ]; then
      L=$P/data/local/tmp/oneios/session/xnuhost.log
      first=$(head -n 1 "$L"); mv "$L" "$P/data/local/tmp/oneios/logs/xnuhost-dropped.log"; printf '%s\n' "$first" > "$L"
    fi
    echo "adb: device offline" >&2
    exit 1
  fi
  if [ -n "${FAKE_CUT:-}" ] && case "$cmd" in *"tail -n +"*" -f "*|*"tail -n +"*" -F "*) true ;; *) false ;; esac; then
    bash -c "$cmd" & t=$!
    trap 'kill $t 2>/dev/null; exit 143' TERM INT
    while kill -0 $t 2>/dev/null; do
      if [ -e "$FAKE_CUT" ]; then rm -f "$FAKE_CUT"; kill $t 2>/dev/null; wait $t; echo "adb: device offline" >&2; exit 1; fi
      sleep 0.1
    done
    wait $t; exit $?
  fi
  exec bash -c "$cmd" ;;
push)
  dst=$(printf '%s' "$3" | rewrite)
  mkdir -p "$(dirname "$dst")" && cp -r "$2" "$dst" && echo "$2: 1 file pushed" ;;
install)
  shift
  while [ $# -gt 1 ]; do shift; done
  [ -f "$1" ] || { echo "adb: failed to stat $1" >&2; exit 1; }
  [ ! -e "$P/.fake/install-fails" ] || { echo "adb: failed to install $1: Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: signatures do not match]" >&2; exit 1; }
  # the APK goes where the stand-in `pm path` names it; `dumpsys package` then says what APK.package.txt
  # (or the phone's .fake/next-package.txt) holds
  mkdir -p "$P/data/app/~~fake==/com.fahadali.oneos-1==" && cp "$1" "$P/data/app/~~fake==/com.fahadali.oneos-1==/base.apk"
  for t in "$1.package.txt" "$P/.fake/next-package.txt"; do [ ! -f "$t" ] || { cp "$t" "$P/.fake/package.txt"; break; }; done
  echo "Performing Streamed Install"; echo Success ;;
*) echo "fake-adb: no stand-in for: $*" >&2; exit 2 ;;
esac
