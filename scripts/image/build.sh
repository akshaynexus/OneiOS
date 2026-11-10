#!/bin/bash
# Build the OneiOS image set of a profile from inputs/<build>/ (scripts/fetch/fetch.sh) into out/image/<build>/,
# and pack it as the .tar.zst the OneOS fork imports. Format: docs/image-format.md.
#
#   scripts/bg.sh image scripts/image/build.sh PROFILE          (hashes and packs about 16 GiB: run it in tmux)
#
# Options: --kernel FILE   the kernel collection to put in the set (default out/kernel/<build>/kernel.kc)
#          --no-kernel     build a set without one (the runtime cannot start it; for work on the images)
#          --hook FILE     the userland hook (default userland/image-hook, when it is there and executable)
#          --no-hook       do not run a hook: the seed is the OS volume's own /private/var
#          --seed tar|apfs the Data seed's format. tar (default) is made anywhere; apfs needs macOS
#          --no-pack       stop after the set folder; no .tar.zst
# Env: INPUTS (default inputs/<build>), OUT (default out/image).
#
# What it makes:
#   os.img, systemos.img, app.img   the decrypted OS, SystemOS and AppOS images, byte for byte (raw APFS
#                                   containers, read-only, mounted by the guest kernel's own APFS)
#   *.hashes                        the SHA-256 of each 1 MiB chunk of an image, for checks while it is read
#   trust/                          the three trust caches and root hashes, as shipped
#   data.seed                       what the guest's Data volume starts from: the OS volume's /private/var
#                                   after the userland hook, with owners and modes kept
#   overlay/                        files the userland hook adds for the runtime (this project's own)
#   kernel.kc                       the kernel collection (from the kernel build)
#   manifest.json                   every file's size and SHA-256
#
# Linux: nothing here needs root; the OS volume is read through apfs-fuse (FUSE, read-only).
# macOS: run with sudo; the OS volume is attached read-only with hdiutil. The macOS path is written
# from UniXNU's scripts and has not been run. Only --seed apfs needs macOS (newfs_apfs).
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
TOOL=$ROOT/scripts/image/imagetool.py
die() { echo "build: $*" >&2; exit 1; }
tool() { python3 -I "$TOOL" "$@"; }

kernel= no_kernel=0 hook= no_hook=0 seed=tar pack=1 profile=
while [ $# -gt 0 ]; do
  case "$1" in
  --kernel) kernel=${2:?--kernel FILE}; shift ;;
  --no-kernel) no_kernel=1 ;;
  --hook) hook=${2:?--hook FILE}; shift ;;
  --no-hook) no_hook=1 ;;
  --seed) seed=${2:?--seed tar|apfs}; shift ;;
  --no-pack) pack=0 ;;
  -*) die "unknown option $1" ;;
  *) [ -z "$profile" ] || die "one profile at a time"; profile=$1 ;;
  esac
  shift
done
[ -n "$profile" ] || die "usage: scripts/image/build.sh [--kernel FILE|--no-kernel] [--hook FILE|--no-hook] [--seed tar|apfs] [--no-pack] PROFILE"
[ -f "$profile" ] || die "no profile $profile"
case "$seed" in tar|apfs) ;; *) die "--seed is tar or apfs" ;; esac
OS=$(uname -s)

# ---- tools first: one line that names everything missing
missing=()
need() { command -v "$1" >/dev/null 2>&1 || missing+=("$1${2:+ ($2)}"); }
need python3 "3.11 or newer"
[ "$pack" = 0 ] || need zstd
case "$OS" in
Linux)
  need apfs-fuse "github.com/sgan81/apfs-fuse; reads the OS volume without root"
  command -v fusermount3 >/dev/null 2>&1 || command -v fusermount >/dev/null 2>&1 || missing+=("fusermount3 (fuse3)")
  [ "$seed" = tar ] || missing+=("macOS (--seed apfs makes an APFS volume; Linux builds --seed tar)") ;;
Darwin)
  need hdiutil
  [ "$seed" = tar ] || need newfs_apfs
  # an attached volume enforces the image's own permissions: only root reads all of /private/var
  [ "$(id -u)" = 0 ] || missing+=("root (on macOS run it with sudo)") ;;
