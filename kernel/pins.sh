# Pinned sources of the kernel build. Sourced by fetch.sh, build-linux.sh and build-macos.sh.
# One row per repository: NAME URL COMMIT BRANCH. A row changes only with a commit that says why.
# shellcheck shell=bash disable=SC2034

# The XNU source and its version fields (UniXNU profiles/os/ipados-27.0.1-24A446/profile.toml).
XNU_TAG=xnu-12377.121.6
XNU_COMMIT=ac9718fb1af618d5ce8678d0dc6e8a58f252216f
XNU_VERSION=12377.121.6
XNU_DARWIN=25.5.0
# macOS release that ships this XNU (darwin-xnu-build's MACOS_VERSION, as iOSPlus10 scripts/ios26/build-kernel.sh).
MACOS_VERSION=26.5
# Headers-only KDK (kdk-headers.py). Only read when KDK_HEADERS=1; UniXNU's series builds without it.
KDK_URL=https://github.com/dortania/KdkSupportPkg/releases/download/25F84/Kernel_Debug_Kit_26.5.2_build_25F84.dmg
KDK_SHA256=faef4393becf65c88a2791770904909babf1826d23d2ea478bf878a6417851ab
# Xcode on the Mac (the version UniXNU-droidvm records its 12377 arm64e build with).
XCODE_VERSION=27.1

# external/NAME
# Droid-VM-UniXNU is the build base: the fork of UniXNU that Droid-VM's iPadOS 27 guest is built with, the only
# published tree whose xnu-12377 arm64e series, with UniXNU-droidvm's board patches on top, has brought iPadOS 27.0.1
# up to SpringBoard. UniXNU-droidvm's patches apply on that fork's series and not on upstream's (2026-11-10: 10 of
# 24 fail on upstream 1446f44). UniXNU (upstream) is the reference the profile notes link to; nothing is built from it.
PINS_SOURCES='
UniXNU               https://github.com/UniXNU/UniXNU.git                 1446f449b6ac98b8a386752f520e402866ddbe99 main
Droid-VM-UniXNU      https://github.com/Droid-VM/UniXNU.git               62d0b11773ec17561fb409ed6e4c5adfa29ff45b wip/droidvm
UniXNU-droidvm       https://github.com/Droid-VM/UniXNU-droidvm.git       f9121cdc629d64cbd148980cdeef1f069c7f0068 wip/hackintosh
xnu-linux-toolchain  https://github.com/Allenwdk/xnu-linux-toolchain.git  909990e4ec2c6cda30c20cdb23219946918c7fd6 main
'
# out/toolchain/src/NAME: what xnu-linux-toolchain's bootstrap.sh builds. Its own clones are unpinned heads;
# linux/setup-toolchain.sh puts these commits there first. cctools-port: the ld64-956.6 branch UniXNU qualifies
# (scripts/linux/setup-toolchain.py: "its master, ld64-609, ..." corrupts the kernel link).
PINS_TOOLCHAIN='
mig           https://github.com/markmentovai/bootstrap_cmds.git  f337e600741638896fb6056ce87c6479f74b89a3 cross_platform
cctools-port  https://github.com/tpoechtrager/cctools-port.git    3e694ee7393af2a4ce8bd6e7dae28fe37450aa87 1030.6.3-ld64-956.6
lzfse         https://github.com/lzfse/lzfse.git                  e634ca58b4821d9f3d560cdc6df5dec02ffc93fd master
unifdef       https://github.com/fanf2/unifdef.git                08d12cd3059fe072929ea001ad9833f5487fd355 master
tcsh          https://github.com/tcsh-org/tcsh.git                b7db07931546178c4cb2542b5fc850c483628ed5 master
'

# pin_fetch DEST NAME URL COMMIT [BRANCH]: shallow fetch of one commit into DEST/NAME; an existing directory must be at the pin.
pin_fetch() {
  local dest=$1 name=$2 url=$3 commit=$4 branch=${5:-} d tmp head
  d=$dest/$name
  if [ -e "$d" ]; then
    head=$(git -C "$d" rev-parse HEAD 2>/dev/null || echo '?')
    if [ "$head" = "$commit" ]; then echo "$name: at the pin"; return 0; fi
    echo "$name: $d is at $head, the pin is $commit (remove the directory to fetch the pin)" >&2
    return 1
  fi
  tmp=$d.partial
  rm -rf "$tmp"; mkdir -p "$tmp"
  git -C "$tmp" init -q
  git -C "$tmp" remote add origin "$url"
  echo "$name: fetching $commit"
  if git -C "$tmp" fetch -q --depth 1 origin "$commit" 2>/dev/null; then
    git -C "$tmp" checkout -q --detach FETCH_HEAD
  else
    echo "$name: the server refused a fetch by commit; shallow fetch of ${branch:-the default branch}"
    git -C "$tmp" fetch -q --depth 200 origin ${branch:+"$branch"}
    git -C "$tmp" checkout -q --detach "$commit"
  fi
  head=$(git -C "$tmp" rev-parse HEAD)
  [ "$head" = "$commit" ] || { echo "$name: HEAD $head is not the pin $commit" >&2; return 1; }
  mv "$tmp" "$d"
  echo "$name: $d"
}

# pin_rows TABLE: the non-empty rows
pin_rows() { printf '%s\n' "$1" | sed '/^[[:space:]]*$/d'; }
