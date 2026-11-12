#!/bin/bash
# Host test of the capture plug-in's device on a Mac (macOS 27: CoreMedia's derived objects and CMCapture's
# capture classes are there, as on iOS 27; macOS's BWFigCaptureDeviceVendor makes the device with our create
# function). Elsewhere it says so and passes. The plug-in's own lines go to stderr here.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if [ "$(uname -s)" != Darwin ] || [ ! -e /System/Library/PrivateFrameworks/CMCapture.framework ]; then
  echo "camera device (Mac): not a Mac with CMCapture: skipped"; exit 0
fi
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
clang -fobjc-arc -fblocks -O1 -Wall -Wextra -Werror -Wno-unused-parameter -fsanitize=address,undefined -I. -I../../drivers/camera -DONEIOS_CAMERA_SAY_STDERR \
  "-DONEIOS_CAMERA_NODE_PATH=getenv(\"ONEIOS_TEST_NODE\")" "-DONEIOS_CAMERA_SESSION_PLIST=getenv(\"ONEIOS_TEST_PLIST\")" \
  OneiOSCamera.c OneiOSCaptureDevice.c oneios_camera_reader.c tests/test_device_mac.m \
  -framework Foundation -framework CoreMedia -framework CoreVideo -o "$dir/test"
ONEIOS_TEST_NODE=$dir/node ONEIOS_TEST_PLIST=$dir/AVCaptureSession.plist "$dir/test"
