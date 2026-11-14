#!/bin/bash
# Host tests of the runtime's link rule for Android shared objects (runtime/host.mk: ANDROID_SO_LDFLAGS, -z defs)
# and of runtime/package/elf-check.sh (every NEEDED library an API 30 stub or shipped, every undefined symbol in a
# NEEDED library). Needs the NDK, and what runtime/build.sh needs (a JDK and d8 for the display helper's dex).
#   runtime/package/elf-check-test.sh
# The fault they exist for (the phone, 2026-11-14): liboneios-display.so of 1f7dd1f called
# ANativeWindow_getBuffersDataSpace and _setBuffersDataSpace (libnativewindow's) and linked `-landroid -lm` only.
# The link passed; app_process could not load the library. The tests make that library again, in a temporary folder:
# with the rule's flags its link must fail, without them elf-check.sh must fail and name libnativewindow.so, and the
# library as f7bb1c3 links it must pass both. They also check that executables fail to link on an undefined symbol
# with no flag at all, the NEEDED rule, and a whole stage of runtime/build.sh.
# NDK: $NDK, else $ANDROID_NDK_ROOT, else $ANDROID_NDK_HOME, else runtime/host.mk's default.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
case "$(uname -s)" in
  Darwin) NDK_HOST=darwin-x86_64; NDK_DEFAULT=$HOME/Library/Android/sdk/ndk/28.2.13676358 ;;
  *)      NDK_HOST=linux-x86_64;  NDK_DEFAULT=$HOME/Android/Sdk/ndk/28.2.13676358 ;;
esac
export NDK=${NDK:-${ANDROID_NDK_ROOT:-${ANDROID_NDK_HOME:-$NDK_DEFAULT}}}
CC=$NDK/toolchains/llvm/prebuilt/$NDK_HOST/bin/aarch64-linux-android30-clang
[ -x "$CC" ] || { echo "elf-check-test: no NDK at $NDK ($NDK_HOST)" >&2; exit 1; }
CHECK=$ROOT/runtime/package/elf-check.sh
FAULT_C=$ROOT/runtime/package/elf-fault.c
DISPLAY=$ROOT/runtime/display
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
pass=0
ok() { pass=$((pass + 1)); echo "ok $pass: $*"; }

link_fails() { # NAME SYMBOL COMMAND...: the link must fail, and on SYMBOL
  local name=$1 symbol=$2; shift 2
  if "$@" > "$T/log" 2>&1; then fail "$name: the link passed"; fi
  grep -q "undefined symbol: $symbol\$" "$T/log" || { cat "$T/log"; fail "$name: the link failed, but not on $symbol"; }
}
links() { # NAME COMMAND...
  local name=$1; shift
  "$@" > "$T/log" 2>&1 || { cat "$T/log"; fail "$name: the link failed"; }
}
check_fails() { # NAME PATH LINE...: elf-check.sh PATH must end with exit 1 and print every LINE
  local name=$1 path=$2 rc=0 line; shift 2
  "$CHECK" "$path" > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 1 ] || { cat "$T/log"; fail "$name: elf-check.sh ended with $rc, not 1"; }
  for line in "$@"; do
    grep -qxF -- "$line" "$T/log" || { cat "$T/log"; fail "$name: elf-check.sh did not print: $line"; }
  done
}
check_passes() { # NAME PATH...
  local name=$1; shift
  "$CHECK" "$@" > "$T/log" 2>&1 || { cat "$T/log"; fail "$name: elf-check.sh failed"; }
}
missing() { # LIB SYMBOL NEEDED WHERE: elf-check.sh's line for a symbol that no NEEDED library has
  echo "$1: symbol $2 is in none of its NEEDED libraries (NEEDED: $3); $4"
}

# 1. The rule: host.mk's flags have -z defs, and every `-shared` link line of a runtime Makefile uses them.
printf 'include %s\nprint:\n\t@echo $(ANDROID_SO_LDFLAGS)\n' "$ROOT/runtime/host.mk" > "$T/print.mk"
SO_LDFLAGS=$(make -s --no-print-directory -f "$T/print.mk" NDK="$NDK" print)
case " $SO_LDFLAGS " in *" -Wl,-z,defs "*) ;; *) fail "runtime/host.mk: ANDROID_SO_LDFLAGS is '$SO_LDFLAGS': no -Wl,-z,defs" ;; esac
read -r -a SO_FLAGS <<< "$SO_LDFLAGS"
shared=0
while IFS= read -r line; do
  shared=$((shared + 1))
  case "$line" in *'$(ANDROID_SO_LDFLAGS)'*) ;; *) fail "a -shared link without \$(ANDROID_SO_LDFLAGS): $line" ;; esac
