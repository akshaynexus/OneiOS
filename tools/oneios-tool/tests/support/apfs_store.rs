//! Test stores with two checkpoints, and the damage a killed kernel leaves in the newest one. For `cargo test`
//! (included with `#[path]` by crates/oneios-image/tests/apfs_checkpoints.rs and
//! crates/oneios-tool/tests/ioslog_checkpoints.rs); no Apple file, no owner data.
//!
//! `mkapfs.py` (tests/ioslog/mkapfs.py, checked against apfs-fuse by tests/ioslog/compare.sh) makes a store with
//! one checkpoint from folders. `graft` puts two such stores into one container: the older one stays where it is
//! (transaction 5, block 0 is its superblock: the state block 0 has is old, as on the phone), the newer one's
//! blocks follow it, every address in them moved, and its superblock becomes the checkpoint of transaction 6 in
//! the second place of the checkpoint descriptor area. `Layout` says where the objects of a checkpoint are, read
//! by a walk written here (not the reader's), so a test can damage one by name.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub const BLOCK: usize = 4096;
/// mkapfs.py's layout: block 0, four blocks of checkpoint descriptors (map, superblock; map, superblock), four
/// of checkpoint data.
pub const DESC: u64 = 1;
/// The address pattern the phone's copy failed with: block 0xffff0000ffff, the two empty free lists of a tree
/// node (`ff ff 00 00 ff ff 00 00`) read as an object map's tree address.
pub const PATTERN_BLOCK: u64 = 0x0000_FFFF_0000_FFFF;

pub fn fletcher64(block: &[u8]) -> u64 {
    let (mut a, mut b) = (0u64, 0u64);
    for word in block[8..].as_chunks::<4>().0 {
        a = (a + u32::from_le_bytes(*word) as u64) % 0xFFFF_FFFF;
        b = (b + a) % 0xFFFF_FFFF;
    }
    let low = 0xFFFF_FFFF - (a + b) % 0xFFFF_FFFF;
    let high = 0xFFFF_FFFF - (a + low) % 0xFFFF_FFFF;
    high << 32 | low
}

/// A raw image in memory.
#[derive(Clone)]
pub struct Store {
    pub data: Vec<u8>,
}

impl Store {
    pub fn read(path: &Path) -> Store {
        Store { data: std::fs::read(path).unwrap() }
    }
    pub fn write(&self, path: &Path) -> String {
        std::fs::write(path, &self.data).unwrap();
        path.to_str().unwrap().to_string()
    }
    pub fn blocks(&self) -> u64 {
        (self.data.len() / BLOCK) as u64
    }
    pub fn u16(&self, block: u64, at: usize) -> u16 {
        u16::from_le_bytes(self.data[block as usize * BLOCK + at..][..2].try_into().unwrap())
    }
    pub fn u32(&self, block: u64, at: usize) -> u32 {
        u32::from_le_bytes(self.data[block as usize * BLOCK + at..][..4].try_into().unwrap())
    }
    pub fn u64(&self, block: u64, at: usize) -> u64 {
        u64::from_le_bytes(self.data[block as usize * BLOCK + at..][..8].try_into().unwrap())
    }
    pub fn put32(&mut self, block: u64, at: usize, value: u32) {
        self.data[block as usize * BLOCK + at..][..4].copy_from_slice(&value.to_le_bytes());
    }
    pub fn put64(&mut self, block: u64, at: usize, value: u64) {
        self.data[block as usize * BLOCK + at..][..8].copy_from_slice(&value.to_le_bytes());
    }
    pub fn block(&self, block: u64) -> &[u8] {
        &self.data[block as usize * BLOCK..][..BLOCK]
    }
    /// Write the block's checksum again, after a change that a writer would have made on purpose.
    pub fn seal(&mut self, block: u64) {
        let sum = fletcher64(self.block(block));
        self.put64(block, 0, sum);
    }

    // ---- the damage of a write that was cut off

