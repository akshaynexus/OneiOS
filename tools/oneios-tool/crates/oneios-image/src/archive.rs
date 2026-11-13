//! imagetool.py's archive commands: `pack SETDIR OUT.tar.zst` (the archive of a verified set: manifest.json
//! first, then the files in manifest order, nothing else) and `verify SET.tar.zst` (as the OneOS fork checks one:
//! its .sha256 if present, the tar rules, then every file while it streams). zstd is the `zstd` program, as in the
//! script (the archive's bytes are its bytes); the tar stream is written with tar.rs (Python's tarfile headers) and
//! read with the `tar` crate.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

use oneios_common::pyjson::Value;
use sha2::{Digest, Sha256};

use crate::{crash, err, hashes, manifest, profile, py, tar, Fail};

fn need_zstd() -> Result<(), Fail> {
    let found = std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("zstd").is_file()));
    if found {
        Ok(())
    } else {
        Err(err!("zstd is missing: install zstd"))
    }
}

/// The folders above a set path, outermost first.
fn parents(rel: &str) -> Vec<&str> {
    rel.match_indices('/').map(|(at, _)| &rel[..at]).collect()
}

/// The tar stream of the set, into `out`: a folder entry before the first file below it, then each file.
fn write_set(setdir: &str, files: &[String], out: &mut impl Write) -> Result<(), Fail> {
    let io = |e: std::io::Error| crash!("{e}");
    let (mut dirs, mut offset, mut header) = (HashSet::new(), 0u64, Vec::new());
    for rel in files {
        for dir in parents(rel) {
            if dirs.insert(dir.to_string()) {
                header.clear();
                tar::TarInfo { kind: tar::DIR, mode: 0o755, ..tar::TarInfo::new(dir) }.write_header(&mut header);
                out.write_all(&header).map_err(io)?;
                offset += header.len() as u64;
            }
        }
        let full = py::join(setdir, rel);
        let size = std::fs::metadata(&full).map_err(|e| crash!("{}", py::os_error(&e, &full)))?.len();
        header.clear();
        tar::TarInfo { size: size as i128, ..tar::TarInfo::new(rel) }.write_header(&mut header);
        out.write_all(&header).map_err(io)?;
        let mut file = std::fs::File::open(&full).map_err(|e| crash!("{}", py::os_error(&e, &full)))?.take(size);
        let copied = std::io::copy(&mut file, out).map_err(io)?;
        let padded = size.next_multiple_of(tar::BLOCK as u64);
        // (a file that shrank since its size was read is filled with zeros, as tarfile's copy would stop short)
        out.write_all(&vec![0u8; (padded - copied) as usize]).map_err(io)?;
        offset += header.len() as u64 + padded;
    }
    out.write_all(&vec![0u8; (tar::end(offset) - offset) as usize]).map_err(io)
}

/// `pack(setdir, out)`: the archive and its `.sha256`; returns the archive's SHA-256.
pub fn pack(setdir: &str, out: &str) -> Result<String, Fail> {
    let m = manifest::verify_dir(setdir, true)?;
    need_zstd()?;
    let mut files = vec!["manifest.json".to_string()];
    for f in m.get("files").and_then(Value::as_list).unwrap_or(&[]) {
        files.push(f.get("path").and_then(Value::as_str).unwrap_or("").to_string());
    }
    let tmp = format!("{out}.part");
    let raw = std::fs::File::create(&tmp).map_err(|e| crash!("{}", py::os_error(&e, &tmp)))?;
    let mut zstd = Command::new("zstd").args(["-q", "-T0", "-10", "--long=27", "-c"]).stdin(Stdio::piped()).stdout(raw).spawn().map_err(|e| crash!("zstd: {e}"))?;
    let mut stdin = std::io::BufWriter::with_capacity(1 << 20, zstd.stdin.take().expect("piped"));
    let written = write_set(setdir, &files, &mut stdin).and_then(|()| stdin.flush().map_err(|e| crash!("{e}")));
    drop(stdin);
    if !zstd.wait().map_err(|e| crash!("zstd: {e}"))?.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(err!("zstd failed"));
    }
    written?;
    std::fs::rename(&tmp, out).map_err(|e| crash!("{}", py::os_error(&e, out)))?;
    let digest = hashes::sha256_file(out)?;
    let side = format!("{out}.sha256");
    std::fs::write(&side, format!("{digest}  {}\n", py::basename(out))).map_err(|e| crash!("{}", py::os_error(&e, &side)))?;
    Ok(digest)
}

/// What the stream's members must be, and what was seen of them.
#[derive(Default)]
struct Seen {
    manifest: Option<Value>,
    listed: Vec<(String, i128, String)>,
    seen: HashSet<String>,
}

