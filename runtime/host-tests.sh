#!/bin/bash
# The runtime's whole host test suite with one command, on a Linux PC or on a Mac.
#   runtime/host-tests.sh          Linux: the suite here.
#                                  macOS: builds the Android parts here with the Mac's NDK (cargo, runtime/ndk-cc), then
#                                  runs the suite in an arm64 Ubuntu 24.04 container of Apple's `container` tool (its own
#                                  small VM, booted with the kernel below) with the checkout mounted at its own path,
#                                  then runtime/package/elf-check-test.sh and runtime/package/test.sh here (they need
#                                  the NDK).
#   runtime/host-tests.sh --kernel macOS: (re)build that kernel only, then stop
#   runtime/host-tests.sh --suite  the suite alone, where it is (the container runs this, with STRICT=1 and PREBUILT=1:
#                                  the Android xnuhost and test image are the Mac's and are never remade there)
# The suite, one line each at the end (pass, FAIL or skipped). The runtime is the Rust workspace runtime/Cargo.toml
# (runtime/RUST.md):
#   workspace cargo test --workspace --locked: every crate's tests (xh-core, the loader, xnuhost with drm2kgsl and its
#             stand-in KGSL scenarios when external/virglrenderer is there (the memory ones need /dev/udmabuf), the display
#             helper, the bridges (the binder tests on a binderfs in a user namespace: they say "skipped" when the kernel
#             has no binderfs), the camera, the encoder (its stand-in streams decoded by ffmpeg when it is there)
#   lint      cargo fmt --check and cargo clippy --workspace --all-targets -D warnings, on the host and (with the NDK) for
#             aarch64-linux-android
#   boot      only with QEMU=1 (skipped by default, CLAUDE.md 2026-11-24): runtime/xnuhost/check-arm64.sh smoke check: the vCPU machinery alone, then the static Android xnuhost
#             booting the Rust test kernel three times (the normal boot, ram=auto, the restart loop) under qemu-aarch64 on
#             an x86_64 PC, as it is on an arm64 host (the container on a Mac: check only, with the Mac's binaries)
#   port      scripts/port/test-verify-host.sh: the read-only phone preflight against stand-in phones (S24 Ultra, S25
#             Ultra, S25, S26 Ultra, A56, and refusals)
#   metal-amd only with METAL_AMD=1: the Galaxy A56's guest GPU stack. On Linux, userland/metal-amd/host-test.sh (the
#             guest Mesa: its patches apply to Mesa cecc0e9, Mesa builds for Linux with radeonsi and the amdgpu virtio
#             context, and the Darwin vdrm backend against a fake of the kernel's user client); then
#             userland/metal-amd/air/host-test.sh (Jesvi-iOS's AIR frontend and air_metal on Mesa cecc0e9, driven through
#             air_metal.h on softpipe; docs/a56-gpu.md sections 3 and 4); then userland/metal-amd/gl/host-test.sh (the
#             A56's OpenGL ES for CoreAnimation, OneiOSRadeonGL: Mesa's GLES on softpipe driven through EAGL's GLI entries and
#             the 1036-slot dispatch table, over stand-in IOSurfaces in place and copied; docs/a56-gpu.md section 4a)
#   links     runtime/package/elf-check-test.sh, when the NDK is there (on a Mac: on the Mac, after the container): the
#             link rule of Android shared objects (-z defs) and runtime/package/elf-check.sh, on a library linked without
#             it (must fail) and on a stage of runtime/build.sh
#   package   runtime/package/test.sh, when the NDK is there (on a Mac it runs on the Mac, after the container)
# The kernel (macOS): the default kernel of `container` has neither binderfs nor udmabuf and loads no modules. The first
# run builds, once, Linux $KVER (kernel.org, SHA-256 checked) with the default kernel's own configuration
# (/proc/config.gz) plus ANDROID_BINDER_IPC, ANDROID_BINDERFS (no default devices) and UDMABUF, in a container, into
# out/host-tests/vmlinux-$KVER-oneios (about 15 minutes on an M4). Only this script's containers boot it (`container run --kernel`);
# the system's default kernel is not changed. On this path a binder test skipped for want of a binderfs, or a missing
# /dev/udmabuf, is a FAIL (the lines the harness-less binder tests print; a library test's skip line is captured by cargo).
# Capabilities: none but CAP_SETFCAP (--cap-drop ALL --cap-add SETFCAP). The binder tests re-run themselves in a user
# namespace (runtime/bridges/hal/binderfs.rs) that maps uid 0, which the kernel allows only with CAP_SETFCAP; the binderfs
# mount in that namespace needs nothing more (there is no seccomp filter). Nothing else of the Mac is mounted.
# Env: IMAGE (oneios-hosttests:4, built from the Containerfile below when missing), NDK (the SDK's ndk/28.2.13676358),
#      JOBS (the container's CPUs; default: the Mac's CPUs), MEMORY (the container's, 8G), CARGO_TARGET_DIR
#      (runtime/target/host-tests), KERNEL (another kernel file for the container).
set -uo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"
JOBS=${JOBS:-$(getconf _NPROCESSORS_ONLN)}

