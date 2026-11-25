#!/bin/bash
# Build the runtime's phone binaries from the Rust workspace (runtime/Cargo.toml, runtime/RUST.md) and leave them in
# STAGE (runtime/package.sh calls this).
#   runtime/build.sh STAGE        cargo build --release --target aarch64-linux-android, then stage, stripped:
#                                   static (crt-static): bin/xnuhost (the task stub is inside it), bin/gpuctl, and the
#                                     phone checks bin/bridgectl, bin/wifictl, bin/brightctl, bin/batteryctl,
#                                     bin/thermalctl, bin/sensorctl, bin/displayctl, and the display helper's launcher bin/oneios-display
#                                   dynamic (they need Android's libraries): bin/oneios-audio (libaaudio), bin/oneios-camera
#                                     and bin/camctl (camera2ndk, mediandk, binder_ndk), bin/oneios-encoder and bin/encctl
#                                     (mediandk), bin/liboneios-display.so (the JNI library, -z defs)
#                                   and bin/oneios-net.sh, lib/oneios-display.dex (javac and d8, runtime/display/build-android.sh)
#                                 Then runtime/package/elf-check.sh STAGE: every dynamic ELF file of the stage must need
#                                 only API 30 stubs of the NDK and find each undefined symbol in one of them, or the build
#                                 fails (exit 1)
#   runtime/build.sh --test       the host suite: runtime/host-tests.sh (cargo test --workspace, the qemu boots of the
#                                 test kernel, the link checks; on a Mac in an arm64 Linux container)
# Build hosts: Linux x86_64 and macOS (the NDK's darwin-x86_64 tools). Needs rustup's aarch64-linux-android and
# aarch64-unknown-none targets (docs/building.md section 0).
# NDK: $NDK, else $ANDROID_NDK_ROOT, else $ANDROID_NDK_HOME, else $HOME/Android/Sdk/ndk/28.2.13676358 (Linux) or
#      $HOME/Library/Android/sdk/ndk/28.2.13676358 (macOS). runtime/ndk-cc is cargo's linker; it finds the NDK the same way.
# A stage needs external/virglrenderer (scripts/setup/get-references.sh virglrenderer: the GPU's renderer, drm2kgsl, built
# by runtime/xnuhost/drm2kgsl/build.rs; VIRGL=DIR another checkout) and is refused without it (exit 1); XNUHOST_2D_ONLY=1
# builds the 2D-only xnuhost on purpose.
# Target dirs: runtime/target/android (dynamic) and runtime/target/android-static, so the two builds never rebuild each other.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
case "$(uname -s)" in
  Darwin) NDK_HOST=darwin-x86_64; NDK_DEFAULT=$HOME/Library/Android/sdk/ndk/28.2.13676358 ;;
  *)      NDK_HOST=linux-x86_64;  NDK_DEFAULT=$HOME/Android/Sdk/ndk/28.2.13676358 ;;
esac
export NDK=${NDK:-${ANDROID_NDK_ROOT:-${ANDROID_NDK_HOME:-$NDK_DEFAULT}}}
TOOLS=$NDK/toolchains/llvm/prebuilt/$NDK_HOST/bin
[ -x "$TOOLS/aarch64-linux-android30-clang" ] || { echo "build: no NDK at $NDK ($NDK_HOST)" >&2; exit 1; }
STRIP=$TOOLS/llvm-strip
if [ "${1:-}" = --test ]; then
  exec "$ROOT/runtime/host-tests.sh"
fi
STAGE=${1:?usage: runtime/build.sh STAGE | --test}
[ -d "$HOME/.cargo/bin" ] && PATH=$HOME/.cargo/bin:$PATH
# The phone's xnuhost needs the GPU's renderer (drm2kgsl, from external/virglrenderer). Without that checkout
# drm2kgsl-sys builds without it and xnuhost says so only when it starts on the phone ("gpu: 2D only: this build has no
# drm2kgsl"). Ported from YumikoHayamato/OneiOS (s25-ultra-sm8750, b7efdea): there external/ was a link into a tmpfs that
# a reboot had emptied, a runtime came out 2D-only without a word, and the S25 Ultra's session 3 drew iOS on the CPU
# (host CPU up to 686 %, the case at 44 C in a minute). So a stage is refused without it; XNUHOST_2D_ONLY=1 builds one
# on purpose (a PC run, a test).
VIRGL_DIR=${VIRGL:-$ROOT/external/virglrenderer}
if [ ! -f "$VIRGL_DIR/src/drm/drm2kgsl/drm2kgsl_renderer.c" ] && [ "${XNUHOST_2D_ONLY:-0}" != 1 ]; then
  echo "build: no GPU renderer: $VIRGL_DIR/src/drm/drm2kgsl is missing$([ -L "$ROOT/external" ] && [ ! -e "$ROOT/external" ] && echo " (external is a link to $(readlink "$ROOT/external"), which does not exist)")" >&2
  echo "build: fetch it with scripts/setup/get-references.sh virglrenderer, or set XNUHOST_2D_ONLY=1 for a 2D-only xnuhost" >&2
  exit 1
