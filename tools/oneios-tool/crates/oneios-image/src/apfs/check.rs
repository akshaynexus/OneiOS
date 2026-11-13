//! Checks before reads, and the choice of the checkpoint: what a store needs that was copied after its kernel was
//! killed (the phone's Data store after every session). Layouts and rules: Apple's "Apple File System Reference"
//! (obj_phys_t, nx_superblock_t, checkpoint_map_phys_t, omap_phys_t, btree_node_phys_t, "Mounting an Apple File
//! System Partition": the superblock with the largest transaction that is not malformed). apfs-fuse takes the
//! newest superblock with a good checksum and stops there; this reader goes on to older checkpoints when an
//! object below that superblock is refused.
//!
//! - Every block address is checked against the container's block count and the file's length before the block is
//!   read; every object's header is checked (Fletcher-64 checksum, type, object id, a transaction not newer than
//!   the checkpoint's) before a field of it is used; every entry of a tree node must lie inside the node. What
//!   fails is refused with the object, its address and the reason: never a bare short read.
//! - The checkpoint: every container superblock of the checkpoint descriptor area is a candidate, tried from the
//!   highest transaction down. The first whose superblock, checkpoint map, object map and (for the volume asked
//!   for) volume superblock, object map and trees all pass is used; the ones skipped are kept with their reasons.
//!
//! Not checked: the contents of files (APFS keeps no checksum of them), the ephemeral objects a checkpoint map
//! names (the space manager, the reaper: this reader never uses them), the nodes of a sealed volume's hashed tree
//! against their hashes (such nodes carry no header; their shape is checked), and of the extent reference tree
//! and the snapshot metadata tree only the root (this reader does not read them).

use super::files::fletcher64;
use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const T_NX_SUPERBLOCK: u32 = 1;
const T_BTREE: u32 = 2;
const T_BTREE_NODE: u32 = 3;
const T_OMAP: u32 = 0xB;
const T_CHECKPOINT_MAP: u32 = 0xC;
const T_FS: u32 = 0xD;
const OID_NX_SUPERBLOCK: u64 = 1;
const OMAP_VAL_DELETED: u32 = 1;
const BTNODE_ROOT: u16 = 1;
const BTNODE_LEAF: u16 = 2;
const BTNODE_FIXED: u16 = 4;
const BTNODE_NOHEADER: u16 = 0x10;
const BTREE_HASHED: u32 = 0x80;
const INCOMPAT_SEALED: u64 = 0x20;
/// No block named the object (the address is a field of block 0, or was given).
pub(super) const NOWHERE: u64 = u64::MAX;

/// One bit per block of the file.
pub(super) struct Bits(Vec<AtomicU64>);

impl Bits {
    pub(super) fn new(blocks: u64) -> Bits {
        Bits((0..blocks.div_ceil(64)).map(|_| AtomicU64::new(0)).collect())
    }
    fn get(&self, block: u64) -> bool {
        self.0.get((block / 64) as usize).is_some_and(|w| w.load(Ordering::Relaxed) & 1 << (block % 64) != 0)
    }
    fn set(&self, block: u64) {
        if let Some(w) = self.0.get((block / 64) as usize) {
            w.fetch_or(1 << (block % 64), Ordering::Relaxed);
        }
    }
}

/// An object's header (obj_phys_t).
#[derive(Clone, Copy)]
struct Header {
    cksum: u64,
    oid: u64,
    xid: u64,
    kind: u32,
    subtype: u32,
}

fn header(block: &[u8]) -> Header {
    let u64_of = |at: usize| u64::from_le_bytes(block[at..at + 8].try_into().expect("8 bytes"));
    let u32_of = |at: usize| u32::from_le_bytes(block[at..at + 4].try_into().expect("4 bytes"));
    Header { cksum: u64_of(0), oid: u64_of(8), xid: u64_of(16), kind: u32_of(24), subtype: u32_of(28) }
}

/// What an address is expected to hold.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum What {
    Super,
    CheckpointMap,
    ContainerOmap,
    ContainerOmapNode,
    VolumeSuper,
    VolumeOmap,
    VolumeOmapNode,
    FsNode,
    ExtentNode,
    ExtentRefRoot,
    SnapMetaRoot,
}

impl What {
    fn name(self) -> &'static str {
        match self {
            What::Super => "the container superblock",
            What::CheckpointMap => "the checkpoint map block",
            What::ContainerOmap => "the container's object map",
            What::ContainerOmapNode => "the node of the container's object map tree",
            What::VolumeSuper => "the volume superblock",
            What::VolumeOmap => "the volume's object map",
            What::VolumeOmapNode => "the node of the volume's object map tree",
            What::FsNode => "the node of the file-system tree",
            What::ExtentNode => "the node of the file extent tree",
            What::ExtentRefRoot => "the root of the extent reference tree",
            What::SnapMetaRoot => "the root of the snapshot metadata tree",
        }
    }
    /// The object types (o_type's low 16 bits) it may have.
    fn types(self) -> &'static [u32] {
        match self {
            What::Super => &[T_NX_SUPERBLOCK],
            What::CheckpointMap => &[T_CHECKPOINT_MAP],
            What::ContainerOmap | What::VolumeOmap => &[T_OMAP],
            What::VolumeSuper => &[T_FS],
            What::ExtentRefRoot | What::SnapMetaRoot => &[T_BTREE],
            What::ContainerOmapNode | What::VolumeOmapNode | What::FsNode | What::ExtentNode => &[T_BTREE, T_BTREE_NODE],
        }
    }
}

fn type_name(kind: u32) -> &'static str {
    match kind & 0xFFFF {
        0 => "nothing",
        T_NX_SUPERBLOCK => "a container superblock",
        T_BTREE => "a tree's root node",
        T_BTREE_NODE => "a tree node",
        5 => "the space manager",
        T_OMAP => "an object map",
        T_CHECKPOINT_MAP => "a checkpoint map",
        T_FS => "a volume superblock",
        0x11 => "the reaper",
        _ => "another object",
    }
}

