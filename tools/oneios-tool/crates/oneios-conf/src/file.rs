//! A file replaced whole: a temporary file in the same folder, its mode, owner and SELinux label taken from the file
//! it replaces, written and synced, then renamed over it (a reader sees the old text or the new, never half); the
//! replaced text kept as `FILE.bak` (one backup).

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// FILE.bak beside FILE.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".bak");
    path.with_file_name(name)
}

/// A link is followed: the file it names is replaced, the link stays.
fn target(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

/// The text of FILE, or None when there is no file.
pub fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Replace FILE with `bytes`; with `backup`, what it had goes to FILE.bak first. A new file is 0644.
pub fn replace(path: &Path, bytes: &[u8], backup: bool) -> io::Result<()> {
    let path = target(path);
    let old = fs::metadata(&path).ok();
    if backup && old.is_some() {
        let before = fs::read(&path)?;
        put(&backup_path(&path), &before, old.as_ref(), &path)?;
    }
    put(&path, bytes, old.as_ref(), &path)
}

/// Put FILE.bak back: FILE gets its text, and the text FILE had becomes FILE.bak (so a second restore undoes it).
pub fn restore(path: &Path) -> io::Result<()> {
    let path = target(path);
    let bak = backup_path(&path);
    let saved = fs::read(&bak)?;
    let now = read(&path)?;
    let like = fs::metadata(&path).or_else(|_| fs::metadata(&bak)).ok();
    put(&path, &saved, like.as_ref(), &bak)?;
    if let Some(now) = now {
        put(&bak, &now, like.as_ref(), &path)?;
    }
    Ok(())
}

fn put(dest: &Path, bytes: &[u8], like: Option<&Metadata>, label_from: &Path) -> io::Result<()> {
    let dir = dest
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.oneios-tool-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mode = like.map_or(0o644, |m| m.permissions().mode() & 0o7777);
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        if let Some(m) = like {
            // (as root on the phone the owner is kept; a user who may not give the file away keeps it)
            let _ = std::os::unix::fs::fchown(&f, Some(m.uid()), Some(m.gid()));
            copy_label(label_from, &f);
        }
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.sync_all()?;
        fs::rename(&tmp, dest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// The SELinux label of `from` on `to` (Android: a file made by root in the app's folder would get another label
/// than the one it replaces); where it cannot be read or set, the new file keeps its own.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn copy_label(from: &Path, to: &File) {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;
    let Ok(path) = std::ffi::CString::new(from.as_os_str().as_bytes()) else {
        return;
    };
    let name = c"security.selinux";
    let mut buf = [0u8; 256];
    // SAFETY: path and name are NUL-terminated; buf is writable for its length.
    let n = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n > 0 {
        // SAFETY: the fd is open; buf holds n bytes the kernel wrote.
        let _ = unsafe {
            libc::fsetxattr(
                to.as_raw_fd(),
                name.as_ptr(),
                buf.as_ptr().cast(),
                n as usize,
                0,
            )
        };
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn copy_label(_from: &Path, _to: &File) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oneios-conf-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn replace_keeps_mode_and_one_backup() {
        let d = dir("replace");
        let f = d.join("xnuhost.conf");
        replace(&f, b"a=1\n", true).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"a=1\n");
        assert!(
            !backup_path(&f).exists(),
            "no backup of a file that was not there"
        );
        assert_eq!(
            fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
        replace(&f, b"a=2\n", true).unwrap();
        replace(&f, b"a=3\n", true).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"a=3\n");
        assert_eq!(
            fs::read(backup_path(&f)).unwrap(),
            b"a=2\n",
            "one backup: the text before the last write"
        );
        assert_eq!(
            fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600,
            "the mode kept"
        );
        let left: Vec<_> = fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "no temporary file left: {left:?}");
        restore(&f).unwrap();
        assert_eq!(
            (fs::read(&f).unwrap(), fs::read(backup_path(&f)).unwrap()),
            (b"a=2\n".to_vec(), b"a=3\n".to_vec())
        );
        restore(&f).unwrap();
        assert_eq!(
            fs::read(&f).unwrap(),
            b"a=3\n",
            "a second restore undoes the first"
        );
        // through a link: the file it names is replaced, the link kept
        let l = d.join("link.conf");
        std::os::unix::fs::symlink(&f, &l).unwrap();
        replace(&l, b"a=4\n", false).unwrap();
        assert!(fs::symlink_metadata(&l).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&f).unwrap(), b"a=4\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn restore_without_a_backup_fails() {
        let d = dir("restore");
        let f = d.join("x.conf");
        fs::write(&f, "a=1\n").unwrap();
        assert!(restore(&f).is_err());
        assert_eq!(fs::read(&f).unwrap(), b"a=1\n");
        let _ = fs::remove_dir_all(&d);
    }
}
