#!/bin/bash
# Check an image set against its manifest, as the OneOS fork does before it stages one.
#
#   scripts/image/verify.sh out/image/24A446                       a set folder: names, sizes, SHA-256, hash lists
#   scripts/image/verify.sh out/image/24A446 --quick               names and sizes only
#   scripts/image/verify.sh out/image/OneiOS_..._.tar.zst          the archive: its .sha256, the tar rules, every file
#
# Exit 0 and a line starting "ok:" when everything matches; otherwise one line that names the first difference.
#
# The checks are `oneios-tool image verify` (tools/oneios-tool, Rust; scripts/image/imagetool.py did them until
# 2026-11-13 and is still in the tree, unused): a folder's files are hashed in parallel; an archive is read through
# the `zstd` program. The binary is built when it is missing or older than its sources; ONEIOS_TOOL=PATH names another.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
die() { echo "verify: $*" >&2; exit 1; }
[ $# -ge 1 ] || die "usage: scripts/image/verify.sh SET_FOLDER|ARCHIVE.tar.zst [--quick]"
[ -e "$1" ] || die "no such set: $1"
[ -d "$1" ] || command -v zstd >/dev/null || die "missing tools: zstd"
. "$ROOT/scripts/lib/oneios-tool.sh"
oneios_tool "$ROOT" || die "the image tool is oneios-tool (Rust): cargo build --release --manifest-path tools/oneios-tool/Cargo.toml"
exec "$ONEIOS_TOOL" image verify "$@"