/// What a tree node must be, for `node_at`.
#[derive(Clone, Copy)]
pub(super) struct Want {
    pub what: What,
    /// The block that named it (NOWHERE: none).
    pub from: u64,
    /// The object id its header must have (`None`: not known, or a node without a header).
    pub oid: Option<u64>,
    pub root: bool,
    pub level: Option<u16>,
}

impl ApfsTree {
    /// The message of a refusal: the object, its address, its header when it was read, who named it, and why.
    fn refuse(&self, what: What, paddr: u64, from: u64, header: Option<&Header>, why: &str) -> Fail {
        let header = header.map(|h| format!(" [type {:#x}/{:#x}: {}, oid {}, xid {}]", h.kind, h.subtype, type_name(h.kind), h.oid, h.xid)).unwrap_or_default();
        let from = if from == NOWHERE { String::new() } else { format!(", named by block {from}") };
        err!("{}: refused {} at block {paddr} ({paddr:#x}){header}{from}: {why}", self.path, what.name())
    }

    /// The bytes of block `paddr`, after its address was checked against the container and the file.
    fn block_at(&self, paddr: u64, what: What, from: u64) -> Result<&[u8], Fail> {
        if !self.strict {
            return self.read(paddr);
        }
        if paddr == 0 && what != What::Super {
            return Err(self.refuse(what, paddr, from, None, "address 0 is block 0, the container superblock's copy: no other object is there"));
        }
        if paddr >= self.blocks {
            return Err(self.refuse(what, paddr, from, None, &format!("the address is past the container's end ({} blocks)", self.blocks)));
        }
        if paddr >= self.file_blocks {
            let why = format!("the block is past the file's end (the file holds {} of the container's {} blocks: a copy that was cut off?)", self.file_blocks, self.blocks);
            return Err(self.refuse(what, paddr, from, None, &why));
        }
        self.read(paddr)
    }

    /// The object at `paddr`, after its address and its header were checked: the checksum, the type, the object
    /// id (`oid`, when it is known) and a transaction that is not newer than the checkpoint's.
    pub(super) fn object(&self, paddr: u64, what: What, oid: Option<u64>, from: u64) -> Result<&[u8], Fail> {
        let data = self.block_at(paddr, what, from)?;
        if !self.strict {
            return Ok(data);
        }
        let h = header(data);
        if !self.good.get(paddr) {
            let sum = fletcher64(data);
            if sum != h.cksum {
                if data.iter().all(|b| *b == 0) {
                    return Err(self.refuse(what, paddr, from, None, "the block is all zeros (never written, or cut off)"));
                }
                let why = format!("its checksum is wrong (stored {:#018x}, computed {sum:#018x}): half written, or not an object", h.cksum);
                return Err(self.refuse(what, paddr, from, Some(&h), &why));
            }
            self.good.set(paddr);
        }
        if !what.types().contains(&(h.kind & 0xFFFF)) {
            let why = format!("it is {}, not {}", type_name(h.kind), what.name());
            return Err(self.refuse(what, paddr, from, Some(&h), &why));
        }
        if let Some(oid) = oid.filter(|oid| *oid != h.oid) {
            return Err(self.refuse(what, paddr, from, Some(&h), &format!("its object id is {}, not {oid}", h.oid)));
        }
        if h.xid > self.xid {
            let why = format!("its transaction {} is newer than the checkpoint's ({}): the block was written again after this checkpoint", h.xid, self.xid);
            return Err(self.refuse(what, paddr, from, Some(&h), &why));
        }
        Ok(data)
    }

