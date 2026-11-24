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

# The series and profile checks are `oneios-tool kernel series-check` (kernel/tests/series-check.py until 2026-11-13),
# and the build record writer is `oneios-tool kernel build-record` (kernel/build-record.py until then). Both run
# here when the tool is built (CI builds it first); `cargo test` in tools/oneios-tool runs the record's checks and
# the frozen record of the Python writer, tools/oneios-tool/tests/kernelpy the frozen cases of series-check.
tool=${ONEIOS_TOOL:-tools/oneios-tool/target/release/oneios-tool}
if [ -x "$tool" ]; then
  "$tool" kernel series-check || fail "series"
  ok "patch series and profile"
  "$tool" kernel build-record --self-test || fail "build record"
  ok "build record"
  # build-hosted.sh's two edits of its device copy, on copies of the committed board.toml and hosted defconfig: a
  # change of either that the edits no longer take fails here, not in a kernel build.
  t=$(mktemp -d)
  cp kernel/device/board.toml kernel/device/configs/oneios_hosted_defconfig "$t/"
  "$tool" kernel hosted-series "$t/board.toml" "$ROOT/kernel/patches/oneios-hosted/series" || fail "hosted-series"
  entries=(); for name in storage board aes keystore net hid audio gpu display; do entries+=("oneios-tool kernel gen $name"); done
  "$tool" kernel hosted-archives "$t/board.toml" "$t/oneios_hosted_defconfig" "${entries[@]}" || fail "hosted-archives"
  grep -q "\"$ROOT/kernel/patches/oneios-hosted/series\"\]" "$t/board.toml" || fail "hosted-series: no hosted series in board.toml"
  [ "$(grep -c '"oneios-tool kernel gen ' "$t/board.toml")" = 9 ] || fail "hosted-archives: not the nine archives"
  [ "$(tail -1 "$t/oneios_hosted_defconfig")" = CONFIG_XNU_NATIVE_LEGACY_DISPLAY=y ] || fail "hosted-archives: defconfig"
  ! "$tool" kernel hosted-archives "$t/board.toml" "$t/oneios_hosted_defconfig" x 2> /dev/null || fail "hosted-archives took an edited board.toml again"
  rm -rf "$t"
  ok "build-hosted's device edits take the committed board.toml and hosted defconfig"
else
  echo "skipped: patch series and profile, build record (no $tool: cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)"
fi
# The codec archive's picture helpers (kernel/patches/oneios-hosted/codec/OneiOSCodecPicture.h, which the staged
# DroidVMVTHost.cpp includes): the NV12 copy of a tile and the HEVC SPS's conformance-window crop, built for the host
# with the sanitizers (without them where the compiler has no sanitizer runtime: a slim Linux container); warnings
# are errors with clang, the kernel's compiler; then real encoders' streams cropped and decoded by ffmpeg (tests/crop.sh:
# skipped without it).
if command -v "${CC:-cc}" > /dev/null; then
  t=$(mktemp -d)
  werror=; "${CC:-cc}" --version 2>/dev/null | grep -q clang && werror=-Werror
  picture=(-std=c11 -O1 -g -Wall -Wextra $werror kernel/patches/oneios-hosted/codec/tests/test_picture.c -o "$t/test_picture")
  "${CC:-cc}" -fsanitize=address,undefined "${picture[@]}" 2> /dev/null || "${CC:-cc}" "${picture[@]}" || fail "codec picture: build"
  "$t/test_picture" || fail "codec picture"
  sh kernel/patches/oneios-hosted/codec/tests/crop.sh "$t/test_picture" "$t" || fail "codec picture: crop.sh"
  rm -rf "$t"
  ok "codec picture helpers: the tile copy and the SPS crop"
else
  echo "skipped: codec picture helpers (no ${CC:-cc})"
fi
# The GPU archive's choice of accelerator (a56-gpu, 2026-11-24; kernel/patches/oneios-hosted/gpu/OneiOSGpuChoice.h, as
# OneiOSAdreno.cpp includes it): an MSM capset (or none read) without oneios-gpu-amd keeps the rule of every boot before
# (the S24 Ultra), an AMDGPU capset never gets the Adreno accelerator, oneios-gpu-amd=1 on it gets the A56's whatever
# oneios-gpu-metal says; and the A56 accelerator's CPU-composition list, discovery keys and open types. With the
# sanitizers where the compiler has them; warnings are errors with clang (gpu/tests/test_choice.c).
if command -v "${CC:-cc}" > /dev/null; then
  t=$(mktemp -d)
  werror=; "${CC:-cc}" --version 2>/dev/null | grep -q clang && werror=-Werror
  choice=(-std=c11 -O1 -g -Wall -Wextra $werror kernel/patches/oneios-hosted/gpu/tests/test_choice.c -o "$t/test_choice")
  "${CC:-cc}" -fsanitize=address,undefined "${choice[@]}" 2> /dev/null || "${CC:-cc}" "${choice[@]}" || fail "gpu choice: build"
  "$t/test_choice" || fail "gpu choice"
  rm -rf "$t"
  ok "gpu: the accelerator's choice by capset and boot argument, the A56 accelerator's CPU list and open types"
else
  echo "skipped: gpu choice (no ${CC:-cc})"
fi
# The arm64e kernel's compiler line (kernel/linux/clang-wrapper): the PAC ABI (signed calls, returns, the intrinsics)
# and function-pointer type discrimination reach cc1 whatever the host clang implies for the triple by itself.
# A clang that refuses the kernel's own driver flags (CI's Ubuntu clang: an older release) builds no kernel either, and
# CI never builds one (the release kernel comes from kernel/release-kernel.txt): skipped with its first error. Until
# 2026-11-21 the wrapper's exit status ended this script under set -e with no line at all (release run 44882714612).
if command -v "${CROSS_CLANG:-clang}" > /dev/null && "${CROSS_CLANG:-clang}" -target arm64e-apple-ios27.0 -### -c -x c /dev/null -o /dev/null > /dev/null 2>&1 &&
   cc1=$(kernel/linux/clang-wrapper -arch arm64e -mkernel -### -c -x c /dev/null -o /dev/null 2>&1); then
  for flag in -fptrauth-calls -fptrauth-returns -fptrauth-intrinsics -fptrauth-function-pointer-type-discrimination; do
    grep -q -- "\"$flag\"" <<< "$cc1" || fail "clang-wrapper: arm64e cc1 has no $flag"
  done
  ok "clang-wrapper: arm64e PAC flags reach cc1 ($("${CROSS_CLANG:-clang}" --version | head -1))"
elif [ -n "${cc1:-}" ]; then
  echo "skipped: clang-wrapper PAC flags ($("${CROSS_CLANG:-clang}" --version | head -1) refuses the kernel's flags: $(grep -m1 -i error <<< "$cc1" || head -1 <<< "$cc1"))"
else
  echo "skipped: clang-wrapper PAC flags (no clang with an arm64e-apple-ios target here)"
fi
echo "all $pass tests passed"
