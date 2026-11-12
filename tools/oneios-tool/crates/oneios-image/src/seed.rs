//! The Data seed: `tree-copy` (copy the OS volume's /private/var as the calling user and record what a non-root
//! copy loses) and `seed-pack` (the deterministic pax tar, byte for byte what Python's `tarfile` writes).
//!
//! Both keep Python's order where it shows (the meta file's entries, the tar's members) and do the work that has
//! no order in parallel: reading xattrs and copying files, reading the image's records, reading the files of the
//! tar.

use crate::apfs::{self, ApfsTree, Rec};
use crate::py;
use crate::tar::{self, TarInfo};
use crate::{crash, err, Fail};
use oneios_common::pyjson::{self, Value};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;

/// xattrs that only describe how APFS stores an entry (compression, a link target): the copy has the plain
/// content and the link itself, so they are dropped
const DROP_XATTRS: [&str; 3] = ["com.apple.decmpfs", "com.apple.ResourceFork", "com.apple.fs.symlink"];

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

fn os_fail(e: std::io::Error, path: &str) -> Fail {
    crash!("{}", py::os_error(&e, path))
}

fn not_utf8(what: &str, raw: &[u8]) -> Fail {
    crash!(
        "{what} {} is not UTF-8; this tool handles UTF-8 names only, use scripts/image/imagetool.py",
        py::repr_str(&String::from_utf8_lossy(raw))
    )
}

fn c_path(path: &str) -> Result<CString, Fail> {
    CString::new(path).map_err(|_| crash!("{}: a NUL in a path", py::repr_str(path)))
}

/// `d[key] = value` of a Python dict.
fn dict_set(d: &mut Vec<(String, String)>, key: String, value: String) {
    match d.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => d.push((key, value)),
    }
}

// ---------------------------------------------------------------- xattrs

