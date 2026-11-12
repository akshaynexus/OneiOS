#!/usr/bin/env python3
"""A raw APFS image made from folders, for tests of the tools that read one (no Apple tool, no root, no mount).

    tools/oneios-tool/tests/ioslog/mkapfs.py OUT.img NAME=FOLDER [NAME=FOLDER ...] [--stale-block0]

One volume per NAME=FOLDER, in this order (an empty FOLDER part, `NAME=`, makes an empty volume). The volumes are
laid out as the phone's Data store is: unencrypted, case-sensitive with hashed names, no role, a virtual
file-system tree behind the volume's object map, regular files, folders and symbolic links, files over 8 blocks in
two extents. Checksums, object headers, the checkpoint areas and the name hashes are real, so apfs-fuse reads the
image too: tests/ioslog/compare.sh checks this writer against apfs-fuse before it trusts the image as an input.

--stale-block0: block 0 holds an older superblock (transaction 4) in which every volume is empty; the newest
checkpoint (transaction 5) holds the files. A store copied after use looks like this.

Not written: the space manager's bitmaps, the reaper, snapshots, the extent reference tree, xattrs other than a
link's target, compression, encryption. Names must be ASCII (their hash needs no Unicode tables then).
"""
import os
import stat
import struct
import sys
import uuid

BLOCK = 4096
PHYSICAL, EPHEMERAL = 0x40000000, 0x80000000
T_NXSB, T_BTREE, T_NODE, T_SPACEMAN, T_OMAP, T_CPMAP, T_FS, T_FSTREE = 1, 2, 3, 5, 0xB, 0xC, 0xD, 0xE


def fletcher64(block):
    a = b = 0
    for (word,) in struct.iter_unpack("<I", block[8:]):
        a = (a + word) % 0xFFFFFFFF
        b = (b + a) % 0xFFFFFFFF
    low = 0xFFFFFFFF - (a + b) % 0xFFFFFFFF
    high = 0xFFFFFFFF - (a + low) % 0xFFFFFFFF
    return high << 32 | low


_CRC = []
for _i in range(256):
    _c = _i
    for _ in range(8):
        _c = (_c >> 1) ^ 0x82F63B78 if _c & 1 else _c >> 1
    _CRC.append(_c)


def name_hash(name):
    """j_drec_hashed_key_t's name_len_and_hash: CRC-32C (start 0xFFFFFFFF, not inverted) of the name's UTF-32
    code points, its low 22 bits above the 10 bits of the length (with the NUL)."""
    crc = 0xFFFFFFFF
    for byte in name.encode("utf-32-le"):
        crc = _CRC[(crc ^ byte) & 0xFF] ^ (crc >> 8)
    return (crc & 0x3FFFFF) << 10 | (len(name.encode()) + 1)


class Image:
    def __init__(self):
        self.data = bytearray()
        self.objects = []          # blocks that carry an object header (checksummed at the end)

    def alloc(self, count=1):
        first = len(self.data) // BLOCK
        self.data += bytes(BLOCK * count)
        return first

    def obj(self, block, oid, xid, kind, subtype=0):
        struct.pack_into("<QQII", self.data, block * BLOCK + 8, oid, xid, kind, subtype)
        self.objects.append(block)
        return block * BLOCK

    def finish(self, path):
        for block in self.objects:
            at = block * BLOCK
            struct.pack_into("<Q", self.data, at, fletcher64(self.data[at:at + BLOCK]))
        with open(path, "wb") as f:
            f.write(self.data)


