#!/bin/bash
# Build xnuhost for arm64 Android and leave it in STAGE/bin (runtime/package.sh calls this).
#   runtime/build.sh STAGE        build and stage bin/xnuhost (static; the task stub is inside it) and the
#                                 bridges' helpers bin/oneios-audio and bin/oneios-net.sh, and the display helper
#                                 (runtime/display): bin/oneios-display, bin/liboneios-display.so, lib/oneios-display.dex,
#                                 and the phone checks: bin/gpuctl, bin/bridgectl, bin/brightctl, bin/batteryctl, bin/displayctl, bin/camctl
#   runtime/build.sh --test       host unit tests of the loader and of xnuhost, then the Android binary
#                                 booting the test kernel under qemu-aarch64 when that is installed (on a Mac: the
#                                 whole host suite in an arm64 Linux container, runtime/host-tests.sh)
# Build hosts: Linux x86_64 (the NDK's linux-x86_64 tools) and macOS (darwin-x86_64; runtime/host.mk).
# NDK: $NDK, else $ANDROID_NDK_HOME, else $HOME/Android/Sdk/ndk/28.2.13676358 (Linux) or
#      $HOME/Library/Android/sdk/ndk/28.2.13676358 (macOS).
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
case "$(uname -s)" in
  Darwin) NDK_HOST=darwin-x86_64; NDK_DEFAULT=$HOME/Library/Android/sdk/ndk/28.2.13676358 ;;
  *)      NDK_HOST=linux-x86_64;  NDK_DEFAULT=$HOME/Android/Sdk/ndk/28.2.13676358 ;;
esac
NDK=${NDK:-${ANDROID_NDK_HOME:-$NDK_DEFAULT}}
TOOLS=$NDK/toolchains/llvm/prebuilt/$NDK_HOST/bin
[ -x "$TOOLS/aarch64-linux-android30-clang" ] || { echo "build: no NDK at $NDK ($NDK_HOST)" >&2; exit 1; }
STRIP=$TOOLS/llvm-strip
# Build every make target in parallel (sub-makes inherit MAKEFLAGS). JOBS overrides, as the kernel build.
export MAKEFLAGS="${MAKEFLAGS:--j${JOBS:-$(getconf _NPROCESSORS_ONLN)}}"
if [ "${1:-}" = --test ] && [ "$(uname -s)" = Darwin ]; then
  exec env NDK="$NDK" "$ROOT/runtime/host-tests.sh"
fi
if [ "${1:-}" = --test ]; then
  make -s -C "$ROOT/runtime/loader" NDK="$NDK" test android
  make -s -C "$ROOT/runtime/xnuhost" NDK="$NDK" test
  make -s -C "$ROOT/runtime/display" NDK="$NDK" test all
  exit 0
fi
STAGE=${1:?usage: runtime/build.sh STAGE | --test}
make -s -C "$ROOT/runtime/xnuhost" NDK="$NDK" build/android/xnuhost
mkdir -p "$STAGE/bin"
"$STRIP" -o "$STAGE/bin/xnuhost" "$ROOT/runtime/xnuhost/build/android/xnuhost"
make -s -C "$ROOT/runtime/bridges" NDK="$NDK" build/android/oneios-audio
"$STRIP" -o "$STAGE/bin/oneios-audio" "$ROOT/runtime/bridges/build/android/oneios-audio"
cp "$ROOT/runtime/bridges/net/oneios-net.sh" "$STAGE/bin/oneios-net.sh"
# The camera helper, and camctl: the camera alone, for a check on the phone (runtime/bridges/camera/README.md)
make -s -C "$ROOT/runtime/bridges/camera" NDK="$NDK" build/android/oneios-camera build/android/camctl
"$STRIP" -o "$STAGE/bin/oneios-camera" "$ROOT/runtime/bridges/camera/build/android/oneios-camera"
"$STRIP" -o "$STAGE/bin/camctl" "$ROOT/runtime/bridges/camera/build/android/camctl"
chmod 0755 "$STAGE/bin/oneios-camera" "$STAGE/bin/camctl"
# gpuctl: the GPU path alone, for a check on the phone (runtime/xnuhost/tools/gpuctl.c, docs/gpu.md)
make -s -C "$ROOT/runtime/xnuhost" NDK="$NDK" build/android/gpuctl
"$STRIP" -o "$STAGE/bin/gpuctl" "$ROOT/runtime/xnuhost/build/android/gpuctl"
chmod 0755 "$STAGE/bin/gpuctl"
# The other phone checks, which the OneOS fork's Hardware checks screen runs with One UI up (as root):
# bridgectl (cpu, haptic, haptic-pattern, bt, audio), brightctl (info, levels, als), batteryctl (reads the battery) and displayctl (test; it
# starts bin/oneios-display beside itself). Each folder's README has the commands and what a good run prints.
make -s -C "$ROOT/runtime/bridges" NDK="$NDK" build/android/bridgectl
make -s -C "$ROOT/runtime/bridges/brightness" NDK="$NDK" build/android/brightctl
make -s -C "$ROOT/runtime/bridges/battery" NDK="$NDK" build/android/batteryctl
make -s -C "$ROOT/runtime/display" NDK="$NDK" build/android/displayctl
"$STRIP" -o "$STAGE/bin/bridgectl" "$ROOT/runtime/bridges/build/android/bridgectl"
"$STRIP" -o "$STAGE/bin/brightctl" "$ROOT/runtime/bridges/brightness/build/android/brightctl"
"$STRIP" -o "$STAGE/bin/batteryctl" "$ROOT/runtime/bridges/battery/build/android/batteryctl"
"$STRIP" -o "$STAGE/bin/displayctl" "$ROOT/runtime/display/build/android/displayctl"
chmod 0755 "$STAGE/bin/bridgectl" "$STAGE/bin/brightctl" "$STAGE/bin/batteryctl" "$STAGE/bin/displayctl"
# The display helper: a launcher, the library app_process loads, and the dex with the layer (needs javac and d8).
D=$ROOT/runtime/display/build/android
make -s -C "$ROOT/runtime/display" NDK="$NDK" build/android/oneios-display build/android/liboneios-display.so build/android/oneios-display.dex
mkdir -p "$STAGE/lib"
"$STRIP" -o "$STAGE/bin/oneios-display" "$D/oneios-display"
"$STRIP" -o "$STAGE/bin/liboneios-display.so" "$D/liboneios-display.so"
cp "$D/oneios-display.dex" "$STAGE/lib/oneios-display.dex"
chmod 0644 "$STAGE/lib/oneios-display.dex"
chmod 0755 "$STAGE/bin/xnuhost" "$STAGE/bin/oneios-audio" "$STAGE/bin/oneios-net.sh" "$STAGE/bin/oneios-display" "$STAGE/bin/liboneios-display.so"
echo "build: $STAGE/bin/xnuhost ($(wc -c < "$STAGE/bin/xnuhost" | tr -d ' ') bytes)"
