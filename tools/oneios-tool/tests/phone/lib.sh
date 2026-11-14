# Shared by tests/phone/run.sh, tests/app/run.sh and tests/cycle/run.sh: a stand-in phone and the checks' helpers.
# Source it from bash with HERE (this folder), ROOT (the checkout), TOOL (the binary) and W (a scratch folder) set.
# Sourcing it sets traps: on the suite's exit, a failure or Ctrl-C every stand-in session under $W is ended
# (stop_sessions) and the suite's own background jobs are killed, so nothing is left running.
FAILED=0
# The checks themselves use GNU coreutils and sed (stat -c, date +%N, sed -i, timeout, sha256sum, truncate): on
# macOS put Homebrew's gnubin folders (coreutils, gnu-sed) first on PATH.
gnu_missing() {
  local m=
  stat -c %s "${BASH_SOURCE[0]}" >/dev/null 2>&1 || m="$m stat"
  case "$(date +%N 2>/dev/null)" in ''|*[!0-9]*) m="$m date" ;; esac
  sed --version 2>/dev/null | grep -q GNU || m="$m sed"
  for c in timeout sha256sum truncate; do command -v "$c" >/dev/null 2>&1 || m="$m $c"; done
  printf '%s' "${m# }"
}
if [ -n "$(gnu_missing)" ]; then
  echo "$(basename "$0"): needs GNU coreutils and sed first on PATH; not GNU or missing: $(gnu_missing)" >&2
  echo "  (macOS: brew install coreutils gnu-sed, then PATH=\"\$(brew --prefix)/opt/coreutils/libexec/gnubin:\$(brew --prefix)/opt/gnu-sed/libexec/gnubin:\$PATH\")" >&2
  exit 2
fi
ok() { echo "ok      $1"; }
bad() { echo "FAILED  $1${2:+: $2}"; FAILED=$((FAILED + 1)); }
# check NAME WANT-STATUS 'grep -E pattern on stdout+stderr' cmd...
check() {
  local name=$1 want=$2 pat=$3; shift 3
  local out; out=$("$@" 2>&1); local st=$?
  if [ "$st" != "$want" ]; then bad "$name" "status $st, wanted $want: $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
  elif [ -n "$pat" ] && ! printf '%s\n' "$out" | grep -Eq -- "$pat"; then bad "$name" "no /$pat/ in: $(printf '%s' "$out" | tail -n 4 | tr '\n' '|')"
  else ok "$name"; fi
}
# The OneOS fork's checkout, for its root script (the stand-in phone runs it under its test root).
fork_dir() {
  local d
  for d in "${ONEOS_DIR:-}" "$(cd "$ROOT" && cd "$(git rev-parse --git-common-dir 2>/dev/null)/../.." 2>/dev/null && pwd)/OneOS" "$ROOT/../OneOS"; do
    [ -n "$d" ] && [ -f "$d/app-android/settings.gradle.kts" ] && { (cd "$d" && pwd); return; }
  done
}
# make_phone DIR: a phone with a stage, an installed app (versionCode 115), root, no session.
make_phone() {
  local P=$1 S=$1/data/local/tmp/oneios f
  rm -rf "$P"
  mkdir -p "$P/.fake/bin" "$S/bin" "$S/session" "$S/image/overlay/userland" "$S/runtime/bin" "$S/data" "$S/control" "$P/data/adb/ksu/bin"
  # macOS has no setsid (util-linux): the stand-in's busybox and fake-root.sh need one, so a host without it gets
  # perl's POSIX::setsid (every check that starts a stand-in session failed on the Mac before, 2026-11-12)
  if ! command -v setsid >/dev/null 2>&1; then
    cat > "$P/.fake/bin/setsid" <<'SH'
#!/bin/sh
exec perl -MPOSIX -e 'POSIX::setsid(); exec { $ARGV[0] } @ARGV or die "setsid: $ARGV[0]: $!\n"' "$@"
SH
    chmod +x "$P/.fake/bin/setsid"
  fi
  chmod 700 "$S"
  ln -s "$HERE/../phone/fake-root.sh" "$S/bin/oneios_root.sh"
  printf '#!/bin/sh\nexec "$@"\n' > "$P/data/adb/ksu/bin/busybox"
  printf '#!/bin/sh\nexit 0\n' > "$P/.fake/bin/chown"
  printf '#!/bin/sh\n[ "$1" = package ] && cat "$FAKE_PHONE/.fake/package.txt" 2>/dev/null\nexit 0\n' > "$P/.fake/bin/dumpsys"
  printf '#!/bin/sh\ncase "$1" in ro.product.model) echo SM-S928B ;; esac\n' > "$P/.fake/bin/getprop"
  printf '#!/bin/sh\n[ "$1" = path ] && [ -f "$FAKE_PHONE/data/app/~~fake==/$2-1==/base.apk" ] && echo "package:/data/app/~~fake==/$2-1==/base.apk"\nexit 0\n' > "$P/.fake/bin/pm"
  # su: `su -c CMD`. As on the phone, the command runs as root: `id -u` there says 0.
  cat > "$P/.fake/bin/su" <<'SU'
#!/bin/bash
[ -z "${FAKE_NO_ROOT:-}" ] || { echo "su: inaccessible or not found" >&2; exit 127; }
[ "$1" = -c ] || exit 2
case "$2" in
"id -u") echo 0 ;;
"sh -s")
  # the script as the tool sent it (FAKE_ROOT_LOG), then as the stand-in runs it (fake-rewrite.sh)
  if [ -n "${FAKE_ROOT_LOG:-}" ]; then t=$(mktemp); cat > "$t"; cat "$t" >> "$FAKE_ROOT_LOG"; "$FAKE_REWRITE" < "$t" | bash; st=$?; rm -f "$t"
  else "$FAKE_REWRITE" | bash; st=$?; fi
  # then the phone's drivers act on what the script wrote to their nodes (a test's .fake/drivers: the touch IC's
  # tsp/enabled reads 1 after `2,1` was written to it), before the tool's next command reads them
  [ ! -x "$FAKE_PHONE/.fake/drivers" ] || "$FAKE_PHONE/.fake/drivers"
  exit $st ;;
