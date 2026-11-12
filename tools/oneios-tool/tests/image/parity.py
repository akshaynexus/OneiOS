#!/usr/bin/env python3
"""oneios-tool image against scripts/image/imagetool.py on generated stand-in files (no Apple files, no network).

    tools/oneios-tool/tests/image/parity.py [-v] [TOOL]

TOOL: the Rust binary (default: $TOOL, else tools/oneios-tool/target/release/oneios-tool).

Each case runs the same command line with both tools (the Python tool as `python3 -I scripts/image/imagetool.py`,
the Rust tool as `TOOL image`), each in its own folder with the same inputs, and compares the exit status, stdout,
stderr and every file written, byte for byte. The stand-in inputs are the ones of scripts/image/tests/fixtures.py.
Where Python ends with a traceback or quotes its TOML or JSON parser, only the exit status (and the start of the
message) is compared: the Rust tool prints one line there (tools/oneios-tool/crates/oneios-image/src/lib.rs).

Prints one line per case with -v, then `parity: N cases, all the same` (exit 0) or the cases that differ (exit 1).
The real image set: tools/oneios-tool/tests/image/compare-python.sh.
"""
import hashlib
import json
import os
import shutil
import stat
import struct
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", "..", ".."))
sys.path.insert(0, os.path.join(ROOT, "scripts", "image"))
sys.path.insert(0, os.path.join(ROOT, "scripts", "image", "tests"))
import fixtures          # noqa: E402
import imagetool as it   # noqa: E402

PY_TOOL = os.path.join(ROOT, "scripts", "image", "imagetool.py")
VERBOSE = "-v" in sys.argv[1:]
_args = [a for a in sys.argv[1:] if a != "-v"]
TOOL = _args[0] if _args else os.environ.get("TOOL") or os.path.join(ROOT, "tools", "oneios-tool", "target", "release", "oneios-tool")

failures = []
cases = 0


class Side:
    """One tool and the folder it works in."""

    def __init__(self, name, command, folder):
        self.name, self.command, self.dir = name, command, folder
        os.makedirs(folder)

    def run(self, args):
        env = {k: v for k, v in os.environ.items() if not k.startswith(("ONEIOS_IMAGE_", "IMAGETOOL_"))}
        r = subprocess.run(self.command + list(args), cwd=self.dir, capture_output=True, env=env)
        return r.returncode, r.stdout, r.stderr

    def path(self, *rel):
        return os.path.join(self.dir, *rel)


def listing(top):
    """Every entry below top: path, type and mode, link target, a file's size, whole-second time and SHA-256."""
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
                    row += [str(st.st_size), str(st.st_mtime_ns), hashlib.sha256(f.read()).hexdigest()]
            rows.append("\t".join(row))
    return "\n".join(sorted(rows)).encode()


def read(path):
    if os.path.isdir(path):
        return listing(path)
    try:
        with open(path, "rb") as f:
            return f.read()
    except OSError as e:
        return f"<{type(e).__name__}>".encode()


def case(name, args, files=(), how="exact", status=None):
    """Run args with both tools. files: paths (relative to each tool's folder) that must be the same afterwards.
    how: exact; "status" (exit status only: Python ends with a traceback); "usage" (both print their usage);
    a str ending in ":" (both messages start with it: the rest quotes a parser)."""
    global cases
    cases += 1
    (ps, po, pe), (rs, ro, re_) = PY.run(args), RS.run(args)
    bad = []
    if ps != rs:
        bad.append(f"exit status {ps} / {rs}")
    if status is not None and ps != status:
        bad.append(f"Python's exit status is {ps}, the case expects {status}")
    if how == "exact":
        if po != ro:
            bad.append(f"stdout\n    python: {po[:400]!r}\n    rust:   {ro[:400]!r}")
        if pe != re_:
            bad.append(f"stderr\n    python: {pe[:400]!r}\n    rust:   {re_[:400]!r}")
    elif how == "usage":
        if b"imagetool.py profile PROFILE" not in pe or b"oneios-tool image profile PROFILE" not in re_:
            bad.append(f"no usage\n    python: {pe[:200]!r}\n    rust:   {re_[:200]!r}")
    elif how != "status":
        if not pe.decode().startswith(how) or not re_.decode().startswith(how):
            bad.append(f"stderr does not start with {how!r}\n    python: {pe[:300]!r}\n    rust:   {re_[:300]!r}")
    for rel in files:
        a, b = read(PY.path(rel)), read(RS.path(rel))
        if a != b:
            bad.append(f"{rel} differs ({len(a)} / {len(b)} bytes)")
    if bad:
        failures.append(f"{name}: " + "; ".join(bad))
    if VERBOSE:
        print(f"{'DIFFERENT' if bad else 'same':<10}{name}  [exit {ps}]")


