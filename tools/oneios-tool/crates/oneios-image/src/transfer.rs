//! transfer: every network request of the tool (`remotezip`, `image fetch`, and `serve`'s `download`, `fetch`,
//! `zip-get`, `zip-ls`), made for a phone's mobile network (tools/oneios-tool/README.md, "serve", "Downloads on a
//! mobile network"):
//!
//! - **a try that fails is tried again** after a sleep that doubles from 1 s to 60 s with a random part (`backoff`),
//!   until no byte arrived for the give-up time. What is tried again: a name lookup, a connect, a connection that
//!   drops or gives no byte for `STALL`, and the answers 408, 429 and 5xx. The sleep is on `job`'s bell: a cancel or
//!   a pause ends it at once;
//! - **the `net` event** says when the request waits for the network and when the network is back (`Health`);
//! - **a file is fetched in pieces** by several connections (HTTP `Range`, scoped threads), each block written in
//!   place (`write_all_at`: pwrite), and a piece goes on from the byte it reached after a drop;
//! - **the state file** beside the part (`PART.state`) lists the byte ranges that are in it and what they are of
//!   (the source, its size, its `ETag` and `Last-Modified`): a later call, in this process or another, goes on from
//!   it, and starts again when the server's file is another one;
//! - **a pause** (`job::pause`) holds every connection's thread without a connection; nothing is counted after it;
//! - a server without range requests is fetched in one stream, from its first byte every time.
//!
//! The library: ureq 2 (blocking, rustls with ring: no OpenSSL, and it was in the tool already), with scoped
//! threads of ours. The download crates that split a file into ranges and resume (`http-downloader`, `fast-down`;
//! `trauma` and `downloader` do not split) are all built on reqwest and a tokio runtime: threads that are not the
//! request's own (`serve` joins a request's threads before its last event and reports the ones left), a second
//! HTTP and TLS stack in the binary, and no place for the address rule below. What is left to write here is the
//! policy, which is the point of this file: when to try again, what to say, what a part is of.
//!
//! Two things ureq 2 decides:
//! - **every request is a new connection** (`max_idle_connections(0)`). A connection that ureq takes back from its
//!   pool has no read timeout (`Stream::reset` clears it, and only a request with a whole-request deadline sets one
//!   again), so a reused connection that goes silent, which is what a phone's network does, would block its thread
//!   for good and a cancel would not be seen. A new connection has the agent's read timeout on its socket. The
//!   pieces are large so that this costs little (`piece_size`: a 32nd of the file per connection of eight, 8 to
//!   256 MiB): a drop loses nothing, whatever the piece's size, because a piece goes on from its byte;
//! - a read that is blocked cannot be ended from another thread (ureq does not hand out the socket): a pause is
//!   said at once and the thread ends its read within `STALL`; a cancel waits that long for it.
//!
//! Connections try each address of the host for at most CONNECT_TRY seconds before the next one, and an address
//! family that failed to connect is tried last from then on. Without that, a host whose IPv6 has no route (seen on
//! a Linux PC on 2026-11-12: Apple's CDN answers AAAA first) waited the whole request timeout on each IPv6 address
//! of every range request before it reached IPv4, about 1.6 MB/s instead of the line's speed.

use oneios_common::job;
use serde_json::{json, Value};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The pieces' size is chosen from the length (`piece_size`).
pub const CHUNK: u64 = 0;
/// The most connections of one transfer.
pub const CONNECTIONS_MAX: usize = 8;
/// A request gives up when no byte arrived for this long, unless it names another time.
pub const GIVE_UP_DEFAULT: u64 = 1800;
const AGENT: &str = "oneios-image-build";
const CONNECT_TRY: Duration = Duration::from_secs(5);
/// How long a connection of a `serve` request may give no byte, and of a command that runs alone.
const STALL: Duration = Duration::from_secs(5);
const STALL_ALONE: Duration = Duration::from_secs(30);
/// The most bytes of one read, and how many are gathered before they are written and counted.
const BLOCK: usize = 256 << 10;
const GATHER: usize = 64 << 10;
/// The state file is written at most this often while bytes arrive.
const SAVE_EVERY: Duration = Duration::from_secs(3);
/// The first wait before a new try, and the longest.
const WAIT_FIRST: f64 = 1.0;
const WAIT_MOST: f64 = 60.0;
/// The wait before a connection is opened again after one that brought at least `QUICK_BYTES` and then broke.
const WAIT_QUICK: f64 = 0.2;
const QUICK_BYTES: u64 = 1 << 20;

/// One line for the caller.
pub type Result<T> = std::result::Result<T, String>;

fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

// ---------------------------------------------------------------- the running request's options and its failure

// One request runs at a time in `serve`, so these are the process's, like `job`'s state.
static GIVE_UP: AtomicU64 = AtomicU64::new(GIVE_UP_DEFAULT);
static FAILURE: Mutex<Option<&'static str>> = Mutex::new(None);

/// Seconds without a byte after which the requests that begin now give up; also forgets an earlier failure.
pub fn begin_request(give_up: u64) {
    GIVE_UP.store(give_up.max(1), Ordering::SeqCst);
    *FAILURE.lock().unwrap() = None;
}

/// Why the last transfer failed, when it was the network's doing: `network` (it gave up) or `changed` (the
/// server's file is another one than the request began with). `serve` makes it the `failed` event's code.
pub fn take_failure() -> Option<&'static str> {
    FAILURE.lock().unwrap().take()
}

// ---------------------------------------------------------------- the addresses

