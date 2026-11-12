#!/bin/sh
# Fetch the reference repos of docs/references.md into external/ (git-ignored; nothing is built from this script's
# clones unless an owner's own script says so, and the kernel's four sources stay in kernel/pins.sh).
# Usage: get-references.sh            fetch every missing repo of the main table
#        get-references.sh -a         the main table and the extra one
#        get-references.sh NAME...    fetch the named repos (either table)
#        get-references.sh -l         list both tables and the on-disk state
# A directory that already exists is never changed (-l says "differs" when it is not at the pin).
# REF_DIR=... puts the clones in another folder. Each fetch is a shallow fetch of the pinned commit; if the
# server refuses it, a shallow fetch of the branch. Source only: no firmware, no Apple files, no release assets.
set -eu
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel 2>/dev/null || (cd "$(dirname "$0")/../.." && pwd))
DEST=${REF_DIR:-$REPO/external}

# NAME URL COMMIT BRANCH
# Main table: what the team builds on or reads while building (docs/references.md, "Use this for ...").
# virglrenderer     drm2kgsl: the msm-protocol server on /dev/kgsl-3d0 (src/drm/drm2kgsl/, MIT). GPU host side.
# crosvm            the device side of every virtio device the guest's drivers speak (devices/src/virtio/). Reference.
# NativeAdreno      the guest kernel's GPU user client, execbuf, memory and DroidVMVirtgpuABI.h, at the commit the
#                   kernel base (Droid-VM-UniXNU 62d0b11) has as drivers/Adreno740.
# AdrenoPM4, ArmSmmu4K  the two kits NativeAdreno's sources include (PM4 encoder, SMMU page-table types), at the kernel
#                   base's kits/ submodule commits: for the compile check of kernel/patches/oneios-hosted/gpu.
# NativeMetal       the Metal plug-in (frontend, metal2vulkan bridge, Turnip glue), at the kernel base's drivers/AdrenoMetal.
# metal2vulkan, mesa-droidvm  the Metal plug-in's translator and Turnip's source, at the pins of the kernel base's
#                   scripts/data/gpu-metal-vulkan-inputs.json and adreno-compiler-inputs.json. userland/metal/build.sh
#                   patches copies of them (patches/metal2vulkan, patches/mesa of Droid-VM-UniXNU); these stay as fetched.
# NativeMobileDisplay  the IOMobileFramebuffer backend (GopFramebuffer fork), at the kernel base's drivers/GopFramebuffer.
# NativeHidInput    UniXNU/ShengHidInput at the commit UniXNU-droidvm's ipados27 build-droidvm-hid-input.py pins (its
#                   arm64e source): touch, buttons, keyboard HID. kernel/patches/oneios-hosted/hid compiles it.
# IOHIDFamily       Apple's source of the family at the OS profile's tag (IOHIDFamily-2238.120.5): the public
#                   IOHIDDevice.h that kernel/patches/oneios-hosted/hid/OriginalHidDeviceABI.h is reduced from. Read only.
# ownervisor, applebox  the owner debug shell and tools of the guest (UniXNU-droidvm's feeds.toml pin; applebox head).
# droidvm-meta-repo PROGRESS.md, plans/ (IOS_DRM2KGSL_MIGRATION, IOS_VIRTIO_GPU_SURVEY, IPADOS27_BUMP), todo/: the
#                   record of iPadOS 27 with Metal on an SM8650 / Adreno 750 phone.
# macOS-DroidVM     docs/ (TODO-gpu, xnu-12377-port, reims-vgpu-vtpv-contract) and compat/ (Linux toolchain layer).
# gunyah_host_mod   udmabuf for GKI 6.1 (drm2kgsl needs /dev/udmabuf) and the Gunyah host modules (docs/plan.md 8).
# XnuPrelink        kernel/prelink/build.sh's matcher and fileset emitter (it prefers external/XnuPrelink to the kernel
#                   base's kits/XnuPrelink submodule at b74ed7c). Needed: from 2b7baca it retargets the `<stub>.original`
#                   branches of the hosted kernel's archives (native_original_branches); b74ed7c leaves all 507 of them
#                   pointing past the collection and the kernel panics at the GPU's start (phone, 2026-11-12).
TABLE='
virglrenderer        https://github.com/Droid-VM/virglrenderer.git        7e17c8e814e963912fc1e4e737137a31b5efa027 wip/hackintosh
crosvm               https://github.com/Droid-VM/crosvm.git               f82707d2cbd8d1666cd11428157ddb45c4d61282 wip/hackintosh
NativeAdreno         https://github.com/Droid-VM/NativeAdreno.git         627836d507ef08037fff2f9b12e9274ba7b2a4f3 wip/hackintosh
AdrenoPM4            https://github.com/UniXNU/AdrenoPM4.git              282606b528b2595bc4274ea7490b62b323f35a5f main
ArmSmmu4K            https://github.com/UniXNU/ArmSmmu4K.git              83969cd874503e65997ff7639f7dcf3d0a92cd17 main
NativeMetal          https://github.com/Droid-VM/NativeMetal.git          b0d6a07a3da3e23486593aff235904177e94a3ff wip/hackintosh
metal2vulkan         https://github.com/steelbrain/metal2vulkan.git       624bab3918de5b978e1101517abca2b4fa426565 development
mesa-droidvm         https://github.com/Droid-VM/mesa.git                 cecc0e96d7193c72356a31abb0cc589e4fc0415e droidvm
NativeMobileDisplay  https://github.com/Droid-VM/NativeMobileDisplay.git  ed08bd437b93772d4c41d74633af12446918f18a wip/hackintosh
NativeHidInput       https://github.com/UniXNU/ShengHidInput.git          24f1f3fbed6feff42e1ca02e2ce386a5c3c36da8 main
IOHIDFamily          https://github.com/apple-oss-distributions/IOHIDFamily.git e31e90a24c27ad6cbe981650d12a1a4124065b03 main
ownervisor           https://github.com/Droid-VM/ownervisor.git           65fc2c7037f494a238f4263f528a68ff21e87406 main
applebox             https://github.com/Droid-VM/applebox.git             6e3b47149e0e460636e7a95bba9d48e12170a925 main
droidvm-meta-repo    https://github.com/Droid-VM/droidvm-meta-repo.git    a18ec72392721e34fbe98216a8be1ee3acc41ddc wip/hackintosh
macOS-DroidVM        https://github.com/Droid-VM/macOS-DroidVM.git        77f761485bfc2c2e379ee49eb5c6a25c19b98739 main
gunyah_host_mod      https://github.com/Droid-VM/gunyah_host_mod.git      44e8ae5a088a3a35290a5cacb205640cff187b99 droidvm
XnuPrelink           https://github.com/UniXNU/XnuPrelink.git             9b950f2e104da77f494e8f2735c6f044f0e4db4d main
'
# Extra table (-a or by name): read-only references, nothing to build on.
# UniXNU-shennong   Xiaomi 14 Pro (SM8650): the only public iPhone iOS 27.0.1 (24A446, iPhone17,1) OS profile. No licence.
# UniXNU-houji      Xiaomi 14 (SM8650): bare-metal Adreno 750 command submission on iOS 16. Mixed licences.
# hakios-sm8650     HakiOS device layer for the Xiaomi 14 Pro: the SM8650 facts (soc/). No licence.
# UniXNU-sheng      package/WifiNet.cpp: the Apple80211 request layouts DroidVMWifi.inc follows.
# tahoe-on-linux    macOS 26 userland on Linux with the XNU personality in user space (x86_64): trap-layer design notes.
# vphone-linux      reims-vgpu + metal2vulkan on Linux: the fallback GPU path of docs/plan.md 5.5.
# Portsmith-iOS     the catalogue of every UniXNU / HakiOS / ios-sweet device port (ports/catalog/unixnu-devices.toml).
EXTRA='
UniXNU-shennong      https://github.com/YX2748198202-lab/UniXNU-shennong.git 363cc770793ca998a70801b70798e17aadb64261 main
UniXNU-houji         https://github.com/lonzzi/UniXNU-houji.git           99beb109d66d24d3a1780901cb8958ec3fb24b06 main
hakios-sm8650        https://github.com/Fearless109/hakios-sm8650.git     ea803679f5a8a0a72c454b5e0bf32cfb3d983107 main
UniXNU-sheng         https://github.com/UniXNU/UniXNU-sheng.git           6bc0ce1e8c3a3733110a80dd80ddfb2a1ca46555 main
tahoe-on-linux       https://github.com/ediwiuwj/tahoe-on-linux.git       2595c97a3a1ff97dc31cc7c4068b1d95eb627b44 main
vphone-linux         https://github.com/YahiaAngelo/vphone-linux.git      9c75e88d4b379ae5018dc4e0e9d8c71a90538567 main
Portsmith-iOS        https://github.com/Chromitsune/Portsmith-iOS.git     8794f29410822d2c258ad6abad57b26150a99fed main
'

