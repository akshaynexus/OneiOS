//! `oneios-tool phone pull-data OUT`: the Data store (data/data.img) from the phone, read only, no session
//! running. A copy that goes on where it stopped, and that is checked against the phone's own sums.
//!
//! Why: over wireless adb the one `busybox gzip -1 -c data.img` stream of the first port broke twice
//! ("incomplete deflate stream", after 1.6 and after 0.6 of 32 GiB, 2026-11-13) with adb still connected, and
//! each time the copy began at zero again.
//!
//! How (README "phone", "The copy that goes on"):
//! - the store is copied in chunks (`--chunk` MiB, 256). Each chunk is one adb stream that reads one range of
//!   the one named file: `busybox dd if=.../data.img bs=1M skip=N count=M 2>/dev/null | busybox gzip -1 -c`
//!   (dd seeks to the range; its statistics on stderr are dropped, they would follow the gzip stream on a phone
//!   whose adb joins stdout and stderr). It is unpacked and written at its offset into `OUT.part`, zero blocks
//!   as holes (`sparse::copy_at`). `OUT.part` has the store's length from the start;
//! - the phone's SHA-256 of the same range (`... | busybox sha256sum`: one more read of the same file) is
//!   compared with the SHA-256 of the range read back from `OUT.part` (a hole reads as zeros). `--verify chunk`
//!   (the default) asks for the sum first: a chunk whose sum is that of zeros is a hole and is not copied at
//!   all. `--verify end` copies every chunk and asks for the sums afterwards;
//! - `OUT.part.progress` (text, a line appended per event, written out before the next step) names the chunks
//!   that are copied (length, SHA-256 as read back, bytes written, bytes over adb) and verified. A run that finds
//!   it goes on at the first chunk that is not done; `--restart` throws it and `OUT.part` away. Its head holds
//!   the store's size and modification time: a store that changed since is refused;
//! - a chunk whose stream breaks, comes short, stalls (`--stall`) or whose sum differs is cleared and copied
//!   again, `--retries` times with a pause that doubles from `--backoff` seconds; then the command ends and says
//!   how to go on;
//! - `OUT.part` becomes `OUT` only when every chunk is verified, the lengths add up to the store's, and the
//!   store's size and modification time are what they were at the start.

use crate::adb::{last_lines, Adb, BUSYBOX, STAGE};
use crate::{log, sparse};
use anyhow::{anyhow, bail, Context, Result};
use std::cell::Cell;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, IsTerminal, Read, Write};
use std::os::unix::io::AsRawFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

const MIB: u64 = 1 << 20;
/// The first line of a progress file. A file of another format is not gone on from.
const HEAD: &str = "oneios-tool phone pull-data progress 1";
/// The longest pause before a retry, in seconds.
const BACKOFF_MAX: u64 = 30;

/// When the phone's sum of a chunk is asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum Verify {
    /// Before the chunk is copied: a chunk of zeros is not copied, and a chunk that differs is copied again at once
    Chunk,
    /// After every chunk is copied
    End,
}

pub struct Options {
    /// A chunk's size in MiB; None: 256, or what the copy that is gone on from began with.
    pub chunk: Option<u64>,
    pub retries: u32,
    /// Seconds before the first retry of a chunk; doubled for each further one.
    pub backoff: u64,
    /// A stream that gave no byte for this many seconds is ended (0: never).
    pub stall: u64,
    pub verify: Verify,
    pub restart: bool,
    /// Then `ioslog boots` and `ioslog checks` on the copy.
    pub ioslog: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options { chunk: None, retries: 5, backoff: 2, stall: 120, verify: Verify::Chunk, restart: false, ioslog: false }
    }
}

// ---------------------------------------------------------------- the progress file

/// What a copy is of: a progress file goes with exactly this file of the phone, in this state.
#[derive(Clone, Debug, PartialEq)]
struct Head {
    image: String,
    size: u64,
    /// the store's modification time on the phone (`stat -c %Y`), seconds
    mtime: u64,
    /// a chunk's size in bytes
    chunk: u64,
}

impl Head {
    fn chunks(&self) -> usize {
        self.size.div_ceil(self.chunk) as usize
    }
    /// Chunk `i`: its offset and its length (the last one may be shorter).
    fn range(&self, i: usize) -> (u64, u64) {
        let at = i as u64 * self.chunk;
        (at, self.chunk.min(self.size - at))
    }
    fn text(&self) -> String {
        format!("{HEAD}\nimage {}\nsize {}\nmtime {}\nchunk {}\n", self.image, self.size, self.mtime, self.chunk)
    }
}

