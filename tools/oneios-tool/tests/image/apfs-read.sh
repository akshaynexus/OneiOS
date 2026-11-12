#!/bin/bash
# tools/oneios-tool/tests/image/apfs-read.sh IMAGE [VOLUME [PATH]]
#
# `oneios-tool image apfs-copy` against apfs-fuse (github.com/sgan81/apfs-fuse, an independent reader) on one
# volume of a raw APFS image: both copy PATH (default: the whole volume); every file that apfs-copy wrote must be
# byte for byte apfs-fuse's, and the two sets of names must be the same except for the files apfs-copy skips
# (files kept compressed, which it counts). Linux only (apfs-fuse); nothing needs root.
#
# VOLUME: the volume's number (default 0). Env: ONEIOS_TOOL (the binary), WORK (default out/rustport-apfs),
# NEWEST=1 to read the newest checkpoint (--newest).
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
tool=${ONEIOS_TOOL:-$root/tools/oneios-tool/target/release/oneios-tool}
image=${1:?usage: apfs-read.sh IMAGE [VOLUME [PATH]]}
volume=${2:-0}
path=${3:-}
work=${WORK:-$root/out/rustport-apfs}
command -v apfs-fuse >/dev/null || { echo "apfs-read: no apfs-fuse on PATH" >&2; exit 2; }
[ -x "$tool" ] || { echo "apfs-read: no tool at $tool" >&2; exit 2; }
rm -rf "$work/ours" "$work/list.ours" "$work/list.fuse"
mkdir -p "$work/mnt"
apfs-fuse -o ro,vol="$volume" "$image" "$work/mnt" || { echo "apfs-read: apfs-fuse cannot mount $image" >&2; exit 2; }
trap 'fusermount3 -u "$work/mnt" 2>/dev/null || fusermount -u "$work/mnt"' EXIT
# apfs-fuse shows the volume below root/ (and private-dir/ beside it)
src=$work/mnt/root/$path
started=$SECONDS
"$tool" image apfs-copy "$image" "/$path" "$work/ours" --volume "$volume" ${NEWEST:+--newest} > "$work/copy.out" 2> "$work/copy.err"
status=$?
cat "$work/copy.out" "$work/copy.err"
[ $status = 0 ] || { echo "FAIL: apfs-copy ended with status $status"; exit 1; }
echo "apfs-copy: $((SECONDS - started)) s"
fail=0
if [ -d "$work/ours" ]; then
  (cd "$work/ours" && find . \( -type f -o -type l \) | LC_ALL=C sort) > "$work/list.ours"
  (cd "$src" && find . \( -type f -o -type l \) 2>/dev/null | LC_ALL=C sort) > "$work/list.fuse"
  only_ours=$(LC_ALL=C comm -23 "$work/list.ours" "$work/list.fuse" | wc -l)
  only_fuse=$(LC_ALL=C comm -13 "$work/list.ours" "$work/list.fuse" | wc -l)
  skipped=$(sed -n 's/.*skipped \([0-9]*\) compressed.*/\1/p' "$work/copy.out"); skipped=${skipped:-0}
  differ=0; same=0
  while IFS= read -r f; do
    if [ -L "$work/ours/$f" ]; then
      [ "$(readlink "$work/ours/$f")" = "$(readlink "$src/$f")" ] && same=$((same + 1)) || { differ=$((differ + 1)); [ $differ -gt 5 ] || echo "differs: $f (link)"; }
    elif cmp -s "$work/ours/$f" "$src/$f"; then same=$((same + 1)); else differ=$((differ + 1)); [ $differ -gt 5 ] || echo "differs: $f"; fi
  done < "$work/list.ours"
  echo "files and links: $same the same, $differ differ; only in ours $only_ours; only in apfs-fuse $only_fuse (apfs-copy skipped $skipped compressed)"
  [ "$differ" = 0 ] && [ "$only_ours" = 0 ] && [ "$only_fuse" = "$skipped" ] || fail=1
else
  cmp "$work/ours" "$src" && echo "the file is the same" || fail=1
fi
[ $fail = 0 ] && echo "apfs-read: ok" || echo "apfs-read: FAIL"
exit $fail
