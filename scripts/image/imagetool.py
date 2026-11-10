#!/usr/bin/env python3
"""The OneiOS image set: profile, inputs, hash lists, Data seed, manifest, archive.

    imagetool.py profile PROFILE [--shell | --members]      check a profile; print it as JSON, shell or a table
    imagetool.py check-ipsw PROFILE [--source URL|FILE]     compare the profile with the IPSW's directory and BuildManifest
    imagetool.py fetch PROFILE INDIR [-j N] [--only ROLE,..] [--source URL|FILE]
    imagetool.py aea-info FILE                              the key-service fields of an AEA header
    imagetool.py im4p IN OUT                                the payload of an IM4P (LZFSE decoded with `lzfse`)
    imagetool.py apfs-info IMAGE                            container and volume UUIDs of a raw APFS image
    imagetool.py hashes IMAGE OUT [--chunk BYTES]           the chunk hash list of an image
    imagetool.py tree-copy SRC DST META                     copy a tree, recording owners, modes, times, xattrs
    imagetool.py seed-pack TREE META OUT --epoch N [--owners FILE]   pack the Data seed (deterministic tar)
    imagetool.py manifest PROFILE SETDIR [--inputs FILE] [--seed-info FILE] [--tweaks FILE]
    imagetool.py verify SET [--quick]                       SET: a set folder or its .tar.zst
    imagetool.py pack SETDIR OUT.tar.zst                    the archive the OneOS fork imports, and OUT.tar.zst.sha256

Formats: docs/image-format.md. Standard library only (Python 3.11+); `zstd` for pack and archive verify,
`lzfse` for im4p. Linux and macOS.
"""
import base64
import hashlib
import json
import os
import plistlib
import re
import shlex
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.parse
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "fetch"))

FORMAT = 1                      # image-set format (manifest.json "format")
PROFILE_SCHEMA = 1
SET_NAME = "oneios-image"
HASHES_MAGIC = b"OIHL1\0\0\0"
DEFAULT_CHUNK = 1 << 20
SEGMENT = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9._+-]*$")   # a path segment the OneOS fork accepts
HEX64 = re.compile(r"^[0-9a-f]{64}$")
BLOCK = 1 << 22

# role -> (BuildManifest component, needs a name in [image.files], kind)
ROLES = {
    "os":              ("OS", True, "volume"),
    "systemos":        ("Cryptex1,SystemOS", True, "volume"),
    "app":             ("Cryptex1,AppOS", True, "volume"),
    "trust-static":    ("StaticTrustCache", True, "file"),
    "trust-system":    ("Cryptex1,SystemTrustCache", True, "file"),
    "trust-app":       ("Cryptex1,AppTrustCache", True, "file"),
    "roothash-os":     ("SystemVolume", True, "file"),
    "roothash-system": ("Cryptex1,SystemVolume", True, "file"),
    "roothash-app":    ("Cryptex1,AppVolume", True, "file"),
    "kernelcache":     ("KernelCache", False, "input"),
    "devicetree":      ("DeviceTree", False, "input"),
    "buildmanifest":   (None, False, "input"),
    "restore":         (None, False, "input"),
}
# xattrs that only describe how APFS stores an entry (compression, a link target): the copy has the plain
# content and the link itself, so they are dropped
DROP_XATTRS = {"com.apple.decmpfs", "com.apple.ResourceFork", "com.apple.fs.symlink"}


class Error(Exception):
    pass


# ---------------------------------------------------------------- hashing

def digest_file(path, algos=("sha256",)):
    hs = [hashlib.new(a) for a in algos]
    with open(path, "rb") as f:
        while block := f.read(BLOCK):
            for h in hs:
                h.update(block)
    return [h.hexdigest() for h in hs]


def sha256_file(path):
    return digest_file(path)[0]


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


# ---------------------------------------------------------------- profile

