//! SHA-256 of files and the chunk hash list (docs/image-format.md section 4): `NAME.hashes` is the header
//! `OIHL1\0\0\0`, chunk size (u32), algorithm 1 (u32), file size (u64), chunk count (u64), then the SHA-256 of
//! each chunk.
//!
//! One pass over a file gives both hashes. The file is read once, in order, in segments of a few chunks. Each
//! segment goes to a thread that runs the SHA-256 of the whole file (sequential by nature) and to the rayon pool,
//! which hashes the segment's chunks. So an image costs the time of one SHA-256 over it, not two.

use crate::{crash, err, Fail};
use oneios_common::hex;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

pub const HASHES_MAGIC: &[u8; 8] = b"OIHL1\0\0\0";
pub const DEFAULT_CHUNK: u64 = 1 << 20;
/// Python's read size for a plain file hash.
const BLOCK: usize = 1 << 22;
/// A segment: this many bytes of whole chunks are read at once and shared by the two hashers.
const SEGMENT: u64 = 8 << 20;
/// Chunks above this size are hashed in the reading thread (a segment of one such chunk would hold too much).
const PARALLEL_CHUNK_MAX: u64 = 64 << 20;
/// Segments that may wait for the pool to hash their chunks, per file.
const WAITING_MAX: usize = 4;

/// What one pass over a file found.
pub struct Digests {
    /// Bytes read.
    pub size: u64,
    /// SHA-256 of the whole file, lower-case hex.
    pub sha256: String,
    /// The SHA-256 of each chunk, 32 bytes each (empty without a chunk size).
    pub chunks: Vec<u8>,
    /// The file had more bytes than `size` when the pass ended.
    pub grew: bool,
}

fn io_fail(path: &str, e: std::io::Error) -> Fail {
    crash!("{path}: {e}")
}

/// Asked between blocks: true ends a pass early, when its result is no longer wanted (`verify` found an earlier
/// file wrong).
pub type Stop<'a> = &'a (dyn Fn() -> bool + Sync);

/// `sha256_file(path)`.
pub fn sha256_file(path: &str) -> Result<String, Fail> {
    Ok(sha256_file_until(path, &|| false)?.expect("nothing stops this pass"))
}