/// A chunk as it was written: its length, the SHA-256 of its range read back from `OUT.part`, the bytes written
/// (the rest are holes) and the bytes its stream moved over adb.
#[derive(Clone, Debug, PartialEq)]
struct Copied {
    len: u64,
    sha: String,
    data: u64,
    wire: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Chunk {
    copied: Option<Copied>,
    /// the phone's sum of the range was `copied`'s
    verified: bool,
}

fn is_sha256(word: &str) -> bool {
    word.len() == 64 && word.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A progress file's text. Only whole lines count (a line that was being written when the run was killed has no
/// end). None: the head is not whole, so nothing was copied yet. Err: not a progress file, or a line that cannot
/// be one of this copy.
fn parse(text: &str) -> Result<Option<(Head, Vec<Chunk>)>> {
    let whole = &text[..text.rfind('\n').map_or(0, |end| end + 1)];
    let mut lines = whole.lines();
    match lines.next() {
        None => return Ok(None),
        Some(first) if first != HEAD => bail!("its first line is \"{}\", not \"{HEAD}\"", first.chars().take(60).collect::<String>()),
        Some(_) => {}
    }
    let mut field = |name: &str| -> Result<Option<String>> {
        match lines.next() {
            None => Ok(None),
            Some(line) => match line.split_once(' ') {
                Some((key, value)) if key == name => Ok(Some(value.to_string())),
                _ => bail!("the line \"{line}\" is where \"{name} ...\" belongs"),
            },
        }
    };
    let number = |name: &str, value: Option<String>| -> Result<Option<u64>> {
        value.map(|v| v.parse::<u64>().map_err(|_| anyhow!("{name} is \"{v}\""))).transpose()
    };
    let image = field("image")?;
    let size = number("size", field("size")?)?;
    let mtime = number("mtime", field("mtime")?)?;
    let chunk = number("chunk", field("chunk")?)?;
    let (Some(image), Some(size), Some(mtime), Some(chunk)) = (image, size, mtime, chunk) else { return Ok(None) };
    if chunk == 0 || chunk % MIB != 0 {
        bail!("its chunk size is {chunk} bytes, not whole MiB");
    }
    let head = Head { image, size, mtime, chunk };
    let n = head.chunks();
    let mut chunks = vec![Chunk::default(); n];
    for line in lines {
        let words: Vec<&str> = line.split(' ').collect();
        let index = |word: &str| word.parse::<usize>().ok().filter(|i| *i < n);
        match words[..] {
            ["copied", i, len, sha, data, wire] => {
                let (Some(i), Ok(len), Ok(data), Ok(wire)) = (index(i), len.parse::<u64>(), data.parse::<u64>(), wire.parse::<u64>()) else {
                    bail!("the line \"{line}\" is not a chunk of this copy")
                };
                if len != head.range(i).1 || !is_sha256(sha) || data > len {
                    bail!("the line \"{line}\" is not a chunk of this copy");
                }
                chunks[i] = Chunk { copied: Some(Copied { len, sha: sha.to_string(), data, wire }), verified: false };
            }
            ["verified", i, sha] => {
                let Some(i) = index(i) else { bail!("the line \"{line}\" is not a chunk of this copy") };
                // only the copy that is there now: a chunk copied again after it was verified is not
                chunks[i].verified = chunks[i].copied.as_ref().is_some_and(|c| c.sha == sha);
            }
            _ => bail!("the line \"{line}\" is none of this tool's"),
        }
    }
    Ok(Some((head, chunks)))
}

/// The progress file, open for the lines this run adds, and what it says so far.
struct Progress {
    file: File,
    chunks: Vec<Chunk>,
}

impl Progress {
    /// One event: the line is on the disk before the run goes on (a line per chunk: 128 for the 32 GiB store).
    fn line(&mut self, line: String) -> Result<()> {
        self.file.write_all(format!("{line}\n").as_bytes()).and_then(|_| self.file.sync_data()).context("writing the progress file")
    }
    fn copied(&mut self, i: usize, c: &Copied) -> Result<()> {
        self.line(format!("copied {i} {} {} {} {}", c.len, c.sha, c.data, c.wire))?;
        self.chunks[i] = Chunk { copied: Some(c.clone()), verified: false };
        Ok(())
    }
    fn verified(&mut self, i: usize) -> Result<()> {
        let sha = self.chunks[i].copied.as_ref().expect("a verified chunk is copied").sha.clone();
        self.line(format!("verified {i} {sha}"))?;
        self.chunks[i].verified = true;
        Ok(())
    }
    fn done(&self, i: usize) -> bool {
        self.chunks[i].copied.is_some() && self.chunks[i].verified
    }
}

/// Only one run writes a copy: the progress file is locked until the run ends (also when it is killed).
fn lock(file: &File, path: &str) -> Result<()> {
    // SAFETY: flock takes no pointer.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("another oneios-tool is writing this copy ({path} is locked): wait for it or end it");
    }
    Ok(())
}

// ---------------------------------------------------------------- the phone's side

/// The store's size and modification time: one `stat` of the one named file.
fn stat(adb: &Adb, image: &str) -> Result<(u64, u64)> {
    let out = adb.root_ok(&format!("stat -c '%s %Y' {image}\n"), "reading data.img's size and time")?;
    let mut words = out.split_whitespace().map(|w| w.parse::<u64>());
    match (words.next(), words.next()) {
        (Some(Ok(size)), Some(Ok(mtime))) => Ok((size, mtime)),
        _ => bail!("data.img's size and time are \"{}\"", out.trim()),
    }
}

/// The command that reads chunk `i`'s range of the store on the phone, for a pipe. dd seeks (skip= of a file),
/// so a chunk costs its own bytes only. Its statistics go to /dev/null: on stdout's side of an adb that joins
/// the two they would follow the stream.
fn range_reader(head: &Head, i: usize) -> String {
    let (at, len) = head.range(i);
    format!("{BUSYBOX} dd if={} bs=1M skip={} count={} 2>/dev/null", head.image, at / MIB, len.div_ceil(MIB))
}

/// adb's stdout, counted. A read does not wait for ever: it ends with an error when Ctrl-C came, and when the
/// stream gave nothing for `stall` seconds (a connection that stands still without ending; 0: never). The pipe
/// is polled, so that neither needs the stream to end first (a process behind adb may hold it open).
struct Wire {
    from: std::process::ChildStdout,
    /// the bytes read so far, also for the progress line
    seen: Rc<Cell<u64>>,
    last: Instant,
    stall: u64,
}

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if log::stopped() {
                return Err(io::Error::other("stopped"));
            }
            if self.stall > 0 && self.last.elapsed() >= Duration::from_secs(self.stall) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, format!("the stream gave nothing for {} s and was ended (--stall)", self.stall)));
            }
            let mut pipe = libc::pollfd { fd: self.from.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: poll reads and writes the one pollfd it is given.
            match unsafe { libc::poll(&mut pipe, 1, 250) } {
                0 => continue,
                -1 => match io::Error::last_os_error() {
                    e if e.kind() == io::ErrorKind::Interrupted => continue, // a signal: Ctrl-C is seen above
                    e => return Err(e),
                },
                _ => {}
            }
            let n = self.from.read(buf)?;
            self.seen.set(self.seen.get() + n as u64);
            self.last = Instant::now();
            return Ok(n);
        }
    }
}

