//! `image apfs-ls` and `image apfs-copy`: a folder's entries and a copy of a file or a folder, read from a raw
//! APFS image without mounting it. Only in this tool (imagetool.py has no such command).

use crate::apfs::fuse::{self, Backend, Mount};
use crate::apfs::{ApfsTree, DT_DIR, DT_LNK, DT_REG};
use crate::{crash, err, say, Fail};
use std::path::Path;

/// The volume `--volume N|NAME` names (default: the first).
pub fn open(image: &str, volume: Option<&str>, newest: bool) -> Result<ApfsTree, Fail> {
    let Some(want) = volume else { return ApfsTree::open_volume(image, 0, newest) };
    if let Ok(index) = want.parse::<usize>() {
        if index >= ApfsTree::volume_count(image)? {
            return Err(err!("{image}: no volume {index}"));
        }
        return ApfsTree::open_volume(image, index, newest);
    }
    let mut names = Vec::new();
    for index in 0..ApfsTree::volume_count(image)? {
        match ApfsTree::open_volume(image, index, newest) {
            Ok(tree) if tree.name == want => return Ok(tree),
            Ok(tree) => names.push(tree.name),
            Err(_) => {}
        }
    }
    Err(err!("{image}: no volume named {want} (it has: {})", names.join(", ")))
}

/// The volume as apfs-fuse mounts it: `--volume` must be a number there, or a name our reader can look up.
fn mount(image: &str, volume: Option<&str>) -> Result<Mount, Fail> {
    let index = match volume {
        None => 0,
        Some(v) => match v.parse::<usize>() {
            Ok(index) => index,
            // a name: its number, which our reader can say when it reads the container
            Err(_) => (0..ApfsTree::volume_count(image).unwrap_or(0))
                .find(|i| ApfsTree::open_volume(image, *i, false).is_ok_and(|tree| tree.name == v))
                .ok_or_else(|| err!("--apfs fuse: no volume named {v} that our reader can look up; give the volume's number"))?,
        },
    };
    Mount::new(image, index)
}

/// Our reader, apfs-fuse, or ours first and apfs-fuse when ours refuses (`--apfs auto`): what `own` gives, else
/// what `fuse` gives on the mounted volume's root.
fn with_backend<T>(image: &str, volume: Option<&str>, backend: Backend, own: impl FnOnce() -> Result<T, Fail>, fuse: impl FnOnce(&Path) -> Result<T, Fail>) -> Result<T, Fail> {
    // ONEIOS_APFS_TEST_REFUSE: for the tests of the fallback, our reader refuses as it would an image it cannot read
    let own = || if std::env::var_os("ONEIOS_APFS_TEST_REFUSE").is_some() { Err(err!("{image}: refused for a test (ONEIOS_APFS_TEST_REFUSE)")) } else { own() };
    match backend {
        Backend::Own => own(),
        Backend::Fuse => fuse(&mount(image, volume)?.root()),
        Backend::Auto => match own() {
            Ok(done) => Ok(done),
            Err(e) if fuse::available() => {
                eprintln!("{e}\nour APFS reader refused; reading through apfs-fuse instead");
                fuse(&mount(image, volume)?.root())
            }
            Err(e) => Err(e),
        },
    }
}

pub fn ls(image: &str, path: &str, volume: Option<&str>, newest: bool, backend: Backend) -> Result<(), Fail> {
    let own = || -> Result<String, Fail> {
        let tree = open(image, volume, newest)?;
        let dir = tree.find(path)?.ok_or_else(|| err!("{image}: no /{path} in the volume"))?;
        let mut entries = tree.list(dir)?;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let mut text = String::new();
        for e in &entries {
            let st = tree.stat(e.id)?;
            text.push_str(&format!("{:06o} {:>5} {:>5} {:>12} {}\n", st.mode, st.uid, st.gid, st.size, e.name));
        }
        Ok(text)
    };
    let mounted = |root: &Path| -> Result<String, Fail> {
        use std::os::unix::fs::MetadataExt;
        let dir = root.join(path.trim_matches('/'));
        let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(&dir).map_err(|_| err!("{image}: no /{path} in the volume"))?.flatten().map(|e| e.file_name()).collect();
        names.sort();
        let mut text = String::new();
        for name in names {
            let st = std::fs::symlink_metadata(dir.join(&name)).map_err(|e| err!("{}: {e}", dir.join(&name).display()))?;
            // a folder's size is the readers' own business (ours: 0, apfs-fuse: a count): 0 for both
            let size = if st.is_dir() { 0 } else { st.size() };
            text.push_str(&format!("{:06o} {:>5} {:>5} {:>12} {}\n", st.mode(), st.uid(), st.gid(), size, name.to_string_lossy()));
        }
        Ok(text)
    };
    let text = with_backend(image, volume, backend, own, mounted)?;
    say(text.trim_end_matches('\n'))
}

