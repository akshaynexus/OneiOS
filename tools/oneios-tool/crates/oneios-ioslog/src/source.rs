//! Where the log files are: a raw image of the Data store (read with our APFS reader, no mount), a `.logarchive`
//! folder, or a folder that holds `db/diagnostics` (a mounted Data volume). One provider for all three, so that
//! the decoder sees the same files in the same order whatever the source is.
//!
//! The order of the tracev3 files and the choice of the string files are the ones of the library's own
//! `LogarchiveProvider` (macos-unifiedlogs `src/filesystem.rs`): `tests/ioslog/compare.sh` checks that by
//! comparing this tool's CSV with `unifiedlog_iterator`'s, byte for byte.

use oneios_image::apfs::fuse::{self, Backend};
use anyhow::{anyhow, bail, Context, Result};
use macos_unifiedlogs::chunks::{ChunkPreamble, ChunkTag, PREAMBLE_SIZE};
use macos_unifiedlogs::header::{RawHeaderChunk, RAW_HEADER_CHUNK_SIZE};
use macos_unifiedlogs::traits::{FileProvider, SourceFile};
use oneios_image::apfs::{ApfsTree, DT_DIR, DT_REG};
use std::collections::HashMap;
use std::io::{Error, ErrorKind, Read};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;

/// A file or a folder of the source.
#[derive(Clone, Debug)]
pub enum Node {
    Path(PathBuf),
    Object(u64),
}

/// One entry of a folder.
pub struct Entry {
    pub name: String,
    pub node: Node,
    pub dir: bool,
    pub file: bool,
}

/// What holds the files.
pub enum Store {
    Folder,
    Image(Box<ApfsTree>),
}

fn other(e: impl std::fmt::Display) -> Error {
    Error::other(e.to_string())
}

impl Store {
    /// The entries of a folder, sorted by name (bytes). A folder that is not there has none.
    pub fn list(&self, dir: &Node) -> Vec<Entry> {
        let mut out = Vec::new();
        match (self, dir) {
            (Store::Folder, Node::Path(path)) => {
                let Ok(entries) = std::fs::read_dir(path) else { return out };
                for e in entries.flatten() {
                    let path = e.path();
                    let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
                    out.push(Entry { name, dir: path.is_dir(), file: path.is_file(), node: Node::Path(path) });
                }
            }
            (Store::Image(tree), Node::Object(oid)) => {
                for e in tree.list(*oid).unwrap_or_default() {
                    out.push(Entry { name: e.name, node: Node::Object(e.id), dir: e.kind == DT_DIR, file: e.kind == DT_REG });
                }
            }
            _ => {}
        }
        out.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        out
    }

    fn child(&self, dir: &Node, name: &str) -> Option<Entry> {
        self.list(dir).into_iter().find(|e| e.name == name)
    }

    /// The folder at `path` below `dir` (`None`: not there).
    fn folder(&self, dir: &Node, path: &str) -> Option<Node> {
        let mut at = dir.clone();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let e = self.child(&at, part)?;
            if !e.dir {
                return None;
            }
            at = e.node;
        }
        Some(at)
    }

    pub fn read(&self, node: &Node) -> std::io::Result<Vec<u8>> {
        match (self, node) {
            (Store::Folder, Node::Path(path)) => std::fs::read(path),
            (Store::Image(tree), Node::Object(oid)) => tree.read_file(*oid).map_err(other),
            _ => Err(other("a node of another store")),
        }
    }

    /// The first `count` bytes of a file (fewer when it is shorter).
    fn head(&self, node: &Node, count: usize) -> std::io::Result<Vec<u8>> {
        match (self, node) {
            (Store::Folder, Node::Path(path)) => {
                let mut data = Vec::new();
                std::fs::File::open(path)?.take(count as u64).read_to_end(&mut data)?;
                Ok(data)
            }
            _ => {
                let mut data = self.read(node)?;
                data.truncate(count);
                Ok(data)
            }
        }
    }
}

/// A log file: what the decoder calls it, and where its bytes are.
pub struct Item {
    pub label: String,
    pub node: Node,
}

/// The log files of a source, in the decoder's order.
pub struct Source {
    pub store: Store,
    /// What the user named, for messages.
    pub what: String,
    /// `diagnostics` (or the archive's root) and `uuidtext` (or the archive's root).
    pub diagnostics: Node,
    pub uuidtext: Option<Node>,
    /// True when the source is laid out as a `.logarchive` (everything in one folder).
    pub archive: bool,
    pub tracev3: Vec<Item>,
    pub timesync: Vec<Item>,
    uuidtext_index: HashMap<Uuid, Node>,
    dsc_index: HashMap<Uuid, Node>,
    /// The apfs-fuse mount the files are read through (`--apfs fuse`); unmounted when the source is dropped.
    mount: Option<oneios_image::apfs::fuse::Mount>,
}

