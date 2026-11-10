#!/bin/bash
# Build xnuhost for arm64 Android and leave it in STAGE/bin (runtime/package.sh calls this).
#   runtime/build.sh STAGE        build and stage bin/xnuhost (static; the task stub is inside it) and the
#                                 bridges' helpers bin/oneios-audio and bin/oneios-net.sh
#   runtime/build.sh --test       host unit tests of the loader and of xnuhost, then the Android binary
#                                 booting the test kernel under qemu-aarch64 when that is installed
# NDK: $NDK, else $ANDROID_NDK_HOME, else $HOME/Android/Sdk/ndk/28.2.13676358.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
NDK=${NDK:-${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/28.2.13676358}}
[ -x "$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android30-clang" ] || { echo "build: no NDK at $NDK" >&2; exit 1; }
if [ "${1:-}" = --test ]; then
  make -s -C "$ROOT/runtime/loader" NDK="$NDK" test android
  make -s -C "$ROOT/runtime/xnuhost" NDK="$NDK" test
  exit 0
fi
STAGE=${1:?usage: runtime/build.sh STAGE | --test}
make -s -C "$ROOT/runtime/xnuhost" NDK="$NDK" build/android/xnuhost
mkdir -p "$STAGE/bin"
"$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip" -o "$STAGE/bin/xnuhost" "$ROOT/runtime/xnuhost/build/android/xnuhost"
make -s -C "$ROOT/runtime/bridges" NDK="$NDK" build/android/oneios-audio
"$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip" -o "$STAGE/bin/oneios-audio" "$ROOT/runtime/bridges/build/android/oneios-audio"
cp "$ROOT/runtime/bridges/net/oneios-net.sh" "$STAGE/bin/oneios-net.sh"
chmod 0755 "$STAGE/bin/xnuhost" "$STAGE/bin/oneios-audio" "$STAGE/bin/oneios-net.sh"
echo "build: $STAGE/bin/xnuhost ($(stat -c %s "$STAGE/bin/xnuhost") bytes)"
