#!/usr/bin/env bash
# Reattach private, runner-local inputs and incremental build outputs after checkout.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
WORKSPACE=$(realpath -e "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is unset}")
CACHE_ROOT=$WORKSPACE/.oneios-image-cache
PROFILE=profiles/ios-27.0.1-iPhone18,2.toml
BUILD=24A446

case "$ROOT/" in
  "$WORKSPACE"/*/) ;;
  *) echo "prepare-image-cache: checkout is outside GITHUB_WORKSPACE" >&2; exit 2 ;;
esac
case "$CACHE_ROOT/" in
  "$ROOT/"*) echo "prepare-image-cache: cache must be outside the checkout" >&2; exit 2 ;;
esac

mkdir -p "$CACHE_ROOT"
chmod 700 "$CACHE_ROOT"

tracked_hash() {
  git -C "$ROOT" ls-files -z -- "$@" |
    LC_ALL=C sort -z |
    xargs -0 -r sha256sum |
    sha256sum | awk '{print $1}'
}

# The profile pins the exact restore members. Its hash keeps those verified downloads
# reusable across workflow-only changes and new run attempts.
INPUT_KEY=$(sha256sum "$ROOT/$PROFILE" | awk '{print $1}')
REF_KEY=$(tracked_hash scripts/setup/get-references.sh kernel/pins.sh)
CLANG_ID=$(clang --version | head -n 1)
LLD_ID=$(ld.lld --version | head -n 1)
RUST_ID=$(rustc --version)
BUILD_KEY=$(
  {
    printf 'schema=1\nbuild=%s\nprofile=%s\narch=%s\nclang=%s\nlld=%s\nrust=%s\n' \
      "$BUILD" "$INPUT_KEY" "$(uname -m)" "$CLANG_ID" "$LLD_ID" "$RUST_ID"
    tracked_hash kernel tools/oneios-tool scripts/lib/oneios-tool.sh scripts/setup/get-references.sh
  } | sha256sum | awk '{print $1}'
)

INPUT_CACHE=$CACHE_ROOT/inputs-$INPUT_KEY
BUILD_CACHE=$CACHE_ROOT/build-$BUILD_KEY
REF_CACHE=$CACHE_ROOT/references-$REF_KEY
CARGO_CACHE=$CACHE_ROOT/cargo-target
INPUT_SUFFIX=ios-$BUILD-$INPUT_KEY
KERNEL_SUFFIX=kernel-$BUILD_KEY
IMAGE_SUFFIX=ci-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-1}
INPUT_MANIFEST=$INPUT_CACHE/inputs/$INPUT_SUFFIX/SHA256SUMS
HOSTED_TREE=$BUILD_CACHE/out/kernel-hosted-$KERNEL_SUFFIX
CACHE_RESUME=false
if [ -s "$INPUT_MANIFEST" ] && [ -d "$HOSTED_TREE" ]; then
  CACHE_RESUME=true
fi

mkdir -p "$INPUT_CACHE/inputs" "$BUILD_CACHE/out" "$REF_CACHE/external" "$CARGO_CACHE"
chmod 700 "$INPUT_CACHE" "$BUILD_CACHE" "$REF_CACHE" "$CARGO_CACHE"

link_cache() {
  local name=$1 target=$2 link=$ROOT/$1 expected actual
  expected=$(realpath -m "$target")
  if [ -L "$link" ]; then
    actual=$(realpath -m "$link")
    [ "$actual" = "$expected" ] || {
      echo "prepare-image-cache: $link points to $actual, expected $expected" >&2
      exit 2
    }
  elif [ -e "$link" ]; then
    echo "prepare-image-cache: refusing to replace existing $link" >&2
    exit 2
  else
    ln -s "$expected" "$link"
  fi
}

link_cache inputs "$INPUT_CACHE/inputs"
link_cache out "$BUILD_CACHE/out"
link_cache external "$REF_CACHE/external"
touch "$INPUT_CACHE" "$BUILD_CACHE" "$REF_CACHE" "$CARGO_CACHE"

# Keep failed/cancelled attempts available for a week, then prune only named cache
# directories directly under this dedicated private cache root.
for old in "$CACHE_ROOT"/inputs-* "$CACHE_ROOT"/build-* "$CACHE_ROOT"/references-* "$CARGO_CACHE"; do
  [ -e "$old" ] || continue
  case "$old" in
    "$CACHE_ROOT"/inputs-*|"$CACHE_ROOT"/build-*|"$CACHE_ROOT"/references-*|"$CARGO_CACHE") ;;
    *) echo "prepare-image-cache: refusing to prune unexpected path $old" >&2; exit 2 ;;
  esac
  [ "$old" = "$INPUT_CACHE" ] || [ "$old" = "$BUILD_CACHE" ] || [ "$old" = "$REF_CACHE" ] || [ "$old" = "$CARGO_CACHE" ] || {
    if find "$old" -maxdepth 0 -mmin +10080 -print -quit | grep -q .; then
      rm -rf -- "$old"
      echo "prepare-image-cache: pruned expired cache $old"
    fi
  }
done

{
  echo "ONEIOS_CACHE_ROOT=$CACHE_ROOT"
  echo "ONEIOS_INPUT_CACHE_DIR=$INPUT_CACHE"
  echo "ONEIOS_BUILD_CACHE_DIR=$BUILD_CACHE"
  echo "ONEIOS_REFERENCE_CACHE_DIR=$REF_CACHE"
  echo "ONEIOS_INPUT_CACHE_SUFFIX=$INPUT_SUFFIX"
  echo "ONEIOS_KERNEL_CACHE_SUFFIX=$KERNEL_SUFFIX"
  echo "ONEIOS_IMAGE_SUFFIX=$IMAGE_SUFFIX"
  echo "ONEIOS_IMAGE_CACHE_RESUME=$CACHE_RESUME"
  echo "CARGO_TARGET_DIR=$CARGO_CACHE"
} >> "${GITHUB_ENV:?GITHUB_ENV is unset}"

echo "prepare-image-cache: attached private input, source, and incremental build caches"
echo "prepare-image-cache: cache keys inputs=$INPUT_KEY build=$BUILD_KEY refs=$REF_KEY"
echo "prepare-image-cache: full resume cache available=$CACHE_RESUME"
echo "prepare-image-cache: failed/cancelled caches are retained for up to 7 days"
