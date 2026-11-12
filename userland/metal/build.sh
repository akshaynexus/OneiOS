#!/bin/bash
# Build the Metal plug-in, NativeAdrenoMetal.bundle, for iOS 27 arm64e on macOS (Xcode) or Linux, for this guest's shared cache.
#
#   scripts/bg.sh metal userland/metal/build.sh            everything (about 25 minutes the first time)
#   userland/metal/build.sh stage|turnip|plugin|finish|addon   one step
#
# What is built (docs/gpu.md section 7, docs/userland.md 7.1):
#   libvulkan_freedreno.dylib   Mesa's Turnip (external/mesa-droidvm cecc0e9 + Droid-VM-UniXNU patches/mesa, the list of
#                               scripts/data/adreno-compiler-inputs.json) with the darwin virtio-gpu backend, a Mach-O dylib
#   libadreno_metal_bridge.a    metal2vulkan (external/metal2vulkan 624bab3 + NativeMetal bridge/patches + Droid-VM-UniXNU
#                               patches/metal2vulkan) behind NativeMetal's C ABI; Rust, arm64e-apple-ios, std from source
#   NativeAdrenoMetal           external/NativeMetal b0d6a07: the plug-in and the Metal frontend, linked with the bridge
#                               (GpuScanoutPresenter.h includes the wire header of external/NativeMobileDisplay)
#   -> out/userland/NativeAdrenoMetal.bundle (Info.plist, NativeAdrenoMetal, libvulkan_freedreno.dylib, oneios-target)
#      out/userland/NativeAdrenoMetal.gpuaddon: the bundle as the kernel.kc.gpuaddon sidecar (build.sh addon makes it alone)
#      out/userland/NativeAdrenoMetal.build.json: the SDK and stubs used, the pins, sizes and CDHashes
#
# How: the kernel base's own build scripts (Droid-VM-UniXNU scripts/build-turnip-darwin.py, build-adreno-metal-bridge.py,
# build-adreno-metal-plugin.py, --target ios27) run from a staged copy of its tree (out/metal/root), with this folder's
# shim/ in front of PATH: xcrun's clang is scripts/ios/ios-clang (so every link is against the guest's stubs), codesign
# is scripts/ios/ios-sign.py (byte for byte Apple's codesign for a thin file; no bundle seal), strip and nm the linker's
# siblings. On macOS ios-clang is Xcode's clang and ld, and xcrun's other answers are Xcode's; on Linux the cross toolchain.
# The staged copies differ from upstream in the few lines `stage` prints. Before the compile, the plug-in's shared
# cache addresses are replaced with this guest's (hooks/<product>-<build>.json); `finish` then marks the bundle with
# `oneios-target`, which is what lets userland/roottweak place it.
#
# Needs: scripts/setup/ios-toolchain.sh says "ok"; scripts/setup/get-references.sh (NativeMetal, NativeAdreno,
# NativeMobileDisplay, AdrenoPM4, metal2vulkan, mesa-droidvm, Droid-VM-UniXNU); meson, ninja, glslang, glslc, spirv-val, bison, flex, llvm-as,
# llvm-objcopy, llvm-ar, python3 with mako and yaml; rustup with 1.98.0 and its rust-src
# (rustup toolchain install 1.98.0 && rustup +1.98.0 component add rust-src).
# macOS: brew install ninja glslang shaderc spirv-tools bison llvm (LLVM's tools are found in `brew --prefix llvm`/bin,
# Homebrew's bison is put before /usr/bin's 2.3); uv pip install --python "$(command -v python3)" meson mako pyyaml.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
HERE=$ROOT/userland/metal
. "$ROOT/scripts/ios/env.sh"
TARGET_ID=${ONEIOS_METAL_TARGET:-iPhone18,2 24A446}
HOOKS=$HERE/hooks/${TARGET_ID/ /-}.json
W=${METAL_WORK:-$ROOT/out/metal}
R=$W/root
EXT=$ROOT/external
BASE=$EXT/Droid-VM-UniXNU
OUT=${METAL_OUT:-$ROOT/out/userland}
JOBS=${JOBS:-$(getconf _NPROCESSORS_ONLN)}
export PATH="$HERE/shim:$PATH"
AR=llvm-ar
if [ "$IOS_HOST" = Darwin ]; then
  # Homebrew's LLVM is keg-only: its llvm-ar here, its llvm-as and llvm-objcopy found by the base's scripts themselves
  LLVM_BIN=${LLVM_BIN:-$(brew --prefix llvm 2>/dev/null || echo /opt/homebrew/opt/llvm)/bin}
  if [ -x "$LLVM_BIN/llvm-ar" ]; then AR=$LLVM_BIN/llvm-ar; else AR=$(/usr/bin/xcrun --sdk iphoneos --find ar); fi
