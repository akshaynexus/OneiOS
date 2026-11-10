#!/bin/bash
# CI: the pinned NDK (through the SDK's own manager) and the Rust target, for runtime/package.sh.
# The revision is the one runtime/package.sh checks.
set -euo pipefail
NDK_REVISION=28.2.13676358
SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:?no Android SDK on this runner}}
if [ ! -f "$SDK/ndk/$NDK_REVISION/source.properties" ]; then
  manager=$(ls "$SDK"/cmdline-tools/*/bin/sdkmanager | tail -1)
  # `yes` ends with SIGPIPE when the manager has read its answers: not a failure of the install.
  set +o pipefail
  yes 2>/dev/null | "$manager" --install "ndk;$NDK_REVISION" > /dev/null
  set -o pipefail
fi
grep -q "Pkg.Revision = $NDK_REVISION" "$SDK/ndk/$NDK_REVISION/source.properties"
echo "ANDROID_NDK_ROOT=$SDK/ndk/$NDK_REVISION" >> "$GITHUB_ENV"
rustup target add aarch64-linux-android
