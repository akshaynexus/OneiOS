//! Stand-in inputs for the tests. Everything here is generated; nothing of Apple's is used.
//! A port of scripts/image/tests/fixtures.py: `fake_apfs` writes the smallest image `apfs_info` reads (a container
//! superblock, its object map with a one-node tree, one volume superblock, laid out as Apple's "Apple File System
//! Reference" documents them), `fake_apfs_tree` one whose volume has a file-system tree for `ApfsTree`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU32, Ordering};

const BLOCK: usize = 4096;

/// A folder of its own under the system's temporary folder, removed when the test ends
/// (kept when ONEIOS_IMAGE_KEEP_TMP is set, to look at what a test made).
pub struct Tmp {
    dir: String,
}

impl Tmp {
    pub fn new(name: &str) -> Tmp {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let base = std::env::temp_dir().canonicalize().unwrap();
        let dir = format!("{}/oneios-image-test-{}-{}-{name}", base.display(), std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Tmp { dir }
    }
    pub fn path(&self, rel: &str) -> String {
        format!("{}/{rel}", self.dir)
    }
    pub fn write(&self, rel: &str, data: &[u8]) -> String {
        let path = self.path(rel);
        fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
        path
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        if std::env::var_os("ONEIOS_IMAGE_KEEP_TMP").is_none() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// `len` bytes that look random and are the same for the same seed.
pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

pub fn copy_tree(from: &str, to: &str) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (a, b) = (format!("{from}/{}", e.file_name().to_str().unwrap()), format!("{to}/{}", e.file_name().to_str().unwrap()));
        if e.file_type().unwrap().is_dir() {
            copy_tree(&a, &b);
        } else {
            fs::copy(&a, &b).unwrap();
        }
    }
}

fn put(image: &mut [u8], at: usize, data: &[u8]) {
    image[at..at + data.len()].copy_from_slice(data);
}
fn put16(image: &mut [u8], at: usize, v: u16) {
    put(image, at, &v.to_le_bytes());
}
fn put32(image: &mut [u8], at: usize, v: u32) {
    put(image, at, &v.to_le_bytes());
}
fn put64(image: &mut [u8], at: usize, v: u64) {
    put(image, at, &v.to_le_bytes());
}

fn uuid_of(name: &str) -> [u8; 16] {
    use sha2::Digest;
    sha2::Sha256::digest(name.as_bytes())[..16].try_into().unwrap()
}

fn uuid_text(b: &[u8; 16]) -> String {
    let h: Vec<String> = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{}-{}-{}-{}-{}", h[0..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..16].concat())
}

/// An object map leaf with fixed-size entries: (object id, block).
fn fixed_leaf(image: &mut [u8], block: usize, mappings: &[(u64, u64)]) {
    let node = block * BLOCK;
    put16(image, node + 32, 1 | 2 | 4);
    put16(image, node + 34, 0);
    put32(image, node + 36, mappings.len() as u32);
    put16(image, node + 40, 0);
    put16(image, node + 42, 4 * mappings.len() as u16);
    let (keys, vals) = (node + 56 + 4 * mappings.len(), node + BLOCK - 40);
    for (i, (oid, paddr)) in mappings.iter().enumerate() {
        put16(image, node + 56 + 4 * i, 16 * i as u16);
        put16(image, node + 56 + 4 * i + 2, 16 * (i as u16 + 1));
        put64(image, keys + 16 * i, *oid);
        put64(image, keys + 16 * i + 8, 1);
        put32(image, vals - 16 * (i + 1), 0);
        put32(image, vals - 16 * (i + 1) + 4, BLOCK as u32);
        put64(image, vals - 16 * (i + 1) + 8, *paddr);
    }
}

/// A raw APFS image with one volume and no files. Returns the volume's UUID as `apfs_info` prints it.
pub fn fake_apfs(path: &str, name: &str, role: u16, blocks: usize, fill: &[u8]) -> String {
    let vol_uuid = uuid_of(name);
    let mut image = vec![0u8; BLOCK * blocks];
    // block 0: container superblock
    put(&mut image, 32, b"NXSB");
    put32(&mut image, 36, BLOCK as u32);
    put64(&mut image, 40, blocks as u64);
    put(&mut image, 72, &uuid_of(&format!("container{name}")));
    put64(&mut image, 160, 1); // object map: block 1
    put32(&mut image, 180, 1); // one volume
    put64(&mut image, 184, 1026); // its object id
    // block 1: object map, tree root in block 2
    put64(&mut image, BLOCK + 48, 2);
    // block 2: a root leaf with fixed-size entries and one mapping 1026 -> block 3
    let node = 2 * BLOCK;
    put16(&mut image, node + 32, 1 | 2 | 4);
    put32(&mut image, node + 36, 1);
    put16(&mut image, node + 42, 64);
    put16(&mut image, node + 56 + 2, 16);
    put64(&mut image, node + 56 + 64, 1026);
    put64(&mut image, node + 56 + 64 + 8, 1);
    put32(&mut image, node + BLOCK - 40 - 16 + 4, BLOCK as u32);
    put64(&mut image, node + BLOCK - 40 - 16 + 8, 3);
    // block 3: volume superblock
    let vol = 3 * BLOCK;
    put(&mut image, vol + 32, b"APSB");
    put64(&mut image, vol + 56, 0x28); // sealed, normalization-insensitive, case-sensitive
    put(&mut image, vol + 240, &vol_uuid);
    put(&mut image, vol + 704, name.as_bytes());
    put16(&mut image, vol + 964, role);
    put(&mut image, 4 * BLOCK, fill);
    fs::write(path, image).unwrap();
    uuid_text(&vol_uuid)
}

/// An APFS image whose one volume has a file-system tree, for `ApfsTree`: entries are
/// (path, st_mode, uid, gid, xattr names) below the root, parents before children. The tree is laid out as a sealed
/// volume's is: a virtual root index node and, when hashed, children numbered relative to the root's object id.
pub fn fake_apfs_tree(path: &str, entries: &[(&str, u16, u32, u32, Vec<&str>)], hashed: bool) {
    let (blocks, root_oid) = (16usize, 1100u64);
    let mut image = vec![0u8; BLOCK * blocks];
    put(&mut image, 32, b"NXSB");
    put32(&mut image, 36, BLOCK as u32);
    put64(&mut image, 40, blocks as u64);
    put64(&mut image, 160, 1);
    put32(&mut image, 180, 1);
    put64(&mut image, 184, 1026);
    put64(&mut image, BLOCK + 48, 2);
    fixed_leaf(&mut image, 2, &[(1026, 3)]);
    let vol = 3 * BLOCK;
    put(&mut image, vol + 32, b"APSB");
    put64(&mut image, vol + 56, 0x28);
    put32(&mut image, vol + 0x74, 2); // root tree: virtual
    put64(&mut image, vol + 0x80, 4); // volume object map
    put64(&mut image, vol + 0x88, root_oid); // root tree oid
    put64(&mut image, vol + 0x108, 1); // unencrypted
    put(&mut image, vol + 704, b"Fake");
    put64(&mut image, 4 * BLOCK + 48, 5);
    let child = if hashed { 1 } else { root_oid + 1 };
    fixed_leaf(&mut image, 5, &[(root_oid, 6), (root_oid + 1, 7)]);

    let inode = |parent: u64, oid: u64, uid: u32, gid: u32, mode: u16| -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&parent.to_le_bytes());
        v.extend_from_slice(&oid.to_le_bytes());
        v.extend_from_slice(&[0u8; 56]);
        v.extend_from_slice(&uid.to_le_bytes());
        v.extend_from_slice(&gid.to_le_bytes());
        v.extend_from_slice(&mode.to_le_bytes());
        v.extend_from_slice(&[0u8; 10]);
        v
    };
    // (key header, the key's bytes after the header, value)
    let mut records: Vec<(u64, Vec<u8>, Vec<u8>)> = vec![(2 | 3 << 60, Vec::new(), inode(1, 2, 0, 0, 0o40755))];
    let mut ids: Vec<(String, u64)> = vec![(String::new(), 2)];
    for (n, (rel, mode, uid, gid, xattrs)) in entries.iter().enumerate() {
        let oid = 16 + n as u64;
        ids.push((rel.to_string(), oid));
        let (dir, base) = rel.rsplit_once('/').unwrap_or(("", rel));
        let parent = ids.iter().find(|(r, _)| r == dir).unwrap().1;
        let mut name = base.as_bytes().to_vec();
        name.push(0);
        let mut key = (name.len() as u32 | 0x1234 << 10).to_le_bytes().to_vec();
        key.extend_from_slice(&name);
        let mut val = oid.to_le_bytes().to_vec();
        val.extend_from_slice(&0u64.to_le_bytes());
        val.extend_from_slice(&((mode >> 12) & 0xF).to_le_bytes());
        records.push((parent | 9 << 60, key, val));
        records.push((oid | 3 << 60, Vec::new(), inode(parent, oid, *uid, *gid, *mode)));
        for x in xattrs {
            let mut xn = x.as_bytes().to_vec();
            xn.push(0);
            let mut key = (xn.len() as u16).to_le_bytes().to_vec();
            key.extend_from_slice(&xn);
            records.push((oid | 4 << 60, key, vec![2, 0, 1, 0, b'v']));
        }
    }
    const MASK: u64 = (1 << 60) - 1;
    records.sort_by(|a, b| (a.0 & MASK, a.0 >> 60, &a.1).cmp(&(b.0 & MASK, b.0 >> 60, &b.1)));

    // a node with variable-size entries: (key, value)
    let mut var_node = |block: usize, level: u16, items: &[(Vec<u8>, Vec<u8>)], flags: u16, info: Option<u32>| {
        let node = block * BLOCK;
        put16(&mut image, node + 32, flags);
        put16(&mut image, node + 34, level);
        put32(&mut image, node + 36, items.len() as u32);
        put16(&mut image, node + 40, 0);
        put16(&mut image, node + 42, 8 * items.len() as u16);
        let keys = node + 56 + 8 * items.len();
        let vals = node + BLOCK - if flags & 1 != 0 { 40 } else { 0 };
        let (mut k_at, mut v_at) = (0usize, 0usize);
        for (i, (key, val)) in items.iter().enumerate() {
            v_at += val.len();
            assert!(keys + k_at + key.len() <= vals - v_at, "the fixture's records do not fit one block");
            put16(&mut image, node + 56 + 8 * i, k_at as u16);
            put16(&mut image, node + 56 + 8 * i + 2, key.len() as u16);
            put16(&mut image, node + 56 + 8 * i + 4, v_at as u16);
            put16(&mut image, node + 56 + 8 * i + 6, val.len() as u16);
            put(&mut image, keys + k_at, key);
            put(&mut image, vals - v_at, val);
            k_at += key.len();
        }
        if let Some(info) = info {
            put32(&mut image, node + BLOCK - 40, info);
            put32(&mut image, node + BLOCK - 36, BLOCK as u32);
        }
    };
    let full_key = |r: &(u64, Vec<u8>, Vec<u8>)| [r.0.to_le_bytes().to_vec(), r.1.clone()].concat();
    let leaf: Vec<(Vec<u8>, Vec<u8>)> = records.iter().map(|r| (full_key(r), r.2.clone())).collect();
    var_node(7, 0, &leaf, 2 | if hashed { 8 } else { 0 }, None);
    let mut value = child.to_le_bytes().to_vec();
    value.extend_from_slice(&vec![0u8; if hashed { 32 } else { 0 }]);
    var_node(6, 1, &[(full_key(&records[0]), value)], 1 | if hashed { 8 } else { 0 }, Some(if hashed { 0x80 } else { 0 }));
    fs::write(path, image).unwrap();
}

