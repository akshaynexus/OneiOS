#!/bin/bash
# oneios-tool image against scripts/image/imagetool.py on the real image set: every output byte for byte.
#
#   tools/oneios-tool/tests/image/compare-python.sh            everything (about 6 minutes; Python's part is most of it)
#   SKIP_SLOW=1 tools/oneios-tool/tests/image/compare-python.sh    without apfs-owners of the whole OS volume (Python: 2 minutes)
#   FULL_BUILD=1 ...                                           also scripts/image/build.sh --no-pack with each tool
#                                                              (it was IMAGETOOL_IMPL=python, then rust: since 2026-11-13
#                                                              build.sh runs the CLI either way, so the two builds
#                                                              are the same tool twice), and the two sets compared;
#                                                              BUILD_ARGS: more options for build.sh (--no-hook, or
#                                                              --systemos-root DIR when the image is attached elsewhere)
#
# Runs each ported subcommand with the Python tool and with the Rust tool on the same inputs, and compares what
# they print (stdout, stderr), their exit status and every file they write with cmp. One line per item:
#   NAME   python SECONDS s   rust SECONDS s   same | DIFFERENT (what)
# then the times as a table, then `ALL SAME` (exit 0) or `DIFFERENT: N` (exit 1).
#
# Items: profile (JSON, --shell, --members); hashes of os.img, systemos.img, app.img, kernel.kc (also against the
# set's own .hashes); apfs-info and apfs-owners of each image; tree-copy of the OS volume's /private/var (the meta
# file and the copied tree) and seed-pack of it (data.seed); seed-pack of a tree changed as a hook changes it, with
# owner rules; manifest (manifest.json and the hash lists; also against the set's own manifest.json); verify and
# verify --quick of the set, and of sets with one thing wrong each.
#
# Inputs (Apple's files, read in place, never copied into the repository): the built image set
# $ONEIOS_MAIN/out/image/<build>/ and, when there, $ONEIOS_MAIN/inputs/<build>/inputs.json.
# Scratch: $WORK (default out/rustport-image/ of this checkout, git-ignored). It has to be on the set's volume:
# the scratch sets are hard links to the set's images, so nothing big is copied. About 5 GiB while it runs.
# tree-copy reads the OS volume through a read-only mount (macOS: hdiutil with owners off, the owners from the
# image's records; Linux: apfs-fuse). OS_ROOT=DIR uses a volume root that is already readable instead; on macOS
# an os.img that is attached elsewhere with owners off is found and read there (an image attaches once).
# The times are wall times of one run each: they mean something only on a machine that does nothing else.
#
# Env: ONEIOS_MAIN  the checkout whose out/ and inputs/ hold the set (default: this checkout; from a worktree
#                   under .claude/worktrees/, the main checkout)
#      TOOL         the Rust binary (default tools/oneios-tool/target/release/oneios-tool)
#      PROFILE      the profile (default profiles/ios-27.0.1-iPhone18,2.toml)
#      INPUTS       the folder with the IPSW members (default $ONEIOS_MAIN/inputs/<build>; from a worktree whose
#                   out/ holds the set, the main checkout's)
#      WORK, OS_ROOT, SKIP_SLOW, FULL_BUILD  as above
#      KEEP=1       keep the scratch files
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
die() { echo "compare-python: $*" >&2; exit 2; }

