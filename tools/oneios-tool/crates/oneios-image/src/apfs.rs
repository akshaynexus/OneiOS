//! Read-only facts of a raw APFS image: `apfs_info` (container and volume UUIDs, for the manifest) and `ApfsTree`
//! (the owners, modes and xattr names of a volume's files, from the file-system tree's records; no file data).
//! Layouts: Apple's "Apple File System Reference". The checks and the messages are imagetool.py's.

use crate::{crash, err, Fail};
use oneios_common::pyjson::Value;
use oneios_common::Map;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::FileExt;

/// A read outside a block: Python's struct.error, which ends the tool with a traceback.
fn outside(path: &str) -> Fail {
    crash!("{path}: a field is outside its block (the image is damaged or not what this tool reads)")
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at.checked_add(2)?)?.try_into().ok()?))
}
fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// `str(uuid.UUID(bytes=b)).upper()`.
fn uuid_text(b: &[u8]) -> String {
    let h: Vec<String> = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{}-{}-{}-{}-{}", h[0..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..16].concat())
}

fn role_name(role: u16) -> String {
    match role {
        0x0001 => "system".into(),
        0x0002 => "user".into(),
        0x0004 => "recovery".into(),
        0x0008 => "vm".into(),
        0x0010 => "preboot".into(),
        0x0040 => "data".into(),
        0x00C0 => "update".into(),
        0x0100 => "xart".into(),
        0x0140 => "hardware".into(),
        0 => "none".into(),
        other => format!("{other:#x}"),
    }
}

pub struct Volume {
    pub name: String,
    pub uuid: String,
    pub role: String,
    pub case_sensitive: bool,
    pub sealed: bool,
}

pub struct Info {
    pub container_offset: u64,
    pub block_size: u32,
    pub block_count: u64,
    pub container_uuid: String,
    pub volumes: Vec<Volume>,
}

impl Info {
    /// The dict `apfs_info` returns, in its key order.
    pub fn to_value(&self) -> Value {
        let volumes: Vec<Value> = self
            .volumes
            .iter()
            .map(|v| {
                Value::Dict(vec![
                    ("name".into(), Value::str(&v.name)),
                    ("uuid".into(), Value::str(&v.uuid)),
                    ("role".into(), Value::str(&v.role)),
                    ("case_sensitive".into(), Value::Bool(v.case_sensitive)),
                    ("sealed".into(), Value::Bool(v.sealed)),
                ])
            })
            .collect();
        Value::Dict(vec![
            ("container_offset".into(), Value::from(self.container_offset)),
            ("block_size".into(), Value::from(self.block_size)),
            ("block_count".into(), Value::from(self.block_count)),
            ("container_uuid".into(), Value::str(&self.container_uuid)),
            ("volumes".into(), Value::List(volumes)),
        ])
    }
}

