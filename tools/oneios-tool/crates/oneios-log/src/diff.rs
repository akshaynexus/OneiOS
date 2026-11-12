//! `log diff`: which kinds of lines one log has and the other does not. A kind is a record's source and its first
//! words with the numbers, addresses, pids and times taken out.

use crate::parse::{Log, Src};
use std::collections::BTreeMap;
use std::fmt::Write;

/// A token with its numbers taken out: `0x1f2` -> `0xN`, `disk7s1` -> `diskNsN`, a UUID or a long hex string
/// -> `HEX`.
fn normal_token(token: &str, out: &mut String) {
    let bytes = token.as_bytes();
    let core = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let hexish = core.len() >= 8
        && core.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
        && core.bytes().any(|b| b.is_ascii_digit())
        && core.bytes().any(|b| b.is_ascii_alphabetic());
    if hexish {
        let start = token.find(core).unwrap_or(0);
        out.push_str(&token[..start]);
        out.push_str("HEX");
        out.push_str(&token[start + core.len()..]);
        return;
    }
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'0' && bytes.get(i + 1) == Some(&b'x') && bytes.get(i + 2).is_some_and(|c| c.is_ascii_hexdigit()) {
            i += 2;
            while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                i += 1;
            }
            out.push_str("0xN");
        } else if b.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_digit() || (bytes[i] == b'.' && bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit()))) {
                i += 1;
            }
            out.push('N');
        } else {
            // Copy one character (the token is UTF-8; a multi-byte character has no ASCII byte in it).
            let len = token[i..].chars().next().map_or(1, |c| c.len_utf8());
            out.push_str(&token[i..i + len]);
            i += len;
        }
    }
}

/// The kind of a record: its source and its first `words` words, normalized, runs of one word written once.
pub fn kind(src: Src, text: &str, words: usize) -> String {
    let mut out = match src {
        Src::Host(level) => format!("xnuhost {level} "),
        Src::Guest => "guest ".to_string(),
        Src::Raw => "raw ".to_string(),
    };
    // Report lines whose words after the tag are a list that differs from line to line: the tag is the kind.
    let words = [("oneios-cpu: ", 3), ("oneios-gpu-clients: ", 1), ("oneios-gpu-askers: ", 1), ("oneios-display-report: d+", 2)]
        .iter()
        .find(|(tag, _)| text.starts_with(tag))
        .map_or(words, |(_, n)| (*n).min(words));
    let mut last = String::new();
    let mut count = 0;
    for token in text.split_whitespace() {
        let mut word = String::new();
        normal_token(token, &mut word);
        if word == last {
            continue;
        }
        if count == words {
            out.push_str(" ...");
            break;
        }
        if count > 0 {
            out.push(' ');
        }
        out.push_str(&word);
        last = word;
        count += 1;
    }
    out
}

/// kind -> (count, the first line).
fn kinds(log: &Log, words: usize) -> BTreeMap<String, (usize, usize)> {
    let mut out: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for r in &log.recs {
        let e = out.entry(kind(r.src, &r.text, words)).or_insert((0, r.line));
        e.0 += 1;
    }
    out
}

/// The kernel's boot arguments of the first boot: name -> value (`placed: ... boot-args "..."`).
fn boot_args(log: &Log) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let line = log.recs.iter().find(|r| matches!(r.src, Src::Host(_)) && r.text.starts_with("placed: ") && r.text.contains("boot-args \""));
    if let Some(r) = line {
        let args = r.text.split_once("boot-args \"").map_or("", |x| x.1);
        for arg in args.trim_end_matches('"').split_whitespace() {
            let (name, value) = arg.split_once('=').unwrap_or((arg, ""));
            out.insert(name.to_string(), value.to_string());
        }
    }
    out
}

pub fn report(a: &Log, b: &Log, words: usize, all: bool) -> String {
    let mut out = String::new();
    let ka = kinds(a, words);
    let kb = kinds(b, words);
    let _ = writeln!(out, "A: {} ({} lines, {} records, {} kinds)", a.path, a.lines, a.recs.len(), ka.len());
    let _ = writeln!(out, "B: {} ({} lines, {} records, {} kinds)", b.path, b.lines, b.recs.len(), kb.len());
    let _ = writeln!(out, "a kind: the source and the first {words} words of a record, numbers and addresses taken out");

    let (args_a, args_b) = (boot_args(a), boot_args(b));
    let _ = writeln!(out, "\n== boot arguments");
    let mut any = false;
    for (name, value) in &args_a {
        match args_b.get(name) {
            None => {
                let _ = writeln!(out, "  only in A: {name}{}{value}", if value.is_empty() { "" } else { "=" });
                any = true;
            }
            Some(other) if other != value => {
                let _ = writeln!(out, "  differs:   {name}: A {value}  B {other}");
                any = true;
            }
            _ => {}
        }
    }
    for (name, value) in &args_b {
        if !args_a.contains_key(name) {
            let _ = writeln!(out, "  only in B: {name}{}{value}", if value.is_empty() { "" } else { "=" });
            any = true;
        }
    }
    if !any {
        let _ = writeln!(out, "  the same ({} arguments)", args_a.len());
    }

    let only = |out: &mut String, title: &str, mine: &BTreeMap<String, (usize, usize)>, other: &BTreeMap<String, (usize, usize)>| {
        let mut rows: Vec<(&String, &(usize, usize))> = mine.iter().filter(|(k, _)| !other.contains_key(*k)).collect();
        rows.sort_by_key(|(k, v)| (v.1, k.as_str()));
        let _ = writeln!(out, "\n== only in {title} ({} kinds)", rows.len());
        if !rows.is_empty() {
            let _ = writeln!(out, "  {:>6}  {:>6}  kind", "count", "line");
        }
        for (k, (count, line)) in rows {
            let _ = writeln!(out, "  {count:>6}  {line:>6}  {k}");
        }
    };
    only(&mut out, "A", &ka, &kb);
    only(&mut out, "B", &kb, &ka);

    // (kind, A's count and first line, B's)
    type Both<'a> = (&'a String, (usize, usize), (usize, usize));
    let mut rows: Vec<Both> = ka
        .iter()
        .filter_map(|(k, va)| kb.get(k).map(|vb| (k, *va, *vb)))
        .filter(|(_, va, vb)| va.0 != vb.0 && (all || va.0 >= 2 * vb.0 || vb.0 >= 2 * va.0))
        .collect();
    rows.sort_by_key(|(k, va, _)| (va.1, k.as_str()));
    let rule = if all { "the counts differ" } else { "one count is twice the other or more; --all shows every difference" };
    let _ = writeln!(out, "\n== in both ({rule}): {} kinds", rows.len());
    if !rows.is_empty() {
        let _ = writeln!(out, "  {:>6}  {:>6}  {:>6}  {:>6}  kind", "A", "B", "lineA", "lineB");
    }
    for (k, va, vb) in rows {
        let _ = writeln!(out, "  {:>6}  {:>6}  {:>6}  {:>6}  {k}", va.0, vb.0, va.1, vb.1);
    }
    out
}
