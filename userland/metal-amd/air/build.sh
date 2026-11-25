#!/bin/bash
# The Galaxy A56's Metal plug-in, OneiOSRadeonMetal.bundle, for iOS 16.5 (20F66, the iPhone X build: arm64), and its
# AIR Metal layer (Jesvi-iOS a13e406's, on Mesa Gallium: userland/metal-amd/NOTICE). docs/a56-gpu.md section 4.
#
#   userland/metal-amd/air/build.sh MESA_SRC MESA_BUILD    the guest Mesa stage's call, after its Mesa step: copies the
#        AIR sources to MESA_SRC/src/compiler/air, applies patches/mesa/5*.patch once (on the guest Mesa stage's 0001-4999), sets -Doneios-air=true and
#        -Doneios-air-screen (radeonsi when MESA_SRC defines oneios_radeonsi_screen_create, else none), builds
#        MESA_BUILD/src/gallium/targets/air-metal/libOneiOSRadeonAir.dylib (.so for a Linux build)
#   userland/metal-amd/air/build.sh host                   the host test's Mesa: external/mesa-droidvm at its pin +
#        patches/mesa/*.patch (0001-0006 then 5xxx), softpipe only, in out/metal-amd/host-build (air_metal_test, air2nir; host-test.sh runs them)
#   userland/metal-amd/air/build.sh mesa-ios               the same Mesa cross-built for arm64-apple-ios16.5, softpipe only
#        (no GPU screen), in out/metal-amd/mesa-ios: a library to build the bundle with before the guest Mesa stage's
#        radeonsi Mesa exists (its device open then fails with a sentence)
#   userland/metal-amd/air/build.sh plugin [MESA_BUILD]    the plug-in (userland/metal-amd/plugin) for arm64-apple-ios16.5
#        against out/ios16-tbd, and the bundle out/metal-amd/OneiOSRadeonMetal.bundle (Info.plist, OneiOSRadeonMetal,
#        libOneiOSRadeonAir.dylib from MESA_BUILD, default out/metal-amd/mesa-ios; oneios-target "iPhone10,3 20F66"),
#        then its gpuaddon out/metal-amd/OneiOSRadeonMetal.gpuaddon, and out/metal-amd/metal_caps (Jesvi's probe)
#        (with out/metal-amd/gl/OneiOSRadeonGL.bundle beside the plug-in when the OpenGL ES hook made it: docs/a56-gpu.md 4a)
#   userland/metal-amd/air/build.sh addon                  the gpuaddon alone
#
# Settings (environment): METAL_AMD_OUT (default out/metal-amd), IOS_TBD (default /.../out/ios16-tbd of the main
# checkout when this tree has none), IOS_LD (the same for the toolchain's ld64), JOBS.
# Needs: meson, ninja, llvm-ar, llvm-strip, clang; Mesa's own build needs python3 with mako and pyyaml (Mesa's
# Python, not this project's). Signing is `oneios-tool ios sign` (the codesign of userland/metal/shim).
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
AMD=$ROOT/userland/metal-amd
OUT=${METAL_AMD_OUT:-$ROOT/out/metal-amd}
JOBS=${JOBS:-$(getconf _NPROCESSORS_ONLN)}
MESA_PIN=cecc0e9                         # external/mesa-droidvm (docs/references.md): the guest Mesa stage's Mesa
TARGET_ID="iPhone10,3 20F66"             # K3/K4: the image this bundle is for (the root tweaks place it only there)
say() { echo "metal-amd: $*"; }

