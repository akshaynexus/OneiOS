#!/bin/bash
# The Metal bridge on metal2vulkan-rt (userland/metal/rt/README.md): the source at its pin with this folder's patches,
# and the bridge built over it.
#
#   userland/metal/rt/build.sh source      the pin checked out in $M2V_RT_WORK/src (a worktree of external/metal2vulkan,
#                                          the commit fetched when it is missing), patches/*.patch applied in name order
#   userland/metal/rt/build.sh check       m2v-rt-check and abi_smoke for this machine (Linux or macOS), in $M2V_RT_WORK/target/release
#   userland/metal/rt/build.sh test        rt's unit tests on the patched source (cargo test -p metal2vulkan-runtime --lib)
#   userland/metal/rt/build.sh ios OUT.a   the arm64e-apple-ios static library of the bridge (fat LTO), for userland/metal/build.sh
#                                          with METAL_RUNTIME=rt
#
# M2V_RT_WORK defaults to out/m2v-rt (never out/metal, the default add-on's). Needs: the toolchain of the pin's
# rust-toolchain.toml (nightly-2026-11-03, rustup installs it on first use), llvm-as (LLVM 21 or newer).
# m2v-rt-check DRIVER [--floor-only | --census FILE... | --pair LIB VERTEX FRAGMENT [FORMAT...] [--raw] | --selftest [--qc FILE]]: see bridge/src/bin/m2v-rt-check.rs;
# abi_smoke DRIVER: the C ABI through NativeMetal's headers (bridge/tests/abi_smoke.c). On the laptop
#   m2v-rt-check /usr/lib/libvulkan_intel.so                                   the direct path, one dispatch
#   FD_CHIP_ID=0x43051401 LD_PRELOAD=<mesa>/libfreedreno_noop_drm_shim.so \
#     m2v-rt-check <mesa>/libvulkan_freedreno.so --census metallibs/*.metallib  Turnip as an Adreno 750
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
HERE=$ROOT/userland/metal/rt
PIN=6462199fad8d75b81556a7fa322317da116701fa
W=${M2V_RT_WORK:-$ROOT/out/m2v-rt}
S=$W/src
EXT=$ROOT/external/metal2vulkan
say() { echo "metal-rt: $*"; }

