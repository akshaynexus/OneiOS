#!/bin/bash
# Assemble the OneiOS runtime package: out/release/oneios-runtime-VERSION-arm64.tar.zst and its .sha256.
#   runtime/package.sh VERSION            VERSION like 0.1.0 or 0.1.0-dev (a tag v0.1.0 gives 0.1.0)
# What goes in (oneos/interface.md section 1 is the format the OneOS fork reads):
#   runtime.json           the manifest: version, git commit, every file with size, SHA-256 and mode
#   bin/                   xnuhost and its helpers, built for arm64 Android by runtime/build.sh (the booter's);
#                          without that script: a placeholder xnuhost that prints "not built yet" and exits 2
#   lib/                   what a helper loads that is not a program: oneios-display.dex (runtime/display, the
#                          SurfaceControl layer that bin/oneios-display runs with app_process)
#   scripts/               runtime/scripts/* (session helpers), when there are any
#   kernel/                the XNU built by kernel/ (out/kernel/current, or KERNEL_DIR), with its build record
#   NOTICE, LICENSE        licences; the APSL notice when a kernel is inside
# Nothing of Apple's goes in: `oneios-tool package` (tools/oneios-tool/crates/oneios-package; it was package/pack.py
# until 2026-11-13) refuses firmware names and containers, and any Mach-O that is
# not our own source-built XNU (docs/releases.md).
# Nothing that Android cannot load goes in either: runtime/package/elf-check.sh reads every dynamic ELF file of the
# stage (the display helper's library, the dynamic helpers) and the package is refused (exit 3) when one needs a
# library that is no API 30 stub of the NDK, or has an undefined symbol that none of its NEEDED libraries defines.
# Build hosts: Linux x86_64 and macOS (runtime/host.mk). Needs Rust (cargo) for oneios-tool, which is built when it
# is missing or older than its sources (scripts/lib/oneios-tool.sh; ONEIOS_TOOL=PATH names another binary).
# Env: ANDROID_NDK_ROOT (default $HOME/Android/Sdk/ndk/28.2.13676358, on macOS $HOME/Library/Android/sdk/ndk/...;
#      the revision is checked; runtime/build.sh gets it as NDK unless NDK is set), ANDROID_API (35),
#      KERNEL_DIR, OUT_DIR (out/release), SOURCE_DATE_EPOCH (default: the commit time),
#      PACKAGE_EXTRA_DIR (a folder copied into the package as it is; used by the tests).
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
NDK_REVISION=28.2.13676358
VERSION=${1:-}
if ! [[ $VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.]*)?$ ]]; then
  echo "usage: runtime/package.sh VERSION   (N.N.N or N.N.N-suffix; got '${VERSION}')" >&2; exit 2
fi
for tool in zstd tar git; do
  command -v "$tool" >/dev/null || { echo "package: missing host tool $tool" >&2; exit 1; }
done
# oneios-tool: the checks, the manifest and (without GNU tar) the archive are its `package` commands.
. "$ROOT/scripts/lib/oneios-tool.sh"
oneios_tool "$ROOT"
# Linux and macOS: sha256sum where there is one (macOS 15 and newer have it), else shasum -a 256; GNU tar, else
# `oneios-tool package archive` (the same ustar layout; macOS's tar is bsdtar, which has no --mtime).
if command -v sha256sum >/dev/null; then SHA256=(sha256sum); else SHA256=(shasum -a 256); fi
tar --version 2>/dev/null | grep -q 'GNU tar' && GNU_TAR=1 || GNU_TAR=0
OUT=${OUT_DIR:-$ROOT/out/release}
NAME=oneios-runtime-$VERSION-arm64
WORK=$OUT/.work-$VERSION
STAGE=$WORK/stage
rm -rf "$WORK"; mkdir -p "$STAGE/bin"
trap 'rm -rf "$WORK"' EXIT