*) exec bash -c "$2" ;;
esac
SU
  chmod +x "$P/.fake/bin/"* "$P/data/adb/ksu/bin/busybox"
  cat > "$P/.fake/package.txt" <<'PKG'
Packages:
  Package [com.fahadali.oneos] (1a2b3c4):
    versionCode=115 minSdk=31 targetSdk=37
    versionName=0.4.86
    signatures=PackageSignatures{9d4c8e1 version:3, signatures:[5ac1f00d], past signatures:[]}
PKG
  echo "kernel collection stand-in" > "$S/image/kernel.kc"
  echo "hash list stand-in" > "$S/image/kernel.kc.hashes"
  echo "seed stand-in" > "$S/image/data.seed"
  echo "boot-args=stand-in" > "$S/image/overlay/userland/xnuhost.conf"
  # a manifest that lists the set's files, as imagetool.py writes it (json.dumps indent 2): switch checks it
  python3 -I - "$S/image" <<'PY'
import hashlib, json, os, sys
d = sys.argv[1]
files = [{"path": p, "size": os.path.getsize(f"{d}/{p}"), "sha256": hashlib.sha256(open(f"{d}/{p}", "rb").read()).hexdigest()}
         for p in ("data.seed", "kernel.kc", "kernel.kc.hashes", "overlay/userland/xnuhost.conf")]
