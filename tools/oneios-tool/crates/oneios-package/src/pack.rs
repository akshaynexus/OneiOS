//! Checks and manifest of the OneiOS runtime package (called by runtime/package.sh; was runtime/package/pack.py,
//! retired 2026-11-13).
//!
//!   package check STAGE              refuse Apple files and anything the OneOS fork's unpacker rejects
//!   package manifest STAGE VERSION GIT_COMMIT GIT_DIRTY MIN_FORK PLACEHOLDER   write STAGE/runtime.json
//!   package archive STAGE MEMBERS MTIME OUT.tar   a ustar archive of the files MEMBERS lists, in that order, owner
//!                                    0:0, time MTIME: what GNU tar --format=ustar --owner=0 --group=0
//!                                    --numeric-owner --mtime=@MTIME --no-recursion -T MEMBERS writes (package.sh
//!                                    uses it where tar is not GNU tar, as on macOS)
//!
//! The manifest is the one oneos/interface.md section 1 defines (format 1), with these keys added:
//! git, min_oneos_fork, placeholder, kernel.
//!
//! Apple rule (README "Rules"): a release carries nothing of Apple's. Refused by name: kernelcaches, cryptexes,
//! .ipsw, .dmg, .aea, .img and the other firmware names of .gitignore. Refused by content: IMG4/IM4P containers,
//! AEA archives, disk images, and every Mach-O. The one exception is the XNU that kernel/ builds from Apple's
//! open source: a Mach-O with a build record NAME.build.json next to it that names this builder, carries the
//! file's SHA-256, says no kernelcache kexts were added and lists no source whose licence forbids redistribution.
//! A kernel collection (MH_FILESET) is refused with or without a record: it holds Apple's kexts and belongs to the
//! image set.
//!
//! Every rule of the script is here in the script's order, with its message (`package: refused: ...`, status 3).
//! Where the script ended with a traceback instead (a Mach-O magic with no header behind it, a record that is no
//! JSON object: status 1, no package either), this refuses with a message of its own.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use oneios_common::pyjson::{self, Value};
use oneios_common::pyrepr::{repr_str, repr_value, str_value, truthy};
use oneios_common::pytar;

/// The firmware names (fnmatch patterns over the lower-case file name; `*` is the only wildcard in them).
const APPLE_NAMES: [&str; 19] = [
    "kernelcache*", "*cryptex*", "*.ipsw", "*.dmg", "*.aea", "*.img", "*.simg", "*.erofs", "*.im4p", "*.img4", "*.im4m", "*.kc", "*.trustcache",
    "buildmanifest.plist", "restore.plist", "*.shsh*", "dyld_shared_cache*", "*.kext", "*.dext",
];
/// Thin (both widths, both byte orders) and fat (both widths, both byte orders).
const MACHO_MAGICS: [[u8; 4]; 8] = [
    [0xcf, 0xfa, 0xed, 0xfe], [0xce, 0xfa, 0xed, 0xfe], [0xfe, 0xed, 0xfa, 0xcf], [0xfe, 0xed, 0xfa, 0xce],
    [0xca, 0xfe, 0xba, 0xbe], [0xbe, 0xba, 0xfe, 0xca], [0xca, 0xfe, 0xba, 0xbf], [0xbf, 0xba, 0xfe, 0xca],
];
const MH_EXECUTE: u32 = 2;
const MH_FILESET: u32 = 12;
const BUILDER: &str = "oneios-kernel";
const RECORD_SUFFIX: &str = ".build.json";
const MANIFEST: &str = "runtime.json";

/// Why a command stopped: a refusal (the package may not be made: status 3), or something else (status 1).
#[derive(Debug)]
pub enum Stop {
    Refused(String),
    Failed(String),
}

type Result<T> = std::result::Result<T, Stop>;

fn refused<T>(message: String) -> Result<T> {
    Err(Stop::Refused(message))
}

/// An operating system error on a path: no refusal, the command just fails (the script: a traceback).
fn io<T>(result: std::io::Result<T>, path: &Path) -> Result<T> {
    result.map_err(|e| Stop::Failed(format!("{}: {e}", path.display())))
}

pub fn sha256(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = io(File::open(path), path)?;
    let (mut digest, mut block) = (Sha256::new(), vec![0u8; 1 << 20]);
    loop {
        let n = io(file.read(&mut block), path)?;
        if n == 0 {
            return Ok(oneios_common::hex(&digest.finalize()));
        }
        digest.update(&block[..n]);
    }
}

