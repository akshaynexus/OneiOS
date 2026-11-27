#!/bin/sh
# Host test of scripts/port/verify-host.sh against stand-in phones: a getprop that answers from the environment and a
# sysfs/dev tree in a temporary folder. Run by runtime/host-tests.sh (part "port") and by hand.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
cat > "$T/getprop" <<'EOF'
#!/bin/sh
case "$1" in
  ro.product.model) echo "$FAKE_MODEL" ;;
  ro.product.device) echo "$FAKE_DEVICE" ;;
  ro.soc.model) echo "$FAKE_SOC" ;;
  ro.board.platform) echo "$FAKE_BOARD" ;;
esac
EOF
chmod +x "$T/getprop"
pass=0
fail() { echo "FAIL: $*" >&2; exit 1; }

# phone MODEL DEVICE SOC BOARD GPU(kgsl|amdgpu|none): a fresh stand-in root
phone() {
  rm -rf "$T/root"
  mkdir -p "$T/root/sys/module/udmabuf/parameters" "$T/root/sys/devices/system/cpu/cpu0" "$T/root/sys/devices/system/cpu/cpu7"
  echo 64 > "$T/root/sys/module/udmabuf/parameters/size_limit_mb"
  echo 1024 > "$T/root/sys/module/udmabuf/parameters/list_limit"
  case "$5" in
    kgsl) mkdir -p "$T/root/sys/class/kgsl/kgsl-3d0/device"; echo 0x44050000 > "$T/root/sys/class/kgsl/kgsl-3d0/device/chipid" ;;
    amdgpu) mkdir -p "$T/root/dev/dri"; : > "$T/root/dev/dri/renderD128" ;;
  esac
  FAKE_MODEL=$1 FAKE_DEVICE=$2 FAKE_SOC=$3 FAKE_BOARD=$4
  export FAKE_MODEL FAKE_DEVICE FAKE_SOC FAKE_BOARD
}
run() { VERIFY_HOST_GETPROP="$T/getprop" VERIFY_HOST_ROOT="$T/root" sh "$HERE/verify-host.sh" > "$T/out" 2>&1; }
# expect EXIT TEXT NAME
expect() {
  rc=0; run || rc=$?
  [ "$rc" = "$1" ] || { cat "$T/out" >&2; fail "$3: exit $rc, expected $1"; }
  grep -qF -- "$2" "$T/out" || { cat "$T/out" >&2; fail "$3: no '$2'"; }
  pass=$((pass + 1)); echo "ok $pass: $3"
}

phone SM-S928U e3q SM8650 pineapple kgsl;  expect 0 "profile ok (s24-ultra)" "the S24 Ultra passes"
grep -qF "kgsl chipid: 0x44050000" "$T/out" && grep -qF "udmabuf list_limit: 1024" "$T/out" && grep -qF "cpus: 2" "$T/out" || fail "S24 facts"
# Every other S24 Ultra model number (runtime/bridges/hw/host_device.c's record): the same check, the same profile.
for m in SM-S928B SM-S928U1 SM-S928W SM-S928N SM-S9280; do
  phone "$m" e3q SM8650 pineapple kgsl;    expect 0 "profile ok (s24-ultra)" "the S24 Ultra $m passes"
done
phone SM-S938B pa3q SM8750 sun kgsl;       expect 0 "profile ok (s25-ultra)" "the S25 Ultra passes"
phone SM-S931B pa1q SM8750 sun kgsl;       expect 0 "profile ok (s25)" "the base S25 passes"
phone SM-S948B m3q SM8850 canoe kgsl;      expect 0 "profile ok (s26-ultra)" "the S26 Ultra passes"
phone SM-S942B m1q SM8850 canoe kgsl;      expect 0 "profile ok (s26)" "the base S26 (SM8850) passes"
phone SC-51G m1q SM8850 canoe kgsl;        expect 0 "profile ok (s26)" "the docomo S26 (SC-51G) passes"
phone SM-S941B m1q SM8850 canoe kgsl;      expect 0 "profile ok (s26)" "the S26 SM-S941 (SM8850) passes"
phone SM-S942B m1s s5e9965 s5e9965 amdgpu; expect 0 "profile ok (s26-exynos)" "the base S26 (Exynos) passes"
phone SM-S948B m1q SM8850 canoe kgsl;      expect 2 "device=m1q (expected m3q)" "an S26 Ultra model with another codename is refused"
phone SM-A566E a56x s5e8855 s5e8855 amdgpu; expect 0 "amdgpu render node: /dev/dri/renderD128" "the A56 passes, with its render node"
grep -qF "profile ok (a56)" "$T/out" || fail "A56 profile line"
phone SM-A566E a56x s5e8855 s5e8855 none;  expect 0 "amdgpu render node: none" "an A56 without the render node says so"
phone SM-S931B pa1q SM8750 sun none;       expect 0 "kgsl chipid: none" "a KGSL-less Snapdragon says 2D"
phone SM-S928U pa3q SM8750 sun kgsl;       expect 2 "soc=SM8750 (expected SM8650)" "an S24 model on an SM8750 is refused"
phone SM-S931B pa3q SM8750 sun kgsl;       expect 2 "device=pa3q (expected pa1q)" "a base S25 model with the Ultra's codename is refused"
phone SM-A566E a56x SM8650 x kgsl;         expect 2 "expected s5e8855" "an A56 model on another SoC is refused"
phone SM-G991B o1s s5e9840 exynos2100 none; expect 2 "unsupported model" "another phone is refused"
echo "verify-host tests: $pass passed"
