#!/bin/sh
# OneiOSRadeonGL's host test (the Galaxy A56's OpenGL ES, userland/metal-amd/gl; docs/a56-gpu.md 4a), on this PC:
# Mesa cecc0e9 + patches/mesa/5101 with GLES and softpipe (build.sh host, out/metal-amd-gl/host-build: about 4 minutes
# and 700 MB the first time, then only what changed), and tests/oneios_gl_test driving the GL layer as iOS 16.5 does:
# EAGL's GLI entries (gliGetVersion, gliChoosePixelFormat, gliCreateContextWithShared with two 1036-slot tables,
# gliSetInteger 0x38e/0x399, gliBindViewES, gliPresentViewES, gliDestroyContext) and every GL call through the dispatch
# table as CoreAnimation calls it, over stand-in IOSurfaces (host/). Exit 0 when every check passes.
#
#   userland/metal-amd/gl/host-test.sh                         build, then the checks twice: IOSurfaces in place
#                                                              (softpipe's own way) and copied (ONEIOS_GL_SURFACE=copy:
#                                                              the A56's way, every surface a resource of ours, uploaded
#                                                              on a new seed and written back after draws), and the
#                                                              no-screen gate
#   GL_HOST_BUILD=DIR userland/metal-amd/gl/host-test.sh       the checks against another build directory
#
# The checks (tests/oneios_gl_test.c): the 20F66 eagl_init code words (and 20H392's refused), the gate (bundle files,
# clients list, ONEIOS_GL, the preflight), EAGL's start, a render-target IOSurface drawn and read in memory after
# glFlush, an inverted one (rows turned over), a sampled one (a rectangle sampler in pixels) and its next frame after the
# "app" wrote it, a draw with no program (CA's wallpaper), a CAEAGLLayer drawable bound and presented twice, the self
# tests and the probe, the context's end with every IOSurface lock released.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
B=${GL_HOST_BUILD:-${METAL_AMD_GL_OUT:-$ROOT/out/metal-amd-gl}/host-build}
if [ -z "${GL_HOST_BUILD:-}" ]; then
  bash "$HERE/build.sh" host || { echo "gl-host-test: FAIL: build.sh host"; exit 1; }
fi
T=$B/src/gallium/targets/oneios-gl/oneios_gl_test
[ -x "$T" ] || { echo "gl-host-test: FAIL: no $T"; exit 1; }
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
fail=0
run() { # NAME ENV...
  name=$1; shift
  env "$@" "$T" > "$W/$name.out" 2>&1
  rc=$?
  grep -E '^oneios-gl-test: (FAIL|[0-9]+ passed)' "$W/$name.out" | sed "s/^oneios-gl-test:/gl-host-test: $name:/"
  if [ $rc != 0 ]; then fail=1; grep -E '^oneios-gl: [EW]' "$W/$name.out" | head -20; fi
}
run in-place ONEIOS_GL_SOFTPIPE=1
run copied ONEIOS_GL_SOFTPIPE=1 ONEIOS_GL_SURFACE=copy
if env ONEIOS_GL_SOFTPIPE=0 "$T" --no-screen > "$W/no-screen.out" 2>&1; then
  echo "gl-host-test: no-screen: pass (the gate closes and says why)"
else
  fail=1; echo "gl-host-test: no-screen: FAIL"; cat "$W/no-screen.out"
fi
[ $fail = 0 ] && echo "gl-host-test: all passed" || { echo "gl-host-test: FAIL"; exit 1; }