ndk_ok() { # the NDK's Android compiler for this build host
  local host=linux-x86_64 ndk
  [ "$(uname -s)" = Darwin ] && host=darwin-x86_64
  ndk=${NDK:-$( [ "$host" = darwin-x86_64 ] && echo "$HOME/Library/Android/sdk/ndk/28.2.13676358" || echo "$HOME/Android/Sdk/ndk/28.2.13676358")}
  [ -x "$ndk/toolchains/llvm/prebuilt/$host/bin/aarch64-linux-android30-clang" ]
}
[ -d "$HOME/.cargo/bin" ] && PATH=$HOME/.cargo/bin:$PATH
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$ROOT/runtime/target/host-tests}
# What the Mac builds for the container (PREBUILT=1): the static xnuhost and the test image, under out/host-tests.
PREBUILT_XNUHOST=$ROOT/out/host-tests/xnuhost
PREBUILT_IMAGE=$ROOT/out/host-tests/testimage

results=()
part() { # NAME COMMAND...: run one part, keep its result, go on
  local name=$1; shift
  echo "==== $name: $*"
  local log rc
  log=$(mktemp)
  "$@" 2>&1 | tee "$log"; rc=${PIPESTATUS[0]}
  if [ "$rc" = 0 ]; then
    if [ "${STRICT:-0}" = 1 ] && grep -q 'skipped (no binderfs\|no /dev/udmabuf' "$log"; then
      results+=("$name: FAIL (required here): $(grep -m1 'skipped (no binderfs\|no /dev/udmabuf' "$log" | cut -c1-100)")
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
  part workspace sh -c 'cd runtime && cargo test --workspace --locked'
  part lint sh -c 'cd runtime && cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings'
  if ndk_ok; then part lint-android sh -c 'cd runtime && cargo clippy --workspace --all-targets --locked --target aarch64-linux-android -- -D warnings'; fi
  # PREBUILT=1 (the Mac's container): the Mac built the static xnuhost and the test image with its NDK just before, and
  # the container has no NDK: check-arm64.sh boots those as they are and builds nothing.
  # QEMU=1 runs the boot part. By default it is skipped: the phone is the test (CLAUDE.md, the owner's rule of
  # 2026-11-24); a helper hands the build to the Mac for a phone boot instead.
  if [ "${QEMU:-0}" != 1 ]; then
    results+=("boot: skipped (the qemu boot runs only with QEMU=1: the phone is the test, CLAUDE.md 2026-11-24)")
  elif [ "${PREBUILT:-0}" = 1 ]; then
    if [ -f "$PREBUILT_XNUHOST" ] && [ -f "$PREBUILT_IMAGE/kernel.kc" ]; then
      part boot env XNUHOST="$PREBUILT_XNUHOST" IMAGE="$PREBUILT_IMAGE" runtime/xnuhost/check-arm64.sh check
    else results+=("boot: FAIL: no $PREBUILT_XNUHOST or $PREBUILT_IMAGE: the Mac builds them before the container"); fi
  elif ndk_ok; then
    part boot runtime/xnuhost/check-arm64.sh smoke check
  else results+=("boot: skipped (no NDK here: the static xnuhost and the test kernel need it)"); fi
  part port sh scripts/port/test-verify-host.sh
  # METAL_AMD=1: the A56's guest Mesa and its Metal layer (Mesa builds of several minutes each and about 1.7 GB in
  # out/metal-amd, so not by default; they need external/mesa-droidvm). The guest Mesa test is Linux only.
  if [ "${METAL_AMD:-0}" != 1 ]; then
    results+=("metal-amd: skipped (runs only with METAL_AMD=1: userland/metal-amd/host-test.sh, userland/metal-amd/air/host-test.sh, userland/metal-amd/gl/host-test.sh)")
  else
    if [ "$(uname -s)" = Linux ]; then part metal-amd sh userland/metal-amd/host-test.sh
    else results+=("metal-amd: skipped (Linux only: it builds Mesa for Linux)"); fi
    part metal-amd-air sh userland/metal-amd/air/host-test.sh
    part metal-amd-gl sh userland/metal-amd/gl/host-test.sh
  fi
  if ndk_ok; then
    part links runtime/package/elf-check-test.sh
    part package runtime/package/test.sh
  else results+=("links, package: skipped (no NDK here$( [ "${STRICT:-0}" = 1 ] && echo ': the Mac runs them'))"); fi
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
IMAGE=${IMAGE:-oneios-hosttests:4}
MEMORY=${MEMORY:-8G}
KVER=6.18.56
KSHA=4633bbda2ab95eaf2a7af4041236451d6759c72e6946265531af4b3aacefbf89
KOUT=$ROOT/out/host-tests
KERNEL=${KERNEL:-$KOUT/vmlinux-$KVER-oneios}
if ! container image inspect "$IMAGE" > /dev/null 2>&1; then
  echo "host-tests: building $IMAGE"
  ctx=$(mktemp -d)
  # the tests' tools, Rust (rustup's stable toolchain, as docs/building.md section 0), and what a kernel build needs
  # (bc bison flex libelf-dev libssl-dev). No python3: the suite has no Python. Image :4 adds Rust for the Rust runtime
  # (2026-11-23); :3 had the C toolchain only.
  cat > "$ctx/Containerfile" <<'CONTAINERFILE'
FROM ubuntu:24.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential clang lld make patch zstd openjdk-17-jdk-headless util-linux mount file ca-certificates curl \
      bc bison flex libelf-dev libssl-dev xz-utils kmod cpio ffmpeg \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --component rustfmt,clippy \
    && chmod -R a+rwX /opt/cargo /opt/rustup
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
ndk_ok || { echo "host-tests: no NDK (NDK=...)" >&2; exit 1; }

# The Android parts the Linux tests use (check-arm64 boots the static xnuhost; the test kernel and the task stub are
# bare-metal Rust, objcopied with the NDK's llvm-objcopy): built here, so the container needs no NDK. The container's
# own cargo builds the host tests into a target dir of its own (a Linux one, never the Mac's).
mkdir -p "$PREBUILT_IMAGE"
part android-build sh -c '
  cd runtime &&
  CARGO_TARGET_DIR=target/host-tests-static cargo build --release --locked --target aarch64-linux-android -p xnuhost --bin xnuhost \
    --config "target.aarch64-linux-android.rustflags=[\"-C\",\"target-feature=+crt-static\"]" &&
  cp target/host-tests-static/aarch64-linux-android/release/xnuhost "$1" &&
  sh xnuhost/testkernel/build.sh "$2/testkernel.elf" &&
  cargo run --quiet --locked -p xnuloader --bin mkkernel -- "$2/testkernel.elf" "$2/kernel.kc" "$2/os.img"' \
  sh "$PREBUILT_XNUHOST" "$PREBUILT_IMAGE"
case "${results[*]}" in *FAIL*) summary; exit 1 ;; esac
results=()

container run --rm --progress none --cap-drop ALL --cap-add SETFCAP --kernel "$KERNEL" --cpus "$JOBS" --memory "$MEMORY" -e JOBS="$JOBS" -e STRICT=1 -e PREBUILT=1 \
  -e CARGO_TARGET_DIR="$ROOT/runtime/target/host-tests-linux" -v "$ROOT:$ROOT" -w "$ROOT" "$IMAGE" runtime/host-tests.sh --suite
inside=$?
part links runtime/package/elf-check-test.sh
part package runtime/package/test.sh
echo "==== host tests (macOS: runtime/package/elf-check-test.sh, runtime/package/test.sh)"
for r in "${results[@]}"; do echo "  $r"; done
case "${results[*]}" in *FAIL*) exit 1 ;; esac
exit $inside
