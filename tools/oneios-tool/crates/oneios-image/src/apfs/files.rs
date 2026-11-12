//! File contents and directory listings of a volume, for tools that read an image without mounting it
//! (`oneios-tool ioslog`, `image apfs-ls`, `image apfs-copy`). imagetool.py has no such part: its `ApfsTree`
//! reads owners only. Layouts: Apple's "Apple File System Reference" (j_inode_val_t, j_drec_val_t,
//! j_file_extent_*, fext_tree_*, nx_superblock_t's checkpoint descriptor area).
//!
//! Hand-written because no maintained Rust crate reads iOS's sealed volumes or any volume but the first
//! (tools/oneios-tool/README.md "APFS").

use super::*;

/// APFS's checksum of a block: Fletcher 64 over the 32-bit words after the checksum field.
pub(super) fn fletcher64(block: &[u8]) -> u64 {
    let (mut a, mut b) = (0u64, 0u64);
    for (i, word) in block[8..].as_chunks::<4>().0.iter().enumerate() {
        a += u32::from_le_bytes(*word) as u64;
        b += a;
        // reduced in runs: 1024 additions of 32-bit words cannot overflow 64 bits
        if i % 1024 == 1023 {
            a %= 0xFFFF_FFFF;
            b %= 0xFFFF_FFFF;
        }
    }
    a %= 0xFFFF_FFFF;
    b %= 0xFFFF_FFFF;
    let low = 0xFFFF_FFFF - (a + b) % 0xFFFF_FFFF;
    let high = 0xFFFF_FFFF - (a + low) % 0xFFFF_FFFF;
    high << 32 | low
}

/// One name in a directory. `kind` is the record's DT_ type.
#[derive(Clone, Debug, PartialEq)]
pub struct DirEnt {
    pub name: String,
    pub id: u64,
    pub kind: u16,
}

/// An inode: what a copy of a file needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Stat {
    pub id: u64,
    /// The object the file's extents are recorded under.
    pub private_id: u64,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub bsd_flags: u32,
    /// The data stream's size (0 when the inode has none).
    pub size: u64,
    /// Modification time, nanoseconds since 1970.
    pub mtime: u64,
}

pub const DT_DIR: u16 = 4;
pub const DT_REG: u16 = 8;
pub const DT_LNK: u16 = 10;
const TYPE_FILE_EXTENT: u64 = 8;
const UF_COMPRESSED: u32 = 0x20;
const LENGTH_MASK: u64 = 0x00FF_FFFF_FFFF_FFFF;

impl ApfsTree {
    /// How many volumes the container has (as block 0 lists them).
    pub fn volume_count(path: &str) -> Result<usize, Fail> {
        Ok(apfs_info(path)?.volumes.len())
    }

    /// The block and transaction of the newest container superblock with a good checksum, from the checkpoint
    /// descriptor area; block 0's own when the area is not a plain run of blocks or holds no newer one.
    pub(super) fn newest_superblock(&self) -> Result<(u64, u64), Fail> {
        let bad = || self.bad();
        let head = self.read(0)?;
        let mut best = (0u64, u64_at(head, 16).ok_or_else(bad)?);
        let count = u32_at(head, 104).ok_or_else(bad)?;
        let first = u64_at(head, 112).ok_or_else(bad)?;
        if count & 0x8000_0000 != 0 {
            return Ok(best);
        }
        for i in 0..count as u64 {
            let Ok(block) = self.read(first + i) else { break };
            if block.get(32..36) != Some(b"NXSB") || u32_at(block, 24).ok_or_else(bad)? & 0xFFFF != 1 {
                continue;
            }
            let xid = u64_at(block, 16).ok_or_else(bad)?;
            if xid >= best.1 && fletcher64(block) == u64_at(block, 0).ok_or_else(bad)? {
                best = (first + i, xid);
            }
        }
        Ok(best)
    }

