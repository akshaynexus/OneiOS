#!/bin/bash
# Host tests of runtime/package.sh: the package it makes, and what it must refuse. Needs the NDK (see package.sh).
#   runtime/package/test.sh
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
export OUT_DIR=$T/out KERNEL_DIR=$T/no-kernel
V=0.0.0-test
fail() { echo "FAIL: $*" >&2; exit 1; }
pass=0
if command -v sha256sum >/dev/null; then SHA256=(sha256sum); else SHA256=(shasum -a 256); fi
ok() { pass=$((pass + 1)); echo "ok $pass: $*"; }
# The NDK, found as package.sh finds it (runtime/ndk-cc links the faulty library of the refusals with its clang).
if [ "$(uname -s)" = Darwin ]; then SDK_DEFAULT=$HOME/Library/Android/sdk; else SDK_DEFAULT=$HOME/Android/Sdk; fi
export NDK=${NDK:-${ANDROID_NDK_ROOT:-${ANDROID_SDK_ROOT:-$SDK_DEFAULT}/ndk/28.2.13676358}}
[ -d "$HOME/.cargo/bin" ] && PATH=$HOME/.cargo/bin:$PATH

# runtime/build.sh refuses a stage without external/virglrenderer; a tree without that checkout (a fresh worktree) tests
# the package with the 2D-only xnuhost, on purpose. CI's release job fetches the checkout and checks it is there.
[ -f "$ROOT/external/virglrenderer/src/drm/drm2kgsl/drm2kgsl_renderer.c" ] || export XNUHOST_2D_ONLY=1

