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
# Tools: oneios-tool (tools/oneios-tool, Rust: `image profile`, `image im4p` with LZFSE decoded in Rust, `prelink pin`;
# built when missing or stale, ONEIOS_TOOL=PATH for another, IM4P_TOOL=PATH for the decoder alone). It replaced
# scripts/image/imagetool.py here on 2026-11-13 (which started the `lzfse` program for the payload: the same files
# and the same line); that script is still in the tree, unused.
# The decoded kernelcache must have the SHA-256 the kernel profile pins (kernel/unixnu/profiles/os/*-<build>/
# profile.toml, [kernel.prelink] kernelcache_sha256) when there is such a profile: a mismatch stops the script.
# Linux and macOS. Never in git.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
die() { echo "kernel-inputs: $*" >&2; exit 1; }
[ $# = 1 ] && [ -f "$1" ] || die "usage: scripts/image/kernel-inputs.sh PROFILE"
. "$ROOT/scripts/lib/oneios-tool.sh"
oneios_tool "$ROOT" || die "the image tool is oneios-tool (Rust): cargo build --release --manifest-path tools/oneios-tool/Cargo.toml"
tool() { "$ONEIOS_TOOL" image "$@"; }
# The IM4P decoder: IM4P_TOOL when it is given, else the same binary.
im4p=("${IM4P_TOOL:-$ONEIOS_TOOL}" image im4p)
[ -x "${im4p[0]}" ] || die "IM4P_TOOL=${IM4P_TOOL:-} is not a program"
eval "$(tool profile "$1" --shell)"
O=$ROOT/out/kernel-inputs/$P_BUILD
IN=${INPUTS:-$ROOT/inputs/$P_BUILD}
have_members() {
  local role member name aea size out
  while IFS=$'\t' read -r role member name aea size out; do
    case "$role" in kernelcache|devicetree) [ -f "$1/$name" ] || return 1 ;; esac
  done < <(tool profile "$PROFILE" --members)
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
done < <(tool profile "$1" --members)
( cd "$O" && { sha256sum kernelcache.macho devicetree.bin 2>/dev/null || shasum -a 256 kernelcache.macho devicetree.bin; } > SHA256SUMS && cat SHA256SUMS )
# The pin: whichever decoder ran, the kernelcache is the one the kernel profile names.
for pinned in "$ROOT"/kernel/unixnu/profiles/os/*-"$P_BUILD"/profile.toml; do
  [ -f "$pinned" ] || continue
  want=$("$ONEIOS_TOOL" prelink pin "$pinned" 2>/dev/null || true)
  [ -n "$want" ] || continue
  got=$(cut -d' ' -f1 < <(grep ' kernelcache.macho$' "$O/SHA256SUMS"))
  [ "$got" = "$want" ] || die "$O/kernelcache.macho has sha256 $got, $pinned pins $want"
  echo "kernel-inputs: kernelcache.macho is the one $(basename "$(dirname "$pinned")") pins"
done