def load_profile(path):
    try:
        with open(path, "rb") as f:
            raw = f.read()
        p = tomllib.loads(raw.decode())
    except (OSError, tomllib.TOMLDecodeError, UnicodeDecodeError) as e:
        raise Error(f"{path}: {e}") from None

    def need(table, key, kind):
        if table not in p or key not in p[table] or not isinstance(p[table][key], kind):
            raise Error(f"{path}: [{table}] {key} is missing or not a {kind.__name__}")
        return p[table][key]

    if p.get("schema") != PROFILE_SCHEMA:
        raise Error(f"{path}: schema {p.get('schema')!r}, this tool reads schema {PROFILE_SCHEMA}")
    for key in ("name", "version", "build", "device", "board", "arch"):
        need("os", key, str)
    for key in ("chip_id", "board_id"):
        need("os", key, int)
    need("ipsw", "url", str)
    need("ipsw", "size", int)
    if not HEX64.match(need("ipsw", "sha256", str)):
        raise Error(f"{path}: [ipsw] sha256 is not 64 hex digits")
    if not re.match(r"^[0-9A-Za-z]+$", p["os"]["build"]):
        raise Error(f"{path}: [os] build has characters other than letters and digits")
    members = p.get("member")
    if not isinstance(members, list) or not members:
        raise Error(f"{path}: no [[member]]")
    seen = set()
    for m in members:
        role = m.get("role")
        if role not in ROLES:
            raise Error(f"{path}: member role {role!r} is unknown ({', '.join(ROLES)})")
        if role in seen:
            raise Error(f"{path}: two members with role {role}")
        seen.add(role)
        for key, kind in (("path", str), ("size", int), ("crc32", str)):
            if not isinstance(m.get(key), kind):
                raise Error(f"{path}: member {role}: {key} is missing or not a {kind.__name__}")
        if not re.match(r"^[0-9a-f]{8}$", m["crc32"]):
            raise Error(f"{path}: member {role}: crc32 is not 8 hex digits")
        if ROLES[role][0] and m.get("component", ROLES[role][0]) != ROLES[role][0]:
            raise Error(f"{path}: member {role}: component must be {ROLES[role][0]}")
        if m.get("aea", False) != m["path"].endswith(".aea"):
            raise Error(f"{path}: member {role}: aea must be true exactly when the path ends in .aea")
        for key in ("sha256", "sha384", "plain_sha256"):
            if key in m and not re.match(r"^[0-9a-f]+$", m[key]):
                raise Error(f"{path}: member {role}: {key} is not hex")
    for role in ("os", "systemos", "app", "trust-static", "trust-system", "trust-app", "buildmanifest"):
        if role not in seen:
            raise Error(f"{path}: no member with role {role}")
    image = p.get("image", {})
    if image.get("format") != FORMAT:
        raise Error(f"{path}: [image] format {image.get('format')!r}, this tool writes format {FORMAT}")
    for key in ("id", "archive", "stage"):
        need("image", key, str)
    files = image.get("files", {})
    names = set()
    for role in seen:
        if ROLES[role][1]:
            name = files.get(role)
            if not isinstance(name, str):
                raise Error(f"{path}: [image.files] has no name for {role}")
            check_set_path(name, f"{path}: [image.files] {role}")
            if name in names:
                raise Error(f"{path}: [image.files] {name} is used twice")
            names.add(name)
    for key in ("seed", "kernel"):
        check_set_path(need("image", key, str), f"{path}: [image] {key}")
    check_set_path(image["archive"], f"{path}: [image] archive")
    chunk = image.get("chunk", DEFAULT_CHUNK)
    if not isinstance(chunk, int) or chunk < 4096 or chunk & (chunk - 1):
        raise Error(f"{path}: [image] chunk must be a power of two of at least 4096")
    data = p.get("data", {})
    try:
        uuid.UUID(data.get("volume_uuid", ""))
    except ValueError:
        raise Error(f"{path}: [data] volume_uuid is not a UUID") from None
    if not isinstance(data.get("epoch"), int):
        raise Error(f"{path}: [data] epoch (seconds, the time of files the build adds) is missing")
    p["_sha256"] = sha256_bytes(raw)
    p["_file"] = os.path.basename(path)
    return p


def check_set_path(path, what):
    parts = path.split("/")
    if not parts or any(not SEGMENT.match(s) for s in parts):
        raise Error(f"{what}: {path!r} is not a set path (segments of letters, digits and ._+-, no leading dot)")


def member_of(profile, role):
    for m in profile["member"]:
        if m["role"] == role:
            return m
    return None


def input_name(m):
    """The file name of a member in inputs/<build>/ (AEA members: the decrypted image, without .aea)."""
    name = os.path.basename(m["path"])
    return name[:-4] if m.get("aea") else name


def cmd_profile(args):
    p = load_profile(args[0])
    if "--shell" in args:
        o, i, d = p["os"], p["image"], p["data"]
        pairs = {
            "P_NAME": o["name"], "P_VERSION": o["version"], "P_BUILD": o["build"], "P_DEVICE": o["device"],
            "P_BOARD": o["board"], "P_IPSW_URL": p["ipsw"]["url"], "P_SET_ID": i["id"], "P_ARCHIVE": i["archive"],
            "P_STAGE": i["stage"], "P_SEED": i["seed"], "P_KERNEL": i["kernel"], "P_CHUNK": i.get("chunk", DEFAULT_CHUNK),
            "P_DATA_UUID": d["volume_uuid"], "P_EPOCH": d["epoch"], "P_DATA_SOURCE": d.get("source", "private/var"),
            "P_NEED_GIB": p["ipsw"].get("need_gib", 30),
        }
        for k, v in pairs.items():
            print(f"{k}={shlex.quote(str(v))}")
    elif "--members" in args:
        for m in p["member"]:
            out = p["image"]["files"].get(m["role"], "-")
            print("\t".join([m["role"], m["path"], input_name(m), "1" if m.get("aea") else "0", str(m["size"]), out]))
    else:
        print(json.dumps({k: v for k, v in p.items()}, indent=2))


# ---------------------------------------------------------------- IPSW

def build_identity(manifest, profile):
    o = profile["os"]
    if (manifest.get("ProductVersion"), manifest.get("ProductBuildVersion")) != (o["version"], o["build"]) \
            or o["device"] not in manifest.get("SupportedProductTypes", []):
        raise Error(f"this IPSW is {manifest.get('ProductVersion')} ({manifest.get('ProductBuildVersion')}) for "
                    f"{', '.join(manifest.get('SupportedProductTypes', []))}; the profile wants {o['version']} "
                    f"({o['build']}) for {o['device']}")
    variant = profile["ipsw"].get("variant")
    for item in manifest["BuildIdentities"]:
        info = item["Info"]
        if info["DeviceClass"].lower() == o["board"].lower() and info["RestoreBehavior"] == "Erase" \
                and int(item["ApChipID"], 16) == o["chip_id"] and int(item["ApBoardID"], 16) == o["board_id"] \
                and (variant is None or info.get("Variant") == variant):
            return item
    raise Error(f"no Erase build identity for {o['board']} in this IPSW")


def check_members(profile, infos, manifest):
    """Compare the profile's members with the ZIP directory (name -> ZipInfo) and the BuildManifest. Returns problems."""
    bad = []
    identity = build_identity(manifest, profile) if manifest else None
    for m in profile["member"]:
        info = infos.get(m["path"])
        if info is None:
            bad.append(f"{m['role']}: {m['path']} is not in the IPSW")
            continue
        if info.file_size != m["size"]:
            bad.append(f"{m['role']}: {m['path']} is {info.file_size} bytes, the profile says {m['size']}")
        if f"{info.CRC:08x}" != m["crc32"]:
            bad.append(f"{m['role']}: {m['path']} has CRC-32 {info.CRC:08x}, the profile says {m['crc32']}")
        component = ROLES[m["role"]][0]
        if identity and component:
            path = identity["Manifest"].get(component, {}).get("Info", {}).get("Path")
            if path != m["path"]:
                bad.append(f"{m['role']}: BuildManifest {component} is {path}, the profile says {m['path']}")
    return bad


