# Sourced by the kernel scripts: small helpers that work the same with GNU (Linux) and BSD (macOS) tools, and the
# Xcode setup of a Mac build. Bash 3.2 (macOS /bin/bash) runs this file.
# shellcheck shell=bash

# host_jobs: the number of online CPUs (nproc is GNU coreutils; macOS has sysctl).
host_jobs() { getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4; }
# file_size FILE: its size in bytes (stat -c is GNU, stat -f is BSD).
file_size() { wc -c < "$1" | tr -d ' '; }
# sha256 FILE: its SHA-256 in hex.
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
# sed_i EXPR FILE: sed -i, which takes a different argument in GNU and BSD sed.
sed_i() {
  local tmp
  tmp=$(mktemp "$2.XXXXXX")
  sed "$1" "$2" > "$tmp" && cat "$tmp" > "$2" && rm -f "$tmp"
}

# xcode_setup NAME: the Xcode of a Mac kernel build (NAME is the caller, for messages). Needs XCODE_VERSION
# (pins.sh). Env: XCODE_APP (default /Applications/Xcode.app), ALLOW_XCODE=1 (accept another Xcode version than the
# pin). Exports DEVELOPER_DIR, XNU_BUILD_WERROR=0 and CCC_OVERRIDE_OPTIONS; installs Homebrew's cmake, ninja, unifdef, llvm and lld when
# missing (the archive builders take llvm-ar, llvm-nm and llvm-objcopy from /opt/homebrew/opt/llvm/bin).
xcode_setup() {
  local name=$1 app have t
  app=${XCODE_APP:-/Applications/Xcode.app}
  [ -d "$app/Contents/Developer" ] || { echo "$name: no Xcode at $app (Xcode $XCODE_VERSION; XCODE_APP=/path/to/Xcode.app)" >&2; return 1; }
  export DEVELOPER_DIR=$app/Contents/Developer
  have=$(xcodebuild -version | sed -n 's/^Xcode //p')
  if [ "$have" != "$XCODE_VERSION" ] && [ "${ALLOW_XCODE:-0}" != 1 ]; then
    echo "$name: Xcode $have at $app; the build is recorded with Xcode $XCODE_VERSION (ALLOW_XCODE=1 to build with $have)" >&2
    return 1
  fi
  xcrun --sdk iphoneos --show-sdk-path >/dev/null || { echo "$name: Xcode at $app has no iphoneos SDK" >&2; return 1; }
  for t in cmake ninja unifdef; do
    command -v $t >/dev/null || brew install $t
  done
  [ -x /opt/homebrew/opt/llvm/bin/llvm-objcopy ] || [ -x /usr/local/opt/llvm/bin/llvm-objcopy ] || brew install llvm
  [ -x /opt/homebrew/opt/lld/bin/ld.lld ] || [ -x /opt/homebrew/opt/llvm/bin/ld.lld ] || [ -x /usr/local/opt/lld/bin/ld.lld ] || brew install lld
  # Xcode's clang warns on every kernel header; without this the log grows to gigabytes (iOSPlus10
  # scripts/ios26/build-kernel.sh). UniXNU's script appends its own flags to CFLAGS_EXTRA.
  export XNU_BUILD_WERROR=0
  # The board's archive builders replay xnu's compile records with -Wall -Wextra -Werror (`oneios-tool kernel drivers`). On Linux
  # kernel/linux/clang-wrapper ends every compile with -w, so no warning there is an error; Xcode 27.2's clang warns on
  # Apple's own headers (EXTERNAL_HEADERS/corecrypto: -Wextern-c-compat). The same rule here: warnings stay in the
  # archive logs and do not stop the build. The clang driver reads CCC_OVERRIDE_OPTIONS ('#': quietly; '+': append).
  export CCC_OVERRIDE_OPTIONS='# +-Wno-error'
  echo "$name: $(xcodebuild -version | tr '\n' ' ')SDK $(xcrun --sdk iphoneos --show-sdk-version)"
}