/// The address families (true: IPv6) that failed to connect: tried last from then on.
static FAILED_FAMILIES: Mutex<Vec<bool>> = Mutex::new(Vec::new());
/// The address of a host that connected last, and when. Every request is a new connection (the module comment),
/// and a try is itself a connection that is opened and closed: while this is fresh the address is handed to ureq
/// without a lookup and without a try, which is one connection per request instead of two. A request that fails
/// forgets it (`forget_address`), so the next one looks the host up and tries its addresses again.
static GOOD_ADDRESS: Mutex<Option<(String, SocketAddr, Instant)>> = Mutex::new(None);
const GOOD_FOR: Duration = Duration::from_secs(30);

fn forget_address() {
    *GOOD_ADDRESS.lock().unwrap() = None;
}

/// The host's addresses, with a short try of each: the first that connects is the one ureq gets.
struct Resolver;

impl ureq::Resolver for Resolver {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
        if let Some((_, addr, _)) = GOOD_ADDRESS.lock().unwrap().as_ref().filter(|(host, _, at)| host == netloc && at.elapsed() < GOOD_FOR) {
            return Ok(vec![*addr]);
        }
        let mut addrs: Vec<SocketAddr> = netloc.to_socket_addrs()?.collect();
        let failed = |addr: &SocketAddr| FAILED_FAMILIES.lock().unwrap().contains(&addr.is_ipv6());
        addrs.sort_by_key(failed);
        let mut last = None;
        for addr in &addrs {
            match TcpStream::connect_timeout(addr, CONNECT_TRY) {
                Ok(_) => {
                    FAILED_FAMILIES.lock().unwrap().retain(|family| *family != addr.is_ipv6());
                    *GOOD_ADDRESS.lock().unwrap() = Some((netloc.to_string(), *addr, Instant::now()));
                    return Ok(vec![*addr]);
                }
                Err(e) => {
                    let mut families = FAILED_FAMILIES.lock().unwrap();
                    if !families.contains(&addr.is_ipv6()) {
                        families.push(addr.is_ipv6());
                    }
                    // (ureq reports every error of this hook as a name lookup's: the text tells the two apart)
                    last = Some(std::io::Error::new(e.kind(), format!("{CONNECT_FAILED} {addr}: {e}")));
                }
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other(format!("getaddrinfo returned nothing for {netloc}"))))
    }
}

const CONNECT_FAILED: &str = "connect to";

// ---------------------------------------------------------------- a try that failed

/// What went wrong in one try.
#[derive(Debug)]
enum Trouble {
    /// Try again after a wait. `why`: the `net` event's word; `after`: the server's `Retry-After`.
    Again { why: &'static str, status: Option<u16>, text: String, after: Option<Duration> },
    /// The request ends: another try would give the same.
    Final(String),
    /// The server's file is not the one the request began with.
    Changed(String),
    /// A range request was answered with the whole file (the status).
    NoRanges(u16),
    /// The request is paused or stopped, or was held by the heat limit: the connection is given up and the block
    /// that was read is not kept. Not a failure.
    Held,
}

/// A wait of 1 s after the first failure, doubling to 60 s, shortened by a random part of up to half: connections
/// that failed together do not come back together.
fn backoff(tries: u32) -> Duration {
    let whole = (WAIT_FIRST * f64::from(1u32 << tries.saturating_sub(1).min(16))).min(WAIT_MOST);
    Duration::from_secs_f64(whole * (0.5 + 0.5 * random()))
}

/// 0 to 1. std's hasher keys are random per `RandomState` (no crate for one number per failed try).
fn random() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let bits = std::collections::hash_map::RandomState::new().build_hasher().finish();
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

fn io_trouble(e: &std::io::Error) -> Trouble {
    let timeout = matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock);
    Trouble::Again { why: if timeout { "timeout" } else { "dropped" }, status: None, text: e.to_string(), after: None }
}

fn transport_trouble(t: &ureq::Transport) -> Trouble {
    use ureq::ErrorKind as Kind;
    let source = std::error::Error::source(t);
    let mut text = t.kind().to_string();
    for more in [t.message().map(str::to_string), source.map(|s| s.to_string())].into_iter().flatten() {
        text.push_str(": ");
        text.push_str(&more);
    }
    let io = source.and_then(|s| s.downcast_ref::<std::io::Error>());
    let timeout = text.contains("timed out") || io.is_some_and(|e| matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock));
    let why = match t.kind() {
        Kind::InvalidUrl | Kind::UnknownScheme | Kind::InsecureRequestHttpsOnly | Kind::InvalidProxyUrl | Kind::ProxyUnauthorized | Kind::TooManyRedirects => return Trouble::Final(text),
        Kind::Dns if text.contains(CONNECT_FAILED) => "connect",
        Kind::Dns => "dns",
        Kind::ConnectionFailed => "connect",
        _ if timeout => "timeout",
        _ => "dropped",
    };
    Trouble::Again { why, status: None, text, after: None }
}

// ---------------------------------------------------------------- the network's state, for the `net` event

#[derive(Default)]
struct Said {
    /// The count of the last `waiting` that was said (0: none since the last byte).
    count: u32,
    /// When the last byte had come, at the first `waiting`.
    since_ms: u64,
    /// `job::resumes()` at the last reset: each resume starts the counts anew once, not once per connection.
    epoch: u64,
}

/// What the connections of one source know together.
struct Health {
    /// When the last byte came (or the source was opened, or the request was resumed): the give-up clock.
    last_byte_ms: AtomicU64,
    /// `waiting` was said and no byte came since.
    waiting: AtomicBool,
    /// Connections that have a good answer and read it.
    live: AtomicUsize,
    /// Tries that failed and were repeated.
    retries: AtomicU64,
    said: Mutex<Said>,
}

