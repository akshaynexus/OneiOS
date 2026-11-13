//! `oneios-tool remotezip` (was scripts/fetch/remotezip.py): read single members of a ZIP (an IPSW) without
//! downloading the archive. `image check-ipsw` and `image fetch` use it.
//!
//!   remotezip ls  SOURCE [REGEX]                    size, packed size, method, CRC-32, name of each member
//!   remotezip cat SOURCE MEMBER                     a small member on stdout
//!   remotezip get SOURCE OUTDIR [-j N] MEMBER[=NAME]...   members into OUTDIR (NAME: file name there)
//!   remotezip head SOURCE MEMBER BYTES              the first BYTES of a stored member on stdout
//!
//! SOURCE is an https URL (HTTP range requests) or a local file. `get` fetches a member's packed bytes in
//! parallel chunks into NAME.part; finished chunks are listed in NAME.part.done, so an interrupted run
//! resumes. Deflated members are inflated locally. The CRC-32 of the ZIP directory is checked before the
//! file gets its final name. Linux and macOS.
//!
//! Connections try each address of the host for at most CONNECT_TRY seconds before the next one, and an address
//! family that failed to connect is tried last from then on. Without that, a host whose IPv6 has no route (seen on
//! a Linux PC on 2026-11-12: Apple's CDN answers AAAA first) waited the whole request timeout on each IPv6 address
//! of every range request before it reached IPv4, about 1.6 MB/s instead of the line's speed.
//!
//! Same idea as iOSPlus10's `op10tool ipsw-get` and UniXNU's scripts/unixnu/ipsw.py.
//!
//! Libraries: ureq (HTTP; its resolver hook carries the address rule above), flate2 (inflate), crc32fast, regex.
//! The ZIP directory is read here (70 lines: the end record, its ZIP64 form, the entries): the reader needs the
//! directory alone, from two range requests, and each member's own header only when that member is read.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

pub const CHUNK: u64 = 8 << 20;
const AGENT: &str = "oneios-image-build";
const CONNECT_TRY: Duration = Duration::from_secs(5);

/// The script's `Error`: one line for the caller.
pub type Result<T> = std::result::Result<T, String>;

/// The address families (true: IPv6) that failed to connect: tried last from then on.
static FAILED_FAMILIES: Mutex<Vec<bool>> = Mutex::new(Vec::new());

/// The host's addresses, with a short try of each: the first that connects is the one ureq gets.
struct Resolver;

impl ureq::Resolver for Resolver {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
        let mut addrs: Vec<SocketAddr> = netloc.to_socket_addrs()?.collect();
        let failed = |addr: &SocketAddr| FAILED_FAMILIES.lock().unwrap().contains(&addr.is_ipv6());
        addrs.sort_by_key(failed);
        let mut last = None;
        for addr in &addrs {
            match TcpStream::connect_timeout(addr, CONNECT_TRY) {
                Ok(_) => {
                    FAILED_FAMILIES.lock().unwrap().retain(|family| *family != addr.is_ipv6());
                    return Ok(vec![*addr]);
                }
                Err(e) => {
                    let mut families = FAILED_FAMILIES.lock().unwrap();
                    if !families.contains(&addr.is_ipv6()) {
                        families.push(addr.is_ipv6());
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other(format!("getaddrinfo returned nothing for {netloc}"))))
    }
}

/// Random access to a local file or an https URL.
pub struct Source {
    pub spec: String,
    pub size: u64,
    remote: bool,
    retries: u32,
    agent: ureq::Agent,
}

impl Source {
    pub fn open(spec: &str) -> Result<Source> {
        let remote = spec.starts_with("http://") || spec.starts_with("https://");
        // (no connect timeout of ureq's own: it would start before the resolver's tries, and end the request while
        // they run; the resolver hands over an address that has just answered)
        let agent = ureq::AgentBuilder::new().resolver(Resolver).user_agent(AGENT).build();
        let size = if remote {
            let answer = agent.head(spec).timeout(Duration::from_secs(60)).call().map_err(|e| format!("{spec}: {e}"))?;
            answer.header("Content-Length").and_then(|v| v.parse().ok()).ok_or_else(|| format!("{spec}: no Content-Length"))?
        } else {
            std::fs::metadata(spec).map_err(|e| format!("{spec}: {e}"))?.len()
        };
        Ok(Source { spec: spec.to_string(), size, remote, retries: 6, agent })
    }

