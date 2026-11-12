#!/bin/bash
# The runtime's whole host test suite with one command, on a Linux PC or on a Mac.
#   runtime/host-tests.sh          Linux: the suite here.
#                                  macOS: builds the Android parts here with the Mac's NDK (runtime/host.mk), then runs
#                                  the suite in an arm64 Ubuntu 24.04 container of Apple's `container` tool (its own
#                                  small VM, booted with the kernel below) with the checkout mounted at its own path,
#                                  then runtime/package/test.sh here (it needs the NDK).
#   runtime/host-tests.sh --kernel macOS: (re)build that kernel only, then stop
#   runtime/host-tests.sh --suite  the suite alone, where it is (the container runs this)
# The suite, one line each at the end (pass, FAIL or skipped):
#   loader    make -C runtime/loader test
#   xnuhost   make -C runtime/xnuhost test: the unit tests, drm2kgsl and its leak test (LeakSanitizer) when
#             external/virglrenderer is there (both need /dev/udmabuf), and the Android xnuhost booting the test kernel
#             (under qemu-aarch64 on an x86_64 PC; as it is on an arm64 host, the container on a Mac)
#   display   make -C runtime/display test
#   bridges   runtime/bridges/tests/host.sh: the unit tests, test-binder (a binderfs in a user namespace: it says
#             "skipped" when the kernel has no binderfs), brightness, battery; the Android build when the NDK is there
#   camera    runtime/bridges/camera/tests/host.sh
#   package   runtime/package/test.sh, when the NDK is there (on a Mac it runs on the Mac, after the container)
# The kernel (macOS): the default kernel of `container` has neither binderfs nor udmabuf and loads no modules. The first
# run builds, once, Linux $KVER (kernel.org, SHA-256 checked) with the default kernel's own configuration
# (/proc/config.gz) plus ANDROID_BINDER_IPC, ANDROID_BINDERFS (no default devices) and UDMABUF, in a container, into
# out/host-tests/vmlinux-$KVER-oneios (about 15 minutes on an M4). Only this script's containers boot it (`container run --kernel`);
# the system's default kernel is not changed. On this path a skipped test-binder or a missing /dev/udmabuf is a FAIL.
# Capabilities: none but CAP_SETFCAP (--cap-drop ALL --cap-add SETFCAP). test-binder's `unshare -Urm` maps uid 0, which
# the kernel allows only with CAP_SETFCAP; the binderfs mount in that namespace and LeakSanitizer's ptrace of its own
# threads need nothing more (there is no seccomp filter). Nothing else of the Mac is mounted.
# Env: IMAGE (oneios-hosttests:2, built from the Containerfile below when missing), NDK (host.mk's default),
#      JOBS (parallel make jobs and the container's CPUs; default: the Mac's CPUs), MEMORY (the container's, 8G),
#      KERNEL (another kernel file for the container).
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
    if [ "${STRICT:-0}" = 1 ] && grep -q 'test-binder: skipped\|no /dev/udmabuf' "$log"; then
      results+=("$name: FAIL (required here): $(grep -m1 'test-binder: skipped\|no /dev/udmabuf' "$log" | cut -c1-100)")
    elif grep -q 'skipped' "$log"; then results+=("$name: pass ($(grep -m1 'skipped' "$log" | cut -c1-100))")
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
  else results+=("package: skipped (no NDK here$( [ "${STRICT:-0}" = 1 ] && echo ': the Mac runs it'))"); fi
}

if [ "${1:-}" = --suite ] || [ "$(uname -s)" = Linux ]; then
  suite
  summary
  exit
fi
[ "$(uname -s)" = Darwin ] || { echo "host-tests: Linux or macOS only" >&2; exit 1; }

# ---- macOS: the Android parts here, the suite in an arm64 Linux container of Apple's `container` ----
command -v container > /dev/null || { echo "host-tests: no container tool: brew install container && container system start" >&2; exit 1; }
container system status > /dev/null 2>&1 || container system start --enable-kernel-install || exit 1
IMAGE=${IMAGE:-oneios-hosttests:2}
MEMORY=${MEMORY:-8G}
KVER=6.18.56
KSHA=4633bbda2ab95eaf2a7af4041236451d6759c72e6946265531af4b3aacefbf89
KOUT=$ROOT/out/host-tests
KERNEL=${KERNEL:-$KOUT/vmlinux-$KVER-oneios}
if ! container image inspect "$IMAGE" > /dev/null 2>&1; then
  echo "host-tests: building $IMAGE"
  ctx=$(mktemp -d)
  # the tests' tools, and what a kernel build needs (bc bison flex libelf-dev libssl-dev)
  cat > "$ctx/Containerfile" <<'CONTAINERFILE'
