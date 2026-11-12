//! The JSON records the steps hand to each other (match.map.json, the reviewed files, the steps' own pair lists),
//! read the way the Python steps read them.

use crate::image::unhex;
use anyhow::{anyhow, Result};
use oneios_common::pyjson::Value;

/// `{int(k, 16): v['name'] for k, v in pairing[kind].items() if v.get('name')}`, in the file's order.
pub fn named(pairing: &Value, kind: &str) -> Result<Vec<(u64, String)>> {
    let mut out = Vec::new();
    for (key, value) in pairing.get(kind).and_then(Value::as_dict).ok_or_else(|| anyhow!("the pairing has no \"{kind}\""))? {
        if let Some(name) = value.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()) {
            out.push((unhex(key)?, name.to_string()));
        }
    }
    Ok(out)
}

/// The keys of pairing[kind] as addresses.
pub fn originals(pairing: &Value, kind: &str) -> Result<Vec<u64>> {
    pairing.get(kind).and_then(Value::as_dict).ok_or_else(|| anyhow!("the pairing has no \"{kind}\""))?.iter().map(|(k, _)| unhex(k)).collect()
}

/// `[(int(r['original'], 16), r['native_symbol']) for r in record.get(key, [])]`.
pub fn pairs(record: &Value, key: &str) -> Result<Vec<(u64, String)>> {
    let mut out = Vec::new();
    for row in record.get(key).and_then(Value::as_list).unwrap_or(&[]) {
        out.push((unhex(text(row, "original")?)?, text(row, "native_symbol")?.to_string()));
    }
    Ok(out)
}

pub fn text<'v>(record: &'v Value, key: &str) -> Result<&'v str> {
    record.get(key).and_then(Value::as_str).ok_or_else(|| anyhow!("a record has no \"{key}\""))
}

/// Python's truth of a JSON value (`if record.get('evidence')`).
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Str(s)) => !s.is_empty(),
        Some(Value::List(l)) => !l.is_empty(),
        Some(Value::Dict(d)) => !d.is_empty(),
        Some(Value::Int(n)) => *n != 0,
        Some(Value::Float(f)) => *f != 0.0,
        Some(Value::Bool(true)) => true,
    }
}

/// Python's repr of a dict of counts with string keys: `{'a': 1, 'b': 2}`.
pub fn repr_counts<'x>(items: impl Iterator<Item = (&'x str, i128)>) -> String {
    format!("{{{}}}", items.map(|(k, v)| format!("'{k}': {v}")).collect::<Vec<_>>().join(", "))
}

/// A Counter: counts in the order the keys were first seen.
#[derive(Default)]
pub struct Counter(pub indexmap::IndexMap<String, i128>);

impl Counter {
    pub fn add(&mut self, key: &str, n: i128) {
        match self.0.get_mut(key) {
            Some(c) => *c += n,
            None => {
                self.0.insert(key.to_string(), n);
            }
        }
    }
    pub fn get(&self, key: &str) -> i128 {
        self.0.get(key).copied().unwrap_or(0)
    }
    pub fn to_value(&self) -> Value {
        Value::Dict(self.0.iter().map(|(k, v)| (k.clone(), Value::Int(*v))).collect())
    }
    pub fn repr(&self) -> String {
        repr_counts(self.0.iter().map(|(k, v)| (k.as_str(), *v)))
    }
}