*) missing+=("Linux or macOS (this is $OS)") ;;
esac
[ ${#missing[@]} = 0 ] || die "missing tools: $(IFS=';'; echo "${missing[*]}" | sed 's/;/; /g')"
python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' || die "missing tools: python3 3.11 or newer (this one is $(python3 -V 2>&1))"

eval "$(tool profile "$profile" --shell)"
IN=${INPUTS:-$ROOT/inputs/$P_BUILD}
OUT=${OUT:-$ROOT/out/image}
SET=$OUT/$P_BUILD
WORK=$OUT/.work-$P_BUILD

# ---- inputs: one line that names everything missing
absent=()
os_out= sys_out= app_out=
while IFS=$'\t' read -r role member name aea size out; do
  [ "$out" != - ] || continue
  [ -f "$IN/$name" ] || absent+=("$name")
  case "$role" in os) os_out=$out ;; systemos) sys_out=$out ;; app) app_out=$out ;; esac
done < <(tool profile "$profile" --members)
[ ${#absent[@]} = 0 ] || die "missing inputs in $IN: ${absent[*]} (run scripts/fetch/fetch.sh $profile)"

if [ "$no_kernel" = 0 ]; then
  kernel=${kernel:-$ROOT/out/kernel/$P_BUILD/kernel.kc}
  [ -f "$kernel" ] || die "missing kernel collection $kernel (build it under kernel/, or pass --kernel FILE or --no-kernel)"
fi
if [ "$no_hook" = 0 ]; then
  if [ -n "$hook" ]; then
    [ -x "$hook" ] || die "the hook $hook is not an executable file"
  elif [ -x "$ROOT/userland/image-hook" ]; then
    hook=$ROOT/userland/image-hook
  fi
fi

mounts=()
unmount() {
  case "$OS" in
  Linux) fusermount3 -u "$1" 2>/dev/null || fusermount -u "$1" 2>/dev/null || true ;;
  Darwin) hdiutil detach -quiet "$1" 2>/dev/null || hdiutil detach -quiet -force "$1" 2>/dev/null || true ;;
  esac
}
cleanup() { for m in ${mounts[@]+"${mounts[@]}"}; do unmount "$m"; done; }
trap cleanup EXIT
# mount_ro IMAGE DIR: a read-only view of an image's volume; prints the volume's root folder.
# It runs in $(...), so the caller adds DIR to mounts itself, before the call.
mount_ro() {
  mkdir -p "$2"
  case "$OS" in
  Linux) apfs-fuse "$1" "$2" >&2 || die "apfs-fuse cannot open $1"; echo "$2/root" ;;
  Darwin)
    hdiutil attach -readonly -nobrowse -noverify -noautoopen -owners on -imagekey diskimage-class=CRawDiskImage \
      -mountpoint "$2" "$1" >/dev/null || die "hdiutil cannot attach $1"
    echo "$2" ;;
  esac
}

rm -rf "$WORK"
mkdir -p "$WORK/set" "$WORK/mnt"
NEW=$WORK/set

# ---- the read-only images and the trust files: the inputs, unchanged
put() { # SOURCE NAME: hard link when the folder allows it, else copy
  mkdir -p "$(dirname "$NEW/$2")"
  ln "$1" "$NEW/$2" 2>/dev/null || cp "$1" "$NEW/$2"
}
while IFS=$'\t' read -r role member name aea size out; do
  [ "$out" != - ] || continue
  put "$IN/$name" "$out"
  case "$role" in os|systemos|app)
    tool apfs-info "$NEW/$out" >/dev/null || die "$name is not a raw APFS image (see the line above)" ;;
  esac
done < <(tool profile "$profile" --members)
[ "$no_kernel" = 1 ] || put "$kernel" "$P_KERNEL"