    /// The tree node at `paddr`. Checked (a reader opened with checks): its header, that it is the root or not
    /// and on the level it should be, and that its table and every entry of it lie inside the node.
    pub(super) fn node_at(&self, paddr: u64, want: Want) -> Result<Node<'_>, Fail> {
        if !self.strict {
            return self.node(paddr);
        }
        // A sealed volume's hashed file-system tree: its nodes, the root too, say in their flags that they have no
        // header, and the 32 bytes are zeros (24A446's AppOS and SystemOS images). Only there is a node taken
        // without a header: nothing but its shape can be checked then.
        let data = self.block_at(paddr, want.what, want.from)?;
        let bare = self.noheader && want.what == What::FsNode && u16_at(data, 32).is_some_and(|f| f & BTNODE_NOHEADER != 0) && data[..32].iter().all(|b| *b == 0);
        if !bare {
            self.object(paddr, want.what, want.oid, want.from)?;
        }
        let h = (!bare).then(|| header(data));
        let fail = |why: String| self.refuse(want.what, paddr, want.from, h.as_ref(), &why);
        let short = || fail("the block is shorter than a tree node's header".into());
        let flags = u16_at(data, 32).ok_or_else(short)?;
        let level = u16_at(data, 34).ok_or_else(short)?;
        let nkeys = u32_at(data, 36).ok_or_else(short)? as usize;
        let toc_off = u16_at(data, 40).ok_or_else(short)? as usize;
        let toc_len = u16_at(data, 42).ok_or_else(short)? as usize;
        let root = flags & BTNODE_ROOT != 0;
        if root != want.root {
            return Err(fail(if want.root { "it is not a root node (its flags lack the root bit)".into() } else { "it is a root node where a child node should be".into() }));
        }
        if h.is_some_and(|h| (h.kind & 0xFFFF == T_BTREE) != root) {
            return Err(fail(format!("its type and its flags ({flags:#x}) disagree about being a root")));
        }
        if (flags & BTNODE_LEAF != 0) != (level == 0) {
            return Err(fail(format!("its flags ({flags:#x}) and its level ({level}) disagree about being a leaf")));
        }
        if let Some(need) = want.level.filter(|need| *need != level) {
            return Err(fail(format!("it is on level {level}, not {need}")));
        }
        let toc = 56 + toc_off;
        let keys = toc + toc_len;
        let vals = self.block - if root { 40 } else { 0 };
        let fixed = flags & BTNODE_FIXED != 0;
        let entry = if fixed { 4 } else { 8 };
        if keys > vals || nkeys.checked_mul(entry).is_none_or(|n| n > toc_len) {
            return Err(fail(format!("its table does not fit ({nkeys} entries, a table of {toc_len} bytes at {toc})")));
        }
        if root && u32_at(data, self.block - 36) != Some(self.block as u32) {
            return Err(fail(format!("its tree has nodes of {} bytes; this reader reads nodes of one block ({})", u32_at(data, self.block - 36).unwrap_or(0), self.block)));
        }
        if !self.shaped.get(paddr) {
            // every key and value inside the node's key and value area
            let room = vals - keys;
            for i in 0..nkeys {
                let at = toc + entry * i;
                let (k_off, k_len, v_off, v_len) = if fixed {
                    (u16_at(data, at).unwrap_or(0) as usize, 16, u16_at(data, at + 2).unwrap_or(0) as usize, if level == 0 { 16 } else { 8 })
                } else {
                    let field = |n: usize| u16_at(data, at + n).unwrap_or(0) as usize;
                    (field(0), field(2), field(4), field(6))
                };
                let good = k_off + k_len <= room && v_off <= room && v_len <= v_off && k_len >= 8 && (level == 0 || v_len >= 8);
                if !good {
                    return Err(fail(format!("entry {i} of {nkeys} is outside the node (key {k_len} bytes at {k_off}, value {v_len} bytes at {v_off} from the end, {room} bytes of room): half written?")));
                }
            }
            self.shaped.set(paddr);
        }
        Ok(Node { data, flags, level, nkeys, toc, keys, vals })
    }

    /// Every node of the object map tree whose root is `root`, each checked; `each(oid, xid, flags, paddr)` for
    /// every mapping. A mapping of a live object to an address outside the container is refused.
    fn walk_omap(&self, root: u64, what: What, from: u64, each: &mut dyn FnMut(u64, u64, u32, u64)) -> Result<u64, Fail> {
        // (node, the block that named it, its level when known)
        let mut todo = vec![(root, from, None::<u16>)];
        let mut nodes = 0u64;
        while let Some((paddr, from, level)) = todo.pop() {
            let n = self.node_at(paddr, Want { what, from, oid: Some(paddr), root: nodes == 0, level })?;
            nodes += 1;
            if nodes > self.file_blocks {
                return Err(self.refuse(what, paddr, from, None, "the tree has more nodes than the file has blocks: a loop"));
            }
            if n.flags & BTNODE_FIXED == 0 {
                return Err(self.refuse(what, paddr, from, None, "an object map's node without fixed-size entries"));
            }
            for i in 0..n.nkeys {
                let k = n.keys + u16_at(n.data, n.toc + 4 * i).unwrap_or(0) as usize;
                let v = n.vals - u16_at(n.data, n.toc + 4 * i + 2).unwrap_or(0) as usize;
                let (oid, xid) = (u64_at(n.data, k).unwrap_or(0), u64_at(n.data, k + 8).unwrap_or(0));
                if n.level != 0 {
                    todo.push((u64_at(n.data, v).unwrap_or(0), paddr, Some(n.level - 1)));
                    continue;
                }
                let (flags, to) = (u32_at(n.data, v).unwrap_or(0), u64_at(n.data, v + 8).unwrap_or(0));
                if flags & OMAP_VAL_DELETED == 0 && (to == 0 || to >= self.blocks) {
                    let why = format!("it maps object {oid} (xid {xid}) to block {to} ({to:#x}), which is outside the container ({} blocks)", self.blocks);
                    return Err(self.refuse(what, paddr, from, None, &why));
                }
                each(oid, xid, flags, to);
            }
        }
        Ok(nodes)
    }

    /// Every node of the file-system tree, each checked (a child through the volume's object map, as a lookup
    /// reaches it). Returns how many nodes.
    fn walk_fs(&self) -> Result<u64, Fail> {
        let mut todo = vec![(self.root, self.root_oid, NOWHERE, None::<u16>, true)];
        let mut nodes = 0u64;
        while let Some((paddr, oid, from, level, root)) = todo.pop() {
            let n = self.node_at(paddr, Want { what: What::FsNode, from, oid: Some(oid), root, level })?;
            nodes += 1;
            if nodes > self.file_blocks {
                return Err(self.refuse(What::FsNode, paddr, from, None, "the tree has more nodes than the file has blocks: a loop"));
            }
            if n.flags & BTNODE_FIXED != 0 {
                return Err(self.refuse(What::FsNode, paddr, from, None, "a file-system tree node with fixed-size entries"));
            }
            if n.level == 0 {
                continue;
            }
            for i in 0..n.nkeys {
                let v = n.vals - u16_at(n.data, n.toc + 8 * i + 4).unwrap_or(0) as usize;
                let child = u64_at(n.data, v).unwrap_or(0);
                let (at, oid) = self.fs_child(child, paddr)?;
                todo.push((at, oid, paddr, Some(n.level - 1), false));
            }
        }
        Ok(nodes)
    }

    /// Where the file-system tree's child `child` (an index entry's value) is, and the object id its header has.
    pub(super) fn fs_child(&self, child: u64, from: u64) -> Result<(u64, u64), Fail> {
        if self.physical {
            return Ok((child, child));
        }
        let oid = self.child_base.wrapping_add(child);
        let at = self.omap_lookup(self.vol_omap, oid).map_err(|e| match e {
            Fail::Error(m) if self.strict && !m.contains(": refused ") => err!("{m} (a child of the file-system tree's node at block {from})"),
            other => other,
        })?;
        Ok((at, oid))
    }

    /// Every node of a sealed volume's file extent tree, each checked.
    fn walk_fext(&self) -> Result<u64, Fail> {
        let mut todo = vec![(self.fext_root, NOWHERE, None::<u16>)];
        let mut nodes = 0u64;
        while let Some((paddr, from, level)) = todo.pop() {
            let n = self.node_at(paddr, Want { what: What::ExtentNode, from, oid: Some(paddr), root: nodes == 0, level })?;
            nodes += 1;
            if nodes > self.file_blocks {
                return Err(self.refuse(What::ExtentNode, paddr, from, None, "the tree has more nodes than the file has blocks: a loop"));
            }
            if n.level == 0 {
                continue;
            }
            for i in 0..n.nkeys {
                let v_off = if n.flags & BTNODE_FIXED != 0 { u16_at(n.data, n.toc + 4 * i + 2) } else { u16_at(n.data, n.toc + 8 * i + 4) };
                todo.push((u64_at(n.data, n.vals - v_off.unwrap_or(0) as usize).unwrap_or(0), paddr, Some(n.level - 1)));
            }
        }
        Ok(nodes)
    }

    /// A file extent's blocks, checked against the container before they are read.
    pub(super) fn extent_at(&self, stream: u64, offset: u64, len: usize, block: u64) -> Result<&[u8], Fail> {
        let start = self.base as u128 + block as u128 * self.block as u128;
        let end = start + len as u128;
        if self.strict {
            let last = block as u128 + (len as u128).div_ceil(self.block as u128);
            let why = if last > self.blocks as u128 {
                format!("it runs past the container's end ({} blocks)", self.blocks)
            } else if end > self.map.len() as u128 {
                format!("it runs past the file's end (the file holds {} of the container's {} blocks: a copy that was cut off?)", self.file_blocks, self.blocks)
            } else {
                return Ok(&self.map[start as usize..end as usize]);
            };
            return Err(err!("{}: refused an extent of data stream {stream}: {len} bytes at file offset {offset} from block {block} ({block:#x}): {why}", self.path));
        }
        if end > self.map.len() as u128 {
            return Err(err!("{}: short read at block {block}", self.path));
        }
        Ok(&self.map[start as usize..end as usize])
    }
}

