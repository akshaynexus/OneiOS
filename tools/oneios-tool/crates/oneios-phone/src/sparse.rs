//! Write a stream to a file with its zero runs as holes (the Mac's sparsewrite.py of docs/newtodo.md "How to
//! resume", kept as tests/phone/sparsewrite.py): the phone's Data store is a 32 GiB image of which a few GiB hold
//! data.
//!
//! What is written: only the blocks that hold a byte that is not zero, as runs, each at its offset; the length is
//! set at the end. A block is the unit the file's file system makes holes of (`hole_block`).
//!
//! What a file system does with a range that was never written (README "The sparse copy" has the maps):
//! - ext4, btrfs, f2fs, xfs, tmpfs (Linux): it is a hole. Nothing more is needed.
//! - APFS (macOS): the format holds a hole of any whole number of its 4096-byte blocks (a file extent record with
//!   no block: `j_file_extent_val_t`, the inode's INODE_IS_SPARSE; crates/oneios-image/src/apfs/files.rs reads
//!   them). Apple's driver is not documented and its source is not public. What others measured (the test below
//!   prints what this Mac does): a write behind a hole shorter than about 16 MiB makes APFS fill that hole with
//!   zero blocks it allocates, whatever the order of the writes and whenever the length is set; a hole of 17 MiB
//!   or more stays; a write allocates whole pages (16 KiB on Apple silicon). So a file of 9 MiB written this way
//!   holds no hole at all there. `fcntl(F_PUNCHHOLE)` (fcntl(2): "Deallocate a region and replace it with a hole", offset and
//!   length multiples of the file system's block size) gives a filled range back: on macOS every hole in front
//!   of a write is punched after the write (`ONEIOS_SPARSE_PUNCH=0` turns that off, `=1` turns it on elsewhere,
//!   where it changes nothing: Linux's is fallocate's FALLOC_FL_PUNCH_HOLE).

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::os::unix::io::AsRawFd;

/// One read of the stream; a multiple of every block size `hole_block` gives. Measured (README "The sparse
/// copy"): 64 KiB to 1 MiB cost about the same CPU, 256 KiB the least; 4 MiB costs twice that (the buffer leaves
/// the cache between the unpacking and the scan); a second thread for the scan and the writes costs a third more
/// CPU and saves no time.
const BUFFER: usize = 256 << 10;
/// Only a hole up to this length is punched: APFS was seen to fill holes of up to 16 MiB and none of 17 MiB or more.
const PUNCH_MAX: u64 = 64 << 20;
static ZERO: [u8; 1 << 16] = [0; 1 << 16];

/// What `copy` did.
pub struct Written {
    /// bytes of the stream: the file's length
    pub size: u64,
    /// bytes written; the rest are holes
    pub data: u64,
    /// the block holes are made of
    pub block: u64,
    /// holes given back with a punch (macOS)
    pub punched: u64,
}

/// The unit holes are made of on `file`'s file system: its block (statvfs's f_frsize: 4096 on ext4, btrfs, f2fs,
/// tmpfs and APFS; on macOS statvfs's f_bsize is the preferred transfer size, not the block). On macOS not less than
/// a page: a write there allocates whole pages.
pub fn hole_block(file: &File) -> usize {
    let mut vfs = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: fstatvfs fills the struct it is given; a zeroed statvfs is a valid one.
    let mut block = match unsafe { libc::fstatvfs(file.as_raw_fd(), vfs.as_mut_ptr()) } {
        0 => {
            let vfs = unsafe { vfs.assume_init() };
            if vfs.f_frsize != 0 { vfs.f_frsize as usize } else { vfs.f_bsize as usize }
        }
        _ => 0,
    };
    if cfg!(target_os = "macos") {
        // SAFETY: sysconf reads a constant of the system.
        block = block.max(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(0) as usize);
    }
    if block.is_power_of_two() && (512..=BUFFER).contains(&block) { block } else { 4096 }
}

