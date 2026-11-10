#!/bin/bash
# Build OneiOS's guest helpers for iOS 27 (arm64e) and sign them ad hoc with 4 KiB signature pages.
#
#   userland/helpers/build.sh [OUT_DIR]        default out/userland/helpers (git-ignored)
#   userland/helpers/build.sh --host [OUT_DIR] host build with cc, for the tests (all but oneios-mg)
#
# Output: OUT_DIR/{oneios-init,oneios-bootstat,oneios-seed,oneios-mg} and OUT_DIR/cdhashes.txt. Give OUT_DIR to
# `roottweak apply --helpers`. Any other signed Mach-O put into OUT_DIR is installed too (a dropbear named
# "dropbear" is started as the ssh service on port 2222): see README.md.
#
# macOS: Xcode with the iPhoneOS SDK (xcrun clang, codesign --pagesize 4096), as Droid-VM/ownervisor build.py.
# Linux: stock clang (arm64e needs LLVM 19 or later), a Mach-O linker and an iPhoneOS SDK, the toolchain
#   iOSPlus10 userland/ssh/build.sh uses: theos' ld64 and ldid (THEOS=DIR, toolchain/linux/iphone/bin) and
#   theos/sdks' iPhoneOS SDK (IOS_SDK=DIR; iOSPlus10 userland/ssh/fetch-sdk.sh fetches 16.5, about 100 MB).
#   Signing: ldid -S (4 KiB pages, its only size). NOT RUN on this project's build machine yet: there is no
#   SDK on it. iOSPlus10 builds plain arm64 this way; arm64e with this linker is untested.
# Why arm64e: iOS 27 kills arm64 platform binaries at exec (ownervisor build.py, DROIDVM_IOS_TARGET).
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
TARGET=${ONEIOS_IOS_TARGET:-arm64e-apple-ios27.0}
WARN=(-O2 -Wall -Wextra -Werror)
host=0
if [ "${1:-}" = --host ]; then host=1; shift; fi
OUT=${1:-$REPO/out/userland/helpers}
mkdir -p "$OUT"

if [ "$host" = 1 ]; then
  for name in oneios-init oneios-bootstat oneios-seed; do
    cc -std=c99 -D_DEFAULT_SOURCE -D_POSIX_C_SOURCE=200809L "${WARN[@]}" "$HERE/$name.c" -o "$OUT/$name"
  done
  echo "host build in $OUT (not for the guest)"
  exit 0
fi

ENT=$OUT/entitlements.plist
# Outside any app container, as ownervisor signs its manager; nothing else.
cat > "$ENT" <<'P'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.private.security.no-container</key><true/></dict></plist>
P

case "$(uname -s)" in
Darwin)
  SDK=$(xcrun --sdk iphoneos --show-sdk-path)
  compile() { xcrun clang -target "$TARGET" -isysroot "$SDK" "${WARN[@]}" "$@"; }
  sign() { codesign --force --sign - --pagesize 4096 --identifier "com.oneios.$(basename "$1")" --entitlements "$ENT" "$1"; }
  ;;
Linux)
  TC=${THEOS:?set THEOS to a theos checkout with toolchain/linux/iphone (see the header)}/toolchain/linux/iphone/bin
  SDK=${IOS_SDK:?set IOS_SDK to an iPhoneOS SDK directory (see the header)}
  [ -x "$TC/ld" ] && [ -x "$TC/ldid" ] || { echo "missing $TC/ld or $TC/ldid" >&2; exit 1; }
  [ -d "$SDK/usr/include" ] || { echo "$SDK: not an SDK" >&2; exit 1; }
  compile() { clang -target "$TARGET" -isysroot "$SDK" --ld-path="$TC/ld" -Wno-unused-command-line-argument "${WARN[@]}" "$@"; }
  sign() { "$TC/ldid" -S"$ENT" -I"com.oneios.$(basename "$1")" "$1"; }
  ;;
*) echo "build.sh: macOS or Linux" >&2; exit 1 ;;
esac

compile "$HERE/oneios-init.c" -o "$OUT/oneios-init"
compile "$HERE/oneios-bootstat.c" -o "$OUT/oneios-bootstat"
compile "$HERE/oneios-seed.c" -o "$OUT/oneios-seed"
compile "$HERE/oneios-mg.c" -framework CoreFoundation -o "$OUT/oneios-mg"
for name in oneios-init oneios-bootstat oneios-seed oneios-mg; do sign "$OUT/$name"; done
rm -f "$ENT"
# The same reader the image build uses: fails on a file that is not signed, says the architecture and page size.
(cd "$HERE/.." && python3 -I -m roottweak cdhash "$OUT/oneios-init" "$OUT/oneios-bootstat" "$OUT/oneios-seed" "$OUT/oneios-mg") | tee "$OUT/cdhashes.txt"
if grep -v -q 'arm64e pages=4096' "$OUT/cdhashes.txt"; then
  echo "build.sh: a helper is not arm64e with 4 KiB signature pages (lines above)" >&2; exit 1
fi
