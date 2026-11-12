# The iOS user-space toolchain's settings, for sh. Sourced by ios-clang, ios-ld and the build scripts:
#
#   . scripts/ios/env.sh          (returns 1 and says what is missing when the SDK or ld64 is not there)
#
# macOS: Xcode's own toolchain (xcrun): its clang, its ld and its iPhoneOS SDK. Linux: the host's clang, Apple's ld64
# built for Linux and the SDK xtool extracted from Xcode (a cross toolchain). Either way the link is against IOS_TBD.
# What it sets (each can be given in the environment instead):
#   IOS_SDK      the iPhoneOS SDK (headers, and .tbd stubs as a fallback). Default on macOS:
#                `xcrun --sdk iphoneos --show-sdk-path`. On Linux: the newest iPhoneOS*.sdk that
#                `xtool sdk install Xcode.xip` extracted under ~/.config/swiftpm/swift-sdks/darwin.artifactbundle.
#   IOS_TBD      .tbd stubs made from the guest's own iOS 27 shared cache (scripts/ios/make-tbds.py), laid out like
#                an SDK; the linker looks here before IOS_SDK. Default: out/ios27-tbd. Used when it exists.
#   IOS_LD       the linker, one that reads .tbd stubs. Default on macOS: `xcrun --sdk iphoneos --find ld` (Xcode's).
#                On Linux: Apple's ld64 built with libtapi, out/ios-toolchain/cctools/bin/ld
#                (scripts/setup/ios-toolchain.sh --ld); the kernel's ld64 in out/toolchain cannot link user space.
#   IOS_MIN      the deployment target. Default 27.0.
#   IOS_TARGET   the clang target. Default arm64e-apple-ios$IOS_MIN.
#   IOS_CLANG    the clang to run (clang++ is "${IOS_CLANG}++"). Default on macOS: Xcode's (xcrun --find clang).
#                On Linux: clang.
# Also: IOS_SDK_VERSION, IOS_LD_VERSION (ld64's version; Xcode's ld-prime has none: empty), IOS_CCTOOLS (the folder
# of the linker's sibling tools: strip, codesign_allocate), IOS_DIR (this folder), IOS_REPO, IOS_HOST (Darwin|Linux).
IOS_DIR=${IOS_DIR:-$(cd "$(dirname "$(readlink -f "${BASH_SOURCE:-$0}")")" && pwd)}
IOS_HOST=${IOS_HOST:-$(uname -s)}
case "$IOS_DIR" in */scripts/ios) ;; *) echo "env.sh: set IOS_DIR to scripts/ios before sourcing it from this shell" >&2; return 1 2>/dev/null || exit 1 ;; esac
IOS_REPO=${IOS_REPO:-$(cd "$IOS_DIR/../.." && pwd)}

if [ -z "${IOS_SDK:-}" ] && [ "$IOS_HOST" = Darwin ]; then
  IOS_SDK=$(xcrun --sdk iphoneos --show-sdk-path 2>/dev/null)
  [ -n "$IOS_SDK" ] || { echo "ios toolchain: no iPhoneOS SDK (install Xcode with the iOS platform; xcode-select -s /Applications/Xcode.app)" >&2; return 1 2>/dev/null || exit 1; }
fi
if [ -z "${IOS_SDK:-}" ]; then
  _sdks=${IOS_SDK_DIR:-$HOME/.config/swiftpm/swift-sdks/darwin.artifactbundle/Developer/Platforms/iPhoneOS.platform/Developer/SDKs}
  # The newest versioned name (iPhoneOS27.1.sdk over iPhoneOS26.5.sdk); a bare iPhoneOS.sdk only when no name has a version.
  IOS_SDK=$(ls -d "$_sdks"/iPhoneOS[0-9]*.sdk 2>/dev/null | sort -V | tail -1)
  [ -n "$IOS_SDK" ] || IOS_SDK=$_sdks/iPhoneOS.sdk
  unset _sdks
fi
if [ ! -f "$IOS_SDK/usr/include/stdio.h" ]; then
  echo "ios toolchain: no iPhoneOS SDK at $IOS_SDK (xtool sdk install Xcode.xip, or set IOS_SDK)" >&2
  return 1 2>/dev/null || exit 1
fi
IOS_SDK_VERSION=$(sed -n 's/.*"Version" *: *"\([0-9.]*\)".*/\1/p' "$IOS_SDK/SDKSettings.json" 2>/dev/null | head -1)
[ -n "$IOS_SDK_VERSION" ] || IOS_SDK_VERSION=$(basename "$IOS_SDK" .sdk | sed 's/^iPhoneOS//')

if [ "$IOS_HOST" = Darwin ]; then
  IOS_LD=${IOS_LD:-$(xcrun --sdk iphoneos --find ld 2>/dev/null)}
  IOS_CLANG=${IOS_CLANG:-$(xcrun --sdk iphoneos --find clang 2>/dev/null)}
fi
IOS_LD=${IOS_LD:-$IOS_REPO/out/ios-toolchain/cctools/bin/ld}
if [ ! -x "$IOS_LD" ]; then
  echo "ios toolchain: no ld64 at $IOS_LD (scripts/setup/ios-toolchain.sh --ld builds it, or set IOS_LD)" >&2
  return 1 2>/dev/null || exit 1
fi
IOS_CCTOOLS=$(dirname "$IOS_LD")
# ld64 says "PROJECT:ld64-956.6"; Xcode 15 and later's ld says "PROJECT:ld-27106.1", which is no ld64 version: clang
# is then given no -mlinker-version and asks the linker itself.
IOS_LD_VERSION=${IOS_LD_VERSION:-$("$IOS_LD" -v 2>&1 | sed -n 's/.*PROJECT:ld64-\([0-9.]*\).*/\1/p' | head -1)}

IOS_TBD=${IOS_TBD:-$IOS_REPO/out/ios27-tbd}
IOS_MIN=${IOS_MIN:-27.0}
IOS_TARGET=${IOS_TARGET:-arm64e-apple-ios$IOS_MIN}
IOS_CLANG=${IOS_CLANG:-clang}
export IOS_HOST IOS_DIR IOS_REPO IOS_SDK IOS_SDK_VERSION IOS_LD IOS_LD_VERSION IOS_CCTOOLS IOS_TBD IOS_MIN IOS_TARGET IOS_CLANG