/// Which state of the container is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// As block 0's superblock has it: the state of the last clean unmount (imagetool.py reads this one).
    Block0,
    /// The newest checkpoint that passes the checks.
    Newest,
    /// The checkpoint of this transaction, if it passes the checks.
    Xid(u64),
}

/// One container superblock: of the checkpoint descriptor area, or block 0's.
pub struct Checkpoint {
    /// Its transaction, as the block says (not to be trusted when `refused` names the checksum).
    pub xid: u64,
    /// Where the superblock is.
    pub block: u64,
    /// Why the container cannot be read at this checkpoint (`None`: its superblock, checkpoint map and object
    /// map passed).
    pub refused: Option<String>,
    blocks: u64,
    fs_oids: Vec<u64>,
    /// The container's object map: (oid, xid, flags, block).
    mappings: Vec<(u64, u64, u32, u64)>,
}

/// A volume as one volume superblock has it, after its checks.
#[derive(Clone)]
struct VolState {
    xid: u64,
    vol_omap: u64,
    physical: bool,
    root: u64,
    root_oid: u64,
    child_base: u64,
    fext_root: u64,
    noheader: bool,
    name: String,
    role: String,
    /// Tree nodes that were checked (0: only the roots, `Pick::Block0`).
    nodes: u64,
}

enum Bad {
    /// This state of the volume cannot be read: an older one may.
    Refused(String),
    /// No state will do (an encrypted volume).
    Final(Fail),
}

/// Which checkpoint a volume was opened at, and what was passed over on the way.
#[derive(Clone, Debug)]
pub struct Opened {
    pub pick: Pick,
    /// The checkpoint's transaction and the block of its superblock.
    pub xid: u64,
    pub block: u64,
    /// The transaction of the volume superblock that is read.
    pub volume_xid: u64,
    pub block0_xid: u64,
    /// How many container superblocks were candidates.
    pub candidates: usize,
    /// The candidates newer than the one used: (transaction, block, why it was refused).
    pub skipped: Vec<(u64, u64, String)>,
    /// Tree nodes checked before the volume was accepted.
    pub nodes: u64,
}

impl Opened {
    /// For stderr: which checkpoint is read, and why not a newer one.
    pub fn text(&self, path: &str, volume: usize, name: &str) -> String {
        let head = format!("{path}: volume {volume} ({name}) is read at checkpoint xid {} (superblock at block {}, volume superblock xid {})", self.xid, self.block, self.volume_xid);
        match self.pick {
            Pick::Block0 if self.skipped.is_empty() => {
                format!("{path}: volume {volume} ({name}) is read as block 0 has it (xid {}, volume superblock xid {}); the checkpoint area was not looked at", self.xid, self.volume_xid)
            }
            _ if self.skipped.is_empty() => format!(
                "{head}: {}; block 0 has xid {}; {} tree nodes checked",
                if self.pick == Pick::Newest { format!("the newest of {}", self.candidates) } else { format!("the one asked for of {}", self.candidates) },
                self.block0_xid,
                self.nodes
            ),
            _ => {
                // skipped: newer checkpoints, and (a container's object map with a history) a newer state of the
                // volume in the checkpoint that is read
                let newer = self.skipped.iter().filter(|s| s.0 > self.xid).count();
                let mut text = match (newer, self.skipped.len() - newer) {
                    (newer, 0) => format!("{head}; {newer} newer of {} skipped:", self.candidates),
                    (0, _) => format!("{head}; a newer state of the volume in this checkpoint was skipped:"),
                    (newer, _) => format!("{head}; {newer} newer of {} and a newer state of the volume in this checkpoint were skipped:", self.candidates),
                };
                for (xid, block, why) in &self.skipped {
                    text += &format!("\n  xid {xid} at block {block}: {why}");
                }
                text + &format!("\n  (block 0 has xid {}; {} tree nodes checked)", self.block0_xid, self.nodes)
            }
        }
    }
}