/// A connection that reads a good answer, until this is dropped.
struct Live<'a>(&'a Health);

impl Drop for Live<'_> {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

fn announce(fields: &[(&str, Value)], text: &str) {
    // (a command that runs alone has no listener: its standard output is the command's own, so the line goes to
    // standard error)
    if !job::event("net", fields) {
        eprintln!("{text}");
    }
}

impl Health {
    fn new() -> Health {
        Health { last_byte_ms: AtomicU64::new(now_ms()), waiting: AtomicBool::new(false), live: AtomicUsize::new(0), retries: AtomicU64::new(0), said: Mutex::new(Said { epoch: job::resumes(), ..Said::default() }) }
    }

    fn live(&self) -> Live<'_> {
        self.live.fetch_add(1, Ordering::SeqCst);
        Live(self)
    }

    /// Bytes came that the transfer did not have: the give-up clock starts again. (Not an answer's headers, and
    /// not the bytes a stream without ranges brings a second time: a server that answers and then says nothing,
    /// or breaks one stream at the same byte every time, must run into the give-up time.)
    fn advance(&self) {
        self.last_byte_ms.store(now_ms(), Ordering::Relaxed);
        self.alive();
    }

    /// Bytes came: the network is back, when it was waited for.
    fn alive(&self) {
        if self.waiting.load(Ordering::Relaxed) {
            let now = now_ms();
            let mut said = self.said.lock().unwrap();
            if self.waiting.swap(false, Ordering::SeqCst) {
                let (tries, waited) = (said.count, now.saturating_sub(said.since_ms) as f64 / 1000.0);
                said.count = 0;
                let text = format!("the network is back after {tries} tr{} and {:.0} s", if tries == 1 { "y" } else { "ies" }, waited);
                announce(&[("state", json!("resumed")), ("tries", json!(tries)), ("waited", json!((waited * 10.0).round() / 10.0)), ("text", json!(text))], &text);
            }
        }
    }

    /// The request was resumed after a pause: the give-up clock and the count of tries start anew.
    fn resumed(&self) {
        let mut said = self.said.lock().unwrap();
        if said.epoch != job::resumes() {
            said.epoch = job::resumes();
            said.count = 0;
            said.since_ms = now_ms();
            self.last_byte_ms.store(now_ms(), Ordering::Relaxed);
        }
    }

    /// A try failed for the `tries`th time in a row and the next one comes after `wait`: said once per count, and
    /// only when no connection receives.
    fn waiting(&self, tries: u32, wait: Duration, left_ms: u64, why: &str, status: Option<u16>, error: &str) {
        if self.live.load(Ordering::SeqCst) > 0 {
            return;
        }
        let mut said = self.said.lock().unwrap();
        if tries <= said.count {
            return;
        }
        if !self.waiting.swap(true, Ordering::SeqCst) {
            said.since_ms = self.last_byte_ms.load(Ordering::Relaxed);
        }
        said.count = tries;
        let next = (wait.as_secs_f64() * 10.0).round() / 10.0;
        let text = format!("waiting for the network (try {tries}, next in {} s): {error}", if next < 1.0 { format!("{next:.1}") } else { format!("{next:.0}") });
        let mut fields = vec![("state", json!("waiting")), ("try", json!(tries)), ("next", json!(next)), ("left", json!(left_ms / 1000)), ("why", json!(why))];
        fields.extend(status.map(|status| ("status", json!(status))));
        fields.extend([("error", json!(error)), ("text", json!(text))]);
        announce(&fields, &text);
    }
}

// ---------------------------------------------------------------- the byte ranges of a part

/// Byte ranges `[first, end)`, in order and merged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ranges(Vec<(u64, u64)>);

impl Ranges {
    pub fn add(&mut self, first: u64, end: u64) {
        if first >= end {
            return;
        }
        // the ranges that touch the new one become one with it
        let from = self.0.partition_point(|range| range.1 < first);
        let (mut first, mut end, mut to) = (first, end, from);
        while to < self.0.len() && self.0[to].0 <= end {
            first = first.min(self.0[to].0);
            end = end.max(self.0[to].1);
            to += 1;
        }
        self.0.splice(from..to, [(first, end)]);
    }

    pub fn bytes(&self) -> u64 {
        self.0.iter().map(|(first, end)| end - first).sum()
    }

    /// How far the part is whole from its first byte.
    pub fn prefix(&self) -> u64 {
        match self.0.first() {
            Some(&(0, end)) => end,
            _ => 0,
        }
    }

    /// What is missing of `length` bytes.
    fn gaps(&self, length: u64) -> Vec<(u64, u64)> {
        let (mut gaps, mut at) = (Vec::new(), 0);
        for &(first, end) in &self.0 {
            if first > at {
                gaps.push((at, first));
            }
            at = end;
        }
        if at < length {
            gaps.push((at, length));
        }
        gaps
    }

    /// A state file's list, when it is one: pairs in order, apart, within `length`.
    fn of_list(list: &Value, length: u64) -> Option<Ranges> {
        let mut ranges = Vec::new();
        for pair in list.as_array()? {
            let (first, end) = (pair.get(0)?.as_u64()?, pair.get(1)?.as_u64()?);
            if pair.as_array()?.len() != 2 || first >= end || end > length || ranges.last().is_some_and(|last: &(u64, u64)| last.1 >= first) {
                return None;
            }
            ranges.push((first, end));
        }
        Some(Ranges(ranges))
    }
}