source_tree() {
  [ -d "$EXT/.git" ] || [ -f "$EXT/.git" ] || { echo "metal-rt: no external/metal2vulkan (scripts/setup/get-references.sh metal2vulkan)" >&2; exit 1; }
  git -C "$EXT" cat-file -e "$PIN^{commit}" 2>/dev/null || git -C "$EXT" fetch -q origin development
  if [ ! -e "$S/.git" ]; then
    mkdir -p "$W"
    git -C "$EXT" worktree add -q --detach "$S" "$PIN"
  fi
  [ "$(git -C "$S" rev-parse HEAD)" = "$PIN" ] || { echo "metal-rt: $S is not at $PIN" >&2; exit 1; }
  # Idempotent: the tree is reset to the pin, then every patch applied.
  git -C "$S" checkout -q -- .
  for p in "$HERE"/patches/*.patch; do
    git -C "$S" apply "$p"
    say "applied $(basename "$p")"
  done
}

manifest() {
  local B=$W/bridge
  mkdir -p "$B"
  llvm-as -o "$B/runtime_examples.air" "$S/crates/metal2vulkan-validation/fixtures/public/examples/runtime_examples.ll"
  llvm-as -o "$B/function_constant.air" "$S/crates/metal2vulkan-validation/fixtures/public/kernel_function_constant_gate_fold.ll"
  llvm-as -o "$B/scanout_convert.air" "$ROOT/external/NativeMetal/shaders/scanout_convert.ll"
  cp "$S/rust-toolchain.toml" "$B/"
  cat > "$B/Cargo.toml" <<EOF
[package]
name = "adreno-metal-bridge-rt"
version = "0.2.0"
edition = "2021"
[workspace]
[lib]
name = "adreno_metal_bridge_rt"
path = "$HERE/bridge/src/lib.rs"
crate-type = ["rlib", "staticlib"]
[[bin]]
name = "m2v-rt-check"
path = "$HERE/bridge/src/bin/m2v-rt-check.rs"
[dependencies]
metal2vulkan-runtime = { path = "$S/crates/metal2vulkan-runtime" }
metal2vulkan = { path = "$S/crates/metal2vulkan", features = ["ash"] }
ash = { version = "0.38", default-features = false, features = ["std"] }
libc = "0.2"
# The render server's display thread spends most of a frame in the runtime, much of it in small functions across the
# translator's crates: one unit with link-time optimisation lets them inline across crates (as the old bridge was built).
[profile.release]
panic = "unwind"
debug = 1
lto = "fat"
codegen-units = 1
EOF
}

bridge_env() {
  export NMB_FIXTURE_AIR=$W/bridge/runtime_examples.air NMB_FC_AIR=$W/bridge/function_constant.air NMB_SCANOUT_AIR=$W/bridge/scanout_convert.air
  # The translator source the runtime is built from (its pin and our patches), which keys the frontend's shader priming.
  NMB_RUNTIME_ID=$( (echo "$PIN"; cat "$HERE"/patches/*.patch) | sha256sum | cut -c1-32)
  export NMB_RUNTIME_ID
}

check() {
  [ -e "$S/.git" ] || source_tree
  manifest
  bridge_env
  (cd "$W/bridge" && CARGO_TARGET_DIR=$W/target cargo build --release)
  # The C ABI as the frontend calls it, against NativeMetal's own headers (bridge/tests/abi_smoke.c).
  cc -O1 -Wall -I "$ROOT/external/NativeMetal/bridge" -o "$W/target/release/abi_smoke" "$HERE/bridge/tests/abi_smoke.c" \
    "$W/target/release/libadreno_metal_bridge_rt.a" -lpthread -ldl -lm
  say "$W/target/release/m2v-rt-check, $W/target/release/abi_smoke"
}

# The arm64e-apple-ios static library, for the plug-in (userland/metal/build.sh with METAL_RUNTIME=rt), copied to OUT.a. The
# environment is userland/metal/build.sh's (scripts/ios/env.sh: the guest's SDK and the cross clang).
ios() {
  local out=$1
  [ -e "$S/.git" ] || source_tree
  manifest
  bridge_env
  . "$ROOT/scripts/ios/env.sh"
  [ "$IOS_HOST" = Darwin ] || export SDKROOT=$IOS_SDK
  export CC_arm64e_apple_ios=$IOS_DIR/ios-clang CXX_arm64e_apple_ios=$IOS_DIR/ios-clang++
  export AR_arm64e_apple_ios=${AR:-llvm-ar} CARGO_TARGET_ARM64E_APPLE_IOS_LINKER=$IOS_DIR/ios-clang
  export IPHONEOS_DEPLOYMENT_TARGET=27.0 RUSTC_BOOTSTRAP=1
  (cd "$W/bridge" && CARGO_TARGET_DIR=$W/target cargo build --release --lib -Z build-std=std,panic_unwind --target arm64e-apple-ios)
  mkdir -p "$(dirname "$out")"
  cp "$W/target/arm64e-apple-ios/release/libadreno_metal_bridge_rt.a" "$out"
  say "$out ($(wc -c < "$out") bytes; runtime id $NMB_RUNTIME_ID)"
}

unit_tests() {
  [ -e "$S/.git" ] || source_tree
  (cd "$S" && CARGO_TARGET_DIR=$W/target cargo test -p metal2vulkan-runtime --lib)
}

case "${1:-}" in
  source) source_tree ;;
  check) check ;;
  ios) ios "${2:?ios OUT.a}" ;;
  test) unit_tests ;;
  *) sed -n "2,19p" "$0"; exit 2 ;;
esac