def each(fn):
    """Do the same thing in both tools' folders."""
    for side in (PY, RS):
        fn(side)


def write(rel, data):
    def put(side):
        full = side.path(rel)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "wb" if isinstance(data, bytes) else "w") as f:
            f.write(data)
    each(put)


def noise(n, seed):
    out, h = b"", hashlib.sha256(str(seed).encode()).digest()
    while len(out) < n:
        h = hashlib.sha256(h).digest()
        out += h
    return out[:n]


# ---------------------------------------------------------------- the cases

def profile_cases(IN):
    _, profile, _ = fixtures.fake_ipsw(os.path.join(IN, "src"))
    for mode in ([], ["--shell"], ["--members"]):
        case(f"profile {' '.join(mode)}", ["profile", profile] + mode, status=0)
    with open(profile) as f:
        good = f.read()
    broken = {
        "schema": good.replace("schema = 1", "schema = 2"),
        "schema-text": good.replace("schema = 1", 'schema = "one"'),
        "no-schema": good.replace("schema = 1\n", ""),
        "role": good.replace('role = "restore"', 'role = "bogus"'),
        "two-roles": good.replace('role = "restore"', 'role = "os"'),
        "crc": 'crc32 = "'.join((good.rsplit('crc32 = "', 1)[0], good.rsplit('crc32 = "', 1)[1][1:])),   # seven digits
        "aea": good.replace("aea = true", "aea = false", 1),
        "format": good.replace("format = 1", "format = 9"),
        "name": good.replace('os = "os.img"', 'os = "../os.img"'),
        "twice": good.replace('app = "app.img"', 'app = "os.img"'),
        "no-name": good.replace('app = "app.img"\n', ""),
        "uuid": good.replace("11111111-2222-3333-4444-555555555555", "not-a-uuid"),
        "chunk": good.replace("chunk = 4096", "chunk = 1000"),
        "epoch": good.replace("epoch = 1700000000", 'epoch = "soon"'),
        "build": good.replace('build = "0T1"', 'build = "0T-1"'),
        "board": good.replace('board = "t1ap"', "board = 5"),
        "ipsw-sha": good.replace('sha256 = "', 'sha256 = "zz', 1),
        "stage": good.replace('stage = "/data/local/tmp/oneios/image"\n', ""),
        "seed": good.replace('seed = "data.seed"', 'seed = ".seed"'),
        "member-size": good.replace("size = ", 'size = "big" #', 2).replace('size = "big" #', "size = ", 1),
        "no-members": good.split("[[member]]")[0] + "[image]\nformat = 1\n",
    }
    for name, text in broken.items():
        path = os.path.join(IN, f"profile-{name}.toml")
        with open(path, "w") as f:
            f.write(text)
        if text == good:
            failures.append(f"profile {name}: the case does not change the profile")
        case(f"profile: {name}", ["profile", path], status=1)
    bad = os.path.join(IN, "profile-syntax.toml")
    with open(bad, "w") as f:
        f.write("schema = = 1\n")
    case("profile: not TOML", ["profile", bad], how=f"imagetool: {bad}:", status=1)
    case("profile: no such file", ["profile", os.path.join(IN, "none.toml")], status=1)
    case("no arguments", ["profile"], how="usage", status=1)
    case("unknown command", ["bogus", "x"], how="usage", status=1)
    return profile


def gpt_image(path, inner):
    """The image `inner` as the one partition of a GPT with 512-byte sectors."""
    with open(inner, "rb") as f:
        data = f.read()
    disk = bytearray(64 * 512)
    disk[512:520] = b"EFI PART"
    struct.pack_into("<QII", disk, 512 + 72, 2, 4, 128)
    entry = 2 * 512 + 128
    disk[entry] = 0xEF
    struct.pack_into("<Q", disk, entry + 32, 64)
    with open(path, "wb") as f:
        f.write(bytes(disk) + data)


