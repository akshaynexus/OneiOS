#!/usr/bin/env bash
# After a verified artifact upload, remove Apple-derived inputs while keeping reusable source/toolchain outputs.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
WORKSPACE=$(realpath -e "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is unset}")
EXPECTED_CACHE=$WORKSPACE/.oneios-image-cache
CACHE_ROOT=$(realpath -e "${ONEIOS_CACHE_ROOT:?image cache was not prepared}")
INPUT_CACHE=$(realpath -e "${ONEIOS_INPUT_CACHE_DIR:?input cache was not prepared}")
BUILD_CACHE=$(realpath -e "${ONEIOS_BUILD_CACHE_DIR:?build cache was not prepared}")
INPUT_SUFFIX=${ONEIOS_INPUT_CACHE_SUFFIX:?input suffix was not prepared}
KERNEL_SUFFIX=${ONEIOS_KERNEL_CACHE_SUFFIX:?kernel suffix was not prepared}
IMAGE_SUFFIX=${ONEIOS_IMAGE_SUFFIX:?image suffix was not prepared}
BUILD=24A446

case "$ROOT/" in "$WORKSPACE"/*/) ;; *) echo "cleanup-image-set: checkout is outside GITHUB_WORKSPACE" >&2; exit 2 ;; esac
[ "$CACHE_ROOT" = "$EXPECTED_CACHE" ] || { echo "cleanup-image-set: unexpected cache root $CACHE_ROOT" >&2; exit 2; }
case "$CACHE_ROOT/" in "$ROOT/"*) echo "cleanup-image-set: refusing to clean a cache inside the checkout" >&2; exit 2 ;; esac

case "$INPUT_CACHE" in "$CACHE_ROOT"/inputs-*) ;; *) echo "cleanup-image-set: unexpected input cache $INPUT_CACHE" >&2; exit 2 ;; esac
case "$BUILD_CACHE" in "$CACHE_ROOT"/build-*) ;; *) echo "cleanup-image-set: unexpected build cache $BUILD_CACHE" >&2; exit 2 ;; esac
[[ "$INPUT_SUFFIX" =~ ^ios-24A446-[a-f0-9]{64}$ ]] || { echo "cleanup-image-set: invalid input suffix" >&2; exit 2; }
[[ "$KERNEL_SUFFIX" =~ ^kernel-[a-f0-9]{64}$ ]] || { echo "cleanup-image-set: invalid kernel suffix" >&2; exit 2; }
[[ "$IMAGE_SUFFIX" =~ ^ci-[0-9]+-[0-9]+$ ]] || { echo "cleanup-image-set: invalid image suffix" >&2; exit 2; }
[ "$INPUT_CACHE" = "$CACHE_ROOT/inputs-${INPUT_SUFFIX#ios-24A446-}" ] || { echo "cleanup-image-set: input cache does not match its suffix" >&2; exit 2; }
[ "$BUILD_CACHE" = "$CACHE_ROOT/build-${KERNEL_SUFFIX#kernel-}" ] || { echo "cleanup-image-set: build cache does not match its suffix" >&2; exit 2; }

check_link() {
  local name=$1 expected=$2 actual
  [ -L "$ROOT/$name" ] || { echo "cleanup-image-set: expected $ROOT/$name to be a cache symlink" >&2; exit 2; }
  actual=$(realpath -e "$ROOT/$name")
  [ "$actual" = "$expected" ] || {
    echo "cleanup-image-set: $ROOT/$name points to $actual, expected $expected" >&2
    exit 2
  }
}

check_link inputs "$INPUT_CACHE/inputs"
check_link out "$BUILD_CACHE/out"

# Remove only this successful attempt's verified Apple-derived files. Preserve the
# hosted-kernel incremental tree, external source checkouts, and Cargo target cache.
rm -rf -- \
  "$BUILD_CACHE/out/kernel-inputs/$BUILD" \
  "$BUILD_CACHE/out/kernel/prelink-$KERNEL_SUFFIX" \
  "$BUILD_CACHE/out/image-$IMAGE_SUFFIX"

rm -- "$ROOT/inputs"
rm -rf -- "$INPUT_CACHE"
echo "cleanup-image-set: removed verified Apple-derived inputs and image staging after upload"
