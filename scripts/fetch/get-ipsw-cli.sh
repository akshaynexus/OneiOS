#!/bin/sh
# Get the blacktop `ipsw` CLI (MIT) into out/bin/ipsw: it fetches AEA keys from Apple's key service and
# decrypts AEA disk images on Linux and macOS. The release archive is pinned by version and SHA-256 and
# checked before it is unpacked.
#   scripts/fetch/get-ipsw-cli.sh
# Another version: IPSW_VERSION=x.y.z IPSW_SHA256=<of the archive, from the release's checksums.txt>.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
V=${IPSW_VERSION:-3.1.743}
case "$(uname -s)/$(uname -m)" in
Linux/x86_64)  A=ipsw_${V}_linux_x86_64.tar.gz; PIN=56e9ba754781dfeaa3b84f733ad25e63ef8e2683214a321e06b31a0d85f95e4c ;;
Linux/aarch64) A=ipsw_${V}_linux_arm64.tar.gz;  PIN=c7976f40d648ede62bc2d8bb80b5e57ab6365d5ba531a917a3515466a753c21e ;;
Darwin/arm64)  A=ipsw_${V}_macOS_arm64.tar.gz;  PIN=e463a5d2ab984b4bb7eb072f69875498ba6ab3e67d1741d09c10adc4ab865a50 ;;
Darwin/x86_64) A=ipsw_${V}_macOS_x86_64.tar.gz; PIN=b47bb4b32dbd5650cfdde1b71ee9e8b5850c103403828bea848f88fded506dbb ;;
*) echo "get-ipsw-cli: no ipsw release for $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac
[ "$V" = 3.1.743 ] || PIN=
want=${IPSW_SHA256:-$PIN}
[ -n "$want" ] || { echo "get-ipsw-cli: version $V is not pinned here: set IPSW_SHA256" >&2; exit 1; }
T=$ROOT/out/ipsw-cli
mkdir -p "$ROOT/out/bin" "$T"
curl -fsSL -o "$T/$A" "https://github.com/blacktop/ipsw/releases/download/v$V/$A"
if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$T/$A" | cut -d' ' -f1); else got=$(shasum -a 256 "$T/$A" | cut -d' ' -f1); fi
[ "$got" = "$want" ] || { rm -f "$T/$A"; echo "get-ipsw-cli: SHA-256 of $A is $got, pinned $want" >&2; exit 1; }
tar -xzf "$T/$A" -C "$T"
find "$T" -type f -name ipsw -exec cp {} "$ROOT/out/bin/ipsw" \;
chmod +x "$ROOT/out/bin/ipsw"
rm -f "$T/$A"
"$ROOT/out/bin/ipsw" version
