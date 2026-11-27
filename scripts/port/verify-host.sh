#!/bin/sh
# Read-only host check for OneiOS porting (run on the phone as root beside One UI).
# Exits 0 when model, device and SoC are one of the phones this tree has a record for; 2 otherwise.
# Prints the GPU, udmabuf and CPU facts a port needs; changes nothing.
#   SM-S928* e3q  SM8650   Galaxy S24 Ultra (home)                       Adreno 750, KGSL
#   SM-S938* pa3q SM8750   Galaxy S25 Ultra (docs/s25.md)                Adreno 830, KGSL
#   SM-S931* pa1q SM8750   Galaxy S25 (docs/s25.md)                      Adreno 830, KGSL
#   SM-S948* m3q  SM8850   Galaxy S26 Ultra (docs/s26.md)            Adreno 840, KGSL
#   SM-A566* a56x s5e8855  Galaxy A56 (docs/a56.md)                      Xclipse 540, amdgpu (no 3D host backend here)
# Stand-in (host tests, scripts/port/test-verify-host.sh): VERIFY_HOST_GETPROP names the getprop to run,
# VERIFY_HOST_ROOT is prefixed to every /sys and /dev path.

set -eu

GETPROP=${VERIFY_HOST_GETPROP:-getprop}
R=${VERIFY_HOST_ROOT:-}
MODEL=$("$GETPROP" ro.product.model 2>/dev/null || true)
DEVICE=$("$GETPROP" ro.product.device 2>/dev/null || true)
SOC=$("$GETPROP" ro.soc.model 2>/dev/null || true)
BOARD=$("$GETPROP" ro.board.platform 2>/dev/null || true)

echo "host: model=$MODEL device=$DEVICE soc=$SOC board=$BOARD"

case "$MODEL" in
  SM-S928*) HOST=s24-ultra WANT_DEVICE=e3q WANT_SOC=SM8650 ;;
  SM-S938*) HOST=s25-ultra WANT_DEVICE=pa3q WANT_SOC=SM8750 ;;
  SM-S931*) HOST=s25 WANT_DEVICE=pa1q WANT_SOC=SM8750 ;;
  SM-S948*) HOST=s26-ultra WANT_DEVICE=m3q WANT_SOC=SM8850 ;;
  SM-S942*|SM-S941*)
    case "$DEVICE:$SOC" in
      m1s:*|*:s5e9965) HOST=s26-exynos WANT_DEVICE=m1s WANT_SOC=s5e9965 ;;
      *) HOST=s26 WANT_DEVICE=m1q WANT_SOC=SM8850 ;;
    esac
    ;;
  SC-51G*|SCG36*) HOST=s26 WANT_DEVICE=m1q WANT_SOC=SM8850 ;;
  SM-A566*) HOST=a56 WANT_DEVICE=a56x WANT_SOC=s5e8855 ;;
  *) echo "verify-host: unsupported model (need SM-S928, SM-S938, SM-S931, SM-S948, SM-S942/SM-S941/SC-51G/SCG36 or SM-A566)" >&2; exit 2 ;;
esac
[ "$SOC" = "$WANT_SOC" ] || { echo "verify-host: $MODEL but soc=$SOC (expected $WANT_SOC)" >&2; exit 2; }
[ "$DEVICE" = "$WANT_DEVICE" ] || { echo "verify-host: $WANT_SOC but device=$DEVICE (expected $WANT_DEVICE)" >&2; exit 2; }

echo "verify-host: profile ok ($HOST)"

if [ -r "$R/sys/class/kgsl/kgsl-3d0/device/chipid" ]; then
  printf 'kgsl chipid: '
  cat "$R/sys/class/kgsl/kgsl-3d0/device/chipid" 2>/dev/null || true
elif [ "$HOST" = a56 ]; then
  # Xclipse (amdgpu): the render node the fork's tools/test_a56_gpu.c opens. This tree's xnuhost has no amdgpu 3D
  # backend, so gpu=auto gives the guest 2D here (docs/a56.md).
  if [ -e "$R/dev/dri/renderD128" ]; then echo "amdgpu render node: /dev/dri/renderD128 (3D host backend: none in this tree, 2D)"
  else echo "amdgpu render node: none at /dev/dri/renderD128"; fi
else
  echo "kgsl chipid: none ($R/sys/class/kgsl/kgsl-3d0 missing: xnuhost will run 2D)"
fi
if [ -r "$R/sys/module/udmabuf/parameters/size_limit_mb" ]; then
  printf 'udmabuf size_limit_mb: '
  cat "$R/sys/module/udmabuf/parameters/size_limit_mb"
  printf 'udmabuf list_limit: '
  cat "$R/sys/module/udmabuf/parameters/list_limit"
fi

ncpu=$(ls -d "$R"/sys/devices/system/cpu/cpu[0-9]* 2>/dev/null | wc -l | tr -d ' ')
echo "cpus: $ncpu"

exit 0
