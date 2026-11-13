//! Python's `repr()`, `str()` and `bool()` of values read from JSON or TOML (a pyjson::Value), for messages that
//! must read as the Python tools' did. Moved here from oneios-image's py.rs when the prelink and kernel ports needed them.

use crate::pyjson::{float_repr, Value};
use std::fmt::Write as _;

/// `repr(str)`. Printable text stays as it is; the tests of what is printable cover ASCII, the control
/// characters and the common separators and format characters, not the whole Unicode table.
pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !printable(c) => {
                let n = c as u32;
                if n < 0x100 {
                    let _ = write!(out, "\\x{n:02x}");
                } else if n < 0x10000 {
                    let _ = write!(out, "\\u{n:04x}");
                } else {
                    let _ = write!(out, "\\U{n:08x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn printable(c: char) -> bool {
    let n = c as u32;
    if n < 0x7f {
        return n >= 0x20;
    }
    !(c.is_control()
        || matches!(
            n,
            0xa0 | 0xad | 0x1680 | 0x2000..=0x200f | 0x2028..=0x202f | 0x205f..=0x2064 | 0x3000 | 0xfeff | 0xfff9..=0xfffb
        ))
}

/// `repr()` of a value read from JSON or TOML.
pub fn repr_value(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            if f.is_nan() {
                "nan".into()
            } else if f.is_infinite() {
                if *f > 0.0 { "inf".into() } else { "-inf".into() }
            } else {
                float_repr(*f)
            }
        }
        Value::Str(s) => repr_str(s),
        Value::List(items) => format!("[{}]", items.iter().map(repr_value).collect::<Vec<_>>().join(", ")),
        Value::Dict(items) => format!(
            "{{{}}}",
            items.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr_value(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `str()` of a value read from JSON or TOML.
pub fn str_value(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        other => repr_value(other),
    }
}

/// `repr()` of a list of strings.
pub fn repr_strs<S: AsRef<str>>(items: &[S]) -> String {
    format!("[{}]", items.iter().map(|s| repr_str(s.as_ref())).collect::<Vec<_>>().join(", "))
}

/// `bool(value)`.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Int(i)) => *i != 0,
        Some(Value::Float(f)) => *f != 0.0,
        Some(Value::Str(s)) => !s.is_empty(),
        Some(Value::List(l)) => !l.is_empty(),
        Some(Value::Dict(d)) => !d.is_empty(),
    }
}