/// A container and its checkpoints, newest first.
pub struct Container {
    tree: ApfsTree,
    pick: Pick,
    pub checkpoints: Vec<Checkpoint>,
    pub block0_xid: u64,
    /// The checkpoint descriptor area: its first block and its length (0: not a plain run of blocks).
    pub area: (u64, u64),
    /// How many container superblocks were found (`Pick::Xid` keeps one of them in `checkpoints`).
    found: usize,
    volumes: HashMap<u64, Result<VolState, String>>,
}

impl Container {
    /// The container of a raw image (plain, or one APFS partition in a GPT), with its candidates checked as far
    /// as the container goes: superblock, checkpoint map, object map.
    pub fn open(path: &str, pick: Pick) -> Result<Container, Fail> {
        let (base, head) = container_head(path)?;
        let bad = || outside(path);
        let block = u32_at(&head, 36).ok_or_else(bad)?;
        if !block.is_power_of_two() || !(4096..=65536).contains(&block) {
            return Err(err!("{path}: block 0 says the container's blocks have {block} bytes: not a container superblock this reader can start from"));
        }
        let map = Arc::new(Map::open(path).map_err(|e| crash!("{e:#}"))?);
        let file_blocks = (map.len() as u64).saturating_sub(base) / block as u64;
        let mut tree = ApfsTree {
            path: path.to_string(),
            map,
            base,
            block: block as usize,
            vol_omap: 0,
            physical: false,
            root: 0,
            root_oid: 0,
            child_base: 0,
            xid: u64::MAX,
            fext_root: 0,
            blocks: u64_at(&head, 40).ok_or_else(bad)?,
            file_blocks,
            strict: true,
            noheader: false,
            good: Arc::new(Bits::new(file_blocks)),
            shaped: Arc::new(Bits::new(file_blocks)),
            opened: None,
            name: String::new(),
            role: String::new(),
        };
        let block0_xid = u64_at(&head, 16).ok_or_else(bad)?;
        let uuid: [u8; 16] = head[72..88].try_into().expect("16 bytes");
        let mut at = vec![];
        let mut area = (0, 0);
        if pick != Pick::Block0 {
            let count = u32_at(&head, 104).ok_or_else(bad)?;
            let first = u64_at(&head, 112).ok_or_else(bad)?;
            // the high bit: the area is a tree, not a run of blocks (not seen; block 0 is the only candidate then)
            if count & 0x8000_0000 == 0 {
                area = (first, count as u64);
                for i in 0..count as u64 {
                    let paddr = first.saturating_add(i);
                    if paddr >= tree.blocks.min(tree.file_blocks) {
                        break;
                    }
                    if tree.read(paddr)?.get(32..36) == Some(b"NXSB") {
                        at.push(paddr);
                    }
                }
            }
        }
        let mut checkpoints: Vec<Checkpoint> = at.into_iter().map(|paddr| tree.checkpoint(paddr, &uuid, area)).collect();
        if !checkpoints.iter().any(|c| c.xid == block0_xid && c.refused.is_none()) {
            checkpoints.push(tree.checkpoint(0, &uuid, area));
        }
        // newest first; of two with one transaction, the area's
        checkpoints.sort_by(|a, b| b.xid.cmp(&a.xid).then(b.block.cmp(&a.block)));
        let found = checkpoints.len();
        if let Pick::Xid(xid) = pick {
            let all: Vec<String> = checkpoints.iter().map(|c| c.xid.to_string()).collect();
            checkpoints.retain(|c| c.xid == xid);
            if checkpoints.is_empty() {
                return Err(err!("{path}: no checkpoint of transaction {xid} (the container's: {})", all.join(", ")));
            }
        }
        // (each candidate was checked with its own transaction and block count)
        tree.xid = u64::MAX;
        tree.blocks = u64_at(&head, 40).ok_or_else(bad)?;
        Ok(Container { tree, pick, checkpoints, block0_xid, area, found, volumes: HashMap::new() })
    }

    pub fn block_size(&self) -> usize {
        self.tree.block
    }

    /// The container's blocks, and how many of them the file holds.
    pub fn blocks(&self) -> (u64, u64) {
        (self.tree.blocks, self.tree.file_blocks)
    }

    /// The reasons of every candidate, for the error when none will do.
    fn reasons(&self) -> String {
        self.checkpoints.iter().map(|c| format!("\n  xid {} at block {}: {}", c.xid, c.block, c.refused.as_deref().unwrap_or("the container passed"))).collect()
    }

    /// The newest checkpoint at which the container can be read.
    fn newest(&self) -> Result<&Checkpoint, Fail> {
        self.checkpoints.iter().find(|c| c.refused.is_none()).ok_or_else(|| {
            let which = if self.pick == Pick::Block0 { "block 0's superblock cannot be read (without --block0 the checkpoint area is searched)".to_string() } else { format!("none of the {} container superblocks can be read", self.checkpoints.len()) };
            err!("{}: {which}:{}", self.tree.path, self.reasons())
        })
    }

    /// How many volume places the container has at its newest readable checkpoint: one more than the number of
    /// its last volume. A volume's number is its place in the superblock's list (`nx_fs_oid`; `apfs_fs_index`),
    /// as apfs-fuse's `vol=N` counts: a place may be empty (the Data store iOS made on the PC has its one volume
    /// in place 2).
    pub fn volume_count(&self) -> Result<usize, Fail> {
        Ok(self.newest()?.fs_oids.len())
    }

    /// The numbers of the container's volumes (the places that are not empty).
    pub fn volumes(&self) -> Result<Vec<usize>, Fail> {
        Ok(self.newest()?.fs_oids.iter().enumerate().filter(|(_, oid)| **oid != 0).map(|(index, _)| index).collect())
    }