fi
# cc-rs (the Rust crates' C and assembly) and rustc's own probes: the same compiler and SDK. On macOS SDKROOT stays
# unset: the crates' build scripts are linked for the Mac with its SDK, and rustc asks xcrun (the shim) for iOS's.
[ "$IOS_HOST" = Darwin ] || export SDKROOT=$IOS_SDK
export CC_arm64e_apple_ios=$IOS_DIR/ios-clang CXX_arm64e_apple_ios=$IOS_DIR/ios-clang++
export AR_arm64e_apple_ios=$AR CARGO_TARGET_ARM64E_APPLE_IOS_LINKER=$IOS_DIR/ios-clang

say() { echo "metal: $*"; }

stage() {
  [ -f "$HOOKS" ] || { echo "metal: no $HOOKS: the plug-in's shared cache addresses are not known for $TARGET_ID" >&2; exit 1; }
  for d in Droid-VM-UniXNU NativeMetal NativeAdreno NativeMobileDisplay AdrenoPM4 metal2vulkan mesa-droidvm; do
    [ -d "$EXT/$d" ] || { echo "metal: no external/$d (scripts/setup/get-references.sh $d)" >&2; exit 1; }
  done
  mkdir -p "$R/drivers" "$R/kits" "$R/.cache/gpu-reference" "$R/.cache/mesa-venv/bin" "$R/.cache/mesa-tools/bin" "$R/build"
  rsync -a --delete "$BASE/scripts/" "$R/scripts/"
  rsync -a --delete "$BASE/patches/" "$R/patches/"
  rsync -a --delete --exclude .git "$EXT/NativeMetal/" "$R/drivers/AdrenoMetal/"
  rsync -a --delete --exclude .git "$EXT/NativeAdreno/" "$R/drivers/Adreno740/"
  rsync -a --delete --exclude .git "$EXT/NativeMobileDisplay/" "$R/drivers/GopFramebuffer/"
  rsync -a --delete --exclude .git "$EXT/AdrenoPM4/" "$R/kits/AdrenoPM4/"
  ln -sf "$(command -v meson)" "$R/.cache/mesa-venv/bin/meson"
  # macOS's own bison is 2.3: Homebrew's goes first (the base's scripts put .cache/mesa-tools/bin first in PATH)
  if [ "$IOS_HOST" = Darwin ] && [ -x "$(brew --prefix bison 2>/dev/null)/bin/bison" ]; then
    ln -sf "$(brew --prefix bison)/bin/bison" "$R/.cache/mesa-tools/bin/bison"
  fi
  if [ ! -d "$R/.cache/mesa-work/.git" ]; then
    git clone -q "$EXT/mesa-droidvm" "$R/.cache/mesa-work"
    python3 - "$R" <<'EOF'
import json, subprocess, sys
root = sys.argv[1]
for patch in json.load(open(root + '/scripts/data/adreno-compiler-inputs.json'))['patches']:
    subprocess.run(['git', '-C', root + '/.cache/mesa-work', 'apply', root + '/' + patch], check=True)
EOF
    say "Mesa $(git -C "$R/.cache/mesa-work" rev-parse --short HEAD) with the recorded patches in $R/.cache/mesa-work"
  fi
  [ -d "$R/.cache/gpu-reference/metal2vulkan/.git" ] || git clone -q "$EXT/metal2vulkan" "$R/.cache/gpu-reference/metal2vulkan"
  # The staged scripts and sources: what differs from upstream, each said.
  # clang 23's -Wunused-but-set-global: Xcode 27.2's clang 21 does not know it (and -Werror makes an unknown
  # warning option an error), so the plug-in script is told about it only when the compiler knows it
  local knows=0
  echo 'int x;' | "$IOS_DIR/ios-clang" -Werror -Wno-error=unused-but-set-global -x c -c - -o /dev/null 2>/dev/null && knows=1
  python3 - "$R" "$HOOKS" "$JOBS" "$knows" <<'EOF'
import json, re, sys
from pathlib import Path
root, hooks, jobs, knows = Path(sys.argv[1]), json.load(open(sys.argv[2])), sys.argv[3], sys.argv[4] == '1'

def edit(path, pairs):
    text = (root / path).read_text()
    for old, new, why in pairs:
        if text.count(old) != 1:
            sys.exit(f'metal: {path} no longer has exactly one {old!r}')
        text = text.replace(old, new)
        print(f'metal: staged {path}: {why}')
    (root / path).write_text(text)

edit('scripts/build-turnip-darwin.py', [
    ("env.update(CC=find('clang'), CXX=find('clang++'), CFLAGS='', CXXFLAGS='', CPPFLAGS='', LDFLAGS='')",
     "env.update(CC='clang', CXX='clang++', CFLAGS='', CXXFLAGS='', CPPFLAGS='', LDFLAGS='')",
     "meson's build-machine compiler is the host's clang (on a Mac xcrun's clang is both)"),
    ("'-j6'", "'-j%s'" % jobs, 'ninja jobs'),
])
edit('scripts/build-adreno-metal-bridge.py', [
    ('timeout=1800)', 'timeout=10800)', 'a longer limit for the fat-LTO Rust build'),
])
edit('scripts/build-adreno-metal-plugin.py', [
    ("subprocess.run(['python3', ROOT / 'scripts/gen-turnip-replay.py'], check=True)\n", '',
     "no TurnipReplay.h: it is recorded on a Mac's host simulation, and NativeAdrenoUserTest.h includes it only when it exists"),
] + ([
    ("'-Wall', '-Wextra', '-Werror',", "'-Wall', '-Wextra', '-Werror', '-Wno-error=unused-but-set-global',",
     "clang 23's -Wunused-but-set-global (gZeroRest), which Xcode's clang does not have, is not an error"),
] if knows else []))
# The frontend at b0d6a07 has the caller-by-framework answer of supportsFamily: for RenderBox and WebGPU; the base's
# plug-in script inserts its own, newer one (ANGLE too) before installAnswers and the two collide. The frontend's
# goes, the script's is inserted.
frontend = root / 'drivers/AdrenoMetal/NativeBridgeMetalFrontend.mm'
text = frontend.read_text()
begin, end = 'static BOOL (*gFamilyAnswer)(id,SEL,NSInteger);\n', 'static void installAnswers(Class cls) {\n'
if text.count(begin) != 1 or text.count(end) != 1 or text.index(begin) > text.index(end):
    sys.exit('metal: NativeBridgeMetalFrontend.mm: the supportsFamily: caller block is not where it was')
frontend.write_text(text[:text.index(begin)] + text[text.index(end):])
print("metal: staged NativeBridgeMetalFrontend.mm: its supportsFamily: caller block removed; build-adreno-metal-plugin.py inserts the base's (RenderBox, WebGPU, ANGLE)")
# The plug-in's shared cache addresses: upstream's are the iPad17,3 24A446 cache's.
plugin = root / 'drivers/AdrenoMetal/NativeAdrenoMetalPlugin.mm'
text = plugin.read_text()
anchor, branch = hooks['anchor'], hooks['branch']
for item in [anchor] + hooks['replace']:
    if not re.search(re.escape(item['upstream']) + r'(?![0-9a-fA-F])', text):
        sys.exit(f'metal: NativeAdrenoMetalPlugin.mm has no {item["upstream"]}: the hooks file is for another revision')
    text, n = re.subn(re.escape(item['upstream']) + r'(?![0-9a-fA-F])', item['value'], text)
    if item['upstream'] != item['value']:
        print(f'metal: staged NativeAdrenoMetalPlugin.mm: {item["upstream"]} -> {item["value"]} ({n} places): {item.get("what", item.get("symbol"))}')
for fixed in (branch['address'], f'word != {branch["word"]}'):
    if fixed not in text:
        sys.exit(f'metal: NativeAdrenoMetalPlugin.mm has no {fixed!r}')
# Every address the arm64e code uses must now be one the hooks file names.
known = {i['value'] for i in hooks['replace']} | {i['value'] for i in hooks['same']} | {anchor['value']}
arm64e_only = text[text.index('setenv("CA_DISABLE_FRAMEBUFFER_COMPRESSION"'):]
arm64e_only = arm64e_only[:arm64e_only.index('#endif')] + text[text.index('static unsigned patchFramebufferBranch'):text.index('static unsigned gReports')]
unknown = sorted(set(re.findall(r'0x1[0-9a-f]{8}\b', arm64e_only)) - known)
if unknown:
    sys.exit('metal: shared cache addresses in the arm64e code that the hooks file does not name: ' + ', '.join(unknown))
plugin.write_text(text)
EOF
}