TREE = [("private", 0o40755, 0, 0, []), ("private/var", 0o40755, 0, 0, []),
        ("private/var/mobile", 0o40750, 501, 501, []),
        ("private/var/mobile/a.plist", 0o100600, 501, 501, ["com.example.tag", "com.apple.decmpfs"]),
        ("private/var/db", 0o40700, 0, 0, []), ("private/var/db/l", 0o120755, 0, 80, [])]


def apfs_cases(IN):
    img = lambda name: os.path.join(IN, name)   # noqa: E731
    fixtures.fake_apfs(img("os.img"), "FakeOS", role=1)
    fixtures.fake_apfs(img("data.img"), "FakeData", role=0x40, blocks=8)
    fixtures.fake_apfs(img("none.img"), "NoRole", role=0, blocks=8)
    fixtures.fake_apfs(img("odd.img"), "Odd", role=0x999, blocks=8)
    gpt_image(img("gpt.img"), img("data.img"))
    with open(img("junk.img"), "wb") as f:
        f.write(b"\0" * 8192 + b"koly" + b"\0" * 508)
    with open(img("zeros.img"), "wb") as f:
        f.write(b"\0" * 16384)
    with open(img("short.img"), "wb") as f:
        f.write(b"abc")
    for name in ("os.img", "data.img", "none.img", "odd.img", "gpt.img"):
        case(f"apfs-info {name}", ["apfs-info", img(name)], status=0)
    for name in ("junk.img", "zeros.img", "short.img"):
        case(f"apfs-info {name}", ["apfs-info", img(name)], status=1)
    case("apfs-info: no such file", ["apfs-info", img("nothing")], how="status", status=1)
    for hashed in (True, False):
        image = img(f"tree-{hashed}.img")
        fixtures.fake_apfs_tree(image, TREE, hashed)
        case(f"apfs-owners (hashed {hashed})", ["apfs-owners", image], status=0)
        case(f"apfs-owners private/var (hashed {hashed})", ["apfs-owners", image, "private/var"], status=0)
        case(f"apfs-owners /private//var/ (hashed {hashed})", ["apfs-owners", image, "/private//var/"], status=0)
        case(f"apfs-owners of a missing folder (hashed {hashed})", ["apfs-owners", image, "private/nothing"], status=1)
    case("apfs-owners of a volume without a tree", ["apfs-owners", img("os.img")], status=1)
    with open(img("tree-True.img"), "rb") as f:
        data = bytearray(f.read())
    data[7 * 4096 + 34] = 3                      # the leaf claims to be an index node
    with open(img("tree-level.img"), "wb") as f:
        f.write(data)
    case("apfs-owners: a node on the wrong level", ["apfs-owners", img("tree-level.img")], status=1)


def hashes_cases(IN):
    for n, size in enumerate((0, 1, 4096, 4096 * 5 + 123, 3 * (1 << 20) + 17)):
        image = os.path.join(IN, f"h{n}.img")
        with open(image, "wb") as f:
            f.write(noise(size, n))
        for chunk in (None, 4096, 65536):
            out = f"h{n}-{chunk}.hashes"
            args = ["hashes", image, out] + ([] if chunk is None else ["--chunk", str(chunk)])
            case(f"hashes of {size} bytes, chunk {chunk}", args, files=[out], status=0)
    case("hashes: no such image", ["hashes", os.path.join(IN, "nothing"), "x.hashes"], how="status", status=1)
    case("hashes: no output named", ["hashes", os.path.join(IN, "h1.img")], how="usage", status=1)


def hook(data):
    """What a hook does to the copied tree (test_image.py SeedTest.build)."""
    prefs = os.path.join(data, "mobile", "Library", "Preferences")
    with open(os.path.join(prefs, "com.example.setup.plist"), "wb") as f:
        f.write(b"rewritten")
    with open(os.path.join(prefs, "com.example.new.plist"), "wb") as f:
        f.write(b"new")
    os.chmod(os.path.join(prefs, "com.example.new.plist"), 0o600)
    os.makedirs(os.path.join(data, "oneios", "tools"))
    for rel in ("oneios", "oneios/tools"):
        os.chmod(os.path.join(data, rel), 0o755)
    for rel, mode in (("oneios/tools/probe", 0o755), ("oneios/café.txt", 0o644), ("oneios/" + "n" * 120, 0o640)):
        with open(os.path.join(data, rel), "w") as f:
            f.write("#!/bin/sh\n")
        os.chmod(os.path.join(data, rel), mode)
    os.symlink("tools/" + "t" * 110, os.path.join(data, "oneios", "long-link"))
    os.unlink(os.path.join(data, "root", ".profile"))


