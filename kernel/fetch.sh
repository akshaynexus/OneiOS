#!/bin/bash
# Fetch the kernel's sources into external/ at the commits of pins.sh (shallow: one commit each, about 40 MB in all).
#   kernel/fetch.sh            every row of pins.sh
#   kernel/fetch.sh -l         the pins and the state on disk
#   kernel/fetch.sh NAME...    only these
# UniXNU's own sources (the XNU tag, Apple's open-source references, its driver and kit submodules) are fetched by
# its driver during the build (`unixnu fetch`), pinned by its feeds.toml and the profile.
# Nothing here is Apple firmware, and external/ is git-ignored.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
. "$ROOT/kernel/pins.sh"
DEST=${EXTERNAL_DIR:-$ROOT/external}
if [ "${1:-}" = -l ]; then
  printf '%-20s %-8s %s\n' NAME STATE COMMIT
  pin_rows "$PINS_SOURCES" | while read -r name _ commit _; do
    if [ -e "$DEST/$name" ]; then
      head=$(git -C "$DEST/$name" rev-parse HEAD 2>/dev/null || echo '?')
      if [ "$head" = "$commit" ]; then state=pinned; else state=differs; fi
    else state=absent; fi
    printf '%-20s %-8s %s\n' "$name" "$state" "$commit"
  done
  exit 0
fi
mkdir -p "$DEST"
fail=0
while read -r name url commit branch; do
  if [ $# -gt 0 ]; then
    case " $* " in *" $name "*) ;; *) continue ;; esac
  fi
  pin_fetch "$DEST" "$name" "$url" "$commit" "$branch" </dev/null || fail=1
done <<ROWS
$(pin_rows "$PINS_SOURCES")
ROWS
exit $fail
