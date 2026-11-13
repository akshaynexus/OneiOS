#!/bin/bash
# The kernel build's small steps against the frozen output of the Python scripts they replaced (retired
# 2026-11-13): kernel/hosted-fixup.py, kernel/linux/iig-fix.py, kernel/tests/series-check.py, kernel/kdk-headers.py
# and drivers/build.py -> `oneios-tool kernel hosted-fixup | iig-fix | series-check | kdk-headers | drivers`.
#
#   tools/oneios-tool/tests/kernelpy/compare-golden.sh     every case against golden/cases.txt: one line per case
#                                                          (same | DIFFERENT), then the counts; exit 1 when one differs
#   UPDATE=1 ...                                           write golden/cases.txt from this run instead
#   IMPL=python ...                                        run the Python scripts instead of the commands: only at a
#                                                          commit that still has them (the goldens were frozen with
#                                                          IMPL=python UPDATE=1 at the last such commit, then checked
#                                                          with the commands)
#
# Every input is made here or by `oneios-tool debug fixture` (crates/oneios-testkit): a made-up kernel, a made-up
# iig (fixtures/iig.in), a copy of this checkout's series and profiles with one thing broken per case, a made-up
# KernelDebugKit.pkg (and a stand-in `7z` that hands it over, as 7z extracts it from a disk image on Linux), and a
# made-up checkout whose drivers/ are one-line stand-ins compiled by a stand-in compiler. No Apple file is read.
# A case is: the command's stdout and stderr, its exit status, and the SHA-256 of what it wrote, with the scratch
# folder's path taken out. Not covered here: `kdk-headers` on a real Kernel Debug Kit and over HTTP (hdiutil, macOS
# only; on macOS its cases here are skipped, because hdiutil attaches no made-up image), and `drivers` in a real
# kernel tree (compare-python.sh, before the retirement: README "kernelpy").
# Env: ONEIOS_TOOL (the binary), KEEP=1 (keep the scratch folder), TMPDIR.
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
TOOL=${ONEIOS_TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
IMPL=${IMPL:-rust}
[ -x "$TOOL" ] || { echo "compare-golden: no $TOOL (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)" >&2; exit 2; }
# (the real path: on macOS /tmp is a link, and the steps print resolved paths)
W=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/kernelpy.XXXXXX")" && pwd -P)
[ "${KEEP:-0}" = 1 ] || trap 'rm -rf "$W"' EXIT
FOUND=$W/found.txt; : > "$FOUND"
sha() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

# step NAME ARGS...: one of the five steps, by the chosen implementation. REPO is the checkout a step works in.
step() {
  local name=$1; shift
  if [ "$IMPL" = python ]; then
    local script
    case "$name" in
      hosted-fixup) script=$ROOT/kernel/hosted-fixup.py ;;
      iig-fix) script=$ROOT/kernel/linux/iig-fix.py ;;
      kdk-headers) script=$ROOT/kernel/kdk-headers.py ;;
      series-check) script=$REPO/kernel/tests/series-check.py ;;   # it finds its checkout from its own place
      drivers) script=$REPO/drivers/build.py ;;                    # and so does this one
    esac
    [ -f "$script" ] || { echo "compare-golden: $script is not there (retired): IMPL=python cannot run" >&2; exit 2; }
    python3 -I "$script" "$@"
  else
    case "$name" in
      series-check|drivers) "$TOOL" kernel "$name" "$@" --repo "$REPO" ;;
      *) "$TOOL" kernel "$name" "$@" ;;
    esac
  fi
}
# a NAME COMMAND...: one case: the command's stdout, then its stderr (the scratch folder and an archive's size taken
# out; apart, because Python wrote its buffered stdout after the stderr of a failure), and its status.
a() {
  local name=$1; shift
  "$@" > "$W/case.out" 2> "$W/case.err"; local status=$?
  { echo "== $name"; sed -e "s#$W#W#g" -e 's/([0-9]* bytes)$/(N bytes)/' "$W/case.out"; sed -e "s#$W#W#g" -e 's/^/stderr: /' "$W/case.err"; echo "status $status"; } >> "$FOUND"
}
# wrote FILE...: the files' hashes; tree DIR: every file's hash, by name.
wrote() { local f; for f in "$@"; do echo "sha256 $(sha < "$f" | cut -c1-64) ${f#"$W"/}" >> "$FOUND"; done; }
tree() { ( cd "$1" && find . -type f | LC_ALL=C sort | while read -r f; do echo "sha256 $(sha < "$f" | cut -c1-64) $f"; done ) >> "$FOUND"; }
REPO=$ROOT

# ---- hosted-fixup
"$TOOL" debug fixture hosted-kernel "$W/kernel.macho"
a "hosted-fixup: a made-up kernel" step hosted-fixup "$W/kernel.macho"; wrote "$W/kernel.macho"
a "hosted-fixup: the fixed kernel again" step hosted-fixup "$W/kernel.macho"; wrote "$W/kernel.macho"
printf 'not a kernel, but longer than a Mach-O header is\n' > "$W/text"
a "hosted-fixup: a file that is no kernel" step hosted-fixup "$W/text"; wrote "$W/text"

