//! Small pieces of Python that imagetool.py's output depends on: `repr()` and `str()` of values in messages,
//! `int()`, `os.path` on strings, `shlex.quote`, `urllib.parse.quote`, base64, and the text of an `OSError`.

use oneios_common::pyjson::Value;
pub use oneios_common::pyrepr::{repr_str, repr_strs, repr_value, str_value, truthy};

/// The value as a Python `int`: an integer, or a bool (`isinstance(True, int)` holds in Python).
pub fn as_int(v: &Value) -> Option<i128> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Bool(b) => Some(*b as i128),
        _ => None,
    }
}

/// `value == number` as Python compares an int with an int, a bool or a float.
pub fn eq_int(v: Option<&Value>, n: i128) -> bool {
    match v {
        Some(Value::Int(i)) => *i == n,
        Some(Value::Bool(b)) => *b as i128 == n,
        Some(Value::Float(f)) => *f == n as f64,
        _ => false,
    }
}

pub fn int(text: &str, base: u32) -> Option<i128> {
    let t = text.trim_matches(|c: char| c.is_whitespace());
    let (negative, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut t = t;
    if base == 8 && (t.starts_with("0o") || t.starts_with("0O")) {
        t = &t[2..];
        if let Some(rest) = t.strip_prefix('_') {
            t = rest;
        }
    }
    if t.is_empty() || t.starts_with('_') || t.ends_with('_') || t.contains("__") {
        return None;
    }
    let mut n: i128 = 0;
    for c in t.chars() {
        if c == '_' {
            continue;
        }
        let d = c.to_digit(base)? as i128;
        n = n.checked_mul(base as i128)?.checked_add(d)?;
    }
    Some(if negative { -n } else { n })
}

/// What `re.match(r"^[SET]+$", s)` tests when every character must be in a set: `$` also matches before one
/// last newline.
pub fn before_newline(s: &str) -> &str {
    s.strip_suffix('\n').unwrap_or(s)
}

pub fn is_lower_hex(s: &str) -> bool {
    let s = before_newline(s);
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn is_hex64(s: &str) -> bool {
    is_lower_hex(s) && before_newline(s).len() == 64
}

/// `os.path.join(a, b)`.
pub fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') {
        b.to_string()
    } else if a.is_empty() || a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

/// `os.path.dirname(p)`.
pub fn dirname(p: &str) -> &str {
    match p.rfind('/') {
        None => "",
        Some(i) => {
            let head = &p[..i + 1];
            let trimmed = head.trim_end_matches('/');
            if trimmed.is_empty() { head } else { trimmed }
        }
    }
}

/// `os.path.basename(p)`.
pub fn basename(p: &str) -> &str {
    match p.rfind('/') {
        None => p,
        Some(i) => &p[i + 1..],
    }
}

/// `os.path.normpath(p)` (posixpath).
pub fn normpath(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let slashes = if p.starts_with("//") && !p.starts_with("///") {
        2
    } else if p.starts_with('/') {
        1
    } else {
        0
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in p.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part != ".." || (slashes == 0 && parts.is_empty()) || parts.last() == Some(&"..") {
            parts.push(part);
        } else if !parts.is_empty() {
            parts.pop();
        }
    }
    let body = parts.join("/");
    let out = format!("{}{}", "/".repeat(slashes), body);
    if out.is_empty() { ".".into() } else { out }
}

/// `shlex.quote(s)`.
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// `urllib.parse.quote(s, safe=".-_")`: letters, digits and `_.-~` stay, every other byte is `%XX`.
pub fn url_quote(s: &str) -> String {
    use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
    const QUOTED: &AsciiSet = &NON_ALPHANUMERIC.remove(b'_').remove(b'.').remove(b'-').remove(b'~');
    utf8_percent_encode(s, QUOTED).to_string()
}

/// `base64.b64encode(data).decode()`.
pub fn b64(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// `str(OSError)` for an error on a path: `[Errno 2] No such file or directory: 'PATH'`.
pub fn os_error(e: &std::io::Error, path: &str) -> String {
    match e.raw_os_error() {
        Some(n) => {
            // Safety: strerror returns a pointer to a NUL-terminated message; it is copied at once.
            let text = unsafe { std::ffi::CStr::from_ptr(libc::strerror(n)) }.to_string_lossy().into_owned();
            format!("[Errno {n}] {text}: {}", repr_str(path))
        }
        None => format!("{e}: {}", repr_str(path)),
    }
}

/// `int(st.st_mtime)`: Python makes the float `sec + 1e-9 * nsec` and int() cuts it towards zero, so a time
/// one nanosecond below a second can come out as the next second.
pub fn int_mtime(sec: i64, nsec: i64) -> i64 {
    (sec as f64 + 1e-9 * nsec as f64) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_as_python() {
        assert_eq!(repr_str("a/b"), "'a/b'");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(repr_str("x\n\t\\\u{1}\u{7f}"), "'x\\n\\t\\\\\\x01\\x7f'");
        assert_eq!(repr_strs(&["a", "b c"]), "['a', 'b c']");
        assert_eq!(repr_strs::<&str>(&[]), "[]");
        let v = Value::List(vec![Value::Int(1), Value::Null, Value::Bool(true), Value::str("s"), Value::Float(1.5)]);
        assert_eq!(repr_value(&v), "[1, None, True, 's', 1.5]");
        assert_eq!(str_value(&Value::str("s")), "s");
    }

    #[test]
    fn int_as_python() {
        assert_eq!(int("501", 10), Some(501));
        assert_eq!(int(" -7\n", 10), Some(-7));
        assert_eq!(int("1_000", 10), Some(1000));
        assert_eq!(int("4755", 8), Some(0o4755));
        assert_eq!(int("0o755", 8), Some(0o755));
        assert_eq!(int("8", 8), None);
        assert_eq!(int("", 10), None);
        assert_eq!(int("1__0", 10), None);
        assert_eq!(int("0x10", 10), None);
    }

    #[test]
    fn paths_as_python() {
        assert_eq!(join("a", "b"), "a/b");
        assert_eq!(join("a/", "b"), "a/b");
        assert_eq!(join("", "b"), "b");
        assert_eq!(join("a", "/b"), "/b");
        assert_eq!(dirname("a/b/c"), "a/b");
        assert_eq!(dirname("c"), "");
        assert_eq!(dirname("/c"), "/");
        assert_eq!(basename("a/b/c"), "c");
        assert_eq!(normpath("a//b/./c/../d"), "a/b/d");
        assert_eq!(normpath("../x"), "../x");
        assert_eq!(normpath("a/.."), ".");
        assert_eq!(normpath("empty"), "empty");
    }

    #[test]
    fn quotes_as_python() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("iPhone18,2"), "iPhone18,2");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(url_quote("com.apple.x_y-z~"), "com.apple.x_y-z~");
        assert_eq!(url_quote("a/b c#\u{e9}"), "a%2Fb%20c%23%C3%A9");
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"v"), "dg==");
        assert_eq!(b64(b"\0\x01\x02"), "AAEC");
        assert_eq!(b64(b"hello"), "aGVsbG8=");
    }

    #[test]
    fn mtime_as_python() {
        assert_eq!(int_mtime(1600000000, 0), 1600000000);
        assert_eq!(int_mtime(1600000000, 500_000_000), 1600000000);
        assert_eq!(int_mtime(1700000000, 999_999_999), 1700000001);
        assert_eq!(int_mtime(-2, 500_000_000), -1);
    }
}