# 1. A plain package: verifies, runtime.json first, manifest equals the content.
"$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || { cat "$T/log"; fail "package.sh"; }
P=$OUT_DIR/oneios-runtime-$V-arm64.tar.zst
( cd "$OUT_DIR" && "${SHA256[@]}" -c "$(basename "$P").sha256" > /dev/null ) || fail "sha256 file"
[ "$(wc -l < "$P.sha256" | tr -d ' ')" = 1 ] || fail ".sha256 is not one line"
[ "$(zstd -dc "$P" | tar -t | head -1)" = runtime.json ] || fail "runtime.json is not the first member"
mkdir "$T/x" && zstd -dc "$P" | tar -x -C "$T/x"
# The manifest (oneios-tool json: runtime/package.sh builds or finds the tool the same way, ONEIOS_TOOL=PATH another).
. "$ROOT/scripts/lib/oneios-tool.sh"
oneios_tool "$ROOT"
J=("$ONEIOS_TOOL" json)
M=$T/x/runtime.json
manifest_check() {
  local path size sha mode want on_disk listed
  [ "$("${J[@]}" get "$M" format)/$("${J[@]}" get "$M" name)/$("${J[@]}" get "$M" version)/$("${J[@]}" get "$M" arch)/$("${J[@]}" get "$M" session_abi)" = "1/oneios-runtime/$V/arm64/1" ] || { echo "format, name, version, arch, session_abi: $(cat "$M")"; return 1; }
  [ "$("${J[@]}" get "$M" xnuhost)" = bin/xnuhost ] || { echo "xnuhost"; return 1; }
  [ "$("${J[@]}" get "$M" git.commit | tr -d '\n' | wc -c | tr -d ' ')" = 40 ] || { echo "git.commit"; return 1; }
  [ -n "$("${J[@]}" get "$M" min_oneos_fork)" ] || { echo "min_oneos_fork"; return 1; }
  listed=$("${J[@]}" lines "$M" files path | LC_ALL=C sort)
  on_disk=$(cd "$T/x" && find . ! -type d ! -path ./runtime.json | sed 's#^\./##' | LC_ALL=C sort)
  [ "$listed" = "$on_disk" ] || { echo "listed and on disk differ:"; diff <(echo "$listed") <(echo "$on_disk"); return 1; }
  while IFS="$(printf '\t')" read -r path size sha mode; do
    [ "$(wc -c < "$T/x/$path" | tr -d ' ')" = "$size" ] && [ "$("${SHA256[@]}" "$T/x/$path" | cut -d' ' -f1)" = "$sha" ] || { echo "size or sha256: $path"; return 1; }
    if [ -x "$T/x/$path" ]; then want=0755; else want=0644; fi
    [ "$mode" = "$want" ] || { echo "mode $mode, not $want: $path"; return 1; }
  done < <("${J[@]}" lines "$M" files path size sha256 mode)
  # the phone checks the OneOS fork's Hardware checks screen runs (runtime/build.sh stages them)
  for tool in xnuhost gpuctl bridgectl wifictl brightctl displayctl camctl oneios-camera oneios-audio oneios-display oneios-encoder encctl; do
    [ "$("${J[@]}" get "$M" "files[path=bin/$tool].mode")" = 0755 ] || { echo "bin/$tool is not 0755"; return 1; }
  done
}
manifest_check || fail "manifest does not match the content"
ok "package, checksum, member order, manifest"
zstd -dc "$P" | tar -tv | awk '$1 !~ /^-/ {bad=1} END {exit bad}' || fail "a member is not a regular file"
ok "regular files only"
# The phone's oneios-tool: in the package (0755, listed in runtime.json), and beside it with its .sha256, the same bytes.
TA=$OUT_DIR/oneios-tool-$V-android-arm64
[ -x "$T/x/bin/oneios-tool" ] || fail "no bin/oneios-tool in the package"
grep -q '"path": "bin/oneios-tool"' "$T/x/runtime.json" || fail "bin/oneios-tool is not in runtime.json"
( cd "$OUT_DIR" && "${SHA256[@]}" -c "$(basename "$TA").sha256" > /dev/null ) || fail "oneios-tool's .sha256"
cmp -s "$TA" "$T/x/bin/oneios-tool" || fail "oneios-tool beside the package is not the package's bin/oneios-tool"
[ "$(od -An -tx1 -N20 "$TA" | tr -d ' \n')" = 7f454c460201010000000000000000000300b700 ] || fail "oneios-tool is not an arm64 PIE"
ok "bin/oneios-tool and $(basename "$TA") ($(wc -c < "$TA" | tr -d ' ') bytes, stripped)"
cmp -s "$T/x/lib/profiles/ios-27.0.1-iPhone18-2.toml" "$ROOT/profiles/ios-27.0.1-iPhone18,2.toml" || fail "lib/profiles/ is not profiles/"
grep -q '"path": "lib/profiles/ios-27.0.1-iPhone18-2.toml"' "$T/x/runtime.json" || fail "lib/profiles/ is not in runtime.json"
# the Galaxy A56's iOS 16.5 profile (docs/a56.md): the OneOS fork's Make iOS image sends it on an A56
cmp -s "$T/x/lib/profiles/ios-16.5-a56.toml" "$ROOT/profiles/ios-16.5-a56.toml" || fail "lib/profiles/ has no A56 profile"
ok "lib/profiles/: the IPSW profiles for the phone's fetch"
# The switch catalog (package.sh step 1d): there, in runtime.json, version 1, and read back by `switches --runtime`
# (what the OneOS app runs on the phone) as this tree's catalog.
grep -q '"path": "lib/switches.json"' "$T/x/runtime.json" || fail "lib/switches.json is not in runtime.json"
[ "$("${J[@]}" get "$T/x/lib/switches.json" version)" = 1 ] || fail "lib/switches.json is not a catalog of version 1"
N=$("${J[@]}" len "$T/x/lib/switches.json" switches)
[ "$N" -gt 40 ] || fail "lib/switches.json has $N switches"
"$ONEIOS_TOOL" switches --json --runtime "$T/x" > "$T/switches.json" || fail "switches --runtime does not read lib/switches.json"
"$ONEIOS_TOOL" switches --json | cmp -s - "$T/switches.json" || fail "lib/switches.json is not this tree's catalog"
ok "lib/switches.json: the switch catalog ($N switches)"
# Its dynamic ELF files were checked against the NDK's API 30 stubs (elf-check.sh), twice: runtime/build.sh on its
# stage, then package.sh on the package's content.
[ "$(grep -c '^elf-check: .* 0 failures$' "$T/log")" = 2 ] || { cat "$T/log"; fail "package.sh did not run elf-check.sh twice"; }
grep -q '^elf-check: bin/liboneios-display.so: NEEDED .*libnativewindow.so' "$T/log" || { cat "$T/log"; fail "bin/liboneios-display.so was not checked"; }
ok "NEEDED and symbol check: $(grep -m1 '^elf-check: .* 0 failures$' "$T/log" | sed 's/^elf-check: //')"

