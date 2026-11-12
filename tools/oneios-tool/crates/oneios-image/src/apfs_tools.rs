//! `image apfs-ls` and `image apfs-copy`: a folder's entries and a copy of a file or a folder, read from a raw
//! APFS image without mounting it. Only in this tool (imagetool.py has no such command).

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

pub fn ls(image: &str, path: &str, volume: Option<&str>, newest: bool) -> Result<(), Fail> {
    let tree = open(image, volume, newest)?;
    let dir = tree.find(path)?.ok_or_else(|| err!("{image}: no /{path} in the volume"))?;
    let mut entries = tree.list(dir)?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut text = String::new();
    for e in &entries {
        let st = tree.stat(e.id)?;
        text.push_str(&format!("{:06o} {:>5} {:>5} {:>12} {}\n", st.mode, st.uid, st.gid, st.size, e.name));
    }
    say(text.trim_end_matches('\n'))
}

struct Copied {
    files: u64,
    bytes: u64,
    compressed: u64,
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
            if ApfsTree::is_compressed(&st) {
                done.compressed += 1;
                return Ok(());
            }
            let data = tree.read_file(id)?;
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

pub fn copy(image: &str, src: &str, dst: &str, volume: Option<&str>, newest: bool) -> Result<(), Fail> {
    let tree = open(image, volume, newest)?;
    let id = tree.find(src)?.ok_or_else(|| err!("{image}: no /{} in the volume", src.trim_matches('/')))?;
    let kind = match tree.stat(id)?.mode & 0o170000 {
        0o040000 => DT_DIR,
        0o100000 => DT_REG,
        0o120000 => DT_LNK,
        other => return Err(err!("{image}: /{} is not a file, a folder or a link (mode {other:o})", src.trim_matches('/'))),
    };
    let mut done = Copied { files: 0, bytes: 0, compressed: 0 };
    copy_one(&tree, id, kind, Path::new(dst), &mut done)?;
    let more = if done.compressed == 0 { String::new() } else { format!(", skipped {} compressed files", done.compressed) };
    say(&format!("copied {} files, {} bytes{more}", done.files, done.bytes))
}
