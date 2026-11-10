#!/bin/bash
# Build the hosted OneiOS XNU: the kernel of kernel/build-linux.sh plus the hosted machine layer
# (kernel/patches/oneios-hosted), which runs as a Linux process under xnuhost instead of at EL1.
#   scripts/bg.sh kernel-hosted kernel/build-hosted.sh
# Output: out/kernel-hosted/unixnu/oneios/<os profile>/oneios_hosted/kernel/kernel.macho, linked as
# out/kernel/current-hosted/xnu.macho. The VM kernel's build (build-linux.sh, out/kernel/current) is not touched:
# this one has its own output root and its own copy of the device repository (out/hosted-device: kernel/device as committed, with
# the hosted series added to the board's patch series), so the two never share a work tree.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
[ "$(uname -s)" = Linux ] || { echo "build-hosted: Linux only" >&2; exit 1; }
# HOSTED_OUT=NAME: a second, independent work tree and output (out/kernel-hosted-NAME, out/hosted-device-NAME,
# out/kernel/current-hosted-NAME), so that several people can build and edit at once. The default tree is the lead's.
# It must be named: HOSTED_OUT=lead is the default tree (out/kernel-hosted, out/kernel/current-hosted), which is the
# lead's; a build with no name once rebuilt that tree under its owner.
[ -n "${HOSTED_OUT:-}" ] || { echo "build-hosted: set HOSTED_OUT=<your name> (your own work tree; 'lead' is the lead's)" >&2; exit 2; }
[ "$HOSTED_OUT" != lead ] || HOSTED_OUT=
SUFFIX=${HOSTED_OUT:+-$HOSTED_OUT}
export KERNEL_OUT=$ROOT/out/kernel-hosted$SUFFIX KERNEL_DEFCONFIG=oneios_hosted_defconfig
. "$ROOT/kernel/lib/stage.sh"
JOBS=${JOBS:-$(nproc)}
TOOLCHAIN=${XNU_LINUX_PREFIX:-$ROOT/out/toolchain}
"$ROOT/kernel/linux/setup-toolchain.sh"
[ -x "$TOOLCHAIN/unixnu/CommandLineTools/usr/bin/xnu-host-cc" ] && [ "$TOOLCHAIN/unixnu" -nt "$ROOT/kernel/linux/clang-wrapper" ] || "$ROOT/kernel/linux/unixnu-layout.sh"
stage_sources

# The device repository of this build: the committed one, the hosted defconfig, and the hosted series last.
# (two levels below the repository, as kernel/device is: the board's archive script finds drivers/ from there)
DEVICE=$ROOT/out/hosted-device$SUFFIX
rm -rf "$DEVICE"; mkdir -p "$DEVICE"
git -C "$ROOT" archive HEAD kernel/device | tar -x -C "$DEVICE" --strip-components=2
cp "$ROOT/kernel/device/configs/oneios_hosted_defconfig" "$DEVICE/configs/"
sed -i "s#^\.\./\.\./\.\./\.\./external/#$ROOT/external/#" "$DEVICE/patches/xnu-12377/series"
python3 -I - "$DEVICE/board.toml" "${HOSTED_SERIES:-$ROOT/kernel/patches/oneios-hosted/series}" <<'PY'
import re, sys
path, series = sys.argv[1:]
text = open(path).read()
new, n = re.subn(r'(patch_series\s*=\s*\[[^\]]*?)\]', lambda m: m.group(1).rstrip().rstrip(',') + ', "' + series + '"]', text, count=1)
if n != 1:
    raise SystemExit('build-hosted: board.toml has no [kernel] patch_series')
open(path, 'w').write(new)
PY
# The disks (0004-root-mount-launchd.patch): OneiOSHostedDisk and the source IOBlockStorageDevice as one more board
# archive of this build only (kernel/patches/oneios-hosted/storage/build-oneios-hosted-storage.py), force-loaded by
# the hosted defconfig. It goes before build-oneios-board.py, which stays the last of the committed list.
cp "$ROOT/kernel/patches/oneios-hosted/storage/build-oneios-hosted-storage.py" "$DEVICE/scripts/"
python3 -I - "$DEVICE/board.toml" <<'PY'
import re, sys
board = sys.argv[1]
text = open(board).read()
new, n = re.subn(r'^(\s*)("build-oneios-board\.py")', lambda m: m.group(1) + '"build-oneios-hosted-storage.py",\n' + m.group(1) + m.group(2), text, count=1, flags=re.M)
if n != 1:
    raise SystemExit('build-hosted: board.toml archives have no build-oneios-board.py')
open(board, 'w').write(new)
PY
# The GPU (0005-gpu-3d.patch): Droid-VM's virtio-gpu 3D driver as one more board archive of this build only
# (kernel/patches/oneios-hosted/gpu/build-oneios-gpu.py), force-loaded like the board's others.
cp "$ROOT/kernel/patches/oneios-hosted/gpu/build-oneios-gpu.py" "$DEVICE/scripts/"
python3 -I - "$DEVICE/board.toml" "$DEVICE/configs/oneios_hosted_defconfig" <<'PY'
import re, sys
board, config = sys.argv[1:]
text = open(board).read()
new, n = re.subn(r'("build-oneios-board\.py",?\s*\n)(\s*\])', lambda m: m.group(1).rstrip().rstrip(',') + ',\n  "build-oneios-gpu.py",\n' + m.group(2), text, count=1)
if n != 1:
    raise SystemExit('build-hosted: board.toml archives do not end with build-oneios-board.py')