/// `apfs_info(path)`: container offset, block size, UUIDs and volumes of a raw APFS image (plain, or one APFS
/// partition in a GPT).
pub fn apfs_info(path: &str) -> Result<Info, Fail> {
    let f = File::open(path).map_err(|e| crash!("{}", crate::py::os_error(&e, path)))?;
    let size = f.metadata().map_err(|e| crash!("{path}: {e}"))?.len();
    let at = |offset: u128, count: usize| -> Result<Vec<u8>, Fail> {
        let offset = u64::try_from(offset).map_err(|_| crash!("{path}: an offset of {offset} is outside any file"))?;
        let mut data = vec![0u8; count];
        let mut done = 0;
        while done < count {
            match f.read_at(&mut data[done..], offset + done as u64) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(crash!("{path}: {e}")),
            }
        }
        if done != count {
            return Err(err!("{path}: short read at {offset}"));
        }
        Ok(data)
    };
    let bad = || outside(path);

    let mut head = at(0, 4096)?;
    let mut base: Option<u64> = Some(0);
    if &head[32..36] != b"NXSB" {
        base = None;
        for sector in [512u128, 4096] {
            let gpt = at(sector, 92)?;
            if &gpt[..8] != b"EFI PART" {
                continue;
            }
            let entries_lba = u64_at(&gpt, 72).ok_or_else(bad)? as u128;
            let count = u32_at(&gpt, 80).ok_or_else(bad)? as u128;
            let entry_size = u32_at(&gpt, 84).ok_or_else(bad)?;
            for i in 0..count.min(128) {
                let entry = at(entries_lba * sector + i * entry_size as u128, entry_size as usize)?;
                let first = u64_at(&entry, 32).ok_or_else(bad)? as u128;
                if entry.iter().take(16).any(|b| *b != 0) && &at(first * sector, 36)?[32..36] == b"NXSB" {
                    base = Some(u64::try_from(first * sector).map_err(|_| bad())?);
                    break;
                }
            }
            if base.is_some() {
                break;
            }
        }
        let Some(found) = base else {
            let tail = size.checked_sub(512).ok_or_else(|| crash!("{path}: too small to look for a UDIF trailer"))?;
            let kind = if at(tail as u128, 4)? == b"koly" { "a UDIF disk image (convert it to raw first)" } else { "not APFS" };
            return Err(err!("{path}: no APFS container found: {kind}"));
        };
        head = at(found as u128, 4096)?;
    }
    let base = base.unwrap();
    let block = u32_at(&head, 36).ok_or_else(bad)?;
    let blocks = u64_at(&head, 40).ok_or_else(bad)?;
    let container_uuid = uuid_text(&head[72..88]);
    let omap_oid = u64_at(&head, 160).ok_or_else(bad)?;
    let max_fs = u32_at(&head, 180).ok_or_else(bad)?;
    let fs_oids: Vec<u64> =
        (0..max_fs.min(100) as usize).map(|i| u64_at(&head, 184 + 8 * i).unwrap()).filter(|o| *o != 0).collect();
    let node = |paddr: u64| at(base as u128 + paddr as u128 * block as u128, block as usize);

    let omap = node(omap_oid)?;
    let root = u64_at(&omap, 48).ok_or_else(bad)?;

    // The newest mapping of oid in the container object map: its block address.
    let lookup = |mut paddr: u64, oid: u64| -> Result<Option<u64>, Fail> {
        for _ in 0..64 {
            let data = node(paddr)?;
            let flags = u16_at(&data, 32).ok_or_else(bad)?;
            let level = u16_at(&data, 34).ok_or_else(bad)?;
            let nkeys = u32_at(&data, 36).ok_or_else(bad)? as usize;
            let toc = 56 + u16_at(&data, 40).ok_or_else(bad)? as usize;
            let keys = toc + u16_at(&data, 42).ok_or_else(bad)? as usize;
            let vals = (block as usize).checked_sub(if flags & 1 != 0 { 40 } else { 0 }).ok_or_else(bad)?;
            let mut newest: Option<(u64, u64)> = None;
            let mut child: Option<u64> = None;
            for i in 0..nkeys {
                let (k, v) = if flags & 4 != 0 {
                    (u16_at(&data, toc + 4 * i).ok_or_else(bad)?, u16_at(&data, toc + 4 * i + 2).ok_or_else(bad)?)
                } else {
                    (u16_at(&data, toc + 8 * i).ok_or_else(bad)?, u16_at(&data, toc + 8 * i + 4).ok_or_else(bad)?)
                };
                let key_oid = u64_at(&data, keys + k as usize).ok_or_else(bad)?;
                let key_xid = u64_at(&data, keys + k as usize + 8).ok_or_else(bad)?;
                let value = vals.checked_sub(v as usize).ok_or_else(bad)?;
                if level == 0 {
                    if key_oid == oid && newest.is_none_or(|(xid, _)| key_xid > xid) {
                        newest = Some((key_xid, u64_at(&data, value + 8).ok_or_else(bad)?));
                    }
                } else if key_oid <= oid {
                    child = Some(u64_at(&data, value).ok_or_else(bad)?);
                }
            }
            if level == 0 {
                return Ok(newest.map(|(_, paddr)| paddr));
            }
            match child {
                Some(next) => paddr = next,
                None => return Ok(None),
            }
        }
        Err(crash!("{path}: the container's object map is deeper than 64 levels"))
    };

    let mut volumes = Vec::new();
    for oid in fs_oids {
        let Some(paddr) = lookup(root, oid)? else {
            return Err(err!("{path}: volume object {oid} is not in the container's object map"));
        };
        let sb = node(paddr)?;
        if sb.get(32..36) != Some(b"APSB") {
            return Err(err!("{path}: block {paddr} is not a volume superblock"));
        }
        let incompat = u64_at(&sb, 56).ok_or_else(bad)?;
        let role = u16_at(&sb, 964).ok_or_else(bad)?;
        let name = sb.get(704..960).ok_or_else(bad)?;
        let name = &name[..name.iter().position(|b| *b == 0).unwrap_or(name.len())];
        volumes.push(Volume {
            name: String::from_utf8_lossy(name).into_owned(),
            uuid: uuid_text(sb.get(240..256).ok_or_else(bad)?),
            role: role_name(role),
            case_sensitive: incompat & 1 == 0,
            sealed: incompat & 0x20 != 0,
        });
    }
    Ok(Info { container_offset: base, block_size: block, block_count: blocks, container_uuid, volumes })
}