// ---------------------------------------------------------------- the copy

/// What this run did (the progress file has the copy's whole story).
#[derive(Default)]
struct Run {
    /// bytes of the store whose chunks this run finished
    covered: u64,
    /// chunks this run copied through a stream, and chunks of zeros it did not have to copy
    copied: u64,
    zeros: u64,
    /// attempts that were repeated: a stream that broke, a sum that differed
    retries: u64,
    /// chunks whose first copy did not have the phone's sum
    differed: u64,
}

/// What became of a chunk in one attempt.
enum Did {
    /// the phone's sum is that of zeros: a hole, nothing was copied
    Zeros,
    /// copied and the phone's sum is the copy's (`again`: over a copy that did not have it)
    Copied { c: Copied, again: bool },
    /// copied; the sum is asked at the end (`--verify end`)
    Unverified(Copied),
    /// a copy made before has the phone's sum
    Agreed,
}

struct Job<'a> {
    adb: &'a Adb,
    o: &'a Options,
    head: Head,
    part: String,
    file: File,
    progress: Progress,
    start: Instant,
    run: Run,
    /// bytes over adb in this run, the streams that broke too
    wire: Cell<u64>,
    /// `OUT.part` is this run's own: a range it did not write to yet reads as zeros
    fresh: bool,
    /// chunks this run wrote to
    tried: Vec<bool>,
    /// the file system's map of holes is used when a range is read back (`sparse::range_sha256`)
    map: bool,
    zero_sums: HashMap<u64, String>,
    last_line: Instant,
}

fn gib(n: u64) -> f64 {
    n as f64 / (1u64 << 30) as f64
}

/// MB/s (10^6 bytes) of `bytes` in `time`.
fn rate(bytes: u64, time: Duration) -> f64 {
    bytes as f64 / 1e6 / time.as_secs_f64().max(1e-3)
}