open(f"{d}/manifest.json", "w").write(json.dumps({"format": 1, "build": "24A446", "files": files}, indent=2) + "\n")
PY
  (cd "$S/image" && for f in kernel.kc kernel.kc.hashes manifest.json data.seed overlay/userland/xnuhost.conf; do
    sha256sum "$f" >> ../image.sha256; echo "$(stat -c '%d:%i:%s:%Y' "$f") $f" >> ../image.ids; done)
  echo "xnuhost stand-in" > "$S/runtime/bin/xnuhost"
  printf 'version=0.1.6\nsession_abi=1\nxnuhost=bin/xnuhost\nimage_formats=1\nplaceholder=0\n' > "$S/runtime.conf"
  (cd "$S/runtime" && sha256sum bin/xnuhost > ../runtime.sha256)
  # the session scripts: the switch's stand-in answers the root script's preflight question (--check)
  printf '%s\n' '[ -e "$ONEIOS_TEST_ROOT/.fake/preflight-refuses" ] && echo "PREFLIGHT REFUSED: $(cat "$ONEIOS_TEST_ROOT/.fake/preflight-refuses")" || echo "PREFLIGHT OK"' > "$S/oneios_switch.sh"
  echo "# return stand-in" > "$S/oneios_return.sh"; chmod 600 "$S/oneios_switch.sh" "$S/oneios_return.sh"
  # the Data store: 48 MiB, of which three places hold data
  truncate -s $((48 << 20)) "$S/data/data.img"
  printf 'first block' | dd of="$S/data/data.img" conv=notrunc status=none
  head -c 300000 /dev/urandom | dd of="$S/data/data.img" bs=1M seek=17 conv=notrunc status=none
  printf 'the end' | dd of="$S/data/data.img" bs=1 seek=$(((48 << 20) - 7)) conv=notrunc status=none
  echo 0.1 > "$P/.fake/step"
}
# end_session DIR: stop a stand-in session that is still there.
end_session() {
  local p; p=$(cat "$1/data/local/tmp/oneios/session/xnuhost.pid" 2>/dev/null)
  [ -z "$p" ] || kill "$p" 2>/dev/null
  rm -f "$1/data/local/tmp/oneios/session/xnuhost.pid"
}
# session_procs DIR: the pids of the stand-in sessions (fake-session.sh, named fake-xnuhost) of phones under DIR.
# ps, not the pid files or the stand-in's /proc: a session whose files are gone is found too.
session_procs() {
  ps -A -o pid= -o command= 2>/dev/null | awk -v d="$1/" '$2 == "fake-xnuhost" && index($0, d) { print $1 }'
}
# stop_sessions DIR: end every stand-in session of the phones under DIR: its fake xnuhost and what it started
# (its process group: fake-root.sh starts it with setsid), then its pid file and its /proc entry. Prints how many.
stop_sessions() {
  local d=$1 p pids n=0 i
  pids=$(session_procs "$d")
  for p in $pids; do kill -TERM -- "-$p" 2>/dev/null || kill -TERM "$p" 2>/dev/null; n=$((n + 1)); done
  if [ -n "$pids" ]; then
    for i in $(seq 50); do [ -n "$(session_procs "$d")" ] || break; sleep 0.1; done
    for p in $(session_procs "$d"); do kill -KILL -- "-$p" 2>/dev/null || kill -KILL "$p" 2>/dev/null; done
  fi
  for p in "$d"/*/data/local/tmp/oneios/session/xnuhost.pid; do [ ! -e "$p" ] || rm -f "$p"; done
  for p in "$d"/*/.fake/proc/*; do [ ! -e "$p" ] || rm -rf "$p"; done
  echo $n
}
# end_case: after a case that leaves no session, end one it left anyway (it failed), so that it does not spread
# to the next case (the fork's root script refuses a switch while one runs)
end_case() {
  local n; n=$(stop_sessions "$W")
  [ "$n" = 0 ] || echo "(ended $n stand-in session(s) the case before left running)"
}
# the suite's end, a failure or Ctrl-C: no stand-in session and no job of the suite stays
standin_exit() {
  local j; j=$(jobs -p); [ -z "$j" ] || kill $j 2>/dev/null
  [ -z "${W:-}" ] || stop_sessions "$W" > /dev/null
}
trap standin_exit EXIT
trap 'echo "(stopped by Ctrl-C)"; exit 130' INT
trap 'echo "(stopped by SIGTERM: Ctrl-C while a command ran, or kill)"; exit 143' TERM
# Ctrl-C while a command runs: oneios-tool catches SIGINT and ends with status 0, and bash then takes the signal
# as handled by it and runs no INT trap (the suite would go on). This watcher (perl: a background job starts with
# SIGINT ignored, which bash cannot trap again) sees the Ctrl-C and sends the suite a SIGTERM, whose trap runs
# when the command ends. It ends with the suite.
perl -e '$SIG{INT} = sub { kill "TERM", $ARGV[0]; exit 0 }; while (kill 0, $ARGV[0]) { sleep 1 }' $$ &
# no_session_left NAME: the check at a suite's end
no_session_left() {
  local n; n=$(stop_sessions "$W")
  [ "$n" = 0 ] && ok "$1: no stand-in session is left running" || bad "$1: $n stand-in session(s) were left running"
}
has_session() { [ -n "$(session_pid "$1")" ]; }
no_session() { [ -z "$(session_pid "$1")" ]; }
session_pid() { cat "$1/data/local/tmp/oneios/session/xnuhost.pid" 2>/dev/null; }
wait_for() { # SECONDS cmd...: until cmd succeeds
  local n=$(($1 * 10)); shift
  while ! "$@" 2>/dev/null; do n=$((n - 1)); [ $n -gt 0 ] || return 1; sleep 0.1; done
}
