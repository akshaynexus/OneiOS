#!/bin/bash
# Unpack the kernel build's inputs from inputs/<build>/ (scripts/fetch/fetch.sh): Apple's kernelcache as a
# Mach-O (its kexts are prelinked into OneiOS's kernel collection) and the device tree.
#
#   scripts/image/kernel-inputs.sh PROFILE      ->  out/kernel-inputs/<build>/kernelcache.macho, devicetree.bin,
#                                                   SHA256SUMS (the kernel profile pins the kernelcache by it)
# The members are read from inputs/<build>/ (the full fetch) when they are there. Otherwise only the three the kernel
# needs (kernelcache, device tree, BuildManifest; about 24 MB) are fetched with HTTP range requests into
# out/kernel-inputs/<build>/ipsw/ (scripts/fetch/fetch.sh --only), so the kernel needs no 10.6 GiB download.
# INPUTS=DIR reads the members from DIR instead (and fetches into it when they are missing).
# Tools: python3 3.11+, and for the kernelcache's LZFSE payload either the `lzfse` program (imagetool.py im4p starts
# it; used when it is on PATH) or oneios-tool (`oneios-tool image im4p`, LZFSE in Rust: used when there is no lzfse;
# built with cargo build --release --manifest-path tools/oneios-tool/Cargo.toml). IM4P_TOOL=PATH names a oneios-tool
# binary and makes it the decoder whether lzfse is there or not. Both write the same files and print the same line.
# The decoded kernelcache must have the SHA-256 the kernel profile pins (kernel/unixnu/profiles/os/*-<build>/
# profile.toml, [kernel.prelink] kernelcache_sha256) when there is such a profile: a mismatch stops the script.
# Linux and macOS. Never in git.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
TOOL=$ROOT/scripts/image/imagetool.py
die() { echo "kernel-inputs: $*" >&2; exit 1; }
[ $# = 1 ] && [ -f "$1" ] || die "usage: scripts/image/kernel-inputs.sh PROFILE"
missing=()
command -v python3 >/dev/null || missing+=("python3 (3.11 or newer)")
# The IM4P decoder: IM4P_TOOL, else imagetool.py with lzfse, else oneios-tool where it is built or on PATH.
im4p=()
if [ -n "${IM4P_TOOL:-}" ]; then
  [ -x "$IM4P_TOOL" ] || die "IM4P_TOOL=$IM4P_TOOL is not a program"
  im4p=("$IM4P_TOOL" image im4p)
elif command -v lzfse >/dev/null; then
  im4p=(python3 -I "$TOOL" im4p)
elif [ -x "$ROOT/tools/oneios-tool/target/release/oneios-tool" ]; then
  im4p=("$ROOT/tools/oneios-tool/target/release/oneios-tool" image im4p)
elif command -v oneios-tool >/dev/null; then
  im4p=(oneios-tool image im4p)
else
  missing+=("lzfse (github.com/lzfse/lzfse), or oneios-tool (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml; IM4P_TOOL=PATH for one elsewhere)")
fi
[ ${#missing[@]} = 0 ] || die "missing tools: ${missing[*]}"
eval "$(python3 -I "$TOOL" profile "$1" --shell)"
O=$ROOT/out/kernel-inputs/$P_BUILD
IN=${INPUTS:-$ROOT/inputs/$P_BUILD}
have_members() {
  local role member name aea size out
  while IFS=$'\t' read -r role member name aea size out; do
    case "$role" in kernelcache|devicetree) [ -f "$1/$name" ] || return 1 ;; esac
  done < <(python3 -I "$TOOL" profile "$PROFILE" --members)
}
PROFILE=$1
if ! have_members "$IN"; then
  [ -n "${INPUTS:-}" ] || IN=$O/ipsw
  have_members "$IN" || INPUTS=$IN "$ROOT/scripts/fetch/fetch.sh" --only kernelcache,devicetree,buildmanifest "$PROFILE"
fi
mkdir -p "$O"
while IFS=$'\t' read -r role member name aea size out; do
  case "$role" in
  kernelcache) dst=kernelcache.macho ;;
  devicetree) dst=devicetree.bin ;;
  *) continue ;;
  esac
  [ -f "$IN/$name" ] || die "missing input $IN/$name (run scripts/fetch/fetch.sh --only kernelcache,devicetree,buildmanifest $1)"
  "${im4p[@]}" "$IN/$name" "$O/$dst"
done < <(python3 -I "$TOOL" profile "$1" --members)
( cd "$O" && { sha256sum kernelcache.macho devicetree.bin 2>/dev/null || shasum -a 256 kernelcache.macho devicetree.bin; } > SHA256SUMS && cat SHA256SUMS )
# The pin: whichever decoder ran, the kernelcache is the one the kernel profile names.
for pinned in "$ROOT"/kernel/unixnu/profiles/os/*-"$P_BUILD"/profile.toml; do
  [ -f "$pinned" ] || continue
  want=$(python3 -I -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1],"rb")).get("kernel",{}).get("prelink",{}).get("kernelcache_sha256",""))' "$pinned")
  [ -n "$want" ] || continue
  got=$(cut -d' ' -f1 < <(grep ' kernelcache.macho$' "$O/SHA256SUMS"))
  [ "$got" = "$want" ] || die "$O/kernelcache.macho has sha256 $got, $pinned pins $want"
  echo "kernel-inputs: kernelcache.macho is the one $(basename "$(dirname "$pinned")") pins"
done