def cmd_check_ipsw(args):
    import remotezip
    profile = load_profile(args[0])
    spec = args[args.index("--source") + 1] if "--source" in args else profile["ipsw"]["url"]
    z = remotezip.RemoteZip(spec)
    if z.source.size != profile["ipsw"]["size"]:
        raise Error(f"the IPSW is {z.source.size} bytes, the profile says {profile['ipsw']['size']}")
    infos = {i.filename: i for i in z.infos()}
    manifest = plistlib.loads(z.read(member_of(profile, "buildmanifest")["path"]))
    bad = check_members(profile, infos, manifest)
    total = 0
    for m in profile["member"]:
        info = infos.get(m["path"])
        if info:
            total += info.compress_size
            print(f"{info.file_size:>13} {info.compress_size:>13} {info.CRC:08x} {m['role']:<16} {m['path']}")
    print(f"{len(profile['member'])} members, {total} bytes to download ({total / (1 << 30):.2f} GiB) of {z.source.size}")
    if bad:
        raise Error("the profile does not match the IPSW:\n  " + "\n  ".join(bad))
    print(f"ok: {profile['os']['name']} matches {spec}")


def cmd_fetch(args):
    import remotezip
    profile, indir = load_profile(args[0]), args[1]
    jobs = int(args[args.index("-j") + 1]) if "-j" in args else 8
    only = set(args[args.index("--only") + 1].split(",")) if "--only" in args else None
    spec = args[args.index("--source") + 1] if "--source" in args else profile["ipsw"]["url"]
    os.makedirs(indir, exist_ok=True)
    record_path = os.path.join(indir, "inputs.json")
    record = {"build": profile["os"]["build"], "profile": profile["_file"], "members": {}}
    if os.path.isfile(record_path):
        with open(record_path) as f:
            old = json.load(f)
        if old.get("build") == profile["os"]["build"]:
            record["members"] = old.get("members", {})
    z = remotezip.RemoteZip(spec)
    infos = {i.filename: i for i in z.infos()}
    bm = member_of(profile, "buildmanifest")
    manifest = plistlib.loads(z.read(bm["path"]))
    bad = check_members(profile, infos, manifest)
    if bad:
        raise Error("the profile does not match the IPSW:\n  " + "\n  ".join(bad))
    identity = build_identity(manifest, profile)
    for m in profile["member"]:
        role = m["role"]
        if only and role not in only:
            continue
        name = os.path.basename(m["path"])
        out = os.path.join(indir, name)
        plain = os.path.join(indir, input_name(m))
        known = record["members"].get(role, {})
        # an AEA whose decrypted image is recorded and still there is not downloaded again
        if m.get("aea") and not os.path.isfile(out) and os.path.isfile(plain) and known.get("member") == m["path"] \
                and known.get("crc32") == m["crc32"] and known.get("plain_size") == os.path.getsize(plain):
            print(f"{m['path']}: decrypted image present")
            continue
        z.get(m["path"], out, jobs)
        sha256, sha384 = digest_file(out, ("sha256", "sha384"))
        for key, got in (("sha256", sha256), ("sha384", sha384)):
            if key in m and m[key] != got:
                os.replace(out, out + ".bad")
                raise Error(f"{m['path']}: {key} {got}, the profile pins {m[key]} (kept as {name}.bad)")
        entry = {"member": m["path"], "size": m["size"], "crc32": m["crc32"], "sha256": sha256, "sha384": sha384,
                 "pinned": [k for k in ("sha256", "sha384") if k in m] + ["size", "crc32"]}
        component = ROLES[role][0]
        if component:
            digest = identity["Manifest"][component].get("Digest", b"").hex()
            entry["buildmanifest_digest"] = digest
            entry["buildmanifest_digest_is_file_sha384"] = digest == sha384
        record["members"][role] = entry
        with open(record_path + ".tmp", "w") as f:
            json.dump(record, f, indent=2)
            f.write("\n")
        os.replace(record_path + ".tmp", record_path)
    print(f"fetched into {indir}; hashes in {record_path}")


def record_plain(indir, role, plain):
    """After an AEA was decrypted: note the plain image's size and SHA-256 in inputs.json."""
    record_path = os.path.join(indir, "inputs.json")
    with open(record_path) as f:
        record = json.load(f)
    record["members"][role]["plain_size"] = os.path.getsize(plain)
    record["members"][role]["plain_sha256"] = sha256_file(plain)
    with open(record_path + ".tmp", "w") as f:
        json.dump(record, f, indent=2)
        f.write("\n")
    os.replace(record_path + ".tmp", record_path)


# ---------------------------------------------------------------- AEA, IM4P

def aea_info(data):
    """The auth-data fields of an AEA1 header: {key: bytes}."""
    if data[:4] != b"AEA1":
        raise Error("not an AEA1 archive")
    profile = int.from_bytes(data[4:7], "little")
    (size,) = struct.unpack_from("<I", data, 8)
    if 12 + size > len(data):
        raise Error("the AEA header is longer than the data given")
    fields, at, end = {}, 12, 12 + size
    while at + 4 <= end:
        (n,) = struct.unpack_from("<I", data, at)
        if n < 5 or at + n > end:
            break
        key, _, value = data[at + 4:at + n].partition(b"\0")
        fields[key.decode("ascii", "replace")] = value
        at += n
    return profile, fields


def cmd_aea_info(args):
    with open(args[0], "rb") as f:
        profile, fields = aea_info(f.read(1 << 16))
    print(f"profile {profile}")
    for k, v in fields.items():
        text = v.decode("ascii", "replace")
        print(f"{k}: {text if len(text) <= 100 else text[:60] + '... (' + str(len(v)) + ' bytes)'}")
    if not any(k in fields for k in ("com.apple.wkms.fcs-key-url", "com.apple.wkms.url")):
        raise Error("no key-service field: this archive's key cannot be fetched from Apple")


