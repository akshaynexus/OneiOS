#!/bin/sh
# Can Android load every dynamic ELF file of the runtime? The NEEDED and undefined-symbol check against the NDK's stubs.
#   runtime/package/elf-check.sh [--api N] PATH...     PATH: a stage (every file under it is looked at) or one file
# For each arm64 ELF file with a dynamic section (a shared object, or an executable that is not static):
#   - every NEEDED library must be a stub of the NDK for API N (sysroot/usr/lib/aarch64-linux-android/N/*.so: what an
#     Android of that level has for sure) or a library among PATH (one the runtime ships: by its SONAME, or by its
#     file name when that ends in .so);
#   - every undefined dynamic symbol must be defined by one of the file's own NEEDED libraries (a versioned
#     reference NAME@VERSION by that version of it). Weak references are not checked: Android loads a file with
#     an unresolved weak symbol.
# A failure is one line on stderr, and the exit code is 1:
#   LIB: symbol SYM is in none of its NEEDED libraries (NEEDED: ...); it is in LIBX
#   LIB: NEEDED library LIBY is not an API N stub of the NDK and not a library the runtime ships
# Why: a shared object links with symbols left undefined unless the link has -z defs (runtime/host.mk's
# ANDROID_SO_LDFLAGS), and Android's linker then refuses the file at load time. liboneios-display.so of 1f7dd1f
# called ANativeWindow_getBuffersDataSpace (libnativewindow's) and linked -landroid only: the build passed and the
# phone said `dlopen failed: cannot locate symbol "ANativeWindow_getBuffersDataSpace"` (2026-11-14). The link flag
# stops that at the link; this check stops it in a stage, however the file was made (runtime/package.sh runs it and
# refuses the package; runtime/package/elf-check-test.sh tests it; docs/building.md section 4).
# Reads with the NDK's llvm-readelf and llvm-nm. NDK: $NDK, else $ANDROID_NDK_ROOT, else $ANDROID_NDK_HOME, else the
# default place of runtime/host.mk. N: 30 (the API level every runtime Makefile compiles for).
# Exit: 0 all good, 1 a failure, 2 a wrong command line or no NDK.
set -eu
LC_ALL=C; export LC_ALL
usage() { echo "usage: runtime/package/elf-check.sh [--api N] PATH..." >&2; exit 2; }
API=30
while [ $# -gt 0 ]; do
  case "$1" in
    --api) [ $# -ge 2 ] || usage; API=$2; shift 2 ;;
    --) shift; break ;;
    -*) usage ;;
    *) break ;;
  esac
done
[ $# -ge 1 ] || usage
case "$(uname -s)" in
  Darwin) NDK_HOST=darwin-x86_64; NDK_DEFAULT=$HOME/Library/Android/sdk/ndk/28.2.13676358 ;;
  *)      NDK_HOST=linux-x86_64;  NDK_DEFAULT=$HOME/Android/Sdk/ndk/28.2.13676358 ;;
esac
NDK=${NDK:-${ANDROID_NDK_ROOT:-${ANDROID_NDK_HOME:-$NDK_DEFAULT}}}
LLVM=$NDK/toolchains/llvm/prebuilt/$NDK_HOST
READELF=$LLVM/bin/llvm-readelf
NM=$LLVM/bin/llvm-nm
STUBS=$LLVM/sysroot/usr/lib/aarch64-linux-android/$API
if [ ! -x "$READELF" ] || [ ! -x "$NM" ] || [ ! -f "$STUBS/libc.so" ]; then
  echo "elf-check: no llvm-readelf, llvm-nm or API $API stubs in the NDK at $NDK ($NDK_HOST)" >&2; exit 2
fi
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
mkdir "$W/stub" "$W/ship"
TAB=$(printf '\t')
failures=0
fail() { failures=$((failures + 1)); echo "$*" >&2; }

# The first 20 bytes in hex: 7f454c46 (ELF) 02 (64-bit) 01 (little-endian) ... b700 (e_machine: AArch64).
head_of() { od -An -tx1 -N20 "$1" 2>/dev/null | tr -d ' \n'; }
is_elf() { case "$(head_of "$1")" in 7f454c46*) return 0 ;; esac; return 1; }
# FILE OUT: what FILE defines for the dynamic linker, one per line: NAME, and NAME@VERSION when it has a version.
defs_of() {
  [ -f "$2" ] && return 0
  "$NM" -D --defined-only -P "$1" 2>/dev/null |
    awk '{ n = $1; i = index(n, "@"); if (i) { print substr(n, 1, i - 1); sub(/@@/, "@", n) } print n }' | sort -u > "$2"
}

# 1. The files: NAME (as the messages print it) and its path.
: > "$W/files"
for arg in "$@"; do
  if [ -d "$arg" ]; then
    dir=${arg%/}
    find "$dir" -type f | sort | while IFS= read -r f; do printf '%s\t%s\n' "${f#"$dir"/}" "$f"; done >> "$W/files"
  elif [ -f "$arg" ]; then
    printf '%s\t%s\n' "$arg" "$arg" >> "$W/files"
  else
    echo "elf-check: no file or folder $arg" >&2; exit 2
  fi
