//! Shared helpers of oneios-tool: read-only file maps, the JSON of the records and reports (`json`: serde_json
//! with an object that keeps its key order), a request's limits (`job`).

pub mod job;
pub mod json;
pub mod pyrepr;

use anyhow::{Context, Result};
use std::fs::File;
use std::path::Path;

/// A file mapped read-only. An empty file maps to an empty slice.
pub struct Map {
    map: Option<memmap2::Mmap>,
}

impl Map {
    /// A map of no bytes (a file that is not there: a subcache left out of a partial copy, oneios-sym).
    pub fn empty() -> Map {
        Map { map: None }
    }

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

/// Where a kernel panic's head is in the text of a guest console line: `panic(cpu N caller 0x...)`, also when the
/// console lost its first letters (`anic(cpu 0 caller 0x...)`: the phone's panic of 2026-11-23, where the line before
/// it was cut at the kernel's 512-byte console buffer). Returns the byte offset just after `(cpu `. A cut head must
/// start the text (after blanks) and go on with the CPU's number and ` caller 0x`, so running text is not taken.
pub fn panic_head(text: &str) -> Option<usize> {
    if let Some(at) = text.find("panic(cpu ") {
        return Some(at + "panic(cpu ".len());
    }
    let body = text.trim_start();
    let lead = text.len() - body.len();
    for cut in 1..="panic".len() {
        let word = &"panic"[cut..];
        let Some(rest) = body.strip_prefix(word).and_then(|r| r.strip_prefix("(cpu ")) else { continue };
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if digits > 0 && rest[digits..].starts_with(" caller 0x") {
            return Some(lead + word.len() + "(cpu ".len());
        }
    }
    None
}

#[cfg(test)]
mod panic_head_tests {
    use super::panic_head;

    #[test]
    fn whole_and_cut_heads() {
        assert_eq!(panic_head("panic(cpu 1 caller 0xfffffff00ade5440): x"), Some(10));
        assert_eq!(panic_head("hosted: panic(cpu 1 caller 0x1): x"), Some(18));
        // The phone's line of 2026-11-23 (its `p` lost), and shorter cuts.
        let cut = "anic(cpu 0 caller 0xfffffff00ade5440): DebuggerXCallEnter>Debugger synch pending on cpu 1";
        assert_eq!(panic_head(cut).map(|at| &cut[at..at + 1]), Some("0"));
        assert_eq!(panic_head("  ic(cpu 7 caller 0x2): y"), Some(9));
        assert_eq!(panic_head("(cpu 3 caller 0x2): y"), Some(5));
        // Not a panic: running text, a head without its caller, a cut head inside a line.
        assert_eq!(panic_head("the panic (cpu 3) was"), None);
        assert_eq!(panic_head("anic(cpu 0): x"), None);
        assert_eq!(panic_head("words anic(cpu 0 caller 0x1): x"), None);
        assert_eq!(panic_head("mechanic(cpu"), None);
    }
}
