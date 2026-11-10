#!/bin/bash
# Build the OneiOS XNU (xnu-12377.121.6, arm64e with PAC, iOS 27 / iPhone18,2 profile) on x86_64 or arm64 Linux,
# without a Mac and without Xcode: UniXNU's build driver, the toolchain of Allenwdk/xnu-linux-toolchain (cctools-port
# ld64, mig, iig) and the host's clang. Always in tmux:
#   scripts/bg.sh kernel kernel/build-linux.sh
#   STAGE=prepare ...     sources, toolchain, profile and the patched XNU tree only (no compile)
#   CLEAN=1 ...           remove the build tree first
#   KERNEL_DEFCONFIG=...  kernel/device/configs/NAME (default oneios_defconfig)
# Output: out/kernel/builds/<build id>/ (xnu.macho, xnu.macho.build.json, logs), linked as out/kernel/current.
# Nothing of Apple's that is not open source is fetched: no SDK, no KDK, no kernelcache (the build stops before
# the prelink; that step belongs to the image build, on the owner's machine).
# kernel/README.md has the host packages and what the steps do.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
[ "$(uname -s)" = Linux ] || { echo "build-linux: this is the Linux build; on a Mac use kernel/build-macos.sh" >&2; exit 1; }
for t in clang git make cmake ninja python3 flex bison m4 dsymutil; do
  command -v $t >/dev/null || command -v llvm-$t >/dev/null || { echo "build-linux: missing host tool $t" >&2; exit 1; }
done
. "$ROOT/kernel/lib/stage.sh"
JOBS=${JOBS:-$(nproc)}
TOOLCHAIN=${XNU_LINUX_PREFIX:-$ROOT/out/toolchain}
T0=$(date +%s)

"$ROOT/kernel/fetch.sh"
"$ROOT/kernel/linux/setup-toolchain.sh"
[ -x "$TOOLCHAIN/unixnu/CommandLineTools/usr/bin/xnu-host-cc" ] && [ "$TOOLCHAIN/unixnu" -nt "$ROOT/kernel/linux/unixnu-layout.sh" ] \
  && [ "$TOOLCHAIN/unixnu" -nt "$ROOT/kernel/linux/clang-wrapper" ] || "$ROOT/kernel/linux/unixnu-layout.sh"
stage_sources
[ "${CLEAN:-0}" = 1 ] && rm -rf "${UNIXNU_OUT_DIR:?}/oneios"

# xnu's makefiles call /usr/bin/xcrun, /usr/bin/plutil and /usr/bin/codesign by their full paths, and UniXNU's driver
# checks for xcrun there; doconf wants /bin/csh. UniXNU's installer writes the three as root and installs tcsh; here
# the build runs in a mount namespace where /usr/bin has those files (bubblewrap: a temporary overlay, nothing on disk changes). A machine that already
# has the shims there (CI: sudo install) needs no namespace.
run_build() {
  if [ -x /usr/bin/xcrun ] && [ -x /usr/bin/plutil ] && [ -x /usr/bin/codesign ] && [ -x /bin/csh ]; then "$@"; return; fi
  [ "$(readlink -f /bin)" = /usr/bin ] || [ -x /bin/csh ] || { echo "build-linux: /bin is not /usr/bin here and there is no /bin/csh: install tcsh" >&2; return 1; }
  command -v bwrap >/dev/null || { echo "build-linux: /usr/bin has no xcrun, plutil, codesign and there is no bubblewrap (install bubblewrap, or as root: install -m755 $TOOLCHAIN/unixnu/usrbin/* /usr/bin/)" >&2; return 1; }
  local binds=() shim
  for shim in xcrun plutil codesign csh; do
    [ -e "/usr/bin/$shim" ] || binds+=(--ro-bind "$TOOLCHAIN/unixnu/usrbin/$shim" "/usr/bin/$shim")
  done
  bwrap --dev-bind / / --overlay-src /usr/bin --tmp-overlay /usr/bin "${binds[@]}" -- "$@"
}
export UNIXNU_TOOLCHAIN=$TOOLCHAIN/unixnu ONEIOS_IOS_VERSION=27.0 JOBS
export PATH=$TOOLCHAIN/unixnu/hostbin:$PATH
# The driver replays compile commands for the driver archives with -Werror; xnu-linux-toolchain's list of the
# diagnostics stock clang adds on the existing sources stays a warning (its README section 3).
export UNIXNU_OUT=$UNIXNU_OUT_DIR

export -f unixnu
export UNIXNU DEVICE UNIXNU_OUT_DIR
unixnu defconfig "$CONFIG_NAME"
if [ "${STAGE:-build}" = prepare ]; then
  run_build bash -c 'unixnu tree'
  echo "build-linux: prepared $TREE ($(( $(date +%s) - T0 )) s)"
  exit 0
fi
rc=0
run_build bash -c "unixnu build kernel --skip-prelink -j $JOBS" || rc=$?
LOGS=$UNIXNU_OUT_DIR/oneios/$OS_PROFILE/${CONFIG_NAME%_defconfig}/logs
if [ "$rc" != 0 ]; then
  echo "build-linux: FAILED (exit $rc). Logs: $LOGS" >&2
  for log in "$LOGS"/kernel.log "$LOGS"/kernel-bootstrap.log; do
    [ -f "$log" ] && { grep -n -m 40 -E "error:|Undefined symbols|symbol\(s\) not found|No rule to make|failed$" "$log" >&2 || true; break; }
  done
  exit 1
fi
python3 -I "$ROOT/kernel/build-record.py" finish --host linux --work "$(dirname "$TREE")" --config "$CONFIG_NAME" --out "${KERNEL_OUT:-$ROOT/out/kernel}"
echo "build-linux: done in $(( $(date +%s) - T0 )) s"