done < <(cd "$ROOT" && find runtime \( -name Makefile -o -name '*.mk' \) -not -path '*/build/*' -print0 | sort -z |
           xargs -0 grep -nE '(^|[[:space:]])-shared([[:space:]]|$)' /dev/null | grep -vE '^[^:]+:[0-9]+:[[:space:]]*#' || true)
[ "$shared" -ge 1 ] || fail "no -shared link line found in the runtime Makefiles (liboneios-display.so has one)"
ok "host.mk: ANDROID_SO_LDFLAGS = $SO_LDFLAGS; the $shared -shared link line(s) of the runtime Makefiles use it"

# 2. liboneios-display.so itself, by its own Makefile rule, while its source still calls the dataspace functions.
getter=ANativeWindow_getBuffersDataSpace
setter=ANativeWindow_setBuffersDataSpace
old_needed='libandroid.so, libm.so, libdl.so, libc.so'
display_make() { # FOLDER VARIABLE=VALUE...: the rule of liboneios-display.so, into FOLDER
  local dir=$1; shift
  make -s -C "$DISPLAY" NDK="$NDK" A="$dir" "$@" "$dir/liboneios-display.so"
}
if grep -q "$getter" "$DISPLAY/src/jni.c" && grep -q "$setter" "$DISPLAY/src/jni.c"; then
  link_fails "1f7dd1f's libraries with the rule's flags" "$getter" display_make "$T/old-defs" DISPLAY_LIBS='-landroid -lm'
  grep -q "undefined symbol: $setter\$" "$T/log" || fail "the link did not name $setter"
  [ ! -e "$T/old-defs/liboneios-display.so" ] || fail "the failed link left a library"
  ok "liboneios-display.so with 1f7dd1f's libraries (-landroid -lm) and -z defs: the link fails on $getter and $setter"
  links "1f7dd1f's link" display_make "$T/old" DISPLAY_LIBS='-landroid -lm' ANDROID_SO_LDFLAGS=
  lib=$T/old/liboneios-display.so
  check_fails "1f7dd1f's library" "$lib" \
    "$(missing "$lib" "$getter" "$old_needed" 'it is in libnativewindow.so')" \
    "$(missing "$lib" "$setter" "$old_needed" 'it is in libnativewindow.so')"
  ok "liboneios-display.so as 1f7dd1f linked it (no -z defs): it links, and elf-check.sh fails: $(grep -m1 -o "symbol $getter.*" "$T/log")"
  links "f7bb1c3's link" display_make "$T/new"
  check_passes "f7bb1c3's library" "$T/new/liboneios-display.so"
  grep -q 'NEEDED libandroid.so, libnativewindow.so, libm.so, libdl.so, libc.so; ' "$T/log" || { cat "$T/log"; fail "f7bb1c3's library: other NEEDED libraries"; }
  ok "liboneios-display.so as the Makefile links it (f7bb1c3: -lnativewindow, -z defs): it links, and elf-check.sh passes"
else
  echo "runtime/display/src/jni.c no longer calls $getter and $setter: its three tests are skipped (elf-fault.c below does the same)"
fi

# 3. The same fault in a library of its own (elf-fault.c), which does not depend on the display helper's source.
mkdir "$T/f"
so=(-std=gnu11 -O2 -Wall -Wextra -Werror -fPIC -shared -Wl,-soname,libfault.so)
link_fails "elf-fault.c, -landroid, the rule's flags" "$getter" "$CC" "${so[@]}" "${SO_FLAGS[@]}" "$FAULT_C" -landroid -lm -o "$T/f/defs.so"
ok "a shared object that calls $getter with -landroid only and -z defs: the link fails"
links "elf-fault.c, -landroid" "$CC" "${so[@]}" "$FAULT_C" -landroid -lm -o "$T/f/libfault.so"
check_fails "elf-fault.c, -landroid" "$T/f/libfault.so" \
  "$(missing "$T/f/libfault.so" "$getter" "$old_needed" 'it is in libnativewindow.so')" \
  "$(missing "$T/f/libfault.so" "$setter" "$old_needed" 'it is in libnativewindow.so')"
ok "the same without -z defs: it links, and elf-check.sh fails and names libnativewindow.so"
links "elf-fault.c, -lnativewindow" "$CC" "${so[@]}" "${SO_FLAGS[@]}" "$FAULT_C" -landroid -lnativewindow -lm -o "$T/f/good.so"
check_passes "elf-fault.c, -lnativewindow" "$T/f/good.so"
ok "the same with -lnativewindow and -z defs: it links, and elf-check.sh passes"

