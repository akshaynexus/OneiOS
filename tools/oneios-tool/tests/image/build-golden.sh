#!/bin/bash
# tools/oneios-tool/tests/image/build-golden.sh   the whole image build (scripts/image/build.sh, which runs
# oneios-tool image) against the set the Python tool built: every file of the set by name, size and SHA-256.
#
#   tools/oneios-tool/tests/image/build-golden.sh --kernel FILE [BUILD.SH OPTIONS...]
#   tools/oneios-tool/tests/image/build-golden.sh --no-kernel [BUILD.SH OPTIONS...]
#
# The set is built with --no-pack into WORK (default out/rustport-image-build; about 18 GB) and compared with
# golden/set-BUILD-KERNEL[-OPTIONS].txt, where KERNEL is the first 16 hex digits of the kernel.kc's SHA-256 (or
# "nokernel") and OPTIONS the other options joined ("--no-hook" -> "nohook"). The goldens were written from sets
# that scripts/image/build.sh built with IMAGETOOL_IMPL=python (scripts/image/imagetool.py) at commit a083125,
# the last with that switch. They hold names, sizes and hashes only. Without a golden for the inputs the script
# exits 3; UPDATE=1 writes one from this run (a record, not a comparison). KEEP=1 keeps the built set.
#
# Env: INPUTS (the IPSW members; default inputs/<build> of this checkout or of ONEIOS_MAIN), PROFILE (default
# profiles/ios-27.0.1-iPhone18,2.toml), WORK, ONEIOS_TOOL. Needs what build.sh needs (apfs-fuse on Linux).
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
profile=${PROFILE:-$root/profiles/ios-27.0.1-iPhone18,2.toml}
work=${WORK:-$root/out/rustport-image-build}
[ $# -ge 1 ] || { sed -n '2,8p' "$0" >&2; exit 2; }
key=nokernel
[ "$1" != --kernel ] || key=$(sha256sum < "$2" | cut -c1-16)
opts=("$@"); extra=""
for o in "${opts[@]:$([ "$1" = --kernel ] && echo 2 || echo 1)}"; do extra="$extra-$(echo "$o" | tr -cd 'a-zA-Z0-9')"; done
. "$root/scripts/lib/oneios-tool.sh"; oneios_tool "$root" || exit 2
eval "$("$ONEIOS_TOOL" image profile "$profile" --shell)"
inputs=${INPUTS:-$root/inputs/$P_BUILD}
[ -d "$inputs" ] || inputs=${ONEIOS_MAIN:-${root%%/.claude/worktrees/*}}/inputs/$P_BUILD
golden=$here/golden/set-$P_BUILD-$key$extra.txt
rm -rf "$work"; mkdir -p "$work"
TIMEFORMAT='build.sh: %R s'
time env INPUTS="$inputs" OUT="$work" "$root/scripts/image/build.sh" --no-pack "$@" "$profile" > "$work/build.out" 2> "$work/build.err" \
  || { echo "build.sh failed:"; tail -5 "$work/build.err"; exit 1; }
tail -2 "$work/build.out"
( cd "$work/$P_BUILD" && find . -type f | sort | while read -r f; do printf '%s %s %s\n' "$(sha256sum < "$f" | cut -c1-64)" "$(stat -c %s "$f")" "${f#./}"; done ) > "$work/found.txt"
[ -n "${KEEP:-}" ] || rm -rf "$work/$P_BUILD" "$work/.work" 2>/dev/null
if [ -n "${UPDATE:-}" ]; then mkdir -p "$here/golden"; cp "$work/found.txt" "$golden"; echo "wrote $golden (this run's set, not a comparison)"; exit 0; fi
[ -f "$golden" ] || { echo "image build: no golden for these inputs ($golden)"; exit 3; }
if diff "$golden" "$work/found.txt" > "$work/golden.diff"; then
  echo "image build: $(wc -l < "$golden") files the same as the Python tool's set ($(basename "$golden"))"
else
  echo "image build: FAILED against $(basename "$golden")"; sed 's/^/  /' "$work/golden.diff" | head -30; exit 1
fi