const TRACE_SUBDIRS: [&str; 4] = ["HighVolume", "Persist", "Signpost", "Special"];
/// The folders of a diagnostics folder that the manual copy took (docs/newtodo.md "How to resume" step 2).
pub const ARCHIVE_DIRS: [&str; 5] = ["Persist", "Special", "Signpost", "HighVolume", "timesync"];
const LIVE_DATA: &str = "logdata.LiveData.tracev3";

/// `Path::extension() == Some(ext)` for a file name.
fn has_extension(name: &str, ext: &str) -> bool {
    match name.rsplit_once('.') {
        Some((stem, e)) => !stem.is_empty() && e == ext,
        None => false,
    }
}

fn only_hex(text: &str) -> bool {
    text.chars().all(|c| c.is_ascii_hexdigit())
}

impl Source {
    /// Open `what`: a raw image (its volume chosen by `volume`: a number or a name; default: the first volume
    /// that has the log folders), or a folder. An image is read by our APFS reader, by apfs-fuse (`Backend::Fuse`,
    /// Linux), or by ours and then apfs-fuse when ours finds no log (`Backend::Auto`).
    pub fn open(what: &str, volume: Option<&str>, block0: bool, backend: Backend) -> Result<Source> {
        let meta = std::fs::metadata(what).with_context(|| what.to_string())?;
        if meta.is_dir() {
            let root = Node::Path(PathBuf::from(what));
            return Source::layout(Store::Folder, root, what, what.to_string());
        }
        match backend {
            Backend::Own => Source::open_image(what, volume, block0),
            Backend::Fuse => Source::open_mounted(what, volume),
            Backend::Auto => match Source::open_image(what, volume, block0) {
                Ok(source) => Ok(source),
                Err(e) if fuse::available() => {
                    eprintln!("{e:#}\nour APFS reader found no log; reading through apfs-fuse instead");
                    Source::open_mounted(what, volume)
                }
                Err(e) => Err(e),
            },
        }
    }

    /// The image through apfs-fuse: the volume asked for by its number, else the first of the first eight that
    /// mounts and holds a log.
    fn open_mounted(what: &str, volume: Option<&str>) -> Result<Source> {
        let volumes: Vec<usize> = match volume {
            None => (0..8).collect(),
            Some(v) => vec![v.parse().map_err(|_| anyhow!("--apfs fuse takes a volume's number, not its name (--volume {v})"))?],
        };
        let mut tried = Vec::new();
        for index in volumes {
            let mount = match fuse::Mount::new(what, index) {
                Ok(mount) => mount,
                Err(e) => {
                    tried.push(format!("volume {index}: {e}"));
                    continue;
                }
            };
            match Source::layout(Store::Folder, Node::Path(mount.root()), what, format!("{what} (volume {index}, through apfs-fuse)")) {
                Ok(mut source) => {
                    source.mount = Some(mount);
                    return Ok(source);
                }
                Err(e) => tried.push(format!("volume {index}: {e}")),
            }
        }
        bail!("{what}: no volume with a unified log through apfs-fuse:\n  {}", tried.join("\n  "))
    }

    fn open_image(what: &str, volume: Option<&str>, block0: bool) -> Result<Source> {
        let count = ApfsTree::volume_count(what).map_err(|e| anyhow!("{e}"))?;
        let open = |index: usize| ApfsTree::open_volume(what, index, !block0).map_err(|e| anyhow!("{e}"));
        let mut tried = Vec::new();
        for index in 0..count {
            let tree = match open(index) {
                Ok(tree) => tree,
                Err(e) => {
                    tried.push(format!("volume {index}: {e}"));
                    continue;
                }
            };
            let wanted = match volume {
                None => true,
                Some(v) => v.parse::<usize>().map(|n| n == index).unwrap_or(v == tree.name),
            };
            if !wanted {
                tried.push(format!("volume {index} ({}): not asked for", tree.name));
                continue;
            }
            let name = tree.name.clone();
            let label = format!("{what}:");
            match Source::layout(Store::Image(Box::new(tree)), Node::Object(2), &label, format!("{what} (volume {index}, {name})")) {
                Ok(source) => return Ok(source),
                Err(e) => tried.push(format!("volume {index} ({name}): {e}")),
            }
        }
        bail!("{what}: no volume with a unified log{}:\n  {}", volume.map(|v| format!(" (--volume {v})")).unwrap_or_default(), tried.join("\n  "))
    }