    /// One bit of the block's body changed, the stored checksum left: the checksum is wrong.
    pub fn spoil_checksum(&mut self, block: u64) {
        self.data[block as usize * BLOCK + 100] ^= 0x40;
    }
    /// The block from `offset` on is zeros: the first part was written, the rest was not.
    pub fn half_zeros(&mut self, block: u64, offset: usize) {
        self.data[block as usize * BLOCK + offset..(block as usize + 1) * BLOCK].fill(0);
    }
    /// The block from `offset` on is the bytes `ff ff 00 00` over and over.
    pub fn half_pattern(&mut self, block: u64, offset: usize) {
        for (i, byte) in self.data[block as usize * BLOCK + offset..(block as usize + 1) * BLOCK].iter_mut().enumerate() {
            *byte = if i % 4 < 2 { 0xFF } else { 0 };
        }
    }
}

/// The (key offset, value offset from the end of the value area) of every entry of the tree node in `block`, with
/// its key area's start and its value area's end.
fn entries(s: &Store, block: u64) -> (Vec<(usize, usize)>, usize, usize) {
    let flags = s.u16(block, 32);
    let count = s.u32(block, 36) as usize;
    let toc = 56 + s.u16(block, 40) as usize;
    let keys = toc + s.u16(block, 42) as usize;
    let vals = BLOCK - if flags & 1 != 0 { 40 } else { 0 };
    let list = (0..count).map(|i| if flags & 4 != 0 { (s.u16(block, toc + 4 * i) as usize, s.u16(block, toc + 4 * i + 2) as usize) } else { (s.u16(block, toc + 8 * i) as usize, s.u16(block, toc + 8 * i + 4) as usize) }).collect();
    (list, keys, vals)
}

/// An object map: its own block, its tree's nodes (the root first), its mappings (oid, xid, block).
#[derive(Clone, Debug, Default)]
pub struct Omap {
    pub block: u64,
    pub nodes: Vec<u64>,
    pub mappings: Vec<(u64, u64, u64)>,
}

fn omap(s: &Store, block: u64) -> Omap {
    let mut out = Omap { block, nodes: Vec::new(), mappings: Vec::new() };
    let mut todo = vec![s.u64(block, 48)];
    while let Some(node) = todo.pop() {
        out.nodes.push(node);
        let (list, keys, vals) = entries(s, node);
        for (k, v) in list {
            if s.u16(node, 34) == 0 {
                out.mappings.push((s.u64(node, keys + k), s.u64(node, keys + k + 8), s.u64(node, vals - v + 8)));
            } else {
                todo.push(s.u64(node, vals - v));
            }
        }
    }
    out.mappings.sort();
    out
}

/// One volume of a checkpoint.
#[derive(Clone, Debug)]
pub struct Volume {
    pub oid: u64,
    pub superblock: u64,
    pub omap: Omap,
    /// The file-system tree's nodes: (block, level), the root first.
    pub nodes: Vec<(u64, u16)>,
}

impl Volume {
    /// A leaf of the file-system tree (the last one: it holds the records of the highest object ids).
    pub fn leaf(&self) -> u64 {
        self.nodes.iter().rev().find(|(_, level)| *level == 0).unwrap().0
    }
    pub fn root(&self) -> u64 {
        self.nodes[0].0
    }
}

/// Where the objects of one checkpoint are.
#[derive(Clone, Debug)]
pub struct Layout {
    pub xid: u64,
    pub superblock: u64,
    pub map: u64,
    pub omap: Omap,
    pub volumes: Vec<Volume>,
}

/// The checkpoint whose superblock is in `superblock` (a block of the descriptor area; its map is the block before).
pub fn layout(s: &Store, superblock: u64) -> Layout {
    assert_eq!(&s.block(superblock)[32..36], b"NXSB");
    let container = omap(s, s.u64(superblock, 160));
    let mut volumes = Vec::new();
    for i in 0..100 {
        let oid = s.u64(superblock, 184 + 8 * i);
        if oid == 0 {
            continue;
        }
        let sb = container.mappings.iter().rev().find(|m| m.0 == oid).unwrap().2;
        assert_eq!(&s.block(sb)[32..36], b"APSB");
        let vol_omap = omap(s, s.u64(sb, 0x80));
        let at = |oid: u64| vol_omap.mappings.iter().rev().find(|m| m.0 == oid).unwrap().2;
        let mut nodes = Vec::new();
        let mut todo = vec![at(s.u64(sb, 0x88))];
        while let Some(node) = todo.pop() {
            let level = s.u16(node, 34);
            nodes.push((node, level));
            if level != 0 {
                let (list, _, vals) = entries(s, node);
                // (pushed in reverse so that the nodes come out in key order)
                for (_, v) in list.into_iter().rev() {
                    todo.push(at(s.u64(node, vals - v)));
                }
            }
        }
        volumes.push(Volume { oid, superblock: sb, omap: vol_omap, nodes });
    }
    Layout { xid: s.u64(superblock, 16), superblock, map: superblock - 1, omap: container, volumes }
}

