#!/bin/bash
# The Galaxy A56's OpenGL ES, OneiOSRadeonGL.bundle, for iOS 16.5 (20F66, the iPhone X build: arm64): Mesa's GLES
# frontend over the A56's Gallium screen as the GLD/GLI plug-in OpenGLES loads by the accelerator's IOGLESBundleName
# (NegroniGL of iOSPlus10, ported: userland/metal-amd/gl/NOTICE). docs/a56-gpu.md section 4a.
#
#   userland/metal-amd/gl/build.sh MESA_SRC MESA_BUILD   the guest Mesa stage's hook (userland/metal-amd/build.sh
#        mesa, after the Metal layer's): copies userland/metal-amd/gl to MESA_SRC/src/gallium/targets/oneios-gl,
#        applies patches/mesa/5101-oneios-gl-target.patch once, turns GLES on (-Dgles2=enabled), builds the bundle's
#        executable and oneios-gl-probe in MESA_BUILD (radeonsi through K3 when the tree has targets/oneios-radeonsi),
#        then `plugin MESA_BUILD`
#   userland/metal-amd/gl/build.sh host                  the host test's Mesa: external/mesa-droidvm at its pin +
#        5101, softpipe only, GLES, in $OUT/host-build: oneios_gl_test (host-test.sh runs it)
#   userland/metal-amd/gl/build.sh mesa-ios              the same Mesa for arm64-apple-ios16.5, softpipe only, in
#        $OUT/mesa-ios: a bundle to check the link with before the guest Mesa stage's radeonsi (its gate then gives every
#        process but backboardd softpipe, and backboardd no GL)
#   userland/metal-amd/gl/build.sh plugin [MESA_BUILD]   the bundle $OUT/OneiOSRadeonGL.bundle (Info.plist,
#        OneiOSRadeonGL, oneios-target "iPhone10,3 20F66") and $OUT/oneios-gl-probe from MESA_BUILD (default
#        $OUT/mesa-ios), signed (`oneios-tool ios sign`), checked against out/ios16-tbd (`ios macho-check`)
#
# Settings (environment): METAL_AMD_GL_OUT (default out/metal-amd-gl; the guest hook writes beside MESA_BUILD:
# MESA_BUILD/../gl), IOS_TBD (default out/ios16-tbd of this tree, else of the main checkout), IOS_LD, JOBS.
# Needs: meson, ninja, bison, flex, llvm-strip, clang, sh and awk (gen-gli.sh, plugin/gen-gld.sh); Mesa's own build
# needs python3 with mako and pyyaml (Mesa's Python, not this project's). Disk: about 700 MB a Mesa build.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
OUT=${METAL_AMD_GL_OUT:-$ROOT/out/metal-amd-gl}
JOBS=${JOBS:-$(getconf _NPROCESSORS_ONLN)}
MESA_PIN=cecc0e9                         # external/mesa-droidvm (docs/references.md): the guest Mesa stage's Mesa
TARGET_ID="iPhone10,3 20F66"             # K3/K4: the image this bundle is for
TARGET=src/gallium/targets/oneios-gl
say() { echo "metal-amd-gl: $*"; }

# Our sources into a Mesa tree (copied, not linked: the tree stays whole when this checkout moves), and 5101 once.
into_mesa() {
  local src=$1 p=$HERE/patches/mesa/5101-oneios-gl-target.patch
  [ -f "$src/meson.options" ] || { echo "metal-amd-gl: $src is no Mesa tree" >&2; exit 1; }
  rm -rf "${src:?}/$TARGET"
  mkdir -p "$src/$TARGET"
  (cd "$HERE" && tar cf - --exclude=build.sh --exclude=host-test.sh --exclude=patches --exclude=NOTICE .) |
    tar xf - -C "$src/$TARGET"
  if ! git -C "$src" apply --reverse --check "$p" 2>/dev/null; then
    git -C "$src" apply "$p"
    say "applied $(basename "$p") to $src"
  fi
}

