#!/bin/bash
# The Python scripts this compares with were retired on 2026-11-13 (the result: tools/oneios-tool/README.md
# "kernel"; the frozen output: compare-golden.sh). At commit a5b2d95, the last with every script, all of it runs;
# since then a retired name says so and ends with status 2. The last one, `compile-command`, could run while
# drivers/build.py was there: that script was retired the same day (tests/kernelpy; its command for a made-up record
# is frozen in golden/synthetic/command.*.json, which `cargo test` checks).
#
# The board archive generators: `oneios-tool kernel gen NAME` against the Python script it replaces, in the tree of
# a finished hosted kernel build. Each generator runs twice in the same place (the Python script, then the Rust
# command), in the environment and directory UniXNU's driver gave the archive step; every file either one writes
# is compared with cmp, and so are stdout and the exit status.
#
#   tools/oneios-tool/tests/kernelgen/compare-python.sh WORK ENVFILE [NAME...]
#
#   WORK      UniXNU's work directory of the build: out/kernel-hosted-<name>/unixnu/oneios/<os profile>/oneios_hosted
#             (its tree/ holds the first pass's compile records and the sources the archives are built from)
#   ENVFILE   the environment of one archive step of that build: /proc/PID/environ of a `build-oneios-*.py` process,
#             saved while the build ran (tests/kernelgen/save-env.sh NAME OUTDIR does that)
#   NAME      board storage aes keystore net hid audio gpu display, or one of the names below
#             build-record      kernel/build-record.py against `oneios-tool kernel build-record`: --self-test, and
#                               `finish` on WORK into two scratch folders (the record, series.txt, the kernel, the
#                               link and stdout compared)
#             compile-command   drivers/build.py's command() against `oneios-tool kernel compile-command`
#                               (tests/kernelgen/drivers-command.py, deleted with drivers/build.py), for a C and a
#                               C++ source with each of WORK's three records
#             layout-hid, layout-net, port-legacy-display   (not in the default list) the records made from the
#                               kernelcache: hid/layout.py, net/layout.py and display/port-legacy-display.py against
#                               `oneios-tool kernel layout hid|net` and `kernel port-legacy-display`.
#                               KERNELCACHE=FILE (default out/kernel-inputs/24A446/kernelcache.macho),
#                               PRELINK_REPORT=FILE (default out/kernel/24A446/kernel.kc.prelink.json)
#   ONEIOS_TOOL   the binary (default: tools/oneios-tool/target/release/oneios-tool)
#   SCRATCH       where the two sides' outputs are kept (default: out/kernelgen-compare)
#
# One expected difference, in the objects.json of audio and of net: the Python script lists itself (its copy in
# the tree) as the last source; the Rust command lists the staged source there (the upstream file with the script's
# edits applied). The script checks that this entry is the only difference.
# The tree is left with the Rust outputs. Exit status: 0 all the same, 1 a difference.
set -uo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
[ $# -ge 2 ] || { sed -n '2,20p' "$0" >&2; exit 2; }
WORK=$(cd "$1" && pwd); ENVFILE=$(cd "$(dirname "$2")" && pwd)/$(basename "$2"); shift 2
TREE=$WORK/tree
TOOL=${ONEIOS_TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
SCRATCH=${SCRATCH:-$ROOT/out/kernelgen-compare}
[ -x "$TOOL" ] || { echo "compare-python: no $TOOL (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)" >&2; exit 2; }
[ -d "$TREE/build/xnu" ] && [ -s "$ENVFILE" ] || { echo "compare-python: $TREE has no build, or $ENVFILE is empty" >&2; exit 2; }
# (the default was every generator, build-record and compile-command while the scripts were there; then
# compile-command alone, until drivers/build.py went too)
[ $# -gt 0 ] || set -- compile-command
H=$ROOT/kernel/patches/oneios-hosted

script_of() {
  case "$1" in
    board) echo "$ROOT/kernel/device/scripts/build-oneios-board.py" ;;
    storage) echo "$H/storage/build-oneios-hosted-storage.py" ;;
    *) echo "$H/$1/build-oneios-$1.py" ;;
  esac
}
dirs_of() {
  case "$1" in
    board) echo "oneios-drivers oneios-board" ;;
    storage) echo "oneios-hosted-storage" ;;
    display) echo "native-legacy-display" ;;
    *) echo "oneios-$1" ;;
  esac
}