/// `SEGMENT.match(segment)` for `^[A-Za-z0-9_][A-Za-z0-9._+-]*$`. Python's `$` also matches before a newline that
/// ends the text, so a name that ends with one newline passes there, and here.
fn segment_ok(segment: &str) -> bool {
    let body = segment.strip_suffix('\n').unwrap_or(segment);
    let mut bytes = body.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_') && bytes.all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

/// Relative paths of every regular file below stage, in `os.walk`'s order with the names sorted (a folder's own
/// entries first, then its folders one after the other); refuses links, special files and names the fork's
/// unpacker rejects.
pub fn files_of(stage: &Path) -> Result<Vec<String>> {
    let mut result = Vec::new();
    walk(stage, "", &mut result)?;
    Ok(result)
}

fn walk(stage: &Path, below: &str, result: &mut Vec<String>) -> Result<()> {
    let directory = stage.join(below);
    let mut names = Vec::new();
    for entry in io(std::fs::read_dir(&directory), &directory)? {
        let name = io(entry, &directory)?.file_name();
        // (a name that is not UTF-8 is outside the allowed characters anyway: its lossy form says so)
        names.push(name.to_string_lossy().into_owned());
    }
    names.sort();
    let mut folders = Vec::new();
    for name in names {
        let relative = if below.is_empty() { name.clone() } else { format!("{below}/{name}") };
        let path = stage.join(&relative);
        let kind = io(std::fs::symlink_metadata(&path), &path)?.file_type();
        if !(kind.is_file() || kind.is_dir()) {
            return refused(format!("{relative}: not a regular file or directory"));
        }
        if let Some(segment) = relative.split('/').find(|s| !segment_ok(s)) {
            return refused(format!("{relative}: path segment {} is outside [A-Za-z0-9_][A-Za-z0-9._+-]*", repr_str(segment)));
        }
        if kind.is_file() {
            result.push(relative);
        } else {
            folders.push(relative);
        }
    }
    folders.iter().try_for_each(|folder| walk(stage, folder, result))
}

/// What Apple container or binary the file is, or None.
fn content_kind(path: &Path) -> Result<Option<&'static str>> {
    let mut file = io(File::open(path), path)?;
    let size = io(file.metadata(), path)?.len();
    let mut head = Vec::new();
    io(Read::by_ref(&mut file).take(64).read_to_end(&mut head), path)?;
    let mut tail = Vec::new();
    if size >= 512 {
        io(file.seek(SeekFrom::Start(size - 512)), path)?;
        io(Read::by_ref(&mut file).take(4).read_to_end(&mut tail), path)?;
    }
    let at = |from: usize, to: usize| head.get(from..to.min(head.len())).unwrap_or(&[]);
    let holds = |tag: &[u8]| at(0, 24).windows(4).any(|w| w == tag);
    Ok(if head.len() >= 4 && MACHO_MAGICS.iter().any(|m| m == at(0, 4)) {
        Some("Mach-O")
    } else if at(0, 1) == [0x30] && [b"IM4P", b"IMG4", b"IM4M", b"IM4R"].iter().any(|tag| holds(*tag)) {
        // a DER sequence with an Image4 tag in its first bytes
        Some("IMG4 container")
    } else if at(0, 4) == b"AEA1" {
        Some("AEA archive")
    } else if tail == b"koly" || at(0, 4) == b"koly" {
        // the UDIF trailer, 512 bytes before the end (or a file that is the trailer)
        Some("disk image (UDIF)")
    } else if at(0, 4) == b"dyld" || at(0, 4) == b"NXSB" || at(32, 36) == b"NXSB" {
        Some("dyld cache or APFS container")
    } else {
        None
    })
}

