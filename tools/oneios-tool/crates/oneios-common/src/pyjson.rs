//! JSON as Python's `json` module reads and writes it.
//!
//! `dumps` is serde_json's serializer with a `Formatter` for Python's layout (below); the parser is this module's
//! own (serde_json does not read Python's `NaN` and `Infinity`, nor integers beyond 64 bits as integers).
//! `dumps` gives the bytes of `json.dumps(value, indent=..., sort_keys=...)` with Python's defaults
//! (`ensure_ascii=True`; separators `", "` and `": "` without an indent, `","` + newline and `": "` with one;
//! floats as `repr(float)`; `NaN`, `Infinity`, `-Infinity`). Objects keep their insertion order, like a Python dict.
//! `parse` keeps the order of an object's keys and the difference between `1` and `1.0`, so a file read and written
//! again is the file Python would write.

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use serde_json::ser::Formatter;
use std::io;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Dict(Vec<(String, Value)>),
}

impl Value {
    pub fn dict() -> Value {
        Value::Dict(Vec::new())
    }
    pub fn list() -> Value {
        Value::List(Vec::new())
    }
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(s.into())
    }
    /// `d[key] = value` of a Python dict: replaces the value in place when the key exists, else appends.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        let key = key.into();
        let Value::Dict(items) = self else { panic!("pyjson: set on a non-dict") };
        match items.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value.into(),
            None => items.push((key, value.into())),
        }
    }
    pub fn push(&mut self, value: impl Into<Value>) {
        let Value::List(items) = self else { panic!("pyjson: push on a non-list") };
        items.push(value.into());
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self {
            Value::Dict(items) => items.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i128> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        self.as_int().and_then(|i| u64::try_from(i).ok())
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }
    pub fn as_dict(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Dict(items) => Some(items),
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

macro_rules! from_int {
    ($($t:ty),*) => {$(impl From<$t> for Value { fn from(v: $t) -> Value { Value::Int(v as i128) } })*};
}
from_int!(i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, usize);
impl From<bool> for Value {
    fn from(v: bool) -> Value {
        Value::Bool(v)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Value {
        Value::Float(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Value {
        Value::Str(v.to_string())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Value {
        Value::Str(v)
    }
}
impl From<&String> for Value {
    fn from(v: &String) -> Value {
        Value::Str(v.clone())
    }
}
impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Value {
        Value::List(v.into_iter().map(Into::into).collect())
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Value {
        v.map_or(Value::Null, Into::into)
    }
}

/// `repr(float)` of Python: the shortest digits that read back as the same double; exponent form below 1e-4 and
/// from 1e16 up, with a sign and at least two exponent digits.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // Rust's `{:e}` prints the shortest round-trip digits: "d.ddde-7".
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if f < 0.0 { "-" } else { "" };
    let n = digits.len() as i32;
    if (-4..16).contains(&exp) {
        let body = if exp < 0 {
            format!("0.{}{}", "0".repeat((-exp - 1) as usize), digits)
        } else if n <= exp + 1 {
            format!("{}{}.0", digits, "0".repeat((exp + 1 - n) as usize))
        } else {
            format!("{}.{}", &digits[..(exp + 1) as usize], &digits[(exp + 1) as usize..])
        };
        format!("{sign}{body}")
    } else {
        let m = if n == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{sign}{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// One value as serde sees it: a dict's entries in their own order (or sorted), an integer of any size.
struct Py<'a> {
    value: &'a Value,
    sort_keys: bool,
}

impl Serialize for Py<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let inner = |value| Py { value, sort_keys: self.sort_keys };
        match self.value {
            Value::Null => s.serialize_unit(),
            Value::Bool(b) => s.serialize_bool(*b),
            Value::Int(i) => s.serialize_i128(*i),
            Value::Float(f) if f.is_finite() => s.serialize_f64(*f),
            // serde_json writes `null` for these before it asks the formatter; Python writes the word. The
            // formatter below writes a byte string as it is, and nothing else here is one.
            Value::Float(f) => s.serialize_bytes(float_repr(*f).as_bytes()),
            Value::Str(text) => s.serialize_str(text),
            Value::List(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&inner(item))?;
                }
                seq.end()
            }
            Value::Dict(items) => {
                let mut order: Vec<&(String, Value)> = items.iter().collect();
                if self.sort_keys {
                    // Python sorts str keys by code point; UTF-8 byte order is the same order.
                    order.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
                }
                let mut map = s.serialize_map(Some(order.len()))?;
                for (key, value) in order {
                    map.serialize_entry(key, &inner(value))?;
                }
                map.end()
            }
        }
    }
}

/// The layout of Python's `json.dumps` for serde_json's serializer: `", "` and `": "` without an indent; with
/// one, `","`, a line end and `indent * level` spaces (`[]` and `{}` stay closed); every character outside
/// 0x20..0x7e as `\uXXXX` (`ensure_ascii`); floats as `repr(float)`.
struct PyFormatter {
    indent: Option<usize>,
    level: usize,
    has_value: bool,
}

impl PyFormatter {
    fn open<W: ?Sized + io::Write>(&mut self, w: &mut W, bracket: &[u8]) -> io::Result<()> {
        self.level += 1;
        self.has_value = false;
        w.write_all(bracket)
    }
    fn close<W: ?Sized + io::Write>(&mut self, w: &mut W, bracket: &[u8]) -> io::Result<()> {
        self.level -= 1;
        if self.has_value {
            self.line(w)?;
        }
        w.write_all(bracket)
    }
    fn line<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        if let Some(n) = self.indent {
            w.write_all(b"\n")?;
            for _ in 0..n * self.level {
                w.write_all(b" ")?;
            }
        }
        Ok(())
    }
    fn item<W: ?Sized + io::Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        match (first, self.indent) {
            (true, _) => {}
            (false, None) => w.write_all(b", ")?,
            (false, Some(_)) => w.write_all(b",")?,
        }
        self.line(w)
    }
}

impl Formatter for PyFormatter {
    fn write_f64<W: ?Sized + io::Write>(&mut self, w: &mut W, value: f64) -> io::Result<()> {
        w.write_all(float_repr(value).as_bytes())
    }
    fn write_byte_array<W: ?Sized + io::Write>(&mut self, w: &mut W, value: &[u8]) -> io::Result<()> {
        w.write_all(value)
    }
    fn write_string_fragment<W: ?Sized + io::Write>(&mut self, w: &mut W, fragment: &str) -> io::Result<()> {
        // serde_json escapes the quote, the backslash and everything below 0x20 itself, as Python does
        let mut plain = 0;
        for (at, c) in fragment.char_indices() {
            if (c as u32) > 0x7e {
                w.write_all(&fragment.as_bytes()[plain..at])?;
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    write!(w, "\\u{unit:04x}")?;
                }
                plain = at + c.len_utf8();
            }
        }
        w.write_all(&fragment.as_bytes()[plain..])
    }
    fn begin_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.open(w, b"[")
    }
    fn end_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.close(w, b"]")
    }
    fn begin_array_value<W: ?Sized + io::Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        self.item(w, first)
    }
    fn end_array_value<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        self.has_value = true;
        Ok(())
    }
    fn begin_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.open(w, b"{")
    }
    fn end_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.close(w, b"}")
    }
    fn begin_object_key<W: ?Sized + io::Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        self.item(w, first)
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        w.write_all(b": ")
    }
    fn end_object_value<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        self.has_value = true;
        Ok(())
    }
}

