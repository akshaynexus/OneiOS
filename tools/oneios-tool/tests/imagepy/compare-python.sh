#!/bin/bash
# imagetool.py's six last subcommands and remotezip.py against their Rust ports, side by side on one stand-in IPSW:
#   scripts/fetch/remotezip.py ls|cat|head|get                 ->  oneios-tool remotezip ...
#   scripts/image/imagetool.py check-ipsw|fetch|record-plain|aea-info  ->  oneios-tool image ...
# (`image pack` and `image verify ARCHIVE` are compared in `cargo test -p oneios-image`: archive_tests.rs runs
# imagetool.py on the same stand-in set, and the two archives are the same bytes.)
#
#   tools/oneios-tool/tests/imagepy/compare-python.sh      one line per case (same | DIFFERENT), then the counts
#
# imagetool.py is not retired (the owner decides), so both sides run here. The stand-in IPSW and its profile are
# made by the Python tool's own test fixtures (scripts/image/tests/fixtures.py: `fake_ipsw`, the one existing
# Python this script runs beside the two originals); its members are random bytes, so there is no golden: each
# case is the two tools' stdout, stderr (their last line), status and the files they left, on the same input.
# No Apple file is read, and nothing is fetched from a network: SOURCE is the local file.
# Env: ONEIOS_TOOL (the binary), KEEP=1 (keep the scratch folder), TMPDIR.
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
TOOL=${ONEIOS_TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
[ -x "$TOOL" ] || { echo "compare-python: no $TOOL (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)" >&2; exit 2; }
for f in scripts/image/imagetool.py scripts/fetch/remotezip.py scripts/image/tests/fixtures.py; do
  [ -f "$ROOT/$f" ] || { echo "compare-python: $f is not there (retired): cargo test -p oneios-image has the ports' tests" >&2; exit 2; }
done
W=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/imagepy.XXXXXX")" && pwd -P)
[ "${KEEP:-0}" = 1 ] || trap 'rm -rf "$W"' EXIT
sha() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
same=0; differ=0
# side py|rs TOOL-NAME ARGS...: one command by one side
side() {
  local which=$1 tool=$2; shift 2
  if [ "$which" = py ]; then
    case "$tool" in remotezip) python3 -I "$ROOT/scripts/fetch/remotezip.py" "$@" ;; image) python3 -I "$ROOT/scripts/image/imagetool.py" "$@" ;; esac
  else "$TOOL" "$tool" "$@"; fi
}
# c NAME TOOL ARGS...: the case with `@` in ARGS as each side's own folder ($W/py, $W/rs); what is compared: stdout,
# the last line of stderr (a Python traceback's last line is its error), the status, the folder's files.
c() {
  local name=$1 tool=$2 s a args; shift 2
  for s in py rs; do
    args=(); for a in "$@"; do args+=("${a//@/$W/$s}"); done
    side $s "$tool" "${args[@]}" > "$W/$s.out" 2> "$W/$s.err"; echo "status $?" >> "$W/$s.out"
    tail -n 1 "$W/$s.err" | sed -e "s#$W/$s#DIR#g" -e 's/^[A-Za-z_.]*Error: //' -e 's/^remotezip\.Error: //' >> "$W/$s.out"
    ( cd "$W/$s" && find . -type f | LC_ALL=C sort | while read -r f; do echo "$(sha < "$f" | cut -c1-64) $f"; done ) >> "$W/$s.out"
    sed "s#$W/$s#DIR#g" "$W/$s.out" > "$W/$s.said"
  done
  if cmp -s "$W/py.said" "$W/rs.said"; then same=$((same + 1)); echo "same       $name"
  else differ=$((differ + 1)); echo "DIFFERENT  $name"; diff "$W/py.said" "$W/rs.said" | head -8; fi
}

# ---- the stand-in IPSW and its profile (and one whose pin of a member is wrong)
mkdir -p "$W/py" "$W/rs" "$W/src" "$W/bad"
python3 -I -c 'import sys; sys.path.insert(0, sys.argv[1]); import fixtures; fixtures.fake_ipsw(sys.argv[2]); fixtures.fake_ipsw(sys.argv[3], wrong_pin="trust-app")' \
  "$ROOT/scripts/image/tests" "$W/src" "$W/bad" || { echo "compare-python: the fixtures failed" >&2; exit 2; }
IPSW=$(ls "$W"/src/*.ipsw); P=$W/src/test.toml
edit() { sed "$1" "$P" > "$W/src/$2.toml"; }

# ---- remotezip
c "remotezip ls" remotezip ls "$IPSW"
c "remotezip ls with a pattern" remotezip ls "$IPSW" 'trustcache$|\.plist'
c "remotezip cat (a deflated member)" remotezip cat "$IPSW" BuildManifest.plist
c "remotezip cat (by the base name)" remotezip cat "$IPSW" unrelated.bin
c "remotezip head (a stored member)" remotezip head "$IPSW" 001-00001-001.dmg.aea 64
c "remotezip head (a deflated member)" remotezip head "$IPSW" BuildManifest.plist 64
c "remotezip get (a stored and a deflated member, one under another name)" remotezip get "$IPSW" @/got -j 3 001-00001-001.dmg.aea Restore.plist=restore.copy
c "remotezip get again (already complete)" remotezip get "$IPSW" @/got 001-00001-001.dmg.aea
c "remotezip: a member that is not there" remotezip cat "$IPSW" no-such-member

# ---- check-ipsw
c "check-ipsw" image check-ipsw "$P"
c "check-ipsw --source" image check-ipsw "$P" --source "$IPSW"
edit 's/^path = "001-00003-001.dmg"/path = "001-00009-001.dmg"/' nomember; c "check-ipsw: a member that is not in the IPSW" image check-ipsw "$W/src/nomember.toml"
edit 's/^build = "0T1"/build = "0T2"/' build; c "check-ipsw: another build" image check-ipsw "$W/src/build.toml"
edit 's/^crc32 = "\([0-9a-f]\)/crc32 = "f\1/' crc; c "check-ipsw: other CRC-32s" image check-ipsw "$W/src/crc.toml"
c "check-ipsw: an IPSW of another size" image check-ipsw "$P" --source "$W/bad/$(basename "$IPSW")"
c "check-ipsw: no profile" image check-ipsw

# ---- fetch, aea-info, record-plain
c "fetch" image fetch "$P" @/inputs -j 2
c "fetch again" image fetch "$P" @/inputs
c "aea-info: a fetched stand-in" image aea-info @/inputs/001-00001-001.dmg.aea
c "aea-info: a file that is no AEA" image aea-info "$P"
for s in py rs; do cp "$W/$s/inputs/001-00003-001.dmg" "$W/$s/inputs/001-00001-001.dmg"; rm "$W/$s/inputs/001-00001-001.dmg.aea"; done
c "record-plain" image record-plain @/inputs os @/inputs/001-00001-001.dmg
c "fetch --only os: the decrypted image is there" image fetch "$P" @/inputs --only os
c "fetch --only with two roles" image fetch "$P" @/two --only kernelcache,restore -j 1
c "fetch: a pinned SHA-384 that is another file's" image fetch "$W/bad/test.toml" @/bad
c "fetch: another build" image fetch "$W/src/build.toml" @/other
echo "imagepy: $same the same, $differ different"
[ $differ = 0 ]