# 2. The same inputs give the same bytes.
first=$(cut -d' ' -f1 "$P.sha256")
"$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || fail "second run"
[ "$first" = "$(cut -d' ' -f1 "$P.sha256")" ] || fail "not reproducible"
ok "reproducible"

# 3. Refusals: exit 3 and no package left behind for that version.
refuse() { # NAME, then a command that fills $T/extra
  local name=$1; shift
  rm -rf "$T/extra" "$OUT_DIR"; mkdir -p "$T/extra"
  "$@"
  local rc=0
  PACKAGE_EXTRA_DIR=$T/extra "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 3 ] || { cat "$T/log"; fail "$name: exit $rc, expected the refusal (3)"; }
  [ ! -e "$OUT_DIR/oneios-runtime-$V-arm64.tar.zst" ] || fail "$name: a package was written"
  ok "refused: $name ($(grep -m1 'refused' "$T/log" | cut -c1-110))"
}
touch_file() { mkdir -p "$(dirname "$T/extra/$1")"; printf 'x' > "$T/extra/$1"; }
macho() { # PATH FILETYPE: a minimal arm64 Mach-O header (mach_header_64, little-endian), then 64 zero bytes
  mkdir -p "$(dirname "$T/extra/$1")"
  { printf '\317\372\355\376\014\000\000\001\002\000\000\000'; printf "\\$(printf %03o "$2")\\000\\000\\000"; dd if=/dev/zero bs=80 count=1 2> /dev/null; } > "$T/extra/$1"
}
record() { # PATH [KEY=JSON...]: a build record for the Mach-O, with fields replaced
  local r=$T/extra/$1.build.json pair
  printf '{"builder": "oneios-kernel", "build_id": "test-1", "sha256": "%s", "prelinked": false, "kernelcache_kexts_added": false, "apple_binary_inputs": [], "restricted_sources": [], "xnu": "xnu-0", "profile": "test"}\n' \
    "$("${SHA256[@]}" "$T/extra/$1" | cut -d' ' -f1)" > "$r"
  for pair in "${@:2}"; do "${J[@]}" set "$r" "${pair%%=*}" "${pair#*=}" || fail "record: $pair"; done
}
refuse "kernelcache by name" touch_file firmware/kernelcache.release.iphone18
refuse "a path the fork's unpacker rejects (comma)" touch_file notes,v2.txt
refuse ".ipsw" touch_file restore.ipsw
refuse ".dmg" touch_file os/090-12345-678.dmg
refuse ".aea" touch_file os.dmg.aea
refuse ".img" touch_file image/os.img
refuse "cryptex" touch_file cryptex1-systemos
im4p() { mkdir -p "$T/extra/bin"; printf '\x30\x83\x12\x34\x56\x16\x04IM4P\x16\x04krnl' > "$T/extra/bin/blob"; }
refuse "IM4P content under another name" im4p
m1() { macho kernel/xnu.macho 2; }
refuse "Mach-O without a build record" m1
m2() { macho kernel/xnu.macho 2; record kernel/xnu.macho 'kernelcache_kexts_added=true'; }
refuse "kernel with kernelcache kexts" m2
m3() { macho kernel/xnu.macho 2; record kernel/xnu.macho 'sha256="00"'; }
refuse "record for another file" m3
m4() { macho kernel/xnu.macho 12; record kernel/xnu.macho; }
refuse "kernel collection (MH_FILESET)" m4
m5() { macho kernel/xnu.macho 2; record kernel/xnu.macho 'builder="someone"'; }
refuse "record of another builder" m5
m6() { macho kernel/xnu.macho 2; record kernel/xnu.macho 'restricted_sources=["corecrypto-2023"]'; }
refuse "kernel that links a non-redistributable source" m6
link() { ln -s /etc/passwd "$T/extra/link"; }
refuse "symbolic link" link
# A library that Android cannot load, in the display helper's place: what liboneios-display.so of 1f7dd1f was.
# elf-fault.rs calls libnativewindow's dataspace functions and is linked against libandroid only, without -z defs.
faulty_library() {
  mkdir -p "$T/extra/bin"
  rustc --edition 2021 --target aarch64-linux-android -C opt-level=2 -C linker="$ROOT/runtime/ndk-cc" -A linker_messages \
    --crate-type cdylib "$ROOT/runtime/package/elf-fault.rs" -o "$T/extra/bin/liboneios-display.so"
}
refuse "a shared object with a symbol that none of its NEEDED libraries has" faulty_library
for symbol in ANativeWindow_getBuffersDataSpace ANativeWindow_setBuffersDataSpace; do
  grep -qxF "bin/liboneios-display.so: symbol $symbol is in none of its NEEDED libraries (NEEDED: libdl.so, libc.so); it is in libnativewindow.so" "$T/log" ||
    { cat "$T/log"; fail "the refusal does not name $symbol and libnativewindow.so"; }