open(board, 'w').write(new)
text = open(config).read()
new, n = re.subn(r'^(CONFIG_XNU_EXTRA_LD_ARCHIVES="[^"\n]*)"', lambda m: m.group(1) + ' -Wl,-force_load,{tree}/build/oneios-gpu/liboneiosgpu.a"', text, count=1, flags=re.M)
if n != 1:
    raise SystemExit('build-hosted: the hosted defconfig has no CONFIG_XNU_EXTRA_LD_ARCHIVES')
open(config, 'w').write(new)
PY
# Software AES (0008-software-aes.patch): UniXNU's NativeAES under the collection's IOAESAccelerator, one more board
# archive of this build only (kernel/patches/oneios-hosted/aes/build-oneios-aes.py). AppleImage4's start waits for it.
cp "$ROOT/kernel/patches/oneios-hosted/aes/build-oneios-aes.py" "$DEVICE/scripts/"
python3 -I - "$DEVICE/board.toml" "$DEVICE/configs/oneios_hosted_defconfig" <<'PY'
import re, sys
board, config = sys.argv[1:]
text = open(board).read()
new, n = re.subn(r'^(\s*)("build-oneios-gpu\.py")', lambda m: m.group(1) + '"build-oneios-aes.py",\n' + m.group(1) + m.group(2), text, count=1, flags=re.M)
if n != 1:
    raise SystemExit('build-hosted: board.toml archives have no build-oneios-gpu.py')
open(board, 'w').write(new)
text = open(config).read()
new, n = re.subn(r'^(CONFIG_XNU_EXTRA_LD_ARCHIVES="[^"\n]*)"', lambda m: m.group(1) + ' -Wl,-force_load,{tree}/build/oneios-aes/liboneiosaes.a"', text, count=1, flags=re.M)
if n != 1:
    raise SystemExit('build-hosted: the hosted defconfig has no CONFIG_XNU_EXTRA_LD_ARCHIVES')
open(config, 'w').write(new)
PY
mkdir -p "$UNIXNU_OUT_DIR"
[ -e "$UNIXNU_OUT_DIR/feeds" ] || [ ! -d "$ROOT/out/kernel/unixnu/feeds" ] || ln -s "$ROOT/out/kernel/unixnu/feeds" "$UNIXNU_OUT_DIR/feeds"

run_build() {
  if [ -x /usr/bin/xcrun ] && [ -x /usr/bin/plutil ] && [ -x /usr/bin/codesign ] && [ -x /bin/csh ]; then "$@"; return; fi
  local binds=() shim
  for shim in xcrun plutil codesign csh; do
    [ -e "/usr/bin/$shim" ] || binds+=(--ro-bind "$TOOLCHAIN/unixnu/usrbin/$shim" "/usr/bin/$shim")
  done
  bwrap --dev-bind / / --overlay-src /usr/bin --tmp-overlay /usr/bin "${binds[@]}" -- "$@"
}
export UNIXNU_TOOLCHAIN=$TOOLCHAIN/unixnu ONEIOS_IOS_VERSION=27.0 JOBS
export PATH=$TOOLCHAIN/unixnu/hostbin:$PATH
export UNIXNU_OUT=$UNIXNU_OUT_DIR
export -f unixnu
export UNIXNU DEVICE UNIXNU_OUT_DIR
unixnu defconfig "$CONFIG_NAME"
WORK=$UNIXNU_OUT_DIR/oneios/$OS_PROFILE/${CONFIG_NAME%_defconfig}
if ! run_build bash -c "unixnu build kernel --skip-prelink -j $JOBS"; then
  echo "build-hosted: FAILED. Logs: $WORK/logs" >&2
  grep -n -m 40 -E "error:|Undefined symbols|symbol\(s\) not found|No rule to make" "$WORK/logs/kernel.log" >&2 || true
  exit 1
fi
# The kernel to boot is the sealed one: the build fills corecrypto's POST HMAC field (and writes the static_if test
# sites in their boot state) in mach.development.bcm2837.unstripped only. UniXNU's driver stages the .noctf file,
# whose HMAC field is empty: corecrypto's integrity self-test fails on it (-2074).
SEALED=$WORK/tree/build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837/mach.development.bcm2837.unstripped
[ -f "$SEALED.crypto-hmac.json" ] || { echo "build-hosted: $SEALED was not sealed" >&2; exit 1; }
mkdir -p "$ROOT/out/kernel/current-hosted$SUFFIX"
cp "$SEALED" "$ROOT/out/kernel/current-hosted$SUFFIX/xnu.macho"
# Post-link text fixes of the hosted layer (kernel/hosted-fixup.py: a vCPU's identity comes from xnuhost, not
# MPIDR_EL1), on the copy; then the seal again, over the changed text (the build's own seal script, which takes
# an already sealed kernel).
SEALED=$ROOT/out/kernel/current-hosted$SUFFIX/xnu.macho
python3 -I "$ROOT/kernel/hosted-fixup.py" "$SEALED"
( cd "$WORK/tree" && UNIXNU_XNU_ARCH=arm64e python3 scripts/seal-source-crypto-kernel.py "$SEALED" )
echo "build-hosted: $ROOT/out/kernel/current-hosted$SUFFIX/xnu.macho ($(stat -c %s "$SEALED") bytes, sha256 $(sha256sum "$SEALED" | cut -c1-16)...)"