turnip() {
  (cd "$R" && python3 scripts/build-turnip-darwin.py --target ios27)
}

plugin() {
  (cd "$R" && python3 scripts/build-adreno-metal-plugin.py --target ios27)
}

finish() {
  local built=$R/build/adreno-metal-plugin27/NativeAdrenoMetal.bundle bundle=$OUT/NativeAdrenoMetal.bundle
  [ -f "$built/NativeAdrenoMetal" ] && [ -f "$built/libvulkan_freedreno.dylib" ] || { echo "metal: no built bundle in $built" >&2; exit 1; }
  mkdir -p "$OUT"
  rm -rf "$bundle"
  cp -r "$built" "$bundle"
  # The file userland/roottweak looks for: this bundle's addresses are this guest's.
  echo "$TARGET_ID" > "$bundle/oneios-target"
  "$IOS_DIR/macho-check.py" --local "$bundle/libvulkan_freedreno.dylib" "$bundle/NativeAdrenoMetal" "$bundle/libvulkan_freedreno.dylib" | tee "$OUT/NativeAdrenoMetal.check.txt"
  python3 - "$ROOT" "$R" "$bundle" "$OUT/NativeAdrenoMetal.build.json" "$HOOKS" "$TARGET_ID" <<'EOF'
import json, os, subprocess, sys
root, staged, bundle, record, hooks, target = sys.argv[1:7]
sys.path.insert(0, os.path.join(root, 'userland'))
from roottweak import macho
def head(path):
    return subprocess.run(['git', '-C', path, 'rev-parse', 'HEAD'], capture_output=True, text=True).stdout.strip()
files = {}
for name in ('NativeAdrenoMetal', 'libvulkan_freedreno.dylib'):
    data = open(os.path.join(bundle, name), 'rb').read()
    info = macho.describe(data)[0]
    files[name] = {'bytes': len(data), 'cdhash': info['cdhash'], 'arch': info['arch'], 'page_size': info['page_size'],
                   'identifier': info['identifier'], 'minos': info['minos']}
env = os.environ
tbd = os.path.join(env['IOS_TBD'], 'tbd.json')
json.dump({
    'bundle': 'NativeAdrenoMetal.bundle', 'target': target, 'files': files,
    'cdhashes': sorted(f['cdhash'] for f in files.values()),
    'sdk': {'path': env['IOS_SDK'], 'version': env['IOS_SDK_VERSION']},
    'stubs': json.load(open(tbd)) if os.path.exists(tbd) else None,
    'deployment_target': env['IOS_MIN'], 'clang': subprocess.run([env['IOS_CLANG'], '--version'], capture_output=True, text=True).stdout.splitlines()[0],
    'ld64': env['IOS_LD_VERSION'] or None,
    'linker': (subprocess.run([env['IOS_LD'], '-v'], capture_output=True, text=True).stderr.splitlines() or [''])[0].strip(),
    'rustc': subprocess.run(['rustc', '+1.98.0', '--version'], capture_output=True, text=True).stdout.strip(),
    'sources': {name: head(os.path.join(root, 'external', name)) for name in
                ('NativeMetal', 'metal2vulkan', 'mesa-droidvm', 'Droid-VM-UniXNU', 'NativeAdreno', 'AdrenoPM4')},
    'hooks': os.path.relpath(hooks, root), 'check': 'macho-check.py passed against the stubs above',
}, open(record, 'w'), indent=2)
print('metal: built for', target)
for name, f in files.items():
    print(f'metal:   {name}: {f["bytes"]} bytes, CDHash {f["cdhash"]} ({f["arch"]}, {f["page_size"]}-byte pages)')
print('metal:   SDK', env['IOS_SDK_VERSION'], env['IOS_SDK'])
EOF
  say "bundle $bundle; record $OUT/NativeAdrenoMetal.build.json"
}

