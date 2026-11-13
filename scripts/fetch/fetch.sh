#!/bin/bash
# Get the parts of Apple's restore file that a build profile names, into inputs/<build>/ (never in git).
#
#   scripts/fetch/fetch.sh --list PROFILE          only read the IPSW's ZIP directory and BuildManifest and
#                                                  compare them with the profile (about 1 MiB of traffic)
#   scripts/bg.sh fetch scripts/fetch/fetch.sh PROFILE     the download: 10.6 GiB for iOS 27.0.1, in tmux
#
# Options: -j N        parallel range requests per member (default 8)
#          --only R,.. only these roles (os, systemos, app, trust-static, kernelcache, ...)
#          --keep-aea  keep the encrypted files after they are decrypted
#          --source S  another copy of the same IPSW: a URL or a local file
# Env: INPUTS (default inputs/<build>), IPSW (the ipsw CLI; default out/bin/ipsw, then PATH).
#
# Only the named ZIP members are read, with HTTP range requests; an interrupted run resumes. Each member is
# checked against the profile (size, CRC-32, and SHA-384 or SHA-256 where the profile pins one). AEA members
# (the OS and SystemOS images) are decrypted: `ipsw fw aea --key` reads the key-service fields of the AEA
# header and gets the key from Apple, and `ipsw fw aea` decrypts with it. The key is not written to disk.
# Linux and macOS. Tools: oneios-tool (tools/oneios-tool, Rust: `image check-ipsw`, `fetch`, `aea-info`, `record-plain`;
# built when missing or stale, ONEIOS_TOOL=PATH for another; scripts/image/imagetool.py did this until 2026-11-13 and is
# still in the tree, unused), python3 (the last check of the decrypted images), and for the decryption ipsw
# (scripts/fetch/get-ipsw-cli.sh).
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
. "$ROOT/scripts/lib/oneios-tool.sh"
die() { echo "fetch: $*" >&2; exit 1; }

list=0 keep=0 jobs=8 only= source= profile=
while [ $# -gt 0 ]; do
  case "$1" in
  --list) list=1 ;;
  --keep-aea) keep=1 ;;
  -j) jobs=${2:?-j N}; shift ;;
  --only) only=${2:?--only ROLES}; shift ;;
  --source) source=${2:?--source URL|FILE}; shift ;;
  -*) die "unknown option $1" ;;
  *) [ -z "$profile" ] || die "one profile at a time"; profile=$1 ;;
  esac
  shift
done
[ -n "$profile" ] || die "usage: scripts/fetch/fetch.sh [--list] [-j N] [--only ROLES] [--keep-aea] [--source S] PROFILE"
[ -f "$profile" ] || die "no profile $profile"

oneios_tool "$ROOT" || die "the image tool is oneios-tool (Rust): cargo build --release --manifest-path tools/oneios-tool/Cargo.toml"
tool() { "$ONEIOS_TOOL" image "$@"; }
command -v python3 >/dev/null || die "missing tool: python3 (3.11 or newer)"
python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' || die "missing tool: python3 3.11 or newer (this one is $(python3 -V 2>&1))"

if [ "$list" = 1 ]; then
  exec "$ONEIOS_TOOL" image check-ipsw "$profile" ${source:+--source "$source"}
fi

eval "$(tool profile "$profile" --shell)"
IN=${INPUTS:-$ROOT/inputs/$P_BUILD}
IPSW=${IPSW:-$ROOT/out/bin/ipsw}
[ -x "$IPSW" ] || IPSW=$(command -v ipsw || true)

# which AEA members this run decrypts: the ipsw CLI is needed only for those
aea_roles=()
while IFS=$'\t' read -r role member name aea size out; do
  case ",$only," in ,,|*,"$role",*) ;; *) continue ;; esac
  [ "$aea" = 1 ] && [ ! -f "$IN/$name" ] && aea_roles+=("$role")
done < <(tool profile "$profile" --members)
if [ ${#aea_roles[@]} -gt 0 ] && [ -z "$IPSW" ]; then
  die "missing tool: ipsw (run scripts/fetch/get-ipsw-cli.sh; it decrypts the AEA images)"
fi

mkdir -p "$IN"
if [ ${#aea_roles[@]} -gt 0 ] && [ -z "$only" ]; then
  free=$(df -Pk "$IN" | awk 'NR==2 {print int($4 / 1048576)}')
  [ "$free" -ge "$P_NEED_GIB" ] || die "$free GiB free in $IN, the profile asks for $P_NEED_GIB GiB: not started"
fi

tool fetch "$profile" "$IN" -j "$jobs" ${only:+--only "$only"} ${source:+--source "$source"}

while IFS=$'\t' read -r role member name aea size out; do
  case ",$only," in ,,|*,"$role",*) ;; *) continue ;; esac
  [ "$aea" = 1 ] || continue
  enc=$IN/$(basename "$member")
  [ -f "$enc" ] || continue                    # already decrypted by an earlier run
  tool aea-info "$enc" >/dev/null || die "$enc: no key-service field in its header"
  echo "$(basename "$enc"): fetching its key from Apple and decrypting"
  key=$("$IPSW" fw aea --key "$enc" | tail -n 1 | awk '{print $NF}' | sed 's/^base64://')
  [ -n "$key" ] || die "$enc: no key from Apple's key service"
  rm -rf "$IN/.aea-out"; mkdir -p "$IN/.aea-out"
  "$IPSW" fw aea --key-val "$key" -o "$IN/.aea-out" "$enc" >/dev/null || die "$enc: decryption failed"
  unset key
  [ -f "$IN/.aea-out/$name" ] || die "$enc: the decrypted image is not where ipsw was told to put it"
  mv "$IN/.aea-out/$name" "$IN/$name"
  rmdir "$IN/.aea-out"
  tool record-plain "$IN" "$role" "$IN/$name"
  [ "$keep" = 1 ] || rm -f "$enc"
done < <(tool profile "$profile" --members)

( cd "$IN" && find . -maxdepth 1 -type f ! -name SHA256SUMS ! -name 'inputs.json*' ! -name '*.part*' -print0 |
  sort -z | xargs -0 python3 -I -c '
import hashlib, sys
for p in sys.argv[1:]:
    h = hashlib.sha256()
    with open(p, "rb") as f:
        while b := f.read(1 << 22):
            h.update(b)
    print(h.hexdigest() + "  " + p[2:])' > SHA256SUMS )
ls -l "$IN"
echo "inputs of $P_NAME are in $IN (hashes: SHA256SUMS, inputs.json). Next: scripts/image/build.sh $profile"
