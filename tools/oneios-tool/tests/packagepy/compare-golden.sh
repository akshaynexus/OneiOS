#!/bin/bash
# `oneios-tool package` (the runtime package's Apple-file check, manifest and archive) against the frozen output of
# runtime/package/pack.py, which it replaced on 2026-11-13: every refusal rule, what is accepted, the order in
# which two things are refused, the manifest's bytes and the archive's bytes.
#
#   tools/oneios-tool/tests/packagepy/compare-golden.sh    every case against golden/cases.txt: one line per case
#                                                          (same | DIFFERENT), then the counts; exit 1 when one differs
#   UPDATE=1 ...                                           write golden/cases.txt from this run instead
#   IMPL=python ...                                        run pack.py instead of the command: only at a commit that
#                                                          still has it (the golden was frozen with IMPL=python
#                                                          UPDATE=1 at the last such commit, then checked with the
#                                                          command)
#
# Every stage is made here: files of a few made-up bytes with the names and the first bytes the rules look at (a
# Mach-O header of 32 bytes, an Image4 tag, a UDIF trailer's four letters). No Apple file is read.
# A case is the command's stderr (the refusal), its status, and for the manifest and the archive the SHA-256 of
# what was written. runtime/package/test.sh (19 tests) runs the whole runtime/package.sh over the command.
# Env: ONEIOS_TOOL (the binary), KEEP=1 (keep the scratch folder), TMPDIR.
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
TOOL=${ONEIOS_TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
IMPL=${IMPL:-rust}
[ -x "$TOOL" ] || { echo "compare-golden: no $TOOL (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)" >&2; exit 2; }
W=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/packagepy.XXXXXX")" && pwd -P)
[ "${KEEP:-0}" = 1 ] || trap 'chmod -R u+rwx "$W" 2>/dev/null; rm -rf "$W"' EXIT
FOUND=$W/found.txt; : > "$FOUND"
S=$W/s
sha() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
step() {
  if [ "$IMPL" = python ]; then
    [ -f "$ROOT/runtime/package/pack.py" ] || { echo "compare-golden: runtime/package/pack.py is not there (retired): IMPL=python cannot run" >&2; exit 2; }
    python3 -I "$ROOT/runtime/package/pack.py" "$@"
  else "$TOOL" package "$@"; fi
}
# stage: an empty stage. put PATH [BYTES]: a file of the stage (printf's escapes). c NAME ARGS...: one case.
stage() { chmod -R u+rwx "$S" 2>/dev/null; rm -rf "$S"; mkdir -p "$S"; }
put() { mkdir -p "$(dirname "$S/$1")"; printf "${2:-x}" > "$S/$1"; }
c() {
  local name=$1; shift
  step "$@" > "$W/case.out" 2> "$W/case.err"; local status=$?
  { echo "== $name"; sed "s#$W#W#g" "$W/case.out"; sed -e "s#$W#W#g" -e 's/^/stderr: /' "$W/case.err"; echo "status $status"; } >> "$FOUND"
}
check() { c "$1" check "$S"; }
# a Mach-O header: macho PATH [FILETYPE [CPUTYPE-BYTES [MAGIC-BYTES]]] (an arm64 MH_EXECUTE without more)
macho() { put "$1" "${4:-\\xcf\\xfa\\xed\\xfe}${3:-\\x0c\\x00\\x00\\x01}\\x02\\x00\\x00\\x00\\x$(printf '%02x' "${2:-2}")\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00made-up, no code"; }
# record PATH [sed-script]: the good build record of a kernel, with fields changed by the script
record() {
  printf '{"builder": "oneios-kernel", "build_id": "test-1", "sha256": "%s", "prelinked": false, "kernelcache_kexts_added": false, "apple_binary_inputs": [], "restricted_sources": [], "xnu": "xnu-0", "profile": "test"}\n' \
    "$(sha < "$S/$1" | cut -c1-64)" | sed "${2:-s/x/x/}" > "$S/$1.build.json"
}

