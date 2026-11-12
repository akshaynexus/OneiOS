#!/bin/bash
# The iOS user-space toolchain: checks it, and builds or makes the parts that can be made here. On macOS it is Xcode's
# own (xcrun clang and ld, the iPhoneOS SDK; nothing to build); on Linux a cross toolchain (xtool's SDK, ld64 built here).
#
#   scripts/setup/ios-toolchain.sh            check; prints one line: "ios toolchain: ok ..." or "... missing: A; B"
#   scripts/setup/ios-toolchain.sh --ld       Linux: build Apple's ld64 with .tbd support into out/ios-toolchain (about
#                                             6 min; scripts/bg.sh ios-ld scripts/setup/ios-toolchain.sh --ld)
#   scripts/setup/ios-toolchain.sh --tbd      make the iOS 27 stubs from the image set's shared cache (out/ios27-tbd,
#                                             scripts/ios/make-tbds.py; about 6 min)
#   scripts/setup/ios-toolchain.sh --test     build, sign and check a small executable (out/ios-test)
#
# The parts (scripts/ios/env.sh has the variables that override each):
#   SDK      macOS: Xcode's iPhoneOS SDK (xcrun --sdk iphoneos --show-sdk-path). Linux: the newest iPhoneOS*.sdk xtool
#            extracted from Xcode: `xtool sdk install Xcode_27.1.xip` (the .xip is the owner's download from Apple).
#            Headers come from it; the link is against the iOS 27 stubs below.
#   linker   macOS: Xcode's ld. Linux: Apple's ld64-956.6 (cctools-port at the kernel's pin, kernel/pins.sh) built
#            with Apple's libtapi, so that it reads .tbd stubs: the kernel's ld64 in out/toolchain is built without it.
#   clang    macOS: Xcode's. Linux: the host's, 19 or newer (arm64e).
#   stubs    out/ios27-tbd: what iOS 27.0.1 (24A446) exports, from the guest's own shared cache. Derived from
#            Apple's binaries: never committed.
#   signing  scripts/ios/ios-sign.py with the linker's sibling codesign_allocate (Xcode's on macOS, cctools' on
#            Linux); its output is byte for byte Apple's `codesign --force --sign - --pagesize 4096`.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
PREFIX=$ROOT/out/ios-toolchain
# tpoechtrager/apple-libtapi, branch 1600.0.11.8 (the TAPI of Xcode 16; reads the .tbd v4 of the SDK and of ipsw).
TAPI_URL=https://github.com/tpoechtrager/apple-libtapi.git
TAPI_COMMIT=2a5705470a92691520fa2ff589482ea6c4039740

build_ld() {
  if [ "$(uname -s)" = Darwin ]; then echo "ios-toolchain: on macOS the linker is Xcode's: nothing to build"; return 0; fi
  . "$ROOT/kernel/pins.sh"
  local src=$PREFIX/src jobs=${JOBS:-$(getconf _NPROCESSORS_ONLN)}
  mkdir -p "$src"
  if [ ! -f "$PREFIX/lib/libtapi.so" ]; then
    if [ ! -d "$src/apple-libtapi/.git" ]; then
      git init -q "$src/apple-libtapi"
      git -C "$src/apple-libtapi" fetch -q --depth 1 "$TAPI_URL" "$TAPI_COMMIT"
      git -C "$src/apple-libtapi" checkout -q FETCH_HEAD
    fi
    (cd "$src/apple-libtapi" && INSTALLPREFIX=$PREFIX JOBS=$jobs CC=clang CXX=clang++ ./build.sh && INSTALLPREFIX=$PREFIX ./install.sh)
  fi
  # The same cctools-port commit the kernel's toolchain uses.
  while read -r name url commit branch; do
    [ "$name" = cctools-port ] && pin_fetch "$src" "$name" "$url" "$commit" "$branch" </dev/null
  done <<ROWS
$(pin_rows "$PINS_TOOLCHAIN")
ROWS
  local cpp="" ld="-Wl,-rpath,$PREFIX/lib"
  if ! [ -e /usr/include/dispatch/dispatch.h ]; then
    for d in /usr/lib/swift /usr/lib/swift/usr/lib/swift; do
      if [ -e "$d/dispatch/dispatch.h" ]; then
        lib=$(dirname "$(ldconfig -p | awk '/libdispatch.so / && !seen {print $NF; seen = 1}')")
        cpp="-I$d -I$d/Block"; ld="$ld -L$lib -Wl,-rpath,$lib"
        break
      fi
    done
  fi
  (cd "$src/cctools-port/cctools" &&
    CC=clang CXX=clang++ CPPFLAGS="$cpp" LDFLAGS="$ld" ./configure --prefix="$PREFIX/cctools" --with-libtapi="$PREFIX" >/dev/null &&
    make -j"$jobs" >/dev/null && make install >/dev/null)
  "$PREFIX/cctools/bin/ld" -v 2>&1 | grep -q 'TAPI support' || { echo "ios-toolchain: the ld64 built has no TAPI support" >&2; exit 1; }
  echo "ios-toolchain: $("$PREFIX/cctools/bin/ld" -v 2>&1 | head -1), with TAPI, in $PREFIX/cctools/bin"
}

