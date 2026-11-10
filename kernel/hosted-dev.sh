#!/bin/bash
# Work on the hosted machine layer in place, without a full rebuild per change.
#   kernel/hosted-dev.sh base     once after a full kernel/build-hosted.sh: record the tree without the hosted patch
#   kernel/hosted-dev.sh build    incremental build of the work tree as edited -> out/kernel/current-hosted/xnu.macho
#   kernel/hosted-dev.sh patch    write kernel/patches/oneios-hosted/0001-hosted-machine-layer.patch from the work tree
#                                 (and tell the build that the tree already has it, so the next build stays incremental)
# The work tree is out/kernel-hosted/unixnu/oneios/<profile>/oneios_hosted/tree/.cache/xnu-work. A full
# kernel/build-hosted.sh from the committed patch is the check that the patch alone reproduces the tree.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# HOSTED_OUT=NAME selects the work tree of `HOSTED_OUT=NAME kernel/build-hosted.sh`; HOSTED_PATCH=FILE (a name in
# kernel/patches/oneios-hosted, default 0001-hosted-machine-layer.patch) is the patch that `base` leaves out and
# `patch` writes. A new patch: build with it absent, `base`, edit, `patch`, add its name to `series`.
[ "${HOSTED_OUT:-}" != lead ] || HOSTED_OUT=
WORK=$ROOT/out/kernel-hosted${HOSTED_OUT:+-$HOSTED_OUT}/unixnu/oneios/ios-27.0.1-24A446/oneios_hosted
TREE=$WORK/tree/.cache/xnu-work
PATCH=$ROOT/kernel/patches/oneios-hosted/${HOSTED_PATCH:-0001-hosted-machine-layer.patch}
[ -d "$TREE/.git" ] || { echo "hosted-dev: no work tree; run kernel/build-hosted.sh first" >&2; exit 1; }
tree_now() { ( cd "$TREE" && export GIT_INDEX_FILE=$TREE/.git/hosted-index && git read-tree HEAD && git add -A . >/dev/null 2>&1 && git write-tree ); }
case "${1:-}" in
base)
  now=$(tree_now)
  ( cd "$TREE" && export GIT_INDEX_FILE=$TREE/.git/hosted-index && git read-tree "$now" && { [ ! -e "$PATCH" ] || git apply --cached -R "$PATCH"; } &&
    git update-ref refs/hosted/base "$(git commit-tree "$(git write-tree)" -m base </dev/null)" )
  echo "hosted-dev: base recorded" ;;
patch)
  git -C "$TREE" rev-parse -q --verify refs/hosted/base >/dev/null || { echo "hosted-dev: no base; run 'hosted-dev.sh base' after a full build" >&2; exit 1; }
  now=$(tree_now)
  git -C "$TREE" diff-tree -p --no-renames refs/hosted/base "$now" > "$PATCH.new"
  mv "$PATCH.new" "$PATCH"
  # The build compares each patch's hash with its record and starts the tree over when one differs.
  python3 -I - "$WORK/xnu-inputs.json" "$PATCH" <<'PY'
import hashlib, json, sys
stamp, patch = sys.argv[1:]
record = json.load(open(stamp))
for entry in record['patches']:
    if entry['path'] == patch:
        entry['sha256'] = hashlib.sha256(open(patch, 'rb').read()).hexdigest()
open(stamp, 'w').write(json.dumps(record, indent=2) + '\n')
PY
  echo "hosted-dev: $PATCH ($(grep -c '^diff --git' "$PATCH") files, $(wc -l < "$PATCH") lines)" ;;
build)
  HOSTED_OUT=${HOSTED_OUT:-lead} exec "$ROOT/kernel/build-hosted.sh" ;;
*) sed -n 2,8p "$0"; exit 2 ;;
esac
