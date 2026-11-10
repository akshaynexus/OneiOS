#!/bin/bash
# Host tests of kernel/ that need no toolchain and no network: script syntax, the pin tables, the patch series
# files, the profile and the build-record writer. Run by CI and by hand: kernel/tests/host.sh
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"
pass=0
ok() { pass=$((pass + 1)); echo "ok $pass: $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

for f in kernel/*.sh kernel/linux/*.sh kernel/tests/*.sh; do bash -n "$f" || fail "syntax: $f"; done
for f in kernel/*.py kernel/linux/*.py; do [ -e "$f" ] || continue; python3 -I -m py_compile "$f" || fail "syntax: $f"; done
ok "script syntax"

. kernel/pins.sh
for table in "$PINS_SOURCES" "$PINS_TOOLCHAIN"; do
  pin_rows "$table" | while read -r name url commit branch; do
    [[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "pin $name: '$commit' is not a full commit"
    [[ $url == https://github.com/*.git ]] || fail "pin $name: url $url"
    [ -n "$branch" ] || fail "pin $name: no branch"
  done
done
[[ $XNU_COMMIT =~ ^[0-9a-f]{40}$ ]] || fail "XNU_COMMIT"
ok "pins are full commits"

if [ -x kernel/tests/series-check.py ]; then
  python3 -I kernel/tests/series-check.py || fail "series"
  ok "patch series and profile"
fi
if [ -f kernel/build-record.py ]; then
  python3 -I kernel/build-record.py --self-test || fail "build record"
  ok "build record"
fi
echo "all $pass tests passed"
