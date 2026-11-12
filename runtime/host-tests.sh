#!/bin/bash
# The runtime's whole host test suite with one command, on a Linux PC or on a Mac.
#   runtime/host-tests.sh          Linux: the suite here.
#                                  macOS: builds the Android parts here with the Mac's NDK (runtime/host.mk), then runs
#                                  the suite in an arm64 Ubuntu 24.04 container (docker; colima on the Mac) with the
#                                  checkout mounted at its own path, then runtime/package/test.sh here (it needs the NDK).
#   runtime/host-tests.sh --suite  the suite alone, where it is (the container runs this)
# The suite, one line each at the end (pass, FAIL or skipped):
#   loader    make -C runtime/loader test
#   xnuhost   make -C runtime/xnuhost test: the unit tests, drm2kgsl and its leak test (LeakSanitizer) when
#             external/virglrenderer is there, and the Android xnuhost booting the test kernel (under qemu-aarch64 on
#             an x86_64 PC; as it is on an arm64 host, the container on a Mac)
#   display   make -C runtime/display test
#   bridges   runtime/bridges/tests/host.sh: the unit tests, test-binder (a binderfs in a user namespace: it says
#             "skipped" when the kernel has no binderfs), brightness, battery; the Android build when the NDK is there
#   camera    runtime/bridges/camera/tests/host.sh
#   package   runtime/package/test.sh, when the NDK is there (on a Mac it runs on the Mac, after the container)
# The container is privileged: test-binder mounts a binderfs in a user namespace and LeakSanitizer stops threads with
# ptrace, which docker's default profile refuses. Nothing else of the Mac is mounted.
# Env: IMAGE (oneios-hosttests:1, built from the Dockerfile below when missing), DOCKER (docker), NDK (host.mk's default),
#      JOBS (parallel make jobs; default: the CPUs).
set -uo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"
JOBS=${JOBS:-$(getconf _NPROCESSORS_ONLN)}
export MAKEFLAGS="${MAKEFLAGS:--j$JOBS}"

ndk_ok() { # the NDK's Android compiler for this build host
  local host=linux-x86_64 ndk
  [ "$(uname -s)" = Darwin ] && host=darwin-x86_64
  ndk=${NDK:-$( [ "$host" = darwin-x86_64 ] && echo "$HOME/Library/Android/sdk/ndk/28.2.13676358" || echo "$HOME/Android/Sdk/ndk/28.2.13676358")}
  [ -x "$ndk/toolchains/llvm/prebuilt/$host/bin/aarch64-linux-android30-clang" ]
}

results=()
part() { # NAME COMMAND...: run one part, keep its result, go on
  local name=$1; shift
  echo "==== $name: $*"
  local log rc
  log=$(mktemp)
  "$@" 2>&1 | tee "$log"; rc=${PIPESTATUS[0]}
  if [ "$rc" = 0 ]; then
    if grep -q 'skipped' "$log"; then results+=("$name: pass ($(grep -m1 'skipped' "$log" | cut -c1-100))")
    else results+=("$name: pass"); fi
  else
    results+=("$name: FAIL (exit $rc): $(grep -v '^\s*$' "$log" | tail -1 | cut -c1-140)")
  fi
  rm -f "$log"
  return 0
}
summary() {
  echo "==== host tests ($(uname -s) $(uname -m))"
  local bad=0 r
  for r in "${results[@]}"; do echo "  $r"; case "$r" in *": FAIL"*) bad=1 ;; esac; done
  return $bad
}

suite() {
  part loader make -s -C runtime/loader test
  part xnuhost make -s -C runtime/xnuhost test
  part display make -s -C runtime/display test
  part bridges runtime/bridges/tests/host.sh
  part camera runtime/bridges/camera/tests/host.sh
  if ndk_ok; then part package runtime/package/test.sh
  else results+=("package: skipped (no NDK here$( [ -f /.dockerenv ] && echo ': the Mac runs it'))"); fi
}

if [ "${1:-}" = --suite ] || [ "$(uname -s)" = Linux ]; then
  suite
  summary
  exit
fi
[ "$(uname -s)" = Darwin ] || { echo "host-tests: Linux or macOS only" >&2; exit 1; }

# ---- macOS: the Android parts here, the suite in an arm64 Linux container ----
DOCKER=${DOCKER:-docker}
IMAGE=${IMAGE:-oneios-hosttests:1}
"$DOCKER" info > /dev/null 2>&1 || {
  echo "host-tests: no docker daemon. Start one: colima start --arch aarch64 --vm-type vz --mount-type virtiofs --cpu 6 --memory 8" >&2
  exit 1; }
ndk_ok || { echo "host-tests: no NDK (runtime/host.mk: NDK=...)" >&2; exit 1; }
if ! "$DOCKER" image inspect "$IMAGE" > /dev/null 2>&1; then
  echo "host-tests: building $IMAGE"
  "$DOCKER" build --platform linux/arm64 -t "$IMAGE" - <<'DOCKERFILE' || exit 1
FROM ubuntu:24.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential clang lld make patch python3 zstd openjdk-17-jdk-headless util-linux mount file ca-certificates \
    && rm -rf /var/lib/apt/lists/*
DOCKERFILE
fi

# The Android parts the Linux tests use (check-arm64 runs build/android/xnuhost; the test kernel is built with the
# NDK's clang): built here, so the container needs no NDK. Nothing of build/host is made on the Mac.
part android-build sh -c 'make -s -C runtime/loader android && make -s -C runtime/xnuhost all build/android/testkernel.elf &&
  make -s -C runtime/bridges all && make -s -C runtime/bridges/camera all && make -s -C runtime/display all'
case "${results[*]}" in *FAIL*) summary; exit 1 ;; esac
results=()

# test-binder needs binderfs in the docker host's kernel. colima's Ubuntu VM has it as a module (binder_linux, in
# linux-modules-extra); docs/building.md section 4 has the one-time command. A VM's restart unloads it: load it again.
# Without it the test says "skipped".
if command -v colima > /dev/null && [ "$("$DOCKER" context show 2> /dev/null)" = colima ]; then
  colima ssh -- sh -c 'grep -qw binder /proc/filesystems || sudo modprobe binder_linux' > /dev/null 2>&1 ||
    echo "host-tests: colima's VM has no binder_linux module: test-binder will be skipped (docs/building.md section 4)"
fi
"$DOCKER" run --rm --platform linux/arm64 --privileged -e JOBS="$JOBS" -v "$ROOT:$ROOT" -w "$ROOT" "$IMAGE" \
  runtime/host-tests.sh --suite
container=$?
part package runtime/package/test.sh
echo "==== host tests (macOS: runtime/package/test.sh)"
for r in "${results[@]}"; do echo "  $r"; done
case "${results[*]}" in *FAIL*) exit 1 ;; esac
exit $container