# ---- refused by name: every pattern, in lower and in other case, deep in a folder
for name in kernelcache.release.iphone18 KernelCache.Research x-cryptex1-systemos restore.ipsw 090-12345-678.dmg os.dmg.aea os.img data.simg \
            system.erofs kc.im4p boot.img4 ap.im4m kernel.kc static.trustcache BuildManifest.plist Restore.plist apnonce.shsh2 x.shsh \
            dyld_shared_cache_arm64e IOSomething.kext Some.dext UPPER.IPSW; do
  stage; put "ok.txt"; put "deep/er/$name"; check "name: $name"
done
stage; put "kernelcache"; put "buildmanifest.plist.txt"; put "restore.plist.bak"; put "my.kextension"; put "x.imgs"; put "cryptex"; check "name: the first of several (cryptex), and names that only look like the patterns"
stage; put "buildmanifest.plist.txt"; put "restore.plist.bak"; put "my.kextension"; put "x.imgs"; put "Info.kext/Contents/plain.txt"; put "a.kc/x"; check "names that only look like the patterns, and folders named like firmware: accepted"

# ---- refused by content, under plain names
stage; put blob '\x30\x83\x12\x34\x56\x16\x04IM4P\x16\x04krnl'; check "content: IM4P"
stage; put blob '\x30\x82\x01\x00\x16\x04IMG4'; check "content: IMG4"
stage; put blob '\x30\x82\x01\x00\x16\x04IM4M'; check "content: IM4M"
stage; put blob '\x30\x82\x01\x00\x16\x04IM4R'; check "content: IM4R"
stage; put blob '\x30\x82\x01\x00\x16\x04IM4X-----------------IM4P'; check "content: a DER sequence whose Image4 tag comes after 24 bytes: accepted"
stage; put blob 'IMG4 without the DER byte'; put list 'bplist00 and more'; check "content: IMG4 letters without the DER byte, a binary plist: accepted"
stage; put blob 'AEA1\x00\x00\x00\x00rest'; check "content: AEA"
stage; put blob "$(printf 'd%.0s' $(seq 1 300))$(printf 'koly')$(printf 'e%.0s' $(seq 1 508))"; check "content: a UDIF trailer 512 bytes before the end"
stage; put blob "$(printf 'd%.0s' $(seq 1 300))$(printf 'koly')$(printf 'e%.0s' $(seq 1 507))"; check "content: the same letters one byte off: accepted"
stage; put blob 'koly and nothing else'; check "content: UDIF letters at the start"
stage; put blob 'dyld_v1  arm64e'; check "content: a dyld cache"
stage; put blob 'NXSB....'; check "content: an APFS container (magic at 0)"
stage; put blob '0123456789abcdef0123456789abcdefNXSB....'; check "content: an APFS container (magic at 32)"
stage; put blob '\x7fELF\x02\x01\x01'; put text 'plain'; put empty ''; check "content: an ELF program, text, an empty file: accepted"