impl Job<'_> {
    fn n(&self) -> usize {
        self.head.chunks()
    }

    /// `chunk 3/128` and where it is.
    fn name(&self, i: usize) -> String {
        format!("chunk {}/{} at {:.2} GiB", i + 1, self.n(), gib(self.head.range(i).0))
    }

    fn zeros(&mut self, len: u64) -> String {
        self.zero_sums.entry(len).or_insert_with(|| sparse::zeros_sha256(len)).clone()
    }

    /// One adb stream of `cmd`, read by `read`; with it the bytes it moved. Err: the reader's error (the stream
    /// broke, stood still for `--stall` seconds, or Ctrl-C came), or adb's status.
    fn stream<T>(&self, cmd: &str, read: impl FnOnce(&mut Wire) -> Result<T>) -> Result<(T, u64)> {
        let mut child = self.adb.exec_out(cmd)?;
        let seen = Rc::new(Cell::new(0));
        let mut wire = Wire { from: child.stdout.take().expect("piped"), seen: seen.clone(), last: Instant::now(), stall: self.o.stall };
        let result = read(&mut wire);
        drop(wire);
        if result.is_err() {
            let _ = child.kill(); // a reader that gave up: adb may still be writing, or waiting
        }
        let status = child.wait().context("waiting for adb")?;
        self.wire.set(self.wire.get() + seen.get());
        match result {
            Err(e) => Err(e),
            Ok(_) if !status.success() => bail!("adb exec-out ended with {status}"),
            Ok(value) => Ok((value, seen.get())),
        }
    }

    /// The phone's SHA-256 of chunk `i`'s range: one more read of the same named file.
    fn phone_sum(&self, i: usize) -> Result<String> {
        let cmd = format!("su -c '{} | {BUSYBOX} sha256sum'", range_reader(&self.head, i));
        let (said, _) = self.stream(&cmd, |wire| {
            let mut said = String::new();
            wire.take(4096).read_to_string(&mut said).context("reading the phone's sha256sum")?;
            Ok(said)
        })?;
        match said.split_whitespace().next() {
            Some(sum) if is_sha256(sum) => Ok(sum.to_string()),
            _ => bail!("the phone's sha256sum of the range gave \"{}\"", last_lines(&said, 1)),
        }
    }

    /// The range read back from `OUT.part`, a hole as zeros.
    fn local_sum(&mut self, at: u64, len: u64) -> Result<String> {
        match sparse::range_sha256(&self.file, at, len, self.map)? {
            Some(sum) => Ok(sum),
            None => Ok(self.zeros(len)),
        }
    }

    /// Before a range is written (again): what an attempt that broke, or an earlier run, may have left there goes.
    fn clear(&mut self, i: usize) -> Result<()> {
        let (at, len) = self.head.range(i);
        if !self.fresh || self.tried[i] {
            sparse::clear(&self.file, at, len).with_context(|| format!("clearing the range in {}", self.part))?;
        }
        self.tried[i] = true;
        Ok(())
    }

    /// Chunk `i` from the phone into its range: one stream, unpacked, zero blocks as holes; then written out and
    /// read back for its sum.
    fn copy(&mut self, i: usize) -> Result<Copied> {
        let (at, len) = self.head.range(i);
        self.clear(i)?;
        let cmd = format!("su -c '{} | {BUSYBOX} gzip -1 -c'", range_reader(&self.head, i));
        let (name, begun, total) = (self.name(i), Instant::now(), self.head.size);
        let (file, mut last, terminal) = (&self.file, self.last_line, io::stderr().is_terminal());
        let ((written, longer), wire) = self.stream(&cmd, |wire| {
            let seen = wire.seen.clone();
            let mut unpacked = flate2::read::MultiGzDecoder::new(BufReader::with_capacity(1 << 20, wire));
            // not a byte past the range is written, whatever the stream gives
            let written = sparse::copy_at((&mut unpacked).take(len), file, at, |size, data| {
                if last.elapsed() >= Duration::from_secs(2) {
                    last = Instant::now();
                    let over_adb = seen.get();
                    eprint!(
                        "{name}: {:.2} of {:.2} GiB read, {:.1} MB written, {:.1} MB over adb, {:.1} MB/s ({:.2} MB/s over adb){}",
                        gib(at + size),
                        gib(total),
                        data as f64 / 1e6,
                        over_adb as f64 / 1e6,
                        rate(size, begun.elapsed()),
                        rate(over_adb, begun.elapsed()),
                        if terminal { "\x1b[K\r" } else { "\n" }
                    );
                }
            })?;
            // the stream must end here: this read also checks the gzip stream's own end (its CRC and length)
            let mut one = [0u8; 1];
            let longer = loop {
                match unpacked.read(&mut one) {
                    Ok(n) => break n > 0,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e).context("reading the stream"),
                }
            };
            Ok((written, longer))
        })?;
        self.last_line = last;
        if longer || written.size != len {
            bail!("the stream gave {}{} bytes, the range has {len}", if longer { "more than " } else { "" }, written.size);
        }
        self.file.sync_data().with_context(|| format!("writing {} out", self.part))?;
        let sha = self.local_sum(at, len)?;
        Ok(Copied { len, sha, data: written.data, wire })
    }

    /// One attempt at chunk `i`. `sum`: the phone's sum is asked now (else the chunk is only copied).
    fn attempt(&mut self, i: usize, sum: bool) -> Result<Did> {
        let (at, len) = self.head.range(i);
        if !sum {
            let c = self.copy(i)?;
            self.progress.copied(i, &c)?;
            return Ok(Did::Unverified(c));
        }
        let phone = self.phone_sum(i)?;
        let before = self.progress.chunks[i].copied.as_ref().map(|c| c.sha.clone());
        if before.as_deref() == Some(phone.as_str()) {
            self.progress.verified(i)?;
            return Ok(Did::Agreed);
        }
        if phone == self.zeros(len) {
            // nothing to copy: the range is a hole (made one, if something was written there)
            self.clear(i)?;
            let local = self.local_sum(at, len)?;
            if local != phone {
                bail!("the phone's range is zeros, the range of {} reads back as {local}", self.part);
            }
            self.progress.copied(i, &Copied { len, sha: local, data: 0, wire: 0 })?;
            self.progress.verified(i)?;
            return Ok(Did::Zeros);
        }
        let mut c = self.copy(i)?;
        if c.sha != phone && self.map {
            // before the copy is called wrong: the same range with every byte read, in case this file system's
            // map of holes is not what the file reads as
            if let Some(whole) = sparse::range_sha256(&self.file, at, len, false)? {
                if whole == phone {
                    eprintln!("pull-data: this file system's map of holes does not match what the file reads as: from here on every byte is read back");
                    self.map = false;
                    c.sha = whole;
                }
            }
        }
        self.progress.copied(i, &c)?;
        if c.sha != phone {
            self.run.differed += 1;
            bail!("the copy's sha256 {} is not the phone's {} of the same range", &c.sha[..16], &phone[..16]);
        }
        self.progress.verified(i)?;
        Ok(Did::Copied { c, again: before.is_some() })
    }

    /// The store is not the one the copy began with: `now` is its size and modification time.
    fn changed(&self, now: (u64, u64)) -> anyhow::Error {
        anyhow!(
            "data.img changed on the phone while it was copied (size {} -> {}, modified {} -> {}): the copy may mix two states of it and is not kept. {} stays; begin again with --restart, with no session open",
            self.head.size,
            now.0,
            self.head.mtime,
            now.1,
            self.part
        )
    }

    fn stopped(&self, i: usize) -> anyhow::Error {
        anyhow!("stopped at {} (Ctrl-C). {}", self.name(i), self.how_to_go_on(i))
    }

    fn how_to_go_on(&self, i: usize) -> String {
        let done = (0..self.n()).filter(|i| self.progress.done(*i)).count();
        format!(
            "{done} of {} chunks are done and stay in {} ({}.progress lists them): run the same command again and it goes on at chunk {}; --restart begins again",
            self.n(),
            self.part,
            self.part,
            i + 1
        )
    }

    /// Chunk `i` until an attempt ends well, at most `--retries` more times, each after a pause.
    fn chunk(&mut self, i: usize, sum: bool) -> Result<()> {
        let (at, len) = self.head.range(i);
        let mut failed = 0;
        loop {
            if log::stopped() {
                return Err(self.stopped(i));
            }
            let (begun, wire) = (Instant::now(), self.wire.get());
            let error = match self.attempt(i, sum) {
                Ok(did) => {
                    // the second pass of --verify end moves no byte of the store: it adds nothing to the rate
                    if !(matches!(did, Did::Agreed) && self.o.verify == Verify::End) {
                        self.run.covered += len;
                    }
                    self.say(i, &did, begun.elapsed(), self.wire.get() - wire);
                    return Ok(());
                }
                Err(e) => e,
            };
            if log::stopped() {
                return Err(self.stopped(i));
            }
            // a store that changed is why a range no longer has its length or its sum: said now, not after the retries
            if let Ok(now) = stat(self.adb, &self.head.image) {
                if now != (self.head.size, self.head.mtime) {
                    return Err(self.changed(now));
                }
            }
            if failed >= self.o.retries {
                bail!("{} (offset {at}) failed {} time(s), the last time: {error:#}. {}", self.name(i), failed + 1, self.how_to_go_on(i));
            }
            failed += 1;
            self.run.retries += 1;
            let pause = self.o.backoff.saturating_mul(1 << (failed - 1).min(16)).min(BACKOFF_MAX);
            eprintln!(
                "{}: {error:#} ({:.1} s, {} bytes over adb). The chunk starts again at offset {at} in {pause} s (retry {failed} of {})",
                self.name(i),
                begun.elapsed().as_secs_f64(),
                self.wire.get() - wire,
                self.o.retries
            );
            if log::sleep_or_stop(Duration::from_secs(pause)) {
                return Err(self.stopped(i));
            }
        }
    }

    /// A chunk's line: what it was, its time and MB/s (of the store, and over adb), and the run so far.
    fn say(&mut self, i: usize, did: &Did, time: Duration, wire: u64) {
        let len = self.head.range(i).1;
        let seconds = time.as_secs_f64();
        let what = match did {
            Did::Zeros => format!("zeros (the phone's sha256): a hole, not copied; {seconds:.1} s, {:.1} MB/s", rate(len, time)),
            Did::Agreed => format!("the phone's sha256 is the copy's ({seconds:.1} s)"),
            Did::Copied { c, again } => format!(
                "{} bytes, {} written, {} over adb, {seconds:.1} s, {:.1} MB/s ({:.2} MB/s over adb); the phone's sha256 is the copy's{}",
                c.len,
                c.data,
                c.wire,
                rate(len, time),
                rate(wire, time),
                if *again { " (copied again: the copy before it had another)" } else { "" }
            ),
            Did::Unverified(c) => format!(
                "{} bytes, {} written, {} over adb, {seconds:.1} s, {:.1} MB/s ({:.2} MB/s over adb); sha256 at the end",
                c.len,
                c.data,
                c.wire,
                rate(len, time),
                rate(wire, time)
            ),
        };
        match did {
            Did::Zeros => self.run.zeros += 1,
            Did::Copied { .. } | Did::Unverified(_) => self.run.copied += 1,
            Did::Agreed => {}
        }
        let covered = self.run.covered;
        let elapsed = self.start.elapsed();
        eprintln!(
            "{}: {what} | this run: {:.2} GiB in {:.0} s, {:.1} MB/s ({:.2} MB/s over adb)",
            self.name(i),
            gib(covered),
            elapsed.as_secs_f64(),
            rate(covered, elapsed),
            rate(self.wire.get(), elapsed)
        );
        self.last_line = Instant::now();
    }
}

