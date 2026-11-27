#!/bin/bash
# Assemble the OneiOS runtime package: out/release/oneios-runtime-VERSION-arm64.tar.zst and its .sha256.
#   runtime/package.sh VERSION            VERSION like 0.1.0 or 0.1.0-dev (a tag v0.1.0 gives 0.1.0)
# What goes in (oneos/interface.md section 1 is the format the OneOS fork reads):
#   runtime.json           the manifest: version, git commit, every file with size, SHA-256 and mode
#   bin/                   xnuhost and its helpers, built for arm64 Android by runtime/build.sh (the booter's);
#                          without that script: a placeholder xnuhost that prints "not built yet" and exits 2
#   bin/oneios-tool        the Rust tools (tools/oneios-tool) for aarch64-linux-android, stripped: the OneOS app runs
#                          them as root from the staged runtime. Also written beside the package as
#                          out/release/oneios-tool-VERSION-android-arm64 with its .sha256 (the same bytes)
#   lib/                 what a helper loads that is not a program: oneios-display.dex (runtime/display, the
#                          SurfaceControl layer that bin/oneios-display runs with app_process)
#   lib/switches.json      xnuhost's switch catalog (`oneios-tool switches --json`, step 1d), which the OneOS app's
#                          Switches screen reads with bin/oneios-tool switches --runtime DIR
#   scripts/               runtime/scripts/* (session helpers), when there are any
#   kernel/                the XNU built by kernel/ (out/kernel/current, or KERNEL_DIR), with its build record; another
#                          iOS build's in kernel/<build>/ (KERNEL_MORE_DIRS, step 3a: iOS 27.2.1's 24B156)
#   lib/guest/             the iOS plug-ins, when METAL_DIR (and CAMERA_DIR) are given: NativeAdrenoMetal.bundle with its
#                          build record, camera/H9ISP.mediacapture, and NOTICE (their sources' licences and pins); the
#                          phone's Make iOS image seeds them into the image (step 3b); with METAL_AMD_DIR also the
#                          Galaxy A56's OneiOSRadeonMetal.bundle and OneiOSRadeonGL.bundle with their records (runtime.json
#                          "metal_a56"; the phone makes an iOS 16.5 image's kernel.kc.gpuaddon of them: step 3c); another iOS
#                          build's plug-ins in lib/guest/<build>/ (METAL_MORE_DIRS, step 3d)
#   NOTICE, LICENSE        licences; the APSL notice when a kernel is inside
# Nothing of Apple's goes in: `oneios-tool package` (tools/oneios-tool/crates/oneios-package) refuses firmware names
# and containers, and any Mach-O that is not our own source-built XNU or, under lib/guest/, our own ad hoc signed iOS
# plug-in (docs/releases.md).
# Nothing that Android cannot load goes in either: runtime/package/elf-check.sh reads every dynamic ELF file of the
# stage (the display helper's library, the dynamic helpers) and the package is refused (exit 3) when one needs a
# library that is no API 30 stub of the NDK, or has an undefined symbol that none of its NEEDED libraries defines.
# Build hosts: Linux x86_64 and macOS (runtime/build.sh and runtime/ndk-cc pick the NDK's tools by uname -s). Needs
# Rust (cargo) for the runtime (runtime/build.sh, runtime/RUST.md) and for oneios-tool, which is built when it is
# missing or older than its sources (scripts/lib/oneios-tool.sh; ONEIOS_TOOL=PATH names another binary), and the Rust
# targets aarch64-linux-android (the runtime, the phone's bin/oneios-tool) and aarch64-unknown-none (the task stub)
# (ONEIOS_TOOL_ANDROID=PATH names a phone oneios-tool built elsewhere; CARGO_TARGET_DIR is honoured).
# Env: ANDROID_NDK_ROOT (default $HOME/Android/Sdk/ndk/28.2.13676358, on macOS $HOME/Library/Android/sdk/ndk/...;
#      the revision is checked; runtime/build.sh gets it as NDK unless NDK is set), ANDROID_API (35),
#      KERNEL_DIR, ONEIOS_GPUADDON (ios addon output plus .cdhashes, packed as kernel/gpuaddon), METAL_BOOT_ADDON=1
#      (each Metal folder's NativeAdrenoMetal.gpuaddon as kernel/gpuaddon, kernel/<build>/gpuaddon: step 3e),
#      OUT_DIR (out/release; a relative one is taken from the current folder), SOURCE_DATE_EPOCH (default: the commit time),
#      KERNEL_MORE_DIRS (step 3a), METAL_DIR, CAMERA_DIR, REQUIRE_METAL (step 3b; neither given: no lib/guest/), METAL_AMD_DIR,
#      REQUIRE_METAL_AMD (3c), METAL_MORE_DIRS (step 3d),
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
# Absolute: the archive step lists the members from inside the stage (a relative OUT_DIR, e.g. out/release-x, failed
# there with "members: No such file or directory" after every other step had passed).
mkdir -p "$OUT" && OUT=$(cd "$OUT" && pwd)
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
  rustc --edition 2021 --target aarch64-linux-android -C opt-level=s -C panic=abort -C relocation-model=static \
    -C linker="$CC_aarch64_linux_android" -C link-arg=-nostdlib -C link-arg=-static -C link-arg=-Wl,-e,_start \
    -C link-arg=-Wl,--build-id=none -C strip=symbols -A linker_messages \
    -o "$STAGE/bin/xnuhost" "$ROOT/runtime/package/placeholder-xnuhost.rs"
