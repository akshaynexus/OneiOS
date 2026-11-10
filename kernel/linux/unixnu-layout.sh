#!/bin/bash
# Arrange out/toolchain (xnu-linux-toolchain: cctools-port, mig, iig, lzfse, tcsh, unifdef) the way UniXNU's Linux
# kernel step expects its toolchain (scripts/unixnu/kernel.py linux_env, scripts/linux/setup-toolchain.py), in
# out/toolchain/unixnu, without root and without building LLVM:
#   llvm/bin/clang                       the host's clang (no __builtin_xnu_type_*: the series carries
#                                        xnu-linux-toolchain's portable-kalloc-type-builtins.patch instead)
#   CommandLineTools/usr/bin             cctools' ld64 and Mach-O tools, mig, UniXNU's shims (cc, codesign, gm4,
#                                        install, plutil), our clang wrapper (arm64e), xnu-host-cc
#   CommandLineTools/usr/libexec/migcom
#   XcodeDefault.xctoolchain/usr/bin/iig the Python iig of xnu-linux-toolchain, with the two xnu-12377 fixes
#   bin/lzfse, usrbin/{xcrun,plutil,codesign}  UniXNU's shims; build-linux.sh binds them into /usr/bin
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
SRC=${XNU_LINUX_PREFIX:-$ROOT/out/toolchain}
DST=$SRC/unixnu
UNIXNU=$ROOT/external/Droid-VM-UniXNU
[ -x "$SRC/cctools/bin/ld" ] && [ -x "$SRC/bin/migcom" ] || { echo "unixnu-layout: run kernel/linux/setup-toolchain.sh first" >&2; exit 1; }
CLT=$DST/CommandLineTools/usr
XCT=$DST/XcodeDefault.xctoolchain/usr
rm -rf "$DST"
mkdir -p "$DST/llvm/bin" "$DST/bin" "$CLT/bin" "$CLT/lib" "$CLT/libexec" "$CLT/include" "$XCT/bin"
HOST_CLANG=$(command -v "${XNU_LINUX_CLANG:-clang}")
ln -s "$HOST_CLANG" "$DST/llvm/bin/clang"
ln -s "$(dirname "$HOST_CLANG")/clang++" "$DST/llvm/bin/clang++"
for tool in "$SRC"/cctools/bin/*; do ln -s "$tool" "$CLT/bin/$(basename "$tool")"; done
# dsymutil and the other LLVM tools xnu's makefiles ask xcrun for
for tool in dsymutil; do ln -s "$(command -v $tool || command -v llvm-$tool)" "$CLT/bin/$tool"; done
for shim in cc codesign gm4 install plutil; do install -m755 "$UNIXNU/scripts/linux/$shim" "$CLT/bin/$shim"; done
install -m755 "$ROOT/kernel/linux/clang-wrapper" "$CLT/bin/clang"
ln -s clang "$CLT/bin/clang++"
# The three shims UniXNU's installer writes to /usr/bin as root (USR_BIN_SHIMS); build-linux.sh binds them there.
mkdir -p "$DST/usrbin"
for shim in xcrun plutil codesign; do install -m755 "$UNIXNU/scripts/linux/$shim" "$DST/usrbin/$shim"; done
ln -s ../usrbin/xcrun "$DST/bin/xcrun"
# xnu's SETUP/config/doconf starts with #!/bin/csh; a host without csh gets the toolchain's tcsh there the same way.
install -m755 "$SRC/bin/tcsh" "$DST/usrbin/csh"
install -m755 "$SRC/bin/mig" "$CLT/bin/mig"
ln -s "$SRC/bin/migcom" "$CLT/libexec/migcom"
for tool in unifdef tcsh csh; do ln -s "$SRC/bin/$tool" "$CLT/bin/$tool"; done
ln -s "$SRC/bin/lzfse" "$DST/bin/lzfse"
# Host tools the driver looks for on PATH before it sets its own (unifdef, and csh for xnu's doconf).
mkdir -p "$DST/hostbin"
for tool in unifdef tcsh csh; do ln -s "$SRC/bin/$tool" "$DST/hostbin/$tool"; done
# xnu's makefiles ask sw_vers for the host version (only to print it): the macOS release of this XNU.
printf '#!/bin/sh\ncase "${1:-}" in -productVersion) echo 26.5 ;; -buildVersion) echo 25F71 ;; *) printf "ProductName:\\tmacOS\\nProductVersion:\\t26.5\\n" ;; esac\n' > "$DST/hostbin/sw_vers"
chmod +x "$DST/hostbin/sw_vers"
# iig: a copy with the fixes iOSPlus10's scripts/laurel/ios26-iig-fix.py found for xnu-12377's .iig files.
cp "$SRC/bin/iig" "$XCT/bin/iig"
python3 -I "$ROOT/kernel/linux/iig-fix.py" "$XCT/bin/iig"
chmod +x "$XCT/bin/iig"
# Host headers and libraries of xnu's SETUP tools: cctools' <mach-o/*.h> and libmacho/libstuff, and UniXNU's
# xnu-host-cc with its prefix header (taken from its setup-toolchain.py, so they stay in step with the pin).
cp -R "$SRC/cctools/include/." "$CLT/include/"
cp "$SRC/src/cctools-port/cctools/libmacho/.libs/libmacho.a" "$SRC/src/cctools-port/cctools/libstuff/.libs/libstuff.a" "$CLT/lib/"
python3 -I - "$UNIXNU/scripts/linux/setup-toolchain.py" "$CLT" "$HOST_CLANG" <<'PY'
import importlib.util, platform, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location('setup_toolchain', sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
clt, clang = Path(sys.argv[2]), sys.argv[3]
include = clt / 'include'
(include / 'unixnu-host-prefix.h').write_text(module.HOST_PREFIX_H)
(include / 'copyfile.h').write_text(module.COPYFILE_H)
arch = '-D__arm64__ -fsigned-char' if platform.machine() == 'aarch64' else ''
wrapper = clt / 'bin/xnu-host-cc'
wrapper.write_text(module.HOST_CC.format(clang=clang, arch_defs=arch, include=include, lib=clt / 'lib'))
wrapper.chmod(0o755)
PY
echo "unixnu-layout: $DST"
