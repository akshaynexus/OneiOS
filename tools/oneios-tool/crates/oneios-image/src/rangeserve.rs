//! `oneios-tool rangeserve DIR PORT` (was scripts/serve/rangeserve.py, retired 2026-11-13): serve the files of one
//! folder over HTTP with byte ranges, so a download manager can fetch one file in many parallel parts (and from
//! several mirrors of the same server at once). scripts/serve/serve-image.sh starts it.
//!
//!   rangeserve DIR PORT        binds 127.0.0.1 only: reach it through a tunnel, never from the LAN
//!
//! Only plain files directly in DIR (or symlinks to files) are served; no listing beyond them, no paths with '/'.
//!
//! The server is 150 lines over std's TcpListener, a thread per connection (Python's http.server did the same
//! job): GET and HEAD, one range per request, HTTP/1.1 with connections kept open. No HTTP library: what is
//! served is one fixed set of headers, and the rule of which requests are answered is the point of the file.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

const CHUNK: usize = 1 << 20;

/// `urllib.parse.unquote`: %XX to its byte; the result as UTF-8 (a broken sequence: the replacement character).
fn unquote(text: &str) -> String {
    let (bytes, mut out, mut at) = (text.as_bytes(), Vec::new(), 0);
    while at < bytes.len() {
        let hex = bytes.get(at + 1..at + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[at], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The file a request's path names: a plain file directly in the folder, not hidden.
fn file_of(root: &Path, target: &str) -> Option<PathBuf> {
    // (the path alone: no query, no fragment)
    let path = target.split(['?', '#']).next().unwrap_or("");
    let name = unquote(path);
    let name = name.trim_start_matches('/');
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return None;
    }
    Some(root.join(name)).filter(|path| path.is_file())
}

/// What a Range header asks of a file of `size` bytes.
#[derive(Debug, PartialEq)]
enum Range {
    /// No header, or one that is not a single `bytes=` range: the whole file.
    Whole,
    /// First and last byte.
    Part(u64, u64),
    /// A range that the file does not have, or that is no number: 416.
    Bad,
}

fn range_of(header: Option<&str>, size: u64) -> Range {
    let Some(spec) = header.and_then(|h| h.strip_prefix("bytes=")).filter(|h| !h.contains(',')) else { return Range::Whole };
    let (a, b) = spec.trim().split_once('-').unwrap_or((spec.trim(), ""));
    let number = |text: &str| text.trim().parse::<i128>().ok();
    if a.is_empty() {
        // the last N bytes
        return match number(b) {
            // (of an empty file there is no last byte: the script answered 206 with a range of no bytes)
            Some(n) if n > 0 && size > 0 => Range::Part((size as i128 - n).max(0) as u64, size - 1),
            _ => Range::Bad,
        };
    }
    let (Some(start), Some(end)) = (number(a), if b.is_empty() { Some(size as i128 - 1) } else { number(b) }) else { return Range::Bad };
    if start < 0 || start >= size as i128 || end < start {
        return Range::Bad;
    }
    Range::Part(start as u64, (end as u64).min(size - 1))
}

/// One request's answer: the status line and headers, then the body for a GET. Returns the status.
fn answer(root: &Path, method: &str, target: &str, range: Option<&str>, out: &mut TcpStream) -> std::io::Result<u16> {
    let Some(path) = file_of(root, target) else {
        let body = "404 Not Found\n";
        write!(out, "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
        if method == "GET" {
            out.write_all(body.as_bytes())?;
        }
        return Ok(404);
    };
    let size = std::fs::metadata(&path)?.len();
    let (status, start, end) = match range_of(range, size) {
        Range::Bad => {
            write!(out, "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\nContent-Length: 0\r\n\r\n")?;
            return Ok(416);
        }
        Range::Whole => (200, 0, size),
        Range::Part(start, end) => (206, start, end + 1),
    };
    write!(out, "HTTP/1.1 {status} {}\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n", if status == 206 { "Partial Content" } else { "OK" }, end - start)?;
    if status == 206 {
        write!(out, "Content-Range: bytes {start}-{}/{size}\r\n", end - 1)?;
    }
    out.write_all(b"\r\n")?;
    if method == "GET" {
        let mut file = File::open(&path)?;
        file.seek(SeekFrom::Start(start))?;
        let (mut left, mut block) = ((end - start) as usize, vec![0u8; CHUNK]);
        while left > 0 {
            let n = file.read(&mut block[..left.min(CHUNK)])?;
            if n == 0 {
                break;
            }
            out.write_all(&block[..n])?;
            left -= n;
        }
    }
    Ok(status)
}

/// The requests of one connection, until it closes or asks to.
fn serve(root: &Path, stream: TcpStream) -> std::io::Result<()> {
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let request = line.trim_end().to_string();
        let (mut range, mut close) = (None, false);
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 || header.trim_end().is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                match name.to_ascii_lowercase().as_str() {
                    "range" => range = Some(value.trim().to_string()),
                    "connection" => close = value.trim().eq_ignore_ascii_case("close"),
                    _ => {}
                }
            }
        }
        let words: Vec<&str> = request.split_whitespace().collect();
        let status = match words[..] {
            [method @ ("GET" | "HEAD"), target, ..] => answer(root, method, target, range.as_deref(), &mut out)?,
            _ => {
                write!(out, "HTTP/1.1 501 Unsupported method\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
                501
            }
        };
        // the log: one line per request on stderr, as http.server wrote one
        eprintln!("\"{request}\" {status} -");
        if close || status == 404 || status == 501 {
            return Ok(());
        }
    }
}

const USAGE: &str = "usage: oneios-tool rangeserve DIR PORT   (binds 127.0.0.1 only)";

/// `oneios-tool rangeserve DIR PORT`; serves until it is ended.
pub fn main(args: &[String]) -> i32 {
    let [dir, port] = args else {
        eprintln!("{USAGE}");
        return 2;
    };
    let (Ok(root), Ok(port)) = (std::fs::canonicalize(dir), port.parse::<u16>()) else {
        eprintln!("rangeserve: {dir} is no folder, or {port} is no port\n{USAGE}");
        return 2;
    };
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("rangeserve: 127.0.0.1:{port}: {e}");
            return 1;
        }
    };
    for stream in listener.incoming().flatten() {
        let root = root.clone();
        // (a client that goes away in the middle of a file ends its own thread: nothing to report)
        std::thread::spawn(move || serve(&root, stream));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_as_the_script_read_them() {
        assert_eq!(range_of(None, 100), Range::Whole);
        assert_eq!(range_of(Some("items=0-1"), 100), Range::Whole);
        assert_eq!(range_of(Some("bytes=0-1,5-6"), 100), Range::Whole);
        assert_eq!(range_of(Some("bytes=0-9"), 100), Range::Part(0, 9));
        assert_eq!(range_of(Some("bytes=90-"), 100), Range::Part(90, 99));
        assert_eq!(range_of(Some("bytes=90-500"), 100), Range::Part(90, 99));
        assert_eq!(range_of(Some("bytes=-10"), 100), Range::Part(90, 99));
        assert_eq!(range_of(Some("bytes=-500"), 100), Range::Part(0, 99));
        assert_eq!(range_of(Some("bytes=5"), 100), Range::Part(5, 99));
        assert_eq!(range_of(Some("bytes=-5"), 0), Range::Bad);
        for bad in ["bytes=100-", "bytes=9-3", "bytes=-0", "bytes=-", "bytes=a-b", "bytes=0-9"] {
            assert_eq!(range_of(Some(bad), if bad == "bytes=0-9" { 0 } else { 100 }), Range::Bad, "{bad}");
        }
    }

    #[test]
    fn only_plain_files_directly_in_the_folder() {
        let root = std::env::temp_dir().join(format!("oneios-rangeserve-test.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for name in ["a b.bin", ".hidden", "sub/inner"] {
            std::fs::write(root.join(name), b"x").unwrap();
        }
        assert_eq!(file_of(&root, "/a%20b.bin?x=1"), Some(root.join("a b.bin")));
        for target in ["/", "/.hidden", "/sub", "/sub/inner", "/sub%2Finner", "/missing", "/..", "/%2e%2e"] {
            assert_eq!(file_of(&root, target), None, "{target}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