    /// Find the log folders below `root` and list the files.
    fn layout(store: Store, root: Node, label: &str, what: String) -> Result<Source> {
        let names: Vec<String> = store.list(&root).into_iter().map(|e| e.name).collect();
        let archive = names.iter().any(|n| n == LIVE_DATA || ARCHIVE_DIRS.contains(&n.as_str()));
        let (diagnostics, uuidtext, prefix) = if archive {
            (root.clone(), Some(root.clone()), String::new())
        } else {
            let mut found = None;
            for prefix in ["db", "private/var/db", "var/db", ""] {
                if let Some(diag) = store.folder(&root, &format!("{prefix}/diagnostics")) {
                    let has = store.list(&diag).iter().any(|e| e.name == LIVE_DATA || ARCHIVE_DIRS.contains(&e.name.as_str()));
                    if has {
                        found = Some((diag, store.folder(&root, &format!("{prefix}/uuidtext")), prefix.to_string()));
                        break;
                    }
                }
            }
            found.ok_or_else(|| anyhow!("no log folders (db/diagnostics, or a .logarchive's Persist, Special, ...)"))?
        };
        // The labels: the file's path as the library's providers would name it (a folder), or IMAGE:/path.
        let join = |dir: &str, name: &str| -> String {
            let mut p = match &store {
                Store::Folder => PathBuf::from(label),
                Store::Image(_) => PathBuf::from("/"),
            };
            for part in dir.split('/').filter(|s| !s.is_empty()) {
                p.push(part);
            }
            p.push(name);
            match &store {
                Store::Folder => p.to_string_lossy().into_owned(),
                Store::Image(_) => format!("{label}{}", p.to_string_lossy()),
            }
        };
        let diag_dir = if archive { String::new() } else { format!("{prefix}/diagnostics") };

        // tracev3: the four folders in this order, names sorted; then by the header's times (stable); LiveData last.
        let mut keyed: Vec<((u64, u64), Item)> = Vec::new();
        for sub in TRACE_SUBDIRS {
            let Some(dir) = store.folder(&diagnostics, sub) else { continue };
            for e in store.list(&dir) {
                if has_extension(&e.name, "tracev3") {
                    let key = sort_key(&store, &e.node).unwrap_or((u64::MAX, u64::MAX));
                    keyed.push((key, Item { label: join(&format!("{diag_dir}/{sub}"), &e.name), node: e.node }));
                }
            }
        }
        keyed.sort_by_key(|(key, _)| *key);
        let mut tracev3: Vec<Item> = keyed.into_iter().map(|(_, item)| item).collect();
        if let Some(e) = store.child(&diagnostics, LIVE_DATA).filter(|e| e.file) {
            tracev3.push(Item { label: join(&diag_dir, LIVE_DATA), node: e.node });
        }
        let mut timesync = Vec::new();
        if let Some(dir) = store.folder(&diagnostics, "timesync") {
            for e in store.list(&dir) {
                if has_extension(&e.name, "timesync") {
                    timesync.push(Item { label: join(&format!("{diag_dir}/timesync"), &e.name), node: e.node });
                }
            }
        }
        // uuidtext: XX/<30 hex digits>; dsc: dsc/<32 hex digits>.
        let mut uuidtext_index = HashMap::new();
        let mut dsc_index = HashMap::new();
        if let Some(root) = &uuidtext {
            for d in store.list(root) {
                if d.name.len() != 2 || !only_hex(&d.name) || !d.dir {
                    continue;
                }
                for f in store.list(&d.node) {
                    if !f.file {
                        continue;
                    }
                    if let Ok(uuid) = Uuid::parse_str(&format!("{}{}", d.name, f.name)) {
                        uuidtext_index.insert(uuid, f.node);
                    }
                }
            }
            if let Some(dsc) = store.folder(root, "dsc") {
                for f in store.list(&dsc) {
                    if let Ok(uuid) = Uuid::parse_str(&f.name) {
                        dsc_index.insert(uuid, f.node);
                    }
                }
            }
        }
        Ok(Source { store, what, diagnostics, uuidtext, archive, tracev3, timesync, uuidtext_index, dsc_index, mount: None })
    }

    pub fn string_files(&self) -> (usize, usize) {
        (self.uuidtext_index.len(), self.dsc_index.len())
    }