def seed_cases(IN):
    src = fixtures.fake_os_root(os.path.join(IN, "os"))
    var = os.path.join(src, "private", "var")
    os.mkfifo(os.path.join(var, "root", "pipe"))
    case("tree-copy", ["tree-copy", var, "data", "meta.json"], files=["meta.json", "data"], status=0)
    case("tree-copy into a folder that is not empty", ["tree-copy", var, "data", "meta2.json"], status=1)
    case("tree-copy of a folder that is not there", ["tree-copy", os.path.join(IN, "nothing"), "d2", "m2.json"], how="status", status=1)
    case("tree-copy: too few arguments", ["tree-copy", var, "d3"], how="usage", status=1)
    write("owners.tsv", "")
    case("seed-pack (as copied)", ["seed-pack", "data", "meta.json", "plain.seed", "--epoch", "1700000000", "--owners", "owners.tsv"],
         files=["plain.seed"], status=0)

    # stand in for the image's real owners and an xattr, then what a hook does
    def owners(side):
        with open(side.path("meta.json")) as f:
            meta = json.load(f)
        for rel, e in meta["entries"].items():
            e["u"], e["g"] = (501, 501) if rel.startswith("mobile") else (0, 0)
            e.pop("x", None)
        meta["entries"]["mobile/Library/Preferences/com.example.kept.plist"]["x"] = {"com.example.tag": "AAEC", "com.apple.a b/c": "dg=="}
        meta["entries"]["mobile"]["u"] = 10 ** 9              # more than the header's octal digits hold
        with open(side.path("meta.json"), "w") as f:
            json.dump(meta, f)
        hook(side.path("data"))
    each(owners)
    pack = ["seed-pack", "data", "meta.json", "data.seed", "--epoch", "1700000000"]
    case("seed-pack (after a hook)", pack, files=["data.seed"], status=0)
    write("rules.tsv", "# rules\n\noneios\t501\t501\t-\tR\noneios/tools/probe\t0\t0\t4755\n/empty/\t33\t33\t700\n.\t7\t7\t-\tR\r\nmobile\t1_0\t+5\t0o750\n")
    case("seed-pack with owner rules", pack + ["--owners", "rules.tsv"], files=["data.seed"], status=0)
    case("seed-pack with rules that are not there", pack + ["--owners", "nothing.tsv"], files=["data.seed"], status=0)
    for n, text in enumerate(("a b c\n", "../x\t0\t0\t-\n", "x\t0\t0\t-\tQ\n", "x\t0\t0\t9\n", "ok\t0\t0\t-\nx\tu\t0\t-\n", "x\t0\t0\n")):
        write(f"bad{n}.tsv", text)
        case(f"seed-pack: bad owner rule {n}", pack + ["--owners", f"bad{n}.tsv"], status=1)
    case("seed-pack without --epoch", ["seed-pack", "data", "meta.json", "x.seed"], how="usage", status=1)
    each(lambda side: os.mkfifo(side.path("data", "oneios", "pipe")))
    case("seed-pack of a tree with a fifo", pack, status=1)
    each(lambda side: os.unlink(side.path("data", "oneios", "pipe")))
    write("empty.json", '{"entries": {}}')
    case("seed-pack of a tree the meta file does not know", ["seed-pack", "data", "empty.json", "new.seed", "--epoch", "5"], files=["new.seed"], status=0)
    case("seed-pack of no tree", ["seed-pack", "nothing", "empty.json", "none.seed", "--epoch", "5"], files=["none.seed"], status=0)