struct Copied {
    files: u64,
    bytes: u64,
    /// files that were kept compressed in the image and are written plain
    compressed: u64,
    /// compressed files that could not be decoded
    skipped: u64,
}

fn copy_one(tree: &ApfsTree, id: u64, kind: u16, dst: &Path, done: &mut Copied) -> Result<(), Fail> {
    let bad = |e: std::io::Error| crash!("{}: {e}", dst.display());
    match kind {
        DT_DIR => {
            std::fs::create_dir_all(dst).map_err(bad)?;
            for e in tree.list(id)? {
                if e.name.contains('/') || e.name == "." || e.name == ".." {
                    return Err(err!("a name in the volume's records is not a file name: {:?}", e.name));
                }
                copy_one(tree, e.id, e.kind, &dst.join(&e.name), done)?;
            }
        }
        DT_REG => {
            let st = tree.stat(id)?;
            let data = match tree.read_file(id) {
                Ok(data) => data,
                // a compressed file this reader cannot decode (LZBITMAP) is said, counted and left out
                Err(e) if ApfsTree::is_compressed(&st) => {
                    eprintln!("{}: {e}", dst.display());
                    done.skipped += 1;
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            if ApfsTree::is_compressed(&st) {
                done.compressed += 1;
            }
            std::fs::write(dst, &data).map_err(bad)?;
            done.files += 1;
            done.bytes += data.len() as u64;
        }
        DT_LNK => {
            if let Some(target) = tree.xattr(id, "com.apple.fs.symlink")? {
                let target = String::from_utf8_lossy(&target[..target.iter().position(|b| *b == 0).unwrap_or(target.len())]).into_owned();
                let _ = std::fs::remove_file(dst);
                std::os::unix::fs::symlink(target, dst).map_err(bad)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// A file, a link or a folder of a mounted volume to `dst`, as `copy_one` writes them.
fn copy_mounted(src: &Path, dst: &Path, done: &mut Copied) -> Result<(), Fail> {
    let bad = |e: std::io::Error| crash!("{}: {e}", dst.display());
    let kind = std::fs::symlink_metadata(src).map_err(|e| err!("{}: {e}", src.display()))?.file_type();
    if kind.is_dir() {
        std::fs::create_dir_all(dst).map_err(bad)?;
        let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(src).map_err(|e| err!("{}: {e}", src.display()))?.flatten().map(|e| e.file_name()).collect();
        names.sort();
        for name in names {
            copy_mounted(&src.join(&name), &dst.join(&name), done)?;
        }
    } else if kind.is_file() {
        done.bytes += std::fs::copy(src, dst).map_err(|e| crash!("{} -> {}: {e}", src.display(), dst.display()))?;
        done.files += 1;
    } else if kind.is_symlink() {
        let _ = std::fs::remove_file(dst);
        std::os::unix::fs::symlink(std::fs::read_link(src).map_err(bad)?, dst).map_err(bad)?;
    }
    Ok(())
}

pub fn copy(image: &str, src: &str, dst: &str, volume: Option<&str>, newest: bool, backend: Backend) -> Result<(), Fail> {
    let own = || -> Result<Copied, Fail> {
        let tree = open(image, volume, newest)?;
        let id = tree.find(src)?.ok_or_else(|| err!("{image}: no /{} in the volume", src.trim_matches('/')))?;
        let kind = match tree.stat(id)?.mode & 0o170000 {
            0o040000 => DT_DIR,
            0o100000 => DT_REG,
            0o120000 => DT_LNK,
            other => return Err(err!("{image}: /{} is not a file, a folder or a link (mode {other:o})", src.trim_matches('/'))),
        };
        let mut done = Copied { files: 0, bytes: 0, compressed: 0, skipped: 0 };
        copy_one(&tree, id, kind, Path::new(dst), &mut done)?;
        Ok(done)
    };
    let mounted = |root: &Path| -> Result<Copied, Fail> {
        let from = root.join(src.trim_matches('/'));
        if std::fs::symlink_metadata(&from).is_err() {
            return Err(err!("{image}: no /{} in the volume", src.trim_matches('/')));
        }
        let mut done = Copied { files: 0, bytes: 0, compressed: 0, skipped: 0 };
        copy_mounted(&from, Path::new(dst), &mut done)?;
        Ok(done)
    };
    let done = with_backend(image, volume, backend, own, mounted)?;
    let was = if done.compressed == 0 { String::new() } else { format!(" ({} of them were kept compressed)", done.compressed) };
    let more = if done.skipped == 0 { String::new() } else { format!(", skipped {} compressed files", done.skipped) };
    say(&format!("copied {} files, {} bytes{was}{more}", done.files, done.bytes))
}
