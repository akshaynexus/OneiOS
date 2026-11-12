#!/bin/sh
# Stage OneiOS files on the phone over adb, as the OneOS fork's app would (docs/building.md, "Test cycle on the phone").
#
#   scripts/phone/stage.sh runtime DIR    DIR: an unpacked runtime package (runtime.json at its top). Goes through the
#                                         fork's own root script (oneios_root.sh begin / receive / finish), so
#                                         runtime.sha256, runtime.ids and runtime.conf are written as an import does.
#   scripts/phone/stage.sh runtime-asis DIR   the same, but each file's size and SHA-256 are the file's own, not
#                                         runtime.json's: to put back a backed-up stage whose files were replaced by
#                                         hand (the phone's 0.1.6 of 2026-11-11). Modes still come from runtime.json.
#   scripts/phone/stage.sh image DIR      DIR: some files of an image set, at their paths in the set (kernel.kc,
#                                         kernel.kc.hashes, manifest.json, overlay/userland/xnuhost.conf ...). Each one
#                                         replaces the staged file, and its lines in image.sha256 and image.ids are
#                                         written again (the preflight checks both). The rest of image/ is untouched.
#   scripts/phone/stage.sh sums           print the sha256 of the staged kernel.kc, kernel.kc.hashes, manifest.json,
#                                         data.seed's line in image.sha256, xnuhost.conf and every runtime file
#
# Needs: adb with one phone, KernelSU root (su), the fork's stage at /data/local/tmp/oneios with bin/oneios_root.sh.
# Refuses while a session is open (session/xnuhost.pid names a live process). Changes nothing of One UI's.
set -eu
STAGE=/data/local/tmp/oneios
TMP=/data/local/tmp/oneios-stage-tmp
die() { echo "stage.sh: $*" >&2; exit 1; }
root() { adb shell "su -c 'sh -s'"; }
open_check() {
	r=$(printf '%s\n' "p=\$(cat $STAGE/session/xnuhost.pid 2>/dev/null); [ -n \"\$p\" ] && [ -d /proc/\$p ] && echo open || echo closed" | root | tr -d '\r')
	[ "$r" = closed ] || die "a session is open: return to One UI first"
}
case "${1:-}" in
runtime|runtime-asis)
	asis=0; [ "$1" = runtime ] || asis=1
	d=${2:?DIR}; [ -f "$d/runtime.json" ] || die "$d has no runtime.json"
	open_check
	adb shell "rm -rf $TMP" && adb push "$d" "$TMP" >/dev/null
	python3 - "$d" "$TMP" "$STAGE" "$asis" <<'PY' | root
import hashlib, json, sys
d, tmp, stage, asis = sys.argv[1:5]
raw = open(d + "/runtime.json", "rb").read()
j = json.loads(raw)
files = [("runtime.json", len(raw), hashlib.sha256(raw).hexdigest(), "0644")]
def own(p):
    b = open(d + "/" + p, "rb").read()
    return len(b), hashlib.sha256(b).hexdigest()
files += [(f["path"], *(own(f["path"]) if asis == "1" else (f["size"], f["sha256"])), f.get("mode", "0644")) for f in j["files"]]
print(f'set -e; R="sh {stage}/bin/oneios_root.sh"')
print("$R begin runtime")
for p, s, h, m in files:
    print(f"$R receive runtime {p} {s} {h} {m} < {tmp}/{p}")
formats = ",".join(str(x) for x in j["image_formats"])
print(f"$R finish runtime {len(files)} {j['version']} {j['session_abi']} {j['xnuhost']} {formats} {1 if j.get('placeholder') else 0}")
print(f"cat {stage}/runtime.conf; rm -rf {tmp}")
PY
	;;
image)
	d=${2:?DIR}; [ -d "$d" ] || die "no folder $d"
	open_check
	adb shell "rm -rf $TMP" && adb push "$d" "$TMP" >/dev/null
	list=$(cd "$d" && find . -type f | sed 's|^\./||' | sort)
	{
		echo "set -e; cd $STAGE"
		for p in $list; do
			echo "f=image/$p; [ -f \"\$f\" ] || { echo \"not staged: \$f\"; exit 1; }"
			echo "cp $TMP/$p \"\$f.part\" && chown root:root \"\$f.part\" && chmod 0644 \"\$f.part\" && mv -f \"\$f.part\" \"\$f\""
			echo "s=\$(sha256sum \"\$f\" | cut -d' ' -f1); i=\$(stat -c '%d:%i:%s:%Y' \"\$f\")"
			echo "awk -v p='$p' -v s=\"\$s\" '\$2 == p { print s \"  \" p; next } { print }' image.sha256 > image.sha256.new && mv -f image.sha256.new image.sha256"
			echo "awk -v p='$p' -v i=\"\$i\" '\$2 == p { print i \" \" p; next } { print }' image.ids > image.ids.new && mv -f image.ids.new image.ids"
			echo "echo \"staged \$s  \$f\""
		done
		echo "chmod 0600 image.sha256 image.ids; sync; rm -rf $TMP"
	} | root
	;;
sums)
	printf '%s\n' "cd $STAGE && sha256sum image/kernel.kc image/kernel.kc.hashes image/manifest.json image/overlay/userland/xnuhost.conf && grep ' data.seed\$' image.sha256 && cat runtime.conf && cat runtime.sha256" | root
	;;
*) sed -n '2,15p' "$0"; exit 2 ;;
esac
