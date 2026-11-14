#!/bin/bash
# oneios-tool phone against a stand-in phone (fake-adb.sh): no phone and no root needed.
#
#   tools/oneios-tool/tests/phone/run.sh        one line per check (ok | FAILED), then `ALL OK` (exit 0) or `FAILED: N`
#
# What it checks: one phone only (none, several, --serial, ANDROID_SERIAL, a phone that is not ready); root is
# asked for first and its absence said; status; `stage runtime|runtime-asis|image` and `sums` against
# scripts/phone/stage.sh on a second, equal stand-in phone (the same output, the same stage afterwards); the
# refusals while a session is open; switch (the versionCode is the installed app's) and a refused preflight;
# return (the restore line, no session, a return that hangs); the session's own commands (session-cmds.sh:
# display-restart with --lib and --dex, touch status and wake, backlight status and set, each refused without a
# session, and the return after them); pull-data (the copy is the phone's file, sparse;
# a size that does not match); sparse-write. The session check's command is the phone's, byte for byte, and a pid
# that is not an xnuhost's is no session. Every case ends the stand-in session it left (end_case), and at the
# end, a failure or Ctrl-C none is left running (lib.sh's traps).
# With the OneOS fork's checkout at hand (ONEOS_DIR, or ../OneOS beside the main checkout), begin, receive, finish
# and status are the fork's own root script under its test root; without it those checks are skipped, with a line.
# With it, usb.sh also runs the fork's switch and return scripts' USB steps (the record, the session's USB watcher
# across unplug and replug, the return's restore) against a stand-in gadget: oneos/interface.md, "USB and adb".
# Env: TOOL (the binary), KEEP=1 (keep the scratch folder out/rustport-phone), FAKE_PROC=host|stand-in (the
# /proc the stand-in's commands read: the host's, or the stand-in's own as on macOS; default: the host's where it
# has one). tests/phone/modes.sh runs this suite, tests/app and tests/cycle in both.
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
TOOL=${TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
W=${WORK:-$ROOT/out/rustport-phone}
[ -x "$TOOL" ] || { echo "run.sh: no $TOOL (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)" >&2; exit 2; }
. "$HERE/lib.sh"
rm -rf "$W"; mkdir -p "$W/bin"
ln -s "$HERE/fake-adb.sh" "$W/bin/adb"       # for scripts/phone/stage.sh, which calls `adb`
export ADB=$HERE/fake-adb.sh FAKE_ADB_LOG=$W/adb.log FAKE_ROOT_LOG=$W/root.log
[ -n "${FAKE_PROC:-}" ] || { [ -r /proc/self/cmdline ] && FAKE_PROC=host || FAKE_PROC=stand-in; }
export FAKE_PROC; echo "(the stand-in's /proc: $FAKE_PROC)"
unset ANDROID_SERIAL FAKE_NO_ROOT FAKE_DEVICES
FORK=$(fork_dir)
if [ -n "$FORK" ]; then export FAKE_FORK_ROOT_SH=$FORK/app-android/app/src/full/assets/root/oneios_root.sh; echo "(the fork's root script: $FAKE_FORK_ROOT_SH)"
else echo "(no OneOS checkout: the checks that run the fork's root script are skipped)"; fi
phone() { "$TOOL" phone "$@"; }
P=$W/phone; export FAKE_PHONE=$P
make_phone "$P"

# ---- one phone only
check "no phone: refused" 1 "no phone on adb" env FAKE_DEVICES= "$TOOL" phone sums
two=$'AAA111\tdevice\nBBB222\tdevice'
check "two phones: refused, both named" 1 "2 phones are on adb \(AAA111 device, BBB222 device\)" env FAKE_DEVICES="$two" "$TOOL" phone sums
check "two phones, --serial" 0 "kernel.kc" env FAKE_DEVICES="$two" "$TOOL" phone --serial BBB222 sums
check "two phones, ANDROID_SERIAL" 0 "kernel.kc" env FAKE_DEVICES="$two" ANDROID_SERIAL=AAA111 "$TOOL" phone sums
check "--serial of a phone that is not attached" 1 "CCC333 is not on adb" env FAKE_DEVICES="$two" "$TOOL" phone -s CCC333 sums
check "a phone that is not ready" 1 '"unauthorized"' env FAKE_DEVICES=$'AAA111\tunauthorized' "$TOOL" phone sums

# ---- root first
for c in "status" "sums" "switch" "return" "stage image $W" "pull-data $W/x.img"; do
  check "no root: $c says so" 1 "no root on the phone FAKE0001.*does not survive a restart.*Nothing was changed" env FAKE_NO_ROOT=1 "$TOOL" phone $c
done
[ ! -e "$W/x.img" ] && [ ! -e "$W/x.img.part" ] && ok "no root: pull-data wrote no file" || bad "no root: pull-data wrote a file"

# ---- status
if [ -n "$FORK" ]; then
  check "status" 0 "@@RESULT stage present" phone status
  out=$(phone status 2>&1)
  for want in "phone: FAKE0001" "root: yes" "app: com.fahadali.oneos 0.4.86 \(versionCode 115\)" "session: none" "@@RESULT runtime 0.1.6 1 bin/xnuhost 1 0" "@@OK status"; do
    printf '%s\n' "$out" | grep -Eq "^$want" && ok "status: $want" || bad "status: $want" "$(printf '%s' "$out" | tr '\n' '|')"
  done
else echo "skipped status (no fork root script)"; fi
mv "$P/.fake/package.txt" "$P/.fake/package.off"
check "status without the app" 0 "is not installed" sh -c "'$TOOL' phone status 2>&1; true"
check "switch without the app: refused, no session" 1 "not installed" phone switch
mv "$P/.fake/package.off" "$P/.fake/package.txt"

# ---- stage and sums against scripts/phone/stage.sh on an equal phone
P2=$W/phone-sh
cp -a "$P" "$P2"
stage_sh() { (export FAKE_PHONE=$P2 PATH="$W/bin:$PATH"; sh "$ROOT/scripts/phone/stage.sh" "$@"); }
same() { # NAME: the two outputs and the two stages
  local name=$1
  sed -i 's/^staged=[0-9]*/staged=TIME/' "$W/rs.out" "$W/sh.out"   # runtime.conf's time of staging
  # the set's check after `stage image` is this tool's own (stage.sh has none)
  sed -i -E '/^(image: manifest.json and the|warning: the staged image set|  [^ ]+: manifest.json lists|make a manifest for the files)/d' "$W/rs.out"
  if ! cmp -s "$W/rs.out" "$W/sh.out"; then bad "$name: output as stage.sh's" "$(diff "$W/sh.out" "$W/rs.out" | head -n 4 | tr '\n' '|')"; return; fi
  # the ids lines hold inode numbers and times, which differ between two folders: compare the paths they list
  local d; d=$(diff -r -x '*.ids' -I '^staged=' "$P/data/local/tmp/oneios" "$P2/data/local/tmp/oneios" 2>&1 | head -n 5)
  [ -z "$d" ] || { bad "$name: the stage as stage.sh leaves it" "$(printf '%s' "$d" | tr '\n' '|')"; return; }
  local l
  for l in image.ids runtime.ids; do
    [ "$(cut -d' ' -f2 "$P/data/local/tmp/oneios/$l" 2>/dev/null)" = "$(cut -d' ' -f2 "$P2/data/local/tmp/oneios/$l" 2>/dev/null)" ] || { bad "$name: $l names"; return; }
  done
  ok "$name: output and stage as stage.sh's ($(wc -l < "$W/rs.out") lines)"
}
phone sums > "$W/rs.out" 2>&1; stage_sh sums > "$W/sh.out" 2>&1; same "sums"
grep -q "  image/kernel.kc$" "$W/rs.out" && grep -q " data.seed$" "$W/rs.out" && grep -q "^version=0.1.6" "$W/rs.out" && ok "sums: kernel.kc, data.seed's line, runtime.conf" || bad "sums content"
# image: two files of the set at their paths
I=$W/newimage; mkdir -p "$I/overlay/userland"
echo "a newer kernel collection" > "$I/kernel.kc"; echo "boot-args=newer" > "$I/overlay/userland/xnuhost.conf"
phone stage image "$I" > "$W/rs.out" 2>&1; rs=$?; stage_sh image "$I" > "$W/sh.out" 2>&1; sh_=$?
[ $rs = 0 ] && [ $sh_ = 0 ] && same "stage image" || bad "stage image" "status $rs and $sh_: $(tail -n 2 "$W/rs.out" | tr '\n' '|')"
S=$P/data/local/tmp/oneios
cmp -s "$I/kernel.kc" "$S/image/kernel.kc" && grep -q "^$(sha256sum < "$I/kernel.kc" | cut -d' ' -f1)  kernel.kc$" "$S/image.sha256" \
  && grep -q "^$(stat -c '%d:%i:%s:%Y' "$S/image/kernel.kc") kernel.kc$" "$S/image.ids" && [ "$(wc -l < "$S/image.sha256")" = 5 ] \
  && ok "stage image: the file, its image.sha256 line and its image.ids line" || bad "stage image: the lists"
[ ! -e "$P/data/local/tmp/oneios-stage-tmp" ] && ok "stage image: the pushed copy is gone" || bad "stage image: the pushed copy stayed"
mkdir -p "$W/notstaged"; echo x > "$W/notstaged/other.bin"
phone stage image "$W/notstaged" > "$W/rs.out" 2>&1; rs=$?; stage_sh image "$W/notstaged" > "$W/sh.out" 2>&1; sh_=$?
[ $rs != 0 ] && [ $sh_ != 0 ] && grep -q "not staged: image/other.bin" "$W/rs.out" && grep -q "not staged: image/other.bin" "$W/sh.out" && ok "stage image: a file that is not in the stage is refused by both" || bad "stage image of a file that is not staged" "$rs $sh_"
rm -rf "$P/data/local/tmp/oneios-stage-tmp" "$P2/data/local/tmp/oneios-stage-tmp"
mkdir -p "$W/badname"; echo x > "$W/badname/a b"
check "stage image: a name with a space never reaches root's shell" 1 "characters this tool does not put into a root shell" phone stage image "$W/badname"
# runtime: an unpacked package
R=$W/runtime-pkg; mkdir -p "$R/bin"
echo "xnuhost 0.1.7" > "$R/bin/xnuhost"; echo "a helper" > "$R/bin/stub"; echo "notes" > "$R/NOTES"
python3 -I - "$R" <<'PY'
import hashlib, json, os, sys
d = sys.argv[1]
files = [{"path": p, "size": os.path.getsize(f"{d}/{p}"), "sha256": hashlib.sha256(open(f"{d}/{p}", "rb").read()).hexdigest(), **({"mode": "0755"} if p.startswith("bin/") else {})}
         for p in ("bin/xnuhost", "bin/stub", "NOTES")]
json.dump({"format": 1, "name": "oneios-runtime", "version": "0.1.7-dev", "arch": "arm64", "session_abi": 1, "image_formats": [1, 2],
           "xnuhost": "bin/xnuhost", "files": files}, open(f"{d}/runtime.json", "w"), indent=2)
PY
if [ -n "$FORK" ]; then
  phone stage runtime "$R" > "$W/rs.out" 2>&1; rs=$?; stage_sh runtime "$R" > "$W/sh.out" 2>&1; sh_=$?
  [ $rs = 0 ] && [ $sh_ = 0 ] && same "stage runtime" || bad "stage runtime" "status $rs and $sh_: $(tail -n 2 "$W/rs.out" | tr '\n' '|')"
  grep -q "^version=0.1.7-dev" "$S/runtime.conf" && grep -q "^image_formats=1,2" "$S/runtime.conf" && cmp -s "$R/bin/xnuhost" "$S/runtime/bin/xnuhost" \
    && [ "$(stat -c %a "$S/runtime/bin/xnuhost")" = 755 ] && ok "stage runtime: runtime.conf, the files and their modes (the fork's begin/receive/finish)" || bad "stage runtime: the result" "$(cat "$S/runtime.conf" | tr '\n' '|')"
  # runtime-asis: a file replaced by hand no longer has runtime.json's hash
  echo "xnuhost replaced by hand" > "$R/bin/xnuhost"
  check "stage runtime: a file that is not runtime.json's is refused by the fork's script" 1 "@@REFUSED|@@FAILED" phone stage runtime "$R"
  phone stage runtime-asis "$R" > "$W/rs.out" 2>&1; rs=$?; stage_sh runtime-asis "$R" > "$W/sh.out" 2>&1; sh_=$?
  [ $rs = 0 ] && [ $sh_ = 0 ] && same "stage runtime-asis" || bad "stage runtime-asis" "status $rs and $sh_: $(tail -n 2 "$W/rs.out" | tr '\n' '|')"
  cmp -s "$R/bin/xnuhost" "$S/runtime/bin/xnuhost" && ok "stage runtime-asis: the replaced file is staged" || bad "stage runtime-asis: the file"
else echo "skipped stage runtime, runtime-asis (no fork root script)"; fi
check "stage runtime: a folder without runtime.json" 1 "no runtime.json" phone stage runtime "$I"

# ---- the set's manifest: a staged kernel whose manifest still lists the old one
check "stage image: says the manifest no longer lists the staged files" 0 "warning: the staged image set's manifest.json does not list" sh -c "'$TOOL' phone stage image '$I' 2>&1"
check "image-check: names each file the manifest does not list" 1 "kernel.kc: manifest.json lists [0-9a-f]{16}, the staged file is [0-9a-f]{16}" phone image-check
check "switch: refused before One UI is touched when xnuhost would refuse the set" 1 "does not list the staged files" phone switch
[ -z "$(session_pid "$P")" ] && ok "switch refused by the set's check: no session" || bad "switch refused by the set's check: a session started"
check "manifest DIR: the phone's, with DIR's entries" 0 "\"sha256\": \"$(sha256sum < "$I/kernel.kc" | cut -d' ' -f1)\"" phone manifest "$I"
check "manifest DIR: a file that is not in the set is refused" 1 "other.bin is not a file of the phone's image set" phone manifest "$W/notstaged"
phone manifest "$I" --out "$W/manifest.json" > /dev/null 2>&1
python3 -I -c 'import json,sys; m=json.load(open(sys.argv[1])); assert [f["path"] for f in m["files"]] == ["data.seed","kernel.kc","kernel.kc.hashes","overlay/userland/xnuhost.conf"] and m["build"] == "24A446"' "$W/manifest.json" \
  && ok "manifest --out: every entry and key kept, in order" || bad "manifest --out: the file"
check "manifest DIR --stage: the files and their manifest, then the check" 0 "image: manifest.json and the 4 files it lists agree" phone manifest "$I" --stage
check "image-check: the set agrees again" 0 "image: manifest.json and the 4 files it lists agree" phone image-check

# ---- switch, the refusals in a session, return
echo $$ > "$S/session/xnuhost.pid"   # a pid used again: a live process that is not an xnuhost
check "a pid that is not an xnuhost's is no session" 0 "kernel.kc" phone sums
rm -f "$S/session/xnuhost.pid"
echo "the phone is too warm (stand-in)" > "$P/.fake/preflight-refuses"
check "switch: a refused preflight is the answer" 1 "did not start: @@REFUSED the phone is too warm" phone switch
rm "$P/.fake/preflight-refuses"
[ -z "$(session_pid "$P")" ] && ok "switch refused: no session" || bad "switch refused: a session started"
sed -i 's/versionCode=115/versionCode=116/' "$P/.fake/package.txt"
check "switch" 0 "switch started \(versionCode 116\)" phone switch
grep -q "versionCode 116 ===$" "$S/switch.log" && ok "switch: the versionCode is the installed app's (116, not a fixed 115)" || bad "switch: versionCode" "$(tail -n 1 "$S/switch.log")"
wait_for 5 has_session "$P" && ok "switch: a session is open" || bad "switch: no session"
check "status in a session" 0 "session: open" sh -c "'$TOOL' phone status 2>&1; true"
for c in "switch" "sums" "stage image $I" "stage runtime $R" "pull-data $W/x.img"; do
  check "in a session: $c is refused" 1 "a session is open: return to One UI first" phone $c
done
check "switch --version-code" 1 "a session is open" phone switch --version-code 7
# the session check is the phone's command on every host: the stand-in maps /proc, the tool does not
read -r want <<'CMD'
p=$(cat /data/local/tmp/oneios/session/xnuhost.pid 2>/dev/null); [ -n "$p" ] && cat /proc/$p/cmdline 2>/dev/null | tr '\0' ' ' | grep -q xnuhost && echo open || echo closed
CMD
grep -Fxq -- "$want" "$W/root.log" && ! grep -F 'xnuhost.pid' "$W/root.log" | grep -Fxqv -- "$want" \
  && ok "the session check sent is the phone's command, unchanged ($FAKE_PROC /proc)" || bad "the session check sent" "$(grep -F xnuhost.pid "$W/root.log" | sort -u | head -n 2 | tr '\n' '|')"
wait_for 10 test -e "$S/session/ready"
check "return: waits for One UI's restore line" 0 "One UI restored in place \(Enforcing\)\. \([0-9]+ s\)" phone return --note "run.sh: done; rm -rf /"
grep -q "asked: run.sh: done rm -rf  ===" "$S/return.log" && ok "return: the note is in return.request, without shell characters" || bad "return: the note" "$(grep asked "$S/return.log")"
check "return: the outcome" 0 "return.outcome: inplace [0-9]+ fake-boot-id" phone return
check "return with no session" 0 "no session is open" phone return
[ -z "$(session_pid "$P")" ] && ok "after the return: no session" || bad "after the return: a session"
touch "$P/.fake/no-return"
phone switch --version-code 115 > /dev/null 2>&1; wait_for 5 has_session "$P"
check "return: a return that hangs ends at --timeout, with the log's tail" 1 "no restore line after [0-9]+ s" phone return --timeout 3
rm "$P/.fake/no-return"; wait_for 10 no_session "$P" || end_session "$P"
end_case

# ---- the session's own commands: display-restart, touch, backlight (tests/phone/session-cmds.sh, a phone of its own)
. "$HERE/session-cmds.sh"; session_cmd_checks

# ---- pull-data and sparse-write
check "pull-data" 0 "$W/data.img: 50331648 bytes, [0-9]+ of them written" phone pull-data "$W/data.img"
cmp -s "$W/data.img" "$S/data/data.img" && ok "pull-data: the copy is the phone's data.img" || bad "pull-data: the copy differs"
blocks=$(stat -c %b "$W/data.img"); [ $((blocks * 512)) -lt $((2 << 20)) ] && ok "pull-data: zero runs are holes ($((blocks * 512)) bytes on disk of 50331648)" || bad "pull-data: not sparse" "$blocks blocks"
[ ! -e "$W/data.img.part" ] && ok "pull-data: no .part left" || bad "pull-data: .part left"
grep -q "busybox dd if=/data/local/tmp/oneios/data/data.img bs=1M skip=0 count=48 2>/dev/null | /data/adb/ksu/bin/busybox gzip -1 -c" "$W/adb.log" && ok "pull-data: a range of the one named file through gzip -1 on the phone (tests/phone/pull-resume.sh has the chunks, the retries and the resume)" || bad "pull-data: the command"
# a stream of fewer bytes than its range: the stand-in gzip packs only the first of them
printf '#!/bin/sh\n[ "$1" = gzip ] && { head -c 1000000 | gzip -1 -c; exit 0; }\nexec "$@"\n' > "$P/data/adb/ksu/bin/busybox"
check "pull-data: fewer bytes than the range" 1 "the stream gave 1000000 bytes, the range has 50331648" phone pull-data "$W/short.img" --retries 0
[ ! -e "$W/short.img" ] && [ -e "$W/short.img.part" ] && ok "pull-data: the short copy stays as .part" || bad "pull-data: the short copy's name"
printf '#!/bin/sh\nexec "$@"\n' > "$P/data/adb/ksu/bin/busybox"
if grep -Eqi 'find |ls -R|du |/sys/power|reboot' "$W/adb.log" "$W/root.log"; then bad "a tree walk, /sys/power or a restart was sent to the phone" "$(grep -Ehi 'find |ls -R|du |/sys/power|reboot' "$W/adb.log" "$W/root.log" | head -n 2)"
else ok "nothing sent to the phone walks a tree, writes /sys/power or restarts ($(wc -l < "$W/adb.log") adb calls, their root scripts too)"; fi
"$TOOL" phone sparse-write "$W/sw.img" < "$S/data/data.img" > "$W/sw.out" 2>&1 && cmp -s "$W/sw.img" "$S/data/data.img" && [ $(($(stat -c %b "$W/sw.img") * 512)) -lt $((2 << 20)) ] \
  && ok "sparse-write: the same bytes, sparse" || bad "sparse-write" "$(cat "$W/sw.out")"
gzip -1 -c "$S/data/data.img" | "$TOOL" phone sparse-write "$W/swz.img" --gunzip --expect-size 50331648 > "$W/sw.out" 2>&1 && cmp -s "$W/swz.img" "$S/data/data.img" \
  && ok "sparse-write --gunzip --expect-size" || bad "sparse-write --gunzip" "$(cat "$W/sw.out")"
check "sparse-write --expect-size that does not match" 1 "50331648 bytes came, 5 were expected" sh -c "'$TOOL' phone sparse-write '$W/sw2.img' --expect-size 5 < '$S/data/data.img'"

# ---- USB and adb during a session: the fork's switch and return scripts (tests/phone/usb.sh)
if [ -z "$FORK" ]; then echo "skipped usb (no fork checkout: its switch and return scripts)"
elif ! grep -q -- '--usb-watch)' "$FORK/switcher/assets/oneios_switch.sh"; then
  echo "skipped usb (the fork checkout $FORK has no USB watcher yet: OneOS branch fix-usb-adb)"
else . "$HERE/usb.sh"; usb_checks; fi

no_session_left "phone suite"
[ -n "${KEEP:-}" ] || rm -rf "$W"
[ $FAILED = 0 ] && echo "ALL OK" || { echo "FAILED: $FAILED"; exit 1; }