/// One entry of a volume: the inode's owner and whole `st_mode`, and the sorted xattr names.
#[derive(Clone, Debug, PartialEq)]
pub struct Rec {
    pub u: u32,
    pub g: u32,
    pub m: u16,
    pub x: Vec<String>,
}

/// What `records` calls for each record: its type, key and value.
type Each<'a> = &'a mut dyn FnMut(u64, &[u8], &[u8]) -> Result<(), Fail>;

impl Rec {
    pub fn to_value(&self) -> Value {
        Value::Dict(vec![
            ("u".into(), Value::from(self.u)),
            ("g".into(), Value::from(self.g)),
            ("m".into(), Value::from(self.m)),
            ("x".into(), Value::List(self.x.iter().map(Value::str).collect())),
        ])
    }
}

struct Node<'a> {
    data: &'a [u8],
    flags: u16,
    level: u16,
    nkeys: usize,
    toc: usize,
    keys: usize,
    vals: usize,
}

/// What `entry(oid)` returns: the inode (owner, group, mode), the xattr names, the directory entries.
type Entry = ((u32, u32, u16), Vec<String>, Vec<(String, u64)>);

/// The owners, modes and xattr names of a volume's files, read from the APFS records of a raw image.
///
/// For macOS without root: an image a user attaches with `hdiutil attach -owners off` shows every file as the
/// user's own, so the real owners come from here. Reads the inode, directory and xattr records of the
/// file-system tree (physical, as on a sealed volume, or virtual through the volume's object map); no file
/// data. The first volume only, unencrypted. tree_copy checks it against the mounted volume entry by entry.
pub struct ApfsTree {
    path: String,
    map: Map,
    base: u64,
    block: usize,
    vol_omap: u64,
    physical: bool,
    root: u64,
    child_base: u64,
}

const OBJ_PHYSICAL: u32 = 0x4000_0000;
const TYPE_INODE: u64 = 3;
const TYPE_XATTR: u64 = 4;
const TYPE_DIR_REC: u64 = 9;
const ROOT_DIR: u64 = 2;