def btree(image, xid, subtype, items, fixed, new_node):
    """Write a B-tree of (key, value) items (sorted). new_node() -> (block, oid) for a node; a child is named
    by its oid. fixed: (key size, value size) of an object map, None for variable entries. Returns the root's
    (block, oid)."""
    toc_size = 4 if fixed else 8

    def pack(level_items, level, single):
        """Split one level's items into nodes; each node: (first key, [items])."""
        nodes, current, used = [], [], 0
        # every node is filled as if it were the root (40 bytes less): the root is not known yet
        room = BLOCK - 56 - 40
        for key, val in level_items:
            need = toc_size + len(key) + len(val)
            if current and used + need > room:
                nodes.append(current)
                current, used = [], 0
            current.append((key, val))
            used += need
        if current or single:
            nodes.append(current)
        return nodes

    def write(node_items, level, root, block, oid):
        at = image.obj(block, oid, xid, (T_BTREE if root else T_NODE) | (PHYSICAL if fixed else 0), subtype)
        flags = (1 if root else 0) | (2 if level == 0 else 0) | (4 if fixed else 0)
        table = toc_size * len(node_items)
        struct.pack_into("<HHIHH", image.data, at + 32, flags, level, len(node_items), 0, table)
        keys, vals = at + 56 + table, at + BLOCK - (40 if root else 0)
        k_at = v_at = 0
        for i, (key, val) in enumerate(node_items):
            v_at += len(val)
            if fixed:
                struct.pack_into("<HH", image.data, at + 56 + 4 * i, k_at, v_at)
            else:
                struct.pack_into("<HHHH", image.data, at + 56 + 8 * i, k_at, len(key), v_at, len(val))
            image.data[keys + k_at:keys + k_at + len(key)] = key
            image.data[vals - v_at:vals - v_at + len(val)] = val
            k_at += len(key)
        # free space: between the keys and the values
        struct.pack_into("<HH", image.data, at + 44, k_at, (vals - v_at) - (keys + k_at))
        struct.pack_into("<HHHH", image.data, at + 48, 0xFFFF, 0, 0xFFFF, 0)

    level, level_items, total_nodes, count = 0, items, 0, len(items)
    longest_key = max((len(k) for k, _ in items), default=0)
    longest_val = max((len(v) for _, v in items), default=0)
    while True:
        nodes = pack(level_items, level, single=True)
        total_nodes += len(nodes)
        if len(nodes) == 1:
            block, oid = new_node()
            write(nodes[0], level, True, block, oid)
            at = block * BLOCK + BLOCK - 40
            key_size, val_size = fixed if fixed else (0, 0)
            struct.pack_into("<IIIIIIQQ", image.data, at, 0, BLOCK, key_size, val_size, longest_key, longest_val, count, total_nodes)
            return block, oid
        parents = []
        for node_items in nodes:
            block, oid = new_node()
            write(node_items, level, False, block, oid)
            parents.append((node_items[0][0], struct.pack("<Q", oid)))
        level, level_items = level + 1, parents


def omap(image, xid, mappings):
    """An object map of {oid: block}; returns its block."""
    block = image.alloc()
    def new_node():
        b = image.alloc()
        return b, b
    items = [(struct.pack("<QQ", oid, xid), struct.pack("<IIQ", 0, BLOCK, paddr)) for oid, paddr in sorted(mappings.items())]
    root, _ = btree(image, xid, T_OMAP, items, (16, 16), new_node)
    at = image.obj(block, block, xid, T_OMAP | PHYSICAL)
    struct.pack_into("<IIIIQ", image.data, at + 32, 0, 0, T_BTREE | PHYSICAL, T_BTREE | PHYSICAL, root)
    return block


