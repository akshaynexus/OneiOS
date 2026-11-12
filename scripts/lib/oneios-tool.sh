# Sourced by the build scripts: oneios_tool ROOT finds the Rust tools' binary (tools/oneios-tool, docs/building.md
# section 10) and exports its path as ONEIOS_TOOL.
#   ONEIOS_TOOL=PATH set by the caller: that binary, as it is.
#   Otherwise tools/oneios-tool/target/release/oneios-tool, built first with `cargo build --release` when it is
#   missing or older than a file of its sources (cargo from PATH or ~/.cargo/bin; Rust 1.88 or newer).
# shellcheck shell=bash
oneios_tool() {
  local dir=$1/tools/oneios-tool bin cargo
  if [ -n "${ONEIOS_TOOL:-}" ]; then
    [ -x "$ONEIOS_TOOL" ] || { echo "ONEIOS_TOOL=$ONEIOS_TOOL is not a program" >&2; return 1; }
    export ONEIOS_TOOL; return 0
  fi
  bin=$dir/target/release/oneios-tool
  if [ ! -x "$bin" ] || [ -n "$(find "$dir/Cargo.toml" "$dir/Cargo.lock" "$dir/crates" -type f -newer "$bin" -print 2>/dev/null | head -1)" ]; then
    cargo=$(command -v cargo || echo "$HOME/.cargo/bin/cargo")
    [ -x "$cargo" ] || { echo "oneios-tool is not built and there is no cargo: install Rust 1.88 or newer (https://rustup.rs; docs/building.md section 0)" >&2; return 1; }
    echo "oneios-tool: building (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)" >&2
    "$cargo" build --release --manifest-path "$dir/Cargo.toml" >&2 || { echo "oneios-tool: the build failed" >&2; return 1; }
    touch "$bin"
  fi
  ONEIOS_TOOL=$bin; export ONEIOS_TOOL
}