done

# 2. The ELF files among them: the dynamic ones (elfs: index, name, path) and the libraries the runtime ships (ships).
: > "$W/elfs"; : > "$W/ships"
count=0; dynamic=0; static=0
while IFS="$TAB" read -r name path; do
  kind=$(head_of "$path")
  case "$kind" in 7f454c46*) ;; *) continue ;; esac
  case "$kind" in
    7f454c460201*b700) ;;
    *) fail "$name: an ELF file that is not 64-bit little-endian AArch64"; continue ;;
  esac
  count=$((count + 1))
  if ! "$READELF" -d "$path" > "$W/dyn.$count" 2> /dev/null; then fail "$name: llvm-readelf cannot read it"; continue; fi
  if ! grep -q '^Dynamic section' "$W/dyn.$count"; then static=$((static + 1)); continue; fi
  dynamic=$((dynamic + 1))
  printf '%s\t%s\t%s\n' "$count" "$name" "$path" >> "$W/elfs"
  soname=$(sed -n 's/^.*(SONAME).*\[\(.*\)\][[:space:]]*$/\1/p' "$W/dyn.$count")
  base=${path##*/}
  case "$base" in *.so|*.so.*) ;; *) base= ;; esac
  for lib in "$soname" "$base"; do
    case "$lib" in ''|*/*) continue ;; esac
    [ -n "$(awk -F "$TAB" -v lib="$lib" '$1 == lib' "$W/ships")" ] || printf '%s\t%s\n' "$lib" "$path" >> "$W/ships"
  done
done < "$W/files"

# Where a symbol lives, for the message: every stub of this API level and every shipped library that defines it.
# Called in a command substitution: what it indexed is kept in files, its variables are its own.
where_is() {
  if [ ! -f "$W/indexed" ]; then
    for f in "$STUBS"/*.so; do
      if is_elf "$f"; then defs_of "$f" "$W/stub/${f##*/}"; fi
    done
    while IFS="$TAB" read -r lib path; do defs_of "$path" "$W/ship/$lib"; done < "$W/ships"
    : > "$W/indexed"
  fi
  for d in "$W/stub" "$W/ship"; do
    for f in "$d"/*; do
      if [ -f "$f" ] && grep -qxF -e "$1" "$f"; then printf '%s\n' "${f##*/}"; fi
    done
  done | sort -u | tr '\n' ' ' | sed 's/ $//; s/ /, /g'
}

# 3. Each dynamic file: its NEEDED libraries, then its undefined symbols against what those define.
while IFS="$TAB" read -r index name path; do
  sed -n 's/^.*(NEEDED).*\[\(.*\)\][[:space:]]*$/\1/p' "$W/dyn.$index" > "$W/needed"
  : > "$W/have"
  bad=0
  while IFS= read -r lib; do
    case "$lib" in */*) lib_file= ;; *) lib_file=$STUBS/$lib ;; esac
    shipped=$(awk -F "$TAB" -v lib="$lib" '$1 == lib { print $2; exit }' "$W/ships")
    if [ -n "$lib_file" ] && [ -f "$lib_file" ] && is_elf "$lib_file"; then
      defs_of "$lib_file" "$W/stub/$lib"; cat "$W/stub/$lib" >> "$W/have"
    elif [ -n "$shipped" ]; then
      defs_of "$shipped" "$W/ship/$lib"; cat "$W/ship/$lib" >> "$W/have"
    else
      fail "$name: NEEDED library $lib is not an API $API stub of the NDK and not a library the runtime ships"; bad=1
    fi
  done < "$W/needed"
  sort -u "$W/have" > "$W/have.sorted"
  needed=$(tr '\n' ' ' < "$W/needed" | sed 's/ $//; s/ /, /g')
  "$NM" -D --undefined-only -P "$path" 2> /dev/null > "$W/undefined" || { fail "$name: llvm-nm cannot read it"; continue; }
  awk '$2 == "U" { print $1 }' "$W/undefined" | sort -u > "$W/strong"
  weak=$(awk '$2 != "U"' "$W/undefined" | wc -l | tr -d ' ')
  comm -23 "$W/strong" "$W/have.sorted" > "$W/missing"
  while IFS= read -r symbol; do
    where=$(where_is "${symbol%%@*}")
    if [ -n "$where" ]; then where="it is in $where"; else where="no API $API stub and no library the runtime ships has it"; fi
    fail "$name: symbol $symbol is in none of its NEEDED libraries (NEEDED: ${needed:-none}); $where"
    bad=1
  done < "$W/missing"
  if [ "$bad" = 0 ]; then
    echo "elf-check: $name: NEEDED ${needed:-none}; $(wc -l < "$W/strong" | tr -d ' ') undefined symbols, each defined by one of them$([ "$weak" = 0 ] || echo " (and $weak weak, not checked)")"
  fi
done < "$W/elfs"

echo "elf-check: $dynamic dynamic ELF files checked against the NDK's API $API stubs, $static static ones need none: $failures failures"
[ "$failures" = 0 ]