impl ApfsTree {
    pub fn open(path: &str) -> Result<ApfsTree, Fail> {
        let volume = 0usize;
        let info = apfs_info(path)?;
        let map = Map::open(path).map_err(|e| crash!("{e:#}"))?;
        let mut tree = ApfsTree {
            path: path.to_string(),
            map,
            base: info.container_offset,
            block: info.block_size as usize,
            vol_omap: 0,
            physical: false,
            root: 0,
            child_base: 0,
        };
        let bad = || outside(path);
        let head = tree.read(0)?;
        let omap = tree.read(u64_at(head, 160).ok_or_else(bad)?)?;
        let mut fs_oids = Vec::new();
        for i in 0..100 {
            let oid = u64_at(head, 184 + 8 * i).ok_or_else(bad)?;
            if oid != 0 {
                fs_oids.push(oid);
            }
        }
        // Python: fs_oids[volume] of an empty list is an IndexError, which prints the usage.
        let first = *fs_oids.get(volume).ok_or(Fail::Usage)?;
        let sb = tree.read(tree.omap_lookup(u64_at(omap, 48).ok_or_else(bad)?, first)?)?;
        if sb.get(32..36) != Some(b"APSB") {
            return Err(err!("{path}: no volume superblock for volume {volume}"));
        }
        if u64_at(sb, 0x108).ok_or_else(bad)? & 0x1 == 0 {
            return Err(err!("{path}: volume {volume} is encrypted"));
        }
        let vol_omap = u64_at(tree.read(u64_at(sb, 0x80).ok_or_else(bad)?)?, 48).ok_or_else(bad)?;
        let physical = u32_at(sb, 0x74).ok_or_else(bad)? & OBJ_PHYSICAL != 0;
        let root = u64_at(sb, 0x88).ok_or_else(bad)?;
        let root_block = if physical { root } else { tree.omap_lookup(vol_omap, root)? };
        // In the hashed tree of a sealed volume (btree_info_t's BTREE_HASHED) an index entry's child is a number
        // relative to the root's virtual object id (seen in 24A446's images: root 1158, children 1158 + 1..122).
        let info_at = tree.block.checked_sub(40).ok_or_else(bad)?;
        let hashed = u32_at(tree.read(root_block)?, info_at).ok_or_else(bad)? & 0x80 != 0;
        tree.vol_omap = vol_omap;
        tree.physical = physical;
        tree.root = root_block;
        tree.child_base = if hashed && !physical { root } else { 0 };
        Ok(tree)
    }

    fn bad(&self) -> Fail {
        outside(&self.path)
    }

    fn read(&self, paddr: u64) -> Result<&[u8], Fail> {
        let start = (self.base as u128) + (paddr as u128) * (self.block as u128);
        let end = start + self.block as u128;
        if end > self.map.len() as u128 {
            return Err(err!("{}: short read at block {paddr}", self.path));
        }
        Ok(&self.map[start as usize..end as usize])
    }