    /// Bytes start..end inclusive.
    pub fn read(&self, start: u64, end: u64) -> Result<Vec<u8>> {
        if end < start {
            return Ok(Vec::new());
        }
        let want = (end - start + 1) as usize;
        if !self.remote {
            let mut data = vec![0u8; want];
            let file = File::open(&self.spec).map_err(|e| format!("{}: {e}", self.spec))?;
            file.read_exact_at(&mut data, start).map_err(|_| format!("{}: short read at {start}", self.spec))?;
            return Ok(data);
        }
        let mut last = String::new();
        for attempt in 0..self.retries {
            if attempt > 0 {
                std::thread::sleep(Duration::from_secs(2 * attempt as u64));
            }
            match self.range(start, end, want) {
                Ok(Ok(data)) => return Ok(data),
                Ok(Err(refused)) => return Err(refused),
                Err(e) => last = e,
            }
        }
        Err(format!("{}: range {start}-{end} failed: {last}", self.spec))
    }

    /// One range request. The outer error is tried again; the inner one (a server without ranges) is final.
    fn range(&self, start: u64, end: u64, want: usize) -> std::result::Result<Result<Vec<u8>>, String> {
        let answer = self.agent.get(&self.spec).set("Range", &format!("bytes={start}-{end}")).timeout(Duration::from_secs(120)).call().map_err(|e| e.to_string())?;
        if answer.status() != 206 {
            return Ok(Err(format!("{}: the server ignores range requests (status {})", self.spec, answer.status())));
        }
        let mut data = Vec::with_capacity(want);
        answer.into_reader().take(want as u64 + 1).read_to_end(&mut data).map_err(|e| e.to_string())?;
        if data.len() == want {
            Ok(Ok(data))
        } else {
            Err(format!("range {start}-{end}: got {} bytes", data.len()))
        }
    }
}

/// One member of the archive, as its directory has it.
#[derive(Clone, Debug)]
pub struct Info {
    pub filename: String,
    pub file_size: u64,
    pub compress_size: u64,
    pub crc: u32,
    /// 0 stored, 8 deflate.
    pub compress_type: u16,
    pub flag_bits: u16,
    pub header_offset: u64,
}

fn u16_at(data: &[u8], at: usize) -> u64 {
    u16::from_le_bytes(data[at..at + 2].try_into().unwrap()) as u64
}

fn u32_at(data: &[u8], at: usize) -> u64 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as u64
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

/// The archive's directory: where it is (the end record, or its ZIP64 form) and its entries.
fn directory(source: &Source) -> Result<Vec<Info>> {
    let not_zip = || format!("{}: not a ZIP archive", source.spec);
    // the end record is the last 22 bytes, or before a comment of up to 64 KiB; the ZIP64 locator is 20 bytes before it
    let tail_start = source.size.saturating_sub(22 + 65535 + 20);
    let tail = source.read(tail_start, source.size.saturating_sub(1))?;
    let end = (0..tail.len().saturating_sub(21)).rev().find(|&at| &tail[at..at + 4] == b"PK\x05\x06").ok_or_else(not_zip)?;
    let (mut count, mut size, mut offset) = (u16_at(&tail, end + 10), u32_at(&tail, end + 12), u32_at(&tail, end + 16));
    if end >= 20 && &tail[end - 20..end - 16] == b"PK\x06\x07" {
        let record = source.read(u64_at(&tail, end - 12), u64_at(&tail, end - 12) + 55)?;
        if &record[..4] != b"PK\x06\x06" {
            return Err(not_zip());
        }
        (count, size, offset) = (u64_at(&record, 32), u64_at(&record, 40), u64_at(&record, 48));
    }
    let data = source.read(offset, (offset + size).saturating_sub(1))?;
    let (mut infos, mut at) = (Vec::new(), 0usize);
    for _ in 0..count {
        if at + 46 > data.len() || &data[at..at + 4] != b"PK\x01\x02" {
            return Err(not_zip());
        }
        let (name_len, extra_len, comment_len) = (u16_at(&data, at + 28) as usize, u16_at(&data, at + 30) as usize, u16_at(&data, at + 32) as usize);
        let mut info = Info {
            filename: String::from_utf8_lossy(&data[at + 46..at + 46 + name_len]).into_owned(),
            file_size: u32_at(&data, at + 24),
            compress_size: u32_at(&data, at + 20),
            crc: u32_at(&data, at + 16) as u32,
            compress_type: u16_at(&data, at + 10) as u16,
            flag_bits: u16_at(&data, at + 8) as u16,
            header_offset: u32_at(&data, at + 42),
        };
        zip64(&mut info, &data[at + 46 + name_len..at + 46 + name_len + extra_len]);
        infos.push(info);
        at += 46 + name_len + extra_len + comment_len;
    }
    Ok(infos)
}

/// The ZIP64 extra field (id 1): the 64-bit value of each field that is all ones in the entry, in this order.
fn zip64(info: &mut Info, extra: &[u8]) {
    let mut at = 0;
    while at + 4 <= extra.len() {
        let (id, len) = (u16_at(extra, at), u16_at(extra, at + 2) as usize);
        if id == 1 {
            let mut field = at + 4;
            for value in [&mut info.file_size, &mut info.compress_size, &mut info.header_offset] {
                if *value == 0xffff_ffff && field + 8 <= at + 4 + len {
                    *value = u64_at(extra, field);
                    field += 8;
                }
            }
        }
        at += 4 + len;
    }
}

pub struct RemoteZip {
    pub source: Source,
    infos: Vec<Info>,
}

impl RemoteZip {
    pub fn open(spec: &str) -> Result<RemoteZip> {
        let source = Source::open(spec)?;
        let infos = directory(&source)?;
        Ok(RemoteZip { source, infos })
    }