def _der(data, at):
    tag, length = data[at], data[at + 1]
    at += 2
    if length & 0x80:
        count = length & 0x7F
        length = int.from_bytes(data[at:at + count], "big")
        at += count
    return tag, at, length


def im4p_payload(data):
    """(type, payload) of an IM4P: DER SEQUENCE { "IM4P", type, description, data, ... }."""
    tag, start, length = _der(data, 0)
    if tag != 0x30:
        raise Error("not an IM4P")
    at, items = start, []
    while at < start + length and len(items) < 4:
        _, body, size = _der(data, at)
        items.append(data[body:body + size])
        at = body + size
    if len(items) < 4 or items[0] != b"IM4P":
        raise Error("not an IM4P")
    return items[1].decode("ascii", "replace"), items[3]


def cmd_im4p(args):
    with open(args[0], "rb") as f:
        kind, payload = im4p_payload(f.read())
    if payload[:4] in (b"bvx2", b"bvx1", b"bvxn", b"bvx-"):
        tool = shutil.which("lzfse")
        if not tool:
            raise Error("the payload is LZFSE: install lzfse (github.com/lzfse/lzfse)")
        with tempfile.NamedTemporaryFile(dir=os.path.dirname(os.path.abspath(args[1]))) as packed:
            packed.write(payload)
            packed.flush()
            subprocess.run([tool, "-decode", "-i", packed.name, "-o", args[1]], check=True)
        with open(args[1], "rb") as f:
            payload = f.read()
    elif payload[:8] == b"complzss":
        raise Error("the payload is LZSS, which this tool does not decode")
    if payload[:4] == b"\xca\xfe\xba\xbe":            # fat: keep the arm64 slice
        count = struct.unpack(">I", payload[4:8])[0]
        slices = [struct.unpack(">5I", payload[8 + 20 * i:28 + 20 * i]) for i in range(count)]
        arm64 = [s for s in slices if s[0] == 0x0100000C]
        if len(arm64) != 1:
            raise Error("expected one arm64 slice")
        payload = payload[arm64[0][2]:arm64[0][2] + arm64[0][3]]
    with open(args[1], "wb") as f:
        f.write(payload)
    print(f"{kind} {len(payload)} bytes sha256 {sha256_bytes(payload)} -> {args[1]}")


# ---------------------------------------------------------------- APFS (read-only facts for the manifest)

APFS_ROLES = {0x0001: "system", 0x0002: "user", 0x0004: "recovery", 0x0008: "vm", 0x0010: "preboot",
              0x0040: "data", 0x00C0: "update", 0x0100: "xart", 0x0140: "hardware"}


def apfs_info(path):
    """Container offset, block size, UUIDs and volumes of a raw APFS image (plain, or one APFS partition in a GPT)."""
    with open(path, "rb") as f:
        size = os.fstat(f.fileno()).st_size

        def at(offset, count):
            f.seek(offset)
            data = f.read(count)
            if len(data) != count:
                raise Error(f"{path}: short read at {offset}")
            return data

        head = at(0, 4096)
        base = 0
        if head[32:36] != b"NXSB":
            base = None
            for sector in (512, 4096):
                gpt = at(sector, 92)
                if gpt[:8] != b"EFI PART":
                    continue
                entries_lba, count, entry_size = struct.unpack_from("<QII", gpt, 72)
                for i in range(min(count, 128)):
                    entry = at(entries_lba * sector + i * entry_size, entry_size)
                    first = struct.unpack_from("<Q", entry, 32)[0]
                    if any(entry[:16]) and at(first * sector, 36)[32:36] == b"NXSB":
                        base = first * sector
                        break
                if base is not None:
                    break
            if base is None:
                kind = "a UDIF disk image (convert it to raw first)" if at(size - 512, 4) == b"koly" else "not APFS"
                raise Error(f"{path}: no APFS container found: {kind}")
            head = at(base, 4096)
        block = struct.unpack_from("<I", head, 36)[0]
        blocks = struct.unpack_from("<Q", head, 40)[0]
        container_uuid = str(uuid.UUID(bytes=head[72:88])).upper()
        omap_oid = struct.unpack_from("<Q", head, 160)[0]
        max_fs = struct.unpack_from("<I", head, 180)[0]
        fs_oids = [o for o in struct.unpack_from(f"<{min(max_fs, 100)}Q", head, 184) if o]

        def node(paddr):
            return at(base + paddr * block, block)

        omap = node(omap_oid)
        root = struct.unpack_from("<Q", omap, 48)[0]

        def lookup(paddr, oid, is_root=True):
            """The newest mapping of oid in the container object map: its block address."""
            data = node(paddr)
            flags, level, nkeys = struct.unpack_from("<HHI", data, 32)
            toc_off, toc_len = struct.unpack_from("<HH", data, 40)
            toc = 56 + toc_off
            keys = toc + toc_len
            vals = block - (40 if flags & 1 else 0)
            best = None
            for i in range(nkeys):
                if flags & 4:
                    k, v = struct.unpack_from("<HH", data, toc + 4 * i)
                else:
                    k, _, v, _ = struct.unpack_from("<HHHH", data, toc + 8 * i)
                key_oid, key_xid = struct.unpack_from("<QQ", data, keys + k)
                if level == 0:
                    if key_oid == oid and (best is None or key_xid > best[0]):
                        best = (key_xid, struct.unpack_from("<Q", data, vals - v + 8)[0])
                else:
                    if (key_oid, key_xid) <= (oid, 0xFFFFFFFFFFFFFFFF):
                        best = struct.unpack_from("<Q", data, vals - v)[0]
            if level == 0:
                return best[1] if best else None
            return lookup(best, oid, False) if best is not None else None

        volumes = []
        for oid in fs_oids:
            paddr = lookup(root, oid)
            if paddr is None:
                raise Error(f"{path}: volume object {oid} is not in the container's object map")
            sb = node(paddr)
            if sb[32:36] != b"APSB":
                raise Error(f"{path}: block {paddr} is not a volume superblock")
            incompat = struct.unpack_from("<Q", sb, 56)[0]
            role = struct.unpack_from("<H", sb, 964)[0]
            volumes.append({
                "name": sb[704:960].split(b"\0", 1)[0].decode("utf-8", "replace"),
                "uuid": str(uuid.UUID(bytes=sb[240:256])).upper(),
                "role": APFS_ROLES.get(role, "none" if role == 0 else hex(role)),
                "case_sensitive": not incompat & 1,
                "sealed": bool(incompat & 0x20),
            })
    return {"container_offset": base, "block_size": block, "block_count": blocks, "container_uuid": container_uuid,
            "volumes": volumes}


