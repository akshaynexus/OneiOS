#!/bin/bash
# A stand-in phone for oneios-tool's phone, app and cycle commands: ADB=tools/oneios-tool/tests/phone/fake-adb.sh.
# It runs what the tool sends to the phone on this machine, inside a folder that plays the phone's file system.
#
#   FAKE_PHONE=DIR     the phone's root (DIR/data/local/tmp/oneios is the stage). tests/phone/run.sh makes one.
#   FAKE_DEVICES=TEXT  the lines `adb devices` lists (default: "FAKE0001<tab>device")
#   FAKE_NO_ROOT=1     su refuses, as on a phone whose root is gone after a restart
#   FAKE_DROP=FILE     a `tail -F` (phone log --follow) is cut off after one second, once, while FILE exists
#                      (the fake removes it): adb dropping
#   FAKE_ADB_LOG=FILE  every call is appended to FILE
#
# Commands: devices; [-s SERIAL] shell CMD; exec-out CMD; push LOCAL REMOTE; install [-r] [-d] APK.
# In CMD every /data/... path becomes $FAKE_PHONE/data/...; `su -c X` runs X (`su -c 'sh -s'`: stdin); the
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
rewrite() { sed -e "s#\(^\|[ '\"=<>(;]\)/data/#\1$P/data/#g"; }
export FAKE_PHONE=$P PATH="$P/.fake/bin:$PATH" ONEIOS_TEST_ROOT=$P
case "${1:-}" in
devices)
  echo "List of devices attached"
  [ -z "$devices" ] || printf '%s\n' "$devices"
  echo ;;
shell|exec-out)
  [ -n "$serial" ] || [ "$(printf '%s\n' "$devices" | grep -c .)" = 1 ] || { echo "adb: more than one device/emulator" >&2; exit 1; }
  cmd=$(printf '%s' "$2" | rewrite)
  if [ -n "${FAKE_DROP:-}" ] && [ -e "$FAKE_DROP" ] && case "$cmd" in *"tail -n +"*" -F "*) true ;; *) false ;; esac; then
    rm -f "$FAKE_DROP"
    timeout 1 bash -c "$cmd"
    echo "adb: device offline" >&2
    exit 1
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
