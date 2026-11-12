"""Stand-in inputs for the image tests. Everything here is generated; nothing of Apple's is used.

fake_apfs() writes the smallest image imagetool.apfs_info reads: a container superblock, its object map
with a one-node tree, and one volume superblock, laid out as Apple's "Apple File System Reference"
documents them. It holds no files and no file system could mount it.
"""
import hashlib
import os
import plistlib
import struct
import uuid
import zipfile
import zlib

BLOCK = 4096


def fake_apfs(path, name="FakeOS", vol_uuid=None, role=1, blocks=64, fill=b""):
    vol_uuid = vol_uuid or uuid.UUID(bytes=hashlib.md5(name.encode()).digest())
    image = bytearray(BLOCK * blocks)
    # block 0: container superblock
    image[32:36] = b"NXSB"
    struct.pack_into("<IQ", image, 36, BLOCK, blocks)
    image[72:88] = uuid.UUID(bytes=hashlib.md5(b"container" + name.encode()).digest()).bytes
    struct.pack_into("<Q", image, 160, 1)            # object map: block 1
    struct.pack_into("<I", image, 180, 1)            # one volume
    struct.pack_into("<Q", image, 184, 1026)         # its object id
    # block 1: object map, tree root in block 2
    struct.pack_into("<Q", image, BLOCK + 48, 2)
    # block 2: a root leaf with fixed-size entries and one mapping 1026 -> block 3
    node = 2 * BLOCK
    struct.pack_into("<HHI", image, node + 32, 1 | 2 | 4, 0, 1)
    struct.pack_into("<HH", image, node + 40, 0, 64)
    struct.pack_into("<HH", image, node + 56, 0, 16)
    struct.pack_into("<QQ", image, node + 56 + 64, 1026, 1)
    struct.pack_into("<IIQ", image, node + BLOCK - 40 - 16, 0, BLOCK, 3)
    # block 3: volume superblock
    vol = 3 * BLOCK
    image[vol + 32:vol + 36] = b"APSB"
    struct.pack_into("<Q", image, vol + 56, 0x28)    # sealed, normalization-insensitive, case-sensitive
    image[vol + 240:vol + 256] = vol_uuid.bytes
    image[vol + 704:vol + 704 + len(name)] = name.encode()
    struct.pack_into("<H", image, vol + 964, role)
    if fill:
        image[4 * BLOCK:4 * BLOCK + len(fill)] = fill
    with open(path, "wb") as f:
        f.write(image)
    return str(vol_uuid).upper()