fi
# 1b. oneios-tool for the phone (tools/oneios-tool, Rust, aarch64-linux-android): bin/oneios-tool, which the OneOS
# app runs as root from the staged runtime (`oneios_root.sh tool`, oneos/interface.md section 9), and the same file
# beside the package as oneios-tool-VERSION-android-arm64 with its .sha256 (docs/releases.md). Linked by the NDK's
# clang for API 30 (the level elf-check.sh reads), stripped with the NDK's llvm-strip. ONEIOS_VERSION=VERSION is
# its `serve` hello's `tool` (before 0.1.17 the hello said the crate's 0.1.0: issue 75). ONEIOS_TOOL_ANDROID=PATH
# takes a binary built elsewhere instead (it is stripped and checked the same way; its hello says its own version).
NDK_STRIP=$NDK_BIN/llvm-strip
if [ -n "${ONEIOS_TOOL_ANDROID:-}" ]; then
  [ -f "$ONEIOS_TOOL_ANDROID" ] || { echo "package: ONEIOS_TOOL_ANDROID=$ONEIOS_TOOL_ANDROID is not a file" >&2; exit 1; }
  TOOL_ANDROID=$ONEIOS_TOOL_ANDROID
else
  echo "package: oneios-tool for aarch64-linux-android (cargo build --release --target aarch64-linux-android)"
  CARGO=$(command -v cargo || echo "$HOME/.cargo/bin/cargo")
  [ -x "$CARGO" ] || { echo "package: no cargo for oneios-tool (docs/building.md section 0)" >&2; exit 1; }
  ONEIOS_VERSION=$VERSION \
  CC_aarch64_linux_android=$NDK_BIN/aarch64-linux-android30-clang \
  CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$NDK_BIN/aarch64-linux-android30-clang \
    "$CARGO" build --release --target aarch64-linux-android -p oneios-tool \
      --manifest-path "$ROOT/tools/oneios-tool/Cargo.toml" ||
    { echo "package: oneios-tool did not build for aarch64-linux-android (rustup target add aarch64-linux-android?)" >&2; exit 1; }
  TOOL_ANDROID=${CARGO_TARGET_DIR:-$ROOT/tools/oneios-tool/target}/aarch64-linux-android/release/oneios-tool
  [ -f "$TOOL_ANDROID" ] || { echo "package: no $TOOL_ANDROID after the build" >&2; exit 1; }
fi
"$NDK_STRIP" --strip-all -o "$STAGE/bin/oneios-tool" "$TOOL_ANDROID"
# A PIE for /system/bin/linker64, without a symbol table (the strip worked). The headers are read once into a
# variable: `readelf | grep -q` fails under pipefail when grep's early exit gives readelf SIGPIPE.
TOOL_HEADERS=$("$NDK_BIN/llvm-readelf" -h -l -S "$STAGE/bin/oneios-tool")
grep -q 'Type: *DYN' <<< "$TOOL_HEADERS" ||
  { echo "package: bin/oneios-tool is not a position-independent arm64 executable" >&2; exit 1; }
