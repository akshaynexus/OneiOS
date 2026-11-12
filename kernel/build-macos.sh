#!/bin/bash
# Build the OneiOS XNU (xnu-12377.121.6, arm64e with PAC, iOS 27 / iPhone18,2 profile) on a Mac. The fallback
# of kernel/build-linux.sh: the same sources, series, configuration and build record, with Apple's clang.
#   scripts/bg.sh kernel kernel/build-macos.sh          (always in tmux)
#   STAGE=prepare ...      sources, profile and the patched XNU tree only
#   CLEAN=1 ...            remove the build tree first
#   KDK_HEADERS=1 ...      add a headers-only KDKROOT (kernel/kdk-headers.py, about 200 headers read over HTTP from the
#                          KDK of pins.sh; no 1.2 GB download). UniXNU's series replaces those headers with public
#                          ones, so the default build needs no KDK; this is for comparing against Apple's.
# Needs: macOS 27 with the Xcode of pins.sh (27.2) at /Applications/Xcode.app (XCODE_APP=...), the iphoneos SDK it ships, and
# Homebrew's cmake, ninja, unifdef and llvm (lld). Missing brew packages are installed.
# Env: XCODE_APP, JOBS, KERNEL_DEFCONFIG, KERNEL_OUT, KDKROOT, ALLOW_XCODE=1 (accept another Xcode version).
# Output: out/kernel/builds/<build id>/ and out/kernel/current, as on Linux. The build stops before the prelink:
# no kernelcache is read. Bash 3.2 (macOS /bin/bash) runs this file.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
[ "$(uname -s)" = Darwin ] || { echo "build-macos: this is the macOS build; on Linux use kernel/build-linux.sh" >&2; exit 1; }
. "$ROOT/kernel/lib/stage.sh"
. "$ROOT/kernel/lib/host.sh"
xcode_setup build-macos
JOBS=${JOBS:-$(sysctl -n hw.ncpu)}
T0=$(date +%s)

# One build at a time in this tree.
mkdir -p "${KERNEL_OUT:-$ROOT/out/kernel}"
LOCK=${KERNEL_OUT:-$ROOT/out/kernel}/build.lock
until mkdir "$LOCK" 2>/dev/null; do
  pid=$(cat "$LOCK/pid" 2>/dev/null || true)
  if [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; then rm -rf "${LOCK:?}"; continue; fi
  echo "build-macos: waiting for the running kernel build (pid ${pid:-?})"; sleep 10
done
echo $$ > "$LOCK/pid"
trap 'rm -rf "${LOCK:?}"' EXIT

stage_sources
stage_tool
[ "${CLEAN:-0}" = 1 ] && rm -rf "${UNIXNU_OUT_DIR:?}/oneios"

# The macOS release this XNU ships in (pins.sh). darwin-xnu-build reads MACOS_VERSION to choose its patch set
# (iOSPlus10 scripts/ios26/build-kernel.sh); UniXNU's driver takes the versions from the OS profile instead, so here
# it is checked against the profile and written to the log.
export MACOS_VERSION
grep -q "xnu_version = \"$XNU_VERSION\"" "$UNIXNU/profiles/os/$OS_PROFILE/profile.toml" \
  || { echo "build-macos: the OS profile is not for xnu-$XNU_VERSION (MACOS_VERSION $MACOS_VERSION)" >&2; exit 1; }
if [ "${KDK_HEADERS:-0}" = 1 ]; then
  export KDKROOT=${KDKROOT:-${KERNEL_OUT:-$ROOT/out/kernel}/kdk-25F84-headers}
  if [ ! -f "$KDKROOT/.kdk-headers" ]; then
    echo "build-macos: KDK headers -> $KDKROOT"
    python3 -I "$ROOT/kernel/kdk-headers.py" --url "$KDK_URL" "$KDKROOT"
  fi
else
  unset KDKROOT
fi
echo "build-macos: MACOS_VERSION=$MACOS_VERSION KDKROOT=${KDKROOT:-none} jobs=$JOBS"

export UNIXNU DEVICE UNIXNU_OUT_DIR UNIXNU_OUT=$UNIXNU_OUT_DIR JOBS
unixnu defconfig "$CONFIG_NAME"
if [ "${STAGE:-build}" = prepare ]; then
  unixnu tree
  echo "build-macos: prepared $TREE ($(( $(date +%s) - T0 )) s)"
  exit 0
fi
rc=0
unixnu build kernel --skip-prelink -j "$JOBS" || rc=$?
LOGS=$UNIXNU_OUT_DIR/oneios/$OS_PROFILE/${CONFIG_NAME%_defconfig}/logs
if [ "$rc" != 0 ]; then
  echo "build-macos: FAILED (exit $rc). Logs: $LOGS" >&2
  for log in "$LOGS"/kernel.log "$LOGS"/kernel-bootstrap.log; do
    if [ -f "$log" ]; then grep -n -m 40 -E "error:|Undefined symbols|symbol\(s\) not found|No rule to make" "$log" >&2 || true; break; fi
  done
  exit 1
fi
"$ONEIOS_TOOL" kernel build-record finish --host macos --work "$(dirname "$TREE")" --config "$CONFIG_NAME" --out "${KERNEL_OUT:-$ROOT/out/kernel}" --repo "$ROOT"
echo "build-macos: done in $(( $(date +%s) - T0 )) s"