    pub fn infos(&self) -> &[Info] {
        &self.infos
    }

    /// The entry called name, or the only one whose path ends in /name.
    pub fn find(&self, name: &str) -> Result<&Info> {
        if let Some(info) = self.infos.iter().rev().find(|i| i.filename == name) {
            return Ok(info);
        }
        let tail = format!("/{name}");
        let hits: Vec<&Info> = self.infos.iter().filter(|i| i.filename.ends_with(&tail)).collect();
        match hits[..] {
            [one] => Ok(one),
            [] => Err(format!("{name}: not in the archive")),
            _ => Err(format!("{name}: {} members match", hits.len())),
        }
    }

    /// Where a member's bytes begin: after its own header, whose name and extra field have their own lengths.
    fn data_start(&self, info: &Info) -> Result<u64> {
        let head = self.source.read(info.header_offset, info.header_offset + 29)?;
        if &head[..4] != b"PK\x03\x04" {
            return Err(format!("{}: no local header", info.filename));
        }
        Ok(info.header_offset + 30 + u16_at(&head, 26) + u16_at(&head, 28))
    }

    /// A whole (small) member.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let info = self.find(name)?;
        let start = self.data_start(info)?;
        let packed = self.source.read(start, (start + info.compress_size).wrapping_sub(1))?;
        let data = match info.compress_type {
            0 => packed,
            8 => {
                let mut data = Vec::with_capacity(info.file_size as usize);
                flate2::read::DeflateDecoder::new(&packed[..]).read_to_end(&mut data).map_err(|e| format!("{}: {e}", info.filename))?;
                data
            }
            other => return Err(format!("{}: compression method {other} is not supported", info.filename)),
        };
        if crc32fast::hash(&data) != info.crc {
            return Err(format!("{}: Bad CRC-32", info.filename));
        }
        Ok(data)
    }

    /// The first `count` bytes of a stored member.
    pub fn head(&self, name: &str, count: u64) -> Result<Vec<u8>> {
        let info = self.find(name)?;
        if info.compress_type != 0 {
            return Err(format!("{}: not stored, cannot read its first bytes alone", info.filename));
        }
        let start = self.data_start(info)?;
        self.source.read(start, (start + count.min(info.file_size)).wrapping_sub(1))
    }

