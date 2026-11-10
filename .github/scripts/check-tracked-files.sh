#!/bin/bash
# No Apple firmware and no large binaries in git (README "Rules"). Same name list as runtime/package/pack.py.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
bad=0
while IFS= read -r -d '' f; do
  n=$(basename "$f" | tr '[:upper:]' '[:lower:]')
  case "$n" in
    kernelcache*|*cryptex*.dmg|*.ipsw|*.dmg|*.aea|*.img|*.simg|*.erofs|*.im4p|*.img4|*.im4m|*.kc|*.trustcache|buildmanifest.plist|restore.plist|*.shsh*|dyld_shared_cache*|*.macho)
      echo "tracked firmware-like file: $f" >&2; bad=1 ;;
  esac
  [ -f "$f" ] || continue
  if [ "$(stat -c %s "$f")" -gt 5242880 ]; then echo "tracked file over 5 MiB: $f" >&2; bad=1; fi
  case "$(od -An -tx1 -N4 "$f" | tr -d ' \n')" in
    cffaedfe|cefaedfe|cafebabe|cafebabf) echo "tracked Mach-O: $f" >&2; bad=1 ;;
  esac
done < <(git ls-files -z)
exit $bad
