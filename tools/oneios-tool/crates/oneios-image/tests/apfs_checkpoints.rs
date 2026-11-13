//! The APFS reader on stores whose newest checkpoint was cut off in the middle of a write (`apfs/check.rs`): the
//! checks before a read, and the fall back to the newest checkpoint that passes. The stores: two of
//! tests/ioslog/mkapfs.py's grafted into one container with two checkpoints and then damaged
//! (tests/support/apfs_store.rs). No Apple file, no owner data. Needs python3 (mkapfs.py); skipped with a line on
//! stderr where there is none.

#[path = "../../../tests/support/apfs_store.rs"]
mod store;

use oneios_image::apfs::{ApfsTree, Container, Pick, DT_DIR, DT_LNK, DT_REG};
use oneios_image::Fail;
use std::collections::BTreeMap;
use std::path::Path;
use store::{Layout, Store};

/// Every file of a folder: path -> contents (a link: its target).
fn folder(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            let rel = path.strip_prefix(root).unwrap().to_str().unwrap().to_string();
            let kind = std::fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_dir() {
                walk(root, &path, out);
            } else if kind.is_symlink() {
                out.insert(rel, std::fs::read_link(&path).unwrap().to_str().unwrap().as_bytes().to_vec());
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Every file of a volume, as `folder` gives a folder's.
fn volume(tree: &ApfsTree) -> Result<BTreeMap<String, Vec<u8>>, Fail> {
    fn walk(tree: &ApfsTree, dir: u64, rel: &str, out: &mut BTreeMap<String, Vec<u8>>) -> Result<(), Fail> {
        for e in tree.list(dir)? {
            let path = if rel.is_empty() { e.name.clone() } else { format!("{rel}/{}", e.name) };
            match e.kind {
                DT_DIR => walk(tree, e.id, &path, out)?,
                DT_REG => drop(out.insert(path, tree.read_file(e.id)?)),
                DT_LNK => {
                    let target = tree.xattr(e.id, "com.apple.fs.symlink")?.unwrap_or_default();
                    out.insert(path, target[..target.len().saturating_sub(1)].to_vec());
                }
                _ => {}
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(tree, 2, "", &mut out)?;
    Ok(out)
}

fn text(e: Fail) -> String {
    let text = e.to_string();
    assert!(!text.contains("short read"), "a bare short read: {text}");
    text
}

struct Fixture {
    dir: std::path::PathBuf,
    good: Store,
    old: Layout,
    new: Layout,
    /// The Data volume's files at transaction 5 and at transaction 6.
    older: BTreeMap<String, Vec<u8>>,
    newer: BTreeMap<String, Vec<u8>>,
    count: usize,
}

impl Fixture {
    fn new(name: &str) -> Option<Fixture> {
        let manifest = env!("CARGO_MANIFEST_DIR");
        if !store::can_make(manifest) {
            return None;
        }
        let dir = store::scratch(name);
        let (a, b) = (dir.join("older"), dir.join("newer"));
        for (root, grown) in [(&a, false), (&b, true)] {
            store::put(root, "db/diagnostics/Persist/0000000000000001.tracev3", &store::pattern(if grown { 9000 } else { 5000 }, 1));
            store::put(root, "db/diagnostics/Special/0000000000000002.tracev3", &store::pattern(200_000, 2)); // two extents
            store::put(root, "db/diagnostics/timesync/0000000000000001.timesync", b"timesync");
            store::put(root, "db/uuidtext/AB/CDEF0123456789ABCDEF0123456789", b"uuidtext");
            std::os::unix::fs::symlink("diagnostics/Persist", root.join("db/link")).unwrap();
            for i in 0..300 {
                // enough records for several leaves and an index node
                store::put(root, &format!("mobile/f{i:03}"), format!("file {i}").as_bytes());
            }
            if grown {
                store::put(root, "db/diagnostics/Persist/0000000000000003.tracev3", &store::pattern(70_000, 3));
            }
        }
        let (good, old, new) = store::graft(&store::make(manifest, &dir, "older", &a), &store::make(manifest, &dir, "newer", &b));
        assert!(new.volumes[2].nodes.iter().any(|(_, level)| *level > 0), "the Data volume's tree has an index node");
        Some(Fixture { older: folder(&a), newer: folder(&b), dir, good, old, new, count: 0 })
    }

    /// The store after `damage`, written to a file of its own.
    fn damaged(&mut self, damage: impl FnOnce(&mut Store, &Layout, &Layout)) -> String {
        let mut s = self.good.clone();
        damage(&mut s, &self.old, &self.new);
        self.count += 1;
        s.write(&self.dir.join(format!("case-{}.img", self.count)))
    }

    /// The Data volume of a damaged store must be the older checkpoint's, with one newer checkpoint skipped for a
    /// reason that holds every text of `why`.
    fn falls_back(&mut self, what: &str, why: &[&str], damage: impl FnOnce(&mut Store, &Layout, &Layout)) {
        let image = self.damaged(damage);
        let tree = Container::open(&image, Pick::Newest).and_then(|mut c| c.volume(2)).unwrap_or_else(|e| panic!("{what}: {e}"));
        let opened = tree.opened().unwrap().clone();
        let said = opened.text(&image, 2, &tree.name);
        assert_eq!((opened.xid, opened.volume_xid, opened.skipped.len()), (5, 5, 1), "{what}: {said}");
        assert_eq!(opened.skipped[0].0, 6, "{what}: {said}");
        for part in why {
            assert!(opened.skipped[0].2.contains(part), "{what}: no {part:?} in: {said}");
        }
        assert!(said.contains("is read at checkpoint xid 5") && said.contains("1 newer of 2 skipped:") && said.contains("\n  xid 6 at block 4: "), "{what}: {said}");
        assert!(!said.contains("short read"), "{what}: {said}");
        assert!(volume(&tree).unwrap() == self.older, "{what}: the files are not the older checkpoint's");
    }
}

#[test]
fn the_newest_checkpoint_of_a_good_store() {
    let Some(f) = Fixture::new("good") else { return };
    let image = f.good.write(&f.dir.join("good.img"));
    let mut container = Container::open(&image, Pick::Newest).unwrap();
    assert_eq!(container.volume_count().unwrap(), 3);
    assert_eq!(container.checkpoints.iter().map(|c| (c.xid, c.block, c.refused.is_none())).collect::<Vec<_>>(), [(6, 4, true), (5, 2, true)]);
    let tree = container.volume(2).unwrap();
    let opened = tree.opened().unwrap();
    assert_eq!((tree.name.as_str(), opened.xid, opened.block, opened.volume_xid, opened.block0_xid, opened.skipped.len()), ("Data", 6, 4, 5, 5, 0));
    assert!(opened.nodes > 6, "every node of the trees was checked: {}", opened.nodes);
    let said = opened.text(&image, 2, &tree.name);
    assert!(said.ends_with(&format!("is read at checkpoint xid 6 (superblock at block 4, volume superblock xid 5): the newest of 2; block 0 has xid 5; {} tree nodes checked", opened.nodes)), "{said}");
    assert!(volume(&tree).unwrap() == f.newer);
    assert!(f.newer != f.older);
    // the other volumes, the count, a volume by its name
    assert_eq!(volume(&container.volume(0).unwrap()).unwrap()["a/file"], b"preboot of newer\n");
    assert!(volume(&container.volume(1).unwrap()).unwrap().is_empty());
    assert!(text(container.volume(3).err().unwrap()).ends_with("no volume 3 (the container's volumes: 0, 1, 2)"));
    assert_eq!((container.volume_named(Some("Data")).unwrap().0, container.volume_named(Some("1")).unwrap().1.name.as_str(), container.volume_named(None).unwrap().0), (2, "xART", 0));
    assert!(text(container.volume_named(Some("Nothing")).err().unwrap()).ends_with("no volume named Nothing (it has: Preboot, xART, Data)"));
    // one checkpoint by its transaction; block 0's state (the older one's, as on a store copied after use)
    let at5 = Container::open(&image, Pick::Xid(5)).unwrap().volume(2).unwrap();
    assert!(at5.opened().unwrap().text(&image, 2, "Data").contains("is read at checkpoint xid 5 (superblock at block 2, volume superblock xid 5): the one asked for of 2;"));
    assert!(volume(&at5).unwrap() == f.older);
    assert!(text(Container::open(&image, Pick::Xid(7)).err().unwrap()).ends_with("no checkpoint of transaction 7 (the container's: 6, 5)"));
    let block0 = ApfsTree::open_volume(&image, 2, false).unwrap();
    assert!(block0.opened().unwrap().text(&image, 2, "Data").contains("is read as block 0 has it (xid 5, volume superblock xid 5)"));
    assert!(volume(&block0).unwrap() == f.older);
    assert!(volume(&ApfsTree::open_volume(&image, 2, true).unwrap()).unwrap() == f.newer);
    assert_eq!((ApfsTree::volume_count(&image, true).unwrap(), ApfsTree::volume_count(&image, false).unwrap()), (3, 3));
    // what `image apfs-checkpoints` prints
    let report = Container::open(&image, Pick::Newest).unwrap().report();
    assert!(report.contains("block 0 has xid 5; the checkpoint descriptor area is blocks 1..5, 2 container superblocks\nxid 6 at block 4: the container passes (3 volumes)\n  volume 0 (Preboot): passes;"), "{report}");
    assert_eq!(report.matches(": passes; superblock xid 5 at block ").count(), 6, "{report}");
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn a_cut_off_newest_checkpoint_falls_back_to_the_one_before() {
    let Some(mut f) = Fixture::new("cut") else { return };
    // (a) the newest superblock's checksum is wrong
    f.falls_back("a: superblock checksum", &["refused the container superblock at block 4 (0x4)", "[type 0x80000001/0x0: a container superblock, oid 1, xid 6]", "its checksum is wrong (stored 0x"], |s, _, new| s.spoil_checksum(new.superblock));
    f.falls_back("a: superblock half zeros", &["refused the container superblock at block 4", "its checksum is wrong"], |s, _, new| s.half_zeros(new.superblock, 64));
    f.falls_back("a: checkpoint map", &["refused the checkpoint map block at block 3 (0x3)", "named by block 4", "its checksum is wrong"], |s, _, new| s.half_pattern(new.map, 512));
    // (b) the newest superblock is whole, and names an object map past the container's end: the phone's pattern
    f.falls_back(
        "b: omap address",
        &["refused the container's object map at block 281470681808895 (0xffff0000ffff), named by block 4: the address is past the container's end ("],
        |s, _, new| {
            s.put64(new.superblock, 160, store::PATTERN_BLOCK);
            s.seal(new.superblock);
        },
    );
    // ... the object map whole, its tree's address the pattern
    f.falls_back("b: omap tree address", &["refused the node of the container's object map tree at block 281470681808895 (0xffff0000ffff), named by block ", "the address is past the container's end"], |s, _, new| {
        s.put64(new.omap.block, 48, store::PATTERN_BLOCK);
        s.seal(new.omap.block);
    });
    // ... and as on the phone: where the object map was, a tree node now is (its free lists at the place of the
    // tree's address)
    f.falls_back("b: a tree node where the object map was", &["refused the container's object map at block ", ": a tree node, oid ", "named by block 4: it is a tree node, not the container's object map"], |s, _, new| {
        let (leaf, omap) = (new.volumes[2].leaf() as usize, new.omap.block as usize);
        s.data.copy_within(leaf * store::BLOCK..(leaf + 1) * store::BLOCK, omap * store::BLOCK);
        assert_eq!(s.u64(omap as u64, 48), store::PATTERN_BLOCK);
    });
    f.falls_back("b: address 0", &["refused the container's object map at block 0 (0x0), named by block 4: address 0 is block 0"], |s, _, new| {
        s.put64(new.superblock, 160, 0);
        s.seal(new.superblock);
    });
    // (c) a tree node is half written: zeros, or a pattern, from some offset on
    let node = "refused the node of the ";
    f.falls_back("c: container omap node, zeros", &[node, "container's object map tree at block ", "its checksum is wrong"], |s, _, new| s.half_zeros(new.omap.nodes[0], 1024));
    f.falls_back("c: volume superblock, pattern", &["volume 2, superblock xid 5 at block ", "refused the volume superblock at block ", "its checksum is wrong"], |s, _, new| s.half_pattern(new.volumes[2].superblock, 600));
    f.falls_back("c: volume omap, all zeros", &["refused the volume's object map at block ", "the block is all zeros (never written, or cut off)"], |s, _, new| s.half_zeros(new.volumes[2].omap.block, 0));
    f.falls_back("c: volume omap node, pattern", &[node, "volume's object map tree at block ", "its checksum is wrong"], |s, _, new| s.half_pattern(new.volumes[2].omap.nodes[0], 2000));
    f.falls_back("c: fs root, zeros", &[node, "file-system tree at block ", "its checksum is wrong"], |s, _, new| s.half_zeros(new.volumes[2].root(), 3000));
    f.falls_back("c: fs leaf, zeros", &[node, "file-system tree at block ", ": a tree node, oid ", ", named by block ", "its checksum is wrong (stored 0x"], |s, _, new| s.half_zeros(new.volumes[2].leaf(), 2048));
    f.falls_back("c: fs leaf, pattern", &[node, "file-system tree at block ", "half written, or not an object"], |s, _, new| s.half_pattern(new.volumes[2].leaf(), 64));
    f.falls_back("c: fs leaf, the last bytes", &[node, "file-system tree at block ", "its checksum is wrong"], |s, _, new| s.half_zeros(new.volumes[2].leaf(), store::BLOCK - 16));
    // a node that is whole and is not what its parent says: of a later transaction, another object, a wrong
    // level, an entry outside the node
    f.falls_back("a later transaction", &[node, "its transaction 9 is newer than the checkpoint's (6)"], |s, _, new| {
        let leaf = new.volumes[2].leaf();
        s.put64(leaf, 16, 9);
        s.seal(leaf);
    });
    f.falls_back("another object id", &[node, "its object id is 77777, not "], |s, _, new| {
        let leaf = new.volumes[2].leaf();
        s.put64(leaf, 8, 77777);
        s.seal(leaf);
    });
    f.falls_back("a wrong level", &[node, "its flags (0x2) and its level (3) disagree about being a leaf"], |s, _, new| {
        let leaf = new.volumes[2].leaf();
        s.data[leaf as usize * store::BLOCK + 34] = 3;
        s.seal(leaf);
    });
    f.falls_back("an entry outside its node", &[node, "entry 0 of ", "is outside the node"], |s, _, new| {
        let leaf = new.volumes[2].leaf();
        s.data[leaf as usize * store::BLOCK + 56 + 4..][..2].copy_from_slice(&0xFFFFu16.to_le_bytes());
        s.seal(leaf);
    });
    f.falls_back("a mapping past the end", &[node, "volume's object map tree", "it maps object ", " to block 281470681808895 (0xffff0000ffff), which is outside the container"], |s, _, new| {
        let leaf = *new.volumes[2].omap.nodes.last().unwrap();
        s.put64(leaf, store::BLOCK - 40 * (leaf == new.volumes[2].omap.nodes[0]) as usize - 8, store::PATTERN_BLOCK);
        s.seal(leaf);
    });
    // a superblock left in the area from a container that was there before
    f.falls_back("another container's superblock", &["refused the container superblock at block 4", "it is of another container ("], |s, _, new| {
        s.data[new.superblock as usize * store::BLOCK + 72] ^= 0xFF;
        s.seal(new.superblock);
    });
    f.falls_back("not the last block of its checkpoint", &["refused the container superblock at block 4", "it is not the last block of its own checkpoint"], |s, _, new| {
        s.put32(new.superblock, 136, 0);
        s.seal(new.superblock);
    });
    // a copy that was cut off in the newer checkpoint's blocks
    f.falls_back("a file that is cut off", &["the block is past the file's end (the file holds ", "a copy that was cut off?"], |s, old, new| {
        let first = new.omap.block.min(new.volumes[2].omap.block).min(new.volumes[2].superblock);
        assert!(first > old.volumes[2].nodes.iter().map(|n| n.0).max().unwrap());
        s.data.truncate(first as usize * store::BLOCK);
    });
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn one_volume_falls_back_and_the_others_do_not() {
    let Some(mut f) = Fixture::new("volume") else { return };
    // the newest checkpoint's container is whole; the Data volume's tree is not
    let image = f.damaged(|s, _, new| s.half_zeros(new.volumes[2].leaf(), 2048));
    let mut container = Container::open(&image, Pick::Newest).unwrap();
    assert!(container.checkpoints.iter().all(|c| c.refused.is_none()));
    for (index, name) in [(0, "Preboot"), (1, "xART")] {
        let tree = container.volume(index).unwrap();
        let opened = tree.opened().unwrap();
        assert_eq!((tree.name.as_str(), opened.xid, opened.skipped.len()), (name, 6, 0));
    }
    assert_eq!(volume(&container.volume(0).unwrap()).unwrap()["a/file"], b"preboot of newer\n");
    let data = container.volume(2).unwrap();
    assert_eq!((data.opened().unwrap().xid, data.opened().unwrap().skipped.len()), (5, 1));
    assert!(volume(&data).unwrap() == f.older);
    let report = container.report();
    assert!(report.contains("xid 6 at block 4: the container passes (3 volumes)\n  volume 0 (Preboot): passes;") && report.contains("\n  volume 2 (object 1028): superblock xid 5 at block "), "{report}");

    // the same with a history in the container's object map: the volume's older state is read in the newest
    // checkpoint itself
    let image = f.damaged(|s, old, new| {
        store::add_history(s, old, new, 2);
        s.half_zeros(new.volumes[2].leaf(), 2048);
    });
    let mut container = Container::open(&image, Pick::Newest).unwrap();
    let data = container.volume(2).unwrap();
    let opened = data.opened().unwrap();
    let said = opened.text(&image, 2, "Data");
    assert_eq!((opened.xid, opened.block, opened.volume_xid, opened.skipped.len()), (6, 4, 5, 1), "{said}");
    assert!(opened.skipped[0].2.starts_with("volume 2, superblock xid 6 at block ") && opened.skipped[0].2.ends_with("(an older state of the volume in the same checkpoint is read)"), "{said}");
    assert!(volume(&data).unwrap() == f.older);
    assert_eq!(container.volume(0).unwrap().opened().unwrap().xid, 6);
    // the history alone changes nothing: the newest state is read
    let image = f.damaged(|s, old, new| store::add_history(s, old, new, 2));
    let data = Container::open(&image, Pick::Newest).unwrap().volume(2).unwrap();
    assert_eq!((data.opened().unwrap().xid, data.opened().unwrap().volume_xid, data.opened().unwrap().skipped.len()), (6, 6, 0));
    assert!(volume(&data).unwrap() == f.newer);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn no_checkpoint_that_passes_is_a_clear_error() {
    let Some(mut f) = Fixture::new("none") else { return };
    // (d) every superblock: the two of the area and block 0
    let image = f.damaged(|s, old, new| {
        s.spoil_checksum(new.superblock);
        s.half_zeros(old.omap.block, 40);
    });
    let said = text(Container::open(&image, Pick::Newest).unwrap().volume_count().err().unwrap());
    assert!(said.contains(": none of the 3 container superblocks can be read:\n  xid 6 at block 4: refused the container superblock at block 4"), "{said}");
    assert!(said.contains("\n  xid 5 at block 2: refused the container's object map at block ") && said.contains("\n  xid 5 at block 0: refused the container's object map at block "), "{said}");
    assert_eq!(said.lines().count(), 4, "{said}");
    assert!(ApfsTree::open_volume(&image, 2, true).is_err() && ApfsTree::volume_count(&image, true).is_err());
    let said = text(ApfsTree::open_volume(&image, 2, false).err().unwrap());
    assert!(said.contains(": block 0's superblock cannot be read (without --block0 the checkpoint area is searched):\n  xid 5 at block 0: refused the container's object map"), "{said}");
    // the containers pass, the Data volume passes at none: the other volumes are read
    let image = f.damaged(|s, old, new| {
        s.half_zeros(new.volumes[2].leaf(), 100);
        s.half_pattern(old.volumes[2].root(), 100);
    });
    let mut container = Container::open(&image, Pick::Newest).unwrap();
    assert_eq!(container.volume(0).unwrap().opened().unwrap().xid, 6);
    let said = text(container.volume(2).err().unwrap());
    assert!(said.contains(": volume 2 cannot be read at any of the 2 checkpoints:\n  xid 6 at block 4: volume 2, superblock xid 5 at block "), "{said}");
    assert!(said.contains("\n  xid 5 at block 2: volume 2, superblock xid 5 at block ") && said.matches("refused the node of the file-system tree at block ").count() == 2, "{said}");
    // not a container at all, an empty file
    let junk = f.dir.join("junk.img");
    std::fs::write(&junk, vec![0x5A; 3 * store::BLOCK]).unwrap();
    assert!(text(Container::open(junk.to_str().unwrap(), Pick::Newest).err().unwrap()).ends_with("no APFS container found: not APFS"));
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn what_is_not_checked_at_the_open_is_checked_at_the_read() {
    let Some(mut f) = Fixture::new("read") else { return };
    // a file extent that runs past the container: the checkpoint passes (its trees are whole), the file's read
    // is refused by name
    let image = f.damaged(|s, _, new| {
        let leaf = new.volumes[2].leaf();
        let (count, toc, keys) = (s.u32(leaf, 36) as usize, 56 + s.u16(leaf, 40) as usize, 56 + s.u16(leaf, 40) as usize + s.u16(leaf, 42) as usize);
        let extent = (0..count).find(|i| s.u64(leaf, keys + s.u16(leaf, toc + 8 * i) as usize) >> 60 == 8).expect("a file extent in the last leaf");
        let value = store::BLOCK - s.u16(leaf, toc + 8 * extent + 4) as usize;
        s.put64(leaf, value + 8, store::PATTERN_BLOCK);
        s.seal(leaf);
    });
    let tree = ApfsTree::open_volume(&image, 2, true).unwrap();
    assert_eq!(tree.opened().unwrap().xid, 6);
    let said = text(volume(&tree).err().unwrap());
    assert!(said.contains(": refused an extent of data stream ") && said.contains(" from block 281470681808895 (0xffff0000ffff): it runs past the container's end ("), "{said}");
    // block 0's state is not checked as a whole when it is opened (it is the only candidate): a damaged node is
    // refused when a read reaches it
    let image = f.damaged(|s, old, _| s.half_zeros(old.volumes[2].leaf(), 2048));
    let tree = ApfsTree::open_volume(&image, 2, false).unwrap();
    let said = text(volume(&tree).err().unwrap());
    assert!(said.contains(": refused the node of the file-system tree at block ") && said.contains("its checksum is wrong"), "{said}");
    // (with the checkpoints: the newest one is whole)
    assert!(volume(&ApfsTree::open_volume(&image, 2, true).unwrap()).unwrap() == f.newer);
    let _ = std::fs::remove_dir_all(&f.dir);
}
