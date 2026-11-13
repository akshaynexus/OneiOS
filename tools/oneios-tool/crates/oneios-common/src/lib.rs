//! Shared helpers of oneios-tool: read-only file maps and JSON text that is byte for byte what Python's `json`
//! module writes (`pyjson`), so a Rust step can replace a Python step and leave the same file.

pub mod pyjson;
pub mod pyrepr;
pub mod pytar;

use anyhow::{Context, Result};
use std::fs::File;
use std::path::Path;

/// A file mapped read-only. An empty file maps to an empty slice.
pub struct Map {
    map: Option<memmap2::Mmap>,
}

impl Map {
    pub fn open(path: impl AsRef<Path>) -> Result<Map> {
        let path = path.as_ref();
        let file = File::open(path).with_context(|| format!("{}", path.display()))?;
        let len = file.metadata().with_context(|| format!("{}", path.display()))?.len();
        if len == 0 {
            return Ok(Map { map: None });
        }
        // Safety: the map is read-only; the tools' inputs are files nobody rewrites while a tool runs.
        let map = unsafe { memmap2::Mmap::map(&file) }.with_context(|| format!("{}: mmap", path.display()))?;
        Ok(Map { map: Some(map) })
    }
}

impl std::ops::Deref for Map {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.map.as_deref().unwrap_or(&[])
    }
}

/// Lower-case hex of bytes.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}

/// SHA-256 of bytes, as lower-case hex.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(data))
}

/// Write a file through a temporary name in the same folder, then rename it in place.
pub fn atomic_write(path: impl AsRef<Path>, data: &[u8]) -> Result<()> {
    let path = path.as_ref();
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    std::fs::write(&tmp, data).with_context(|| format!("{}", path.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("{}", path.display()))?;
    Ok(())
}