# The pinned Mesa, in a clone of its own (the objects shared with external/mesa-droidvm, which stays untouched)
mesa_tree() {
  local src=$OUT/mesa-src
  [ -d "$ROOT/external/mesa-droidvm/.git" ] ||
    { echo "metal-amd-gl: no external/mesa-droidvm (scripts/setup/get-references.sh mesa-droidvm)" >&2; exit 1; }
  if [ ! -d "$src/.git" ]; then
    git clone -q --shared --no-checkout "$ROOT/external/mesa-droidvm" "$src"
    git -C "$src" checkout -q "$MESA_PIN"
  fi
  [ "$(git -C "$src" rev-parse --short=7 HEAD)" = "$MESA_PIN" ] || { echo "metal-amd-gl: $src is not at $MESA_PIN" >&2; exit 1; }
  into_mesa "$src"
}

# Mesa's options for GLES alone (no desktop GL, EGL, GLX, Vulkan or window system: the frontend is ours)
MESA_OPTS=(--wrap-mode=nofallback -Dplatforms= -Dvulkan-drivers= -Dglx=disabled -Degl=disabled -Dgbm=disabled
  -Dllvm=disabled -Dopengl=false -Dgles1=disabled -Dgles2=enabled -Dzstd=disabled -Dexpat=disabled -Dvalgrind=disabled
  -Dlibunwind=disabled -Dglvnd=disabled -Dvideo-codecs= -Dtools= -Dgallium-va=disabled
  -Dgallium-mediafoundation=disabled -Dlmsensors=disabled -Dbuild-tests=false -Dshader-cache=disabled)

host() {
  mesa_tree
  local b=$OUT/host-build
  [ -f "$b/build.ninja" ] || meson setup "$b" "$OUT/mesa-src" --buildtype=debugoptimized "${MESA_OPTS[@]}" \
    -Dgallium-drivers=softpipe
  ninja -C "$b" -j "$JOBS" "$TARGET/oneios_gl_test"
  say "host: $b/$TARGET/oneios_gl_test"
}

# The iOS 16.5 toolchain (K4): scripts/ios/env.sh with the 20F66 stubs, behind wrappers so that meson and ninja get
# the same compiler every time (userland/metal-amd/air/build.sh's way).
ios_env() {
  local main=/home/akshay/Documents/GitHub/OneiOS
  [ -d "$ROOT/out/ios16-tbd" ] && : "${IOS_TBD:=$ROOT/out/ios16-tbd}" || : "${IOS_TBD:=$main/out/ios16-tbd}"
  [ -x "$ROOT/out/ios-toolchain/cctools/bin/ld" ] && : "${IOS_LD:=$ROOT/out/ios-toolchain/cctools/bin/ld}" ||
    : "${IOS_LD:=$main/out/ios-toolchain/cctools/bin/ld}"
  export IOS_TBD IOS_LD IOS_MIN=16.5 IOS_TARGET=arm64-apple-ios16.5
  [ -f "$IOS_TBD/usr/lib/libSystem.B.tbd" ] || { echo "metal-amd-gl: no 20F66 stubs at $IOS_TBD (docs/building.md)" >&2; exit 1; }
  . "$ROOT/scripts/ios/env.sh"
  BIN=$OUT/ios16-bin
  mkdir -p "$BIN"
  for t in clang clang++; do
    printf '#!/bin/sh\nIOS_TBD=%s IOS_LD=%s IOS_MIN=16.5 IOS_TARGET=arm64-apple-ios16.5 exec %s "$@"\n' \
      "$IOS_TBD" "$IOS_LD" "$ROOT/scripts/ios/ios-$t" > "$BIN/$t"
    chmod +x "$BIN/$t"
  done
}