# in_step CMD...: CMD in the archive step's environment (ENVFILE) and directory (the tree), with UniXNU's /usr/bin
# shims where the host has none (as kernel/build-hosted.sh's run_build).
in_step() {
  local wrap=() shim toolchain
  if [ "$(uname -s)" = Linux ] && ! { [ -x /usr/bin/xcrun ] && [ -x /usr/bin/plutil ] && [ -x /usr/bin/codesign ] && [ -x /bin/csh ]; }; then
    toolchain=$(tr '\0' '\n' < "$ENVFILE" | sed -n 's/^UNIXNU_TOOLCHAIN=//p')
    wrap=(bwrap --dev-bind / / --overlay-src /usr/bin --tmp-overlay /usr/bin)
    for shim in xcrun plutil codesign csh; do
      [ -e "/usr/bin/$shim" ] || wrap+=(--ro-bind "$toolchain/usrbin/$shim" "/usr/bin/$shim")
    done
    wrap+=(--)
  fi
  "${wrap[@]}" env -i ENVFILE="$ENVFILE" bash --noprofile --norc -c '
    while IFS= read -r -d "" kv; do
      name=${kv%%=*}
      case "$name" in ""|*[!A-Za-z0-9_]*|[0-9]*) ;; *) export "$kv" ;; esac
    done < "$ENVFILE"
    unset ENVFILE; cd "$1" && shift && exec "$@"' bash "$TREE" "$@"
}

# The objects.json of audio and net without its last source, and that source alone.
without_last() { python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); d["sources"].pop(); print(json.dumps(d, indent=2))' "$1"; }
last_source() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["sources"][-1]["path"])' "$1"; }

# kernel/build-record.py against the Rust command. Both read WORK; each writes its own OUT.
compare_build_record() {
  local py=$SCRATCH/py/build-record rs=$SCRATCH/rs/build-record host=linux same=yes differences
  [ "$(uname -s)" = Linux ] || host=macos
  [ -f "$ROOT/kernel/build-record.py" ] || { echo "compare-python: kernel/build-record.py is not there (retired)" >&2; exit 2; }
  rm -rf "$py" "$rs"; mkdir -p "$py" "$rs"
  python3 -I "$ROOT/kernel/build-record.py" --self-test > "$py/self-test" 2>&1; echo $? >> "$py/self-test"
  "$TOOL" kernel build-record --self-test > "$rs/self-test" 2>&1; echo $? >> "$rs/self-test"
  python3 -I "$ROOT/kernel/build-record.py" finish --host $host --work "$WORK" --config oneios_hosted_defconfig --out "$py/out" > "$py/stdout" 2> "$py/stderr"
  echo $? > "$py/status"
  "$TOOL" kernel build-record finish --host $host --work "$WORK" --config oneios_hosted_defconfig --out "$rs/out" --repo "$ROOT" > "$rs/stdout" 2> "$rs/stderr"
  echo $? > "$rs/status"
  # stdout names the folder written to: taken out before the comparison
  sed "s#$py/##" "$py/stdout" > "$py/stdout.said" && mv "$py/stdout.said" "$py/stdout"
  sed "s#$rs/##" "$rs/stdout" > "$rs/stdout.said" && mv "$rs/stdout.said" "$rs/stdout"
  readlink "$py/out/current" > "$py/current"; readlink "$rs/out/current" > "$rs/current"
  differences=$(diff -r --no-dereference -q "$py" "$rs" 2>&1)
  [ -z "$differences" ] || { same=NO; fail=1; }
  printf '%-9s %-6s %7s %7s  %s\n' build-rec "$same" - - "$(cd "$py" && find . -type f | wc -l) files, status $(cat "$py/status") and $(cat "$rs/status"), $(tail -1 "$py/current")"
  [ -z "$differences" ] || echo "$differences" | sed 's/^/    /' | head -20
}

