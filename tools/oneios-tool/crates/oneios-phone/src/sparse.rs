//! Write a stream to a file with its zero runs as holes (the Mac's sparsewrite.py of docs/newtodo.md "How to
//! resume"): the phone's Data store is a 64 GiB image of which a few GiB hold data.

use anyhow::{Context, Result};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;

/// Holes are made of whole blocks of this size (the file systems' block).
const BLOCK: usize = 4096;
const BUFFER: usize = 4 << 20;
static ZERO: [u8; BLOCK] = [0; BLOCK];

/// (bytes of the stream, bytes written; the rest are holes)
pub struct Written {
    pub size: u64,
    pub data: u64,
}

/// Copy `from` to the new file `out`: a block that is all zeros is skipped, the length is set at the end.
/// `progress` is called with the bytes so far after every buffer.
pub fn copy(mut from: impl Read, out: &File, mut progress: impl FnMut(u64, u64)) -> Result<Written> {
    let mut buf = vec![0u8; BUFFER];
    let (mut size, mut data) = (0u64, 0u64);
    loop {
        // fill the buffer, so that blocks stay aligned to the file's blocks
        let mut have = 0;
        while have < buf.len() {
            match from.read(&mut buf[have..]) {
                Ok(0) => break,
                Ok(n) => have += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e).context("reading the stream"),
            }
        }
        if have == 0 {
            break;
        }
        // runs of blocks that hold data, each written at its place
        let mut run: Option<usize> = None;
        let mut at = 0;
        while at < have {
            let end = (at + BLOCK).min(have);
            let zero = buf[at..end] == ZERO[..end - at];
            match (zero, run) {
                (false, None) => run = Some(at),
                (true, Some(start)) => {
                    out.write_all_at(&buf[start..at], size + start as u64).context("writing")?;
                    data += (at - start) as u64;
                    run = None;
                }
                _ => {}
            }
            at = end;
        }
        if let Some(start) = run {
            out.write_all_at(&buf[start..have], size + start as u64).context("writing")?;
            data += (have - start) as u64;
        }
        size += have as u64;
        progress(size, data);
        if have < buf.len() {
            break;
        }
    }
    out.set_len(size).context("setting the length")?;
    Ok(Written { size, data })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn zero_runs_become_holes_and_the_bytes_are_the_stream() {
        let mut stream = vec![0u8; 9 << 20];
        stream[0] = 1; // the first block
        stream[(4 << 20) - 1] = 2; // the last byte of the first buffer
        stream[(6 << 20) + 5000..(6 << 20) + 5010].fill(3); // inside the second
        stream.extend_from_slice(&[0u8; 1000]); // a tail that is not a whole block, all zero
        let path = std::env::temp_dir().join(format!("oneios-sparse-{}", std::process::id()));
        let out = File::create(&path).unwrap();
        let w = copy(&stream[..], &out, |_, _| {}).unwrap();
        drop(out);
        assert_eq!(w.size, stream.len() as u64);
        assert_eq!(w.data, 3 * 4096);
        assert!(std::fs::read(&path).unwrap() == stream);
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), stream.len() as u64);
        // far fewer blocks than the length (the file system may round a hole's edges)
        assert!(meta.blocks() * 512 < 1 << 20, "{} blocks", meta.blocks());
        std::fs::remove_file(&path).unwrap();
    }
}