/// role, member path, AEA, size, CRC-32 (the last two are only pins: nothing here checks them against a file)
const MEMBERS: [(&str, &str, bool, u64, &str); 13] = [
    ("os", "001-00001-001.dmg.aea", true, 300044, "00000001"),
    ("systemos", "001-00002-001.dmg.aea", true, 300044, "00000002"),
    ("app", "001-00003-001.dmg", false, 32768, "00000003"),
    ("trust-static", "Firmware/001-00001-001.dmg.aea.trustcache", false, 100, "00000004"),
    ("trust-system", "Firmware/001-00002-001.dmg.aea.trustcache", false, 100, "00000005"),
    ("trust-app", "Firmware/001-00003-001.dmg.trustcache", false, 100, "00000006"),
    ("roothash-os", "Firmware/001-00001-001.dmg.aea.root_hash", false, 100, "00000007"),
    ("roothash-system", "Firmware/001-00002-001.dmg.aea.root_hash", false, 100, "00000008"),
    ("roothash-app", "Firmware/001-00003-001.dmg.root_hash", false, 100, "00000009"),
    ("kernelcache", "kernelcache.release.t1", false, 100, "0000000a"),
    ("devicetree", "Firmware/all_flash/DeviceTree.t1ap.im4p", false, 100, "0000000c"),
    ("buildmanifest", "BuildManifest.plist", false, 100, "0000000b"),
    ("restore", "Restore.plist", false, 100, "0000000d"),
];