def fake_apfs_tree(path, entries, hashed=True):
    """An APFS image whose one volume has a file-system tree, for imagetool.ApfsTree: entries are
    (path, st_mode, uid, gid, [xattr names]) below the root, parents before children. The tree is laid out as a sealed
    volume's is: a virtual root index node and, when hashed, children numbered relative to the root's object id."""
    blocks, root_oid = 16, 1100
    image = bytearray(BLOCK * blocks)
    image[32:36] = b"NXSB"
    struct.pack_into("<IQ", image, 36, BLOCK, blocks)
    struct.pack_into("<QIQ", image, 160, 1, 0, 0)
    struct.pack_into("<I", image, 180, 1)
    struct.pack_into("<Q", image, 184, 1026)

    def fixed_leaf(block, mappings):          # an object map: (oid, paddr), fixed-size entries
        node = block * BLOCK
        struct.pack_into("<HHI", image, node + 32, 1 | 2 | 4, 0, len(mappings))
        struct.pack_into("<HH", image, node + 40, 0, 4 * len(mappings))
        keys, vals = node + 56 + 4 * len(mappings), node + BLOCK - 40
        for i, (oid, paddr) in enumerate(mappings):
            struct.pack_into("<HH", image, node + 56 + 4 * i, 16 * i, 16 * (i + 1))
            struct.pack_into("<QQ", image, keys + 16 * i, oid, 1)
            struct.pack_into("<IIQ", image, vals - 16 * (i + 1), 0, BLOCK, paddr)

    struct.pack_into("<Q", image, 1 * BLOCK + 48, 2)
    fixed_leaf(2, [(1026, 3)])
    vol = 3 * BLOCK
    image[vol + 32:vol + 36] = b"APSB"
    struct.pack_into("<Q", image, vol + 56, 0x28)
    struct.pack_into("<I", image, vol + 0x74, 2)               # root tree: virtual
    struct.pack_into("<QQ", image, vol + 0x80, 4, root_oid)    # volume object map, root tree oid
    struct.pack_into("<Q", image, vol + 0x108, 1)              # unencrypted
    image[vol + 704:vol + 708] = b"Fake"
    struct.pack_into("<Q", image, 4 * BLOCK + 48, 5)
    child = 1 if hashed else root_oid + 1
    fixed_leaf(5, [(root_oid, 6), (root_oid + 1, 7)])

    ids = {"": 2}
    records = [((2 << 0) | (3 << 60), struct.pack("<QQ", 1, 2) + bytes(56) + struct.pack("<IIH", 0, 0, 0o40755) + bytes(10))]
    for n, (rel, mode, uid, gid, xattrs) in enumerate(entries):
        oid = 16 + n
        ids[rel] = oid
        parent = ids[os.path.dirname(rel)]
        name = os.path.basename(rel).encode() + b"\0"
        records.append((parent | (9 << 60), struct.pack("<I", len(name) | 0x1234 << 10) + name,
                        struct.pack("<QQH", oid, 0, (mode >> 12) & 0xF)))
        records.append((oid | (3 << 60), b"", struct.pack("<QQ", parent, oid) + bytes(56) + struct.pack("<IIH", uid, gid, mode) + bytes(10)))
        for x in xattrs:
            xn = x.encode() + b"\0"
            records.append((oid | (4 << 60), struct.pack("<H", len(xn)) + xn, struct.pack("<HH", 2, 1) + b"v"))
    records[0] = (records[0][0], b"", records[0][1])
    records.sort(key=lambda r: (r[0] & ((1 << 60) - 1), r[0] >> 60, r[1]))

    def var_node(block, level, items, flags, info=None):       # (key bytes, value bytes)
        node = block * BLOCK
        struct.pack_into("<HHI", image, node + 32, flags, level, len(items))
        struct.pack_into("<HH", image, node + 40, 0, 8 * len(items))
        keys, vals = node + 56 + 8 * len(items), node + BLOCK - (40 if flags & 1 else 0)
        k_at = v_at = 0
        for i, (key, val) in enumerate(items):
            v_at += len(val)
            struct.pack_into("<HHHH", image, node + 56 + 8 * i, k_at, len(key), v_at, len(val))
            image[keys + k_at:keys + k_at + len(key)] = key
            image[vals - v_at:vals - v_at + len(val)] = val
            k_at += len(key)
        if info is not None:
            struct.pack_into("<II", image, node + BLOCK - 40, info, BLOCK)

    var_node(7, 0, [(struct.pack("<Q", h) + rest, val) for h, rest, val in records], 2 | (8 if hashed else 0))
    first = struct.pack("<Q", records[0][0]) + records[0][1]
    var_node(6, 1, [(first, struct.pack("<Q", child) + bytes(32 if hashed else 0))], 1 | (8 if hashed else 0),
             0x80 if hashed else 0)
    with open(path, "wb") as f:
        f.write(image)


def fake_aea(payload=b"encrypted bytes"):
    fields = [(b"com.apple.wkms.url", b"https://keys.example.invalid"),
              (b"com.apple.wkms.fcs-key-url", b"https://keys.example.invalid/fcs-keys/abc=")]
    auth = b"".join(struct.pack("<I", 4 + len(k) + 1 + len(v)) + k + b"\0" + v for k, v in fields)
    return b"AEA1" + (1).to_bytes(3, "little") + b"\0" + struct.pack("<I", len(auth)) + auth + payload