grep -q '/system/bin/linker64' <<< "$TOOL_HEADERS" ||
  { echo "package: bin/oneios-tool does not name /system/bin/linker64" >&2; exit 1; }
if grep -q -E ' \.(symtab|debug_(info|line|abbrev|str|ranges|rnglists|loclists|aranges)) ' <<< "$TOOL_HEADERS"; then
  echo "package: bin/oneios-tool still has a symbol table or debug sections" >&2; exit 1
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

# 1c. The IPSW profiles (profiles/*.toml: this repository's own text, the members' names, sizes and hashes, no
# Apple file) as lib/profiles/, for the phone's `oneios-tool serve` `fetch` (the OneOS app's Make iOS image). A package path
# has no comma (oneos/interface.md section 1): "iPhone18,2" is "iPhone18-2" there.
mkdir -p "$STAGE/lib/profiles"
for p in "$ROOT"/profiles/*.toml; do n=${p##*/}; cp "$p" "$STAGE/lib/profiles/$(echo "$n" | tr , -)"; done
chmod 0644 "$STAGE"/lib/profiles/*.toml

# 1d. The switch catalog (runtime/switches, the list xnuhost's switches line is made from) as lib/switches.json,
# written by this tree's oneios-tool (`oneios-tool switches --json`): the OneOS app's Switches screen reads it with
# `bin/oneios-tool switches --json --runtime DIR`, so it describes the xnuhost of this package whatever tool reads it
# (tools/oneios-tool/README.md, "switches"; docs/switches.md).
"$ONEIOS_TOOL" switches --json > "$STAGE/lib/switches.json" ||
  { echo "package: oneios-tool switches --json failed" >&2; exit 1; }
"$ONEIOS_TOOL" switches --json --runtime "$STAGE" > /dev/null ||
  { echo "package: lib/switches.json does not read back" >&2; exit 1; }
chmod 0644 "$STAGE/lib/switches.json"

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
# 3a. Other iOS builds' kernels (KERNEL_MORE_DIRS: folders, space-separated, each kernel/build-hosted.sh's output for
# another OS profile, ONEIOS_OS_PROFILE=ios-27.2.1-24B156): each as kernel/<build>/, the build its record's profile
# names, beside the default's (KERNEL_DIR). runtime.json lists every kernel in "kernels"; "kernel" stays the default's.
# Each must be releasable (a release never drops one without saying so).
for d in ${KERNEL_MORE_DIRS:-}; do
  [ "$HAVE_KERNEL" = 1 ] || { echo "package: KERNEL_MORE_DIRS without a default kernel: runtime.json \"kernel\" names kernel/xnu.macho" >&2; exit 1; }
  [ -f "$d/xnu.macho" ] && [ -f "$d/xnu.macho.build.json" ] || { echo "package: KERNEL_MORE_DIRS: $d holds no xnu.macho with its build record" >&2; exit 1; }
  grep -q '"releasable": true' "$d/xnu.macho.build.json" || { echo "package: KERNEL_MORE_DIRS: the kernel at $d is not releasable (see its build record)" >&2; exit 1; }
  profile=$("$ONEIOS_TOOL" json get "$d/xnu.macho.build.json" profile)
  build=${profile##*-}
  case "$build" in ''|*[!A-Za-z0-9]*) echo "package: KERNEL_MORE_DIRS: $d's record names profile '$profile'" >&2; exit 1 ;; esac
  [ ! -e "$STAGE/kernel/$build" ] || { echo "package: KERNEL_MORE_DIRS: a second kernel for build $build ($d)" >&2; exit 1; }
  mkdir -p "$STAGE/kernel/$build"
  cp "$d/xnu.macho" "$d/xnu.macho.build.json" "$STAGE/kernel/$build/"
  chmod 0644 "$STAGE/kernel/$build"/*
  echo "package: kernel for $build $(sed -n 's/.*"build_id": *"\([^"]*\)".*/\1/p' "$STAGE/kernel/$build/xnu.macho.build.json") (kernel/$build/)"