/// The profile of the stand-in build (fixtures.py PROFILE).
pub fn profile_text() -> String {
    let members: Vec<String> = MEMBERS
        .iter()
        .map(|(role, path, aea, size, crc)| {
            format!("[[member]]\nrole = \"{role}\"\npath = \"{path}\"\nsize = {size}\ncrc32 = \"{crc}\"{}", if *aea { "\naea = true" } else { "" })
        })
        .collect();
    format!(
        "schema = 1
[os]
name = \"ios-0.0.1-0T1-iTest1,1\"
version = \"0.0.1\"
build = \"0T1\"
device = \"iTest1,1\"
board = \"t1ap\"
chip_id = 0x1234
board_id = 0x02
arch = \"arm64e\"
[ipsw]
url = \"https://example.invalid/iTest.ipsw\"
size = 12345
sha256 = \"{}\"
need_gib = 0
{}
[image]
format = 1
id = \"ios-0.0.1-0T1-iTest1-1\"
stage = \"/data/local/tmp/oneios/image\"
archive = \"OneiOS_test.tar.zst\"
chunk = 4096
seed = \"data.seed\"
kernel = \"kernel.kc\"
[image.files]
os = \"os.img\"
systemos = \"systemos.img\"
app = \"app.img\"
trust-static = \"trust/static.trustcache\"
trust-system = \"trust/system.trustcache\"
trust-app = \"trust/app.trustcache\"
roothash-os = \"trust/os.root_hash\"
roothash-system = \"trust/system.root_hash\"
roothash-app = \"trust/app.root_hash\"
[data]
volume_uuid = \"11111111-2222-3333-4444-555555555555\"
epoch = 1700000000
",
        "00".repeat(32),
        members.join("\n")
    )
}