# drivers/build.py's command() against this crate's copy.
compare_compile_command() {
  [ -f "$ROOT/drivers/build.py" ] || { echo "compare-python: drivers/build.py is not there (retired: tests/kernelpy/compare-golden.sh)" >&2; exit 2; }
  local records=$TREE/build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837 same=yes count=0 name source py rs
  for name in osfmk/DEVELOPMENT/startup.o.json bsd/DEVELOPMENT/kern_control.o.json iokit/DEVELOPMENT/IOPlatformExpert.cpo.json; do
    for source in /some/where/a.c /some/where/B.cpp; do
      count=$((count + 1))
      py=$(python3 -I "$HERE/drivers-command.py" "$ROOT/drivers/build.py" "$records" "$name" "$source" /an/object.o)
      rs=$("$TOOL" kernel compile-command "$records/$name" "$source" /an/object.o --repo "$ROOT")
      [ -n "$py" ] && [ "$py" = "$rs" ] || { same=NO; fail=1; echo "    $name $source: the commands differ"; }
    done
  done
  printf '%-9s %-6s %7s %7s  %s\n' command "$same" - - "$count commands"
}

# The records made from the kernelcache: hid/layout.py, net/layout.py and display/port-legacy-display.py against
# `oneios-tool kernel layout hid|net` and `kernel port-legacy-display`. Each side writes its record into its own
# scratch folder; the record and stdout are compared, and the record with the committed one.
compare_record() {   # NAME SCRIPT COMMITTED RUST-COMMAND...
  local name=$1 script=$2 committed=$3 same=yes note differences t0 t_py t_rs; shift 3
  local py=$SCRATCH/py/$name rs=$SCRATCH/rs/$name file
  file=$(basename "$committed")
  [ -f "$script" ] || { echo "compare-python: $script is not there (retired)" >&2; exit 2; }
  [ -f "$KERNELCACHE" ] || { echo "compare-python: KERNELCACHE=$KERNELCACHE is not there (scripts/image/kernel-inputs.sh)" >&2; exit 2; }
  rm -rf "$py" "$rs"; mkdir -p "$py" "$rs"
  t0=$(date +%s.%N)
  if [ "$name" = port-legacy-display ]; then
    python3 -I "$script" --xnuprelink "$ROOT/external/XnuPrelink" --kernelcache "$KERNELCACHE" --out "$py/$file" > "$py/stdout" 2> "$py/stderr"
  else
    python3 -I "$script" --kernelcache "$KERNELCACHE" --prelink-report "$PRELINK_REPORT" --out "$py/$file" > "$py/stdout" 2> "$py/stderr"
  fi
  echo $? > "$py/status"; t_py=$(echo "$(date +%s.%N) $t0" | awk '{printf "%.1f", $1 - $2}'); t0=$(date +%s.%N)
  if [ "$name" = port-legacy-display ]; then
    "$TOOL" kernel "$@" --kernelcache "$KERNELCACHE" --out "$rs/$file" --repo "$ROOT" > "$rs/stdout" 2> "$rs/stderr"
  else
    "$TOOL" kernel "$@" --kernelcache "$KERNELCACHE" --prelink-report "$PRELINK_REPORT" --out "$rs/$file" --repo "$ROOT" > "$rs/stdout" 2> "$rs/stderr"
  fi
  echo $? > "$rs/status"; t_rs=$(echo "$(date +%s.%N) $t0" | awk '{printf "%.1f", $1 - $2}')
  sed "s#$py/##" "$py/stdout" > "$py/stdout.said" && mv "$py/stdout.said" "$py/stdout"
  sed "s#$rs/##" "$rs/stdout" > "$rs/stdout.said" && mv "$rs/stdout.said" "$rs/stdout"
  differences=$(diff -r -q "$py" "$rs" 2>&1)
  [ -z "$differences" ] || { same=NO; fail=1; }
  if cmp -s "$rs/$file" "$committed"; then note="the committed $file"; else note="NOT the committed $file"; fi
  printf '%-9s %-6s %6ss %6ss  %s\n' "${name:0:9}" "$same" "$t_py" "$t_rs" "status $(cat "$py/status") and $(cat "$rs/status"), $note"
  [ -z "$differences" ] || echo "$differences" | sed 's/^/    /' | head -20
}
KERNELCACHE=${KERNELCACHE:-$ROOT/out/kernel-inputs/24A446/kernelcache.macho}
PRELINK_REPORT=${PRELINK_REPORT:-$ROOT/out/kernel/24A446/kernel.kc.prelink.json}

