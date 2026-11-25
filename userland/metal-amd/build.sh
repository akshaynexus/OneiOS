#!/bin/bash
# Build Mesa's radeonsi for the Galaxy A56's iOS 16.5 (20F66, iPhone X: arm64-apple-ios16.5), over the amdgpu virtio
# native context and the Darwin vdrm backend (docs/a56-gpu.md section 3), and the guest tool oneios-amdgpu-probe.
#
#   userland/metal-amd/build.sh            mesa, then probe (about 10 minutes the first time on the build laptop)
#   userland/metal-amd/build.sh mesa       Mesa set up and liboneios_radeonsi.dylib built (and the Metal layer's hook)
#   userland/metal-amd/build.sh probe      oneios-amdgpu-probe (needs mesa's build directory)
#   METAL_AMD_OUT=DIR ...                  another output folder (default out/metal-amd)
#
# What is built, into $METAL_AMD_OUT/ios:
#   liboneios_radeonsi.dylib   radeonsi (ACO, no LLVM), its amdgpu winsys and Mesa's amdgpu virtio context, exporting
#                              oneios_radeonsi_screen_create (src/gallium/targets/oneios-radeonsi/oneios_radeonsi.h); install
#                              name @loader_path/liboneios_radeonsi.dylib: it goes beside the Metal plug-in that links it
#   oneios-amdgpu-probe        the lasting guest tool: device, capset facts, a guest BO, a GFX NOP IB and its fence; with
#                              --screen the radeonsi screen and a clear read back (one PASS or FAIL line a step)
#   oneios-radeonsi.build.txt  the SDK, stubs, target, Mesa and libdrm pins, the patches, sizes and CDHashes
# Both binaries are signed by `oneios-tool ios sign` and checked by `oneios-tool ios macho-check` against the 20F66 stubs.
#
# How: external/mesa-droidvm (cecc0e9, MIT) is cloned into $METAL_AMD_OUT/mesa (never changed in place) and
# userland/metal-amd/patches/mesa/*.patch are applied in name order (0001-4999 this build's; 5xxx-air-*.patch the Metal
# layer's, after them). The clone is made again when the patch set changes. Darwin has no DRM: the build takes libdrm's
# headers and its amdgpu library from the libdrm release Mesa's own wrap pins (subprojects/libdrm.wrap: 2.4.133, its
# sha256 checked; fetched once into $METAL_AMD_OUT/dl), and patch 0001's src/oneios/oneios_drm.c answers the DRM calls.
# meson cross build, release, -Dgallium-drivers=radeonsi -Damdgpu-virtio=true -Dllvm=disabled -Damd-use-llvm=false
# -Doneios-radeonsi-screen=true, no GL, EGL or Vulkan; the compilers are scripts/ios/ios-clang (links through ios-ld
# against the 20F66 stubs). The build directory is set up again when the cross file's inputs change.
# The Metal layer's hook: after `mesa`, when userland/metal-amd/air/build.sh exists it is run as
#   userland/metal-amd/air/build.sh MESA_SOURCE_DIR MESA_BUILD_DIR
# and then the OpenGL ES hook (userland/metal-amd/gl/build.sh MESA_SOURCE_DIR MESA_BUILD_DIR: GLES turned on in this
# build, OneiOSRadeonGL.bundle with radeonsi through K3 and oneios-gl-probe in $METAL_AMD_OUT/gl; docs/a56-gpu.md 4a).
#
# Needs: scripts/setup/ios-toolchain.sh's toolchain (Linux: the SDK from xtool, ld64 in out/ios-toolchain), the 20F66
# stubs (out/ios16-tbd: docs/building.md section 9), external/mesa-droidvm (scripts/setup/get-references.sh), meson,
# ninja, bison, flex, llvm-ar, python3 with mako and PyYAML (Mesa's meson build: its Python, not this project's), curl or
# an already fetched libdrm-2.4.133.tar.xz in $METAL_AMD_OUT/dl. Disk: about 1.5 GB.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"
HERE=$ROOT/userland/metal-amd
OUT=${METAL_AMD_OUT:-$ROOT/out/metal-amd}
EXT=${ONEIOS_EXTERNAL:-$ROOT/external}
# K4: 20F66 is the iPhone X's build: arm64, its own stubs.
export IOS_TBD=${IOS_TBD:-$ROOT/out/ios16-tbd} IOS_MIN=${IOS_MIN:-16.5}
export IOS_TARGET=${IOS_TARGET:-arm64-apple-ios$IOS_MIN}
. "$ROOT/scripts/ios/env.sh"
. "$ROOT/scripts/lib/oneios-tool.sh"
oneios_tool "$ROOT"
JOBS=${JOBS:-$(getconf _NPROCESSORS_ONLN)}
MESA_PIN=cecc0e96d7193c72356a31abb0cc589e4fc0415e
SRC=$OUT/mesa
BUILD=$OUT/build-ios
DEST=$OUT/ios
say() { echo "metal-amd: $*"; }