impl Seen {
    /// One member of the stream, in the script's order of checks.
    fn member(&mut self, path: &str, entry: &mut ::tar::Entry<impl Read>) -> Result<(), Fail> {
        let kind = entry.header().entry_type();
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        // (tarfile takes the slash off a folder's name)
        let name = if kind.is_dir() { name.trim_end_matches('/').to_string() } else { name };
        profile::check_set_path(&name, path)?;
        if kind.is_dir() {
            return Ok(());
        }
        if !matches!(kind, ::tar::EntryType::Regular | ::tar::EntryType::Continuous | ::tar::EntryType::GNUSparse) {
            return Err(err!("{path}: {name} is not a regular file"));
        }
        if self.manifest.is_none() {
            if name != "manifest.json" {
                return Err(err!("{path}: the first file is {name}, not manifest.json"));
            }
            return self.take_manifest(entry);
        }
        let Some((_, size, sha256)) = self.listed.iter().find(|f| f.0 == name).filter(|_| !self.seen.contains(&name)) else {
            return Err(err!("{path}: {name} is not in the manifest, or is there twice"));
        };
        let (mut digest, mut n, mut block) = (Sha256::new(), 0i128, vec![0u8; 1 << 22]);
        loop {
            let got = entry.read(&mut block).map_err(|_| err!("{path}: unexpected end of data"))?;
            if got == 0 {
                break;
            }
            digest.update(&block[..got]);
            n += got as i128;
        }
        if n != *size || oneios_common::hex(&digest.finalize()) != *sha256 {
            return Err(err!("{path}: {name} differs from the manifest"));
        }
        self.seen.insert(name);
        Ok(())
    }

    /// manifest.json (its first 16 MiB), checked as a set's manifest is; then its list of files.
    fn take_manifest(&mut self, entry: &mut impl Read) -> Result<(), Fail> {
        let mut text = Vec::new();
        entry.take(16 << 20).read_to_end(&mut text).map_err(|e| crash!("{e}"))?;
        let tmp = std::env::temp_dir().join(format!("oneios-manifest.{}.json", std::process::id()));
        let tmp_path = tmp.to_string_lossy().into_owned();
        std::fs::write(&tmp, &text).map_err(|e| crash!("{}", py::os_error(&e, &tmp_path)))?;
        let loaded = manifest::load_manifest(&tmp_path);
        let _ = std::fs::remove_file(&tmp);
        let manifest = loaded?;
        for f in manifest.get("files").and_then(Value::as_list).unwrap_or(&[]) {
            let text = |key: &str| f.get(key).and_then(Value::as_str).unwrap_or("").to_string();
            self.listed.push((text("path"), f.get("size").and_then(py::as_int).unwrap_or(-1), text("sha256")));
        }
        self.manifest = Some(manifest);
        Ok(())
    }
}

/// `verify_archive(path)`: the archive's manifest when everything in it is what the manifest says.
pub fn verify_archive(path: &str) -> Result<Value, Fail> {
    need_zstd()?;
    let side = format!("{path}.sha256");
    if std::path::Path::new(&side).is_file() {
        let text = std::fs::read_to_string(&side).map_err(|e| crash!("{}", py::os_error(&e, &side)))?;
        let want = text.split_whitespace().next().ok_or_else(|| crash!("{side} is empty"))?;
        if hashes::sha256_file(path)? != want {
            return Err(err!("{path}: SHA-256 differs from {}", py::basename(&side)));
        }
    }
    let mut zstd = Command::new("zstd").args(["-q", "-d", "--long=31", "-c", path]).stdout(Stdio::piped()).spawn().map_err(|e| crash!("zstd: {e}"))?;
    let mut seen = Seen::default();
    let mut stdout = zstd.stdout.take().expect("piped");
    let checked = stream(path, &mut stdout, &mut seen);
    // (what follows the tar's end, its padding, is read too: zstd closed early would end with an error of its own)
    if checked.is_ok() {
        let _ = std::io::copy(&mut stdout, &mut std::io::sink());
    }
    drop(stdout);
    let status = zstd.wait().map_err(|e| crash!("zstd: {e}"))?;
    checked?;
    if !status.success() {
        return Err(err!("{path}: zstd could not read it"));
    }
    let Some(manifest) = seen.manifest else { return Err(err!("{path}: no manifest.json")) };
    let mut missing: Vec<&str> = seen.listed.iter().map(|f| f.0.as_str()).filter(|name| !seen.seen.contains(*name)).collect();
    missing.sort_unstable();
    missing.dedup();
    if !missing.is_empty() {
        return Err(err!("{path}: missing {}", missing.join(", ")));
    }
    Ok(manifest)
}

/// A reader that counts what it gave.
struct Counted<R>(R, u64);

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.0.read(buf)?;
        self.1 += n as u64;
        Ok(n)
    }
}

/// Every member of the tar stream. A stream that is no tar ends as tarfile's two common errors do: `empty file`
/// when there is not one byte (zstd gave nothing), `unexpected end of data` when it breaks off later.
fn stream(path: &str, source: impl Read, seen: &mut Seen) -> Result<(), Fail> {
    let mut source = Counted(source, 0);
    // None: the tar reader's own error
    let read = (|| -> Result<(), Option<Fail>> {
        let mut archive = ::tar::Archive::new(&mut source);
        for entry in archive.entries().map_err(|_| None)? {
            seen.member(path, &mut entry.map_err(|_| None)?).map_err(Some)?;
        }
        Ok(())
    })();
    match read {
        Err(Some(fail)) => Err(fail),
        Ok(()) if source.1 > 0 => Ok(()),
        Err(None) if source.1 > 0 => Err(err!("{path}: unexpected end of data")),
        _ => Err(err!("{path}: empty file")),
    }
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
