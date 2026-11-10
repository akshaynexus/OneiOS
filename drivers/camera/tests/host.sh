#!/bin/bash
# Host test of the camera driver's pure parts (CI runs every executable */tests/host.sh). The kernel
# sources themselves are compiled by drivers/tests/host.sh (drivers/build.py --check) where a kernel
# build is on the machine.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
out=$(mktemp)
trap 'rm -f "$out"' EXIT
${CC:-cc} -std=c11 -Wall -Wextra -Werror -fsanitize=address,undefined -I. tests/test_camera_dev.c -o "$out"
"$out"
