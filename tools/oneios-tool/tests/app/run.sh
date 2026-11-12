#!/bin/bash
# oneios-tool app with a stand-in OneOS checkout (its gradlew is a script), a stand-in gh and the stand-in phone:
# no JDK, no Android SDK, no phone and no network needed. The app is never built for real here.
#
#   tools/oneios-tool/tests/app/run.sh        one line per check, then `ALL OK` (exit 0) or `FAILED: N`
#
# What it checks: where the checkout is found (--oneos, ONEOS_DIR, the current folder, beside the OneiOS
# checkout from a worktree) and that the way is said; version (the gradle fields, the series suffix, the commit
# and the dirty state, the installed app's version and certificate); build (the gradle command, --no-test, a
# failing build, the APK's facts); doctor (each prerequisite missing and present; a token is never printed);
# install (a new install, the same certificate, another certificate refused without an uninstall, a session
# open, --release through gh with its .sha256); stage-scripts (the three files, their sha256 before and after).
# `app version --no-phone` also runs on the real OneOS clone when there is one. Needs python3, openssl, git.
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
TOOL=${TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
W=${WORK:-$ROOT/out/rustport-app}
[ -x "$TOOL" ] || { echo "run.sh: no $TOOL" >&2; exit 2; }
. "$HERE/../phone/lib.sh"
rm -rf "$W"; mkdir -p "$W/home" "$W/bin"
export ADB=$HERE/../phone/fake-adb.sh FAKE_ADB_LOG=$W/adb.log HOME=$W/home
unset ANDROID_SERIAL FAKE_NO_ROOT FAKE_DEVICES ONEOS_DIR JAVA_HOME ANDROID_HOME ANDROID_SDK_ROOT GH_PACKAGES_USER GH_PACKAGES_TOKEN
REAL=$(HOME=~ fork_dir)
app() { "$TOOL" app "$@"; }

# ---- a stand-in checkout
F=$W/OneOS
mkdir -p "$F/app-android/app/src/full/assets/root" "$F/switcher/assets"
echo "// stand-in" > "$F/app-android/settings.gradle.kts"
cat > "$F/app-android/app/build.gradle.kts" <<'G'
android {
    compileSdk { version = release(37) }
    defaultConfig {
        applicationId = "com.fahadali.oneos"
        versionCode = 115
        versionName = "0.4.86"
    }
}
G
echo "# switch of the stand-in fork" > "$F/switcher/assets/oneios_switch.sh"
echo "# return of the stand-in fork" > "$F/switcher/assets/oneios_return.sh"
echo "# root script of the stand-in fork" > "$F/app-android/app/src/full/assets/root/oneios_root.sh"
# two keys, and an APK signed by each
for k in a b; do
  openssl req -x509 -newkey rsa:2048 -nodes -keyout "$W/$k.key" -out "$W/$k.der" -outform DER -days 2 -subj "/CN=stand-in $k" 2>/dev/null
  python3 -I "$HERE/mkapk.py" "$W/$k.apk" "$W/$k.der" "apk $k"
  printf 'Packages:\n  Package [com.fahadali.oneos] (1a2b3c4):\n    versionCode=117 minSdk=31 targetSdk=37\n    versionName=0.4.88\n' > "$W/$k.apk.package.txt"
done
certa=$(openssl dgst -sha256 "$W/a.der" | sed 's/.*= //'); certb=$(openssl dgst -sha256 "$W/b.der" | sed 's/.*= //')
cat > "$F/app-android/gradlew" <<G
#!/bin/sh
echo "\$PWD: gradlew \$*" >> "$W/gradle.log"
[ ! -e "$W/gradle-fails" ] || { echo "FAILURE: Build failed with an exception."; exit 1; }
mkdir -p app/build/outputs/apk/full/debug && cp "$W/a.apk" app/build/outputs/apk/full/debug/app-full-debug.apk
echo "BUILD SUCCESSFUL"
G
chmod +x "$F/app-android/gradlew"
(cd "$F" && git init -q -b oneios . && git add -A && git -c user.name=t -c user.email=t@example.invalid commit -q -m stand-in)

# ---- where the checkout is found
check "--oneos" 0 "^OneOS checkout: $F \(--oneos\)" app --oneos "$F" version --no-phone
check "ONEOS_DIR" 0 "^OneOS checkout: $F \(ONEOS_DIR\)" env ONEOS_DIR="$F" "$TOOL" app version --no-phone
check "the current folder" 0 "^OneOS checkout: $F \(the current folder\)" sh -c "cd '$F' && '$TOOL' app version --no-phone"
check "--oneos wins over ONEOS_DIR" 0 "\(--oneos\)" env ONEOS_DIR=/nowhere "$TOOL" app --oneos "$F" version --no-phone
check "--oneos of a folder that is no checkout" 1 "is not a OneOS checkout \(no app-android/settings.gradle.kts\)" app --oneos "$W" version --no-phone
# a OneiOS checkout with a worktree, and OneOS beside the checkout: found from inside the worktree
mkdir -p "$W/side/OneiOS" && cp -r "$F" "$W/side/OneOS"
(cd "$W/side/OneiOS" && git init -q . && git -c user.name=t -c user.email=t@example.invalid commit -q --allow-empty -m x && git worktree add -q .claude/worktrees/agent-x 2>/dev/null)
check "beside the OneiOS checkout, from a worktree" 0 "^OneOS checkout: $W/side/OneOS \(beside the OneiOS checkout\)" sh -c "cd '$W/side/OneiOS/.claude/worktrees/agent-x' && '$TOOL' app version --no-phone"
nowhere=$(mktemp -d /tmp/oneios-app-test.XXXXXX)   # outside every git checkout
check "no checkout anywhere" 1 "no OneOS checkout: name it with --oneos DIR or ONEOS_DIR" sh -c "cd '$nowhere' && '$TOOL' app version --no-phone"
rmdir "$nowhere"
export ONEOS_DIR=$F

# ---- version
out=$(app version --no-phone 2>&1)
for want in "applicationId: com.fahadali.oneos \(the full flavour; probe: com.fahadali.oneos.probe\)" "versionName: 0.4.86 \(full: 0.4.86, probe: 0.4.86-probe\)" "versionCode: 115" "commit: [0-9a-f]{7,} \(branch oneios\), clean"; do
  printf '%s\n' "$out" | grep -Eq "^$want$" && ok "version: $want" || bad "version: $want" "$(printf '%s' "$out" | tr '\n' '|')"
done
echo "oneos.series=fold8" > "$F/app-android/gradle.properties"
check "version: the series suffix and a dirty tree" 0 "full: 0.4.86-fold8" app version --no-phone
check "version: dirty" 0 "dirty \(1 changed files\)" app version --no-phone
rm "$F/app-android/gradle.properties"
P=$W/phone; export FAKE_PHONE=$P; make_phone "$P"
mkdir -p "$P/data/app/~~fake==/com.fahadali.oneos-1=="; cp "$W/a.apk" "$P/data/app/~~fake==/com.fahadali.oneos-1==/base.apk"
check "version with a phone: the installed version" 0 "^installed: 0.4.86 \(versionCode 115\) on FAKE0001$" app version
check "version with a phone: the installed certificate" 0 "^installed certificate sha256: $certa$" app version
check "version with no phone attached" 0 "^installed: not asked \(no phone on adb" env FAKE_DEVICES= "$TOOL" app version
if [ -n "$REAL" ]; then
  check "version --no-phone on the real clone ($REAL)" 0 "^versionCode: [0-9]+$" env ONEOS_DIR="$REAL" "$TOOL" app version --no-phone
  env ONEOS_DIR="$REAL" "$TOOL" app version --no-phone | sed 's/^/    /'
else echo "skipped: version on the real clone (none found)"; fi

# ---- build
check "build" 0 "^APK: $F/app-android/app/build/outputs/apk/full/debug/app-full-debug.apk$" app build
tail -n 1 "$W/gradle.log" | grep -q "^$F/app-android: gradlew test assembleFullDebug$" && ok "build: cd app-android && ./gradlew test assembleFullDebug" || bad "build: the command" "$(tail -n 1 "$W/gradle.log")"
out=$(app build --no-test 2>&1)
tail -n 1 "$W/gradle.log" | grep -q ": gradlew assembleFullDebug$" && ok "build --no-test: assembleFullDebug only" || bad "build --no-test" "$(tail -n 1 "$W/gradle.log")"
for want in "size: $(stat -c %s "$W/a.apk")" "sha256: $(sha256sum < "$W/a.apk" | cut -d' ' -f1)" "version: 0.4.86 \(versionCode 115\)" "certificate sha256: $certa"; do
  printf '%s\n' "$out" | grep -Eq "^$want$" && ok "build: $want" || bad "build: $want" "$(printf '%s' "$out" | tr '\n' '|')"
done
touch "$W/gradle-fails"
check "build: a failing gradle is a failure" 1 "gradlew ended with exit status: 1" app build
rm "$W/gradle-fails"

# ---- doctor
check "doctor: nothing there" 1 "MISSING JDK 21|MISSING Android SDK" app doctor
out=$(app doctor 2>&1)
for want in "ok      gradlew" "MISSING Android SDK: none found" "MISSING GitHub Packages user" "MISSING GitHub Packages token"; do
  printf '%s\n' "$out" | grep -q "^$want" && ok "doctor, bare: $want" || bad "doctor, bare: $want" "$(printf '%s' "$out" | tr '\n' '|')"
done
mkdir -p "$W/jdk21/bin" "$W/jdk17/bin" "$W/sdk/platforms/android-37.0" "$W/sdk36/platforms/android-36" "$W/home/.config/twidget"
printf '#!/bin/sh\necho "openjdk version \\"21.0.5\\" 2025-10-15" >&2\n' > "$W/jdk21/bin/java"
printf '#!/bin/sh\necho "openjdk version \\"17.0.19\\" 2026-04-21" >&2\n' > "$W/jdk17/bin/java"
chmod +x "$W"/jdk*/bin/java
secret=ghp_STANDINSECRETSTANDINSECRET0123456789
printf 'ghUsername=someone\nghAccessToken=%s\n' "$secret" > "$W/home/.config/twidget/github.properties"
check "doctor: everything there" 0 "the build's prerequisites are there" env JAVA_HOME="$W/jdk21" ANDROID_HOME="$W/sdk" "$TOOL" app doctor
out=$(env JAVA_HOME="$W/jdk21" ANDROID_HOME="$W/sdk" "$TOOL" app doctor 2>&1)
for want in "ok      JDK 21: .* is 21.0.5" "ok      Android SDK: $W/sdk \(ANDROID_HOME\)" "ok      platform 37: platforms/android-37.0" "ok      GitHub Packages user: set \(github.properties\)" "ok      GitHub Packages token: set \(github.properties; a classic token, 40 characters"; do
  printf '%s\n' "$out" | grep -Eq "^$want" && ok "doctor: $want" || bad "doctor: $want" "$(printf '%s' "$out" | tr '\n' '|')"
done
printf '%s\n' "$out" | grep -q "STANDINSECRET\|someone" && bad "doctor printed the token or the user" || ok "doctor: neither the token nor the user name is printed"
check "doctor: JDK 17" 1 "MISSING JDK 21: .* is 17.0.19; the build's toolchain is 21" env JAVA_HOME="$W/jdk17" ANDROID_HOME="$W/sdk" "$TOOL" app doctor
check "doctor: an SDK without platform 37" 1 "MISSING platform 37: not in the SDK" env JAVA_HOME="$W/jdk21" ANDROID_SDK_ROOT="$W/sdk36" "$TOOL" app doctor
echo "sdk.dir=$W/sdk" > "$F/app-android/local.properties"
check "doctor: the SDK of local.properties" 0 "Android SDK: $W/sdk \(local.properties sdk.dir\)" env JAVA_HOME="$W/jdk21" "$TOOL" app doctor
rm "$W/home/.config/twidget/github.properties"
check "doctor: the login from the environment" 0 "GitHub Packages token: set \(GH_PACKAGES_TOKEN" env JAVA_HOME="$W/jdk21" GH_PACKAGES_USER=someone GH_PACKAGES_TOKEN=$secret "$TOOL" app doctor
out=$(env JAVA_HOME="$W/jdk21" GH_PACKAGES_USER=someone GH_PACKAGES_TOKEN=github_pat_STANDINSECRET "$TOOL" app doctor 2>&1); st=$?
[ $st = 1 ] && printf '%s\n' "$out" | grep -q "MISSING GitHub Packages token: set (GH_PACKAGES_TOKEN), but it is a fine-grained token" && ! printf '%s\n' "$out" | grep -q STANDINSECRET \
  && ok "doctor: a fine-grained token is refused, not printed" || bad "doctor: a fine-grained token" "$st"

# ---- install
rm -rf "$P/data/app" "$P/.fake/package.txt"
cp "$W/a.apk.package.txt" "$P/.fake/next-package.txt"   # what the phone says after an install
installed() { cmp -s "$1" "$P/data/app/~~fake==/com.fahadali.oneos-1==/base.apk"; }
check "install: nothing installed yet" 0 "^installed now: 0.4.88 \(versionCode 117\)$" app install
installed "$W/a.apk" && ok "install: the built APK is on the phone" || bad "install: the APK on the phone"
grep -q "^-s FAKE0001 install -r $F/app-android/app/build/outputs/apk/full/debug/app-full-debug.apk$" "$W/adb.log" && ok "install: adb install -r" || bad "install: the adb command" "$(grep install "$W/adb.log" | tail -n 1)"
check "install: the same certificate goes over the installed app" 0 "^installed: 0.4.88 \(versionCode 117\), certificate sha256 $certa$" app install "$W/a.apk"
check "install: another certificate is refused" 1 "refused: the installed app is signed with another certificate.*Do not uninstall: that wipes the app's data" app install "$W/b.apk"
installed "$W/a.apk" && ok "install refused: the installed app is untouched" || bad "install refused: the installed app changed"
grep -qi "uninstall" "$W/adb.log" && bad "an uninstall was sent to the phone" || ok "install: no uninstall is ever sent"
check "install: a file that is no APK" 1 "not an APK|signing block|no signing certificate" app install "$W/a.der"
check "install: no root" 1 "no root on the phone" env FAKE_NO_ROOT=1 "$TOOL" app install "$W/a.apk"
"$TOOL" phone switch --version-code 117 > /dev/null 2>&1; wait_for 5 has_session "$P"
check "install: refused while a session is open" 1 "a session is open: return to One UI first" app install "$W/a.apk"
check "stage-scripts: refused while a session is open" 1 "a session is open" app stage-scripts
"$TOOL" phone return > /dev/null 2>&1; wait_for 10 no_session "$P" || end_session "$P"
# --release: a stand-in gh that serves the prerelease's two files
cat > "$W/bin/gh" <<G
#!/bin/sh
echo "gh \$*" >> "$W/gh.log"
[ "\$1 \$2 \$3" = "release download oneios-0.4.88" ] || { echo "release not found" >&2; exit 1; }
while [ \$# -gt 1 ]; do shift; done
cp "$W/a.apk" "\$1/OneOS-oneios-0.4.88-debug.apk"
[ -e "$W/gh-bad-sum" ] && echo "0000  x" > "\$1/OneOS-oneios-0.4.88-debug.apk.sha256" || (cd "\$1" && sha256sum OneOS-oneios-0.4.88-debug.apk > OneOS-oneios-0.4.88-debug.apk.sha256)
G
chmod +x "$W/bin/gh"; export GH=$W/bin/gh
check "install --release" 0 "^sha256 as the release's .sha256: $(sha256sum < "$W/a.apk" | cut -d' ' -f1)$" app install --release 0.4.88 --dir "$W/dl"
grep -q "^gh release download oneios-0.4.88 --repo akshaynexus/OneOS --pattern OneOS-oneios-0.4.88-debug.apk " "$W/gh.log" && ok "install --release: gh release download oneios-<version>" || bad "install --release: the gh command" "$(tail -n 1 "$W/gh.log")"
check "install --release of a version that is not there" 1 "gh release download oneios-9.9.9 ended with" app install --release 9.9.9 --dir "$W/dl"
touch "$W/gh-bad-sum"
check "install --release: a download that is not the release's sha256" 1 "the release's .sha256 says 0000" app install --release 0.4.88 --dir "$W/dl"
rm "$W/gh-bad-sum"

# ---- stage-scripts
S=$P/data/local/tmp/oneios
link=$(readlink "$S/bin/oneios_root.sh"); before=$(sha256sum < "$link")
out=$(app stage-scripts 2>&1); st=$?
[ $st = 0 ] && ok "stage-scripts" || bad "stage-scripts" "$(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
for f in "switcher/assets/oneios_switch.sh oneios_switch.sh" "switcher/assets/oneios_return.sh oneios_return.sh" "app-android/app/src/full/assets/root/oneios_root.sh bin/oneios_root.sh"; do
  set -- $f; sum=$(sha256sum < "$F/$1" | cut -d' ' -f1)
  cmp -s "$F/$1" "$S/$2" && [ ! -L "$S/$2" ] && printf '%s\n' "$out" | grep -Eq "^$2: before [0-9a-f]{64}, after $sum \(replaced\)$" && ok "stage-scripts: $2 is the fork's, sha256 before and after" || bad "stage-scripts: $2" "$(printf '%s' "$out" | tr '\n' '|')"
done
[ "$(sha256sum < "$link")" = "$before" ] && ok "stage-scripts: what was there was replaced, not written through (a link's target is untouched)" || bad "stage-scripts wrote through a link"
[ "$(stat -c %a "$S/oneios_switch.sh")" = 700 ] && [ ! -e "$P/data/local/tmp/oneios-scripts-tmp" ] && ok "stage-scripts: mode 0700, the pushed copies are gone" || bad "stage-scripts: modes or leftovers"
check "stage-scripts again: unchanged" 0 "^bin/oneios_root.sh: before ([0-9a-f]{64}), after \1 \(unchanged\)$" app stage-scripts
if grep -Eqi 'find |ls -R|du |/sys/power|reboot|uninstall' "$W/adb.log"; then bad "a tree walk, /sys/power, a restart or an uninstall was sent to the phone"
else ok "nothing sent to the phone walks a tree, writes /sys/power, restarts or uninstalls ($(wc -l < "$W/adb.log") adb calls)"; fi

end_session "$P"
[ -n "${KEEP:-}" ] || rm -rf "$W"
[ $FAILED = 0 ] && echo "ALL OK" || { echo "FAILED: $FAILED"; exit 1; }