def inode(parent, oid, mode, name, size=None, children=0, uid=0, gid=0, mtime=0):
    """A j_inode_val_t with the name and, for a file, the data stream as extended fields."""
    fields = [(4, 2, name.encode() + b"\0")]
    if size is not None:
        fields.append((8, 0x20, struct.pack("<QQQQQ", size, (size + BLOCK - 1) // BLOCK * BLOCK, 0, size, 0)))
    table = b"".join(struct.pack("<BBH", t, f, len(d)) for t, f, d in fields)
    values = b"".join(d + bytes(-len(d) % 8) for _, _, d in fields)
    blob = struct.pack("<HH", len(fields), len(values)) + table + values
    return struct.pack("<QQQQQQQiIIIIIHHQ", parent, oid, mtime, mtime, mtime, mtime, 0x8000, children if stat.S_ISDIR(mode) else 1,
                       0, 0, 0, uid, gid, mode, 0, 0) + blob


def volume(image, xid, oid, index, name, folder, next_oid):
    """One volume holding folder's tree (None: an empty volume). Returns its superblock's block."""
    records = []   # (object, type, sort bytes, key bytes after the header, value)
    ids = [16]

    def add(obj, kind, rest, val, order=b""):
        records.append((obj, kind, order, rest, val))

    def walk(path, parent, oid_, entry_name):
        st = os.lstat(path)
        mtime = st.st_mtime_ns
        if stat.S_ISDIR(st.st_mode):
            names = sorted(os.listdir(path))
            add(oid_, 3, b"", inode(parent, oid_, 0o40755, entry_name, children=len(names), mtime=mtime))
            for child in names:
                if any(ord(c) > 126 or ord(c) < 32 for c in child):
                    sys.exit(f"mkapfs: {path}/{child}: only ASCII names")
                cid = ids[0]
                ids[0] += 1
                cst = os.lstat(os.path.join(path, child))
                kind = 4 if stat.S_ISDIR(cst.st_mode) else 10 if stat.S_ISLNK(cst.st_mode) else 8
                h = name_hash(child)
                raw = child.encode() + b"\0"
                add(oid_, 9, struct.pack("<I", h) + raw, struct.pack("<QQH", cid, mtime, kind),
                    order=struct.pack(">I", h & 0xFFFFFC00) + raw)
                walk(os.path.join(path, child), oid_, cid, child)
        elif stat.S_ISLNK(st.st_mode):
            target = os.readlink(path).encode() + b"\0"
            add(oid_, 3, b"", inode(parent, oid_, 0o120755, entry_name, mtime=mtime))
            xname = b"com.apple.fs.symlink\0"
            add(oid_, 4, struct.pack("<H", len(xname)) + xname, struct.pack("<HH", 2, len(target)) + target, order=xname)
        elif stat.S_ISREG(st.st_mode):
            with open(path, "rb") as f:
                data = f.read()
            add(oid_, 3, b"", inode(parent, oid_, 0o100644, entry_name, size=len(data), mtime=mtime))
            blocks = (len(data) + BLOCK - 1) // BLOCK
            # over 8 blocks: two extents with a block of something else between them
            parts = [(0, blocks)] if blocks <= 8 else [(0, blocks // 2), (blocks // 2, blocks - blocks // 2)]
            for n, (first, count) in enumerate(parts):
                if n:
                    image.alloc()
                at = image.alloc(count)
                chunk = data[first * BLOCK:(first + count) * BLOCK]
                image.data[at * BLOCK:at * BLOCK + len(chunk)] = chunk
                add(oid_, 8, struct.pack("<Q", first * BLOCK), struct.pack("<QQQ", count * BLOCK, at, 0),
                    order=struct.pack(">Q", first * BLOCK))

    if folder:
        walk(folder, 1, 2, "root")
    else:
        add(2, 3, b"", inode(1, 2, 0o40755, "root"))
    add(3, 3, b"", inode(1, 3, 0o40755, "private-dir"))
    # the root folder and the private folder are entries of object 1 (the root's parent)
    for top, top_id in (("root", 2), ("private-dir", 3)):
        raw = top.encode() + b"\0"
        add(1, 9, struct.pack("<I", name_hash(top)) + raw, struct.pack("<QQH", top_id, 0, 4),
            order=struct.pack(">I", name_hash(top) & 0xFFFFFC00) + raw)
    records.sort(key=lambda r: (r[0], r[1], r[2]))
    items = [(struct.pack("<Q", obj | kind << 60) + rest, val) for obj, kind, _, rest, val in records]

    nodes = {}
    def new_node():
        block = image.alloc()
        node_oid = next_oid[0]
        next_oid[0] += 1
        nodes[node_oid] = block
        return block, node_oid
    _, root_oid = btree(image, xid, T_FSTREE, items, None, new_node)
    vol_omap = omap(image, xid, nodes)

    block = image.alloc()
    at = image.obj(block, oid, xid, T_FS)
    d = image.data
    d[at + 32:at + 36] = b"APSB"
    struct.pack_into("<I", d, at + 36, index)
    struct.pack_into("<Q", d, at + 56, 0x8)                       # names hashed, normalization-insensitive
    struct.pack_into("<III", d, at + 116, T_BTREE, T_BTREE | PHYSICAL, T_BTREE | PHYSICAL)
    struct.pack_into("<QQ", d, at + 128, vol_omap, root_oid)
    struct.pack_into("<Q", d, at + 176, ids[0])
    d[at + 240:at + 256] = uuid.uuid5(uuid.NAMESPACE_DNS, f"oneios-test-{name}").bytes
    struct.pack_into("<Q", d, at + 264, 1)                        # unencrypted
    d[at + 704:at + 704 + len(name)] = name.encode()
    return block


def main(argv):
    stale = "--stale-block0" in argv
    args = [a for a in argv if a != "--stale-block0"]
    if len(args) < 2 or any("=" not in a for a in args[1:]):
        sys.exit(__doc__)
    out, volumes = args[0], [a.split("=", 1) for a in args[1:]]
    image = Image()
    image.alloc(1)                       # block 0
    desc = image.alloc(4)                # two checkpoints: (map, superblock) each
    data_area = image.alloc(4)
    spaceman = data_area
    image.obj(spaceman, 1024, 5, T_SPACEMAN | EPHEMERAL)
    next_oid = [1100]

    def checkpoint(slot, xid, with_files):
        mappings = {}
        for index, (name, folder) in enumerate(volumes):
            vol_oid = 1026 + index
            mappings[vol_oid] = volume(image, xid, vol_oid, index, name, folder if with_files else None, next_oid)
        container_omap = omap(image, xid, mappings)
        cpm = desc + 2 * slot
        at = image.obj(cpm, cpm, xid, T_CPMAP | PHYSICAL)
        struct.pack_into("<II", image.data, at + 32, 1, 1)
        struct.pack_into("<IIIIQQQ", image.data, at + 40, T_SPACEMAN | EPHEMERAL, 0, BLOCK, 0, 0, 1024, spaceman)
        sb = bytearray(BLOCK)
        sb[32:36] = b"NXSB"
        struct.pack_into("<IQ", sb, 36, BLOCK, 0)                 # the block count is set at the end
        struct.pack_into("<Q", sb, 64, 2)
        sb[72:88] = uuid.uuid5(uuid.NAMESPACE_DNS, "oneios-test-container").bytes
        struct.pack_into("<QQ", sb, 88, next_oid[0] + 1000, xid + 1)
        struct.pack_into("<IIQQIIIIII", sb, 104, 4, 4, desc, data_area, 0, 0, 2 * slot, 2, 0, 1)
        struct.pack_into("<QQQII", sb, 152, 1024, container_omap, 0, 0, 100)
        for index in range(len(volumes)):
            struct.pack_into("<Q", sb, 184 + 8 * index, 1026 + index)
        return sb

    old = checkpoint(0, 4, False) if stale else None
    new = checkpoint(1 if stale else 0, 5, True)
    count = len(image.data) // BLOCK
    for slot, sb, xid in ((0, old, 4), (1 if stale else 0, new, 5)):
        if sb is None:
            continue
        struct.pack_into("<Q", sb, 40, count)
        block = desc + 2 * slot + 1
        image.data[block * BLOCK:(block + 1) * BLOCK] = sb
        image.obj(block, 1, xid, T_NXSB | EPHEMERAL)
    first = old if stale else new
    image.data[0:BLOCK] = first
    image.obj(0, 1, 4 if stale else 5, T_NXSB | EPHEMERAL)
    image.finish(out)
    print(f"{out}: {count} blocks, {len(volumes)} volumes" + (", block 0 one transaction behind" if stale else ""))


if __name__ == "__main__":
    main(sys.argv[1:])