/// (name, value) of every xattr of path, not following a link. An error reads as "none", like Python's.
#[cfg(target_os = "macos")]
fn xattrs_raw(path: &CString) -> Vec<(Vec<u8>, Vec<u8>)> {
    let nofollow = libc::XATTR_NOFOLLOW;
    let mut out: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    // Safety: path is NUL-terminated; each buffer is as long as the size passed with it.
    unsafe {
        let size = libc::listxattr(path.as_ptr(), std::ptr::null_mut(), 0, nofollow);
        if size <= 0 {
            return out;
        }
        let mut names = vec![0u8; size as usize];
        let size = libc::listxattr(path.as_ptr(), names.as_mut_ptr().cast(), names.len(), nofollow);
        names.truncate(size.max(0) as usize);
        for name in names.split(|b| *b == 0) {
            if name.is_empty() {
                continue;
            }
            let c_name = CString::new(name).expect("split at NUL");
            let n = libc::getxattr(path.as_ptr(), c_name.as_ptr(), std::ptr::null_mut(), 0, 0, nofollow);
            if n < 0 {
                continue;
            }
            let mut value = vec![0u8; (n as usize).max(1)];
            let n = libc::getxattr(path.as_ptr(), c_name.as_ptr(), value.as_mut_ptr().cast(), n as usize, 0, nofollow);
            if n >= 0 {
                value.truncate(n as usize);
                match out.iter_mut().find(|(k, _)| k == name) {
                    Some(slot) => slot.1 = value,
                    None => out.push((name.to_vec(), value)),
                }
            }
        }
    }
    out
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn xattrs_raw(path: &CString) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    // Safety: path is NUL-terminated; each buffer is as long as the size passed with it.
    unsafe {
        let mut names = Vec::new();
        loop {
            let size = libc::llistxattr(path.as_ptr(), std::ptr::null_mut(), 0);
            if size <= 0 {
                return out;
            }
            names.resize(size as usize, 0u8);
            let got = libc::llistxattr(path.as_ptr(), names.as_mut_ptr().cast(), names.len());
            if got >= 0 {
                names.truncate(got as usize);
                break;
            }
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::ERANGE) {
                return out;
            }
        }
        for name in names.split(|b| *b == 0) {
            if name.is_empty() {
                continue;
            }
            let c_name = CString::new(name).expect("split at NUL");
            loop {
                let size = libc::lgetxattr(path.as_ptr(), c_name.as_ptr(), std::ptr::null_mut(), 0);
                if size < 0 {
                    break;
                }
                let mut value = vec![0u8; (size as usize).max(1)];
                let got = libc::lgetxattr(path.as_ptr(), c_name.as_ptr(), value.as_mut_ptr().cast(), size as usize);
                if got >= 0 {
                    value.truncate(got as usize);
                    out.push((name.to_vec(), value));
                    break;
                }
                if std::io::Error::last_os_error().raw_os_error() != Some(libc::ERANGE) {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn xattrs_raw(_path: &CString) -> Vec<(Vec<u8>, Vec<u8>)> {
    Vec::new()
}

/// `_xattrs(path)`: name -> base64 of the value, in the order the system lists them.
fn xattrs(path: &str) -> Result<Vec<(String, String)>, Fail> {
    let mut out = Vec::new();
    for (name, value) in xattrs_raw(&c_path(path)?) {
        let name = String::from_utf8(name).map_err(|e| not_utf8("the xattr name", e.as_bytes()))?;
        if cfg!(target_os = "macos") {
            if !DROP_XATTRS.contains(&name.as_str()) {
                dict_set(&mut out, name, py::b64(&value));
            }
        } else {
            let plain = if name.starts_with("user.") && name.contains("com.apple") { &name[5..] } else { name.as_str() };
            if DROP_XATTRS.contains(&plain) || ["security.", "system.", "trusted."].iter().any(|p| name.starts_with(p)) {
                continue;
            }
            dict_set(&mut out, plain.to_string(), py::b64(&value));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- tree-copy

struct Item {
    rel: String,
    kind: char,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: i64,
    x: Vec<(String, String)>,
}

impl Item {
    fn value(&self, top: bool) -> Value {
        let mut d: Vec<(String, Value)> = Vec::with_capacity(6);
        let kind = ("t".to_string(), Value::Str(self.kind.to_string()));
        if top {
            d.push(kind.clone());
        }
        d.push(("m".into(), Value::from(self.mode)));
        d.push(("u".into(), Value::from(self.uid)));
        d.push(("g".into(), Value::from(self.gid)));
        d.push(("s".into(), Value::from(self.mtime)));
        if !self.x.is_empty() {
            d.push(("x".into(), Value::Dict(self.x.iter().map(|(k, v)| (k.clone(), Value::str(v))).collect())));
        }
        if !top {
            d.push(kind);
        }
        Value::Dict(d)
    }
}

/// The names in a folder, sorted as Python sorts str (by code point, which is the order of the UTF-8 bytes).
fn sorted_names(dir: &str) -> Result<Vec<String>, Fail> {
    let mut names = Vec::new();
    for e in fs::read_dir(dir).map_err(|e| os_fail(e, dir))? {
        let e = e.map_err(|e| os_fail(e, dir))?;
        let name = e.file_name();
        names.push(String::from_utf8(name.as_bytes().to_vec()).map_err(|e| not_utf8("the file name", e.as_bytes()))?);
    }
    names.sort_unstable();
    Ok(names)
}

/// What the writing side does for one entry. The reading side (lstat, xattrs, a link's target, a small file's
/// content) runs on all of the pool's threads; the writing side (mkdir, create, write, chmod, utime) has WRITERS
/// threads of its own.
enum Put {
    Dir,
    Link(std::path::PathBuf),
    /// A file read whole by the reading side: its content and time.
    Small(Vec<u8>, i64),
    /// A larger file: the writing side copies it from this path.
    Large(String, i64),
}

struct Job {
    id: usize,
    dst: String,
    write: Put,
}

/// Files up to this size are read by the reading side and handed over in memory.
const SMALL_FILE: u64 = 1 << 20;
/// Threads of the writing side (ONEIOS_IMAGE_WRITERS overrides it, to measure). Measured on an M4 (10 cores, APFS)
/// with 24A446's /private/var, 20,798 entries of which 14,796 are folders, 1.1 GiB: 1 thread 2.0 s, 2: 1.2 s,
/// 4: 0.8 s, 8: 0.8 s, 16: 0.75 s, while the time in the kernel grows from 2.1 s to 4.5 s (the threads wait for
/// each other in the file system). Python's copy of the same tree: 5.5 s.
const WRITERS: usize = 4;

fn create(dst: &str) -> Result<File, Fail> {
    fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(dst).map_err(|e| os_fail(e, dst))
}

/// What Python does after `shutil.copyfile`: chmod 600 and both times to the source's whole seconds.
fn finish(to: &File, dst: &str, mtime: i64) -> Result<(), Fail> {
    to.set_permissions(fs::Permissions::from_mode(0o600)).map_err(|e| os_fail(e, dst))?;
    let times = [libc::timespec { tv_sec: mtime as libc::time_t, tv_nsec: 0 }; 2];
    // Safety: the descriptor is open and `times` holds two timespecs.
    if unsafe { libc::futimens(to.as_raw_fd(), times.as_ptr()) } != 0 {
        return Err(os_fail(std::io::Error::last_os_error(), dst));
    }
    Ok(())
}

fn write_entry(job: &Job, buf: &mut Vec<u8>) -> Result<(), Fail> {
    let dst = job.dst.as_str();
    match &job.write {
        Put::Dir => fs::DirBuilder::new().mode(0o700).create(dst).map_err(|e| os_fail(e, dst)),
        Put::Link(target) => std::os::unix::fs::symlink(target, dst).map_err(|e| os_fail(e, dst)),
        Put::Small(data, mtime) => {
            let mut to = create(dst)?;
            to.write_all(data).map_err(|e| os_fail(e, dst))?;
            finish(&to, dst, *mtime)
        }
        Put::Large(src, mtime) => {
            let mut from = File::open(src).map_err(|e| os_fail(e, src))?;
            let mut to = create(dst)?;
            buf.resize(4 << 20, 0);
            loop {
                let n = match from.read(buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(os_fail(e, src)),
                };
                to.write_all(&buf[..n]).map_err(|e| os_fail(e, dst))?;
            }
            finish(&to, dst, *mtime)
        }
    }
}

/// The jobs handed to the writing side and not done yet, and what failed there.
struct Writing {
    next_id: std::sync::atomic::AtomicUsize,
    left: std::sync::Mutex<usize>,
    idle: std::sync::Condvar,
    failed: std::sync::Mutex<HashMap<usize, Fail>>,
}

type Jobs = std::sync::mpsc::SyncSender<Job>;

impl Writing {
    /// Hand over a job; returns its number, under which a failure is kept.
    fn send(&self, jobs: &Jobs, dst: String, write: Put) -> Result<usize, Fail> {
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *self.left.lock().unwrap() += 1;
        if jobs.send(Job { id, dst, write }).is_err() {
            self.done(id, Ok(()));
            return Err(crash!("the writing side of tree-copy stopped"));
        }
        Ok(id)
    }
    fn done(&self, id: usize, result: Result<(), Fail>) {
        if let Err(e) = result {
            self.failed.lock().unwrap().insert(id, e);
        }
        let mut left = self.left.lock().unwrap();
        *left -= 1;
        if *left == 0 {
            self.idle.notify_all();
        }
    }
    /// Wait until every job handed over is done: the folders of a level exist before the level below is copied.
    fn wait(&self) {
        let mut left = self.left.lock().unwrap();
        while *left > 0 {
            left = self.idle.wait(left).unwrap();
        }
    }
}

/// What tree-copy did with one entry of a folder.
enum Found {
    /// Read, and handed to the writing side as job `.1`, with what the meta file records.
    Item(Item, usize),
    /// A socket, a fifo or a device: nothing a seed carries.
    Skipped(String),
}

/// Read the entries of SRC/rel_dir and hand them to the writing side: what Python's loop does for one folder,
/// the entries in parallel. The outer error is the folder's (it cannot be listed), the inner ones the entries'.
fn copy_folder(src: &str, dst: &str, rel_dir: &str, writing: &Writing, jobs: &Jobs) -> Result<Vec<Result<Found, Fail>>, Fail> {
    let names = sorted_names(&py::join(src, rel_dir))?;
    Ok(names
        .par_iter()
        .map(|name| {
            let rel = py::join(rel_dir, name);
            let (s, d) = (py::join(src, &rel), py::join(dst, &rel));
            let st = fs::symlink_metadata(&s).map_err(|e| os_fail(e, &s))?;
            let x = xattrs(&s)?;
            let mtime = py::int_mtime(st.mtime(), st.mtime_nsec());
            let (kind, write) = match st.mode() & S_IFMT {
                S_IFDIR => ('d', Put::Dir),
                S_IFLNK => ('l', Put::Link(fs::read_link(&s).map_err(|e| os_fail(e, &s))?)),
                S_IFREG if st.size() <= SMALL_FILE => {
                    let mut data = Vec::with_capacity(st.size() as usize + 1);
                    File::open(&s).and_then(|mut f| f.read_to_end(&mut data)).map_err(|e| os_fail(e, &s))?;
                    ('f', Put::Small(data, mtime))
                }
                S_IFREG => ('f', Put::Large(s, mtime)),
                _ => return Ok(Found::Skipped(rel)),
            };
            let job = writing.send(jobs, d, write)?;
            Ok(Found::Item(Item { rel, kind, mode: st.mode() & 0o7777, uid: st.uid(), gid: st.gid(), mtime, x }, job))
        })
        .collect())
}

/// `tree_copy(src, dst, meta_path, owners_from, owners_path)`: copy SRC to DST as the calling user and record
/// what a non-root copy loses: owner, mode, time, xattrs. Returns (entries, skipped special files).
///
/// owners_from: the raw APFS image SRC is mounted from, with owners ignored (macOS without root): the owners
/// are read from its records below owners_path (ApfsTree), and every entry's type, mode and xattr names must
/// agree with what the mount showed.
pub fn tree_copy(
    src: &str,
    dst: &str,
    meta_path: &str,
    owners_from: Option<&str>,
    owners_path: &str,
) -> Result<(usize, Vec<String>), Fail> {
    if fs::metadata(dst).is_ok() && fs::read_dir(dst).map_err(|e| os_fail(e, dst))?.next().is_some() {
        return Err(err!("{dst}: not empty"));
    }
    fs::create_dir_all(dst).map_err(|e| os_fail(e, dst))?;
    let top = fs::symlink_metadata(src).map_err(|e| os_fail(e, src))?;
    let mut top = Item {
        rel: ".".into(),
        kind: 'd',
        mode: top.mode() & 0o7777,
        uid: top.uid(),
        gid: top.gid(),
        mtime: py::int_mtime(top.mtime(), top.mtime_nsec()),
        x: Vec::new(),
    };
    let writers: usize = std::env::var("ONEIOS_IMAGE_WRITERS").ok().and_then(|v| v.parse().ok()).filter(|n| *n > 0).unwrap_or(WRITERS);
    let (jobs, taken) = std::sync::mpsc::sync_channel::<Job>(64);
    let taken = std::sync::Mutex::new(taken);
    let writing = Writing {
        next_id: std::sync::atomic::AtomicUsize::new(0),
        left: std::sync::Mutex::new(0),
        idle: std::sync::Condvar::new(),
        failed: std::sync::Mutex::new(HashMap::new()),
    };
    let (writing, taken) = (&writing, &taken);

    std::thread::scope(|scope| {
        // The image's records do not depend on the copy: they are read while it runs.
        let records = owners_from.map(|image| {
            scope.spawn(move || -> Result<Vec<(String, Rec)>, Fail> { ApfsTree::open(image)?.walk(owners_path) })
        });

        let mut laps = crate::Laps::start();
        for _ in 0..writers {
            scope.spawn(move || {
                let mut buf = Vec::new();
                loop {
                    let job = taken.lock().unwrap().recv();
                    match job {
                        Ok(job) => writing.done(job.id, write_entry(&job, &mut buf)),
                        Err(_) => break,
                    }
                }
            });
        }

        // The copy goes level by level: the folders of a level in parallel, and in a folder its entries in
        // parallel; the folders of a level are made before the level below it is read. `folders` keeps what
        // each one held, in sorted order, for the pass after it that puts the entries in Python's order.
        struct Folder {
            rel: String,
            held: Option<Result<Vec<Result<Found, Fail>>, Fail>>,
            /// For each entry of `held` that is a folder: its place in `folders`.
            below: Vec<usize>,
        }
        let mut folders = vec![Folder { rel: String::new(), held: None, below: Vec::new() }];
        let mut level = vec![0usize];
        while !level.is_empty() {
            let read: Vec<Result<Vec<Result<Found, Fail>>, Fail>> =
                level.par_iter().map(|i| copy_folder(src, dst, &folders[*i].rel, writing, &jobs)).collect();
            writing.wait();
            let mut next = Vec::new();
            for (i, held) in level.iter().zip(read) {
                if let Ok(held) = &held {
                    for found in held {
                        if let Ok(Found::Item(item, job)) = found {
                            if item.kind == 'd' && !writing.failed.lock().unwrap().contains_key(job) {
                                let at = folders.len();
                                next.push(at);
                                folders[*i].below.push(at);
                                folders.push(Folder { rel: item.rel.clone(), held: None, below: Vec::new() });
                            }
                        }
                    }
                }
                folders[*i].held = Some(held);
            }
            level = next;
        }
        drop(jobs); // no more jobs: the writing threads end
        // Python's order: a stack of folders, so the last folder of a folder comes first; and its first error.
        let mut failed = std::mem::take(&mut *writing.failed.lock().unwrap());
        let mut items: Vec<Item> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            for found in folders[i].held.take().expect("every folder was read")? {
                match found? {
                    Found::Skipped(rel) => skipped.push(rel),
                    Found::Item(item, job) => {
                        if let Some(e) = failed.remove(&job) {
                            return Err(e);
                        }
                        items.push(item);
                    }
                }
            }
            stack.extend(folders[i].below.iter().copied());
        }
        laps.lap("tree-copy: copy the tree");

        if let Some(records) = records {
            let image = owners_from.unwrap();
            let records = records.join().map_err(|_| crash!("{image}: reading the records failed"))??;
            owners_from_image(&mut top, &mut items, &skipped, &records, image, owners_path)?;
            laps.lap("tree-copy: wait for the image's records, check the owners");
        }

        let mut entries: Vec<(String, Value)> = Vec::with_capacity(items.len() + 1);
        entries.push((".".into(), top.value(true)));
        entries.extend(items.iter().map(|item| (item.rel.clone(), item.value(false))));
        let meta = Value::Dict(vec![
            ("entries".into(), Value::Dict(entries)),
            ("skipped".into(), Value::List(skipped.iter().map(Value::str).collect())),
        ]);
        fs::write(meta_path, pyjson::dumps(&meta, None, false)).map_err(|e| os_fail(e, meta_path))?;
        Ok((items.len(), skipped))
    })
}

/// `_owners_from_image`: the owners of the image's records replace the mount's (the top entry's too), after every
/// entry's type, mode and xattr names were found equal.
fn owners_from_image(
    top: &mut Item,
    items: &mut [Item],
    skipped: &[String],
    walked: &[(String, Rec)],
    image: &str,
    subdir: &str,
) -> Result<(), Fail> {
    let records = apfs::index(walked);
    let check = |item: &Item| -> Result<(u32, u32), Fail> {
        let rel = &item.rel;
        let Some(rec) = records.get(rel.as_str()) else {
            return Err(err!("{image}: /{subdir}/{rel} is mounted but not in the volume's records"));
        };
        let kind = match item.kind {
            'd' => S_IFDIR,
            'l' => S_IFLNK,
            _ => S_IFREG,
        };
        let m = rec.m as u32;
        if m & S_IFMT != kind || m & 0o7777 != item.mode {
            return Err(err!("{image}: /{subdir}/{rel}: mode {m:o} in the records, {:o} mounted", kind | item.mode));
        }
        let mut shown: Vec<&str> = item.x.iter().map(|(k, _)| k.as_str()).collect();
        shown.sort_unstable();
        let kept: Vec<&str> = rec.x.iter().map(String::as_str).filter(|x| !DROP_XATTRS.contains(x)).collect();
        if kept != shown {
            return Err(err!(
                "{image}: /{subdir}/{rel}: xattrs {} in the records, {} mounted",
                py::repr_strs(&rec.x),
                py::repr_strs(&shown)
            ));
        }
        Ok((rec.u, rec.g))
    };
    (top.uid, top.gid) = check(top)?;
    for item in items.iter_mut() {
        let (u, g) = check(item)?;
        item.uid = u;
        item.gid = g;
    }
    let mut have: HashSet<&str> = items.iter().map(|i| i.rel.as_str()).collect();
    have.insert(".");
    have.extend(skipped.iter().map(String::as_str));
    let mut extra: Vec<&str> = records.keys().copied().filter(|rel| !have.contains(rel)).collect();
    if !extra.is_empty() {
        extra.sort_unstable();
        return Err(err!(
            "{image}: {} entries of /{subdir} are in the records but were not copied: {}",
            extra.len(),
            py::repr_strs(&extra[..extra.len().min(5)])
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------- seed-pack

pub struct Rule {
    rel: String,
    uid: i128,
    gid: i128,
    mode: Option<i128>,
    recursive: bool,
}

/// `read_owners(path)`: owner rules a hook wrote: tab-separated PATH, UID, GID, MODE (octal or -), optional R
/// (the whole tree below).
pub fn read_owners(path: Option<&str>) -> Result<Vec<Rule>, Fail> {
    let mut rules = Vec::new();
    let Some(path) = path.filter(|p| !p.is_empty() && fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)) else {
        return Ok(rules);
    };
    let raw = fs::read(path).map_err(|e| os_fail(e, path))?;
    let text = String::from_utf8(raw).map_err(|_| crash!("{path}: not UTF-8 text"))?;
    // Python reads text with universal newlines.
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    for (n, line) in lines.iter().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        let rule = (|| {
            if !(parts.len() == 4 || (parts.len() == 5 && parts[4] == "R")) {
                return None;
            }
            let stripped = parts[0].trim_matches('/');
            let rel = if stripped.is_empty() { ".".to_string() } else { py::normpath(stripped) };
            if rel.starts_with("..") {
                return None;
            }
            let mode = if parts[3] == "-" { None } else { Some(py::int(parts[3], 8)?) };
            Some(Rule { rel, uid: py::int(parts[1], 10)?, gid: py::int(parts[2], 10)?, mode, recursive: parts.len() == 5 })
        })();
        match rule {
            Some(rule) => rules.push(rule),
            None => return Err(err!("{path}:{}: want PATH<tab>UID<tab>GID<tab>MODE[<tab>R]", n + 1)),
        }
    }
    Ok(rules)
}

/// One folder of `walk_paths`: (its entries, the folders among them).
type Listed = Result<(Vec<String>, Vec<String>), Fail>;

/// Every entry below `tree` as `os.walk` finds them (a folder that cannot be read is skipped, a link to a folder
/// is listed and not followed), relative to `tree`.
fn walk_paths(tree: &str) -> Result<Vec<String>, Fail> {
    // One folder: (its entries, the folders among them). The folders of a level are listed in parallel.
    let list = |rel_dir: &String| -> Listed {
        let (mut all, mut folders) = (Vec::new(), Vec::new());
        let Ok(entries) = fs::read_dir(py::join(tree, rel_dir)) else { return Ok((all, folders)) };
        for e in entries {
            let Ok(e) = e else { break };
            let name = e.file_name();
            let name = std::str::from_utf8(name.as_bytes()).map_err(|_| not_utf8("the file name", name.as_bytes()))?;
            let rel = py::join(rel_dir, name);
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                folders.push(rel.clone());
            }
            all.push(rel);
        }
        Ok((all, folders))
    };
    let mut out = Vec::new();
    let mut level = vec![String::new()];
    while !level.is_empty() {
        let listed: Vec<Listed> = level.par_iter().map(list).collect();
        level = Vec::new();
        for l in listed {
            let (all, folders) = l?;
            out.extend(all);
            level.extend(folders);
        }
    }
    out.par_sort_unstable();
    Ok(out)
}

/// A member of the seed: its header, and for a file the path and the size the header names.
struct Member {
    header: Vec<u8>,
    file: Option<(String, u64)>,
}

impl Member {
    fn len(&self) -> usize {
        self.header.len() + self.file.as_ref().map_or(0, |(_, size)| (*size as usize).next_multiple_of(tar::BLOCK))
    }
}

/// Members whose files are read whole into one buffer, in parallel; a larger file is streamed by the writer.
const STREAM_FROM: u64 = 16 << 20;
const BATCH_BYTES: usize = 48 << 20;

enum Piece {
    Bytes(Vec<u8>),
    Stream(Vec<u8>, String, u64),
}

fn read_exact_file(path: &str, out: &mut [u8]) -> Result<(), Fail> {
    let mut f = File::open(path).map_err(|e| os_fail(e, path))?;
    f.read_exact(out).map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => crash!("{path}: unexpected end of data"),
        _ => os_fail(e, path),
    })
}

/// The bytes of a run of members: headers, contents, padding. The files are read in parallel, each into its place.
fn load(members: &[Member]) -> Result<Vec<u8>, Fail> {
    let mut buf = vec![0u8; members.iter().map(Member::len).sum()];
    let mut places: Vec<(&Member, &mut [u8])> = Vec::with_capacity(members.len());
    let mut rest = buf.as_mut_slice();
    for m in members {
        let (here, after) = rest.split_at_mut(m.len());
        places.push((m, here));
        rest = after;
    }
    let done: Vec<Result<(), Fail>> = places
        .into_par_iter()
        .map(|(m, place)| {
            let (head, body) = place.split_at_mut(m.header.len());
            head.copy_from_slice(&m.header);
            match &m.file {
                Some((path, size)) if *size > 0 => read_exact_file(path, &mut body[..*size as usize]),
                _ => Ok(()),
            }
        })
        .collect();
    for r in done {
        r?;
    }
    Ok(buf)
}

/// `seed_pack(tree, meta_path, out, epoch, owners)`: pack TREE as the Data seed: a deterministic pax tar whose
/// owners, modes and times are the source's.
///
/// An entry the copy made and nobody touched keeps the source's owner, mode and time. A new entry takes its
/// parent's owner, its own mode and the time `epoch`. A rewritten file keeps the source's owner and mode and
/// takes `epoch`. Owner rules override all of that. Returns (entries, added, changed).
pub fn seed_pack(tree: &str, meta_path: &str, out: &str, epoch: i128, owners: Option<&str>) -> Result<(usize, u64, u64), Fail> {
    let mut laps = crate::Laps::start();
    let meta = pyjson::load(meta_path).map_err(|e| crash!("{e:#}"))?;
    let Some(Value::Dict(meta)) = meta.get("entries") else {
        return Err(crash!("{meta_path}: no \"entries\" object"));
    };
    let meta: HashMap<&str, &Value> = meta.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let rules = read_owners(owners)?;
    let exact: HashMap<&str, &Rule> = rules.iter().map(|r| (r.rel.as_str(), r)).collect();
    let mut recursive: Vec<&Rule> = rules.iter().filter(|r| r.recursive).collect();
    recursive.sort_by_key(|r| r.rel.chars().count());
    laps.lap("seed-pack: read the meta file and the rules");
    let paths = walk_paths(tree)?;
    laps.lap("seed-pack: list the tree");

    let field = |rel: &str, old: &Value, key: &str| -> Result<i128, Fail> {
        old.get(key).and_then(py::as_int).ok_or_else(|| crash!("{meta_path}: \"{key}\" of {} is not a whole number", py::repr_str(rel)))
    };
    // What the file system says about each path (in parallel), then the members in order: an entry's owner
    // can come from its folder, which is earlier in the list.
    let stats: Vec<Result<(fs::Metadata, Option<String>), Fail>> = paths
        .par_iter()
        .map(|rel| {
            let full = py::join(tree, rel);
            let st = fs::symlink_metadata(&full).map_err(|e| os_fail(e, &full))?;
            let target = if st.file_type().is_symlink() {
                let t = fs::read_link(&full).map_err(|e| os_fail(e, &full))?;
                Some(String::from_utf8(t.as_os_str().as_bytes().to_vec()).map_err(|e| not_utf8("the link target", e.as_bytes()))?)
            } else {
                None
            };
            Ok((st, target))
        })
        .collect();

    laps.lap("seed-pack: lstat and readlink");
    let top = meta.get(".");
    let top_owner = |key: &str| -> Result<i128, Fail> {
        match top {
            Some(v) => field(".", v, key),
            None => Ok(0),
        }
    };
    let mut owner: HashMap<&str, (i128, i128)> = HashMap::with_capacity(paths.len() + 1);
    owner.insert(".", (top_owner("u")?, top_owner("g")?));
    let (mut added, mut changed) = (0u64, 0u64);
    let mut members: Vec<Member> = Vec::with_capacity(paths.len());
    for (rel, stat) in paths.iter().zip(stats) {
        let (st, target) = stat?;
        let kind = match st.mode() & S_IFMT {
            S_IFDIR => 'd',
            S_IFLNK => 'l',
            S_IFREG => 'f',
            _ => return Err(err!("{rel}: only files, folders and symbolic links can be in the seed")),
        };
        let old = meta.get(rel.as_str()).filter(|old| py::truthy(Some(old)));
        let same_kind = match old {
            Some(old) => match old.get("t") {
                Some(t) => t.as_str() == Some(kind.to_string().as_str()),
                None => return Err(crash!("{meta_path}: no \"t\" of {}", py::repr_str(rel))),
            },
            None => false,
        };
        let (mut uid, mut gid, mut mode, mtime);
        let mut xattrs: Vec<(String, String)> = Vec::new();
        if let (Some(old), true) = (old, same_kind) {
            (uid, gid, mode) = (field(rel, old, "u")?, field(rel, old, "g")?, field(rel, old, "m")?);
            let s = field(rel, old, "s")?;
            if kind == 'f' && py::int_mtime(st.mtime(), st.mtime_nsec()) as i128 != s {
                mtime = epoch;
                changed += 1;
            } else {
                mtime = s;
            }
            match old.get("x") {
                None => {}
                Some(Value::Dict(x)) => {
                    for (k, v) in x {
                        let Some(v) = v.as_str() else {
                            return Err(crash!("{meta_path}: an xattr of {} is not text", py::repr_str(rel)));
                        };
                        xattrs.push((k.clone(), v.to_string()));
                    }
                    xattrs.sort();
                }
                Some(_) => return Err(crash!("{meta_path}: \"x\" of {} is not an object", py::repr_str(rel))),
            }
        } else {
            let parent = py::dirname(rel);
            (uid, gid) = *owner
                .get(if parent.is_empty() { "." } else { parent })
                .ok_or_else(|| crash!("{rel}: its folder is not in the tree"))?;
            mode = (st.mode() & 0o7777) as i128;
            mtime = epoch;
            added += 1;
        }
        for r in &recursive {
            if *rel == r.rel || (rel.starts_with(r.rel.as_str()) && rel.as_bytes().get(r.rel.len()) == Some(&b'/')) || r.rel == "." {
                (uid, gid) = (r.uid, r.gid);
                if let (Some(m), true) = (r.mode, kind != 'l') {
                    mode = m;
                }
            }
        }
        if let Some(r) = exact.get(rel.as_str()) {
            (uid, gid) = (r.uid, r.gid);
            if let Some(m) = r.mode {
                mode = m;
            }
        }
        owner.insert(rel.as_str(), (uid, gid));
        let target = target.unwrap_or_default();
        let mut info = TarInfo::new(rel);
        (info.uid, info.gid, info.mtime) = (uid, gid, mtime);
        info.mode = if kind == 'l' { 0o777 } else { mode };
        info.pax = xattrs.iter().map(|(k, v)| (format!("LIBARCHIVE.xattr.{}", py::url_quote(k)), v.clone())).collect();
        let mut file = None;
        match kind {
            'd' => info.kind = tar::DIR,
            'l' => {
                info.kind = tar::SYM;
                info.linkname = &target;
            }
            _ => {
                info.size = st.size() as i128;
                file = Some((py::join(tree, rel), st.size()));
            }
        }
        members.push(Member { header: info.header(), file });
    }

    laps.lap("seed-pack: the headers");
    // One thread reads (runs of small files in parallel), this one writes, so the disk is read while it is written.
    let mut o = File::create(out).map_err(|e| os_fail(e, out))?;
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Piece, Fail>>(2);
    let members = &members;
    let offset = std::thread::scope(|scope| -> Result<u64, Fail> {
        scope.spawn(move || {
            let mut at = 0;
            while at < members.len() {
                let m = &members[at];
                let piece = match &m.file {
                    Some((path, size)) if *size >= STREAM_FROM => {
                        at += 1;
                        Ok(Piece::Stream(m.header.clone(), path.clone(), *size))
                    }
                    _ => {
                        let start = at;
                        let mut bytes = 0;
                        while at < members.len()
                            && !matches!(&members[at].file, Some((_, size)) if *size >= STREAM_FROM)
                            && (at == start || bytes + members[at].len() <= BATCH_BYTES)
                        {
                            bytes += members[at].len();
                            at += 1;
                        }
                        load(&members[start..at]).map(Piece::Bytes)
                    }
                };
                let failed = piece.is_err();
                if tx.send(piece).is_err() || failed {
                    break;
                }
            }
        });
        let mut offset = 0u64;
        for piece in rx {
            match piece? {
                Piece::Bytes(bytes) => {
                    o.write_all(&bytes).map_err(|e| os_fail(e, out))?;
                    offset += bytes.len() as u64;
                }
                Piece::Stream(header, path, size) => {
                    o.write_all(&header).map_err(|e| os_fail(e, out))?;
                    let mut f = File::open(&path).map_err(|e| os_fail(e, &path))?;
                    let mut buf = vec![0u8; 4 << 20];
                    let mut left = size;
                    while left > 0 {
                        let want = (left as usize).min(buf.len());
                        match f.read(&mut buf[..want]) {
                            Ok(0) => return Err(crash!("{path}: unexpected end of data")),
                            Ok(n) => {
                                o.write_all(&buf[..n]).map_err(|e| os_fail(e, out))?;
                                left -= n as u64;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                            Err(e) => return Err(os_fail(e, &path)),
                        }
                    }
                    let padded = size.next_multiple_of(tar::BLOCK as u64);
                    o.write_all(&vec![0u8; (padded - size) as usize]).map_err(|e| os_fail(e, out))?;
                    offset += header.len() as u64 + padded;
                }
            }
        }
        Ok(offset)
    })?;
    o.write_all(&tar::trailer(offset)).map_err(|e| os_fail(e, out))?;
    laps.lap("seed-pack: read the files, write the tar");
    Ok((paths.len(), added, changed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{fake_apfs_tree, python_seed_sha256, Tmp};
    use std::os::unix::fs::symlink;

    fn set_mtime(path: &str, sec: i64) {
        let f = File::open(path).unwrap();
        let times = [libc::timespec { tv_sec: sec as libc::time_t, tv_nsec: 0 }; 2];
        assert_eq!(unsafe { libc::futimens(f.as_raw_fd(), times.as_ptr()) }, 0);
    }

    /// What an OS volume's /private/var looks like to the build (scripts/image/tests/fixtures.py fake_os_root).
    fn fake_var(tmp: &Tmp) -> String {
        let var = tmp.path("os/private/var");
        fs::create_dir_all(format!("{var}/mobile/Library/Preferences")).unwrap();
        fs::create_dir_all(format!("{var}/root")).unwrap();
        fs::create_dir_all(format!("{var}/empty")).unwrap();
        for (rel, data) in [
            ("mobile/Library/Preferences/com.example.kept.plist", "kept: true\n"),
            ("mobile/Library/Preferences/com.example.setup.plist", "setup: false\n"),
            ("root/.profile", "# fixture\n"),
        ] {
            fs::write(format!("{var}/{rel}"), data).unwrap();
            fs::set_permissions(format!("{var}/{rel}"), fs::Permissions::from_mode(0o644)).unwrap();
            set_mtime(&format!("{var}/{rel}"), 1600000000);
        }
        for rel in ["mobile", "mobile/Library", "mobile/Library/Preferences", "root", "empty", ""] {
            fs::set_permissions(format!("{var}/{rel}"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        symlink("mobile/Library", format!("{var}/link")).unwrap();
        var
    }

    fn entries(meta: &Value) -> Vec<&str> {
        meta.get("entries").unwrap().as_dict().unwrap().iter().map(|(k, _)| k.as_str()).collect()
    }

    /// macOS tags the files some processes make with the xattr com.apple.provenance (a test's files can have it,
    /// a volume's files do not): the entry without it.
    fn plain(entry: &Value) -> Value {
        let Value::Dict(items) = entry else { return entry.clone() };
        let mut out = Vec::new();
        for (k, v) in items {
            if k == "x" {
                let x: Vec<(String, Value)> = v.as_dict().unwrap().iter().filter(|(name, _)| name != "com.apple.provenance").cloned().collect();
                if !x.is_empty() {
                    out.push((k.clone(), Value::Dict(x)));
                }
            } else {
                out.push((k.clone(), v.clone()));
            }
        }
        Value::Dict(out)
    }

    #[test]
    fn copy_records_the_tree() {
        let tmp = Tmp::new("tree-copy");
        let var = fake_var(&tmp);
        let (count, skipped) = tree_copy(&var, &tmp.path("data"), &tmp.path("meta.json"), None, "").unwrap();
        assert_eq!((count, skipped.len()), (9, 0));
        let text = fs::read_to_string(tmp.path("meta.json")).unwrap();
        let meta = pyjson::parse(&text).unwrap();
        // Python's order: a folder's entries sorted, then the folders below it, the last one first
        assert_eq!(
            entries(&meta),
            [
                ".",
                "empty",
                "link",
                "mobile",
                "root",
                "root/.profile",
                "mobile/Library",
                "mobile/Library/Preferences",
                "mobile/Library/Preferences/com.example.kept.plist",
                "mobile/Library/Preferences/com.example.setup.plist"
            ]
        );
        let e = meta.get("entries").unwrap();
        let me = unsafe { (libc::getuid(), libc::getgid()) };
        let top_gid = fs::metadata(&var).unwrap().gid();
        assert_eq!(
            pyjson::dumps(e.get(".").unwrap(), None, false),
            format!("{{\"t\": \"d\", \"m\": 493, \"u\": {}, \"g\": {top_gid}, \"s\": {}}}", me.0, fs::metadata(&var).unwrap().mtime())
        );
        let kept = e.get("mobile/Library/Preferences/com.example.kept.plist").unwrap();
        let gid = fs::metadata(format!("{var}/mobile/Library/Preferences/com.example.kept.plist")).unwrap().gid();
        assert_eq!(
            pyjson::dumps(&plain(kept), None, false),
            format!("{{\"m\": 420, \"u\": {}, \"g\": {gid}, \"s\": 1600000000, \"t\": \"f\"}}", me.0)
        );
        assert_eq!(e.get("link").unwrap().get("t").unwrap().as_str(), Some("l"));
        assert!(text.ends_with(", \"skipped\": []}"));
        // the copy: the content, mode 600 and the source's time for files, mode 700 for folders, the link itself
        let copied = tmp.path("data/mobile/Library/Preferences/com.example.kept.plist");
        assert_eq!(fs::read(&copied).unwrap(), b"kept: true\n");
        let st = fs::metadata(&copied).unwrap();
        assert_eq!((st.mode() & 0o7777, st.mtime(), st.mtime_nsec()), (0o600, 1600000000, 0));
        assert_eq!(fs::metadata(tmp.path("data/mobile")).unwrap().mode() & 0o7777, 0o700);
        assert_eq!(fs::read_link(tmp.path("data/link")).unwrap().to_str(), Some("mobile/Library"));
        assert!(fs::read_dir(tmp.path("data/empty")).unwrap().next().is_none());

        match tree_copy(&var, &tmp.path("data"), &tmp.path("meta2.json"), None, "") {
            Err(Fail::Error(m)) => assert!(m.ends_with("data: not empty"), "{m}"),
            _ => panic!("a copy into a full folder passed"),
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    #[test]
    fn copy_records_xattrs_and_skips_special_files() {
        let tmp = Tmp::new("tree-copy-x");
        let var = fake_var(&tmp);
        let tagged = format!("{var}/root/.profile");
        let (name, dropped) = if cfg!(target_os = "macos") {
            ("com.example.tag", "com.apple.ResourceFork")
        } else {
            ("user.com.example.tag", "user.com.apple.ResourceFork")
        };
        let set = |name: &str, value: &[u8]| {
            let (p, n) = (CString::new(tagged.as_str()).unwrap(), CString::new(name).unwrap());
            #[cfg(target_os = "macos")]
            let r = unsafe { libc::setxattr(p.as_ptr(), n.as_ptr(), value.as_ptr().cast(), value.len(), 0, 0) };
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let r = unsafe { libc::setxattr(p.as_ptr(), n.as_ptr(), value.as_ptr().cast(), value.len(), 0) };
            r == 0
        };
        if !set(name, b"\0\x01\x02") || !set(dropped, b"fork") {
            eprintln!("this file system has no xattrs: not tested");
            return;
        }
        let fifo = CString::new(format!("{var}/root/pipe")).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let (count, skipped) = tree_copy(&var, &tmp.path("data"), &tmp.path("meta.json"), None, "").unwrap();
        assert_eq!((count, skipped), (9, vec!["root/pipe".to_string()]));
        let meta = pyjson::load(tmp.path("meta.json")).unwrap();
        let profile = plain(meta.get("entries").unwrap().get("root/.profile").unwrap());
        assert_eq!(pyjson::dumps(profile.get("x").unwrap(), None, false), format!("{{\"{name}\": \"AAEC\"}}"));
        assert!(!fs::exists(tmp.path("data/root/pipe")).unwrap());
        assert_eq!(pyjson::dumps(meta.get("skipped").unwrap(), None, false), "[\"root/pipe\"]");
    }

    #[test]
    fn owners_come_from_the_image() {
        let tmp = Tmp::new("tree-copy-owners");
        // the mounted tree, as a user sees it, and an image whose records name the real owners
        let var = tmp.path("mnt/private/var");
        fs::create_dir_all(format!("{var}/mobile")).unwrap();
        fs::create_dir_all(format!("{var}/db")).unwrap();
        fs::write(format!("{var}/mobile/a.plist"), "a").unwrap();
        symlink("x", format!("{var}/db/l")).unwrap();
        fs::set_permissions(&var, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(format!("{var}/mobile"), fs::Permissions::from_mode(0o750)).unwrap();
        fs::set_permissions(format!("{var}/db"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(format!("{var}/mobile/a.plist"), fs::Permissions::from_mode(0o600)).unwrap();
        let link_mode = (fs::symlink_metadata(format!("{var}/db/l")).unwrap().mode() & 0o7777) as u16;
        // The records name the xattrs the test's own files have (macOS can tag them, see `plain`), and one that
        // only describes how APFS stores the file.
        let shown = |rel: &str| -> Vec<String> { xattrs(&format!("{var}/{rel}")).unwrap().into_iter().map(|(k, _)| k).collect() };
        let (x_mobile, mut x_plist, x_db, x_l) = (shown("mobile"), shown("mobile/a.plist"), shown("db"), shown("db/l"));
        x_plist.push("com.apple.decmpfs".into());
        fn names(x: &[String]) -> Vec<&str> {
            x.iter().map(String::as_str).collect()
        }
        let tree = |plist_mode: u16, extra: bool| {
            let mut entries = vec![
                ("private", 0o40755, 0, 0, vec![]),
                ("private/var", 0o40755, 0, 0, vec![]),
                ("private/var/mobile", 0o40750, 501, 501, names(&x_mobile)),
                ("private/var/mobile/a.plist", 0o100000 | plist_mode, 501, 501, names(&x_plist)),
                ("private/var/db", 0o40700, 0, 0, names(&x_db)),
                ("private/var/db/l", 0o120000 | link_mode, 0, 80, names(&x_l)),
            ];
            if extra {
                entries.push(("private/var/db/more", 0o100644, 0, 0, vec![]));
            }
            let image = tmp.path(&format!("tree-{plist_mode:o}-{extra}.img"));
            fake_apfs_tree(&image, &entries, true);
            image
        };
        let image = tree(0o600, false);
        let (count, _) = tree_copy(&var, &tmp.path("data"), &tmp.path("meta.json"), Some(&image), "private/var").unwrap();
        assert_eq!(count, 4);
        let meta = pyjson::load(tmp.path("meta.json")).unwrap();
        let e = meta.get("entries").unwrap();
        let owner = |rel: &str| (e.get(rel).unwrap().get("u").unwrap().as_int().unwrap(), e.get(rel).unwrap().get("g").unwrap().as_int().unwrap());
        assert_eq!((owner("mobile"), owner("mobile/a.plist"), owner("db/l"), owner("db")), ((501, 501), (501, 501), (0, 80), (0, 0)));
        assert_eq!(owner("."), (0, 0));

        for (image, word, n) in [
            (tree(0o644, false), "/private/var/mobile/a.plist: mode 100644 in the records, 100600 mounted", 2),
            (tree(0o600, true), "1 entries of /private/var are in the records but were not copied: ['db/more']", 3),
        ] {
            match tree_copy(&var, &tmp.path(&format!("data{n}")), &tmp.path("m.json"), Some(&image), "private/var") {
                Err(Fail::Error(m)) => assert!(m.ends_with(word), "{m}"),
                _ => panic!("{word}: passed"),
            }
        }
        fs::write(format!("{var}/db/new"), "n").unwrap();
        match tree_copy(&var, &tmp.path("data4"), &tmp.path("m.json"), Some(&image), "private/var") {
            Err(Fail::Error(m)) => assert!(m.ends_with("/private/var/db/new is mounted but not in the volume's records"), "{m}"),
            _ => panic!("an entry without a record passed"),
        }
    }

    /// The seed of scripts/image/tests/test_image.py SeedTest.build: the copy, stand-in owners, then what a hook
    /// does. Returns (stats, the tar's bytes).
    fn build(tmp: &Tmp, owners: &str) -> ((usize, u64, u64), Vec<u8>) {
        let var = fake_var(tmp);
        tree_copy(&var, &tmp.path("data"), &tmp.path("meta.json"), None, "").unwrap();
        let mut meta = pyjson::load(tmp.path("meta.json")).unwrap();
        let Some(Value::Dict(entries)) = meta.get_mut("entries") else { panic!() };
        for (rel, e) in entries.iter_mut() {
            *e = plain(e);
            let id = if rel.starts_with("mobile") { 501 } else { 0 };
            e.set("u", id);
            e.set("g", id);
            e.set("s", 1600000000);
            if rel.ends_with("kept.plist") {
                let mut x = Value::dict();
                x.set("com.example.tag", "AAEC");
                x.set("com.apple.a b/c", "dg==");
                e.set("x", x);
            }
        }
        fs::write(tmp.path("meta.json"), pyjson::dumps(&meta, None, false)).unwrap();
        let prefs = tmp.path("data/mobile/Library/Preferences");
        fs::write(format!("{prefs}/com.example.setup.plist"), "setup: true\n").unwrap();
        fs::write(format!("{prefs}/com.example.new.plist"), "new: 1\n").unwrap();
        fs::set_permissions(format!("{prefs}/com.example.new.plist"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::create_dir_all(tmp.path("data/oneios/tools")).unwrap();
        for d in ["data/oneios", "data/oneios/tools"] {
            fs::set_permissions(tmp.path(d), fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(tmp.path("data/oneios/tools/probe"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(tmp.path("data/oneios/tools/probe"), fs::Permissions::from_mode(0o755)).unwrap();
        // a name that is not ASCII and one longer than a ustar header holds
        fs::write(tmp.path(&format!("data/oneios/caf{}.txt", '\u{e9}')), "x").unwrap();
        fs::set_permissions(tmp.path(&format!("data/oneios/caf{}.txt", '\u{e9}')), fs::Permissions::from_mode(0o644)).unwrap();
        let long = format!("data/oneios/{}", "n".repeat(120));
        fs::write(tmp.path(&long), vec![7u8; 1500]).unwrap();
        fs::set_permissions(tmp.path(&long), fs::Permissions::from_mode(0o644)).unwrap();
        fs::remove_file(tmp.path("data/root/.profile")).unwrap();
        fs::write(tmp.path("owners.tsv"), owners).unwrap();
        let out = tmp.path("data.seed");
        let stats = seed_pack(&tmp.path("data"), &tmp.path("meta.json"), &out, 1700000000, Some(&tmp.path("owners.tsv"))).unwrap();
        (stats, fs::read(out).unwrap())
    }

    /// (name, mode, uid, gid, size, mtime, type, linkname) of each ustar header, and the pax records before it.
    #[allow(clippy::type_complexity)]
    fn members(tar: &[u8]) -> Vec<(String, u32, u32, u32, u64, u64, u8, String, String)> {
        let oct = |b: &[u8]| u64::from_str_radix(std::str::from_utf8(b).unwrap().trim_end_matches('\0'), 8).unwrap();
        let text = |b: &[u8]| String::from_utf8(b.iter().copied().take_while(|c| *c != 0).collect()).unwrap();
        let mut out = Vec::new();
        let mut at = 0;
        let mut pax = String::new();
        while at + 512 <= tar.len() && tar[at] != 0 {
            let h = &tar[at..at + 512];
            let size = oct(&h[124..136]);
            let body = &tar[at + 512..at + 512 + size as usize];
            at += 512 + (size as usize).next_multiple_of(512);
            if h[156] == b'x' {
                pax = String::from_utf8(body.to_vec()).unwrap();
                continue;
            }
            out.push((text(&h[..100]), oct(&h[100..108]) as u32, oct(&h[108..116]) as u32, oct(&h[116..124]) as u32, size, oct(&h[136..148]), h[156], text(&h[157..257]), std::mem::take(&mut pax)));
        }
        assert!(tar[at..].iter().all(|b| *b == 0));
        assert_eq!(tar.len() % 10240, 0);
        out
    }

    #[test]
    fn pack_owners_modes_times() {
        let tmp = Tmp::new("seed-pack");
        let (stats, tar) = build(&tmp, "");
        assert_eq!(stats, (14, 6, 1)); // entries, added, rewritten
        let m = members(&tar);
        let names: Vec<&str> = m.iter().map(|x| x.0.as_str()).collect();
        let long = "n".repeat(120);
        assert_eq!(
            names,
            [
                "empty/",
                "link",
                "mobile/",
                "mobile/Library/",
                "mobile/Library/Preferences/",
                "mobile/Library/Preferences/com.example.kept.plist",
                "mobile/Library/Preferences/com.example.new.plist",
                "mobile/Library/Preferences/com.example.setup.plist",
                "oneios/",
                "oneios/caf?.txt",
                &format!("oneios/{}", &long[..93]),
                "oneios/tools/",
                "oneios/tools/probe",
                "root/"
            ]
        );
        let by = |name: &str| m.iter().find(|x| x.0 == name).unwrap();
        let kept = by("mobile/Library/Preferences/com.example.kept.plist");
        assert_eq!((kept.1, kept.2, kept.3, kept.4, kept.5, kept.6), (0o644, 501, 501, 11, 1600000000, b'0'));
        assert_eq!(kept.8, "45 LIBARCHIVE.xattr.com.apple.a%20b%2Fc=dg==\n41 LIBARCHIVE.xattr.com.example.tag=AAEC\n");
        let changed = by("mobile/Library/Preferences/com.example.setup.plist");
        assert_eq!((changed.2, changed.5), (501, 1700000000));
        let new = by("mobile/Library/Preferences/com.example.new.plist");
        assert_eq!((new.1, new.2, new.3, new.5), (0o600, 501, 501, 1700000000)); // the owner of its folder
        let probe = by("oneios/tools/probe");
        assert_eq!((probe.1, probe.2, probe.3), (0o755, 0, 0));
        let link = by("link");
        assert_eq!((link.1, link.6, link.7.as_str()), (0o777, b'2', "mobile/Library"));
        assert_eq!((by("empty/").6, by("empty/").4), (b'5', 0));
        assert_eq!(by("oneios/caf?.txt").8, format!("25 path=oneios/caf{}.txt\n", '\u{e9}'));
        assert_eq!(by(&format!("oneios/{}", &long[..93])).8, format!("137 path=oneios/{long}\n"));
        assert!(!names.contains(&"root/.profile"));
    }

    #[test]
    fn pack_owner_rules() {
        let tmp = Tmp::new("seed-rules");
        let (_, tar) = build(&tmp, "oneios\t501\t501\t-\tR\noneios/tools/probe\t0\t0\t4755\n/empty/\t33\t33\t700\n# a comment\n\n");
        let m = members(&tar);
        let by = |name: &str| m.iter().find(|x| x.0 == name).unwrap();
        assert_eq!((by("oneios/").2, by("oneios/tools/").2), (501, 501));
        assert_eq!((by("oneios/tools/probe").2, by("oneios/tools/probe").1), (0, 0o4755));
        assert_eq!((by("empty/").2, by("empty/").3, by("empty/").1), (33, 33, 0o700));
        for (name, text) in [("bad.tsv", "a b c\n"), ("bad2.tsv", "../x\t0\t0\t-\n"), ("bad3.tsv", "x\t0\t0\t-\tQ\n"), ("bad4.tsv", "x\t0\t0\t9\n")] {
            let path = tmp.write(name, text.as_bytes());
            match read_owners(Some(&path)) {
                Err(Fail::Error(m)) => assert!(m.ends_with(":1: want PATH<tab>UID<tab>GID<tab>MODE[<tab>R]"), "{m}"),
                _ => panic!("{name} passed"),
            }
        }
        assert!(read_owners(Some(&tmp.path("no-such-file"))).unwrap().is_empty());
        assert!(read_owners(None).unwrap().is_empty());
    }

    #[test]
    fn pack_same_bytes_every_time_and_as_python() {
        let first = build(&Tmp::new("seed-same-1"), "oneios\t501\t501\t-\tR\n").1;
        let second = build(&Tmp::new("seed-same-2"), "oneios\t501\t501\t-\tR\n").1;
        assert_eq!(first.len(), second.len());
        assert!(first == second, "two packs of the same tree differ");
        // The same tree packed by scripts/image/imagetool.py (Python 3.15.2 tarfile) on 2026-11-12.
        assert_eq!(oneios_common::sha256_hex(&first), python_seed_sha256());
    }

    #[test]
    fn pack_streams_large_files() {
        let tmp = Tmp::new("seed-large");
        fs::create_dir_all(tmp.path("data/d")).unwrap();
        let big = crate::fixtures::noise(STREAM_FROM as usize + 12345, 9);
        fs::write(tmp.path("data/d/big"), &big).unwrap();
        fs::write(tmp.path("data/d/small"), b"s").unwrap();
        fs::write(tmp.path("data/a"), b"").unwrap();
        fs::write(tmp.path("meta.json"), "{\"entries\": {}}").unwrap();
        let out = tmp.path("out.tar");
        assert_eq!(seed_pack(&tmp.path("data"), &tmp.path("meta.json"), &out, 5, None).unwrap(), (4, 4, 0));
        let tar = fs::read(&out).unwrap();
        let m = members(&tar);
        assert_eq!(m.iter().map(|x| (x.0.as_str(), x.4)).collect::<Vec<_>>(), [("a", 0), ("d/", 0), ("d/big", big.len() as u64), ("d/small", 1)]);
        let at = 512 * 3;
        assert!(tar[at..at + big.len()] == big[..]);
        assert_eq!(&tar[at + big.len().next_multiple_of(512) + 512..][..1], b"s");
    }
}