/// `json.dumps(value, indent=indent, sort_keys=sort_keys)`.
pub fn dumps(value: &Value, indent: Option<usize>, sort_keys: bool) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PyFormatter { indent, level: 0, has_value: false });
    Py { value, sort_keys }.serialize(&mut ser).expect("a Vec takes every write, and every key is a string");
    String::from_utf8(out).expect("the formatter writes ASCII only")
}

/// `json.loads(text)`.
pub fn parse(text: &str) -> Result<Value, String> {
    let mut p = Parser { s: text.as_bytes(), at: 0 };
    p.space();
    let value = p.value()?;
    p.space();
    if p.at != p.s.len() {
        return Err(format!("json: extra data at byte {}", p.at));
    }
    Ok(value)
}

/// `json.load(open(path))`.
pub fn load(path: impl AsRef<std::path::Path>) -> anyhow::Result<Value> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.at < self.s.len() && matches!(self.s[self.at], b' ' | b'\t' | b'\n' | b'\r') {
            self.at += 1;
        }
    }
    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("json: {what} at byte {}", self.at))
    }
    fn value(&mut self) -> Result<Value, String> {
        let Some(&c) = self.s.get(self.at) else { return self.err("end of text") };
        match c {
            b'{' => {
                self.at += 1;
                let mut items: Vec<(String, Value)> = Vec::new();
                let mut seen: Option<std::collections::HashMap<String, usize>> = None;
                self.space();
                if self.s.get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Ok(Value::Dict(items));
                }
                loop {
                    self.space();
                    if self.s.get(self.at) != Some(&b'"') {
                        return self.err("expected a key");
                    }
                    let key = self.string()?;
                    self.space();
                    if self.s.get(self.at) != Some(&b':') {
                        return self.err("expected ':'");
                    }
                    self.at += 1;
                    self.space();
                    let value = self.value()?;
                    // A repeated key keeps its first position and takes the last value, like a Python dict.
                    // Small objects are searched; a large one gets an index.
                    if seen.is_none() && items.len() >= 32 {
                        seen = Some(items.iter().enumerate().map(|(i, (k, _))| (k.clone(), i)).collect());
                    }
                    match &mut seen {
                        Some(index) => match index.get(&key) {
                            Some(&i) => items[i].1 = value,
                            None => {
                                index.insert(key.clone(), items.len());
                                items.push((key, value));
                            }
                        },
                        None => match items.iter_mut().find(|(k, _)| *k == key) {
                            Some(slot) => slot.1 = value,
                            None => items.push((key, value)),
                        },
                    }
                    self.space();
                    match self.s.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Value::Dict(items));
                        }
                        _ => return self.err("expected ',' or '}'"),
                    }
                }
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.s.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(Value::List(items));
                }
                loop {
                    self.space();
                    items.push(self.value()?);
                    self.space();
                    match self.s.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Value::List(items));
                        }
                        _ => return self.err("expected ',' or ']'"),
                    }
                }
            }
            b'"' => Ok(Value::Str(self.string()?)),
            b't' if self.s[self.at..].starts_with(b"true") => {
                self.at += 4;
                Ok(Value::Bool(true))
            }
            b'f' if self.s[self.at..].starts_with(b"false") => {
                self.at += 5;
                Ok(Value::Bool(false))
            }
            b'n' if self.s[self.at..].starts_with(b"null") => {
                self.at += 4;
                Ok(Value::Null)
            }
            b'N' if self.s[self.at..].starts_with(b"NaN") => {
                self.at += 3;
                Ok(Value::Float(f64::NAN))
            }
            b'I' if self.s[self.at..].starts_with(b"Infinity") => {
                self.at += 8;
                Ok(Value::Float(f64::INFINITY))
            }
            b'-' if self.s[self.at..].starts_with(b"-Infinity") => {
                self.at += 9;
                Ok(Value::Float(f64::NEG_INFINITY))
            }
            b'-' | b'0'..=b'9' => {
                let start = self.at;
                let mut float = false;
                self.at += 1;
                while let Some(&c) = self.s.get(self.at) {
                    match c {
                        b'0'..=b'9' => {}
                        b'.' | b'e' | b'E' | b'+' | b'-' => float = true,
                        _ => break,
                    }
                    self.at += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.at]).unwrap();
                if float {
                    text.parse::<f64>().map(Value::Float).or_else(|_| self.err("bad number"))
                } else {
                    text.parse::<i128>().map(Value::Int).or_else(|_| self.err("bad number"))
                }
            }
            _ => self.err("unexpected character"),
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        let Some(h) = self.s.get(self.at..self.at + 4) else { return self.err("short \\u escape") };
        let text = std::str::from_utf8(h).map_err(|_| "json: bad \\u escape".to_string())?;
        let v = u32::from_str_radix(text, 16).map_err(|_| "json: bad \\u escape".to_string())?;
        self.at += 4;
        Ok(v)
    }
    fn string(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let start = self.at;
            while self.at < self.s.len() && self.s[self.at] != b'"' && self.s[self.at] != b'\\' {
                self.at += 1;
            }
            out.extend_from_slice(&self.s[start..self.at]);
            match self.s.get(self.at) {
                None => return self.err("unterminated string"),
                Some(b'"') => {
                    self.at += 1;
                    return String::from_utf8(out).map_err(|_| "json: bad UTF-8".to_string());
                }
                _ => {
                    self.at += 1;
                    let Some(&e) = self.s.get(self.at) else { return self.err("unterminated string") };
                    self.at += 1;
                    let c = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut u = self.hex4()?;
                            if (0xd800..0xdc00).contains(&u) && self.s[self.at..].starts_with(b"\\u") {
                                let save = self.at;
                                self.at += 2;
                                let low = self.hex4()?;
                                if (0xdc00..0xe000).contains(&low) {
                                    u = 0x10000 + ((u - 0xd800) << 10) + (low - 0xdc00);
                                } else {
                                    self.at = save;
                                }
                            }
                            // A lone surrogate has no Rust char; U+FFFD stands in (the tools' data has none).
                            char::from_u32(u).unwrap_or('\u{fffd}')
                        }
                        _ => return self.err("bad escape"),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The escapes of e-acute and of U+1F600 as Python writes them (built here so no tool rewrites them).
    fn esc() -> String {
        ["u00e9", "ud83d", "ude00"].iter().map(|h| format!("{}{h}", '\\')).collect()
    }

    #[test]
    fn floats_as_python_repr() {
        for (f, want) in [
            (0.1, "0.1"),
            (1.0, "1.0"),
            (100.0, "100.0"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (9999999999999998.0, "9999999999999998.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.25e-7, "1.25e-07"),
            (-2.5, "-2.5"),
            (123456.789, "123456.789"),
            (1e100, "1e+100"),
            (0.9995, "0.9995"),
            (3.0e-5, "3e-05"),
        ] {
            assert_eq!(float_repr(f), want);
        }
    }

    #[test]
    fn dumps_as_python() {
        let mut d = Value::dict();
        d.set("b", 1);
        d.set("a", vec![Value::Int(1), Value::Null, Value::Bool(true)]);
        d.set("e", Value::list());
        d.set("s", "a\"\\\n\u{e9}\u{1f600}\u{7f}");
        assert_eq!(
            dumps(&d, None, false),
            r#"{"b": 1, "a": [1, null, true], "e": [], "s": "a\"\\\nESC\u007f"}"#.replace("ESC", &esc())
        );
        assert_eq!(dumps(&d, None, true).split(": ").next().unwrap(), "{\"a\"");
        let mut small = Value::dict();
        small.set("k", vec![Value::Int(1), Value::Int(2)]);
        small.set("d", Value::dict());
        assert_eq!(dumps(&small, Some(1), false), "{\n \"k\": [\n  1,\n  2\n ],\n \"d\": {}\n}");
        assert_eq!(dumps(&small, Some(0), false), "{\n\"k\": [\n1,\n2\n],\n\"d\": {}\n}");
    }

    #[test]
    fn parse_round_trip() {
        let text = r#"{"z": 1, "a": [1.0, -2, 3.5e-07, "xESC"], "n": null, "t": true, "z": 2}"#.replace("ESC", &esc());
        let v = parse(&text).unwrap();
        assert_eq!(v.get("z").unwrap().as_int(), Some(2));
        assert_eq!(
            dumps(&v, None, false),
            r#"{"z": 2, "a": [1.0, -2, 3.5e-07, "xESC"], "n": null, "t": true}"#.replace("ESC", &esc())
        );
    }
}