    /// Volume `index` at the newest checkpoint where the container and the volume pass the checks.
    pub fn volume(&mut self, index: usize) -> Result<ApfsTree, Fail> {
        let path = self.tree.path.clone();
        let Some(oid) = self.newest()?.fs_oids.get(index).copied().filter(|oid| *oid != 0) else {
            let have: Vec<String> = self.volumes()?.iter().map(usize::to_string).collect();
            return Err(err!("{path}: no volume {index} (the container's volumes: {})", if have.is_empty() { "none".to_string() } else { have.join(", ") }));
        };
        let deep = self.pick != Pick::Block0;
        let mut skipped: Vec<(u64, u64, String)> = Vec::new();
        for i in 0..self.checkpoints.len() {
            let (xid, block, blocks) = (self.checkpoints[i].xid, self.checkpoints[i].block, self.checkpoints[i].blocks);
            if let Some(why) = &self.checkpoints[i].refused {
                skipped.push((xid, block, why.clone()));
                continue;
            }
            if !self.checkpoints[i].fs_oids.contains(&oid) {
                skipped.push((xid, block, format!("volume {index} (object {oid}) is not a volume of this checkpoint")));
                continue;
            }
            // the volume superblocks this checkpoint's object map has for the volume, newest first: the object map
            // of a container keeps one (it has no snapshots), but the format allows more
            let mut states: Vec<(u64, u64)> = self.checkpoints[i].mappings.iter().filter(|m| m.0 == oid && m.1 <= xid && m.2 & OMAP_VAL_DELETED == 0).map(|m| (m.1, m.3)).collect();
            states.sort_by(|a, b| b.cmp(a));
            let mut why = Vec::new();
            if states.is_empty() {
                why.push(format!("volume {index} (object {oid}) is not in the container's object map"));
            }
            for (state_xid, paddr) in states {
                self.tree.xid = xid;
                self.tree.blocks = blocks;
                let state = match self.volumes.get(&paddr) {
                    Some(known) => known.clone(),
                    None => {
                        let state = match self.tree.volume_state(oid, paddr, deep) {
                            Ok(state) => Ok(state),
                            Err(Bad::Refused(m)) => Err(m),
                            Err(Bad::Final(e)) => return Err(e),
                        };
                        self.volumes.insert(paddr, state.clone());
                        state
                    }
                };
                match state {
                    Ok(state) => {
                        if !why.is_empty() {
                            // the same checkpoint, an older state of the volume
                            skipped.push((xid, block, format!("{} (an older state of the volume in the same checkpoint is read)", why.join("; "))));
                        }
                        let mut tree = self.tree.fork();
                        tree.vol_omap = state.vol_omap;
                        tree.physical = state.physical;
                        tree.root = state.root;
                        tree.root_oid = state.root_oid;
                        tree.child_base = state.child_base;
                        tree.fext_root = state.fext_root;
                        tree.noheader = state.noheader;
                        tree.name = state.name;
                        tree.role = state.role;
                        tree.opened = Some(Opened { pick: self.pick, xid, block, volume_xid: state.xid, block0_xid: self.block0_xid, candidates: self.found, skipped, nodes: state.nodes });
                        return Ok(tree);
                    }
                    Err(m) => why.push(format!("volume {index}, superblock xid {state_xid} at block {paddr}: {}", m.strip_prefix(&format!("{path}: ")).unwrap_or(&m))),
                }
            }
            skipped.push((xid, block, why.join("; ")));
        }
        let lines: String = skipped.iter().map(|(xid, block, why)| format!("\n  xid {xid} at block {block}: {why}")).collect();
        Err(err!("{path}: volume {index} cannot be read at any of the {} checkpoints:{lines}", self.checkpoints.len()))
    }

    /// The volume `want` names: its number, or its name at the checkpoint it is read at; `None`: the first one.
    pub fn volume_named(&mut self, want: Option<&str>) -> Result<(usize, ApfsTree), Fail> {
        let Some(want) = want else {
            let first = self.volumes()?.first().copied().unwrap_or(0);
            return Ok((first, self.volume(first)?));
        };
        if let Ok(index) = want.parse::<usize>() {
            return Ok((index, self.volume(index)?));
        }
        let mut names = Vec::new();
        for index in self.volumes()? {
            match self.volume(index) {
                Ok(tree) if tree.name == want => return Ok((index, tree)),
                Ok(tree) => names.push(tree.name),
                Err(_) => {}
            }
        }
        Err(err!("{}: no volume named {want} (it has: {})", self.tree.path, names.join(", ")))
    }