/// Whether holes in front of a write are punched: on macOS, or as ONEIOS_SPARSE_PUNCH (0 or 1) says.
fn punch_wanted() -> bool {
    match std::env::var("ONEIOS_SPARSE_PUNCH").as_deref() {
        Ok("0") => false,
        Ok("1") => true,
        _ => cfg!(target_os = "macos"),
    }
}

/// Give the blocks of `len` bytes at `at` back to the file system; the range then reads as zeros.
#[cfg(target_os = "macos")]
fn punch_hole(file: &File, at: u64, len: u64) -> io::Result<()> {
    let hole = libc::fpunchhole_t { fp_flags: 0, reserved: 0, fp_offset: at as libc::off_t, fp_length: len as libc::off_t };
    // SAFETY: F_PUNCHHOLE reads the fpunchhole_t it is given.
    match unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PUNCHHOLE, &hole) } {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn punch_hole(file: &File, at: u64, len: u64) -> io::Result<()> {
    let mode = libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE;
    // SAFETY: fallocate takes no pointer.
    match unsafe { libc::fallocate(file.as_raw_fd(), mode, at as libc::off_t, len as libc::off_t) } {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn punch_hole(_: &File, _: u64, _: u64) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Not byte by byte: memcmp against zeros (the C library's, vectorized; 10 to 25 GB/s where a byte loop does 1).
fn is_zero(bytes: &[u8]) -> bool {
    bytes.chunks(ZERO.len()).all(|c| c == &ZERO[..c.len()])
}

/// Where the blocks go: the file, or the tests' model of a file system.
trait Sink {
    fn write_at(&mut self, bytes: &[u8], at: u64) -> io::Result<()>;
    fn punch(&mut self, at: u64, len: u64) -> io::Result<()>;
    fn set_len(&mut self, len: u64) -> io::Result<()>;
}

impl Sink for &File {
    fn write_at(&mut self, bytes: &[u8], at: u64) -> io::Result<()> {
        self.write_all_at(bytes, at)
    }
    fn punch(&mut self, at: u64, len: u64) -> io::Result<()> {
        punch_hole(self, at, len)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        File::set_len(self, len)
    }
}

struct Writer<S: Sink> {
    sink: S,
    block: usize,
    punch: bool,
    /// the end of the data written last: the next hole begins here
    end: u64,
    data: u64,
    punched: u64,
}

impl<S: Sink> Writer<S> {
    /// One buffer of the stream, at offset `at` (a multiple of the block): its runs of blocks that hold data.
    fn put(&mut self, buf: &[u8], at: u64) -> Result<()> {
        let mut run: Option<usize> = None;
        let mut i = 0;
        while i < buf.len() {
            let next = (i + self.block).min(buf.len());
            match (is_zero(&buf[i..next]), run) {
                (false, None) => run = Some(i),
                (true, Some(start)) => {
                    self.run(&buf[start..i], at + start as u64)?;
                    run = None;
                }
                _ => {}
            }
            i = next;
        }
        match run {
            Some(start) => self.run(&buf[start..], at + start as u64),
            None => Ok(()),
        }
    }

    fn run(&mut self, bytes: &[u8], at: u64) -> Result<()> {
        self.sink.write_at(bytes, at).context("writing")?;
        self.data += bytes.len() as u64;
        self.hole(at);
        self.end = at + bytes.len() as u64;
        Ok(())
    }

    /// The hole from the last data to `to`, after the write (or the length) that may have made the file system
    /// fill it. Whole blocks only. A punch that fails costs space, not bytes: it is said once and not tried again.
    fn hole(&mut self, to: u64) {
        let from = self.end.next_multiple_of(self.block as u64);
        if !self.punch || to <= from || to - from > PUNCH_MAX {
            return;
        }
        match self.sink.punch(from, to - from) {
            Ok(()) => self.punched += 1,
            Err(e) => {
                self.punch = false;
                eprintln!("sparse: punching the hole of {} bytes at {from} failed ({e}): the holes this file system fills stay filled", to - from);
            }
        }
    }

    /// The stream ended at offset `end`. `whole`: it was the whole of a new file, whose length is set now; a
    /// range of a file that has its length already keeps it.
    fn finish(&mut self, end: u64, whole: bool) -> Result<()> {
        if whole {
            self.sink.set_len(end).context("setting the length")?;
        }
        self.hole(end - end % self.block as u64);
        Ok(())
    }
}

/// Read until `buf` is full or the stream ends, so that every buffer but the last is whole blocks.
fn fill(from: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut have = 0;
    while have < buf.len() {
        match from.read(&mut buf[have..]) {
            Ok(0) => break,
            Ok(n) => have += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e).context("reading the stream"),
        }
    }
    Ok(have)
}

/// Copy `from` to the new file `out`: a block that is all zeros is not written, the length is set at the end.
/// `progress` is called with the bytes read and the bytes written so far after every buffer.
pub fn copy(from: impl Read, out: &File, progress: impl FnMut(u64, u64)) -> Result<Written> {
    copy_to(from, out, hole_block(out), punch_wanted(), progress)
}

/// Copy `from` into the range of `out` that begins at `at` (a multiple of the block): as `copy`, but the file
/// has its length already and keeps it (`phone pull-data` copies the store chunk by chunk, each at its offset).
/// The range must read as zeros before (`clear`), because a block of zeros is not written here either.
/// `Written::size` is what the stream gave: the caller checks it against the range's length.
pub fn copy_at(from: impl Read, out: &File, at: u64, progress: impl FnMut(u64, u64)) -> Result<Written> {
    copy_from(from, out, hole_block(out), punch_wanted(), at, false, progress)
}

fn copy_to<S: Sink>(from: impl Read, sink: S, block: usize, punch: bool, progress: impl FnMut(u64, u64)) -> Result<Written> {
    copy_from(from, sink, block, punch, 0, true, progress)
}

/// `at`: where the stream begins in the file. `whole`: the stream is the whole file (`Writer::finish`).
fn copy_from<S: Sink>(mut from: impl Read, sink: S, block: usize, punch: bool, at: u64, whole: bool, mut progress: impl FnMut(u64, u64)) -> Result<Written> {
    let mut w = Writer { sink, block, punch, end: at, data: 0, punched: 0 };
    let mut buf = vec![0u8; BUFFER];
    let mut size = 0u64;
    loop {
        let have = fill(&mut from, &mut buf)?;
        if have == 0 {
            break;
        }
        w.put(&buf[..have], at + size)?;
        size += have as u64;
        progress(size, w.data);
        if have < BUFFER {
            break;
        }
    }
    w.finish(at + size, whole)?;
    Ok(Written { size, data: w.data, block: block as u64, punched: w.punched })
}

/// `lseek` to the next data (`SEEK_DATA`) or the next hole (`SEEK_HOLE`) at or after `at`. None: there is none
/// before the file's end (ENXIO). A file system that does not report holes says "data here, a hole at the end".
fn seek(file: &File, at: u64, whence: libc::c_int) -> io::Result<Option<u64>> {
    // SAFETY: lseek takes no pointer. The file's position is not used anywhere here (pread, pwrite).
    match unsafe { libc::lseek(file.as_raw_fd(), at as libc::off_t, whence) } {
        -1 => match io::Error::last_os_error() {
            e if e.raw_os_error() == Some(libc::ENXIO) => Ok(None),
            e => Err(e),
        },
        found => Ok(Some(found as u64)),
    }
}

/// The data regions of `file` inside `at..at + len`, as the file system reports them; when it reports none
/// (an error of `lseek`), the whole range.
fn data_regions(file: &File, at: u64, len: u64) -> Vec<(u64, u64)> {
    let end = at + len;
    let (mut regions, mut pos) = (Vec::new(), at);
    while pos < end {
        let data = match seek(file, pos, libc::SEEK_DATA) {
            Ok(Some(data)) if data < end => data.max(pos),
            Ok(_) => break,
            Err(_) => return vec![(at, end)],
        };
        let hole = match seek(file, data, libc::SEEK_HOLE) {
            Ok(Some(hole)) if hole > data => hole.min(end),
            _ => end,
        };
        regions.push((data, hole));
        pos = hole;
    }
    regions
}

/// Make `at..at + len` read as zeros again (`at` a multiple of the block): a range that holds what a stream
/// that broke left there is copied a second time, and `copy_at` writes no block of zeros over it. A hole where
/// the file system punches one; where it does not, zeros are written (they cost space, the bytes are right).
/// The file system's map is not asked whether the range holds data: the caller knows whether it was written to
/// (a map of data not yet written out is not trusted on every file system).
pub fn clear(file: &File, at: u64, len: u64) -> Result<()> {
    let (block, end) = (hole_block(file) as u64, at + len);
    // whole blocks only (F_PUNCHHOLE refuses anything else); a tail inside a block is written
    let whole = (end - end % block).saturating_sub(at);
    let punched = whole > 0 && punch_hole(file, at, whole).is_ok();
    let mut pos = if punched { at + whole } else { at };
    while pos < end {
        let n = (end - pos).min(ZERO.len() as u64) as usize;
        file.write_all_at(&ZERO[..n], pos).context("writing zeros")?;
        pos += n as u64;
    }
    Ok(())
}

/// SHA-256 (lower-case hex) of `len` zero bytes: what a range that is one hole reads as.
pub fn zeros_sha256(len: u64) -> String {
    let mut hash = Sha256::new();
    hash_zeros(&mut hash, len);
    oneios_common::hex(&hash.finalize())
}

fn hash_zeros(hash: &mut Sha256, mut len: u64) {
    while len > 0 {
        let n = len.min(ZERO.len() as u64) as usize;
        hash.update(&ZERO[..n]);
        len -= n as u64;
    }
}

/// SHA-256 (lower-case hex) of the bytes `at..at + len` of `file` as they read back, a hole as zeros; None when
/// the range holds no data region at all (its sum is `zeros_sha256(len)`, which the caller keeps). With `map`
/// the holes the file system reports are not read (reading a hole fills the page cache with zero pages): zeros
/// are hashed in their place. Without it every byte is read. Call it after the data was written out
/// (`sync_data`): the map of data still in memory is not trusted on every file system.
pub fn range_sha256(file: &File, at: u64, len: u64, map: bool) -> Result<Option<String>> {
    let regions = if map { data_regions(file, at, len) } else { vec![(at, at + len)] };
    if regions.is_empty() {
        return Ok(None);
    }
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut pos = at;
    for (from, to) in regions {
        hash_zeros(&mut hash, from - pos);
        pos = from;
        while pos < to {
            let want = (to - pos).min(buf.len() as u64) as usize;
            match file.read_at(&mut buf[..want], pos) {
                Ok(0) => anyhow::bail!("the file ends at {pos}, inside the range {at}..{}", at + len),
                Ok(n) => {
                    hash.update(&buf[..n]);
                    pos += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e).context("reading the copy back"),
            }
        }
    }
    hash_zeros(&mut hash, at + len - pos);
    Ok(Some(oneios_common::hex(&hash.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;

    const MIB: usize = 1 << 20;

    /// A stream and the name of what it tests.
    struct Layout {
        name: &'static str,
        stream: Vec<u8>,
    }

    /// The streams: every kind of zero run and data run, and the ends.
    fn layouts() -> Vec<Layout> {
        let mut all = Vec::new();
        // zero runs shorter than a block, of one block, between a block and 4 MiB, around APFS's 16 MiB, and long
        let mut s = vec![0u8; 150 * MIB + 1000];
        let mut data = |at: usize, len: usize| {
            for (i, b) in s[at..at + len].iter_mut().enumerate() {
                *b = (i % 251) as u8 + 1;
            }
        };
        data(0, 1); // one byte: the first block
        data(2 * 16384, 100); // one block of zeros in front (a page on Apple silicon)
        data(2 * 16384 + 300, 16384 * 3); // 200 zero bytes in front: no hole
        data(3 * MIB - 1, 2); // the last byte of a buffer and the first of the next
        data(4 * MIB + 5000, 70_000); // not on a block's edge
        data(19 * MIB, 4096); // 15 MiB of zeros in front: APFS fills that
        data(37 * MIB, 3 * MIB + 17); // 18 MiB in front: APFS keeps that; a run longer than a buffer
        data(100 * MIB, 4096); // 60 MiB in front
        // the last 50 MiB and the 1000 bytes of the tail are zeros
        all.push(Layout { name: "runs", stream: s });
        // a data byte at the very end, in a tail that is not a whole block
        let mut s = vec![0u8; 40 * MIB + 1000];
        *s.last_mut().unwrap() = 7;
        all.push(Layout { name: "last byte", stream: s });
        // data that begins and ends inside blocks, zeros in front and behind, in a small file
        let mut s = vec![0u8; 300_001];
        s[100_000..200_000].fill(9);
        all.push(Layout { name: "small", stream: s });
        all.push(Layout { name: "all zero", stream: vec![0u8; 40 * MIB + 1000] });
        all.push(Layout { name: "one zero byte", stream: vec![0u8; 1] });
        all.push(Layout { name: "empty", stream: Vec::new() });
        all.push(Layout { name: "no zeros", stream: vec![5u8; 3 * MIB + 3] });
        all
    }

    /// The ranges of whole blocks that hold a byte that is not zero: what must be data, and nothing else.
    fn data_runs(stream: &[u8], block: usize) -> Vec<(u64, u64)> {
        let mut runs: Vec<(u64, u64)> = Vec::new();
        for (i, c) in stream.chunks(block).enumerate() {
            if c.iter().any(|&b| b != 0) {
                let (from, to) = ((i * block) as u64, (i * block + c.len()) as u64);
                match runs.last_mut() {
                    Some(last) if last.1 == from => last.1 = to,
                    _ => runs.push((from, to)),
                }
            }
        }
        runs
    }

    /// The file's data regions, as the file system reports them (SEEK_DATA, SEEK_HOLE).
    fn data_map(file: &File) -> io::Result<Vec<(u64, u64)>> {
        let fd = file.as_raw_fd();
        let mut map = Vec::new();
        let mut at: libc::off_t = 0;
        loop {
            // SAFETY: lseek takes no pointer.
            let data = unsafe { libc::lseek(fd, at, libc::SEEK_DATA) };
            if data < 0 {
                let e = io::Error::last_os_error();
                return if e.raw_os_error() == Some(libc::ENXIO) { Ok(map) } else { Err(e) };
            }
            let hole = unsafe { libc::lseek(fd, data, libc::SEEK_HOLE) };
            if hole <= data {
                return if hole < 0 { Err(io::Error::last_os_error()) } else { Ok(map) };
            }
            map.push((data as u64, hole as u64));
            at = hole;
        }
    }

    /// `map` on the edges of blocks (a file system may report finer or to the end of the last block), joined.
    fn on_blocks(map: &[(u64, u64)], block: u64, size: u64) -> Vec<(u64, u64)> {
        let mut out: Vec<(u64, u64)> = Vec::new();
        for &(from, to) in map {
            let (from, to) = (from - from % block, to.next_multiple_of(block).min(size));
            match out.last_mut() {
                Some(last) if last.1 >= from => last.1 = last.1.max(to),
                _ => out.push((from, to)),
            }
        }
        out
    }

    fn show(map: &[(u64, u64)]) -> String {
        if map.is_empty() {
            return "none".into();
        }
        map.iter().map(|(a, b)| format!("{a:#x}..{b:#x}")).collect::<Vec<_>>().join(" ")
    }

    fn total(map: &[(u64, u64)]) -> u64 {
        map.iter().map(|(a, b)| b - a).sum()
    }

    /// The folders to test in: ONEIOS_SPARSE_TEST_DIRS (':' between them; one per file system), else the
    /// temporary folder.
    fn folders() -> Vec<PathBuf> {
        match std::env::var("ONEIOS_SPARSE_TEST_DIRS") {
            Ok(dirs) if !dirs.is_empty() => dirs.split(':').map(PathBuf::from).collect(),
            _ => vec![std::env::temp_dir()],
        }
    }

    /// Whether a range of `dir`'s file system that was never written is a hole (HFS+, FAT and exFAT have none).
    fn keeps_holes(dir: &std::path::Path) -> bool {
        let path = dir.join(format!("oneios-sparse-probe-{}", std::process::id()));
        let file = File::options().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
        file.set_len(64 << 20).unwrap();
        file.sync_all().unwrap();
        let map = data_map(&file);
        let blocks = file.metadata().unwrap().blocks();
        std::fs::remove_file(&path).unwrap();
        matches!(map, Ok(m) if m.is_empty()) && blocks * 512 < 1 << 20
    }

    /// Every layout into a file of each folder, with the punch on and off: the bytes are the stream's, and the
    /// data regions the file system reports and the blocks it counts are what this file system is known to do.
    /// Prints what it saw (`cargo test -p oneios-phone sparse -- --nocapture`).
    #[test]
    fn holes_are_what_the_file_system_keeps_and_the_bytes_are_the_stream() {
        let layouts = layouts();
        for dir in folders() {
            let holes = keeps_holes(&dir);
            for punch in [cfg!(target_os = "macos"), !cfg!(target_os = "macos")] {
                for (n, layout) in layouts.iter().enumerate() {
                    let path = dir.join(format!("oneios-sparse-{}-{n}", std::process::id()));
                    let file = File::options().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
                    let block = hole_block(&file);
                    if n == 0 {
                        println!("--- {}: block {block}, page {}, punch {}{}", dir.display(), unsafe { libc::sysconf(libc::_SC_PAGESIZE) },
                            if punch { "on" } else { "off" }, if holes { "" } else { "; this file system keeps no holes: only the bytes are checked" });
                    }
                    let stream = &layout.stream;
                    let mut calls = 0;
                    let w = copy_to(&stream[..], &file, block, punch, |_, _| calls += 1).unwrap();
                    file.sync_all().unwrap(); // APFS allocates when it writes out; the map of a file not yet written out is not to be trusted
                    let (size, bl) = (stream.len() as u64, block as u64);
                    let want = data_runs(stream, block);
                    let meta = file.metadata().unwrap();
                    let map = data_map(&file).unwrap();
                    let allocated = meta.blocks() * 512;
                    println!("{:>13}: {size} bytes, {} written in {} runs, {} punched; st_blocks {} ({allocated} bytes); data: {}",
                        layout.name, w.data, want.len(), w.punched, meta.blocks(), show(&map));
                    assert_eq!((w.size, w.block, meta.len()), (size, bl, size), "{}", layout.name);
                    assert_eq!(w.data, total(&want), "{}: the bytes written are the blocks that hold data", layout.name);
                    assert_eq!(calls, stream.len().div_ceil(BUFFER), "{}: progress after every buffer", layout.name);
                    assert!(std::fs::read(&path).unwrap() == *stream, "{}: the file is not the stream", layout.name);
                    std::fs::remove_file(&path).unwrap();
                    if !holes {
                        continue;
                    }
                    let got = on_blocks(&map, bl, size);
                    // Blocks beyond the data: an extent or node block of the file's own (ext4, f2fs count them).
                    let spare = 16 * bl + total(&want) / 100;
                    if cfg!(target_os = "macos") && !punch {
                        // APFS as it is: a hole shorter than about 16 MiB (in front of a write; here also at the end)
                        // may be filled. Longer ones stay.
                        let filled: u64 = want.iter().copied().chain([(size, size)]).scan(0u64, |end, (from, to)| {
                            let gap = from - *end;
                            *end = to;
                            Some(if gap <= 32 << 20 { gap } else { 0 })
                        }).sum();
                        assert!(total(&got) <= total(&want) + filled, "{}: data {} where holes of more than 32 MiB were left", layout.name, show(&got));
                        assert!(allocated <= total(&want) + filled + spare, "{}: {allocated} bytes allocated", layout.name);
                    } else {
                        // Linux (ext4, btrfs, f2fs, xfs, tmpfs), and APFS with the punch: every block not written is a hole.
                        assert_eq!(show(&got), show(&want), "{}: the data regions are not the blocks that hold data", layout.name);
                        assert!(allocated <= total(&want) + spare, "{}: {allocated} bytes allocated for {} of data", layout.name, total(&want));
                    }
                }
            }
            // What a punch costs here (macOS punches every hole in front of a write).
            let path = dir.join(format!("oneios-sparse-cost-{}", std::process::id()));
            let file = File::create(&path).unwrap();
            let block = hole_block(&file) as u64;
            file.write_all_at(&vec![1u8; 2000 * block as usize], 0).unwrap();
            let start = std::time::Instant::now();
            let done = (0..1000).filter(|i| punch_hole(&file, i * 2 * block, block).is_ok()).count();
            println!("  1000 punches of one block in written data: {done} done, {:.1} ms", start.elapsed().as_secs_f64() * 1e3);
            std::fs::remove_file(&path).unwrap();
        }
    }

    /// A file system as APFS was measured (see the top of this file): 4096-byte blocks; a write allocates whole
    /// 16 KiB pages and fills a hole shorter than 16 MiB in front of it; a punch must be whole blocks inside the
    /// file.
    struct Apfs {
        bytes: Vec<u8>,
        allocated: Vec<bool>,
        punches: usize,
    }

    impl Apfs {
        const BLOCK: usize = 4096;
        const PAGE: usize = 16384;
        const FILL: usize = 16 << 20;

        fn allocate(&mut self, from: usize, to: usize) {
            if self.allocated.len() < to.div_ceil(Self::BLOCK) {
                self.allocated.resize(to.div_ceil(Self::BLOCK), false);
            }
            self.allocated[from / Self::BLOCK..to.div_ceil(Self::BLOCK)].fill(true);
        }

        /// The allocated ranges, on pages.
        fn map(&self) -> Vec<(u64, u64)> {
            let ranges: Vec<(u64, u64)> = self.allocated.iter().enumerate().filter(|(_, a)| **a)
                .map(|(i, _)| ((i * Self::BLOCK) as u64, ((i + 1) * Self::BLOCK) as u64)).collect();
            on_blocks(&ranges, Self::PAGE as u64, self.bytes.len() as u64)
        }
    }

    impl Sink for &mut Apfs {
        fn write_at(&mut self, bytes: &[u8], at: u64) -> io::Result<()> {
            let (at, end) = (at as usize, at as usize + bytes.len());
            let before = self.allocated.iter().take(at / Apfs::BLOCK).rposition(|a| *a).map_or(0, |i| (i + 1) * Apfs::BLOCK);
            if at > before && at - before < Apfs::FILL {
                self.allocate(before, at);
            }
            self.allocate(at - at % Apfs::PAGE, end.next_multiple_of(Apfs::PAGE));
            if self.bytes.len() < end {
                self.bytes.resize(end, 0);
            }
            self.bytes[at..end].copy_from_slice(bytes);
            Ok(())
        }
        fn punch(&mut self, at: u64, len: u64) -> io::Result<()> {
            let (at, len) = (at as usize, len as usize);
            if at % Apfs::BLOCK != 0 || len % Apfs::BLOCK != 0 || at + len > self.bytes.len() {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
            self.bytes[at..at + len].fill(0);
            let last = (at + len).div_ceil(Apfs::BLOCK).min(self.allocated.len());
            self.allocated[(at / Apfs::BLOCK).min(last)..last].fill(false);
            self.punches += 1;
            Ok(())
        }
        fn set_len(&mut self, len: u64) -> io::Result<()> {
            self.bytes.resize(len as usize, 0);
            Ok(())
        }
    }

    /// The writer against the model of APFS: without the punch the short holes are filled (what the Mac's test
    /// of the first port saw); with it, the allocated pages are the pages that hold data, and no punch was refused
    /// or took a byte of data.
    #[test]
    fn on_a_model_of_apfs_the_punch_gives_the_filled_holes_back() {
        for layout in layouts() {
            let stream = &layout.stream;
            let want = data_runs(stream, Apfs::PAGE);
            for punch in [false, true] {
                let mut fs = Apfs { bytes: Vec::new(), allocated: Vec::new(), punches: 0 };
                let w = copy_to(&stream[..], &mut fs, Apfs::PAGE, punch, |_, _| {}).unwrap();
                assert!(fs.bytes == *stream, "{}: the file is not the stream", layout.name);
                assert_eq!(w.punched as usize, fs.punches, "{}: a punch was refused", layout.name);
                let got = fs.map();
                println!("{:>13}: punch {punch}: {} punched, {} bytes allocated for {} of data", layout.name, w.punched, total(&got), total(&want));
                if punch {
                    assert_eq!(show(&got), show(&want), "{}", layout.name);
                } else if layout.name == "runs" {
                    // the holes of 1 block, 3 MiB and 15 MiB are filled; those of 18 and 60 MiB and the end are not
                    assert_eq!(total(&got), 19 * MIB as u64 + 16384 + (3 * MIB as u64 + 16384) + 16384, "{}", show(&got));
                }
            }
        }
    }

    #[test]
    fn a_block_of_zeros_is_told_from_one_with_a_byte() {
        for len in [0, 1, 511, 4096, 16384, (1 << 16) + 1, 1 << 20] {
            let mut block = vec![0u8; len];
            assert!(is_zero(&block), "{len}");
            for at in [0, len / 2, len.saturating_sub(1)] {
                if len > 0 {
                    block[at] = 1;
                    assert!(!is_zero(&block), "{len} {at}");
                    block[at] = 0;
                }
            }
        }
    }

    /// A stream that fails, and a file that cannot be written: the error says which.
    #[test]
    fn errors_of_the_stream_and_of_the_file_are_reported() {
        struct Fails(usize);
        impl Read for Fails {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0 == 0 {
                    return Err(io::Error::other("the pipe broke"));
                }
                let n = buf.len().min(self.0);
                buf[..n].fill(1);
                self.0 -= n;
                Ok(n)
            }
        }
        let mut fs = Apfs { bytes: Vec::new(), allocated: Vec::new(), punches: 0 };
        let e = copy_to(Fails(3 * BUFFER + 5), &mut fs, 4096, false, |_, _| {}).err().unwrap();
        assert_eq!(format!("{e:#}"), "reading the stream: the pipe broke");
        assert_eq!(fs.bytes.len(), 3 * BUFFER, "what came whole before the error is written");

        struct Full;
        impl Sink for Full {
            fn write_at(&mut self, _: &[u8], _: u64) -> io::Result<()> {
                Err(io::Error::other("no space"))
            }
            fn punch(&mut self, _: u64, _: u64) -> io::Result<()> {
                Ok(())
            }
            fn set_len(&mut self, _: u64) -> io::Result<()> {
                Ok(())
            }
        }
        let e = copy_to(&vec![1u8; 20 * BUFFER][..], Full, 4096, false, |_, _| {}).err().unwrap();
        assert_eq!(format!("{e:#}"), "writing: no space");
    }
}