    /// Fetch one member to the path out, `chunk` bytes at a time. Returns (size, crc32).
    pub fn get(&self, name: &str, out: &str, jobs: usize, chunk: u64, log: &(dyn Fn(String) + Sync)) -> Result<(u64, u32)> {
        let info = self.find(name)?;
        if info.compress_type != 0 && info.compress_type != 8 {
            return Err(format!("{}: compression method {} is not supported", info.filename, info.compress_type));
        }
        if info.flag_bits & 1 != 0 {
            return Err(format!("{}: encrypted ZIP member", info.filename));
        }
        if std::fs::metadata(out).is_ok_and(|m| m.is_file() && m.len() == info.file_size) && crc_file(out)? == info.crc {
            log(format!("{}: already complete", info.filename));
            return Ok((info.file_size, info.crc));
        }
        let stored = info.compress_type == 0;
        let part = format!("{out}{}", if stored { ".part" } else { ".deflate.part" });
        let done_path = format!("{part}.done");
        self.chunks(info, &part, &done_path, jobs, chunk, log).map_err(|e| format!("{}: download failed, run again to resume ({e})", info.filename))?;
        let (tmp, crc) = if stored {
            (part.clone(), crc_file(&part)?)
        } else {
            log(format!("{}: inflating", info.filename));
            let tmp = format!("{out}.part");
            let crc = inflate(&part, &tmp).map_err(|e| format!("{}: {e}", info.filename))?;
            (tmp, crc)
        };
        let size = std::fs::metadata(&tmp).map_err(|e| format!("{tmp}: {e}"))?.len();
        if crc != info.crc || size != info.file_size {
            let _ = std::fs::remove_file(&done_path);
            return Err(format!("{}: CRC-32 {crc:08x}, the archive says {:08x}; chunk list reset, run again", info.filename, info.crc));
        }
        std::fs::rename(&tmp, out).map_err(|e| format!("{out}: {e}"))?;
        let _ = std::fs::remove_file(&done_path);
        if !stored {
            let _ = std::fs::remove_file(&part);
        }
        log(format!("{}: {} bytes, CRC-32 ok -> {out}", info.filename, info.file_size));
        Ok((info.file_size, info.crc))
    }