done

# 3a. The runtime's boot addon (Metal and capture plug-ins) alongside the default kernel. The image's own sidecar wins;
# when absent, xnuhost loads this copy (kernel/<build>/gpuaddon for an image of that build, else kernel/gpuaddon, when
# its plug-ins' oneios-target names the image's build) and adds its CDHashes to the boot trust cache without rebuilding
# the image: so a runtime update brings its plug-in to an image made before it (issue #86; runtime/xnuhost/src/addon.rs).
# ONEIOS_GPUADDON names the default one; METAL_BOOT_ADDON=1 (step 3e) takes each Metal folder's NativeAdrenoMetal.gpuaddon.
pack_addon() { # pack_addon ADDON DEST (DEST under the stage: kernel/gpuaddon or kernel/<build>/gpuaddon)
  local addon=$1 dest=$2 addon_bytes addon_hashes line word
  [ "$HAVE_KERNEL" = 1 ] ||
    { echo "package: $dest needs a kernel in the package (the boot addon is its sidecar)" >&2; exit 1; }
  [ -f "$addon" ] && [ -f "$addon.cdhashes" ] ||
    { echo "package: $addon and $addon.cdhashes (the pair oneios-tool ios addon writes) are needed" >&2; exit 1; }
  [ "$(head -c 8 "$addon")" = NGPUADD2 ] ||
    { echo "package: $addon is not a boot addon (NGPUADD2): make one with oneios-tool ios addon" >&2; exit 1; }
  addon_bytes=$(wc -c < "$addon" | tr -d ' ')
  [ "$addon_bytes" -le $((64 << 20)) ] ||
    { echo "package: $addon is $addon_bytes bytes, over xnuhost's 64 MiB limit for the boot addon" >&2; exit 1; }
  addon_hashes=0
  while IFS= read -r line; do
    case "$line" in ''|'#'*) continue ;; esac
    word=${line%%[[:space:]]*}
    [ "${#word}" = 40 ] && [ -z "$(printf '%s' "$word" | tr -d '0123456789abcdefABCDEF')" ] ||
      { echo "package: $addon.cdhashes: ${line%% *} is not a 40-digit CDHash" >&2; exit 1; }
    addon_hashes=$((addon_hashes + 1))
  done < "$addon.cdhashes"
  [ "$addon_hashes" -gt 0 ] ||
    { echo "package: $addon.cdhashes lists no CDHash: the plug-in's code would not be trusted" >&2; exit 1; }
  [ ! -e "$STAGE/$dest" ] || { echo "package: a second boot addon for $dest ($addon)" >&2; exit 1; }
  mkdir -p "$(dirname "$STAGE/$dest")"
  cp "$addon" "$STAGE/$dest"
  cp "$addon.cdhashes" "$STAGE/$dest.cdhashes"
  chmod 0644 "$STAGE/$dest" "$STAGE/$dest.cdhashes"
  echo "package: boot addon $addon_bytes bytes, $addon_hashes CDHashes ($dest)"
}
[ -z "${ONEIOS_GPUADDON:-}" ] || pack_addon "$ONEIOS_GPUADDON" kernel/gpuaddon