# The NDK, pinned: the same revision here and in .github/workflows.
if [ "$(uname -s)" = Darwin ]; then SDK_DEFAULT=$HOME/Library/Android/sdk; else SDK_DEFAULT=$HOME/Android/Sdk; fi
export ANDROID_NDK_ROOT=${ANDROID_NDK_ROOT:-${ANDROID_SDK_ROOT:-$SDK_DEFAULT}/ndk/$NDK_REVISION}
have=$(sed -n 's/^Pkg.Revision *= *//p' "$ANDROID_NDK_ROOT/source.properties" 2>/dev/null || true)
[ "$have" = "$NDK_REVISION" ] || { echo "package: NDK $NDK_REVISION needed at $ANDROID_NDK_ROOT (found '${have:-none}')" >&2; exit 1; }
export ANDROID_API=${ANDROID_API:-35}
NDK_BIN=$(echo "$ANDROID_NDK_ROOT"/toolchains/llvm/prebuilt/*/bin)
export CC_aarch64_linux_android=$NDK_BIN/aarch64-linux-android$ANDROID_API-clang
export CXX_aarch64_linux_android=$NDK_BIN/aarch64-linux-android$ANDROID_API-clang++
export AR_aarch64_linux_android=$NDK_BIN/llvm-ar
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$CC_aarch64_linux_android
[ -x "$CC_aarch64_linux_android" ] || { echo "package: no $CC_aarch64_linux_android" >&2; exit 1; }
[ -d "$HOME/.cargo/bin" ] && PATH=$HOME/.cargo/bin:$PATH
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then SOURCE_DATE_EPOCH=$(git -C "$ROOT" log -1 --format=%ct); fi
export SOURCE_DATE_EPOCH

# 1. The binaries. runtime/build.sh STAGE (the booter's) leaves bin/xnuhost and whatever it runs in STAGE/bin.
PLACEHOLDER=0
if [ -x "$ROOT/runtime/build.sh" ]; then
  echo "package: runtime/build.sh"
  NDK=${NDK:-$ANDROID_NDK_ROOT} "$ROOT/runtime/build.sh" "$STAGE"
  [ -f "$STAGE/bin/xnuhost" ] || { echo "package: runtime/build.sh left no bin/xnuhost" >&2; exit 1; }
else
  echo "package: no runtime/build.sh yet: placeholder xnuhost"
  PLACEHOLDER=1
  "$CC_aarch64_linux_android" -O2 -static -nostdlib -ffreestanding -fno-stack-protector \
    -Wl,-e,_start -Wl,--build-id=none -Wl,-s -o "$STAGE/bin/xnuhost" "$ROOT/runtime/package/placeholder-xnuhost.c"
fi
chmod 0755 "$STAGE"/bin/*
for binary in "$STAGE"/bin/*; do
  kind=$(od -An -tx1 -N20 "$binary" | tr -d ' \n')
  # ELF, 64-bit, little-endian, machine 0xb7 (AArch64); or a helper script (#!)
  case "$kind" in 7f454c460201*b700|2321*) ;; *) echo "package: $binary is not an arm64 ELF or a script" >&2; exit 1 ;; esac
done
# The display helper is whole or absent: its launcher needs the library and the dex (runtime/display).
if [ -e "$STAGE/bin/oneios-display" ]; then
  [ -f "$STAGE/bin/liboneios-display.so" ] && [ "$(head -c 4 "$STAGE/lib/oneios-display.dex" 2>/dev/null)" = "dex" ] ||
    { echo "package: bin/oneios-display without bin/liboneios-display.so or lib/oneios-display.dex" >&2; exit 1; }
fi

# 2. Scripts (0755 when they have a #! line).
if [ -d "$ROOT/runtime/scripts" ] && [ -n "$(ls -A "$ROOT/runtime/scripts")" ]; then
  mkdir -p "$STAGE/scripts"
  cp -R "$ROOT/runtime/scripts/." "$STAGE/scripts/"
  find "$STAGE/scripts" -type f | while read -r f; do
    if [ "$(head -c 2 "$f")" = '#!' ]; then chmod 0755 "$f"; else chmod 0644 "$f"; fi
  done
fi

# 3. The kernel, when one was built: only the native XNU and its record (`package manifest` checks the record).
KERNEL_DIR=${KERNEL_DIR:-$ROOT/out/kernel/current}
HAVE_KERNEL=0
if [ -f "$KERNEL_DIR/xnu.macho" ] && grep -q '"releasable": false' "$KERNEL_DIR/xnu.macho.build.json" && [ "${REQUIRE_KERNEL:-0}" != 1 ]; then
  # A local build that links a source that may not be redistributed (its record says which): it stays out, and the
  # package is runtime only. REQUIRE_KERNEL=1 makes this an error instead (`package manifest` refuses the kernel).
  echo "package: the kernel at $KERNEL_DIR is not releasable (see its build record): runtime only"
elif [ -f "$KERNEL_DIR/xnu.macho" ]; then
  HAVE_KERNEL=1
  mkdir -p "$STAGE/kernel"
  cp "$KERNEL_DIR/xnu.macho" "$KERNEL_DIR/xnu.macho.build.json" "$STAGE/kernel/"
  chmod 0644 "$STAGE"/kernel/*
  echo "package: kernel $(sed -n 's/.*"build_id": *"\([^"]*\)".*/\1/p' "$STAGE/kernel/xnu.macho.build.json")"
else
  echo "package: no kernel at $KERNEL_DIR (runtime only)"
fi

# 4. Notices.
{
  echo "OneiOS runtime $VERSION (https://github.com/akshaynexus/OneiOS)"
  echo
  echo "This package holds OneiOS's own code and builds only. It holds no Apple firmware: the iOS image set is"
  echo "built by its owner from Apple's restore files and is never part of a release."
  if [ "$HAVE_KERNEL" = 1 ]; then
    cat "$ROOT/kernel/NOTICE.kernel"
  fi
} > "$STAGE/NOTICE"
[ -f "$ROOT/LICENSE" ] && cp "$ROOT/LICENSE" "$STAGE/LICENSE"
if [ -n "${PACKAGE_EXTRA_DIR:-}" ]; then cp -R "$PACKAGE_EXTRA_DIR/." "$STAGE/"; fi

# 5. Checks and manifest. First: can Android load every dynamic ELF file of the stage (elf-check.sh's header)?
# runtime/build.sh has checked its own stage; this is the package's content as it goes out.
elf=0
NDK=${NDK:-$ANDROID_NDK_ROOT} "$ROOT/runtime/package/elf-check.sh" "$STAGE" || elf=$?
if [ "$elf" = 1 ]; then
  echo "package: refused: Android cannot load a file of the package (the lines above; runtime/package/elf-check.sh, docs/releases.md)" >&2
  exit 3
fi
[ "$elf" = 0 ] || { echo "package: runtime/package/elf-check.sh could not run (exit $elf)" >&2; exit 1; }
COMMIT=$(git -C "$ROOT" rev-parse HEAD)
DIRTY=0
[ -z "$(git -C "$ROOT" status --porcelain --untracked-files=no -- runtime kernel)" ] || DIRTY=1
"$ONEIOS_TOOL" package manifest "$STAGE" "$VERSION" "$COMMIT" "$DIRTY" \
  "$(tr -d ' \n' < "$ROOT/runtime/package/min-oneos-fork")" "$PLACEHOLDER"

# 6. The archive: runtime.json first, then the rest sorted; fixed owner and time, so the same inputs give the same bytes.
mkdir -p "$OUT"
( cd "$STAGE" && { echo runtime.json; find . -mindepth 1 -type f ! -name runtime.json -path './*' | sed 's|^\./||' | LC_ALL=C sort; } > "$WORK/members" )
if [ "$GNU_TAR" = 1 ]; then
  tar -C "$STAGE" --format=ustar --no-recursion --owner=0 --group=0 --numeric-owner --mtime="@$SOURCE_DATE_EPOCH" \
    -T "$WORK/members" -cf "$WORK/$NAME.tar"
else
  "$ONEIOS_TOOL" package archive "$STAGE" "$WORK/members" "$SOURCE_DATE_EPOCH" "$WORK/$NAME.tar"
fi
zstd -q -19 -f -o "$OUT/$NAME.tar.zst" "$WORK/$NAME.tar"
( cd "$OUT" && "${SHA256[@]}" "$NAME.tar.zst" > "$NAME.tar.zst.sha256" )
cp "$STAGE/runtime.json" "$OUT/$NAME.runtime.json"
rm -rf "$WORK"
echo "package: $OUT/$NAME.tar.zst ($(wc -c < "$OUT/$NAME.tar.zst" | tr -d ' ') bytes)"
cat "$OUT/$NAME.tar.zst.sha256"
