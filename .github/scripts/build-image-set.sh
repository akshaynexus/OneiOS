#!/usr/bin/env bash
# Build and verify the pinned iOS image set, then stage only its age-encrypted archive for Actions.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"

PROFILE=profiles/ios-27.0.1-iPhone18,2.toml
BUILD=24A446
RUN_SUFFIX="ci-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-1}"
INPUTS=$ROOT/inputs/$RUN_SUFFIX
IMAGE_OUT=$ROOT/out/image-$RUN_SUFFIX
PRELINK_OUT=$ROOT/out/kernel/prelink-$RUN_SUFFIX
HOSTED_OUT=$RUN_SUFFIX
ARTIFACT_DIR=$IMAGE_OUT/artifact
AGE_RECIPIENT=${ONEIOS_AGE_RECIPIENT:?set the age public recipient for this build}
JOBS=${JOBS:-4}

command -v age >/dev/null || { echo "build-image-set: install age first" >&2; exit 1; }
command -v apfs-fuse >/dev/null || { echo "build-image-set: install apfs-fuse first" >&2; exit 1; }

# Fail on an invalid public key before downloading the 10.6 GiB of Apple restore members.
key_probe=$(mktemp)
plain_probe=$(mktemp)
trap 'rm -f "$key_probe" "$plain_probe"' EXIT
printf 'OneiOS image recipient check\n' > "$plain_probe"
age --encrypt --recipient "$AGE_RECIPIENT" --output "$key_probe" "$plain_probe"
test -s "$key_probe" || { echo "build-image-set: age did not accept the recipient" >&2; exit 1; }
rm -f "$key_probe" "$plain_probe"
trap - EXIT

. "$ROOT/scripts/lib/oneios-tool.sh"
oneios_tool "$ROOT"

# Fetch only source repositories pinned by this kernel and its prelink, plus the two hosted-kernel drivers.
kernel/fetch.sh
scripts/setup/get-references.sh NativeMobileDisplay NativeHidInput XnuPrelink

# Fetch, decrypt and verify Apple's required IPSW members, including its kernel inputs.
scripts/fetch/get-ipsw-cli.sh
INPUTS="$INPUTS" scripts/fetch/fetch.sh "$PROFILE"
INPUTS="$INPUTS" scripts/image/kernel-inputs.sh "$PROFILE"

# Build this checkout's hosted kernel and the reviewed kernel collection from the fetched iOS kernelcache.
HOSTED_OUT="$HOSTED_OUT" JOBS="$JOBS" XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh
KERNEL=$ROOT/out/kernel/current-hosted-$HOSTED_OUT/xnu.macho
REFERENCE=$ROOT/out/kernel/current-hosted-$HOSTED_OUT/reference.macho
KERNELCACHE=$ROOT/out/kernel-inputs/$BUILD/kernelcache.macho
OUT="$PRELINK_OUT" KERNEL="$KERNEL" REFERENCE="$REFERENCE" KERNELCACHE="$KERNELCACHE" \
  kernel/prelink/build.sh

# Run the repository's normal hook, pack the app-importable image set, and verify its complete archive.
INPUTS="$INPUTS" OUT="$IMAGE_OUT" scripts/image/build.sh "$PROFILE" --kernel "$PRELINK_OUT/kernel.kc"
. "$ROOT/scripts/lib/oneios-tool.sh"
oneios_tool "$ROOT"
eval "$("$ONEIOS_TOOL" image profile "$PROFILE" --shell)"
ARCHIVE=$IMAGE_OUT/$P_ARCHIVE
test -s "$ARCHIVE" && test -s "$ARCHIVE.sha256"
scripts/image/verify.sh "$ARCHIVE"
( cd "$IMAGE_OUT" && sha256sum -c "$P_ARCHIVE.sha256" )

# This is a public repository's Actions artifact, so never stage plaintext Apple-derived data.
mkdir -p "$ARTIFACT_DIR"
encrypted=$ARTIFACT_DIR/$P_ARCHIVE.age
age --encrypt --recipient "$AGE_RECIPIENT" --output "$encrypted" "$ARCHIVE"
cp "$ARCHIVE.sha256" "$ARTIFACT_DIR/"
( cd "$ARTIFACT_DIR" && sha256sum "$(basename "$encrypted")" > "$(basename "$encrypted").sha256" )
archive_sha=$(sha256sum "$ARCHIVE" | cut -d' ' -f1)
encrypted_sha=$(sha256sum "$encrypted" | cut -d' ' -f1)

cat > "$ARTIFACT_DIR/README.txt" <<EOF
OneiOS iOS 27.0.1 (24A446) image set — age-encrypted

Verify the downloaded encrypted file:
  sha256sum -c $P_ARCHIVE.age.sha256

Decrypt it with the age private identity file kept on your computer:
  age --decrypt --identity oneios-age-key.txt --output $P_ARCHIVE $P_ARCHIVE.age

Verify the decrypted image set before importing it:
  sha256sum -c $P_ARCHIVE.sha256

Windows PowerShell checksum checks:
  \$expected = ((Get-Content .\\$P_ARCHIVE.age.sha256 -Raw).Trim() -split '\\s+')[0]
  \$actual = (Get-FileHash -Algorithm SHA256 .\\$P_ARCHIVE.age).Hash.ToLowerInvariant()
  if (\$actual -ne \$expected) { throw 'Encrypted artifact checksum mismatch' }
Decrypt in PowerShell with:
  age.exe --decrypt --identity .\\oneios-age-key.txt --output .\\$P_ARCHIVE .\\$P_ARCHIVE.age
  \$expected = ((Get-Content .\\$P_ARCHIVE.sha256 -Raw).Trim() -split '\\s+')[0]
  \$actual = (Get-FileHash -Algorithm SHA256 .\\$P_ARCHIVE).Hash.ToLowerInvariant()
  if (\$actual -ne \$expected) { throw 'Decrypted image set checksum mismatch' }

The age private key was not sent to GitHub. The workflow artifact expires after one day.
EOF

cat > "$ARTIFACT_DIR/build.json" <<EOF
{
  "repository": "${GITHUB_REPOSITORY:-orumanayurpanjayat-ux/OneiOS}",
  "commit": "${GITHUB_SHA:-local}",
  "profile": "$PROFILE",
  "build": "$BUILD",
  "archive": "$P_ARCHIVE",
  "archive_sha256": "$archive_sha",
  "encrypted_archive": "$(basename "$encrypted")",
  "encrypted_archive_sha256": "$encrypted_sha",
  "encryption": "age",
  "artifact_retention_days": 1
}
EOF

echo "build-image-set: verified $P_ARCHIVE ($archive_sha)"
echo "build-image-set: staged only the encrypted archive ($encrypted_sha) in $ARTIFACT_DIR"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "## OneiOS image set built"
    echo
    echo "- Profile: \`$PROFILE\`"
    echo "- Image archive SHA-256: \`$archive_sha\`"
    echo "- Encrypted artifact file: \`$(basename "$encrypted")\`"
    echo "- Encrypted file SHA-256: \`$encrypted_sha\`"
    echo "- Retention: one day; decrypt with the age identity kept by the owner."
  } >> "$GITHUB_STEP_SUMMARY"
fi
