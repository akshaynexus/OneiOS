#!/bin/bash
# tools/oneios-tool/tests/compare-python.sh   every comparison of oneios-tool with the tool it replaces, one after
# the other; each part runs when its inputs are named, and says so when they are not.
#
#   part      script                              inputs (environment)
#   units     cargo test                          none
#   parity    tests/image/parity.py               none (stand-in files)
#   image     tests/image/compare-python.sh       SET=<image set folder> (or the set in out/image/<build>)
#   apfs      tests/image/apfs-read.sh, apfs-fuse.sh   APFS_IMAGES="IMAGE[:VOLUME] ..." and apfs-fuse (Linux)
#   sym       tests/sym/compare-golden.sh         ONEIOS_DSC_DIR (the 24A446 cache; the script makes its logs)
#   log       tests/log/run.sh                    ONEIOS_LOG_DIR or ONEIOS_LOGS
#   ioslog    tests/ioslog/compare.sh             REF_ITER=<unifiedlog_iterator>, IOSLOG_ARCHIVES="A.logarchive ..."
#   prelink   tests/prelink/compare-golden.sh     KERNELCACHE (default out/kernel-inputs/24A446/kernelcache.macho);
#                                                 KERNEL and REFERENCE for the whole chain (a golden exists for two kernels)
#   phone     tests/phone/run.sh                  none (a stand-in phone; scripts/phone/stage.sh is the reference)
#   app       tests/app/run.sh                    none (a stand-in OneOS checkout, gradlew, gh and phone)
#   cycle     tests/cycle/run.sh                  the OneOS fork's checkout (its root script); tmux, zstd
#   ksym      tests/ksym/compare.sh               KSYM="KC KERNEL [LOG]" (default: the newest out/kernel/*/kernel.kc
#                                                 and the xnu.macho its record names); llvm-symbolizer, dsymutil
#
# Usage: compare-python.sh [PART...]   (default: all). Exit 0 when every part that ran passed; a part without its
# inputs is "skipped" and does not fail the run. Each script's header has its own options.
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
export PATH=$HOME/.cargo/bin:$PATH
parts=${*:-units parity image apfs sym log ioslog prelink phone app cycle}
[ $# -eq 0 ] && parts="$parts ksym"
failed="" skipped="" passed=""
run() {  # run PART COMMAND...
  local part=$1; shift
  echo "======== $part"
  if "$@"; then passed="$passed $part"; else failed="$failed $part"; fi
}
skip() { echo "======== $1: skipped ($2)"; skipped="$skipped $1"; }
for part in $parts; do
  case "$part" in
  phone) run phone "$here/phone/run.sh" ;;
  app) run app "$here/app/run.sh" ;;
  cycle) run cycle "$here/cycle/run.sh" ;;
  units) run units cargo test --release --manifest-path "$root/tools/oneios-tool/Cargo.toml" ;;
  parity) run parity python3 "$here/image/parity.py" ;;
  image)
    if [ -n "${SET:-}" ] || ls "$root"/out/image/*/manifest.json >/dev/null 2>&1; then run image "$here/image/compare-python.sh"
    else skip image "no image set: SET=FOLDER"; fi ;;
  apfs)
    if [ -z "${APFS_IMAGES:-}" ]; then skip apfs "APFS_IMAGES is not set"
    elif ! command -v apfs-fuse >/dev/null; then skip apfs "no apfs-fuse"
    else
      ok=0
      for item in $APFS_IMAGES; do
        "$here/image/apfs-read.sh" "${item%%:*}" "$([ "$item" = "${item#*:}" ] && echo 0 || echo "${item#*:}")" || ok=1
        "$here/image/apfs-fuse.sh" "${item%%:*}" "$([ "$item" = "${item#*:}" ] && echo 0 || echo "${item#*:}")" || ok=1
      done
      run apfs test $ok = 0
    fi ;;
  sym)
    if [ -n "${ONEIOS_DSC_DIR:-}" ]; then run sym "$here/sym/compare-golden.sh"
    else skip sym "ONEIOS_DSC_DIR is not set"; fi ;;
  log)
    if [ -n "${ONEIOS_LOG_DIR:-}${ONEIOS_LOGS:-}" ]; then run log "$here/log/run.sh"
    else skip log "ONEIOS_LOG_DIR or ONEIOS_LOGS is not set"; fi ;;
  ioslog)
    if [ -z "${REF_ITER:-}" ] || [ -z "${IOSLOG_ARCHIVES:-}" ]; then skip ioslog "REF_ITER and IOSLOG_ARCHIVES are not both set"
    else
      ok=0
      for archive in $IOSLOG_ARCHIVES; do "$here/ioslog/compare.sh" "$archive" || ok=1; done
      run ioslog test $ok = 0
    fi ;;
  prelink)
    if [ -f "${KERNELCACHE:-$root/out/kernel-inputs/24A446/kernelcache.macho}" ]; then run prelink "$here/prelink/compare-golden.sh"
    else skip prelink "no kernelcache: scripts/image/kernel-inputs.sh, or KERNELCACHE=FILE"; fi ;;
  ksym)
    # sym kernel against llvm-symbolizer: KSYM="KC KERNEL [LOG]", else the pair `sym kernel` finds under out/kernel.
    if ! command -v llvm-symbolizer >/dev/null; then skip ksym "no llvm-symbolizer"
    elif [ -z "${KSYM:-}" ] && ! ls "$root"/out/kernel/*/kernel.kc >/dev/null 2>&1; then skip ksym "no out/kernel/*/kernel.kc: KSYM=\"KC KERNEL [LOG]\""
    else run ksym "$here/ksym/compare.sh" ${KSYM:-}; fi ;;
  *) echo "compare-python: no part $part" >&2; exit 2 ;;
  esac
done
echo "======== passed:${passed:- none}; skipped:${skipped:- none}; failed:${failed:- none}"
[ -z "$failed" ]
