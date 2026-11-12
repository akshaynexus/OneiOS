//! An xnuhost session log as records.
//!
//! The framing is `runtime/xnuhost/src/log.c`'s: `[seconds.millis] xnuhost I|E|D text`, `[seconds.millis] guest
//! text` (the guest's console), and lines of the helpers and of the session's scripts with no time. Two writers
//! share the guest's console, so a record can be cut and the next one appended to the same line, and a record
//! can lose its first characters (`neios-ubt: ...`). `load` splits such lines into their records and marks which
//! ones are cut; nothing here panics on a malformed line.

use anyhow::Result;
use rayon::prelude::*;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Src {
    /// `xnuhost L text`: the level letter.
    Host(char),
    /// The guest's console.
    Guest,
    /// A line with no time: a helper's or a script's.
    Raw,
}

#[derive(Clone, Debug)]
pub struct Rec {
    /// 1-based line of the log.
    pub line: usize,
    /// Seconds of the host's clock, from the line's `[...]`.
    pub time: Option<f64>,
    pub src: Src,
    pub text: String,
    /// This record was appended to a cut one on the same line.
    pub appended: bool,
    /// The record's first characters were lost and put back (`neios-ubt:` read as `oneios-ubt:`).
    pub head_cut: bool,
    /// The record's end is probably lost: another record was appended to it, or the next one lost its head.
    pub tail_cut: bool,
}

pub struct Log {
    pub path: String,
    pub lines: usize,
    pub recs: Vec<Rec>,
}

/// The guest tags a record starts with when it is appended to a cut one: `oneios-NAME: `, `ONEIOS NAME: `,
/// `sheng-NAME` followed by `:`, `[` or a space.
fn tag_at(text: &str, at: usize) -> bool {
    let rest = &text.as_bytes()[at..];
    let word_then = |start: usize, ok: fn(u8) -> bool| -> Option<usize> {
        let mut i = start;
        while i < rest.len() && ok(rest[i]) {
            i += 1;
        }
        (i > start).then_some(i)
    };
    if rest.starts_with(b"oneios-") {
        if let Some(i) = word_then(7, |c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-') {
            return rest[i..].starts_with(b": ");
        }
    } else if rest.starts_with(b"ONEIOS ") {
        if let Some(i) = word_then(7, |c| c.is_ascii_uppercase()) {
            return rest[i..].starts_with(b": ");
        }
    } else if rest.starts_with(b"sheng-") {
        if text[..at].trim_end() == "sheng-ubt" {
            return false; // `sheng-ubt sheng-exc: ...` is one record
        }
        if let Some(i) = word_then(6, |c| c.is_ascii_lowercase()) {
            return matches!(rest.get(i), Some(b':') | Some(b'[')) || rest[i..].starts_with(b" sheng-exc:");
        }
    }
    false
}

/// Split a guest line's text where another record was appended: the pieces, in order.
pub fn split_joined(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if (bytes[i] == b'o' || bytes[i] == b'O' || bytes[i] == b's') && text.is_char_boundary(i) && tag_at(&text[start..], i - start) {
            out.push(&text[start..i]);
            start = i;
        }
        i += 1;
    }
    out.push(&text[start..]);
    out
}

/// A record that lost up to three first characters of `oneios-`: the text with them put back.
fn repair_head(text: &str) -> Option<String> {
    for lost in 1..=3 {
        let rest = &"oneios-"[lost..];
        if let Some(tail) = text.strip_prefix(rest) {
            let fixed = format!("oneios-{tail}");
            if tag_at(&fixed, 0) {
                return Some(fixed);
            }
        }
    }
    None
}

fn parse_line(no: usize, raw: &[u8]) -> Vec<Rec> {
    let text = String::from_utf8_lossy(raw);
    let text = text.trim_end_matches(['\r', '\n']);
    let rec = |time, src, text: &str| Rec { line: no, time, src, text: text.to_string(), appended: false, head_cut: false, tail_cut: false };
    let framed = text.strip_prefix('[').and_then(|rest| {
        let close = rest.find(']')?;
        let time: f64 = rest[..close].trim().parse().ok()?;
        Some((time, rest[close + 1..].strip_prefix(' ').unwrap_or(&rest[close + 1..])))
    });
    let Some((time, rest)) = framed else {
        return vec![rec(None, Src::Raw, text)];
    };
    if let Some(body) = rest.strip_prefix("xnuhost ") {
        let mut chars = body.chars();
        if let (Some(level), Some(' ')) = (chars.next(), chars.next()) {
            if level.is_ascii_uppercase() {
                return vec![rec(Some(time), Src::Host(level), chars.as_str())];
            }
        }
        return vec![rec(Some(time), Src::Host('?'), body)];
    }
    let Some(body) = rest.strip_prefix("guest") else {
        return vec![rec(Some(time), Src::Raw, rest)];
    };
    // The tag is `guest` padded to nine characters and a space; a record's own indent comes after it.
    let pad = body.bytes().take(5).take_while(|&b| b == b' ').count();
    let body = &body[pad..];
    let mut out = Vec::new();
    for (i, piece) in split_joined(body).into_iter().enumerate() {
        let mut r = rec(Some(time), Src::Guest, piece);
        r.appended = i > 0;
        if i == 0 {
            if let Some(fixed) = repair_head(piece) {
                r.text = fixed;
                r.head_cut = true;
            }
        }
        out.push(r);
    }
    let n = out.len();
    for r in &mut out[..n - 1] {
        r.tail_cut = true;
    }
    out
}