done
ok "the refusal names it: $(grep -m1 -o 'symbol ANativeWindow_getBuffersDataSpace.*' "$T/log")"

# 4. Our own kernel with its record is accepted and named in the manifest.
rm -rf "$T/extra" "$OUT_DIR" "$T/k"; mkdir -p "$T/k" "$T/extra/kernel"
macho kernel/xnu.macho 2; record kernel/xnu.macho
mv "$T/extra/kernel/xnu.macho" "$T/extra/kernel/xnu.macho.build.json" "$T/k/"
KERNEL_DIR=$T/k "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || { cat "$T/log"; fail "package with a kernel"; }
zstd -dc "$P" | tar -xO runtime.json > "$T/m.json"
[ "$("${J[@]}" get "$T/m.json" kernel.build_id)/$("${J[@]}" get "$T/m.json" kernel.path)" = test-1/kernel/xnu.macho ] || fail "kernel not in the manifest"
zstd -dc "$P" | tar -xO NOTICE | grep -q 'Apple Public Source License' || fail "no APSL notice"
ok "own kernel accepted, APSL notice present"
# 4b. Another iOS build's kernel (KERNEL_MORE_DIRS): kernel/<build>/, both in runtime.json "kernels", "kernel" the default's
rm -rf "$T/extra" "$OUT_DIR" "$T/k2"; mkdir -p "$T/k2" "$T/extra/kernel"
macho kernel/xnu.macho 2; record kernel/xnu.macho profile='"ios-27.2.1-24B156"' build_id='"test-2"' releasable=true
mv "$T/extra/kernel/xnu.macho" "$T/extra/kernel/xnu.macho.build.json" "$T/k2/"
"${J[@]}" set "$T/k/xnu.macho.build.json" profile '"ios-27.0.1-24A446"'
KERNEL_DIR=$T/k KERNEL_MORE_DIRS=$T/k2 "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || { cat "$T/log"; fail "package with two kernels"; }
zstd -dc "$P" | tar -xO runtime.json > "$T/m.json"
[ "$("${J[@]}" get "$T/m.json" kernel.path)|$("${J[@]}" len "$T/m.json" kernels)|$("${J[@]}" get "$T/m.json" kernels.0.build)|$("${J[@]}" get "$T/m.json" kernels.1.build)|$("${J[@]}" get "$T/m.json" kernels.1.path)|$("${J[@]}" get "$T/m.json" kernels.1.build_id)" = \
  "kernel/xnu.macho|2|24A446|24B156|kernel/24B156/xnu.macho|test-2" ] || { cat "$T/m.json"; fail "runtime.json's kernels"; }
grep -q '^package: kernel for 24B156 test-2 (kernel/24B156/)$' "$T/log" || { cat "$T/log"; fail "no line for the second kernel"; }
rc=0; KERNEL_MORE_DIRS=$T/k2 "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
[ "$rc" = 1 ] && grep -q 'KERNEL_MORE_DIRS without a default kernel' "$T/log" || { cat "$T/log"; fail "KERNEL_MORE_DIRS alone: exit $rc"; }
"${J[@]}" set "$T/k2/xnu.macho.build.json" releasable false
rc=0; KERNEL_DIR=$T/k KERNEL_MORE_DIRS=$T/k2 "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
[ "$rc" = 1 ] && grep -q 'is not releasable' "$T/log" || { cat "$T/log"; fail "a second kernel not releasable: exit $rc"; }
ok "two kernels: kernel/24B156/ beside kernel/, runtime.json \"kernels\" lists both; refused alone or not releasable"