mesa_ios() {
  mesa_tree
  ios_env
  local b=$OUT/mesa-ios cross=$OUT/ios16.cross
  cat > "$cross" <<EOF
[binaries]
c = '$BIN/clang'
cpp = '$BIN/clang++'
objc = '$BIN/clang'
ar = 'llvm-ar'
strip = 'llvm-strip'
pkg-config = 'false'

[built-in options]
c_args = ['-D_DARWIN_C_SOURCE']
cpp_args = ['-D_DARWIN_C_SOURCE']

[host_machine]
system = 'darwin'
subsystem = 'ios'
cpu_family = 'aarch64'
cpu = 'arm64'
endian = 'little'

[properties]
needs_exe_wrapper = true
EOF
  [ -f "$b/build.ninja" ] || meson setup "$b" "$OUT/mesa-src" --cross-file "$cross" --buildtype=release -Db_ndebug=true \
    "${MESA_OPTS[@]}" -Dgallium-drivers=softpipe
  ninja -C "$b" -j "$JOBS" "$TARGET/OneiOSRadeonGL.bin" "$TARGET/oneios-gl-probe"
  say "mesa-ios: $b/$TARGET/OneiOSRadeonGL.bin (softpipe only: no GPU screen)"
}

# The guest Mesa stage's call: its tree and its build directory (set up by its own meson setup, GLES off)
guest_mesa() {
  local src=$1 b=$2
  [ -f "$b/build.ninja" ] || { echo "metal-amd-gl: $b is no meson build directory" >&2; exit 1; }
  # the guest stage's compilers (scripts/ios/ios-clang) read the target from the environment at every call: K4's, here
  # too, so that a call of this hook on its own does not compile for the SDK's default (arm64e, iOS 27)
  ios_env
  into_mesa "$src"
  # GLES, and softpipe beside radeonsi: the CPU stand-in of every process but backboardd when radeonsi does not open there
  # (CoreImage dies on a nil EAGLContext: gl_core.c softpipe_allowed)
  local drivers=softpipe
  [ -f "$src/src/gallium/targets/oneios-radeonsi/oneios_radeonsi.c" ] && drivers=radeonsi,softpipe
  meson configure "$b" -Dgles2=enabled -Dgallium-drivers=$drivers
  ninja -C "$b" -j "$JOBS" "$TARGET/OneiOSRadeonGL.bin" "$TARGET/oneios-gl-probe"
  local screen=softpipe
  [ -f "$src/src/gallium/targets/oneios-radeonsi/oneios_radeonsi.c" ] && screen="radeonsi (K3) and softpipe"
  say "$b/$TARGET/OneiOSRadeonGL.bin (screen: $screen)"
  OUT=${METAL_AMD_GL_OUT:-$(cd "$b/.." && pwd)/gl}
  plugin "$b"
}

plugin() {
  local mesa_build=${1:-$OUT/mesa-ios}
  local exe=$mesa_build/$TARGET/OneiOSRadeonGL.bin probe=$mesa_build/$TARGET/oneios-gl-probe
  [ -f "$exe" ] || { echo "metal-amd-gl: no $exe (build.sh mesa-ios, or the guest Mesa stage's build)" >&2; exit 1; }
  ios_env
  . "$ROOT/scripts/lib/oneios-tool.sh"
  oneios_tool "$ROOT"
  local bundle=$OUT/OneiOSRadeonGL.bundle
  rm -rf "$bundle"
  mkdir -p "$bundle"
  cp "$HERE/plugin/Info.plist" "$bundle/Info.plist"
  llvm-strip -x -o "$bundle/OneiOSRadeonGL" "$exe"
  echo "$TARGET_ID" > "$bundle/oneios-target"
  "$ONEIOS_TOOL" ios sign "$bundle/OneiOSRadeonGL"
  cp "$probe" "$OUT/oneios-gl-probe"
  "$ONEIOS_TOOL" ios sign "$OUT/oneios-gl-probe"
  "$ONEIOS_TOOL" ios macho-check --arch arm64 --tbd "$IOS_TBD" "$bundle/OneiOSRadeonGL" "$OUT/oneios-gl-probe" |
    tee "$OUT/OneiOSRadeonGL.check.txt"
  say "bundle $bundle (from $mesa_build), probe $OUT/oneios-gl-probe"
}

case "${1:-}" in
  host) host ;;
  mesa-ios) mesa_ios ;;
  plugin) plugin "${2:-}" ;;
  ''|-h|--help) sed -n '2,24p' "$0"; exit 2 ;;
  *) [ $# -eq 2 ] || { sed -n '2,24p' "$0"; exit 2; }; guest_mesa "$1" "$2" ;;
esac
