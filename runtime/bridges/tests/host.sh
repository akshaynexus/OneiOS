#!/bin/bash
# Host tests of the bridges (CI runs every executable */tests/host.sh): the unit tests, and the
# Android build when the NDK is there. No phone and no Apple files.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
make test
NDK=${NDK:-$HOME/Android/Sdk/ndk/28.2.13676358}
if [ -x "$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android30-clang" ]; then
  make NDK="$NDK"
  echo "bridges: Android build ok"
else
  echo "bridges: no NDK at $NDK: Android build skipped"
fi