[ -f "$IOS_TBD/usr/lib/libSystem.B.tbd" ] || { echo "metal-amd: no 20F66 stubs in $IOS_TBD (docs/building.md section 9)" >&2; exit 1; }

# The patch set's identity: the clone is made again when it changes.
patch_list() { ls "$HERE"/patches/mesa/*.patch 2>/dev/null | LC_ALL=C sort; }
patch_sum() { patch_list | while read -r p; do printf '%s ' "$(basename "$p")"; cksum < "$p"; done; }

# libdrm: Mesa's own pin of it (subprojects/libdrm.wrap), fetched once and checked.
libdrm() {
  local wrap=$EXT/mesa-droidvm/subprojects/libdrm.wrap dir url sum file
  dir=$(sed -n 's/^directory = //p' "$wrap")
  url=$(sed -n 's/^source_url = //p' "$wrap")
  sum=$(sed -n 's/^source_hash = //p' "$wrap")
  file=$OUT/dl/$(sed -n 's/^source_filename = //p' "$wrap")
  LIBDRM=$OUT/$dir
  [ -f "$LIBDRM/amdgpu/amdgpu_vamgr.c" ] && return 0
  mkdir -p "$OUT/dl"
  [ -f "$file" ] || { say "fetching $url"; curl -sSLfo "$file.part" "$url" && mv "$file.part" "$file"; }
  echo "$sum  $file" | sha256sum -c --quiet - || { echo "metal-amd: $file: not the sha256 Mesa's wrap pins" >&2; exit 1; }
  tar -C "$OUT" -xf "$file"
  say "libdrm $dir unpacked in $LIBDRM"
}

source_tree() {
  [ -d "$EXT/mesa-droidvm/.git" ] || { echo "metal-amd: no external/mesa-droidvm (scripts/setup/get-references.sh mesa-droidvm)" >&2; exit 1; }
  local want
  want=$(patch_sum)
  if [ -d "$SRC/.git" ] && [ "$(cat "$OUT/mesa.patches" 2>/dev/null)" != "$want" ]; then
    say "the patch set changed: $SRC made again"
    rm -rf "${SRC:?}" "${BUILD:?}"
  fi
  if [ ! -d "$SRC/.git" ]; then
    git clone -q "$EXT/mesa-droidvm" "$SRC"
    git -C "$SRC" checkout -q "$MESA_PIN"
    patch_list | while read -r p; do
      git -C "$SRC" apply "$p" || { echo "metal-amd: $(basename "$p") does not apply to $MESA_PIN" >&2; exit 1; }
      say "applied $(basename "$p")"
    done
    printf '%s\n' "$want" > "$OUT/mesa.patches"
  fi
  [ "$(git -C "$SRC" rev-parse HEAD)" = "$MESA_PIN" ] || { echo "metal-amd: $SRC is not at $MESA_PIN" >&2; exit 1; }
}

cross_file() {
  local ar
  ar=$(command -v llvm-ar || true)
  [ "$IOS_HOST" = Darwin ] && [ -z "$ar" ] && ar=$(/usr/bin/xcrun --sdk iphoneos --find ar)
  [ -n "$ar" ] || { echo "metal-amd: no llvm-ar" >&2; exit 1; }
  # -D_DARWIN_C_SOURCE: flex 2.6.10's scanners define _POSIX_C_SOURCE, which hides Darwin's extensions (docs/gpu.md 7).
  cat <<EOF
[binaries]
c = '$IOS_DIR/ios-clang'
cpp = '$IOS_DIR/ios-clang++'
objc = '$IOS_DIR/ios-clang'
ar = '$ar'
strip = '$IOS_CCTOOLS/strip'
pkg-config = '/usr/bin/false'
[host_machine]
system = 'darwin'
cpu_family = 'aarch64'
cpu = 'arm64'
endian = 'little'
[properties]
needs_exe_wrapper = true
[built-in options]
c_args = ['-D_DARWIN_C_SOURCE', '-fstack-protector-strong']
cpp_args = ['-D_DARWIN_C_SOURCE', '-fstack-protector-strong']
objc_args = ['-D_DARWIN_C_SOURCE']
EOF
  # What the file was made from (the SDK, the target, the stubs, the compiler): a change sets the build up again.
  printf '# %s %s %s %s %s\n' "$IOS_SDK" "$IOS_TARGET" "$IOS_TBD" "$("$IOS_CLANG" --version | head -1)" "$IOS_LD_VERSION"
}

mesa() {
  libdrm
  source_tree
  local cross=$OUT/ios.cross now
  now=$(cross_file)
  if [ -f "$BUILD/build.ninja" ] && [ "$(cat "$cross" 2>/dev/null)" != "$now" ]; then
    say "the cross file's inputs changed: $BUILD set up again"
    rm -rf "${BUILD:?}"
  fi
  printf '%s\n' "$now" > "$cross"
  if [ ! -f "$BUILD/build.ninja" ]; then
    # Mesa's build runs its own Python (mako); nothing of this project's.
    meson setup "$BUILD" "$SRC" --cross-file "$cross" --wrap-mode=nodownload \
      -Dbuildtype=release -Db_ndebug=true \
      -Dgallium-drivers=radeonsi -Damdgpu-virtio=true -Dllvm=disabled -Damd-use-llvm=false \
      -Doneios-radeonsi-screen=true -Doneios-libdrm-dir="$LIBDRM" \
      -Dvulkan-drivers= -Dplatforms= -Dglx=disabled -Degl=disabled -Dgbm=disabled -Dopengl=false -Dgles1=disabled \
      -Dgles2=disabled -Dxmlconfig=disabled -Dtools= -Dbuild-tests=false -Dvideo-codecs= -Dvalgrind=disabled \
      -Dlibunwind=disabled -Dlmsensors=disabled -Dzstd=disabled -Dexpat=disabled -Dgallium-va=disabled \
      -Dgallium-rusticl=false -Dshader-cache=disabled -Dvulkan-layers= -Dperfetto=false \
      > "$OUT/meson-setup-ios.log" 2>&1 || { tail -30 "$OUT/meson-setup-ios.log" >&2; exit 1; }
  fi
  ninja -C "$BUILD" -j "$JOBS" src/gallium/targets/oneios-radeonsi/liboneios_radeonsi.dylib
  mkdir -p "$DEST"
  install -m 755 "$BUILD/src/gallium/targets/oneios-radeonsi/liboneios_radeonsi.dylib" "$DEST/"
  "$ONEIOS_TOOL" ios sign --identifier com.oneios.radeonsi "$DEST/liboneios_radeonsi.dylib" | tee "$DEST/liboneios_radeonsi.dylib.cdhash"
  "$ONEIOS_TOOL" ios macho-check --arch arm64 --tbd "$IOS_TBD" "$DEST/liboneios_radeonsi.dylib" | tee "$DEST/liboneios_radeonsi.dylib.check.txt"
  if [ -x "$HERE/air/build.sh" ]; then
    say "the Metal layer's hook: $HERE/air/build.sh $SRC $BUILD"
    "$HERE/air/build.sh" "$SRC" "$BUILD"
  fi
  # OpenGL ES for CoreAnimation (OneiOSRadeonGL, docs/a56-gpu.md 4a): GLES on in this build and the bundle, in $OUT/gl
  if [ -x "$HERE/gl/build.sh" ]; then
    say "the OpenGL ES hook: $HERE/gl/build.sh $SRC $BUILD"
    METAL_AMD_GL_OUT="$OUT/gl" "$HERE/gl/build.sh" "$SRC" "$BUILD"
  fi
}

probe() {
  [ -f "$BUILD/build.ninja" ] || { echo "metal-amd: no $BUILD: run the mesa step first" >&2; exit 1; }
  ninja -C "$BUILD" -j "$JOBS" src/gallium/targets/oneios-radeonsi/oneios-amdgpu-probe
  mkdir -p "$DEST"
  install -m 755 "$BUILD/src/gallium/targets/oneios-radeonsi/oneios-amdgpu-probe" "$DEST/"
  "$ONEIOS_TOOL" ios sign --identifier com.oneios.amdgpu-probe "$DEST/oneios-amdgpu-probe" | tee "$DEST/oneios-amdgpu-probe.cdhash"
  "$ONEIOS_TOOL" ios macho-check --arch arm64 --tbd "$IOS_TBD" "$DEST/oneios-amdgpu-probe" | tee "$DEST/oneios-amdgpu-probe.check.txt"
}

record() {
  local r=$DEST/oneios-radeonsi.build.txt f
  {
    echo "sdk $IOS_SDK_VERSION ($IOS_SDK)"
    echo "target $IOS_TARGET; stubs $IOS_TBD; linker ld64-$IOS_LD_VERSION; $("$IOS_CLANG" --version | head -1)"
    echo "mesa external/mesa-droidvm $MESA_PIN; libdrm $(basename "$LIBDRM") ($(sed -n 's/^source_hash = //p' "$EXT/mesa-droidvm/subprojects/libdrm.wrap"))"
    patch_sum | sed 's/^/patch /'
    for f in liboneios_radeonsi.dylib oneios-amdgpu-probe; do
      [ -f "$DEST/$f" ] && echo "$f $(wc -c < "$DEST/$f") bytes, cdhash $(cut -d' ' -f1 "$DEST/$f.cdhash")"
    done
  } > "$r"
  say "record $r"
}

say "SDK $IOS_SDK_VERSION ($IOS_SDK); stubs $IOS_TBD; target $IOS_TARGET; out $OUT"
case "${1:-all}" in
  mesa) mesa; record ;;
  probe) libdrm; probe; record ;;
  all) mesa; probe; record ;;
  *) sed -n '2,8p' "$0"; exit 2 ;;
esac