# 5. The guest's plug-ins (METAL_DIR, CAMERA_DIR: lib/guest/): made-up signed Mach-Os (`debug fixture`), taken and named
# in runtime.json; Apple's code or an executable there refused. lib/guest/NOTICE takes licence texts from
# METAL_LICENSES_DIR (default external/); CI supplies the pinned licence files from the Metal build job.
guest_dir() { # DIR TARGET: userland/metal/build.sh's output folder, and DIR/camera as userland/camera/build.sh's
  local d=$1 b=$1/NativeAdrenoMetal.bundle p t
  mkdir -p "$b" "$d/camera"
  "$ONEIOS_TOOL" debug fixture guest-bundle "$b/NativeAdrenoMetal" && chmod 0755 "$b/NativeAdrenoMetal"
  "$ONEIOS_TOOL" debug fixture guest-dylib "$b/libvulkan_freedreno.dylib"
  "$ONEIOS_TOOL" debug fixture guest-dylib "$d/camera/H9ISP.mediacapture"
  printf '<plist/>\n' > "$b/Info.plist"
  echo "$2" > "$b/oneios-target"
  p=$("$ONEIOS_TOOL" image tweaks cdhash "$b/NativeAdrenoMetal" | cut -d' ' -f1)
  t=$("$ONEIOS_TOOL" image tweaks cdhash "$b/libvulkan_freedreno.dylib" | cut -d' ' -f1)
  printf '{"bundle": "NativeAdrenoMetal.bundle", "target": "%s", "files": {"NativeAdrenoMetal": {"cdhash": "%s"}, "libvulkan_freedreno.dylib": {"cdhash": "%s"}}, "cdhashes": ["%s", "%s"], "sources": {"NativeMetal": "b0d6a07a3da3e23486593aff235904177e94a3ff"}}\n' \
    "$2" "$p" "$t" "$p" "$t" > "$d/NativeAdrenoMetal.build.json"
}
METAL_LICENSES_DIR=${METAL_LICENSES_DIR:-$ROOT/external}
if [ -f "$METAL_LICENSES_DIR/metal2vulkan/LICENSE-GPL-2.0" ] && [ -f "$METAL_LICENSES_DIR/mesa-droidvm/LICENSING.md" ]; then
  rm -rf "$T/extra" "$OUT_DIR" "$T/g"
  guest_dir "$T/g" "iPhone18,2 24A446"
  METAL_DIR=$T/g CAMERA_DIR=$T/g/camera "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || { cat "$T/log"; fail "package with the guest plug-ins"; }
  zstd -dc "$P" | tar -xO runtime.json > "$T/m.json"
  [ "$("${J[@]}" get "$T/m.json" metal.bundle)|$("${J[@]}" get "$T/m.json" metal.target)|$("${J[@]}" len "$T/m.json" metal.cdhashes)|$("${J[@]}" get "$T/m.json" camera.path)" = \
    "lib/guest/NativeAdrenoMetal.bundle|iPhone18,2 24A446|2|lib/guest/camera" ] || { cat "$T/m.json"; fail "runtime.json's metal and camera"; }
  for f in NativeAdrenoMetal.bundle/NativeAdrenoMetal NativeAdrenoMetal.bundle/libvulkan_freedreno.dylib NativeAdrenoMetal.bundle/oneios-target NativeAdrenoMetal.build.json camera/H9ISP.mediacapture NOTICE; do
    grep -q "\"path\": \"lib/guest/$f\"" "$T/m.json" || fail "lib/guest/$f is not in runtime.json"
  done
  [ "$("${J[@]}" get "$T/m.json" 'files[path=lib/guest/NativeAdrenoMetal.bundle/NativeAdrenoMetal].mode')" = 0755 ] || fail "the plug-in is not 0755"
  # (read from files: grep -q at the end of a pipe from tar makes tar fail with SIGPIPE under pipefail)
  zstd -dc "$P" | tar -xO lib/guest/NOTICE > "$T/guest-notice"; zstd -dc "$P" | tar -xO NOTICE > "$T/notice"
  grep -q '^==== metal2vulkan/LICENSE-GPL-2.0$' "$T/guest-notice" || fail "lib/guest/NOTICE has no metal2vulkan licence"
  grep -q '^==== mesa-droidvm/licenses/exceptions/Linux-Syscall-Note$' "$T/guest-notice" || fail "lib/guest/NOTICE has not every file of Mesa's licenses/"
  grep -q '^  NativeMetal https://github.com/Droid-VM/NativeMetal.git b0d6a07a3da3e23486593aff235904177e94a3ff$' "$T/guest-notice" || { head -20 "$T/guest-notice"; fail "lib/guest/NOTICE does not name NativeMetal's source"; }
  grep -q 'lib/guest/NOTICE' "$T/notice" || fail "NOTICE does not name lib/guest/NOTICE"
  grep -q '^package: Metal plug-in for iPhone18,2 24A446 from ' "$T/log" || fail "no Metal line"
  ok "the guest plug-ins: lib/guest/ in runtime.json (metal: 2 CDHashes, camera), lib/guest/NOTICE with the licences"
  # another iOS build of the device (METAL_MORE_DIRS): lib/guest/<build>/, listed in "metals" and "cameras" after the
  # default's, which "metal" and "camera" still name
  rm -rf "$T/g3"; guest_dir "$T/g3" "iPhone18,2 24B156"
  METAL_DIR=$T/g CAMERA_DIR=$T/g/camera METAL_MORE_DIRS=$T/g3 "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || { cat "$T/log"; fail "package with two targets"; }
  zstd -dc "$P" | tar -xO runtime.json > "$T/m.json"
  [ "$("${J[@]}" get "$T/m.json" metal.target)|$("${J[@]}" get "$T/m.json" camera.path)|$("${J[@]}" len "$T/m.json" metals)|$("${J[@]}" get "$T/m.json" metals.0.target)|$("${J[@]}" get "$T/m.json" metals.1.target)|$("${J[@]}" get "$T/m.json" metals.1.bundle)|$("${J[@]}" get "$T/m.json" cameras.1.path)|$("${J[@]}" get "$T/m.json" cameras.1.target)" = \
    "iPhone18,2 24A446|lib/guest/camera|2|iPhone18,2 24A446|iPhone18,2 24B156|lib/guest/24B156/NativeAdrenoMetal.bundle|lib/guest/24B156/camera|iPhone18,2 24B156" ] || { cat "$T/m.json"; fail "runtime.json's metals and cameras"; }
  for f in NativeAdrenoMetal.bundle/NativeAdrenoMetal NativeAdrenoMetal.bundle/oneios-target NativeAdrenoMetal.build.json camera/H9ISP.mediacapture; do
    grep -q "\"path\": \"lib/guest/24B156/$f\"" "$T/m.json" || fail "lib/guest/24B156/$f is not in runtime.json"
  done
  grep -q '^package: Metal plug-in for iPhone18,2 24B156 from .* (lib/guest/24B156/)$' "$T/log" || { cat "$T/log"; fail "no line for the second target"; }
  ok "two targets: lib/guest/24B156/ beside lib/guest/, runtime.json \"metals\" and \"cameras\" list both, \"metal\" stays 24A446's"
  rc=0; METAL_MORE_DIRS=$T/g3 "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 1 ] && grep -q 'METAL_MORE_DIRS without METAL_DIR' "$T/log" || { cat "$T/log"; fail "METAL_MORE_DIRS alone: exit $rc"; }
  rc=0; METAL_DIR=$T/g METAL_MORE_DIRS="$T/g3 $T/g3" "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 1 ] && grep -q 'a second bundle for build 24B156' "$T/log" || { cat "$T/log"; fail "one build twice: exit $rc"; }
  "${J[@]}" set "$T/g3/NativeAdrenoMetal.build.json" sources.NativeMetal '"0000000000000000000000000000000000000000"'
  rc=0; METAL_DIR=$T/g METAL_MORE_DIRS=$T/g3 "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 1 ] && grep -q 'is built from another NativeMetal than' "$T/log" || { cat "$T/log"; fail "another target of other sources: exit $rc"; }
  ok "stopped: METAL_MORE_DIRS without METAL_DIR, one build twice, a second target built from other sources"
  apple_dylib() { mkdir -p "$T/extra/lib/guest/x"; "$ONEIOS_TOOL" debug fixture apple-dylib "$T/extra/lib/guest/x/lib.dylib"; }
  refuse "Apple's code under lib/guest/ (not ad hoc, com.apple.*)" apple_dylib
  grep -q 'not signed ad hoc (com.apple.test.dylib)' "$T/log" || { cat "$T/log"; fail "the refusal does not say why"; }
  guest_tool() { mkdir -p "$T/extra/lib/guest/x"; "$ONEIOS_TOOL" debug fixture guest-execute "$T/extra/lib/guest/x/tool"; }
  refuse "an executable under lib/guest/" guest_tool
  rm -rf "$T/g2"; guest_dir "$T/g2" "iPhone18,2 24A446"; "$ONEIOS_TOOL" debug fixture guest-bundle "$T/g2/NativeAdrenoMetal.bundle/libvulkan_freedreno.dylib"
  rc=0; METAL_DIR=$T/g2 "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 3 ] && grep -q 'libvulkan_freedreno.dylib: CDHash .* is not the one its build record' "$T/log" || { cat "$T/log"; fail "a bundle changed after its build: exit $rc"; }
  ok "refused: a bundle file that is not the one its record lists"
  rm -rf "$T/g2"; mkdir -p "$T/g2"; rc=0
  CAMERA_DIR=$T/g/camera "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 1 ] && grep -q 'CAMERA_DIR without METAL_DIR' "$T/log" || { cat "$T/log"; fail "CAMERA_DIR alone: exit $rc"; }
  rc=0; METAL_DIR=$T/g2 "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 1 ] && grep -q 'holds no whole NativeAdrenoMetal.bundle' "$T/log" || { cat "$T/log"; fail "an empty METAL_DIR: exit $rc"; }
  rc=0; REQUIRE_METAL=1 "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 1 ] && grep -q 'REQUIRE_METAL=1 and no METAL_DIR' "$T/log" || { cat "$T/log"; fail "REQUIRE_METAL=1 alone: exit $rc"; }
  ok "stopped: CAMERA_DIR alone, a METAL_DIR without a bundle, REQUIRE_METAL=1 without one"