    fn node(&self, paddr: u64) -> Result<Node<'_>, Fail> {
        let data = self.read(paddr)?;
        let flags = u16_at(data, 32).ok_or_else(|| self.bad())?;
        let level = u16_at(data, 34).ok_or_else(|| self.bad())?;
        let nkeys = u32_at(data, 36).ok_or_else(|| self.bad())? as usize;
        let toc = 56 + u16_at(data, 40).ok_or_else(|| self.bad())? as usize;
        let keys = toc + u16_at(data, 42).ok_or_else(|| self.bad())? as usize;
        let vals = self.block.checked_sub(if flags & 1 != 0 { 40 } else { 0 }).ok_or_else(|| self.bad())?;
        Ok(Node { data, flags, level, nkeys, toc, keys, vals })
    }

    /// The block of the newest mapping of oid in the object map tree rooted at paddr.
    fn omap_lookup(&self, mut paddr: u64, oid: u64) -> Result<u64, Fail> {
        let bad = || self.bad();
        for _ in 0..64 {
            let n = self.node(paddr)?;
            let mut newest: Option<(u64, u64)> = None;
            let mut child: Option<u64> = None;
            for i in 0..n.nkeys {
                let k = u16_at(n.data, n.toc + 4 * i).ok_or_else(bad)? as usize;
                let v = u16_at(n.data, n.toc + 4 * i + 2).ok_or_else(bad)? as usize;
                let key_oid = u64_at(n.data, n.keys + k).ok_or_else(bad)?;
                let key_xid = u64_at(n.data, n.keys + k + 8).ok_or_else(bad)?;
                let value = n.vals.checked_sub(v).ok_or_else(bad)?;
                if n.level == 0 {
                    if key_oid == oid && newest.is_none_or(|(xid, _)| key_xid > xid) {
                        newest = Some((key_xid, u64_at(n.data, value + 8).ok_or_else(bad)?));
                    }
                } else if key_oid <= oid {
                    child = Some(u64_at(n.data, value).ok_or_else(bad)?);
                }
            }
            let found = if n.level == 0 { newest.map(|(_, paddr)| paddr) } else { child };
            let Some(found) = found else {
                return Err(err!("{}: object {oid} is not in the object map", self.path));
            };
            if n.level == 0 {
                return Ok(found);
            }
            paddr = found;
        }
        Err(crash!("{}: an object map is deeper than 64 levels", self.path))
    }

    /// Calls `each(type, key, value)` for every file-system record of object oid, in key order.
    fn records(
        &self,
        oid: u64,
        paddr: u64,
        depth: u32,
        each: Each,
    ) -> Result<(), Fail> {
        if depth > 64 {
            return Err(crash!("{}: the file-system tree is deeper than 64 levels", self.path));
        }
        let bad = || self.bad();
        let n = self.node(paddr)?;
        if n.flags & 4 != 0 {
            return Err(err!("{}: a file-system tree node with fixed-size entries", self.path));
        }
        const MASK: u64 = (1 << 60) - 1;
        let mut children: Vec<(u64, u64)> = Vec::new();
        for i in 0..n.nkeys {
            let at = n.toc + 8 * i;
            let k_off = u16_at(n.data, at).ok_or_else(bad)? as usize;
            let k_len = u16_at(n.data, at + 2).ok_or_else(bad)? as usize;
            let v_off = u16_at(n.data, at + 4).ok_or_else(bad)? as usize;
            let v_len = u16_at(n.data, at + 6).ok_or_else(bad)? as usize;
            // Python slices: an end past the block is cut at the block's end.
            let k_start = (n.keys + k_off).min(n.data.len());
            let key = &n.data[k_start..(k_start + k_len).min(n.data.len())];
            let hdr = u64_at(key, 0).ok_or_else(bad)?;
            let v_start = n.vals.checked_sub(v_off).ok_or_else(bad)?;
            if n.level == 0 {
                if hdr & MASK == oid {
                    each(hdr >> 60, key, &n.data[v_start..(v_start + v_len).min(n.data.len())])?;
                }
            } else {
                // the child's object id (a hashed node's value has the child's hash after it)
                children.push((hdr & MASK, u64_at(n.data, v_start).ok_or_else(bad)?));
            }
        }
        for (i, (first, child)) in children.iter().enumerate() {
            if *first > oid {
                break;
            }
            if i + 1 < children.len() && children[i + 1].0 < oid {
                continue;
            }
            let at = if self.physical { *child } else { self.omap_lookup(self.vol_omap, self.child_base.wrapping_add(*child))? };
            if u16_at(self.read(at)?, 34).ok_or_else(bad)? != n.level - 1 {
                return Err(err!("{}: the file-system tree's node at block {at} is not on level {}", self.path, n.level - 1));
            }
            self.records(oid, at, depth + 1, each)?;
        }
        Ok(())
    }

    fn name(&self, raw: &[u8]) -> Result<String, Fail> {
        let end = raw.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
        String::from_utf8(raw[..end].to_vec()).map_err(|_| {
            crash!(
                "{}: a name in the volume's records is not UTF-8 ({}); this tool reads UTF-8 names only, use scripts/image/imagetool.py",
                self.path,
                String::from_utf8_lossy(&raw[..end])
            )
        })
    }

    /// The inode of oid, its xattr names and its directory entries.
    fn entry(&self, oid: u64) -> Result<Entry, Fail> {
        let mut inode = None;
        let mut xattrs = Vec::new();
        let mut entries = Vec::new();
        let bad = || self.bad();
        self.records(oid, self.root, 0, &mut |kind, key, val| {
            if kind == TYPE_INODE {
                inode = Some((
                    u32_at(val, 72).ok_or_else(bad)?,
                    u32_at(val, 76).ok_or_else(bad)?,
                    u16_at(val, 80).ok_or_else(bad)?,
                ));
            } else if kind == TYPE_XATTR {
                let n = u16_at(key, 8).ok_or_else(bad)? as usize;
                let start = 10.min(key.len());
                xattrs.push(self.name(&key[start..(10 + n).min(key.len()).max(start)])?);
            } else if kind == TYPE_DIR_REC {
                let name = if (u32_at(key, 8).ok_or_else(bad)? & 0x3FF) as i64 == key.len() as i64 - 12 {
                    &key[12..] // j_drec_hashed_key_t
                } else if u16_at(key, 8).ok_or_else(bad)? as i64 == key.len() as i64 - 10 {
                    &key[10..] // j_drec_key_t
                } else {
                    return Err(err!("{}: a directory record of object {oid} with an unknown key layout", self.path));
                };
                entries.push((self.name(name)?, u64_at(val, 0).ok_or_else(bad)?));
            }
            Ok(())
        })?;
        let Some(inode) = inode else {
            return Err(err!("{}: object {oid} has no inode record", self.path));
        };
        Ok((inode, xattrs, entries))
    }

    /// `walk(subdir)`: (path relative to subdir, '.' for subdir itself; its record), in the order Python's dict
    /// has them. A path that is there twice (not seen in a real volume) is listed twice: `index` keeps the last.
    ///
    /// The tree is walked level by level, the entries of a level in parallel. The result and the error (the
    /// first one Python's walk would meet) do not depend on the threads.
    pub fn walk(&self, subdir: &str) -> Result<Vec<(String, Rec)>, Fail> {
        let mut oid = ROOT_DIR;
        for part in subdir.split('/').filter(|p| !p.is_empty()) {
            let (_, _, entries) = self.entry(oid)?;
            match entries.iter().rev().find(|(name, _)| name == part) {
                Some((_, child)) => oid = *child,
                None => return Err(err!("{}: no /{subdir} in the volume", self.path)),
            }
        }
        struct Item {
            rel: String,
            oid: u64,
            rec: Option<Result<Rec, Fail>>,
            children: Vec<usize>,
        }
        let mut items = vec![Item { rel: ".".into(), oid, rec: None, children: Vec::new() }];
        let mut level: Vec<usize> = vec![0];
        while !level.is_empty() {
            let found: Vec<Result<Entry, Fail>> = level.par_iter().map(|i| self.entry(items[*i].oid)).collect();
            let mut next = Vec::new();
            for (i, entry) in level.iter().zip(found) {
                match entry {
                    Ok(((u, g, m), mut x, entries)) => {
                        x.sort();
                        items[*i].rec = Some(Ok(Rec { u, g, m, x }));
                        for (name, child) in entries {
                            let rel = if items[*i].rel == "." { name } else { format!("{}/{name}", items[*i].rel) };
                            let at = items.len();
                            items.push(Item { rel, oid: child, rec: None, children: Vec::new() });
                            items[*i].children.push(at);
                            next.push(at);
                        }
                    }
                    Err(e) => items[*i].rec = Some(Err(e)),
                }
            }
            level = next;
        }
        // Python's order: a stack, so an entry's last child comes right after it.
        let mut out = Vec::with_capacity(items.len());
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let rec = items[i].rec.take().expect("every entry was read")?;
            out.push((std::mem::take(&mut items[i].rel), rec));
            stack.extend(items[i].children.iter().copied());
        }
        Ok(out)
    }
}

