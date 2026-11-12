#!/bin/bash
# Host tests of kernel/ that need no toolchain and no network: script syntax, the pin tables, the patch series
# files, the profile and the build-record writer. Run by CI and by hand: kernel/tests/host.sh
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"
pass=0
ok() { pass=$((pass + 1)); echo "ok $pass: $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

for f in kernel/*.sh kernel/lib/*.sh kernel/linux/*.sh kernel/prelink/*.sh kernel/tests/*.sh scripts/image/kernel-inputs.sh; do bash -n "$f" || fail "syntax: $f"; done
for f in kernel/*.py kernel/linux/*.py; do [ -e "$f" ] || continue; python3 -I -m py_compile "$f" || fail "syntax: $f"; done
ok "script syntax"

# The portable helpers (kernel/lib/host.sh) with this host's tools: GNU on Linux, BSD on macOS.
. kernel/lib/host.sh
t=$(mktemp -d)
printf 'abc\n' > "$t/f"
[ "$(file_size "$t/f")" = 4 ] || fail "file_size"
[ "$(sha256 "$t/f")" = edeaaff3f1774ad2888673770c6d64097e391bc362d7d6fb34982ddf0efd18cb ] || fail "sha256"
sed_i 's/b/x/' "$t/f"; [ "$(cat "$t/f")" = axc ] || fail "sed_i"
[ "$(host_jobs)" -ge 1 ] || fail "host_jobs"
rm -rf "$t"
ok "portable helpers ($(uname -s))"

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
# The build record writer is `oneios-tool kernel build-record` (kernel/build-record.py until 2026-11-13). Its
# self-test runs here when the tool is built; `cargo test` in tools/oneios-tool runs the same checks and the frozen
# record of the Python writer.
tool=${ONEIOS_TOOL:-tools/oneios-tool/target/release/oneios-tool}
if [ -x "$tool" ]; then
  "$tool" kernel build-record --self-test || fail "build record"
  ok "build record"
else
  echo "skipped: build record (no $tool: cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)"
fi
echo "all $pass tests passed"
