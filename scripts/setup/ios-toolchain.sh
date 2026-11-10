#!/bin/bash
# The iOS user-space cross toolchain on Linux (no Mac): checks it, and builds or makes the parts that can be made here.
#
#   scripts/setup/ios-toolchain.sh            check; prints one line: "ios toolchain: ok ..." or "... missing: A; B"
#   scripts/setup/ios-toolchain.sh --ld       build Apple's ld64 with .tbd support into out/ios-toolchain (about 6 min;
#                                             scripts/bg.sh ios-ld scripts/setup/ios-toolchain.sh --ld)
#   scripts/setup/ios-toolchain.sh --tbd      make the iOS 27 stubs from the image set's shared cache (out/ios27-tbd,
#                                             scripts/ios/make-tbds.py; about 6 min)
#   scripts/setup/ios-toolchain.sh --test     build, sign and check a small executable (out/ios-test)
#
# The parts (scripts/ios/env.sh has the variables that override each):
#   SDK      the newest iPhoneOS*.sdk xtool extracted from Xcode: `xtool sdk install Xcode_27.1.xip` (the .xip is the
#            owner's download from Apple). Headers come from it; with only the iOS 26.5 SDK installed the headers are
#            26.5's and the link is against the iOS 27 stubs below.
#   ld64     Apple's ld64-956.6 (cctools-port at the kernel's pin, kernel/pins.sh) built with Apple's libtapi, so that
#            it reads .tbd stubs: the kernel's ld64 in out/toolchain is built without it (the kernel links no stub).
#   clang    the host's, 19 or newer (arm64e).
#   stubs    out/ios27-tbd: what iOS 27.0.1 (24A446) exports, from the guest's own shared cache. Derived from
#            Apple's binaries: never committed.
#   signing  scripts/ios/ios-sign.py with cctools' codesign_allocate.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
PREFIX=$ROOT/out/ios-toolchain
# tpoechtrager/apple-libtapi, branch 1600.0.11.8 (the TAPI of Xcode 16; reads the .tbd v4 of the SDK and of ipsw).
TAPI_URL=https://github.com/tpoechtrager/apple-libtapi.git
TAPI_COMMIT=2a5705470a92691520fa2ff589482ea6c4039740

build_ld() {
  . "$ROOT/kernel/pins.sh"
  local src=$PREFIX/src jobs=${JOBS:-$(nproc)}
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
  local sdk_line ld_line
  if ! command -v clang >/dev/null; then
    missing+=("clang (19 or newer)")
  elif ! echo 'int x;' | clang -target arm64e-apple-ios27.0 -x c -c - -o /dev/null 2>/dev/null; then
    missing+=("a clang that compiles for arm64e-apple-ios (LLVM 19 or newer; this one is $(clang --version | head -1))")
  fi
  for tool in llvm-nm llvm-objdump; do command -v $tool >/dev/null || missing+=("$tool"); done
  python3 -c 'import yaml' 2>/dev/null || missing+=("python3 with PyYAML (macho-check.py)")
  if ! sdk_line=$( (IOS_LD=/bin/true; . "$ROOT/scripts/ios/env.sh" && echo "$IOS_SDK $IOS_SDK_VERSION") 2>/dev/null); then
    missing+=("an iPhoneOS SDK (xtool sdk install Xcode_27.1.xip; or IOS_SDK=DIR)")
  fi
  if ! ld_line=$( (. "$ROOT/scripts/ios/env.sh" && echo "$IOS_LD $IOS_LD_VERSION") 2>/dev/null); then
    [ -n "$sdk_line" ] && missing+=("ld64 (scripts/setup/ios-toolchain.sh --ld; or IOS_LD=FILE)")
    [ -n "$sdk_line" ] || missing+=("ld64 (scripts/setup/ios-toolchain.sh --ld)")
  else
    "${ld_line% *}" -v 2>&1 | grep -q 'TAPI support' || missing+=("ld64 with .tbd support (${ld_line% *} has none; scripts/setup/ios-toolchain.sh --ld)")
    [ -x "$(dirname "${ld_line% *}")/codesign_allocate" ] || missing+=("codesign_allocate beside ld64")
  fi
  local tbd=${IOS_TBD:-$ROOT/out/ios27-tbd}
  if [ -f "$tbd/usr/lib/libSystem.B.tbd" ]; then
    note="iOS 27 stubs $(find "$tbd" -name '*.tbd' | wc -l)"
  else
    missing+=("the iOS 27 stubs in $tbd (scripts/setup/ios-toolchain.sh --tbd, after the image set is built)")
  fi
  if [ ${#missing[@]} -gt 0 ]; then
    local IFS=';'
    echo "ios toolchain: missing: ${missing[*]}" | sed 's/;/; /g'
    return 1
  fi
  echo "ios toolchain: ok: SDK ${sdk_line#* } (${sdk_line% *}), ld64-${ld_line#* }, $(clang --version | head -1 | sed 's/ version / /'), $note"
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