# 3b. The guest's own plug-ins, when given: METAL_DIR (userland/metal/build.sh's output folder: NativeAdrenoMetal.bundle
# and NativeAdrenoMetal.build.json) and CAMERA_DIR (userland/camera/build.sh's: H9ISP.mediacapture), as lib/guest/.
# The phone's Make iOS image (`serve make-image`) seeds them into the image it makes when the bundle's oneios-target is
# that image's (docs/image-format.md, "The Metal plug-in"). `package manifest` takes a Mach-O there only as our own
# plug-in (ad hoc signed for iOS, not com.apple.*, a bundle's files with its record's CDHashes) and names them in
# runtime.json ("metal", "camera"). lib/guest/NOTICE has the licences of the sources the bundle is built from, and
# their pins (the record's "sources"). REQUIRE_METAL=1: no METAL_DIR is an error (scripts/release/release.sh).
GUEST=$STAGE/lib/guest
HAVE_METAL=0
if [ -n "${METAL_DIR:-}" ]; then
  B=$METAL_DIR/NativeAdrenoMetal.bundle
  METAL_LICENSES_DIR=${METAL_LICENSES_DIR:-$ROOT/external}
  [ -f "$B/NativeAdrenoMetal" ] && [ -f "$B/oneios-target" ] && [ -f "$METAL_DIR/NativeAdrenoMetal.build.json" ] ||
    { echo "package: METAL_DIR=$METAL_DIR holds no whole NativeAdrenoMetal.bundle (NativeAdrenoMetal, oneios-target) with NativeAdrenoMetal.build.json beside it (userland/metal/build.sh)" >&2; exit 1; }
  mkdir -p "$GUEST"
  cp -R "$B" "$GUEST/"
  cp "$METAL_DIR/NativeAdrenoMetal.build.json" "$GUEST/"
  HAVE_METAL=1
  echo "package: Metal plug-in for $(cat "$B/oneios-target") from $METAL_DIR"
  {
    echo "lib/guest/: OneiOS's iOS plug-ins, run by the guest (iOS), never on Android. NativeAdrenoMetal.bundle is built"
    echo "from these sources (NativeAdrenoMetal.build.json \"sources\"; this repository's userland/metal/ has the patches"
    echo "applied to them, at the commit runtime.json names), linked against stubs of the SDK, with no Apple code in it:"
    echo
    # (the pins of the bundle's record; the URL of each as scripts/setup/get-references.sh or kernel/pins.sh fetches it)
    for name in NativeMetal metal2vulkan mesa-droidvm Droid-VM-UniXNU NativeAdreno AdrenoPM4 NativeMobileDisplay; do
      ref=$(awk -v n="$name" '$1 == n && $2 ~ /^https:/ { print $2, $3; exit }' "$ROOT/scripts/setup/get-references.sh" "$ROOT/kernel/pins.sh")
      pin=$("$ONEIOS_TOOL" json get --default "${ref#* }" "$METAL_DIR/NativeAdrenoMetal.build.json" "sources.$name")
      echo "  $name ${ref%% *} $pin"
    done
    echo
    echo "metal2vulkan is GPL-2.0-or-later or LGPL-3.0-or-later; Mesa (Turnip) MIT and the licences of its LICENSING.md;"
    echo "NativeMetal, NativeAdreno and AdrenoPM4 MIT; Droid-VM-UniXNU and NativeMobileDisplay BSD-2-Clause-Patent."
    echo "The corresponding source is at the URLs and commits above. Their licence texts follow."
    for f in NativeMetal/LICENSE metal2vulkan/LICENSE metal2vulkan/LICENSE-GPL-2.0 metal2vulkan/LICENSE-GPL-3.0 \
             metal2vulkan/LICENSE-LGPL-3.0 mesa-droidvm/LICENSING.md mesa-droidvm/licenses Droid-VM-UniXNU/LICENSE \
             NativeAdreno/LICENSE AdrenoPM4/LICENSE NativeMobileDisplay/LICENSE; do
      p=$METAL_LICENSES_DIR/$f
      [ -e "$p" ] || { echo "package: no $p: the Metal plug-in's licence texts come from the pinned source checkouts" >&2; exit 1; }
      # (a folder: every file below it, sorted)
      while IFS= read -r q; do
        printf '\n==== %s\n\n' "${q#"$METAL_LICENSES_DIR"/}"; cat "$q"
      done < <(find "$p" -type f | LC_ALL=C sort)
    done
  } > "$GUEST/NOTICE"
elif [ "${REQUIRE_METAL:-0}" = 1 ]; then
  echo "package: REQUIRE_METAL=1 and no METAL_DIR (userland/metal/build.sh's output folder)" >&2; exit 1
else
  echo "package: no METAL_DIR: no Metal plug-in (an image made on the phone with this runtime composites on the CPU)"