# The bundle as a GPU addon: the sidecar kernel.kc.gpuaddon (format NGPUADD2, the format of Droid-VM-UniXNU's
# make-gpu-addon.py). xnuhost hands it to the kernel, which writes it into /private/var/ngpu before its addon mount
# (0031-gpu-addon-mount.patch), so the Data store survives a rebuilt plug-in. The bundle's CDHashes must be in the
# image's overlay/userland/owner-trust.json all the same; this is for changing the plug-in without a new Data seed.
addon() {
  local bundle=$OUT/NativeAdrenoMetal.bundle
  [ -f "$bundle/NativeAdrenoMetal" ] || { echo "metal: no bundle in $OUT: run the build first" >&2; exit 1; }
  python3 -I - "$bundle" "$OUT/NativeAdrenoMetal.gpuaddon" <<'EOF2'
import os, struct, sys, zlib
bundle, output = sys.argv[1:3]
files = []
for current, dirs, names in os.walk(bundle):
    dirs.sort()
    for name in sorted(names):
        path = os.path.join(current, name)
        rel = os.path.relpath(path, bundle).replace(os.sep, '/')
        files.append(('addons/NativeAdrenoMetal.bundle/' + rel, 0o755 if os.access(path, os.X_OK) else 0o644, open(path, 'rb').read()))
files.append(('addons/NativeAdrenoMetal.publish', 0o644, b'1\n'))
def pad(data):
    return data + bytes(-len(data) % 8)
body = b''
for name, mode, data in files:
    encoded = name.encode() + b'\0'
    assert len(encoded) <= 160
    packed = zlib.compress(data, 9)
    body += struct.pack('<IIQQ', len(encoded), mode, len(data), len(packed)) + pad(encoded) + pad(packed)
blob = b'NGPUADD2' + struct.pack('<IIQ', len(files), 0, 24 + len(body)) + body
assert len(blob) <= 64 << 20, 'over xnuhost\'s 64 MiB limit for the sidecar'
open(output, 'wb').write(blob)
print(f'metal: {output}: {len(files)} files, {len(blob)} bytes')
EOF2
}

say "SDK $IOS_SDK_VERSION ($IOS_SDK); stubs $([ -f "$IOS_TBD/usr/lib/libSystem.B.tbd" ] && echo "$IOS_TBD" || echo "none: the SDK's own"); target $IOS_TARGET"
case "${1:-all}" in
  stage) stage ;;
  turnip) turnip ;;
  plugin) plugin ;;
  finish) finish; addon ;;
  addon) addon ;;
  all) stage; turnip; plugin; finish; addon ;;
  *) sed -n '2,6p' "$0"; exit 2 ;;
esac
