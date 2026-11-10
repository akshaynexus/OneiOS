#!/bin/bash
# Build the Linux XNU toolchain (Allenwdk/xnu-linux-toolchain at the pin) into out/toolchain, without root:
# cctools-port (Apple's ld64, ar, nm, strip, libtool), mig, lzfse, unifdef, tcsh and the shims (iig, xcrun, plutil).
# The compiler is the host's clang. About 5 minutes; run it in tmux:
#   scripts/bg.sh toolchain kernel/linux/setup-toolchain.sh
# Host packages: git cmake make clang flex bison autoconf automake pkg-config python3, and libdispatch with
# BlocksRuntime (Arch: libdispatch or swift; Debian/Ubuntu: libdispatch-dev libblocksruntime-dev).
# The upstream bootstrap clones branch heads; this script puts the commits of kernel/pins.sh there first.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
. "$ROOT/kernel/pins.sh"
PREFIX=${XNU_LINUX_PREFIX:-$ROOT/out/toolchain}
TC=$ROOT/external/xnu-linux-toolchain
[ "$(uname -s)" = Linux ] || { echo "setup-toolchain: Linux only (a Mac builds with Xcode: kernel/build-macos.sh)" >&2; exit 1; }
[ -f "$TC/bootstrap.sh" ] || "$ROOT/kernel/fetch.sh" xnu-linux-toolchain
STAMP=$PREFIX/.oneios-toolchain
WANT=$(printf '%s\n%s\n' "$PINS_TOOLCHAIN" "$(git -C "$TC" rev-parse HEAD)" | sha256sum | cut -c1-16)
if [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$WANT" ] && [ -x "$PREFIX/bin/migcom" ] && [ -x "$PREFIX/cctools/bin/ld" ]; then
  echo "setup-toolchain: $PREFIX is up to date"; exit 0
fi
mkdir -p "$PREFIX/src"
while read -r name url commit branch; do
  pin_fetch "$PREFIX/src" "$name" "$url" "$commit" "$branch" </dev/null
done <<ROWS
$(pin_rows "$PINS_TOOLCHAIN")
ROWS
# libdispatch and BlocksRuntime outside the default paths (Swift's copy on Arch): tell cctools' configure and the loader.
if ! [ -e /usr/include/dispatch/dispatch.h ]; then
  for d in /usr/lib/swift /usr/lib/swift/usr/lib/swift; do
    if [ -e "$d/dispatch/dispatch.h" ]; then
      lib=$(dirname "$(ldconfig -p | awk '/libdispatch.so / && !seen {print $NF; seen = 1}')")
      export CPPFLAGS="${CPPFLAGS:-} -I$d -I$d/Block" LDFLAGS="${LDFLAGS:-} -L$lib -Wl,-rpath,$lib"
      echo "setup-toolchain: libdispatch headers $d, library $lib"
      break
    fi
  done
fi
# unifdef 2.12 uses "constexpr" as a variable name, a keyword in C23 (the default of GCC 15 and clang 23): build it as
# C11 here; bootstrap.sh's own make then finds it up to date.
make -C "$PREFIX/src/unifdef" -j"${JOBS:-$(nproc)}" CFLAGS="-O2 -std=gnu11" unifdef >/dev/null
# bootstrap.sh tests `ldconfig -p | grep -q libdispatch` under pipefail: with a long library list grep leaves early,
# ldconfig dies of SIGPIPE and the test fails though the library is there. Give it a short answer.
ldconfig -p | grep libdispatch > "$PREFIX/src/ldconfig.out" || { echo "setup-toolchain: libdispatch not installed" >&2; exit 1; }
mkdir -p "$PREFIX/src/shim"
printf '#!/bin/sh\ncat "%s"\n' "$PREFIX/src/ldconfig.out" > "$PREFIX/src/shim/ldconfig"
chmod +x "$PREFIX/src/shim/ldconfig"
PATH=$PREFIX/src/shim:$PATH XNU_LINUX_PREFIX=$PREFIX XNU_LINUX_WORK=$PREFIX/src bash "$TC/bootstrap.sh" --prefix "$PREFIX" --jobs "${JOBS:-$(nproc)}" || rc=$?
# bootstrap.sh's last step copies toolchain/* with a plain cp; toolchain/csh is a link to a tcsh that is only in the
# prefix, so that cp ends with an error after it has copied everything else. Finish its steps here and check the result.
cp -f "$TC/env.sh" "$PREFIX/env.sh"
ln -sf tcsh "$PREFIX/bin/csh"
chmod +x "$PREFIX"/bin/*
for tool in bin/migcom bin/mig bin/tcsh bin/csh bin/unifdef bin/lzfse bin/iig bin/xcrun bin/plutil bin/target-clang \
            cctools/bin/ld cctools/bin/libtool cctools/bin/nm cctools/bin/strip cctools/include/mach-o/loader.h; do
  [ -e "$PREFIX/$tool" ] || { echo "setup-toolchain: $tool missing after bootstrap.sh (exit ${rc:-0}); see the log above" >&2; exit 1; }
done
version=$("$PREFIX/cctools/bin/ld" -v 2>&1 || true)
echo "${version%%$'\n'*}"
echo "$WANT" > "$STAMP"
echo "setup-toolchain: done: $PREFIX"