/// Two stores of mkapfs.py (one checkpoint each, the same volumes) as one container with two checkpoints:
/// `older` as transaction 5 (and block 0), `newer` as transaction 6. Returns the store and the two layouts.
pub fn graft(older: &Store, newer: &Store) -> (Store, Layout, Layout) {
    let shift = older.blocks();
    let total = shift + newer.blocks();
    let from = layout(newer, DESC + 1);
    let mut out = Store { data: [older.data.clone(), newer.data.clone()].concat() };
    // the newer store's own block 0 and descriptor area are not part of the container
    out.data[shift as usize * BLOCK..(shift + DESC + 4) as usize * BLOCK].fill(0);
    let move_omap = |out: &mut Store, map: &Omap| {
        let at = map.block + shift;
        out.put64(at, 8, at);
        out.put64(at, 48, newer.u64(map.block, 48) + shift);
        out.seal(at);
        for node in &map.nodes {
            let at = node + shift;
            out.put64(at, 8, at);
            let (list, _, vals) = entries(newer, *node);
            for (_, v) in list {
                // a leaf's value: flags, size, block; an index entry's: the child's block
                let field = vals - v + if newer.u16(*node, 34) == 0 { 8 } else { 0 };
                out.put64(at, field, newer.u64(*node, field) + shift);
            }
            out.seal(at);
        }
    };
    move_omap(&mut out, &from.omap);
    for volume in &from.volumes {
        let sb = volume.superblock + shift;
        out.put64(sb, 0x80, newer.u64(volume.superblock, 0x80) + shift);
        out.seal(sb);
        move_omap(&mut out, &volume.omap);
        for (node, level) in &volume.nodes {
            if *level != 0 {
                continue; // an index entry names its child by object id
            }
            let (list, keys, vals) = entries(newer, *node);
            for (k, v) in list {
                // a file extent's record (type 8): length, block, crypto id
                if newer.u64(*node, keys + k) >> 60 == 8 {
                    let block = newer.u64(*node, vals - v + 8);
                    if block != 0 {
                        out.put64(node + shift, vals - v + 8, block + shift);
                    }
                }
            }
            out.seal(node + shift);
        }
    }
    // the newer checkpoint: its map and its superblock in the second place of the older store's descriptor area
    let (map, sb) = (DESC + 2, DESC + 3);
    let old_map = older.block(DESC).to_vec();
    out.data[map as usize * BLOCK..][..BLOCK].copy_from_slice(&old_map);
    out.put64(map, 8, map);
    out.put64(map, 16, 6);
    out.seal(map);
    let new_sb = newer.block(DESC + 1).to_vec();
    out.data[sb as usize * BLOCK..][..BLOCK].copy_from_slice(&new_sb);
    out.put64(sb, 16, 6);
    out.put64(sb, 96, 7); // the next transaction
    out.put64(sb, 112, older.u64(0, 112)); // the areas are the older store's
    out.put64(sb, 120, older.u64(0, 120));
    out.put32(sb, 136, 2); // this checkpoint: blocks 2 and 3 of the descriptor area
    out.put32(sb, 140, 2);
    out.put64(sb, 160, newer.u64(DESC + 1, 160) + shift);
    for block in [0, DESC + 1, sb] {
        out.put64(block, 40, total);
        out.seal(block);
    }
    let (old, new) = (layout(&out, DESC + 1), layout(&out, sb));
    assert_eq!((old.xid, new.xid), (5, 6));
    (out, old, new)
}

