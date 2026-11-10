#!/bin/bash
# Make out/kernel/series/xnu: xnu at the pin with the whole series applied, one git commit per patch, for
# writing and checking patches without a build (the build applies the same list: UniXNU's driver, patches.py).
#   kernel/series-tree.sh            build the tree; prints one line per patch, exit 1 on the first that fails
#   kernel/series-tree.sh --list     only print the resolved patch list, in order
# A new patch: edit files in the tree, `git -C out/kernel/series/xnu diff > kernel/device/patches/xnu-12377/NAME.patch`,
# add NAME.patch to kernel/device/patches/xnu-12377/series, run this again.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
. "$ROOT/kernel/lib/stage.sh"
ARCH_SERIES=$UNIXNU/profiles/arch/arm64e/patches/xnu/series
OS_SERIES=$ROOT/kernel/unixnu/profiles/os/$OS_PROFILE/patches/xnu/series
DEVICE_SERIES=$DEVICE/patches/xnu-12377/series
[ -d "$UNIXNU" ] || "$ROOT/kernel/fetch.sh"
# Entries as UniXNU's patches.plan reads them: "arch:REL" is relative to profiles/arch/arm64e, anything else to the series file.
resolve() { # SERIES
  local line dir
  dir=$(dirname "$1")
  while IFS= read -r line; do
    line=${line%%#*}; line=$(echo "$line" | tr -d '[:space:]')
    [ -n "$line" ] || continue
    case "$line" in
      arch:*) realpath "$UNIXNU/profiles/arch/arm64e/${line#arch:}" ;;
      *) realpath "$dir/$line" ;;
    esac
  done < "$1"
}
LIST=$( { resolve "$ARCH_SERIES"; resolve "$OS_SERIES"; resolve "$DEVICE_SERIES"; } )
if [ "${1:-}" = --list ]; then printf '%s\n' "$LIST"; exit 0; fi
W=${KERNEL_OUT:-$ROOT/out/kernel}/series
mkdir -p "$W"
if [ ! -d "$W/xnu-pristine/.git" ]; then
  mirror=$(ls -d "$UNIXNU_OUT_DIR"/feeds/xnu-* 2>/dev/null | head -1 || true)
  if [ -n "$mirror" ] && [ "$(git -C "$mirror" rev-parse HEAD)" = "$XNU_COMMIT" ]; then
    git -c advice.detachedHead=false clone -q --shared "$mirror" "$W/xnu-pristine"
  else
    git -c advice.detachedHead=false clone -q --depth 1 --branch "$XNU_TAG" https://github.com/apple-oss-distributions/xnu.git "$W/xnu-pristine"
  fi
fi
[ "$(git -C "$W/xnu-pristine" rev-parse HEAD)" = "$XNU_COMMIT" ] || { echo "series-tree: $W/xnu-pristine is not $XNU_COMMIT" >&2; exit 1; }
rm -rf "$W/xnu"
git -c advice.detachedHead=false clone -q --shared "$W/xnu-pristine" "$W/xnu"
cd "$W/xnu"
git checkout -q --detach "$XNU_COMMIT"
n=0
while IFS= read -r patch; do
  if ! git apply --index "$patch" 2>"$W/apply.err"; then
    echo "FAIL $(basename "$patch"): $(grep -m1 -E 'patch failed|does not exist|No such file|already exists' "$W/apply.err" || head -1 "$W/apply.err")"
    echo "series-tree: stopped after $n patches" >&2
    exit 1
  fi
  git -c user.name=series -c user.email=series@localhost commit -q --no-verify -m "$(basename "$patch")"
  n=$((n + 1))
  echo "ok   $(basename "$patch")"
done <<< "$LIST"
echo "series-tree: $n patches applied: $W/xnu"