fail=0
printf '%-9s %-6s %7s %7s  %s\n' NAME SAME PYTHON RUST FILES
for name in "$@"; do
  case "$name" in
    build-record) compare_build_record; continue ;;
    compile-command) compare_compile_command; continue ;;
    layout-hid) compare_record layout-hid "$H/hid/layout.py" "$H/hid/IOHIDDevice-24A446-v54.json" layout hid; continue ;;
    layout-net) compare_record layout-net "$H/net/layout.py" "$H/net/IONetworking-24A446-v54.json" layout net; continue ;;
    port-legacy-display) compare_record port-legacy-display "$H/display/port-legacy-display.py" "$H/display/legacy-display-24A446-v54.json" port-legacy-display; continue ;;
  esac
  script=$(script_of "$name"); dirs=$(dirs_of "$name")
  [ -f "$script" ] || { echo "compare-python: $script is not there (retired: use compare-golden.sh)" >&2; exit 2; }
  cp "$script" "$TREE/scripts/"
  for side in py rs; do
    for d in $dirs; do rm -rf "$TREE/build/$d"; done
    rm -rf "$SCRATCH/$side/$name"; mkdir -p "$SCRATCH/$side/$name"
    t0=$(date +%s.%N)
    if [ $side = py ]; then
      in_step python3 "$TREE/scripts/$(basename "$script")" > "$SCRATCH/$side/$name/stdout" 2> "$SCRATCH/$side/$name/stderr"
    else
      in_step "$TOOL" kernel gen "$name" > "$SCRATCH/$side/$name/stdout" 2> "$SCRATCH/$side/$name/stderr"
    fi
    echo $? > "$SCRATCH/$side/$name/status"
    eval "t_$side=$(echo "$(date +%s.%N) $t0" | awk '{printf "%.1f", $1 - $2}')"
    for d in $dirs; do [ -e "$TREE/build/$d" ] && cp -a "$TREE/build/$d" "$SCRATCH/$side/$name/"; done
  done
  same=yes; note=
  files=$(cd "$SCRATCH/py/$name" && find . \( -type f -o -type l \) | wc -l)
  differences=$(diff -r --no-dereference -q "$SCRATCH/py/$name" "$SCRATCH/rs/$name" 2>&1)
  case "$name" in audio|net)
    d=$(dirs_of "$name")
    listed=$(echo "$differences" | grep -c . || true)
    if [ "$listed" = 1 ] && echo "$differences" | grep -q "/$d/objects.json " &&
       [ "$(without_last "$SCRATCH/py/$name/$d/objects.json")" = "$(without_last "$SCRATCH/rs/$name/$d/objects.json")" ]; then
      differences=; note=" (objects.json: the last source is $(last_source "$SCRATCH/rs/$name/$d/objects.json"), the script's is $(last_source "$SCRATCH/py/$name/$d/objects.json"))"
    fi ;;
  esac
  [ -z "$differences" ] || { same=NO; fail=1; }
  printf '%-9s %-6s %6ss %6ss  %s files, status %s and %s%s\n' "$name" "$same" "$t_py" "$t_rs" "$files" \
    "$(cat "$SCRATCH/py/$name/status")" "$(cat "$SCRATCH/rs/$name/status")" "$note"
  [ -z "$differences" ] || echo "$differences" | sed 's/^/    /' | head -20
done
exit $fail
