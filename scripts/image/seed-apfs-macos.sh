#!/bin/bash
# macOS only, as root: turn the packed Data seed (the tar of build.sh) into an APFS volume image, so that the
# guest starts from a ready Data volume instead of making one on its first start.
#
#   sudo scripts/image/seed-apfs-macos.sh SEED.tar OUT.img VOLUME_UUID PROFILE
#
# build.sh --seed apfs calls it. The volume is case-sensitive with expanded records (newfs_apfs -e -x), as an
# iOS 27 device's Data volume is and as UniXNU makes its seed (UniXNU-droidvm build-droidvm-data-seed.py),
# and has the UUID the profile pins. WRITTEN FROM THOSE SCRIPTS, NOT RUN: the Mac session of 2026-11-12 had no sudo
# (the default tar seed needs none). Size: SEED_MIB (default: the tar's size plus a half, at least 256).
set -euo pipefail
die() { echo "seed-apfs: $*" >&2; exit 1; }
[ "$(uname -s)" = Darwin ] || die "needs macOS (newfs_apfs); on Linux build with --seed tar"
[ "$(id -u)" = 0 ] || die "run with sudo (owners are written as they are in the seed)"
[ $# = 4 ] || die "usage: seed-apfs-macos.sh SEED.tar OUT.img VOLUME_UUID PROFILE"
tar_in=$1 out=$2 vol_uuid=$3
name=$(python3 -I "$(dirname "$0")/imagetool.py" profile "$4" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["data"].get("volume_name","Data"))')
[ ! -e "$out" ] || die "refusing to overwrite $out"
bytes=$(stat -f %z "$tar_in")
mib=${SEED_MIB:-$(( bytes * 3 / 2 / 1048576 + 1 ))}
[ "$mib" -ge 256 ] || mib=256
mnt=$(mktemp -d /tmp/oneios-seed.XXXXXX)
dev=
cleanup() {
  [ -z "$dev" ] || { umount "$mnt" 2>/dev/null || true; hdiutil detach -quiet -force "$dev" 2>/dev/null || true; }
  rmdir "$mnt" 2>/dev/null || true
}
trap cleanup EXIT
mkfile -n "${mib}m" "$out"
dev=$(hdiutil attach -nomount -imagekey diskimage-class=CRawDiskImage "$out" | awk 'NR==1 {print $1}')
[ -n "$dev" ] || die "hdiutil did not attach $out"
newfs_apfs -e -x -U "$vol_uuid" -v "$name" "$dev"
vol=$(diskutil list "$dev" | awk -v n="$name" '$0 ~ n {print $NF}' | tail -1)
[ -n "$vol" ] || die "no APFS volume on $dev after newfs_apfs"
mount -t apfs -o owners "/dev/$vol" "$mnt"
# bsdtar restores numeric owners, modes, times and the LIBARCHIVE.xattr records of the seed
tar -xpf "$tar_in" --numeric-owner -C "$mnt"
sync
umount "$mnt"
hdiutil detach -quiet "$dev"; dev=
[ "$(dd if="$out" bs=1 skip=32 count=4 2>/dev/null)" = NXSB ] || die "$out is not an APFS container"
ls -l "$out"