# ---- iig-fix
cp "$HERE/fixtures/iig.in" "$W/iig"
a "iig-fix: a made-up iig" step iig-fix "$W/iig"; wrote "$W/iig"
a "iig-fix: the fixed copy is refused" step iig-fix "$W/iig"; wrote "$W/iig"

# ---- series-check: a copy of what it reads, then one thing broken per case
fresh() {
  REPO=$W/series; rm -rf "$REPO"; mkdir -p "$REPO/kernel/tests" "$REPO/kernel/unixnu/profiles/os" "$REPO/profiles"
  cp -R "$ROOT/kernel/device" "$REPO/kernel/device"; cp "$ROOT/kernel/pins.sh" "$REPO/kernel/"
  cp -R "$ROOT/kernel/unixnu/profiles/os/ios-27.0.1-24A446" "$REPO/kernel/unixnu/profiles/os/"
  cp "$ROOT/profiles/ios-27.0.1-iPhone18,2.toml" "$REPO/profiles/"
  [ ! -f "$ROOT/kernel/tests/series-check.py" ] || cp "$ROOT/kernel/tests/series-check.py" "$REPO/kernel/tests/"
}
S=kernel/device/patches/xnu-12377
edit() { sed "$1" "$REPO/$2" > "$W/edit.tmp" && mv "$W/edit.tmp" "$REPO/$2"; }
fresh; a "series-check: this checkout's series and profiles (no external/ beside them)" step series-check
fresh; echo made-up > "$REPO/$S/9998-made-up.patch"; printf '9998-made-up.patch\n9998-made-up.patch\n' >> "$REPO/$S/series"
a "series-check: a patch listed twice" step series-check
fresh; echo made-up > "$REPO/$S/9999-not-listed.patch"; echo 9997-missing.patch >> "$REPO/$S/series"
a "series-check: a listed patch missing, a patch not listed" step series-check
fresh; mkdir -p "$REPO/external/Droid-VM-UniXNU/made-up"
printf '%s\n' ../../../../external/SomethingElse/a.patch ../../../../external/Droid-VM-UniXNU/made-up/not-there.patch >> "$REPO/$S/series"
a "series-check: an entry outside the pinned repositories, an external patch that is not at the pin" step series-check
fresh; printf 'CONFIG_XNU_MADE_UP=maybe\nCONFIG_OTHER=y\n' >> "$REPO/kernel/device/configs/oneios_defconfig"
a "series-check: two bad defconfig lines" step series-check
fresh; edit 's/^product = "iPhone18,2"/product = "iPhone17,1"/; s/^build = "24A446"/build = "24A000"/; s/^arch = "arm64e"/arch = "arm64"/' kernel/unixnu/profiles/os/ios-27.0.1-24A446/profile.toml
edit 's/^XNU_COMMIT=.*/XNU_COMMIT=0000/' kernel/pins.sh; edit 's/^os_profile = .*/os_profile = "another"/' kernel/device/board.toml
a "series-check: another product, build and arch in the OS profile, another pin, another profile in board.toml" step series-check
fresh; edit 's/^chip_id = .*/chip_id = 1/' profiles/ios-27.0.1-iPhone18,2.toml
a "series-check: another chip in the image profile" step series-check
REPO=$ROOT

# ---- kdk-headers: the made-up package, handed over by a stand-in 7z (7z e -y -oDIR IMAGE '*/KernelDebugKit.pkg' -r)
mkdir -p "$W/bin" "$W/kdk"
"$TOOL" debug fixture kdk-pkg "$W/kdk/good.pkg"; printf "no package: forty bytes of made-up text\n" > "$W/kdk/bad.pkg"; echo "a disk image, never read" > "$W/kdk/made-up.dmg"
{ echo '#!/bin/sh'; echo "for a in \"\$@\"; do case \"\$a\" in -o*) cp \"$W/kdk/\$(cat \"$W/kdk/which\").pkg\" \"\${a#-o}/KernelDebugKit.pkg\" ;; esac; done"; } > "$W/bin/7z"
chmod +x "$W/bin/7z"
kdk() { PATH=$W/bin:$PATH step kdk-headers "$@"; }
if [ "$(uname -s)" = Darwin ]; then
  echo "(kdk-headers: on macOS a local image is attached with hdiutil, which attaches no made-up image: its cases are taken from the golden)" >&2
  awk '/^== kdk-headers/ {on=1} /^== drivers/ {on=0} on' "$HERE/golden/cases.txt" >> "$FOUND"