pub fn parse(path: &str, data: &[u8]) -> Log {
    let mut lines: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let per_line: Vec<Vec<Rec>> = lines.par_iter().enumerate().map(|(i, raw)| parse_line(i + 1, raw)).collect();
    let mut recs: Vec<Rec> = per_line.into_iter().flatten().collect();
    // A record whose first characters are lost follows one whose end is lost.
    for i in 1..recs.len() {
        if recs[i].head_cut {
            recs[i - 1].tail_cut = true;
        }
    }
    Log { path: path.to_string(), lines: lines.len(), recs }
}

pub fn load(path: &Path) -> Result<Log> {
    let map = oneios_common::Map::open(path)?;
    Ok(parse(&path.display().to_string(), &map))
}

/// The boots of a log: a boot starts at `xnuhost: session ABI` (or at a restart in place) and has one kernel
/// banner; a second banner with no start line before it starts a boot too.
#[derive(Clone, Debug)]
pub struct Boot {
    /// Record range `[first, end)`.
    pub first: usize,
    pub end: usize,
    pub first_line: usize,
    pub last_line: usize,
    /// The time of the boot's first timed record.
    pub start_time: Option<f64>,
}

pub fn boots(log: &Log) -> Vec<Boot> {
    let mut starts = vec![0usize];
    let mut banner = false;
    for (i, r) in log.recs.iter().enumerate() {
        let start_line = matches!(r.src, Src::Host(_))
            && (r.text.starts_with("xnuhost: session ABI") || r.text.starts_with("the guest asked for a restart"));
        let is_banner = r.src == Src::Guest && r.text.starts_with("Darwin Kernel Version");
        if (start_line || is_banner) && banner {
            starts.push(i);
            banner = false;
        }
        if is_banner {
            banner = true;
        }
    }
    let mut out = Vec::new();
    for (k, &first) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(log.recs.len());
        if first == end {
            continue;
        }
        out.push(Boot {
            first,
            end,
            first_line: log.recs[first].line,
            last_line: log.recs[end - 1].line,
            start_time: log.recs[first..end].iter().find_map(|r| r.time),
        });
    }
    out
}

/// A cursor over a record's text that reads the number after a label and moves on: fields that a cut record or
/// an older format does not have stay None and do not disturb the ones after them.
pub struct Scan<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Scan<'a> {
    pub fn new(text: &'a str) -> Scan<'a> {
        Scan { text, at: 0 }
    }

    /// Move after the next `label`; false (and no move) when it is not there.
    pub fn skip(&mut self, label: &str) -> bool {
        match self.text[self.at..].find(label) {
            Some(i) => {
                self.at += i + label.len();
                true
            }
            None => false,
        }
    }

    fn number_len(&self) -> usize {
        let rest = &self.text.as_bytes()[self.at..];
        let mut i = 0;
        if i < rest.len() && (rest[i] == b'-' || rest[i] == b'+') {
            i += 1;
        }
        let digits = i;
        while i < rest.len() && rest[i].is_ascii_digit() {
            i += 1;
        }
        if i == digits {
            return 0;
        }
        if i + 1 < rest.len() && rest[i] == b'.' && rest[i + 1].is_ascii_digit() {
            i += 1;
            while i < rest.len() && rest[i].is_ascii_digit() {
                i += 1;
            }
        }
        i
    }

    /// The number right after the next `label` (an integer or a decimal, with an optional sign).
    pub fn num(&mut self, label: &str) -> Option<f64> {
        let saved = self.at;
        if !self.skip(label) {
            return None;
        }
        let len = self.number_len();
        let value = self.text[self.at..self.at + len].trim_start_matches('+').parse::<f64>().ok();
        match value {
            Some(v) => {
                self.at += len;
                Some(v)
            }
            None => {
                self.at = saved;
                None
            }
        }
    }

    /// The hex number (after `0x`) right after the next `label`.
    pub fn hex(&mut self, label: &str) -> Option<u64> {
        let saved = self.at;
        if !self.skip(label) {
            return None;
        }
        let rest = &self.text[self.at..];
        let digits = rest.strip_prefix("0x").unwrap_or(rest);
        let len = digits.bytes().take_while(|b| b.is_ascii_hexdigit()).count();
        match u64::from_str_radix(&digits[..len], 16) {
            Ok(v) => {
                self.at += rest.len() - digits.len() + len;
                Some(v)
            }
            Err(_) => {
                self.at = saved;
                None
            }
        }
    }

    pub fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }
}

/// `0x...` at the start of a token, as a number.
pub fn hex_token(token: &str) -> Option<u64> {
    let digits = token.strip_prefix("0x")?;
    if digits.is_empty() || digits.len() > 16 {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}
