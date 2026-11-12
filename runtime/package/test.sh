#!/bin/bash
# Host tests of runtime/package.sh: the package it makes, and what it must refuse. Needs the NDK (see package.sh).
#   runtime/package/test.sh
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
export OUT_DIR=$T/out KERNEL_DIR=$T/no-kernel
V=0.0.0-test
fail() { echo "FAIL: $*" >&2; exit 1; }
pass=0
if command -v sha256sum >/dev/null; then SHA256=(sha256sum); else SHA256=(shasum -a 256); fi
ok() { pass=$((pass + 1)); echo "ok $pass: $*"; }

# 1. A plain package: verifies, runtime.json first, manifest equals the content.
"$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || { cat "$T/log"; fail "package.sh"; }
P=$OUT_DIR/oneios-runtime-$V-arm64.tar.zst
( cd "$OUT_DIR" && "${SHA256[@]}" -c "$(basename "$P").sha256" > /dev/null ) || fail "sha256 file"
[ "$(wc -l < "$P.sha256" | tr -d ' ')" = 1 ] || fail ".sha256 is not one line"
[ "$(zstd -dc "$P" | tar -t | head -1)" = runtime.json ] || fail "runtime.json is not the first member"
mkdir "$T/x" && zstd -dc "$P" | tar -x -C "$T/x"
python3 -I - "$T/x" $V <<'PY' || fail "manifest does not match the content"
import hashlib, json, os, sys
root, version = sys.argv[1:]
m = json.load(open(os.path.join(root, 'runtime.json')))
assert (m['format'], m['name'], m['version'], m['arch'], m['session_abi']) == (1, 'oneios-runtime', version, 'arm64', 1), m
assert m['xnuhost'] == 'bin/xnuhost' and len(m['git']['commit']) == 40 and m['min_oneos_fork']
listed = {f['path']: f for f in m['files']}
on_disk = {os.path.relpath(os.path.join(d, n), root) for d, _, ns in os.walk(root) for n in ns} - {'runtime.json'}
assert set(listed) == on_disk, (set(listed) ^ on_disk)
for path, f in listed.items():
    data = open(os.path.join(root, path), 'rb').read()
    assert len(data) == f['size'] and hashlib.sha256(data).hexdigest() == f['sha256'], path
    assert f['mode'] == ('0755' if os.access(os.path.join(root, path), os.X_OK) else '0644'), path
assert listed['bin/xnuhost']['mode'] == '0755'
# the phone checks the OneOS fork's Hardware checks screen runs (runtime/build.sh stages them)
for tool in ('gpuctl', 'bridgectl', 'wifictl', 'brightctl', 'displayctl', 'camctl', 'oneios-camera', 'oneios-audio', 'oneios-display'):
    assert listed['bin/' + tool]['mode'] == '0755', tool
PY
ok "package, checksum, member order, manifest"
zstd -dc "$P" | tar -tv | awk '$1 !~ /^-/ {bad=1} END {exit bad}' || fail "a member is not a regular file"
ok "regular files only"

# 2. The same inputs give the same bytes.
first=$(cut -d' ' -f1 "$P.sha256")
"$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || fail "second run"
[ "$first" = "$(cut -d' ' -f1 "$P.sha256")" ] || fail "not reproducible"
ok "reproducible"

