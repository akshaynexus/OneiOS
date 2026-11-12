#!/bin/bash
# Pack the built kernel for a release, and (with --upload) publish it as the release kernel-<build id> of this
# repository, from where the release workflow takes it (docs/releases.md, "The kernel in a release").
#   kernel/publish.sh             pack out/kernel/current -> out/kernel/publish/xnu-<build id>.tar.zst, print its SHA-256
#   kernel/publish.sh --upload    the same, then create the GitHub release and upload the archive (asks nothing: only
#                                 run it when the build is the one to release)
# The archive holds xnu.macho, xnu.macho.build.json and series.txt, and nothing else. Before packing, the same
# check as the runtime package runs (runtime/package/pack.py): a kernel that is prelinked, has kernelcache kexts,
# links a non-redistributable source, or has no record of this builder is refused.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SRC=${KERNEL_DIR:-$ROOT/out/kernel/current}
[ -f "$SRC/xnu.macho" ] && [ -f "$SRC/xnu.macho.build.json" ] || { echo "publish: no kernel at $SRC (kernel/build-linux.sh)" >&2; exit 1; }
ID=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["build_id"])' "$SRC/xnu.macho.build.json")
OUT=$ROOT/out/kernel/publish
STAGE=$OUT/.stage
rm -rf "$STAGE"; mkdir -p "$STAGE"
trap 'rm -rf "$STAGE"' EXIT
cp "$SRC/xnu.macho" "$SRC/xnu.macho.build.json" "$STAGE/"
[ -f "$SRC/series.txt" ] && cp "$SRC/series.txt" "$STAGE/"
chmod 0644 "$STAGE"/*
python3 -I "$ROOT/runtime/package/pack.py" check "$STAGE"
EPOCH=$(git -C "$ROOT" log -1 --format=%ct)
ARCHIVE=$OUT/xnu-$ID.tar.zst
( cd "$STAGE" && ls | LC_ALL=C sort > "$OUT/.members" )
tar -C "$STAGE" --format=ustar --owner=0 --group=0 --numeric-owner --mtime="@$EPOCH" -T "$OUT/.members" -cf - | zstd -q -19 -f -o "$ARCHIVE"
rm -f "$OUT/.members"
SHA=$( { sha256sum "$ARCHIVE" 2>/dev/null || shasum -a 256 "$ARCHIVE"; } | cut -d" " -f1)
echo "publish: $ARCHIVE"
echo "publish: line for kernel/release-kernel.txt:"
echo "kernel-$ID $SHA"
if [ "${1:-}" = --upload ]; then
  gh release create "kernel-$ID" --repo akshaynexus/OneiOS --prerelease --title "XNU $ID" \
    --notes "Source-built XNU for the OneiOS runtime package (APSL 2.0; kernel/NOTICE.kernel). Not Apple's kernelcache: no Apple kernel extension inside. Build record in the archive." \
    "$ARCHIVE"
fi