/// The build record of a Mach-O that is the XNU kernel/ built: an arm64 MH_EXECUTE with a matching record.
/// Everything else of a Mach-O is refused.
fn own_kernel(path: &Path, relative: &str) -> Result<Value> {
    let mut header = Vec::new();
    io(io(File::open(path), path)?.take(32).read_to_end(&mut header), path)?;
    if header.get(..4) != Some(&[0xcf, 0xfa, 0xed, 0xfe]) {
        return refused(format!("{relative}: a Mach-O that is not a 64-bit little-endian image"));
    }
    if header.len() < 16 {
        return refused(format!("{relative}: a Mach-O magic with no header behind it"));
    }
    let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    let (cputype, filetype) = (word(4), word(12));
    if filetype == MH_FILESET {
        return refused(format!("{relative}: a kernel collection (MH_FILESET) holds Apple's kexts; it belongs to the image set"));
    }
    if cputype != 0x0100_000c || filetype != MH_EXECUTE {
        return refused(format!("{relative}: a Mach-O that is not an arm64 kernel executable"));
    }
    let record_path = format!("{}{RECORD_SUFFIX}", path.display());
    let record_name = Path::new(&record_path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !Path::new(&record_path).is_file() {
        return refused(format!("{relative}: a Mach-O without a build record ({record_name})"));
    }
    let text = io(std::fs::read(&record_path), Path::new(&record_path))?;
    let record = match String::from_utf8(text).map_err(|e| e.to_string()).and_then(|text| pyjson::parse(&text)) {
        Ok(record) => record,
        Err(error) => return refused(format!("{relative}: unreadable build record: {error}")),
    };
    if record.as_dict().is_none() {
        return refused(format!("{relative}: unreadable build record: not a JSON object"));
    }
    let get = |key: &str| record.get(key);
    let shown = |key: &str| get(key).map_or("None".to_string(), str_value);
    if get("builder").and_then(Value::as_str) != Some(BUILDER) {
        return refused(format!("{relative}: the build record is not from kernel/ (builder {})", get("builder").map_or("None".to_string(), repr_value)));
    }
    if get("sha256").and_then(Value::as_str) != Some(sha256(path)?.as_str()) {
        return refused(format!("{relative}: the build record is for another file (SHA-256 differs)"));
    }
    // (`is not False`: the JSON value false and nothing else)
    if get("kernelcache_kexts_added") != Some(&Value::Bool(false)) || get("prelinked") != Some(&Value::Bool(false)) {
        return refused(format!("{relative}: the build record does not say \"no kernelcache kexts, not prelinked\""));
    }
    let empty_list = |key: &str| get(key).and_then(Value::as_list).is_some_and(<[Value]>::is_empty);
    if !empty_list("apple_binary_inputs") {
        return refused(format!("{relative}: the build record lists Apple binary inputs: {}", shown("apple_binary_inputs")));
    }
    if !empty_list("restricted_sources") {
        return refused(format!("{relative}: the kernel links sources that may not be redistributed: {}", shown("restricted_sources")));
    }
    if !truthy(get("build_id")) {
        return refused(format!("{relative}: the build record has no build_id"));
    }
    Ok(record)
}

/// `fnmatch.fnmatch(name, pattern)` for a pattern whose only wildcard is `*`.
fn name_matches(name: &str, pattern: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = name.strip_prefix(first) else { return false };
    let mut parts: Vec<&str> = parts.collect();
    let Some(last) = parts.pop() else { return rest.is_empty() };
    for part in parts {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// The build record of the kernel in the stage, with "path" set to the kernel's (None without one); a refusal
/// for anything else of Apple's.
pub fn check(stage: &Path) -> Result<Option<Value>> {
    let mut kernel: Option<Value> = None;
    for relative in files_of(stage)? {
        let path = stage.join(&relative);
        let name = relative.rsplit('/').next().unwrap_or(&relative).to_lowercase();
        match content_kind(&path)? {
            Some("Mach-O") => {
                let mut record = own_kernel(&path, &relative)?;
                if kernel.is_some() {
                    return refused(format!("{relative}: a second kernel in one package"));
                }
                record.set("path", relative.as_str());
                kernel = Some(record);
                continue;
            }
            Some(kind) => return refused(format!("{relative}: {kind}")),
            None => {}
        }
        if let Some(pattern) = APPLE_NAMES.iter().find(|pattern| name_matches(&name, pattern)) {
            return refused(format!("{relative}: the name matches {pattern} (Apple firmware)"));
        }
    }
    Ok(kernel)
}

/// STAGE/runtime.json: every file with its size, SHA-256 and mode (which is set to 0755 or 0644 on the way).
pub fn manifest(stage: &Path, version: &str, commit: &str, dirty: &str, min_fork: &str, placeholder: &str) -> Result<()> {
    let kernel = check(stage)?;
    let mut entries = Value::list();
    let mut has_xnuhost = false;
    for relative in files_of(stage)?.into_iter().filter(|r| r != MANIFEST) {
        let path = stage.join(&relative);
        let meta = io(std::fs::metadata(&path), &path)?;
        let executable = meta.mode() & 0o111 != 0;
        let mode = if executable { 0o755 } else { 0o644 };
        io(std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)), &path)?;
        has_xnuhost |= relative == "bin/xnuhost" && executable;
        let mut entry = Value::dict();
        entry.set("path", relative.as_str());
        entry.set("size", meta.len());
        entry.set("sha256", sha256(&path)?);
        entry.set("mode", if executable { "0755" } else { "0644" });
        entries.push(entry);
    }
    if !has_xnuhost {
        return refused("bin/xnuhost is missing or not executable".to_string());
    }
    let mut git = Value::dict();
    git.set("commit", commit);
    git.set("dirty", dirty == "1");
    let mut document = Value::dict();
    document.set("format", 1);
    document.set("name", "oneios-runtime");
    document.set("version", version);
    document.set("arch", "arm64");
    document.set("session_abi", 1);
    document.set("image_formats", vec![1]);
    document.set("xnuhost", "bin/xnuhost");
    document.set("git", git);
    document.set("min_oneos_fork", min_fork);
    document.set("placeholder", placeholder == "1");
    document.set("kernel", kernel.map(|record| kernel_entry(&record)));
    document.set("files", entries);
    let path = stage.join(MANIFEST);
    io(std::fs::write(&path, pyjson::dumps(&document, Some(2), false) + "\n"), &path)?;
    io(std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)), &path)
}