/// `sha256_file(path)`, or None when `stop` ended it.
pub fn sha256_file_until(path: &str, stop: Stop) -> Result<Option<String>, Fail> {
    let mut f = File::open(path).map_err(|e| io_fail(path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; BLOCK];
    loop {
        if stop() {
            return Ok(None);
        }
        let n = f.read(&mut buf).map_err(|e| io_fail(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(Some(hex(&h.finalize())))
}

/// A segment's buffer; it goes back to the reader's pool when both hashers are done with it.
struct Segment {
    data: Vec<u8>,
    len: usize,
    pool: mpsc::Sender<Vec<u8>>,
}

impl Drop for Segment {
    fn drop(&mut self) {
        let _ = self.pool.send(std::mem::take(&mut self.data));
    }
}

fn read_full(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        match f.read(&mut buf[done..]) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}

/// Read `size` bytes of `path` once: the SHA-256 of all of it and of each `chunk` bytes.
/// `size` is the file's size as the caller saw it; fewer bytes on disk end the pass early (`Digests.size` says so).
pub fn digests(path: &str, size: u64, chunk: u64) -> Result<Digests, Fail> {
    Ok(digests_until(path, size, chunk, &|| false)?.expect("nothing stops this pass"))
}

/// `digests`, or None when `stop` ended the pass.
pub fn digests_until(path: &str, size: u64, chunk: u64, stop: Stop) -> Result<Option<Digests>, Fail> {
    let mut f = File::open(path).map_err(|e| io_fail(path, e))?;
    let count = size.div_ceil(chunk);
    let mut chunks = vec![0u8; (count * 32) as usize];
    let per_segment = if chunk > PARALLEL_CHUNK_MAX { 1 } else { (SEGMENT / chunk).max(1) };
    let segment = (per_segment * chunk) as usize;

    if chunk > PARALLEL_CHUNK_MAX {
        // Rare (the format's chunks are 1 MiB): stream, both hashers in this thread.
        let mut whole = Sha256::new();
        let mut buf = vec![0u8; BLOCK];
        let mut done = 0u64;
        for slot in chunks.chunks_mut(32) {
            let mut one = Sha256::new();
            let mut left = chunk.min(size - done);
            while left > 0 {
                if stop() {
                    return Ok(None);
                }
                let want = (left as usize).min(buf.len());
                let n = read_full(&mut f, &mut buf[..want]).map_err(|e| io_fail(path, e))?;
                whole.update(&buf[..n]);
                one.update(&buf[..n]);
                done += n as u64;
                left -= n as u64;
                if n < want {
                    left = 0;
                }
            }
            slot.copy_from_slice(&one.finalize());
        }
        let grew = read_full(&mut f, &mut buf[..1]).map_err(|e| io_fail(path, e))? != 0;
        return Ok(Some(Digests { size: done, sha256: hex(&whole.finalize()), chunks, grew }));
    }

    let (pool_tx, pool_rx) = mpsc::channel::<Vec<u8>>();
    let (whole_tx, whole_rx) = mpsc::sync_channel::<Arc<Segment>>(2);
    let whole = std::thread::spawn(move || {
        let mut h = Sha256::new();
        for seg in whole_rx {
            h.update(&seg.data[..seg.len]);
        }
        hex(&h.finalize())
    });
    let all_slots = &mut chunks;
    let waiting = Arc::new(AtomicUsize::new(0));
    let (done, failed, grew, stopped) = rayon::scope(move |scope| {
        let mut done = 0u64;
        let mut failed: Option<std::io::Error> = None;
        let mut grew = false;
        let mut stopped = false;
        let mut slots = all_slots.chunks_mut(32 * per_segment as usize);
        while done < size {
            if stop() {
                stopped = true;
                break;
            }
            let mut data = pool_rx.try_recv().unwrap_or_default();
            data.resize(segment, 0);
            let want = ((size - done) as usize).min(segment);
            let n = match read_full(&mut f, &mut data[..want]) {
                Ok(n) => n,
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            };
            done += n as u64;
            let seg = Arc::new(Segment { data, len: n, pool: pool_tx.clone() });
            let slot = slots.next().expect("a slot per segment");
            let for_chunks = seg.clone();
            let mut hash_chunks = move || {
                // Python hashes what it read: a chunk the file no longer has is the hash of nothing.
                let data = &for_chunks.data[..for_chunks.len];
                slot.par_chunks_mut(32).enumerate().for_each(|(i, out)| {
                    let start = (i * chunk as usize).min(data.len());
                    let end = (start + chunk as usize).min(data.len());
                    out.copy_from_slice(&Sha256::digest(&data[start..end]));
                });
            };
            // The pool's other threads take the segments' chunks. When they do not keep up (every thread reads
            // a file of its own, or the pool has one thread), this thread hashes the segment itself: the
            // segments waiting never hold more than a few buffers.
            if waiting.load(Ordering::Relaxed) >= WAITING_MAX {
                hash_chunks();
            } else {
                waiting.fetch_add(1, Ordering::Relaxed);
                let waiting = waiting.clone();
                scope.spawn(move |_| {
                    hash_chunks();
                    waiting.fetch_sub(1, Ordering::Relaxed);
                });
            }
            if whole_tx.send(seg).is_err() {
                break;
            }
            if n < want {
                break;
            }
        }
        for slot in slots {
            let empty = Sha256::digest(b"");
            slot.chunks_mut(32).for_each(|out| out.copy_from_slice(&empty));
        }
        if failed.is_none() && !stopped && done == size {
            let mut one = [0u8; 1];
            match read_full(&mut f, &mut one) {
                Ok(n) => grew = n != 0,
                Err(e) => failed = Some(e),
            }
        }
        (done, failed, grew, stopped)
    });
    let sha256 = whole.join().map_err(|_| crash!("{path}: the hashing thread failed"))?;
    if let Some(e) = failed {
        return Err(io_fail(path, e));
    }
    if stopped {
        return Ok(None);
    }
    Ok(Some(Digests { size: done, sha256, chunks, grew }))
}

pub fn file_size(path: &str) -> Result<u64, Fail> {
    Ok(std::fs::metadata(path).map_err(|e| crash!("{}", crate::py::os_error(&e, path)))?.len())
}

fn header(chunk: u64, size: u64) -> Vec<u8> {
    let mut out = HASHES_MAGIC.to_vec();
    out.extend_from_slice(&(chunk as u32).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&size.div_ceil(chunk).to_le_bytes());
    out
}

/// `write_hashes(image, out, chunk)`: IMAGE's chunk hash list. Returns (size, SHA-256 of the image).
pub fn write_hashes(image: &str, out: &str, chunk: u64) -> Result<(u64, String), Fail> {
    if chunk == 0 || chunk > u32::MAX as u64 {
        return Err(crash!("--chunk {chunk}: a chunk is 1 to 4294967295 bytes"));
    }
    let size = file_size(image)?;
    // Like Python: the image is opened, then OUT is made (so a missing image leaves no file behind).
    File::open(image).map_err(|e| crash!("{}", crate::py::os_error(&e, image)))?;
    let mut o = File::create(out).map_err(|e| crash!("{}", crate::py::os_error(&e, out)))?;
    let d = digests(image, size, chunk)?;
    let mut data = header(chunk, size);
    data.extend_from_slice(&d.chunks);
    std::io::Write::write_all(&mut o, &data).map_err(|e| io_fail(out, e))?;
    if d.size != size || d.grew {
        return Err(err!("{image}: changed while it was read"));
    }
    Ok((size, d.sha256))
}

pub struct HashList {
    pub chunk: u64,
    pub size: u64,
    pub digests: Vec<u8>,
}

/// `read_hashes(path)`.
pub fn read_hashes(path: &str) -> Result<HashList, Fail> {
    let data = std::fs::read(path).map_err(|e| crash!("{}", crate::py::os_error(&e, path)))?;
    if data.len() < 32 || &data[..8] != HASHES_MAGIC {
        return Err(err!("{path}: not a chunk hash list"));
    }
    let chunk = u32::from_le_bytes(data[8..12].try_into().unwrap()) as u64;
    let algo = u32::from_le_bytes(data[12..16].try_into().unwrap());
    let size = u64::from_le_bytes(data[16..24].try_into().unwrap());
    let count = u64::from_le_bytes(data[24..32].try_into().unwrap());
    if algo != 1
        || chunk < 4096
        || chunk & (chunk - 1) != 0
        || count != size.div_ceil(chunk)
        || data.len() as u128 != 32 + 32 * count as u128
    {
        return Err(err!("{path}: bad chunk hash list header"));
    }
    Ok(HashList { chunk, size, digests: data[32..].to_vec() })
}

/// The first chunk of `got` that differs from the list, as `check_hashes` reports it.
pub fn first_difference(image: &str, list: &HashList, got: &[u8]) -> Result<(), Fail> {
    for (i, (want, have)) in list.digests.chunks(32).zip(got.chunks(32)).enumerate() {
        if want != have {
            return Err(err!("{image}: chunk {i} (offset {}) differs from its hash list", i as u64 * list.chunk));
        }
    }
    Ok(())
}

/// `check_hashes(image, hashes)`.
#[cfg(test)]
pub fn check_hashes(image: &str, hashes: &str) -> Result<(), Fail> {
    let list = read_hashes(hashes)?;
    let size = file_size(image)?;
    if size != list.size {
        return Err(err!("{image}: {size} bytes, its hash list says {}", list.size));
    }
    let d = digests(image, size, list.chunk)?;
    first_difference(image, &list, &d.chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{noise, Tmp};

    #[test]
    fn hash_list() {
        let tmp = Tmp::new("hashes");
        let data = noise(4096 * 5 + 123, 7);
        let image = tmp.write("a.img", &data);
        let hashes = tmp.path("a.img.hashes");
        let (size, digest) = write_hashes(&image, &hashes, 4096).unwrap();
        assert_eq!((size, digest.as_str()), (data.len() as u64, oneios_common::sha256_hex(&data).as_str()));
        assert_eq!(sha256_file(&image).unwrap(), digest);
        let raw = std::fs::read(&hashes).unwrap();
        assert_eq!(&raw[..8], b"OIHL1\0\0\0");
        assert_eq!(&raw[8..16], &[0, 0x10, 0, 0, 1, 0, 0, 0]);
        assert_eq!(u64::from_le_bytes(raw[16..24].try_into().unwrap()), data.len() as u64);
        assert_eq!(u64::from_le_bytes(raw[24..32].try_into().unwrap()), 6);
        assert_eq!(raw.len(), 32 + 32 * 6);
        for (i, part) in data.chunks(4096).enumerate() {
            assert_eq!(&raw[32 + 32 * i..64 + 32 * i], Sha256::digest(part).as_slice(), "chunk {i}");
        }
        let list = read_hashes(&hashes).unwrap();
        assert_eq!((list.chunk, list.size, list.digests.len()), (4096, data.len() as u64, 32 * 6));
        check_hashes(&image, &hashes).unwrap();

        let mut changed = data.clone();
        changed[4096 * 3 + 7] ^= 0xff;
        std::fs::write(&image, &changed).unwrap();
        match check_hashes(&image, &hashes) {
            Err(Fail::Error(m)) => assert!(m.ends_with("a.img: chunk 3 (offset 12288) differs from its hash list"), "{m}"),
            _ => panic!("a changed chunk passed"),
        }
        let bad = tmp.write("bad.hashes", b"OIHL1\0\0\0\0\0\0\0\0\0\0\0\0\0");
        assert!(matches!(read_hashes(&bad), Err(Fail::Error(m)) if m.ends_with("not a chunk hash list")));
        let mut wrong = raw.clone();
        wrong[24] = 7;
        let bad = tmp.write("bad2.hashes", &wrong);
        assert!(matches!(read_hashes(&bad), Err(Fail::Error(m)) if m.ends_with("bad chunk hash list header")));
    }

    #[test]
    fn sizes_around_the_segment() {
        let tmp = Tmp::new("hashes-sizes");
        // empty, one byte, exactly one chunk, several segments with a short tail, a chunk larger than a segment
        for (len, chunk) in [(0usize, 4096u64), (1, 4096), (4096, 4096), (3 * (8 << 20) + 5, 1 << 20), (100_000, 1 << 24)] {
            let data = noise(len, len as u64 + 1);
            let image = tmp.write("x.img", &data);
            let out = tmp.path("x.hashes");
            let (size, digest) = write_hashes(&image, &out, chunk).unwrap();
            assert_eq!(size, len as u64);
            assert_eq!(digest, oneios_common::sha256_hex(&data));
            let raw = std::fs::read(&out).unwrap();
            let mut want = header(chunk, len as u64);
            for part in data.chunks(chunk as usize) {
                want.extend_from_slice(&Sha256::digest(part));
            }
            assert_eq!(raw, want, "len {len} chunk {chunk}");
        }
    }

    #[test]
    fn streaming_path_for_huge_chunks() {
        let tmp = Tmp::new("hashes-huge");
        let data = noise(300_000, 3);
        let image = tmp.write("x.img", &data);
        let d = digests(&image, data.len() as u64, PARALLEL_CHUNK_MAX * 2).unwrap();
        assert_eq!(d.sha256, oneios_common::sha256_hex(&data));
        assert_eq!(d.chunks, Sha256::digest(&data).to_vec());
        assert!(!d.grew);
    }
}