/// Give the container's object map of `new` (one root leaf, as mkapfs.py writes it for a few volumes) a history
/// for volume `index`: its superblock of `new` becomes transaction 6, and the mapping of transaction 5 names the
/// volume superblock of `old`. The object map of a real container keeps one mapping per volume (it has no
/// snapshots); the format allows this.
pub fn add_history(s: &mut Store, old: &Layout, new: &Layout, index: usize) {
    let (oid, sb) = (new.volumes[index].oid, new.volumes[index].superblock);
    s.put64(sb, 16, 6);
    s.seal(sb);
    let mut mappings: Vec<(u64, u64, u64)> = new.omap.mappings.iter().map(|m| if m.0 == oid { (oid, 6, m.2) } else { *m }).collect();
    mappings.push((oid, 5, old.volumes[index].superblock));
    mappings.sort();
    let leaf = new.omap.nodes[0];
    assert_eq!((new.omap.nodes.len(), s.u16(leaf, 32)), (1, 1 | 2 | 4), "the container's object map is one root leaf");
    let (xid, subtype) = (s.u64(leaf, 16), s.u32(leaf, 28));
    let info = s.block(leaf)[BLOCK - 40..].to_vec();
    let at = leaf as usize * BLOCK;
    s.data[at..at + BLOCK].fill(0);
    s.put64(leaf, 8, leaf);
    s.put64(leaf, 16, xid);
    s.put32(leaf, 24, 0x4000_0002);
    s.put32(leaf, 28, subtype);
    let count = mappings.len();
    let node = &mut s.data[at..at + BLOCK];
    node[32..34].copy_from_slice(&(1u16 | 2 | 4).to_le_bytes());
    node[36..40].copy_from_slice(&(count as u32).to_le_bytes());
    node[42..44].copy_from_slice(&(4 * count as u16).to_le_bytes());
    let (keys, vals) = (56 + 4 * count, BLOCK - 40);
    for (i, (oid, xid, block)) in mappings.iter().enumerate() {
        node[56 + 4 * i..][..2].copy_from_slice(&(16 * i as u16).to_le_bytes());
        node[56 + 4 * i + 2..][..2].copy_from_slice(&(16 * (i as u16 + 1)).to_le_bytes());
        node[keys + 16 * i..][..8].copy_from_slice(&oid.to_le_bytes());
        node[keys + 16 * i + 8..][..8].copy_from_slice(&xid.to_le_bytes());
        let value = vals - 16 * (i + 1);
        node[value + 4..][..4].copy_from_slice(&(BLOCK as u32).to_le_bytes());
        node[value + 8..][..8].copy_from_slice(&block.to_le_bytes());
    }
    // the free space between the keys and the values, the two empty free lists, the tree's facts
    let (key_end, val_start) = (16 * count, vals - 16 * count);
    node[44..46].copy_from_slice(&(key_end as u16).to_le_bytes());
    node[46..48].copy_from_slice(&((val_start - keys - key_end) as u16).to_le_bytes());
    node[48..56].copy_from_slice(&[0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 0, 0]);
    node[BLOCK - 40..].copy_from_slice(&info);
    node[BLOCK - 16..BLOCK - 8].copy_from_slice(&(count as u64).to_le_bytes());
    s.seal(leaf);
}

// ---- folders and mkapfs.py

pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oneios-teststore-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Bytes that differ from block to block.
pub fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| ((i / 7) as u8) ^ (i as u8).wrapping_mul(31) ^ seed).collect()
}

pub fn put(root: &Path, rel: &str, data: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

/// tests/ioslog/mkapfs.py (`manifest`: CARGO_MANIFEST_DIR of a crate of this workspace).
fn mkapfs_py(manifest: &str) -> PathBuf {
    Path::new(manifest).join("../../tests/ioslog/mkapfs.py")
}

/// Whether this host can make a store (python3 for mkapfs.py); says so on stderr when it cannot.
pub fn can_make(manifest: &str) -> bool {
    if Command::new("python3").arg("--version").output().is_ok_and(|o| o.status.success()) {
        return true;
    }
    eprintln!("skipped: no python3 for {}", mkapfs_py(manifest).display());
    false
}

/// A store with the volumes Preboot, xART and Data, as the phone's: `data` is the Data volume's folder.
pub fn make(manifest: &str, dir: &Path, name: &str, data: &Path) -> Store {
    let small = dir.join(format!("{name}-preboot"));
    put(&small, "a/file", format!("preboot of {name}\n").as_bytes());
    let image = dir.join(format!("{name}.img"));
    let made = Command::new("python3")
        .arg("-I")
        .arg(mkapfs_py(manifest))
        .arg(&image)
        .arg(format!("Preboot={}", small.display()))
        .arg("xART=")
        .arg(format!("Data={}", data.display()))
        .output()
        .unwrap();
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    Store::read(&image)
}