    /// The member's packed bytes into `part`, in chunks, `jobs` at a time; each finished chunk is a line of
    /// `done_path` (whose first line names what the list belongs to), so a later run fetches the others only.
    fn chunks(&self, info: &Info, part: &str, done_path: &str, jobs: usize, chunk: u64, log: &(dyn Fn(String) + Sync)) -> Result<()> {
        let (size, start) = (info.compress_size, self.data_start(info)?);
        let title = format!("{:08x} {size} {chunk}", info.crc);
        let mut done: HashSet<u64> = HashSet::new();
        if std::fs::metadata(part).is_ok_and(|m| m.is_file() && m.len() == size) {
            if let Ok(list) = std::fs::read_to_string(done_path) {
                let mut lines = list.lines();
                if lines.next().map(str::trim) == Some(title.as_str()) {
                    done = lines.filter_map(|line| line.trim().parse().ok()).collect();
                }
            }
        }
        let io = |e: std::io::Error| e.to_string();
        let file = File::options().read(true).write(true).create(true).truncate(false).open(part).map_err(io)?;
        file.set_len(size).map_err(io)?;
        let todo: Vec<u64> = (0..size.div_ceil(chunk)).filter(|k| !done.contains(k)).collect();
        let marks = if done.is_empty() {
            let mut marks = File::create(done_path).map_err(io)?;
            writeln!(marks, "{title}").map_err(io)?;
            marks
        } else {
            File::options().append(true).open(done_path).map_err(io)?
        };
        // (the marks and the count of fetched bytes change together)
        let state = Mutex::new((marks, done.len() as u64 * chunk));
        let (next, failure) = (AtomicUsize::new(0), Mutex::new(None::<String>));
        std::thread::scope(|scope| {
            for _ in 0..jobs.max(1).min(todo.len().max(1)) {
                scope.spawn(|| loop {
                    let Some(&k) = todo.get(next.fetch_add(1, Ordering::SeqCst)) else { return };
                    if failure.lock().unwrap().is_some() {
                        return;
                    }
                    let (st, en) = (k * chunk, ((k + 1) * chunk).min(size) - 1);
                    let fetched = self.source.read(start + st, start + en).and_then(|data| file.write_all_at(&data, st).map_err(io));
                    if let Err(e) = fetched {
                        failure.lock().unwrap().get_or_insert(e);
                        return;
                    }
                    let mut state = state.lock().unwrap();
                    let _ = writeln!(state.0, "{k}").and_then(|()| state.0.flush());
                    state.1 = (state.1 + en - st + 1).min(size);
                    log(format!("{}: {}% ({}/{} MiB)", info.filename, state.1 * 100 / size.max(1), state.1 >> 20, size >> 20));
                });
            }
        });
        match failure.into_inner().unwrap() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Inflate a raw deflate stream from `part` into `tmp`; the CRC-32 of what came out.
fn inflate(part: &str, tmp: &str) -> std::io::Result<u32> {
    let mut source = flate2::read::DeflateDecoder::new(std::io::BufReader::with_capacity(1 << 20, File::open(part)?));
    let mut out = std::io::BufWriter::with_capacity(1 << 20, File::create(tmp)?);
    let (mut crc, mut block) = (crc32fast::Hasher::new(), vec![0u8; 1 << 20]);
    loop {
        let n = source.read(&mut block)?;
        if n == 0 {
            out.flush()?;
            return Ok(crc.finalize());
        }
        crc.update(&block[..n]);
        out.write_all(&block[..n])?;
    }
}

pub fn crc_file(path: &str) -> Result<u32> {
    let mut file = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let (mut crc, mut block) = (crc32fast::Hasher::new(), vec![0u8; 1 << 20]);
    loop {
        let n = file.read(&mut block).map_err(|e| format!("{path}: {e}"))?;
        if n == 0 {
            return Ok(crc.finalize());
        }
        crc.update(&block[..n]);
    }
}

const USAGE: &str = "usage: oneios-tool remotezip ls SOURCE [REGEX] | cat SOURCE MEMBER | get SOURCE OUTDIR [-j N] MEMBER[=NAME]... | head SOURCE MEMBER BYTES\n(SOURCE: an https URL or a local file)";

fn command(args: &[String]) -> Result<bool> {
    let (Some(cmd), Some(spec)) = (args.first(), args.get(1)) else { return Ok(false) };
    let rest = &args[2..];
    let z = RemoteZip::open(spec)?;
    let mut out = std::io::stdout().lock();
    match (cmd.as_str(), rest) {
        ("ls", _) => {
            let pattern = rest.first().map(|p| regex::Regex::new(p).map_err(|e| e.to_string())).transpose()?;
            for i in z.infos().iter().filter(|i| pattern.as_ref().is_none_or(|p| p.is_match(&i.filename))) {
                let method = match i.compress_type {
                    0 => "stored".to_string(),
                    8 => "deflate".to_string(),
                    other => other.to_string(),
                };
                let _ = writeln!(out, "{:>13} {:>13} {method:<7} {:08x} {}", i.file_size, i.compress_size, i.crc, i.filename);
            }
        }
        ("cat", [member, ..]) => out.write_all(&z.read(member)?).map_err(|e| e.to_string())?,
        ("head", [member, count, ..]) => out.write_all(&z.head(member, count.parse().map_err(|_| format!("{count} is not a number of bytes"))?)?).map_err(|e| e.to_string())?,
        ("get", [outdir, members @ ..]) => {
            drop(out);
            let (mut jobs, mut names, mut words) = (8usize, Vec::new(), members.iter());
            while let Some(word) = words.next() {
                match word.as_str() {
                    "-j" => jobs = words.next().and_then(|n| n.parse().ok()).ok_or("-j needs a number")?,
                    _ => names.push(word),
                }
            }
            std::fs::create_dir_all(outdir).map_err(|e| format!("{outdir}: {e}"))?;
            for member in names {
                let (name, as_name) = member.split_once('=').unwrap_or((member, ""));
                let file = if as_name.is_empty() { name.rsplit('/').next().unwrap_or(name) } else { as_name };
                z.get(name, &Path::new(outdir).join(file).to_string_lossy(), jobs, CHUNK, &|line| println!("{line}"))?;
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// `oneios-tool remotezip ARGS...`; the exit status.
pub fn main(args: &[String]) -> i32 {
    match command(args) {
        Ok(true) => 0,
        Ok(false) => {
            eprintln!("{USAGE}");
            1
        }
        Err(e) => {
            eprintln!("remotezip: {e}");
            1
        }
    }
}

#[cfg(test)]
#[path = "remotezip_tests.rs"]
mod tests;