    /// The entries of directory `oid`, in the order of its records (by name hash on a volume with hashed names).
    pub fn list(&self, oid: u64) -> Result<Vec<DirEnt>, Fail> {
        let mut out = Vec::new();
        let bad = || self.bad();
        self.records(oid, self.root, 0, &mut |kind, key, val| {
            if kind == TYPE_DIR_REC {
                let name = if (u32_at(key, 8).ok_or_else(bad)? & 0x3FF) as i64 == key.len() as i64 - 12 {
                    &key[12..]
                } else if u16_at(key, 8).ok_or_else(bad)? as i64 == key.len() as i64 - 10 {
                    &key[10..]
                } else {
                    return Err(err!("{}: a directory record of object {oid} with an unknown key layout", self.path));
                };
                out.push(DirEnt {
                    name: self.name(name)?,
                    id: u64_at(val, 0).ok_or_else(bad)?,
                    kind: u16_at(val, 16).unwrap_or(0) & 0xF,
                });
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// The object of `path` below the volume's root (`None`: no such entry). Names are compared byte for byte.
    pub fn find(&self, path: &str) -> Result<Option<u64>, Fail> {
        let mut oid = ROOT_DIR;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            match self.list(oid)?.into_iter().rev().find(|e| e.name == part) {
                Some(e) => oid = e.id,
                None => return Ok(None),
            }
        }
        Ok(Some(oid))
    }

    pub fn stat(&self, oid: u64) -> Result<Stat, Fail> {
        let mut found: Option<Stat> = None;
        // a compressed file's size is in its decmpfs attribute (its inode has no data stream)
        let mut plain_size = None;
        let bad = || self.bad();
        self.records(oid, self.root, 0, &mut |kind, key, val| {
            if kind == TYPE_INODE {
                let mut size = 0;
                // The extended fields: a count, the bytes used, a table of (type, flags, size), then the values,
                // each padded to 8 bytes. Type 8 is the data stream (its first field: the size).
                if let Some(count) = u16_at(val, 92) {
                    let mut at = 96 + 4 * count as usize;
                    for i in 0..count as usize {
                        let field = *val.get(96 + 4 * i).ok_or_else(bad)?;
                        let len = u16_at(val, 96 + 4 * i + 2).ok_or_else(bad)? as usize;
                        if field == 8 {
                            size = u64_at(val, at).ok_or_else(bad)?;
                        }
                        at += (len + 7) & !7;
                    }
                }
                found = Some(Stat {
                    id: oid,
                    private_id: u64_at(val, 8).ok_or_else(bad)?,
                    mode: u16_at(val, 80).ok_or_else(bad)?,
                    uid: u32_at(val, 72).ok_or_else(bad)?,
                    gid: u32_at(val, 76).ok_or_else(bad)?,
                    bsd_flags: u32_at(val, 68).ok_or_else(bad)?,
                    size,
                    mtime: u64_at(val, 24).ok_or_else(bad)?,
                });
            } else if kind == TYPE_XATTR && self.xattr_name(key)? == "com.apple.decmpfs" {
                if let Some(value) = self.xattr_value(val)? {
                    plain_size = decmpfs::header(&value).map(|(_, size)| size);
                }
            }
            Ok(())
        })?;
        let mut st = found.ok_or_else(|| err!("{}: object {oid} has no inode record", self.path))?;
        if let (true, Some(size)) = (st.bsd_flags & UF_COMPRESSED != 0, plain_size) {
            st.size = size;
        }
        Ok(st)
    }

    fn xattr_name(&self, key: &[u8]) -> Result<String, Fail> {
        let n = u16_at(key, 8).ok_or_else(|| self.bad())? as usize;
        let start = 10.min(key.len());
        self.name(&key[start..(10 + n).min(key.len()).max(start)])
    }

    /// An attribute record's value: in the record itself (flag 2), or in a data stream of its own (flag 1: the
    /// stream's id and size), as a resource fork is.
    fn xattr_value(&self, val: &[u8]) -> Result<Option<Vec<u8>>, Fail> {
        let bad = || self.bad();
        let flags = u16_at(val, 0).ok_or_else(bad)?;
        if flags & 2 != 0 {
            let len = u16_at(val, 2).ok_or_else(bad)? as usize;
            Ok(Some(val.get(4..4 + len).ok_or_else(bad)?.to_vec()))
        } else if flags & 1 != 0 {
            Ok(Some(self.read_stream(u64_at(val, 4).ok_or_else(bad)?, u64_at(val, 12).ok_or_else(bad)?)?))
        } else {
            Ok(None)
        }
    }

    /// The value of extended attribute `name` of `oid` (`None`: no such attribute). A symbolic link's target is
    /// `com.apple.fs.symlink`.
    pub fn xattr(&self, oid: u64, name: &str) -> Result<Option<Vec<u8>>, Fail> {
        let mut found = None;
        self.records(oid, self.root, 0, &mut |kind, key, val| {
            if kind == TYPE_XATTR && found.is_none() && self.xattr_name(key)? == name {
                found = self.xattr_value(val)?;
            }
            Ok(())
        })?;
        Ok(found)
    }

    /// The extents of data stream `id`: (offset in the file, length, first block; block 0 is a hole).
    fn extents(&self, id: u64) -> Result<Vec<(u64, u64, u64)>, Fail> {
        let mut out = Vec::new();
        let bad = || self.bad();
        if self.fext_root != 0 {
            self.fext(id, self.fext_root, 0, &mut out)?;
            return Ok(out);
        }
        self.records(id, self.root, 0, &mut |kind, key, val| {
            if kind == TYPE_FILE_EXTENT {
                out.push((
                    u64_at(key, 8).ok_or_else(bad)?,
                    u64_at(val, 0).ok_or_else(bad)? & LENGTH_MASK,
                    u64_at(val, 8).ok_or_else(bad)?,
                ));
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// The extents of `id` in a sealed volume's extent tree: physical nodes, keys (stream, offset), values
    /// (length, block).
    fn fext(&self, id: u64, paddr: u64, depth: u32, out: &mut Vec<(u64, u64, u64)>) -> Result<(), Fail> {
        if depth > 64 {
            return Err(crash!("{}: the extent tree is deeper than 64 levels", self.path));
        }
        let bad = || self.bad();
        let n = self.node(paddr)?;
        let mut children: Vec<(u64, u64)> = Vec::new();
        for i in 0..n.nkeys {
            let (k, v) = if n.flags & 4 != 0 {
                (u16_at(n.data, n.toc + 4 * i).ok_or_else(bad)?, u16_at(n.data, n.toc + 4 * i + 2).ok_or_else(bad)?)
            } else {
                (u16_at(n.data, n.toc + 8 * i).ok_or_else(bad)?, u16_at(n.data, n.toc + 8 * i + 4).ok_or_else(bad)?)
            };
            let key = n.keys + k as usize;
            let stream = u64_at(n.data, key).ok_or_else(bad)?;
            let value = n.vals.checked_sub(v as usize).ok_or_else(bad)?;
            if n.level != 0 {
                children.push((stream, u64_at(n.data, value).ok_or_else(bad)?));
            } else if stream == id {
                out.push((
                    u64_at(n.data, key + 8).ok_or_else(bad)?,
                    u64_at(n.data, value).ok_or_else(bad)? & LENGTH_MASK,
                    u64_at(n.data, value + 8).ok_or_else(bad)?,
                ));
            }
        }
        for (i, (first, child)) in children.iter().enumerate() {
            if *first > id {
                break;
            }
            if i + 1 < children.len() && children[i + 1].0 < id {
                continue;
            }
            self.fext(id, *child, depth + 1, out)?;
        }
        Ok(())
    }

    /// The whole contents of file `oid`; a file kept compressed (decmpfs) is decoded.
    pub fn read_file(&self, oid: u64) -> Result<Vec<u8>, Fail> {
        let st = self.stat(oid)?;
        if st.bsd_flags & UF_COMPRESSED != 0 {
            let attr = self.xattr(oid, "com.apple.decmpfs")?.ok_or_else(|| err!("{}: object {oid} is marked compressed and has no com.apple.decmpfs attribute", self.path))?;
            let fork = || match self.xattr(oid, "com.apple.ResourceFork") {
                Ok(Some(fork)) => Ok(fork),
                Ok(None) => Err("no resource fork".to_string()),
                Err(e) => Err(e.to_string()),
            };
            return decmpfs::decode(&attr, fork).map_err(|e| err!("{}: object {oid}, a compressed file (decmpfs): {e}", self.path));
        }
        self.read_stream(st.private_id, st.size)
    }

    /// `size` bytes of data stream `id`.
    fn read_stream(&self, id: u64, size: u64) -> Result<Vec<u8>, Fail> {
        if size > self.map.len() as u64 {
            return Err(err!("{}: stream {id} says it has {size} bytes, more than the image", self.path));
        }
        let mut data = vec![0u8; size as usize];
        for (offset, len, block) in self.extents(id)? {
            if block == 0 || offset >= size {
                continue;
            }
            let len = len.min(size - offset) as usize;
            let start = self.base as u128 + block as u128 * self.block as u128;
            let end = start + len as u128;
            if end > self.map.len() as u128 {
                return Err(err!("{}: short read at block {block}", self.path));
            }
            data[offset as usize..offset as usize + len].copy_from_slice(&self.map[start as usize..end as usize]);
        }
        Ok(data)
    }

    /// Whether `oid`'s contents are kept compressed (decmpfs); `read_file` decodes them.
    pub fn is_compressed(st: &Stat) -> bool {
        st.bsd_flags & UF_COMPRESSED != 0
    }
}