# The AIR sources and the 5xxx patches into a Mesa tree. The sources are copied, not linked, so the tree stays
# whole when this checkout moves; a copy older than ours is replaced.
into_mesa() { # SRC [all]: the 5xxx patches (the guest Mesa stage applied 0001-4999), or with "all" every patch in name order
  local src=$1 which=5
  [ "${2:-}" = all ] && which=
  [ -f "$src/meson.options" ] || { echo "metal-amd: $src is no Mesa tree" >&2; exit 1; }
  mkdir -p "$src/src/compiler/air"
  cp "$HERE"/*.c "$HERE"/*.h "$HERE/meson.build" "$src/src/compiler/air/"
  for p in $(ls "$AMD"/patches/mesa/$which*.patch | LC_ALL=C sort); do   # 5002 sits on 0001 and 0005
    if git -C "$src" apply --reverse --check "$p" 2>/dev/null; then
      continue
    fi
    git -C "$src" apply "$p"
    say "applied $(basename "$p") to $src"
  done
}

# radeonsi when the tree defines the K3 entry (its definition, not our declaration in oneios_radeonsi_screen.h)
screen_of() {
  if grep -rqsE '^oneios_radeonsi_screen_create *\(|[ *]oneios_radeonsi_screen_create *\([^;]*$' \
       --include='*.c' --include='*.cpp' "$1/src/gallium" "$1/src/amd" 2>/dev/null; then
    echo radeonsi
  else
    echo none
  fi
}

# The pinned Mesa, in a clone of its own (the objects shared with external/mesa-droidvm, which stays untouched)
mesa_tree() {
  local src=$OUT/mesa-src
  [ -d "$ROOT/external/mesa-droidvm/.git" ] || { echo "metal-amd: no external/mesa-droidvm (scripts/setup/get-references.sh mesa-droidvm)" >&2; exit 1; }
  if [ ! -d "$src/.git" ]; then
    git clone -q --shared --no-checkout "$ROOT/external/mesa-droidvm" "$src"
    git -C "$src" checkout -q "$MESA_PIN"
  fi
  [ "$(git -C "$src" rev-parse --short=7 HEAD)" = "$MESA_PIN" ] || { echo "metal-amd: $src is not at $MESA_PIN" >&2; exit 1; }
  into_mesa "$src" all
}

# Mesa's options for a build of the AIR library alone (no OpenGL, no Vulkan, no window system)
MESA_OPTS=(--wrap-mode=nofallback -Dplatforms= -Dvulkan-drivers= -Dglx=disabled -Degl=disabled -Dgbm=disabled
  -Dllvm=disabled -Dopengl=false -Dgles1=disabled -Dgles2=disabled -Dzstd=disabled -Dexpat=disabled -Dvalgrind=disabled
  -Dlibunwind=disabled -Dglvnd=disabled -Dvideo-codecs= -Dtools= -Dgallium-va=disabled
  -Dgallium-mediafoundation=disabled -Dlmsensors=disabled -Dbuild-tests=false -Dshader-cache=disabled -Doneios-air=true)

host() {
  mesa_tree
  local b=$OUT/host-build
  [ -f "$b/build.ninja" ] || meson setup "$b" "$OUT/mesa-src" --buildtype=debugoptimized "${MESA_OPTS[@]}" \
    -Dgallium-drivers=softpipe -Doneios-air-screen=none
  ninja -C "$b" -j "$JOBS" src/gallium/targets/air-metal/air_metal_test src/compiler/air/air2nir
  say "host: $b/src/gallium/targets/air-metal/air_metal_test, $b/src/compiler/air/air2nir"
}

# The iOS 16.5 toolchain (K4): scripts/ios/env.sh with the 20F66 stubs, behind wrappers so that meson and ninja
# (which keep no environment of ours) get the same compiler every time.
ios_env() {
  local main=/home/akshay/Documents/GitHub/OneiOS
  [ -d "$ROOT/out/ios16-tbd" ] && : "${IOS_TBD:=$ROOT/out/ios16-tbd}" || : "${IOS_TBD:=$main/out/ios16-tbd}"
  [ -x "$ROOT/out/ios-toolchain/cctools/bin/ld" ] && : "${IOS_LD:=$ROOT/out/ios-toolchain/cctools/bin/ld}" ||
    : "${IOS_LD:=$main/out/ios-toolchain/cctools/bin/ld}"
  export IOS_TBD IOS_LD IOS_MIN=16.5 IOS_TARGET=arm64-apple-ios16.5
  [ -f "$IOS_TBD/usr/lib/libSystem.B.tbd" ] || { echo "metal-amd: no 20F66 stubs at $IOS_TBD (oneios-tool ios tbds; docs/building.md)" >&2; exit 1; }
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
    "${MESA_OPTS[@]}" -Dgallium-drivers=softpipe -Doneios-air-screen=none
  ninja -C "$b" -j "$JOBS" src/gallium/targets/air-metal/libOneiOSRadeonAir.dylib
  say "mesa-ios: $b/src/gallium/targets/air-metal/libOneiOSRadeonAir.dylib (softpipe only: no GPU screen)"
}

# The guest Mesa stage's call: its tree and its build directory (already set up by its own meson setup)
guest_mesa() {
  local src=$1 b=$2 screen
  [ -f "$b/build.ninja" ] || { echo "metal-amd: $b is no meson build directory" >&2; exit 1; }
  into_mesa "$src"
  screen=$(screen_of "$src")
  meson configure "$b" -Doneios-air=true -Doneios-air-screen="$screen"
  local lib=libOneiOSRadeonAir.dylib
  grep -q "system = 'darwin'" "$b/meson-info/intro-machines.json" 2>/dev/null || grep -q '"system": "darwin"' "$b/meson-info/intro-machines.json" 2>/dev/null || lib=libOneiOSRadeonAir.so
  ninja -C "$b" -j "$JOBS" "src/gallium/targets/air-metal/$lib"
  say "$b/src/gallium/targets/air-metal/$lib (screen: $screen)"
}

plugin() {
  local mesa_build=${1:-$OUT/mesa-ios}
  local lib=$mesa_build/src/gallium/targets/air-metal/libOneiOSRadeonAir.dylib
  [ -f "$lib" ] || { echo "metal-amd: no $lib (build.sh mesa-ios, or the guest Mesa stage's build)" >&2; exit 1; }
  ios_env
  . "$ROOT/scripts/lib/oneios-tool.sh"
  oneios_tool "$ROOT"
  local obj=$OUT/plugin-obj bundle=$OUT/OneiOSRadeonMetal.bundle P=$AMD/plugin
  mkdir -p "$obj"
  # No ARC (Jesvi's driver manages its own references) and -lobjc: most of what it answers is MTLDeviceSPI,
  # whose header no SDK has. Linked against Metal's private _OBJC_CLASS_$__MTLDevice, which the 20F66 stubs export.
  "$BIN/clang" -O2 -g -fno-objc-arc -Wall -Wno-unused-function -Wno-deprecated-declarations \
    -Wno-unguarded-availability-new -bundle \
    -framework Foundation -framework IOKit -framework Metal -framework IOSurface -lobjc \
    -o "$obj/OneiOSRadeonMetal" "$P/redwoodmetal.m"
  rm -rf "$bundle"
  mkdir -p "$bundle"
  cp "$P/metal_plugin.plist" "$bundle/Info.plist"
  cp "$obj/OneiOSRadeonMetal" "$bundle/OneiOSRadeonMetal"
  llvm-strip -x -o "$bundle/libOneiOSRadeonAir.dylib" "$lib"
  echo "$TARGET_ID" > "$bundle/oneios-target"
  for m in OneiOSRadeonMetal libOneiOSRadeonAir.dylib; do
    "$ONEIOS_TOOL" ios sign "$bundle/$m"
  done
  # Jesvi's capability probe (plugin/metal_caps.m): what MTLCreateSystemDefaultDevice() answers in a process, the
  # families, limits and feature sets; a guest program beside the bundle, not in it (no image places it yet)
  "$BIN/clang" -O2 -fno-objc-arc -framework Foundation -framework IOKit -framework Metal -framework MetalFX -lobjc \
    -o "$OUT/metal_caps" "$P/metal_caps.m"
  "$ONEIOS_TOOL" ios sign "$OUT/metal_caps"
  "$ONEIOS_TOOL" ios macho-check --arch arm64 --tbd "$IOS_TBD" --local "$bundle/libOneiOSRadeonAir.dylib" \
    "$bundle/OneiOSRadeonMetal" "$bundle/libOneiOSRadeonAir.dylib" | tee "$OUT/OneiOSRadeonMetal.check.txt"
  say "bundle $bundle (libOneiOSRadeonAir.dylib from $mesa_build)"
  addon
}

addon() {
  local bundle=$OUT/OneiOSRadeonMetal.bundle gl=$OUT/gl/OneiOSRadeonGL.bundle
  [ -f "$bundle/OneiOSRadeonMetal" ] || { echo "metal-amd: no $bundle: build.sh plugin first" >&2; exit 1; }
  . "$ROOT/scripts/lib/oneios-tool.sh"
  oneios_tool "$ROOT"
  if [ -f "$gl/OneiOSRadeonGL" ]; then
    # the A56's OpenGL ES beside it (userland/metal-amd/gl, the guest Mesa stage's hook): xnuhost then turns GL on
    "$ONEIOS_TOOL" ios gpuaddon "$bundle" "$OUT/OneiOSRadeonMetal.gpuaddon" --gl "$gl"
  else
    "$ONEIOS_TOOL" ios gpuaddon "$bundle" "$OUT/OneiOSRadeonMetal.gpuaddon"
  fi
}

case "${1:-}" in
  host) host ;;
  mesa-ios) mesa_ios ;;
  plugin) plugin "${2:-}" ;;
  addon) addon ;;
  ''|-h|--help) sed -n '2,24p' "$0"; exit 2 ;;
  *) [ $# -eq 2 ] || { sed -n '2,24p' "$0"; exit 2; }; guest_mesa "$1" "$2" ;;
esac
