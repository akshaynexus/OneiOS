#!/usr/bin/env bash
# Remove only this run's Apple-derived inputs and build products from the self-hosted runner.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
case "$ROOT/" in
  "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is unset}"/*) ;;
  *) echo "cleanup-image-set: refusing to clean outside GITHUB_WORKSPACE ($ROOT)" >&2; exit 2 ;;
esac

RUN_SUFFIX="ci-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-1}"
BUILD=24A446
rm -rf -- \
  "$ROOT/inputs/$RUN_SUFFIX" \
  "$ROOT/out/kernel-inputs/$BUILD" \
  "$ROOT/out/kernel/prelink-$RUN_SUFFIX" \
  "$ROOT/out/kernel-hosted-$RUN_SUFFIX" \
  "$ROOT/out/hosted-device-$RUN_SUFFIX" \
  "$ROOT/out/kernel/current-hosted-$RUN_SUFFIX" \
  "$ROOT/out/image-$RUN_SUFFIX"

echo "cleanup-image-set: removed this run's Apple-derived images, kernel collection, and encrypted artifact staging area"