def owners_cases(IN):
    """tree-copy --owners-from: a tree as a mount with owners ignored shows it, and images whose records name the owners."""
    var = os.path.join(IN, "mnt", "private", "var")
    os.makedirs(os.path.join(var, "mobile"))
    os.makedirs(os.path.join(var, "db"))
    with open(os.path.join(var, "mobile", "a.plist"), "w") as f:
        f.write("a")
    os.symlink("x", os.path.join(var, "db", "l"))
    for rel, mode in (("", 0o755), ("mobile", 0o750), ("db", 0o700), ("mobile/a.plist", 0o600)):
        os.chmod(os.path.join(var, rel), mode)
    link_mode = stat.S_IMODE(os.lstat(os.path.join(var, "db", "l")).st_mode)
    # the records name the xattrs this tree's files have here (macOS tags the files some processes make)
    shown = lambda rel: sorted(it._xattrs(os.path.join(var, rel)))   # noqa: E731

    def image(name, plist_mode=0o600, extra=False, plist_x=("com.apple.decmpfs",)):
        entries = [("private", 0o40755, 0, 0, []), ("private/var", 0o40755, 0, 0, []),
                   ("private/var/mobile", 0o40750, 501, 501, shown("mobile")),
                   ("private/var/mobile/a.plist", 0o100000 | plist_mode, 501, 501, shown("mobile/a.plist") + list(plist_x)),
                   ("private/var/db", 0o40700, 0, 0, shown("db")), ("private/var/db/l", 0o120000 | link_mode, 0, 80, shown("db/l"))]
        if extra:
            entries.append(("private/var/db/more", 0o100644, 0, 0, []))
        path = os.path.join(IN, name)
        fixtures.fake_apfs_tree(path, entries, True)
        return path

    def copy(name, img, sub="private/var", status=1, files=()):
        case(f"tree-copy --owners-from: {name}", ["tree-copy", var, f"o-{name}", f"o-{name}.json", "--owners-from", img, "--owners-path", sub],
             files=files, status=status)

    copy("good", image("owners-good.img"), status=0, files=["o-good.json", "o-good"])
    copy("mode", image("owners-mode.img", plist_mode=0o644))
    copy("xattrs", image("owners-x.img", plist_x=("com.example.other",)))
    copy("extra", image("owners-extra.img", extra=True))
    copy("folder", image("owners-good.img"), sub="private/nothing")
    copy("not-apfs", os.path.join(IN, "zeros.img"))
    with open(os.path.join(var, "db", "new"), "w") as f:
        f.write("n")
    copy("missing", image("owners-good.img"))
    os.unlink(os.path.join(var, "db", "new"))


def make_set(IN, profile, name, kernel=True, overlay=True):
    """A set folder of stand-in files in both tools' folders (test_image.py SetTest.make_set)."""
    p = it.load_profile(profile)

    def make(side):
        s = side.path(name)
        shutil.rmtree(s, True)
        for m in p["member"]:
            out = p["image"]["files"].get(m["role"])
            if out:
                os.makedirs(os.path.dirname(os.path.join(s, out)), exist_ok=True)
                shutil.copyfile(os.path.join(IN, "in", it.input_name(m)), os.path.join(s, out))
        with open(os.path.join(s, "data.seed"), "wb") as f:
            f.write(b"\0" * 10240)
        if kernel:
            with open(os.path.join(s, "kernel.kc"), "wb") as f:
                f.write(b"\xcf\xfa\xed\xfe" + b"stand-in kernel" * 1000)
        if overlay:
            os.makedirs(os.path.join(s, "overlay", "trust"))
            with open(os.path.join(s, "overlay", "trust", "owner.bin"), "wb") as f:
                f.write(b"owner bundle")
    each(make)


