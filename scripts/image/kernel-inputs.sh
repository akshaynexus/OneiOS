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
# Tools: python3 3.11+, lzfse (the kernelcache payload is LZFSE). Linux and macOS. Never in git.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
TOOL=$ROOT/scripts/image/imagetool.py
die() { echo "kernel-inputs: $*" >&2; exit 1; }
[ $# = 1 ] && [ -f "$1" ] || die "usage: scripts/image/kernel-inputs.sh PROFILE"
missing=()
command -v python3 >/dev/null || missing+=("python3 (3.11 or newer)")
command -v lzfse >/dev/null || missing+=("lzfse (github.com/lzfse/lzfse)")
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
  python3 -I "$TOOL" im4p "$IN/$name" "$O/$dst"
done < <(python3 -I "$TOOL" profile "$1" --members)
( cd "$O" && { sha256sum kernelcache.macho devicetree.bin 2>/dev/null || shasum -a 256 kernelcache.macho devicetree.bin; } > SHA256SUMS && cat SHA256SUMS )
