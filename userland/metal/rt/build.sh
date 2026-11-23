#!/bin/bash
# The Metal bridge on metal2vulkan-rt (userland/metal/rt/README.md): the source at its pin with this folder's patches,
# and the bridge built over it.
#
#   userland/metal/rt/build.sh source      the pin checked out in $M2V_RT_WORK/src (a worktree of external/metal2vulkan,
#                                          the commit fetched when it is missing), patches/*.patch applied in name order
#   userland/metal/rt/build.sh check       m2v-rt-check for this machine (Linux or macOS), in $M2V_RT_WORK/target/release
#   userland/metal/rt/build.sh test        rt's unit tests on the patched source (cargo test -p metal2vulkan-runtime --lib)
#
# M2V_RT_WORK defaults to out/m2v-rt (never out/metal, the default add-on's). Needs: the toolchain of the pin's
# rust-toolchain.toml (nightly-2026-11-03, rustup installs it on first use), llvm-as (LLVM 21 or newer).
# m2v-rt-check DRIVER [--floor-only | --census FILE...]: see bridge/src/bin/m2v-rt-check.rs; on the laptop
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
EOF
}

check() {
  [ -e "$S/.git" ] || source_tree
  manifest
  (cd "$W/bridge" && NMB_FIXTURE_AIR=$W/bridge/runtime_examples.air CARGO_TARGET_DIR=$W/target cargo build --release --bin m2v-rt-check)
  say "$W/target/release/m2v-rt-check"
}

unit_tests() {
  [ -e "$S/.git" ] || source_tree
  (cd "$S" && CARGO_TARGET_DIR=$W/target cargo test -p metal2vulkan-runtime --lib)
}

case "${1:-}" in
  source) source_tree ;;
  check) check ;;
  test) unit_tests ;;
  *) sed -n '2,16p' "$0"; exit 2 ;;
esac