rows() { printf '%s\n' "$1" | sed '/^[[:space:]]*$/d'; }

list() { # TABLE LABEL
  rows "$1" | while read -r name url commit branch; do
    d=$DEST/$name
    if [ -e "$d" ]; then
      head=$(git -C "$d" rev-parse HEAD 2>/dev/null || echo '?')
      [ "$head" = "$commit" ] && pin=match || pin=differs
      printf '%-20s %-6s %-8s %-8s %s\n' "$name" "$2" present "$pin" "$head"
    else
      printf '%-20s %-6s %-8s %-8s %s\n' "$name" "$2" absent - "(pin $commit, branch $branch)"
    fi
  done
}

fetch() { # NAME URL COMMIT BRANCH
  name=$1 url=$2 commit=$3 branch=${4:-}
  d=$DEST/$name
  if [ -e "$d" ]; then echo "$name: $d exists, not changed"; return 0; fi
  tmp=$d.partial
  rm -rf "$tmp"
  mkdir -p "$tmp"
  git -C "$tmp" init -q
  git -C "$tmp" remote add origin "$url"
  echo "$name: fetching $commit from $url"
  if git -C "$tmp" fetch -q --depth 1 origin "$commit" 2>/dev/null; then
    git -C "$tmp" checkout -q --detach FETCH_HEAD
  else
    echo "$name: commit fetch refused, shallow fetch of ${branch:-the default branch}"
    git -C "$tmp" fetch -q --depth 200 origin ${branch:+"$branch"}
    git -C "$tmp" checkout -q --detach "$commit"
  fi
  head=$(git -C "$tmp" rev-parse HEAD)
  if [ "$head" != "$commit" ]; then echo "$name: HEAD $head is not the pin $commit" >&2; rm -rf "$tmp"; return 1; fi
  mv "$tmp" "$d"
  echo "$name: $d at $commit"
}

fetch_table() { # TABLE
  while read -r name url commit branch; do
    fetch "$name" "$url" "$commit" "$branch" </dev/null || fail=1
  done <<ROWS
$(rows "$1")
ROWS
}

if [ "${1:-}" = -l ]; then
  printf '%-20s %-6s %-8s %-8s %s\n' NAME TABLE STATE PIN HEAD
  list "$TABLE" main
  list "$EXTRA" extra
  exit 0
fi
mkdir -p "$DEST"
fail=0
if [ $# -eq 0 ]; then
  fetch_table "$TABLE"
elif [ "$1" = -a ]; then
  fetch_table "$TABLE"
  fetch_table "$EXTRA"
else
  for want in "$@"; do
    row=$(rows "$TABLE$EXTRA" | awk -v n="$want" '$1 == n')
    if [ -z "$row" ]; then echo "$want: not in the tables (see -l)" >&2; fail=1; continue; fi
    # shellcheck disable=SC2086
    fetch $row || fail=1
  done
fi
exit $fail