else
  echo "not run: the guest plug-ins (no external/metal2vulkan or external/mesa-droidvm: scripts/setup/get-references.sh)"
fi

# 6. The Galaxy A56's GPU plug-ins (METAL_AMD_DIR, step 3c) beside the Adreno one: made-up signed Mach-Os in
# userland/metal-amd's layout, their records made by package.sh (ios bundle-record), runtime.json "metal_a56", and
# lib/guest/NOTICE with Mesa's, Jesvi-iOS's, iOSPlus10's and libdrm's notices. Needs external/Jesvi-iOS too.
a56_dir() { # DIR TARGET-OF-METAL TARGET-OF-GL: userland/metal-amd's output folder
  local d=$1 m=$1/OneiOSRadeonMetal.bundle g=$1/gl/OneiOSRadeonGL.bundle
  mkdir -p "$m" "$g" "$d/libdrm-2.4.133/amdgpu"
  "$ONEIOS_TOOL" debug fixture guest-bundle "$m/OneiOSRadeonMetal" && chmod 0755 "$m/OneiOSRadeonMetal"
  "$ONEIOS_TOOL" debug fixture guest-dylib "$m/libOneiOSRadeonAir.dylib"
  "$ONEIOS_TOOL" debug fixture guest-bundle "$g/OneiOSRadeonGL" && chmod 0755 "$g/OneiOSRadeonGL"
  printf '<plist/>\n' > "$m/Info.plist"; printf '<plist/>\n' > "$g/Info.plist"
  echo "$2" > "$m/oneios-target"; echo "$3" > "$g/oneios-target"
  printf '/*\n * Copyright 2014 Advanced Micro Devices, Inc. (a stand-in notice)\n */\n#include <x.h>\n' > "$d/libdrm-2.4.133/amdgpu/amdgpu_bo.c"
}
if [ -f "$ROOT/external/metal2vulkan/LICENSE-GPL-2.0" ] && [ -f "$ROOT/external/mesa-droidvm/LICENSING.md" ] && [ -f "$ROOT/external/Jesvi-iOS/LICENSE" ]; then
  rm -rf "$T/extra" "$OUT_DIR" "$T/g" "$T/a"
  guest_dir "$T/g" "iPhone18,2 24A446"
  a56_dir "$T/a" "iPhone10,3 20F66" "iPhone10,3 20F66"
  METAL_DIR=$T/g METAL_AMD_DIR=$T/a "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || { cat "$T/log"; fail "package with the A56's GPU plug-ins"; }
  zstd -dc "$P" | tar -xO runtime.json > "$T/m.json"
  [ "$("${J[@]}" get "$T/m.json" metal.target)|$("${J[@]}" get "$T/m.json" metal_a56.target)|$("${J[@]}" get "$T/m.json" metal_a56.metal.bundle)|$("${J[@]}" get "$T/m.json" metal_a56.gl.record)|$("${J[@]}" len "$T/m.json" metal_a56.cdhashes)" = \
    "iPhone18,2 24A446|iPhone10,3 20F66|lib/guest/OneiOSRadeonMetal.bundle|lib/guest/OneiOSRadeonGL.build.json|3" ] || { cat "$T/m.json"; fail "runtime.json's metal_a56"; }
  for f in OneiOSRadeonMetal.bundle/OneiOSRadeonMetal OneiOSRadeonMetal.bundle/libOneiOSRadeonAir.dylib OneiOSRadeonGL.bundle/OneiOSRadeonGL OneiOSRadeonMetal.build.json OneiOSRadeonGL.build.json; do
    grep -q "\"path\": \"lib/guest/$f\"" "$T/m.json" || fail "lib/guest/$f is not in runtime.json"
  done
  zstd -dc "$P" | tar -xO lib/guest/NOTICE > "$T/guest-notice"
  grep -q '^==== metal2vulkan/LICENSE-GPL-2.0$' "$T/guest-notice" || fail "the Adreno plug-in's notices are gone"
  grep -q '^  Jesvi-iOS https://github.com/jesvi-eulah/Jesvi-iOS.git a13e4060dbc81b9db54f4688efbd51e695998420$' "$T/guest-notice" || fail "lib/guest/NOTICE does not name Jesvi-iOS's source"
  grep -q '^  iOSPlus10 https://github.com/akshaynexus/iOSPlus10.git 146e889430bab755329dde7104cc0fa002f44bfb' "$T/guest-notice" || fail "lib/guest/NOTICE does not name iOSPlus10's source"
  grep -q '^==== Jesvi-iOS/LICENSE$' "$T/guest-notice" && grep -q '^==== userland/metal-amd/gl/NOTICE$' "$T/guest-notice" &&
    grep -q 'a stand-in notice' "$T/guest-notice" || fail "lib/guest/NOTICE lacks Jesvi-iOS's licence, the GL notice or libdrm's"
  ok "the A56's GPU plug-ins: lib/guest/ beside the Adreno one, runtime.json metal_a56 (3 CDHashes), lib/guest/NOTICE"
  rm -rf "$T/a"; a56_dir "$T/a" "iPhone10,3 20F66" "iPhone18,2 24A446"
  rc=0; METAL_AMD_DIR=$T/a "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 1 ] && grep -q 'OneiOSRadeonGL.bundle is for iPhone18,2 24A446, OneiOSRadeonMetal.bundle for iPhone10,3 20F66' "$T/log" || { cat "$T/log"; fail "bundles for two images: exit $rc"; }
  rm -rf "$T/a"; a56_dir "$T/a" "iPhone10,3 20F66" "iPhone10,3 20F66"; "$ONEIOS_TOOL" debug fixture apple-dylib "$T/a/OneiOSRadeonMetal.bundle/libOneiOSRadeonAir.dylib"
  rc=0; METAL_AMD_DIR=$T/a "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
  [ "$rc" != 0 ] && grep -q 'not signed ad hoc' "$T/log" || { cat "$T/log"; fail "Apple's code in the A56 bundle: exit $rc"; }
  rc=0; REQUIRE_METAL_AMD=1 "$ROOT/runtime/package.sh" $V > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 1 ] && grep -q 'REQUIRE_METAL_AMD=1 and no METAL_AMD_DIR' "$T/log" || { cat "$T/log"; fail "REQUIRE_METAL_AMD=1 alone: exit $rc"; }
  ok "stopped: the A56 bundles for two images, Apple's code in one, REQUIRE_METAL_AMD=1 without them"
else
  echo "not run: the A56's GPU plug-ins (no external/metal2vulkan, external/mesa-droidvm or external/Jesvi-iOS)"
fi
echo "all $pass tests passed"
