#!/bin/bash
# Build OneiOSCamera.dylib, the capture plug-in for iOS 27 (arm64e), signed ad hoc with 4 KiB signature pages.
#
#   userland/camera/build.sh [OUT_DIR]        default out/userland/camera (git-ignored)
#
# Toolchain: as userland/helpers/build.sh (its header says where each part comes from).
#   macOS: Xcode with the iPhoneOS SDK. Linux: clang 19 or later, THEOS=DIR (ld64 and ldid), IOS_SDK=DIR.
# NOT RUN on this project's build machine: it has no iPhoneOS SDK. tests/host.sh checks the source's syntax
# against stand-in headers and runs the reader. Add the device-create source (README.md, "Not written") to
# SOURCES when it exists; without it the plug-in loads, reports and replaces nothing.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
TARGET=${ONEIOS_IOS_TARGET:-arm64e-apple-ios27.0}
OUT=${1:-$REPO/out/userland/camera}
SOURCES=("$HERE/OneiOSCamera.c" "$HERE/oneios_camera_reader.c")
FLAGS=(-O2 -Wall -Wextra -Werror -I"$HERE" -I"$REPO/drivers/camera" -dynamiclib -install_name /private/var/oneios/lib/OneiOSCamera.dylib
       -framework CoreFoundation -framework CoreVideo -lobjc)
mkdir -p "$OUT"
case "$(uname -s)" in
Darwin)
  SDK=$(xcrun --sdk iphoneos --show-sdk-path)
  xcrun clang -target "$TARGET" -isysroot "$SDK" "${FLAGS[@]}" "${SOURCES[@]}" -o "$OUT/OneiOSCamera.dylib"
  codesign --force --sign - --pagesize 4096 --identifier com.oneios.OneiOSCamera "$OUT/OneiOSCamera.dylib"
  ;;
Linux)
  TC=${THEOS:?set THEOS to a theos checkout with toolchain/linux/iphone (userland/helpers/build.sh)}/toolchain/linux/iphone/bin
  SDK=${IOS_SDK:?set IOS_SDK to an iPhoneOS SDK directory (userland/helpers/build.sh)}
  [ -x "$TC/ld" ] && [ -x "$TC/ldid" ] || { echo "missing $TC/ld or $TC/ldid" >&2; exit 1; }
  clang -target "$TARGET" -isysroot "$SDK" --ld-path="$TC/ld" -Wno-unused-command-line-argument "${FLAGS[@]}" "${SOURCES[@]}" -o "$OUT/OneiOSCamera.dylib"
  "$TC/ldid" -S -Icom.oneios.OneiOSCamera "$OUT/OneiOSCamera.dylib"
  ;;
*) echo "build.sh: macOS or Linux" >&2; exit 1 ;;
esac
# The CDHash for the kernel's owner trust module (overlay/userland/owner-trust.json).
(cd "$HERE/.." && python3 -I -m roottweak cdhash "$OUT/OneiOSCamera.dylib") | tee "$OUT/cdhashes.txt"
echo "built $OUT/OneiOSCamera.dylib"