# 3. Refusals: exit 3 and no package left behind for that version.
refuse() { # NAME, then a command that fills $T/extra
  local name=$1; shift
  rm -rf "$T/extra" "$OUT_DIR"; mkdir -p "$T/extra"
  "$@"
  local rc=0
  PACKAGE_EXTRA_DIR=$T/extra "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 3 ] || { cat "$T/log"; fail "$name: exit $rc, expected the refusal (3)"; }
  [ ! -e "$OUT_DIR/oneios-runtime-$V-arm64.tar.zst" ] || fail "$name: a package was written"
  ok "refused: $name ($(grep -m1 'refused' "$T/log" | cut -c1-110))"
}
touch_file() { mkdir -p "$(dirname "$T/extra/$1")"; printf 'x' > "$T/extra/$1"; }
macho() { # PATH FILETYPE: a minimal arm64 Mach-O header
  mkdir -p "$(dirname "$T/extra/$1")"
  python3 -I -c 'import struct,sys; open(sys.argv[1],"wb").write(struct.pack("<IIIIIIII",0xfeedfacf,0x0100000c,2,int(sys.argv[2]),0,0,0,0)+b"\0"*64)' "$T/extra/$1" "$2"
}
record() { # PATH [KEY=JSON...]: a build record for the Mach-O, with fields replaced
  python3 -I - "$T/extra/$1" "${@:2}" <<'PY'
import hashlib, json, sys
path = sys.argv[1]
r = {'builder': 'oneios-kernel', 'build_id': 'test-1', 'sha256': hashlib.sha256(open(path, 'rb').read()).hexdigest(),
     'prelinked': False, 'kernelcache_kexts_added': False, 'apple_binary_inputs': [], 'restricted_sources': [], 'xnu': 'xnu-0', 'profile': 'test'}
for pair in sys.argv[2:]:
    key, value = pair.split('=', 1)
    r[key] = json.loads(value)
json.dump(r, open(path + '.build.json', 'w'))
PY
}
refuse "kernelcache by name" touch_file firmware/kernelcache.release.iphone18
refuse "a path the fork's unpacker rejects (comma)" touch_file notes,v2.txt
refuse ".ipsw" touch_file restore.ipsw
refuse ".dmg" touch_file os/090-12345-678.dmg
refuse ".aea" touch_file os.dmg.aea
refuse ".img" touch_file image/os.img
refuse "cryptex" touch_file cryptex1-systemos
im4p() { mkdir -p "$T/extra/bin"; printf '\x30\x83\x12\x34\x56\x16\x04IM4P\x16\x04krnl' > "$T/extra/bin/blob"; }
refuse "IM4P content under another name" im4p
m1() { macho kernel/xnu.macho 2; }
refuse "Mach-O without a build record" m1
m2() { macho kernel/xnu.macho 2; record kernel/xnu.macho 'kernelcache_kexts_added=true'; }
refuse "kernel with kernelcache kexts" m2
m3() { macho kernel/xnu.macho 2; record kernel/xnu.macho 'sha256="00"'; }
refuse "record for another file" m3
m4() { macho kernel/xnu.macho 12; record kernel/xnu.macho; }
refuse "kernel collection (MH_FILESET)" m4
m5() { macho kernel/xnu.macho 2; record kernel/xnu.macho 'builder="someone"'; }
refuse "record of another builder" m5
m6() { macho kernel/xnu.macho 2; record kernel/xnu.macho 'restricted_sources=["corecrypto-2023"]'; }
refuse "kernel that links a non-redistributable source" m6
link() { ln -s /etc/passwd "$T/extra/link"; }
refuse "symbolic link" link

# 4. Our own kernel with its record is accepted and named in the manifest.
rm -rf "$T/extra" "$OUT_DIR" "$T/k"; mkdir -p "$T/k" "$T/extra/kernel"
macho kernel/xnu.macho 2; record kernel/xnu.macho
mv "$T/extra/kernel/xnu.macho" "$T/extra/kernel/xnu.macho.build.json" "$T/k/"
KERNEL_DIR=$T/k "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || { cat "$T/log"; fail "package with a kernel"; }
zstd -dc "$P" | tar -xO runtime.json | python3 -I -c 'import json,sys; m=json.load(sys.stdin); assert m["kernel"]["build_id"]=="test-1" and m["kernel"]["path"]=="kernel/xnu.macho", m' || fail "kernel not in the manifest"
zstd -dc "$P" | tar -xO NOTICE | grep -q 'Apple Public Source License' || fail "no APSL notice"
ok "own kernel accepted, APSL notice present"
echo "all $pass tests passed"