    /// What `image apfs-checkpoints` prints: every candidate, and how each volume fares at it.
    pub fn report(&mut self) -> String {
        let path = self.tree.path.clone();
        let (blocks, held) = self.blocks();
        let mut text = format!("{path}: {blocks} blocks of {} bytes{}; block 0 has xid {}; ", self.tree.block, if held < blocks { format!(" (the file holds {held})") } else { String::new() }, self.block0_xid);
        text += &match self.area {
            (_, 0) => "the checkpoint descriptor area is not a run of blocks: only block 0's superblock is a candidate\n".to_string(),
            (first, count) => format!("the checkpoint descriptor area is blocks {first}..{}, {} container superblocks\n", first + count, self.checkpoints.iter().filter(|c| c.block != 0).count()),
        };
        let deep = self.pick != Pick::Block0;
        for i in 0..self.checkpoints.len() {
            let (xid, block, blocks) = (self.checkpoints[i].xid, self.checkpoints[i].block, self.checkpoints[i].blocks);
            text += &format!("xid {xid} at block {block}: ");
            if let Some(why) = &self.checkpoints[i].refused {
                text += &format!("{why}\n");
                continue;
            }
            text += &format!("the container passes ({} volumes)\n", self.checkpoints[i].fs_oids.iter().filter(|oid| **oid != 0).count());
            for (index, oid) in self.checkpoints[i].fs_oids.clone().into_iter().enumerate() {
                if oid == 0 {
                    continue;
                }
                let mut states: Vec<(u64, u64)> = self.checkpoints[i].mappings.iter().filter(|m| m.0 == oid && m.1 <= xid && m.2 & OMAP_VAL_DELETED == 0).map(|m| (m.1, m.3)).collect();
                states.sort_by(|a, b| b.cmp(a));
                if states.is_empty() {
                    text += &format!("  volume {index} (object {oid}): not in the container's object map\n");
                }
                for (state_xid, paddr) in states {
                    self.tree.xid = xid;
                    self.tree.blocks = blocks;
                    let state = self.volumes.entry(paddr).or_insert_with(|| match self.tree.volume_state(oid, paddr, deep) {
                        Ok(state) => Ok(state),
                        Err(Bad::Refused(m)) | Err(Bad::Final(Fail::Error(m))) | Err(Bad::Final(Fail::Crash(m))) => Err(m),
                        Err(Bad::Final(Fail::Usage)) => Err("wrong arguments".into()),
                    });
                    text += &match state {
                        Ok(s) => format!("  volume {index} ({}): passes; superblock xid {state_xid} at block {paddr}, {} tree nodes checked\n", s.name, s.nodes),
                        Err(m) => format!("  volume {index} (object {oid}): superblock xid {state_xid} at block {paddr}: {}\n", m.strip_prefix(&format!("{path}: ")).unwrap_or(m)),
                    };
                }
            }
        }
        text
    }
}

impl ApfsTree {
    /// A reader of the same container for another volume: the map and the record of checked blocks are shared.
    fn fork(&self) -> ApfsTree {
        ApfsTree {
            path: self.path.clone(),
            map: self.map.clone(),
            base: self.base,
            block: self.block,
            vol_omap: 0,
            physical: false,
            root: 0,
            root_oid: 0,
            child_base: 0,
            xid: self.xid,
            fext_root: 0,
            blocks: self.blocks,
            file_blocks: self.file_blocks,
            strict: true,
            noheader: false,
            good: self.good.clone(),
            shaped: self.shaped.clone(),
            opened: None,
            name: String::new(),
            role: String::new(),
        }
    }

    /// The container superblock at `paddr` as a candidate: its own checks, its checkpoint map, the container's
    /// object map (every node).
    fn checkpoint(&mut self, paddr: u64, uuid: &[u8; 16], area: (u64, u64)) -> Checkpoint {
        let path = format!("{}: ", self.path);
        let mut cp = Checkpoint { xid: 0, block: paddr, refused: None, blocks: self.blocks, fs_oids: Vec::new(), mappings: Vec::new() };
        if let Ok(data) = self.read(paddr) {
            cp.xid = u64_at(data, 16).unwrap_or(0);
        }
        let checked = (|| -> Result<(), Fail> {
            // its own transaction is the newest a block of it may have
            self.xid = u64::MAX;
            let data = self.object(paddr, What::Super, Some(OID_NX_SUPERBLOCK), NOWHERE)?;
            let h = header(data);
            let fail = |why: String| self.refuse(What::Super, paddr, NOWHERE, Some(&h), &why);
            let bad = || fail("a field is outside the block".into());
            if data.get(32..36) != Some(b"NXSB") {
                return Err(fail("no NXSB magic".into()));
            }
            if u32_at(data, 36) != Some(self.block as u32) {
                return Err(fail(format!("its block size is {}, block 0's is {}", u32_at(data, 36).unwrap_or(0), self.block)));
            }
            if data.get(72..88) != Some(&uuid[..]) {
                return Err(fail(format!("it is of another container ({}; block 0's is {}): left from before the store was made again", uuid_text(&data[72..88]), uuid_text(uuid))));
            }
            let blocks = u64_at(data, 40).ok_or_else(bad)?;
            // the checkpoint's blocks in the descriptor area: its maps, then this superblock as the last
            let (index, len) = (u32_at(data, 136).ok_or_else(bad)? as u64, u32_at(data, 140).ok_or_else(bad)? as u64);
            let (first, count) = area;
            let mut maps = Vec::new();
            if paddr != 0 && count != 0 {
                if index >= count || len == 0 || len > count || paddr != first + (index + len - 1) % count {
                    return Err(fail(format!("it is not the last block of its own checkpoint (blocks {index}..+{len} of the area's {count})")));
                }
                maps = (0..len - 1).map(|i| first + (index + i) % count).collect();
            }
            let (xid, omap, max_fs) = (h.xid, u64_at(data, 160).ok_or_else(bad)?, u32_at(data, 180).ok_or_else(bad)?);
            // the volumes by their place in the list; the empty places after the last volume are left out
            let mut fs_oids: Vec<u64> = (0..max_fs.min(100) as usize).map(|i| u64_at(data, 184 + 8 * i).unwrap_or(0)).collect();
            fs_oids.truncate(fs_oids.iter().rposition(|oid| *oid != 0).map_or(0, |last| last + 1));
            self.xid = xid;
            self.blocks = blocks;
            for (i, map) in maps.iter().enumerate() {
                let data = self.object(*map, What::CheckpointMap, Some(*map), paddr)?;
                let h = header(data);
                let fail = |why: String| self.refuse(What::CheckpointMap, *map, paddr, Some(&h), &why);
                if h.xid != xid {
                    return Err(fail(format!("it is of transaction {}, its superblock of {xid}", h.xid)));
                }
                let (flags, entries) = (u32_at(data, 32).unwrap_or(0), u32_at(data, 36).unwrap_or(0) as usize);
                if (flags & 1 != 0) != (i + 1 == maps.len()) {
                    return Err(fail(format!("block {} of {} of the checkpoint's map {} the mark of the last one", i + 1, maps.len(), if flags & 1 != 0 { "has" } else { "lacks" })));
                }
                if 40 + 40 * entries > self.block {
                    return Err(fail(format!("it says it has {entries} entries, more than fit a block")));
                }
                for e in 0..entries {
                    let at = 40 + 40 * e;
                    let (size, to) = (u32_at(data, at + 8).unwrap_or(0) as u64, u64_at(data, at + 32).unwrap_or(0));
                    if to == 0 || to.saturating_add(size.div_ceil(self.block as u64)) > blocks {
                        return Err(fail(format!("entry {e} puts object {} at block {to} ({to:#x}), {size} bytes: outside the container ({blocks} blocks)", u64_at(data, at + 24).unwrap_or(0))));
                    }
                }
            }
            let data = self.object(omap, What::ContainerOmap, Some(omap), paddr)?;
            let root = u64_at(data, 48).ok_or_else(|| outside(&self.path))?;
            let mut mappings = Vec::new();
            self.walk_omap(root, What::ContainerOmapNode, omap, &mut |oid, xid, flags, to| mappings.push((oid, xid, flags, to)))?;
            cp.xid = xid;
            cp.blocks = blocks;
            cp.fs_oids = fs_oids;
            cp.mappings = mappings;
            Ok(())
        })();
        if let Err(e) = checked {
            let text = e.to_string();
            cp.refused = Some(text.strip_prefix(&path).unwrap_or(&text).to_string());
        }
        cp
    }

