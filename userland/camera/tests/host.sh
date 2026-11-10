#!/bin/bash
# Host tests of the capture plug-in (CI runs every executable */tests/host.sh): the reader it uses,
# run against stand-ins for /dev/oneios-camera, and a syntax check of the plug-in itself against
# stand-in headers (tests/stub-sdk: no iPhoneOS SDK is on the build machine, and none is in the
# repository). The plug-in is not built or run here.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
out=$(mktemp)
trap 'rm -f "$out"' EXIT
${CC:-cc} -std=c11 -D_DEFAULT_SOURCE -Wall -Wextra -Werror -fsanitize=address,undefined -I. -I../../drivers/camera \
  oneios_camera_reader.c tests/test_reader.c -o "$out"
"$out"
if command -v clang >/dev/null; then
  clang -std=c11 -fsyntax-only -Wall -Wextra -Werror -D_GNU_SOURCE -Dweak_import=weak -include tests/stub-sdk/darwin.h \
    -Itests/stub-sdk -I. -I../../drivers/camera OneiOSCamera.c
  echo "camera plug-in: syntax ok against the stand-in headers (not an iOS build)"
else
  echo "camera plug-in: no clang: syntax check skipped"
fi