# ---- every Mach-O but our own kernel
stage; macho bin/tool 2 '\x0c\x00\x00\x01' '\xce\xfa\xed\xfe'; check "Mach-O: 32-bit"
stage; macho bin/tool 2 '\x0c\x00\x00\x01' '\xfe\xed\xfa\xcf'; check "Mach-O: big-endian 64-bit"
stage; macho bin/tool 2 '\x0c\x00\x00\x01' '\xfe\xed\xfa\xce'; check "Mach-O: big-endian 32-bit"
stage; macho bin/tool 2 '\x0c\x00\x00\x01' '\xca\xfe\xba\xbe'; check "Mach-O: fat"
stage; macho bin/tool 2 '\x0c\x00\x00\x01' '\xbe\xba\xfe\xca'; check "Mach-O: fat, the other byte order"
stage; macho bin/tool 2 '\x0c\x00\x00\x01' '\xca\xfe\xba\xbf'; check "Mach-O: fat 64"
stage; macho bin/tool 2 '\x0c\x00\x00\x01' '\xbf\xba\xfe\xca'; check "Mach-O: fat 64, the other byte order"
stage; macho kernel/xnu.macho 12; record kernel/xnu.macho; check "Mach-O: a kernel collection (MH_FILESET), with a good record"
stage; macho kernel/xnu.macho 12; check "Mach-O: a kernel collection without a record"
stage; macho bin/tool 6; record bin/tool; check "Mach-O: an arm64 dylib, with a record"
stage; macho kernel/xnu.macho 2 '\x07\x00\x00\x01'; record kernel/xnu.macho; check "Mach-O: an x86_64 executable, with a record"
stage; macho kernel/xnu.macho 2; check "kernel: no build record"
stage; macho kernel/xnu.macho 2; printf 'not json' > "$S/kernel/xnu.macho.build.json"; c "kernel: a record that is no JSON (the message's tail is the parser's)" check "$S"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"oneios-kernel"/"someone"/'; check "kernel: the record of another builder"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"builder": "oneios-kernel", //'; check "kernel: a record without a builder"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"sha256": "[0-9a-f]*"/"sha256": "00"/'; check "kernel: a record for another file"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"kernelcache_kexts_added": false/"kernelcache_kexts_added": true/'; check "kernel: kernelcache kexts added"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"kernelcache_kexts_added": false, //'; check "kernel: the record does not say whether kexts were added"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"kernelcache_kexts_added": false/"kernelcache_kexts_added": 0/'; check "kernel: 0 instead of false"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"prelinked": false/"prelinked": true/'; check "kernel: prelinked"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"apple_binary_inputs": \[\]/"apple_binary_inputs": ["a.kext", "b"]/'; check "kernel: Apple binary inputs"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"apple_binary_inputs": \[\], //'; check "kernel: the record does not list its binary inputs"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"restricted_sources": \[\]/"restricted_sources": ["corecrypto-2023"]/'; check "kernel: a source that may not be redistributed"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"restricted_sources": \[\]/"restricted_sources": null/'; check "kernel: restricted sources null"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"build_id": "test-1", //'; check "kernel: no build_id"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho 's/"build_id": "test-1"/"build_id": ""/'; check "kernel: an empty build_id"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho; macho other/second.macho 2; record other/second.macho; check "kernel: a second kernel in one package"
stage; macho kernel/xnu.macho 2; record kernel/xnu.macho; put bin/xnuhost; check "kernel: our own, with its record: accepted"
stage; macho kernel/kernelcache.kc 2; record kernel/kernelcache.kc; check "kernel: our own under a firmware name (its record's name then matches a pattern)"

# ---- paths the fork's unpacker rejects, links and special files
stage; put 'notes,v2.txt'; check "path: a comma"
stage; put 'two words'; check "path: a space"
stage; put '.hidden'; check "path: a leading dot"
stage; put 'dir/-dash'; check "path: a leading dash"
stage; put 'bad dir/fine.txt'; check "path: a folder's name"
stage; put 'ok/a+b_c-d.e'; check "path: every allowed character: accepted"
stage; put a; ln -s /etc/passwd "$S/link"; check "a symbolic link to a file"
stage; put a/b; ln -s a "$S/dirlink"; check "a symbolic link to a folder"
stage; put a; ln -s nowhere "$S/dangling"; check "a symbolic link to nothing"
stage; put a; mkfifo "$S/pipe"; check "a named pipe"

# ---- the order: the stage's own files first, then its folders by name; a path refusal before any content
stage; put z.dmg; put a/kernelcache; check "order: z.dmg at the top before a/kernelcache"
stage; put a/z.dmg; put b/a.ipsw; check "order: a/z.dmg before b/a.ipsw"
stage; put a.dmg; put 'zz/bad,name'; check "order: a path the unpacker rejects, though a firmware name comes first"
stage; put a/b.ipsw; put 'a,b/c'; check "order: a folder's bad name is seen when its parent is listed"