/// The size of a transfer's pieces: a 4th of each connection's share, 8 to 256 MiB in whole MiB. Each piece is one
/// request on a new connection (the module comment), so the pieces are few; four per connection so that a slow
/// connection holds up only its own piece.
fn piece_size(length: u64, connections: usize) -> u64 {
    (length / (4 * connections.max(1) as u64)).clamp(8 << 20, 256 << 20).next_multiple_of(1 << 20)
}

// ---------------------------------------------------------------- the state file

/// What a part is of. A state file with another one of these is not this transfer's.
struct Ident<'a> {
    source: &'a str,
    what: &'a str,
    size: u64,
    etag: Option<&'a str>,
    last_modified: Option<&'a str>,
    offset: u64,
    length: u64,
    ranges: bool,
}

impl Ident<'_> {
    /// The state file's one line, its keys in the README's order.
    fn line(&self, done: &Ranges) -> String {
        let text = |value: &str| Value::from(value).to_string();
        let opt = |value: Option<&str>| value.map_or("null".to_string(), text);
        let list: Vec<String> = done.0.iter().map(|(first, end)| format!("[{first},{end}]")).collect();
        format!(
            "{{\"v\":1,\"source\":{},\"what\":{},\"size\":{},\"etag\":{},\"last_modified\":{},\"offset\":{},\"length\":{},\"ranges\":{},\"done\":[{}]}}\n",
            text(self.source),
            text(self.what),
            self.size,
            opt(self.etag),
            opt(self.last_modified),
            self.offset,
            self.length,
            self.ranges,
            list.join(",")
        )
    }

    /// The ranges an earlier transfer left in `part`, when its state file is of this very transfer. `Err`: there is
    /// something, and it cannot be gone on from (why, for a line); `Ok` of nothing: there is nothing.
    fn earlier(&self, state_path: &str, part: &str) -> Result<Ranges> {
        let part_len = std::fs::metadata(part).ok().map(|m| m.len());
        let text = match std::fs::read_to_string(state_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return if part_len.is_some_and(|len| len > 0) { Err("its part has no state file".to_string()) } else { Ok(Ranges::default()) };
            }
            Err(e) => return Err(format!("the state file cannot be read ({e})")),
        };
        let state: Value = serde_json::from_str(&text).map_err(|e| format!("the state file cannot be read ({e})"))?;
        if state["v"] != 1 {
            return Err("the state file is of another version of this tool".to_string());
        }
        let then = |key: &str| state[key].as_str().map_or("none".to_string(), str::to_string);
        if state["source"] != self.source || state["what"] != self.what || state["offset"] != self.offset || state["length"] != self.length {
            return Err(format!("its part is of another download ({} of {})", then("what"), then("source")));
        }
        if state["size"] != self.size {
            return Err(format!("the server's file changed ({} bytes then, {} now)", state["size"], self.size));
        }
        for (key, name, now) in [("etag", "ETag", self.etag), ("last_modified", "Last-Modified", self.last_modified)] {
            if state[key].as_str() != now {
                return Err(format!("the server's file changed ({name} {} then, {} now)", then(key), now.unwrap_or("none")));
            }
        }
        if part_len != Some(self.length) {
            return Err(format!("its part has {} bytes, not {}", part_len.unwrap_or(0), self.length));
        }
        Ranges::of_list(&state["done"], self.length).ok_or_else(|| "the state file's list of ranges is not one".to_string())
    }

    /// The part's data to the disk, then the state file under another name, synced, and renamed: the state never
    /// lists bytes that a power loss could take from the part, and it is whole or the one before.
    fn save(&self, part: &File, state_path: &str, done: &Ranges) -> std::io::Result<()> {
        part.sync_data()?;
        let tmp = format!("{state_path}.tmp");
        let mut file = File::create(&tmp)?;
        file.write_all(self.line(done).as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, state_path)
    }
}

// ---------------------------------------------------------------- the part's unbroken beginning, for a hash

/// How far a part is whole from its first byte, for a thread that hashes it while the transfer runs (`serve`'s
/// `download`: SHA-256 has one order and the pieces arrive in any).
#[derive(Default)]
pub struct Frontier {
    /// (the length of the unbroken beginning, the transfer is over)
    reach: Mutex<(u64, bool)>,
    bell: Condvar,
}

impl Frontier {
    pub fn new() -> Frontier {
        Frontier::default()
    }

    fn set(&self, reach: u64) {
        let mut at = self.reach.lock().unwrap();
        if reach > at.0 {
            at.0 = reach;
            self.bell.notify_all();
        }
    }

    /// The transfer is over (whole, failed or stopped): nothing more comes.
    pub fn close(&self) {
        self.reach.lock().unwrap().1 = true;
        self.bell.notify_all();
    }

    /// Wait until the beginning is longer than `have` bytes: its length. `None`: the transfer is over and it is not.
    pub fn wait(&self, have: u64) -> Option<u64> {
        let mut at = self.reach.lock().unwrap();
        loop {
            if at.0 > have {
                return Some(at.0);
            }
            if at.1 {
                return None;
            }
            at = self.bell.wait(at).unwrap();
        }
    }
}

// ---------------------------------------------------------------- the source

/// Random access to a local file or an https URL.
pub struct Source {
    pub spec: String,
    pub size: u64,
    /// The server's validators as it sent them (a file: its modification time as `last_modified`).
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    remote: bool,
    agent: ureq::Agent,
    health: Health,
    /// Whether the server answers range requests (asked once, when a whole file is fetched).
    ranges: OnceLock<bool>,
}