# ---- the Data seed: the OS volume's /private/var, then the userland hook
echo "reading $P_DATA_SOURCE of the OS volume"
mounts+=("$WORK/mnt/os")
os_root=$(mount_ro "$NEW/$os_out" "$WORK/mnt/os")
[ -d "$os_root/$P_DATA_SOURCE" ] || die "the OS volume has no /$P_DATA_SOURCE"
tool tree-copy "$os_root/$P_DATA_SOURCE" "$WORK/data" "$WORK/data.meta.json"
mkdir -p "$WORK/overlay"
: > "$WORK/owners.tsv"
tweaks=$WORK/tweaks.json
echo null > "$tweaks"
if [ -n "$hook" ]; then
  echo "running the userland hook $hook"
  mounts+=("$WORK/mnt/systemos" "$WORK/mnt/app")
  sys_root=$(mount_ro "$NEW/$sys_out" "$WORK/mnt/systemos")
  app_root=$(mount_ro "$NEW/$app_out" "$WORK/mnt/app")
  : > "$WORK/hook-info.json"
  ONEIOS_HOOK_API=1 ONEIOS_PROFILE=$(cd "$(dirname "$profile")" && pwd)/$(basename "$profile") \
  ONEIOS_BUILD=$P_BUILD ONEIOS_VERSION=$P_VERSION ONEIOS_DEVICE=$P_DEVICE ONEIOS_BOARD=$P_BOARD \
  ONEIOS_INPUTS=$IN ONEIOS_OS_ROOT=$os_root ONEIOS_SYSTEMOS_ROOT=$sys_root ONEIOS_APP_ROOT=$app_root \
  ONEIOS_DATA=$WORK/data ONEIOS_OVERLAY=$WORK/overlay ONEIOS_OWNERS=$WORK/owners.tsv \
  ONEIOS_INFO=$WORK/hook-info.json ONEIOS_EPOCH=$P_EPOCH ONEIOS_DATA_UUID=$P_DATA_UUID \
    "$hook" || die "the userland hook failed (exit $?); nothing was written to $SET"
  python3 -I - "$hook" "$WORK/hook-info.json" "$ROOT" > "$tweaks" <<'PY'
import hashlib, json, os, subprocess, sys
hook, info_path, root = sys.argv[1:4]
out = {"hook": os.path.relpath(os.path.abspath(hook), root), "hook_sha256": hashlib.sha256(open(hook, "rb").read()).hexdigest()}
try:
    out["commit"] = subprocess.check_output(["git", "-C", root, "rev-parse", "HEAD"], text=True, stderr=subprocess.DEVNULL).strip()
    out["dirty"] = bool(subprocess.check_output(["git", "-C", root, "status", "--porcelain", "--", "userland"], text=True).strip())
except (OSError, subprocess.CalledProcessError):
    pass
text = open(info_path).read().strip()
if text:
    out["info"] = json.loads(text)
print(json.dumps(out))
PY
  unmount "$WORK/mnt/systemos"; unmount "$WORK/mnt/app"
else
  echo "no userland hook: the seed is the OS volume's own /$P_DATA_SOURCE"
fi
unmount "$WORK/mnt/os"; mounts=()

tool seed-pack "$WORK/data" "$WORK/data.meta.json" "$WORK/data.seed.tar" --epoch "$P_EPOCH" --owners "$WORK/owners.tsv" > "$WORK/seed.json"
if [ "$seed" = apfs ]; then
  "$ROOT/scripts/image/seed-apfs-macos.sh" "$WORK/data.seed.tar" "$NEW/$P_SEED" "$P_DATA_UUID" "$profile"
  python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); d["format"]="apfs-raw"; json.dump(d,open(sys.argv[1],"w"))' "$WORK/seed.json"
else
  mv "$WORK/data.seed.tar" "$NEW/$P_SEED"
fi
if [ -n "$(find "$WORK/overlay" -mindepth 1 -print -quit)" ]; then
  [ -z "$(find "$WORK/overlay" -mindepth 1 ! -type f ! -type d -print -quit)" ] || die "the hook put something other than files and folders in the overlay"
  cp -R "$WORK/overlay" "$NEW/overlay"
fi

# ---- manifest, check, replace the old set, pack
margs=(--seed-info "$WORK/seed.json" --tweaks "$tweaks")
[ ! -f "$IN/inputs.json" ] || margs+=(--inputs "$IN/inputs.json")
tool manifest "$profile" "$NEW" "${margs[@]}"
tool verify "$NEW"
rm -rf "$SET"
mv "$NEW" "$SET"
chmod -R u+w "$WORK"; rm -rf "$WORK"
echo "image set: $SET"
if [ "$pack" = 1 ]; then
  tool pack "$SET" "$OUT/$P_ARCHIVE"
  tool verify "$OUT/$P_ARCHIVE"
  echo "archive: $OUT/$P_ARCHIVE (+ .sha256). Import it in the OneOS fork's app; never upload it."
fi