# ---- the manifest
good() { stage; put bin/xnuhost '#!/bin/sh\necho placeholder\n'; chmod 0755 "$S/bin/xnuhost"; put lib/oneios-display.dex 'dex'; put NOTICE 'notice\n'; put scripts/run.sh '#!/bin/sh\n'; chmod 0700 "$S/scripts/run.sh"; chmod 0600 "$S/NOTICE"; }
modes() { ( cd "$S" && find . -type f | LC_ALL=C sort | while read -r f; do echo "mode $(stat -c %a "$f" 2>/dev/null || stat -f %Lp "$f") $f"; done ) >> "$FOUND"; }
good; c "manifest: a runtime without a kernel" manifest "$S" 1.2.3-dev 0123456789abcdef0123456789abcdef01234567 0 0.4.80 1
echo "sha256 $(sha < "$S/runtime.json" | cut -c1-64) runtime.json" >> "$FOUND"; modes
c "manifest: again over its own runtime.json" manifest "$S" 1.2.3-dev 0123456789abcdef0123456789abcdef01234567 1 0.4.80 0
echo "sha256 $(sha < "$S/runtime.json" | cut -c1-64) runtime.json" >> "$FOUND"
good; macho kernel/xnu.macho 2; record kernel/xnu.macho; c "manifest: with our own kernel" manifest "$S" 0.1.0 0123456789abcdef0123456789abcdef01234567 0 0.4.80 0
echo "sha256 $(sha < "$S/runtime.json" | cut -c1-64) runtime.json" >> "$FOUND"; sed -n '/"kernel": {/,/}/p' "$S/runtime.json" | sed 's/"sha256": "[0-9a-f]*"/"sha256": "..."/' >> "$FOUND"
good; rm "$S/bin/xnuhost"; c "manifest: no bin/xnuhost" manifest "$S" 0.1.0 00 0 0.4.80 0
good; chmod 0644 "$S/bin/xnuhost"; c "manifest: bin/xnuhost is not executable" manifest "$S" 0.1.0 00 0 0.4.80 0
good; put fw/os.img; c "manifest: an Apple name in the stage" manifest "$S" 0.1.0 00 0 0.4.80 0
[ ! -e "$S/runtime.json" ] && echo "no runtime.json written" >> "$FOUND"

# ---- the archive
good; step manifest "$S" 0.1.0 00 0 0.4.80 0
long=$(printf 'd%.0s' $(seq 1 60))/$(printf 'e%.0s' $(seq 1 60)); put "$long/file" 'long name'
printf 'runtime.json\nNOTICE\nbin/xnuhost\n\nlib/oneios-display.dex\nscripts/run.sh\n%s/file\n' "$long" > "$W/members"
c "archive: the members in the list's order, owner 0:0, a fixed time" archive "$S" "$W/members" 1731400000 "$W/out.tar"
echo "sha256 $(sha < "$W/out.tar" | cut -c1-64) $(wc -c < "$W/out.tar" | tr -d ' ') bytes out.tar" >> "$FOUND"
ln -s NOTICE "$S/alias"; printf 'NOTICE\nalias\n' > "$W/members"; c "archive: a member that is a link" archive "$S" "$W/members" 1731400000 "$W/out2.tar"
printf 'NOTICE\nbin\n' > "$W/members"; c "archive: a member that is a folder" archive "$S" "$W/members" 1731400000 "$W/out3.tar"
ln "$S/NOTICE" "$S/NOTICE2"; printf 'NOTICE\nNOTICE2\n' > "$W/members"; c "archive: a second name of a file already written" archive "$S" "$W/members" 1731400000 "$W/out4.tar"

# ---- the result
# (the JSON parser's own words for a record that is no JSON differ between Python and Rust: its case keeps the
# refusal up to them)
sed 's/^\(stderr: package: refused: kernel\/xnu.macho: unreadable build record:\).*/\1 .../' "$FOUND" > "$FOUND.n" && mv "$FOUND.n" "$FOUND"
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
[ "$(grep -c '^== ' "$FOUND")" = "$(grep -c '^== ' "$HERE/golden/cases.txt")" ] || { differ=$((differ + 1)); echo "DIFFERENT  the list of cases"; }
echo "packagepy (IMPL=$IMPL): $same the same, $differ different"
[ $differ = 0 ]