# 4. Executables need no flag: an undefined symbol fails the link of a dynamic and of a static one.
printf 'extern int oneios_no_such_symbol(void);\nint oneios_user(void);\nint oneios_user(void) { return oneios_no_such_symbol(); }\n' > "$T/nosuch.c"
printf 'int oneios_user(void);\nint main(void) { return oneios_user(); }\n' > "$T/main.c"
link_fails "a dynamic executable, -landroid" "$getter" "$CC" -DELF_FAULT_MAIN "$FAULT_C" -landroid -o "$T/f/exe"
link_fails "a dynamic executable" oneios_no_such_symbol "$CC" "$T/main.c" "$T/nosuch.c" -o "$T/f/exe"
link_fails "a static executable" oneios_no_such_symbol "$CC" -static "$T/main.c" "$T/nosuch.c" -o "$T/f/exe"
ok "executables with no flag: a dynamic one that calls $getter with -landroid only, a dynamic and a static one with a symbol nothing has: every link fails"
mkdir "$T/e"
links "a dynamic executable, -lnativewindow" "$CC" -DELF_FAULT_MAIN "$FAULT_C" -landroid -lnativewindow -o "$T/e/exe"
check_passes "a dynamic executable, -lnativewindow" "$T/e/exe"
grep -q '^elf-check: 1 dynamic ELF files' "$T/log" || { cat "$T/log"; fail "the dynamic executable was not checked"; }
ok "a dynamic executable with -lnativewindow: it links, and elf-check.sh checks it and passes"

# 5. The NEEDED rule: a library that is no API 30 stub must be shipped; and a symbol that nothing has.
mkdir "$T/n" "$T/alone"
printf 'int oneios_fixture(void);\nint oneios_fixture(void) { return 7; }\n' > "$T/fixture.c"
printf 'extern int oneios_fixture(void);\nint oneios_user(void);\nint oneios_user(void) { return oneios_fixture(); }\n' > "$T/user.c"
links "libonefixture.so" "$CC" -fPIC -shared "${SO_FLAGS[@]}" -Wl,-soname,libonefixture.so "$T/fixture.c" -o "$T/n/libonefixture.so"
links "libuser.so" "$CC" -fPIC -shared "${SO_FLAGS[@]}" "$T/user.c" -L"$T/n" -lonefixture -o "$T/n/libuser.so"
cp "$T/n/libuser.so" "$T/alone/libuser.so"
check_fails "a NEEDED library that is not shipped" "$T/alone" \
  'libuser.so: NEEDED library libonefixture.so is not an API 30 stub of the NDK and not a library the runtime ships' \
  "$(missing libuser.so oneios_fixture 'libonefixture.so, libdl.so, libc.so' 'no API 30 stub and no library the runtime ships has it')"
check_passes "a NEEDED library that is shipped" "$T/n"
ok "a NEEDED library that is no API 30 stub: refused alone, accepted when the stage ships it"
links "a shared object with a symbol nothing has" "$CC" -fPIC -shared "$T/nosuch.c" -o "$T/alone/libuser.so"
check_fails "a symbol nothing has" "$T/alone" \
  "$(missing libuser.so oneios_no_such_symbol 'libdl.so, libc.so' 'no API 30 stub and no library the runtime ships has it')"
ok "a symbol that no stub has: elf-check.sh fails and says so"

# 6. A stage of runtime/build.sh: it runs elf-check.sh itself; then the same stage with the faulty library in it.
"$ROOT/runtime/build.sh" "$T/stage" > "$T/build.log" 2>&1 || { cat "$T/build.log"; fail "runtime/build.sh"; }
for elf in liboneios-display.so oneios-audio oneios-camera camctl; do
  grep -q "^elf-check: bin/$elf: NEEDED " "$T/build.log" || { cat "$T/build.log"; fail "runtime/build.sh did not check bin/$elf"; }
done
grep -q '^elf-check: bin/liboneios-display.so: NEEDED .*libnativewindow.so' "$T/build.log" || fail "bin/liboneios-display.so does not need libnativewindow.so"
grep -q '^elf-check: .* 0 failures$' "$T/build.log" || { cat "$T/build.log"; fail "runtime/build.sh: no summary of elf-check.sh"; }
ok "runtime/build.sh STAGE: $(grep '^elf-check: .* 0 failures$' "$T/build.log" | sed 's/^elf-check: //')"
cp "$T/f/libfault.so" "$T/stage/bin/liboneios-display.so"
check_fails "a stage with the faulty library" "$T/stage" \
  "$(missing bin/liboneios-display.so "$getter" "$old_needed" 'it is in libnativewindow.so')" \
  "$(missing bin/liboneios-display.so "$setter" "$old_needed" 'it is in libnativewindow.so')"
ok "the same stage with the faulty library as bin/liboneios-display.so: elf-check.sh fails: $(grep -m1 -o "symbol $getter.*" "$T/log")"
echo "all $pass tests passed"