/// `{path: record}` of a walk, as Python's dict has it (the last of two entries with one path).
pub fn index(walked: &[(String, Rec)]) -> HashMap<&str, &Rec> {
    walked.iter().map(|(rel, rec)| (rel.as_str(), rec)).collect()
}

/// The walk as the dict `apfs-owners` prints.
pub fn owners_value(walked: Vec<(String, Rec)>) -> Value {
    let mut place: HashMap<String, usize> = HashMap::with_capacity(walked.len());
    let mut out: Vec<(String, Value)> = Vec::with_capacity(walked.len());
    for (rel, rec) in walked {
        match place.get(&rel) {
            Some(&at) => out[at].1 = rec.to_value(),
            None => {
                place.insert(rel.clone(), out.len());
                out.push((rel, rec.to_value()));
            }
        }
    }
    Value::Dict(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{fake_apfs, fake_apfs_tree, Tmp};

    #[test]
    fn info_of_a_plain_image() {
        let tmp = Tmp::new("apfs-info");
        let image = tmp.path("os.img");
        let want = fake_apfs(&image, "FakeOS", 1, 64, b"");
        let info = apfs_info(&image).unwrap();
        assert_eq!((info.container_offset, info.block_size, info.block_count), (0, 4096, 64));
        assert_eq!(info.volumes.len(), 1);
        let v = &info.volumes[0];
        assert_eq!((v.name.as_str(), v.uuid.as_str(), v.role.as_str(), v.case_sensitive, v.sealed), ("FakeOS", want.as_str(), "system", true, true));
        let text = oneios_common::pyjson::dumps(&info.to_value(), Some(2), false);
        assert!(text.starts_with("{\n  \"container_offset\": 0,\n  \"block_size\": 4096,\n  \"block_count\": 64,\n  \"container_uuid\": \""), "{text}");
        assert!(text.ends_with("\"role\": \"system\",\n      \"case_sensitive\": true,\n      \"sealed\": true\n    }\n  ]\n}"), "{text}");

        let mut junk = vec![0u8; 8192];
        junk.extend_from_slice(b"koly");
        junk.extend_from_slice(&[0u8; 508]);
        let junk = tmp.write("junk.img", &junk);
        match apfs_info(&junk) {
            Err(Fail::Error(m)) => assert!(m.ends_with("no APFS container found: a UDIF disk image (convert it to raw first)"), "{m}"),
            _ => panic!("junk was read as APFS"),
        }
        let zeros = tmp.write("zeros.img", &vec![0u8; 16384]);
        assert!(matches!(apfs_info(&zeros), Err(Fail::Error(m)) if m.ends_with("no APFS container found: not APFS")));
        let short = tmp.write("short.img", b"abc");
        assert!(matches!(apfs_info(&short), Err(Fail::Error(m)) if m.ends_with("short read at 0")));
    }

    #[test]
    fn info_inside_a_gpt() {
        let tmp = Tmp::new("apfs-gpt");
        let plain = tmp.path("plain.img");
        let want = fake_apfs(&plain, "InGpt", 0x40, 16, b"");
        let inner = std::fs::read(&plain).unwrap();
        // sector 512: the header at LBA 1, the entries at LBA 2, the container at LBA 64
        let mut disk = vec![0u8; 64 * 512];
        disk[512..520].copy_from_slice(b"EFI PART");
        disk[512 + 72..512 + 80].copy_from_slice(&2u64.to_le_bytes());
        disk[512 + 80..512 + 84].copy_from_slice(&4u32.to_le_bytes());
        disk[512 + 84..512 + 88].copy_from_slice(&128u32.to_le_bytes());
        let entry = 2 * 512 + 128; // the second entry; the first is empty
        disk[entry] = 0xEF;
        disk[entry + 32..entry + 40].copy_from_slice(&64u64.to_le_bytes());
        disk.extend_from_slice(&inner);
        let image = tmp.write("gpt.img", &disk);
        let info = apfs_info(&image).unwrap();
        assert_eq!(info.container_offset, 64 * 512);
        assert_eq!((info.volumes[0].uuid.as_str(), info.volumes[0].role.as_str()), (want.as_str(), "data"));
    }

    fn entries() -> Vec<(&'static str, u16, u32, u32, Vec<&'static str>)> {
        vec![
            ("private", 0o40755, 0, 0, vec![]),
            ("private/var", 0o40755, 0, 0, vec![]),
            ("private/var/mobile", 0o40750, 501, 501, vec![]),
            ("private/var/mobile/a.plist", 0o100600, 501, 501, vec!["com.example.tag", "com.apple.decmpfs"]),
            ("private/var/db", 0o40700, 0, 0, vec![]),
            ("private/var/db/l", 0o120755, 0, 80, vec![]),
        ]
    }

    #[test]
    fn tree_walk() {
        let tmp = Tmp::new("apfs-tree");
        for hashed in [true, false] {
            let image = tmp.path(&format!("tree-{hashed}.img"));
            fake_apfs_tree(&image, &entries(), hashed);
            let tree = ApfsTree::open(&image).unwrap();
            let got = tree.walk("private/var").unwrap();
            // Python's dict order: the last child first
            let order: Vec<&str> = got.iter().map(|(rel, _)| rel.as_str()).collect();
            assert_eq!(order, [".", "mobile", "mobile/a.plist", "db", "db/l"], "hashed {hashed}");
            let by = index(&got);
            assert_eq!(
                by["mobile/a.plist"],
                &Rec { u: 501, g: 501, m: 0o100600, x: vec!["com.apple.decmpfs".into(), "com.example.tag".into()] }
            );
            assert_eq!((by["db/l"].g, by["mobile"].m), (80, 0o40750));
            let text = oneios_common::pyjson::dumps(&owners_value(got), None, true);
            assert_eq!(
                text,
                "{\".\": {\"g\": 0, \"m\": 16877, \"u\": 0, \"x\": []}, \"db\": {\"g\": 0, \"m\": 16832, \"u\": 0, \"x\": []}, \
                 \"db/l\": {\"g\": 80, \"m\": 41453, \"u\": 0, \"x\": []}, \"mobile\": {\"g\": 501, \"m\": 16872, \"u\": 501, \"x\": []}, \
                 \"mobile/a.plist\": {\"g\": 501, \"m\": 33152, \"u\": 501, \"x\": [\"com.apple.decmpfs\", \"com.example.tag\"]}}"
            );
            assert_eq!(tree.walk("").unwrap().len(), 7);
            match tree.walk("private/nothing") {
                Err(Fail::Error(m)) => assert!(m.ends_with("no /private/nothing in the volume"), "{m}"),
                _ => panic!("a missing folder was walked"),
            }
        }
    }

    #[test]
    fn tree_refuses() {
        let tmp = Tmp::new("apfs-refuse");
        let image = tmp.path("tree.img");
        fake_apfs_tree(&image, &entries(), true);
        let good = std::fs::read(&image).unwrap();
        // an encrypted volume
        let mut data = good.clone();
        data[3 * 4096 + 0x108] = 0;
        let path = tmp.write("encrypted.img", &data);
        assert!(matches!(ApfsTree::open(&path), Err(Fail::Error(m)) if m.ends_with("volume 0 is encrypted")));
        // the root node of the file-system tree is not in the volume's object map
        let mut data = good.clone();
        data[3 * 4096 + 0x88..3 * 4096 + 0x90].copy_from_slice(&7777u64.to_le_bytes());
        let path = tmp.write("no-root.img", &data);
        assert!(matches!(ApfsTree::open(&path), Err(Fail::Error(m)) if m.ends_with("object 7777 is not in the object map")));
        // a leaf that claims to be an index node
        let mut data = good.clone();
        data[7 * 4096 + 34] = 3;
        let path = tmp.write("level.img", &data);
        let tree = ApfsTree::open(&path).unwrap();
        assert!(matches!(tree.walk(""), Err(Fail::Error(m)) if m.ends_with("the file-system tree's node at block 7 is not on level 0")));
        // an image without a file-system tree at all
        let plain = tmp.path("plain.img");
        fake_apfs(&plain, "Plain", 1, 16, b"");
        assert!(ApfsTree::open(&plain).is_err());
    }
}
