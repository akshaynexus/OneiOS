#!/bin/bash
# Build the capture plug-in: one dylib, H9ISP.mediacapture, signed ad hoc with 4 KiB pages. The target is env.sh's
# IOS_TARGET (iOS 27 arm64e by default); IOS_TBD=out/ios16-tbd IOS_MIN=16.5 IOS_TARGET=arm64-apple-ios16.5 builds it
# for a 16.5 guest (the iPhone X image), and ONEIOS_IOS_TARGET overrides the target alone.
#
#   userland/camera/build.sh [OUT_DIR]        default out/userland/camera (git-ignored)
#
# Output: OUT_DIR/H9ISP.mediacapture, OUT_DIR/cdhashes.txt (its CDHash for the owner trust module) and
# OUT_DIR/H9ISP.gpuaddon with H9ISP.gpuaddon.cdhashes (the boot addon of hosted 0090: `oneios-tool phone addon` stages it
# and the CDHash, so a rebuilt plug-in reaches an existing Data store without a new seed). The image
# hook's camera-plugin tweak places it in the Data seed as /private/var/oneios/addons/MediaCapture/H9ISP.mediacapture
# (`oneios-tool image tweaks apply --camera-plugin OUT_DIR`); hosted patch 0056 shows that folder over /System/Library/MediaCapture,
# where CMCapture 27 loads it for the kernel's AppleH9CamIn (README.md).
#
# Toolchain: scripts/ios (docs/building.md section 9), as userland/helpers/build.sh: ios-clang, then `oneios-tool ios sign`
# (byte for byte `codesign --force --sign - --pagesize 4096`). macOS: Xcode's clang, ld and iPhoneOS SDK; Linux:
# the cross toolchain. Linked against the guest's own iOS 27 stubs (out/ios27-tbd) when they are there.
# No entitlements: a library takes the process's.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
OUT=${1:-$REPO/out/userland/camera}
NAME=H9ISP.mediacapture
mkdir -p "$OUT"
. "$REPO/scripts/ios/env.sh"
# The target the toolchain was set up for (env.sh's IOS_TARGET: arm64e-apple-ios27.0 by default, arm64-apple-ios16.5
# for the iPhone X guest) unless the caller names one.
TARGET=${ONEIOS_IOS_TARGET:-$IOS_TARGET}
case "$TARGET" in
  arm64e-*) ARCH=arm64e ;;
  arm64-*) ARCH=arm64 ;;
  *) echo "camera: $TARGET: not an arm64 or arm64e iOS target" >&2; exit 2 ;;
esac
. "$REPO/scripts/lib/oneios-tool.sh"
oneios_tool "$REPO"
echo "camera: SDK $IOS_SDK_VERSION ($IOS_SDK); stubs $([ -f "$IOS_TBD/usr/lib/libSystem.B.tbd" ] && echo "$IOS_TBD" || echo "none: the SDK's own"); $("$IOS_CLANG" --version | head -1)"
"$IOS_DIR/ios-clang" -target "$TARGET" -O2 -Wall -Wextra -Werror -DONEIOS_CAMERA_IOKIT -I"$HERE" -I"$REPO/drivers/camera" \
  -dynamiclib -install_name "/System/Library/MediaCapture/$NAME" \
  "$HERE/OneiOSCamera.c" "$HERE/OneiOSCaptureDevice.c" "$HERE/oneios_camera_formats.c" "$HERE/oneios_camera_scale.c" "$HERE/oneios_camera_reader.c" \
  -framework CoreFoundation -framework CoreMedia -framework CoreVideo -framework IOKit -o "$OUT/$NAME"
"$ONEIOS_TOOL" ios sign --identifier com.oneios.H9ISP "$OUT/$NAME" >/dev/null
"$ONEIOS_TOOL" image tweaks cdhash "$OUT/$NAME" | tee "$OUT/cdhashes.txt"
grep -q "$ARCH pages=4096" "$OUT/cdhashes.txt" || { echo "build.sh: $NAME is not $ARCH with 4 KiB signature pages" >&2; exit 1; }
# (cctools' nm beside the linker, else llvm-nm: a Linux host's own nm does not read Mach-O)
NM=$IOS_CCTOOLS/nm; [ -x "$NM" ] || NM=llvm-nm
"$NM" -gU "$OUT/$NAME" | grep -q ' _H9ISPCaptureDeviceCreate$' || { echo "build.sh: $NAME does not export H9ISPCaptureDeviceCreate" >&2; exit 1; }
# The boot addon of the plug-in alone (hosted 0090): staged as kernel.kc.gpuaddon (oneios-tool phone addon), the kernel
# writes it into an existing Data store at the next boot; with the Metal plug-in too: oneios-tool ios addon OUT --metal
# out/userland/NativeAdrenoMetal.bundle --camera $OUT.
"$ONEIOS_TOOL" ios addon "$OUT/H9ISP.gpuaddon" --camera "$OUT"
echo "built $OUT/$NAME"