fi

mkdir -p "$STAGE/bin" "$STAGE/lib"
STAGE=$(cd "$STAGE" && pwd)
cd "$ROOT/runtime"
A=aarch64-linux-android
STATIC_DIR=$ROOT/runtime/target/android-static
DYNAMIC_DIR=$ROOT/runtime/target/android
cargo_static() {
  CARGO_TARGET_DIR=$STATIC_DIR cargo build --release --locked --target $A \
    --config "target.$A.rustflags=[\"-C\",\"target-feature=+crt-static\"]" "$@"
}
cargo_dynamic() { CARGO_TARGET_DIR=$DYNAMIC_DIR cargo build --release --locked --target $A "$@"; }
stage() { # DIR NAME...: strip each built NAME into STAGE/bin
  local dir=$1; shift
  for name in "$@"; do "$STRIP" -o "$STAGE/bin/$name" "$dir/$A/release/$name"; chmod 0755 "$STAGE/bin/$name"; done
}

# Static: xnuhost and gpuctl (runtime/xnuhost; docs/booter.md, docs/gpu.md), and the phone checks, which the OneOS fork's
# Hardware checks screen runs with One UI up (as root): bridgectl (cpu, haptic, haptic-pattern, bt, audio), wifictl (status
# and scan are read only; join takes wlan0 as a session does; restore gives wlan0's settings back after a killed xnuhost),
# brightctl (info, levels, als), batteryctl, thermalctl (`watch` logs the temperatures), sensorctl (`qrtr` is read only:
# whether the sensor hub is reachable over QRTR; `stream` streams it for N s). Each folder's README has the
# commands and what a good run prints.
cargo_static -p xnuhost --bins
cargo_static -p onebridges --bin bridgectl --bin wifictl --bin brightctl --bin batteryctl --bin thermalctl --bin sensorctl
stage "$STATIC_DIR" xnuhost gpuctl bridgectl wifictl brightctl batteryctl thermalctl sensorctl
# Dynamic: the helpers that call Android's own libraries (AAudio, Camera2, MediaCodec) and their phone checks
# (runtime/bridges/camera/README.md, runtime/bridges/encoder/README.md).
cargo_dynamic -p onebridges --bin oneios-audio
cargo_dynamic -p onecamera --bins
cargo_dynamic -p oneencoder --bins
stage "$DYNAMIC_DIR" oneios-audio oneios-camera camctl oneios-encoder encctl
cp "$ROOT/runtime/bridges/net/oneios-net.sh" "$STAGE/bin/oneios-net.sh"
chmod 0755 "$STAGE/bin/oneios-net.sh"
# The display helper: a launcher, the library app_process loads, the dex with the layer (needs javac and d8), and
# displayctl (runtime/display/README.md).
# A folder of this build's own: two builds at once (the release's runtime lane and runtime/package/test.sh in its
# tests lane, 2026-11-23) shared runtime/target/display-stage, and one's rm -rf took the other's files.
mkdir -p "$ROOT/runtime/target"
D=$(mktemp -d "$ROOT/runtime/target/display-stage.XXXXXX")
CARGO_TARGET_DIR=$DYNAMIC_DIR "$ROOT/runtime/display/build-android.sh" "$D"
for f in oneios-display displayctl liboneios-display.so; do "$STRIP" -o "$STAGE/bin/$f" "$D/bin/$f"; chmod 0755 "$STAGE/bin/$f"; done
cp "$D/lib/oneios-display.dex" "$STAGE/lib/oneios-display.dex"
chmod 0644 "$STAGE/lib/oneios-display.dex"
rm -rf -- "${D:?}"
# Can Android load what was built? A shared object links with undefined symbols unless its link has -z defs, and the
# phone then refuses it (liboneios-display.so, 2026-11-14): a stage with such a file fails here, on the build machine
# (docs/building.md section 4).
"$ROOT/runtime/package/elf-check.sh" "$STAGE"
echo "build: $STAGE/bin/xnuhost ($(wc -c < "$STAGE/bin/xnuhost" | tr -d ' ') bytes)"