FROM ubuntu:24.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential clang lld make patch python3 zstd openjdk-17-jdk-headless util-linux mount file ca-certificates \
      bc bison flex libelf-dev libssl-dev xz-utils kmod cpio \
    && rm -rf /var/lib/apt/lists/*
CONTAINERFILE
  container build --platform linux/arm64 -t "$IMAGE" -f "$ctx/Containerfile" "$ctx" || { rm -rf "$ctx"; exit 1; }
  rm -rf "$ctx"
fi

if [ ! -f "$KERNEL" ] || [ "${1:-}" = --kernel ]; then
  echo "host-tests: building the container's kernel: Linux $KVER + binderfs + udmabuf -> $KERNEL"
  mkdir -p "$KOUT"
  src=$KOUT/linux-$KVER.tar.xz
  if [ "$(shasum -a 256 "$src" 2> /dev/null | cut -d' ' -f1)" != "$KSHA" ]; then
    curl -fL --retry 3 -o "$src.part" "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$KVER.tar.xz" || exit 1
    [ "$(shasum -a 256 "$src.part" | cut -d' ' -f1)" = "$KSHA" ] || { echo "host-tests: linux-$KVER.tar.xz: SHA-256 differs" >&2; exit 1; }
    mv "$src.part" "$src"
  fi
  # The base: the configuration of the kernel `container` boots by default (the same 6.18 series).
  container run --rm --progress none "$IMAGE" sh -c 'uname -r >&2; zcat /proc/config.gz' > "$KOUT/config.default" || exit 1
  container run --rm --progress none --cpus "$JOBS" --memory "$MEMORY" -v "$KOUT:/k" "$IMAGE" bash -c '
    set -e
    mkdir -p /kbuild && tar -xf /k/linux-'"$KVER"'.tar.xz -C /kbuild --strip-components=1 && cd /kbuild
    cp /k/config.default .config
    scripts/config --enable ANDROID_BINDER_IPC --enable ANDROID_BINDERFS --set-str ANDROID_BINDER_DEVICES "" \
      --enable UDMABUF --set-str LOCALVERSION "-oneios"
    make -s olddefconfig
    for c in ANDROID_BINDER_IPC ANDROID_BINDERFS UDMABUF USER_NS; do grep -qx "CONFIG_$c=y" .config || { echo "no CONFIG_$c" >&2; exit 1; }; done
    make -s -j"$(nproc)" Image
    cp .config /k/config.oneios && cp arch/arm64/boot/Image /k/vmlinux.new' || exit 1
  mv "$KOUT/vmlinux.new" "$KERNEL"
  echo "host-tests: kernel $KERNEL"
  [ "${1:-}" = --kernel ] && exit 0
fi
ndk_ok || { echo "host-tests: no NDK (runtime/host.mk: NDK=...)" >&2; exit 1; }

# The Android parts the Linux tests use (check-arm64 runs build/android/xnuhost; the test kernel is built with the
# NDK's clang): built here, so the container needs no NDK. Nothing of build/host is made on the Mac.
part android-build sh -c 'make -s -C runtime/loader android && make -s -C runtime/xnuhost all build/android/testkernel.elf &&
  make -s -C runtime/bridges all && make -s -C runtime/bridges/camera all && make -s -C runtime/display all'
case "${results[*]}" in *FAIL*) summary; exit 1 ;; esac
results=()

container run --rm --progress none --cap-drop ALL --cap-add SETFCAP --kernel "$KERNEL" --cpus "$JOBS" --memory "$MEMORY" -e JOBS="$JOBS" -e STRICT=1 \
  -v "$ROOT:$ROOT" -w "$ROOT" "$IMAGE" runtime/host-tests.sh --suite
inside=$?
part package runtime/package/test.sh
echo "==== host tests (macOS: runtime/package/test.sh)"
for r in "${results[@]}"; do echo "  $r"; done
case "${results[*]}" in *FAIL*) exit 1 ;; esac
exit $inside