    /// The volume as the superblock at `paddr` has it. `deep`: every node of its object map and of its trees is
    /// checked; otherwise the roots only (the rest is checked when it is read).
    fn volume_state(&mut self, oid: u64, paddr: u64, deep: bool) -> Result<VolState, Bad> {
        self.noheader = false;
        let refused = |e: Fail| Bad::Refused(e.to_string());
        let sb = self.object(paddr, What::VolumeSuper, Some(oid), NOWHERE).map_err(refused)?;
        let h = header(sb);
        let fail = |why: &str| Bad::Refused(self.refuse(What::VolumeSuper, paddr, NOWHERE, Some(&h), why).to_string());
        let field = || fail("a field is outside the block");
        if sb.get(32..36) != Some(b"APSB") {
            return Err(fail("no APSB magic"));
        }
        let name = sb.get(704..960).ok_or_else(field)?;
        let name = String::from_utf8_lossy(&name[..name.iter().position(|b| *b == 0).unwrap_or(name.len())]).into_owned();
        let role = role_name(u16_at(sb, 964).ok_or_else(field)?);
        if u64_at(sb, 0x108).ok_or_else(field)? & 0x1 == 0 {
            return Err(Bad::Final(err!("{}: volume {name} (object {oid}) is encrypted", self.path)));
        }
        let (omap, root_oid) = (u64_at(sb, 0x80).ok_or_else(field)?, u64_at(sb, 0x88).ok_or_else(field)?);
        let (extentref, snap_meta) = (u64_at(sb, 0x90).ok_or_else(field)?, u64_at(sb, 0x98).ok_or_else(field)?);
        let physical = u32_at(sb, 0x74).ok_or_else(field)? & OBJ_PHYSICAL != 0;
        let sealed = u64_at(sb, 56).ok_or_else(field)? & INCOMPAT_SEALED != 0;
        let fext_root = if sealed { u64_at(sb, 1032).ok_or_else(field)? } else { 0 };
        let vol_omap = u64_at(self.object(omap, What::VolumeOmap, Some(omap), paddr).map_err(refused)?, 48).ok_or_else(field)?;
        let mut nodes = 0;
        if deep {
            nodes += self.walk_omap(vol_omap, What::VolumeOmapNode, omap, &mut |_, _, _, _| {}).map_err(refused)?;
        } else {
            self.node_at(vol_omap, Want { what: What::VolumeOmapNode, from: omap, oid: Some(vol_omap), root: true, level: None }).map_err(refused)?;
        }
        self.vol_omap = vol_omap;
        self.physical = physical;
        self.child_base = 0;
        self.noheader = sealed;
        let root = if physical { root_oid } else { self.omap_lookup(vol_omap, root_oid).map_err(refused)? };
        let top = self.node_at(root, Want { what: What::FsNode, from: paddr, oid: Some(root_oid), root: true, level: None }).map_err(refused)?;
        let flags = u32_at(top.data, self.block - 40).unwrap_or(0);
        let state = VolState {
            xid: h.xid,
            vol_omap,
            physical,
            root,
            root_oid,
            // in the hashed tree of a sealed volume an index entry's child is a number relative to the root's object id
            child_base: if flags & BTREE_HASHED != 0 && !physical { root_oid } else { 0 },
            fext_root,
            noheader: sealed,
            name,
            role,
            nodes: 0,
        };
        self.root = state.root;
        self.root_oid = state.root_oid;
        self.child_base = state.child_base;
        self.fext_root = state.fext_root;
        if fext_root != 0 {
            self.node_at(fext_root, Want { what: What::ExtentNode, from: paddr, oid: Some(fext_root), root: true, level: None }).map_err(refused)?;
        }
        // not read by this reader: their roots only, as a sign of a checkpoint that was not written to its end
        for (what, at) in [(What::ExtentRefRoot, extentref), (What::SnapMetaRoot, snap_meta)] {
            if at != 0 {
                self.object(at, what, Some(at), paddr).map_err(refused)?;
            }
        }
        if deep {
            nodes += self.walk_fs().map_err(refused)?;
            if fext_root != 0 {
                nodes += self.walk_fext().map_err(refused)?;
            }
        }
        Ok(VolState { nodes, ..state })
    }

    /// Which checkpoint this volume was opened at (`None`: opened without checks, as imagetool.py reads).
    pub fn opened(&self) -> Option<&Opened> {
        self.opened.as_ref()
    }
}