/// The manifest's "kernel": the record's identity, and where the record is in the package.
fn kernel_entry(record: &Value) -> Value {
    let field = |key: &str| record.get(key).cloned().unwrap_or(Value::Null);
    let path = record.get("path").and_then(Value::as_str).unwrap_or_default();
    let mut entry = Value::dict();
    entry.set("path", path);
    entry.set("build_id", field("build_id"));
    entry.set("sha256", field("sha256"));
    entry.set("xnu", field("xnu"));
    entry.set("profile", field("profile"));
    entry.set("record", format!("{path}{RECORD_SUFFIX}"));
    entry
}

/// OUT.tar: the files MEMBERS lists, in that order, as Python's tarfile writes them in USTAR_FORMAT with owner
/// 0:0, no owner names and the time MTIME.
pub fn archive(stage: &Path, members: &Path, mtime: &str, out: &Path) -> Result<()> {
    let mtime: u64 = mtime.trim().parse().map_err(|_| Stop::Failed(format!("the time {mtime} is not a number")))?;
    let list = io(std::fs::read_to_string(members), members)?;
    let names = list.split_inclusive('\n').filter(|line| !line.trim().is_empty()).map(|line| line.strip_suffix('\n').unwrap_or(line));
    let mut tar = std::io::BufWriter::new(io(File::create(out), out)?);
    let (mut offset, mut seen) = (0u64, std::collections::HashMap::new());
    for name in names {
        let path = stage.join(name);
        let meta = io(std::fs::symlink_metadata(&path), &path)?;
        // (tarfile makes another name of a file it has already written a hard link entry: not a regular file either)
        let stored = name.trim_start_matches('/');
        let again = meta.nlink() > 1 && seen.get(&(meta.dev(), meta.ino())).is_some_and(|first: &String| first != stored);
        if !meta.file_type().is_file() || again {
            return refused(format!("{name}: not a regular file"));
        }
        seen.insert((meta.dev(), meta.ino()), stored.to_string());
        let (prefix, short) = if stored.len() <= 100 { ("", stored) } else { pytar::posix_split_name(stored).ok_or_else(|| Stop::Failed(format!("{name}: name is too long")))? };
        let block = pytar::ustar_prefixed(short, prefix, meta.mode() & 0o7777, 0, 0, meta.len(), mtime, pytar::REG, "");
        io(tar.write_all(&block), out)?;
        let copied = io(std::io::copy(&mut io(File::open(&path), &path)?.take(meta.len()), &mut tar), out)?;
        if copied != meta.len() {
            return Err(Stop::Failed(format!("{name}: the file changed while it was read")));
        }
        let padded = meta.len().next_multiple_of(pytar::BLOCK as u64);
        io(tar.write_all(&vec![0u8; (padded - meta.len()) as usize]), out)?;
        offset += pytar::BLOCK as u64 + padded;
    }
    io(tar.write_all(&vec![0u8; (pytar::end(offset) - offset) as usize]), out)?;
    io(tar.flush(), out)
}

#[cfg(test)]
#[path = "pack_tests.rs"]
mod tests;