/// Copy the Data store (data/data.img) from the phone into `out`: see the top of this file.
pub fn pull_data(adb: &Adb, out: &str, o: &Options) -> Result<()> {
    adb.need_root()?;
    adb.refuse_in_session()?;
    if o.chunk == Some(0) {
        bail!("--chunk 0: a chunk is at least 1 MiB");
    }
    let image = format!("{STAGE}/data/data.img");
    let (size, mtime) = stat(adb, &image)?;
    let part = format!("{out}.part");
    let list = format!("{part}.progress");
    // a copy to go on from: its progress file and its .part, both of this store as it is now
    let mut earlier: Option<(Head, Vec<Chunk>)> = None;
    if o.restart {
        // both are emptied below, under the lock
        if fs::metadata(&part).is_ok() || fs::metadata(&list).is_ok() {
            eprintln!("--restart: what {part} and {list} hold is thrown away");
        }
    } else if fs::metadata(&part).is_ok() {
        match fs::read(&list) {
            Ok(bytes) => {
                earlier = parse(&String::from_utf8_lossy(&bytes)).with_context(|| format!("{list} is not a progress file this tool can go on from (--restart begins again)"))?;
                if earlier.is_none() {
                    eprintln!("{list} has no whole head: nothing was copied yet, beginning again");
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => eprintln!("{part} has no progress file ({list}): beginning again"),
            Err(e) => return Err(e).with_context(|| list.clone()),
        }
    }
    if let Some((head, _)) = &earlier {
        if head.image != image || head.size != size || head.mtime != mtime {
            bail!(
                "the phone's data.img is not the one {part} was begun from (then {} bytes, modified {}; now {size} bytes, modified {mtime}): the chunks copied are of the old one. Begin again: --restart",
                head.size,
                head.mtime
            );
        }
        if o.chunk.is_some_and(|mib| mib * MIB != head.chunk) {
            bail!("{part} was begun with --chunk {}: go on with that (leave --chunk out), or begin again with --restart", head.chunk / MIB);
        }
        let have = fs::metadata(&part).with_context(|| part.clone())?.len();
        if have != size {
            bail!("{part} has {have} bytes, not the {size} its progress file is for: begin again with --restart");
        }
    }
    let fresh = earlier.is_none();
    let (head, chunks) = match earlier {
        Some(earlier) => earlier,
        None => {
            let head = Head { image, size, mtime, chunk: o.chunk.unwrap_or(256) * MIB };
            let chunks = vec![Chunk::default(); head.chunks()];
            (head, chunks)
        }
    };
    let progress_file = File::options().read(true).append(true).create(true).open(&list).with_context(|| list.clone())?;
    lock(&progress_file, &list)?;
    let file = if fresh {
        // the length from the start: every range that is never written is a hole
        let file = File::options().read(true).write(true).create(true).truncate(true).open(&part).with_context(|| part.clone())?;
        file.set_len(size).with_context(|| part.clone())?;
        progress_file.set_len(0).with_context(|| list.clone())?;
        (&progress_file).write_all(head.text().as_bytes()).and_then(|_| progress_file.sync_data()).with_context(|| list.clone())?;
        file
    } else {
        // a line that was being written when the run before was killed goes
        let text = fs::read(&list).with_context(|| list.clone())?;
        let whole = text.iter().rposition(|b| *b == b'\n').map_or(0, |end| end + 1);
        if whole != text.len() {
            progress_file.set_len(whole as u64).with_context(|| list.clone())?;
        }
        File::options().read(true).write(true).open(&part).with_context(|| part.clone())?
    };
    let n = head.chunks();
    let mut job = Job {
        adb,
        o,
        head,
        part: part.clone(),
        file,
        progress: Progress { file: progress_file, chunks },
        start: Instant::now(),
        run: Run::default(),
        wire: Cell::new(0),
        fresh,
        tried: vec![false; n],
        map: true,
        zero_sums: HashMap::new(),
        last_line: Instant::now(),
    };
    let done_before = (0..n).filter(|i| job.progress.done(*i)).count();
    let copied_before = job.progress.chunks.iter().filter(|c| c.copied.is_some()).count();
    let when = match o.verify {
        Verify::Chunk => "asked before each chunk: a chunk of zeros is not copied",
        Verify::End => "asked after the last chunk",
    };
    eprintln!("data.img: {size} bytes on the phone; copying to {out} in {n} chunk(s) of {} MiB, each checked against the phone's sha256 ({when})", job.head.chunk / MIB);
    if !fresh {
        let next = (0..n).find(|i| !job.progress.done(*i));
        eprintln!(
            "going on with {part}: {copied_before} of {n} chunks are copied, {done_before} of them verified; {}",
            next.map_or("nothing is left to copy".to_string(), |i| format!("the first that is not done is {}", job.name(i)))
        );
    }

    match o.verify {
        Verify::Chunk => {
            for i in 0..n {
                if !job.progress.done(i) {
                    job.chunk(i, true)?;
                }
            }
        }
        Verify::End => {
            for i in 0..n {
                if job.progress.chunks[i].copied.is_none() {
                    job.chunk(i, false)?;
                }
            }
            eprintln!("every chunk is copied; asking the phone for each chunk's sha256");
            for i in 0..n {
                if !job.progress.done(i) {
                    job.chunk(i, true)?;
                }
            }
        }
    }

    // the store must be the one the copy began with
    let after = stat(adb, &job.head.image)?;
    if after != (size, mtime) {
        return Err(job.changed(after));
    }
    let total: u64 = job.progress.chunks.iter().filter(|c| c.verified).filter_map(|c| c.copied.as_ref()).map(|c| c.len).sum();
    let have = job.file.metadata().with_context(|| part.clone())?.len();
    if total != size || have != size {
        bail!("the verified chunks hold {total} bytes and {part} has {have}, the phone's data.img has {size}; {part} is what came");
    }
    job.file.sync_all().with_context(|| part.clone())?;
    fs::rename(&part, out).with_context(|| out.to_string())?;
    fs::remove_file(&list).with_context(|| list.clone())?;

    let copied: Vec<&Copied> = job.progress.chunks.iter().filter_map(|c| c.copied.as_ref()).collect();
    let (written, wire_all): (u64, u64) = (copied.iter().map(|c| c.data).sum(), copied.iter().map(|c| c.wire).sum());
    let elapsed = job.start.elapsed();
    let wire = job.wire.get();
    println!(
        "{out}: {size} bytes, {written} of them written, {wire} bytes over adb ({} s, {:.1} MB/s, {:.2} MB/s over adb); {n} chunk(s) of {} MiB: {} copied, {} of zeros not copied, {} retried, every chunk's sha256 is the phone's{}",
        elapsed.as_secs(),
        rate(job.run.covered, elapsed),
        rate(wire, elapsed),
        job.head.chunk / MIB,
        job.run.copied,
        job.run.zeros,
        job.run.retries,
        if fresh { String::new() } else { format!(" (this run; {copied_before} chunks were copied before, the whole copy moved {wire_all} bytes in the streams that ended well)") }
    );
    if o.ioslog {
        for step in ["boots", "checks"] {
            println!("--- ioslog {step} {out}");
            let status = oneios_ioslog::run(&[step.to_string(), out.to_string()]);
            if status != 0 {
                bail!("ioslog {step} ended with status {status}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn head() -> Head {
        // three chunks of 2 MiB and one of 1 MiB and 5 bytes
        Head { image: "/data/local/tmp/oneios/data/data.img".into(), size: 7 * MIB + 5, mtime: 1763000000, chunk: 2 * MIB }
    }

    #[test]
    fn chunks_cover_the_store_and_the_last_is_the_rest() {
        let h = head();
        assert_eq!(h.chunks(), 4);
        assert_eq!((h.range(0), h.range(3)), ((0, 2 * MIB), (6 * MIB, MIB + 5)));
        assert_eq!((0..h.chunks()).map(|i| h.range(i).1).sum::<u64>(), h.size);
        assert_eq!(range_reader(&h, 3), "/data/adb/ksu/bin/busybox dd if=/data/local/tmp/oneios/data/data.img bs=1M skip=6 count=2 2>/dev/null");
        assert_eq!(Head { size: 4 * MIB, ..head() }.chunks(), 2);
        assert_eq!(Head { size: 0, ..head() }.chunks(), 0);
    }

    #[test]
    fn a_progress_file_is_read_back_line_by_line() {
        let h = head();
        let mut text = h.text();
        assert_eq!(parse(&text).unwrap(), Some((h.clone(), vec![Chunk::default(); 4])));
        text += &format!("copied 0 {} {A} 4096 900\nverified 0 {A}\ncopied 3 {} {B} 0 0\n", 2 * MIB, MIB + 5);
        let (_, chunks) = parse(&text).unwrap().unwrap();
        assert_eq!(chunks[0], Chunk { copied: Some(Copied { len: 2 * MIB, sha: A.into(), data: 4096, wire: 900 }), verified: true });
        assert_eq!((chunks[1].clone(), chunks[3].verified), (Chunk::default(), false));
        // a line cut off by a kill does not count, whatever it would have said
        let cut = format!("{text}verified 3 {B}");
        assert!(!parse(&cut).unwrap().unwrap().1[3].verified);
        assert!(parse(&format!("{cut}\n")).unwrap().unwrap().1[3].verified);
        // a chunk copied again is not verified by the line of the copy before it, only by one with its own sum
        let again = format!("{text}copied 0 {} {B} 4096 900\n", 2 * MIB);
        assert!(!parse(&again).unwrap().unwrap().1[0].verified);
        assert!(!parse(&format!("{again}verified 0 {A}\n")).unwrap().unwrap().1[0].verified);
        assert!(parse(&format!("{again}verified 0 {B}\n")).unwrap().unwrap().1[0].verified);
    }

    #[test]
    fn a_head_that_is_not_whole_is_no_copy_and_a_foreign_file_is_refused() {
        let text = head().text();
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse(&text[..text.len() - 1]).unwrap(), None, "the head's last line has no end");
        assert_eq!(parse(&format!("{HEAD}\nimage /x\nsize 5\n")).unwrap(), None);
        for (bad, why) in [
            ("{\"chunks\": []}\n".to_string(), "its first line"),
            (format!("{HEAD}\nsize 5\n"), "where \"image ...\" belongs"),
            (format!("{HEAD}\nimage /x\nsize five\n"), "size is \"five\""),
            (format!("{HEAD}\nimage /x\nsize 5\nmtime 1\nchunk 1000\n"), "not whole MiB"),
            (format!("{text}copied 4 {} {A} 0 0\n", 2 * MIB), "not a chunk of this copy"),
            (format!("{text}copied 3 {} {A} 0 0\n", 2 * MIB), "not a chunk of this copy"),
            (format!("{text}copied 0 {} {} 0 0\n", 2 * MIB, &A[..63]), "not a chunk of this copy"),
            (format!("{text}verified 9 {A}\n"), "not a chunk of this copy"),
            (format!("{text}done 0\n"), "none of this tool's"),
        ] {
            let e = format!("{:#}", parse(&bad).err().unwrap_or_else(|| panic!("{bad} was taken")));
            assert!(e.contains(why), "{bad}: {e}");
        }
    }
}
