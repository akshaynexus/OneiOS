#!/bin/bash
# Host tests of kernel/ that need no toolchain and no network: script syntax, the pin tables, the patch series
# files, the profile, the build-record writer, and two checks of the hosted patches (recount reads with interrupts
# masked, function-pointer strips). Run by CI and by hand: kernel/tests/host.sh
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
# recount's readers of the current thread assert interrupts off (recount.c: recount_current_thread_time_mach, _times,
# _usage, _perf_level_usage, _energy_nj). A patch's call of one must mask first: ml_set_interrupts_enabled(FALSE),
# disable_ints*, hosted_irq_mask() in the 4 lines before it, or "interrupts off" written on the call's line (a caller
# that holds them off). Found: 0095 read the thread's time right after enable_ints_and_unlock(); the phone panicked on
# the first callout (2026-11-23 22:16, "recount.c:753 Assertion failed: ml_get_interrupts_enabled() == FALSE").
recount_unmasked() {
  awk '
    { line[NR % 5] = $0 }
    /^\+/ && !/^\+\+\+/ && /recount_current_thread_(time_mach|times|usage|perf_level_usage|energy_nj)\(/ && !/interrupts off/ {
      ok = 0
      for (i = 1; i <= 4; i++) if (line[(NR - i) % 5] ~ /ml_set_interrupts_enabled\(FALSE\)|disable_ints|hosted_irq_mask\(\)/) ok = 1
      if (!ok) { print FILENAME ":" FNR ": " $0; bad = 1 }
    }
    END { exit bad }' "$@"
}
t=$(mktemp -d)
printf '%s\n' '+		enable_ints_and_unlock(group, s);' '+		uint64_t t0 = recount_current_thread_time_mach();' > "$t/unmasked.patch"
printf '%s\n' '+	boolean_t was = ml_set_interrupts_enabled(FALSE);' '+	uint64_t t = recount_current_thread_time_mach();' > "$t/masked.patch"
! recount_unmasked "$t/unmasked.patch" > /dev/null || fail "recount check: took a read with interrupts on"
recount_unmasked "$t/masked.patch" || fail "recount check: refused a masked read"
rm -rf "$t"
recount_unmasked kernel/patches/oneios-hosted/*.patch kernel/patches/oneios-hosted/optional/*.patch ||
  fail "a hosted patch reads recount's current-thread time with interrupts on (above)"
ok "hosted patches read recount's current-thread time with interrupts masked"
# A function pointer's address without its signature: ptrauth_strip((void *)p, ...) or ptrauth_strip(ptrauth_nop_cast(
# void *, p), ...). On the typed pointer itself, ptrauth_strip(func, ptrauth_key_function_pointer), clang converts the
# stripped value back by authenticating it with the type's discriminator (xpaci, then autia), and that fails every time.
# Found: 0095's first fix, a qemu boot of 2026-11-23: "PAC failure from kernel with IA key while authing x16" at
# thread_call_thread+0x288 (hosted_tc_count), the first callout.
fptr_strip_typed() {
  awk '/^\+/ && !/^\+\+\+/ && /ptrauth_strip\(/ && /ptrauth_key_function_pointer/ &&
       !/ptrauth_strip\(\(void \*\)/ && !/ptrauth_strip\(ptrauth_nop_cast\(/ { print FILENAME ":" FNR ": " $0; bad = 1 }
       END { exit bad }' "$@"
}
t=$(mktemp -d)
printf '%s\n' '+	f = (uint64_t)ptrauth_strip(func, ptrauth_key_function_pointer);' > "$t/typed.patch"
printf '%s\n' '+	f = (uint64_t)ptrauth_strip(ptrauth_nop_cast(void *, func), ptrauth_key_function_pointer);' > "$t/raw.patch"
! fptr_strip_typed "$t/typed.patch" > /dev/null || fail "strip check: took a typed function pointer"
fptr_strip_typed "$t/raw.patch" || fail "strip check: refused ptrauth_nop_cast"
rm -rf "$t"
fptr_strip_typed kernel/patches/oneios-hosted/*.patch kernel/patches/oneios-hosted/optional/*.patch ||
  fail "a hosted patch strips a typed function pointer (above): clang authenticates the stripped value"
ok "hosted patches strip function pointers through (void *) or ptrauth_nop_cast"
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
# The codec transport's wait policy (kernel/patches/oneios-hosted/codec/OneiOSCodecWait.h, which the staged
# DroidVMVirtioMedia.c and OneiOSCodec.c include, 2026-11-25): interrupt or polled and why, the event worker's sleep and
# guard, the wake counters, the "transport:" line, both paths; built as the picture test is.
if command -v "${CC:-cc}" > /dev/null; then
  t=$(mktemp -d)
  werror=; "${CC:-cc}" --version 2>/dev/null | grep -q clang && werror=-Werror
  wait_test=(-std=c11 -O1 -g -Wall -Wextra $werror kernel/patches/oneios-hosted/codec/tests/test_wait.c -o "$t/test_wait")
  "${CC:-cc}" -fsanitize=address,undefined "${wait_test[@]}" 2> /dev/null || "${CC:-cc}" "${wait_test[@]}" || fail "codec wait: build"
  "$t/test_wait" || fail "codec wait"
  rm -rf "$t"
  ok "codec transport's wait policy: interrupt and polled"
else
  echo "skipped: codec transport's wait policy (no ${CC:-cc})"
fi
# Hosted 0098's rename of launchd's DisableAllowed marks: the function as the patch adds it (its + lines from
# "static int" to the closing brace), built with a made-up plist test (kernel/patches/oneios-hosted/tests).
if command -v "${CC:-cc}" > /dev/null; then
  t=$(mktemp -d)
  awk '/^\+oneios_disable_allowed_marks_in\(/ { print prev; on = 1 } on { print } on && /^\+}/ { exit } { prev = $0 }' \
    kernel/patches/oneios-hosted/0098-disable-allowed.patch | sed 's/^+//' > "$t/marks.c"
  grep -q 'memcpy(data + i + 1, to, 14)' "$t/marks.c" || fail "0098: oneios_disable_allowed_marks_in not found in the patch"
  "${CC:-cc}" -std=c11 -Wall -Wextra -I "$t" kernel/patches/oneios-hosted/tests/test_disable_allowed.c -o "$t/test" || fail "0098: build"
  "$t/test" || fail "0098: rename"
  rm -rf "$t"
  ok "0098: DisableAllowed marks renamed in place"
else
  echo "skipped: 0098's rename (no ${CC:-cc})"
fi
# Hosted 0103's rewrite of iOS 27 digitizer events into iOS 16.5's layout (the A56's touch): hosted_hid16_element and
# its helpers as the patch adds them (its + lines from "static int hosted_hid16" to the queue walk, which needs the
# kernel's types), built with an element made like the fork's dumped touch entry (kernel/patches/oneios-hosted/tests).
if command -v "${CC:-cc}" > /dev/null; then
  t=$(mktemp -d)
  awk '/^\+static int hosted_hid16 = -1;/ { on = 1 } /^\+\/\* The unread elements/ { exit } on { print }' \
    kernel/patches/oneios-hosted/0103-hid-digitizer-ios16.patch | sed 's/^+//' > "$t/hid16.c"
  grep -qF 'hosted_hid16_element(uint8_t *p, uint32_t len)' "$t/hid16.c" || fail "0103: hosted_hid16_element not found in the patch"
  "${CC:-cc}" -std=c11 -Wall -Wextra -Wno-unused-variable -I "$t" kernel/patches/oneios-hosted/tests/test_hid16.c -o "$t/test" || fail "0103: build"
  "$t/test" || fail "0103: digitizer rewrite"
  rm -rf "$t"
  ok "0103: iOS 27 digitizer events rewritten in iOS 16.5's layout"
else
  echo "skipped: 0103's rewrite (no ${CC:-cc})"
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
