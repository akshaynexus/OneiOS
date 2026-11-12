#!/bin/bash
# Check an image set against its manifest, as the OneOS fork does before it stages one.
#
#   scripts/image/verify.sh out/image/24A446                       a set folder: names, sizes, SHA-256, hash lists
#   scripts/image/verify.sh out/image/24A446 --quick               names and sizes only
#   scripts/image/verify.sh out/image/OneiOS_..._.tar.zst          the archive: its .sha256, the tar rules, every file
#
# Exit 0 and a line starting "ok:" when everything matches; otherwise one line that names the first difference.
#
# Env: IMAGETOOL_IMPL=python|rust (default python). rust: a set folder is checked by tools/oneios-tool's
#      `oneios-tool image verify` (the same checks and lines, the files hashed in parallel; build it with
#      cargo build --release --manifest-path tools/oneios-tool/Cargo.toml). An archive is always checked by the
#      Python tool. ONEIOS_TOOL: the Rust binary (default tools/oneios-tool/target/release/oneios-tool).
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
die() { echo "verify: $*" >&2; exit 1; }
[ $# -ge 1 ] || die "usage: scripts/image/verify.sh SET_FOLDER|ARCHIVE.tar.zst [--quick]"
[ -e "$1" ] || die "no such set: $1"
case "${IMAGETOOL_IMPL:-python}" in
python) ;;
rust)
  RTOOL=${ONEIOS_TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
  [ -x "$RTOOL" ] || die "IMAGETOOL_IMPL=rust: no $RTOOL (build it: cargo build --release --manifest-path tools/oneios-tool/Cargo.toml; or set ONEIOS_TOOL)"
  [ ! -d "$1" ] || exec "$RTOOL" image verify "$@" ;;
*) die "IMAGETOOL_IMPL is python or rust, not $IMAGETOOL_IMPL" ;;
esac
command -v python3 >/dev/null || die "missing tools: python3 (3.11 or newer)"
python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' || die "missing tools: python3 3.11 or newer"
[ -d "$1" ] || command -v zstd >/dev/null || die "missing tools: zstd"
exec python3 -I "$ROOT/scripts/image/imagetool.py" verify "$@"
