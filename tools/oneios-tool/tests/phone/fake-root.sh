#!/bin/sh
# The stand-in phone's bin/oneios_root.sh (tests/phone/fake-adb.sh). switch and return are played here; every
# other verb is the OneOS fork's own root script under its test root ($FAKE_FORK_ROOT_SH), so status, begin,
# receive and finish are the real code.
P=${FAKE_PHONE:?}
STAGE=$P/data/local/tmp/oneios
here=$(dirname "$(readlink -f "$0")")
case "${1:-}" in
switch)
  [ $# -eq 3 ] || { echo "@@REFUSED usage: switch <versionCode> <ack>"; exit 3; }
  [ "$3" = 1 ] || { echo "@@REFUSED the experimental tick was not given"; exit 2; }
  [ -f "$STAGE/oneios_switch.sh" ] && [ -f "$STAGE/oneios_return.sh" ] || { echo "@@REFUSED oneios_switch.sh is not staged: set up again"; exit 2; }
  p=$(cat "$STAGE/session/xnuhost.pid" 2>/dev/null)
  [ -z "$p" ] || ! kill -0 "$p" 2>/dev/null || { echo "@@REFUSED an iOS session is already running"; exit 2; }
  [ ! -e "$P/.fake/preflight-refuses" ] || { echo "@@REFUSED $(cat "$P/.fake/preflight-refuses")"; exit 2; }
  echo "=== OneOS app switch $(date '+%F %T') epoch $(date +%s) versionCode $2 ===" >> "$STAGE/switch.log"
  setsid bash -c 'exec -a fake-xnuhost bash "$0" "$1"' "$here/fake-session.sh" "$P" </dev/null >/dev/null 2>&1 &
  echo "@@OK switch" ;;
return)
  echo "@@OK return nothing" ;;
*)
  [ -n "${FAKE_FORK_ROOT_SH:-}" ] || { echo "@@REFUSED the stand-in phone has no fork root script (FAKE_FORK_ROOT_SH)"; exit 2; }
  exec sh "$FAKE_FORK_ROOT_SH" "$@" ;;
esac