/// What to fetch into which file.
pub struct Part<'a> {
    /// For the lines.
    pub name: &'a str,
    /// What the bytes are (`file`, `member CRC32 NAME`): the state file's `what`.
    pub what: String,
    /// Which bytes of the source.
    pub offset: u64,
    pub length: u64,
    /// The part file; its state file is this and `.state`.
    pub path: &'a str,
    pub connections: usize,
    /// The pieces' size (`CHUNK`: chosen from the length).
    pub piece: u64,
    /// The source's whole content: a server without range requests is then fetched in one stream.
    pub whole: bool,
    pub frontier: Option<&'a Frontier>,
}

/// How a transfer went.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fetched {
    /// Bytes that were in the part already.
    pub resumed_from: u64,
    /// False: one stream, the server has no range requests.
    pub ranges: bool,
    /// Tries that failed and were repeated (of the source, so far).
    pub retries: u64,
    pub connections: usize,
}

/// What the threads of one transfer share.
struct Shared {
    done: Mutex<Ranges>,
    /// The ranges changed since the state file was written.
    dirty: AtomicBool,
    failure: Mutex<Option<String>>,
    /// The next piece to take.
    next: AtomicUsize,
    /// Threads that still run.
    active: AtomicUsize,
}

impl Shared {
    fn fail(&self, text: String) {
        self.failure.lock().unwrap().get_or_insert(text);
    }

    fn failed(&self) -> bool {
        self.failure.lock().unwrap().is_some()
    }
}