fi
if [ -n "${CAMERA_DIR:-}" ]; then
  [ "$HAVE_METAL" = 1 ] || { echo "package: CAMERA_DIR without METAL_DIR: the bundle's oneios-target says which iOS both are for" >&2; exit 1; }
  [ -f "$CAMERA_DIR/H9ISP.mediacapture" ] || { echo "package: CAMERA_DIR=$CAMERA_DIR has no H9ISP.mediacapture (userland/camera/build.sh)" >&2; exit 1; }
  mkdir -p "$GUEST/camera"
  cp "$CAMERA_DIR/H9ISP.mediacapture" "$GUEST/camera/"
  echo "package: capture plug-in from $CAMERA_DIR"
fi
# 3d. Other iOS builds of the same device (METAL_MORE_DIRS: folders, space-separated, each userland/metal/build.sh's
# output for another target, ONEIOS_METAL_TARGET="iPhone18,2 24B156", with camera/H9ISP.mediacapture in it when the
# capture plug-in goes too): each as lib/guest/<build>/, the build its bundle's oneios-target names, beside the default's
# (METAL_DIR). Its record must name the same sources as the default's (lib/guest/NOTICE lists those). runtime.json lists
# every target in "metals" and "cameras"; "metal" and "camera" stay the default's (docs/releases.md).
for d in ${METAL_MORE_DIRS:-}; do
  [ "$HAVE_METAL" = 1 ] || { echo "package: METAL_MORE_DIRS without METAL_DIR: runtime.json \"metal\" names the default target's bundle" >&2; exit 1; }
  B=$d/NativeAdrenoMetal.bundle
  [ -f "$B/NativeAdrenoMetal" ] && [ -f "$B/oneios-target" ] && [ -f "$d/NativeAdrenoMetal.build.json" ] ||
    { echo "package: METAL_MORE_DIRS: $d holds no whole NativeAdrenoMetal.bundle with NativeAdrenoMetal.build.json beside it" >&2; exit 1; }
  target=$(cat "$B/oneios-target")
  build=${target#* }
  case "$build" in ''|*[!A-Za-z0-9]*|"$target") echo "package: $B/oneios-target says '$target', not '<device> <build>'" >&2; exit 1 ;; esac
  [ ! -e "$GUEST/$build" ] || { echo "package: METAL_MORE_DIRS: a second bundle for build $build ($d)" >&2; exit 1; }
  for name in NativeMetal metal2vulkan mesa-droidvm Droid-VM-UniXNU NativeAdreno AdrenoPM4 NativeMobileDisplay; do
    [ "$("$ONEIOS_TOOL" json get --default none "$d/NativeAdrenoMetal.build.json" "sources.$name")" = \
      "$("$ONEIOS_TOOL" json get --default none "$METAL_DIR/NativeAdrenoMetal.build.json" "sources.$name")" ] ||
      { echo "package: $d is built from another $name than $METAL_DIR (lib/guest/NOTICE lists the default's sources): build both from one checkout" >&2; exit 1; }
  done
  mkdir -p "$GUEST/$build"
  cp -R "$B" "$GUEST/$build/"
  cp "$d/NativeAdrenoMetal.build.json" "$GUEST/$build/"
  echo "package: Metal plug-in for $target from $d (lib/guest/$build/)"
  if [ -f "$d/camera/H9ISP.mediacapture" ]; then
    mkdir -p "$GUEST/$build/camera"
    cp "$d/camera/H9ISP.mediacapture" "$GUEST/$build/camera/"
    echo "package: capture plug-in for $target from $d/camera"
  fi
done

# 3e. METAL_BOOT_ADDON=1: each Metal folder's NativeAdrenoMetal.gpuaddon (userland/metal/build.sh `addon`: the same
# bundle as lib/guest/, as a boot addon with its .cdhashes) as the runtime's boot addon for its build: METAL_DIR's as
# kernel/gpuaddon (unless ONEIOS_GPUADDON gave one), each METAL_MORE_DIRS's as kernel/<build>/gpuaddon (step 3a).
if [ "${METAL_BOOT_ADDON:-0}" = 1 ]; then
  [ "$HAVE_METAL" = 1 ] || { echo "package: METAL_BOOT_ADDON=1 without METAL_DIR" >&2; exit 1; }
  [ -e "$STAGE/kernel/gpuaddon" ] || pack_addon "$METAL_DIR/NativeAdrenoMetal.gpuaddon" kernel/gpuaddon
  for d in ${METAL_MORE_DIRS:-}; do
    target=$(cat "$d/NativeAdrenoMetal.bundle/oneios-target")
    pack_addon "$d/NativeAdrenoMetal.gpuaddon" "kernel/${target#* }/gpuaddon"
  done