check() {
  local missing=() note=""
  local sdk_line ld_line cc
  if [ "$(uname -s)" = Darwin ]; then
    cc=$(xcrun --sdk iphoneos --find clang 2>/dev/null) || missing+=("Xcode with the iOS platform (xcode-select -s /Applications/Xcode.app)")
  else
    cc=$(command -v clang) || missing+=("clang (19 or newer)")
    for tool in llvm-nm llvm-objdump; do command -v $tool >/dev/null || missing+=("$tool"); done
  fi
  if [ -n "$cc" ] && ! echo 'int x;' | "$cc" -target arm64e-apple-ios27.0 -x c -c - -o /dev/null 2>/dev/null; then
    missing+=("a clang that compiles for arm64e-apple-ios (LLVM 19 or newer; this one is $("$cc" --version | head -1))")
  fi
  python3 -c 'import yaml' 2>/dev/null || missing+=("python3 with PyYAML (macho-check.py)")
  if ! sdk_line=$( (IOS_LD=$(type -P true); . "$ROOT/scripts/ios/env.sh" && echo "$IOS_SDK $IOS_SDK_VERSION") 2>/dev/null); then
    missing+=("an iPhoneOS SDK (macOS: Xcode's iOS platform, Linux: xtool sdk install Xcode_27.1.xip, or IOS_SDK=DIR)")
  fi
  if ! ld_line=$( (. "$ROOT/scripts/ios/env.sh" && echo "$IOS_LD $IOS_LD_VERSION") 2>/dev/null); then
    [ -n "$sdk_line" ] && missing+=("ld64 (scripts/setup/ios-toolchain.sh --ld; or IOS_LD=FILE)")
    [ -n "$sdk_line" ] || missing+=("ld64 (scripts/setup/ios-toolchain.sh --ld)")
  else
    ld_line=${ld_line% }
    if [ "$(uname -s)" != Darwin ]; then      # Xcode's ld reads .tbd stubs (it has no "TAPI support" line)
      "${ld_line% *}" -v 2>&1 | grep -q 'TAPI support' || missing+=("ld64 with .tbd support (${ld_line% *} has none; scripts/setup/ios-toolchain.sh --ld)")
    fi
    [ -x "$(dirname "${ld_line%% *}")/codesign_allocate" ] || missing+=("codesign_allocate beside the linker")
  fi
  local tbd=${IOS_TBD:-$ROOT/out/ios27-tbd}
  if [ -f "$tbd/usr/lib/libSystem.B.tbd" ]; then
    note="iOS 27 stubs $(find "$tbd" -name '*.tbd' | wc -l | tr -d ' ')"
  else
    missing+=("the iOS 27 stubs in $tbd (scripts/setup/ios-toolchain.sh --tbd, after the image set is built)")
  fi
  if [ ${#missing[@]} -gt 0 ]; then
    local IFS=';'
    echo "ios toolchain: missing: ${missing[*]}" | sed 's/;/; /g'
    return 1
  fi
  local ld_name="ld64-${ld_line#* }"
  [ "${ld_line#* }" != "$ld_line" ] || ld_name="$("$ld_line" -v 2>&1 | sed -n 's/.*PROJECT:\([^ ]*\).*/\1/p' | head -1) ($ld_line)"
  echo "ios toolchain: ok: SDK ${sdk_line#* } (${sdk_line% *}), $ld_name, $("$cc" --version | head -1 | sed 's/ version / /'), $note"
}

run_test() {
  . "$ROOT/scripts/ios/env.sh"
  local out=$ROOT/out/ios-test
  mkdir -p "$out"
  cat > "$out/hello.c" <<'EOF'
#include <CoreFoundation/CoreFoundation.h>
#include <stdio.h>
int main(void) {
    CFStringRef s = CFSTR("OneiOS");
    printf("hello from %ld characters\n", (long)CFStringGetLength(s));
    return 0;
}
EOF
  echo "ios toolchain: SDK $IOS_SDK_VERSION ($IOS_SDK), target $IOS_TARGET"
  "$ROOT/scripts/ios/ios-clang" -O2 -Wall -Werror "$out/hello.c" -framework CoreFoundation -o "$out/hello"
  "$ROOT/scripts/ios/ios-sign.py" --identifier com.oneios.hello "$out/hello" | tee "$out/hello.cdhash"
  python3 -I "$ROOT/userland/tweak" cdhash "$out/hello"
  "$ROOT/scripts/ios/macho-check.py" "$out/hello"
}

case "${1:-}" in
  "") check ;;
  --ld) build_ld ;;
  --tbd) shift; "$ROOT/scripts/ios/make-tbds.py" "$@" ;;
  --test) check && run_test ;;
  *) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