# ---------------------------------------------------------------- chunk hash list

def write_hashes(image, out, chunk=DEFAULT_CHUNK):
    """IMAGE's chunk hash list. Returns (size, sha256 of the image)."""
    whole = hashlib.sha256()
    size = os.path.getsize(image)
    count = -(-size // chunk)
    with open(image, "rb") as f, open(out, "wb") as o:
        o.write(HASHES_MAGIC + struct.pack("<IIQQ", chunk, 1, size, count))
        done = 0
        for _ in range(count):
            data = f.read(chunk)
            done += len(data)
            whole.update(data)
            o.write(hashlib.sha256(data).digest())
        if done != size or f.read(1):
            raise Error(f"{image}: changed while it was read")
    return size, whole.hexdigest()


def read_hashes(path):
    with open(path, "rb") as f:
        data = f.read()
    if data[:8] != HASHES_MAGIC or len(data) < 32:
        raise Error(f"{path}: not a chunk hash list")
    chunk, algo, size, count = struct.unpack_from("<IIQQ", data, 8)
    if algo != 1 or chunk < 4096 or chunk & (chunk - 1) or count != -(-size // chunk) or len(data) != 32 + 32 * count:
        raise Error(f"{path}: bad chunk hash list header")
    return chunk, size, data[32:]


def check_hashes(image, hashes):
    chunk, size, digests = read_hashes(hashes)
    if os.path.getsize(image) != size:
        raise Error(f"{image}: {os.path.getsize(image)} bytes, its hash list says {size}")
    with open(image, "rb") as f:
        for i in range(len(digests) // 32):
            if hashlib.sha256(f.read(chunk)).digest() != digests[32 * i:32 * i + 32]:
                raise Error(f"{image}: chunk {i} (offset {i * chunk}) differs from its hash list")


# ---------------------------------------------------------------- Data seed

def _xattrs(path):
    out = {}
    try:
        names = os.listxattr(path, follow_symlinks=False)
    except (OSError, AttributeError):
        return out
    for name in names:
        plain = name[5:] if name.startswith("user.") and "com.apple" in name else name
        if plain in DROP_XATTRS or name.startswith(("security.", "system.", "trusted.")):
            continue
        try:
            out[plain] = base64.b64encode(os.getxattr(path, name, follow_symlinks=False)).decode()
        except OSError:
            pass
    return out


def tree_copy(src, dst, meta_path):
    """Copy SRC to DST as the calling user and record what a non-root copy loses: owner, mode, time, xattrs."""
    if os.path.exists(dst) and os.listdir(dst):
        raise Error(f"{dst}: not empty")
    os.makedirs(dst, exist_ok=True)
    top = os.lstat(src)
    meta = {".": {"t": "d", "m": stat.S_IMODE(top.st_mode), "u": top.st_uid, "g": top.st_gid, "s": int(top.st_mtime)}}
    skipped = []
    stack = [""]
    while stack:
        rel_dir = stack.pop()
        with os.scandir(os.path.join(src, rel_dir)) as it:
            entries = sorted(it, key=lambda e: e.name)
        for e in entries:
            rel = os.path.join(rel_dir, e.name)
            s, d = os.path.join(src, rel), os.path.join(dst, rel)
            st = os.lstat(s)
            item = {"m": stat.S_IMODE(st.st_mode), "u": st.st_uid, "g": st.st_gid, "s": int(st.st_mtime)}
            x = _xattrs(s)
            if x:
                item["x"] = x
            if stat.S_ISDIR(st.st_mode):
                item["t"] = "d"
                os.mkdir(d, 0o700)
                stack.append(rel)
            elif stat.S_ISLNK(st.st_mode):
                item["t"] = "l"
                os.symlink(os.readlink(s), d)
            elif stat.S_ISREG(st.st_mode):
                item["t"] = "f"
                shutil.copyfile(s, d, follow_symlinks=False)
                os.chmod(d, 0o600)
                os.utime(d, (item["s"], item["s"]))
            else:
                skipped.append(rel)        # sockets, fifos, devices: nothing a seed carries
                continue
            meta[rel] = item
    with open(meta_path, "w") as f:
        json.dump({"entries": meta, "skipped": skipped}, f)
    return len(meta) - 1, skipped


def read_owners(path):
    """Owner rules a hook wrote: tab-separated PATH, UID, GID, MODE (octal or -), optional R (the whole tree below)."""
    rules = []
    if not path or not os.path.isfile(path):
        return rules
    with open(path) as f:
        for n, line in enumerate(f, 1):
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            try:
                if len(parts) not in (4, 5) or (len(parts) == 5 and parts[4] != "R"):
                    raise ValueError
                rel = os.path.normpath(parts[0].strip("/")) if parts[0].strip("/") else "."
                if rel.startswith(".."):
                    raise ValueError
                rules.append((rel, int(parts[1]), int(parts[2]), None if parts[3] == "-" else int(parts[3], 8),
                              len(parts) == 5))
            except ValueError:
                raise Error(f"{path}:{n}: want PATH<tab>UID<tab>GID<tab>MODE[<tab>R]") from None
    return rules


def seed_pack(tree, meta_path, out, epoch, owners=None):
    """Pack TREE as the Data seed: a deterministic pax tar whose owners, modes and times are the source's.

    An entry the copy made and nobody touched keeps the source's owner, mode and time. A new entry takes its
    parent's owner, its own mode and the time `epoch`. A rewritten file keeps the source's owner and mode and
    takes `epoch`. Owner rules override all of that. Returns (entries, added, changed).
    """
    with open(meta_path) as f:
        meta = json.load(f)["entries"]
    rules = read_owners(owners)
    exact = {r[0]: r for r in rules}
    recursive = sorted((r for r in rules if r[4]), key=lambda r: len(r[0]))
    paths = []
    for base, dirs, files in os.walk(tree):
        dirs.sort()
        for name in dirs + files:
            paths.append(os.path.relpath(os.path.join(base, name), tree))
    paths.sort(key=lambda p: p.encode())
    final = {}        # rel -> (uid, gid)
    top = meta.get(".", {"u": 0, "g": 0})
    final["."] = (top["u"], top["g"])
    added = changed = 0
    with tarfile.open(out, "w", format=tarfile.PAX_FORMAT) as tar:
        for rel in paths:
            full = os.path.join(tree, rel)
            st = os.lstat(full)
            kind = "d" if stat.S_ISDIR(st.st_mode) else "l" if stat.S_ISLNK(st.st_mode) else \
                "f" if stat.S_ISREG(st.st_mode) else None
            if kind is None:
                raise Error(f"{rel}: only files, folders and symbolic links can be in the seed")
            old = meta.get(rel)
            if old and old["t"] == kind:
                uid, gid, mode, mtime = old["u"], old["g"], old["m"], old["s"]
                if kind == "f" and int(st.st_mtime) != old["s"]:
                    mtime = epoch
                    changed += 1
                xattrs = old.get("x", {})
            else:
                uid, gid = final[os.path.dirname(rel) or "."]
                mode, mtime, xattrs = stat.S_IMODE(st.st_mode), epoch, {}
                added += 1
            for r in recursive:
                if rel == r[0] or rel.startswith(r[0] + "/") or r[0] == ".":
                    uid, gid = r[1], r[2]
                    if r[3] is not None and kind != "l":
                        mode = r[3]
            if rel in exact:
                r = exact[rel]
                uid, gid = r[1], r[2]
                if r[3] is not None:
                    mode = r[3]
            final[rel] = (uid, gid)
            info = tarfile.TarInfo(rel)
            info.uid, info.gid, info.uname, info.gname, info.mtime = uid, gid, "", "", mtime
            info.mode = 0o777 if kind == "l" else mode
            if xattrs:
                info.pax_headers = {"LIBARCHIVE.xattr." + urllib.parse.quote(k, safe=".-_"): v
                                    for k, v in sorted(xattrs.items())}
            if kind == "d":
                info.type = tarfile.DIRTYPE
                tar.addfile(info)
            elif kind == "l":
                info.type, info.linkname = tarfile.SYMTYPE, os.readlink(full)
                tar.addfile(info)
            else:
                info.size = st.st_size
                with open(full, "rb") as f:
                    tar.addfile(info, f)
    return len(paths), added, changed


# ---------------------------------------------------------------- manifest

def set_files(setdir):
    out = []
    for base, dirs, files in os.walk(setdir):
        dirs.sort()
        for name in sorted(files):
            rel = os.path.relpath(os.path.join(base, name), setdir)
            if rel != "manifest.json":
                out.append(rel)
    return sorted(out)


def make_manifest(profile, setdir, inputs=None, seed_info=None, tweaks=None):
    o, image = profile["os"], profile["image"]
    names = image["files"]
    by_name = {v: k for k, v in names.items()}
    chunk = image.get("chunk", DEFAULT_CHUNK)
    present = set_files(setdir)
    files = []
    for rel in present:
        check_set_path(rel, "set file")
        if os.path.islink(os.path.join(setdir, rel)):
            raise Error(f"{rel}: a link; the set holds regular files only")
    for rel in present:
        if rel.endswith(".hashes"):
            continue
        full = os.path.join(setdir, rel)
        role = by_name.get(rel)
        entry = {"path": rel}
        if role and ROLES[role][2] == "volume" or rel == image["kernel"]:
            hashes = rel + ".hashes"
            size, digest = write_hashes(full, os.path.join(setdir, hashes), chunk)
            entry.update(size=size, sha256=digest)
        else:
            entry.update(size=os.path.getsize(full), sha256=sha256_file(full))
        if role:
            m = member_of(profile, role)
            entry["role"] = role
            entry["source"] = {"component": ROLES[role][0], "member": m["path"], "decrypted": bool(m.get("aea"))}
            if ROLES[role][2] == "volume":
                entry.update(format="apfs-raw", read_only=True, hashes=hashes, apfs=apfs_info(full))
                rec = (inputs or {}).get("members", {}).get(role, {})
                want = rec.get("plain_sha256") if m.get("aea") else rec.get("sha256")
                if want and want != entry["sha256"]:
                    raise Error(f"{rel}: differs from the input the fetch recorded ({want})")
                pin = m.get("plain_sha256") if m.get("aea") else m.get("sha256")
                if pin and pin != entry["sha256"]:
                    raise Error(f"{rel}: SHA-256 {entry['sha256']}, the profile pins {pin}")
            else:
                entry["format"] = "img4-payload" if role.startswith("trust-") else "root-hash"
        elif rel == image["seed"]:
            entry.update(role="data-seed", **(seed_info or {"format": "tar"}))
            if entry["format"] == "apfs-raw":
                entry["apfs"] = apfs_info(full)
        elif rel == image["kernel"]:
            entry.update(role="kernel", format="kernel-collection", read_only=True, hashes=rel + ".hashes")
        elif rel.startswith("overlay/"):
            entry["role"] = "overlay"
        else:
            raise Error(f"{rel}: not a file of this profile's set (names: [image.files], [image] seed, kernel, overlay/)")
        files.append(entry)
    for rel in set_files(setdir):
        if rel.endswith(".hashes"):
            if not any(f.get("hashes") == rel for f in files):
                raise Error(f"{rel}: a hash list without its image")
            files.append({"path": rel, "size": os.path.getsize(os.path.join(setdir, rel)),
                          "sha256": sha256_file(os.path.join(setdir, rel)), "role": "hashes", "format": "oneios-hash-list-1"})
    files.sort(key=lambda f: f["path"])
    have = {f["path"] for f in files}
    missing = [names[r] for r in names if names[r] not in have] + ([image["seed"]] if image["seed"] not in have else [])
    if missing:
        raise Error("the set is missing " + ", ".join(sorted(missing)))
    manifest = {
        "format": FORMAT,
        "name": SET_NAME,
        "id": image["id"],
        "os": "iOS" if o["device"].startswith("iPhone") else o.get("family", "iOS"),
        "version": o["version"],
        "build": o["build"],
        "device": o["device"],
        "board": o["board"],
        "arch": o["arch"],
        "chip_id": o["chip_id"],
        "board_id": o["board_id"],
        "profile": {"name": o["name"], "file": profile["_file"], "sha256": profile["_sha256"]},
        "source": {"ipsw_url": profile["ipsw"]["url"], "ipsw_size": profile["ipsw"]["size"],
                   "ipsw_sha256": profile["ipsw"]["sha256"]},
        "stage": image["stage"],
        "chunk": chunk,
        "data": {"seed": image["seed"], "volume_uuid": profile["data"]["volume_uuid"].upper(),
                 "volume_name": profile["data"].get("volume_name", "Data"),
                 "case_sensitive": True, "store_gib": profile["data"].get("store_gib", 16)},
        "kernel": image["kernel"] if image["kernel"] in have else None,
        "tweaks": tweaks,
        "files": files,
    }
    text = json.dumps(manifest, indent=2) + "\n"
    with open(os.path.join(setdir, "manifest.json"), "w") as f:
        f.write(text)
    return manifest


def load_manifest(path):
    try:
        with open(path) as f:
            m = json.load(f)
    except (OSError, ValueError) as e:
        raise Error(f"{path}: {e}") from None
    if not isinstance(m, dict) or m.get("name") != SET_NAME:
        raise Error(f"{path}: not a OneiOS image manifest")
    if not isinstance(m.get("format"), int) or m["format"] < 1:
        raise Error(f"{path}: no format number")
    if m["format"] > FORMAT:
        raise Error(f"{path}: format {m['format']} is newer than this tool reads ({FORMAT})")
    for key in ("version", "build", "device"):
        if not isinstance(m.get(key), str):
            raise Error(f"{path}: {key} is missing")
    files = m.get("files")
    if not isinstance(files, list) or not files:
        raise Error(f"{path}: no files")
    seen = set()
    for f in files:
        if not isinstance(f, dict) or not isinstance(f.get("path"), str) or not isinstance(f.get("size"), int) \
                or f["size"] < 0 or not isinstance(f.get("sha256"), str) or not HEX64.match(f["sha256"]):
            raise Error(f"{path}: a file entry without path, size or sha256")
        check_set_path(f["path"], path)
        if f["path"] == "manifest.json" or f["path"] in seen:
            raise Error(f"{path}: {f['path']} listed twice or not allowed")
        seen.add(f["path"])
    for f in files:
        if "hashes" in f and f["hashes"] not in seen:
            raise Error(f"{path}: {f['path']} names the hash list {f['hashes']}, which is not listed")
    return m


def verify_dir(setdir, quick=False):
    """Check a set folder against its manifest. quick: names and sizes only. Returns the manifest."""
    m = load_manifest(os.path.join(setdir, "manifest.json"))
    listed = {f["path"]: f for f in m["files"]}
    present = set(set_files(setdir))
    extra, missing = sorted(present - set(listed)), sorted(set(listed) - present)
    if missing:
        raise Error(f"{setdir}: missing {', '.join(missing)}")
    if extra:
        raise Error(f"{setdir}: not in the manifest: {', '.join(extra)}")
    for rel, f in sorted(listed.items()):
        full = os.path.join(setdir, rel)
        if os.path.islink(full) or not os.path.isfile(full):
            raise Error(f"{rel}: not a regular file")
        if os.path.getsize(full) != f["size"]:
            raise Error(f"{rel}: {os.path.getsize(full)} bytes, the manifest says {f['size']}")
        if not quick and sha256_file(full) != f["sha256"]:
            raise Error(f"{rel}: SHA-256 differs from the manifest")
    for rel, f in sorted(listed.items()):
        if "hashes" in f:
            chunk, size, _ = read_hashes(os.path.join(setdir, f["hashes"]))
            if size != f["size"] or chunk != m.get("chunk", chunk):
                raise Error(f"{f['hashes']}: is for {size} bytes in {chunk}-byte chunks, not for {rel}")
            if not quick:
                check_hashes(full := os.path.join(setdir, rel), os.path.join(setdir, f["hashes"]))
    return m


def need_zstd():
    tool = shutil.which("zstd")
    if not tool:
        raise Error("zstd is missing: install zstd")
    return tool


def pack(setdir, out):
    """The archive of a verified set: manifest.json first, then the files in manifest order, nothing else."""
    m = verify_dir(setdir, quick=True)
    zstd = need_zstd()
    tmp = out + ".part"
    with open(tmp, "wb") as raw:
        z = subprocess.Popen([zstd, "-q", "-T0", "-10", "--long=27", "-c"], stdin=subprocess.PIPE, stdout=raw)
        try:
            with tarfile.open(fileobj=z.stdin, mode="w|", format=tarfile.PAX_FORMAT) as tar:
                dirs = set()
                for rel in ["manifest.json"] + [f["path"] for f in m["files"]]:
                    parent = os.path.dirname(rel)
                    if parent and parent not in dirs:
                        for i in range(1, parent.count("/") + 2):
                            d = "/".join(parent.split("/")[:i])
                            if d not in dirs:
                                info = tarfile.TarInfo(d)
                                info.type, info.mode = tarfile.DIRTYPE, 0o755
                                tar.addfile(info)
                                dirs.add(d)
                    full = os.path.join(setdir, rel)
                    info = tarfile.TarInfo(rel)
                    info.size, info.mode = os.path.getsize(full), 0o644
                    with open(full, "rb") as f:
                        tar.addfile(info, f)
        finally:
            z.stdin.close()
            if z.wait():
                os.unlink(tmp)
                raise Error("zstd failed")
    os.replace(tmp, out)
    digest = sha256_file(out)
    with open(out + ".sha256", "w") as f:
        f.write(f"{digest}  {os.path.basename(out)}\n")
    return digest


def verify_archive(path):
    """Check a .tar.zst as the OneOS fork does: its .sha256 if present, the tar rules, then every file while it streams."""
    zstd = need_zstd()
    side = path + ".sha256"
    if os.path.isfile(side):
        with open(side) as f:
            want = f.read().split()[0]
        if sha256_file(path) != want:
            raise Error(f"{path}: SHA-256 differs from {os.path.basename(side)}")
    z = subprocess.Popen([zstd, "-q", "-d", "--long=31", "-c", path], stdout=subprocess.PIPE)
    manifest, seen = None, set()
    try:
        with tarfile.open(fileobj=z.stdout, mode="r|") as tar:
            for info in tar:
                check_set_path(info.name, path)
                if info.isdir():
                    continue
                if not info.isreg():
                    raise Error(f"{path}: {info.name} is not a regular file")
                src = tar.extractfile(info)
                if manifest is None:
                    if info.name != "manifest.json":
                        raise Error(f"{path}: the first file is {info.name}, not manifest.json")
                    with tempfile.NamedTemporaryFile("wb", suffix=".json") as t:
                        t.write(src.read(16 << 20))
                        t.flush()
                        manifest = load_manifest(t.name)
                    listed = {f["path"]: f for f in manifest["files"]}
                    continue
                f = listed.get(info.name)
                if f is None or info.name in seen:
                    raise Error(f"{path}: {info.name} is not in the manifest, or is there twice")
                seen.add(info.name)
                h, n = hashlib.sha256(), 0
                while block := src.read(BLOCK):
                    h.update(block)
                    n += len(block)
                if n != f["size"] or h.hexdigest() != f["sha256"]:
                    raise Error(f"{path}: {info.name} differs from the manifest")
    except tarfile.TarError as e:
        raise Error(f"{path}: {e}") from None
    finally:
        z.stdout.close()
        z.wait()
    if z.returncode:
        raise Error(f"{path}: zstd could not read it")
    if manifest is None:
        raise Error(f"{path}: no manifest.json")
    missing = sorted(set(listed) - seen)
    if missing:
        raise Error(f"{path}: missing {', '.join(missing)}")
    return manifest


# ---------------------------------------------------------------- command line

def _opt(args, name, default=None):
    return args[args.index(name) + 1] if name in args else default


def _load_json(path):
    if not path:
        return None
    with open(path) as f:
        return json.load(f)


def main(argv):
    if len(argv) < 3:
        sys.exit(__doc__)
    cmd, args = argv[1], argv[2:]
    if cmd == "profile":
        cmd_profile(args)
    elif cmd == "check-ipsw":
        cmd_check_ipsw(args)
    elif cmd == "fetch":
        cmd_fetch(args)
    elif cmd == "record-plain":
        record_plain(args[0], args[1], args[2])
    elif cmd == "aea-info":
        cmd_aea_info(args)
    elif cmd == "im4p":
        cmd_im4p(args)
    elif cmd == "apfs-info":
        print(json.dumps(apfs_info(args[0]), indent=2))
    elif cmd == "hashes":
        size, digest = write_hashes(args[0], args[1], int(_opt(args, "--chunk", DEFAULT_CHUNK)))
        print(f"{digest}  {size}  {args[0]}")
    elif cmd == "tree-copy":
        count, skipped = tree_copy(args[0], args[1], args[2])
        print(f"copied {count} entries" + (f", skipped {len(skipped)} special files" if skipped else ""))
    elif cmd == "seed-pack":
        count, added, changed = seed_pack(args[0], args[1], args[2], int(_opt(args, "--epoch")), _opt(args, "--owners"))
        print(json.dumps({"format": "tar", "root": "/private/var", "entries": count, "added": added, "changed": changed}))
    elif cmd == "manifest":
        m = make_manifest(load_profile(args[0]), args[1], _load_json(_opt(args, "--inputs")),
                          _load_json(_opt(args, "--seed-info")), _load_json(_opt(args, "--tweaks")))
        print(f"manifest.json: {len(m['files'])} files, {sum(f['size'] for f in m['files'])} bytes")
    elif cmd == "verify":
        m = verify_dir(args[0], "--quick" in args) if os.path.isdir(args[0]) else verify_archive(args[0])
        print(f"ok: {m.get('id', m['build'])} format {m['format']}, {len(m['files'])} files, "
              f"{sum(f['size'] for f in m['files'])} bytes")
    elif cmd == "pack":
        print(f"{pack(args[0], args[1])}  {args[1]}")
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    try:
        main(sys.argv)
    except Error as e:
        sys.exit(f"imagetool: {e}")
    except (IndexError, TypeError):
        sys.exit(__doc__)
