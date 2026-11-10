# Sourced by build-linux.sh and build-macos.sh: the sources at their pins and the OneiOS layers staged into them.
# Needs ROOT. Sets UNIXNU (the checkout), DEVICE (the device repository), OS_PROFILE, UNIXNU_OUT_DIR, TREE.
# shellcheck shell=bash
. "$ROOT/kernel/pins.sh"
UNIXNU=$ROOT/external/Droid-VM-UniXNU
DEVICE=$ROOT/kernel/device
OS_PROFILE=ios-27.0.1-24A446
UNIXNU_OUT_DIR=${KERNEL_OUT:-$ROOT/out/kernel}/unixnu
CONFIG_NAME=${KERNEL_DEFCONFIG:-oneios_defconfig}
TREE=$UNIXNU_OUT_DIR/oneios/$OS_PROFILE/${CONFIG_NAME%_defconfig}/tree

stage_sources() {
  "$ROOT/kernel/fetch.sh"
  # The kernel needs three of UniXNU's submodules. Its driver would check out all of them on every run (and stops when a
  # submodule commit is not on its server yet, as AppleKeyStoreState's at this pin); kernel/patches/unixnu/0001 lets us
  # keep the ones checked out here.
  git -C "$UNIXNU" submodule --quiet update --init --force -- drivers/ArmCpu drivers/ArmDTPlatform kits/XnuPrelink
  # The hosted kernel's software AES archive (kernel/patches/oneios-hosted/aes) compiles NativeAES from this one.
  git -C "$UNIXNU" submodule --quiet update --init --force -- drivers/SoftwareAES
  # The hosted kernel's key store archive (kernel/patches/oneios-hosted/keystore) compiles NativeKeyStoreEvents.cpp
  # from this one (its pinned commit is on its server now: fetched by hash on 2026-11-11).
  git -C "$UNIXNU" submodule --quiet update --init --force -- drivers/AppleKeyStoreState
  export UNIXNU_SUBMODULES=keep
  local sm
  for sm in drivers/ArmCpu drivers/ArmDTPlatform kits/XnuPrelink; do
    [ -n "$(ls -A "$UNIXNU/$sm" 2>/dev/null)" ] || { echo "stage: submodule $sm is empty" >&2; return 1; }
  done
  # The OS profile lives in the UniXNU checkout (its driver reads profiles/os/<name> there). A copy, replaced each time.
  rm -rf "${UNIXNU:?}/profiles/os/$OS_PROFILE"
  cp -R "$ROOT/kernel/unixnu/profiles/os/$OS_PROFILE" "$UNIXNU/profiles/os/$OS_PROFILE"
  # Our changes to UniXNU's build scripts (kernel/patches/unixnu/*.patch), applied once to the pinned checkout.
  local p
  for p in "$ROOT"/kernel/patches/unixnu/*.patch; do
    [ -e "$p" ] || continue
    if git -C "$UNIXNU" apply --reverse --check "$p" 2>/dev/null; then continue; fi
    git -C "$UNIXNU" apply --check "$p" || { echo "stage: $p does not apply to UniXNU $(git -C "$UNIXNU" rev-parse --short HEAD)" >&2; return 1; }
    git -C "$UNIXNU" apply "$p"
    echo "stage: applied $(basename "$p") to UniXNU"
  done
}

# unixnu ARGS...: UniXNU's driver for the OneiOS device repository and output root.
unixnu() {
  # UNIXNU_DEVICE: the board's archive scripts find this repository through it (the fork's driver does not pass it on).
  ( cd "$UNIXNU" && UNIXNU_DEVICE=$DEVICE python3 ./unixnu "$@" -D "$DEVICE" --out "$UNIXNU_OUT_DIR" )
}