/// A set folder of stand-in files (test_image.py SetTest.make_set), and its profile as `test.toml` beside it.
/// Returns the set folder.
pub fn fake_set(tmp: &Tmp, kernel: bool, overlay: bool) -> String {
    tmp.write("test.toml", profile_text().as_bytes());
    let set = tmp.path("set");
    let _ = fs::remove_dir_all(&set);
    fs::create_dir_all(format!("{set}/trust")).unwrap();
    fake_apfs(&format!("{set}/os.img"), "FakeOS", 1, 40, &b"os".repeat(3000));
    fake_apfs(&format!("{set}/systemos.img"), "FakeSystemOS", 0, 24, &b"sys".repeat(2000));
    fake_apfs(&format!("{set}/app.img"), "AppCryptex", 0, 8, b"");
    for name in ["static", "system", "app"] {
        fs::write(format!("{set}/trust/{name}.trustcache"), format!("trust cache of {name}").repeat(20)).unwrap();
    }
    for name in ["os", "system", "app"] {
        fs::write(format!("{set}/trust/{name}.root_hash"), format!("root hash of {name}")).unwrap();
    }
    fs::write(format!("{set}/data.seed"), vec![0u8; 10240]).unwrap();
    if kernel {
        fs::write(format!("{set}/kernel.kc"), [b"\xcf\xfa\xed\xfe".to_vec(), b"stand-in kernel".repeat(1000)].concat()).unwrap();
    }
    if overlay {
        fs::create_dir_all(format!("{set}/overlay/trust")).unwrap();
        fs::write(format!("{set}/overlay/trust/owner.bin"), b"owner bundle").unwrap();
    }
    for rel in ["", "trust"] {
        fs::set_permissions(format!("{set}/{rel}"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    set
}

/// SHA-256 of the seed `seed::tests::build` packs with the rule `oneios<tab>501<tab>501<tab>-<tab>R`, as
/// scripts/image/imagetool.py seed-pack (Python 3.15.2's tarfile) wrote it from the same tree on 2026-11-12.
/// To make it again: run the test `pack_same_bytes_every_time_and_as_python` with ONEIOS_IMAGE_KEEP_TMP=1, then
/// `python3 -I scripts/image/imagetool.py seed-pack TMP/data TMP/meta.json OUT --epoch 1700000000 --owners TMP/owners.tsv`
/// and `shasum -a 256 OUT`.
pub fn python_seed_sha256() -> &'static str {
    "2c67844caea44de295b23058889e95c116f030ec20d5e5bd39952d1a02c3ae3c"
}
