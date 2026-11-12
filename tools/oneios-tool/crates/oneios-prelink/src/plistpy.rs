//! The kernelcache's prelink information as Python's plistlib reads and writes it. Reading is the `plist` crate;
//! the tree keeps the dictionaries' order, and the writer is plistlib's XML writer byte for byte (tabs, `%d`
//! integers, base64 lines of 76 columns less the indentation), because the rewritten property list is part of
//! kernel.kc, whose hash is recorded. The `plist` crate's own writer lays the text out differently.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;

#[derive(Clone, Debug, PartialEq)]
pub enum Plist {
    Str(String),
    Bool(bool),
    Int(i128),
    Real(f64),
    Dict(Vec<(String, Plist)>),
    Data(Vec<u8>),
    Date(String),
    Array(Vec<Plist>),
}

impl Plist {
    pub fn load(raw: &[u8]) -> Result<Plist> {
        let value = plist::Value::from_reader_xml(std::io::Cursor::new(raw)).context("__PRELINK_INFO is not a property list")?;
        convert(value)
    }
    pub fn get(&self, key: &str) -> Option<&Plist> {
        match self {
            Plist::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Plist> {
        match self {
            Plist::Dict(items) => items.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    /// `d[key] = value`: in place when the key exists, else at the end.
    pub fn set(&mut self, key: &str, value: Plist) {
        let Plist::Dict(items) = self else { return };
        match items.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => items.push((key.to_string(), value)),
        }
    }
    /// `d.pop(key, None)`: the others keep their order.
    pub fn remove(&mut self, key: &str) -> Option<Plist> {
        let Plist::Dict(items) = self else { return None };
        let at = items.iter().position(|(k, _)| k == key)?;
        Some(items.remove(at).1)
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Plist::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i128> {
        match self {
            Plist::Int(n) => Some(*n),
            Plist::Bool(b) => Some(*b as i128),
            _ => None,
        }
    }
    /// The keys of a dictionary (`entry.get(key) or {}`).
    pub fn keys(&self) -> Vec<&str> {
        match self {
            Plist::Dict(items) => items.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }
    /// Python's truth of the value.
    pub fn truthy(&self) -> bool {
        match self {
            Plist::Str(s) => !s.is_empty(),
            Plist::Bool(b) => *b,
            Plist::Int(n) => *n != 0,
            Plist::Real(f) => *f != 0.0,
            Plist::Dict(d) => !d.is_empty(),
            Plist::Data(d) => !d.is_empty(),
            Plist::Date(_) => true,
            Plist::Array(a) => !a.is_empty(),
        }
    }

    /// `plistlib.dumps(value, sort_keys=False)`.
    pub fn dumps(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        out.extend_from_slice(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
        out.extend_from_slice(b"<plist version=\"1.0\">\n");
        self.write(&mut out, 0)?;
        out.extend_from_slice(b"</plist>\n");
        Ok(out)
    }

    fn write(&self, out: &mut Vec<u8>, level: usize) -> Result<()> {
        let line = |out: &mut Vec<u8>, level: usize, text: &str| {
            out.extend(std::iter::repeat_n(b'\t', level));
            out.extend_from_slice(text.as_bytes());
            out.push(b'\n');
        };
        match self {
            Plist::Str(s) => line(out, level, &format!("<string>{}</string>", escape(s)?)),
            Plist::Bool(true) => line(out, level, "<true/>"),
            Plist::Bool(false) => line(out, level, "<false/>"),
            Plist::Int(n) => {
                if !(-(1i128 << 63)..1i128 << 64).contains(n) {
                    bail!("a property list integer is out of range: {n}");
                }
                line(out, level, &format!("<integer>{n}</integer>"));
            }
            Plist::Real(f) => line(out, level, &format!("<real>{}</real>", oneios_common::pyjson::float_repr(*f))),
            Plist::Date(d) => line(out, level, &format!("<date>{d}</date>")),
            Plist::Data(data) => {
                line(out, level, "<data>");
                let max = 16.max(76usize.saturating_sub(8 * level));
                for chunk in data.chunks((max / 4) * 3) {
                    line(out, level, &base64::engine::general_purpose::STANDARD.encode(chunk));
                }
                line(out, level, "</data>");
            }
            Plist::Dict(items) => {
                if items.is_empty() {
                    line(out, level, "<dict/>");
                } else {
                    line(out, level, "<dict>");
                    for (key, value) in items {
                        line(out, level + 1, &format!("<key>{}</key>", escape(key)?));
                        value.write(out, level + 1)?;
                    }
                    line(out, level, "</dict>");
                }
            }
            Plist::Array(items) => {
                if items.is_empty() {
                    line(out, level, "<array/>");
                } else {
                    line(out, level, "<array>");
                    for value in items {
                        value.write(out, level + 1)?;
                    }
                    line(out, level, "</array>");
                }
            }
        }
        Ok(())
    }
}

fn escape(text: &str) -> Result<String> {
    if text.bytes().any(|b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r')) {
        bail!("strings can't contain control characters; use bytes instead");
    }
    Ok(text.replace("\r\n", "\n").replace('\r', "\n").replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;"))
}

fn convert(value: plist::Value) -> Result<Plist> {
    Ok(match value {
        plist::Value::String(s) => Plist::Str(s),
        plist::Value::Boolean(b) => Plist::Bool(b),
        plist::Value::Integer(n) => Plist::Int(n.as_signed().map(i128::from).or_else(|| n.as_unsigned().map(i128::from)).ok_or_else(|| anyhow!("an integer of the property list has no value"))?),
        plist::Value::Real(f) => Plist::Real(f),
        plist::Value::Data(d) => Plist::Data(d),
        plist::Value::Date(d) => Plist::Date(d.to_xml_format()),
        plist::Value::Array(items) => Plist::Array(items.into_iter().map(convert).collect::<Result<_>>()?),
        plist::Value::Dictionary(items) => Plist::Dict(items.into_iter().map(|(k, v)| Ok((k, convert(v)?))).collect::<Result<_>>()?),
        other => bail!("the property list holds a value plistlib's XML has no element for: {other:?}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_what_plistlib_writes() {
        // plistlib.dumps of this value, Python 3.14
        let value = Plist::Dict(vec![
            ("b".into(), Plist::Int(-5)),
            ("a".into(), Plist::Array(vec![Plist::Str("x<y&z".into()), Plist::Bool(true), Plist::Real(1.5), Plist::Dict(vec![]), Plist::Array(vec![])])),
            ("d".into(), Plist::Dict(vec![("data".into(), Plist::Data((0u8..100).collect()))])),
            ("big".into(), Plist::Int(18446744005107531776)),
        ]);
        assert_eq!(String::from_utf8(value.dumps().unwrap()).unwrap(), include_str!("plistpy_expect.in"));
    }
}