fi

# 3c. The Galaxy A56's GPU plug-ins for iOS 16.5, when given: METAL_AMD_DIR (userland/metal-amd's output folder,
# out/metal-amd: OneiOSRadeonMetal.bundle from `air/build.sh plugin`, gl/OneiOSRadeonGL.bundle from the GL hook of
# `build.sh all`; docs/a56-gpu.md 6). Beside the Adreno plug-in, never instead of it: lib/guest/OneiOSRadeonMetal.bundle
# and lib/guest/OneiOSRadeonGL.bundle, each with its build record (`oneios-tool ios bundle-record`: the CDHash of each
# Mach-O, the target, the source pins), which `package manifest` checks as it checks the Adreno bundle (our own ad hoc
# signed iOS code only) and names in runtime.json "metal_a56". The phone builds an iOS 16.5 image's kernel.kc.gpuaddon
# of them at Update the kernel and Make iOS image (`oneios-tool image a56-gpu-place`); an iOS 27 image never takes them.
# REQUIRE_METAL_AMD=1: no METAL_AMD_DIR is an error.
IOSPLUS10_PIN=146e889430bab755329dde7104cc0fa002f44bfb   # akshaynexus/iOSPlus10 origin/main: NegroniGL (userland/metal-amd/gl/NOTICE)
if [ -n "${METAL_AMD_DIR:-}" ]; then
  AB=$METAL_AMD_DIR/OneiOSRadeonMetal.bundle GB=$METAL_AMD_DIR/gl/OneiOSRadeonGL.bundle
  [ -f "$AB/OneiOSRadeonMetal" ] && [ -f "$AB/libOneiOSRadeonAir.dylib" ] && [ -f "$AB/oneios-target" ] ||
    { echo "package: METAL_AMD_DIR=$METAL_AMD_DIR holds no whole OneiOSRadeonMetal.bundle (userland/metal-amd/air/build.sh plugin)" >&2; exit 1; }
  [ -f "$GB/OneiOSRadeonGL" ] && [ -f "$GB/oneios-target" ] ||
    { echo "package: METAL_AMD_DIR=$METAL_AMD_DIR holds no whole gl/OneiOSRadeonGL.bundle (userland/metal-amd/build.sh all)" >&2; exit 1; }
  A56_TARGET=$(cat "$AB/oneios-target")
  [ "$(cat "$GB/oneios-target")" = "$A56_TARGET" ] ||
    { echo "package: OneiOSRadeonGL.bundle is for $(cat "$GB/oneios-target"), OneiOSRadeonMetal.bundle for $A56_TARGET" >&2; exit 1; }
  mkdir -p "$GUEST"
  cp -R "$AB" "$GB" "$GUEST/"
  "$ONEIOS_TOOL" ios bundle-record "$ROOT" "$GUEST/OneiOSRadeonMetal.bundle" "$GUEST/OneiOSRadeonMetal.build.json" "$A56_TARGET" \
    mesa-droidvm Jesvi-iOS
  "$ONEIOS_TOOL" ios bundle-record "$ROOT" "$GUEST/OneiOSRadeonGL.bundle" "$GUEST/OneiOSRadeonGL.build.json" "$A56_TARGET" \
    mesa-droidvm "iOSPlus10=$IOSPLUS10_PIN"
  echo "package: the A56's GPU plug-ins (Metal, OpenGL ES) for $A56_TARGET from $METAL_AMD_DIR"
  LIBDRM=$METAL_AMD_DIR/libdrm-2.4.133/amdgpu/amdgpu_bo.c
  [ -f "$LIBDRM" ] || { echo "package: no $LIBDRM: libdrm's notice comes from the A56 Mesa build (userland/metal-amd/build.sh)" >&2; exit 1; }
  {
    [ ! -s "$GUEST/NOTICE" ] || echo
    echo "lib/guest/OneiOSRadeonMetal.bundle and lib/guest/OneiOSRadeonGL.bundle: the Galaxy A56's GPU plug-ins for iOS 16.5"
    echo "($A56_TARGET), run by the guest (iOS), never on Android, built from these sources (their .build.json \"sources\";"
    echo "this repository's userland/metal-amd/ has the patches and the ported code, at the commit runtime.json names),"
    echo "linked against stubs of the SDK, with no Apple code in them:"
    echo
    for name in mesa-droidvm Jesvi-iOS; do
      ref=$(awk -v n="$name" '$1 == n && $2 ~ /^https:/ { print $2, $3; exit }' "$ROOT/scripts/setup/get-references.sh")
      echo "  $name ${ref%% *} ${ref#* }"
    done
    echo "  iOSPlus10 https://github.com/akshaynexus/iOSPlus10.git $IOSPLUS10_PIN (NegroniGL, userland/negronigl)"
    echo "  libdrm https://dri.freedesktop.org/libdrm/libdrm-2.4.133.tar.xz (2.4.133, Mesa's subprojects/libdrm.wrap)"
    echo
    echo "Mesa (radeonsi, ACO, the amdgpu virtio context, Gallium) is MIT and the licences of its LICENSING.md; Jesvi-iOS's"
    echo "AIR frontend and Metal device (outside its xnu/) MIT; iOSPlus10's NegroniGL MIT; libdrm MIT. The corresponding"
    echo "source is at the URLs and commits above. Their licence texts follow."
    for f in mesa-droidvm/LICENSING.md mesa-droidvm/licenses Jesvi-iOS/LICENSE Jesvi-iOS/NOTICE; do
      p=$ROOT/external/$f
      [ -e "$p" ] || { echo "package: no $p: the A56 plug-ins' licence texts come from external/ (scripts/setup/get-references.sh)" >&2; exit 1; }
      while IFS= read -r q; do
        printf '\n==== %s\n\n' "${q#"$ROOT"/external/}"; cat "$q"
      done < <(find "$p" -type f | LC_ALL=C sort)
    done
    for q in "$ROOT/userland/metal-amd/NOTICE" "$ROOT/userland/metal-amd/gl/NOTICE"; do
      printf '\n==== %s\n\n' "${q#"$ROOT"/}"; cat "$q"
    done
    printf '\n==== libdrm 2.4.133 (amdgpu/amdgpu_bo.c, its notice)\n\n'
    sed -n '1,/\*\//p' "$LIBDRM"
  } >> "$GUEST/NOTICE"