    /// Write the source's log files as a `.logarchive` folder: what the manual steps copied
    /// (`db/diagnostics/{Persist,Special,Signpost,HighVolume,timesync}` and `db/uuidtext/*`), plus
    /// `logdata.LiveData.tracev3` when the source has one. Returns (files, bytes).
    pub fn save(&self, out: &Path) -> Result<(u64, u64)> {
        std::fs::create_dir_all(out).with_context(|| out.display().to_string())?;
        let mut done = (0u64, 0u64);
        for sub in ARCHIVE_DIRS {
            if let Some(dir) = self.store.folder(&self.diagnostics, sub) {
                self.copy_dir(&dir, &out.join(sub), &mut done)?;
            }
        }
        if let Some(e) = self.store.child(&self.diagnostics, LIVE_DATA).filter(|e| e.file) {
            self.copy_file(&e.node, &out.join(LIVE_DATA), &mut done)?;
        }
        if let Some(root) = &self.uuidtext {
            for e in self.store.list(root) {
                // an archive's root holds the log folders too: they are copied above
                if self.archive && (ARCHIVE_DIRS.contains(&e.name.as_str()) || e.name == LIVE_DATA) {
                    continue;
                }
                if e.dir {
                    self.copy_dir(&e.node, &out.join(&e.name), &mut done)?;
                } else if e.file {
                    self.copy_file(&e.node, &out.join(&e.name), &mut done)?;
                }
            }
        }
        Ok(done)
    }

    fn copy_dir(&self, dir: &Node, out: &Path, done: &mut (u64, u64)) -> Result<()> {
        std::fs::create_dir_all(out).with_context(|| out.display().to_string())?;
        for e in self.store.list(dir) {
            if e.name.contains('/') || e.name == "." || e.name == ".." {
                bail!("a name in the source is not a file name: {:?}", e.name);
            }
            if e.dir {
                self.copy_dir(&e.node, &out.join(&e.name), done)?;
            } else if e.file {
                self.copy_file(&e.node, &out.join(&e.name), done)?;
            }
        }
        Ok(())
    }

    fn copy_file(&self, node: &Node, out: &Path, done: &mut (u64, u64)) -> Result<()> {
        let data = self.store.read(node).with_context(|| format!("reading the source's file for {}", out.display()))?;
        std::fs::write(out, &data).with_context(|| out.display().to_string())?;
        done.0 += 1;
        done.1 += data.len() as u64;
        Ok(())
    }
}

/// The library's sort key of a tracev3 file: the header chunk's wall-clock start and continuous time.
fn sort_key(store: &Store, node: &Node) -> Option<(u64, u64)> {
    let data = store.head(node, PREAMBLE_SIZE + RAW_HEADER_CHUNK_SIZE).ok()?;
    let (body, preamble) = ChunkPreamble::parse(&data).ok()?;
    if preamble.tag != ChunkTag::Header {
        return None;
    }
    let (_, header) = RawHeaderChunk::parse(body).ok()?;
    Some((header.unknown_time, header.continous_time))
}

/// One file handed to the decoder.
pub struct File<'a> {
    store: &'a Store,
    item: &'a Item,
}

impl SourceFile for File<'_> {
    fn read(&self) -> Result<Vec<u8>, Error> {
        self.store.read(&self.item.node)
    }
    fn source_path(&self) -> &str {
        &self.item.label
    }
}

fn sorted(index: &HashMap<Uuid, Node>) -> impl Iterator<Item = Uuid> {
    let mut uuids: Vec<Uuid> = index.keys().copied().collect();
    uuids.sort();
    uuids.into_iter()
}

impl FileProvider for Source {
    fn tracev3_files(&self) -> impl Iterator<Item = impl SourceFile> {
        self.tracev3.iter().map(|item| File { store: &self.store, item })
    }
    fn timesync_files(&self) -> impl Iterator<Item = impl SourceFile> {
        self.timesync.iter().map(|item| File { store: &self.store, item })
    }
    fn read_uuidtext(&self, uuid: &Uuid) -> Result<Vec<u8>, Error> {
        match self.uuidtext_index.get(uuid) {
            Some(node) => self.store.read(node),
            None => Err(Error::from(ErrorKind::NotFound)),
        }
    }
    fn read_dsc(&self, uuid: &Uuid) -> Result<Vec<u8>, Error> {
        match self.dsc_index.get(uuid) {
            Some(node) => self.store.read(node),
            None => Err(Error::from(ErrorKind::NotFound)),
        }
    }
    fn uuidtext_uuids(&self) -> impl Iterator<Item = Uuid> {
        sorted(&self.uuidtext_index)
    }
    fn dsc_uuids(&self) -> impl Iterator<Item = Uuid> {
        sorted(&self.dsc_index)
    }
}
