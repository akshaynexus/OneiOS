#!/bin/bash
# Build the capture plug-in for iOS 27 (arm64e): one dylib, H9ISP.mediacapture, signed ad hoc with 4 KiB pages.
#
#   userland/camera/build.sh [OUT_DIR]        default out/userland/camera (git-ignored)
#
# Output: OUT_DIR/H9ISP.mediacapture and OUT_DIR/cdhashes.txt (its CDHash for the owner trust module). The image
# hook's camera-plugin tweak places it in the Data seed as /private/var/oneios/addons/MediaCapture/H9ISP.mediacapture
# (`roottweak apply --camera-plugin OUT_DIR`); hosted patch 0056 shows that folder over /System/Library/MediaCapture,
# where CMCapture 27 loads it for the kernel's AppleH9CamIn (README.md).
#
# Toolchain: scripts/ios (docs/building.md section 9), as userland/helpers/build.sh: ios-clang, then ios-sign.py
# (byte for byte `codesign --force --sign - --pagesize 4096`). macOS: Xcode's clang, ld and iPhoneOS SDK; Linux:
# the cross toolchain. Linked against the guest's own iOS 27 stubs (out/ios27-tbd) when they are there.
# No entitlements: a library takes the process's.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
TARGET=${ONEIOS_IOS_TARGET:-arm64e-apple-ios27.0}
OUT=${1:-$REPO/out/userland/camera}
NAME=H9ISP.mediacapture
mkdir -p "$OUT"
. "$REPO/scripts/ios/env.sh"
echo "camera: SDK $IOS_SDK_VERSION ($IOS_SDK); stubs $([ -f "$IOS_TBD/usr/lib/libSystem.B.tbd" ] && echo "$IOS_TBD" || echo "none: the SDK's own"); $("$IOS_CLANG" --version | head -1)"
"$IOS_DIR/ios-clang" -target "$TARGET" -O2 -Wall -Wextra -Werror -DONEIOS_CAMERA_IOKIT -I"$HERE" -I"$REPO/drivers/camera" \
  -dynamiclib -install_name "/System/Library/MediaCapture/$NAME" \
  "$HERE/OneiOSCamera.c" "$HERE/OneiOSCaptureDevice.c" "$HERE/oneios_camera_reader.c" \
  -framework CoreFoundation -framework CoreMedia -framework CoreVideo -framework IOKit -o "$OUT/$NAME"
"$IOS_DIR/ios-sign.py" --identifier com.oneios.H9ISP "$OUT/$NAME" >/dev/null
python3 -I "$HERE/../tweak" cdhash "$OUT/$NAME" | tee "$OUT/cdhashes.txt"
grep -q 'arm64e pages=4096' "$OUT/cdhashes.txt" || { echo "build.sh: $NAME is not arm64e with 4 KiB signature pages" >&2; exit 1; }
nm -gU "$OUT/$NAME" | grep -q ' _H9ISPCaptureDeviceCreate$' || { echo "build.sh: $NAME does not export H9ISPCaptureDeviceCreate" >&2; exit 1; }
echo "built $OUT/$NAME"
