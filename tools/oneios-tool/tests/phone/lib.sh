# Shared by tests/phone/run.sh, tests/app/run.sh and tests/cycle/run.sh: a stand-in phone and the checks' helpers.
# Source it from bash with HERE (this folder), ROOT (the checkout), TOOL (the binary) and W (a scratch folder) set.
FAILED=0
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
"sh -s") sed -e "s#\(^\|[ '\"=<>(;]\)/data/#\1$FAKE_PHONE/data/#g" | bash ;;
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
  echo '{"format": 1, "build": "24A446"}' > "$S/image/manifest.json"
  echo "seed stand-in" > "$S/image/data.seed"
  echo "boot-args=stand-in" > "$S/image/overlay/userland/xnuhost.conf"
  (cd "$S/image" && for f in kernel.kc kernel.kc.hashes manifest.json data.seed overlay/userland/xnuhost.conf; do
    sha256sum "$f" >> ../image.sha256; echo "$(stat -c '%d:%i:%s:%Y' "$f") $f" >> ../image.ids; done)
  echo "xnuhost stand-in" > "$S/runtime/bin/xnuhost"
  printf 'version=0.1.6\nsession_abi=1\nxnuhost=bin/xnuhost\nimage_formats=1\nplaceholder=0\n' > "$S/runtime.conf"
  (cd "$S/runtime" && sha256sum bin/xnuhost > ../runtime.sha256)
  echo "# switch stand-in" > "$S/oneios_switch.sh"; echo "# return stand-in" > "$S/oneios_return.sh"
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
has_session() { [ -n "$(session_pid "$1")" ]; }
no_session() { [ -z "$(session_pid "$1")" ]; }
session_pid() { cat "$1/data/local/tmp/oneios/session/xnuhost.pid" 2>/dev/null; }
wait_for() { # SECONDS cmd...: until cmd succeeds
  local n=$(($1 * 10)); shift
  while ! "$@" 2>/dev/null; do n=$((n - 1)); [ $n -gt 0 ] || return 1; sleep 0.1; done
}