elif [ "${REQUIRE_METAL_AMD:-0}" = 1 ]; then
  echo "package: REQUIRE_METAL_AMD=1 and no METAL_AMD_DIR (userland/metal-amd's output folder)" >&2; exit 1
fi

# 4. Notices.
{
  echo "OneiOS runtime $VERSION (https://github.com/akshaynexus/OneiOS)"
  echo
  echo "This package holds OneiOS's own code and builds only. It holds no Apple firmware: the iOS image set is"
  echo "built by its owner from Apple's restore files and is never part of a release."
  if [ "$HAVE_METAL" = 1 ]; then
    echo
    echo "lib/guest/ holds OneiOS's iOS plug-ins (the Metal plug-in, the capture plug-in), built from source: their"
    echo "licences and sources are in lib/guest/NOTICE."
  fi
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
# The phone's oneios-tool by itself (the same bytes as the package's bin/oneios-tool), for a developer's adb push
# and for the release page.
TOOL_NAME=oneios-tool-$VERSION-android-arm64
cp "$STAGE/bin/oneios-tool" "$OUT/$TOOL_NAME" && chmod 0755 "$OUT/$TOOL_NAME"
( cd "$OUT" && "${SHA256[@]}" "$TOOL_NAME" > "$TOOL_NAME.sha256" )
rm -rf "$WORK"
echo "package: $OUT/$NAME.tar.zst ($(wc -c < "$OUT/$NAME.tar.zst" | tr -d ' ') bytes)"
cat "$OUT/$NAME.tar.zst.sha256"
echo "package: $OUT/$TOOL_NAME ($(wc -c < "$OUT/$TOOL_NAME" | tr -d ' ') bytes)"
cat "$OUT/$TOOL_NAME.sha256"
