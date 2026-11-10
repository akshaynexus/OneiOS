#!/bin/bash
# Check an image set against its manifest, as the OneOS fork does before it stages one.
#
#   scripts/image/verify.sh out/image/24A446                       a set folder: names, sizes, SHA-256, hash lists
#   scripts/image/verify.sh out/image/24A446 --quick               names and sizes only
#   scripts/image/verify.sh out/image/OneiOS_..._.tar.zst          the archive: its .sha256, the tar rules, every file
#
# Exit 0 and a line starting "ok:" when everything matches; otherwise one line that names the first difference.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
die() { echo "verify: $*" >&2; exit 1; }
[ $# -ge 1 ] || die "usage: scripts/image/verify.sh SET_FOLDER|ARCHIVE.tar.zst [--quick]"
[ -e "$1" ] || die "no such set: $1"
command -v python3 >/dev/null || die "missing tools: python3 (3.11 or newer)"
python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' || die "missing tools: python3 3.11 or newer"
[ -d "$1" ] || command -v zstd >/dev/null || die "missing tools: zstd"
exec python3 -I "$ROOT/scripts/image/imagetool.py" verify "$@"