def _der(tag, body):
    n = len(body)
    head = bytes([n]) if n < 128 else bytes([0x80 | len(n.to_bytes((n.bit_length() + 7) // 8, "big"))]) + \
        n.to_bytes((n.bit_length() + 7) // 8, "big")
    return bytes([tag]) + head + body


def fake_im4p(kind, payload):
    return _der(0x30, _der(0x16, b"IM4P") + _der(0x16, kind) + _der(0x16, b"fixture") + _der(0x04, payload))


PROFILE = """schema = 1
[os]
name = "ios-0.0.1-0T1-iTest1,1"
version = "0.0.1"
build = "0T1"
device = "iTest1,1"
board = "t1ap"
chip_id = 0x1234
board_id = 0x02
arch = "arm64e"
[ipsw]
url = "{url}"
size = {size}
sha256 = "{sha256}"
need_gib = 0
{members}
[image]
format = 1
id = "ios-0.0.1-0T1-iTest1-1"
stage = "/data/local/tmp/oneios/image"
archive = "OneiOS_test.tar.zst"
chunk = 4096
seed = "data.seed"
kernel = "kernel.kc"
[image.files]
os = "os.img"
systemos = "systemos.img"
app = "app.img"
trust-static = "trust/static.trustcache"
trust-system = "trust/system.trustcache"
trust-app = "trust/app.trustcache"
roothash-os = "trust/os.root_hash"
roothash-system = "trust/system.root_hash"
roothash-app = "trust/app.root_hash"
[data]
volume_uuid = "11111111-2222-3333-4444-555555555555"
epoch = 1700000000
"""

# role -> (component, member path, AEA)
MEMBERS = [
    ("os", "OS", "001-00001-001.dmg.aea", True),
    ("systemos", "Cryptex1,SystemOS", "001-00002-001.dmg.aea", True),
    ("app", "Cryptex1,AppOS", "001-00003-001.dmg", False),
    ("trust-static", "StaticTrustCache", "Firmware/001-00001-001.dmg.aea.trustcache", False),
    ("trust-system", "Cryptex1,SystemTrustCache", "Firmware/001-00002-001.dmg.aea.trustcache", False),
    ("trust-app", "Cryptex1,AppTrustCache", "Firmware/001-00003-001.dmg.trustcache", False),
    ("roothash-os", "SystemVolume", "Firmware/001-00001-001.dmg.aea.root_hash", False),
    ("roothash-system", "Cryptex1,SystemVolume", "Firmware/001-00002-001.dmg.aea.root_hash", False),
    ("roothash-app", "Cryptex1,AppVolume", "Firmware/001-00003-001.dmg.root_hash", False),
    ("kernelcache", "KernelCache", "kernelcache.release.t1", False),
    ("devicetree", "DeviceTree", "Firmware/all_flash/DeviceTree.t1ap.im4p", False),
]


def fake_ipsw(folder, pin_sha384=True, wrong_pin=None):
    """A stand-in restore file (a ZIP with the member layout of an IPSW) and its profile. Returns (ipsw, profile)."""
    os.makedirs(folder, exist_ok=True)
    tmp = os.path.join(folder, "app.tmp")
    fake_apfs(tmp, "AppCryptex", role=0, blocks=8)
    with open(tmp, "rb") as f:
        app = f.read()
    os.unlink(tmp)
    content = {}
    for role, component, path, aea in MEMBERS:
        if role == "app":
            content[path] = app
        elif aea:
            content[path] = fake_aea(os.urandom(300000))
        elif role.startswith("trust-"):
            content[path] = fake_im4p(b"trst", b"trust cache of " + role.encode() * 50)
        elif role == "kernelcache":
            content[path] = fake_im4p(b"krnl", b"\xcf\xfa\xed\xfe" + b"kernel" * 4000)
        elif role == "devicetree":
            content[path] = fake_im4p(b"dtre", b"device tree" * 100)
        else:
            content[path] = b"root hash of " + role.encode()
    identity = {"Info": {"DeviceClass": "t1ap", "RestoreBehavior": "Erase", "Variant": "Customer Erase Install (IPSW)"},
                "ApChipID": "0x1234", "ApBoardID": "0x02", "Manifest": {}}
    for role, component, path, aea in MEMBERS:
        identity["Manifest"][component] = {"Info": {"Path": path}, "Digest": hashlib.sha384(content[path]).digest()}
    content["BuildManifest.plist"] = plistlib.dumps({
        "ProductVersion": "0.0.1", "ProductBuildVersion": "0T1", "SupportedProductTypes": ["iTest1,1"],
        "BuildIdentities": [identity]})
    content["Restore.plist"] = plistlib.dumps({"ProductBuildVersion": "0T1"})
    content["Firmware/unrelated.bin"] = os.urandom(5000)
    ipsw = os.path.join(folder, "iTest1,1_0.0.1_0T1_Restore.ipsw")
    with zipfile.ZipFile(ipsw, "w") as z:
        for path, data in content.items():
            # like a real IPSW: the big images stored, the rest deflated
            z.writestr(zipfile.ZipInfo(path), data,
                       zipfile.ZIP_STORED if path.endswith((".aea", ".dmg")) else zipfile.ZIP_DEFLATED)
    blocks = []
    for role, component, path, aea in MEMBERS + [("buildmanifest", None, "BuildManifest.plist", False),
                                                  ("restore", None, "Restore.plist", False)]:
        data = content[path]
        lines = ["[[member]]", f'role = "{role}"', f'path = "{path}"', f"size = {len(data)}",
                 f'crc32 = "{zlib.crc32(data):08x}"']
        if aea:
            lines.append("aea = true")
        elif component and pin_sha384:
            digest = hashlib.sha384(data).hexdigest()
            if wrong_pin == role:
                digest = "0" * 96
            lines.append(f'sha384 = "{digest}"')
        elif not component:
            lines.append(f'sha256 = "{hashlib.sha256(data).hexdigest()}"')
        blocks.append("\n".join(lines))
    with open(ipsw, "rb") as f:
        whole = hashlib.sha256(f.read()).hexdigest()
    profile = os.path.join(folder, "test.toml")
    with open(profile, "w") as f:
        f.write(PROFILE.format(url=ipsw, size=os.path.getsize(ipsw), sha256=whole, members="\n".join(blocks)))
    return ipsw, profile, content


def fake_inputs(folder):
    """inputs/<build>/ as the fetch and the decryption would leave it: plain images, trust files."""
    os.makedirs(folder, exist_ok=True)
    fake_apfs(os.path.join(folder, "001-00001-001.dmg"), "FakeOS", role=1, blocks=40, fill=b"os" * 3000)
    fake_apfs(os.path.join(folder, "001-00002-001.dmg"), "FakeSystemOS", role=0, blocks=24, fill=b"sys" * 2000)
    fake_apfs(os.path.join(folder, "001-00003-001.dmg"), "AppCryptex", role=0, blocks=8)
    for role, component, path, aea in MEMBERS:
        name = os.path.basename(path)
        if role.startswith("trust-"):
            with open(os.path.join(folder, name), "wb") as f:
                f.write(fake_im4p(b"trst", b"trust cache of " + role.encode()))
        elif role.startswith("roothash-"):
            with open(os.path.join(folder, name), "wb") as f:
                f.write(b"root hash of " + role.encode())


def fake_os_root(folder):
    """What an OS volume's root looks like to the build: /private/var with a few owners and kinds of entries."""
    var = os.path.join(folder, "private", "var")
    os.makedirs(os.path.join(var, "mobile", "Library", "Preferences"))
    os.makedirs(os.path.join(var, "root"))
    os.makedirs(os.path.join(var, "empty"))
    with open(os.path.join(var, "mobile", "Library", "Preferences", "com.example.kept.plist"), "wb") as f:
        f.write(plistlib.dumps({"Kept": True}))
    with open(os.path.join(var, "mobile", "Library", "Preferences", "com.example.setup.plist"), "wb") as f:
        f.write(plistlib.dumps({"SetupDone": False}))
    with open(os.path.join(var, "root", ".profile"), "w") as f:
        f.write("# fixture\n")
    os.symlink("mobile/Library", os.path.join(var, "link"))
    for base, dirs, files in os.walk(var):
        for name in files:
            os.utime(os.path.join(base, name), (1600000000, 1600000000))
    return folder
