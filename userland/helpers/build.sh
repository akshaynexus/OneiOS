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
# Toolchain: scripts/ios (docs/building.md section 9): ios-clang, then ios-sign.py, whose signature is byte for byte
#   `codesign --force --sign - --pagesize 4096`. macOS: Xcode's clang, ld and iPhoneOS SDK. Linux: the cross toolchain
#   (scripts/setup/ios-toolchain.sh says what is missing). The link is against the guest's iOS 27 stubs (out/ios27-tbd)
#   when they are there, else against the SDK's.
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
    cc -std=c99 -D_DEFAULT_SOURCE -D_POSIX_C_SOURCE=200809L -DONEIOS_HOST_TEST $([ "$(uname -s)" != Darwin ] || echo -D_DARWIN_C_SOURCE) \
      "${WARN[@]}" "$HERE/$name.c" -o "$OUT/$name"
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

. "$REPO/scripts/ios/env.sh"
compile() { "$IOS_DIR/ios-clang" -target "$TARGET" "${WARN[@]}" "$@"; }
sign() { "$IOS_DIR/ios-sign.py" --identifier "com.oneios.$(basename "$1")" --entitlements "$ENT" "$1" >/dev/null; }
echo "helpers: SDK $IOS_SDK_VERSION ($IOS_SDK); stubs $([ -f "$IOS_TBD/usr/lib/libSystem.B.tbd" ] && echo "$IOS_TBD" || echo "none: the SDK's own"); $("$IOS_CLANG" --version | head -1)"

compile "$HERE/oneios-init.c" -o "$OUT/oneios-init"
compile "$HERE/oneios-bootstat.c" -o "$OUT/oneios-bootstat"
compile "$HERE/oneios-seed.c" -o "$OUT/oneios-seed"
compile "$HERE/oneios-mg.c" -framework CoreFoundation -o "$OUT/oneios-mg"
for name in oneios-init oneios-bootstat oneios-seed oneios-mg; do sign "$OUT/$name"; done
rm -f "$ENT"
# The same reader the image build uses: fails on a file that is not signed, says the architecture and page size.
python3 -I "$HERE/../tweak" cdhash "$OUT/oneios-init" "$OUT/oneios-bootstat" "$OUT/oneios-seed" "$OUT/oneios-mg" | tee "$OUT/cdhashes.txt"
if grep -v -q 'arm64e pages=4096' "$OUT/cdhashes.txt"; then
  echo "build.sh: a helper is not arm64e with 4 KiB signature pages (lines above)" >&2; exit 1
fi