impl Source {
    pub fn open(spec: &str) -> Result<Source> {
        let remote = spec.starts_with("http://") || spec.starts_with("https://");
        // ureq's connect timeout starts before the resolver's tries and must not end the request while they run:
        // it is three tries long (an address family that failed is tried last from then on, so the tries before
        // the good address are few). The resolver hands over an address that has just answered, or one that
        // answered in the last 30 s: that one may be dead by now (the phone changed its network), and this is how
        // long its connect may hang.
        // A connection that gives no byte for the stall time is given up and tried again from the byte it reached.
        // For a request of `serve` (someone listens) that is 5 s, so that a stop of the request is seen within it.
        // No connection is kept for a second request: the module comment says why.
        let stall = if job::listening() { STALL } else { STALL_ALONE };
        let agent = ureq::AgentBuilder::new().resolver(Resolver).user_agent(AGENT).timeout_connect(3 * CONNECT_TRY).timeout_read(stall).timeout_write(stall).max_idle_connections(0).build();
        let mut source = Source { spec: spec.to_string(), size: 0, etag: None, last_modified: None, remote, agent, health: Health::new(), ranges: OnceLock::new() };
        if remote {
            let answer = source.retried(|| source.call("HEAD", None)).map_err(|e| format!("{spec}: {e}"))?;
            let header = |name: &str| answer.header(name).map(str::to_string);
            let size = header("Content-Length").and_then(|v| v.parse().ok()).ok_or_else(|| format!("{spec}: no Content-Length"))?;
            (source.size, source.etag, source.last_modified) = (size, header("ETag"), header("Last-Modified"));
        } else {
            let meta = std::fs::metadata(spec).map_err(|e| format!("{spec}: {e}"))?;
            source.size = meta.len();
            source.last_modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|t| t.as_nanos().to_string());
        }
        Ok(source)
    }

    /// Tries that failed and were repeated, so far.
    pub fn retries(&self) -> u64 {
        self.health.retries.load(Ordering::Relaxed)
    }

    /// The state file's line for `length` bytes at `offset` of this source, of which `done` are there (the tests
    /// make the state an interrupted run leaves).
    #[cfg(test)]
    pub(crate) fn state_line(&self, what: &str, offset: u64, length: u64, done: &[(u64, u64)]) -> String {
        Ident { source: &self.spec, what, size: self.size, etag: self.etag.as_deref(), last_modified: self.last_modified.as_deref(), offset, length, ranges: true }.line(&Ranges(done.to_vec()))
    }

    /// One request; an answer that is no success is the try's trouble.
    fn call(&self, method: &str, range: Option<(u64, u64)>) -> std::result::Result<ureq::Response, Trouble> {
        // (the bytes as they are stored: a range counts the file's bytes, not a compressed answer's)
        let mut request = self.agent.request(method, &self.spec).set("Accept-Encoding", "identity");
        if let Some((first, last)) = range {
            request = request.set("Range", &format!("bytes={first}-{last}"));
        }
        match request.call() {
            Ok(answer) => Ok(answer),
            Err(ureq::Error::Status(status @ (408 | 429 | 500..=599), answer)) => {
                let after = answer.header("Retry-After").and_then(|v| v.trim().parse().ok()).map(Duration::from_secs);
                Err(Trouble::Again { why: "server", status: Some(status), text: format!("the server answered {status} {}", answer.status_text()), after })
            }
            Err(ureq::Error::Status(status, answer)) => Err(Trouble::Final(format!("status {status} {}", answer.status_text()))),
            Err(ureq::Error::Transport(transport)) => Err(transport_trouble(&transport)),
        }
    }

    /// Before a try: a pause is waited out here (the caller has no connection), and a stop ends the work.
    fn gate(&self, tries: &mut u32) -> Result<()> {
        if job::hold().map_err(|stop| stop.to_string())? {
            *tries = 0;
            self.health.resumed();
        }
        // The give-up clock counts the time the network gave nothing, not the time nothing was asked of it (a
        // member's check and inflating between two requests, a pause by the heat limit): a try that follows a
        // success, or a hold, starts it again. Tries that follow a failure do not.
        if *tries == 0 {
            self.health.last_byte_ms.fetch_max(now_ms(), Ordering::Relaxed);
        }
        Ok(())
    }

    /// A try failed. `Ok`: try again now (the wait is over, or it was no failure); `Err`: the request ends.
    fn failed(&self, tries: &mut u32, trouble: Trouble, brought: u64) -> Result<()> {
        let (why, status, text, after) = match trouble {
            Trouble::Held => return Ok(()),
            Trouble::Final(text) => return Err(text),
            Trouble::Changed(text) => {
                *FAILURE.lock().unwrap() = Some("changed");
                return Err(text);
            }
            Trouble::NoRanges(status) => return Err(format!("the server ignores range requests (status {status})")),
            Trouble::Again { why, status, text, after } => (why, status, text, after),
        };
        // (the next try looks the host up and tries its addresses: the network may be another one now)
        forget_address();
        // (a connection that was given up because of a pause or a stop did not fail)
        if job::paused() || job::cancelled() {
            return Ok(());
        }
        *tries += 1;
        self.health.retries.fetch_add(1, Ordering::Relaxed);
        let (idle, give_up) = (now_ms().saturating_sub(self.health.last_byte_ms.load(Ordering::Relaxed)), GIVE_UP.load(Ordering::SeqCst) * 1000);
        // `>`: both times are whole milliseconds rounded down, so `idle` can be up to 1 ms more than the real time
        // (with `>=` the test of a 2 s limit saw it give up after 1.99999 s, 2026-11-25)
        if idle > give_up {
            *FAILURE.lock().unwrap() = Some("network");
            return Err(format!("no byte arrived for {} s: gave up ({text})", idle / 1000));
        }
        // A connection that worked (it brought a mebibyte or more) and then broke is opened again after a short
        // sleep (0.1 to 0.2 s), and nothing is said: the network is there. Measured on 2026-11-15, on 127.0.0.1:
        // 1 GiB from a server that breaks every body after about 4 MB took 204 s by one connection when each of
        // the 269 drops waited the first time of the row below (0.5 to 1 s), and 41 s with this one. Every other
        // failure waits the row's time.
        let quick = *tries == 1 && brought >= QUICK_BYTES;
        let wait = if quick { Duration::from_secs_f64(WAIT_QUICK * (0.5 + 0.5 * random())) } else { backoff(*tries) };
        let wait = wait.max(after.unwrap_or_default().min(Duration::from_secs_f64(WAIT_MOST))).min(Duration::from_millis(give_up - idle));
        if !quick {
            self.health.waiting(*tries, wait, give_up - idle, why, status, &text);
        }
        // (on the bell: a cancel ends it at once, a pause too and the gate then holds)
        job::nap(wait).map_err(|stop| stop.to_string())
    }

    /// `one` until it succeeds, with the waits between.
    fn retried<T>(&self, mut one: impl FnMut() -> std::result::Result<T, Trouble>) -> Result<T> {
        let mut tries = 0;
        loop {
            self.gate(&mut tries)?;
            match one() {
                Ok(value) => {
                    self.health.advance();
                    return Ok(value);
                }
                Err(trouble) => self.failed(&mut tries, trouble, 0)?,
            }
        }
    }

    /// Bytes start..end inclusive (small: the archive's directory, a member's header, a small member).
    pub fn read(&self, start: u64, end: u64) -> Result<Vec<u8>> {
        if end < start {
            return Ok(Vec::new());
        }
        let mut data = Vec::with_capacity((end - start + 1) as usize);
        self.retried(|| {
            data.clear();
            self.stream(start, end + 1, false, &mut |block| {
                data.extend_from_slice(block);
                Ok(())
            })
        })
        .map_err(|e| format!("{}: range {start}-{end} failed: {e}", self.spec))?;
        Ok(data)
    }

    /// The answer is of the file the source was opened as.
    fn same_file(&self, answer: &ureq::Response) -> std::result::Result<(), Trouble> {
        for (name, then) in [("ETag", &self.etag), ("Last-Modified", &self.last_modified)] {
            if let (Some(then), Some(now)) = (then, answer.header(name)) {
                if then != now {
                    return Err(Trouble::Changed(format!("the server's file changed during the download ({name} {then} then, {now} now)")));
                }
            }
        }
        Ok(())
    }

    /// Bytes `first..end` of the source to `sink`, block by block as they arrive (`whole`: the file from its first
    /// byte without a range request). What arrived before a failure was given to `sink`.
    fn stream(&self, first: u64, end: u64, whole: bool, sink: &mut dyn FnMut(&[u8]) -> std::result::Result<(), Trouble>) -> std::result::Result<(), Trouble> {
        let mut block = vec![0u8; BLOCK.min((end - first) as usize)];
        if !self.remote {
            let file = File::open(&self.spec).map_err(|e| Trouble::Final(format!("{}: {e}", self.spec)))?;
            let mut at = first;
            while at < end {
                let n = block.len().min((end - at) as usize);
                file.read_exact_at(&mut block[..n], at).map_err(|_| Trouble::Final(format!("{}: short read at {at}", self.spec)))?;
                sink(&block[..n])?;
                at += n as u64;
            }
            return Ok(());
        }
        let answer = self.call("GET", (!whole).then_some((first, end - 1)))?;
        if whole {
            if answer.status() != 200 {
                return Err(Trouble::Final(format!("status {} to a request of the whole file", answer.status())));
            }
            if let Some(length) = answer.header("Content-Length").and_then(|v| v.trim().parse::<u64>().ok()).filter(|length| *length != self.size) {
                return Err(Trouble::Changed(format!("the server's file changed during the download ({} bytes then, {length} now)", self.size)));
            }
        } else if answer.status() != 206 {
            return Err(Trouble::NoRanges(answer.status()));
        } else if let Some(range) = answer.header("Content-Range") {
            // "bytes FIRST-LAST/TOTAL"
            let (span, total) = range.trim_start_matches("bytes").trim().split_once('/').unwrap_or(("", ""));
            if span.split('-').next().and_then(|v| v.trim().parse::<u64>().ok()) != Some(first) {
                return Err(Trouble::Final(format!("the server answered another range than {first}-{} ({range})", end - 1)));
            }
            if total.trim().parse::<u64>().is_ok_and(|total| total != self.size) {
                return Err(Trouble::Changed(format!("the server's file changed during the download ({} bytes then, {total} now)", self.size)));
            }
        }
        self.same_file(&answer)?;
        let _live = self.health.live();
        let mut reader = answer.into_reader();
        let mut left = end - first;
        while left > 0 {
            let want = block.len().min(left.min(usize::MAX as u64) as usize);
            let (mut got, mut trouble) = (0, None);
            while got < want.min(GATHER) {
                match reader.read(&mut block[got..want]) {
                    Ok(0) => trouble = Some(Trouble::Again { why: "dropped", status: None, text: format!("the connection ended {} bytes early", left - got as u64), after: None }),
                    Ok(n) => {
                        got += n;
                        self.health.alive();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => trouble = Some(io_trouble(&e)),
                }
                if trouble.is_some() {
                    break;
                }
            }
            if got > 0 {
                sink(&block[..got])?;
                left -= got as u64;
            }
            if let Some(trouble) = trouble {
                return Err(trouble);
            }
        }
        Ok(())
    }

    /// Whether the server answers range requests: one request for the first byte. 206: yes; 200: no (the answer is
    /// given up without its body).
    fn has_ranges(&self) -> Result<bool> {
        if !self.remote || self.size == 0 {
            return Ok(true);
        }
        if let Some(known) = self.ranges.get() {
            return Ok(*known);
        }
        let known = self.retried(|| match self.stream(0, 1, false, &mut |_| Ok(())) {
            Err(Trouble::NoRanges(200)) => Ok(false),
            other => other.map(|()| true),
        })?;
        Ok(*self.ranges.get_or_init(|| known))
    }

    /// The bytes of `p` into its part file, by its connections, and the part's state file beside it. An earlier
    /// transfer's part is gone on from when its state file says it is of this very file; otherwise a line says why
    /// not. `Ok`: every byte is in the part and the state file lists it whole (the caller checks the part, gives it
    /// its name and removes the state file).
    pub fn fetch(&self, p: &Part, log: &(dyn Fn(String) + Sync)) -> Result<Fetched> {
        let stop = |stop: job::Stop| stop.to_string();
        job::hold().map_err(stop)?;
        let ranges = !p.whole || self.has_ranges()?;
        let io = |e: std::io::Error| format!("{}: {e}", p.path);
        let state_path = format!("{}.state", p.path);
        // (the list of finished chunks that this tool wrote before it had the state file)
        let _ = std::fs::remove_file(format!("{}.done", p.path));
        let ident = Ident { source: &self.spec, what: &p.what, size: self.size, etag: self.etag.as_deref(), last_modified: self.last_modified.as_deref(), offset: p.offset, length: p.length, ranges };
        let again = |why: &str| log(format!("{}: {why}: starting again from the first byte", p.name));
        let mut done = ident.earlier(&state_path, p.path).unwrap_or_else(|why| {
            again(&why);
            Ranges::default()
        });
        if !ranges {
            log(format!("{}: the server has no range requests: one stream, which can only begin at the first byte", p.name));
            if done.bytes() > 0 {
                again("the server has no range requests");
                done = Ranges::default();
            }
        }
        let resumed_from = done.bytes();
        let file = File::options().read(true).write(true).create(true).truncate(false).open(p.path).map_err(io)?;
        if resumed_from == 0 {
            file.set_len(0).map_err(io)?;
        } else {
            log(format!("{}: going on from {resumed_from} of {} bytes", p.name, p.length));
            if self.remote && self.etag.is_none() && self.last_modified.is_none() {
                log(format!("{}: the server names no ETag and no Last-Modified: only the size says that the file is the same", p.name));
            }
        }
        file.set_len(p.length).map_err(io)?;
        // what an earlier run fetched counts as done in the running request's progress (`serve`)
        job::count(resumed_from);
        if let Some(frontier) = p.frontier {
            frontier.set(done.prefix());
        }
        let connections = p.connections.clamp(1, CONNECTIONS_MAX);
        let piece = if p.piece == CHUNK { piece_size(p.length, connections) } else { p.piece };
        // the missing bytes, cut where a piece ends (the same places in every run), in rising order
        let pieces: Vec<(u64, u64)> = match ranges {
            true => done.gaps(p.length).into_iter().flat_map(|(first, end)| std::iter::successors(Some((first, ((first / piece + 1) * piece).min(end))), move |&(_, at)| (at < end).then(|| (at, (at + piece).min(end))))).collect(),
            false => vec![(0, p.length)],
        };
        let threads = connections.min(pieces.len());
        let shared = Shared { done: Mutex::new(done), dirty: AtomicBool::new(true), failure: Mutex::new(None), next: AtomicUsize::new(0), active: AtomicUsize::new(threads) };
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    self.work(p, &file, &pieces, ranges, &shared, log);
                    shared.active.fetch_sub(1, Ordering::SeqCst);
                    job::ring();
                });
            }
            // This thread writes the state file: when the transfer begins, every few seconds while bytes arrive, at
            // a pause, and when the last connection's thread has ended. It sleeps on the bell between.
            let (mut saved, mut was_paused) = (None::<Instant>, false);
            loop {
                let (over, paused) = (shared.active.load(Ordering::SeqCst) == 0, job::paused());
                if shared.dirty.load(Ordering::SeqCst) && (over || (paused && !was_paused) || saved.is_none_or(|at| at.elapsed() >= SAVE_EVERY)) {
                    shared.dirty.store(false, Ordering::SeqCst);
                    // (the list first, then the sync: the file never lists a byte that was written after the sync)
                    let list = shared.done.lock().unwrap().clone();
                    if let Err(e) = ident.save(&file, &state_path, &list) {
                        shared.fail(format!("{state_path}: {e}"));
                    }
                    saved = Some(Instant::now());
                }
                if over {
                    break;
                }
                was_paused = paused;
                job::wait_bell(if paused { Duration::from_secs(30) } else { Duration::from_secs(1) }, || shared.active.load(Ordering::SeqCst) == 0 || job::paused() != was_paused);
            }
        });
        if let Some(text) = shared.failure.into_inner().unwrap() {
            return Err(text);
        }
        // (a pause that came with the last byte: the request does not go on before the resume)
        job::hold().map_err(stop)?;
        Ok(Fetched { resumed_from, ranges, retries: self.retries(), connections: threads })
    }

    /// One connection's thread: the next piece, from the byte it reached after each drop, until none is left.
    fn work(&self, p: &Part, file: &File, pieces: &[(u64, u64)], ranges: bool, shared: &Shared, log: &(dyn Fn(String) + Sync)) {
        let mut tries = 0u32;
        while let Some(&(first, end)) = pieces.get(shared.next.fetch_add(1, Ordering::SeqCst)) {
            let (mut at, mut best) = (first, first);
            while at < end {
                if shared.failed() {
                    return;
                }
                if let Err(text) = self.gate(&mut tries) {
                    return shared.fail(text);
                }
                if !ranges && at > 0 {
                    // one stream: what it had is fetched again, and counted again
                    at = 0;
                    *shared.done.lock().unwrap() = Ranges::default();
                    shared.dirty.store(true, Ordering::SeqCst);
                    job::recount(0);
                }
                // (how far the transfer was before this try: what the try brings that is new counts for its wait)
                let began = best;
                let fetched = self.stream(p.offset + at, p.offset + end, !ranges, &mut |block| {
                    // between blocks: the heat and battery limits of `serve`. A pause by them was waited out
                    // here, with a connection that is stale now: it is given up, which is no failure
                    let began = Instant::now();
                    if job::check().is_err() || began.elapsed() >= Duration::from_millis(500) {
                        return Err(Trouble::Held);
                    }
                    file.write_all_at(block, at).map_err(|e| Trouble::Final(format!("{}: {e}", p.path)))?;
                    // (written, then counted: after a pause was said nothing is counted, and what is not counted
                    // is not listed)
                    let mut reach = 0;
                    let listed = || {
                        let mut done = shared.done.lock().unwrap();
                        done.add(at, at + block.len() as u64);
                        reach = done.prefix();
                        shared.dirty.store(true, Ordering::SeqCst);
                    };
                    if !job::count_live(block.len() as u64, listed) {
                        return Err(Trouble::Held);
                    }
                    if let Some(frontier) = p.frontier {
                        frontier.set(reach);
                    }
                    at += block.len() as u64;
                    // (bytes the transfer did not have: a stream that began again brings some only past the
                    // byte it had reached)
                    if at > best {
                        best = at;
                        tries = 0;
                        self.health.advance();
                    }
                    Ok(())
                });
                if let Err(trouble) = fetched {
                    if let Err(text) = self.failed(&mut tries, trouble, best.saturating_sub(began)) {
                        return shared.fail(text);
                    }
                }
            }
            // (the line a finished chunk always had; a listener gets the numbers as progress instead)
            let have = shared.done.lock().unwrap().bytes();
            log(format!("{}: {}% ({}/{} MiB)", p.name, have * 100 / p.length.max(1), have >> 20, p.length >> 20));
        }
    }
}

/// A whole file from an https URL (or a local file) into `OUT.part`, by `connections` connections; `OUT.part.state`
/// lists what is there, so a later call goes on. Returns the size and how it went. The caller checks `OUT.part`
/// (its SHA-256: `frontier` says how far it can be read while the download runs) and gives it its name (`serve`'s
/// `download`).
pub fn download(spec: &str, out: &str, connections: usize, chunk: u64, frontier: Option<&Frontier>, begin: &dyn Fn(u64) -> Result<()>, log: &(dyn Fn(String) + Sync)) -> Result<(u64, Fetched)> {
    let source = Source::open(spec)?;
    // (the caller learns the size before the first byte: the room it needs, its progress)
    begin(source.size)?;
    let part = format!("{out}.part");
    let name = spec.rsplit('/').next().unwrap_or(spec);
    let part = Part { name, what: "file".to_string(), offset: 0, length: source.size, path: &part, connections, piece: chunk, whole: true, frontier };
    let fetched = source.fetch(&part, log).map_err(|e| format!("{spec}: download failed ({e})"))?;
    Ok((source.size, fetched))
}

#[cfg(test)]
#[path = "transfer_tests.rs"]
mod tests;