def set_cases(IN, profile):
    fixtures.fake_inputs(os.path.join(IN, "in"))
    write("seed.json", '{"format": "tar", "root": "/private/var", "entries": 1}')
    write("tweaks.json", '{"hook": "userland/image-hook", "info": {"n": [1, 2.5, null, true], "t": "caf\\u00e9"}}')
    outputs = ["set/manifest.json", "set/os.img.hashes", "set/systemos.img.hashes", "set/app.img.hashes", "set/kernel.kc.hashes"]
    make_set(IN, profile, "set")
    case("manifest", ["manifest", profile, "set", "--seed-info", "seed.json", "--tweaks", "tweaks.json"], files=outputs, status=0)
    case("manifest again (hash lists there)", ["manifest", profile, "set", "--seed-info", "seed.json", "--tweaks", "tweaks.json"], files=outputs, status=0)
    case("verify", ["verify", "set"], status=0)
    case("verify --quick", ["verify", "set", "--quick"], status=0)
    make_set(IN, profile, "bare", kernel=False, overlay=False)
    case("manifest without kernel, overlay, seed information", ["manifest", profile, "bare"], files=["bare/manifest.json"], status=0)
    case("verify of it", ["verify", "bare"], status=0)

    def refuse(name, change, more=()):
        make_set(IN, profile, "r")
        each(lambda side: change(side.path("r")))
        case(f"manifest refuses: {name}", ["manifest", profile, "r"] + list(more), status=1)

    def put(rel, data=b"x"):
        def change(d):
            os.makedirs(os.path.dirname(os.path.join(d, rel)), exist_ok=True)
            with open(os.path.join(d, rel), "wb") as f:
                f.write(data)
        return change

    refuse("a file missing", lambda d: os.unlink(os.path.join(d, "trust", "app.root_hash")))
    refuse("two files missing", lambda d: (os.unlink(os.path.join(d, "trust", "app.root_hash")), os.unlink(os.path.join(d, "data.seed"))))
    refuse("a stray file", put("stray.bin"))
    refuse("a stray file first", put("a-stray.bin"))
    refuse("a hidden file", put(".hidden"))
    refuse("a hash list without its image", put("lost.hashes"))
    refuse("a link", lambda d: os.symlink("os.img", os.path.join(d, "overlay", "link")))
    refuse("an image that is not APFS", put("app.img", b"\0" * 8192))
    write("inputs.json", json.dumps({"members": {"os": {"plain_sha256": "0" * 64}}}))
    refuse("not the input the fetch recorded", lambda d: None, ["--inputs", "inputs.json"])
    with open(profile) as f:
        text = f.read()
    pinned = os.path.join(IN, "pinned.toml")
    with open(pinned, "w") as f:
        f.write(text.replace('role = "app"', 'role = "app"\nsha256 = "' + "1" * 64 + '"'))
    make_set(IN, profile, "r")
    case("manifest refuses: not the image the profile pins", ["manifest", pinned, "r"], status=1)
    write("apfs-seed.json", '{"format": "apfs-raw"}')
    make_set(IN, profile, "r")
    case("manifest refuses: a seed that is not the APFS image it is said to be", ["manifest", profile, "r", "--seed-info", "apfs-seed.json"], status=1)
    case("manifest: no such seed information", ["manifest", profile, "r", "--seed-info", "nothing.json"], how="status", status=1)

    def broken(name, change, quick=False, how="exact"):
        def make(side):
            shutil.rmtree(side.path("copy"), True)
            shutil.copytree(side.path("set"), side.path("copy"))
            change(side.path("copy"))
        each(make)
        case(f"verify catches: {name}", ["verify", "copy"] + (["--quick"] if quick else []), how=how, status=1)

    def flip(rel, at):
        def change(d):
            with open(os.path.join(d, rel), "r+b") as f:
                f.seek(at)
                byte = f.read(1)
                f.seek(at)
                f.write(bytes([byte[0] ^ 1]))
        return change

    def edit(fn):
        def change(d):
            path = os.path.join(d, "manifest.json")
            with open(path) as f:
                m = json.load(f)
            fn(m)
            with open(path, "w") as f:
                json.dump(m, f)
        return change

    def grow(rel):
        def change(d):
            with open(os.path.join(d, rel), "ab") as f:
                f.write(b"x")
        return change

    def swap(d):
        data = noise(os.path.getsize(os.path.join(d, "os.img")), 99)
        with open(os.path.join(d, "x"), "wb") as f:
            f.write(data)
        it.write_hashes(os.path.join(d, "x"), os.path.join(d, "os.img.hashes"), 4096)
        os.unlink(os.path.join(d, "x"))
        edit(lambda m: [f.update(sha256=it.sha256_file(os.path.join(d, "os.img.hashes")))
                        for f in m["files"] if f["path"] == "os.img.hashes"])(d)

    def other_size(d):
        with open(os.path.join(d, "x"), "wb") as f:
            f.write(noise(5000, 1))
        it.write_hashes(os.path.join(d, "x"), os.path.join(d, "app.img.hashes"), 4096)
        os.unlink(os.path.join(d, "x"))
        edit(lambda m: [f.update(sha256=it.sha256_file(os.path.join(d, "app.img.hashes")), size=os.path.getsize(os.path.join(d, "app.img.hashes")))
                        for f in m["files"] if f["path"] == "app.img.hashes"])(d)

    def link(d):
        os.rename(os.path.join(d, "kernel.kc"), d + ".k")
        os.symlink(d + ".k", os.path.join(d, "kernel.kc"))

    broken("a byte of an image", flip("os.img", 20000))
    broken("a byte of a trust cache", flip("trust/static.trustcache", 3))
    broken("a byte of a hash list", flip("os.img.hashes", 40))
    broken("a file missing", lambda d: os.unlink(os.path.join(d, "systemos.img")))
    broken("two files missing", lambda d: (os.unlink(os.path.join(d, "systemos.img")), os.unlink(os.path.join(d, "app.img"))))
    broken("a file too many", put("extra"))
    broken("two files too many", lambda d: (put("zz")(d), put("overlay/aa")(d)))
    broken("a longer file, --quick", grow("app.img"), quick=True)
    broken("a longer file", grow("app.img"))
    broken("a longer file after a changed one", lambda d: (grow("os.img")(d), flip("app.img", 9)(d)))
    broken("a link in place of a file", link, quick=True)
    broken("a newer format", edit(lambda m: m.update(format=2)))
    broken("a format that is text", edit(lambda m: m.update(format="1")))
    broken("another name", edit(lambda m: m.update(name="other")))
    broken("a path that leaves the set", edit(lambda m: m["files"][0].update(path="../escape")))
    broken("a file listed twice", edit(lambda m: m["files"].append(dict(m["files"][0]))))
    broken("the manifest lists itself", edit(lambda m: m["files"][0].update(path="manifest.json")))
    broken("a SHA-256 that is not one", edit(lambda m: m["files"][0].update(sha256="xyz")))
    broken("a size below zero", edit(lambda m: m["files"][0].update(size=-1)))
    broken("a hash list that is not listed", edit(lambda m: m["files"][0].update(hashes="nothing")))
    broken("no build", edit(lambda m: m.pop("build")))
    broken("no files", edit(lambda m: m.update(files=[])))
    broken("another chunk size, --quick", edit(lambda m: m.update(chunk=8192)), quick=True)
    broken("a hash list of other content", swap)
    broken("a hash list for another size", other_size)
    broken("a hash list that is not one", lambda d: (put("app.img.hashes", b"junk" * 9)(d), edit(lambda m: [
        f.update(sha256=hashlib.sha256(b"junk" * 9).hexdigest(), size=36) for f in m["files"] if f["path"] == "app.img.hashes"])(d)))
    broken("no manifest", lambda d: os.unlink(os.path.join(d, "manifest.json")))
    broken("a manifest that is not JSON", put("manifest.json", b"{"), how="imagetool: copy/manifest.json:")
    broken("a manifest that is a list", put("manifest.json", b"[1]"))
    # unknown keys are ignored: a later writer may add them without a new format number
    each(lambda side: edit(lambda m: m.update(future_key={"a": 1}))(side.path("set")))
    case("verify with a key of a later writer", ["verify", "set"], status=0)
    case("verify of a folder that is no set", ["verify", "."], status=1)


def main():
    global PY, RS
    if not os.access(TOOL, os.X_OK):
        sys.exit(f"parity: no {TOOL} (build it: cargo build --release --manifest-path tools/oneios-tool/Cargo.toml; or pass the binary)")
    tmp = tempfile.mkdtemp(prefix="oneios-parity-")
    try:
        IN = os.path.join(tmp, "in")
        os.makedirs(IN)
        PY = Side("python", [sys.executable, "-I", PY_TOOL], os.path.join(tmp, "python"))
        RS = Side("rust", [TOOL, "image"], os.path.join(tmp, "rust"))
        profile = profile_cases(IN)
        apfs_cases(IN)
        hashes_cases(IN)
        seed_cases(IN)
        owners_cases(IN)
        set_cases(IN, profile)
    finally:
        shutil.rmtree(tmp, True)
    if failures:
        print("\n".join(failures))
        sys.exit(f"parity: {len(failures)} of {cases} cases DIFFERENT")
    print(f"parity: {cases} cases, all the same")


if __name__ == "__main__":
    main()