main=$ROOT
case "$ROOT" in */.claude/worktrees/*) [ -d "$ROOT/out/image" ] || main=${ROOT%%/.claude/worktrees/*} ;; esac
MAIN=${ONEIOS_MAIN:-$main}
TOOL=${TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
PY_TOOL=$ROOT/scripts/image/imagetool.py
PROFILE=${PROFILE:-$ROOT/profiles/ios-27.0.1-iPhone18,2.toml}
WORK=${WORK:-$ROOT/out/rustport-image}
[ -x "$TOOL" ] || die "no $TOOL (build it: cargo build --release --manifest-path tools/oneios-tool/Cargo.toml, or pass TOOL=)"
[ -f "$PROFILE" ] || die "no profile $PROFILE"
command -v python3 >/dev/null || die "python3 is missing"
unset ONEIOS_IMAGE_TIMING ONEIOS_IMAGE_WRITERS IMAGETOOL_IMPL
export IMAGETOOL_PY=$PY_TOOL

py() { python3 -I "$PY_TOOL" "$@"; }
rs() { "$TOOL" image "$@"; }
eval "$(py profile "$PROFILE" --shell)" || die "the profile does not load"
SET=${SET:-$MAIN/out/image/$P_BUILD}
[ -f "$SET/manifest.json" ] || die "no image set at $SET (build it: scripts/image/build.sh $PROFILE; or pass ONEIOS_MAIN= or SET=)"
# the IPSW members: beside the set, or in the main checkout when the set is in a worktree's out/
INPUTS_DIR=${INPUTS:-$MAIN/inputs/$P_BUILD}
[ -d "$INPUTS_DIR" ] || case "$ROOT" in */.claude/worktrees/*) INPUTS_DIR=${ROOT%%/.claude/worktrees/*}/inputs/$P_BUILD ;; esac
INPUTS_JSON=$INPUTS_DIR/inputs.json

LOGS=$WORK/logs
rm -rf "$LOGS"
mkdir -p "$LOGS" "$WORK/.work" || die "cannot make $WORK"
OS=$(uname -s)
mounted=
unmount() {
  [ -n "$mounted" ] || return 0
  case "$OS" in
  Darwin) hdiutil detach -quiet "$mounted" 2>/dev/null || hdiutil detach -quiet -force "$mounted" 2>/dev/null ;;
  *) fusermount3 -u "$mounted" 2>/dev/null || fusermount -u "$mounted" 2>/dev/null ;;
  esac
  rmdir "$mounted" 2>/dev/null
  mounted=
}
cleanup() {
  unmount
  if [ "${KEEP:-0}" != 1 ]; then
    chmod -R u+w "$WORK/.work" 2>/dev/null
    rm -rf "$WORK/.work" "$WORK/sets" "$WORK/build-python" "$WORK/build-rust"
  fi
}
trap cleanup EXIT

# ---- running and comparing
TIMEFORMAT=%R
n=0 different=0
rows=()
run() { # BASE CMD...: CMD's stdout, stderr, exit status and wall time into BASE.out, .err, .status, .time
  local base=$1; shift
  { time "$@" > "$base.out" 2> "$base.err"; echo $? > "$base.status"; } 2> "$base.time"
}
base() { n=$((n + 1)); printf '%s/%02d' "$LOGS" "$n"; }
report() { # LABEL BASE [PYTHON_FILE RUST_FILE]...: compare BASE.py.* with BASE.rs.* and the file pairs
  local label=$1 b=$2 bad=""; shift 2
  cmp -s "$b.py.out" "$b.rs.out" || bad="$bad stdout"
  cmp -s "$b.py.err" "$b.rs.err" || bad="$bad stderr"
  cmp -s "$b.py.status" "$b.rs.status" || bad="$bad exit-status"
  while [ $# -ge 2 ]; do
    if [ ! -e "$1" ] || [ ! -e "$2" ] || ! cmp -s "$1" "$2"; then bad="$bad $(basename "$2")"; fi
    shift 2
  done
  local tp tr verdict=same
  tp=$(cat "$b.py.time") tr=$(cat "$b.rs.time")
  if [ -n "$bad" ]; then verdict="DIFFERENT:$bad (see $b.*)"; different=$((different + 1)); fi
  printf '%-46s python %8.2f s   rust %8.2f s   %s\n' "$label" "$tp" "$tr" "$verdict"
  rows+=("$label|$tp|$tr|$(cat "$b.py.status")")
}
note() { # LABEL same|DIFFERENT TEXT: a comparison without a pair of runs
  printf '%-46s %s %s\n' "$1" "$2" "$3"
  [ "$2" = same ] || different=$((different + 1))
}
pair() { # LABEL ARGS...: both tools with the same arguments, nothing written
  local label=$1 b; shift; b=$(base)
  run "$b.py" py "$@"
  run "$b.rs" rs "$@"
  report "$label" "$b"
}

echo "set: $SET"
echo "tool: $TOOL"
echo "python: $(python3 -V 2>&1), $(sysctl -n hw.ncpu 2>/dev/null || nproc) CPUs, $OS"
echo

# ---- profile
pair "profile (JSON)" profile "$PROFILE"
pair "profile --shell" profile "$PROFILE" --shell
pair "profile --members" profile "$PROFILE" --members

# ---- hashes
mkdir -p "$WORK/.work/hashes"
for name in os.img systemos.img app.img kernel.kc; do
  [ -f "$SET/$name" ] || continue
  b=$(base)
  run "$b.py" py hashes "$SET/$name" "$WORK/.work/hashes/$name.py.hashes"
  run "$b.rs" rs hashes "$SET/$name" "$WORK/.work/hashes/$name.rs.hashes"
  report "hashes $name" "$b" "$WORK/.work/hashes/$name.py.hashes" "$WORK/.work/hashes/$name.rs.hashes" \
    "$SET/$name.hashes" "$WORK/.work/hashes/$name.rs.hashes"
done
b=$(base)
run "$b.py" py hashes "$SET/app.img" "$WORK/.work/hashes/app.py.4096" --chunk 4096
run "$b.rs" rs hashes "$SET/app.img" "$WORK/.work/hashes/app.rs.4096" --chunk 4096
report "hashes app.img --chunk 4096" "$b" "$WORK/.work/hashes/app.py.4096" "$WORK/.work/hashes/app.rs.4096"

# ---- apfs-info, apfs-owners
for name in os.img systemos.img app.img; do
  pair "apfs-info $name" apfs-info "$SET/$name"
done
pair "apfs-info kernel.kc (not APFS)" apfs-info "$SET/kernel.kc"
pair "apfs-owners os.img $P_DATA_SOURCE" apfs-owners "$SET/os.img" "$P_DATA_SOURCE"
pair "apfs-owners systemos.img" apfs-owners "$SET/systemos.img"
pair "apfs-owners app.img" apfs-owners "$SET/app.img"
pair "apfs-owners os.img no/such/folder" apfs-owners "$SET/os.img" no/such/folder
if [ "${SKIP_SLOW:-0}" = 1 ]; then
  echo "apfs-owners os.img (the whole volume)             skipped (SKIP_SLOW=1)"
else
  pair "apfs-owners os.img (the whole volume)" apfs-owners "$SET/os.img"
fi

# ---- tree-copy and seed-pack
os_root=${OS_ROOT:-}
owners=()
# macOS: a volume attached with owners off shows every file as the user's; the owners come from the image's records
[ "$OS" != Darwin ] || owners=(--owners-from "$SET/os.img" --owners-path "$P_DATA_SOURCE")
attached_at() { # IMAGE: where the same file is attached already with owners off (macOS attaches an image once)
  python3 -I - "$1" <<'PY'
import os, subprocess, sys
want = os.stat(sys.argv[1])
mounts = subprocess.run(["mount"], capture_output=True, text=True).stdout
same = False
for line in subprocess.run(["hdiutil", "info"], capture_output=True, text=True).stdout.splitlines():
    if line.startswith("image-path"):
        try:
            st = os.stat(line.split(":", 1)[1].strip())
            same = (st.st_dev, st.st_ino) == (want.st_dev, want.st_ino)
        except OSError:
            same = False
    elif same and line.startswith("/dev/disk") and "\t/" in line:
        device, where = line.split("\t")[0].strip(), line.split("\t")[-1].strip()
        how = [m for m in mounts.splitlines() if m.startswith(f"{device} on {where} (")]
        if how and "noowners" in how[0] and os.path.isdir(os.path.join(where, "System")):
            print(where)
            break
PY
}
if [ -z "$os_root" ]; then
  mnt=$WORK/.work/mnt-os
  mkdir -p "$mnt"
  case "$OS" in
  Darwin)
    # as build.sh: owners off, so a user may read every file
    if hdiutil attach -readonly -nobrowse -noverify -noautoopen -owners off -imagekey diskimage-class=CRawDiskImage \
      -mountpoint "$mnt" "$SET/os.img" >/dev/null 2>&1 && [ -d "$mnt/System" ]; then
      mounted=$mnt os_root=$mnt
    else
      os_root=$(attached_at "$SET/os.img")
      [ -z "$os_root" ] || echo "(os.img is attached elsewhere: reading $os_root)"
    fi ;;
  Linux)
    if command -v apfs-fuse >/dev/null 2>&1 && apfs-fuse "$SET/os.img" "$mnt" >/dev/null 2>&1; then
      mounted=$mnt os_root=$mnt/root
    fi ;;
  esac
fi
tree_listing() { # DIR OUT: every entry's path, type, mode, link target, and a file's size, time and SHA-256
  python3 -I - "$1" > "$2" <<'PY'
import hashlib, os, stat, sys
top = sys.argv[1]
rows = []
for base, dirs, files in os.walk(top):
    for name in dirs + files:
        full = os.path.join(base, name)
        st = os.lstat(full)
        row = [os.path.relpath(full, top), stat.filemode(st.st_mode)]
        if stat.S_ISLNK(st.st_mode):
            row.append(os.readlink(full))
        elif stat.S_ISREG(st.st_mode):
            with open(full, "rb") as f:
                row += [str(st.st_size), str(st.st_mtime_ns), hashlib.file_digest(f, "sha256").hexdigest()]
        rows.append("\t".join(row))
print("\n".join(sorted(rows)))
PY
}
if [ -n "$os_root" ] && [ -d "$os_root/$P_DATA_SOURCE" ]; then
  T=$WORK/.work/tree
  mkdir -p "$T"
  b=$(base)
  run "$b.py" py tree-copy "$os_root/$P_DATA_SOURCE" "$T/py" "$T/py.meta.json" ${owners[@]+"${owners[@]}"}
  run "$b.rs" rs tree-copy "$os_root/$P_DATA_SOURCE" "$T/rs" "$T/rs.meta.json" ${owners[@]+"${owners[@]}"}
  tree_listing "$T/py" "$T/py.listing"
  tree_listing "$T/rs" "$T/rs.listing"
  report "tree-copy $P_DATA_SOURCE (meta, tree)" "$b" "$T/py.meta.json" "$T/rs.meta.json" "$T/py.listing" "$T/rs.listing"
  pair "tree-copy into a folder that is not empty" tree-copy "$os_root/$P_DATA_SOURCE" "$T/py" "$T/again.meta.json"

  # each tool packs the tree it copied
  : > "$T/owners.tsv"
  b=$(base)
  run "$b.py" py seed-pack "$T/py" "$T/py.meta.json" "$T/py.seed" --epoch "$P_EPOCH" --owners "$T/owners.tsv"
  run "$b.rs" rs seed-pack "$T/rs" "$T/rs.meta.json" "$T/rs.seed" --epoch "$P_EPOCH" --owners "$T/owners.tsv"
  report "seed-pack (the copied tree)" "$b" "$T/py.seed" "$T/rs.seed"
  rm -f "$T/py.seed" "$T/rs.seed"

  # what a hook does to the tree: rewrite a file, add files (a name that is not ASCII, one longer than 100
  # bytes), a folder and a link, remove a file; and owner rules
  python3 -I - "$T/py" "$T/py.meta.json" "$T/owners.tsv" <<'PY'
import json, os, sys
tree, meta, owners = sys.argv[1:4]
entries = json.load(open(meta))["entries"]
files = sorted(k for k, v in entries.items() if v["t"] == "f")
with open(os.path.join(tree, files[0]), "ab") as f:
    f.write(b"changed by compare-python.sh\n")
os.unlink(os.path.join(tree, files[1]))
os.makedirs(os.path.join(tree, "oneios", "tools", "empty"))
for rel, mode in (("oneios/tools/probe", 0o755), ("oneios/café.txt", 0o644), ("oneios/" + "n" * 120, 0o600)):
    with open(os.path.join(tree, rel), "w") as f:
        f.write("added by compare-python.sh: " + rel + "\n")
    os.chmod(os.path.join(tree, rel), mode)
for rel in ("oneios", "oneios/tools", "oneios/tools/empty"):
    os.chmod(os.path.join(tree, rel), 0o755)
os.symlink("tools/probe", os.path.join(tree, "oneios", "link"))
with open(owners, "w") as f:
    f.write("# compare-python.sh\noneios\t501\t501\t-\tR\noneios/tools/probe\t0\t0\t4755\n/mobile/\t501\t501\t750\n")
PY
  b=$(base)
  run "$b.py" py seed-pack "$T/py" "$T/py.meta.json" "$T/py.seed" --epoch "$P_EPOCH" --owners "$T/owners.tsv"
  run "$b.rs" rs seed-pack "$T/py" "$T/py.meta.json" "$T/rs.seed" --epoch "$P_EPOCH" --owners "$T/owners.tsv"
  report "seed-pack (a changed tree, owner rules)" "$b" "$T/py.seed" "$T/rs.seed"
  printf 'x\t0\t0\n' > "$T/bad.tsv"
  pair "seed-pack with a bad owner rule" seed-pack "$T/py" "$T/py.meta.json" "$T/bad.seed" --epoch "$P_EPOCH" --owners "$T/bad.tsv"
  chmod -R u+w "$T" 2>/dev/null
  rm -rf "$T"
else
  echo "tree-copy, seed-pack                           skipped: the OS volume is not readable (hdiutil or apfs-fuse; or OS_ROOT=DIR)"
fi
unmount

# ---- manifest and verify: scratch sets of hard links to the set's files
linked_set() { # DIR: the set's files without its manifest and hash lists
  rm -rf "$1"
  (cd "$SET" && find . -type f ! -name manifest.json ! -name '*.hashes') | while IFS= read -r f; do
    mkdir -p "$1/$(dirname "$f")"
    ln "$SET/$f" "$1/$f" || exit 1
  done
}
linked_copy() { # FROM DIR: every file of FROM, by hard link
  rm -rf "$2"
  (cd "$1" && find . -type f) | while IFS= read -r f; do
    mkdir -p "$2/$(dirname "$f")"
    ln "$1/$f" "$2/$f" || exit 1
  done
}
S=$WORK/sets
mkdir -p "$S"
linked_set "$S/py" && linked_set "$S/rs" || die "cannot hard-link $SET into $S (WORK has to be on the set's volume)"
# the seed information and the tweaks the set was built with are in its manifest
python3 -I - "$SET/manifest.json" "$S/seed.json" "$S/tweaks.json" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
seed = next(f for f in m["files"] if f.get("role") == "data-seed")
json.dump({k: v for k, v in seed.items() if k not in ("path", "size", "sha256", "role", "apfs")}, open(sys.argv[2], "w"))
json.dump(m["tweaks"], open(sys.argv[3], "w"))
PY
margs=(--seed-info "$S/seed.json" --tweaks "$S/tweaks.json")
[ ! -f "$INPUTS_JSON" ] || margs+=(--inputs "$INPUTS_JSON")
b=$(base)
run "$b.py" py manifest "$PROFILE" "$S/py" "${margs[@]}"
run "$b.rs" rs manifest "$PROFILE" "$S/rs" "${margs[@]}"
files=("$S/py/manifest.json" "$S/rs/manifest.json")
for h in "$S"/py/*.hashes; do files+=("$h" "$S/rs/$(basename "$h")"); done
report "manifest" "$b" "${files[@]}"
if cmp -s "$SET/manifest.json" "$S/rs/manifest.json"; then
  note "manifest: the set's own manifest.json" same "(the Rust tool wrote the file the build wrote)"
else
  echo "manifest: the set's own manifest.json          differs from what both tools write now (the profile or the tools changed since the set was built)"
fi
b=$(base)
echo x > "$S/rs/a-stray.bin"; cp "$S/rs/a-stray.bin" "$S/py/a-stray.bin"
run "$b.py" py manifest "$PROFILE" "$S/py" "${margs[@]}"
run "$b.rs" rs manifest "$PROFILE" "$S/rs" "${margs[@]}"
report "manifest of a set with a stray file" "$b"
rm -f "$S/rs/a-stray.bin" "$S/py/a-stray.bin"

pair "verify" verify "$S/rs"
pair "verify --quick" verify "$S/rs" --quick
flip() { # FILE OFFSET: FILE becomes a copy of itself (no longer a hard link) with one byte changed
  python3 -I - "$1" "$2" <<'PY'
import os, sys
path, at = sys.argv[1], int(sys.argv[2])
data = bytearray(open(path, "rb").read())
data[at] ^= 1
os.unlink(path)
open(path, "wb").write(data)
PY
}
bad() { linked_copy "$S/rs" "$S/bad" || die "cannot make $S/bad"; }
bad; flip "$S/bad/trust/app.root_hash" 5
pair "verify: one byte of a small file changed" verify "$S/bad"
pair "verify --quick: the same set" verify "$S/bad" --quick
bad; flip "$S/bad/app.img.hashes" 40
pair "verify: one byte of a hash list changed" verify "$S/bad"
bad; flip "$S/bad/app.img.hashes" 140
python3 -I - "$S/bad" <<'PY'
import hashlib, json, os, sys
d = sys.argv[1]
m = json.load(open(os.path.join(d, "manifest.json")))
for f in m["files"]:
    if f["path"] == "app.img.hashes":
        f["sha256"] = hashlib.sha256(open(os.path.join(d, f["path"]), "rb").read()).hexdigest()
os.unlink(os.path.join(d, "manifest.json"))
json.dump(m, open(os.path.join(d, "manifest.json"), "w"), indent=2)
PY
pair "verify: a hash list of other content" verify "$S/bad"
bad; rm "$S/bad/trust/os.root_hash"
pair "verify: a file missing" verify "$S/bad"
bad; echo x > "$S/bad/extra.bin"
pair "verify: a file too many" verify "$S/bad" --quick
bad; cp "$S/bad/trust/app.trustcache" "$S/bad/t"; echo x >> "$S/bad/t"; mv "$S/bad/t" "$S/bad/trust/app.trustcache"
pair "verify --quick: a file one byte longer" verify "$S/bad" --quick
bad; rm "$S/bad/manifest.json"; echo '{"name": "oneios-image", "format": 2}' > "$S/bad/manifest.json"
pair "verify: a manifest of a newer format" verify "$S/bad"
bad; rm "$S/bad/manifest.json"
pair "verify: no manifest" verify "$S/bad"
rm -rf "$S/bad"

# ---- the whole build with each tool
if [ "${FULL_BUILD:-0}" = 1 ]; then
  kernel=(--no-kernel)
  [ ! -f "$SET/kernel.kc" ] || kernel=(--kernel "$SET/kernel.kc")
  for impl in python rust; do
    b=$LOGS/build-$impl
    # shellcheck disable=SC2086
    { time env IMAGETOOL_IMPL=$impl ONEIOS_TOOL="$TOOL" INPUTS="$INPUTS_DIR" OUT="$WORK/build-$impl" \
      "$ROOT/scripts/image/build.sh" --no-pack "${kernel[@]}" ${BUILD_ARGS:-} "$PROFILE" > "$b.out" 2> "$b.err"; echo $? > "$b.status"; } 2> "$b.time"
  done
  bad=""
  [ "$(cat "$LOGS/build-python.status")" = 0 ] && [ "$(cat "$LOGS/build-rust.status")" = 0 ] || bad=" exit-status"
  # the same lines, apart from the folder each build wrote to
  sed "s|$WORK/build-python|OUT|g" "$LOGS/build-python.out" > "$LOGS/build-python.lines"
  sed "s|$WORK/build-rust|OUT|g" "$LOGS/build-rust.out" > "$LOGS/build-rust.lines"
  cmp -s "$LOGS/build-python.lines" "$LOGS/build-rust.lines" || bad="$bad stdout"
  if [ -z "$bad" ]; then
    (cd "$WORK/build-python/$P_BUILD" && find . -type f | sort) > "$LOGS/build-python.files"
    (cd "$WORK/build-rust/$P_BUILD" && find . -type f | sort) > "$LOGS/build-rust.files"
    cmp -s "$LOGS/build-python.files" "$LOGS/build-rust.files" || bad="$bad file-names"
    while IFS= read -r f; do
      cmp -s "$WORK/build-python/$P_BUILD/$f" "$WORK/build-rust/$P_BUILD/$f" || bad="$bad $f"
    done < "$LOGS/build-python.files"
  fi
  verdict=same
  if [ -n "$bad" ]; then verdict="DIFFERENT:$bad (see $LOGS/build-*)"; different=$((different + 1)); fi
  printf '%-46s python %8.2f s   rust %8.2f s   %s\n' "build.sh --no-pack (every file of the set)" \
    "$(cat "$LOGS/build-python.time")" "$(cat "$LOGS/build-rust.time")" "$verdict"
  rows+=("build.sh --no-pack (whole script)|$(cat "$LOGS/build-python.time")|$(cat "$LOGS/build-rust.time")|0")
fi

# ---- the times
echo
printf '| %-44s | %10s | %10s | %7s |\n' "Command (exit status)" "Python (s)" "Rust (s)" "Faster"
printf '| %-44s | %10s | %10s | %7s |\n' "---" "---" "---" "---"
for row in "${rows[@]}"; do
  IFS='|' read -r label tp tr status <<< "$row"
  printf '| %-44s | %10.2f | %10.2f | %7s |\n' "$label ($status)" "$tp" "$tr" \
    "$(awk -v p="$tp" -v r="$tr" 'BEGIN { if (r > 0.005) printf "%.1fx", p / r; else printf "-" }')"
done
echo
if [ "$different" = 0 ]; then
  echo "ALL SAME"
else
  echo "DIFFERENT: $different (the outputs are in $LOGS)"
  exit 1
fi