else
  echo good > "$W/kdk/which"
  a "kdk-headers: the SDK payload's headers and our Kernel.h" kdk --dmg "$W/kdk/made-up.dmg" "$W/kdk/out"; tree "$W/kdk/out"
  a "kdk-headers: the same image again" kdk --dmg "$W/kdk/made-up.dmg" "$W/kdk/out"; tree "$W/kdk/out"
  a "kdk-headers: --apple-kernel-h (the KDK payload read up to its Kernel.h)" kdk --apple-kernel-h --dmg "$W/kdk/other-name.dmg" "$W/kdk/out"; tree "$W/kdk/out"
  echo bad > "$W/kdk/which"
  a "kdk-headers: a package that is no xar" kdk --dmg "$W/kdk/third.dmg" "$W/kdk/out2"
fi

# ---- drivers: a made-up checkout with this checkout's list of sources, each a one-line stand-in; the records'
# compiler is a stand-in that writes what it was asked to compile, or fails for a source that says so
REPO=$W/checkout; mkdir -p "$REPO/drivers" "$W/records/osfmk/DEVELOPMENT" "$W/records/bsd/DEVELOPMENT" "$W/records/iokit/DEVELOPMENT" "$W/tree"
cp "$HERE/fixtures/drivers-sources.txt" "$REPO/drivers/sources.txt"
[ ! -f "$ROOT/drivers/build.py" ] || cp "$ROOT/drivers/build.py" "$REPO/drivers/"
grep -v '^#' "$REPO/drivers/sources.txt" | while read -r universe name; do
  [ -n "$name" ] || continue
  mkdir -p "$REPO/drivers/$(dirname "$name")"; echo "/* stand-in for $name ($universe) */" > "$REPO/drivers/$name"
done
cat > "$W/cc" <<'EOF'
#!/bin/sh
# the stand-in compiler: cc RECORD-NAME ... SOURCE -o OBJECT ...: the object is a line of what was asked
record=$1; shift; out=; src=; all="$*"
while [ $# -gt 0 ]; do case "$1" in -o) out=$2; shift ;; *.c|*.cpp) src=$1 ;; esac; shift; done
if grep -q 'does not compile' "$src"; then echo "$(basename "$src"):1: error: made-up" >&2; exit 1; fi
echo "$record: $(basename "$src") in $(basename "$PWD"): $all" | sed "s#$(dirname "$out")#OBJECTS#g; s#$(dirname "$(dirname "$src")")#DRIVERS#g" > "$out"
EOF
chmod +x "$W/cc"
for r in osfmk/DEVELOPMENT/startup.o.json bsd/DEVELOPMENT/kern_control.o.json iokit/DEVELOPMENT/IOPlatformExpert.cpo.json; do
  printf '{"directory": "%s", "file": "/made/up/%s.c", "output": "x.o", "arguments": ["%s", "%s", "-DKERNEL", "-Wall", "-Wa,-keep", "/made/up/%s.c", "-MD", "-MF", "x.d", "-Xclang", "-Wno-x", "-o", "x.o", "-MT", "x.o"]},\n' \
    "$W/tree" "$(basename "$r")" "$W/cc" "$(basename "$r" .json)" "$(basename "$r")" > "$W/records/$r"
done
a "drivers: --check" step drivers --records "$W/records" --out "$W/drivers-out" --check
a "drivers: the archive and objects.json" step drivers --records "$W/records/" --out "$W/drivers-out"
tree "$W/drivers-out/objects"; sed "s#$W#W#g" "$W/drivers-out/objects.json" >> "$FOUND"
{ echo "members:"; ar t "$W/drivers-out/liboneiosdrivers.a"; } >> "$FOUND"
echo "this one does not compile" >> "$REPO/drivers/bluetooth/oneios_bt_h4.c"
a "drivers: a source that does not compile" step drivers --records "$W/records" --out "$W/drivers-out2" --check
a "drivers: no records" step drivers --records "$W/none" --out "$W/drivers-out3"
REPO=$ROOT

# ---- the result
if [ "${UPDATE:-0}" = 1 ]; then
  mkdir -p "$HERE/golden"; cp "$FOUND" "$HERE/golden/cases.txt"
  echo "compare-golden: golden/cases.txt written from IMPL=$IMPL ($(grep -c '^== ' "$FOUND") cases)"; exit 0
fi
block() { awk -v n="== $1" '$0 == n {on=1; print; next} /^== / {on=0} on' "$2"; }
same=0; differ=0
while read -r name; do
  if [ "$(block "$name" "$FOUND")" = "$(block "$name" "$HERE/golden/cases.txt")" ]; then same=$((same + 1)); echo "same       $name"
  else differ=$((differ + 1)); echo "DIFFERENT  $name"; diff <(block "$name" "$HERE/golden/cases.txt") <(block "$name" "$FOUND") | head -8; fi
done < <(grep '^== ' "$HERE/golden/cases.txt" | sed 's/^== //')
# (a case that ran and is not in the golden counts too)
[ "$(grep -c '^== ' "$FOUND")" = "$(grep -c '^== ' "$HERE/golden/cases.txt")" ] || { differ=$((differ + 1)); echo "DIFFERENT  the list of cases"; }
echo "kernelpy (IMPL=$IMPL): $same the same, $differ different"
[ $differ = 0 ]
